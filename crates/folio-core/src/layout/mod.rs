//! 排版引擎 E1+E2：块级布局 + 多 run 行内布局。
//!
//! 输入 `Dom` + `StyledDom`，输出扁平的 `BlockBox` / `LineBox` 两张表。
//! 之所以是扁平表而不是树：M4 的 fragmentation 要按高度切行盒，树形反而碍事；
//! 需要父子关系时按 `BlockBox::lines` 的包含关系还原。
//!
//! 分工是刻意切开的：
//! 1. **收集** [`collect`]：把行内子树展平成一段文本 + piece 表。每个 piece 带着
//!    **它自己那个元素**的计算样式，所以 `<em>` 的斜体、`<small>` 的小字不会丢。
//! 2. **断行** [`Ctx::wrap_lines`]：由引擎自己做，字体只提供两种能力 ——
//!    单字前进量与整段绘制度量。断点取自 UAX #14（`unicode-linebreak`），
//!    这正是中文禁则（标点不该出现在行首/行尾）的依据。
//! 3. **摆放** [`Ctx::children`]：块级顺序、兄弟间外边距折叠、`text-indent`、`text-align`。
//!
//! 为什么断行要引擎自持而不是调 `TextSystem::line_wrapper`：那个 API 一次只能
//! 按一种字体折一行，多 run 段落用不了；而且禁则与行盒基线要我们自己说了算。
//! 断点按单字前进量累加判定（与 gpui 自己的 LineWrapper 同源），
//! 行宽则按实际 shaping 报告并用于对齐 —— 对齐用后者，绘制出来的行不会超出可用宽度。
//!
//! 坐标一律是布局空间内的**绝对值**，绘制阶段直接取用。
//! `blocks` 按**先序**入表：父块的 `lines` 区间包含子块，`block_of_line`
//! 取最后一个命中的盒子，就是真正拥有这些行的那个叶子块。
//!
//! 本层的已知取舍（同步记在 docs/known-limitations.md）：
//! - 不做父子外边距折叠：嵌套容器里首个子块的 top margin 会计入父高度，
//!   比浏览器多一档空白，但不会重叠、也不会丢间距。
//! - 行内图片不产出 run（M3 的后半）；负外边距按 0；`width` / `max-width` / `float` 未接入。
//! - 嵌套元素自己的 `white-space` 不单独判断，整段按外层块的设置走。
//! - 分片（[`fragment`]）在断点处**丢掉上下 margin**：第 N 页末行下面那段空白不会出现在
//!   第 N+1 页开头。CSS 是两侧都留，这里选每页顶部稳定。

pub mod fragment;

use std::ops::Range;
use std::rc::Rc;

use unicode_linebreak::linebreaks;

use crate::css::computed::{Color, ComputedStyle, Display, Length, StyledDom, TextAlign};
use crate::dom::{Dom, NodeId, NodeKind};

/// 一次度量需要的最小字体描述。`family == "monospace"` 是泛族名，
/// 由 `Measure` 的实现决定它对应哪个真实 family。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
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

/// 一个行内 run 的绘制属性。从 `ComputedStyle` 里挑出排版层真正要用的那几项，
/// 这样 `LineRun` 不必背着整份样式表。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RunStyle {
    pub font: FontKey,
    pub font_size: f32,
    /// 倍数，不是像素。
    pub line_height: f32,
    pub color: Option<Color>,
    /// 每个空格之后额外加的宽度（px），来自 CSS 的 `word-spacing`。
    pub word_spacing: f32,
    /// 基线偏移，px，正数往上。来自 `vertical-align`，绘制层把它从行的基线里减掉。
    pub baseline_shift: f32,
    /// `text-decoration` 的两条线。装饰线**不计入行盒高**（CSS 也是这样），
    /// 所以排版层只负责把它带到 run 上。
    pub underline: bool,
    pub line_through: bool,
}

impl RunStyle {
    pub fn new(style: &ComputedStyle) -> Self {
        Self {
            font: FontKey::of(style),
            font_size: style.font_size,
            line_height: style.line_height,
            color: style.color,
            word_spacing: style.word_spacing.max(0.0),
            baseline_shift: style.baseline_shift,
            underline: style.underline,
            line_through: style.line_through,
        }
    }

    /// 该 run 的 inline box 高度。
    pub fn box_height(&self) -> f32 {
        (self.font_size * self.line_height).max(1.0)
    }

    /// 一个字符的前进量。`word-spacing` 叠在空格上 —— CSS 里它加在 word separator 之后，
    /// 而我们的折行就是按字符累加的，所以两处（折行与重算已用宽）都走这一个函数，
    /// 不会出现「折行算得下、画出来超出」的错位。
    pub fn advance(&self, measure: &dyn Measure, ch: char) -> f32 {
        let base = measure.char_width(ch, &self.font, self.font_size);
        if ch == ' ' {
            base + self.word_spacing
        } else {
            base
        }
    }

    /// 这段文本里所有空格叠出来的额外宽度。绘制宽度由 `run_metrics` 给，
    /// 它不认识词距，所以 run 的宽度要在这里补上。
    pub fn extra_for_spaces(&self, text: &str) -> f32 {
        if self.word_spacing == 0.0 {
            return 0.0;
        }
        text.chars().filter(|ch| *ch == ' ').count() as f32 * self.word_spacing
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LineMetrics {
    pub width: f32,
    pub ascent: f32,
    pub descent: f32,
}

/// 字体能力的注入点。
///
/// spike① 的结论：度量与前进量挂在 App 级 `TextSystem` 上，不要求 `&mut Window`，
/// 所以整条 DOM→Style→Layout 链可以进 `background_spawn`。
/// 前提是本模块的输出全是 `Send` 纯数据。
pub trait Measure: Send + Sync {
    /// 单个字符的前进量，只用于决定断点。
    fn char_width(&self, ch: char, font: &FontKey, font_size: f32) -> f32;

    /// 一段同样式文本的实际绘制宽度与基线。
    fn run_metrics(&self, text: &str, font: &FontKey, font_size: f32) -> LineMetrics;
}

/// 一张已解析的图片：容器路径 + 固有像素尺寸。CSS 的缩放由排版层再算。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedImage {
    pub href: String,
    /// 固有尺寸，取不到时（SVG 等）为 0，由 [`image_size`] 降级。
    pub width: f32,
    pub height: f32,
}

/// 图片固有尺寸的注入点。`src` 是 `<img>` 的**原始** `src` 属性值，
/// 实现方负责按当前章的目录把它解析成容器路径再查表。
///
/// 与 `Measure` 分开是因为两者的数据来源根本不同：字体度量来自窗口文本系统，
/// 图片尺寸来自书籍资源。
pub trait ImageProvider {
    fn resolve(&self, src: &str) -> Option<ResolvedImage>;
}

/// 没有图片时的空实现：一律返回 None，`<img>` 会被跳过。
pub struct NoImages;

impl ImageProvider for NoImages {
    fn resolve(&self, _src: &str) -> Option<ResolvedImage> {
        None
    }
}

/// 按 CSS 与固有尺寸算一张图的显示尺寸（CSS px）。
///
/// 顺序：`width`（px/%/auto→固有宽）→ `height`（px/auto→按纵横比）→ `max-width` /
/// `max-height` 夹 → 兜底不超容器。固有尺寸缺失（SVG）时用指定宽或容器宽，比例按 1:1。
fn image_size(style: &ComputedStyle, iw: f32, ih: f32, container_w: f32) -> (f32, f32) {
    let basis = container_w.max(1.0);
    let has_intrinsic = iw > 0.0 && ih > 0.0;

    let mut w = match style.width {
        Some(l) => l.resolve(basis),
        None if has_intrinsic => iw,
        None => basis,
    }
    .max(0.0);

    let mut h = match style.height {
        // 百分比高度没有包含块高度作基准，按 auto 处理（见 known-limitations）。
        Some(Length::Px(v)) => v,
        _ if has_intrinsic && w > 0.0 => ih * (w / iw),
        _ => w,
    }
    .max(0.0);

    if let Some(max_w) = style.max_width.map(|l| l.resolve(basis))
        && max_w >= 0.0
        && w > max_w
    {
        if w > 0.0 {
            h *= max_w / w;
        }
        w = max_w;
    }
    if let Some(Length::Px(max_h)) = style.max_height
        && max_h >= 0.0
        && h > max_h
    {
        if h > 0.0 {
            w *= max_h / h;
        }
        h = max_h;
    }
    // 绝不溢出容器：书里没写 max-width 的插图也不该被裁掉。
    if w > basis {
        if w > 0.0 {
            h *= basis / w;
        }
        w = basis;
    }
    (w, h)
}

/// 行盒里的一段连续同样式文本，同时带着两套坐标。
#[derive(Debug, Clone, PartialEq)]
pub struct LineRun {
    /// 文本节点。§4.2 把锚点定义在 Layout **之前**的坐标系上，就是这个 node。
    pub node: NodeId,
    /// `BlockBox::text` 的字节区间 —— 绘制用。
    pub norm: Range<usize>,
    /// `node.text` 的字节区间 —— 选择与锚点用。
    pub orig: Range<usize>,
    pub style: RunStyle,
    /// 实际绘制宽度与基线/下沉量。
    pub width: f32,
    pub ascent: f32,
    pub descent: f32,
}

/// 行盒里的一段图片。`href` 是**解析后的容器绝对路径**，绘制层按它取字节解码。
/// 宽度/高度已按 CSS 与内在尺寸算好，绘制层不需要再量。
#[derive(Debug, Clone, PartialEq)]
pub struct ImageRun {
    pub node: NodeId,
    pub href: String,
    pub width: f32,
    pub height: f32,
}

/// 行盒里的一段内容：文本或替换元素（图片）。M6 的 ruby 会加第三个分支。
#[derive(Debug, Clone, PartialEq)]
pub enum LineFragment {
    Text(LineRun),
    Image(ImageRun),
}

#[derive(Debug, Clone)]
pub struct LineBox {
    /// 所属块的 DOM 节点。
    pub owner: NodeId,
    /// 行内片段，按绘制顺序排列。空行（`<p><br></p>`）没有片段。
    pub fragments: Vec<LineFragment>,
    pub x: f32,
    pub y: f32,
    /// 各片段绘制宽度之和。
    pub width: f32,
    /// 行盒高 = 各片段 inline box 高度的最大值。
    pub height: f32,
    /// 相对 `y` 的基线下沉量：各片段最大的 ascent。
    pub baseline: f32,
    pub align: TextAlign,
}

impl LineBox {
    /// 文本片段，按绘制顺序。
    pub fn text_runs(&self) -> impl Iterator<Item = &LineRun> {
        self.fragments.iter().filter_map(|f| match f {
            LineFragment::Text(r) => Some(r),
            LineFragment::Image(_) => None,
        })
    }

    /// 图片片段，按绘制顺序。
    pub fn images(&self) -> impl Iterator<Item = &ImageRun> {
        self.fragments.iter().filter_map(|f| match f {
            LineFragment::Image(r) => Some(r),
            LineFragment::Text(_) => None,
        })
    }

    /// 这一行里最大的字号。状态栏与「这行是不是标题」一类判断用；
    /// 纯图片行没有文本字号，返回 0。
    pub fn dominant_font_size(&self) -> f32 {
        self.text_runs()
            .map(|r| r.style.font_size)
            .fold(0.0f32, f32::max)
    }
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
    /// 本块行内内容的归一化文本；`LineRun::norm` 索引到这里。容器块为空串。
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

    /// 第 `i` 行的可见文本。图片片段不产出字符，行内图位置上的哨兵字符会被去掉。
    pub fn line_text(&self, i: usize) -> String {
        let (Some(line), Some(block)) = (self.lines.get(i), self.block_of_line(i)) else {
            return String::new();
        };
        let Some(first) = line.text_runs().next() else {
            return String::new();
        };
        let start = first.norm.start;
        let end = line.text_runs().last().map(|r| r.norm.end).unwrap_or(start);
        block
            .text
            .get(start..end)
            .unwrap_or("")
            .replace(IMAGE_GLYPH, "")
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LayoutOptions {
    /// 内容区宽度（页面左右边距已扣）。
    pub width: f32,
}

/// 排版 `root` 的全部内容。`root` 通常是 `<body>`：它的 padding 生效，
/// 字号与字体取它自己的计算样式（读者偏好已经在 `cascade` 的根样式里注入）。
///
/// `images` 提供 `<img>` 的固有尺寸与解析后的路径；没有图片时传 [`NoImages`]。
pub fn layout(
    dom: &Dom,
    styles: &StyledDom,
    root: NodeId,
    options: LayoutOptions,
    measure: &dyn Measure,
    images: &dyn ImageProvider,
) -> Layout {
    let mut ctx = Ctx {
        dom,
        styles,
        measure,
        images,
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
    images: &'a dyn ImageProvider,
    blocks: Vec<BlockBox>,
    lines: Vec<LineBox>,
}

/// 一段行内内容展平后的产物。
struct Collected {
    text: String,
    pieces: Vec<Piece>,
    /// 文本流里的图片占位。`at` 指向 `text` 里那个哨兵字符的字节起点。
    images: Vec<ImageAtom>,
}

/// 一个文本节点对段落的贡献：段内区间、原节点区间、以及**该文本所属元素**的样式。
struct Piece {
    node: NodeId,
    norm: Range<usize>,
    orig: Range<usize>,
    style: Rc<RunStyle>,
}

/// 段落里一张图片：锚定在文本流某个位置，宽度已按 CSS 与内在尺寸算好。
struct ImageAtom {
    node: NodeId,
    href: String,
    width: f32,
    height: f32,
    /// 哨兵在 `Collected::text` 里的字节起点。
    at: usize,
}

/// 行内图片在文本流里的占位字符。用它而不是另建 atom 流，是为了复用既有的
/// 字节区间机制（`linebreaks` / 切片 / 断行宽度累加全都按字节走）。
const IMAGE_GLYPH: char = '\u{FFFC}';

impl Collected {
    /// 覆盖 `at` 的 piece 下标。落在 piece 之间的空白上时返回前一个 ——
    /// 那个空格本来就是从上一个元素带过来的。
    fn piece_at(&self, at: usize) -> Option<usize> {
        let idx = self.pieces.partition_point(|p| p.norm.start <= at);
        if idx == 0 {
            return None;
        }
        Some(idx - 1)
    }

    fn style_at(&self, at: usize) -> Option<Rc<RunStyle>> {
        self.piece_at(at).map(|i| Rc::clone(&self.pieces[i].style))
    }

    /// 正好落在 `at` 上的图片。
    fn image_at(&self, at: usize) -> Option<&ImageAtom> {
        let idx = self.images.partition_point(|im| im.at < at);
        self.images.get(idx).filter(|im| im.at == at)
    }
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
        // 替换元素（`display: block` 的图片）：整块就是这张图，独立成行。
        if self.dom.is_element(node, "img") {
            let src = self.dom.attr(node, "src").map(str::to_string);
            let Some(img) = src.as_deref().and_then(|s| self.images.resolve(s)) else {
                return 0.0; // 图取不到就整块不留痕，不塌成空盒子
            };
            let (w, h) = image_size(&style, img.width, img.height, width);
            let index = self.push_block(node, x, y, width, String::new());
            let offset = match style.text_align {
                TextAlign::Right => (width - w).max(0.0),
                TextAlign::Center => (width - w).max(0.0) / 2.0,
                _ => 0.0,
            };
            self.lines.push(LineBox {
                owner: node,
                fragments: vec![LineFragment::Image(ImageRun {
                    node,
                    href: img.href,
                    width: w,
                    height: h,
                })],
                x: x + offset,
                y,
                width: w,
                height: h,
                baseline: h,
                align: style.text_align,
            });
            return self.close_block(index, h);
        }

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
        let collected = collect(
            pieces,
            self.dom,
            self.styles,
            style.white_space_pre,
            self.images,
            width,
        );
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

    /// 断行。先只算出「哪些字节区间构成一行、该行让出多少缩进」，
    /// 再统一摆放 —— 分两趟是为了让 `&mut self.lines` 的写入不跟度量借用打架。
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
        // 缩进最多吃掉半个版心，否则首行会被挤成零宽。
        let indent = style.text_indent.max(0.0).min(width * 0.5);
        let mut plans: Vec<(Range<usize>, f32)> = Vec::new();
        let mut first = true;

        for hard in hard_lines(&collected.text) {
            let slice = collected.text.get(hard.clone()).unwrap_or("");
            // UAX #14：给出的下标是「下一行可以从这里开始」。
            let breaks: Vec<usize> = linebreaks(slice)
                .filter(|(ix, _)| *ix > 0 && *ix < slice.len())
                .map(|(ix, _)| hard.start + ix)
                .collect();
            let mut line_start = hard.start;
            // 首行让出去的宽度由 `avail` 扣，不再在 `used` 里加一遍：
            // 两处都算会让首行少容两个字。
            let mut used = 0.0f32;
            for (i, ch) in slice.char_indices() {
                let at = hard.start + i;
                let w = if ch == IMAGE_GLYPH {
                    match collected.image_at(at) {
                        Some(img) => img.width,
                        None => continue,
                    }
                } else {
                    match collected.style_at(at) {
                        Some(s) => s.advance(self.measure, ch),
                        None => continue,
                    }
                };
                let avail = if first {
                    (width - indent).max(1.0)
                } else {
                    width.max(1.0)
                };
                if at > line_start && used + w > avail {
                    let cut = last_break_before(&breaks, at, line_start).unwrap_or(at);
                    plans.push((line_start..cut, if first { indent } else { 0.0 }));
                    line_start = cut;
                    used = self.span_width(collected, cut..at) + w;
                    first = false;
                } else {
                    used += w;
                }
            }
            if line_start < hard.end {
                plans.push((line_start..hard.end, if first { indent } else { 0.0 }));
                first = false;
            } else if hard.start == hard.end {
                // 空硬行（`<br><br>`）也要占一行高，否则空行会塌掉。
                plans.push((hard.clone(), if first { indent } else { 0.0 }));
                first = false;
            }
        }

        let mut cursor = y;
        for (range, indent) in plans {
            cursor += self.emit_line(
                owner,
                collected,
                range,
                LinePlace {
                    x,
                    y: cursor,
                    width,
                    indent,
                },
                style,
            );
        }
        cursor - y
    }

    /// 一段文本按各自 run 累加的前进量。断行后重算新行已用宽度要用。
    fn span_width(&self, collected: &Collected, range: Range<usize>) -> f32 {
        let Some(text) = collected.text.get(range.clone()) else {
            return 0.0;
        };
        let mut sum = 0.0f32;
        for (i, ch) in text.char_indices() {
            let at = range.start + i;
            if ch == IMAGE_GLYPH {
                if let Some(img) = collected.image_at(at) {
                    sum += img.width;
                }
            } else if let Some(style) = collected.style_at(at) {
                sum += style.advance(self.measure, ch);
            }
        }
        sum
    }

    /// 把 `[start, end)` 收成一个行盒，返回它的高度。
    ///
    /// 落位的四个数打包成 [`LinePlace`]：折行只决定「这一行从哪起、占多高的一档」，
    /// 剩下的宽度与对齐都在这里按它算。
    fn emit_line(
        &mut self,
        owner: NodeId,
        collected: &Collected,
        range: Range<usize>,
        place: LinePlace,
        style: &ComputedStyle,
    ) -> f32 {
        let end = trim_end_spaces(&collected.text, range.start, range.end);
        let mut acc = LineAcc::default();
        // 按位置把文本与图片交替收进一行：图片锚在文本流的字节偏移上。
        let mut cursor = range.start;
        for img in collected
            .images
            .iter()
            .filter(|im| im.at >= range.start && im.at < end)
        {
            if img.at > cursor {
                acc.push_text(self.measure, collected, cursor..img.at);
            }
            acc.push_image(img);
            cursor = img.at + IMAGE_GLYPH.len_utf8();
        }
        if cursor < end {
            acc.push_text(self.measure, collected, cursor..end);
        }
        if acc.fragments.is_empty() {
            // 空行取块自己的样式，否则 `<p><br></p>` 会塌成零高。
            let block_style = RunStyle::new(style);
            acc.height = block_style.box_height();
            acc.ascent = block_style.font_size * 0.8;
        }
        // 行盒高至少要把字形本体装下：line-height 给得很小时也不能让两行叠在一起。
        let height = acc.height.max(acc.ascent + acc.descent);
        // 可用宽要扣掉这一行让出去的左边距（首行的 `text-indent`），
        // 否则居中的标题会按整幅宽算偏移，把字推出版心。
        let avail = (place.width - place.indent).max(1.0);
        let offset = match style.text_align {
            TextAlign::Right => (avail - acc.width).max(0.0),
            TextAlign::Center => (avail - acc.width).max(0.0) / 2.0,
            _ => 0.0,
        };
        self.lines.push(LineBox {
            owner,
            fragments: acc.fragments,
            x: place.x + place.indent + offset,
            y: place.y,
            width: acc.width,
            height,
            baseline: acc.ascent,
            align: style.text_align,
        });
        height
    }
}

/// 一行在版心里的落位。
///
/// 四个数各自的意义：`x` 是版心左缘，`y` 是这一行行顶（逐行推进的游标），
/// `width` 是版心宽，`indent` 是这一行让出去的左边距 —— 首行是 `text-indent`，
/// 后续行是 0。左缘与对齐偏移都按 `x + indent` 起算，可用宽度也按它扣一次，
/// 两处口径不一致就会出现在「缩进正好等于一个字」时居中标题被推出去的情况。
#[derive(Clone, Copy)]
struct LinePlace {
    x: f32,
    y: f32,
    width: f32,
    indent: f32,
}

/// 一行的累积器：`[start, end)` 区间内按片段顺序拼接文本与图片，同时求行盒几何。
#[derive(Default)]
struct LineAcc {
    fragments: Vec<LineFragment>,
    width: f32,
    ascent: f32,
    descent: f32,
    height: f32,
}

impl LineAcc {
    /// 把 `seg`（不跨图片）与各文本 piece 求交，逐 piece 产出一个文本 run。
    fn push_text(&mut self, measure: &dyn Measure, collected: &Collected, seg: Range<usize>) {
        for piece in &collected.pieces {
            let start = seg.start.max(piece.norm.start);
            let stop = seg.end.min(piece.norm.end);
            if start >= stop {
                continue;
            }
            let norm = start..stop;
            let text = collected.text.get(norm.clone()).unwrap_or("");
            let metrics = measure.run_metrics(text, &piece.style.font, piece.style.font_size);
            // 词距要叠在绘制宽度上：`run_metrics` 只按字形给前进量，不认识 `word-spacing`。
            let width = metrics.width + piece.style.extra_for_spaces(text);
            // 基线偏移同时改这一段的 ascent / descent：往上抬就多吃 ascent、少占 descent，
            // 行盒高与基线（下面 `emit_line` 用 `acc.ascent`）因此自动跟着涨 ——
            // 上标不再会被下一行压到，这是这个字段唯一需要参与几何的地方。
            let shift = piece.style.baseline_shift;
            self.width += width;
            self.ascent = self.ascent.max(metrics.ascent + shift);
            self.descent = self.descent.max(metrics.descent - shift);
            self.height = self.height.max(piece.style.box_height());
            self.fragments.push(LineFragment::Text(LineRun {
                node: piece.node,
                orig: orig_of(piece, &norm),
                norm,
                style: (*piece.style).clone(),
                width,
                ascent: metrics.ascent,
                descent: metrics.descent,
            }));
        }
    }

    /// 图片底贴基线：它的整高就是本行的 ascent，行盒高也随之撑起。
    fn push_image(&mut self, img: &ImageAtom) {
        self.width += img.width;
        self.ascent = self.ascent.max(img.height);
        self.height = self.height.max(img.height);
        self.fragments.push(LineFragment::Image(ImageRun {
            node: img.node,
            href: img.href.clone(),
            width: img.width,
            height: img.height,
        }));
    }
}

/// 最近的、不晚于 `at` 且严格晚于 `line_start` 的合法断点。
fn last_break_before(breaks: &[usize], at: usize, line_start: usize) -> Option<usize> {
    let idx = breaks.partition_point(|b| *b <= at);
    for i in (0..idx).rev() {
        if breaks[i] > line_start {
            return Some(breaks[i]);
        }
    }
    None
}

/// 行尾空白不计宽也不绘制（CSS 规定）：从 `end` 往回吞掉空格。
fn trim_end_spaces(text: &str, start: usize, end: usize) -> usize {
    let Some(slice) = text.get(start..end) else {
        return end;
    };
    let trimmed = slice.trim_end_matches([' ', '\t']);
    end - (slice.len() - trimmed.len())
}

/// piece 的 norm 与 orig 等长时（绝大多数）映射是仿射的，逐字符精确；
/// 折叠掉内部空白后两者不等长，这时退化成整段 piece 的跨度 ——
/// 精确还原留给 M5 的锚点重定位，那里本来就要按节点重跑一次同样的折叠。
fn orig_of(piece: &Piece, norm: &Range<usize>) -> Range<usize> {
    if piece.norm.len() == piece.orig.len() {
        let base = piece.orig.start + (norm.start - piece.norm.start);
        base..base + (norm.end - norm.start)
    } else {
        piece.orig.clone()
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

/// 把若干兄弟子树展平成一段文本，同时记下每个来源节点的区间与其元素样式。
/// `width` 是段落的可用宽度，用来折算图片的百分比宽度。
fn collect(
    pieces: &[NodeId],
    dom: &Dom,
    styles: &StyledDom,
    pre: bool,
    images: &dyn ImageProvider,
    width: f32,
) -> Collected {
    let mut out = Collector {
        text: String::new(),
        pieces: Vec::new(),
        images: Vec::new(),
        pending_space: false,
        dom,
        styles,
        provider: images,
        width,
    };
    for node in pieces {
        let style = out.style_of(*node);
        out.push_node(*node, pre, &style);
    }
    Collected {
        text: out.text,
        pieces: out.pieces,
        images: out.images,
    }
}

struct Collector<'a> {
    text: String,
    pieces: Vec<Piece>,
    images: Vec<ImageAtom>,
    /// 折叠空白是**跨节点**的：上一段尾巴上的空白与下一段开头的空白是同一次折叠，
    /// 谁先落地谁占位，所以这个状态挂在收集器上。
    pending_space: bool,
    dom: &'a Dom,
    styles: &'a StyledDom,
    provider: &'a dyn ImageProvider,
    /// 段落可用宽度，图片百分比宽度的基准。
    width: f32,
}

impl<'a> Collector<'a> {
    fn style_of(&self, node: NodeId) -> Rc<RunStyle> {
        Rc::new(RunStyle::new(
            &self.styles.get(node).cloned().unwrap_or_default(),
        ))
    }

    fn push_node(&mut self, node: NodeId, pre: bool, style: &Rc<RunStyle>) {
        let dom_node = self.dom.get(node);
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
                    style: Rc::clone(style),
                });
            }
            return;
        }
        if self.dom.is_element(node, "br") {
            self.text.push('\n');
            self.pending_space = false;
            return;
        }
        if self.dom.is_element(node, "img") {
            self.push_image(node);
            return;
        }
        // 文本继承的是**它所在元素**的样式，所以往下走要换成元素自己的样式。
        let own = self.style_of(node);
        for child in dom_node.children.clone() {
            self.push_node(child, pre, &own);
        }
    }

    /// 行内图片：命中资源就写一个哨兵占位并记下尺寸；命中不到就整张跳过
    /// （不产出任何字符，也不会把段落撑乱）。
    fn push_image(&mut self, node: NodeId) {
        let Some(src) = self.dom.attr(node, "src").map(str::to_string) else {
            return;
        };
        let Some(img) = self.provider.resolve(&src) else {
            return;
        };
        let style = self.styles.get(node).cloned().unwrap_or_default();
        let (w, h) = image_size(&style, img.width, img.height, self.width);
        self.flush_space();
        let at = self.text.len();
        self.text.push(IMAGE_GLYPH);
        self.images.push(ImageAtom {
            node,
            href: img.href,
            width: w,
            height: h,
            at,
        });
        self.pending_space = false;
    }

    /// CSS 空白折叠：连续空白并成一个空格，段首与段尾的空白吞掉。
    /// 返回真正落进 `text` 的那段原文的字节跨度。
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
