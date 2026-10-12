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

use folio_core::infra::config::Mode;
use gpui_kit::assets::IconName;
use gpui_kit::base::{NavMotion, NavStackState, v_virtual_list};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, Selectable, Sizable, StyledExt as _, ThemeMode,
    ThemeStyled as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Corners, ElementId, Entity, FontWeight,
    InteractiveElement, IntoElement, ParentElement, Pixels, Render, SharedString, Size,
    StatefulInteractiveElement, Styled, Subscription, WeakEntity, Window, div, hsla, px, size,
};

use super::bands;
use super::chrome::{
    self, Chrome, Effect, FONT_SIZE_RANGE, LINE_HEIGHT_RANGE, PageMode, Panel, Reserve, zone,
};
use super::session::ReaderSession;
use super::toc_tree::{self, TocView};
use crate::ui::features::settings::CompactSettings;
use crate::ui::shared::config_model::ConfigHolder;
use crate::ui::shared::l10n::{tf, tr};

/// 目录缩进的级差（逻辑像素）。27 是在真机上量的那一档：三级以内还能看出「这条挂在那条
/// 下面」，再宽就把小屏的正文挤成一条竖缝。
const TOC_INDENT: f32 = 27.0;
/// chevron 那一列的宽度。叶子也占同样一格，标题才不会因为有没有下级而左右跳。
const TOC_CHEVRON: f32 = 20.0;
/// 目录行的左右内缩。
const TOC_GUTTER: f32 = 16.0;
/// 当前行左边那条标记条的宽度。
const TOC_MARKER: f32 = 3.0;

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
    /// 目录那一层开着时的状态（整棵树 + 折叠集 + 搜索词）。`None` = 还没按当前位置
    /// 建过 —— 每次重新打开都重建一次，于是「树跟着阅读位置走」这条规矩不会被上一次
    /// 手动折的格带过来。
    toc: Option<TocView>,
    /// 目录里的搜索框开没开。框自己的状态由 `window.use_keyed_state` 按这一屏的键持有
    /// （与设置里那些输入框同一条路），所以这里只存一个开关。
    toc_search_open: bool,
    /// 目录虚拟列表的逐项尺寸表。只在「行数或行高变了」时重建，不每帧分配一张千项的表。
    toc_sizes: Rc<Vec<Size<Pixels>>>,
    toc_len: usize,
    toc_row_height: f32,
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
            toc: None,
            toc_search_open: false,
            toc_sizes: Rc::new(Vec::new()),
            toc_len: 0,
            toc_row_height: 0.0,
            _watches: watches,
        }
    }

    pub fn open(&mut self, book_id: i64, cx: &mut Context<Self>) {
        self.reader
            .update(cx, |reader, cx| reader.open_book(book_id, cx));
    }

    /// 落库。外壳在 pop 之前调它。
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.reader.update(cx, |reader, _cx| {
            reader.persist_position();
            // 位置与时长是同一次离席要结的两笔账：漏掉后一笔，这一段就只能靠
            // 那条时钟在五分钟之内结掉，而读者往往在五分钟之内就再没回来过。
            reader.end_reading();
        });
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
    fn bottom_bar(&self, cx: &mut Context<Self>) -> AnyElement {
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
            // 目录不在这块卡片里：它盖住的是整块正文，见 [`Self::toc_layer`]。
            // 排版面板留在这里，因为它只有两条滑条，抬着正文看一眼再放回去正合适。
            Panel::Toc => card,
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
        if panel == Panel::Toc && self.chrome.panel() != Panel::Toc {
            // 每次重新打开都按当前位置重算折叠集：读者上次手动折的那些格不该跨次生效，
            // 他这一次要回答的问题还是「我现在在哪」。搜索框一并收掉。
            self.toc = None;
            self.toc_search_open = false;
        }
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

    /// 「设置」= 往同一栈里再压一屏钻入式设置。
    ///
    /// 手机上没有独立设置窗口（两栏的整页字段塞不进这块小屏），压进栈的这屏吃的是
    /// 与桌面同一份页面规格。压进栈而不是切目的地：系统返回 pop 它就回到刚才那
    /// 一行字，阅读位置一点没动。桌面同样走这条 —— 少一个只在一端存在的窗口，
    /// 就少一处「这功能那端没有」。
    fn open_settings(&mut self, cx: &mut Context<Self>) {
        let settings = cx.new(|_| CompactSettings::new());
        let _ = self
            .stack
            .update(cx, |state, cx| state.push(settings, NavMotion::Animated, cx));
    }

    fn advance(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.reader
            .update(cx, |reader, cx| reader.advance_chapter(delta, cx));
    }

    /// 按当前位置建一次目录树。已经建过就不动 —— 重建会把读者手动折的格冲掉；
    /// 「重新打开要跟着位置走」那条重置在 [`Self::pick`] 里显式做。
    fn ensure_toc(&mut self, cx: &mut Context<Self>) {
        if self.toc.is_some() {
            return;
        }
        let rows = self.reader.read(cx).toc_rows();
        let current = self.reader.read(cx).chapter_index();
        let current_ix = toc_tree::row_of_chapter(&rows, current);
        self.toc = Some(TocView::open(rows, current_ix));
    }

    /// 目录那一层：顶栏下面到底栏上面，整块盖住正文。
    ///
    /// 不弹浮层，而是画进同一棵渲染树：这一层要盖的是**正文**，而正文与顶栏底栏是兄弟，
    /// 谁在上面只由摆的顺序决定 —— 与其去调那个，不如让「干净态两行小字 / 顶栏加底栏 /
    /// 顶栏加目录加底栏」成为同一块地方的三种样子（见 [`Self::render`] 那一条分支）。
    fn toc_layer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.ensure_toc(cx);
        // 搜索框的状态按这一屏的键持有，订阅句柄跟着它一起活：输入一改变就重画这一屏。
        let search = window.use_keyed_state("reader-toc-search", cx, |window, cx| {
            let input = cx.new(|cx| InputState::new(window, cx));
            let watch = cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify());
            (input, watch)
        });
        // 只克隆那个句柄：对 `search.read(cx)` 整体调 `.clone()` 克隆的是**引用**
        // （`&T: Clone` 是平凡实现），拿回来的还是借用的元组，下面要 `&mut cx` 的
        // 每一次调用都会撞在它上面。
        let input = search.read(cx).0.clone();
        if let Some(toc) = &mut self.toc {
            toc.set_query(input.read(cx).value().to_string());
        }

        let row_height = ConfigHolder::reader(cx).toc_density.row_height();
        let count = self.toc.as_ref().map_or(0, |toc| toc.visible().len());
        // 行数（折展、搜索会改）或行高（设置里换了密度）变了才重造那张尺寸表。
        if count != self.toc_len || (row_height - self.toc_row_height).abs() > 0.01 {
            let width = f32::from(window.viewport_size().width);
            self.toc_sizes = Rc::new(vec![size(px(width), px(row_height)); count]);
            self.toc_len = count;
            self.toc_row_height = row_height;
        }

        // 三个子元素先各自算完：`cx.theme()` 还回去的 `&Theme` 借的是 `*cx`，把它留在
        // 一条链里会活过整条 `let`，链上那几个要 `&mut cx` 的调用就过不去。
        let header = self.toc_header(cx);
        let mut search = None;
        if self.toc_search_open {
            search = Some(self.toc_search_row(&input, cx));
        }
        let list = self.toc_list(cx);
        // 高度必须显式给：`absolute` 同时钉 `top` 与 `bottom` 在这一层量不出高，
        // 结果是这一层渲染成零高 —— 状态确实切过去了，屏幕上却什么都没有，
        // 看起来就像那颗入口按了没反应。
        let height = (f32::from(window.viewport_size().height)
            - chrome::TOP_BAR_HEIGHT
            - chrome::BOTTOM_BAR_HEIGHT)
            .max(0.0);
        let mut layer = v_flex()
            .id("reader-toc-layer")
            .absolute()
            .left_0()
            .right_0()
            .top(px(chrome::TOP_BAR_HEIGHT))
            .h(px(height))
            .bg(cx.theme().background)
            .child(header);
        if let Some(search) = search {
            layer = layer.child(search);
        }
        layer.child(list).into_any_element()
    }

    /// 头部那一行：「已读 N / M 章」+ 搜索 + 目录样式。
    ///
    /// 那颗「目录样式」直接压进设置那一屏（与底栏的「设置」同一动作）：从目录里想改
    /// 目录的样子，是这一屏最常发生的一次跳转，不该让读者先把它收起来。
    fn toc_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.reader.read(cx).chapter_index();
        let (read, total) = self
            .toc
            .as_ref()
            .map(|toc| toc.read_count(toc.current_ix(current)))
            .unwrap_or((0, 0));
        let label = tf(cx, "reader.toc.read", &[("read", &read), ("total", &total)]);
        let search_page = cx.entity().clone();
        let style_page = cx.entity().clone();
        let search_label = tr(cx, "reader.toc.search");
        let style_label = tr(cx, "reader.toc.style");
        h_flex()
            .w_full()
            .items_center()
            .gap_1()
            .px(px(TOC_GUTTER))
            .h(px(44.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(
                Button::new("toc-search")
                    .ghost()
                    .small()
                    .selected(self.toc_search_open)
                    .icon(IconName::Search)
                    .tooltip(search_label.clone().to_string())
                    .accessibility_label(search_label)
                    .on_click(move |_, _, cx| {
                        search_page.update(cx, |this, cx| {
                            this.toc_search_open = !this.toc_search_open;
                            cx.notify();
                        })
                    }),
            )
            .child(
                Button::new("toc-style")
                    .ghost()
                    .small()
                    .icon(IconName::SlidersHorizontal)
                    .tooltip(style_label.clone().to_string())
                    .accessibility_label(style_label)
                    .on_click(move |_, _, cx| {
                        style_page.update(cx, |this, cx| this.open_settings(cx))
                    }),
            )
            .into_any_element()
    }

    fn toc_search_row(&self, input: &Entity<InputState>, cx: &mut Context<Self>) -> AnyElement {
        div()
            .w_full()
            .px(px(TOC_GUTTER))
            .pb_2()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .child(
                        Icon::new(IconName::Search)
                            .small()
                            .text_color(cx.theme().muted_foreground),
                    )
                    // 没有占位文案可给：`InputState` 与 `Input` 都不收 placeholder
                    // （gpui-kit 0.7.1 只有数字输入框有那一项）。这一行只在点了搜索
                    // 之后出现，旁边那颗放大镜就是它自己的说明。
                    .child(Input::new(input).flex_1().min_w_0()),
            )
            .into_any_element()
    }

    /// 列表本体。没有目录的书（TXT / MD / FB2 与定版式）在这里给一句实话，
    /// 而不是摆一屏空白让读者以为是排版没做完。
    fn toc_list(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.toc_len == 0 {
            return div()
                .w_full()
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(tr(cx, "reader.toc.empty"))
                .into_any_element();
        }
        let view = cx.entity().clone();
        v_virtual_list(view, "reader-toc", self.toc_sizes.clone(), |page, range, _, cx| {
            page.toc_rows(range, cx)
        })
        .into_any_element()
    }

    /// `[range.start, range.end)` 这几行目录。
    ///
    /// 三个信号各管各的，不互相顶：当前行 = 底色 + 左边那条标记；来过 = 文字变灰
    /// （设置里可以关掉）；对不上正文的条目 = 灰到底且**没有**点击回调。
    fn toc_rows(&mut self, range: std::ops::Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(toc) = &self.toc else {
            return Vec::new();
        };
        let rows = toc.visible();
        let end = range.end.min(rows.len());
        if range.start >= end {
            return Vec::new();
        }
        let prefs = ConfigHolder::reader(cx);
        let row_height = prefs.toc_density.row_height();
        let lines = prefs.toc_max_title_lines as usize;
        let current = self.reader.read(cx).chapter_index();
        let read = self.reader.read(cx).read_chapters().clone();
        let (foreground, muted, surface) = (
            cx.theme().foreground,
            cx.theme().muted_foreground,
            cx.theme().muted,
        );
        let transparent = hsla(0.0, 0.0, 0.0, 0.0);
        let page = cx.entity().clone();
        rows[range.start..end]
            .iter()
            .map(|row| {
                let ix = row.ix;
                let active = row.chapter == Some(current);
                let visited =
                    prefs.toc_dim_read && row.chapter.is_some_and(|chapter| read.contains(&chapter));
                // 当前行不跟着「来过」变灰：它已经吃了底色与标记条，再灰一层就变成
                // 「看不出现在在哪」，而那正是这一屏唯一必须答对的问题。
                let color = if active || !visited { foreground } else { muted };
                let chevron = if row.branch {
                    let page_toggle = page.clone();
                    Button::new(ElementId::named_usize("reader-toc-toggle", ix))
                        .ghost()
                        .small()
                        .w(px(TOC_CHEVRON))
                        .p_0()
                        .justify_center()
                        .icon(if toc.is_collapsed(ix) {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        })
                        .on_click(move |_, _, cx| {
                            // 不拦的话这一击会接着冒到整行上：读者点的是「展开」，
                            // 结果书也跳了、面板也收了，看起来像这颗箭头是坏的。
                            cx.stop_propagation();
                            page_toggle.update(cx, |this, cx| {
                                if let Some(toc) = &mut this.toc {
                                    toc.toggle(ix);
                                }
                                cx.notify();
                            })
                        })
                        .into_any_element()
                } else {
                    // 叶子占同样一格，标题才不会因为有没有下级而左右跳。
                    div().w(px(TOC_CHEVRON)).flex_none().into_any_element()
                };
                let mut row_el = div()
                    .id(ElementId::named_usize("reader-toc-row", ix))
                    .w_full()
                    .h(px(row_height))
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .pl(px(TOC_GUTTER + TOC_MARKER + (row.depth as f32) * TOC_INDENT))
                    .pr(px(TOC_GUTTER))
                    .border_l(px(TOC_MARKER))
                    .border_color(if active { foreground } else { transparent })
                    .when(active, |el| el.bg(surface))
                    .child(chevron)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .line_clamp(lines)
                            .when(active, |el| el.font_weight(FontWeight::SEMIBOLD))
                            .text_color(color)
                            .child(row.title.clone()),
                    );
                // 对不上正文的那几条不挂回调：灰着且点不动，比「点了什么也没发生」诚实。
                if let Some(chapter) = row.chapter {
                    let fragment = row.fragment.clone();
                    let page_jump = page.clone();
                    row_el = row_el.on_click(move |_, _, cx| {
                        page_jump.update(cx, |this, cx| {
                            this.jump_to_entry(chapter, fragment.clone(), cx)
                        })
                    });
                }
                row_el.into_any_element()
            })
            .collect()
    }

    /// 跳到目录里的某一条，并把这一层整个收掉：读者要看的正是跳到的那几行字。
    fn jump_to_entry(&mut self, chapter: usize, fragment: Option<String>, cx: &mut Context<Self>) {
        self.reader
            .update(cx, |reader, cx| reader.goto_entry(chapter, fragment, cx));
        self.chrome = Chrome::default();
        self.toc = None;
        self.toc_search_open = false;
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
            let root = root
                .child(self.top_bar(cx))
                .child(self.bottom_bar(cx));
            // 目录是第三档：顶栏底栏都留着，中间那块正文整块换成树。
            if self.chrome.panel() == Panel::Toc {
                root.child(self.toc_layer(window, cx))
            } else {
                root
            }
        } else {
            root.child(bands::header(&self.reader, cx))
                .child(bands::footer(&self.reader, cx))
        };
        root
    }
}
