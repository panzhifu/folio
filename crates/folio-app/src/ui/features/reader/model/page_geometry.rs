//! 阅读页的屏幕几何：一张纸、两条带、正文可排区。
//!
//! 这是**屏幕坐标**的唯一来源。引擎的 `Page`/`PageList` 只认这里的 `content` 矩形；
//! 页眉/页脚是不是占位由 [`BandPolicy`] 决定，但纸、安全区、两条带、正文这四个矩形
//! 都从这一处算出来。
//!
//! 坐标：全部是**窗口坐标**，原点是窗口左上。引擎的 `origin_y` 是章内坐标，绘制时
//! 只做一次换算：
//!
//! ```text
//! window = content.min + (slice.origin_y − page.origin_y) + column.x
//! ```
//!
//! 不在这里算行盒、断页规则、字形；那些归 `folio-core`。这里只回答“这一页在屏幕上
//! 占哪一块”。

use folio_core::layout::fragment::{column_offset, column_width};

/// 屏幕上的一个矩形（窗口坐标，逻辑像素）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PageRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl PageRect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self {
            x,
            y,
            w: w.max(0.0),
            h: h.max(0.0),
        }
    }

    #[allow(dead_code)] // 命中与裁切接线前只有测试在用。
    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    #[allow(dead_code)] // 命中与裁切接线前只有测试在用。
    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }

    #[allow(dead_code)] // 命中接线前只有测试在用。
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    /// 这一块按 `columns` / `gap` 切出来的第 `index` 栏。
    ///
    /// 除法与位移都走 `folio-core` 的同一对函数，界面不再自己写一遍
    /// `(total - gap*(n-1))/n`。
    pub fn column(&self, index: usize, columns: usize, gap: f32) -> Self {
        let columns = columns.max(1);
        let width = column_width(self.w, columns, gap);
        let x = self.x + column_offset(width, gap, index.min(columns - 1));
        Self {
            x,
            y: self.y,
            w: width,
            h: self.h,
        }
    }
}

/// 系统栏 / 桌面侧栏占掉的边。
///
/// 眼下只用到 `top` / `bottom`：手机的状态栏与手势条。`left` / `right` 是给横屏挖孔
/// 与桌面侧栏留的口，默认 0。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PageInsets {
    pub top: f32,
    pub bottom: f32,
    pub left: f32,
    pub right: f32,
}

/// 两条读数带的高（含各自的页边距）。`ReadingPage` 算好递进来，`PageBox` 只读。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ChromeBands {
    pub header: f32,
    pub footer: f32,
}

/// 两条带和正文的关系。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BandPolicy {
    /// 可重排：两条带占位，`content` 在它们之间。正文永远进不去。
    Reserved,
    /// 定版式：两条带浮在页图之上，`content` 仍是整张纸。页图可以铺到系统栏下面。
    Overlay,
}

/// 交给引擎的排版 / 分片参数。
///
/// 只有可重排有这一份；定版式走页表，不排版也不分片。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageMetrics {
    /// 一栏的宽（已经除好栏数）。折行发生在它上面。
    pub column_width: f32,
    /// 一页可用的高。`0` 是连续滚动的“不分片”哨兵，由调用方自己换成 0。
    pub content_height: f32,
    pub columns: usize,
    pub gap: f32,
}

/// 一页的屏幕几何。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageBox {
    /// 整张纸：窗口矩形。
    pub paper: PageRect,
    /// 纸去掉系统栏 / 侧栏。
    pub safe: PageRect,
    /// 正文（可重排）或页图（定版式）占的那一块。
    pub content: PageRect,
    /// 页眉带。`Reserved` 时它占位；`Overlay` 时它只是浮层的锚区。
    pub header: PageRect,
    /// 页脚带。
    pub footer: PageRect,
    /// 页眉底色要铺到纸顶的那一块（`Overlay` 时给渐变用）。
    pub header_paint: PageRect,
    /// 页脚底色要铺到纸底的那一块。
    pub footer_paint: PageRect,
    pub columns: usize,
    pub gap: f32,
    pub bands: BandPolicy,
}

impl Default for PageBox {
    fn default() -> Self {
        Self::new(
            PageRect::default(),
            PageInsets::default(),
            0.0,
            0.0,
            0.0,
            0.0,
            1,
            0.0,
            BandPolicy::Reserved,
        )
    }
}

impl PageBox {
    /// `header_height` / `footer_height` 是**含页边距**的整条带高，与
    /// `ReaderSession::bands.header` / `bands.footer` 同一个口径。
    ///
    /// `margin_x` / `max_measure` 只影响可重排的正文宽；定版式的 `content` 是整张纸，
    /// 这两个数不参与。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        paper: PageRect,
        insets: PageInsets,
        header_height: f32,
        footer_height: f32,
        margin_x: f32,
        max_measure: f32,
        columns: usize,
        gap: f32,
        bands: BandPolicy,
    ) -> Self {
        let left = insets.left.max(0.0);
        let right = insets.right.max(0.0);
        let top = insets.top.max(0.0);
        let bottom = insets.bottom.max(0.0);
        let safe = PageRect::new(
            paper.x + left,
            paper.y + top,
            (paper.w - left - right).max(0.0),
            (paper.h - top - bottom).max(0.0),
        );

        let header_height = header_height.max(0.0).min(safe.h);
        let footer_height = footer_height.max(0.0).min(safe.h);
        let header = PageRect::new(safe.x, safe.y, safe.w, header_height);
        let footer = PageRect::new(
            safe.x,
            safe.y + safe.h - footer_height,
            safe.w,
            footer_height,
        );
        let header_paint = PageRect::new(
            paper.x,
            paper.y,
            paper.w,
            (header.bottom() - paper.y).max(0.0),
        );
        let footer_paint = PageRect::new(
            paper.x,
            footer.y,
            paper.w,
            (paper.bottom() - footer.y).max(0.0),
        );

        let columns = columns.max(1);
        let gap = if columns > 1 { gap.max(0.0) } else { 0.0 };
        let content = match bands {
            BandPolicy::Reserved => {
                let margin = margin_x.max(0.0);
                let available = (safe.w - margin * 2.0).max(0.0);
                let width = if max_measure > 0.0 {
                    available.min(max_measure.max(0.0))
                } else {
                    available
                };
                PageRect::new(
                    safe.x + margin,
                    header.bottom(),
                    width,
                    (footer.y - header.bottom()).max(0.0),
                )
            }
            // 页图铺满整窗：两条带浮在它上面，不从 content 里减。
            BandPolicy::Overlay => paper,
        };

        Self {
            paper,
            safe,
            content,
            header,
            footer,
            header_paint,
            footer_paint,
            columns,
            gap,
            bands,
        }
    }

    /// 交给引擎的那一份。定版式没有（它不排版、不分片）。
    pub fn metrics(&self) -> Option<PageMetrics> {
        match self.bands {
            BandPolicy::Reserved => Some(PageMetrics {
                column_width: column_width(self.content.w, self.columns, self.gap),
                content_height: self.content.h,
                columns: self.columns,
                gap: self.gap,
            }),
            BandPolicy::Overlay => None,
        }
    }

    /// 第 `index` 栏的矩形。双页时是左/右半页。
    pub fn column(&self, index: usize) -> PageRect {
        self.content.column(index, self.columns, self.gap)
    }
}

#[cfg(test)]
mod tests {
    use super::{BandPolicy, PageBox, PageInsets, PageRect};

    fn paper() -> PageRect {
        PageRect::new(0.0, 0.0, 384.0, 853.0)
    }

    fn insets() -> PageInsets {
        PageInsets {
            top: 24.0,
            bottom: 24.0,
            left: 0.0,
            right: 0.0,
        }
    }

    fn reflowable() -> PageBox {
        PageBox::new(
            paper(),
            insets(),
            28.0,
            28.0,
            24.0,
            0.0,
            1,
            0.0,
            BandPolicy::Reserved,
        )
    }

    fn fixed() -> PageBox {
        PageBox::new(
            paper(),
            insets(),
            28.0,
            28.0,
            0.0,
            0.0,
            1,
            0.0,
            BandPolicy::Overlay,
        )
    }

    #[test]
    fn reserved_bands_put_content_between_them() {
        let page = reflowable();
        assert_eq!(page.safe, PageRect::new(0.0, 24.0, 384.0, 805.0));
        assert_eq!(page.header, PageRect::new(0.0, 24.0, 384.0, 28.0));
        assert_eq!(page.footer, PageRect::new(0.0, 801.0, 384.0, 28.0));
        assert_eq!(page.content, PageRect::new(24.0, 52.0, 336.0, 749.0));
        assert_eq!(page.content.y, page.header.bottom());
        assert_eq!(page.content.bottom(), page.footer.y);
        assert_eq!(
            page.metrics().unwrap().content_height,
            749.0,
            "正文高就是两带之间那一段"
        );
        assert_eq!(page.metrics().unwrap().column_width, 336.0);
    }

    #[test]
    fn overlay_bands_do_not_shrink_the_page() {
        let page = fixed();
        assert_eq!(page.content, paper(), "定版式页图铺满整窗");
        assert_eq!(page.content.h, 853.0);
        assert_eq!(page.header, PageRect::new(0.0, 24.0, 384.0, 28.0));
        assert_eq!(page.footer, PageRect::new(0.0, 801.0, 384.0, 28.0));
        assert!(page.metrics().is_none(), "定版式不排版、不分片");
    }

    #[test]
    fn band_paint_reaches_the_paper_edges() {
        let page = reflowable();
        assert_eq!(page.header_paint, PageRect::new(0.0, 0.0, 384.0, 52.0));
        assert_eq!(page.footer_paint, PageRect::new(0.0, 801.0, 384.0, 52.0));
    }

    #[test]
    fn max_measure_caps_the_text_but_keeps_the_left_edge() {
        let page = PageBox::new(
            PageRect::new(0.0, 0.0, 800.0, 1000.0),
            PageInsets::default(),
            28.0,
            28.0,
            24.0,
            600.0,
            1,
            0.0,
            BandPolicy::Reserved,
        );
        assert_eq!(page.content.x, 24.0);
        assert_eq!(page.content.w, 600.0);
        assert_eq!(
            page.content.h, 944.0,
            "页高只由带子决定，不受 max_measure 影响"
        );
    }

    #[test]
    fn columns_split_with_the_shared_gap() {
        let page = PageBox::new(
            paper(),
            insets(),
            28.0,
            28.0,
            24.0,
            0.0,
            2,
            24.0,
            BandPolicy::Reserved,
        );
        assert_eq!(page.metrics().unwrap().column_width, 156.0);
        assert_eq!(page.column(0), PageRect::new(24.0, 52.0, 156.0, 749.0));
        assert_eq!(page.column(1), PageRect::new(204.0, 52.0, 156.0, 749.0));
        assert_eq!(
            page.column(1).right(),
            page.content.right(),
            "两栏加中间缝正好是正文宽"
        );
    }

    #[test]
    fn a_paper_smaller_than_its_insets_does_not_panic() {
        let page = PageBox::new(
            PageRect::new(0.0, 0.0, 10.0, 10.0),
            PageInsets {
                top: 20.0,
                bottom: 20.0,
                left: 20.0,
                right: 20.0,
            },
            28.0,
            28.0,
            24.0,
            0.0,
            1,
            0.0,
            BandPolicy::Reserved,
        );
        assert!(page.safe.is_empty());
        assert!(page.content.is_empty());
        assert_eq!(page.content.h, 0.0);
    }

    #[test]
    fn the_two_bands_never_overlap_the_content_in_reserved_mode() {
        let page = reflowable();
        assert!(page.header.bottom() <= page.content.y);
        assert!(page.content.bottom() <= page.footer.y);
        assert!(!page.header.contains(0.0, 60.0));
        assert!(page.content.contains(24.0, 60.0));
    }
}
