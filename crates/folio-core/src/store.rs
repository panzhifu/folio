use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::Result;
use crate::anchor::TextAnchor;
use crate::format::{Chapter, Content, DocMeta, FixedDoc, FixedPage, FlowDoc, ImageRes};
use crate::group::{GroupBy, ShelfGroup, UNGROUPED, group_shelf};
use crate::position::Locator;
use crate::time::{civil_date, day_start, local_day};

const MIGRATIONS: &[&str] = &[
    // v1：书库、章节、进度、偏好、标注、集合。
    // 标注与集合的表在 v1 一次建好 —— 已经发布过的迁移不能回头改形状。
    r#"
CREATE TABLE books (
    id             INTEGER PRIMARY KEY,
    path           TEXT    NOT NULL UNIQUE,
    title          TEXT    NOT NULL,
    authors        TEXT    NOT NULL DEFAULT '',
    format         TEXT    NOT NULL,
    size           INTEGER NOT NULL DEFAULT 0,
    cover          BLOB,
    added_at       INTEGER NOT NULL,
    last_opened_at INTEGER
);

CREATE TABLE chapters (
    id         INTEGER PRIMARY KEY,
    book_id    INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    idx        INTEGER NOT NULL,
    title      TEXT    NOT NULL DEFAULT '',
    spine_href TEXT    NOT NULL DEFAULT '',
    char_len   INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_chapters_book ON chapters(book_id, idx);

CREATE TABLE chapter_text (
    chapter_id INTEGER PRIMARY KEY REFERENCES chapters(id) ON DELETE CASCADE,
    text       TEXT NOT NULL
);

CREATE TABLE positions (
    book_id      INTEGER PRIMARY KEY REFERENCES books(id) ON DELETE CASCADE,
    mode         TEXT    NOT NULL,
    locator_json TEXT    NOT NULL,
    updated_at   INTEGER NOT NULL
);

CREATE TABLE prefs (
    key        TEXT PRIMARY KEY,
    value_json TEXT NOT NULL
);

CREATE TABLE annotations (
    id          INTEGER PRIMARY KEY,
    book_id     INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    chapter_id  INTEGER REFERENCES chapters(id) ON DELETE CASCADE,
    anchor_json TEXT    NOT NULL,
    note        TEXT    NOT NULL DEFAULT '',
    color       TEXT    NOT NULL DEFAULT 'amber',
    stale       INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);
CREATE INDEX idx_annotations_book ON annotations(book_id, chapter_id);

CREATE TABLE collections (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
);

CREATE TABLE collection_books (
    collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    book_id       INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    added_at      INTEGER NOT NULL,
    PRIMARY KEY (collection_id, book_id)
);
"#,
    // v2：全文检索。中文必须用 trigram —— FTS5 默认的 unicode61 会把一整段
    // 没有空格的中文当成单个 token，中文查询永远命中不了。
    r#"
CREATE VIRTUAL TABLE chapter_fts USING fts5(
    text,
    content='chapter_text',
    content_rowid='chapter_id',
    tokenize='trigram case_sensitive 0'
);

CREATE TRIGGER chapter_fts_ai AFTER INSERT ON chapter_text BEGIN
    INSERT INTO chapter_fts(rowid, text) VALUES (new.chapter_id, new.text);
END;

CREATE TRIGGER chapter_fts_ad AFTER DELETE ON chapter_text BEGIN
    INSERT INTO chapter_fts(chapter_fts, rowid, text) VALUES ('delete', old.chapter_id, old.text);
END;

CREATE TRIGGER chapter_fts_au AFTER UPDATE ON chapter_text BEGIN
    INSERT INTO chapter_fts(chapter_fts, rowid, text) VALUES ('delete', old.chapter_id, old.text);
    INSERT INTO chapter_fts(rowid, text) VALUES (new.chapter_id, new.text);
END;
"#,
    // v3：EPUB 的文档级数据。CSS 与目录属于整本书，不属于任何一章，
    // 所以它们跟着 books 走而不是被塞进每份章节源文本里重复存一遍。
    r#"
ALTER TABLE books ADD COLUMN css TEXT NOT NULL DEFAULT '';
ALTER TABLE books ADD COLUMN toc_json TEXT NOT NULL DEFAULT '[]';
"#,
    // v4：整本书的图片资源。key 是解析后的容器绝对路径（与 chapter.spine_href 同形），
    // 排版与绘制都按它查表；固有尺寸在读取时算好，省得每次打开都重新解 header。
    r#"
CREATE TABLE images (
    id         INTEGER PRIMARY KEY,
    book_id    INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    href       TEXT    NOT NULL,
    media_type TEXT    NOT NULL DEFAULT '',
    width      INTEGER NOT NULL DEFAULT 0,
    height     INTEGER NOT NULL DEFAULT 0,
    bytes      BLOB    NOT NULL
);
CREATE INDEX idx_images_book ON images(book_id, href);
"#,
    // v5：图片字节不再入库，只留元数据（href / media_type / 宽高）。学 KOReader：
    // 原图随时能从容器里按 href 再拿一次，就不该在库里留第二份。
    //
    // 旧库的 `bytes` 在这里被丢掉。SQLite 的 `DROP COLUMN` 只改 schema、不还磁盘
    // （腾出的页给后续写入复用），所以库文件不会立即变小；真要在意体积，得手动
    // `VACUUM` —— 那是一次写全套，不在开书路径上做。
    r#"
ALTER TABLE images DROP COLUMN bytes;
"#,
    // v6：阅读统计。存**原始会话**而不是按天汇总 —— 「今天读了多久」依赖读者所在
    // 时区，而时区只有界面侧知道。core 只留事实（何时开始、多久、前进多少字），
    // 汇总时把偏移当参数收进来（见 [`crate::time`]），不把某个时区的日界线写死进表。
    r#"
CREATE TABLE reading_sessions (
    id         INTEGER PRIMARY KEY,
    book_id    INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    started_at INTEGER NOT NULL,
    ended_at   INTEGER NOT NULL,
    seconds    INTEGER NOT NULL,
    chars      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_sessions_book ON reading_sessions(book_id, started_at);
CREATE INDEX idx_sessions_time ON reading_sessions(started_at);
"#,
    // v7：定版式文档（漫画 / 将来的 PDF）。页表存成 JSON 一列 —— 页是一条线性的
    // 「一页接一页」，没有要按列查的东西，为它单建一张表只会多一层 join。
    // 图片字节照旧不入库（见 v5）：页表里的 href 只是去容器里现取的钥匙。
    r#"
ALTER TABLE books ADD COLUMN fixed_json TEXT NOT NULL DEFAULT '';
"#,
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookRow {
    pub id: i64,
    pub path: String,
    pub title: String,
    pub authors: Vec<String>,
    pub format: String,
    pub size: u64,
    pub last_opened_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub book_id: i64,
    pub chapter_id: i64,
    pub chapter_title: String,
    pub snippet: String,
}

/// 书架上的一张卡片：`BookRow` 那些项再加封面字节。
///
/// 单开一个类型而不是往 `BookRow` 上加 `cover`，是因为封面是这排里唯一大的东西
/// （一本几百 KB 到几 MB）。桌面端的侧栏把整个书库握在手里当标签页列表用，
/// 那个用途不需要封面字节，也就没必要为它把每本书的字节都读进内存。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShelfItem {
    pub id: i64,
    pub title: String,
    pub authors: Vec<String>,
    pub format: String,
    /// 封面原始字节，未解码。没封面（Markdown / TXT，或 EPUB 里没声明）时是 `None`，
    /// 界面要画一块占位而不是空的一格。
    pub cover: Option<Vec<u8>>,
    pub last_opened_at: Option<i64>,
}

/// 界面那四色高亮的合法取值。存的是**名字**而不是 ARGB：配色属于主题，
/// 把颜色值写死进标注，将来换主题就成了修不了的数据问题。
pub const ANNOTATION_COLORS: &[&str] = &["amber", "rose", "sky", "lime"];

/// 一条标注：坐标（含找回它所需的指纹）+ 读者写的东西。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    pub id: i64,
    pub book_id: i64,
    /// 章在 `FlowDoc::chapters` 里的下标。表里另有 `chapter_id` 外键，但那是索引与
    /// 级删用的；**坐标属于哪一章以这里为准**，因为它和锚点是一起写进去的。
    pub chapter: u32,
    pub anchor: TextAnchor,
    pub note: String,
    pub color: String,
    /// 锚点找不回来了。界面要显式标出来，而不是把批注画在猜出来的位置上。
    pub stale: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 一个集合（分组）。名字是用户输入的、跨书存在的东西，所以它跟着库走而不是配置。
///
/// `book_count` 是列表页要显示的那个数。带上它而不是让调用方为每个集合再查一次，
/// 是因为「N 本」本来就在这条 `LEFT JOIN ... GROUP BY` 里算得出来，多一次往返只是白费。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    pub id: i64,
    pub name: String,
    pub book_count: usize,
}

/// 一次阅读会话。时间都是 Unix 秒。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadingSession {
    pub id: i64,
    pub book_id: i64,
    pub started_at: i64,
    pub ended_at: i64,
    pub seconds: i64,
    /// 会话期间净前进的字符数。翻回前面会让它变小甚至变负 —— 它记的是「位置移动」，
    /// 不是「翻了多少」，所以显示时按 `max(0)` 的语义读。
    pub chars: i64,
}

/// 一本书的阅读累计。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BookReading {
    pub book_id: i64,
    pub seconds: i64,
    pub sessions: i64,
    pub chars: i64,
    /// 最后一次会话的结束时间。没读过是 `None`。
    pub last_read_at: Option<i64>,
}

/// 读过的书的合计。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReadingTotals {
    pub seconds: i64,
    pub sessions: i64,
    /// 至少读过一次的书有几本。
    pub books: i64,
    pub chars: i64,
}

/// 一天（读者本地日）的合计。`day` 是 `YYYY-MM-DD`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyReading {
    pub day: String,
    pub seconds: i64,
    pub chars: i64,
}

pub struct Store {
    conn: Connection,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn default_db_path() -> PathBuf {
    crate::paths::db_path()
}

fn book_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<BookRow> {
    let authors: String = r.get(3)?;
    Ok(BookRow {
        id: r.get(0)?,
        path: r.get(1)?,
        title: r.get(2)?,
        authors: authors
            .lines()
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect(),
        format: r.get(4)?,
        size: r.get::<_, i64>(5)? as u64,
        last_opened_at: r.get(6)?,
    })
}

const BOOK_COLUMNS: &str = "id, path, title, authors, format, size, last_opened_at";

/// 书架要的那几列：书目那些再加封面字节。
const SHELF_COLUMNS: &str = "id, path, title, authors, format, size, last_opened_at, cover";

fn shelf_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ShelfItem> {
    let authors: String = r.get(3)?;
    Ok(ShelfItem {
        id: r.get(0)?,
        title: r.get(2)?,
        authors: authors
            .lines()
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect(),
        format: r.get(4)?,
        last_opened_at: r.get(6)?,
        cover: r.get::<_, Option<Vec<u8>>>(7)?,
    })
}

impl Store {
    pub fn open_default() -> Result<Self> {
        Self::open_at(&default_db_path())
    }

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
        }
        Ok(Self { conn })
    }

    pub fn user_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    /// 同一路径重复导入是更新，不是第二本书。
    pub fn add_book(&self, path: &Path, meta: &DocMeta) -> Result<i64> {
        let path_s = path.to_string_lossy().into_owned();
        self.conn.execute(
            "INSERT INTO books (path, title, authors, format, size, added_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(path) DO UPDATE SET
               title = excluded.title, authors = excluded.authors,
               format = excluded.format, size = excluded.size",
            params![
                path_s,
                meta.title,
                meta.authors.join("\n"),
                meta.format.as_str(),
                meta.size as i64,
                now()
            ],
        )?;
        let id: i64 = self.conn.query_row(
            "SELECT id FROM books WHERE path = ?1",
            params![path_s],
            |r| r.get(0),
        )?;
        tracing::info!(
            "store: 第 {id} 本《{}》入库（{}，{} 字节）",
            meta.title,
            meta.format.as_str(),
            meta.size
        );
        Ok(id)
    }

    pub fn list_books(&self) -> Result<Vec<BookRow>> {
        let sql = format!(
            "SELECT {BOOK_COLUMNS} FROM books
             ORDER BY last_opened_at IS NULL, last_opened_at DESC, added_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        Ok(stmt
            .query_map([], book_from_row)?
            .collect::<std::result::Result<_, _>>()?)
    }

    /// 书架：整架或按**书名 / 作者**过滤，排序与 [`Store::list_books`] 同一条
    /// （最近在读在前，从没打开过的按导入时间倒序）。
    ///
    /// 空查询走 `?1 = ''` 那个短路分支而不是拼两条 SQL：一条语句两种绑定，
    /// 免得两处各写一遍排序而写歪。
    ///
    /// 这里**不**查正文。全文检索是另一件事（[`Store::search`]，trigram FTS，
    /// 短于 3 个字命中不了），而书架上那颗框在读者眼里是「找我那本书」，
    /// 输入两个字就该立刻列出书名 —— 把两者混在一个入口会让短查询静默返回空。
    pub fn shelf(&self, query: &str) -> Result<Vec<ShelfItem>> {
        let trimmed = query.trim();
        let sql = format!(
            r#"SELECT {SHELF_COLUMNS} FROM books
             WHERE (?1 = ''
                    OR title LIKE ?2 ESCAPE '\'
                    OR authors LIKE ?2 ESCAPE '\')
             ORDER BY last_opened_at IS NULL, last_opened_at DESC, added_at DESC"#
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let pattern = like_pattern(trimmed);
        Ok(stmt
            .query_map(params![trimmed, pattern], shelf_from_row)?
            .collect::<std::result::Result<_, _>>()?)
    }

    pub fn book(&self, id: i64) -> Result<Option<BookRow>> {
        let sql = format!("SELECT {BOOK_COLUMNS} FROM books WHERE id = ?1");
        Ok(self
            .conn
            .query_row(&sql, params![id], book_from_row)
            .optional()?)
    }

    pub fn book_by_path(&self, path: &Path) -> Result<Option<BookRow>> {
        let sql = format!("SELECT {BOOK_COLUMNS} FROM books WHERE path = ?1");
        let s = path.to_string_lossy().into_owned();
        Ok(self
            .conn
            .query_row(&sql, params![s], book_from_row)
            .optional()?)
    }

    pub fn remove_book(&self, id: i64) -> Result<()> {
        let rows = self
            .conn
            .execute("DELETE FROM books WHERE id = ?1", params![id])?;
        // 删掉 0 行只有两种可能：那本书本来就不在，或者调用方拿错了 id。
        // 两种都表现为「界面上按了删除但书还在」，所以这里要说一句。
        if rows == 0 {
            tracing::warn!("store: 删第 {id} 本，但没有这一行");
        } else {
            tracing::debug!("store: 删第 {id} 本（连带 {rows} 行外键）");
        }
        Ok(())
    }

    pub fn touch_opened(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE books SET last_opened_at = ?2 WHERE id = ?1",
            params![id, now()],
        )?;
        Ok(())
    }

    /// 整本替换章节、CSS、目录、封面与图片，一个事务保证不留半本。
    pub fn save_flow(&self, book_id: i64, doc: &FlowDoc) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM chapters WHERE book_id = ?1", params![book_id])?;
        tx.execute("DELETE FROM images WHERE book_id = ?1", params![book_id])?;
        for (idx, ch) in doc.chapters.iter().enumerate() {
            tx.execute(
                "INSERT INTO chapters (book_id, idx, title, spine_href, char_len)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    book_id,
                    idx as i64,
                    ch.title,
                    ch.spine_href,
                    ch.char_len() as i64
                ],
            )?;
            let chapter_id = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO chapter_text (chapter_id, text) VALUES (?1, ?2)",
                params![chapter_id, ch.source],
            )?;
        }
        for img in &doc.images {
            tx.execute(
                "INSERT INTO images (book_id, href, media_type, width, height)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    book_id,
                    img.href,
                    img.media_type,
                    img.width as i64,
                    img.height as i64
                ],
            )?;
        }
        tx.execute(
            "UPDATE books SET css = ?2, toc_json = ?3, cover = ?4 WHERE id = ?1",
            params![
                book_id,
                doc.css,
                serde_json::to_string(&doc.toc)?,
                doc.cover
            ],
        )?;
        tx.commit()?;
        tracing::debug!(
            "store: 第 {book_id} 本落库 {} 章、{} 张图、css {} 字节、目录 {} 项",
            doc.chapters.len(),
            doc.images.len(),
            doc.css.len(),
            doc.toc.len()
        );
        Ok(())
    }

    pub fn load_flow(&self, book_id: i64) -> Result<Option<FlowDoc>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.title, c.spine_href, t.text
             FROM chapters c JOIN chapter_text t ON t.chapter_id = c.id
             WHERE c.book_id = ?1 ORDER BY c.idx",
        )?;
        let chapters = stmt
            .query_map(params![book_id], |r| {
                Ok(Chapter {
                    title: r.get(0)?,
                    spine_href: r.get(1)?,
                    source: r.get(2)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if chapters.is_empty() {
            tracing::debug!("store: 第 {book_id} 本没有入库的章节，回 None");
            return Ok(None);
        }
        let (css, toc_json, cover) = self.conn.query_row(
            "SELECT css, toc_json, cover FROM books WHERE id = ?1",
            params![book_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<Vec<u8>>>(2)?,
                ))
            },
        )?;
        let toc = match serde_json::from_str(&toc_json) {
            Ok(toc) => toc,
            Err(err) => {
                // 目录读不出来只是没有目录，正文照读 —— 但这一句必须留下，
                // 否则「这本书没有目录」将来查起来和「它本来就没目录」完全一样。
                tracing::warn!("store: 第 {book_id} 本的 toc_json 解不开，当没有目录：{err}");
                Vec::new()
            }
        };
        let mut img_stmt = self.conn.prepare(
            "SELECT href, media_type, width, height FROM images
             WHERE book_id = ?1 ORDER BY id",
        )?;
        let images = img_stmt
            .query_map(params![book_id], |r| {
                Ok(ImageRes {
                    href: r.get(0)?,
                    media_type: r.get(1)?,
                    width: r.get::<_, i64>(2)? as u32,
                    height: r.get::<_, i64>(3)? as u32,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(Some(FlowDoc {
            chapters,
            css,
            toc,
            cover,
            images,
        }))
    }

    pub fn set_position(&self, book_id: i64, mode: &str, locator: &Locator) -> Result<()> {
        self.conn.execute(
            "INSERT INTO positions (book_id, mode, locator_json, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(book_id) DO UPDATE SET
               mode = excluded.mode, locator_json = excluded.locator_json,
               updated_at = excluded.updated_at",
            params![book_id, mode, locator.to_json(), now()],
        )?;
        Ok(())
    }

    pub fn position(&self, book_id: i64) -> Result<Option<(String, Locator)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT mode, locator_json FROM positions WHERE book_id = ?1",
                params![book_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
            .map(|(mode, json)| (mode, Locator::from_json(&json))))
    }

    /// trigram 分词要求查询串至少 3 个字符，短于 3 直接返回空而不是假装能搜。
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        if query.chars().count() < 3 {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT c.book_id, c.id, c.title,
                    snippet(chapter_fts, 0, '[', ']', '…', 12)
             FROM chapter_fts JOIN chapters c ON c.id = chapter_fts.rowid
             WHERE chapter_fts MATCH ?1 LIMIT ?2",
        )?;
        let quoted = fts_phrase(query);
        let hits = stmt
            .query_map(params![quoted, limit as i64], |r| {
                Ok(SearchHit {
                    book_id: r.get(0)?,
                    chapter_id: r.get(1)?,
                    chapter_title: r.get(2)?,
                    snippet: r.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<SearchHit>, rusqlite::Error>>()?;
        // 0 条命中有两种成因（trigram 的 3 字下限、FTS 把输入当短语），界面上看不出是哪种。
        tracing::debug!(
            "store: 全文搜 {:?} 命中 {} 条（上限 {limit}）",
            query,
            hits.len()
        );
        Ok(hits)
    }

    /// 记一条标注。`anchor` 是调用方从可见文本坐标建好的（见 `crate::anchor`），
    /// 存储层不参与坐标换算 —— 它不知道排版，也不该知道。
    ///
    /// 颜色不认识时回落到默认那条，而不是拒写：写失败会让读者刚划的批注直接丢掉，
    /// 那比颜色不对严重得多。
    pub fn add_annotation(
        &self,
        book_id: i64,
        anchor: &TextAnchor,
        note: &str,
        color: &str,
    ) -> Result<i64> {
        // `chapter_id` 只是索引与级删的挂钩，坐标里的章下标才是权威。
        // 书没重新入库时这一列可能没有对应行，那就存 NULL —— 不能因为外键查不到
        // 就把批注写失败。
        let chapter_id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM chapters WHERE book_id = ?1 AND idx = ?2",
                params![book_id, anchor.chapter as i64],
                |r| r.get(0),
            )
            .optional()?;
        let stamp = now();
        self.conn.execute(
            "INSERT INTO annotations (book_id, chapter_id, anchor_json, note, color, stale, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?6)",
            params![book_id, chapter_id, anchor.to_json(), note, color_of(color), stamp],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// 一本书的全部标注，按创建顺序。
    ///
    /// **解不出锚点的行会被跳过**而不是让整个列表读失败：这一列可能来自旧版本或
    /// 别的机器，为一块坏 JSON 让读者看不到他所有的批注，是最糟的失败方式。
    pub fn annotations(&self, book_id: i64) -> Result<Vec<Annotation>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, book_id, anchor_json, note, color, stale, created_at, updated_at
             FROM annotations WHERE book_id = ?1 ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map(params![book_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)? != 0,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, book, json, note, color, stale, created_at, updated_at) = row?;
            let Some(anchor) = TextAnchor::from_json(&json) else {
                continue;
            };
            out.push(Annotation {
                id,
                book_id: book,
                chapter: anchor.chapter,
                anchor,
                note,
                color,
                stale,
                created_at,
                updated_at,
            });
        }
        Ok(out)
    }

    pub fn annotation(&self, id: i64) -> Result<Option<Annotation>> {
        let row: Option<(String, i64, i64, i64, i64, String, String)> = self
            .conn
            .query_row(
                "SELECT anchor_json, book_id, stale, created_at, updated_at, note, color
                 FROM annotations WHERE id = ?1",
                params![id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get::<_, i64>(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .optional()?;
        Ok(row.and_then(
            |(json, book_id, stale, created_at, updated_at, note, color)| {
                TextAnchor::from_json(&json).map(|anchor| Annotation {
                    id,
                    book_id,
                    chapter: anchor.chapter,
                    anchor,
                    note,
                    color,
                    stale: stale != 0,
                    created_at,
                    updated_at,
                })
            },
        ))
    }

    /// 改批注文字与颜色。`updated_at` 跟着走 —— 界面的「最近改动」排序用它。
    pub fn update_annotation(&self, id: i64, note: &str, color: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE annotations SET note = ?2, color = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, note, color_of(color), now()],
        )?;
        Ok(())
    }

    /// 记下「锚点找不回来了」。这**不**碰 `updated_at`：stale 是排版结果的属性，
    /// 不是读者编辑过的证据，跟着它排序会把从没动过的批注顶到列表最前。
    pub fn set_annotation_stale(&self, id: i64, stale: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE annotations SET stale = ?2 WHERE id = ?1",
            params![id, stale as i64],
        )?;
        Ok(())
    }

    pub fn delete_annotation(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM annotations WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// 新建一个集合；**同名即复用**（`UNIQUE` 上的 `DO NOTHING` 再回查 id）。
    ///
    /// 做成幂等而不是报「名字已存在」，是照 [`Store::add_book`] 的同一套判据：
    /// 集合在用户眼里是文件夹，重复点「新建」时他要的是「进到那个文件夹」，
    /// 不是一条报错。真要拦重名，那是界面在提交前该提示的事。
    pub fn create_collection(&self, name: &str) -> Result<i64> {
        let name = name.trim();
        if name.is_empty() {
            return Err(crate::Error::EmptyCollectionName);
        }
        self.conn.execute(
            "INSERT INTO collections (name) VALUES (?1) ON CONFLICT(name) DO NOTHING",
            params![name],
        )?;
        let id: i64 = self.conn.query_row(
            "SELECT id FROM collections WHERE name = ?1",
            params![name],
            |r| r.get(0),
        )?;
        tracing::debug!("store: 集合「{name}」→ id {id}");
        Ok(id)
    }

    /// 全部集合，按名字排序，每条带上书数。
    pub fn collections(&self) -> Result<Vec<Collection>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.name, count(cb.book_id)
             FROM collections c
             LEFT JOIN collection_books cb ON cb.collection_id = c.id
             GROUP BY c.id, c.name
             ORDER BY c.name, c.id",
        )?;
        Ok(stmt
            .query_map([], collection_from_row)?
            .collect::<std::result::Result<_, _>>()?)
    }

    pub fn collection(&self, id: i64) -> Result<Option<Collection>> {
        Ok(self
            .conn
            .query_row(
                "SELECT c.id, c.name, count(cb.book_id)
                 FROM collections c
                 LEFT JOIN collection_books cb ON cb.collection_id = c.id
                 WHERE c.id = ?1
                 GROUP BY c.id, c.name",
                params![id],
                collection_from_row,
            )
            .optional()?)
    }

    /// 改名。撞上别的集合的名字会让 `UNIQUE` 报错（不是静默合并）——
    /// 改名是用户明确说的「从此以后叫这个」，替他并进另一个集合属于乱猜。
    pub fn rename_collection(&self, id: i64, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(crate::Error::EmptyCollectionName);
        }
        let rows = self.conn.execute(
            "UPDATE collections SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        if rows == 0 {
            tracing::warn!("store: 改集合 {id} 的名字，但没有这一行");
        }
        Ok(())
    }

    /// 删集合。只断链接，里头的书一本都不动（`collection_books` 上那条级联）。
    pub fn delete_collection(&self, id: i64) -> Result<()> {
        let rows = self
            .conn
            .execute("DELETE FROM collections WHERE id = ?1", params![id])?;
        if rows == 0 {
            tracing::warn!("store: 删集合 {id}，但没有这一行");
        } else {
            tracing::debug!("store: 删集合 {id}");
        }
        Ok(())
    }

    /// 把一本书放进集合。返回「这次真的放进去没有」——重复调用是 `false`，不是错。
    pub fn add_to_collection(&self, collection_id: i64, book_id: i64) -> Result<bool> {
        let rows = self.conn.execute(
            "INSERT OR IGNORE INTO collection_books (collection_id, book_id, added_at)
             VALUES (?1, ?2, ?3)",
            params![collection_id, book_id, now()],
        )?;
        Ok(rows > 0)
    }

    /// 把一本书移出集合。返回「这次真的移出了没有」。
    pub fn remove_from_collection(&self, collection_id: i64, book_id: i64) -> Result<bool> {
        let rows = self.conn.execute(
            "DELETE FROM collection_books WHERE collection_id = ?1 AND book_id = ?2",
            params![collection_id, book_id],
        )?;
        Ok(rows > 0)
    }

    /// 一本书属于哪几个集合，按名字排序。书详情页要显示「在哪些分组里」。
    pub fn collections_of_book(&self, book_id: i64) -> Result<Vec<Collection>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.name,
                    (SELECT count(*) FROM collection_books x WHERE x.collection_id = c.id)
             FROM collections c
             JOIN collection_books cb ON cb.collection_id = c.id
             WHERE cb.book_id = ?1
             ORDER BY c.name, c.id",
        )?;
        Ok(stmt
            .query_map(params![book_id], collection_from_row)?
            .collect::<std::result::Result<_, _>>()?)
    }

    /// 集合里的书，最近放进去的在前（同一秒靠 id 定序，保证稳定）。
    /// 排序与 [`Store::list_books`] 那条「最近在读」无关：这里的主语是集合，
    /// 读者刚往这个文件夹里丢的那本应当先看见。
    pub fn books_in_collection(&self, collection_id: i64) -> Result<Vec<BookRow>> {
        let sql = format!(
            "SELECT {BOOK_COLUMNS} FROM books
             JOIN collection_books cb ON cb.book_id = books.id
             WHERE cb.collection_id = ?1
             ORDER BY cb.added_at DESC, books.id DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        Ok(stmt
            .query_map(params![collection_id], book_from_row)?
            .collect::<std::result::Result<_, _>>()?)
    }

    /// 记一次阅读会话。`seconds` 由起止时间算，负数夹到 0 —— 调用方给起止就够了，
    /// 两处各报一个「时长」迟早会对不上。
    pub fn record_session(
        &self,
        book_id: i64,
        started_at: i64,
        ended_at: i64,
        chars: i64,
    ) -> Result<i64> {
        let seconds = (ended_at - started_at).max(0);
        self.conn.execute(
            "INSERT INTO reading_sessions (book_id, started_at, ended_at, seconds, chars)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![book_id, started_at, ended_at, seconds, chars],
        )?;
        tracing::debug!("store: 第 {book_id} 本记了 {seconds} 秒阅读（{chars:+} 字）");
        Ok(self.conn.last_insert_rowid())
    }

    /// 一本书的会话，最近的在先。
    pub fn sessions(&self, book_id: i64, limit: usize) -> Result<Vec<ReadingSession>> {
        let mut stmt = self.conn.prepare(
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
        let (seconds, sessions, books, chars) = self.conn.query_row(
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
        let (seconds, sessions, chars, last_read_at) = self.conn.query_row(
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
        if days == 0 {
            return Ok(Vec::new());
        }
        let today = local_day(now(), tz_offset_secs);
        let first = today - days as i64 + 1;
        let mut stmt = self.conn.prepare(
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

    /// 书架分组：先按 `query` 过滤，再按 `by` 分。
    ///
    /// 分组本身是纯函数（[`crate::group::group_shelf`]），这里只负责先取书架。
    /// 集合那种「组是读者建的、存在库里」的情形不走这里，见
    /// [`Store::collection_groups`]。
    pub fn shelf_grouped(&self, query: &str, by: GroupBy) -> Result<Vec<ShelfGroup>> {
        Ok(group_shelf(self.shelf(query)?, by))
    }

    /// 每个集合一组，末尾追一个「未分组」（装了但还没进任何集合的书）。
    ///
    /// **空集合也留着**：读者刚建的空组如果在分组视图里消失，他会以为没建成。
    /// 组名用 [`UNGROUPED`] 而不是空串，理由见那里的说明。
    pub fn collection_groups(&self) -> Result<Vec<ShelfGroup>> {
        let mut groups = Vec::new();
        for c in self.collections()? {
            groups.push(ShelfGroup {
                name: c.name,
                items: self.shelf_in_collection(c.id)?,
            });
        }
        let loose = self.shelf_in_no_collection()?;
        if !loose.is_empty() {
            groups.push(ShelfGroup {
                name: UNGROUPED.to_string(),
                items: loose,
            });
        }
        Ok(groups)
    }

    /// 一个集合里的书，最近放进去的在先 —— 与 [`Store::books_in_collection`] 同一条排序，
    /// 只是这里带着封面字节，给分组视图直接画卡片用。
    pub fn shelf_in_collection(&self, collection_id: i64) -> Result<Vec<ShelfItem>> {
        let sql = format!(
            "SELECT {SHELF_COLUMNS} FROM books
             JOIN collection_books cb ON cb.book_id = books.id
             WHERE cb.collection_id = ?1
             ORDER BY cb.added_at DESC, books.id DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        Ok(stmt
            .query_map(params![collection_id], shelf_from_row)?
            .collect::<std::result::Result<_, _>>()?)
    }

    /// 还没进任何集合的书，排序与 [`Store::shelf`] 同一条。
    fn shelf_in_no_collection(&self) -> Result<Vec<ShelfItem>> {
        let sql = format!(
            "SELECT {SHELF_COLUMNS} FROM books
             WHERE id NOT IN (SELECT book_id FROM collection_books)
             ORDER BY last_opened_at IS NULL, last_opened_at DESC, added_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        Ok(stmt
            .query_map([], shelf_from_row)?
            .collect::<std::result::Result<_, _>>()?)
    }

    /// 存一本定版式文档。与 [`Store::save_flow`] 对称：封面与目录跟着一起写。
    pub fn save_fixed(&self, book_id: i64, doc: &FixedDoc) -> Result<()> {
        let pages = serde_json::to_string(&doc.pages)?;
        self.conn.execute(
            "UPDATE books SET fixed_json = ?2, toc_json = ?3, cover = ?4 WHERE id = ?1",
            params![book_id, pages, serde_json::to_string(&doc.toc)?, doc.cover],
        )?;
        tracing::debug!(
            "store: 第 {book_id} 本落库 {} 页（定版式）",
            doc.pages.len()
        );
        Ok(())
    }

    /// 读一本定版式文档。没存过、或存的是可重排时返回 `None`。
    ///
    /// 坏 JSON 一律当「没有」而不是让整本书打不开：这一列可能来自以后的版本。
    /// 图片表由页表**重建**（href + 尺寸），不单独存一遍。
    pub fn load_fixed(&self, book_id: i64) -> Result<Option<FixedDoc>> {
        let row: Option<(String, String, Option<Vec<u8>>)> = self
            .conn
            .query_row(
                "SELECT fixed_json, toc_json, cover FROM books WHERE id = ?1",
                params![book_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((pages_json, toc_json, cover)) = row else {
            return Ok(None);
        };
        if pages_json.trim().is_empty() {
            return Ok(None);
        }
        let pages: Vec<FixedPage> = match serde_json::from_str(&pages_json) {
            Ok(pages) => pages,
            Err(err) => {
                tracing::warn!("store: 第 {book_id} 本的 fixed_json 解不开，当没有：{err}");
                return Ok(None);
            }
        };
        let toc = serde_json::from_str(&toc_json).unwrap_or_default();
        let (width, height) = pages.first().map_or((0, 0), |p| (p.width, p.height));
        let images = pages.iter().filter_map(page_image).collect();
        Ok(Some(FixedDoc {
            pages,
            toc,
            cover,
            images,
            width,
            height,
        }))
    }

    /// 按内容家族存。界面拿到 [`Content`] 直接丢进来，不必自己分流。
    pub fn save_content(&self, book_id: i64, content: &Content) -> Result<()> {
        match content {
            Content::Reflow(flow) => self.save_flow(book_id, flow),
            Content::Fixed(fixed) => self.save_fixed(book_id, fixed),
        }
    }

    /// 把一份已解析的文档落库。
    ///
    /// 与 [`Self::save_content`] 同一件事，只是入口收在「文档」而不是「内容」：
    /// 导入那一步手上只有一份 `dyn Document`（`document::open` 的产物），
    /// 让它自己 match 家族等于把 `Content` 的形状又散给调用方一次。
    pub fn save_document(&self, book_id: i64, doc: &dyn crate::document::Document) -> Result<()> {
        match doc.content() {
            crate::document::ContentRef::Reflow(flow) => self.save_flow(book_id, flow),
            crate::document::ContentRef::Fixed(fixed) => self.save_fixed(book_id, fixed),
        }
    }

    /// 读一本已入库的书。先试可重排、再试定版式 —— 两边的「有没有」互斥
    /// （有章节的必定是可重排），不会歧义。
    pub fn load_content(&self, book_id: i64) -> Result<Option<Content>> {
        if let Some(flow) = self.load_flow(book_id)? {
            return Ok(Some(Content::Reflow(flow)));
        }
        Ok(self.load_fixed(book_id)?.map(Content::Fixed))
    }
}

/// 定版式的一页 → 图片元数据。渲染型（PDF）没有 href，不进图片表。
fn page_image(page: &FixedPage) -> Option<ImageRes> {
    let href = page.image_href()?.to_string();
    let ext = Path::new(&href)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    Some(ImageRes {
        href,
        media_type: crate::format::media_type_of(&ext).to_string(),
        width: page.width,
        height: page.height,
    })
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

fn collection_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Collection> {
    Ok(Collection {
        id: r.get(0)?,
        name: r.get(1)?,
        book_count: r.get::<_, i64>(2)? as usize,
    })
}

/// 颜色的规范化：小写、去空白，不在清单里的用默认值。
fn color_of(input: &str) -> String {
    let key = input.trim().to_ascii_lowercase();
    if ANNOTATION_COLORS.contains(&key.as_str()) {
        key
    } else {
        ANNOTATION_COLORS[0].to_string()
    }
}

/// 把用户输入包成 FTS5 短语，避免引号与括号被当查询语法解析。
fn fts_phrase(query: &str) -> String {
    format!("\"{}\"", query.replace('"', "\"\""))
}

/// 用户输入 → `LIKE` 的匹配串。
///
/// 三个都要转：`%` 和 `_` 是通配符，读者书名里真会有（`100%` 那种营销标题、
/// 下划线分隔的文件名），不转的话他搜 `100%` 结果把整个书架都列出来。
/// 转义符本身先转，否则 `a\` 会被转成 `a\\` 而后面的 `%` 就成了通配。
/// SQL 侧必须配 `ESCAPE '\'`，那一条见 [`Store::shelf`]。
fn like_pattern(query: &str) -> String {
    let mut out = String::with_capacity(query.len() + 2);
    out.push('%');
    for c in query.chars() {
        match c {
            '\\' | '%' | '_' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push('%');
    out
}

#[cfg(test)]
mod tests {
    //! 存储层：迁移、书库、章节/图片/进度/偏好的往返与级联。只依赖公开 API。
    //!
    //! 级联那条测试要看「牌面之下的表」，但绝不碰私有字段：用公开的 `Store::open_at`
    //! 建库，再用一条裸 `rusqlite` 连接按**库文件契约**查计数。

    use std::path::{Path, PathBuf};

    use crate::Store;
    use crate::format::{Chapter, DocMeta, FlowDoc, Format, ImageRes, TocEntry, guess_media_type};
    use crate::position::Locator;
    use crate::store::MIGRATIONS;

    fn meta(title: &str) -> DocMeta {
        DocMeta {
            title: title.into(),
            authors: vec!["作者甲".into(), "作者乙".into()],
            format: Format::Markdown,
            size: 1234,
        }
    }

    fn path(name: &str) -> PathBuf {
        Path::new("/books").join(name)
    }

    fn flow(parts: &[(&str, &str)]) -> FlowDoc {
        FlowDoc {
            chapters: parts
                .iter()
                .enumerate()
                .map(|(i, (title, source))| Chapter {
                    title: (*title).into(),
                    source: (*source).into(),
                    spine_href: format!("#{i}"),
                })
                .collect(),
            // 文档级三项一起进往返测试：EPUB 的 CSS / 目录 / 封面走的是 books 表的列。
            css: "p { text-indent: 2em }".into(),
            toc: vec![TocEntry {
                title: parts.first().map(|t| t.0.to_string()).unwrap_or_default(),
                href: "#0".into(),
                fragment: Some("sec1".into()),
                children: Vec::new(),
            }],
            cover: Some(vec![0x89, b'P', b'N', b'G', 13, 10]),
            // 一张图片一起进往返：EPUB 的内嵌图走 images 表。
            images: vec![ImageRes {
                href: "OEBPS/Images/fig1.png".into(),
                media_type: "image/png".into(),
                width: 640,
                height: 480,
            }],
        }
    }

    #[test]
    fn migrations_are_idempotent_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("folio.db");

        let store = Store::open_at(&db).unwrap();
        assert_eq!(store.user_version().unwrap(), MIGRATIONS.len() as i64);
        let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
        store.save_flow(id, &flow(&[("第一章", "正文一")])).unwrap();
        drop(store);

        let again = Store::open_at(&db).unwrap();
        assert_eq!(
            again.user_version().unwrap(),
            MIGRATIONS.len() as i64,
            "重开不应重复跑迁移"
        );
        assert_eq!(again.list_books().unwrap().len(), 1);
        assert_eq!(
            again.load_flow(id).unwrap().unwrap().chapters[0].title,
            "第一章"
        );
    }

    /// v5 迁移：旧库（v4，`images.bytes` 还在）升上来时，字节要被丢掉、元数据留下。
    ///
    /// 这条不是形式主义 —— 已经装过的 `folio.db` 就是 v4 形状，
    /// `ALTER TABLE ... DROP COLUMN` 在它上面成功与否，决定老用户开不开得了书。
    #[test]
    fn the_v5_migration_drops_image_bytes_from_an_old_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("folio.db");
        // 手工造一个 v4 库：跑前四条迁移（到 v4 为止），塞一张带字节的图，
        // 然后把版本号停在 4。
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            for sql in &MIGRATIONS[..4] {
                conn.execute_batch(sql).unwrap();
            }
            conn.pragma_update(None, "user_version", 4).unwrap();
            conn.execute(
                "INSERT INTO books (path, title, authors, format, size, added_at)
                 VALUES ('a.epub', 'A', '', 'epub', 0, 0)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO images (book_id, href, media_type, width, height, bytes)
                 VALUES (1, 'OEBPS/i.png', 'image/png', 3, 2, x'89504E47')",
                [],
            )
            .unwrap();
        }

        let store = Store::open_at(&db).unwrap();
        assert_eq!(store.user_version().unwrap(), MIGRATIONS.len() as i64);
        drop(store);

        let conn = rusqlite::Connection::open(&db).unwrap();
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(images)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert!(
            !cols.iter().any(|c| c == "bytes"),
            "v5 之后 images 表不该还有 bytes 列：{cols:?}"
        );
        let (href, w, h): (String, i64, i64) = conn
            .query_row(
                "SELECT href, width, height FROM images WHERE book_id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((href.as_str(), w, h), ("OEBPS/i.png", 3, 2), "元数据要留下");
    }

    #[test]
    fn adding_the_same_path_twice_updates_in_place() {
        let store = Store::in_memory().unwrap();
        let first = store.add_book(&path("a.md"), &meta("旧标题")).unwrap();
        let second = store.add_book(&path("a.md"), &meta("新标题")).unwrap();
        assert_eq!(first, second, "同一路径不该变成两本书");
        let books = store.list_books().unwrap();
        assert_eq!(books.len(), 1);
        assert_eq!(books[0].title, "新标题");
        assert_eq!(books[0].authors, vec!["作者甲", "作者乙"]);
    }

    #[test]
    fn flow_roundtrips_byte_for_byte() {
        let store = Store::in_memory().unwrap();
        let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
        let doc = flow(&[
            ("前言", "# A\n中文前言，带标点。\n"),
            ("第一章", "## 第一章\n「引号」和 emoji 😀 也要原样回来。\n"),
        ]);
        store.save_flow(id, &doc).unwrap();
        assert_eq!(store.load_flow(id).unwrap().unwrap(), doc);
    }

    /// 图片的元数据必须原样回来（字节不入库，见 v5 迁移），且重存时是替换而非追加。
    #[test]
    fn images_roundtrip_and_replace() {
        let store = Store::in_memory().unwrap();
        let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
        let mut doc = flow(&[("一", "正文")]);
        store.save_flow(id, &doc).unwrap();

        let got = store.load_flow(id).unwrap().unwrap();
        assert_eq!(got.images.len(), 1);
        assert_eq!(got.images[0].href, "OEBPS/Images/fig1.png");
        assert_eq!(got.images[0].media_type, "image/png");
        assert_eq!((got.images[0].width, got.images[0].height), (640, 480));

        doc.images.clear();
        store.save_flow(id, &doc).unwrap();
        assert!(
            store.load_flow(id).unwrap().unwrap().images.is_empty(),
            "重存应替换旧图片，而不是留下孤行"
        );
    }

    #[test]
    fn saving_a_flow_twice_replaces_rather_than_appends() {
        let store = Store::in_memory().unwrap();
        let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
        store
            .save_flow(id, &flow(&[("一", "a"), ("二", "b")]))
            .unwrap();
        store.save_flow(id, &flow(&[("新", "c")])).unwrap();
        let got = store.load_flow(id).unwrap().unwrap();
        assert_eq!(got.chapters.len(), 1);
        assert_eq!(got.chapters[0].title, "新");
    }

    #[test]
    fn unknown_book_has_no_flow() {
        let store = Store::in_memory().unwrap();
        assert_eq!(store.load_flow(4242).unwrap(), None);
    }

    #[test]
    fn position_roundtrips_and_overwrites() {
        let store = Store::in_memory().unwrap();
        let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
        assert_eq!(store.position(id).unwrap(), None);

        let loc = Locator {
            chapter: 3,
            offset: 1024,
        };
        store.set_position(id, "continuous", &loc).unwrap();
        let (mode, got) = store.position(id).unwrap().unwrap();
        assert_eq!(mode, "continuous");
        assert_eq!(got, loc);

        let moved = Locator {
            chapter: 4,
            offset: 8,
        };
        store.set_position(id, "paged", &moved).unwrap();
        let (mode, got) = store.position(id).unwrap().unwrap();
        assert_eq!(mode, "paged");
        assert_eq!(got, moved, "同一本书只应有一份进度");
    }

    /// 删书要把章节、图片、进度一起带走。表之间是外键级联，所以按**库文件里的表**验，
    /// 不通过私有连接字段。
    #[test]
    fn removing_a_book_cascades_its_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("folio.db");
        {
            let store = Store::open_at(&db).unwrap();
            let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
            store.save_flow(id, &flow(&[("一", "内容一")])).unwrap();
            store
                .set_position(id, "continuous", &Locator::start())
                .unwrap();

            store.remove_book(id).unwrap();
            assert_eq!(store.book(id).unwrap(), None);
            assert!(store.load_flow(id).unwrap().is_none(), "章节随书一起消失");
            assert!(store.position(id).unwrap().is_none(), "进度随书一起消失");
        }

        let conn = rusqlite::Connection::open(&db).unwrap();
        let count = |table: &str| -> i64 {
            conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(count("chapters"), 0, "章节必须随书一起消失");
        assert_eq!(count("images"), 0, "图片必须随书一起消失");
    }

    #[test]
    fn chinese_fulltext_search_works_via_trigram() {
        let store = Store::in_memory().unwrap();
        let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
        store
            .save_flow(
                id,
                &flow(&[
                    ("第一章", "于是我们奋力向前，逆水行舟。"),
                    ("第二章", "另一段毫无关系的文字。"),
                ]),
            )
            .unwrap();

        let hits = store.search("逆水行舟", 10).unwrap();
        assert_eq!(hits.len(), 1, "只应命中第一章");
        assert_eq!(hits[0].chapter_title, "第一章");
        assert!(
            hits[0].snippet.contains("逆水行舟"),
            "snippet={}",
            hits[0].snippet
        );

        assert!(
            store.search("逆水", 10).unwrap().is_empty(),
            "trigram 需要 3 个字符"
        );
        assert!(store.search("不存在的词", 10).unwrap().is_empty());
    }

    #[test]
    fn search_index_follows_chapter_replacement() {
        let store = Store::in_memory().unwrap();
        let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
        store.save_flow(id, &flow(&[("一", "逆水行舟")])).unwrap();
        assert_eq!(store.search("逆水行舟", 10).unwrap().len(), 1);

        store
            .save_flow(id, &flow(&[("一", "完全不同的内容")]))
            .unwrap();
        assert!(
            store.search("逆水行舟", 10).unwrap().is_empty(),
            "重写章节后旧命中不该还在"
        );
        assert_eq!(store.search("完全不同的", 10).unwrap().len(), 1);
    }

    #[test]
    fn fts_query_syntax_in_user_input_is_treated_as_text() {
        let store = Store::in_memory().unwrap();
        let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
        store
            .save_flow(id, &flow(&[("一", "这里有个 AND 关键字和引号 \"x\"")]))
            .unwrap();
        let hits = store.search("AND", 10).unwrap();
        assert_eq!(hits.len(), 1, "裸 AND 不该被当成布尔运算符");
        assert!(store.search("NEAR/a", 10).unwrap().is_empty());
    }

    /// 打开过的书排在从没打开过的前面。
    #[test]
    fn opened_books_rank_before_never_opened_ones() {
        let store = Store::in_memory().unwrap();
        let a = store.add_book(&path("a.md"), &meta("A")).unwrap();
        let _b = store.add_book(&path("b.md"), &meta("B")).unwrap();
        store.touch_opened(a).unwrap();
        let books = store.list_books().unwrap();
        assert_eq!(books.len(), 2);
        assert_eq!(books[0].title, "A", "打开过的排最前，没打开过的垫后");
    }

    /// 书架的一条：封面字节跟着书目一起回来，排序与 `list_books` 同一条规则。
    ///
    /// 封面存在 `books.cover`，但只有 `save_flow` 会写它，所以这条测试必须走
    /// `add_book` + `save_flow` 两步，而不是只看 `add_book`。
    #[test]
    fn shelf_returns_cover_bytes_and_ranks_like_list_books() {
        let store = Store::in_memory().unwrap();
        let a = store.add_book(&path("a.epub"), &meta("A")).unwrap();
        let mut flow_a = flow(&[("一", "正文")]);
        flow_a.cover = Some(png_header());
        store.save_flow(a, &flow_a).unwrap();
        let b = store.add_book(&path("b.md"), &meta("B")).unwrap();
        store.save_flow(b, &flow(&[("一", "正文")])).unwrap();

        store.touch_opened(b).unwrap();
        let shelf = store.shelf("").unwrap();
        assert_eq!(shelf.len(), 2, "空查询就是整架");
        assert_eq!(shelf[0].title, "B", "打开过的排最前");
        assert_eq!(shelf[1].title, "A");
    }

    /// `BookRow` 那些项之外的两列都得对：`cover` 是卡片的主视觉，`last_opened_at`
    /// 决定「继续读」那格的措辞。
    #[test]
    fn shelf_carries_the_cover_and_the_opened_stamp() {
        let store = Store::in_memory().unwrap();
        let id = store.add_book(&path("a.epub"), &meta("蛇结")).unwrap();
        let mut flow_a = flow(&[("一", "正文")]);
        flow_a.cover = Some(png_header());
        store.save_flow(id, &flow_a).unwrap();

        let item = &store.shelf("蛇结").unwrap()[0];
        assert_eq!(item.id, id);
        assert_eq!(
            item.authors,
            vec!["作者甲".to_string(), "作者乙".to_string()]
        );
        assert_eq!(item.format, "markdown");
        assert_eq!(item.cover.as_deref(), Some(png_header().as_slice()));
        assert_eq!(item.last_opened_at, None, "没打开过就是 NULL，不是 0");
    }

    /// 没封面的书也得进架（Markdown / TXT 恒如此），否则「导入了一本但看不到」
    /// 就成了没法和「导入失败」区分开的问题。
    #[test]
    fn shelf_includes_books_without_a_cover() {
        let store = Store::in_memory().unwrap();
        let id = store.add_book(&path("a.md"), &meta("无封面")).unwrap();
        // `flow()` 那个公共夹具带着一份假封面，这里要的是真·没封面的一本。
        let mut no_cover = flow(&[("一", "正文")]);
        no_cover.cover = None;
        store.save_flow(id, &no_cover).unwrap();
        let shelf = store.shelf("").unwrap();
        assert_eq!(shelf.len(), 1);
        assert_eq!(shelf[0].cover, None);
    }

    #[test]
    fn shelf_filters_by_title_or_author() {
        let store = Store::in_memory().unwrap();
        for (name, title, author) in [
            ("a.epub", "蛇结", "弗朗索瓦·莫里亚克"),
            ("b.epub", "窄门", "安德烈·纪德"),
            ("c.md", "纪德的日记", "安德烈·纪德"),
        ] {
            let id = store
                .add_book(&path(name), &meta_by(title, &[author]))
                .unwrap();
            store.save_flow(id, &flow(&[("一", "正文")])).unwrap();
        }
        let by_title = store.shelf("蛇结").unwrap();
        assert_eq!(by_title.len(), 1);
        assert_eq!(by_title[0].title, "蛇结");

        // 作者那一列存的是换行拼接的多个人名，LIKE 是子串匹配，因此照样命中。
        let by_author = store.shelf("纪德").unwrap();
        assert_eq!(by_author.len(), 2, "作者命中要带上两本");

        assert!(store.shelf("加缪").unwrap().is_empty());
        assert_eq!(store.shelf("  蛇结  ").unwrap().len(), 1, "首尾空白先去掉");
    }

    /// 书名里的 `%` 和 `_` 是字面字符，不是通配符。
    ///
    /// 不转义的话搜 `100%` 会把整个书架列出来（`%` 匹配任意串），
    /// 搜 `_门` 会命中「窄门」以外的任何两字书名（`_` 匹配任意单字）。
    #[test]
    fn shelf_treats_like_wildcards_in_query_as_text() {
        let store = Store::in_memory().unwrap();
        for title in ["100% 数学", "窄门", "X门"] {
            let id = store.add_book(&path(title), &meta(title)).unwrap();
            store.save_flow(id, &flow(&[("一", "正文")])).unwrap();
        }
        assert_eq!(store.shelf("100%").unwrap().len(), 1);
        assert_eq!(store.shelf("_门").unwrap().len(), 0, "下划线不该当通配符用");
        assert_eq!(
            store.shelf(r"a\b").unwrap().len(),
            0,
            "转义符本身也要当字面量"
        );
    }

    /// 封面的媒体类型：`books.cover` 那一列只有字节，所以格式得按字节头反推。
    /// 每种都写满探测窗口（前 16 字节），因为 `with_guessed_format` 只读那段。
    #[test]
    fn guess_media_type_reads_the_header_of_a_cover_with_no_media_type() {
        assert_eq!(guess_media_type(&png_header()), Some("image/png"));
        assert_eq!(
            guess_media_type(&[
                0xFF, 0xD8, 0xFF, 0xE0, 0x00, b'J', b'F', b'I', b'F', 0, 0, 0, 0, 0, 0, 0
            ]),
            Some("image/jpeg")
        );
        assert_eq!(
            guess_media_type(b"GIF89a\0\0\0\0\0\0\0\0\0\0"),
            Some("image/gif")
        );
        assert_eq!(
            guess_media_type(b"RIFF\x04\x00\x00\x00WEBPVP8 "),
            Some("image/webp")
        );
        assert_eq!(
            guess_media_type(b"BM\0\0\0\0\0\0\0\0\0\0\0\0\0\0"),
            Some("image/bmp")
        );
        // 认不出就是 None，让调用方去画占位 —— 猜成 PNG 去解一次只会静默画空。
        assert_eq!(guess_media_type(b"<svg xmlns='x'></svg>"), None);
        assert_eq!(guess_media_type(b""), None);
    }

    /// 一段够探测用的 PNG 头（magic + IHDR 的前几字节）。
    fn png_header() -> Vec<u8> {
        vec![
            0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0x0D, b'I', b'H', b'D', b'R',
        ]
    }

    fn meta_by(title: &str, authors: &[&str]) -> DocMeta {
        DocMeta {
            title: title.into(),
            authors: authors.iter().map(|a| (*a).to_string()).collect(),
            format: Format::Markdown,
            size: 1234,
        }
    }
}

#[cfg(test)]
mod annotations {
    //! 标注存储：锚点 JSON 的往返、颜色规范化、坏数据的影响面、级联删除。
    //!
    //! 与本文件的 `tests` 模块同一套办法：用公开的 `Store::open_at` 建库，需要看或改
    //! 「API 之下的那一列」时用一条裸 `rusqlite` 连接按**库文件契约**动手，不碰私有字段。

    use std::path::{Path, PathBuf};

    use crate::anchor::TextAnchor;
    use crate::format::{DocMeta, Format};
    use crate::{ANNOTATION_COLORS, Store};

    fn meta(title: &str) -> DocMeta {
        DocMeta {
            title: title.into(),
            authors: vec!["作者甲".into()],
            format: Format::Epub,
            size: 4096,
        }
    }

    fn path(name: &str) -> PathBuf {
        Path::new("/books").join(name)
    }

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_at(&dir.path().join("folio.db")).unwrap();
        (dir, store)
    }

    /// 一条有指纹的锚点。坐标本身随便取，测试要的是它能不能原样回来。
    fn anchor(chapter: u32, start: usize, end: usize) -> TextAnchor {
        TextAnchor {
            chapter,
            start,
            end,
            exact: "重点句子".into(),
            prefix: "前面的字".into(),
            suffix: "后面的字".into(),
            path: "/html[1]/body[1]/p[1]#0".into(),
        }
    }

    #[test]
    fn an_annotation_round_trips_through_the_database() {
        let (_dir, store) = store();
        let book = store.add_book(&path("a.epub"), &meta("书甲")).unwrap();
        let want = anchor(3, 120, 124);
        let id = store
            .add_annotation(book, &want, "这一段写得真好", "rose")
            .unwrap();

        let got = store.annotation(id).unwrap().expect("刚写的标注该读得回来");
        assert_eq!(got.id, id);
        assert_eq!(got.book_id, book);
        assert_eq!(
            got.anchor, want,
            "锚点必须逐字段回来，坐标与四个指纹都是数据"
        );
        // 章以锚点里的为准：表里那一列 `chapter_id` 只是索引与级删的挂钩。
        assert_eq!(got.chapter, 3);
        assert_eq!(got.note, "这一段写得真好");
        assert_eq!(got.color, "rose");
        assert!(!got.stale, "新写的标注不该是 stale");
        assert!(got.created_at > 0 && got.updated_at >= got.created_at);

        let list = store.annotations(book).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0], got);
    }

    #[test]
    fn colors_are_normalized_and_an_unknown_one_falls_back_to_the_default() {
        let (_dir, store) = store();
        let book = store.add_book(&path("a.epub"), &meta("书甲")).unwrap();
        let up = store
            .add_annotation(book, &anchor(0, 0, 4), "大写与空白", "  ROSE  ")
            .unwrap();
        let odd = store
            .add_annotation(book, &anchor(0, 0, 4), "没这个颜色", "chartreuse")
            .unwrap();
        assert_eq!(store.annotation(up).unwrap().unwrap().color, "rose");
        assert_eq!(
            store.annotation(odd).unwrap().unwrap().color,
            ANNOTATION_COLORS[0],
            "不认识的颜色回落到默认那条，而不是把批注写失败"
        );
    }

    #[test]
    fn updating_a_note_and_color_moves_updated_at_only() {
        let (_dir, store) = store();
        let book = store.add_book(&path("a.epub"), &meta("书甲")).unwrap();
        let id = store
            .add_annotation(book, &anchor(1, 10, 14), "初稿", "sky")
            .unwrap();
        store.update_annotation(id, "改过的批注", "lime").unwrap();
        let got = store.annotation(id).unwrap().expect("改过之后还在");
        assert_eq!(got.note, "改过的批注");
        assert_eq!(got.color, "lime");
    }

    /// stale 是「锚点现在落不落了回去」这件事，不是读者编辑过的证据。
    /// 它跟着 `updated_at` 走的话，每次改字号都会把从没动过的批注顶到列表最前。
    #[test]
    fn marking_stale_does_not_touch_the_timestamps() {
        let (_dir, store) = store();
        let book = store.add_book(&path("a.epub"), &meta("书甲")).unwrap();
        let id = store
            .add_annotation(book, &anchor(0, 0, 4), "批注", "amber")
            .unwrap();
        let before = store.annotation(id).unwrap().unwrap();
        store.set_annotation_stale(id, true).unwrap();
        let after = store.annotation(id).unwrap().unwrap();
        assert!(after.stale);
        assert_eq!(after.created_at, before.created_at);
        assert_eq!(after.updated_at, before.updated_at);

        store.set_annotation_stale(id, false).unwrap();
        assert!(!store.annotation(id).unwrap().unwrap().stale, "能回清");
    }

    /// 一本书里可能有多条标注，读列表要按创建顺序稳定返回（同秒创建时用 id 定序）。
    #[test]
    fn the_list_keeps_insertion_order() {
        let (_dir, store) = store();
        let book = store.add_book(&path("a.epub"), &meta("书甲")).unwrap();
        let first = store
            .add_annotation(book, &anchor(0, 0, 4), "一", "amber")
            .unwrap();
        let second = store
            .add_annotation(book, &anchor(0, 10, 14), "二", "rose")
            .unwrap();
        let ids: Vec<i64> = store
            .annotations(book)
            .unwrap()
            .iter()
            .map(|a| a.id)
            .collect();
        assert_eq!(ids, vec![first, second]);

        store.delete_annotation(first).unwrap();
        let left: Vec<i64> = store
            .annotations(book)
            .unwrap()
            .iter()
            .map(|a| a.id)
            .collect();
        assert_eq!(left, vec![second]);
    }

    /// 列表只属于那一本书。跨书读到别人的批注是数据泄露级别的错。
    #[test]
    fn annotations_do_not_leak_across_books() {
        let (_dir, store) = store();
        let a = store.add_book(&path("a.epub"), &meta("书甲")).unwrap();
        let b = store.add_book(&path("b.epub"), &meta("书乙")).unwrap();
        store
            .add_annotation(a, &anchor(0, 0, 4), "甲的", "amber")
            .unwrap();
        assert!(store.annotations(b).unwrap().is_empty(), "乙那儿该是空的");
    }

    /// 锚点那一列可能来自旧版本、或别的机器同步来的坏数据。
    /// 一条读不出来的行只能让它自己不出现，不能让整本书的批注列表报错。
    #[test]
    fn one_corrupt_anchor_json_hides_that_row_not_the_whole_list() {
        let (dir, store) = store();
        let book = store.add_book(&path("a.epub"), &meta("书甲")).unwrap();
        let good = store
            .add_annotation(book, &anchor(0, 0, 4), "读得到的", "amber")
            .unwrap();
        let broken = store
            .add_annotation(book, &anchor(0, 20, 24), "读不到的", "rose")
            .unwrap();

        let db = dir.path().join("folio.db");
        drop(store);
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute(
                "UPDATE annotations SET anchor_json = '{ 这不是 JSON' WHERE id = ?1",
                rusqlite::params![broken],
            )
            .unwrap();
        }
        let store = Store::open_at(&db).unwrap();
        let list = store.annotations(book).unwrap();
        assert_eq!(
            list.iter().map(|a| a.id).collect::<Vec<_>>(),
            vec![good],
            "坏那一条要静默跳过，其余照常可见"
        );
        assert!(
            store.annotation(broken).unwrap().is_none(),
            "单条读不出也当没有，而不是 panic"
        );
    }

    /// 章节还没入库（第一次打开、或 `save_flow` 之前就有批注）时外键列留 NULL，
    /// 但批注本身必须写得进去 —— 坐标里已经有章下标了。
    #[test]
    fn an_annotation_survives_a_missing_chapter_row() {
        let (dir, store) = store();
        let book = store.add_book(&path("a.epub"), &meta("书甲")).unwrap();
        let id = store
            .add_annotation(book, &anchor(7, 0, 4), "没有章节行也能记", "amber")
            .unwrap();

        let db = dir.path().join("folio.db");
        drop(store);
        let conn = rusqlite::Connection::open(&db).unwrap();
        let chapter_id: Option<i64> = conn
            .query_row(
                "SELECT chapter_id FROM annotations WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            chapter_id, None,
            "查不到对应章节行时留 NULL，而不是拒写或写一个错 id"
        );
        // 列表仍然按锚点里的章下标报回来。
        drop(conn);
        let store = Store::open_at(&db).unwrap();
        assert_eq!(store.annotation(id).unwrap().unwrap().chapter, 7);
    }

    /// 删书要连带删掉它的批注 —— 这一条走的是 schema 里的 `ON DELETE CASCADE`，
    /// 但级联只在连接开了外键时才生效，所以必须实测。
    #[test]
    fn removing_a_book_takes_its_annotations_with_it() {
        let (_dir, store) = store();
        let a = store.add_book(&path("a.epub"), &meta("书甲")).unwrap();
        let b = store.add_book(&path("b.epub"), &meta("书乙")).unwrap();
        store
            .add_annotation(a, &anchor(0, 0, 4), "甲的", "amber")
            .unwrap();
        store
            .add_annotation(b, &anchor(0, 0, 4), "乙的", "amber")
            .unwrap();

        store.remove_book(a).unwrap();
        assert!(
            store.annotations(a).unwrap().is_empty(),
            "删掉的书不该留批注"
        );
        assert_eq!(
            store.annotations(b).unwrap().len(),
            1,
            "别人的批注一根手指都不能碰"
        );
    }
}

#[cfg(test)]
mod collections {
    //! 集合（分组）：新建/改名/删除、书的进出、与书的级联。
    //!
    //! 与本文件其他测试同一套办法：只用公开 API；要看「API 之下那一列」时用裸
    //! `rusqlite` 连接按**库文件契约**查，不碰私有字段。

    use std::path::{Path, PathBuf};

    use crate::Store;
    use crate::format::{DocMeta, Format};

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

    fn book(store: &Store, name: &str) -> i64 {
        store.add_book(&path(name), &meta(name)).unwrap()
    }

    #[test]
    fn a_collection_round_trips_with_its_book_count() {
        let store = Store::in_memory().unwrap();
        let a = book(&store, "a.epub");
        let b = book(&store, "b.epub");
        let shelf = store.create_collection("想读").unwrap();

        assert_eq!(store.collection(shelf).unwrap().unwrap().name, "想读");
        assert!(store.add_to_collection(shelf, a).unwrap());
        assert!(store.add_to_collection(shelf, b).unwrap());

        let got = store.collection(shelf).unwrap().unwrap();
        assert_eq!(got.book_count, 2, "书数是列表页要显示的那个");
        let all = store.collections().unwrap();
        assert_eq!(all, vec![got]);
    }

    /// 同名再建是「回到那个集合」，不是第二条同名行 —— 否则 `UNIQUE` 会先报错。
    #[test]
    fn recreating_a_name_reuses_the_row_instead_of_failing() {
        let store = Store::in_memory().unwrap();
        let first = store.create_collection("想读").unwrap();
        let again = store.create_collection("  想读  ").unwrap();
        assert_eq!(first, again, "同名集合是同一个，且首尾空白先去掉");
        assert_eq!(store.collections().unwrap().len(), 1);
    }

    #[test]
    fn an_empty_name_is_rejected_on_create_and_rename() {
        let store = Store::in_memory().unwrap();
        assert!(matches!(
            store.create_collection("   ").unwrap_err(),
            crate::Error::EmptyCollectionName
        ));
        let ok = store.create_collection("有名字").unwrap();
        assert!(matches!(
            store.rename_collection(ok, "").unwrap_err(),
            crate::Error::EmptyCollectionName
        ));
    }

    #[test]
    fn joining_twice_is_a_no_op_and_leaving_twice_is_too() {
        let store = Store::in_memory().unwrap();
        let a = book(&store, "a.epub");
        let shelf = store.create_collection("想读").unwrap();
        assert!(store.add_to_collection(shelf, a).unwrap(), "第一次是新增");
        assert!(!store.add_to_collection(shelf, a).unwrap(), "第二次是重复");
        assert_eq!(store.collection(shelf).unwrap().unwrap().book_count, 1);

        assert!(store.remove_from_collection(shelf, a).unwrap());
        assert!(!store.remove_from_collection(shelf, a).unwrap());
        assert_eq!(store.collection(shelf).unwrap().unwrap().book_count, 0);
    }

    #[test]
    fn collections_of_a_book_are_listed_by_name() {
        let store = Store::in_memory().unwrap();
        let a = book(&store, "a.epub");
        let b = book(&store, "b.epub");
        let z = store.create_collection("b 组").unwrap();
        let y = store.create_collection("a 组").unwrap();
        store.add_to_collection(z, a).unwrap();
        store.add_to_collection(y, a).unwrap();

        let names: Vec<String> = store
            .collections_of_book(a)
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, vec!["a 组", "b 组"]);
        assert!(
            store.collections_of_book(b).unwrap().is_empty(),
            "b 不在任何组里"
        );
    }

    #[test]
    fn books_in_a_collection_come_back_newest_first() {
        let store = Store::in_memory().unwrap();
        let a = book(&store, "a.epub");
        let b = book(&store, "b.epub");
        let shelf = store.create_collection("想读").unwrap();
        store.add_to_collection(shelf, a).unwrap();
        store.add_to_collection(shelf, b).unwrap();

        let ids: Vec<i64> = store
            .books_in_collection(shelf)
            .unwrap()
            .into_iter()
            .map(|x| x.id)
            .collect();
        assert_eq!(ids, vec![b, a]);
        assert!(store.books_in_collection(9999).unwrap().is_empty());
    }

    /// 删集合只断链接，书本身必须还在 —— 这是集合与「删除」最容易搞混的一条。
    #[test]
    fn deleting_a_collection_keeps_its_books() {
        let store = Store::in_memory().unwrap();
        let a = book(&store, "a.epub");
        let shelf = store.create_collection("想读").unwrap();
        store.add_to_collection(shelf, a).unwrap();
        store.delete_collection(shelf).unwrap();

        assert!(store.collection(shelf).unwrap().is_none());
        assert_eq!(
            store.collections_of_book(a).unwrap().len(),
            0,
            "链接随集合走"
        );
        assert_eq!(
            store.book(a).unwrap().unwrap().title,
            "a.epub",
            "书本身还在"
        );
    }

    /// 删书要把它在各个集合里的名分一起带走（`collection_books` 上那条级联）。
    #[test]
    fn removing_a_book_clears_its_membership() {
        let store = Store::in_memory().unwrap();
        let a = book(&store, "a.epub");
        let shelf = store.create_collection("想读").unwrap();
        store.add_to_collection(shelf, a).unwrap();
        store.remove_book(a).unwrap();
        assert_eq!(store.collection(shelf).unwrap().unwrap().book_count, 0);
    }

    #[test]
    fn renaming_keeps_membership_and_a_duplicate_is_an_error() {
        let store = Store::in_memory().unwrap();
        let a = book(&store, "a.epub");
        let one = store.create_collection("甲").unwrap();
        let other = store.create_collection("乙").unwrap();
        store.add_to_collection(one, a).unwrap();

        store.rename_collection(one, " 新名字 ").unwrap();
        let got = store.collection(one).unwrap().unwrap();
        assert_eq!(got.name, "新名字");
        assert_eq!(got.book_count, 1, "改名不动名分");

        assert!(
            store.rename_collection(other, "新名字").is_err(),
            "撞上已有名字要被 UNIQUE 挡下，而不是静默合并"
        );
    }
}

#[cfg(test)]
mod reading {
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

#[cfg(test)]
mod grouping {
    //! 书架分组：按作者/格式分，以及集合那一种「组存在库里」的分组。
    //!
    //! 组内顺序只在确定时才断言（同一秒入库的书之间没有稳定先后），
    //! 组名与成员集合才是这几条要钉住的。

    use std::path::{Path, PathBuf};

    use crate::Store;
    use crate::format::{DocMeta, Format};
    use crate::group::{GroupBy, UNGROUPED};
    use crate::store::ShelfItem;

    fn meta(title: &str, author: &str) -> DocMeta {
        DocMeta {
            title: title.into(),
            authors: vec![author.into()],
            format: Format::Epub,
            size: 1,
        }
    }

    fn path(name: &str) -> PathBuf {
        Path::new("/books").join(name)
    }

    fn ids(items: &[ShelfItem]) -> Vec<i64> {
        items.iter().map(|i| i.id).collect()
    }

    #[test]
    fn shelf_grouped_splits_by_author() {
        let store = Store::in_memory().unwrap();
        let a = store
            .add_book(&path("a.epub"), &meta("甲书", "纪德"))
            .unwrap();
        let b = store
            .add_book(&path("b.epub"), &meta("乙书", "莫里亚克"))
            .unwrap();
        let c = store
            .add_book(&path("c.epub"), &meta("丙书", "纪德"))
            .unwrap();

        let groups = store.shelf_grouped("", GroupBy::Author).unwrap();
        let names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, vec!["纪德", "莫里亚克"]);
        assert_eq!(ids(&groups[0].items), vec![a, c]);
        assert_eq!(ids(&groups[1].items), vec![b]);
    }

    #[test]
    fn shelf_grouped_applies_the_query_filter_first() {
        let store = Store::in_memory().unwrap();
        let a = store
            .add_book(&path("a.epub"), &meta("蛇结", "莫里亚克"))
            .unwrap();
        let _b = store
            .add_book(&path("b.epub"), &meta("窄门", "纪德"))
            .unwrap();
        let groups = store.shelf_grouped("蛇结", GroupBy::Author).unwrap();
        assert_eq!(groups.len(), 1, "只留下命中的那一组");
        assert_eq!(ids(&groups[0].items), vec![a]);
    }

    #[test]
    fn collection_groups_end_with_an_ungrouped_bucket() {
        let store = Store::in_memory().unwrap();
        let a = store
            .add_book(&path("a.epub"), &meta("甲", "作者"))
            .unwrap();
        let b = store
            .add_book(&path("b.epub"), &meta("乙", "作者"))
            .unwrap();
        let c = store
            .add_book(&path("c.epub"), &meta("丙", "作者"))
            .unwrap();
        let shelf = store.create_collection("想读").unwrap();
        store.add_to_collection(shelf, a).unwrap();

        let groups = store.collection_groups().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].name, "想读");
        assert_eq!(ids(&groups[0].items), vec![a]);
        assert_eq!(groups[1].name, UNGROUPED);
        let mut loose = ids(&groups[1].items);
        loose.sort_unstable();
        assert_eq!(loose, vec![b, c]);
    }

    /// 刚建的空组也要在分组里看得见，否则读者会以为没建成。
    #[test]
    fn an_empty_collection_still_shows_up_as_an_empty_group() {
        let store = Store::in_memory().unwrap();
        store.create_collection("空组").unwrap();
        let groups = store.collection_groups().unwrap();
        assert_eq!(groups.len(), 1, "没有未入组的书时不追加那个桶");
        assert_eq!(groups[0].name, "空组");
        assert!(groups[0].items.is_empty());
    }
}

#[cfg(test)]
mod fixed {
    //! 定版式持久化：v7 迁移、页表往返、按内容家族读写。

    use std::path::{Path, PathBuf};

    use crate::Store;
    use crate::format::{
        Chapter, Content, DocMeta, FixedDoc, FixedPage, FlowDoc, Format, PageSource,
    };
    use crate::store::MIGRATIONS;

    fn meta(title: &str) -> DocMeta {
        DocMeta {
            title: title.into(),
            authors: vec!["作者".into()],
            format: Format::Epub,
            size: 1,
        }
    }

    fn path(name: &str) -> PathBuf {
        Path::new("/books").join(name)
    }

    fn page(href: &str) -> FixedPage {
        FixedPage {
            source: PageSource::Image {
                href: href.to_string(),
            },
            width: 800,
            height: 1200,
        }
    }

    fn fixed_doc() -> FixedDoc {
        FixedDoc {
            pages: vec![page("1.png"), page("2.png")],
            toc: Vec::new(),
            cover: Some(vec![1, 2, 3]),
            // 存的时候会由页表重建，这里故意留空。
            images: Vec::new(),
            width: 800,
            height: 1200,
        }
    }

    fn flow_doc() -> FlowDoc {
        FlowDoc {
            chapters: vec![Chapter {
                title: "一".into(),
                source: "正文".into(),
                spine_href: "#0".into(),
            }],
            ..Default::default()
        }
    }

    /// 已发布的 v6 库要能升到 v7，而且它原有的书一本不能少。
    #[test]
    fn a_v6_database_upgrades_and_keeps_its_books() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("folio.db");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            for sql in &MIGRATIONS[..6] {
                conn.execute_batch(sql).unwrap();
            }
            conn.pragma_update(None, "user_version", 6).unwrap();
            conn.execute(
                "INSERT INTO books (path, title, authors, format, size, added_at)
                 VALUES ('a.epub', 'A', '', 'epub', 0, 0)",
                [],
            )
            .unwrap();
        }

        let store = Store::open_at(&db).unwrap();
        assert_eq!(store.user_version().unwrap(), MIGRATIONS.len() as i64);
        assert_eq!(store.book(1).unwrap().unwrap().title, "A");
        assert!(
            store.load_fixed(1).unwrap().is_none(),
            "升级上来的书没有定版式，该是 None 而不是空页表"
        );
    }

    /// 页表要逐字段往返，图片表由它重建（不单独存一遍）。
    #[test]
    fn a_fixed_document_round_trips_and_rebuilds_its_image_table() {
        let store = Store::in_memory().unwrap();
        let book = store.add_book(&path("c.cbz"), &meta("漫画")).unwrap();
        let doc = fixed_doc();
        store.save_fixed(book, &doc).unwrap();

        let got = store.load_fixed(book).unwrap().expect("刚存的该读得回来");
        assert_eq!(got.pages, doc.pages);
        assert_eq!(got.cover, doc.cover);
        assert_eq!((got.width, got.height), (800, 1200));
        assert_eq!(got.images.len(), 2, "图片表由页表重建");
        assert_eq!(got.images[0].href, "1.png");
        assert_eq!(got.images[0].media_type, "image/png");
        assert_eq!((got.images[0].width, got.images[0].height), (800, 1200));
    }

    #[test]
    fn content_dispatch_reads_back_the_right_family() {
        let store = Store::in_memory().unwrap();
        let reflow = store.add_book(&path("a.md"), &meta("文")).unwrap();
        store
            .save_content(reflow, &Content::Reflow(flow_doc()))
            .unwrap();
        assert!(matches!(
            store.load_content(reflow).unwrap(),
            Some(Content::Reflow(_))
        ));

        let comic = store.add_book(&path("b.cbz"), &meta("图")).unwrap();
        store
            .save_content(comic, &Content::Fixed(fixed_doc()))
            .unwrap();
        assert!(matches!(
            store.load_content(comic).unwrap(),
            Some(Content::Fixed(_))
        ));
        assert!(store.load_content(9999).unwrap().is_none(), "没这本书");
    }

    /// 渲染型（PDF）没有 href，不该被塞进图片表。
    #[test]
    fn a_render_page_does_not_become_an_image_row() {
        let store = Store::in_memory().unwrap();
        let book = store.add_book(&path("d.pdf"), &meta("PDF")).unwrap();
        let doc = FixedDoc {
            pages: vec![FixedPage {
                source: PageSource::Render { index: 0 },
                width: 595,
                height: 842,
            }],
            ..Default::default()
        };
        store.save_fixed(book, &doc).unwrap();
        let got = store.load_fixed(book).unwrap().unwrap();
        assert_eq!(got.pages.len(), 1);
        assert!(got.images.is_empty(), "渲染型没有 href，不进图片表");
    }
}
