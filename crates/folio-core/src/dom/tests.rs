use super::*;

fn parse(s: &str) -> Dom {
    Dom::parse(s.as_bytes())
}

/// `LocalName` 是 interned atom，不能直接和 `&str` 比较；测试里统一取字符串。
fn tag_of(node: &Node) -> String {
    node.tag.as_ref().map(|t| t.local.to_string()).unwrap_or_default()
}

fn first_element<'a>(dom: &'a Dom, tag: &str) -> Option<(NodeId, &'a Node)> {
    dom.elements()
        .find(|(_, n)| tag_of(n) == tag)
        .map(|(id, _)| (id, dom.get(id)))
}

#[test]
fn parses_a_simple_chapter() {
    let dom = parse("<html><body><h1>第一章</h1><p>正文一。</p></body></html>");
    let (h1_id, _) = first_element(&dom, "h1").expect("应有 h1");
    assert_eq!(dom.text_of_subtree(h1_id), "第一章");
    let (p_id, _) = first_element(&dom, "p").expect("应有 p");
    assert_eq!(dom.text_of_subtree(p_id), "正文一。");
    assert_eq!(dom.get(p_id).kind, NodeKind::Element);
}

#[test]
fn unclosed_tags_are_recovered_like_a_browser() {
    // 真实 EPUB 里最常见的畸形：段落不闭合。
    let dom = parse("<p>甲<p>乙<p>丙");
    let ps: Vec<_> = dom
        .elements()
        .filter(|(_, n)| tag_of(n) == "p")
        .collect();
    assert_eq!(ps.len(), 3, "三个未闭合的 <p> 应各自成节点");
    assert_eq!(dom.text_of_subtree(ps[1].0), "乙");
}

#[test]
fn bom_and_crlf_do_not_break_parsing() {
    let dom = parse("\u{feff}<p>甲\r\n乙</p>");
    let (p, _) = first_element(&dom, "p").expect("带 BOM 与 CRLF 的文档应能解析");
    let text = dom.text_of_subtree(p);
    assert!(text.contains('甲') && text.contains('乙'), "got {text:?}");
}

#[test]
fn entities_are_decoded_into_the_text_nodes() {
    let dom = parse("<p>甲&amp;乙 &lt;tag&gt;</p>");
    let (p, _) = first_element(&dom, "p").unwrap();
    assert_eq!(dom.text_of_subtree(p), "甲&乙 <tag>");
}

#[test]
fn element_paths_are_stable_across_reparsing() {
    // 这是锚点能跨「重新解析」成立的前提：同样的字节必须得同样的路径。
    let src = "<div><p>一</p><p>二</p><span>三</span></div>";
    let a = parse(src);
    let b = parse(src);
    let paths_a: Vec<String> = a
        .elements()
        .map(|(_, n)| n.path.clone())
        .filter(|p| !p.is_empty())
        .collect();
    let paths_b: Vec<String> = b
        .elements()
        .map(|(_, n)| n.path.clone())
        .filter(|p| !p.is_empty())
        .collect();
    assert_eq!(paths_a, paths_b);
    assert!(!paths_a.is_empty(), "路径不应全为空");
}

#[test]
fn same_tag_siblings_get_distinct_paths() {
    let dom = parse("<div><p>一</p><p>二</p></div>");
    let ps: Vec<String> = dom
        .elements()
        .filter(|(_, n)| tag_of(n) == "p")
        .map(|(_, n)| n.path.clone())
        .collect();
    assert_eq!(ps.len(), 2);
    assert_ne!(ps[0], ps[1], "同标签兄弟节点的路径必须可区分：{ps:?}");
    assert!(ps[0].ends_with("[1]") && ps[1].ends_with("[2]"), "got {ps:?}");
}

#[test]
fn tag_and_attribute_names_are_lowercased() {
    let dom = parse("<P CLASS=\"intro\" Id=\"x1\">文</P>");
    let (id, node) = first_element(&dom, "p").expect("大写标签应被规范化");
    assert!(matches!(node.tag.as_ref().map(|t| t.local.as_ref()), Some("p")));
    assert_eq!(dom.attr(id, "class"), Some("intro"));
    assert_eq!(dom.id_attr(id), Some("x1"));
}

#[test]
fn comments_are_not_body_text() {
    let dom = parse("<p>甲<!-- 别把我算进正文 -->乙</p>");
    let (p, _) = first_element(&dom, "p").unwrap();
    let text = dom.text_of_subtree(p);
    assert_eq!(text, "甲乙", "注释内容不应混进正文，got {text:?}");
}

#[test]
fn foreign_namespaces_are_preserved() {
    let dom = parse("<body><svg viewBox=\"0 0 1 1\"><rect/></svg></body>");
    let (svg_id, svg) = first_element(&dom, "svg").expect("svg 元素应存在");
    assert_ne!(
        svg.tag.as_ref().unwrap().ns,
        html5ever::ns!(html),
        "svg 不应被当成 HTML 命名空间"
    );
    assert!(
        dom.elements()
            .any(|(_, n)| tag_of(n) == "rect"),
        "svg 子树应被解析"
    );
    let _ = svg_id;
}

#[test]
fn style_and_script_content_is_not_body_text() {
    let dom = parse(
        "<head><style>p{color:red}</style></head><body><p>正文</p><script>var a=1</script></body>",
    );
    let (body_id, _) = first_element(&dom, "body").unwrap();
    let text = dom.text_of_subtree(body_id);
    assert!(text.contains("正文"));
    assert!(!text.contains("color:red"), "CSS 不该进正文：{text:?}");
    assert!(!text.contains("var a"), "JS 不该进正文：{text:?}");
}

#[test]
fn parent_and_child_links_agree() {
    let dom = parse("<div><section><p>文</p></section></div>");
    for (id, node) in dom.elements() {
        for child in &node.children {
            assert_eq!(
                dom.get(*child).parent,
                Some(id),
                "父子互指必须一致（子节点 {}）",
                dom.get(*child).path
            );
        }
    }
}

#[test]
fn dom_is_send_so_layout_can_run_off_the_main_thread() {
    fn assert_send<T: Send>() {}
    assert_send::<Dom>();
}

#[test]
fn malformed_nesting_does_not_panic() {
    // 只要求不 panic、且仍能取到正文：闭合顺序错乱在真书里很常见。
    for src in [
        "<b><i>甲</b></i>",
        "<table><td>一<td>二",
        "<ul><li>a<li>b</ul>",
        "<<>>",
        "<p title=未加引号>文</p>",
        "</div>裸文本",
        "<a href=x><a href=y>嵌套链接",
    ] {
        let dom = parse(src);
        assert!(!dom.nodes.is_empty(), "{src:?} 应至少产出 document");
    }
}
