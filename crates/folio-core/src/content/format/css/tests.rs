//! CSS 子集：规则装配、层叠与 specificity。只依赖公开 API。

use crate::content::dom::{Dom, NodeId};
use crate::content::format::css::computed::{Color, ComputedStyle, cascade};
use crate::content::format::css::{
    Origin, RuleSet, chapter_style_rules, style_tag_text, style_text_of,
};
use crate::content::format::dir_of;

fn rules(text: &str) -> RuleSet {
    RuleSet::parse(text, Origin::Author)
}

fn first_p(dom: &Dom) -> NodeId {
    dom.elements()
        .find(|(_, n)| {
            n.tag
                .as_ref()
                .map(|t| t.local.to_string())
                .unwrap_or_default()
                == "p"
        })
        .map(|(id, _)| id)
        .expect("应有 p")
}

/// `@charset` / `@import` 没有块，在分号处结束。把它们和后面那条规则一起当成
/// 「at-rule 前奏」吞掉，是最典型的「样式表明明在、body 规则却凭空没了」。
#[test]
fn statement_at_rules_end_at_the_semicolon() {
    let set =
        rules("@charset \"utf-8\"; body { font-family: serif } @import \"x\"; p { color: red }");
    assert_eq!(set.rules.len(), 2, "两条普通规则都要活下来");
    assert_eq!(set.rules[0].declarations[0].property, "font-family");
    assert_eq!(set.rules[1].declarations[0].property, "color");
    assert_eq!(set.ignored, 2, "@charset 与 @import 各计一条不支持");
}

#[test]
fn parses_plain_rules() {
    let set = rules("p { color: red; font-size: 14px } .intro { text-indent: 2em; }");
    assert_eq!(set.rules.len(), 2);
    assert_eq!(set.rules[0].declarations.len(), 2);
    assert_eq!(set.rules[0].declarations[0].property, "color");
    assert_eq!(set.rules[0].declarations[0].value, "red");
    assert_eq!(set.rules[1].declarations[0].value, "2em");
    assert_eq!(set.ignored, 0);
}

#[test]
fn trailing_semicolon_and_missing_one_are_both_fine() {
    for text in ["p { color: red }", "p { color: red; }", "p{color:red;}"] {
        let set = rules(text);
        assert_eq!(set.rules.len(), 1, "{text}");
        assert_eq!(set.rules[0].declarations.len(), 1, "{text}");
    }
}

#[test]
fn comments_do_not_break_rule_splitting() {
    let set = rules("/* p { color: blue } */ p { color: red } /* 尾巴 */");
    assert_eq!(set.rules.len(), 1, "注释里的规则不该被当真规则");
    assert_eq!(set.rules[0].declarations[0].value, "red");
}

#[test]
fn braces_inside_strings_are_balanced_correctly() {
    let set = rules(r#"p[content="{"] { color: red }"#);
    assert_eq!(set.rules.len(), 1);
}

#[test]
fn media_queries_are_inlined_and_other_at_rules_dropped() {
    let set = rules(
        "@media screen { p { color: red } } @font-face { font-family: X } @page { margin: 1cm }",
    );
    assert_eq!(set.rules.len(), 1, "只有 @media 里的规则该被收进来");
    assert_eq!(set.rules[0].declarations[0].property, "color");
    assert_eq!(
        set.ignored, 0,
        "@page 的页边距与 @font-face 都有去处，不算被忽略"
    );
    assert_eq!(
        set.page_margins.top,
        Some(crate::content::format::css::computed::Length::Px(
            96.0 / 2.54
        )),
        "1cm 按 96 dpi 折成像素"
    );
    assert_eq!(
        set.font_faces.len(),
        1,
        "@font-face 是「这本书带了字体」的唯一线索，丢了就等于对内嵌字体装看不见"
    );
    assert_eq!(set.font_faces[0].family, "X");
}

/// `@page` 的边距是整本书的默认页边距：简写按 CSS 的四值规则展开，没出现的边保持
/// `None`（后一份样式表才能只覆盖自己写的那几条）。认不出的页属性逐条计 ignored。
#[test]
fn page_margins_are_parsed_for_the_reader() {
    use crate::content::format::css::computed::Length;

    let set = rules("@page { margin: 1px 2% 3px 4px; padding: 0 }");
    assert_eq!(set.page_margins.top, Some(Length::Px(1.0)));
    assert_eq!(set.page_margins.right, Some(Length::Percent(0.02)));
    assert_eq!(set.page_margins.bottom, Some(Length::Px(3.0)));
    assert_eq!(set.page_margins.left, Some(Length::Px(4.0)));
    assert_eq!(set.ignored, 1, "@page 里的 padding 没有去处");

    let sides = rules("@page { margin-top: 12px; margin-bottom: 0 }");
    assert_eq!(sides.page_margins.top, Some(Length::Px(12.0)));
    assert_eq!(sides.page_margins.bottom, Some(Length::Px(0.0)));
    assert_eq!(sides.page_margins.left, None, "没写的边不能被补成 0");
    assert_eq!(sides.ignored, 0);

    // 百分比边距由阅读器按页框宽折；没声明的边折出来是 0。
    let (top, bottom) = sides.page_margins.vertical_px(600.0);
    assert_eq!((top, bottom), (12.0, 0.0));
    let percent = rules("@page { margin-top: 10% }");
    assert_eq!(percent.page_margins.top, Some(Length::Percent(0.1)));
    assert_eq!(
        percent.page_margins.vertical_px(600.0),
        (60.0, 0.0),
        "百分比按页框宽折"
    );
}

/// 多份样式表按出现顺序拼接：后一份的 `@page` 只覆盖它写出来的边，没写的边保留前一份。
#[test]
fn a_later_page_rule_only_overrides_the_sides_it_writes() {
    let mut first = rules("@page { margin-top: 10px; margin-bottom: 20px }");
    first.extend(rules("@page { margin-top: 30px }"));
    assert_eq!(
        first.page_margins.top,
        Some(crate::content::format::css::computed::Length::Px(30.0))
    );
    assert_eq!(
        first.page_margins.bottom,
        Some(crate::content::format::css::computed::Length::Px(20.0)),
        "后一份没写 bottom，前一份的值要留着"
    );
}

/// `src` 的写法比我们能用的多：`format()` 与 `tech()` 是修饰前一个来源的函数，
/// 逗号把多个来源分开，而 `url()` 里的路径自己可能带逗号。
#[test]
fn font_face_src_keeps_urls_and_locals() {
    let set = rules(
        "@font-face { font-family: Body; src: url(../Fonts/body.otf) format(\"opentype\"), \
             local(\"Book Serif\"), url(\"Fonts/body italic.otf\") }",
    );
    assert_eq!(set.font_faces.len(), 1);
    assert_eq!(
        set.font_faces[0].src,
        vec![
            crate::content::format::css::FontSrc::Url("../Fonts/body.otf".into()),
            crate::content::format::css::FontSrc::Local("Book Serif".into()),
            crate::content::format::css::FontSrc::Url("Fonts/body italic.otf".into()),
        ],
        "来源要保住书写顺序：第一个是首选"
    );
}

/// 远程字体一期不取，但也不能假装这本书没有字体：只丢这一条来源，同块里的
/// 本地来源照收。
#[test]
fn remote_src_is_dropped_but_local_ones_survive() {
    let set = rules(
        "@font-face { font-family: Body; src: url(https://x/y.otf), url(//z.otf), \
             url(C:/Fonts/y.otf), url(y.otf) }",
    );
    assert_eq!(
        set.font_faces[0].src,
        vec![
            crate::content::format::css::FontSrc::Url("C:/Fonts/y.otf".into()),
            crate::content::format::css::FontSrc::Url("y.otf".into()),
        ],
        "带协议的三条被丢；盘符开头的不是协议，得留着"
    );
}

/// `res://` / `file://` / 绝对路径指的是**设备上的字体文件**，不是容器里的资源：
/// 留下来交给平台层按文件名找系统家族；http(s) 仍然丢掉。
#[test]
fn device_font_sources_are_kept_as_system_references() {
    let set = rules(
        "@font-face { font-family: Body; src: url(res:///system/fonts/DroidSansFallback.ttf), \
             url(file:///usr/share/fonts/x.otf), url(/system/fonts/y.ttc), \
             url(https://x/z.otf), url(//host/w.otf) }",
    );
    assert_eq!(
        set.font_faces[0].src,
        vec![
            crate::content::format::css::FontSrc::System(
                "res:///system/fonts/DroidSansFallback.ttf".into()
            ),
            crate::content::format::css::FontSrc::System("file:///usr/share/fonts/x.otf".into()),
            crate::content::format::css::FontSrc::System("/system/fonts/y.ttc".into()),
        ],
        "设备字体来源要保住书写顺序；远程那两条仍然丢"
    );
}

/// 一条声明挂两个别名是 CSS 的正常写法（同一份数据，两个名字都能引用到）。
#[test]
fn font_face_family_list_yields_one_face_per_name() {
    let set = rules("@font-face { font-family: \"Body\", BodyAlt; src: url(y.otf) }");
    let families: Vec<&str> = set.font_faces.iter().map(|f| f.family.as_str()).collect();
    assert_eq!(
        families,
        ["Body", "BodyAlt"],
        "先去引号再切逗号会把这串削坏"
    );
}

#[test]
fn font_face_weight_and_style_are_parsed() {
    let set = rules(
        "@font-face { font-family: B; font-weight: 700 900; font-style: oblique; src: url(b.otf) }",
        // 区间取第一个值；oblique 在 FontKey 那一侧与 italic 同一个布尔。
    );
    assert_eq!(set.font_faces[0].weight, 700);
    assert_eq!(set.font_faces[0].italic, Some(true));

    let plain = rules("@font-face { font-family: B; src: url(b.otf) }");
    assert_eq!(plain.font_faces[0].weight, 400);
    assert_eq!(
        plain.font_faces[0].italic, None,
        "没写 font-style 不等于写了 normal"
    );
}

/// 缺 `font-family` 的那块无处可查，整块作废；其余认不出的写法只少一条线索。
#[test]
fn font_face_without_family_is_dropped_but_stray_declarations_are_not() {
    let no_family = rules("@font-face { src: url(y.otf) }");
    assert!(no_family.font_faces.is_empty());

    let stray = rules("@font-face { font-family: B; src: url(y.otf); panax: 3 }");
    assert_eq!(stray.font_faces.len(), 1, "认不出的声明不该带走整块");
    assert_eq!(stray.ignored, 1);
}

#[test]
fn font_faces_from_both_stylesheets_are_kept_in_order() {
    let mut set = rules("@font-face { font-family: A; src: url(a.otf) }");
    set.extend(rules("@font-face { font-family: B; src: url(b.otf) }"));
    let families: Vec<&str> = set.font_faces.iter().map(|f| f.family.as_str()).collect();
    assert_eq!(
        families,
        ["A", "B"],
        "两份 CSS 的字体声明都要活着，按出现顺序"
    );
}

/// 字族匹配是**整串同名**。`"Book Serif"` 里就含 `serif`，用子串会把泛族名
/// 也认成书的字体，而那正是要落回读者偏好字体的那一种。
#[test]
fn font_face_matches_are_whole_string_and_case_insensitive() {
    assert!(crate::content::format::css::font_face_matches(
        "Book Serif",
        "book serif"
    ));
    assert!(!crate::content::format::css::font_face_matches(
        "serif",
        "Book Serif"
    ));
    assert!(!crate::content::format::css::font_face_matches(
        "",
        "Book Serif"
    ));
}

#[test]
fn important_flag_is_recognised() {
    let set = rules("p { color: red !important; font-size: 12px }");
    assert!(set.rules[0].declarations[0].important);
    assert!(!set.rules[0].declarations[1].important);
}

#[test]
fn unsupported_selector_discards_the_whole_rule_body() {
    let set = rules("p:hover { color: red; font-size: 1px } p { color: blue }");
    assert_eq!(set.rules.len(), 1, "带 :hover 的整条应被丢掉");
    assert_eq!(set.rules[0].declarations[0].value, "blue");
    assert_eq!(set.ignored, 2, "两条声明都该被计成被忽略");
}

#[test]
fn malformed_tail_does_not_panic() {
    for text in [
        "p { color: red",
        "}",
        "{ color: red }",
        "p {",
        "",
        "p { }",
        "p { : red }",
        "@media",
    ] {
        let _ = rules(text);
    }
}

fn winner(dom: &Dom, set: &RuleSet, property: &str) -> String {
    let p = first_p(dom);
    set.matching_declarations(dom, p)
        .into_iter()
        .rfind(|d| d.property == property)
        .map(|d| d.value.clone())
        .unwrap_or_default()
}

#[test]
fn author_normal_beats_reader_normal() {
    let dom = Dom::parse(b"<p id='b'>x</p>");
    let mut set = RuleSet::parse("p { color: black }", Origin::Reader);
    set.extend(RuleSet::parse("p { color: red }", Origin::Author));
    assert_eq!(winner(&dom, &set, "color"), "red");
}

#[test]
fn four_cascade_levels_follow_the_css_order() {
    let dom = Dom::parse(b"<p id='b'>x</p>");

    // 普通声明：作者盖读者。
    let mut normal = RuleSet::parse("p { color: black }", Origin::Reader);
    normal.extend(RuleSet::parse("p { color: red }", Origin::Author));
    assert_eq!(winner(&dom, &normal, "color"), "red");

    // 加了 !important 反过来：读者盖作者。
    let mut flipped = RuleSet::parse("p { color: green !important }", Origin::Reader);
    flipped.extend(RuleSet::parse("p { color: red }", Origin::Author));
    assert_eq!(winner(&dom, &flipped, "color"), "green");

    // 两边都 !important 时仍是读者赢 —— CSS Cascade 从低到高是
    // UA normal < user normal < author normal < author !important < user !important。
    let mut both = RuleSet::parse("p { color: green !important }", Origin::Reader);
    both.extend(RuleSet::parse(
        "p { color: red !important }",
        Origin::Author,
    ));
    assert_eq!(winner(&dom, &both, "color"), "green");
}

#[test]
fn important_is_ordered_per_declaration_not_per_rule() {
    // 一条规则里混了重要与普通声明：按规则聚合排序会把两条一起抬到
    // AuthorImportant 级，让普通的 font-size 错误地盖掉后面的作者规则。
    let dom = Dom::parse(b"<p>x</p>");
    let mut set = RuleSet::parse(
        "p { color: red !important; font-size: 99px }",
        Origin::Author,
    );
    set.extend(RuleSet::parse("p { font-size: 20px }", Origin::Author));
    assert_eq!(
        winner(&dom, &set, "font-size"),
        "20px",
        "普通声明不该被同规则里的 !important 抬级"
    );
    assert_eq!(winner(&dom, &set, "color"), "red");
}

#[test]
fn higher_specificity_wins_when_both_are_author() {
    let dom = Dom::parse(b"<p class='a' id='b'>x</p>");
    let set = rules("p { color: one } .a { color: two } #b { color: three }");
    assert_eq!(winner(&dom, &set, "color"), "three");
}

#[test]
fn equal_specificity_later_rule_wins() {
    let dom = Dom::parse(b"<p>x</p>");
    let set = rules("p { color: one } p { color: two }");
    assert_eq!(winner(&dom, &set, "color"), "two");
}

/// 写在章里的 `<style>` 也要能变成规则表：样式表**文件**按位置清单回容器取，而这一份
/// 随正文一起到达 —— 作者把样式写在哪一份里，层叠结果都不该有差别（EasyPub 那一族工具
/// 的封面页就是这么写的）。排版那一侧与量 CSS 缺口的工具读的都是这一个出口。
///
/// 顺带钉住这条路上的 `url(...)` 折算：基准是**这一章自己的目录**，与元素上那句
/// `style="…"` 同一个数。漏折的表现与漏接 `background-image` 一模一样 —— 图片索引按
/// 容器路径精确查，查不到就白底，而屏上看不出「是没接住」还是「折错了层」。
#[test]
fn a_chapters_own_style_block_becomes_author_rules() {
    let source = "<html><head><style>p { color: #C0392B }\n\
                      body { background: url(../Images/back.jpg) no-repeat center } }</style></head>\
                      <body><p>字</p></body></html>";
    let dom = Dom::parse(source.as_bytes());
    let base_dir = dir_of("OEBPS/Text/v2.xhtml");
    let rules = chapter_style_rules(&dom, &base_dir);
    assert_eq!(rules.rules.len(), 2, "章里就两条规则：{:?}", rules.rules);

    let styles = cascade(&dom, &rules, &ComputedStyle::default(), &base_dir);
    let p = first_p(&dom);
    assert_eq!(
        styles.get(p).unwrap().color,
        Some(Color(0xC0, 0x39, 0x2B)),
        "章内 <style> 的颜色要落到 p 上"
    );
    let backdrop = styles
        .iter()
        .find_map(|(_, style)| style.background_pictures().next())
        .expect("章内 <style> 的那张底图该折出一层");
    assert_eq!(
        backdrop.image.as_deref(),
        Some("OEBPS/Images/back.jpg"),
        "按这一章的目录折；少一层就是「有样式、没底色」"
    );
}

/// 章内 `<style>` 与外链 `<link>` 在同一份文档里**交错**出现时，收进规则表的次序
/// 必须是文档序。先把所有 `<style>` 收完再接所有外链，等于把 specificity 打平时
/// 那一刀反着落 —— 这条钉住 `format::html` 那一族逐元素收的写法。
#[test]
fn style_text_of_returns_only_the_style_tags_text_in_document_order() {
    let dom = Dom::parse(
        b"<html><head><style>p { color: red }</style><link rel=stylesheet href=a.css>\
           <style>h1 { color: blue }</style></head><body><p>x</p></body></html>",
    );
    let mut collected = Vec::new();
    for (id, _) in dom.elements() {
        if let Some(text) = style_text_of(&dom, id) {
            collected.push(text.trim().to_string());
        }
    }
    assert_eq!(
        collected,
        vec![
            "p { color: red }".to_string(),
            "h1 { color: blue }".to_string()
        ],
        "两块 <style> 按文档序各给一条，`<link>` 不从这里走"
    );
    assert_eq!(
        style_tag_text(&dom).trim(),
        "p { color: red }\nh1 { color: blue }",
        "整份拼接的就是这两块"
    );
}

/// `@media` 只认「这条查询说的是不是纸」：`print` 独有的块整块丢掉（纸质的页眉页脚与
/// 分页收进来就是错），含 `screen` / `all`、以及判不了的条件照收。
#[test]
fn print_only_media_blocks_are_dropped_while_screen_ones_are_kept() {
    let color_of = |css: &str| -> Option<Color> {
        let dom = Dom::parse("<p>x</p>".as_bytes());
        let styled = cascade(&dom, &rules(css), &ComputedStyle::default(), "");
        styled.get(first_p(&dom)).and_then(|s| s.color)
    };
    assert_eq!(
        color_of("@media print { p { color: #ff0000 } } p { color: #123456 }"),
        Some(Color(0x12, 0x34, 0x56)),
        "print 里那条不许生效"
    );
    assert_eq!(
        color_of("@media screen and (min-width: 100px) { p { color: #111111 } }"),
        Some(Color(0x11, 0x11, 0x11)),
        "screen / 判不了的条件照收"
    );
    assert_eq!(
        color_of("@media all and (max-width: 600px) { p { color: #222222 } }"),
        Some(Color(0x22, 0x22, 0x22)),
        "含 all 的照收"
    );
}
