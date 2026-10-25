//! 排版引擎经真实 EPUB provider 与字体栈的集成验收（M1–M3 的门）。

use super::harness::{epub_flow, open};
use folio_core::{Chapter, FlowDoc, Locator, Reader};
use gpui_kit::TestAppContext;

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
