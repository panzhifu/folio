//! 一行文本片段物化后的绘制输入（[`RowUnit`]）。
//!
//! 单开一间是因为 `chapter_layout.rs` 已经贴着行数上限；这一格只装数据与它的取舍，
//! 不读排版过程。槽位语义、主题色与 `FontKey` 的理由都跟着类型走。

use std::sync::Arc;

use folio_core::content::format::css::computed::{
    Color, DecorationStyle, EmphasisPosition, EmphasisStyle, TextShadow,
};
use folio_core::layout::{FontKey, Justify};
use gpui_kit::SharedString;

/// 一条文本片段物化一次之后的全部绘制输入。
///
/// 故意**不存**的两样，存了就是把失效问题从一处拆成两处：
///
/// - 解析好的 `gpui_kit::Font`：`FontCache` 已经有「换书 `reset`、改偏好 `clear`」那条
///   纪律（`reader/measure.rs`），表里再存一份就是第二个真相。这里存 `FontKey`，
///   每帧现解析——那是一次哈希查表加一次 `Arc` 克隆，不是这笔开销的主角。
/// - 主题色：`LayoutKey` 里没有主题，把 `run_color` 的结果烘进表就是「换了夜间模式
///   但线还是旧色」那种看不出来的错。这里存 CSS 那一格 `Option<Color>`，每帧现落回主题。
#[derive(Clone, Debug)]
pub struct RowUnit {
    /// 在**本行片段数组**里的槽位。连同行号一起构成 shaping 表的键
    /// （`chapter_layout::Document::shaped`）。槽位连图片一起数，所以文本片段的槽位
    /// 不会随图片增减漂移。
    pub slot: u32,
    /// 文本句柄。这一格就是这次缓存的主体：从前每帧从归一化串里切一个新的出来。
    pub text: SharedString,
    pub font: Arc<FontKey>,
    pub font_size: f32,
    /// 相对**行首**的 x（章内量）。
    pub x: f32,
    /// 基线偏移，px，正数往上。来自 `vertical-align`，绘制层把它从行的基线里减掉。
    pub baseline_shift: f32,
    /// 这一片的 inline box 高。绘制层用它定装饰线的纵向落点，而 shaping 里没有这个数。
    pub box_height: f32,
    pub underline: bool,
    pub line_through: bool,
    /// `text-decoration-style` / `text-decoration-color`：形状与颜色。
    pub decoration_style: DecorationStyle,
    pub decoration_color: Option<Color>,
    /// `text-shadow` 与 `letter-spacing` 一样，是**逐 run** 的：CSS 里它可继承，
    /// 所以通常在块上写一次，但一条子元素的声明要能盖掉它。绘制层拿它画偏移副本。
    pub text_shadows: Arc<[TextShadow]>,
    /// CSS 声明的文字色。`None` = 书里没写，每帧落到主题前景。
    pub color: Option<Color>,
    /// 这一段在不在一条 `<a href>` 里（`ComputedStyle::link`）。`color` 为 `None` 时绘制层
    /// 拿它落到主题的链接色 —— 出版工具的目录页普遍不写颜色，而那栏看着必须是能点的。
    pub link: bool,
    /// 字距（px），来自 `letter-spacing`。绘制时按字形所在字符序号右推 `ls × k`。
    pub letter_spacing: f32,
    /// 两端对齐：这一段起点已累计的右推量（px），以及这一行的摊法。绘制时字形再按
    /// **运行内**的隙序号右推 `justify.extra × 局部隙序号`，与 `hit`/选区同一句规则。
    /// `None` = 这一行不两端对齐。
    pub justify: Option<Justify>,
    pub justify_shift: f32,
    /// `text-emphasis` 着重号：形状、颜色、位置（2026-11-23）。绘制层据它在每个字形旁
    /// 画小圆点。形状 `None` 时整片不画。
    pub emphasis: EmphasisStyle,
    pub emphasis_color: Option<Color>,
    pub emphasis_position: EmphasisPosition,
}
