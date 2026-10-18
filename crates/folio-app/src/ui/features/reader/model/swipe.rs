//! 移动端跟手翻页：把手指那一趟拖动折成「这一趟走了多少」，抬手时判走完还是弹回。
//!
//! 分两半，与 [`turn_animation`] 同一形状：**这一份里判据全是纯函数**（[`passed`] 定
//! 「是不是在拖」、[`dominant`] 定轴、[`toward`] 折阅读方向、[`progress`] 归一、[`snap`]
//! 判落点），能在宿主的 `cargo test` 里表驱动钉住；[`swipe_scroll`] 那一头只做
//! 事件搬运，被 `on_scroll` 在滚轮监听里调（`view.rs` 挂在 `folio-reader` 那个 `div` 上的
//! `.on_scroll_wheel`），所以这一层自己不再 `on_mouse_event`。
//!
//! **事件走的是滚动那一路，不是 `TouchDragEvent`。** 识别器在手指走过 `touch_slop` 那一刻
//! 就把这一笔判给了某一个轴（`gpui-pre-0.3.9/src/gestures.rs` 的 `Panning`：`dominant_axis`
//! 定轴、`lock_delta_to_axis` 锁死），此后一路只发锁了轴的 `ScrollWheelEvent`，抬手再补一条
//! `Ended`。而 `TouchDragEvent` 只在**按下那一下**递一次探针（`window.rs:5816` 那句
//! `offer_touch_drag`），接不接要在还看不出这一下是点按还是拖动的那一刻用 `prevent_default`
//! 答 —— 一接就把点屏翻页与长按吸词一起吃掉。横扫本来就已经作为滚动事件送到了
//! `folio-reader` 那个监听（`on_scroll` 从前只读 `delta.y`，横的那一半被整份丢掉），顺着这一
//! 路接走即可，平台侧一个字都不用改。
//!
//! 五条不动的规矩：
//!
//! - **页号只在松手那一刻翻**。跟手期间 [`TurnRun::Following`] 只摆两张纸，权威位置一个字
//!   没动 —— 于是「弹回」不需要撤销任何东西，只要不前进。
//! - **定一次轴就锁死这一趟**。斜着划时 dx 与 dy 都会抖，来回换轴的画面就是一张纸在抽搐。
//! - **章界不跟手**。走到章末再往外翻时目标章还没排版，没有第二张纸可摆；这一趟只记着
//!   方向，松手时交给既有的那一套（点屏同一条路：等排版 → 整趟跨章滑动）。
//! - **两根轴都归翻页，跟手只认那一档的轴**。横着拖与竖着拖都是翻一页；但「纸跟着手指走」
//!   要求拖的那根轴与动画走的那根轴是同一根 —— 纸横着走而手指竖着拖，读起来不是跟手，是画错
//!   了。轴不合的那一半照旧翻页，只是到松手那一刻才走（与点屏、键盘同一条路）。
//! - **一笔只认一次**。让开之后（竖轴、章界、定版式）同一趟里的后续增量不再重新起头 ——
//!   一次斜划中途转横不该半路杀出翻页；抬手后惯性那段合成增量也走这一条。
//!
//! [`swipe_scroll`]: 事件搬运与累计；[`claim_swipe`]: 定轴与认领；[`swipe_release`]: 落定或弹回。

use gpui_kit::{App, Context, TouchPhase};

use crate::ui::features::reader::model::session::{PageRef, ReaderSession, SwipeTrack, Turn, TurnRun, start_turn};
use crate::ui::features::reader::model::turn::{TurnIntent, TurnPlan};
use crate::ui::features::reader::motion::turn_animation::{self, Axis};
use crate::ui::services::config::ConfigHolder;

/// 按下来后要走过多远才算「在拖」而不是「点了一下」。
///
/// 与拖选那条 4px 的阈值（`session::DRAG_THRESHOLD_PX`）**不是同一个数**：那一条判的是
/// 「手指挪过一点点，该出高亮了」，这一条判的是「这一下是翻页」。拇指落在屏上的抖动就有
/// 4–8 像素，取 4 会让每一次点屏都先抖出半张纸。
pub(in crate::ui::features::reader) const SLOP: f32 = 16.0;
/// 抬手时走过这一屏的几成才算「要走完」。
pub(in crate::ui::features::reader) const SNAP_FRACTION: f32 = 1.0 / 3.0;

/// 走过 [`SLOP`] 了吗（按平面距离算，不按单轴 —— 斜着划也该被认下来）。
pub(in crate::ui::features::reader) fn passed(dx: f32, dy: f32) -> bool {
    dx * dx + dy * dy >= SLOP * SLOP
}

/// 这一趟的主导轴。两轴都不够 `SLOP` 时给 `None` = 还没到定性的时候。
///
/// 「谁长听谁的」而不是「过阈值就算数」：一趟翻页拖动通常在主导轴上走两百像素、另一轴上
/// 抖二十像素，后者也过了阈值，拿它参与判断就会在斜着划时把轴换来换去。
pub(in crate::ui::features::reader) fn dominant(dx: f32, dy: f32) -> Option<Axis> {
    if !passed(dx, dy) {
        return None;
    }
    Some(if dx.abs() >= dy.abs() {
        Axis::Horizontal
    } else {
        Axis::Vertical
    })
}

/// 沿这一档的轴、**按读书顺序**折好符号的那一段位移：正 = 往后翻（下一页）。
///
/// 横轴在从右往左读的书里整体反号（那一本里往右划才是下一页）；纵轴与书的阅读方向无关，
/// 手指往下划（`dy > 0`）永远是「回到上一页」—— 与连续滚动 `scroll_y -= dy` 同一口径。
///
/// 这一格只用来**定方向**（[`TurnIntent::from_delta`] 取它的符号）。要算「走了多远」得先过
/// [`travel`] —— 往上一页那一趟它天生是负的，直接喂 [`progress`] 会被夹成 0。
pub(in crate::ui::features::reader) fn toward(axis: Axis, rtl: bool, dx: f32, dy: f32) -> f32 {
    match axis {
        Axis::Horizontal if rtl => dx,
        Axis::Horizontal => -dx,
        Axis::Vertical => -dy,
    }
}

/// 把读书顺序那一段位移折到**已认领的那一步**上：往后翻原样，往前翻反号。
///
/// 认领之后那一步的方向已经定了，「走了多远」该按它量：往上一页那一趟在读书顺序里是负的，
/// 但手指确实实打实拖过了那么远。从前两处都直接把 [`toward`] 喂给 [`progress`] 与 [`snap`]，
/// 负的那一半被夹成 0 —— 症状是横扫只能往后翻，往回拖那一下纸不动、松手也永远不过线。
/// 拖到认领方向的反面仍然给 0（[`progress`] 那条夹法），画面停在「还没起步」那一帧。
pub(in crate::ui::features::reader) fn travel(along: f32, intent: TurnIntent) -> f32 {
    match intent {
        TurnIntent::Next => along,
        TurnIntent::Previous => -along,
    }
}

/// 位移折成「这一趟走了多少」：落在 `[0, 1]`。
///
/// 负的不给负数而是夹到 0：手指拖过去又拖回起点以外时，画面停在「还没起步」那一帧，等
/// 下一次拖回来。给一个负进度，两张纸就会反向穿过彼此。
pub(in crate::ui::features::reader) fn progress(toward: f32, dist: f32) -> f32 {
    if dist <= 0.0 {
        return 0.0;
    }
    (toward / dist).clamp(0.0, 1.0)
}

/// 抬手：走完还是弹回。只看过没过 [`SNAP_FRACTION`]。
///
/// 不看速度 —— 这一版没有可靠的逐帧时间戳，而「慢而长」的一笔本来就该翻页、「快而短」的
/// 一抖不该。真机上若「轻轻一甩就该翻」成了抱怨，再补速度那一条。
pub(in crate::ui::features::reader) fn snap(toward: f32, dist: f32) -> bool {
    progress(toward, dist) >= SNAP_FRACTION
}

impl ReaderSession {
    /// 这一笔能不能归翻页管。给回 `(读者那一档走的轴, 沿**这一笔那根轴**的一屏有多长,
    /// 往哪边读)`；`None` = 让回去。
    ///
    /// 让开的三种情况各有各的理由：连续滚动里手指本来就是「滚」（那一档没有「一页」可摆）；
    /// 定版式那一臂没有正文切片可摆（`view.rs` 的 `Source::Fixed` 那条）；而那一屏还没量出来
    /// 时距离是 0，进度无从归一。
    ///
    /// 距离按**这一笔**的轴量，不按动画的轴：竖着拖半屏就是竖着拖半屏，哪怕读者选的是一档
    /// 横着走的动画。两件事在这里分开，正是「轴不合也翻页、只是不跟手」那条规矩的落点。
    pub(in crate::ui::features::reader) fn swipe_geometry(
        &self,
        cx: &App,
        drag: Axis,
    ) -> Option<(Axis, f32, bool)> {
        if self.laid_out.mode.is_continuous() || self.is_fixed() || self.book().is_none() {
            return None;
        }
        let style = ConfigHolder::appearance(cx).page_turn;
        let dist = match drag {
            Axis::Horizontal => f32::from(self.content_bounds.get().size.width),
            Axis::Vertical => self.laid_out.viewport,
        };
        (dist > 0.0).then(|| (turn_animation::of(style).axis(), dist, self.progression_rtl()))
    }

    /// 滚动那一路的一步增量落到跟手翻页。`step_x` / `step_y` 是**这一帧走的那一段**，
    /// 累计由这一层自己做（[`SwipeTrack`]）。
    ///
    /// 给回 `true` = 这一趟归翻页管，调用方（`on_scroll`）不该再走纵向那两条路。
    pub(in crate::ui::features::reader) fn swipe_scroll(
        &mut self,
        phase: TouchPhase,
        step_x: f32,
        step_y: f32,
        cx: &mut Context<Self>,
    ) -> bool {
        match phase {
            // 一笔的开头。识别器要走过它自己的 `touch_slop` 才发这一条，且带来的那一段
            // 是从按下点算起的整截（它把 slop 也留在里面，内容才不落后于手指）。
            TouchPhase::Started => {
                self.swipe = Some(SwipeTrack::default());
                self.swipe_walk(step_x, step_y, cx)
            }
            // 没有跟踪态的 `Moved` 只有两种来源：让开之后的同一趟（规矩第五条：不再起头）
            // 与抬手后惯性那段合成增量。两种都不该被认成新的一笔。
            TouchPhase::Moved => self.swipe_walk(step_x, step_y, cx),
            TouchPhase::Ended | TouchPhase::Cancelled => {
                if self.swipe.is_none() {
                    return false;
                }
                self.swipe_walk(step_x, step_y, cx);
                self.swipe_release(cx)
            }
        }
    }

    /// 把这一步记进累计，然后按累计值决定：还没定性的试着定性，已认领的摆跟手。
    fn swipe_walk(&mut self, step_x: f32, step_y: f32, cx: &mut Context<Self>) -> bool {
        let Some(track) = self.swipe.as_mut() else {
            return false;
        };
        track.dx += step_x;
        track.dy += step_y;
        let (dx, dy, claimed) = (track.dx, track.dy, track.claim.is_some());
        if !claimed && !self.claim_swipe(dx, dy, cx) {
            return false;
        }
        // 已经认领。摆得出第二张纸的就跟手，摆不出的（章界、轴不合、不走动画那一档）也照样
        // 回 `true` —— 那一趟仍然归翻页管，只是它到松手那一刻才落地。
        let Some(track) = self.swipe else {
            return true;
        };
        let Some((axis, intent)) = track.claim else {
            return true;
        };
        let Some((style_axis, dist, rtl)) = self.swipe_geometry(cx, axis) else {
            return true;
        };
        if style_axis != axis {
            return true; // 轴不合：等松手，那一趟走的是读者选的那一档
        }
        let along = travel(toward(axis, rtl, track.dx, track.dy), intent);
        let Some(turn) = self.turn.as_mut() else {
            return true;
        };
        turn.run = TurnRun::Following {
            t: progress(along, dist),
        };
        cx.notify();
        true
    }

    /// 定性并认领这一趟。给回 `false` 有两种意思，靠要不要清掉跟踪态区分：
    /// 还没过阈值（留着，那一下仍可能是点按）或这一趟根本不该归翻页（清掉，让回去）。
    fn claim_swipe(&mut self, dx: f32, dy: f32, cx: &mut Context<Self>) -> bool {
        if !passed(dx, dy) {
            return false; // 还在阈值之内
        }
        let Some(drag) = dominant(dx, dy) else {
            self.swipe = None;
            return false;
        };
        let Some((style_axis, _, rtl)) = self.swipe_geometry(cx, drag) else {
            self.swipe = None;
            return false;
        };
        let Some(intent) = TurnIntent::from_delta(toward(drag, rtl, dx, dy).signum() as isize)
        else {
            self.swipe = None;
            return false;
        };
        // 问决策层那一步是什么：章界与书尾都不摆第二张纸，但章界仍归这一趟管
        // （松手时把方向交给既有的那一套，跟点屏同一条路）。
        let facts = self.paging_facts();
        let plan = self.laid_out.mode.paging().plan(&facts, intent);
        if matches!(plan, TurnPlan::None | TurnPlan::Scroll { .. }) {
            self.swipe = None;
            return false;
        }
        if let Some(track) = self.swipe.as_mut() {
            track.claim = Some((drag, intent));
        }
        // 手指之前攒下的那一截滚动增量作废：不然松手后它还会再多翻一页。
        self.wheel_accum = 0.0;
        // 轴不合就不摆第二张纸：那一趟到松手才走，走的是读者选的那一档。
        if style_axis != drag {
            return true;
        }
        let TurnPlan::Page { to } = plan else {
            return true;
        };
        if to == self.page() {
            return true;
        }
        let chapter = self.chapter_index();
        // 页号**不动**：`leaving` 是眼下这一页（权威），`arriving` 是松手后可能去的那一页。
        let leaving = PageRef {
            chapter,
            page: self.page(),
        };
        let arriving = PageRef { chapter, page: to };
        let style = ConfigHolder::appearance(cx).page_turn;
        let mode = self.laid_out.mode;
        start_turn(
            &mut self.turn,
            &mut self.turn_seq,
            style,
            mode,
            Turn {
                leaving: Some(leaving),
                arriving: Some(arriving),
                forward: intent == TurnIntent::Next,
                run: TurnRun::Following { t: 0.0 },
                ..Default::default()
            },
        );
        cx.notify();
        true
    }

    /// 抬手：走完就把页号翻过去、剩下那一截接着走完；没过线就退回原地、页号一个字不动。
    /// 没摆跟手那一趟的（章界、`PageTurn::None`）落回点屏那条路，方向照旧。
    fn swipe_release(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(track) = self.swipe.take() else {
            return false;
        };
        let Some((axis, intent)) = track.claim else {
            return false; // 没定性过：这一按该由点按那条路处理（我们不该消费它）
        };
        let following = self
            .turn
            .is_some_and(|turn| matches!(turn.run, TurnRun::Following { .. }));
        if !following {
            self.turn(intent, cx);
            return true;
        }
        let Some((_, dist, rtl)) = self.swipe_geometry(cx, axis) else {
            self.cancel_turn();
            return true;
        };
        let along = travel(toward(axis, rtl, track.dx, track.dy), intent);
        let from = progress(along, dist);
        let revert = !snap(along, dist);
        let target = self
            .turn
            .and_then(|turn| turn.arriving)
            .map(|page| page.page)
            .unwrap_or(self.page());
        if !revert {
            // 先翻页再补完剩下的那半截 —— 与点屏那条同一顺序，落库、清选中都跟着走。
            self.set_page(target, cx);
        }
        // 换一把时钟键：松手这一腿是从 `from` 起跑的新的一趟，复用旧键会把跟手期间
        // 攒下的「已走完」状态接过来，画面当场落定、看不出还有一段在走。
        self.turn_seq += 1;
        if let Some(turn) = self.turn.as_mut() {
            turn.seq = self.turn_seq;
            turn.run = TurnRun::Released { from, revert };
        }
        cx.notify();
        true
    }
}

#[cfg(test)]
mod tests {
    //! 只钉这一层独有的那五条判据。事件搬运那一半不用在真机上验：`view::tests` 里
    //! 「横扫该翻页」那条发的是**原始触摸**，走的就是识别器 → 滚动事件这一条真路。

    use super::{
        Axis, SLOP, SNAP_FRACTION, TurnIntent, dominant, passed, progress, snap, toward, travel,
    };

    const DIST: f32 = 400.0;

    /// 没过阈值时既不算「在拖」也不定轴：那一下大概率是点屏翻页。
    #[test]
    fn a_small_move_is_still_a_tap() {
        assert!(!passed(4.0, 4.0), "拇指抖动不该被认成拖动");
        assert!(passed(SLOP, 0.0));
        assert!(passed(0.0, SLOP), "按平面距离算，不看单轴");
        assert_eq!(dominant(3.0, 2.0), None, "没过阈值就还没到定性的时候");
    }

    /// 主导轴 = 长的那一根；两轴一样长时算横的。
    #[test]
    fn the_longer_axis_wins() {
        assert_eq!(dominant(-120.0, 30.0), Some(Axis::Horizontal));
        assert_eq!(dominant(30.0, -120.0), Some(Axis::Vertical));
        assert_eq!(dominant(-200.0, -200.0), Some(Axis::Horizontal));
    }

    /// 折符号：横轴在从右往左读的书里整体反号，纵轴与阅读方向无关，而「手指往下划」
    /// 永远读成回上一页 —— 与连续滚动 `scroll_y -= dy` 同一口径。
    #[test]
    fn the_reading_direction_folds_the_sign() {
        assert_eq!(
            toward(Axis::Horizontal, false, -100.0, 0.0),
            100.0,
            "左划=往后翻"
        );
        assert_eq!(
            toward(Axis::Horizontal, false, 100.0, 0.0),
            -100.0,
            "右划=往前翻"
        );
        assert_eq!(
            toward(Axis::Horizontal, true, 100.0, 0.0),
            100.0,
            "RTL 里右划才是往后翻"
        );
        assert_eq!(toward(Axis::Horizontal, true, -100.0, 0.0), -100.0);
        assert_eq!(
            toward(Axis::Vertical, false, 0.0, 100.0),
            -100.0,
            "往下划=回上一页"
        );
        assert_eq!(
            toward(Axis::Vertical, true, 0.0, -100.0),
            100.0,
            "竖轴不看阅读方向"
        );
    }

    /// 进度夹在 `[0, 1]`：拖过去又拖回起点以外时停在「还没起步」那一帧；
    /// 那一屏还没量过时不该起任何进度。
    #[test]
    fn the_progress_is_clamped_into_one_screen() {
        assert_eq!(progress(0.0, DIST), 0.0);
        assert_eq!(progress(DIST * 0.5, DIST), 0.5);
        assert_eq!(progress(DIST * 3.0, DIST), 1.0, "拖过一屏也只算走完");
        assert_eq!(progress(-100.0, DIST), 0.0, "拖回起点之外不给负进度");
        assert_eq!(progress(100.0, 0.0), 0.0, "版心还没量过时不起动画");
    }

    /// 抬手判据：过三分之一才落定，差一点就弹回 —— 这一条决定「松手后到底翻没翻」。
    #[test]
    fn it_lands_only_past_a_third_of_the_screen() {
        let edge = SNAP_FRACTION * DIST;
        assert!(snap(edge, DIST), "刚好过线该走完");
        assert!(snap(edge + 1.0, DIST));
        assert!(!snap(edge - 1.0, DIST), "差一点就弹回，不该翻");
        assert!(!snap(-edge, DIST), "往反方向拖绝不该被当成翻页");
    }

    /// 认领之后距离按**那一步**算，不按读书顺序算：回上一页那一趟在读书顺序里天生是负的，
    /// 直接喂 [`progress`] 会被夹成 0 —— 症状就是横扫只有「往后翻」半个方向能用。
    #[test]
    fn the_distance_is_measured_along_the_claimed_step() {
        // 顺读的书里左划 = 往后翻：读书顺序为正，`travel` 原样递过去。
        let next = toward(Axis::Horizontal, false, -200.0, 0.0);
        assert_eq!(travel(next, TurnIntent::Next), 200.0);
        assert!(snap(travel(next, TurnIntent::Next), DIST));
        // 右划 = 回上一页：读书顺序那一段是负的，反一次号才是手指真正走过的距离。
        let prev = toward(Axis::Horizontal, false, 200.0, 0.0);
        assert_eq!(prev, -200.0, "回上一页那一趟在读书顺序里是负的");
        assert_eq!(travel(prev, TurnIntent::Previous), 200.0);
        assert!(snap(travel(prev, TurnIntent::Previous), DIST), "回上一页也该过得了线");
        // 认领之后拖到那一步的反面：仍给 0，画面停在「还没起步」那一帧。
        assert_eq!(progress(-travel(prev, TurnIntent::Previous), DIST), 0.0);
        // RTL 里整套符号反过来，句式子不变：那一本里右划才是往后翻。
        assert_eq!(
            travel(toward(Axis::Horizontal, true, 200.0, 0.0), TurnIntent::Next),
            200.0
        );
        assert_eq!(
            travel(toward(Axis::Horizontal, true, -200.0, 0.0), TurnIntent::Previous),
            200.0
        );
    }
}
