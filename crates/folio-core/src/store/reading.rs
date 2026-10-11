//! 阅读统计：会话往返、时长派生、按本地日的汇总与补零。
//!
//! 存的是**原始会话**而不是按天汇总 —— 「今天读了多久」依赖读者所在时区，而时区只有
//! 界面侧知道。core 只留事实（何时开始、多久、前进多少字），汇总时把偏移当参数收进来
//! （见 `crate::time`），不把某个时区的日界线写死进表。

use std::collections::BTreeMap;

use rusqlite::params;

use super::types::{BookReading, DailyReading, ReadingSession, ReadingTotals};
use super::{Store, now};
use crate::Result;
use crate::time::{civil_date, day_start, local_day};

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
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        if days == 0 {
            return Ok(Vec::new());
        }
        let today = local_day(now(), tz_offset_secs);
        let first = today - days as i64 + 1;
        let mut stmt = conn.prepare(
            "SELECT started_at, seconds, chars FROM reading_sessions
             WHERE started_at >= ?1 ORDER BY started_at",
        )?;
        let mut buckets: BTreeMap<i64, (i64, i64)> = BTreeMap::new();
        let rows = stmt.query_map(params![day_start(first, tz_offset_secs)], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (started_at, seconds, chars) = row?;
            let slot = buckets
                .entry(local_day(started_at, tz_offset_secs))
                .or_default();
            slot.0 += seconds;
            slot.1 += chars;
        }
        Ok((first..=today)
            .map(|day| {
                let (seconds, chars) = buckets.get(&day).copied().unwrap_or((0, 0));
                DailyReading {
                    day: civil_date(day),
                    seconds,
                    chars,
                }
            })
            .collect())
    }
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
    use crate::format::{DocMeta, Format};
    use crate::store::DailyReading;
    use crate::time::{civil_date, day_start, local_day};

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
