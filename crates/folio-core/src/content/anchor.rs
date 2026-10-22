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
//! 两族文档共用这一把尺，只是**建表的来源**不同：可重排走 [`TextMap::build`]（DOM 的
//! 文本节点），定版式（PDF / AZW4 / DjVu 的一页）走 [`TextMap::from_visible_text`]
//! （栅格引擎交出的字序）。两侧的 `start` / `end` / `exact` 因此同形，读回来的那一头
//! 不必问这本书属于哪一族 —— 差别只在那一条路上有 DOM 路径、另一条没有
//! （见那条入口写的降级代价）。
//!
//! 代价是这个坐标**包含 `display:none` 块里的文本**（那些字在 DOM 里、不在屏幕上）。
//! 不在这里排除它是有意的：可见性要到排版期才算得出来，而锚点必须在排版之前也稳定 ——
//! 隐藏块显示/隐藏会让整章坐标平移，那比坐标里有几个不显示的字糟得多。
//!
//! [`Locator`]: crate::Locator

use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::content::dom::{Dom, NodeId, NodeKind};
use crate::content::format::css::ua;
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

    /// 从一段**已经是可见文本**的串建坐标系（定版式那一族：一页的字序）。
    ///
    /// 这条入口存在的理由是「坐标系」与「坐标系从哪儿来」是两件事。上面那条
    /// [`TextMap::build`] 的来源是 DOM 的文本节点，而一页 PDF / DjVu / AZW4 的字不住在
    /// DOM 里 —— 它们来自栅格引擎的文字层，一份扁平的字序加逐字的盒子
    /// （[`crate::content::format::PageText`]）。锚点要的那三样（可截取的串、可子串
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
    /// [`PageText::slice`]: crate::content::format::PageText::slice
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

/// 「第几行覆盖哪些字」这张索引。折一次 O(行数)，之后两条查询只碰与查询相交的那几行。
///
/// 存在的理由：`hit::rects_for_range` 与 `line_at_offset` 从前每问一次就把**整章的行**走一遍，
/// 而每一行的判断都要过一次 [`line_chars`]（逐 run 问一次 `TextMap`）。批注回显是
/// 「每条批注 × 每帧 × 每个进链的章」问一次 —— 2026-10-15 实测：一章 25 条批注就是
/// **每帧 +1.7–2.2 ms**（`plans/阅读管线.md` §十一 第 9 条有数）。
///
/// **单调性是量出来的，不是假设的**：[`CharLines::ordered`] 记「字符区间随行号不减」。
/// 二分只在它成立时用；不成立时两条查询都退回整表扫，与接线前**逐字同一条口径**。
/// 这跟 `Layout::stacked`（行盒是否叠放才敢二分 y）是同一族防线 —— 这一带已经踩过一次
/// 「默认边界表递增、方向一反就塌成零宽」（`hit::rects_for_range` 里那段 RTL 注释），
/// 所以不再对顺序做任何未经测量的假设。
#[derive(Debug, Default)]
pub struct CharLines {
    /// 有字的行，按行号升序：`(行号, 字符起, 字符止)`。空白行与纯图片行不在表里
    /// （它们本来就没有字符，`line_chars` 给 `None`）。
    entries: Vec<(u32, u32, u32)>,
    /// `entries` 的 `字符起` 与 `字符止` **两样都不减**。二分要的是这个：
    /// 只 `起` 不减时，「相交的那些」未必是一段连续下标 —— 一条长行可以越过后面某行。
    ordered: bool,
}

impl CharLines {
    /// 走一遍整章的行折出索引。**每次排版换代一次，不是每帧一次**；调用方按需要懒建
    /// （没人选字、没人划线时这一趟根本不该付）。
    pub fn build(layout: &Layout, dom: &Dom, map: &TextMap) -> Self {
        let mut entries: Vec<(u32, u32, u32)> = Vec::new();
        let mut ordered = true;
        for line in 0..layout.lines.len() {
            let Some(range) = line_chars(layout, dom, map, line) else {
                continue;
            };
            // 折成 u32 是为了驻留量（一章两千行：12 KB 而不是 24 KB）。装不下就整表扫 ——
            // 饱和截断会让「这行覆盖哪些字」比真值短，那是**少画一块底色**级别的错，
            // 比退回整表慢一点贵得多。
            if range.end > u32::MAX as usize {
                ordered = false;
                continue;
            }
            let (start, end) = (range.start as u32, range.end as u32);
            // 单调性是**量出来的**：起点或终点倒退一次，二分的前提就不在了。
            if let Some((_, last_start, last_end)) = entries.last()
                && (start < *last_start || end < *last_end)
            {
                ordered = false;
            }
            entries.push((line as u32, start, end));
        }
        Self { entries, ordered }
    }

    /// 有没有可用二分的那条前提。`false` 时下面两个查询都给 `None`，调用方退回整表扫。
    pub fn is_ordered(&self) -> bool {
        self.ordered
    }

    /// 与 `range` **可能**相交的行号区间（左闭右开，是 `layout.lines` 的下标）。
    ///
    /// 给的是**超集**：中间夹着的空白行、以及区间上不重叠的行都还在，由调用方那道
    /// 「这一行与选区交集为空就跳过」继续滤 —— 所以窄化只可能少算，不可能漏算。
    /// 不单调时返回 `None`。
    pub fn lines_for(&self, range: Range<usize>) -> Option<Range<usize>> {
        if !self.ordered {
            return None;
        }
        if self.entries.is_empty() {
            // 整章一行字都没有（纯图片章）：谁都相交不了，空区间就是答案。
            return Some(0..0);
        }
        let (from, to) = (range.start, range.end.max(range.start));
        // 空区间与反向区间夹平后没有字符：谁都不相交，空行号段就是答案。
        if from >= to {
            return Some(0..0);
        }
        // 第一条「结束位置还在 from 之前」的行越过去就是第一个可能相交的行。
        let first = self
            .entries
            .partition_point(|&(_, _, end)| (end as usize) <= from);
        // 最后一条「起点还在 to 之前」的行，取到它的行号（含）为止。
        let last = self
            .entries
            .partition_point(|&(_, start, _)| (start as usize) < to);
        // 两段不可能有重叠（一行的 `起 <= 止`），所以 `first <= last`；相等就是
        // 查询落在两行文字之间（或整章之外），一行都不用扫。
        if first == last {
            return Some(0..0);
        }
        let start_line = self.entries[first].0 as usize;
        let end_line = self.entries[last - 1].0 as usize + 1;
        Some(start_line..end_line)
    }

    /// 覆盖 `offset` 的那一行；落在两行之间给前面最近的一行。
    ///
    /// 口径与 `line_at_offset` 逐字相同，包括两处反直觉：`before` 初值是 0，所以
    /// 「offset 在全部文字之前」也给第 0 行（哪怕第 0 行没字）；多个行都覆盖它时给
    /// **行号最小的那一个**（整表扫是从上往下第一个 `contains` 就返回）。
    /// 不单调时返回 `None`。
    pub fn line_at(&self, offset: usize) -> Option<usize> {
        if !self.ordered {
            return None;
        }
        // 覆盖者 = 「止 > offset」那一段 与 「起 <= offset」那一段 的交。前者的下界
        // 就是第一个可能覆盖的行，而按行号升序排着，它就是整表扫会返回的那一条。
        let covering = self
            .entries
            .partition_point(|&(_, _, end)| (end as usize) <= offset);
        if let Some(&(line, start, _)) = self.entries.get(covering) {
            // 后面的行起点只增不减：这一行没盖住（`start > offset`）就再没有能盖住的。
            if (start as usize) <= offset {
                return Some(line as usize);
            }
        }
        // 没有覆盖者：与整表扫那条 `before` 同一口径 —— 起点 <= offset 的最后一行。
        let before = self
            .entries
            .partition_point(|&(_, start, _)| (start as usize) <= offset)
            .checked_sub(1)?;
        Some(self.entries[before].0 as usize)
    }
}

/// 行首字符的可见偏移。**进度存这一个数**：字号、行距、宽度、分页模式都不动它。
pub fn line_offset(layout: &Layout, dom: &Dom, map: &TextMap, line: usize) -> Option<usize> {
    line_chars(layout, dom, map, line).map(|range| range.start)
}

/// 覆盖 `offset` 的那一行；落在两行之间（图片或空白行）时给它前面最近的一行。
///
/// 恢复进度用的就是这个：把界面滚到这一行的行顶，读者看到的正是他上次看的那句。
///
/// 整表扫的那一条。有 [`CharLines`] 的调用方走 [`line_at_offset_indexed`] —— 两者口径
/// 由同一个函数保证，不是一套规则写两遍（写两遍必然漂，漂出来是「进度与滚动不一致」）。
pub fn line_at_offset(layout: &Layout, dom: &Dom, map: &TextMap, offset: usize) -> usize {
    line_at_offset_indexed(layout, dom, map, offset, None)
}

/// [`line_at_offset`] 的窄化版：`index` 给了就用二分，没给或它说「不单调」就走整表。
pub fn line_at_offset_indexed(
    layout: &Layout,
    dom: &Dom,
    map: &TextMap,
    offset: usize,
    index: Option<&CharLines>,
) -> usize {
    if let Some(line) = index.and_then(|index| index.line_at(offset)) {
        // 索引里只有「有字的行」，而调用方按行号取行盒：那一行可能根本没字
        // （纯图片行被整表扫那条 `continue` 掉，索引也照样没有它）—— 二分给的
        // 是最近的一条，语义与 `before` 相同。
        return line.min(layout.lines.len().saturating_sub(1));
    }
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
///
/// 实现挪到了 [`Layout::line_covering_or_above`]：那里才有「行盒是否叠放」那一格，
/// 叠放就二分、不叠放才整表扫。留在这里的那句「没有覆盖者给最后一行」的口径没变，
/// `anchor/tests.rs::line_at_y_chooses_the_line_that_covers_the_pixel` 钉的就是它。
pub fn line_at_y(layout: &Layout, y: f32) -> usize {
    layout.line_covering_or_above(y)
}

#[cfg(test)]
mod tests;
