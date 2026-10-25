//! 入库与位置：正文出库、进度落库、坐标往返与重启恢复。

use super::harness::{
    epub_flow, flow, meta, offset_of, open, open_with_document, open_with_store, prefs,
    progress_of, scroll_down, settle,
};
use crate::ui::features::reader::model::chrome::PageMode;
use folio_core::content::document::Document;
use folio_core::store::Store;
use folio_core::{Content, DocMeta, Format, Locator};
#[allow(unused_imports)] // `#[gpui_kit::test]` 展开要它，unused 分析看不见宏里的使用
use gpui_kit::{AppContext as _, TestAppContext};
use std::sync::Arc;

/// **正文出库之后真正的开书路**：库里只有索引，正文按位置回容器取。
///
/// 这条钉的是三段接得上：
///
/// 1. `save_document` 把正文换成位置（库里读回来 `source` 是空的）；
/// 2. `with_container` 把索引与容器接上；
/// 3. 阅读会话能把正文取出并摊成行盒 —— 包括**换章**后再取一次。
///
/// 前两条在 core 有单测，第三条只有到这里才成立：`Book` 的按章缓存、
/// `Document::set_chapter` 的重排、以及“换章取不到时不该把上一章留在屏上”。
#[gpui_kit::test]
fn a_book_whose_text_is_not_in_the_db_loads_it_from_the_container(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/oss/minimal-zh.epub");
    // 库里的 `path` 要指向真文件，所以先拷一份（拷不到的那天也能一眼看出是这条挂了）。
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("book.epub");
    std::fs::copy(&fixture, &path).unwrap();

    // 导入那一步：读一遍 → 落库（只落索引）→ 丢掉手上那份。
    let store = Arc::new(Store::open_at(&dir.path().join("folio.db")).unwrap());
    let opened = crate::ui::features::reader::model::chapter_layout::open(&path).unwrap();
    let id = store.add_book(&path, opened.meta()).unwrap();
    store.save_document(id, opened.as_ref()).unwrap();
    drop(opened);

    // 开书那一步：从库里读回的 `Content` 里**没有正文**，就只有位置。
    let row = store.book(id).unwrap().unwrap();
    let content = store.load_content(id).unwrap().unwrap();
    let Content::Reflow(flow) = &content else {
        panic!("合成 EPUB 该读成可重排");
    };
    assert!(
        flow.chapters.iter().all(|c| c.source().is_empty()),
        "库里不该存正文"
    );
    let doc: Arc<dyn Document> = Arc::from(folio_core::content::document::with_container(
        DocMeta {
            title: row.title,
            authors: row.authors,
            format: Format::parse(&row.format).unwrap(),
            size: row.size,
            ..DocMeta::default()
        },
        content,
        &path,
    ));

    // 字号故意给到 60：这本合成书的三章都只有几行，默认字号下整本书比一个
    // 视口还短，「把下一章顶到视口顶」物理上不可达，恢复落点会被滚动上限
    // 拉回去、锚章也按「视口顶在哪章」的规矩落回上一章 —— 断言就成了测空集。
    // 大字号下每章都高过视口，换章的落点才可断言（与
    // `the_engine_progress_is_a_character_offset_that_comes_back` 同一个手法）。
    let (view, _handle) =
        open_with_document(cx, doc, prefs(60.0), Locator::start(), 460.0, None, None);
    let first: String = view.read_with(cx, |v, _| v.line_texts().join(""));
    assert!(
        first.contains("夜色从花园那头压过来"),
        "第一章的正文该从容器里取出来摊成行盒：{first:.120}"
    );

    // 换到第二章：这是另一条 zip 条目，也要真的取到（不是上一章的残留）。
    view.update(cx, |v, cx| v.advance_chapter(1, cx));
    settle(cx, _handle);
    let second: String = view.read_with(cx, |v, _| v.line_texts().join(""));
    assert!(
        second.contains("喷泉的水声"),
        "第二章该取到自己的正文：{second:.120}"
    );
    assert!(!second.contains("夜色从花园那头"), "不该把上一章留在屏上");
    assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 1);
}

#[gpui_kit::test]
fn wheel_scrolling_moves_the_reading_position(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);

    // 「章首」的真实不变式是字偏移为 0：恢复落点摆的是首行盒顶，章内比例
    // 可能带着首段的段前距（不是严格的 0.0），落库的坐标必须还是 0。
    assert_eq!(offset_of(&view, cx), 0, "刚打开时应在章首，不该已有位移");

    scroll_down(cx, handle, 900.0);
    let progress = progress_of(&view, cx);
    assert!(
        progress > 0.05,
        "真实滚轮事件应产生可测位移，实际比例 {progress:.3}"
    );
}

/// 滚动**不碰库**。位置那一趟 SQLite 写挂在输入路径上，平时 265 µs，可一旦摊上 WAL
/// checkpoint 或那条全库连接上的锁竞争就没有上界 —— 真机量到一趟 34.75 ms，紧接着
/// 一格走出邻格三倍的位移（80 px → 237 px），那就是「停止滑动那一下发抖」。所以位置
/// 留到离席才结（`persist_position`：退出阅读、以及窗口失去焦点）。
///
/// 这一条钉的是分工：滚的时候库里那一行不该出现，一结账它就得对得上屏幕上那一处。
/// 而「到过哪一章」反过来 —— 它当场就记，目录那一屏的浅色等不起退出。
#[gpui_kit::test]
fn scrolling_leaves_the_database_alone_until_the_book_closes(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let store = Arc::new(Store::in_memory().expect("内存库"));
    let id = store
        .add_book(std::path::Path::new("memory://fixture"), &meta("markdown"))
        .expect("入库");
    store
        .save_flow(id, &flow, Format::Markdown)
        .expect("章节入库");
    let (view, handle) = open_with_store(
        cx,
        &flow,
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
        Some(store.clone()),
        Some(id),
    );
    assert!(
        store.position(id).expect("读进度不该报错").is_none(),
        "开书不该先写一行位置"
    );

    scroll_down(cx, handle, 900.0);
    let offset = offset_of(&view, cx);
    assert!(offset > 0, "滚动该把位置推出去，实际偏移 {offset}");
    assert!(
        store.position(id).expect("读进度不该报错").is_none(),
        "滚动途中不该落库 —— 那一趟写库就是要摘掉的那一刀"
    );
    assert!(
        view.read_with(cx, |v, _| v.read_chapters().contains(&0)),
        "「到过第 0 章」要当场记进库，这一笔不在该等的范围里"
    );

    view.update(cx, |v, _| v.persist_position());
    let (_, saved) = store
        .position(id)
        .expect("读回进度")
        .expect("离席该把位置落下");
    assert_eq!(saved.chapter, 0);
    assert_eq!(saved.offset, offset, "落库的该是屏幕上滚到的那一行");
}

#[gpui_kit::test]
fn locator_matches_the_scrolled_ratio(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    // 语料是 `Chapter::inline` 建的，正文就在手上，所以字数一定算得出来。
    let total = flow.chapters[0].char_len().expect("正文在手上的章该有字数");
    assert!(total > 1000, "语料要够长才滚得动，实际 {total} 字");

    let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
    assert_eq!(offset_of(&view, cx), 0, "未滚动时偏移应为 0");

    scroll_down(cx, handle, 900.0);
    let progress = progress_of(&view, cx);
    let offset = offset_of(&view, cx);

    assert!(offset > 0, "滚动后偏移应前进");
    assert!(offset < total, "偏移不应越出章尾");
    // 正文全是等高段落，所以「滚过的比例」与「读到的字数比例」该吻合。
    // 留一档余量：首行缩进与段距让行顶与字数不是严格线性。
    let ratio = offset as f32 / total as f32;
    assert!(
        (ratio - progress).abs() < 0.1,
        "偏移↔比例 换算不一致：offset {offset} / {total} = {ratio:.3}，progress = {progress:.3}"
    );
}

/// 「重启回到原处」里属于我们自己的那一半：位置必须能无损穿过 SQLite 回来，
/// 连**当时那一档摆法**一起回来（进度存的是字偏移，模式只是它的读数方式）。
#[gpui_kit::test]
fn position_survives_a_restart_through_the_store(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);

    scroll_down(cx, handle, 900.0);
    let locator = Locator {
        chapter: 0,
        offset: offset_of(&view, cx),
    };
    assert!(locator.offset > 0, "滚动后应能算出字符偏移");

    let store = Arc::new(Store::in_memory().expect("内存库"));
    let id = store
        .add_book(std::path::Path::new("memory://fixture"), &meta("markdown"))
        .expect("入库");
    store
        .save_flow(id, &flow, Format::Markdown)
        .expect("章节入库");
    store
        .set_position(id, PageMode::Continuous.storage_name(), &locator)
        .expect("写入进度");

    let (mode, restored) = store.position(id).expect("读回进度").expect("进度应存在");
    assert_eq!(mode, "continuous");
    assert_eq!(restored, locator, "进度穿过 SQLite 之后必须逐字符一致");

    // 用读回来的进度重开：恢复请求应在排版做完那一帧被消费掉，偏移落回同一个字。
    let (reopened, _) = open_with_store(
        cx,
        &flow,
        "markdown",
        prefs(16.0),
        restored,
        460.0,
        Some(store),
        Some(id),
    );
    assert!(
        !reopened.read_with(cx, |v, _| v.has_pending_restore()),
        "解析完成后恢复请求应被消费掉，而不是永远挂着"
    );
    assert_eq!(offset_of(&reopened, cx), restored.offset);
    assert!(progress_of(&reopened, cx) > 0.0, "重开之后正文真的滚出去了");
}

/// M4 的门：坐标 → 行 → 落点 → 再反解回坐标，必须回到同一个字。
/// 这一条与「进度落在第几个字上」整套设计同源，所以它红了就是锚点坐标系红了。
#[gpui_kit::test]
fn the_engine_progress_is_a_character_offset_that_comes_back(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = epub_flow();
    // 字号故意给到 60：这本合成书的第一章很短，默认字号下整章只有几行、比窗口还短，
    // 那一档下没有可滚的范围，测试就成了测空集。
    let prefs = prefs(60.0);
    let (view, handle) = open(cx, &flow, "epub", prefs.clone(), Locator::start(), 460.0);
    let lines = view.read_with(cx, |v, _| v.lines().len());
    assert!(lines > 4, "大字号下第一章该排出多行，实际 {lines}");
    // 跳转目标取「本章中点」。写死一个坐标的话，CI 上字体不同 → 行高不同 →
    // 可能落在第一行里，那条测试就变成测空集了。
    let total = view.read_with(cx, |v, _| v.text_length());
    assert!(total > 40, "第一章该有几十个字可跳，实际 {total}");
    let target = total / 2;
    // 章首那一行的坐标不是 0：pretty-print 过的 XHTML 里 `<body>` 与 `<p>` 之间的
    // 换行本身是文本节点，也进坐标系。
    let start = offset_of(&view, cx);
    assert!(start < 8, "刚打开该在章首附近，实际坐标 {start}");

    view.update(cx, |v, cx| v.jump_to_offset(target, cx));
    settle(cx, handle);

    let saved = offset_of(&view, cx);
    assert!(
        saved > start,
        "跳到中点坐标 {target} 之后章内偏移该往前走，实际 {start} → {saved}"
    );
    assert!(
        saved <= target,
        "落点该是覆盖 {target} 那一行的行首，不该越过它：{saved}"
    );
    assert!(
        target - saved < target / 4,
        "落点离目标 {target} 太远：{saved}"
    );
    let progress = progress_of(&view, cx);
    assert!(
        progress > 0.0 && progress < 1.0,
        "滚动比例该在 (0,1) 之间，实际 {progress}"
    );

    // 用同一个坐标重开：落回的行反解出来的偏移必须还是它。
    let (reopened, _) = open(
        cx,
        &flow,
        "epub",
        prefs,
        Locator {
            chapter: 0,
            offset: saved,
        },
        460.0,
    );
    assert!(
        !reopened.read_with(cx, |v, _| v.has_pending_restore()),
        "恢复请求该在排版做完的那一帧被消费掉"
    );
    assert_eq!(
        offset_of(&reopened, cx),
        saved,
        "坐标 → 行顶 → 再反解回坐标，必须回到同一个字"
    );
    assert!(
        progress_of(&reopened, cx) > 0.0,
        "重开之后正文真的滚出去了，不是只记了个数"
    );
}
