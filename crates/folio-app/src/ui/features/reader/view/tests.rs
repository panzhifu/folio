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

use folio_core::content::document::Document;
use folio_core::infra::metrics::{Metric, take_local};
use folio_core::store::Store;
use folio_core::{Chapter, Config, Content, DocMeta, FlowDoc, Format, Locator, Reader};
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

/// 多章语料：每章 30 段（16px 下一章约三个视口高，章内可滚区间远大于
/// 测试的滚动步长，跨章之后认得出读到的是哪一章、进度也还在章首附近）。
fn chapters_flow(count: usize) -> FlowDoc {
    FlowDoc {
        chapters: (0..count)
            .map(|i| {
                let source = (0..30)
                    .map(|k| format!("<p>{PARA}（第{i}章 第{k}段）</p>"))
                    .collect::<String>();
                Chapter::inline(&format!("第{i}章"), source)
            })
            .collect(),
        ..Default::default()
    }
}

fn meta(format: &str) -> DocMeta {
    DocMeta {
        title: "测试书".into(),
        authors: Vec::new(),
        format: Format::parse(format).expect("格式名要认得"),
        size: 0,
    }
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

/// 第 `line` 行第 `k` 个字的中心（窗口坐标）。阅读会话直开时页眉让位为 0、
/// 滚动在章首，所以窗口坐标 = 排版坐标 + 版心左缘。字的横向位置按字号估：
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
            px(box_.y + box_.height * 0.5),
        )
    })
}

/// 按下 → 拖过阈值 → 松手，成立一个选区（共享给几条用例的前置步骤）。
fn drag_select(cx: &mut TestAppContext, view: &Entity<ReaderSession>, handle: AnyWindowHandle) {
    let start = char_point(view, cx, 1, 2);
    let end = char_point(view, cx, 1, 4);
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
    assert!(
        view.read_with(cx, |v, _| v.selection().is_some()),
        "拖过阈值的松手应成立选中"
    );
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

/// 量具在记，而且记下的是**今天真实付的那笔**：只滚动一屏，满屏的 run 就被重新物化一遍、
/// `shape_line` 就被重新调一遍。
///
/// 这条与下面那条 `#[ignore]` 的目标断言**配成一对**读：这条钉的是「计数器真的会动」——
/// 把 `Scene::build` 或 `paint_run` 里那两句计数删掉，它当场红（三格读出来全是零），于是
/// 那条目标门不会变成一盏永远绿的假灯。行单元表接上之后这一条要删掉（那时它反过来才该红）。
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
/// **这一条故意不断 `ShapeLineCalls`**：`paint_run` 仍每 run 调一次 `shape_line`，而 gpui 那个
/// `LineLayoutCache` 只保两帧（`previous_frame` 与 `current_frame` 在 `finish_frame` 里互换）。
/// 把 `ShapedLine` 存进行单元表是**已记为「可省」的下一步**——它省的是那趟整段文本的哈希，
/// 不是这 62 次拷贝，等这条门绿了再按数决定。
///
/// 今天跑它是红的（红在哪个数见上一条门），所以挂 `#[ignore]`：接线之后去掉这一句就成了
/// 守门员（同 `epub::tests` 那条「一百次取图只开一次容器」走过的路）。
#[gpui_kit::test]
#[ignore = "行单元表还没接：滚回走过的地方也要把满屏的 run 重新物化一遍"]
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
