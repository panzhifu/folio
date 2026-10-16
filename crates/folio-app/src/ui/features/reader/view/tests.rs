//! M1–M4 的验收门，现在挂在**统一的那一份**视图/会话上。
//!
//! 这批用例原来在 `desktop.rs`（虚拟列表那一套）里，`mobile.rs` 那边一条都没有 —— 于是
//! 「手机上正文渲染成立」这件事靠的是真机截图，而桌面上那条回路红过也没人知道。
//! 两端一份实现之后，这些断言测的就是真机上跑的那条代码路径。
//!
//! 用生产类型 `ReaderSession`，不在测试里另搭近似视图；滚动走真实滚轮事件，
//! 因为直接写 `scroll_y` 会绕过要测的那段行为。
//! 导入点名而不 `use super::*`：`gpui_kit` 根把 `gpui` 整个转了出来，里面有一个
//! 叫 `test` 的属性宏，glob 会让下面的 `#[test]` 递归展开成它。

use std::sync::Arc;
use std::time::Duration;

use folio_core::content::document::Document;
use folio_core::content::format::TocEntry;
use folio_core::infra::metrics::{Metric, take_local};
use folio_core::store::Store;
use folio_core::{Chapter, Config, Content, DocMeta, FlowDoc, Format, Locator, PageTurn, Reader};
use gpui_kit::component::Root;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Entity, InputEvent as _, LongPressEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, PlatformInput, Point, ScrollDelta,
    TestAppContext, TouchPhase, point, px, size,
};

use crate::ui::features::reader::chrome::PageMode;
use crate::ui::features::reader::session::ReaderSession;
use crate::ui::shared::config_model::ConfigHolder;
use crate::ui::shell::NAV_TRANSITION;

/// 一段足够长的中文，保证它会折行 —— 不折行的话「缩进改变了布局」这种断言无从成立。
const PARA: &str = "于是我们奋力向前，逆水行舟，被不断地向后推，推入过去。";

fn flow() -> FlowDoc {
    // `Chapter::source` 的契约是**引擎就绪的 XHTML**（纯文本的包装在 provider 里做完）。
    // 这里直接进排版层，所以自己按契约包好 —— 塞纯文本进来是测试自己违约，不是引擎该兜的。
    let source = (0..80)
        .map(|i| format!("<p>{PARA}（第 {i} 段）</p>"))
        .collect::<String>();
    FlowDoc {
        chapters: vec![Chapter::inline("第一章", source)],
        ..Default::default()
    }
}

/// 一章只有一行字：那一行下面紧接着就是空白。
///
/// 「点在划线下方的空白里不该把那条挑中」这条判据只有在这样的形状上才测得到 ——
/// 多行章里每一行都被邻行夹着，`hit_test` 那个「夹到最近的行」永远给你一行**里**的字。
fn one_line_flow() -> FlowDoc {
    FlowDoc {
        chapters: vec![Chapter::inline("一行", "<p>于是我们奋力向前</p>")],
        ..Default::default()
    }
}

/// 多章语料：每章 30 段（16px 下一章约三个视口高，章内可滚区间远大于
/// 测试的滚动步长，跨章之后认得出读到的是哪一章、进度也还在章首附近）。
fn chapters_flow(count: usize) -> FlowDoc {
    FlowDoc {
        chapters: (0..count)
            .map(|i| {
                let source = (0..30)
                    .map(|k| format!("<p>{PARA}（第{i}章 第{k}段）</p>"))
                    .collect::<String>();
                Chapter::inline(format!("第{i}章"), source)
            })
            .collect(),
        ..Default::default()
    }
}

/// 目录跳转用的语料：每章带着自己的 `spine_href`（目录条目靠它对上章号），正文第 6 段
/// 写着 `id="mid{i}"`，目录里每章给两条 —— 一条指章首、一条指那个锚点。真书里「一个
/// spine 文件挂着好几条小节」（一章对十条 nav 条目）就是这个形状。
///
/// 锚点放在第 6 段而不是第一段：锚点如果就落在章首第一个字上，「跳到锚点」与
/// 「跳到章首」得到同一个偏移，锚点那条换算整个坏掉了测试也看不出来。
fn anchored_flow(count: usize) -> FlowDoc {
    FlowDoc {
        chapters: (0..count)
            .map(|i| {
                let source = (0..30)
                    .map(|k| {
                        if k == 6 {
                            format!("<p id=\"mid{i}\">锚点第{i}章{PARA}</p>")
                        } else {
                            format!("<p>{PARA}（第{i}章 第{k}段）</p>")
                        }
                    })
                    .collect::<String>();
                Chapter::inline(format!("第{i}章"), source).with_spine_href(format!("c{i}.xhtml"))
            })
            .collect(),
        toc: (0..count)
            .flat_map(|i| {
                [
                    TocEntry {
                        title: format!("第{i}章"),
                        href: format!("c{i}.xhtml"),
                        ..Default::default()
                    },
                    TocEntry {
                        title: format!("第{i}章第二节"),
                        href: format!("c{i}.xhtml"),
                        fragment: Some(format!("mid{i}")),
                        ..Default::default()
                    },
                ]
            })
            .collect(),
        ..Default::default()
    }
}

/// 目录里某一条的锚点在这本书的章内偏移表里的位置。
///
/// 偏移表（`Document::id_offsets`）是排版产物，问的是**当前锚章**那一份，所以只能在
/// 跳到那一章、并且那一章排完之后问。落点断言拿它当基准，而不是在测试里写死一个数 ——
/// 写死的数换个字体就变了，变了会让测试红在几何上而不是红在跳转上。
///
/// 落点本身读视口顶那一行反解出来的章内偏移（[`offset_of`]）：章内偏移就是进度坐标系，
/// 落点对了行自然对，而行序还夹着缩进与换行，多一层要解释的东西。
fn anchor_offset(view: &Entity<ReaderSession>, id: &str, cx: &mut TestAppContext) -> usize {
    view.read_with(cx, |v, _| {
        v.book()
            .and_then(|book| book.document().id_offsets().get(id).copied())
            .unwrap_or_else(|| panic!("锚点 {id} 该在偏移表里"))
    })
}

fn meta(format: &str) -> DocMeta {
    DocMeta {
        title: "测试书".into(),
        authors: Vec::new(),
        format: Format::parse(format).expect("格式名要认得"),
        size: 0,
        ..DocMeta::default()
    }
}

/// 把这份语料落进一个内存库并登记成一行书目，返回 `(库, book_id)`。
///
/// 划线的用例都得有**真的**库：写不进库是「划了个寂寞」，删不掉是「取消划线」那颗按钮
/// 是坏的，这两件事只有对着存储层才测得出来。
fn store_with_book(flow: &FlowDoc, format: &str) -> (Arc<Store>, i64) {
    let store = Arc::new(Store::in_memory().expect("内存库"));
    let id = store
        .add_book(std::path::Path::new("memory://fixture"), &meta(format))
        .expect("入库");
    store
        .save_flow(id, flow, Format::Markdown)
        .expect("章节入库");
    (store, id)
}

/// 这一组用例的偏好。
///
/// **段间距钉在 1em**：那些断言量的是「像素进度 ↔ 字偏移」两套坐标对得上，而它们的
/// 容差是按章高标定的 —— 段距一改，章高跟着改，同样的滚动量占总高的比例就变了，
/// 容差会莫名其妙地翻红。段距本身由 `document/tests.rs` 的
/// `a_layout_preference_change_moves_the_geometry` 管，不在这里测。
fn prefs(size: f32) -> Reader {
    Reader {
        font_size: size,
        paragraph_gap_em: 1.0,
        ..Reader::default()
    }
}

/// 开一扇窗、装好配置、把会话摆上去。`width` 决定这一帧能不能长出双页 ——
/// 退档判据按宽度而不是按 target，所以窗口尺寸就是测试里唯一要控制的变量。
fn open(
    cx: &mut TestAppContext,
    flow: &FlowDoc,
    format: &str,
    prefs: Reader,
    locator: Locator,
    width: f32,
) -> (Entity<ReaderSession>, AnyWindowHandle) {
    open_with_store(cx, flow, format, prefs, locator, width, None, None)
}

#[allow(clippy::too_many_arguments)]
fn open_with_store(
    cx: &mut TestAppContext,
    flow: &FlowDoc,
    format: &str,
    prefs: Reader,
    locator: Locator,
    width: f32,
    store: Option<Arc<Store>>,
    book_id: Option<i64>,
) -> (Entity<ReaderSession>, AnyWindowHandle) {
    let meta = meta(format);
    // 不开文件地造一本书：`from_content` 把章节收进统一文档接口，没有容器
    // （图片与内嵌字体都不来，与「容器丢了」同一个表现）。
    let doc: Arc<dyn Document> = Arc::from(folio_core::content::document::from_content(
        meta,
        Content::Reflow(flow.clone()),
    ));
    open_with_document(cx, doc, prefs, locator, width, store, book_id)
}

/// 直接拿一份已经造好的文档开一屏。给「容器在、正文不在手上」那类测试用 ——
/// 那是正文出库之后生产上真正走的那条路。
#[allow(clippy::too_many_arguments)]
fn open_with_document(
    cx: &mut TestAppContext,
    doc: Arc<dyn Document>,
    prefs: Reader,
    locator: Locator,
    width: f32,
    store: Option<Arc<Store>>,
    book_id: Option<i64>,
) -> (Entity<ReaderSession>, AnyWindowHandle) {
    let mut view = None;
    let handle = cx.open_window(size(px(width), px(520.)), |window, cx| {
        // 偏好必须**装进配置**而不是只交给构造函数：会话每帧从 `ConfigHolder`
        // 同步一次偏好（读者在设置页改字号就是这条路），装默认配置就是把这里传的
        // `prefs` 当场冲掉 —— 而画面照样出得来，只有几何不对，最难查的那种红。
        let config = Config {
            reader: prefs.clone(),
            ..Default::default()
        };
        ConfigHolder::install(config, cx);
        let built = ReaderSession::with_document(doc, prefs, store, book_id, locator, cx);
        let created = cx.new(|_| built);
        view = Some(created.clone());
        Root::new(created, window, cx)
    });
    let view = view.expect("ReaderSession 应该被创建");
    settle(cx, handle.into());
    (view, handle.into())
}

/// 两轮「跑空事件 → 渲染一帧」。一轮不够：首帧只是把恢复请求挂上，
/// 而 `run_until_parked` 只跑事件不画帧。框架自己的 `settle()` 也是这个形状。
fn settle(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("渲染应成功");
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("第二帧应成功");
    cx.run_until_parked();
}

/// 真实滚轮事件投进正文。gpui 的约定是「向下滚为负」，所以往下滚给负的 dy。
fn scroll_down(cx: &mut TestAppContext, handle: AnyWindowHandle, pixels: f32) {
    cx.update_window(handle, |_, window, cx| {
        window.scroll(
            "folio-reader",
            ScrollDelta::Pixels(point(px(0.0), px(-pixels))),
            cx,
        );
    })
    .expect("滚动应送达阅读视图");
    cx.run_until_parked();
}

fn progress_of(view: &Entity<ReaderSession>, cx: &mut TestAppContext) -> f32 {
    view.read_with(cx, |v, _| v.progress())
}

fn offset_of(view: &Entity<ReaderSession>, cx: &mut TestAppContext) -> usize {
    view.read_with(cx, |v, _| v.current_offset().unwrap_or_default())
}

/// 合成书：仓库里那本三章的 `minimal-zh.epub`，**正文与样式表已经取回来**的那种。
///
/// `document::open` 现在只给索引（库里就存这个，见 `plans/text-out-of-db.md`），
/// 所以这里按位置把每一章的正文与样式文本补上 —— 这几条测的是排版，手上得有字
/// 也得有样式。
fn epub_flow() -> FlowDoc {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/oss/minimal-zh.epub");
    let doc = crate::ui::features::reader::document::open(&path).expect("合成 EPUB 应能打开");
    let mut flow = doc.flow().expect("合成 EPUB 应读成可重排").clone();
    for (ix, chapter) in flow.chapters.iter_mut().enumerate() {
        let text = doc.chapter_source(ix).expect("正文该能从容器取到");
        *chapter = Chapter::inline(chapter.title.clone(), text)
            .with_spine_href(chapter.spine_href.clone());
    }
    flow.css = doc.stylesheet();
    flow
}

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
    let opened = crate::ui::features::reader::document::open(&path).unwrap();
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

/// M2 的验收门：中文排版基础三件事必须由排版引擎产出。上一版在桌面上这些是
/// `TextView` 画的，测到的是框架的行为；现在两端都是自绘，测的是引擎自己的行盒。
#[gpui_kit::test]
fn the_engine_indents_centers_and_spaces_lines(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = epub_flow();
    assert_eq!(flow.chapters.len(), 3, "spine 三章");

    let (view, _handle) = open(
        cx,
        &flow,
        "epub",
        Reader {
            font_size: 20.0,
            line_height: 1.8,
            ..Reader::default()
        },
        Locator::start(),
        460.0,
    );
    let lines = view.read_with(cx, |v, _| v.lines());
    let texts = view.read_with(cx, |v, _| v.line_texts());
    assert!(!lines.is_empty(), "引擎应产出行盒");

    // 第一章首个行盒是标题：UA 的 h1 = 2em 之外，书自己的 CSS 写了 1.8em 且居中。
    let title = &lines[0];
    assert_eq!(texts[0], "第一章 蛇结");
    assert!(
        (title.dominant_font_size() - 36.0).abs() < 0.5,
        "h1 = 1.8em × 20px，实际 {}",
        title.dominant_font_size()
    );
    assert!(
        title.x > 20.0,
        "居中标题应离开版心左缘，实际 x = {}",
        title.x
    );

    let body = lines
        .iter()
        .find(|l| l.dominant_font_size() == 20.0 && l.x > 0.0)
        .expect("应有一个首行被缩进的正文行");
    assert!(
        (body.x - 40.0).abs() < 1.0,
        "2em 缩进在 20px 字号下应是 40px，实际 {}",
        body.x
    );
    assert!(
        title.height > body.height * 1.5,
        "行盒高应随字号走，标题 {} vs 正文 {}",
        title.height,
        body.height
    );
    assert!(
        (body.height - 36.0).abs() < 0.5,
        "正文行高 = 20 × 1.8，实际 {}",
        body.height
    );

    // 长段落必须真的折了行：没有任何一行容得下整段原文，才算折过。
    let longest_paragraph = flow.chapters[0]
        .source()
        .split('<')
        .map(|s| s.chars().filter(|c| !c.is_whitespace()).count())
        .max()
        .unwrap_or(0);
    let longest_line = texts.iter().map(|t| t.chars().count()).max().unwrap_or(0);
    assert!(
        longest_line < longest_paragraph && longest_line > 4,
        "段落应被折行：最长段 {longest_paragraph} 字，最长行只有 {longest_line} 字"
    );
    for pair in lines.windows(2) {
        assert!(
            pair[1].y >= pair[0].y + pair[0].height - 0.5,
            "行盒不能重叠：{} + {} > {}",
            pair[0].y,
            pair[0].height,
            pair[1].y
        );
    }
}

/// M3 的验收：一个段落里的局部字重、斜体与颜色必须各自成立。
/// 这一条测的是**真实字体栈**上的多 run 度量与摆放，mock 测不到。
#[gpui_kit::test]
fn inline_styles_survive_into_runs(cx: &mut TestAppContext) {
    use folio_core::content::format::css::computed::Color;

    cx.update(gpui_kit::init);
    let flow = epub_flow();
    let (view, _handle) = open(
        cx,
        &flow,
        "epub",
        Reader::default(),
        Locator::start(),
        460.0,
    );
    let lines = view.read_with(cx, |v, _| v.lines());

    let rich = lines
        .iter()
        .find(|l| l.text_runs().count() >= 4)
        .expect("带 em/strong/span 的那一行应被切成多个 run");
    assert!(
        rich.text_runs().any(|r| r.style.font.weight >= 700),
        "strong 应带粗体"
    );
    assert!(rich.text_runs().any(|r| r.style.font.italic), "em 应带斜体");
    assert!(
        rich.text_runs()
            .any(|r| r.style.color == Some(Color(0xC0, 0x39, 0x2B))),
        "span.red 的局部颜色要落到 run 上"
    );
    assert!(
        rich.text_runs().all(|r| r.width > 0.0),
        "每个 run 都要有实际绘制宽度"
    );
    let sum: f32 = rich.text_runs().map(|r| r.width).sum();
    assert!(
        (sum - rich.width).abs() < 0.5,
        "行宽应是各 run 宽度之和：{sum} vs {}",
        rich.width
    );
}

/// M3-E3 验收：EPUB 章里的内嵌图要经排版引擎变成图片片段，`src` 按本章目录解析到
/// 容器路径，且可见文本里不留哨兵字符。字节按需现取的这一层由 `BookImages` 管，
/// 这里钉的是排版侧：解析成容器路径、带出固有尺寸。
#[gpui_kit::test]
fn images_flow_through_the_engine_as_fragments(cx: &mut TestAppContext) {
    use folio_core::content::format::ImageRes;

    cx.update(gpui_kit::init);
    let flow = FlowDoc {
        chapters: vec![
            Chapter::inline(
                "图",
                "<p>前<img src=\"../Images/pic.png\" alt=\"图\">后</p>",
            )
            .with_spine_href("OEBPS/Text/chap.xhtml"),
        ],
        images: vec![ImageRes {
            href: "OEBPS/Images/pic.png".into(),
            source: "OEBPS/Images/pic.png".into(),
            media_type: "image/png".into(),
            width: 40,
            height: 20,
        }],
        ..Default::default()
    };
    let (view, _handle) = open(
        cx,
        &flow,
        "epub",
        Reader::default(),
        Locator::start(),
        460.0,
    );
    let lines = view.read_with(cx, |v, _| v.lines());
    let texts = view.read_with(cx, |v, _| v.line_texts());

    assert_eq!(texts[0], "前后", "行内图不产出字符，哨兵要被去掉");
    let img = lines[0]
        .images()
        .next()
        .expect("../Images/pic.png 应解析成 OEBPS/Images/pic.png 并命中");
    assert_eq!(img.href, "OEBPS/Images/pic.png");
    assert_eq!((img.width, img.height), (40.0, 20.0));
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

/// 默认档只替「从没选过」的人决定：库里存过档位，宽窗口也不许改他的选择。
/// 反过来（没存过 → 1200 宽给双页）由上面那条 `a_spread_is_two_contiguous...` 钉。
#[gpui_kit::test]
fn a_stored_mode_beats_the_wide_screen_default(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let store = Arc::new(Store::in_memory().expect("内存库"));
    let id = store
        .add_book(std::path::Path::new("memory://fixture"), &meta("markdown"))
        .expect("入库");
    store
        .save_flow(id, &flow, Format::Markdown)
        .expect("章节入库");
    store
        .set_position(id, PageMode::Continuous.storage_name(), &Locator::start())
        .expect("写入进度");

    let (view, handle) = open_with_store(
        cx,
        &flow,
        "markdown",
        prefs(16.0),
        Locator::start(),
        1200.0,
        Some(store),
        Some(id),
    );
    settle(cx, handle);
    assert_eq!(
        view.read_with(cx, |v, _| v.mode()),
        PageMode::Continuous,
        "读者存过的那一档优先，默认档不能覆盖它"
    );
}

/// 双页成立的那两条判据：一页确实是**两栏**，而且两栏首尾相接 —— 右栏的第一行
/// 紧跟左栏的最后一行，读者按「先左后右」读不会跳行。
#[gpui_kit::test]
fn a_spread_is_two_contiguous_columns_of_one_sheet(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 1200.0);
    assert_eq!(
        view.read_with(cx, |v, _| v.mode()),
        PageMode::Spread,
        "1200 宽的窗口没选过档，默认就该是双页（读者选过的照旧，见 set_mode）"
    );

    settle(cx, handle);
    assert_eq!(
        view.read_with(cx, |v, _| v.mode()),
        PageMode::Spread,
        "1200 宽的窗口该放得下两栏"
    );
    let pages = view.read_with(cx, |v, _| v.page_count());
    assert!(pages > 2, "长一章双页也该有多页，实际 {pages}");

    let left = view
        .read_with(cx, |v, _| v.sheet_range(0, 0))
        .expect("第 0 页左栏应有内容");
    let right = view
        .read_with(cx, |v, _| v.sheet_range(0, 1))
        .expect("第 0 页右栏应有内容");
    assert!(!left.is_empty() && !right.is_empty(), "两栏都要有行");
    assert_eq!(
        left.end, right.start,
        "右栏该从左栏断的那一行接着排，不能重叠也不能留缝"
    );

    // 一页一号：翻一页 = 翻一张纸（两栏一起走），偏移该往前走并且不超过两栏的行数。
    let before = offset_of(&view, cx);
    view.update(cx, |v, cx| v.scroll_page(1, cx));
    settle(cx, handle);
    assert_eq!(view.read_with(cx, |v, _| v.page()), 1);
    let after = offset_of(&view, cx);
    assert!(after > before, "翻一张纸该往前走，实际 {before} → {after}");
}

/// 窄窗口点「双页」不该得到两条窄柱子：按宽度退回分页。
/// 这条判据按宽度而不是按 target，所以横屏平板能真双页、竖屏手机自动单页。
#[gpui_kit::test]
fn a_narrow_window_falls_back_from_spread_to_one_column(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
    view.update(cx, |v, cx| v.set_mode(PageMode::Spread, cx));
    settle(cx, handle);

    assert_eq!(
        view.read_with(cx, |v, _| v.mode()),
        PageMode::Paged,
        "460 宽放不下两栏，该退回分页"
    );
    // 读者**选**的仍然是双页：按钮的选中态读它，不然一退档那格就自己弹回去了。
    assert_eq!(view.read_with(cx, |v, _| v.wanted_mode()), PageMode::Spread);
    // 只有一栏，所以第 1 栏不存在。
    assert!(
        view.read_with(cx, |v, _| v.sheet_range(0, 1)).is_none(),
        "退档之后不该还剩一条空栏"
    );
}

/// 换摆法不换位置：连续滚到某个字，切双页（宽窗）再切回来，还在同一个字上。
/// 进度是「第几个字」，模式只是它的读数方式 —— 这条要是红了，读者的位置就会在
/// 点一下「双页」时飘掉。
#[gpui_kit::test]
fn switching_modes_keeps_the_same_character(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 1200.0);
    // 这条测的是「换档不丢字」，起点得自己钉住：1200 宽的默认档已经是双页了。
    view.update(cx, |v, cx| v.set_mode(PageMode::Continuous, cx));
    settle(cx, handle);
    scroll_down(cx, handle, 1200.0);
    let wanted = offset_of(&view, cx);
    assert!(wanted > 0, "先滚到一个字上，实际 {wanted}");

    view.update(cx, |v, cx| v.set_mode(PageMode::Spread, cx));
    settle(cx, handle);
    assert_eq!(view.read_with(cx, |v, _| v.mode()), PageMode::Spread);
    let in_spread = offset_of(&view, cx);
    // 落点是「覆盖那个字的那张纸的开头」，所以它只能等于或早于原位置，
    // 且不能早过一页之内的距离。
    assert!(
        in_spread <= wanted,
        "落位不该越过原来那一行：{wanted} → {in_spread}"
    );

    view.update(cx, |v, cx| v.set_mode(PageMode::Continuous, cx));
    settle(cx, handle);
    assert_eq!(
        offset_of(&view, cx),
        in_spread,
        "切回连续滚动该落在双页那一屏的开头那个字上"
    );
}

// ---- M6 选中：真实指针/手势事件驱动的状态机（见 plans/M6-selection.md） ----

/// 拉丁词的语料：长按吸词的验收要「吸出整词」可判定。
fn latin_flow() -> FlowDoc {
    FlowDoc {
        chapters: vec![Chapter::inline(
            "第一章",
            "<p>Hello world and then some more words so the paragraph wraps once or twice here</p>",
        )],
        ..Default::default()
    }
}

/// 把一个平台事件投进窗口 —— 与真机同一入口（`Window::dispatch_event`），
/// 不直调会话方法，注册在 paint 里的那层监听必须真的收得到。
fn dispatch(cx: &mut TestAppContext, handle: AnyWindowHandle, event: PlatformInput) {
    cx.update_window(handle, |_, window, cx| {
        window.dispatch_event(event, cx);
    })
    .expect("事件应送达窗口");
    cx.run_until_parked();
}

fn mouse_down(cx: &mut TestAppContext, handle: AnyWindowHandle, at: Point<Pixels>) {
    dispatch(
        cx,
        handle,
        MouseDownEvent {
            button: MouseButton::Left,
            position: at,
            ..Default::default()
        }
        .to_platform_input(),
    );
}

fn mouse_move(cx: &mut TestAppContext, handle: AnyWindowHandle, at: Point<Pixels>) {
    dispatch(
        cx,
        handle,
        MouseMoveEvent {
            position: at,
            pressed_button: Some(MouseButton::Left),
            ..Default::default()
        }
        .to_platform_input(),
    );
}

fn mouse_up(cx: &mut TestAppContext, handle: AnyWindowHandle, at: Point<Pixels>) {
    dispatch(
        cx,
        handle,
        MouseUpEvent {
            button: MouseButton::Left,
            position: at,
            ..Default::default()
        }
        .to_platform_input(),
    );
}

/// 第 `line` 行第 `k` 个字的中心（窗口坐标）。
///
/// 换算与 `ReaderSession::to_layout` 互逆：排版坐标加回页眉让位、再减掉当前的 `scroll_y`。
/// 不这么办就等于假设正文从没动过 —— 恢复请求是在头几帧里落地的（`settle` 的那几帧正文
/// 自己挪走一档），按「滚动为 0」取点，点到的就已经是别的一行。字的横向位置按字号估：
/// 命中引擎会 snap 到落点最近的字，差小半个字不影响断言。
fn char_point(
    view: &Entity<ReaderSession>,
    cx: &TestAppContext,
    line: usize,
    k: usize,
) -> Point<Pixels> {
    view.read_with(cx, |v, _| {
        let box_ = &v.lines()[line];
        let char_w = box_.dominant_font_size();
        point(
            px(v.prefs.margin_x + box_.x + char_w * (k as f32 + 0.5)),
            px(box_.y + box_.height * 0.5 - v.scroll_y + v.header_reserve),
        )
    })
}

/// 按下 → 拖过阈值 → 松手，成立一个选区（共享给几条用例的前置步骤）。
///
/// 起点在第 1 行：那是这份语料里第一处「上面还有字」的位置，两端都测得着。
fn drag_select(cx: &mut TestAppContext, view: &Entity<ReaderSession>, handle: AnyWindowHandle) {
    let start = char_point(view, cx, 1, 2);
    let end = char_point(view, cx, 1, 4);
    drag_select_between(cx, handle, start, end);
    assert!(
        view.read_with(cx, |v, _| v.selection().is_some()),
        "拖过阈值的松手应成立选中"
    );
}

/// 同样的三连，起止点由调用方给 —— 只有一行字的语料里 `drag_select` 那个第 1 行不存在。
fn drag_select_between(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    start: Point<Pixels>,
    end: Point<Pixels>,
) {
    mouse_down(cx, handle, start);
    for step in 1..=4 {
        let t = step as f32 / 4.0;
        mouse_move(
            cx,
            handle,
            point(
                px(f32::from(start.x) + (f32::from(end.x) - f32::from(start.x)) * t),
                px(f32::from(start.y) + (f32::from(end.y) - f32::from(start.y)) * t),
            ),
        );
    }
    mouse_up(cx, handle, end);
}

/// M6 验收：按下 → 拖过阈值 → 松手，选区成立；阈值内的普通点击不留高亮；
/// 拖选松手攒下的 click 豁免恰好拦住那一下；复制的内容与可视文本一致。
#[gpui_kit::test]
fn a_pointer_drag_selects_and_swallows_the_trailing_click(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );

    // 普通点击（按下即抬，无位移）不留高亮：阈值之内的一切都不算拖选。
    let tap_at = char_point(&view, cx, 0, 1);
    mouse_down(cx, handle, tap_at);
    mouse_up(cx, handle, tap_at);
    assert!(
        view.read_with(cx, |v, _| v.selection().is_none()),
        "普通点击不该留下选中"
    );

    // 真实拖选：down → 8 步 move → up（跨过阈值、跨行）。
    let start = char_point(&view, cx, 0, 1);
    let end = char_point(&view, cx, 2, 3);
    mouse_down(cx, handle, start);
    for step in 1..=8 {
        let t = step as f32 / 8.0;
        mouse_move(
            cx,
            handle,
            point(
                px(f32::from(start.x) + (f32::from(end.x) - f32::from(start.x)) * t),
                px(f32::from(start.y) + (f32::from(end.y) - f32::from(start.y)) * t),
            ),
        );
    }
    mouse_up(cx, handle, end);
    let Some(selected) = view.read_with(cx, |v, _| v.selection()) else {
        panic!("拖过阈值的松手应成立选中");
    };
    assert!(
        selected.range.start < selected.range.end,
        "选区应非空：{selected:?}"
    );

    // 拖选松手攒下的豁免：下一次读得到，再读就没有 —— 恰好拦住那一下 click。
    let first = view.update(cx, |v, _| v.consume_tap_suppression());
    let second = view.update(cx, |v, _| v.consume_tap_suppression());
    assert!(first, "拖选松手应拦住紧跟着的 click");
    assert!(!second, "豁免是一次性的");

    // 复制：与可视文本一致（TextMap::slice）。
    view.update(cx, |v, cx| v.copy_selection(cx));
    let copied = cx
        .update(|cx| cx.read_from_clipboard().expect("剪贴板应有内容").text())
        .expect("应是文本");
    let expected = view.read_with(cx, |v, _| {
        v.book()
            .expect("有书")
            .document()
            .text_map()
            .slice(selected.range)
    });
    assert_eq!(copied, expected, "复制内容应与可视文本一致");
}

/// M6 验收：长按吸词 —— 点在词首字符上，吸出的是整个词（"Hello"，词界稳定），
/// 手势结束后菜单条开着。
#[gpui_kit::test]
fn a_long_press_snaps_to_the_word_and_opens_the_menu(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &latin_flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );

    // 第 0 行的首字符「H」内取点：拉丁字宽约半个 em，1/4 em 处稳落第一字。
    let at = view.read_with(cx, |v, _| {
        let box_ = &v.lines()[0];
        point(
            px(v.prefs.margin_x + box_.x + box_.dominant_font_size() * 0.25),
            px(box_.y + box_.height * 0.5),
        )
    });
    for phase in [TouchPhase::Started, TouchPhase::Ended] {
        dispatch(
            cx,
            handle,
            LongPressEvent {
                phase,
                start_position: at,
                position: at,
            }
            .to_platform_input(),
        );
    }
    let (selection, menu) = view.read_with(cx, |v, _| (v.selection(), v.selection_menu_open()));
    let Some(selected) = selection else {
        panic!("长按应选中一个词");
    };
    let text = view.read_with(cx, |v, _| {
        v.book()
            .expect("有书")
            .document()
            .text_map()
            .slice(selected.range.clone())
    });
    assert_eq!(text, "Hello", "点在词首应吸出整词：{selected:?} → {text:?}");
    assert!(menu, "长按成选后菜单条应开着");
}

/// M7 划线：选中 → 落一条批注 → 重开这本书还能解回同一区间。
///
/// 两头都要钉住：写不进库是「划了个寂寞」，读不回坐标是「重开就丢」。
/// 拿掉 `add_highlight` 里那次写库、或 `open_book` 里那句 `load_annotations`，这条就红。
#[gpui_kit::test]
fn a_highlight_lands_in_the_store_and_comes_back(cx: &mut TestAppContext) {
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
    drag_select(cx, &view, handle);
    let selected = view.read_with(cx, |v, _| v.selection()).expect("已选中");

    view.update(cx, |v, cx| v.add_highlight(cx));

    let rows = store.annotations(id).expect("读库");
    assert_eq!(rows.len(), 1, "划一下应落一条批注");
    let row = &rows[0];
    assert_eq!(row.chapter, selected.chapter as u32, "锚点属于选区那一章");
    assert_eq!(
        (row.anchor.start, row.anchor.end),
        (selected.range.start, selected.range.end),
        "锚点要就是划中的那一段"
    );
    assert!(
        !row.anchor.exact.is_empty(),
        "锚点要带原文指纹，重排后靠它找回"
    );
    assert_eq!(row.color, "amber", "还没得选颜色时落清单里第一种");
    assert!(
        view.read_with(cx, |v, _| v.selection().is_none()),
        "划完选区收掉：那片底色从此来自批注，不是来自「选中」"
    );

    // 重开这本书：库里那一条读回来，并解回本章坐标上的同一区间。
    let (again, again_handle) = open_with_store(
        cx,
        &flow,
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
        Some(store),
        Some(id),
    );
    settle(cx, again_handle);
    let spans = again.read_with(cx, |v, _| {
        v.annotation_spans.borrow().get(&0).map(|(_, spans)| {
            spans
                .iter()
                .map(|span| (span.start, span.end, span.is_stale()))
                .collect::<Vec<_>>()
        })
    });
    assert_eq!(
        spans,
        Some(vec![(selected.range.start, selected.range.end, false)]),
        "重开这一章，批注要回显在原处且不是 stale"
    );
}

/// 拖选 → 菜单里那颗「划线」→ 点在画出来的那片底色上。把「已经重选好」这一状态共享给
/// 后面几条用例，返回那条划线的章内区间（含头不含尾）。
fn highlight_then_tap_it_back(
    cx: &mut TestAppContext,
    view: &Entity<ReaderSession>,
    handle: AnyWindowHandle,
) -> (usize, usize) {
    // 先把排版定下来：头一代行盒与最终那代对不上（第一代还没按真实版心宽重排过），
    // 在它上面选中的字，落库之后画出来的底色并不在原地。
    settle(cx, handle);
    drag_select(cx, view, handle);
    let (start, end) = view
        .read_with(cx, |v, _| {
            v.selection()
                .map(|selected| (selected.range.start, selected.range.end))
        })
        .expect("已选中");
    view.update(cx, |v, cx| v.add_highlight(cx));
    // 拖选松手那一下攒下的「吃掉下一个 click」要先放掉 —— 真实时序里它被正文那层吃掉了
    // （`tap_on_body` 第一条判据）。不吃掉，下面这一下会被它拦下来，测的就还是第一条。
    assert!(
        view.update(cx, |v, _| v.consume_tap_suppression()),
        "拖选松手该攒下一次豁免"
    );
    // 画一帧：底色上屏、`content_bounds` 就位 —— 命中问的就是画出来的那几块矩形。
    settle(cx, handle);
    let at = char_point(view, cx, 1, 2);
    let consumed = cx
        .update_window(handle, |_, window, cx| {
            view.update(cx, |v, cx| v.tap_on_body(window, cx, at))
        })
        .expect("窗口应可更新");
    assert!(
        consumed,
        "点在一条已有的划线上应被「选中」消费，不该再拿去翻页或开浮层"
    );
    (start, end)
}

/// M7 二期：留在书上的那片底色可以**再点回去**。点上去就是重新选中那一条，菜单跟着回来，
/// 并且认出「这一条已经在库里」—— 第二格是「取消划线」，不是再划一次。
///
/// 拿掉 `tap_on_body` 里「没选区时试着重选」那一条判据，这一击返回 `false`、也不留选区：
/// 读者划过的痕迹变成只读，正是这条要挡住的坏状态。
#[gpui_kit::test]
fn a_tap_on_a_highlight_selects_it_again(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (store, id) = store_with_book(&flow, "markdown");
    let (view, handle) = open_with_store(
        cx,
        &flow,
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
        Some(store),
        Some(id),
    );
    let (start, end) = highlight_then_tap_it_back(cx, &view, handle);
    let (range, menu, marked) = view.read_with(cx, |v, _| {
        (
            v.selection()
                .map(|selected| (selected.range.start, selected.range.end)),
            v.selection_menu_open(),
            v.selection_annotation().map(|(_, color)| color),
        )
    });
    assert_eq!(range, Some((start, end)), "重选中的就是那一条原本那一段字");
    assert!(menu, "菜单条该跟着回来");
    assert_eq!(
        marked.as_deref(),
        Some("amber"),
        "选区正好等于库里那一条，菜单该换上「取消划线」那一格"
    );

    // 浮层开着时让位：那一击的语义是收顶栏与底栏，不是顺手把那条划线挑中
    // （`overlay_open` 由阅读页每帧报进来）。先清掉选区，走的才是「没有选区」那一路。
    view.update(cx, |v, _| v.clear_selection());
    view.update(cx, |v, cx| v.set_overlay_open(true, cx));
    let on_mark = char_point(&view, cx, 1, 2);
    let consumed = cx
        .update_window(handle, |_, window, cx| {
            view.update(cx, |v, cx| v.tap_on_body(window, cx, on_mark))
        })
        .expect("窗口应可更新");
    assert!(!consumed, "浮层开着的一击归收浮层，不该被重选中消费掉");
    view.update(cx, |v, cx| v.set_overlay_open(false, cx));

    // 没有选区时点在**没划过**的那一行：这一击不该被选中消费（仍归翻页与浮层）。
    let elsewhere = char_point(&view, cx, 9, 2);
    let consumed = cx
        .update_window(handle, |_, window, cx| {
            view.update(cx, |v, cx| v.tap_on_body(window, cx, elsewhere))
        })
        .expect("窗口应可更新");
    assert!(!consumed, "没划线的地方不该吃掉这一击");
    assert!(
        view.read_with(cx, |v, _| v.selection().is_none()),
        "那一击也不该留下选区"
    );
}

/// 点在划线**下面**的空白里：不算压住那一条。
///
/// `hit_test` 的纵向语义是「夹到最近的行」（那是拖选收边要的），只按「最近的字落在区间里」
/// 判的话，读者在末行之下一击就把某条批注挑中。拿掉 `annotation_at` 里那道行盒检查这条会红。
#[gpui_kit::test]
fn a_tap_below_the_marked_line_selects_nothing(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = one_line_flow();
    let (store, id) = store_with_book(&flow, "markdown");
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
    // 同一条理由：先让排版定下来，那一行的行盒才是后面要量它的那个。
    settle(cx, handle);
    let start = char_point(&view, cx, 0, 1);
    let end = char_point(&view, cx, 0, 3);
    drag_select_between(cx, handle, start, end);
    view.update(cx, |v, cx| v.add_highlight(cx));
    assert!(
        view.update(cx, |v, _| v.consume_tap_suppression()),
        "拖选松手该攒下一次豁免"
    );
    settle(cx, handle);
    assert_eq!(
        view.read_with(cx, |v, _| v.annotations.len()),
        1,
        "先真的落一条"
    );

    // 同一列、往下 6 像素：横向还在那几个字上，纵向已经出了那一行的行盒。
    let below = view.read_with(cx, |v, _| {
        let box_ = &v.lines()[0];
        point(
            px(v.prefs.margin_x + box_.x + box_.dominant_font_size() * 1.5),
            px(box_.y + box_.height + 6.0 - v.scroll_y + v.header_reserve),
        )
    });
    let consumed = cx
        .update_window(handle, |_, window, cx| {
            view.update(cx, |v, cx| v.tap_on_body(window, cx, below))
        })
        .expect("窗口应可更新");
    assert!(!consumed, "末行之下的空白不该把那条划线挑中");
    assert!(
        view.read_with(cx, |v, _| v.selection().is_none()),
        "更不该留下选区"
    );
}

/// 「取消划线」真的删得掉库里那一条，删完那片底色与选区一起收掉。
#[gpui_kit::test]
fn the_reselected_highlight_can_be_removed(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (store, id) = store_with_book(&flow, "markdown");
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
    highlight_then_tap_it_back(cx, &view, handle);
    assert!(
        view.read_with(cx, |v, _| v.selection_annotation().is_some()),
        "菜单认得出这一条在库里，那颗按钮才有得按"
    );

    view.update(cx, |v, cx| v.delete_highlight(cx));

    assert!(
        store.annotations(id).expect("读库").is_empty(),
        "取消划线要真的删掉库里那一条"
    );
    assert!(
        view.read_with(cx, |v, _| v.selection().is_none()),
        "那片底色已经没了，选区也该收掉"
    );
}

/// 换颜色改的就是库里那一条自己：条目不多一条、锚点一个字没动、选区留着。
#[gpui_kit::test]
fn recoloring_a_highlight_moves_nothing(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (store, id) = store_with_book(&flow, "markdown");
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
    let (start, end) = highlight_then_tap_it_back(cx, &view, handle);

    view.update(cx, |v, cx| v.set_highlight_color("sky", cx));

    let rows = store.annotations(id).expect("读库");
    assert_eq!(rows.len(), 1, "换颜色不该多落一条");
    assert_eq!(rows[0].color, "sky", "库里那条的颜色要跟着改");
    assert_eq!(
        (rows[0].anchor.start, rows[0].anchor.end),
        (start, end),
        "改的只是颜色，锚点一个字都没动"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.selection_annotation().map(|(_, color)| color))
            .as_deref(),
        Some("sky"),
        "菜单认得的还是那一条，读的也是新那一档"
    );
    assert!(
        view.read_with(cx, |v, _| v.selection().is_some()),
        "选区留着：读者往往还要再比一档"
    );
}

/// M6 验收：选中态下的一击——点选区内把菜单请回来、选区不动，点选区外清空，
/// 两下都消费（`page.rs` 不该再拿它们翻页）。
#[gpui_kit::test]
fn a_tap_with_active_selection_clears_or_keeps(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    drag_select(cx, &view, handle);
    let range = view
        .read_with(cx, |v, _| v.selection().map(|selected| selected.range))
        .expect("已选中");

    // 点在选区起点（起拖那个字必在选区内）：菜单回开，选区不动，这一击被消费。
    let inside = char_point(&view, cx, 1, 2);
    let (consumed, menu, still) = cx
        .update_window(handle, |_, window, cx| {
            view.update(cx, |v, cx| {
                let consumed = v.tap_with_selection(window, cx, inside);
                (
                    consumed,
                    v.selection_menu_open(),
                    v.selection().map(|s| s.range),
                )
            })
        })
        .expect("窗口应可更新");
    assert!(consumed, "点在选区内的一击应被选中消费");
    assert!(menu, "菜单条应回开");
    assert_eq!(still, Some(range), "选区不该动");

    // 点选区外（往下六行）：清空，这一击也被消费（不翻页）。
    let outside = char_point(&view, cx, 7, 2);
    let (consumed, cleared) = cx
        .update_window(handle, |_, window, cx| {
            view.update(cx, |v, cx| {
                let consumed = v.tap_with_selection(window, cx, outside);
                (consumed, v.selection().is_none())
            })
        })
        .expect("窗口应可更新");
    assert!(consumed, "点选区外的一击也应被消费");
    assert!(cleared, "点选区外应清空选中");
}

/// 菜单把选区清掉之后，落在菜单上的那一下仍要被「选中」这一套消费掉。
///
/// 钉的是 `tap_on_body` 里那三条判据的**顺序**：那颗「划线」「取消」在自己的 click
/// 里就清了选区，等这一下冒泡到正文时 `selection()` 已是 `None` —— 把
/// `taps_selection_ui` 那一步挪到「有没有选区」之后，这里就会返回 `false`，
/// 于是它既做了动作、又被当成「点了一下正文」，顶栏与底栏跟着一起冒出来。
///
/// 清完选区不 `settle`：真实时序里 click 处理跑在下一帧绘制之前，而绘制会把菜单
/// 那份锚位收掉（菜单已经不摆了）。`settle` 一下就把要测的那个窗口期抹平了。
#[gpui_kit::test]
fn a_tap_on_the_menu_is_consumed_even_after_it_cleared_the_selection(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    drag_select(cx, &view, handle);
    settle(cx, handle);
    // 拖选松手那一下攒下的「吃掉下一个 click」要先放掉：真实时序里它就是被正文那层
    // 吃掉的（`tap_on_body` 第一条判据）。不吃掉它，下面这一下会被它拦下来，
    // 测试就只是验了第一条判据、看着像验了菜单那条。
    assert!(
        view.update(cx, |v, _| v.consume_tap_suppression()),
        "拖选松手该攒下一次豁免"
    );
    let menu = view
        .read_with(cx, |v, _| v.menu_bounds.get())
        .expect("菜单上屏后该留下锚位");
    view.update(cx, |v, _| v.clear_selection());
    assert!(
        view.read_with(cx, |v, _| v.selection().is_none()),
        "这一条测的就是「选区已经没了」那个时刻"
    );
    let consumed = cx
        .update_window(handle, |_, window, cx| {
            view.update(cx, |v, cx| v.tap_on_body(window, cx, menu.center()))
        })
        .expect("窗口应可更新");
    assert!(
        consumed,
        "落在菜单上的一击要被消费掉，不该再往下开浮层或翻页"
    );
}

/// M6 验收：翻走即清 —— 分页翻页与换摆法都不留旧高亮（连续滚动不清，
/// `set_scroll_y` 不调 `clear_selection`，选中的字随滚动一起走）。
#[gpui_kit::test]
fn a_page_turn_and_a_mode_switch_clear_the_selection(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );

    // 分页翻页：清。
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    settle(cx, handle);
    drag_select(cx, &view, handle);
    view.update(cx, |v, cx| v.scroll_page(1, cx));
    assert!(
        view.read_with(cx, |v, _| v.selection().is_none()),
        "翻走即清"
    );
    // 让那一张平移走完，再挑下一次。命中的是**画出来的**位置（`page_shift`），而测试时钟
    // 不自己走 —— 不走完这一趟，第二次拖选按下去落在新页前方外整整一页的地方，
    // 选不出字来。这不是选中坏了，是这一帧的字确实还不在那儿。
    turn_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));

    // 换摆法：清。
    drag_select(cx, &view, handle);
    view.update(cx, |v, cx| v.set_mode(PageMode::Continuous, cx));
    assert!(
        view.read_with(cx, |v, _| v.selection().is_none()),
        "换摆法即清"
    );
}

// ---- 跨章连续滚动：锚章 ± 窗口拼成一条全局坐标（见 book.rs） ----

/// 向下滚过章尾：正文接上下一章，锚章迁移、进度的章号跟着走。
#[gpui_kit::test]
fn scrolling_past_the_chapter_end_flows_into_the_next_one(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &chapters_flow(3),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 0);

    // 100px 一步：章界是按像素判定的，步子太大可能一步迈过整章的可滚区间，
    // 「刚跨进来」的断言就失真了。
    for _ in 0..120 {
        scroll_down(cx, handle, 100.0);
        settle(cx, handle);
        if view.read_with(cx, |v, _| v.chapter_index()) == 1 {
            break;
        }
    }
    assert_eq!(
        view.read_with(cx, |v, _| v.chapter_index()),
        1,
        "滚过章尾该迁移到下一章"
    );
    let texts = view.read_with(cx, |v, _| v.line_texts().join(""));
    assert!(
        texts.contains("第1章"),
        "锚章迁移后屏上该是下一章：{texts:.80}"
    );
    let (chapter, _) = view.read_with(cx, |v, _| v.resolve_position().expect("滚得动就该有位置"));
    assert_eq!(chapter, 1, "进度的章号要跟着视口顶走");
    let progress = progress_of(&view, cx);
    assert!(
        progress < 0.5,
        "刚跨进下一章，章内比例该接近开头：{progress:.3}"
    );
}

/// 向上滚过章首：接回上一章，锚章跟着回去。
#[gpui_kit::test]
fn scrolling_back_up_reaches_the_previous_chapter(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &chapters_flow(3),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    for _ in 0..40 {
        scroll_down(cx, handle, 400.0);
        settle(cx, handle);
        if view.read_with(cx, |v, _| v.chapter_index()) == 1 {
            break;
        }
    }
    assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 1);

    for _ in 0..40 {
        scroll_down(cx, handle, -400.0);
        settle(cx, handle);
        if view.read_with(cx, |v, _| v.chapter_index()) == 0 {
            break;
        }
    }
    assert_eq!(
        view.read_with(cx, |v, _| v.chapter_index()),
        0,
        "滚回章首之上该接回上一章"
    );
    let texts = view.read_with(cx, |v, _| v.line_texts().join(""));
    assert!(
        texts.contains("第0章"),
        "回去之后屏上该是上一章：{texts:.80}"
    );
}

/// 进度存在中间章，重开：落点是那一章的章首（全局坐标 = 章基线），
/// 向上滚能接回上一章 —— 拼接坐标对恢复进度是透明的。
#[gpui_kit::test]
fn a_locator_in_a_middle_chapter_restores_into_the_spine(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = chapters_flow(3);
    let locator = Locator {
        chapter: 1,
        offset: 0,
    };

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

    let (view, handle) = open_with_store(
        cx,
        &flow,
        "markdown",
        prefs(16.0),
        locator,
        460.0,
        Some(store),
        Some(id),
    );
    assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 1);
    let texts = view.read_with(cx, |v, _| v.line_texts().join(""));
    assert!(texts.contains("第1章"), "该落在第二章的开头：{texts:.80}");

    // 往上滚：上一章就拼在正上方，滚过章首就能接回去。
    for _ in 0..40 {
        scroll_down(cx, handle, -400.0);
        settle(cx, handle);
        if view.read_with(cx, |v, _| v.chapter_index()) == 0 {
            break;
        }
    }
    assert_eq!(
        view.read_with(cx, |v, _| v.chapter_index()),
        0,
        "中间章的章首之上该是上一章"
    );
}

/// 拼接窗口钉在锚章 ± 2：滚到哪里，预排版就跟到哪里，窗外整块让出去。
#[gpui_kit::test]
fn the_spine_window_stays_pinned_to_the_anchor(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &chapters_flow(6),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.span_window()),
        vec![0, 1, 2],
        "开书时窗口 = 锚章 + 下方两章"
    );

    // 一路滚到最后一章。
    for _ in 0..120 {
        scroll_down(cx, handle, 400.0);
        settle(cx, handle);
        if view.read_with(cx, |v, _| v.chapter_index()) == 5 {
            break;
        }
    }
    assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 5);
    assert_eq!(
        view.read_with(cx, |v, _| v.span_window()),
        vec![3, 4, 5],
        "滚到书尾，窗口滑到锚章 + 上方两章，窗外的章整块让出去"
    );
}

// ---- 目录跳转：读者点目录里的一条，屏上真的换成那一处 ----

/// 目录每一行带着「跳去哪儿」的两个数：`chapter` 由 `spine_href` 查表得到，
/// `fragment` 是那条目录自己的 `#` 后面一截。这两格错了，目录那一屏照样画得出来、
/// 标题也对，点下去才发现送到别处 —— 那是最难从截图看出来的错。
#[gpui_kit::test]
fn each_toc_row_carries_where_clicking_it_goes(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, _) = open(
        cx,
        &anchored_flow(4),
        "epub",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    let rows = view.read_with(cx, |v, _| {
        v.toc_rows()
            .iter()
            .map(|row| (row.title.clone(), row.chapter, row.fragment.clone()))
            .collect::<Vec<_>>()
    });
    assert_eq!(rows.len(), 8, "四章各两条目录");
    assert_eq!(
        rows[0],
        ("第0章".to_string(), Some(0), None),
        "指章首的那条：有章号、没有锚点"
    );
    assert_eq!(
        rows[1],
        ("第0章第二节".to_string(), Some(0), Some("mid0".to_string())),
        "指小节的那条：同一个章号，加上它自己的锚点"
    );
    // 对不上正文的条目会带 `chapter == None`，界面按「画出来但点不动」处置。
    // 这一本每一条都对得上；哪天查表坏了，这一条先红。
    assert!(
        rows.iter().all(|(_, chapter, _)| chapter.is_some()),
        "有目录条目对不上章节：{rows:?}"
    );
}

/// 点一条指向别的章的目录：锚章换过去、屏上摆那一章的开头、恢复请求在锚章排完那一帧
/// 被取走、位置当场落库（杀掉进程回来还在这一章），「来过」集合也记上这一章。
#[gpui_kit::test]
fn a_toc_jump_moves_the_screen_to_that_chapter_and_saves_it(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = anchored_flow(5);
    let store = Arc::new(Store::in_memory().expect("内存库"));
    let id = store
        .add_book(std::path::Path::new("memory://fixture"), &meta("epub"))
        .expect("入库");
    store.save_flow(id, &flow, Format::Epub).expect("章节入库");
    let (view, handle) = open_with_store(
        cx,
        &flow,
        "epub",
        prefs(16.0),
        Locator::start(),
        460.0,
        Some(store.clone()),
        Some(id),
    );
    assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 0);

    view.update(cx, |v, cx| v.goto_entry(3, None, cx));
    settle(cx, handle);

    assert_eq!(
        view.read_with(cx, |v, _| v.chapter_index()),
        3,
        "锚章该挪到第 3 章"
    );
    assert_eq!(offset_of(&view, cx), 0, "指章首的那条目录，落点就是章首");
    assert!(
        !view.read_with(cx, |v, _| v.has_pending_restore()),
        "恢复请求该在锚章排完那一帧被取走，而不是永远挂着"
    );
    let texts = view.read_with(cx, |v, _| v.line_texts());
    assert!(
        texts.iter().any(|text| text.contains("第3章 第0段")),
        "屏上该是第 3 章的开头：{texts:?}"
    );
    assert!(
        !texts.iter().any(|text| text.contains("第0章")),
        "第 0 章的行一份都不该在这份排版里：{texts:?}"
    );

    let (_, locator) = store.position(id).expect("读回进度").expect("进度应存在");
    assert_eq!(
        locator,
        Locator {
            chapter: 3,
            offset: 0
        },
        "跳转当场就把位置写了盘：读者杀进程回来还在第 3 章"
    );
    assert!(
        view.read_with(cx, |v, _| v.read_chapters().contains(&3)),
        "到过第 3 章要记进「来过」集合，目录那一屏的浅色按它画"
    );
}

/// 点一条带锚点的小节（跳到别的章里的某一处）：落点不是那一章的章首，而是那个 `id`
/// 所在那一段的第一个字。锚点换算必须等排版做完才能做，所以这一条测的是「排完那一帧
/// 落对了地方」。
#[gpui_kit::test]
fn a_toc_jump_with_an_anchor_lands_on_that_paragraph(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &anchored_flow(4),
        "epub",
        prefs(16.0),
        Locator::start(),
        460.0,
    );

    view.update(cx, |v, cx| v.goto_entry(2, Some("mid2".to_string()), cx));
    settle(cx, handle);

    assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 2);
    let expected = anchor_offset(&view, "mid2", cx);
    // 没有这一条，`assert_eq!(offset_of, expected)` 在「两边都是 0」上也成立 ——
    // 那正是锚点整个没生效的样子。锚点写在第 6 段，它绝不等于章首。
    assert!(
        expected > 0,
        "锚点在第 6 段上，它的章内偏移不该等于章首的 0：{expected}"
    );
    assert_eq!(
        offset_of(&view, cx),
        expected,
        "视口顶那一行该正好从锚点那一段开始"
    );
    assert!(
        view.read_with(cx, |v, _| v
            .line_texts()
            .iter()
            .any(|text| text.contains("锚点第2章"))),
        "落点所在的那一段要在这一章的排版里"
    );
    assert!(
        !view.read_with(cx, |v, _| v.has_pending_restore()),
        "带着锚点的请求同样要在落地那一帧被取走"
    );
}

/// 一个 spine 文件挂着好几条小节（一章对十条 nav）时，跳到**同一章**的另一条不算换章，
/// `Book::goto` 会说「没动」—— 可落点照样得挪。这一段分支单独一条：
/// 少了它，读者点同章的小节就是「按下去什么都不发生」。
#[gpui_kit::test]
fn an_anchor_in_the_chapter_you_are_in_still_moves_the_landing(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    // 起点就在第 1 章的章首。
    let (view, handle) = open(
        cx,
        &anchored_flow(4),
        "epub",
        prefs(16.0),
        Locator {
            chapter: 1,
            offset: 0,
        },
        460.0,
    );
    assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 1);
    assert_eq!(offset_of(&view, cx), 0, "起点是第 1 章的章首");

    view.update(cx, |v, cx| v.goto_entry(1, Some("mid1".to_string()), cx));
    settle(cx, handle);

    assert_eq!(
        view.read_with(cx, |v, _| v.chapter_index()),
        1,
        "还在同一章，没白跳"
    );
    assert_eq!(
        offset_of(&view, cx),
        anchor_offset(&view, "mid1", cx),
        "落点从章首挪到了那一条小节"
    );
}

/// 目录指向一个书里不存在的 `id`（作者写错、或锚点挂在一个没有正文的元素上）：
/// 章还是要照跳，落点留在章首 —— 宁可回到本章开头，也不要凭猜落在正文中间。
#[gpui_kit::test]
fn an_anchor_the_layout_cannot_find_falls_back_to_the_chapter_head(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &anchored_flow(4),
        "epub",
        prefs(16.0),
        Locator::start(),
        460.0,
    );

    view.update(cx, |v, cx| {
        v.goto_entry(2, Some("这本书里没有这个id".to_string()), cx)
    });
    settle(cx, handle);

    assert_eq!(
        view.read_with(cx, |v, _| v.chapter_index()),
        2,
        "锚点解不出来，章还是要跳"
    );
    assert_eq!(
        offset_of(&view, cx),
        0,
        "落点留在章首，不是某个猜出来的中间位置"
    );
    assert!(
        view.read_with(cx, |v, _| v
            .line_texts()
            .iter()
            .any(|text| text.contains("第2章 第0段"))),
        "屏上该从第 2 章第一段开始"
    );
}

/// 改字号全链重排：进度的章号不换 —— 坐标是「第几个字」，跨重排存活。
#[gpui_kit::test]
fn a_pref_change_reflows_the_whole_spine_without_losing_the_chapter(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &chapters_flow(3),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    for _ in 0..40 {
        scroll_down(cx, handle, 400.0);
        settle(cx, handle);
        if view.read_with(cx, |v, _| v.chapter_index()) == 1 {
            break;
        }
    }
    assert_eq!(
        view.read_with(cx, |v, _| v.resolve_position().map(|(chapter, _)| chapter)),
        Some(1),
        "上面那一串滚动要真把人送进第二章，否则下面量的是别处"
    );
    // 刚跨进第二章那一刻，位置正是「章内第 0 个字」：换字号再怎么重排，章首那一行盒顶
    // 反解回来还是 0，于是「偏移变了才算真重排过」这一条永远证不了。补一格滚动把落点
    // 摆进章内 —— 段距一改，章首那一刻停在第几行也会跟着挪，所以这一步不能省。
    scroll_down(cx, handle, 300.0);
    settle(cx, handle);
    let (chapter, offset) =
        view.read_with(cx, |v, _| v.resolve_position().expect("重排前就该有位置"));
    assert_eq!(chapter, 1, "补的那一格滚动不该把人送出第二章");
    assert!(
        offset > 0,
        "落点要在章内而不是章首，否则下面那条「偏移变了」无从成立：实测 {offset}"
    );

    cx.update(|cx| ConfigHolder::edit_reader(cx, |prefs| prefs.font_size = 24.0));
    settle(cx, handle);
    let (after_chapter, after_offset) =
        view.read_with(cx, |v, _| v.resolve_position().expect("重排后该有位置"));
    assert_eq!(after_chapter, 1, "重排不换章");
    assert!(
        after_offset > 0 && after_offset != offset,
        "字号变了行数变了，章内偏移落在别处才是重排过的证据：{offset} → {after_offset}"
    );
}

/// 量具在记：往前滚一屏，新进入带的行要物化单元、没 shape 过的行要落到 gpui 那层。
///
/// 这条是下面两条目标门（`scrolling_back_to_the_same_band_materializes_no_new_runs` 与
/// `scrolling_back_to_the_same_band_does_not_reshape`）的**配对探测器**：它钉的是
/// 「计数器真的会动」—— 把 `Document::row_units` 或 `paint::shape` 里那句计数删掉，
/// 它当场红（三格读出来全是零），于是那两条目标门不会变成永远绿的假灯。
///
/// **两张表接上之后这一条不删**（原稿写的是「接上之后要删掉」，2026-10-15 接线时更正）：
/// 单元表是**按行懒物化**的，往前滚必然有新行第一次进带，那一趟该付；`SceneRuns` 的口径
/// 也已同步改成「这一帧新建了几条单元」，所以它量的始终是真正付过的物化次数，
/// 而不是一帧搬运了多少条。
#[gpui_kit::test]
fn a_scrolled_frame_currently_rematerializes_the_visible_runs(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (_view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
    // 先滚一段把上一帧的账结掉，再把三格清零：断言的是「只滚动」这一步的增量，
    // 而不是进程总量（`take_local` 的读法，见 `infra::metrics`）。
    scroll_down(cx, handle, 240.0);
    settle(cx, handle);
    take_local(Metric::SceneRebuilds);
    take_local(Metric::SceneRuns);
    take_local(Metric::ShapeLineCalls);

    scroll_down(cx, handle, 240.0);
    settle(cx, handle);
    let rebuilds = take_local(Metric::SceneRebuilds);
    let runs = take_local(Metric::SceneRuns);
    let shapes = take_local(Metric::ShapeLineCalls);
    // 实测基线（2026-10-13，debug 构建，`flow()` 那份语料、窗宽 460、字号 16）：
    // 一次 240px 滚动走完两帧 = 5 次场景重建、62 个 run 物化、62 次 `shape_line`。
    // 5 次重建对应连续模式**每个进链的章各一份场景**（锚章 ±2）；`runs` 与 `shapes`
    // 相等，说明这一屏里每一个被拷出来的 run 同时都付了一次查表。
    // 记在这里是为了下一轮能拿同一组数比，而不是只看到「红/绿」。
    assert!(
        rebuilds > 0 && runs > 0 && shapes > 0,
        "滚动这一步应当数得出场景重建、run 物化与 shape_line 调用，实际 {rebuilds} / {runs} / {shapes}"
    );
}

/// 章那张 shaping 表接上之后的**目标断言**：滚回走过的地方，一次都不该重新 shape。
///
/// 与上面那条探测器配成一对读：它钉的是「`ShapeLineCalls` 真的会动」（往前滚一屏增量 > 0），
/// 所以这一条不会是一盏永远绿的假灯。把 `Document::shaped` 那张表拿掉、或把
/// `view::shape_runs` 那一步拿掉，这一条当场红。
///
/// 为什么「表没接上」就必然红：gpui 自己的 `LineLayoutCache` 只保两帧
/// （`previous_frame` 与 `current_frame` 在 `finish_frame` 里互换），而这里往返隔了远不止
/// 两帧 —— 邻章那些行从「排版作业 shape 完」到「进屏」也是同样的道理，所以进屏那一帧
/// 从前总要重 shape 一次，并且命中也要把整段文本再哈希一遍。
///
/// **这一条与 `SceneRuns` 那条 `#[ignore]` 的目标门是两件事**：那条钉的是 run 物化
/// （行单元表，§九），这一条钉的是 shaping。物化那一笔今天照付，所以那条门还红着。
#[gpui_kit::test]
fn scrolling_back_to_the_same_band_does_not_reshape(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (_view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
    // 往前一屏：这一趟**该**付钱（新进入带的行第一次 shape），所以先把账结掉。
    // 往返只用一屏的量，是为了待在同一个锚章的 ±2 窗口里 —— 走远了整个 `Document`
    // 被裁掉，表跟着它一起没了，那一次的重新 shape 是对的，不该记在这笔账上。
    scroll_down(cx, handle, 240.0);
    settle(cx, handle);
    take_local(Metric::ShapeLineCalls);

    // 回原处（`scroll_down` 的约定是「向下滚为负」）。
    scroll_down(cx, handle, -240.0);
    settle(cx, handle);
    assert_eq!(
        take_local(Metric::ShapeLineCalls),
        0,
        "滚回走过的地方不该再 shape 一次：几何该由章那张 shaping 表原样给回"
    );
}

/// 表的**失效**：改一次字号，同样的行必须重新 shape。
///
/// 钉的是「表不能比排版活得久」。失效信号只有一处 —— `Document::drop_fragments`
/// （`set_chapter` / `set_prefs` / `set_images` / `apply_typeset` 四个都走它）；哪天有人
/// 给 shaping 表另立一套失效判断、或者漏挂在某一条上，这一条就会红。
/// 旧几何是按旧字号量的，留在表里就是「看着改了等于没改」那种看不出来的错。
#[gpui_kit::test]
fn changing_the_font_size_reshapes_the_same_lines(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (_view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
    settle(cx, handle);
    take_local(Metric::ShapeLineCalls);

    cx.update(|cx| ConfigHolder::edit_reader(cx, |prefs| prefs.font_size = 24.0));
    settle(cx, handle);
    assert!(
        take_local(Metric::ShapeLineCalls) > 0,
        "改字号必须作废 shaping 表：那一屏的行要按新字号重新 shape"
    );
}

/// 目标断言（`plans/阅读管线.md` §九 + §十一 第 6 条）：**滚下去再滚回同一屏，第二趟一个
/// 新 run 都不该物化**。
///
/// 口径为什么是**往返**而不是「往前滚增量为 0」：行单元表是**懒物化**的（§九 已决：整章一份、
/// 某一行的单元只在它第一次进带时才建）。往前滚必然有新行进来，那些行第一次被画就是要拷一次
/// 文本——拿「往前滚为 0」当验收钉的是一件本来就不该发生的事。回到走过的地方才是那个确定的零。
///
/// 根子在 `GlyphRun` 的 `x` / `baseline` 存的是**相对元素原点的屏幕值**（`paint.rs:26-38`）：
/// 滚动一动，每条 run 的 `baseline` 都跟着变，场景就只能整份重造。这是「叶子只存局部值、
/// 全局值靠现算」那条规则在呈现层欠着的一笔——排版层已经照它做了（`BlockBox.y` 相对章顶、
/// 全局 y 由 ±2 章的拼接链现算），绘制层还没。
///
/// 修法的形状：「按行号索引的行单元表（值全是章内局部量）+ 每帧一个平移量」，整章一份挂在
/// `Document` 上随它进出窗口；主题色与解析好的 `Font` **都不许烘进表**（前者因为 `LayoutKey`
/// 里没有主题，后者因为 `FontCache` 已有自己的失效纪律，见 §九 那四条硬约束）。
///
/// **这一条只断 `SceneRuns`，不断 `ShapeLineCalls`**：两笔钱分开钉。shaping 那一笔
/// 2026-10-15 也接了，但接成了**另一张自己的表**（`Document::shaped`，键
/// `(行号, 片段槽位)`，值是 `Arc<LineLayout>`），配自己的门
/// [`scrolling_back_to_the_same_band_does_not_reshape`]，没有并进这张行单元表 ——
/// 因为 gpui 的 `ShapedLine.layout` 是 `pub(crate)`（外面拿不出那份 `Arc`），
/// 而它自带的 `decoration_runs` 里有主题色。
///
/// **2026-10-15 已接线，`#[ignore]` 摘掉了**（原来那句「接线之后去掉这一句就成了守门员」
/// 走的就是 `epub::tests` 那条「一百次取图只开一次容器」走过的路）：行单元表在
/// `Document::row_units`（挂在 `Document` 上随章进出窗口，失效只跟 `drop_fragments`
/// 那一处信号），`Metric::SceneRuns` 的口径同步从「每帧摆几条 run」改成「这一帧新建几条
/// 单元」——不改口径这一格就会在缓存生效时照样涨，那条确定的零就永远量不出来。
/// 上面那条探测器（`a_scrolled_frame_currently_rematerializes_the_visible_runs`）现在
/// 反过来才是它的对头：往前滚那一趟仍然该付，所以它照旧断三格都 > 0。
#[gpui_kit::test]
fn scrolling_back_to_the_same_band_materializes_no_new_runs(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (_view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);

    // 往前一屏：这一趟**该**付钱（新行第一次进带），所以先把账结掉。
    scroll_down(cx, handle, 240.0);
    settle(cx, handle);
    take_local(Metric::SceneRuns);

    // 回原处：`scroll_down` 的约定是「向下滚为负」，给它一个负数就是向上滚。往返只用一屏的量，
    // 是为了待在同一个锚章的 ±2 窗口里——走远了整个 `Document` 被裁掉，那一次的重新物化是对的，
    // 不该记在「还没接」这笔账上。
    scroll_down(cx, handle, -240.0);
    settle(cx, handle);
    assert_eq!(
        take_local(Metric::SceneRuns),
        0,
        "走过的地方再走回来，只该改帧内那个平移量，不该物化任何一段新文本"
    );
}

// ---- 插图的可点性：点一张图，要把它交给放大查看那一层 ----

/// 一份「图片字节就在手里」的文档。
///
/// `from_content` 造出来的那本没有容器，`image_bytes` 永远给不出字节，于是插图一张
/// 都不画 —— 而「点图上那一下」在测试里就没有目标可点。这一份按 `href` 直接交出字节，
/// 排版、按需解码、绘制走的仍然是生产那几条路。
struct Illustrated {
    meta: DocMeta,
    content: Content,
}

impl Document for Illustrated {
    fn meta(&self) -> &DocMeta {
        &self.meta
    }

    fn kind(&self) -> folio_core::content::format::ContentKind {
        self.content.kind()
    }

    fn content(&self) -> folio_core::content::document::ContentRef<'_> {
        match &self.content {
            Content::Reflow(flow) => folio_core::content::document::ContentRef::Reflow(flow),
            Content::Fixed(fixed) => folio_core::content::document::ContentRef::Fixed(fixed),
        }
    }

    fn image_bytes(&self, href: &str) -> Option<Vec<u8>> {
        (href == "p.png").then(|| crate::ui::shared::images::TINY_PNG.to_vec())
    }

    fn stylesheet(&self) -> String {
        String::new()
    }

    /// 正文按章现取那一条：字节不在容器里，就在手上这份 `FlowDoc` 里。
    fn chapter_source(&self, index: usize) -> folio_core::Result<String> {
        let Content::Reflow(flow) = &self.content else {
            return Err(folio_core::Error::NoSuchChapter(index as i64));
        };
        flow.chapters
            .get(index)
            .map(|chapter| chapter.source().to_string())
            .ok_or(folio_core::Error::NoSuchChapter(index as i64))
    }
}

/// 一章：两段话中间夹一张 200×100 的插图，第一屏就一定摆得上。
fn image_flow() -> FlowDoc {
    FlowDoc {
        chapters: vec![Chapter::inline(
            "第一章",
            format!("<p>{PARA}</p><p><img src=\"p.png\" alt=\"\"></p><p>{PARA}</p>"),
        )],
        images: vec![folio_core::content::format::ImageRes {
            href: "p.png".into(),
            source: "p.png".into(),
            media_type: "image/png".into(),
            width: 200,
            height: 100,
        }],
        ..Default::default()
    }
}

/// 回归门：点正文里的一张插图，把这张图交给放大那一层。
///
/// 两条断言各钉一段路：
/// - `is_open` 钉「图上真挂了点击，而且那个 handler 认得要看哪张」；
/// - 固有尺寸钉「排版有没有把尺寸带到界面」。少了后一条没有任何可见症状 —— 查看器会
///   静默退到经验上限，读者只看得见「放不大」，而线索指向的是排版。
#[gpui_kit::test]
fn tapping_an_illustration_hands_it_to_the_zoom_layer(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let doc: Arc<dyn Document> = Arc::new(Illustrated {
        meta: meta("epub"),
        content: Content::Reflow(image_flow()),
    });
    let viewer = cx
        .update(|cx| cx.new(|_| crate::ui::features::reader::image_viewer::ImageViewer::default()));
    let (view, handle) =
        open_with_document(cx, doc, prefs(16.0), Locator::start(), 460.0, None, None);
    // 生产上这一层由阅读页挂上（它画在那一屏的最上面）；这里只要那个句柄，
    // 层本身不在本窗口的树里 —— 钉的是「点击有没有把图交出去」，不是层的摆位。
    cx.update(|app| {
        view.update(app, |session, _| {
            session.set_image_viewer(viewer.clone());
        })
    });
    settle(cx, handle);

    let found = cx
        .update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.try_find(("reader-image", 0usize)).is_some()
        })
        .expect("渲染应成功");
    assert!(found, "前置：那张插图要真被摆上屏，否则根本没有可点的东西");

    cx.update_window(handle, |_, window, cx| {
        window.click(("reader-image", 0usize), cx);
    })
    .expect("点击应送达");
    cx.run_until_parked();

    assert!(
        viewer.read_with(cx, |viewer, _| viewer.is_open()),
        "点插图该把这张图交给放大那一层"
    );
    assert_eq!(
        viewer.read_with(cx, |viewer, _| viewer.seen_natural()),
        Some((200.0, 100.0)),
        "固有尺寸要从 manifest 一路带到查看器：丢了它只会静默退到经验上限"
    );
}

// ---- 正文内的链接：点目录页正文里的 `<a href>` 跳到那一章 ----

/// 一本书内目录页：第一章的正文本身就是一份 `<ul>` 链接表，指向后面两章。
/// 这是真书里「内容里的目录」的形状（`罗生门` 的 `part0003.xhtml` 就是
/// `<a href="part0004.xhtml#a005">罗生门</a>`）。`c2` 那条带章内锚点。
fn link_flow() -> FlowDoc {
    FlowDoc {
        chapters: vec![
            Chapter::inline(
                "目录",
                "<ul>\
                 <li><a href=\"c1.xhtml\">第一章</a></li>\
                 <li><a href=\"c2.xhtml#mid2\">第二章</a></li>\
                 </ul>",
            )
            .with_spine_href("toc.xhtml"),
            Chapter::inline("第一章", "<p>第一章正文</p>").with_spine_href("c1.xhtml"),
            Chapter::inline("第二章", "<p>第二章正文</p><p id=\"mid2\">锚点段</p>")
                .with_spine_href("c2.xhtml"),
        ],
        ..Default::default()
    }
}

/// 点正文里的一条链接，锚章真的换到它指向的那一章。走的是 `on_body_tap` 里
/// 链接优先那一支（`page.rs`）落到的那段逻辑。
#[gpui_kit::test]
fn tapping_a_link_in_the_body_text_jumps_to_its_target(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &link_flow(),
        "epub",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    // 前置：目录页那一行真有字，否则根本没有可点的目标。
    let first_line: String = view.read_with(cx, |v, _| v.line_texts().join(""));
    assert!(
        first_line.contains("第一章"),
        "前置：目录页第一行该摆着链接文字：{first_line:.80}"
    );
    // 点第一个链接（第一行第一个字的中心）。
    let at = char_point(&view, cx, 0, 0);
    let consumed = cx
        .update_window(handle, |_, window, cx| {
            view.update(cx, |v, cx| v.open_link_at(window, cx, at))
        })
        .expect("渲染应成功");
    assert!(consumed, "点在链接上，这一击该被链接吃掉");
    assert_eq!(
        view.read_with(cx, |v, _| v.chapter_index()),
        1,
        "该跳到链接指向的第一章"
    );
}

// ---- 脚注：点中一条引用就把注文抬出来 ----

/// 一本带脚注的书：正文里一个上标链接（`<a epub:type="noteref">`），章尾一个
/// `<aside epub:type="footnote">`。
fn footnote_flow() -> FlowDoc {
    FlowDoc {
        chapters: vec![Chapter::inline(
            "第一章",
            "<p>正文开头<a epub:type=\"noteref\" href=\"#fn1\">1</a>，正文结尾。</p>\
             <aside epub:type=\"footnote\" id=\"fn1\"><p>这是一条脚注的全文。</p></aside>",
        )],
        ..Default::default()
    }
}

/// 点中脚注引用（正文里的那个上标）就把注文抬出来，而不是去跳那一行。
#[gpui_kit::test]
fn tapping_a_footnote_ref_raises_the_note(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &footnote_flow(),
        "epub",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    // 第一行是「正文开头1，正文结尾。」，第 5 个字（下标 4）就是那个上标「1」。
    let at = char_point(&view, cx, 0, 4);
    let consumed = cx
        .update_window(handle, |_, window, cx| {
            view.update(cx, |v, cx| v.open_footnote_at(window, cx, at))
        })
        .expect("渲染应成功");
    assert!(consumed, "点在脚注引用上，这一击该被脚注吃掉");
    assert_eq!(
        view.read_with(cx, |v, _| v.footnote().map(|f| f.text.clone())),
        Some("这是一条脚注的全文。".to_string()),
        "弹层该拿到章尾 <aside> 里那句原文"
    );
}

/// 短章语料：每章只有一小段（远不到一屏），用来钉连续滚动在「章比屏短」时也不卡死。
fn short_chapters_flow(count: usize) -> FlowDoc {
    FlowDoc {
        chapters: (0..count)
            .map(|i| {
                let source = format!("<p>第{i}章 的正文只有这一小段字。</p>");
                Chapter::inline(format!("第{i}章"), source)
            })
            .collect(),
        ..Default::default()
    }
}

/// 短章也要能一章一章往下滚：连续滚动在「锚章 ± 固定几章加起来不够一屏」时，
/// 视口顶永远进不了下一章，锚章就不迁移、拼接窗口也不往前长 —— 读者卡在原地。
/// 语料给 40 个一行章：老窗口（锚章 ±2）只有 3 章 ≈ 不到一屏，必然卡死。
#[gpui_kit::test]
fn scrolling_advances_through_short_chapters(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &short_chapters_flow(40),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 0);
    for _ in 0..2000 {
        scroll_down(cx, handle, 40.0);
        settle(cx, handle);
        if view.read_with(cx, |v, _| v.chapter_index()) >= 30 {
            break;
        }
    }
    let last = view.read_with(cx, |v, _| v.chapter_index());
    assert!(
        last >= 30,
        "短章的连续滚动该能一直往下一章走，实际停在 {last}"
    );
}

/// 「长章 + 两小章」语料：照《未来简史》第 3 节那一段的真实形状搭 ——
/// 第 0 章比一屏高（读者停在它中后段），第 1 章两行字，第 2 章一张整页图加一行图说，
/// 第 3 章又是正常正文。后两章连缝加起来比一屏还短，这就是会卡死的那个形状。
fn long_chapter_then_a_short_tail() -> FlowDoc {
    use folio_core::content::format::ImageRes;

    // 33 行目录：16px × 1.75 行高 = 28px 一行，整章 924px（真机那本量出来正是 924）。
    let toc = (0..33)
        .map(|i| format!("<div>目录第{i}行</div>"))
        .collect::<String>();
    FlowDoc {
        chapters: vec![
            Chapter::inline("目录", toc),
            Chapter::inline("献词", format!("<p>{PARA}</p>")),
            Chapter::inline(
                "图",
                "<div class=\"chatu\"><img src=\"fig.jpg\" alt=\"\" />\
                 <p class=\"tushuo\">图1 一行图说</p></div>",
            ),
            Chapter::inline(
                "第一章",
                (0..20)
                    .map(|k| format!("<p>{PARA}（第{k}段）</p>"))
                    .collect::<String>(),
            ),
        ],
        images: vec![ImageRes {
            href: "fig.jpg".into(),
            source: "fig.jpg".into(),
            media_type: "image/jpeg".into(),
            width: 1000,
            height: 700,
        }],
        ..Default::default()
    }
}

/// 窗口里各章排出来的高度（`0` = 还没排）。
fn chapter_heights(view: &Entity<ReaderSession>, cx: &mut TestAppContext) -> Vec<f32> {
    view.read_with(cx, |v, _| {
        let book = v.book().expect("这本书该开着");
        (0..book.chapter_count())
            .map(|i| book.span_doc(i).map(|doc| doc.height()).unwrap_or_default())
            .collect()
    })
}

/// 长章后面接着两小章：链尾不能停在「一屏装不满」的地方。
///
/// 读者停在长章中后段时，章首到视口那段已经滚过去了。前向预算若从**锚章章首**算起，
/// 长章自己就把「一屏半」吃光，前方只剩 `SPAN_WINDOW` 那两章的兜底；那两章连缝若比一屏
/// 还短，`content_height` 到它们末尾就没了 —— 视口顶最远只走得到链尾减一屏，永远进不了
/// 下一章：锚章不迁移、窗口不往前长，屏幕就钉在图与图说上（页脚还念 100%）。实测真机
/// 症状与这一条逐字对应。
///
/// 与 `scrolling_advances_through_short_chapters` 是一对：那一条钉「锚章自己就是短章」，
/// 这一条钉「锚章很长、前方那两章很短」。
#[gpui_kit::test]
fn a_long_chapter_with_a_short_tail_still_scrolls_on(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let prefs = prefs(16.0);
    let (view, handle) = open(
        cx,
        &long_chapter_then_a_short_tail(),
        "epub",
        prefs.clone(),
        Locator::start(),
        460.0,
    );

    // 语料的前提（也就是真机上卡死的那条几何）：尾巴两章连缝比一屏还短。
    // 少了这一条，哪天语料被改高了，这条用例会绿着却什么都没测。
    let viewport = view.read_with(cx, |v, _| v.laid_out.viewport);
    let heights = chapter_heights(&view, cx);
    let gap = crate::ui::features::reader::book::Book::chapter_gap(&prefs);
    let tail = heights[1] + gap + heights[2];
    assert!(
        tail < viewport,
        "尾巴要短于一屏才会卡死：两章连缝 {tail:.0}px，视口 {viewport:.0}px"
    );

    assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 0);
    for _ in 0..200 {
        scroll_down(cx, handle, 100.0);
        settle(cx, handle);
        if view.read_with(cx, |v, _| v.chapter_index()) >= 3 {
            break;
        }
    }
    let last = view.read_with(cx, |v, _| v.chapter_index());
    assert!(
        last >= 3,
        "图说后面那一章该滚得到，实际停在 {last}（窗口 {:?}）",
        view.read_with(cx, |v, _| v.span_window())
    );
}

// ---- 翻页动画：左右平移 / 上下平移 / 连续滚动的补间（`chrome::slide_shifts`、`turn_frame`）----

/// 走一帧翻页动画：先把测试时钟推进 `wait`，再渲一帧。
///
/// 必须推时钟：`Presence` 的进度取自 `background_executor().now()`，而测试时钟不会自己走
/// （`run_until_parked` 只走到下一个到期的定时器）。不推就是永远停在起步那一格 ——
/// 那种红看着像「动画没接上」，其实只是表没走。
fn turn_frame(cx: &mut TestAppContext, handle: AnyWindowHandle, wait: Duration) {
    cx.executor().advance_clock(wait);
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("渲染应成功");
    cx.run_until_parked();
}

/// 这一帧重建了几次场景。分页且定住时应当是 1（一栏一份），动画在走时是 2
/// （出场页 + 进场页各一份）—— 这是「两张纸真的都在树上」的唯一可测证据，
/// 因为位移本身看不见，而少摆一张的画法在屏幕上只是「下一页没来」。
fn rebuilds_this_frame(cx: &mut TestAppContext, handle: AnyWindowHandle, wait: Duration) -> u64 {
    take_local(Metric::SceneRebuilds);
    turn_frame(cx, handle, wait);
    take_local(Metric::SceneRebuilds)
}

/// 把翻页动画的两张纸摆上屏：分页翻页在走的那一段，出场页与进场页**同时**在场，
/// 方向是「下一页从右外进来、上一页往左走」，走完就只画一张。
#[gpui_kit::test]
fn a_paged_turn_draws_both_pages_until_the_slide_has_run(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    settle(cx, handle);
    let first = view.read_with(cx, |v, _| v.page());
    assert!(
        view.read_with(cx, |v, _| v.turn.is_none()),
        "定住的时候不该有动画挂着"
    );
    assert_eq!(
        rebuilds_this_frame(cx, handle, Duration::from_millis(1)),
        1,
        "定住的一帧只该摆当前这一页"
    );

    view.update(cx, |v, cx| v.scroll_page(1, cx));
    // 起步那一帧：时钟还没走，进场那张整页停在阅读方向的前方外，出场那张还占着屏。
    let rebuilds = rebuilds_this_frame(cx, handle, Duration::from_millis(1));
    assert_eq!(
        rebuilds, 2,
        "翻页在走，出场页与进场页要各摆一份场景（拿到 {rebuilds} 份）"
    );
    let (turn, shift) = view.read_with(cx, |v, _| (v.turn, v.page_shift));
    let turn = turn.expect("起步那一帧动画还挂着");
    assert_eq!(turn.leaving, Some(first), "出场页该是刚离开的那一页");
    assert!(turn.forward, "往下一页翻 ⇒ 方向是往后");
    assert!(
        shift.0 > 100.0 && shift.1 == 0.0,
        "左右平移应当只沿 x 走，实得 {shift:?}"
    );

    // 走到一半：进场那张在靠近，还没落位。
    turn_frame(cx, handle, Duration::from_millis(140));
    let half = view.read_with(cx, |v, _| v.page_shift);
    assert!(
        half.0 > 0.0 && half.0 < shift.0,
        "位移该单调收小：{shift:?} → {half:?}"
    );

    // 走完：状态收干净，第二张不再入树。
    let rebuilds = rebuilds_this_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));
    assert_eq!(
        rebuilds, 1,
        "走完的那一帧不该再摆出场页（拿到 {rebuilds} 份场景）"
    );
    view.read_with(cx, |v, _| {
        assert!(v.turn.is_none(), "走完该把动画状态收掉");
        assert_eq!(v.page_shift, (0.0, 0.0), "定住后当前页不该带着位移");
    });
    assert_eq!(view.read_with(cx, |v, _| v.page()), first + 1);
}

/// 竖档只沿 y 走：接到 x 上不报错，画面是「一页从左边滑进来」，而这是这一档唯一的
/// 可见差别，所以钉在位移的两个分量上。
#[gpui_kit::test]
fn the_vertical_axis_moves_the_page_up_not_across(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    settle(cx, handle);
    // 轴是**全局**偏好（`appearance.page_turn`），不挂在书上：走的是配置那条路，
    // 与设置页里那一档同一个入口。
    cx.update(|app| {
        ConfigHolder::edit_appearance(app, |a| a.page_turn = PageTurn::Vertical);
    });
    turn_frame(cx, handle, Duration::from_millis(1));

    view.update(cx, |v, cx| v.scroll_page(1, cx));
    turn_frame(cx, handle, Duration::from_millis(1));
    let shift = view.read_with(cx, |v, _| v.page_shift);
    assert_eq!(shift.0, 0.0, "竖档不该在 x 上留下位移：{shift:?}");
    assert!(shift.1 > 100.0, "竖档起步该整页偏在 y 上：{shift:?}");

    // 翻回上一页：方向反号，仍然是竖着走。
    turn_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));
    view.update(cx, |v, cx| v.scroll_page(-1, cx));
    turn_frame(cx, handle, Duration::from_millis(1));
    let back = view.read_with(cx, |v, _| v.page_shift);
    assert_eq!(back.0, 0.0, "往回翻也不该在 x 上位移：{back:?}");
    assert!(back.1 < 0.0, "往回翻时进场那张该从上方来：{back:?}");
}

/// 连续滚动的「上下滚动」：一次离散翻页跳的是**画出来的**那个 y，
/// 从起点补间到终点，而 `scroll_y` 一开始就是终点（那才是权威位置、落库的口径）。
#[gpui_kit::test]
fn a_continuous_turn_tweens_the_painted_position(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    settle(cx, handle);
    let start = view.read_with(cx, |v, _| v.scroll_y);

    view.update(cx, |v, cx| v.scroll_page(1, cx));
    let (target, turn) = view.read_with(cx, |v, _| (v.scroll_y, v.turn));
    assert!(
        target > start,
        "连续模式点一下要往前一屏：{start} → {target}"
    );
    let turn = turn.expect("离散翻页该起一趟补间");
    assert!(turn.scroll.is_some(), "连续那一臂的两端在这里");
    assert_eq!(turn.scroll.expect("上面刚判过"), (start, target));

    turn_frame(cx, handle, Duration::from_millis(1));
    let painted = view.read_with(cx, |v, _| v.scroll_painted);
    assert!(
        (painted - start).abs() < 1.0,
        "起步该还画在起点上，实得 {painted}（起点 {start}）"
    );

    turn_frame(cx, handle, Duration::from_millis(140));
    let half = view.read_with(cx, |v, _| v.scroll_painted);
    assert!(
        half > start && half < target,
        "走到一半该落在两点之间：{start} < {half} < {target}"
    );

    turn_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));
    let (painted, settled) = view.read_with(cx, |v, _| (v.scroll_painted, v.turn));
    assert!(settled.is_none(), "走完要把补间收掉");
    assert!(
        (painted - target).abs() < 1.0,
        "定住后画的位置就是权威位置：{painted} vs {target}"
    );
}

/// 手指还在屏上，补间就该让位：滚轮事件一到当场落定，别让一张纸追在手指后面滑。
#[gpui_kit::test]
fn a_wheel_event_cancels_a_scroll_tween(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    settle(cx, handle);
    view.update(cx, |v, cx| v.scroll_page(1, cx));
    turn_frame(cx, handle, Duration::from_millis(60));
    assert!(view.read_with(cx, |v, _| v.turn.is_some()), "补间还在走");

    scroll_down(cx, handle, 40.0);
    assert!(
        view.read_with(cx, |v, _| v.turn.is_none()),
        "滚轮一到位，补间必须当场让路"
    );
    turn_frame(cx, handle, Duration::from_millis(1));
    let (painted, target) = view.read_with(cx, |v, _| (v.scroll_painted, v.scroll_y));
    assert_eq!(painted, target, "没有动画在走时，画的位置就是权威位置");
}

/// 系统不让动的时候（减弱动态效果）：翻页当场落位，一张多余的纸都不画。
/// 判据不能只写「时长归零」—— 那要落到 `Presence` 给终值、而给终值那一帧仍可能多摆一张。
#[gpui_kit::test]
fn reduced_motion_lands_the_page_without_drawing_a_second_one(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    settle(cx, handle);
    cx.update(|app| app.set_reduce_motion(true));
    let first = view.read_with(cx, |v, _| v.page());

    view.update(cx, |v, cx| v.scroll_page(1, cx));
    let rebuilds = rebuilds_this_frame(cx, handle, Duration::from_millis(1));
    assert_eq!(
        rebuilds, 1,
        "减动效那一档只画当前这一页（拿到 {rebuilds} 份场景）"
    );
    let (turn, shift, page) = view.read_with(cx, |v, _| (v.turn, v.page_shift, v.page()));
    assert!(turn.is_none(), "减动效不该留下还在走的动画");
    assert_eq!(shift, (0.0, 0.0), "位移当场归零");
    assert_eq!(page, first + 1, "页号照旧往前，动画让路而不是拦下这一翻");
}
