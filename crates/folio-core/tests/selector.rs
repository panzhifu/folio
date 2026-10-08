//! 选择器：子集范围、匹配、组合器与 specificity。只依赖公开 API。

use folio_core::css::selector::Selector;
use folio_core::dom::{Dom, Node, NodeId};

fn parse_dom(html: &str) -> Dom {
    let wrapped = if html.contains("<html") || html.contains("<body") {
        html.to_string()
    } else {
        format!("<html><body>{html}</body></html>")
    };
    Dom::parse(wrapped.as_bytes())
}

fn tag_of(node: &Node) -> String {
    node.tag
        .as_ref()
        .map(|t| t.local.to_string())
        .unwrap_or_default()
}

fn find(dom: &Dom, tag: &str) -> NodeId {
    dom.elements()
        .find(|(_, n)| tag_of(n) == tag)
        .map(|(id, _)| id)
        .expect("应找到该元素")
}

fn of(dom: &Dom, tag: &str) -> Vec<NodeId> {
    dom.elements()
        .filter(|(_, n)| tag_of(n) == tag)
        .map(|(id, _)| id)
        .collect()
}

fn sel(text: &str) -> Selector {
    Selector::parse(text).unwrap_or_else(|| panic!("选择器应能解析：{text}"))
}

#[test]
fn parses_the_supported_forms() {
    for text in [
        "*",
        "p",
        ".intro",
        "#ch1",
        "p.intro",
        "p.intro#first",
        "[lang]",
        "[lang=zh]",
        "p[lang=\"zh\"]",
        "div p",
        "div > p",
        "h1 + p",
        "h1 ~ p",
        "body p .x",
        ":first-child",
        "p:first-child",
        ":lang(zh)",
        "a, b > c",
    ] {
        assert!(Selector::parse(text).is_some(), "应能解析 {text:?}");
    }
}

#[test]
fn rejects_constructs_outside_the_subset() {
    // 清单外的构造必须整条作废，不能部分生效。
    for text in [
        "p:hover",
        "div::first-line",
        "p:nth-child(2n)",
        "a:not(b)",
        "div span::before",
        "p > ",
        "> p",
        "p >> q",
        "",
        "p:has(span)",
        "div ~ ",
    ] {
        assert!(Selector::parse(text).is_none(), "不该接受 {text:?}");
    }
}

#[test]
fn matches_by_tag_class_and_id() {
    let dom = parse_dom(r#"<p class="a b" id="x1">文</p>"#);
    let p = find(&dom, "p");
    assert!(sel("p").matches(&dom, p));
    assert!(sel(".a").matches(&dom, p));
    assert!(sel(".b").matches(&dom, p));
    assert!(sel("#x1").matches(&dom, p));
    assert!(sel("p.a#x1").matches(&dom, p));
    assert!(!sel("div").matches(&dom, p));
    assert!(!sel(".c").matches(&dom, p));
    assert!(!sel("p#other").matches(&dom, p));
}

#[test]
fn matches_attributes() {
    let dom = parse_dom(r#"<p lang="zh" data-x="">文</p>"#);
    let p = find(&dom, "p");
    assert!(sel("[lang]").matches(&dom, p));
    assert!(sel("[lang=zh]").matches(&dom, p));
    assert!(sel(r#"[lang="zh"]"#).matches(&dom, p));
    assert!(!sel("[lang=en]").matches(&dom, p));
    assert!(sel("[data-x]").matches(&dom, p), "空值也算存在该属性");
    assert!(!sel("[missing]").matches(&dom, p));
}

#[test]
fn descendant_and_child_combinators_differ() {
    let dom = parse_dom("<div><section><p>文</p></section></div>");
    let p = find(&dom, "p");
    assert!(sel("div p").matches(&dom, p));
    assert!(sel("section p").matches(&dom, p));
    assert!(sel("div section p").matches(&dom, p));
    assert!(!sel("div > p").matches(&dom, p), "p 的直接父是 section");
    assert!(sel("section > p").matches(&dom, p));
    assert!(!sel("p div").matches(&dom, p), "方向不能反");
}

#[test]
fn sibling_combinators() {
    let dom = parse_dom("<h1>题</h1><p>一</p><p>二</p>");
    let ps = of(&dom, "p");
    let h1s = of(&dom, "h1");
    let first = ps[0];
    let second = ps[1];
    assert_eq!(dom.prev_element_sibling(first), h1s.first().copied());
    assert_ne!(dom.prev_element_sibling(second), h1s.first().copied());
    assert!(sel("h1 + p").matches(&dom, first));
    assert!(!sel("h1 + p").matches(&dom, second), "+ 只认紧邻的前一个");
    assert!(sel("h1 ~ p").matches(&dom, second));
}

#[test]
fn first_and_last_child() {
    let dom = parse_dom("<div><p>一</p><span>中</span><p>末</p></div>");
    let ps = of(&dom, "p");
    assert!(sel("p:first-child").matches(&dom, ps[0]));
    assert!(!sel("p:first-child").matches(&dom, ps[1]));
    assert!(sel("p:last-child").matches(&dom, ps[1]));
    assert!(!sel("p:last-child").matches(&dom, ps[0]));
}

#[test]
fn lang_matches_with_and_without_region() {
    let dom = parse_dom(r#"<div lang="zh-CN"><p>一</p></div><p>外</p>"#);
    let inner = of(&dom, "p")[0];
    assert!(sel(":lang(zh)").matches(&dom, inner), "zh 应命中 zh-CN");
    assert!(sel(":lang(zh-CN)").matches(&dom, inner));
    assert!(!sel(":lang(en)").matches(&dom, inner));
}

#[test]
fn specificity_orders_id_above_class_above_type() {
    let (id_only, class_only, type_only, mixed) = (
        sel("#a").specificity(),
        sel(".b").specificity(),
        sel("p").specificity(),
        sel("p.b#c").specificity(),
    );
    assert!(id_only > class_only, "id 选择器应高于 class 选择器");
    assert!(class_only > type_only, ".class 应高于 type");
    assert!(mixed > id_only, "叠加应更高");
    assert_eq!(sel("*").specificity(), 0, "通用选择器不贡献 specificity");
}

#[test]
fn grouped_selector_takes_the_highest_specificity() {
    assert_eq!(sel("p, #a").specificity(), sel("#a").specificity());
}

#[test]
fn comma_splitting_ignores_commas_inside_brackets() {
    let s = sel(r#"p[title="a,b"], h1"#);
    assert_eq!(s.complexes.len(), 2, "引号里的逗号不应分组：{s:?}");
}

#[test]
fn text_nodes_are_not_matched_directly() {
    // 文本节点靠继承拿样式，不参与选择器命中。
    let dom = parse_dom("<p>文</p>");
    let text_id = dom.get(find(&dom, "p")).children[0];
    assert!(!sel("*").matches(&dom, text_id));
}
