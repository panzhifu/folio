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
//!
//! 三张图（趋势的柱、时段的柱、星期几的雷达）共用同一条规矩：**图的下面永远跟一句
//! muted 的结论**。图说的是形状，那句说的是形状里唯一要记住的那个数 —— 移动端的浮层
//! 点不住（`shelf` 里记过的那条事实），读数通路不能只剩 tooltip 一条。


pub use super::model::report::Range;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::chart::{BarChart, RadarChart};
use gpui_kit::component::plot::AxisLabelPlacement;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme, Selectable, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Corners, ElementId, EventEmitter, FontWeight, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    TestSupportExt as _, Window, div, px,
};

use crate::ui::services::app_store;
use crate::ui::services::l10n::{tf, tr};
use crate::ui::shell::navigation::SizeClass;
use crate::ui::components::{hairline, muted_label};
use super::model::report::{Bar, HourCell, Report, WeekdayPattern, WeekdaySpoke};

mod sections;

use sections::*;

/// 内容列的最大宽度（逻辑像素）。
///
/// 这里是裸 `px` 而不是 rem 档：它是一块**版心**，与阅读页的版心同性质 —— 量的是
/// 「一行多长还读得下去」，不该随界面缩放一起放大（Guides 允许的那类几何边界）。
const COLUMN_MAX: f32 = 720.0;
/// 左右留白，与书架共用同一个数：切目的地时那条对齐脊线不该跳。
const GUTTER: f32 = 12.0;
/// 趋势图盒的高。
///
/// 必须显式给：图表不自己量高，而它在滚动层里拿不到能被 `flex_1` 解析的高 ——
/// 不给就是零高，症状是「趋势那一节只剩标题，图不见了」。与阅读页目录层同一条坑
/// （`reader::page::toc_sheet_element` 那处注释）。
const TREND_HEIGHT: f32 = 168.0;
/// 时段图盒的高：比趋势矮一档。它是「一天里哪几个钟点在读」的次要读数，
/// 而且 24 根柱子本来就比 30 根矮胖，不需要那么多垂直空间。
const HOURS_HEIGHT: f32 = 112.0;
/// 星期几那张雷达盒的高。
///
/// 半径取盒高的 40%（组件那条默认），所以 208 一圈是 83 像素：七条辐条加外面
/// 那一圈星期名刚好放得进 336 宽的窄屏，不会顶到边。
const WEEKDAY_HEIGHT: f32 = 208.0;
/// 榜单行里那条占比栏的固定车道宽。
const SHARE_LANE: f32 = 96.0;
/// 榜单行里读数那一列的车道宽（右对齐，让竖着比的数是齐的）。
const READOUT_LANE: f32 = 96.0;

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
    pub(in crate::ui::features::stats) range: Range,
    /// `None` = 书库读不出来（没装上，或那几条查询全失败）。
    /// 这一格要说的是「读不到」，与「库里没有阅读记录」是两句话，别混。
    pub(in crate::ui::features::stats) report: Option<Report>,
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
    pub(in crate::ui::features::stats) fn pick(&mut self, range: Range, cx: &mut Context<Self>) {
        if range == self.range {
            return;
        }
        self.range = range;
        self.refresh(cx);
    }
}

impl EventEmitter<StatsEvent> for StatsView {}

impl Render for StatsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let wide = matches!(SizeClass::of(window), SizeClass::Expanded);
        // 轴上标几个刻度按**屏宽**决定，与 `wide` 不是一件事：那条 720 版心管的是
        // 「一行多长还读得下去」，这一条管的是「一排标签排得下几个」。
        let compact = matches!(SizeClass::of(window), SizeClass::Compact);
        let blocks: Vec<AnyElement> = match &self.report {
            None => vec![self.unavailable(cx)],
            Some(report) => {
                let mut blocks = vec![self.today(report, cx), self.trend(report, compact, cx)];
                // 累计不跟着档位变，但它是「读了多少」这个总账，仍排在趋势之后。
                blocks.push(self.totals(report, cx));
                // 两节按数据有没有说话决定画不画：一本的书名在「今天」附近已经出现过，
                // 一张两个夜晚的时段图读起来是噪声而不是规律。
                if report.shows_ranks() {
                    blocks.push(self.ranks(report, cx));
                }
                if report.shows_hours() {
                    blocks.push(self.hours(report, compact, cx));
                }
                // 星期几排在时段之后：两节说的都是「什么时候读」，一个看一天里、
                // 一个看一周里，连着读才是节律。门槛见 `super::model::report::weekdays_for`。
                if let Some(pattern) = &report.weekdays {
                    blocks.push(self.weekday(pattern, cx));
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

/// 趋势轴上标几个日期，按屏宽给。
///
/// 这里原来是一条**步长**（`tick_margin`），而步长有两笔亏：14 天档等于把 14 个
/// `MM-DD` 全排上，窄屏那一排必然叠字；而最后一格（今天）标不标得上，取决于窗口
/// 天数是不是那一步长的整数倍 —— 30 配 3 刚好压中，换个档位就漏。全图唯一的焦点
/// 不该靠一次除法侥幸才有名字。`band_tick_count` 是从第一格匀到最后一格，
/// 最后一格永远标得到。
fn day_labels(compact: bool) -> usize {
    if compact { 4 } else { 7 }
}

/// 时段轴上标几个钟点：宽屏一天切五段，窄屏三段（0 / 12 / 23 前后）。
fn hour_labels(compact: bool) -> usize {
    if compact { 3 } else { 5 }
}

/// 值轴的刻度写在图外那条车道里，还是写在图里侧紧贴网格线。
///
/// 写在外面要占一条车道，而组件是按最宽的那个标签把车道撑开的：窄屏那一条是从柱子
/// 身上扣的，14 根柱子本来就挤，再扣二十来像素就成了一片缝。宽屏不缺那点地方，
/// 就把刻度留在图外，别让它压在柱子上。
fn value_axis_placement(compact: bool) -> AxisLabelPlacement {
    if compact {
        AxisLabelPlacement::Inside
    } else {
        AxisLabelPlacement::Outside
    }
}
