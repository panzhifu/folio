//! 翻页的**决策层**：输入只回答「往哪边走」，这里回答「这一步落在哪」。
//!
//! 连续滚动与分页/双页在状态上看起来只差一个 `if`，但差别不只是「动哪个字段」：
//!
//! - 连续滚动一步是「视口往下挪 90%」，夹在整条拼接链的 `[0, max_scroll]` 里；
//! - 分页一步是「章内页号 ±1」，到章界必须换成 [`TurnPlan::Chapter`] ——
//!   从前那条分支只把页号夹在 `0..pages-1`，章末就是一条死路（读者按方向键没反应）。
//!
//! 把这条决策抽成纯函数有几个直接好处：
//!
//! - 方向键、点击分区、音量键、滚轮章界走的是同一个答案，不会四套规则；
//! - 章界规则能在宿主的 `cargo test` 里表驱动地钉住，不需要开窗口；
//! - 「排版还没就绪」可以被显式表达成 `None`，而不是拿上一章的 `pages` 乱跳。
//!
//! 决策层**不碰** gpui、不碰 `ReaderSession`：输入是一份 [`PagingFacts`] 快照，
//! 输出是一个 [`TurnPlan`]。真正改 `scroll_y` / `page` / 换章的动作在
//! `modules/navigation.rs`，动画在 `view.rs`。

use crate::ui::features::reader::model::chrome::PageMode;

/// 一次离散翻页的方向（**阅读序**，不是屏幕物理方向）。
///
/// 左/右键在 RTL 书里换手，这个换算在输入层做（[`crate::ui::features::reader::model::chrome::Arrow::turn`]）；
/// 到了这里只剩「上一页 / 下一页」，策略层就不必知道书往哪边翻。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::ui::features::reader) enum TurnIntent {
    Previous,
    Next,
}

impl TurnIntent {
    /// 读书顺序里的 ±1。`0` 与超出范围的值都不产生意图。
    pub(in crate::ui::features::reader) fn from_delta(delta: isize) -> Option<Self> {
        match delta.signum() {
            -1 => Some(Self::Previous),
            1 => Some(Self::Next),
            _ => None,
        }
    }

    pub(in crate::ui::features::reader) fn delta(self) -> isize {
        match self {
            Self::Previous => -1,
            Self::Next => 1,
        }
    }
}

/// 策略层做决定所需的**事实**。
///
/// 不带 `ReaderSession` 引用，是为了让两个实现都能在纯单元测试里直接跑；
/// 也避免「策略顺手改状态」这种把判断和副作用混在一起的写法。
#[derive(Clone, Copy, Debug)]
pub(in crate::ui::features::reader) struct PagingFacts {
    pub page: usize,
    /// 本章页数；双页模式是「张数」。`0` = 还没切出来。
    pub pages: usize,
    pub scroll: f32,
    pub max_scroll: f32,
    pub viewport: f32,
    pub can_prev_chapter: bool,
    pub can_next_chapter: bool,
    /// 本章排版/切片是否已就绪。没就绪时策略一律返回 `None`。
    pub ready: bool,
    /// 定版式没有章节可跨；`can_*_chapter` 在会话侧已经会是 `false`，这一格只为让
    /// `Paged` 的测试能显式表达「没有章」。
    pub fixed: bool,
}

/// 策略给出的下一步。会话负责把它落成状态与动画。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::ui::features::reader) enum TurnPlan {
    /// 这一步不翻（书首/书尾、排版未就绪、或已经在边界）。
    None,
    /// 连续滚动：把 `scroll_y` 补间到 `to`。
    Scroll { to: f32 },
    /// 章内翻一页。
    Page { to: usize },
    /// 章界：换章。正数往后、负数往前。
    Chapter { delta: isize },
}

/// 两种摆法各自一套决策。
///
/// 用 trait 而不是在会话里写两个 `if`：章界规则是纯的，能单独测；以后加「仿真翻页」
/// 或横向卷轴时，也只多一个实现，不用再改方向键/点击/滚轮四处入口。
pub(in crate::ui::features::reader) trait PagingStrategy:
    Sync
{
    fn plan(&self, facts: &PagingFacts, intent: TurnIntent) -> TurnPlan;
}

/// 连续滚动：一步 = 视口高的 90%，夹在拼接链的可滚范围内。
pub(in crate::ui::features::reader) struct Continuous;

impl PagingStrategy for Continuous {
    fn plan(&self, facts: &PagingFacts, intent: TurnIntent) -> TurnPlan {
        if !facts.ready {
            return TurnPlan::None;
        }
        let step = facts.viewport * 0.9;
        let target = (facts.scroll + intent.delta() as f32 * step).clamp(0.0, facts.max_scroll);
        // 与 `set_scroll_y` 的早退同一个数：夹在章首/章尾的那一下不该起一趟什么都不动的动画。
        if (target - facts.scroll).abs() < 0.5 {
            return TurnPlan::None;
        }
        TurnPlan::Scroll { to: target }
    }
}

/// 分页 / 双页：章内翻页；到章界换章。双页的 `pages` 已经是张数。
pub(in crate::ui::features::reader) struct Paged;

impl PagingStrategy for Paged {
    fn plan(&self, facts: &PagingFacts, intent: TurnIntent) -> TurnPlan {
        if !facts.ready || facts.pages == 0 {
            return TurnPlan::None;
        }
        match intent {
            TurnIntent::Next => {
                if facts.page + 1 < facts.pages {
                    TurnPlan::Page { to: facts.page + 1 }
                } else if !facts.fixed && facts.can_next_chapter {
                    TurnPlan::Chapter { delta: 1 }
                } else {
                    TurnPlan::None
                }
            }
            TurnIntent::Previous => {
                if facts.page > 0 {
                    TurnPlan::Page { to: facts.page - 1 }
                } else if !facts.fixed && facts.can_prev_chapter {
                    TurnPlan::Chapter { delta: -1 }
                } else {
                    TurnPlan::None
                }
            }
        }
    }
}

impl PageMode {
    /// 这套摆法用哪套决策。定版式走 `Paged`（章界被 `fixed` + `can_*_chapter` 挡住）。
    pub(in crate::ui::features::reader) fn paging(self) -> &'static dyn PagingStrategy {
        static CONTINUOUS: Continuous = Continuous;
        static PAGED: Paged = Paged;
        match self {
            PageMode::Continuous => &CONTINUOUS,
            PageMode::Paged | PageMode::Spread => &PAGED,
        }
    }
}

#[cfg(test)]
mod tests {
    //! 只钉决策，不碰窗口。章界那几条是这一层的存在理由 —— 从前方向键在章末
    //! `set_page` 夹一下就没下文了。

    use super::{
        Continuous, PageMode, Paged, PagingFacts, PagingStrategy as _, TurnIntent, TurnPlan,
    };

    fn facts(page: usize, pages: usize) -> PagingFacts {
        PagingFacts {
            page,
            pages,
            scroll: 0.0,
            max_scroll: 0.0,
            viewport: 800.0,
            can_prev_chapter: true,
            can_next_chapter: true,
            ready: true,
            fixed: false,
        }
    }

    #[test]
    fn a_paged_step_stays_inside_the_chapter_until_the_edge() {
        let paged = Paged;
        assert_eq!(
            paged.plan(&facts(1, 4), TurnIntent::Next),
            TurnPlan::Page { to: 2 }
        );
        assert_eq!(
            paged.plan(&facts(1, 4), TurnIntent::Previous),
            TurnPlan::Page { to: 0 }
        );
    }

    #[test]
    fn a_paged_edge_turns_the_chapter_instead_of_stopping() {
        let paged = Paged;
        assert_eq!(
            paged.plan(&facts(3, 4), TurnIntent::Next),
            TurnPlan::Chapter { delta: 1 }
        );
        assert_eq!(
            paged.plan(&facts(0, 4), TurnIntent::Previous),
            TurnPlan::Chapter { delta: -1 }
        );
        // 一页章：进去就能继续走，这正是「第一章按方向键没反应」的根。
        assert_eq!(
            paged.plan(&facts(0, 1), TurnIntent::Next),
            TurnPlan::Chapter { delta: 1 }
        );
    }

    #[test]
    fn the_book_edges_and_unready_layout_do_nothing() {
        let paged = Paged;
        let mut edge = facts(3, 4);
        edge.can_next_chapter = false;
        assert_eq!(paged.plan(&edge, TurnIntent::Next), TurnPlan::None);
        let mut start = facts(0, 4);
        start.can_prev_chapter = false;
        assert_eq!(paged.plan(&start, TurnIntent::Previous), TurnPlan::None);

        let mut pending = facts(1, 4);
        pending.ready = false;
        assert_eq!(paged.plan(&pending, TurnIntent::Next), TurnPlan::None);
        assert_eq!(
            paged.plan(&facts(0, 0), TurnIntent::Next),
            TurnPlan::None,
            "还没切出页时不能拿旧页数乱跳"
        );
    }

    #[test]
    fn continuous_takes_ninety_percent_of_the_viewport_and_clamps() {
        let continuous = Continuous;
        let mut f = facts(0, 1);
        f.max_scroll = 5000.0;
        f.viewport = 800.0;
        assert_eq!(
            continuous.plan(&f, TurnIntent::Next),
            TurnPlan::Scroll { to: 720.0 }
        );
        f.scroll = 5000.0;
        assert_eq!(continuous.plan(&f, TurnIntent::Next), TurnPlan::None);
    }

    #[test]
    fn spread_shares_the_paged_plan() {
        assert_eq!(
            PageMode::Spread
                .paging()
                .plan(&facts(3, 4), TurnIntent::Next),
            TurnPlan::Chapter { delta: 1 }
        );
    }
}
