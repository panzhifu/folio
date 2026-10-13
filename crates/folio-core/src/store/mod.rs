//! SQLite 书库存储。[`Store`] 是 `Send + Sync` 的：连接锁在内部的一把 [`Mutex`] 上，
//! 界面线程与后台线程（导入管线）共用同一份句柄。
//!
//! 锁纪律：**一次调用只上锁一次**。公开方法只负责「拿守卫 + 调对应的 `*_in(&conn, ..)`
//! 私有助手」，助手内部绝不再碰 `conn` —— `Mutex` 不可重入，方法套方法就是自锁。
//! 调用方那头还有一条配套纪律（见 `crate::ingest::import` 的模块文档）：拷贝与解析**不持锁**，
//! 只有 register 那一小段持 —— 后台导一本书时，界面最多等几十毫秒。
//!
//! 这一层按主题拆开，一个主题一个文件，`impl Store` 分散在各自那一份里：
//!
//! - [`schema`] —— 迁移清单（只增不改的日志）。
//! - [`types`] —— 对外那一行的形状。
//! - [`books`] —— 书目与书架：入库、查询、删除、进度、分组。
//! - [`content`] —— 正文与页表：整本落库/读回、正文压缩。
//! - [`annotations`] / [`collections`] / [`reading`] —— 批注 / 集合 / 阅读统计。
//!
//! 拆文件不拆实体：`Store` 还是一个类型、一把锁。**跨文件访问 `conn` 是允许的**
//! （子模块看得见父模块的私有字段），但那条锁纪律在拆开之后更要紧 ——
//! 一个公开方法只该上锁一次，`*_in` 助手只接 `&Connection`。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, params};

use crate::Result;

mod annotations;
mod books;
mod collections;
mod content;
mod reading;
mod schema;
mod types;

#[cfg(test)]
mod tests;

pub use books::StaleReason;
// 封面落盘与缩略图都要按「这本副本在哪儿」反推书库根，而书库位置不在 `Store` 上
// （那是读者确认过的目录）—— 这个函数就是那条反推规则。公开它是因为界面侧也要用：
// 缩略图缓存目录挂在书库底下，而界面手上只有副本路径。
pub use books::book_root;
pub use types::*;

use schema::MIGRATIONS;

pub struct Store {
    conn: Mutex<Connection>,
    /// 这一本的重扫作业**已在飞**的 `book_id`（[`crate::ingest::import::schedule_reindex`] 的记账）。
    ///
    /// 挂在 `Store` 上而不是进程级的 `static`：判据说的是「这个库里的那一本」，而测试各开
    /// 各的内存库、`book_id` 都从 1 起 —— 放进进程级集合里，两条并发用例的同名 id 会互相
    /// 挡掉对方的提交，那是一条绿不了也红不明的假门。
    reindex_queued: Mutex<HashSet<i64>>,
    /// 试过且**失败**的那些 `book_id`：不再自动重投。
    ///
    /// 判据只看库里的 `index_version`，所以一本注定重扫失败的书，没有这一格就要每次点开
    /// 都重付一次全量重读（重扫必然读完整本，见 `plans/导入管线.md` §八 第 3 条）。
    /// 只在这一进程内记，不落库：下次启动该再给它一次机会。
    reindex_failed: Mutex<HashSet<i64>>,
}

// 这条断言是「Store 可跨线程」的承诺本身：后台线程（导入管线）能拿着 `&Store`
// 直接落库。将来谁要把 `Store` 改回非 `Sync`，编译器在这里拦住。
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Store>();
};

/// 当前 Unix 秒。全库只走这一个时钟,测试要改也只改一处。
///
/// 实现搬到 [`crate::infra::time::now`]：界面侧记阅读会话的起止要读**同一个**钟,否则
/// 「库里那一段的时长」和「屏幕上算出来的」会差出几十秒,而那没有任何报错。
pub(super) use crate::infra::time::now;

pub fn default_db_path() -> PathBuf {
    crate::infra::paths::db_path()
}

impl Store {
    pub fn open_default() -> Result<Self> {
        Self::open_at(&default_db_path())
    }

    /// 打开（必要时建）库文件并跑迁移。
    ///
    /// **一份打不开的库只报错，绝不删掉重来**（`plans/基础设施.md` §5.2）：进度、批注、
    /// 集合都只在这里，损坏的库被自动重建等于丢数据。`Connection::open` 对已存在的坏文件
    /// 不会覆写，迁移失败会把错误原样抛上来 —— 怎么提示是入口的事，这层保证**磁盘上那个
    /// 文件一个字节都不动**（`a_corrupt_database_is_reported_not_overwritten` 钉住这条）。
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
        // `auto_vacuum` **必须在建表之前**设，此时不生效。
        //
        // 它管的是「删掉的行把页还给系统，而不是留在 freelist 里等复用」。重导入一整本
        // （`save_flow` 先删光再重插）与删书都是大块删，不开这个开关库文件就只会一路涨到
        // 历史最高水位 —— 实测那台机器上 5.47 MB 的库里 1.96 MB 是这种没人要的空页。
        //
        // `INCREMENTAL` 而不是 `FULL`：全量模式下每次提交都要挪页，重导入一本书会变成
        // 几千次搬动；增量模式把「什么时候还」交给调用方，见 [`Store::reclaim`]。
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
            // 每条迁移各自一句：`execute_batch` 中间失败时，日志里最后一条「完成」的就是
            // 该看的那条 —— 库停在 v3 还是 v4，决定的是后面怎么修。
            tracing::debug!("store: 应用迁移 v{target}");
            conn.execute_batch(sql)?;
            conn.pragma_update(None, "user_version", target)?;
            tracing::info!("store: 迁移 v{target} 完成");
            // v8 的 Rust 那半必须在 v9 之前跑：v9 要删掉 `chapters.spine_href`，
            // 而回填位置正是按它算的。
            if target == 8 {
                Self::finish_v8(&conn)?;
            }
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
        })
    }

    /// v8 的 Rust 那半：给老库的章行算出位置，然后把 `chapter_text` 丢掉。
    ///
    /// **必须在 v9 之前跑**（`from` 里就卡在 v8 之后）：v9 会删掉 `chapters.spine_href`，
    /// 而这里正是按它算位置。
    ///
    /// 两件事 SQL 都做不干净：
    ///
    /// - 位置得按 `books.format` 分派（名单式的按 `spine_href` 认条目，单文件的按序号），
    ///   而位置是 JSON，在 SQL 里拼字符串要自己处理转义；
    /// - 删表要等回填完。
    ///
    /// **可重入**：位置非空的行不动，`chapter_text` 不在了就直接返回。
    /// 回填不出来的（格式不认识、EPUB 又没 href）留空 —— 空位置在打开时会被当作
    /// 「回容器重读一遍」，而不是「这一章是空的」。
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
                let at = crate::content::format::Format::parse(format).map(|format| {
                    let chapter =
                        crate::content::format::Chapter::inline(String::new(), String::new())
                            .with_spine_href(spine_href.clone());
                    crate::content::format::locator_for(format, &chapter, *idx as usize)
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

    /// 把已经在 freelist 里的页还给系统。
    ///
    /// 开了 `auto_vacuum = INCREMENTAL` 之后，删除本身只把页标成空闲；要真正吐出去
    /// 得主动叫一次。重导入与删书都是「整本正文一起没」的大块删，正好在这里收。
    /// **必须在事务外调**：`VACUUM` 系列不能在事务里跑。
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
}
