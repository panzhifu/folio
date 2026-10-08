//! 排版引擎 E1：块级布局 + 单 run 行内布局。
//!
//! 输入 `Dom` + `StyledDom`，输出扁平的 `BlockBox` / `LineBox` 两张表。
//! 之所以是扁平表而不是树：M4 的 fragmentation 要按高度切行盒，树形反而碍事；
//! 需要父子关系时按 `BlockBox::lines` 的包含关系还原。
//!
//! 三段各自独立可测：
//! 1. **收集** [`collect`]：把行内子树展平成一段文本 + piece 表（空白折叠、`<br>` 硬换行）。
//! 2. **断行**：交给注入的 [`Measure`]（app 侧用 `TextSystem::line_wrapper` 实现）。
//! 3. **摆放** [`Ctx::children`]：块级顺序、兄弟间外边距折叠、`text-indent`、`text-align`。
//!
//! 坐标一律是布局空间内的**绝对值**，绘制阶段直接取用，不再二次换算。
//! `blocks` 按**先序**入表：父块的 `lines` 区间包含子块，`block_of_line`
//! 取最后一个命中的盒子，就是真正拥有这些行的那个叶子块。
//!
//! 本层的已知取舍（同步记在 docs/known-limitations.md）：
//! - 段落内只用**块元素自己的**样式测行：`<strong>` 的粗、`<small>` 的小在这一层被
//!   拉平成同一种度量。多 run 度量是 M3 的 E2。
//! - 不做父子外边距折叠：嵌套容器里首个子块的 top margin 会计入父高度，
//!   比浏览器多一档空白，但不会重叠、也不会丢间距。
//! - 行内图片不产出片段（M3）；负外边距按 0；`width` / `max-width` / `float` 未接入。

use std::ops::Range;

use crate::css::computed::{ComputedStyle, Display, StyledDom, TextAlign};
use crate::dom::{Dom, NodeId, NodeKind};

/// 一次度量需要的最小字体描述。`family == "monospace"` 是泛族名，
/// 由 `Measure` 的实现决定它对应哪个真实 family。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontKey {
    pub family: String,
    pub weight: u16,
    pub italic: bool,
}

impl FontKey {
    pub fn of(style: &ComputedStyle) -> Self {
        Self {
            family: style.font_family.clone(),
            weight: style.font_weight,
            italic: style.font_italic,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LineMetrics {
    pub width: f32,
    pub ascent: f32,
    pub descent: f32,
}

/// 度量与断行的注入点。
///
/// spike① 的结论：`line_wrapper` / `advance` 挂在 App 级 `TextSystem` 上，
/// 不要求 `&mut Window`，所以整条 DOM→Style→Layout 链可以进 `background_spawn`。
/// 前提是本模块的输出全是 `Send` 纯数据。
pub trait Measure: Send + Sync {
    /// 把 `text` 按可用宽度切行，返回覆盖全文的不重叠字节区间。
    /// `avail` 是整行可用的宽度，`first_indent` 是第一行要让出来的部分 ——
    /// 与 `TextSystem::line_wrapper(..).wrap_line(text, width, IndentAdjustment)` 同形。
    fn split(
        &self,
        text: &str,
        font: &FontKey,
        font_size: f32,
        avail: f32,
        first_indent: f32,
    ) -> Vec<Range<usize>>;

    /// 一段文本的自然宽度与基线。
    fn metrics(&self, text: &str, font: &FontKey, font_size: f32) -> LineMetrics;
}

/// 行盒里的一段连续文本，同时带着两套坐标。
#[derive(Debug, Clone, PartialEq)]
pub struct Fragment {
    /// 文本节点。§4.2 把锚点定义在 Layout **之前**的坐标系上，就是这个 node。
    pub node: NodeId,
    /// `BlockBox::text` 的字节区间 —— 绘制用。
    pub norm: Range<usize>,
    /// `node.text` 的字节区间 —— 选择与锚点用。
    pub orig: Range<usize>,
}

#[derive(Debug, Clone)]
pub struct LineBox {
    /// 所属块的 DOM 节点。
    pub owner: NodeId,
    /// 空行（`<p><br></p>`、整段空白）没有片段。
    pub fragments: Vec<Fragment>,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    /// 行盒高 = 字号 × 行距倍数。
    pub height: f32,
    /// 相对 `y` 的基线下沉量。
    pub baseline: f32,
    pub align: TextAlign,
    pub font_size: f32,
    /// 这一行**被测量时**用的字体。绘制侧必须用同一个字体，否则
    /// 「按 A 量出来的居中偏移」配「按 B 画出来的字宽」，标题就会歪到版心外。
    pub font: FontKey,
}

#[derive(Debug, Clone)]
pub struct BlockBox {
    pub node: NodeId,
    /// 边框盒左上角（不含 margin）。
    pub x: f32,
    pub y: f32,
    /// 边框盒尺寸（含 padding，不含 margin）。
    pub width: f32,
    pub height: f32,
    /// 本块行内内容的归一化文本；`Fragment::norm` 索引到这里。容器块为空串。
    pub text: String,
    /// `Layout::lines` 的下标区间。
    pub lines: Range<usize>,
}

#[derive(Debug, Default)]
pub struct Layout {
    pub blocks: Vec<BlockBox>,
    pub lines: Vec<LineBox>,
    pub width: f32,
    pub height: f32,
}

impl Layout {
    /// 命中测试：纵坐标 → 行盒下标。选择与批注回显从这里起步。
    pub fn line_at(&self, y: f32) -> Option<usize> {
        self.lines
            .iter()
            .position(|l| y >= l.y && y < l.y + l.height)
    }

    /// 真正拥有第 `i` 行的盒子：先序入表 ⇒ 最后一个区间包含它的就是最深的叶子块。
    pub fn block_of_line(&self, i: usize) -> Option<&BlockBox> {
        self.blocks.iter().rev().find(|b| b.lines.contains(&i))
    }

    /// 第 `i` 行的文本。
    pub fn line_text(&self, i: usize) -> &str {
        let (Some(line), Some(block)) = (self.lines.get(i), self.block_of_line(i)) else {
            return "";
        };
        let start = line.fragments.first().map(|f| f.norm.start).unwrap_or(0);
        let end = line.fragments.last().map(|f| f.norm.end).unwrap_or(0);
        block.text.get(start..end).unwrap_or("")
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LayoutOptions {
    /// 内容区宽度（页面左右边距已扣）。
    pub width: f32,
}

/// 排版 `root` 的全部内容。`root` 通常是 `<body>`：它的 padding 生效，
/// 字号与字体取它自己的计算样式（读者偏好已经在 `cascade` 的根样式里注入）。
pub fn layout(
    dom: &Dom,
    styles: &StyledDom,
    root: NodeId,
    options: LayoutOptions,
    measure: &dyn Measure,
) -> Layout {
    let mut ctx = Ctx {
        dom,
        styles,
        measure,
        blocks: Vec::new(),
        lines: Vec::new(),
    };
    let style = ctx.style_of(root);
    let pl = style.padding_left.max(0.0);
    let pt = style.padding_top.max(0.0);
    let content_width = (options.width - pl - style.padding_right.max(0.0)).max(1.0);
    let height = ctx.children(root, pl, pt, content_width);
    Layout {
        blocks: ctx.blocks,
        lines: ctx.lines,
        width: content_width,
        height: pt + height,
    }
}

struct Ctx<'a> {
    dom: &'a Dom,
    styles: &'a StyledDom,
    measure: &'a dyn Measure,
    blocks: Vec<BlockBox>,
    lines: Vec<LineBox>,
}

/// 一段行内内容展平后的产物。
struct Collected {
    text: String,
    pieces: Vec<Piece>,
}

/// 一个文本节点对段落的贡献：段内区间 + 原节点区间。
struct Piece {
    node: NodeId,
    norm: Range<usize>,
    orig: Range<usize>,
}

impl<'a> Ctx<'a> {
    fn style_of(&self, node: NodeId) -> ComputedStyle {
        self.styles.get(node).cloned().unwrap_or_default()
    }

    fn children_of(&self, node: NodeId) -> Vec<NodeId> {
        self.dom.get(node).children.clone()
    }

    /// 摆放 `container` 的流内子内容。`x`/`y` 是其内容盒左上角（绝对），返回内容高度。
    fn children(&mut self, container: NodeId, x: f32, y: f32, width: f32) -> f32 {
        let container_style = self.style_of(container);
        let mut cursor = y;
        let mut prev_bottom = 0f32;
        let mut pending: Vec<NodeId> = Vec::new();

        for child in self.children_of(container) {
            let style = self.style_of(child);
            if style.display == Display::None {
                continue;
            }
            if self.is_inline_flow(child, &style) {
                pending.push(child);
                continue;
            }
            // 遇到块级子节点，先把攒着的行内内容冲成一个匿名块。
            if !pending.is_empty() {
                let pieces = std::mem::take(&mut pending);
                cursor +=
                    self.wrap_paragraph(container, &pieces, x, cursor, width, &container_style);
                prev_bottom = 0.0;
            }
            let ml = style.margin_left.max(0.0);
            let mr = style.margin_right.max(0.0);
            let mt = style.margin_top.max(0.0);
            // 兄弟之间折叠取较大值而不是相加：这是段间距不显松的唯一原因。
            cursor += if prev_bottom > 0.0 {
                mt.max(prev_bottom)
            } else {
                mt
            };
            cursor += self.block(child, x + ml, cursor, (width - ml - mr).max(1.0));
            prev_bottom = style.margin_bottom.max(0.0);
        }
        if !pending.is_empty() {
            cursor += self.wrap_paragraph(container, &pending, x, cursor, width, &container_style);
        }
        cursor - y
    }

    /// 走行内路径吗：文本节点、`<br>`、`display: inline` 都是。
    fn is_inline_flow(&self, node: NodeId, style: &ComputedStyle) -> bool {
        self.dom.get(node).kind != NodeKind::Element
            || self.dom.is_element(node, "br")
            || style.display == Display::Inline
    }

    /// 直接子节点里有没有块级元素。只看一层：更深的嵌套由递归时重新判断。
    fn has_block_children(&self, node: NodeId) -> bool {
        self.children_of(node).into_iter().any(|child| {
            let style = self.style_of(child);
            style.display != Display::None && !self.is_inline_flow(child, &style)
        })
    }

    /// 摆放一个块级元素，返回边框盒高度。
    fn block(&mut self, node: NodeId, x: f32, y: f32, width: f32) -> f32 {
        let style = self.style_of(node);
        let pl = style.padding_left.max(0.0);
        let pr = style.padding_right.max(0.0);
        let pt = style.padding_top.max(0.0);
        let pb = style.padding_bottom.max(0.0);
        let content_width = (width - pl - pr).max(1.0);
        let index = self.push_block(node, x, y, width, String::new());
        let inner = if self.has_block_children(node) {
            self.children(node, x + pl, y + pt, content_width)
        } else {
            let pieces = self.children_of(node);
            self.wrap_paragraph(node, &pieces, x + pl, y + pt, content_width, &style)
        };
        self.close_block(index, pt + inner + pb)
    }

    /// 把一段行内内容（块自己的子节点，或容器里混排的匿名块）摆成若干行盒。
    fn wrap_paragraph(
        &mut self,
        owner: NodeId,
        pieces: &[NodeId],
        x: f32,
        y: f32,
        width: f32,
        style: &ComputedStyle,
    ) -> f32 {
        let collected = collect(pieces, self.dom, style.white_space_pre);
        let index = self.push_block(owner, x, y, width, collected.text.clone());
        let height = self.wrap_lines(owner, &collected, x, y, width, style);
        self.close_block(index, height)
    }

    /// 先序入表：盒子必须在断行之前拿到自己的下标，否则 `lines` 区间无法回填。
    fn push_block(&mut self, node: NodeId, x: f32, y: f32, width: f32, text: String) -> usize {
        let start = self.lines.len();
        self.blocks.push(BlockBox {
            node,
            x,
            y,
            width,
            height: 0.0,
            text,
            lines: start..start,
        });
        self.blocks.len() - 1
    }

    fn close_block(&mut self, index: usize, height: f32) -> f32 {
        let block = &mut self.blocks[index];
        block.height = height;
        block.lines = block.lines.start..self.lines.len();
        height
    }

    /// 断行 + 摆放，返回行盒总高。
    fn wrap_lines(
        &mut self,
        owner: NodeId,
        collected: &Collected,
        x: f32,
        y: f32,
        width: f32,
        style: &ComputedStyle,
    ) -> f32 {
        if collected.text.is_empty() {
            return 0.0;
        }
        let font = FontKey::of(style);
        let line_height = (style.font_size * style.line_height).max(1.0);
        // 缩进最多吃掉半个版心，否则首行会被挤成零宽。
        let indent = style.text_indent.max(0.0).min(width * 0.5);
        let mut cursor = y;
        let mut first = true;
        for hard in hard_lines(&collected.text) {
            let slice = collected.text.get(hard.clone()).unwrap_or("");
            // 断行阶段先问「首行让出多少」，摆放阶段再逐行决定缩进与对齐偏移。
            let lead = if first { indent } else { 0.0 };
            let ranges = if slice.is_empty() {
                vec![hard]
            } else {
                self.measure
                    .split(slice, &font, style.font_size, width, lead)
                    .into_iter()
                    .map(|r| (hard.start + r.start)..(hard.start + r.end))
                    .collect()
            };
            for norm in ranges {
                // text-indent 只作用于块里的第一个行盒，后续行回到版心左缘。
                let lead = if first { indent } else { 0.0 };
                let avail = (width - lead).max(1.0);
                let text = collected.text.get(norm.clone()).unwrap_or("");
                let metrics = self.measure.metrics(text, &font, style.font_size);
                let offset = match style.text_align {
                    TextAlign::Right => (avail - metrics.width).max(0.0),
                    TextAlign::Center => (avail - metrics.width).max(0.0) / 2.0,
                    _ => 0.0,
                };
                self.lines.push(LineBox {
                    owner,
                    fragments: fragments_for(norm, &collected.pieces),
                    x: x + lead + offset,
                    y: cursor,
                    width: metrics.width,
                    height: line_height,
                    baseline: metrics.ascent,
                    align: style.text_align,
                    font_size: style.font_size,
                    font: font.clone(),
                });
                cursor += line_height;
                first = false;
            }
        }
        cursor - y
    }
}

/// `\n` 是唯一在 `white-space: normal` 下也生效的硬换行，来源只有 `<br>` 与 `pre`。
fn hard_lines(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, _) in text.match_indices('\n') {
        out.push(start..i);
        start = i + 1;
    }
    out.push(start..text.len());
    out
}

/// 把若干兄弟子树展平成一段文本，同时记下每个来源节点的区间。
fn collect(pieces: &[NodeId], dom: &Dom, pre: bool) -> Collected {
    let mut out = Collector::default();
    for node in pieces {
        out.push_node(*node, dom, pre);
    }
    Collected {
        text: out.text,
        pieces: out.pieces,
    }
}

/// 折叠空白必须**跨节点**进行：`<p>前   <em>中</em></p>` 里上一段尾巴上的空白
/// 和下一段开头的空白是同一次折叠，谁先落地谁占位，两边都不能各留一个空格。
/// 所以「攒着一个空格」的状态挂在收集器上，而不是每个节点各算一遍。
#[derive(Default)]
struct Collector {
    text: String,
    pieces: Vec<Piece>,
    pending_space: bool,
}

impl Collector {
    fn push_node(&mut self, node: NodeId, dom: &Dom, pre: bool) {
        let dom_node = dom.get(node);
        if dom_node.kind == NodeKind::Text {
            let src = dom_node.text.as_str();
            let norm_start = self.text.len();
            let orig = if pre {
                self.text.push_str(src);
                0..src.len()
            } else {
                self.push_collapsed(src)
            };
            if self.text.len() > norm_start {
                self.pieces.push(Piece {
                    node,
                    norm: norm_start..self.text.len(),
                    orig,
                });
            }
            return;
        }
        if dom.is_element(node, "br") {
            self.text.push('\n');
            self.pending_space = false;
            return;
        }
        for child in dom_node.children.clone() {
            // 嵌套元素自己的 white-space 不再单独判断，整段按外层处理（一期）。
            self.push_node(child, dom, pre);
        }
    }

    /// 连续空白并成一个空格；段首（此前还没有任何内容）与段尾（还没遇到内容）的空白吞掉。
    /// 返回真正落地的那段原文的字节跨度。
    fn push_collapsed(&mut self, src: &str) -> Range<usize> {
        let bytes = src.as_bytes();
        let Some(first) = bytes.iter().position(|b| !b.is_ascii_whitespace()) else {
            // 整段是空白：只留下「这里有过空白」的事实。
            self.pending_space = true;
            return 0..0;
        };
        let last = bytes
            .iter()
            .rposition(|b| !b.is_ascii_whitespace())
            .unwrap_or(first);
        // 原文跨度把开头的空白也算进来：那个空格确实来自本节点。
        let orig_start = if bytes[0].is_ascii_whitespace() {
            0
        } else {
            first
        };
        // 前导空白要先记账：下面的循环只处理内容**内部**的空白 run，
        // 少了这步，`<em>中</em>   后` 里后一个节点的前导空白会被整个丢掉。
        self.pending_space = self.pending_space || first > 0;
        for ch in src[first..last + 1].chars() {
            if ch.is_whitespace() {
                self.pending_space = true;
                continue;
            }
            self.flush_space();
            self.text.push(ch);
        }
        // 尾巴上的空白不立刻落地，交给下一个内容去决定要不要它。
        self.pending_space =
            self.pending_space || bytes[last + 1..].iter().any(|b| b.is_ascii_whitespace());
        orig_start..last + 1
    }

    fn flush_space(&mut self) {
        if !self.pending_space {
            return;
        }
        self.pending_space = false;
        if !self.text.is_empty() && !self.text.ends_with([' ', '\n']) {
            self.text.push(' ');
        }
    }
}

/// 把一个段内区间按来源节点切成片段。
///
/// piece 的 norm 与 orig 等长时（绝大多数）映射是仿射的，逐字符精确；
/// 折叠掉内部空白后两者不等长，这时退化成整段 piece 的跨度 ——
/// 精确还原留给 M5 的锚点重定位，那里本来就要按节点重跑一次同样的折叠。
fn fragments_for(norm: Range<usize>, pieces: &[Piece]) -> Vec<Fragment> {
    let mut out = Vec::new();
    for piece in pieces {
        let start = norm.start.max(piece.norm.start);
        let end = norm.end.min(piece.norm.end);
        if start >= end {
            continue;
        }
        let orig = if piece.norm.len() == piece.orig.len() {
            let base = piece.orig.start + (start - piece.norm.start);
            base..base + (end - start)
        } else {
            piece.orig.clone()
        };
        out.push(Fragment {
            node: piece.node,
            norm: start..end,
            orig,
        });
    }
    out
}

#[cfg(test)]
mod tests;
