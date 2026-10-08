use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::Result;
use crate::format::{Chapter, DocMeta, FlowDoc};
use crate::position::Locator;
use crate::prefs::Prefs;

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
    let base = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
                .join(".local/share")
        });
    base.join("folio").join("folio.db")
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

impl Store {
    pub fn open_default() -> Result<Self> {
        Self::open_at(&default_db_path())
    }

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
            conn.execute_batch(sql)?;
            conn.pragma_update(None, "user_version", target)?;
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
        Ok(self.conn.query_row(
            "SELECT id FROM books WHERE path = ?1",
            params![path_s],
            |r| r.get(0),
        )?)
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
        self.conn
            .execute("DELETE FROM books WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn touch_opened(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE books SET last_opened_at = ?2 WHERE id = ?1",
            params![id, now()],
        )?;
        Ok(())
    }

    /// 整本替换章节、CSS、目录与封面，一个事务保证不留半本。
    pub fn save_flow(&self, book_id: i64, doc: &FlowDoc) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM chapters WHERE book_id = ?1", params![book_id])?;
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
        let toc = serde_json::from_str(&toc_json).unwrap_or_default();
        Ok(Some(FlowDoc {
            chapters,
            css,
            toc,
            cover,
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

    pub fn set_prefs(&self, prefs: &Prefs) -> Result<()> {
        self.conn.execute(
            "INSERT INTO prefs (key, value_json) VALUES ('reader', ?1)
             ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
            params![prefs.to_json()],
        )?;
        Ok(())
    }

    pub fn prefs(&self) -> Prefs {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT value_json FROM prefs WHERE key = 'reader'",
                [],
                |r| r.get(0),
            )
            .optional()
            .unwrap_or(None);
        json.map(|j| Prefs::from_json(&j)).unwrap_or_default()
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
        Ok(stmt
            .query_map(params![quoted, limit as i64], |r| {
                Ok(SearchHit {
                    book_id: r.get(0)?,
                    chapter_id: r.get(1)?,
                    chapter_title: r.get(2)?,
                    snippet: r.get(3)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?)
    }
}

/// 把用户输入包成 FTS5 短语，避免引号与括号被当查询语法解析。
fn fts_phrase(query: &str) -> String {
    format!("\"{}\"", query.replace('"', "\"\""))
}

#[cfg(test)]
mod tests;
