//! Compact 档（手机）的设置：与桌面**同一份页面规格**，版式按 Apple 的分组列表。
//!
//! 桌面的 `Settings` 组件是「侧栏 + 正文」两栏，塞不进一块 360dp 的小屏；这里吃
//! 同一份 [`spec_pages`]，渲染成「页列表 → 全屏子页 → 选项子页」三层，外加根屏顶上那颗
//! 搜索框（框里有词时整屏换成结果那一屏）：
//!
//! - 根屏一张会员卡 + 三块带节头的入口 + 末尾一张无节头的卡，一行一页，末尾只留一枚进去的
//!   箭头；分了块这一屏就装不下，所以它跟着子页一起滚。**顶上不再摆一个大标题**：这一屏
//!   在底栏里就叫「我的」，再写一句「设置」是同一个入口的两个名字。右端也**不挂这一页的
//!   当前值**：入口列表那一行管的是「去哪儿」，不是「现在是多少」，而一页几十行里只能挑一个
//!   数当代表（「文字」那页从前露字号、看不到行距），念不准还看着像已经调过了；
//! - 子页顶部一条 [`TopBar`]（返回 + 页名），下面按规格里的组各成一张分组卡，**多于一组时
//!   卡上方摆一句节头**（就是规格里那句组标题，桌面侧栏早就在用它）：32 的缝只说得出「换了
//!   一块」，说不出这一块叫什么，而文字页四张卡、存储页两张卡的时候，读者得把每行标题读完才
//!   知道自己在哪一格。只有一组时不摆 —— 那句话和顶栏上的页名是同一个东西。钻进子页（以及
//!   它的选项屏）时**整窗都是它**：底下那条应用导航栏由外壳收起，判据是 [`CompactSettings::pushed`]，
//!   退回根屏那一趟它自己回来；
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
//! | 组与组的缝 35（观测） | 带节头 `mt_4`（16）；不带节头 `mt_8`（32） | 节头就是「换了一块」那句话，缝只需要说「这块不属于上一张卡」；只剩缝在说话时留大的一档 |
//! | 节头 13pt semibold、灰（UIKit 观测） | `text_sm`（14）+ 600 + `muted_foreground` | 字号阶是 12/14/16/18/20，取 14 那一档 |
//! | 节头到自己那张卡 8（观测） | `pb_2`（8） | 块内比块间小，「先认块、再认行」那条路才走得通 |
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
    AnyElement, App, AppContext as _, ClickEvent, Context, Div, Entity, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Subscription, TestSupportExt as _, Window, div, px,
    rems,
};

use std::rc::Rc;

use super::SettingsPage;
use super::pages::membership::status_line;
use super::spec::{self, RowKind};
use super::{NotesSheet, spec_pages};
use crate::ui::components::{FontPicker, SearchBox, SearchBoxEvent, TopBar, muted_label};
use crate::ui::services::l10n::tr;
use crate::ui::services::storage::{self, Job};
use crate::ui::shell::navigation;

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

/// 卡上方那一句节头：说出下面这一块里的行有什么共同点。
///
/// 这一句早就在规格里（[`spec::Group::title`]），只是从前只有桌面的侧栏读它（拿它当页的
/// 子项，见 `view/mod.rs` 的 `setting_page`），手机那一屏整页是「一串光卡」—— 32 的缝说得
/// 出「这里换了一块」，说不出这一块叫什么。文字页四张卡、存储页两张卡的时候，读者得把每行
/// 的标题读完才知道自己在哪一格。所以这里不新增文案，只把已有那一句摆回来。
///
/// 层次靠字级、字重与留白，不靠容器与颜色：`text_sm` 比正文小一档，`muted_foreground` 比
/// 正文淡一档，字重加一档。不做大写与小字距那一档 —— 九门语言里四门是 CJK，没有大小写可
/// 用，而 CJK 加字距只会散（规范「avoid excessive uppercase or letter spacing, especially
/// for CJK」）。
///
/// 左端与卡片同一条 16 的脊柱，由调用方那一层 `px_4` 给（`card` 自己也不带内缩）：节头与
/// 卡是同一层的兄弟，缩到行文字那一列的 56 会让它看起来像卡里的一行，而两处各垫一次 `px_4`
/// 就变成 32 —— 规范那条「已经垫过内缩的面板里别再垫一整份」。
fn section_header(title: SharedString, cx: &App) -> AnyElement {
    div()
        .w_full()
        .pb_2()
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(cx.theme().muted_foreground)
        .text_ellipsis()
        .child(title)
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
    let mut parts: Vec<AnyElement> = vec![label_block(title).flex_1().min_w_0().into_any_element()];
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

/// 根屏那条入口列表的分块：块名 + 落进这一块的页。
///
/// 为什么要块：[`SettingsPage::ALL`] 已经 14 格，从前除会员外全拍成一条平铺列表 —— 十三行
/// 同一个字号、同一种行、只靠发丝线分开，读者要找「行距」得把整列读完才知道它在第几格。
/// 分块把这条改成「先落到哪一类，再在那一类里找」，与规范那条「一个个看起来合理的控件凑在
/// 一起仍可能失败，因为它们没有形成一个阅读顺序」对得上。
///
/// 两块不在这里。**会员**在根顶单独一张卡（要当场念等级与到期日，见 [`root_screen`]）。
/// **「关于」**在末尾那张无节头的卡：一块只剩一行时，节头念的就是那一行自己的名字，那句是
/// 重复（规范「上下文已经建立的信息不要再讲一遍」）。
///
/// **没登记的页不会从屏上消失**：它落进末尾那张无节头的卡，同时
/// `every_page_lands_in_a_root_block` 那条用例红 —— 加一页忘了分块，症状是「它少一句块名」，
/// 不是「它不见了」。
const ROOT_BLOCKS: [(&str, &[SettingsPage]); 3] = [
    (
        "section.reading",
        &[
            SettingsPage::Appearance,
            SettingsPage::Typography,
            SettingsPage::Layout,
            SettingsPage::Fonts,
            SettingsPage::Speech,
        ],
    ),
    (
        "section.translation_ai",
        &[SettingsPage::Translation, SettingsPage::Ai],
    ),
    (
        "section.books_data",
        &[
            SettingsPage::Library,
            SettingsPage::TxtRules,
            SettingsPage::Toc,
            SettingsPage::Sync,
            SettingsPage::Storage,
        ],
    ),
];

/// 这一页落进根屏的哪一块；`None` = 末尾那张无节头的卡。
///
/// 按**页**查而不是按下标：块里写的是目的地，`ALL` 挪顺序时块跟着走，不会指错格子。
fn root_block(page: SettingsPage) -> Option<usize> {
    ROOT_BLOCKS
        .iter()
        .position(|(_, members)| members.contains(&page))
}

/// 一次搜索的命中。
///
/// 两种命中要的东西不同，所以不是一种东西：`Page` 是「知道有这一页、忘了它叫什么」，进去
/// 那一屏就完事；`Row` 是「知道要调哪一格」，于是把**那一格连同它的控件**直接摆在这里，
/// 当场就能改 —— 搜索结果不是链接列表。
enum Hit {
    Page(usize),
    Row(usize, usize, usize),
}

/// 按页序扫一遍规格，挑出整页名字对上的页，以及标题或关键词对上的行。
///
/// 匹配只有一条规则：**忽略大小写的子串**。拼音、缩写那种「猜词」不做 —— 那份要先养一张
/// 拼音表，而它一错就是「明明有却搜不到」，比少一种匹配方式贵得多。
///
/// 扫的是**当前这一份规格**：被关掉的那一档根本不在规格里（见 `pages/translation.rs` 的
/// 「只摆选中的那套引擎的设置」），所以没选 DeepL 时搜「DeepL」不会摆出一格按不动的字段 ——
/// 结果不会指向进不去的地方。
fn search_hits(pages: &[spec::Page], query: &str) -> Vec<Hit> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for (page_ix, page) in pages.iter().enumerate() {
        if page.title.to_lowercase().contains(&needle) {
            hits.push(Hit::Page(page_ix));
        }
        for (group_ix, group) in page.groups.iter().enumerate() {
            for (row_ix, row) in group.rows.iter().enumerate() {
                let title = row.title.to_lowercase();
                let said = title.contains(&needle)
                    || row
                        .keywords
                        .iter()
                        .any(|word| word.to_lowercase().contains(&needle));
                if said {
                    hits.push(Hit::Row(page_ix, group_ix, row_ix));
                }
            }
        }
    }
    hits
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
    /// 对「WebDAV 同步」状态实体的订阅。同步那趟在后台线程上结束，状态与计数靠
    /// 实体 notify 才能出现在设置页上。
    _sync_watch: Subscription,
    /// 对「会员兑换」状态实体的订阅。那一趟在后台线程上结束，到期日与结果那句都要靠
    /// 实体 notify 才出现在这一屏上。
    _activation_watch: Subscription,
    /// 对「没拿到所有文件访问」那一问的订阅：面板的摆与收都靠它 notify 这一屏。
    _access_watch: Subscription,
    /// 「更新日志」那张卡片。**本屏一份**：与桌面那一屏同一个形状 —— 关于那一页的按钮拿的是
    /// 这一个句柄，挂的也是这一屏，所以两屏同时活着时各收各的（理由写在 `notes_sheet` 顶部）。
    notes: Entity<NotesSheet>,
    /// 顶上那颗搜索框：`(框, 订阅, 建它时那句占位文案)`。
    ///
    /// 建它要 `window`，而外壳那头 `cx.new(CompactSettings::new)` 手里没有 window，所以第一帧
    /// 就地建（与文本行那格 `InputState` 同一形状）。第三格不是装饰：占位文案是建框那一刻
    /// 译的，切语言后规格整份重译，只有这句跟着旧语言 —— 拿它比一次就知道该不该重建。
    search: Option<(Entity<SearchBox>, Subscription, SharedString)>,
    /// 框里当前那句查询词。空串 = 没在搜，根屏摆分块的入口列表。
    ///
    /// 真源在这一格而不是框里：返回键收掉搜索态时改的是它，下一帧把框里的字拉回来。
    query: SharedString,
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
        let sync = crate::ui::services::sync::entity(cx);
        let sync_watch = cx.observe(&sync, |_, _, cx| cx.notify());
        let activation = crate::ui::services::activation::entity(cx);
        let activation_watch = cx.observe(&activation, |_, _, cx| cx.notify());
        let notes = cx.new(NotesSheet::new);
        // 「没拿到所有文件访问」那一问：面板摆不摆由这份实体说了算，所以它一 notify
        // 这一屏就得重画（与上面那几条订阅同一理由）。
        let question = crate::ui::services::library::access(cx);
        let question_watch = cx.observe(&question, |_, _, cx| cx.notify());
        Self {
            screen: Screen::Root,
            _storage_watch: watch,
            _update_watch: update_watch,
            _feedback_watch: feedback_watch,
            _sync_watch: sync_watch,
            _activation_watch: activation_watch,
            _access_watch: question_watch,
            notes,
            search: None,
            query: SharedString::default(),
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
    ///
    /// 「更新日志」那张卡片压在这一屏上面时，返回键**只收卡片**：与阅读页那几块弹层同一条
    /// 次序（先最上面那一层，再往外退），否则一按就把底下的子页也弹掉了。
    pub(crate) fn go_back(&mut self, cx: &mut Context<Self>) -> bool {
        if self.notes.read(cx).is_open(cx) {
            self.notes.update(cx, |card, cx| card.hide(cx));
            cx.notify();
            return true;
        }
        // 搜索态是这一屏最外面的一层：根屏上先按一次收掉查询词，再按一次才轮到外壳
        // （最终是退出应用）。与「更新日志」那张卡片同一规矩 —— 一次只退一层。
        if matches!(self.screen, Screen::Root) && !self.query.is_empty() {
            self.query = SharedString::default();
            cx.notify();
            return true;
        }
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

    /// 根屏：搜索框 + 一张会员卡 + 三块带节头的入口 + 末尾一张无节头的卡，一行一页，右端只留那枚进去的箭头。
    ///
    /// 会员那一格**不摆在入口列表里**，而是单独一张卡顶在列表上面：它要当场露出等级与
    /// 到期日，而列表那一行按定稿是不挂当前值的（见文件头）。两处摆同一个目的地是噪声，
    /// 所以列表里跳过它。
    ///
    /// 框里有词时整屏换成结果那一屏（[`Self::search_results`]）：块结构回答的是「这一类里有
    /// 什么」，回答不了「我知道要找『行距』，但不知道它在哪一页」。
    fn root_screen(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // 悬浮底栏（Compact 档）压在内容上：滚动内容自己要在尾部让出那一段。判据只在
        // 根屏为真 —— 钻进来的子页由外壳把栏整条收起，那一屏吃满整窗（`pushed`）。
        let tail = navigation::clearance(crate::ui::host::safe_area(window, cx).bottom);
        let field = self.search_field(window, cx);
        if !self.query.is_empty() {
            return v_flex()
                .w_full()
                .flex_1()
                .min_h_0()
                .child(field)
                .child(self.search_results(window, tail, cx))
                .into_any_element();
        }
        let entity = cx.entity().clone();
        let pages = spec_pages(cx, &self.notes);
        let membership_ix = SettingsPage::Membership.index();
        let mut blocks: Vec<Vec<AnyElement>> = ROOT_BLOCKS.iter().map(|_| Vec::new()).collect();
        let mut unblocked: Vec<AnyElement> = Vec::new();
        for (ix, page) in pages.iter().enumerate() {
            if ix == membership_ix {
                continue;
            }
            // 分隔线按「这一块前面有没有行」摆，不按页序：跳过会员那一格以后，第一块的
            // 第一行不再是从前那个 `ix == 0`。
            let rows = match SettingsPage::ALL.get(ix).and_then(|p| root_block(*p)) {
                Some(block) => &mut blocks[block],
                None => &mut unblocked,
            };
            if !rows.is_empty() {
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
        // 的两个名字，读者先看见的是那句和底栏对不上的字。
        // 缝的档位由**这一块自己带不带节头**决定，与上一块是什么无关：会员卡与「关于」那张
        // 都没有节头，所以它们下面那一块照旧按下一块的档位走（带节头的 16）。
        let mut body = v_flex().w_full().child(
            div()
                .w_full()
                .px_4()
                .mt_2()
                .child(card(vec![self.membership_cell(cx)], cx)),
        );
        for (block, (key, _)) in ROOT_BLOCKS.iter().enumerate() {
            let rows = std::mem::take(&mut blocks[block]);
            // 空块不摆：它连一道缝都不该占。这一档在今天的页表里不会发生，但根屏是读者唯一的
            // 入口，宁可少一块，也别摆一张空卡。
            if rows.is_empty() {
                continue;
            }
            body = body.child(
                v_flex()
                    .w_full()
                    .px_4()
                    .mt_4()
                    .child(section_header(tr(cx, key), cx))
                    .child(card(rows, cx)),
            );
        }
        // 末尾那一张不摆节头：里头只有「关于」，而一块只剩一行时，块名念的就是那一行自己的
        // 名字。将来若有没登记的页也落在这里 —— 少一句块名，不少一个入口。
        // 正因为它没有节头，这一道缝留大的一档（32）：这一处只剩缝在说「这是另一件事」。
        if !unblocked.is_empty() {
            body = body.child(div().w_full().px_4().mt_8().child(card(unblocked, cx)));
        }
        // 分了块之后这一屏装不下（13 行 + 三句节头 + 五道缝，比魅族 20 那一档去掉状态栏与
        // 底栏的可用高度还高），所以根屏也跟着滚起来。底栏那一段留白交给 `scrollable`
        // 垫在滚动内容里（见那一格的注释）。
        // 搜索框不进滚动区：它是要反复用的输入口，滚一下就没得输入不叫「省一格」。
        v_flex()
            .w_full()
            .flex_1()
            .min_h_0()
            .child(field)
            .child(Self::scrollable(body, tail))
            .into_any_element()
    }

    /// 顶上那颗搜索框：第一帧就地建，换了语言重建，并把 `query` 拉回框里。
    fn search_field(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let placeholder = tr(cx, "account.search_placeholder");
        if self
            .search
            .as_ref()
            .is_some_and(|(_, _, built)| *built != placeholder)
        {
            // 语言换了。占位文案是建框那一刻译的，不重建就成了「整屏是新语言、框里那句还是
            // 旧语言」。查询词留在 `query` 那一格，跟着带进新框。
            self.search = None;
        }
        if self.search.is_none() {
            let boxed = cx.new(|cx| SearchBox::new(placeholder.clone(), window, cx));
            let watch = cx.subscribe(&boxed, move |this, _, event: &SearchBoxEvent, cx| {
                let SearchBoxEvent::Change(query) = event;
                if this.query != *query {
                    this.query = query.clone();
                    cx.notify();
                }
            });
            self.search = Some((boxed, watch, placeholder));
        }
        let (boxed, _, _) = self.search.as_ref().expect("上面要么建好、要么本来就有");
        // 框里的字与 `query` 走偏时以 `query` 为准（返回键收掉搜索态走的就是这一格）——
        // 与文本行那格「配置在他处被改时把显示拉回来」同一条做法。
        let query = self.query.clone();
        boxed.update(cx, |search, cx| search.set_value(query, window, cx));
        div()
            .w_full()
            .px_4()
            .py_2()
            .child(boxed.clone())
            .into_any_element()
    }

    /// 搜出来那一屏：页命中一张卡，行命中按页归堆、每堆一句节头（页名）加一张卡。
    ///
    /// 卡里摆的是**那一格自己** —— 开关就是开关、步进就是步进，当场能改。把结果做成一串
    /// 链接，读者按完还得在那一屏里把同一句话再找一遍，而这次搜索要治的就是「找不到」。
    fn search_results(&self, window: &mut Window, tail: f32, cx: &mut Context<Self>) -> AnyElement {
        let pages = spec_pages(cx, &self.notes);
        let hits = search_hits(&pages, &self.query);
        let opened: Vec<usize> = hits
            .iter()
            .filter_map(|hit| match hit {
                Hit::Page(page_ix) => Some(*page_ix),
                Hit::Row(..) => None,
            })
            .collect();
        let mut grouped: Vec<(usize, Vec<(usize, usize)>)> = Vec::new();
        for hit in &hits {
            if let Hit::Row(page_ix, group_ix, row_ix) = hit {
                match grouped.iter_mut().find(|(page, _)| *page == *page_ix) {
                    Some((_, rows)) => rows.push((*group_ix, *row_ix)),
                    None => grouped.push((*page_ix, vec![(*group_ix, *row_ix)])),
                }
            }
        }
        let mut body = v_flex().w_full();
        let mut first = true;
        if !opened.is_empty() {
            let entity = cx.entity().clone();
            let mut rows: Vec<AnyElement> = Vec::new();
            for page_ix in opened.iter().copied() {
                if !rows.is_empty() {
                    rows.push(separator(3.5, cx).into_any_element());
                }
                let page = &pages[page_ix];
                let entity = entity.clone();
                rows.push(push_cell(
                    SharedString::from(format!("settings-hit-page-{page_ix}")),
                    Some(page.icon),
                    page.title.clone(),
                    None,
                    move |_, _, app| entity.update(app, |this, cx| this.open(page_ix, cx)),
                    cx,
                ));
            }
            let gap = if first {
                v_flex().w_full().mt_2()
            } else {
                v_flex().w_full().mt_8()
            };
            first = false;
            body = body.child(gap.child(div().w_full().px_4().child(card(rows, cx))));
        }
        for (page_ix, found) in &grouped {
            let mut rows: Vec<AnyElement> = Vec::new();
            for (group_ix, row_ix) in found {
                if !rows.is_empty() {
                    rows.push(separator(1.0, cx).into_any_element());
                }
                rows.push(self.row(
                    *page_ix,
                    *group_ix,
                    *row_ix,
                    &pages[*page_ix].groups[*group_ix].rows[*row_ix],
                    window,
                    cx,
                ));
            }
            let mut block = if first {
                v_flex().w_full().mt_2()
            } else {
                // 结果里每一堆都带节头（那句就是页名），所以缝走 16 那一档，与子页同一数。
                v_flex().w_full().mt_4()
            };
            first = false;
            block = block
                .child(
                    div()
                        .w_full()
                        .px_4()
                        .child(section_header(pages[*page_ix].title.clone(), cx)),
                )
                .child(div().w_full().px_4().child(card(rows, cx)));
            body = body.child(block);
        }
        if opened.is_empty() && grouped.is_empty() {
            body = body.child(
                div()
                    .w_full()
                    .px_4()
                    .mt_2()
                    .child(muted_label(tr(cx, "account.search_empty"), cx)),
            );
        }
        Self::scrollable(body, tail)
    }

    /// 会员卡那一行：图标 + 两行字（页名，与「哪天到期 / 未开通」那一句）+ 一枚进去的箭头。
    ///
    /// 入口列表那些行不挂当前值，理由是「一行管的是去哪儿，不是现在是多少」；这一格是那条
    /// 规矩上唯一的例外：会员要看的正是「我现在算哪一档、哪天到期」，把这句藏进下一屏
    /// 等于这张卡白做。次要那一句走 [`muted_label`] —— 它是**行的内容**（状态），不是注释，
    /// 与同步那格的状态字同一档。
    ///
    /// 命中高仍由 [`ROW_HIT`] 兜：`min_h` 而不是固定高，多一句字这一行长一点，44 那一档不缩。
    fn membership_cell(&self, cx: &mut Context<Self>) -> AnyElement {
        let page_ix = SettingsPage::Membership.index();
        let entity = cx.entity().clone();
        let title = tr(cx, "page.membership");
        list_row(
            SharedString::from("settings-membership-card"),
            title.clone(),
            vec![
                icon_slot(IconName::Crown, cx).into_any_element(),
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(label_block(title))
                    .child(muted_label(status_line(cx), cx).into_any_element())
                    .into_any_element(),
                Icon::new(IconName::ChevronRight)
                    .small()
                    .text_color(cx.theme().muted_foreground)
                    .into_any_element(),
            ],
            move |_, _, app| entity.update(app, |this, cx| this.open(page_ix, cx)),
        )
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

    /// 子页正文：规格里的每个组各成一张卡，**多于一组时卡上方摆节头**。
    ///
    /// 节头就是规格里那句组标题（[`spec::Group::title`]），从前只有桌面侧栏读它。只有一组时
    /// 不摆：那一句话和顶栏上的页名是同一个东西，摆两遍是重复（规范「上下文已经建立的信息
    /// 不要再讲一遍」）—— 版面、字体、目录那些页就吃这一条。
    fn page_screen(
        &self,
        page_ix: usize,
        page: &spec::Page,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let headed = page.groups.len() > 1;
        let mut body = v_flex().w_full();
        for (group_ix, group) in page.groups.iter().enumerate() {
            let mut rows: Vec<AnyElement> = Vec::new();
            for (row_ix, row) in group.rows.iter().enumerate() {
                if row_ix > 0 {
                    rows.push(separator(1.0, cx).into_any_element());
                }
                rows.push(self.row(page_ix, group_ix, row_ix, row, window, cx));
            }
            // 第一块与顶栏只留 8（Material 的列表贴着顶栏的那一档）。块与块之间**分两种**：
            // 带节头的 16，不带节头的仍然 32 —— 节头自己就是「这里换了一块」那句话，缝只需要
            // 再说一句「这块不属于上一张卡」；没有节头时只剩缝在说话，那就得留大的一档
            // （规范那条：相等的留白会被读成相等的关系）。
            //
            // 从前带节头也留 32，量出来上一张卡底到下一张卡顶是 32 + 22 = 54，比 Apple 那一档
            // （约 35）宽一半。现在 16 + 22 = 38，与它同一档。
            let gap = if group_ix == 0 {
                v_flex().w_full().mt_2()
            } else if headed {
                v_flex().w_full().mt_4()
            } else {
                v_flex().w_full().mt_8()
            };
            let mut block = div().w_full().px_4();
            if headed {
                block = block.child(section_header(group.title.clone(), cx));
            }
            body = body.child(gap.child(block.child(card(rows, cx))));
        }
        // 子页由外壳收起底栏、吃满整窗，尾部不必再让那一段。
        Self::scrollable(body, 0.0)
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
            return Self::scrollable(div().w_full(), 0.0);
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
        Self::scrollable(div().w_full().mt_2().px_4().child(card(rows, cx)), 0.0)
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
                        .child(label_block(row.title.clone()).flex_1().min_w_0())
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
                        .child(label_block(row.title.clone()).flex_1().min_w_0())
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
                        .child(label_block(row.title.clone()).flex_1().min_w_0())
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
            RowKind::Font { get, set } => {
                let (get, set) = (get.clone(), set.clone());
                let picker = window.use_keyed_state(id("font-picker"), cx, move |window, cx| {
                    let get_value = get.clone();
                    let set_value = set.clone();
                    FontPicker::new(
                        Rc::new(move |cx: &App| get_value(cx).to_string()),
                        Rc::new(move |value: String, cx: &mut App| {
                            set_value(SharedString::from(value), cx)
                        }),
                        window,
                        cx,
                    )
                });
                control_cell(
                    id("font-row"),
                    v_flex()
                        .w_full()
                        .gap_1()
                        .child(label_block(row.title.clone()))
                        .child(picker.clone()),
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
    ///
    /// `tail` 是悬浮底栏要的那一段（Compact 档的根屏，见 `navigation::clearance`）。它垫在
    /// **滚动内容里**，与架子 / 统计 / 笔记同一份写法：垫在视口外面时视口被永久截短，底栏
    /// 一让位（往下读）底部就空出一条，内容怎么滚都到不了屏幕底；垫在内容里，滚动途中内容
    /// 从栏底下穿过去，滚到底时最后一排正好停在栏的上沿。
    fn scrollable(body: impl IntoElement, tail: f32) -> AnyElement {
        div()
            .id("settings-compact-scroll")
            .overflow_y_scroll()
            .flex_1()
            .min_h_0()
            .w_full()
            .pb_8()
            .child(body)
            .child(div().w_full().h(px(tail)))
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
            // 卡片那一层是绝对定位的，要有一个能落位的父盒子（与架面那三块同一形状）。
            .relative()
            .size_full()
            .bg(cx.theme().background)
            .test_support();
        match self.screen {
            Screen::Root => root = root.child(self.root_screen(window, cx)),
            Screen::Page(page_ix) => {
                let page = spec_pages(cx, &self.notes).swap_remove(page_ix);
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
                let page = spec_pages(cx, &self.notes).swap_remove(page_ix);
                let title = page.groups[group].rows[row].title.clone();
                root = root
                    .child(self.pick_header(&title, page_ix, cx))
                    .child(self.pick_screen(page_ix, group, row, &page, cx));
            }
        }
        // 「更新日志」那张卡片挂在**这一屏的根**上，不挂在某一格里：读者从子页钻进来时
        // 卡片是子页的、退回根屏那一程它还在往下沉，两处都得有它落位的地方。
        // 元素每帧都交（弹簧的采样在里面），收稳的那一程它自己不给内容。
        root.child(self.notes.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use folio_core::Config;
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext, px, size};

    use super::{
        CompactSettings, Hit, NotesSheet, ROOT_BLOCKS, Screen, SettingsPage, root_block,
        search_hits, spec_pages,
    };
    use crate::ui::services::config::ConfigHolder;
    use crate::ui::services::update::Notes;

    /// 分块不许漏页：除会员（根顶那张卡）外，每一页要么落进 `ROOT_BLOCKS` 的一块，要么落进
    /// 末尾那张无节头的卡 —— 而后者今天只该站着「关于」。
    ///
    /// 这条用例管的是「加一页忘了登记」。那种页不会从屏上消失（它掉进无节头那一张），但它
    /// 没有块名，而根屏那条列表又会重新变回「一串看不出类的行」—— 正是这次分块要治的那个病。
    /// 红在这里比红在「读者扫不到那一格」便宜得多。
    #[test]
    fn every_page_lands_in_a_root_block() {
        let unblocked: Vec<SettingsPage> = SettingsPage::ALL
            .iter()
            .filter(|page| root_block(**page).is_none())
            .copied()
            .collect();
        assert_eq!(
            unblocked,
            vec![SettingsPage::Membership, SettingsPage::About],
            "除会员与关于之外，每一页都要登记进 ROOT_BLOCKS 的某一块"
        );
        // 同一页不许落进两块：`root_block` 用 `position` 只认第一次，所以登记总数与块里的
        // 名额对不上就是重复 —— 重复的症状是那一页在屏上出现两次。
        let registered = SettingsPage::ALL
            .iter()
            .filter(|page| root_block(**page).is_some())
            .count();
        let slots: usize = ROOT_BLOCKS.iter().map(|(_, members)| members.len()).sum();
        assert_eq!(registered, slots, "ROOT_BLOCKS 里有一页被登记了两次");
    }

    /// 搜一行标题要带出**它在哪一页**：`Row` 命中摆的是那一格本身，而读者要的是「它归哪页管」
    /// 那一句；`Page` 命中则只给页。两头都用规格里现取的文案当查询词，不写死任何一句话 ——
    /// 这一条问的是命中的形状，与九门语言各自怎么措辞无关。
    #[gpui_kit::test]
    async fn a_hit_carries_the_page_it_lives_on(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| ConfigHolder::install(Config::default(), cx));
        let notes = cx.update(|cx| cx.new(NotesSheet::new));
        let (hits, text_ix, storage_ix) = cx.read(|cx| {
            let pages = spec_pages(cx, &notes);
            let text_ix = SettingsPage::Typography.index();
            let storage_ix = SettingsPage::Storage.index();
            let needle = pages[text_ix].groups[0].rows[0].title.clone();
            let mut hits = search_hits(&pages, &needle);
            hits.extend(search_hits(&pages, &pages[storage_ix].title.clone()));
            (hits, text_ix, storage_ix)
        });
        assert!(
            hits.iter().any(|hit| matches!(
                hit,
                Hit::Row(page, 0, 0) if *page == text_ix
            )),
            "拿「文字」页第一格的标题搜，该命中它自己那一格"
        );
        assert!(
            hits.iter()
                .any(|hit| matches!(hit, Hit::Page(page) if *page == storage_ix)),
            "拿一整页的页名搜，该把那一页本身摆出来"
        );
    }

    /// 空串与全空格不搜：那不是一个查询词，而根屏该回到分块的入口列表。
    #[gpui_kit::test]
    async fn an_empty_query_hits_nothing(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| ConfigHolder::install(Config::default(), cx));
        let notes = cx.update(|cx| cx.new(NotesSheet::new));
        cx.read(|cx| {
            let pages = spec_pages(cx, &notes);
            assert!(search_hits(&pages, "").is_empty());
            assert!(search_hits(&pages, "   ").is_empty());
        });
    }

    /// 带着查询词按返回，先收掉搜索态；第二下才递给外壳（最终是退出应用）。
    ///
    /// 收的是 `query` 那一格，框里的字由下一帧拉回来 —— 少这一步就成了「按了一下什么都没变，
    /// 只是屏外的东西没了」。
    #[gpui_kit::test]
    async fn back_clears_the_search_before_it_leaves_the_screen(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let view = cx.update(|cx| cx.new(CompactSettings::new));
        cx.update(|cx| view.update(cx, |this, _| this.query = "字".into()));
        let eaten = cx.update(|cx| view.update(cx, |this, cx| this.go_back(cx)));
        assert!(
            eaten,
            "根屏上带着查询词时那一按该先收搜索态，不该直接递给外壳"
        );
        let query = cx.read(|cx| view.read(cx).query.clone());
        assert!(query.is_empty(), "收掉的是查询词，屏还停在根屏");
        assert!(
            !cx.update(|cx| view.update(cx, |this, cx| this.go_back(cx))),
            "搜索态收干净之后，第二下才轮到外壳"
        );
    }

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
        // 按页名取序号，不写死的 `2`：这一条问的是层级（选项屏 → 那一页 → 根屏），
        // 与「第几页」无关，而导航顺序调过一次（会员置顶）之后写死的数就指到别处去了。
        let page = SettingsPage::Layout.index();

        cx.update(|cx| {
            view.update(cx, |this, _| {
                this.screen = Screen::Pick {
                    page,
                    group: 1,
                    row: 0,
                };
            })
        });
        assert!(press(cx), "选项屏上按返回该被这一屏吃掉");
        assert_eq!(shown(cx), Screen::Page(page), "退回的是它来的那一页");
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
        // 同样按页名取：这一格不能是 Storage（`open` 对它另有安排），Layout 是一页普通的整屏。
        let page = SettingsPage::Layout.index();
        cx.update(|cx| view.update(cx, |this, cx| this.open(page, cx)));
        assert!(pushed(cx), "钻进某一页要吃满整窗");

        cx.update(|cx| {
            view.update(cx, |this, _| {
                this.screen = Screen::Pick {
                    page,
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

    /// 卡片开着时那一按**只收卡片**，不吃层级。
    ///
    /// 少这一档的症状在真机上是一按两用：卡片沉下去的同时子页也退回根屏，读者以为自己按错了。
    /// 层级那一条上面已经钉过，这条钉的是「压在最上面的那一层先收」那一步次序。
    #[gpui_kit::test]
    fn the_back_key_only_folds_the_card_while_it_is_up(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let view = cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            cx.new(CompactSettings::new)
        });
        let card_up =
            |cx: &TestAppContext| view.read_with(cx, |this, cx| this.notes.read(cx).is_open(cx));

        cx.update(|app| {
            view.update(app, |this, cx| {
                this.open(SettingsPage::About.index(), cx);
                this.notes.update(cx, |card, cx| {
                    card.open(Notes::Ready("### 新增".into()), cx)
                })
            })
        });
        assert!(card_up(cx), "前置：卡片该是开着的");

        let eaten = view.update(cx, |this, cx| this.go_back(cx));
        assert!(eaten, "开着卡片时那一按该被卡片吃掉");
        assert!(!card_up(cx), "返回键收掉的是卡片");
        assert!(
            view.read_with(cx, |this, _| this.pushed()),
            "子页该还压在那儿，不该被顺手弹掉"
        );
    }

    /// 手机那一屏**也**挂着「更新日志」那张卡片：抬起来时这一屏得有落位的地方。
    ///
    /// 桌面那一头的同一条写在 `pages::about` 的测试里（那条从按下按钮走完整条链）。这里只钉
    /// 挂载这一半，因为少挂一格的症状是「按了没反应」而编译照旧过 —— 两端各钉一次，
    /// 哪一头漏挂都会红。
    #[gpui_kit::test]
    fn the_compact_screen_mounts_the_same_notes_card(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let mut built = None;
        let handle: AnyWindowHandle = cx
            .open_window(size(px(390.), px(844.)), |window, cx| {
                ConfigHolder::install(Config::default(), cx);
                let view = cx.new(CompactSettings::new);
                built = Some(view.clone());
                Root::new(view, window, cx)
            })
            .into();
        let view = built.expect("这一屏该建起来");

        let raised = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.try_find("bottom-sheet-surface").is_some()
            })
            .expect("渲染应成功")
        };
        assert!(!raised(cx), "没抬起来之前不该有这块弹层");

        cx.update(|app| {
            view.update(app, |this, cx| {
                this.notes.update(cx, |card, cx| {
                    card.open(
                        Notes::Ready("### 新增\n- **听书（合成朗读）。**".into()),
                        cx,
                    )
                })
            })
        });
        // 弹簧按测试钟计步：不推开钟它永远停在 0，卡片看着就像「没升起来」。
        for _ in 0..40 {
            cx.executor().advance_clock(Duration::from_millis(16));
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .expect("渲染应成功");
        }
        assert!(
            raised(cx),
            "手机那一屏也要挂上这张卡片，不然抬起来没有落位的地方"
        );
    }

    /// 手机端把「TXT 分章规则」这一屏**真画一帧**：一组里有一条关掉的出厂规则，
    /// 另一组里有一条自定义规则（带模式与那颗删除钮）。
    ///
    /// 这条钉的不是画面而是「画得出」：那一屏的开关、名字、匹配方式、模式、编号、删除六格
    /// 全是渲染时才跑的闭包，编译过不等于画得出来 —— 少一格在两端之一炸，真机上就是
    /// 一点进去就闪退，而那正是最贵的一种红。
    #[gpui_kit::test]
    fn the_rules_screen_paints_a_frame_with_a_custom_rule(cx: &mut TestAppContext) {
        use folio_core::content::format::novel::rules::{MatchKind, NumberingStyle};

        cx.update(gpui_kit::init);
        let mut built = None;
        let handle: AnyWindowHandle = cx
            .open_window(size(px(390.), px(844.)), |window, cx| {
                ConfigHolder::install(Config::default(), cx);
                let view = cx.new(CompactSettings::new);
                built = Some(view.clone());
                Root::new(view, window, cx)
            })
            .into();
        let view = built.expect("这一屏该建起来");

        cx.update(|app| {
            view.update(app, |this, cx| {
                this.open(SettingsPage::TxtRules.index(), cx)
            });
            ConfigHolder::edit_novel(app, |novel| {
                novel.edits.off.push("番外".to_string());
                novel
                    .edits
                    .extra
                    .push(folio_core::content::format::novel::rules::Marker {
                        name: "幕".to_string(),
                        kind: MatchKind::Regex,
                        pattern: Some(r"^\s*第[一二三]幕".to_string()),
                        text: None,
                        level: 1,
                        numbering: NumberingStyle::Arabic,
                        examples: Default::default(),
                    });
            });
        });

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
        })
        .expect("关一条 + 加一条之后，这一屏该画得出一帧");
    }
}
