//! SQLite 书库：书目、阅读记录、批注、集合与同步数据。
//! Store 用一把连接锁跨线程共享；公开方法只上锁一次，*_in 助手不再次取锁。
//! 拷贝、解析和文件探测放在库锁外。书文件暂不可用时保留用户记录。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension as _, params};

use folio_format::ext::ChapterExt;

/// 书库错误保留 SQL、JSON 与带路径的 I/O 因果链。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite: {0}")]
    Sql(#[from] rusqlite::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    /// 书库目录里那一格文件读不了 / 删不掉（残留副本清理、封面落盘）。
    /// `path` 是出事的那个文件，「删不掉」与「读不到」对读者是两句话。
    #[error("读 {path} 失败：{source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    /// 空名称在入口拒绝，避免落成误导的 UNIQUE 冲突。
    #[error("集合名不能为空")]
    EmptyCollectionName,

    #[error("数据完整性：{0}")]
    Integrity(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// 把底层错误套上「哪个文件」再往外传，原始错误留在链上。
pub trait IoResultExt<T> {
    fn io_path(self, path: impl Into<std::path::PathBuf>) -> Result<T>;
}

impl<T, E: std::error::Error + Send + Sync + 'static> IoResultExt<T> for std::result::Result<T, E> {
    fn io_path(self, path: impl Into<std::path::PathBuf>) -> Result<T> {
        self.map_err(|e| Error::Io {
            path: path.into(),
            source: Box::new(e),
        })
    }
}

mod annotations;
pub mod assets;
mod bookmarks;
mod books;
mod collections;
mod content;
mod cover_backfill;
mod import_log;
pub mod notes;
mod passphrases;
mod reading;
mod record_revisions;
mod record_writes;
mod restore;
mod revisions;
mod schema;
mod sources;
pub mod stats;
mod sync;
mod sync_order;
mod tags;
mod translations;
// 保留既有类型导出路径。
pub use folio_model::types;

#[cfg(test)]
mod tests;

pub use books::book_root;
pub use books::{SourceUnavailable, StaleReason, UnavailableSource};
pub use cover_backfill::CoverBackfillRow;
pub use folio_model::revision::{BookRevision, BookSource, RevisionFence};
pub use import_log::{IMPORT_LOG_KEEP, ImportLanding, ImportLogEntry};
pub use revisions::{
    AdoptResult, ImportRegistration, LeasedRevision, RevisionLease, RevisionReady, RevisionSnapshot,
};
pub use types::*;

use schema::MIGRATIONS;

pub struct Store {
    conn: Mutex<Connection>,
    /// 按库记录在飞作业；不同 Store 的 book_id 可以相同，不能改为全局集合。
    reindex_queued: Mutex<HashSet<i64>>,
    /// 本进程内失败的作业不自动重投，下次启动允许重试。
    reindex_failed: Mutex<HashSet<i64>>,
    revision_leases: std::sync::Arc<Mutex<std::collections::HashMap<(i64, RevisionFence), usize>>>,
}

// 编译期保证后台作业可共享书库句柄。
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Store>();
};

/// 与界面会话共用的 Unix 秒时钟。
pub(crate) use folio_base::time::now;

pub fn default_db_path() -> PathBuf {
    folio_base::paths::db_path()
}

impl Store {
    pub fn open_default() -> Result<Self> {
        Self::open_at(&default_db_path())
    }

    /// 打开数据库并运行迁移；损坏只报错，绝不删除或重建已有库。
    #[tracing::instrument(level = "debug", skip_all, fields(path = %path.display()))]
    pub fn open_at(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        Self::from(Connection::open(path)?)
    }

    pub fn in_memory() -> Result<Self> {
        Self::from(Connection::open_in_memory()?)
    }

    fn from(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // 建表前设置增量回收；避免每次提交搬页，实际回收由 reclaim 触发。
        let was_auto_vacuum: i64 = conn.query_row("PRAGMA auto_vacuum", [], |r| r.get(0))?;
        if was_auto_vacuum == 0 {
            conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
        }
        let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        for (i, sql) in MIGRATIONS.iter().enumerate() {
            let target = i as i64 + 1;
            if current >= target {
                continue;
            }
            tracing::debug!("store: 应用迁移 v{target}");
            // DDL、Rust 回填和 user_version 同一事务，避免崩溃留下半次迁移。
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
            // v8 的 Rust 那半必须在 v9 之前跑：v9 要删掉 `chapters.spine_href`，
            // 而回填位置正是按它算的。
            if target == 8 {
                Self::finish_v8(&tx)?;
            }
            // 版本跟着这一笔走：`PRAGMA user_version` 写的是库头，提交才落地，回滚就当没发生。
            tx.pragma_update(None, "user_version", target)?;
            tx.commit()?;
            tracing::info!("store: 迁移 v{target} 完成");
        }
        if was_auto_vacuum == 0 {
            // 改 `auto_vacuum` 要一次全量 VACUUM 才会生效，顺带把历史高水位压回去。
            // 一次性代价：老库第一次升级时会重写一遍文件。
            tracing::info!("store: 启用增量 auto_vacuum，重写库文件");
            conn.execute_batch("VACUUM")?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
            reindex_queued: Mutex::new(HashSet::new()),
            reindex_failed: Mutex::new(HashSet::new()),
            revision_leases: std::sync::Arc::new(Mutex::new(std::collections::HashMap::new())),
        })
    }

    /// v8 按格式回填章节位置后删除旧正文表，必须早于 v9 删除 spine_href。
    /// 可重入；未知位置留空，让打开流程回容器读取，不能当作空章节。
    fn finish_v8(conn: &Connection) -> Result<()> {
        if Self::table_exists(conn, "chapter_text")? {
            let rows: Vec<(i64, String, String, i64)> = {
                let mut stmt = conn.prepare(
                    "SELECT c.id, b.format, c.spine_href, c.idx FROM chapters c
                     JOIN books b ON b.id = c.book_id WHERE c.locator = ''",
                )?;
                stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                    .collect::<std::result::Result<Vec<_>, _>>()?
            };
            tracing::info!("store: 给 {} 章补上容器里的位置", rows.len());
            for (chapter_id, format, spine_href, idx) in &rows {
                let at = folio_format::Format::parse(format).map(|format| {
                    let chapter = folio_format::Chapter::inline(String::new(), String::new())
                        .with_spine_href(spine_href.clone());
                    folio_format::locator_for(format, &chapter, *idx as usize)
                });
                // 认不出格式就留空：打开时走「回容器重读」那条降级路。
                let Some(at) = at else { continue };
                conn.execute(
                    "UPDATE chapters SET locator = ?2 WHERE id = ?1",
                    params![chapter_id, serde_json::to_string(&at)?],
                )?;
            }
            conn.execute_batch("DROP TABLE chapter_text")?;
        }
        Ok(())
    }

    fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
        let found: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![name],
            |r| r.get(0),
        )?;
        Ok(found > 0)
    }

    /// 回收 freelist 空页；须在事务外调用。
    fn reclaim(conn: &Connection) -> Result<()> {
        let empty: i64 = conn.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
        if empty > 0 {
            conn.execute_batch("PRAGMA incremental_vacuum")?;
            tracing::debug!("store: 回收 {empty} 个空页");
        }
        Ok(())
    }

    pub fn user_version(&self) -> Result<i64> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    /// 读取一次性标记；缺键或无效值返回 None，允许幂等补齐重试。
    pub fn pref_number(&self, key: &str) -> Result<Option<i64>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let raw: Option<String> = conn
            .query_row("SELECT value_json FROM prefs WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(raw.and_then(|s| serde_json::from_str::<i64>(&s).ok()))
    }

    /// 写一个数字型的一次性标记（有则覆盖）。
    pub fn set_pref_number(&self, key: &str, value: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT INTO prefs (key, value_json) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
            params![key, serde_json::to_string(&value)?],
        )?;
        Ok(())
    }

    /// 原样读取一段文本型标记（封面补齐的失败账本用它存一小段 JSON）；缺键返回 None。
    pub fn pref_text(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let raw: Option<String> = conn
            .query_row("SELECT value_json FROM prefs WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(raw)
    }

    /// 原样写一段文本型标记（有则覆盖）。值按 JSON 文本保存，与 [`Self::pref_number`] 共表。
    pub fn set_pref_text(&self, key: &str, text: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT INTO prefs (key, value_json) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
            params![key, text],
        )?;
        Ok(())
    }

    /// freelist_count × page_size；这是空闲页总量，不保证增量回收能全部释放。
    pub fn freeable_bytes(&self) -> Result<u64> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Self::freeable_in(&conn)
    }

    /// 整库 VACUUM，返回实际减少的字节数。
    /// 不可在事务内调用；应在后台运行，并预留约一份数据库大小的临时空间。
    pub fn vacuum(&self) -> Result<u64> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let before = Self::file_bytes(&conn)?;
        conn.execute_batch("VACUUM")?;
        let after = Self::file_bytes(&conn)?;
        Ok(before.saturating_sub(after))
    }

    /// 用 VACUUM INTO 生成一致快照；WAL 模式不能直接复制数据库主文件。
    /// 已存在的目标会被替换，调用方必须传专用备份路径。
    pub fn backup_to(&self, path: &Path) -> Result<()> {
        if path.exists() {
            std::fs::remove_file(path).map_err(|err| crate::Error::Io {
                path: path.to_path_buf(),
                source: Box::new(err),
            })?;
        }
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute("VACUUM INTO ?1", [path.to_string_lossy().as_ref()])?;
        Ok(())
    }

    /// freelist 折成字节。私有助手：只接 `&Connection`，自己不上锁（那条纪律见模块头）。
    fn freeable_in(conn: &Connection) -> Result<u64> {
        let pages: i64 = conn.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
        Ok(pages.max(0) as u64 * Self::page_size(conn)? as u64)
    }

    /// page_count × page_size；与 WAL checkpoint 后的主文件大小对应。
    fn file_bytes(conn: &Connection) -> Result<u64> {
        let pages: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
        Ok(pages.max(0) as u64 * Self::page_size(conn)? as u64)
    }

    fn page_size(conn: &Connection) -> Result<i64> {
        Ok(conn.query_row("PRAGMA page_size", [], |r| r.get(0))?)
    }
}
