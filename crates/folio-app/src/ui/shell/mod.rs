//! 应用外壳：自适应导航（Material 3）+ 每个目的地一个返回栈 + 盖在最上面的一本打开的书。
//!
//! 这是两端**唯一**的根视图。它只关心四件事：当前是哪个目的地、导航栏长什么样、栈顶是哪个
//! 页面、有没有一本开着读的书。平台特有的东西（安全区、输入法、正文字体）在 `super::host`，
//! 阅读页的两种摆法在 `super::features`，外壳对两者都只给数、不写 `cfg(mobile)`。
//!
//! 路由用 gpui-base 的 [`NavStack`]：每个目的地一条栈，系统返回 = 当前栈 pop。
//!
//! **书不在那条栈上**：阅读层是根 div 的最后一个子元素，`inset_0` 盖住导航栏与架面。同一只
//! 容器装两张页面时，导航栏收回来那一下必然挤到「退场还没走完」的那一张 —— 正文按窗口量、
//! 干净态那两条带按容器贴，跳出去的就是那条进度带。分成两层之后，架面的盒子从头到尾不因读书
//! 而变，进出的动画里再没有东西可跳（取舍见 [`ReadingLayer`]）。

pub mod navigation;

use std::time::Duration;

use folio_core::Library;
use folio_core::infra::config::Appearance;
use gpui_kit::base::motion::{
    Easing, Presence, PresencePhase, PresenceSample, Transition, TransitionId,
};
use gpui_kit::base::{NavMotion, NavOperation, NavPage, NavStack, NavStackEvent, NavStackState};
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AnyView, App, AppContext as _, Context, ElementId, Entity, InteractiveElement as _,
    IntoElement, KeyBinding, ParentElement, Render, SharedString, Styled, Subscription, Task,
    Window, actions, div, px,
};

use crate::ui::features::notes::NotesView;
use crate::ui::features::reader::{ReadingPage, Reserve};
use crate::ui::features::settings::{CompactSettings, SettingsPage, SettingsView};
use crate::ui::features::shelf::{ShelfEvent, ShelfView};
use crate::ui::features::stats::{StatsEvent, StatsView};
use crate::ui::features::welcome::WelcomeView;
use crate::ui::host;
use crate::ui::shared::config_model::ConfigHolder;
use crate::ui::shared::theme;
use crate::ui::widgets::hairline;
use navigation::{Destination, SizeClass, navigation};

#[cfg(not(mobile))]
use gpui_kit::TestSupportExt as _;

// 系统返回。Android 上 gpui-mobile 把 `AKEYCODE_BACK`（含边缘滑动）与
// `OnBackInvokedCallback` 都翻成 **escape**，所以绑住 escape 就接住了返回。
//
// `PageNext` / `PagePrev` 是音量键那两条（见 [`VolumeKey`]）：fork 把
// `AKEYCODE_VOLUME_UP/DOWN` 翻成 `volumeup` / `volumedown` 两个键名，绑定名不能写成
// 带连字符的 `volume-up` —— gpui 的 `Keystroke::parse` 会把连字符前面那段当修饰键读。
actions!(folio_shell, [Back, PageNext, PagePrev]);

/// 一次音量键按下该去哪儿。
///
/// 判据只有一条，而它两边都错不起：有一本书正在被读时音量键是翻页，别处必须把这一按
/// 还给系统去调音量（不然书架、设置里音量键直接成了死键）。抽成纯函数是因为翻页要走
/// 阅读页、调音量要走 JNI，宿主测试里两样都摸不到，只有这条分流能被直接断言。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum VolumeKey {
    /// 按读书顺序走这么多页（`+1` = 往后一页）。
    Turn(isize),
    /// 交给系统调音量，`up` 是哪一边。
    SystemVolume { up: bool },
}

impl VolumeKey {
    /// 「音量上 = 往后一页」是拍的口径，改它只改这一行。RTL 不反过来：页号本来就按
    /// 阅读序排（左右分区那套换算在 `reader::chrome::zone` 里），所以 +1 在两种方向下
    /// 都是「往后一页」。
    pub(crate) fn of(reading_open: bool, up: bool) -> Self {
        if reading_open {
            Self::Turn(if up { 1 } else { -1 })
        } else {
            Self::SystemVolume { up }
        }
    }
}

/// 系统级那一层要不要沉浸。**有一本书挂着就沉浸**，不分档位：桌面上是窗口真全屏，Android 上
/// 是把状态栏与导航栏交给系统藏起来。退场那一段也算挂着 —— 读者按完返回，动画还在书面上走，
/// 中途把状态栏抬起来会当场挤一次容器。
///
/// 应用内那条导航栏**不再跟着书**：阅读层盖在它之上（见 [`ReadingLayer`]），藏着露着对读者是
/// 同一件事，而收起它会挤动架面的盒子 —— 少一处挤，就少一处可跳的地方。双页对开要的那 260
/// 也照样拿到手：层是不透明的整屏，本来就把侧栏盖住了。
///
/// 抽成自由函数是因为宿主测试进不到 Compact 档 —— [`SizeClass::of`] 在桌面把窄窗口
/// 夹到 `Medium`，只有真机能长出底栏。
pub fn immersive_nav(_size: SizeClass, reading_mounted: bool) -> bool {
    reading_mounted
}

/// 报给正文的「不属于它的那一段」。两个轴都带着：正文自己看哪一轴非零（宽那一轴现在只由
/// 目录那条左侧栏占，见 `reader::page` 里把 `rail` 加回去那一步）。
///
/// 只剩安全区两条：宽恒为 0（阅读层 `inset_0` 盖满整窗，左右两条边没人占），高是状态栏那一条，
/// 手机上再加手势条那一条 —— 那是系统的地盘不是应用 chrome，沉浸态把它交给系统藏起来时这两个
/// 数跟着归零。阅读页量的是**窗口**高，少扣一条它就会把正文盒算高几十逻辑像素，多出来的那一段
/// 落在页脚带下面被裁掉，于是章尾那几行永远滚不进视野。
pub fn reading_reserve(size: SizeClass, safe: &host::Insets) -> Reserve {
    Reserve {
        width: 0.0,
        height: safe.top + if size.is_compact() { safe.bottom } else { 0.0 },
    }
}

/// 一次 push / pop 走这么久。与底部弹层那根弹簧的响应时间同一档（`widgets::bottom_sheet`），
/// 两处不同步的话，同一只手在两屏之间会感到两种快慢。
///
/// `pub(crate)` 只因为门的测试要按这一个数推进时钟（正文翻页的动画门同用），不是给谁
/// 另起一个时长用的口子 —— 那个口子是 [`nav_transition`]，全仓只有它取值。
pub(crate) const NAV_TRANSITION: Duration = Duration::from_millis(280);
/// 一次进出的时钟：栈里的每一页与阅读层自己都走这一口，两端不该有两种快慢。
///
/// 正文翻页共用**这一口**（`reader/view.rs` 的 `turn_frame`）：同一只手在同一屏里换两张纸，
/// 不该比换一整屏还慢，而时长这个数一旦有两个家，改一处另一处就悄悄不一致了。
///
/// 减动效那一档给零时长 —— `NavStack` 内部本来就是这个判据（它的 `immediate` 那一条），
/// 阅读层在这里跟着同一件事，才不会出现在「系统不许动」的时候还硬滑 280 毫秒。
pub(crate) fn nav_transition(cx: &App) -> Transition {
    if cx.reduce_motion() {
        Transition::new(Duration::ZERO)
    } else {
        Transition::new(NAV_TRANSITION).easing(Easing::EaseInOut)
    }
}

/// 画在上面那一张在转场里走的距离，按视口宽的比例。
const LEAVE: f32 = 0.35;
/// 底下一张走的距离：只走这一小段。两张同向、不同速才读成「一摞纸被抽走」，
/// 走同样的量就是两张卡片各自平移。
const FOLLOW: f32 = 0.12;

/// 外壳占在正文之外的那两层：应用内导航栏画不画，系统级沉浸态要不要。
///
/// 成对存而不是散成两个字段，是因为转场期间要把**整对**冻住（见 [`AppShell::chrome_frozen`]）：
/// 两条边各自回来的话，正文容器会被挤两次，读者看见的就是两次跳。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChromeState {
    /// 应用内的导航栏：手机的底栏、桌面的侧栏与自绘标题栏。
    nav_gone: bool,
    /// 系统级那一层：桌面窗口真全屏、Android 藏状态栏与导航栏。
    immersive: bool,
}

/// 盖在整个窗口上的那一本打开的书。
///
/// 它**不是**架面那条栈上的一页，而是根 div 的最后一个子元素：`inset_0` 盖住导航栏与架面。
/// 分成两层要解决的是转场里那一次挤动 —— 同一只容器装两张页面时，导航栏收回来那一下必然把
/// 「退场还没走完」的那一张压矮，而阅读页的正文按**窗口**量、干净态那两条带却贴在容器边
/// （`bands::band` 的 `bottom_0`），于是字不动、带动，两套参照系在读者眼前分开。现在架面的
/// 盒子从头到尾不因读书而变，进出的动画里再没有东西可跳。
///
/// 这一层的进出与里面压进来的每一屏共用同一个时钟（[`nav_transition`]）与同一种摆法
/// （[`nav_offset`]：从右边进来、回右边出去）。底下的架面一动不动 —— 这一层是不透明的整屏。
struct ReadingLayer {
    /// 正文那一屏。
    page: Entity<ReadingPage>,
    /// 这一层自己的返回栈：**正文是栈底**，从阅读页里压进来的那一屏（设置）住在里面。
    /// 借架面那条栈的话，压进来的那一屏会被这一层盖住。
    ///
    /// 栈里存着正文的 `AnyView`，而正文手上握着这条栈 —— 所以那一头拿的是弱句柄
    /// （见 [`ReadingPage::new`]），否则这一层放手之后两边互相拽着不放。
    stack: Entity<NavStackState>,
    /// 那条子栈一变就敲一下外壳重画：`NavStack` 是个元素而不是视图，栈顶换了没人替它重画。
    _stack_watch: Subscription,
    /// 是不是正在退场：读者已经离开、位置已经落库，画面还在往外走。由
    /// [`AppShell::close_reading`] 翻成 `true`，走完那一步由 [`Presence`] 的采样决定放手。
    closing: bool,
}

/// 阅读层那一张元素的时钟键：按实体分，换一本书就是新的一次进入。
fn reading_layer_id(page: &Entity<ReadingPage>) -> TransitionId {
    TransitionId::from(ElementId::from(("reading-layer", page.entity_id())))
}

/// 一页在转场中的横向位移与不透明度。
///
/// 抽成纯函数是为了钉住**方向**：返回时正文往右走、架面从左外滑回来；进书正好反过来。
/// 接反了不报错，只会在真机上读成「书是从右边来的」，而这一层是宿主测试唯一摸得到的地方。
///
/// `progress` 是 `NavStack` 已经过完缓动的那个数：0 = 刚起步，1 = 动完。
fn nav_offset(
    operation: NavOperation,
    phase: PresencePhase,
    progress: f32,
    width: f32,
) -> (f32, f32) {
    match (operation, phase) {
        // 返回：被弹掉的那一张画在上面，往右走满 `LEAVE` 并淡出。全场只有它淡 —— 它是唯一
        // 一张「身后露出别的东西」的，淡入的那一张身后只有外壳背景。
        (NavOperation::Pop, PresencePhase::Exiting) => (width * LEAVE * progress, 1.0 - progress),
        (NavOperation::Pop, PresencePhase::Entering) => (-width * FOLLOW * (1.0 - progress), 1.0),
        // 进书：新那一张从右外走满 `LEAVE` 盖上来，底下那一张往左退 `FOLLOW`，两张都不淡。
        (NavOperation::Push | NavOperation::Replace, PresencePhase::Entering) => {
            (width * LEAVE * (1.0 - progress), 1.0)
        }
        (NavOperation::Push | NavOperation::Replace, PresencePhase::Exiting) => {
            (-width * FOLLOW * progress, 1.0)
        }
        // 栈定住的时候 `NavStack` 给的是 `Present` 且 `operation` 为 `None`，走不到这里；
        // 这一支只负责把 match 补全，不让漏档变成编译期的猜测。
        _ => (0.0, 1.0),
    }
}

/// 阅读层在进出那一段里的位移与不透明度。
///
/// `Presence` 报的是「还有多在场」（进场 0→1、退场 1→0），而 [`nav_offset`] 要的是「这一趟
/// 走了多远」—— 进场那一段两者同向，退场那一段正好相反。这一格就是把两句话接对头的地方：
/// 接反了不报错，画面是「书一边出现一边往右 drift 走」。
///
/// 定住（`Present` / `Absent`）时一个样式都不加。
fn layer_offset(phase: PresencePhase, present: f32, width: f32) -> (f32, f32) {
    match phase {
        PresencePhase::Exiting => nav_offset(
            NavOperation::Pop,
            PresencePhase::Exiting,
            1.0 - present,
            width,
        ),
        PresencePhase::Entering => {
            nav_offset(NavOperation::Push, PresencePhase::Entering, present, width)
        }
        _ => (0.0, 1.0),
    }
}

/// 转场里的那一页该摆在哪。
///
/// `NavStack` 只管时钟 —— 两个页面都挂在树上、进度已过完缓动、动的期间拦住点击，
/// **怎么走归应用**。这一格原先是原样交出，所以转场跑满了 200 毫秒而画面一动不动。
///
/// 位移按**视口宽**而不是容器宽：`.item` 拿不到容器尺寸，而两端的正文盒本来就是按窗口量
/// 出来的（见 [`reading_reserve`] 那段）。
fn nav_page(page: NavPage, window: &mut Window, _cx: &mut App) -> AnyElement {
    let Some(operation) = page.operation() else {
        // 栈没在动：一个样式都不加，页面就按容器铺满。
        return page.into_any_element();
    };
    let width = f32::from(window.viewport_size().width).max(1.0);
    let (shift, alpha) = nav_offset(operation, page.phase(), page.progress(), width);
    // 左右两条边各挪同样的量：`NavPage` 内部是 `inset_0`，只改 `left` 会让这一张越拉越窄
    // （两条边都钉着），两边一起挪才是平移。
    page.left(px(shift))
        .right(px(-shift))
        .opacity(alpha)
        .into_any_element()
}

/// 焦点路上是不是站着一个文本输入框。判据用在 `AppShell::render` 里那条元素级 `on_action`。
///
/// 走键位上下文而不是比焦点句柄：那一份句柄在输入框自己的元素里，外面拿不到，而 gpui-kit
/// 给每个输入框都挂了 `"Input"` 这一档上下文。也不是「有没有焦点」—— 按钮也带焦点态。
fn field_holds_focus(window: &Window) -> bool {
    window
        .context_stack()
        .iter()
        .any(|context| context.primary().is_some_and(|entry| entry.key == "Input"))
}

/// 配置落盘的去抖窗口（`plans/基础设施.md` §2.5）。拖一次字号滑条会产出几十次改动，
/// 逐次 `save` 就是逐次文件写；攒几百毫秒，一次落盘。退出时由 `on_app_quit` 兜底 flush。
const SAVE_DEBOUNCE: Duration = Duration::from_millis(400);

pub struct AppShell {
    /// 当前目的地。
    destination: Destination,
    /// 每个目的地一条返回栈，下标见 [`Destination::index`]。
    stacks: Vec<gpui_kit::Entity<NavStackState>>,
    /// 「我的」那一个目的地的钻入式设置视图，只有手机端有：那一屏自管内部子屏
    /// （页列表 → 全屏子页），返回要问它。桌面那一格画的是设置整页，没有子屏要问。
    account: Option<gpui_kit::Entity<CompactSettings>>,
    /// 盖在窗口上的那一本打开的书（两端单本）。`None` = 没有在读书。
    reading: Option<ReadingLayer>,
    /// 「统计」那一格的视图。外壳留着它有两个理由：榜单那一行点开的订阅要挂在它上面，
    /// 而**关掉一本书之后要叫它重取一次**（读者读的那几分钟不进账，那一屏就还是旧数）。
    stats: gpui_kit::Entity<StatsView>,
    /// 首次启动的欢迎页。书库位置没确认前，外壳画的是它而不是主界面。
    welcome: gpui_kit::Entity<WelcomeView>,
    /// 现在是不是停在欢迎页。启动时由「书库位置确认过没有」决定（见 [`AppShell::boot`]），
    /// 读者挑完位置、配置一变，就翻掉（见 `_config_watch`）。
    onboarding: bool,
    /// 配置观察者：改偏好即落盘并重画。drop 掉就失效，所以必须活着。
    _config_watch: Subscription,
    /// 书架「打开一本书」的订阅。
    _shelf_watch: Subscription,
    /// 统计榜单「打开一本书」的订阅。与 `_shelf_watch` 走的是同一个 `open_reading`。
    _stats_watch: Subscription,
    /// 书架栈的 `Popped` 订阅：pop 出阅读页时把位置落库。
    _stack_watch: Subscription,
    /// 上一次涂到界面上的外观配置。渲染时比较它。
    applied: Option<Appearance>,
    /// 上一次已经同步给监听服务的 `library` 一节。配置观察者比较它，
    /// 只有书库目录或监听那几项真的变了才重启监听（`plans/基础设施.md` §2.5）。
    watched_library: Library,
    /// 待落盘的去抖任务。每来一次配置改动就换掉它（drop 即取消计时），
    /// 安静 [`SAVE_DEBOUNCE`] 之后才真的写一次盘（§2.5）。
    _save_debounce: Option<Task<()>>,
    /// 上一次向系统**要过**的全屏态（阅读页在最上面 = 要）。渲染时比较它。
    ///
    /// 要存这一份而不是每帧比 `window.is_fullscreen()`：桌面上那一档是异步生效的
    /// （合成器 configure 回来才算数），比实际值会在等回执的那几帧里反复翻转窗口。
    fullscreen_asked: Option<bool>,
    /// 上一帧窗口活不活跃（在前台）。只为**回到前台的那一帧**留的：那一帧要把手上的
    /// 意愿重递一次。
    ///
    /// 原因是 `fullscreen_asked` 记的是「我要过什么」，不是「系统现在是什么」——
    /// 而这两件事会分家：Android 上退后台会把 surface 拆掉重建（`term_window` →
    /// `InitWindow`，真机实测每次前后台都走一遍），系统也可能顺手把栏还原。不重递的
    /// 话，读者带着阅读页回来会看见栏又露出来，而应用还以为藏着。
    was_active: bool,
    /// 转场期间冻住的那对 chrome 状态。`None` = 没有在跑的转场，每帧现算。
    ///
    /// 为什么要冻：`NavStackState::pop` 一调，栈深**当场**就变了，而出场那一页还要挂满整个
    /// 转场（进度就是靠它每帧采样出来的）。于是导航栏在动画第 0 帧就回来，把正文容器挤掉
    /// 一段 —— Android 上是状态栏还原后 `safe.top` 那一截再加 56 那条底栏，桌面上是侧栏那
    /// 260 宽。凡是把东西贴在容器边上的界面都会跟着跳一下。
    ///
    /// 冻到转场结束，动画期间容器尺寸不变，导航栏的回来落在「出场页已经不在了」那一帧。
    /// 代价说清楚：进场那一页在动画这 280 毫秒里停在改变前的位置上。
    ///
    /// 今天走这条路的只剩架面那条栈（「信息」那一页）。**书不在上面** —— 阅读层盖住导航栏，
    /// 收与不收都挤不到它（见 [`ReadingLayer`]）；这一格留在这里，是因为它当初要修的正是
    /// 读书那一处的跳，而那一处现在由分层去修、不再由时序去修。
    chrome_frozen: Option<ChromeState>,
    /// 上一帧**真的画出来**的那一对。pop 事件到达时栈深已经变了，而画面还停在改变之前 ——
    /// 冻进 [`AppShell::chrome_frozen`] 的就是这一份。
    chrome_painted: ChromeState,
    /// 到点放开 `chrome_frozen` 的那个闹钟。`Task` 一 drop 就取消计时，所以要存着。
    _chrome_release: Option<Task<()>>,
}

impl AppShell {
    pub fn boot(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // 系统返回 → `Back`，由本视图的 `on_action` 处理。
        cx.bind_keys([KeyBinding::new("escape", Back, None)]);
        // 音量键 → 翻页动作。归谁管在 `volume_key` 里判（阅读页翻页，别处调系统音量），
        // 所以这里两条都无上下文地绑上：Android 的音量键不经过任何输入框，不存在和打字
        // 抢键的问题；桌面那几个方向键要抢，就得先给阅读页一条 key context，另做一轮。
        cx.bind_keys([
            KeyBinding::new("volumeup", PageNext, None),
            KeyBinding::new("volumedown", PagePrev, None),
        ]);

        // 先把主题的应用字体换成这台设备真能解析出的家族，再建任何用文字的视图：
        // 顺序反了，第一帧就会拿一个不存在的家族去 resolve 而 panic。
        let wanted = ConfigHolder::reader(cx).font_family;
        let family: SharedString = host::device_family(&wanted, cx).into();
        theme::set_application_font(&family, cx);

        // 首次启动：书库位置没确认过就先画欢迎页（见 [`welcome`](crate::ui::features::welcome)）。
        // 判据取自配置实体，与「导入要不要拦住」用的是同一条 `is_confirmed()`。
        let welcome = cx.new(WelcomeView::boot);
        let onboarding = !ConfigHolder::library(cx).is_confirmed();

        // 四个目的地的根视图。
        //
        // 书架那条栈**先于**架面建：架面手上要留一份栈的弱句柄，「信息」那一整页由它
        // 自己压上去。栈归外壳这件事没变，变的只是「谁往里放一页」。
        // （开着的那本书**不在这条栈上** —— 它是盖在整窗上的一层，自己带一条子栈，
        // 见 [`ReadingLayer`]。）
        let shelf_stack = cx.new(|_| NavStackState::new());
        let shelf = cx.new(|cx| ShelfView::boot(window, cx, shelf_stack.downgrade()));
        let stats = cx.new(|_| StatsView::new());
        let notes = cx.new(|_| NotesView::new());
        // 「我的」这一格两端画的不一样：桌面直接画整页字段（[`SettingsView`]，
        // 落点是「关于」那一页），手机画钻入式的紧凑列表（[`CompactSettings`]）——
        // 两份界面吃的是同一份页面规格。「这一端塞不塞得下两栏」问 host，不写 `cfg(mobile)`。
        let (account, account_root): (Option<gpui_kit::Entity<CompactSettings>>, AnyView) =
            if host::supports_overlay_controls(cx) {
                let settings = cx.new(|_| SettingsView::new(SettingsPage::About));
                (None, settings.into())
            } else {
                let view = cx.new(|_| CompactSettings::new());
                let root: AnyView = view.clone().into();
                (Some(view), root)
            };

        // 每个目的地一条栈，根视图先压进去。书架那一条已经在架面之前建好了。
        let mut stacks = Vec::with_capacity(Destination::ALL.len());
        for destination in Destination::ALL {
            let root: AnyView = match destination {
                Destination::Shelf => shelf.clone().into(),
                Destination::Stats => stats.clone().into(),
                Destination::Notes => notes.clone().into(),
                Destination::Account => account_root.clone(),
            };
            let stack = if destination == Destination::Shelf {
                shelf_stack.clone()
            } else {
                cx.new(|_| NavStackState::new())
            };
            stack.update(cx, |state, cx| state.push(root, NavMotion::Immediate, cx));
            stacks.push(stack);
        }

        // 架面的两个动作都要过外壳：点开一本书 = 把那一层盖上来；删掉一本书 = 如果删的正是
        // 开着的那一本，那一层就该退回去。
        //
        // 后一半不是洁癖：`delete_book` 删的是库里的**行**（连封面文件与没人指着的那份副本），
        // 而那一层不知道自己盖的书已经没了 —— 它会继续显示一本不存在书，而离席那两笔账各走
        // 一条 `tracing::warn!`（位置写不进没了的行、会话挂不到不存在的外键上）。那几十分钟
        // 就此无声无息地不记账。今天删书只有架上那张卡的菜单一个入口，而开书时那一层把架面
        // 整个盖住、按不到那张卡 —— 所以这一条现在撞不上。把它接上是因为**这道防线是布局给的**，
        // 而布局最容易在别处被悄悄改掉（多一个从「信息」那一屏删书的入口，洞就开了）。
        //
        // 不特殊照顾「删的是开着的那本」的落库：走同一条 `close_reading`，让它按自己的原样
        // 报那两条 warn —— 那一段没记上账这件事，正该留下一行声响，而不是消音。
        let _shelf_watch = cx.subscribe(&shelf, move |this, _, event, cx| match event {
            ShelfEvent::Open(book_id) => this.open_reading(*book_id, cx),
            ShelfEvent::Deleted(book_id) => {
                let open = this
                    .reading
                    .as_ref()
                    .and_then(|layer| layer.page.read(cx).book_id(cx));
                if open == Some(*book_id) {
                    this.close_reading(cx);
                }
            }
        });

        // 架面那条栈的形状变了 = 导航栏该不该收回来。今天压进去的只有「信息」那一页 ——
        // 书不在栈上（见 [`ReadingLayer`]），它的落库与统计重取在 [`AppShell::close_reading`]。
        let _stack_watch = cx.subscribe(&shelf_stack, |this, _, event, cx| match event {
            NavStackEvent::Popped => {
                // 先把导航栏与沉浸态冻住，理由见 [`AppShell::chrome_frozen`]。减动效那一档
                // `NavStack` 自己走 Immediate，转场根本不跑，冻住只会让导航栏慢 280 毫秒。
                if !cx.reduce_motion() {
                    this.hold_chrome(cx);
                }
                // 导航栏画不画看栈上压了几页，所以栈一变这一帧就得重画 ——
                // 不敲这一下，pop 回架面后那条底栏不会自己回来。
                cx.notify();
            }
            // 压上来一页 = 导航栏本来就该收起，上一趟 pop 留的冻结已经没有意义。
            NavStackEvent::Pushed => {
                this.chrome_frozen = None;
                this._chrome_release = None;
            }
            _ => {}
        });

        // 统计的榜单点开一本书 = 与书架同一动作、同一条路（把那一层盖上来）。
        // 目的地不用切：这一层盖的是**当前那一屏**，退完场读者就回到他点进去之前看着的那页。
        let _stats_watch = cx.subscribe(&stats, move |this, _, event, cx| {
            // 只有一种事件，所以这一下不必判（写成 `if let` 会是个永真的模式匹配，
            // 编译器也会说它 irrefutable）。将来统计页多出别的动作，这里再换回 `match`。
            let StatsEvent::Open(book_id) = event;
            this.open_reading(*book_id, cx);
        });

        // 配置改一次就落盘并重画这一屏（重涂主题在渲染那一帧做）。
        let _config_watch = cx.observe(&ConfigHolder::handle(cx), |this, _, cx| {
            // 落盘**去抖**（§2.5）：拖滑条会产出几十次改动，攒一攒再写一次盘。
            this.schedule_save(cx);
            // 监听只在**监听相关的那几项**真的变了才动：书库目录或 `watch`（开关/目录/节奏）。
            // 改个字号也去重启一遍监听服务，是文档 §2.5 点名要避免的「无关服务重启」。
            let library = ConfigHolder::library(cx);
            if library != this.watched_library {
                this.watched_library = library;
                crate::ui::shared::watch::sync_from_config(cx);
            }
            // 欢迎页那一关：读者挑完书库位置后位置被确认，就该把这一屏换掉。
            // 不用欢迎页自己回调 —— 位置写进配置是唯一的信号，这里观察到即可。
            if this.onboarding && ConfigHolder::library(cx).is_confirmed() {
                this.onboarding = false;
            }
            cx.notify();
        });

        // 退出时把去抖窗口里那份配置立刻落盘，并把还开着的那本书的位置与阅读时长一起
        // 结掉（§1.3：退出要 flush 的只有位置与配置 —— 而时长和位置是同一次离席的两笔账，
        // 漏掉后一笔，那一段就只能等那条五分钟一次的时钟，而读者往往不会在五分钟里回来）。
        // `on_app_quit` 的回调在 `App` 的全局 drop **之前**跑，两次同步写远在它的预算内。
        cx.on_app_quit(|this, cx| {
            ConfigHolder::save(cx);
            // 退出没有动画可走：直接把那一层拿来落库，不等退场走完。
            if let Some(layer) = this.reading.take() {
                layer.page.update(cx, |page, cx| page.close(cx));
            }
            async {}
        })
        .detach();

        // 全局返回兜底。Android 上返回键 / 边缘滑动被 fork 翻成 escape；而停在书架上
        // （焦点路径里没有任何会处理 escape 的元素）时，元素级的 `.on_action` 收不到它。
        // `App::on_action` 在动作冒泡的**末尾**跑、与焦点无关，正好接住这种情况。
        // 「焦点在输入框里」那一档不在这里，在 `render` 的元素级 `on_action` —— 收键盘要
        // `&mut Window`，而动作正在这个窗口自己的派发里跑，窗口是借着的，进不去第二次。
        let weak = cx.entity().downgrade();
        App::on_action(&mut *cx, move |_: &Back, cx: &mut App| {
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |this, cx| this.back(cx));
            }
        });

        // 音量键两条动作。与 `Back` 同一个位置注册，理由也一样：这两个键是**全局**的，
        // 焦点路径里没有任何元素接它们时也要有反应，而元素级 `.on_action` 那时候收不到。
        let weak = cx.entity().downgrade();
        App::on_action(&mut *cx, move |_: &PageNext, cx: &mut App| {
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |this, cx| this.volume_key(true, cx));
            }
        });
        let weak = cx.entity().downgrade();
        App::on_action(&mut *cx, move |_: &PagePrev, cx: &mut App| {
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |this, cx| this.volume_key(false, cx));
            }
        });

        Self {
            destination: Destination::Shelf,
            stacks,
            account,
            reading: None,
            stats,
            welcome,
            onboarding,
            _config_watch,
            _shelf_watch,
            _stats_watch,
            _stack_watch,
            applied: None,
            watched_library: ConfigHolder::library(cx),
            _save_debounce: None,
            fullscreen_asked: None,
            // 建起来就算「在前台」：启动那一帧不是「回到前台」，意愿由上面那条
            // `fullscreen_asked != Some(..)` 第一次递出去，不必这里再递一遍。
            was_active: true,
            chrome_frozen: None,
            // 第一帧画的就是「导航栏在、不沉浸」那一对（架面是栈底）。渲染每帧把它更新成
            // 当帧真正摆出来的值，所以这份初值只服务于第一次 pop 之前。
            chrome_painted: ChromeState {
                nav_gone: false,
                immersive: false,
            },
            _chrome_release: None,
        }
    }

    /// 把「落盘」排到 [`SAVE_DEBOUNCE`] 之后；这期间又来一次改动，就换掉这个任务
    /// （drop `Task` 即取消计时）并重新计时。exit 前的最后一份由 `on_app_quit` flush。
    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self._save_debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let _ = this.update(cx, |_, cx| ConfigHolder::save(cx));
        }));
    }

    /// 把**上一帧画出来的**那对 chrome 状态冻住 [`NAV_TRANSITION`]，到点放开重算。
    ///
    /// 为什么要冻：栈深在 `pop` 那一刻就变了，而出场那一页还要挂满整个转场 —— 让导航栏在第
    /// 0 帧回来，就是让正文容器在动画中间矮一截，把贴着容器边摆的那些东西当场顶走。
    /// 完整的取舍写在 [`AppShell::chrome_frozen`]。
    ///
    /// 冻的是上一帧的**画面**而不是「pop 之前该是什么」：事件到达时栈的形状已经是新的了，
    /// 只有这一份记得改变之前摆的是什么。转场还没跑完又来一次变化时，冻结的值往下顺延一个
    /// 时长，放开的那一刻按当帧现算的目标重画一次。
    ///
    /// 这里不问栈「这一趟动不动」：`NavStackState` 没把在跑的转场暴露出来，而外壳与阅读页
    /// 那两条返回路的 pop 全是 `NavMotion::Animated`，减动效那一档已经在调用处挡掉了。
    fn hold_chrome(&mut self, cx: &mut Context<Self>) {
        self.chrome_frozen = Some(self.chrome_painted);
        self._chrome_release = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NAV_TRANSITION).await;
            let _ = this.update(cx, |this, cx| {
                this.chrome_frozen = None;
                cx.notify();
            });
        }));
    }

    /// 打开一本书：已经开着就换书，否则把那一层盖上来。
    ///
    /// 换书**不重走动画**：那一层还在，只是里面的会话换了另一本 —— 读者从架上点另一本书时
    /// 看到的是「书换了」，而不是「又开了一次」。
    fn open_reading(&mut self, book_id: i64, cx: &mut Context<Self>) {
        if let Some(layer) = self.reading.as_ref().filter(|layer| !layer.closing) {
            layer.page.update(cx, |page, cx| page.open(book_id, cx));
            cx.notify();
            return;
        }
        // 这一层自己的返回栈：正文是栈底，从正文压进去的那一屏（设置）叠在它上面。
        let stack = cx.new(|_| NavStackState::new());
        let shell = cx.entity().downgrade();
        let page = cx.new(|cx| ReadingPage::new(stack.downgrade(), shell, cx));
        // 栈底那一次压入不给动画：这一层的进场由 [`Presence`] 那口时钟管，两层各动一次是套娃。
        stack.update(cx, |state, cx| {
            state.push(page.clone(), NavMotion::Immediate, cx)
        });
        // 压进去的那一屏换了栈顶，而 `NavStack` 是个元素不是视图 —— 它跟着外壳重画才看得见
        // 变化，所以这条栈的每一动作用敲一下外壳收尾（层里不需要冻导航栏：那一层的盒子本来就
        // 不因栈的形状而变）。
        let _stack_watch = cx.subscribe(&stack, |_, _, _, cx| cx.notify());
        page.update(cx, |page, cx| page.open(book_id, cx));
        self.reading = Some(ReadingLayer {
            page,
            stack,
            _stack_watch,
            closing: false,
        });
        cx.notify();
    }

    /// 读者离开这一本书：落库、重取统计、开始退场。走完那一步由 [`Presence`] 的采样决定，
    /// 这里不等动画。
    ///
    /// 两条入口都走到这里：系统返回键（[`AppShell::back`]）与顶栏那颗返回
    /// （`ReadingPage::pop` —— 正文是自己的子栈的栈底，弹不动就该请外壳收层）。
    ///
    /// 落库在**退场开始**那一刻而不是动完之后（与今天在同一语义上）：位置与时长是同一次离席的
    /// 两笔账，动画被打断、进程被杀都不该把它们带走。
    pub(crate) fn close_reading(&mut self, cx: &mut Context<Self>) {
        let Some(layer) = self.reading.as_mut() else {
            return;
        };
        if layer.closing {
            return;
        }
        layer.page.update(cx, |page, cx| page.close(cx));
        layer.closing = true;
        // 刚结掉的那一段要出现在统计上：读者从阅读页切去「统计」看自己读了多久，
        // 那一屏拿的还是上一次进来的快照，就少算了刚才那几十分钟。
        self.stats.update(cx, |stats, cx| stats.refresh(cx));
        cx.notify();
    }

    /// 那一层还挂着吗（**含**正在退场）。系统栏的沉浸跟着它：读者按完返回，动画这一路还在
    /// 书面上走，中途把状态栏抬起来会当场挤一次容器。
    fn reading_mounted(&self) -> bool {
        self.reading.is_some()
    }

    /// 是不是正在读书（挂着、且没在退场）。音量键那一条按它分流：翻正文的页，还是把这一按
    /// 还给系统。
    fn reading_open(&self) -> bool {
        self.reading.as_ref().is_some_and(|layer| !layer.closing)
    }

    /// 系统返回 / 边缘滑动：那一层在最上面时先在层里走完三步，再弹当前栈，栈底交给页面自己
    /// （「我的」的子屏），最后才轮到退出。
    fn back(&mut self, cx: &mut Context<Self>) {
        if self.reading_mounted() {
            if self.reading_open() {
                // 子栈先问：压进来的那一屏（设置）此刻正盖着正文，那一层的浮层在它身后，
                // 先收身后的东西就是把这一按吃掉在读者看不见的地方。
                let popped = self.reading.as_ref().and_then(|layer| {
                    layer
                        .stack
                        .update(cx, |state, cx| state.pop(NavMotion::Animated, cx))
                });
                if popped.is_some() {
                    return;
                }
                // 浮层（图片放大、脚注、目录、顶栏底栏）是此刻最上面那一层，返回该先把它收掉
                // 而不是把人整页弹走。桌面那侧的阅读 chrome 是常驻的，`dismiss_chrome` 恒为
                // false，这一步等于不存在。
                let dismissed = self
                    .reading
                    .as_ref()
                    .map(|layer| layer.page.update(cx, |page, cx| page.dismiss_chrome(cx)))
                    .unwrap_or(false);
                if dismissed {
                    return;
                }
                // 正文就是子栈的栈底，弹不动、浮层也收干净了：收起这一层。
                self.close_reading(cx);
            }
            // 正在退场时再按返回：这一层还是最上面那一屏，这一按就吃在这里 —— 别去弹架面那条栈，
            // 更别在书还看着的时候把应用退掉。
            return;
        }
        let stack = self.stacks[self.destination.index()].clone();
        if stack.read(cx).depth() > 1 {
            stack.update(cx, |state, cx| {
                state.pop(NavMotion::Animated, cx);
            });
            return;
        }
        if self.destination == Destination::Account
            && let Some(account) = self.account.clone()
            && account.update(cx, |account, cx| account.go_back(cx))
        {
            return;
        }
        // 栈底再往后要不要退出，问平台边界：`#[cfg(mobile)]` 写在外壳里就是越界
        // （`plans/基础设施.md` §5.9 —— 平台判断只允许出现在 `host` 与 `platform`）。
        if host::back_exits_app(cx) {
            cx.quit();
        }
    }

    /// 切到一个目的地。导航栏那一次点击与宿主测试都走这一条 —— 抽出来就是为了测试能
    /// 走到「我的」那一格，那正是设置整页站着的地方。
    fn go(&mut self, destination: Destination, window: &mut Window, cx: &mut Context<Self>) {
        // 切屏先把编辑态退掉：目的地已经换了，输入框实体却还活着（每条栈都常驻），
        // 焦点不清软键盘就不收 —— gpui-mobile 只认「焦点离开输入框」这一条路
        // （`text_input_state_changed(FocusLost)`）。桌面同一刀：换页后输入框
        // 留着焦点圈也不像话。
        window.blur(cx);
        // 进「统计」重取一次快照。统计页不在 `render` 里查库（那会把画一屏变成做一轮
        // IO），所以它自己不会知道库里刚才多了什么 —— 进来这一格就是它该问的时刻。
        // 每次都取而不是「只在第一次进来时取」：读者会读一读、切回架面、再切回来看数。
        if destination == Destination::Stats {
            self.stats.update(cx, |stats, cx| stats.refresh(cx));
        }
        self.destination = destination;
        cx.notify();
    }

    /// 当前目的地那条栈：用 `NavStack` 画栈顶（带转场）。
    fn content(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let stack = self.stacks[self.destination.index()].clone();
        // `size_full` 不能省：`NavStack` 里的页面是绝对定位、按容器铺满的，
        // 容器没有确定尺寸时它只会按内容撑高 —— 书架的网格会塌成 0 高，只剩搜索框。
        //
        // `transition` 给的是**时钟**，不是画面：怎么走在这条栈的每一页上，由 [`nav_page`]
        // 那一格决定。少了 `.item` 的位移，时钟跑满也只是一个不动的 280 毫秒。
        NavStack::new(&stack)
            .size_full()
            .transition(nav_transition(cx))
            .item(nav_page)
    }

    /// 那一层该摆在哪。底下由 `layer.stack` 那条子栈摆着正文与压进来的屏。
    ///
    /// 进出复用 [`nav_offset`] 的两支：从右边进来（`Push` / `Entering`）、回右边出去
    /// （`Pop` / `Exiting`）—— 与栈里每一页同一只手，读者不必学两种方向。`Presence` 那两个数
    /// 怎么对接到「走了多远」在 [`layer_offset`]。
    ///
    /// 定住的时候（`Present`）一个样式都不加，这一层就是 `inset_0` 铺满整窗。动的时候挂
    /// `.occlude()`：与 `NavStack` 在转场里拦住点击同一个理由 —— 正在滑的那 280 毫秒里，
    /// 落在纸上的一击既不该算给这一页也不该算给下一页。
    ///
    /// 底下的架面在动画期间一动不动：这一层是不透明的整屏，它盖着。
    fn reading_overlay(
        &self,
        layer: &ReadingLayer,
        sample: PresenceSample,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let width = f32::from(window.viewport_size().width).max(1.0);
        let (shift, alpha) = layer_offset(sample.phase, sample.progress, width);
        div()
            .absolute()
            .inset_0()
            .bg(cx.theme().background)
            .left(px(shift))
            .right(px(-shift))
            .opacity(alpha)
            .when(
                matches!(
                    sample.phase,
                    PresencePhase::Entering | PresencePhase::Exiting
                ),
                |this| this.occlude(),
            )
            .child(
                NavStack::new(&layer.stack)
                    .size_full()
                    .transition(nav_transition(cx))
                    .item(nav_page),
            )
            .into_any_element()
    }

    /// 音量键按下：翻页还是把这一按还给系统，分流判据在 [`VolumeKey::of`]。
    ///
    /// 翻页走阅读页自己那条路（与点屏幕左右两边同一个入口），所以分页 / 连续滚动 /
    /// 章末跨章的行为全都跟着正文。调音量那一路只有 Android 有实现，见
    /// [`crate::ui::platform::adjust_system_volume`]。
    fn volume_key(&mut self, up: bool, cx: &mut Context<Self>) {
        match VolumeKey::of(self.reading_open(), up) {
            VolumeKey::Turn(delta) => {
                if let Some(layer) = self.reading.as_ref() {
                    layer.page.update(cx, |page, cx| page.turn_page(delta, cx));
                }
            }
            VolumeKey::SystemVolume { up } => crate::ui::platform::adjust_system_volume(up),
        }
    }

    /// 当前目的地这条栈上是不是压着页（栈底之外还有东西）。
    ///
    /// 判据只看栈的形状，不看压上来的是谁：阅读页、「信息」那一页、阅读里压进去的设置屏
    /// 要的都是整屏，而它们各自到外壳报一次「我开着呢」迟早会漏报一个。
    /// 问的是**当前目的地**那一条栈：切去别的目的地时，那边的导航栏不该跟着消失。
    fn page_pushed(&self, cx: &App) -> bool {
        self.stacks[self.destination.index()].read(cx).depth() > 1
    }

    /// 当前档位的导航栏：点击即切目的地。
    fn nav(&self, size: SizeClass, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().clone();
        navigation(
            cx,
            self.destination,
            size,
            move |destination, window, cx| {
                entity.update(cx, |this, cx| this.go(destination, window, cx))
            },
        )
    }
}

impl Render for AppShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 外观一改（或第一帧还没涂过）就重涂一次。用 `repaint` 而非 `apply`：
        // 这一帧本来就要重画，再刷所有窗口是自己触发自己。
        let appearance = ConfigHolder::appearance(cx);
        if self.applied.as_ref() != Some(&appearance) {
            theme::repaint(&appearance, Some(window), cx);
            self.applied = Some(appearance);
        }

        let size = SizeClass::of(window);
        let safe = host::safe_area(window, cx);
        // 栈上压了页 = 那一页要吃满整屏 → 导航栏整条收起。今天走这一条的只有「信息」那一页：
        // 书不在栈上（它盖在整层之上），而设置那一屏住在书自己那条子栈里，也不在这条上。
        // 系统级那一层（真全屏、藏状态栏）只跟着书：一页表单不该把状态栏也藏掉。
        // 应用内的导航栏不再跟着书 —— 那一层盖在它上面，收它只会挤动架面（见 [`ReadingLayer`]）。
        let wanted = ChromeState {
            nav_gone: self.page_pushed(cx),
            immersive: immersive_nav(size, self.reading_mounted()),
        };
        // 转场在跑的时候画面上还是**改变之前**那一对，理由见 [`AppShell::chrome_frozen`]。
        let chrome = self.chrome_frozen.unwrap_or(wanted);
        self.chrome_painted = chrome;
        let ChromeState {
            nav_gone,
            immersive,
        } = chrome;

        // 先问时钟再决定挂不挂：`closing` 把 `Presence` 的目标翻成「不在」，它自己走完退场，
        // 走到 `Absent` 那一帧才真的放手。落库与统计早在 [`AppShell::close_reading`] 记过了，
        // 这里不再记第二笔。
        let reading_sample = self.reading.as_ref().map(|layer| {
            Presence::new(reading_layer_id(&layer.page), !layer.closing)
                .transition(nav_transition(cx))
                .sample(window, cx)
        });
        if reading_sample.is_some_and(|sample| !sample.should_render()) {
            self.reading = None;
        }
        // 报给正文的那一条只剩安全区（左右两条边没人占了），见 [`reading_reserve`]。
        let reserve = reading_reserve(size, &safe);
        if let Some(layer) = self.reading.as_ref() {
            layer
                .page
                .update(cx, |page, cx| page.set_reserve(reserve, cx));
        }

        // 系统级那一层也跟着沉浸：桌面上窗口真全屏、Android 上藏掉状态栏与导航栏，
        // 应用内那层（侧栏 / 自绘标题栏）在 `body` 里收。只在**期望变了**或**回到前台**
        // 的那一帧动手，两条理由分别见 [`AppShell::fullscreen_asked`] 与
        // [`AppShell::was_active`]。
        let active = window.is_window_active();
        if self.fullscreen_asked != Some(immersive) || (active && !self.was_active) {
            self.fullscreen_asked = Some(immersive);
            // 不就地递：`render` 按约定只算画面，而这一条是发给系统的作用。挪到
            // `defer` 里，它跑在**本帧效果循环的末尾** —— 那时第一帧已经画完、窗口挂好，
            // 而视图的借用也都还回去了。（不是「`open_main_window` 返回之后」：实测它仍在那
            // 句日志前几十微秒，只是不再夹在渲染中间。）
            window.defer(cx, move |window, _cx| {
                crate::ui::platform::set_reader_fullscreen(immersive, window);
            });
        }
        self.was_active = active;

        // 欢迎页先于主界面：书库位置没确认前，导航与内容都不画，整屏让给欢迎页。
        // 安全区照样扣（顶/底都 pad）：移动端手势条与状态栏是系统的地盘，不是应用 chrome。
        let body: AnyElement = if self.onboarding {
            v_flex()
                .size_full()
                .bg(cx.theme().background)
                .text_color(cx.theme().foreground)
                .pt(px(safe.top))
                .pb(px(safe.bottom))
                .child(self.welcome.clone())
                .into_any_element()
        } else {
            let content = self.content(cx).into_any_element();
            if size.is_compact() {
                let mut column = v_flex()
                    .size_full()
                    .bg(cx.theme().background)
                    .text_color(cx.theme().foreground)
                    .pt(px(safe.top))
                    .child(div().flex_1().min_h_0().overflow_hidden().child(content));
                // 底栏连同它上面那条发丝线一起收起；底部的留白是系统安全区，不是应用
                // chrome，收起导航栏也不动它，否则正文会滑进手势条下面。
                if !nav_gone {
                    column = column.child(hairline(cx)).child(self.nav(size, cx));
                }
                column
                    .child(div().w_full().h(px(safe.bottom)))
                    .into_any_element()
            } else {
                // 阅读页在最上面时这条侧栏整条不画（不是留个空位）：它占的那 260
                // 逻辑像素要还给正文，双页才摆得开。
                let mut row = h_flex()
                    .size_full()
                    .bg(cx.theme().background)
                    .text_color(cx.theme().foreground);
                if !nav_gone {
                    row = row.child(self.nav(size, cx));
                }
                row.child(
                    div()
                        // `h_full` 不能省：`h_flex` 在交叉轴上是**居中**而不是拉伸的
                        // （与 `v_flex` 不对称），所以这一格不给高度就只按内容高、
                        // 整页浮在中间，而页面自己的 `flex_1` 与滚动区拿不到可解析的高。
                        .h_full()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .pt(px(safe.top))
                        .child(content),
                )
                .into_any_element()
            }
        };

        // 桌面：窗口 chrome 由应用自绘（窗口选项声明了透明自定义标题栏），
        // 所以最上面补一条 `TitleBar`。移动端没有窗口 chrome，走 `cfg`。
        // 正文那一格给 `flex_1` 而不是让它自己 `size_full`：标题栏已经占掉一段高，
        // 再按 100% 算就会把页面底边（连同底栏）挤出窗口外。
        #[cfg(not(mobile))]
        let body = {
            // 沉浸态连这条自绘标题栏一起收起：窗口 chrome 本来就是应用自己画的
            // （窗口选项声明了透明自定义标题栏），收掉那一段高就还给正文；读者回到
            // 任何一个目的地它都会跟着回来。拖窗与最大化在读书这几十分钟里不需要。
            let stack = v_flex().size_full();
            let stack = if immersive {
                stack
            } else {
                stack.child(
                    gpui_kit::component::TitleBar::new().child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .gap_2()
                            .child(div().id("app-title").test_support().child("Folio")),
                    ),
                )
            };
            // 正文那一格给 `flex_1` 而不是让它自己 `size_full`：标题栏已经占掉一段高，
            // 再按 100% 算就会把页面底边（连同底栏）挤出窗口外。
            stack
                .child(div().flex_1().min_h_0().child(body))
                .into_any_element()
        };

        // 阅读层最后挂：谁在上面由入树的先后定，而这一层要盖住导航栏与架面（见 [`ReadingLayer`]）。
        // 放手那一帧 `reading_sample` 还留着（本帧采的），所以两边都非空才画 —— 不多留一帧。
        let overlay = match (self.reading.as_ref(), reading_sample) {
            (Some(layer), Some(sample)) => Some(
                self.reading_overlay(layer, sample, window, cx)
                    .into_any_element(),
            ),
            _ => None,
        };

        div()
            .id("folio-shell")
            .size_full()
            // 阅读层是 `absolute`，要有个定位基准：根不给 `relative`，它就以窗口为基准 ——
            // 数值上此刻相同，但那等于把「这一层盖住整个应用」交给巧合。
            .relative()
            .on_action(cx.listener(|_this, _: &Back, window, cx| {
                // 返回键的**第一下**给输入框。软键盘只在焦点离开输入框时才收（`gpui-mobile` 的
                // `text_input_state_changed(FocusLost)` 是唯一那条路），而 gpui-kit 的输入框收到
                // escape 只把它当 `input::Escape` 处理、再 `cx.propagate()`，焦点留在框上 —— 键盘
                // 就退不掉。这一键接着还会被 `AppShell::boot` 里那条 `App::on_action` 兜底当成
                // 「弹栈」用，手机上到栈底就是退出应用。
                //
                // 判据是「焦点路上有没有输入框那一档键位上下文」，不是「有没有焦点」：gpui-kit 的
                // 按钮也带焦点态，按后者会变成「点过任何东西之后要多按一次返回才离开这一屏」。
                if field_holds_focus(window) {
                    window.blur(cx);
                } else {
                    cx.propagate();
                }
            }))
            .child(body)
            .children(overlay)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests;
