//! 「统计」那一屏的数据快照。
//!
//! 分成两份东西是刻意的：这一份只问库要数、并把它们换算成界面能直接画的形状，
//! `mod.rs` 那一侧只画。切开的那条线是**取数时机** —— 快照在「切进这一屏 / 换了时间
//! 范围 / 关掉一本书」这三处重取一次，`render` 一律不碰库。每帧查一遍 sqlite 不会
//! 慢在哪条语句上，它慢在把「画一屏」变成「做一轮 IO」，而那正是渲染帧不该干的事。
//!
//! 换算产物是三种图的格子：[`Bar`]（趋势那串柱）、[`HourCell`]（时段那 24 格）、
//! [`WeekdaySpoke`]（星期几那 7 根辐条）。后两种是**在这份快照里归并出来的**，库里没有
//! 对应的查询：时段把已经查来的 24 格秒数换成图要的形状，星期几把窗口那串补过零的
//! 日桶按周摊平。归并都只是纯算术，所以「图上有那一格、数却没算它」这类错不会出现。

use std::sync::Arc;

use folio_core::store::{ReadingRank, ReadingSummary, ReadingTotals, Store};
use folio_core::{DailyReading, civil_date, duration_parts, infra::time, local_day};
use gpui_kit::{App, SharedString};

use crate::ui::shared::l10n::{tf, tr};

/// 窗口档位。三档都是「往回看几天」：切换换的是筛选，不是目的地。
///
/// 默认 [`Range::Fourteen`]：7 天在周末两天没读时图上一半是空的，30 天的柱子密到
/// 读不出单日。这一档两头都还留得住。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Range {
    Seven,
    #[default]
    Fourteen,
    Thirty,
}

impl Range {
    /// 三颗按钮的顺序，也是「按一下切到下一档」的轮转顺序。
    pub const ALL: [Range; 3] = [Range::Seven, Range::Fourteen, Range::Thirty];

    pub fn days(self) -> usize {
        match self {
            Range::Seven => 7,
            Range::Fourteen => 14,
            Range::Thirty => 30,
        }
    }

    /// 档位标签。键写成字面量的 `tr` 调用而不是「返回一个键名」的方法：
    /// `ui/shared/l10n.rs` 那条扫描测试只认字面量，而它守的是一种编译与测试全绿的
    /// 坏状态 —— 键解不出来时界面会把 `stats.range_7` 本身画出来。
    pub fn label(self, cx: &App) -> SharedString {
        match self {
            Range::Seven => tr(cx, "stats.range_7"),
            Range::Fourteen => tr(cx, "stats.range_14"),
            Range::Thirty => tr(cx, "stats.range_30"),
        }
    }
}

/// 一段时长的读数，按语言的形状挑句式。
///
/// 数由 `folio_core::duration_parts` 拆，这里只决定说哪一句：整点不提分钟、
/// 不足一小时不提小时、不足一分钟才说秒。三条规则各语言自己写在 TOML 里，
/// 所以这一段一次都没拼过「小时」两个字（`plans/基础设施.md` §五.4）。
pub fn duration(cx: &App, secs: i64) -> SharedString {
    let parts = duration_parts(secs);
    let text = match parts.hours {
        0 if parts.sub_minute => tf(cx, "stats.duration_seconds", &[("seconds", &parts.seconds)]),
        0 => tf(cx, "stats.duration_minutes", &[("minutes", &parts.minutes)]),
        hours if parts.minutes == 0 => tf(cx, "stats.duration_hours", &[("hours", &hours)]),
        hours => tf(
            cx,
            "stats.duration_hours_minutes",
            &[("hours", &hours), ("minutes", &parts.minutes)],
        ),
    };
    text.into()
}

/// 图上的一根柱子。
///
/// 文案（`readout`）在算快照时就拼好塞进来：图表那些闭包要求 `'static`，拿不到 `&App`，
/// 而本地化恰恰需要它。与其为图表造一套延迟取词的机制，不如在唯一一处有 `cx` 的地方
/// 把话说完。
#[derive(Clone)]
pub struct Bar {
    /// 轴上那一格。**用 `MM-DD`**：30 天的窗口里它不重复（只写「日」会跨月撞车，
    /// 撞上的两根会被当成同一格），而完整的 `YYYY-MM-DD` 摆在轴上太长。
    pub band: SharedString,
    /// 完整日期，给 tooltip 当标题。
    pub day: SharedString,
    /// 分钟。按分钟画而不是按小时：一天的阅读几乎总在「不到一小时」这一档里，
    /// 用小时当单位全年的柱子都会挤在 0 与 1 之间。
    pub minutes: f64,
    pub readout: SharedString,
    /// 是不是今天。这一根要画得比别人实，它是全图唯一的焦点。
    pub today: bool,
}

/// 时段图上的一格（窗口内某个本地小时的合计）。
///
/// 与 [`Bar`] 同一条理由：`readout` 在这里就说成本地化的句子，图表那些闭包要 `'static`。
#[derive(Clone)]
pub struct HourCell {
    /// 轴上的那一格，只有小时数（`0`…`23`）：轴上排 24 个「14 点」会叠成一团，
    /// 而小节标题已经说了这一图读的是「几点」。
    pub band: SharedString,
    /// tooltip 的标题，本地化过的那一句（中文「14 点」、英文「14:00」）。
    pub title: SharedString,
    /// 分钟，与趋势图同一个单位：两张贴在一起时读者才不用换算。
    pub minutes: f64,
    /// 窗口里最高的那一格。全零时没有。
    pub peak: bool,
    pub readout: SharedString,
}

/// 星期几那一节轴上的一格（雷达图的一根辐条）。
#[derive(Clone)]
pub struct WeekdaySpoke {
    /// 本地化的星期名，同时是 tooltip 的标题。
    pub name: SharedString,
    /// 那一个星期几的**平均**日读数（分钟），按窗口里它出现的天数摊平 ——
    /// 没读的那几个周六也摊进去。不摊就成了只看峰值的错觉，而这一节要回答的是
    /// 「哪一天通常在读」。
    pub minutes: f64,
    /// 平均读数的本地化句子，给 tooltip 念。
    pub readout: SharedString,
}

/// 星期几那一节的整份内容：七根辐条，加上那一句结论要说的那一格。
///
/// `busiest` 单列一份是因为「哪一天读得最多」这个结论**必须**有，而它存在的那一刻就
/// 有（这一节只在窗口有读数时才造）。把它做成辐条上的一个 `top: bool` 或者
/// `Option<WeekdaySpoke>`，界面那边就得写一条永远走不到的分支。
#[derive(Clone)]
pub struct WeekdayPattern {
    pub spokes: Vec<WeekdaySpoke>,
    pub busiest: WeekdaySpoke,
}

/// 一屏要画的全部数。取一次，画到下一次重取为止。
pub struct Report {
    /// 窗口内的汇总（含补零的每日桶、连续天数、活跃天数）。
    pub window: ReadingSummary,
    /// 跨全时段的合计。与 `window` 分开摆是故意的：「累计」不该跟着筛选范围变。
    pub totals: ReadingTotals,
    /// 读过的天数（全时段）。
    pub days_read: usize,
    /// 库里的批注条数。
    pub notes: i64,
    pub bars: Vec<Bar>,
    /// 窗口内时间最多的几本书。
    pub ranks: Vec<ReadingRank>,
    /// 24 格的本地小时分布，已经换成图直接画的形状。
    pub hours: Vec<HourCell>,
    /// 星期几那一节；`None` = 窗口太窄或活跃天数太少，画出来是噪声。
    /// 见 [`WEEKDAY_MIN_DAYS`] 与 [`WEEKDAY_MIN_ACTIVE_DAYS`]。
    pub weekdays: Option<WeekdayPattern>,
}

impl Report {
    /// 取一次快照。`tz_offset_secs` 由界面侧给（core 不读系统时区，见 `folio_core::infra::time`）。
    ///
    /// 每一条都是「读不出来就别拦这一屏」：统计页上任何一块都不该因为一条查询失败而
    /// 变成空白或一次 panic，所以各块各自 `or_log` 交出默认值，声音留在日志里。
    /// 全库打不开时上面那层给的是 `None`，那一屏说的是另一句话（见 `stats.no_store`）。
    pub fn load(store: &Arc<Store>, range: Range, tz_offset_secs: i64, cx: &App) -> Report {
        let days = range.days();
        let window = store
            .reading_summary(days, tz_offset_secs)
            .unwrap_or_else(|err| {
                log_failed("统计：读窗口汇总", err);
                empty_summary(days, tz_offset_secs)
            });
        let hours = read_or(
            "统计：读时段分布",
            store.hour_distribution(days, tz_offset_secs),
        );
        Report {
            hours: hours_for(&hours, cx),
            weekdays: weekdays_for(&window, tz_offset_secs, cx),
            bars: bars_for(&window, tz_offset_secs, cx),
            window,
            ranks: read_or(
                "统计：读书目排行",
                store.top_books(days, tz_offset_secs, RANK_LIMIT),
            ),
            totals: read_or("统计：读全时段合计", store.reading_totals()),
            days_read: read_or("统计：读天数", store.reading_days(tz_offset_secs)),
            notes: read_or("统计：读批注条数", store.annotation_count()),
        }
    }

    /// 窗口里有没有任何东西。全零不是错误，但它决定这一屏画报告还是画引导。
    pub fn is_empty(&self) -> bool {
        self.window.sessions == 0
    }

    /// 榜单那一节画不画。一本的时候，那本书的书名在「今天」下面已经说过一遍了。
    pub fn shows_ranks(&self) -> bool {
        self.ranks.len() > 1
    }

    /// 时段图画不画。见 [`HOURS_MIN_SESSIONS`]。
    pub fn shows_hours(&self) -> bool {
        self.hours.len() == folio_core::infra::time::HOURS_PER_DAY
            && self.window.sessions >= HOURS_MIN_SESSIONS
    }

    /// 时段图里最高的那一格是几点；全零时 `None`。
    ///
    /// 深浅与高低都由图自己画了，这个数只剩一件事要说：那句「最常在读 X 点前后」。
    /// 那一格只在全表有读数时才会被点亮，见 [`hours_for`]。
    pub fn peak_hour(&self) -> Option<usize> {
        self.hours.iter().position(|cell| cell.peak)
    }
}

/// 榜单取几本。五本是「一屏看完不用滚」与「看得出错峰」之间的那个点。
const RANK_LIMIT: usize = 5;

/// 时段图画出来的门槛（窗口内的会话段数），理由见 [`Report::shows_hours`]。
pub const HOURS_MIN_SESSIONS: i64 = 20;

/// 星期几那一节要的最小窗口（天）。少于这个数，七根辐条各摊不到 4 个样本，
/// 那张多边形读出来是噪声而不是节律 —— 与 [`HOURS_MIN_SESSIONS`] 同一个判据。
const WEEKDAY_MIN_DAYS: usize = 28;

/// 星期几那一节要的窗口内活跃天数。整窗口只读了两三天时，七个星期几里有五六个
/// 摊不到一次，画出来的形状说的是「几乎没读」，而那已经是「今天」那一块的话了。
const WEEKDAY_MIN_ACTIVE_DAYS: usize = 10;

/// 一周几天。core 只给了 `HOURS_PER_DAY`，星期这一头是界面自己的划分。
const WEEKDAYS_PER_WEEK: usize = 7;

/// 一条读不出来的统计查询：写一句日志，交出默认值。
///
/// 不用 `ui::shared::error::or_log` 而在这开一个小口子，是因为那一条收 `anyhow::Result`
/// 而这里手上是 `folio_core::Result` —— 为七条只读查询各挂一次 `.context()` 再把同一句话
/// 当第二个参数重抄一遍，换来的只是日志前缀的位置不同。
/// 要守的两条规矩这里都守住了：**失败一定留声音**，以及**默认值不会伪装成数据**。
fn read_or<T: Default>(what: &str, result: folio_core::Result<T>) -> T {
    result.unwrap_or_else(|err| {
        log_failed(what, err);
        Default::default()
    })
}

/// 把 core 那条错误交给 anyhow 再打 `{:#}`。
///
/// 直接 `format!("{err}")` 只念最外一句，而 core 那层的约定是「底层错误留在
/// `#[source]` 链上」—— 恰好把唯一查得到的信息丢掉。「sqlite 打不开」与「没有这本书」
/// 在屏幕上可能都是这句默认值，日志里必须分得开。收的是值而不是引用，也是为了这条：
/// `anyhow::Error` 要 `'static`，借来的那条活不到转换那一刻。
fn log_failed(what: &str, err: folio_core::Error) {
    let chain: anyhow::Error = err.into();
    tracing::warn!("{what}：{chain:#}");
}

/// 一条查询都没成功时的形状：窗口那几格仍然补零，图表因此仍然画得出一段轴，
/// 而不是塌成「什么都没有」。
fn empty_summary(days: usize, tz_offset_secs: i64) -> ReadingSummary {
    let today = local_day(time::now(), tz_offset_secs);
    ReadingSummary {
        days: (today - days as i64 + 1..=today)
            .map(|day| DailyReading {
                day: civil_date(day),
                seconds: 0,
                chars: 0,
            })
            .collect(),
        seconds: 0,
        sessions: 0,
        books: 0,
        active_days: 0,
        streak: 0,
        longest_streak: 0,
    }
}

/// 把补过零的每日桶换成图表要的那串柱子。
fn bars_for(sum: &ReadingSummary, tz_offset_secs: i64, cx: &App) -> Vec<Bar> {
    // 今天算一次就好：逐根柱子问一遍会把同一句话问 30 次。
    let today = civil_date(local_day(time::now(), tz_offset_secs));
    sum.days
        .iter()
        .map(|day| Bar {
            // `civil_date` 只会产出 `YYYY-MM-DD`（全 ASCII），剪掉前 5 个字节就是 `MM-DD`。
            band: SharedString::from(day.day[5..].to_string()),
            day: day.day.clone().into(),
            minutes: day.seconds as f64 / 60.0,
            readout: duration(cx, day.seconds),
            today: day.day == today,
        })
        .collect()
}

/// 把 24 格的秒数换成时段图要画的那串格子。
///
/// 单位是分钟而不是秒，与 [`bars_for`] 那根趋势柱同一条刻度：两张贴着摆时读者不必
/// 先换算一遍才看得出「晚上读得比早上多」。
///
/// 峰值只点亮一格，并列时取最早那一个钟点：「最常在读几点」这句结论只能说一个数，
/// 说两个就得在文案里加一个「或」。全零时一格都不亮 —— 那是「还没有读数」，
/// 不是「凌晨零点最常被读」。
fn hours_for(seconds: &[i64], cx: &App) -> Vec<HourCell> {
    let peak = seconds
        .iter()
        .copied()
        .max()
        .filter(|top| *top > 0)
        .and_then(|top| seconds.iter().position(|secs| *secs == top));
    seconds
        .iter()
        .enumerate()
        .map(|(hour, secs)| HourCell {
            band: SharedString::from(hour.to_string()),
            title: SharedString::from(tf(cx, "stats.hour_cell", &[("hour", &hour)])),
            minutes: *secs as f64 / 60.0,
            peak: peak == Some(hour),
            readout: duration(cx, *secs),
        })
        .collect()
}

/// 把窗口的每日桶按星期几归并成雷达图的那七格。
///
/// 星期既不查库也不排日历：[`ReadingSummary::days`] 那条约定是**补过零的连续桶、
/// 最后一条是今天**（`store::types` 里钉着的），于是第 `ix` 格就是「今天减 `n-1-ix` 天」，
/// 而历元日号模 7 就是星期几。归并因此只是那条约定上的一段纯算术，与 core 里
/// 连续天数、活跃天数那几个派生数同一种算法。约定哪天变了，
/// `the_weekday_spokes_agree_with_a_calendar` 那条用 chrono 独立核一遍的测试会先红。
///
/// 每格交出去的是**平均**日读数（含没读的那几天），不是总量：30 天窗口里周六出现
/// 5 次、周三出现 4 次，比总量就等于偷偷把「多出来的那一天」算成了读得多。
fn weekdays_for(sum: &ReadingSummary, tz_offset_secs: i64, cx: &App) -> Option<WeekdayPattern> {
    let n = sum.days.len();
    if n < WEEKDAY_MIN_DAYS || sum.active_days < WEEKDAY_MIN_ACTIVE_DAYS {
        return None;
    }
    let today = local_day(time::now(), tz_offset_secs);
    let mut seconds = [0i64; WEEKDAYS_PER_WEEK];
    let mut samples = [0usize; WEEKDAYS_PER_WEEK];
    for (ix, day) in sum.days.iter().enumerate() {
        let spoke = weekday_of_epoch_day(today - (n - 1 - ix) as i64);
        seconds[spoke] += day.seconds;
        samples[spoke] += 1;
    }
    // 窗口至少 28 天 ⇒ 每个星期几都摊得到 4 次，所以下面这个分母不会是零。
    // 摊几次不进那份格子：图只画平均值，而「摊了几次」这件事测试直接从日历数。
    let average: Vec<i64> = (0..WEEKDAYS_PER_WEEK)
        .map(|spoke| seconds[spoke] / samples[spoke] as i64)
        .collect();
    let spokes: Vec<WeekdaySpoke> = (0..WEEKDAYS_PER_WEEK)
        .map(|spoke| WeekdaySpoke {
            name: weekday_name(spoke, cx),
            minutes: average[spoke] as f64 / 60.0,
            readout: duration(cx, average[spoke]),
        })
        .collect();
    // 活跃天数已过门槛 ⇒ 至少有一格平均大于零 ⇒ `max_by` 一定给出东西。
    let busiest = spokes
        .iter()
        .max_by(|a, b| a.minutes.total_cmp(&b.minutes))
        .cloned()
        .expect("七根辐条总有一根");
    Some(WeekdayPattern { spokes, busiest })
}

/// 历元日号 → 星期几，Monday = 0 … Sunday = 6。
///
/// 历元第 0 天（1970-01-01）是星期四，所以先偏 3 天再取模。用 `rem_euclid` 而不是 `%`：
/// 1970 年以前的日号取模会是负数，而它要当索引。
fn weekday_of_epoch_day(epoch_day: i64) -> usize {
    (epoch_day + 3).rem_euclid(WEEKDAYS_PER_WEEK as i64) as usize
}

/// 星期几那一格的轴标签。
///
/// 与 [`Range::label`] 同一条理由写成逐个字面量的 `tr` 调用（不返回键名）：
/// `ui/shared/l10n.rs` 那条扫描测试只认字面量，而键解不出来时界面会把键本身画出来。
fn weekday_name(spoke: usize, cx: &App) -> SharedString {
    match spoke {
        0 => tr(cx, "stats.weekday_mon"),
        1 => tr(cx, "stats.weekday_tue"),
        2 => tr(cx, "stats.weekday_wed"),
        3 => tr(cx, "stats.weekday_thu"),
        4 => tr(cx, "stats.weekday_fri"),
        5 => tr(cx, "stats.weekday_sat"),
        _ => tr(cx, "stats.weekday_sun"),
    }
}

#[cfg(test)]
mod tests {
    //! 快照那一步：读数按语言换形状，库里那几条查询到界面手上是什么形状。
    //!
    //! 时区固定按东八区算而不是取设备的那个：断言里那些「今天 / 昨天」必须与查询用的
    //! 日界线是同一份，跟着测试机的时钟走就会在某些时刻红掉。日期全部相对今天算，
    //! 与 `store/reading.rs` 那批测试同一条路子。

    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use folio_core::{Config, DocMeta, Format, Language, Locale, Store, infra::time};
    use gpui_kit::{App, AppContext as _, Entity, SharedString, TestAppContext};

    use super::{Bar, HOURS_MIN_SESSIONS, Range, Report, WEEKDAYS_PER_WEEK, duration};
    use crate::ui::features::stats::StatsView;
    use crate::ui::shared::config_model::ConfigHolder;

    const TZ: i64 = 8 * 3600;

    fn meta(title: &str) -> DocMeta {
        DocMeta {
            title: title.into(),
            authors: vec!["作者甲".into()],
            format: Format::Epub,
            size: 1,
        }
    }

    fn path(name: &str) -> PathBuf {
        Path::new("/books").join(name)
    }

    /// 某一天正午的 Unix 秒。
    fn noon(day_offset: i64) -> i64 {
        let today = time::local_day(time::now(), TZ);
        time::day_start(today + day_offset, TZ) + 12 * 3600
    }

    /// 某一天某个小时起点的 Unix 秒（给时段图按小时分桶用）。
    fn at_hour(hour: i64, day_offset: i64) -> i64 {
        let today = time::local_day(time::now(), TZ);
        time::day_start(today + day_offset, TZ) + hour * 3600
    }

    /// 某个历元日号是周几，Monday = 0 … Sunday = 6 —— **由 chrono 的日历算**，
    /// 与被测的那段 `weekday_of_epoch_day` 完全无关。取正午那一刻是为了让日界线
    /// 落在测试用的那个固定时区里，不与 UTC 那天混掉。
    fn calendar_weekday(epoch_day: i64) -> usize {
        use chrono::{Datelike, FixedOffset};
        let at = time::day_start(epoch_day, TZ) + 12 * 3600;
        let offset = FixedOffset::east_opt(TZ as i32).expect("东八区是个合法的固定偏移");
        chrono::DateTime::from_timestamp(at, 0)
            .expect("这些秒数在 chrono 的日期范围内")
            .with_timezone(&offset)
            .weekday()
            .num_days_from_monday() as usize
    }

    /// 把界面语言装进这个上下文。`duration` 要说哪门语言的句子由配置实体决定
    /// （不是进程全局，见 `ui/shared/l10n` 那份说明），所以这几条绕不开一次装配。
    fn set_locale(cx: &mut TestAppContext, locale: Locale) {
        cx.update(|cx: &mut App| {
            ConfigHolder::install(
                Config {
                    language: Language { locale },
                    ..Default::default()
                },
                cx,
            );
        });
    }

    #[gpui_kit::test]
    fn the_duration_sentence_changes_shape_with_the_language(cx: &mut TestAppContext) {
        set_locale(cx, Locale::ZhHans);
        assert_eq!(cx.read(|cx| duration(cx, 5_040)).as_str(), "1 小时 24 分钟");
        assert_eq!(cx.read(|cx| duration(cx, 45)).as_str(), "45 秒");
        assert_eq!(cx.read(|cx| duration(cx, 600)).as_str(), "10 分钟");
        // 整点不 trailing 一个「0 分钟」，那读起来像话没说完。
        assert_eq!(cx.read(|cx| duration(cx, 7_200)).as_str(), "2 小时");

        set_locale(cx, Locale::En);
        assert_eq!(cx.read(|cx| duration(cx, 5_040)).as_str(), "1h 24m");

        set_locale(cx, Locale::Ja);
        assert_eq!(cx.read(|cx| duration(cx, 5_040)).as_str(), "1 時間 24 分");
    }

    /// 空库：窗口那几格仍然补零到 14 根。趋势图因此仍然画得出一段轴，而不是塌成
    /// 「这里什么也没有」—— 全零时界面说的是「还没读」，不是「读不到」。
    #[gpui_kit::test]
    fn an_empty_library_still_hands_the_chart_a_full_axis(cx: &mut TestAppContext) {
        set_locale(cx, Locale::ZhHans);
        let store = Arc::new(Store::in_memory().unwrap());
        let report = cx.update(|cx| Report::load(&store, Range::Fourteen, TZ, cx));
        assert!(report.is_empty());
        assert_eq!(report.bars.len(), 14, "空库也要有 14 根柱子");
        assert!(report.bars.iter().all(|bar| bar.minutes == 0.0));
        assert!(report.ranks.is_empty(), "没有读数就没有排行");
        assert!(!report.shows_ranks());
        assert!(!report.shows_hours(), "零段记录不该画时段图");
        assert_eq!(report.peak_hour(), None, "全零时没有『最高那一格』");
        assert!(
            report.weekdays.is_none(),
            "空库不该画一张全塌在圆心的多边形"
        );
    }

    /// 读数到手上的样子：哪根是今天、排行谁在前、时段表满 24 格。
    #[gpui_kit::test]
    fn a_library_with_sessions_becomes_a_plottable_report(cx: &mut TestAppContext) {
        set_locale(cx, Locale::ZhHans);
        let store = Arc::new(Store::in_memory().unwrap());
        let long = store
            .add_book(&path("long.epub"), &meta("漫长的书"))
            .unwrap();
        let short = store
            .add_book(&path("short.epub"), &meta("短的书"))
            .unwrap();
        store
            .record_session(long, noon(0), noon(0) + 3_600, 0)
            .unwrap();
        store
            .record_session(short, noon(-1), noon(-1) + 900, 0)
            .unwrap();

        let report = cx.update(|cx| Report::load(&store, Range::Fourteen, TZ, cx));
        assert!(!report.is_empty());
        assert_eq!(report.window.seconds, 4_500);
        assert_eq!(report.window.streak, 2, "今天与昨天连着");
        assert_eq!(report.days_read, 2);
        assert_eq!(report.totals.seconds, 4_500);

        let today: &Bar = report.bars.last().expect("窗口最后一格是今天");
        assert!(today.today, "最后一根要认得出是今天，否则全图没有焦点");
        assert_eq!(today.minutes, 60.0);
        assert_eq!(today.band.len(), 5, "轴上是 MM-DD 那五个字符");
        assert_eq!(
            today.readout.as_str(),
            "1 小时",
            "tooltip 念的是本地化过的句子"
        );
        assert_eq!(
            report.bars.iter().filter(|bar| bar.today).count(),
            1,
            "只亮一根"
        );
        assert_eq!(
            report.bars.iter().filter(|bar| bar.minutes == 0.0).count(),
            12
        );

        assert_eq!(report.ranks.len(), 2);
        assert_eq!(report.ranks[0].title, "漫长的书", "花时间多的在前");
        assert!(report.shows_ranks(), "两本才排得出榜");

        assert_eq!(report.hours.len(), time::HOURS_PER_DAY);
        assert_eq!(
            report.hours[12].minutes, 75.0,
            "两段都从正午开始：4 500 秒摊到第 12 格"
        );
        assert_eq!(report.peak_hour(), Some(12), "峰值那格点在第 12 格");
        assert!(!report.shows_hours(), "两段记录还没到画时段图的门槛");
        assert!(
            report.weekdays.is_none(),
            "14 天档每个星期几摊不到两次，画出来是噪声"
        );
    }

    /// 时段图的门槛（[`super::HOURS_MIN_SESSIONS`]）越过之后那一节才该出现。
    #[gpui_kit::test]
    fn the_hour_strip_waits_until_there_is_a_pattern_to_show(cx: &mut TestAppContext) {
        set_locale(cx, Locale::ZhHans);
        let store = Arc::new(Store::in_memory().unwrap());
        let book = store.add_book(&path("a.epub"), &meta("A")).unwrap();
        for ix in 0..HOURS_MIN_SESSIONS {
            let at = noon(0) + i64::from(ix) * 60;
            store.record_session(book, at, at + 30, 0).unwrap();
        }
        let report = cx.update(|cx| Report::load(&store, Range::Fourteen, TZ, cx));
        assert!(report.shows_hours(), "够多的段就该画出规律");
    }

    /// 时段图上那一格的三个说法：轴上的裸钟点、tooltip 的本地化标题、本地化的读数。
    ///
    /// 轴与标题刻意不同：24 格轴上排 24 个「21 点」会叠成一团，而小节标题已经说了
    /// 这一图读的是几点，轴只需要那一个数。
    #[gpui_kit::test]
    fn an_hour_cell_carries_its_own_label_and_reading(cx: &mut TestAppContext) {
        set_locale(cx, Locale::ZhHans);
        let store = Arc::new(Store::in_memory().unwrap());
        let book = store.add_book(&path("a.epub"), &meta("A")).unwrap();
        // 午后 13 点读 20 段各 60 秒，晚上 21 点读 3 段各 900 秒：后者高，且段数少。
        for _ in 0..HOURS_MIN_SESSIONS {
            let at = at_hour(13, 0);
            store.record_session(book, at, at + 60, 0).unwrap();
        }
        for _ in 0..3 {
            let at = at_hour(21, 0);
            store.record_session(book, at, at + 900, 0).unwrap();
        }
        let report = cx.update(|cx| Report::load(&store, Range::Fourteen, TZ, cx));

        let evening = &report.hours[21];
        assert_eq!(evening.band.as_str(), "21", "轴上是裸钟点");
        assert_eq!(
            evening.title.as_str(),
            "21 点",
            "tooltip 说的是本地化的那一句"
        );
        assert_eq!(evening.minutes, 45.0, "分钟，与趋势柱同一单位");
        assert_eq!(evening.readout.as_str(), "45 分钟");
        assert!(evening.peak, "晚上那格最高");
        assert!(!report.hours[13].peak, "段数多不等于读得久");
        assert_eq!(report.hours[0].minutes, 0.0);
        assert!(!report.hours[0].peak, "空的格不亮");
        assert_eq!(report.peak_hour(), Some(21));
        assert_eq!(
            report.hours.iter().filter(|cell| cell.peak).count(),
            1,
            "「最常在读几点」只能说一个数"
        );
    }

    /// 星期几那一节的归并要跟一份独立日历对得上。
    ///
    /// 刻意不用 [`super::weekday_of_epoch_day`] 自己验自己：那一串格子落在周几由 chrono
    /// 的日历另算一遍，两条路子对不上就是这段算术偏了。偏一天最容易藏在「今天」那格，
    /// 而那格正是这一节要说的那个周末。分母也一样：摊几次由日历数出来，不问快照自己
    /// 说了什么 —— 快照把「哪几天」记错时，它报出来的次数会跟着一起错。
    #[gpui_kit::test]
    fn the_weekday_spokes_agree_with_a_calendar(cx: &mut TestAppContext) {
        set_locale(cx, Locale::ZhHans);
        let store = Arc::new(Store::in_memory().unwrap());
        let book = store.add_book(&path("a.epub"), &meta("A")).unwrap();
        // 连着 11 天，越靠近今天读得越久（今天那段 6_600 秒）。
        for k in 0..11i64 {
            let secs = (11 - k) * 600;
            store
                .record_session(book, noon(-k), noon(-k) + secs, 0)
                .unwrap();
        }
        let report = cx.update(|cx| Report::load(&store, Range::Thirty, TZ, cx));
        let pattern = report
            .weekdays
            .expect("30 天窗口与 11 个活跃天就该画这一节");
        let spokes = &pattern.spokes;
        assert_eq!(spokes.len(), WEEKDAYS_PER_WEEK);
        assert_eq!(
            spokes
                .iter()
                .map(|spoke| spoke.name.to_string())
                .collect::<Vec<String>>(),
            vec!["周一", "周二", "周三", "周四", "周五", "周六", "周日"],
            "轴从周一排到周日，顺序由文案表说了算"
        );

        let today = time::local_day(time::now(), TZ);
        // 窗口那 30 天里每个星期几各摊几次：七者之和是 30，每一根 4 或 5。
        let occurrences: Vec<usize> = (0..WEEKDAYS_PER_WEEK)
            .map(|ix| {
                (0..30i64)
                    .filter(|k| calendar_weekday(today - k) == ix)
                    .count()
            })
            .collect();
        assert_eq!(occurrences.iter().sum::<usize>(), 30);
        assert!(
            occurrences.iter().all(|times| (4..=5).contains(times)),
            "30 天里每个星期几该摊 4 或 5 次，实得{occurrences:?}"
        );

        for (ix, spoke) in spokes.iter().enumerate() {
            let seeded: Vec<i64> = (0..11i64)
                .filter(|k| calendar_weekday(today - k) == ix)
                .map(|k| (11 - k) * 600)
                .collect();
            assert_eq!(
                spoke.minutes > 0.0,
                !seeded.is_empty(),
                "第 {ix} 根辐条有没有读数与日历不符"
            );
            // 摊的是**整个窗口**里那几天的平均（没读的那几天也算进分母）：
            // 只摊有读的那些天就成了「哪天真读了」，而不是「哪一天通常在读」。
            let total: i64 = seeded.iter().sum();
            let expect = total as f64 / occurrences[ix] as f64 / 60.0;
            assert!(
                (spoke.minutes - expect).abs() < 1e-9,
                "第 {ix} 根辐条的平均读数：{} vs {expect}",
                spoke.minutes
            );
        }

        // 结论那一格：它就是平均最高的那根辐条，并且话说成了本地化的那一句。
        let best = spokes
            .iter()
            .map(|spoke| spoke.minutes)
            .fold(f64::MIN, f64::max);
        assert_eq!(
            pattern.busiest.minutes, best,
            "结论要说的就是最高的那根辐条"
        );
        assert!(
            !pattern.busiest.readout.is_empty(),
            "那一格得能念出一句人话"
        );
    }

    /// 两个门槛各拦一样：窗口太窄摊不出平均，活跃天数太少画出来是一片空。
    #[gpui_kit::test]
    fn the_weekday_section_stays_out_until_the_window_can_average(cx: &mut TestAppContext) {
        set_locale(cx, Locale::ZhHans);
        let busy = Arc::new(Store::in_memory().unwrap());
        let book = busy.add_book(&path("a.epub"), &meta("A")).unwrap();
        // 连着 11 天、每天两段：段数过了时段图那道门槛（20 段），活跃天数也过了
        // 星期几那道（10 天），差的只剩窗口宽度。
        for k in 0..11i64 {
            for shift in 0..2i64 {
                let at = noon(-k) + shift * 30 * 60;
                busy.record_session(book, at, at + 600, 0).unwrap();
            }
        }
        let narrow = cx.update(|cx| Report::load(&busy, Range::Fourteen, TZ, cx));
        assert!(
            narrow.weekdays.is_none(),
            "14 天档每根辐条摊不到两次，画出来是噪声"
        );
        assert!(
            narrow.shows_hours(),
            "同一批读数够画时段图：两节的门槛各管各的"
        );

        // 同一个库，只把窗口拉宽到 30 天，那一节就该出现。
        let wide = cx.update(|cx| Report::load(&busy, Range::Thirty, TZ, cx));
        assert!(
            wide.weekdays.is_some(),
            "窗口一宽，每个星期几摊得到 4 次以上，节律就读得出来了"
        );

        let sparse = Arc::new(Store::in_memory().unwrap());
        let book = sparse.add_book(&path("b.epub"), &meta("B")).unwrap();
        for k in 0..3i64 {
            sparse
                .record_session(book, noon(-k), noon(-k) + 600, 0)
                .unwrap();
        }
        let wide_but_thin = cx.update(|cx| Report::load(&sparse, Range::Thirty, TZ, cx));
        assert!(
            wide_but_thin.weekdays.is_none(),
            "30 天只读了 3 天：七格里四格是空的，那张形状说的是「几乎没读」"
        );
    }

    /// 排行只有一本时那一节整块不画：书名在「今天」附近已经说过一遍，再摆一行
    /// 「这本书花了全部时间」是废话占了一格。
    #[gpui_kit::test]
    fn one_book_is_not_a_ranking(cx: &mut TestAppContext) {
        set_locale(cx, Locale::ZhHans);
        let store = Arc::new(Store::in_memory().unwrap());
        let book = store.add_book(&path("a.epub"), &meta("A")).unwrap();
        store
            .record_session(book, noon(0), noon(0) + 600, 0)
            .unwrap();
        let report = cx.update(|cx| Report::load(&store, Range::Fourteen, TZ, cx));
        assert_eq!(report.ranks.len(), 1);
        assert!(!report.shows_ranks());
    }

    /// 换档位换的是窗口宽度，其余读数各归各：累计不跟着筛选变。
    #[gpui_kit::test]
    fn switching_the_range_widens_the_window_but_not_the_totals(cx: &mut TestAppContext) {
        set_locale(cx, Locale::ZhHans);
        let store = Arc::new(Store::in_memory().unwrap());
        let book = store.add_book(&path("a.epub"), &meta("A")).unwrap();
        store
            .record_session(book, noon(0), noon(0) + 600, 0)
            .unwrap();
        store
            .record_session(book, noon(-20), noon(-20) + 600, 0)
            .unwrap();

        let narrow = cx.update(|cx| Report::load(&store, Range::Seven, TZ, cx));
        let wide = cx.update(|cx| Report::load(&store, Range::Thirty, TZ, cx));
        assert_eq!(narrow.window.seconds, 600, "近 7 天看不见 20 天前那段");
        assert_eq!(wide.window.seconds, 1_200);
        assert_eq!(narrow.bars.len(), 7);
        assert_eq!(wide.bars.len(), 30);
        assert_eq!(
            narrow.totals.seconds, wide.totals.seconds,
            "累计是全时段的，跟着筛选走就不叫累计"
        );
    }

    /// `band` 重复会让两根柱子被图表并成一格（那是同一条类目），所以这条要钉住。
    #[gpui_kit::test]
    fn the_band_labels_are_unique_across_a_month_boundary(cx: &mut TestAppContext) {
        set_locale(cx, Locale::ZhHans);
        let store = Arc::new(Store::in_memory().unwrap());
        let report = cx.update(|cx| Report::load(&store, Range::Thirty, TZ, cx));
        let bands: Vec<String> = report
            .bars
            .iter()
            .map(|bar: &Bar| bar.band.to_string())
            .collect();
        let unique: HashSet<&String> = bands.iter().collect();
        assert_eq!(
            unique.len(),
            bands.len(),
            "30 天窗口里 `MM-DD` 不该出现两次，撞车的两根会被并成一格：{bands:?}"
        );
    }

    /// 没有书库时 `refresh` 该把快照交回 `None`，而不是拿着旧数据继续画。
    ///
    /// 这一格要说的是「读不到书库」。与「库里没有阅读记录」混起来，读者会以为是自己
    /// 把书读空了 —— 而 `widgets::empty_state` 顶上立的就是这条区分。
    #[gpui_kit::test]
    fn a_missing_library_leaves_the_view_with_nothing_to_plot(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let view: Entity<StatsView> = cx.update(|cx| cx.new(|_| StatsView::new()));
        cx.update(|cx| view.update(cx, |stats, cx| stats.refresh(cx)));
        // 没 `app_store::install` 过，所以这里没有书库句柄可读。
        assert!(
            cx.read(|cx| view.read(cx).report.is_none()),
            "读不到书库时不该留一份看起来像数据的空报告"
        );
    }

    /// 有书库时进来就该拿到一份快照，档位切换要立刻反映到快照上。
    ///
    /// 钉的是「取数时机」这条接线：`render` 不查库，所以库里的新会话只能靠这几处
    /// `refresh` 上屏。漏掉哪一处，读者的表现都是「读完切回来，数没变」。
    #[gpui_kit::test]
    fn the_view_redraws_when_the_shell_tells_it_to_look_again(cx: &mut TestAppContext) {
        set_locale(cx, Locale::ZhHans);
        // 库先装进去，再通过 `app_store` 那份句柄写：`install` 收的是值，而 `Store`
        // 不可复制，手里再留一份 `Arc` 就得靠 handle 拿回来的那一份。
        cx.update(|cx: &mut App| {
            crate::ui::shared::app_store::install(Store::in_memory().unwrap(), cx)
        });
        let store = cx.update(|cx| crate::ui::shared::app_store::handle(cx).unwrap());
        let book = store.add_book(&path("a.epub"), &meta("A")).unwrap();
        store
            .record_session(book, noon(0), noon(0) + 600, 0)
            .unwrap();

        let view: Entity<StatsView> = cx.update(|cx| cx.new(|_| StatsView::new()));
        cx.update(|cx| view.update(cx, |stats, cx| stats.refresh(cx)));
        let first = cx.read(|cx| {
            view.read(cx)
                .report
                .as_ref()
                .map(|report| report.window.seconds)
        });
        assert_eq!(first, Some(600), "进来就该拿到库里的读数");

        // 又读了一段：不重取就不该看见它（快照是快照），重取了才看见。
        store
            .record_session(book, noon(0) + 700, noon(0) + 1_000, 0)
            .unwrap();
        assert_eq!(
            cx.read(|cx| view.read(cx).report.as_ref().unwrap().window.seconds),
            600,
            "`render` 不该自己查库：没被叫过重取就还是进来那一份"
        );
        cx.update(|cx| view.update(cx, |stats, cx| stats.refresh(cx)));
        assert_eq!(
            cx.read(|cx| view.read(cx).report.as_ref().unwrap().window.seconds),
            900,
            "叫一次重取，数就该跟上来"
        );

        // 换档位走的是同一条重取路（30 天档把 20 天前那段也收进来）。
        store
            .record_session(book, noon(-20), noon(-20) + 300, 0)
            .unwrap();
        cx.update(|cx| view.update(cx, |stats, cx| stats.pick(Range::Thirty, cx)));
        let picked = cx.read(|cx| {
            let view = view.read(cx);
            (view.range, view.report.as_ref().map(|r| r.window.seconds))
        });
        assert_eq!(picked.0, Range::Thirty, "档位要真的切过去");
        assert_eq!(picked.1, Some(1_200), "切档立刻重取，不留一张过期的图");
    }

    /// 榜单行点下去要说得出对象：那是一本书，打开它走的是外壳已有那条 `open_reading`。
    /// 这条把事件本身钉住（路由的落点在 `shell::tests` 里钉）。
    #[gpui_kit::test]
    fn a_rank_row_emits_the_book_it_names(cx: &mut TestAppContext) {
        set_locale(cx, Locale::ZhHans);
        let store = Arc::new(Store::in_memory().unwrap());
        let wanted = store
            .add_book(&path("b.epub"), &meta("要点开的那本"))
            .unwrap();
        let other = store.add_book(&path("a.epub"), &meta("另一本")).unwrap();
        store
            .record_session(wanted, noon(0), noon(0) + 900, 0)
            .unwrap();
        store
            .record_session(other, noon(0), noon(0) + 300, 0)
            .unwrap();

        let report = cx.update(|cx| Report::load(&store, Range::Fourteen, TZ, cx));
        assert_eq!(report.ranks[0].book_id, wanted, "榜首该是花时间多的那本");
        // 榜单行拿到的 `book_id` 就是事件要交出去的数：这一条不靠渲染，
        // 而渲染那一侧的 id 车道在 `mod.rs` 里按同一个数发。
        let id: SharedString = report.ranks[0].book_id.to_string().into();
        assert_eq!(id.as_str(), wanted.to_string());
    }
}
