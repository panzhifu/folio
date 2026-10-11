//! 阅读页：正文 + 干净态的两行小字 + 点正文中间浮出的顶栏与底栏。
//!
//! 形态照 Reeden 的阅读界面量出来（1080×2400 @3.0）：干净态只剩左上角一行章名与底部
//! 一行进度；点正文中间才浮出顶栏（返回 + 书名）与底栏（进度条 + 一排入口）。
//! 入口就地换面板，**不弹浮层** —— 浮层在 Android 上现在点不住（同 `shelf` 里
//! 「书要拷到哪儿」那块面板记的那条事实），而目录点不动等于没有目录。
//!
//! 这一屏两端**共用一份**：桌面上原来的工具条 + 状态栏那套常驻 chrome 换成了这个形态，
//! 因为「以手机那一版为准」不只是挑一套皮肤 —— 两套 chrome 意味着每个新功能要落两次地，
//! 而其中一端一定会漏。桌面多出来的那一格是「双页」，它按宽度自己退档（见
//! [`chrome::PageMode::effective`]），所以这一格在窄窗口里就是分页，不需要 `cfg`。
//!
//! 顶栏与底栏**压在正文之上**，不推动正文重排：读者点一下屏幕，字不该跳走。
//! 正文只让出干净态那两条带的高度，那两个数住在 [`chrome`] 里，布局与视口算法共用它。
//!
//! 点击分区、面板档位、模式退档都在 [`chrome`] 里判：那里是纯函数，宿主的 `cargo test`
//! 直接跑，这一屏只把结果落成动作。

use std::rc::Rc;

use folio_core::config::Mode;
use gpui_kit::assets::IconName;
use gpui_kit::base::{NavMotion, NavStackState, v_virtual_list};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable, StyledExt as _, ThemeMode, ThemeStyled as _,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Corners, ElementId, Entity, FontWeight,
    InteractiveElement, IntoElement, ParentElement, Pixels, Render, SharedString, Size,
    StatefulInteractiveElement, Styled, Subscription, WeakEntity, Window, div, px, size,
};

use super::bands;
use super::chrome::{
    self, Chrome, Effect, FONT_SIZE_RANGE, LINE_HEIGHT_RANGE, PageMode, Panel, Reserve, zone,
};
use super::session::ReaderSession;
use crate::ui::features::settings::AccountView;
use crate::ui::shared::config_model::ConfigHolder;
use crate::ui::shared::l10n::tr;

/// 目录一行的行高（逻辑像素）。拇指能稳定命中的下限，也是虚拟列表的逐项尺寸。
const TOC_ROW_HEIGHT: f32 = 48.0;

pub struct ReadingPage {
    reader: Entity<ReaderSession>,
    /// 外壳报告不属于正文的那条边占掉多少（桌面宽、移动高）。
    reserve: Reserve,
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
        let reader = cx.new(|cx| ReaderSession::boot(cx));
        // 字号与行距两条滑条的初值先取出来：`cx.new` 要 `&mut cx`，而把 `cx` 再关进
        // 构造闭包里就是同一个 `Context` 的二次可变借用。
        let prefs = ConfigHolder::reader(cx);
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
                .default_value(prefs.font_size)
        });
        let line_height = cx.new(|_| {
            SliderState::new()
                .min(LINE_HEIGHT_RANGE.0)
                .max(LINE_HEIGHT_RANGE.1)
                .step(0.05)
                .default_value(prefs.line_height)
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
            reserve: Reserve::ZERO,
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
        self.reader
            .update(cx, |reader, _| reader.persist_position());
    }

    pub fn book_id(&self, cx: &App) -> Option<i64> {
        self.reader.read(cx).book_id()
    }

    pub fn set_reserve(&mut self, reserve: Reserve, cx: &mut Context<Self>) {
        self.reserve = reserve;
        self.reader
            .update(cx, |reader, cx| reader.set_reserve(reserve, cx));
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

    /// 正文上的一击：先让选中说话（拖选松手的豁免、选中的清除/菜单回开），
    /// 再按分区照结果动。见 [`chrome::Chrome::handle_body_tap`] 与
    /// `modules/selection.rs` 的 `tap_with_selection`。
    fn on_body_tap(&mut self, event: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let position = event.position();
        // 三条判据的归因与顺序都在 `ReaderSession::tap_on_body` 里（那里才测得到）。
        let consumed = self
            .reader
            .update(cx, |reader, cx| reader.tap_on_body(window, cx, position));
        if consumed {
            cx.notify();
            return;
        }
        let width = f32::from(window.viewport_size().width);
        let tap = zone(f32::from(position.x), width);
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

    /// 把「别处也改得动」的数推回滑条：正文滚了要挪进度滑块，设置页改了字号
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

    /// 顶栏：返回 + 书名，右边是这一章的章名。
    ///
    /// 三处与底栏对齐：同一套左右内缩（那颗 `pl_1` 会让箭头比底栏的入口缩进一截）、
    /// 同一份 `popover_style` 表面、同一档文字层级。右边不摆页码 —— 底栏那条状态行
    /// 已经念过一遍，而「读到哪一章了」是两栏都想一眼看到的。
    fn top_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let title = self.reader.read(cx).title();
        let chapter: SharedString = self.reader.read(cx).chapter_title().into();
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
            .px_4()
            .gap_2()
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
            .child(div().flex_1().min_w_0().truncate().text_sm().child(title))
            .child(
                // 章名给一个上限而不是 `flex_1`：长章名会把书名挤成三个字，
                // 而读者认路的顺序是先「哪本书」再「哪一章」。
                div()
                    .min_w_0()
                    .max_w(px(320.))
                    .truncate()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .when(!chapter.is_empty(), |label| label.child(chapter)),
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

    /// 状态行：章内位置与进度各占一端的边。分页 / 双页时左边那半念「第几页 / 共几页」，
    /// 连续滚动念「第几节 / 共几节」—— 读者认的是自己那一档的刻度。
    fn status_row(&self, cx: &mut Context<Self>) -> AnyElement {
        // 与干净态那条页脚共用 `bands::readouts`：两处念的必须是同一份数。
        let bands::Readouts {
            position, percent, ..
        } = bands::readouts(&self.reader, cx);
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
        let can_prev = reader.can_advance(-1);
        let can_next = reader.can_advance(1);
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
    /// 的动作，双页是当场换摆法，设置是另一屏 —— 四件事不同，所以只有带档位的那三格
    /// 吃 `selected`。
    fn entry_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let panel = self.chrome.panel();
        let dark = cx.theme().mode == ThemeMode::Dark;
        let spread = self.reader.read(cx).wanted_mode() != PageMode::Continuous;
        let page_toc = cx.entity().clone();
        let page_type = cx.entity().clone();
        let page_night = cx.entity().clone();
        let page_spread = cx.entity().clone();
        let page_settings = cx.entity().clone();
        let night_label = tr(cx, if dark { "reader.day" } else { "reader.night" });
        let spread_label = tr(cx, "reader.mode_spread");
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
                // 这一格只给图标：一排五个入口再各带两个字，手机上每格会被挤到点不准。
                // 双页按宽度退档，所以这格在窄屏上也仍然按得下去（按下去就是分页）。
                Button::new("entry-spread")
                    .ghost()
                    .small()
                    .flex_1()
                    .justify_center()
                    .selected(spread)
                    .icon(IconName::Columns3)
                    .tooltip(spread_label.clone().to_string())
                    .accessibility_label(spread_label)
                    .on_click(move |_, _, cx| {
                        page_spread.update(cx, |this, cx| this.toggle_spread(cx))
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

    /// 连续 ⇄ 双页。换档不动位置：进度是「第几个字」，两档之间搬的是同一句话。
    fn toggle_spread(&mut self, cx: &mut Context<Self>) {
        let wanted = self.reader.read(cx).wanted_mode();
        let next = wanted.next();
        self.reader
            .update(cx, |reader, cx| reader.set_mode(next, cx));
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
    /// 一行字，阅读位置一点没动。桌面同样走这条 —— 少一个只在一端存在的窗口，
    /// 就少一处「这功能那端没有」。
    fn open_settings(&mut self, cx: &mut Context<Self>) {
        let account = cx.new(AccountView::boot);
        let _ = self
            .stack
            .update(cx, |state, cx| state.push(account, NavMotion::Animated, cx));
    }

    fn advance(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.reader
            .update(cx, |reader, cx| reader.advance_chapter(delta, cx));
    }

    /// 目录面板：就地展开的一块章节表。
    ///
    /// 高度取「窗口剩下的」和 `PANEL_HEIGHT` 里小的那个，并且总给正文留一线天：
    /// 读者跳完要知道跳到了哪儿，全屏列表反而没有参照。
    fn toc_panel(&self, cx: &mut Context<Self>, window: &mut Window) -> AnyElement {
        let height = f32::from(window.viewport_size().height);
        // 顶栏 + 底栏自己（状态行、进度条、入口条）占掉的，加正文留的 120，是列表的下限。
        let chrome_bands = chrome::TOP_BAR_HEIGHT + chrome::BOTTOM_BAR_HEIGHT + 120.0;
        let list_height = (height - chrome_bands).clamp(160.0, chrome::PANEL_HEIGHT);
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
                    // 三个参数都要写上类型：只标注最后一个，闭包就对不生命周期通用化，
                    // 报的是「implementation of `Fn` is not general enough」。
                    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
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
        // 浮层开合同时报给会话：开着时指针拖选让位（那一击的语义是收浮层）。
        let chrome_open = self.chrome.is_open();
        self.reader.update(cx, |reader, cx| {
            reader.set_chrome_bands(chrome::HEADER_HEIGHT, chrome::FOOTER_HEIGHT, cx);
            reader.set_overlay_open(chrome_open, cx);
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
                // 正文的点击落在这层而不是 `ReaderSession` 自己身上：浮层是它的**兄弟**，
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
            root.child(bands::header(&self.reader, cx))
                .child(bands::footer(&self.reader, cx))
        };
        root
    }
}
