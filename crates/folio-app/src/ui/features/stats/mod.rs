//! 「统计」目的地：读者这一段时间读得怎么样。
//!
//! 形状按 `Design Guides` 的「先写任务再摆控件」量出来，而不是一张统计卡片墙：
//!
//! - 全屏只有「今天读了多久」这一个大字号读数。其余的数都比它小一档，因为读者点开
//!   这一屏第一个要回答的问题就是那一个（`Design the primary task before distributing
//!   controls`）。
//! - 唯一的动作是榜单那一行 —— 打开那本书。整屏是报告，不是操作台，所以没有第二颗按钮。
//! - 页面标题不写「统计」二字：侧栏 / 底栏那条导航已经把这格点亮了，在内容区再重复一遍
//!   是 `Let context carry context` 点名要避免的废话。
//! - 桌面 `Expanded` 用定宽居中列而不是拉满：报告类内容拉到 1600 宽会把柱子拉成一片沙漠，
//!   而「今天读了多久」那一句在满幅下会散成三个字那么宽。
//!
//! 每一节都直接收 [`Report`] 而不是自己去 `self.report` 里取：取不到书库时整屏换成的
//! 那一句话由 `render` 一处决定，各节里不必各写一条永远走不到的 `None` 分支。
//! 取数不在 `render` 里，见 [`report`] 那一份的模块说明。

mod report;

pub use report::Range;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::chart::BarChart;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme, Sizable as _, Selectable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Corners, ElementId, EventEmitter, FontWeight, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    TestSupportExt as _, Window, div, px,
};

use crate::ui::shared::app_store;
use crate::ui::shared::l10n::{tf, tr};
use crate::ui::shell::navigation::SizeClass;
use crate::ui::widgets::{hairline, muted_label};
use report::{Bar, Report};

/// 内容列的最大宽度（逻辑像素）。
///
/// 这里是裸 `px` 而不是 rem 档：它是一块**版心**，与阅读页的版心同性质 —— 量的是
/// 「一行多长还读得下去」，不该随界面缩放一起放大（Guides 允许的那类几何边界）。
const COLUMN_MAX: f32 = 720.0;
/// 左右留白，与书架共用同一个数：切目的地时那条对齐脊线不该跳。
const GUTTER: f32 = 12.0;
/// 图表盒的高。
///
/// 必须显式给：图表不自己量高，而它在滚动层里拿不到能被 `flex_1` 解析的高 ——
/// 不给就是零高，症状是「趋势那一节只剩标题，图不见了」。与阅读页目录层同一条坑
/// （`reader::page::toc_layer` 那处注释）。
const TREND_HEIGHT: f32 = 168.0;
/// 榜单行里那条占比栏的固定车道宽。
const SHARE_LANE: f32 = 96.0;
/// 榜单行里读数那一列的车道宽（右对齐，让竖着比的数是齐的）。
const READOUT_LANE: f32 = 96.0;
/// 时段热力条一格的高。
const HOUR_CELL_HEIGHT: f32 = 14.0;

/// 读者此刻在这块屏上要回答的问题，按阅读顺序排下来。
#[derive(Clone, Debug)]
pub enum StatsEvent {
    /// 从榜单那一行打开一本书。外壳收到之后走**它已有的**那条 `open_reading`，
    /// 与书架点开一本是同一个动作 —— 不为这一屏新造一条路由。
    Open(i64),
}

#[derive(Default)]
pub struct StatsView {
    /// 默认档由 [`Range::default`] 定，不在这里再写一遍那个数。
    range: Range,
    /// `None` = 书库读不出来（没装上，或那几条查询全失败）。
    /// 这一格要说的是「读不到」，与「库里没有阅读记录」是两句话，别混。
    report: Option<Report>,
}

impl StatsView {
    pub fn new() -> Self {
        Self {
            range: Range::Fourteen,
            report: None,
        }
    }

    /// 重取一次快照。切进这一屏、关掉一本书、换档位都走这里。
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(store) = app_store::handle(cx) else {
            self.report = None;
            cx.notify();
            return;
        };
        self.report = Some(Report::load(
            &store,
            self.range,
            crate::ui::host::local_offset_secs(),
            cx,
        ));
        cx.notify();
    }

    /// 现在手上有没有一份快照。外壳那几条回归门用它钉「切进这一屏确实重取了一次」。
    pub fn has_report(&self) -> bool {
        self.report.is_some()
    }

    /// 窗口内的段数；没有快照时是 `None`（那表示「还没去看」，不是「库里是零」）。
    pub fn window_sessions(&self) -> Option<i64> {
        self.report.as_ref().map(|report| report.window.sessions)
    }

    /// 换窗口档位：立刻重取。留着旧快照等于画一张过期的图 —— 那一排按钮问的就是窗口。
    fn pick(&mut self, range: Range, cx: &mut Context<Self>) {
        if range == self.range {
            return;
        }
        self.range = range;
        self.refresh(cx);
    }

    /// 「今天」那一块：全屏唯一的焦点。
    fn today(&self, report: &Report, cx: &mut Context<Self>) -> AnyElement {
        let today = report.window.seconds_at_today();
        // 三种话说三条路：今天读了 ⇒ 报连续天数；今天还没读但读过别的 ⇒ 说「今天还没读」；
        // 整库空的 ⇒ 说「还没有连续记录」，而不是「今天还没开始读」那种把人绕进去的话。
        let second = if today > 0 {
            match report.window.streak {
                0 => tr(cx, "stats.streak_none"),
                days => SharedString::from(tf(cx, "stats.streak", &[("days", &days)])),
            }
        } else if report.is_empty() {
            tr(cx, "stats.streak_none")
        } else {
            tr(cx, "stats.today_none")
        };
        v_flex()
            .w_full()
            .gap_1()
            .child(section_label(tr(cx, "stats.today"), cx))
            .child(
                div()
                    .text_3xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(report::duration(cx, today)),
            )
            .child(muted_label(second, cx))
            .into_any_element()
    }

    /// 趋势：一根柱子一天，今天那根更实。
    ///
    /// 「哪根是今天」有两处说明而不是只有颜色（Guides：不许只靠颜色传义）：tooltip 里
    /// 写着完整日期，而图下面那一行 muted 直接把窗口结论念出来。后一条尤其要紧 ——
    /// 移动端的浮层点不住（`shelf` 里记过的那条事实），tooltip 不能是唯一那条读数通路。
    fn trend(&self, report: &Report, cx: &mut Context<Self>) -> AnyElement {
        let bars: Vec<Bar> = report.bars.clone();
        let solid = cx.theme().chart_3;
        let faded = solid.opacity(0.45);
        let radius = cx.theme().radius;
        let days = self.range.days();
        let note = SharedString::from(tf(
            cx,
            "stats.trend_note",
            &[
                ("days", &days),
                (
                    "total",
                    &report::duration(cx, report.window.seconds).to_string(),
                ),
                (
                    "avg",
                    &report::duration(cx, report.window.average_per_active_day()).to_string(),
                ),
            ],
        ));
        section(
            tr(cx, "stats.trend"),
            v_flex()
                .w_full()
                .gap_2()
                .child(
                    div()
                        .w_full()
                        .h(px(TREND_HEIGHT))
                        .child(
                            BarChart::new(bars)
                                .band(|bar| bar.band.clone())
                                .value(|bar| bar.minutes)
                                .name(tr(cx, "stats.minutes_unit"))
                                .fill(move |bar: &Bar, _, _, _| {
                                    if bar.today { solid } else { faded }
                                })
                                // 柱子从轴线上长出来，底角不该是圆的 —— 与阅读页顶栏
                                // 「只圆下面两个角」是同一条判断。
                                .corner_radii(Corners {
                                    top_left: radius,
                                    top_right: radius,
                                    bottom_left: px(0.),
                                    bottom_right: px(0.),
                                })
                                // 没读的那几天留一小截：`reading_summary` 会把它们补零，
                                // 而零高的柱子与「轴上根本没有这一格」看起来一模一样。
                                .min_length(2.)
                                .value_axis(true)
                                .value_tick_count(3)
                                .value_tick_format(|v| format!("{}", v.round() as i64))
                                // 窗口越宽标得越稀：30 天每根都标，日期会叠成一团。
                                .tick_margin(if days >= 30 { 3 } else { 1 })
                                .tooltip_title(|bar| bar.day.clone())
                                .tooltip_value(|bar: &Bar, _| bar.readout.clone())
                                .appear_key(days),
                        ),
                )
                .child(muted_label(note, cx)),
            cx,
        )
        .into_any_element()
    }

    /// 累计：四个全时段的读数。不跟着窗口档位变 —— 「累计」跟着筛就不是累计了。
    fn totals(&self, report: &Report, cx: &mut Context<Self>) -> AnyElement {
        let cells = [
            (
                tr(cx, "stats.total_time"),
                report::duration(cx, report.totals.seconds),
            ),
            (
                tr(cx, "stats.total_days"),
                SharedString::from(report.days_read.to_string()),
            ),
            (
                tr(cx, "stats.total_books"),
                SharedString::from(report.totals.books.to_string()),
            ),
            (
                tr(cx, "stats.total_notes"),
                SharedString::from(report.notes.to_string()),
            ),
        ];
        section(
            tr(cx, "stats.total"),
            div()
                .w_full()
                .grid()
                .grid_cols(2)
                .gap_4()
                .children(cells.into_iter().map(|(label, value)| {
                    v_flex()
                        .min_w_0()
                        .gap_1()
                        .child(div().text_lg().child(value))
                        .child(muted_label(label, cx))
                })),
            cx,
        )
        .into_any_element()
    }

    /// 时间花在哪些书上。一行一本，点一下就去读它。
    fn ranks(&self, report: &Report, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity().clone();
        let total = (report.window.seconds.max(1)) as f64;
        let track = cx.theme().muted;
        let fill = cx.theme().chart_3;
        let rows = report
            .ranks
            .iter()
            .enumerate()
            .map(|(ix, rank)| {
                let book_id = rank.book_id;
                let ratio = (rank.seconds as f64 / total).clamp(0.0, 1.0) as f32;
                let page = view.clone();
                div()
                    .id(ElementId::named_usize("stats-rank", ix))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_3()
                    .py_1()
                    // 一行是一个可点的去处，所以它是行不是卡片：hover 只给一点点底色，
                    // 点击的去处由栈自己说明（按完这本书就回到阅读页）。
                    .cursor_pointer()
                    .hover(|style| style.bg(track))
                    .on_click(move |_, _, cx| {
                        page.update(cx, |_, cx| cx.emit(StatsEvent::Open(book_id)))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_ellipsis()
                            .child(rank.title.clone()),
                    )
                    .child(
                        div()
                            .w(px(SHARE_LANE))
                            .flex_none()
                            .child(
                                Progress::new(ElementId::named_usize("stats-share", ix))
                                    .value(ratio)
                                    .color(fill),
                            ),
                    )
                    .child(
                        // 可比的数字右对齐：读者是竖着比这几本书的。
                        div()
                            .w(px(READOUT_LANE))
                            .flex_none()
                            .text_right()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(report::duration(cx, rank.seconds)),
                    )
                    .into_any_element()
            })
            .collect::<Vec<AnyElement>>();
        section(
            tr(cx, "stats.top"),
            v_flex().w_full().children(rows),
            cx,
        )
        .into_any_element()
    }

    /// 一般几点读。24 格深浅 + 一句结论（结论是那两格颜色说不出来的话）。
    fn hours(&self, report: &Report, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        // 四档深浅全取主题的图表色板：`chart_1` 最浅、`chart_4` 最深，本身就是同一支
        // 蓝色的四个台阶，而空的那格退回中性 `muted` —— 不引入任何新颜色，浅深两套主题
        // 都跟着主题走。
        let (empty, low, mid, high) = (theme.muted, theme.chart_1, theme.chart_3, theme.chart_4);
        let peak = (report.hours_peak).max(1) as f64;
        let cells = report
            .hours
            .iter()
            .map(|seconds| {
                let share = *seconds as f64 / peak;
                let bg = if share <= 0.0 {
                    empty
                } else if share < 0.34 {
                    low
                } else if share < 0.67 {
                    mid
                } else {
                    high
                };
                div()
                    .flex_1()
                    .min_w_0()
                    .h(px(HOUR_CELL_HEIGHT))
                    .bg(bg)
            })
            .collect::<Vec<_>>();
        let busiest = report
            .hours
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.cmp(b.1))
            .map_or(0, |(hour, _)| hour);
        let note = SharedString::from(tf(cx, "stats.hours_peak", &[("hour", &busiest)]));
        section(
            tr(cx, "stats.hours"),
            v_flex()
                .w_full()
                .gap_2()
                .child(h_flex().w_full().gap_1().children(cells))
                .child(muted_label(note, cx)),
            cx,
        )
        .into_any_element()
    }

    /// 读不到书库。这一句与「还没有阅读记录」必须分得开：前者是这台设备上取不到数据源，
    /// 后者是读者真的还没读 —— 并成一句「暂无数据」，读者会以为是自己把书读空了
    /// （`widgets::empty_state` 顶上立的就是这条规矩）。
    fn unavailable(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .w_full()
            .gap_1()
            .child(div().child(tr(cx, "stats.no_store")))
            .child(muted_label(tr(cx, "stats.no_store_hint"), cx))
            .into_any_element()
    }

    /// 顶栏那一排档位。三颗 ghost 按钮带 `selected` 态，与阅读页底栏那一排入口同一种
    /// 控件：这一档不是页面导航，是这一屏的筛选，所以不长成标签页。
    ///
    /// 库里读不出东西时整排不画：没有可筛的数据，就不该有一排按下去什么也不变的按钮
    /// （Guides：每个动作都要说得出它的对象、范围，以及点下去之后哪一屏会变）。
    ///
    /// 交回 `AnyElement` 而不是 `impl IntoElement`：那个不透明类型会把 `cx` 的可变借用
    /// 一直带在类型里，后面每一处 `cx.theme()` 都撞在它上面（与 `reader::page::toc_layer`
    /// 那处是同一个坑）。
    fn range_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.report.as_ref()?;
        let view = cx.entity().clone();
        let current = self.range;
        Some(
            h_flex()
                .w_full()
                .gap_1()
                .children(Range::ALL.into_iter().map(|range| {
                    let page = view.clone();
                    let label = range.label(cx);
                    Button::new(ElementId::from(format!("stats-range-{}", range.days())))
                        .ghost()
                        .small()
                        .selected(current == range)
                        .label(label.to_string())
                        .accessibility_label(label)
                        .on_click(move |_, _, cx| {
                            page.update(cx, |this, cx| this.pick(range, cx))
                        })
                }))
                .into_any_element(),
        )
    }
}

impl EventEmitter<StatsEvent> for StatsView {}

impl Render for StatsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let wide = matches!(SizeClass::of(window), SizeClass::Expanded);
        let blocks: Vec<AnyElement> = match &self.report {
            None => vec![self.unavailable(cx)],
            Some(report) => {
                let mut blocks = vec![self.today(report, cx), self.trend(report, cx)];
                // 累计不跟着档位变，但它是「读了多少」这个总账，仍排在趋势之后。
                blocks.push(self.totals(report, cx));
                // 两节按数据有没有说话决定画不画：一本的书名在「今天」附近已经出现过，
                // 一张两个夜晚的时段图读起来是噪声而不是规律。
                if report.shows_ranks() {
                    blocks.push(self.ranks(report, cx));
                }
                if report.shows_hours() {
                    blocks.push(self.hours(report, cx));
                }
                blocks
            }
        };
        let range_bar = self.range_bar(cx);
        // 节与节之间那条发丝线：它们同为 `background` 色，没有边界感，靠这一条分上下
        // （`widgets::hairline` 的用法说明）。第一节不需要 —— 它上面那排档位按钮的底边
        // 就是它的上边界，再画一条就是两条并排的实线。
        let mut stacked: Vec<AnyElement> = Vec::with_capacity(blocks.len() * 2);
        for (ix, block) in blocks.into_iter().enumerate() {
            if ix > 0 {
                stacked.push(hairline(cx).into_any_element());
            }
            stacked.push(block);
        }
        let column = v_flex()
            .w_full()
            .when(wide, |el| el.max_w(px(COLUMN_MAX)))
            .px(px(GUTTER))
            .pb(px(GUTTER))
            .gap_6()
            .children(stacked);
        v_flex()
            .id("folio-stats")
            // 外壳那扇回归门要按 id 认出这一屏画没画出来。非 `test-support` 构建里
            // 这一层是透明的（`Observed` 原样转交内部元素），真机不多花一个节点。
            .test_support()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .when_some(range_bar, |el, bar| {
                el.child(
                    h_flex()
                        .id("stats-ranges")
                        .test_support()
                        .w_full()
                        .px(px(GUTTER))
                        .pt_2()
                        .pb_1()
                        .child(bar),
                )
            })
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

/// 一个小节的标题：muted 的小字，不是大标题。
///
/// 这一屏的主角是数，标题只负责分组。用尺寸与间距分层而不是加色、加容器，
/// 是 `Hierarchy` 那条（"use size, weight, spacing, and separators before adding color
/// or containers"）。
fn section_label(title: SharedString, cx: &App) -> impl IntoElement {
    muted_label(title, cx)
}

/// 一节 = 一个 muted 小标题 + 内容。标题与内容之间 `gap_2`（一组之内），
/// 节与节之间那条 `gap_6` 与发丝线由调用点给。
fn section(title: SharedString, content: impl IntoElement, cx: &App) -> impl IntoElement {
    v_flex()
        .w_full()
        .gap_2()
        .child(section_label(title, cx))
        .child(content)
}
