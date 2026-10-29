//! `ComputedStyle` 计算：单位换算、继承、ignored 计数、层叠端到端。
//!
//! 只依赖公开 API。选择器命中和 specificity / `!important` 的排序在 `css::tests` 已覆盖，
//! 这里验的是「拿到有序声明之后算得对不对」。

use crate::content::dom::{Dom, NodeId};
use crate::content::format::css::computed::{
    BackgroundAxis, BackgroundEdge, BackgroundPosition, BackgroundSize, BackgroundTile, BorderEdge,
    BorderStyle, BoxSizing, BreakWhen, Clear, Color, ComputedStyle, DeclStatus, Direction, Display,
    Float, Hyphens, Length, ListStyle, StyledDom, TextAlign, TextJustify, WordBreak, WritingMode,
    cascade, declaration_status,
};
use crate::content::format::css::{Declaration, Origin, RuleSet};

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
    let styled = cascade(&dom, &rules, &ComputedStyle::default(), "");
    Page { dom, styled }
}

/// 与 [`page`] 同一件事，只是这一份文档在容器里的**目录**给得出来。
///
/// 只有内联 `style="…"` 与章内 `<style>` 会读它：那两处的 `url(...)` 基准是**这一份文档
/// 自己的目录**，而外链样式表按它自己的目录折（在拼接前由 `absolutize_css_urls` 做过）。
/// 绝大多数用例不涉及路径，走 [`page`] 那个空目录就够。
fn page_in_dir(html: &str, css: &str, base_dir: &str) -> Page {
    let dom = Dom::parse(html.as_bytes());
    let mut rules = RuleSet::default();
    rules.extend(RuleSet::parse(css, Origin::Author));
    let styled = cascade(&dom, &rules, &ComputedStyle::default(), base_dir);
    Page { dom, styled }
}

fn author(html: &str, css: &str) -> Page {
    page(html, &[(css, Origin::Author)])
}

fn style(page: &Page, id: NodeId) -> &ComputedStyle {
    page.styled.get(id).expect("样式表应覆盖每个节点")
}

/// 断言按 px 读那一条边距 / 缩进。折出 `Percent` 会被这条直接判失败 —— 层叠期
/// 不该折算百分比（那是排版期拿到包含块宽之后的事），所以这些用例里出现
/// `Percent` 意味着取值层走错了路。要验百分比本身就用 `Length::Percent(..)` 直接对。
/// `Auto` 同理：那是「等排版期均分」的记号，要断言它就写 `Length::Auto`。
fn px(l: Length) -> f32 {
    match l {
        Length::Px(v) => v,
        Length::Percent(p) => panic!("层叠期把百分比折掉了（{p}），折算点在排版期"),
        Length::Auto => {
            panic!("这一条是 `auto`，均分在排版期（layout::collect::solve_block_width）")
        }
    }
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
    assert_eq!(px(style(&first, tag(&first, "p")).text_indent), 40.0);
    assert_eq!(
        px(style(&second, tag(&second, "p")).text_indent),
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
    assert_eq!(px(style(&page, tag(&page, "p")).margin_top), 32.0);
}

#[test]
fn pt_converts_at_the_css_ratio() {
    let page = author("<p>字</p>", "p { font-size: 12pt }");
    assert_eq!(style(&page, tag(&page, "p")).font_size, 16.0);
}

#[test]
fn bare_number_is_treated_as_px() {
    let page = author("<p>字</p>", "p { text-indent: 3 }");
    assert_eq!(px(style(&page, tag(&page, "p")).text_indent), 3.0);
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
         word-spacing: 2px; margin-bottom: 12px; padding-top: 8px; display: inline }",
    );
    let p = style(&page, tag(&page, "p"));
    assert_eq!(p.color, Some(Color(255, 0, 0)));
    assert_eq!(p.text_align, TextAlign::Center);
    assert_eq!(p.font_weight, 700);
    assert!(p.font_italic);
    assert_eq!(p.word_spacing, 2.0);
    assert_eq!(
        px(p.margin_bottom),
        16.0,
        "p 的 1em 下边距来自 UA 表，不是继承父元素的 12px"
    );
    assert_eq!(px(p.padding_top), 0.0, "padding 不该继承");
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
    assert_eq!(px(p.text_indent), 32.0, "缩进继承下来但已在根元素折算成 px");
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
/// 百分比边距**存下来，不在层叠期折**。
///
/// 这一条以前是「宁可判不支持」：`margin-top: 10%` 整条丢掉、计进 ignored。改掉的理由
/// 是账 —— 多看转换的《哈利·波特》有 86441 个元素挂着 `margin: 0% 1% 0% 1%`，全书
/// 92698 条「不认识」里绝大多数就是这一族写法。判不支持等于把作者的边距静默抹掉，
/// 而基准（包含块宽）只是**当时**不知道，排版期拿得到。
/// 折算点在哪：`layout::collect` 的 `basis`，见 `the_percent_margin_folds_against_the_containing_block`。
#[test]
fn percentage_boxes_are_kept_for_the_layout_phase() {
    let page = author("<div>文</div>", "div { margin-top: 10%; text-indent: 5% }");
    let div = style(&page, tag(&page, "div"));
    assert_eq!(div.margin_top, Length::Percent(0.1));
    assert_eq!(div.text_indent, Length::Percent(0.05));
    assert_eq!(
        page.styled.ignored, 0,
        "合法的百分比不该再计进 ignored —— 那一格是留给真落差的"
    );
}

/// `auto` 边距**原样留在计算样式里**，既不折成 0，也不计进 `ignored`。
///
/// 这一条改过两次。第一版把 `margin: 5px auto` 整条判为不认识，于是连那 5px 的上下
/// 边距一起丢掉（《蛇结》就是这么丢的）；第二版折成 `Px(0)`，因为当时块级不吃
/// `width` / `max-width` 的收窄，剩余宽度恒为 0，取 0 恰好等于 CSS 的均分结果。
/// 现在排版层真按作者的 `width` 收窄盒子了（`layout::collect::solve_block_width`），
/// 「均分多少」取决于那块折出来多宽 —— 那是排版期才知道的数，层叠期再猜一次就是
/// 把居中猜没了。所以这一格只负责「认得、留下」，具体几个像素归排版层，
/// 那一侧的断言在 `layout::tests`。
#[test]
fn auto_margin_survives_the_cascade() {
    let page = author("<p>文</p>", "p { margin-left: auto }");
    let p = style(&page, tag(&page, "p"));
    assert_eq!(p.margin_left, Length::Auto);
    assert_eq!(
        px(p.margin_top),
        16.0,
        "UA 给 <p> 的 1em 不该被这条声明碰坏"
    );
    assert_eq!(page.styled.ignored, 0, "认得的合法值不该计进 ignored");
}

/// `auto` 只许出现在 `margin` 这一族：CSS 里 `padding: auto` 与 `text-indent: auto`
/// 都是非法值。认下来就等于把一条写坏的声明显示成「已处理」。
#[test]
fn auto_is_not_a_padding_or_indent_value() {
    let page = author(
        "<p>文</p>",
        "p { padding: 4px auto; text-indent: auto; max-width: auto }",
    );
    let p = style(&page, tag(&page, "p"));
    assert_eq!(px(p.padding_top), 0.0, "整条作废，不该留下半个 4px");
    assert_eq!(px(p.text_indent), 0.0);
    assert_eq!(p.max_width, None, "`max-width: auto` 与「没写过」同解");
    assert_eq!(
        page.styled.ignored, 2,
        "`padding: auto` 与 `text-indent: auto` 各计一条不认识"
    );
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
        (
            px(a.margin_top),
            px(a.margin_right),
            px(a.margin_bottom),
            px(a.margin_left)
        ),
        (16.0, 0.0, 16.0, 0.0),
        "两值 = 「上下 左右」"
    );
    let b = style(&page, class(&page, "b"));
    assert_eq!(
        (
            px(b.margin_top),
            px(b.margin_right),
            px(b.margin_bottom),
            px(b.margin_left)
        ),
        (5.0, 0.0, 16.0, 0.0),
        "三值 = 「上 左右 下」"
    );
    let c = style(&page, class(&page, "c"));
    assert_eq!(
        (
            px(c.margin_top),
            px(c.margin_right),
            px(c.margin_bottom),
            px(c.margin_left)
        ),
        (1.0, 2.0, 3.0, 4.0),
        "四值顺时针"
    );
    let d = style(&page, class(&page, "d"));
    assert_eq!(
        px(d.margin_top),
        15.0,
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
            px(a.padding_top),
            px(a.padding_right),
            px(a.padding_bottom),
            px(a.padding_left)
        ),
        (0.0, 16.0, 0.0, 16.0)
    );
    let b = style(&page, class(&page, "b"));
    assert_eq!(
        (px(b.padding_top), px(b.padding_right)),
        (0.0, 0.0),
        "`padding: auto` 非法，整条作废而不是半信半疑"
    );
    assert_eq!(page.styled.ignored, 1, "只有那一条该计 ignored");
}

/// `float` / `clear`：认得的写法接住并生效，认不出的整条计进 `ignored`。
#[test]
fn float_and_clear_count_only_unrecognized_values_as_gap() {
    let page = author("<p></p>", "p { float: left; clear: nope }");
    assert_eq!(
        style(&page, tag(&page, "p")).float,
        Float::Left,
        "`float: left` 是真生效的属性了，不再是「认得不生效」"
    );
    assert_eq!(page.styled.ignored, 1, "只有 `clear: nope` 那一条算落差");
}

/// `list-style-type` 收 EPUB 3 CSS Profile 点名的记数；表外的关键字整条判不认识。
///
/// `hebrew` 是 profile 收录而我们**故意**没接的那一个：它那套数值记数本机没有一本
/// 希伯来书可验，画出一个看着像的号比承认没做更难发现。所以它进 `ignored`。
#[test]
fn list_style_types_parse_and_unknown_ones_are_counted() {
    let page = author(
        "<p class=a></p><p class=b></p>",
        ".a { list-style-type: cjk-ideographic } .b { list-style-type: hebrew }",
    );
    assert_eq!(
        style(&page, class(&page, "a")).list_style_type,
        ListStyle::CjkIdeographic
    );
    assert_eq!(
        style(&page, class(&page, "b")).list_style_type,
        ListStyle::Disc,
        "认不出的类型回到初值，不降级成别的记数"
    );
    assert_eq!(page.styled.ignored, 1, "只有 `hebrew` 那一条该计落差");
}

/// `list-style` 简写取**类型**那一项；`inside` 与 `url(…)` 听得出、这一轮不生效。
/// 与 `background` 简写同一个口径：接住能接的那一项，不假装接住了整条。
#[test]
fn the_list_style_shorthand_takes_the_type_token() {
    let page = author(
        "<p class=a></p><p class=b></p>",
        ".a { list-style: square inside } .b { list-style: url(dot.png) }",
    );
    assert_eq!(
        style(&page, class(&page, "a")).list_style_type,
        ListStyle::Square,
        "简写里的 `square` 要落进类型"
    );
    assert_eq!(
        page.styled.ignored, 0,
        "两条都是认得的写法：一项生效、其余听得出"
    );
}

/// 值数超出 1–4、或任一个值不认识，整条作废。静默只取前两个值是最难查的错法。
///
/// 第三条用的是 `calc()`：百分比已经不再是「不认识」的那一类了（见
/// [`percentage_boxes_are_kept_for_the_layout_phase`]），所以这一格要换一个真折不出的写法。
#[test]
fn a_shorthand_with_too_many_or_bad_values_is_dropped_whole() {
    let page = author(
        "<div class=\"a\"></div><div class=\"b\"></div><div class=\"c\"></div>",
        ".a { margin: 1px 2px 3px 4px 5px } \
         .b { margin: 1px nonsense } \
         .c { margin: calc(1px + 2px) }",
    );
    for name in ["a", "b", "c"] {
        let s = style(&page, class(&page, name));
        assert_eq!(
            (px(s.margin_top), px(s.margin_left)),
            (0.0, 0.0),
            ".{name} 的写法都该整条不生效"
        );
    }
    assert_eq!(
        page.styled.ignored, 3,
        "三条都要计进 ignored，落差不然会被静默吞掉"
    );
}

/// `orphans` / `widows` 的 `0` 夹到 1，不再计进 ignored；负数仍判不认识。
///
/// 这一条的账在《斗战狂潮》：`body` 与 `p` 上各写一次，全书 138336 条声明就是它。
/// 判不认识等于把那本书推回引擎默认的 2 / 2，症状是页底比作者要的少留一行。
#[test]
fn a_zero_orphan_or_widow_count_clamps_to_one() {
    let page = author(
        "<div><p>文</p></div>",
        "p { orphans: 0; widows: 0 } div { orphans: -2; widows: 3 }",
    );
    let p = style(&page, tag(&page, "p"));
    assert_eq!(
        (p.orphans, p.widows),
        (1, 1),
        "0 的意思是「不设约束」，折到合法域的下沿"
    );
    assert_eq!(page.styled.ignored, 1, "只有那条负数该计 ignored");
    let div = style(&page, tag(&page, "div"));
    assert_eq!(
        (div.orphans, div.widows),
        (2, 3),
        "负数整条不生效，回到初值 2；同一条声明里的 widows 不受牵连"
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

/// `hyphens` 三个值 + EPUB 前缀拼法 + 可继承。认不出的值回 CSS 初值 `manual`
/// 并计一条 ignored —— 静悄悄变成 `none` 会把整本书的折行改掉还没人知道。
#[test]
fn hyphens_parses_the_three_values_and_inherits() {
    let page = author(
        "<div><p class=a>一</p></div><p class=b>二</p><p class=c>三</p>",
        "div { hyphens: none } .b { -epub-hyphens: auto } .c { hyphens: tall }",
    );
    assert_eq!(
        style(&page, class(&page, "a")).hyphens,
        Hyphens::None,
        "子元素继承到 div 的 none"
    );
    assert_eq!(
        style(&page, class(&page, "b")).hyphens,
        Hyphens::Auto,
        "`-epub-hyphens` 这种拼法也认"
    );
    assert_eq!(
        style(&page, class(&page, "c")).hyphens,
        Hyphens::Manual,
        "认不出的值回到 CSS 初值"
    );
    assert_eq!(page.styled.ignored, 1, "`hyphens: tall` 要留一条痕迹");
}

/// 元素的 `style="…"` 是作者写的、压在作者样式表之上那一档（CSS Cascade §6.4：
/// 作者普通 < 行内普通 < 行内 `!important` < 作者 `!important`）。
/// 追加到最后是常见错法 —— 那会让行内的普通声明盖掉书里的 `color: blue !important`。
#[test]
fn inline_style_beats_author_normal_but_not_important() {
    let plain = author(r#"<p style="color: red">文</p>"#, "p { color: blue }");
    assert_eq!(
        style(&plain, tag(&plain, "p")).color,
        Some(Color(255, 0, 0)),
        "行内的普通声明该压过样式表的普通声明"
    );

    let important = author(
        r#"<p style="color: red">文</p>"#,
        "p { color: blue !important }",
    );
    assert_eq!(
        style(&important, tag(&important, "p")).color,
        Some(Color(0, 0, 255)),
        "行内的普通声明不该盖掉作者的 !important"
    );

    let both = author(
        r#"<p style="color: red !important">文</p>"#,
        "p { color: blue !important }",
    );
    assert_eq!(
        style(&both, tag(&both, "p")).color,
        Some(Color(0, 0, 255)),
        "两边都 !important 时仍是样式表那份赢"
    );
}

/// 读者的普通声明（偏好折算出来的那一份）在作者行内样式之下，
/// 但读者的 `!important` 仍在所有作者声明之上。
#[test]
fn inline_style_sits_between_reader_normal_and_reader_important() {
    let over_reader = page(
        r#"<p style="font-size: 30px">文</p>"#,
        &[("p { font-size: 12px }", Origin::Reader)],
    );
    assert_eq!(style(&over_reader, tag(&over_reader, "p")).font_size, 30.0);

    let under_reader = page(
        r#"<p style="font-size: 30px">文</p>"#,
        &[("p { font-size: 12px !important }", Origin::Reader)],
    );
    assert_eq!(
        style(&under_reader, tag(&under_reader, "p")).font_size,
        12.0,
        "读者的 !important 是最高一档，行内也拦不住"
    );
}

/// 行内那份同样讲「font-size 先算」：`text-indent: 2em` 的 em 要按这条声明
/// 自己的 20px 折，不是按继承来的 16px。
#[test]
fn inline_em_resolves_against_the_inline_font_size() {
    let page = author(r#"<p style="text-indent: 2em; font-size: 20px">文</p>"#, "");
    assert_eq!(px(style(&page, tag(&page, "p")).text_indent), 40.0);
}

/// 行内样式不进任何样式表，所以 `ignored` 是它唯一的痕迹 ——
/// 没有这个计数，「这本书写了我们不支持的东西」就彻底没人说了。
#[test]
fn unsupported_inline_declarations_are_counted_as_ignored() {
    let page = author(
        r#"<p style="transform: rotate(3deg); color: green">文</p>"#,
        "",
    );
    assert_eq!(style(&page, tag(&page, "p")).color, Some(Color(0, 128, 0)));
    assert_eq!(page.styled.ignored, 1, "transform 那条要留痕");
}

/// 引号里的分号不是声明分隔符：切错了会把 `color: green` 削成半条，
/// 而半条与整条在 ignored 上看不出区别，所以断言落在颜色上。
#[test]
fn semicolon_inside_quotes_is_not_a_declaration_separator() {
    let page = author(r#"<p style='content: "a;b"; color: green'>文</p>"#, "");
    assert_eq!(
        style(&page, tag(&page, "p")).color,
        Some(Color(0, 128, 0)),
        "引号里的分号不该把 color 切开"
    );
    assert_eq!(page.styled.ignored, 1, "只有 content 那一条是不认识的");
}

/// `p::first-letter` 的声明只进首字符那一份，元素自己一点不受影响。
/// 两边混在一起是最典型的错法：`p::first-letter { color: red }` 会把整段涂红。
#[test]
fn first_letter_declarations_land_on_the_cap_not_the_element() {
    let page = author(
        "<p>文</p><h1>题</h1>",
        "p { font-size: 20px } p::first-letter { font-size: 3em; color: red } h1 { color: blue }",
    );
    let p = style(&page, tag(&page, "p"));
    assert_eq!(p.font_size, 20.0, "元素自己的字号不该被首字符那条改掉");
    assert_eq!(p.color, None, "元素自己不该拿到首字符的颜色");

    let cap = p.first_letter.as_deref().expect("应有首字符那一份");
    assert_eq!(cap.font_size, 60.0, "3em 按元素自己的 20px 折");
    assert_eq!(cap.color, Some(Color(255, 0, 0)));
    assert_eq!(cap.font_weight, 400, "没写就沿用元素的值");

    // 没写 `::first-letter` 的元素必须是 `None` —— 排版层靠这一个判走不走下沉。
    assert!(style(&page, tag(&page, "h1")).first_letter.is_none());
    assert_eq!(page.styled.ignored, 0);
}

/// 首字符那份**不继承**给后代：子元素自己没写 `::first-letter` 就是 `None`，
/// 否则一个块的下沉会顺着树一路复制到每个嵌套容器。
#[test]
fn first_letter_does_not_inherit() {
    let page = author(
        "<div><p>文</p></div>",
        "div::first-letter { font-size: 3em } p::first-letter { font-size: 2em }",
    );
    let cap = style(&page, tag(&page, "p"))
        .first_letter
        .as_deref()
        .expect("p 自己写过");
    assert_eq!(cap.font_size, 32.0, "2em 按 p 的 16px，不是按 div 的 3em");
}

/// 候选链整条留下来（只留第一个的话，设备上没装第一名就等于作者的第二名也没了），
/// 引号与空白都清掉，逗号在引号里的不算分隔符。
#[test]
fn font_family_keeps_the_whole_candidate_chain() {
    let page = author(
        "<p class=a>一</p><p class=b>二</p><p class=c>三</p>",
        ".a { font-family: \"LXGW WenKai\", 'Noto Serif CJK SC', serif } \
         .b { font-family: easypub, \"Times New Roman\" } \
         .c { font: 1em \"Book Body\", serif }",
    );
    assert_eq!(
        style(&page, class(&page, "a")).font_families,
        ["LXGW WenKai", "Noto Serif CJK SC", "serif"]
    );
    assert_eq!(
        style(&page, class(&page, "b")).font_families,
        ["easypub", "Times New Roman"]
    );
    assert_eq!(
        style(&page, class(&page, "c")).font_families,
        ["Book Body", "serif"],
        "`font` 简写的字族部分同样留整条链"
    );
    assert_eq!(
        style(&page, class(&page, "a")).font_family,
        "LXGW WenKai",
        "第一个候选仍是 `font_family`"
    );
}

/// 引号里的逗号属于家族名，不是候选分隔符。
#[test]
fn comma_inside_quotes_is_not_a_candidate_separator() {
    let page = author("<p>文</p>", "p { font-family: \"A, B\", serif }");
    let s = style(&page, tag(&page, "p"));
    assert_eq!(s.font_families, ["A, B", "serif"]);
    assert_eq!(s.font_family, "A, B");
}

/// 候选链可继承：`body { font-family: X, Y }` 之后每个段落都带着这两名。
#[test]
fn the_candidate_chain_inherits() {
    let page = author(
        "<div><p>文</p></div>",
        "div { font-family: \"My Font\", \"Noto Serif\" }",
    );
    let p = style(&page, tag(&page, "p"));
    assert_eq!(p.font_families, ["My Font", "Noto Serif"]);
    assert_eq!(page.styled.ignored, 0);
}

#[test]
fn font_family_takes_the_first_entry_and_strips_quotes() {
    let page = author("<p>文</p>", "p { font-family: \"LXGW WenKai\", serif }");
    assert_eq!(style(&page, tag(&page, "p")).font_family, "LXGW WenKai");
}

/// `bolder` / `lighter` 相对**父字重**查表，不是两个常量。
/// 这条真落差在标题里：`<h1>` 已经是 700，里面再写 `bolder` 应当到 900，
/// 写成常量就与外层一样粗 —— 读者看到的是「强调没生效」。
#[test]
fn bolder_and_lighter_are_relative_to_the_parent_weight() {
    let page = author(
        "<div class=deep><p class=a>一</p></div><p class=b>二</p><p class=c>三</p>",
        "div { font-weight: 700 } \
         .a { font-weight: bolder } \
         .b { font-weight: bolder } \
         .c { font-weight: lighter }",
    );
    assert_eq!(
        style(&page, class(&page, "a")).font_weight,
        900,
        "父 700 → 900"
    );
    assert_eq!(
        style(&page, class(&page, "b")).font_weight,
        700,
        "父 400 → 700"
    );
    assert_eq!(
        style(&page, class(&page, "c")).font_weight,
        100,
        "父 400 的 lighter → 100（表就是这数）"
    );
}

/// 书里写非整档字重（`font-weight: 450`）是常态，查表前先归到最近的百位档。
#[test]
fn relative_weight_buckets_a_non_round_parent() {
    let page = author(
        "<div class=five><p class=a>一</p></div><div class=six><p class=b>二</p></div>",
        ".five { font-weight: 450 } .six { font-weight: 550 } \
         .a { font-weight: bolder } .b { font-weight: bolder }",
    );
    assert_eq!(
        style(&page, class(&page, "a")).font_weight,
        700,
        "450 归到 500 档"
    );
    assert_eq!(
        style(&page, class(&page, "b")).font_weight,
        800,
        "550 归到 600 档"
    );
}

/// 字距 2026-10-16 起真的生效了：`letter-spacing` 落到 `ComputedStyle.letter_spacing`，
/// 不再是「解析进去没人读」的空转字段。这一条钉的是「认得出」；四处同源（宽度表/绘制/
/// 命中的兑现）由 `layout` 与 `content::hit` 的测试守。
#[test]
fn letter_spacing_is_applied_and_inherited() {
    use crate::content::format::css::Declaration;
    use crate::content::format::css::computed::{DeclStatus, declaration_status};

    let status = |value: &str| {
        declaration_status(
            &Declaration {
                property: "letter-spacing".into(),
                value: value.into(),
                important: false,
            },
            &ComputedStyle::default(),
            16.0,
        )
    };
    assert_eq!(status("2px"), DeclStatus::Applied);
    assert_eq!(status("normal"), DeclStatus::Applied);

    // 值真的写进去了，且可继承（CSS 里 `letter-spacing` 是继承属性）。
    let page = author("<div><p>文</p></div>", "div { letter-spacing: 3px }");
    let p = style(&page, tag(&page, "p"));
    assert_eq!(p.letter_spacing, 3.0, "声明在 div 上，p 继承到同一份字距");
}

/// `text-justify` 认得并入库：`inter-ideograph` 折进 `InterCharacter`，且可继承
/// （书里在 `<body>` 上写一次就管全书）。
#[test]
fn text_justify_is_recognized_and_inherited() {
    let page = author(
        "<div><p>文</p></div>",
        "div { text-justify: inter-ideograph }",
    );
    let p = style(&page, tag(&page, "p"));
    assert_eq!(p.text_justify, TextJustify::InterCharacter);
}

/// 定位一族 2026-10-29 起真生效（绝对定位按这一章的盒摆），`z-index` 仍记
/// 「认得、不生效」—— 我们的层序是文档序，不是 z 树。
#[test]
fn the_position_family_is_applied_except_z_index() {
    use crate::content::format::css::Declaration;
    use crate::content::format::css::computed::{DeclStatus, declaration_status};

    let status = |property: &str, value: &str| {
        declaration_status(
            &Declaration {
                property: property.into(),
                value: value.into(),
                important: false,
            },
            &ComputedStyle::default(),
            16.0,
        )
    };
    for (property, value) in [
        ("position", "absolute"),
        ("position", "fixed"),
        ("position", "static"),
        ("left", "0%"),
        ("bottom", "12%"),
    ] {
        assert_eq!(
            status(property, value),
            DeclStatus::Applied,
            "`{property}: {value}` 现在真摆盒子"
        );
    }
    assert_eq!(
        status("position", "relative"),
        DeclStatus::Unparsed,
        "`relative` 要不脱流地挪已排好的子树，还没做 —— 判不认识"
    );
    assert_eq!(
        status("z-index", "3"),
        DeclStatus::Inert,
        "层序按文档序，不是 z 树"
    );
}

/// `currentColor` 在背景色与 `color` 上同样成立：`color` 那一批先落地，所以
/// `background-color: currentcolor` 写在它前面也折得对。
#[test]
fn current_color_works_for_background_and_color_itself() {
    let page = author(
        "<div><p class=a>甲</p><p class=b>乙</p></div>",
        "div { color: #112233 }          .a { background-color: currentcolor; color: #445566 }          .b { color: currentcolor }",
    );
    assert_eq!(
        style(&page, class(&page, "a")).background_color,
        Some(Color(0x44, 0x55, 0x66)),
        "背景跟的是同一元素自己的 `color`，与书写顺序无关"
    );
    assert_eq!(
        style(&page, class(&page, "b")).color,
        Some(Color(0x11, 0x22, 0x33)),
        "`color: currentcolor` 就是继承"
    );
    assert_eq!(page.styled.ignored, 0);
}

/// 属性选择器的五种算子：`~=` 与 `*=` 是出版书里的真写法。
#[test]
fn attribute_selectors_with_operators_match() {
    let page = author(
        "<p class=\"a solid b\">x</p><p class=\"biaoti-1\">y</p>",
        "[class~=\"solid\"] { color: #010203 } [class*=\"biaoti\"] { color: #040506 }",
    );
    assert_eq!(
        style(&page, class(&page, "solid")).color,
        Some(Color(1, 2, 3)),
        "[class~=\"solid\"] 要命中 `a solid b`"
    );
    assert_eq!(
        style(&page, class(&page, "biaoti-1")).color,
        Some(Color(4, 5, 6)),
        "[class*=\"biaoti\"] 要命中 `biaoti-1`"
    );
}

/// 名单里的字体 OpenType 属性报 `Inert`，不再算「不认识」——
/// `examples/css-gap` 的表要是把认得的属性写成「听都没听过」，
/// 看的人就会去补根本没有落差的地方。
#[test]
fn opentype_font_properties_report_inert_not_unknown() {
    use crate::content::format::css::Declaration;
    use crate::content::format::css::computed::{DeclStatus, declaration_status};

    let base = ComputedStyle::default();
    let status = |property: &str, value: &str| {
        declaration_status(
            &Declaration {
                property: property.into(),
                value: value.into(),
                important: false,
            },
            &base,
            16.0,
        )
    };
    // `font-stretch` / `font-kerning` / `font-feature-settings` 仍是整条「认得、不生效」：
    // 值写什么都不改变这件事。
    assert_eq!(status("font-stretch", "condensed"), DeclStatus::Inert);
    assert_eq!(status("font-kerning", "none"), DeclStatus::Inert);
    assert_eq!(
        status("font-feature-settings", "\"liga\" 0"),
        DeclStatus::Inert
    );
    // `font-variant` 与 `font-variant-caps` 2026-10-29 改成只收初值：`normal` 就是
    // 「不启用任何特性」，正是我们做的事；其余值（`small-caps` 这类）判不认识。
    assert_eq!(status("font-variant", "normal"), DeclStatus::Applied);
    assert_eq!(status("font-variant-caps", "normal"), DeclStatus::Applied);
    assert_eq!(status("font-variant", "small-caps"), DeclStatus::Unparsed);
    assert_eq!(
        status("font-variant-caps", "small-caps"),
        DeclStatus::Unparsed
    );
    assert_eq!(status("font-variant", "全角"), DeclStatus::Unparsed);
    // OEB 1.x 的栏数：`1` 就是不分栏（我们的行为），其余值要作者多栏。
    assert_eq!(status("oeb-column-number", "1"), DeclStatus::Applied);
    assert_eq!(status("oeb-column-number", "2"), DeclStatus::Unparsed);
    // `background: initial` 是重置块里的写法：清空图层与底色。
    assert_eq!(status("background", "initial"), DeclStatus::Applied);
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
    assert_eq!(px(style(&page, tag(&page, "p")).text_indent), 99.0);
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
    let styled = cascade(&dom, &rules, &base, "");
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
    assert_eq!(px(s.margin_top), 24.0, "1em 按偏好字号而不是 16px");
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
    let middle = style(&page, class(&page, "b"));
    assert_eq!(middle.baseline_shift, 0.0, "`middle` 不是基线上的偏移");
    assert_eq!(
        middle.cell_valign,
        crate::content::format::css::computed::CellVAlign::Middle,
        "`middle` 落到表格格那一格（格内竖直对齐），不收进 baseline_shift"
    );
    assert_eq!(page.styled.ignored, 0, "`middle` 是认得的取值");
    let bad = author("<p>x</p>", "p { vertical-align: 太上面了 }");
    assert_eq!(
        bad.styled.ignored, 1,
        "真不认的取值必须计 ignored，否则落差被静默吞掉"
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
    use crate::content::format::css::Declaration;
    use crate::content::format::css::computed::{DeclStatus, declaration_status};

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

/// 边框一族：四边简写要把三要素一次写全，颜色认 hex 与 `rgb()`。
#[test]
fn border_shorthand_fills_all_four_edges() {
    let page = author(
        "<p>正文</p>",
        "p { border: 2px solid #586357; background-color: rgb(193, 204, 192) }",
    );
    let style = style(&page, tag(&page, "p"));
    assert_eq!(style.border_drawn(), [2.0, 2.0, 2.0, 2.0]);
    assert_eq!(style.border[0].style, BorderStyle::Solid);
    assert_eq!(style.border[0].color, Some(Color(88, 99, 87)));
    assert_eq!(style.background_color, Some(Color(193, 204, 192)));
}

/// `background` 简写里的颜色要接住：出版书用「深色底块 + 白字标题」，
/// 只认 `background-color` 会让白字落在白纸上、整条标题消失（《罗生门》正是这么没的）。
#[test]
fn the_background_shorthand_yields_its_color() {
    let page = author(
        "<h1 class=t>标题</h1>",
        ".t { background: #b71c1c; color: #ffffff }",
    );
    let styled = style(&page, class(&page, "t"));
    assert_eq!(styled.background_color, Some(Color(0xb7, 0x1c, 0x1c)));
    assert_eq!(styled.color, Some(Color(255, 255, 255)));

    // 简写里带图时只取颜色；图（还没画）不影响这条声明被认下。
    let page = author(
        "<p class=t>正文</p>",
        ".t { background: url(x.png) no-repeat #fff }",
    );
    assert_eq!(
        style(&page, class(&page, "t")).background_color,
        Some(Color(255, 255, 255))
    );
}

/// 背景图那条路：`url()` 要留在图层表里，而不是像从前那样只把颜色挑出来、图整条丢掉。
/// 症状写在 `the_background_shorthand_clears_a_picture_it_does_not_mention` 那一条的注释上，
/// 是哈利·波特七张册扉页「白底上一片看不见的淡金字」。
#[test]
fn a_background_url_reaches_the_layer_table() {
    let page = author(
        "<p>正文</p>",
        "p { background-image: url(\"OEBPS/Images/a.png\") }",
    );
    let style = style(&page, tag(&page, "p"));
    assert_eq!(style.background_layers.len(), 1, "一层");
    assert_eq!(
        style.background_layers[0].image.as_deref(),
        Some("OEBPS/Images/a.png"),
        "路径原样留着：样式表拼接前已经折成容器内绝对路径，这里不该再动它"
    );
    // 没写的那三条落回 CSS 初值，不是落回「看不出来」的值。
    assert_eq!(style.background_layers[0].size, BackgroundSize::Auto);
    assert_eq!(style.background_layers[0].repeat.x, BackgroundTile::Repeat);
}

/// 简写要把一条声明里的**四件事**一起接住。这一条抄的是哈利波特 `.speci` 的原文
/// （`background: #f4f7f1 url(../Images/p0.png) no-repeat bottom right`
/// 加 `background-size: 55% auto`），次序随意是这一族的常态。
#[test]
fn the_background_shorthand_yields_every_item_it_carries() {
    let page = author(
        "<p class=t>正文</p>",
        ".t { background: #f4f7f1 url(\"OEBPS/Images/p0.png\") no-repeat bottom right;\n\
               background-size: 55% auto }",
    );
    let style = style(&page, class(&page, "t"));
    assert_eq!(style.background_color, Some(Color(0xf4, 0xf7, 0xf1)));
    let layer = &style.background_layers[0];
    assert_eq!(layer.image.as_deref(), Some("OEBPS/Images/p0.png"));
    assert_eq!(layer.repeat.x, BackgroundTile::NoRepeat);
    assert_eq!(layer.repeat.y, BackgroundTile::NoRepeat);
    // `bottom right`：两根轴都到末端。纵轴那个 `bottom` 不该被当成「还没写」。
    assert_eq!(
        layer.position.x,
        BackgroundAxis::Edge(BackgroundEdge::End, None)
    );
    assert_eq!(
        layer.position.y,
        BackgroundAxis::Edge(BackgroundEdge::End, None)
    );
    // 长手那条跟在简写后面，按层叠顺序盖住简写里同一属性 —— 这里简写没写尺寸，
    // 所以两条各说各的，不打架。
    assert_eq!(
        layer.size,
        BackgroundSize::Explicit {
            width: Some(Length::Percent(0.55)),
            height: None,
        }
    );
}

/// **简写没写到的长手要拉回初值**（CSS 明文）。这一条是这一轮最要紧的门：
/// 不重置的症状是转换器写的 `.a{background:url(x) center/cover}` 后面再跟一条
/// `.a{background:#fff}` 时，白底**叠在图上**，扉页那张底图变成灰蒙蒙一片。
/// 把 `apply_background_shorthand` 里那两行 `clear()` / `= None` 去掉，这一条立刻红。
#[test]
fn the_background_shorthand_clears_a_picture_it_does_not_mention() {
    let page = author(
        "<p class=t>正文</p>",
        ".t { background: url(\"a.png\") }\n.t { background: #ffffff }",
    );
    let style = style(&page, class(&page, "t"));
    assert_eq!(
        style.background_pictures().count(),
        0,
        "第二条简写没提图，就要把第一条那层图撤掉"
    );
    assert_eq!(style.background_color, Some(Color(255, 255, 255)));
}

/// 百分比**不许在层叠期折算**。`background-position` 的百分比基准是「盒减图」那段余量，
/// `background-size` 的是这一块自己的盒 —— 两个都要等排版期才拿得到。
/// 当场折成 px 等于拿窗口宽猜，正是 `margin` 那一族从前踩过的那一处。
#[test]
fn background_percentages_stay_unfolded() {
    let page = author(
        "<p class=t>正文</p>",
        ".t { background-image: url(\"a.png\"); background-position: 90% 14% }",
    );
    let position = style(&page, class(&page, "t")).background_layers[0].position;
    assert_eq!(position.x, BackgroundAxis::Fraction(0.9));
    assert_eq!(position.y, BackgroundAxis::Fraction(0.14));

    let page = author(
        "<p class=t>正文</p>",
        ".t { background-image: url(\"a.png\"); background-size: 50% }",
    );
    assert_eq!(
        style(&page, class(&page, "t")).background_layers[0].size,
        BackgroundSize::Explicit {
            width: Some(Length::Percent(0.5)),
            height: None,
        },
        "只写一条时第二轴是 `auto`，不是 0"
    );
}

/// `cover` / `contain` 是简写斜杠后面那一截，也是七张册扉页用的那两个词。
#[test]
fn background_size_keywords_survive_the_shorthand() {
    for (css, want) in [
        ("center/cover", BackgroundSize::Cover),
        ("center / contain", BackgroundSize::Contain),
        ("center", BackgroundSize::Auto),
    ] {
        let page = author(
            "<p class=t>正文</p>",
            &format!(".t {{ background: url(a.png) {css} }}"),
        );
        let layer = &style(&page, class(&page, "t")).background_layers[0];
        assert_eq!(layer.size, want, "写法 {css:?}");
        // 斜杠前面那一截仍然是位置，不是被尺寸吃掉半条声明。
        assert_eq!(
            layer.position.x,
            BackgroundAxis::Edge(BackgroundEdge::Center, None),
            "写法 {css:?} 的位置"
        );
    }
}

/// `repeat-x` 一句话就把**两根轴**都说了（纵轴是 `no-repeat`）。
/// 拆成「只说横轴、纵轴落回初值 `repeat`」，一条横向分隔纹理会变成整片马赛克。
#[test]
fn repeat_x_sets_both_axes_at_once() {
    let page = author(
        "<p class=t>正文</p>",
        ".t { background-image: url(a.png); background-repeat: repeat-x }",
    );
    let repeat = style(&page, class(&page, "t")).background_layers[0].repeat;
    assert_eq!(repeat.x, BackgroundTile::Repeat);
    assert_eq!(repeat.y, BackgroundTile::NoRepeat);
}

/// 图层数取各条列表的较大者，短的那一条**从头循环**（规范定的）。
/// 只把值给第一层的症状：第二张图在扉页上铺成一片马赛克。
#[test]
fn a_short_background_list_cycles_over_the_layers() {
    let page = author(
        "<p class=t>正文</p>",
        ".t { background-image: url(a.png), url(b.png); background-repeat: no-repeat }",
    );
    let layers = &style(&page, class(&page, "t")).background_layers;
    assert_eq!(layers.len(), 2, "图层数跟着最长那条列表走");
    assert_eq!(layers[0].repeat.x, BackgroundTile::NoRepeat);
    assert_eq!(
        layers[1].repeat.x,
        BackgroundTile::NoRepeat,
        "只有一条值时两层都要吃到它"
    );
}

/// 渐变这一版不画。**整条判不认识**而不是折成「这一层没有图」：
/// 后者会把作者的渐变底块悄悄画空，屏幕上与「书里本来没写」长得一模一样。
#[test]
fn a_gradient_background_is_counted_as_unrecognized() {
    let page = author(
        "<p class=t>正文</p>",
        ".t { background: linear-gradient(#fff, #000) }",
    );
    assert_eq!(page.styled.ignored, 1, "整条作废并计进 ignored");
    assert!(style(&page, class(&page, "t")).background_layers.is_empty());

    // 四值边缘语法同样判不认识（见 `parse_background_position` 那条注释）。
    // 注意这里**图那一层还在**：作废的只有 `background-position` 那一条声明，
    // 图是另一条已经接住的声明 —— 一条声明失败不许牵连另一条。
    let page = author(
        "<p class=t>正文</p>",
        ".t { background-image: url(a.png); background-position: left 10px top 20px }",
    );
    assert_eq!(page.styled.ignored, 1);
    let layer = &style(&page, class(&page, "t")).background_layers[0];
    assert_eq!(layer.image.as_deref(), Some("a.png"));
    assert_eq!(
        layer.position,
        BackgroundPosition {
            x: BackgroundAxis::Edge(BackgroundEdge::Start, None),
            y: BackgroundAxis::Edge(BackgroundEdge::Start, None),
        },
        "位置那条没接住，就该留在初值（CSS 初值是 `0% 0%` = 左上）"
    );
}

/// 背景图层**必须进那份手写的样式哈希**。块级排版缓存按子树样式哈希判命中，
/// 漏掉的表现不是画错而是画旧：只改 `background-size` 时缓存命中上一代，
/// 图仍按旧的摆 —— 读者改设置看不出为什么没变。
/// 把 `computed.rs` 里 `self.background_layers.len().hash(h)` 那一行删掉，这一条红。
#[test]
fn the_background_layers_change_the_style_hash() {
    let of = |css: &str| {
        let page = author("<p class=t>正文</p>", css);
        let mut hasher = std::hash::DefaultHasher::new();
        std::hash::Hash::hash(style(&page, class(&page, "t")), &mut hasher);
        std::hash::Hasher::finish(&hasher)
    };
    let base = ".t { background-image: url(a.png); background-size: cover }";
    assert_eq!(of(base), of(base), "同一条声明必须同哈希");
    assert_ne!(
        of(base),
        of(".t { background-image: url(a.png); background-size: contain }"),
        "只改尺寸也要换哈希"
    );
    assert_ne!(
        of(base),
        of(
            ".t { background-image: url(a.png); background-size: cover; background-repeat: no-repeat }"
        ),
        "只改铺法也要换哈希"
    );
    assert_ne!(
        of(base),
        of(".t { background-image: url(b.png); background-size: cover }"),
        "换一张图更要换哈希"
    );
}

/// 「认得、不生效」与「接住了」是两件事，缺口量具必须说得出这一层差别。
///
/// `background-attachment: fixed` 单写时屏上确实没有任何变化（流式排版里没有对应物，
/// 判据写在 `KNOWN_UNIMPLEMENTED` 那一格），所以它只能记 `Inert`。真在 `apply` 里给那条
/// 写一个 `=> true` 的臂，`examples/css-gap` 就会报「这个属性我们做完了」——
/// 那是把「没做」记成「做了」，比少接一条属性更坏。
/// 反过来，同一句话**写在简写里**时必须被吃掉而不作废整条声明：
/// `background: url(x) no-repeat fixed center` 里那个 `fixed` 要是不认，
/// 底色与图会一起消失，症状比「fixed 没生效」大一个量级。
#[test]
fn attachment_is_known_but_inert_while_the_shorthand_still_eats_it() {
    let page = author(
        "<p class=t>正文</p>",
        ".t { background-image: url(a.png); background-attachment: fixed; background-clip: padding-box }",
    );
    let own = style(&page, class(&page, "t"));
    assert_eq!(own.background_pictures().count(), 1, "图那条照常接住");
    assert_eq!(
        page.styled.ignored, 0,
        "两条认得的长手不该计进 ignored，它们是「不生效」不是「不认识」"
    );
    for name in [
        "background-attachment",
        "background-clip",
        "background-origin",
    ] {
        let decl = Declaration {
            property: name.to_string(),
            value: if name == "background-attachment" {
                "fixed".to_string()
            } else {
                "padding-box".to_string()
            },
            important: false,
        };
        assert_eq!(
            declaration_status(&decl, own, own.font_size),
            DeclStatus::Inert,
            "{name} 该报「认得，不生效」"
        );
    }

    // 同一句话挪进简写：不许作废整条声明。
    let page = author(
        "<p class=t>正文</p>",
        ".t { background: url(a.png) no-repeat fixed center }",
    );
    let own = style(&page, class(&page, "t"));
    assert_eq!(page.styled.ignored, 0);
    let picture = own
        .background_pictures()
        .next()
        .expect("简写里的 fixed 不该让整条作废");
    assert_eq!(picture.image.as_deref(), Some("a.png"));
    assert_eq!(picture.repeat.x, BackgroundTile::NoRepeat);
}

/// 背景色与背景图层**都不继承**（CSS 规定）。扉页那张底图只属于 `<body>` 自己，
/// 跟着继承走会让整章每一段都垫一层图。
#[test]
fn background_does_not_inherit() {
    let page = author(
        "<div class=w><p>正文</p></div>",
        ".w { background: #eee url(a.png) repeat }",
    );
    let style = style(&page, tag(&page, "p"));
    assert!(style.background_color.is_none());
    assert!(style.background_layers.is_empty());
}

/// 内联 `style="…"` 里的 `url(...)` **必须折成容器内路径**，与样式表那一侧同一个形状。
///
/// 这一条是 CI 上跑得到的那一半：真书门（`the_harry_potter_title_pages_carry_their_backdrop`）
/// 是 `#[ignore]`，而这处断口本来就写在**合成夹具**里就能复现的形状上 ——
/// 七张册扉页的原文就是 `<body style="background: url(../Images/v2.jpg) no-repeat center">`。
/// 不折的表现是图层表里躺着一条 `../Images/v2.jpg`：图片索引按容器路径精确查，查不到，
/// 屏上仍是「白底上一片看不见的淡金字」，而 `css-gap` 那台秤报「这条声明接住了」。
/// 把 `parse_inline_style` 里那一行 `absolutize_css_urls` 去掉，这一条立刻红。
#[test]
fn a_relative_url_in_an_inline_style_folds_like_a_stylesheet() {
    let page = page_in_dir(
        r#"<body style="background: url(../Images/v2.jpg) no-repeat center"><p>正文</p></body>"#,
        "",
        "OEBPS/Text/",
    );
    let body = page
        .styled
        .iter()
        .find(|(_, style)| !style.background_layers.is_empty())
        .map(|(id, _)| id)
        .expect("body 上那条内联样式该折出一层图");
    let picture = &page.styled.get(body).unwrap().background_layers[0];
    assert_eq!(
        picture.image.as_deref(),
        Some("OEBPS/Images/v2.jpg"),
        "按这一章自己的目录折，少一层就查不到"
    );
    // 同一句话写在样式表里，答案必须一模一样。基准**不同**是应该的：样式表写在
    // `OEBPS/Styles/main.css`，它那句 `../Images/…` 相对的是自己那一层目录；
    // 折算是拼进层叠**之前**做的（`book.rs` 对每一份样式表走一次 `absolutize_css_urls`），
    // 所以这里照抄那一步，而不是指望 `cascade` 去折样式表。
    let sheet_css = crate::content::format::absolutize_css_urls(
        "body { background: url(../Images/v2.jpg) no-repeat center }",
        "OEBPS/Styles/",
    );
    let sheet = page_in_dir("<p>正文</p>", &sheet_css, "OEBPS/Text/");
    let inner = sheet.styled.iter().find_map(|(_, style)| {
        style
            .background_layers
            .first()
            .and_then(|layer| layer.image.clone())
    });
    assert_eq!(
        inner.as_deref(),
        picture.image.as_deref(),
        "写在 class 里有底、写在 style 里没底，就是这两处形状漂了"
    );

    // 章内那块 `<style>` 由 `css::chapter_style_rules` 收进规则表（排版那一趟与量 CSS
    // 缺口的工具都读它），层叠这一侧根本不读 DOM 里的 `<style>` 元素 —— 这一点也顺手
    // 钉住：两边都收会收两次，两边都折会折两次。那条路的测试在同目录的 `css/tests.rs`。
    let style_block = page_in_dir(
        "<style>body { background-image: url(../Images/p.png) }</style><p>正文</p>",
        "",
        "OEBPS/Text/",
    );
    assert!(
        style_block
            .styled
            .iter()
            .all(|(_, style)| style.background_layers.is_empty()),
        "层叠不该自己去读章内 `<style>` —— 那是 `css::chapter_style_rules` 唯一的收口"
    );
}

/// `border: 20px` 没写样式 —— CSS 里这一条等于「这条边不存在」，宽度按 0 参与盒模型。
/// 把它当成「20px 的实线」来画，或者反过来保留上一条声明的样式，都是改作者的意思。
#[test]
fn a_border_width_without_a_style_draws_nothing() {
    let page = author("<p>正文</p>", "p { border: 20px }");
    assert_eq!(
        style(&page, tag(&page, "p")).border_drawn(),
        [0.0; 4],
        "样式是 none 时宽度不占位"
    );
}

/// 出版书章标题那一条：四值宽度 + 四值样式 + 一个颜色，左边一道 20px 双线。
#[test]
fn four_value_border_shorthands_land_on_the_matching_edges() {
    let page = author(
        "<h2 class=t>章</h2>",
        ".t { border-width: 0px 5px 0px 20px; border-style: none double none solid; border-color: #586357 }",
    );
    let style = style(&page, class(&page, "t"));
    assert_eq!(style.border_drawn(), [0.0, 5.0, 0.0, 20.0]);
    assert_eq!(
        style.border.map(|edge| edge.style),
        [
            BorderStyle::None,
            BorderStyle::Double,
            BorderStyle::None,
            BorderStyle::Solid
        ]
    );
}

/// 单边长写只动那一条边；一条边给两个宽度是错的，整条作废而不是取第一个。
#[test]
fn single_side_longhand_touches_one_edge_only() {
    let page = author(
        "<p>正文</p>",
        "p { border: 1px solid black; border-left-width: 6px }",
    );
    assert_eq!(
        style(&page, tag(&page, "p")).border_drawn(),
        [1.0, 1.0, 1.0, 6.0]
    );
    let wrong = author("<p>正文</p>", "p { border-top-width: 1px 2px }");
    assert_eq!(wrong.styled.ignored, 1, "一条边只能给一个宽度");
}

/// 边框的十档画法全部接住；认不得的值按「没接住」计数，而不是悄悄画成实线。
#[test]
fn every_border_style_is_accepted_and_unknown_words_are_counted() {
    for (value, want) in [
        ("none", BorderStyle::None),
        ("hidden", BorderStyle::None),
        ("solid", BorderStyle::Solid),
        ("double", BorderStyle::Double),
        ("dotted", BorderStyle::Dotted),
        ("dashed", BorderStyle::Dashed),
        ("inset", BorderStyle::Inset),
        ("outset", BorderStyle::Outset),
        ("groove", BorderStyle::Groove),
        ("ridge", BorderStyle::Ridge),
    ] {
        let page = author("<p>正文</p>", &format!("p {{ border: 2px {value} black }}"));
        assert_eq!(
            style(&page, tag(&page, "p")).border[0].style,
            want,
            "`border-style: {value}` 该接住"
        );
        assert_eq!(page.styled.ignored, 0, "`{value}` 不该算落差");
    }
    let unknown = author("<p>正文</p>", "p { border: 2px wavy black }");
    assert_eq!(
        unknown.styled.ignored, 1,
        "`wavy` 不是边框画法 —— 判不认识，不悄悄当 solid"
    );
    // 《西游记》那 101 个元素写的正是这条：颜色是 `currentColor`，画法是 `inset`。
    let book = author("<p>正文</p>", "p { border: currentColor inset 1px }");
    assert_eq!(book.styled.ignored, 0, "真书里那条写法该整条接住");
    assert_eq!(
        style(&book, tag(&book, "p")).border[0].style,
        BorderStyle::Inset
    );
    // `border-radius` 2026-10-29 接住了（底色那一块的圆角），不再是落差；
    // 仍不接的是单角长手，见 `border_radius_keeps_percent_and_rejects_bad_values`。
    let radius = author("<p>正文</p>", "p { border-radius: 4px }");
    assert_eq!(radius.styled.ignored, 0, "圆角已经接住，不许再算成落差");
    let corner_longhand = author("<p>正文</p>", "p { border-top-left-radius: 4px }");
    assert_eq!(corner_longhand.styled.ignored, 1, "单角长手仍不接");
}

/// 背景色与边框都**不**继承（CSS 规定）；`transparent` 是「有这条声明、但不画」。
#[test]
fn background_and_border_are_not_inherited() {
    let page = author(
        "<div><p>正文</p></div>",
        "div { background-color: #C1CCC0; border: 4px solid black } p { background-color: transparent }",
    );
    let div = style(&page, tag(&page, "div"));
    assert_eq!(div.background_color, Some(Color(193, 204, 192)));
    assert_eq!(div.border_drawn(), [4.0; 4]);
    let p = style(&page, tag(&page, "p"));
    assert_eq!(p.background_color, None, "子元素不继承父元素的背景色");
    assert_eq!(p.border_drawn(), [0.0; 4], "也不继承父元素的边框");
}

/// 手写 `Hash` 要覆盖**每个字段** —— 块级排版缓存靠它判「这个块的样式变了没」，
/// 漏一个字段就会把过期结果当成命中。这里抽几个代表字段（含浮点、枚举、子结构）
/// 钉住「改了就变」，并钉住「不改就一样」。
#[test]
fn style_hash_covers_fields() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    fn h(s: &ComputedStyle) -> u64 {
        let mut hasher = DefaultHasher::new();
        s.hash(&mut hasher);
        hasher.finish()
    }
    let base = ComputedStyle::default();
    assert_eq!(h(&base), h(&base.clone()), "同一份样式两次哈希要一样");

    let mut bigger = base.clone();
    bigger.font_size = 20.0;
    assert_ne!(h(&base), h(&bigger), "字号变了哈希该变");

    let mut spaced = base.clone();
    spaced.word_spacing = 3.0;
    assert_ne!(h(&base), h(&spaced), "词距变了哈希该变");

    let mut centered = base.clone();
    centered.text_align = TextAlign::Center;
    assert_ne!(h(&base), h(&centered), "对齐变了哈希该变");

    let mut family = base.clone();
    family.font_families = vec!["Noto Serif".to_string()];
    assert_ne!(h(&base), h(&family), "候选字体链变了哈希该变");

    let mut bordered = base.clone();
    bordered.border[0] = BorderEdge {
        width: 2.0,
        style: BorderStyle::Solid,
        color: None,
    };
    assert_ne!(h(&base), h(&bordered), "边框变了哈希该变");
}

// ---- 方向：`direction` 与 `start` / `end` ----

/// `direction` 以前在「认得、接受、不生效」那份名单里，现在它是真生效的**继承**属性。
///
/// 继承这一格是全部意义的来源：RTL 的书在 `<html>` 或 `<body>` 上写一次就管全书，
/// 断了继承等于每个段落都要重贴一遍。
#[test]
fn direction_applies_and_inherits() {
    let page = author("<div><p>文字</p></div>", "div { direction: rtl }");
    let div = tag(&page, "div");
    let p = tag(&page, "p");
    assert_eq!(style(&page, div).direction, Direction::Rtl, "写了就该生效");
    assert_eq!(
        style(&page, p).direction,
        Direction::Rtl,
        "子孙靠继承拿到方向"
    );
    // 反过来也要能翻回来：作者明确写 ltr 的一小节不该被整本书的方向带走。
    let back = author("<div dir=\"rtl\"><p>文字</p></div>", "p { direction: ltr }");
    assert_eq!(style(&back, tag(&back, "p")).direction, Direction::Ltr);
}

/// 竖排那根轴也靠继承走完全书：这样的本子只在 `<html>` 或 `<body>` 上写一次
/// `writing-mode: vertical-rl`，不会往每个段落重贴一遍。这一条钉的是继承没断 ——
/// 屏上这时候还是横排（排版层还没读这一格，见 [`writing_mode_is_applied_now_not_inert`]）。
#[test]
fn writing_mode_applies_and_inherits() {
    let page = author(
        "<div><p>文字</p></div>",
        "div { writing-mode: vertical-rl }",
    );
    let div = tag(&page, "div");
    let p = tag(&page, "p");
    assert_eq!(
        style(&page, div).writing_mode,
        WritingMode::VerticalRl,
        "写了就该进计算样式"
    );
    assert_eq!(
        style(&page, p).writing_mode,
        WritingMode::VerticalRl,
        "子孙靠继承拿到推进轴"
    );
    // 反过来：作者明确写回横排的一小节（竖排本里的拉丁引文、诗页那种）不该被整本书的轴带走。
    let back = author(
        "<div><p>文字</p></div>",
        "div { writing-mode: vertical-rl } p { writing-mode: horizontal-tb }",
    );
    assert_eq!(
        style(&back, tag(&back, "p")).writing_mode,
        WritingMode::HorizontalTb
    );
    // 一本没写这一条的书必须是横排初值：默认值错了是全书跟着歪。
    assert_eq!(
        style(&back, tag(&back, "div")).writing_mode,
        WritingMode::VerticalRl,
        "同一份夹具里 div 仍该是竖排"
    );
    let plain = author("<p>正文</p>", "");
    assert_eq!(
        style(&plain, tag(&plain, "p")).writing_mode,
        WritingMode::HorizontalTb,
        "初值"
    );
    assert_eq!(plain.styled.ignored, 0, "没写过这一条就不该有任何 ignored");
}

/// EPUB 里 RTL 的书几乎只在根元素写一个 `dir="rtl"`，不另外写样式表。
/// 只认样式表等于整本不认 —— 认这个属性才是接到真书的那条路。
///
/// 它是 presentational hint，所以排在作者样式**之下**：作者真写了 `direction` 就压回来。
#[test]
fn the_dir_attribute_sets_direction_and_author_css_beats_it() {
    let hinted = author("<p dir=\"rtl\">مرحبا</p>", "");
    assert_eq!(style(&hinted, tag(&hinted, "p")).direction, Direction::Rtl);
    // 大小写与空格都不该成为第二条门槛（真书里 `dir=" RTL "` 这种写法存在）。
    let sloppy = author("<p dir=\" RTL \">x</p>", "");
    assert_eq!(style(&sloppy, tag(&sloppy, "p")).direction, Direction::Rtl);
    let overridden = author("<p dir=\"rtl\">x</p>", "p { direction: ltr }");
    assert_eq!(
        style(&overridden, tag(&overridden, "p")).direction,
        Direction::Ltr,
        "作者层高于呈现提示"
    );
}

/// `dir="auto"` 按**内容**定方向：第一个强方向字符算哪一边就往哪边排。
///
/// 三条用例各钉一个真会踩的点：阿拉伯文的 Bidi_Class 是 `AL` 而**不是** `R`（只认 `R`
/// 会把一整类书判成 LTR）；数字是 `EN`、不属于强方向，所以「1234 后面跟阿拉伯文」仍然
/// 是 RTL（按「第一个非空白字符」判就会翻错）；汉字的 Bidi_Class 是 `L`。
/// 第四个 case 钉的是「一个强方向字符都没有」—— 那必须留继承值而不是猜成 ltr。
#[test]
fn dir_auto_resolves_from_the_first_strong_character() {
    for (body, want, why) in [
        ("مرحبا", Direction::Rtl, "阿拉伯文是 AL，也是强方向"),
        ("1234 مرحبا", Direction::Rtl, "数字不是强方向，要跳过去"),
        ("مرحبا 1234", Direction::Rtl, "首强在开头就已经定了"),
        ("中文正文", Direction::Ltr, "汉字的 Bidi_Class 是 L"),
        ("Hello", Direction::Ltr, "拉丁字母是 L"),
        (
            "1234 56,78",
            Direction::Ltr,
            "全是数字与标点：没有强方向字符，落回默认的 ltr",
        ),
    ] {
        let page = author(&format!("<p dir=\"auto\">{body}</p>"), "");
        assert_eq!(
            style(&page, tag(&page, "p")).direction,
            want,
            "dir=\"auto\" 的内容是 {body:?}（{why}）"
        );
    }
    // 一个强方向字符都没有时，继承的那一档不许被覆盖：阿拉伯语的书里一段纯页码
    // 该继续留在右边，而不是被折回 LTR。
    let nested = author("<div dir=\"rtl\"><p dir=\"auto\">1234</p></div>", "");
    let p = tag(&nested, "p");
    assert_eq!(
        style(&nested, p).direction,
        Direction::Rtl,
        "判不出方向就留继承值，「不知道」不等于「左」"
    );
}

/// CSS 的 `text-align` 初值是 `start` 而不是 `left`，两者在 RTL 下**结果相反**。
/// 合成一档就是把每一本 RTL 的书左对齐 —— 那是读者一眼看得出的错。
#[test]
fn the_four_alignment_keywords_stay_four_apart() {
    assert_eq!(
        ComputedStyle::default().text_align,
        TextAlign::Start,
        "初值该是 start（CSS 的规定），不是 left"
    );
    for (keyword, want) in [
        ("start", TextAlign::Start),
        ("end", TextAlign::End),
        ("left", TextAlign::Left),
        ("right", TextAlign::Right),
        ("center", TextAlign::Center),
        ("justify", TextAlign::Justify),
    ] {
        let page = author("<p>x</p>", &format!("p {{ text-align: {keyword} }}"));
        assert_eq!(
            style(&page, tag(&page, "p")).text_align,
            want,
            "text-align: {keyword}"
        );
    }
}

/// `start` / `end` 按 `direction` 折边，`left` / `right` 不折；折完的偏移只有一份算术。
#[test]
fn logical_alignment_resolves_against_direction() {
    use crate::content::format::css::computed::Direction as D;
    let off = |align: TextAlign, dir: D| align.offset_in(dir, 200.0, 48.0);
    assert_eq!(off(TextAlign::Start, D::Ltr), 0.0);
    assert_eq!(off(TextAlign::Start, D::Rtl), 152.0, "起始边翻到右");
    assert_eq!(off(TextAlign::End, D::Rtl), 0.0);
    assert_eq!(off(TextAlign::End, D::Ltr), 152.0);
    assert_eq!(off(TextAlign::Left, D::Rtl), 0.0, "作者钉死的左不许跟着翻");
    assert_eq!(off(TextAlign::Right, D::Ltr), 152.0);
    assert_eq!(off(TextAlign::Center, D::Rtl), 76.0);
    assert_eq!(
        off(TextAlign::Justify, D::Rtl),
        0.0,
        "两端对齐本身不产生偏移"
    );
    // 内容比版心还宽时不许给负偏移（旧的写法里有 `.max(0.0)`，这条不能悄悄丢）。
    assert_eq!(off(TextAlign::Start, D::Rtl), 152.0);
    assert_eq!(TextAlign::Right.offset_in(D::Ltr, 40.0, 48.0), 0.0);
}

/// 属性换了状态，`examples/css-gap` 那张表就不该再把它报成「认得但没接」。
#[test]
fn direction_is_applied_now_not_inert() {
    use crate::content::format::css::Declaration;
    use crate::content::format::css::computed::{DeclStatus, declaration_status};

    let base = ComputedStyle::default();
    let status = |value: &str| {
        declaration_status(
            &Declaration {
                property: "direction".into(),
                value: value.into(),
                important: false,
            },
            &base,
            16.0,
        )
    };
    assert_eq!(status("rtl"), DeclStatus::Applied);
    assert_eq!(status("LTR"), DeclStatus::Applied, "值大小写不敏感");
    assert_eq!(
        status("sideways"),
        DeclStatus::Unparsed,
        "认不出的值要进 ignored 计数"
    );
}

/// `writing-mode` 2026-10-19 起也离开了那份「认得、接受、不生效」名单，但**只走完半程**：
/// 值算出来了、进样式哈希、按 CSS 继承下来，而排版层还没读它，所以屏上仍是横排
/// （生效是 `plans/README.md` M8 后面那几刀的事）。
///
/// 因此这一条门断的是「`examples/css-gap` 那张表别再把它报成没接的属性」，不是「竖起来了」。
/// 报 `Applied` 是有意的：`DeclStatus` 讲的是**层叠有没有接住这条声明**，与它改过哪个像素
/// 无关 —— 把这半程写成 `Inert` 会让报表说「这本书的竖排声明丢了」，而事实是它已经进了
/// 计算样式，只差排版层去读。
#[test]
fn writing_mode_is_applied_now_not_inert() {
    use crate::content::format::css::Declaration;
    use crate::content::format::css::computed::{DeclStatus, declaration_status};

    let base = ComputedStyle::default();
    let status = |value: &str| {
        declaration_status(
            &Declaration {
                property: "writing-mode".into(),
                value: value.into(),
                important: false,
            },
            &base,
            16.0,
        )
    };
    assert_eq!(status("horizontal-tb"), DeclStatus::Applied);
    assert_eq!(
        status("vertical-rl"),
        DeclStatus::Applied,
        "竖排本里唯一的那条竖排来源"
    );
    assert_eq!(status("VERTICAL-LR"), DeclStatus::Applied, "值大小写不敏感");
    assert_eq!(
        status("  vertical-rl  "),
        DeclStatus::Applied,
        "两侧空格不该成为第二道门槛"
    );
    // 这两条折进 vertical-*：与 sideways 的唯一差别在拉丁侧转，而那一项本来就显式降级
    // （渲染器没有字形旋转通道），所以合起来报是诚实的，不是把写法悄悄改掉。
    assert_eq!(status("sideways-rl"), DeclStatus::Applied);
    assert_eq!(status("sideways-lr"), DeclStatus::Applied);
    // SVG 遗留那一族不接：本机书目里没出现过，而它是「先转整页再转字」的另一套语义。
    // 按不认识计数 = 记进报表，猜错语义 = 屏幕上一本读不下去的书。
    assert_eq!(
        status("tb-rl"),
        DeclStatus::Unparsed,
        "认不出的值要进 ignored 计数"
    );
    assert_eq!(status("auto"), DeclStatus::Unparsed);
}

/// 新增字段必须进样式哈希，否则块级缓存会读到过期结果（`ComputedStyle` 那段手写 `Hash`
/// 的规矩）。这一条是那个规矩在 `direction` 上的落点。
#[test]
fn the_style_hash_covers_direction() {
    let hash = |c: &ComputedStyle| {
        use std::hash::{Hash as _, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        c.hash(&mut h);
        h.finish()
    };
    let ltr = ComputedStyle::default();
    let rtl = ComputedStyle {
        direction: Direction::Rtl,
        ..ComputedStyle::default()
    };
    assert_ne!(hash(&ltr), hash(&rtl), "方向改了，块级缓存该失效");
}

/// 同一条规矩在 `writing-mode` 上的落点。这一格比 `direction` 更要紧：竖排一旦生效，
/// 块级缓存复用旧行盒的表现是「整章按错轴折行」，而样式哈希没变、章文本没变、
/// 读者偏好也没变 —— 只有这一格会动。
#[test]
fn the_style_hash_covers_writing_mode() {
    let hash = |c: &ComputedStyle| {
        use std::hash::{Hash as _, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        c.hash(&mut h);
        h.finish()
    };
    let horizontal = ComputedStyle::default();
    let vertical = ComputedStyle {
        writing_mode: WritingMode::VerticalRl,
        ..ComputedStyle::default()
    };
    assert_ne!(
        hash(&horizontal),
        hash(&vertical),
        "推进轴改了，块级缓存该失效"
    );
}

/// 「这一段在不在一条链接里」是 DOM 的事实，不是 CSS 属性。
///
/// 为什么引擎要带这一格：出版工具的目录页几乎不给链接上色 —— 本机那本基督山伯爵的目录章
/// 是 38 条 `<a class="calibre1">`，`stylesheet.css` 全文一个 `color` 都没有（`.calibre1`
/// 写的还是 `text-decoration: none`）。于是那一栏与正文逐字同色，读者看不出它是能点的。
/// 颜色由绘制层从**当前主题**取（`ui::widgets::run_color_link`），所以样式层只负责把
/// 「在链接里」这一件事传到 run 上，`color` 照旧留 `None`。
#[test]
fn an_anchor_with_a_target_marks_its_subtree_as_a_link() {
    let page = author(
        "<p><a href=\"part0042.html#x\">跳过去</a><a name=\"old\">老式锚</a>\
         <a href=\"b.html\"><span>套一层</span><b>再套一层</b></a></p>",
        "",
    );
    // 按标签加一条属性找一个元素：这份夹具里同标签有三个，位置不算身份。
    let one = |tag: &str, key: &str, value_prefix: &str| -> NodeId {
        page.dom
            .elements()
            .find(|(id, _)| {
                page.dom.tag_name(*id) == Some(tag)
                    && page
                        .dom
                        .attr(*id, key)
                        .is_some_and(|v| v.starts_with(value_prefix))
            })
            .map(|(id, _)| id)
            .unwrap_or_else(|| panic!("该找得到那个 <{tag} {key}=…>"))
    };

    let with_href = one("a", "href", "part0042");
    assert!(style(&page, with_href).link, "有目标的 <a> 是链接");
    assert_eq!(
        style(&page, with_href).color,
        None,
        "不许在这一层写死一个颜色 —— 色是主题的事"
    );

    // `<a name="old">` 那种老式锚点没有 `href`：点不动，染蓝等于骗读者。
    let named = page
        .dom
        .elements()
        .find(|(id, _)| {
            page.dom.tag_name(*id) == Some("a")
                && page.dom.attr(*id, "href").is_none()
                && page.dom.attr(*id, "name") == Some("old")
        })
        .map(|(id, _)| id)
        .expect("只有 name 的那个锚");
    assert!(!style(&page, named).link, "没有可去的地方的不算链接");

    let deep = page
        .dom
        .elements()
        .find(|(id, _)| page.dom.tag_name(*id) == Some("b"))
        .map(|(id, _)| id)
        .expect("链接里再套一层的 <b>");
    assert!(
        style(&page, deep).link,
        "链接里的后代也算链接的一部分（与装饰线向后代传播同理）"
    );

    // 传到 run 上：绘制层拿的是 `RunStyle`，不是 `ComputedStyle`。
    assert!(
        crate::layout::RunStyle::new(style(&page, with_href)).link,
        "run 要带得上这一格，否则界面读不到"
    );
    assert!(
        !crate::layout::RunStyle::new(style(&page, named)).link,
        "非链接的 run 不许带上这一格"
    );
}

/// 作者写了颜色就照作者的：主题那一格只在作者没写时才有话语权。
#[test]
fn an_author_color_on_a_link_beats_the_theme_slot() {
    let page = author("<p><a href=\"x.html\">绿的</a></p>", "a { color: #00ff00 }");
    let a = page
        .dom
        .elements()
        .find(|(id, _)| page.dom.tag_name(*id) == Some("a"))
        .map(|(id, _)| id)
        .expect("<a>");
    let styled = style(&page, a);
    assert!(styled.link, "在链接里这件事不变");
    assert_eq!(
        styled.color,
        Some(Color(0, 255, 0)),
        "作者写明了色，主题那一格让位"
    );
}

/// 两个 `min-*` 只收**长度**，`auto` 与 `none` 整条作废。
///
/// 前四条走 `declaration_status` —— 它是 `examples/css-gap.rs` 的唯一判定口，所以这一格
/// 同时钉住「`min-height` 不再报 Inert」「`min-width` 不再报不认识」，两边少一条那张
/// 落差表就指错地方；后两条钉住**不假装接住**：CSS 2.1 的 `<min-width>` / `<min-height>`
/// 不收这两个词（`min-*: auto` 是 CSS Sizing 3 给弹性盒的），浏览器整条丢掉，我们若在
/// 层叠期收成「清掉显式值并返回成功」，屏上没错而报表说好，那是更难查的一侧。
#[test]
fn min_width_and_min_height_take_lengths_only() {
    use crate::content::format::css::Declaration;
    use crate::content::format::css::computed::{DeclStatus, declaration_status};

    let status = |property: &str, value: &str| {
        declaration_status(
            &Declaration {
                property: property.into(),
                value: value.into(),
                important: false,
            },
            &ComputedStyle::default(),
            16.0,
        )
    };
    assert_eq!(status("min-width", "60px"), DeclStatus::Applied);
    assert_eq!(status("min-height", "50%"), DeclStatus::Applied);
    assert_eq!(status("min-width", "auto"), DeclStatus::Unparsed);
    assert_eq!(status("min-height", "none"), DeclStatus::Unparsed);

    let page = author(
        "<div class=\"a\"></div><div class=\"b\"></div>",
        ".a { font-size: 20px; min-width: 60px; min-height: 2em } \
         .b { min-width: auto; min-height: none }",
    );
    let a = style(&page, class(&page, "a"));
    assert_eq!(a.min_width, Some(Length::Px(60.0)));
    assert_eq!(
        a.min_height,
        Some(Length::Px(40.0)),
        "2em 按自己的 20px 字号"
    );
    let b = style(&page, class(&page, "b"));
    assert_eq!(b.min_width, None, "非法值不该留下任何一个数");
    assert_eq!(b.min_height, None);
    assert_eq!(page.styled.ignored, 2, "两条非法值都该计「不认识」");
}

/// 块级排版缓存按「这一块子树的样式」判命中，而 `ComputedStyle` 的 `Hash` 是逐字段手写的。
///
/// 新增字段忘了补那一行时，缓存会拿着旧的块高服务 —— 屏上高了对，滚动位置与目录进度
/// 还是旧的。这一格把两个 `min-*` 钉进哈希，让「忘了补」至少在这里变红一次。
#[test]
fn the_min_sizing_fields_reach_the_style_hash() {
    use std::hash::{Hash, Hasher};

    let hash_of = |style: &ComputedStyle| {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        style.hash(&mut h);
        h.finish()
    };
    let base = ComputedStyle::default();
    let wide = ComputedStyle {
        min_width: Some(Length::Px(60.0)),
        ..Default::default()
    };
    let tall = ComputedStyle {
        min_height: Some(Length::Px(60.0)),
        ..Default::default()
    };
    assert_ne!(hash_of(&wide), hash_of(&base), "min-width 没进哈希");
    assert_ne!(hash_of(&tall), hash_of(&base), "min-height 没进哈希");
}

/// `box-sizing` 只认两档，而**百分比不许在层叠期折**。
///
/// 后半句是这一条存在的主要理由：`width` 那一族的百分比折算自**包含块宽**，那是排版期
/// 才拿到手的数（`Length::Percent` 就是为它存在的）。谁在 `apply` 里把 `width: 50%` 折成
/// px，能用的基准只有字号 —— 折出来的其实是「父字号的一半」，600px 版心里变成 8px，
/// 而屏上只是那一块窄得离谱，报表上说这条已接住。
#[test]
fn box_sizing_takes_two_keywords_and_leaves_percent_for_the_layout_phase() {
    use crate::content::format::css::Declaration;
    use crate::content::format::css::computed::{BoxSizing, DeclStatus, declaration_status};

    let status = |value: &str| {
        declaration_status(
            &Declaration {
                property: "box-sizing".into(),
                value: value.into(),
                important: false,
            },
            &ComputedStyle::default(),
            16.0,
        )
    };
    assert_eq!(status("border-box"), DeclStatus::Applied);
    assert_eq!(status("content-box"), DeclStatus::Applied);
    // `padding-box`（css3-ui，只有个别引擎收）判不认识，而不是当成 `border-box`：
    // 那一档要把边框留着只扣内边距，认成邻近的一档就是错得看不出来。
    assert_eq!(status("padding-box"), DeclStatus::Unparsed);

    let page = author(
        "<div class=\"a\"></div><div class=\"b\"></div>",
        ".a { box-sizing: border-box; width: 50%; max-height: 40% } \
         .b { box-sizing: padding-box }",
    );
    let a = style(&page, class(&page, "a"));
    assert_eq!(a.box_sizing, BoxSizing::BorderBox);
    assert_eq!(
        a.width,
        Some(Length::Percent(0.5)),
        "border-box 不改百分比的**基准**，只改扣不扣边"
    );
    assert_eq!(a.max_height, Some(Length::Percent(0.4)));
    let b = style(&page, class(&page, "b"));
    assert_eq!(
        b.box_sizing,
        BoxSizing::ContentBox,
        "不认识的那一档不该改任何东西"
    );
    assert_eq!(page.styled.ignored, 1, "只有 padding-box 那条计不认识");

    // 默认档 = CSS 初值，且**不继承**（CSS 规定 `box-sizing` 非继承属性）。
    let nested = author(
        "<div class=o><p class=i>文</p></div>",
        ".o { box-sizing: border-box }",
    );
    assert_eq!(
        style(&nested, class(&nested, "i")).box_sizing,
        BoxSizing::ContentBox,
        "`box-sizing` 不跟着往下传"
    );
}

/// **真书门**：哈利·波特全集那七张册扉页，样式表里的整页底图必须真的落到图层表上，
/// 而且那个容器路径必须在这本书自己的图片清单里查得到。
///
/// 为什么值得为它开一次真书（`#[ignore]`，几秒）：这一条同时钉住两处**屏幕上看不出区别**
/// 的断口 ——
/// - 解析断了：`<body style="background: url(...) no-repeat center; background-size: cover">`
///   里那个 `url()` 被当成颜色之外的东西丢掉，扉页变成白底上一片看不见的淡金字；
/// - 图源断了：EPUB 的图片清单按 manifest 里所有 `image/*` 建（`epub.rs` 的
///   「全文图片」那一段），而样式表里的路径是 `absolutize_css_urls` 折算出来的形状。
///   两边**拼写对不上时屏上同样是白底**，而且比解析断了更难查 —— CSS 那侧一切正常。
///   只被 CSS 引用、不被任何 `<img src>` 引用的图（`v2.jpg` 就是）正踩在这一条上。
#[test]
#[ignore = "开真书并把 301 章的样式表走一遍"]
fn the_harry_potter_title_pages_carry_their_backdrop() {
    let path = std::path::Path::new("../../tests/fixtures/local/hali-bote-quanji.epub");
    if !path.is_file() {
        // 夹具目录在 .gitignore 里，克隆的人手上可能没有这一本 —— 跳过要说出来，
        // 静默通过等于这条门不存在。
        println!("跳过：{} 不在", path.display());
        return;
    }
    let doc = crate::content::document::open(path).expect("真书该打得开");
    let flow = doc.flow().expect("这本是可重排");
    let rules = RuleSet::parse(&doc.stylesheet(), Origin::Author);
    let base = ComputedStyle::default();

    // 期望值不写死在测试里，而是从这本书自己的图片清单反查：换一本再导一次也不会红在
    // 「书名改了」上，只会红在「CSS 引用的那张图不在清单里」这一件真事上。
    let listed: Vec<&str> = flow.images.iter().map(|i| i.href.as_str()).collect();
    let mut found = 0usize;
    for index in 0..flow.chapters.len() {
        let Ok(source) = doc.chapter_source(index) else {
            continue;
        };
        let dom = Dom::parse(source.as_bytes());
        // 基准目录按**这一章自己的** spine 项折出来，与界面那条路（`ChapterImages::new`
        // 拿的就是同一个数）逐字同一句。传空串会得到 `Images/v1.jpg` 这种少一层的形状，
        // 而清单里躺着的是 `OEBPS/Images/v1.jpg` —— 差的正是那一层，而且屏上看不出来。
        let chapter_dir = crate::content::format::dir_of(&flow.chapters[index].spine_href);
        let styled = cascade(&dom, &rules, &base, &chapter_dir);
        for (id, _) in dom.elements() {
            let Some(style) = styled.get(id) else {
                continue;
            };
            for picture in style.background_pictures() {
                let href = picture.image.clone().unwrap_or_default();
                found += 1;
                assert!(
                    listed.contains(&href.as_str()),
                    "第 {index} 章有一张背景图（{href}）不在这本书的图片清单里：\
                     屏上会是「有样式、没底色」，而 CSS 那侧一切正常，最难查的那一类",
                );
                // 扉页那一族写的是 `cover`，别在层叠里把它折没了。
                assert!(
                    matches!(
                        picture.size,
                        BackgroundSize::Cover
                            | BackgroundSize::Contain
                            | BackgroundSize::Auto
                            | BackgroundSize::Explicit { .. }
                    ),
                    "尺寸那一格必须有值"
                );
            }
        }
    }
    assert!(
        found >= 20,
        "这本书的 CSS-only 背景图应该至少 20 处（实测 23），只找到 {found} 处：\
         要么解析断了，要么样式表没读进来"
    );
}

// ---- 层叠期认得、但从没被写进任何一条测试的那些值 ----
//
// 这一段的来源是一次全量核对：把 `computed::apply` 每一条 match 臂接受的
// (属性, 值) 组合列出来，再拿全部 152 个带测试的文件对（既对声明原文、也对关键字
// 字面量，因为有几张表是 `for (keyword, want) in […]` 循环喂 `format!` 的，只搜声明
// 原文会把它们误报成没测）。剩下 28 条两种都对不上的，逐条补在下面。

/// `font-size` 那七个绝对关键字是 CSS 定死的一张比例表。从前只钉过 `large` 与
/// `smaller` 两格，其余五格漂了没人发现。
///
/// 注意 `medium` 恰好等于根字号 16.0：**光断言数值是空过的**（表整个删掉它也过），
/// 所以每一格同时钉 `ignored` 计数 —— 那条被判不认识时它会涨，这才是闸门。
#[test]
fn the_seven_absolute_font_size_keywords_are_the_css_table() {
    for (keyword, want) in [
        ("xx-small", 9.0),
        ("x-small", 10.0),
        ("small", 13.0),
        ("medium", 16.0),
        ("large", 18.0),
        ("x-large", 24.0),
        ("xx-large", 32.0),
    ] {
        let page = author("<p>x</p>", &format!("p {{ font-size: {keyword} }}"));
        assert_eq!(
            style(&page, tag(&page, "p")).font_size,
            want,
            "font-size: {keyword} 该是 {want}"
        );
        assert_eq!(page.styled.ignored, 0, "`font-size: {keyword}` 该被接住");
    }
}

/// `larger` 按父字号放大一档。从前这张表只钉过 `smaller` 那一侧，
/// 两侧共用一个 1.2 因子，只测一侧等于另一侧没闸门。
#[test]
fn larger_scales_the_parent_font_size_by_the_same_step_as_smaller() {
    let page = author(
        "<div><p>大</p><q>小</q></div>",
        "div { font-size: 20px } p { font-size: larger } q { font-size: smaller }",
    );
    assert_eq!(style(&page, tag(&page, "p")).font_size, 20.0 * 1.2);
    assert_eq!(style(&page, tag(&page, "q")).font_size, 20.0 / 1.2);
    assert_eq!(page.styled.ignored, 0);
}

/// `line-height: normal` 是 1.2 倍，而**初值是 1.75**（读者的默认行距）。
/// 两者不等，所以这一条不是空过的：写成 `1.75` 会被这里抓住。
#[test]
fn line_height_normal_is_the_css_multiple_not_the_reader_default() {
    let page = author("<p>文</p>", "p { line-height: normal }");
    assert_eq!(style(&page, tag(&page, "p")).line_height, 1.2);
    assert_eq!(page.styled.ignored, 0);
    assert_ne!(
        ComputedStyle::default().line_height,
        1.2,
        "初值若也是 1.2，上面那条断言就空过了"
    );
}

/// `font-weight` 的两个字面量与 `font-style: normal`：从前这张表只钉过
/// `bolder` / `lighter` / `italic` 三格，`normal` 与 `bold` 一个都没写过声明。
/// `font-weight: normal` 那一格要把继承来的 700 拉回 400，才看得出它真被读了。
#[test]
fn the_plain_weight_and_style_keywords_override_what_they_inherit() {
    let page = author(
        "<div><p class=a>粗</p><p class=b>正</p><q class=c>斜</q></div>",
        "div { font-weight: 700; font-style: italic } \
         .a { font-weight: bold } .b { font-weight: normal } .c { font-style: normal }",
    );
    assert_eq!(
        style(&page, class(&page, "a")).font_weight,
        700,
        "`bold` 就是 700"
    );
    assert_eq!(
        style(&page, class(&page, "b")).font_weight,
        400,
        "`normal` 该把继承来的 700 拉回 400"
    );
    assert!(!style(&page, class(&page, "c")).font_italic);
    assert_eq!(page.styled.ignored, 0);
}

/// `text-justify` 六种写法折成三档。从前只钉过 `inter-ideograph` 一格。
///
/// 折法是刻意的：`inter-character` / `inter-ideograph` / `distribute` 都是「在字间摊」，
/// 而 `auto` 与它们**不同档** —— `auto` 由实现决定，我们让排版层按「有空格拉空格、
/// 没有才拉字间」走，与 `inter-character` 那一臂同路但仍是独立的一格。
#[test]
fn text_justify_folds_six_spellings_into_three_arms() {
    for (keyword, want) in [
        ("auto", TextJustify::Auto),
        ("inter-word", TextJustify::InterWord),
        ("inter-character", TextJustify::InterCharacter),
        ("inter-ideograph", TextJustify::InterCharacter),
        ("distribute", TextJustify::InterCharacter),
        ("none", TextJustify::None),
    ] {
        let page = author("<p>x</p>", &format!("p {{ text-justify: {keyword} }}"));
        assert_eq!(
            style(&page, tag(&page, "p")).text_justify,
            want,
            "text-justify: {keyword}"
        );
        assert_eq!(page.styled.ignored, 0, "`{keyword}` 该被接住");
    }
}

/// 厂商前缀那几条拼法与标准名是**同一条声明**：EPUB 3 风格规范点名的正是带前缀的写法，
/// 只认标准名等于把按规范写的书丢掉。三条前缀从前一条都没进过测试。
#[test]
fn the_prefixed_spellings_reach_the_same_computed_value() {
    let page = author(
        "<p class=a>一</p><p class=b>二</p><p class=c>三</p><p class=d>四</p>",
        ".a { -webkit-hyphens: none } .b { -moz-hyphens: none } \
         .c { -epub-hyphens: auto } .d { -epub-text-justify: inter-ideograph }",
    );
    let find = |c: &str| class(&page, c);
    assert_eq!(style(&page, find("a")).hyphens, Hyphens::None);
    assert_eq!(style(&page, find("b")).hyphens, Hyphens::None);
    assert_eq!(style(&page, find("c")).hyphens, Hyphens::Auto);
    assert_eq!(
        style(&page, find("d")).text_justify,
        TextJustify::InterCharacter
    );
    assert_eq!(
        page.styled.ignored, 0,
        "四条前缀写法都认得，一条都不该进落差表"
    );
}

/// `hyphens: manual` 是 CSS 初值，与 `none` / `auto` 三档之一。光看数值会空过
/// （初值本来就是 `Manual`），所以这里让它去**盖掉**父层继承来的 `none`。
#[test]
fn hyphens_manual_overrides_an_inherited_none() {
    let page = author(
        "<div><p>文</p></div>",
        "div { hyphens: none } p { hyphens: manual }",
    );
    assert_eq!(
        style(&page, tag(&page, "p")).hyphens,
        Hyphens::Manual,
        "`manual` 该把继承的 `none` 盖回去"
    );
    assert_eq!(page.styled.ignored, 0);
}

/// `letter-spacing` / `word-spacing` 的 `normal`：折成 0，且要能盖掉继承来的字距。
/// 书里在 `<body>` 上写一次 `letter-spacing: 2px`、再在某一类元素上写 `normal` 复原，
/// 靠的就是这一档。
#[test]
fn the_spacing_keywords_normal_reset_an_inherited_track() {
    let page = author(
        "<div><p class=a>一</p><p class=b>二</p></div>",
        "div { letter-spacing: 3px; word-spacing: 4px } \
         .a { letter-spacing: normal } .b { word-spacing: normal }",
    );
    assert_eq!(style(&page, class(&page, "a")).letter_spacing, 0.0);
    assert_eq!(
        style(&page, class(&page, "a")).word_spacing,
        4.0,
        "另一条不受牵连"
    );
    assert_eq!(style(&page, class(&page, "b")).word_spacing, 0.0);
    assert_eq!(page.styled.ignored, 0);
}

/// `white-space`：`pre` 与 `pre-wrap` 在这一版共用同一个「保留换行」标记（折行与否
/// 交给排版，不是这一档的事），`normal` 把它关掉。
/// `nowrap` / `pre-line` 有意**不接**：四本真书实测 0 次，为它们把布尔换成五态枚举不值。
#[test]
fn white_space_pre_wrap_shares_the_pre_flag_and_normal_clears_it() {
    let page = author(
        "<p class=a>一</p><p class=b>二</p><p class=c>三</p><p class=d>四</p>",
        ".a { white-space: pre } .b { white-space: pre-wrap } \
         .c { white-space: normal } .d { white-space: nowrap }",
    );
    let find = |c: &str| class(&page, c);
    assert!(style(&page, find("a")).white_space_pre);
    assert!(
        style(&page, find("b")).white_space_pre,
        "`pre-wrap` 与 `pre` 同一档"
    );
    assert!(
        !style(&page, find("c")).white_space_pre,
        "`normal` 要关掉它"
    );
    assert_eq!(
        page.styled.ignored, 1,
        "只有 `nowrap` 那一条算落差；它被接进来了说明五态那一档被顺手做了"
    );
}

/// `display: list-item` 是「画标记」那一档的唯一入口（UA 只给 `<li>`），
/// 所以在 `<p>` 上写它应当真的让这一段带上标记序号。从前这条声明一次没写过。
#[test]
fn display_list_item_is_recorded_on_a_plain_element() {
    let page = author("<p>项</p>", "p { display: list-item }");
    assert_eq!(style(&page, tag(&page, "p")).display, Display::ListItem);
    assert_eq!(page.styled.ignored, 0);
}

/// `display: inline-block` 判**不认识**，不是「接住了」。
///
/// 排版层只分四档：`None` 跳过、`Inline` 走行内、`ListItem` 画标记、其余按块
/// （`layout::collect` 的 `is_inline_flow` 与 `is_item` 那两个判据）。从前这里存进一个
/// 谁都不读的 `InlineBlock`，屏上与 `display: block` 一字不差，而 `examples/css-gap`
/// 报 `Applied` —— 那是把「没做」记成「做了」。行内块要的 shrink-to-fit 宽与「占一行内
/// 位置」都还没做，所以这里明说没接住。本机四本真书这条写法 0 处。
#[test]
fn inline_block_is_not_accepted_as_a_display_value() {
    let base = ComputedStyle::default();
    let decl = |property: &str, value: &str| Declaration {
        property: property.into(),
        value: value.into(),
        important: false,
    };
    assert_eq!(
        declaration_status(&decl("display", "inline-block"), &base, 16.0),
        DeclStatus::Unparsed,
        "`display: inline-block` 不许报成接住了"
    );
    for value in ["block", "inline", "list-item", "none"] {
        assert_eq!(
            declaration_status(&decl("display", value), &base, 16.0),
            DeclStatus::Applied,
            "另外四档是真生效的，别跟着一起划掉"
        );
    }
    let page = author(
        "<div><p>x</p></div>",
        "div { display: block } p { display: inline-block }",
    );
    let p = style(&page, tag(&page, "p"));
    assert_eq!(page.styled.ignored, 1, "只有那条 inline-block 计落差");
    assert_eq!(
        p.display,
        Display::Block,
        "作废的声明不改已有值：这一格回到 CSS 初值，而不是留下一个没人读的记号"
    );
}

/// `break-before` / `break-after` 的 `avoid` 判**不认识**，理由与上面那一条同形：
/// 排版层除了 `is_forced` 与 `parity` 没有第三处读 `BreakWhen`，而 `is_forced` 明写
/// 「不是强制断」，所以存进去的 `Avoid` 是一个谁都不看的记号。
///
/// 别和 `break-inside: avoid` 搞混 —— 那一条落在 `break_inside_avoid` 这个布尔上，
/// 分片器真读，是真生效的。
#[test]
fn avoid_is_not_a_break_before_or_after_value_but_break_inside_avoid_is() {
    let base = ComputedStyle::default();
    let decl = |property: &str, value: &str| Declaration {
        property: property.into(),
        value: value.into(),
        important: false,
    };
    for property in [
        "break-before",
        "break-after",
        "page-break-before",
        "page-break-after",
    ] {
        assert_eq!(
            declaration_status(&decl(property, "avoid"), &base, 16.0),
            DeclStatus::Unparsed,
            "`{property}: avoid` 兑现不了，不该报成接住"
        );
    }
    assert_eq!(
        declaration_status(&decl("break-inside", "avoid"), &base, 16.0),
        DeclStatus::Applied,
        "`break-inside: avoid` 是另一条路，真的生效"
    );
    let page = author("<p>x</p>", "p { break-before: avoid }");
    assert_eq!(page.styled.ignored, 1);
    assert_eq!(style(&page, tag(&page, "p")).break_before, BreakWhen::Auto);
}

/// `break-before` / `break-after` 剩下的那几个值：`auto` 是**设定**而不是查询
/// （同一份样式表里先 `page` 后 `auto` 是合法写法，要能把强制断取消掉），
/// `always` 是与 `page` 同档的强制断（CSS 里 `always` 不带奇偶要求）。
#[test]
fn auto_cancels_a_forced_break_and_always_is_a_forced_break() {
    let page = author(
        "<p class=a>一</p><p class=b>二</p><p class=c>三</p>",
        ".a { break-before: page; break-after: page } \
         .b { break-before: auto; break-after: always } \
         .c { break-before: always; break-after: auto }",
    );
    let find = |c: &str| class(&page, c);
    assert_eq!(style(&page, find("a")).break_before, BreakWhen::Page);
    assert_eq!(
        style(&page, find("b")).break_before,
        BreakWhen::Auto,
        "`auto` 写在同一元素的后一条，要把前面的强制断取消"
    );
    assert_eq!(style(&page, find("b")).break_after, BreakWhen::Always);
    assert!(style(&page, find("b")).break_after.is_forced());
    assert_eq!(style(&page, find("c")).break_before, BreakWhen::Always);
    assert_eq!(page.styled.ignored, 0);
    assert_eq!(
        style(&page, find("c")).break_after.parity(),
        None,
        "`always` 不关心奇偶，与 `left` / `right` 有别"
    );
}

/// CSS2.1 的 `page-break-*` 前缀版本与标准名走同一条解析。
/// 老 EPUB 满地都是这一族写法。
#[test]
fn the_legacy_page_break_spellings_are_the_same_declaration() {
    let page = author(
        "<p class=a>一</p><p class=b>二</p>",
        ".a { page-break-before: page } .b { page-break-inside: avoid }",
    );
    assert_eq!(
        style(&page, class(&page, "a")).break_before,
        BreakWhen::Page,
        "`page-break-before` 与 `break-before` 同一个数"
    );
    assert!(style(&page, class(&page, "b")).break_inside_avoid);
    assert_eq!(page.styled.ignored, 0, "两条旧写法都认得");
}

/// `clear` 四个值：从前只钉过 `both`。左右两档是「只躲开那一侧的浮动」，
/// 存错一边表现为浮动把行带挤到屏外。
#[test]
fn clear_takes_each_side_and_none() {
    for (keyword, want) in [
        ("none", Clear::None),
        ("left", Clear::Left),
        ("right", Clear::Right),
        ("both", Clear::Both),
    ] {
        let page = author("<p>x</p>", &format!("p {{ clear: {keyword} }}"));
        assert_eq!(
            style(&page, tag(&page, "p")).clear,
            want,
            "clear: {keyword}"
        );
        assert_eq!(page.styled.ignored, 0);
    }
}

/// `background-repeat` 六个关键字与两值写法。从前只钉过 `repeat-x` 一格。
/// `repeat-x` / `repeat-y` 只能单独出现（CSS 规定），两值写法才是「横轴 纵轴」。
#[test]
fn background_repeat_sets_both_axes() {
    use BackgroundTile::{NoRepeat, Repeat, Round, Space};
    for (keyword, x, y) in [
        ("repeat", Repeat, Repeat),
        ("no-repeat", NoRepeat, NoRepeat),
        ("repeat-x", Repeat, NoRepeat),
        ("repeat-y", NoRepeat, Repeat),
        ("space", Space, Space),
        ("round", Round, Round),
    ] {
        let page = author("<p>x</p>", &format!("p {{ background-repeat: {keyword} }}"));
        let repeat = style(&page, tag(&page, "p")).background_layers[0].repeat;
        assert_eq!((repeat.x, repeat.y), (x, y), "background-repeat: {keyword}");
        assert_eq!(page.styled.ignored, 0, "`{keyword}` 该被接住");
    }
    let page = author("<p>x</p>", "p { background-repeat: no-repeat repeat }");
    let repeat = style(&page, tag(&page, "p")).background_layers[0].repeat;
    assert_eq!(
        (repeat.x, repeat.y),
        (NoRepeat, Repeat),
        "两值写法是「横轴 纵轴」"
    );
    assert_eq!(page.styled.ignored, 0);
    let bad = author("<p>x</p>", "p { background-repeat: repeat-x repeat-y }");
    assert_eq!(
        bad.styled.ignored, 1,
        "`repeat-x` 不能再当「只说横轴」，两值写法里它非法"
    );
}

/// `background-position` 的三个单边关键字：只给一根轴时另一根取 `center`（CSS 明文），
/// 而 CSS 初值是 `left top`（两轴都 `Start`），所以「关键字解错」与「这条没解」在数值上
/// 分得开。从前 `top` / `left` 钉过，`bottom` / `right` / `center` 一格都没有。
#[test]
fn background_position_keywords_fill_the_other_axis_with_center() {
    use BackgroundEdge::{Center, End, Start};
    let edge = |a: BackgroundAxis| match a {
        BackgroundAxis::Edge(edge, None) => Some(edge),
        _ => None,
    };
    for (keyword, x, y) in [
        ("center", Center, Center),
        ("bottom", Center, End),
        ("right", End, Center),
        ("left", Start, Center),
        ("top", Center, Start),
        ("left bottom", Start, End),
    ] {
        let page = author(
            "<p>x</p>",
            &format!("p {{ background-position: {keyword} }}"),
        );
        let position = style(&page, tag(&page, "p")).background_layers[0].position;
        assert_eq!(
            (edge(position.x), edge(position.y)),
            (Some(x), Some(y)),
            "background-position: {keyword}"
        );
        assert_eq!(page.styled.ignored, 0);
    }
    assert_eq!(
        BackgroundPosition::default(),
        BackgroundPosition {
            x: BackgroundAxis::Edge(Start, None),
            y: BackgroundAxis::Edge(Start, None),
        },
        "初值是 CSS 那一条（`left top`），不是 center —— 上面那几格才分得开"
    );
}

/// `text-decoration-line` 是简写的**长手别名**，从前一次没写过；
/// 线型与厚度那几个 token（`solid` / `double` / `dashed`）认得但画法不变
/// （一律 1px 实线、按当前文字色），`overline` 仍然整条不接。
/// 这里钉的是「不作废」与「不假装接住了线型」两件事。
#[test]
fn the_decoration_line_alias_and_the_line_style_tokens() {
    let page = author(
        "<p class=a>一</p><p class=b>二</p><p class=c>三</p>",
        ".a { text-decoration-line: underline } \
         .b { text-decoration: underline double } \
         .c { text-decoration: line-through dashed }",
    );
    let find = |c: &str| class(&page, c);
    assert!(style(&page, find("a")).underline);
    assert!(style(&page, find("b")).underline, "`double` 不许把整条作废");
    assert!(
        !style(&page, find("b")).line_through,
        "线型 token 不改线在哪"
    );
    assert!(style(&page, find("c")).line_through);
    assert!(!style(&page, find("c")).underline);
    assert_eq!(
        page.styled.ignored, 0,
        "长手别名与三种线型都认得：报成落差不对，报成接住了线型也不对"
    );
    let over = author("<p>x</p>", "p { text-decoration: overline }");
    assert_eq!(
        over.styled.ignored, 1,
        "`overline` 画不出来，仍要整条判不认识"
    );
    assert!(
        !style(&over, tag(&over, "p")).underline,
        "作废的声明不许留下半个下划线"
    );
}

/// `list-style-type: disc` 是 CSS 初值 —— 光断言值会空过，所以让它去**盖掉**父层的
/// `circle`。`arabic-indic` 是有意不收的那一族（与 `hebrew` 同一个理由：本机没有
/// 一本阿拉伯数字书可验记数规则），要落在落差表里而不是画出一个看着像的号。
#[test]
fn the_initial_marker_type_still_takes_and_arabic_indic_is_not_invented() {
    let page = author(
        "<ul><li class=a>一</li><li class=b>二</li></ul>",
        "ul { list-style-type: circle } .a { list-style-type: disc } \
         .b { list-style-type: arabic-indic }",
    );
    assert_eq!(
        style(&page, class(&page, "a")).list_style_type,
        ListStyle::Disc,
        "`disc` 虽是初值，也要真把继承来的 `circle` 盖掉"
    );
    assert_eq!(
        style(&page, class(&page, "b")).list_style_type,
        ListStyle::Circle,
        "认不出的类型回继承值，不降级成别的记数"
    );
    assert_eq!(page.styled.ignored, 1, "只有 `arabic-indic` 那一条算落差");
}

/// `box-sizing: content-box` 是 CSS 初值，与 `border-box` 相对。数值上它等于「没写」，
/// 所以钉的是它能盖掉父层的 `border-box` 且不作废。
#[test]
fn box_sizing_content_box_takes_back_an_inherited_border_box() {
    let page = author(
        "<div><p>文</p></div>",
        "div { box-sizing: border-box } p { box-sizing: content-box }",
    );
    assert_eq!(
        style(&page, tag(&page, "p")).box_sizing,
        BoxSizing::ContentBox
    );
    assert_eq!(page.styled.ignored, 0);
    assert_eq!(
        ComputedStyle::default().box_sizing,
        BoxSizing::ContentBox,
        "初值本身就是 content-box，所以上面那条靠的是继承被改回去"
    );
}

// ---- 2026-10-29：text-shadow / border-radius / duokan 前缀 ----

/// `text-shadow` 四段：位移、模糊半径、颜色；`em` 按**元素自己的**字号折，
/// 不写颜色就是 CSS 的 `currentColor`（存 `None`，绘制层落本 run 的文字色）。
#[test]
fn text_shadow_parses_offset_blur_and_color() {
    let page = author(
        "<p class=a>一</p><p class=b>二</p><p class=c>三</p>",
        ".a { text-shadow: 0 1px 2px #112233 } \
         .b { text-shadow: 0.2em 3px } \
         .c { text-shadow: 1px 1px currentcolor }",
    );
    let a = &style(&page, class(&page, "a")).text_shadows;
    assert_eq!(a.len(), 1);
    assert_eq!((a[0].dx, a[0].dy, a[0].blur), (0.0, 1.0, 2.0));
    assert_eq!(a[0].color, Some(Color(0x11, 0x22, 0x33)));
    let b = &style(&page, class(&page, "b")).text_shadows;
    assert_eq!(
        (b[0].dx, b[0].dy),
        (0.2 * 16.0, 3.0),
        "`em` 按元素自己的字号折"
    );
    assert_eq!(b[0].blur, 0.0);
    assert_eq!(b[0].color, None, "没写颜色就是 currentColor");
    let c = &style(&page, class(&page, "c")).text_shadows;
    assert_eq!(c[0].color, None, "`currentcolor` 与不写同一档");
    assert_eq!(page.styled.ignored, 0);
}

/// `text-shadow` 可继承（书里在 `<body>` 或标题规则上写一次就管一片），
/// 而 `none` 要能把继承来的那份**清掉** —— 两者在层叠上是两件事。
#[test]
fn text_shadow_inherits_and_none_clears_it() {
    let page = author(
        "<div><p class=a>一</p><p class=b>二</p></div>",
        "div { text-shadow: 1px 2px 3px #000 } .b { text-shadow: none }",
    );
    assert_eq!(
        style(&page, class(&page, "a")).text_shadows.len(),
        1,
        "子元素没写也带着继承来的影子"
    );
    assert!(
        style(&page, class(&page, "b")).text_shadows.is_empty(),
        "`none` 清掉继承值"
    );
    assert_eq!(page.styled.ignored, 0);
}

/// 逗号分层；写坏的层（少一个位移、负模糊半径）整条判不认识，而不是悄悄丢掉那一层。
#[test]
fn text_shadow_takes_a_list_and_rejects_broken_layers() {
    let page = author(
        "<p class=a>一</p><p class=b>二</p><p class=c>三</p>",
        ".a { text-shadow: 0 0 2px #000, 1px 1px #f00 } \
         .b { text-shadow: 1px } \
         .c { text-shadow: 1px 1px -2px #000 }",
    );
    assert_eq!(
        style(&page, class(&page, "a")).text_shadows.len(),
        2,
        "逗号分隔的两层都要在"
    );
    assert!(style(&page, class(&page, "b")).text_shadows.is_empty());
    assert!(style(&page, class(&page, "c")).text_shadows.is_empty());
    assert_eq!(page.styled.ignored, 2, "少位移与负模糊半径各计一条");
}

/// 样式哈希要跟上新字段：漏了它，块级排版缓存会在改了影子之后仍读旧结果。
#[test]
fn the_style_hash_covers_text_shadows_and_border_radius() {
    use std::hash::{Hash, Hasher};
    let page = author(
        "<p class=a>一</p><p class=b>二</p>",
        ".a { text-shadow: 0 1px 1px #000 } .b { border-radius: 4px }",
    );
    let hash = |s: &ComputedStyle| {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        s.hash(&mut h);
        h.finish()
    };
    let base = hash(&ComputedStyle::default());
    assert_ne!(
        hash(style(&page, class(&page, "a"))),
        base,
        "影子必须进哈希"
    );
    assert_ne!(
        hash(style(&page, class(&page, "b"))),
        base,
        "圆角必须进哈希"
    );
}

/// `border-radius` 的 1/2/3/4 值展开，顺序是 CSS 的角序 `[左上, 右上, 右下, 左下]`。
/// 2 值时第一个管对角、3 值时左下随右上 —— 与 `padding` 那套「上右下左」同名不同序，
/// 抄错了的症状是「斜着的两个角才好、另两个角方」。
#[test]
fn border_radius_expands_one_to_four_values() {
    for (value, want) in [
        ("4px", [4.0, 4.0, 4.0, 4.0]),
        ("4px 8px", [4.0, 8.0, 4.0, 8.0]),
        ("1px 2px 3px", [1.0, 2.0, 3.0, 2.0]),
        ("1px 2px 3px 4px", [1.0, 2.0, 3.0, 4.0]),
    ] {
        let page = author("<p>x</p>", &format!("p {{ border-radius: {value} }}"));
        let got = style(&page, tag(&page, "p")).border_radius.map(px);
        assert_eq!(got, want, "border-radius: {value}");
        assert_eq!(page.styled.ignored, 0);
    }
}

/// 百分比在层叠期**不折**（要等排版期拿到盒尺寸，与 `margin` 同一条口径）；
/// 负值与椭圆的斜杠写法整条作废；四个长手还不在清单里，判不认识而不是半接。
#[test]
fn border_radius_keeps_percent_and_rejects_bad_values() {
    let page = author(
        "<p class=a>一</p><p class=b>二</p><p class=c>三</p>",
        ".a { border-radius: 50% } .b { border-radius: -4px } .c { border-radius: 10px / 20px }",
    );
    let a = style(&page, class(&page, "a")).border_radius;
    assert!(
        matches!(a[0], Length::Percent(p) if (p - 0.5).abs() < 1e-6),
        "百分比要留到排版期折，实际 {:?}",
        a[0]
    );
    assert_eq!(page.styled.ignored, 2, "负值与椭圆写法各计一条");
    let longhand = author("<p>x</p>", "p { border-top-left-radius: 4px }");
    assert_eq!(
        longhand.styled.ignored, 1,
        "单角长手不在清单里：不许把它算成接住了"
    );
}

/// 折成 px：百分比按**盒宽**，每角封顶 `min(宽, 高) / 2`。
#[test]
fn corner_radii_resolve_against_the_box_and_clamp() {
    let style = ComputedStyle {
        border_radius: [
            Length::Px(6.0),
            Length::Percent(0.5),
            Length::Px(999.0),
            Length::Percent(0.1),
        ],
        ..ComputedStyle::default()
    };
    // 盒 100×40 → 封顶 20；`50%` 按宽折成 50 再被夹到 20。
    assert_eq!(style.corner_radii(100.0, 40.0), [6.0, 20.0, 20.0, 10.0]);
}

/// `duokan-text-indent` 是多看转换器写在标准属性旁边的那一份副本
/// （《哈利·波特》21 个规则块全部成对出现），收它就是同一个数。
#[test]
fn the_duokan_text_indent_prefix_is_the_same_declaration() {
    let page = author(
        "<p class=a>一</p><p class=b>二</p>",
        ".a { text-indent: 2em } .b { duokan-text-indent: 2em }",
    );
    assert_eq!(px(style(&page, class(&page, "a")).text_indent), 32.0);
    assert_eq!(
        px(style(&page, class(&page, "b")).text_indent),
        32.0,
        "两个拼法要给同一个数"
    );
    assert_eq!(page.styled.ignored, 0);
}

/// 影子列表进 `RunStyle` 之后仍要能去重：同值并成一格、不同值各占一格。
#[test]
fn the_run_style_carries_text_shadows_and_the_arena_tells_them_apart() {
    let page = author(
        "<p class=a>一</p><p class=b>二</p>",
        ".a { text-shadow: 0 1px 1px #000 } .b { text-shadow: 0 2px 1px #000 }",
    );
    let a = crate::layout::RunStyle::new(style(&page, class(&page, "a")));
    let a_again = crate::layout::RunStyle::new(style(&page, class(&page, "a")));
    let b = crate::layout::RunStyle::new(style(&page, class(&page, "b")));
    assert_eq!(a.text_shadows.len(), 1);
    assert_eq!(a, a_again, "同值要能并成一格（Hash + Eq 覆盖了新字段）");
    assert_ne!(a, b, "位移不同就是不同样式");
    let plain = crate::layout::RunStyle::new(&ComputedStyle::default());
    assert!(plain.text_shadows.is_empty());
    assert_ne!(a, plain);
}

/// `list-style-image` 与 `list-style-position` **按初值做就等于做对了**：标记只有文字、
/// 挂在流动之外，正是 `none` 与 `outside` 的意思。所以收初值、拒其余值 ——
/// 从前这两条整条记「认得不生效」，《蛇结》51 个 `li` 因此白挂 102 条 Inert。
#[test]
fn the_list_style_initials_are_applied_and_other_values_are_not() {
    let base = ComputedStyle::default();
    let decl = |property: &str, value: &str| Declaration {
        property: property.into(),
        value: value.into(),
        important: false,
    };
    assert_eq!(
        declaration_status(&decl("list-style-image", "none"), &base, 16.0),
        DeclStatus::Applied,
        "`none` 就是我们画标记的方式"
    );
    assert_eq!(
        declaration_status(&decl("list-style-position", "outside"), &base, 16.0),
        DeclStatus::Applied,
        "`outside` 就是我们摆标记的位置"
    );
    assert_eq!(
        declaration_status(&decl("list-style-image", "url(dot.png)"), &base, 16.0),
        DeclStatus::Unparsed,
        "图源还没做：判不认识，别报成接住"
    );
    assert_eq!(
        declaration_status(&decl("list-style-position", "inside"), &base, 16.0),
        DeclStatus::Unparsed,
        "`inside` 要把标记塞进首行，还没做"
    );
    assert_eq!(
        declaration_status(&decl("list-style-image", "none; x"), &base, 16.0),
        DeclStatus::Unparsed,
        "整条值里有别的 token 就不算初值"
    );
}

/// `border-collapse` 两个值都收，且**可继承**（写在 `<table>` 上要管到每个格子 ——
/// 合并是按格逐边判的，格子看不见它就没法判）。认不出的值计落差，不悄悄当 `separate`。
#[test]
fn border_collapse_parses_and_inherits_into_the_cells() {
    let page = author(
        "<table><tr><td>x</td></tr></table>",
        "table { border-collapse: collapse }",
    );
    assert!(style(&page, tag(&page, "table")).border_collapse);
    assert!(
        style(&page, tag(&page, "td")).border_collapse,
        "继承断在表格上，格子就判不了该不该画自己那条边"
    );
    assert_eq!(page.styled.ignored, 0);
    let separate = author(
        "<table><tr><td>x</td></tr></table>",
        "table { border-collapse: separate }",
    );
    assert!(!style(&separate, tag(&separate, "table")).border_collapse);
    assert_eq!(separate.styled.ignored, 0);
    let bad = author("<p>x</p>", "p { border-collapse: 塌陷 }");
    assert_eq!(bad.styled.ignored, 1, "认不出的值算落差");
    use std::hash::{Hash, Hasher};
    let hash = |s: &ComputedStyle| {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        s.hash(&mut h);
        h.finish()
    };
    assert_ne!(
        hash(style(&page, tag(&page, "td"))),
        hash(&ComputedStyle::default()),
        "这一格必须进样式哈希"
    );
}

/// 边框颜色里的 `currentColor`：它就是我们 `BorderEdge::color: None` 那个记号
/// （绘制层取这一块自己的 `color`）。从前它没被认，`border-top: currentColor none 0`
/// 这种写法会把**整条**边框丢掉 —— 《世界简史》5 个元素正是这么写的。
#[test]
fn current_color_is_accepted_wherever_a_border_color_can_go() {
    let page = author(
        "<p class=a>一</p><p class=b>二</p><p class=c>三</p>",
        ".a { border-top: currentColor none 0 } \
         .b { border-top: currentColor solid 2px } \
         .c { border-color: currentColor }",
    );
    let a = &style(&page, class(&page, "a")).border[0];
    assert_eq!((a.width, a.style, a.color), (0.0, BorderStyle::None, None));
    let b = &style(&page, class(&page, "b")).border[0];
    assert_eq!((b.width, b.style, b.color), (2.0, BorderStyle::Solid, None));
    assert_eq!(
        style(&page, class(&page, "c")).border[0].color,
        None,
        "长手里的 currentColor 同一个记号"
    );
    assert_eq!(page.styled.ignored, 0, "三条都该接住");
}

/// `cursor` 是界面提示，不是排版属性 —— 记「认得、不生效」，不再报成「不认识」。
#[test]
fn cursor_is_known_but_inert() {
    let base = ComputedStyle::default();
    let decl = |property: &str, value: &str| Declaration {
        property: property.into(),
        value: value.into(),
        important: false,
    };
    assert_eq!(
        declaration_status(&decl("cursor", "pointer"), &base, 16.0),
        DeclStatus::Inert
    );
    assert_eq!(
        declaration_status(&decl("cursor", "眨眼"), &base, 16.0),
        DeclStatus::Inert,
        "名单里的属性不看值 —— 值怎么写在屏上都不生效，这是名单的语义"
    );
}

/// `word-break` 只收 `normal` / `break-all` 两个值，且**可继承**（书里在 `<body>` 上写一次）。
/// `keep-all` 判不认识 —— 它要的是「中文也不许逐字断」，与我们的断行模型相反。
#[test]
fn word_break_takes_two_values_and_inherits() {
    let page = author("<div><p>x</p></div>", "div { word-break: break-all }");
    assert_eq!(
        style(&page, tag(&page, "p")).word_break,
        WordBreak::BreakAll,
        "可继承：段落没写也带着"
    );
    assert_eq!(page.styled.ignored, 0);
    let keep = author("<p>x</p>", "p { word-break: keep-all }");
    assert_eq!(keep.styled.ignored, 1, "`keep-all` 判不认识");
    assert_eq!(
        style(&keep, tag(&keep, "p")).word_break,
        WordBreak::Normal,
        "认不出的值不改已有值"
    );
    use std::hash::{Hash, Hasher};
    let hash = |s: &ComputedStyle| {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        s.hash(&mut h);
        h.finish()
    };
    assert_ne!(
        hash(style(&page, tag(&page, "p"))),
        hash(&ComputedStyle::default())
    );
}

/// `ex` 单位：按 `0.5em` 近似（拿不到 x-height 真度量）。7.5ex × 16px = 60px。
#[test]
fn the_ex_unit_approximates_half_an_em() {
    let page = author("<p>x</p>", "p { padding-left: 7.5ex; font-size: 20px }");
    assert_eq!(
        px(style(&page, tag(&page, "p")).padding_left),
        75.0,
        "7.5ex、字号 20 → 7.5 × 10"
    );
    assert_eq!(page.styled.ignored, 0);
}

/// `all: initial`：把**每一个**属性拉回初值（含继承来的），`link` 那个记号留着
/// （它不是 CSS 属性，是「这段在不在 `<a>` 里」）。
#[test]
fn all_initial_resets_every_property() {
    let page = author(
        "<div><p class=a>甲</p><p class=b>乙</p></div>",
        "div { color: #112233; font-size: 20px; text-align: center } \
         .a { all: initial } .b { all: initial; text-align: center }",
    );
    let a = style(&page, class(&page, "a"));
    assert_eq!(a.font_size, 16.0, "字号回到初值，不再继承 20");
    assert_eq!(a.color, None, "颜色回到「没写过」");
    assert_eq!(a.text_align, TextAlign::Start, "对齐回到 start");
    assert_eq!(
        style(&page, class(&page, "b")).text_align,
        TextAlign::Center,
        "同一块里 `all` 后面的声明照样盖过它"
    );
    assert_eq!(page.styled.ignored, 0);
    // 认不出的写法（`all: unset` / `all: inherit`）判不认识。
    let bad = author("<p>x</p>", "p { all: unset }");
    assert_eq!(bad.styled.ignored, 1);
}
