//! 移动阅读页：正文 + 干净态的两行小字 + 点正文中间浮出的顶栏与底栏。
//!
//! 形态照 Reeden 的阅读界面量出来（1080×2400 @3.0）：干净态只剩左上角一行章名与底部
//! 一行进度；点正文中间才浮出顶栏（返回 + 书名）与底栏（进度条 + 一排入口）。
//! 入口就地换面板，**不弹浮层** —— 浮层在 Android 上现在点不住（同 `shelf` 里
//! 「书要拷到哪儿」那块面板记的那条事实），而目录点不动等于没有目录。
//!
//! 顶栏与底栏**压在正文之上**，不推动正文重排：读者点一下屏幕，字不该跳走。
//! 正文只让出干净态那两条带的高度，那两个数住在 [`chrome`] 里，布局与视口算法共用它。
//!
//! 点击分区也在 [`chrome`] 里判：那里是纯函数，宿主的 `cargo test` 直接跑，
//! 这一屏只把结果落成动作。

use std::rc::Rc;

use folio_core::config::Mode;
use gpui_kit::assets::IconName;
use gpui_kit::base::{NavMotion, NavStackState, v_virtual_list};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState, SliderValue};
use gpui_kit::component::{
    ActiveTheme, Disableable, Sizable, StyledExt as _, ThemeMode, ThemeStyled as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Corners, ElementId, Entity, FontWeight,
    IntoElement, ParentElement, Pixels, Render, SharedString, Size, StatefulInteractiveElement,
    Styled, Subscription, WeakEntity, Window, div, px, size,
};

use super::chrome::{
    self, Chrome, Effect, FONT_SIZE_RANGE, LINE_HEIGHT_RANGE, Panel, percent_label, zone,
};
use super::mobile::ReaderView;
use super::{advance_chapter, book_id, can_advance, persist, set_reserve};
use crate::ui::features::settings::AccountView;
use crate::ui::shared::config_model::ConfigHolder;
use crate::ui::shared::l10n::tr;

/// 目录一行的行高（逻辑像素）。拇指能稳定命中的下限，也是虚拟列表的逐项尺寸。
const TOC_ROW_HEIGHT: f32 = 48.0;

pub struct ReadingPage {
    reader: Entity<ReaderView>,
    /// 外壳报告底部导航栏的高度，正文据此缩短有效视口。
    reserve: f32,
    stack: WeakEntity<NavStackState>,
    /// 浮层的开合与面板档位。见 [`chrome`]。
    chrome: Chrome,
    /// 进度滑条。`Slider` 是有状态组件，句柄必须跨帧持有 —— 每帧新建会把拖动打断。
    progress: Entity<SliderState>,
    font_size: Entity<SliderState>,
    line_height: Entity<SliderState>,
    /// 目录虚拟列表的逐项尺寸。只在章数变了时重建，不每帧分配一张千项的表。
    toc_sizes: Rc<Vec<Size<Pixels>>>,
    toc_len: usize,
    _watches: Vec<Subscription>,
}

impl ReadingPage {
    pub fn new(stack: WeakEntity<NavStackState>, cx: &mut Context<Self>) -> Self {
        let reader = cx.new(|cx| ReaderView::boot(cx));
        let progress = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(1.0)
                .step(0.001)
                .default_value(0.0)
        });
        let font_size = cx.new(|_| {
            SliderState::new()
                .min(FONT_SIZE_RANGE.0)
                .max(FONT_SIZE_RANGE.1)
                .step(0.5)
                .default_value(ConfigHolder::reader(cx).font_size)
        });
        let line_height = cx.new(|_| {
            SliderState::new()
                .min(LINE_HEIGHT_RANGE.0)
                .max(LINE_HEIGHT_RANGE.1)
                .step(0.05)
                .default_value(ConfigHolder::reader(cx).line_height)
        });
        // 三条滑条各订阅一次：拖动当场生效（正文滚到那一处 / 换字号重排），
        // 所以这里没有「应用」按钮，也没有改了不生效的中间态。
        let watches = vec![
            cx.subscribe(&progress, |this: &mut Self, _, event, cx| {
                if let SliderEvent::Change(value) = event {
                    this.reader
                        .update(cx, |reader, cx| reader.seek_ratio(value.end(), cx));
                }
            }),
            cx.subscribe(&font_size, |_, _, event, cx| {
                if let SliderEvent::Change(value) = event {
                    let value = value.end();
                    ConfigHolder::edit_reader(cx, |prefs| prefs.font_size = value);
                }
            }),
            cx.subscribe(&line_height, |_, _, event, cx| {
                if let SliderEvent::Change(value) = event {
                    let value = value.end();
                    ConfigHolder::edit_reader(cx, |prefs| prefs.line_height = value);
                }
            }),
        ];
        Self {
            reader,
            reserve: 0.0,
            stack,
            chrome: Chrome::default(),
            progress,
            font_size,
            line_height,
            toc_sizes: Rc::new(Vec::new()),
            toc_len: 0,
            _watches: watches,
        }
    }

    pub fn open(&mut self, book_id: i64, cx: &mut Context<Self>) {
        self.reader
            .update(cx, |reader, cx| reader.open_book(book_id, cx));
    }

    /// 落库。外壳在 pop 之前调它。
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.reader.update(cx, |reader, cx| persist(reader, cx));
    }

    pub fn book_id(&self, cx: &App) -> Option<i64> {
        book_id(&*self.reader.read(cx))
    }

    pub fn set_reserve(&mut self, reserve: f32, cx: &mut Context<Self>) {
        self.reserve = reserve;
        self.reader
            .update(cx, |reader, cx| set_reserve(reader, reserve, cx));
    }

    /// 浮层挡着的时候，系统返回先收它；收干净了才轮到 pop 这一页。
    ///
    /// 返回 `true` 表示这次返回已被浮层消费，调用方不该再往下走。
    pub fn dismiss_chrome(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.chrome.dismiss() {
            return false;
        }
        cx.notify();
        true
    }

    fn pop(&mut self, cx: &mut Context<Self>) {
        let _ = self
            .stack
            .update(cx, |state, cx| state.pop(NavMotion::Animated, cx));
    }

    /// 正文上的一击：分区判完就照结果动。见 [`chrome::Chrome::handle_body_tap`]。
    fn on_body_tap(&mut self, event: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let width = f32::from(window.viewport_size().width);
        let tap = zone(f32::from(event.position().x), width);
        match self.chrome.handle_body_tap(tap) {
            Effect::OpenChrome | Effect::DismissChrome => cx.notify(),
            Effect::PrevPage => {
                self.reader
                    .update(cx, |reader, cx| reader.scroll_page(-1, cx));
            }
            Effect::NextPage => {
                self.reader
                    .update(cx, |reader, cx| reader.scroll_page(1, cx));
            }
        }
    }

    /// 把「别处也改得动」的三个数推回滑条：正文滚了要挪进度滑块，设置页改了字号
    /// 要挪字号滑块。
    ///
    /// 带阈值比较：拖动的时候滑条自己就是权威，每帧再按正文读回来的数写回去，
    /// 就是拿手指算出来的值和滚动算出来的值互相追。
    fn sync_sliders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let progress = self.reader.read(cx).progress();
        let prefs = ConfigHolder::reader(cx);
        let wanted = [
            (&self.progress, progress),
            (&self.font_size, prefs.font_size),
            (&self.line_height, prefs.line_height),
        ];
        for (state, value) in wanted {
            if (state.read(cx).value().end() - value).abs() > 0.005 {
                state.update(cx, |state, cx| state.set_value(value, window, cx));
            }
        }
    }

    /// 干净态页眉：只有章名，一行小字。
    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let chapter: SharedString = self.reader.read(cx).chapter_title().into();
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(chrome::HEADER_HEIGHT))
            .px_4()
            .flex()
            .items_center()
            .overflow_hidden()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .whitespace_nowrap()
                    .when(!chapter.is_empty(), |label| label.child(chapter)),
            )
            .into_any_element()
    }

    /// 干净态页脚：左边章内位置，右边进度。两端各占一条边，所以是 `justify_between`。
    fn footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let position = self.reader.read(cx).chapter_position(cx);
        let percent = percent_label(self.reader.read(cx).progress());
        h_flex()
            .absolute()
            .bottom_0()
            .left_0()
            .right_0()
            .h(px(chrome::FOOTER_HEIGHT))
            .px_4()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(position),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(percent),
            )
            .into_any_element()
    }

    /// 顶栏：返回 + 书名。只有这两样，因为这一屏现在只有这两件事可做 ——
    /// 摆一排点不动的图标比空着更糟。
    fn top_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let title = self.reader.read(cx).title();
        let radius = cx.theme().radius_lg;
        let page = cx.entity().clone();
        let back_label = tr(cx, "shelf.back");
        h_flex()
            .id("reader-top-bar")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(chrome::TOP_BAR_HEIGHT))
            .items_center()
            .pl_1()
            .pr_3()
            .popover_style(cx)
            // 只圆下面两个角：这一条是从屏幕顶边长出来的，上面不该有缝。
            .corner_radii(Corners {
                top_left: px(0.),
                top_right: px(0.),
                bottom_left: radius,
                bottom_right: radius,
            })
            .child(
                Button::new("shelf-back")
                    .ghost()
                    .icon(IconName::ArrowLeft)
                    .tooltip(back_label.clone().to_string())
                    .accessibility_label(back_label)
                    .on_click(move |_, _, cx| page.update(cx, |this, cx| this.pop(cx))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(title),
            )
            .into_any_element()
    }

    /// 底栏：一块卡片，从上到下是「面板（可有可无）+ 状态行 + 进度条 + 一排入口」。
    ///
    /// 面板和入口条在**同一块**表面里，不是两张卡片：两张会各有一套圆角与投影，
    /// 接缝处读起来是两个东西叠着，而它们说的是同一件事。
    fn bottom_bar(&self, cx: &mut Context<Self>, window: &mut Window) -> AnyElement {
        let radius = cx.theme().radius_lg;
        let mut card = v_flex()
            .id("reader-bottom-bar")
            .absolute()
            .bottom_0()
            .left_0()
            .right_0()
            .px_4()
            .pt_3()
            .pb_2()
            .gap_3()
            .popover_style(cx)
            .corner_radii(Corners {
                top_left: radius,
                top_right: radius,
                bottom_left: px(0.),
                bottom_right: px(0.),
            });
        card = match self.chrome.panel() {
            Panel::None => card,
            Panel::Toc => card.child(self.toc_panel(cx, window)),
            Panel::Typography => card.child(self.typography_panel(cx)),
        };
        card.child(self.status_row(cx))
            .child(self.progress_row(cx))
            .child(self.entry_row(cx))
            .into_any_element()
    }

    /// 状态行：章内位置与进度各占一端的边。
    fn status_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let position = self.reader.read(cx).chapter_position(cx);
        let percent = percent_label(self.reader.read(cx).progress());
        let progress_label = format!("{} {}", tr(cx, "reader.progress"), percent);
        h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(position),
            )
            .child(div().text_sm().child(progress_label))
            .into_any_element()
    }

    /// 进度条两端是上一章 / 下一章：手指在条上既能「跳到这一章的哪儿」，
    /// 也能顺手跨到隔壁一章。
    fn progress_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let reader = self.reader.read(cx);
        let can_prev = can_advance(&*reader, -1);
        let can_next = can_advance(&*reader, 1);
        let page_prev = cx.entity().clone();
        let page_next = cx.entity().clone();
        let prev_label = tr(cx, "reader.prev_chapter");
        let next_label = tr(cx, "reader.next_chapter");
        h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .child(
                Button::new("prev-chapter")
                    .ghost()
                    .small()
                    .icon(IconName::ChevronLeft)
                    .tooltip(prev_label.clone().to_string())
                    .accessibility_label(prev_label)
                    .disabled(!can_prev)
                    .on_click(move |_, _, cx| {
                        page_prev.update(cx, |this, cx| this.advance(-1, cx))
                    }),
            )
            .child(Slider::new(&self.progress).flex_1().min_w_0())
            .child(
                Button::new("next-chapter")
                    .ghost()
                    .small()
                    .icon(IconName::ChevronRight)
                    .tooltip(next_label.clone().to_string())
                    .accessibility_label(next_label)
                    .disabled(!can_next)
                    .on_click(move |_, _, cx| page_next.update(cx, |this, cx| this.advance(1, cx))),
            )
            .into_any_element()
    }

    /// 一排入口。目录与排版是面板（选中态留在自己这一格的表面上），夜间是一个当场生效
    /// 的动作，设置是另一屏 —— 三件事不同，所以只有前两格吃 `selected`。
    fn entry_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let panel = self.chrome.panel();
        let dark = cx.theme().mode == ThemeMode::Dark;
        let page_toc = cx.entity().clone();
        let page_type = cx.entity().clone();
        let page_night = cx.entity().clone();
        let page_settings = cx.entity().clone();
        let night_label = tr(cx, if dark { "reader.day" } else { "reader.night" });
        h_flex()
            .w_full()
            .items_center()
            .child(
                Button::new("entry-toc")
                    .ghost()
                    .small()
                    .flex_1()
                    .justify_center()
                    .selected(panel == Panel::Toc)
                    .icon(IconName::ListTree)
                    .label(tr(cx, "reader.toc").to_string())
                    .on_click(move |_, _, cx| {
                        page_toc.update(cx, |this, cx| this.pick(Panel::Toc, cx))
                    }),
            )
            .child(
                Button::new("entry-typography")
                    .ghost()
                    .small()
                    .flex_1()
                    .justify_center()
                    .selected(panel == Panel::Typography)
                    .icon(IconName::Type)
                    .label(tr(cx, "page.typography").to_string())
                    .on_click(move |_, _, cx| {
                        page_type.update(cx, |this, cx| this.pick(Panel::Typography, cx))
                    }),
            )
            .child(
                Button::new("entry-night")
                    .ghost()
                    .small()
                    .flex_1()
                    .justify_center()
                    .icon(if dark {
                        IconName::SunMedium
                    } else {
                        IconName::Moon
                    })
                    .label(night_label.to_string())
                    .on_click(move |_, _, cx| {
                        page_night.update(cx, |this, cx| this.toggle_night(cx))
                    }),
            )
            .child(
                Button::new("entry-settings")
                    .ghost()
                    .small()
                    .flex_1()
                    .justify_center()
                    .icon(IconName::Settings)
                    .label(tr(cx, "account.settings").to_string())
                    .on_click(move |_, _, cx| {
                        page_settings.update(cx, |this, cx| this.open_settings(cx))
                    }),
            )
            .into_any_element()
    }

    fn pick(&mut self, panel: Panel, cx: &mut Context<Self>) {
        self.chrome.select(panel);
        cx.notify();
    }

    /// 浅深一键。
    ///
    /// 判据是**解析出来的**那一档（`cx.theme().mode`），不是配置里写的 `system`：
    /// 跟着系统走的读者点这一下要落到系统此刻的反面，而不是在 `system` 上原地打转。
    fn toggle_night(&mut self, cx: &mut Context<Self>) {
        let dark = cx.theme().mode == ThemeMode::Dark;
        ConfigHolder::edit_appearance(cx, |appearance| {
            appearance.mode = if dark { Mode::Light } else { Mode::Dark }
        });
    }

    /// 「设置」= 往同一栈里再压一屏「我的」。
    ///
    /// 手机上没有独立设置窗口（那是桌面为了一整页字段开的小窗），而「我的」这一格里
    /// 站着主题、书库位置与关于。压进栈而不是切目的地：系统返回 pop 它就回到刚才那
    /// 一行字，阅读位置一点没动。
    fn open_settings(&mut self, cx: &mut Context<Self>) {
        let account = cx.new(AccountView::boot);
        let _ = self
            .stack
            .update(cx, |state, cx| state.push(account, NavMotion::Animated, cx));
    }

    fn advance(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.reader
            .update(cx, |reader, cx| advance_chapter(reader, delta, cx));
    }

    /// 目录面板：就地展开的一块章节表。
    ///
    /// 高度取「窗口剩下的」和 `PANEL_HEIGHT` 里小的那个，并且总给正文留一线天：
    /// 读者跳完要知道跳到了哪儿，全屏列表反而没有参照。
    fn toc_panel(&self, cx: &mut Context<Self>, window: &mut Window) -> AnyElement {
        let height = f32::from(window.viewport_size().height);
        // 顶栏 + 底栏自己（状态行、进度条、入口条）占掉的，加正文留的 120，是列表的下限。
        let chrome_bands = chrome::TOP_BAR_HEIGHT + chrome::BOTTOM_BAR_HEIGHT + 120.0;
        let list_height = (height - chrome_bands).min(chrome::PANEL_HEIGHT).max(160.0);
        let width = f32::from(window.viewport_size().width);
        div()
            .w_full()
            .h(px(list_height))
            .min_h_0()
            .child(self.toc_list(cx, width))
            .into_any_element()
    }

    /// 章节表的虚拟列表：只画视口内那十几行。
    ///
    /// 逐项尺寸表按章数缓存。每帧为 1225 章现造一张表，代价比列表本身还高，
    /// 而这一帧恰好什么都没变的时候（读正文）也要造。
    fn toc_list(&self, cx: &mut Context<Self>, width: f32) -> AnyElement {
        let count = self.reader.read(cx).chapter_count();
        let sizes = if count == self.toc_len {
            self.toc_sizes.clone()
        } else {
            Rc::new(vec![size(px(width), px(TOC_ROW_HEIGHT)); count])
        };
        let view = cx.entity().clone();
        v_virtual_list(view, "reader-toc", sizes, |page, range, _window, cx| {
            page.toc_rows(range, cx)
        })
        .into_any_element()
    }

    /// `[range.start, range.end)` 这几行章节。当前章那一行有自己的表面
    /// （`muted` 底 + 更重的字），不画左侧标记条 —— 那条会打破这一行的圆角轮廓。
    fn toc_rows(
        &mut self,
        range: std::ops::Range<usize>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let current = self.reader.read(cx).chapter_index();
        let selected = cx.theme().muted;
        let foreground = cx.theme().foreground;
        let page = cx.entity().clone();
        (range.start..range.end)
            .map(|ix| {
                let title = self.reader.read(cx).chapter_title_at(ix);
                let active = ix == current;
                let on_click = {
                    let page = page.clone();
                    move |_, _, cx: &mut App| {
                        page.update(cx, |this, cx| this.jump_to_chapter(ix, cx))
                    }
                };
                div()
                    .id(ElementId::named_usize("reader-toc-row", ix))
                    .w_full()
                    .h(px(TOC_ROW_HEIGHT))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded_md()
                    .text_color(foreground)
                    .overflow_hidden()
                    .when(active, |row| row.bg(selected))
                    .child(
                        div()
                            .w_full()
                            .whitespace_nowrap()
                            .text_sm()
                            .when(active, |label| label.font_weight(FontWeight::SEMIBOLD))
                            .child(title),
                    )
                    .on_click(on_click)
                    .into_any_element()
            })
            .collect()
    }

    fn jump_to_chapter(&mut self, index: usize, cx: &mut Context<Self>) {
        self.reader
            .update(cx, |reader, cx| reader.goto_chapter(index, cx));
        // 跳完就把浮层整个收掉：读者要看的正是跳到的那几行字，而底栏正压着下半屏。
        self.chrome = Chrome::default();
        cx.notify();
    }

    /// 排版面板：字号与行距两条滑条。标签在左、当前值在右、滑条在下面 ——
    /// 手机上横着排三样会让滑条只剩一半宽，调一档要划好几下。
    fn typography_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let prefs = ConfigHolder::reader(cx);
        v_flex()
            .w_full()
            .gap_3()
            .child(self.slider_row(
                tr(cx, "field.font_size").to_string(),
                format!("{:.1}", prefs.font_size),
                &self.font_size,
                cx,
            ))
            .child(self.slider_row(
                tr(cx, "field.line_height").to_string(),
                format!("{:.2}", prefs.line_height),
                &self.line_height,
                cx,
            ))
            .into_any_element()
    }

    fn slider_row(
        &self,
        label: String,
        value: String,
        state: &Entity<SliderState>,
        cx: &App,
    ) -> AnyElement {
        v_flex()
            .w_full()
            .gap_1()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(label),
                    )
                    .child(div().text_sm().child(value)),
            )
            .child(Slider::new(state).w_full())
            .into_any_element()
    }
}

impl Render for ReadingPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 正文让出干净态那两条带。浮层不参与：它压字，不推字。
        self.reader.update(cx, |reader, cx| {
            reader.set_chrome_bands(chrome::HEADER_HEIGHT, chrome::FOOTER_HEIGHT, cx)
        });
        self.sync_sliders(window, cx);
        let tap = cx.listener(
            |this: &mut Self, event: &ClickEvent, window: &mut Window, cx: &mut Context<Self>| {
                this.on_body_tap(event, window, cx)
            },
        );
        let mut root = div()
            .id("reader-page")
            .size_full()
            .relative()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                // 正文的点击落在这层而不是 `ReaderView` 自己身上：浮层是它的**兄弟**，
                // 点击只沿祖先链冒泡，所以点在顶栏/底栏上不会被当成「点了一下正文」。
                div()
                    .id("reader-body")
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .on_click(tap)
                    .child(self.reader.clone()),
            );
        // 干净态那两行与浮层是同一片地方的两种样子，永远只画一套。
        root = if self.chrome.is_open() {
            root.child(self.top_bar(cx))
                .child(self.bottom_bar(cx, window))
        } else {
            root.child(self.header(cx)).child(self.footer(cx))
        };
        root
    }
}
