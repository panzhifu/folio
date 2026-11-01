//! 排版引擎经真实 EPUB provider 与字体栈的集成验收（M1–M3 的门）。

use super::harness::{epub_flow, open, prefs, settle};
use folio_core::{Chapter, FlowDoc, Locator, Reader};
#[allow(unused_imports)] // `#[gpui_kit::test]` 展开要它，unused 分析看不见宏里的使用
use gpui_kit::{AppContext as _, TestAppContext};

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

    // 标点挤压会把收尾标点切成单独一段，run 不再按「一行一组」聚；这里按整屏的
    // run 判三种局部样式都在，不再要求它们恰好落在同一条「≥4 run」的行上。
    let all: Vec<_> = lines.iter().flat_map(|l| l.text_runs()).collect();
    assert!(
        all.iter().any(|r| r.style.font.weight >= 700),
        "strong 应带粗体"
    );
    assert!(all.iter().any(|r| r.style.font.italic), "em 应带斜体");
    assert!(
        all.iter()
            .any(|r| r.style.color == Some(Color(0xC0, 0x39, 0x2B, 255))),
        "span.red 的局部颜色要落到 run 上"
    );
    assert!(
        all.iter().all(|r| r.width > 0.0),
        "每个 run 都要有实际绘制宽度"
    );
    let line = lines
        .iter()
        .find(|l| l.text_runs().next().is_some())
        .expect("至少有一行文本");
    let sum: f32 = line.text_runs().map(|r| r.width).sum();
    assert!(
        (sum - line.width).abs() < 0.5,
        "行宽应是各 run 宽度之和：{sum} vs {}",
        line.width
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

#[gpui_kit::test]
#[ignore = "探针：读 /tmp/folio-css/h2probe.epub（本机合成的那一份），CI 机器上没那个目录"]
fn scratch_probe_h2_metrics(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let path = std::path::Path::new("/tmp/folio-css/h2probe.epub");
    let doc = crate::ui::features::reader::model::chapter_layout::open(path).expect("probe 开得出");
    let mut flow = doc.flow().expect("可重排").clone();
    for (ix, chapter) in flow.chapters.iter_mut().enumerate() {
        let text = doc.chapter_source(ix).expect("正文");
        *chapter = folio_core::Chapter::inline(chapter.title.clone(), text)
            .with_spine_href(chapter.spine_href.clone());
    }
    flow.css = doc.stylesheet();
    let (view, handle) = super::harness::open(
        cx,
        &flow,
        "epub",
        super::harness::prefs(16.0),
        folio_core::Locator::start(),
        460.0,
    );
    super::harness::settle(cx, handle);
    let lines = view.read_with(cx, |v, _| v.lines());
    let texts = view.read_with(cx, |v, _| v.line_texts());
    for (l, t) in lines.iter().zip(texts.iter()) {
        println!(
            "y={:.1} h={:.1} bl={:.1} size={:.1} asc={:.1} desc={:.1} text={}",
            l.y,
            l.height,
            l.baseline,
            l.dominant_font_size(),
            l.text_runs().map(|r| r.ascent).fold(0.0f32, f32::max),
            l.text_runs().map(|r| r.descent).fold(0.0f32, f32::max),
            t
        );
    }
}

/// 一叠三张图：一帧只画一张（角上有 `k/n`），切一张之后**不许重排**。
///
/// 「现在看第几张」住界面（`Document::stack_current`），它不进 `LayoutKey`、不进样式哈希 ——
/// 那等于每划一下重排整章，分页档的页数还会跟着变 —— 那是硬约束。
/// 这条门量的就是这一格：切图前后行数与页数一个数都不许动，而画出来的确实换了一张。
#[gpui_kit::test]
fn a_stack_draws_one_member_and_switches_without_relayout(cx: &mut TestAppContext) {
    use crate::ui::foundation::images::{BookImages, ChapterImages};
    use gpui_kit::test::TestWindowExt as _;

    cx.update(gpui_kit::init);
    let png: Vec<u8> = {
        use image::ImageEncoder as _;
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(
                &vec![200u8; 600 * 300 * 3],
                600,
                300,
                image::ExtendedColorType::Rgb8,
            )
            .expect("夹具 PNG 该编得出来");
        bytes
    };
    let image = |href: &str| folio_core::content::format::ImageRes {
        href: href.into(),
        source: href.into(),
        media_type: "image/png".into(),
        width: 600,
        height: 300,
    };
    let images = vec![image("a.png"), image("b.png"), image("c.png")];
    let flow = FlowDoc {
        chapters: vec![Chapter::inline(
            "画廊",
            "<p><img src=a.png></p><p><img src=b.png></p><p><img src=c.png></p><p>正文</p>",
        )],
        images: images.clone(),
        ..Default::default()
    };
    let meta = super::harness::meta("epub");
    let doc: std::sync::Arc<dyn folio_core::content::document::Document> = std::sync::Arc::from(
        folio_core::content::document::from_content(meta, folio_core::Content::Reflow(flow)),
    );
    let (view, handle) = super::harness::build_with_document(
        cx,
        doc,
        prefs(16.0),
        Locator::start(),
        460.0,
        None,
        None,
        folio_core::PageTurn::Slide,
    );
    // `from_content` 造出来的书没有容器，`Book::new` 会把图片索引换成空表 —— 补上真清单
    // 与内存字节，走的是生产上 EPUB 那一条（同 `capture.rs` 那一族）。
    view.update(cx, |v, _| {
        let book = match &mut v.source {
            crate::ui::features::reader::model::session::Source::Book(book) => book,
            _ => panic!("前置：这一本该是可重排那一族"),
        };
        let bytes = png.clone();
        let source: crate::ui::foundation::images::ByteSource =
            std::sync::Arc::new(move |_href: &str| Some(bytes.clone()));
        let index = std::sync::Arc::new(BookImages::new(&images, source));
        book.document_mut()
            .set_images(std::sync::Arc::new(ChapterImages::new(index, "c0.xhtml")));
    });
    settle(cx, handle);
    let marks = view.read_with(cx, |v, _| {
        v.book()
            .expect("有书")
            .document()
            .layout()
            .lines
            .iter()
            .filter_map(|line| line.stack)
            .collect::<Vec<_>>()
    });
    assert_eq!(marks.len(), 3, "三张图都该带叠标记：{marks:?}");
    assert!(
        marks.iter().all(|m| m.count == 3 && m.id == marks[0].id),
        "一叠三格：{marks:?}"
    );
    let badge = cx
        .update_window(handle, |_, window, _| {
            window.try_find("reader-stack-badge").is_some()
        })
        .expect("窗口应能渲染");
    assert!(
        badge,
        "一叠只画一张：角标只该有一条，读者才知道这里还有两张"
    );

    let (lines, pages) = view.read_with(cx, |v, _| (v.lines().len(), v.page_count()));
    let moved = view.update(cx, |v, _| {
        let book = v.book().expect("有书");
        let mark = book
            .document()
            .layout()
            .lines
            .iter()
            .find_map(|line| line.stack)
            .expect("上面刚数过有叠");
        book.document().advance_stack(mark.id, 1, mark.count)
    });
    assert!(moved, "第 1 张 → 第 2 张该报「变了」");
    settle(cx, handle);
    let (lines_after, pages_after, current) = view.read_with(cx, |v, _| {
        let book = v.book().expect("有书");
        let mark = book
            .document()
            .layout()
            .lines
            .iter()
            .find_map(|line| line.stack)
            .expect("叠还在");
        (
            v.lines().len(),
            v.page_count(),
            book.document().stack_current(mark.id),
        )
    });
    assert_eq!(current, 1, "切到了第 2 张");
    assert_eq!(
        (lines, pages),
        (lines_after, pages_after),
        "切图不许重排：行数与页数都不该动"
    );
    // 两头停住：回到第一张之后不许回绕，也不该再报「变了」。
    let again = view.update(cx, |v, _| {
        let doc = v.book().expect("有书").document();
        (
            doc.advance_stack(0, -1, 3),
            doc.advance_stack(0, -1, 3),
            doc.stack_current(0),
        )
    });
    assert_eq!(again, (true, false, 0), "走到第一张就该停住，不回绕");

    // 分页档也要那两条「还能滑」的提示。分页这一臂从前只画当前那张（角标有、卡片边
    // 与小点没有），所以分页里叠起来的几张看起来就是一整张图，读者不知道还能横滑。
    view.update(cx, |v, cx| {
        v.set_mode(
            crate::ui::features::reader::model::chrome::PageMode::Paged,
            cx,
        )
    });
    settle(cx, handle);
    let (peek, dots) = cx
        .update_window(handle, |_, window, _| {
            (
                window.try_find("reader-stack-peek").is_some(),
                window.try_find("reader-stack-dots").is_some(),
            )
        })
        .expect("窗口应能渲染");
    assert!(peek, "分页档也该在图下露出「下一张」的卡片边");
    assert!(dots, "分页档也该有这一叠的小点");
}
