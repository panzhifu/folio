//! 把命中的声明算成 `ComputedStyle`：一条声明进哪几个字段 + 继承 + 沿树展开。
//!
//! 单个值怎么解成 Rust 在 [`super::value`]；这里只管属性名到字段的对应关系。
//!
//! 支持面就是 `computed/apply.rs` 里那批 match 臂（一条声明写进哪几个字段），加上
//! [`KNOWN_UNIMPLEMENTED`]（认得、这一版不生效）。清单外的声明**被忽略而不是报错**，
//! 因为真实出版书里总有些用不上的声明，为一两个 `transform` 让整本书排版失败
//! 是最糟的失败方式。
mod cascade;
mod derived;
mod known;
mod types;
mod types2;
pub use cascade::cascade;
pub(super) use known::KNOWN_UNIMPLEMENTED;
pub use types::{
    BackgroundAttachment, BackgroundAxis, BackgroundEdge, BackgroundLayer, BackgroundPosition,
    BackgroundRepeat, BackgroundSize, BackgroundTile, BoxSizing, Clear, Float,
};
pub use types2::{
    BackgroundClip, BreakWhen, CellVAlign, Color, ColumnFill, ColumnSpan, Cursor, FontKerning,
    FontSmoothing, FontStretch, Hyphens, InlineVAlign, Length, Positioning, RubyAlign,
    RubyPosition, SvgLineCap, SvgLineJoin, SvgPaint, TextCombine, TextRendering, TextShadow,
    TextTransform, WordBreak,
};

use crate::dom::NodeId;
use std::sync::Arc;

use super::{Declaration, RuleSet};
pub(super) mod apply;
pub(super) use apply::boxes::{apply_column_rule_parts, apply_column_span};

// `inherit_from` 原本就是 `pub(crate)`，`css::ua` 从这条路径取它，所以这里必须是再导出而非私有导入。
pub(crate) use apply::inherit_from;
use apply::{apply, parse_inline_style};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextAlign {
    /// 起始边：`ltr` 是左，`rtl` 是右。CSS 的初值就是这一档（不是 `left`）。
    #[default]
    Start,
    /// 结束边：`ltr` 是右，`rtl` 是左。
    End,
    /// 物理左。作者显式写 `text-align: left` 时用它 —— 那种写法**不跟着方向翻**，
    /// 而 `Start` 翻，这是 CSS 的分界。
    Left,
    Right,
    Center,
    Justify,
}

/// 一张图要「出血」到哪几条边（多看的 `duokan-bleed` 扩展）。
///
/// 它不是 CSS 的盒模型，而是「这张图铺到屏幕边上、不要页边距」这件事的最小表示。
/// **横向那两条边（`left` / `right`）真被排版层消费**：判据与走法见
/// `layout::collect::bleeds_wide` —— 图自己或它的任意一层祖先写了都算，因为真书把
/// 这一条写在包图的那个块上（《诡秘之主》三个类全是 `div`）。基准因此换成屏幕宽，
/// 于是 `width:100%` 折成满屏而不是满栏，行宽仍按栏宽折回。
///
/// `top` / `bottom` 存下来而**这一版没有对应动作**：纵向出血要的是「这张图盖住页顶/页底」，
/// 那要改的是分片的边界而不是一张图的盒尺寸 —— 改分片等于把「一页多高」这件事重做一遍。
/// 所以只置纵向那两条时，屏上与没写一模一样。为什么不把它记进 `KNOWN_UNIMPLEMENTED`：
/// 那一名单是按属性名算的，而这条属性名**已经在生效**（横半）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Bleed {
    pub top: bool,
    pub right: bool,
    pub bottom: bool,
    pub left: bool,
}

impl Bleed {
    pub const NONE: Self = Self {
        top: false,
        right: false,
        bottom: false,
        left: false,
    };

    /// 有没有一条边出血（四条都没置等于没写）。
    pub fn any(self) -> bool {
        self.top || self.right || self.bottom || self.left
    }

    /// 横向出血：这张图的可用宽换成整屏宽。
    pub fn horizontal(self) -> bool {
        self.left || self.right
    }
}

/// `object-fit`：一张图在它自己的盒里怎么摆。
///
/// 排版期只用到「盒子多大」，所以这一格**不影响排版**；绘制期拿它决定「图按哪一块区域
/// 采样进这个盒」——`cover` 要裁、`contain` 要留底、`fill` 拉伸。
///
/// 以前这里一条都不认：修图书里常见的 `img { width: 100%; object-fit: cover }` 会把一张
/// 扁图画成拉长的样子（《基督山伯爵》699 MB 那本 660 个元素写了 `contain`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ObjectFit {
    /// 拉伸填满，不管比例。CSS 初值。
    #[default]
    Fill,
    /// 保比例缩到盒内，留底。
    Contain,
    /// 保比例铺满盒，超出裁掉。
    Cover,
    /// 原尺寸，超出裁掉，不缩放。
    None,
    /// `cover` 与 `contain` 里**更小**的那一个（不放大）。
    ScaleDown,
}

/// 一段文字的书写方向，即 CSS 的 `direction`。
///
/// 这一版做到「块与行」这一层：它决定 `start` / `end` 落在物理的哪一边、`text-indent`
/// 缩哪一边、以及一行里各片段的**视觉次序**（`layout::collect` 那一次同级反转）。
/// 片段**内部**的字形重排不在这里做，也不该在这里做：绘制那条路是按整段做 shaping 的，
/// bidi 与镜像已经在那一层发生（`cosmic-text` 每次 `shape_line` 自己跑 UAX #9），
/// 这里再排一次就是两处各排各的。
///
/// 没接的那一样要说清：CSS 的 `unicode-bidi: isolate` / `bidi-override` 那类显式嵌入与
/// 覆盖 —— 它出错的样子是「混排里那一小截位置反了」，不是整段读不了。竖排那一根轴是
/// [`WritingMode`]，与这一格是两条互不相干的声明。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Direction {
    #[default]
    Ltr,
    Rtl,
}

impl Direction {
    pub fn is_rtl(self) -> bool {
        self == Direction::Rtl
    }
}

/// 一段文字的推进轴与行的堆叠方向，即 CSS 的 `writing-mode`。它与 [`Direction`] 是
/// **两根独立的轴**：`direction` 改「一行里的字符往哪边排、`start`/`end` 落在物理的
/// 哪一边」，行的推进轴不变；`writing-mode` 改「字沿哪根轴推进、行沿哪根轴堆」。
/// 竖排的本子通常两样都写（`vertical-rl` 的书列序天然从右往左）。
///
/// **落地程度：排版层真的按这一根轴排了**（2026-10-19 那一轮）。架构是「逻辑横排 +
/// 一次转置」——引擎照旧排横长的带，`Layout::assemble` 的 `transposed` 在折索引前
/// 把两张表整体投影成物理坐标，shaping、命中与选区一行都不用改；界面唯一该知道轴的
/// 地方收成本文件的 [`WritingMode::project`] 与 [`project_delta`]。
///
/// 明确降级的三处（`LayoutOptions::allow_vertical` 与 `transposed` 的文档）：盒模型
/// 那几族边是**物理**的；连续滚动与双页对开回退横排；行内图片不转置。
///
/// 触发源只有一处，就是书的 CSS：`writing-mode` 在真实竖排书里是**唯一**的竖排来源，
/// 所以这一格必须先接住层叠，报表才数得出库里哪几本声明了竖排。读者那一档没有加，
/// 落点埋在 `ui::features::reader::model::chapter_layout` 的 `reader_rules` 注释里。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WritingMode {
    /// 横排：字沿 `+x` 推进，行沿 `+y` 堆。CSS 的初值。
    #[default]
    HorizontalTb,
    /// 竖排，列从右往左换 —— 中日韩竖排本的那一档。
    VerticalRl,
    /// 竖排，列从左往右换。
    VerticalLr,
}

impl WritingMode {
    /// 这一档是不是竖排（推进轴换成竖直那一根）。
    pub fn is_vertical(self) -> bool {
        self != WritingMode::HorizontalTb
    }

    /// 把「内联轴上 `inline`、侧轴上 `cross`」这两个纯量折成物理的 `(x, y)`。
    ///
    /// 界面那一层（`ui::reader::view` 的绘制与命中）手里的数从来只有这两个纯量：
    /// 「沿字走了多远」与「离基线多远」。横排里它们分别是 x 与 y，竖排里对调。
    /// **符号在 [`Self::project_delta`] 那一句里**，这一句只管哪一格落到哪根轴。
    pub fn project(self, inline: f32, cross: f32) -> (f32, f32) {
        match self {
            WritingMode::HorizontalTb => (inline, cross),
            WritingMode::VerticalRl | WritingMode::VerticalLr => (cross, inline),
        }
    }

    /// 增量版：`along` 是沿字又走了多远，`depth` 是从基线沿块轴走了多远
    /// （正数在横排是**往下**，在 `vertical-rl` 是往**左** —— 那一档的列从右往左堆，
    /// 块轴整个反着走），物理的 `(dx, dy)` 就从这里出。
    ///
    /// 逐字形绘制、逐字命中、选区矩形、装饰线四处在推同一笔位移，所以符号只许写在这一个
    /// match 上：别处再折一次，就会出现「字与下划线各朝一边」这种两边看着都合理、合起来
    /// 一定错的账。
    pub fn project_delta(self, along: f32, depth: f32) -> (f32, f32) {
        match self {
            WritingMode::HorizontalTb => (along, depth),
            WritingMode::VerticalRl => (-depth, along),
            WritingMode::VerticalLr => (depth, along),
        }
    }
}

/// HTML 的 `dir="auto"`：这一节里**第一个强方向字符**决定方向，一个都没有就回 `None`
/// （调用方留继承值 —— 「不知道」不等于「左」）。
///
/// 「强方向」按 UAX #9 就是 Bidi_Class ∈ {L, R, AL}。两条容易踩的正好都在这里：
/// **阿拉伯文与希伯来文是 `AL` 而不是 `R`**，只认 `R` 会把一整类 RTL 的书判成 LTR；
/// **CJK 汉字的 Bidi_Class 是 `L`**，所以中文段落在第一个字就上定下来，不必扫完全文。
///
/// 表用 `unicode-bidi`（锁里本来就有那一份，理由写在根 `Cargo.toml`），自己不抄那七千行区间。
pub(super) fn first_strong_direction(text: &str) -> Option<Direction> {
    use unicode_bidi::BidiClass::{AL, L, R};
    text.chars()
        .find_map(|ch| match unicode_bidi::bidi_class(ch) {
            L => Some(Direction::Ltr),
            R | AL => Some(Direction::Rtl),
            _ => None,
        })
}

impl TextAlign {
    /// 折成物理一侧。只有 `Start` / `End` 跟着 `direction` 翻，其余原样。
    pub fn physical(self, dir: Direction) -> TextAlign {
        match (self, dir) {
            (TextAlign::Start, Direction::Rtl) | (TextAlign::End, Direction::Ltr) => {
                TextAlign::Right
            }
            (TextAlign::Start, Direction::Ltr) | (TextAlign::End, Direction::Rtl) => {
                TextAlign::Left
            }
            (other, _) => other,
        }
    }

    /// 内容宽 `content` 在可用宽 `avail` 里该让出的水平偏移。
    ///
    /// 两处落位（块级替换元素、折好的行盒）都走这一句，于是「物理左右」这件事在全仓
    /// 只有一份定义 —— 加 `direction` 之前那两处各写了一遍 `Right => avail - width`。
    pub fn offset_in(self, dir: Direction, avail: f32, content: f32) -> f32 {
        let spare = (avail - content).max(0.0);
        match self.physical(dir) {
            TextAlign::Right => spare,
            TextAlign::Center => spare / 2.0,
            _ => 0.0,
        }
    }
}

/// `text-justify` 的取值 —— 两端对齐时余量往**哪种隙**上摊。它是 `text-align: justify`
/// 的修饰，本身不改变对齐与否（`None` 除外，那是「这个元素不要两端对齐」）。
///
/// `inter-ideograph`（中日韩出版物的标准写法）与 `distribute` 都折进
/// [`TextJustify::InterCharacter`]：它们要的都是「没有空格也要在字间摊开」，而
/// ideograph 与 character 在这一层的差别（一个字 vs 一个字位）我们拿不到。
///
/// **一期只让 `InterWord` 与 `None` 改变行为**（`InterWord` 只拉空格、`None` 关掉），
/// 其余一律走默认口径「有空格拉空格，否则拉字间」—— 对纯中文段落，默认口径本就在字间摊，
/// 与 `inter-ideograph` 同解。认得并入库的价值是让 `css-gap` 不再把它算成「不认识」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextJustify {
    /// 初值：由排版层按内容定（有空格拉空格，否则拉字间）。
    #[default]
    Auto,
    /// 只在词（空格）间摊开。
    InterWord,
    /// 在字间摊开（`inter-character` / `inter-ideograph` / `distribute`）。
    InterCharacter,
    /// 这个元素不做两端对齐。
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Display {
    #[default]
    Block,
    Inline,
    ListItem,
    /// `display: none`：排版层负责整块跳过。
    None,
    /// CSS 的 `display: table`（`<table>`）。排版层走 `layout::table` 那一套（auto 布局）。
    Table,
    /// `display: table-row`（`<tr>`）。只有站在一张表里才按行排。
    TableRow,
    /// `display: table-cell`（`<td>` / `<th>`）。只有站在一行里才按格排。
    TableCell,
    /// `display: table-row-group`（`<thead>` / `<tbody>` / `<tfoot>`）；抽取网格时展开一层。
    TableRowGroup,
    /// `display: flex`。排版层走 `layout::flex` 那套（主轴 + 交叉轴摆子项）。
    Flex,
    /// `display: inline-flex`。与 `Flex` 同一套布局；一期没有行内块，落点仍按块排。
    InlineFlex,
    /// `display: inline-block`：行内级原子盒，宽度 shrink-to-fit（内容固有宽）。
    InlineBlock,
    /// `display: inline-table`：行内级表格原子盒。
    InlineTable,
    /// `display: grid`：网格容器，走 `layout::grid` 那套。
    Grid,
    /// `display: inline-grid`：行内级网格容器。
    InlineGrid,
}

/// `flex-direction`：主轴方向。只收 `row` / `column`（`-reverse` 本子集不做）。
///
/// 它是**逻辑**方向而非物理 x/y：`row` 沿书写模式的行内轴，`column` 沿块轴，
/// 竖排下由排版层用 [`WritingMode`] 的投影折成物理坐标。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FlexDirection {
    #[default]
    Row,
    Column,
}

/// `flex-wrap`：主轴放不下时是否换行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FlexWrap {
    #[default]
    NoWrap,
    Wrap,
}

/// `align-items`：子项在**交叉轴**上的对齐。`baseline` 本子集不做（它要字形度量对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AlignItems {
    #[default]
    Stretch,
    Start,
    End,
    Center,
}

/// `justify-self`：grid 子项在自身格子里的**行内轴**对齐。初值 `auto` 按 stretch 处理。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum JustifySelf {
    #[default]
    Auto,
    Start,
    End,
    Center,
    Stretch,
}

/// `justify-content`：子项在**主轴**上的分布。只收书里用到的四档。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum JustifyContent {
    #[default]
    Start,
    End,
    Center,
    SpaceBetween,
}

/// `transform` 里会建立层叠上下文、且绘制侧能兑现的那一部分。
///
/// 完整矩阵与 3D 变换不做（落进 `ignored`）。这里只留层叠相关、能折成简单平移/缩放的
/// 2D 基本型：`translate(x, y)`（长度按 `Length` 折，`%` 折自自身盒）与 `scale(sx, sy)`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Transform {
    Translate(Length, Length),
    Scale(f32, f32),
}

/// `text-decoration-style`：装饰线的形状。颜色另存 `text_decoration_color`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DecorationStyle {
    #[default]
    Solid,
    Double,
    Dotted,
    Dashed,
    Wavy,
}

/// `text-emphasis-style`：着重号的形状（横排/竖排绘制侧据字形方向选字符）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum EmphasisStyle {
    #[default]
    None,
    /// `filled dot` —— 实心圆点。
    FilledDot,
    /// `open dot` —— 空心圆点。
    OpenDot,
    /// `filled circle` / `double-circle` / `triangle` / `sesame` 折成相应字形由绘制侧定。
    FilledCircle,
    Triangle,
    Sesame,
}

/// `text-emphasis-position` 的横排 `over` / `under` 与竖排 `left` / `right`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum EmphasisPosition {
    #[default]
    Over,
    Under,
}

/// 一条 `box-shadow` / `outline` 用的颜色与尺寸（尺寸已折成 px）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShadowLayer {
    pub dx: f32,
    pub dy: f32,
    pub blur: f32,
    /// `inset` 阴影（画在边框内）。
    pub inset: bool,
    pub color: Option<Color>,
}

/// `overflow` / `overflow-x` / `overflow-y` 的取值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Overflow {
    #[default]
    Visible,
    Hidden,
    Clip,
    Scroll,
    Auto,
}

/// `visibility`：可继承；`hidden` / `collapse` 的盒子仍占位但不绘制。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Visibility {
    #[default]
    Visible,
    Hidden,
    Collapse,
}

/// `grid-template-columns` 里的一条轨道尺寸：固定 px、百分比、`fr` 弹性、或 `minmax(min,max)`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GridTrack {
    Length(Length),
    Fr(f32),
    MinMax(Length, Length),
    /// `minmax(<min>, Nfr)`：弹性轨道，min 是下界。
    MinMaxFr(Length, f32),
    Auto,
}

/// `quotes` 的一对开合引号字符（首位字符，足够中文「」『』与西文 ""）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct QuotePair {
    pub open: char,
    pub close: char,
}

/// `list-style-type` 的取值 —— 列表标记的内容，不是它的画法。
///
/// 名单以 **EPUB 3 CSS Profile** 为准：除了 CSS 2.1 那一套（`disc` / `circle` /
/// `square` / `decimal` / `lower-alpha` / `lower-roman` …），profile 还点名收录了
/// CSS 2.0 的记数（`cjk-ideographic` / `hebrew` / `hiragana` / `hiragana-iroha` /
/// `katakana` / `katakana-iroha`）。这一版收到汉字与假名，**`hebrew` 还没收**：
/// 它那套数值记数（1–10 单字、20/100 另字、组合规则）本机没有一本希伯来书可验，
/// 而猜错了是把整本目录的编号排错 —— 所以让它落在 `parse_list_style` 的表外，
/// 由 `css-gap` 报成「不认识」，比画出一个看着像的号诚实。
/// `armenian` / `georgian` / `devanagari` 同一处置。
///
/// 标记字符串由 `layout::marker` 折，序号由排版层按同层的 `list-item` 兄弟数出来。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ListStyle {
    /// 初值。UA 表不给 `<li>` 写类型，靠这一格兜住 `<ul>`。
    #[default]
    Disc,
    Circle,
    Square,
    Decimal,
    LowerAlpha,
    UpperAlpha,
    LowerRoman,
    UpperRoman,
    CjkIdeographic,
    Hiragana,
    HiraganaIroha,
    Katakana,
    KatakanaIroha,
    /// `list-style-type: none` —— 作者明确不要标记。这一本要的是「一条都不画」，
    /// 不是「画不出来」，所以它单独一档而不是 `Option`：《蛇结》的 51 个 `li` 就靠它。
    None,
}

/// 一条边的画法。CSS 的 `border-style` 有八种实线以外的样式；这里接
/// `none` / `hidden`（都不画）、`solid`、`double`、`dotted`、`dashed`。
///
/// 点线/虚线（哈利·波特 `.line` 那种一条点线分隔条，转换本常规写法）由绘制层
/// 把带子按段切开画（`ui::reader::view::body::push_border`），不是画成实线。
/// `groove` / `ridge` / `inset` / `outset` 四档要的是**明暗**，由作者的边框色派生
/// 亮/暗两档（`ui::reader::view::body::three_d_tone`）——`inset` 上/左暗、下/右亮，
/// `outset` 相反，`groove` 每条边外暗内亮，`ridge` 相反。与浏览器的斜角拼角差在
/// 角上（四条轴对齐带子互相压边）。《西游记》101 个元素的 `border: currentColor inset 1px`
/// 正是这一档。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BorderStyle {
    #[default]
    None,
    Solid,
    Double,
    Dotted,
    Dashed,
    /// 左上暗、右下亮（像一个凹进去的框）。
    Inset,
    /// 左上亮、右下暗（像一个凸出来的框）。
    Outset,
    /// 每条边外半暗、内半亮。
    Groove,
    /// 每条边外半亮、内半暗。
    Ridge,
}

/// 一条边的三要素。CSS 的 `border-style: none` 时宽度**按 0 参与盒模型**，
/// 所以「画不画」不能只看 `width`：`border-width: 5px` 而没写 style 的声明
/// 在浏览器里就是一条不存在的边。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BorderEdge {
    pub width: f32,
    pub style: BorderStyle,
    /// 没写就是 `currentColor`：留 `None`，由绘制层取这一块的 `color`
    /// （`color` 可继承，所以那一色在层叠期不一定写在本元素上）。
    pub color: Option<Color>,
}

impl BorderEdge {
    /// 这一边在盒模型里占多宽：不画就是 0。
    pub fn drawn(&self) -> f32 {
        match self.style {
            BorderStyle::None => 0.0,
            _ if self.width > 0.0 => self.width,
            _ => 0.0,
        }
    }
}

impl ComputedStyle {
    /// 四条边各自在盒模型里占多宽，顺序 `[上, 右, 下, 左]`。
    ///
    /// 走 [`BorderEdge::drawn`] 而不是直接取 `width`：`border-style` 是 `none` 时这一边
    /// 按 0 参与计算，所以书里那句只写了 `border-width: 20px` 而没配 style 的声明，
    /// 不该把正文挤窄 40px。
    pub fn border_drawn(&self) -> [f32; 4] {
        self.border.map(|edge| edge.drawn())
    }

    /// 四个角的圆角半径折成 px，顺序 `[左上, 右上, 右下, 左下]`，交给绘制层。
    ///
    /// 百分比按**盒宽**折（CSS 里横半径按盒宽、纵半径按盒高，两者不等时是一个椭圆角；
    /// gpui 的 `Corners` 只有圆角，所以这里取横半径那一份当代表）。
    ///
    /// 相邻两个角沿同一条边相加超过边长时，CSS 会把所有半径**等比缩小**；这里用
    /// 「每个角不超过 `min(宽, 高) / 2`」这一条近似 —— 真书里的圆角是 `1em` 量级，
    /// 离溢出差得远，等比缩放那一档只在作者写出荒唐数时才轮到。
    pub fn corner_radii(&self, width: f32, height: f32) -> [f32; 4] {
        let cap = (width.min(height) / 2.0).max(0.0);
        self.border_radius.map(|radius| {
            let px = match radius {
                Length::Px(v) => v,
                Length::Percent(p) => p * width,
                Length::Auto => 0.0,
            };
            px.clamp(0.0, cap)
        })
    }

    /// 把作者写下的一个**尺寸**折成内容盒上的数（`box-sizing` 落在这一个式子里）。
    ///
    /// `pad_border` 交的是这一格该扣的那一对：横向是「左 + 右的边框与内边距」，纵向是
    /// 「上 + 下的」。四个落点（流内块的宽、浮动盒的宽、块高、替换元素）共用这一份，
    /// 免得「量高」与「实排」两趟各解释一遍、同一张插图框给出两个宽。
    /// 扣穿了回 0 而不是负数：浏览器把内容盒夹到 0，盒子本身还在。
    pub fn content_size(&self, declared: f32, pad_border: f32) -> f32 {
        match self.box_sizing {
            BoxSizing::ContentBox => declared,
            BoxSizing::BorderBox => (declared - pad_border).max(0.0),
        }
    }
}

/// `position`：只收 `static`（初值）与 `absolute` / `fixed`。
///
/// 排版层需要的计算后样式。除 `margin` / `padding` / `text-indent` / 替换元素的
/// 尺寸这一族（它们可能带百分比，要等排版期拿到包含块宽才折得动，见 [`Length`]），
/// 长度一律已折算成 px，`line_height` 是倍数。
#[derive(Debug, Clone, PartialEq)]
pub struct ComputedStyle {
    pub font_size: f32,
    pub line_height: f32,
    pub font_weight: u16,
    pub font_italic: bool,
    /// `font-stretch`：可继承；百分比按 CSS 关键字区间就近折档。
    ///
    /// 连续 `wdth` 轴不在这里，折档后选面。
    pub font_stretch: FontStretch,
    /// `font-variant: small-caps`（可继承）。
    ///
    /// 排版层把小写字母换成等字节长度的 uppercase，并把这一段字号乘 0.8；
    /// 原文不动（选区与复制仍取 DOM 那一份），见 `layout::wrap` 的 `small_caps_text`。
    pub small_caps: bool,
    pub font_family: String,
    /// `font-family` 的**整条候选链**，按书写顺序；`font_family` 就是它的第一个。
    ///
    /// 只留第一个是以前的一处真落差：书里写 `font-family: "MyFont", "Noto Serif CJK SC"`
    /// 时，设备上没装 `MyFont` 就直接落回读者的字体，作者排在第二位的那一份明明装着。
    /// 逐个试的工作在平台层做（只有它知道这台设备注册了哪些家族，见 `ui::reader::measure`）。
    pub font_families: Vec<String>,
    /// `font-kerning`（可继承）。三档见 [`FontKerning`]；落在 shaping 的特性表里。
    pub font_kerning: FontKerning,
    /// `font-feature-settings` 的 OpenType 特性表（可继承）；同名标签后者胜，不改原文。
    pub font_features: Arc<[(String, u32)]>,
    /// `text-rendering`（可继承）。只用来关掉连字/字偶距的那一档有行为差别。
    pub text_rendering: TextRendering,
    pub font_smoothing: FontSmoothing,
    /// 书写方向。CSS 的 `direction`，可继承：作者样式表、`dir` 属性与继承。
    pub direction: Direction,
    /// 推进轴（`writing-mode`），可继承；落地程度见 [`WritingMode`]。
    pub writing_mode: WritingMode,
    pub text_align: TextAlign,
    /// `text-justify`：两端对齐时余量往哪种隙摊。见 [`TextJustify`]；CSS 里**可继承**。
    pub text_justify: TextJustify,
    /// 读者选择的图片尺寸折法（[`crate::css::ImageScale`]），界面注入根样式后继承。
    pub image_scale: crate::css::ImageScale,
    /// 首行缩进。百分比折算自包含块宽；读者那条注入时已是 px。
    pub text_indent: Length,
    /// 四边外边距。百分比折算自包含块宽 —— 就是 `layout` 逐层递下来的那一个 `basis`。
    /// 左右两边可以是 [`Length::Auto`]（均分剩余宽度），那一步在排版期解，
    /// 见 `layout::collect::solve_block_width`。
    pub margin_top: Length,
    pub margin_bottom: Length,
    pub margin_left: Length,
    pub margin_right: Length,
    /// 四边内边距。百分比与外边距同一个基准（CSS 规定 `padding` 的百分比也按包含块宽
    /// 折算，不是按自己的宽）；`padding: auto` 是非法值，所以这一族不吃 `auto`。
    pub padding_top: Length,
    pub padding_bottom: Length,
    pub padding_left: Length,
    pub padding_right: Length,
    pub display: Display,
    // ---- Flex 容器属性（只被 `display: flex` / `inline-flex` 的块消费）----
    /// 主轴方向。不继承（CSS 规定 Flex 容器属性都不继承）。
    pub flex_direction: FlexDirection,
    pub flex_wrap: FlexWrap,
    /// 交叉轴对齐。不继承。
    pub align_items: AlignItems,
    /// `align-self`：flex/grid 子项**逐项**覆盖容器的 `align-items`。`None` = 跟容器。不继承。
    pub align_self: Option<AlignItems>,
    /// 主轴分布。不继承。
    pub justify_content: JustifyContent,
    /// `gap` 的两个分量：主轴为 `row` 时 `column_gap` 是横缝、`row_gap` 是纵缝；
    /// 主轴为 `column` 时互换。CSS 里 `gap` / `row-gap` / `column-gap` **不继承**。
    pub row_gap: Option<Length>,
    pub column_gap: Option<Length>,
    /// 块级多栏容器：`column-count`（显式栏数）与 `column-width`（由栏宽折栏数，
    /// 见 `layout::columns::plan`），`None` = `auto`；两条都写按 CSS 以 count 为准。
    /// 不继承。降级口径（贪心配平、不跨栏拆单元）记在 `collect::columns_layout`。
    pub column_count: Option<u16>,
    pub column_width: Option<Length>,
    /// `column-fill`：`balance`（初值，二分配平）或 `auto`（按序装满）。不继承。
    pub column_fill: ColumnFill,
    /// `column-rule`：栏间分隔线三要素（同 [`BorderEdge`]）；画在栏缝正中。不继承。
    pub column_rule: BorderEdge,
    pub column_span: ColumnSpan,
    // ---- Flex 子项属性（只被 Flex 容器的直接子项消费）----
    /// `flex-grow` / `flex-shrink` 因子与 `flex-basis`。不继承。
    pub flex_grow: f32,
    pub flex_shrink: f32,
    pub flex_basis: Option<Length>,
    /// `z-index`：层叠顺序。只对定位元素与 Flex 子项生效，见 `stacking`。不继承。
    pub z_index: i32,
    /// `opacity`：不透明度，`1.0` = 初值（不建立层叠上下文）。不继承。
    pub opacity: f32,
    /// 祖先与本元素 `opacity` 的乘积（绘制 alpha；层叠仍看 `opacity`）。
    pub paint_opacity: f32,
    /// `transform`：会建立层叠上下文的 2D 基本变换。不继承。
    pub transform: Option<Transform>,
    /// `white-space: nowrap`：合并空白但不换行。与 `white_space_pre`（保留空白）正交，
    /// 只影响断行：nowrap 时整段不换行。不继承外的语义按 CSS（它是可继承的，见 inherit_from）。
    pub white_space_nowrap: bool,
    /// `text-decoration-style` / `-webkit-text-decoration-style` 与 `text-decoration-color`。不继承。
    pub decoration_style: DecorationStyle,
    pub decoration_color: Option<Color>,
    /// `text-emphasis` 一族：着重号形状、颜色与位置。CSS 可继承。
    pub text_emphasis: EmphasisStyle,
    pub text_emphasis_color: Option<Color>,
    pub text_emphasis_position: EmphasisPosition,
    /// `box-shadow`（含 `-webkit-`）：阴影层列表。不继承。
    pub box_shadows: Arc<[ShadowLayer]>,
    /// `outline` / `outline-offset`：轮廓（宽, 色, 画法, 偏移）。不继承。
    pub outline: Option<(f32, Option<Color>, BorderStyle, f32)>,
    /// `overflow-x` / `overflow-y`（`overflow` 是两者简写）。不继承。
    pub overflow_x: Overflow,
    pub overflow_y: Overflow,
    /// `overflow-wrap: break-word | anywhere`：允许在词内断行。可继承。
    pub overflow_wrap_break: bool,
    /// `visibility`：可继承；hidden/collapse 占位但不绘制。
    pub visibility: Visibility,
    /// `table-layout: fixed`：表格用固定列宽算法。不继承。
    pub table_layout_fixed: bool,
    /// SVG 呈现属性（可继承）。只在序列化内联 `<svg>` 子树时消费：
    /// `fill`/`stroke` 的颜色或 `none`、`stroke-width`（用户单位）、线端与折角形状。
    pub svg_fill: SvgPaint,
    pub svg_stroke: SvgPaint,
    pub svg_stroke_width: f32,
    pub svg_stroke_linecap: SvgLineCap,
    pub svg_stroke_linejoin: SvgLineJoin,
    /// `text-transform`：可继承。只在**字节长度不变**时真的换字符 —— 锚点是源文本字节
    /// 偏移，改长度会让选区/进度漂（见 `wrap::transform_text`）。
    pub text_transform: TextTransform,
    /// 竖排数字并格（可继承）；横排里不生效。
    pub text_combine: TextCombine,
    /// `text-size-adjust` / `-webkit-text-size-adjust`：阅读器字号提示（100% = 不缩放）。
    pub text_size_adjust: Option<f32>,
    /// `grid-template-columns` 的轨道定义与 `quotes`（均可继承 quotes、grid 轨道不继承）。
    pub grid_template_columns: Arc<[GridTrack]>,
    pub quotes: Option<[QuotePair; 2]>,
    /// `background-clip`：底色与背景图裁到哪个盒。不继承。初值 `border-box`。
    pub background_clip: BackgroundClip,
    /// `background-origin`：背景图（cover / 百分比定位）的定位基准盒。不继承。
    /// 初值 `padding-box`（CSS 同），显式给而不靠派生 —— 派生的 `Default` 是 clip 的初值。
    pub background_origin: BackgroundClip,
    /// `grid-column` / `grid-row` 的显式定位：起点（1-based，`None` = 自动）与跨格数。
    /// 不继承 —— 子项各自定位。
    pub grid_column_start: Option<u16>,
    pub grid_column_span: u16,
    pub grid_row_start: Option<u16>,
    pub grid_row_span: u16,
    /// `justify-self`：grid 子项在格子里的行内轴对齐。不继承。
    pub justify_self: JustifySelf,
    /// `list-style-type`。只有 `display: list-item`（UA 给 `<li>`）的块会读它。
    ///
    /// CSS 2.1 里它**是可继承**的，而这正是 `<ol>` 的十进制落到每一个 `<li>` 的唯一
    /// 通路：浏览器 UA 表写的是 `ol { list-style-type: decimal }`，而不是往每个 li 上
    /// 撒一条规则。所以 `inherit_from` 必须带它 —— 不带的话 `<ol><li>` 会全线画成圆点。
    pub list_style_type: ListStyle,
    /// `float`。非 `None` 时这一块离开常规流：排版层给它算宽、摆在侧边，
    /// 并把它记进当前块格式上下文的浮动表（见 `layout::collect::PlacedFloat`）。
    /// 块级盒与行内盒走的是两条路，行内那一条只认段首那一档 —— 判据与没接的那半截
    /// 写在 [`Float`]。
    pub float: Float,
    /// `clear`。不继承（CSS 规定），只在**这一块**的顶边上生效。
    pub clear: Clear,
    pub color: Option<Color>,
    /// `background-color`。`None` = 书里没写过这一条（CSS 的初值是 `transparent`，
    /// 与「写了一个颜色」在绘制上是两件事：前者什么都不画，后者要盖住读者的主题底色）。
    /// 背景色在 CSS 里**不继承**，所以 `inherit_from` 不带它。
    pub background_color: Option<Color>,
    /// `background-image` 及其**配对**的 size / position / repeat，按 CSS 书写顺序排列。
    /// 画的时候反过来（先写的在**最上面**，最后写的垫底），与 CSS 的图层次序一致。
    ///
    /// 空表 = 这一层根本没有图（`None` 与「一条不画的图」在这里是同一件事，所以不需要
    /// `Option`）。`background-color` 不在这里：CSS 把它归给最底那一层，而我们的底色字段
    /// 早就存在并且绘制侧按它筛装饰表，两处并存的差别只在「同一块同时写了底色与多层图」
    /// 这种书上极少的组合里，按 CSS 画就是底色垫在所有图下面 —— 落点确实是那一个。
    ///
    /// 与 `background_color` 同一条规矩：**不继承**，所以 `inherit_from` 不带它。
    pub background_layers: Vec<BackgroundLayer>,
    /// `backdrop-filter: blur(<长度>)` 的模糊半径（px），不继承。只收单个 `blur()`；
    /// 改像素颜色的滤镜与连写整条判不认识。绘制侧落成 gpui `BackdropGlass`。
    pub backdrop_blur: Option<f32>,
    /// 四边边框，顺序 `[上, 右, 下, 左]`（与 `box_sides` 同一套展开规则）。
    /// 布局拿它算盒宽与盒高，绘制拿它画线；不继承。
    pub border: [BorderEdge; 4],
    /// `border-spacing` 的 `(横, 纵)` 缝宽（px）。CSS 里**可继承**，且只在
    /// `border-collapse: separate`（初值）下生效 —— 塌陷模式里它被忽略。
    pub border_spacing: [f32; 2],
    /// `border-collapse: collapse`。CSS 里**可继承**，所以写在 `<table>` 上就管到每个格子。
    ///
    /// 落地程度：合并只影响**画哪一条边**（`layout::geometry::Layout::decor_seeds` 里逐边
    /// 与邻格比一次，赢的那侧留、输的那侧抹掉）。几何**没有**做「相邻两格各让出半个边宽」，
    /// 所以合并后那条线落在先画那一格的边上（半个像素的偏移，肉眼分不出），
    /// 而表格自己的边框与最外圈格子边框仍是两条（CSS 里它们也会合并）。
    pub border_collapse: bool,
    /// 四个角的圆角半径，顺序 `[左上, 右上, 右下, 左下]`（CSS 的顺序）。
    ///
    /// 百分比在这里**不折**：`border-radius` 的百分比一是按盒宽（横半径）、盒高（纵半径）
    /// 各折一次，而那要等排版期才知道盒有多大 —— 与 `margin` 一族同一条口径，
    /// 折算点在 [`Self::corner_radii`]。不继承。
    ///
    /// 落地程度：只有**底色**那一块四边形带圆角。边框仍是四条轴对齐的直条
    /// （见 `ui::reader::view::body::decoration_child`），圆角盒配粗边框时四角是直的。
    pub border_radius: [Length; 4],
    /// `word-break`。CSS 里**可继承**。
    pub word_break: WordBreak,
    /// 表格格里内容的竖直对齐（`vertical-align` 的关键字那一面）。不继承。
    pub cell_valign: CellVAlign,
    /// `ruby-align`：注音在容器里的横向对齐。CSS 里**可继承**。
    ///
    /// 消费它的是 `layout::wrap` 折 ruby 容器那一段 —— 注音排在基字上方哪一段、
    /// 容器让出多宽，都在那里按这一格算。
    pub ruby_align: RubyAlign,
    /// `ruby-position`：注音排在基字上方还是下方。CSS 里**可继承**。
    ///
    /// `-epub-ruby-position` 是同一条属性的 EPUB 写法（`-webkit-` 那一族旧拼法也一并收），
    /// 折成同一个数，所以《乌合之众》那 157 条 `over` 与《诡秘之主》的显式 `center` 走同一条路。
    pub ruby_position: RubyPosition,
    /// 定位方式。不继承 —— 只有 `absolute` / `fixed` 会脱流。
    pub position: Positioning,
    /// 四条定位偏移。`%` 的基准是**包含块**：`left`/`right` 折自宽，`top`/`bottom` 折自高。
    /// 不继承。
    pub left: Option<Length>,
    pub right: Option<Length>,
    pub top: Option<Length>,
    pub bottom: Option<Length>,
    /// `text-shadow` 的图层列表（CSS 可继承、可多值）。
    ///
    /// 用 `Arc<[TextShadow]>` 而不是 `Vec`：`ComputedStyle` 每个 DOM 节点都克隆一份，
    /// 而绝大部分节点没有影子 —— 空表克隆只是一次引用计数；有影子时也是共享。
    pub text_shadows: Arc<[TextShadow]>,
    /// 词间额外宽度（px）。CSS 初值 `normal` 折成 0；它叠在**每个空格之后**，
    /// 所以折行与行宽都要算它（见 `layout::run_width`）。
    pub word_spacing: f32,
    /// 字间额外宽度（px）。CSS 初值 `normal` 折成 0；它叠在**每个字符之后**（含最后一个），
    /// 所以折行、行宽、绘制与命中四处都要算它。
    ///
    /// 这四处**必须同批改**：只改宽度表会出现
    /// 「断行提前了、字却还挤在一起」，比不做更糟。绘制按字形所在字符序号右推，
    /// 命中/选区按同一偏移推 `char_edges`。
    pub letter_spacing: f32,
    /// 基线偏移，px，**正数往上**。`vertical-align: super` / `sub` / 长度 / 百分比
    /// 折出来的就是它；`text-top` / `text-bottom` 另走 [`Self::inline_valign`]。
    ///
    /// 0 意味着「贴基线」（CSS 的 `baseline`）。`top` / `bottom` / `middle` 要的是
    /// 行盒对齐机制，一期不折成这个数，并且按不认识计数 —— 悄悄当 0 会把作者的
    /// 上标写成正常位置还查不出来。
    pub baseline_shift: f32,
    /// `vertical-align: text-top` / `text-bottom` —— 这两档**不能**在层叠期折成一个
    /// px 数：它们要对齐的是父盒的字体上下沿，而父盒的字体度量要到排版层才量得出来。
    /// 记下关键字，[`crate::layout`] 在收集行内片段时用真正的 `Measure::font_extents`
    /// 折成该片段（`RunStyle` / `ImageAtom`）的基线偏移。
    ///
    /// 与 [`CellVAlign`] 是同一属性名的两副面孔：那一边管表格格，这一边管行内盒。
    pub inline_valign: InlineVAlign,
    /// `text-decoration` 的下划线与删除线。绘制层用它画线；CSS 里装饰线**不计入行盒高**，
    /// 所以排版层只用它透传到 run 上。
    pub underline: bool,
    pub line_through: bool,
    /// 这一格在不在一条 `<a href>` 里。**不是 CSS 属性**，是 DOM 的事实：书面样式表给不给链接
    /// 上色是作者的事，而「链接看着得像链接」是阅读器的事。
    ///
    /// 为什么需要这一格：出版工具的目录页几乎都不写链接色 —— 本机那本基督山伯爵的目录页是
    /// 38 条 `<a class="calibre1">`，而 `stylesheet.css` 全文（2678 字节）一个 `color` 都没有，
    /// `.calibre1` 写的还是 `text-decoration: none`。于是目录项与正文逐字同色同饰，读者看不出
    /// 那是一栏可以点的东西。UA 那一档（`css/ua.rs` 的 `a`）当年只落下划线、把颜色「留给以后
    /// 按主题着墨」，这一格就是那个着墨的入口：`color` 仍是 `None`（作者没写），由绘制层换成
    /// **当前主题**的链接色，而不是在引擎里嵌一个十六进制数。
    ///
    /// 走继承传播（`inherit_from` 里那一行）：子元素与其中的文本都算在链接里，与 CSS 装饰线
    /// 向后代传播同理。作者写了 `color` 时它照旧赢 —— 这一格只在 `color == None` 时才有话语权。
    pub link: bool,
    /// `cursor`：指针形状（可继承）。只被界面层消费 —— 它不改版面，也不改绘制，
    /// 只改「指针停在字上时该长什么样」。
    pub cursor: Cursor,
    pub white_space_pre: bool,
    /// 尺寸六件套。作者写的数**含不含**边框与内边距由 [`ComputedStyle::box_sizing`] 定，
    /// 折算只走 [`ComputedStyle::content_size`]（四个落点共用）。落点分两条：
    ///
    /// - 替换元素：`layout::ports::image_size` 拿它与固有尺寸、可用宽折出实际大小。
    /// - 流内块级盒：`width` / `max-*` / `min-*` 定边框盒宽与盒高，**内容照排、溢出可见**；
    ///   `<body>` 除外（根不吃 `width`，判据在 `layout::layout_inner`）。
    ///
    /// `min-*` 的夹序是 CSS 2.1 §10.4/§10.7：`max-*` 先夹、`min-*` 后夹，也管 `width: auto`
    /// 那档（填满后仍被抬到溢出）。`height` 一族的百分比基准是包含块**高**，流式里拿不到，
    /// 同一条规则判 `auto`。
    pub width: Option<Length>,
    pub min_width: Option<Length>,
    pub max_width: Option<Length>,
    pub height: Option<Length>,
    pub min_height: Option<Length>,
    pub max_height: Option<Length>,
    /// `object-fit`：替换元素（图片）在盒里怎么摆。不继承。
    pub object_fit: ObjectFit,
    /// 出血边（多看的 `duokan-bleed`）。不继承：它说的是**这一张图**怎么铺。
    pub bleed: Bleed,
    /// `box-sizing`：上面那六格的数含不含边框与内边距。不继承（CSS 规定）。
    pub box_sizing: BoxSizing,
    /// 分片意图。见 `fragment` 模块：连续滚动 / 分页 / 双页对开共用同一套分片代码，
    /// 差别只是「可用高度」这一个数。
    pub break_before: BreakWhen,
    pub break_after: BreakWhen,
    /// `break-inside: avoid`：整块尽量不被切开。装不下一整页时仍然必须切开 ——
    /// 排不出版来比违反作者意图更糟。
    pub break_inside_avoid: bool,
    /// 一段被切开后，前一片至少要留几行。CSS 初值 2。
    pub orphans: u16,
    /// 一段被切开后，后一片至少要留几行。CSS 初值 2。
    pub widows: u16,
    /// 连字符能不能当断点。见 [`Hyphens`]；CSS 里它是**可继承**的，
    /// 所以书在 `<body>` 上写一次 `hyphens: none` 就管全书。
    pub hyphens: Hyphens,
    /// `::first-letter` 算出来的那一份，只在书写过首字符样式时才有。
    ///
    /// 装的是**完整的 ComputedStyle** 而不是几个字段：首字符要的东西就是「一套字体」，
    /// 以后 `font-family` 的候选链、字距这些加进来不用再动这里。
    /// 排版层拿它给首字符单成一个 run，并据此决定它压住几行（`layout` 里的 `drop_cap`）。
    pub first_letter: Option<Box<ComputedStyle>>,
    /// `content` 声明：生成盒的文字从哪来。只对带 `::before` / `::after` 规则的元素有意义。
    pub content: GeneratedContent,
    /// `::before` / `::after` 各自算出来的那一份样式（含 `content`）。
    /// `None` = 这条伪元素没写过规则，不生成盒。
    pub before: Option<Box<ComputedStyle>>,
    pub after: Option<Box<ComputedStyle>>,
}

/// `content` 声明的取值。生成字符没有 DOM 节点，派生出来的 run 把 `orig` 设为所属
/// 元素的零长字节区间，因而不会移动选区、命中与进度的锚点。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum GeneratedContent {
    #[default]
    None,
    /// `content: "字面文字"`，去引号后的字符串。
    Literal(String),
    /// `content: open-quote` / `close-quote`：按继承的 [`ComputedStyle::quotes`] 取字符。
    OpenQuote,
    CloseQuote,
    /// `content: attr(名字)`：生成盒的文字取自所属元素的那个属性。
    Attr(String),
}

/// 把 open-quote / close-quote 按 `quotes` 折成字面字符。没有 quotes 表时
/// 生成盒不产生字符（与浏览器「quotes: none 时 open-quote 为空」一致）。
pub(crate) fn resolve_generated(
    content: GeneratedContent,
    quotes: Option<[QuotePair; 2]>,
) -> GeneratedContent {
    match content {
        GeneratedContent::OpenQuote => quotes
            .and_then(|q| q.last().copied())
            .map(|pair| GeneratedContent::Literal(pair.open.to_string()))
            .unwrap_or(GeneratedContent::None),
        GeneratedContent::CloseQuote => quotes
            .and_then(|q| q.last().copied())
            .map(|pair| GeneratedContent::Literal(pair.close.to_string()))
            .unwrap_or(GeneratedContent::None),
        other => other,
    }
}

impl Default for ComputedStyle {
    fn default() -> Self {
        Self {
            font_size: 16.0,
            line_height: 1.75,
            font_weight: 400,
            font_italic: false,
            font_stretch: FontStretch::Normal,
            small_caps: false,
            font_family: String::new(),
            font_families: Vec::new(),
            font_kerning: FontKerning::Auto,
            font_features: Arc::from(Vec::new()),
            text_rendering: TextRendering::Auto,
            font_smoothing: FontSmoothing::Auto,
            direction: Direction::Ltr,
            writing_mode: WritingMode::HorizontalTb,
            text_align: TextAlign::Start,
            text_justify: TextJustify::Auto,
            image_scale: crate::css::ImageScale::default(),
            text_indent: Length::ZERO,
            margin_top: Length::ZERO,
            margin_bottom: Length::ZERO,
            margin_left: Length::ZERO,
            margin_right: Length::ZERO,
            padding_top: Length::ZERO,
            padding_bottom: Length::ZERO,
            padding_left: Length::ZERO,
            padding_right: Length::ZERO,
            display: Display::Block,
            flex_direction: FlexDirection::default(),
            flex_wrap: FlexWrap::default(),
            align_items: AlignItems::default(),
            align_self: None,
            justify_content: JustifyContent::default(),
            row_gap: None,
            column_gap: None,
            column_count: None,
            column_width: None,
            column_fill: ColumnFill::Balance,
            column_rule: BorderEdge::default(),
            column_span: ColumnSpan::default(),
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: None,
            z_index: 0,
            opacity: 1.0,
            paint_opacity: 1.0,
            transform: None,
            white_space_nowrap: false,
            decoration_style: DecorationStyle::default(),
            decoration_color: None,
            text_emphasis: EmphasisStyle::default(),
            text_emphasis_color: None,
            text_emphasis_position: EmphasisPosition::default(),
            box_shadows: Arc::from(Vec::new()),
            outline: None,
            overflow_x: Overflow::default(),
            overflow_y: Overflow::default(),
            overflow_wrap_break: false,
            visibility: Visibility::default(),
            table_layout_fixed: false,
            svg_fill: SvgPaint::Color(Color(0, 0, 0, 255)),
            svg_stroke: SvgPaint::None,
            svg_stroke_width: 1.0,
            svg_stroke_linecap: SvgLineCap::default(),
            svg_stroke_linejoin: SvgLineJoin::default(),
            text_transform: TextTransform::None,
            text_combine: TextCombine::None,
            text_size_adjust: None,
            grid_template_columns: Arc::from(Vec::new()),
            background_clip: BackgroundClip::BorderBox,
            background_origin: BackgroundClip::PaddingBox,
            grid_column_start: None,
            grid_column_span: 1,
            grid_row_start: None,
            grid_row_span: 1,
            justify_self: JustifySelf::Auto,
            quotes: None,
            list_style_type: ListStyle::default(),
            float: Float::default(),
            clear: Clear::default(),
            color: None,
            background_color: None,
            background_layers: Vec::new(),
            backdrop_blur: None,
            border: [BorderEdge::default(); 4],
            border_collapse: false,
            border_spacing: [0.0; 2],
            border_radius: [Length::ZERO; 4],
            word_break: WordBreak::Normal,
            cell_valign: CellVAlign::Top,
            ruby_align: RubyAlign::Auto,
            ruby_position: RubyPosition::Over,
            position: Positioning::Static,
            left: None,
            right: None,
            top: None,
            bottom: None,
            text_shadows: Arc::from(Vec::new()),
            word_spacing: 0.0,
            letter_spacing: 0.0,
            baseline_shift: 0.0,
            inline_valign: InlineVAlign::Baseline,
            underline: false,
            line_through: false,
            link: false,
            cursor: Cursor::Auto,
            white_space_pre: false,
            width: None,
            min_width: None,
            max_width: None,
            height: None,
            min_height: None,
            max_height: None,
            object_fit: ObjectFit::default(),
            bleed: Bleed::NONE,
            box_sizing: BoxSizing::default(),
            break_before: BreakWhen::Auto,
            break_after: BreakWhen::Auto,
            break_inside_avoid: false,
            // 2 是 CSS 的初值，不是我们挑的：一行的孤行（或寡行）在任何排版传统里都算缺陷。
            orphans: 2,
            widows: 2,
            hyphens: Hyphens::Manual,
            first_letter: None,
            content: GeneratedContent::None,
            before: None,
            after: None,
        }
    }
}

/// 手写 `Hash`：块级排版缓存要按「**这个块子树的样式**」判命中（改一个块的样式，
/// 只有那一块失效），所以需要一份能覆盖**每个字段**的样式哈希。`ComputedStyle` 带
/// `f32`，不能 derive `Hash`，于是逐字段手写。
///
/// **新增字段时必须在这里补一行**，否则漏掉的字段改了而哈希不变，块级缓存会读到过期结果。
/// 浮点走 `to_bits`：`0.0` 与 `-0.0` 会被当成不同的值，最坏也只是多一次未命中，不会错命中。
impl std::hash::Hash for ComputedStyle {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        fn f<H: std::hash::Hasher>(x: f32, h: &mut H) {
            x.to_bits().hash(h);
        }
        fn len<H: std::hash::Hasher>(l: &Length, h: &mut H) {
            match l {
                Length::Px(v) => {
                    1u8.hash(h);
                    f(*v, h);
                }
                Length::Percent(p) => {
                    2u8.hash(h);
                    f(*p, h);
                }
                // `auto` 没有数可折，档位本身就是键里的那一格。
                Length::Auto => 3u8.hash(h),
            }
        }
        fn opt_len<H: std::hash::Hasher>(l: &Option<Length>, h: &mut H) {
            match l {
                None => 0u8.hash(h),
                Some(inner) => len(inner, h),
            }
        }
        fn axis<H: std::hash::Hasher>(a: &BackgroundAxis, h: &mut H) {
            match a {
                BackgroundAxis::Edge(edge, offset) => {
                    1u8.hash(h);
                    match edge {
                        BackgroundEdge::Start => 0u8.hash(h),
                        BackgroundEdge::Center => 1u8.hash(h),
                        BackgroundEdge::End => 2u8.hash(h),
                    }
                    opt_len(offset, h);
                }
                BackgroundAxis::Length(l) => {
                    2u8.hash(h);
                    len(l, h);
                }
                BackgroundAxis::Fraction(p) => {
                    3u8.hash(h);
                    f(*p, h);
                }
            }
        }
        fn tile<H: std::hash::Hasher>(t: &BackgroundTile, h: &mut H) {
            match t {
                BackgroundTile::Repeat => 0u8.hash(h),
                BackgroundTile::Space => 1u8.hash(h),
                BackgroundTile::Round => 2u8.hash(h),
                BackgroundTile::NoRepeat => 3u8.hash(h),
            }
        }
        f(self.font_size, h);
        f(self.line_height, h);
        self.font_weight.hash(h);
        self.font_italic.hash(h);
        (self.font_stretch as u8).hash(h);
        self.small_caps.hash(h);
        self.font_family.hash(h);
        self.font_families.hash(h);
        (self.font_kerning as u8).hash(h);
        self.font_features.len().hash(h);
        for (tag, value) in self.font_features.iter() {
            tag.hash(h);
            value.hash(h);
        }
        (self.text_rendering as u8).hash(h);
        (self.font_smoothing as u8).hash(h);
        (self.direction as u8).hash(h);
        (self.writing_mode as u8).hash(h);
        (self.text_align as u8).hash(h);
        (self.text_justify as u8).hash(h);
        (self.image_scale as u8).hash(h);
        len(&self.text_indent, h);
        len(&self.margin_top, h);
        len(&self.margin_bottom, h);
        len(&self.margin_left, h);
        len(&self.margin_right, h);
        len(&self.padding_top, h);
        len(&self.padding_bottom, h);
        len(&self.padding_left, h);
        len(&self.padding_right, h);
        (self.display as u8).hash(h);
        (self.flex_direction as u8).hash(h);
        (self.flex_wrap as u8).hash(h);
        (self.align_items as u8).hash(h);
        self.align_self.map(|v| v as u8).hash(h);
        (self.justify_content as u8).hash(h);
        opt_len(&self.row_gap, h);
        opt_len(&self.column_gap, h);
        self.column_count.hash(h);
        opt_len(&self.column_width, h);
        (self.column_fill as u8, self.column_span as u8).hash(h);
        self.column_rule.width.to_bits().hash(h);
        (self.column_rule.style as u8, self.column_rule.color).hash(h);
        f(self.flex_grow, h);
        f(self.flex_shrink, h);
        opt_len(&self.flex_basis, h);
        self.z_index.hash(h);
        f(self.opacity, h);
        f(self.paint_opacity, h);
        match &self.transform {
            None => 0u8.hash(h),
            Some(Transform::Translate(a, b)) => {
                1u8.hash(h);
                len(a, h);
                len(b, h);
            }
            Some(Transform::Scale(x, y)) => {
                2u8.hash(h);
                f(*x, h);
                f(*y, h);
            }
        }
        self.white_space_nowrap.hash(h);
        (self.decoration_style as u8).hash(h);
        self.decoration_color.hash(h);
        (self.text_emphasis as u8).hash(h);
        self.text_emphasis_color.hash(h);
        (self.text_emphasis_position as u8).hash(h);
        self.box_shadows.len().hash(h);
        for shadow in self.box_shadows.iter() {
            f(shadow.dx, h);
            f(shadow.dy, h);
            f(shadow.blur, h);
            shadow.inset.hash(h);
            shadow.color.hash(h);
        }
        // outline: (宽, 色, 画法, 偏移)
        match &self.outline {
            None => 0u8.hash(h),
            Some((w, color, style, offset)) => {
                1u8.hash(h);
                f(*w, h);
                color.hash(h);
                (*style as u8).hash(h);
                f(*offset, h);
            }
        }
        (self.overflow_x as u8).hash(h);
        (self.overflow_y as u8).hash(h);
        self.overflow_wrap_break.hash(h);
        (self.visibility as u8).hash(h);
        self.table_layout_fixed.hash(h);
        self.svg_fill.hash(h);
        self.svg_stroke.hash(h);
        self.svg_stroke_width.to_bits().hash(h);
        (self.svg_stroke_linecap as u8).hash(h);
        (self.svg_stroke_linejoin as u8).hash(h);
        (self.text_transform as u8).hash(h);
        self.text_combine.hash(h);
        self.text_size_adjust.map(f32::to_bits).hash(h);
        self.grid_template_columns.len().hash(h);
        (self.background_clip as u8).hash(h);
        self.backdrop_blur.map(f32::to_bits).hash(h);
        (self.background_origin as u8).hash(h);
        self.grid_column_start.hash(h);
        self.grid_column_span.hash(h);
        self.grid_row_start.hash(h);
        self.grid_row_span.hash(h);
        (self.justify_self as u8).hash(h);
        for track in self.grid_template_columns.iter() {
            match track {
                GridTrack::Length(l) => {
                    0u8.hash(h);
                    len(l, h);
                }
                GridTrack::Fr(v) => {
                    1u8.hash(h);
                    f(*v, h);
                }
                GridTrack::MinMax(a, b) => {
                    2u8.hash(h);
                    len(a, h);
                    len(b, h);
                }
                GridTrack::MinMaxFr(a, fr) => {
                    4u8.hash(h);
                    len(a, h);
                    f(*fr, h);
                }
                GridTrack::Auto => 3u8.hash(h),
            }
        }
        self.quotes.hash(h);
        (self.list_style_type as u8).hash(h);
        (self.float as u8).hash(h);
        (self.clear as u8).hash(h);
        self.color.hash(h);
        self.background_color.hash(h);
        // 背景图层一族。漏了这一格的表现不是「画错」而是「画旧」：只改 `background-size`
        // 时块级缓存命中上一代，盒子和图都按旧的摆 —— 与上面那条「新增字段必须补一行」
        // 讲的是同一件事，这里就是那一条新写的字段。
        self.background_layers.len().hash(h);
        for layer in &self.background_layers {
            layer.image.hash(h);
            match &layer.size {
                BackgroundSize::Auto => 0u8.hash(h),
                BackgroundSize::Contain => 1u8.hash(h),
                BackgroundSize::Cover => 2u8.hash(h),
                BackgroundSize::Explicit { width, height } => {
                    3u8.hash(h);
                    opt_len(width, h);
                    opt_len(height, h);
                }
            }
            axis(&layer.position.x, h);
            axis(&layer.position.y, h);
            tile(&layer.repeat.x, h);
            tile(&layer.repeat.y, h);
            (layer.attachment as u8).hash(h);
        }
        for edge in &self.border {
            f(edge.width, h);
            (edge.style as u8).hash(h);
            edge.color.hash(h);
        }
        f(self.word_spacing, h);
        f(self.letter_spacing, h);
        f(self.baseline_shift, h);
        (self.inline_valign as u8).hash(h);
        self.underline.hash(h);
        self.line_through.hash(h);
        // 「在一条链接里」这一格 2026-11-04 之前漏在清单外。漏掉的不是「画错」而是「画旧」：
        // 它决定绘制层要不要换成主题的链接色，而块级缓存按这份哈希判命中。
        self.link.hash(h);
        (self.cursor as u8).hash(h);
        // 「在一条链接里」这一格 2026-11-04 之前漏在清单外。漏掉的不是「画错」而是「画旧」：
        // 它决定绘制层要不要换成主题的链接色，而块级缓存按这份哈希判命中。
        self.white_space_pre.hash(h);
        opt_len(&self.width, h);
        opt_len(&self.min_width, h);
        opt_len(&self.max_width, h);
        opt_len(&self.height, h);
        opt_len(&self.min_height, h);
        opt_len(&self.max_height, h);
        self.object_fit.hash(h);
        self.bleed.hash(h);
        self.box_sizing.hash(h);
        self.border_collapse.hash(h);
        f(self.border_spacing[0], h);
        f(self.border_spacing[1], h);
        self.word_break.hash(h);
        self.cell_valign.hash(h);
        self.ruby_align.hash(h);
        self.ruby_position.hash(h);
        self.position.hash(h);
        opt_len(&self.left, h);
        opt_len(&self.right, h);
        opt_len(&self.top, h);
        opt_len(&self.bottom, h);
        for radius in &self.border_radius {
            len(radius, h);
        }
        // 影子列表：先把条数进哈希，再逐条按值 —— 少了条数这一行，`[a, b]` 与 `[a, b, c]`
        // 只靠逐条哈希还分得开，但空表与「一条全零影子」会撞在一起。
        self.text_shadows.len().hash(h);
        for shadow in self.text_shadows.iter() {
            shadow.hash(h);
        }
        (self.break_before as u8).hash(h);
        (self.break_after as u8).hash(h);
        self.break_inside_avoid.hash(h);
        self.orphans.hash(h);
        self.widows.hash(h);
        (self.hyphens as u8).hash(h);
        match &self.first_letter {
            None => 0u8.hash(h),
            Some(inner) => {
                1u8.hash(h);
                inner.hash(h);
            }
        }
        self.content.hash(h);
        for pseudo in [&self.before, &self.after] {
            match pseudo {
                None => 0u8.hash(h),
                Some(inner) => {
                    1u8.hash(h);
                    inner.hash(h);
                }
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct StyledDom {
    /// 按 `Dom::nodes` 下标索引；文本节点也有一条（整份继承父元素）。
    pub styles: Vec<ComputedStyle>,
    /// 认识不了的值。`KNOWN_UNIMPLEMENTED` 里的属性不计入。
    pub ignored: usize,
}

/// 一条声明在我们引擎里的落地情况。
///
/// 判定走的是 [`declaration_status`]，而它内部就是层叠用的同一个 `apply` ——
/// 所以「这本书有多少样式还没接」可以从外部统计出来，不会和实际渲染行为分叉。
/// 这份统计是 `examples/css-gap.rs` 的唯一依据，也是它 before/after 可比的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclStatus {
    /// 认得，且写进了 `ComputedStyle`。
    Applied,
    /// 属性名认得，但一期不生效（在 `KNOWN_UNIMPLEMENTED` 名单里）。
    Inert,
    /// 属性名或值不认识；`cascade` 会把这一条计入 `ignored`。
    Unparsed,
}

/// 判定一条声明的落地情况，不碰任何长期状态：在一份临时的 `ComputedStyle` 上试算。
///
/// `parent` 与 `root_px` 只影响 `em` / `%` 折算成多少像素，**不影响认不认得**，
/// 所以纯统计时可以一律传 `ComputedStyle::default()`。
/// `inherit` 这个关键字算 `Applied`：它是合法值，继承到的值由 `inherit_from` 负责给。
/// 名单里的属性不看值 —— 一期整体不生效，值写对写错都不改变「有没有生效」这件事。
pub fn declaration_status(decl: &Declaration, parent: &ComputedStyle, root_px: f32) -> DeclStatus {
    let value = decl.value.trim();
    if value.is_empty() {
        return DeclStatus::Unparsed;
    }
    if value.eq_ignore_ascii_case("inherit") {
        return DeclStatus::Applied;
    }
    if KNOWN_UNIMPLEMENTED.contains(&decl.property.as_str()) {
        return DeclStatus::Inert;
    }
    let mut scratch = parent.clone();
    if apply(&mut scratch, parent, root_px, decl) {
        DeclStatus::Applied
    } else {
        DeclStatus::Unparsed
    }
}

impl StyledDom {
    pub fn get(&self, id: NodeId) -> Option<&ComputedStyle> {
        self.styles.get(id as usize)
    }

    pub fn len(&self) -> usize {
        self.styles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.styles.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (NodeId, &ComputedStyle)> {
        self.styles
            .iter()
            .enumerate()
            .map(|(id, style)| (id as NodeId, style))
    }
}

/// 一份 `style="…"` 文本 → 声明列表，用的就是层叠内部那**同一个**解析（含按本章目录
/// 折算 `url(...)` 那一步）。抬到公开面只为一件事：缺口量具从前只遍历样式表规则，
/// 元素上写的内联声明一条也没进那台秤，而哈利波特那七张册扉页的底图恰恰全部写在
/// 内联里 —— 量具漏计一个形状，比引擎少接一个属性更坏。
pub fn inline_declarations(text: &str, base_dir: &str) -> Vec<Declaration> {
    parse_inline_style(text, base_dir)
}

#[cfg(test)]
mod tests;
