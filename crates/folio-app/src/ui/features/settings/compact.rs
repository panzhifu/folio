//! Compact 档（手机）的设置：与桌面**同一份页面规格**，版式按 Apple 的分组列表。
//!
//! 桌面的 `Settings` 组件是「侧栏 + 正文」两栏，塞不进一块 360dp 的小屏；这里吃
//! 同一份 [`spec_pages`]，渲染成「页列表 → 全屏子页 → 选项子页」三层：
//!
//! - 根屏一个大标题 + 一张内嵌分组，一行一页，右端挂这一页的**当前值**（值从那一行
//!   自己的 getter 取，见 [`page_summary`]），末尾一枚进去的箭头；
//! - 子页顶部一条 [`TopBar`]（返回 + 页名），下面按规格里的组各成一张分组卡，组名当节头，
//!   节头下面才摆行；
//! - 多选一（浅深、翻页方式、排列密度…）**推一屏勾选**，不再开内联下拉：七项的下拉在
//!   393dp 那块屏上要盖掉半屏，而推入式的每一行都能整行命中 44；
//! - 开关用 `Switch`（受控值：回调给的就是要写进去的新值），数值用 −/+ 步进行：拖动滑条
//!   要跨帧持有状态，步进无状态、在什么屏上都稳，范围与步长直接取自规格；
//! - 文本用行内输入框，状态按 (页, 组, 行) 键控持有 —— 框架自己的 Input 字段就是这么干的
//!   （见 `setting/fields/string.rs`），输入到一半的重画不会把状态扔掉。
//!
//! 返回键的契约与曾经的 `AccountView` 相同：[`CompactSettings::go_back`] 往外弹一层，
//! 根屏返回 `false` 让外壳接着决定（最终是退出应用）。
//!
//! ## 版式按 Apple，数为什么不是一堆 `px`
//!
//! 〔GPUI Kit Design Guides → Zoom, base font, and `rem`〕那条规矩是：产品布局不该直接
//! 写 `px`，例外只有「一个物理像素的发丝线」「最小命中容差」「必须对齐外部表面的几何」
//! 这几类。所以这里只有两处 `px`（[`ROW_HIT`] 与发丝线的 `0.5`），其余全部落在 rem 阶与
//! 主题角色上，跟着界面缩放一起动：
//!
//! | Apple 那一格 | 这里用的 | 为什么不是原数 |
//! | --- | --- | --- |
//! | 行的可命中高 44×44（HIG 明文） | [`ROW_HIT`]，`px` | 命中容差是规范点名的 `px` 例外 |
//! | 横向内缩 16（UIKit 观测） | `px_4`（1rem） | 正好同值，走 rem |
//! | 图标槽 29、图标到文字 12（观测） | `size_7`（28）、`gap_3`（12） | 29 不在阶上，取 28 |
//! | 组与组的缝 35（观测） | `mt_8`（32） | 35 不在阶上，取最近那一档 |
//! | 正文 17（HIG body） | `text_base`（16） | 字号阶是 12/14/16/18/20 |
//! | 大标题 34（HIG large title） | `text_3xl`（30）加粗 | 阶上最大就这一档 |
//! | 节头与尾注 13（HIG footnote） | `text_sm`（14） | 同上，取最近那一档 |
//! | 分组卡圆角 10（观测） | `theme.radius_lg`（8） | 规范：圆角只从主题那几档里挑 |
//! | 开关 51×31（UIKit 固定尺寸） | `Switch::large()`（44×24） | 规范：保组件家族的几何，不手搓外框 |
//!
//! 颜色一支都不定义：两层表面只取主题的角色名（见 [`surfaces`]），这个应用不往界面里塞
//! 色值（`ui/shared/theme.rs` 那条规矩）。

use gpui_kit::Anchor;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Icon, Sizable as _, ThemeMode, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Div, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement as _, Styled, TestSupportExt as _, Window, div, px, rems,
};

use super::spec::{self, RowKind, page_summary};
use super::spec_pages;
use crate::ui::services::l10n::tr;
use crate::ui::components::{TopBar, hairline};

/// 一行的可命中高度。Apple HIG 明文那条 44×44；规范点名的 `px` 例外之一就是
/// 「最小命中容差」，所以这一格不跟着 rem 走。
const ROW_HIT: f32 = 44.0;

/// 分组卡里那条发丝线：一个物理像素（也是规范点名的 `px` 例外），颜色取 `border`。
///
/// 线左端缩进到标题文字那一列，用的是 rem：不带图标 1rem（16），带图标
/// 1rem + 1.75rem + 0.75rem（16+28+12=56）。基准字号一放大标题就往右移，线跟着移，
/// 把 56 写死成 `px` 就错开了。
fn separator(rem: f32, cx: &App) -> impl IntoElement {
    div()
        .w_full()
        .h(px(0.5))
        .ml(rems(rem))
        .bg(cx.theme().border)
}

/// 分组列表的两层表面：Apple 那一对的关系是**卡片比页面亮一档**。
///
/// 浅色里 `background` 是白、`muted` 是灰，所以页面垫 `muted`、卡片是 `background`；
/// 深色正好反过来，最低那一档是 `background`，`muted` 是抬起来的那一层。同一个方向要在
/// 两套外观里都成立，才需要按 `mode` 分这一刀。这条不靠注释自证：测试
/// `the_grouped_surfaces_stay_elevated_in_both_appearances` 现算两套外观的相对亮度。
fn surfaces(cx: &App) -> (Hsla, Hsla) {
    let theme = cx.theme();
    match theme.mode {
        ThemeMode::Light => (theme.muted, theme.background),
        ThemeMode::Dark => (theme.background, theme.muted),
    }
}

/// 一张分组卡：圆角取主题那几档里最大的 `radius_lg`，卡内的行自己不再画边。
///
/// GPUI 不保证圆角的父容器裁掉孩子，所以这里显式裁：悬停与选中的底色长在行那一层，
/// 不裁就会在卡角外头露出方角（规范那条「一个圆角要管住整张可见表面」）。
fn card(rows: Vec<AnyElement>, cx: &App) -> AnyElement {
    v_flex()
        .w_full()
        .rounded(cx.theme().radius_lg)
        .overflow_hidden()
        .bg(surfaces(cx).1)
        .children(rows)
        .into_any_element()
}

/// 图标那一列：28 的槽装一枚小图标。槽不吃点击，整行才是命中面。
fn icon_slot(icon: IconName, cx: &App) -> impl IntoElement {
    div()
        .size_7()
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .text_color(cx.theme().foreground)
        .child(Icon::new(icon).small())
}

/// 标题 + 可选的次要说明。行左侧共用的那半边。
///
/// 返回具体的 `Div` 而不是 `impl IntoElement`：调用方还要往这块上接 `flex_1()` 与
/// `min_w_0()`，而 `Styled` 那一整套方法只长在具体类型上 —— 擦成 `impl IntoElement`
/// 之后调用方就再也加不了样式。
fn label_block(title: SharedString, desc: Option<SharedString>, cx: &App) -> Div {
    let mut col = v_flex()
        .gap_0p5()
        .child(div().text_base().text_ellipsis().child(title));
    if let Some(desc) = desc {
        col = col.child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(desc),
        );
    }
    col
}

/// 右端那一格「当前值」：与标题同字号、取次要文字色、右对齐（Apple 的 value1 就是
/// body 那一档）。
///
/// 只许占一行、放不下就省略号 —— 真机上「书库」那一格装的是 `/storage/emulated/0/folio`
/// 这么一串，让它折行会把一行 44 撑成三行，整张卡的节奏就散了（Apple 那一格从不折行）。
fn value_text(value: SharedString, cx: &App) -> impl IntoElement {
    div()
        .min_w_0()
        .flex_1()
        .text_base()
        .text_color(cx.theme().muted_foreground)
        .text_right()
        .text_ellipsis()
        .child(value)
}

/// 一格可点的行：把已经排好的那几块装进 `ListItem`，命中高一律 [`ROW_HIT`]。
///
/// 无障碍名称只给标题，不给说明与值 —— 读屏走到这一行该念的是「翻页方式」，
/// 而不是把整行的字串一遍。所以 `name` 由调用方单独给，不从 `parts` 里凑。
fn list_row(
    id: SharedString,
    name: SharedString,
    parts: Vec<AnyElement>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    ListItem::new(id)
        .w_full()
        .min_h(px(ROW_HIT))
        .px_4()
        .child(h_flex().w_full().items_center().gap_3().children(parts))
        .accessibility_label(name)
        .on_click(on_click)
        .into_any_element()
}

/// 「进去还有一屏」那一类行：图标位（可空）+ 标题（+说明）+ 右端的当前值 + 一枚箭头。
///
/// 右端没有值（「关于」这类纯推入的页）就只留箭头 —— Apple 的 push 行本来就不挂值。
fn push_cell(
    id: SharedString,
    icon: Option<IconName>,
    title: SharedString,
    desc: Option<SharedString>,
    value: Option<SharedString>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let name = title.clone();
    let mut parts: Vec<AnyElement> = Vec::new();
    if let Some(icon) = icon {
        parts.push(icon_slot(icon, cx).into_any_element());
    }
    parts.push(
        label_block(title, desc, cx)
            .flex_1()
            .min_w_0()
            .into_any_element(),
    );
    if let Some(value) = value {
        parts.push(value_text(value, cx).into_any_element());
    }
    parts.push(
        Icon::new(IconName::ChevronRight)
            .small()
            .text_color(cx.theme().muted_foreground)
            .into_any_element(),
    );
    list_row(id, name, parts, on_click)
}

/// 选项屏里的一行：标题 + 当前那项右侧一枚勾。勾是这一屏唯一的装饰，不再叠 radio 圆点
/// —— 一行只能有一个右端标记，否则勾和箭头打架。
fn choice_cell(
    id: SharedString,
    title: SharedString,
    checked: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let name = title.clone();
    let mut parts: Vec<AnyElement> = vec![
        label_block(title, None, cx)
            .flex_1()
            .min_w_0()
            .into_any_element(),
    ];
    if checked {
        parts.push(
            Icon::new(IconName::Check)
                .small()
                .text_color(cx.theme().primary)
                .into_any_element(),
        );
    }
    list_row(id, name, parts, on_click)
}

/// 一行只摆控件、整行不可点（开关、步进、输入框那类）。底色与内缩和 [`push_cell`] 同一套，
/// 命中面归控件本身 —— 给整行加点击会让「按哪里都算」变成「按哪里都可能改」。
fn control_cell(id: SharedString, inner: impl IntoElement) -> AnyElement {
    div()
        .id(id)
        .w_full()
        .min_h(px(ROW_HIT))
        .px_4()
        .py_2()
        .flex()
        .items_center()
        .child(inner)
        .into_any_element()
}

/// 「我的」这一格里现在站着哪一屏。页用**序号**寻址 —— 与桌面深链同一条
/// `SettingsPage::index` 顺序，规格数组的下标就是它。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
    /// 入口列表。
    Root,
    Page(usize),
    /// 某个「多选一」字段推出来的选项屏。三元组是 (页, 组, 行)，用的还是规格数组的下标，
    /// 不另存一份选项表：值与写回都从那一行现取。
    Pick {
        page: usize,
        group: usize,
        row: usize,
    },
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
    /// 自己吞掉返回。选项屏退回它来的那一页，而不是直接跳回根屏。
    pub(crate) fn go_back(&mut self, cx: &mut Context<Self>) -> bool {
        let next = match self.screen {
            Screen::Root => return false,
            Screen::Page(_) => Screen::Root,
            Screen::Pick { page, .. } => Screen::Page(page),
        };
        self.screen = next;
        cx.notify();
        true
    }

    /// 根屏：大标题 + 一张分组卡，一行一页，右端挂这一页的当前值。
    fn root_screen(&self, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity().clone();
        let pages = spec_pages(cx);
        let mut rows: Vec<AnyElement> = Vec::new();
        for (ix, page) in pages.iter().enumerate() {
            if ix > 0 {
                rows.push(separator(3.5, cx).into_any_element());
            }
            let value = page_summary(page, cx);
            let entity = entity.clone();
            rows.push(push_cell(
                SharedString::from(format!("settings-page-{ix}")),
                Some(page.icon),
                page.title.clone(),
                None,
                value,
                move |_, _, app| entity.update(app, |this, cx| this.open(ix, cx)),
                cx,
            ));
        }
        v_flex()
            .w_full()
            .child(
                div()
                    .w_full()
                    .px_4()
                    .pt_6()
                    .pb_2()
                    .text_3xl()
                    .font_weight(FontWeight::BOLD)
                    .child(tr(cx, "account.settings")),
            )
            .child(div().w_full().px_4().mt_8().child(card(rows, cx)))
            .into_any_element()
    }

    /// 子页顶部那一条：返回 + 标题，与阅读页顶栏、书信息页同一条 [`TopBar`]。
    ///
    /// Apple 那一套里推入页的标题是**居中**的，这里没有给设置单做一条居中版：同族几何
    /// 只该有一份，要居中该改 [`TopBar`] 本身（那会同时动到阅读层与书信息页）。
    /// 返回那格也不再挂「返回」二字 —— 44 的箭头自己就说清了去处。
    fn header(&self, title: &SharedString, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().clone();
        TopBar::new("settings-bar")
            .title(title.clone())
            .strong()
            .pad_x(16.0)
            .back_tooltip(tr(cx, "shelf.back"))
            .on_back(move |_, cx| {
                entity.update(cx, |this, cx| {
                    this.screen = Screen::Root;
                    cx.notify();
                });
            })
    }

    /// 选项屏那一条顶栏：标题是**字段名**（「翻页方式」），退回它来的那一页。
    fn pick_header(
        &self,
        title: &SharedString,
        page_ix: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let entity = cx.entity().clone();
        TopBar::new("settings-pick-bar")
            .title(title.clone())
            .strong()
            .pad_x(16.0)
            .back_tooltip(tr(cx, "shelf.back"))
            .on_back(move |_, cx| {
                entity.update(cx, |this, cx| {
                    this.screen = Screen::Page(page_ix);
                    cx.notify();
                });
            })
    }

    /// 子页正文：规格里的每个组各成一节 —— 节头在上，分组卡在下。
    fn page_screen(
        &self,
        page_ix: usize,
        page: &spec::Page,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut body = v_flex().w_full();
        for (group_ix, group) in page.groups.iter().enumerate() {
            let mut rows: Vec<AnyElement> = Vec::new();
            for (row_ix, row) in group.rows.iter().enumerate() {
                if row_ix > 0 {
                    rows.push(separator(1.0, cx).into_any_element());
                }
                rows.push(self.row(page_ix, group_ix, row_ix, row, window, cx));
            }
            body = body.child(
                v_flex()
                    .w_full()
                    .mt_8()
                    .child(
                        div()
                            .w_full()
                            .px_4()
                            .pb_1()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(group.title.clone()),
                    )
                    .child(div().w_full().px_4().child(card(rows, cx))),
            );
        }
        Self::scrollable(body)
    }

    /// 选项屏：一项一行，当前那项右侧一枚勾。选中即写回并退回上一屏，没有「确定」按钮。
    fn pick_screen(
        &self,
        page_ix: usize,
        group_ix: usize,
        row_ix: usize,
        page: &spec::Page,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let RowKind::Dropdown {
            options, get, set, ..
        } = &page.groups[group_ix].rows[row_ix].kind
        else {
            // 只有「多选一」那一行会推到这一屏。类型对不上是调用方写错了，这里摆空屏
            // 而不是 panic：这一屏在读者眼前，崩了比空着更难查。
            return Self::scrollable(div().w_full());
        };
        let current = get(cx);
        let entity = cx.entity().clone();
        let set = set.clone();
        let mut rows: Vec<AnyElement> = Vec::new();
        for (ix, (value, label)) in options.iter().enumerate() {
            if ix > 0 {
                rows.push(separator(1.0, cx).into_any_element());
            }
            let checked = *value == current;
            let value = value.clone();
            let entity = entity.clone();
            let set = set.clone();
            rows.push(choice_cell(
                SharedString::from(format!("settings-pick-{ix}")),
                label.clone(),
                checked,
                move |_, _, app| {
                    set(value.clone(), app);
                    entity.update(app, |this, cx| {
                        this.screen = Screen::Page(page_ix);
                        cx.notify();
                    });
                },
                cx,
            ));
        }
        Self::scrollable(div().w_full().mt_8().px_4().child(card(rows, cx)))
    }

    /// 一个字段行。控件的种类决定右端摆什么（或整行摆什么）。
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
                control_cell(
                    id("toggle-row"),
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_3()
                        .child(
                            label_block(row.title.clone(), row.desc.clone(), cx)
                                .flex_1()
                                .min_w_0(),
                        )
                        .child(
                            // `large` 那一档是组件家族给拇指准备的框（44×24），字号与内缩
                            // 跟着一起放大；不手搓一个只改外框的尺寸。
                            Switch::new(id("toggle"))
                                .large()
                                .checked(get(cx))
                                .on_click(move |checked, _, app| set(*checked, app)),
                        ),
                )
            }
            RowKind::Dropdown {
                options,
                scrollable,
                push,
                get,
                set,
            } => {
                let current = get(cx);
                // 取值对不上任何一个选项值时显示裸值兜底，而不是留一个空位 ——
                // 与桌面那份映射同一条规矩。
                let label = options
                    .iter()
                    .find(|(value, _)| *value == current)
                    .map(|(_, label)| label.clone())
                    .unwrap_or_else(|| current.clone());
                let entity = cx.entity().clone();
                if *push {
                    // 项多或名字长的那几格（翻页方式、排列密度）推一屏勾选：整行 44 都是
                    // 命中区，浮层不会盖掉半屏。
                    return push_cell(
                        id("dropdown"),
                        None,
                        row.title.clone(),
                        row.desc.clone(),
                        Some(label),
                        move |_, _, app| {
                            entity.update(app, |this, cx| {
                                this.screen = Screen::Pick {
                                    page: page_ix,
                                    group: group_ix,
                                    row: row_ix,
                                };
                                cx.notify();
                            });
                        },
                        cx,
                    );
                }
                // 其余（浅深、主题、界面语言）仍是就地浮层：与桌面同一条路，
                // 而「浮层在 Android 上点得住」这条已经跟着行列表验过（`plans/facts.md`）。
                let (options, current, scrollable, set) =
                    (options.clone(), current, *scrollable, set.clone());
                control_cell(
                    id("dropdown-row"),
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_3()
                        .child(
                            label_block(row.title.clone(), row.desc.clone(), cx)
                                .flex_1()
                                .min_w_0(),
                        )
                        .child(
                            Button::new(id("dropdown-menu"))
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
                        ),
                )
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
                control_cell(
                    id("number-row"),
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_2()
                        .child(
                            label_block(row.title.clone(), row.desc.clone(), cx)
                                .flex_1()
                                .min_w_0(),
                        )
                        .child(div().min_w(px(40.0)).text_base().child(shown))
                        .child(
                            Button::new(id("number-minus"))
                                .ghost()
                                .large()
                                .icon(IconName::Minus)
                                .on_click(move |_, _, app| {
                                    set_minus((get_minus(app) - spec.step).max(spec.min), app)
                                }),
                        )
                        .child(
                            Button::new(id("number-plus"))
                                .ghost()
                                .large()
                                .icon(IconName::Plus)
                                .on_click(move |_, _, app| {
                                    set_plus((get_plus(app) + spec.step).min(spec.max), app)
                                }),
                        ),
                )
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
                control_cell(
                    id("text-row"),
                    v_flex()
                        .w_full()
                        .gap_1()
                        .child(label_block(row.title.clone(), row.desc.clone(), cx))
                        .child(Input::new(input).w_full()),
                )
            }
            RowKind::Info { value } => control_cell(
                id("info-row"),
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_base()
                            .flex_1()
                            .min_w_0()
                            .child(row.title.clone()),
                    )
                    .child(
                        div()
                            .text_base()
                            .text_color(cx.theme().muted_foreground)
                            .min_w_0()
                            .child(value.clone()),
                    ),
            ),
            RowKind::Custom { title, render } => {
                let content = render(cx);
                let title = title.clone();
                control_cell(
                    id("custom-row"),
                    match title {
                        Some(title) => v_flex()
                            .w_full()
                            .gap_1()
                            .child(div().text_base().child(title))
                            .child(content)
                            .into_any_element(),
                        None => content,
                    },
                )
            }
        }
    }

    /// 内容区的通用外壳：可滚动。左右内缩归各屏自己（分组卡要贴着 16 那一档铺）。
    fn scrollable(body: impl IntoElement) -> AnyElement {
        div()
            .id("settings-compact-scroll")
            .overflow_y_scroll()
            .flex_1()
            .min_h_0()
            .w_full()
            .pb_8()
            .child(body)
            .into_any_element()
    }
}

impl Render for CompactSettings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 宿主测试按这一格确认「我的」那一个目的地画的是分组列表（同 `SettingsView`）。
        let mut root = v_flex()
            .id("settings-compact")
            .size_full()
            .bg(surfaces(cx).0)
            .test_support();
        match self.screen {
            Screen::Root => root = root.child(self.root_screen(cx)),
            Screen::Page(page_ix) => {
                let page = spec_pages(cx).swap_remove(page_ix);
                root = root
                    .child(self.header(&page.title, cx))
                    .child(hairline(cx))
                    .child(self.page_screen(page_ix, &page, window, cx));
            }
            Screen::Pick {
                page: page_ix,
                group,
                row,
            } => {
                let page = spec_pages(cx).swap_remove(page_ix);
                let title = page.groups[group].rows[row].title.clone();
                root = root
                    .child(self.pick_header(&title, page_ix, cx))
                    .child(hairline(cx))
                    .child(self.pick_screen(page_ix, group, row, &page, cx));
            }
        }
        root
    }
}

#[cfg(test)]
mod tests {
    use super::{CompactSettings, Hsla, Screen, ThemeMode, surfaces};
    use gpui_kit::component::Theme;
    use gpui_kit::{AppContext as _, Entity, TestAppContext};

    /// 返回键一层一层往外退：选项屏退回它来的那一页，那一页退回根屏，根屏那一次让给
    /// 外壳（最终是退出应用）。
    ///
    /// 这一屏的「页」是内部状态、不是导航栈，所以退出层级没有别处兜着：把选项屏的返回
    /// 写成直接回根屏，读者的感觉是「按一下就全没了」，而测试这里当场红。
    #[gpui_kit::test]
    async fn back_steps_out_one_layer_at_a_time(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let view: Entity<CompactSettings> = cx.update(|cx| cx.new(|_| CompactSettings::new()));
        let press =
            |cx: &mut TestAppContext| cx.update(|cx| view.update(cx, |this, cx| this.go_back(cx)));
        let shown = |cx: &mut TestAppContext| cx.read(|cx| view.read(cx).screen);

        cx.update(|cx| {
            view.update(cx, |this, _| {
                this.screen = Screen::Pick {
                    page: 2,
                    group: 1,
                    row: 0,
                };
            })
        });
        assert!(press(cx), "选项屏上按返回该被这一屏吃掉");
        assert_eq!(shown(cx), Screen::Page(2), "退回的是它来的那一页");
        assert!(press(cx), "子屏上按返回也该被吃掉");
        assert_eq!(shown(cx), Screen::Root);
        assert!(!press(cx), "已经在根屏就不该吞下返回，让外壳接着决定");
        assert_eq!(shown(cx), Screen::Root);
    }

    /// 两层表面那一对的方向：卡片必须比页面亮一档，浅深两套外观都得成立。
    ///
    /// 这条不靠注释说「我按 Apple 分了层」：把两支角色的相对亮度现算一遍比大小。谁把
    /// [`super::surfaces`] 指反（深色里让页面去吃 `muted`）当场红。判顺序不判比值 ——
    /// 那两层灰本来就过不了非文字对比 3.0 那条门槛，那条管的是控件边界。
    #[gpui_kit::test]
    async fn the_grouped_surfaces_stay_elevated_in_both_appearances(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(crate::ui::foundation::theme::register_builtin_themes);
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            cx.update(|app| {
                Theme::change(mode, None, app);
                let (page, card) = relative_luminance_pair(app);
                assert!(
                    card > page,
                    "{mode:?} 下两层表面翻反了：页面 {page:.4}，卡片 {card:.4}"
                );
            });
        }
    }

    /// 把 [`super::surfaces`] 那两支角色读出来并算成相对亮度。
    ///
    /// 公式是 WCAG 那一条（先线性化再加权和），与线框板「视觉闸」那一屏同一个口径，
    /// 两处不该各有一套数。`Hsla` 给的是 h/s/l，所以先解回 r/g/b。
    fn relative_luminance_pair(app: &gpui_kit::App) -> (f32, f32) {
        let (page, card) = surfaces(app);
        (luminance(page), luminance(card))
    }

    fn luminance(color: Hsla) -> f32 {
        let [r, g, b] = hsl_to_rgb(color);
        let linear = |v: f32| {
            if v <= 0.040_45 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
    }

    fn hsl_to_rgb(color: Hsla) -> [f32; 3] {
        let h = color.h.fract().mul_add(360.0, 0.0).rem_euclid(360.0);
        let s = color.s.clamp(0.0, 1.0);
        let l = color.l.clamp(0.0, 1.0);
        let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
        let x = c * (1.0 - ((h / 60.0).fract() * 2.0 - 1.0).abs());
        let m = l - c / 2.0;
        let (r, g, b) = match h {
            0.0..=60.0 => (c, x, 0.0),
            60.0..=120.0 => (x, c, 0.0),
            120.0..=180.0 => (0.0, c, x),
            180.0..=240.0 => (0.0, x, c),
            240.0..=300.0 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        [r + m, g + m, b + m]
    }
}
