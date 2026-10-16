//! 引擎向宿主索取的两项能力：文本度量与图片尺寸。后端因此能脱离窗口跑，测试能用手搓的假实现。
//!
//! 从 `layout` 主模块切出来只为了一个文件能一次读完；跨文件的条目用
//! `pub(super)`，即**只对 `layout` 内部开放**，不属于对外公开面。

use super::font::FontKey;
use crate::content::format::css::computed::{ComputedStyle, Length};

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

    /// 一段文本的基线上下 extent `(ascent, descent)`。
    ///
    /// **`text` 是这个签名存在的全部理由**：一份样式声明的字体**不等于**这段字实际用的字体。
    /// 中文书里 `<p>` 声明了拉丁正文字族，汉字与假名走的是回退链里另一张脸，而那张脸的
    /// ascent/descent 才是行盒该用的数——**只有按这段文字去量才拿得到**。曾经这里不收 `text`、
    /// 实现方拿一个固定代表字符（拉丁的 `x`）量一次按 `(字体, 字号)` 缓存，于是中英混排的行
    /// 盒高、基线、`line-height` 折出来的半 leading 全都按拉丁字体的度量算，汉字行被压低、
    /// 上下沿切字（2026-10-13 修，`METRICS_REV` 同批 +1）。
    ///
    /// 代价要说清：正文**不许**因此退回逐行 shaping。行盒的宽已经出自宽度表（与断行同源），
    /// 这一格只在**新的（字体, 字号, 这段里出现的文字类别）组合**上量一次并缓存——一章里
    /// 那种组合是个位数到十位数，不是行数。实现方可以按 `(字体, 字号)` 退化到只量一个代表字符，
    /// 但那是**明知会错**的取巧，不是等价实现。
    ///
    /// 默认实现给「0.8 / 0.2 em」这一档常见比例（与空行 strut 同一档），忽略 `text`。
    fn font_extents(&self, _text: &str, _font: &FontKey, font_size: f32) -> (f32, f32) {
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

    /// run 内每个字符**在阅读方向上的起点**的 x（与真实 shaping 同源），返回长度 =
    /// 字符数 + 1，末项是这一段在阅读方向上的**终点**。
    ///
    /// RTL 那一段是**递减**的（起点在右、终点在左），LTR 递增 —— 所以取一个字符的矩形
    /// 要把相邻两项 min/max 都算，不许假设表是升的（`content::hit` 的选中矩形就按这条写）。
    /// 选中矩形的左右缘用它画，才能贴住绘制出来的字。
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
/// 宽高是**一对**：只写 `height` 时宽要按纵横比反算，只写 `width` 时高按比例算；
/// 两边都写死才不保持比例。从前这里先按 intrinsic 定宽、再单独拿 `height` 覆盖高，
/// 于是 `sup img { height:1em; width:auto }` 得到一张「固有宽 × 1em」的扁图 ——
/// 脚注「注」图因此既过大又和行盒对不上。
pub(super) fn image_size(style: &ComputedStyle, iw: f32, ih: f32, container_w: f32) -> (f32, f32) {
    let basis = container_w.max(1.0);
    let has_intrinsic = iw > 0.0 && ih > 0.0;
    let width = style.width.map(|l| l.resolve(basis)).filter(|w| *w >= 0.0);
    // 百分比高度没有包含块高度作基准，按 auto 处理（见 known-limitations）。
    let height = match style.height {
        Some(Length::Px(v)) if v >= 0.0 => Some(v),
        _ => None,
    };

    let (mut w, mut h) = match (width, height, has_intrinsic) {
        (Some(w), Some(h), _) => (w, h),
        (Some(w), None, true) if w > 0.0 => (w, ih * (w / iw)),
        (Some(w), None, _) => (w, w),
        (None, Some(h), true) if h > 0.0 => (iw * (h / ih), h),
        (None, Some(h), _) => (h, h),
        (None, None, true) => (iw, ih),
        (None, None, false) => (basis, basis),
    };
    w = w.max(0.0);
    h = h.max(0.0);

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
