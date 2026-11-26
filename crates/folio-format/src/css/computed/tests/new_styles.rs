//! 新增的样式开关回归：OpenType 特性、毛玻璃、cursor、column-fill、column-rule。
//!
//! 从 `tests.rs` 切出来只因为那个文件贴着行数上限；辅助函数仍从父模块取。

use super::{author, style, tag};
use crate::css::computed::{
    BorderStyle, ColumnFill, ColumnSpan, Cursor, FontKerning, FontSmoothing, FontStretch,
    TextCombine, TextRendering,
};

/// `font-stretch` 九档关键字与百分比都按宽度档落字段；百分比按 CSS 边界就近折；
/// 继承，越界整条作废。连续可变轴仍不在这一档表达。
#[test]
fn font_stretch_keywords_and_percentages_are_recognised() {
    let page = author(
        "<p>字</p><b>字</b>",
        "body { font-stretch: condensed } b { font-stretch: 125% }",
    );
    assert_eq!(
        style(&page, tag(&page, "p")).font_stretch,
        FontStretch::Condensed
    );
    assert_eq!(
        style(&page, tag(&page, "b")).font_stretch,
        FontStretch::SemiExpanded,
        "125% 正落在 semi-expanded 的下边界"
    );
    assert_eq!(page.styled.ignored, 0);

    let normal = author("<p>x</p>", "p { font-stretch: 100% }");
    assert_eq!(
        style(&normal, tag(&normal, "p")).font_stretch,
        FontStretch::Normal
    );
    let bad = author("<p>x</p>", "p { font-stretch: 250% }");
    assert_eq!(bad.styled.ignored, 1);
    assert_eq!(
        style(&bad, tag(&bad, "p")).font_stretch,
        FontStretch::Normal
    );
}

/// 三个 OpenType 开关都落在 shaping 表里：可继承、`text-rendering` 的速度档先关、
/// `font-feature-settings` 后写覆盖；标签写坏整条作废。
#[test]
fn font_kerning_features_and_text_rendering_reach_the_shaping_table() {
    let page = author(
        "<p>字字</p>",
        "body { font-kerning: none; text-rendering: optimizeSpeed; \
         font-feature-settings: \"liga\" 0, palt 1 }",
    );
    let p = style(&page, tag(&page, "p"));
    assert_eq!(p.font_kerning, FontKerning::None);
    assert_eq!(p.text_rendering, TextRendering::OptimizeSpeed);
    assert_eq!(
        p.font_features.as_ref(),
        &[("liga".to_string(), 0), ("palt".to_string(), 1)],
        "特性表按书写顺序留下来"
    );
    let features = p.shaping_features();
    assert_eq!(
        features.iter().filter(|(t, _)| t == "kern").count(),
        1,
        "同名标签只留一条"
    );
    assert_eq!(features.iter().find(|(t, _)| t == "kern").unwrap().1, 0);
    assert_eq!(features.iter().find(|(t, _)| t == "liga").unwrap().1, 0);
    assert_eq!(features.iter().find(|(t, _)| t == "calt").unwrap().1, 0);
    assert_eq!(features.iter().find(|(t, _)| t == "palt").unwrap().1, 1);
    assert_eq!(page.styled.ignored, 0, "写对了的三条都不该计落差");

    let bad = author("<p>x</p>", "p { font-feature-settings: 'toolong' 1 }");
    assert_eq!(bad.styled.ignored, 1, "五字符的标签不是 OpenType tag");
}

/// `-webkit-font-smoothing` 可继承，四档逐元素覆盖；写坏整条作废。
#[test]
fn font_smoothing_is_inherited_and_overridable() {
    let page = author(
        "<p>字</p><b>字</b><i>字</i>",
        "body { -webkit-font-smoothing: antialiased } \
         b { -webkit-font-smoothing: subpixel-antialiased } \
         i { -webkit-font-smoothing: none }",
    );
    assert_eq!(
        style(&page, tag(&page, "p")).font_smoothing,
        FontSmoothing::Antialiased
    );
    assert_eq!(
        style(&page, tag(&page, "b")).font_smoothing,
        FontSmoothing::SubpixelAntialiased
    );
    assert_eq!(
        style(&page, tag(&page, "i")).font_smoothing,
        FontSmoothing::None
    );
    assert_eq!(page.styled.ignored, 0);

    let bad = author("<p>x</p>", "p { -webkit-font-smoothing: smooth }");
    assert_eq!(bad.styled.ignored, 1);
    assert_eq!(
        style(&bad, tag(&bad, "p")).font_smoothing,
        FontSmoothing::Auto
    );
}

/// `opacity` 的绘制值乘祖先：层叠仍看本元素自己的 `opacity`。
#[test]
fn nested_opacity_multiplies_for_painting_only() {
    let page = author(
        "<div><p>字</p></div>",
        "div { opacity: 0.5 } p { opacity: 0.5 }",
    );
    let p = style(&page, tag(&page, "p"));
    assert_eq!(p.opacity, 0.5, "层叠上下文只认本元素的声明");
    assert_eq!(p.paint_opacity, 0.25, "绘制 alpha 要乘祖先");
    assert_eq!(page.styled.ignored, 0);
}

/// 毛玻璃只收单个 `blur()`：连写别的滤镜整条作废，不偷偷只做一半。
#[test]
fn backdrop_filter_takes_one_blur_and_rejects_filter_chains() {
    let page = author("<div>x</div>", "div { backdrop-filter: blur(6px) }");
    assert_eq!(style(&page, tag(&page, "div")).backdrop_blur, Some(6.0));
    assert_eq!(page.styled.ignored, 0);

    let bad = author(
        "<div>x</div>",
        "div { -webkit-backdrop-filter: blur(6px) brightness(0.5) }",
    );
    assert_eq!(bad.styled.ignored, 1, "滤镜链不在这一层做");
    assert_eq!(style(&bad, tag(&bad, "div")).backdrop_blur, None);

    let none = author("<div>x</div>", "div { backdrop-filter: none }");
    assert_eq!(style(&none, tag(&none, "div")).backdrop_blur, None);
    assert_eq!(none.styled.ignored, 0);
}

/// `cursor` 可继承，认得的关键字进枚举；没有对应形状的照实计不认识。
#[test]
fn cursor_keywords_are_recognised_and_inherited() {
    let page = author(
        "<p><a href=\"#x\">链</a></p>",
        "body { cursor: pointer } a { cursor: default }",
    );
    assert_eq!(style(&page, tag(&page, "p")).cursor, Cursor::Pointer);
    assert_eq!(style(&page, tag(&page, "a")).cursor, Cursor::Default);
    assert_eq!(page.styled.ignored, 0);

    let bad = author("<p>x</p>", "p { cursor: wait }");
    assert_eq!(bad.styled.ignored, 1, "gpui 没有等待形状，不悄悄当 auto");
    assert_eq!(style(&bad, tag(&bad, "p")).cursor, Cursor::Auto);
}

/// `column-fill` 两档都认；写坏的值整条作废。
#[test]
fn column_fill_parses_both_keywords() {
    let page = author("<div>x</div>", "div { column-count: 2; column-fill: auto }");
    let div = style(&page, tag(&page, "div"));
    assert_eq!(div.column_count, Some(2));
    assert_eq!(div.column_fill, ColumnFill::Auto);
    assert_eq!(page.styled.ignored, 0);

    let balanced = author("<div>x</div>", "div { column-fill: balance }");
    assert_eq!(
        style(&balanced, tag(&balanced, "div")).column_fill,
        ColumnFill::Balance
    );

    let bad = author("<div>x</div>", "div { column-fill: balance-all }");
    assert_eq!(bad.styled.ignored, 1);
    assert_eq!(
        style(&bad, tag(&bad, "div")).column_fill,
        ColumnFill::Balance,
        "作废的声明不改默认值"
    );
}

/// `column-rule` 三要素与简写：与边框同构、缺项按 `medium none currentColor` 重置、
/// 不继承；写坏的值整条作废。
#[test]
fn column_rule_shorthand_and_longhands_are_applied() {
    let page = author(
        "<div><p>字</p></div>",
        "div { column-count: 2; column-rule: 1px dashed #123456 }",
    );
    let div = style(&page, tag(&page, "div"));
    assert_eq!(div.column_rule.width, 1.0);
    assert_eq!(div.column_rule.style, BorderStyle::Dashed);
    assert_eq!(div.column_rule.color.map(|c| c.0), Some(0x12));
    assert_eq!(
        style(&page, tag(&page, "p")).column_rule.style,
        BorderStyle::None,
        "column-rule 不继承"
    );
    assert_eq!(page.styled.ignored, 0);

    // 只给 style：宽回到 medium、色回到 None（currentColor），不是保留前一条的宽。
    let style_only = author(
        "<div>x</div>",
        "div { column-rule-width: 5px; column-rule: solid }",
    );
    let div = style(&style_only, tag(&style_only, "div"));
    assert_eq!(div.column_rule.width, 3.0);
    assert_eq!(div.column_rule.style, BorderStyle::Solid);
    assert_eq!(div.column_rule.color, None);

    // 三条子属性各自只改一格。
    let longhands = author(
        "<div>x</div>",
        "div { column-rule-width: thin; column-rule-style: double; column-rule-color: red }",
    );
    let div = style(&longhands, tag(&longhands, "div"));
    assert_eq!(div.column_rule.width, 1.0);
    assert_eq!(div.column_rule.style, BorderStyle::Double);
    assert!(div.column_rule.color.is_some());

    let bad = author("<div>x</div>", "div { column-rule: 1px groovy }");
    assert_eq!(bad.styled.ignored, 1);
}

/// `column-span` 只收 `none` / `all`，不继承，写坏整条作废。
#[test]
fn column_span_recognises_all_and_none() {
    let page = author(
        "<div><p>字</p></div>",
        "div { column-count: 2; column-span: all }",
    );
    assert_eq!(style(&page, tag(&page, "div")).column_span, ColumnSpan::All);
    assert_eq!(
        style(&page, tag(&page, "p")).column_span,
        ColumnSpan::None,
        "column-span 不继承"
    );
    assert_eq!(page.styled.ignored, 0);

    let none = author("<div>x</div>", "div { column-span: none }");
    assert_eq!(
        style(&none, tag(&none, "div")).column_span,
        ColumnSpan::None
    );
    let bad = author("<div>x</div>", "div { column-span: start }");
    assert_eq!(bad.styled.ignored, 1);
}

/// `text-combine-upright` 与旧 WebKit 名共用一档；`digits 2` 起按两位并格，
/// 越界与多余 token 整条作废。`text_combine` 可继承。
#[test]
fn text_combine_recognises_upright_and_webkit_names() {
    let page = author(
        "<p>12</p><b>34</b>",
        "body { -webkit-text-combine: horizontal } b { text-combine-upright: digits 2 }",
    );
    assert_eq!(style(&page, tag(&page, "p")).text_combine, TextCombine::All);
    assert_eq!(
        style(&page, tag(&page, "b")).text_combine,
        TextCombine::Digits(2)
    );
    let page = author(
        "<p>123</p><b>1234</b>",
        "p { text-combine-upright: digits 3 } b { text-combine-upright: digits 4 }",
    );
    assert_eq!(
        style(&page, tag(&page, "p")).text_combine,
        TextCombine::Digits(3)
    );
    assert_eq!(
        style(&page, tag(&page, "b")).text_combine,
        TextCombine::Digits(4)
    );
    assert_eq!(page.styled.ignored, 0);

    let bad = author("<p>x</p>", "p { text-combine-upright: digits 5 }");
    assert_eq!(bad.styled.ignored, 1);
    assert_eq!(style(&bad, tag(&bad, "p")).text_combine, TextCombine::None);
}
