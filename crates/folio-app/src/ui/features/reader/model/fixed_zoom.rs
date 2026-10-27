//! 定版式的显示缩放：倍率、平移，以及「绕手指那一点放大」。
//!
//! 定版式的页是既定的位图，缩放只是**显示变换**：页栈（[`super::fixed_book::FixedStack`]）、
//! `scroll_y`、位置落库全都留在 1x 坐标里，一动都不动。这一层只回答两件事：
//!
//! - 一个 1x 的矩形摆到屏上该是哪儿、多大（[`FixedZoom::map`]）；
//! - 手指拖动与捏合之后，平移量该收到哪儿（[`clamp_axis`] 与 [`anchor_pan`]）。
//!
//! **轴是分开算的**。两档共用同一套式子，只是轴心（`pivot`）不同：
//!
//! - 整页翻：轴心是铺满那一刻那一页的左上角 —— 放大看起来是从这张纸里长出来的；
//! - 上下滚动：轴心是版心左上角 `(0, header_reserve)` —— 纵向由 `scroll_y` 管，
//!   这里只映射离版心顶有多远。
//!
//! 上限按**源图像素**封顶（[`max_scale`]）：再往上放大只是把同一批像素摊开，字不会更清楚，
//! 而读者会以为「放大了反而糊」。所以放不大时不硬放。

use gpui_kit::{Bounds, Pixels, point, px, size};

/// 不缩到比「铺满」更小。要看得更小是一屏摆两页的事（那一档定版式没有），不是缩放的事。
pub const MIN_SCALE: f32 = 1.0;

/// 最多放大到 4 倍。源图像素不够时按 [`max_scale`] 先封顶。
///
/// 4 这个数管的是「别把一张 600 像素宽的扫描页摊满整屏」那种不成样子的放大；
/// 它不承诺清晰度 —— 清晰度由源图与 PDF 的栅格档位（`RENDER_EDGE`）决定。
pub const MAX_SCALE: f32 = 4.0;

/// 定版式阅读页的显示变换。`Default` 就是「铺满、没有平移」。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::ui::features::reader) struct FixedZoom {
    scale: f32,
    pan: (f32, f32),
}

impl Default for FixedZoom {
    fn default() -> Self {
        Self {
            scale: MIN_SCALE,
            pan: (0.0, 0.0),
        }
    }
}

impl FixedZoom {
    pub(in crate::ui::features::reader) fn scale(&self) -> f32 {
        self.scale
    }

    pub(in crate::ui::features::reader) fn pan(&self) -> (f32, f32) {
        self.pan
    }

    /// 现在是不是放大了（比铺满大一截）。
    pub(in crate::ui::features::reader) fn is_zoomed(&self) -> bool {
        self.scale > MIN_SCALE + f32::EPSILON
    }

    /// 回到铺满，平移一并归零。
    pub(in crate::ui::features::reader) fn reset(&mut self) {
        *self = Self::default();
    }

    /// 捏合一步之后会变成多少（不落地）。事件那一路要先拿它算锚点，再决定平移。
    ///
    /// 乘法而不是加法：两趟 10% 与一趟 21% 是同一件事，读者手指走多远只由自己的指距决定。
    pub(in crate::ui::features::reader) fn next_scale(&self, delta: f32, max: f32) -> f32 {
        (self.scale * (1.0 + delta)).clamp(MIN_SCALE, max.max(MIN_SCALE))
    }

    /// 直接给倍率（也用于换档/换行之后的夹取）。回到 1x 时平移必须归零 ——
    /// 留着上一档的平移，读者看到的是「铺满但歪着」。
    pub(in crate::ui::features::reader) fn set_scale(&mut self, scale: f32, max: f32) {
        self.scale = scale.clamp(MIN_SCALE, max.max(MIN_SCALE));
        if !self.is_zoomed() {
            self.pan = (0.0, 0.0);
        }
    }

    pub(in crate::ui::features::reader) fn set_pan(&mut self, pan: (f32, f32)) {
        self.pan = pan;
    }

    /// 平移一段增量（屏上像素）。夹取由调用方按当前那一格的几何做（[`clamp_axis`]）：
    /// 两档「那一格」不同（整页翻是铺满的那一页，上下滚动是当前这一行），这一层不认识页。
    pub(in crate::ui::features::reader) fn pan_by(&mut self, delta: (f32, f32)) {
        self.pan = (self.pan.0 + delta.0, self.pan.1 + delta.1);
    }

    /// 把一个 1x 坐标里的矩形映射到屏上：`pivot + (p - pivot) * scale + pan`。
    pub(in crate::ui::features::reader) fn map(
        &self,
        rect: Bounds<Pixels>,
        pivot: (f32, f32),
    ) -> Bounds<Pixels> {
        let (x, y) = (f32::from(rect.origin.x), f32::from(rect.origin.y));
        let (w, h) = (f32::from(rect.size.width), f32::from(rect.size.height));
        Bounds::new(
            point(
                px(pivot.0 + (x - pivot.0) * self.scale + self.pan.0),
                px(pivot.1 + (y - pivot.1) * self.scale + self.pan.1),
            ),
            size(px(w * self.scale), px(h * self.scale)),
        )
    }
}

/// 放大上限：源图那一页有多宽（像素）除以它在铺满时画多宽。
///
/// 源图比铺满还窄时返回 1 —— 那时放大只是摊开同一批像素，不该假装能看清。
/// 再按 [`MAX_SCALE`] 封顶。
pub(in crate::ui::features::reader) fn max_scale(source_width: f32, drawn_width: f32) -> f32 {
    if drawn_width <= 0.0 || !source_width.is_finite() {
        return MIN_SCALE;
    }
    (source_width / drawn_width).clamp(MIN_SCALE, MAX_SCALE)
}

/// 一根轴上「绕 `screen` 这一点从 `from` 倍变到 `to` 倍」之后的平移量。
///
/// 要求解的是「手指底下那一块字不动」：屏幕上 `screen` 那一点对应的内容坐标是
/// `content = pivot + (screen - pivot - pan) / from`，改完倍率再把它放回同一个 `screen`。
pub(in crate::ui::features::reader) fn anchor_pan(
    screen: f32,
    pivot: f32,
    pan: f32,
    from: f32,
    to: f32,
) -> f32 {
    if from <= 0.0 || to <= 0.0 {
        return pan;
    }
    let content = pivot + (screen - pivot - pan) / from;
    screen - pivot - (content - pivot) * to
}

/// 一根轴上把平移夹住：放大后比视口**窄**就居中，**宽**就两头夹住。
///
/// `pivot` 是缩放轴心，`rect`/`len` 是这一格在 1x 里的起点与长、`scale` 是倍率，
/// `area` 是它可占的那一段（整页翻 = 版心，上下滚动 = 版心宽）。
///
/// 「窄就居中」不是随手加的：多图页与窄页铺满后两边本来就空着，放大一档之后还按左缘
/// 对齐，读者会看到整张纸贴在左边——而没有一根手指拖动能把那半边空补回来（夹取把它
/// 钉在 0）。
pub(in crate::ui::features::reader) fn clamp_axis(
    pan: f32,
    pivot: f32,
    rect: f32,
    len: f32,
    scale: f32,
    area: f32,
) -> f32 {
    let scaled = len * scale;
    // 平移为 0 时这一格的起点。
    let base = pivot + (rect - pivot) * scale;
    if scaled <= area {
        return (area - scaled) / 2.0 - base;
    }
    // 左边不许离开视口左缘、右边不许离开视口右缘。
    let hi = -base;
    let lo = area - scaled - base;
    pan.clamp(lo, hi)
}

/// 上下滚动那一档：捏合之后让手指下那一点不动，算新的 `scroll_y`（1x 内容坐标）。
///
/// 与 [`anchor_pan`] 同一条式子，只是纵向不存平移、存的是滚动：屏幕上那一段离版心顶
/// `focal_y - pivot` 的内容坐标是 `scroll + (focal_y - pivot) / from`。
pub(in crate::ui::features::reader) fn anchor_scroll(
    scroll: f32,
    focal_y: f32,
    pivot_y: f32,
    from: f32,
    to: f32,
) -> f32 {
    if from <= 0.0 || to <= 0.0 {
        return scroll;
    }
    let content = scroll + (focal_y - pivot_y) / from;
    content - (focal_y - pivot_y) / to
}

/// 定版式这一帧的缩放几何，**元素坐标**。绘制帧写、事件帧读。
///
/// 为什么要这一格：捏合与拖动发生在事件里，而「这一页铺满在哪、多大、能放到几倍」
/// 只有渲染那一帧算得出来（它要先量版心、读页表）。`page_area` 那格解决的是折页的同类
/// 问题，这一格只多几项与缩放有关的数。
#[derive(Clone, Copy, Debug)]
pub(in crate::ui::features::reader) struct FixedFrame {
    /// 版心左上角。纵向的夹取与滚动锚都按它归零。
    pub origin: (f32, f32),
    /// 缩放轴心：整页翻 = 铺满那一页的左上角；上下滚动 = 版心左上角。
    pub pivot: (f32, f32),
    /// 铺满时那一页（整页翻）/ 当前那一行（上下滚动，1x）的宽高。
    pub page: (f32, f32),
    /// 可占的那一块：版心宽 × 版心高。
    pub area: (f32, f32),
    /// 上下滚动：当前那一行在 1x 里的左缘与宽；整页翻不看。
    pub row: (f32, f32),
    /// 上下滚动：页栈总高（1x）；整页翻不看。
    pub total: f32,
    /// 放大上限（见 [`max_scale`]）。还没量到几何时是 1，也就是放不大。
    pub max: f32,
    pub continuous: bool,
}

impl Default for FixedFrame {
    fn default() -> Self {
        Self {
            origin: (0.0, 0.0),
            pivot: (0.0, 0.0),
            page: (0.0, 0.0),
            area: (1.0, 1.0),
            row: (0.0, 0.0),
            total: 0.0,
            max: MIN_SCALE,
            continuous: false,
        }
    }
}

impl FixedFrame {
    /// 把平移夹进这一帧允许的范围。`clamp_axis` 用的是「相对版心」的坐标，这里换算。
    pub(in crate::ui::features::reader) fn clamp_pan(
        &self,
        pan: (f32, f32),
        scale: f32,
    ) -> (f32, f32) {
        if self.continuous {
            // 纵向归 `scroll_y`，平移只留横向。
            let x = clamp_axis(
                pan.0,
                0.0,
                self.row.0 - self.origin.0,
                self.row.1,
                scale,
                self.area.0,
            );
            (x, 0.0)
        } else {
            let (px, py) = (self.pivot.0 - self.origin.0, self.pivot.1 - self.origin.1);
            (
                clamp_axis(pan.0, px, px, self.page.0, scale, self.area.0),
                clamp_axis(pan.1, py, py, self.page.1, scale, self.area.1),
            )
        }
    }

    /// 上下滚动这一档在新的倍率下能滚到的最远处：视口在 1x 坐标里只占 `area / scale`。
    pub(in crate::ui::features::reader) fn max_scroll(&self, scale: f32) -> f32 {
        (self.total - self.area.1 / scale.max(MIN_SCALE)).max(0.0)
    }
}

#[cfg(test)]
mod tests {
    //! 判据都是「手指底下那一块字不动」与「两头顶住」的代数形式，这里只钉端点与临界。
    //! 不 `use super::*`：`gpui_kit` 根有个叫 `test` 的属性宏，glob 会把它带进来。

    use super::{
        FixedZoom, MAX_SCALE, MIN_SCALE, anchor_pan, anchor_scroll, clamp_axis, max_scale,
    };

    /// `delta` 是乘法：两趟 10% 等于一趟 21%。
    #[test]
    fn zoom_steps_are_multiplicative() {
        let mut zoom = FixedZoom::default();
        let once = zoom.next_scale(0.1, MAX_SCALE);
        zoom.set_scale(once, MAX_SCALE);
        zoom.set_scale(zoom.next_scale(0.1, MAX_SCALE), MAX_SCALE);
        assert!((zoom.scale() - 1.21).abs() < 1e-4, "实测 {}", zoom.scale());
    }

    /// 两头都夹住：往里缩不许小于铺满，往外放不许越过上限。
    #[test]
    fn zoom_clamps_at_both_ends() {
        let mut zoom = FixedZoom::default();
        zoom.set_scale(zoom.next_scale(-0.5, MAX_SCALE), MAX_SCALE);
        assert_eq!(zoom.scale(), MIN_SCALE);
        zoom.set_scale(zoom.next_scale(100.0, MAX_SCALE), MAX_SCALE);
        assert_eq!(zoom.scale(), MAX_SCALE);
    }

    /// 上限按源图像素算，再按 4 倍封顶；源图比铺满窄时不给放大。
    #[test]
    fn the_ceiling_follows_the_source_pixels() {
        assert_eq!(max_scale(2000.0, 1000.0), 2.0);
        assert_eq!(max_scale(100_000.0, 1000.0), MAX_SCALE);
        assert_eq!(max_scale(600.0, 1000.0), MIN_SCALE);
        assert_eq!(max_scale(0.0, 0.0), MIN_SCALE, "还没量到尺寸时不放大");
    }

    /// 绕手指那一点缩放：屏幕上的锚点前后对应同一个内容点（解出来的平移回代验算）。
    #[test]
    fn the_focal_point_does_not_move() {
        let pivot = 0.0;
        let (from, to) = (1.0, 2.0);
        let screen = 300.0;
        let pan = anchor_pan(screen, pivot, 0.0, from, to);
        // 反算：手指底下那个内容点在两档里的屏幕位置一致。
        let content_before = pivot + (screen - pivot - 0.0) / from;
        let content_after = pivot + (screen - pivot - pan) / to;
        assert!((content_before - content_after).abs() < 1e-3);
        // 1x 时锚点正中放大两倍：平移 = -300（纸从手指处向两边长开）。
        assert!((pan + 300.0).abs() < 1e-3, "实测 {pan}");
    }

    /// 比视口窄就居中，宽就两头夹住 —— 两头顶不到外面去。
    #[test]
    fn the_pan_stays_inside_the_page() {
        // 400 宽的版心，铺满时那一页 200 宽、居中（起点 100）；1x 时平移是 0。
        assert_eq!(clamp_axis(0.0, 100.0, 100.0, 200.0, 1.0, 400.0), 0.0);

        // 放大两倍：400 宽、正好铺满。轴心是纸自己的左上角，所以它只往右下长，
        // 想让整张回到版心里得把它左移 100。
        assert_eq!(clamp_axis(0.0, 100.0, 100.0, 200.0, 2.0, 400.0), -100.0);

        // 放大四倍：800 宽。左半边是平移 -100（左缘贴 0），右半边是 -500（右缘贴 400），
        // 两头都不许再往外。
        assert_eq!(clamp_axis(999.0, 100.0, 100.0, 200.0, 4.0, 400.0), -100.0);
        assert_eq!(clamp_axis(-999.0, 100.0, 100.0, 200.0, 4.0, 400.0), -500.0);
    }

    /// 映射：绕轴心等比，尺寸一起放大。
    #[test]
    fn mapping_scales_about_the_pivot() {
        let mut zoom = FixedZoom::default();
        zoom.set_scale(2.0, MAX_SCALE);
        zoom.set_pan((10.0, -4.0));
        let at = gpui_kit::Bounds::new(
            gpui_kit::point(gpui_kit::px(120.0), gpui_kit::px(40.0)),
            gpui_kit::size(gpui_kit::px(100.0), gpui_kit::px(200.0)),
        );
        let mapped = zoom.map(at, (100.0, 0.0));
        assert_eq!(f32::from(mapped.origin.x), 100.0 + 20.0 * 2.0 + 10.0);
        assert_eq!(f32::from(mapped.origin.y), 0.0 + 40.0 * 2.0 - 4.0);
        assert_eq!(f32::from(mapped.size.width), 200.0);
        assert_eq!(f32::from(mapped.size.height), 400.0);
    }

    /// 缩回 1x 时平移必须归零：留着它，读者看到的是一张铺满但歪着的纸。
    #[test]
    fn going_back_to_fit_drops_the_pan() {
        let mut zoom = FixedZoom::default();
        zoom.set_scale(3.0, MAX_SCALE);
        zoom.set_pan((40.0, -90.0));
        zoom.set_scale(1.0, MAX_SCALE);
        assert_eq!(zoom.pan(), (0.0, 0.0));
        assert!(!zoom.is_zoomed());
    }

    /// 上下滚动那一档：捏合前后，手指下那一段内容停在原处。
    #[test]
    fn the_scroll_anchor_holds_under_the_fingers() {
        let next = anchor_scroll(120.0, 400.0, 60.0, 1.0, 2.0);
        // 内容点 = 120 + (400-60)/1 = 460；2 倍时 scroll = 460 - 340/2 = 290。
        assert_eq!(next, 290.0);
        // 反向缩回去正好回到原处。
        assert!((anchor_scroll(next, 400.0, 60.0, 2.0, 1.0) - 120.0).abs() < 1e-3);
    }
}
