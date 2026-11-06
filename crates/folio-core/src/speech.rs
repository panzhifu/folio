//! 听书（合成朗读）的引擎收口：一根专职线程持有 `tts::Tts`，其余代码只跟它换
//! [`Command`] 与 [`Event`]。
//!
//! 主语是**合成朗读**，不是有声书：库里没有音频文件、没有音频时间点，一切位置都是
//! 内容坐标（字符偏移）。设计稿在本机 `plans/听书.md`（9fa3354 起从仓库移除、按
//! `.gitignore` 的口径只留本地），下面每条平台事实都读过 0.26.3 源码对过，出处随文。
//!
//! ## 为什么是一根专职线程
//!
//! `tts::Tts` 内部是 `Rc<RwLock<Box<dyn Backend>>>`（0.26.3 `lib.rs:262`），那两行
//! `unsafe impl Send/Sync` 只让类型系统放行，跨线程用会炸在 clone-on-drop 与回调表
//! 上。所以句柄只能由一根线程持有，别处通过两条管道跟它说话：
//!
//! - 命令与回调走**同一条**输入管道（[`gate::Input`]）：引擎线程一根 `recv_blocking`
//!   同时收两路，不需要 select。选 `async_channel` 而不是 `std::sync::mpsc`，是因为
//!   事件的消费端在 gpui 的 async 环境里 `recv().await`；它在依赖图上本来就有
//!   （folio-app 的直接依赖，见根 `Cargo.toml` 那条），core 只是加一条边，锁里零新包。
//! - 事件走输出管道：一条 [`Event::Ready`]，之后是 [`Event::Begin`] / [`Event::Done`]。
//!
//! ## 两平台的回调映射是反的，归一在 [`gate::reduce`]
//!
//! 0.26.3 逐条对过（`backends/android.rs:60-164`、`backends/speech_dispatcher.rs:28-60`）：
//!
//! | 事实 | Android | Linux（Speech Dispatcher） |
//! |---|---|---|
//! | 念完 | Java `onDone` → tts `utterance_stop` | `on_end` → tts `utterance_end` |
//! | 出错 | Java `onError` → tts `utterance_end` | —— |
//! | 被取消 | 永远不来（`Bridge.java` 的 `onStop` 声明的是装箱 `Boolean`，重载不是覆盖） | `on_cancel` → tts `utterance_stop` |
//!
//! 所以三条 tts 回调按平台各自归一成 [`gate::Callback`]（`imp::register_callbacks`），
//! 状态转移交给 [`gate::reduce`] 这个纯函数：**发过 [`Command::Stop`] 之后到达的一切
//! 回调都吞掉**（静音窗）。Android 的 stop 不产生任何回调、Linux 的 stop 产生
//! Cancelled、「Done 与 Stop 撞在同一瞬间」的竞态，全被这一条吃掉 —— 上层的「停」
//! 因此不需要等任何回调确认。
//!
//! ## 语速与音高：管道里只走语义值
//!
//! 配置（`infra::config::Speech`）与两条管道里流动的都是**语义值**（0.5–2.0，常速
//! 1.0）。引擎那一档的数在 [`gate::rate_to_engine`] / [`gate::pitch_to_engine`] 里
//! 换算：Android 的区间本来就是 0.1–10.0（常速 1.0），恒等；Linux Speech Dispatcher
//! 是 −100–100（常速 0），线性 `(v−1)×100`。上下限不许问引擎：`Tts` 的 `min_*` /
//! `max_*` 那一族没有 `Features` 闸门（0.26.3 `lib.rs:389-424`），而 Android 的
//! `min_volume` 是 `todo!()`（`android.rs:362`），问了就崩。
//!
//! ## 平台矩阵
//!
//! `tts` 依赖只挂在 Linux（非 ohos）与 Android 两档（见根与本 crate 的 `Cargo.toml`
//! 那条 target 门）。macOS / Windows 的构建没有这个后端：[`engine`] 恒回 `None`，
//! 界面按「这一格不存在」降级。鸿蒙同属这一档 —— 它的三元组报 linux，被
//! `not(target_env = "ohos")` 挡在门外，听书要等 NAPI / ArkTS 那层另起后端。

use std::sync::Arc;
use std::sync::atomic::{AtomicI8, Ordering};

use async_channel::{Receiver, Sender};

use self::gate::Input;

/// `Ready` 快照的三态：-1 还在探测 / 0 不可用 / 1 可用。
const READY_UNKNOWN: i8 = -1;

/// 引擎线程收到的命令。语速/音高是**语义值**，换算住在引擎线程（见模块文档）。
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// 念一段。一次一条：念完的 [`Event::Done`] 里推下一段，不做预排 —— 预排要处理
    /// 「读者翻页时队列里那几段怎么办」，一次一条把这个分支整个消掉。
    Speak {
        text: String,
    },
    /// 停。暂停与停止都是它（0.26.3 没有 pause/resume 这一格），差别只在会话层
    /// 要不要记住当前段。
    Stop,
    SetRate(f32),
    SetPitch(f32),
}

/// 引擎线程发回的事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// 引擎探测完成。`true` = 可用；`false` = 建句柄失败（Linux 没有 speechd、
    /// Android 初始化超时/失败），此后 [`engine`] 恒回 `None`（进程内缓存）。
    Ready(bool),
    /// 一段开始念（Android 的 Java `onStart` / Linux 的 `on_begin`）。
    Begin,
    /// 一段结束。念完与引擎报错在回调层不可分（Android 上恰好映射进
    /// `utterance_stop` 与 `utterance_end` 两个名字，见模块文档那张表），这里归一成
    /// 一个词 —— 会话层的推进只认它。
    Done,
}

/// 引擎句柄：命令的发送端与事件的接收端。`Clone` 便宜（两条 channel 各一份），
/// 进程内只有一个实例（[`engine`]）。
#[derive(Clone)]
pub struct Engine {
    input_tx: Sender<Input>,
    event_rx: Receiver<Event>,
    /// 探测结论的**持久快照**。`Event::Ready` 全进程只发一次，而它的消费者是「当前
    /// 那个阅读会话」—— 会话一走，那条事件就跟着没了。新会话接手时读这一格，
    /// 不必赌事件还在不在管道里（真机上赌输了就是「听书按钮有时候不显示」）。
    ready: Arc<AtomicI8>,
}

impl Engine {
    /// 引擎探测完没有。`None` = 还在探测（等 [`Event::Ready`]），`Some(ok)` = 已有结论。
    pub fn ready(&self) -> Option<bool> {
        match self.ready.load(Ordering::Relaxed) {
            1 => Some(true),
            0 => Some(false),
            _ => None,
        }
    }

    fn set_ready(&self, ok: bool) {
        self.ready.store(if ok { 1 } else { 0 }, Ordering::Relaxed);
    }

    /// 发一条命令。引擎线程已经退场（探测失败、进程收尾）时安静丢弃：调用方不需要
    /// 为此分支，可用性只看 [`Event::Ready`]。
    pub fn send(&self, command: Command) {
        let _ = self.input_tx.send_blocking(Input::Cmd(command));
    }

    /// 事件接收端。同一时刻只有一个消费者（当前的阅读会话）在 `recv().await`；
    /// 新会话接手前先把手头的事件抽干，别把上一段的尾巴当成这一段的。
    pub fn events(&self) -> Receiver<Event> {
        self.event_rx.clone()
    }
}

/// 进程级引擎句柄。第一次调用起引擎线程，探测结果经 [`Event::Ready`] **异步**到达
/// （`Tts::default()` 在引擎线程里调，调用方不被阻塞）。探测终审失败后句柄留在槽里，
/// 但下一次调用会把它整个换掉重新拉起 —— 重试留给下一次点开听书屏，Linux 上也可以
/// 先跑起 `speechd` 再点一次。
pub fn engine() -> Option<Engine> {
    imp::engine()
}

/// 引擎线程的内部协议：回调归一、两态门与一步转移。纯函数单独成节，为的是
/// **所有平台**（包括没有引擎的 macOS / Windows）都能 headless 测 —— 「停不依赖
/// 回调」那条验收门就钉在 [`gate::reduce`] 的测试上。
pub mod gate {
    use super::{Command, Event};

    /// tts 三条回调归一后的形态。归一的映射随平台走（模块文档那张反接表）：
    /// Android 的注册点把 `utterance_stop`（Java onDone）与 `utterance_end`
    /// （Java onError）都归 [`Callback::Finished`]；Linux 把 `utterance_end`
    /// 归 Finished、`utterance_stop`（on_cancel）归 Cancelled。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Callback {
        /// 一段开始念。
        Begin,
        /// 这一段不会再继续了：念完（两平台）或引擎报错（Android）。
        Finished,
        /// 被 cancel（Linux：我们的 stop 与 interrupt-speak 的前置 cancel）。
        Cancelled,
    }

    /// 引擎线程的两态。`Idle` 兼任静音窗。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Gate {
        Idle,
        Speaking,
    }

    /// 输入管道里的一步：界面来的命令，或后端回调归一后的形态。两条来源合走一条
    /// 管道，引擎线程才不需要 select。
    #[derive(Debug, Clone, PartialEq)]
    pub enum Input {
        Cmd(Command),
        Cb(Callback),
    }

    /// 引擎线程要对 tts 做的动作。语速/音高在这里仍是语义值，换算发生在真正
    /// 调用 tts 的那一刻（`imp::apply`）。
    #[derive(Debug, Clone, PartialEq)]
    pub enum Action {
        Speak(String),
        Stop,
        Rate(f32),
        Pitch(f32),
    }

    /// 一步转移：`(状态, 输入) → (状态, 对 tts 的动作, 上抛的事件)`。
    ///
    /// 两条不变量，各有测试钉着：
    /// 1. **发过 [`Command::Stop`] 之后再到达的一切回调都被吞** —— Android 的 stop
    ///    本就不产生回调、Linux 的 stop 产生 [`Callback::Cancelled`]、还有 Done 与
    ///    Stop 撞车的竞态。上层的「停」因此不等任何回调确认。
    /// 2. **`Done` 只在 `Speaking` 中由 [`Callback::Finished`] 产生** —— Linux 的
    ///    interrupt-speak 会先 cancel 旧段，那条 Cancelled 在新段的 `Speaking` 里
    ///    到达，吞掉它，新段才不会被当成「已经念完」。
    pub fn reduce(gate: Gate, input: Input) -> (Gate, Option<Action>, Option<Event>) {
        match (gate, input) {
            (_, Input::Cmd(Command::Stop)) => (Gate::Idle, Some(Action::Stop), None),
            (_, Input::Cmd(Command::Speak { text })) => {
                (Gate::Speaking, Some(Action::Speak(text)), None)
            }
            (gate, Input::Cmd(Command::SetRate(value))) => (gate, Some(Action::Rate(value)), None),
            (gate, Input::Cmd(Command::SetPitch(value))) => {
                (gate, Some(Action::Pitch(value)), None)
            }
            (Gate::Speaking, Input::Cb(Callback::Begin)) => {
                (Gate::Speaking, None, Some(Event::Begin))
            }
            (Gate::Speaking, Input::Cb(Callback::Finished)) => {
                (Gate::Idle, None, Some(Event::Done))
            }
            // 静音窗的两个臂：Idle 下什么回调都吞；Speaking 下吞 Cancelled ——
            // 那条取消属于已经被顶掉或刚刚停掉的那一段，不是新段的结束。
            (Gate::Idle, _) => (Gate::Idle, None, None),
            (Gate::Speaking, Input::Cb(Callback::Cancelled)) => (Gate::Speaking, None, None),
        }
    }

    /// 语义语速（0.5–2.0，常速 1.0）→ 引擎那一档。三臂互斥，每档平台只编一支。
    #[cfg(target_os = "android")]
    pub fn rate_to_engine(value: f32) -> f32 {
        value
    }
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    pub fn rate_to_engine(value: f32) -> f32 {
        (value - 1.0) * 100.0
    }
    #[cfg(not(any(
        target_os = "android",
        all(target_os = "linux", not(target_env = "ohos"))
    )))]
    pub fn rate_to_engine(value: f32) -> f32 {
        value
    }

    /// 语义音高 → 引擎那一档。Android 的 pitch 区间是 0.1–2.0（常速 1.0），语义档
    /// 0.5–2.0 整段落在其中，恒等；Linux 与语速同一套 −100–100。
    #[cfg(target_os = "android")]
    pub fn pitch_to_engine(value: f32) -> f32 {
        value
    }
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    pub fn pitch_to_engine(value: f32) -> f32 {
        (value - 1.0) * 100.0
    }
    #[cfg(not(any(
        target_os = "android",
        all(target_os = "linux", not(target_env = "ohos"))
    )))]
    pub fn pitch_to_engine(value: f32) -> f32 {
        value
    }
}

/// 真引擎档：判据与本 crate `Cargo.toml` 那条 `tts` 依赖门**同一个**，两边要同进同退。
#[cfg(any(
    all(target_os = "linux", not(target_env = "ohos")),
    target_os = "android"
))]
mod imp {
    use std::sync::atomic::AtomicI8;
    use std::sync::{Arc, Mutex, MutexGuard};
    use std::time::Duration;

    use async_channel::{Receiver, Sender};
    use tts::Tts;

    use super::gate::{self, Action, Callback, Gate};
    use super::{Engine, Event, Input, READY_UNKNOWN};

    /// 进程里那份引擎句柄。`None` = 还没起过；`Some(false)` 结论的句柄 = 它的线程已经
    /// 退出（[`run`] 里 `set_ready(false)` 之后必然 `return`），下一次 [`engine`] 会把
    /// 它整个换掉 —— 探测失败不再是进程级终审，「点了听书钮」就是重试。
    static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);

    /// 引擎探测的总次数与两次之间的间隔。Google TTS 冷启动 1–3 s 就绪（真机量过
    /// 服务端建连到回调的间隔），4 次 × 每次窗口 0.5 s + 间隔 0.9 s ≈ 4.7 s 到终审。
    const PROBE_ATTEMPTS: u32 = 4;
    const PROBE_RETRY_GAP: Duration = Duration::from_millis(900);

    pub(super) fn engine() -> Option<Engine> {
        let mut slot: MutexGuard<Option<Engine>> = ENGINE.lock().ok()?;
        match &*slot {
            // 探测在途（`ready == None`）或可用（`Some(true)`）：复用，不双开线程。
            Some(engine) if engine.ready() != Some(false) => Some(engine.clone()),
            // 没起过，或上一次探测终审失败（旧线程已退）：重新拉起。
            _ => {
                let (input_tx, input_rx) = async_channel::unbounded::<Input>();
                let (event_tx, event_rx) = async_channel::unbounded::<Event>();
                let engine = Engine {
                    input_tx: input_tx.clone(),
                    event_rx,
                    ready: Arc::new(AtomicI8::new(READY_UNKNOWN)),
                };
                let callback_tx = input_tx;
                let run_engine = engine.clone();
                let _ = std::thread::Builder::new()
                    .name("folio-speech".into())
                    .spawn(move || run(callback_tx, input_rx, event_tx, run_engine));
                *slot = Some(engine.clone());
                Some(engine)
            }
        }
    }

    /// 引擎线程的主循环：建句柄、挂回调、收输入走 `reduce`。
    ///
    /// `Tts::default()` 必须在这一根线程里调：Android 的 `Android::new()` 阻塞至多
    /// 500 ms 并 `attach_current_thread_permanently`（0.26.3 `android.rs:185-220`），
    /// 放到界面线程就是开书卡半秒、外加一根永不归还的线程。
    fn run(
        callback_tx: Sender<Input>,
        input_rx: Receiver<Input>,
        event_tx: Sender<Event>,
        engine: Engine,
    ) {
        // tts 0.26.3 给初始化的窗口只有 **500 ms**（`android.rs:206` 的
        // `MAX_WAIT_TIME`）。Google TTS 的服务进程被系统收走之后，下一次连接是
        // 冷启动 —— 绑定成功、初始化回调却赶不上这半秒（真机 2026-11-07 实测：
        // bound → 476 ms 后报 `Operation failed`，服务日志同刻有 DeadObjectException）。
        // 这只是**这一次**没赶上，不是这台设备没有引擎，所以在这里隔一小段重试几遍。
        // 真没有引擎的设备（Linux 没有 speechd、探测秒失败）多花的只是几次立刻返回
        // 的空试；4 次全败按终审处理，圆钮仍在，下一次点开听书屏再来（见 [`engine`]）。
        let mut tts = None;
        for attempt in 1..=PROBE_ATTEMPTS {
            match Tts::default() {
                Ok(ready) => {
                    tts = Some(ready);
                    break;
                }
                Err(err) => {
                    tracing::warn!("听书引擎探测第 {attempt}/{PROBE_ATTEMPTS} 次失败：{err}");
                    if attempt < PROBE_ATTEMPTS {
                        std::thread::sleep(PROBE_RETRY_GAP);
                    }
                }
            }
        }
        let Some(mut tts) = tts else {
            tracing::warn!("听书引擎不可用：{PROBE_ATTEMPTS} 次探测全部失败");
            engine.set_ready(false);
            let _ = event_tx.send_blocking(Event::Ready(false));
            return;
        };
        if let Err(err) = register_callbacks(&tts, callback_tx) {
            tracing::warn!("听书回调挂不上（{err}），引擎按不可用处理");
            engine.set_ready(false);
            let _ = event_tx.send_blocking(Event::Ready(false));
            return;
        }
        engine.set_ready(true);
        tracing::info!("听书引擎可用");
        let _ = event_tx.send_blocking(Event::Ready(true));

        let mut gate = Gate::Idle;
        while let Ok(input) = input_rx.recv_blocking() {
            let (next, action, event) = gate::reduce(gate, input);
            gate = next;
            if let Some(action) = action {
                apply(&mut tts, action);
            }
            if let Some(event) = event {
                // 一句一条的低频事件，info 档常开：真机上「念完不推进」这类问题，
                // logcat 里 Done 来没来一眼可辨。
                tracing::info!(?event, "听书引擎事件");
                let _ = event_tx.send_blocking(event);
            }
        }
    }

    /// 三条回调按平台归一（模块文档那张反接表）。闭包在后端自己的线程上被调
    /// （Android 的 JNI 线程、Linux 的 speechd 事件线程），只做一件事：塞进输入
    /// 管道 —— 状态转移全部在引擎线程的 `reduce` 里，回调里不碰任何共享状态。
    fn register_callbacks(tts: &Tts, tx: Sender<Input>) -> Result<(), tts::Error> {
        tts.on_utterance_begin(Some(Box::new({
            let tx = tx.clone();
            move |_| {
                let _ = tx.send_blocking(Input::Cb(Callback::Begin));
            }
        })))?;
        #[cfg(target_os = "android")]
        {
            // 念完（Java onDone → utterance_stop）与出错（Java onError →
            // utterance_end）在回调层不可分，都归 Finished：会话层的推进只认
            // 「这一段不会再继续了」。Java onStop 那条因为装箱 Boolean 的重载缺陷
            // 永远不来，不用为它留位置。
            tts.on_utterance_stop(Some(Box::new({
                let tx = tx.clone();
                move |_| {
                    let _ = tx.send_blocking(Input::Cb(Callback::Finished));
                }
            })))?;
            tts.on_utterance_end(Some(Box::new(move |_| {
                let _ = tx.send_blocking(Input::Cb(Callback::Finished));
            })))?;
        }
        #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
        {
            tts.on_utterance_end(Some(Box::new({
                let tx = tx.clone();
                move |_| {
                    let _ = tx.send_blocking(Input::Cb(Callback::Finished));
                }
            })))?;
            tts.on_utterance_stop(Some(Box::new(move |_| {
                let _ = tx.send_blocking(Input::Cb(Callback::Cancelled));
            })))?;
        }
        Ok(())
    }

    fn apply(tts: &mut Tts, action: Action) {
        let result = match action {
            Action::Speak(text) => {
                tracing::info!(chars = text.chars().count(), "听书开口");
                tts.speak(text, true).map(|_| ())
            }
            Action::Stop => tts.stop().map(|_| ()),
            // set_rate / set_pitch 成功时回 `Result<&Tts>`（链式调用的形状），收成 ()。
            Action::Rate(value) => tts.set_rate(gate::rate_to_engine(value)).map(|_| ()),
            Action::Pitch(value) => tts.set_pitch(gate::pitch_to_engine(value)).map(|_| ()),
        };
        if let Err(err) = result {
            tracing::warn!("听书引擎调用失败：{err}");
        }
    }
}

/// macOS / Windows（与鸿蒙）没有 `tts` 后端，依赖门见 `Cargo.toml`：不存在引擎，
/// [`engine`] 恒回 `None`，界面按「这一格不存在」降级。
#[cfg(not(any(
    all(target_os = "linux", not(target_env = "ohos")),
    target_os = "android"
)))]
mod imp {
    use super::Engine;

    pub(super) fn engine() -> Option<Engine> {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicI8;

    use crate::speech::gate::{Action, Callback, Gate, pitch_to_engine, rate_to_engine, reduce};
    use crate::speech::{Command, Engine, Event, Input, READY_UNKNOWN};

    /// 一条不带真引擎的句柄，只为验「结论快照」这一格。
    fn engine_with_ready(value: i8) -> Engine {
        let (input_tx, _input_rx) = async_channel::unbounded::<Input>();
        let (_event_tx, event_rx) = async_channel::unbounded::<Event>();
        Engine {
            input_tx,
            event_rx,
            ready: Arc::new(AtomicI8::new(value)),
        }
    }

    /// 结论是**持久**的：新会话接手时读它，不必赌那条只发一次的 `Ready` 还在不在
    /// 事件管道里（赌输了就是真机上「听书按钮有时候不显示」）。
    #[test]
    fn the_ready_snapshot_outlives_the_single_event() {
        assert_eq!(engine_with_ready(READY_UNKNOWN).ready(), None, "还在探测");
        assert_eq!(engine_with_ready(0).ready(), Some(false), "探测失败");
        assert_eq!(engine_with_ready(1).ready(), Some(true), "探测成功");
        let engine = engine_with_ready(READY_UNKNOWN);
        engine.set_ready(true);
        assert_eq!(engine.clone().ready(), Some(true), "克隆也读同一份结论");
        engine.set_ready(false);
        assert_eq!(engine.ready(), Some(false), "结论只写一次，之后不变");
    }

    fn speak(text: &str) -> Input {
        Input::Cmd(Command::Speak {
            text: text.to_string(),
        })
    }

    fn cb(callback: Callback) -> Input {
        Input::Cb(callback)
    }

    /// 停不依赖回调（验收门）：`Stop` 之后撞到的 `Done` 必须被静音窗吞掉 ——
    /// 段落念完与读者按下停止撞在同一瞬间是真实会发生的竞态，漏吞一次，
    /// 会话层就多推进一段。
    #[test]
    fn everything_after_stop_is_swallowed() {
        let (gate, action, event) = reduce(Gate::Speaking, super::gate::Input::Cmd(Command::Stop));
        assert_eq!((gate, event), (Gate::Idle, None));
        assert_eq!(action, Some(Action::Stop));

        for input in [
            cb(Callback::Finished),
            cb(Callback::Begin),
            cb(Callback::Cancelled),
        ] {
            let (gate, action, event) = reduce(Gate::Idle, input);
            assert_eq!(gate, Gate::Idle);
            assert_eq!(action, None);
            assert_eq!(event, None, "Stop 之后不允许再上抛任何事件");
        }
    }

    /// `Done` 的唯一出口：Speaking 中收到 Finished。 Idle 下同样的回调必须无声。
    #[test]
    fn finished_emits_done_only_while_speaking() {
        let (gate, action, event) = reduce(Gate::Speaking, cb(Callback::Finished));
        assert_eq!((gate, action, event), (Gate::Idle, None, Some(Event::Done)));

        let (_, _, event) = reduce(Gate::Idle, cb(Callback::Finished));
        assert_eq!(event, None);
    }

    /// Linux 的 interrupt-speak 会先 cancel 旧段：那条 Cancelled 属于被顶掉的一段，
    /// 在新段的 Speaking 里到达，吞掉它 —— 不然新段刚开口就被当成「念完了」。
    #[test]
    fn cancel_during_the_new_utterance_is_swallowed() {
        let (gate, action, event) = reduce(Gate::Speaking, speak("下一段"));
        assert_eq!(gate, Gate::Speaking);
        assert!(
            matches!(action, Some(Action::Speak(text)) if text == "下一段"),
            " Speak 的动作要把文本带给引擎"
        );
        assert_eq!(event, None);

        let (gate, action, event) = reduce(Gate::Speaking, cb(Callback::Cancelled));
        assert_eq!((gate, action, event), (Gate::Speaking, None, None));
    }

    /// 暂停 → 恢复：Stop 与重新 Speak 之间迟到的 Cancelled（Linux）在 Idle 里吞掉，
    /// 重新开口之后再走 Begin → Finished 的整条链。
    #[test]
    fn pause_then_resume_runs_a_full_cycle_again() {
        let (gate, _, _) = reduce(Gate::Speaking, super::gate::Input::Cmd(Command::Stop));
        let (gate, _, _) = reduce(gate, cb(Callback::Cancelled));

        let (gate, action, _) = reduce(gate, speak("同一段从头"));
        assert_eq!(gate, Gate::Speaking);
        assert!(matches!(action, Some(Action::Speak(_))));

        let (gate, _, event) = reduce(gate, cb(Callback::Begin));
        assert_eq!(event, Some(Event::Begin));
        let (gate, _, event) = reduce(gate, cb(Callback::Finished));
        assert_eq!((gate, event), (Gate::Idle, Some(Event::Done)));
    }

    /// 换算：常速落在本引擎的常速档上，区间端点落在引擎 min/max 之内，且保序
    /// （`Tts::set_rate` 对出界的值回 OutOfRange，0.26.3 `lib.rs:421`）。
    #[test]
    fn conversions_keep_the_normal_point_and_stay_in_range() {
        #[cfg(target_os = "android")]
        {
            assert_eq!(rate_to_engine(1.0), 1.0);
            assert_eq!(pitch_to_engine(1.0), 1.0);
            assert_eq!(rate_to_engine(0.5), 0.5);
            assert_eq!(rate_to_engine(2.0), 2.0);
            assert!(pitch_to_engine(2.0) <= 2.0);
        }
        #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
        {
            assert_eq!(rate_to_engine(1.0), 0.0);
            assert_eq!(pitch_to_engine(1.0), 0.0);
            assert_eq!(rate_to_engine(0.5), -50.0);
            assert_eq!(rate_to_engine(2.0), 100.0);
            assert!(rate_to_engine(0.5) >= -100.0 && rate_to_engine(2.0) <= 100.0);
        }
        #[cfg(not(any(
            target_os = "android",
            all(target_os = "linux", not(target_env = "ohos"))
        )))]
        {
            assert_eq!(rate_to_engine(1.0), 1.0);
            assert_eq!(pitch_to_engine(1.0), 1.0);
        }
    }
}
