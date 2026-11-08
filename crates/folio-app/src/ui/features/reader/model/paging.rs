//! 导航：摆法（连续 / 分页 / 双页）、翻一屏、跳到某个比例、以及把滚动增量落到正文。
//!
//! 这些是**纯状态迁移**：给定当前模式与视口，动 `scroll_y` / `page` 并顺手落库。
//! 位置本身存在 [`crate::ui::features::reader::model::session::ReaderSession`] 上，方法的可见性靠 `pub(in crate::ui::features::reader)`。

use folio_core::{Locator, PageTurn};
use gpui_kit::{App, Context, ScrollDelta, ScrollEvent};

use crate::ui::features::reader::model::chrome::{Arrow, AutoReadMode, PageMode};
use crate::ui::features::reader::model::session::{
    PageRef, ReaderSession, Source, Turn, TurnRun, start_turn,
};
use crate::ui::features::reader::model::swipe::SwipeStep;
use crate::ui::features::reader::model::turn::{PagingFacts, TurnIntent, TurnPlan};
use crate::ui::services::config::ConfigHolder;
use crate::ui::shell::turn_duration;

/// 自动前进那一帧的在途状态：上一帧的时刻、分页档攒下的间隔债、滚动档攒下的
/// 不足 1px 的余数。
///
/// 推进由**渲染帧**驱动（`view::body::render`），不再有 16ms 后台定时器：那条
/// 定时器与 vsync 的 16.67ms 互相拍频，有的帧收到 0 次推进、有的收到 2 次，
/// 读起来就是「自动滚动在抖」。帧驱动保证每画一帧恰好推一次。
pub(in crate::ui::features::reader) struct AutoState {
    last: std::time::Instant,
    debt: f32,
    /// 滚动档不足 1 逻辑像素的那点位移：攒到整 1px 才落到 `scroll_y`。
    /// 慢速档（2 页/分 ≈ 每帧 0.3px）才不会被 0.5px 死区逐帧吞掉、
    /// 攒几帧突然跳一下。
    remainder: f32,
    /// 滚动档这一趟的时间轴起点：推进量按 `speed × (now − origin) + origin_scroll` 算，
    /// 而不是逐帧累加 dt。
    ///
    /// 逐帧累加的问题在**抖动**：`now` 来自 `Instant::now()`，帧间隔本身在
    /// 16.0 / 16.8 / 17.2 ms 之间跳，累加出来的位移跟着一帧快一帧慢。时间轴每帧只
    /// 重新折一次「从起点到现在走了多远」，帧间隔跳多少都不影响这一帧该在的位置 ——
    /// 掉帧时它自己追上，停顿一段再回来也不会多推。
    origin: std::time::Instant,
    /// 时间轴起点对应的那个 `scroll_y`。读者手动滚动、或坐标链补偿挪了位置时，
    /// 这一格跟着重设 —— 否则「读者拖了 100px」会被下一帧的时间轴抹掉。
    origin_scroll: f32,
    /// 时间轴是否已经起表。`false` = 刚开自动 / 刚切到滚动档 / 刚被挪过，下一帧
    /// 才把起点与基准一起记下。
    timed: bool,
    /// 视口高（逻辑像素），折「一页 = 一屏」用。会话自己不存屏高 —— `mark_scroll`
    /// 每帧从视图递进来，开着自动时顺手喂一份；默认 600 是第一次推进前的兜底。
    pub(in crate::ui::features::reader) viewport: f32,
}

impl Default for AutoState {
    fn default() -> Self {
        Self {
            last: std::time::Instant::now(),
            debt: 0.0,
            remainder: 0.0,
            origin: std::time::Instant::now(),
            origin_scroll: 0.0,
            timed: false,
            viewport: 600.0,
        }
    }
}

impl ReaderSession {
    /// 换成另一种摆法。位置**不丢**：进度存的是「第几个字」，所以先把当前字偏移取下来，
    /// 等下一帧按它在新模式里落位。落库的模式名跟着走。
    pub fn set_mode(&mut self, mode: PageMode, cx: &mut Context<Self>) {
        self.change_mode(mode, true, cx);
    }

    /// 渲染帧按窗口宽或「翻页方式」解出来的摆法。位置迁移与 [`Self::set_mode`] 同一条，
    /// 但**不**替读者落「已经选过档」那面旗：跟着宽度变不是一次选择，落下去之后
    /// 窗口再变宽就再也长不回双页了。
    ///
    /// 摆法换档必须走这里，不能在渲染帧里直接赋 `self.mode`：连续那一档的位置是全局
    /// `scroll_y`，分页那一档是页号，两者之间要经过 `resolve_position` + `pend_restore`
    /// 才换算得过去。直接赋值会让「选了上下滚动再选回仿真翻页」在章首重新开始。
    pub(in crate::ui::features::reader) fn adopt_mode(
        &mut self,
        mode: PageMode,
        cx: &mut Context<Self>,
    ) {
        self.change_mode(mode, false, cx);
    }

    fn change_mode(&mut self, mode: PageMode, chosen: bool, cx: &mut Context<Self>) {
        if mode == self.mode {
            return;
        }
        self.mode = mode;
        // 摆法换档也是换参照系：定版式整页翻的平移与上下滚动那档的页宽不是一回事，
        // 带着上一档的倍率过去会读成「一进这一档就歪着」。
        self.reset_fixed_zoom();
        // 单页/双页的捕获纹理不是同一块：一页一栏 vs 一页两栏。不重烘就按旧布局取 uv，
        // 双页的右半张会采到单页那张纹理的空白/邻区。摆法一换就作废。
        self.capture_dirty = true;
        // 读者亲手选的这一档要落下那面旗，之后落库就写他选的；跟着宽度 / 翻页方式
        // 解出来的那一档不算选择（判据在 [`Self::adopt_mode`]）。
        if chosen {
            self.mode_undecided = false;
        }
        // 摆法换了，同一句话会挪到另一处坐标位置；选中跟着字走虽不至于指错，
        // 但「换摆法」在读者心里是一次新动作，留着一个旧高亮只会碍事。
        self.clear_selection();
        // 连续模式的落点是**全局 y**（章基线 + 章内行顶），基线未必是 0 ——
        // 上一章可能正拼在上方，所以哪怕偏移是 0 也挂恢复请求，让渲染帧把它
        // 摆到正确的那一像素。分页落到页号，偏移 0 就是第 0 页，不需要请求。
        //
        // 已经挂着一份恢复请求（刚换书 / 刚跳目录）时不许用当前屏幕上的坐标覆盖它：
        // 那份请求是**目标**，而此刻的 `scroll_y` / `page` 还停在旧摆法的起点或尚未
        // 落位。进书第一帧恰好两件事撞在一起 —— 排版还没回来，`resolve_position`
        // 读出来是章首，覆盖掉恢复请求就会把重启后的位置从第 1250 个字丢回 0。
        if self.pending_restore.is_none() {
            let (chapter, offset) = self.resolve_position().unwrap_or((self.chapter_index(), 0));
            self.pend_restore((mode.is_continuous() || offset > 0).then_some(Locator {
                chapter: chapter as u32,
                offset,
            }));
        }
        self.scroll_y = 0.0;
        self.page = 0;
        self.wheel_accum = 0.0;
        // 摆法换了，动画里那张出场页属于旧摆法（旧片高、旧栏数），让它继续平移只会
        // 把两张对不上的纸叠在屏幕上。
        self.cancel_turn();
        cx.notify();
    }

    /// 按「一屏」翻正文。这一趟**会**播动画（分页沿当前轴平移两张纸，连续滚动补间
    /// 画出来的那个 y），因为这一句的来路都是「一次离散动作」：点屏幕左右两边、音量键、
    /// 鼠标滚轮一格。手指触摸拖动走 [`ReaderSession::on_scroll`] 的跟手那一路，那是手在
    /// 直接控制纸，不该有东西追在手指后面补间。
    ///
    /// 连续滚动走 90% 而不是 100%：整屏跳会让上一屏的末行彻底消失，读者找不到接续的那句。
    /// 分页 / 双页是一页一号，跳半页在这里没有意义。
    /// 方向键翻页（PC）。返回 `false` = 这一键不归正文，调用方该放手。
    ///
    /// 左右两颗按整本的翻页方向换手（判据在 [`Arrow::turn`]，与点屏幕左右分区同一个
    /// 口径），上下不换。文字输入框到不了这里：焦点是文字输入时 gpui **根本不派发键绑定**
    /// （`InputPreference::CharacterInput`，见 `Window::dispatch_key_event`）。
    ///
    /// 图片查看器开着时这一层让开：那一层自己的 `on_key_down` 比键绑定后跑，这里不放手
    /// 就轮不到它 —— 而「左右 = 上一张 / 下一张」正是那一层的功能。
    pub fn arrow_turn(&mut self, arrow: Arrow, cx: &mut Context<Self>) -> bool {
        if self
            .image_viewer
            .as_ref()
            .is_some_and(|viewer| viewer.read(cx).is_open())
        {
            return false;
        }
        // 浮层（顶栏底栏 / 两块弹层 / 脚注）压着的时候这一键也不翻正文：与 `claim_swipe`、
        // 滚轮那两条同一规矩 —— 工具栏露头之后正文上的输入只表达「收浮层」。放手给焦点元素，
        // 底栏那条进度滑条与弹层里的列表正该接这一键。
        if self.overlay_open {
            return false;
        }
        let delta = arrow.turn(self.progression_rtl());
        if let Some(intent) = TurnIntent::from_delta(delta) {
            self.turn(intent, cx);
        }
        true
    }

    /// 兼容旧入口：方向键/点击分区/音量键最终都转成 [`TurnIntent`] 再走 [`Self::turn`]。
    /// 保留这个按 `isize` 取方向的口子，是因为宿主测试与页面事件都按「±1 屏」说话。
    pub fn scroll_page(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(intent) = TurnIntent::from_delta(delta) {
            self.turn(intent, cx);
        }
    }

    /// 自动前进开着没有。
    pub(in crate::ui::features::reader) fn auto_on(&self) -> bool {
        self.auto.is_some()
    }

    /// 这一趟的推进方式（自动工具栏那两格读它来画选中态）。
    pub(in crate::ui::features::reader) fn auto_mode(&self) -> AutoReadMode {
        self.auto_mode
    }

    /// 读者动了（或听书起念）：自动前进停表。见 [`Self::set_auto`] 那条注释里的清单。
    ///
    /// 只清 `auto`，不碰 `auto_saved_layout`：还原摆法交给下一帧渲染帧统一做，因为停表
    /// 的路不止这一条（读者自己划一下、自动走到头都直接清 `auto`），挂在任何单一入口
    /// 上都会漏。
    pub(in crate::ui::features::reader) fn stop_auto(&mut self, cx: &mut Context<Self>) {
        if self.auto.take().is_some() {
            cx.notify();
        }
    }

    /// 自动前进 on/off。开的那一下起一条拍子循环（[`AUTO_TICK`] 一拍），关的那一下
    /// 只清状态 —— 循环下一拍看到 `None` 自己退场。
    ///
    /// 开的时候多两件事：记下读者原本的摆法（退出时还回去），并按这一趟的
    /// [`AutoReadMode`] 把正文切到它要的参照系。两档的推进算法不同，摆法不同，
    /// 彼此不共用一套坐标。
    ///
    /// 谁会关它：读者自己（底栏那一格再按一下）、或任何一次手动的翻页与滚动
    /// （[`Self::turn`] / [`Self::on_scroll`] 入口的那条闸）、听书起念（`ReadingPage`
    /// 那一头递的）—— 两个「推位置」的东西同时在跑，读者分不清是谁在翻。
    pub(in crate::ui::features::reader) fn set_auto(&mut self, on: bool, cx: &mut Context<Self>) {
        if on == self.auto_on() {
            return;
        }
        if on {
            self.auto_saved_layout = Some((self.mode, self.mode_undecided));
            self.auto = Some(AutoState::default());
            // 换摆法走 adopt_mode：它把「第几个字」从旧坐标搬到新坐标，直接赋 self.mode
            // 会让读者从这一章的章首重新开始。
            let target = self.auto_mode.layout(self.mode);
            self.adopt_mode(target, cx);
        } else {
            self.auto = None;
        }
        // 第一帧由这一下 notify 唤起，往后每一步都由渲染帧自己续
        // （`view::body::render` 里的 `request_animation_frame`）；起一条独立的
        // 后台定时器只会多一把与 vsync 不同步的钟。
        cx.notify();
    }

    /// 自动工具栏那两格：换这一趟的推进方式。跑着的时候切，摆法跟着换、拍子重新起算 ——
    /// 翻页档攒下的间隔债不能带到滚动档去，反过来也一样。
    ///
    /// `auto_saved_layout` 在这里**不动**：它记的是开自动之前的摆法，不是上一档模式的。
    pub(in crate::ui::features::reader) fn set_auto_read_mode(
        &mut self,
        mode: AutoReadMode,
        cx: &mut Context<Self>,
    ) {
        if mode == self.auto_mode {
            return;
        }
        self.auto_mode = mode;
        if self.auto.is_some() {
            self.auto = Some(AutoState::default());
            let target = mode.layout(self.mode);
            self.adopt_mode(target, cx);
        }
        cx.notify();
    }

    /// 一帧的决策：按这一趟的 [`AutoReadMode`] 走，不看书的摆法 ——
    /// 滚动档按这一帧的真实 dt 推进像素（不足 1px 的攒着），翻页档攒间隔债、
    /// 攒够一次翻一张。摆法在 `set_auto` / `set_auto_read_mode` 里已经切到对应
    /// 参照系，这里两条腿的坐标才都成立。
    ///
    /// 由 `view::body::render` **每帧调一次**：推进与绘制严格 1:1，也不再有第二条
    /// 时钟同时写 `scroll_y`。上一张还在走（翻页动画在途）的那一帧跳过 —— 不打断
    /// 读者看得见的那一趟。时刻显式递进来，测试才能按自己的假钟一步步推。
    pub(in crate::ui::features::reader) fn auto_step_at(
        &mut self,
        now: std::time::Instant,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.auto.as_mut() else {
            return;
        };
        let dt = now.duration_since(state.last).as_secs_f32();
        state.last = now;
        // 视口高与 `scroll_y` 先抄出来：下面 `paging_facts()` / `auto_scroll_by()`
        // 都要 `self`，而 `state` 那一笔可变借用还活着。
        let scroll_y = self.scroll_y;
        if self.turn.is_some() {
            return;
        }
        let speed = ConfigHolder::appearance(cx).auto_advance_speed;
        if self.auto_mode == AutoReadMode::Scroll {
            // 起点报进来等于**换了基准**：读者刚拖过、或这一帧还停在开自动那一格。
            // 起表就是「这一帧不动，从现在开始算」—— 时间轴从这一格起，位移从这一格计。
            let state = self.auto.as_mut().expect("上面刚判过 Some");
            if !state.timed {
                state.timed = true;
                state.origin = now;
                state.origin_scroll = scroll_y;
                state.remainder = 0.0;
            }
            // 已经到底就停表：再推也没有新字，留着「开着」只会让按钮一直亮、屏却不动。
            // 两道「还没就绪」的闸：排版结果没回来（`ready` 为假）、或刚切到滚动档、
            // `laid_out` 还停在上一档分页（`max_scroll` 是旧坐标里的 0）—— 都不算到底。
            // 少了后一条，切档那一帧会被误判成「滚到底」当场停表。
            let facts = self.paging_facts();
            if facts.ready
                && self.laid_out.mode.is_continuous()
                && facts.max_scroll - scroll_y <= 0.5
            {
                self.auto = None;
                cx.notify();
                return;
            }
            // 时间轴：这一帧该在哪 = 起点 + 速度 × 从起点到现在的时长。**不累加 dt。**
            // 帧间隔跳多少（16.0 / 16.8 / 17.2ms）都不改变这一帧落在哪儿，掉一帧时
            // 下一帧自己补上；这是「自动滚动有种抖动」的正解 —— 逐帧累加时，位移的
            // 二阶差就是帧间隔的二阶差，读起来一快一慢。
            //
            // 小时长那一小段（不到 1px）直接丢掉，**不留余数**：余数若带着时间轴一起
            // 攒，攒够 1px 那一帧就会「一次跳两像素」；丢掉的那点由下一帧的时间轴自己
            // 补回来（`elapsed` 是从起点算的，不是从上一帧算的）。
            let state = self.auto.as_mut().expect("上面刚判过 Some");
            let viewport = state.viewport.max(1.0);
            let px_per_sec = speed * viewport / 60.0;
            let elapsed = now.duration_since(state.origin).as_secs_f32();
            let ahead = px_per_sec * elapsed;
            let step = ahead.trunc();
            let _ = state.remainder;
            if step >= 1.0 {
                // 时间轴挪到「已走到的那个整像素」那一刻：`origin_scroll` 跟着整像素走，
                // `origin` 挪到「本该走到这一格」的时刻（`origin + step / px_per_sec`），
                // 不是 `now` —— 用 `now` 会把这一帧剩下的小数时间也丢掉，慢速档逐帧丢
                // 一点，读起来就是「一顿一顿」。
                if px_per_sec > 0.0 {
                    state.origin += std::time::Duration::from_secs_f32(step / px_per_sec);
                } else {
                    state.origin = now;
                }
                state.origin_scroll += step;
                self.auto_scroll_by(step);
            }
        } else {
            let state = self.auto.as_mut().expect("上面刚判过 Some");
            state.debt += dt;
            let interval = 60.0 / speed;
            if state.debt >= interval {
                state.debt = 0.0;
                if let Some(intent) = TurnIntent::from_delta(1) {
                    self.auto_driving = true;
                    self.turn(intent, cx);
                    self.auto_driving = false;
                }
            }
        }
    }

    /// 一次离散翻页的唯一入口。
    ///
    /// 决策在 [`crate::ui::features::reader::model::turn`] 里（连续 / 分页 / 双页 + 章界），这里只负责把
    /// `TurnPlan` 落成状态与动画。方向键、点击分区、音量键、**鼠标滚轮一格**都从这里过 ——
    /// 四种输入答的是同一个问题（「往哪边走一步」），所以只该有一份答案：谁绕过它自己写
    /// 页号，谁就跟其余三个不一致（动画、章界、双页步进、缩放复位都会分叉）。
    /// 触摸板的像素增量不走这一句，理由写在 [`Self::on_scroll`] 的分页那一段。
    pub(in crate::ui::features::reader) fn turn(
        &mut self,
        intent: TurnIntent,
        cx: &mut Context<Self>,
    ) {
        // 读者自己动的（拍分区、按键、滚轮一格、甩屏）：自动前进停表。自动那一拍
        // 自己发起的翻页带着 `auto_driving`，不会被这一下误杀。
        if !self.auto_driving {
            self.auto = None;
        }
        // 章界已在等排版或正在滑：再按也只有一个去向，重开一趟只会把动画打断。
        if self.turn_in_flight() {
            return;
        }
        let facts = self.paging_facts();
        let plan = self.laid_out.mode.paging().plan(&facts, intent);
        // 定版式翻页回到铺满：读者按的是「看下一页」，不是「在新的一页上接着看这个角」。
        // 连续滚动那一档（`Scroll`）不在此列 —— 那里挪的是正文自己，缩放该留着。
        //
        // 必须排在那道捕获门**之前**：放大着的那几帧不烘纹理（`view::body` 的
        // `is_zoomed` 闸），若先被门拦下并排队，读者停在放大档里那一趟就永远补不上。
        if self.is_fixed() && matches!(plan, TurnPlan::Page { .. } | TurnPlan::Chapter { .. }) {
            self.reset_fixed_zoom();
        }
        // 旧页纹理没烘好（进书横移还没停、尺寸/偏好刚变）就等一下：折页那张网格按整页采
        // 纹理，拿半张纹理去卷，右侧会直接露出捕获矩形外的黑边。下一帧烘完自然会重试。
        // 只挡真正要上纸的 Page/Chapter；连续滚动不读这张纹理，不能一起挡住。
        // 定版式也要过这道门：它的页图同样是先烘一张整页纹理，卷曲才采得到。
        let needs_curl_capture = crate::ui::features::reader::model::chrome::effective_turn(
            &ConfigHolder::appearance(cx),
        ) == PageTurn::Curl
            && matches!(plan, TurnPlan::Page { .. } | TurnPlan::Chapter { .. });
        if needs_curl_capture && !self.curl_capture_ready() {
            // 不丢这一下：记下方向，等门开了在渲染帧末补一趟。从前这里直接 return，
            // 读者读到的是「进书后第一划没反应」。
            self.queue_pending_turn(intent, cx);
            return;
        }
        // 这一趟已经能走了：之前排队的那一趟作废，别在动画走完后又补一页。
        self.pending_turn = None;
        match plan {
            TurnPlan::None => {
                // 自动前进走到头（最后一页 / 最后一章末）：停表，按钮跟着熄灭。
                // 排版没就绪不是尽头（`ready == false`），下一拍再试。
                if self.auto_driving && facts.ready {
                    self.auto = None;
                    cx.notify();
                }
            }
            TurnPlan::Scroll { to } => {
                // 起点是**画出来的**那一个数而不是 `scroll_y`：读者连点两下时，第二趟
                // 要从他眼前那一屏起步，否则画面先跳回上一趟的终点再走，读成卡了一下。
                self.begin_turn(
                    cx,
                    Turn {
                        scroll: Some((self.scroll_painted, to)),
                        forward: intent == TurnIntent::Next,
                        ..Default::default()
                    },
                );
                self.set_scroll_y(to, cx);
            }
            TurnPlan::Page { to } => {
                let before = self.page();
                self.set_page(to, cx);
                // 定版式与流式走同一条纸面动画；折页在 `begin_turn` 里换成平移
                // （定版式没有可卷的排版纹理）。
                if self.page() != before {
                    let chapter = self.chapter_index();
                    self.begin_turn(
                        cx,
                        Turn {
                            leaving: Some(PageRef {
                                chapter,
                                page: before,
                            }),
                            arriving: Some(PageRef {
                                chapter,
                                page: self.page(),
                            }),
                            forward: intent == TurnIntent::Next,
                            ..Default::default()
                        },
                    );
                }
            }
            TurnPlan::Chapter { delta } => self.begin_chapter_turn(delta, cx),
        }
    }

    /// 决策层要的那份事实快照。不借 `self` 进策略：策略只认这些数。
    ///
    /// 跟手那一趟（`swipe.rs`）在定轴那一刻也要问同一份决策，所以它对 `reader` 这一纵切可见。
    pub(in crate::ui::features::reader) fn paging_facts(&self) -> PagingFacts {
        let fixed = self.is_fixed();
        let continuous = self.laid_out.mode.is_continuous();
        let ready = if fixed {
            true
        } else if let Some(book) = self.book() {
            // 分页还要等切片；连续滚动只要正文排版在手就能滚。
            book.document().is_current() && (continuous || self.laid_out.pages > 0)
        } else {
            false
        };
        PagingFacts {
            page: self.page,
            pages: self.laid_out.pages,
            scroll: self.scroll_y,
            max_scroll: self.max_scroll(),
            viewport: self.laid_out.viewport,
            can_prev_chapter: !fixed && self.can_advance(-1),
            can_next_chapter: !fixed && self.can_advance(1),
            ready,
            fixed,
            fixed_spread: fixed && self.laid_out.mode == PageMode::Spread,
        }
    }

    /// 章界那一下：先把目标章放上「等排版」的位子，等渲染帧把它的邻章
    /// `ensure_viewport` 做完，再起跨章滑动。目标章已预取时，下一帧就开滑。
    fn begin_chapter_turn(&mut self, delta: isize, cx: &mut Context<Self>) {
        if delta == 0 {
            return;
        }
        self.cancel_turn();
        self.clear_selection();
        self.selection_menu_open = false;
        self.wheel_accum = 0.0;
        self.pending_chapter_turn = Some(delta.signum());
        cx.notify();
    }

    /// 跨章滑动落地：把目标章换成锚章，页号落到它的第一页 / 最后一页。
    ///
    /// 只在动画走到 1.0 的那一帧调用；此时目标章的排版与切片已经在 `spans` 里备好，
    /// `set_anchor` 会把它摘下来复用，不会再排一遍。
    pub(in crate::ui::features::reader) fn finish_chapter_turn(&mut self, to: PageRef) {
        if to.chapter == self.chapter_index() {
            return;
        }
        let prefs = self.prefs.clone();
        let (pages, offset) = {
            let Source::Book(book) = &mut self.source else {
                return;
            };
            book.set_anchor(to.chapter, &prefs);
            let pages = book.document().page_count().max(1);
            let page = to.page.min(pages.saturating_sub(1));
            let offset = book.document().offset_of_page(page);
            (pages, offset)
        };
        self.page = to.page.min(pages.saturating_sub(1));
        self.laid_out.pages = pages;
        self.scroll_y = 0.0;
        self.scroll_painted = 0.0;
        self.content_height = 0.0;
        self.wheel_accum = 0.0;
        self.page_shift = (0.0, 0.0);
        self.turn = None;
        self.pending_chapter_turn = None;
        self.clear_selection();
        self.persist_locator(Locator {
            chapter: to.chapter as u32,
            offset,
        });
    }

    /// 起一次翻页动画，把上一趟还没走完的那次**换掉**而不是排队：读者的第二下点击要看见
    /// 新的一趟在走，而两张以上的纸同时在屏幕上位移没有意义（出场页只留最近一张）。
    ///
    /// 时钟键、档位与时长都在 [`start_turn`] 那一格里定 —— 章界那一趟（渲染帧里排版落地时补的）
    /// 走的也是同一句。两处各写一遍，就是从前那个「第二次跨章不滑」的成因。
    pub(in crate::ui::features::reader) fn begin_turn(&mut self, cx: &App, turn: Turn) {
        let mode = self.laid_out.mode;
        // 定版式与可重排走同一张档位表：折页要的那张捕获纹理，定版式也烘（页图就是
        // 那位图，见 `view/body/fixed.rs` 的捕获通道）。上下滚动在定版式上走连续竖滚。
        let style = crate::ui::features::reader::model::chrome::effective_turn(
            &ConfigHolder::appearance(cx),
        );
        start_turn(
            &mut self.turn,
            &mut self.turn_seq,
            style,
            turn_duration(cx),
            mode,
            turn,
        );
    }

    /// 作废正在走的动画，画面当场落定。跳章、换摆法、改字号、排版恢复落地都走这一句 ——
    /// 它们的共同点是**位置本身换了参照系**，让一张按旧参照系摆出来的纸继续平移没有意义。
    pub(in crate::ui::features::reader) fn cancel_turn(&mut self) {
        self.turn = None;
        self.pending_chapter_turn = None;
        // 参照系换了：排队那趟还按旧页/旧方向算，补上只会翻错一页。
        self.pending_turn = None;
    }

    /// 画面正处在「两套坐标同时在上屏」的那一段时间：章界在等排版、正在跨章滑动，
    /// 或者**手指正握着一次跟手翻页**。这时正文上的点按 / 长按 / 拖选一律让位 ——
    /// 命中坐标只可能属于其中一套；让位比猜错强。
    pub(in crate::ui::features::reader) fn turn_in_flight(&self) -> bool {
        self.pending_chapter_turn.is_some()
            // 定版式整页翻那一档的页是位图，动画期间没有 `page_shift` 那条命中补偿；
            // 手势直接让位到这一趟走完，别拿最终位置的文字层去接一张正在滑的纸。
            // 连续竖滚不在此列：那一趟滚的是内容本身的 y，与可重排连续滚动同一条路，
            // 跟手与连续点按都该照常接住。
            || (self.is_fixed() && !self.laid_out.mode.is_continuous() && self.turn.is_some())
            || self
                .turn
                .and_then(|turn| turn.arriving)
                .is_some_and(|to| to.chapter != self.chapter_index())
            || self
                .turn
                .is_some_and(|turn| matches!(turn.run, TurnRun::Following { .. }))
    }

    /// 跳到全书的某个比例处（进度条）。
    ///
    /// 与 `progress()` 互为反函数：拖到 73% 松手，页脚念回来的还是 73%。从前这一格落在
    /// **视口顶那一章**的章内刻度上，于是进度条是「本章的条」—— 读者把它拖到最右，到的
    /// 是本章末屏，而不是这本书的末尾。
    ///
    /// 落点走 [`ReaderSession::goto_offset`]（目录跳转那一条），连续与分页两种摆法它都
    /// 认，进度条因此不必自己再算一遍「章内偏移怎么变成 scroll_y」。
    pub fn seek_ratio(&mut self, ratio: f32, cx: &mut Context<Self>) {
        // 进度条那一拖的意思是「跳到哪儿」，不是「翻一页」：位移可以大到跨越整章，
        // 播一张出场页的平移只会把读者甩进一段他没法预期的滑行里。
        self.cancel_turn();
        if self.is_fixed() {
            let last = self.page_count().saturating_sub(1);
            self.set_page((ratio.clamp(0.0, 1.0) * last as f32).round() as usize, cx);
            return;
        }
        let Some((chapter, offset)) = self.book().and_then(|book| book.locate_progress(ratio))
        else {
            // 有章的字数还没算过：这一格没有可信的全书落点，什么都不做 —— 与
            // `progress()` 回 `None` 同一个决定，不拿章内比例凑一个像样的落点。
            return;
        };
        self.goto_offset(chapter, offset, cx);
    }

    /// 读者那两条滚动入口共用的落点：夹进范围、落库、重画。
    ///
    /// 0.5px 死区是给**手指**的：手势报上来的亚像素抖动不该每帧都摇一帧。
    /// 自动那一帧走 [`Self::auto_scroll_by`]，绕过死区也绕过 `cx.notify()`。
    pub(in crate::ui::features::reader) fn set_scroll_y(
        &mut self,
        target: f32,
        cx: &mut Context<Self>,
    ) {
        let clamped = target.clamp(0.0, self.max_scroll());
        if (clamped - self.scroll_y).abs() < 0.5 {
            return;
        }
        self.apply_scroll_y(clamped);
        // 读者自己把位置挪了：自动前进那条时间轴的起点跟着这一格走，否则下一帧
        // 会把读者拖出来的这一段当成「时间轴走出来的」再算一遍 —— 拖一下，屏幕跳回去。
        if let Some(state) = self.auto.as_mut() {
            state.origin_scroll = clamped;
            state.timed = false;
        }
        cx.notify();
    }

    /// 自动那一帧推进 `scroll_y`：与手动同一条落点，但不打 0.5px 死区
    /// （余数已经攒到整 1px）、不 `cx.notify()`（推进本来就在渲染帧里，
    /// 下一帧由 `request_animation_frame` 续）。
    fn auto_scroll_by(&mut self, step: f32) {
        let before = self.scroll_y;
        let clamped = (self.scroll_y + step).clamp(0.0, self.max_scroll());
        self.apply_scroll_y(clamped);
        // 落到 `max_scroll` 上被夹住了：时间轴那本账会把「没走成的那一段」当成已经
        // 走完（`origin_scroll` 照加），下一次读者拖回去时会被一个凭空多出来的基准
        // 拽一下。这里把基准贴回真正落下的那个数 —— 到底之后自动本来就停表，这一格
        // 只在「到底那一帧」有一点点用，但少了它就是一次看得见的跳。
        let landed = clamped - before;
        if (landed - step).abs() > 0.01
            && let Some(state) = self.auto.as_mut()
        {
            state.origin_scroll = clamped;
        }
    }

    /// 写 `scroll_y` 之后的那一串账，两条入口共用，别写第二份。
    fn apply_scroll_y(&mut self, clamped: f32) {
        self.scroll_y = clamped;
        // 定版式的页号是「视口里占得最多的那一页」，滚动之后要跟着改写；
        // 可重排那一族由 `resolve_position` 当场反解，不必存第二份。
        if self.is_fixed() {
            self.sync_fixed_page();
        }
        self.persist_if_moved();
    }

    /// 连续竖滚：把「视口里占得最多的那一页」写回页表。页号变了才清选中 ——
    /// 高亮还指得上字，但那一页已经整体滚出去了。
    fn sync_fixed_page(&mut self) {
        // 放大着时视口在 1x 内容坐标里只占 `viewport / scale`（页栈按 1x 铺）。
        // 不除这一下，放大之后「占得最多的那一页」会按整屏算，页号跟着偏。
        let viewport = self.laid_out.viewport / self.fixed_zoom.scale().max(1.0);
        let Some(page) = self
            .fixed()
            .and_then(|book| book.page_at_scroll(self.scroll_y, viewport))
        else {
            return;
        };
        if self.page() == page {
            return;
        }
        if let Some(book) = self.fixed_mut() {
            book.set_page(page);
        }
        self.page = page;
        self.clear_selection();
    }

    /// 翻到第 `target` 页（越界夹到最后一页）。页号变了才落库、才重画。
    /// 翻走即清选中：高亮还指得上字，但被翻走的内容留在看不见的那一页上没有意义。
    pub(in crate::ui::features::reader) fn set_page(
        &mut self,
        target: usize,
        cx: &mut Context<Self>,
    ) {
        // 定版式：页号由页表持有，翻页即改它（进度、恢复都按 `locator.chapter = 页号`）。
        // 连续竖滚里页号只是读数，落页要同时把 `scroll_y` 摆到那一页的页首 ——
        // 目录、进度条、图片定位都从这里走，三处因此都落在「那一页的开头」。
        //
        // 定版式双页的页号归**左边那一页**：目录点到奇数页时摊开的是它所在的那一对
        // （2/3）。不折的话，这一帧的 `page_key` 还是那个奇数，捕获会把它记错一页。
        let target = if self.is_fixed() && self.laid_out.mode == PageMode::Spread {
            target & !1
        } else {
            target
        };
        let continuous = self.laid_out.mode.is_continuous();
        let fixed = self.fixed_mut().map(|book| {
            let before = book.page();
            book.set_page(target);
            let now = book.page();
            let count = book.page_count();
            let top = continuous.then(|| book.stack_top_of(now)).flatten();
            (before, now, count, top)
        });
        if let Some((before, now, count, top)) = fixed {
            self.page = now;
            self.laid_out.pages = count;
            let mut scrolled = false;
            if let Some(top) = top {
                let top = top.clamp(0.0, self.max_scroll());
                if (self.scroll_y - top).abs() >= 0.5 {
                    self.scroll_y = top;
                    self.scroll_painted = top;
                    scrolled = true;
                }
            }
            if now == before {
                if scrolled {
                    cx.notify();
                }
                return;
            }
            self.clear_selection();
            // 跳到别的页（目录、书签、进度条、搜索命中）与翻一页同一件事：
            // 新的一页从铺满看起，不把上一页的倍率与角度带过去。
            self.reset_fixed_zoom();
            self.wheel_accum = 0.0;
            self.persist_if_moved();
            cx.notify();
            return;
        }
        let target = target.min(self.laid_out.pages.saturating_sub(1));
        if target == self.page {
            return;
        }
        self.page = target;
        self.clear_selection();
        self.wheel_accum = 0.0;
        self.persist_if_moved();
        cx.notify();
    }

    /// 平台层算好的滚动增量落到正文。返回「这一下真的挪动了吗」——
    /// 调用方拿它决定要不要重画（见 `view.rs` 的滚轮监听）。
    ///
    /// 触摸向下滑（dy > 0）时内容跟着手指往下走 ⇒ 露出的行号变小，所以是减。
    pub(in crate::ui::features::reader) fn on_scroll(
        &mut self,
        event: &ScrollEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        // ── 临时探针（量完随 `perf::scroll_*` 整块摘掉）──
        // 读者自己滚的（触摸/触摸板/滚轮像素）：自动前进停表 —— 与翻页那一格
        // （[`Self::turn`]）同一条规矩。自动那一拍自己推进时带着 `auto_driving`。
        if !self.auto_driving {
            self.auto = None;
        }
        let __gap_us = crate::ui::features::reader::model::perf::scroll_begin();
        let __start = std::time::Instant::now();
        let __y_before = self.scroll_y;
        // 手指还按在屏幕上这件事，即使内容已被夹在章尾、一个字都没挪，也是「在读」。
        // 所以这一条不只靠 `persist_locator` 那个证据点（它要求位置真的变过）。
        self.note_reading_activity();
        // 帧间隔归谁的那道判据之一：滚动事件到了几个（与「正文画了几次」「会话出了几次
        // 元素树」三条一起看，见 `perf.rs` 模块头）。挂在最前面，因为它要数的是**事件**，
        // 而下面那几条 `return false` 恰恰是「事件到了但没挪动」。
        crate::ui::features::reader::model::perf::note_scroll();
        let (dx, dy) = match event.delta {
            ScrollDelta::Pixels(delta) => (f32::from(delta.x), f32::from(delta.y)),
            // 鼠标滚轮（外接键盘/调试时）按「行」给增量，按正文字号折算，别当 0 忽略。
            ScrollDelta::Lines(lines) => (
                lines.x * self.prefs.font_size,
                lines.y * self.prefs.font_size,
            ),
        };
        // 跟手翻页那一层先拿这一笔，它自己按读者选的档位与那根轴决定认不认（`swipe_scroll`
        // 给回 `false` 就是让回去，下面纵向那两条路照旧）。
        //
        // 这一问**必须**在 `turn_in_flight` 之前 —— 跟手那一趟一挂上，那条判据就为真，
        // 第二帧起的每一个增量都会被它挡掉，读者看到的是纸跟了半程就停住。
        //
        // 也不能按增量是否为 0 筛：抬手那一条 `Ended` 恒为零增量，而恰恰是它带着「松手」；
        // 一笔的 `Started` 同样可能是零增量 —— 上一笔的惯性被这一按接走时，识别器先把那段
        // 尾巴结掉，再给这一笔开个头，错过这一格就没有跟踪态，后面每一个增量都会被当成
        // 惯性尾巴丢掉（症状：连着划第二下没反应）。
        // 滚轮不是手势：增量按「行」给。它一到就把上一笔的惯性闸门放下 —— 触屏设备上
        // 拖完一次之后，第一格滚轮不该被当成惯性吃掉。
        let wheel = matches!(event.delta, ScrollDelta::Lines(_));
        // 落在**可拖动的插图**上：这一笔归「拖图」，不翻页也不滚。
        //
        // 判据按**按下点**（`ScrollEvent.position` 在一整趟拖动里恒等于按下点，
        // 见 `SwipeTrack` 的注释），而命中区是上一帧画图时登记下来的窗口矩形 —— 两处
        // 同一套坐标。只接管「图比框大」的那些（`slack > 0`，即 `object-fit: cover`
        // 被裁掉的那一圈），图刚好装满框时读者拖它仍然翻页。
        //
        // 滚轮不在此列：它不是手指，`position` 是光标位置，转一格就拖动一张图没有道理。
        if !wheel && let Some(hit) = self.image_hit_at(event.position) {
            self.pan_image(&hit, dx, dy, cx);
            crate::ui::features::reader::model::perf::scroll_end(
                __gap_us,
                (__start.elapsed().as_micros() as u64).max(1),
                0,
                dy,
                0.0,
                __y_before,
                phase_code(event.touch_phase),
            );
            return true;
        }
        // 放大着的时候，手指那一趟归**平移**：平移本身由原始触摸流那一格做掉了
        // （识别器把另一轴抹成 0，两轴同时拖只有原始触摸给得全），这里把折出来的这一笔
        // 直接让掉 —— 不让的话横向拖动会被判成翻页，而读者只是在挪着看细节。
        //
        // 滚轮不在此列：它是按键不是手指，桌面放大着时滚一格换一页正好用。
        if self.fixed_zoomed() && !wheel {
            return false;
        }
        // 手指按在版心的哪个位置（按 `page_area` 归一化，与折线、捕获矩形同一把尺）。
        // 折页那一档按它挑**从哪个角起折**：落在下半屏就从下面卷起来。
        // 版心还没量出来时给 `None`，那一档退回上缘。
        let at = {
            let area = self.page_area();
            if area.width() <= 1.0 || area.height() <= 1.0 {
                None
            } else {
                // 从前这里按 `content_bounds`（字形元素自己那块）归一，而折线按 `page_area`
                // 还原 —— 两个矩形差着页边距，手指在左缘时折线的锚点已经偏出三十来像素。
                let x = f32::from(event.position.x) - area.left;
                let y = f32::from(event.position.y) - area.top;
                Some((
                    (x / area.width()).clamp(0.0, 1.0),
                    (y / area.height()).clamp(0.0, 1.0),
                ))
            }
        };
        // 手势层：翻页那一趟（跟手 / 松手 / 惯性尾巴 / 轴不对）全在这里结账。
        // `Ignore` 才是「这一笔不归我」—— 连续滚动里手指带着内容走的那一段。
        // 手指此刻的位置先进跟踪态：折页那一档每一步都要按它重算折线。
        if let (Some(at), Some(track)) = (at, self.swipe.as_mut()) {
            track.at = Some(at);
        }
        if self.swipe_scroll(event.touch_phase, dx, dy, wheel, at, cx) != SwipeStep::Ignore {
            crate::ui::features::reader::model::perf::scroll_end(
                __gap_us,
                (__start.elapsed().as_micros() as u64).max(1),
                0,
                dy,
                0.0,
                __y_before,
                phase_code(event.touch_phase),
            );
            // 这一趟挪动的是那两张纸，不是 `scroll_y`：重画由跟手那几处自己 `cx.notify()`，
            // 这里给回 `false`，调用方不必再补一次。
            return false;
        }
        // 跨章滑动（或等目标章排版）期间滚轮不参与：这一趟的坐标属于两章，
        // 再叠一次页码会把两张纸的方向搅乱。等它落地再滚。
        if self.turn_in_flight() {
            return false;
        }
        // 浮层（顶栏底栏 / 脚注小纸片）压着的时候，分页那两条「滚一下翻一页」的路让位：
        // 滚轮一格一页、触摸板的像素增量攒半屏一页，走的都是翻页，与 `claim_swipe` 那条
        // 跟手翻页的让位是同一条规矩 —— 工具栏露头之后正文上的输入只表达「收浮层」。
        // 连续滚动不在此列：那一路没有「翻页」这个动作，手指与滚轮滚的就是正文本身，
        // 挡了它，读者开着浮层就再也读不下去半行。
        if self.overlay_open && !self.laid_out.mode.is_continuous() {
            return false;
        }
        if dy == 0.0 {
            // 零增量也是样本：惯性尾巴上「这一帧该走的距离被上一帧走过了」就是它。
            crate::ui::features::reader::model::perf::scroll_end(
                __gap_us,
                (__start.elapsed().as_micros() as u64).max(1),
                0,
                dy,
                0.0,
                __y_before,
                phase_code(event.touch_phase),
            );
            // 这一条**必须**在落定动画之前问：一次纯横扫（没被上面那一路认领时）就是
            // `dy == 0`，而横扫在读者那儿不是「开始控制这一屏的纵向位置」。从前这句落定
            // 写在 `dy` 判据之前，症状是点屏翻页那两百多毫秒的平移被紧随的横扫当场按停，
            // 读者看到的是「没有动画，直接跳页」。
            return false;
        }
        // 位置有没有真的变过：滚到章尾被夹住、手指拖到一半都不该重画。
        // 这条判据与 gpui 自己的 `div` 滚动容器、zed editor 的 `scroll` 同源
        // （位置没变就不 notify），省下的是一整帧的场景重建。
        let before = (self.scroll_y, self.page);
        let mut boundary = None;
        if self.laid_out.mode.is_continuous() {
            // 手已经在直接控制这一屏了，补间不该追在手指后面：正在走的那一趟当场落定。
            // 落定之后画出来的位置由渲染帧按「没有动画」那一条重新对齐到 `scroll_y`。
            //
            // **这一句只属于连续滚动**：分页里正在走的是那两张纸，滚动事件不该能按停它
            // （从前它挂在两支共用的位置上，于是竖向拖动的惯性把松手动画当场掐掉）。
            self.cancel_turn();
            let max = self.max_scroll();
            self.scroll_y = (self.scroll_y - dy).clamp(0.0, max);
            // 定版式：滚动要带着页号走。可重排那一族的位置由 `resolve_position` 现解，
            // 这里只把定版式的「视口顶那一页」写回页表。
            if self.is_fixed() {
                self.sync_fixed_page();
            }
        } else {
            // 分页 / 双页 / 定版式：这里不「滚」，它翻页。两种来源分两种算法：
            //
            // - **鼠标滚轮**（按「行」给）一格一页 —— 与点屏幕两边、方向键、音量键是同一类
            //   **离散动作**，所以走同一个入口 [`Self::turn`]。章界往哪边换、定版式双页步进
            //   是 2 还是 1、动画起不起、落页与清选中、进度落库，全在那一条路上算一次。
            //   从前这里自己写页号，于是同一本书上「点」有卷页而「滚」没有，定版式双页还
            //   少迈一步（步进被写死成 1）—— 读者看到的就是两条输入逻辑不一致。
            // - **平台的像素增量**（触摸板 / 没有阶段信息的合成滚动）攒半屏翻一页：一次横扫
            //   会发几十条增量，一增量一页就是几十页。那是连续输入流而不是离散动作，所以
            //   不进 `turn` —— 一趟一条动画的话，读者攒出来的那几页只会剩最后一次的过场。
            //
            // 手指拖动**到不了这里**：`swipe_scroll` 已经把它接走（`Turn` / `Swallow`）。
            // 从前它漏进来，惯性把余量推过半屏就又翻一页 —— 那是「竖向滑动影响竖向翻页」
            // 的另一半现场。
            let last = self.page_count().saturating_sub(1) as isize;
            let from = self.page() as isize;
            let mut page = from;
            if wheel {
                let step = if dy > 0.0 { -1 } else { 1 };
                if let Some(intent) = TurnIntent::from_delta(step) {
                    self.turn(intent, cx);
                }
            } else {
                self.wheel_accum -= dy;
                let threshold = (self.laid_out.viewport * 0.5).max(1.0);
                while self.wheel_accum.abs() >= threshold {
                    let step = if self.wheel_accum > 0.0 { 1 } else { -1 };
                    self.wheel_accum -= step as f32 * threshold;
                    let next = page + step;
                    if next < 0 || next > last {
                        boundary = (!self.is_fixed()).then_some(step);
                        break;
                    }
                    page = next;
                }
            }
            if page != from {
                // 落页也走 [`Self::set_page`]：定版式的页表与双页对齐（页号归左边那一张）、
                // `laid_out.pages` 跟着页表走、清选中、进度落库、`cx.notify()` 都在它里面。
                // 从前这里直接写 `self.page` 与 `fixed.set_page`，等于在旁边抄了一份
                // `set_page` 的简化版 —— 少抄的那几格就是「触摸板翻页后缩放还留着」这类事。
                self.set_page(page as usize, cx);
            }
            if let Some(step) = boundary {
                self.begin_chapter_turn(step, cx);
            }
        }
        let __persist = std::time::Instant::now();
        self.persist_if_moved();
        // ── 临时探针：这一趟的收尾三笔（落库耗时 / 处理器总耗时 / 这一步真正挪了多少）──
        let __persist_us = __persist.elapsed().as_micros() as u64;
        let __handler_us = __start.elapsed().as_micros() as u64;
        crate::ui::features::reader::model::perf::scroll_end(
            __gap_us,
            __handler_us.max(1),
            __persist_us,
            dy,
            self.scroll_y - __y_before,
            __y_before,
            phase_code(event.touch_phase),
        );
        boundary.is_some() || (self.scroll_y, self.page) != before
    }
}

/// 临时探针用的相位编码（量完随 `perf::scroll_*` 一起摘）。
fn phase_code(phase: gpui_kit::TouchPhase) -> u8 {
    match phase {
        gpui_kit::TouchPhase::Started => 0,
        gpui_kit::TouchPhase::Moved => 1,
        gpui_kit::TouchPhase::Ended => 2,
        gpui_kit::TouchPhase::Cancelled => 3,
    }
}

/// 静止帧该替哪一张纸预解图：下一张，还是上一张。
///
/// 为什么要有「上一张」这一档：读者只往前读，但往回翻那一下与往前翻一样会露出整页图，
/// 而回翻走的那条路（`TurnIntent::Previous`）与往前翻完全对称 —— 只预热下一张，回翻时
/// 图还是要等后台解完才补上来。
///
/// 取哪一头按两条判据：
///
/// - 这一页**上面**还有内容（`has_above`）才回看。首页回看是负页号，跨章那一头由调用方
///   用 `cross_prev` 表达（上一章的末页）。
/// - 这一页**下面**还有内容（`has_below`）才往前看。末页同理。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::ui::features::reader) enum PrefetchSide {
    Ahead,
    Behind,
}

/// 静止帧预解哪一侧的图。两侧都可预解时只给**下一张**：`24 MiB` 的图集封顶那一档
/// （`image_atlas::soft_limit`）经不起两个方向的图同时在手，而往后读是绝大多数。
/// 两侧都没有内容时回 `None` —— 这一章只有一页且前后都没有邻章。
pub(in crate::ui::features::reader) fn prefetch_side(
    has_above: bool,
    has_below: bool,
) -> Option<PrefetchSide> {
    match (has_above, has_below) {
        (_, true) => Some(PrefetchSide::Ahead),
        (true, false) => Some(PrefetchSide::Behind),
        (false, false) => None,
    }
}

#[cfg(test)]
mod prefetch_tests {
    //! 只钉选边那一条。真机上它没接住时的症状是「翻到带图的那一页图晚一帧才出来」，
    //! 与「没预取」长得一样，区别只在帧数 —— 所以这里只保证两个方向都**选得到**。
    use super::{PrefetchSide, prefetch_side};

    #[test]
    fn the_resting_frame_looks_one_page_ahead_when_there_is_one() {
        assert_eq!(prefetch_side(true, true), Some(PrefetchSide::Ahead));
        assert_eq!(prefetch_side(false, true), Some(PrefetchSide::Ahead));
        assert_eq!(
            prefetch_side(true, false),
            Some(PrefetchSide::Behind),
            "末页只能回看"
        );
        assert_eq!(
            prefetch_side(false, false),
            None,
            "单页一章且前后都没有邻章：没有可预解的那一张"
        );
    }
}

#[cfg(test)]
mod tests {
    //! 自动前进的那条门：连续滚动档按拍子往前爬，读者的任何一次翻页把它停掉。
    //! 夹具与 `speech.rs` 那一组同形（一扇窗 + 一条真有 id 的库行）。

    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::ui::features::reader::model::session::ReaderSession;
    use folio_core::content::document::Document;
    use folio_core::{Chapter, Config, Content, DocMeta, FlowDoc, Format, Locator, Reader};
    use gpui_kit::component::Root;
    use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext, px, size};

    fn meta() -> DocMeta {
        DocMeta {
            title: "自动前进测试".into(),
            format: Format::parse("markdown").expect("格式名要认得"),
            ..DocMeta::default()
        }
    }

    fn store_with_book(flow: &FlowDoc) -> (Arc<folio_core::Store>, i64) {
        let store = Arc::new(folio_core::Store::in_memory().expect("内存库"));
        let id = store
            .add_book(std::path::Path::new("memory://fixture"), &meta())
            .expect("入库");
        store
            .save_flow(id, flow, Format::Markdown)
            .expect("章节入库");
        (store, id)
    }

    fn open(
        cx: &mut TestAppContext,
        flow: &FlowDoc,
        store: Option<(Arc<folio_core::Store>, i64)>,
    ) -> Entity<ReaderSession> {
        let doc: Arc<dyn Document> = Arc::from(folio_core::content::document::from_content(
            meta(),
            Content::Reflow(flow.clone()),
        ));
        let (store, book_id) = match store {
            Some((store, id)) => (Some(store), Some(id)),
            None => (None, None),
        };
        let mut view = None;
        let handle = cx.open_window(size(px(460.), px(520.)), |window, cx| {
            ConfigHolder::install(Config::default(), cx);
            let built = ReaderSession::with_document(
                doc,
                Reader::default(),
                store,
                book_id,
                Locator::start(),
                cx,
            );
            let created = cx.new(|_| built);
            view = Some(created.clone());
            Root::new(created, window, cx)
        });
        let _handle: AnyWindowHandle = handle.into();
        cx.run_until_parked();
        view.expect("ReaderSession 应该被创建")
    }

    /// 滚动档：按真实 dt 把 scroll_y 往前推；读者自己一动就停表。
    /// 这一档现在由 `AutoReadMode` 显式选定，不再跟着书的摆法猜 —— 默认翻页档会让
    /// 连续摆法的书起自动时先被切回分页，这条门就钉住「选滚动才是真滚动」。
    #[gpui_kit::test]
    fn auto_advance_scroll_mode_creeps_then_user_input_stops_it(cx: &mut TestAppContext) {
        use folio_core::infra::config::PageTurn;

        cx.update(gpui_kit::init);
        let body = "<p>这是一段用来把章节撑过一屏的正文，重复许多次。</p>".repeat(30);
        let flow = FlowDoc {
            chapters: vec![Chapter::inline("第一章", &body)],
            ..Default::default()
        };
        let (store, id) = store_with_book(&flow);
        let session = open(cx, &flow, Some((store, id)));
        cx.update(|app| {
            ConfigHolder::edit_book_appearance(app, |a| a.page_turn = PageTurn::Scroll);
        });

        cx.run_until_parked();
        assert!(
            matches!(session.read_with(cx, |s, _| s.mode), PageMode::Continuous),
            "前置：这本该已经折到连续滚动档"
        );

        // 显式选滚动档再开自动：起表那一瞬摆法跟着这一档切过去，拍子按像素推。
        cx.update(|app| {
            session.update(app, |s, cx| {
                s.set_auto_read_mode(AutoReadMode::Scroll, cx);
                s.set_auto(true, cx);
            })
        });
        assert!(session.read_with(cx, |s, _| s.auto_on()));
        assert!(
            matches!(session.read_with(cx, |s, _| s.mode), PageMode::Continuous),
            "滚动档该把正文摆在连续一列上"
        );
        // 显式递时刻（假钟不吃 background timer，见 auto_step_at 那条）。
        // 速度默认 12 页/分，视口 600 兜底 → 一秒推 120px。
        let before = session.read_with(cx, |s, _| s.scroll_y);
        let at = std::time::Instant::now();
        for step in 1..=3 {
            let now = at + Duration::from_millis(1000 * step);
            cx.update(|app| session.update(app, |s, cx| s.auto_step_at(now, cx)));
        }
        let after = session.read_with(cx, |s, _| s.scroll_y);
        assert!(
            after > before,
            "三秒的滚动自动该把正文往前爬：{before} → {after}"
        );

        // 读者自己翻一下（连续档的 scroll_page 也是 turn 那条路）：自动停表。
        cx.update(|app| session.update(app, |s, cx| s.scroll_page(1, cx)));
        assert!(
            !session.read_with(cx, |s, _| s.auto_on()),
            "读者的任何一次翻页都要停掉自动前进"
        );
    }

    /// 慢速档的「抖」：2 页/分每帧只推约 0.3px，从前全被 `set_scroll_y` 的
    /// 0.5px 死区吞掉，攒几帧突然跳一下（甚至整段不动）。余数进位之后总量
    /// 贴着理论值走，一帧都不丢。
    #[gpui_kit::test]
    fn a_slow_auto_scroll_keeps_its_subpixel_remainder(cx: &mut TestAppContext) {
        use folio_core::infra::config::PageTurn;

        cx.update(gpui_kit::init);
        let body = "<p>这是一段用来把章节撑过一屏的正文，重复许多次。</p>".repeat(30);
        let flow = FlowDoc {
            chapters: vec![Chapter::inline("第一章", &body)],
            ..Default::default()
        };
        let (store, id) = store_with_book(&flow);
        let session = open(cx, &flow, Some((store, id)));
        cx.update(|app| {
            ConfigHolder::edit_book_appearance(app, |a| {
                a.page_turn = PageTurn::Scroll;
                a.auto_advance_speed =
                    crate::ui::features::reader::model::chrome::AUTO_ADVANCE_RANGE.0;
            });
        });
        cx.run_until_parked();
        cx.update(|app| {
            session.update(app, |s, cx| {
                s.set_auto_read_mode(AutoReadMode::Scroll, cx);
                s.set_auto(true, cx);
            })
        });
        let at = std::time::Instant::now();
        let start = session.read_with(cx, |s, _| s.scroll_y);
        let mut prev = start;
        for step in 1..=60 {
            let now = at + Duration::from_millis(16 * step);
            cx.update(|app| session.update(app, |s, cx| s.auto_step_at(now, cx)));
            let y = session.read_with(cx, |s, _| s.scroll_y);
            assert!(y >= prev, "第 {step} 帧不许往回跳：{prev} → {y}");
            prev = y;
        }
        // 量**增量**：`scroll_y` 是全局坐标，开书那一帧未必从 0 起。
        // 2 页/分 × 600px 视口 = 20 px/s，60 帧 × 16ms = 0.96s ≈ 19px。
        // 旧实现（死区逐帧吞）这里是 0，所以这一条同时是「余数进位」的门。
        let moved = prev - start;
        assert!(
            moved > 8.0 && moved < 30.0,
            "慢档增量该贴着理论值，不追赶也不丢：{moved}"
        );
    }

    /// 帧间隔**不匀**时，位移仍然贴着时间轴走 —— 「自动滚动有种抖动」那一半的门。
    ///
    /// 逐帧累加的写法（`dpx = v × dt` 每帧加一笔）在这里会露馅：16ms 一帧与
    /// 33ms 一帧交替时，每帧的位移一快一慢，二阶差就是帧间隔的二阶差。时间轴写法
    /// 只问「从起点到今天走了多久」，帧间隔在哪儿跳都不影响该落在的位置。
    ///
    /// 判据用**每帧位移的方差**：等间隔时钟下它该是 0（除了 1px 取整那一步），
    /// 而交替间隔的逐帧累加至少会出现两种明显不同的步长。
    #[gpui_kit::test]
    fn an_uneven_clock_still_advances_by_the_timeline(cx: &mut TestAppContext) {
        use folio_core::infra::config::PageTurn;

        cx.update(gpui_kit::init);
        let body = "<p>这是一段用来把章节撑过一屏的正文，重复许多次。</p>".repeat(30);
        let flow = FlowDoc {
            chapters: vec![Chapter::inline("第一章", &body)],
            ..Default::default()
        };
        let (store, id) = store_with_book(&flow);
        let session = open(cx, &flow, Some((store, id)));
        cx.update(|app| {
            ConfigHolder::edit_book_appearance(app, |a| {
                a.page_turn = PageTurn::Scroll;
                a.auto_advance_speed =
                    crate::ui::features::reader::model::chrome::AUTO_ADVANCE_RANGE.1;
            });
        });
        cx.run_until_parked();
        cx.update(|app| {
            session.update(app, |s, cx| {
                s.set_auto_read_mode(AutoReadMode::Scroll, cx);
                s.set_auto(true, cx);
            })
        });
        let at = std::time::Instant::now();
        let start = session.read_with(cx, |s, _| s.scroll_y);
        let mut prev = start;
        let mut elapsed_ms = 0u64;
        let mut steps = Vec::new();
        // 16 / 17 / 33 / 16 这样交替：真实帧间隔的粗略样子。
        for ms in [16u64, 17, 33, 16, 17, 33, 16, 17, 33, 16, 17, 33] {
            elapsed_ms += ms;
            let now = at + Duration::from_millis(elapsed_ms);
            cx.update(|app| session.update(app, |s, cx| s.auto_step_at(now, cx)));
            let y = session.read_with(cx, |s, _| s.scroll_y);
            assert!(y >= prev, "不许往回跳：{prev} → {y}");
            steps.push(y - prev);
            prev = y;
        }
        // 40 页/分 × 600px = 400 px/s，每个固定时长的间隔里该走一样多的像素。
        // 「同样时长的两帧位移相同」就是这条门：16ms 那几帧彼此相等、33ms 那几帧
        // 彼此相等，且后者约是前者的两倍。
        let sixteen: Vec<f32> = steps.iter().copied().step_by(3).collect();
        let thirty_three: Vec<f32> = steps.iter().copied().skip(2).step_by(3).collect();
        let spread = |xs: &[f32]| {
            let max = xs.iter().cloned().fold(f32::MIN, f32::max);
            let min = xs.iter().cloned().fold(f32::MAX, f32::min);
            max - min
        };
        assert!(
            spread(&sixteen) <= 1.01,
            "同样 16ms 的帧位移该一致（1px 取整那一档）：{sixteen:?}"
        );
        assert!(
            spread(&thirty_three) <= 1.01,
            "同样 33ms 的帧位移该一致：{thirty_three:?}"
        );
        // 位移的**分布**是这一条真正要钉的：逐帧累加时 33ms 与 16ms 会拉开明显距离，
        // 时间轴写法里每一帧的位移只由「这一帧跨了多久」决定，两种间隔各自内部一致，
        // 比值取决于起点对齐，不必钉死到 2 倍 —— 钉的是**总量**（下面那句）。
        assert!(
            thirty_three.iter().sum::<f32>() > sixteen.iter().sum::<f32>(),
            "33ms 那几帧加起来该比 16ms 那几帧多：{thirty_three:?} vs {sixteen:?}"
        );
        // 总量贴着时间轴：400 px/s × 0.234s ≈ 93px。
        let moved = prev - start;
        assert!(
            (85.0..110.0).contains(&moved),
            "总量该贴着时间轴算出来的值：{moved}"
        );
    }

    /// 帧驱动：等间隔的每一帧都推进一次、位移单调、总量贴着 dt 算出来的值。
    /// 定时器那条路会与 vsync 拍频（有的帧 0 次、有的帧 2 次），这条门钉的是
    /// 「每画一帧恰好推一次」这条契约。
    #[gpui_kit::test]
    fn a_frame_driven_auto_scroll_advances_once_per_frame(cx: &mut TestAppContext) {
        use folio_core::infra::config::PageTurn;

        cx.update(gpui_kit::init);
        let body = "<p>这是一段用来把章节撑过一屏的正文，重复许多次。</p>".repeat(30);
        let flow = FlowDoc {
            chapters: vec![Chapter::inline("第一章", &body)],
            ..Default::default()
        };
        let (store, id) = store_with_book(&flow);
        let session = open(cx, &flow, Some((store, id)));
        cx.update(|app| {
            ConfigHolder::edit_book_appearance(app, |a| a.page_turn = PageTurn::Scroll);
        });
        cx.run_until_parked();
        cx.update(|app| {
            session.update(app, |s, cx| {
                s.set_auto_read_mode(AutoReadMode::Scroll, cx);
                s.set_auto(true, cx);
            })
        });
        let at = std::time::Instant::now();
        let start = session.read_with(cx, |s, _| s.scroll_y);
        let mut prev = start;
        let mut advances = 0;
        for step in 1..=60 {
            let now = at + Duration::from_millis(16 * step);
            cx.update(|app| session.update(app, |s, cx| s.auto_step_at(now, cx)));
            let y = session.read_with(cx, |s, _| s.scroll_y);
            assert!(y >= prev, "第 {step} 帧不许往回跳：{prev} → {y}");
            if y > prev {
                advances += 1;
            }
            prev = y;
        }
        assert!(advances >= 55, "默认速度下几乎每帧都该挪：{advances}/60");
        // 12 页/分 × 600px = 120 px/s，0.96s ≈ 115px。同样只量增量。
        let moved = prev - start;
        assert!(
            moved > 90.0 && moved < 140.0,
            "增量贴着 dt 算出来的值，不多也不少：{moved}"
        );
    }

    /// 翻页档：攒够间隔债翻一张，翻完还开着 —— 自动那一拍带着 auto_driving，
    /// 不该被 `turn` 入口「读者动了就停表」那道闸误杀。
    #[gpui_kit::test]
    fn auto_advance_page_mode_turns_a_page_and_keeps_running(cx: &mut TestAppContext) {
        use folio_core::infra::config::PageTurn;

        cx.update(gpui_kit::init);
        let body = "<p>这是一段用来把章节撑过一屏的正文，重复许多次。</p>".repeat(30);
        let flow = FlowDoc {
            chapters: vec![Chapter::inline("第一章", &body)],
            ..Default::default()
        };
        let (store, id) = store_with_book(&flow);
        let session = open(cx, &flow, Some((store, id)));

        cx.update(|app| {
            ConfigHolder::edit_book_appearance(app, |a| a.page_turn = PageTurn::Cover);
        });
        cx.run_until_parked();
        cx.update(|app| {
            session.update(app, |s, cx| {
                s.set_auto_read_mode(AutoReadMode::Page, cx);
                s.set_auto(true, cx);
            })
        });
        assert!(
            !matches!(session.read_with(cx, |s, _| s.mode), PageMode::Continuous),
            "翻页档该站在分页上，不是连续一列"
        );
        let at = std::time::Instant::now();
        for step in 1..=6 {
            let now = at + Duration::from_millis(1000 * step);
            cx.update(|app| session.update(app, |s, cx| s.auto_step_at(now, cx)));
        }
        assert!(
            session.read_with(cx, |s, _| s.turn.is_some()),
            "攒够一分钟 12 页的间隔，该翻出一张"
        );
        assert!(
            session.read_with(cx, |s, _| s.auto_on()),
            "翻页档翻完一张还该开着 —— 自动那一拍带着 auto_driving，不该被 turn 入口的闸误杀"
        );
    }

    /// 运行中切档：摆法跟着换、拍子重新起算，开自动之前那一份摆法不动 —— 退出时还的是
    /// 读者原本的读法，不是上一档模式的。
    #[gpui_kit::test]
    fn switching_auto_modes_while_running_keeps_the_saved_layout(cx: &mut TestAppContext) {
        use folio_core::infra::config::PageTurn;

        cx.update(gpui_kit::init);
        let body = "<p>这是一段用来把章节撑过一屏的正文，重复许多次。</p>".repeat(30);
        let flow = FlowDoc {
            chapters: vec![Chapter::inline("第一章", &body)],
            ..Default::default()
        };
        let (store, id) = store_with_book(&flow);
        let session = open(cx, &flow, Some((store, id)));
        cx.update(|app| {
            ConfigHolder::edit_book_appearance(app, |a| a.page_turn = PageTurn::Cover);
        });
        cx.run_until_parked();

        cx.update(|app| {
            session.update(app, |s, cx| {
                s.set_auto_read_mode(AutoReadMode::Page, cx);
                s.set_auto(true, cx);
            })
        });
        let saved = session.read_with(cx, |s, _| s.auto_saved_layout);
        assert_eq!(
            saved.map(|(mode, _)| mode),
            Some(PageMode::Paged),
            "开自动前这本站在分页上，记下的该是它"
        );

        cx.update(|app| {
            session.update(app, |s, cx| s.set_auto_read_mode(AutoReadMode::Scroll, cx))
        });
        assert!(
            matches!(session.read_with(cx, |s, _| s.mode), PageMode::Continuous),
            "运行中切到滚动档，正文该换到连续一列"
        );
        assert_eq!(
            session.read_with(cx, |s, _| s.auto_saved_layout),
            saved,
            "切档不该改写开自动之前的摆法"
        );
        assert!(
            session.read_with(cx, |s, _| s.auto_on()),
            "切档不该把拍子停掉"
        );
        // 切回翻页档又站回分页；停表后还原交给渲染帧（那里才收 auto_saved_layout）。
        cx.update(|app| session.update(app, |s, cx| s.set_auto_read_mode(AutoReadMode::Page, cx)));
        assert!(
            matches!(session.read_with(cx, |s, _| s.mode), PageMode::Paged),
            "切回翻页档又该是分页"
        );
        cx.update(|app| session.update(app, |s, cx| s.set_auto(false, cx)));
        assert!(!session.read_with(cx, |s, _| s.auto_on()), "停表");
    }

    /// 走到头就停表：滚动档滚到底、翻页档翻到最后一页，都不该让按钮一直亮着。
    #[gpui_kit::test]
    fn auto_advance_stops_at_the_end_of_the_book(cx: &mut TestAppContext) {
        use folio_core::infra::config::PageTurn;

        cx.update(gpui_kit::init);
        // 一页就完的短书：滚动档没有可推的像素，分页档没有下一张。
        let flow = FlowDoc {
            chapters: vec![Chapter::inline("第一章", "<p>短。</p>")],
            ..Default::default()
        };
        let (store, id) = store_with_book(&flow);
        let session = open(cx, &flow, Some((store, id)));

        // 滚动档：显式选中，一拍之后该停表。
        cx.update(|app| {
            ConfigHolder::edit_book_appearance(app, |a| a.page_turn = PageTurn::Scroll);
        });
        cx.run_until_parked();
        cx.update(|app| {
            session.update(app, |s, cx| {
                s.set_auto_read_mode(AutoReadMode::Scroll, cx);
                s.set_auto(true, cx);
            })
        });
        let at = std::time::Instant::now();
        cx.update(|app| {
            session.update(app, |s, cx| {
                s.auto_step_at(at + Duration::from_millis(1000), cx)
            })
        });
        assert!(
            !session.read_with(cx, |s, _| s.auto_on()),
            "滚动档已经在底，一拍之后该停表"
        );

        // 翻页档：显式选中，攒够间隔债也没有下一张，该停表。
        cx.update(|app| {
            ConfigHolder::edit_book_appearance(app, |a| a.page_turn = PageTurn::Cover);
        });
        cx.run_until_parked();
        cx.update(|app| {
            session.update(app, |s, cx| {
                s.set_auto_read_mode(AutoReadMode::Page, cx);
                s.set_auto(true, cx);
            })
        });
        let at = std::time::Instant::now();
        for step in 1..=6 {
            let now = at + Duration::from_millis(1000 * step);
            cx.update(|app| session.update(app, |s, cx| s.auto_step_at(now, cx)));
        }
        assert!(
            !session.read_with(cx, |s, _| s.auto_on()),
            "分页档停在最后一页，攒够间隔债也没有下一张，该停表"
        );
    }
}
