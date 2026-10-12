//! 引擎向宿主索取的两项能力：文本度量与图片尺寸。后端因此能脱离窗口跑，测试能用手搓的假实现。
//!
//! 从 `layout` 主模块切出来只为了一个文件能一次读完；跨文件的条目用
//! `pub(super)`，即**只对 `layout` 内部开放**，不属于对外公开面。

use super::font::FontKey;
use crate::css::computed::{ComputedStyle, Length};

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
pub(super) fn image_size(style: &ComputedStyle, iw: f32, ih: f32, container_w: f32) -> (f32, f32) {
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
