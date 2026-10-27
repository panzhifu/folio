//! 一屏统计的背后那一份数。
//!
//! 输入是库里的全部会话行，输出是**当前区间**已经切好、补过零、排好序的形状。
//! 界面只负责把这里的字段摆到屏上，不再算任何除法或分组。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use folio_core::infra::time::{
    HOURS_PER_DAY, civil_from_days, days_from_civil, days_in_month, local_day, local_hour,
    weekday_monday0,
};
use folio_core::store::{ReadingRecord, Store};

/// 里的一档。「总」看全部历史，「日 / 周 / 月 / 年」看一个可翻页的窗口。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Range {
    Total,
    Day,
    Week,
    #[default]
    Month,
    Year,
}

impl Range {
    pub const ALL: [Range; 5] = [
        Range::Total,
        Range::Day,
        Range::Week,
        Range::Month,
        Range::Year,
    ];

    pub fn index(self) -> usize {
        match self {
            Range::Total => 0,
            Range::Day => 1,
            Range::Week => 2,
            Range::Month => 3,
            Range::Year => 4,
        }
    }

    pub fn from_index(index: usize) -> Range {
        Self::ALL[index.min(Self::ALL.len() - 1)]
    }

    pub fn label_key(self) -> &'static str {
        match self {
            Range::Total => "stats.range_total",
            Range::Day => "stats.range_day",
            Range::Week => "stats.range_week",
            Range::Month => "stats.range_month",
            Range::Year => "stats.range_year",
        }
    }

    /// 这一段是不是「总」—— 它没有上一页 / 下一页。
    pub fn is_total(self) -> bool {
        self == Range::Total
    }

    /// `anchor` 所在的窗口，两端都是**本地日序号且含端点**。
    ///
    /// 「总」的窗口由 [`Snapshot::build`] 按第一条记录改写成「第一天 .. 今天」，
    /// 这里先给一个退化的答案，让调用方不必为它单开一条分支。
    pub fn window(self, anchor: i64) -> (i64, i64) {
        match self {
            Range::Total | Range::Day => (anchor, anchor),
            Range::Week => {
                let monday = anchor - weekday_monday0(anchor) as i64;
                (monday, monday + 6)
            }
            Range::Month => {
                let date = civil_from_days(anchor);
                let first = days_from_civil(date.year, date.month, 1);
                let last =
                    days_from_civil(date.year, date.month, days_in_month(date.year, date.month));
                (first, last)
            }
            Range::Year => {
                let date = civil_from_days(anchor);
                (
                    days_from_civil(date.year, 1, 1),
                    days_from_civil(date.year, 12, 31),
                )
            }
        }
    }

    /// 往前 / 往后翻一页。月份与年份要按日历进位，不能按 30 / 365 天加。
    pub fn shift(self, anchor: i64, step: i64) -> i64 {
        match self {
            Range::Total => anchor,
            Range::Day => anchor + step,
            Range::Week => anchor + step * 7,
            Range::Month => {
                let date = civil_from_days(anchor);
                let months = i64::from(date.year) * 12 + i64::from(date.month) - 1 + step;
                let year = months.div_euclid(12) as i32;
                let month = months.rem_euclid(12) as u32 + 1;
                days_from_civil(year, month, date.day.min(days_in_month(year, month)))
            }
            Range::Year => {
                let date = civil_from_days(anchor);
                let year = date.year + step as i32;
                days_from_civil(
                    year,
                    date.month,
                    date.day.min(days_in_month(year, date.month)),
                )
            }
        }
    }
}

/// 一天（本地日）的合计。`books` 是**这一天的不同书数**，不是会话数。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DayStat {
    pub day: i64,
    pub seconds: i64,
    pub sessions: usize,
    pub books: usize,
}

impl DayStat {
    pub fn is_active(&self) -> bool {
        self.seconds > 0
    }
}

/// 一本书在窗口内的合计。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookStat {
    pub book_id: i64,
    pub title: String,
    pub seconds: i64,
    pub sessions: usize,
    /// 在这段时间里读过几天。
    pub days: usize,
    pub first_at: i64,
    pub last_at: i64,
}

/// 一条会话的读数，带上书名。最值那几格用它。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Moment {
    pub book_id: i64,
    pub title: String,
    pub at: i64,
    pub seconds: i64,
}

/// 时段桶里「读得最久的那本」。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookRef {
    pub book_id: i64,
    pub title: String,
    pub seconds: i64,
}

/// 一个时段（凌晨 / 上午 / 下午 / 晚上）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bucket {
    pub seconds: i64,
    /// 这个时段读过的不同书数。
    pub books: usize,
    pub top: Option<BookRef>,
}

/// 时段数：凌晨、上午、下午、晚上。
pub const BUCKETS: usize = 4;
/// 单次阅读时长分布的档数：<5m、5–15m、15–30m、30–60m、>60m。
pub const SESSION_BUCKETS: usize = 5;
/// 「总」页最近 30 天的那条尾巴。
pub const RECENT_DAYS: usize = 30;

/// 一屏要画的全部数。
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub range: Range,
    /// 当前窗口里的锚点（日 / 周 / 月 / 年翻页时它动，「总」不动）。
    pub anchor: i64,
    pub start: i64,
    pub end: i64,
    pub today: i64,
    /// 第一次阅读的那一天。从没读过是 `None`。
    pub first_day: Option<i64>,

    // —— 阅读全时段（「总」页的大数） ——
    pub lifetime_seconds: i64,
    pub lifetime_sessions: usize,
    pub lifetime_books: usize,
    pub lifetime_days: usize,

    // —— 当前窗口 ——
    pub seconds: i64,
    pub sessions: usize,
    pub books: usize,
    pub active_days: usize,
    pub longest_streak: usize,
    pub current_streak: usize,
    /// 最长那一段的两端（含端点）。没有读时是 `None`。
    pub longest_run: Option<(i64, i64)>,
    /// 窗口内每天一格，含补零，`start..=end`。
    pub days: Vec<DayStat>,
    /// 窗口内读过的书，按时长降序。
    pub book_stats: Vec<BookStat>,
    pub hours: [i64; HOURS_PER_DAY],
    pub buckets: [Bucket; BUCKETS],
    pub best_day: Option<DayStat>,
    /// 一天里同时读过的书最多的一天。
    pub busiest_day: Option<DayStat>,
    pub longest_session: Option<Moment>,
    pub earliest: Option<Moment>,
    pub latest: Option<Moment>,
    pub session_hist: [usize; SESSION_BUCKETS],
    /// 最近 [`RECENT_DAYS`] 天（到 `today` 为止），含补零。只给「总」页用。
    pub recent: Vec<DayStat>,
}

impl Snapshot {
    /// 取一次快照。查库失败**不当成错误**：留一句日志，交出一份空快照 ——
    /// 那一屏说的是「还没读」，与「读不到书库」（视图给 `None` 的那一路）是两句话。
    pub fn load(store: &Arc<Store>, range: Range, anchor: i64, tz_offset_secs: i64) -> Snapshot {
        let records = match store.reading_history() {
            Ok(records) => records,
            Err(error) => {
                tracing::warn!(error = %error, "reading_history: 统计取数失败");
                Vec::new()
            }
        };
        let today = local_day(folio_core::infra::time::now(), tz_offset_secs);
        Self::build(records, range, anchor, today, tz_offset_secs)
    }

    /// 纯计算那一半。测试直接喂一批假记录，不装库。
    pub fn build(
        mut records: Vec<ReadingRecord>,
        range: Range,
        anchor: i64,
        today: i64,
        tz_offset_secs: i64,
    ) -> Snapshot {
        records.sort_by_key(|record| record.started_at);

        let mut lifetime_seconds = 0;
        let mut lifetime_days = HashSet::new();
        let mut lifetime_books = HashSet::new();
        let mut first_day: Option<i64> = None;
        for record in &records {
            lifetime_seconds += record.seconds;
            let day = local_day(record.started_at, tz_offset_secs);
            lifetime_days.insert(day);
            lifetime_books.insert(record.book_id);
            first_day = Some(first_day.map_or(day, |first| first.min(day)));
        }

        let (mut start, end) = match range {
            Range::Total => (first_day.unwrap_or(today), today),
            _ => range.window(anchor),
        };
        if start > end {
            start = end;
        }

        let mut day_acc: BTreeMap<i64, DayAcc> = BTreeMap::new();
        let mut book_acc: HashMap<i64, BookAcc> = HashMap::new();
        let mut hours = [0i64; HOURS_PER_DAY];
        let mut buckets: [BucketAcc; BUCKETS] = std::array::from_fn(|_| BucketAcc::default());
        let mut session_hist = [0usize; SESSION_BUCKETS];
        let mut longest_session: Option<Moment> = None;
        let mut earliest: Option<Moment> = None;
        let mut latest: Option<Moment> = None;

        for record in &records {
            let day = local_day(record.started_at, tz_offset_secs);
            if day < start || day > end {
                continue;
            }
            let hour = local_hour(record.started_at, tz_offset_secs);
            let moment = Moment {
                book_id: record.book_id,
                title: record.title.clone(),
                at: record.started_at,
                seconds: record.seconds,
            };

            let day_slot = day_acc.entry(day).or_default();
            day_slot.seconds += record.seconds;
            day_slot.sessions += 1;
            day_slot.books.insert(record.book_id);

            let book_slot = book_acc.entry(record.book_id).or_insert_with(|| BookAcc {
                title: record.title.clone(),
                seconds: 0,
                sessions: 0,
                marks: HashSet::new(),
                first_at: record.started_at,
                last_at: record.started_at,
            });
            book_slot.seconds += record.seconds;
            book_slot.sessions += 1;
            book_slot.marks.insert(day);
            book_slot.first_at = book_slot.first_at.min(record.started_at);
            book_slot.last_at = book_slot.last_at.max(record.started_at);

            hours[hour] += record.seconds;
            let bucket = &mut buckets[hour / (HOURS_PER_DAY / BUCKETS)];
            bucket.seconds += record.seconds;
            bucket.books.insert(record.book_id);
            bucket
                .by_book
                .entry(record.book_id)
                .or_insert_with(|| (record.title.clone(), 0))
                .1 += record.seconds;

            session_hist[session_bucket(record.seconds)] += 1;
            if longest_session
                .as_ref()
                .is_none_or(|current| record.seconds > current.seconds)
            {
                longest_session = Some(moment.clone());
            }
            if earliest
                .as_ref()
                .is_none_or(|current| record.started_at < current.at)
            {
                earliest = Some(moment.clone());
            }
            if latest
                .as_ref()
                .is_none_or(|current| record.started_at > current.at)
            {
                latest = Some(moment);
            }
        }

        let days: Vec<DayStat> = (start..=end)
            .map(|day| {
                day_acc
                    .remove(&day)
                    .map(|acc| DayStat {
                        day,
                        seconds: acc.seconds,
                        sessions: acc.sessions,
                        books: acc.books.len(),
                    })
                    .unwrap_or(DayStat {
                        day,
                        ..DayStat::default()
                    })
            })
            .collect();

        let seconds = days.iter().map(|day| day.seconds).sum();
        let sessions = days.iter().map(|day| day.sessions).sum();
        let active_days = days.iter().filter(|day| day.is_active()).count();
        let (current_streak, longest_streak, longest_run) = streaks(&days, today);

        let mut book_stats: Vec<BookStat> = book_acc
            .into_iter()
            .map(|(book_id, acc)| BookStat {
                book_id,
                title: acc.title,
                seconds: acc.seconds,
                sessions: acc.sessions,
                days: acc.marks.len(),
                first_at: acc.first_at,
                last_at: acc.last_at,
            })
            .collect();
        book_stats.sort_by(|a, b| {
            b.seconds
                .cmp(&a.seconds)
                .then(b.sessions.cmp(&a.sessions))
                .then_with(|| a.title.cmp(&b.title))
        });

        let best_day = days
            .iter()
            .filter(|day| day.is_active())
            .max_by(|a, b| a.seconds.cmp(&b.seconds))
            .cloned();
        let busiest_day = days
            .iter()
            .filter(|day| day.books > 0)
            .max_by(|a, b| a.books.cmp(&b.books).then(a.seconds.cmp(&b.seconds)))
            .cloned();

        let buckets = buckets.map(BucketAcc::finish);
        let recent = recent_days(&records, today, tz_offset_secs);

        Snapshot {
            range,
            anchor,
            start,
            end,
            today,
            first_day,
            lifetime_seconds,
            lifetime_sessions: records.len(),
            lifetime_books: lifetime_books.len(),
            lifetime_days: lifetime_days.len(),
            seconds,
            sessions,
            books: book_stats.len(),
            active_days,
            longest_streak,
            current_streak,
            longest_run,
            days,
            book_stats,
            hours,
            buckets,
            best_day,
            busiest_day,
            longest_session,
            earliest,
            latest,
            session_hist,
            recent,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.seconds == 0
    }

    /// 「我们相识的第 N 天」。没读过时没有这个数。
    pub fn days_since_first(&self) -> Option<i64> {
        self.first_day.map(|first| (self.today - first + 1).max(1))
    }

    /// 只对有读的天取平均：把没读的那几天摊进去会把「一次读多久」压低到没有意义。
    pub fn average_active_day(&self) -> i64 {
        if self.active_days == 0 {
            0
        } else {
            self.seconds / self.active_days as i64
        }
    }

    pub fn average_session(&self) -> i64 {
        if self.sessions == 0 {
            0
        } else {
            self.seconds / self.sessions as i64
        }
    }

    /// 每个活跃日平均翻开几本书。用逐日的书数求和，而不是整个窗口的书数除以天数 ——
    /// 后者会把「同一本书读了十天」算成 0.1 本/天。
    pub fn average_books_per_active_day(&self) -> f64 {
        if self.active_days == 0 {
            0.0
        } else {
            self.days.iter().map(|day| day.books).sum::<usize>() as f64 / self.active_days as f64
        }
    }

    /// 「总」页最近 `n` 天；`n` 大过 [`RECENT_DAYS`] 时给满那 30 天。
    pub fn recent_slice(&self, n: usize) -> &[DayStat] {
        let from = self.recent.len().saturating_sub(n);
        &self.recent[from..]
    }

    /// 窗口里读得最久的那本。
    pub fn top_book(&self) -> Option<&BookStat> {
        self.book_stats.first()
    }

    /// 翻开次数最多的那本（「当日之最」的第二格用）。
    pub fn most_sessions_book(&self) -> Option<&BookStat> {
        self.book_stats
            .iter()
            .max_by(|a, b| a.sessions.cmp(&b.sessions).then(a.seconds.cmp(&b.seconds)))
    }

    /// 陪伴天数最多的那本。
    pub fn most_days_book(&self) -> Option<&BookStat> {
        self.book_stats
            .iter()
            .max_by(|a, b| a.days.cmp(&b.days).then(a.seconds.cmp(&b.seconds)))
    }
}

#[derive(Default)]
struct DayAcc {
    seconds: i64,
    sessions: usize,
    books: HashSet<i64>,
}

struct BookAcc {
    title: String,
    seconds: i64,
    sessions: usize,
    marks: HashSet<i64>,
    first_at: i64,
    last_at: i64,
}

#[derive(Default)]
struct BucketAcc {
    seconds: i64,
    books: HashSet<i64>,
    by_book: HashMap<i64, (String, i64)>,
}

impl BucketAcc {
    fn finish(self) -> Bucket {
        let top = self
            .by_book
            .into_iter()
            .max_by(|a, b| a.1.1.cmp(&b.1.1))
            .map(|(book_id, (title, seconds))| BookRef {
                book_id,
                title,
                seconds,
            });
        Bucket {
            seconds: self.seconds,
            books: self.books.len(),
            top,
        }
    }
}

fn session_bucket(seconds: i64) -> usize {
    match seconds {
        0..=299 => 0,
        300..=899 => 1,
        900..=1_799 => 2,
        1_800..=3_600 => 3,
        _ => 4,
    }
}

/// 最近 [`RECENT_DAYS`] 天（到 `today` 为止）的逐日合计，补零。
fn recent_days(records: &[ReadingRecord], today: i64, tz_offset_secs: i64) -> Vec<DayStat> {
    let first = today - RECENT_DAYS as i64 + 1;
    let mut acc: BTreeMap<i64, DayAcc> = BTreeMap::new();
    for record in records {
        let day = local_day(record.started_at, tz_offset_secs);
        if day < first || day > today {
            continue;
        }
        let slot = acc.entry(day).or_default();
        slot.seconds += record.seconds;
        slot.sessions += 1;
        slot.books.insert(record.book_id);
    }
    (first..=today)
        .map(|day| {
            acc.remove(&day)
                .map(|slot| DayStat {
                    day,
                    seconds: slot.seconds,
                    sessions: slot.sessions,
                    books: slot.books.len(),
                })
                .unwrap_or(DayStat {
                    day,
                    ..DayStat::default()
                })
        })
        .collect()
}

fn streaks(days: &[DayStat], today: i64) -> (usize, usize, Option<(i64, i64)>) {
    // 月 / 年窗口会带着「今天之后」那几格，它们不是「断签」，从今天倒着数才对得上
    // 「我现在连了几天」。今天不在窗口里（回看旧月份）时，当前连续没有意义。
    let mut current = 0;
    if let Some(position) = days.iter().position(|day| day.day == today) {
        for (step_back, day) in days[..=position].iter().rev().enumerate() {
            if day.is_active() {
                current += 1;
            } else if step_back == 0 {
                continue;
            } else {
                break;
            }
        }
    }
    let mut longest = 0;
    let mut run = 0;
    let mut run_start: Option<i64> = None;
    let mut longest_run = None;
    for day in days {
        if day.is_active() {
            run += 1;
            run_start.get_or_insert(day.day);
            if run > longest {
                longest = run;
                longest_run = Some((run_start.unwrap_or(day.day), day.day));
            }
        } else {
            run = 0;
            run_start = None;
        }
    }
    (current, longest, longest_run)
}

#[cfg(test)]
mod tests {
    use super::{Range, SESSION_BUCKETS, Snapshot, days_from_civil, weekday_monday0};
    use folio_core::infra::time::day_start;
    use folio_core::store::ReadingRecord;

    fn record(book_id: i64, title: &str, day: i64, hour: i64, seconds: i64) -> ReadingRecord {
        ReadingRecord {
            book_id,
            title: title.into(),
            started_at: day_start(day, 0) + hour * 3_600,
            seconds,
        }
    }

    fn day(year: i32, month: u32, day: u32) -> i64 {
        days_from_civil(year, month, day)
    }

    /// 「日」只收锚点那一天，窗口外的会话连书都不该出现在排行里。
    #[test]
    fn a_day_window_only_counts_its_own_day() {
        let today = day(2026, 10, 27);
        let records = vec![
            record(1, "A", today, 9, 600),
            record(1, "A", today, 10, 300),
            record(2, "B", today - 1, 9, 900),
        ];
        let snapshot = Snapshot::build(records, Range::Day, today, today, 0);
        assert_eq!(snapshot.days.len(), 1);
        assert_eq!(snapshot.seconds, 900);
        assert_eq!(snapshot.sessions, 2);
        assert_eq!(snapshot.books, 1);
        assert_eq!(snapshot.lifetime_seconds, 1_800, "总量仍看全历史");
        assert_eq!(snapshot.lifetime_books, 2);
    }

    /// 月窗口首尾都在、按书聚合的天数是「读过几天」而不是会话数。
    #[test]
    fn a_month_window_fills_every_day_and_aggregates_books() {
        let today = day(2026, 10, 27);
        let records = vec![
            record(1, "A", day(2026, 10, 1), 9, 600),
            record(1, "A", day(2026, 10, 3), 9, 600),
            record(2, "B", day(2026, 10, 3), 20, 1_500),
            record(2, "B", day(2026, 9, 30), 20, 9_999),
        ];
        let snapshot = Snapshot::build(records, Range::Month, today, today, 0);
        assert_eq!(snapshot.days.len(), 31, "十月有 31 格");
        assert_eq!(snapshot.seconds, 2_700);
        assert_eq!(snapshot.active_days, 2);
        assert_eq!(snapshot.books, 2);
        assert_eq!(snapshot.book_stats[0].title, "B", "时长降序");
        assert_eq!(snapshot.book_stats[1].days, 2);
        assert_eq!(snapshot.best_day.as_ref().unwrap().day, day(2026, 10, 3));
        assert_eq!(snapshot.busiest_day.as_ref().unwrap().books, 2);
    }

    /// 连续天数：今天没读时从昨天起算，最长的一段与文件头那条口径一致。
    #[test]
    fn streaks_ignore_an_empty_today_but_count_the_run_before_it() {
        let today = day(2026, 10, 27);
        let records = vec![
            record(1, "A", today - 1, 9, 600),
            record(1, "A", today - 2, 9, 600),
            record(1, "A", today - 4, 9, 600),
        ];
        let snapshot = Snapshot::build(records, Range::Month, today, today, 0);
        assert_eq!(snapshot.current_streak, 2);
        assert_eq!(snapshot.longest_streak, 2);
        assert_eq!(
            snapshot.longest_run,
            Some((today - 2, today - 1)),
            "最长那一段的两端要能直接写出来"
        );
    }

    /// 「总」的窗口从第一条记录那天起；相识天数按历史上的第一天算。
    #[test]
    fn total_starts_at_the_first_record_and_reports_how_long_we_have_known_each_other() {
        let today = day(2026, 10, 27);
        let first = day(2026, 5, 20);
        let records = vec![record(1, "A", first, 9, 600)];
        let snapshot = Snapshot::build(records, Range::Total, today, today, 0);
        assert_eq!(snapshot.start, first);
        assert_eq!(snapshot.days.len(), (today - first + 1) as usize);
        assert_eq!(snapshot.days_since_first(), Some(today - first + 1));
    }

    /// 翻月跨年、翻年遇闰日都要落在合法日期上。
    #[test]
    fn shifting_a_month_or_a_year_respects_the_calendar() {
        let jan = day(2027, 1, 31);
        assert_eq!(
            Range::Month.shift(jan, -1),
            day(2026, 12, 31),
            "一月往前是上一年十二月"
        );
        let leap = day(2024, 2, 29);
        assert_eq!(
            Range::Year.shift(leap, 1),
            day(2025, 2, 28),
            "闰日翻到平年要夹到月末"
        );
        assert_eq!(Range::Week.shift(day(2026, 10, 27), -1), day(2026, 10, 20));
    }

    /// 周的左端是周一；无论锚点是周几都落在同一个周一。
    #[test]
    fn a_week_window_starts_on_monday() {
        for offset in 0..7 {
            let anchor = day(2026, 10, 26) + offset;
            let (start, end) = Range::Week.window(anchor);
            assert_eq!(weekday_monday0(start), 0);
            assert_eq!(end - start, 6);
            assert!(start <= anchor && anchor <= end);
        }
    }

    /// 时段分桶按会话**开始**那一刻，单次时长分布落在正确的档里。
    #[test]
    fn hours_buckets_and_session_length_histogram_follow_the_session_start() {
        let today = day(2026, 10, 27);
        let records = vec![
            record(1, "A", today, 1, 120),
            record(2, "B", today, 8, 600),
            record(1, "A", today, 14, 1_200),
            record(1, "A", today, 19, 3_600),
        ];
        let snapshot = Snapshot::build(records, Range::Day, today, today, 0);
        assert_eq!(snapshot.hours[1], 120);
        assert_eq!(snapshot.buckets[0].seconds, 120, "凌晨");
        assert_eq!(snapshot.buckets[1].seconds, 600, "上午");
        assert_eq!(snapshot.buckets[2].seconds, 1_200, "下午");
        assert_eq!(snapshot.buckets[3].seconds, 3_600, "晚上");
        assert_eq!(snapshot.buckets[0].top.as_ref().unwrap().title, "A");
        assert_eq!(snapshot.session_hist.len(), SESSION_BUCKETS);
        assert_eq!(snapshot.session_hist[0], 1, "<5 分钟");
        assert_eq!(snapshot.session_hist[1], 1, "5–15 分钟");
        assert_eq!(snapshot.session_hist[2], 1, "15–30 分钟");
        assert_eq!(snapshot.session_hist[3], 1, "30–60 分钟");
    }
}
