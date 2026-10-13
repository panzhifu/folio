//! Compact 档（手机）的设置：与桌面**同一份页面规格**，钻入式的紧凑布局。
//!
//! 桌面的 `Settings` 组件是「侧栏 + 正文」两栏，塞不进一块 360dp 的小屏；这里吃
//! 同一份 [`spec_pages`]，渲染成「页列表 → 全屏子页」：根屏一行一页（图标 + 标题），
//! 点进子页后标题行带返回、分组标题当节头，每行按字段种类摆控件 ——
//!
//! - 开关用 `Switch`（受控值：回调给的就是要写进去的新值）；
//! - 下拉用浮层菜单，与桌面同一条路（`plans/facts.md`：浮层在 Android 上点得住；
//!   曾经「手机全用普通按钮」的约束已随行列表一起退场）；
//! - 数值用 −/+ 步进行：拖动滑条要跨帧持有状态，步进无状态、在什么屏上都稳，
//!   范围与步长直接取自规格；
//! - 文本用行内输入框，状态按 (页, 组, 行) 键控持有 —— 框架自己的 Input 字段
//!   就是这么干的（见 `setting/fields/string.rs`），输入到一半的重画不会把状态扔掉。
//!
//! 返回键的契约与曾经的 `AccountView` 相同：`go_back` 往外弹一层，根屏返回 `false`
//! 让外壳接着决定（最终是退出应用）。

use gpui_kit::Anchor;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Div, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement as _, Styled,
    TestSupportExt as _, Window, div, px,
};

use super::spec::{self, RowKind};
use super::spec_pages;
use crate::ui::shared::l10n::tr;
use crate::ui::widgets::hairline;

/// 内容区两侧的留白，和书架的格间距同一档。
const GUTTER: f32 = 12.0;

/// 「我的」这一格里现在站着哪一屏。页用**序号**寻址 —— 与桌面深链同一条
/// `SettingsPage::index` 顺序，规格数组的下标就是它。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    /// 入口列表。
    Root,
    Page(usize),
}

pub(crate) struct CompactSettings {
    screen: Screen,
}

impl CompactSettings {
    /// 由外壳（「我的」目的地）与阅读页各建一屏，所以是 `pub(crate)` 而不是 `pub(super)`。
    pub(crate) fn new() -> Self {
        Self {
            screen: Screen::Root,
        }
    }

    fn open(&mut self, page_ix: usize, cx: &mut Context<Self>) {
        self.screen = Screen::Page(page_ix);
        cx.notify();
    }

    /// 系统返回键（escape）打到这里：往外弹一层，并报告有没有消费掉。
    ///
    /// 已经在入口列表时返回 `false`，让外壳接着决定（最终是退出应用）—— 这一屏不该
    /// 自己吞掉返回。
    pub(crate) fn go_back(&mut self, cx: &mut Context<Self>) -> bool {
        match self.screen {
            Screen::Root => false,
            Screen::Page(_) => {
                self.screen = Screen::Root;
                cx.notify();
                true
            }
        }
    }

    /// 根屏：一行一页，图标 + 标题 + 进去的箭头。
    fn root_screen(&self, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity().clone();
        let mut body = v_flex().w_full().gap_1();
        for (ix, page) in spec_pages(cx).iter().enumerate() {
            let entity = entity.clone();
            body = body.child(
                ListItem::new(SharedString::from(format!("settings-page-{ix}")))
                    .w_full()
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .gap_2()
                            .child(Icon::new(page.icon).small())
                            .child(div().min_w_0().flex_1().child(page.title.clone()))
                            .child(
                                Icon::new(gpui_kit::assets::IconName::ChevronRight)
                                    .small()
                                    .text_color(cx.theme().muted_foreground),
                            ),
                    )
                    .accessibility_label(page.title.clone())
                    .on_click(move |_, _, cx| entity.update(cx, |this, cx| this.open(ix, cx))),
            );
        }
        Self::scrollable(body)
    }

    /// 子页顶部那一条：返回 + 标题。返回回到页列表，和正文那条返回同一个道理。
    fn header(&self, title: &SharedString, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().clone();
        let back_label = tr(cx, "shelf.back");
        h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .px_3()
            .h(px(44.0))
            .child(
                Button::new("settings-back")
                    .ghost()
                    .small()
                    .icon(gpui_kit::assets::IconName::ArrowLeft)
                    .label(back_label.clone().to_string())
                    .accessibility_label(back_label)
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |this, cx| {
                            this.screen = Screen::Root;
                            cx.notify();
                        })
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title.clone()),
            )
    }

    /// 子页正文：分组标题当节头，组里的行依次排开。
    fn page_screen(
        &self,
        page_ix: usize,
        page: &spec::Page,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut body = v_flex().w_full().gap_1();
        for (group_ix, group) in page.groups.iter().enumerate() {
            body = body.child(Self::section_title(group.title.clone(), cx));
            for (row_ix, row) in group.rows.iter().enumerate() {
                body = body.child(self.row(page_ix, group_ix, row_ix, row, window, cx));
            }
        }
        Self::scrollable(body)
    }

    fn section_title(text: SharedString, cx: &App) -> impl IntoElement {
        div()
            .w_full()
            .pt_3()
            .pb_1()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(text)
    }

    /// 标题块：一行小字 + 可选的说明。字段行左侧共用的那半边。
    ///
    /// 返回具体的 `Div` 而不是 `impl IntoElement`：调用方还要往这块上接 `flex_1()`
    /// 与 `min_w_0()`，而 `Styled` 的那一整套方法只长在具体类型上 —— 擦成
    /// `impl IntoElement` 之后调用方就再也加不了样式。
    fn label_block(row: &spec::Row, cx: &App) -> Div {
        let mut col = v_flex()
            .gap(px(2.0))
            .child(div().text_sm().child(row.title.clone()));
        if let Some(desc) = &row.desc {
            col = col.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(desc.clone()),
            );
        }
        col
    }

    /// 一个字段行。控件的种类决定右侧摆什么（或整行摆什么）。
    fn row(
        &self,
        page_ix: usize,
        group_ix: usize,
        row_ix: usize,
        row: &spec::Row,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = move |part: &str| {
            SharedString::from(format!("settings-{part}-{page_ix}-{group_ix}-{row_ix}"))
        };
        match &row.kind {
            RowKind::Toggle { get, set } => {
                let (get, set) = (get.clone(), set.clone());
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_3()
                    .child(Self::label_block(row, cx).flex_1().min_w_0())
                    .child(
                        Switch::new(id("toggle"))
                            .checked(get(cx))
                            .on_click(move |checked, _, cx| set(*checked, cx)),
                    )
                    .into_any_element()
            }
            RowKind::Dropdown {
                options,
                scrollable,
                get,
                set,
            } => {
                let current = get(cx);
                // 取值对不上任何一个选项值时显示裸值兜底，而不是留一个空按钮 ——
                // 与桌面那份映射同一条规矩。
                let label = options
                    .iter()
                    .find(|(value, _)| *value == current)
                    .map(|(_, label)| label.clone())
                    .unwrap_or_else(|| current.clone());
                let (options, current, scrollable, set) =
                    (options.clone(), current, *scrollable, set.clone());
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_3()
                    .child(Self::label_block(row, cx).flex_1().min_w_0())
                    .child(
                        Button::new(id("dropdown"))
                            .outline()
                            .small()
                            .label(label.to_string())
                            .dropdown_caret(true)
                            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                                let menu = options.iter().fold(menu, |menu, (value, label)| {
                                    let checked = &current == value;
                                    let value = value.clone();
                                    // 菜单本身是 `Fn` 闭包，每一项的回调又要一个拥有者：
                                    // 写回闭包在这里按项克隆，而不是从外层挪走。
                                    let set = set.clone();
                                    menu.item(
                                        PopupMenuItem::new(label.clone())
                                            .checked(checked)
                                            .on_click(move |_, _, cx| set(value.clone(), cx)),
                                    )
                                });
                                menu.scrollable(scrollable)
                            }),
                    )
                    .into_any_element()
            }
            RowKind::Number { spec, get, set } => {
                let (spec, get, set) = (*spec, get.clone(), set.clone());
                let value = get(cx);
                // 整数步长的显示整数，小数步长显示两位 —— 17.0 与 1.75 各自念起来顺眼。
                let shown = if spec.step >= 1.0 {
                    format!("{value:.0}")
                } else {
                    format!("{value:.2}")
                };
                // 两颗步进钮各要一个拥有者，所以各拿一份 `Rc` 的克隆 —— 同一份读写，
                // 只是不能共用一次移动。
                let (get_minus, set_minus) = (get.clone(), set.clone());
                let (get_plus, set_plus) = (get, set);
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .child(Self::label_block(row, cx).flex_1().min_w_0())
                    .child(
                        Button::new(id("number-minus"))
                            .ghost()
                            .small()
                            .icon(gpui_kit::assets::IconName::Minus)
                            .on_click(move |_, _, cx| {
                                set_minus((get_minus(cx) - spec.step).max(spec.min), cx)
                            }),
                    )
                    .child(div().min_w(px(40.0)).text_sm().child(shown))
                    .child(
                        Button::new(id("number-plus"))
                            .ghost()
                            .small()
                            .icon(gpui_kit::assets::IconName::Plus)
                            .on_click(move |_, _, cx| {
                                set_plus((get_plus(cx) + spec.step).min(spec.max), cx)
                            }),
                    )
                    .into_any_element()
            }
            RowKind::Text { get, set } => {
                let (get, set) = (get.clone(), set.clone());
                let value = get(cx);
                let state = window.use_keyed_state(id("text"), cx, {
                    let value = value.clone();
                    move |window, cx| {
                        let input = cx.new(|cx| InputState::new(window, cx).default_value(value));
                        // 输入即写配置：外壳那份订阅会落盘并重排。订阅句柄必须跟着
                        // 状态一起活，掉了事件就断 —— 与框架的 string 字段同一形状。
                        // 订阅回调要 `'static`，所以给它一份自己的克隆，而不是从
                        // 建状态的那层闭包里把 `set` 挪走。
                        let set = set.clone();
                        let watch =
                            cx.subscribe(&input, move |_, input, event: &InputEvent, cx| {
                                if let InputEvent::Change = event {
                                    let value = input.read(cx).value();
                                    set(value, cx);
                                }
                            });
                        (input, watch)
                    }
                });
                // 配置在他处被改（极端情形：未来的同步）时把显示拉回来。
                state.update(cx, |(input, _), cx| {
                    if input.read(cx).value() != value {
                        input.update(cx, |input, cx| {
                            input.set_value(value.clone(), window, cx);
                        });
                    }
                });
                let (input, _) = state.read(cx);
                v_flex()
                    .w_full()
                    .gap_1()
                    .child(Self::label_block(row, cx))
                    .child(Input::new(input).w_full())
                    .into_any_element()
            }
            RowKind::Info { value } => h_flex()
                .w_full()
                .items_center()
                .gap_3()
                .child(div().text_sm().flex_1().min_w_0().child(row.title.clone()))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .min_w_0()
                        .child(value.clone()),
                )
                .into_any_element(),
            RowKind::Custom { title, render } => {
                let content = render(cx);
                match title {
                    Some(title) => v_flex()
                        .w_full()
                        .gap_1()
                        .child(div().text_sm().child(title.clone()))
                        .child(content)
                        .into_any_element(),
                    None => content,
                }
            }
        }
    }

    /// 内容区的通用外壳：可滚动 + 有边距。
    fn scrollable(body: impl IntoElement) -> AnyElement {
        div()
            .id("settings-compact-scroll")
            .overflow_y_scroll()
            .flex_1()
            .min_h_0()
            .px(px(GUTTER))
            .pb(px(GUTTER))
            .child(body)
            .into_any_element()
    }
}

impl Render for CompactSettings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 宿主测试按这一格确认「我的」那一个目的地画的是钻入式列表（同 `SettingsView`）。
        let mut root = v_flex().id("settings-compact").size_full().test_support();
        match self.screen {
            Screen::Root => root = root.child(self.root_screen(cx)),
            Screen::Page(page_ix) => {
                let page = spec_pages(cx).swap_remove(page_ix);
                root = root
                    .child(self.header(&page.title, cx))
                    .child(hairline(cx))
                    .child(self.page_screen(page_ix, &page, window, cx));
            }
        }
        root
    }
}
