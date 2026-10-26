//! 阅读设置那一屏：一块从屏幕底升起来的卡片，装着读者在一本书里会当场调的那几样。
//!
//! 形状照目录那一屏（[`super::toc_panel`]）：同一份状态、同一个底部弹层、同一套退场
//! （拇指往下划、点遮罩、按返回三条路都收这一层）。从前这些控件长在底栏卡片**里面**，
//! 底栏因此有两张脸 —— 收起时三行、展开时七行，而进度条与入口排在最底下，面板一展开
//! 就被顶到读者刚调完的那几格下面。搬进弹层之后底栏只剩那一块常驻的三行，设置想装多少
//! 都行，装不下就自己滚。
//!
//! ## 为什么留一档半屏
//!
//! 卡片调的是**字的样子**：行距、段距、留白改完，读者要看的正是身后那几行字重排成什么样。
//! 所以两档都留着 —— 最高那档也只占窗口高的六成出头，往下拖到半屏那一档就能一边调一边
//! 看着字走。这条是这块弹层与目录那块唯一的差别：目录不需要看见正文，需要。
//!
//! ## 三组，按读者此刻要调的东西分开
//!
//! 卡片顶上是「外观 / 字体 / 排版」三段，照读者实际改字的顺序分：浅深、亮度与翻页
//! 那两张纸是环境；字体家族、字重与字号是字的身份；行距、字距、段距、缩进、对齐、版心、
//! 上下左右留白与文字方向是版面。三组各自装在一页里，切换即时完成（内容早已在手上，
//! 不等加载），换页不会把弹层收起。
//!
//! ## 每格只摆「标题 + 控件」
//!
//! 行里没有灰色说明，也没有分组标题，与手机端设置那一定稿同一口径（见
//! `settings::view::compact` 文件头那条）。这些字段「干什么用」的话仍然有落点 ——
//! 完整设置屏那一格，桌面那侧照样渲染 `desc`。卡片这边靠控件自己说话：滑条推着走，
//! 正文当场重排，不需要一句话预告这件事会发生。
//!
//! ## 量程不在这里抄第二份
//!
//! 每条滑条的两端引用 [`chrome`] 里那几对常数，而它们引用的是配置自己的上下限
//! （`Reader::clamped` 与 `Appearance::normalized` 夹的就是同一对数）。界面里一旦写死
//! 第二个「最大行距」，滑条就能拖到一个存不进配置的位置：屏幕上数变了，落盘那刻被夹回去，
//! 下一帧又缩回来。`chrome::FONT_SIZE_RANGE` 那一档是**故意**比配置窄的（阅读时顺手调只
//! 铺常用的一段），那种例外写在它自己的注释里，并由 `model/chrome.rs` 的量程那条门钉住。

use std::rc::Rc;

use gpui_kit::Anchor;
use gpui_kit::component::button::Button;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme, Sizable, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Window, div,
};

use folio_core::{Mode, PageTurn, Reader, TextDirection};

use crate::ui::components::{BottomSheet, BottomSheetState, FontFamilySetter, FontPicker};
use crate::ui::features::reader::model::chrome::{
    self, BRIGHTNESS_RANGE, FONT_SIZE_RANGE, INDENT_RANGE, LETTER_SPACING_RANGE, LINE_HEIGHT_RANGE,
    MARGIN_X_RANGE, MARGIN_Y_RANGE, MAX_MEASURE_RANGE, PARAGRAPH_GAP_RANGE, TURN_DURATION_RANGE,
};
use crate::ui::features::reader::model::session::ReaderSession;
use crate::ui::services::config::ConfigHolder;
use crate::ui::services::l10n::tr;

/// 卡片顶部那三段。顺序就是读者的动线：先看环境，再挑字，最后调版面。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SheetTab {
    #[default]
    Appearance,
    Font,
    Layout,
}

impl SheetTab {
    const ALL: [Self; 3] = [Self::Appearance, Self::Font, Self::Layout];

    fn index(self) -> usize {
        match self {
            Self::Appearance => 0,
            Self::Font => 1,
            Self::Layout => 2,
        }
    }

    fn from_index(index: usize) -> Self {
        Self::ALL.get(index).copied().unwrap_or_default()
    }

    fn label(self, cx: &App) -> SharedString {
        match self {
            Self::Appearance => tr(cx, "reader.tab_appearance"),
            Self::Font => tr(cx, "reader.tab_font"),
            Self::Layout => tr(cx, "reader.tab_layout"),
        }
    }
}

/// 卡片能停的最高那一档：窗口高的 62%。三组分开之后最长的那组（排版九格）到这一档
/// 也装得下，同时上方还留出近四成正文 —— 调行距、段距时读者要看的就是那几行字。
/// 从前九格挤在一块，最高档只能开到 85%；分组之后把那截高度还给正文。
const SHEET_EXPANDED: f32 = 0.62;
/// 往下拖还能停的那一档：半屏。卡片调的是字的样子，读者要看得见身后那几行重排成什么样，
/// 所以这一档不许省 —— 弹层组件在 `peek` 不低于 `expanded` 时只剩一档（见
/// [`crate::ui::components::bottom_sheet::anchors`]），那一档拖到哪都盖满正文。
const SHEET_PEEK: f32 = 0.5;

/// 一行的值与滑条之间那点步进的差别写在调用处，这里只负责把区间摆对。
///
/// `max` 必须写在 `min` 前面：`SliderState::new()` 的默认区间是 0..**100**，而每一步
/// setter 都会拿当前区间去 `clamp` 已有的值。翻页时长那一条的下限是 120，先写 `min`
/// 就是 `clamp(120.0, 100.0)` —— 当场 panic（实测炸在 `ReadingPage::new` 那一格）。
fn slider(
    range: (f32, f32),
    step: f32,
    value: f32,
    cx: &mut Context<ReadingSheet>,
) -> Entity<SliderState> {
    let value = value.clamp(range.0, range.1);
    cx.new(|_| {
        SliderState::new()
            .max(range.1)
            .min(range.0)
            .step(step)
            .default_value(value)
    })
}

/// 阅读设置那一屏自己的状态。跨帧持有，一页一个（阅读页建它，随阅读页一起活）。
pub(in crate::ui::features::reader) struct ReadingSheet {
    /// 底部弹层：开合与停在哪一档都在它手里。句柄必须跨帧持有 —— 弹层每帧都要构造
    /// （弹簧的采样在里面），与下面那几条滑条同一条理由。
    sheet: Entity<BottomSheetState>,
    /// 这一屏背后的会话。只借它问一件事：这一本是不是定版式 —— 翻页动画那个下拉
    /// 在定版式上要筛掉折页（判据在 `chrome::fixed_turn_applies`；上下滚动在定版式上
    /// 就是连续竖滚，留着）。
    reader: Entity<ReaderSession>,
    /// 顶上停在哪一组。跨帧留着：读者刚在排版里调完行距，收起再打开该还在排版。
    tab: SheetTab,
    brightness: Entity<SliderState>,
    font_size: Entity<SliderState>,
    line_height: Entity<SliderState>,
    letter_spacing: Entity<SliderState>,
    paragraph_gap: Entity<SliderState>,
    indent: Entity<SliderState>,
    max_measure: Entity<SliderState>,
    margin_x: Entity<SliderState>,
    margin_y: Entity<SliderState>,
    turn_duration: Entity<SliderState>,
    _watches: Vec<Subscription>,
}

impl ReadingSheet {
    /// 建这一屏。十条滑条各订阅一次：拖动当场生效（换字号重排 / 换背光 / 下一趟翻页走多久），
    /// 所以这里没有「应用」按钮，也没有改了不生效的中间态。
    pub(in crate::ui::features::reader) fn new(
        reader: Entity<ReaderSession>,
        cx: &mut Context<Self>,
    ) -> Self {
        let prefs = ConfigHolder::reader(cx);
        let appearance = ConfigHolder::appearance(cx);
        // 「跟随系统」在滑条上就是推到顶：配置里那一格是 `None`，滑条上得有个能显示的数。
        let brightness = appearance.brightness.unwrap_or(BRIGHTNESS_RANGE.1);
        let brightness = slider(BRIGHTNESS_RANGE, 0.01, brightness, cx);
        let font_size = slider(FONT_SIZE_RANGE, 0.5, prefs.font_size, cx);
        let line_height = slider(LINE_HEIGHT_RANGE, 0.05, prefs.line_height, cx);
        let letter_spacing = slider(LETTER_SPACING_RANGE, 0.01, prefs.letter_spacing_em, cx);
        let paragraph_gap = slider(PARAGRAPH_GAP_RANGE, 0.05, prefs.paragraph_gap_em, cx);
        let indent = slider(INDENT_RANGE, 0.25, prefs.indent_em, cx);
        let max_measure = slider(MAX_MEASURE_RANGE, 20.0, prefs.max_measure, cx);
        let margin_x = slider(MARGIN_X_RANGE, 8.0, prefs.margin_x, cx);
        let margin_y = slider(MARGIN_Y_RANGE, 8.0, prefs.margin_y, cx);
        // 步长 10 ms：一整条滑条 58 个刻度，手指一次划动能停稳的刻度比这少，再细则是在
        // 骗读者说这格能调准 —— 两三百毫秒的差别肉眼分不出 5 ms。
        let turn_duration = slider(TURN_DURATION_RANGE, 10.0, appearance.turn_duration, cx);
        // 弹层自己也要观察：它不是这一屏的子视图，`notify` 只有观察方才收得到，
        // 而开合、换档、读者一把下拉关掉全靠随后那一次重画。
        let sheet = cx.new(|cx| BottomSheetState::closed(cx));
        let watches = vec![
            cx.observe(&sheet, |_, _, cx| cx.notify()),
            Self::watch(&brightness, cx, |value, cx| {
                // 顶格归成 `None`：配置里于是只有「跟随系统」一个写法，滑条与文件不会是
                // 两套「最大亮度」（同 `Appearance::normalized` 那一条）。
                ConfigHolder::edit_appearance(cx, |appearance| {
                    appearance.brightness = if value >= BRIGHTNESS_RANGE.1 {
                        None
                    } else {
                        Some(value)
                    }
                });
            }),
            Self::watch(&font_size, cx, |value, cx| {
                ConfigHolder::edit_reader(cx, |prefs| prefs.font_size = value);
            }),
            Self::watch(&line_height, cx, |value, cx| {
                ConfigHolder::edit_reader(cx, |prefs| prefs.line_height = value);
            }),
            Self::watch(&letter_spacing, cx, |value, cx| {
                ConfigHolder::edit_reader(cx, |prefs| prefs.letter_spacing_em = value);
            }),
            Self::watch(&paragraph_gap, cx, |value, cx| {
                ConfigHolder::edit_reader(cx, |prefs| prefs.paragraph_gap_em = value);
            }),
            Self::watch(&indent, cx, |value, cx| {
                ConfigHolder::edit_reader(cx, |prefs| prefs.indent_em = value);
            }),
            Self::watch(&max_measure, cx, |value, cx| {
                ConfigHolder::edit_reader(cx, |prefs| prefs.max_measure = value);
            }),
            Self::watch(&margin_x, cx, |value, cx| {
                ConfigHolder::edit_reader(cx, |prefs| prefs.margin_x = value);
            }),
            Self::watch(&margin_y, cx, |value, cx| {
                ConfigHolder::edit_reader(cx, |prefs| prefs.margin_y = value);
            }),
            Self::watch(&turn_duration, cx, |value, cx| {
                ConfigHolder::edit_appearance(cx, |appearance| appearance.turn_duration = value);
            }),
        ];
        Self {
            sheet,
            reader,
            tab: SheetTab::default(),
            brightness,
            font_size,
            line_height,
            letter_spacing,
            paragraph_gap,
            indent,
            max_measure,
            margin_x,
            margin_y,
            turn_duration,
            _watches: watches,
        }
    }

    /// 一条滑条订阅一次：把拖出来的那个数交给写入口。
    ///
    /// 写成关联函数而不是 `self` 方法：这些路唯一的差别是往哪一格写，而订阅句柄要由
    /// 调用方存进字段 —— 方法里 `self._watches.push(...)` 与同时借着 `self.xxx` 那条句柄
    /// 是同一块的两个借用，编译不过。
    fn watch(
        state: &Entity<SliderState>,
        cx: &mut Context<Self>,
        write: impl Fn(f32, &mut App) + 'static,
    ) -> Subscription {
        cx.subscribe(state, move |_, _, event, cx| {
            if let SliderEvent::Change(value) = event {
                write(value.end(), cx);
            }
        })
    }

    /// 卡片开着吗。阅读页拿它决定「身上压着东西」要不要报给会话（那一击的语义是收这一层，
    /// 不是选字），也拿它决定底栏那一格的选中态。
    pub(in crate::ui::features::reader) fn is_open(&self, cx: &App) -> bool {
        self.sheet.read(cx).is_open()
    }

    /// 开 or 关。
    pub(in crate::ui::features::reader) fn toggle(&mut self, cx: &mut Context<Self>) {
        if self.sheet.read(cx).is_open() {
            self.hide(cx);
            return;
        }
        self.sheet.update(cx, |sheet, cx| sheet.show(cx));
        cx.notify();
    }

    /// 收掉这一层（点了遮罩、下拉到底、按了系统返回，或从底栏那颗再按一下）。
    pub(in crate::ui::features::reader) fn hide(&mut self, cx: &mut Context<Self>) {
        self.sheet.update(cx, |sheet, cx| sheet.hide(cx));
        cx.notify();
    }

    /// 把「别处也改得动」的数推回滑条：完整设置屏改了行距要挪这条滑条，而亮度那一格在
    /// 「跟随系统」时没有数 —— 滑条上把它显示成顶格，配置里它是 `None`。
    ///
    /// 带阈值比较：拖动的时候滑条自己就是权威，每帧再按配置读回来的数写回去，
    /// 就是拿手指算出来的值和落盘算出来的值互相追。
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prefs = ConfigHolder::reader(cx);
        let appearance = ConfigHolder::appearance(cx);
        let brightness = appearance.brightness.unwrap_or(BRIGHTNESS_RANGE.1);
        let wanted = [
            (&self.brightness, brightness),
            (&self.font_size, prefs.font_size),
            (&self.line_height, prefs.line_height),
            (&self.letter_spacing, prefs.letter_spacing_em),
            (&self.paragraph_gap, prefs.paragraph_gap_em),
            (&self.indent, prefs.indent_em),
            (&self.max_measure, prefs.max_measure),
            (&self.margin_x, prefs.margin_x),
            (&self.margin_y, prefs.margin_y),
            (&self.turn_duration, appearance.turn_duration),
        ];
        for (state, value) in wanted {
            if (state.read(cx).value().end() - value).abs() > 0.005 {
                state.update(cx, |state, cx| state.set_value(value, window, cx));
            }
        }
    }

    /// 一格滑条：标签在左、当前值在右、滑条在下面。
    ///
    /// 竖着摆而不是横着排三样：手机上横排会让滑条只剩一半宽，调一档要划好几下。
    fn slider_row(
        &self,
        label: SharedString,
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
                    .child(div().text_base().child(label))
                    .child(
                        div()
                            .text_base()
                            .text_color(cx.theme().muted_foreground)
                            .child(value),
                    ),
            )
            .child(Slider::new(state).w_full())
            .into_any_element()
    }

    /// 「阅读浅深」那一格：跟随软件 / 跟随系统 / 浅色 / 深色。
    ///
    /// 改的是**阅读页自己**那一格（`Appearance::reading_mode`），不是软件外观：关书之后
    /// 架面与设置还是读者在完整设置里选的那一档。它住在「外观」这组是因为换浅深当场生效，
    /// 与排版无关（不做排版失效键，见 `folio_core::infra::config::Appearance`）。
    fn reading_mode_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let mode = ConfigHolder::appearance(cx).reading_mode;
        let label_of = |mode: Option<Mode>| match mode {
            None => tr(cx, "appearance.follow_app"),
            Some(Mode::System) => tr(cx, "appearance.follow_system"),
            Some(Mode::Light) => tr(cx, "appearance.light"),
            Some(Mode::Dark) => tr(cx, "appearance.dark"),
        };
        let items: Vec<(Option<Mode>, SharedString)> = [
            None,
            Some(Mode::System),
            Some(Mode::Light),
            Some(Mode::Dark),
        ]
        .into_iter()
        .map(|value| (value, label_of(value)))
        .collect();
        let current = label_of(mode);
        let title = tr(cx, "field.reading_mode");
        let sheet = cx.entity().clone();
        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .child(div().flex_1().min_w_0().text_base().child(title.clone()))
            .child(
                Button::new("reader-reading-mode")
                    .outline()
                    .small()
                    .label(current.to_string())
                    .dropdown_caret(true)
                    .accessibility_label(title)
                    .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                        items.iter().fold(menu, |menu, (value, label)| {
                            let checked = *value == mode;
                            let value = *value;
                            // 菜单闭包是 `Fn`，每一项的回调要各自一份句柄（`Entity` 不 `Copy`）。
                            let sheet = sheet.clone();
                            menu.item(PopupMenuItem::new(label.clone()).checked(checked).on_click(
                                move |_, _, cx| {
                                    sheet.update(cx, |this, cx| this.set_reading_mode(value, cx))
                                },
                            ))
                        })
                    }),
            )
            .into_any_element()
    }

    fn set_reading_mode(&mut self, mode: Option<Mode>, cx: &mut Context<Self>) {
        ConfigHolder::edit_appearance(cx, |appearance| appearance.reading_mode = mode);
        cx.notify();
    }

    /// 「翻页动画」那一格：一颗往上开的下拉，与它正下面那条时长滑的是同一张纸。
    ///
    /// 下拉**往上开**（`Anchor::BottomLeft`）：这一行长在贴着屏幕底的卡片里，往下开的
    /// 话那两块菜单有一半掉到屏幕外，读者只点得到上面那一块。
    ///
    /// 六档**全摆**，不按当前摆法筛：这一条列表就是摆法的上游 —— 选「上下滚动」会把这本
    /// 书带进连续，选任何一张纸的走法会把它带出连续（`chrome::reconcile` 在渲染帧里解）。
    /// 从前这里筛掉四档，于是同一设置在两处给出两个可选集，读者在两处都能选到一个当场
    /// 不出场的档。
    fn turn_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let configured = ConfigHolder::appearance(cx).page_turn;
        let fixed = self.reader.read(cx).is_fixed();
        // 定版式：折页没有可卷的排版纹理，那一档不摆；配置里若正是它（从流式那一本
        // 带过来的），按钮念的是实际会走的平移。上下滚动留着 —— 定版式现在也有
        // 一条纵向页栈可滚。
        let turn = if fixed {
            chrome::fixed_turn_style(configured)
        } else {
            configured
        };
        let label_of = |turn| match turn {
            PageTurn::Slide => tr(cx, "page_turn.slide"),
            PageTurn::SlideUp => tr(cx, "page_turn.slide_up"),
            PageTurn::Cover => tr(cx, "page_turn.cover"),
            PageTurn::Curl => tr(cx, "page_turn.curl"),
            PageTurn::Scroll => tr(cx, "page_turn.scroll"),
            PageTurn::None => tr(cx, "page_turn.none"),
        };
        let items: Vec<(PageTurn, SharedString)> = PageTurn::ALL
            .iter()
            .copied()
            .filter(|value| !fixed || chrome::fixed_turn_applies(*value))
            .map(|value| (value, label_of(value)))
            .collect();
        let current = label_of(turn);
        let title = tr(cx, "field.page_turn");
        let sheet = cx.entity().clone();
        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .child(div().flex_1().min_w_0().text_base().child(title.clone()))
            .child(
                Button::new("reader-page-turn")
                    .outline()
                    .small()
                    .label(current.to_string())
                    .dropdown_caret(true)
                    .accessibility_label(title)
                    .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                        items.iter().fold(menu, |menu, (value, label)| {
                            let checked = *value == turn;
                            let value = *value;
                            // 菜单闭包是 `Fn`，每一项的回调要各自一份句柄（`Entity` 不 `Copy`）。
                            let sheet = sheet.clone();
                            menu.item(PopupMenuItem::new(label.clone()).checked(checked).on_click(
                                move |_, _, cx| {
                                    sheet.update(cx, |this, cx| this.set_page_turn(value, cx))
                                },
                            ))
                        })
                    }),
            )
            .into_any_element()
    }

    /// 「翻页动画」写回 `appearance` 那一节：它不是排版失效键，换它不该让任何一章重排
    /// （见 `folio_core::infra::config::PageTurn` 为什么这一格不在 `Reader` 里）。
    fn set_page_turn(&mut self, turn: PageTurn, cx: &mut Context<Self>) {
        ConfigHolder::edit_appearance(cx, |appearance| appearance.page_turn = turn);
        cx.notify();
    }

    /// 「字重」那一格：三档里选一档，写回 `Reader`（字重会改变字形，必须重排）。
    ///
    /// 配到档位之间的数（手改配置）在按钮上落回最近的一档，见
    /// [`Reader::nearest_font_weight`]；菜单里选一档就把它写实。
    fn weight_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let weight = Reader::nearest_font_weight(ConfigHolder::reader(cx).font_weight);
        let label_of = |weight: u16| match weight {
            300 => tr(cx, "font_weight.light"),
            700 => tr(cx, "font_weight.bold"),
            _ => tr(cx, "font_weight.regular"),
        };
        // 菜单闭包是 `'static`：标签先在这里按当前 locale 译好，闭包里只搬已译好的串。
        let items: Vec<(u16, SharedString)> = Reader::FONT_WEIGHT_PRESETS
            .iter()
            .copied()
            .map(|preset| (preset, label_of(preset)))
            .collect();
        let current = label_of(weight);
        let title = tr(cx, "field.font_weight");
        let sheet = cx.entity().clone();
        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .child(div().flex_1().min_w_0().text_base().child(title.clone()))
            .child(
                Button::new("reader-font-weight")
                    .outline()
                    .small()
                    .label(current.to_string())
                    .dropdown_caret(true)
                    .accessibility_label(title)
                    .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                        items.iter().fold(menu, |menu, (preset, label)| {
                            let checked = *preset == weight;
                            let preset = *preset;
                            // 菜单闭包是 `Fn`，每一项的回调要各自一份句柄（`Entity` 不 `Copy`）。
                            let sheet = sheet.clone();
                            menu.item(PopupMenuItem::new(label.clone()).checked(checked).on_click(
                                move |_, _, cx| {
                                    sheet.update(cx, |this, cx| this.set_font_weight(preset, cx))
                                },
                            ))
                        })
                    }),
            )
            .into_any_element()
    }

    fn set_font_weight(&mut self, weight: u16, cx: &mut Context<Self>) {
        ConfigHolder::edit_reader(cx, |prefs| prefs.font_weight = weight);
        cx.notify();
    }

    /// 「文字方向」那一格：按书原样 / 竖排 / 横排。
    ///
    /// 竖排只在分页单栏那一档落地（见 `folio_core::layout::LayoutOptions::allow_vertical`）；
    /// 卡片里不摆那句解释 —— 这一屏的每格都只有标题与控件，长解释在完整设置屏那一格。
    fn direction_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let direction = ConfigHolder::reader(cx).text_direction;
        let label_of = |direction| match direction {
            TextDirection::Auto => tr(cx, "text_direction.auto"),
            TextDirection::Vertical => tr(cx, "text_direction.vertical"),
            TextDirection::Horizontal => tr(cx, "text_direction.horizontal"),
        };
        // 菜单闭包是 `'static`：标签先在这里按当前 locale 译好，闭包里只搬已译好的串。
        let items: Vec<(TextDirection, SharedString)> = TextDirection::ALL
            .iter()
            .copied()
            .map(|value| (value, label_of(value)))
            .collect();
        let current = label_of(direction);
        let title = tr(cx, "field.text_direction");
        let sheet = cx.entity().clone();
        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .child(div().flex_1().min_w_0().text_base().child(title.clone()))
            .child(
                Button::new("reader-text-direction")
                    .outline()
                    .small()
                    .label(current.to_string())
                    .dropdown_caret(true)
                    .accessibility_label(title)
                    .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                        items.iter().fold(menu, |menu, (value, label)| {
                            let checked = *value == direction;
                            let value = *value;
                            let sheet = sheet.clone();
                            menu.item(PopupMenuItem::new(label.clone()).checked(checked).on_click(
                                move |_, _, cx| {
                                    sheet.update(cx, |this, cx| this.set_text_direction(value, cx))
                                },
                            ))
                        })
                    }),
            )
            .into_any_element()
    }

    fn set_text_direction(&mut self, direction: TextDirection, cx: &mut Context<Self>) {
        ConfigHolder::edit_reader(cx, |prefs| prefs.text_direction = direction);
        cx.notify();
    }

    /// 「正文字体」那一格：这台设备装着的字体家族，可搜索。
    ///
    /// 列表来自 `TextSystem::all_font_names()`（含随包字体与这本书注册进来的内嵌字体），
    /// 不再让读者手打一个可能不存在的家族名 —— 打错的名字在排版层会静默退回读者的字体，
    /// 而读者以为自己改成功了。
    fn font_row(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let picker = window.use_keyed_state("reader-font-picker", cx, {
            let get: Rc<dyn Fn(&App) -> String> =
                Rc::new(|cx: &App| ConfigHolder::reader(cx).font_family.clone());
            let set: FontFamilySetter = Rc::new(|value: String, cx: &mut App| {
                ConfigHolder::edit_reader(cx, |prefs| prefs.font_family = value);
            });
            move |window, cx| FontPicker::new(get, set, window, cx)
        });
        v_flex()
            .w_full()
            .gap_1()
            .child(div().text_base().child(tr(cx, "field.font_family")))
            .child(picker.clone())
            .into_any_element()
    }

    /// 顶上那三段。`segmented` 的选中块、滑动指示器与键盘焦点都归 `TabBar`；这一屏
    /// 只把「选了哪一段」记成 [`SheetTab`]，三段共用同一条渲染路径。
    fn tab_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let sheet = cx.entity().clone();
        let mut bar = TabBar::new("reader-settings-tabs")
            .segmented()
            .small()
            .w_full()
            .selected_index(self.tab.index())
            .on_click(move |index, _, cx| {
                let tab = SheetTab::from_index(*index);
                sheet.update(cx, |this, cx| this.select_tab(tab, cx));
            });
        for tab in SheetTab::ALL {
            // 三段等宽：标签是「外观 / 字体 / 排版」这类短词，一段按内容宽、另一段
            // 被挤成一条缝会让读者以为那一段点不到。`TabBar` 会把 tab 的 `flex_1`
            // 交给外面那层 wrapper，所以这里的 grow 真的落到三等分上。
            bar = bar.child(Tab::new().label(tab.label(cx)).flex_1());
        }
        div()
            .w_full()
            .px_4()
            .pt_2()
            .pb_3()
            .child(bar)
            .into_any_element()
    }

    fn select_tab(&mut self, tab: SheetTab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            cx.notify();
        }
    }

    /// 当前这一段的内容。三组各自只装自己那几格，切组不新建滑条 —— 句柄都在
    /// `self` 上跨帧活着，拖到一半切走再切回来不会跳回旧值。
    fn section_rows(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.tab {
            SheetTab::Appearance => self.appearance_rows(cx),
            SheetTab::Font => self.font_rows(window, cx),
            SheetTab::Layout => self.layout_rows(cx),
        }
    }

    /// 外观：阅读页自己的浅深；亮度跟着光线走；翻页那两张纸怎么走、走多久，
    /// 是钉下来就不常碰的一对。
    fn appearance_rows(&self, cx: &mut Context<Self>) -> AnyElement {
        // 「跟随系统」念成一句话而不是「100%」：那一档应用确实什么都没接管。
        let brightness_label = match ConfigHolder::appearance(cx).brightness {
            None => tr(cx, "appearance.follow_system").to_string(),
            Some(value) => format!("{}%", (value * 100.0).round()),
        };
        v_flex()
            .w_full()
            .gap_4()
            .px_4()
            .pt_1()
            .pb_6()
            .child(self.reading_mode_row(cx))
            .child(self.slider_row(
                tr(cx, "reader.brightness"),
                brightness_label,
                &self.brightness,
                cx,
            ))
            .child(self.turn_row(cx))
            .child(self.slider_row(
                tr(cx, "reader.turn_duration"),
                format!("{:.0} ms", ConfigHolder::appearance(cx).turn_duration),
                &self.turn_duration,
                cx,
            ))
            .into_any_element()
    }

    /// 字体：家族名要打字，放在字号前面 —— 先认这一本书用的字，再调大小与粗细。
    fn font_rows(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let prefs = ConfigHolder::reader(cx);
        v_flex()
            .w_full()
            .gap_4()
            .px_4()
            .pt_1()
            .pb_6()
            .child(self.font_row(window, cx))
            .child(self.slider_row(
                tr(cx, "field.font_size"),
                format!("{:.1}", prefs.font_size),
                &self.font_size,
                cx,
            ))
            .child(self.weight_row(cx))
            .into_any_element()
    }

    /// 排版：对齐与缩进定段落形状，行距 / 字距 / 段距 / 版心 / 留白定整页密度，
    /// 文字方向排最后 —— 它换的是整页那根轴。
    ///
    /// 顺序照读者扫一页的目光：先看两端怎么收、首行怎么起，再往下看行与段之间的空，
    /// 最后才是版心、页边与整页方向。
    fn layout_rows(&self, cx: &mut Context<Self>) -> AnyElement {
        let prefs = ConfigHolder::reader(cx);
        // 版心推到最左就是 `0`，而 `0` 不是「栏宽 0 像素」，是「不设上限」。那一格没有
        // 别的读数能替它说话，所以念一句话；数字照旧带单位，与翻页时长那一格同一格式。
        let measure = prefs.max_measure;
        let measure_label = if measure <= MAX_MEASURE_RANGE.0 {
            tr(cx, "reader.measure_fill").to_string()
        } else {
            format!("{measure:.0} px")
        };
        let justify_label = tr(cx, "field.justify");
        // `Switch` 是受控的：它只把「想要的值」交出来，写配置与重画由这一屏做。
        // 重画是必须的 —— 不 notify 的话开关要等下一帧别的事发生才翻过去。
        let sheet = cx.entity().clone();
        v_flex()
            .w_full()
            .gap_4()
            .px_4()
            .pt_1()
            .pb_6()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(div().text_base().child(justify_label.clone()))
                    .child(
                        Switch::new("reader-justify")
                            .checked(prefs.justify)
                            .accessibility_label(justify_label)
                            .on_click(move |value, _, cx| {
                                ConfigHolder::edit_reader(cx, |prefs| prefs.justify = *value);
                                sheet.update(cx, |_, cx| cx.notify());
                            }),
                    ),
            )
            .child(self.slider_row(
                tr(cx, "field.indent"),
                format!("{:.2}", prefs.indent_em),
                &self.indent,
                cx,
            ))
            .child(self.slider_row(
                tr(cx, "field.line_height"),
                format!("{:.2}", prefs.line_height),
                &self.line_height,
                cx,
            ))
            .child(self.slider_row(
                tr(cx, "field.letter_spacing"),
                format!("{:.2}", prefs.letter_spacing_em),
                &self.letter_spacing,
                cx,
            ))
            .child(self.slider_row(
                tr(cx, "field.paragraph_gap"),
                format!("{:.2}", prefs.paragraph_gap_em),
                &self.paragraph_gap,
                cx,
            ))
            .child(self.slider_row(
                tr(cx, "field.max_measure"),
                measure_label,
                &self.max_measure,
                cx,
            ))
            .child(self.slider_row(
                tr(cx, "field.margin_x"),
                format!("{:.0} px", prefs.margin_x),
                &self.margin_x,
                cx,
            ))
            .child(self.slider_row(
                tr(cx, "field.margin_y"),
                format!("{:.0} px", prefs.margin_y),
                &self.margin_y,
                cx,
            ))
            .child(self.direction_row(cx))
            .into_any_element()
    }
}

impl Render for ReadingSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync(window, cx);
        let sheet = BottomSheet::new(&self.sheet, "reader-settings")
            .expanded(SHEET_EXPANDED)
            .peek(SHEET_PEEK);
        // 收稳了就不白算这一屏：三段的滑条与那颗下拉每帧都要重新取配置、比阈值。
        // 元素照旧构造（弹簧的采样在里面），只是这一帧不给内容。
        if self.sheet.read(cx).is_idle() {
            return sheet.into_any_element();
        }
        // 分段控件钉在把手下面：读者在排版那一段往下滚到「左右页边距」时，
        // 不用再滚回顶上去换组。内容那一格自己滚，与目录那一屏同一条形状。
        let tabs = self.tab_bar(cx);
        let rows = self.section_rows(window, cx);
        sheet.header(tabs).child(rows).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    //! 这一屏能在宿主上跑完的部分：开合、三段各画一帧不炸，以及「十条滑条各接哪一格
    //! 配置」那两条接线（初值接错、别处改了不跟）。摆出来的样子 —— 每格多高、卡片停在
    //! 哪一档 —— 只有真机量得出来。
    //!
    //! 点名导入而不 `use super::*`：`gpui_kit` 根把 `gpui` 整个转了出来，里面有一个
    //! 叫 `test` 的属性宏，glob 会让下面的 `#[test]` 递归展开成它。

    use folio_core::Config;
    use gpui_kit::component::Root;
    use gpui_kit::component::slider::SliderState;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AnyWindowHandle, App, AppContext as _, Entity, TestAppContext, px, size};

    use super::{ReadingSheet, SheetTab};
    use crate::ui::features::reader::model::session::ReaderSession;
    use crate::ui::services::config::ConfigHolder;

    /// 一扇把**这一屏**摆在根上的窗。底部弹层与八条滑条都要真窗口，而这一屏的渲染路径
    /// 正是这里要测的那条。
    fn open_sheet(cx: &mut TestAppContext) -> (Entity<ReadingSheet>, AnyWindowHandle) {
        let mut built = None;
        let handle = cx.open_window(size(px(390.), px(844.)), |window, cx| {
            ConfigHolder::install(Config::default(), cx);
            let reader = cx.new(|cx| ReaderSession::boot(cx));
            let sheet = cx.new(move |cx| ReadingSheet::new(reader, cx));
            let root_view = sheet.clone();
            built = Some(sheet);
            Root::new(root_view, window, cx)
        });
        (built.expect("这一屏该建起来"), handle.into())
    }

    /// 滑条此刻指着哪儿。
    fn thumb(state: &Entity<SliderState>, cx: &App) -> f32 {
        state.read(cx).value().end()
    }

    /// 底栏那颗「设置」按下去该把卡片抬起来，再按一下收回去。
    ///
    /// 画一帧这件事本身就是这条的一半：三段滑条、那颗往上开的下拉、那只行内输入框都在
    /// 这条路上，漏接任何一根线都会红在这里，而不是等读者在真机上看到一屏空白。
    #[gpui_kit::test]
    fn the_card_opens_and_draws_then_closes_on_the_same_button(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (sheet, handle) = open_sheet(cx);
        sheet.update(cx, |sheet, cx| sheet.toggle(cx));
        assert!(
            sheet.read_with(cx, |sheet, cx| sheet.is_open(cx)),
            "toggle 该把这块底部弹层开起来"
        );
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("这一屏该画得出来");
        sheet.update(cx, |sheet, cx| sheet.toggle(cx));
        assert!(
            !sheet.read_with(cx, |sheet, cx| sheet.is_open(cx)),
            "再按一下同一格是「不要了」"
        );
        // 退场那一程也得画得出来：弹簧还在走，元素还在构造。
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("收下去那一程该画得出来");
    }

    /// 默认停在外观，三段各画一帧；每一段都得走同一条渲染路径而不只是开屏那一段。
    #[gpui_kit::test]
    fn the_card_switches_between_appearance_font_and_layout(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (sheet, handle) = open_sheet(cx);
        sheet.update(cx, |sheet, cx| sheet.toggle(cx));
        assert_eq!(
            sheet.read_with(cx, |sheet, _| sheet.tab),
            SheetTab::Appearance,
            "开屏先看环境那一组"
        );
        for tab in SheetTab::ALL {
            sheet.update(cx, |sheet, cx| sheet.select_tab(tab, cx));
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .unwrap_or_else(|_| panic!("{tab:?} 这一组该画得出来"));
            assert_eq!(sheet.read_with(cx, |sheet, _| sheet.tab), tab);
        }
    }

    /// 每一条滑条的初值必须就是这一本书此刻用的那一格。
    ///
    /// 接错一条不会报错，只会让读者拖行距那一条时把段距改了 —— 而卡片上念的还是行距。
    #[gpui_kit::test]
    fn each_slider_starts_on_the_value_the_book_is_using(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (sheet, _handle) = open_sheet(cx);
        let prefs = cx.update(|cx| ConfigHolder::reader(cx));
        let turn_duration = cx.update(|cx| ConfigHolder::appearance(cx).turn_duration);
        sheet.read_with(cx, |sheet, cx| {
            assert_eq!(thumb(&sheet.font_size, cx), prefs.font_size);
            assert_eq!(thumb(&sheet.line_height, cx), prefs.line_height);
            assert_eq!(thumb(&sheet.letter_spacing, cx), prefs.letter_spacing_em);
            assert_eq!(thumb(&sheet.paragraph_gap, cx), prefs.paragraph_gap_em);
            assert_eq!(thumb(&sheet.indent, cx), prefs.indent_em);
            assert_eq!(thumb(&sheet.margin_x, cx), prefs.margin_x);
            assert_eq!(thumb(&sheet.margin_y, cx), prefs.margin_y);
            assert_eq!(thumb(&sheet.max_measure, cx), prefs.max_measure);
            assert_eq!(thumb(&sheet.turn_duration, cx), turn_duration);
            // 「跟随系统」在配置里是 `None`，滑条上得有个能显示的数 —— 顶格。
            assert_eq!(
                thumb(&sheet.brightness, cx),
                super::BRIGHTNESS_RANGE.1,
                "亮度那一格在「跟随系统」时该停在顶格"
            );
        });
    }

    /// 完整设置那一屏改了行距，卡片重画一帧就得把滑块挪过去（反过来也一样：这里的拖动
    /// 落进配置）。这条钉的是 `sync` 那一张表，漏一条就是「两处各说一套」。
    ///
    /// 阈值比较不许把这一趟吃掉：`0.005` 远小于这里改的 `0.75`。
    #[gpui_kit::test]
    fn a_change_made_elsewhere_lands_on_its_own_slider(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (sheet, handle) = open_sheet(cx);
        let before = cx.update(|cx| ConfigHolder::reader(cx));
        let target = (before.line_height + 0.75).min(super::LINE_HEIGHT_RANGE.1);
        cx.update(|cx| ConfigHolder::edit_reader(cx, |prefs| prefs.line_height = target));
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("这一屏该画得出来");
        sheet.read_with(cx, |sheet, cx| {
            assert_eq!(
                thumb(&sheet.line_height, cx),
                target,
                "设置屏改了行距，卡片那条滑条该跟着走"
            );
            assert_eq!(
                thumb(&sheet.paragraph_gap, cx),
                before.paragraph_gap_em,
                "挪行距那一条不该把段距也带过去"
            );
        });
    }

    /// 阅读设置里那一格浅深只写**阅读页自己**：全局 `mode` 一个字节都不许动。
    ///
    /// 这条钉的是读者报过的那件事：在书里点一下夜间，退出之后整个软件都黑着。
    #[gpui_kit::test]
    fn the_reader_mode_row_does_not_rewrite_the_app_appearance(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (sheet, _handle) = open_sheet(cx);
        cx.update(|cx| {
            ConfigHolder::edit_appearance(cx, |appearance| {
                appearance.mode = folio_core::Mode::Light
            })
        });

        sheet.update(cx, |sheet, cx| {
            sheet.set_reading_mode(Some(folio_core::Mode::Dark), cx)
        });
        let appearance = cx.update(|cx| ConfigHolder::appearance(cx));
        assert_eq!(appearance.reading_mode, Some(folio_core::Mode::Dark));
        assert_eq!(
            appearance.mode,
            folio_core::Mode::Light,
            "阅读页那一格不该改到全局浅深"
        );

        sheet.update(cx, |sheet, cx| sheet.set_reading_mode(None, cx));
        assert_eq!(
            cx.update(|cx| ConfigHolder::appearance(cx)).reading_mode,
            None,
            "「跟随软件」在配置里只有一个写法：None"
        );
    }
}
