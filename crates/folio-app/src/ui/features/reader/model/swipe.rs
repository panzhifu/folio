//! 移动端跟手翻页：把手指那一趟拖动折成「这一趟走了多少」，抬手时判走完还是弹回。
//!
//! 分两半，与 [`turn_animation`] 同一形状：**这一份里判据全是纯函数**（[`passed`] 定
//! 「是不是在拖」、[`dominant`] 定轴、[`toward`] 折阅读方向、[`progress`] 归一、[`snap`]
//! 判落点），能在宿主的 `cargo test` 里表驱动钉住；[`swipe_scroll`] 那一头只做
//! 事件搬运，被 `on_scroll` 在滚轮监听里调（`view.rs` 挂在 `folio-reader` 那个 `div` 上的
//! `.on_scroll_wheel`），所以这一层自己不再 `on_mouse_event`。松手速度也在事件搬运那一头量
//! （[`SwipeTrack::note_step`]：`ScrollWheelEvent` 自己不带时间戳，钟取
//! `cx.background_executor().now()`），折成沿已认领那一步的那个数之后与距离一起喂 [`snap`]。
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

use std::rc::Rc;
use std::time::{Duration, Instant};

use folio_core::PageTurn;
use gpui_kit::base::motion::{Easing, Transition};
use gpui_kit::{App, Context, Pixels, Point, TouchPhase};

use crate::ui::features::reader::model::chrome::PageMode;
use crate::ui::features::reader::model::session::{
    PageRef, ReaderSession, SwipeTrack, Turn, TurnRun, start_turn,
};
use crate::ui::features::reader::model::turn::{TurnIntent, TurnPlan};
use crate::ui::features::reader::motion::turn_animation::{self, Axis};
use crate::ui::services::config::ConfigHolder;
use crate::ui::shell::turn_duration;

/// 按下来后要走过多远才算「在拖」而不是「点了一下」。
///
/// 与拖选那条 4px 的阈值（`session::DRAG_THRESHOLD_PX`）**不是同一个数**：那一条判的是
/// 「手指挪过一点点，该出高亮了」，这一条判的是「这一下是翻页」。拇指落在屏上的抖动就有
/// 4–8 像素，取 4 会让每一次点屏都先抖出半张纸。
pub(in crate::ui::features::reader) const SLOP: f32 = 16.0;
/// 抬手时走过这一屏的几成才算「要走完」（慢速那一半的唯一判据）。
///
/// 从前是 1/3：384 宽的一屏要拖过 128 才落定，慢拖不到半屏就弹回，读起来是纸跟了手而
/// 松手没翻。这一格只降到 0.2（同一屏 77），**没有**一路降到 1/10 —— 撑住「轻碰一下不该
/// 翻页」的不是这一格，是 [`snap`] 里速度那一条连同它的 [`fling_floor`] 下限：慢而短照旧
/// 弹回，快而短才走。两头各管一头，才敢把这一格往下挪。
pub(in crate::ui::features::reader) const SNAP_FRACTION: f32 = 0.2;
/// 靠速度落定时，至少走过这一屏的几成。
pub(in crate::ui::features::reader) const FLING_MIN_FRACTION: f32 = 0.08;
/// 与上面那条取大者的**下限**（逻辑像素）：屏再窄也至少走过这一截才允许靠速度落定。
///
/// 这一格是「极短促的误触」唯一那道闸：只有速度判据的话，拇指在屏上蹭一下的两三毫秒
/// 三十像素就能算出上千像素每秒。
pub(in crate::ui::features::reader) const FLING_MIN_PX: f32 = 36.0;
/// 松手那一下沿已认领那一步至少这么快（逻辑像素/秒 —— 手机上 gpui 的 Pixels 就是 dp，
/// 见 `gpui-mobile` 的 `AndroidDisplay::logical_width`），短距离那一档才翻页。
pub(in crate::ui::features::reader) const FLING_SPEED: f32 = 700.0;
/// 速度取样：这一段至少攒够这么久才除一次。识别器会合帧，两条事件挤在同一毫秒时
/// 除出来的是天文数字。
const FLING_SAMPLE_MIN: Duration = Duration::from_millis(8);
/// 停手这么久，「最近一次有位移的速度」作废：拖到一半停住再松手不该翻页。
///
/// 比识别器那一条动量的门宽（`gpui-pre/src/gestures.rs` 的 `VELOCITY_ASSUME_STOPPED_GAP`
/// = 40ms，量出来是给连续滚动收尾用的）：动量松手之后还要替手指滚很长一段，判错一眼就看得见；
/// 翻页只在意松手那一下，这一截可以给得宽，但 100 是上限那一头 —— 抬手前已经停这么久，
/// 读者的意图是「停住」，不是「甩」。
const FLING_STOPPED: Duration = Duration::from_millis(100);

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

/// 靠速度落定时那一截最低距离：页宽的 [`FLING_MIN_FRACTION`] 与 [`FLING_MIN_PX`] 取大者。
///
/// 取大而不是取小：36 是「拇指蹭一下」的下限，8% 是「窄屏上别把半屏当一甩」的比例，
/// 哪一头更严听哪一头的。384 宽那一屏上它是 36，800 宽（平板横档）上是 64。
pub(in crate::ui::features::reader) fn fling_floor(dist: f32) -> f32 {
    (dist * FLING_MIN_FRACTION).max(FLING_MIN_PX)
}

/// 抬手：走完还是弹回。两律任一成立就走完。
///
/// - `along >= 页宽 × [`SNAP_FRACTION`]` —— 慢而长的一笔本来就该翻页。
/// - `along >= [`fling_floor`]` **且** `speed >= [`FLING_SPEED`]` —— 短而快的一甩。
///
/// `along` 是沿**已认领那一步**真正走过的距离，`speed` 是松手那一下沿同一步的速度，
/// 两者同一把尺、同一个符号（反着甩 `speed` 就是负的，过不了第二条）。速度由事件层量
/// （[`SwipeTrack::note_step`] / [`SwipeTrack::speed_along`]），停过手传进来的就是 0。
pub(in crate::ui::features::reader) fn snap(along: f32, dist: f32, speed: f32) -> bool {
    progress(along, dist) >= SNAP_FRACTION || (along >= fling_floor(dist) && speed >= FLING_SPEED)
}

/// 松手那一腿的时长下限。
///
/// 再狠的一甩也不许短到看不清纸怎么落（落定读成「啪」）。上限不在这儿：上限就是 `fallback`
/// （读者那一格「翻页时长」，`crate::ui::shell::turn_duration`），见 [`release_duration`]。
const RELEASE_MIN: Duration = Duration::from_millis(120);

/// 松手那一腿走多久：**以松手那一刻的速度起步、匀减速到落点正好停住**。
///
/// 匀减速走完剩余行程 `Δ` 用时 `T` ⇒ `Δ = v·T/2` ⇒ `T = 2Δ/v`。这一格同时替掉两处旧毛病：
///
/// - 从前这一腿用全仓共用的那一个转场时长（280 ms、`EaseInOut`），而那条 S 形**两端速度都是
///   0** —— 松手前纸正跟着手指在动，手一抬纸先停死再重新加速，读起来就是「顿一下」。
/// - 时长与还剩多少路无关：拖到 95% 松手，最后 5% 也走满 280 ms（慢慢爬完）；拖到 21% 松手，
///   79% 也在 280 ms 里走完。同一只手，两种快慢差四倍。
///
/// 参照那三家没有一家读手指速度，但**没有一家用两端零速的曲线**，而且时长都跟着剩余行程走：
/// `harism/android-pagecurl` 固定 300 ms 而 pointer 是**线性**插值（起步即全速）；`StPageFlip`
/// 干脆 `duration = 剩余帧数 / 1000 × flippingTime`（恒定速度、时长 ∝ 距离）；`XBPageCurl` 固定
/// 300 ms + EaseInOut，靠**就近吸附点**让剩余行程天生短。这里取 StPageFlip 那条比例再乘上实测
/// 速度，两头各夹一次：
///
/// - **上不越过 `fallback`**：慢而长的一笔按公式能算出 600–700 ms，那是「纸自己磨」。夹住之后
///   起点速度反而**高于**手指那一刻（`2Δ/T > v`），读起来是纸抢跑半步，不是停死 —— 误差只朝
///   这一边偏，是有意的。
/// - **下不到 [`RELEASE_MIN`] 为止**：一甩落定也得让人看见纸怎么落的。
///
/// 速度读成 0（拖到一半停过手再松，[`SwipeTrack::speed_along`] 按新鲜度判的那一格）就整条退回
/// `fallback`，等于今天的行为，只是曲线换了。
pub(in crate::ui::features::reader) fn release_duration(
    remaining_dp: f32,
    speed_dp_s: f32,
    fallback: Duration,
) -> Duration {
    if remaining_dp <= 0.0 || speed_dp_s <= 0.0 {
        return fallback;
    }
    let ms = 2000.0 * f64::from(remaining_dp) / f64::from(speed_dp_s);
    Duration::from_millis(
        ms.clamp(RELEASE_MIN.as_millis() as f64, fallback.as_millis() as f64) as u64,
    )
}

/// 松手那一腿的形状：走了这一腿几成时间 → 走了几成路。
///
/// `2x − x²` 就是「匀减速」那条抛物线本身：起点斜率 2、终点斜率 0。斜率 2 不是调出来的数 ——
/// 由 [`release_duration`] 那句 `T = 2Δ/v` 反解，匀减速恰好让第一帧的速度等于松手时手指的速度。
/// 换掉的正是 `EaseInOut` 的起点斜率 0。
fn release_shape(x: f32) -> f32 {
    x * (2.0 - x)
}

/// 松手那一腿的**整条补间**：时长与曲线一处给全，渲染那头只管拿它去问时钟。
///
/// 两半分开挑的话，「按了速度算出时长」与「用了两端零速的曲线」会各自成立、合起来仍是「手一抬
/// 纸先停死」—— 所以收在这一个出口里。
pub(in crate::ui::features::reader) fn release_transition(over: Duration) -> Transition {
    Transition::new(over).easing(release_easing())
}

/// 那条曲线本身，单列一个出口：`Transition::sample` 不对外，光测 [`release_transition`] 钉不住
/// 曲线，只钉得住时长。
pub(in crate::ui::features::reader) fn release_easing() -> Easing {
    Easing::Custom(Rc::new(release_shape))
}

/// 一笔拖动这一帧该给谁。
///
/// 三档而不是布尔：`Swallow` 正是这次要修的那个洞的反面 —— 「这一笔归翻页管，但这一帧
/// 什么都不做」。从前它是 `false`，于是轴不对、书尾、松手后的惯性尾巴一律漏进滚动 / 滚轮
/// 那条路，一次竖向拖动变成「翻页 + 又滚又翻」。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::ui::features::reader) enum SwipeStep {
    /// 归翻页（或刚落地）。
    Turn,
    /// 归翻页，但这一帧什么都不做。调用方**不要**滚动。
    Swallow,
    /// 不归翻页，调用方照旧（连续滚动里手指就是滚）。
    Ignore,
}

/// 定性这一步的三种答案。
enum Claim {
    /// 这一趟归翻页，方向已经锁好。
    Claimed,
    /// 还没走够 [`SLOP`]，那一下仍可能是点按：留着跟踪态。
    NotYet,
    /// 这一趟不归翻页（轴不对 / 书尾 / 定版式）：标记掉，后面每一步都 `Swallow`。
    Refused,
}

impl SwipeTrack {
    /// 每一步量一次「这一段走了多快」，攒够 [`FLING_SAMPLE_MIN`] 才除一次。
    ///
    /// 三条各自的用处，缺一条就出对应的错：
    ///
    /// - **不逐事件除**：识别器会合帧，两条挤在同一毫秒时除出来的是天文数字。没攒够就不动
    ///   分母起点，增量继续往这一段里攒。
    /// - **这一段没走过位移就不更新**：松手那一条 `Ended` 恒为零增量，除掉来的是 0，会把
    ///   「最近一次**有位移**的速度」抹掉 —— 于是轻轻一甩永远过不了速度那一档。
    /// - **这一段的时长过 [`FLING_STOPPED`] 就丢**：中间停过手，除出来的是「含一段静止」的
    ///   平均，既不是松手那一下的，也可能虚高（拖一半停住、停手前那一格攒了二百像素，
    ///   一百多毫秒一除还剩一千多）。丢它而不是清零：清零由 [`SwipeTrack::speed_along`]
    ///   按新鲜度判，只有一处说「作废」。
    fn note_step(&mut self, now: Instant) {
        let Some((from, x, y)) = self.speed_from else {
            // 这一笔的第一格只立分母的起点：还没有一段时间可以除。
            self.speed_from = Some((now, self.dx, self.dy));
            return;
        };
        let dt = now.duration_since(from);
        if dt < FLING_SAMPLE_MIN {
            return;
        }
        self.speed_from = Some((now, self.dx, self.dy));
        let seg = (self.dx - x, self.dy - y);
        if seg.0 == 0.0 && seg.1 == 0.0 {
            return;
        }
        if dt > FLING_STOPPED {
            return;
        }
        let secs = dt.as_secs_f32();
        self.speed = (seg.0 / secs, seg.1 / secs);
        self.speed_at = Some(now);
    }

    /// 松手那一下沿**已认领那一步**有多快（与 `along` 同一把尺、同一个符号）。
    ///
    /// 最近那次取样距今过 [`FLING_STOPPED`] 给 0 —— 「拖到一半停住再松手」到这里就是没有速度。
    fn speed_along(&self, axis: Axis, rtl: bool, intent: TurnIntent, now: Instant) -> f32 {
        let fresh = self
            .speed_at
            .is_some_and(|at| now.duration_since(at) <= FLING_STOPPED);
        if !fresh {
            return 0.0;
        }
        travel(toward(axis, rtl, self.speed.0, self.speed.1), intent)
    }
}

impl ReaderSession {
    /// 这一屏这一档的翻页轴、沿**那一根轴**的一屏有多长、往哪边读。
    ///
    /// 两件事分开判：**「这一笔算不算翻页」**由 `PageTurnStyle::claims` 看手指那根轴（折页两轴
    /// 都认，其余只认自己那根），**「走了多远」永远沿这一档自己的轴**（`PageTurnStyle::axis`）量
    /// —— 与动画那一头 `area.distance(self.axis())` 同一把尺。从前两头都按手指那根轴量：折页是一笔
    /// 两轴的，于是一笔竖着划的按页高归一，纸几乎没卷、进度却已经过半，松手读出来是「页翻了纸没
    /// 动」。其余几档 `claims` 只认自己那根，两把尺本来就重合。
    /// `None` = 这一屏没有「翻页」这件事：连续滚动（手指就是滚）、定版式（没有正文切片）、
    /// 还没量出版心（距离是 0，进度无从归一）、或者没有书。
    pub(in crate::ui::features::reader) fn swipe_geometry(
        &self,
        cx: &App,
        axis: Axis,
        intent: Option<TurnIntent>,
    ) -> Option<(Axis, f32, bool)> {
        if self.is_fixed() || self.book().is_none() || self.laid_out.mode.is_continuous() {
            return None;
        }
        let style = turn_animation::of(ConfigHolder::appearance(cx).page_turn);
        if !style.claims(axis) {
            return None;
        }
        // 归一化与判轴用的是同一块版心：[`ReaderSession::page_area`]，与折线、捕获纹理同一把尺。
        // 双页的「一屏」是**一张 leaf**，不是整张跨页：从右侧划到中缝就该走满，不是走一半。
        // 这一条从前只认折页，于是双页里选平移时手指按整块版心归一、纸却只走半块 ——
        // 拖到一半纸就落定了，而「走过大半屏才翻页」那一条判据还在按整块量。
        let rtl = self.progression_rtl();
        let dist = if self.laid_out.mode == PageMode::Spread {
            let free_right = intent.is_none_or(|intent| (intent == TurnIntent::Next) != rtl);
            self.page_area.leaf(free_right).distance(style.axis())
        } else {
            self.page_area.distance(style.axis())
        };
        (dist > 0.0).then(|| (style.axis(), dist, rtl))
    }

    /// 跨章那一笔的落点：目标章**已经排好版并切好页**才给得出页号，否则 `None`。
    ///
    /// 往后翻落在第 0 页、往回翻落在它末页 —— 与 `view::body` 里起跨章滑动那一句同一个取法，
    /// 两处各数一遍「目标章第几页」迟早有一处走样。`None` 只有一种读法：那一章还没排版完，
    /// 这一笔不跟手（不是不翻 —— 松手仍交给 `turn()` 那条等排版的路）。
    pub(in crate::ui::features::reader) fn chapter_landing(&self, delta: isize) -> Option<PageRef> {
        let target = self.chapter_index() as isize + delta;
        let book = self.book()?;
        if target < 0 || target as usize >= book.chapter_count() {
            return None;
        }
        let doc = book.span_doc(target as usize)?;
        if !doc.is_current() {
            return None;
        }
        let page = if delta > 0 {
            0
        } else {
            doc.page_count().saturating_sub(1)
        };
        Some(PageRef {
            chapter: target as usize,
            page,
        })
    }

    /// 把一个**窗口坐标**下的落点折成 [`ReaderSession::page_area`] 里的归一化位置。
    ///
    /// `origin` 是挂监听那一栏在窗口里的原点：`page_area` 记的是这一栏自己的坐标，两头差着
    /// 这一个数。两个轴都不夹 —— 读者把角拖到页外，折线才走得完。
    pub(in crate::ui::features::reader) fn finger_of(
        &self,
        at: Point<Pixels>,
        origin: Point<Pixels>,
    ) -> (f32, f32) {
        let area = self.page_area;
        (
            (f32::from(at.x) - f32::from(origin.x) - area.left) / area.width().max(1.0),
            (f32::from(at.y) - f32::from(origin.y) - area.top) / area.height().max(1.0),
        )
    }

    /// 滚动那一路的一步增量落到跟手翻页。`step_x` / `step_y` 是**这一帧走的那一段**，
    /// 累计由这一层自己做（[`SwipeTrack`]）。
    ///
    /// `wheel` = 这一条事件的增量是按「行」给的（鼠标滚轮），不是手指拖动。滚轮不是手势：
    /// 它一到就把「上一笔的惯性尾巴」这道闸门放下，否则触屏设备上拖完一次之后，
    /// 第一格滚轮会被当成惯性吃掉。
    pub(in crate::ui::features::reader) fn swipe_scroll(
        &mut self,
        phase: TouchPhase,
        step_x: f32,
        step_y: f32,
        wheel: bool,
        at: Option<(f32, f32)>,
        cx: &mut Context<Self>,
    ) -> SwipeStep {
        if wheel {
            self.swipe_sealed = false;
        }
        // 连续滚动里没有「翻页这一屏」：`swipe_geometry` 对这一档给的是 `None`（那一格自己
        // 写着「连续滚动（手指就是滚）」），可下面 `claim_swipe` 把 `None` 一律读成
        // `Refused`，随后整笔 `Swallow` —— 于是手指走过 `SLOP` 之后一个一个增量都被吞掉，
        // `scroll_y` 再也不动。读者看到的是「选了上下滚动，往下滑没反应」。
        //
        // 这一档必须在跟踪态建起来之前就交还给滚动层：手指的每一段增量就是滚，翻页层一步
        // 也不认领。滚轮不走这条（它没有跟踪态，本来就不会被认领），照旧往下走去分页 / 定
        // 版式那两条路。
        if !wheel && self.laid_out.mode.is_continuous() {
            self.swipe = None;
            return SwipeStep::Ignore;
        }
        match phase {
            // 一笔的开头。识别器要走过它自己的 `touch_slop` 才发这一条，且带来的那一段
            // 是从按下点算起的整截（它把 slop 也留在里面，内容才不落后于手指）。
            TouchPhase::Started => {
                self.swipe_sealed = false;
                self.swipe = Some(SwipeTrack {
                    from: at,
                    at,
                    ..SwipeTrack::default()
                });
                self.swipe_walk(step_x, step_y, cx)
            }
            // 没有跟踪态的 `Moved` 只有两种来源：上一笔松手后的惯性，与「让开之后」的
            // 同一趟。前者由 `swipe_sealed` 吃掉，后者是这里 `Ignore` 的那一半。
            TouchPhase::Moved => {
                if self.swipe.is_none() {
                    return if self.swipe_sealed {
                        SwipeStep::Swallow
                    } else {
                        SwipeStep::Ignore
                    };
                }
                self.swipe_walk(step_x, step_y, cx)
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                if self.swipe.is_none() {
                    return if self.swipe_sealed {
                        SwipeStep::Swallow
                    } else {
                        SwipeStep::Ignore
                    };
                }
                let walked = self.swipe_walk(step_x, step_y, cx);
                if walked == SwipeStep::Ignore {
                    self.swipe = None;
                    return SwipeStep::Ignore;
                }
                // 这一笔已经归翻页管过（不管有没有落地）：收掉跟踪态、落下闸门，
                // 让紧随其后的惯性 `Moved` 全都 `Swallow`。
                self.swipe_release(cx);
                SwipeStep::Turn
            }
        }
    }

    /// 把这一步记进累计，然后按累计值决定：还没定性的试着定性，已认领的摆跟手。
    ///
    /// 手指此刻的位置**不能**读 `ScrollWheelEvent.position`：那一格在整趟拖动里
    /// 恒等于按下点（见 [`SwipeTrack`] 的注释）。优先用原始触摸流给的那一个点（两个轴都在），
    /// 拿不到才退回「起手点 + 累计位移」反推 —— 后者只有一个自由度，识别器把另一根的增量抹成
    /// 了 0。两条来源都每一步写回 [`SwipeTrack::at`]：那条律的**朝向**与**推进量**全读划动向量，
    /// 不更新就钉在按下那一格不动 —— 真机上读成「不跟手」。
    ///
    /// 归一化按 [`ReaderSession::page_area`]，与折线、捕获纹理那块矩形同一把尺。参照里是同一个
    /// 做法：`harism/android-pagecurl` 的 `CurlView.onTouchEvent` 在 `ACTION_MOVE` 里每一步把
    /// 当前点写进 `mCurlPos`，再走一遍 `updateCurlPos`。
    ///
    /// 两条来源都**不夹**在 0..1 里：手指要拖到页的另一侧之外，整页才算折完；夹住了，一笔拖到
    /// 底画面永远停在半页。判定走完/弹回的那一格（[`progress`]）照旧夹，不吃这个数。
    fn swipe_walk(&mut self, step_x: f32, step_y: f32, cx: &mut Context<Self>) -> SwipeStep {
        // 先取版心尺寸与这一格的时刻：下面 `self.swipe.as_mut()` 会把 `self` 的字段借走。
        let now = cx.background_executor().now();
        let area = self.page_area;
        let width = area.width().max(1.0);
        let height = area.height().max(1.0);
        let Some(track) = self.swipe.as_mut() else {
            return SwipeStep::Ignore;
        };
        if track.refused {
            return SwipeStep::Swallow;
        }
        track.dx += step_x;
        track.dy += step_y;
        track.note_step(now);
        // 手指此刻在哪：原始触摸流给得出就用它（两个轴都在），给不出才退回「起手点 + 累计
        // 位移」反推 —— 那一格只有一个自由度，因为识别器把另一根的增量抹成了 0。
        let raw = self.finger.map(|(_, at)| at);
        let at = raw.or(track
            .from
            .map(|from| (from.0 + track.dx / width, from.1 + track.dy / height)));
        track.at = at;
        let grip = match (track.from, at) {
            (Some(from), Some(at)) => Some(turn_animation::Grip::new(from, at)),
            _ => None,
        };
        if let (Some(grip), Some(turn)) = (grip, self.turn.as_mut()) {
            turn.grip = Some(grip);
        }
        let (dx, dy) = (track.dx, track.dy);
        if track.claim.is_none() {
            match self.claim_swipe(dx, dy, cx) {
                Claim::Claimed => {}
                // 没过阈值：连续滚动里手指本来就该带着内容走，分页里则一步都不动。
                Claim::NotYet => {
                    return if self.laid_out.mode.is_continuous() {
                        SwipeStep::Ignore
                    } else {
                        SwipeStep::Swallow
                    };
                }
                Claim::Refused => {
                    if let Some(track) = self.swipe.as_mut() {
                        track.refused = true;
                    }
                    return SwipeStep::Swallow;
                }
            }
        }
        let Some(track) = self.swipe else {
            return SwipeStep::Swallow;
        };
        let Some((axis, intent)) = track.claim else {
            return SwipeStep::Swallow;
        };
        // 距离沿**这一档自己的那根轴**量（折页两轴都认，但它翻的是一张横排页）。
        // 双页折页的「一屏」是一张 leaf；意图已经由 `track.claim` 定下，传进去才能拿到半屏宽。
        let Some((_, dist, rtl)) = self.swipe_geometry(cx, axis, Some(intent)) else {
            return SwipeStep::Swallow;
        };
        let along = travel(toward(axis, rtl, track.dx, track.dy), intent);
        let Some(turn) = self.turn.as_mut() else {
            return SwipeStep::Swallow;
        };
        turn.run = TurnRun::Following {
            t: progress(along, dist),
        };
        cx.notify();
        SwipeStep::Turn
    }

    /// 定性这一趟。
    ///
    /// 「轴不对就让回去」是这次修的那一条：以前轴不合也认领（只是不摆第二张纸），
    /// 松手照样翻一页 —— 于是选了横向动画，竖着划也翻。现在**只有读者选的那一档那根轴
    /// 上的拖动才算翻页**，另一根轴的手势归滚动那一路（连续）或者干脆不动（分页）。
    fn claim_swipe(&mut self, dx: f32, dy: f32, cx: &mut Context<Self>) -> Claim {
        if !passed(dx, dy) {
            return Claim::NotYet;
        }
        let Some(drag) = dominant(dx, dy) else {
            return Claim::Refused;
        };
        // 方向读**手指那根轴**（那一根上位移最大，读数最稳），走了多远沿这一档自己的那根量。
        // `intent` 要先定：双页折页的「一屏」是左/右哪一张 leaf 由它决定，距离也跟着半屏走。
        let rtl = self.progression_rtl();
        let Some(intent) = TurnIntent::from_delta(toward(drag, rtl, dx, dy).signum() as isize)
        else {
            return Claim::Refused;
        };
        let Some((page_axis, _, _)) = self.swipe_geometry(cx, drag, Some(intent)) else {
            return Claim::Refused;
        };
        // 问决策层那一步是什么：书尾与「排版还没就绪」都不该起一趟。
        let facts = self.paging_facts();
        let plan = self.laid_out.mode.paging().plan(&facts, intent);
        if matches!(plan, TurnPlan::None | TurnPlan::Scroll { .. }) {
            return Claim::Refused;
        }
        if let Some(track) = self.swipe.as_mut() {
            track.claim = Some((page_axis, intent));
        }
        // 手指之前攒下的那一截滚动增量作废：不然松手后它还会再多翻一页。
        self.wheel_accum = 0.0;
        let chapter = self.chapter_index();
        // 页号**不动**：`leaving` 是眼下这一页（权威），`arriving` 是松手后可能去的那一页。
        let leaving = PageRef {
            chapter,
            page: self.page(),
        };
        // 这一笔要揭到哪一页去：章内是邻页，跨章是目标章的首页 / 末页。
        //
        // 跨章从前在这一格直接放弃跟手（理由写成「目标章还没排版」），于是**整章只有一页**
        // 的那种插页 —— 一本书里每张整页照片各自成一章 —— 每一划都不跟手，要等抬手由
        // `turn()` 补一趟自动动画，读者读到的就是「划不动、松手才翻」。可邻章预排版每帧都
        // 在做（`view::body` 里那两句 `ensure_paged_span` + `ensure_viewport`），目标章当场
        // 就在手上了，所以这里先问它一句：排好了就照常起跟手。
        let arriving = match plan {
            TurnPlan::Page { to } => PageRef { chapter, page: to },
            TurnPlan::Chapter { delta } => match self.chapter_landing(delta) {
                Some(to) => to,
                // 真还没排好（刚进书、大章的第一帧）：这一笔仍归翻页认领（不落到滚动那条），
                // 但不跟手，松手交给 `turn()` 走原来那条等排版的路。
                None => return Claim::Claimed,
            },
            // 上面 530 那一句已经把这两类挡回去了；这里只为穷尽。
            TurnPlan::None | TurnPlan::Scroll { .. } => return Claim::Refused,
        };
        if arriving == leaving {
            return Claim::Claimed;
        }
        // 旧页捕获还没完整落地时不让这一笔起跟手翻页。否则兜底会退回 CPU 裁页，两张正常
        // 页面叠在一起；等入场动画结束、捕获完整后再划才有那张带正文的卷曲纸。
        let style = ConfigHolder::appearance(cx).page_turn;
        if style == PageTurn::Curl && !self.curl_capture_ready() {
            // 这一笔先不跟手，但**别吞掉**：方向记下来，松手时 `turn()` 会在门开的那一帧
            // 补一趟。返回 `Claimed` 让识别器把整笔归给翻页（不落到滚动），页号仍然不动。
            self.pending_turn = Some(intent);
            return Claim::Claimed;
        }
        let mode = self.laid_out.mode;
        start_turn(
            &mut self.turn,
            &mut self.turn_seq,
            style,
            turn_duration(cx),
            mode,
            Turn {
                leaving: Some(leaving),
                arriving: Some(arriving),
                forward: intent == TurnIntent::Next,
                // 折页那一档：锚点是按下点投影到页的自由边，朝向与推进量看这一笔的划动向量。
                grip: self.swipe.and_then(|track| {
                    track
                        .from
                        .map(|from| turn_animation::Grip::new(from, track.at.unwrap_or(from)))
                }),
                run: TurnRun::Following { t: 0.0 },
                ..Default::default()
            },
        );
        cx.notify();
        Claim::Claimed
    }

    /// 抬手：走完就把页号翻过去、剩下那一截接着走完；没过线就退回原地、页号一个字不动。
    /// 没摆跟手那一趟的（章界、`PageTurn::None`）落回点屏那条路，方向照旧。
    ///
    /// 无论落没落地，这里都把 `swipe_sealed` 放下：紧随其后的惯性 `Moved` 属于这一笔，
    /// 该由翻页这一趟吃掉，不该漏回滚动那条路（从前漏回去，收尾动画被 `cancel_turn()`
    /// 当场按停、`wheel_accum` 还攒出第二页）。
    fn swipe_release(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(track) = self.swipe.take() else {
            return false;
        };
        self.swipe_sealed = true;
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
        let Some((_, dist, rtl)) = self.swipe_geometry(cx, axis, Some(intent)) else {
            self.cancel_turn();
            return true;
        };
        let along = travel(toward(axis, rtl, track.dx, track.dy), intent);
        let from = progress(along, dist);
        let speed = track.speed_along(axis, rtl, intent, cx.background_executor().now());
        let revert = !snap(along, dist, speed);
        // 这一腿还要走多远（dp）：走完 = 剩下那一截页宽，弹回 = 已经走过那一截。
        let remaining = (if revert { from } else { 1.0 - from }) * dist;
        let over = release_duration(remaining, speed, turn_duration(cx));
        let arriving = self.turn.and_then(|turn| turn.arriving);
        if !revert && arriving.is_some_and(|to| to.chapter == self.chapter_index()) {
            // 先翻页再补完剩下的那半截 —— 与点屏那条同一顺序，落库、清选中都跟着走。
            //
            // 跨章那一趟**不在这里换页号**：`set_page` 只动当前章的页号，把「下一章第一页」
            // 落成「本章第一页」。换锚章由动画走到 1.0 那一刻的 `finish_chapter_turn` 接手
            // （`turn.run.lands()` 那一条），位置落库也在它里面。
            let target = arriving.map_or(self.page(), |to| to.page);
            self.set_page(target, cx);
        }
        // 换一把时钟键：松手这一腿是从 `from` 起跑的新的一趟，复用旧键会把跟手期间
        // 攒下的「已走完」状态接过来，画面当场落定、看不出还有一段在走。
        self.turn_seq += 1;
        if let Some(turn) = self.turn.as_mut() {
            turn.seq = self.turn_seq;
            turn.run = TurnRun::Released { from, revert, over };
        }
        cx.notify();
        true
    }
}

#[cfg(test)]
mod tests {
    //! 只钉这一层独有的那五条判据。事件搬运那一半不用在真机上验：`view::tests` 里
    //! 「横扫该翻页」那条发的是**原始触摸**，走的就是识别器 → 滚动事件这一条真路。

    use std::time::Duration;

    use super::{
        Axis, FLING_MIN_PX, FLING_SPEED, RELEASE_MIN, SLOP, SNAP_FRACTION, TurnIntent, dominant,
        fling_floor, passed, progress, release_duration, release_easing, release_shape, snap,
        toward, travel,
    };
    use crate::ui::shell::NAV_TRANSITION;

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

    /// 慢拖那一半的抬手判据：过五分之一才落定，差一点就弹回。
    ///
    /// 速度一律给 0 —— 这一条只钉距离那一律，把速度那一条混进来就分不清是哪一条放行的。
    #[test]
    fn it_lands_only_past_a_fifth_of_the_screen() {
        let edge = SNAP_FRACTION * DIST;
        assert!(snap(edge, DIST, 0.0), "刚好过线该走完");
        assert!(snap(edge + 1.0, DIST, 0.0));
        assert!(!snap(edge - 1.0, DIST, 0.0), "差一点就弹回，不该翻");
        assert!(!snap(-edge, DIST, 0.0), "往反方向拖绝不该被当成翻页");
        // 慢拖到线之前，再快也不该由这一条放行（速度那条自己另有下限，见下面两条）。
        assert!(!snap(edge - 1.0, DIST, FLING_SPEED - 1.0));
    }

    /// 距离没过线但甩得够快，也算走完 —— 这一条救的是「短促一甩」。
    ///
    /// 但速度不是万能：先要走过 [`fling_floor`] 那一截，方向还得是已认领的那一步。
    #[test]
    fn a_fast_flick_lands_below_the_distance_line() {
        let floor = fling_floor(DIST);
        assert!(
            floor < SNAP_FRACTION * DIST,
            "前置：下限那一截要真的低于落定线，否则这条测的是距离那一律"
        );
        assert!(
            snap(floor, DIST, FLING_SPEED),
            "刚好那一截 + 刚好那个速度该落定"
        );
        assert!(
            !snap(floor - 1.0, DIST, FLING_SPEED * 10.0),
            "没走过最小那一截，再快也不翻 —— 挡的是拇指在屏上蹭一下"
        );
        assert!(
            !snap(floor + 1.0, DIST, FLING_SPEED - 1.0),
            "差一点速度就还是弹回"
        );
        assert!(!snap(floor, DIST, -FLING_SPEED), "朝反方向甩不该翻");
        assert!(
            !snap(-floor, DIST, FLING_SPEED * 10.0),
            "拖回起点之外不该翻"
        );
    }

    /// 那一截下限随页宽走：窄屏靠 36 兜住误触，宽屏（平板横档）按 8% 抬上去，
    /// 免得「走过半屏的十分之一」也算一甩。
    #[test]
    fn the_flick_floor_follows_the_page_width() {
        assert_eq!(
            fling_floor(384.0),
            FLING_MIN_PX,
            "384 的 8% 才 30.7，取下限"
        );
        assert_eq!(
            FLING_MIN_PX / 384.0,
            0.09375,
            "所以 384 宽那屏是九分之一的距离"
        );
        assert_eq!(fling_floor(800.0), 64.0, "宽屏取比例那一头：800 的 8%");
        assert!(!snap(50.0, 800.0, 3000.0), "800 宽走过 50 不算甩");
        assert!(snap(64.0, 800.0, FLING_SPEED));
    }

    /// 认领之后距离按**那一步**算，不按读书顺序算：回上一页那一趟在读书顺序里天生是负的，
    /// 直接喂 [`progress`] 会被夹成 0 —— 症状就是横扫只有「往后翻」半个方向能用。
    #[test]
    fn the_distance_is_measured_along_the_claimed_step() {
        // 顺读的书里左划 = 往后翻：读书顺序为正，`travel` 原样递过去。
        let next = toward(Axis::Horizontal, false, -200.0, 0.0);
        assert_eq!(travel(next, TurnIntent::Next), 200.0);
        assert!(snap(travel(next, TurnIntent::Next), DIST, 0.0));
        // 右划 = 回上一页：读书顺序那一段是负的，反一次号才是手指真正走过的距离。
        let prev = toward(Axis::Horizontal, false, 200.0, 0.0);
        assert_eq!(prev, -200.0, "回上一页那一趟在读书顺序里是负的");
        assert_eq!(travel(prev, TurnIntent::Previous), 200.0);
        assert!(
            snap(travel(prev, TurnIntent::Previous), DIST, 0.0),
            "回上一页也该过得了线"
        );
        // 认领之后拖到那一步的反面：仍给 0，画面停在「还没起步」那一帧。
        assert_eq!(progress(-travel(prev, TurnIntent::Previous), DIST), 0.0);
        // RTL 里整套符号反过来，句式子不变：那一本里右划才是往后翻。
        assert_eq!(
            travel(toward(Axis::Horizontal, true, 200.0, 0.0), TurnIntent::Next),
            200.0
        );
        assert_eq!(
            travel(
                toward(Axis::Horizontal, true, -200.0, 0.0),
                TurnIntent::Previous
            ),
            200.0
        );
    }

    /// 松手那一腿的时长**跟着还剩多远走**：同一只手，路长一倍就多花一倍时间。
    ///
    /// 从前它是全仓共用的那一个数，于是拖到 95% 松手也要用满 280 ms 走完最后 5%（慢慢爬完），
    /// 而拖到 21% 松手用同一个数走完 79% —— 同一只手两种快慢差四倍。这两个取样点都落在没被
    /// 夹紧的区间里，比例才读得出来。
    #[test]
    fn the_release_leg_scales_with_what_is_left() {
        // 2Δ/v：250 dp @ 2000 dp/s = 250 ms；125 dp @ 同速 = 125 ms。
        assert_eq!(
            release_duration(250.0, 2000.0, NAV_TRANSITION),
            Duration::from_millis(250)
        );
        assert_eq!(
            release_duration(125.0, 2000.0, NAV_TRANSITION),
            Duration::from_millis(125)
        );
    }

    /// 两头各一道闸：一甩不许短到看不见纸怎么落，慢拖也不许自己磨。
    ///
    /// 上限就是那个共用的转场数 —— 慢而长的一笔按 `2Δ/v` 能算出 600 ms，那是「纸自己爬」。
    /// 夹住之后起点速度反而**高于**手指那一刻，误差只朝「抢跑半步」那一边偏，不朝停死那一边。
    #[test]
    fn the_release_leg_is_clamped_at_both_ends() {
        assert_eq!(
            release_duration(384.0, 12000.0, NAV_TRANSITION),
            RELEASE_MIN,
            "狠甩被夹到下限了吗"
        );
        assert_eq!(
            release_duration(300.0, 1000.0, NAV_TRANSITION),
            NAV_TRANSITION,
            "慢拖不许比共用的那一档更久"
        );
    }

    /// 停过手再松（`SwipeTrack::speed_along` 按新鲜度读成 0）⇒ 整条退回共用的那一档。
    #[test]
    fn a_stalled_release_falls_back_to_the_shared_turn() {
        assert_eq!(release_duration(300.0, 0.0, NAV_TRANSITION), NAV_TRANSITION);
        assert_eq!(
            release_duration(0.0, 3000.0, NAV_TRANSITION),
            NAV_TRANSITION
        );
    }

    /// 松手那一腿的补间**第一格就已经在走**：八分之一时间过，路走过两成。
    ///
    /// 这一条钉的是「时长与曲线一处给全」那一句。只测 [`release_duration`] 测不到它 —— 时长算得
    /// 再对，曲线换回全仓那条 `EaseInOut`（起点斜率 0）仍是「手一抬纸先停死」。线性那一档是
    /// 0.125，匀减速在八分之一处是 `2x − x²` = 0.234，S 形只有 0.06 上下。
    #[test]
    fn the_release_easing_is_already_moving_on_the_first_frame() {
        let first = release_easing().sample(0.125);
        assert!(
            first > 0.2,
            "第一格只走了 {first:.3}：松手那一帧纸没接上速度"
        );
        let end = release_easing().sample(1.0);
        assert!(
            (end - 1.0).abs() < 1e-4,
            "走完没落到 1.0，实得 {end:.4}：那一帧纸会自己跳一下"
        );
    }

    /// 松手那一腿的曲线**起点有速度、终点没有**：起点斜率 2、终点斜率 0。
    ///
    /// 斜率 2 不是调出来的：由 `T = 2Δ/v` 反解，匀减速恰好让起步那一帧的速度等于松手时手指的
    /// 速度。换回全仓那条 `EaseInOut`（起点斜率 0 = 手一抬纸先停死）这一条就红。
    #[test]
    fn the_release_shape_starts_at_full_speed_and_ends_at_rest() {
        let start_slope = release_shape(0.01) / 0.01;
        let end_slope = (1.0 - release_shape(0.99)) / 0.01;
        assert!(
            start_slope > 1.9,
            "起点斜率只剩 {start_slope:.2}，松手那一帧纸会先停死"
        );
        assert!(
            end_slope < 0.02,
            "终点斜率 {end_slope:.2}，落位那一刻还在冲"
        );
        assert_eq!(release_shape(0.0), 0.0);
        assert_eq!(release_shape(1.0), 1.0);
        for k in 1..=20 {
            let (a, b) = (
                release_shape((k - 1) as f32 / 20.0),
                release_shape(k as f32 / 20.0),
            );
            assert!(b > a, "走了 {k}/20 反而往回退：{a} → {b}");
        }
    }
}
