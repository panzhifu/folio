//! 把命中的声明算成 `ComputedStyle`：一条声明进哪几个字段 + 继承 + 沿树展开。
//!
//! 单个值怎么解成 Rust 在 [`super::value`]；这里只管属性名到字段的对应关系。
//!
//! 支持面就是 `computed/apply.rs` 里那批 match 臂（一条声明写进哪几个字段），加上
//! [`KNOWN_UNIMPLEMENTED`]（认得、这一版不生效）。清单外的声明**被忽略而不是报错**，
//! 因为真实出版书里总有些用不上的声明，为一两个 `transform` 让整本书排版失败
//! 是最糟的失败方式。

use crate::content::dom::{Dom, NodeId, NodeKind};

use super::{Declaration, RuleSet};

mod apply;

// `inherit_from` 原本就是 `pub(crate)`，`css::ua` 从这条路径取它，所以这里必须是再导出而非私有导入。
pub(crate) use apply::inherit_from;
use apply::{apply, apply_in_cascade_order, parse_inline_style};

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

/// 一段文字的推进轴与行的堆叠方向，即 CSS 的 `writing-mode`。
///
/// 它与 [`Direction`] 是**两根独立的轴**：`direction` 改的是「一行里的字符往哪边排、
/// `start`/`end` 落在物理的哪一边」，行的推进轴不变；`writing-mode` 改的是「字沿哪根轴
/// 推进、行沿哪根轴堆」。竖排的本子通常两样都写（`vertical-rl` 的书列序天然从右往左）。
///
/// **落地程度：排版层真的按这一根轴排了**（2026-10-19 那一轮）。架构是「逻辑横排 +
/// 一次转置」——引擎照旧排一条横长的带，`layout::Layout::assemble` 里的 `transposed`
/// 在折索引之前把两张表整体投影成物理坐标，所以 shaping、命中与选区那几套一行都不用改。
/// 界面唯一该知道轴的地方收成本文件的 [`WritingMode::project`] 与 [`project_delta`]。
///
/// 明确降级的那三处，都在 `layout::LayoutOptions::allow_vertical` 与 `transposed` 的文档里：
/// 盒模型那几族边是**物理**的（`margin-top` 这类落到转置后的错边，logical→physical
/// 那张表没接）；连续滚动与双页对开回退横排；行内图片不转置。
///
/// 触发源只有一处，就是书的 CSS：`writing-mode` 在真实竖排书里是**唯一**的竖排来源
/// （那些本子不会另外写别的声明），所以这一格必须先接住层叠，报表才数得出库里哪几本
/// 声明了竖排。读者那一档没有加，落点埋在 `ui::features::reader::model::chapter_layout`
/// 的 `reader_rules` 注释里。
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
fn first_strong_direction(text: &str) -> Option<Direction> {
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
    InlineBlock,
    ListItem,
    /// `display: none`：排版层负责整块跳过。
    None,
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

/// 一条边的画法。CSS 的 `border-style` 有八种实线以外的样式；这里只接
/// `none` / `hidden`（都不画）与 `solid` / `double` —— 出版书的章标题色带左边那道
/// 双线就是 `double`，正文插图框是 `solid`。`dashed` / `dotted` / `groove` 这些
/// 解析失败、按「没接住」计数：把它们悄悄画成实线，书里的虚线框就变成了另一回事。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BorderStyle {
    #[default]
    None,
    Solid,
    Double,
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

/// `float`：盒子贴到所在块的哪一侧，后面的行带在它旁边让出宽度。
///
/// CSS 3 的 `inline-start` / `inline-end` / `float-inherit` 不接（判不认识）：那三个
/// 要的是「方向敏感的左右」，而我们这一族里唯一真实的写法（出版书的插图与章首下沉）
/// 都写死 `left` / `right`。
///
/// 落地分两条路，行内盒那一条只走了一半，这里说清：
///
/// - **块级盒**（UA 判成 block 的 `<div>` / `<p>` …）走 `layout::collect::place_float`，
///   真的离开常规流。它的宽认两档：作者写了 `width`（`.muen .left { width:20%; float:left }`），
///   或整块内容就是一张图（`div.img1` 那一族）。两档都对不上时按不浮动排。
///   max-content 那种 shrink-to-fit 还没做 —— 猜宽会把整页正文挤成一条窄柱。
/// - **行内盒**（`<span class=first>` 那种章首下沉）只有**站在这一段第一片**时才生效：
///   `wrap_lines` 把它收成下沉盒（`Ctx::drop_cap`），按它自己的宽高跨几行让宽。
///   落在段中或段尾的行内浮动不认 —— 那要的是「行内盒改判成块、并从行内流里摘出去」
///   那一步（CSS 的 used-value 规则），动的是 `children` 的整条分流，比这一格大。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Float {
    #[default]
    None,
    Left,
    Right,
}

/// `clear`：这一块（的行盒）不许与哪一侧的浮动盒并排，于是往下挪到它们的下缘。
///
/// `left` / `right` 是真的按那一侧算的，不是都折成 `both`：那会让「右边还空着」
/// 的写法白白多挪一段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Clear {
    #[default]
    None,
    Left,
    Right,
    Both,
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
}

/// 连字符相关的折行意图，来自 `hyphens`（含 EPUB 的 `-epub-` / `-webkit-` 前缀写法）。
///
/// 一期没有连字词典，所以 `Auto` 与 `Manual` 的**行为**相同：都只在已有的连字符处断，
/// 不会把 `sais` 拆成 `sa-is`。差别留在「作者的意图有没有被记下来」——词典接上时不用
/// 回头改书的样式表。这条边界记在 `plans/facts.md` 的 CSS 一节。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Hyphens {
    /// CSS 初值：已有连字符可断，不主动造连字符。
    #[default]
    Manual,
    /// 连字符**不是**断点：`je-ne-sais-quoi` 整个留着。
    None,
    /// 请求词典连字；一期按 `Manual` 执行。
    Auto,
}

/// 分片（分页 / 分栏）时的断点意图。
///
/// `Left` / `Right` 会补出空片占位，「双页对开」的左右页才不会错位；
/// `Always` 是 CSS2.1 的旧写法，与 `Page` 同义 —— 老 EPUB 里满地都是，
/// 认不出它就等于把这些书的作者意图丢掉。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BreakWhen {
    /// 没有声明：由剩余可用高度决定。
    #[default]
    Auto,
    /// 这里尽量不断（`break-after: avoid` 常见于标题，意思是别把标题孤零零留在页尾）。
    Avoid,
    Page,
    Left,
    Right,
    Always,
}

impl BreakWhen {
    /// 是不是「强制断在这里」的意图。`Avoid` 与 `Auto` 都不是。
    pub fn is_forced(self) -> bool {
        matches!(self, Self::Page | Self::Left | Self::Right | Self::Always)
    }

    /// 强制断之后应当落在哪一侧。`None` = 不关心奇偶。
    pub fn parity(self) -> Option<bool> {
        match self {
            Self::Left => Some(false),
            Self::Right => Some(true),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color(pub u8, pub u8, pub u8);

/// 一个尺寸值。`none` 用 `Option::None` 表示。百分比不在此刻折算 —— 它要按**包含块
/// 的宽度**来算，那是排版期才知道的数，见 [`Length::resolve`]。
///
/// 带百分比的字段都存这个类型，不是只有替换元素的 `width`：外边距、内边距与
/// `text-indent` 的百分比在 CSS 里同样折算自包含块宽，而那一族写法在真书上是大头
/// （多看转换的本每段都带 `margin: 0 1%`）。层叠期把它折成 px 等于拿窗口宽猜。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Length {
    Px(f32),
    /// 比例，`50%` 存成 `0.5`。
    Percent(f32),
    /// `auto`。**只有外边距会走到这一档**：CSS 里 `padding: auto` 与 `text-indent: auto`
    /// 是非法值（层叠期就判不认识），`width: auto` 那一族用 `Option::None` 表示。
    ///
    /// 为什么不在层叠期折成 0：CSS 2.1 §10.3.3 里 `margin-left: auto` 与
    /// `margin-right: auto` 是「把这一块横向剩余宽度均分」，而剩余宽度取决于**作者的
    /// `width` 折出来是多少**，那是排版期才有的一对数。折成 0 等于宣布「本书没有块级
    /// 居中」，症状是写着 `margin: 0 auto` 的定宽插图与分隔条一律贴左缘。
    /// 求解那一步在 `layout::collect::solve_block_width`；[`Length::resolve`] 给 `auto`
    /// 回 0，所以**没接进那条求解式的调用点行为与以前逐字相同**。
    Auto,
}

impl Length {
    /// 每个长度字段的初值。`Px(0.0)` 而不是 `None`：`None` 在这一族里表示
    /// 「作者写了 `auto` / `none`」，与「没写过」是两件事。
    pub const ZERO: Length = Length::Px(0.0);

    /// 按包含块基准长度折算成 px。`Auto` 给 0 —— 它真正的用处由排版层拿去均分
    /// （见 [`Length::Auto`]），单看这一格就是「不占位」。
    pub fn resolve(self, basis: f32) -> f32 {
        match self {
            Length::Px(v) => v,
            Length::Percent(p) => p * basis,
            Length::Auto => 0.0,
        }
    }

    /// 这一格是不是 `auto`。横向求解要靠它分辨「作者写的 0」与「作者要均分」。
    pub fn is_auto(self) -> bool {
        matches!(self, Length::Auto)
    }
}

/// 排版层需要的计算后样式。除 `margin` / `padding` / `text-indent` / 替换元素的
/// 尺寸这一族（它们可能带百分比，要等排版期拿到包含块宽才折得动，见 [`Length`]），
/// 长度一律已折算成 px，`line_height` 是倍数。
#[derive(Debug, Clone, PartialEq)]
pub struct ComputedStyle {
    pub font_size: f32,
    pub line_height: f32,
    pub font_weight: u16,
    pub font_italic: bool,
    pub font_family: String,
    /// `font-family` 的**整条候选链**，按书写顺序；`font_family` 就是它的第一个。
    ///
    /// 只留第一个是以前的一处真落差：书里写 `font-family: "MyFont", "Noto Serif CJK SC"`
    /// 时，设备上没装 `MyFont` 就直接落回读者的字体，作者排在第二位的那一份明明装着。
    /// 逐个试的工作在平台层做（只有它知道这台设备注册了哪些家族，见 `ui::reader::measure`）。
    pub font_families: Vec<String>,
    /// 书写方向。CSS 的 `direction`，**可继承**；来源有三处：作者样式表、元素的
    /// `dir` 属性（HTML 的 presentational hint，优先级在作者样式之下），以及继承。
    /// 见 [`Direction`] 那一段「做到哪一层」的说明。
    pub direction: Direction,
    /// 推进轴（CSS 的 `writing-mode`），**可继承**：竖排的本子在 `<html>` 或 `<body>` 上
    /// 写一次就管全书，靠的是 `inherit_from` 里那一行，与 `direction` 同一条通路。
    /// 落地程度（层叠接住、排版层还没读）见 [`WritingMode`]。
    pub writing_mode: WritingMode,
    pub text_align: TextAlign,
    /// `text-justify`：两端对齐时余量往哪种隙摊。见 [`TextJustify`]；CSS 里**可继承**。
    pub text_justify: TextJustify,
    /// 首行缩进。百分比折算自**这一块的包含块宽**（父块的内容宽），与 `margin` 同一条
    /// 口径。读者那条「首行缩进（em）」从 `ui::features::reader::document` 注入时已是 px。
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
    /// 四边边框，顺序 `[上, 右, 下, 左]`（与 `box_sides` 同一套展开规则）。
    /// 布局拿它算盒宽与盒高，绘制拿它画线；不继承。
    pub border: [BorderEdge; 4],
    /// 词间额外宽度（px）。CSS 初值 `normal` 折成 0；它叠在**每个空格之后**，
    /// 所以折行与行宽都要算它（见 `layout::run_width`）。
    pub word_spacing: f32,
    /// 字间额外宽度（px）。CSS 初值 `normal` 折成 0；它叠在**每个字符之后**（含最后一个），
    /// 所以折行、行宽、绘制与命中四处都要算它。
    ///
    /// 这四处**必须同批改**（见 `plans/阅读管线.md` §11 第 15 条）：只改宽度表会出现
    /// 「断行提前了、字却还挤在一起」，比不做更糟。绘制按字形所在字符序号右推，
    /// 命中/选区按同一偏移推 `char_edges`。
    pub letter_spacing: f32,
    /// 基线偏移，px，**正数往上**。`vertical-align: super` 折出来的就是它。
    ///
    /// 0 意味着「贴基线」（CSS 的 `baseline`）。`top` / `bottom` / `middle` 要的是
    /// 行盒对齐机制，一期不折成这个数，并且按不认识计数 —— 悄悄当 0 会把作者的
    /// 上标写成正常位置还查不出来。
    pub baseline_shift: f32,
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
    pub white_space_pre: bool,
    /// 尺寸四件套。**一律按内容盒解释**（`box-sizing` 不认，见下）。落点分两条：
    ///
    /// - 替换元素（`<img>` / `<svg>`）：`layout::ports::image_size` 拿它与固有尺寸、
    ///   可用宽一起折出这张图实际多大。这一条早就生效。
    /// - 流内块级盒：`width` / `max-width` 定出这一块的边框盒宽
    ///   （`layout::collect::solve_block_width`），于是写着 `margin: 0 auto` 的定宽插图
    ///   与分隔条第一次能居中；`height` / `max-height` 定出盒高与游标推进量，
    ///   **内容照排、溢出可见** —— 与浏览器同一条规则，不是我们的取巧。
    ///   `<body>` 那一层除外：根不吃 `width`，版心归视口与读者设定，判据写在
    ///   `layout::layout_inner`。
    ///
    /// `height` / `max-height` 的百分比在这里落空：它们的基准是包含块的**高**，
    /// 而流式排版里父块的高本来就是由内容算出来的，CSS 在同一条规则下也判 `auto`。
    /// `min-width` 连属性名都不在清单里（计「不认识」），`min-height` 认得、不生效。
    pub width: Option<Length>,
    pub height: Option<Length>,
    pub max_width: Option<Length>,
    pub max_height: Option<Length>,
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
}

impl Default for ComputedStyle {
    fn default() -> Self {
        Self {
            font_size: 16.0,
            line_height: 1.75,
            font_weight: 400,
            font_italic: false,
            font_family: String::new(),
            font_families: Vec::new(),
            direction: Direction::Ltr,
            writing_mode: WritingMode::HorizontalTb,
            text_align: TextAlign::Start,
            text_justify: TextJustify::Auto,
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
            list_style_type: ListStyle::default(),
            float: Float::default(),
            clear: Clear::default(),
            color: None,
            background_color: None,
            border: [BorderEdge::default(); 4],
            word_spacing: 0.0,
            letter_spacing: 0.0,
            baseline_shift: 0.0,
            underline: false,
            line_through: false,
            link: false,
            white_space_pre: false,
            width: None,
            height: None,
            max_width: None,
            max_height: None,
            break_before: BreakWhen::Auto,
            break_after: BreakWhen::Auto,
            break_inside_avoid: false,
            // 2 是 CSS 的初值，不是我们挑的：一行的孤行（或寡行）在任何排版传统里都算缺陷。
            orphans: 2,
            widows: 2,
            hyphens: Hyphens::Manual,
            first_letter: None,
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
        f(self.font_size, h);
        f(self.line_height, h);
        self.font_weight.hash(h);
        self.font_italic.hash(h);
        self.font_family.hash(h);
        self.font_families.hash(h);
        (self.direction as u8).hash(h);
        (self.writing_mode as u8).hash(h);
        (self.text_align as u8).hash(h);
        (self.text_justify as u8).hash(h);
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
        (self.list_style_type as u8).hash(h);
        (self.float as u8).hash(h);
        (self.clear as u8).hash(h);
        self.color.hash(h);
        self.background_color.hash(h);
        for edge in &self.border {
            f(edge.width, h);
            (edge.style as u8).hash(h);
            edge.color.hash(h);
        }
        f(self.word_spacing, h);
        f(self.letter_spacing, h);
        f(self.baseline_shift, h);
        self.underline.hash(h);
        self.line_through.hash(h);
        self.white_space_pre.hash(h);
        opt_len(&self.width, h);
        opt_len(&self.height, h);
        opt_len(&self.max_width, h);
        opt_len(&self.max_height, h);
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
    }
}

/// 认识但一期不实现：接受、不生效、不计入 ignored。
///
/// 这一份和下面 `inherit_from` 的区别在于：这些属性**真实出版书里满地都是**
/// （`width`、`text-decoration`、`float`），若按不支持计数，ignored 会被刷到
/// 几百条，真正需要关注的「我们没预料到的写法」就淹没了。
pub(super) const KNOWN_UNIMPLEMENTED: &[&str] = &[
    // `background` 简写 2026-10-14 起**接住了它里面的颜色**（见 `apply` 的 `background`
    // 那一臂）：出版书里「深色底块 + 白字标题」满地都是，只认 `background-color`
    // 会让这种标题变成白纸上的白字、整条标题消失。简写里的图（`url(...)`）仍不接。
    "min-height",
    "text-shadow",
    // `direction` 以前在这份名单里（「认得、接受、不生效」），2026-10-14 那一轮把它
    // 变成真生效的属性，所以移出去了。
    // `writing-mode` 同一条路，2026-10-19 那一轮移出去的，而且那一轮排版层真读到了它：
    // 竖排按「逻辑横排 + 一次转置」排，降级清单写在 [`WritingMode`] 的落地程度那一段。
    // 这一档里「认得且不生效」讲的是**这条声明会不会被记进 ignored**，
    // 不是屏上有没有变化；两件事的差别就以这一格为例 —— 移出去的理由是「已经生效」。
    //
    // `orphans` / `widows` / `break-inside` / `page-break-*` 以前在这份名单里
    // （「接受、不生效」），M4 的分片器把它们变成真生效的属性，所以移出去了。
    // `float` / `clear` 同一条路，2026-10-16 那一轮移出去的 —— 现在真会摆浮动盒、
    // 真会把后面的行带折窄，见 `layout::collect` 的 `PlacedFloat`。
    // `columns` / `column-gap` 留下：分片器认的是**视口**栏数（`FragmentOptions::columns`，
    // 由界面或读者给），作者写的块级 `column-count` 要改排版期的折行宽度，还没接。
    // 详见 `plans/facts.md` 与 `css/computed.rs` 的忽略清单。
    "columns",
    "column-gap",
    // 列表标记 2026-10-15 起真的画了，所以 `list-style` 与 `list-style-type` 从这份
    // 名单里移了出去 —— 它们现在会改屏上的东西（`layout::marker`）。
    // 留下的两条：`list-style-image` 没有图源可画（标记只有文字那一条路），
    // `list-style-position` 则是 `inside` / `outside` 一律按 `outside` 摆 ——
    // 听得出、也画得出标记，但作者要的那一处差别给不了。
    "list-style-image",
    "list-style-position",
    // 它会改变字符本身，而锚点与进度都建立在**源文本字节偏移**上（`Locator` 的坐标系）：
    // `capitalize` / `ß → SS` 这类一改就让偏移对不上。要接就得连 M5 的锚点映射一起定，
    // 不是加一条 `to_uppercase()` 的事。
    "text-transform",
    // 字体的 OpenType 层：小写大写（`font-variant` / `font-variant-caps`）、横向压缩
    // （`font-stretch`）、字距调整开关（`font-kerning`）、特性开关表（`font-feature-settings`）。
    // 这四条以前**不在这份名单里**，于是被算成「不认识」，`examples/css-gap` 的表就会说
    // 「这些属性我们听都没听过」—— 事实是认得，但要的是 gpui 的 `FontFeatures` 通道
    // （见开发方案 §3.2），那是接字体文件那一层的事，不是层叠期能折出来的数。
    "font-variant",
    "font-variant-caps",
    "font-stretch",
    "font-kerning",
    "font-feature-settings",
    // 字距 2026-10-16 起真的生效了（`letter-spacing` 已从这份名单移除），所以这里不再列。
    //
    // 定位一族。**不做绝对定位**：`position: absolute|fixed` 要的是「脱离常规流、按包含块
    // 换算偏移」那一整套，而我们的排版从头到尾假定块沿游标自上而下摆（`Layout::stacked`
    // 那格成立的前提）。真接得上先动 `layout::collect` 的整条流动，代价远大于它在真书里的
    // 收益 —— 本机四本书里带非 static 定位的元素是个位数。所以四个偏移属性与 `z-index`
    // 一并记「认得，不生效」：书里那处写法不再被报成「我们听都没听过」。
    "position",
    "left",
    "right",
    "top",
    "bottom",
    "z-index",
];

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

/// 层叠整棵树。`root_base` 是读者偏好折算出来的根样式（字号、行距、字体等）。
pub fn cascade(dom: &Dom, rules: &RuleSet, root_base: &ComputedStyle) -> StyledDom {
    let mut styles = vec![root_base.clone(); dom.nodes.len()];
    let mut ignored = 0usize;

    // 先序遍历：算任一节点时它的父样式必定已算完，继承只需往上走一步。
    for id in document_order(dom) {
        let node = dom.get(id);
        let parent = match node.parent {
            Some(p) => styles[p as usize].clone(),
            None => root_base.clone(),
        };
        if node.kind != NodeKind::Element {
            // 文本节点没有选择器可言，直接沿用父元素算完的样式。
            styles[id as usize] = parent;
            continue;
        }
        // 空标签名命中不到任何 UA 规则，等价于「只继承」。
        let mut style =
            crate::content::format::css::ua::seed(&parent, dom.tag_name(id).unwrap_or_default());
        // 「在一条链接里」这一格来自 DOM 而不是样式表（判据与理由见 [`ComputedStyle::link`]）：
        // 空 `href`（`<a name="x">` 那种老式锚点）不算链接 —— 它点不动，染蓝等于骗读者。
        if dom.is_element(id, "a")
            && dom
                .attr(id, "href")
                .is_some_and(|href| !href.trim().is_empty())
        {
            style.link = true;
        }
        // HTML 的 `dir="rtl|ltr"` 是一条**呈现提示**（presentational hint）：优先级在作者
        // 样式表之下、UA 默认之上，所以落在 `apply_in_cascade_order` 之前 —— 作者真写了
        // `direction` 就把这一格盖掉。RTL 的书在 EPUB 里几乎都靠根元素这一个属性标注，
        // 只认样式表等于整本不认：出版工具很少往 RTL 书里另外写 `direction` 声明。
        //
        // `auto` 有意不当 RTL 用（也不当 LTR 用 —— 它就落回继承值）。那档要按「第一个强
        // 方向字符」判，而我们没有那份字符表：`unicode-bidi` 只在依赖图里（gpui 的文本栈
        // 带进来的），没转成直接依赖。猜错的方向比不猜更难查，所以宁可少接一档。
        if let Some(dir) = dom.attr(id, "dir") {
            match dir.trim().to_ascii_lowercase().as_str() {
                "ltr" => style.direction = Direction::Ltr,
                "rtl" => style.direction = Direction::Rtl,
                "auto" => {
                    // `auto` 要**看内容**才能定：第一个强方向字符是哪一边。判得出就订下来，
                    // 一个强方向字符都没有就留继承值 —— 「不知道」不等于「左」。
                    // 这一档常见到不能不做：聊天与社媒导出的 HTML 给每一段写 `<p dir="auto">`。
                    if let Some(found) = first_strong_direction(&dom.text_of_subtree(id)) {
                        style.direction = found;
                    }
                }
                _ => {}
            }
        }
        let decls = rules.matching_declarations(dom, id);
        // 元素的 `style="…"`。CSS 把它排在「作者普通声明」之上、`!important` 那两档之下：
        // 整份追加到最后会让行内普通声明盖掉书里的 `color: red !important`，
        // 放在最前又等于作者的行内样式没有优先权。
        // `matching_declarations` 已经按层叠级别从低到高排好，而**普通声明全在重要声明
        // 之前**，所以按 `important` 一刀切成两半，中间插入行内那两半，就是那个位置。
        let inline = dom
            .attr(id, "style")
            .map(parse_inline_style)
            .unwrap_or_default();
        let mut ordered: Vec<&Declaration> = Vec::with_capacity(decls.len() + inline.len());
        ordered.extend(decls.iter().filter(|d| !d.important));
        ordered.extend(inline.iter().filter(|d| !d.important));
        ordered.extend(inline.iter().filter(|d| d.important));
        ordered.extend(decls.iter().filter(|d| d.important));

        ignored += apply_in_cascade_order(&mut style, &parent, root_base.font_size, &ordered);

        // `::first-letter`：首字符的盒要等这个元素自己算完才能起算（`3em` 的基准
        // 就是它），所以放在后面。一份都没命中时留 `None`，排版层因此完全不走下沉那条路。
        let cap_decls = rules.matching_first_letter(dom, id);
        if !cap_decls.is_empty() {
            let mut cap = style.clone();
            // `parent` 传的是**这个元素自己**：`::first-letter` 的父盒就是它，
            // 所以 `font-size: 3em` 的 em 按段落自己的字号折，不是按段落的父。
            ignored += apply_in_cascade_order(&mut cap, &style, root_base.font_size, &cap_decls);
            style.first_letter = Some(Box::new(cap));
        }
        styles[id as usize] = style;
    }
    StyledDom { styles, ignored }
}

fn document_order(dom: &Dom) -> Vec<NodeId> {
    let mut out = Vec::with_capacity(dom.nodes.len());
    let mut stack = vec![dom.document];
    while let Some(id) = stack.pop() {
        out.push(id);
        let children = dom.get(id).children.clone();
        stack.extend(children.iter().rev().copied());
    }
    out
}

#[cfg(test)]
mod tests;
