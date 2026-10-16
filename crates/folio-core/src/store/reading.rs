//! 阅读统计：会话往返、时长派生、按本地日的汇总与补零、窗口内的派生读数。
//!
//! 存的是**原始会话**而不是按天汇总 —— 「今天读了多久」依赖读者所在时区，而时区只有
//! 界面侧知道。core 只留事实（何时开始、多久、前进多少字），汇总时把偏移当参数收进来
//! （见 `crate::infra::time`），不把某个时区的日界线写死进表。
//!
//! 窗口内的每一个派生数（连续天数、活跃天数、时段分布、书目排行）都是从**同一批**
//! `reading_sessions` 行上算出来的，所以它们与图表之间不存在「两条查询之间被新写入
//! 打断」那种对不上的可能。

use std::collections::{BTreeMap, HashSet};

use rusqlite::params;

use super::types::{
    BookReading, DailyReading, ReadingRank, ReadingSession, ReadingSummary, ReadingTotals,
};
use super::{Store, now};
use crate::Result;
use crate::infra::time::{HOURS_PER_DAY, civil_date, day_start, local_day, local_hour};

impl Store {
    /// 记一次阅读会话。`seconds` 由起止时间算，负数夹到 0 —— 调用方给起止就够了，
    /// 两处各报一个「时长」迟早会对不上。
    pub fn record_session(
        &self,
        book_id: i64,
        started_at: i64,
        ended_at: i64,
        chars: i64,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let seconds = (ended_at - started_at).max(0);
        conn.execute(
            "INSERT INTO reading_sessions (book_id, started_at, ended_at, seconds, chars)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![book_id, started_at, ended_at, seconds, chars],
        )?;
        tracing::debug!("store: 第 {book_id} 本记了 {seconds} 秒阅读（{chars:+} 字）");
        Ok(conn.last_insert_rowid())
    }

    /// 一本书的会话，最近的在先。
    pub fn sessions(&self, book_id: i64, limit: usize) -> Result<Vec<ReadingSession>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, book_id, started_at, ended_at, seconds, chars
             FROM reading_sessions WHERE book_id = ?1
             ORDER BY started_at DESC, id DESC LIMIT ?2",
        )?;
        Ok(stmt
            .query_map(params![book_id, limit as i64], session_from_row)?
            .collect::<std::result::Result<_, _>>()?)
    }

    /// 全部读过的书的合计。跟时区挂钩的「读了多少天」不在这里 —— 它属于
    /// [`Store::daily_reading`]，调用时要带偏移。
    pub fn reading_totals(&self) -> Result<ReadingTotals> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let (seconds, sessions, books, chars) = conn.query_row(
            "SELECT coalesce(sum(seconds), 0), count(*), count(DISTINCT book_id),
                    coalesce(sum(chars), 0)
             FROM reading_sessions",
            [],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            },
        )?;
        Ok(ReadingTotals {
            seconds,
            sessions,
            books,
            chars,
        })
    }

    /// 读过的天数（跨全时段，按读者本地日，只算有读数的那些天）。
    ///
    /// 这条不在 SQL 里 `count(DISTINCT ...)`：日界线要把时区偏移算进去，而那是界面侧
    /// 才知道的事（文件头的理由）。行数就是会话数，全库读一遍也就是几万行定点算术。
    pub fn reading_days(&self, tz_offset_secs: i64) -> Result<usize> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare("SELECT started_at FROM reading_sessions WHERE seconds > 0")?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        let mut days: HashSet<i64> = HashSet::new();
        for row in rows {
            days.insert(local_day(row?, tz_offset_secs));
        }
        Ok(days.len())
    }

    /// 一本书的累计。没读过时各项为零、`last_read_at` 为 `None`，不是错误 ——
    /// 「这本书还没读」是常见状态，不是异常。
    pub fn book_reading(&self, book_id: i64) -> Result<BookReading> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let (seconds, sessions, chars, last_read_at) = conn.query_row(
            "SELECT coalesce(sum(seconds), 0), count(*), coalesce(sum(chars), 0), max(ended_at)
             FROM reading_sessions WHERE book_id = ?1",
            params![book_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                ))
            },
        )?;
        Ok(BookReading {
            book_id,
            seconds,
            sessions,
            chars,
            last_read_at,
        })
    }

    /// 最近 `days` 天（读者本地日）每天的合计，**含没有记录的天（补零）**，
    /// 最后一条是今天。图表的 x 轴要连续，空日不能让两边的柱子贴在一起。
    ///
    /// 一次会话整个记在**开始那天**（跨午夜的那段不摊开）：把一次会话按秒拆到两天
    /// 需要存更细的数据，而阅读界面根本不产生那种粒度 —— 凌晨读着读着跨过零点，
    /// 归到开始那天完全说得通。
    pub fn daily_reading(&self, days: usize, tz_offset_secs: i64) -> Result<Vec<DailyReading>> {
        Ok(self.reading_summary(days, tz_offset_secs)?.days)
    }

    /// 窗口内的全部读数：**一条 SQL 取回原始会话行，其余的数全在 Rust 里派生**。
    ///
    /// 不派生第二条查询的理由写在 [`ReadingSummary`] 上 —— 这一屏的每个数必须来自同一批
    /// 行，否则「今天 + 前两天 = 窗口合计」这种关系会在两次查询之间被新写入的会话打断。
    /// `chars` 这一列在这里**故意不参与**：它的口径还没定（见 `plans/Locator.md` 的开放
    /// 问题），所以汇总不出来「字数」这个数，等口径定了再补。
    pub fn reading_summary(&self, days: usize, tz_offset_secs: i64) -> Result<ReadingSummary> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let today = local_day(now(), tz_offset_secs);
        let first = today - days as i64 + 1;
        let mut stmt = conn.prepare(
            "SELECT started_at, seconds, chars, book_id FROM reading_sessions
             WHERE started_at >= ?1 ORDER BY started_at",
        )?;
        let rows = stmt.query_map(params![day_start(first, tz_offset_secs)], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        let mut buckets: BTreeMap<i64, (i64, i64)> = BTreeMap::new();
        let mut books: HashSet<i64> = HashSet::new();
        let mut seconds = 0;
        let mut sessions = 0;
        for row in rows {
            let (started_at, spent, chars, book_id) = row?;
            let slot = buckets
                .entry(local_day(started_at, tz_offset_secs))
                .or_default();
            slot.0 += spent;
            slot.1 += chars;
            books.insert(book_id);
            seconds += spent;
            sessions += 1;
        }
        let days: Vec<DailyReading> = (first..=today)
            .map(|day| {
                let (seconds, chars) = buckets.get(&day).copied().unwrap_or((0, 0));
                DailyReading {
                    day: civil_date(day),
                    seconds,
                    chars,
                }
            })
            .collect();
        let (streak, longest_streak) = streaks(&days);
        Ok(ReadingSummary {
            active_days: days.iter().filter(|d| d.seconds > 0).count(),
            streak,
            longest_streak,
            seconds,
            sessions,
            books: books.len() as i64,
            days,
        })
    }

    /// 窗口内按**本地小时**分桶的时长，24 个一格（「一般几点读」）。
    ///
    /// 记的是会话**开始**那一刻的小时，不是把会话摊到它覆盖的每一格里：摊开需要按分钟
    /// 存数据，而这一屏要回答的是「他习惯什么时候坐下读」，开始那一刻才是那个答案。
    pub fn hour_distribution(&self, days: usize, tz_offset_secs: i64) -> Result<Vec<i64>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let today = local_day(now(), tz_offset_secs);
        let since = day_start(today - days as i64 + 1, tz_offset_secs);
        let mut stmt = conn
            .prepare("SELECT started_at, seconds FROM reading_sessions WHERE started_at >= ?1")?;
        let rows = stmt.query_map(params![since], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?;
        let mut hours = vec![0i64; HOURS_PER_DAY];
        for row in rows {
            let (started_at, seconds) = row?;
            hours[local_hour(started_at, tz_offset_secs)] += seconds;
        }
        Ok(hours)
    }

    /// 窗口内读书时间最多的几本书，最近的读数在先。
    pub fn top_books(
        &self,
        days: usize,
        tz_offset_secs: i64,
        limit: usize,
    ) -> Result<Vec<ReadingRank>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let today = local_day(now(), tz_offset_secs);
        let since = day_start(today - days as i64 + 1, tz_offset_secs);
        let mut stmt = conn.prepare(
            "SELECT s.book_id, b.title, coalesce(sum(s.seconds), 0), count(*)
             FROM reading_sessions s JOIN books b ON b.id = s.book_id
             WHERE s.started_at >= ?1
             GROUP BY s.book_id, b.title
             ORDER BY sum(s.seconds) DESC, max(s.started_at) DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![since, limit as i64], |r| {
            Ok(ReadingRank {
                book_id: r.get(0)?,
                title: r.get(1)?,
                seconds: r.get(2)?,
                sessions: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}

/// `(此刻的连续天数, 窗口内最长的一段)`，都按**有读**的天算。
///
/// 从末尾（今天）往前数。末尾那一格为 0 时跳过它再继续 —— 否则每天早晨到第一次翻开书
/// 之前，「连续 N 天」都会显示成 0，而读者那一天其实还没过完。
fn streaks(days: &[DailyReading]) -> (usize, usize) {
    let read = |day: &DailyReading| day.seconds > 0;
    let mut current = 0;
    for (step_back, day) in days.iter().rev().enumerate() {
        if read(day) {
            current += 1;
        } else if step_back == 0 {
            continue;
        } else {
            break;
        }
    }
    let mut longest = 0;
    let mut run = 0;
    for day in days {
        if read(day) {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    (current, longest)
}

fn session_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ReadingSession> {
    Ok(ReadingSession {
        id: r.get(0)?,
        book_id: r.get(1)?,
        started_at: r.get(2)?,
        ended_at: r.get(3)?,
        seconds: r.get(4)?,
        chars: r.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    //! 阅读统计：会话往返、时长派生、按本地日的汇总与补零、级联删除。
    //!
    //! 「今天」取自 `SystemTime::now()`，所以这里的日序断言全部是**相对**算出来的：
    //! 测试从不断言 `civil_date(now)` 是哪个字面量（那会随运行时刻翻车），
    //! 而是先拿真实时刻定出今天，再往前后推。

    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::Store;
    use crate::content::format::{DocMeta, Format};
    use crate::infra::time::{civil_date, day_start, local_day};
    use crate::store::DailyReading;

    fn meta(title: &str) -> DocMeta {
        DocMeta {
            title: title.into(),
            authors: vec!["作者甲".into()],
            format: Format::Epub,
            size: 1,
            ..DocMeta::default()
        }
    }

    fn path(name: &str) -> PathBuf {
        Path::new("/books").join(name)
    }

    fn now_secs() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    fn at_days(days: &[DailyReading], day: i64) -> i64 {
        days.iter()
            .find(|d| d.day == civil_date(day))
            .map(|d| d.seconds)
            .unwrap_or_else(|| panic!("第 {day} 天不在窗口里：{days:?}"))
    }

    #[test]
    fn a_session_round_trips_and_seconds_come_from_the_bounds() {
        let store = Store::in_memory().unwrap();
        let book = store.add_book(&path("a.epub"), &meta("A")).unwrap();
        let id = store.record_session(book, 1_000, 1_600, 42).unwrap();

        let got = store.sessions(book, 10).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, id);
        assert_eq!(got[0].started_at, 1_000);
        assert_eq!(got[0].ended_at, 1_600);
        assert_eq!(got[0].seconds, 600, "时长由起止算出，不由调用方另报");
        assert_eq!(got[0].chars, 42);
    }

    #[test]
    fn an_inverted_session_is_clamped_to_zero_seconds() {
        let store = Store::in_memory().unwrap();
        let book = store.add_book(&path("a.epub"), &meta("A")).unwrap();
        store.record_session(book, 1_000, 900, 0).unwrap();
        assert_eq!(store.sessions(book, 10).unwrap()[0].seconds, 0);
    }

    #[test]
    fn sessions_are_newest_first_and_limited() {
        let store = Store::in_memory().unwrap();
        let book = store.add_book(&path("a.epub"), &meta("A")).unwrap();
        store.record_session(book, 100, 160, 0).unwrap();
        store.record_session(book, 200, 260, 0).unwrap();
        store.record_session(book, 300, 360, 0).unwrap();

        let first_two = store.sessions(book, 2).unwrap();
        assert_eq!(first_two.len(), 2);
        assert_eq!(first_two[0].started_at, 300, "最近的在先");
        assert_eq!(first_two[1].started_at, 200);
    }

    #[test]
    fn totals_add_up_across_books() {
        let store = Store::in_memory().unwrap();
        let a = store.add_book(&path("a.epub"), &meta("A")).unwrap();
        let b = store.add_book(&path("b.epub"), &meta("B")).unwrap();
        store.record_session(a, 0, 100, 10).unwrap();
        store.record_session(a, 1_000, 1_200, 20).unwrap();
        store.record_session(b, 2_000, 2_050, 5).unwrap();

        let totals = store.reading_totals().unwrap();
        assert_eq!(totals.seconds, 350);
        assert_eq!(totals.sessions, 3);
        assert_eq!(totals.books, 2, "两本书都读过");
        assert_eq!(totals.chars, 35);
    }

    #[test]
    fn book_reading_is_per_book_and_empty_until_read() {
        let store = Store::in_memory().unwrap();
        let a = store.add_book(&path("a.epub"), &meta("A")).unwrap();
        let b = store.add_book(&path("b.epub"), &meta("B")).unwrap();

        let empty = store.book_reading(b).unwrap();
        assert_eq!(empty.seconds, 0);
        assert_eq!(empty.sessions, 0);
        assert_eq!(empty.last_read_at, None, "没读过不是错误，只是没有");

        store.record_session(a, 500, 800, 30).unwrap();
        let got = store.book_reading(a).unwrap();
        assert_eq!(got.seconds, 300);
        assert_eq!(got.sessions, 1);
        assert_eq!(got.chars, 30);
        assert_eq!(got.last_read_at, Some(800));
        assert_eq!(store.book_reading(b).unwrap().sessions, 0, "不串到别人的书");
    }

    #[test]
    fn daily_reading_buckets_by_local_day_and_zero_fills() {
        let store = Store::in_memory().unwrap();
        let book = store.add_book(&path("a.epub"), &meta("A")).unwrap();
        let tz = 8 * 3600;
        let today = local_day(now_secs(), tz);
        let noon = |day: i64| day_start(day, tz) + 12 * 3600;

        // 今天两次、昨天一次，前天没有。
        store
            .record_session(book, noon(today), noon(today) + 600, 100)
            .unwrap();
        store
            .record_session(book, noon(today) + 60, noon(today) + 180, 10)
            .unwrap();
        store
            .record_session(book, noon(today - 1), noon(today - 1) + 300, 50)
            .unwrap();

        let days = store.daily_reading(3, tz).unwrap();
        assert_eq!(days.len(), 3);
        assert_eq!(days[0].day, civil_date(today - 2));
        assert_eq!(
            days[0].seconds, 0,
            "没有记录的那天补零，不能把两边柱子挤到一起"
        );
        assert_eq!(days[1].day, civil_date(today - 1));
        assert_eq!(days[1].seconds, 300);
        assert_eq!(days[2].day, civil_date(today), "最后一条是今天");
        assert_eq!(days[2].seconds, 720, "同一天的两段要相加");
        assert_eq!(days[2].chars, 110);
        assert!(store.daily_reading(0, tz).unwrap().is_empty());
    }

    /// 同一个 Unix 秒在两个时区可以落在不同的日序上 —— 这就是为什么
    /// 汇总必须把偏移当参数收进来，而不是在表里写死一个日界线。
    #[test]
    fn the_day_boundary_follows_the_offset() {
        let store = Store::in_memory().unwrap();
        let book = store.add_book(&path("a.epub"), &meta("A")).unwrap();
        let tz_east = 8 * 3600;
        let now = now_secs();
        let today_east = local_day(now, tz_east);
        // 东八区的「昨天 01:00」：UTC 下已是前天的 17:00。
        let at = day_start(today_east - 1, tz_east) + 3600;
        store.record_session(book, at, at + 60, 0).unwrap();

        let east = store.daily_reading(3, tz_east).unwrap();
        let utc = store.daily_reading(3, 0).unwrap();
        assert_eq!(at_days(&east, local_day(at, tz_east)), 60);
        assert_eq!(at_days(&utc, local_day(at, 0)), 60);
        assert_ne!(
            local_day(at, tz_east),
            local_day(at, 0),
            "同一瞬间，两个时区的日序本来就不同"
        );
    }

    /// 窗口汇总把「合计 / 段数 / 几本 / 活跃天数 / 连续天数」一次算齐。
    #[test]
    fn reading_summary_derives_every_window_number_from_the_same_rows() {
        let store = Store::in_memory().unwrap();
        let a = store.add_book(&path("a.epub"), &meta("A")).unwrap();
        let b = store.add_book(&path("b.epub"), &meta("B")).unwrap();
        let tz = 8 * 3600;
        let today = local_day(now_secs(), tz);
        let noon = |day: i64| day_start(day, tz) + 12 * 3600;

        // 今天两本各一段，昨天一段，前天没有。
        store
            .record_session(a, noon(today), noon(today) + 600, 0)
            .unwrap();
        store
            .record_session(b, noon(today) + 60, noon(today) + 260, 0)
            .unwrap();
        store
            .record_session(a, noon(today - 1), noon(today - 1) + 300, 0)
            .unwrap();

        let s = store.reading_summary(3, tz).unwrap();
        assert_eq!(s.seconds, 1100, "600 + 200 + 300");
        assert_eq!(s.sessions, 3);
        assert_eq!(s.books, 2, "两本各算一本，不按段数");
        assert_eq!(s.active_days, 2, "前天那一格不算活跃");
        assert_eq!(s.days.len(), 3);
        assert_eq!(s.streak, 2, "今天与昨天连着");
        assert_eq!(s.longest_streak, 2);
    }

    /// 「今天还没读」不该把连续天数打成 0 —— 那一天还没过完。
    #[test]
    fn an_unread_today_does_not_break_the_streak() {
        let store = Store::in_memory().unwrap();
        let book = store.add_book(&path("a.epub"), &meta("A")).unwrap();
        let tz = 8 * 3600;
        let today = local_day(now_secs(), tz);
        let noon = |day: i64| day_start(day, tz) + 12 * 3600;
        store
            .record_session(book, noon(today - 1), noon(today - 1) + 300, 0)
            .unwrap();
        store
            .record_session(book, noon(today - 2), noon(today - 2) + 300, 0)
            .unwrap();

        let s = store.reading_summary(5, tz).unwrap();
        assert_eq!(s.seconds_at_today(), 0, "今天那一格确实是 0");
        assert_eq!(s.streak, 2, "从昨天起算，不是 0");
        assert_eq!(s.active_days, 2);
    }

    /// 断了就是断了：昨天的空档要终止连续，但最长那一段还在。
    #[test]
    fn a_gap_ends_the_current_streak_but_not_the_longest() {
        let store = Store::in_memory().unwrap();
        let book = store.add_book(&path("a.epub"), &meta("A")).unwrap();
        let tz = 8 * 3600;
        let today = local_day(now_secs(), tz);
        let noon = |day: i64| day_start(day, tz) + 12 * 3600;
        store
            .record_session(book, noon(today), noon(today) + 60, 0)
            .unwrap();
        // 昨天空着，前天与大前天连着读过。
        store
            .record_session(book, noon(today - 2), noon(today - 2) + 60, 0)
            .unwrap();
        store
            .record_session(book, noon(today - 3), noon(today - 3) + 60, 0)
            .unwrap();

        let s = store.reading_summary(6, tz).unwrap();
        assert_eq!(s.streak, 1, "今天读完就断在昨天那个空档上");
        assert_eq!(s.longest_streak, 2, "前天那两段仍是窗口内最长");
    }

    /// 时段分布按**本地**小时分 24 格，并且跟着偏移走。
    #[test]
    fn hour_distribution_buckets_by_local_hour_in_a_fixed_table() {
        let store = Store::in_memory().unwrap();
        let book = store.add_book(&path("a.epub"), &meta("A")).unwrap();
        let tz = 8 * 3600;
        let today = local_day(now_secs(), tz);
        // 本地 08:00 读了一刻钟。
        let morning = day_start(today, tz) + 8 * 3600;
        store
            .record_session(book, morning, morning + 900, 0)
            .unwrap();

        let hours = store.hour_distribution(3, tz).unwrap();
        assert_eq!(
            hours.len(),
            crate::infra::time::HOURS_PER_DAY,
            "表宽永远是 24"
        );
        assert_eq!(hours[8], 900);
        assert_eq!(hours.iter().sum::<i64>(), 900, "一段只进一格");
        // 同一个瞬间换到 UTC 看是凌晨 0 点 —— 格子跟着日界线一起挪。
        assert_eq!(store.hour_distribution(3, 0).unwrap()[0], 900);
    }

    #[test]
    fn top_books_ranks_by_time_spent_and_takes_the_title_along() {
        let store = Store::in_memory().unwrap();
        let a = store.add_book(&path("a.epub"), &meta("漫长的书")).unwrap();
        let b = store.add_book(&path("b.epub"), &meta("短的书")).unwrap();
        let tz = 8 * 3600;
        let today = local_day(now_secs(), tz);
        let noon = |day: i64| day_start(day, tz) + 12 * 3600;
        store
            .record_session(b, noon(today), noon(today) + 60, 0)
            .unwrap();
        store
            .record_session(a, noon(today), noon(today) + 600, 0)
            .unwrap();
        store
            .record_session(a, noon(today) + 700, noon(today) + 1000, 0)
            .unwrap();
        // 窗口之外（30 天前）的一段不该参与排行。
        store
            .record_session(b, noon(today - 30), noon(today - 30) + 9000, 0)
            .unwrap();

        let top = store.top_books(7, tz, 5).unwrap();
        assert_eq!(top.len(), 2, "窗口内只有两本有读数");
        assert_eq!(top[0].title, "漫长的书", "花时间多的在前");
        assert_eq!(top[0].seconds, 900);
        assert_eq!(top[0].sessions, 2);
        assert_eq!(top[1].book_id, b);

        assert_eq!(store.top_books(7, tz, 1).unwrap().len(), 1, "limit 收得住");
        let wide = store.top_books(60, tz, 5).unwrap();
        assert_eq!(
            (wide[0].book_id, wide[0].seconds),
            (b, 9060),
            "放宽窗口就把 30 天前那一段算进来，榜首跟着换人"
        );
    }

    #[test]
    fn removing_a_book_takes_its_sessions_with_it() {
        let store = Store::in_memory().unwrap();
        let a = store.add_book(&path("a.epub"), &meta("A")).unwrap();
        let b = store.add_book(&path("b.epub"), &meta("B")).unwrap();
        store.record_session(a, 0, 100, 0).unwrap();
        store.record_session(b, 0, 100, 0).unwrap();

        store.remove_book(a).unwrap();
        assert!(
            store.sessions(a, 10).unwrap().is_empty(),
            "删书带走它的会话"
        );
        assert_eq!(
            store.reading_totals().unwrap().sessions,
            1,
            "别人的会话一根手指都不能碰"
        );
    }
}
