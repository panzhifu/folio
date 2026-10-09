use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::Result;
use crate::anchor::TextAnchor;
use crate::format::{Chapter, DocMeta, FlowDoc, ImageRes};
use crate::position::Locator;

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
                "INSERT INTO images (book_id, href, media_type, width, height, bytes)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    book_id,
                    img.href,
                    img.media_type,
                    img.width as i64,
                    img.height as i64,
                    img.bytes
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
        let mut img_stmt = self.conn.prepare(
            "SELECT href, media_type, width, height, bytes FROM images
             WHERE book_id = ?1 ORDER BY id",
        )?;
        let images = img_stmt
            .query_map(params![book_id], |r| {
                Ok(ImageRes {
                    href: r.get(0)?,
                    media_type: r.get(1)?,
                    width: r.get::<_, i64>(2)? as u32,
                    height: r.get::<_, i64>(3)? as u32,
                    bytes: r.get(4)?,
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
                children: Vec::new(),
            }],
            cover: Some(vec![0x89, b'P', b'N', b'G', 13, 10]),
            // 一张图片一起进往返：EPUB 的内嵌图走 images 表。
            images: vec![ImageRes {
                href: "OEBPS/Images/fig1.png".into(),
                media_type: "image/png".into(),
                width: 640,
                height: 480,
                bytes: vec![0x89, b'P', b'N', b'G', 1, 2, 3],
            }],
        }
    }

    #[test]
    fn migrations_are_idempotent_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("folio.db");

        let store = Store::open_at(&db).unwrap();
        assert_eq!(store.user_version().unwrap(), 4);
        let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
        store.save_flow(id, &flow(&[("第一章", "正文一")])).unwrap();
        drop(store);

        let again = Store::open_at(&db).unwrap();
        assert_eq!(again.user_version().unwrap(), 4, "重开不应重复跑迁移");
        assert_eq!(again.list_books().unwrap().len(), 1);
        assert_eq!(
            again.load_flow(id).unwrap().unwrap().chapters[0].title,
            "第一章"
        );
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

    /// 图片与尺寸必须原样回来，且重存时是替换而非追加。
    #[test]
    fn images_roundtrip_and_replace() {
        let store = Store::in_memory().unwrap();
        let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
        let mut doc = flow(&[("一", "正文")]);
        store.save_flow(id, &doc).unwrap();

        let got = store.load_flow(id).unwrap().unwrap();
        assert_eq!(got.images.len(), 1);
        assert_eq!(got.images[0].href, "OEBPS/Images/fig1.png");
        assert_eq!((got.images[0].width, got.images[0].height), (640, 480));
        assert_eq!(got.images[0].bytes, vec![0x89, b'P', b'N', b'G', 1, 2, 3]);

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
