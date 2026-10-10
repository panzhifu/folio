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

use folio_core::config::Appearance;
use gpui_kit::base::motion::Transition;
use gpui_kit::base::{NavMotion, NavStack, NavStackEvent, NavStackState};
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};
use gpui_kit::{
    AnyElement, AnyView, App, AppContext as _, Context, InteractiveElement as _, IntoElement,
    KeyBinding, ParentElement, Render, SharedString, Styled, Subscription, Window, actions, div,
    px,
};

use crate::ui::features::notes::NotesView;
use crate::ui::features::reader::{ReadingPage, Reserve};
use crate::ui::features::settings::{AccountView, SettingsPage, SettingsView};
use crate::ui::features::shelf::{ShelfEvent, ShelfView};
use crate::ui::features::stats::StatsView;
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

/// 导航栏该不该整条收起。
///
/// 只收 Compact 档（手机底栏）：桌面的 rail / 侧栏还是切目的地的主要入口，
/// 且桌面阅读页自带工具条，不缺这一条。
///
/// 抽成自由函数是因为宿主测试进不到 Compact 档 —— [`SizeClass::of`] 在桌面把窄窗口
/// 夹到 `Medium`，只有真机能长出底栏。
pub fn immersive_nav(size: SizeClass, reading_on_top: bool) -> bool {
    size.is_compact() && reading_on_top
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

pub struct AppShell {
    /// 当前目的地。
    destination: Destination,
    /// 每个目的地一条返回栈，下标见 [`Destination::index`]。
    stacks: Vec<gpui_kit::Entity<NavStackState>>,
    /// 「我的」那一个目的地的行列表视图，只有手机端有：那一屏自管内部子屏
    /// （P0 暂不搬上栈），返回要问它。桌面那一格画的是设置整页，没有子屏要问。
    account: Option<gpui_kit::Entity<AccountView>>,
    /// 被 push 进书架栈的阅读页（两端单本）。
    reading: Option<gpui_kit::Entity<ReadingPage>>,
    /// 首次启动的欢迎页。书库位置没确认前，外壳画的是它而不是主界面。
    welcome: gpui_kit::Entity<WelcomeView>,
    /// 现在是不是停在欢迎页。启动时由「书库位置确认过没有」决定（见 [`AppShell::boot`]），
    /// 读者挑完位置、配置一变，就翻掉（见 `_config_watch`）。
    onboarding: bool,
    /// 配置观察者：改偏好即落盘并重画。drop 掉就失效，所以必须活着。
    _config_watch: Subscription,
    /// 书架「打开一本书」的订阅。
    _shelf_watch: Subscription,
    /// 书架栈的 `Popped` 订阅：pop 出阅读页时把位置落库。
    _stack_watch: Subscription,
    /// 上一次涂到界面上的外观配置。渲染时比较它。
    applied: Option<Appearance>,
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
        let shelf = cx.new(|cx| ShelfView::boot(window, cx));
        let stats = cx.new(|_| StatsView::new());
        let notes = cx.new(|_| NotesView::new());
        // 「我的」这一格两端画的不一样：能点浮层的那一端直接画整页字段（[`SettingsView`]，
        // 落点是「关于」那一页），另一端仍是行列表（[`AccountView`] 的子屏入口）。
        // 「这一端点得住浮层吗」问 host，不在这里写 `cfg(mobile)`。
        let (account, account_root): (Option<gpui_kit::Entity<AccountView>>, AnyView) =
            if host::supports_overlay_controls(cx) {
                let settings = cx.new(|_| SettingsView::new(SettingsPage::About));
                (None, settings.into())
            } else {
                let view = cx.new(AccountView::boot);
                let root: AnyView = view.clone().into();
                (Some(view), root)
            };

        // 每个目的地一条栈，根视图先压进去。
        let mut stacks = Vec::with_capacity(Destination::ALL.len());
        for destination in Destination::ALL {
            let root: AnyView = match destination {
                Destination::Shelf => shelf.clone().into(),
                Destination::Stats => stats.clone().into(),
                Destination::Notes => notes.clone().into(),
                Destination::Account => account_root.clone(),
            };
            let stack = cx.new(|_| NavStackState::new());
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
            if matches!(event, NavStackEvent::Popped)
                && let Some(reading) = this.reading.take()
            {
                reading.update(cx, |reading, cx| reading.close(cx));
            }
        });

        // 配置改一次就落盘并重画这一屏（重涂主题在渲染那一帧做）。
        let _config_watch = cx.observe(&ConfigHolder::handle(cx), |this, _, cx| {
            ConfigHolder::save(cx);
            // 欢迎页那一关：读者挑完书库位置后位置被确认，就该把这一屏换掉。
            // 不用欢迎页自己回调 —— 位置写进配置是唯一的信号，这里观察到即可。
            if this.onboarding && ConfigHolder::library(cx).is_confirmed() {
                this.onboarding = false;
            }
            cx.notify();
        });

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
            welcome,
            onboarding,
            _config_watch,
            _shelf_watch,
            _stack_watch,
            applied: None,
        }
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
        #[cfg(mobile)]
        cx.quit();
        #[cfg(not(mobile))]
        let _ = cx;
    }

    /// 切到一个目的地。导航栏那一次点击与宿主测试都走这一条 —— 抽出来就是为了测试能
    /// 走到「我的」那一格，那正是设置整页站着的地方。
    fn go(&mut self, destination: Destination, window: &mut Window, cx: &mut Context<Self>) {
        // 切屏先把编辑态退掉：目的地已经换了，输入框实体却还活着（每条栈都常驻），
        // 焦点不清软键盘就不收 —— gpui-mobile 只认「焦点离开输入框」这一条路
        // （`text_input_state_changed(FocusLost)`）。桌面同一刀：换页后输入框
        // 留着焦点圈也不像话。
        window.blur(cx);
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
        // 手机上点开一本书 = 整屏都是正文，底栏收起。
        let immersive = immersive_nav(size, self.reading_on_top(cx));
        // 阅读页要扣掉的那两条边（导航栏 + 安全区），见 [`reading_reserve`]。
        let reserve = reading_reserve(size, immersive, &safe);
        if let Some(reading) = self.reading.clone() {
            reading.update(cx, |reading, cx| reading.set_reserve(reserve, cx));
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
                if !immersive {
                    column = column.child(hairline(cx)).child(self.nav(size, cx));
                }
                column
                    .child(div().w_full().h(px(safe.bottom)))
                    .into_any_element()
            } else {
                h_flex()
                    .size_full()
                    .bg(cx.theme().background)
                    .text_color(cx.theme().foreground)
                    .child(self.nav(size, cx))
                    .child(
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
        let body = v_flex()
            .size_full()
            .child(
                gpui_kit::component::TitleBar::new().child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_2()
                        .child(div().id("app-title").test_support().child("Folio")),
                ),
            )
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element();

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
mod tests {
    //! 外壳要钉住的两层：导航栏在哪些档位收起（纯判据，两端都编译），
    //! 以及「外壳起得来、标题栏真在视图树里、换语言能重画」这串启动回路。
    //!
    //! 后半截只在桌面跑：`try_find` 来自 `TestSupportExt`，那个 trait 在本文件是
    //! `#[cfg(not(mobile))]` 才挂上的（移动端没有测试支持注册路径）。

    /// 回归门：手机上正文压在栈顶时底栏收起，正文也不再为它预留那 56px。
    ///
    /// 宿主到不了 Compact 档（`SizeClass::of` 在桌面把窄窗口夹到 `Medium`，底栏只在真机
    /// 长出来），所以钉住渲染时用的那两条判据：收起与否、以及随之而来的预留量。
    /// 预留不归零的话，正文底下会白留一条永远滚不进视野的空带；而预留报错了轴，
    /// 另一端就会拿它去扣错的那一边。
    #[test]
    fn the_bottom_bar_yields_to_the_reading_page() {
        use super::{Reserve, immersive_nav, nav_reserve};
        use crate::ui::shell::navigation::{
            BOTTOM_BAR_HEIGHT, RAIL_WIDTH, SIDEBAR_WIDTH, SizeClass,
        };

        assert!(
            immersive_nav(SizeClass::Compact, true),
            "阅读页在栈顶时底栏应收起"
        );
        assert_eq!(
            nav_reserve(SizeClass::Compact, true),
            Reserve::ZERO,
            "底栏收起后正文不该再预留底栏高"
        );

        // 没进阅读页就是原样：底栏在、预留也还在 —— 而手机上它横躺着，占的是**高**。
        assert!(!immersive_nav(SizeClass::Compact, false));
        assert_eq!(
            nav_reserve(SizeClass::Compact, false),
            Reserve {
                width: 0.0,
                height: BOTTOM_BAR_HEIGHT
            },
            "手机的导航栏占的是正文的高，不是宽"
        );

        // 桌面（Medium / Expanded）的阅读不收起侧栏 —— 那是切目的地唯一的入口，
        // 而它竖着，占的是**宽**。
        assert!(!immersive_nav(SizeClass::Medium, true));
        assert!(!immersive_nav(SizeClass::Expanded, true));
        assert_eq!(
            nav_reserve(SizeClass::Medium, false),
            Reserve {
                width: RAIL_WIDTH,
                height: 0.0
            }
        );
        assert_eq!(
            nav_reserve(SizeClass::Expanded, false),
            Reserve {
                width: SIDEBAR_WIDTH,
                height: 0.0
            }
        );
    }

    /// 回归门：报给阅读页的那份扣减里必须含外壳 pad 掉的安全区。
    ///
    /// 阅读页自己量的是**窗口**高，正文盒 = 窗口高 − 报过去的这一条 − 页眉 − 页脚。
    /// 少扣一条安全区，那个盒子就比真能画的地方高几十逻辑像素，多出来的一段被裁在
    /// 页脚带下面 —— 屏幕上的表现是「已经滚到底了，最后几行还是看不见」。
    #[test]
    fn the_reading_page_is_told_about_the_safe_areas_too() {
        use super::{Reserve, SizeClass, nav_reserve, reading_reserve};
        use crate::ui::host::Insets;
        use crate::ui::shell::navigation::{BOTTOM_BAR_HEIGHT, SIDEBAR_WIDTH};

        let safe = Insets {
            top: 41.0,
            bottom: 25.0,
            ..Default::default()
        };

        // Compact：底栏 + 上下两条一起扣。底栏收起时那两条还在 —— 那是系统的地盘，
        // 不是应用 chrome，收起导航栏不动它。
        assert_eq!(
            reading_reserve(SizeClass::Compact, false, &safe),
            Reserve {
                width: 0.0,
                height: BOTTOM_BAR_HEIGHT + 41.0 + 25.0
            }
        );
        assert_eq!(
            reading_reserve(SizeClass::Compact, true, &safe),
            Reserve {
                width: 0.0,
                height: 41.0 + 25.0
            }
        );

        // 桌面那一支：导航栏占的是宽，纵向只 pad 上面那条安全区，所以高里只加它。
        assert_eq!(
            reading_reserve(SizeClass::Expanded, false, &safe),
            Reserve {
                width: SIDEBAR_WIDTH,
                height: 41.0
            }
        );

        // 安全区全 0（桌面的真实值）时这条退回 `nav_reserve` 原样：不多扣一个像素。
        assert_eq!(
            reading_reserve(SizeClass::Medium, false, &Insets::ZERO),
            nav_reserve(SizeClass::Medium, false)
        );
    }

    #[cfg(not(mobile))]
    mod booted {
        use crate::ui::features::shelf::ShelfView;
        use crate::ui::shared::config_model::ConfigHolder;
        use crate::ui::shell::AppShell;
        use crate::ui::shell::navigation::Destination;
        use folio_core::i18n::{Locale, t_in};
        use folio_core::{Config, Language, Library, Store};
        use gpui_kit::component::Root;
        use gpui_kit::test::TestWindowExt;
        use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext, px, size};

        /// 起一版外壳，顺带把它那个实体也交出来 —— 「切个目的地」这个动作要从它上面调，
        /// 而设置整页正站在「我的」那一个目的地里。
        ///
        /// `desktop_host` 决定装不装平台服务：装了就是「这一端点得住浮层」（桌面那一档），
        /// 没装按点不住算（宿主测试的默认，与手机同一档）。
        ///
        /// 书库位置**显式确认过**：没确认时外壳画的是欢迎页（见
        /// [`the_welcome_screen_gates_the_app_until_a_library_is_chosen`]），
        /// 既有用例要验的是正常的壳路径，不能被欢迎页挡在门外。
        fn boot(
            cx: &mut TestAppContext,
            desktop_host: bool,
        ) -> (AnyWindowHandle, Entity<AppShell>) {
            cx.update(gpui_kit::init);
            let mut shell = None;
            let handle = cx
                .open_window(size(px(900.), px(600.)), |window, cx| {
                    crate::ui::shared::app_store::install(Store::in_memory().expect("内存库"), cx);
                    ConfigHolder::install(confirmed_config(), cx);
                    if desktop_host {
                        crate::ui::host::install(cx);
                    }
                    let view = cx.new(|cx| AppShell::boot(window, cx));
                    shell = Some(view.clone());
                    Root::new(view, window, cx)
                })
                .into();
            (handle, shell.expect("外壳应已建起"))
        }

        /// 已确认书库位置的一份配置。壳里的正常用例都从这里起。
        fn confirmed_config() -> Config {
            Config {
                library: Library {
                    dir: "/tmp/folio-books".into(),
                },
                ..Config::default()
            }
        }

        /// 起一版**首次启动**的外壳：书库位置为空串（＝没确认过），应当停在欢迎页。
        fn boot_first_run(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<AppShell>) {
            cx.update(gpui_kit::init);
            let mut shell = None;
            let handle = cx
                .open_window(size(px(900.), px(600.)), |window, cx| {
                    crate::ui::shared::app_store::install(Store::in_memory().expect("内存库"), cx);
                    ConfigHolder::install(Config::default(), cx);
                    let view = cx.new(|cx| AppShell::boot(window, cx));
                    shell = Some(view.clone());
                    Root::new(view, window, cx)
                })
                .into();
            (handle, shell.expect("外壳应已建起"))
        }

        fn open_app(cx: &mut TestAppContext) -> AnyWindowHandle {
            boot(cx, false).0
        }

        /// 切到「我的」再画一帧，问那一格里长出的是设置整页还是行列表。
        fn account_shows_the_settings_page(
            cx: &mut TestAppContext,
            handle: AnyWindowHandle,
            shell: &Entity<AppShell>,
        ) -> bool {
            cx.update_window(handle, |_, window, cx| {
                shell.update(cx, |this, cx| this.go(Destination::Account, window, cx));
                window.render_frame(cx);
                window.try_find("folio-settings").is_some()
            })
            .expect("窗口应能渲染")
        }

        /// 回归门：自定义标题栏必须挂在视图树里（曾经的 bug 是窗口选项声明了透明标题栏、
        /// 却没有任何人绘制它，于是顶上什么都没有）。
        #[gpui_kit::test]
        fn the_app_renders_a_custom_title_bar(cx: &mut TestAppContext) {
            let handle = open_app(cx);
            let present = cx
                .update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    window.try_find("app-title").is_some()
                })
                .expect("窗口应能渲染");
            assert!(present, "标题栏里的应用标题应出现在视图树中");
        }

        /// 回归门：首次启动（书库位置没确认过）时停在欢迎页，不进主界面；选完位置就放行。
        ///
        /// 判据就是 `Library::is_confirmed()` —— 与「导入要不要拦住」同一条。写反的代价是
        /// 读者第一次打开应用直接撞见书架（导入时又被拦下问位置），或者永远卡在欢迎页出不来。
        #[gpui_kit::test]
        fn the_welcome_screen_gates_the_app_until_a_library_is_chosen(cx: &mut TestAppContext) {
            let (handle, shell) = boot_first_run(cx);

            let welcome_shown = cx
                .update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    window.try_find("folio-welcome").is_some()
                })
                .expect("窗口应能渲染");
            assert!(welcome_shown, "书库位置没确认时应画欢迎页");
            assert!(
                shell.read_with(cx, |this, _| this.onboarding),
                "没确认位置时外壳该停在欢迎页"
            );

            // 选完位置（等价于选择器选中或点「使用默认位置」）：配置一变，门禁就该放行。
            cx.update(|cx| {
                ConfigHolder::edit_library(cx, |library| library.dir = "/tmp/folio-books".into());
            });
            let welcome_shown = cx
                .update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    window.try_find("folio-welcome").is_some()
                })
                .expect("窗口应能渲染");
            assert!(!welcome_shown, "位置确认后欢迎页应消失");
            assert!(
                !shell.read_with(cx, |this, _| this.onboarding),
                "位置确认后外壳该离开欢迎页、进主界面"
            );
        }

        /// 回归门：返回键先退掉输入框的编辑态，而不是把这一屏弹走。
        ///
        /// Android 上系统返回与软键盘的「完成」都被 fork 翻成 `escape`，而软键盘**只在焦点
        /// 离开输入框时**才收（`gpui-mobile` 的 `text_input_state_changed(FocusLost)`），
        /// gpui-kit 的输入框对 escape 又只做 `cx.propagate()`。两头都不接，读者就看见键盘停在
        /// 屏上收不掉，而同一键还会顺手把当前那一屏弹走（栈底那一档在手机上就是退出应用）。
        #[gpui_kit::test]
        fn back_gives_up_the_field_focus_instead_of_navigating(cx: &mut TestAppContext) {
            let (handle, shell) = boot(cx, false);
            let stack = shell.read_with(cx, |this, _| {
                this.stacks[Destination::Shelf.index()].clone()
            });

            // 焦点走真实那一条路：架面渲染过一帧，搜索框的元素真在树上。
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let shelf = stack
                    .read(cx)
                    .current()
                    .cloned()
                    .and_then(|top| top.downcast::<ShelfView>().ok())
                    .expect("书架栈底就是架面本身");
                shelf.update(cx, |shelf, cx| shelf.focus_search(window, cx));
                assert!(window.focused(cx).is_some(), "搜索框应真的拿到焦点");
            })
            .expect("窗口应能渲染");

            cx.simulate_keystrokes(handle, "escape");

            cx.update_window(handle, |_, window, cx| {
                assert!(
                    window.focused(cx).is_none(),
                    "返回该先把编辑态退掉：焦点还在框上时，键盘没有任何一条路能收起来"
                );
            })
            .expect("窗口应能渲染");
            // 退编辑态与弹栈是两件事，一次按键只做前者。
            assert_eq!(
                stack.read_with(cx, |state, _| state.depth()),
                1,
                "退掉焦点的那一次返回不该把这一屏弹走"
            );
            assert!(
                shell.read_with(cx, |this, _| this.reading.is_none()),
                "同上：不该顺手 push 出别的一屏"
            );
        }

        /// 回归门：点在架面的空白上，搜索框的编辑态要退掉。
        ///
        /// 这条走的是**真实嵌入**（书架顶上那颗 [`SearchBox`]）：组件自测钉的是
        /// 判点机制本身，这里钉的是书架没有把它包丢 —— 哪天有人把搜索框换回裸
        /// `Input` 或在外面多套一层容器，判点失聪，真机上的表现又是键盘钉在屏上。
        #[gpui_kit::test]
        fn clicking_blank_on_the_shelf_gives_up_the_search_focus(cx: &mut TestAppContext) {
            let (handle, shell) = boot(cx, false);
            let stack = shell.read_with(cx, |this, _| {
                this.stacks[Destination::Shelf.index()].clone()
            });

            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let shelf = stack
                    .read(cx)
                    .current()
                    .cloned()
                    .and_then(|top| top.downcast::<ShelfView>().ok())
                    .expect("书架栈底就是架面本身");
                shelf.update(cx, |shelf, cx| shelf.focus_search(window, cx));
                assert!(window.focused(cx).is_some(), "搜索框应真的拿到焦点");
            })
            .expect("窗口应能渲染");

            // 窗口正中落一次点击：900×600 的桌面上那是空架说明文所在的空白 ——
            // 框外的空白。（测试观测只注册带交互的元素，架面根上没有可点目标，
            // 所以这里像组件自测一样按坐标落点。）
            cx.update_window(handle, |_, window, cx| {
                let at = gpui_kit::point(px(450.), px(300.));
                window.drag(at, at, cx);
                assert!(
                    window.focused(cx).is_none(),
                    "点在架面空白上应退掉搜索框的编辑态"
                );
            })
            .expect("窗口应能渲染");
        }

        /// 回归门：切目的地时输入框的编辑态要跟着退。
        ///
        /// 每条导航栈都常驻，切去别的页面只是不画书架了，输入框实体还活着 ——
        /// 焦点不清，手机上软键盘就压在新的一屏上不下来。这一刀在 [`AppShell::go`]
        /// 里，导航栏那一次点击与宿主测试走的是同一条路。
        #[gpui_kit::test]
        fn switching_destination_gives_up_the_field_focus(cx: &mut TestAppContext) {
            let (handle, shell) = boot(cx, false);
            let stack = shell.read_with(cx, |this, _| {
                this.stacks[Destination::Shelf.index()].clone()
            });

            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let shelf = stack
                    .read(cx)
                    .current()
                    .cloned()
                    .and_then(|top| top.downcast::<ShelfView>().ok())
                    .expect("书架栈底就是架面本身");
                shelf.update(cx, |shelf, cx| shelf.focus_search(window, cx));
                assert!(window.focused(cx).is_some(), "搜索框应真的拿到焦点");
            })
            .expect("窗口应能渲染");

            cx.update_window(handle, |_, window, cx| {
                shell.update(cx, |this, cx| this.go(Destination::Stats, window, cx));
                assert!(
                    window.focused(cx).is_none(),
                    "切走目的地后编辑态不应留着：焦点不清键盘就收不起来"
                );
            })
            .expect("窗口应能渲染");
        }

        /// 回归门：没有焦点时，返回照常弹栈 —— 上面那一档只在真有异议时才吃掉这一键。
        #[gpui_kit::test]
        fn back_still_pops_when_nothing_has_focus(cx: &mut TestAppContext) {
            let (handle, shell) = boot(cx, false);
            let stack = shell.read_with(cx, |this, _| {
                this.stacks[Destination::Shelf.index()].clone()
            });
            shell.update(cx, |this, cx| this.open_reading(1, cx));
            assert_eq!(
                stack.read_with(cx, |state, _| state.depth()),
                2,
                "阅读页应已压上书架栈"
            );

            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                assert!(window.focused(cx).is_none(), "点开书之后没有输入框拿着焦点");
            })
            .expect("窗口应能渲染");
            cx.simulate_keystrokes(handle, "escape");

            assert_eq!(
                stack.read_with(cx, |state, _| state.depth()),
                1,
                "没有焦点时返回要弹栈，而不是空转一次"
            );
        }

        /// 回归门：桌面上「我的」那一格是**一整页字段**（`SettingsView`）。
        ///
        /// 独立设置窗口删掉之后，那一整页字段只有这一个落脚处；这条钉住它真的被挂上去了，
        /// 而不是留在原地等一个再也没人调的 `open`。
        #[gpui_kit::test]
        fn the_account_destination_is_the_settings_page_where_overlays_work(
            cx: &mut TestAppContext,
        ) {
            let (handle, shell) = boot(cx, true);
            assert!(
                account_shows_the_settings_page(cx, handle, &shell),
                "桌面那一格应画设置整页"
            );
        }

        /// 回归门：浮层点不住的那一端，「我的」那一格仍是行列表。
        ///
        /// 整页字段的每一项控件都是浮层（下拉框、滚动下拉、数字框），在这端挂上去等于
        /// 给读者一屏按不动的开关 —— 判据写反时这条会红。
        #[gpui_kit::test]
        fn the_account_destination_stays_a_list_where_overlays_cannot_be_tapped(
            cx: &mut TestAppContext,
        ) {
            let (handle, shell) = boot(cx, false);
            assert!(
                !account_shows_the_settings_page(cx, handle, &shell),
                "浮层点不住的那一端不该画出设置整页"
            );
        }

        /// 多语言的验收：导航栏的文案由配置里的语言驱动，改偏好后视图能重画到新语言。
        ///
        /// 不断言侧栏那一格的元素 id —— `SidebarMenuItem` 的 id 是内部拼的
        /// （`"{menu-id}-{ix}"`），不是标签。这里渲染一帧确认能重画，再按当前语言解出
        /// 导航标签，等于同时验了「配置 → locale」与「视图可重画」两件事。
        #[gpui_kit::test]
        fn switching_the_interface_language_changes_the_rendered_labels(cx: &mut TestAppContext) {
            cx.update(gpui_kit::init);
            let store = Store::in_memory().expect("内存库");
            let config = Config {
                language: Language {
                    locale: Locale::ZhHans,
                },
                ..Default::default()
            };
            let handle: AnyWindowHandle = cx
                .open_window(size(px(900.), px(600.)), |window, cx| {
                    crate::ui::shared::app_store::install(store, cx);
                    ConfigHolder::install(config.clone(), cx);
                    let view = cx.new(|cx| AppShell::boot(window, cx));
                    Root::new(view, window, cx)
                })
                .into();

            let nav_shelf = |cx: &mut TestAppContext| {
                cx.update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    let locale = ConfigHolder::language(cx).locale;
                    t_in(locale, "nav.shelf", &[])
                })
                .expect("窗口应能渲染")
            };
            assert_eq!(nav_shelf(cx), "书架", "偏好是中文时导航标签应为中文");

            cx.update(|cx| {
                ConfigHolder::edit_language(cx, |l| l.locale = Locale::En);
            });
            assert_eq!(nav_shelf(cx), "Shelf", "切英文后导航标签应跟着变");

            cx.update(|cx| {
                ConfigHolder::edit_language(cx, |l| l.locale = Locale::Ja);
            });
            assert_eq!(nav_shelf(cx), "本棚", "日文文案也要生效");
        }
    }
}
