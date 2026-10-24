//! Compact 档（手机）的设置：与桌面**同一份页面规格**，版式按 Apple 的分组列表。
//!
//! 桌面的 `Settings` 组件是「侧栏 + 正文」两栏，塞不进一块 360dp 的小屏；这里吃
//! 同一份 [`spec_pages`]，渲染成「页列表 → 全屏子页 → 选项子页」三层：
//!
//! - 根屏一张内嵌分组，一行一页，末尾只留一枚进去的箭头。**顶上不再摆一个大标题**：这一屏
//!   在底栏里就叫「我的」，再写一句「设置」是同一个入口的两个名字。右端也**不挂这一页的
//!   当前值**：入口列表那一行管的是「去哪儿」，不是「现在是多少」，而一页几十行里只能挑一个
//!   数当代表（「文字」那页从前露字号、看不到行距），念不准还看着像已经调过了；
//! - 子页顶部一条 [`TopBar`]（返回 + 页名），下面按规格里的组各成一张分组卡，**卡上方不摆
//!   节头**：组与组只靠 32 那一道缝分开，整页是一串光卡。钻进子页（以及它的选项屏）时**整窗
//!   都是它**：底下那条应用导航栏由外壳收起，判据是 [`CompactSettings::pushed`]，退回根屏
//!   那一趟它自己回来；
//! - **行内不摆灰色说明**：每行只有标题与控件，44 一行。规格里那句「这一格干什么用、取值范围
//!   是什么意思」（[`spec::Row::desc`]）只有桌面渲染 —— 代价是「最大行长 0 表示随窗口铺满」
//!   这类约定在这一屏没有界面内的解释，靠各控件自己的语义兜住。服务状态字（Wi-Fi 传书没开时
//!   那句、检查更新的「还没查过」）不受这一条影响：那是**行的内容**而不是注释，走
//!   [`RowKind::Custom`]，灰色照旧；
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
//! | 顶栏到第一张卡 | `mt_2`（8） | Material 的列表贴着顶栏那一档；分隔线去掉后由这段留白换段 |
//! | 正文 17（HIG body） | `text_base`（16） | 字号阶是 12/14/16/18/20 |
//! | 分组卡圆角 10（观测） | `theme.radius_lg`（8） | 规范：圆角只从主题那几档里挑 |
//! | 分组卡那圈描边 | `border_1` + `theme.border` | 规范：区域靠背景与分隔线定，不靠填色分层 |
//! | 开关 51×31（UIKit 固定尺寸） | `Switch::large()`（44×24） | 规范：保组件家族的几何，不手搓外框 |
//!
//! 颜色一支都不定义：整屏一层表面（`background`）加卡片那一圈 `border`，取的都是主题的角色名，
//! 这个应用不往界面里塞色值（`ui/shared/theme.rs` 那条规矩）。

use gpui_kit::Anchor;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Div, InteractiveElement as _,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement as _, Styled,
    Subscription, TestSupportExt as _, Window, div, px, rems,
};

use super::SettingsPage;
use super::spec::{self, RowKind};
use super::spec_pages;
use crate::ui::components::TopBar;
use crate::ui::services::l10n::tr;
use crate::ui::services::storage::{self, Job};

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

/// 一张分组卡：圆角取主题那几档里最大的 `radius_lg`，外框由卡这一层画、卡内的行不再画边。
///
/// **卡片靠描边立住，不靠填色**：整屏只有一层表面（`theme().background`），区域由分隔线与留白
/// 来定 —— 规范那两条：「先尺寸、字重、留白与分隔线，再考虑加颜色或容器」与「基面是平的，
/// 区域由分隔线与背景对比定义」。从前这里分两层：页面垫 `muted`、卡片吃 `background`，浅深
/// 两侧还得各判一次谁亮；而手机那一屏的底色比外壳涂在状态栏那一条 `safe.top` 下面的那一档
/// 深一档，屏幕顶就是两道色。
///
/// GPUI 不保证圆角的父容器裁掉孩子，所以这里显式裁：悬停与选中的底色长在行那一层，
/// 不裁就会在卡角外头露出方角（规范那条「一个圆角要管住整张可见表面」）。
fn card(rows: Vec<AnyElement>, cx: &App) -> AnyElement {
    v_flex()
        .w_full()
        .rounded(cx.theme().radius_lg)
        .overflow_hidden()
        .border_1()
        .border_color(cx.theme().border)
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

/// 标题。行左侧共用的那半边。
///
/// 只有标题一格：规格里那句次要说明（[`spec::Row::desc`]）归桌面，这一屏行内不摆灰色说明
/// （理由写在文件头那条定稿里）。
///
/// 返回具体的 `Div` 而不是 `impl IntoElement`：调用方还要往这块上接 `flex_1()` 与
/// `min_w_0()`，而 `Styled` 那一整套方法只长在具体类型上 —— 擦成 `impl IntoElement`
/// 之后调用方就再也加不了样式。
fn label_block(title: SharedString) -> Div {
    div().text_base().text_ellipsis().child(title)
}

/// 右端那一格「当前值」：与标题同字号、取次要文字色、右对齐（Apple 的 value1 就是
/// body 那一档）。
///
/// 只许占一行、放不下就省略号：这一格现在挂在子页那些「多选一」的行尾，选项标签有九门语言
/// 的版本，德语那一串完全可能长过剩下那点宽度。让它折行会把一行 44 撑成三行，整张卡的节奏
/// 就散了（Apple 那一格从不折行）。
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

/// 「进去还有一屏」那一类行：图标位（可空）+ 标题 + 可选的当前值 + 一枚箭头。
///
/// 右端没有值就只留箭头：根屏每一行都不挂值（那里的一行是「去哪儿」），而子页里「多选一」
/// 那一行要挂 —— 那一行的当前值就是它自己，点进去之前读者得先看得见现在用的是哪一档。
fn push_cell(
    id: SharedString,
    icon: Option<IconName>,
    title: SharedString,
    value: Option<SharedString>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let name = title.clone();
    let mut parts: Vec<AnyElement> = Vec::new();
    if let Some(icon) = icon {
        parts.push(icon_slot(icon, cx).into_any_element());
    }
    parts.push(label_block(title).flex_1().min_w_0().into_any_element());
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
    let mut parts: Vec<AnyElement> =
        vec![label_block(title).flex_1().min_w_0().into_any_element()];
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
    /// 对「存储」那一屏读数实体的订阅。**必须是个字段**：gpui 的 `Subscription` 一掉就退订
    /// （它的 `Drop` 直接调 `unsubscribe`），绑成局部变量等于没订阅 —— 少了它，后台扫完那一趟
    /// 永远不会让这一屏重画，读者看到的是一句「还没统计」。
    _storage_watch: Subscription,
    /// 对「检查更新」状态实体的订阅，同上一条的理由：GitHub 那次往返在后台线程上结束，
    /// 「正在检查…」要变成结果只能靠实体 `notify` 唤醒这一屏。手机上那一屏停住的样子
    /// 与桌面完全一样，所以两条订阅一处都不能只写一半。
    _update_watch: Subscription,
    /// 对「反馈日志」状态实体的订阅。同上一条：读盘与拼快照在后台线程上跑，
    /// 那一格要从「正在整理日志…」变成结果，只能靠实体 notify 唤醒这一屏。
    _feedback_watch: Subscription,
}

impl CompactSettings {
    /// 由外壳（「我的」目的地）与阅读页各建一屏，所以是 `pub(crate)` 而不是 `pub(in crate::ui::features::settings)`。
    ///
    /// 这里要 `cx` 只为一件事：**观察存储那一屏的读数实体**。「存储」那一页的每一格是现读
    /// 实体（见 `pages/storage.rs` 的模块说明），而静态设置页自己拿不住 `Task` —— 后台扫完
    /// 那一趟只能靠实体 `notify`、这一屏跟着重画，读者才看见新的数。句柄在这里就地取（没有
    /// 就建），所以不必等外壳先装过；`_storage_watch` 这个空绑定是**要留着的**：句柄掉了，
    /// 订阅就断了，那一屏会永远停在旧账上。
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let storage = storage::entity(cx);
        let watch = cx.observe(&storage, |_, _, cx| cx.notify());
        let update = crate::ui::services::update::entity(cx);
        let update_watch = cx.observe(&update, |_, _, cx| cx.notify());
        let feedback = crate::ui::services::feedback::entity(cx);
        let feedback_watch = cx.observe(&feedback, |_, _, cx| cx.notify());
        Self {
            screen: Screen::Root,
            _storage_watch: watch,
            _update_watch: update_watch,
            _feedback_watch: feedback_watch,
        }
    }

    fn open(&mut self, page_ix: usize, cx: &mut Context<Self>) {
        self.screen = Screen::Page(page_ix);
        // 钻进「存储」那一页就是起一次统计的时刻：那一屏的数只能是**现在**的数，而静态
        // 设置页没有一个「进来查一眼」的地方可以放这件事。统计在飞时这一句是空转
        // （[`storage::request`] 那头拦），所以反复进出同一页不会叠出第二条线。
        if page_ix == SettingsPage::Storage.index() {
            storage::request(cx, Job::Rescan);
        }
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

    /// 这一屏是不是**占满整窗**：钻进子页或选项屏之后，底下那条应用导航栏该整条收起，
    /// 读者看到的是「换了一屏」，不是「同一屏的下半段换了内容」。
    ///
    /// 判据只有这一句，画不画那条栏归外壳（[`crate::ui::shell`]）—— 栏是外壳的 chrome，
    /// 而「现在站着哪一屏」是这一屏自己的状态。这两格之间靠一条 `observe` 接上：本屏每一处
    /// 改 `screen` 都敲了 `cx.notify()`，外壳收到就把栏重画一次，所以这里不用另发一种事件。
    /// 根屏返回 `false`：那一屏是「我的」这一个目的地，栏该在。
    pub(crate) fn pushed(&self) -> bool {
        !matches!(self.screen, Screen::Root)
    }

    /// 外壳测试用的钻入入口：`open` 是本模块私有的，而「栏收不收」那条判据要钉的是外壳
    /// 那一头（宿主到不了 Compact 档的渲染，只能问判据），所以留这一个口子。
    #[cfg(test)]
    pub(crate) fn drill_into_for_test(&mut self, page_ix: usize, cx: &mut Context<Self>) -> bool {
        self.open(page_ix, cx);
        self.pushed()
    }

    /// 根屏：大标题 + 一张分组卡，一行一页，右端只留那枚进去的箭头。
    fn root_screen(&self, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity().clone();
        let pages = spec_pages(cx);
        let mut rows: Vec<AnyElement> = Vec::new();
        for (ix, page) in pages.iter().enumerate() {
            if ix > 0 {
                rows.push(separator(3.5, cx).into_any_element());
            }
            let entity = entity.clone();
            rows.push(push_cell(
                SharedString::from(format!("settings-page-{ix}")),
                Some(page.icon),
                page.title.clone(),
                None,
                move |_, _, app| entity.update(app, |this, cx| this.open(ix, cx)),
                cx,
            ));
        }
        // 大标题不摆了：这一屏在底部导航里就叫「我的」，顶上再写一句「设置」是同一个入口
        // 的两个名字，读者先看见的是那句和底栏对不上的字。卡片自己就是这一屏。
        v_flex()
            .w_full()
            .child(div().w_full().px_4().mt_2().child(card(rows, cx)))
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

    /// 子页正文：规格里的每个组各成一张卡，卡上方不摆节头 —— 整页是一串光卡。
    ///
    /// 组标题在这一屏**不被读**：它归桌面的侧栏当子项（见 `view/mod.rs` 的 `setting_page`），
    /// 而这边一行小灰字换成 32 的缝之后，「换了一块」由留白自己说。
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
            // 第一张卡与顶栏只留 8（Material 的列表贴着顶栏的那一档），组与组之间仍是 32
            // 那一道缝。分隔线去掉以后，这两段留白就是「换了一块」唯一说话的地方。
            let gap = if group_ix == 0 {
                v_flex().w_full().mt_2()
            } else {
                v_flex().w_full().mt_8()
            };
            body = body.child(gap.child(div().w_full().px_4().child(card(rows, cx))));
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
        Self::scrollable(div().w_full().mt_2().px_4().child(card(rows, cx)))
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
                            label_block(row.title.clone())
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
                            label_block(row.title.clone())
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
                            label_block(row.title.clone())
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
                        .child(label_block(row.title.clone()))
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
        // 底色与外壳涂在状态栏那一条 `safe.top` 下面的那一档**同一个角色**：这一屏比它深一档
        // 时，屏幕顶就是一道色（理由写在 `card` 那段）。
        let mut root = v_flex()
            .id("settings-compact")
            .size_full()
            .bg(cx.theme().background)
            .test_support();
        match self.screen {
            Screen::Root => root = root.child(self.root_screen(cx)),
            Screen::Page(page_ix) => {
                let page = spec_pages(cx).swap_remove(page_ix);
                // 顶栏底下**不画分隔线**：Material 3 的顶栏不带 divider，容器色一滚起来才变。
                // 线与它下面那一段留白说的是同一件事（「这里换了一块」），留白已经在，那条
                // 0.5 就把一屏切成两截 —— 卡片自己那圈描边是这一屏唯一的边界。
                root = root
                    .child(self.header(&page.title, cx))
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
                    .child(self.pick_screen(page_ix, group, row, &page, cx));
            }
        }
        root
    }
}

#[cfg(test)]
mod tests {
    use super::{CompactSettings, Screen};
    use gpui_kit::{AppContext as _, Entity, TestAppContext};

    /// 返回键一层一层往外退：选项屏退回它来的那一页，那一页退回根屏，根屏那一次让给
    /// 外壳（最终是退出应用）。
    ///
    /// 这一屏的「页」是内部状态、不是导航栈，所以退出层级没有别处兜着：把选项屏的返回
    /// 写成直接回根屏，读者的感觉是「按一下就全没了」，而测试这里当场红。
    #[gpui_kit::test]
    async fn back_steps_out_one_layer_at_a_time(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let view: Entity<CompactSettings> = cx.update(|cx| cx.new(CompactSettings::new));
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

    /// 钻进展屏 = 那一屏要吃满整窗。这一句要报给外壳，因为底栏归外壳画。
    ///
    /// 钉的是「根屏不报、进了详情就报、退回根屏又不报」这一整条判据，不是某一帧的画面：
    /// 底栏只长在真机上（`SizeClass::of` 在桌面把窄窗口夹到 `Medium`），宿主测试进不到那一屏。
    /// 恒写成 `true` 会让「我的」那一格从此看不见底栏，恒写成 `false` 会让读者钻进详情时底下
    /// 那条栏还占着 56 —— 两个方向这条都红。
    #[gpui_kit::test]
    async fn the_drilled_screen_reports_that_it_occupies_the_window(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let view: Entity<CompactSettings> = cx.update(|cx| cx.new(CompactSettings::new));
        let pushed = |cx: &mut TestAppContext| cx.read(|cx| view.read(cx).pushed());
        let back =
            |cx: &mut TestAppContext| cx.update(|cx| view.update(cx, |this, cx| this.go_back(cx)));

        assert!(!pushed(cx), "根屏就是「我的」这一格，底栏该在");
        cx.update(|cx| view.update(cx, |this, cx| this.open(2, cx)));
        assert!(pushed(cx), "钻进某一页要吃满整窗");

        cx.update(|cx| {
            view.update(cx, |this, _| {
                this.screen = Screen::Pick {
                    page: 2,
                    group: 0,
                    row: 0,
                };
            })
        });
        assert!(pushed(cx), "选项屏同样是整屏");

        assert!(back(cx), "选项屏往外弹一层");
        assert!(pushed(cx), "退回的是那一页，还在详情里，底栏不该当场回来");
        assert!(back(cx), "那一页再往外弹一层");
        assert!(!pushed(cx), "回到根屏，底栏跟着回来");
    }
}
