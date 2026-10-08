use std::path::PathBuf;

use folio_core::store::BookRow;
use folio_core::{Config, Locator, Store, open};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::setting::{SelectIndex, Settings};
use gpui_kit::component::sidebar::{Sidebar, SidebarMenu, SidebarMenuItem, SidebarToggleButton};
use gpui_kit::component::status_bar::StatusBar;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::toolbar::{Toolbar, ToolbarGroup};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, Selectable, Sizable, TitleBar, WindowExt, h_flex, v_flex,
};
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, Styled, Subscription, TestSupportExt, Window, div, px,
};

use crate::account::{account_avatar, display_name};
use crate::config_model::ConfigHolder;
use crate::l10n::{book_count, tf, tr};
use crate::reader::ReaderView;
use crate::settings_view;

/// 侧栏展开宽度。阅读视图按它预留版心，所以这个数只能有一份。
pub const SIDEBAR_WIDTH: f32 = 260.0;
/// 侧栏折叠后的宽度，与组件 `Sidebar` 的 `COLLAPSED_WIDTH` 对齐。
/// 阅读视图的版心预留要跟着折叠态在这两个值之间切，否则折叠后正文会留一大块空白。
pub const SIDEBAR_COLLAPSED_WIDTH: f32 = 48.0;

pub struct FolioApp {
    store: Store,
    library: Vec<BookRow>,
    tabs: Vec<Entity<ReaderView>>,
    active: usize,
    sidebar_collapsed: bool,
    /// 正在看的设置页下标；`None` 表示在读书。
    ///
    /// 设置是**一个页面**而不是对话框：`Settings` 自带一页导航，组件的默认对话框只有
    /// 448px，扣掉它自己的侧栏后正文不到 480px 的堆叠阈值，每个字段都会竖着堆成一列
    /// 没法看的窄条。Design Guides 说的就是这件事：短决策用对话框，要看的字段多用整页。
    settings: Option<usize>,
    /// 配置实体的句柄由 `ConfigHolder` 这个 Global 持有，这里只留订阅：
    /// 它一旦被 drop，阅读器就不会随设置变化重排。
    _config_watch: Subscription,
    /// 操作系统的浅深观察者。drop 它会注销订阅，所以必须活到视图结束。
    _appearance: Subscription,
}

impl FolioApp {
    pub fn new(
        store: Store,
        config: Config,
        startup: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // 先装配置再读书库：装的时候会把界面语言推给 `rust_i18n` 的全局值，
        // 于是读库失败那条通知也是用读者自己选的语言写的。
        let config_state = ConfigHolder::install(config, cx);
        // 首帧就把外观配上。传窗口进去是为了直接读它所在的系统浅深 —— Linux 上
        // `cx.window_appearance()` 走一个 `RefCell`，而下面那个观察者正持有它的借用。
        crate::theme::apply(&ConfigHolder::appearance(cx), Some(window), cx);
        // 读者选了「跟随系统」时，系统在运行中换浅深，界面要跟着换。
        let appearance_id = window.window_handle();
        let _appearance = window.observe_window_appearance(move |_, cx| {
            let _ = appearance_id.update(cx, |_, window, cx| {
                crate::theme::apply(&ConfigHolder::appearance(cx), Some(window), cx);
            });
        });
        let library = store.list_books().unwrap_or_else(|e| {
            eprintln!("{}", tf(cx, "error.library_load_failed", &[("reason", &e)]));
            Vec::new()
        });
        // 配置改一次就：重涂主题 → 推给所有已打开的书 → 落盘（配置文件）。
        // 没有这一步，设置面板改完字号要重开书才生效。
        let _config_watch = cx.observe(&config_state, |this, _, cx| {
            let prefs = ConfigHolder::reader(cx);
            crate::theme::apply(&ConfigHolder::appearance(cx), None, cx);
            for reader in &this.tabs {
                reader.update(cx, |reader, cx| reader.apply_prefs(prefs.clone(), cx));
            }
            ConfigHolder::save(cx);
            cx.notify();
        });
        let mut this = Self {
            store,
            library,
            tabs: Vec::new(),
            active: 0,
            sidebar_collapsed: false,
            settings: None,
            _appearance,
            _config_watch,
        };
        for path in startup {
            this.import(path, window, cx);
        }
        this
    }

    fn refresh_library(&mut self, cx: &mut Context<Self>) {
        self.library = self.store.list_books().unwrap_or_default();
        cx.notify();
    }

    fn import(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let (meta, content) = match open(&path) {
            Ok(v) => v,
            Err(e) => {
                let message = tf(cx, "error.import_failed", &[("reason", &e)]);
                self.notify(message, window, cx);
                return;
            }
        };
        let folio_core::Content::Reflow(flow) = content;
        let book_id = match self.store.add_book(&path, &meta) {
            Ok(id) => id,
            Err(e) => {
                let message = tf(cx, "error.store_write_failed", &[("reason", &e)]);
                self.notify(message, window, cx);
                return;
            }
        };
        if let Err(e) = self.store.save_flow(book_id, &flow) {
            let message = tf(cx, "error.cache_chapters_failed", &[("reason", &e)]);
            self.notify(message, window, cx);
            return;
        }
        self.refresh_library(cx);
        let row = self.store.book(book_id).ok().flatten();
        match row {
            Some(row) => self.open_book(&row, window, cx),
            None => {
                let message = tr(cx, "error.import_not_found");
                self.notify(message, window, cx);
            }
        }
    }

    fn open_book(&mut self, row: &BookRow, window: &mut Window, cx: &mut Context<Self>) {
        // 从书架选书就是回到阅读：设置页留着开着会让下一次点开还是设置。
        self.settings = None;
        if let Some(ix) = self
            .tabs
            .iter()
            .position(|t| t.read(cx).book_id() == row.id)
        {
            self.active = ix;
            cx.notify();
            return;
        }
        let flow = match self.store.load_flow(row.id) {
            Ok(Some(flow)) => flow,
            Ok(None) => {
                let message = tr(cx, "error.book_not_parsed");
                self.notify(message, window, cx);
                return;
            }
            Err(e) => {
                let message = tf(cx, "error.read_chapter_failed", &[("reason", &e)]);
                self.notify(message, window, cx);
                return;
            }
        };
        let locator = self
            .store
            .position(row.id)
            .ok()
            .flatten()
            .map(|(_, loc)| loc)
            .unwrap_or_else(Locator::start);
        let _ = self.store.touch_opened(row.id);
        let prefs = ConfigHolder::reader(cx);
        let reader = cx.new(|cx| ReaderView::open(row, flow, prefs, locator, window, cx));
        self.tabs.push(reader);
        self.active = self.tabs.len() - 1;
        cx.notify();
    }

    fn close_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(reader) = self.tabs.get(ix).cloned() {
            self.persist_position(&reader, cx);
        }
        self.tabs.remove(ix);
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
        cx.notify();
    }

    fn persist_position(&mut self, reader: &Entity<ReaderView>, cx: &mut Context<Self>) {
        let locator = reader.read(cx).locator(cx);
        let mode = reader.read(cx).mode_name();
        let book_id = reader.read(cx).book_id();
        let _ = self.store.set_position(book_id, mode, &locator);
    }

    fn save_all(&mut self, cx: &mut Context<Self>) {
        for reader in self.tabs.clone() {
            self.persist_position(&reader, cx);
        }
        ConfigHolder::save(cx);
    }

    fn notify(
        &mut self,
        message: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.push_notification(message.into(), cx);
    }

    /// 只写配置，传播交给 `_config_watch`：推给所有阅读器 + 落盘 + 重涂主题。
    fn zoom(&mut self, delta: f32, window: &mut Window, cx: &mut Context<Self>) {
        ConfigHolder::edit_reader(cx, |p| p.font_size += delta);
        let size = ConfigHolder::reader(cx).font_size;
        let message = tf(cx, "toast.font_size", &[("size", &format!("{:.0}", size))]);
        self.notify(message, window, cx);
    }

    /// 换肤快捷键：只在**当前这一档**（浅或深）的主题里循环，改的也只是这一侧的选择。
    /// 另一侧保持不动 —— 读者之后切到深色时该看到他上次为深色挑的那套，
    /// 而不是被这次浅色循环顺手改掉。
    fn cycle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mode = cx.theme().mode;
        let names = crate::theme::theme_names(cx, mode);
        if names.is_empty() {
            return;
        }
        let dark = mode.is_dark();
        let stored = {
            let appearance = ConfigHolder::appearance(cx);
            if dark {
                appearance.dark
            } else {
                appearance.light
            }
        };
        let fallback = if dark {
            crate::theme::DEFAULT_DARK
        } else {
            crate::theme::DEFAULT_LIGHT
        };
        let index = stored
            .as_deref()
            .and_then(|name| names.iter().position(|found| found.as_ref() == name))
            .unwrap_or_else(|| {
                names
                    .iter()
                    .position(|found| found.as_ref() == fallback)
                    .unwrap_or(0)
            });
        let next = names[(index + 1) % names.len()].to_string();
        ConfigHolder::edit_appearance(cx, |appearance| {
            let slot = if dark {
                &mut appearance.dark
            } else {
                &mut appearance.light
            };
            *slot = Some(next.clone());
        });
        let message = tf(cx, "toast.theme", &[("name", &next)]);
        self.notify(message, window, cx);
    }

    /// 进设置页。`page` 是要定位到的页面下标（见 `settings_view::PAGE_*`）。
    fn open_settings(&mut self, page: usize, cx: &mut Context<Self>) {
        self.settings = Some(page);
        cx.notify();
    }

    fn close_settings(&mut self, cx: &mut Context<Self>) {
        self.settings = None;
        cx.notify();
    }

    fn open_import_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(tr(cx, "import.placeholder").to_string())
        });
        let this = cx.entity().clone();
        window.open_dialog(cx, move |dialog, _window, cx| {
            // open_dialog 的闭包是 Fn，所以每次重建都拿一份新的句柄。
            let input = input.clone();
            let this = this.clone();
            dialog
                .title(tr(cx, "import.title"))
                .child(Input::new(&input))
                .child(tr(cx, "import.hint"))
                .on_ok(move |_, window, cx| {
                    let raw = input.read(cx).value().trim().to_string();
                    if raw.is_empty() {
                        return true;
                    }
                    this.update(cx, |this, cx| {
                        this.import(PathBuf::from(shexpand(&raw)), window, cx)
                    });
                    false
                })
        });
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active_book = self.tabs.get(self.active).map(|t| t.read(cx).book_id());
        let entity = cx.entity().clone();
        let menu_entity = entity.clone();
        let import_entity = entity.clone();
        let toggle_entity = entity.clone();
        let account_entity = entity.clone();
        let collapsed = self.sidebar_collapsed;

        let toggle = || {
            let e = toggle_entity.clone();
            SidebarToggleButton::new()
                .collapsed(collapsed)
                .on_click(move |_, _, cx| {
                    e.update(cx, |this, cx| {
                        this.sidebar_collapsed = !this.sidebar_collapsed;
                        cx.notify();
                    })
                })
        };
        let import_label = tr(cx, "sidebar.import");
        let import = || {
            Button::new("import")
                .ghost()
                .small()
                .icon(IconName::Plus)
                .tooltip(import_label.clone())
                .accessibility_label(import_label.clone())
                .on_click(move |_, window, cx| {
                    import_entity.update(cx, |this, cx| this.open_import_dialog(window, cx))
                })
        };

        // 折叠后侧栏只剩 48px，扣掉组件自己给 `#header` 的 `px_2` 只有 31px 放东西，
        // 所以两个图标按钮只能竖着排。这里**不再**套 `SidebarHeader`：它自己还带一层
        // `p_2`，叠起来会把可用宽度再吃掉 16px，品牌名和按钮都会被 `overflow_hidden` 裁掉。
        let header = if collapsed {
            v_flex()
                .w_full()
                .items_center()
                .gap_1()
                .child(import())
                .child(toggle())
        } else {
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .child(Icon::new(IconName::Library).small())
                .child(div().font_weight(FontWeight::SEMIBOLD).child("Folio"))
                .child(div().flex_1())
                .child(import())
                .child(toggle())
        };

        // 底部账号入口：头像 + 读者名，进入设置并定位到「账号」页。
        // 用 Button 而不是裸 h_flex：它是应用命令，需要 hover/pressed/selected 与可访问名。
        let name = display_name(cx);
        let mut account = Button::new("account")
            .ghost()
            .small()
            .w_full()
            .selected(self.settings == Some(settings_view::PAGE_ACCOUNT))
            .tooltip(tr(cx, "sidebar.account"))
            .accessibility_label(tr(cx, "sidebar.account"))
            .on_click(move |_, _, cx| {
                account_entity.update(cx, |this, cx| {
                    this.open_settings(settings_view::PAGE_ACCOUNT, cx)
                })
            })
            .child(account_avatar(&name, 24.0, cx));
        if collapsed {
            account = account.justify_center();
        } else {
            account = account.child(div().flex_1().min_w_0().child(name));
        }

        Sidebar::new("library")
            .w(px(SIDEBAR_WIDTH))
            .collapsed(collapsed)
            .header(header)
            .child(
                SidebarMenu::new().children(self.library.clone().into_iter().enumerate().map(
                    move |(ix, book)| {
                        let entity = menu_entity.clone();
                        SidebarMenuItem::new(book.title.clone())
                            .icon(IconName::Book)
                            .active(active_book == Some(book.id))
                            .on_click(move |_, window, cx| {
                                let row = entity.read(cx).library.get(ix).cloned();
                                if let Some(row) = row {
                                    entity.update(cx, |this, cx| this.open_book(&row, window, cx));
                                }
                            })
                    },
                )),
            )
            // 同理不再套 `SidebarFooter`：它没有 style 字段，外层那层 `p_2` 覆盖不掉，
            // 叠上 `#footer` 的 `px_2` 之后 48px 的折叠栏里只剩 15px 给头像，会被裁掉。
            .footer(account)
    }

    /// 自定义标题栏：品牌名在左、当前书名在右，窗口按钮由 `TitleBar` 内建。
    fn title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let book = self
            .tabs
            .get(self.active)
            .map(|t| t.read(cx).title().clone());
        let mut bar = h_flex()
            .w_full()
            .gap_2()
            .child(Icon::new(IconName::BookOpen).small());
        // `test_support` 让 UI 集成测试能查到这个标题栏内容节点（回归门：别把标题栏又弄丢）。
        bar = bar.child(div().id("app-title").test_support().child("Folio"));
        if let Some(book) = book {
            bar = bar.child(
                div()
                    .ml_auto()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(book),
            );
        }
        TitleBar::new().child(bar)
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let zoom_out = cx.entity().clone();
        let zoom_in = cx.entity().clone();
        let theme = cx.entity().clone();
        let chapter_nav = cx.entity().clone();
        let next_chapter = cx.entity().clone();
        let settings = cx.entity().clone();
        let font_size = ConfigHolder::reader(cx).font_size;
        let has_reader = !self.tabs.is_empty();
        let no_book = tr(cx, "reader.no_book").to_string();
        let chapter_label = self
            .tabs
            .get(self.active)
            .map(|t| t.read(cx).chapter_label())
            .unwrap_or(no_book);

        Toolbar::new("reader-toolbar")
            .w_full()
            .child(
                ToolbarGroup::new("type").child(
                    Button::new("zoom-out")
                        .icon(IconName::Minus)
                        .tooltip(tr(cx, "reader.zoom_out"))
                        .on_click(move |_, window, cx| {
                            zoom_out.update(cx, |this, cx| this.zoom(-1.0, window, cx))
                        }),
                ),
            )
            .child(
                ToolbarGroup::new("type-in").child(
                    Button::new("zoom-in")
                        .icon(IconName::Plus)
                        .tooltip(tr(cx, "reader.zoom_in"))
                        .on_click(move |_, window, cx| {
                            zoom_in.update(cx, |this, cx| this.zoom(1.0, window, cx))
                        }),
                ),
            )
            .child(
                ToolbarGroup::new("theme").child(
                    Button::new("theme")
                        .icon(if cx.theme().mode.is_dark() {
                            IconName::Sun
                        } else {
                            IconName::Moon
                        })
                        .label(format!("{:.0}px", font_size))
                        .on_click(move |_, window, cx| {
                            theme.update(cx, |this, cx| this.cycle_theme(window, cx))
                        }),
                ),
            )
            .child(
                ToolbarGroup::new("settings").child(
                    Button::new("settings")
                        .icon(IconName::Settings)
                        .tooltip(tr(cx, "settings.title"))
                        .on_click(move |_, _, cx| {
                            settings.update(cx, |this, cx| {
                                this.open_settings(settings_view::PAGE_TYPOGRAPHY, cx)
                            })
                        }),
                ),
            )
            .content(div().flex_1().px_3().child(chapter_label))
            .child(
                ToolbarGroup::new("chapter").child(
                    Button::new("prev-chapter")
                        .icon(IconName::ChevronLeft)
                        .disabled(!has_reader)
                        .on_click(move |_, _, cx| {
                            chapter_nav.update(cx, |this, cx| {
                                if let Some(reader) = this.tabs.get(this.active) {
                                    reader.update(cx, |reader, cx| reader.prev_chapter(cx));
                                }
                            })
                        }),
                ),
            )
            .child(
                ToolbarGroup::new("chapter-next").child(
                    Button::new("next-chapter")
                        .icon(IconName::ChevronRight)
                        .disabled(!has_reader)
                        .on_click(move |_, _, cx| {
                            next_chapter.update(cx, |this, cx| {
                                if let Some(reader) = this.tabs.get(this.active) {
                                    reader.update(cx, |reader, cx| reader.next_chapter(cx));
                                }
                            })
                        }),
                ),
            )
    }

    fn status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (progress, position) = match self.tabs.get(self.active) {
            Some(reader) => (
                reader.read(cx).progress(cx),
                reader.read(cx).chapter_position(cx),
            ),
            None => (0.0, String::new()),
        };
        StatusBar::new()
            .left(book_count(cx, self.library.len()))
            .child(position)
            .right(format!("{:.0}%", progress * 100.0))
    }

    /// 设置页：标题栏（窗口 chrome）+ 页头（返回 + 标题）+ `Settings` 整页。
    ///
    /// `Settings` 内部是一个 `h_resizable`（它自己的页面导航 + 正文两栏），它和它的
    /// 正文都要一个**确定的**盒子，所以这里给的是 `flex_1().min_h_0()`，
    /// 而不是对话框那种滚动容器。
    ///
    /// 侧栏保持可见：它是全局导航，「选一本书」就是回到阅读的那条路。
    fn settings_screen(&self, page: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let back = cx.entity().clone();
        // 组件的 `Settings` 只在**第一次**渲染时读 `default_selected_index` —— 它的选择
        // 状态挂在 `window.use_keyed_state(id)` 上，之后再传这个参数也没用。所以
        // 「从账号进账号页、从工具条进文字页」只能靠给每个入口一个自己的 id。
        let id = format!("reader-settings-{page}");
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(self.title_bar(cx))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("settings-back")
                            .ghost()
                            .small()
                            .icon(IconName::ChevronLeft)
                            .label(tr(cx, "settings.back"))
                            .on_click(move |_, _, cx| {
                                back.update(cx, |this, cx| this.close_settings(cx))
                            }),
                    )
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(tr(cx, "settings.title")),
                    ),
            )
            .child(
                div().flex_1().min_h_0().child(
                    Settings::new(id)
                        .pages(settings_view::pages(cx))
                        .default_selected_index(SelectIndex {
                            page_ix: page,
                            group_ix: None,
                        }),
                ),
            )
    }

    fn reading_screen(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().clone();
        let tabs = self.tabs.clone();
        let active = self.active;

        let tab_bar = TabBar::new("books")
            .pill()
            .selected_index(active)
            .on_click({
                let entity = entity.clone();
                move |ix: &usize, _, cx| {
                    entity.update(cx, |this, cx| {
                        // 切走之前先把当前这本书的位置落库，否则进度只会在关标签时才保存。
                        this.save_all(cx);
                        this.active = *ix;
                        cx.notify();
                    })
                }
            })
            .children(tabs.iter().enumerate().map(|(ix, reader)| {
                let title = reader.read(cx).title().clone();
                let entity = entity.clone();
                Tab::new().label(title).selected(ix == active).suffix(
                    Button::new(format!("close-tab-{ix}"))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .on_click(move |_, _, cx| {
                            entity.update(cx, |this, cx| this.close_tab(ix, cx))
                        }),
                )
            }));

        let area: AnyElement = match self.tabs.get(active) {
            Some(reader) => reader.clone().into_any_element(),
            None => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr(cx, "reader.empty_hint")),
                )
                .child(
                    // 空库时的主要动作，但它是「加一条记录」这类普通命令，不是决策区的
                    // 默认提交 —— 按 Design Guides 用默认样式，不占 primary。
                    Button::new("empty-import")
                        .label(tr(cx, "import.submit"))
                        .icon(IconName::Plus)
                        .on_click({
                            let entity = entity.clone();
                            move |_, window, cx| {
                                entity.update(cx, |this, cx| this.open_import_dialog(window, cx))
                            }
                        }),
                )
                .into_any_element(),
        };

        v_flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .child(self.title_bar(cx))
            .child(self.toolbar(cx))
            .child(tab_bar)
            .child(div().flex_1().min_h_0().overflow_hidden().child(area))
            .child(self.status_bar(cx))
    }
}

impl Render for FolioApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 阅读视图量不到 flex 之后的真实栏宽，所以由外壳把侧栏占位报给它。
        // 这个数必须跟着折叠态变：折叠后侧栏只有 48px，仍按 260 预留会让正文
        // 被挤进一个过窄的居中列、右侧空一大块。
        let reserve = if self.sidebar_collapsed {
            SIDEBAR_COLLAPSED_WIDTH
        } else {
            SIDEBAR_WIDTH
        };
        for reader in &self.tabs {
            reader.update(cx, |reader, cx| reader.set_pane_reserve(reserve, cx));
        }

        // 侧栏直通窗口顶部：标题栏只在右侧内容上方，窗口按钮仍在右上角。
        let content: AnyElement = match self.settings {
            Some(page) => self.settings_screen(page, cx).into_any_element(),
            None => self.reading_screen(cx).into_any_element(),
        };
        h_flex()
            .size_full()
            .relative()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.sidebar(cx))
            .child(content)
    }
}

/// 展开 `~`，让用户能在导入框里直接写 `~/Books/x.md`。
fn shexpand(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{home}/{rest}");
    }
    path.to_string()
}

/// 桌面入口：装主题集、开书库、开窗。
///
/// 这段原先在 `main.rs` 里。合进一个 crate 后 bin 需要一份「桌面 / 移动」共用的门面，
/// 就挪到这里，`main.rs` 只按 target 分流（移动端真正的入口在 `crate::mobile`）。
pub fn run_desktop() {
    use gpui_kit::{App, Font, TitlebarOptions, size};

    gpui_kit::application()
        // 默认图标包不含 library/book/circle-user 等，换 AllAssets 才拿得到全套 Lucide 图标。
        .with_assets(gpui_kit::assets::AllAssets)
        .run(|cx: &mut App| {
            gpui_kit::init(cx);

            // 主题集要在 `init` 之后装：那一步才把 `ThemeRegistry` 这个 Global 建出来。
            // 先装内置的（构建时从依赖源码嵌进来），再装读者自己放进 `themes/` 的，
            // 后者的同名项按 registry 的规则不覆盖已有名字，所以顺序决定谁优先。
            crate::theme::register_builtin_themes(cx);
            crate::theme::register_user_themes(cx);

            // 配置先读出来，并立刻把界面语言推给全局：下面任何一条要给人看的消息
            // （包括书库打不开）都得用读者自己选的语言写。
            let config = Config::load();
            config.language.locale.apply();

            let store = match Store::open_default() {
                Ok(store) => store,
                Err(e) => {
                    eprintln!(
                        "{}",
                        folio_core::i18n::t_with("error.database_unopenable", &[("reason", &e)])
                    );
                    std::process::exit(1);
                }
            };
            // 配置存成按系统标准目录的 JSON 文件；这里把落盘目标交给配置持有者。
            cx.set_global(crate::config_model::ConfigPath(Some(
                folio_core::config_path(),
            )));

            // 命令行传入的路径直接导入并打开：既是「用 Folio 打开这本书」的正常用法，
            // 也让端到端验证可以不依赖 GUI 自动化。
            let startup: Vec<PathBuf> = std::env::args()
                .skip(1)
                .filter(|arg| !arg.starts_with('-'))
                .map(PathBuf::from)
                .collect();

            // 阅读字体是否已由 fontconfig 解析到，决定「字体随包」的优先级。
            let probe = Font {
                family: config.reader.font_family.clone().into(),
                ..Default::default()
            };
            println!(
                "字体探测：{} -> {:?}",
                config.reader.font_family,
                cx.text_system().resolve_font(&probe)
            );

            // 自定义标题栏：以 `TitleBar::window_options()` 为基底（透明标题栏 +
            // 由应用自己负责拖拽），再补上 Wayland 窗口分组需要的 app_id。
            // `title` 只用于任务栏/切换器显示，标题栏本身由 FolioApp 绘制。
            let mut options = TitleBar::window_options();
            options.titlebar = Some(TitlebarOptions {
                title: Some("Folio".into()),
                ..TitleBar::title_bar_options()
            });
            options.app_id = Some("dev.folio.Reader".into());
            options.window_min_size = Some(size(px(520.), px(360.)));

            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| FolioApp::new(store, config, startup, window, cx))
            })
            .expect("failed to open Folio window");
        });
}
