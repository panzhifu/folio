//! 「统计」那一屏的七个区块：今天、趋势、合计、排行、时段、星期、取不到书库。
//!
//! 它们都是 `impl StatsView` 的一块，读的是同一份 `Report` 快照 —— 拆出来是为了让
//! `mod.rs` 只剩「什么时候画哪几块」。这一屏的主角是数，每个区块的判据只有一条：
//! 图说的是形状，图下面那句 muted 说的是形状里唯一要记住的那个数。

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::chart::{BarChart, RadarChart};
use gpui_kit::component::plot::AxisLabelPlacement;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{ActiveTheme, Selectable, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, IntoElement, ParentElement, SharedString, Styled, WeakEntity, px,
    relative, rems,
};

use super::super::model::report::{Bar, HourCell, Report, WeekdayPattern, WeekdaySpoke};
use super::*;
use crate::ui::components::muted_label;
use crate::ui::services::l10n::{tf, tr};


impl StatsView {
    pub(in crate::ui::features::stats) fn today(&self, report: &Report, cx: &mut Context<Self>) -> AnyElement {
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
                    .child(super::super::model::report::duration(cx, today)),
            )
            .child(muted_label(second, cx))
            .into_any_element()
    }

    /// 趋势：一根柱子一天，今天那根更实。
    ///
    /// 「哪根是今天」有三处说明而不是只有颜色（Guides：不许只靠颜色传义）：轴上永远标着
    /// 最后一格那个日期、tooltip 里写着完整日期，而图下面那一行 muted 直接把窗口结论念出来。
    /// 后两条尤其要紧 —— 移动端的浮层点不住（`shelf` 里记过的那条事实），
    /// tooltip 不能是唯一那条读数通路。
    pub(in crate::ui::features::stats) fn trend(&self, report: &Report, compact: bool, cx: &mut Context<Self>) -> AnyElement {
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
                    &super::super::model::report::duration(cx, report.window.seconds).to_string(),
                ),
                (
                    "avg",
                    &super::super::model::report::duration(cx, report.window.average_per_active_day()).to_string(),
                ),
            ],
        ));
        section(
            tr(cx, "stats.trend"),
            v_flex()
                .w_full()
                .gap_2()
                .child(
                    div().w_full().h(px(TREND_HEIGHT)).child(
                        BarChart::new(bars)
                            .band(|bar| bar.band.clone())
                            .value(|bar| bar.minutes)
                            .name(tr(cx, "stats.minutes_unit"))
                            .fill(
                                move |bar: &Bar, _, _, _| {
                                    if bar.today { solid } else { faded }
                                },
                            )
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
                            .band_tick_count(day_labels(compact))
                            .value_axis_label_placement(value_axis_placement(compact))
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
    pub(in crate::ui::features::stats) fn totals(&self, report: &Report, cx: &mut Context<Self>) -> AnyElement {
        let cells = [
            (
                tr(cx, "stats.total_time"),
                super::super::model::report::duration(cx, report.totals.seconds),
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
    pub(in crate::ui::features::stats) fn ranks(&self, report: &Report, cx: &mut Context<Self>) -> AnyElement {
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
                        div().w(px(SHARE_LANE)).flex_none().child(
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
                            .child(super::super::model::report::duration(cx, rank.seconds)),
                    )
                    .into_any_element()
            })
            .collect::<Vec<AnyElement>>();
        section(tr(cx, "stats.top"), v_flex().w_full().children(rows), cx).into_any_element()
    }

    /// 一般几点读。24 根柱子，读得最多的那一个钟点更实，下面跟一句结论。
    ///
    /// 这一节原来是 24 格手画的深浅块，那时有两条说不通：深浅是唯一的读数通路（Guides
    /// 不许只靠颜色传义），而四档深浅把「读了 40 分钟」和「读了 55 分钟」压成同一格。
    /// 换成柱子之后高低自己会说话，轴上有钟点可数，悬停还能读出本地化过的那一句。
    /// 结论那一行照旧留着 —— 移动端的浮层点不住。
    pub(in crate::ui::features::stats) fn hours(&self, report: &Report, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        let cells: Vec<HourCell> = report.hours.clone();
        let fill = cx.theme().chart_3;
        let peak = cx.theme().chart_4;
        let radius = cx.theme().radius;
        // 峰值那一格由快照钉好（并列时取最早的钟点，见 `super::super::model::report::hours_for`）。
        // 画到这一节的前提是窗口有 20 段以上的读数，所以那个 `None` 走不到；
        // 留着兜底只是因为类型上是 `Option`，与改造成图之前那一句同形。
        let busiest = report.peak_hour().unwrap_or(0);
        let note = SharedString::from(tf(cx, "stats.hours_peak", &[("hour", &busiest)]));
        section(
            tr(cx, "stats.hours"),
            v_flex()
                .w_full()
                .gap_2()
                .child(
                    div().w_full().h(px(HOURS_HEIGHT)).child(
                        BarChart::new(cells)
                            .band(|cell: &HourCell| cell.band.clone())
                            .value(|cell| cell.minutes)
                            .name(tr(cx, "stats.minutes_unit"))
                            // 与趋势图同一套说法：全图只有一格是焦点。
                            .fill(
                                move |cell: &HourCell, _, _, _| {
                                    if cell.peak { peak } else { fill }
                                },
                            )
                            .corner_radii(Corners {
                                top_left: radius,
                                top_right: radius,
                                bottom_left: px(0.),
                                bottom_right: px(0.),
                            })
                            .min_length(2.)
                            .value_axis(true)
                            .value_tick_count(3)
                            .value_tick_format(|v| format!("{}", v.round() as i64))
                            .band_tick_count(hour_labels(compact))
                            .value_axis_label_placement(value_axis_placement(compact))
                            // 轴上是裸钟点，tooltip 的标题才是「21 点」那一句：
                            // 24 个带单位的名字排在一行上会叠。
                            .tooltip_title(|cell: &HourCell| cell.title.clone())
                            .tooltip_value(|cell: &HourCell, _| cell.readout.clone())
                            .appear_key(self.range.days()),
                    ),
                )
                .child(muted_label(note, cx)),
            cx,
        )
        .into_any_element()
    }

    /// 一周里哪天读得多。七根辐条的一张雷达图，下面跟一句结论。
    ///
    /// 用雷达而不是再来一排柱子：星期几是**会绕回去**的一圈（周日之后接周一），
    /// 拉成一条线就把那个绕口摆在了读数路径正中；而这一节要的是**形状** ——
    /// 周末鼓起来、工作日塌下去 —— 那正是多边形一眼说得出、一排柱子得逐根比的东西。
    ///
    /// 雷达没有数值轴，图自己也说不出「平均多少分钟」，所以下面那一行 muted 是这一节
    /// 唯一的读数通路（与趋势、时段两节同一条规矩：移动端浮层点不住）。
    pub(in crate::ui::features::stats) fn weekday(&self, pattern: &WeekdayPattern, cx: &mut Context<Self>) -> AnyElement {
        let spokes: Vec<WeekdaySpoke> = pattern.spokes.clone();
        let line = cx.theme().chart_3;
        let note = SharedString::from(tf(
            cx,
            "stats.weekday_note",
            &[
                ("day", &pattern.busiest.name),
                ("avg", &pattern.busiest.readout),
            ],
        ));
        section(
            tr(cx, "stats.weekday"),
            v_flex()
                .w_full()
                .gap_2()
                .child(
                    div().w_full().h(px(WEEKDAY_HEIGHT)).child(
                        RadarChart::new(spokes)
                            .label(|spoke: &WeekdaySpoke| spoke.name.clone())
                            .value(|spoke: &WeekdaySpoke| spoke.minutes)
                            .stroke(line)
                            .fill(line.opacity(0.2))
                            .dot()
                            // 四环：半径是盒高的 40%，每环就是一档「平均多少分钟」，
                            // 读者不必知道那一圈是几分钟也知道鼓起来的那面高了几档。
                            .grid_levels(4)
                            .name(tr(cx, "stats.minutes_unit"))
                            // 标题由辐条那串星期名供给（字符串标签自带标题），
                            // 这里只把那一行的读数换成本地化过的句子。
                            .tooltip_value(|spoke: &WeekdaySpoke, _, _| spoke.readout.clone())
                            .appear_key(self.range.days()),
                    ),
                )
                .child(muted_label(note, cx)),
            cx,
        )
        .into_any_element()
    }

    /// 读不到书库。这一句与「还没有阅读记录」必须分得开：前者是这台设备上取不到数据源，
    /// 后者是读者真的还没读 —— 并成一句「暂无数据」，读者会以为是自己把书读空了
    /// （`widgets::empty_state` 顶上立的就是这条规矩）。
    pub(in crate::ui::features::stats) fn unavailable(&self, cx: &mut Context<Self>) -> AnyElement {
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
    /// 一直带在类型里，后面每一处 `cx.theme()` 都撞在它上面（与 `reader::page::toc_sheet_element`
    /// 那处是同一个坑）。
    pub(in crate::ui::features::stats) fn range_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
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
                        .on_click(move |_, _, cx| page.update(cx, |this, cx| this.pick(range, cx)))
                }))
                .into_any_element(),
        )
    }
}

/// 一个小节的标题：muted 的小字，不是大标题。
///
/// 这一屏的主角是数，标题只负责分组。用尺寸与间距分层而不是加色、加容器，
/// 是 `Hierarchy` 那条（"use size, weight, spacing, and separators before adding color
/// or containers"）。
pub(in crate::ui::features::stats) fn section_label(title: SharedString, cx: &App) -> impl IntoElement {
    muted_label(title, cx)
}

/// 一节 = 一个 muted 小标题 + 内容。标题与内容之间 `gap_2`（一组之内），
/// 节与节之间那条 `gap_6` 与发丝线由调用点给。
pub(in crate::ui::features::stats) fn section(title: SharedString, content: impl IntoElement, cx: &App) -> impl IntoElement {
    v_flex()
        .w_full()
        .gap_2()
        .child(section_label(title, cx))
        .child(content)
}

