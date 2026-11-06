//! **导入记录**：一批导入跑完之后，逐本的着落留在本机，重启还在。
//!
//! 存在的理由：那张卡从前只活在这一次运行里 —— 应用一关、进程一被系统收走，「刚才那几本
//! 怎么样了」就没了，而那正是导入这件事唯一要回答的问题。
//!
//! 为什么是一张独立的表（而不是 `books` 上多几列）：同步那条链的取数面是逐条 SELECT 写死的
//! （[`store::sync`](super::sync) 只碰 `books` / `positions` / `annotations` / `bookmarks` /
//! `sync_tombstones`），而这里带着读者手上那些文件的**原始文件名** —— 那是本机的事，不该
//! 跟着快照传到 WebDAV 上去。这条由本文件底部的 `import_records_never_enter_the_sync_payload`
//! 钉着：它看的是那份取数清单，不是这张表的名字。
//!
//! `book_id` 不建外键，读的时候用一条 LEFT JOIN 现算：删掉一本书不该把那次的导入记录一并
//! 抹掉（那件事发生过），而那本已经不在了，屏幕上就不该再摆那颗「打开」。

use rusqlite::params;

use crate::infra::error::ImportReject;

use super::Store;

/// 留下多少条记录。封顶是为了不让这张表无限长：它是「刚才那几本」，不是书库的账
/// （要长期翻的在 `books` 与「信息」那一页）。掉的是最老的那些。
pub const IMPORT_LOG_KEEP: i64 = 500;

/// 一本导入进来的书最后怎么样了。与 [`ImportReport`](crate::ingest::import::ImportReport)
/// 那五个类别一一对应，只是这里说的是**一本**，而报表数的是**一批**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportLanding {
    /// 新落地的一本。
    Added,
    /// 内容哈希已经有那一行了：架上那一本早就在。
    Duplicate,
    /// 没进来，按类别。
    Rejected(ImportReject),
}

impl ImportLanding {
    /// 存进库里那串文本码。存文本而不是序号：序号把表的形状绑死在枚举的排列顺序上，
    /// 那个顺序哪天动一下，老行就集体改口。
    fn code(&self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Duplicate => "duplicate",
            Self::Rejected(ImportReject::NeedsPassword) => "needs_password",
            Self::Rejected(ImportReject::Encrypted) => "encrypted",
            Self::Rejected(ImportReject::CorruptContainer) => "corrupt",
            Self::Rejected(ImportReject::UnsupportedFormat) => "unsupported",
            Self::Rejected(ImportReject::Unreadable) => "unreadable",
        }
    }

    /// 文本码 → 结局。认不出来的码算「读不动」那一档：报表不许悄悄少算一本，而读者只会
    /// 看见「少了几本」那种坏法，从前在 `ImportReport::tally` 里已经认过一次了。
    fn from_code(code: &str) -> Self {
        match code {
            "added" => Self::Added,
            "duplicate" => Self::Duplicate,
            "needs_password" => Self::Rejected(ImportReject::NeedsPassword),
            "encrypted" => Self::Rejected(ImportReject::Encrypted),
            "corrupt" => Self::Rejected(ImportReject::CorruptContainer),
            "unsupported" => Self::Rejected(ImportReject::UnsupportedFormat),
            _ => Self::Rejected(ImportReject::Unreadable),
        }
    }
}

/// 一条导入记录。字段就是屏幕上那一行要说的东西，措辞归界面。
#[derive(Debug, Clone, PartialEq)]
pub struct ImportLogEntry {
    /// 落定那一刻（Unix 秒）。屏幕上那个日期由界面按读者的时区算。
    pub made_at: i64,
    /// 当时那个文件名（带扩展名）：界面上削掉扩展名再说它，削法只有一处。
    pub name: String,
    /// 扩展名的大写。没有就 `None`，那一格不摆。
    pub format: Option<String>,
    /// 字节数。`None` = 当时不知道（远端没给长度）。
    pub size: Option<u64>,
    pub landing: ImportLanding,
    /// 现在还能打开的那一本。`None` = 没进来，或者进来过又被删了（读的时候现算，见模块头）。
    pub book_id: Option<i64>,
    /// 跑完那一格花了多久（秒）。一个包算一趟，包里那几条同一个数。
    pub secs: f32,
}

impl Store {
    /// 记下一批的逐本着落（新的在后），顺手把超出封顶的最老那些剪掉。
    pub fn record_imports(&self, entries: &[ImportLogEntry]) -> crate::Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        // 一批一次插入：逐本一次 `execute` 就是逐本一次事务，导一千本等于敲一千次盘。
        for entry in entries {
            conn.execute(
                "INSERT INTO import_records (made_at, name, format, size, status, book_id, secs)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    entry.made_at,
                    entry.name,
                    entry.format,
                    entry.size.map(|size| size as i64),
                    entry.landing.code(),
                    entry.book_id,
                    entry.secs as f64,
                ],
            )?;
        }
        conn.execute(
            "DELETE FROM import_records
             WHERE id NOT IN (SELECT id FROM import_records ORDER BY id DESC LIMIT ?1)",
            params![IMPORT_LOG_KEEP],
        )?;
        Ok(())
    }

    /// 最近 `limit` 条，新的在前。
    ///
    /// `book_id` 是 JOIN 出来的：库里那一格记的是「当时落地的哪一本」，而那一本可能已经被
    /// 读者删了 —— 记录要留着，那颗「打开」不能留。
    pub fn import_log(&self, limit: i64) -> crate::Result<Vec<ImportLogEntry>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows = conn.prepare(
            "SELECT r.made_at, r.name, r.format, r.size, r.status, b.id, r.secs
             FROM import_records r
             LEFT JOIN books b ON b.id = r.book_id
             ORDER BY r.id DESC
             LIMIT ?1",
        )?;
        let taken = rows.query_map(params![limit], |row| {
            Ok(ImportLogEntry {
                made_at: row.get(0)?,
                name: row.get(1)?,
                format: row.get(2)?,
                size: row.get::<_, Option<i64>>(3)?.map(|size| size as u64),
                landing: ImportLanding::from_code(&row.get::<_, String>(4)?),
                book_id: row.get(5)?,
                secs: row.get::<_, f64>(6)? as f32,
            })
        })?;
        taken.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// 清空这张记录表，交回删掉了多少条。书库一条不动。
    pub fn clear_import_log(&self) -> crate::Result<usize> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Ok(conn.execute("DELETE FROM import_records", [])?)
    }
}

#[cfg(test)]
mod tests {
    //! 这张表存在的全部理由是「重开一次库还在」，所以每条门都从**关掉再开一次**那一手起；
    //! 全程只走公开的 `Store::open_at` / `add_book` / `remove_book`，不碰私有字段。

    use std::path::Path;

    use crate::content::format::{DocMeta, Format};
    use crate::infra::error::ImportReject;
    use crate::store::{IMPORT_LOG_KEEP, ImportLanding, ImportLogEntry, Store};

    fn open(dir: &Path) -> Store {
        Store::open_at(&dir.join("folio.db")).unwrap()
    }

    fn entry(
        name: &str,
        made_at: i64,
        landing: ImportLanding,
        book_id: Option<i64>,
    ) -> ImportLogEntry {
        ImportLogEntry {
            made_at,
            name: name.into(),
            format: Some("EPUB".into()),
            size: Some(4096),
            landing,
            book_id,
            secs: 1.5,
        }
    }

    fn meta(title: &str) -> DocMeta {
        DocMeta {
            title: title.into(),
            format: Format::Epub,
            size: 4096,
            ..DocMeta::default()
        }
    }

    /// 记下的那些重开一次库还在，而且新的在前。
    #[test]
    fn the_log_survives_reopening_the_library() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        store
            .record_imports(&[
                entry("第一本.epub", 100, ImportLanding::Added, None),
                entry(
                    "第二本.mobi",
                    200,
                    ImportLanding::Rejected(ImportReject::UnsupportedFormat),
                    None,
                ),
            ])
            .unwrap();
        drop(store);

        let log = open(dir.path()).import_log(10).unwrap();
        assert_eq!(log.len(), 2, "重启之后这两条都该还在");
        assert_eq!(log[0].name, "第二本.mobi", "新的在前");
        assert_eq!(
            log[0].landing,
            ImportLanding::Rejected(ImportReject::UnsupportedFormat)
        );
        assert_eq!(log[1].secs, 1.5, "耗时是记下来的，不是重算出来的");
    }

    /// 删掉一本书带走的是那一本书，不是那次导入：记录还在，那颗「打开」没了。
    #[test]
    fn a_deleted_book_keeps_its_record_but_loses_the_opener() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let id = store
            .add_book(Path::new("/tmp/第一本.epub"), &meta("第一本"))
            .unwrap();
        store
            .record_imports(&[entry("第一本.epub", 100, ImportLanding::Added, Some(id))])
            .unwrap();
        assert_eq!(
            store.import_log(10).unwrap()[0].book_id,
            Some(id),
            "书还在时那颗「打开」要有对象"
        );

        store.remove_book(id).unwrap();
        let log = store.import_log(10).unwrap();
        assert_eq!(log.len(), 1, "书没了，那件事还发生过");
        assert_eq!(
            log[0].book_id, None,
            "而那本已经不在了，界面上不该再摆「打开」"
        );
    }

    /// 封顶剪的是最老的那些，留下的是最新那一批。
    #[test]
    fn the_log_keeps_the_newest_rows_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let over: Vec<ImportLogEntry> = (0..IMPORT_LOG_KEEP + 50)
            .map(|i| entry(&format!("第{i}本.epub"), i, ImportLanding::Added, None))
            .collect();
        store.record_imports(&over).unwrap();

        let log = store.import_log(IMPORT_LOG_KEEP + 500).unwrap();
        assert_eq!(
            log.len() as i64,
            IMPORT_LOG_KEEP,
            "超出封顶的最老那些该被剪掉"
        );
        assert_eq!(log[0].name, format!("第{}本.epub", IMPORT_LOG_KEEP + 49));
    }

    /// 文本码与枚举是双向的；认不出来的那一串算「读不动」——报表不许悄悄少算一本。
    #[test]
    fn every_status_code_round_trips_and_unknown_reads_as_unreadable() {
        let all = [
            ImportLanding::Added,
            ImportLanding::Duplicate,
            ImportLanding::Rejected(ImportReject::NeedsPassword),
            ImportLanding::Rejected(ImportReject::Encrypted),
            ImportLanding::Rejected(ImportReject::CorruptContainer),
            ImportLanding::Rejected(ImportReject::UnsupportedFormat),
            ImportLanding::Rejected(ImportReject::Unreadable),
        ];
        for landing in all {
            assert_eq!(ImportLanding::from_code(landing.code()), landing);
        }
        assert_eq!(
            ImportLanding::from_code("哪天新加的一档"),
            ImportLanding::Rejected(ImportReject::Unreadable)
        );
    }

    /// 这张表不进同步的载荷：它带着读者手上那些文件的原始文件名，那是本机的事。
    ///
    /// 判据看的是**载荷里有没有那个名字**，而不是那张表的名字 —— 哪天有人把记录塞进
    /// `collect_sync` 的某一条 SELECT，这一句就红了，而它不要求测试跟着改表名。
    #[test]
    fn import_records_never_enter_the_sync_payload() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        store
            .record_imports(&[entry(
                "一份不该上云的文件名.epub",
                100,
                ImportLanding::Added,
                None,
            )])
            .unwrap();
        let payload = format!("{:?}", store.collect_sync("device-1", 0).unwrap());
        assert!(
            !payload.contains("不该上云"),
            "同步载荷里出现了导入记录里的文件名：{payload}"
        );
    }

    /// 清空只清这一张表，书库一条不动。
    #[test]
    fn clearing_the_log_leaves_the_library_alone() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let id = store
            .add_book(Path::new("/tmp/第一本.epub"), &meta("第一本"))
            .unwrap();
        store
            .record_imports(&[entry("第一本.epub", 100, ImportLanding::Added, Some(id))])
            .unwrap();

        assert_eq!(store.clear_import_log().unwrap(), 1);
        assert!(store.import_log(10).unwrap().is_empty());
        assert_eq!(
            store.shelf("").unwrap().len(),
            1,
            "「清空」清的是记录，不是书"
        );
    }
}
