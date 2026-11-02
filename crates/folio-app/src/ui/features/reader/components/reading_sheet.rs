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
//! ## 一格三个把手：`−` 滑条 `+`
//!
//! 数值那几格是「标题 + 读数」一行，下面一行 `−` 一颗钮、缩短居中的滑条、`+` 一颗钮。
//! 通栏滑条从前是唯一的入口，于是两个端点都顶在屏幕边上，拇指要够到边框才调得到最小档；
//! 而滑块本身只有那么大，精确停在一档全靠运气。两颗钮借完整设置屏那族 `−`/`+` 同一形态
//! （`ghost` + `large` + 图标，见 `settings::view::compact` 的 `RowKind::Number`），
//! 滑条留着负责粗跳。
//!
//! 按钮走的是**粗档**（`chrome::SliderSteps::nudge`），比滑条自己的落档粗一个量级：
//! 按细档走满程要划 56–200 下，那不该是按钮的活。两条路都只改同一份 `SliderState`
//! 与同一条写入口 [`Knob::apply`] —— 拖动与按钮各留一份落点迟早会飘成「按 + 改的是行距」
//! 那种形状。
//!
//! ## 量程与步长不在这里抄第二份
//!
//! 每条滑条的两端引用 [`chrome`] 里那几对常数，而它们引用的是配置自己的上下限
//! （`Reader::clamped` 与 `Appearance::normalized` 夹的就是同一对数）。界面里一旦写死
//! 第二个「最大行距」，滑条就能拖到一个存不进配置的位置：屏幕上数变了，落盘那刻被夹回去，
//! 下一帧又缩回来。`chrome::FONT_SIZE_RANGE` 那一档是**故意**比配置窄的（阅读时顺手调只
//! 铺常用的一段），那种例外写在它自己的注释里，并由 `model/chrome.rs` 的量程那条门钉住。
//! 步长同理：两档各住在 `chrome` 的 `*_STEPS` 里，不写在渲染代码上。

use std::rc::Rc;

use gpui_kit::Anchor;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Window, div,
};

use folio_core::{ImageScale, Mode, PageTurn, Reader, TextDirection};

use crate::ui::components::{BottomSheet, BottomSheetState, FontFamilySetter, FontPicker};
use crate::ui::features::reader::model::chrome::{
    self, BRIGHTNESS_RANGE, FONT_SIZE_RANGE, INDENT_RANGE, LETTER_SPACING_RANGE, LINE_HEIGHT_RANGE,
    MARGIN_X_RANGE, MARGIN_Y_RANGE, MAX_MEASURE_RANGE, PARAGRAPH_GAP_RANGE, SliderSteps,
    TURN_DURATION_RANGE,
};
use crate::ui::services::config::ConfigHolder;
use crate::ui::services::l10n::{tf, tr};

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

/// 这一屏上那十条「拖一下就走一值」的格。名字就是它写进配置的那一格。
///
/// 为什么要一张表而不是十个字段各接各的闭包：**滑条拖动、`−` / `+` 按钮、`sync` 从别处
/// 读回来**这三条路必须落在同一个配置字段上。从前只有拖动那条路有落点（十条近乎相同的
/// 闭排在 `new` 里），加按钮就是往第二份落点上添一次 —— 那正是「按 + 改的是段距」这种
/// 形状的来源。有了这一张表，「漏了一条」在写第三条路时会直接编译不过。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Knob {
    Brightness,
    FontSize,
    LineHeight,
    LetterSpacing,
    ParagraphGap,
    Indent,
    MaxMeasure,
    MarginX,
    MarginY,
    TurnDuration,
}

/// `ReadingSheet::knobs` 那个数组的长度必须与 [`Knob::ALL`] 一致。
const _: () = assert!(Knob::ALL.len() == 10);

impl Knob {
    /// 顺序就是 `knobs` 数组的顺序。
    const ALL: [Self; 10] = [
        Self::Brightness,
        Self::FontSize,
        Self::LineHeight,
        Self::LetterSpacing,
        Self::ParagraphGap,
        Self::Indent,
        Self::MaxMeasure,
        Self::MarginX,
        Self::MarginY,
        Self::TurnDuration,
    ];

    fn index(self) -> usize {
        match self {
            Self::Brightness => 0,
            Self::FontSize => 1,
            Self::LineHeight => 2,
            Self::LetterSpacing => 3,
            Self::ParagraphGap => 4,
            Self::Indent => 5,
            Self::MaxMeasure => 6,
            Self::MarginX => 7,
            Self::MarginY => 8,
            Self::TurnDuration => 9,
        }
    }

    /// 两颗钮的元素 id 前缀。每一段里一个名字只出现一次，但拼上名字才不靠那个巧合。
    fn key(self) -> &'static str {
        match self {
            Self::Brightness => "brightness",
            Self::FontSize => "font-size",
            Self::LineHeight => "line-height",
            Self::LetterSpacing => "letter-spacing",
            Self::ParagraphGap => "paragraph-gap",
            Self::Indent => "indent",
            Self::MaxMeasure => "max-measure",
            Self::MarginX => "margin-x",
            Self::MarginY => "margin-y",
            Self::TurnDuration => "turn-duration",
        }
    }

    /// 两端。引用 `chrome` 里那族常数 —— 文件头那条：不在这里抄第二份量程。
    fn range(self) -> (f32, f32) {
        match self {
            Self::Brightness => BRIGHTNESS_RANGE,
            Self::FontSize => FONT_SIZE_RANGE,
            Self::LineHeight => LINE_HEIGHT_RANGE,
            Self::LetterSpacing => LETTER_SPACING_RANGE,
            Self::ParagraphGap => PARAGRAPH_GAP_RANGE,
            Self::Indent => INDENT_RANGE,
            Self::MaxMeasure => MAX_MEASURE_RANGE,
            Self::MarginX => MARGIN_X_RANGE,
            Self::MarginY => MARGIN_Y_RANGE,
            Self::TurnDuration => TURN_DURATION_RANGE,
        }
    }

    /// 两档步长：滑条的细档与 `±` 的粗档。
    fn steps(self) -> SliderSteps {
        match self {
            Self::Brightness => chrome::BRIGHTNESS_STEPS,
            Self::FontSize => chrome::FONT_SIZE_STEPS,
            Self::LineHeight => chrome::LINE_HEIGHT_STEPS,
            Self::LetterSpacing => chrome::LETTER_SPACING_STEPS,
            Self::ParagraphGap => chrome::PARAGRAPH_GAP_STEPS,
            Self::Indent => chrome::INDENT_STEPS,
            Self::MaxMeasure => chrome::MAX_MEASURE_STEPS,
            Self::MarginX => chrome::MARGIN_X_STEPS,
            Self::MarginY => chrome::MARGIN_Y_STEPS,
            Self::TurnDuration => chrome::TURN_DURATION_STEPS,
        }
    }

    /// 这一本书**此刻在用的**那一格数：滑条初值与 `sync` 的权威都从这一条来。
    ///
    /// 亮度那一档特殊：配置里「跟随系统」是 `None`，而滑条上得有个能显示的数，于是摆顶格。
    /// 这一条从前在「建滑条」和「每帧同步」两处各写一遍，合到这里只剩一份。
    fn configured(self, cx: &App) -> f32 {
        match self {
            Self::Brightness => ConfigHolder::appearance(cx)
                .brightness
                .unwrap_or(BRIGHTNESS_RANGE.1),
            Self::TurnDuration => ConfigHolder::appearance(cx).turn_duration,
            Self::FontSize => ConfigHolder::reader(cx).font_size,
            Self::LineHeight => ConfigHolder::reader(cx).line_height,
            Self::LetterSpacing => ConfigHolder::reader(cx).letter_spacing_em,
            Self::ParagraphGap => ConfigHolder::reader(cx).paragraph_gap_em,
            Self::Indent => ConfigHolder::reader(cx).indent_em,
            Self::MaxMeasure => ConfigHolder::reader(cx).max_measure,
            Self::MarginX => ConfigHolder::reader(cx).margin_x,
            Self::MarginY => ConfigHolder::reader(cx).margin_y,
        }
    }

    /// 写回配置那一格。拖动与 `±` 两条路都走这扇门，所以「按哪一格改哪一格」只有一个地方
    /// 可以说错。
    fn apply(self, value: f32, cx: &mut App) {
        match self {
            Self::Brightness => ConfigHolder::edit_book_appearance(cx, |appearance| {
                // 顶格归成 `None`：配置里于是只有「跟随系统」一个写法，滑条与文件不会是
                // 两套「最大亮度」（同 `Appearance::normalized` 那一条）。
                appearance.brightness = if value >= BRIGHTNESS_RANGE.1 {
                    None
                } else {
                    Some(value)
                }
            }),
            Self::TurnDuration => ConfigHolder::edit_book_appearance(cx, |appearance| {
                appearance.turn_duration = value
            }),
            Self::FontSize => ConfigHolder::edit_book_reader(cx, |prefs| prefs.font_size = value),
            Self::LineHeight => {
                ConfigHolder::edit_book_reader(cx, |prefs| prefs.line_height = value)
            }
            Self::LetterSpacing => {
                ConfigHolder::edit_book_reader(cx, |prefs| prefs.letter_spacing_em = value)
            }
            Self::ParagraphGap => {
                ConfigHolder::edit_book_reader(cx, |prefs| prefs.paragraph_gap_em = value)
            }
            Self::Indent => ConfigHolder::edit_book_reader(cx, |prefs| prefs.indent_em = value),
            Self::MaxMeasure => {
                ConfigHolder::edit_book_reader(cx, |prefs| prefs.max_measure = value)
            }
            Self::MarginX => ConfigHolder::edit_book_reader(cx, |prefs| prefs.margin_x = value),
            Self::MarginY => ConfigHolder::edit_book_reader(cx, |prefs| prefs.margin_y = value),
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

/// 建一条滑条：两端与细档来自 [`Knob`]，初值来自 [`Knob::configured`]。
///
/// `max` 必须写在 `min` 前面：`SliderState::new()` 的默认区间是 0..**100**，而每一步
/// setter 都会拿当前区间去 `clamp` 已有的值。翻页时长那一条的下限是 120，先写 `min`
/// 就是 `clamp(120.0, 100.0)` —— 当场 panic（实测炸在 `ReadingPage::new` 那一格）。
fn slider(knob: Knob, cx: &mut Context<ReadingSheet>) -> Entity<SliderState> {
    let (range, steps) = (knob.range(), knob.steps());
    let value = knob.configured(cx).clamp(range.0, range.1);
    cx.new(|_| {
        SliderState::new()
            .max(range.1)
            .min(range.0)
            .step(steps.slide)
            .default_value(value)
    })
}

/// 阅读设置那一屏自己的状态。跨帧持有，一页一个（阅读页建它，随阅读页一起活）。
pub(in crate::ui::features::reader) struct ReadingSheet {
    /// 底部弹层：开合与停在哪一档都在它手里。句柄必须跨帧持有 —— 弹层每帧都要构造
    /// （弹簧的采样在里面），与下面那几条滑条同一条理由。
    sheet: Entity<BottomSheetState>,
    /// 顶上停在哪一组。跨帧留着：读者刚在排版里调完行距，收起再打开该还在排版。
    tab: SheetTab,
    /// 十条滑条的状态，按 [`Knob::ALL`] 的顺序摆，取用走 [`ReadingSheet::knob`]。
    /// 这里刻意不留十个字段加十个各自的写入口：值只有一份，拖动、`−` / `+`、`sync`
    /// 三条路都改同一格、都从同一扇门写回配置。
    knobs: [Entity<SliderState>; 10],
    _watches: Vec<Subscription>,
}

impl ReadingSheet {
    /// 建这一屏。十条滑条各订阅一次：拖动当场生效（换字号重排 / 换背光 / 下一趟翻页走多久），
    /// 所以这里没有「应用」按钮，也没有改了不生效的中间态。
    pub(in crate::ui::features::reader) fn new(cx: &mut Context<Self>) -> Self {
        // 十条滑条由 [`Knob`] 那一张表建出来：两端、细档、初值都在表里，这里不再逐格写数。
        let knobs = Knob::ALL.map(|knob| slider(knob, cx));
        // 弹层自己也要观察：它不是这一屏的子视图，`notify` 只有观察方才收得到，
        // 而开合、换档、读者一把下拉关掉全靠随后那一次重画。
        let sheet = cx.new(BottomSheetState::closed);
        let mut watches = vec![cx.observe(&sheet, |_, _, cx| cx.notify())];
        // 一条滑条订阅一次：拖出来的那个数交给 [`Knob::apply`] —— 与 `−` / `+` 同一扇门。
        for (state, knob) in knobs.iter().zip(Knob::ALL) {
            watches.push(cx.subscribe(state, move |_, _, event, cx| {
                if let SliderEvent::Change(value) = event {
                    knob.apply(value.end(), cx);
                }
            }));
        }
        Self {
            sheet,
            tab: SheetTab::default(),
            knobs,
            _watches: watches,
        }
    }

    /// 某一格此刻的滑条状态。数组按 [`Knob::ALL`] 摆，索引由 [`Knob::index`] 给。
    fn knob(&self, knob: Knob) -> &Entity<SliderState> {
        &self.knobs[knob.index()]
    }

    /// 一颗 `−` / `+` 按下去走的那一档（粗档，见 [`Knob::steps`]）。
    ///
    /// 先加粗档、再落回滑条自己的细格、最后夹进区间：顺序反了会在端点上留下一个格外的数
    /// —— 亮度推到顶再加半档是 1.05，落盘时被配置夹回 1.0，而屏幕上还念着 1.05。
    ///
    /// **两条都要写**。`SliderState::set_value` 只 `notify`、**不发** `SliderEvent::Change`
    /// （gpui-base 的 `slider.rs` 里只有拖动那一条路 `cx.emit`）。少写配置那一笔就是
    /// 「钮按下去滑块动了、书里的字没变」；只写配置不写滑块是反过来的坏法：值最终对，
    /// 但按钮那一下看着没反应，下一帧 `sync` 还会把滑块拽回旧值。
    fn nudge(&mut self, knob: Knob, sign: f32, window: &mut Window, cx: &mut Context<Self>) {
        let steps = knob.steps();
        let (min, max) = knob.range();
        let at = self.knob(knob).read(cx).value().end();
        let next = ((at + sign * steps.nudge) / steps.slide).round() * steps.slide;
        let next = next.clamp(min, max);
        let state = self.knob(knob).clone();
        state.update(cx, |state, cx| state.set_value(next, window, cx));
        knob.apply(next, cx);
        cx.notify();
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
    /// 「跟随系统」时没有数 —— 滑条上把它显示成顶格，配置里它是 `None`（那条换算住在
    /// [`Knob::configured`]，与建滑条时用的是同一条权威，所以这里不可能漏掉某一条）。
    ///
    /// 带阈值比较：拖动的时候滑条自己就是权威，每帧再按配置读回来的数写回去，
    /// 就是拿手指算出来的值和落盘算出来的值互相追。
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (index, knob) in Knob::ALL.iter().enumerate() {
            let value = knob.configured(cx);
            let state = &self.knobs[index];
            if (state.read(cx).value().end() - value).abs() > 0.005 {
                state.update(cx, |state, cx| state.set_value(value, window, cx));
            }
        }
    }

    /// 一格的三个把手：上面一行「标签在左、当前值在右」，下面一行 `−` 滑条 `+`。
    ///
    /// 滑条从前是通栏的，于是两个端点都顶在屏幕边上，拇指够不着；滑块又只有那么大，
    /// 精确停在一档全靠运气。现在滑条让出两端（`flex_1` + `min_w_0`，居中那一段），
    /// 两端各一颗钮 —— 形态借完整设置屏那族 `−`/`+`（`settings::view::compact` 的
    /// `RowKind::Number`：`ghost` + `large` + 图标），钮走的是**粗档**，满程按十几下到
    /// 几十下算，细跳仍然交给滑条。
    ///
    /// 标签这一份文本三处要用（行首、两颗钮的可读名、滑条自己的可读名），所以先把三份
    /// 名字取好再搭元素。
    fn stepper_row(
        &self,
        knob: Knob,
        label: SharedString,
        value: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (min, max) = knob.range();
        let at = self.knob(knob).read(cx).value().end();
        let state = self.knob(knob).clone();
        // 两颗钮各要一个拥有者：点击闭包是 `'static` 的，同一份句柄不能移动两次
        //（与 `settings::view::compact` 那族 `−` / `+` 同一写法）。
        let down_sheet = cx.entity();
        let up_sheet = cx.entity();
        let down = tf(cx, "reader.nudge_down", &[("field", &label)]);
        let up = tf(cx, "reader.nudge_up", &[("field", &label)]);
        let track_label = label.clone();
        v_flex()
            .w_full()
            .gap_1()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(div().text_base().child(label.clone()))
                    .child(
                        div()
                            .text_base()
                            .text_color(cx.theme().muted_foreground)
                            .child(value),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new(format!("{}-minus", knob.key()))
                            .ghost()
                            .large()
                            // 推到端点就是禁用，而不是按下无反应：这一格到底到边了没有，
                            // 读者要从控件本身读出来。
                            .disabled(at <= min + 0.001)
                            .icon(IconName::Minus)
                            .accessibility_label(down)
                            .on_click(move |_, window, app| {
                                down_sheet.update(app, |this, cx| {
                                    this.nudge(knob, -1.0, window, cx);
                                });
                            }),
                    )
                    .child(
                        Slider::new(&state)
                            .flex_1()
                            .min_w_0()
                            .accessibility_label(track_label),
                    )
                    .child(
                        Button::new(format!("{}-plus", knob.key()))
                            .ghost()
                            .large()
                            .disabled(at >= max - 0.001)
                            .icon(IconName::Plus)
                            .accessibility_label(up)
                            .on_click(move |_, window, app| {
                                up_sheet.update(app, |this, cx| {
                                    this.nudge(knob, 1.0, window, cx);
                                });
                            }),
                    ),
            )
            .into_any_element()
    }

    /// 「阅读浅深」那一格：跟随软件 / 跟随系统 / 浅色 / 深色，**按下的这一格是这一本书的**。
    ///
    /// 与设置屏那一屏的「外观」分文不远但各管各的：那一格定的是整个应用（书架、弹窗、
    /// 下一次开书之前），这一格只定阅读页 —— 关掉书，屏上回到那一格选的样子。
    /// 「跟随软件」就是这一本没单独设过：那一档在差量里不落字。它住在「外观」这组是因为
    /// 换浅深当场生效，与排版无关（不做排版失效键，见 `folio_core::infra::config::Appearance`）。
    fn reading_mode_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = ConfigHolder::appearance(cx).reading_mode;
        let label_of = |value: Option<Mode>| match value {
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
                    .label(label_of(current).to_string())
                    .dropdown_caret(true)
                    .accessibility_label(title)
                    .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                        items.iter().fold(menu, |menu, (value, label)| {
                            let checked = *value == current;
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

    /// 这一本的阅读浅深。`None` = 跟随软件外观，那一档**不落进差量**（与默认同值）。
    fn set_reading_mode(&mut self, reading: Option<Mode>, cx: &mut Context<Self>) {
        ConfigHolder::edit_book_appearance(cx, |appearance| appearance.reading_mode = reading);
        cx.notify();
    }

    /// 「翻页动画」那一格：一颗往上开的下拉，与它正下面那条时长滑的是同一张纸。
    ///
    /// 下拉**往上开**（`Anchor::BottomLeft`）：这一行长在贴着屏幕底的卡片里，往下开的
    /// 话那两块菜单有一半掉到屏幕外，读者只点得到上面那一块。
    ///
    /// 六档**全摆**，不按当前摆法筛：这一条列表就是摆法的上游 —— 选「上下滚动」会把这本
    /// 书带进连续，选任何一张纸的走法会把它带出连续（`chrome::reconcile` 在渲染帧里解）。
    /// 定版式与可重排同一个可选集：折页也从位图页上烘一张捕获纹理来卷。
    fn turn_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let configured = ConfigHolder::appearance(cx).page_turn;
        let turn = configured;
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
        ConfigHolder::edit_book_appearance(cx, |appearance| appearance.page_turn = turn);
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
        ConfigHolder::edit_book_reader(cx, |prefs| prefs.font_weight = weight);
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
        ConfigHolder::edit_book_reader(cx, |prefs| prefs.text_direction = direction);
        cx.notify();
    }

    /// 「图片缩放」那一格：跟书 / 铺满屏幕 / 原始尺寸。
    ///
    /// 三档各说的是什么、边界在哪（小图不许被撑大、原始只保证不溢出屏幕），判据写在
    /// `folio_core::infra::config::ImageScale` 那一处；卡片里不摆解释 —— 这一屏每格
    /// 只有标题与控件，长解释在完整设置屏那一格。
    fn image_scale_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let scale = ConfigHolder::reader(cx).image_scale;
        let label_of = |scale| match scale {
            ImageScale::Follow => tr(cx, "image_scale.follow"),
            ImageScale::Screen => tr(cx, "image_scale.screen"),
            ImageScale::Original => tr(cx, "image_scale.original"),
        };
        // 菜单闭包是 `'static`：标签先在这里按当前 locale 译好，闭包里只搬已译好的串。
        let items: Vec<(ImageScale, SharedString)> = ImageScale::ALL
            .iter()
            .copied()
            .map(|value| (value, label_of(value)))
            .collect();
        let current = label_of(scale);
        let title = tr(cx, "field.image_scale");
        let sheet = cx.entity().clone();
        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .child(div().flex_1().min_w_0().text_base().child(title.clone()))
            .child(
                Button::new("reader-image-scale")
                    .outline()
                    .small()
                    .label(current.to_string())
                    .dropdown_caret(true)
                    .accessibility_label(title)
                    .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                        items.iter().fold(menu, |menu, (value, label)| {
                            let checked = *value == scale;
                            let value = *value;
                            let sheet = sheet.clone();
                            menu.item(PopupMenuItem::new(label.clone()).checked(checked).on_click(
                                move |_, _, cx| {
                                    sheet.update(cx, |this, cx| this.set_image_scale(value, cx))
                                },
                            ))
                        })
                    }),
            )
            .into_any_element()
    }

    fn set_image_scale(&mut self, scale: ImageScale, cx: &mut Context<Self>) {
        ConfigHolder::edit_book_reader(cx, |prefs| prefs.image_scale = scale);
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
                ConfigHolder::edit_book_reader(cx, |prefs| prefs.font_family = value);
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
    /// 「恢复本书默认」那一格：抹掉这一本书的全部差量，每一格回到设置屏那个默认档。
    ///
    /// 摆在整张卡片的最底下而不是各段里 —— 它清的是这一本的**全部**，塞进某一段会被
    /// 读成只管那一段。按下去之后各条滑条当场跳回默认位置：滑条读的就是生效值。
    ///
    /// 这一本没被单独设过时那颗钮是灰的 —— 反馈走禁用态，不在旁边补一行灰字（与这屏
    /// 其余各格同一条规矩）。
    fn reset_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let label = tr(cx, "reader.reset_defaults");
        let untouched = ConfigHolder::book(cx).is_none_or(|patch| patch.is_empty());
        div()
            .w_full()
            .child(
                Button::new("reader-reset-defaults")
                    .outline()
                    .small()
                    .w_full()
                    .disabled(untouched)
                    .label(label.clone())
                    .accessibility_label(label)
                    .on_click(|_, _, cx| ConfigHolder::clear_book(cx)),
            )
            .into_any_element()
    }

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
            .child(self.stepper_row(
                Knob::Brightness,
                tr(cx, "reader.brightness"),
                brightness_label,
                cx,
            ))
            .child(self.turn_row(cx))
            .child(self.stepper_row(
                Knob::TurnDuration,
                tr(cx, "reader.turn_duration"),
                format!("{:.0} ms", ConfigHolder::appearance(cx).turn_duration),
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
            .child(self.stepper_row(
                Knob::FontSize,
                tr(cx, "field.font_size"),
                format!("{:.1}", prefs.font_size),
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
                                ConfigHolder::edit_book_reader(cx, |prefs| prefs.justify = *value);
                                sheet.update(cx, |_, cx| cx.notify());
                            }),
                    ),
            )
            .child(self.stepper_row(
                Knob::Indent,
                tr(cx, "field.indent"),
                format!("{:.2}", prefs.indent_em),
                cx,
            ))
            .child(self.stepper_row(
                Knob::LineHeight,
                tr(cx, "field.line_height"),
                format!("{:.2}", prefs.line_height),
                cx,
            ))
            .child(self.stepper_row(
                Knob::LetterSpacing,
                tr(cx, "field.letter_spacing"),
                format!("{:.2}", prefs.letter_spacing_em),
                cx,
            ))
            .child(self.stepper_row(
                Knob::ParagraphGap,
                tr(cx, "field.paragraph_gap"),
                format!("{:.2}", prefs.paragraph_gap_em),
                cx,
            ))
            .child(self.stepper_row(
                Knob::MaxMeasure,
                tr(cx, "field.max_measure"),
                measure_label,
                cx,
            ))
            .child(self.stepper_row(
                Knob::MarginX,
                tr(cx, "field.margin_x"),
                format!("{:.0} px", prefs.margin_x),
                cx,
            ))
            .child(self.stepper_row(
                Knob::MarginY,
                tr(cx, "field.margin_y"),
                format!("{:.0} px", prefs.margin_y),
                cx,
            ))
            .child(self.direction_row(cx))
            .child(self.image_scale_row(cx))
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
        // 各段的行在下面，整张卡片最底下那一格是「恢复本书默认」（它管的不是某一段）。
        sheet
            .header(tabs)
            .child(rows)
            .child(self.reset_row(cx))
            .into_any_element()
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

    use folio_core::{Config, ImageScale};
    use gpui_kit::component::Root;
    use gpui_kit::component::slider::SliderState;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AnyWindowHandle, App, AppContext as _, Entity, TestAppContext, px, size};

    use super::{Knob, ReadingSheet, SheetTab};
    use crate::ui::services::config::ConfigHolder;

    /// 一扇把**这一屏**摆在根上的窗。底部弹层与八条滑条都要真窗口，而这一屏的渲染路径
    /// 正是这里要测的那条。
    fn open_sheet(cx: &mut TestAppContext) -> (Entity<ReadingSheet>, AnyWindowHandle) {
        let mut built = None;
        let handle = cx.open_window(size(px(390.), px(844.)), |window, cx| {
            ConfigHolder::install(Config::default(), cx);
            let sheet = cx.new(ReadingSheet::new);
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
    ///
    /// **这一张表故意手写、不用 `Knob::ALL` 循环**：滑条初值走的正是 `Knob::configured`，
    /// 拿同一个函数当期望值是自证（`configured` 里把 `LineHeight` 那臂写成读 `paragraph_gap_em`
    /// 也照样绿）。这里每一份期望值都是独立取自配置结构体的那一格，才量得出接线。
    #[gpui_kit::test]
    fn each_slider_starts_on_the_value_the_book_is_using(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (sheet, _handle) = open_sheet(cx);
        let prefs = cx.update(|cx| ConfigHolder::reader(cx));
        let turn_duration = cx.update(|cx| ConfigHolder::appearance(cx).turn_duration);
        sheet.read_with(cx, |sheet, cx| {
            assert_eq!(thumb(sheet.knob(Knob::FontSize), cx), prefs.font_size);
            assert_eq!(thumb(sheet.knob(Knob::LineHeight), cx), prefs.line_height);
            assert_eq!(
                thumb(sheet.knob(Knob::LetterSpacing), cx),
                prefs.letter_spacing_em
            );
            assert_eq!(
                thumb(sheet.knob(Knob::ParagraphGap), cx),
                prefs.paragraph_gap_em
            );
            assert_eq!(thumb(sheet.knob(Knob::Indent), cx), prefs.indent_em);
            assert_eq!(thumb(sheet.knob(Knob::MarginX), cx), prefs.margin_x);
            assert_eq!(thumb(sheet.knob(Knob::MarginY), cx), prefs.margin_y);
            assert_eq!(thumb(sheet.knob(Knob::MaxMeasure), cx), prefs.max_measure);
            assert_eq!(
                thumb(sheet.knob(Knob::TurnDuration), cx),
                turn_duration,
                "翻页时长那条接的是 `appearance`，不是 `reader` 那一节"
            );
            // 「跟随系统」在配置里是 `None`，滑条上得有个能显示的数 —— 顶格。
            assert_eq!(
                thumb(sheet.knob(Knob::Brightness), cx),
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
                thumb(sheet.knob(Knob::LineHeight), cx),
                target,
                "设置屏改了行距，卡片那条滑条该跟着走"
            );
            assert_eq!(
                thumb(sheet.knob(Knob::ParagraphGap), cx),
                before.paragraph_gap_em,
                "挪行距那一条不该把段距也带过去"
            );
        });
    }

    /// `−` / `+` 那两颗钮：滑条、配置、以及滑条自己的细档格，三样必须同时对。
    ///
    /// 三条红路各是一条：`SliderState::set_value` **不发** `SliderEvent::Change`（只有拖动
    /// 发），少了 [`Knob::apply`] 那一笔就是「滑块动了而书里的字没变」；反过来只写配置不写
    /// 滑块，`sync` 下一帧把滑块拽回旧值，钮按下去看着没反应。第三件是格子对齐 ——
    /// `nudge` 是粗档，按完必须是 `slide` 的整数倍，不然随后一次拖动会先「跳」一下才回格。
    ///
    /// 先挂一本书：这一屏写的是**这一本的差量**（`edit_book_reader`），没挂着时那一格只留
    /// 一句 warn 就返回，于是滑块被 `sync` 拽回原值，看上去像按钮没使（与浅深那一格同一条门径）。
    /// 顺序也有关：`open_sheet` 里那次 `ConfigHolder::install` 会换掉整份配置，书要在那之后挂。
    #[gpui_kit::test]
    fn a_nudge_moves_the_thumb_and_the_config_together(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (sheet, handle) = open_sheet(cx);
        cx.update(|cx| ConfigHolder::set_book(cx, Some(folio_core::BookSettings::default())));
        let steps = Knob::FontSize.steps();
        let before = cx.update(|cx| ConfigHolder::reader(cx));
        for _ in 0..10 {
            cx.update_window(handle, |_, window, cx| {
                sheet.update(cx, |sheet, cx| {
                    sheet.nudge(Knob::FontSize, 1.0, window, cx);
                });
            })
            .expect("这一屏该画得出来");
        }
        let wanted = before.font_size + 10.0 * steps.nudge;
        let shown = sheet.read_with(cx, |sheet, cx| thumb(sheet.knob(Knob::FontSize), cx));
        let stored = cx.update(|cx| ConfigHolder::reader(cx));
        assert_eq!(shown, wanted, "连点十次该走十个粗档（滑块上的读数）");
        assert_eq!(
            stored.font_size, wanted,
            "按出来的数必须落进配置，否则字不会变"
        );
        assert_eq!(
            stored.line_height, before.line_height,
            "按字号的 + 不该把行距一起带走"
        );
        let grid = shown / steps.slide;
        assert!(
            (grid - grid.round()).abs() < 1e-3,
            "粗档按完必须落在滑条自己的细档格上，实测量到 {shown}"
        );
    }

    /// 往一个方向按到底：夹在端点就停住，不许写出量程外的数。
    ///
    /// 禁用态在元素上（`at <= min` / `at >= max` 那两把闸），这里钉的是它依据的那两个数：
    /// 端点之外没有值，`sync` 于是也不会反过来把滑块拽一下。
    #[gpui_kit::test]
    fn nudging_past_the_end_stops_at_the_end(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (sheet, handle) = open_sheet(cx);
        cx.update(|cx| ConfigHolder::set_book(cx, Some(folio_core::BookSettings::default())));
        let max = Knob::MarginX.range().1;
        for _ in 0..60 {
            cx.update_window(handle, |_, window, cx| {
                sheet.update(cx, |sheet, cx| {
                    sheet.nudge(Knob::MarginX, 1.0, window, cx);
                });
            })
            .expect("这一屏该画得出来");
        }
        let shown = sheet.read_with(cx, |sheet, cx| thumb(sheet.knob(Knob::MarginX), cx));
        assert_eq!(
            shown, max,
            "按过头要停在端点，而不是滑条上一个存不进配置的位置"
        );
        assert_eq!(
            cx.update(|cx| ConfigHolder::reader(cx)).margin_x,
            max,
            "端点那一格写进配置的也要是同一个数"
        );
    }

    /// 阅读设置里那一格浅深只写**这一本书**：软件外观 `mode` 与全局默认都不许动。
    ///
    /// 这条钉的是读者报过的那件事：在书里点一下夜间，退出之后整个软件都黑着。现在它连
    /// 「往后每一本都黑着」也不会了 —— 落点是这一本的差量，与默认同值时那一格自动消失。
    #[gpui_kit::test]
    fn the_reader_mode_row_writes_this_book_and_nothing_else(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (sheet, _handle) = open_sheet(cx);
        cx.update(|cx| {
            ConfigHolder::edit_appearance(cx, |appearance| {
                appearance.mode = folio_core::Mode::Light
            });
            // 这一格写的是「这一本」，所以得先有一本书挂着：没挂着时
            // `edit_book_appearance` 只留一句 warn 就返回（见 `services::config`），
            // 少了这一步下面全是空跑。
            ConfigHolder::set_book(cx, Some(folio_core::BookSettings::default()));
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
        assert_eq!(
            cx.update(|cx| ConfigHolder::default_appearance(cx))
                .reading_mode,
            None,
            "按下去只动这一本：设置屏那个全局默认要还空着"
        );
        assert!(
            cx.update(|cx| ConfigHolder::book(cx))
                .is_some_and(|patch| !patch.is_empty()),
            "这一本的差量里该落下这一格"
        );

        sheet.update(cx, |sheet, cx| sheet.set_reading_mode(None, cx));
        assert_eq!(
            cx.update(|cx| ConfigHolder::appearance(cx)).reading_mode,
            None,
            "「跟随软件」在差量里只有一个写法：不落字"
        );
        assert!(
            cx.update(|cx| ConfigHolder::book(cx))
                .is_none_or(|patch| patch.is_empty()),
            "选回「跟随软件」后这本书该回到完全跟随默认，差量里不许留着残格"
        );
    }

    /// 「图片缩放」那一格：三档都选得动，而落点只有**这一本书**。
    ///
    /// 两件事各自有红路：三档是从 `ImageScale::ALL` 铺的，少一份 locale 或漏一个臂，
    /// `every_language_carries_the_same_keys` 与这里的标签断言分别红；落点必须是这一本的
    /// 差量（与浅深那一格同一条规矩），不然在书里点一下「铺满屏幕」就把往后每一本都改了。
    /// 默认那一档不落差量 —— 与默认同值时那一格要自己消失，否则退出这本书之后残格还挂着。
    #[gpui_kit::test]
    fn the_image_scale_row_writes_this_book_and_shows_its_three_tiers(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (sheet, handle) = open_sheet(cx);
        // 这一格写的是「这一本」，所以得先有一本书挂着（同浅深那一格的门径）。
        cx.update(|cx| ConfigHolder::set_book(cx, Some(folio_core::BookSettings::default())));

        // 卡片开起来，三段各画一帧：这一格长在版式那一段里，不切过去就画不到，
        // 「接了线但从没画出来」这种形状只有走一遍渲染路径才暴露。
        sheet.update(cx, |sheet, cx| sheet.toggle(cx));
        for tab in SheetTab::ALL {
            sheet.update(cx, |sheet, cx| sheet.select_tab(tab, cx));
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .expect("这一屏该画得出来");
        }

        for scale in [ImageScale::Follow, ImageScale::Screen, ImageScale::Original] {
            sheet.update(cx, |sheet, cx| sheet.set_image_scale(scale, cx));
            assert_eq!(
                cx.update(|cx| ConfigHolder::reader(cx)).image_scale,
                scale,
                "按下去生效值该是 {} 那一档",
                scale.as_str()
            );
            let patch = cx.update(|cx| ConfigHolder::book(cx));
            let blank = patch.as_ref().is_none_or(|book| book.is_empty());
            // 与**默认那一档**同值时不落差量。默认现在是 `Screen`（铺满），
            // 从前这里是 `Follow` —— 这一格跟着默认走，写在下面那条形同「默认是铺满」的断言里。
            if scale == ImageScale::default() {
                assert!(
                    blank,
                    "默认档不落差量：与默认同值时这一格要消失，实得 {patch:?}"
                );
            } else {
                assert!(
                    !blank,
                    "{} 那一档该落在这一本的差量里，实得 {patch:?}",
                    scale.as_str()
                );
            }
        }
        assert_eq!(
            cx.update(|cx| ConfigHolder::default_reader(cx)).image_scale,
            ImageScale::Screen,
            "书里那一格不许改到全局默认；默认档是「铺满」（2026-10-31 从跟书换过来）"
        );
    }
}
