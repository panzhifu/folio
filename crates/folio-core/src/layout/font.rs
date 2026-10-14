//! 字体标识与 run 样式：`FontKey`、`RunStyle`、按值去重的 `StyleArena`。
//!
//! 从 `layout` 主模块切出来只为了一个文件能一次读完；跨文件的条目用
//! `pub(super)`，即**只对 `layout` 内部开放**，不属于对外公开面。

use std::sync::Arc;

use super::ports::Measure;
use crate::content::format::css::computed::{Color, ComputedStyle};

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
    /// 每个字符之后额外加的宽度（px），来自 CSS 的 `letter-spacing`。形状与 `word_spacing`
    /// 一样（都叠在两个字符之间），差别只在**叠在每个字符**还是只叠在空格上。
    /// 宽度表（`wrap::Collector::push_widths`）与这里的 `advance` 必须同源。
    pub letter_spacing: f32,
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
        self.letter_spacing.to_bits().hash(state);
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
            // 字距**不夹下限**：`letter-spacing` 允许负值（收紧），绘制与折行对负字距同样成立。
            letter_spacing: style.letter_spacing,
            baseline_shift: style.baseline_shift,
            underline: style.underline,
            line_through: style.line_through,
        }
    }

    /// 该 run 的 inline box 高度。
    pub fn box_height(&self) -> f32 {
        (self.font_size * self.line_height).max(1.0)
    }

    /// 一个字符的前进量。`letter-spacing` 叠在**每个**字符之后，`word-spacing` 叠在空格上
    /// —— CSS 里 `word-spacing` 加在 word separator 之后，而我们的折行就是按字符累加的，
    /// 所以两处（折行与重算已用宽）都走这一个函数，不会出现「折行算得下、画出来超出」的错位。
    /// 宽度表（`wrap::Collector::push_widths`）里那份累加必须与这里逐字一致。
    pub fn advance(&self, measure: &dyn Measure, ch: char) -> f32 {
        let base = measure.char_width(ch, &self.font, self.font_size);
        let mut w = base + self.letter_spacing;
        if ch == ' ' {
            w += self.word_spacing;
        }
        w
    }
}
