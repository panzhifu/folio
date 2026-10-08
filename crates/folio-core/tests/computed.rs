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

#[test]
fn auto_margin_is_rejected() {
    let page = author("<p>文</p>", "p { margin-left: auto }");
    assert_eq!(style(&page, tag(&page, "p")).margin_left, 0.0);
    assert_eq!(page.styled.ignored, 1);
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
