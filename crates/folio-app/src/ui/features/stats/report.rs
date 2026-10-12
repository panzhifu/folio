//! 「统计」那一屏的数据快照。
//!
//! 分成两份东西是刻意的：这一份只问库要数、并把它们换算成界面能直接画的形状，
//! `mod.rs` 那一侧只画。切开的那条线是**取数时机** —— 快照在「切进这一屏 / 换了时间
//! 范围 / 关掉一本书」这三处重取一次，`render` 一律不碰库。每帧查一遍 sqlite 不会
//! 慢在哪条语句上，它慢在把「画一屏」变成「做一轮 IO」，而那正是渲染帧不该干的事。

use std::sync::Arc;

use folio_core::store::{ReadingRank, ReadingSummary, ReadingTotals, Store};
use folio_core::{DailyReading, civil_date, duration_parts, local_day, time};
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
    /// 24 格的本地小时分布（秒）。
    pub hours: Vec<i64>,
    /// `hours` 里最高的那一格，热力条按它算深浅。
    pub hours_peak: i64,
}

impl Report {
    /// 取一次快照。`tz_offset_secs` 由界面侧给（core 不读系统时区，见 `folio_core::time`）。
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
        Report {
            bars: bars_for(&window, tz_offset_secs, cx),
            hours_peak: 0,
            window,
            ranks: read_or(
                "统计：读书目排行",
                store.top_books(days, tz_offset_secs, RANK_LIMIT),
            ),
            hours: read_or(
                "统计：读时段分布",
                store.hour_distribution(days, tz_offset_secs),
            ),
            totals: read_or("统计：读全时段合计", store.reading_totals()),
            days_read: read_or("统计：读天数", store.reading_days(tz_offset_secs)),
            notes: read_or("统计：读批注条数", store.annotation_count()),
        }
        .with_peak()
    }

    /// 热力条的深浅基准。单独一步是因为 `hours` 那一条在结构体字面量里，取不到自己。
    fn with_peak(mut self) -> Self {
        self.hours_peak = self.hours.iter().copied().max().unwrap_or(0);
        self
    }

    /// 窗口里有没有任何东西。全零不是错误，但它决定这一屏画报告还是画引导。
    pub fn is_empty(&self) -> bool {
        self.window.sessions == 0
    }

    /// 榜单那一节画不画。一本的时候，那本书的书名在「今天」下面已经说过一遍了。
    pub fn shows_ranks(&self) -> bool {
        self.ranks.len() > 1
    }

    /// 时段那条热力图画不画。见 [`HOURS_MIN_SESSIONS`]。
    pub fn shows_hours(&self) -> bool {
        self.hours.len() == folio_core::time::HOURS_PER_DAY
            && self.window.sessions >= HOURS_MIN_SESSIONS
    }
}

/// 榜单取几本。五本是「一屏看完不用滚」与「看得出错峰」之间的那个点。
const RANK_LIMIT: usize = 5;

/// 热力条画出来的门槛（窗口内的会话段数），理由见 [`Report::shows_hours`]。
pub const HOURS_MIN_SESSIONS: i64 = 20;

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

    use folio_core::{Config, DocMeta, Format, Language, Locale, Store, time};
    use gpui_kit::{App, AppContext as _, Entity, SharedString, TestAppContext};

    use super::{Bar, HOURS_MIN_SESSIONS, Range, Report, duration};
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
        store.record_session(long, noon(0), noon(0) + 3_600, 0).unwrap();
        store.record_session(short, noon(-1), noon(-1) + 900, 0).unwrap();

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
        assert_eq!(today.readout.as_str(), "1 小时", "tooltip 念的是本地化过的句子");
        assert_eq!(report.bars.iter().filter(|bar| bar.today).count(), 1, "只亮一根");
        assert_eq!(report.bars.iter().filter(|bar| bar.minutes == 0.0).count(), 12);

        assert_eq!(report.ranks.len(), 2);
        assert_eq!(report.ranks[0].title, "漫长的书", "花时间多的在前");
        assert!(report.shows_ranks(), "两本才排得出榜");

        assert_eq!(report.hours.len(), time::HOURS_PER_DAY);
        assert_eq!(report.hours[12], 4_500, "两段都从正午开始");
        assert_eq!(report.hours_peak, 4_500);
        assert!(!report.shows_hours(), "两段记录还没到画时段图的门槛");
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

    /// 排行只有一本时那一节整块不画：书名在「今天」附近已经说过一遍，再摆一行
    /// 「这本书花了全部时间」是废话占了一格。
    #[gpui_kit::test]
    fn one_book_is_not_a_ranking(cx: &mut TestAppContext) {
        set_locale(cx, Locale::ZhHans);
        let store = Arc::new(Store::in_memory().unwrap());
        let book = store.add_book(&path("a.epub"), &meta("A")).unwrap();
        store.record_session(book, noon(0), noon(0) + 600, 0).unwrap();
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
        store.record_session(book, noon(0), noon(0) + 600, 0).unwrap();
        store.record_session(book, noon(-20), noon(-20) + 600, 0).unwrap();

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
        store.record_session(book, noon(0), noon(0) + 600, 0).unwrap();

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
        store.record_session(book, noon(0) + 700, noon(0) + 1_000, 0).unwrap();
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
        store.record_session(book, noon(-20), noon(-20) + 300, 0).unwrap();
        cx.update(|cx| view.update(cx, |stats, cx| stats.pick(Range::Thirty, cx)));
        let picked = cx.read(|cx| {
            let view = view.read(cx);
            (
                view.range,
                view.report.as_ref().map(|r| r.window.seconds),
            )
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
            .add_book(&path("b.epub"), &meta("要点开的那本")).unwrap();
        let other = store
            .add_book(&path("a.epub"), &meta("另一本")).unwrap();
        store.record_session(wanted, noon(0), noon(0) + 900, 0).unwrap();
        store.record_session(other, noon(0), noon(0) + 300, 0).unwrap();

        let report = cx.update(|cx| Report::load(&store, Range::Fourteen, TZ, cx));
        assert_eq!(report.ranks[0].book_id, wanted, "榜首该是花时间多的那本");
        // 榜单行拿到的 `book_id` 就是事件要交出去的数：这一条不靠渲染，
        // 而渲染那一侧的 id 车道在 `mod.rs` 里按同一个数发。
        let id: SharedString = report.ranks[0].book_id.to_string().into();
        assert_eq!(id.as_str(), wanted.to_string());
    }
}
