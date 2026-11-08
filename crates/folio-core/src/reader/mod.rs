//! 阅读器里最纯的那几块：摆法模式（[`PageMode`]）与翻页决策（[`turn`]）、
//! 屏幕几何（[`geometry`]）、拼接坐标与全书进度的公式（[`book`]）。
//!
//! 这一层**不碰** gpui、不碰任何会话状态：输入是纯数据快照，输出是纯数据计划。
//! 方向键、点击分区、音量键、鼠标滚轮走的是同一个答案，章界规则、窗口边界与页框
//! 几何能在 `cargo test` 里表驱动地钉住，不需要开窗口。真正改 `scroll_y` / `page` /
//! 换章的动作与动画、手势、浮层都住在 app。

pub mod book;
pub mod geometry;
pub mod turn;

pub use book::{Link, Placement};
pub use geometry::{BandPolicy, ChromeBands, PageBox, PageInsets, PageMetrics, PageRect};

/// 双页对开时一栏最少要多宽（逻辑像素）。低于这个宽度，一行只容得下十几个字，
/// 「两页」就变成了两条窄柱子，读者的眼球要在柱子间来回跳 —— 那是比单页更差的阅读。
pub const MIN_COLUMN_WIDTH: f32 = 200.0;
/// 读者把页边距调到 0 时中缝的下限：两页之间总得有一条缝，否则页界靠猜。
pub const MIN_SPREAD_GAP: f32 = 24.0;

use crate::infra::config::PageTurn;
use crate::layout::fragment::column_width;

/// 正文的呈现模式。**三种模式是同一套分片代码的三个参数**，不是三条代码路
/// （见 [`crate::layout::fragment`]）：区别只有「一片多高」与「一页几栏」。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PageMode {
    /// 连续滚动：一章一条到底的正文。只有「翻页方式」里选了「上下滚动」才走到这里
    /// （见 [`reconcile`]），它不再是谁的默认值。
    Continuous,
    /// 整页翻，一页一栏。**默认档**：一本没动过设置的书就是无动画左右翻页。
    #[default]
    Paged,
    /// 双页对开：一页两栏。
    Spread,
}

impl PageMode {
    /// 一页几栏。
    pub fn columns(self) -> usize {
        match self {
            Self::Spread => 2,
            Self::Continuous | Self::Paged => 1,
        }
    }

    /// 是不是连续滚动（只有它是「一片高 0」，也就只有它靠滚动位置而不是页号定位）。
    pub fn is_continuous(self) -> bool {
        matches!(self, Self::Continuous)
    }

    /// 装不下两栏时退回整页翻。手机上点「双页」不会得到两条窄柱子，而是分页 ——
    /// 这一档由宽度决定、不由 target 决定，所以横屏的平板真能双页，竖屏手机不能。
    pub fn effective(self, total_width: f32, gap: f32) -> Self {
        if self == Self::Spread && !spread_fits(total_width, gap) {
            Self::Paged
        } else {
            self
        }
    }

    /// 落库用的模式名。进度本身存的是「第几个字」，与模式无关，所以换模式不丢位置；
    /// 这个名字记的是「这份进度当时长什么样」，重开一本书时照它把模式也带回来。
    pub fn storage_name(self) -> &'static str {
        match self {
            Self::Continuous => "continuous",
            Self::Paged => "page",
            Self::Spread => "spread",
        }
    }

    /// 读回的模式名认不出来就当默认档：旧库里的名字、手改过的配置都不该让书打不开。
    pub fn from_storage_name(name: &str) -> Self {
        match name {
            "continuous" => Self::Continuous,
            "spread" => Self::Spread,
            _ => Self::Paged,
        }
    }

    /// 阅读设置里「分页」那一行摆出来的两档：一页一栏与一页两栏。
    ///
    /// **连续滚动不在这一行里**：那一档只由「翻页方式」那条列表里唯一的入口决定
    /// （选「上下滚动」，见 [`reconcile`]）。一行里出现两栏与滚动，读者按下哪一格都会
    /// 把另一件事顶掉，那就是两个控件在抢同一件事。
    pub const PAGE_ROW: [Self; 2] = [Self::Paged, Self::Spread];
}

/// 双页的中缝：取读者自己那条页边距，并给一条下限。
///
/// 为什么跟着 `margin_x` 走而不是定一个数：左右两页读起来该是「两页」，中缝比外面
/// 那条边距窄就会变成「一块被硬切开的正文」，页界靠猜。
pub fn spread_gap(margin_x: f32) -> f32 {
    margin_x.max(MIN_SPREAD_GAP)
}

/// 这宽度装不装得下双页。`total` 是**已扣掉左右页边距**的可用宽（[`column_width`] 只算栏）。
pub fn spread_fits(total: f32, gap: f32) -> bool {
    column_width(total, 2, gap) >= MIN_COLUMN_WIDTH
}

/// 读者从没为这本书选过档位时，按窗口宽给一个默认：**装得下两栏就双页**，装不下就整页翻。
/// 平板与桌面上那条侧栏让出来之后，1024 宽的横屏正是靠这一条长出双页的。
///
/// **默认永不给 `Continuous`**：连续滚动是一种要读者主动选的读法（它就摆在「翻页方式」
/// 那条列表里，叫「上下滚动」），不是「还没选」时的替身。从前这里装不下两栏时回
/// Continuous，于是「翻页方式」里选的平移 / 覆盖 / 仿真折页在手机上永远不出场 ——
/// 两轴各存一份真相、又互相否决，读起来就是「选了不生效」。
pub fn default_mode(total: f32, gap: f32) -> PageMode {
    if spread_fits(total, gap) {
        PageMode::Spread
    } else {
        PageMode::Paged
    }
}

/// 「翻页方式」里那一档与摆法之间**唯一**的那条耦合。
///
/// 摆法与动画档是两件事（双页可以配任何一种纸面动画），但有一对组合是自相矛盾的：
/// 连续滚动里没有「翻页」这个动作，而纸面动画在连续里永远不出场。这一格回答
/// 「选了这一档，摆法必须跟着去哪」，两个写入口（底栏那一格与设置屏那一格）因此共用
/// 同一条判据，而不是各筛一次 —— 设置屏那一格正是因为它不知道摆法、当初故意不筛，
/// 才让读者选到一个当场不出场的档。
///
/// `by_width` 是「这本书没选过摆法时该长成什么样」（[`default_mode`] 给出来的那个数）：
/// 从连续里走出去时没有别的依据可退，双页还是单页只有宽度答得了。
///
/// 只在**渲染帧**里解一次（见 app 的 `view::body`），不在写配置那一头解：设置屏改的是
/// 全局 `appearance.page_turn`，而摆法是按书存在会话里的，两头隔着一条边界。
pub fn reconcile(turn: PageTurn, mode: PageMode, by_width: PageMode) -> PageMode {
    match turn {
        // 选了「上下滚动」：摆法就是连续，别管读者之前在看单页还是双页。
        PageTurn::Scroll => PageMode::Continuous,
        // 选了任何一张纸的走法却还站在连续里：退到按宽度该长出的那一档。
        _ if mode == PageMode::Continuous => by_width,
        // 其余组合都讲得通，原样交回。
        _ => mode,
    }
}

#[cfg(test)]
mod tests {
    use super::{MIN_COLUMN_WIDTH, PageMode, default_mode, reconcile, spread_fits, spread_gap};
    use crate::infra::config::PageTurn;

    #[test]
    fn spread_yields_to_paged_when_two_columns_do_not_fit() {
        // 手机上那点宽度（减掉 48×2 页边距后约 190）装不下两栏 → 退回整页翻。
        assert_eq!(
            PageMode::Spread.effective(190.0, spread_gap(48.0)),
            PageMode::Paged
        );
        // 桌面窗口铺开就装得下。
        assert_eq!(
            PageMode::Spread.effective(900.0, spread_gap(48.0)),
            PageMode::Spread
        );
        // 连续滚动与分页不改变档：退档只可能发生在使用户要双页的那一步。
        assert_eq!(
            PageMode::Continuous.effective(0.0, 0.0),
            PageMode::Continuous
        );
        assert_eq!(PageMode::Paged.effective(0.0, 0.0), PageMode::Paged);

        // 判据是「一栏至少多宽」，所以正好卡在临界点上。
        let gap = 24.0;
        assert!(spread_fits(MIN_COLUMN_WIDTH * 2.0 + gap, gap));
        assert!(!spread_fits(MIN_COLUMN_WIDTH * 2.0 + gap - 1.0, gap));
        // 页边距调到 0 也留一条缝，别让两页贴成一条。
        assert_eq!(spread_gap(0.0), 24.0);
        assert_eq!(spread_gap(64.0), 64.0);
    }

    /// 默认档的那条线画在哪：装得下两栏给双页，装不下给**分页**，任何宽度都不给连续。
    ///
    /// 这条守的是「选了不生效」那一半的根：从前装不下两栏时默认回 Continuous，于是
    /// 手机上「翻页方式」里那四档纸面动画全都在一个永不出场的摆法里等着，读者在设置屏
    /// （那一格不按摆法筛）选到仿真翻页，屏上什么都不动。
    #[test]
    fn the_default_mode_follows_the_width_and_never_lands_in_continuous() {
        let gap = spread_gap(24.0);
        assert_eq!(default_mode(1200.0, gap), PageMode::Spread);
        assert_eq!(default_mode(400.0, gap), PageMode::Paged);
        // 临界点与 spread_fits 同一条线，不多让也不早让。
        assert_eq!(
            default_mode(MIN_COLUMN_WIDTH * 2.0 + gap, gap),
            PageMode::Spread
        );
        assert_eq!(
            default_mode(MIN_COLUMN_WIDTH * 2.0 + gap - 1.0, gap),
            PageMode::Paged
        );
        for width in [0.0, 100.0, 400.0, 900.0, 1200.0, 4000.0] {
            assert_ne!(
                default_mode(width, gap),
                PageMode::Continuous,
                "{width} 的默认档算出了连续滚动"
            );
        }
    }

    /// 一条列表 + 一颗「双页」按钮，合起来必须**永远**给出一个自洽的摆法：
    /// 连续滚动当且仅当选了「上下滚动」。这条是 [`reconcile`] 的全部契约，也是
    /// 读者那句「现在不是根据选择的翻页效果进行翻页」的正面。把 `reconcile` 整个
    /// 退回成「原样交回 self.mode」，这一条在 (Slide, Continuous) 那一格当场红。
    #[test]
    fn every_turn_entry_lands_on_a_mode_that_can_run_it() {
        let by_width = PageMode::Spread;
        for turn in PageTurn::ALL.iter().copied() {
            for started in [PageMode::Continuous, PageMode::Paged, PageMode::Spread] {
                let mode = reconcile(turn, started, by_width);
                assert_eq!(
                    mode == PageMode::Continuous,
                    turn == PageTurn::Scroll,
                    "{turn:?} 从 {started:?} 出发落进了 {mode:?}：连续滚动与纸面动画同时成立"
                );
            }
        }
        // 装不下两栏的那本（默认档是单页）从连续里走出去时，落的是单页不是双页。
        assert_eq!(
            reconcile(PageTurn::Curl, PageMode::Continuous, PageMode::Paged),
            PageMode::Paged
        );
    }

    /// 「分页」那一行摆的两档：只有单页与双页，**永不含连续滚动** —— 那一格按下去只会把
    /// 读者从滚动里踢回纸面，是两件不相干的事挤在同一个入口里。这条从前由那颗按钮的
    /// 循环律（`next`）答，按钮搬进设置、改成显式选档之后，由这一行摆哪两档答。
    #[test]
    fn the_page_row_offers_single_and_double_but_never_continuous() {
        assert_eq!(
            PageMode::PAGE_ROW,
            [PageMode::Paged, PageMode::Spread],
            "「分页」那一行该摆单页与双页两档"
        );
        for mode in [PageMode::Continuous, PageMode::Paged, PageMode::Spread] {
            assert_eq!(
                PageMode::from_storage_name(mode.storage_name()),
                mode,
                "{} 存出去再读回来必须是同一档",
                mode.storage_name()
            );
        }
        // 认不出的名字（旧库、手改的 JSON）当默认档，不是 panic、不是双页。
        assert_eq!(PageMode::from_storage_name(""), PageMode::Paged);
        assert_eq!(PageMode::from_storage_name("paginated"), PageMode::Paged);
    }
}
