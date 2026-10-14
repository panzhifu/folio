//! 选择器：子集范围、匹配、组合器与 specificity。只依赖公开 API。

use crate::content::dom::{Dom, Node, NodeId};
use crate::content::format::css::selector::{PseudoElement, Selector};

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

/// 属性选择器的五种算子 + 大小写标志。出版书里的真写法是 `~=` 与 `*=`；
/// `|=`（`en` 命中 `en-US`）、`^=`、`$=` 一并接住，免得它们静默命中 0 个元素。
#[test]
fn attribute_selector_operators() {
    let dom = parse_dom(
        r#"<p class="a solid b" data-lang="en-US" title="Introduction">x</p>
           <p class="biaoti-1" data-lang="fr">y</p>"#,
    );
    let ps = of(&dom, "p");
    let (first, second) = (ps[0], ps[1]);
    let hit = |s: &str, node: NodeId| sel(s).matches(&dom, node);

    // 词包含：整词才命中。
    assert!(hit(r#"[class~="solid"]"#, first));
    assert!(!hit(r#"[class~="sol"]"#, first), "~= 要整词，不是子串");
    // 破折号匹配：`en` 命中 `en-US`，但不命中 `enx`。
    assert!(hit(r#"[data-lang|=en]"#, first));
    assert!(!hit(r#"[data-lang|=e]"#, first));
    // 前缀 / 后缀 / 子串。
    assert!(hit(r#"[class^="a "]"#, first));
    assert!(hit(r#"[class$="b"]"#, first));
    assert!(hit(r#"[title*="duct"]"#, first));
    // 大小写标志 `i`。
    assert!(hit(r#"[title*="INTRODUCT" i]"#, first));
    assert!(
        !hit(r#"[title*="INTRODUCT"]"#, first),
        "没有 i 就要区分大小写"
    );
    // 子串算子对空值一律不命中。
    assert!(!hit(r#"[title^=""]"#, first));
    assert!(hit(r#"[class*="biaoti"]"#, second));
    assert!(!hit(r#"[class*="biaoti"]"#, first));
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
        ":nth-child(2n)",
        ":nth-child(2n+1)",
        ":nth-child(odd)",
        ":nth-child(even)",
        ":nth-child(3)",
        ":nth-child(-n+2)",
        ":nth-child(+ 2 n - 1)",
        "p::first-letter",
        "body > p.intro::first-letter",
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
        "a:not(b)",
        "div span::before",
        "p > ",
        "> p",
        "p >> q",
        "",
        "p:has(span)",
        "div ~ ",
        // `:nth-child()` 的清单只到 `An+B`：`of S` 要的是「先过滤兄弟再数」，
        // `nth-last-child` / `nth-of-type` 根本不在承诺范围内。
        ":nth-child(2n of .x)",
        ":nth-child()",
        ":nth-child(2n3)",
        ":nth-child(nn)",
        ":nth-last-child(2)",
        ":nth-of-type(2)",
        // 伪元素：只接 `::first-letter`，其余构造要造出不存在的盒。
        // 单冒号的遗留写法也不接 —— 接了就得给每个伪类名再判一次「它是不是伪元素」。
        "p:first-letter",
        "p::before",
        "p::first-letter em",
        "::first-letter",
    ] {
        assert!(Selector::parse(text).is_none(), "不该接受 {text:?}");
    }
}

/// `An+B` 里 `n` 只能取非负整数，所以 `-n+3` 是「前 3 个」、`2n` 是「偶数」。
#[test]
fn nth_child_counts_element_siblings_from_one() {
    let dom = parse_dom("<p>一</p><span>中</span><p>二</p><p>三</p><p>四</p>");
    let ps = of(&dom, "p");
    assert!(sel("p:nth-child(1)").matches(&dom, ps[0]));
    assert!(!sel("p:nth-child(1)").matches(&dom, ps[1]));
    // 第三个 p 在兄弟里排第 4（span 占了一位），文本节点不占。
    assert!(sel("p:nth-child(4)").matches(&dom, ps[2]));
    assert!(
        sel("p:nth-child(even)").matches(&dom, ps[2]),
        "第 4 个是偶数"
    );
    assert!(!sel("p:nth-child(odd)").matches(&dom, ps[2]));
    assert!(sel("p:nth-child(2n+1)").matches(&dom, ps[3]), "第 5 个");
    // `-n+3` = 前三个；第 4、5 个不再命中。
    assert!(sel("p:nth-child(-n+3)").matches(&dom, ps[1]));
    assert!(!sel("p:nth-child(-n+3)").matches(&dom, ps[3]));
    // `0` 与负数命中不到任何元素，因为 n 非负、位置从 1 起。
    assert!(!sel("p:nth-child(0)").matches(&dom, ps[0]));
    assert!(!sel("p:nth-child(-2)").matches(&dom, ps[0]));
}

/// `:nth-child()` 是个伪类，specificity 与 `:first-child` 同级 ——
/// 写成 id 级会让它盖掉真正该赢的 `#id` 规则。
#[test]
fn nth_child_specificity_is_one_pseudo_class() {
    assert_eq!(
        sel("p:nth-child(2n+1)").specificity(),
        sel("p:first-child").specificity()
    );
    assert!(sel("#a").specificity() > sel("p:nth-child(2)").specificity());
    assert!(sel("p:nth-child(2)").specificity() > sel("p").specificity());
}

/// 首字符的声明属于**另一个盒**：`matches` 认它不出来，只有 `matches_pseudo` 认。
/// 两者混为一谈的话，`p::first-letter { color: red }` 会把整段正文都涂红。
#[test]
fn first_letter_declarations_have_a_separate_target() {
    let dom = parse_dom("<p>文</p><h1>题</h1>");
    let p = find(&dom, "p");
    let h1 = find(&dom, "h1");

    let cap = sel("p::first-letter");
    assert!(!cap.matches(&dom, p), "元素自己不该拿到首字符的声明");
    assert!(cap.matches_pseudo(&dom, p, Some(PseudoElement::FirstLetter)));

    let plain = sel("p");
    assert!(plain.matches_pseudo(&dom, p, None));
    assert!(!plain.matches_pseudo(&dom, p, Some(PseudoElement::FirstLetter)));

    // 分组里两条各归各的落点。
    let mixed = sel("p::first-letter, h1");
    assert!(mixed.matches(&dom, h1), "h1 那条属于 h1 自己");
    assert!(!mixed.matches(&dom, p));
    assert!(mixed.matches_pseudo(&dom, p, Some(PseudoElement::FirstLetter)));
}

/// 伪元素按一个 type 计 specificity：高于裸 `p`，与 `p em` 同级。
#[test]
fn pseudo_element_specificity_counts_as_one_type() {
    assert!(sel("p::first-letter").specificity() > sel("p").specificity());
    assert_eq!(
        sel("p::first-letter").specificity(),
        sel("p em").specificity(),
        "两个 type 级的东西该同级"
    );
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
