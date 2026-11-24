//! 值类型：分片/连字符意图、颜色、SVG 呈现属性的小枚举。
//!
//! 从 `css::computed` 切出来只因为 computed.rs 贴着行数上限；对外路径经 `pub use` 保持不变。

/// 连字符相关的折行意图，来自 `hyphens`（含 EPUB 的 `-epub-` / `-webkit-` 前缀写法）。
///
/// 一期没有连字词典，所以 `Auto` 与 `Manual` 的**行为**相同：都只在已有的连字符处断，
/// 不会把 `sais` 拆成 `sa-is`。差别留在「作者的意图有没有被记下来」——词典接上时不用
/// 回头改书的样式表。这条边界记在 CSS 的忽略清单里。
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
    Page,
    Left,
    Right,
    Always,
    /// `page-break-*: avoid` / `break-*: avoid`：尽量别在这断。不是强制断，
    /// 所以 `is_forced()` 为假；分片器在「块能整块放下」时尊重它（见 fragment.rs）。
    Avoid,
}

impl BreakWhen {
    /// 是不是「强制断在这里」的意图。`Auto` 与 `Avoid` 都不是。
    pub fn is_forced(self) -> bool {
        matches!(self, Self::Page | Self::Left | Self::Right | Self::Always)
    }

    /// 是不是「尽量别在这断」的意图。分片器据此在放得下时避免切开。
    pub fn is_avoid(self) -> bool {
        matches!(self, Self::Avoid)
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

/// 一个 CSS 颜色：RGBA 四通道。
///
/// 第四格是 alpha（0 = 全透明，255 = 不透明）。出版书里 `rgba(80, 70, 86, .7)`
/// 这类半透明底色很常见（多看系的「人物志」框就是），丢掉 alpha 会把底图完全盖死。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color(pub u8, pub u8, pub u8, pub u8);

/// SVG 呈现属性 `fill` / `stroke` 的取值：`none` 或一格颜色。
///
/// `fill` 的 CSS 初值是黑色、`stroke` 是 `none`；这两个初值在
/// `ComputedStyle::default` 里分别写死，不靠 [`Default`] 的派生值 ——
/// 派生的 `None` 只是占位，用来给「还没算到这个属性」的临时样式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SvgPaint {
    #[default]
    None,
    Color(Color),
}

/// SVG `stroke-linecap`：线端的形状。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SvgLineCap {
    #[default]
    Butt,
    Round,
    Square,
}

/// SVG `stroke-linejoin`：折角的形状。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SvgLineJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}
/// **落地范围**：绝对定位的盒脱流、按 `left` / `right` / `top` / `bottom`（`%` 折自
/// **这一章的盒**）摆，`margin` 加在偏移上（封面那套 `left:50%; margin-left:-9.5em` 的居中
/// 写法就是靠它）。**只认章级包含块**：CSS 的「最近的定位祖先」没做，所以嵌在别的定位盒
/// （包括 `relative` 盒）里的绝对定位元素仍然按这一章摆。
///
/// [`Self::Relative`] 那一档见 [`Positioning::Relative`]：流内整体位移，不脱流。
/// `sticky` 判不认识 —— 它要的是滚动状态，不是排版这一层的事。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Positioning {
    #[default]
    Static,
    Absolute,
    Fixed,
    /// `position: relative`：**留在常规流里**，只把自己已经排好的那一坨整体挪一下。
    ///
    /// 与 [`Self::Absolute`] 的分界就在游标：排完这一块的兄弟仍按它**没挪**时的位置接，
    /// 所以挪出去的那一截会压在邻居身上（浏览器同样如此，那是作者要的效果）。
    /// 偏移的取用规则、百分比基准与不跟着挪的那几路，全写在 `layout::collect` 的
    /// `shift_relative`。
    Relative,
}

/// `vertical-align` 的**表格格**那一档：格里的内容在行高里贴哪一边。
///
/// CSS 的 `vertical-align` 有两副面孔：对行内盒是「相对行盒抬多少」（我们落成
/// [`ComputedStyle::baseline_shift`]），对表格格是「贴在行高的哪一边」（就是这一格）。
/// 这一格**不继承**（CSS 规定），所以整行的 `vertical-align: middle` 不会漏到格子上 ——
/// 浏览器同样如此（Calibre 转换的书要另写 `vertical-align: inherit` 才生效）。
///
/// 落地程度：`top`（初值，也就是我们一直以来的行为）、`middle`、`bottom` 三档；
/// 行内盒上的 `top` / `middle` / `bottom`（相对行盒）**还没做**，收下这个值但在行内不产生
/// 位移 —— 本机语料里这几种写法全落在 `display: table-row` 上，浏览器也不动。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CellVAlign {
    #[default]
    Top,
    Middle,
    Bottom,
}

/// `vertical-align` 落在**行内盒**上的那两档「对齐父盒字体上下沿」的关键字。
///
/// `super` / `sub` / 长度 / 百分比在层叠期就能折成 px（[`ComputedStyle::baseline_shift`]），
/// 所以不在这里；`top` / `bottom` / `middle` 要对齐**整条行盒**，得等一行排完才知道，
/// 仍在 `KNOWN_UNIMPLEMENTED` 那一档。这里只收真书里出现的那两条：
/// `text-top`（《哈利·波特》`sup img` 482 个元素）与 `text-bottom`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum InlineVAlign {
    /// 贴基线，也就是初值。
    #[default]
    Baseline,
    /// 盒顶对齐父盒的字体顶（父盒的 ascent）。
    TextTop,
    /// 盒底对齐父盒的字体底（父盒的 descent，基线以下）。
    TextBottom,
}

/// `ruby-align`：注音（`<rt>`）在它的基字上方横向摆在哪一段。
///
/// 容器的前进量取「基字宽 / 注音宽」较宽那一条，注音就在这一段里贴起始边、居中或贴末边。
/// 三档的落点只是同一个数的三种取法（[`RubyAlign::flow_offset`]），所以三档都真兑现。
///
/// `auto` 是 CSS 的初值，规范让它「按基字与注音的长短自己定」，浏览器对单字与等长一律居中，
/// 所以这里把它折成与 `center` 同一个画面 —— 而不是另猜一套分配规则。《诡秘之主》552 处
/// 显式写的就是 `center`。
///
/// **不收** `justify` / `distribute` / `space-between` / `space-around`：那几档要改的是注音
/// 自己逐字之间的字距（把注音摊到基字宽上），判不认识比猜一种摊法诚实。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RubyAlign {
    #[default]
    Auto,
    Start,
    Center,
    End,
}

impl RubyAlign {
    /// 注音起点相对**容器起点**的横向偏移占「容器宽 − 注音宽」的几成。
    ///
    /// `Auto` 与 `Center` 同一数（见这一格的文档）。排版层只问这一个数，档位与像素的
    /// 换算留在唯一一处，RTL 要反向时也只动调用点的那一个符号。
    pub fn flow_offset(self, container: f32, annotation: f32) -> f32 {
        let slack = (container - annotation).max(0.0);
        match self {
            Self::Start => 0.0,
            Self::Auto | Self::Center => slack * 0.5,
            Self::End => slack,
        }
    }
}

/// `ruby-position`：注音排在基字的哪一侧。
///
/// `over`（初值，中文/日文书里的常态）排在字上方，`under` 排在字下方 —— 两侧的行盒都照
/// 同一式子长高，所以 `under` 的代价只是符号相反。
///
/// `inter-character`（竖排里排在字侧的上一格，用于假名注音的另一种摆法）不收：它要的是
/// 「沿字轴错开半格」的另一套几何，本机语料 0 处。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RubyPosition {
    #[default]
    Over,
    Under,
}

/// `word-break`：拉丁词内部能不能断。
///
/// 只两档：`normal`（初值，词只能在空格/连字符处断）与 `break-all`（任意字符之间都能断）。
/// `keep-all`（不许在 CJK 之间断）没收 —— 中文书的正文靠逐字断行，那条是给韩文/混排用的，
/// 本机语料 0 处，判不认识比猜错好。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WordBreak {
    #[default]
    Normal,
    BreakAll,
}

/// `text-shadow` 的一层影子：位移 + 模糊半径 + 颜色。
///
/// 位移与模糊半径在这一层就已经折成 px（`em` 按**元素自己的**字号折，与其余长度属性同一条
/// 口径）。CSS 里 `text-shadow` **可继承**，并且可以写多值（逗号分隔的图层），所以
/// `ComputedStyle` 里存的是一整条列表。
///
/// 落地程度要说清：绘制层逐字形再画一遍偏移副本，**模糊半径不是高斯**，是按半径摊出来的
/// 几份偏移副本（见 `ui::reader::view::paint::shadow_copies`）。真书里这个值普遍是 1px，
/// 两者在屏上分不出来；写大了会被副本数封顶，不会把一帧画糊。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextShadow {
    pub dx: f32,
    pub dy: f32,
    /// 模糊半径，px。0 = 硬边影子。
    pub blur: f32,
    /// `None` = 没写颜色，绘制时取**本 run 的文字色**（CSS 的 `currentColor`）。
    pub color: Option<Color>,
}

impl std::hash::Hash for TextShadow {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        self.dx.to_bits().hash(h);
        self.dy.to_bits().hash(h);
        self.blur.to_bits().hash(h);
        self.color.hash(h);
    }
}
