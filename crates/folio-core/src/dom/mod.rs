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
use html5ever::{parse_document, ParseOpts};

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub ns: Namespace,
    pub local: LocalName,
}

impl Default for Tag {
    fn default() -> Self {
        Self {
            ns: Namespace::default(),
            local: LocalName::default(),
        }
    }
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
    pub fn parse(bytes: &[u8]) -> Dom {
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
        let last = self.state.borrow().nodes[parent as usize].children.last().copied();
        if let Some(last) = last
            && self.state.borrow().nodes[last as usize].kind == NodeKind::Text
        {
            self.state.borrow_mut().nodes[last as usize].text.push_str(&text);
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
    type ElemName<'a> = Tag where Self: 'a;

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
                .map(|a| (a.name.local.to_string().to_ascii_lowercase(), a.value.to_string()))
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
        let has_children = !self.state.borrow().nodes[*element as usize].children.is_empty();
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
mod tests;
