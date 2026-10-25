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
//! 所以两档都留着 —— 最高那档留出窗口高的一成半，往下拖到半屏那一档就能一边调一边看着字走。
//! 这条是这块弹层与目录那块唯一的差别：目录不需要看见正文，需要。
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

use gpui_kit::Anchor;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::{ActiveTheme, Sizable, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Window, div,
};

use folio_core::PageTurn;

use crate::ui::components::{BottomSheet, BottomSheetState};
use crate::ui::features::reader::model::chrome::{
    BRIGHTNESS_RANGE, FONT_SIZE_RANGE, LINE_HEIGHT_RANGE, MARGIN_X_RANGE, MAX_MEASURE_RANGE,
    PARAGRAPH_GAP_RANGE, TURN_DURATION_RANGE,
};
use crate::ui::services::config::ConfigHolder;
use crate::ui::services::l10n::tr;

/// 卡片能停的最高那一档：窗口高的 85%。与目录那一屏同一档，留出的那一成半是「自己
/// 压着的是哪本书」的那一点余量。
const SHEET_EXPANDED: f32 = 0.85;
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
    brightness: Entity<SliderState>,
    font_size: Entity<SliderState>,
    line_height: Entity<SliderState>,
    paragraph_gap: Entity<SliderState>,
    max_measure: Entity<SliderState>,
    margin_x: Entity<SliderState>,
    turn_duration: Entity<SliderState>,
    _watches: Vec<Subscription>,
}

impl ReadingSheet {
    /// 建这一屏。七条滑条各订阅一次：拖动当场生效（换字号重排 / 换背光 / 下一趟翻页走多久），
    /// 所以这里没有「应用」按钮，也没有改了不生效的中间态。
    pub(in crate::ui::features::reader) fn new(cx: &mut Context<Self>) -> Self {
        let prefs = ConfigHolder::reader(cx);
        let appearance = ConfigHolder::appearance(cx);
        // 「跟随系统」在滑条上就是推到顶：配置里那一格是 `None`，滑条上得有个能显示的数。
        let brightness = appearance.brightness.unwrap_or(BRIGHTNESS_RANGE.1);
        let brightness = slider(BRIGHTNESS_RANGE, 0.01, brightness, cx);
        let font_size = slider(FONT_SIZE_RANGE, 0.5, prefs.font_size, cx);
        let line_height = slider(LINE_HEIGHT_RANGE, 0.05, prefs.line_height, cx);
        let paragraph_gap = slider(PARAGRAPH_GAP_RANGE, 0.05, prefs.paragraph_gap_em, cx);
        let max_measure = slider(MAX_MEASURE_RANGE, 20.0, prefs.max_measure, cx);
        let margin_x = slider(MARGIN_X_RANGE, 8.0, prefs.margin_x, cx);
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
            Self::watch(&paragraph_gap, cx, |value, cx| {
                ConfigHolder::edit_reader(cx, |prefs| prefs.paragraph_gap_em = value);
            }),
            Self::watch(&max_measure, cx, |value, cx| {
                ConfigHolder::edit_reader(cx, |prefs| prefs.max_measure = value);
            }),
            Self::watch(&margin_x, cx, |value, cx| {
                ConfigHolder::edit_reader(cx, |prefs| prefs.margin_x = value);
            }),
            Self::watch(&turn_duration, cx, |value, cx| {
                ConfigHolder::edit_appearance(cx, |appearance| appearance.turn_duration = value);
            }),
        ];
        Self {
            sheet,
            brightness,
            font_size,
            line_height,
            paragraph_gap,
            max_measure,
            margin_x,
            turn_duration,
            _watches: watches,
        }
    }

    /// 一条滑条订阅一次：把拖出来的那个数交给写入口。
    ///
    /// 写成关联函数而不是 `self` 方法：七条路唯一的差别是往哪一格写，而订阅句柄要由
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
            (&self.paragraph_gap, prefs.paragraph_gap_em),
            (&self.max_measure, prefs.max_measure),
            (&self.margin_x, prefs.margin_x),
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
        let turn = ConfigHolder::appearance(cx).page_turn;
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
        ConfigHolder::edit_appearance(cx, |appearance| appearance.page_turn = turn);
        cx.notify();
    }

    /// 「正文字体」那一格：一个行内输入框，家族名填进去就当场生效。
    ///
    /// 不列本机字体清单：这台设备上不存在的那个家族名，排版层会退回设备能解析的名字
    /// （见 `ui/shared/measure.rs` 的泛族名处理）而不是把字画成方框，而清单反而会把
    /// 没装的也列出来 —— 与完整设置那一屏「字体」同一判据，两处不该给读者两种说法。
    ///
    /// 输入框的状态按元素键跨帧持有（框架自己的 Input 字段就是这么干的），所以输入到
    /// 一半的一次重画不会把已打的字扔掉。
    fn font_row(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let family: SharedString = ConfigHolder::reader(cx).font_family.clone().into();
        let state = window.use_keyed_state("reader-font-family", cx, {
            let value = family.clone();
            move |window, cx| {
                let input = cx.new(|cx| InputState::new(window, cx).default_value(value));
                // 输入即写配置：外壳那份订阅会落盘并重排。订阅句柄必须跟着状态一起活，
                // 掉了事件就断。
                let watch = cx.subscribe(&input, move |_, input, event: &InputEvent, cx| {
                    if let InputEvent::Change = event {
                        let value = input.read(cx).value();
                        // 空串不改：清空输入框不该把正文换成 gpui 的默认家族，
                        // 那是个没有汉字的拉丁字体。
                        if !value.trim().is_empty() {
                            ConfigHolder::edit_reader(cx, |prefs| {
                                prefs.font_family = value.to_string()
                            });
                        }
                    }
                });
                (input, watch)
            }
        });
        // 配置在他处被改（完整设置那一屏、未来的同步）时把显示拉回来。
        state.update(cx, |(input, _), cx| {
            if input.read(cx).value() != family {
                input.update(cx, |input, cx| input.set_value(family.clone(), window, cx));
            }
        });
        let (input, _) = state.read(cx);
        v_flex()
            .w_full()
            .gap_1()
            .child(div().text_base().child(tr(cx, "field.font_family")))
            .child(Input::new(input).w_full())
            .into_any_element()
    }

    /// 卡片里的九格，按读者动它们的频度排：亮度一天里跟着光线走；字号换一本书动一次；
    /// 行距、段距、留白、版心是调完字号顺手看一眼的那几样；家族名要打字，放在动作类
    /// 那两格之前；翻页动画与它的时长留在最下面 —— 钉下来就不再碰。
    ///
    /// 没有分组标题：卡片本身已经是一组，标题只会把九格挤成十四行。
    fn rows(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let prefs = ConfigHolder::reader(cx);
        // 「跟随系统」念成一句话而不是「100%」：那一档应用确实什么都没接管。
        let brightness_label = match ConfigHolder::appearance(cx).brightness {
            None => tr(cx, "appearance.follow_system").to_string(),
            Some(value) => format!("{}%", (value * 100.0).round()),
        };
        // 版心推到最左就是 `0`，而 `0` 不是「栏宽 0 像素」，是「不设上限」。那一格没有
        // 别的读数能替它说话，所以念一句话；数字照旧带单位，与翻页时长那一格同一格式。
        let measure = prefs.max_measure;
        let measure_label = if measure <= MAX_MEASURE_RANGE.0 {
            tr(cx, "reader.measure_fill").to_string()
        } else {
            format!("{measure:.0} px")
        };
        v_flex()
            .w_full()
            .gap_4()
            .px_4()
            .pt_2()
            .pb_6()
            .child(self.slider_row(
                tr(cx, "reader.brightness"),
                brightness_label,
                &self.brightness,
                cx,
            ))
            .child(self.slider_row(
                tr(cx, "field.font_size"),
                format!("{:.1}", prefs.font_size),
                &self.font_size,
                cx,
            ))
            .child(self.slider_row(
                tr(cx, "field.line_height"),
                format!("{:.2}", prefs.line_height),
                &self.line_height,
                cx,
            ))
            .child(self.slider_row(
                tr(cx, "field.paragraph_gap"),
                format!("{:.2}", prefs.paragraph_gap_em),
                &self.paragraph_gap,
                cx,
            ))
            .child(self.slider_row(
                tr(cx, "field.margin_x"),
                format!("{:.0} px", prefs.margin_x),
                &self.margin_x,
                cx,
            ))
            .child(self.slider_row(
                tr(cx, "field.max_measure"),
                measure_label,
                &self.max_measure,
                cx,
            ))
            .child(self.font_row(window, cx))
            .child(self.turn_row(cx))
            .child(self.slider_row(
                tr(cx, "reader.turn_duration"),
                format!("{:.0} ms", ConfigHolder::appearance(cx).turn_duration),
                &self.turn_duration,
                cx,
            ))
            .into_any_element()
    }
}

impl Render for ReadingSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync(window, cx);
        let sheet = BottomSheet::new(&self.sheet, "reader-settings")
            .expanded(SHEET_EXPANDED)
            .peek(SHEET_PEEK);
        // 收稳了就不白算这一屏：九格滑条与那颗下拉每帧都要重新取配置、比阈值。
        // 元素照旧构造（弹簧的采样在里面），只是这一帧不给内容。
        if self.sheet.read(cx).is_idle() {
            return sheet.into_any_element();
        }
        let rows = self.rows(window, cx);
        sheet.child(rows).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    //! 这一屏能在宿主上跑完的部分：开合、九格画一帧不炸，以及「七条滑条各接哪一格配置」
    //! 那两条接线（初值接错、别处改了不跟）。摆出来的样子 —— 每格多高、卡片停在哪一档 ——
    //! 只有真机量得出来。
    //!
    //! 点名导入而不 `use super::*`：`gpui_kit` 根把 `gpui` 整个转了出来，里面有一个
    //! 叫 `test` 的属性宏，glob 会让下面的 `#[test]` 递归展开成它。

    use folio_core::Config;
    use gpui_kit::component::Root;
    use gpui_kit::component::slider::SliderState;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AnyWindowHandle, App, AppContext as _, Entity, TestAppContext, px, size};

    use super::ReadingSheet;
    use crate::ui::services::config::ConfigHolder;

    /// 一扇把**这一屏**摆在根上的窗。底部弹层与七条滑条都要真窗口，而这一屏的渲染路径
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
    /// 画一帧这件事本身就是这条的一半：九格滑条、那颗往上开的下拉、那只行内输入框都在
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
            assert_eq!(thumb(&sheet.paragraph_gap, cx), prefs.paragraph_gap_em);
            assert_eq!(thumb(&sheet.margin_x, cx), prefs.margin_x);
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
}
