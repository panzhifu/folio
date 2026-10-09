//! 桌面阅读页：工具条 + 正文 + 状态栏。
//!
//! 从旧的 `desktop/app.rs` 的 `reading_screen` 搬来，按决策**去掉多书标签**（两端单本）。
//! 它现在是「被外壳 push 进书架导航栈」的一个页面，不再由外壳自己持有一块区域。

use folio_core::Locator;
use gpui_kit::assets::IconName;
use gpui_kit::base::{NavMotion, NavStackState};
use gpui_kit::component::button::Button;
use gpui_kit::component::status_bar::StatusBar;
use gpui_kit::component::toolbar::{Toolbar, ToolbarGroup};
use gpui_kit::component::{ActiveTheme, Disableable, v_flex};
use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription,
    WeakEntity, Window, div,
};

use super::desktop::ReaderView;
use super::{advance_chapter, can_advance, persist, position_label, set_reserve};
use crate::ui::features::settings;
use crate::ui::shared::config_model::ConfigHolder;
use crate::ui::shared::l10n::tr;
use crate::ui::widgets::muted_label;

/// 被 push 进书架栈的阅读页。打开前是空提示。
pub struct ReadingPage {
    reader: Option<Entity<ReaderView>>,
    book_id: Option<i64>,
    /// 外壳报告阅读栏之外的宽度（侧栏 / 图标栏），版心据此收窄。
    reserve: f32,
    /// 书架导航栈：顶部那颗返回键 pop 它。
    stack: WeakEntity<NavStackState>,
    _config_watch: Subscription,
}

impl ReadingPage {
    pub fn new(stack: WeakEntity<NavStackState>, cx: &mut Context<Self>) -> Self {
        // 配置一改就推给已打开的阅读器：改字号/行距当场重排。
        let handle = ConfigHolder::handle(cx);
        let _config_watch = cx.observe(&handle, |this, _, cx| {
            if let Some(reader) = this.reader.clone() {
                let prefs = ConfigHolder::reader(cx);
                reader.update(cx, |reader, cx| reader.apply_prefs(prefs, cx));
            }
            cx.notify();
        });
        Self {
            reader: None,
            book_id: None,
            reserve: 0.0,
            stack,
            _config_watch,
        }
    }

    /// 打一本书：从书库读章节与上次位置，构造阅读视图。
    pub fn open(&mut self, book_id: i64, cx: &mut Context<Self>) {
        let Some(store) = crate::ui::shared::app_store::handle(cx) else {
            return;
        };
        let Ok(Some(row)) = store.book(book_id) else {
            return;
        };
        let Some(flow) = store.load_flow(book_id).ok().flatten() else {
            return;
        };
        let locator = store
            .position(book_id)
            .ok()
            .flatten()
            .map(|(_, locator)| locator)
            .unwrap_or_else(Locator::start);
        let _ = store.touch_opened(book_id);
        let prefs = ConfigHolder::reader(cx);
        let reserve = self.reserve;
        let reader = cx.new(|cx| {
            let mut reader = ReaderView::open(&row, flow, prefs, locator, cx);
            reader.set_pane_reserve(reserve, cx);
            reader
        });
        self.reader = Some(reader);
        self.book_id = Some(book_id);
        cx.notify();
    }

    /// 关掉当前阅读：先把位置落库。外壳在 pop 之前调它。
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if let Some(reader) = self.reader.take() {
            reader.update(cx, |reader, cx| persist(reader, cx));
        }
        self.book_id = None;
        cx.notify();
    }

    pub fn book_id(&self) -> Option<i64> {
        self.book_id
    }

    /// 外壳报告阅读栏之外的宽度。
    pub fn set_reserve(&mut self, reserve: f32, cx: &mut Context<Self>) {
        self.reserve = reserve;
        if let Some(reader) = self.reader.clone() {
            reader.update(cx, |reader, cx| set_reserve(reader, reserve, cx));
        }
    }

    /// 这一屏没有可收的浮层：桌面的工具条与状态栏是常驻 chrome，不是点出来的一层。
    ///
    /// 签名与移动那侧同名方法一致，是因为外壳的返回键要先进这一层再决定 pop ——
    /// 外壳不写 `cfg(mobile)`，所以这条判据落在两边各自的文件里。
    pub fn dismiss_chrome(&mut self, _cx: &mut Context<Self>) -> bool {
        false
    }

    fn advance(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(reader) = self.reader.clone() {
            reader.update(cx, |reader, cx| advance_chapter(reader, delta, cx));
        }
    }

    /// 回书架：pop 书架栈。落库由外壳在 `NavStackEvent::Popped` 时做。
    fn pop(&mut self, cx: &mut Context<Self>) {
        let _ = self
            .stack
            .update(cx, |state, cx| state.pop(NavMotion::Animated, cx));
    }
}

impl Render for ReadingPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(reader) = self.reader.clone() else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(muted_label(tr(cx, "reader.no_book"), cx));
        };

        let font_size = ConfigHolder::reader(cx).font_size;
        let can_prev = can_advance(&*reader.read(cx), -1);
        let can_next = can_advance(&*reader.read(cx), 1);
        let crumb = position_label(&*reader.read(cx), cx);
        let progress = reader.read(cx).progress(cx);

        let page_prev = cx.entity().clone();
        let page_next = cx.entity().clone();
        let page_back = cx.entity().clone();

        let toolbar = Toolbar::new("reader-toolbar")
            .w_full()
            .child(
                ToolbarGroup::new("nav").child(
                    Button::new("shelf-back")
                        .icon(IconName::ArrowLeft)
                        .label(tr(cx, "shelf.back").to_string())
                        .tooltip(tr(cx, "shelf.back"))
                        .accessibility_label(tr(cx, "shelf.back"))
                        .on_click(move |_, _, cx| page_back.update(cx, |this, cx| this.pop(cx))),
                ),
            )
            .child(
                ToolbarGroup::new("type").child(
                    Button::new("zoom-out")
                        .icon(IconName::Minus)
                        .tooltip(tr(cx, "reader.zoom_out"))
                        .on_click(|_, _, cx| {
                            ConfigHolder::edit_reader(cx, |prefs| prefs.font_size -= 1.0)
                        }),
                ),
            )
            .child(
                ToolbarGroup::new("type-in").child(
                    Button::new("zoom-in")
                        .icon(IconName::Plus)
                        .label(format!("{font_size:.0}px"))
                        .on_click(|_, _, cx| {
                            ConfigHolder::edit_reader(cx, |prefs| prefs.font_size += 1.0)
                        }),
                ),
            )
            .child(
                ToolbarGroup::new("settings").child(
                    Button::new("settings")
                        .icon(IconName::Settings)
                        .tooltip(tr(cx, "settings.open"))
                        .accessibility_label(tr(cx, "settings.open"))
                        .on_click(|_, _, cx| settings::open(cx)),
                ),
            )
            .content(div().flex_1().px_3().child(crumb.clone()))
            .child(
                ToolbarGroup::new("chapter").child(
                    Button::new("prev-chapter")
                        .icon(IconName::ChevronLeft)
                        .disabled(!can_prev)
                        .on_click(move |_, _, cx| {
                            page_prev.update(cx, |this, cx| this.advance(-1, cx))
                        }),
                ),
            )
            .child(
                ToolbarGroup::new("chapter-next").child(
                    Button::new("next-chapter")
                        .icon(IconName::ChevronRight)
                        .disabled(!can_next)
                        .on_click(move |_, _, cx| {
                            page_next.update(cx, |this, cx| this.advance(1, cx))
                        }),
                ),
            );

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(toolbar)
            .child(div().flex_1().min_h_0().overflow_hidden().child(reader))
            .child(
                StatusBar::new()
                    .child(crumb)
                    .right(format!("{:.0}%", progress * 100.0)),
            )
    }
}
