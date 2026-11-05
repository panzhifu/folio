//! 导航栏让位的判据：一次竖向滚动增量 → 该收还是该放。
//!
//! ## 为什么住在导航栏这一层，而不是每个目的地各自判
//!
//! 从前只有架面判：它从自己的虚拟列表句柄读位移，用方向加滞回算出「该让位了」，再
//! `ShelfEvent::Fold` 报给外壳。结果就是**只有一屏有这行为** —— 统计、笔记、「我的」
//! 那三屏各是一块 `overflow_y_scroll`，读者在那些屏上往回翻到底栏不会让开，切回架面
//! 又会，读起来像 bug。
//!
//! 各屏自己判要三件东西：句柄、判据、上报的路。gpui 的滚动容器把 `ScrollHandle` 关在
//! gpui-kit 的 `Scrollable` 里面（桌面「我的」那一屏的滚动甚至在 kit 的 `Settings`
//! 组件内部），四个目的地要各接一次，接不全就是新的不统一。而滚动事件本身会**冒泡**
//! （`InteractiveElement::on_scroll` 挂在 bubble 相），外壳在目的地那一格上听一次就
//! 全覆盖：一句判据、一处状态、四屏同一档行为。
//!
//! ## 为什么用增量而不是位置
//!
//! 架面那份判据读的是滚动位移，于是它需要两条配套修正才能不抖：视口自己变高时把那一段
//! 位移剔掉（[`super`] 旧笔记里那条「滑到底导航栏自己弹出来」）、以及离顶部多近一定展开。
//! 增量这一路这两条都不存在 —— 视口变化不发滚动事件，滚到顶再往下拽给的是往回的增量，
//! 自然就展开。所以这一格只剩一个概念：**攒够多少**。

/// 往下读攒够多少逻辑像素才让位。太短会跟着每一段滚动抖，太长读者会以为界面卡在半路。
/// 这两个数沿用架面当年量出来的那一档（18 / 8）：换判据的来源不该顺手改手感。
const FOLD_TRAVEL: f32 = 18.0;

/// 往回拨多少就放回来。**不是零**：滚到列表尽头时内容会有一小段回弹，零阈值会把那几像素
/// 读成「读者要往回翻」，于是滑到底那一刻导航栏自己弹出来 —— 读者报的正是这一下。
const UNFOLD_TRAVEL: f32 = 8.0;

/// 一次事件里小过这个数的增量当抖动，不计入累积（与旧判据那条同一条：gpui 每帧可能给
/// 一个极小的修正量）。
const JITTER: f32 = 0.5;

/// 导航栏的让位状态。`Default` = 没让位（新起的窗口、换目的地、开书前后都从这一档开始）。
#[derive(Default)]
pub(in crate::ui::shell) struct FoldTracker {
    folded: bool,
    /// 攒着的位移：符号就是方向（正 = 往下读）。换向时重新攒，来回抖动不该把阈值攒满。
    travel: f32,
}

impl FoldTracker {
    pub(in crate::ui::shell) fn folded(&self) -> bool {
        self.folded
    }

    /// 吃一次竖向滚动增量。gpui 的约定是**向下为负**（与架面那句「向下滚 1000 传 -1000」
    /// 同一份，也与滚动容器把 `scroll_offset += delta` 施加在 ≤ 0 的偏移上自洽）。
    ///
    /// 回**状态变没变** —— 只有变了才值得敲一次重画：滚动每帧都给事件，而让位是低频事。
    pub(in crate::ui::shell) fn scroll(&mut self, delta_y: f32, reduce_motion: bool) -> bool {
        // 减动效那一档整个不做让位：收起是一次空间变化，不是必须传达的信息（与架面同一条）。
        if reduce_motion {
            return self.set(false);
        }
        if delta_y.abs() < JITTER {
            return false;
        }
        let toward_content = -delta_y;
        self.travel = if self.travel.signum() == toward_content.signum() {
            self.travel + toward_content
        } else {
            toward_content
        };
        if self.travel >= FOLD_TRAVEL {
            self.travel = 0.0;
            self.set(true)
        } else if self.travel <= -UNFOLD_TRAVEL {
            self.travel = 0.0;
            self.set(false)
        } else {
            false
        }
    }

    /// 回到「没让位」：换目的地、栈弹回一页、合上一本书。回变没变。
    pub(in crate::ui::shell) fn reset(&mut self) -> bool {
        self.travel = 0.0;
        self.set(false)
    }

    fn set(&mut self, folded: bool) -> bool {
        let changed = self.folded != folded;
        self.folded = folded;
        if changed {
            self.travel = 0.0;
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::FoldTracker;
    use super::{FOLD_TRAVEL, JITTER, UNFOLD_TRAVEL};

    /// 往下读攒够 [`FOLD_TRAVEL`] 才让位；往回拨 [`UNFOLD_TRAVEL`] 就放回来。
    /// 两个阈值不同是刻意的：找回导航不该等攒够距离，而抖一下不该把栏收掉。
    /// 两个阈值的**大小关系**是设计的一部分（收要攒、放只认方向），写成编译期断言：
    /// 运行时那条 assert 会被 clippy 报「常量值断言」，而这个关系根本不该等到跑起来才判。
    const _: () = assert!(FOLD_TRAVEL > UNFOLD_TRAVEL);

    #[test]
    fn the_two_thresholds_are_deliberately_different() {
        let mut tracker = FoldTracker::default();
        assert!(!tracker.scroll(-10.0, false), "10 像素不该就收");
        assert!(!tracker.folded());
        assert!(tracker.scroll(-10.0, false), "攒到 20 该换了");
        assert!(tracker.folded());
        assert!(!tracker.scroll(4.0, false), "回拨 4 像素不到线");
        assert!(tracker.folded(), "放回来要等够那 8 像素");
        assert!(tracker.scroll(6.0, false));
        assert!(!tracker.folded());
    }

    /// 来回抖动不攒阈值：换向就从零重新攒。否则「一下上一下下」各 10 像素也能把栏收掉。
    #[test]
    fn a_direction_change_restarts_the_accumulator() {
        let mut tracker = FoldTracker::default();
        for _ in 0..20 {
            tracker.scroll(-12.0, false);
            tracker.scroll(12.0, false);
        }
        assert!(!tracker.folded(), "上下各半的抖动不该让位");
    }

    /// 小过 [`JITTER`] 的增量不计入：gpui 每帧可能补一个极小的修正量。
    /// 线两边各取一点，改阈值时这条会跟着动。
    #[test]
    fn sub_pixel_increments_are_noise() {
        let mut tracker = FoldTracker::default();
        let noise = JITTER - 0.1;
        for _ in 0..200 {
            assert!(!tracker.scroll(-noise, false));
        }
        assert!(!tracker.folded(), "抖了 200 帧也不该让位");
        assert!(tracker.scroll(-(JITTER + 0.5), false) || !tracker.folded());
        tracker.reset();
        // 攒够线才算数：一格一格攒，攒到 `FOLD_TRAVEL` 那一下才换。
        let steps = (FOLD_TRAVEL / (JITTER + 0.5)).ceil() as usize;
        for _ in 0..steps {
            tracker.scroll(-(JITTER + 0.5), false);
        }
        assert!(tracker.folded(), "攒过 FOLD_TRAVEL 就该让位");
    }

    /// 减动效那一档永不让位，并且把已经让出去的那一条放回来。
    #[test]
    fn reduce_motion_never_folds() {
        let mut tracker = FoldTracker::default();
        assert!(tracker.scroll(-100.0, false));
        assert!(tracker.folded());
        assert!(tracker.scroll(0.0, true), "开了减动效要立刻放回来");
        assert!(!tracker.folded());
        assert!(!tracker.scroll(-500.0, true));
        assert!(!tracker.folded());
    }

    /// 换目的地 / 弹回一页 / 合上书都回到「没让位」，且只在真变了时报变化。
    #[test]
    fn a_reset_unfolds_once_and_clears_the_partial_travel() {
        let mut tracker = FoldTracker::default();
        tracker.scroll(-12.0, false);
        assert!(!tracker.folded(), "半条线不该就让位");
        assert!(!tracker.reset(), "本来就没让位，不该敲重画");
        // 攒了一半没到线，reset 把它清零：重置后要从头攒，不能接上那 12 像素立刻让位。
        assert!(!tracker.scroll(-12.0, false));
        assert!(!tracker.folded());
        assert!(tracker.scroll(-12.0, false), "重新攒够 FOLD_TRAVEL 才让位");
        assert!(tracker.folded());
        assert!(tracker.reset(), "让着位被重置，这一次变了");
        assert!(!tracker.folded());
    }
}
