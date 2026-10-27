//! 「统计」目的地：读了多少、什么时候读、时间花在哪些书上。
//!
//! 这一屏照的是阅读器 Readingo 的统计页：顶上一排区间（总 / 日 / 周 / 月 / 年），
//! 下面按区间换几块卡片 —— 总览、时间分析、热力图 / 日历、行为分析、毅力成就、排行。
//! 仿的是**信息的层次**，不是那一屏的填色：Folio 的卡片靠描边与留白立住
//! （`gpui-kit-design-guides` 的「先尺寸、字重、留白与分隔线，再考虑加颜色或容器」），
//! 所以这里没有 Readingo 那种整块浅紫底。
//!
//! 状态归 [`StatsView`]：快照在 [`StatsView::refresh`]（切进这一屏、关掉一本书、切区间、
//! 翻页）里取，`render` 一律不碰库。区间由用户选，翻页只改锚点日。
//!
//! 三条仍然成立的规矩：
//!
//! 1. **每个数字都要有话说**。图下面跟一句结论，不把读数只交给 tooltip —— 移动端
//!    点不住浮层，而这一屏的几张图本来就没有 tooltip。
//! 2. **不许只靠颜色传义**。热力图的深浅之外，日历格子还写着时长，图例写着档位；
//!    排行的名次是数字不是色块。
//! 3. **没有的指标不编**。库里只有会话时长，没有逐页翻页数、没有已读字数，所以这一屏
//!    不摆「翻页次数 / 字数」；对应位置换成真实的「阅读次数 / 阅读天数」。
//!
//! 图的下面永远跟一句 muted 的结论；页面标题不写「统计」二字 —— 导航已经点亮了这一格。

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::chart::{AreaChart, BarChart};
use gpui_kit::component::description_list::{DescriptionItem, DescriptionList};
use gpui_kit::component::empty::{Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle};
use gpui_kit::component::group_box::{GroupBox, GroupBoxVariant, GroupBoxVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::table::{Table, TableBody, TableCaption, TableCell, TableRow};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, Sizable, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Background, Context, InteractiveElement as _, IntoElement, ParentElement,
    Render, SharedString, Styled, TestSupportExt as _, Window, div, linear_color_stop,
    linear_gradient, px,
};

use super::model::{Bucket, DayStat, Range, Snapshot};
use crate::ui::components::muted_label;
use crate::ui::services::app_store;
use crate::ui::services::l10n::{duration, tf, tr};
use crate::ui::shell::navigation::{self, SizeClass};

/// 内容列的最大宽度。统计是报表面，拉太宽只会让数字之间隔出一片海。
const COLUMN_MAX: f32 = 760.0;
/// 左右留白，与书架共用同一个数：切目的地时那条对齐脊线不该跳。
const GUTTER: f32 = 16.0;
/// 卡片之间的呼吸。
const CARD_GAP: f32 = 12.0;
/// 热力图五档的分界（秒）：<30m、30–60m、1–3h、>3h。与图例一字不差。
const HEAT_STEPS: [i64; 3] = [1_800, 3_600, 10_800];

#[derive(Default)]
pub struct StatsView {
    /// `None` = 书库读不出来（没装上，或那条查询失败）。
    snapshot: Option<Snapshot>,
    /// 当前区间。默认「月」：翻开统计最常问的是「这个月读了多少」。
    range: Range,
    /// 窗口里的锚点日（本地日序号）。`0` = 还没初始化，`refresh` 会填成今天。
    anchor: i64,
}

impl StatsView {
    pub fn new() -> Self {
        Self {
            snapshot: None,
            range: Range::Month,
            anchor: 0,
        }
    }

    /// 重取一次快照。切进这一屏、关掉一本书、切区间、翻页都走这里。
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let tz = crate::ui::host::local_offset_secs();
        let today = folio_core::local_day(folio_core::infra::time::now(), tz);
        if self.anchor == 0 || self.anchor > today {
            self.anchor = today;
        }
        let Some(store) = app_store::handle(cx) else {
            self.snapshot = None;
            cx.notify();
            return;
        };
        self.snapshot = Some(Snapshot::load(&store, self.range, self.anchor, tz));
        cx.notify();
    }

    /// 现在手上有没有一份快照。外壳那扇回归门用它钉「切进这一屏确实取到了一次」。
    pub fn has_heatmap(&self) -> bool {
        self.snapshot.is_some()
    }

    /// 窗口内有读的天数；没有快照时是 `None`（那表示「还没去看」，不是「库里是零」）。
    pub fn active_days(&self) -> Option<usize> {
        self.snapshot.as_ref().map(|snapshot| snapshot.active_days)
    }

    /// 测试用：按索引切一档区间。真机上的入口是顶上那排 `TabBar`。
    #[cfg(test)]
    pub(crate) fn show_range_for_test(&mut self, index: usize, cx: &mut Context<Self>) {
        self.range = Range::from_index(index);
        self.anchor = 0;
        self.refresh(cx);
    }

    fn select_range(&mut self, range: Range, cx: &mut Context<Self>) {
        if self.range == range {
            return;
        }
        self.range = range;
        self.anchor = 0;
        self.refresh(cx);
    }

    fn shift(&mut self, step: i64, cx: &mut Context<Self>) {
        if self.range.is_total() {
            return;
        }
        self.anchor = self.range.shift(self.anchor, step);
        self.refresh(cx);
    }
}

impl Render for StatsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let size = SizeClass::of(window);
        // Compact 档那条底栏是浮的：内容自己把栏底下那一段空出来。
        let tail = if size.is_compact() {
            navigation::clearance(crate::ui::host::safe_area(window, cx).bottom)
        } else {
            0.0
        };

        let mut sections = vec![self.range_tabs(cx)];
        if let Some(snapshot) = &self.snapshot {
            if !snapshot.range.is_total() {
                sections.push(self.period_bar(snapshot, cx));
            }
            sections.extend(self.report(snapshot, size, cx));
        } else {
            sections.push(self.unavailable(cx));
        }

        v_flex()
            .id("folio-stats")
            .test_support()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .id("stats-scroll")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scrollbar()
                    .child(
                        v_flex().w_full().items_center().child(
                            v_flex()
                                .w_full()
                                .max_w(px(COLUMN_MAX))
                                .px(px(GUTTER))
                                .pt(px(8.0))
                                .pb(px(GUTTER + tail))
                                .gap(px(CARD_GAP))
                                .children(sections),
                        ),
                    ),
            )
            .into_any_element()
    }
}

impl StatsView {
    /// 顶上的区间开关。`TabBar` 负责选中块、滑动指示器与键盘焦点。
    fn range_tabs(&self, cx: &mut Context<Self>) -> AnyElement {
        let me = cx.entity().clone();
        let mut bar = TabBar::new("stats-range-tabs")
            .segmented()
            .small()
            .w_full()
            .selected_index(self.range.index())
            .on_click(move |index, _, cx| {
                let range = Range::from_index(*index);
                me.update(cx, |this, cx| this.select_range(range, cx));
            });
        for range in Range::ALL {
            bar = bar.child(Tab::new().label(tr(cx, range.label_key())).flex_1());
        }
        div().w_full().child(bar).into_any_element()
    }

    /// 周期导航：`‹ 2026-10 ›`。当前周期及未来的人不往前翻。
    fn period_bar(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> AnyElement {
        let me = cx.entity().clone();
        let can_forward = snapshot.end < snapshot.today;
        let label = period_label(snapshot, cx);
        v_flex()
            .w_full()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .px(px(6.0))
            .py(px(4.0))
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child({
                        let label = tr(cx, "stats.previous_period");
                        Button::new("stats-period-prev")
                            .ghost()
                            .small()
                            .icon(IconName::ChevronLeft)
                            .accessibility_label(label)
                            .on_click(move |_, _, cx| {
                                me.update(cx, |this, cx| this.shift(-1, cx));
                            })
                    })
                    .child(div().text_lg().font_semibold().child(label))
                    .child({
                        let me = cx.entity().clone();
                        let label = tr(cx, "stats.next_period");
                        Button::new("stats-period-next")
                            .ghost()
                            .small()
                            .icon(IconName::ChevronRight)
                            .disabled(!can_forward)
                            .accessibility_label(label)
                            .on_click(move |_, _, cx| {
                                me.update(cx, |this, cx| this.shift(1, cx));
                            })
                    }),
            )
            .into_any_element()
    }

    fn report(
        &self,
        snapshot: &Snapshot,
        size: SizeClass,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        match snapshot.range {
            Range::Total => self.total_report(snapshot, cx),
            Range::Day => self.day_report(snapshot, cx),
            Range::Week | Range::Month | Range::Year => self.period_report(snapshot, size, cx),
        }
    }

    /// 「总」：相识天数、四个大数、最近 10/30 天、单次时长分布、时长排行。
    fn total_report(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![
            self.since_first(snapshot, cx),
            self.total_overview(snapshot, cx),
            self.recent_area(snapshot, cx),
            self.recent_grid(snapshot, cx),
            self.session_histogram(snapshot, cx),
            self.ranking(snapshot, 10, cx),
        ]
    }

    /// 「日」：当日总览、效率指标、当日之最、当日书目。
    fn day_report(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![
            self.day_overview(snapshot, cx),
            self.efficiency(snapshot, cx),
            self.day_best(snapshot, cx),
            self.ranking(snapshot, 10, cx),
        ]
    }

    /// 「周 / 月 / 年」：总览、时间分析、热力图、行为、毅力、排行；「年」多两块。
    fn period_report(
        &self,
        snapshot: &Snapshot,
        size: SizeClass,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut out = Vec::new();
        out.push(self.period_overview(snapshot, size, cx));
        out.push(self.time_analysis(snapshot, cx));
        out.push(match snapshot.range {
            Range::Year => self.year_heatmap(snapshot, cx),
            Range::Week => self.week_calendar(snapshot, cx),
            _ => self.month_calendar(snapshot, cx),
        });
        if snapshot.range == Range::Year {
            out.push(self.monthly_read(snapshot, cx));
            out.push(self.hour_chart(snapshot, cx));
            out.push(self.year_records(snapshot, cx));
        }
        out.push(self.behavior(snapshot, cx));
        out.push(self.persistence(snapshot, cx));
        out.push(self.ranking(snapshot, 10, cx));
        out
    }

    fn unavailable(&self, cx: &mut Context<Self>) -> AnyElement {
        // 整屏的空状态交给 `Empty`：图标位、标题与说明的层次是它自己的事，
        // 这一屏不再用一行 muted 字假装那里「本来就有内容」。
        card_plain()
            .child(
                Empty::new().border_0().p(px(8.0)).header(
                    EmptyHeader::new()
                        .media(
                            EmptyMedia::new().child(
                                Icon::new(IconName::ChartColumn)
                                    .large()
                                    .text_color(cx.theme().muted_foreground),
                            ),
                        )
                        .title(EmptyTitle::new().child(tr(cx, "stats.no_store")))
                        .description(EmptyDescription::new().child(tr(cx, "stats.no_store_hint"))),
                ),
            )
            .into_any_element()
    }

    /// 「我们相识的第 N 天」。没有记录时说的是另一句话。
    fn since_first(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> AnyElement {
        let body: AnyElement = match snapshot.days_since_first() {
            Some(days) => h_flex()
                .w_full()
                .items_baseline()
                .gap_1()
                .child(div().text_lg().text_color(cx.theme().foreground).child({
                    // 「我们相识的第 %{days} 天」在九门语言里语序不同，整句交给词表。
                    SharedString::from(tf(cx, "stats.since_first", &[("days", &days)]))
                }))
                .into_any_element(),
            None => v_flex()
                .gap_1()
                .child(div().child(tr(cx, "stats.empty_period")))
                .child(muted_label(tr(cx, "stats.start_reading"), cx))
                .into_any_element(),
        };
        card_plain().child(body).into_any_element()
    }

    fn total_overview(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> AnyElement {
        let cells = [
            (
                tr(cx, "stats.total_time"),
                duration(cx, snapshot.lifetime_seconds),
            ),
            (
                tr(cx, "stats.read_days"),
                count_text(snapshot.lifetime_days, "stats.count_days", cx),
            ),
            (
                tr(cx, "stats.books_read"),
                count_text(snapshot.lifetime_books, "stats.count_books", cx),
            ),
            (
                tr(cx, "stats.read_sessions"),
                count_text(snapshot.lifetime_sessions, "stats.count_sessions", cx),
            ),
        ];
        grid(2)
            .w_full()
            .children(cells.into_iter().map(|(label, value)| {
                v_flex()
                    .gap_1()
                    .p_4()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(muted_label(label, cx))
                    .child(div().text_xl().font_semibold().child(value))
                    .into_any_element()
            }))
            .into_any_element()
    }

    fn recent_area(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> AnyElement {
        let points: Vec<ChartPoint> = snapshot
            .recent_slice(10)
            .iter()
            .map(|day| ChartPoint {
                label: weekday_label(day.day, cx),
                value: day.seconds as f64 / 60.0,
            })
            .collect();
        let max = points.iter().map(|p| p.value).fold(1.0_f64, f64::max);
        let solid = cx.theme().chart_3;
        let chart = AreaChart::new(points)
            .x(|point| point.label.clone())
            .y(|point| point.value as f32)
            .stroke(solid)
            .fill(area_gradient(solid))
            .y_domain(0.0, max as f32 * 1.15)
            .y_tick_count(3)
            .tick_margin(0)
            .x_tick_count(10)
            .grid_dashed(true)
            .id("stats-recent-area");
        card(cx, tr(cx, "stats.recent_10"), tr(cx, "stats.daily_minutes"))
            .child(div().w_full().h(px(170.0)).child(chart))
            .child(muted_label(
                SharedString::from(tf(
                    cx,
                    "stats.recent_note",
                    &[(
                        "total",
                        &duration(
                            cx,
                            snapshot
                                .recent_slice(10)
                                .iter()
                                .map(|d| d.seconds)
                                .sum::<i64>(),
                        )
                        .to_string(),
                    )],
                )),
                cx,
            ))
            .into_any_element()
    }

    fn recent_grid(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> AnyElement {
        let fills = heat_ramp(cx);
        let radius = cx.theme().radius_tokens().sm;
        card(cx, tr(cx, "stats.recent_30"), tr(cx, "stats.daily_hours"))
            .child(
                grid(15)
                    .w_full()
                    .gap_1()
                    .children(snapshot.recent.iter().map(|day| {
                        div()
                            .h(px(14.0))
                            .rounded(radius)
                            .bg(fills[heat_level(day.seconds)])
                            .into_any_element()
                    })),
            )
            .child(heat_legend(&fills, cx))
            .into_any_element()
    }

    fn session_histogram(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> AnyElement {
        let labels = [
            tr(cx, "stats.session_under_5"),
            tr(cx, "stats.session_5_15"),
            tr(cx, "stats.session_15_30"),
            tr(cx, "stats.session_30_60"),
            tr(cx, "stats.session_over_60"),
        ];
        let points: Vec<ChartPoint> = snapshot
            .session_hist
            .iter()
            .zip(labels)
            .map(|(count, label)| ChartPoint {
                label,
                value: *count as f64,
            })
            .collect();
        let solid = cx.theme().chart_3;
        let chart = BarChart::new(points)
            .band(|point| point.label.clone())
            .value(|point| point.value as f32)
            .fill(move |_, _, _, _| solid)
            .label(|point| SharedString::from(format!("{}", point.value as i64)))
            .label_axis(true)
            .value_axis(false)
            .grid(false)
            .band_tick_count(5)
            .corner_radii(bar_caps())
            .id("stats-session-hist");
        let body = if snapshot.lifetime_sessions == 0 {
            empty_inline(tr(cx, "stats.empty_period"), cx)
        } else {
            div().w_full().h(px(150.0)).child(chart).into_any_element()
        };
        card(
            cx,
            tr(cx, "stats.session_rhythm"),
            tr(cx, "stats.session_rhythm_hint"),
        )
        .child(body)
        .into_any_element()
    }

    fn day_overview(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> AnyElement {
        let headline: AnyElement = if snapshot.is_empty() {
            div()
                .text_3xl()
                .font_semibold()
                .text_color(cx.theme().muted_foreground)
                .child(tr(cx, "stats.no_reading_day"))
                .into_any_element()
        } else {
            div()
                .text_3xl()
                .font_semibold()
                .child(duration(cx, snapshot.seconds))
                .into_any_element()
        };
        let cells = [
            (
                tr(cx, "stats.day_books"),
                count_text(snapshot.books, "stats.count_books", cx),
            ),
            (
                tr(cx, "stats.day_sessions"),
                count_text(snapshot.sessions, "stats.count_sessions", cx),
            ),
            (
                tr(cx, "stats.day_longest"),
                snapshot
                    .longest_session
                    .as_ref()
                    .map(|moment| duration(cx, moment.seconds))
                    .unwrap_or_else(|| SharedString::from("—")),
            ),
        ];
        card(cx, tr(cx, "stats.day_overview"), SharedString::default())
            .child(v_flex().items_center().w_full().gap_1().child(headline))
            .child(
                h_flex()
                    .w_full()
                    .justify_around()
                    .children(cells.into_iter().map(|(label, value)| {
                        v_flex()
                            .items_center()
                            .gap_1()
                            .child(div().text_lg().font_semibold().child(value))
                            .child(muted_label(label, cx))
                            .into_any_element()
                    })),
            )
            .into_any_element()
    }

    fn efficiency(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> AnyElement {
        let cells = [
            (
                tr(cx, "stats.avg_session"),
                duration(cx, snapshot.average_session()),
                tr(cx, "stats.per_session"),
            ),
            (
                tr(cx, "stats.day_longest"),
                snapshot
                    .longest_session
                    .as_ref()
                    .map(|moment| duration(cx, moment.seconds))
                    .unwrap_or_else(|| SharedString::from("—")),
                tr(cx, "stats.per_session"),
            ),
            (
                tr(cx, "stats.first_open"),
                snapshot
                    .earliest
                    .as_ref()
                    .map(|moment| clock_label(moment.at))
                    .unwrap_or_else(|| SharedString::from("—")),
                tr(cx, "stats.first_session"),
            ),
            (
                tr(cx, "stats.last_open"),
                snapshot
                    .latest
                    .as_ref()
                    .map(|moment| clock_label(moment.at))
                    .unwrap_or_else(|| SharedString::from("—")),
                tr(cx, "stats.last_session"),
            ),
        ];
        card(
            cx,
            tr(cx, "stats.efficiency"),
            tr(cx, "stats.efficiency_hint"),
        )
        .child(
            grid(2)
                .w_full()
                .gap_2()
                .children(cells.into_iter().map(|(label, value, hint)| {
                    v_flex()
                        .gap_1()
                        .p_3()
                        .rounded(cx.theme().radius_tokens().md)
                        .bg(cx.theme().muted)
                        .child(muted_label(label, cx))
                        .child(div().text_xl().font_semibold().child(value))
                        .child(muted_label(hint, cx))
                        .into_any_element()
                })),
        )
        .into_any_element()
    }

    fn day_best(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> AnyElement {
        let rows = [
            (
                tr(cx, "stats.most_time"),
                snapshot.top_book().map(|book| {
                    (
                        SharedString::from(book.title.clone()),
                        duration(cx, book.seconds),
                    )
                }),
            ),
            (
                tr(cx, "stats.most_sessions"),
                snapshot.most_sessions_book().map(|book| {
                    (
                        SharedString::from(book.title.clone()),
                        count_text(book.sessions, "stats.count_sessions", cx),
                    )
                }),
            ),
        ];
        card(cx, tr(cx, "stats.day_best"), tr(cx, "stats.day_best_hint"))
            .child(report_rows(
                rows.into_iter().map(|(label, value)| {
                    let (detail, right): (SharedString, SharedString) = value
                        .unwrap_or_else(|| (tr(cx, "stats.empty_period"), SharedString::from("—")));
                    (label, detail, right)
                }),
                cx,
            ))
            .into_any_element()
    }

    fn period_overview(
        &self,
        snapshot: &Snapshot,
        size: SizeClass,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cells = [
            (
                IconName::Clock,
                duration(cx, snapshot.seconds),
                tr(cx, "stats.total_time"),
            ),
            (
                IconName::BookOpen,
                count_text(snapshot.books, "stats.count_books", cx),
                tr(cx, "stats.books_read"),
            ),
            (
                IconName::Activity,
                count_text(snapshot.sessions, "stats.count_sessions", cx),
                tr(cx, "stats.read_sessions"),
            ),
            (
                IconName::CalendarDays,
                count_text(snapshot.active_days, "stats.count_days", cx),
                tr(cx, "stats.read_days"),
            ),
            (
                IconName::Timer,
                duration(cx, snapshot.average_active_day()),
                tr(cx, "stats.avg_duration"),
            ),
            (
                IconName::Flame,
                count_text(snapshot.longest_streak, "stats.count_days", cx),
                tr(cx, "stats.longest_streak"),
            ),
        ];
        let columns = if size == SizeClass::Expanded { 3 } else { 2 };
        card(
            cx,
            SharedString::from(tf(
                cx,
                "stats.overview",
                &[("range", &tr(cx, snapshot.range.label_key()))],
            )),
            tr(cx, "stats.overview_hint"),
        )
        .child(
            grid(columns)
                .w_full()
                .gap_3()
                .children(cells.into_iter().map(|(icon, value, label)| {
                    v_flex()
                        .items_center()
                        .gap_1()
                        .child(
                            Icon::new(icon)
                                .small()
                                .text_color(cx.theme().muted_foreground),
                        )
                        .child(div().text_xl().font_semibold().child(value))
                        .child(muted_label(label, cx))
                        .into_any_element()
                })),
        )
        .into_any_element()
    }

    fn time_analysis(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> AnyElement {
        let leads = [
            (
                duration(cx, snapshot.average_active_day()),
                tr(cx, "stats.avg_duration"),
            ),
            (
                SharedString::from(format!("{:.1}", snapshot.average_books_per_active_day())),
                tr(cx, "stats.avg_books"),
            ),
        ];
        card(
            cx,
            tr(cx, "stats.time_analysis"),
            tr(cx, "stats.time_analysis_hint"),
        )
        .child(
            h_flex()
                .w_full()
                .items_center()
                .py_2()
                .child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .gap_1()
                        .child(div().text_xl().font_semibold().child(leads[0].0.clone()))
                        .child(muted_label(leads[0].1.clone(), cx)),
                )
                .child(div().w(px(1.0)).h(px(44.0)).bg(cx.theme().border))
                .child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .gap_1()
                        .child(div().text_xl().font_semibold().child(leads[1].0.clone()))
                        .child(muted_label(leads[1].1.clone(), cx)),
                ),
        )
        .child(
            grid(2).w_full().gap_2().children(
                snapshot
                    .buckets
                    .iter()
                    .enumerate()
                    .map(|(index, bucket)| bucket_card(index, bucket, cx)),
            ),
        )
        .into_any_element()
    }

    fn month_calendar(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> AnyElement {
        let fills = heat_ramp(cx);
        let radius = cx.theme().radius_tokens().sm;
        let offset = snapshot
            .days
            .first()
            .map_or(0, |day| folio_core::weekday_monday0(day.day));
        let mut slots: Vec<AnyElement> = Vec::new();
        for _ in 0..offset {
            slots.push(div().h(px(48.0)).into_any_element());
        }
        for day in &snapshot.days {
            slots.push(calendar_cell(day, &fills, radius, cx));
        }
        while !slots.len().is_multiple_of(7) {
            slots.push(div().h(px(48.0)).into_any_element());
        }
        card(cx, tr(cx, "stats.activity"), tr(cx, "stats.activity_hint"))
            .child(grid(7).w_full().gap_1().children((0..7).map(|index| {
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(weekday_name(index, cx))
                    .into_any_element()
            })))
            // 外壳那些回归门按这个名字问「热力图那一块画没画出来」。
            .child(
                grid(7)
                    .id("stats-heatmap")
                    .test_support()
                    .w_full()
                    .gap_1()
                    .children(slots),
            )
            .child(heat_legend(&fills, cx))
            .into_any_element()
    }

    fn week_calendar(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> AnyElement {
        let fills = heat_ramp(cx);
        let radius = cx.theme().radius_tokens().sm;
        card(cx, tr(cx, "stats.activity"), tr(cx, "stats.activity_hint"))
            .child(grid(7).w_full().gap_1().children((0..7).map(|index| {
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(weekday_name(index, cx))
                    .into_any_element()
            })))
            .child(
                grid(7)
                    .id("stats-heatmap")
                    .test_support()
                    .w_full()
                    .gap_1()
                    .children(
                        snapshot
                            .days
                            .iter()
                            .map(|day| calendar_cell(day, &fills, radius, cx)),
                    ),
            )
            .child(heat_legend(&fills, cx))
            .into_any_element()
    }

    fn year_heatmap(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> AnyElement {
        let fills = heat_ramp(cx);
        let radius = cx.theme().radius_tokens().sm;
        let year = folio_core::civil_from_days(snapshot.anchor).year;
        let mut months: [[i64; 31]; 12] = [[0; 31]; 12];
        for day in &snapshot.days {
            let date = folio_core::civil_from_days(day.day);
            let month = (date.month as usize).clamp(1, 12) - 1;
            let index = (date.day as usize).clamp(1, 31) - 1;
            months[month][index] = day.seconds;
        }
        let lengths: Vec<u32> = (1..=12)
            .map(|month| folio_core::days_in_month(year, month))
            .collect();
        card(
            cx,
            tr(cx, "stats.year_activity"),
            tr(cx, "stats.activity_hint"),
        )
        .child(
            v_flex()
                .id("stats-heatmap")
                .test_support()
                .w_full()
                .gap(px(3.0))
                .children(months.iter().enumerate().map(|(month, days)| {
                    let length = lengths[month] as usize;
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .w(px(30.0))
                                .flex_none()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(SharedString::from(tf(
                                    cx,
                                    "stats.month_short",
                                    &[("month", &(month + 1))],
                                ))),
                        )
                        .child(grid(31).flex_1().min_w_0().gap(px(2.0)).children(
                            days.iter().enumerate().map(|(index, seconds)| {
                                // 小月的第 31 格不是「没读」，是这一天不存在。
                                let fill = if index < length {
                                    fills[heat_level(*seconds)]
                                } else {
                                    cx.theme().transparent
                                };
                                div().h(px(8.0)).rounded(radius).bg(fill).into_any_element()
                            }),
                        ))
                        .into_any_element()
                })),
        )
        .child(heat_legend(&fills, cx))
        .into_any_element()
    }

    /// 十二个月的柱状图。
    ///
    /// 与「单次时长」「24 小时」那两张共用 `BarChart`：轴、网格、柱宽与入场动画都交给
    /// 组件，这里只把每个月折成分钟数。空月份就是一根 0 高的柱子 —— 留一块灰底反而要
    /// 读者去分辨「这个月没读」与「这个月的数据没取到」。
    fn monthly_read(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> AnyElement {
        let mut totals = [0i64; 12];
        for day in &snapshot.days {
            let month = folio_core::civil_from_days(day.day).month as usize;
            totals[month.clamp(1, 12) - 1] += day.seconds;
        }
        let points: Vec<ChartPoint> = totals
            .iter()
            .enumerate()
            .map(|(index, seconds)| ChartPoint {
                label: SharedString::from((index + 1).to_string()),
                value: *seconds as f64 / 60.0,
            })
            .collect();
        let solid = cx.theme().chart_3;
        let chart = BarChart::new(points)
            .band(|point| point.label.clone())
            .value(|point| point.value as f32)
            .fill(move |_, _, _, _| solid)
            .value_axis(false)
            .grid(false)
            .band_tick_count(12)
            .corner_radii(bar_caps())
            .id("stats-monthly-chart");
        card(
            cx,
            tr(cx, "stats.monthly_reading"),
            tr(cx, "stats.monthly_reading_hint"),
        )
        .child(div().w_full().h(px(150.0)).child(chart))
        .into_any_element()
    }

    fn hour_chart(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> AnyElement {
        let points: Vec<ChartPoint> = snapshot
            .hours
            .iter()
            .enumerate()
            .map(|(hour, seconds)| ChartPoint {
                label: SharedString::from(format!("{hour:02}")),
                value: *seconds as f64 / 60.0,
            })
            .collect();
        let solid = cx.theme().chart_3;
        let chart = BarChart::new(points)
            .band(|point| point.label.clone())
            .value(|point| point.value as f32)
            .fill(move |_, _, _, _| solid)
            .value_axis(false)
            .grid(false)
            .band_tick_count(8)
            .corner_radii(bar_caps())
            .id("stats-hour-chart");
        let body = if snapshot.seconds == 0 {
            empty_inline(tr(cx, "stats.empty_period"), cx)
        } else {
            div().w_full().h(px(150.0)).child(chart).into_any_element()
        };
        card(cx, tr(cx, "stats.hourly"), tr(cx, "stats.hourly_hint"))
            .child(body)
            .into_any_element()
    }

    fn behavior(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> AnyElement {
        let rows = [
            (
                tr(cx, "stats.most_engaged"),
                snapshot.top_book().map(|book| {
                    (
                        SharedString::from(book.title.clone()),
                        duration(cx, book.seconds),
                    )
                }),
            ),
            (
                tr(cx, "stats.busiest_day"),
                snapshot.busiest_day.as_ref().map(|day| {
                    (
                        short_date(day.day),
                        count_text(day.books, "stats.count_books", cx),
                    )
                }),
            ),
            (
                tr(cx, "stats.longest_companion"),
                snapshot.most_days_book().map(|book| {
                    (
                        SharedString::from(book.title.clone()),
                        count_text(book.days, "stats.count_days", cx),
                    )
                }),
            ),
        ];
        card(cx, tr(cx, "stats.behavior"), tr(cx, "stats.behavior_hint"))
            .child(report_rows(
                rows.into_iter().map(|(label, value)| {
                    let (detail, right) = value
                        .unwrap_or_else(|| (tr(cx, "stats.empty_period"), SharedString::from("—")));
                    (label, detail, right)
                }),
                cx,
            ))
            .into_any_element()
    }

    fn persistence(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> AnyElement {
        let streak_detail = snapshot
            .longest_run
            .map(|(start, end)| {
                SharedString::from(format!("{} ~ {}", short_date(start), short_date(end)))
            })
            .unwrap_or_else(|| SharedString::from("—"));
        let today_in_window = snapshot.start <= snapshot.today && snapshot.today <= snapshot.end;
        let rows = [
            (
                tr(cx, "stats.longest_streak"),
                (
                    streak_detail,
                    count_text(snapshot.longest_streak, "stats.count_days", cx),
                ),
            ),
            (
                tr(cx, "stats.current_streak"),
                (
                    SharedString::default(),
                    if today_in_window {
                        count_text(snapshot.current_streak, "stats.count_days", cx)
                    } else {
                        SharedString::from("—")
                    },
                ),
            ),
            (
                tr(cx, "stats.best_day"),
                snapshot
                    .best_day
                    .as_ref()
                    .map(|day| (short_date(day.day), duration(cx, day.seconds)))
                    .unwrap_or_else(|| (SharedString::from("—"), SharedString::from("—"))),
            ),
        ];
        card(
            cx,
            tr(cx, "stats.persistence"),
            tr(cx, "stats.persistence_hint"),
        )
        .child(report_rows(
            rows.into_iter()
                .map(|(label, (detail, right))| (label, detail, right)),
            cx,
        ))
        .into_any_element()
    }

    fn year_records(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> AnyElement {
        let rows = [
            (
                tr(cx, "stats.most_readable_day"),
                snapshot
                    .best_day
                    .as_ref()
                    .map(|day| (short_date(day.day), duration(cx, day.seconds))),
            ),
            (
                tr(cx, "stats.most_varied_day"),
                snapshot.busiest_day.as_ref().map(|day| {
                    (
                        short_date(day.day),
                        count_text(day.books, "stats.count_books", cx),
                    )
                }),
            ),
        ];
        card(
            cx,
            tr(cx, "stats.year_records"),
            tr(cx, "stats.year_records_hint"),
        )
        .child(report_rows(
            rows.into_iter().map(|(label, value)| {
                let (detail, right) =
                    value.unwrap_or_else(|| (SharedString::from("—"), SharedString::from("—")));
                (label, detail, right)
            }),
            cx,
        ))
        .into_any_element()
    }

    /// 排行榜：按时长取前 `limit` 本，超出时在末尾补一句还差几本。
    ///
    /// 三列交给 `Table`（名次 / 书名 / 时长），行分隔线、单元格内边距与右侧对齐都由
    /// 组件给。前三名的浅色底保留 —— 但名次仍然由数字说，颜色只是第二层。
    fn ranking(&self, snapshot: &Snapshot, limit: usize, cx: &mut Context<Self>) -> AnyElement {
        let card = card(cx, tr(cx, "stats.ranking"), tr(cx, "stats.ranking_hint"));
        if snapshot.book_stats.is_empty() {
            return card
                .child(empty_inline(tr(cx, "stats.empty_period"), cx))
                .into_any_element();
        }
        let mut body = TableBody::new();
        for (index, book) in snapshot.book_stats.iter().take(limit).enumerate() {
            // 前三名给一层很浅的图表色，名次仍由数字说 —— 不许只靠颜色传义。
            let (badge, ink) = if index < 3 {
                (cx.theme().chart_3.opacity(0.16), cx.theme().foreground)
            } else {
                (cx.theme().muted, cx.theme().muted_foreground)
            };
            body = body.child(
                TableRow::new()
                    .child(
                        TableCell::new().child(
                            div()
                                .size(px(24.0))
                                .rounded(cx.theme().radius_tokens().full)
                                .bg(badge)
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_xs()
                                .text_color(ink)
                                .child((index + 1).to_string()),
                        ),
                    )
                    .child(
                        TableCell::new().col_span(5).child(
                            v_flex()
                                .min_w_0()
                                .gap_1()
                                .child(
                                    div()
                                        .font_semibold()
                                        .line_clamp(2)
                                        .child(book.title.clone()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .truncate()
                                        .child(SharedString::from(format!(
                                            "{} · {}",
                                            tf(cx, "stats.book_days", &[("days", &book.days)]),
                                            tf(
                                                cx,
                                                "stats.book_sessions",
                                                &[("sessions", &book.sessions)]
                                            ),
                                        ))),
                                ),
                        ),
                    )
                    .child(
                        TableCell::new()
                            .col_span(3)
                            .text_right()
                            .child(div().font_semibold().child(duration(cx, book.seconds))),
                    ),
            );
        }
        let more = snapshot.book_stats.len().saturating_sub(limit);
        card.child(
            Table::new()
                .accessibility_label(tr(cx, "stats.ranking"))
                .child(body)
                .when(more > 0, |table| {
                    table.child(TableCaption::new().child(SharedString::from(tf(
                        cx,
                        "stats.ranking_more",
                        &[("count", &more)],
                    ))))
                }),
        )
        .into_any_element()
    }
}

/// 一张卡片：`GroupBox` 的描边变体，标题槽里是「粗体标题 + 可选副题」。
///
/// 容器本身（圆角、内边距、标题与内容之间的间距、描边）交给组件，这一屏不再自己拼
/// 「一张带边框的 v_flex + 一段标题」。标题的字重仍由这里给：`GroupBox` 的标题槽
/// 默认是 muted 小字，而统计每一块要的是一个说得清「这块是什么」的标题。
fn card(cx: &App, title: SharedString, hint: SharedString) -> GroupBox {
    let mut head = v_flex().gap_1().child(
        div()
            .text_base()
            .font_semibold()
            .text_color(cx.theme().foreground)
            .child(title),
    );
    if !hint.is_empty() {
        head = head.child(muted_label(hint, cx));
    }
    GroupBox::new()
        .with_variant(GroupBoxVariant::Outline)
        .title(head)
}

/// 没有标题的卡片（「我们相识的第 N 天」那一张本来就是一句话）。
fn card_plain() -> GroupBox {
    GroupBox::new().with_variant(GroupBoxVariant::Outline)
}

/// 卡内「这一段没有数据」：`Empty` 去掉虚线边、收紧内边距，高度与原来那一句持平。
fn empty_inline(text: SharedString, cx: &App) -> AnyElement {
    Empty::new()
        .border_0()
        .p(px(12.0))
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(text),
        )
        .into_any_element()
}

/// 一块「报表行」：标签 / 明细 / 读数。
///
/// 用 `DescriptionList` 的描边排列替掉原先每条一张 muted 小卡：行与行之间由组件自己的
/// 分隔线分开，标签列宽固定，读数在右端对齐 —— 三块读数的「账」看上去才是一张表。
fn report_rows(
    rows: impl IntoIterator<Item = (SharedString, SharedString, SharedString)>,
    cx: &App,
) -> AnyElement {
    let items: Vec<DescriptionItem> = rows
        .into_iter()
        .map(|(label, detail, right)| {
            let value = h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(cx.theme().muted_foreground)
                        .child(detail),
                )
                .child(div().flex_none().font_semibold().child(right));
            DescriptionItem::new(label).value(value.into_any_element())
        })
        .collect();
    DescriptionList::new()
        .bordered(true)
        .label_width(px(96.0))
        .small()
        .children(items)
        .into_any_element()
}

/// 日历里的一格：日期在上、时长在下。没有读的那天只留日期与 0，不画色块。
fn calendar_cell(
    day: &DayStat,
    fills: &[gpui_kit::Hsla; 5],
    radius: gpui_kit::Pixels,
    cx: &App,
) -> AnyElement {
    let level = heat_level(day.seconds);
    let muted = cx.theme().muted_foreground;
    // 深档是实色块，字要取与背景相反的那一档；浅档仍用次要文字色。
    let ink = if level >= 3 {
        cx.theme().background
    } else {
        muted
    };
    v_flex()
        .h(px(48.0))
        .justify_between()
        .p(px(6.0))
        .rounded(radius)
        .bg(fills[level])
        .child(
            div()
                .text_xs()
                .text_color(ink)
                .child(folio_core::civil_from_days(day.day).day.to_string()),
        )
        .child(
            div()
                .text_xs()
                .text_color(ink)
                .child(hours_text(day.seconds)),
        )
        .into_any_element()
}

fn bucket_card(index: usize, bucket: &Bucket, cx: &App) -> AnyElement {
    let label = match index {
        0 => tr(cx, "stats.bucket_night"),
        1 => tr(cx, "stats.bucket_morning"),
        2 => tr(cx, "stats.bucket_afternoon"),
        _ => tr(cx, "stats.bucket_evening"),
    };
    let mut body = v_flex()
        .gap_1()
        .p_3()
        .rounded(cx.theme().radius_tokens().md)
        .bg(cx.theme().muted)
        .child(muted_label(label, cx))
        .child(
            div()
                .text_sm()
                .font_semibold()
                .child(duration(cx, bucket.seconds)),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(count_text(bucket.books, "stats.count_books", cx)),
        );
    if let Some(book) = &bucket.top {
        body = body
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr(cx, "stats.most_time")),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .truncate()
                    .child(book.title.clone()),
            );
    }
    body.into_any_element()
}

fn heat_legend(fills: &[gpui_kit::Hsla; 5], cx: &App) -> AnyElement {
    let radius = cx.theme().radius_tokens().sm;
    let labels = [
        tr(cx, "stats.unread"),
        tr(cx, "stats.level_under_30"),
        tr(cx, "stats.level_30_60"),
        tr(cx, "stats.level_1_3h"),
        tr(cx, "stats.level_over_3h"),
    ];
    h_flex()
        .w_full()
        .flex_wrap()
        .items_center()
        .gap_2()
        .children(fills.iter().zip(labels).map(|(fill, label)| {
            h_flex()
                .items_center()
                .gap_1()
                .child(div().size(px(10.0)).rounded(radius).bg(*fill))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(label),
                )
                .into_any_element()
        }))
        .into_any_element()
}

fn count_text(count: usize, key: &str, cx: &App) -> SharedString {
    SharedString::from(tf(cx, key, &[("count", &count)]))
}

fn weekday_name(index: usize, cx: &App) -> SharedString {
    let key = match index {
        0 => "stats.weekday_mon",
        1 => "stats.weekday_tue",
        2 => "stats.weekday_wed",
        3 => "stats.weekday_thu",
        4 => "stats.weekday_fri",
        5 => "stats.weekday_sat",
        _ => "stats.weekday_sun",
    };
    tr(cx, key)
}

fn weekday_label(day: i64, cx: &App) -> SharedString {
    weekday_name(folio_core::weekday_monday0(day), cx)
}

fn short_date(day: i64) -> SharedString {
    let date = folio_core::civil_from_days(day);
    SharedString::from(format!("{:02}-{:02}", date.month, date.day))
}

fn period_label(snapshot: &Snapshot, cx: &App) -> SharedString {
    let date = folio_core::civil_from_days(snapshot.anchor);
    match snapshot.range {
        Range::Day => SharedString::from(folio_core::civil_date(snapshot.anchor)),
        Range::Week => {
            let from = folio_core::civil_from_days(snapshot.start);
            let to = folio_core::civil_from_days(snapshot.end);
            SharedString::from(tf(
                cx,
                "stats.week_range",
                &[
                    ("from", &format!("{:02}-{:02}", from.month, from.day)),
                    ("to", &format!("{:02}-{:02}", to.month, to.day)),
                ],
            ))
        }
        Range::Month => SharedString::from(tf(
            cx,
            "stats.period_month",
            &[
                ("year", &date.year),
                ("month", &format!("{:02}", date.month)),
            ],
        )),
        Range::Year => SharedString::from(tf(cx, "stats.period_year", &[("year", &date.year)])),
        Range::Total => SharedString::default(),
    }
}

fn clock_label(unix_secs: i64) -> SharedString {
    let tz = crate::ui::host::local_offset_secs();
    let local = (unix_secs + tz).rem_euclid(86_400);
    SharedString::from(format!("{:02}:{:02}", local / 3_600, local % 3_600 / 60))
}

/// 热度分档。分界是绝对值而不是分位数：同一格深浅不随读者的习惯漂。
fn heat_level(seconds: i64) -> usize {
    if seconds <= 0 {
        0
    } else if seconds < HEAT_STEPS[0] {
        1
    } else if seconds < HEAT_STEPS[1] {
        2
    } else if seconds < HEAT_STEPS[2] {
        3
    } else {
        4
    }
}

fn heat_ramp(cx: &App) -> [gpui_kit::Hsla; 5] {
    let solid = cx.theme().chart_3;
    [
        cx.theme().muted,
        solid.opacity(0.18),
        solid.opacity(0.38),
        solid.opacity(0.62),
        solid,
    ]
}

fn hours_text(seconds: i64) -> String {
    if seconds <= 0 {
        "0h".to_string()
    } else if seconds < 3_600 {
        format!("{}m", (seconds / 60).max(1))
    } else {
        format!("{:.1}h", seconds as f64 / 3_600.0)
    }
}

fn area_gradient(color: gpui_kit::Hsla) -> Background {
    linear_gradient(
        0.,
        linear_color_stop(color.opacity(0.45), 1.),
        linear_color_stop(color.opacity(0.), 0.),
    )
}

fn bar_caps() -> gpui_kit::Corners<gpui_kit::Pixels> {
    gpui_kit::Corners {
        top_left: px(4.0),
        top_right: px(4.0),
        bottom_left: px(0.0),
        bottom_right: px(0.0),
    }
}

fn grid(columns: usize) -> gpui_kit::Div {
    div().grid().grid_cols(columns as u16)
}

/// 图表要的数据点：标签与值。图表的闭包要 `'static`，所以标签在进图之前就拼好。
struct ChartPoint {
    label: SharedString,
    value: f64,
}

#[cfg(test)]
mod tests {
    use super::{heat_level, hours_text};

    /// 五档的分界就是图例上那几句话；改档位时图例必须跟着改，所以钉在这里。
    #[test]
    fn the_heat_levels_match_the_legend() {
        assert_eq!(heat_level(0), 0, "没读");
        assert_eq!(heat_level(1), 1, "读了一秒也算 <30m");
        assert_eq!(heat_level(1_799), 1);
        assert_eq!(heat_level(1_800), 2, "30 分钟");
        assert_eq!(heat_level(3_599), 2);
        assert_eq!(heat_level(3_600), 3, "1 小时");
        assert_eq!(heat_level(10_799), 3);
        assert_eq!(heat_level(10_800), 4, "3 小时");
    }

    /// 格子里的时长要短到塞得进一格，同时不能把「读了几分钟」读成 0。
    #[test]
    fn the_cell_reading_stays_short_and_never_rounds_to_zero() {
        assert_eq!(hours_text(0), "0h");
        assert_eq!(hours_text(59), "1m", "不足一分钟也要说 1 分钟，不能是 0");
        assert_eq!(hours_text(600), "10m");
        assert_eq!(hours_text(3_600), "1.0h");
        assert_eq!(hours_text(5_400), "1.5h");
    }
}
