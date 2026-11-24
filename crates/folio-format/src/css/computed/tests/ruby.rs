//! 注音/书写模式/垂直对齐的一组层叠用例。
//! 从 `computed/tests.rs` 切出来只因为那个文件贴着行数上限；共享 helper 经 `super` 取用。

use super::{author, class, page, style, tag};
use crate::css::computed::{
    ComputedStyle, InlineVAlign, Length, WritingMode,
};
use crate::css::Origin;

#[test]
fn the_ua_sheet_halves_the_ruby_annotation() {
    let page = page("<p><ruby>愚者<rt>The Fool</rt></ruby></p>", &[]);
    assert_eq!(
        style(&page, tag(&page, "rt")).font_size,
        8.0,
        "16 px 正文的注音"
    );
    let authored = author(
        "<p><ruby>愚者<rt>The Fool</rt></ruby></p>",
        "p { font-size: 20px } rt { font-size: 0.4em }",
    );
    assert_eq!(
        style(&authored, tag(&authored, "rt")).font_size,
        8.0,
        "作者写了就照作者的走（20 px 的 0.4 倍）"
    );
}

/// `vertical-align: text-top / text-bottom` 不再算「不认识」。
///
/// 这两档要贴父盒字体的上下沿，而父盒的度量要到排版层才量得出来，所以层叠只记关键字
/// （[`InlineVAlign`]），折 px 在 `layout::wrap`。这里钉的是「记下来了、没进 ignored」，
/// 以及后来的值能把它顶掉 —— 同一属性只留最后一条。
#[test]
fn text_top_and_bottom_are_recorded_for_the_layout_to_resolve() {
    let top = author("<p><span>x</span></p>", "span { vertical-align: text-top }");
    assert_eq!(
        style(&top, tag(&top, "span")).inline_valign,
        InlineVAlign::TextTop
    );
    assert_eq!(top.styled.ignored, 0);

    let bottom = author(
        "<p><span>x</span></p>",
        "span { vertical-align: text-bottom }",
    );
    assert_eq!(
        style(&bottom, tag(&bottom, "span")).inline_valign,
        InlineVAlign::TextBottom
    );
    assert_eq!(bottom.styled.ignored, 0);

    // `super` 顶掉关键字：关键字归零，数值位移接班。
    let superseded = author(
        "<p><span>x</span></p>",
        "span { vertical-align: text-top; vertical-align: super }",
    );
    let span = style(&superseded, tag(&superseded, "span"));
    assert_eq!(span.inline_valign, InlineVAlign::Baseline);
    assert!(span.baseline_shift > 0.0, "super 要折成向上的位移");
    assert_eq!(superseded.styled.ignored, 0);
}

/// `-webkit-` / `-epub-` 前缀的 `writing-mode` 与无前缀那条同义。
///
/// 后果不是排版细节：认不出前缀，整本竖排书会横着排 —— 本机那份日文样本
/// （`09-ja-vertical.azw3`）写的正是 `-webkit-writing-mode: vertical-rl`。
#[test]
fn webkit_and_epub_writing_mode_are_the_same_property() {
    for property in ["-webkit-writing-mode", "-epub-writing-mode"] {
        let css = format!("p {{ {property}: vertical-rl }}");
        let page = author("<p>x</p>", &css);
        assert_eq!(
            style(&page, tag(&page, "p")).writing_mode,
            WritingMode::VerticalRl,
            "{property} 要认成 writing-mode"
        );
        assert_eq!(page.styled.ignored, 0, "{property} 不该进 ignored");
    }
}

/// `text-orientation: mixed` 是初值，收下等于没改；`upright` / `sideways` 要逐字形旋转，
/// 排版层还没有那条路，照实计不认识。
#[test]
fn text_orientation_mixed_is_a_no_op_but_upright_is_not() {
    let mixed = author("<p>x</p>", "p { text-orientation: mixed }");
    assert_eq!(mixed.styled.ignored, 0);
    let upright = author("<p>x</p>", "p { text-orientation: upright }");
    assert_eq!(upright.styled.ignored, 1, "upright 还没做，不许假装接住");
}

/// `color: initial` 落成「读者主题的正文色」，不是写死黑色。
///
/// `ComputedStyle::color` 的 `None` 正是「没有作者色、听主题」的记号；暗色主题下
/// 写死黑字等于把这一行藏掉。`inherit` / `currentColor` 仍走父级那一档。
#[test]
fn color_initial_resets_to_the_theme_default_not_black() {
    let page = author(
        "<div><p class=\"a\">x</p></div>",
        "div { color: red } .a { color: initial }",
    );
    assert_eq!(
        style(&page, class(&page, "a")).color,
        None,
        "initial 该回主题色（None），不是父级的红、也不是黑"
    );
    assert_eq!(page.styled.ignored, 0);

    let inherit = author(
        "<div><p class=\"a\">x</p></div>",
        "div { color: red } .a { color: currentColor }",
    );
    assert_eq!(
        style(&inherit, class(&inherit, "a")).color,
        style(&inherit, tag(&inherit, "div")).color,
        "currentColor 仍是父级那一份"
    );
}

/// HTML 尺寸属性里的 `%` 判的是数字后面紧跟的那一格，不是整串末尾。
///
/// 多看导出的《藩屏》封面页写的是 `<img width="100%;">`：百分号后拖一条分号。
/// 按「整串以 % 结尾」判会落进像素那一支，把 100% 读成 100px，封面因此在手机上
/// 只画出 100×143 一小块（排版层实测，见 `parse_dimension_attribute` 的注释）。
/// 其余几档也一并钉住：单位后缀照旧被忽略、纯百分比照旧收、无效值照旧不写样式。
#[test]
fn dimension_attribute_reads_the_percent_right_after_the_number() {
    let semicolon = author(r#"<div><img width="100%;" src="c.jpg"/></div>"#, "");
    assert_eq!(
        style(&semicolon, tag(&semicolon, "img")).width,
        Some(Length::Percent(1.0)),
        "width=\"100%;\" 该是 100%，不是 100px"
    );

    let percent = author(r#"<div><img width="40%" src="c.jpg"/></div>"#, "");
    assert_eq!(
        style(&percent, tag(&percent, "img")).width,
        Some(Length::Percent(0.4)),
    );

    let unit = author(
        r#"<div><img width="400px" height="300" src="c.jpg"/></div>"#,
        "",
    );
    let img = tag(&unit, "img");
    assert_eq!(style(&unit, img).width, Some(Length::Px(400.0)));
    assert_eq!(style(&unit, img).height, Some(Length::Px(300.0)));

    let invalid = author(r#"<div><img width="auto" src="c.jpg"/></div>"#, "");
    assert_eq!(
        style(&invalid, tag(&invalid, "img")).width,
        None,
        "认不出的值不许悄悄写成一个尺寸"
    );

    // 呈现提示落在作者样式之前：样式表真写了 width 就以它为准。
    let overridden = author(
        r#"<div><img width="100%;" src="c.jpg"/></div>"#,
        "img { width: 60% }",
    );
    assert_eq!(
        style(&overridden, tag(&overridden, "img")).width,
        Some(Length::Percent(0.6)),
    );
}
