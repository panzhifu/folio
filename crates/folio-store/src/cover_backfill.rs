//! 封面补齐作业的读取入口：分页扫描与按书号取回。
//!
//! 单独成文件，是因为这几个查询只服务 `ingest::import::maintenance` 的补扫作业：
//! 它按页推进、带失败账本，和书架 / 阅读路径的查询不是同一条懒补口径。

use rusqlite::params;

use super::Store;
use crate::Result;

/// 封面补齐那一趟要读的一行：`BookRow` 不带封面描述，而补齐既要判「指针在不在」
/// 也要判「那个文件还在不在」。
#[derive(Debug, Clone)]
pub struct CoverBackfillRow {
    pub id: i64,
    pub path: String,
    pub cover_hash: Option<String>,
    pub cover_mime: Option<String>,
}

impl Store {
    /// 返回全部行的封面描述，包括已有哈希但文件可能丢失的行。
    /// 此处不打开容器；调用方按 book_root + cover::resolve 检查文件。
    pub fn rows_for_cover_backfill(&self) -> Result<Vec<CoverBackfillRow>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt =
            conn.prepare("SELECT id, path, cover_hash, cover_mime FROM books ORDER BY id")?;
        let rows = stmt
            .query_map([], |r| {
                Ok(CoverBackfillRow {
                    id: r.get(0)?,
                    path: r.get(1)?,
                    cover_hash: r.get(2)?,
                    cover_mime: r.get(3)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    /// 分页取封面补齐的行：只取书号严格大于 `after` 的下一批，按书号升序。
    ///
    /// 分页不是为了省内存，而是让取消点之间的库锁持有时间有界：整趟补齐可能要看几千本，
    /// 一次全取出来会把「用户点了关闭」拖到扫完为止。
    pub fn cover_backfill_page(&self, after: i64, limit: usize) -> Result<Vec<CoverBackfillRow>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, path, cover_hash, cover_mime FROM books
             WHERE id > ?1 ORDER BY id LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![after, limit as i64], |r| {
                Ok(CoverBackfillRow {
                    id: r.get(0)?,
                    path: r.get(1)?,
                    cover_hash: r.get(2)?,
                    cover_mime: r.get(3)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    /// 按书号取回一批行，保持入参顺序（失败账本按「先该重试的先来」排好）。
    /// 库里面目全非时只返回还在的行，调用方据此销掉已经删掉的账。
    pub fn cover_backfill_rows(&self, ids: &[i64]) -> Result<Vec<CoverBackfillRow>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let marks = vec!["?"; ids.len()].join(",");
        let sql =
            format!("SELECT id, path, cover_hash, cover_mime FROM books WHERE id IN ({marks})");
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt
            .query_map(rusqlite::params_from_iter(ids), |r| {
                Ok(CoverBackfillRow {
                    id: r.get(0)?,
                    path: r.get(1)?,
                    cover_hash: r.get(2)?,
                    cover_mime: r.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let order: std::collections::HashMap<i64, usize> =
            ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        rows.sort_by_key(|row| order.get(&row.id).copied().unwrap_or(usize::MAX));
        Ok(rows)
    }
}
