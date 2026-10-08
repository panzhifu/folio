//! CSS 子集：规则装配、层叠与 specificity。只依赖公开 API。

use folio_core::css::{Origin, RuleSet};
use folio_core::dom::{Dom, NodeId};

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
    assert_eq!(set.ignored, 2, "@font-face 与 @page 应计为忽略");
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
