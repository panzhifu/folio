//! 值类型：背景摆法、盒模型取向下的一小组公开枚举与其 `Default`。
//!
//! 从 `css::computed` 切出来只因为 computed.rs 贴着行数上限；对外路径经 `pub use` 保持不变。

use super::Length;

/// 背景图的一层：`background-image` 的一个 `url()` 加上与它**配对**的摆法三件。
///
/// 存的是 CSS 侧的原始数，一块背景图真正落在哪、多大要等排版期拿到盒尺寸才算得出
/// （`cover` / `contain` / 百分比都要读那一个盒），所以这里不带任何 px 结果。
///
/// **图不在这里取**：`href` 是容器内的绝对路径（样式表拼进层叠前已由
/// `content::format::absolutize_css_urls` 把 `../Images/a.png` 折算成 `OEBPS/Images/a.png`
/// 并去掉引号），字节与固有尺寸每帧现问图片索引 —— 与行内图片 `RowImage::href`
/// 同一个口径，理由也同一条：那两样随图片索引变，烘进这张表就是第二个真相。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BackgroundLayer {
    /// `None` = 这一层是 `background-image: none`（作者明确不要图），不是「没写过」。
    pub image: Option<String>,
    pub size: BackgroundSize,
    pub position: BackgroundPosition,
    pub repeat: BackgroundRepeat,
    /// `background-attachment`。绘制一律按 `scroll`（图跟着纸走），这一格只有
    /// `fixed` 在**章节根**上被排版层读：浏览器里那是铺满视口画布的整页底图，
    /// 所以这一章至少占一屏高（见 `layout::layout_inner`）。挂在普通块上的 `fixed`
    /// 仍与 `scroll` 同效 —— 流式排版里没有「钉在视口」的对应物。
    pub attachment: BackgroundAttachment,
}

/// `background-attachment`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum BackgroundAttachment {
    #[default]
    Scroll,
    /// 只看根章节这一格：铺满视口的整页底图，章节至少一屏高。
    Fixed,
    /// 跟着块的滚动区走；流式块没有自己的滚动区，按 `scroll` 摆。
    Local,
}

/// `background-size`。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum BackgroundSize {
    /// 初值：图按固有尺寸摆，两轴都不缩。
    #[default]
    Auto,
    /// 缩到**刚好装下**整张图（留白可见）。
    Contain,
    /// 缩到**盖满**这个盒（多出来的那一部分裁掉）。
    Cover,
    /// 一到两个显式长度／百分比；`None` 那一轴就是 `auto`。
    /// 百分比按**这个盒**的对应轴折，不是按包含块 —— 与 `width` 那一族不同源是有意的，
    /// CSS 规定 `background-size` 的基准是背景定位区（我们一律取边框盒）。
    Explicit {
        width: Option<Length>,
        height: Option<Length>,
    },
}

/// 一轴的摆法。`Fraction` 单独一档是因为 CSS 的百分比在这里的基准**不是盒宽**：
/// `background-position: 25%` 说的是「图的 25% 处对齐盒的 25% 处」，
/// 于是余量（盒宽 − 图宽）乘那个分数，`0%` 贴起始边、`100%` 贴结束边。
/// 把它折成「盒宽的分数」会在图比盒大时把图推出可视区。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BackgroundAxis {
    /// 关键字档（`left`/`top` = Start，`center` = Center，`right`/`bottom` = End），
    /// 后面那条偏移是 `right 10px` 那种写法：从这一边再往里推。
    Edge(BackgroundEdge, Option<Length>),
    /// 一条长度（px / em / rem），从起始边量。
    Length(Length),
    /// CSS 的百分比：按「盒减图」那段余量的分数，`50%` 存成 `0.5`。
    Fraction(f32),
}

/// `background-position` 落在哪一边。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundEdge {
    Start,
    Center,
    End,
}

/// `background-position` 的两轴。单值写法（`center`）在解析期就把另一轴补成 `Center`，
/// 所以这里不带 `Option`。初值见下面那条手写的 `Default`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BackgroundPosition {
    pub x: BackgroundAxis,
    pub y: BackgroundAxis,
}

impl Default for BackgroundPosition {
    /// CSS 初值是 `0% 0%`（两条规范版本都写作 `left top`），不是 `center`。
    fn default() -> Self {
        Self {
            x: BackgroundAxis::Edge(BackgroundEdge::Start, None),
            y: BackgroundAxis::Edge(BackgroundEdge::Start, None),
        }
    }
}

/// `background-repeat` 的两轴。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BackgroundRepeat {
    pub x: BackgroundTile,
    pub y: BackgroundTile,
}

/// 一个轴上的铺法。CSS 初值两轴都是 `repeat`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackgroundTile {
    #[default]
    Repeat,
    /// 铺满那一轴，隙缝均摊（不裁剪图）。
    Space,
    /// 铺满那一轴，整张图跟着缩放（会改纵横比）。
    Round,
    NoRepeat,
}

/// 作者写的那个尺寸**含不含**边框与内边距，即 CSS 的 `box-sizing`。
///
/// 只有两档：`content-box` 是 CSS 初值也是这一族从前唯一的行为；`border-box` 是出版书与
/// 转换器实际写的那一档（插图框、色带标题、`padding` 不为 0 的定宽分隔条）。
/// `padding-box`（css3-ui，Firefox 收）有意**不认**，判「不认识」而不是当成 `border-box`：
/// 那一档要把边框留着只扣内边距，而扣错的表现是「盒子宽了边框那几像素」，比一条明确的
/// 「不认识」难查。`min-*` 与 `max-*` 跟着同一格走 —— 分开判就成了两份「谁含谁不含」的定义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BoxSizing {
    /// 初值：作者写的数是内容盒，边框与内边距在**外面**加。
    #[default]
    ContentBox,
    /// 作者写的数已经含边框与内边距。
    BorderBox,
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
