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
//! 本层的已知取舍（同步记在 plans/facts.md）：
//! - 不做父子外边距折叠：嵌套容器里首个子块的 top margin 会计入父高度，
//!   比浏览器多一档空白，但不会重叠、也不会丢间距。
//! - 行内图片不产出 run（M3 的后半）；负外边距按 0；`width` / `max-width` / `float` 未接入。
//! - 嵌套元素自己的 `white-space` 不单独判断，整段按外层块的设置走。
//! - 分片（[`fragment`]）在断点处**丢掉上下 margin**：第 N 页末行下面那段空白不会出现在
//!   第 N+1 页开头。CSS 是两侧都留，这里选每页顶部稳定。

pub mod fragment;

use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::sync::{Arc, LazyLock};

use unicode_linebreak::linebreaks;

use crate::css::computed::{Color, ComputedStyle, Display, Hyphens, Length, StyledDom, TextAlign};
use crate::dom::{Dom, NodeId, NodeKind};

/// 一次度量需要的最小字体描述。`family == "monospace"` 是泛族名，
/// 由 `Measure` 的实现决定它对应哪个真实 family。
///
/// `Hash` 是为了绘制路径那份 [`FontKey`] → `Font` 缓存：正文里同一条 run 样式
/// 一帧会被问到几百次，而 `gpui_font` 每次都要拿候选链去设备字体表里线性扫。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct FontKey {
    pub family: String,
    /// 作者的完整候选链，按他的优先级排。空 = 只有 `family` 那一个
    /// （UA 表那些直接写 `style.font_family` 的地方因此不用跟着改）。
    /// 逐个试「这台设备到底装了哪一个」只能由平台层做，它才看得见注册表。
    pub families: Vec<String>,
    pub weight: u16,
    pub italic: bool,
}

impl FontKey {
    pub fn of(style: &ComputedStyle) -> Self {
        Self {
            family: style.font_family.clone(),
            families: style.font_families.clone(),
            weight: style.font_weight,
            italic: style.font_italic,
        }
    }

    /// 候选链，至少含 `family` 本身。
    pub fn candidates(&self) -> &[String] {
        if self.families.is_empty() {
            std::slice::from_ref(&self.family)
        } else {
            &self.families
        }
    }
}

/// 一个行内 run 的绘制属性。从 `ComputedStyle` 里挑出排版层真正要用的那几项，
/// 这样 `LineRun` 不必背着整份样式表。
///
/// 实现了 `Hash` + `Eq` 供 [`StyleArena`] 去重：同一章里成百上千个 run 的样式
/// 其实只有几种（正文、粗体、斜体、标题…），按值去重后每个 piece 只存一个
/// `StyleRef`，不再是每个 piece 一份 `Rc<RunStyle>`。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RunStyle {
    /// 字体描述，**用 `Arc` 包着**。
    ///
    /// 这是 `RunStyle` 里唯一带分配的两个字段（`String` + `Vec<String>`）的所在，
    /// 而每行每个 run 都要 clone 一份 `RunStyle` 进 `LineRun`（一章一万多次）。
    /// 包一层 `Arc` 之后 clone 只是一次引用计数，不再每次都深拷贝字体家族链。
    pub font: Arc<FontKey>,
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

impl std::hash::Hash for RunStyle {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // f32 字段按位 hash：`to_bits` 保证 NaN/±0 的语义稳定，
        // 而样式值来自层叠结果、不会是 NaN（分母已在计算处兜底）。
        self.font.hash(state);
        self.font_size.to_bits().hash(state);
        self.line_height.to_bits().hash(state);
        self.color.hash(state);
        self.word_spacing.to_bits().hash(state);
        self.baseline_shift.to_bits().hash(state);
        self.underline.hash(state);
        self.line_through.hash(state);
    }
}

impl Eq for RunStyle {}

/// 样式在 [`StyleArena`] 里的下标。
///
/// 用 `u32` 而不是 `usize`：一个段落里的样式种类是个位数，`usize` 省下的几个字节
/// 换不来什么，而 `Piece` 是热路径上的结构。`u32::MAX` 留给「没有样式」。
pub type StyleRef = u32;

/// 一章里所有用到的 run 样式，按值去重。
///
/// 对应 KOReader 的 CSS 样式索引（`lvtinydom.cpp` 里样式按 ref 存，不复制）。
/// 为什么值得做：一章几千个文本片段，每个都带一份 `Rc<RunStyle>`（内含候选字体链
/// `Vec<String>`）时，堆分配与 `Rc` 计数操作是排版的热点之一；去重之后一份样式只
/// 建一次，片段只存 4 字节下标。
#[derive(Default)]
pub struct StyleArena {
    styles: Vec<RunStyle>,
    /// 值 → 下标。`RunStyle` 实现了 `Hash` + `Eq`（f32 按位比）。
    index: std::collections::HashMap<RunStyle, StyleRef>,
}

impl StyleArena {
    pub fn new() -> Self {
        Self::default()
    }

    /// 把一份样式收进表里，返回它的下标。同一份样式重复 intern 只占一格。
    pub fn intern(&mut self, style: RunStyle) -> StyleRef {
        if let Some(&existing) = self.index.get(&style) {
            return existing;
        }
        let id = self.styles.len() as StyleRef;
        self.styles.push(style.clone());
        self.index.insert(style, id);
        id
    }

    /// 按下标取样式。
    pub fn get(&self, id: StyleRef) -> Option<&RunStyle> {
        self.styles.get(id as usize)
    }

    /// 表里一共几种样式。测试与日志用。
    pub fn len(&self) -> usize {
        self.styles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.styles.is_empty()
    }

    /// 全部样式，按 intern 顺序。
    pub fn iter(&self) -> impl Iterator<Item = (StyleRef, &RunStyle)> {
        self.styles
            .iter()
            .enumerate()
            .map(|(id, style)| (id as StyleRef, style))
    }
}

impl RunStyle {
    pub fn new(style: &ComputedStyle) -> Self {
        Self {
            font: Arc::new(FontKey::of(style)),
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

    /// 一段同样式文本里**每个字符**的前进量。返回长度 = 字符数。
    ///
    /// 默认实现逐字调 [`char_width`](Self::char_width)；实现方可以覆盖它做批量预度量
    /// （gpui 那条路一次 shaping 就能拿到整段前进量，比 N 次虚函数调用便宜得多）。
    /// 断行与已用宽重算都走这一个出口，两处不会不同源。
    fn char_widths(&self, text: &str, font: &FontKey, font_size: f32) -> Vec<f32> {
        text.chars()
            .map(|ch| self.char_width(ch, font, font_size))
            .collect()
    }

    /// 一段同样式文本的实际绘制宽度与基线。
    ///
    /// 只在**首字下沉**那一段（每段至多一次）用；正文的行宽与行盒几何改走宽度表 +
    /// [`font_extents`](Self::font_extents)，不再每行 shaping（见 §7.5）。
    fn run_metrics(&self, text: &str, font: &FontKey, font_size: f32) -> LineMetrics;

    /// 一个 run 的基线上下 extent `(ascent, descent)`：**只跟字体与字号有关，与具体文本无关**
    /// （特殊情况下才因字体回退略有出入）。
    ///
    /// 行盒的上下 extent 用它算（正文的绘制宽度已由宽度表给出）；实现方应当覆盖它，
    /// 并且可以按 `(font, font_size)` 缓存 —— 一章里 distinct 的样式很少，量几次就够。
    /// 默认给「0.8 / 0.2 em」这一档常见比例，与空行 strut 同一档。
    fn font_extents(&self, _font: &FontKey, font_size: f32) -> (f32, f32) {
        (font_size * 0.8, font_size * 0.2)
    }

    /// 像素 x → run 内**字节**偏移（含头不含尾），与真实 shaping 同源
    /// （kerning、字体回退、标点压缩都算在内）。gpui-kit 的文本选择走的就是
    /// 这条路（`index_for_position`），命中才不会比绘制漂。
    /// 返回 `None` = 这份度量做不了 shaping，调用方退回 [`char_width`](Self::char_width)
    /// 累加（命中略漂但功能不断）。
    fn byte_at_x(&self, _text: &str, _font: &FontKey, _font_size: f32, _x: f32) -> Option<usize> {
        None
    }

    /// run 内每个字符**起点**的 x（与真实 shaping 同源），返回长度 = 字符数 + 1，
    /// 末项是整段宽度 —— 选中矩形的左右缘用它画，才能贴住绘制出来的字。
    /// `None` = 同上，退回 [`char_width`](Self::char_width) 累加。
    fn char_edges(&self, _text: &str, _font: &FontKey, _font_size: f32) -> Option<Vec<f32>> {
        None
    }
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
    /// 轻量模式：只算块高，不产出行盒。
    ///
    /// 用于目录跳转、进度条等只需要「这一章多高」的场合，不需要行盒级别的细节。
    /// 跳过断行、跳过 `run_metrics`、跳过 `LineFragment` 构建，速度提升一个量级。
    pub light: bool,
}

impl LayoutOptions {
    /// 完整排版（默认）。
    pub const FULL: Self = Self {
        width: 0.0,
        light: false,
    };

    /// 轻量排版（只算高）。
    pub const LIGHT: Self = Self {
        width: 0.0,
        light: true,
    };

    /// 指定宽度的完整排版。
    pub fn with_width(width: f32) -> Self {
        Self {
            width,
            light: false,
        }
    }
}
/// 排版 `root` 的全部内容。`root` 通常是 `<body>`：它的 padding 生效，
/// 字号与字体取它自己的计算样式（读者偏好已经在 `cascade` 的根样式里注入）。
///
/// `images` 提供 `<img>` 的固有尺寸与解析后的路径；没有图片时传 [`NoImages`]。
///
/// span 开着到函数返回，所以 `logging` 把 `folio_core::layout` 开到 debug 时，日志里
/// 每个 span 一行 `close time.busy=…` —— 排版慢在哪一章，靠的就是这一句。
#[tracing::instrument(level = "debug", name = "layout", skip_all, fields(width = %options.width))]
pub fn layout(
    dom: &Dom,
    styles: &StyledDom,
    root: NodeId,
    options: LayoutOptions,
    measure: &dyn Measure,
    images: &dyn ImageProvider,
) -> Layout {
    layout_inner(dom, styles, root, options, measure, images, None)
}

/// 带**块级缓存**的排版入口。对应 KOReader 的 `_renderedBlockCache`。
///
/// `dom_hash` 只用来做一道保险：DOM 变了（换章 / 改正文）就整清缓存（`NodeId` 只在
/// 同一份 DOM 里才有意义）。样式变了**不用**清 —— 每个块的键含它自己子树的样式哈希，
/// 只有样式真的变了的块会失效，其余照旧命中。这正是「改一个块只重排一块」的落点。
///
/// 调用方负责持有并复用同一个 `BlockCache`（跨多次 `layout_with_blocks`）；换章请调
/// [`BlockCache::clear`]（或换个 `dom_hash` 让它自动清）。
#[allow(clippy::too_many_arguments)]
pub fn layout_with_blocks(
    cache: &mut BlockCache,
    dom: &Dom,
    styles: &StyledDom,
    root: NodeId,
    options: LayoutOptions,
    measure: &dyn Measure,
    images: &dyn ImageProvider,
    dom_hash: u64,
) -> Layout {
    cache.sync_dom(dom_hash);
    layout_inner(dom, styles, root, options, measure, images, Some(cache))
}

/// 排版主流程。`cache` 为 `None` 时不走块级复用（[`layout`] 的路径）。
fn layout_inner(
    dom: &Dom,
    styles: &StyledDom,
    root: NodeId,
    options: LayoutOptions,
    measure: &dyn Measure,
    images: &dyn ImageProvider,
    cache: Option<&mut BlockCache>,
) -> Layout {
    // 子树样式哈希只在开了缓存时才算 —— 它是一趟额外的 DOM 遍历。
    let hashes = cache.as_ref().map(|_| subtree_style_hash(dom, styles));
    let mut ctx = Ctx {
        dom,
        styles,
        measure,
        images,
        light: options.light,
        blocks: Vec::new(),
        lines: Vec::new(),
        cache,
        style_hash: hashes.as_deref(),
    };
    let style = ctx.style_of(root);
    let pl = style.padding_left.max(0.0);
    let pt = style.padding_top.max(0.0);
    let content_width = (options.width - pl - style.padding_right.max(0.0)).max(1.0);
    let height = ctx.children(root, pl, pt, content_width);
    let out = Layout {
        blocks: ctx.blocks,
        lines: ctx.lines,
        width: content_width,
        height: pt + height,
    };
    // 块数与行数是一章排版结果的体检指标：0 行意味着 DOM 或样式空了，而不是「这一章短」。
    tracing::debug!(
        "layout: {} 块 / {} 行，版心 {}，高 {}",
        out.blocks.len(),
        out.lines.len(),
        out.width,
        out.height
    );
    out
}

/// 两阶段排版的第一阶段产物：整章「只算高」得到的**顶层渲染单元表** + 整章高。
///
/// 对应 KOReader 的 render 阶段（`requestLightFormatting`）：只算几何、不建行盒，
/// 供分页 / 滚动 / 进度用。走 `light` 走法，一章 0.1 ms 量级。
pub struct ChapterMeasure {
    root: NodeId,
    /// 触发测量时的原始页宽（窗口宽）；退化整章重排要用。
    page_width: f32,
    /// 版心宽（内容宽）。
    pub width: f32,
    /// 整章高。
    pub height: f32,
    /// 顶层流内单元，文档序；`y` 是绝对值。
    units: Vec<RenderUnit>,
    /// 顶层是不是「纯块级」。有匿名行内内容时为 `false`（见 [`render_window`]）。
    simple: bool,
}

/// 一个顶层渲染单元：一个块级子节点及其在章内的位置与（估算）高。
struct RenderUnit {
    node: NodeId,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

impl ChapterMeasure {
    /// 落在 `y` 处的顶层单元的节点 —— 「跳到某处」把 y 解回 DOM 节点用。
    pub fn block_at(&self, y: f32) -> Option<NodeId> {
        self.units
            .iter()
            .find(|u| y >= u.y && y < u.y + u.height)
            .map(|u| u.node)
    }
}

/// 阶段一：整章只算高。等价于 [`layout`] 的 `light` 走法，但把**顶层单元**的位置记下来，
/// 供 [`render_window`] 只重排可见窗口。
///
/// 只处理「顶层纯块级」的常见章节；顶层混有匿名行内内容时标为退化，`render_window`
/// 会退回整章完整排版（正确但没了增量收益）。
pub fn measure_chapter(
    dom: &Dom,
    styles: &StyledDom,
    root: NodeId,
    options: LayoutOptions,
    measure: &dyn Measure,
    images: &dyn ImageProvider,
) -> ChapterMeasure {
    let mut ctx = Ctx {
        dom,
        styles,
        measure,
        images,
        light: true,
        blocks: Vec::new(),
        lines: Vec::new(),
        cache: None,
        style_hash: None,
    };
    let style = ctx.style_of(root);
    let pl = style.padding_left.max(0.0);
    let pt = style.padding_top.max(0.0);
    let pb = style.padding_bottom.max(0.0);
    let content_width = (options.width - pl - style.padding_right.max(0.0)).max(1.0);

    let mut units = Vec::new();
    let mut cursor = pt;
    let mut prev_bottom = 0.0f32;
    let mut simple = true;
    for &child in ctx.children_of(root) {
        let s = ctx.style_of(child);
        if s.display == Display::None {
            continue;
        }
        // 顶层出现匿名行内内容就切不动块了，标成退化。
        if ctx.is_inline_flow(child, s) {
            simple = false;
            break;
        }
        let ml = s.margin_left.max(0.0);
        let mr = s.margin_right.max(0.0);
        let mt = s.margin_top.max(0.0);
        // 与 `children_light` 同一条外边距折叠：兄弟间取较大值，不叠加。
        cursor += if prev_bottom > 0.0 {
            mt.max(prev_bottom)
        } else {
            mt
        };
        let cw = (content_width - ml - mr).max(1.0);
        let height = ctx.block_light(child, cw);
        units.push(RenderUnit {
            node: child,
            x: pl + ml,
            y: cursor,
            width: cw,
            height,
        });
        cursor += height;
        prev_bottom = s.margin_bottom.max(0.0);
    }
    if !simple {
        units.clear();
    }
    ChapterMeasure {
        root,
        page_width: options.width,
        width: content_width,
        height: if simple { cursor + pb } else { 0.0 },
        units,
        simple,
    }
}

/// 阶段二：只对与 `[from_y, to_y)` 相交的顶层单元做**完整排版**
/// （对应 KOReader 的 draw 阶段：只对可见页涉及的块做对齐 / shaping / 装饰）。
///
/// 首个相交单元锚在测得的 `y` 上，其余按**实际**排出的高依次下推、并保持章内间距，
/// 所以窗口内部是自洽的（不会重叠）。返回的 [`Layout`] 只含这个窗口的行盒，
/// `height` 仍是整章高（来自 [`measure_chapter`]）。
///
/// 顶层有匿名行内内容（`simple == false`）时退化为整章完整排版。
pub fn render_window(
    chapter: &ChapterMeasure,
    dom: &Dom,
    styles: &StyledDom,
    measure: &dyn Measure,
    images: &dyn ImageProvider,
    from_y: f32,
    to_y: f32,
) -> Layout {
    if !chapter.simple {
        return layout(
            dom,
            styles,
            chapter.root,
            LayoutOptions {
                width: chapter.page_width,
                light: false,
            },
            measure,
            images,
        );
    }
    let mut ctx = Ctx {
        dom,
        styles,
        measure,
        images,
        light: false,
        blocks: Vec::new(),
        lines: Vec::new(),
        cache: None,
        style_hash: None,
    };
    let mut bottom = 0.0f32;
    let mut prev: Option<&RenderUnit> = None;
    for unit in &chapter.units {
        if unit.y + unit.height < from_y {
            continue; // 整块在窗口上方
        }
        if unit.y > to_y {
            break; // 整块在窗口下方，后面更靠下
        }
        let top = match prev {
            None => unit.y, // 首个相交单元锚在测得的 y 上
            // 上一个实际底边 + 章内间距（map 里这个间距就是外边距折叠的结果）
            Some(p) => bottom + (unit.y - (p.y + p.height)),
        };
        let h = ctx.block(unit.node, unit.x, top, unit.width);
        bottom = top + h;
        prev = Some(unit);
    }
    Layout {
        blocks: ctx.blocks,
        lines: ctx.lines,
        width: chapter.width,
        height: chapter.height,
    }
}

/// 排版结果缓存。
///
/// 缓存键 = `(内容宽, 偏好 hash, DOM hash, 样式 hash)`。
/// 任一维度变了就重排。对应 KOReader 的 `checkRenderContext()`。
pub struct LayoutCache {
    cached: Option<CacheEntry>,
}

struct CacheEntry {
    width: f32,
    prefs_hash: u64,
    styles_hash: u64,
    dom_hash: u64,
    light: bool,
    layout: Layout,
}

impl LayoutCache {
    pub fn new() -> Self {
        Self { cached: None }
    }

    /// 检查缓存是否存在（不返回引用，避免借用冲突）。
    pub fn has(
        &self,
        width: f32,
        prefs_hash: u64,
        styles_hash: u64,
        dom_hash: u64,
        light: bool,
    ) -> bool {
        self.cached.as_ref().is_some_and(|entry| {
            entry.width == width
                && entry.prefs_hash == prefs_hash
                && entry.styles_hash == styles_hash
                && entry.dom_hash == dom_hash
                && entry.light == light
        })
    }

    /// 取缓存。参数不变时直接复用。
    pub fn get(
        &self,
        width: f32,
        prefs_hash: u64,
        styles_hash: u64,
        dom_hash: u64,
        light: bool,
    ) -> Option<&Layout> {
        self.cached.as_ref().and_then(|entry| {
            if entry.width == width
                && entry.prefs_hash == prefs_hash
                && entry.styles_hash == styles_hash
                && entry.dom_hash == dom_hash
                && entry.light == light
            {
                Some(&entry.layout)
            } else {
                None
            }
        })
    }

    /// 存缓存。
    pub fn put(
        &mut self,
        width: f32,
        prefs_hash: u64,
        styles_hash: u64,
        dom_hash: u64,
        light: bool,
        layout: Layout,
    ) {
        self.cached = Some(CacheEntry {
            width,
            prefs_hash,
            styles_hash,
            dom_hash,
            light,
            layout,
        });
    }

    /// 失效（换章、改样式时调用）。
    pub fn invalidate(&mut self) {
        self.cached = None;
    }
}

impl Default for LayoutCache {
    fn default() -> Self {
        Self::new()
    }
}

/// 块级排版缓存的键：**块节点 + 该块子树的样式哈希 + 宽度**。
///
/// 子树样式哈希（[`subtree_style_hash`]）让「改一个块的样式」只改它自己的哈希，
/// 从而只失效那一个块（它的祖先虽然哈希也变了，但祖先重排时会逐个问子节点，
/// 没变的子节点照样命中）。宽度按位存，因为 `f32` 不能当 `HashMap` 键。
///
/// 键里**不含 DOM 内容**：DOM 变了（换章 / 改正文）必须 [`BlockCache::clear`]，
/// [`layout_with_blocks`] 会用 `dom_hash` 做这道保险，变更时自动清空。
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct BlockKey {
    node: NodeId,
    style_hash: u64,
    width: u32,
}

/// 一块排好后的产物，坐标**相对该块边框盒左上角**（复用时要平移到新位置）。
///
/// `lines` 的区间在 `blocks` 里是相对上面这张 `lines` 表的（0 起）—— 复用时要整体
/// 加上落点偏移，`blocks` 的 `lines` 区间再平移到 `Layout::lines` 里。
struct CachedBlock {
    lines: Vec<LineBox>,
    blocks: Vec<BlockBox>,
    height: f32,
}

/// 块级排版缓存。对应 KOReader 的 `_renderedBlockCache`。
///
/// 与整章粒度的 [`LayoutCache`] 互补：整章缓存「什么都变了就全重排」，这一层
/// 「只有某些块的样式变了就只重排那些块」。批注高亮（改一个块的样式）就是它的主场。
///
/// **默认不限容量**（换章时 [`clear`](Self::clear) 整清，所以上限就是一章的块数）。
/// 这里刻意不照搬 KOReader 的「256 条 LRU」：一次整章重排是**顺序扫过所有块**，只要
/// 容量小于块数，LRU 就会在下一趟把每块都淘汰掉 —— 命中率归零（实测 600 段落一章
/// 全未命中）。想限容量用 [`with_capacity`](Self::with_capacity)，那时才启用 LRU。
pub struct BlockCache {
    entries: HashMap<BlockKey, CachedBlock>,
    /// LRU 顺序，最近用过的在末尾。只在**限容量**时维护（不限容量时永不淘汰）。
    order: VecDeque<BlockKey>,
    /// `None` = 不限容量。
    cap: Option<usize>,
    /// 建缓存时那份 DOM 的哈希；对不上就整清（NodeId 只在同一份 DOM 里才有意义）。
    dom: Option<u64>,
    /// 命中 / 未命中次数，测试与基准用来核对「只重排了一块」。
    pub hits: u64,
    pub misses: u64,
}

impl BlockCache {
    /// 不限容量（推荐）：上限是一章的块数，换章时整清。
    pub fn new() -> Self {
        Self::with_capacity_opt(None)
    }

    /// 限容量（LRU 淘汰）。**注意**：容量小于一章块数时，整章顺序重排会来回淘汰、
    /// 命中率归零，所以只在「工作集确实小」时用。
    pub fn with_capacity(cap: usize) -> Self {
        Self::with_capacity_opt(Some(cap.max(1)))
    }

    fn with_capacity_opt(cap: Option<usize>) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            cap,
            dom: None,
            hits: 0,
            misses: 0,
        }
    }

    /// 整清（换章 / 改 DOM 时调用）。命中计数保留，方便跨轮观察。
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.dom = None;
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// DOM 换了就整清。返回是否有过清理。
    fn sync_dom(&mut self, dom_hash: u64) -> bool {
        if self.dom != Some(dom_hash) {
            self.entries.clear();
            self.order.clear();
            self.dom = Some(dom_hash);
            true
        } else {
            false
        }
    }

    fn touch(&mut self, key: &BlockKey) {
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
        }
        self.order.push_back(*key);
    }

    /// 取一块（克隆一份给调用方）；限容量时顺带把它挪到 LRU 末尾。
    fn get(&mut self, key: &BlockKey) -> Option<CachedBlock> {
        let out = self.entries.get(key).map(|entry| CachedBlock {
            lines: entry.lines.clone(),
            blocks: entry.blocks.clone(),
            height: entry.height,
        })?;
        if self.cap.is_some() {
            self.touch(key);
        }
        Some(out)
    }

    /// 存一块；限容量且超上限时从 LRU 头部淘汰。
    fn put(&mut self, key: BlockKey, block: CachedBlock) {
        let fresh = self.entries.insert(key, block).is_none();
        let Some(cap) = self.cap else {
            return; // 不限容量：不维护 LRU，也不淘汰
        };
        if fresh {
            self.touch(&key);
        }
        while self.entries.len() > cap {
            match self.order.pop_front() {
                Some(old) => {
                    self.entries.remove(&old);
                }
                None => break,
            }
        }
    }
}

impl Default for BlockCache {
    fn default() -> Self {
        Self::new()
    }
}

/// 每个节点「自己 + 整棵子树」的样式哈希，供 [`BlockCache`] 做键。
///
/// 后序聚合：`h(node) = hash(own_style(node), h(child₁), h(child₂), …)`。用显式栈做后序，
/// 不依赖「子节点下标一定大于父节点」这种隐含假设（折叠/注入会不会破坏它不好保证）。
/// 一次 `layout_with_blocks` 算一遍，约等于多遍历一次 DOM —— 相对整章排版可忽略。
fn subtree_style_hash(dom: &Dom, styles: &StyledDom) -> Vec<u64> {
    use std::hash::{Hash, Hasher};
    let mut preorder: Vec<NodeId> = Vec::with_capacity(dom.nodes.len());
    let mut stack = vec![dom.document];
    while let Some(node) = stack.pop() {
        preorder.push(node);
        for &child in &dom.get(node).children {
            stack.push(child);
        }
    }
    let mut out = vec![0u64; dom.nodes.len()];
    // 前序的**逆序**是一个合法后序：任一节点的子树在逆序里都排在它自己前面。
    for &node in preorder.iter().rev() {
        let mut hasher = std::hash::DefaultHasher::new();
        styles.get(node).unwrap_or(&DEFAULT_STYLE).hash(&mut hasher);
        for &child in &dom.get(node).children {
            out[child as usize].hash(&mut hasher);
        }
        out[node as usize] = hasher.finish();
    }
    out
}

/// 带缓存的排版入口。
///
/// 参数不变时直接复用上一次的结果。对应 KOReader 的 `checkRenderContext()`。
#[allow(clippy::too_many_arguments)]
pub fn layout_cached<'a>(
    cache: &'a mut LayoutCache,
    dom: &Dom,
    styles: &StyledDom,
    root: NodeId,
    options: LayoutOptions,
    measure: &dyn Measure,
    images: &dyn ImageProvider,
    prefs_hash: u64,
    styles_hash: u64,
    dom_hash: u64,
) -> &'a Layout {
    let is_hit = cache.has(
        options.width,
        prefs_hash,
        styles_hash,
        dom_hash,
        options.light,
    );
    if !is_hit {
        let layout = layout(dom, styles, root, options, measure, images);
        cache.put(
            options.width,
            prefs_hash,
            styles_hash,
            dom_hash,
            options.light,
            layout,
        );
    }
    cache
        .get(
            options.width,
            prefs_hash,
            styles_hash,
            dom_hash,
            options.light,
        )
        .expect("just put")
}

/// 计算读者偏好的 hash（用于缓存键）。
pub fn prefs_hash(prefs: &crate::config::Reader) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    prefs.font_size.to_bits().hash(&mut hasher);
    prefs.line_height.to_bits().hash(&mut hasher);
    prefs.margin_x.to_bits().hash(&mut hasher);
    prefs.max_measure.to_bits().hash(&mut hasher);
    prefs.indent_em.to_bits().hash(&mut hasher);
    prefs.justify.hash(&mut hasher);
    prefs.font_family.hash(&mut hasher);
    hasher.finish()
}

/// 计算样式表的 hash（用于缓存键）。
pub fn styles_hash(styles: &StyledDom) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    styles.len().hash(&mut hasher);
    for (id, style) in styles.iter().take(100) {
        id.hash(&mut hasher);
        style.font_size.to_bits().hash(&mut hasher);
        style.font_weight.hash(&mut hasher);
        style.display.hash(&mut hasher);
    }
    hasher.finish()
}

/// 计算 DOM 结构的 hash（用于缓存键）。
pub fn dom_hash(dom: &Dom, root: NodeId) -> u64 {
    use std::hash::Hasher;
    let mut hasher = std::hash::DefaultHasher::new();
    hash_node(dom, root, &mut hasher);
    hasher.finish()
}

fn hash_node(dom: &Dom, node: NodeId, hasher: &mut std::hash::DefaultHasher) {
    use std::hash::Hash;
    node.hash(hasher);
    let dom_node = dom.get(node);
    match dom_node.kind {
        NodeKind::Text => {
            let text = &dom_node.text;
            if text.len() > 200 {
                // 只能切在字符边界上：`&text[..200]` 对中文会直接 panic。
                // 取前 64 个字符（足够检测变化），再混入总长。
                let head: String = text.chars().take(64).collect();
                head.hash(hasher);
                text.len().hash(hasher);
            } else {
                text.hash(hasher);
            }
        }
        NodeKind::Element => {
            dom.tag_name(node).hash(hasher);
            let child_count = dom_node.children.len();
            child_count.hash(hasher);
            for &child in dom_node.children.iter().take(50) {
                hash_node(dom, child, hasher);
            }
        }
        // Document / Comment 节点不影响排版，跳过。
        NodeKind::Document | NodeKind::Comment => {}
    }
}

/// `style_of` 查不到时给的兵底样式。正常不会用到 —— `StyledDom` 为每个节点都算了一条。
static DEFAULT_STYLE: LazyLock<ComputedStyle> = LazyLock::new(ComputedStyle::default);

struct Ctx<'a> {
    dom: &'a Dom,
    styles: &'a StyledDom,
    measure: &'a dyn Measure,
    images: &'a dyn ImageProvider,
    /// 轻量模式：只算块高，不产出行盒。
    light: bool,
    blocks: Vec<BlockBox>,
    lines: Vec<LineBox>,
    /// 块级缓存；`None` = 不做块级复用（[`layout`] 的默认路径）。
    cache: Option<&'a mut BlockCache>,
    /// 每节点的子树样式哈希，与 `cache` 同生共死（见 [`subtree_style_hash`]）。
    style_hash: Option<&'a [u64]>,
}

/// 一段行内内容展平后的产物。
struct Collected {
    text: String,
    pieces: Vec<Piece>,
    /// 文本流里的图片占位。`at` 指向 `text` 里那个哨兵字符的字节起点。
    images: Vec<ImageAtom>,
    /// 这一段用到的所有 run 样式，按值去重。`Piece::style` 是它的下标。
    styles: StyleArena,
    /// 每个字符的前进量，按 `text` 的**字节下标**索引（非字符起点的字节是 0）。
    ///
    /// 断行与已用宽重算本来是逐字符做 `piece_at` 二分 + 一次 `char_width` 虚调用；
    /// 预算成这张表之后两处都只是一次数组取值。长度 = `text.len()`，与字节下标一一对应，
    /// 所以取的时候不必再判字符边界。
    widths: Vec<f32>,
}

/// 一个文本节点对段落的贡献：段内区间、原节点区间、以及**该文本所属元素**的样式。
struct Piece {
    node: NodeId,
    norm: Range<usize>,
    orig: Range<usize>,
    /// 样式下标，指向 `Collected::styles`。
    ///
    /// 以前这里是 `Rc<RunStyle>`（一份样式一个堆分配 + 每次克隆一次引用计数）；
    /// 去重后同一样式只存一次，片段只占 4 字节。
    style: StyleRef,
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

    /// 按字节下标取它所属的样式。宽度表已经预先算好了前进量，所以断行不再走这里；
    /// 保留它给绘制、命中、`::first-letter` 那几条按位置查样式的路径。
    #[allow(dead_code)]
    fn style_at(&self, at: usize) -> Option<&RunStyle> {
        let piece = self.piece_at(at)?;
        self.styles.get(self.pieces[piece].style)
    }

    /// 字节下标 `at` 处那个字符的前进量。超出表长给 0。
    ///
    /// 表在 [`collect`] 那一趟里已经填好了（见 [`Collector::push_widths`]），这里只读。
    fn width_at(&self, at: usize) -> f32 {
        self.widths.get(at).copied().unwrap_or(0.0)
    }

    /// 正好落在 `at` 上的图片。
    #[allow(dead_code)] // 图片宽度已进 `widths`，这里留给命中/绘制路径
    fn image_at(&self, at: usize) -> Option<&ImageAtom> {
        let idx = self.images.partition_point(|im| im.at < at);
        self.images.get(idx).filter(|im| im.at == at)
    }
}

impl<'a> Ctx<'a> {
    /// 节点的计算样式。
    ///
    /// 返回**引用**而不是克隆：`Ctx.styles` 是 `&'a StyledDom`，所以这个引用的生命周期
    /// 独立于 `&self` —— 拿到它之后还能继续调 `&mut self` 的方法（排版里每层都要这么干），
    /// 而克隆一份 `ComputedStyle`（内含 `String` + `Vec<String>`）在每章是几千次分配。
    ///
    /// `StyledDom` 为每个节点（含文本节点）都算了一条，所以查不到只可能是节点下标越界；
    /// 那种时候给一份静态默认样式，而不是 panic。
    fn style_of(&self, node: NodeId) -> &'a ComputedStyle {
        self.styles.get(node).unwrap_or(&DEFAULT_STYLE)
    }

    /// 节点的子节点列表。同上：借出 `Dom` 里的那一份，不克隆 `Vec`。
    fn children_of(&self, node: NodeId) -> &'a [NodeId] {
        &self.dom.get(node).children
    }

    /// 摆放 `container` 的流内子内容。`x`/`y` 是其内容盒左上角（绝对），返回内容高度。
    fn children(&mut self, container: NodeId, x: f32, y: f32, width: f32) -> f32 {
        if self.light {
            return self.children_light(container, x, y, width);
        }
        let container_style = self.style_of(container);
        let mut cursor = y;
        let mut prev_bottom = 0f32;
        let mut pending: Vec<NodeId> = Vec::new();

        for &child in self.children_of(container) {
            let style = self.style_of(child);
            if style.display == Display::None {
                continue;
            }
            if self.is_inline_flow(child, style) {
                pending.push(child);
                continue;
            }
            // 遇到块级子节点，先把攒着的行内内容冲成一个匿名块。
            if !pending.is_empty() {
                let pieces = std::mem::take(&mut pending);
                cursor +=
                    self.wrap_paragraph(container, &pieces, x, cursor, width, container_style);
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
            cursor += self.wrap_paragraph(container, &pending, x, cursor, width, container_style);
        }
        cursor - y
    }

    /// 轻量模式：只算块高，不断行。
    ///
    /// 跳过 `wrap_paragraph` / `run_metrics` / `LineFragment` 构建。
    /// 文本块按「字符数 / 每行字数」估算行数，乘以行高得到高度。
    fn children_light(&mut self, container: NodeId, x: f32, y: f32, width: f32) -> f32 {
        let mut cursor = y;
        let mut prev_bottom = 0f32;

        for &child in self.children_of(container) {
            let style = self.style_of(child);
            if style.display == Display::None {
                continue;
            }
            let ml = style.margin_left.max(0.0);
            let mr = style.margin_right.max(0.0);
            let mt = style.margin_top.max(0.0);
            let mb = style.margin_bottom.max(0.0);
            cursor += if prev_bottom > 0.0 {
                mt.max(prev_bottom)
            } else {
                mt
            };

            let content_width = (width - ml - mr).max(1.0);
            let height = self.block_light(child, content_width);

            // 记录块（无行盒）。
            self.blocks.push(BlockBox {
                node: child,
                x: x + ml,
                y: cursor,
                width,
                height,
                text: String::new(),
                lines: 0..0,
            });
            cursor += height;
            prev_bottom = mb;
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
        self.children_of(node).iter().any(|&child| {
            let style = self.style_of(child);
            style.display != Display::None && !self.is_inline_flow(child, style)
        })
    }

    /// 摆放一个块级元素，返回边框盒高度。开了块级缓存时先查缓存、未命中才实排。
    fn block(&mut self, node: NodeId, x: f32, y: f32, width: f32) -> f32 {
        let Some(key) = self.block_key(node, width) else {
            return self.block_uncached(node, x, y, width);
        };
        if let Some(height) = self.reuse_block(key, x, y) {
            return height;
        }
        let lines_start = self.lines.len();
        let blocks_start = self.blocks.len();
        let height = self.block_uncached(node, x, y, width);
        self.stash_block(key, lines_start, blocks_start, x, y, height);
        height
    }

    /// 这一块的缓存键；没开缓存时给 `None`（直接走实排）。
    fn block_key(&self, node: NodeId, width: f32) -> Option<BlockKey> {
        self.cache.as_ref()?;
        let style_hash = self.style_hash?.get(node as usize).copied().unwrap_or(0);
        Some(BlockKey {
            node,
            style_hash,
            width: width.to_bits(),
        })
    }

    /// 缓存命中：把缓存的块按 `(x, y)` 平移后并入 `self.lines`/`self.blocks`，返回块高。
    fn reuse_block(&mut self, key: BlockKey, x: f32, y: f32) -> Option<f32> {
        let cache = self.cache.as_deref_mut()?;
        let Some(entry) = cache.get(&key) else {
            cache.misses += 1;
            return None;
        };
        cache.hits += 1;
        let height = entry.height;
        let base = self.lines.len();
        for mut line in entry.lines {
            line.x += x;
            line.y += y;
            self.lines.push(line);
        }
        for mut block in entry.blocks {
            block.x += x;
            block.y += y;
            block.lines = (block.lines.start + base)..(block.lines.end + base);
            self.blocks.push(block);
        }
        Some(height)
    }

    /// 把刚排好的这一块（`self.lines`/`self.blocks` 的尾段）归一化成块局部坐标存进缓存。
    fn stash_block(
        &mut self,
        key: BlockKey,
        lines_start: usize,
        blocks_start: usize,
        x: f32,
        y: f32,
        height: f32,
    ) {
        let Some(cache) = self.cache.as_deref_mut() else {
            return;
        };
        let lines: Vec<LineBox> = self.lines[lines_start..]
            .iter()
            .map(|line| {
                let mut line = line.clone();
                line.x -= x;
                line.y -= y;
                line
            })
            .collect();
        let blocks: Vec<BlockBox> = self.blocks[blocks_start..]
            .iter()
            .map(|block| {
                let mut block = block.clone();
                block.x -= x;
                block.y -= y;
                block.lines = (block.lines.start - lines_start)..(block.lines.end - lines_start);
                block
            })
            .collect();
        cache.put(
            key,
            CachedBlock {
                lines,
                blocks,
                height,
            },
        );
    }

    /// 摆放一个块级元素（不做块级缓存），返回边框盒高度。
    fn block_uncached(&mut self, node: NodeId, x: f32, y: f32, width: f32) -> f32 {
        let style = self.style_of(node);
        // 盒模型从外到内一次算清：边框与内边距一起从可用宽里扣掉，正文与折行都在内容盒里
        // 算；外层那条 `push_block` 记的仍是**边框盒**（绘制背景与边框要用它）。
        let [bt, br, bb, bl] = style.border_drawn();
        let pl = bl + style.padding_left.max(0.0);
        let pr = br + style.padding_right.max(0.0);
        let pt = bt + style.padding_top.max(0.0);
        let pb = bb + style.padding_bottom.max(0.0);
        // 替换元素（`display: block` 的图片）：整块就是这张图，独立成行。
        if self.dom.is_element(node, "img") {
            let src = self.dom.attr(node, "src").map(str::to_string);
            let Some(img) = src.as_deref().and_then(|s| self.images.resolve(s)) else {
                return 0.0; // 图取不到就整块不留痕，不塌成空盒子
            };
            let content_width = (width - pl - pr).max(1.0);
            let (w, h) = image_size(style, img.width, img.height, content_width);
            let index = self.push_block(node, x, y, width, String::new());
            let offset = match style.text_align {
                TextAlign::Right => (content_width - w).max(0.0),
                TextAlign::Center => (content_width - w).max(0.0) / 2.0,
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
                x: x + pl + offset,
                y: y + pt,
                width: w,
                height: h,
                baseline: h,
                align: style.text_align,
            });
            return self.close_block(index, pt + h + pb);
        }

        let content_width = (width - pl - pr).max(1.0);
        let index = self.push_block(node, x, y, width, String::new());
        let inner = if self.has_block_children(node) {
            self.children(node, x + pl, y + pt, content_width)
        } else {
            let pieces = self.children_of(node);
            self.wrap_paragraph(node, pieces, x + pl, y + pt, content_width, style)
        };
        self.close_block(index, pt + inner + pb)
    }

    /// 轻量模式：算一个块的高，不断行。
    fn block_light(&mut self, node: NodeId, width: f32) -> f32 {
        let style = self.style_of(node);
        let [bt, br, bb, bl] = style.border_drawn();
        let pl = bl + style.padding_left.max(0.0);
        let pr = br + style.padding_right.max(0.0);
        let pt = bt + style.padding_top.max(0.0);
        let pb = bb + style.padding_bottom.max(0.0);

        // 替换元素（图片）：按固有尺寸算高。
        if self.dom.is_element(node, "img") {
            let src = self.dom.attr(node, "src").map(str::to_string);
            if let Some(img) = src.as_deref().and_then(|s| self.images.resolve(s)) {
                let content_width = (width - pl - pr).max(1.0);
                let (_, h) = image_size(style, img.width, img.height, content_width);
                return pt + h + pb;
            }
            return 0.0;
        }

        let content_width = (width - pl - pr).max(1.0);
        let inner = if self.has_block_children(node) {
            // 有块级子节点：递归。
            self.children_light(node, 0.0, 0.0, content_width)
        } else {
            // 纯文本块：按**估算总宽**除以版心宽估行数。用总宽（连续量）而不是「每行字数」
            // 的离散近似 —— 两阶段排版据此定位可见块，行数偏一行就会错位。
            let est = self.text_est_em(node) * style.font_size;
            let line_px = (style.font_size * style.line_height).max(1.0);
            let lines = (est / content_width).ceil().max(1.0);
            lines * line_px
        };
        pt + inner + pb
    }

    /// 估算一个节点下文本的总宽，单位是 em（用于轻量模式 / 两阶段量高）。
    ///
    /// 逐**字节**给一个近似宽：ASCII 空格 0.25 em、其余 ASCII 0.5 em、3 字节字符（CJK 等）
    /// 1 em、2 字节字符 0.5 em、续字节不计。以 0.25 em 为单位**整数**累计，比逐字符解码 +
    /// 浮点求和便宜（一章 0.3 ms 量级）。不是真字体度量，但比「一字 1 em」准得多 ——
    /// 后者会把纯拉丁段估宽一倍、行数翻番，两阶段据此定位可见块就会错位。
    fn text_est_em(&self, node: NodeId) -> f32 {
        let dom_node = self.dom.get(node);
        if dom_node.kind == NodeKind::Text {
            let mut quarters = 0u32; // 0.25 em 为单位
            for &b in dom_node.text.as_bytes() {
                quarters += if b < 0x80 {
                    if b == b' ' { 1 } else { 2 }
                } else if b >= 0xE0 {
                    4
                } else if b >= 0xC0 {
                    2
                } else {
                    0 // 0x80..0xC0：UTF-8 续字节
                };
            }
            return quarters as f32 / 4.0;
        }
        // 有块级子节点时递归会在 block_light 里处理，这里只算直接文本。
        if self.has_block_children(node) {
            return 0.0;
        }
        dom_node.children.iter().map(|&c| self.text_est_em(c)).sum()
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
            self.measure,
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
        let cap = self.drop_cap(collected, style, width);
        let mut plans: Vec<LinePlan> = Vec::new();
        let mut first = true;

        for hard in hard_lines(&collected.text) {
            let slice = collected.text.get(hard.clone()).unwrap_or("");
            // UAX #14：给出的下标是「下一行可以从这里开始」。
            let breaks: Vec<usize> = linebreaks(slice)
                .filter(|(ix, _)| *ix > 0 && *ix < slice.len())
                .map(|(ix, _)| hard.start + ix)
                // `hyphens: none` 的书不许在连字符处断，所以把那一批断点摘掉。
                .filter(|at| !hyphen_break_banned(&collected.text, *at, style.hyphens))
                .collect();
            // `cap` 在整段里都不变，先把「有没有」问一次，别每字符问。
            let cap_norm = cap.as_ref().map(|dc| dc.norm.clone());
            // 断点把硬行切成若干「词」（两个断点之间的连续段）：拉丁文里≈单词，
            // CJK 里退化成单字。稀疏（平均每词 ≥ 4 字节 —— 拉丁那种词边界断行）走词级
            // 扫描，每个词只做一次溢出判断；密集（CJK 几乎每字一个断点）或带下沉盒时走
            // 逐字符。两条路径输出逐行一致，这个分类只影响性能。
            let sparse = !breaks.is_empty() && slice.len() >= 4 * (breaks.len() + 1);
            // 词级扫描碰到「行首超长词」时把位置交回来，由逐字符那段从那儿接着硬切。
            let fallback = if cap.is_none() && sparse {
                scan_by_word(
                    collected,
                    &cap,
                    &breaks,
                    hard.clone(),
                    width,
                    indent,
                    &mut first,
                    &mut plans,
                )
            } else {
                None
            };
            if cap.is_some() || !sparse || fallback.is_some() {
                scan_by_char(
                    collected,
                    &cap,
                    &cap_norm,
                    &breaks,
                    fallback.unwrap_or(hard.start),
                    hard.end,
                    width,
                    indent,
                    &mut first,
                    &mut plans,
                );
            }
        }

        let mut cursor = y;
        // 这个块的第一行行盒的下标 —— 首字符的基线要贴着它。
        let first_line = self.lines.len();
        for plan in plans {
            cursor += self.emit_line(
                owner,
                collected,
                plan.range,
                LinePlace {
                    x,
                    y: cursor,
                    width,
                    indent: plan.indent,
                    reserved: plan.reserved,
                },
                cap.as_ref(),
                style,
            );
        }
        // 下沉的字比它占的那几行还长时（`line-height` 给得很小），尾巴会伸到段落底下，
        // 下一个块就会压在那半截字上。把这一段过 hang 补进段落高 —— 行盒本身不动，
        // 正文的基线位置因此不受影响。
        if let Some(dc) = &cap {
            let baseline = self
                .lines
                .get(first_line)
                .map(|line| line.y + line.baseline)
                .unwrap_or(y);
            cursor = cursor.max(baseline + dc.descent);
        }
        cursor - y
    }

    /// 首字下沉那一段的几何。三个条件缺一不可：这个块写过 `::first-letter`、
    /// 第一个硬行里找得到一个**字母单元**、并且量得出宽度。字母单元按 UAX #29 的说法
    /// 算（数字也算 —— 浏览器给 `1984` 的 `::first-letter` 上的就是那个 `1`），
    /// 全段一个字母单元都没有（纯标点、纯图片）就不造这个盒。
    ///
    /// 跨几行按字形的实际上下 extent 除以块自己的行盒高向上取整：用 extent 而不是
    /// inline box 高，是因为 `line-height: 0.9`（首字下沉的经典写法）会把盒高压到字形
    /// 以下，少算一行就让下一行的正文压在字的尾巴上。
    fn drop_cap(
        &self,
        collected: &Collected,
        style: &ComputedStyle,
        width: f32,
    ) -> Option<DropCap> {
        let cap = style.first_letter.as_deref()?;
        let head = collected.text.split('\n').next()?;
        let at = head
            .char_indices()
            .find(|(_, ch)| ch.is_alphabetic() || ch.is_ascii_digit())?
            .0;
        let end = at + head[at..].chars().next()?.len_utf8();
        // 下沉盒从段首起：紧贴首字母的标点按 CSS 也算在这个盒里面（`「開」` 的引号），
        // 而它们本来就被这一段涵盖了，不必再往回找一遍。
        let norm = 0..end;
        let text = collected.text.get(norm.clone())?;
        let mut run = RunStyle::new(cap);
        // 首字符就贴在第一行的基线上，`vertical-align` 对它没有意义；那个数会把
        // 绘制基线推走，所以清掉。
        run.baseline_shift = 0.0;
        let m = self.measure.run_metrics(text, &run.font, run.font_size);
        let line_px = (style.font_size * style.line_height).max(1.0);
        Some(DropCap {
            width: m.width.min(width * 0.6).max(1.0),
            ascent: m.ascent,
            descent: m.descent,
            lines: (((m.ascent + m.descent) / line_px).ceil() as usize).max(1),
            norm,
            run,
        })
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
        cap: Option<&DropCap>,
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
            // 空行走块的 strut：没有字形可量，就按块自己的字号与 line-height 撑出一行，
            // 否则 `<p><br></p>` 会塌成零高。上下 extent 用 0.8 / 0.2 这一档近似
            // （中文与多数拉丁字体的常见比例），半 leading 照同一个式子分。
            let block_style = RunStyle::new(style);
            let em = block_style.font_size;
            let half_leading = (block_style.box_height() - em) / 2.0;
            acc.ascent = em * 0.8 + half_leading;
            acc.descent = em * 0.2 + half_leading;
        }
        // 下沉的首字符排在片段最前面：绘制层从 `line.x` 起按片段宽度逐个右推，
        // 正文自然就从它右边开始，不必再给这一行加一次左边距。
        // 它的上下 extent 故意**不进** `acc.ascent` —— 基线必须留在正文那一份上，
        // 否则一行正文会被一个大字整行推下去（`float` 在 CSS 里正是这个意思）。
        if let Some(dc) = cap.filter(|_| place.reserved > 0.0)
            && let Some(index) = collected.piece_at(dc.norm.start)
        {
            let piece = &collected.pieces[index];
            acc.fragments.insert(
                0,
                LineFragment::Text(LineRun {
                    node: piece.node,
                    norm: dc.norm.clone(),
                    orig: orig_of(piece, &dc.norm),
                    style: dc.run.clone(),
                    width: dc.width,
                    ascent: dc.ascent,
                    descent: dc.descent,
                }),
            );
        }
        // 行盒 = 各片段（含自己的半 leading）到基线上下距离的并集。作者把 line-height
        // 收到字形本体之下时它照它缩，两行的字形因此叠在一起 —— 浏览器就是这么画的。
        // 夹一个 0 下限：行顶是按行盒高逐行推进的，负数会让后面的几何与分片失去单调性。
        let height = (acc.ascent + acc.descent).max(0.0);
        // 基线离行顶就是累积出来的上方 extent（每个片段自己那份 leading 已经算在里面）。
        let baseline = acc.ascent;
        // 可用宽要扣掉这一行让出去的左边距（首行的 `text-indent`、被首字符压着的
        // 那几行让开的一格），否则居中的标题会按整幅宽算偏移，把字推出版心。
        let avail = (place.width - place.indent - place.reserved).max(1.0);
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
            baseline,
            align: style.text_align,
        });
        height
    }
}

/// 一行在版心里的落位。
///
/// 四个数各自的意义：`x` 是版心左缘，`y` 是这一行行顶（逐行推进的游标），
/// `width` 是版心宽，`indent` 是这一行让出去的左边距 —— 首行是 `text-indent`，
/// 被下沉首字符压着的行还要再加上首字符的宽。左缘与对齐偏移都按 `x + indent` 起算，
/// 可用宽度另外再扣一次 `reserved`：那一格不是空白，是首字符**自己站在那儿**，
/// 所以它推不开这一行的正文（正文排在它右边），却实实在在写不了字。
#[derive(Clone, Copy)]
struct LinePlace {
    x: f32,
    y: f32,
    width: f32,
    indent: f32,
    reserved: f32,
}

/// 一个行盒的方案：正文的字节区间（首字符那一段已摘出去），加上落位要让开的两个数。
#[derive(Clone)]
struct LinePlan {
    range: Range<usize>,
    indent: f32,
    reserved: f32,
}

/// 下沉的首字符。`norm` 从段首起，含紧贴首字母的那些标点 —— CSS 的
/// `::first-letter` 盒就是这个范围（`「引号」里的字` 的引号也算进去）。
#[derive(Clone)]
struct DropCap {
    norm: Range<usize>,
    run: RunStyle,
    /// 画它占多宽，也是被压着的那几行要让开的宽度。
    width: f32,
    ascent: f32,
    descent: f32,
    /// 跨几行。按字形的实际上下 extent 除以块自己的行盒高，向上取整。
    lines: usize,
}

/// 第 `index` 行要让开的左边距，和「首字符站着、正文写不进去」的那一段宽。
/// 两个数都要从可用宽里扣，但落位不同：前者把整行推开，后者那一格站着首字符本身。
fn line_inset(cap: &Option<DropCap>, index: usize, first_of_hard: bool, indent: f32) -> (f32, f32) {
    let mut left = if first_of_hard { indent } else { 0.0 };
    let mut reserved = 0.0f32;
    if let Some(dc) = cap {
        if index == 0 {
            reserved = dc.width;
        } else if index < dc.lines {
            left += dc.width;
        }
    }
    (left, reserved)
}

fn plan_for(
    range: Range<usize>,
    cap: &Option<DropCap>,
    index: usize,
    first_of_hard: bool,
    indent: f32,
) -> LinePlan {
    let (left, reserved) = line_inset(cap, index, first_of_hard, indent);
    // 首字符盒只属于第一行；正文从它之后开始。
    let range = match cap {
        Some(dc) if index == 0 && range.start < dc.norm.end => {
            dc.norm.end.min(range.end)..range.end
        }
        _ => range,
    };
    LinePlan {
        range,
        indent: left,
        reserved,
    }
}

/// 一行的累积器：`[start, end)` 区间内按片段顺序拼接文本与图片，同时求行盒几何。
///
/// `ascent` / `descent` 存的是**各片段自己那份算完半 leading 之后**到基线的上下距离，
/// 也就是 CSS 的画法：每个行内盒占 `[基线 − ascent − half-leading, 基线 + descent +
/// half-leading]`，行盒是它们的并集。所以行盒高 = `ascent + descent`，不再单独存一份，
/// 也不用「取各盒最大」再和字形高度比大小 —— 那两种写法在混排字号或图片时都会算错。
#[derive(Default)]
struct LineAcc {
    fragments: Vec<LineFragment>,
    width: f32,
    ascent: f32,
    descent: f32,
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
            // 样式从 arena 取，每个 piece 只存了 4 字节下标。
            let style = collected
                .styles
                .get(piece.style)
                .expect("piece 的样式一定在 arena 里");
            // 绘制宽度直接取宽度表（与断行同源，且已含 `word-spacing`），不再每行重新 shaping。
            // 这是 §7.5：一行一个 run 就省一次 `run_metrics`，一章上万行。
            let width = sum_widths(collected, norm.clone());
            // 上下 extent 只跟字体/字号有关（`font_extents` 内部可按样式缓存），也不再 shaping。
            let (ascent, descent) = measure.font_extents(&style.font, style.font_size);
            // 半 leading 按**这个 piece 自己**的字号与 line-height 算：`(盒高 − 字形上下 extent) / 2`，
            // 可正可负。它同时决定这一行里基线离行顶/行底多远，所以松行距的字在行盒里居中、
            // 紧行距（作者写 125% 而字体自然盒 144% 那种）行盒就照作者缩，字形溢出到相邻行。
            // 基线偏移（`vertical-align`）再叠一次：往上抬就多吃上方、少占下方。
            let content = ascent + descent;
            let half_leading = (style.box_height() - content) / 2.0;
            let shift = style.baseline_shift;
            self.width += width;
            self.ascent = self.ascent.max(ascent + half_leading + shift);
            self.descent = self.descent.max(descent + half_leading - shift);
            self.fragments.push(LineFragment::Text(LineRun {
                node: piece.node,
                orig: orig_of(piece, &norm),
                norm,
                style: style.clone(),
                width,
                ascent,
                descent,
            }));
        }
    }

    /// 图片底贴基线：它的整高就是本行基线以上的距离（行内替换盒没有 leading 可言）。
    fn push_image(&mut self, img: &ImageAtom) {
        self.width += img.width;
        self.ascent = self.ascent.max(img.height);
        self.fragments.push(LineFragment::Image(ImageRun {
            node: img.node,
            href: img.href.clone(),
            width: img.width,
            height: img.height,
        }));
    }
}

/// 宽度表里 `range` 这一段的总宽。续字节是 0，所以连续切片求和就够（且可向量化）。
fn sum_widths(collected: &Collected, range: Range<usize>) -> f32 {
    collected.widths.get(range).map_or(0.0, |s| s.iter().sum())
}

/// 硬行扫描收尾：把最后一段收成一行。空硬行（`<br><br>`）也要占一行高，否则会塌掉。
/// `from` 是这次扫描的起点（整行扫描时是硬行首，逐字符兜底时是被交回的断点）。
fn finish_hard_line(
    cap: &Option<DropCap>,
    from: usize,
    hard_end: usize,
    line_start: usize,
    first: &mut bool,
    indent: f32,
    plans: &mut Vec<LinePlan>,
) {
    if line_start < hard_end {
        plans.push(plan_for(
            line_start..hard_end,
            cap,
            plans.len(),
            *first,
            indent,
        ));
        *first = false;
    } else if from == hard_end {
        plans.push(plan_for(from..hard_end, cap, plans.len(), *first, indent));
        *first = false;
    }
}

/// 逐字符扫描硬行区间 `[from, hard_end)`，把折出的行方案追加进 `plans`。
///
/// 这是断行的基准实现：密集行（CJK）、带下沉盒的段落、以及词级扫描碰到超长词时的
/// 兜底都走它。`breaks` 是绝对字节位置的合法断点表；`cap_norm` 是下沉盒覆盖的字节范围。
/// 溢出时断在「最后一个不晚于当前位置的合法断点」，没有断点可选就退回硬切（`unwrap_or(at)`）。
#[allow(clippy::too_many_arguments)]
fn scan_by_char(
    collected: &Collected,
    cap: &Option<DropCap>,
    cap_norm: &Option<Range<usize>>,
    breaks: &[usize],
    from: usize,
    hard_end: usize,
    width: f32,
    indent: f32,
    first: &mut bool,
    plans: &mut Vec<LinePlan>,
) {
    let mut line_start = from;
    let mut used = 0.0f32;
    // `line_inset` 只跟「第几行」有关，一行之内不变。提到循环外，不然每个字符都要
    // 重算一次缩进与保留宽 —— 那是热路上最贵的一个函数调用。首行让出去的宽度由
    // `avail` 扣，不再在 `used` 里加一遍（两处都算会让首行少容两个字）。
    let mut inset = line_inset(cap, plans.len(), *first, indent);
    let mut avail = (width - inset.0 - inset.1).max(1.0);
    let slice = collected.text.get(from..hard_end).unwrap_or("");
    for (i, _ch) in slice.char_indices() {
        let at = from + i;
        // 下沉的那一段不进这一行的累加宽度：它自己成一个片段摆在行首。
        if cap_norm.as_ref().is_some_and(|norm| norm.contains(&at)) {
            continue;
        }
        // 宽度表按字节下标存，图片哨兵也已经写进去了。
        let w = collected.width_at(at);
        if at > line_start && used + w > avail {
            let cut = last_break_before(breaks, at, line_start).unwrap_or(at);
            plans.push(plan_for(line_start..cut, cap, plans.len(), *first, indent));
            line_start = cut;
            used = sum_widths(collected, cut..at) + w;
            *first = false;
            // 换了行 → 重算这一行让出去的宽。
            inset = line_inset(cap, plans.len(), *first, indent);
            avail = (width - inset.0 - inset.1).max(1.0);
        } else {
            used += w;
        }
    }
    finish_hard_line(cap, from, hard_end, line_start, first, indent, plans);
}

/// 按「词」扫描一个硬行，把行方案追加进 `plans`。
///
/// 词 = 两个断点之间的连续段（拉丁文≈单词，CJK 退化成单字）；词宽用宽度表的连续
/// 切片求和，每个词只做一次溢出判断，把「逐字符判溢出」降成「逐词判溢出」。
///
/// 溢出发生在某个词内部时，逐字符实现里 `last_break_before` 返回的正是**该词词首**
/// （词首就是一个断点），所以在词首断行与逐字符结果一致。唯一例外是「行首超长词」
/// （单个词就宽过整行）：这时返回 `Some(位置)`，交给 [`scan_by_char`] 从该位置继续
/// 硬切（复现「排不下就硬切」的兜底）；正常走完返回 `None`。
#[allow(clippy::too_many_arguments)]
fn scan_by_word(
    collected: &Collected,
    cap: &Option<DropCap>,
    breaks: &[usize],
    hard: Range<usize>,
    width: f32,
    indent: f32,
    first: &mut bool,
    plans: &mut Vec<LinePlan>,
) -> Option<usize> {
    // 虚拟边界表：0 → 硬行首，1..=breaks.len() → 各断点，末项 → 硬行尾。
    let boundary = |j: usize| -> usize {
        if j == 0 {
            hard.start
        } else if j - 1 < breaks.len() {
            breaks[j - 1]
        } else {
            hard.end
        }
    };
    let atoms = breaks.len() + 1;
    let mut line_start = hard.start;
    let mut used = 0.0f32;
    let mut inset = line_inset(cap, plans.len(), *first, indent);
    let mut avail = (width - inset.0 - inset.1).max(1.0);
    let mut j = 0;
    while j < atoms {
        let s = boundary(j);
        let e = boundary(j + 1);
        let w = sum_widths(collected, s..e);
        if s > line_start && used + w > avail {
            // 整个词放不下且本行已有内容 → 在词首断行。
            plans.push(plan_for(line_start..s, cap, plans.len(), *first, indent));
            line_start = s;
            used = 0.0;
            *first = false;
            inset = line_inset(cap, plans.len(), *first, indent);
            avail = (width - inset.0 - inset.1).max(1.0);
            // 不推进 `j`：让同一个词在新行上重新评估。
            continue;
        }
        if s == line_start && used + w > avail {
            // 行首的一个词就宽过整行 → 交回逐字符硬切。
            return Some(line_start);
        }
        used += w;
        j += 1;
    }
    finish_hard_line(cap, hard.start, hard.end, line_start, first, indent, plans);
    None
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

/// `hyphens: none` 时，词内的连字符不再是断点 —— `je-ne-sais-quoi` 得整个留着。
///
/// 判据是「这个连字符两侧都挨着拉丁字母或数字」。只拦连字符类（`-`、U+2010、U+2011、
/// 软连字符 U+00AD），中文破折号 `—` 不拦：它是词分隔符，拦掉等于把整段中文的折行
/// 能力砍一半，那比连字符这件事大得多。行尾的 `word-`（后面是空格）也不拦，
/// 那本来就是正常的词边界。
fn hyphen_break_banned(text: &str, at: usize, hyphens: Hyphens) -> bool {
    if hyphens != Hyphens::None {
        return false;
    }
    let (Some(before), Some(after)) = (text.get(..at), text.get(at..)) else {
        return false;
    };
    let mut rev = before.chars().rev();
    let (Some(hy), Some(letter_before)) = (rev.next(), rev.next()) else {
        return false;
    };
    let Some(letter_after) = after.chars().next() else {
        return false;
    };
    is_hyphen_char(hy)
        && letter_before.is_ascii_alphanumeric()
        && letter_after.is_ascii_alphanumeric()
}

fn is_hyphen_char(c: char) -> bool {
    matches!(c, '-' | '\u{2010}' | '\u{2011}' | '\u{00AD}')
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
///
/// **宽度表在这一趟里一并算好**（见 [`Collector::push_widths`]）：原先先展平、
/// 再 `compute_widths` 走第二趟，两趟都要遍历全部字符；合起来之后只剩一趟。
fn collect(
    pieces: &[NodeId],
    dom: &Dom,
    styles: &StyledDom,
    pre: bool,
    images: &dyn ImageProvider,
    measure: &dyn Measure,
    width: f32,
) -> Collected {
    let mut out = Collector {
        text: String::new(),
        widths: Vec::new(),
        pieces: Vec::new(),
        images: Vec::new(),
        arena: StyleArena::new(),
        pending_space: false,
        dom,
        styles,
        provider: images,
        measure,
        width,
    };
    for node in pieces {
        let style = out.intern_style(*node);
        out.push_node(*node, pre, style);
    }
    Collected {
        text: out.text,
        pieces: out.pieces,
        images: out.images,
        styles: out.arena,
        widths: out.widths,
    }
}

struct Collector<'a> {
    text: String,
    /// 与 `text` **按字节同步增长**的每字符前进量表（非字符起点的字节是 0）。
    widths: Vec<f32>,
    pieces: Vec<Piece>,
    images: Vec<ImageAtom>,
    /// 这一段用到的样式的去重表。
    arena: StyleArena,
    /// 折叠空白是**跨节点**的：上一段尾巴上的空白与下一段开头的空白是同一次折叠，
    /// 谁先落地谁占位，所以这个状态挂在收集器上。
    pending_space: bool,
    dom: &'a Dom,
    styles: &'a StyledDom,
    provider: &'a dyn ImageProvider,
    measure: &'a dyn Measure,
    /// 段落可用宽度，图片百分比宽度的基准。
    width: f32,
}

impl<'a> Collector<'a> {
    /// 为一个节点算样式并进表，返回它的下标。
    fn intern_style(&mut self, node: NodeId) -> StyleRef {
        // 不克隆 `ComputedStyle`：`RunStyle::new` 只读它。
        let computed = self.styles.get(node).unwrap_or(&DEFAULT_STYLE);
        self.arena.intern(RunStyle::new(computed))
    }

    /// 为新追加的 `text[start..]` 算每字符前进量，推进 `widths`。
    ///
    /// 与文本同源：`text` 与 `widths` 按字节同步增长，所以 `widths[at]` 就是
    /// `text` 里字节 `at` 处那个字符的前进量。
    fn push_widths(&mut self, start: usize, style: StyleRef) {
        let Some(run) = self.arena.get(style) else {
            return;
        };
        let font = Arc::clone(&run.font);
        let font_size = run.font_size;
        let spacing = run.word_spacing;
        // 借用到此结束，下面才能可变借 `self.widths`。
        let base = self.widths.len();
        let adv = self
            .measure
            .char_widths(&self.text[start..], &font, font_size);
        // 一次 `resize` 把这段扩到目标字节长（多字节字符的后续字节补 0），
        // 再按字符起点写进去。**不能逐字节 `push`**：CJK 一字符三字节，
        // push 三次（其中两次是 0）比一次 memset 填满慢得多（实测 +12%）。
        self.widths.resize(base + (self.text.len() - start), 0.0);
        for (i, (offset, ch)) in self.text[start..].char_indices().enumerate() {
            let mut w = adv.get(i).copied().unwrap_or(0.0);
            if ch == ' ' {
                w += spacing;
            }
            self.widths[base + offset] = w;
        }
    }

    /// 按下标取样式。
    #[allow(dead_code)] // 断行已改走 `Collected::widths`，这里留给以后的按节点查样式
    fn style_ref(&self, id: StyleRef) -> &RunStyle {
        self.arena.get(id).expect("intern 过的样式")
    }

    fn push_node(&mut self, node: NodeId, pre: bool, style: StyleRef) {
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
                self.push_widths(norm_start, style);
                self.pieces.push(Piece {
                    node,
                    norm: norm_start..self.text.len(),
                    orig,
                    style,
                });
            }
            return;
        }
        if self.dom.is_element(node, "br") {
            self.text.push('\n');
            // `\n` 一个字节，占位 0；断行只扫 hard line 内部，这个宽不会被读到。
            self.widths.push(0.0);
            self.pending_space = false;
            return;
        }
        if self.dom.is_element(node, "img") {
            self.push_image(node);
            return;
        }
        // 文本继承的是**它所在元素**的样式，所以往下走要换成元素自己的样式。
        let own = self.intern_style(node);
        for child in dom_node.children.clone() {
            self.push_node(child, pre, own);
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
        let style = self.styles.get(node).unwrap_or(&DEFAULT_STYLE);
        let (w, h) = image_size(style, img.width, img.height, self.width);
        self.flush_space();
        let at = self.text.len();
        self.text.push(IMAGE_GLYPH);
        self.widths.push(w);
        for _ in 1..IMAGE_GLYPH.len_utf8() {
            self.widths.push(0.0);
        }
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
            // 可折叠空白只有 **ASCII** 那一套（CSS 的 document white space）。
            // U+3000（全角空格）是**内容**：EasyPub 一类工具拿它做段首缩进，
            // 浏览器原样渲染 —— `char::is_whitespace()` 会把它当空白吞掉，
            // 整本书的段首缩进就没了（真机实测《斗战狂潮》）。
            if ch.is_ascii_whitespace() {
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

#[cfg(test)]
mod tests {
    //! E1/E2/E3 块级 + 行内 + 图片布局。
    //! 几何用一份「汉字 1 em、拉丁 0.5 em」的 mock `Measure` 来断言，
    //! 真实字体度量在 app 侧接入，逻辑同一。只依赖公开 API。

    use crate::css::computed::{Color, ComputedStyle, Display, cascade};
    use crate::css::{Origin, RuleSet};
    use crate::dom::{Dom, NodeId};
    use crate::layout::{
        BlockBox, BlockCache, FontKey, ImageProvider, Layout, LayoutCache, LayoutOptions,
        LineFragment, LineMetrics, Measure, NoImages, ResolvedImage, RunStyle, StyleArena, collect,
        dom_hash, layout, layout_cached, layout_with_blocks, measure_chapter, prefs_hash,
        render_window, styles_hash,
    };

    /// 断行规则：空格后优先，CJK 逐字可断（硬断）。
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

    fn slice_width(text: &str, size: f32) -> f32 {
        text.chars().map(|c| char_width(c, size)).sum()
    }

    impl Measure for Mono {
        fn char_width(&self, ch: char, _font: &FontKey, font_size: f32) -> f32 {
            char_width(ch, font_size)
        }

        fn run_metrics(&self, text: &str, _font: &FontKey, font_size: f32) -> LineMetrics {
            LineMetrics {
                width: slice_width(text, font_size),
                ascent: font_size * 0.8,
                descent: font_size * 0.2,
            }
        }
    }

    fn parse(html: &str, css: &str) -> (Dom, Vec<ComputedStyle>) {
        let dom = Dom::parse(html.as_bytes());
        let rules = RuleSet::parse(css, Origin::Author);
        let styles = cascade(&dom, &rules, &ComputedStyle::default());
        (dom, styles.styles.clone())
    }

    fn find(dom: &Dom, want: &str) -> NodeId {
        dom.elements()
            .find(|(id, _)| dom.tag_name(*id) == Some(want))
            .map(|(id, _)| id)
            .unwrap_or_else(|| panic!("文档里应有 <{want}>"))
    }

    fn typeset(html: &str, css: &str, width: f32) -> (Dom, Layout) {
        let dom = Dom::parse(html.as_bytes());
        let styles = cascade(
            &dom,
            &RuleSet::parse(css, Origin::Author),
            &ComputedStyle::default(),
        );
        let laid = layout(
            &dom,
            &styles,
            find(&dom, "body"),
            LayoutOptions {
                width,
                light: false,
            },
            &Mono,
            &NoImages,
        );
        (dom, laid)
    }

    fn texts(layout: &Layout) -> Vec<String> {
        (0..layout.lines.len())
            .map(|i| layout.line_text(i))
            .collect()
    }

    #[test]
    fn a_long_paragraph_wraps_and_stacks() {
        let html = format!("<p>{}</p>", "字".repeat(30));
        let (_, layout) = typeset(&html, "", 200.0);
        assert_eq!(
            texts(&layout),
            vec!["字".repeat(12), "字".repeat(12), "字".repeat(6)],
            "16px 字号、200px 版心 → 每行 12 字"
        );
        assert!(
            layout.lines.iter().all(|l| l.height == 28.0),
            "行盒高 = 字号 × 行距"
        );
        for pair in layout.lines.windows(2) {
            assert!(pair[1].y > pair[0].y, "行盒必须单调下移");
        }
    }

    /// 词级断行（稀疏的拉丁文走这条路）：断点把硬行切成「词」，整词判定「装不下就
    /// 挪到下一行」。拉丁 0.5em、空格 0.25em、字号 16 ⇒ 字母 8px、空格 4px，版心 100px。
    /// `alpha beta` 宽 84px，再加 ` gamma` 会超，于是整个 `gamma` 挪行 —— 这与逐字符
    /// 实现逐行一致（词首本身就是断点）。
    #[test]
    fn latin_breaks_at_word_boundaries() {
        let (_, layout) = typeset("<p>alpha beta gamma</p>", "", 100.0);
        assert_eq!(texts(&layout), ["alpha beta", "gamma"]);
    }

    /// 行首的「超长词」（单个词就宽过整行）退回逐字符硬切：切成装得下的段，
    /// 一个字都不丢。前导空格只被行尾裁掉，不是内容丢了。
    #[test]
    fn a_word_wider_than_the_line_is_hard_cut() {
        let (_, layout) = typeset("<p>hi supercalifragilisticexpialidocious ok</p>", "", 100.0);
        assert_eq!(
            texts(&layout),
            ["hi", "supercalifra", "gilisticexpi", "alidocious ok"],
            "超长词该被硬切成装得下的段"
        );
        assert_eq!(
            texts(&layout).concat().replace(' ', ""),
            "hisupercalifragilisticexpialidociousok",
            "裁掉的只是行尾那个空格，字符一个都没丢"
        );
    }

    /// `hyphens` 是连字符唯一那条能一眼看出来的声明：法语的 `je-ne-sais-quoi`
    /// 在 `none` 下不许从连字符处断行。开着时第一个连字符是断点，关掉后第一行
    /// 不能以连字符结尾 —— 但内容一个字都不能丢（排不下就硬切是我们既有的兜底，
    /// 不是浏览器那种溢出）。
    #[test]
    fn hyphens_none_removes_the_hyphen_break_opportunity() {
        let html = "<p>je-ne-sais-quoi</p>";
        let (_, open) = typeset(html, "p { font-size: 20px; hyphens: manual }", 70.0);
        assert_eq!(texts(&open), ["je-ne-", "sais-", "quoi"], "连字符处该断");

        let (_, shut) = typeset(html, "p { font-size: 20px; hyphens: none }", 70.0);
        let lines = texts(&shut);
        assert_eq!(lines.concat(), "je-ne-sais-quoi", "拦掉断点不能丢字");
        assert!(
            !lines[0].ends_with('-'),
            "`none` 之后连字符不该再是断点：{lines:?}"
        );
    }

    /// 中文与这条无关：`hyphens: none` 只关连字符。逐字断行照旧，
    /// 破折号（词分隔符）也照断 —— 拦掉它等于把整段中文的折行能力砍一半。
    #[test]
    fn hyphens_none_leaves_cjk_breaking_alone() {
        for html in [
            "<p>文文文文文</p>",
            "<p>文—文文文</p>",
            "<p>「引号」里的话</p>",
            "<p>第3章：开端</p>",
        ] {
            let (_, open) = typeset(html, "", 40.0);
            let (_, shut) = typeset(html, "p { hyphens: none }", 40.0);
            let lines = texts(&shut);
            assert!(lines.len() > 1, "{html} 该被折开，否则这条断言是空的");
            assert_eq!(texts(&open), lines, "{html} 中文不该受 hyphens 影响");
        }
    }

    /// 首字下沉：首字符自成一个片段站在第一行的行首，正文从它右边开始；它压着的那几行
    /// 让开同一格宽，之后的行回到版心左缘。基线**不许**被那个大字推下去 ——
    /// 那是 `float` 在 CSS 里的语义，也是我们这一格要的效果。
    #[test]
    fn drop_cap_hangs_the_first_letters_box_over_the_following_lines() {
        let html = format!("<p>{}</p>", "字".repeat(40));
        let (_, plain) = typeset(&html, "p { font-size: 20px }", 200.0);
        let (dom, laid) = typeset(
            &html,
            "p { font-size: 20px } p::first-letter { font-size: 3em }",
            200.0,
        );
        // 3em 的那个字：宽 60px、上下 extent 60px，行盒高 20×1.75=35 ⇒ 跨 2 行。
        let cap_w = 60.0f32;
        let first = &laid.lines[0];
        assert_eq!(first.fragments.len(), 2, "首字符 + 正文两个片段");
        let cap = match &first.fragments[0] {
            LineFragment::Text(run) => run,
            _ => panic!("首字符该是文本片段"),
        };
        assert_eq!(cap.norm, 0..3, "那个字在段里的字节区间");
        assert_eq!(cap.style.font_size, 60.0);
        assert_eq!(cap.width, cap_w);
        // 锚点仍落在真的文本节点上：`orig` 是源节点里的区间，选择与批注都靠它。
        assert!(dom.get(cap.node).tag.is_none(), "首字符的节点该是文本节点");
        assert_eq!(cap.orig, 0..3);

        assert_eq!(first.x, 0.0, "首字符从版心左缘起");
        assert_eq!(
            first.baseline, plain.lines[0].baseline,
            "首字符不许把这一行的基线推下去"
        );
        assert_eq!(laid.lines[1].x, cap_w, "第二行让开首字符那一格");
        assert_eq!(laid.lines[2].x, 0.0, "跨过的行数之外回到左缘");
        // 让开一格之后每行少装 3 个字（60px / 20px）。
        assert_eq!(texts(&plain)[0].chars().count(), 10);
        assert_eq!(texts(&laid)[1].chars().count(), 7);
        // 内容一个字都不能丢。
        assert_eq!(texts(&laid).concat(), "字".repeat(40));
    }

    /// 首行缩进与下沉同时存在时，两者叠着走：首字符从缩进之后起，被压着的行
    /// 让开的只是首字符那一格（缩进只作用于第一行）。
    #[test]
    fn drop_cap_and_text_indent_stack() {
        let html = format!("<p>{}</p>", "字".repeat(40));
        let (_, laid) = typeset(
            &html,
            "p { font-size: 20px; text-indent: 2em } p::first-letter { font-size: 3em }",
            200.0,
        );
        assert_eq!(laid.lines[0].x, 40.0, "首行让出缩进");
        assert_eq!(laid.lines[1].x, 60.0, "第二行只让出首字符那一格");
    }

    /// 没写 `::first-letter` 的段落一行都不该让 —— 下沉必须是显式声明换来的。
    #[test]
    fn no_cap_without_the_pseudo_element_rule() {
        let html = "<p>首字很大的一段话，这里没有声明。</p>";
        let (_, laid) = typeset(html, "p { font-size: 20px }", 200.0);
        assert!(
            laid.lines.iter().all(|line| line.fragments.len() == 1),
            "每行都该只有一个正文片段"
        );
        assert!(laid.lines.iter().all(|line| line.x == 0.0));
    }

    /// 开头是标点时，CSS 把紧贴首字母的标点一起算进首字符盒 ——
    /// `「開」` 那种书名号开头，引号要跟着大字走，否则第一个引号是正常号、第二个是三号字。
    #[test]
    fn cap_box_includes_the_punctuation_before_the_letter() {
        let html = "<p>「開」門見山的一段话。</p>";
        let (_, laid) = typeset(
            html,
            "p { font-size: 20px } p::first-letter { font-size: 2em }",
            200.0,
        );
        let cap = match &laid.lines[0].fragments[0] {
            LineFragment::Text(run) => run,
            _ => panic!("首字符该是文本片段"),
        };
        assert!(
            texts(&laid)[0].starts_with("「開"),
            "引号该在首字符盒里：{:?}",
            texts(&laid)[0]
        );
        assert_eq!(cap.norm, 0.."「開".len(), "下沉盒含前面的标点");
        assert_eq!(cap.style.font_size, 40.0);
    }

    /// 数字开头也下沉，而且只下沉那一个数字单元：UAX #29 的「首字母单元」把数字算在内，
    /// 浏览器给 `1984` 的 `::first-letter` 上的就是那个 `1`。
    #[test]
    fn cap_takes_a_leading_digit_as_its_first_letter_unit() {
        let (_, laid) = typeset(
            "<p>1984 那一年</p>",
            "p { font-size: 20px } p::first-letter { font-size: 2em }",
            200.0,
        );
        let cap = match &laid.lines[0].fragments[0] {
            LineFragment::Text(run) => run,
            _ => panic!("该有首字符盒"),
        };
        assert_eq!(cap.norm, 0..1, "只吃那一个数字，剩下的照正常流程");
        assert_eq!(cap.style.font_size, 40.0);
        assert_eq!(
            texts(&laid).concat(),
            "1984 那一年",
            "摘出去的那一格不能丢正文"
        );
    }

    /// 整段没有字母单元（纯标点）时不造首字符盒 —— 给一个破折号做 3em 的下沉
    /// 在排版上没有任何意义。
    #[test]
    fn no_cap_when_there_is_no_letter_unit() {
        let (_, laid) = typeset(
            "<p>——</p>",
            "p { font-size: 20px } p::first-letter { font-size: 3em }",
            200.0,
        );
        assert_eq!(laid.lines[0].fragments.len(), 1, "纯标点不该造下沉盒");
        assert_eq!(texts(&laid).concat(), "——");
    }

    /// 中文排版基础之一：首行缩进只作用于块里的第一行。
    #[test]
    fn text_indent_shifts_only_the_first_line() {
        let html = format!("<p>{}</p>", "字".repeat(30));
        let (_, layout) = typeset(&html, "p { text-indent: 2em }", 200.0);
        assert_eq!(layout.lines[0].x, 32.0, "首行缩进 2em = 32px");
        assert_eq!(layout.lines[1].x, 0.0, "后续行回到版心左缘");
        assert_eq!(
            texts(&layout)[0].chars().count(),
            10,
            "首行少容 2 字，而不是整段都少"
        );
    }

    #[test]
    fn sibling_paragraph_margins_collapse() {
        let (_, layout) = typeset("<p>甲</p><p>乙</p>", "", 200.0);
        let paras: Vec<&BlockBox> = layout
            .blocks
            .iter()
            .filter(|b| !b.text.is_empty())
            .collect();
        assert_eq!(paras.len(), 2);
        assert_eq!(paras[0].y, 16.0, "UA 给 p 的 1em 上边距");
        assert_eq!(
            paras[1].y - (paras[0].y + paras[0].height),
            16.0,
            "相邻段边距取较大值而不是相加"
        );
    }

    #[test]
    fn headings_get_ua_size_and_weight_and_can_be_centered() {
        let (_, layout) = typeset("<h1>章名</h1>", "h1 { text-align: center }", 200.0);
        assert_eq!(layout.lines.len(), 1);
        assert_eq!(layout.lines[0].dominant_font_size(), 32.0, "UA h1 = 2em");
        assert_eq!(layout.lines[0].x, 68.0, "两字 64px 居中于 200px 版心");

        let (dom, styles) = parse("<h1>x</h1><span>y</span>", "");
        let h1 = styles.get(find(&dom, "h1") as usize).unwrap();
        assert_eq!(h1.font_weight, 700);
        assert_eq!(h1.display, Display::Block);
        let span = styles.get(find(&dom, "span") as usize).unwrap();
        assert_eq!(span.display, Display::Inline, "UA 必须知道 span 是行内");
    }

    #[test]
    fn br_forces_a_break_and_is_not_rendered() {
        let (_, layout) = typeset("<p>甲<br>乙</p>", "", 200.0);
        assert_eq!(texts(&layout), vec!["甲", "乙"]);
    }

    #[test]
    fn nested_padding_offsets_and_narrows_the_child() {
        let html = format!("<div><p>{}</p></div>", "字".repeat(40));
        let (_, layout) = typeset(&html, "div { padding-left: 20px }", 200.0);
        let p = layout
            .blocks
            .iter()
            .find(|b| !b.text.is_empty())
            .expect("段落块");
        assert_eq!(p.x, 20.0);
        assert_eq!(p.width, 180.0);
        assert_eq!(layout.lines[0].x, 20.0, "行盒跟随内容盒");
        assert_eq!(texts(&layout)[0].chars().count(), 11, "180px 只容 11 字");
    }

    #[test]
    fn display_none_removes_the_whole_subtree() {
        let (_, layout) = typeset("<p>留</p><p class=x>去</p>", "p.x { display: none }", 200.0);
        assert_eq!(texts(&layout), vec!["留"]);
    }

    #[test]
    fn inline_pieces_keep_their_own_node_offsets() {
        let (dom, layout) = typeset("<p>甲<strong>乙</strong>丙</p>", "", 200.0);
        assert_eq!(layout.line_text(0), "甲乙丙");
        let runs: Vec<_> = layout.lines[0].text_runs().collect();
        assert_eq!(runs.len(), 3, "三个文本节点各一段");
        let recovered: Vec<String> = runs
            .iter()
            .map(|f| {
                let node = dom.get(f.node);
                node.text.get(f.orig.clone()).unwrap_or("").to_string()
            })
            .collect();
        assert_eq!(
            recovered,
            vec!["甲", "乙", "丙"],
            "锚点坐标要能原路取回文本"
        );
    }

    #[test]
    fn whitespace_between_pieces_collapses_to_one_space() {
        let (_, layout) = typeset("<p>前   <em>中</em>   后</p>", "", 200.0);
        assert_eq!(layout.line_text(0), "前 中 后", "跨节点的空白只折叠一次");
    }

    /// U+3000（全角空格）是**内容**不是空白：EasyPub 一类工具拿它做段首缩进，
    /// CSS 的折叠只管 ASCII 空白，浏览器原样渲染。吞掉它 = 整本书的段首缩进消失
    /// （真机实测《斗战狂潮》）。
    #[test]
    fn ideographic_spaces_survive_collapsing() {
        let (_, layout) = typeset("<p>\u{3000}\u{3000}辛巴的表情</p>", "", 600.0);
        assert_eq!(
            layout.line_text(0),
            "\u{3000}\u{3000}辛巴的表情",
            "段首全角空格必须原样保留"
        );
        // 它们占宽度：行宽 = 两个全角空格 + 五个字。
        assert!(
            (layout.lines[0].width - 7.0 * 16.0).abs() < 0.5,
            "实际 {}",
            layout.lines[0].width
        );
        // 段中出现的全角空格同样不折叠、不并成 ASCII 空格。
        let (_, layout) = typeset("<p>前\u{3000}\u{3000}后</p>", "", 600.0);
        assert_eq!(layout.line_text(0), "前\u{3000}\u{3000}后");
    }

    #[test]
    fn pre_keeps_inner_whitespace_and_hard_breaks() {
        let (_, layout) = typeset("<pre>一\n  二</pre>", "", 200.0);
        assert_eq!(texts(&layout), vec!["一", "  二"]);
    }

    #[test]
    fn empty_paragraph_produces_no_line_box() {
        let (_, layout) = typeset("<p></p><p>有</p>", "", 200.0);
        assert_eq!(layout.lines.len(), 1);
        assert_eq!(layout.lines[0].y, 32.0, "空段不占行高，两段边距折叠成一档");
    }

    #[test]
    fn line_hit_testing_walks_the_stacks() {
        let html = format!("<p>{}</p>", "字".repeat(30));
        let (_, layout) = typeset(&html, "", 200.0);
        assert_eq!(layout.line_at(layout.lines[0].y), Some(0));
        assert_eq!(layout.line_at(layout.lines[1].y + 1.0), Some(1));
        assert_eq!(layout.line_at(layout.height + 10.0), None);
    }

    #[test]
    fn layout_output_is_send_so_it_can_be_built_off_thread() {
        fn assert_send<T: Send>() {}
        assert_send::<Layout>();
        let (_, layout) = typeset("<p>文</p>", "", 200.0);
        assert_eq!(layout.lines.len(), 1);
    }

    #[test]
    fn a_wider_viewport_yields_fewer_lines() {
        let html = format!("<p>{}</p>", "字".repeat(60));
        let (_, narrow) = typeset(&html, "", 160.0);
        let (_, wide) = typeset(&html, "", 320.0);
        assert_eq!(narrow.lines.len(), 6, "10 字/行");
        assert_eq!(wide.lines.len(), 3, "20 字/行");
        assert!(wide.height < narrow.height, "变宽必须变矮");
    }

    /// 段首段尾空白被吞掉、中间并成一个空格 —— 现在从**行盒文本**这一层断言。
    #[test]
    fn collection_trims_paragraph_edges() {
        let (_, layout) = typeset("<p>  甲 乙  </p>", "", 200.0);
        assert_eq!(
            layout.line_text(0),
            "甲 乙",
            "段首段尾空白吞掉，中间并成一个"
        );
    }

    // ---------------------------------------------------------------------------
    // E2：一个段落里的多种样式
    // ---------------------------------------------------------------------------

    /// 局部字号与颜色必须活到行盒里：这是「多 run」存在的全部理由。
    #[test]
    fn a_paragraph_keeps_per_run_size_weight_and_color() {
        let html = "<p>正文<small>小字</small>正文</p>";
        let css = "p { font-size: 20px } small { font-size: 12px; color: #808080 }";
        let (_, layout) = typeset(html, css, 400.0);
        let runs: Vec<_> = layout.lines[0].text_runs().collect();
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].style.font_size, 20.0);
        assert_eq!(runs[1].style.font_size, 12.0, "small 的小字号不该被拉平");
        assert_eq!(runs[2].style.font_size, 20.0);
        assert_eq!(runs[1].style.color, Some(Color(128, 128, 128)));
        assert_eq!(runs[0].style.color, None);
        // 行宽是各 run 实际绘制宽度之和：12px 的两个汉字只占 24px。
        assert_eq!(layout.lines[0].width, 20.0 * 4.0 + 12.0 * 2.0);
    }

    #[test]
    fn bold_and_italic_survive_as_run_styles() {
        let (_, layout) = typeset("<p>甲<strong>乙</strong><em>丙</em></p>", "", 400.0);
        let runs: Vec<_> = layout.lines[0].text_runs().collect();
        assert_eq!(runs[1].style.font.weight, 700, "UA 的 b/strong 加粗");
        assert!(runs[2].style.font.italic, "UA 的 em 斜体");
        assert_eq!(runs[0].style.font.weight, 400);
    }

    /// 行盒高取各 run 的盒（含自己的半 leading）的并集，而不是第一段的。
    #[test]
    fn line_box_height_follows_the_tallest_run() {
        let (_, layout) = typeset(
            "<p>正文<span class=big>大字</span></p>",
            "p { font-size: 20px } .big { font-size: 40px }",
            400.0,
        );
        assert_eq!(layout.lines[0].height, 40.0 * 1.75);
        // 基线不在最大 ascent 上：每个行内盒先分到自己的半 leading，再按共同基线对齐。
        // 大字那一份是 ascent 32 + (70 − 40)/2 = 47，正文那份是 16 + 7.5 = 23.5，取大的。
        assert_eq!(
            layout.lines[0].baseline, 47.0,
            "基线 = 各 run 上方 extent 的最大值"
        );
    }

    /// 边框与内边距一起从可用宽里扣掉，而 `BlockBox` 记的是**边框盒** —— 画底色与边框
    /// 要用外层那一圈，正文与折行要用扣完之后的内容盒，两者必须是同一个盒模型算出来的。
    #[test]
    fn borders_and_padding_inset_the_content_box() {
        let (dom, layout) = typeset(
            "<div class=t><p>正文</p></div>",
            ".t { border-width: 0px 0px 0px 20px; border-style: none none none solid; padding: 10px 5px 10px 5px }",
            400.0,
        );
        let div = layout
            .blocks
            .iter()
            .find(|b| b.node == find(&dom, "div"))
            .expect("div 的盒");
        assert_eq!(
            (div.x, div.width),
            (0.0, 400.0),
            "外层记的是边框盒，不是内容盒"
        );
        let p = layout
            .blocks
            .iter()
            .find(|b| b.node == find(&dom, "p"))
            .expect("p 的盒");
        // 20px 左边框 + 5px 左内边距 = 正文从 25 起，宽也照这个收窄。
        assert_eq!((p.x, p.width), (25.0, 370.0));
        assert_eq!(layout.lines[0].x, 25.0, "行盒跟着内容盒走");
        // 10 = div 的上内边距（上边框是 0），再加 `<p>` 自己的 UA 上边距 16（1em）。
        assert_eq!(p.y, 26.0, "内容起点在边框盒往里数第二层");
    }

    /// 中文禁则：行首不能是收尾标点，行尾不能是起句标点。
    /// 断点取自 UAX #14，所以这条是「引擎自持断行」换来的能力。
    #[test]
    fn kinsoku_keeps_punctuation_off_the_line_edges() {
        // 「了不起」后面的右引号必须跟着引号里的字一起留在行尾，不能被挤到下一行行首。
        let text = "于是我们奋力向前逆水行舟被许多人当作「了不起的盖茨比」中译本里那句检验样本";
        let (_, layout) = typeset(&format!("<p>{text}</p>"), "", 160.0);
        let lines = texts(&layout);
        assert!(lines.len() >= 3, "这段应被折成多行");
        for line in &lines {
            let first = line.chars().next().unwrap_or(' ');
            let last = line.chars().last().unwrap_or(' ');
            assert!(
                !"。，、！？」』；：）》".contains(first),
                "行首出现了禁则标点：{line}"
            );
            assert!(!"「『（《".contains(last), "行尾出现了禁则标点：{line}");
        }
    }

    /// 回归门：行盒高就是作者给的 line-height，**不**被抬到字形的 content area。
    ///
    /// 这里的假度量是 ascent 0.8em + descent 0.2em = 恰好 1em，所以 `line-height: 0.8`
    /// 配 20px 字号必须给出 16px 的行盒，相邻两行的行顶也相差 16px —— 字形本体 20px 高，
    /// 于是两行压在一起。浏览器画紧行距正是这个样子。真书里 `line-height: 125%` 配
    /// Noto Sans CJK（自然盒约 1.44em）之前被抬成 1.44em，整本读起来比实际渲染松一成。
    ///
    /// 顺带钉住半 leading：松行距时上下各分一半（基线不在行顶贴着），紧行距时为负、基线往上收。
    #[test]
    fn the_line_box_is_the_authors_line_height_even_when_glyphs_overflow() {
        let text = "字".repeat(40);
        let (_, tight) = typeset(
            &format!("<p>{text}</p>"),
            "p { font-size: 20px; line-height: 0.8 }",
            100.0,
        );
        assert!(tight.lines.len() >= 3, "这段应被折成多行");
        for line in &tight.lines {
            assert_eq!(line.height, 16.0, "行盒高 = 字号 × line-height");
            // (16 − 20) / 2 = −2：基线从字形的 16px ascent 上再收 2px。
            assert_eq!(line.baseline, 14.0);
        }
        for pair in tight.lines.windows(2) {
            assert_eq!(
                pair[1].y - pair[0].y,
                16.0,
                "行顶相差一个行盒，而不是字形的高度"
            );
        }

        let (_, loose) = typeset(
            &format!("<p>{text}</p>"),
            "p { font-size: 20px; line-height: 2 }",
            100.0,
        );
        for line in &loose.lines {
            assert_eq!(line.height, 40.0);
            // (40 − 20) / 2 = +10：leading 上下各一半，基线落在 16 + 10。
            assert_eq!(
                line.baseline, 26.0,
                "松行距的字该在行盒里居中，不是顶着上缘"
            );
        }
    }

    // ---------------------------------------------------------------------------
    // E3：图片（替换元素）
    // ---------------------------------------------------------------------------

    /// 固定尺寸的图片提供者：`src` 原样当 href，尺寸查表。
    struct MockImages(Vec<(&'static str, f32, f32)>);

    impl ImageProvider for MockImages {
        fn resolve(&self, src: &str) -> Option<ResolvedImage> {
            self.0
                .iter()
                .find(|(k, _, _)| *k == src)
                .map(|(k, w, h)| ResolvedImage {
                    href: (*k).to_string(),
                    width: *w,
                    height: *h,
                })
        }
    }

    fn typeset_images(
        html: &str,
        css: &str,
        width: f32,
        images: &dyn ImageProvider,
    ) -> (Dom, Layout) {
        let dom = Dom::parse(html.as_bytes());
        let styles = cascade(
            &dom,
            &RuleSet::parse(css, Origin::Author),
            &ComputedStyle::default(),
        );
        let laid = layout(
            &dom,
            &styles,
            find(&dom, "body"),
            LayoutOptions {
                width,
                light: false,
            },
            &Mono,
            images,
        );
        (dom, laid)
    }

    /// 行内图片：变成图片片段，参与行宽累加，文本照旧在两侧。
    #[test]
    fn an_inline_image_becomes_a_fragment_and_adds_width() {
        let imgs = MockImages(vec![("pic.png", 60.0, 40.0)]);
        let (_, layout) = typeset_images("<p>前<img src=\"pic.png\">后</p>", "", 200.0, &imgs);
        // 哨兵字符不能漏进可见文本。
        assert_eq!(layout.line_text(0), "前后");
        let img = layout.lines[0].images().next().expect("应有一个图片片段");
        assert_eq!(img.href, "pic.png");
        assert_eq!((img.width, img.height), (60.0, 40.0));
        assert_eq!(
            layout.lines[0].width,
            16.0 + 60.0 + 16.0,
            "汉字各 1em = 16px"
        );
        // 图片底贴基线，行盒还要容下文字 descender，所以 ≥ 图片高。
        assert!(layout.lines[0].height >= 40.0, "行盒要装下图片高度");
        assert_eq!(layout.lines[0].baseline, 40.0, "图片底贴基线");
    }

    /// 放不下的行内图要整体折到下一行，不能被拆开。
    #[test]
    fn an_image_wraps_to_the_next_line_intact() {
        let imgs = MockImages(vec![("pic.png", 100.0, 20.0)]);
        let (_, layout) = typeset_images("<p>甲甲甲<img src=\"pic.png\"></p>", "", 100.0, &imgs);
        assert!(layout.lines.len() >= 2);
        assert_eq!(layout.line_text(0), "甲甲甲");
        assert_eq!(layout.lines[0].images().count(), 0, "第一行不该有图");
        assert_eq!(layout.lines[1].images().count(), 1, "图整体落到第二行");
    }

    /// 块级图片：独立成行，按容器的 text-align 居中。
    #[test]
    fn a_block_image_is_centered_by_the_container() {
        let imgs = MockImages(vec![("pic.png", 100.0, 50.0)]);
        let (_, layout) = typeset_images(
            "<div><img src=\"pic.png\"></div>",
            "div { text-align: center } img { display: block }",
            200.0,
            &imgs,
        );
        assert_eq!(layout.lines.len(), 1);
        assert_eq!(layout.lines[0].x, 50.0, "(200 - 100) / 2");
        assert_eq!(layout.lines[0].images().next().unwrap().width, 100.0);
    }

    /// `width` 用 px：按指定宽，高度按纵横比。
    #[test]
    fn width_in_px_scales_the_height_by_aspect_ratio() {
        let imgs = MockImages(vec![("pic.png", 60.0, 40.0)]);
        let (_, layout) = typeset_images(
            "<p><img src=\"pic.png\"></p>",
            "img { width: 120px }",
            300.0,
            &imgs,
        );
        let img = layout.lines[0].images().next().unwrap();
        assert_eq!((img.width, img.height), (120.0, 80.0));
    }

    /// `width: 100%`：按包含块内容宽度折算。
    #[test]
    fn width_percent_resolves_against_the_container() {
        let imgs = MockImages(vec![("pic.png", 60.0, 40.0)]);
        let (_, layout) = typeset_images(
            "<p><img src=\"pic.png\"></p>",
            "img { width: 100% }",
            300.0,
            &imgs,
        );
        let img = layout.lines[0].images().next().unwrap();
        assert_eq!((img.width, img.height), (300.0, 200.0));
    }

    /// `max-width` 夹小图片，高度按纵横比跟着缩。
    #[test]
    fn max_width_clamps_and_scales_height() {
        let imgs = MockImages(vec![("pic.png", 60.0, 40.0)]);
        let (_, layout) = typeset_images(
            "<p><img src=\"pic.png\"></p>",
            "img { max-width: 30px }",
            300.0,
            &imgs,
        );
        let img = layout.lines[0].images().next().unwrap();
        assert_eq!(img.width, 30.0);
        assert!((img.height - 20.0).abs() < 0.01);
    }

    /// 没写 `max-width` 的超宽图也不能溢出容器。
    #[test]
    fn an_oversized_image_never_overflows() {
        let imgs = MockImages(vec![("big.png", 500.0, 100.0)]);
        let (_, layout) = typeset_images("<p><img src=\"big.png\"></p>", "", 300.0, &imgs);
        let img = layout.lines[0].images().next().unwrap();
        assert_eq!(img.width, 300.0);
        assert!(img.width <= 300.0, "插图宽度不得超过版心");
        assert!((img.height - 60.0).abs() < 0.01);
    }

    /// 解析不到的图整张跳过，不塌出空盒子、也不留哨兵。
    #[test]
    fn an_unresolvable_image_is_skipped_cleanly() {
        let imgs = MockImages(vec![]);
        let (_, layout) = typeset_images("<p><img src=\"missing.png\"></p>", "", 300.0, &imgs);
        assert!(layout.lines.is_empty(), "没有文本也没有图，就不该有行盒");
    }

    // ---- `word-spacing` / `vertical-align` / `text-decoration` 在几何里长什么样 ----
    //
    // 这三个属性的落差都发生在「样式表已经算出数、排版层没用」这一步，所以断言的必须是
    // 行盒的数，而不是 `ComputedStyle` 的数（后者在 `css::computed::tests` 已经钉过）。

    #[test]
    fn word_spacing_widens_the_line_and_pulls_the_break_earlier() {
        // mock 度量：拉丁 0.5em、空格 0.25em，字号 16 ⇒ 字母 8px、空格 4px。
        let html = "<p>a b c d e f</p>";
        let (_, plain) = typeset(html, "", 100.0);
        assert_eq!(plain.lines.len(), 1, "68px 的内容在 100px 版心里是一行");
        assert_eq!(plain.lines[0].width, 68.0);

        // 每个空格后再加 8px：内容变成 68 + 5 × 8 = 108px，100px 放不下，必须提前折。
        let (_, spread) = typeset(html, "p { word-spacing: 8px }", 100.0);
        assert_eq!(spread.lines.len(), 2, "词距该参与折行，不只是画出来宽一点");
        // 第一行 5 个字母 + 4 个空格（行尾空格被裁掉，所以它的词距也不算）。
        assert_eq!(spread.lines[0].width, 88.0, "5 × 8 + 4 × (4 + 8)");
        assert_eq!(spread.lines[1].width, 8.0);
        assert_eq!(spread.line_text(1), "f");
    }

    /// 上标要的是两件事：这一行的基线被抬高，行盒高把抬出去的那截也装下。
    /// `line-height: 1` 是为了让行盒高真的由字形决定 —— 否则 line-height 的 28px 早就盖住了。
    #[test]
    fn vertical_align_lifts_the_baseline_and_grows_the_line_box() {
        let html = "<p>文<em class=s>上</em>文</p>";
        let css = "p { line-height: 1 } .s { vertical-align: super }";
        let (_, laid) = typeset(html, css, 200.0);
        assert_eq!(laid.lines.len(), 1);
        let line = &laid.lines[0];
        let shift = 16.0 / 3.0;
        assert!(
            (line.baseline - (12.8 + shift)).abs() < 0.01,
            "基线该是最高那段（被抬过的）ascent，实际 {}",
            line.baseline
        );
        assert!(
            (line.height - (12.8 + shift + 3.2)).abs() < 0.01,
            "行盒高要把上标抬出去的一截装下，否则它会压到下一行，实际 {}",
            line.height
        );
        // 只有那个 run 带偏移，邻居不受影响。
        let shifts: Vec<f32> = line
            .fragments
            .iter()
            .filter_map(|f| match f {
                crate::layout::LineFragment::Text(run) => Some(run.style.baseline_shift),
                crate::layout::LineFragment::Image(_) => None,
            })
            .collect();
        assert_eq!(shifts.len(), 3);
        assert!(shifts[1].abs() - shift < 0.01, "中间那段是 <em>");
        assert_eq!(shifts[0], 0.0);
        assert_eq!(shifts[2], 0.0);
    }

    /// `sub` 反过来：多吃 descent，基线不动。
    #[test]
    fn vertical_align_sub_extends_the_descent_without_moving_the_baseline() {
        let (_, laid) = typeset(
            "<p>文<em class=s>下</em>文</p>",
            "p { line-height: 1 } .s { vertical-align: sub }",
            200.0,
        );
        let line = &laid.lines[0];
        assert!((line.baseline - 12.8).abs() < 0.01, "基线仍由正文那段决定");
        assert!(
            (line.height - (12.8 + 3.2 + 4.0)).abs() < 0.01,
            "descent 多出 0.25em（4px），行盒高跟着长，实际 {}",
            line.height
        );
    }

    /// 装饰线不改变几何（CSS 也是如此），它必须**原样到达 run**，绘制层才有东西可画。
    #[test]
    fn decorations_reach_the_run_without_changing_geometry() {
        let (_, plain) = typeset("<p>正文<em class=u>下划</em></p>", "", 200.0);
        let (_, marked) = typeset(
            "<p>正文<em class=u>下划</em></p>",
            ".u { text-decoration: underline }",
            200.0,
        );
        let flags = |layout: &Layout| -> Vec<bool> {
            layout.lines[0]
                .fragments
                .iter()
                .filter_map(|f| match f {
                    crate::layout::LineFragment::Text(run) => Some(run.style.underline),
                    crate::layout::LineFragment::Image(_) => None,
                })
                .collect()
        };
        assert_eq!(flags(&plain), vec![false, false], "没声明就不该有下划线");
        assert_eq!(flags(&marked), vec![false, true]);
        assert_eq!(
            marked.lines[0].height, plain.lines[0].height,
            "装饰线不计入行盒高，两次的几何必须一模一样"
        );
        assert_eq!(marked.lines[0].width, plain.lines[0].width);
    }

    // ---- light 模式 ----

    /// light 模式：只算高，不产出行盒。用于目录/进度条。
    #[test]
    fn light_mode_produces_no_lines() {
        let dom =
            Dom::parse("<p>一段足够长的中文正文，用来验证轻量模式只算高不断行。</p>".as_bytes());
        let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default());
        let body = dom
            .elements()
            .find(|(id, _)| dom.tag_name(*id) == Some("body"))
            .map(|(id, _)| id)
            .unwrap_or(dom.document);

        let light = layout(
            &dom,
            &styles,
            body,
            LayoutOptions {
                width: 600.0,
                light: true,
            },
            &Mono,
            &NoImages,
        );
        assert!(light.lines.is_empty(), "light 模式不该产出行盒");
        assert!(!light.blocks.is_empty(), "light 模式仍应产出块");
        assert!(light.height > 0.0, "light 模式应算出高");
    }

    /// light 模式与完整模式的块高应接近（误差在一个行高以内）。
    #[test]
    fn light_mode_height_matches_full_within_tolerance() {
        let dom =
            Dom::parse("<p>第一段正文。</p><p>第二段正文。</p><p>第三段正文。</p>".as_bytes());
        let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default());
        let body = dom
            .elements()
            .find(|(id, _)| dom.tag_name(*id) == Some("body"))
            .map(|(id, _)| id)
            .unwrap_or(dom.document);

        let full = layout(
            &dom,
            &styles,
            body,
            LayoutOptions {
                width: 600.0,
                light: false,
            },
            &Mono,
            &NoImages,
        );
        let light = layout(
            &dom,
            &styles,
            body,
            LayoutOptions {
                width: 600.0,
                light: true,
            },
            &Mono,
            &NoImages,
        );
        let diff = (full.height - light.height).abs();
        let line_px = 16.0 * 1.5; // 默认字号 * 行距
        assert!(
            diff < line_px,
            "light 与完整模式的高差应在一个行高以内：full={} light={} diff={}",
            full.height,
            light.height,
            diff
        );
    }

    // ---- 两阶段排版 ----

    /// 语料：30 段纯汉字，段长递增（40..=69），字号整除版心 → 轻量估高与真实折行逐字对上。
    fn two_stage_corpus() -> (Dom, NodeId, LayoutOptions) {
        let html: String = (0..30)
            .map(|i| format!("<p>{}</p>", "文".repeat(40 + i)))
            .collect();
        let dom = Dom::parse(html.as_bytes());
        let body = dom
            .elements()
            .find(|(id, _)| dom.tag_name(*id) == Some("body"))
            .map(|(id, _)| id)
            .unwrap_or(dom.document);
        (
            dom,
            body,
            LayoutOptions {
                width: 320.0,
                light: false,
            },
        )
    }

    /// `measure_chapter` 的高、以及整窗口 `render_window` 的行，都应与一次性完整排版一致。
    #[test]
    fn two_stage_matches_full_layout() {
        let (dom, body, opts) = two_stage_corpus();
        let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default());

        let full = layout(&dom, &styles, body, opts, &Mono, &NoImages);
        let map = measure_chapter(&dom, &styles, body, opts, &Mono, &NoImages);
        assert!(
            (map.height - full.height).abs() < 0.5,
            "轻量估高应与完整排版一致：light={} full={}",
            map.height,
            full.height
        );

        let all = render_window(&map, &dom, &styles, &Mono, &NoImages, 0.0, map.height + 1.0);
        assert_eq!(
            all.lines.len(),
            full.lines.len(),
            "整窗口的行数应与完整排版一致"
        );
        for (a, b) in all.lines.iter().zip(&full.lines) {
            assert_eq!((a.x, a.y, a.width, a.height), (b.x, b.y, b.width, b.height));
        }
    }

    /// `render_window` 只排可见窗口：行是完整排版里对应那一段的**原样**，且远少于整章。
    #[test]
    fn render_window_only_covers_the_visible_blocks() {
        let (dom, body, opts) = two_stage_corpus();
        let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default());
        let full = layout(&dom, &styles, body, opts, &Mono, &NoImages);
        let map = measure_chapter(&dom, &styles, body, opts, &Mono, &NoImages);

        let from = map.height * 0.5;
        let win = render_window(&map, &dom, &styles, &Mono, &NoImages, from, from + 200.0);
        assert!(!win.lines.is_empty(), "窗口里该有行");
        assert!(win.lines.len() < full.lines.len(), "窗口的行应远少于整章");

        // 窗口的首行能在完整排版里找到；其后应与完整排版逐行一致（锚定后 y 不变）。
        let base = full
            .lines
            .iter()
            .position(|l| (l.y - win.lines[0].y).abs() < 0.01)
            .expect("窗口首行应出现在完整排版里");
        assert!(base + win.lines.len() <= full.lines.len());
        for (k, wl) in win.lines.iter().enumerate() {
            let fl = &full.lines[base + k];
            assert_eq!(
                (wl.x, wl.y, wl.width, wl.height),
                (fl.x, fl.y, fl.width, fl.height)
            );
        }
    }

    // ---- LayoutCache ----

    #[test]
    fn layout_cache_returns_cached_on_hit() {
        let dom = Dom::parse("<p>缓存测试。</p>".as_bytes());
        let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default());
        let body = dom
            .elements()
            .find(|(id, _)| dom.tag_name(*id) == Some("body"))
            .map(|(id, _)| id)
            .unwrap_or(dom.document);
        let mut cache = LayoutCache::new();

        let opts = LayoutOptions {
            width: 600.0,
            light: false,
        };
        let h = prefs_hash(&crate::config::Reader::default());
        let sh = styles_hash(&styles);
        let dh = dom_hash(&dom, body);

        // 第一次：未命中
        {
            let r1 = layout_cached(
                &mut cache, &dom, &styles, body, opts, &Mono, &NoImages, h, sh, dh,
            );
            assert_eq!(r1.lines.len(), 1);
        }

        // 第二次：命中缓存
        let r2 = layout_cached(
            &mut cache, &dom, &styles, body, opts, &Mono, &NoImages, h, sh, dh,
        );
        assert_eq!(r2.lines.len(), 1);
    }

    #[test]
    fn layout_cache_misses_on_width_change() {
        let dom = Dom::parse("<p>宽度变化测试。</p>".as_bytes());
        let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default());
        let body = dom
            .elements()
            .find(|(id, _)| dom.tag_name(*id) == Some("body"))
            .map(|(id, _)| id)
            .unwrap_or(dom.document);
        let mut cache = LayoutCache::new();

        let h = prefs_hash(&crate::config::Reader::default());
        let sh = styles_hash(&styles);
        let dh = dom_hash(&dom, body);

        let _ = layout_cached(
            &mut cache,
            &dom,
            &styles,
            body,
            LayoutOptions {
                width: 600.0,
                light: false,
            },
            &Mono,
            &NoImages,
            h,
            sh,
            dh,
        );
        let r2 = layout_cached(
            &mut cache,
            &dom,
            &styles,
            body,
            LayoutOptions {
                width: 300.0,
                light: false,
            },
            &Mono,
            &NoImages,
            h,
            sh,
            dh,
        );
        assert_eq!(r2.width, 300.0 - 0.0); // 新宽度
    }

    #[test]
    fn layout_cache_invalidate_clears() {
        let mut cache = LayoutCache::new();
        assert!(!cache.has(600.0, 0, 0, 0, false));
        cache.invalidate();
        assert!(!cache.has(600.0, 0, 0, 0, false));
    }

    /// `dom_hash` 在长中文文本上不能 panic —— 切字节会把一个汉字切成两半。
    #[test]
    fn dom_hash_handles_long_non_ascii_text() {
        let long = "字".repeat(500);
        let dom = Dom::parse(format!("<p>{long}</p>").as_bytes());
        let body = dom
            .elements()
            .find(|(id, _)| dom.tag_name(*id) == Some("body"))
            .map(|(id, _)| id)
            .unwrap_or(dom.document);
        // 不 panic 即通过；顺便钉一下两次哈希一致。
        let a = dom_hash(&dom, body);
        let b = dom_hash(&dom, body);
        assert_eq!(a, b, "同一份 DOM 两次 hash 必须一样");
    }

    // ---- BlockCache ----

    /// 一个平铺章节：`<body>` 下三个段落（第二个带 `.hot`），便于断言「只重排某一块」。
    fn flat_three() -> (Dom, NodeId) {
        let dom = Dom::parse("<p>第一段</p><p class=\"hot\">第二段</p><p>第三段</p>".as_bytes());
        let body = dom
            .elements()
            .find(|(id, _)| dom.tag_name(*id) == Some("body"))
            .map(|(id, _)| id)
            .unwrap_or(dom.document);
        (dom, body)
    }

    /// 两份 `Layout` 的行盒几何与总高必须逐行一致。
    fn assert_same_lines(a: &Layout, b: &Layout) {
        assert_eq!(a.lines.len(), b.lines.len(), "行数一致");
        for (x, y) in a.lines.iter().zip(&b.lines) {
            assert_eq!(
                (x.x, x.y, x.width, x.height),
                (y.x, y.y, y.width, y.height),
                "行几何一致"
            );
        }
        assert_eq!(a.height, b.height, "总高一致");
    }

    /// 同一份输入、同一个缓存连排两次：第二次全命中，输出与直接排版逐行一致。
    #[test]
    fn block_cache_reuses_and_matches_fresh_layout() {
        let (dom, body) = flat_three();
        let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default());
        let opts = LayoutOptions {
            width: 300.0,
            light: false,
        };
        let dh = dom_hash(&dom, body);
        let fresh = layout(&dom, &styles, body, opts, &Mono, &NoImages);

        let mut cache = BlockCache::new();
        let first = layout_with_blocks(&mut cache, &dom, &styles, body, opts, &Mono, &NoImages, dh);
        assert_same_lines(&first, &fresh);
        assert_eq!(cache.misses, 3, "首次排版三个段落都没命中");

        let (hits0, misses0) = (cache.hits, cache.misses);
        let second =
            layout_with_blocks(&mut cache, &dom, &styles, body, opts, &Mono, &NoImages, dh);
        assert_same_lines(&second, &fresh);
        assert_eq!(cache.misses, misses0, "第二次不该再有未命中");
        assert_eq!(cache.hits - hits0, 3, "第二次三块全命中");
    }

    /// 只改一个段落的样式：只有那一个块失效（平铺结构下未命中恰好 +1），其余命中 ——
    /// 这就是「改一处只重排一块」。第三段虽然因为第二段变高而整体下移，仍走命中路径
    /// （复用时会按新落点平移）。
    #[test]
    fn block_cache_only_relayouts_the_changed_block() {
        let (dom, body) = flat_three();
        let opts = LayoutOptions {
            width: 300.0,
            light: false,
        };
        let dh = dom_hash(&dom, body);

        let plain = cascade(&dom, &RuleSet::default(), &ComputedStyle::default());
        let mut cache = BlockCache::new();
        layout_with_blocks(&mut cache, &dom, &plain, body, opts, &Mono, &NoImages, dh);

        // 只给 `.hot`（第二段）加字号。
        let hot = cascade(
            &dom,
            &RuleSet::parse(".hot { font-size: 28px }", Origin::Author),
            &ComputedStyle::default(),
        );
        let (hits0, misses0) = (cache.hits, cache.misses);
        let out = layout_with_blocks(&mut cache, &dom, &hot, body, opts, &Mono, &NoImages, dh);
        assert_eq!(cache.misses - misses0, 1, "只有样式变的那一块重排");
        assert_eq!(cache.hits - hits0, 2, "另外两块命中");

        // 输出的行数、总高应与「不缓存地按新样式实排」一致。
        let hot_fresh = layout(&dom, &hot, body, opts, &Mono, &NoImages);
        assert_same_lines(&out, &hot_fresh);
    }

    /// DOM 换了（`dom_hash` 变）→ 整清缓存，杜绝拿旧 `NodeId` 命中。
    #[test]
    fn block_cache_clears_when_the_dom_changes() {
        let (dom, body) = flat_three();
        let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default());
        let opts = LayoutOptions {
            width: 300.0,
            light: false,
        };
        let mut cache = BlockCache::new();
        layout_with_blocks(
            &mut cache,
            &dom,
            &styles,
            body,
            opts,
            &Mono,
            &NoImages,
            dom_hash(&dom, body),
        );
        let misses0 = cache.misses;
        // 用 0 冒充另一份 DOM：这次进来该整清后重新填，于是三块全部未命中。
        layout_with_blocks(&mut cache, &dom, &styles, body, opts, &Mono, &NoImages, 0);
        assert_eq!(cache.misses - misses0, 3, "换了 DOM 该全部重排");
    }

    /// 超容量就按 LRU 淘汰。
    #[test]
    fn block_cache_evicts_over_capacity() {
        let (dom, body) = flat_three();
        let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default());
        let opts = LayoutOptions {
            width: 300.0,
            light: false,
        };
        let mut cache = BlockCache::with_capacity(2);
        layout_with_blocks(
            &mut cache,
            &dom,
            &styles,
            body,
            opts,
            &Mono,
            &NoImages,
            dom_hash(&dom, body),
        );
        assert_eq!(cache.len(), 2, "三个块该挤到只剩容量上限的两个");
    }

    // ---- StyleArena ----

    /// 相同的样式只占一格 —— 这是去重存在的理由。
    #[test]
    fn style_arena_deduplicates_identical_styles() {
        let mut arena = StyleArena::new();
        let style = RunStyle::default();
        let a = arena.intern(style.clone());
        let b = arena.intern(style);
        assert_eq!(a, b, "同一份样式两次 intern 该拿到同一个下标");
        assert_eq!(arena.len(), 1, "只该占一格");
    }

    /// 不同样式各占一格。
    #[test]
    fn style_arena_keeps_distinct_styles_apart() {
        let mut arena = StyleArena::new();
        // `font` 是 `Arc<FontKey>`，要换字段就整个换一份。
        let bold = RunStyle {
            font: std::sync::Arc::new(FontKey {
                weight: 700,
                ..FontKey::default()
            }),
            ..RunStyle::default()
        };
        let a = arena.intern(RunStyle::default());
        let b = arena.intern(bold);
        assert_ne!(a, b);
        assert_eq!(arena.len(), 2);
        assert_eq!(arena.get(a).unwrap().font.weight, 0);
        assert_eq!(arena.get(b).unwrap().font.weight, 700);
    }

    /// 字号不同就是不同样式（f32 按位比）。
    #[test]
    fn style_arena_distinguishes_float_fields() {
        let mut arena = StyleArena::new();
        let big = RunStyle {
            font_size: 20.0,
            ..RunStyle::default()
        };
        let a = arena.intern(RunStyle::default());
        let b = arena.intern(big);
        assert_ne!(a, b, "字号不同不该被并成一份");
    }

    /// 一章里的样式种类数远小于片段数 —— 这是去重的收益。
    #[test]
    fn one_paragraph_shares_one_style() {
        let html = "<p>一段<b>粗体</b>与<i>斜体</i>混排的正文。</p>";
        let dom = Dom::parse(html.as_bytes());
        let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default());
        let body = dom
            .elements()
            .find(|(id, _)| dom.tag_name(*id) == Some("body"))
            .map(|(id, _)| id)
            .unwrap_or(dom.document);
        let collected = collect(&[body], &dom, &styles, false, &NoImages, &Mono, 600.0);
        // 三种样式：正文、粗体、斜体。片段数（pieces）至少 3，但样式表明显更小。
        assert!(
            collected.styles.len() <= 4,
            "一段混排只该有几份样式，实际 {}",
            collected.styles.len()
        );
        assert!(
            collected.pieces.len() >= 3,
            "三个文本节点该是至少三个 piece，实际 {}",
            collected.pieces.len()
        );
    }

    // ---- 宽度表（批量度量）----

    /// 宽度表按字节下标存，长度与文本一致；每个字符起点都有一个值。
    #[test]
    fn the_width_table_covers_every_character() {
        let dom = Dom::parse("<p>abc汉字</p>".as_bytes());
        let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default());
        let body = dom
            .elements()
            .find(|(id, _)| dom.tag_name(*id) == Some("body"))
            .map(|(id, _)| id)
            .unwrap_or(dom.document);
        let collected = collect(&[body], &dom, &styles, false, &NoImages, &Mono, 600.0);
        assert_eq!(collected.widths.len(), collected.text.len());
        // 每个字符起点都该有非零宽度（Mono 对空格也给 0.25em）。
        for (offset, _) in collected.text.char_indices() {
            assert!(
                collected.width_at(offset) > 0.0,
                "字节 {offset} 处的字符宽度不该为 0"
            );
        }
    }

    /// 宽度表与逐字符度量得出同一个总宽 —— 这是「批处理不改变结果」的判据。
    #[test]
    fn the_batch_width_matches_per_char_measurement() {
        let dom = Dom::parse("<p>混排 Latin 与汉字。</p>".as_bytes());
        let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default());
        let body = dom
            .elements()
            .find(|(id, _)| dom.tag_name(*id) == Some("body"))
            .map(|(id, _)| id)
            .unwrap_or(dom.document);
        let collected = collect(&[body], &dom, &styles, false, &NoImages, &Mono, 600.0);

        let mut batch = 0.0;
        let mut per_char = 0.0;
        for (offset, ch) in collected.text.char_indices() {
            batch += collected.width_at(offset);
            let style = collected.style_at(offset).expect("每个字符都该有样式");
            per_char += style.advance(&Mono, ch);
        }
        assert!(
            (batch - per_char).abs() < 0.01,
            "批处理与逐字符必须同源：batch={batch} per_char={per_char}"
        );
    }
}
