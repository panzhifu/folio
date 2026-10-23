//! 存储：这一台设备上 folio 把字节放在哪儿、放了多厚，以及能清掉哪几档。
//!
//! 屏上只有一个大读数 —— **本应用占了多大**。它是这一屏的第一个问题，其余都是它的分解
//! （`Design Guides → Start from the task`：先写任务，别先摆控件）。于是：
//!
//! - 构成用**环形图**，一片一类；环下面那一列明细与它**同源同序** —— 同一个 `Vec<Slice>`
//!   喂给两处，第几片就是第几行。颜色只负责把两者对上，名字与字节都写在行上：规范那条
//!   「不许只靠颜色传义」，而移动端浮层点不住，tooltip 从来不是通路（与「统计」同一条事实）；
//! - 图下面那两行 muted 的小字只说**账本身的成色**（几处没读到、上一次为什么没跑成），不
//!   复述图上已经看得出来的一件事 —— 「哪一类最厚」环上第一片就是它，明细第一行也是它；
//!   文件计数也不摆：读者要的是「哪些能扔」，那一列每一行都带着自己的字节数；
//! - 这块盘还剩多少**不在这一屏**：那台设备的总量是系统设置的地盘，而读者要回答的问题只是
//!   「folio 这些字节里哪些能扔」；
//! - 清理那一组只摆**真有东西**的那几档：没东西可清时那颗钮按下去什么也不变，比不摆更差
//!   （`每个动作都要说得出它的对象、范围与结果`）。删之前摊开确认面板，后果那句按档各写
//!   一条 —— 这几档的可逆程度不一样，用同一句话糊过去就是在骗读者。
//!
//! ## 数从哪儿来，为什么这一页自己不做统计
//!
//! 每一格**现读实体**（[`storage::handle`]），不捕获「建规格那一刻」的数：静态设置页每帧重建
//! （见 `pages/library.rs` 里那句 `detach()` 的注释），捕获进来就是一份旧账，而统计跑完那一趟
//! 重画的正是旧账。实体 `notify` → 观察者（[`super::desktop::SettingsView`] /
//! [`super::compact::CompactSettings`]）重画 → 这些闭包重跑一遍，屏幕才跟着变成新的数。
//!
//! 遍历、删除、`VACUUM` 全在作业池的线程上（见 [`storage`] 的模块说明）。这一页只起活与读数，
//! 这里连一次 `std::fs` 都没有。

use super::super::*;

use gpui_kit::TestSupportExt as _;
use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::chart::PieChart;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, v_flex};
use gpui_kit::{AnyElement, FontWeight, Hsla, InteractiveElement as _, px, rems};

use crate::ui::components::muted_label;
use crate::ui::services::l10n;
use crate::ui::services::storage::{self, Job, Snapshot};
use folio_core::infra::usage::Area;

/// 环的图盒高。**必须显式给**：图表不自己量高，而它在滚动层里拿不到可被 `flex_1` 解析的高，
/// 不给就是零高 —— 症状与「统计」那屏记过的一样：只剩标题，图不见了。
const RING_BOX: f32 = 176.0;
/// 环的外半径与内半径（逻辑像素）。两者定死而不是按盒宽算：窄到 320dp 也放得下直径 164 的
/// 一个环，而宽屏上它不该被拉成一张大饼 —— 环讲比例，不讲面积。
const RING_OUTER: f32 = 82.0;
const RING_INNER: f32 = 54.0;
/// 明细行右端那两条车道：占比一栏、字节一栏。定宽才立得住一条竖着比的脊线。
const SHARE_LANE: f32 = 3.0;
const BYTES_LANE: f32 = 4.5;

/// 环上的一片，同时是明细里的一行。
///
/// 一份数据两处用，所以「这一片是什么、多大、什么色」只算一次；顺序由
/// [`folio_core::infra::usage::Usage::filled`] 定（按字节从大到小），环与明细共用那一个序。
#[derive(Clone)]
struct Slice {
    label: SharedString,
    bytes: u64,
    /// 念出来的大小。**在这里算一次**：图表的 tooltip 闭包要 `'static`，握不住 `&App`，
    /// 而分隔符是跟着语言走的 —— 所以带格式的那一步只能在有 `cx` 的这一层做完。
    shown: SharedString,
    /// 占总数几成（`0.0..1.0`）。摆进 `Progress` 那一档时才乘 100。
    share: f32,
    color: Hsla,
}

/// 这一页。
pub(in crate::ui::features::settings) fn storage_page(cx: &App) -> Page {
    // 手机端不摆三行节头：这一页的三张卡各自说得出自己是什么（大读数、带标题的清理行、
    // 带路径的位置行），上方那行小灰字只是把同一个词再念一遍。桌面照旧摆 —— 那边的组
    // 标题同时是侧栏子项。
    page(
        tr(cx, "page.storage"),
        IconName::HardDrive,
        vec![
            group(tr(cx, "group.usage"), usage_rows()),
            group(tr(cx, "group.cleanup"), cleanup_rows(cx)),
            group(tr(cx, "group.location"), location_rows(cx)),
        ],
    )
    .no_group_titles()
}

/// 一类去处念成什么。键名**写成字面量**交给取文案的函数：`services/l10n.rs` 那道门扫的
/// 正是这种调用的字面量参数，走变量传进去它就没得查，缺翻的那一门会在屏幕上以英文出现。
fn area_label(area: Area, cx: &App) -> SharedString {
    match area {
        Area::Books => tr(cx, "area.books"),
        Area::Covers => tr(cx, "area.covers"),
        Area::Thumbnails => tr(cx, "area.thumbnails"),
        Area::Staging => tr(cx, "area.staging"),
        Area::Database => tr(cx, "area.database"),
        Area::Logs => tr(cx, "area.logs"),
        Area::RotatedLogs => tr(cx, "area.rotated_logs"),
        Area::Comics => tr(cx, "area.comics"),
        Area::Inbox => tr(cx, "area.inbox"),
        Area::Config => tr(cx, "area.config"),
        Area::Other => tr(cx, "area.other"),
    }
}

/// 环上第 `index` 片的颜色：主题那五档图表色循环用，第二圈起压一档透明度。
///
/// 不新增色值（`Tokens before values`）。十一类在五档上必然重色，这是接受过的代价：真要
/// 分清得给这一屏配一套专属调色板，那是另一轮的事 —— 而颜色本来就不是唯一的通路。
fn slice_color(cx: &App, index: usize) -> Hsla {
    let theme = cx.theme();
    let base = [
        theme.chart_1,
        theme.chart_2,
        theme.chart_3,
        theme.chart_4,
        theme.chart_5,
    ][index % 5];
    match index / 5 {
        0 => base,
        1 => base.opacity(0.72),
        _ => base.opacity(0.45),
    }
}

/// 现在这一屏该摆的那份账。没统计过时 `None` —— 那一档只有一句「还没统计」加一颗钮。
fn snapshot_now(cx: &App) -> Option<Snapshot> {
    storage::handle(cx).and_then(|view| view.read(cx).snapshot().cloned())
}

/// 某一类现在多大。库里那一格特殊：那颗「整理」还回来的是**空闲页**那一截，不是库文件
/// 总大小，报总大小会让读者以为能省下一整个库。
fn area_bytes(area: Area, snapshot: &Snapshot) -> u64 {
    match area {
        Area::Database => snapshot.freeable.unwrap_or(0),
        _ => snapshot.usage.get(area).bytes,
    }
}

/// 环与明细共用的那一份数据。一个字节都没有时是空表（那一档不画环）。
fn slices(cx: &App) -> Vec<Slice> {
    let Some(snapshot) = snapshot_now(cx) else {
        return Vec::new();
    };
    if snapshot.usage.total == 0 {
        return Vec::new();
    }
    let total = snapshot.usage.total;
    snapshot
        .usage
        .filled()
        .iter()
        .enumerate()
        .map(|(index, entry)| Slice {
            label: area_label(entry.area, cx),
            bytes: entry.bytes,
            shown: storage::human_bytes(cx, entry.bytes),
            share: entry.bytes as f32 / total as f32,
            color: slice_color(cx, index),
        })
        .collect()
}

/// 占比念成一格数字 `42%`。非零但不足一格时念 `<1%`，不塌成 `0%`：读者要分得开
/// 「几乎没有」与「真的一点没有」。
///
/// `%` 紧跟数字是九门共用的一格数字，不译。
fn percent(cx: &App, share: f32) -> SharedString {
    if share > 0.0 && share < 0.01 {
        return SharedString::from("<1%");
    }
    // 取整交给 `l10n::number`：这一格永远是整数位，但**百分号前那一个空格**按 CLDR 是分语言的
    // （法语/德语要窄空格）。本轮只把分隔符接上，百分号仍紧跟数字 —— 要改就改这一处，
    // 别在九个调用点各折一次。
    SharedString::from(format!(
        "{}%",
        l10n::number(cx, f64::from(share * 100.0), 0)
    ))
}

/// 「占用」那一组：**一整格** —— 大读数、环、明细，加一句只在账不完整时才有的成色。
///
/// 收成一格而不是四行：这四段说的是同一本账，而卡里行与行之间那条发丝线会把一本账切成四格。
/// 那颗「重新统计」也跟着没了：进这一屏起一次、每一档清理之后再起一次（见
/// [`super::super::compact`] 里 `open` 那一句与 [`storage::StorageState::start`]），屏幕上
/// 摆的本来就是刚量出来的数，手动那一颗没有对象。
fn usage_rows() -> Vec<Row> {
    // 这一页的每一行都是**无标题的自定义行**，而桌面那一屏顶上有个搜索框：不给词的话，
    // 「缓存」「占用」这些词搜进设置里，这一页只有页名能被命中，行全隐身。
    const FIND: &[&str] = &[
        "storage", "space", "占用", "存储", "空间", "usage", "disk", "容量",
    ];
    vec![Row::custom(usage_block).keywords(FIND)]
}

/// 那一整块：读数在上，它的分解跟着下来。
fn usage_block(cx: &App) -> AnyElement {
    let mut block = v_flex().w_full().gap_4().child(headline(cx));
    if snapshot_now(cx).is_some() {
        block = block.child(ring(cx)).child(legend(cx));
        // 那两句没话可说时整段不摆：账完整的时候不该有一行字在那儿自言自语。
        if has_note(cx) {
            block = block.child(note(cx));
        }
    }
    block.into_any_element()
}

/// 那一个大读数。
fn headline(cx: &App) -> AnyElement {
    let Some(snapshot) = snapshot_now(cx) else {
        return muted_label(tr(cx, "usage.not_yet"), cx).into_any_element();
    };
    // 大读数上面那一行小字是它的名字。这一屏顶上再没有别的东西说「这是整个应用占的」，
    // 少了那一行，那一串数字就没有对象，而紧跟着的环与明细讲的是它的分解 —— 读者得自己
    // 认出上面那个总数是谁的总数。
    let caption = muted_label(tr(cx, "usage.total"), cx);
    if snapshot.usage.total == 0 {
        // 首次启动那一台干净的机器：一个字节都还没写。说「还没占什么」，而不是摆一个
        // 空环配一句读数。
        return v_flex()
            .w_full()
            .gap_1()
            .child(caption)
            .child(total_marker(cx, 0))
            .child(muted_label(tr(cx, "usage.empty"), cx))
            .into_any_element();
    }
    v_flex()
        .w_full()
        .gap_1()
        .child(caption)
        .child(total_marker(cx, snapshot.usage.total))
        .into_any_element()
}

/// 那一个大读数本身。尺寸与**测试标记只有这一处**：宿主测试按 `storage-total` 认「这一屏
/// 到底画没画出账来」（`view::desktop` 与 `view::compact` 各有一条醒屏的门），而它只在已经有
/// 一份账时才长出来 —— 没统计过时那一格摆的是一句话，不是这个。
fn total_marker(cx: &App, bytes: u64) -> impl IntoElement {
    div()
        .id("storage-total")
        .test_support()
        .text_3xl()
        .font_weight(FontWeight::SEMIBOLD)
        .child(storage::human_bytes(cx, bytes))
}

/// 环：一片一类，按字节从大到小。
fn ring(cx: &App) -> AnyElement {
    let data = slices(cx);
    if data.is_empty() {
        return div().w_full().into_any_element();
    }
    // 动画的重放键就是这份账本身：数字变了才重画一次「长出来」，静止的重画不重播。
    let key = data.iter().map(|slice| slice.bytes).sum::<u64>();
    div()
        .w_full()
        .h(px(RING_BOX))
        .child(
            PieChart::new(data)
                .value(|slice| slice.bytes as f32)
                .inner_radius(RING_INNER)
                .outer_radius(RING_OUTER)
                .color(|slice| slice.color)
                .pad_angle(0.02)
                .name(tr(cx, "usage.total"))
                .tooltip_name(|slice| slice.label.clone())
                .tooltip_value(|slice, _, pct| {
                    SharedString::from(format!("{pct:.1}% · {}", slice.shown))
                })
                .appear_key(key)
                .id("storage-ring"),
        )
        .into_any_element()
}

/// 明细那一列：色点 + 名字 + 占比 + 右对齐的字节。与环同源同序。
fn legend(cx: &App) -> AnyElement {
    let mut list = v_flex().w_full().gap_1();
    for slice in slices(cx) {
        list = list.child(
            h_flex()
                .w_full()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .size_2()
                        .flex_shrink_0()
                        .rounded_full()
                        .bg(slice.color),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .text_base()
                        .text_ellipsis()
                        .child(slice.label),
                )
                // 可比的两格各占一条车道并靠右：读者是竖着比这几行的。
                .child(
                    div()
                        .w(rems(SHARE_LANE))
                        .flex_shrink_0()
                        .text_sm()
                        .text_right()
                        .text_color(cx.theme().muted_foreground)
                        .child(percent(cx, slice.share)),
                )
                .child(
                    div()
                        .w(rems(BYTES_LANE))
                        .flex_shrink_0()
                        .text_base()
                        .text_right()
                        .child(slice.shown),
                ),
        );
    }
    list.into_any_element()
}

/// 那一句「账本身的成色」现在有没有话要说。
///
/// 判据与 [`note`] 里那两个条件**必须是同一份**：这里说有话、那里什么也不摆（或反过来），
/// 那一格里就多一段空白或少一句实话。
fn has_note(cx: &App) -> bool {
    snapshot_now(cx).is_some_and(|snapshot| snapshot.usage.unreadable > 0)
        || storage::handle(cx).is_some_and(|view| view.read(cx).reason().is_some())
}

/// 明细下面那两句「这数不完整」的话：几处读不进去、上一次为什么没跑成。
///
/// 都留在这里而不是弹窗：它们说的都是**账本身的成色**，不是某个动作的结果。两句都不成立时
/// 这一段整个不摆（判据 [`has_note`]）。
fn note(cx: &App) -> AnyElement {
    let Some(snapshot) = snapshot_now(cx) else {
        return div().w_full().into_any_element();
    };
    let mut col = v_flex().w_full().gap_1();
    if snapshot.usage.unreadable > 0 {
        col = col.child(muted_label(
            tf(cx, "usage.unreadable", &[("n", &snapshot.usage.unreadable)]),
            cx,
        ));
    }
    if let Some(reason) = storage::handle(cx).and_then(|view| view.read(cx).reason()) {
        // 失败时旧的账还留着，所以这一句要紧跟着那列数字说：屏幕上有数，但那数是上一趟的。
        col = col.child(muted_label(
            tf(cx, "usage.failed", &[("reason", &reason)]),
            cx,
        ));
    }
    col.into_any_element()
}

/// 「清理」那一组：只摆真有东西的档，以及库里确实有空闲页时那颗整库重写。
///
/// 全空时那一组不摆一排空位，摆一句「没什么可清」。
fn cleanup_rows(cx: &App) -> Vec<Row> {
    let Some(snapshot) = snapshot_now(cx) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for job in [
        Job::ClearThumbs,
        Job::ClearComics,
        Job::ClearPartials,
        Job::ClearLogs,
    ] {
        let Some(area) = job.area() else {
            continue;
        };
        if snapshot.usage.get(area).bytes > 0 {
            rows.push(action_row(job, area));
        }
    }
    // `freeable` 是 `None`（书库没装上、或那一条查询失败）时连「有多少空闲」都报不出来，
    // 也就不该有一颗按下去不知道在做什么的钮。
    if snapshot.freeable.is_some_and(|freeable| freeable > 0) {
        rows.push(action_row(Job::Vacuum, Area::Database));
    }
    if rows.is_empty() {
        return vec![
            Row::custom(move |cx| {
                muted_label(tr(cx, "usage.nothing_to_clean"), cx).into_any_element()
            })
            .keywords(CLEANUP_FIND),
        ];
    }
    rows.into_iter()
        .map(|row| row.keywords(CLEANUP_FIND))
        .collect()
}

/// 清理那一组的搜索词：这一组是这一屏唯一会**改掉磁盘**的地方，找得到比省这几行字要紧。
const CLEANUP_FIND: &[&str] = &[
    "cache",
    "clear",
    "clean",
    "vacuum",
    "缓存",
    "清理",
    "清掉",
    "缩略图",
    "回收",
];

/// 一颗动作钮那一行。常态是「名字 + 说明 + 右端那格字节 + 那颗钮」；摊开确认时**整行换成
/// 面板** —— 「点一下就删掉三百张缩略图」不该靠读者记住那颗钮在哪一侧。
fn action_row(job: Job, area: Area) -> Row {
    Row::custom(move |cx| {
        let view = storage::handle(cx);
        let pending = view
            .as_ref()
            .and_then(|view| view.read(cx).pending())
            .is_some_and(|asked| asked == job);
        let busy = view
            .as_ref()
            .map(|view| view.read(cx).busy())
            .unwrap_or(false);
        let bytes = snapshot_now(cx)
            .map(|snapshot| area_bytes(area, &snapshot))
            .unwrap_or(0);
        let name = action_label(job, cx);
        if pending {
            return confirm_panel(job, cx, bytes);
        }
        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .child(
                v_flex()
                    .min_w_0()
                    .flex_1()
                    .gap_0p5()
                    .child(div().text_base().text_ellipsis().child(name.clone()))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .text_ellipsis()
                            .child(action_hint(job, cx)),
                    ),
            )
            .child(
                div()
                    .w(rems(BYTES_LANE))
                    .flex_shrink_0()
                    .text_base()
                    .text_right()
                    .child(storage::human_bytes(cx, bytes)),
            )
            .child(
                // 图标 + 两三字标签：这一组四行排下来，长的动词短语会把右端那格挤掉。
                // 按钮的 id 用 [`Job::key`] 而不是文案：切语言不该把 hover 与焦点状态换一套。
                Button::new(format!("storage-clear-{}", job.key()))
                    .ghost()
                    .small()
                    .icon(if job == Job::Vacuum {
                        IconName::Database
                    } else {
                        IconName::Broom
                    })
                    .label(tr(cx, "action.clear").to_string())
                    .accessibility_label(name)
                    .disabled(busy)
                    .on_click(move |_, _, cx| storage::ask(cx, job)),
            )
            .into_any_element()
    })
}

/// 确认面板那句问话，一档一条整句。
fn confirm_title(job: Job, cx: &App) -> SharedString {
    match job {
        Job::ClearThumbs => tr(cx, "confirm.thumbs.title"),
        Job::ClearComics => tr(cx, "confirm.comics.title"),
        Job::ClearPartials => tr(cx, "confirm.partials.title"),
        Job::ClearLogs => tr(cx, "confirm.logs.title"),
        Job::Vacuum => tr(cx, "confirm.vacuum.title"),
        // 重扫不是决定，没有面板可摆；走到这一档是调用方写错了。
        Job::Rescan => SharedString::default(),
    }
}

/// 确认面板：一句问的是什么、一句后果、两颗钮（取消在左；结果在右，destructive 上 `danger`）。
///
/// 标题与正文都**一档一条整句**。先前写成「通用问句 + 档位名」的拼法：德语那句动词要落到
/// 句尾、俄语要看那一个词的性数，拼出来的是一句不像界面语言的机器话（规范那条
/// 「按每种语言写，别把源语言的形状搬过去」），而且 `title = "%{what}？"` 在中文里读起来
/// 根本不是问句。代价是十行键值，换的是九门各自成句。
fn confirm_panel(job: Job, cx: &App, bytes: u64) -> AnyElement {
    v_flex()
        .w_full()
        .gap_2()
        .child(
            div()
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .child(confirm_title(job, cx)),
        )
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(confirm_body(job, cx, bytes)),
        )
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .child(
                    Button::new("storage-confirm-cancel")
                        .ghost()
                        .label(tr(cx, "shelf.cancel").to_string())
                        .on_click(|_, _, cx| storage::answer(cx, false)),
                )
                .child(
                    // 结果那颗钮写**动词**（对象名已经问在标题里了）：规范那条「按钮标签讲
                    // 结果」，而这里若再摆一遍档位名，屏幕上就是两个名词并排、没人说要干什么。
                    Button::new("storage-confirm-go")
                        .danger()
                        .label(confirm_result_label(job, cx).to_string())
                        .on_click(|_, _, cx| storage::answer(cx, true)),
                ),
        )
        .into_any_element()
}

/// 面板里那颗钮的文字：四档缓存都是「清掉」，整库重写那一档是它自己的动词短语。
fn confirm_result_label(job: Job, cx: &App) -> SharedString {
    match job {
        Job::Vacuum => tr(cx, "action.vacuum"),
        _ => tr(cx, "action.clear"),
    }
}

/// 每一颗按钮的名字。键写字面量，理由同 [`area_label`]。
fn action_label(job: Job, cx: &App) -> SharedString {
    match job {
        Job::Rescan => tr(cx, "action.rescan"),
        Job::ClearThumbs => tr(cx, "action.clear_thumbs"),
        Job::ClearComics => tr(cx, "action.clear_comics"),
        Job::ClearPartials => tr(cx, "action.clear_partials"),
        Job::ClearLogs => tr(cx, "action.clear_logs"),
        Job::Vacuum => tr(cx, "action.vacuum"),
    }
}

/// 每一行下面那句「这是什么」。键名跟着 `action.*` 那张表走，后缀 `.desc` 与 `[field]`
/// 那些字段同一写法（见 locales 里 `"font_size.desc"`）。
fn action_hint(job: Job, cx: &App) -> SharedString {
    match job {
        Job::Rescan => SharedString::default(),
        Job::ClearThumbs => tr(cx, "action.clear_thumbs.desc"),
        Job::ClearComics => tr(cx, "action.clear_comics.desc"),
        Job::ClearPartials => tr(cx, "action.clear_partials.desc"),
        Job::ClearLogs => tr(cx, "action.clear_logs.desc"),
        Job::Vacuum => tr(cx, "action.vacuum.desc"),
    }
}

/// 确认面板里那句后果，**一档一条整句**。
///
/// 不拆成「通用一句 + 后果片段」再拼：那等于用中文的语序去装配其余八门（规范那条
/// 「别把源语言的词序搬进另一门语言」）。这四档丢东西的程度本来就不一样 —— 缩略图与解压
/// 产物会自己长回来，半成品本来就没进库，旧日志是上一轮的现场，而 `VACUUM` 只是重写、
/// 不丢一行 —— 各写一句也是把这件事逐档说清。
fn confirm_body(job: Job, cx: &App, bytes: u64) -> String {
    let shown = storage::human_bytes(cx, bytes);
    match job {
        Job::ClearThumbs => tf(cx, "confirm.thumbs.body", &[("bytes", &shown)]),
        Job::ClearComics => tf(cx, "confirm.comics.body", &[("bytes", &shown)]),
        Job::ClearPartials => tf(cx, "confirm.partials.body", &[("bytes", &shown)]),
        Job::ClearLogs => tf(cx, "confirm.logs.body", &[("bytes", &shown)]),
        Job::Vacuum => tf(cx, "confirm.vacuum.body", &[("bytes", &shown)]),
        // 重扫不是决定，没有面板可摆。走到这一档是调用方写错了，安静说一句通用话。
        Job::Rescan => String::new(),
    }
}

/// 「位置」那一组：三个去处，各带路径与一颗「打开所在文件夹」。
///
/// 数据根与配置根**合成一格「软件数据目录」**：读者要回答的问题是「folio 的东西在这台设备的
/// 哪儿」，而移动端那两头本来就是同一个应用专属目录（`paths::config_dir` 在 Android 上就落在
/// 数据根里）。桌面上它们确实是两个目录（`~/.config/folio` 与 `~/.local/share/folio`），这一格
/// 报的是**数据根**那一个 —— 少掉的那一条是配置目录，不是少掉一个能打开的地方。
///
/// 路径留在行上而不是只留一颗钮：手机上那一颗是尽力而为（没有应用认领这个目录就什么都不
/// 发生，[`open_folder_button`] 那头只记一条 warn），那时这一串字是读者唯一拿得到的东西。
/// 形状抄「关于」那页「配置文件在哪儿」那一格。
fn location_rows(cx: &App) -> Vec<Row> {
    let areas = storage::areas(cx);
    let mut rows = vec![path_row(cx, "field.app_dir", areas.data.clone())];
    // 书库位置没确认过、日志不落盘：这两种情形各说一句「为什么没有路径可开」。
    // 那两句写成**字面量**留在下面这两个函数里 —— 收进一个 `&'static str` 参数再交给取文案的函数，
    // `services/l10n.rs` 那道门就扫不到它，将来谁改了这个键名，屏幕上念的是键名本身。
    match areas.library {
        Some(path) => rows.push(path_row(cx, "field.library_dir", path)),
        None => rows.push(named_note(cx, "field.library_dir", |cx| {
            tr(cx, "usage.library_unconfirmed")
        })),
    }
    let log_dir = areas
        .log_active
        .as_ref()
        .and_then(|active| active.parent().map(|dir| dir.to_path_buf()));
    match log_dir {
        Some(path) => rows.push(path_row(cx, "field.log_dir", path)),
        None => rows.push(named_note(cx, "field.log_dir", |cx| {
            tr(cx, "usage.logging_off")
        })),
    }
    rows
}

/// 「位置」那一组的搜索词。标题本身已经写着「数据目录」这类词，补的是英文那一种说法：
/// 这一行是无标题的自定义行，不带词就只剩页名能被搜到。
const FOLDER_FIND: &[&str] = &[
    "path",
    "folder",
    "directory",
    "open",
    "目录",
    "路径",
    "位置",
];

/// 一行「路径 + 打开所在文件夹」。
fn path_row(cx: &App, key: &'static str, path: PathBuf) -> Row {
    let title = tr(cx, key);
    let id = format!("storage-open-{key}");
    Row::custom_titled(title, move |cx| {
        let path = path.clone();
        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(muted_label(path.display().to_string(), cx)),
            )
            .child(open_folder_button(ElementId::from(id.clone()), path, cx))
            .into_any_element()
    })
    .keywords(FOLDER_FIND)
}

/// 同一行的标题，右边换成一句现成的话（没有路径可开的时候）。
fn named_note(cx: &App, key: &'static str, note: impl Fn(&App) -> SharedString + 'static) -> Row {
    Row::custom_titled(tr(cx, key), move |cx| {
        muted_label(note(cx), cx).into_any_element()
    })
    .keywords(FOLDER_FIND)
}

#[cfg(test)]
mod tests {
    //! 这一页能钉住的是**它自己不讲两套数**：环上那几片、明细那几行、清理那一组点名的
    //! 去处，全部得从同一份账里长出来。写错一格（比如明细漏一类、或那颗钮管的类别与行上
    //! 报的字节不是一回事）屏幕上看不出来，这一条看得出来。

    use super::*;
    use crate::ui::services::config::ConfigHolder;
    use crate::ui::services::storage::{Snapshot, StorageState};
    use folio_core::Config;
    use folio_core::infra::usage::{Entry, Usage};
    use gpui_kit::{AppContext as _, TestAppContext};

    /// 一份手搓的账：每一类都有一条，填进来的那几类带字节。
    ///
    /// 类别一条不缺是刻意的：[`Usage::default`] 的 `entries` 是**空表**，而
    /// [`folio_core::infra::usage::Usage::get`] 对缺项回零 —— 拿空表当夹具时「旧日志有 4 KB」
    /// 那句话根本落不进去，断言会绿在一个没被写到的分支上。
    fn ledger(pairs: &[(Area, u64)]) -> Usage {
        let mut entries: Vec<Entry> = Area::ALL
            .iter()
            .map(|area| Entry {
                area: *area,
                bytes: 0,
                files: 0,
            })
            .collect();
        for (area, bytes) in pairs {
            let entry = entries
                .iter_mut()
                .find(|entry| entry.area == *area)
                .expect("填进去的那一类必须在 `Area::ALL` 里");
            entry.bytes = *bytes;
            entry.files = 1;
        }
        Usage {
            total: entries.iter().map(|entry| entry.bytes).sum(),
            entries,
            unreadable: 0,
        }
    }

    /// 环与明细同源同序：`filled()` 那个从大到小的序，两处都走它。
    #[gpui_kit::test]
    fn the_ring_and_the_legend_share_one_order(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            let view = storage::entity(cx);
            view.update(cx, |state, _| {
                state.set_snapshot_for_test(Snapshot {
                    usage: ledger(&[
                        (Area::Books, 700),
                        (Area::Database, 300),
                        (Area::RotatedLogs, 100),
                    ]),
                    freeable: Some(64),
                })
            });
            let data = slices(cx);
            // 比的是**读者看到的那串字**，不是枚举值：这一列同时是环的 tooltip 与明细的行。
            assert_eq!(
                data.iter()
                    .map(|slice| slice.label.clone())
                    .collect::<Vec<_>>(),
                vec![
                    area_label(Area::Books, cx),
                    area_label(Area::Database, cx),
                    area_label(Area::RotatedLogs, cx),
                ],
                "环上的片序就是明细的行序，且零字节的那几类不占行"
            );
            assert_eq!(data[0].share, 700.0 / 1100.0);
            assert_eq!(data[0].color, slice_color(cx, 0), "第一片用第一档图表色");
            // 每一片都得有个名字，且九门都有这一行（`area_label` 那一串字面量是本页唯一
            // 的类别词表；漏一类时屏幕上会念成键名本身）。
            for slice in &data {
                assert!(!slice.label.is_empty(), "有一片没有名字");
                assert!(
                    !slice.label.starts_with("area."),
                    "文案键没落到表里，屏幕上会原样念它：{}",
                    slice.label
                );
            }
        });
    }

    /// 行上端出来的那个数，就是那颗钮管的量。
    ///
    /// 这条挡的是「行上写着 1.2 GB，点下去删的是别的东西」。`Vacuum` 单独一档：它那一行报
    /// 的是**空闲页**而不是库文件大小，因为那颗钮还回来的正是那一截；其余四档报的就是
    /// [`Job::area`] 那一类自己的字节。
    #[gpui_kit::test]
    fn each_clear_button_shows_the_very_number_it_acts_on(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            let view = storage::entity(cx);
            view.update(cx, |state, cx| {
                state.set_snapshot_for_test(Snapshot {
                    usage: ledger(&[
                        (Area::Books, 700),
                        (Area::Database, 300),
                        (Area::RotatedLogs, 100),
                    ]),
                    freeable: Some(64),
                });
                cx.notify();
            });
            let snapshot = snapshot_now(cx).expect("夹具该把账交回实体");
            assert_eq!(area_bytes(Area::Books, &snapshot), 700);
            assert_eq!(area_bytes(Area::RotatedLogs, &snapshot), 100);
            assert_eq!(
                area_bytes(Area::Database, &snapshot),
                64,
                "库里那一格报的是空闲页那一截，不是库文件总大小"
            );
            // 没统计出库时那一格回 0：宁可少报，也不把「查不到」念成一个能回收的数。
            let mut bare = snapshot.clone();
            bare.freeable = None;
            assert_eq!(area_bytes(Area::Database, &bare), 0);
        });
    }

    /// 五档动作的**落点各不一样**，而每一档都点名得起自己那一类。
    ///
    /// 界面上四行排下来长得一模一样，只有文案不同；把 `ClearLogs` 写成管 `Area::Logs`
    /// （当前那份）时屏幕上完全看不出来，删掉的却是正被 fd 攥着的那一份。
    /// 「没东西可清就不摆那颗钮」这条判据，是清理那一组行数的唯一来源。
    ///
    /// 四档缓存各自只在**那一类真有字节**时上榜，整库重写只在库里确实有空闲页时上榜；
    /// 全空时那一组收成一句「现在没什么可清的」。恒摆四颗钮的话，屏幕上就有一排按下去
    /// 什么也不变的控件（规范那条「每个动作都要说得出它的对象与结果」）。
    #[gpui_kit::test]
    fn the_cleanup_group_only_offers_what_actually_has_bytes(cx: &mut TestAppContext) {
        use crate::ui::features::settings::model::spec::RowKind;

        cx.update(gpui_kit::init);
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            let view = storage::entity(cx);

            // 全空：一句「没什么可清」，不是一排空钮。
            view.update(cx, |state, cx| {
                state.set_snapshot_for_test(Snapshot {
                    usage: ledger(&[]),
                    freeable: Some(0),
                });
                cx.notify();
            });
            let rows = cleanup_rows(cx);
            assert_eq!(rows.len(), 1, "全空时那一组只该剩一句话");
            assert!(
                matches!(rows[0].kind, RowKind::Custom { title: None, .. }),
                "那一句是无标题的自定义行"
            );

            // 只有旧日志有东西：一颗钮。空闲页是 0，所以「整理数据库」那颗不该上来。
            view.update(cx, |state, cx| {
                state.set_snapshot_for_test(Snapshot {
                    usage: ledger(&[(Area::RotatedLogs, 4096), (Area::Books, 9_000)]),
                    freeable: Some(0),
                });
                cx.notify();
            });
            assert_eq!(
                cleanup_rows(cx).len(),
                1,
                "只有旧日志那档有东西，就该只有一颗钮"
            );

            // 库里出现空闲页：整库重写那颗跟着上来，一共两颗。
            view.update(cx, |state, cx| {
                let mut snapshot = state.snapshot_for_test().expect("上一笔账还在");
                snapshot.freeable = Some(64);
                state.set_snapshot_for_test(snapshot);
                cx.notify();
            });
            assert_eq!(
                cleanup_rows(cx).len(),
                2,
                "有空闲页时那颗「整理数据库」该跟着上来"
            );

            // 没统计过时那一组连一句「没什么可清」都不摆：那是一句它没资格说的话。
            let fresh = cx.new(|_| StorageState::default());
            cx.set_global(storage::StorageHolder(fresh));
            assert!(cleanup_rows(cx).is_empty(), "还没有账时那一组是空的");
        });
    }

    #[gpui_kit::test]
    fn the_five_clear_jobs_point_at_five_different_areas(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            let jobs = [
                Job::ClearThumbs,
                Job::ClearComics,
                Job::ClearPartials,
                Job::ClearLogs,
                Job::Vacuum,
            ];
            let areas: Vec<Area> = jobs.iter().filter_map(|job| job.area()).collect();
            assert_eq!(areas.len(), 5, "五档动作都该管着一类");
            assert_eq!(
                areas.iter().collect::<std::collections::HashSet<_>>().len(),
                5,
                "两档撞进同一类，界面上就是两颗管同一格的钮"
            );
            for job in jobs {
                let label = action_label(job, cx);
                assert!(!label.is_empty(), "{job:?} 没有按钮文字");
                assert!(
                    !label.starts_with("action."),
                    "{job:?} 的文案键没落到表里：{label}"
                );
                // 行上那句说明 + 面板里那句后果：两条都得真落到表里，否则屏幕上原样念键名。
                let hint = action_hint(job, cx);
                assert!(
                    !hint.is_empty() && !hint.starts_with("action."),
                    "{job:?} 的说明没落到表里：{hint}"
                );
                let body = confirm_body(job, cx, 1024);
                assert!(
                    !body.is_empty() && !body.starts_with("confirm."),
                    "{job:?} 的后果那句没落到表里：{body}"
                );
                assert!(
                    body.contains('1'),
                    "{job:?} 的后果那句没把要清掉多少念出来：{body}"
                );
            }
            // 重扫不动任何东西，所以它没有确认面板、也没有类别。
            assert_eq!(Job::Rescan.area(), None);
        });
    }

    /// 「成色」那两句的判据：账完整时那一格里不摆这一段，有读不进去的处数时才长出来。
    ///
    /// 这条钉的是真机上看见过的一个坏状态：那一行没话可说却还摆着，卡里就是两条发丝线中间
    /// 夹一段空白。整块收成一格之后行数不再说话，所以判据本身得有人问一遍。
    #[gpui_kit::test]
    fn the_note_only_shows_when_the_ledger_is_incomplete(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            let view = storage::entity(cx);
            // 一本完整的账。
            view.update(cx, |state, cx| {
                state.set_snapshot_for_test(Snapshot {
                    usage: ledger(&[(Area::Books, 700)]),
                    freeable: None,
                });
                cx.notify();
            });
            assert!(!has_note(cx), "账完整时那一段没有话可说");
            // 有处读不进去：那一句要说「上面的数是下界」。
            let mut incomplete = ledger(&[(Area::Books, 700)]);
            incomplete.unreadable = 2;
            view.update(cx, |state, cx| {
                state.set_snapshot_for_test(Snapshot {
                    usage: incomplete,
                    freeable: None,
                });
                cx.notify();
            });
            assert!(has_note(cx), "读不进去的处数得有人说出来");
            // 「占用」那一组是一整格，不是四行：四行会在卡里长出三条发丝线。
            assert_eq!(usage_rows().len(), 1, "读数、环、明细得长在同一个卡位里");
        });
    }

    /// 占比那一格：非零但不足 1% 时不许塌成 `0%`。
    #[gpui_kit::test]
    async fn a_share_under_one_percent_still_says_something(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| ConfigHolder::install(Config::default(), cx));
        let shown = |share: f32| cx.read(|cx| percent(cx, share).to_string());
        assert_eq!(shown(0.0), "0%");
        assert_eq!(shown(0.005), "<1%");
        assert_eq!(shown(0.42), "42%");
        assert_eq!(shown(1.0), "100%");
        // 整数位的百分比在逗号小数那几门里不该出现小数点，也不该被分组分隔符咬到。
        cx.update(|cx| {
            ConfigHolder::edit_language(cx, |language| {
                language.locale = folio_core::infra::i18n::Locale::De
            });
        });
        assert_eq!(shown(0.42), "42%");
        assert_eq!(shown(0.0), "0%");
    }
}
