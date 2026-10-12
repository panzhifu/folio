//! 应用外壳：自适应导航（Material 3）+ 每个目的地一个返回栈。
//!
//! 这是两端**唯一**的根视图。它只关心三件事：当前是哪个目的地、导航栏长什么样、
//! 栈顶是哪个页面。平台特有的东西（安全区、输入法、正文字体）在 `super::host`，
//! 阅读页的两种摆法在 `super::features::reader`，外壳对两者都只给数、不写 `cfg(mobile)`。
//!
//! 路由用 gpui-base 的 [`NavStack`]：每个目的地一条栈，进正文 = 往书架栈 push，
//! 系统返回 = 当前栈 pop。

pub mod navigation;

use std::time::Duration;

use folio_core::Library;
use folio_core::infra::config::Appearance;
use gpui_kit::base::motion::Transition;
use gpui_kit::base::{NavMotion, NavStack, NavStackEvent, NavStackState};
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};
use gpui_kit::{
    AnyElement, AnyView, App, AppContext as _, Context, InteractiveElement as _, IntoElement,
    KeyBinding, ParentElement, Render, SharedString, Styled, Subscription, Task, Window, actions,
    div, px,
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
use navigation::{
    BOTTOM_BAR_HEIGHT, Destination, RAIL_WIDTH, SIDEBAR_WIDTH, SizeClass, navigation,
};

#[cfg(not(mobile))]
use gpui_kit::TestSupportExt as _;

// 系统返回。Android 上 gpui-mobile 把 `AKEYCODE_BACK`（含边缘滑动）与
// `OnBackInvokedCallback` 都翻成 **escape**，所以绑住 escape 就接住了返回。
actions!(folio_shell, [Back]);

/// 导航栏该不该整条收起。**阅读页在最上面就收**，不分档位：读者点开一本书要的是
/// 一整屏正文，而平板与桌面上那条侧栏会吃掉 260 逻辑像素 —— 双页对开正是靠这 260
/// 才摆得下（[`crate::ui::features::reader::chrome::MIN_COLUMN_WIDTH`]）。
/// 回出的路不缺：阅读页自己那条顶栏有返回，系统返回键也接住。
///
/// 抽成自由函数是因为宿主测试进不到 Compact 档 —— [`SizeClass::of`] 在桌面把窄窗口
/// 夹到 `Medium`，只有真机能长出底栏。
pub fn immersive_nav(_size: SizeClass, reading_on_top: bool) -> bool {
    reading_on_top
}

/// 导航栏占掉多少（逻辑像素），按**它压在哪一条边**分两个轴报给正文：Compact 是横躺的
/// 底栏 ⇒ 高，Medium / Expanded 是竖着的图标栏 / 侧栏 ⇒ 宽。收起时那一轴跟着归零。
///
/// 为什么不是一个数：正文拿它去扣高、桌面拿它去扣宽，同一个参数两种读法，外壳就必须在
/// 中间替两端各换算一次 —— 以前正是这一步让阅读视图长出两套。现在两个轴各归各的，
/// 正文自己看哪一轴非零。
pub fn nav_reserve(size: SizeClass, immersive: bool) -> Reserve {
    if immersive {
        return Reserve::ZERO;
    }
    match size {
        SizeClass::Compact => Reserve {
            width: 0.0,
            height: BOTTOM_BAR_HEIGHT,
        },
        SizeClass::Medium => Reserve {
            width: RAIL_WIDTH,
            height: 0.0,
        },
        SizeClass::Expanded => Reserve {
            width: SIDEBAR_WIDTH,
            height: 0.0,
        },
    }
}

/// 报给阅读页的「不属于正文的量」。
///
/// 宽只由导航栏决定（左右两条边外壳不 pad）。高还要加上外壳自己 pad 掉的安全区：
/// Compact 那一支上下都 pad，Medium / Expanded 那一支只 pad 上面那条。阅读页量的是
/// **窗口**高，少扣一条它就会把正文盒算高几十逻辑像素，多出来的那一段落在页脚带下面
/// 被裁掉，于是章尾那几行永远滚不进视野。
pub fn reading_reserve(size: SizeClass, immersive: bool, safe: &host::Insets) -> Reserve {
    let nav = nav_reserve(size, immersive);
    Reserve {
        width: nav.width,
        height: nav.height + safe.top + if size.is_compact() { safe.bottom } else { 0.0 },
    }
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
    /// 被 push 进书架栈的阅读页（两端单本）。
    reading: Option<gpui_kit::Entity<ReadingPage>>,
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
}

impl AppShell {
    pub fn boot(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // 系统返回 → `Back`，由本视图的 `on_action` 处理。
        cx.bind_keys([KeyBinding::new("escape", Back, None)]);

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
        // 自己压上去 —— 与阅读页把设置屏压进同一条栈是同一条路。栈归外壳这件事没变，
        // 变的只是「谁往里放一页」。
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

        // 书架点开一本书 → 往书架栈 push 一个阅读页。
        let shelf_stack = stacks[Destination::Shelf.index()].clone();
        let _shelf_watch = cx.subscribe(&shelf, move |this, _, event, cx| {
            if let ShelfEvent::Open(book_id) = event {
                this.open_reading(*book_id, cx);
            }
        });

        // 阅读页被 pop 出去时把位置落库（返回键与边缘滑动都走这条路）。
        let _stack_watch = cx.subscribe(&shelf_stack, |this, _, event, cx| {
            if matches!(event, NavStackEvent::Popped) {
                if let Some(reading) = this.reading.take() {
                    reading.update(cx, |reading, cx| reading.close(cx));
                }
                // 刚结掉的那一段要出现在统计上：读者从阅读页切去「统计」看自己读了多久，
                // 那一屏拿的还是上一次进来的快照，就少算了刚才那几十分钟。
                this.stats.update(cx, |stats, cx| stats.refresh(cx));
                // 导航栏画不画看栈上压了几页，所以栈一变这一帧就得重画 ——
                // 不敲这一下，pop 回架面后那条底栏不会自己回来。
                cx.notify();
            }
        });

        // 统计的榜单点开一本书 = 与书架同一动作、同一条路（往书架栈 push 阅读页）。
        // 目的地也顺手切过去：读者点的是「去读这本」，落点必须是正文而不是留在报告里，
        // 而切目的地正是 [`go`](Self::go) 那一格该做的事，不绕过它自己改字段。
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
            if let Some(reading) = this.reading.take() {
                reading.update(cx, |reading, cx| reading.close(cx));
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

    /// 打开一本书：已经开着就换书，否则新建阅读页 push 进书架栈。
    fn open_reading(&mut self, book_id: i64, cx: &mut Context<Self>) {
        if let Some(reading) = self.reading.clone() {
            reading.update(cx, |reading, cx| reading.open(book_id, cx));
            cx.notify();
            return;
        }
        let stack = self.stacks[Destination::Shelf.index()].clone();
        let weak = stack.downgrade();
        let reading = cx.new(|cx| ReadingPage::new(weak, cx));
        reading.update(cx, |reading, cx| reading.open(book_id, cx));
        stack.update(cx, |state, cx| {
            state.push(reading.clone(), NavMotion::Animated, cx)
        });
        self.reading = Some(reading);
        cx.notify();
    }

    /// 系统返回 / 边缘滑动：先收阅读页浮层，再弹当前栈，栈底再交给页面自己（「我的」的子屏），最后退出。
    fn back(&mut self, cx: &mut Context<Self>) {
        // 浮层是此刻最上面那一层，返回该先把它收掉而不是把人整页弹走。
        // 桌面那侧的阅读 chrome 是常驻的，`dismiss_chrome` 恒为 false，这一步等于不存在。
        if let Some(reading) = self.reading.clone()
            && reading.update(cx, |reading, cx| reading.dismiss_chrome(cx))
        {
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
    fn content(&self, _cx: &mut Context<Self>) -> impl IntoElement {
        let stack = self.stacks[self.destination.index()].clone();
        // `size_full` 不能省：`NavStack` 里的页面是绝对定位、按容器铺满的，
        // 容器没有确定尺寸时它只会按内容撑高 —— 书架的网格会塌成 0 高，只剩搜索框。
        NavStack::new(&stack)
            .size_full()
            .transition(Transition::new(Duration::from_millis(200)))
            .item(|page, _, _| page.into_any_element())
    }

    /// 阅读页是不是正压在书架栈的栈顶。
    ///
    /// 判据是「栈顶就是它」而不是「手上有阅读页」：`reading` 要到 pop 事件到达才清空，
    /// 期间切去别的目的地（桌面侧栏一直可见）不该把那边的导航栏一起带走。
    fn reading_on_top(&self, cx: &App) -> bool {
        let (Some(reading), Destination::Shelf) = (self.reading.as_ref(), self.destination) else {
            return false;
        };
        self.stacks[Destination::Shelf.index()]
            .read(cx)
            .current()
            .is_some_and(|top| top.entity_id() == reading.entity_id())
    }

    /// 当前目的地这条栈上是不是压着页（栈底之外还有东西）。
    ///
    /// 判据只看栈的形状，不看压上来的是谁：阅读页、「信息」那一页、阅读里压进去的设置屏
    /// 要的都是整屏，而它们各自到外壳报一次「我开着呢」迟早会漏报一个。
    /// 问的是**当前目的地**那一条栈：切去别的目的地时，那边的导航栏不该跟着消失。
    fn page_pushed(&self, cx: &App) -> bool {
        self.stacks[self.destination.index()]
            .read(cx)
            .depth()
            > 1
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
        // 栈上压了页 = 那一页要吃满整屏 → 导航栏整条收起。阅读页、「信息」那一页、
        // 阅读里压进去的设置屏都走这一条：收不收是**栈的形状**，不必每页到外壳来报一次自己是谁。
        let nav_gone = self.page_pushed(cx);
        // 系统级那一层（真全屏、藏状态栏）只跟着阅读页：一页表单不该把状态栏也藏掉。
        let immersive = immersive_nav(size, self.reading_on_top(cx));
        // 阅读页要扣掉的那两条边（导航栏 + 安全区），见 [`reading_reserve`]。
        // 扣的是**画出来的**那条导航栏，所以跟着 `nav_gone` 而不是 `immersive`。
        let reserve = reading_reserve(size, nav_gone, &safe);
        if let Some(reading) = self.reading.clone() {
            reading.update(cx, |reading, cx| reading.set_reserve(reserve, cx));
        }

        // 系统级那一层也跟着沉浸：桌面上窗口真全屏、Android 上藏掉状态栏与导航栏，
        // 应用内那层（侧栏 / 自绘标题栏）在 `body` 里收。只在**期望变了**的那一帧动手，
        // 理由见 [`AppShell::fullscreen_asked`]。
        if self.fullscreen_asked != Some(immersive) {
            self.fullscreen_asked = Some(immersive);
            crate::ui::platform::set_reader_fullscreen(immersive, window);
        }

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

        div()
            .id("folio-shell")
            .size_full()
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
            .into_any_element()
    }
}


#[cfg(test)]
mod tests;
