use std::path::PathBuf;

use folio_core::store::BookRow;
use folio_core::{Locator, Prefs, Store, ThemeName, open};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::sidebar::{
    Sidebar, SidebarFooter, SidebarHeader, SidebarMenu, SidebarMenuItem, SidebarToggleButton,
};
use gpui_kit::component::status_bar::StatusBar;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::toolbar::{Toolbar, ToolbarGroup};
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable, Theme, ThemeMode, WindowExt, v_flex,
};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Window, div, px,
};

use crate::prefs_model::PrefsHolder;
use crate::reader::ReaderView;
use crate::settings_view;

/// 侧栏展开宽度。阅读视图按它预留版心，所以这个数只能有一份。
pub const SIDEBAR_WIDTH: f32 = 260.0;

pub struct FolioApp {
    store: Store,
    library: Vec<BookRow>,
    tabs: Vec<Entity<ReaderView>>,
    active: usize,
    sidebar_collapsed: bool,
    /// 偏好实体的句柄由 `PrefsHolder` 这个 Global 持有，这里只留订阅：
    /// 它一旦被 drop，阅读器就不会随设置变化重排。
    _prefs_watch: Subscription,
}

impl FolioApp {
    pub fn new(
        store: Store,
        prefs: Prefs,
        startup: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let library = store.list_books().unwrap_or_else(|e| {
            eprintln!("读取书库失败：{e}");
            Vec::new()
        });
        let prefs_state = PrefsHolder::install(prefs, cx);
        apply_theme(&PrefsHolder::get(cx), cx);
        // 偏好改一次就：重涂主题 → 推给所有已打开的书 → 落库。
        // 没有这一步，设置面板改完字号要重开书才生效。
        let _prefs_watch = cx.observe(&prefs_state, |this, _, cx| {
            let prefs = PrefsHolder::get(cx);
            apply_theme(&prefs, cx);
            for reader in &this.tabs {
                reader.update(cx, |reader, cx| reader.apply_prefs(prefs.clone(), cx));
            }
            let _ = this.store.set_prefs(&prefs);
            cx.notify();
        });
        let mut this = Self {
            store,
            library,
            tabs: Vec::new(),
            active: 0,
            sidebar_collapsed: false,
            _prefs_watch,
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
                self.notify(format!("导入失败：{e}"), window, cx);
                return;
            }
        };
        let folio_core::Content::Reflow(flow) = content;
        let book_id = match self.store.add_book(&path, &meta) {
            Ok(id) => id,
            Err(e) => {
                self.notify(format!("写入书库失败：{e}"), window, cx);
                return;
            }
        };
        if let Err(e) = self.store.save_flow(book_id, &flow) {
            self.notify(format!("缓存章节失败：{e}"), window, cx);
            return;
        }
        self.refresh_library(cx);
        let row = self.store.book(book_id).ok().flatten();
        match row {
            Some(row) => self.open_book(&row, window, cx),
            None => self.notify("导入后找不到这本书", window, cx),
        }
    }

    fn open_book(&mut self, row: &BookRow, window: &mut Window, cx: &mut Context<Self>) {
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
                self.notify("这本书还没有解析过，请重新导入", window, cx);
                return;
            }
            Err(e) => {
                self.notify(format!("读取章节失败：{e}"), window, cx);
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
        let prefs = PrefsHolder::get(cx);
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
        let _ = self.store.set_prefs(&PrefsHolder::get(cx));
    }

    fn notify(
        &mut self,
        message: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.push_notification(message.into(), cx);
    }

    /// 只写偏好，传播交给 `_prefs_watch`：推给所有阅读器 + 落库 + 重涂主题。
    fn zoom(&mut self, delta: f32, window: &mut Window, cx: &mut Context<Self>) {
        PrefsHolder::edit(cx, |p| p.font_size += delta);
        let size = PrefsHolder::get(cx).font_size;
        self.notify(format!("字号 {:.0}px", size), window, cx);
    }

    fn cycle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        PrefsHolder::edit(cx, |p| {
            p.theme = match p.theme {
                ThemeName::Light => ThemeName::Dark,
                ThemeName::Dark | ThemeName::Sepia => ThemeName::Light,
            };
        });
        let theme = PrefsHolder::get(cx).theme;
        self.notify(format!("主题：{theme:?}"), window, cx);
    }

    fn open_settings_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::setting::{SettingPage, Settings};

        window.open_dialog(cx, move |dialog, _window, _cx| {
            // 每次重建页面：字段本身只是 (取值, 赋值) 对的包装，重建无成本，
            // 而 open_dialog 的闭包是 Fn，不能把非 Copy 的 builder 移进去。
            let page = SettingPage::new("阅读")
                .icon(IconName::BookOpen)
                .default_open(true)
                .groups(vec![
                    settings_view::typography_group(),
                    settings_view::layout_group(),
                    settings_view::font_group(),
                ]);
            dialog
                .title("阅读设置")
                .child(Settings::new("reader-settings").page(page))
        });
    }

    fn open_import_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input =
            cx.new(|cx| InputState::new(window, cx).placeholder("/path/to/book.md 或 .txt"));
        let this = cx.entity().clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            // open_dialog 的闭包是 Fn，所以每次重建都拿一份新的句柄。
            let input = input.clone();
            let this = this.clone();
            dialog
                .title("导入书籍")
                .child(Input::new(&input))
                .child("一期只支持 Markdown 与纯文本，EPUB 在 M2。")
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
        let footer_entity = entity.clone();
        let toggle_entity = entity.clone();

        Sidebar::new("library")
            .w(px(SIDEBAR_WIDTH))
            .collapsed(self.sidebar_collapsed)
            .header(
                SidebarHeader::new()
                    .child(IconName::Library)
                    .child("Folio")
                    .child(
                        SidebarToggleButton::new()
                            .collapsed(self.sidebar_collapsed)
                            .on_click(move |_, _, cx| {
                                toggle_entity.update(cx, |this, cx| {
                                    this.sidebar_collapsed = !this.sidebar_collapsed;
                                    cx.notify();
                                })
                            }),
                    ),
            )
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
            .footer(
                SidebarFooter::new().child(
                    Button::new("import")
                        .label("导入书籍")
                        .icon(IconName::Plus)
                        .on_click(move |_, window, cx| {
                            footer_entity.update(cx, |this, cx| this.open_import_dialog(window, cx))
                        }),
                ),
            )
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let zoom_out = cx.entity().clone();
        let zoom_in = cx.entity().clone();
        let theme = cx.entity().clone();
        let chapter_nav = cx.entity().clone();
        let next_chapter = cx.entity().clone();
        let settings = cx.entity().clone();
        let font_size = PrefsHolder::get(cx).font_size;
        let has_reader = !self.tabs.is_empty();
        let chapter_label = self
            .tabs
            .get(self.active)
            .map(|t| t.read(cx).chapter_label())
            .unwrap_or_else(|| "没有打开的书".to_string());

        Toolbar::new("reader-toolbar")
            .w_full()
            .child(
                ToolbarGroup::new("type").child(
                    Button::new("zoom-out")
                        .icon(IconName::Minus)
                        .tooltip("减小字号")
                        .on_click(move |_, window, cx| {
                            zoom_out.update(cx, |this, cx| this.zoom(-1.0, window, cx))
                        }),
                ),
            )
            .child(
                ToolbarGroup::new("type-in").child(
                    Button::new("zoom-in")
                        .icon(IconName::Plus)
                        .tooltip("增大字号")
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
                        .tooltip("阅读设置")
                        .on_click(move |_, window, cx| {
                            settings.update(cx, |this, cx| this.open_settings_dialog(window, cx))
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
                reader.read(cx).chapter_position(),
            ),
            None => (0.0, String::new()),
        };
        StatusBar::new()
            .left(format!("{} 本书", self.library.len()))
            .child(position)
            .right(format!("{:.0}%", progress * 100.0))
    }
}

impl Render for FolioApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 阅读视图量不到 flex 之后的真实栏宽，所以由外壳把侧栏占位报给它。
        // 折叠时也按展开宽度预留：版心宁可窄一点，也不能比实际栏宽更宽 ——
        // 宽了就会被裁掉，标题会整块跑到栏外。
        for reader in &self.tabs {
            reader.update(cx, |reader, cx| reader.set_pane_reserve(SIDEBAR_WIDTH, cx));
        }
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
                .gap_2()
                .child("从左侧书架选一本书，或点「导入书籍」")
                .child(
                    Button::new("empty-import")
                        .primary()
                        .label("导入书籍")
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

        div()
            .size_full()
            .relative()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div().size_full().flex().child(self.sidebar(cx)).child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .size_full()
                        .child(self.toolbar(cx))
                        .child(tab_bar)
                        .child(div().flex_1().min_h_0().overflow_hidden().child(area))
                        .child(self.status_bar(cx)),
                ),
            )
    }
}

fn apply_theme(prefs: &Prefs, cx: &mut App) {
    let mode = match prefs.theme {
        // Sepia 需要 ThemeRegistry 注册自定义主题，见任务「M1-前置 主题注册与字体随包」。
        ThemeName::Dark => ThemeMode::Dark,
        ThemeName::Light | ThemeName::Sepia => ThemeMode::Light,
    };
    Theme::change(mode, None, cx);
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
