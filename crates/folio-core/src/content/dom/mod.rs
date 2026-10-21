//! 浏览器级 HTML5 解析出的 DOM，存成扁平 arena。
//!
//! 用 `html5ever` 而不是自己切标签：真实 EPUB 里的 XHTML 经常未闭合、带 BOM、
//! 混 CRLF，浏览器级解析器才有一致的纠错行为。
//!
//! 存 arena（`Vec<Node>` + `u32` 索引）而不是 `Rc<RefCell<>>` 树，有三个理由：
//! 可序列化（DOM 缓存进 SQLite 避免重复解析）、`Send`（布局能在后台线程跑）、
//! 以及遍历时不跟引用计数打架。

use std::borrow::Cow;
use std::cell::RefCell;

use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::tree_builder::{ElemName, ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::{Attribute, LocalName, Namespace, QualName};
use html5ever::{ParseOpts, parse_document};

pub type NodeId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Document,
    Element,
    Text,
    Comment,
}

/// 标签名。
///
/// `TreeSink::elem_name` 要返回一个能借出 `&Namespace` / `&LocalName` 的东西，
/// 但 sink 用 `RefCell` 做内部可变 —— 从 `Ref<'_, State>` 守卫里往外借引用编译不过。
/// 所以这里返回**拥有所有权**的 Tag：GAT `type ElemName<'a>` 并不要求它借用 `'a`。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Tag {
    pub ns: Namespace,
    pub local: LocalName,
}

impl ElemName for Tag {
    fn ns(&self) -> &Namespace {
        &self.ns
    }

    fn local_name(&self) -> &LocalName {
        &self.local
    }
}

#[derive(Debug)]
pub struct Node {
    pub kind: NodeKind,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub tag: Option<Tag>,
    /// 属性名一律小写（HTML 语义），值原样保留。
    pub attrs: Vec<(String, String)>,
    pub text: String,
    /// 从 document 数下来的结构路径，锚点靠它跨重新解析定位。
    pub path: String,
}

pub struct Dom {
    pub nodes: Vec<Node>,
    pub document: NodeId,
    pub quirks: QuirksMode,
    pub errors: Vec<String>,
}

impl Dom {
    /// 解析整篇文档。字节入口，交给 html5ever 的 UTF-8 宽松解码处理 BOM 与坏字节。
    ///
    /// 这里记一次 [`Metric::DomParses`](crate::infra::metrics::Metric::DomParses)：解析与读者偏好
    /// 无关，是章的**解析产物**，所以「改一次字号又重解析一遍」是能被数出来的浪费
    /// （`plans/阅读管线.md` §十二那条计数断言用的就是这一格）。
    pub fn parse(bytes: &[u8]) -> Dom {
        crate::infra::metrics::Metric::DomParses.count();
        let sink = DomSink::new();
        let opts = ParseOpts {
            tokenizer: Default::default(),
            tree_builder: Default::default(),
        };
        let mut dom = parse_document(sink, opts).from_utf8().one(bytes);
        dom.assign_paths();
        dom
    }

    pub fn get(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    /// 元素标签名（已小写）。非元素节点返回 `None`。
    /// UA 样式表和排版层都按这个名字查表，所以不能有分配。
    pub fn tag_name(&self, id: NodeId) -> Option<&str> {
        self.get(id).tag.as_ref().map(|t| t.local.as_ref())
    }

    pub fn is_element(&self, id: NodeId, tag: &str) -> bool {
        self.tag_name(id) == Some(tag)
    }

    pub fn elements(&self) -> impl Iterator<Item = (NodeId, &Node)> {
        self.nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.kind == NodeKind::Element)
            .map(|(i, n)| (i as NodeId, n))
    }

    /// 收集某元素子树下的全部文本，按文档顺序拼接。
    /// 这是锚点坐标里「节点内字符偏移」所依附的那段文本。
    ///
    /// 跳过 `script` / `style` / `head`：它们的文字不是可读内容，
    /// 混进来既会显示到页面上，也会让字符偏移锚点整体错位。
    pub fn text_of_subtree(&self, root: NodeId) -> String {
        let mut out = String::new();
        self.collect_text(root, &mut out);
        out
    }

    /// 同样是子树文本，但**不**跳过 `title` / `style` / `head` / `script`。
    ///
    /// 要的是「这些元素的字本身」那几处：书名取自 `<title>`、样式表取自 `<style>`、
    /// 书目块里的 `<dc:title>` 在 HTML 解析后与 `<title>` 同名同形（html5ever 把
    /// `<dc:Title>` 记成本地名 `dc:title`，而裸 `<title>` 恰好在同一份文档里是空的）。
    /// [`text_of_subtree`] 是为锚点坐标服务的，它必须跳；这一条不是。
    pub fn raw_text_of_subtree(&self, root: NodeId) -> String {
        let mut out = String::new();
        self.collect_raw_text(root, &mut out);
        out
    }

    fn collect_raw_text(&self, id: NodeId, out: &mut String) {
        let node = self.get(id);
        if node.kind == NodeKind::Text {
            out.push_str(&node.text);
        }
        for child in node.children.clone() {
            self.collect_raw_text(child, out);
        }
    }

    fn is_non_content_element(&self, node: &Node) -> bool {
        if node.kind != NodeKind::Element {
            return false;
        }
        matches!(
            node.tag.as_ref().map(|t| t.local.as_ref()),
            Some("script") | Some("style") | Some("head") | Some("title") | Some("template")
        )
    }
    fn collect_text(&self, id: NodeId, out: &mut String) {
        let node = self.get(id);
        if self.is_non_content_element(node) {
            return;
        }
        if node.kind == NodeKind::Text {
            out.push_str(&node.text);
        }
        for child in node.children.clone() {
            self.collect_text(child, out);
        }
    }

    pub fn attr<'a>(&'a self, id: NodeId, name: &str) -> Option<&'a str> {
        self.get(id)
            .attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn classes(&self, id: NodeId) -> impl Iterator<Item = &str> {
        self.attr(id, "class")
            .into_iter()
            .flat_map(|s| s.split_ascii_whitespace())
    }

    pub fn id_attr(&self, id: NodeId) -> Option<&str> {
        self.attr(id, "id")
    }

    /// 由近及远的祖先元素链（不含自身）。
    pub fn ancestors(&self, id: NodeId) -> impl Iterator<Item = NodeId> {
        let mut cur = self.get(id).parent;
        std::iter::from_fn(move || {
            let node = cur?;
            cur = self.get(node).parent;
            Some(node)
        })
    }

    pub fn ancestors_including_self(&self, id: NodeId) -> impl Iterator<Item = NodeId> {
        [id].into_iter().chain(self.ancestors(id))
    }

    pub fn element_children(&self, id: NodeId) -> impl Iterator<Item = NodeId> {
        self.get(id)
            .children
            .iter()
            .copied()
            .filter(|c| self.get(*c).kind == NodeKind::Element)
    }

    fn preceding_siblings(&self, id: NodeId) -> Vec<NodeId> {
        let Some(parent) = self.get(id).parent else {
            return Vec::new();
        };
        let siblings = &self.get(parent).children;
        match siblings.iter().position(|c| *c == id) {
            Some(at) => siblings[..at].iter().rev().copied().collect(),
            None => Vec::new(),
        }
    }

    pub fn prev_element_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.prev_element_siblings(id).next()
    }

    pub fn prev_element_siblings(&self, id: NodeId) -> impl Iterator<Item = NodeId> {
        self.preceding_siblings(id)
            .into_iter()
            .filter(move |c| self.get(*c).kind == NodeKind::Element)
    }

    /// 后序一遍把路径写好。解析期间不能算：`create_element` 返回句柄时
    /// 树构造器还没决定它的父节点是谁。
    fn assign_paths(&mut self) {
        let document = self.document;
        self.nodes[document as usize].path = String::new();
        let mut stack: Vec<(NodeId, String)> = vec![(document, String::new())];
        while let Some((id, path)) = stack.pop() {
            let children = self.nodes[id as usize].children.clone();
            let mut element_index = 0u32;
            for child in children {
                let kind = self.nodes[child as usize].kind;
                let child_path = if kind == NodeKind::Element {
                    let tag = &self.nodes[child as usize].tag;
                    let name = tag
                        .as_ref()
                        .map(|t| t.local.to_string())
                        .unwrap_or_else(|| "?".into());
                    element_index += 1;
                    format!("{path}/{name}[{element_index}]")
                } else {
                    format!("{path}#{}", self.nodes[child as usize].children.len())
                };
                self.nodes[child as usize].path = child_path.clone();
                stack.push((child, child_path));
            }
        }
    }
}

struct DomState {
    nodes: Vec<Node>,
    quirks: QuirksMode,
    errors: Vec<String>,
}

pub struct DomSink {
    state: RefCell<DomState>,
}

impl DomSink {
    fn new() -> Self {
        let nodes = vec![Node {
            kind: NodeKind::Document,
            parent: None,
            children: Vec::new(),
            tag: None,
            attrs: Vec::new(),
            text: String::new(),
            path: String::new(),
        }];
        Self {
            state: RefCell::new(DomState {
                nodes,
                quirks: QuirksMode::NoQuirks,
                errors: Vec::new(),
            }),
        }
    }

    fn push_node(&self, node: Node) -> NodeId {
        let mut state = self.state.borrow_mut();
        let id = state.nodes.len() as NodeId;
        state.nodes.push(node);
        id
    }

    fn link(&self, parent: NodeId, child: NodeId) {
        let mut state = self.state.borrow_mut();
        state.nodes[child as usize].parent = Some(parent);
        state.nodes[parent as usize].children.push(child);
    }

    /// 相邻文本节点合并。不合并的话锚点偏移会在任意一处换段时错位。
    fn append_text(&self, parent: NodeId, text: String) {
        let last = self.state.borrow().nodes[parent as usize]
            .children
            .last()
            .copied();
        if let Some(last) = last
            && self.state.borrow().nodes[last as usize].kind == NodeKind::Text
        {
            self.state.borrow_mut().nodes[last as usize]
                .text
                .push_str(&text);
            return;
        }
        let id = self.push_node(Node {
            kind: NodeKind::Text,
            parent: Some(parent),
            children: Vec::new(),
            tag: None,
            attrs: Vec::new(),
            text,
            path: String::new(),
        });
        self.link(parent, id);
    }
}

impl TreeSink for DomSink {
    type Handle = NodeId;
    type Output = Dom;
    type ElemName<'a>
        = Tag
    where
        Self: 'a;

    fn finish(self) -> Dom {
        let state = self.state.into_inner();
        Dom {
            nodes: state.nodes,
            document: 0,
            quirks: state.quirks,
            errors: state.errors,
        }
    }

    fn parse_error(&self, msg: Cow<'static, str>) {
        self.state.borrow_mut().errors.push(msg.into_owned());
    }

    fn get_document(&self) -> NodeId {
        0
    }

    fn elem_name(&self, target: &NodeId) -> Tag {
        self.state.borrow().nodes[*target as usize]
            .tag
            .clone()
            .unwrap_or_default()
    }

    fn create_element(
        &self,
        name: QualName,
        attrs: Vec<Attribute>,
        _flags: ElementFlags,
    ) -> NodeId {
        self.push_node(Node {
            kind: NodeKind::Element,
            parent: None,
            children: Vec::new(),
            tag: Some(Tag {
                ns: name.ns,
                local: name.local,
            }),
            attrs: attrs
                .into_iter()
                .map(|a| {
                    (
                        a.name.local.to_string().to_ascii_lowercase(),
                        a.value.to_string(),
                    )
                })
                .collect(),
            text: String::new(),
            path: String::new(),
        })
    }

    fn create_comment(&self, text: StrTendril) -> NodeId {
        self.push_node(Node {
            kind: NodeKind::Comment,
            parent: None,
            children: Vec::new(),
            tag: None,
            attrs: Vec::new(),
            text: text.to_string(),
            path: String::new(),
        })
    }

    fn create_pi(&self, target: StrTendril, data: StrTendril) -> NodeId {
        self.push_node(Node {
            kind: NodeKind::Comment,
            parent: None,
            children: Vec::new(),
            tag: None,
            attrs: Vec::new(),
            text: format!("?{target} {data}"),
            path: String::new(),
        })
    }

    fn append(&self, parent: &NodeId, child: NodeOrText<NodeId>) {
        match child {
            NodeOrText::AppendNode(id) => self.link(*parent, id),
            NodeOrText::AppendText(t) => self.append_text(*parent, t.to_string()),
        }
    }

    fn append_based_on_parent_node(
        &self,
        element: &NodeId,
        prev_element: &NodeId,
        child: NodeOrText<NodeId>,
    ) {
        let has_children = !self.state.borrow().nodes[*element as usize]
            .children
            .is_empty();
        let parent = if has_children {
            *element
        } else {
            *prev_element
        };
        self.append(&parent, child);
    }

    fn append_doctype_to_document(&self, _name: StrTendril, _p: StrTendril, _s: StrTendril) {}

    fn get_template_contents(&self, target: &NodeId) -> NodeId {
        *target
    }

    fn same_node(&self, x: &NodeId, y: &NodeId) -> bool {
        x == y
    }

    fn set_quirks_mode(&self, mode: QuirksMode) {
        self.state.borrow_mut().quirks = mode;
    }

    fn append_before_sibling(&self, sibling: &NodeId, new_node: NodeOrText<NodeId>) {
        let (parent, siblings) = {
            let state = self.state.borrow();
            let parent = match state.nodes[*sibling as usize].parent {
                Some(p) => p,
                None => return,
            };
            (parent, state.nodes[parent as usize].children.clone())
        };
        match new_node {
            NodeOrText::AppendNode(id) => {
                let mut state = self.state.borrow_mut();
                state.nodes[id as usize].parent = Some(parent);
                let list = &mut state.nodes[parent as usize].children;
                let at = list.iter().position(|c| c == sibling).unwrap_or(list.len());
                list.insert(at, id);
            }
            NodeOrText::AppendText(t) => self.append_text(parent, t.to_string()),
        }
        let _ = siblings;
    }

    fn add_attrs_if_missing(&self, target: &NodeId, attrs: Vec<Attribute>) {
        let mut state = self.state.borrow_mut();
        let node = &mut state.nodes[*target as usize];
        for attr in attrs {
            let key = attr.name.local.to_string().to_ascii_lowercase();
            if !node.attrs.iter().any(|(k, _)| *k == key) {
                node.attrs.push((key, attr.value.to_string()));
            }
        }
    }

    fn remove_from_parent(&self, target: &NodeId) {
        let parent = match self.state.borrow().nodes[*target as usize].parent {
            Some(p) => p,
            None => return,
        };
        let mut state = self.state.borrow_mut();
        state.nodes[parent as usize]
            .children
            .retain(|c| c != target);
        state.nodes[*target as usize].parent = None;
    }

    fn reparent_children(&self, node: &NodeId, new_parent: &NodeId) {
        let moved = {
            let mut state = self.state.borrow_mut();
            std::mem::take(&mut state.nodes[*node as usize].children)
        };
        for child in &moved {
            state_set_parent(&self.state, *child, *new_parent);
        }
        let mut state = self.state.borrow_mut();
        state.nodes[*new_parent as usize].children.extend(moved);
    }
}

fn state_set_parent(cell: &RefCell<DomState>, node: NodeId, parent: NodeId) {
    cell.borrow_mut().nodes[node as usize].parent = Some(parent);
}

#[cfg(test)]
mod tests {
    //! DOM 层：html5ever → arena 的解析结果与结构不变量。
    //!
    //! 只走公开 API —— 搬进 `src/` 之后编译器不再替我们把关「测试有没有依赖私有项」，
    //! 这条就靠约定守：这里的导入一律点名 `pub` 项，不用 `use super::*`。

    use crate::content::dom::{Dom, Node, NodeId, NodeKind};

    fn parse(s: &str) -> Dom {
        Dom::parse(s.as_bytes())
    }

    /// `LocalName` 是 interned atom，不能直接和 `&str` 比较；测试里统一取字符串。
    fn tag_of(node: &Node) -> String {
        node.tag
            .as_ref()
            .map(|t| t.local.to_string())
            .unwrap_or_default()
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
        let ps: Vec<_> = dom.elements().filter(|(_, n)| tag_of(n) == "p").collect();
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
        assert!(
            ps[0].ends_with("[1]") && ps[1].ends_with("[2]"),
            "got {ps:?}"
        );
    }

    #[test]
    fn tag_and_attribute_names_are_lowercased() {
        let dom = parse("<P CLASS=\"intro\" Id=\"x1\">文</P>");
        let (id, node) = first_element(&dom, "p").expect("大写标签应被规范化");
        assert!(matches!(
            node.tag.as_ref().map(|t| t.local.as_ref()),
            Some("p")
        ));
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
            dom.elements().any(|(_, n)| tag_of(n) == "rect"),
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
}
