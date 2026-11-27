//! 内嵌音频行的排版用例：占位、混排顺序、两趟（全量/量高）一致。
//!
//! 借 [`super`] 的 mock `Measure` 与 `typeset` 夹具；判据的理由写在各用例上。

use super::*;
use folio_format::css::computed::cascade;
use folio_format::css::{Origin, RuleSet};
use folio_format::dom::Dom;

// ---- 内嵌音频行 ----
//
// UA 把 `<audio>` 默认标成 inline，但占位行必须进块表：行内判断放它过关、块级支路在
// 浮动盒之前接住（collect.rs 的三处改动）。这一组用例钉住三件事：占位、混排顺序、
// 两趟（全量/量高）口径一致。

#[test]
fn audio_with_src_takes_a_row_even_when_inline() {
    // UA 默认 inline：这一条全量路径要靠「行内判断放音频过关」才占得到行。
    let (_, layout) = typeset("<body><audio src=\"a.mp3\"></audio></body>", "", 200.0);
    assert_eq!(
        layout.audio_rows.len(),
        1,
        "带 src 的 inline audio 也占一条行"
    );
    let row = &layout.audio_rows[0];
    assert_eq!(row.src, "a.mp3");
    assert!(
        (row.height - 48.0).abs() < 0.5,
        "占位高写死 48px，实测 {}",
        row.height
    );
    assert!(
        (layout.height - 48.0).abs() < 0.5,
        "章高就是这条行，实测 {}",
        layout.height
    );
    // 块表里同样有一条 border 盒，几何与短表一致。
    let block = layout
        .blocks
        .iter()
        .find(|block| block.text.is_empty() && block.height > 40.0)
        .expect("块表里应有音频占位盒");
    assert!((block.height - 48.0).abs() < 0.5);
}

#[test]
fn audio_row_keeps_inline_order_and_two_passes_agree() {
    let html = "<body><p>前文<audio src=\"x.mp3\"></audio>后文</p></body>";
    let (_, full) = typeset(html, "", 200.0);
    assert_eq!(full.audio_rows.len(), 1);
    assert_eq!(texts(&full)[0], "前文", "音频前面的行内内容先冲成匿名块");
    assert_eq!(
        *texts(&full).last().unwrap(),
        "后文",
        "音频后面的行内内容跟在行后"
    );
    // 音频行在第一段文字之后：y 大于首行顶。
    let row = &full.audio_rows[0];
    assert!(row.y > full.lines[0].y, "音频行排在正文之后");

    // 量高那一趟（children_light）必须给出同一个数，否则滚动位置与块对不上。
    let dom = Dom::parse(html.as_bytes());
    let styles = cascade(
        &dom,
        &RuleSet::parse("", Origin::Author),
        &ComputedStyle::default(),
        "",
    );
    let light = layout(
        &dom,
        &styles,
        find(&dom, "body"),
        LayoutOptions {
            width: 200.0,
            light: true,
            allow_vertical: false,
            page_height: None,
            screen_width: None,
            screen_height: None,
        },
        &Mono,
        &NoImages,
    );
    assert!(
        (light.height - full.height).abs() < 0.5,
        "两趟高不一致：量高 {} / 实排 {}",
        light.height,
        full.height
    );
}

#[test]
fn audio_without_src_or_display_none_takes_no_row() {
    // 没有 src 的 audio 播不了：不占位（浏览器「无声的 audio 不渲染控件」同一口径）。
    let (_, bare) = typeset("<body><audio></audio></body>", "", 200.0);
    assert!(bare.audio_rows.is_empty(), "无 src 不占位");

    // display:none 照常脱流：短表与块表都不出现。
    let (_, hidden) = typeset(
        "<body><audio src=\"a.mp3\"></audio></body>",
        "audio { display: none }",
        200.0,
    );
    assert!(hidden.audio_rows.is_empty(), "display:none 不占位");
    assert!(
        (hidden.height).abs() < 0.5,
        "隐藏后章高为零，实测 {}",
        hidden.height
    );
}
