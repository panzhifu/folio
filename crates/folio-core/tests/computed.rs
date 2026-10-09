//! `ComputedStyle` 计算：单位换算、继承、ignored 计数、层叠端到端。
//!
//! 只依赖公开 API。选择器命中和 specificity / `!important` 的排序在 `tests/css.rs` 已覆盖，
//! 这里验的是「拿到有序声明之后算得对不对」。

use folio_core::css::computed::{
    Color, ComputedStyle, Display, Length, StyledDom, TextAlign, cascade,
};
use folio_core::css::{Origin, RuleSet};
use folio_core::dom::{Dom, NodeId};

struct Page {
    dom: Dom,
    styled: StyledDom,
}

fn page(html: &str, sheets: &[(&str, Origin)]) -> Page {
    let dom = Dom::parse(html.as_bytes());
    let mut rules = RuleSet::default();
    for (css, origin) in sheets {
        rules.extend(RuleSet::parse(css, *origin));
    }
    let styled = cascade(&dom, &rules, &ComputedStyle::default());
    Page { dom, styled }
}

fn author(html: &str, css: &str) -> Page {
    page(html, &[(css, Origin::Author)])
}

fn style(page: &Page, id: NodeId) -> &ComputedStyle {
    page.styled.get(id).expect("样式表应覆盖每个节点")
}

fn tag(page: &Page, tag: &str) -> NodeId {
    page.dom
        .elements()
        .find(|(_, n)| {
            n.tag
                .as_ref()
                .map(|t| t.local.to_string())
                .unwrap_or_default()
                == tag
        })
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("文档里应有 <{tag}>"))
}

fn class(page: &Page, want: &str) -> NodeId {
    page.dom
        .elements()
        .find(|(id, _)| page.dom.classes(*id).any(|c| c == want))
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("文档里应有 .{want}"))
}

#[test]
fn no_rules_yields_the_root_base_everywhere() {
    let page = page("<div><p>正文</p></div>", &[]);
    let root = page.dom.document;
    assert_eq!(style(&page, root).font_size, 16.0);
    assert_eq!(style(&page, tag(&page, "p")).font_size, 16.0);
    assert_eq!(page.styled.ignored, 0);
}

/// em 按**元素自己**的字号折算，与声明书写顺序无关。
#[test]
fn em_resolves_against_the_elements_own_font_size() {
    let first = author("<p>甲</p>", "p { font-size: 20px; text-indent: 2em }");
    let second = author("<p>乙</p>", "p { text-indent: 2em; font-size: 20px }");
    assert_eq!(style(&first, tag(&first, "p")).text_indent, 40.0);
    assert_eq!(
        style(&second, tag(&second, "p")).text_indent,
        40.0,
        "font-size 写在后面也要先算"
    );
}

#[test]
fn rem_uses_the_root_font_size_not_the_parent() {
    let page = author(
        "<div><p>文</p></div>",
        "div { font-size: 30px } p { margin-top: 2rem }",
    );
    // root_base 字号是 16，rem 与父元素的 30px 无关。
    assert_eq!(style(&page, tag(&page, "p")).margin_top, 32.0);
}

#[test]
fn pt_converts_at_the_css_ratio() {
    let page = author("<p>字</p>", "p { font-size: 12pt }");
    assert_eq!(style(&page, tag(&page, "p")).font_size, 16.0);
}

#[test]
fn bare_number_is_treated_as_px() {
    let page = author("<p>字</p>", "p { text-indent: 3 }");
    assert_eq!(style(&page, tag(&page, "p")).text_indent, 3.0);
}

#[test]
fn percent_font_size_is_relative_to_the_parent() {
    let page = author(
        "<div><p>文</p></div>",
        "div { font-size: 20px } p { font-size: 50% }",
    );
    assert_eq!(style(&page, tag(&page, "p")).font_size, 10.0);
}

#[test]
fn font_size_keywords_and_relative_keywords() {
    let page = author(
        "<div><p>一</p><h2>二</h2></div>",
        "div { font-size: 20px } p { font-size: smaller } h2 { font-size: large }",
    );
    assert_eq!(style(&page, tag(&page, "p")).font_size, 20.0 / 1.2);
    assert_eq!(style(&page, tag(&page, "h2")).font_size, 18.0);
}

#[test]
fn line_height_accepts_unitless_percent_and_px() {
    let page = author(
        "<p class=a>一</p><p class=b>二</p><p class=c>三</p>",
        ".a { line-height: 2 } \
         .b { font-size: 20px; line-height: 150% } \
         .c { font-size: 20px; line-height: 30px }",
    );
    let find = |c: &str| class(&page, c);
    assert_eq!(style(&page, find("a")).line_height, 2.0);
    assert_eq!(style(&page, find("b")).line_height, 1.5);
    assert_eq!(
        style(&page, find("c")).line_height,
        1.5,
        "px 行高要折算成倍数，排版层只认倍数"
    );
}

/// 可继承的往下传，不可继承的从默认值起算。
#[test]
fn only_inheritable_properties_flow_down() {
    let page = author(
        "<div><p>文</p></div>",
        "div { color: red; text-align: center; font-weight: 700; font-style: italic; \
         letter-spacing: 2px; margin-bottom: 12px; padding-top: 8px; display: inline }",
    );
    let p = style(&page, tag(&page, "p"));
    assert_eq!(p.color, Some(Color(255, 0, 0)));
    assert_eq!(p.text_align, TextAlign::Center);
    assert_eq!(p.font_weight, 700);
    assert!(p.font_italic);
    assert_eq!(p.letter_spacing, 2.0);
    assert_eq!(
        p.margin_bottom, 16.0,
        "p 的 1em 下边距来自 UA 表，不是继承父元素的 12px"
    );
    assert_eq!(p.padding_top, 0.0, "padding 不该继承");
    assert_eq!(p.display, Display::Block, "display 不该继承");
}

#[test]
fn text_align_and_indent_survive_several_levels() {
    let page = author(
        "<div class=root><section><p>文</p></section></div>",
        ".root { text-align: justify; text-indent: 2em }",
    );
    let p = style(&page, tag(&page, "p"));
    assert_eq!(p.text_align, TextAlign::Justify);
    assert_eq!(p.text_indent, 32.0, "缩进继承下来但已在根元素折算成 px");
}

#[test]
fn display_none_is_recorded_not_dropped() {
    let page = author("<p>隐</p>", "p { display: none }");
    assert_eq!(style(&page, tag(&page, "p")).display, Display::None);
    assert_eq!(page.styled.ignored, 0);
}

#[test]
fn text_nodes_carry_their_parents_computed_style() {
    let page = author("<p>正文</p>", "p { font-size: 20px; color: #123456 }");
    let p = tag(&page, "p");
    let text = page
        .dom
        .get(p)
        .children
        .first()
        .copied()
        .expect("p 应有文本子节点");
    assert_eq!(page.styled.get(text), page.styled.get(p));
}

#[test]
fn color_forms() {
    let page = author(
        "<p class=a>一</p><p class=b>二</p><p class=c>三</p><p class=d>四</p>",
        ".a{color:#abc} .b{color:#112233} .c{color:rgb(10, 20, 30)} .d{color:Silver}",
    );
    let find = |c: &str| class(&page, c);
    assert_eq!(style(&page, find("a")).color, Some(Color(0xAA, 0xBB, 0xCC)));
    assert_eq!(style(&page, find("b")).color, Some(Color(0x11, 0x22, 0x33)));
    assert_eq!(style(&page, find("c")).color, Some(Color(10, 20, 30)));
    assert_eq!(style(&page, find("d")).color, Some(Color(192, 192, 192)));
    assert_eq!(page.styled.ignored, 0);
}

/// 认不得的值计入 ignored；认识但一期不做的不计。
#[test]
fn unknown_values_count_as_ignored_but_known_lazily_supported_do_not() {
    let page = author(
        "<p>文</p>",
        "p { transform: rotate(3deg); border: 1px solid red; \
         text-decoration: underline; font: 1em serif; width: 100px }",
    );
    assert_eq!(page.styled.ignored, 1, "只有 transform 是清单外属性");
}

/// 百分比外边距要按包含块宽度算，层叠期不知道那个数，所以宁可判不支持。
/// 用 div 而不是 p：div 的 UA 外边距是 0，能看清「被拒的声明什么都没留下」。
#[test]
fn percentage_boxes_are_rejected_rather_than_guessed() {
    let page = author("<div>文</div>", "div { margin-top: 10%; text-indent: 5% }");
    let div = style(&page, tag(&page, "div"));
    assert_eq!(div.margin_top, 0.0);
    assert_eq!(div.text_indent, 0.0);
    assert_eq!(page.styled.ignored, 2);
}

/// `auto` 边距折成 **0**，并且不再计进 `ignored`。
///
/// 这条改动是因为仓库里那本《蛇结》写了 `margin: 5px auto`：旧规则把整条声明判为不认识，
/// 于是连那 5px 的上下边距一起丢掉 —— 静默丢一半比折成 0 糟得多。折成 0 也不是降级：
/// CSS 的 `auto` 前后边距是均分剩余宽度，而我们的正文块使用宽度就等于可用宽度
/// （块级不吃 `max-width` 收窄），剩余宽度本来就是 0。等排版层支持块级 `max-width`
/// 时这里要改成均分，那时才会出现块级居中。
#[test]
fn auto_margin_computes_to_zero() {
    let page = author("<p>文</p>", "p { margin-left: auto }");
    let p = style(&page, tag(&page, "p"));
    assert_eq!(p.margin_left, 0.0);
    assert_eq!(p.margin_top, 16.0, "UA 给 <p> 的 1em 不该被这条声明碰坏");
    assert_eq!(page.styled.ignored, 0, "认得的合法值不该计进 ignored");
}

/// 四边简写是这本书里最大的一处落差：917 个元素身上有胜出的 `margin` 简写，
/// 而旧实现只认单值形式（`margin: 1em`），`margin: 1em 0` 这种最常见的写法整条作废。
#[test]
fn the_margin_shorthand_expands_one_to_four_values() {
    // em 按元素自己的字号折算，12pt 按 CSS 的 96/72 比例 —— 两种单位都出现过。
    let page = author(
        "<div class=\"a\"></div><div class=\"b\"></div>\
         <div class=\"c\"></div><div class=\"d\"></div>",
        ".a { margin: 12pt 0 } \
         .b { margin: 5px 0 1em } \
         .c { margin: 1px 2px 3px 4px } \
         .d { margin: 0.5em 0 } \
         .d { font-size: 30px }",
    );
    let a = style(&page, class(&page, "a"));
    assert_eq!(
        (a.margin_top, a.margin_right, a.margin_bottom, a.margin_left),
        (16.0, 0.0, 16.0, 0.0),
        "两值 = 「上下 左右」"
    );
    let b = style(&page, class(&page, "b"));
    assert_eq!(
        (b.margin_top, b.margin_right, b.margin_bottom, b.margin_left),
        (5.0, 0.0, 16.0, 0.0),
        "三值 = 「上 左右 下」"
    );
    let c = style(&page, class(&page, "c"));
    assert_eq!(
        (c.margin_top, c.margin_right, c.margin_bottom, c.margin_left),
        (1.0, 2.0, 3.0, 4.0),
        "四值顺时针"
    );
    let d = style(&page, class(&page, "d"));
    assert_eq!(
        d.margin_top, 15.0,
        "0.5em 按它自己的 30px 字号，不是根的 16px"
    );
    assert_eq!(page.styled.ignored, 0, "这四种写法都该被接住");
}

/// `padding` 简写走同一份展开，但**不**吃 `auto`：CSS 里 `padding: auto` 是非法值。
#[test]
fn the_padding_shorthand_expands_without_auto() {
    let page = author(
        "<div class=\"a\"></div><div class=\"b\"></div>",
        ".a { padding: 0 1em } .b { padding: 4px auto }",
    );
    let a = style(&page, class(&page, "a"));
    assert_eq!(
        (
            a.padding_top,
            a.padding_right,
            a.padding_bottom,
            a.padding_left
        ),
        (0.0, 16.0, 0.0, 16.0)
    );
    let b = style(&page, class(&page, "b"));
    assert_eq!(
        (b.padding_top, b.padding_right),
        (0.0, 0.0),
        "`padding: auto` 非法，整条作废而不是半信半疑"
    );
    assert_eq!(page.styled.ignored, 1, "只有那一条该计 ignored");
}

/// 值数超出 1–4、或任一个值不认识，整条作废。静默只取前两个值是最难查的错法。
#[test]
fn a_shorthand_with_too_many_or_bad_values_is_dropped_whole() {
    let page = author(
        "<div class=\"a\"></div><div class=\"b\"></div><div class=\"c\"></div>",
        ".a { margin: 1px 2px 3px 4px 5px } \
         .b { margin: 1px nonsense } \
         .c { margin: 5% }",
    );
    for name in ["a", "b", "c"] {
        let s = style(&page, class(&page, name));
        assert_eq!(
            (s.margin_top, s.margin_left),
            (0.0, 0.0),
            ".{name} 的写法都该整条不生效"
        );
    }
    assert_eq!(
        page.styled.ignored, 3,
        "三条都要计进 ignored，落差不然会被静默吞掉"
    );
}

#[test]
fn bad_values_fall_back_to_the_inherited_or_default_number() {
    let page = author("<p>文</p>", "p { font-size: huge; line-height: tall }");
    let p = style(&page, tag(&page, "p"));
    assert_eq!(p.font_size, 16.0);
    assert_eq!(p.line_height, 1.75);
    assert_eq!(page.styled.ignored, 2);
}

#[test]
fn white_space_pre_is_tracked() {
    let page = author("<pre>文</pre>", "pre { white-space: pre }");
    assert!(style(&page, tag(&page, "pre")).white_space_pre);
    assert_eq!(page.styled.ignored, 0);
}

#[test]
fn font_family_takes_the_first_entry_and_strips_quotes() {
    let page = author("<p>文</p>", "p { font-family: \"LXGW WenKai\", serif }");
    assert_eq!(style(&page, tag(&page, "p")).font_family, "LXGW WenKai");
}

/// 端到端：读者的 `!important` 压过作者的 `!important`。
#[test]
fn reader_important_wins_through_the_whole_pipeline() {
    let page = page(
        "<p>文</p>",
        &[
            ("p { font-size: 30px !important }", Origin::Author),
            ("p { font-size: 18px !important }", Origin::Reader),
        ],
    );
    assert_eq!(style(&page, tag(&page, "p")).font_size, 18.0);
}

#[test]
fn author_beats_reader_without_important() {
    let page = page(
        "<p>文</p>",
        &[
            ("p { font-size: 30px }", Origin::Author),
            ("p { font-size: 18px }", Origin::Reader),
        ],
    );
    assert_eq!(style(&page, tag(&page, "p")).font_size, 30.0);
}

/// 多张表合并后，后出现的那张在同 specificity 时胜出。
#[test]
fn later_stylesheet_wins_after_extend() {
    let page = page(
        "<p>文</p>",
        &[
            ("p { text-indent: 10px }", Origin::Author),
            ("p { text-indent: 99px }", Origin::Author),
        ],
    );
    assert_eq!(style(&page, tag(&page, "p")).text_indent, 99.0);
}

#[test]
fn root_base_from_reader_prefs_is_the_inheritance_seed() {
    let dom = Dom::parse("<div><p>文</p></div>".as_bytes());
    let rules = RuleSet::parse("p { margin-top: 1em }", Origin::Author);
    let base = ComputedStyle {
        font_size: 24.0,
        line_height: 2.0,
        font_family: "Noto Serif CJK SC".into(),
        ..ComputedStyle::default()
    };
    let styled = cascade(&dom, &rules, &base);
    let p = dom
        .elements()
        .find(|(_, n)| {
            n.tag
                .as_ref()
                .map(|t| t.local.to_string())
                .unwrap_or_default()
                == "p"
        })
        .map(|(id, _)| id)
        .unwrap();
    let s = styled.get(p).unwrap();
    assert_eq!(s.font_size, 24.0, "读者字号要能穿到最深层");
    assert_eq!(s.line_height, 2.0);
    assert_eq!(s.font_family, "Noto Serif CJK SC");
    assert_eq!(s.margin_top, 24.0, "1em 按偏好字号而不是 16px");
}

/// 图片尺寸用的长度：px / em / rem 折成 px，% 留到排版期，auto 清空。
#[test]
fn width_and_max_width_parse_units_percent_and_auto() {
    let page = author(
        "<div class=\"a\"></div><div class=\"b\"></div>\
         <div class=\"c\"></div><div class=\"d\"></div>",
        ".a { width: 120px } \
         .b { width: 100% } \
         .c { width: auto; max-width: 50% } \
         .d { width: 2em; font-size: 20px; max-height: 3rem }",
    );
    assert_eq!(
        style(&page, class(&page, "a")).width,
        Some(Length::Px(120.0))
    );
    assert_eq!(
        style(&page, class(&page, "b")).width,
        Some(Length::Percent(1.0))
    );
    let c = style(&page, class(&page, "c"));
    assert_eq!(c.width, None, "auto 清空显式宽度");
    assert_eq!(c.max_width, Some(Length::Percent(0.5)));
    let d = style(&page, class(&page, "d"));
    assert_eq!(d.width, Some(Length::Px(40.0)), "2em 按自己的 20px 字号");
    assert_eq!(d.max_height, Some(Length::Px(48.0)), "3rem 按根 16px 字号");
    assert_eq!(page.styled.ignored, 0);

    assert_eq!(Length::Px(30.0).resolve(200.0), 30.0);
    assert_eq!(Length::Percent(0.5).resolve(200.0), 100.0);
}

/// `width` 不再是「认识但不做」：能算的声明不该被计进 ignored。
#[test]
fn implemented_width_is_not_counted_as_ignored() {
    let page = author("<div></div>", "div { width: 100px; max-width: 100% }");
    assert_eq!(page.styled.ignored, 0);
    assert_eq!(
        style(&page, tag(&page, "div")).width,
        Some(Length::Px(100.0))
    );
}

/// `vertical-align` 折成基线偏移。这本《蛇结》的注号用的就是 `super`（51 个元素），
/// 而旧实现把它整个丢掉 —— 上标写成正文位置，读者看不出那是个脚注引用。
#[test]
fn vertical_align_becomes_a_baseline_shift() {
    let page = author(
        "<div class=\"a\"></div><div class=\"b\"></div>\
         <div class=\"c\"></div><div class=\"d\"></div>",
        ".a { vertical-align: super } \
         .b { vertical-align: sub } \
         .c { vertical-align: 4px } \
         .d { vertical-align: baseline }",
    );
    let a = style(&page, class(&page, "a"));
    assert!(
        (a.baseline_shift - 16.0 / 3.0).abs() < 0.01,
        "super 抬三分之一 em（自己的 16px 字号），实际 {}",
        a.baseline_shift
    );
    assert_eq!(
        style(&page, class(&page, "b")).baseline_shift,
        -4.0,
        "sub 压 0.25em"
    );
    assert_eq!(style(&page, class(&page, "c")).baseline_shift, 4.0);
    assert_eq!(
        style(&page, class(&page, "d")).baseline_shift,
        0.0,
        "`baseline` 就是初值，不该计进 ignored"
    );
    assert_eq!(page.styled.ignored, 0);
}

/// 百分比相对**自己的 line-height**（CSS 的规定），长度可以是 em；
/// `top` / `middle` 这些要行盒对齐机制的不接，也不假装接住。
#[test]
fn vertical_align_percent_is_against_own_line_height_and_unkowns_are_counted() {
    let page = author(
        "<div class=\"a\"></div><div class=\"b\"></div>",
        ".a { vertical-align: 50%; font-size: 20px; line-height: 2 } \
         .b { vertical-align: middle }",
    );
    assert_eq!(
        style(&page, class(&page, "a")).baseline_shift,
        20.0,
        "50% × (20px × 2)"
    );
    assert_eq!(
        style(&page, class(&page, "b")).baseline_shift,
        0.0,
        "`middle` 要的是行盒对齐，一期不折成偏移"
    );
    assert_eq!(
        page.styled.ignored, 1,
        "不认的取值必须计 ignored，否则落差被静默吞掉"
    );
}

/// `text-decoration` 的简写与 `text-decoration-line` 长写法：同一件事的两个入口。
#[test]
fn text_decorations_are_recorded_per_line_kind() {
    let page = author(
        "<div class=\"a\"></div><div class=\"b\"></div>\
         <div class=\"c\"></div><div class=\"d\"></div>",
        ".a { text-decoration: underline } \
         .b { text-decoration: underline line-through } \
         .c { text-decoration-line: line-through } \
         .d { text-decoration: none }",
    );
    let a = style(&page, class(&page, "a"));
    assert!(a.underline && !a.line_through);
    let b = style(&page, class(&page, "b"));
    assert!(b.underline && b.line_through, "多值简写要两条都算");
    let c = style(&page, class(&page, "c"));
    assert!(!c.underline && c.line_through);
    let d = style(&page, class(&page, "d"));
    assert!(!d.underline && !d.line_through);
    assert_eq!(page.styled.ignored, 0);
}

/// 线型与颜色 token 认得但不改画法；`overline` 画不出来就整条不接。
#[test]
fn decoration_line_style_and_color_tokens_are_recognised_but_overline_is_not() {
    let page = author(
        "<div class=\"a\"></div><div class=\"b\"></div>",
        ".a { text-decoration: underline wavy #f00 } .b { text-decoration: overline }",
    );
    let a = style(&page, class(&page, "a"));
    assert!(a.underline, "线型与颜色不改变「有没有下划线」这件事");
    assert!(
        !style(&page, class(&page, "b")).underline,
        "overline 画不出来，不该显示成接住了"
    );
    assert_eq!(page.styled.ignored, 1, "只有 overline 那条计 ignored");
}

/// 装饰线在 CSS 里不是继承属性，但会向后代**传播**；词距则老老实实是可继承的。
#[test]
fn decoration_propagates_and_word_spacing_inherits() {
    let page = author(
        "<div class=outer><p><span class=inner>文</span></p></div>",
        ".outer { text-decoration: underline; word-spacing: 6px }",
    );
    let inner = style(&page, class(&page, "inner"));
    assert!(inner.underline, "父元素的下划线要盖到子元素的文字上");
    assert_eq!(inner.word_spacing, 6.0, "`word-spacing` 在 CSS 里可继承");
    // `vertical-align` **不**继承：上标只抬自己那一小段，不能让整段正文上浮。
    let page = author(
        "<div class=outer><p>文</p></div>",
        ".outer { vertical-align: super }",
    );
    let p = style(&page, tag(&page, "p"));
    assert_eq!(p.baseline_shift, 0.0, "`vertical-align` 不该跟着继承下来");
}

#[test]
fn word_spacing_normal_is_zero_and_units_resolve() {
    let page = author(
        "<div class=\"a\"></div><div class=\"b\"></div><div class=\"c\"></div>",
        ".a { word-spacing: normal } \
         .b { word-spacing: 12pt } \
         .c { word-spacing: 0.5em; font-size: 20px }",
    );
    assert_eq!(style(&page, class(&page, "a")).word_spacing, 0.0);
    assert_eq!(
        style(&page, class(&page, "b")).word_spacing,
        16.0,
        "12pt 按 96/72"
    );
    assert_eq!(
        style(&page, class(&page, "c")).word_spacing,
        10.0,
        "0.5em 按自己的 20px"
    );
    assert_eq!(page.styled.ignored, 0);
}

/// `font` 简写：老 EPUB 满地都是。**有意不重置没写到的子属性** —— 读者偏好走的是
/// `root_base` 的继承链，一句 `font: 1em serif` 把人家的行距改回 normal 是产品级的错。
#[test]
fn the_font_shorthand_sets_what_it_names_and_resets_nothing() {
    let page = author(
        "<div class=\"a\"></div><div class=\"b\"></div>",
        ".a { font: italic bold 12pt/1.5 \"Noto Serif CJK SC\", serif } \
         .b { font-size: 22px; line-height: 2; font: 1em serif }",
    );
    let a = style(&page, class(&page, "a"));
    assert!(a.font_italic);
    assert_eq!(a.font_weight, 700, "bold");
    assert_eq!(a.font_size, 16.0, "12pt");
    assert_eq!(a.line_height, 1.5, "斜杠后是行距");
    assert_eq!(a.font_family, "Noto Serif CJK SC", "带空格的家族名要整个取");
    let b = style(&page, class(&page, "b"));
    assert_eq!(b.font_size, 16.0, "1em 按它自己的 22px？不，em 按父字号");
    assert_eq!(
        b.line_height, 2.0,
        "简写没写行距就不动它：读者设的行距不该被一句 `font` 抹掉"
    );
    assert_eq!(b.font_family, "serif");
    assert_eq!(page.styled.ignored, 0);
}

/// 认不出的 token（`small-caps`、系统字体关键字）整条作废，不做半套。
#[test]
fn a_font_shorthand_with_unsupported_tokens_is_dropped_whole() {
    let page = author(
        "<div class=\"a\"></div><div class=\"b\"></div>",
        ".a { font: small-caps 12pt serif } .b { font: caption }",
    );
    for name in ["a", "b"] {
        let s = style(&page, class(&page, name));
        assert_eq!(s.font_family, "", ".{name} 整条都不该生效");
        assert_eq!(s.font_size, 16.0);
    }
    assert_eq!(page.styled.ignored, 2);
}

/// `declaration_status` 是 `examples/css-gap.rs` 的唯一判定口，所以它必须和层叠一致：
/// 三种状态各钉一条，错一个统计就会把「没接」说成「接住」。
#[test]
fn declaration_status_agrees_with_the_cascade() {
    use folio_core::css::Declaration;
    use folio_core::css::computed::{DeclStatus, declaration_status};

    let base = ComputedStyle::default();
    let decl = |property: &str, value: &str| Declaration {
        property: property.into(),
        value: value.into(),
        important: false,
    };
    assert_eq!(
        declaration_status(&decl("font-size", "20px"), &base, 16.0),
        DeclStatus::Applied
    );
    assert_eq!(
        declaration_status(&decl("text-transform", "uppercase"), &base, 16.0),
        DeclStatus::Inert,
        "名单里的属性该报 Inert，而不是不认识"
    );
    assert_eq!(
        declaration_status(&decl("font-size", "huge"), &base, 16.0),
        DeclStatus::Unparsed
    );
    assert_eq!(
        declaration_status(&decl("margin", "1em auto"), &base, 16.0),
        DeclStatus::Applied,
        "`auto` 折成 0 之后这条就是接住的，不该再出现在落差表里"
    );
    assert_eq!(
        declaration_status(&decl("color", "inherit"), &base, 16.0),
        DeclStatus::Applied,
        "`inherit` 是合法值，继承由 inherit_from 负责"
    );
    assert_eq!(
        declaration_status(&decl("color", ""), &base, 16.0),
        DeclStatus::Unparsed
    );
}

/// UA 表那一份最小副本负责「作者没写、浏览器会给」的默认值。上标与装饰线都在这里，
/// 因为出版书的 XHTML 通常只写 `<sup>`、`<u>`、`<del>` 而不带 class。
#[test]
fn the_ua_sheet_carries_superscripts_and_decorations() {
    let page = author(
        "<p><sup>a</sup><sub>b</sub><u>c</u><del>d</del><s>e</s></p>",
        "",
    );
    let sup = style(&page, tag(&page, "sup"));
    assert_eq!(sup.font_size, 16.0 / 1.5);
    assert!(
        (sup.baseline_shift - 16.0 / 1.5 / 3.0).abs() < 0.01,
        "位移按缩过之后的自己字号折，实际 {}",
        sup.baseline_shift
    );
    let sub = style(&page, tag(&page, "sub"));
    assert!(
        (sub.baseline_shift + 16.0 / 1.5 * 0.25).abs() < 0.01,
        "下标是负偏移（往下），实际 {}",
        sub.baseline_shift
    );
    assert!(style(&page, tag(&page, "u")).underline, "<u> 天生有下划线");
    assert!(style(&page, tag(&page, "del")).line_through);
    assert!(style(&page, tag(&page, "s")).line_through);
    assert!(!sup.underline, "上标不该顺手带上下划线");
    assert_eq!(
        page.styled.ignored, 0,
        "UA 值不来自声明，本就不该计 ignored"
    );
}

/// 同一个式子：作者写 `vertical-align: super` 与 UA 给 `<sup>` 的偏移，
/// 在**同样字号**下必须相等 —— 否则带 class 的注号和裸 `<sup>` 会排成两种高度。
#[test]
fn ua_superscript_and_the_author_declaration_agree() {
    let page = author(
        "<p><sup>a</sup><span class=v>b</span></p>",
        ".v { vertical-align: super; font-size: 10.666666px }",
    );
    let sup = style(&page, tag(&page, "sup"));
    let v = style(&page, class(&page, "v"));
    assert!(
        (sup.font_size - v.font_size).abs() < 0.01,
        "先确认两处字号对齐：{} vs {}",
        sup.font_size,
        v.font_size
    );
    assert!(
        (sup.baseline_shift - v.baseline_shift).abs() < 0.01,
        "同一字号下 UA 的 `<sup>` 与作者的 `vertical-align: super` 该给同一个偏移，\
         实际 {} vs {}",
        sup.baseline_shift,
        v.baseline_shift
    );
}
