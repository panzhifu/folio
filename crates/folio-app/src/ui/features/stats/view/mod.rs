//! 「统计」目的地：这一年什么时候读了书、读得有多密。
//!
//! 整屏只有一张图 —— 一格一天、按日历排开的热力图（GitHub 贡献图那个形状）。
//! 按 `Design Guides` 的「Guidance for AI-generated interfaces」先把四件事说清楚：
//!
//! - **主任务**：读者要的是「我的习惯长什么样」，那是**形状**而不是某个数，所以图占整屏，
//!   眉标、图例、结论各一行小字围着它，不并列第二块内容。
//! - **状态归谁**：快照归 [`StatsView`]，取数只在 [`StatsView::refresh`] 里（切进这一屏、
//!   关掉一本书那两处，由外壳叫），`render` 一律不碰库。
//! - **组件构成**：`component::chart` 那几张是柱、线、雷达，没有按日历排的网格，所以这是
//!   一块**只用 div 的组合**（一列车标 + 一排月标签 + 53 列 × 7 行格子 + 图例）。
//!   不自定义 `Element`：这里没有需要一次画完的东西，交给布局引擎反而能保证那些格子
//!   与车道是真的对齐的。
//! - **键盘通路**：这一屏没有任何动作（旧版榜单那一行「点开一本书」随榜单一起没了），
//!   所以不需要键盘路径，能滚就行。
//!
//! 三条从旧屏继承下来、并且仍然成立的规矩：
//!
//! 1. **图的下面永远跟一句 muted 的结论**。移动端的浮层点不住（`shelf` 里记过的那条事实），
//!    读数通路不能只剩 tooltip —— 这一屏连 tooltip 都没有，那一句就是唯一的数。
//! 2. **不许只靠颜色传义**：深浅之外，「读了多少天、累计多久、最长连续几天」由结论那一行说。
//! 3. **页面标题不写「统计」二字**：侧栏 / 底栏那条导航已经把这格点亮了，在内容区再重复一遍
//!    是 `Let context carry context` 点名要避免的废话。眉标写的是**窗口**（看回去多远），
//!    那才是导航没说的那半句。

use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Hsla, InteractiveElement, IntoElement, ParentElement, Pixels, Render,
    SharedString, Styled, TestSupportExt as _, Window, div, px,
};

use super::model::heatmap::{Heatmap, ROWS, WEEKS, duration};
use crate::ui::components::muted_label;
use crate::ui::services::app_store;
use crate::ui::services::l10n::{tf, tr};
use crate::ui::shell::navigation::{RAIL_WIDTH, SIDEBAR_WIDTH, SizeClass};

/// 内容列的最大宽度（逻辑像素）。报告类内容拉到 1600 宽会把格子拉成一片沙漠。
const COLUMN_MAX: f32 = 720.0;
/// 左右留白，与书架共用同一个数：切目的地时那条对齐脊线不该跳。
const GUTTER: f32 = 12.0;
/// 一格边长的下限。再小就看不出是一格一格的了（371 格糊成一片色面）。
const CELL_MIN: f32 = 8.0;
/// 一格边长的上限。一年 53 列，宽到某个点再放大只是让方块变大，读不出更多信息，
/// 所以封顶在一颗舒适的方块上，多出来的宽度还给两侧的留白。
const CELL_MAX: f32 = 14.0;
/// 格与格的缝。
const CELL_GAP: f32 = 3.0;
/// 左边那列车标（周一 / 周三 / 周五）的车道宽。
const LABEL_LANE: f32 = 28.0;
/// 月标签那一行的车道高：一行 `text_xs` 的字，加上下各一点余量。
const MONTH_LANE: f32 = 18.0;
/// 只标这三行星期几（GitHub 同一处置）。七行全标会把车道撑成图的一半宽。
const ROW_LABELS: [(usize, &str); 3] = [
    (0, "stats.weekday_mon"),
    (2, "stats.weekday_wed"),
    (4, "stats.weekday_fri"),
];

/// 五档的颜色。
///
/// 0 档取 `muted`（那是这一屏「没有内容」的颜色，旧版榜单那条占比栏的轨道同一个色），
/// 1..4 档是**同一个**图表色递进不透明度，而不是 `chart_1..chart_4`：那四个是分类色板，
/// 摆在一起读出来是四种东西，不是一件事的四个程度。
fn ramp(cx: &App) -> [Hsla; 5] {
    let solid = cx.theme().chart_3;
    [
        cx.theme().muted,
        solid.opacity(0.30),
        solid.opacity(0.55),
        solid.opacity(0.80),
        solid,
    ]
}

/// 一格该多大：把可用的宽度均给 53 列，再夹进 [`CELL_MIN`]..=[`CELL_MAX`]。
///
/// 均出来小于下限时**不缩了**，那一格按 `CELL_MIN` 画，多出来的部分横向滚
/// （GitHub 在手机上是同一处置）。窄屏硬缩到 5 像素的结果是整张图糊成一块色面，
/// 那不如能滚。
fn cell_size(width: f32, size: SizeClass) -> f32 {
    let nav = match size {
        SizeClass::Compact => 0.0,
        SizeClass::Medium => RAIL_WIDTH,
        SizeClass::Expanded => SIDEBAR_WIDTH,
    };
    let inset = GUTTER * 2.0 + LABEL_LANE;
    let room = (width - nav - inset).max(0.0);
    // 宽屏那一列被 `COLUMN_MAX` 夹住，格子也就按夹住后的宽度算。
    let room = match size {
        SizeClass::Expanded => room.min(COLUMN_MAX - inset),
        _ => room,
    };
    let pitch = ((room + CELL_GAP) / WEEKS as f32).floor();
    (pitch - CELL_GAP).clamp(CELL_MIN, CELL_MAX)
}

#[derive(Default)]
pub struct StatsView {
    /// `None` = 书库读不出来（没装上，或那条查询失败）。
    /// 这一格要说的是「读不到」，与「库里没有阅读记录」是两句话，别混。
    heatmap: Option<Heatmap>,
}

impl StatsView {
    pub fn new() -> Self {
        Self { heatmap: None }
    }

    /// 重取一次快照。切进这一屏、关掉一本书都走这里。
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(store) = app_store::handle(cx) else {
            self.heatmap = None;
            cx.notify();
            return;
        };
        self.heatmap = Some(Heatmap::load(&store, crate::ui::host::local_offset_secs()));
        cx.notify();
    }

    /// 现在手上有没有一份快照。外壳那扇回归门用它钉「切进这一屏确实取到了一次」。
    pub fn has_heatmap(&self) -> bool {
        self.heatmap.is_some()
    }

    /// 窗口内有读的天数；没有快照时是 `None`（那表示「还没去看」，不是「库里是零」）。
    /// 外壳那条「关掉一本书之后统计要跟上」的门钉的是这个数变了。
    pub fn active_days(&self) -> Option<usize> {
        self.heatmap.as_ref().map(|map| map.active_days)
    }
}

impl Render for StatsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let size = SizeClass::of(window);
        let wide = size == SizeClass::Expanded;
        let cell = cell_size(f32::from(window.viewport_size().width), size);
        let body: AnyElement = match &self.heatmap {
            None => self.unavailable(cx),
            Some(heatmap) => self.block(heatmap, cell, cx),
        };
        let column = v_flex()
            .w_full()
            .when(wide, |el| el.max_w(px(COLUMN_MAX)))
            .px(px(GUTTER))
            .pb(px(GUTTER))
            .child(body);
        v_flex()
            .id("folio-stats")
            // 外壳那扇回归门要按 id 认出这一屏画没画出来。非 `test-support` 构建里
            // 这一层是透明的（`Observed` 原样转交内部元素），真机不多花一个节点。
            .test_support()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                // 滚动这一层归组件（与 `shelf::grid` 同一条理由）：它拿住 ScrollHandle、
                // 把滚轴画在这块区域真正的边沿上。里面的那一层只负责居中。
                div()
                    .id("stats-scroll")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scrollbar()
                    .child(v_flex().w_full().items_center().child(column)),
            )
            .into_any_element()
    }
}

impl StatsView {
    /// 一张图，上面一个说清窗口的眉标，下面图例与结论各一行。
    ///
    /// 交回 `AnyElement` 而不是 `impl IntoElement`：那个不透明类型会把 `cx` 的可变借用
    /// 一直带在类型里，后面每一处 `cx.theme()` 都撞在它上面（旧版 `range_bar` 那处
    /// 记过同一个坑）。
    fn block(&self, map: &Heatmap, cell: f32, cx: &mut Context<Self>) -> AnyElement {
        let fills = ramp(cx);
        v_flex()
            .w_full()
            .gap_2()
            .pt_2()
            .child(muted_label(tr(cx, "stats.window_year"), cx))
            .child(grid(map, cell, &fills, cx))
            .child(legend(&fills, cx))
            .child(note(map, cx))
            .into_any_element()
    }

    /// 读不到书库。这一句与「还没有阅读记录」必须分得开：前者是这台设备上取不到数据源，
    /// 后者是读者真的还没读 —— 并成一句「暂无数据」，读者会以为是自己把书读空了
    /// （`components::empty_state` 顶上立的就是这条规矩）。
    fn unavailable(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .w_full()
            .gap_1()
            .pt_2()
            .child(div().child(tr(cx, "stats.no_store")))
            .child(muted_label(tr(cx, "stats.no_store_hint"), cx))
            .into_any_element()
    }
}

/// 网格本体：左边那列车标固定不动，右边那 53 列放不下时横向滚。
///
/// 月标签用**绝对定位**摆在同一根 pitch 上，而不是排成一行 div：一列只有十来像素，
/// 标签比那一格宽就会被自己的车道切掉；而 `left = 列号 × pitch` 与下面那些格子是同一个
/// 算式 —— 对齐因此是算出来的，不是碰巧排上的。
fn grid(map: &Heatmap, cell: f32, fills: &[Hsla; 5], cx: &App) -> AnyElement {
    let pitch = cell + CELL_GAP;
    let radius = cx.theme().radius_tokens().sm;
    let labels = map.months.iter().map(|label| {
        div()
            .absolute()
            .top_0()
            .left(px(label.column as f32 * pitch))
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(month_label(label.month, label.year, cx))
            .into_any_element()
    });
    let rows = (0..ROWS).map(|row| {
        h_flex()
            .gap(px(CELL_GAP))
            .children(
                map.columns
                    .iter()
                    .map(|column| cell_box(&column[row], cell, radius, fills)),
            )
            .into_any_element()
    });
    h_flex()
        .id("stats-heatmap")
        // 外壳那扇回归门按这个 id 认「图真的画出来了」，而不只是「那一屏的根在」。
        .test_support()
        .w_full()
        .items_start()
        .gap_2()
        .child(row_labels(cell, cx))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().relative().w_full().h(px(MONTH_LANE)).children(labels))
                .child(
                    div().w_full().overflow_x_scrollbar().child(
                        v_flex().gap(px(CELL_GAP)).child(
                            // 内容宽度按 53 列的 pitch 给死：横向滚的那一层要知道自己
                            // 比窗口宽，而 `w_full` 在这里只会跟着窗口走。
                            div()
                                .w(px(map.columns.len() as f32 * pitch - CELL_GAP))
                                .flex()
                                .flex_col()
                                .gap(px(CELL_GAP))
                                .children(rows),
                        ),
                    ),
                ),
        )
        .into_any_element()
}

/// 一格。窗口外的那几格不画底色：它们是补来对齐星期几的，不是数据。
fn cell_box(
    cell: &super::model::heatmap::Cell,
    size: f32,
    radius: Pixels,
    fills: &[Hsla; 5],
) -> AnyElement {
    let slot = div().size(px(size)).rounded(radius);
    if cell.filled {
        slot.bg(fills[cell.level.min(4)]).into_any_element()
    } else {
        slot.into_any_element()
    }
}

/// 左边那列车标。行高与格子的 pitch 对齐（`cell + CELL_GAP`），否则第三行的字会
/// 慢慢爬到第四行的格子身上。
fn row_labels(cell: f32, cx: &App) -> AnyElement {
    v_flex()
        .w(px(LABEL_LANE))
        .pt(px(MONTH_LANE))
        .gap(px(CELL_GAP))
        .children((0..ROWS).map(|row| {
            let key = ROW_LABELS
                .iter()
                .find(|(at, _)| *at == row)
                .map(|(_, key)| *key);
            div()
                .flex()
                .items_center()
                .h(px(cell + CELL_GAP))
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(key.map(|key| tr(cx, key)).unwrap_or_default())
                .into_any_element()
        }))
        .into_any_element()
}

/// 图例：五档方块 + 两端那两个字，右对齐（它是图的注，不是一行内容）。
///
/// 摆「少 / 多」而不是给每档写一个分钟数：五档各带一个数会把这一行变成第二张图，
/// 而读者要问的只是「深的比浅的多」。真正那个数在结论那一行里。
fn legend(fills: &[Hsla; 5], cx: &App) -> AnyElement {
    let radius = cx.theme().radius_tokens().sm;
    h_flex()
        .w_full()
        .justify_end()
        .items_center()
        .gap_1()
        .child(muted_label(tr(cx, "stats.legend_less"), cx))
        .children(fills.iter().map(|color| {
            div()
                .size(px(CELL_MAX))
                .rounded(radius)
                .bg(*color)
                .into_any_element()
        }))
        .child(muted_label(tr(cx, "stats.legend_more"), cx))
        .into_any_element()
}

/// 图下面那一句结论。全零时说的是另一句话。
fn note(map: &Heatmap, cx: &App) -> AnyElement {
    let text = if map.is_empty() {
        tr(cx, "stats.none_yet")
    } else {
        SharedString::from(tf(
            cx,
            "stats.heatmap_note",
            &[
                ("days", &map.active_days),
                ("total", &duration(cx, map.seconds).to_string()),
                ("streak", &map.longest_streak),
            ],
        ))
    };
    muted_label(text, cx).into_any_element()
}

/// 月标签的文字。一月那一列报**年**而不是报「1 月」：这张图横看一年，跨年处最容易
/// 读错的正是「这是哪一年」。
fn month_label(month: i32, year: i32, cx: &App) -> SharedString {
    if month == 1 {
        SharedString::from(year.to_string())
    } else {
        SharedString::from(tf(cx, "stats.month", &[("month", &month)]))
    }
}
