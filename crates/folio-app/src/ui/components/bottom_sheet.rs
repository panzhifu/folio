//! 底部弹层：从屏幕底边升起来、可以拖着走、能停在几档高度的模态面板。
//!
//! 对应 Material3 的 `ModalBottomSheet`（Compose 那边是 `ModalBottomSheet` +
//! `rememberModalBottomSheetState` 的 anchors）。**没有**用 gpui-kit 的
//! `component::Sheet`：它的进入动画写死 150ms/100 像素而且没有退场，`Root::close_sheet`
//! 一调到就当场卸载，一个窗口还只容得下一层 —— 与「拖到一半松手要回弹」「关掉要沉下去、
//! 动完才卸载」这两件都对不上。这里只借它底下那层宿主 `base::Sheet`（焦点陷阱、返回键、
//! 点遮罩关闭、关闭回调的先后次序都在它手里），拖动与弹簧自己接。
//!
//! **动的量是「露出多高」，不是「往下挪了多少」**。Material 那张卡片的底边一直钉在屏幕
//! 底，手指拖的是顶边，所以半屏态与全屏态是同一块底边的两种高。按偏移量算的话，半屏态会
//! 把卡片的下沿推出屏幕外 —— 那是另一种东西。停靠位因此是一列高度（[`BottomSheet::expanded`]
//! 最高、[`BottomSheet::peek`] 更低），[`settle`] 判松手之后落回哪一档、还是干脆关掉。
//!
//! 三处值得先说清的判断：
//!
//! - **挂载与否看露出高，不看意图**。`hide()` 一调顶边就开始往下走，走到 0 那一帧才真的
//!   不挂。所以调用方只需要一个 [`BottomSheetState`]，不需要 Compose 那种
//!   `hide().invokeOnCompletion { show = false }` 的编排 —— 等动完这件事在组件内部。
//! - **这块元素每帧都要构造**（包括已经收好那一段）。弹簧的每帧采样在
//!   [`BottomSheet::render`] 里，收起来时它自己返回空元素。用 `.when(open, ...)`
//!   只在开着时构造，会把弹簧状态连同退场动画一起丢掉：再打开时第一个值直接落到
//!   目标，就没有进入动画了。
//! - **默认只有把手那一条带能拖**。整块面板跟着手指走要跟内容滚动抢同一根手指，
//!   而 gpui 的滚动偏移拿不到（`overflow_y_scroll` 的状态是内部的），判不了「已经滚到
//!   顶还想往下拽」这一步。内容不滚的弹层用 [`BottomSheet::surface_draggable`] 放开整块。

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::base::Sheet as BaseSheet;
use gpui_kit::base::motion::{Spring, spring};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, ThemeStyled as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Bounds, Context, Corners, Entity, EventEmitter, FocusHandle,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, RenderOnce, SharedString, StyleRefinement, Styled,
    TestSupportExt as _, TouchDragEvent, TouchPhase, WeakEntity, Window, canvas, div, px, size,
};

/// 把手那条带的高（含上下留白）：手指落在这一条里才拖得动面板。
const HANDLE_BAND: f32 = 36.0;
/// 把手胶囊的尺寸，Material3 的 grabber 是 32×4。
const HANDLE_WIDTH: f32 = 32.0;
const HANDLE_HEIGHT: f32 = 4.0;
/// 走过多远才算「在拖」，而不是一次点击。Android 的 `scaledTouchSlop` 量级就是这个数。
const DRAG_SLOP: f32 = 10.0;
/// 松手时手指向下的速度（逻辑像素/秒）超过它，就顺着手势再往下走一档；向上同理。
const FLING: f32 = 800.0;
/// 拖过最低那一档的一半就不客气了：手已经明确说了不要这一层。
const DISMISS_RATIO: f32 = 0.5;
/// 两次采样隔过这么久，手指其实是停住了：这一按不算甩。
const VELOCITY_STALE: Duration = Duration::from_millis(100);
/// 进出与换档共用一根弹簧的响应时间。临界阻尼，不过冲。
const SETTLE: Duration = Duration::from_millis(280);
/// 默认那一档：弹层展开到视口高的九成。留那一成是给读者认「我从哪本书里抬上来的」。
const DEFAULT_EXPANDED: f32 = 0.9;
/// 判「已经收到底」的容差：露出高差不到半像素就当到位，不再挂这一层。
const SETTLED_EPSILON: f32 = 0.5;

/// 松手之后的去向：某一档，或者关掉。
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Stop {
    /// 回到第 `n` 档（`anchors` 的下标，0 是最展开的那一档）。
    At(usize),
    /// 收到底，关掉这一层。
    Dismiss,
}

/// 松手去哪一档。
///
/// `shown` 是松手那一刻的露出高；`velocity` 是**手指**的速度（向下为正，与 gpui 的 y 轴
/// 同向 —— 面板跟着的是它的反面，手指往下、露出高变小）。`anchors` 自上而下：第 0 颗最高。
///
/// 三条判据按「越明确越先算」排：
/// 1. 拖过最低那档的一半 → 关掉。这条必须排在「取最近档」之前，否则手指已经把它拖到只剩
///    两成高时，最近档仍会把它拉回半屏，看起来像按了没反应。
/// 2. 甩动方向决定跨档：向下甩走一档（已经在最低那档就关），向上甩走一档（已经在顶档就
///    停在顶档，不许越过最高那档）。
/// 3. 剩下就是离得最近的那一档。
pub fn settle(shown: f32, velocity: f32, anchors: &[f32]) -> Stop {
    let (Some(top), Some(lowest)) = (anchors.first().copied(), anchors.last().copied()) else {
        // 一档都没有（视口还没量到）：判关比把 0 当成锚点停在屏幕底稳妥。
        return Stop::Dismiss;
    };
    if top <= 0.0 || shown <= lowest * (1.0 - DISMISS_RATIO) {
        return Stop::Dismiss;
    }
    let nearest = nearest_anchor(shown, anchors);
    if velocity >= FLING {
        return if nearest + 1 < anchors.len() {
            Stop::At(nearest + 1)
        } else {
            Stop::Dismiss
        };
    }
    if velocity <= -FLING {
        return Stop::At(nearest.saturating_sub(1));
    }
    Stop::At(nearest)
}

/// 离 `shown` 最近的那一档。恰好骑在两档中间时取**更高**的那一档：读者停在中间多半
/// 是想多看几行，而取低的那档会让他觉得面板自己缩回去了。
pub fn nearest_anchor(shown: f32, anchors: &[f32]) -> usize {
    let mut best = (f32::INFINITY, 0usize);
    for (ix, &anchor) in anchors.iter().enumerate() {
        let distance = (anchor - shown).abs();
        // 严格小于：一样远时保留先出现的那颗，也就是更高的那一档（`anchors` 自上而下）。
        if distance < best.0 {
            best = (distance, ix);
        }
    }
    best.1
}

/// 这一屏的停靠位，换算成像素：自上而下，第 0 颗最高。
///
/// `peek` 不低于 `expanded` 时不进这一列：两档一样高就成了一档，与其让「最低那档的一半」
/// 那条判据去猜哪颗是最低的，不如只留一处可停。
pub fn anchors(viewport_height: f32, expanded: f32, peek: Option<f32>) -> Vec<f32> {
    let top = viewport_height * expanded;
    let mut list = vec![top];
    if let Some(peek) = peek {
        let peeked = viewport_height * peek;
        if peeked < top {
            list.push(peeked);
        }
    }
    list
}

/// 拖动跟随：起手时的露出高 **减去** 手指的纵向增量，夹在「收到底」与「最高那档」之间。
///
/// 减号是因为轴反了：手指往下走（增量正）是把它往下拽，露出高变小。往上拖不给过量空间
/// （夹在最高那档）—— 这一版在顶档上面没有橡皮筋。
pub fn follow(base: f32, finger_dy: f32, top: f32) -> f32 {
    (base - finger_dy).clamp(0.0, top)
}

/// 遮罩透明度跟着露出高升降：收到底时是 0，展开到最高那档时是主题那个满值。
///
/// 遮罩不自己走一条时间线，就是为了跟面板**严格同步** —— 两者各动各的话，甩动关掉时
/// 会看到底下一块已经没了、灰还在。半屏态拿到按比例的那一档灰度，正文因此还认得出。
pub fn scrim_alpha(full: f32, shown: f32, top: f32) -> f32 {
    if top <= 0.0 {
        return full;
    }
    full * (shown / top).clamp(0.0, 1.0)
}

/// 可拖的那一块落在哪儿。默认只有顶上把手那一条带；`whole` 时整块面板都算。
///
/// 入参是**画出来的**那块 bounds（`canvas` 在绘制期回传），不是按视口推算的：
/// 宿主那层是 `anchored`，桌面与 Android 上窗口内缩、标题栏各不一样，推算出来的顶边
/// 到了手机上就会整条带错位 —— 而错位的命中区表现为「拖不灵」，最难查。
pub fn drag_band(surface: Bounds<Pixels>, whole: bool) -> Bounds<Pixels> {
    if whole {
        surface
    } else {
        Bounds {
            origin: surface.origin,
            size: size(surface.size.width, px(HANDLE_BAND)),
        }
    }
}

/// 弹层要 announce 的事件。
#[derive(Clone, Debug)]
pub enum BottomSheetEvent {
    /// 关闭是**用户**做出来的（下拉过头、向下甩、点遮罩、返回键）。
    /// 调用方自己 `hide()` 不发这一条 —— 订阅方据此把「读者关掉了」和「我让他关的」分开。
    Dismissed,
}

/// 本应用上下文里**活着**的底部弹层，一条弱引用一块。
///
/// 它存在的唯一理由：手机上那一条浮的底栏是外壳在目的地**之后**画的，所以任何一块模态
/// 弹层都会被它压住 —— 而模态那一层本该盖住导航栏。外壳画那一帧时要问一句「现在有没有
/// 一层浮在上面」，而握答案的人不止一个（架面三块、笔记一块、更新日志一块、书里两块），
/// 逐个目的地开口问就成了「加一屏要记得改外壳」的那类漂移。登记在这里，外壳只问一句。
///
/// 存的是 `WeakEntity` 而不是一个计数：**弹层随它那一屏一起消失时没人记得来减一次**。
/// 计数漏一次，底栏就永久关在外面 —— 那种坏法只在读者关掉某屏之后才看得见。弱引用自己
/// 会失效，问的时候跳过就是。
#[derive(Default)]
struct OpenSheets {
    sheets: Vec<WeakEntity<BottomSheetState>>,
    /// 下一次「抬起来」的序号。判「哪一块在最上面」用它，不用登记顺序：登记发生在
    /// `cx.new(BottomSheetState::closed)` 那一刻（全应用八块，启动时一起建好），而读者
    /// 反复开合的次序与建的先后毫无关系。
    next_order: u64,
}

/// 给这次「抬起来」发一个递增的序号。`register` 保证这一格一定在。
fn next_order(cx: &mut App) -> u64 {
    if cx.try_global::<OpenSheets>().is_none() {
        return 0;
    }
    let global = cx.global_mut::<OpenSheets>();
    global.next_order += 1;
    global.next_order
}

impl gpui_kit::Global for OpenSheets {}

/// 建一块弹层时登记一条弱引用。这张清单只随「建起过几块弹层」长 —— 全应用一共八块，
/// 都在启动时建好，所以它不需要清理那一趟。
fn register(cx: &mut Context<BottomSheetState>) {
    let weak = cx.entity().downgrade();
    if cx.try_global::<OpenSheets>().is_none() {
        cx.set_global(OpenSheets {
            sheets: vec![weak],
            next_order: 0,
        });
    } else {
        cx.global_mut::<OpenSheets>().sheets.push(weak);
    }
}

/// 现在有没有一块底部弹层正开着。外壳拿它判底栏收不收（见 `AppShell::nav_gone`）。
pub fn any_sheet_open(cx: &App) -> bool {
    cx.try_global::<OpenSheets>().is_some_and(|global| {
        global
            .sheets
            .iter()
            .any(|weak| weak.upgrade().is_some_and(|sheet| sheet.read(cx).open))
    })
}

/// 系统返回（返回键与边缘滑动是同一件事）该收的那一块：开着的、最后被抬起来的那一块。
///
/// 收掉了返回 `true`，外壳据此把这一按吃掉、不再往导航栈上走。
///
/// **为什么不是弹层自己接这一键**：gpui-kit 的 `base::Sheet` 在 `"Sheet"` 上下文里绑了
/// `escape → Cancel`，而外壳在 `AppShell::boot` 里无上下文地绑了 `escape → Back`。gpui
/// 把无上下文的绑键算成**最深那一档**（`Keymap::binding_enabled`：`contexts.len()`），
/// 同深度再按登记先后比 —— 外壳那条后绑，于是永远排在前面。`base::Sheet` 的 Cancel 一次
/// 都轮不到（它自己那份宿主测试直接 `dispatch_action(Cancel)`，绕过了键位解析，所以看着是好的）。
/// 弹层的键位再怎么往下钻也赢不了，只能由外壳在 `back()` 里主动问一句。
///
/// 挑「最后抬起来」而不是「登记最早」：同屏可以有好几块（架面上三块各握一份状态），
/// 读者关掉的必须是**他刚看见的那一块**。
pub fn dismiss_top(cx: &mut App) -> bool {
    let Some(top) = top_open(cx) else {
        return false;
    };
    top.update(cx, |state, cx| state.dismiss(cx));
    true
}

/// 开着的弹层里 `order` 最大的那一块。
fn top_open(cx: &App) -> Option<Entity<BottomSheetState>> {
    let global = cx.try_global::<OpenSheets>()?;
    let mut top: Option<(u64, Entity<BottomSheetState>)> = None;
    for weak in &global.sheets {
        let Some(sheet) = weak.upgrade() else {
            continue;
        };
        let state = sheet.read(cx);
        if !state.open {
            continue;
        }
        if top.as_ref().is_none_or(|(order, _)| state.order >= *order) {
            top = Some((state.order, sheet));
        }
    }
    top.map(|(_, sheet)| sheet)
}

/// 一次起手的记录。
#[derive(Clone, Copy)]
struct Grab {
    /// 起手点的 y。
    y: f32,
    /// 起手那一刻的露出高，跟随按它算基准（之后每帧都沿用这一份，不受重画的采样值影响）。
    base: f32,
    /// 已经走过 slop、这一手势归弹层了。
    claimed: bool,
}

/// 弹层的意图、停靠档与手指状态。元素每帧读它、事件回调每帧写它。
///
/// 动画中的那个露出高不在这儿：它在 `motion::spring` 自己那份按元素 id 存的状态里，
/// 这样被打断的拖动能从手指放开的那一点接着走，而不是跳回零点重放。
pub struct BottomSheetState {
    open: bool,
    /// 松手之后停在哪一档（`anchors` 的下标）。开着且没在拖时，这一档就是弹簧的目标。
    anchor: usize,
    /// 这一次「抬起来」的序号，用来判同屏好几块里哪一块在最上面（见 [`dismiss_top`]）。
    order: u64,
    dragging: bool,
    grab: Option<Grab>,
    /// 拖动期间手指给的露出高；只有 `dragging` 为真时才是弹簧的目标。
    shown: f32,
    /// 手指向下的速度，逻辑像素/秒。只在判甩的那一帧用。
    velocity: f32,
    last: Option<(Instant, f32)>,
    /// 弹层自己的焦点位：返回键的绑定挂在 `base::Sheet` 的 `"Sheet"` 键上下文里，
    /// 焦点不在这一层就接不到那一下。
    focus: FocusHandle,
    /// 进弹层之前的焦点，关掉时还回去。
    previous_focus: Option<FocusHandle>,
    /// 该在下一个绘制帧把焦点认领进来。`show()` 不直接挪焦点，原因见 [`BottomSheet::render`]。
    claim_focus: bool,
    /// 该在下一个绘制帧把焦点还回去。
    restore_focus: bool,
    /// 上一帧这块弹层挂没挂载（由元素每帧回写）。给调用方判「这一帧可以不给内容」——
    /// 见 [`Self::is_idle`]。
    mounted: bool,
}

impl BottomSheetState {
    /// 起始是收着的：第一次 [`Self::show`] 才把弹簧从 0 抬到最高那档。
    ///
    /// 建起来就登记进 [`OpenSheets`]（那一格为什么存在写在它自己头上）。登记要拿自己的
    /// `Entity`，所以这一条收 `&mut Context<Self>` 而不是 `&mut App` —— 六处调用点都是在
    /// `cx.new(|cx| …)` 里递进来的那一份，签名一改它们一字不动。
    pub fn closed(cx: &mut Context<Self>) -> Self {
        register(cx);
        Self {
            open: false,
            anchor: 0,
            order: 0,
            dragging: false,
            grab: None,
            shown: 0.0,
            velocity: 0.0,
            last: None,
            focus: cx.focus_handle(),
            previous_focus: None,
            claim_focus: false,
            restore_focus: false,
            mounted: false,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// 收好了、不在动、上一帧也没挂载 —— 调用方这一帧可以**不给内容**。
    ///
    /// 这块弹层的元素必须每帧构造（弹簧在里面采样），但它的**内容**常常不便宜：阅读页那
    /// 一块要给整棵目录树建行、重造尺寸表。用 `is_open()` 判是不够的 —— 退场那一程
    /// `is_open()` 已经是 `false` 而面板还浮在屏幕上，那时断掉内容就等于看着它空着沉下去。
    /// 这一条把「还在画」也算进来，正是为了退场那一程照给、彻底停稳了才省。
    pub fn is_idle(&self) -> bool {
        !self.open && !self.dragging && !self.mounted
    }

    /// 这一手势是否已经被弹层认领（走过 slop 那道门）。内容里的点击不该被抢，
    /// 这条是给那个判断留的可观察口。
    pub fn is_dragging(&self) -> bool {
        self.dragging
    }

    /// 现在停在哪一档。
    pub fn anchor(&self) -> usize {
        self.anchor
    }

    /// 抬起来，停在最高那档。正收在半途时接着从当前位置往上走，不回到零点重放。
    pub fn show(&mut self, cx: &mut Context<Self>) {
        self.open = true;
        self.anchor = 0;
        self.order = next_order(cx);
        self.claim_focus = true;
        self.reset_gesture();
        cx.notify();
    }

    /// 收下去 —— 调用方主动关的，不发 [`BottomSheetEvent::Dismissed`]。
    pub fn hide(&mut self, cx: &mut Context<Self>) {
        self.close(cx, false);
    }

    /// 系统返回 / 边缘滑动收这一层：**算读者关的**，发 [`BottomSheetEvent::Dismissed`]。
    ///
    /// 与 [`Self::hide`] 的差别只在那一发事件：调用方靠它把「这一屏的意图」清掉
    /// （架面三块、笔记那一块都订阅着）。不清的话下一次渲染 `sync_sheet` 看见意图还在，
    /// 会当场把刚收掉的卡再抬起来一次。
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.close(cx, true);
    }

    /// 关这一层：`by_user` 那条路（下拉过头、向下甩、点遮罩、返回键）额外发事件。
    fn close(&mut self, cx: &mut Context<Self>, by_user: bool) {
        if !self.open {
            return;
        }
        self.open = false;
        self.anchor = 0;
        // 焦点不在这里还：挪焦点要 `&mut Window`，而这条路上常常没有窗口 —— 系统返回走的
        // 是 `App::on_action`，那份派发里窗口是借着的，进不去第二次。留个标记，下一帧由
        // [`BottomSheet::render`] 去还（`Window::focus` 不挑绘制阶段）。
        self.restore_focus = self.previous_focus.is_some();
        self.reset_gesture();
        if by_user {
            cx.emit(BottomSheetEvent::Dismissed);
        }
        cx.notify();
    }

    /// 手指落在可拖区：记下起手，但**还不认领** —— 一次点击不该被当成拖动。
    fn touch_started(&mut self, y: f32, base: f32) {
        self.grab = Some(Grab {
            y,
            base,
            claimed: false,
        });
        self.last = None;
        self.velocity = 0.0;
    }

    /// 手指移动。返回 `true` = 这一手势已经归弹层，调用方该 `prevent_default()`
    /// 并止住传播，别让底下的正文跟着滚。
    fn touch_moved(&mut self, y: f32, top: f32, now: Instant) -> bool {
        let Some(grab) = self.grab else { return false };
        if !grab.claimed {
            if (y - grab.y).abs() < DRAG_SLOP {
                return false;
            }
            self.grab = Some(Grab {
                claimed: true,
                ..grab
            });
            self.dragging = true;
        }
        self.shown = follow(grab.base, y - grab.y, top);
        // 同一个时刻 tick 上落了两下（宿主测试的假时钟就是这么省）：速度维持上一个已知值，
        // 而不是拿 0 除。
        if let Some((at, at_y)) = self.last {
            let delta = now.saturating_duration_since(at);
            if !delta.is_zero() {
                self.velocity = if delta > VELOCITY_STALE {
                    0.0
                } else {
                    (y - at_y) / delta.as_secs_f32()
                };
            }
        }
        self.last = Some((now, y));
        self.dragging
    }

    /// 松手：判一次 [`settle`]，该换档就换档，该关就关。返回这一手势**是否归弹层**
    /// （认领过才算）—— 调用方据此决定要不要逼一帧，让弹簧从手指放开那一点接着走。
    fn touch_ended(&mut self, anchors: &[f32], cx: &mut Context<Self>) -> bool {
        let dragging = self.dragging;
        let (shown, velocity) = (self.shown, self.velocity);
        self.reset_gesture();
        if !dragging {
            return false;
        }
        match settle(shown, velocity, anchors) {
            Stop::Dismiss => self.close(cx, true),
            Stop::At(anchor) => {
                self.anchor = anchor;
                cx.notify();
            }
        }
        true
    }

    fn reset_gesture(&mut self) {
        self.dragging = false;
        self.grab = None;
        self.last = None;
        self.velocity = 0.0;
        self.shown = 0.0;
    }
}

impl EventEmitter<BottomSheetEvent> for BottomSheetState {}

/// 一块底部弹层。内容用 `.child()` 交进来，开合与停靠档握在 [`BottomSheetState`] 里。
#[derive(IntoElement)]
pub struct BottomSheet {
    state: Entity<BottomSheetState>,
    /// 弹簧状态的本子名：同屏有两块弹层时它们各走各的高度。
    key: SharedString,
    /// 最高那档占视口高的比例。
    expanded: f32,
    /// 更低的那一档。`None` = 只有一档（Material 的 `skipPartiallyExpanded`）。
    peek: Option<f32>,
    header: Option<AnyElement>,
    children: Vec<AnyElement>,
    surface_draggable: bool,
    overlay_closable: bool,
    style: StyleRefinement,
}

impl BottomSheet {
    /// `key` 是这块弹层的名，用于弹簧状态的存放，也用于测试里找元素。
    pub fn new(state: &Entity<BottomSheetState>, key: impl Into<SharedString>) -> Self {
        Self {
            state: state.clone(),
            key: key.into(),
            expanded: DEFAULT_EXPANDED,
            peek: None,
            header: None,
            children: Vec::new(),
            surface_draggable: false,
            overlay_closable: true,
            style: StyleRefinement::default(),
        }
    }

    /// 展开那一档占视口高的比例，夹在 5%–95%。
    pub fn expanded(mut self, fraction: f32) -> Self {
        self.expanded = fraction.clamp(0.05, 0.95);
        self
    }

    /// 更低的那一档（半屏态），夹在 5%–95%。比 [`Self::expanded`] 还高时当没设过。
    pub fn peek(mut self, fraction: f32) -> Self {
        self.peek = Some(fraction.clamp(0.05, 0.95));
        self
    }

    /// 钉在把手下面、**不跟着内容滚**的那一块（Material 的 sheet title / subheader）。
    ///
    /// 这一槽不加任何内缩：本仓库的横向刻度（目录那 15 像素的栏宽、书头的对齐边）都是在
    /// 真机上逐行数出来的，住在调用侧。组件再垫一层 `px_4` 就会把它们整体推歪 16 像素。
    pub fn header(mut self, header: impl IntoElement) -> Self {
        self.header = Some(header.into_any_element());
        self
    }

    /// 放开整块面板拖拽。内容里有滚动列表时别开 —— 两者抢同一根手指，
    /// 而「滚到顶了再接着往下拽」这一步判不了。
    ///
    /// 内容里**有可点的东西时也别开**：触摸上整块认领拖动手势（见 [`DragRegistration`]
    /// 那条「Started 就 prevent_default」）会把落在内容上的那一下点击一起吃掉 —— gpui 的
    /// 触摸只有「归拖动」或「归滚动/点击」两种去向，不能两者都要。可点的内容请走默认的
    /// 把手带。
    pub fn surface_draggable(mut self, draggable: bool) -> Self {
        self.surface_draggable = draggable;
        self
    }

    /// 点遮罩能不能关掉，默认 `true`。
    pub fn overlay_closable(mut self, closable: bool) -> Self {
        self.overlay_closable = closable;
        self
    }
}

impl ParentElement for BottomSheet {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl Styled for BottomSheet {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for BottomSheet {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let list: Rc<[f32]> = anchors(
            f32::from(window.viewport_size().height),
            self.expanded,
            self.peek,
        )
        .into();
        let top = list.first().copied().unwrap_or(0.0);

        let gesture = self.state.read(cx);
        let (open, dragging, anchor, drag_shown) = (
            gesture.open,
            gesture.dragging,
            gesture.anchor,
            gesture.shown,
        );
        let focus = gesture.focus.clone();
        let (claim, restore) = (gesture.claim_focus, gesture.restore_focus);

        // 焦点这两步在绘制帧里做，不在 `show()` / `hide()` 里做：挪焦点要 `&mut Window`，
        // 而调那两个方法的地方常常没有窗口 —— 系统返回走的是 `App::on_action`，那份派发里
        // 窗口是借着的，进不去第二次。`Window::focus` 不挑绘制阶段，在这儿叫合法。
        if claim && open {
            self.state.update(cx, |state, cx| {
                state.claim_focus = false;
                // 记的是**进层之前**那一位：此刻焦点还在外面（这一帧的 `track_focus` 还没把
                // 自己挂上去），正好就是关掉时要还回去的那个目标。
                if state.previous_focus.is_none() {
                    state.previous_focus = window.focused(cx);
                }
            });
            focus.focus(window, cx);
        }
        if restore && !open {
            let previous = self.state.update(cx, |state, _| {
                state.restore_focus = false;
                state.previous_focus.take()
            });
            // 只有焦点还留在这一层（或层里的某个控件）才还：读者已经点到别处去了，这时候
            // 还焦点就是把他的选择抢回来。
            if let Some(previous) = previous
                && focus.contains_focused(window, cx)
            {
                previous.focus(window, cx);
            }
        }

        // 目标露出高：收到底（关）／跟着手指（拖）／停在那一档（开）。
        let target = if !open {
            0.0
        } else if dragging {
            drag_shown
        } else {
            list[anchor.min(list.len() - 1)]
        };
        let policy = if dragging {
            // 手指已经把它挪到这儿了，弹簧不许再往那儿追 —— 追就是滞后。
            Spring::new(SETTLE).with_travel(false)
        } else {
            // 容差按像素给：默认的 0.001 是归一化 0..1 那一档的量级，用在几百像素的高度上
            // 会为一丝看不见的余震一直要帧。
            Spring::new(SETTLE).with_epsilon(0.5)
        };
        let shown = spring(
            SharedString::from(format!("{}/shown", self.key)),
            target,
            policy,
            window,
            cx,
        );

        // 挂载判露出高不判意图：`hide()` 之后顶边还在往下走，要接着画到收成 0 那一帧。
        let mounted = open || dragging || shown > SETTLED_EPSILON;
        // 回写这一帧的挂载状态，给调用方的 [`BottomSheetState::is_idle`] 用。不调 `notify`：
        // 这一位本来就是这一帧算出来的，再触发一次重画就是自己追自己。
        self.state.update(cx, |state, _| state.mounted = mounted);
        if !mounted {
            return div().into_any_element();
        }

        // 手指的接法要等到**绘制期**才挂得上去（`window.on_mouse_event` 只在那一阶段
        // 合法），所以先把要用的几样存下来，交给下面那块 `canvas` 的绘制回调。
        let drag = DragRegistration {
            state: self.state.clone(),
            open,
            whole: self.surface_draggable,
            anchors: list.clone(),
            shown,
        };
        let scrim = {
            let overlay = cx.theme().overlay;
            gpui_kit::Hsla {
                a: overlay.a * scrim_alpha(1.0, shown, top),
                ..overlay
            }
        };
        let radius = cx.theme().radius_lg;
        // 夹这一段给两种边角兜底：视口忽然变矮时上一帧的露出高会大于当帧的最高档，
        // 而收成 0 那一程的负高度会让整层画不出来。
        let height = shown.clamp(SETTLED_EPSILON, top);

        let surface = v_flex()
            .id("bottom-sheet-surface")
            .test_support()
            .occlude()
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .h(px(height))
            .popover_style(cx)
            .corner_radii(Corners {
                top_left: radius,
                top_right: radius,
                bottom_left: px(0.0),
                bottom_right: px(0.0),
            })
            .refine_style(&self.style)
            .child(
                // 把手那一条带：整条可点不可滚，中间的胶囊只是给人看的。
                div()
                    .w_full()
                    .h(px(HANDLE_BAND))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .w(px(HANDLE_WIDTH))
                            .h(px(HANDLE_HEIGHT))
                            .rounded(px(HANDLE_HEIGHT / 2.0))
                            .bg(cx.theme().border),
                    ),
            )
            .when_some(self.header, |this, header| {
                // 这一槽只做「不跟着滚」，内缩由调用侧给。
                this.child(header)
            })
            .child(
                div().flex_1().min_h_0().overflow_hidden().child(
                    v_flex()
                        // 这一层是**唯一**跟着手指滚的地方：`header` 槽在它外面，才钉得住。
                        // 给个名也是给测试留的口子（路径里有没有它，就是钉住与随滚的区别）。
                        .id("bottom-sheet-scroll")
                        .size_full()
                        .overflow_y_scrollbar()
                        .children(self.children),
                ),
            )
            .child(
                // 面板这一帧真落在哪儿，由绘制期回传：手指的命中区就按这块 bounds 取，
                // 不去推算 anchored 宿主层加窗口内缩的结果。它自己不画任何东西。
                canvas(
                    |bounds, _, _| bounds,
                    |bounds, _, window, _| drag.register(window, bounds),
                )
                .absolute()
                .inset_0(),
            );

        BaseSheet::new(cx)
            .focus_handle(focus)
            .overlay_closable(self.overlay_closable)
            .request_close({
                let state = self.state.clone();
                move |_window, cx| {
                    state.update(cx, |state, cx| state.close(cx, true));
                }
            })
            .on_close(|_, _, _| {})
            // 遮罩得 `occlude`：gpui 把一击发给**每一个** hitbox 含按下点的 handler，
            // 而阅读页的正文 hitbox 是整屏的。不拦的话，点遮罩关掉弹层的同时，底下那一层
            // 也收到这一下 —— 在目录那一屏就是「关掉的同时跳了一章」。
            .overlay(
                div()
                    .occlude()
                    .absolute()
                    .inset_0()
                    .bg(scrim)
                    .into_any_element(),
            )
            .surface(surface)
            .into_any_element()
    }
}

/// 手指要接进哪块状态、按哪一块实测的落点接。从 `render` 交给 `canvas` 的绘制回调。
struct DragRegistration {
    state: Entity<BottomSheetState>,
    /// 收着的那一程（顶边正在往下走）不接新起手：那一程的面板已经在往屏幕底退了。
    open: bool,
    whole: bool,
    anchors: Rc<[f32]>,
    /// 起手那一刻的露出高基准，来自这一帧的弹簧采样。
    shown: f32,
}

impl DragRegistration {
    /// 把这一帧的手指接进状态。触摸走 `TouchDragEvent`（Android 上每一根手指先被当成
    /// 拖动给出，之后才分给点击/长按/滚动），鼠标走 down/move/up 三连（桌面）。
    fn register(self, window: &mut Window, surface: Bounds<Pixels>) {
        if !self.open {
            return;
        }
        let band = drag_band(surface, self.whole);
        let anchors = self.anchors;
        let top = anchors.first().copied().unwrap_or(0.0);
        let shown = self.shown;

        let touch_state = self.state.clone();
        let touch_anchors = anchors.clone();
        window.on_mouse_event(move |event: &TouchDragEvent, phase, window, cx| {
            if !phase.bubble() {
                return;
            }
            let y = f32::from(event.position.y);
            let now = cx.background_executor().now();
            match event.phase {
                TouchPhase::Started => {
                    // **在这一下就认领**（`prevent_default`），不能等 Moved 才认。手势识别器
                    // 只在 Started 问一遍「这根手指归谁」：没在那一刻接下的，它随后就把它判成
                    // 内容滚动，`TouchDragEvent` 的 Moved/Ended **再也不会发过来** —— 于是
                    // 「拖到一半才 prevent_default」这条路在真机上永远走不到（桌面鼠标不走
                    // 识别器，所以宿主测试直发 `TouchDragEvent` 时看着是好的，一到手机上就
                    // 只剩把手拖不动）。把手带里没有可点的东西，先接下它不会吃掉谁的点击；
                    // `default_prevented` 那道门留给压在更上面的一层（「⋯」那块弹层）。
                    if band.contains(&event.position) && !window.default_prevented() {
                        window.prevent_default();
                        touch_state.update(cx, |state, _| state.touch_started(y, shown));
                    }
                }
                TouchPhase::Moved => {
                    let claimed = touch_state.update(cx, |state, _| state.touch_moved(y, top, now));
                    if claimed {
                        window.prevent_default();
                        cx.stop_propagation();
                        window.refresh();
                    }
                }
                TouchPhase::Ended | TouchPhase::Cancelled => {
                    let ours =
                        touch_state.update(cx, |state, cx| state.touch_ended(&touch_anchors, cx));
                    if ours {
                        // 换档与下沉都要弹簧接着走，而它只在被采样时才要帧：这儿逼一帧，
                        // 把手指放开那一点交回给它。
                        window.refresh();
                    }
                }
            }
        });

        let down_state = self.state.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, _, cx| {
            if !phase.capture() || event.button != MouseButton::Left {
                return;
            }
            if band.contains(&event.position) {
                down_state.update(cx, |state, _| {
                    state.touch_started(f32::from(event.position.y), shown)
                });
            }
        });

        let move_state = self.state.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, _, window, cx| {
            if event.pressed_button != Some(MouseButton::Left) {
                return;
            }
            let now = cx.background_executor().now();
            let claimed = move_state.update(cx, |state, _| {
                state.touch_moved(f32::from(event.position.y), top, now)
            });
            if claimed {
                window.prevent_default();
                cx.stop_propagation();
                window.refresh();
            }
        });

        let up_state = self.state.clone();
        window.on_mouse_event(move |_: &MouseUpEvent, phase, window, cx| {
            if !phase.bubble() {
                return;
            }
            if up_state.update(cx, |state, cx| state.touch_ended(&anchors, cx)) {
                window.refresh();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    //! 判据为什么是那个数都写在常数注释里，这一层只钉「几条判据的先后」与临界点。
    //! 点名导入而不 `use super::*`：`gpui_kit` 根把 `gpui` 整个转了出来，里面有一个
    //! 叫 `test` 的属性宏，glob 会让下面的 `#[test]` 递归展开成它。

    use super::{
        HANDLE_BAND, Stop, anchors, drag_band, follow, nearest_anchor, scrim_alpha, settle,
    };
    use gpui_kit::{Bounds, point, px, size};

    /// 视口 600：最高档 540（0.9），半屏档 270（0.45）。
    const TOP: f32 = 540.0;
    const PEEK: f32 = 270.0;
    const ONE: &[f32] = &[TOP];
    const TWO: &[f32] = &[TOP, PEEK];

    /// 只有一档时，旧那条「过半就关」一个字没变。
    #[test]
    fn one_anchor_keeps_the_half_rule() {
        assert_eq!(settle(TOP / 2.0, 0.0, ONE), Stop::Dismiss);
        assert_eq!(settle(TOP / 2.0 + 1.0, 0.0, ONE), Stop::At(0));
    }

    /// 两档时判的是**最低那档**的一半：从半屏再往下拖过 135 才关。
    ///
    /// 这条排在「取最近档」之前是有原因的：从全屏拖向半屏的途中露出高一路都会小于 540，
    /// 若先判最近档就永远关不掉 —— 而读者把最后那一段当成「我要关层」，不是「我想换档」。
    #[test]
    fn the_dismiss_line_is_half_the_lowest_anchor() {
        assert_eq!(settle(PEEK / 2.0, 0.0, TWO), Stop::Dismiss);
        assert_eq!(settle(PEEK / 2.0 + 1.0, 0.0, TWO), Stop::At(1));
        assert_eq!(settle(PEEK + 10.0, 0.0, TWO), Stop::At(1));
    }

    /// 不甩手就取最近那一档；两档一样远时取更高的那档（不许面板自己往回缩）。
    #[test]
    fn the_nearest_anchor_wins_without_a_fling() {
        assert_eq!(settle(500.0, 0.0, TWO), Stop::At(0));
        assert_eq!(settle(300.0, 0.0, TWO), Stop::At(1));
        assert_eq!(settle(PEEK + (TOP - PEEK) / 2.0, 0.0, TWO), Stop::At(0));
        assert_eq!(nearest_anchor(10.0, TWO), 1);
    }

    /// 向下甩走一档；已经在最低那档就顺手关掉。
    #[test]
    fn a_downward_fling_steps_one_anchor_down() {
        assert_eq!(settle(500.0, 900.0, TWO), Stop::At(1));
        assert_eq!(settle(PEEK, 900.0, TWO), Stop::Dismiss);
    }

    /// 向上甩走一档；已经在最高那档就停住，不许越过顶档。
    #[test]
    fn an_upward_fling_stops_at_the_top_anchor() {
        assert_eq!(settle(300.0, -900.0, TWO), Stop::At(0));
        assert_eq!(settle(TOP, -900.0, TWO), Stop::At(0));
    }

    /// 一档都没有（视口还没量到）判关，而不是把 0 当成锚点停在屏幕底。
    #[test]
    fn no_anchor_means_no_stop() {
        assert_eq!(settle(100.0, 0.0, &[]), Stop::Dismiss);
        assert_eq!(settle(0.0, 0.0, &[0.0]), Stop::Dismiss);
    }

    /// `peek` 不低于 `expanded` 时只剩一档：两档一样高会让「最低那档的一半」那条判据
    /// 无的放矢。
    #[test]
    fn an_peek_that_is_not_lower_leaves_one_anchor() {
        assert_eq!(anchors(600.0, 0.9, Some(0.9)), vec![TOP]);
        assert_eq!(anchors(600.0, 0.9, Some(0.45)), vec![TOP, PEEK]);
        assert_eq!(anchors(600.0, 0.9, None), vec![TOP]);
    }

    /// 跟随是反的：手指往下拖（增量正）露出高变小，两头都夹住。
    #[test]
    fn the_sheet_follows_the_finger_between_the_two_ends() {
        assert_eq!(follow(TOP, 60.0, TOP), TOP - 60.0);
        assert_eq!(follow(TOP, -60.0, TOP), TOP);
        assert_eq!(follow(PEEK, PEEK * 3.0, TOP), 0.0);
        assert_eq!(follow(PEEK, -40.0, TOP), PEEK + 40.0);
    }

    /// 遮罩跟着露出高升降、不外溢：半屏态拿到的是半档灰，正文还认得出。
    #[test]
    fn the_scrim_rides_the_sheet_height() {
        assert_eq!(scrim_alpha(0.6, TOP, TOP), 0.6);
        assert_eq!(scrim_alpha(0.6, 0.0, TOP), 0.0);
        assert_eq!(scrim_alpha(0.6, TOP * 3.0, TOP), 0.6);
        assert!(scrim_alpha(0.6, PEEK, TOP) < scrim_alpha(0.6, TOP, TOP));
        assert_eq!(scrim_alpha(0.6, 0.0, 0.0), 0.6);
    }

    /// 命中区取自面板**画出来**的那一块：带子是它顶上的一条，`whole` 时整块都算。
    #[test]
    fn the_band_is_the_top_strip_of_the_painted_surface() {
        let full = Bounds {
            origin: point(px(0.0), px(60.0)),
            size: size(px(400.0), px(TOP)),
        };
        let band = drag_band(full, false);
        assert_eq!(f32::from(band.origin.y), 60.0);
        assert_eq!(f32::from(band.size.height), HANDLE_BAND);
        assert_eq!(f32::from(band.size.width), 400.0);
        assert_eq!(drag_band(full, true), full);

        // 换到半屏档时顶边一起掉下来，带子还贴在面板顶上：手指一路攥得住。
        let peeked = Bounds {
            origin: point(px(0.0), px(330.0)),
            size: size(px(400.0), px(PEEK)),
        };
        assert_eq!(f32::from(drag_band(peeked, false).origin.y), 330.0);
    }
}

#[cfg(all(test, not(mobile)))]
mod view_tests {
    //! 钉的是「挂没挂载」「停哪一档」「手指归谁」这三件 —— 它们在桌面就能测准：
    //! `try_find` 看得见元素在不在树里、量得到顶边在哪一像素，`TouchDragEvent` 走的是
    //! 与真机同一条 `dispatch_event` 入口。
    //!
    //! 返回键那一下不在这里钉：它归外壳（`AppShell::back` → [`dismiss_top`]），
    //! 因为弹层自己那条 `base::Sheet` 的 `"Sheet"` 上下文绑定**赢不了**外壳那条无上下文
    //! 的 `escape → Back`（实测：只挂 Cancel 时这一键能关上弹层；再挂一条无上下文的
    //! escape，Cancel 一次都轮不到）。收哪一块的判据（最后抬起来的先收）钉在
    //! `back_takes_the_sheet_that_was_raised_last`。

    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt as _;
    // `test_support` 得在这儿再导入一次：内层 `mod` 不继承外层文件的 use。
    use gpui_kit::TestSupportExt as _;
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Context, Entity, InputEvent as _,
        InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseUpEvent,
        ParentElement as _, Pixels, PlatformInput, Point, Render, ScrollDelta,
        StatefulInteractiveElement as _, Styled as _, Subscription, TestAppContext, TouchDragEvent,
        TouchPhase, Window, div, point, px, size,
    };

    use super::{BottomSheet, BottomSheetEvent, BottomSheetState, dismiss_top};

    /// 视口 400×600、`expanded 0.9 / peek 0.45`：全屏档顶边在 60（露出 540），
    /// 半屏档顶边在 330（露出 270）。把手是面板顶上那 36 像素。
    const HANDLE_FULL: Point<Pixels> = point(px(200.0), px(78.0));
    const HANDLE_PEEKED: Point<Pixels> = point(px(200.0), px(348.0));
    const BODY_FULL: Point<Pixels> = point(px(200.0), px(450.0));
    /// 内容那一格**自己**的落点：面板顶边 60 + 把手带 36 + header 30 = 126 起，那格高 900
    /// （比滚动区还高，才有得滚），取靠上那一行（186）才真落在带 handler 的元素上。
    const BODY_CELL: Point<Pixels> = point(px(200.0), px(186.0));
    const SCRIM_AT: Point<Pixels> = point(px(200.0), px(30.0));
    /// 一帧的模拟时长：40 帧 @16ms 足够一根 280ms 的弹簧走完进出。
    const FRAME: Duration = Duration::from_millis(16);

    struct SheetHost {
        sheet: Entity<BottomSheetState>,
        dismissals: Rc<Cell<usize>>,
        /// 落在弹层内容那一格上的一击次数：用来证明遮罩没把层里自己的点击一起挡掉。
        body_taps: Rc<Cell<usize>>,
        /// 订阅必须活着事件才来：`Subscription` 一 drop 就退订。
        _watch: Subscription,
    }

    impl SheetHost {
        fn boot(cx: &mut Context<Self>) -> Self {
            let sheet = cx.new(BottomSheetState::closed);
            let dismissals: Rc<Cell<usize>> = Rc::default();
            let body_taps: Rc<Cell<usize>> = Rc::default();
            let sink = dismissals.clone();
            let _watch = cx.subscribe(&sheet, move |_, _, event: &BottomSheetEvent, _| {
                if matches!(event, BottomSheetEvent::Dismissed) {
                    sink.set(sink.get() + 1);
                }
            });
            Self {
                sheet,
                dismissals,
                body_taps,
                _watch,
            }
        }
    }

    impl Render for SheetHost {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let taps = self.body_taps.clone();
            // 弹层每帧都构造（收着时也构造）：弹簧的采样在这一层里，只在开着时构造
            // 会把退场动画连状态一起丢。
            div()
                .size_full()
                .relative()
                .id("sheet-host")
                .child(div().id("page-underneath").size_full())
                .child(
                    BottomSheet::new(&self.sheet, "test-sheet")
                        .expanded(0.9)
                        .peek(0.45)
                        .header(
                            div()
                                .id("sheet-header")
                                .test_support()
                                .h(px(30.0))
                                .child("钉住的那一行"),
                        )
                        .child(
                            div()
                                .id("sheet-body")
                                .test_support()
                                .w_full()
                                .h(px(900.0))
                                .on_click(move |_, _, _| taps.set(taps.get() + 1)),
                        ),
                )
        }
    }

    fn boot(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<SheetHost>) {
        cx.update(gpui_kit::init);
        let mut host = None;
        let handle: AnyWindowHandle = cx
            .open_window(size(px(400.0), px(600.0)), |window, cx| {
                let view = cx.new(SheetHost::boot);
                host = Some(view.clone());
                Root::new(view, window, cx)
            })
            .into();
        (handle, host.expect("测试台应已建起"))
    }

    /// 开或关都走状态自己的方法，**而且不带窗口** —— 这正是系统返回那条路能做的事
    /// （`App::on_action` 的派发里只有 `&mut App`，窗口是借着的）。焦点的认领与归还
    /// 由下一帧的 `render` 补上，这一趟顺带把那条路也走了一遍。
    fn set_open(cx: &mut TestAppContext, host: &Entity<SheetHost>, open: bool) {
        cx.update(|app| {
            host.update(app, |this, cx| {
                this.sheet.update(cx, |state, cx| match open {
                    true => state.show(cx),
                    false => state.hide(cx),
                });
            });
        });
        cx.run_until_parked();
    }

    /// 打开并停在最高那档。
    fn open_full(cx: &mut TestAppContext, handle: AnyWindowHandle, host: &Entity<SheetHost>) {
        set_open(cx, host, true);
        settle_frames(cx, handle);
        assert!(mounted(cx, handle), "前置：打开后该挂着");
        assert_eq!(anchor(cx, host), 0, "前置：该停在最高那档");
    }

    /// 从全屏拖到半屏那档停好，后面的用例都从这个状态起手。
    fn park_at_peek(cx: &mut TestAppContext, handle: AnyWindowHandle, host: &Entity<SheetHost>) {
        open_full(cx, handle, host);
        drag(cx, handle, HANDLE_FULL, point(px(200.0), px(300.0)));
        settle_frames(cx, handle);
        assert_eq!(anchor(cx, host), 1, "前置：该停在半屏那档");
    }

    /// 面板顶边此刻真在窗口坐标里的哪一行（实测，不是推算）。
    fn surface_top(cx: &mut TestAppContext, handle: AnyWindowHandle) -> f32 {
        row_y(cx, handle, "bottom-sheet-surface")
    }

    /// `(header 那一行的顶边, 内容那一格的顶边)`。滚一下之后谁动了，就比这两个数。
    fn sheet_rows(cx: &mut TestAppContext, handle: AnyWindowHandle) -> (f32, f32) {
        (
            row_y(cx, handle, "sheet-header"),
            row_y(cx, handle, "sheet-body"),
        )
    }

    /// 某个挂了名的元素，顶边在窗口坐标的哪一行。找不到就是 `-1.0`（看得见是前提）。
    fn row_y(cx: &mut TestAppContext, handle: AnyWindowHandle, id: &'static str) -> f32 {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window
                .try_find(id)
                .map(|node| f32::from(node.bounds().origin.y))
                .unwrap_or(-1.0)
        })
        .expect("渲染应成功")
    }

    fn is_open(cx: &TestAppContext, host: &Entity<SheetHost>) -> bool {
        host.read_with(cx, |this, cx| this.sheet.read(cx).is_open())
    }

    fn anchor(cx: &TestAppContext, host: &Entity<SheetHost>) -> usize {
        host.read_with(cx, |this, cx| this.sheet.read(cx).anchor())
    }

    fn is_dragging(cx: &TestAppContext, host: &Entity<SheetHost>) -> bool {
        host.read_with(cx, |this, cx| this.sheet.read(cx).is_dragging())
    }

    fn is_idle(cx: &TestAppContext, host: &Entity<SheetHost>) -> bool {
        host.read_with(cx, |this, cx| this.sheet.read(cx).is_idle())
    }

    fn dismissals(cx: &TestAppContext, host: &Entity<SheetHost>) -> usize {
        host.read_with(cx, |this, _| this.dismissals.get())
    }

    fn body_taps(cx: &TestAppContext, host: &Entity<SheetHost>) -> usize {
        host.read_with(cx, |this, _| this.body_taps.get())
    }

    /// 走一帧：先把测试时钟推 16ms 再画。弹簧按 `background_executor().now()` 计步，
    /// 而 `run_until_parked` 只跑事件，不替「没有定时器可等」的那个钟前进。
    fn tick(cx: &mut TestAppContext, handle: AnyWindowHandle) {
        cx.executor().advance_clock(FRAME);
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("渲染应成功");
    }

    fn settle_frames(cx: &mut TestAppContext, handle: AnyWindowHandle) {
        for _ in 0..40 {
            tick(cx, handle);
        }
    }

    /// 这一帧弹层那面板挂没挂在树里。
    fn mounted(cx: &mut TestAppContext, handle: AnyWindowHandle) -> bool {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("bottom-sheet-surface").is_some()
        })
        .expect("渲染应成功")
    }

    fn dispatch(cx: &mut TestAppContext, handle: AnyWindowHandle, event: PlatformInput) {
        cx.update_window(handle, |_, window, cx| {
            window.dispatch_event(event, cx);
        })
        .expect("事件应送达窗口");
        cx.run_until_parked();
    }

    fn touch(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        phase: TouchPhase,
        at: Point<Pixels>,
        from: Point<Pixels>,
    ) {
        dispatch(
            cx,
            handle,
            TouchDragEvent {
                phase,
                start_position: from,
                position: at,
            }
            .to_platform_input(),
        );
    }

    /// 一根手指：落在 `from`，拖到 `to`，抬起。
    fn drag(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        from: Point<Pixels>,
        to: Point<Pixels>,
    ) {
        touch(cx, handle, TouchPhase::Started, from, from);
        touch(cx, handle, TouchPhase::Moved, to, from);
        touch(cx, handle, TouchPhase::Ended, to, from);
    }

    /// 真机那条路：raw `TouchEvent` 交给 gpui 的手势识别器，由它再分派。
    fn raw_touch(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        id: u64,
        phase: TouchPhase,
        at: Point<Pixels>,
    ) {
        use gpui_kit::{InputEvent as _, TouchEvent, TouchId};
        dispatch(
            cx,
            handle,
            TouchEvent {
                id: TouchId(id),
                phase,
                position: at,
                predicted_position: None,
                force: None,
            }
            .to_platform_input(),
        );
    }

    /// 回归门：**真机那条路**下（raw `TouchEvent` → 手势识别器），在把手上起手往下拖，
    /// 面板该换到半屏档。
    ///
    /// 这条与上面那些直发 `TouchDragEvent` 的用例是两回事，别删其一：直发的那条绕过了
    /// gpui 的手势仲裁，所以「Started 没认领 → 后面的 Moved 再也不来」这个真机故障在它
    /// 眼里根本不存在（曾经 21 条全绿而手机上是坏的）。识别器只在 Started 问一遍归属，
    /// 这条门钉的就是「那一刻必须接下」。
    #[gpui_kit::test]
    fn raw_touch_drag_on_the_handle_moves_the_sheet(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        open_full(cx, handle, &host);

        raw_touch(cx, handle, 1, TouchPhase::Started, HANDLE_FULL);
        raw_touch(
            cx,
            handle,
            1,
            TouchPhase::Moved,
            point(px(200.0), px(300.0)),
        );
        raw_touch(
            cx,
            handle,
            1,
            TouchPhase::Ended,
            point(px(200.0), px(300.0)),
        );
        settle_frames(cx, handle);

        assert_eq!(anchor(cx, &host), 1, "真机路径：手柄往下拖该换到半屏档");
    }

    /// 回归门：真机那条路下，在内容区往上滑，内容该跟着滚 —— 与把手带的拖动互不干扰。
    #[gpui_kit::test]
    fn raw_touch_swipe_on_the_body_scrolls_the_content(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        open_full(cx, handle, &host);
        let before = sheet_rows(cx, handle);
        assert!(before.1 >= 0.0, "前置：内容那一格该在树里");

        // 从内容中间往上滑 240。
        raw_touch(cx, handle, 1, TouchPhase::Started, BODY_FULL);
        raw_touch(
            cx,
            handle,
            1,
            TouchPhase::Moved,
            point(px(200.0), px(210.0)),
        );
        raw_touch(
            cx,
            handle,
            1,
            TouchPhase::Ended,
            point(px(200.0), px(210.0)),
        );
        for _ in 0..10 {
            tick(cx, handle);
        }

        let after = sheet_rows(cx, handle);
        assert!(
            after.1 < before.1,
            "真机路径：内容该跟着滑走，实测 {} → {}",
            before.1,
            after.1
        );
    }

    /// 鼠标的一击（桌面上的「点遮罩」走的就是这一条）。
    fn click(cx: &mut TestAppContext, handle: AnyWindowHandle, at: Point<Pixels>) {
        dispatch(
            cx,
            handle,
            MouseDownEvent {
                button: MouseButton::Left,
                position: at,
                ..Default::default()
            }
            .to_platform_input(),
        );
        dispatch(
            cx,
            handle,
            MouseUpEvent {
                button: MouseButton::Left,
                position: at,
                ..Default::default()
            }
            .to_platform_input(),
        );
    }

    /// 回归门：收着时一层都不挂；打开挂上；关掉**先收成 0 再卸载**，不是当场消失。
    #[gpui_kit::test]
    fn it_shrinks_to_zero_before_it_unmounts(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        tick(cx, handle);
        assert!(!mounted(cx, handle), "收着的时候一层都不该挂");

        open_full(cx, handle, &host);
        assert!(
            (surface_top(cx, handle) - 60.0).abs() < 1.0,
            "全屏档的顶边应在 60"
        );

        set_open(cx, &host, false);
        tick(cx, handle);
        assert!(
            mounted(cx, handle),
            "关掉该先收成 0：这一帧就消失的话，`hide()` 等于没有退场动画"
        );
        assert!(
            !is_idle(cx, &host),
            "退场那一程不算 idle —— 调用方按这一位省内容，省早了面板会空着沉下去"
        );
        settle_frames(cx, handle);
        assert!(!mounted(cx, handle), "收成 0 才该卸载");
        assert!(is_idle(cx, &host), "停稳了才算 idle");
        assert_eq!(dismissals(cx, &host), 0, "调用方自己关的不该发 Dismissed");
    }

    /// 两档互换（下）：从全屏拖到半屏，停住、不关 —— 这是「两档可拖」的全部意义。
    #[gpui_kit::test]
    fn dragging_down_to_the_peek_anchor_parks_it_there(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        open_full(cx, handle, &host);

        // 顶边从 60 拖到 300（露出从 540 变 318）：最近档是半屏那档（270）。
        drag(cx, handle, HANDLE_FULL, point(px(200.0), px(300.0)));
        settle_frames(cx, handle);

        assert!(is_open(cx, &host), "换档不该关掉这一层");
        assert_eq!(anchor(cx, &host), 1, "该停在半屏那一档");
        assert!(
            (surface_top(cx, handle) - 330.0).abs() < 1.0,
            "半屏档的顶边应在 330"
        );
        assert_eq!(dismissals(cx, &host), 0);
    }

    /// 从半屏那档再往下拖过半程：这一趟才是关掉。
    #[gpui_kit::test]
    fn pulling_past_the_lowest_anchor_dismisses_it(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        park_at_peek(cx, handle, &host);

        // 露出 270 再往下拽 152 → 118，低于 135（那档的一半）→ 关。
        drag(cx, handle, HANDLE_PEEKED, point(px(200.0), px(500.0)));
        settle_frames(cx, handle);

        assert!(!is_open(cx, &host), "拖过最低那档的一半该关掉");
        assert_eq!(dismissals(cx, &host), 1, "是读者的手做的");
        assert!(!mounted(cx, handle), "从手指放开那一点接着收，动完卸载");
    }

    /// 两档互换（上）：从半屏往上拖回全屏。
    #[gpui_kit::test]
    fn dragging_up_from_the_peek_anchor_returns_to_the_top(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        park_at_peek(cx, handle, &host);

        // 顶边从 330 拖到 200（露出 418）：最近档是全屏（540）。
        drag(cx, handle, HANDLE_PEEKED, point(px(200.0), px(200.0)));
        settle_frames(cx, handle);

        assert_eq!(anchor(cx, &host), 0, "往上拖该回全屏那档");
        assert!(is_open(cx, &host));
        assert!((surface_top(cx, handle) - 60.0).abs() < 1.0);
    }

    /// 回弹门：只拖了一小段就还它回原来那一档 —— 误关比不回弹更难发现。
    #[gpui_kit::test]
    fn a_short_drag_snaps_back_to_the_same_anchor(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        open_full(cx, handle, &host);

        drag(cx, handle, HANDLE_FULL, point(px(200.0), px(158.0)));
        settle_frames(cx, handle);

        assert!(
            is_open(cx, &host),
            "只拖了 80（离半屏档还远）不该换档更不该关"
        );
        assert_eq!(anchor(cx, &host), 0);
        assert_eq!(dismissals(cx, &host), 0);
        assert!(
            (surface_top(cx, handle) - 60.0).abs() < 1.0,
            "回弹途中也该一直挂着，别闪一下"
        );
    }

    /// slop 那道门的两侧都钉住：几个像素的抖动还是一次**点击**（不该抢走内容里的
    /// 那一下），过了 slop 才归弹层。
    #[gpui_kit::test]
    fn a_tap_on_the_handle_is_not_a_drag(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        open_full(cx, handle, &host);

        // 4 像素：还在 slop 里，这一手势仍归点击。
        touch(cx, handle, TouchPhase::Started, HANDLE_FULL, HANDLE_FULL);
        touch(
            cx,
            handle,
            TouchPhase::Moved,
            point(px(200.0), px(82.0)),
            HANDLE_FULL,
        );
        assert!(!is_dragging(cx, &host), "4 像素的抖动不该被弹层认领");

        // 30 像素：过了 slop（10）就该归弹层跟着手指走。
        let moved = point(px(200.0), px(108.0));
        touch(cx, handle, TouchPhase::Moved, moved, HANDLE_FULL);
        assert!(is_dragging(cx, &host), "过了 slop 该由弹层接手");
        touch(cx, handle, TouchPhase::Ended, moved, HANDLE_FULL);

        assert!(is_open(cx, &host), "只拖了 30 不该关");
        assert_eq!(dismissals(cx, &host), 0);
    }

    /// 内容区里的一把长拖归内容（滚动 / 那一格自己接），弹层既不接手也不换档。
    ///
    /// 默认只有把手带认领就是这个意思；`surface_draggable(true)` 才会把整块面板也算上。
    /// 拖的这段必须**足够长**（长到假如被认领就该换档），否则两种行为不断开，
    /// 这条测试就成了摆设。
    #[gpui_kit::test]
    fn a_drag_on_the_body_is_left_to_the_content(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        open_full(cx, handle, &host);

        touch(cx, handle, TouchPhase::Started, BODY_FULL, BODY_FULL);
        let far = point(px(200.0), px(600.0));
        touch(cx, handle, TouchPhase::Moved, far, BODY_FULL);
        assert!(
            !is_dragging(cx, &host),
            "从内容区起手的 150 像素不该被弹层认领"
        );
        touch(cx, handle, TouchPhase::Ended, far, BODY_FULL);

        assert!(is_open(cx, &host), "从内容区起手不该拖走面板");
        assert_eq!(anchor(cx, &host), 0, "也不该顺手换档");
        assert_eq!(dismissals(cx, &host), 0);
    }

    /// 点遮罩（面板之外那一块）关掉它 —— 这一条走的是宿主 `base::Sheet` 的关闭次序。
    #[gpui_kit::test]
    fn tapping_the_scrim_dismisses_the_sheet(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        open_full(cx, handle, &host);

        click(cx, handle, SCRIM_AT);

        assert!(!is_open(cx, &host), "点遮罩该关掉");
        assert_eq!(dismissals(cx, &host), 1, "也是读者的手做的");
    }

    /// 落在面板**里面**的一击归面板：遮罩 `occlude` 掉的是它之前登记的那些 hitbox（比如
    /// 阅读页那整屏的正文层），不该把这一层自己的按钮一起挡掉，也不该顺手关掉这一层。
    ///
    /// 这条是目录迁移的命门：列表每一行、⋯ 那四行都靠这一下才按得动。
    #[gpui_kit::test]
    fn a_click_inside_the_sheet_reaches_the_content(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        open_full(cx, handle, &host);

        // 内容那一格里的一击：元素收得到，这一层不收。
        click(cx, handle, BODY_CELL);
        assert_eq!(body_taps(cx, &host), 1, "落在层里的那一击该到内容元素手上");
        assert!(is_open(cx, &host), "点自己面板上的东西不该关掉");
        assert_eq!(dismissals(cx, &host), 0);

        // 把手带里的一击（没有移动）：同样只是点了一下。
        click(cx, handle, HANDLE_FULL);
        assert!(is_open(cx, &host), "点把手不带移动也不该关掉");
        assert_eq!(dismissals(cx, &host), 0);
    }

    /// `header` 那一槽在滚动区**外面**：目录靠它让「已读 N / M + 搜索 + ⋯」那一行永远够得着。
    ///
    /// 判据是「滚一下之后谁动了」：内容跟着走、header 原地不动，才叫钉住。光看落点分不清
    /// 「钉住」与「刚好排在最上面」—— 后者滚到列表底下就没了。
    #[gpui_kit::test]
    fn the_header_stays_while_the_content_scrolls(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        open_full(cx, handle, &host);
        let before = sheet_rows(cx, handle);
        // 前置：两个元素都得找得到。少了这一条，「header 原地不动」会因为两边都是 -1.0
        // 而空过 —— 找不到元素的测试比红掉的测试更坏。
        assert!(
            before.0 >= 0.0 && before.1 >= 0.0,
            "前置：header 与内容那一格都该在树里，拿到 {before:?}"
        );

        // 向下滚 240：gpui 的约定是「向下为负」（与 `reader/view/tests.rs` 那条滚轮助手同一条）。
        cx.update_window(handle, |_, window, cx| {
            window.scroll(
                "sheet-body",
                ScrollDelta::Pixels(point(px(0.0), px(-240.0))),
                cx,
            );
            window.render_frame(cx);
        })
        .expect("滚动应送达");
        cx.run_until_parked();
        let after = sheet_rows(cx, handle);

        // 先证明两个都找得到：`row_y` 找不到时给 -1.0，两个 -1.0 相等会让上面那条
        // 「原地不动」白过。
        assert!(
            before.0 >= 0.0 && before.1 >= 0.0,
            "前置：header 与内容都该在树里，实测 {before:?}"
        );
        assert_eq!(after.0, before.0, "header 该原地不动（{}）", before.0);
        assert!(
            after.1 < before.1,
            "内容该跟着滚走，实测 {} → {}",
            before.1,
            after.1
        );
    }

    /// 关一件本来就关着的事不许再 announce —— `Dismissed` 只该有一条来源。
    #[gpui_kit::test]
    fn hiding_a_closed_sheet_is_quiet(cx: &mut TestAppContext) {
        let (_handle, host) = boot(cx);
        set_open(cx, &host, false);
        assert_eq!(dismissals(cx, &host), 0);
        assert!(!is_open(cx, &host));
    }

    /// 返回键收的是**最后抬起来**的那一块，不是先登记的那一块。
    ///
    /// 同屏可以有好几块弹层各握一份状态（架面上建组 / 导入 / 扫描三块，启动时一起登记），
    /// 读者关掉的必须是他刚看见的那一块。挑「登记最早」会收掉一块读者看不见的，
    /// 而屏幕上那一块纹丝不动 —— 表现为「按返回没反应」。
    #[gpui_kit::test]
    fn back_takes_the_sheet_that_was_raised_last(cx: &mut TestAppContext) {
        use std::cell::Cell;
        use std::rc::Rc;

        let (_handle, host) = boot(cx);
        let first = host.read_with(cx, |this, _| this.sheet.clone());
        let second = cx.update(|cx| cx.new(BottomSheetState::closed));
        let second_dismissals = Rc::new(Cell::new(0));
        let sink = second_dismissals.clone();
        let _watch = cx.update(|cx| {
            cx.subscribe(&second, move |_, _: &BottomSheetEvent, _| {
                sink.set(sink.get() + 1)
            })
        });

        cx.update(|cx| first.update(cx, |sheet, cx| sheet.show(cx)));
        cx.update(|cx| second.update(cx, |sheet, cx| sheet.show(cx)));

        assert!(cx.update(dismiss_top), "开着一块，返回就该收它");
        assert!(
            !second.read_with(cx, |sheet, _| sheet.is_open()),
            "最后抬起来的那一块先收"
        );
        assert!(
            first.read_with(cx, |sheet, _| sheet.is_open()),
            "先抬起来的那一块还在底下，不该跟着一起收"
        );
        assert_eq!(
            second_dismissals.get(),
            1,
            "收的是读者按的那一下：要发 Dismissed"
        );

        assert!(cx.update(dismiss_top), "再按一次收底下那一块");
        assert!(!first.read_with(cx, |sheet, _| sheet.is_open()));
        assert!(!cx.update(dismiss_top), "一块都不开时不许空报");
    }
}
