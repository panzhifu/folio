//! 热力图那一年的数：把库里的阅读会话折成「一列一周、一行一星期几」的格子。
//!
//! 分成两份东西是刻意的：这一份只问库要数、并把它们换算成界面能直接画的形状，
//! `view` 那一侧只画。切开的那条线是**取数时机** —— 快照在「切进这一屏 / 关掉一本书」
//! 这两处重取一次，`render` 一律不碰库。每帧查一遍 sqlite 不会慢在哪条语句上，它慢在
//! 把「画一屏」变成「做一轮 IO」，而那正是渲染帧不该干的事。
//!
//! 排格子是纯算术，一条 SQL 都不多问：[`Store::reading_summary`] 那条约定是**补过零的
//! 连续日桶、最后一条是今天**，于是「第 `ix` 条 = 今天减 `n-1-ix` 天」直接成立，
//! 星期几与月份都由那个日号算出来。库里没有「按周排」这个查询，也不需要有一个。
//!
//! 这里**不产文案**（月标签只交年月两个数）：格子要画 371 个，把本地化塞进换算那一层
//! 等于在模型里为每个格子问一次词表，而视图拿到数字自己说一句就够了。

use std::sync::Arc;

use folio_core::infra::time;
use folio_core::store::{ReadingSummary, Store};
use folio_core::{civil_date, duration_parts, local_day};
use gpui_kit::{App, SharedString};

use crate::ui::services::l10n::tf;

/// 往回看多少周。53 = 一整年再加一周零头，也是 GitHub 那张图的列数。
pub const WEEKS: usize = 53;
/// 一列几格，也就是几行：周一到周日。
pub const ROWS: usize = 7;
/// 窗口天数。`WEEKS * ROWS` 是**恰好**排满那张网格所需的量，而末列那一周里晚于今天的
/// 那几格由 [`Cell::filled`] 标成不画 —— 于是列数恒等于 `WEEKS`，不会因为这周过到星期几
/// 而多出一列来。
pub const DAYS: usize = WEEKS * ROWS;

/// 四档的分界，单位是分钟。
///
/// 用**绝对档**而不是分位数：分位数会让「每天都读半小时」那种极规律的习惯整年只亮一档，
/// 而那一整年明明读得很密；也会让今天与上个月不可比（同一格的分界随读者动）。
/// 绝对档说的是「这天读了半小时以上」，明年再看同一格还是那句话。
const LEVEL_MINUTES: [i64; 3] = [15, 30, 60];

/// 月标签之间至少隔几列，否则窄屏上两个标签会叠在一起。
const MONTH_LABEL_GAP: usize = 4;

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

/// 一格。
#[derive(Clone, Copy, Debug, Default)]
pub struct Cell {
    /// 落在窗口外的那几格（首列早于今天-370 的部分、末列晚于今天的部分）不画颜色。
    /// 它们存在的唯一理由是**列对齐**：没有它们，星期几那一行就不是行。
    pub filled: bool,
    /// 0..=4。0 = 那天没读，与「那天不在窗口里」是两句话（后者看 [`Cell::filled`]）。
    pub level: usize,
}

/// 顶部那个月标签。
#[derive(Clone, Copy, Debug)]
pub struct MonthLabel {
    /// 落在第几列（0 起）。
    pub column: usize,
    pub year: i32,
    /// 1..=12。
    pub month: i32,
}

/// 一屏热力图要画的全部数。取一次，画到下一次重取为止。
#[derive(Debug)]
pub struct Heatmap {
    /// 列优先：`columns[c][r]`，`r = 0` 是周一。长度恒为 [`WEEKS`]。
    pub columns: Vec<[Cell; ROWS]>,
    /// 从左到右排开，相邻两个至少隔 [`MONTH_LABEL_GAP`] 列。
    pub months: Vec<MonthLabel>,
    /// 窗口内有读的天数。
    pub active_days: usize,
    /// 窗口内的总时长（秒）。
    pub seconds: i64,
    /// 这一年里最长的一段连续。
    pub longest_streak: usize,
}

impl Heatmap {
    /// 取一次快照。`tz_offset_secs` 由界面侧给（core 不读系统时区，见 `folio_core::infra::time`）。
    ///
    /// 查库失败**不当成错误**：留一句日志，交出一张全空的图 —— 那一屏说的是「还没读」，
    /// 与「读不到书库」（视图那边给 `None` 的那一路）是两句话。
    pub fn load(store: &Arc<Store>, tz_offset_secs: i64) -> Heatmap {
        let summary = match store.reading_summary(DAYS, tz_offset_secs) {
            Ok(summary) => summary,
            Err(err) => {
                tracing::warn!("统计:读热力图窗口失败 {err:#}");
                return Self::empty();
            }
        };
        Self::from_summary(&summary, tz_offset_secs)
    }

    /// 窗口里有没有任何东西。全零不是错误，但它决定那一行结论说哪一句。
    pub fn is_empty(&self) -> bool {
        self.active_days == 0
    }

    fn empty() -> Heatmap {
        Heatmap {
            columns: (0..WEEKS)
                .map(|_| std::array::from_fn(|_| Cell::default()))
                .collect(),
            months: Vec::new(),
            active_days: 0,
            seconds: 0,
            longest_streak: 0,
        }
    }

    fn from_summary(summary: &ReadingSummary, tz_offset_secs: i64) -> Heatmap {
        // 那条约定在这里用得上两次：桶是连续的、最后一条是今天。于是日号 `day` 落在
        // 窗口里 ⇔ `first <= day <= today`，而它对应第 `day - first` 条。
        let today = local_day(time::now(), tz_offset_secs);
        let first = today - DAYS as i64 + 1;
        // 最后一列是**今天所在那一周**（周一起），所以它可能已经开始于最多 6 天前。
        let this_monday = today - weekday_of(today) as i64;
        // 窗口外（网格比窗口宽出去的那几天）读 0：`filled` 已经把它标成不画，
        // 这里再兜一层是为了不拿越界的下标去碰那条桶。
        let seconds_of = |day: i64| -> i64 {
            let offset = day - first;
            if (0..DAYS as i64).contains(&offset) {
                summary.days[offset as usize].seconds
            } else {
                0
            }
        };

        let mut columns: Vec<[Cell; ROWS]> = Vec::with_capacity(WEEKS);
        let mut months: Vec<MonthLabel> = Vec::new();
        // 月标签只在**月份换了**那一列摆，且相邻两个至少隔 MONTH_LABEL_GAP 列：
        // 一列一个月，隔得不够时后一个会压在前一个身上（窄屏一列只有十来像素）。
        let mut prev_month = 0;
        let mut last_label = None;
        for column in 0..WEEKS {
            let monday = this_monday - (WEEKS - 1 - column) as i64 * ROWS as i64;
            // 月标签锚在**列首那个周一**上：一个月从周中开始时，那一列仍然属于上个月
            // 的尾巴。差最多 6 天，而换来的是「标签永远落在同一列」这个稳定性。
            let (year, month) = civil_year_month(monday);
            if month != prev_month {
                let far_enough = last_label.is_none_or(|at| column - at >= MONTH_LABEL_GAP);
                if far_enough {
                    months.push(MonthLabel {
                        column,
                        year,
                        month,
                    });
                    last_label = Some(column);
                }
                // 没摆出去的标签也要推进「上个月是哪个月」，否则下一列会以为月还没换。
                prev_month = month;
            }
            columns.push(std::array::from_fn(|row| {
                let day = monday + row as i64;
                let filled = (first..=today).contains(&day);
                let spent = seconds_of(day);
                Cell {
                    filled,
                    level: level_for(spent),
                }
            }));
        }

        Heatmap {
            columns,
            months,
            active_days: summary.active_days,
            seconds: summary.seconds,
            longest_streak: summary.longest_streak,
        }
    }
}

/// 历元日号 → 星期几（0 = 周一）。
///
/// 1970-01-01（日号 0）是星期四，也就是周一往后第 3 天，所以偏移是 +3。
/// 与 chrono 的 `Weekday::num_days_from_monday` 同口径，`heatmap_rows_line_up_with_a_calendar`
/// 那条用 chrono 独立核一遍。
fn weekday_of(epoch_day: i64) -> usize {
    (epoch_day + 3).rem_euclid(7) as usize
}

/// 一秒的读数落在哪一档。0 秒是 0 档，其余按 [`LEVEL_MINUTES`] 切。
fn level_for(seconds: i64) -> usize {
    if seconds <= 0 {
        return 0;
    }
    1 + LEVEL_MINUTES
        .iter()
        .filter(|minutes| seconds >= **minutes * 60)
        .count()
}

/// 历元日号 → `(年, 月)`。
///
/// 不引新的日期算法：[`civil_date`] 只会产出 `YYYY-MM-DD`（全 ASCII、定宽），
/// 剪两段出来就是年月。
fn civil_year_month(epoch_day: i64) -> (i32, i32) {
    let text = civil_date(epoch_day);
    let year = text.get(..4).and_then(|s| s.parse().ok());
    let month = text.get(5..7).and_then(|s| s.parse().ok());
    match (year, month) {
        (Some(year), Some(month)) => (year, month),
        // 走到这里说明 `civil_date` 不再产出它承诺的那个形状：标签少一个比画错一个数好。
        _ => (0, 0),
    }
}

#[cfg(test)]
mod tests {
    //! 排格子那一步：日历对不对、档位切在哪、空库画不画得出来。
    //!
    //! 时区固定按东八区算而不是取设备的那个：断言里那些「今天 / 昨天」必须与查询用的
    //! 日界线是同一份，跟着测试机的时钟走就会在某些时刻红掉。日期全部相对今天算，
    //! 与 `store/reading.rs` 那批测试同一条路子。

    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use folio_core::infra::time;
    use folio_core::{Config, DocMeta, Format, Language, Locale, Store};
    use gpui_kit::App;

    use super::{
        Cell, DAYS, Heatmap, LEVEL_MINUTES, MONTH_LABEL_GAP, ROWS, WEEKS, civil_year_month,
        duration, level_for, weekday_of,
    };
    use crate::ui::services::config::ConfigHolder;
    use crate::ui::services::l10n::tf;

    const TZ: i64 = 8 * 3600;

    fn path(name: &str) -> PathBuf {
        Path::new("/books").join(name)
    }

    /// 某一天正午的 Unix 秒。
    fn noon(day_offset: i64) -> i64 {
        let today = time::local_day(time::now(), TZ);
        time::day_start(today + day_offset, TZ) + 12 * 3600
    }

    fn store_with(sessions: &[(i64, i64)]) -> Arc<Store> {
        let store = Arc::new(Store::in_memory().unwrap());
        let book = store
            .add_book(
                &path("a.epub"),
                &DocMeta {
                    title: "A".into(),
                    authors: vec![],
                    format: Format::Epub,
                    size: 1,
                    ..DocMeta::default()
                },
            )
            .unwrap();
        for (start, seconds) in sessions {
            store
                .record_session(book, *start, *start + seconds, 0)
                .unwrap();
        }
        store
    }

    /// 某个历元日号是周几，Monday = 0 … Sunday = 6 —— **由 chrono 的日历算**，
    /// 与被测的那段 `weekday_of` 完全无关。取正午那一刻是为了让日界线落在测试用的
    /// 那个固定时区里，不与 UTC 那天混掉。
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

    /// 那一列里第 `row` 行的格子（`row = 0` 是周一）。
    fn cell(map: &Heatmap, column: usize, row: usize) -> &Cell {
        &map.columns[column][row]
    }

    /// 把界面语言装进这个上下文（`duration` 要说哪门语言的句子由配置实体决定，
    /// 不是进程全局，见 `ui/services/l10n` 那份说明）。
    fn set_locale(cx: &mut gpui_kit::TestAppContext, locale: Locale) {
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

    /// 行是星期几不是排出来的：那一段 `+3` 的偏移量与 chrono 的日历在整整一年上对齐。
    ///
    /// 这条钉的是最容易静默错的一处 —— 偏移写错，格子仍然排得整整齐齐，只是每一格都
    /// 站在错的星期几上，而屏幕上没有任何一句话会指出这件事。
    #[test]
    fn rows_line_up_with_a_calendar() {
        let today = time::local_day(time::now(), TZ);
        for offset in 0..DAYS as i64 {
            let day = today - offset;
            assert_eq!(
                weekday_of(day),
                calendar_weekday(day),
                "历元日号 {day} 的星期几算错了"
            );
        }
    }

    /// 网格的最后一格有读的天就是今天，而它后面（本周剩下的那几天）是不画的补位。
    ///
    /// 「今天」这一格是全图唯一的焦点：它排错列，读者看整张图的形状都会跟着错一年。
    /// 钉法是按**哪一格亮了**而不是按日期字符串 —— 视图只读档位，格子不背日期。
    #[test]
    fn the_grid_ends_at_today() {
        let store = store_with(&[(noon(0), 3_600), (noon(-7), 3_600)]);
        let map = Heatmap::load(&store, TZ);
        let today = time::local_day(time::now(), TZ);
        let row = weekday_of(today);

        assert_eq!(
            cell(&map, WEEKS - 1, row).level,
            4,
            "今天那一格要落在最后一列的星期几那一行上"
        );
        assert_eq!(
            cell(&map, WEEKS - 2, row).level,
            4,
            "上周同一天落在左边那一列的同一行"
        );
        // 本周还没到的那几天：不填，也不该有任何读数。
        for after in (row + 1)..ROWS {
            assert!(!cell(&map, WEEKS - 1, after).filled, "第 {after} 行还没到");
            assert_eq!(cell(&map, WEEKS - 1, after).level, 0);
        }
        // 有格子的天数 = 一整年减掉本周还没到的那几天。列是按周一对齐排的，所以今天
        // 过到星期几，末列就缺几格 —— 首列因此往回少看同样几天（窗口那 371 天里最早的
        // 那几格落在网格外，`seconds_of` 与 `filled` 两处都是这么判的）。
        assert_eq!(
            map.columns
                .iter()
                .flat_map(|column| column.iter())
                .filter(|cell| cell.filled)
                .count(),
            DAYS - (ROWS - 1 - row)
        );
    }

    /// 档位切在固定的分钟上，而不是按这一年的分布重新算分位。
    ///
    /// 分位数会让「每天都读半小时」那种极规律的习惯整年只亮一档，也会让今天与上个月
    /// 不可比 —— 同一格的分界随读者动。绝对档说的永远是「这天读了半小时以上」。
    #[test]
    fn levels_cut_at_fixed_minutes() {
        assert_eq!(level_for(0), 0, "没读是 0 档，与 1 档差一个颜色");
        assert_eq!(level_for(LEVEL_MINUTES[0] * 60 - 1), 1);
        for (at, want) in LEVEL_MINUTES.iter().zip([2, 3, 4]) {
            let secs = *at * 60;
            assert_eq!(level_for(secs), want, "{at} 分钟整应当进 {want} 档");
            assert_eq!(level_for(secs - 1), want - 1, "差一秒就还在上一档");
            assert_eq!(level_for(secs + 59), want, "刚过界那一分钟不该跳两档");
        }
        // 半小时这个数在结论与图例之外不再出现，但它是那一格的颜色唯一依据。
        assert_eq!(level_for(30 * 60), 3);
        assert_eq!(level_for(59 * 60), 3);
        assert_eq!(level_for(60 * 60), 4);
    }

    /// 空库仍然交出完整的一年：全 0 档，而不是塌成「这里什么也没有」。
    ///
    /// 那一屏说的是「还没读」，得让读者看见一年那么多格子都空着 —— 一张缩成一条线的
    /// 空图读起来像坏了。
    #[test]
    fn an_empty_library_still_hands_the_grid_a_full_year() {
        let store = store_with(&[]);
        let map = Heatmap::load(&store, TZ);
        assert!(map.is_empty());
        assert_eq!(map.columns.len(), WEEKS);
        assert!(
            map.columns
                .iter()
                .flat_map(|column| column.iter())
                .filter(|cell| cell.filled)
                .all(|cell| cell.level == 0)
        );
        assert!(!map.months.is_empty(), "空库也得有月标签，否则轴就塌了");
    }

    /// 月标签不叠字：相邻两个至少隔 [`MONTH_LABEL_GAP`] 列，且月份单调。
    #[test]
    fn month_labels_never_crowd_each_other() {
        let store = store_with(&[(noon(-300), 600)]);
        let map = Heatmap::load(&store, TZ);
        assert!(
            map.months.len() >= 10,
            "一年该有十来个标签，实际 {}",
            map.months.len()
        );
        for pair in map.months.windows(2) {
            assert!(
                pair[1].column - pair[0].column >= MONTH_LABEL_GAP,
                "两个标签隔得太近：{} 与 {}",
                pair[0].column,
                pair[1].column
            );
        }
        // 每个标签说的确实是它那一列开头那个周一的年月：标签与格子是两处算的，
        // 撞在一起才对得上（视图按 `column × pitch` 摆，算错列就是把「7 月」写在 9 月头上）。
        let today = time::local_day(time::now(), TZ);
        let this_monday = today - weekday_of(today) as i64;
        for label in &map.months {
            let monday = this_monday + (label.column as i64 - WEEKS as i64 + 1) * ROWS as i64;
            assert_eq!(
                civil_year_month(monday),
                (label.year, label.month),
                "第 {} 列的标签与那一列的周一不符",
                label.column
            );
        }
    }

    /// 时长那句按语言换形状（函数从旧 `report.rs` 搬了过来，规矩没变）。
    #[gpui_kit::test]
    fn the_duration_sentence_changes_shape_with_the_language(cx: &mut gpui_kit::TestAppContext) {
        set_locale(cx, Locale::ZhHans);
        assert_eq!(cx.read(|cx| duration(cx, 5_040)).as_str(), "1 小时 24 分钟");
        assert_eq!(cx.read(|cx| duration(cx, 45)).as_str(), "45 秒");
        assert_eq!(cx.read(|cx| duration(cx, 7_200)).as_str(), "2 小时");

        set_locale(cx, Locale::En);
        assert_eq!(cx.read(|cx| duration(cx, 5_040)).as_str(), "1h 24m");
        // 月标签那一行：中文带「月」，英文就是那个数字（一年十二个数排成一排，
        // 顺序已经把「这是月份」说清了）。
        assert_eq!(cx.read(|cx| tf(cx, "stats.month", &[("month", &3)])), "3");
    }
}
