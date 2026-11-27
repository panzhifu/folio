//! 应用外壳：自适应导航（Material 3）+ 每个目的地一个返回栈 + 盖在最上面的一本打开的书。
//! 唯一根视图，管目的地、导航形态、栈顶页与阅读层；平台能力经 `host` 取得，不写 `cfg(mobile)`。
//! 路由用 gpui-base 的 [`NavStack`]：每目的地一条栈，系统返回 = 当前栈 pop。
//!
//! **书不在那条栈上**：阅读层是根 div 的最后一个子元素，`inset_0` 盖住导航栏与架面。同一只
//! 容器装两张页面时，导航栏收回来那一下必然挤到「退场还没走完」的那一张 —— 正文按窗口量、
//! 干净态那两条带按容器贴，跳出去的就是那条进度带。分成两层之后，架面的盒子从头到尾不因读书
//! 而变，进出的动画里再没有东西可跳（取舍见 [`ReadingLayer`]）。

pub mod navigation;
pub mod stealth;

use std::time::Duration;

use folio_core::Library;
use folio_core::infra::config::Appearance;
use gpui_kit::base::motion::{Presence, PresencePhase, PresenceSample, TransitionId};
use gpui_kit::base::{NavMotion, NavOperation, NavPage, NavStack, NavStackEvent, NavStackState};
use gpui_kit::component::{ActiveTheme, ThemeMode, h_flex, v_flex};
use gpui_kit::{
    AnyElement, AnyView, App, AppContext as _, AppLifecyclePhase, Context, ElementId, Entity,
    InteractiveElement as _, IntoElement, KeyBinding, ParentElement, Render, ScrollEvent,
    SharedString, Styled, Subscription, Task, WeakEntity, Window, actions, canvas, div, px,
};

use crate::ui::components::ScrollShields;
use crate::ui::features::cloud::{CloudEvent, CloudView};
use crate::ui::features::notes::{NotesEvent, NotesView};
use crate::ui::features::opds::{OpdsEvent, OpdsView};
use crate::ui::features::reader::{Arrow, ReaderSession, ReadingPage, ReadingPageEvent, Reserve};
use crate::ui::features::settings::{CompactSettings, SettingsPage, SettingsView};
use crate::ui::features::shelf::{ShelfEvent, ShelfView};
use crate::ui::features::stats::StatsView;
use crate::ui::features::welcome::WelcomeView;
use crate::ui::foundation::theme;
use crate::ui::host;
use crate::ui::services::config::ConfigHolder;
use crate::ui::services::stealth as stealth_service;
use crate::ui::services::storage;
use navigation::{Destination, FoldTracker, SizeClass, navigation};

// `OnBackInvokedCallback` 也翻成 escape，绑住它就接住返回。
//
// `VolumeUp` / `VolumeDown` 是音量键（见 [`VolumeKey`]）：fork 把
// `AKEYCODE_VOLUME_UP/DOWN` 翻成 `volumeup` / `volumedown` 两个键名，绑定名不能写成
// 带连字符的 `volume-up` —— gpui 的 `Keystroke::parse` 会把连字符前面那段当修饰键读。
//
// 两个动作按**物理键**命名而不是「上一页 / 下一页」：翻页方向归 [`VolumeKey::of`]，
// 而它还要在「不在阅读页」那一支把这一按还给系统调音量 —— 调音量得知道按的是上还是下，
// 所以动作必须带住「哪一颗键」，不能只带「往哪翻」。
//
// `ArrowLeft/Right/Up/Down` 是 PC 的方向键（见 `reader::chrome::Arrow`）：名字同样是物理键，
// 左右在 RTL 的书里要换手，带上「哪一颗键」才换得回来。
actions!(
    folio_shell,
    [
        Back, VolumeUp, VolumeDown, ArrowLeft, ArrowRight, ArrowUp, ArrowDown,
    ]
);

/// 一次音量键按下该去哪儿。
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
    /// 「音量上 = 往前一页、音量下 = 往后一页」是拍的口径（与 KOReader 的
    /// `VOLUME_UP → 上一页` / `VOLUME_DOWN → 下一页` 一致），改它只改这一行。RTL 不
    /// 反过来：页号本来就按阅读序排（左右分区那套换算在 `reader::chrome::zone` 里），
    /// 所以 ±1 在两种方向下都还是「往前 / 往后一页」。
    pub(crate) fn of(reading_open: bool, up: bool) -> Self {
        if reading_open {
            Self::Turn(if up { -1 } else { 1 })
        } else {
            Self::SystemVolume { up }
        }
    }
}

/// 阅读层挂载时保留顶栏沉浸；桌面全屏由顶栏模式键显式请求。
pub fn immersive_nav(_size: SizeClass, reading_mounted: bool) -> bool {
    reading_mounted
}

/// 只有阅读层挂载时接管亮度，其余交还系统。
pub fn reading_brightness(reading_mounted: bool, prefs: Option<f32>) -> Option<f32> {
    reading_mounted.then_some(prefs).flatten()
}

/// 这一帧要涂哪一档浅深：**阅读层完整盖满屏幕**时读阅读页自己那一档，其余时刻（进场中、
/// 退场中、没开书）回到软件外观。
///
/// 交进来的那份是**生效值**，所以「这一本单独选过夜里」已经折在 `reading_mode` 里了
/// （见 `services::config` 那两层怎么叠）。
///
/// **不变量：全局主题只在层盖满的那段时间里是阅读档。** 进场走完那一帧才翻过去 —— 此刻
/// 层正盖着整窗，读者看不见这一翻；退场起步那一帧就翻回来 —— 层还没挪动，同样看不见。
/// 转场全程架面都停在读者在设置里选的那一档，深书退回白天书架的那一闪就没有了
/// （正文自己的颜色不跟壳走，见 `foundation::reading_theme` 的取侧自持）。
///
/// 与 [`reading_brightness`] 同一条口径的变形：亮度只认「层挂着」（含退场），浅深只认
/// 「层盖满」—— 背光在退场动画里还得压着，主题却要当场交还。
pub(crate) fn themed_appearance(mut appearance: Appearance, layer_present: bool) -> Appearance {
    if layer_present {
        // 折叠本身在 `foundation::reading_theme::effective_mode` 一处：正文取色与
        // 水墨判断都调它 —— 三处各写一份就会漂。
        appearance.mode = crate::ui::foundation::reading_theme::effective_mode(&appearance);
    }
    appearance
}

/// 阅读层自己要避开的那一条边：**顶上那条系统状态栏 / 挖孔**（逻辑像素）。层是不透明的整屏，
/// 却把子元素从这里往下推 —— 顶栏、干净态那行章名、正文与目录那几屏都从这里起算。
///
/// 沉浸态把状态栏藏起来了，但这条 inset 照旧非零（挖孔是物理存在的，`displayCutout` 那一条
/// 不跟可见性归零），所以返回栏不会贴到屏幕最顶上、也不会压在摄像头上。
///
/// **底边不在这里**：底边不让层 —— 底下那条手势条由底栏卡片与干净态页脚自己抬高内容、
/// 把底色铺到屏幕底（见 `reader::page` 的 `bottom_inset`），这样卡片与屏幕底之间不是一条空缝。
/// 宽也不在：阅读层 `inset_0` 盖满整窗，左右两条边没人占（平板 / PC 的目录现在是一层
/// 压在正文上的抽屉，不再让宽，见 `reader::toc_panel`）。
///
/// 与 [`reading_reserve`] 是同一个数：那一个是给**量正文**用的（正文自己量窗口高，全靠
/// 它把盒高减对），这一个是给**摆层**用的。两处不一致时，正文盒会比层里真正可用的地方多/少
/// 一条，屏幕上的表现是底部空一截或章尾几行永远滚不进视野。
pub fn reading_top_inset(safe: &host::Insets) -> f32 {
    safe.top
}

/// 报给正文的「不属于它的那一段」。两个轴都带着：正文自己看哪一轴非零（目录改成浮层之后
/// 宽那一轴恒为 0，只剩外壳自己的占位；见 `reader::page` 里把 `reserve` 递回去那一步）。
///
/// 现在只剩**宽**：上下两条系统边由阅读页报给会话的 `PageInsets` 管（见
/// [`reading_top_inset`] 与 `ReadingPage::set_bottom_inset`），不再从这里走一道“高度预留”。
/// 桌面侧栏是压在正文上的浮层，所以这一格实际为 0；留着它是给真的让宽的侧栏。
pub fn reading_reserve(_safe: &host::Insets) -> Reserve {
    Reserve::ZERO
}

use crate::ui::services::motion::{NAV_TRANSITION, nav_transition};
#[cfg(test)]
use crate::ui::services::motion::{nav_duration_for, turn_duration_for, turn_over_for};

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
    _page_watch: Subscription,
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

/// 配置落盘的去抖窗口。拖一次字号滑条会产出几十次改动，
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
    /// 「统计」那一格的视图。外壳留着它只有一个理由：**关掉一本书之后要叫它重取一次**
    /// （读者读的那几分钟不进账，那一屏就还是旧数）。
    stats: gpui_kit::Entity<StatsView>,
    /// 书架那一格的视图。外壳留着它只有一个理由：**关掉一本书之后要叫架面重查一次**。
    /// 开书会把这一本的 `last_opened_at` 顶到最前（`Store::touch_opened`），架面不重查就
    /// 还按旧顺序摆 —— 读者报的「最近阅读不会排到前面」正是这一处（架面查库有去重，
    /// 词没变、架非空就不重查，见 `ShelfView::reload`）。与 [`stats`](Self::stats) 同一条
    /// 理由、同一个调用点（[`AppShell::dismiss_reading`]）。
    shelf: gpui_kit::Entity<ShelfView>,
    /// 「笔记」那一格的视图。与统计同一条理由：批注与书签在阅读页里写，而那一屏
    /// 不在 `render` 里查库 —— 切进这一格、关掉一本书这两个时刻由外壳叫它重取；
    /// 「点了某一条」的回执也归外壳（开书与落点都在它手里）。
    notes: gpui_kit::Entity<NotesView>,
    /// 首次启动的欢迎页。书库位置没确认前，外壳画的是它而不是主界面。
    welcome: gpui_kit::Entity<WelcomeView>,
    onboarding: bool,
    _config_watch: Subscription,
    _shelf_watch: Subscription,
    _remote_watch: Option<Subscription>,
    /// 「笔记」那一屏「点开某一条」的订阅：开书并把落点递进去。
    _notes_watch: Subscription,
    /// 书架栈的 `Popped` 订阅：pop 出阅读页时把位置落库。
    _stack_watch: Subscription,
    /// 系统深浅翻面的订阅。`Window::appearance()` 是 gpui 里的缓存，只由平台回调刷新，
    /// 而一屏静止的文字自己不会要求重画 —— 不接这一条，「跟随系统」就只有改偏好或重启
    /// 才看得见。这里只要一次 `notify`：该不该重涂、涂成哪一档，判据都在渲染那一处。
    _appearance_watch: Subscription,
    /// 窗口重新激活的订阅：最小化隐藏后从系统恢复时把低干扰状态收回可见态。
    _stealth_activation_watch: Subscription,
    /// 移动端应用生命周期（桌面平台不发）：后台结账、前台续段。
    _lifecycle_watch: Subscription,
    /// 系统内存告警（Android `onTrimMemory`）：丢掉可重建的图像缓存。
    _memory_watch: Subscription,
    /// 上次外观配置及浅深；系统翻面也要重涂。
    applied: Option<(Appearance, ThemeMode)>,
    /// 上次同步给监听服务的书库目录；只在监听字段变更时重启。
    watched_library: Library,
    /// 去抖落盘：drop 取消，安静后写盘（§2.5）。
    _save_debounce: Option<Task<()>>,
    /// 桌面阅读想要的全屏态；默认小屏，顶栏模式键请求。
    fullscreen_wanted: bool,
    /// 最近递给系统的全屏态；`None` = 尚未递过。
    fullscreen_asked: Option<bool>,
    /// 上一帧是否活跃；回前台时重递全屏/亮度。
    was_active: bool,
    /// 上次递给系统的背光；`None` = 未递，`Some(None)` = 交还系统。
    brightness_asked: Option<Option<f32>>,
    /// 转场期间冻住的 chrome 状态；`None` = 没有转场，每帧现算。
    /// 冻住是因为 `NavStackState::pop` 当场改栈深，而出场页还要挂满整个
    /// 转场（进度就是靠它每帧采样出来的）。于是导航栏在动画第 0 帧就回来，把正文容器挤掉
    /// 一段 —— 桌面上是侧栏那 96 宽。凡是把东西贴在容器边上的界面都会跟着跳一下。
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
    /// 导航栏让位的判据本体（[`FoldTracker`]，住在导航栏那一层）。
    ///
    /// 四个目的地共用这一句：内容那一路的滚动增量冒泡到 [`AppShell::note_scroll`]，
    /// 攒够就收、回拨就放。从前只有架面自己判（它读虚拟列表的位移再报进来），于是别的
    /// 屏不让位、切回架面又让，读起来像 bug。
    ///
    /// 只影响 Compact 的悬浮底栏与 Medium 的悬浮卡片：Expanded（桌面）的侧栏是常驻
    /// 导航，不参与。真正的位移在 `navigation` 里由弹簧每帧求值 —— 这一格只是目标。
    nav: FoldTracker,
}

impl AppShell {
    pub fn boot(window: &mut Window, cx: &mut Context<Self>) -> Self {
        window.on_next_frame(|window, _| {
            window.on_next_frame(|_, cx| crate::ui::services::speech::prewarm(cx));
        });
        cx.bind_keys([KeyBinding::new("escape", Back, None)]);
        self::stealth::bind_keys(cx);
        cx.bind_keys([
            KeyBinding::new("volumeup", VolumeUp, None),
            KeyBinding::new("volumedown", VolumeDown, None),
        ]);
        // 方向键（PC）→ 翻页。四条都无上下文绑定：输入焦点时 gpui 不派发键绑定，
        // 其余翻页/跨屏/图片查看器的判据在 [`ReadingPage::arrow_turn`]。
        cx.bind_keys([
            KeyBinding::new("left", ArrowLeft, None),
            KeyBinding::new("right", ArrowRight, None),
            KeyBinding::new("up", ArrowUp, None),
            KeyBinding::new("down", ArrowDown, None),
        ]);

        // 系统外观变化使当前窗口刷新。
        let _appearance_watch = cx.observe_window_appearance(window, |_, _, cx| cx.notify());
        let _stealth_activation_watch = self::stealth::observe_activation(window, cx);

        // 移动端生命周期与内存告警：事件只有移动平台会发（桌面平台是空实现，永远不会响）。
        // 只转给当前打开着的那本书；没在读书时什么都不做。
        let weak_shell = cx.weak_entity();
        let _lifecycle_watch = cx.on_app_lifecycle(move |phase, cx| {
            if let Some(shell) = weak_shell.upgrade() {
                shell.update(cx, |this, cx| this.on_app_lifecycle(phase, cx));
            }
        });
        let weak_shell = cx.weak_entity();
        let _memory_watch = cx.on_memory_warning(move |cx| {
            if let Some(shell) = weak_shell.upgrade() {
                shell.update(cx, |this, cx| this.on_memory_warning(cx));
            }
        });

        // 先把主题的应用字体换成这台设备真能解析出的家族，再建任何用文字的视图：
        // 顺序反了，第一帧就会拿一个不存在的家族去 resolve 而 panic。
        let wanted = ConfigHolder::reader(cx).font_family;
        let family: SharedString = host::device_family(&wanted, cx).into();
        theme::set_application_font(&family, cx);

        // Android：fork 那份手动清单之外的系统字体在后台补注册（fontdb 不扫 Android
        // 路径，见 `platform::system_fonts` 模块文档）；完成时推字体表版本并刷新窗口，
        // 已建好的字体选择器会自己重取名单。桌面在 fontdb 里已经全量扫过，这里直接让路。
        crate::ui::platform::system_fonts::load_all_system_fonts(cx);

        // 读者昨天装的字体今天还要在：配置里点过名的自装字体（下载 / 导入）照清单
        // 重注册进文本系统（fontdb 的注册是进程级的，见 `platform::user_fonts`）。
        crate::ui::platform::user_fonts::load_user_fonts(cx);

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
        let notes = cx.new(|cx| NotesView::boot(window, cx));
        // 「笔记」的第一份快照在启动这一刻就取好：目的地那扇门（`go`）只在**切进**那一格
        // 时叫重取，读者若从不切走就永远等不到那一次。
        notes.update(cx, |notes, cx| notes.refresh(cx));
        // 点了一条 = 开那本书、落到那一处。落点递进阅读页那句 `goto`：目标章多半还没
        // 排过版，会话挂恢复请求，排版做完那一帧落地 —— 与目录跳转同一条路。
        let _notes_watch = cx.subscribe(&notes, |this, _, event: &NotesEvent, cx| {
            let NotesEvent::Open { book_id, record } = event;
            this.open_reading(*book_id, cx);
            if let Some(layer) = this.reading.as_ref() {
                layer
                    .page
                    .update(cx, |page, cx| page.goto_record(record.0, record.1, cx));
            }
        });
        // 「我的」这一格两端画的不一样：桌面直接画整页字段（[`SettingsView`]，
        // 落在侧栏第一页「外观」——「关于」已经挪到侧栏末尾，它是办完事顺手看一眼的东西，
        // 不该是这一格开门的那一页），手机画钻入式的紧凑列表（[`CompactSettings`]）——
        // 两份界面吃的是同一份页面规格。「这一端塞不塞得下两栏」问 host，不写 `cfg(mobile)`。
        let (account, account_root): (Option<gpui_kit::Entity<CompactSettings>>, AnyView) =
            if host::supports_overlay_controls(cx) {
                let settings = cx.new(|cx| SettingsView::new(SettingsPage::Appearance, cx));
                (None, settings.into())
            } else {
                let view = cx.new(CompactSettings::new);
                let root: AnyView = view.clone().into();
                (Some(view), root)
            };

        // 手机那一端钻进展屏时，底栏整条收起 —— 而「收不收」画在外壳这一层，所以外壳得跟着
        // 重画一次。观察者挂在 `notify` 上：`CompactSettings` 每改一次 `screen` 都敲 notify
        // （进、退、选项屏选中写回那三处），因此不必为这一格另发一种事件。
        // 目的地的门留在 [`AppShell::settings_pushed`] 那一头：切到别的目的地时「我的」那屏
        // 还停在详情里，那是它自己的状态，不该把别的目的地的栏也收掉。
        let _account_watch = account
            .as_ref()
            .map(|account| cx.observe(account, |_, _, cx| cx.notify()));

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
            ShelfEvent::OpenOpds => this.open_opds(cx),
            ShelfEvent::OpenCloud => this.open_cloud(cx),
            ShelfEvent::Deleted(book_id) => {
                let open = this
                    .reading
                    .as_ref()
                    .and_then(|layer| layer.page.read(cx).book_id(cx));
                if open == Some(*book_id) {
                    this.close_reading(cx);
                }
            }
            // 多选态一起一落都要重画这一帧：那条工具栏画在外壳这一层（见
            // [`AppShell::selection_bar`]），架面自己 `notify` 只会重画架面那片格子。
            ShelfEvent::SelectionChanged => cx.notify(),
        });

        // 架面那条栈的形状变了 = 导航栏该不该收回来。压进去的是「信息」那一页与某一组
        // 那一页 —— 书不在栈上（见 [`ReadingLayer`]），它的落库与统计重取在
        // [`AppShell::close_reading`]。
        let _stack_watch = cx.subscribe(&shelf_stack, |this, _, event, cx| match event {
            NavStackEvent::Popped => {
                // 先把导航栏与沉浸态冻住，理由见 [`AppShell::chrome_frozen`]。减动效那一档
                // `NavStack` 自己走 Immediate，转场根本不跑，冻住只会让导航栏慢 280 毫秒。
                if !cx.reduce_motion() {
                    this.hold_chrome(cx);
                }
                // 弹回整架这一刻重查一遍。在那一页里做的事改的是**库**：把最后一本移出
                // 那个组、新建一个组 —— 而身后那块架面手上还是压页那一刻的组清单，不敲
                // 这一下，读者回到整架看到的还是那个已经空了的组（它不该再摆出来了）。
                this.shelf.update(cx, |shelf, cx| shelf.force_reload(cx));
                this.unfold_nav(cx);
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

        let _config_watch = cx.observe(&ConfigHolder::handle(cx), |this, _, cx| {
            // 落盘**去抖**（§2.5）：拖滑条会产出几十次改动，攒一攒再写一次盘。
            this.schedule_save(cx);
            // 监听只在**监听相关的那几项**真的变了才动：书库目录或 `watch`（开关/目录/节奏）。
            // 改个字号也去重启一遍监听服务，是文档 §2.5 点名要避免的「无关服务重启」。
            let library = ConfigHolder::library(cx);
            if library != this.watched_library {
                this.watched_library = library;
                crate::ui::services::watch::sync_from_config(cx);
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
            folio_core::speech::shutdown();
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
        App::on_action(&mut *cx, move |_: &VolumeUp, cx: &mut App| {
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |this, cx| this.volume_key(true, cx));
            }
        });
        let weak = cx.entity().downgrade();
        App::on_action(&mut *cx, move |_: &VolumeDown, cx: &mut App| {
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |this, cx| this.volume_key(false, cx));
            }
        });

        // 方向键四条。与音量键同一个位置、同一个理由：这几个键要在**没有焦点**时也
        // 有效（正文本身不拿焦点），元素级 `.on_action` 那时候收不到。归不归阅读页由
        // [`ReadingPage::arrow_turn`] 判，不归就 `propagate`（见 [`AppShell::arrow_key`]）。
        let weak = cx.entity().downgrade();
        App::on_action(&mut *cx, move |_: &ArrowLeft, cx: &mut App| {
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |this, cx| this.arrow_key(Arrow::Left, cx));
            }
        });
        let weak = cx.entity().downgrade();
        App::on_action(&mut *cx, move |_: &ArrowRight, cx: &mut App| {
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |this, cx| this.arrow_key(Arrow::Right, cx));
            }
        });
        let weak = cx.entity().downgrade();
        App::on_action(&mut *cx, move |_: &ArrowUp, cx: &mut App| {
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |this, cx| this.arrow_key(Arrow::Up, cx));
            }
        });
        let weak = cx.entity().downgrade();
        App::on_action(&mut *cx, move |_: &ArrowDown, cx: &mut App| {
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |this, cx| this.arrow_key(Arrow::Down, cx));
            }
        });

        self::stealth::register_actions(cx);

        Self {
            destination: Destination::Shelf,
            stacks,
            account,
            reading: None,
            stats,
            shelf,
            notes,
            welcome,
            onboarding,
            _config_watch,
            _shelf_watch,
            _remote_watch: None,
            _notes_watch,
            _stack_watch,
            _appearance_watch,
            _stealth_activation_watch,
            _lifecycle_watch,
            _memory_watch,
            applied: None,
            watched_library: ConfigHolder::library(cx),
            _save_debounce: None,
            fullscreen_wanted: false,
            fullscreen_asked: None,
            // `fullscreen_asked != Some(..)` 第一次递出去，不必这里再递一遍。
            was_active: true,
            // 背光同理：第一次递由 `brightness_asked != Some(..)` 那条判出来。
            brightness_asked: None,
            chrome_frozen: None,
            // 第一帧画的就是「导航栏在、不沉浸」那一对（架面是栈底）。渲染每帧把它更新成
            // 当帧真正摆出来的值，所以这份初值只服务于第一次 pop 之前。
            chrome_painted: ChromeState {
                nav_gone: false,
                immersive: false,
            },
            _chrome_release: None,
            nav: FoldTracker::default(),
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

    fn open_opds(&mut self, cx: &mut Context<Self>) {
        let stack = self.stacks[Destination::Shelf.index()].clone();
        let page = cx.new(|cx| OpdsView::new(stack.downgrade(), cx));
        self._remote_watch = Some(cx.subscribe(&page, |this, _, event, cx| {
            if matches!(event, OpdsEvent::Imported) {
                this.shelf.update(cx, |shelf, cx| shelf.force_reload(cx));
            }
        }));
        stack.update(cx, |state, cx| state.push(page, NavMotion::Animated, cx));
    }

    fn open_cloud(&mut self, cx: &mut Context<Self>) {
        let stack = self.stacks[Destination::Shelf.index()].clone();
        let page = cx.new(|cx| CloudView::new(stack.downgrade(), cx));
        self._remote_watch = Some(cx.subscribe(&page, |this, _, event, cx| {
            if matches!(event, CloudEvent::Imported) {
                this.shelf.update(cx, |shelf, cx| shelf.force_reload(cx));
            }
        }));
        stack.update(cx, |state, cx| state.push(page, NavMotion::Animated, cx));
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
        // 书的打开放在阅读页构造之前：`has_book()` 为假就不建这一层，
        // `Source::Empty` 不允许变成一屏。
        let reader = cx.new(|cx| {
            let mut reader = ReaderSession::boot(cx);
            reader.open_book(book_id, cx);
            reader
        });
        if !reader.read(cx).has_book() {
            tracing::warn!(book_id, "ReaderSession: 书没打开，不构造阅读页");
            return;
        }
        let page = cx.new(|cx| ReadingPage::new(reader, stack.downgrade(), cx));
        // 栈底那一次压入不给动画：这一层的进场由 [`Presence`] 那口时钟管，两层各动一次是套娃。
        stack.update(cx, |state, cx| {
            state.push(page.clone(), NavMotion::Immediate, cx)
        });
        // 压进去的那一屏换了栈顶，而 `NavStack` 是个元素不是视图 —— 它跟着外壳重画才看得见
        // 变化，所以这条栈的每一动作用敲一下外壳收尾（层里不需要冻导航栏：那一层的盒子本来就
        // 不因栈的形状而变）。
        let _stack_watch = cx.subscribe(&stack, |_, _, _, cx| cx.notify());
        let _page_watch = cx.subscribe(&page, |this, page, event, cx| {
            this.reading_event(page, *event, cx)
        });
        self.reading = Some(ReadingLayer {
            page,
            stack,
            _stack_watch,
            _page_watch,
            closing: false,
        });
        cx.notify();
    }

    /// 读者离开这一本书：落库、重取统计、开始退场。走完那一步由 [`Presence`] 的采样决定，
    /// 这里不等动画。
    ///
    /// 系统返回键（[`AppShell::back`]）走这里。顶栏那颗返回**不走这条** —— 它跑在正文自己
    /// 的更新里，见 [`AppShell::dismiss_reading`]。
    ///
    /// 落库在**退场开始**那一刻而不是动完之后（与今天在同一语义上）：位置与时长是同一次离席的
    /// 两笔账，动画被打断、进程被杀都不该把它们带走。
    pub(crate) fn close_reading(&mut self, cx: &mut Context<Self>) {
        let Some(layer) = self.reading.as_ref() else {
            return;
        };
        if layer.closing {
            return;
        }
        layer.page.update(cx, |page, cx| page.close(cx));
        self.dismiss_reading(cx);
    }

    fn reading_event(
        &mut self,
        page: Entity<ReadingPage>,
        event: ReadingPageEvent,
        cx: &mut Context<Self>,
    ) {
        let Some(stack) = self
            .reading
            .as_ref()
            .filter(|layer| layer.page == page && !layer.closing)
            .map(|layer| layer.stack.clone())
        else {
            return;
        };
        match event {
            ReadingPageEvent::Closed => self.dismiss_reading(cx),
            ReadingPageEvent::OpenSettings => {
                let settings = cx.new(CompactSettings::new);
                stack.update(cx, |state, cx| {
                    state.push(settings, NavMotion::Animated, cx)
                });
            }
            ReadingPageEvent::SetFullscreen(on) => {
                self.fullscreen_wanted = !host::has_window_chrome() || on;
                // 让渲染先把这次点击递给系统，再恢复用系统态纠正 Esc。
                self.fullscreen_asked = None;
                cx.notify();
            }
        }
    }

    /// `close_reading` 的**后半步**：只翻「正在退场」这一格、重取统计、重画 —— 一根手指都
    /// 不碰正文那一页。
    ///
    /// 给正文自己那颗返回用（`ReadingPage::pop`）：那一句正跑在**正文的更新里**，请外壳再
    /// `update` 一次同一个实体就是 panic（release 下 `panic = abort`，真机上的表现是
    /// 「点返回直接掉回桌面」）。正文那一步的收尾（进度与时长落库）由它自己先做完。
    pub(crate) fn dismiss_reading(&mut self, cx: &mut Context<Self>) {
        let Some(layer) = self.reading.as_mut() else {
            return;
        };
        if layer.closing {
            return;
        }
        layer.closing = true;
        // 只改意图；`fullscreen_asked` 仍记着系统态，渲染时才真正退出。
        self.fullscreen_wanted = false;
        // 刚结掉的那一段要出现在统计上：读者从阅读页切去「统计」看自己读了多久，
        // 那一屏拿的还是上一次进来的快照，就少算了刚才那几十分钟。
        self.stats.update(cx, |stats, cx| stats.refresh(cx));
        // 架面的顺序与进度同理：开书把这一本的 `last_opened_at` 顶到最前、退场那一刻
        // 位置刚落库，而架面查库有去重（词没变、架非空就不重查）—— 不在这儿推它一把，
        // 读者读完退回来看到的还是开书前的旧顺序，「最近阅读」排不到前面。
        self.shelf.update(cx, |shelf, cx| shelf.force_reload(cx));
        self.unfold_nav(cx);
        // 「笔记」也刚过时了：读者在书里划的线、钉的签，都要等这一下才进那一屏。
        self.notes.update(cx, |notes, cx| notes.refresh(cx));
        cx.notify();
    }

    /// 那一层还挂着吗（**含**正在退场）。系统栏的沉浸跟着它：读者按完返回，动画这一路还在
    /// 书面上走，中途把状态栏抬起来会当场挤一次容器。
    fn reading_mounted(&self) -> bool {
        self.reading.is_some()
    }

    /// [`reading_mounted`](Self::reading_mounted) 给回归门看的那一面：那一屏是私有的，
    /// 而「退场走完了没有」是外壳测试唯一能观察到的那个时刻。
    #[cfg(test)]
    pub(crate) fn reading_mounted_for_test(&self) -> bool {
        self.reading_mounted()
    }

    /// 按生产那条路开书，只是正文由测试递进来。
    ///
    /// 真入口（点架上一张卡）要从书库里把正文与容器一起取出来，而回归门要验的是
    /// 「退出之后谁重渲染架面」—— 链子上解 ZIP 那一环与它无关。这里替掉的只有
    /// `page.open(book_id)` 那一步，层怎么盖、退场怎么走全是 [open_reading](Self::open_reading)
    /// 那一份；会话也走生产那一个构造（`with_document`）。
    #[cfg(test)]
    pub(crate) fn open_reading_for_test(
        &mut self,
        doc: std::sync::Arc<dyn folio_core::content::document::Document>,
        prefs: folio_core::Reader,
        locator: folio_core::Locator,
        cx: &mut Context<Self>,
    ) {
        let stack = cx.new(|_| NavStackState::new());
        let session = ReaderSession::with_document(doc, prefs, None, None, locator, cx);
        let reader = cx.new(|_| session);
        let page = cx.new(|cx| ReadingPage::new(reader, stack.downgrade(), cx));
        stack.update(cx, |state, cx| {
            state.push(page.clone(), NavMotion::Immediate, cx)
        });
        let _stack_watch = cx.subscribe(&stack, |_, _, _, cx| cx.notify());
        let _page_watch = cx.subscribe(&page, |this, page, event, cx| {
            this.reading_event(page, *event, cx)
        });
        self.reading = Some(ReadingLayer {
            page,
            stack,
            _stack_watch,
            _page_watch,
            closing: false,
        });
        cx.notify();
    }

    /// 退场走完：把这一层放掉、撤掉这一本书的差量。
    ///
    /// **采样在调用方**（`render` 最前面那一口）：这一格只管放手。撤差量撤在**退场走完**
    /// 这一帧而不是 [`AppShell::close_reading`] 那一刻 —— 正文那一页的颜色还读着生效层
    /// （本书的夜里也许就折在那里），层还在往外滑就把差量撤了，正文当场翻色。
    /// 差量的真相在库里，这里只撤内存里那一份，不需要落盘。
    ///
    /// 主题的翻回**不在这里**：外壳的折档判据是「层盖满」（[`themed_appearance`]），
    /// 离场起步那一帧架面就已经回到软件外观 —— 那一下层还全盖着，读者看不见。
    fn release_reading(&mut self, cx: &mut Context<Self>) {
        if self.reading.is_none() {
            return;
        }
        self.reading = None;
        ConfigHolder::set_book(cx, None);
    }

    /// 应用生命周期：后台按离开这一刻结账并停钟，前台续段。
    fn on_app_lifecycle(&mut self, phase: AppLifecyclePhase, cx: &mut Context<Self>) {
        let Some(layer) = self.reading.as_ref() else {
            return;
        };
        layer
            .page
            .update(cx, |page, cx| page.on_app_lifecycle(phase, cx));
    }

    /// 系统内存告警：把可重建的图像缓存交还系统，当前屏由图集保护。
    fn on_memory_warning(&mut self, cx: &mut Context<Self>) {
        let Some(layer) = self.reading.as_ref() else {
            return;
        };
        layer.page.update(cx, |page, cx| page.on_memory_warning(cx));
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
        // 底部弹层（架面建组 / 导入 / 扫描、笔记那一本、更新日志）是此刻最上面那一层：
        // 返回先收它，别让这一按穿到栈上 —— 手机上栈底那一档就是丢回后台 / 退出，
        // 读者看到的正是「笔记卡片开着，一下返回就回到系统主界面」。
        //
        // 阅读页那几块弹层不走这里：它们由上面的 `dismiss_chrome` 收，各有各的收尾
        // （目录那一层的搜索要作废、设置那一层要回落）。这一句排在那一支**之后**，
        // 正是为了别把那条路绕过去。
        if crate::ui::components::dismiss_top(cx) {
            return;
        }
        // 架面的多选态先退这一态：勾选做到一半时那一按该放下的是这一批，而不是把读者
        // 连人带勾选弹出这一片书（手机上栈底那一档更是直接丢回后台）。
        if self.destination == Destination::Shelf && self.shelf.read(cx).selection_on(cx) {
            self.shelf.update(cx, |shelf, cx| shelf.drop_selection(cx));
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
        // 栈底再往后该做什么，问平台边界：`#[cfg(mobile)]` 写在外壳里就是越界
        // （—— 平台判断只允许出现在 `host` 与 `platform`）。
        match host::back_at_root(cx) {
            host::BackAtRoot::Quit => cx.quit(),
            host::BackAtRoot::SendToBack => {
                // 丢回后台之前先把该落盘的落盘：`on_app_quit` 这条路上**不会**再来一次，
                // 而进程在后台随时可能被系统回收。结的只有位置那一笔 —— 时长有自己的
                // 60 秒时钟，与「窗口失去焦点」那一档同一个口径（见 `render` 里那条）。
                ConfigHolder::save(cx);
                if let Some(layer) = self.reading.as_ref() {
                    layer.page.update(cx, |page, cx| page.flush_position(cx));
                }
                super::platform::send_task_to_back();
            }
            host::BackAtRoot::Stay => {}
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
        // 「笔记」同一条：批注与书签都在阅读页里写，切进这一格就是它该重取的时刻。
        if destination == Destination::Notes {
            self.notes.update(cx, |notes, cx| notes.refresh(cx));
        }
        // 进「我的」起一次统计，但**只在桌面那一头**起。桌面画的是整页字段（[`SettingsView`]），
        // 换到「存储」那一页由 `Settings` 组件自己在侧栏里办，界面这层拿不到那个回调；手机那
        // 一头有落点（`CompactSettings::open` 按页序判），而且根屏一个数都不摆 —— 在那儿起一趟
        // 几百毫秒的目录遍历是为了一屏看不见的东西。
        //
        // 「这一端塞不塞得下两栏」仍然问 host，与上面建两屏时同一个判据，不写 `cfg(mobile)`。
        if destination == Destination::Account && host::supports_overlay_controls(cx) {
            storage::request(cx, storage::Job::Rescan);
            // 备份那一页的清单同样只认磁盘上的现在：进「我的」时重读一次目录。
            crate::ui::services::backup::request(cx, crate::ui::services::backup::Job::List);
        }
        self.destination = destination;
        // 换屏 = 那一屏的内容位置与这里无关，让位状态回零（判据见 [`Self::unfold_nav`]）。
        self.unfold_nav(cx);
        cx.notify();
    }

    /// 内容滚了一点：导航栏该让位就让，该回来就回来。判据在 [`FoldTracker`]。
    ///
    /// 三处不参与，各有一条理由：
    ///
    /// - **书开着**：阅读层盖在导航栏之上，它那些滚动是翻页与竖滚，与那条栏无关；合书
    ///   那一刻 [`AppShell::unfold_nav`] 把状态放回来。
    /// - **Expanded 档**：桌面那条是常驻侧栏，鼠标滚轮又频繁，收掉只会让人找不着北
    ///   （`navigation` 那一头同样传 `false`，这里连攒都不攒，免得白敲重画）。
    /// - **减动效档**：让位是一次空间变化，不是必须传达的信息 —— 交回 tracker 处理。
    fn note_scroll(&mut self, delta_y: f32, window: &Window, cx: &mut Context<Self>) {
        if self.reading.is_some()
            || matches!(crate::ui::host::size_class(window), SizeClass::Expanded)
        {
            return;
        }
        if self.nav.scroll(delta_y, cx.reduce_motion()) {
            self.push_nav_fold(cx);
        }
    }

    /// 把让位状态报三处：导航栏（这一格是弹簧的目标）、架面（它那一行搜索带跟着收放）
    /// 与统计（顶部那条固定带同理）—— 后两者读的都是自己手上那份投影，判据不再由
    /// 它们自己算。
    fn push_nav_fold(&mut self, cx: &mut Context<Self>) {
        let folded = self.nav.folded();
        self.shelf
            .update(cx, |shelf, cx| shelf.set_fold(folded, cx));
        self.stats
            .update(cx, |stats, cx| stats.set_fold(folded, cx));
        cx.notify();
    }

    /// 回到「没让位」：换目的地、弹回整架、合上一本书。留着不管的症状是读者切到「统计」
    /// 看见一条藏起来的栏，而那一屏他一眼都没滚过。
    fn unfold_nav(&mut self, cx: &mut Context<Self>) {
        if self.nav.reset() {
            self.push_nav_fold(cx);
        }
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
    /// 这一层**始终**挂 `.occlude()`：进场、定住、退场都一样。gpui 的鼠标事件按 hitbox
    /// 分发 —— 一次点击会发给**每一个** hitbox 含有按下点的 handler，不透明的背景并不会
    /// 自动给后面的元素设边界。少了这一条，定住时点正文会同时命中身后书架上的卡片：
    /// 书架发 `ShelfEvent::Open`，外壳已经开着书，于是 `page.open(book_id)` 把当前书换掉
    /// 重载 —— 读者看到的是「点阅读界面突然进了另一本书 / 划线时闪一下」。转场那 280ms
    /// 当然也要挡，但那只解决了一半。
    ///
    /// `occlude` 挡的是**这一层后面**的 hitbox；这一层的子元素（正文、菜单、目录、设置、
    /// 图片查看器）画在它前面，照常收事件。
    ///
    /// 底下的架面在这一层存在期间一动不动：它被这一层盖着，也不该再收到 hover 或滚轮。
    ///
    /// 整屏也要给系统让开顶上那一条（[`reading_top_inset`]）：返回栏与干净态那行章名落到
    /// 状态栏／挖孔下面，不贴屏幕顶。层自己的底色照旧铺满整窗（背景画的是 padding 也在内的
    /// 盒），所以空出来的那一条是阅读页的底色，不是一条白缝。底边不让（见同一个函数）。
    fn reading_overlay(
        &self,
        layer: &ReadingLayer,
        sample: PresenceSample,
        top_inset: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let width = f32::from(window.viewport_size().width).max(1.0);
        let (shift, alpha) = layer_offset(sample.phase, sample.progress, width);
        let alpha = if stealth_service::is_quiet(cx) { 0.0 } else { alpha };
        // 捕获层要按最终位置烘页：把外壳这一帧的横移交给正文，正文用 `-shift` 抵消。
        //
        // 只在**进场**那一段交：退场那一段整层同时在淡出（`alpha` 从 1 走到 0，见
        // [`nav_offset`] 的 `Pop`/`Exiting` 那一支），烘出来的纹理会带着那一层淡，而它会被
        // 下一趟翻页一直采下去。退场时交 0，捕获回调按老规矩因为页框滑出窗口而拒绝这一趟。
        let enter_shift = match sample.phase {
            PresencePhase::Entering => shift,
            _ => 0.0,
        };
        // 顶上这一条与 `pt` 是**同一套**数：层把整页推下去多少，正文就把那一段记在
        // `top_inset` 里多少。捕获矩形是按「阅读页坐标」量的（`view::body` 的
        // `sheet_area_in_page`），外壳绕过去直接写会话字段的话，正文那屏下一次重画时本页
        // 记着的旧数又把会话覆盖回去，纸就会比窗口高出一截。
        layer.page.update(cx, |page, cx| {
            page.set_enter_shift(enter_shift);
            page.set_top_inset(top_inset, cx);
        });
        // 两种纸都从窗口 0 起：图与底色铺到系统边，正文/读数带在页内自己让。
        div()
            .absolute()
            .inset_0()
            .bg(cx.theme().background.opacity(alpha))
            .left(px(shift))
            .right(px(-shift))
            .opacity(alpha)
            .occlude()
            .child(
                NavStack::new(&layer.stack)
                    .size_full()
                    .transition(nav_transition(cx))
                    .item(nav_page),
            )
            .into_any_element()
    }

    /// 音量键按下：翻页还是还给系统，判据在 [`VolumeKey::of`]。
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

    /// 方向键（PC）：翻页或把这一键放回焦点元素；不接时 `propagate`，判据见
    /// [`ReadingPage::arrow_turn`]。
    fn arrow_key(&mut self, arrow: Arrow, cx: &mut Context<Self>) {
        let Some(layer) = self.reading.as_ref() else {
            cx.propagate();
            return;
        };
        let handled = layer.page.update(cx, |page, cx| page.arrow_turn(arrow, cx));
        if !handled {
            cx.propagate();
        }
    }

    /// 当前目的地这条栈上是不是压着页（栈底之外还有东西）。
    /// 判据只看栈的形状，不看压上来的是谁：阅读页、信息页、阅读里压进去的设置屏
    /// 要的都是整屏，而它们各自到外壳报一次「我开着呢」迟早会漏报一个。
    /// 问的是**当前目的地**那一条栈：切去别的目的地时，那边的导航栏不该跟着消失。
    fn page_pushed(&self, cx: &App) -> bool {
        self.stacks[self.destination.index()].read(cx).depth() > 1
    }

    /// 「我的」那一屏自己钻进了详情（某一页，或某一格的选项屏）。
    ///
    /// 这一问不在上面那条栈上，因为钻入没有压页：紧凑设置是一个实体自己攥着「现在站着哪一屏」，
    /// 它的根视图就是这条栈的栈底。把每一屏拆成独立视图压进目的地那条栈，书里那一处复用跟着一起
    /// 改形 —— 阅读页「更多设置」压进子栈的是**同一份** `CompactSettings`（见
    /// `reader::ReadingPage::open_settings`），拆成每屏一个视图就没有一份能两边都用。
    /// 所以「这一屏要占满整窗」得单独问一句；代价是这一句要自己认目的地 —— 切去别的目的地时
    /// 「我的」还停在详情里，那是它自己的状态，不该把别处的底栏也收掉。
    fn settings_pushed(&self, cx: &App) -> bool {
        self.destination == Destination::Account
            && self
                .account
                .as_ref()
                .is_some_and(|account| account.read(cx).pushed())
    }

    /// 这一帧该不该把应用导航栏整条收起：当前目的地的栈上压了页、「我的」那屏钻进了详情，
    /// 或**有一块底部弹层正开着**。
    ///
    /// 第三条只跟着 Compact 档那一格（`compact` 由调用方按当帧档位递进来）：那一档的底栏是
    /// **浮**在内容上的 —— 外壳画完目的地才画它，于是模态那一层的下沿被一条栏压着。模态层
    /// 该盖住导航栏，不是被它盖住。桌面与平板两档的侧栏是**占布局行**的兄弟、不与弹层重叠，
    /// 收掉它只会让整页在弹层开着时横向抖一下，所以那两档不参与这一问。
    ///
    /// 摆成方法而不是留在 `render` 里就地算：宿主到不了 Compact 档（`SizeClass::of` 在桌面把
    /// 窄窗口夹到 `Medium`，底栏只在真机长出来），判据得能被测试问一句才红得起来 ——
    /// 与 [`immersive_nav`] 抽成自由函数同一条理由。
    fn nav_gone(&self, compact: bool, cx: &App) -> bool {
        self.page_pushed(cx)
            || self.settings_pushed(cx)
            || (compact && crate::ui::components::any_sheet_open(cx))
    }

    /// 当前档位的导航栏：点击即切目的地。`safe` 是系统 inset —— Compact 档靠底那一条
    /// 让底栏离开手势条，侧栏两档靠顶那一条让开状态栏 / 挖孔。
    ///
    /// `folded` 是「架面正在往下读，导航让开」那一格；位移由 `navigation` 里的弹簧
    /// 每帧求值，所以这里要窗口。
    fn nav(
        &self,
        size: SizeClass,
        safe: &host::Insets,
        folded: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let entity = cx.entity().clone();
        navigation(
            cx,
            self.destination,
            size,
            safe,
            folded,
            window,
            move |destination, window, cx| {
                entity.update(cx, |this, cx| this.go(destination, window, cx))
            },
        )
    }

    /// 架面进了多选态：浮在底边的那一条换成这一批书的工具栏。
    ///
    /// 画在这里而不是架面自己的渲染里，是因为它顶替的就是导航那一格 —— 那条栏的几何
    /// （左右留白、离手势条多远、胶囊表面）三档共用一份，都在 `navigation` 那一层。
    /// 动作照样只把 [`SelectionAction`](navigation::SelectionAction) 递回架面：它才知道
    /// 勾中的是哪几本、书库在哪儿。
    ///
    /// 三档都摆：Compact 是**顶掉**底栏（那条栏本来就在底边同一个位置），侧栏两档是
    /// **另起一条**浮在底边 —— 侧栏是嵌入的、占布局行，收掉它会让整架横着抖一下，
    /// 而读者在这一态里照样该能切目的地。
    fn selection_bar(&self, safe: &host::Insets, cx: &mut Context<Self>) -> impl IntoElement {
        let summary = self.shelf.read(cx).selection_summary(cx);
        let shelf = self.shelf.clone();
        navigation::selection_bar(
            cx,
            summary.count,
            summary.all_selected,
            summary.single.is_some(),
            summary.groups,
            navigation::bar_offset(safe.bottom),
            move |action, window, cx| {
                shelf.update(cx, |shelf, cx| shelf.selection_action(action, window, cx))
            },
        )
    }
}

impl Render for AppShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 外观一改（或第一帧还没涂过）就重涂一次。用 `repaint` 而非 `apply`：
        // 这一帧本来就要重画，再刷所有窗口是自己触发自己。
        // 亮度那一格先抠出来：下面整个 `appearance` 要按阅读层解析、再挪进 `applied`。
        let mut appearance = ConfigHolder::appearance(cx);
        let brightness_pref = appearance.brightness;
        // **时钟先问一口**：放手、折档、挂层三处用的是同一份采样，一帧之内不问第二次。
        let reading_sample = self.reading.as_ref().map(|layer| {
            Presence::new(reading_layer_id(&layer.page), !layer.closing)
                .transition(nav_transition(cx))
                .sample(window, cx)
        });
        // **书要在算外观之前撤掉**。这一层盖在架面上时不画架面那条栈（见 `content` 的调用点），
        // 而栈底下那一屏的渲染缓存还停在书上屏**之前**的外观上（书一落下，主题就不是架面那一档）：
        // `view.rs` 的 `prepaint_view` 命中缓存时只看尺寸、`content_mask`、`text_style` 与
        // 「脏了没有」，而那一屏在读书期间从没进过脏表。于是退场走完的第一帧只要没有东西
        // 替它重渲染，屏上露出来的就是那张旧图 —— 真机上读成「退出阅读后书架还是深色，
        // 点一下才变回来」。撤在这一帧的最前面，架面就是**这一帧自己渲染**的，读的一定是
        // 下面这份刚换好的外观。
        //
        // 撤掉差量会在**这一次 `render` 里**就换掉生效层，所以那份外观要重新取一次
        // （通知要等这一帧画完才送到，不能指望有人替我们重画）。
        if reading_sample.is_some_and(|sample| !sample.should_render()) {
            self.release_reading(cx);
            appearance = ConfigHolder::appearance(cx);
        }
        // 书**盖满**时用阅读页自己的浅深，进场中 / 退场中都回到软件外观（见
        // [`themed_appearance`] 的不变量）：翻面都落在层盖满屏的时刻，读者看不见。
        // 这一本自己选过夜里时，`reading_mode` 已经在生效层里了（见 `services::config`）。
        let appearance = themed_appearance(
            appearance,
            reading_sample.is_some_and(|sample| sample.phase == PresencePhase::Present),
        );
        // 比较的是「配置 + 它解析出来的浅深」那一对：偏好一个字没动、只有系统翻了面，
        // 变的就只有后面那一格。
        let resolved = theme::resolve_mode(appearance.mode, window.appearance());
        let stale = self
            .applied
            .as_ref()
            .is_none_or(|(last, last_mode)| *last != appearance || *last_mode != resolved);
        if stale {
            theme::repaint(&appearance, Some(window), cx);
            self.applied = Some((appearance, resolved));
        }

        let size = crate::ui::host::size_class(window);
        let safe = host::safe_area(window, cx);
        // 架面的多选态：底部那一条不画导航、改画这一批书的工具栏（见 [`AppShell::selection_bar`]）。
        // 只跟着书架这一个目的地 —— 侧栏两档的导航在多选态里照常能按，切去统计再切回来时
        // 勾选还留在架面上，但那一屏上不该挂着一条属于别处的工具栏。
        //
        // 底部弹层开着时不摆：那条栏浮在一切之上，而弹层是要盖住导航那一层的模态（同一个
        // 判据见 [`AppShell::nav_gone`] 第三条）。删除那一问用的是 Popover，不算弹层。
        let selecting = self.destination == Destination::Shelf
            && !crate::ui::components::any_sheet_open(cx)
            && self.shelf.read(cx).selection_on(cx);
        // 要吃满整屏的那一屏在前面 → 导航栏整条收起。今天走这一条的有两处：架面那条栈上压上来
        // 的「信息」那一页，和「我的」自己钻进详情的那一屏（两条判据见 [`AppShell::nav_gone`]）。
        // 书不在这条栈上（它盖在整层之上），而书里「更多设置」压进去用的是那一层自己的子栈，
        // 两条都不算在这里。
        // 系统级那一层（真全屏、藏状态栏）只跟着书：一页表单不该把状态栏也藏掉。
        // 应用内的导航栏不再跟着书 —— 那一层盖在它上面，收它只会挤动架面（见 [`ReadingLayer`]）。
        let wanted = ChromeState {
            nav_gone: self.nav_gone(size.is_compact(), cx),
            immersive: immersive_nav(size, self.reading_mounted()),
        };
        // 转场在跑的时候画面上还是**改变之前**那一对，理由见 [`AppShell::chrome_frozen`]。
        let chrome = self.chrome_frozen.unwrap_or(wanted);
        self.chrome_painted = chrome;
        let ChromeState {
            nav_gone,
            immersive,
        } = chrome;

        // 放手的判据在 `render` 最前面已经按同一份采样判过（`closing` 把 `Presence` 的目标
        // 翻成「不在」，它自己走完退场，走到 `Absent` 那一帧放手）。这里再查一遍是给采样
        // 迟到的那一帧兜底 —— 那时这一帧还没画出架面，撤掉差量重画一遍比让屏幕多停一帧
        // 旧主题便宜。
        if reading_sample.is_some_and(|sample| !sample.should_render()) {
            self.release_reading(cx);
        }
        // 全屏显式偏好：桌面默认小屏，Android 跟阅读层；真全屏时才收起标题栏。
        let active = window.is_window_active();
        let mut fullscreen = if !host::has_window_chrome() {
            immersive
        } else {
            self.fullscreen_wanted
        };
        // Esc 可改系统态；与已应用状态不符时以系统为准。
        if host::has_window_chrome()
            && self.fullscreen_asked == Some(fullscreen)
            && window.is_fullscreen() != fullscreen
        {
            self.fullscreen_wanted = window.is_fullscreen();
            fullscreen = self.fullscreen_wanted;
            self.fullscreen_asked = Some(fullscreen);
        }
        let reading_top = reading_top_inset(&safe);
        if let Some(layer) = self.reading.as_ref() {
            layer.page.update(cx, |page, cx| {
                page.set_reserve(reading_reserve(&safe), cx);
                let top = if host::has_window_chrome() && !fullscreen {
                    reading_top + f32::from(gpui_kit::component::TITLE_BAR_HEIGHT)
                } else {
                    reading_top
                };
                page.set_top_inset(top, cx);
                page.set_bottom_inset(safe.bottom, cx);
            });
        }
        // 离开前台之前把这一屏的位置结掉。滚动那条路已经不写库了（为什么，见
        // `ReaderSession::persist_if_moved` 那段），而 Android 一切后台，进程什么时候
        // 被回收不由我们说 —— 这是最后一个可靠的机会。桌面上这一条也成立：窗口被别的
        // 应用抢走焦点同样是一次离席。
        if self.was_active
            && !active
            && let Some(layer) = self.reading.as_ref()
        {
            let page = layer.page.clone();
            // 同样挪进 `defer`：`render` 只算画面，写库是发给存储的作用。
            window.defer(cx, move |_, cx| {
                page.update(cx, |page, cx| page.flush_position(cx));
            });
        }
        if self.fullscreen_asked != Some(fullscreen) || (active && !self.was_active) {
            self.fullscreen_asked = Some(fullscreen);
            // 不就地递：`render` 按约定只算画面，而这一条是发给系统的作用。挪到
            // `defer` 里，它跑在**本帧效果循环的末尾** —— 那时第一帧已经画完、窗口挂好，
            // 而视图的借用也都还回去了。（不是「`open_main_window` 返回之后」：实测它仍在那
            // 句日志前几十微秒，只是不再夹在渲染中间。）
            window.defer(cx, move |window, _cx| {
                crate::ui::platform::set_reader_fullscreen(fullscreen, window);
            });
        }
        // 「界面亮度」跟着书走：书挂着才接管，书收了就把背光交还系统（判据在
        // [`reading_brightness`]，而桌面上同一个数还决定那一层黑盖不盖）。动手的时机与上面
        // 全屏那一条**逐字相同** —— 只在要的那一格变了、或回到前台的那一帧递：Android 每次
        // 前后台都把 surface 拆了重建，新窗口带着一份默认的窗口属性回来，不重递的话背光
        // 那一格就悄悄丢了，而应用还以为暗着。
        let brightness = reading_brightness(self.reading_mounted(), brightness_pref);
        if self.brightness_asked != Some(brightness) || (active && !self.was_active) {
            self.brightness_asked = Some(brightness);
            // 同样不就地递：`render` 按约定只算画面，这一条是发给系统的作用。
            window.defer(cx, move |_, _| {
                crate::ui::platform::set_screen_brightness(brightness);
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
                // 底栏是**浮**的：它不吃布局行，内容铺满整窗、自己带尾部留白
                // （`navigation::clearance`，加在各目的地滚动内容里），于是滚动时从栏底下
                // 穿过去。`safe.bottom` 也**不**在这里垫 —— 它已经算进那段留白与栏位偏移，
                // 这里再垫一次，内容就永远到不了栏下面。
                let mut page = div()
                    .relative()
                    .size_full()
                    .bg(cx.theme().background)
                    .text_color(cx.theme().foreground)
                    .pt(px(safe.top))
                    .child(div().size_full().min_h_0().overflow_hidden().child(content));
                // 多选态里这条栏**顶掉**底栏：两者占的是同一个位置，摆两条就是叠在一起。
                // 栈上压了页时导航栏已经收起，但工具栏照样要摆 —— 读者在某一组那一页里
                // 勾选，退路只有这一条栏上那颗「退出」。
                if selecting {
                    page = page.child(self.selection_bar(&safe, cx));
                } else if !nav_gone {
                    page = page.child(self.nav(size, &safe, self.nav.folded(), window, cx));
                }
                page.into_any_element()
            } else {
                // 阅读页在最上面时这条侧栏整条不画（不是留个空位）：它占的那段宽
                // 要还给正文，双页才摆得开。
                let mut row = h_flex()
                    .relative()
                    .size_full()
                    .bg(cx.theme().background)
                    .text_color(cx.theme().foreground);
                if !nav_gone {
                    row = row.child(self.nav(size, &safe, self.nav.folded(), window, cx));
                }
                row = row.child(
                    div()
                        // `h_full` 不能省：`h_flex` 在交叉轴上是**居中**而不是拉伸的
                        // （与 `v_flex` 不对称），所以这一格不给高度就只按内容高、
                        // 整页浮在中间，而页面自己的 `flex_1` 与滚动区拿不到可解析的高。
                        .h_full()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        // 顶让状态栏 / 挖孔，底让手势条。侧栏两档是嵌入的，内容不从栏下
                        // 穿过，所以这两条由这一格自己让，与 Compact 档那条浮栏不同。
                        .pt(px(safe.top))
                        .pb(px(safe.bottom))
                        .child(content),
                );
                // 侧栏两档不动那条嵌入的导航（收掉它整架会横向抖一下，而多选态里读者
                // 照样该能切目的地），工具栏另起一条浮在底边。架面在这一态下也给自己
                // 加上那一段尾部留白，最后一排才滚得出来。
                if selecting {
                    row = row.child(self.selection_bar(&safe, cx));
                }
                row.into_any_element()
            }
        };

        // 正文那一格给 `flex_1` 而不是让它自己 `size_full`：标题栏已经占掉一段高，
        // 再按 100% 算就会把页面底边（连同底栏）挤出窗口外。
        let body = if host::has_window_chrome() {
            // 桌面窗口态保留标题栏，才能拖窗、缩放或双击最大化；真全屏时再收起。
            let stack = v_flex().size_full();
            let stack = if fullscreen {
                stack
            } else {
                // 左侧不摆任何字样：那一段只留作拖窗与双击最大化的热区。
                stack.child(gpui_kit::component::TitleBar::new())
            };
            stack
                .child(div().flex_1().min_h_0().child(body))
                .into_any_element()
        } else {
            body
        };

        // 阅读层最后挂：谁在上面由入树的先后定，而这一层要盖住导航栏与架面（见 [`ReadingLayer`]）。
        // 放手那一帧 `reading_sample` 还留着（本帧采的），所以两边都非空才画 —— 不多留一帧。
        let overlay = match (self.reading.as_ref(), reading_sample) {
            (Some(layer), Some(sample)) => Some(
                self.reading_overlay(layer, sample, reading_top, window, cx)
                    .into_any_element(),
            ),
            _ => None,
        };
        // 低干扰书盖满窗口时不画架面，免得透明阅读层透出书架；没书或进退场时照旧。
        let quiet_reading = stealth_service::is_quiet(cx) && overlay.is_some();

        let mut root = div()
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
            // 四屏共用的那一只耳朵，**画在所有内容之前**：目的地里面的滚动不会冒给祖先的
            // `on_scroll`（gpui 只把它交给被指定的那一个滚动容器），所以要走
            // `window.on_mouse_event` 这条窗口级的路 —— 与阅读层记原始触摸的 `touch_follow`
            // 同一套路。而监听表按绘制顺序排，画在前面才轮得到它第一个收。
            .child(nav_watch(cx.weak_entity()));
        if !quiet_reading {
            root = root.child(body);
        }
        root.children(overlay).into_any_element()
    }
}

/// 把窗口级的滚动监听挂上外壳的根。
///
/// `canvas` 的绘制回调里注册才活得过一帧：`window.on_mouse_event` 只活到本帧事件分发结束，
/// 每个绘制帧都要重挂一遍（与阅读层 `body/gestures.rs` 那条 `touch_follow` 同一条规矩）。
///
/// 这一格**不吃命中**：canvas 不插 hitbox，所以它摆在最后也不会把目的地与导航栏的点击挡掉；
/// 它只是听。两相都收的话一次滚动会被记两次，故只认 bubble 那一次（与 `touch_follow` 同一判据）。
fn nav_watch(shell: WeakEntity<AppShell>) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |_, _, window, cx| {
            // 新的一帧开始收「谁自己吃滚动」的名单（面板关掉之后，它那块矩形不许留到
            // 下一帧继续吞滚动）。外壳的根画在内容之前，记在这儿正好：下面的目的地与浮层
            // 随后各自把自己那一块报进来（`components::scroll_guard`）。
            ScrollShields::begin_frame(cx);
            let shell = shell.clone();
            window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
                // 只认 capture 那一次：这一格画在最前面，capture 相它是第一个收到事件的
                // （bubble 相反着走，而目的地那边自己的滚动容器会在那之前把传播停掉 ——
                // 实测挂在后面的窗口级监听一次都收不到 `ScrollEvent`）。两相都算会把
                // 同一格增量记两次。
                if phase.bubble() {
                    return;
                }
                // 落在「自己吃滚动」的面板上的那一笔不是在翻这一屏：浮层里的清单滚到底，
                // 顶栏不该跟着收起来。捕获相比面板自己的 `stop_propagation` 早，只有这份
                // 名单拦得住它。
                if cx
                    .try_global::<ScrollShields>()
                    .is_some_and(|shields| shields.covers(event.position))
                {
                    return;
                }
                // 行的增量按当前行高折算成像素，与 gpui 自己给滚动容器的那一份同一个换算。
                let dy = event.delta.pixel_delta(window.line_height()).y;
                let _ = shell.update(cx, |this, cx| {
                    this.note_scroll(f32::from(dy), window, cx);
                });
            });
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

#[cfg(all(test, not(mobile)))]
mod fullscreen_tests;
#[cfg(test)]
mod tests;
