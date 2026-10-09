//! 锚点（anchor）：把「第几个字」钉在一个**不随排版变化**的坐标系上。
//!
//! 为什么不能存行号或页码：改字号、改行距、缩窗口、换分页模式，行盒全部重排，
//! 只有「这一章的第 N 个字」不动。这就是 [`Locator`] 早就写在注释里的立场，
//! 本模块把它真正接通：既有**坐标的定义**（[`TextMap`]），也有**坐标 ↔ 行盒**的
//! 双向映射（[`line_offset`] / [`line_at_offset`]），还有**坐标失配时的找回办法**
//! （[`relocate`] 的四级降级）。
//!
//! ## 坐标系是「可见文本」，不是章节源文本
//!
//! 一章的 XHTML 里 `<style>`、`<script>`、标签名、属性都是字节，但不是字。
//! 锚点用的是 [`TextMap::build`] 按文档序拼起来的**文本节点**串，理由有两条：
//! - 它不受标签写法影响。坐标不数进 `<p>` 这种标记，也不数属性，重排或美化源码都不会移动它。
//! - 搜得到。重定位要在文本里做子串匹配，把 CSS 混进坐标系就会出现「在样式表里命中」。
//!
//! 要说清楚的是：**它不等于「读者数得出来的第 N 个字」**。pretty-print 过的 XHTML 里
//! 标签之间的换行与缩进也是文本节点，所以第一段正文的坐标通常是 1 而不是 0。
//! 这个数从来不是给人看的（界面上显示的是页码/百分比），它的唯一职责是**稳**。
//!
//! 代价是这个坐标**包含 `display:none` 块里的文本**（那些字在 DOM 里、不在屏幕上）。
//! 不在这里排除它是有意的：可见性要到排版期才算得出来，而锚点必须在排版之前也稳定 ——
//! 隐藏块显示/隐藏会让整章坐标平移，那比坐标里有几个不显示的字糟得多。
//!
//! [`Locator`]: crate::Locator

use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::css::ua;
use crate::dom::{Dom, NodeId, NodeKind};
use crate::layout::{Layout, LineFragment};

/// `prefix` / `suffix` 各取多少字。24 个汉字在一章里几乎唯一，
/// 又不至于让存进去的 JSON 膨胀（一条批注多两行字）。
pub const CONTEXT_CHARS: usize = 24;

/// 第三级降级在锚点原偏移附近搜多大窗口（字符）。
/// 400 字 ≈ 一页正文：排版参数怎么调，同一处内容都不会跑出这个距离；
/// 再大就会开始跟远处重复出现的短语抢命中。
const NEIGHBORHOOD: usize = 400;

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
        let start = bytes.start.min(bytes.end).min(own.len());
        let end = bytes.end.min(own.len());
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
    fn context(&self, start: usize, end: usize) -> (String, String) {
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

/// 一条锚点：坐标 + 用来找回坐标的文本指纹。
///
/// `exact` / `prefix` / `suffix` / `path` 都是**冗余**的（都能从 `start..end` 推出来），
/// 冗余正是它的用途：源文本变了（不同版本、重新导出、章节顺序动了）时，
/// 只有坐标就只知道「指错了」，而有指纹才能把它找回来。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TextAnchor {
    pub chapter: u32,
    /// 可见文本的字符偏移，含。
    pub start: usize,
    /// 不含。`start == end` 表示一个插入点（进度就是这个形状）。
    pub end: usize,
    /// 选中的原文。进度用不到它，批注与书签用它找回位置。
    pub exact: String,
    /// `exact` 之前的 `CONTEXT_CHARS` 字。
    pub prefix: String,
    /// `exact` 之后的 `CONTEXT_CHARS` 字。
    pub suffix: String,
    /// `start` 所在文本节点的 DOM 路径。
    pub path: String,
}

impl TextAnchor {
    /// 从坐标建一条锚点，指纹由 `map` 现取。
    pub fn new(map: &TextMap, chapter: u32, start: usize, end: usize) -> Self {
        let end = end.max(start);
        let (prefix, suffix) = map.context(start, end);
        Self {
            chapter,
            start,
            end,
            exact: map.slice(start..end),
            prefix,
            suffix,
            path: map
                .node_at(start)
                .and_then(|node| map.node_path(node))
                .unwrap_or("")
                .to_string(),
        }
    }

    /// 只有坐标、没有指纹的那一种（进度）：`Locator` 的一个字符偏移就够。
    pub fn point(chapter: u32, offset: usize) -> Self {
        Self {
            chapter,
            start: offset,
            end: offset,
            ..Default::default()
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    /// 读不回来的 JSON 一律当「没有锚点」，不是 panic 也不是半份数据。
    /// 数据库里这一列来自旧版本或别的机器，坏一条不该让整本书的标注列表打不开。
    pub fn from_json(json: &str) -> Option<TextAnchor> {
        serde_json::from_str(json).ok()
    }
}

/// 重定位的结果：找回了坐标，以及**是哪一级找回的**。
///
/// `how` 不是装饰：界面要能区分「精确」与「大概在这里」，
/// 而 `Stale` 是显式状态 —— 宁可标一条「找不到」的批注，也不把它画到别处去。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// 原偏移上就是那段文字。
    Exact,
    /// 在同一个 DOM 节点里搜到了。
    SameNode,
    /// 在原偏移附近的窗口里搜到了。
    Nearby,
    /// 全章搜到了（重复短语时可能选错，界面上该给个提示）。
    Anywhere,
    /// 找不回来。
    Stale,
}

/// 降级顺序是**从便宜到昂贵**，每一级都只在上一级失败后才试：
/// 偏移还对 → 同节点内搜 → 附近窗口内搜 → 全章搜 → 放弃。
/// 每级都用同一份指纹（`exact`，退到 `prefix+suffix` 拼接），不做「猜一个邻近位置」
/// 这种事 —— 那条会静默把批注画到别的句子上。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relocated {
    pub start: usize,
    pub end: usize,
    pub confidence: Confidence,
}

impl Relocated {
    fn new(start: usize, end: usize, confidence: Confidence) -> Self {
        Self {
            start,
            end,
            confidence,
        }
    }
}

/// 把一条锚点落回当前的可见文本。
pub fn relocate(anchor: &TextAnchor, map: &TextMap) -> Relocated {
    let total = map.chars();
    if total == 0 || anchor.exact.is_empty() {
        return Relocated::new(0, 0, Confidence::Stale);
    }
    let width = anchor.exact.chars().count();

    // ① 偏移仍然对得上。
    if anchor.end <= total && map.slice(anchor.start..anchor.end) == anchor.exact {
        return Relocated::new(anchor.start, anchor.end, Confidence::Exact);
    }

    // ② 同一个 DOM 节点里搜。路径是结构性的，改字号改宽度都不动它，
    //    而书重排章节时它会变 —— 这一级正好夹在两者之间。
    if !anchor.path.is_empty()
        && let Some(hit) = search_in_path(map, anchor, width)
    {
        return hit;
    }

    // ③ 原偏移附近的窗口。排版参数怎么调，同一处内容也跑不出几百字。
    let window = anchor.start.saturating_sub(NEIGHBORHOOD)..anchor.start + NEIGHBORHOOD;
    if let Some(hit) = search_in_window(map, anchor, window, width) {
        return hit;
    }

    // ④ 全章。命中多处时取第一处，并把置信度留给界面。
    if let Some(hit) = search_in_window(map, anchor, 0..total, width) {
        return Relocated::new(hit.start, hit.end, Confidence::Anywhere);
    }

    Relocated::new(
        anchor.start.min(total),
        anchor.end.min(total),
        Confidence::Stale,
    )
}

/// 在 `anchor.path` 指向的节点里搜。
fn search_in_path(map: &TextMap, anchor: &TextAnchor, width: usize) -> Option<Relocated> {
    let span = map.spans.iter().flatten().find(|s| s.path == anchor.path)?;
    search_in_window(map, anchor, span.chars.clone(), width)
        .map(|hit| Relocated::new(hit.start, hit.end, Confidence::SameNode))
}

/// 在 `[window]` 里搜 `exact`；搜不到再试 `prefix + suffix` 拼出来的那段
/// （中间隔开的部分允许不同，专门对付「插入了一段批注文字」这类改动）。
fn search_in_window(
    map: &TextMap,
    anchor: &TextAnchor,
    window: Range<usize>,
    width: usize,
) -> Option<Relocated> {
    let total = map.chars();
    let from = window.start.min(total);
    let to = window.end.clamp(from, total);
    let hay = map.slice(from..to);
    if let Some(at) = find_char_offset(&hay, &anchor.exact) {
        let start = from + at;
        return Some(Relocated::new(
            start,
            clamp_char(map, start + width),
            Confidence::Nearby,
        ));
    }
    if anchor.prefix.is_empty() || anchor.suffix.is_empty() {
        return None;
    }
    let joined = format!("{}{}", anchor.prefix, anchor.suffix);
    let at = find_char_offset(&hay, &joined)?;
    // 命中的是「前文 + 后文」，锚点正文就夹在它们的接缝上。
    let start = from + at + anchor.prefix.chars().count();
    let end = clamp_char(map, start + width);
    // **必须回头验一次**接缝上的文字确实是 `exact`。这条不是形式主义：
    // 「前后文相连」在中文里太容易在别处凑出来（一本用同一套字反复的书尤甚），
    // 不做这一步就会把批注画到一段毫无关系的填充文字上，而且降级出来的置信度
    // 还写着「附近找到了」。宁可判 stale，让界面标「找不到」。
    if map.slice(start..end) != anchor.exact {
        return None;
    }
    Some(Relocated::new(start, end, Confidence::Nearby))
}

/// 字符偏移夹到可见文本末尾。命中贴在章尾时 `start + width` 会越界，
/// 越界的锚点存回磁盘后每一帧都在「找不到」上降级，那是自造的 stale。
fn clamp_char(map: &TextMap, offset: usize) -> usize {
    offset.min(map.chars())
}

/// 在 `hay` 里找 `needle`，返回**字符**偏移（不是字节）。
fn find_char_offset(hay: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let byte = hay.find(needle)?;
    Some(hay[..byte].chars().count())
}

/// 第 `line` 行覆盖的可见文本字符区间。
///
/// 一行的多个 run 若跨了文本节点（`<p>前<em>中</em>后</p>` 很常见），取包住它们的
/// 整个区间 —— 这一行的语义就是「这几段都在我这儿」。图片片段没有字符，跳过。
pub fn line_chars(layout: &Layout, dom: &Dom, map: &TextMap, line: usize) -> Option<Range<usize>> {
    let box_ = layout.lines.get(line)?;
    let mut out: Option<Range<usize>> = None;
    for fragment in &box_.fragments {
        let LineFragment::Text(run) = fragment else {
            continue;
        };
        let Some(range) = map.chars_of_bytes(dom, run.node, run.orig.clone()) else {
            continue;
        };
        if range.is_empty() {
            continue;
        }
        out = Some(match out {
            Some(cur) => cur.start.min(range.start)..cur.end.max(range.end),
            None => range,
        });
    }
    out
}

/// 行首字符的可见偏移。**进度存这一个数**：字号、行距、宽度、分页模式都不动它。
pub fn line_offset(layout: &Layout, dom: &Dom, map: &TextMap, line: usize) -> Option<usize> {
    line_chars(layout, dom, map, line).map(|range| range.start)
}

/// 覆盖 `offset` 的那一行；落在两行之间（图片或空白行）时给它前面最近的一行。
///
/// 恢复进度用的就是这个：把界面滚到这一行的行顶，读者看到的正是他上次看的那句。
pub fn line_at_offset(layout: &Layout, dom: &Dom, map: &TextMap, offset: usize) -> usize {
    let mut before = 0usize;
    for line in 0..layout.lines.len() {
        match line_chars(layout, dom, map, line) {
            Some(range) if range.contains(&offset) => return line,
            Some(range) if range.start <= offset => before = line,
            _ => {}
        }
    }
    before
}

/// 像素偏移 → 行号。连续滚动（移动）与虚拟列表（桌面）共用这一份，
/// 两边各数一次就会在「行盒高为 0 的空行」上给出不同答案。
pub fn line_at_y(layout: &Layout, y: f32) -> usize {
    let mut line = 0usize;
    for (ix, box_) in layout.lines.iter().enumerate() {
        if box_.y + box_.height > y {
            line = ix;
            break;
        }
        line = ix;
    }
    line
}

#[cfg(test)]
mod tests {
    //! 锚点坐标系与降级重定位。
    //!
    //! 这一份测试盯的是 M5 的验收句：「改 3 档字号 + 缩窗口 → 全部锚点命中或显式 stale」。
    //! 几何用与 `layout::tests` 同一套假度量（汉字 1 em、拉丁 0.5 em、空格 0.25 em），
    //! 所以行盒数量与断点都是推出来的，不是照实现回填的。

    use crate::anchor::{
        CONTEXT_CHARS, Confidence, Relocated, TextAnchor, TextMap, line_at_offset, line_at_y,
        line_chars, line_offset, relocate,
    };
    use crate::css::computed::{ComputedStyle, cascade};
    use crate::css::{Origin, RuleSet};
    use crate::dom::{Dom, NodeId};
    use crate::layout::{Layout, LayoutOptions, LineMetrics, Measure, NoImages, layout};

    struct Mono;

    fn char_width(ch: char, size: f32) -> f32 {
        if ch == ' ' {
            size * 0.25
        } else if (ch as u32) >= 0x2e80 {
            size
        } else {
            size * 0.5
        }
    }

    impl Measure for Mono {
        fn char_width(&self, ch: char, _font: &crate::layout::FontKey, font_size: f32) -> f32 {
            char_width(ch, font_size)
        }

        fn run_metrics(
            &self,
            text: &str,
            _font: &crate::layout::FontKey,
            font_size: f32,
        ) -> LineMetrics {
            LineMetrics {
                width: text.chars().map(|c| char_width(c, font_size)).sum(),
                ascent: font_size * 0.8,
                descent: font_size * 0.2,
            }
        }
    }

    fn body(dom: &Dom) -> NodeId {
        dom.elements()
            .find(|(id, _)| dom.tag_name(*id) == Some("body"))
            .map(|(id, _)| id)
            .unwrap_or(dom.document)
    }

    /// 一章的三件套：DOM、排版结果、可见文本坐标。
    struct Chapter {
        dom: Dom,
        layout: Layout,
        map: TextMap,
    }

    fn typeset(html: &str, css: &str, width: f32) -> Chapter {
        let dom = Dom::parse(html.as_bytes());
        let styles = cascade(
            &dom,
            &RuleSet::parse(css, Origin::Author),
            &ComputedStyle::default(),
        );
        let root = body(&dom);
        let map = TextMap::build(&dom, root);
        let laid = layout(
            &dom,
            &styles,
            root,
            LayoutOptions { width },
            &Mono,
            &NoImages,
        );
        Chapter {
            dom,
            layout: laid,
            map,
        }
    }

    fn cjk(chars: usize) -> String {
        "字".repeat(chars)
    }

    /// 第 `line` 行的可见文本（按坐标切出来，不走 `Layout::line_text`）。
    fn line_slice(chapter: &Chapter, line: usize) -> String {
        chapter
            .map
            .slice(line_chars(&chapter.layout, &chapter.dom, &chapter.map, line).unwrap())
    }

    #[test]
    fn the_visible_text_excludes_style_and_keeps_document_order() {
        let html = "<html><head><style>p { color: red }</style><title>书名</title></head>\
                <body><p>甲<em>乙</em>丙</p><p>丁</p></body></html>";
        let chapter = typeset(html, "", 200.0);
        assert_eq!(
            chapter.map.text(),
            "甲乙丙丁",
            "内联 CSS 与 <title> 都不是正文；文本节点按文档序拼"
        );
        assert_eq!(chapter.map.chars(), 4);
        // 字节数是 12（一字三字节），坐标系是字符数 4 —— 数错的那一处一定是中文。
        assert_eq!(chapter.map.text().len(), 12);
    }

    #[test]
    fn an_empty_map_has_no_coordinates() {
        let chapter = typeset("<body><p></p></body>", "", 200.0);
        assert!(chapter.map.is_empty());
        assert_eq!(chapter.map.chars(), 0);
        assert!(line_chars(&chapter.layout, &chapter.dom, &chapter.map, 0).is_none());
    }

    /// 核心往返：**行的偏移**存下来再找回来，必须是同一行。
    #[test]
    fn every_lines_offset_points_back_to_that_line() {
        let chapter = typeset(&format!("<p>{}</p>", cjk(120)), "", 200.0);
        let total = chapter.layout.lines.len();
        assert_eq!(total, 10, "120 个字、一行 12 个");
        for line in 0..total {
            let offset = line_offset(&chapter.layout, &chapter.dom, &chapter.map, line)
                .unwrap_or_else(|| panic!("第 {line} 行该有文本坐标"));
            assert_eq!(offset, line * 12, "每行 12 个字");
            assert_eq!(
                line_at_offset(&chapter.layout, &chapter.dom, &chapter.map, offset),
                line,
                "第 {line} 行的行首偏移找回来必须是同一行"
            );
        }
    }

    #[test]
    fn a_run_spanning_two_nodes_gives_one_wrapping_range() {
        // `<p>` 里有三个文本节点（正文 + <em> + 正文），它们排在同一行上。
        let chapter = typeset("<body><p>前段<em>中段</em>后段</p></body>", "", 600.0);
        assert_eq!(chapter.layout.lines.len(), 1);
        assert_eq!(
            line_slice(&chapter, 0),
            "前段中段后段",
            "一行横跨多个文本节点时要给出包住它们的区间"
        );
    }

    /// M5 验收的那一句：字号与宽度变了，坐标不变，落回的行也还是同一句。
    #[test]
    fn the_offset_survives_three_font_sizes_and_a_narrower_page() {
        let html = format!("<body><p>{}</p><p>{}</p></body>", cjk(60), cjk(60));
        let base = typeset(&html, "", 200.0);
        // 取第二段开头那一行（跨段边界的那一行最容易被排版参数影响）。
        let target = 5usize;
        let offset = line_offset(&base.layout, &base.dom, &base.map, target).unwrap();
        let words = line_slice(&base, target);

        for (css, width) in [
            ("", 140.0),
            ("p { font-size: 24px }", 200.0),
            ("p { font-size: 13px; line-height: 2 }", 320.0),
        ] {
            let changed = typeset(&html, css, width);
            let landed = line_at_offset(&changed.layout, &changed.dom, &changed.map, offset);
            let landed_words = line_slice(&changed, landed);
            assert!(
                words.starts_with(&landed_words) || landed_words.starts_with(&words),
                "宽度 {width}、样式 {css:?}：原行「{words}」与落回行「{landed_words}」不是同一句"
            );
        }
    }

    /// 行盒与偏移之外的另一条通道：像素 → 行号。桌面与移动共用它。
    #[test]
    fn line_at_y_chooses_the_line_that_covers_the_pixel() {
        let chapter = typeset(&format!("<p>{}</p>", cjk(60)), "", 200.0);
        assert_eq!(chapter.layout.lines.len(), 5);
        // 首行行顶 16（UA 给 <p> 的 1em 上边距），行盒 28px。
        assert_eq!(line_at_y(&chapter.layout, 0.0), 0, "还没滚出去也停在首行");
        assert_eq!(line_at_y(&chapter.layout, 16.0), 0);
        assert_eq!(
            line_at_y(&chapter.layout, 43.0),
            0,
            "43 还在第一行里（它的底是 44）"
        );
        assert_eq!(line_at_y(&chapter.layout, 44.0), 1, "到行的底才算下一行");
        assert_eq!(line_at_y(&chapter.layout, 999.0), 4, "滚过头也停在最后一行");
    }

    #[test]
    fn anchors_carry_their_fingerprints() {
        let chapter = typeset(&format!("<body><p>{}</p></body>", cjk(60)), "", 200.0);
        // start 取 30：前面有满 24 字可取，`prefix` 才是全长而不是被夹短。
        let anchor = TextAnchor::new(&chapter.map, 0, 30, 36);
        assert_eq!(anchor.exact.chars().count(), 6);
        assert_eq!(anchor.exact, cjk(6));
        assert_eq!(anchor.prefix.chars().count(), CONTEXT_CHARS, "前 24 字");
        assert_eq!(anchor.suffix.chars().count(), CONTEXT_CHARS, "后 24 字");
        assert!(!anchor.path.is_empty(), "起点所在文本节点的路径要带上");
        // `end < start` 的调用是界面上的拖反向选择，夹平成插入点而不是 panic。
        let flipped = TextAnchor::new(&chapter.map, 0, 20, 10);
        assert_eq!((flipped.start, flipped.end), (20, 20));
    }

    #[test]
    fn an_exact_anchor_relocates_as_exact() {
        let chapter = typeset(&format!("<body><p>{}</p></body>", cjk(60)), "", 200.0);
        let anchor = TextAnchor::new(&chapter.map, 0, 24, 30);
        assert_eq!(
            relocate(&anchor, &chapter.map),
            Relocated {
                start: 24,
                end: 30,
                confidence: Confidence::Exact
            }
        );
    }

    /// 第二级：同一个文本节点里插了字，偏移错了但路径还在。
    #[test]
    fn relocate_falls_back_to_the_same_dom_node() {
        let before = typeset("<body><p>开头重点句子结尾</p></body>", "", 200.0);
        // 「重点句子」在 2..6。
        let anchor = TextAnchor::new(&before.map, 0, 2, 6);
        assert_eq!(anchor.exact, "重点句子");
        assert_eq!(anchor.prefix, "开头");
        assert_eq!(anchor.suffix, "结尾");

        // 同一段前面多了五个字：文本节点没变（路径相同），坐标全体右移。
        let after = typeset("<body><p>新加的字串开头重点句子结尾</p></body>", "", 200.0);
        // 先确认第①级确实失败了 —— 原偏移上已经不是那句话，否则这条测试钉不住降级。
        assert_ne!(after.map.slice(2..6), anchor.exact);
        let hit = relocate(&anchor, &after.map);
        assert_eq!(hit.confidence, Confidence::SameNode, "实际：{hit:?}");
        assert_eq!(hit.start, 7, "原 2 + 前面插的 5");
        assert_eq!(after.map.slice(hit.start..hit.end), anchor.exact);
    }

    /// 第三级：路径没了、偏移也错了，但内容就在附近。
    ///
    /// 注意「同一句话被包进 `<em>`」本身**不会**动可见文本坐标（拼接顺序没变），
    /// 那条只会让第①级继续命中，测不到降级 —— 所以这里同时做两件事：
    /// 段首插字（坐标全体右移，第①级失效）+ 把句子拆进 `<em>`（原来那个文本节点
    /// 现在只含前 44 字，第②级也够不着），剩下的就只有附近窗口这一条路。
    #[test]
    fn relocate_falls_back_to_the_neighbourhood() {
        let before = typeset(
            &format!("<body><p>{}重点句{}</p></body>", cjk(40), cjk(6)),
            "",
            200.0,
        );
        let at = before
            .map
            .text()
            .find("重点句")
            .expect("fixture 里有这句话");
        let start = before.map.text()[..at].chars().count();
        let anchor = TextAnchor::new(&before.map, 0, start, start + 3);
        assert_eq!(anchor.exact, "重点句");
        assert_eq!(start, 40);

        let inserted = "补了四个字";
        let after = typeset(
            &format!(
                "<body><p>{}{}<em>重点句</em>{}</p></body>",
                inserted,
                cjk(40),
                cjk(6)
            ),
            "",
            200.0,
        );
        // 第①级失效：原偏移上是填充文字，不是那句话。
        assert_ne!(after.map.slice(start..start + 3), anchor.exact);
        // 第②级失效：锚点记的那条路径现在指向「段首插字 + 40 个「字」」这一个节点。
        let hit = relocate(&anchor, &after.map);
        assert_eq!(hit.confidence, Confidence::Nearby, "实际：{hit:?}");
        assert_eq!(after.map.slice(hit.start..hit.end), "重点句");
        assert_eq!(
            hit.start,
            start + inserted.chars().count(),
            "内容只跟着段首插的字右移，没有跳到别处去"
        );
    }

    /// 第四级：附近几百字内没有，整章里还有 —— 命中，但置信度要留给界面。
    #[test]
    fn relocate_falls_back_to_the_whole_chapter() {
        let chapter = typeset(
            &format!("<body><p>{}唯一句{}</p></body>", cjk(600), cjk(600)),
            "",
            200.0,
        );
        // 手工造一条「偏移与路径都不再有效、但整章还搜得到」的锚点。
        let anchor = TextAnchor {
            chapter: 0,
            start: 5,
            end: 8,
            exact: "唯一句".into(),
            prefix: cjk(24),
            suffix: cjk(24),
            path: "/html[1]/body[1]/p[1]#999".into(),
        };
        let hit = relocate(&anchor, &chapter.map);
        assert_eq!(hit.confidence, Confidence::Anywhere, "实际：{hit:?}");
        assert_eq!(chapter.map.slice(hit.start..hit.end), "唯一句");
    }

    #[test]
    fn relocate_says_stale_instead_of_guessing_a_neighbour() {
        let chapter = typeset("<body><p>正文在这里</p></body>", "", 200.0);
        let anchor = TextAnchor {
            chapter: 0,
            start: 0,
            end: 4,
            exact: "这一句不在这本书里".into(),
            prefix: String::new(),
            suffix: String::new(),
            path: String::new(),
        };
        let hit = relocate(&anchor, &chapter.map);
        assert_eq!(hit.confidence, Confidence::Stale);
        // stale 不编造坐标：给的是夹到范围内的原值，让调用方自己标「找不到」。
        assert_eq!((hit.start, hit.end), (0, 4));
    }

    #[test]
    fn an_empty_chapter_is_stale_not_a_panic() {
        let chapter = typeset("<body><p></p></body>", "", 200.0);
        let anchor = TextAnchor::new(&chapter.map, 0, 0, 5);
        assert_eq!(
            relocate(&anchor, &chapter.map).confidence,
            Confidence::Stale
        );
    }

    #[test]
    fn the_anchor_json_round_trips_and_garbage_is_not_an_anchor() {
        let chapter = typeset("<body><p>一句中文</p></body>", "", 200.0);
        let anchor = TextAnchor::new(&chapter.map, 3, 1, 3);
        let json = anchor.to_json();
        assert_eq!(TextAnchor::from_json(&json).as_ref(), Some(&anchor));
        // 存的列可能来自旧版本或被别的程序写坏 —— 读不回来是「没有锚点」，不是 panic。
        assert_eq!(TextAnchor::from_json("{ not json"), None);
        assert_eq!(TextAnchor::from_json(r#"{"chapter":"x"}"#), None);
        // 只带坐标的 JSON（进度那种形状）不该被当锚点解出来：缺字段就整体失败。
        assert_eq!(TextAnchor::from_json(r#"{"chapter":0,"start":1}"#), None);
    }

    #[test]
    fn a_point_anchor_carries_only_a_coordinate() {
        let point = TextAnchor::point(2, 1234);
        assert_eq!((point.chapter, point.start, point.end), (2, 1234, 1234));
        assert!(point.exact.is_empty() && point.path.is_empty());
        // 进度这种形状没有指纹，所以 relocate 直接判 stale 而不是猜。
        let chapter = typeset("<body><p>正文</p></body>", "", 200.0);
        assert_eq!(relocate(&point, &chapter.map).confidence, Confidence::Stale);
    }

    /// 标签之间的换行与缩进也是文本节点，所以「第一段正文的第一个字」坐标通常是 1，
    /// 不是 0。这不是 bug，是坐标系的定义（见模块头），但它必须被钉住 ——
    /// 否则将来谁把空白节点滤掉，整本书已存的锚点就会集体平移而没人知道。
    #[test]
    fn whitespace_between_tags_is_part_of_the_coordinate_space() {
        let pretty = typeset("<body>\n  <p>正文</p>\n  <p>第二段</p>\n</body>", "", 600.0);
        // 实测的解析形状：`</p>\n  <p>` 之间是一段空白 ⇒ 一个文本节点，不是两个。
        assert_eq!(
            pretty.map.text(),
            "\n  正文\n  第二段\n",
            "文本节点按文档序原样拼，包括标签之间的空白"
        );
        let offset = line_offset(&pretty.layout, &pretty.dom, &pretty.map, 0).unwrap_or(999);
        assert!(offset > 0, "第一行的坐标不该是 0：{offset}");
        assert_eq!(pretty.map.slice(offset..offset + 2), "正文");
        // 同一份内容压掉空白之后坐标就变了 —— 所以坐标系只能有一套定义。
        let tight = typeset("<body><p>正文</p><p>第二段</p></body>", "", 600.0);
        assert_eq!(
            line_offset(&tight.layout, &tight.dom, &tight.map, 0).unwrap_or(999),
            0
        );
    }
}
