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
    AnyView, App, AppContext as _, Context, InteractiveElement as _, IntoElement, KeyBinding,
    ParentElement, Render, SharedString, Styled, Subscription, Window, actions, div, px,
};

use crate::ui::features::notes::NotesView;
use crate::ui::features::reader::{ReadingPage, Reserve};
use crate::ui::features::settings::AccountView;
use crate::ui::features::shelf::{ShelfEvent, ShelfView};
use crate::ui::features::stats::StatsView;
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

pub struct AppShell {
    /// 当前目的地。
    destination: Destination,
    /// 每个目的地一条返回栈，下标见 [`Destination::index`]。
    stacks: Vec<gpui_kit::Entity<NavStackState>>,
    /// 「我的」目的地：它还自管内部子屏（P0 暂不搬上栈），返回要问它。
    account: gpui_kit::Entity<AccountView>,
    /// 被 push 进书架栈的阅读页（两端单本）。
    reading: Option<gpui_kit::Entity<ReadingPage>>,
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

        // 四个目的地的根视图。
        let shelf = cx.new(|cx| ShelfView::boot(window, cx));
        let stats = cx.new(|_| StatsView::new());
        let notes = cx.new(|_| NotesView::new());
        let account = cx.new(AccountView::boot);

        // 每个目的地一条栈，根视图先压进去。
        let mut stacks = Vec::with_capacity(Destination::ALL.len());
        for destination in Destination::ALL {
            let root: AnyView = match destination {
                Destination::Shelf => shelf.clone().into(),
                Destination::Stats => stats.clone().into(),
                Destination::Notes => notes.clone().into(),
                Destination::Account => account.clone().into(),
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
        let _config_watch = cx.observe(&ConfigHolder::handle(cx), |_, _, cx| {
            ConfigHolder::save(cx);
            cx.notify();
        });

        // 全局返回兜底。Android 上返回键 / 边缘滑动被 fork 翻成 escape；而停在书架上
        // （焦点路径里没有任何会处理 escape 的元素）时，元素级的 `.on_action` 收不到它。
        // `App::on_action` 在动作冒泡的**末尾**跑、与焦点无关，正好接住这种情况。
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
            && self.account.update(cx, |account, cx| account.go_back(cx))
        {
            return;
        }
        #[cfg(mobile)]
        cx.quit();
        #[cfg(not(mobile))]
        let _ = cx;
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
        navigation(cx, self.destination, size, move |destination, _, cx| {
            entity.update(cx, |this, cx| {
                this.destination = destination;
                cx.notify();
            });
        })
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
        // 阅读页按「导航栏占掉多少」缩短正文有效视口：底栏高（Compact）或侧栏宽，
        // 收起时是 0。
        let reserve = nav_reserve(size, immersive);
        if let Some(reading) = self.reading.clone() {
            reading.update(cx, |reading, cx| reading.set_reserve(reserve, cx));
        }

        let content = self.content(cx).into_any_element();

        let root = if size.is_compact() {
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
            column.child(div().w_full().h(px(safe.bottom)))
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
        };

        // 桌面：窗口 chrome 由应用自绘（窗口选项声明了透明自定义标题栏），
        // 所以最上面补一条 `TitleBar`。移动端没有窗口 chrome，走 `cfg`。
        // 正文那一格给 `flex_1` 而不是让它自己 `size_full`：标题栏已经占掉一段高，
        // 再按 100% 算就会把页面底边（连同底栏）挤出窗口外。
        #[cfg(not(mobile))]
        let root = v_flex()
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
            .child(div().flex_1().min_h_0().child(root));

        root.id("folio-shell")
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

    #[cfg(not(mobile))]
    mod booted {
        use crate::ui::shared::config_model::ConfigHolder;
        use crate::ui::shell::AppShell;
        use folio_core::i18n::{Locale, t_in};
        use folio_core::{Config, Language, Store};
        use gpui_kit::component::Root;
        use gpui_kit::test::TestWindowExt;
        use gpui_kit::{AnyWindowHandle, AppContext as _, TestAppContext, px, size};

        fn open_app(cx: &mut TestAppContext) -> AnyWindowHandle {
            cx.update(gpui_kit::init);
            cx.open_window(size(px(900.), px(600.)), |window, cx| {
                crate::ui::shared::app_store::install(Store::in_memory().expect("内存库"), cx);
                ConfigHolder::install(Config::default(), cx);
                let view = cx.new(|cx| AppShell::boot(window, cx));
                Root::new(view, window, cx)
            })
            .into()
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
