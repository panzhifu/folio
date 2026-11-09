//! 一章的可见文本与其字符坐标表。
//!
//! 坐标系的定义：按文档序拼起来的文本节点串，跳过 `<style>` / `<script>` 这类
//! 不进正文的标签。锚点、搜索、进度共用这一把尺 —— 定版式那一族没有 DOM，
//! 改走 [`TextMap::from_visible_text`]。

use std::ops::Range;

use crate::css::ua;
use crate::dom::{Dom, NodeId, NodeKind};

/// `prefix` / `suffix` 各取多少字。24 个汉字在一章里几乎唯一，
/// 又不至于让存进去的 JSON 膨胀（一条批注多两行字）。
pub const CONTEXT_CHARS: usize = 24;

/// 一个文本节点在整章可见文本里的位置。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Span {
    /// `Dom` 里那条路径，重定位时按它找回同一个节点。
    path: String,
    /// 在 [`TextMap::text`] 里的**字符**区间。
    chars: Range<usize>,
}

/// 一章的可见文本与其坐标表。
#[derive(Debug, Clone, Default)]
pub struct TextMap {
    text: String,
    /// 按 `NodeId` 下标索引；非文本节点与被跳过的子树是 `None`。
    spans: Vec<Option<Span>>,
}

impl TextMap {
    /// 从 `body` 往下按文档序收集文本节点。
    ///
    /// 只收**没有**被 `ua::is_skipped_tag` 祖先的文本节点：`<head>` 里的 `<style>`
    /// 有一整段字符，混进坐标系就会让「第 N 个字」和子串搜索都指错地方。
    pub fn build(dom: &Dom, body: NodeId) -> Self {
        let mut text = String::new();
        let mut spans = vec![None; dom.nodes.len()];
        let mut chars = 0usize;
        for id in visible_text_nodes(dom, body) {
            let node = dom.get(id);
            let own = node.text.as_str();
            if own.is_empty() {
                // 空文本节点（解析器常造出来）给它一个零宽区间也值得记，
                // 但既然没有任何字符，指过来与指不存在的节点等价，跳过更干净。
                continue;
            }
            let start = chars;
            text.push_str(own);
            chars += own.chars().count();
            spans[id as usize] = Some(Span {
                path: node.path.clone(),
                chars: start..chars,
            });
        }
        Self { text, spans }
    }

    /// 从一段**已经是可见文本**的串建坐标系（定版式那一族：一页的字序）。
    ///
    /// 这条入口存在的理由是「坐标系」与「坐标系从哪儿来」是两件事。上面那条
    /// [`TextMap::build`] 的来源是 DOM 的文本节点，而一页 PDF / DjVu / AZW4 的字不住在
    /// DOM 里 —— 它们来自栅格引擎的文字层，一份扁平的字序加逐字的盒子
    /// （[`crate::PageText`]）。锚点要的那三样（可截取的串、可子串
    /// 匹配的文本、字符偏移）这一族全都有，缺的只是「没人给它建过表」。
    ///
    /// **口径与 `build` 那一条严格相同**：数的是字，不数断行，也不数标签。所以两侧
    /// 存进磁盘的 `start` / `end` / `exact` 是同一把尺，读回来的那一头不必问这本书是
    /// 哪一族。定版式一页里跨行那一道缝只在复制与展示时还给读者
    /// （[`PageText::slice`]），它不进坐标系 —— 排版那一侧的段落之间同样没有换行符
    /// （`<p>` 是一个文本节点，`</p><p>` 之间不掺字节），两族要是各数各的断行，
    /// 一条批注就会在「翻回原文」那一步对不上自己。
    ///
    /// 代价是**这一份没有 DOM 路径**：`spans` 是空的，于是 [`TextAnchor`] 的 `path`
    /// 恒为空串，[`relocate`] 的第二级（同一个文本节点内搜）对定版式永久不参与。
    /// 这不是偷工：那一级的全部依据是「结构位置不随排版动」，而一页的字由栅格引擎
    /// 直接交出，没有任何结构可指。剩下三级（原偏移、附近窗口、全页搜）都只做子串
    /// 匹配，对这一族照样成立。
    ///
    /// [`PageText::slice`]: crate::PageText::slice
    pub fn from_visible_text(text: String) -> Self {
        Self {
            text,
            spans: Vec::new(),
        }
    }

    /// 整章可见文本。搜索与截取都在它上面做。
    pub fn text(&self) -> &str {
        &self.text
    }

    /// 可见文本的字符数。锚点越界判断与进度百分比都用它。
    pub fn chars(&self) -> usize {
        self.text.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// 某个文本节点占的字符区间。
    pub fn node_chars(&self, node: NodeId) -> Option<Range<usize>> {
        self.spans
            .get(node as usize)
            .and_then(|s| s.as_ref())
            .map(|s| s.chars.clone())
    }

    /// 某个文本节点的路径。
    pub fn node_path(&self, node: NodeId) -> Option<&str> {
        self.spans
            .get(node as usize)
            .and_then(|s| s.as_ref())
            .map(|s| s.path.as_str())
    }

    /// 按 DOM 路径找那个文本节点占的字符区间（重定位的第二级用它）。
    pub fn chars_of_path(&self, path: &str) -> Option<Range<usize>> {
        self.spans
            .iter()
            .flatten()
            .find(|span| span.path == path)
            .map(|span| span.chars.clone())
    }

    /// 节点内的**字节**区间 → 全章的**字符**区间。
    ///
    /// `LineRun::orig` 给的是字节（`node.text` 里的 UTF-8 偏移），坐标系是字符，
    /// 这一层换算就在锚点侧做，不让每个调用方各数一遍 —— 数错的那一处会是中文。
    pub fn chars_of_bytes(
        &self,
        dom: &Dom,
        node: NodeId,
        bytes: Range<usize>,
    ) -> Option<Range<usize>> {
        let span = self.spans.get(node as usize).and_then(|s| s.as_ref())?;
        let own = dom.get(node).text.as_str();
        // 字节偏移夹回**字符边界**。调用方给的跨度绝大多数来自引擎的原样区间，
        // 但退化档位的仿射映射（`orig_of`）在折叠与原文边界结构不一致时可能
        // 落进多字节字符的中间 —— 真机（图文版《人类简史》，段里满是「（」）
        // 实测过直接 panic。这里宁可把那一格算进前一个字符，也不能崩掉整本书。
        let floor = |mut at: usize| -> usize {
            while at > 0 && !own.is_char_boundary(at) {
                at -= 1;
            }
            at
        };
        let start = floor(bytes.start.min(bytes.end).min(own.len()));
        let end = floor(bytes.end.min(own.len()));
        if start > end {
            return None;
        }
        let head = own[..start].chars().count();
        let tail = own[..end].chars().count();
        Some(span.chars.start + head..span.chars.start + tail)
    }

    /// 覆盖 `offset` 的那个文本节点。
    pub fn node_at(&self, offset: usize) -> Option<NodeId> {
        self.spans
            .iter()
            .position(|span| matches!(span, Some(s) if s.chars.contains(&offset)))
            .map(|index| index as NodeId)
    }

    /// 字符区间对应的文本。越界时夹到可用范围，不 panic —— 坐标来自磁盘上的旧数据。
    pub fn slice(&self, range: Range<usize>) -> String {
        let start = range.start.min(self.chars());
        let end = range.end.clamp(start, self.chars());
        self.text
            .chars()
            .skip(start)
            .take(end.saturating_sub(start))
            .collect()
    }

    /// `start` 前后各 [`CONTEXT_CHARS`] 字，用作重定位的上下文指纹。
    ///
    /// 公开是因为 [`TextAnchor`](crate::TextAnchor) 在另一个 crate 里，取指纹要同一把尺。
    pub fn context(&self, start: usize, end: usize) -> (String, String) {
        let prefix = self.slice(start.saturating_sub(CONTEXT_CHARS)..start);
        let suffix = self.slice(end..end + CONTEXT_CHARS);
        (prefix, suffix)
    }
}

/// 按文档序给出 `root` 子树里所有该进坐标系的文本节点。
///
/// 判定在**元素**上做而不是回头查祖先：遇到 `<style>` / `<script>` / `<head>` 这类
/// `ua::is_skipped_tag` 的标签就不下探，它的文本自然进不来。显式栈而不是递归，
/// 是为了不拿 DOM 深度赌栈大小 —— 一本表格套表格的老书能有几百层。
fn visible_text_nodes(dom: &Dom, root: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        let node = dom.get(id);
        match node.kind {
            NodeKind::Text => out.push(id),
            NodeKind::Element => {
                if let Some(tag) = dom.tag_name(id)
                    && ua::is_skipped_tag(tag)
                {
                    continue;
                }
                // 栈是后进先出，倒序压才拿得到文档序。
                stack.extend(node.children.iter().rev().copied());
            }
            // 文档根与 doctest 之类：往下走就是。
            _ => stack.extend(node.children.iter().rev().copied()),
        }
    }
    out
}

/// 与 [`TextMap::build`] 完全同一条遍历，但**只拼文本**：不建 `spans`、不克隆每个文本
/// 节点的 `path`。搜索一次要折一整本书的可见文本，而它一处坐标都不需要 —— 命中偏移
/// 是在同一份 `text` 上量的，与锚点那把尺仍然是同一个字符串。
///
/// 输出必须与 `TextMap::text()` 逐字节相同（两边同一套跳过规则、同一个文档序），
/// 否则「搜到的位置」与「跳过去选中的字」会错开。等价性由 `search` 那一侧的
/// `visible_text_only_matches_the_text_map` 钉着。
pub fn visible_text_only(dom: &Dom, root: NodeId) -> String {
    let mut out = String::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        let node = dom.get(id);
        match node.kind {
            NodeKind::Text => out.push_str(&node.text),
            NodeKind::Element => {
                if let Some(tag) = dom.tag_name(id)
                    && ua::is_skipped_tag(tag)
                {
                    continue;
                }
                stack.extend(node.children.iter().rev().copied());
            }
            _ => stack.extend(node.children.iter().rev().copied()),
        }
    }
    out
}
