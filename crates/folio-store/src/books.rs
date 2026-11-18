//! 书目、阅读位置与书架查询。内容哈希是书的身份，路径只表示副本位置。
//! 老行的 content_hash 可空，由补齐作业填充；按路径查询仍用于确认路径归属。

use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use super::types::{BookMeta, BookRow, Collection, ShelfItem};
use super::{Store, now};
use crate::Result;
use folio_format::{DocMeta, Format, Series};
use folio_model::group::{GroupBy, ShelfGroup, UNGROUPED, group_shelf};
use folio_model::position::Locator;
use folio_model::reading::BookSettings;

/// 作者、贡献者、主题词以换行分隔，与 [`split_list`] 共用编码。
pub(super) fn join_list(items: &[String]) -> String {
    items.join("\n")
}

/// [`join_list`] 的反向。空行丢掉 —— 作者名里不该有空行，而老库那一列可能就是空串。
pub(super) fn split_list(text: &str) -> Vec<String> {
    text.lines()
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

pub(super) fn book_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<BookRow> {
    let authors: String = r.get(3)?;
    Ok(BookRow {
        id: r.get(0)?,
        path: r.get(1)?,
        title: r.get(2)?,
        authors: split_list(&authors),
        format: r.get(4)?,
        size: r.get::<_, i64>(5)? as u64,
        last_opened_at: r.get(6)?,
    })
}

/// [`Store::book_meta`] 的列清单。它**不进** `BOOK_COLUMNS` 那套架面投影，
/// 理由见 [`BookMeta`]：简介与主题词是长文本，架面一次读几百行时用不上。
const META_COLUMNS: &str = "id, title, authors, contributors, subjects, description, publisher, \
                            published, language, isbn, dctype, rights, series, series_position, \
                            format, size";

/// 信息屏那一屏要的一行。列序与 [`META_COLUMNS`] 一致。
fn meta_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<BookMeta> {
    let list =
        |i: usize| -> rusqlite::Result<Vec<String>> { Ok(split_list(&r.get::<_, String>(i)?)) };
    // 系列两格：名字为空 = 不属于任何系列；序号只有在名字在的时候才取（空名字配
    // 一个孤立序号是脏数据，不该画成「第 3 本」）。
    let series = match r.get::<_, String>(12)? {
        name if name.is_empty() => None,
        name => Some(Series {
            name,
            position: r.get(13)?,
        }),
    };
    Ok(BookMeta {
        id: r.get(0)?,
        title: r.get(1)?,
        authors: list(2)?,
        contributors: list(3)?,
        subjects: list(4)?,
        description: r.get(5)?,
        publisher: r.get(6)?,
        published: r.get(7)?,
        language: r.get(8)?,
        isbn: r.get(9)?,
        dctype: r.get(10)?,
        rights: r.get(11)?,
        series,
        format: r.get(14)?,
        size: r.get::<_, i64>(15)? as u64,
    })
}

/// 文件的修改时间（Unix 秒；读不到是 0）。正文只在文件里，所以这是「还是那份文件吗」
/// 的第一道判据，见 [`Store::stale_reason`]。
fn mtime_of(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 书目行的列序。查询与 `book_from_row` 必须同源，所以写成一处常量。
/// 集合那几个查询要按同一列序取 `BookRow`，所以它对 `store` 内可见。
pub(super) const BOOK_COLUMNS: &str = "id, path, title, authors, format, size, last_opened_at";

/// 书架投影附带全书/已读字数及未知字数的章数；有未知章时不显示进度。
/// SQL 只读位置 JSON 的 chapter 整数，格式变更须与 Locator 迁移同步。
const SHELF_COLUMNS: &str = "books.id, books.path, books.title, books.authors, books.format, \
     books.size, books.last_opened_at, \
     (SELECT SUM(char_len) FROM chapters c WHERE c.book_id = books.id) AS total_chars, \
     (SELECT COUNT(*) FROM chapters c \
       WHERE c.book_id = books.id AND c.char_len IS NULL) AS unknown_chapters, \
     (SELECT SUM(c.char_len) FROM chapters c \
       WHERE c.book_id = books.id AND c.char_len IS NOT NULL \
         AND c.idx < json_extract(positions.locator_json, '$.chapter')) AS read_chars, \
     (SELECT COUNT(*) FROM chapters c \
       WHERE c.book_id = books.id AND c.char_len IS NULL \
         AND c.idx < json_extract(positions.locator_json, '$.chapter')) AS unknown_read, \
     positions.locator_json, books.cover_hash, books.cover_mime";

/// 整架与分组共用排序：最近导入或翻开的在前。
/// 秒级时间可能并列，最终以 books.id DESC 保证稳定顺序。
const SHELF_ORDER: &str = "ORDER BY MAX(added_at, COALESCE(last_opened_at, 0)) DESC, \
     last_opened_at IS NULL, last_opened_at DESC, books.id DESC";

/// 索引校验需要的文件、版本、封面与分章规则状态。
struct StaleRow {
    path: String,
    /// `books.format` 那一列，原样（库里是字符串，`Format::parse` 认不出来就当不依赖规则表）。
    format: String,
    size: i64,
    mtime: i64,
    version: i64,
    /// 封面文件名的两半：`covers/<hash 前 20 字>.<mime 决定的扩展名>`。
    /// 少一半就拼不出路径，也就没有「文件在不在」这一问。
    cover_hash: Option<String>,
    cover_mime: Option<String>,
    /// 这份索引算自哪一份分章规则表（v22 起）。NULL 的含义见 `store::schema` 那一版。
    rules_hash: Option<String>,
}

/// 副本父目录是书库根；位于 .staging 的临时副本再向上取一层。
pub fn book_root(copy_path: &Path) -> std::path::PathBuf {
    let parent = copy_path.parent().unwrap_or(Path::new(""));
    if parent.file_name().and_then(|n| n.to_str()) == Some(".staging") {
        return parent
            .parent()
            .map_or_else(|| parent.to_path_buf(), Path::to_path_buf);
    }
    parent.to_path_buf()
}

fn shelf_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ShelfItem> {
    let authors: String = r.get(3)?;
    // 下标须与 SHELF_COLUMNS 同步；未知分母与未读都不显示百分比。
    let total: Option<i64> = r.get(7)?;
    let unknown_chapters: i64 = r.get(8)?;
    let read_chars: Option<i64> = r.get(9)?;
    let unknown_read: i64 = r.get(10)?;
    let locator: Option<String> = r.get(11)?;
    let cover_hash: Option<String> = r.get(12)?;
    let cover_mime: Option<String> = r.get(13)?;
    // 两格要么都有、要么都没有：只剩一格拼不出文件名（见 `load_flow_in` 同一句）。
    let cover_ref = match (cover_hash, cover_mime) {
        (Some(hash), Some(mime)) if !hash.is_empty() && !mime.is_empty() => {
            Some(crate::assets::cover::CoverRef { hash, mime })
        }
        _ => None,
    };
    let copy: String = r.get(1)?;
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
        // 封面字节由 book_card 按 cover_ref 从 covers/ 读取。
        cover: None,
        progress: progress_of(
            total,
            unknown_chapters,
            read_chars,
            unknown_read,
            locator.as_deref(),
        ),
        cover_ref,
        // 反推书库根：缩略图缓存目录挂在它底下。副本路径来自 `books.path`，
        // 所以这一格与封面、正文用的是同一个来源（不是界面另推一遍）。
        cover_root: Some(book_root(Path::new(&copy))),
    })
}

/// 已读比例 = (前面各章字数 + 章内偏移) / 全书字数，结果夹在 [0, 1]。
/// 无位置、未知章字数、空书或位置 JSON 无效时返回 None，不冒充 0%。
fn progress_of(
    total: Option<i64>,
    unknown_chapters: i64,
    read_chars: Option<i64>,
    unknown_read: i64,
    locator: Option<&str>,
) -> Option<f32> {
    let total = total.filter(|n| *n > 0)?;
    if unknown_chapters > 0 || unknown_read > 0 {
        return None;
    }
    let at = Locator::try_from_json(locator?)?;
    let read = read_chars.unwrap_or(0).max(0) as f32;
    let offset = at.offset as f32;
    // 章内偏移可能比整本书还大（旧串、或章表变过），夹到分母以内。
    Some(((read + offset) / total as f32).clamp(0.0, 1.0))
}

/// 封面补齐那一趟要读的一行：`BookRow` 不带封面描述，而补齐既要判「指针在不在」
/// 也要判「那个文件还在不在」。
#[derive(Debug, Clone)]
pub struct CoverBackfillRow {
    pub id: i64,
    pub path: String,
    pub cover_hash: Option<String>,
    pub cover_mime: Option<String>,
}

/// 源文件探测的失败原因；这只是当前快照，不授权删除书库记录。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceUnavailable {
    Missing,
    NotFile,
    Unreadable(ErrorKind),
}

/// 一次只读检查发现的不可用书源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnavailableSource {
    pub book_id: i64,
    pub reason: SourceUnavailable,
}

fn source_unavailable(path: &Path) -> Option<SourceUnavailable> {
    let checked: std::io::Result<bool> = (|| {
        if !std::fs::metadata(path)?.is_file() {
            return Ok(false);
        }
        // metadata 成功不代表有读取权限；打开后再确认类型，且不读取正文。
        std::fs::File::open(path)?
            .metadata()
            .map(|meta| meta.is_file())
    })();
    match checked {
        Ok(true) => None,
        Ok(false) => Some(SourceUnavailable::NotFile),
        Err(error) if error.kind() == ErrorKind::NotFound => Some(SourceUnavailable::Missing),
        Err(error) => Some(SourceUnavailable::Unreadable(error.kind())),
    }
}

impl Store {
    /// 同路径入库更新文件事实，保留读者修改的 title/authors；哈希去重由导入管线负责。
    /// content_hash 另由 set_content_hash 写入，中断留下的 NULL 可由补齐作业恢复。
    pub fn add_book(&self, path: &Path, meta: &DocMeta) -> Result<i64> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let path_s = path.to_string_lossy().into_owned();
        conn.execute(
            "INSERT INTO books (path, title, authors, format, size, added_at, mtime,
                                contributors, subjects, description, publisher, published,
                                language, isbn, dctype, rights, series, series_position)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
             ON CONFLICT(path) DO UPDATE SET
               format = excluded.format, size = excluded.size,
               mtime = excluded.mtime,
               contributors = excluded.contributors, subjects = excluded.subjects,
               description = excluded.description, publisher = excluded.publisher,
               published = excluded.published, language = excluded.language,
               isbn = excluded.isbn, dctype = excluded.dctype, rights = excluded.rights,
               series = excluded.series, series_position = excluded.series_position",
            params![
                path_s,
                meta.title,
                join_list(&meta.authors),
                meta.format.as_str(),
                meta.size as i64,
                now(),
                mtime_of(path),
                join_list(&meta.contributors),
                join_list(&meta.subjects),
                meta.description,
                meta.publisher,
                meta.published,
                meta.language,
                meta.isbn,
                meta.dctype,
                meta.rights,
                meta.series.as_ref().map(|s| s.name.clone()).unwrap_or_default(),
                meta.series
                    .as_ref()
                    .map(|s| s.position.clone())
                    .unwrap_or_default(),
            ],
        )?;
        let id: i64 = conn.query_row(
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
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let sql = format!("SELECT {BOOK_COLUMNS} FROM books {SHELF_ORDER}");
        let mut stmt = conn.prepare(&sql)?;
        Ok(stmt
            .query_map([], book_from_row)?
            .collect::<std::result::Result<_, _>>()?)
    }

    /// 按书名/作者过滤全部书（含集合成员），空查询返回整架；排序见 SHELF_ORDER。
    /// 此入口不查询正文。
    pub fn shelf(&self, query: &str) -> Result<Vec<ShelfItem>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Self::shelf_in(&conn, query)
    }

    pub fn book(&self, id: i64) -> Result<Option<BookRow>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let sql = format!("SELECT {BOOK_COLUMNS} FROM books WHERE id = ?1");
        Ok(conn
            .query_row(&sql, params![id], book_from_row)
            .optional()?)
    }

    /// 按 id 读取完整书目，长文本不进入书架批量投影；缺失字段保持空值。
    pub fn book_meta(&self, id: i64) -> Result<Option<BookMeta>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let sql = format!("SELECT {META_COLUMNS} FROM books WHERE id = ?1");
        Ok(conn
            .query_row(&sql, params![id], meta_from_row)
            .optional()?)
    }

    /// 从 covers/ 读取封面，文件缺失返回 None；不从数据库读取或缓存封面字节。
    pub fn book_cover(&self, id: i64) -> Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let row: Option<(String, Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT path, cover_hash, cover_mime FROM books WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((path, hash, mime)) = row else {
            return Ok(None);
        };
        let (Some(hash), Some(mime)) = (hash, mime) else {
            // 没有描述 = 这本书没有封面（Markdown / TXT，或 EPUB 里没声明）。
            return Ok(None);
        };
        let root = book_root(Path::new(&path));
        let Some(found) = crate::assets::cover::resolve(&root, &hash, &mime) else {
            // 描述在、文件不在：`covers/` 被清过，或库是别处拷来的（没有 `covers/`）。
            // 这一档**只记 debug**：它是「这本书现在没有封面」的正常状态，
            // 而每一帧的书架都会问一遍，warn 会刷爆日志。
            tracing::debug!("store: 第 {id} 本的封面文件不在 {root:?}/covers，画占位");
            return Ok(None);
        };
        match std::fs::read(&found) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(err) => {
                // 文件在但读不动（权限、坏扇区）：对读者来说与「不在」是同一种表现，
                // 但原因是另一回事，所以留给日志一条 warn 级的线索。
                tracing::warn!(path = %found.display(), "store: 封面文件读不动：{err}");
                Ok(None)
            }
        }
    }

    pub fn book_by_path(&self, path: &Path) -> Result<Option<BookRow>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let sql = format!("SELECT {BOOK_COLUMNS} FROM books WHERE path = ?1");
        let s = path.to_string_lossy().into_owned();
        Ok(conn.query_row(&sql, params![s], book_from_row).optional()?)
    }

    /// 按内容身份查询，路径或文件名变化不应产生第二套进度与批注。
    pub fn book_by_hash(&self, hash_hex: &str) -> Result<Option<BookRow>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let sql = format!("SELECT {BOOK_COLUMNS} FROM books WHERE content_hash = ?1");
        Ok(conn
            .query_row(&sql, params![hash_hex], book_from_row)
            .optional()?)
    }

    /// 记下这本书的内容身份。`NULL → 有值` 是懒补唯一的状态转移，所以这里既不改路径
    /// 也不判重：改名由调用方做完之后把新位置交给 [`Store::retarget_book_path`]。
    pub fn set_content_hash(&self, id: i64, hash_hex: &str, algo: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "UPDATE books SET content_hash = ?2, hash_algo = ?3 WHERE id = ?1",
            params![id, hash_hex, algo],
        )?;
        Ok(())
    }

    /// 返回 (字节数, 内容哈希, 行号)，供扫描先按大小筛选，再计算哈希。
    /// 缺少哈希或 size=0 的旧行不参与判重。
    pub fn identities_by_size(&self) -> Result<Vec<(u64, String, i64)>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT size, content_hash, id FROM books
              WHERE content_hash IS NOT NULL AND content_hash <> '' AND size > 0",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)? as u64,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// 更新副本路径、大小与修改时间，保留内容身份；用于改名和缺失副本修复。
    /// 路径被其它记录占用时返回错误，不覆盖记录。
    pub fn retarget_book_path(&self, id: i64, path: &Path) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let s = path.to_string_lossy().into_owned();
        let mtime = mtime_of(path);
        let rows = match std::fs::metadata(path) {
            Ok(meta) => conn.execute(
                "UPDATE books SET path = ?2, size = ?3, mtime = ?4 WHERE id = ?1",
                params![id, s, meta.len() as i64, mtime],
            )?,
            // 文件刚被 rename 走又读不到 metadata（权限、竞态）：位置还是要改，尺寸留着旧数。
            Err(_) => conn.execute(
                "UPDATE books SET path = ?2, mtime = ?3 WHERE id = ?1",
                params![id, s, mtime],
            )?,
        };
        if rows == 0 {
            tracing::warn!("store: 把第 {id} 本改指 {s:?}，但没有这一行");
        }
        Ok(())
    }

    /// 只更新 title/authors；作者按换行存储并去掉空白项。
    /// 重扫与重复导入保留这两项，正文、进度和批注不变。
    pub fn set_book_meta(&self, id: i64, title: &str, authors: &[String]) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let rows = conn.execute(
            "UPDATE books SET title = ?2, authors = ?3 WHERE id = ?1",
            params![id, title, authors.join("\n")],
        )?;
        if rows == 0 {
            tracing::warn!("store: 改第 {id} 本的书名作者，但没有这一行");
        }
        Ok(())
    }

    /// 按本读取阅读设置差量；记录不存在返回空差量并记录警告。
    pub fn book_settings(&self, id: i64) -> Result<BookSettings> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let raw: Option<String> = conn
            .query_row(
                "SELECT settings_json FROM books WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        match raw {
            Some(raw) => Ok(BookSettings::from_column(&raw)),
            None => {
                tracing::warn!("store: 读第 {id} 本的阅读设置，但没有这一行");
                Ok(BookSettings::default())
            }
        }
    }

    /// 整份替换阅读差量；空差量写空串。
    /// 每次写入（含恢复默认）都更新时间戳，供同步比较整份差量。
    pub fn set_book_settings(&self, id: i64, settings: &BookSettings) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let stamp = super::sync_order::next_version(
            super::sync_order::settings(&conn, id)?.map_or(0, |r| r.updated_at),
            now(),
        )?;
        let rows = conn.execute(
            "UPDATE books
                SET settings_json = ?2, settings_updated_at = ?3
              WHERE id = ?1",
            params![id, settings.to_column(), stamp],
        )?;
        if rows == 0 {
            tracing::warn!("store: 改第 {id} 本的阅读设置，但没有这一行");
        }
        Ok(())
    }

    /// 按 id 返回 content_hash 为 NULL 的行，供可中断的补齐作业处理。
    pub fn rows_missing_hash(&self) -> Result<Vec<BookRow>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let sql =
            format!("SELECT {BOOK_COLUMNS} FROM books WHERE content_hash IS NULL ORDER BY id");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map([], book_from_row)?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    /// 统计其它记录对副本的引用；删除文件前必须检查，避免毁掉共享正文。
    pub fn rows_sharing_path(&self, path: &str, except_id: i64) -> Result<usize> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM books WHERE path = ?1 AND id != ?2",
            params![path, except_id],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

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

    /// 写一本书的封面描述（补齐那一趟的落点）：只动这两列，不碰进度、批注与索引版本。
    pub fn set_cover_ref(&self, id: i64, hash: &str, mime: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "UPDATE books SET cover_hash = ?2, cover_mime = ?3 WHERE id = ?1",
            params![id, hash, mime],
        )?;
        Ok(())
    }

    /// 统计其它记录对封面的引用；封面按自身内容寻址，删除前须保护共享文件。
    pub fn rows_sharing_cover(&self, hash: &str, except_id: i64) -> Result<usize> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM books WHERE cover_hash = ?1 AND id != ?2",
            params![hash, except_id],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// 在删除书目行前取得封面路径，行删除后将无法再查询其哈希。
    pub fn orphan_cover_file(&self, id: i64) -> Result<Option<std::path::PathBuf>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let row: Option<(String, Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT path, cover_hash, cover_mime FROM books WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((path, hash, mime)) = row else {
            return Ok(None);
        };
        let (Some(hash), Some(mime)) = (hash, mime) else {
            return Ok(None);
        };
        // 别人还指着同一份字节 → 这个文件不是孤儿。
        let others: i64 = conn.query_row(
            "SELECT COUNT(*) FROM books WHERE cover_hash = ?1 AND id != ?2",
            params![hash, id],
            |r| r.get(0),
        )?;
        if others > 0 {
            return Ok(None);
        }
        Ok(Some(crate::assets::cover::cover_path(
            &book_root(Path::new(&path)),
            &hash,
            &mime,
        )))
    }

    pub fn remove_book(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        // 书从库里消失不是「读者删了某条批注」：它的批注/书签随外键级删，
        // 但不该给别的设备发删除墓碑（别的设备可能还留着这本书）。把这本书名下的
        // 历史墓碑一并清掉，免得以后同名同哈希的书回来时被旧删除误伤。
        let hash: Option<String> = conn
            .query_row(
                "SELECT content_hash FROM books WHERE id = ?1",
                params![id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        let rows = conn.execute("DELETE FROM books WHERE id = ?1", params![id])?;
        if let Some(hash) = hash
            && !hash.is_empty()
        {
            conn.execute(
                "DELETE FROM sync_tombstones WHERE book_hash = ?1",
                params![hash],
            )?;
        }
        // 删掉 0 行只有两种可能：那本书本来就不在，或者调用方拿错了 id。
        // 两种都表现为「界面上按了删除但书还在」，所以这里要说一句。
        if rows == 0 {
            tracing::warn!("store: 删第 {id} 本，但没有这一行");
        } else {
            tracing::debug!("store: 删第 {id} 本（连带 {rows} 行外键）");
            // 一本的正文加章节行是几百 KB 级的删除，空页当场交回去。
            Self::reclaim(&conn)?;
        }
        Ok(())
    }

    pub fn touch_opened(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "UPDATE books SET last_opened_at = ?2 WHERE id = ?1",
            params![id, now()],
        )?;
        Ok(())
    }

    pub fn set_position(&self, book_id: i64, mode: &str, locator: &Locator) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let stamp = super::sync_order::next_version(
            super::sync_order::position(&conn, book_id)?.map_or(0, |r| r.updated_at),
            now(),
        )?;
        conn.execute("INSERT INTO positions (book_id,mode,locator_json,updated_at) VALUES (?1,?2,?3,?4) ON CONFLICT(book_id) DO UPDATE SET mode=excluded.mode,locator_json=excluded.locator_json,updated_at=excluded.updated_at", params![book_id,mode,locator.to_json(),stamp])?;
        Ok(())
    }

    /// 写入进度，但用调用方给的时间戳。**只给同步用**：把远端那份原样搬进来时要保留
    /// 它的时间，否则「本机刚应用了远端一条较早的进度」会被记成现在，下一次合并反过来
    /// 压掉另一台设备更新的进度。
    pub fn set_position_at(
        &self,
        book_id: i64,
        mode: &str,
        locator: &Locator,
        updated_at: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT INTO positions (book_id, mode, locator_json, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(book_id) DO UPDATE SET
               mode = excluded.mode, locator_json = excluded.locator_json,
               updated_at = excluded.updated_at",
            params![book_id, mode, locator.to_json(), updated_at],
        )?;
        Ok(())
    }

    /// 进度连同它的时间戳。同步合并要比较「谁更新」，普通的 [`Store::position`] 不带这一格。
    pub fn position_with_time(&self, book_id: i64) -> Result<Option<(String, Locator, i64)>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Ok(conn
            .query_row(
                "SELECT mode, locator_json, updated_at FROM positions WHERE book_id = ?1",
                params![book_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get(2)?)),
            )
            .optional()?
            .map(|(mode, json, updated_at)| (mode, Locator::from_json(&json), updated_at)))
    }

    pub fn position(&self, book_id: i64) -> Result<Option<(String, Locator)>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Ok(conn
            .query_row(
                "SELECT mode, locator_json FROM positions WHERE book_id = ?1",
                params![book_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
            .map(|(mode, json)| (mode, Locator::from_json(&json))))
    }

    /// 记下「这一章读到过」。
    ///
    /// 与 [`Store::set_position`] 是两件事：那一行只说「现在在哪儿」，读者翻过去就
    /// 覆盖；这一条说的是「来过」，所以只插不改（`INSERT OR IGNORE`）—— 第二次进同一章
    /// 不该把第一次的时间冲掉，也不该多出一行。
    pub fn mark_chapter_read(&self, book_id: i64, idx: usize) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT OR IGNORE INTO chapter_read (book_id, idx, read_at) VALUES (?1, ?2, ?3)",
            params![book_id, idx as i64, now()],
        )?;
        Ok(())
    }

    /// 某本书到过的章号集合。目录那一屏按它把「来过的」画成浅色。
    pub fn read_chapters(&self, book_id: i64) -> Result<HashSet<usize>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows =
            conn.prepare("SELECT idx FROM chapter_read WHERE book_id = ?1 ORDER BY idx")?;
        let out = rows
            .query_map(params![book_id], |r| r.get::<_, i64>(0))?
            .filter_map(|value| value.ok())
            .map(|idx| idx as usize)
            .collect();
        Ok(out)
    }

    /// 抹掉某本书的「来过」痕迹。重新开读一本书时读者要的是那张白纸，
    /// 而进度与标注都不在这一条里 —— 只清这一张表。
    pub fn clear_chapter_read(&self, book_id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "DELETE FROM chapter_read WHERE book_id = ?1",
            params![book_id],
        )?;
        Ok(())
    }

    /// 书架分组：先按 `query` 过滤，再按 `by` 分。
    ///
    /// 分组本身是纯函数（[`folio_model::group::group_shelf`]），这里只负责先取书架。
    /// 集合那种「组是读者建的、存在库里」的情形不走这里，见
    /// [`Store::collection_groups`]。
    pub fn shelf_grouped(&self, query: &str, by: GroupBy) -> Result<Vec<ShelfGroup>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Ok(group_shelf(Self::shelf_in(&conn, query)?, by))
    }

    /// 每个集合一组，保留空集合，末尾追加名为 [`UNGROUPED`] 的未分组。
    /// 这是完整清点接口；架面组卡另按 [`Collection::is_shown`] 隐藏空集合。
    pub fn collection_groups(&self) -> Result<Vec<ShelfGroup>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut groups = Vec::new();
        for c in Self::collections_in(&conn)? {
            groups.push(ShelfGroup {
                name: c.name,
                items: Self::shelf_in_collection_in(&conn, c.id, "")?,
            });
        }
        let loose = Self::shelf_in_no_collection_in(&conn, "")?;
        if !loose.is_empty() {
            groups.push(ShelfGroup {
                name: UNGROUPED.to_string(),
                items: loose,
            });
        }
        Ok(groups)
    }

    /// 架面组卡投影：集合按 [`Store::collections`] 排序，每组只取前 `limit` 本以免读取整组。
    /// 组内沿用 [`Store::shelf_in_collection`] 的最近阅读优先顺序；空组按
    /// [`Collection::is_shown`] 隐藏，完整清点请用 [`Store::collection_groups`]。
    pub fn collection_tiles(&self, limit: usize) -> Result<Vec<(Collection, Vec<ShelfItem>)>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Self::collections_in(&conn)?
            .into_iter()
            .filter(Collection::is_shown)
            .map(|c| {
                let mut items = Self::shelf_in_collection_in(&conn, c.id, "")?;
                items.truncate(limit);
                Ok((c, items))
            })
            .collect()
    }

    /// 按书名/作者筛选集合成员，顺序与整架共用 SHELF_ORDER。
    pub fn shelf_in_collection(&self, collection_id: i64, query: &str) -> Result<Vec<ShelfItem>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Self::shelf_in_collection_in(&conn, collection_id, query)
    }

    /// 按书名/作者筛选未入组的书；未分组是查询结果，不是库里的集合。
    pub fn shelf_ungrouped(&self, query: &str) -> Result<Vec<ShelfItem>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Self::shelf_in_no_collection_in(&conn, query)
    }

    /// [`Store::shelf`] 的不上锁版本；只由已经持了锁的一方调用。
    fn shelf_in(conn: &Connection, query: &str) -> Result<Vec<ShelfItem>> {
        let trimmed = query.trim();
        let sql = format!(
            r#"SELECT {SHELF_COLUMNS} FROM books
             LEFT JOIN positions ON positions.book_id = books.id
             WHERE {}
             {SHELF_ORDER}"#,
            query_clause("?1", "?2")
        );
        let mut stmt = conn.prepare(&sql)?;
        let pattern = like_pattern(trimmed);
        Ok(stmt
            .query_map(params![trimmed, pattern], shelf_from_row)?
            .collect::<std::result::Result<_, _>>()?)
    }

    /// 已持库锁的查询助手。使用 IN 子查询避免 JOIN 带来的 added_at 列名歧义。
    fn shelf_in_collection_in(
        conn: &Connection,
        collection_id: i64,
        query: &str,
    ) -> Result<Vec<ShelfItem>> {
        let trimmed = query.trim();
        let sql = format!(
            r#"SELECT {SHELF_COLUMNS} FROM books
             LEFT JOIN positions ON positions.book_id = books.id
             WHERE books.id IN (SELECT book_id FROM collection_books WHERE collection_id = ?1)
               AND {}
             {SHELF_ORDER}"#,
            query_clause("?2", "?3")
        );
        let mut stmt = conn.prepare(&sql)?;
        let pattern = like_pattern(trimmed);
        Ok(stmt
            .query_map(params![collection_id, trimmed, pattern], shelf_from_row)?
            .collect::<std::result::Result<_, _>>()?)
    }

    /// 还没进任何集合的书，排序与 [`Store::shelf`] 同一条（同一个 `SHELF_ORDER` 常量）。
    /// [`Store::collection_groups`] 与架面整架那一片都走这里，不上锁。
    fn shelf_in_no_collection_in(conn: &Connection, query: &str) -> Result<Vec<ShelfItem>> {
        let trimmed = query.trim();
        let sql = format!(
            r#"SELECT {SHELF_COLUMNS} FROM books
             LEFT JOIN positions ON positions.book_id = books.id
             WHERE books.id NOT IN (SELECT book_id FROM collection_books)
               AND {}
             {SHELF_ORDER}"#,
            query_clause("?1", "?2")
        );
        let mut stmt = conn.prepare(&sql)?;
        let pattern = like_pattern(trimmed);
        Ok(stmt
            .query_map(params![trimmed, pattern], shelf_from_row)?
            .collect::<std::result::Result<_, _>>()?)
    }
}

/// 全架、集合与未分组查询共用的过滤条件；占位符序号由调用方提供。
fn query_clause(word: &str, pattern: &str) -> String {
    format!(
        r#"({word} = ''
                    OR books.title LIKE {pattern} ESCAPE '\'
                    OR books.authors LIKE {pattern} ESCAPE '\')"#
    )
}

/// 将用户输入转成 LIKE 匹配串：先转义反斜杠，再转义 % 和 _。
/// SQL 必须同时声明 ESCAPE。
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
mod grouping {
    //! 书架分组的组名、成员与稳定顺序回归。

    use std::path::{Path, PathBuf};

    use crate::ShelfItem;
    use crate::Store;
    use folio_format::{DocMeta, Format};
    use folio_model::group::{GroupBy, UNGROUPED};

    fn meta(title: &str, author: &str) -> DocMeta {
        DocMeta {
            title: title.into(),
            authors: vec![author.into()],
            format: Format::Epub,
            size: 1,
            ..DocMeta::default()
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
        // 组内是 `[c, a]` 而不是 `[a, c]`：三本同一秒入库，`added_at DESC` 并列，
        // 由 `books.id DESC` 收尾 —— 后落库的在先。见 `shelf_order_breaks_the_import_tie`。
        assert_eq!(ids(&groups[0].items), vec![c, a]);
        assert_eq!(ids(&groups[1].items), vec![b]);
    }

    /// 同秒入库仍须按 id 稳定排序。
    #[test]
    fn shelf_order_breaks_the_import_tie() {
        let store = Store::in_memory().unwrap();
        for name in ["a.epub", "b.epub", "c.epub"] {
            store
                .add_book(&path(name), &meta("同秒入库", "纪德"))
                .unwrap();
        }
        // 三本同一秒：先确认这条测试的前提成立，否则它什么都没钉住。
        let tied = store
            .shelf("")
            .unwrap()
            .iter()
            .map(|item| item.id)
            .collect::<Vec<_>>();
        assert_eq!(tied.len(), 3);
        // 后落库的在先，且连查两次同序 —— 少了终结符，第二条就可能变。
        assert_eq!(tied, vec![3, 2, 1], "同秒入库该按行号倒序");
        assert_eq!(
            store
                .shelf("")
                .unwrap()
                .iter()
                .map(|i| i.id)
                .collect::<Vec<_>>(),
            tied
        );
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

    /// 集合成员按阅读/导入时间排序，加入集合的时间不改变顺序。
    #[test]
    fn a_collection_page_keeps_the_shelf_order() {
        let store = Store::in_memory().unwrap();
        let a = store
            .add_book(&path("a.epub"), &meta("甲", "作者"))
            .unwrap();
        let b = store
            .add_book(&path("b.epub"), &meta("乙", "作者"))
            .unwrap();
        let shelf = store.create_collection("想读").unwrap();
        store.add_to_collection(shelf, a).unwrap();
        store.add_to_collection(shelf, b).unwrap();
        // 后进组的是 b，先翻开的是 a —— 两条序在这里给出相反的答案。
        store.touch_opened(a).unwrap();

        assert_eq!(
            ids(&store.shelf_in_collection(shelf, "").unwrap()),
            vec![a, b],
            "组内该按最近打开排，与整架同一条"
        );
    }

    /// 入组、移出、删书分别维护未分组投影，成员不得重复或残留。
    #[test]
    fn the_ungrouped_bucket_follows_membership() {
        let store = Store::in_memory().unwrap();
        let a = store
            .add_book(&path("a.epub"), &meta("甲", "作者"))
            .unwrap();
        let b = store
            .add_book(&path("b.epub"), &meta("乙", "作者"))
            .unwrap();
        assert_eq!(ids(&store.shelf_ungrouped("").unwrap()), vec![b, a]);

        let shelf = store.create_collection("想读").unwrap();
        store.add_to_collection(shelf, a).unwrap();
        store.add_to_collection(shelf, a).unwrap();
        assert_eq!(
            ids(&store.shelf_ungrouped("").unwrap()),
            vec![b],
            "同一本进两次不算两次"
        );

        store.remove_from_collection(shelf, a).unwrap();
        assert_eq!(
            ids(&store.shelf_ungrouped("").unwrap()),
            vec![b, a],
            "移出来就又回到整架那一片"
        );
        store.remove_book(b).unwrap();
        assert_eq!(
            ids(&store.shelf_ungrouped("").unwrap()),
            vec![a],
            "删掉一本少一本"
        );
    }

    /// 「按组清点」那份数据形状里要看得见空组 —— 架面那两处入口不摆它，判据见
    /// [`Store::collection_tiles`] 与 `Collection::is_shown`。
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

/// 一本书的索引为什么不可信。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaleReason {
    /// 源文件不在了 —— 正文取不到，这本书读不了。
    Missing,
    /// 源文件换了（大小或修改时间对不上）—— 位置还在，指的却是别的内容。
    Changed,
    /// 索引是旧引擎算的（[`folio_format::INDEX_VERSION`] 对不上）：
    /// 切章的算法、位置方案或页表算法改过。
    OldIndex,
    /// 分章规则数据改变（与算法版本 OldIndex 区分），需要重扫。
    RulesChanged,
    /// 已登记的封面文件丢失，需要从书文件重新提取。
    MissingCover,
}

impl Store {
    /// 检查索引是否过期；None 表示可信，文件不可用不授权删除用户记录。
    pub fn stale_reason(&self, book_id: i64) -> Result<Option<StaleReason>> {
        // 锁内查询，锁外访问文件系统，避免磁盘等待阻塞库操作。
        let row: Option<StaleRow> = {
            let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
            // 封面缺失最后检查，不能盖过正文或索引错误。
            conn.query_row(
                "SELECT path, format, size, mtime, index_version, cover_hash, cover_mime, rules_hash
                   FROM books WHERE id = ?1",
                params![book_id],
                |r| {
                    Ok(StaleRow {
                        path: r.get(0)?,
                        format: r.get(1)?,
                        size: r.get(2)?,
                        mtime: r.get(3)?,
                        version: r.get(4)?,
                        cover_hash: r.get(5)?,
                        cover_mime: r.get(6)?,
                        rules_hash: r.get(7)?,
                    })
                },
            )
            .optional()?
        };
        let Some(row) = row else {
            return Ok(None);
        };
        // 文件可用性优先于算法与内容变化；具体不可用原因见 unavailable_sources。
        let meta = std::fs::metadata(&row.path);
        let Ok(meta) = meta else {
            return Ok(Some(StaleReason::Missing));
        };
        if row.version != folio_format::INDEX_VERSION {
            return Ok(Some(StaleReason::OldIndex));
        }
        let now_mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        // `size` 用的是导入那一刻文件的大小；`books.size` 在旧库里可能来自 provider
        // 报的正文长度而不是文件长度，所以只在**两个数都非零且都不一样**时才判换了。
        let changed = row.mtime != 0 && now_mtime != 0 && row.mtime != now_mtime;
        if changed {
            return Ok(Some(StaleReason::Changed));
        }
        // 仅依赖规则表的格式参与；旧行 NULL 也必须触发重扫。
        let format = Format::parse(&row.format);
        let want = format.and_then(folio_format::rules_hash);
        if let Some(want) = want
            && row.rules_hash.as_deref() != Some(want.as_str())
        {
            return Ok(Some(StaleReason::RulesChanged));
        }
        // 只检查已登记封面；从未有封面的书不能因此反复重扫。
        if let (Some(hash), Some(mime)) = (row.cover_hash.as_deref(), row.cover_mime.as_deref())
            && !hash.is_empty()
            && !mime.is_empty()
        {
            let root = book_root(Path::new(&row.path));
            if crate::assets::cover::resolve(&root, hash, mime).is_none() {
                return Ok(Some(StaleReason::MissingCover));
            }
        }
        let _ = row.size;
        Ok(None)
    }

    /// 未在飞且本进程内未失败过才领号；成功后必须提交作业，由 note_reindex 退号。
    pub fn claim_reindex(&self, book_id: i64) -> bool {
        // 两道门各自一个作用域，**不同时持有两把锁**：`note_reindex` 取它们的次序是反的，
        // 嵌套持有就是死锁的素材（作业在池线程上，与界面线程真的会撞上）。
        if self
            .reindex_failed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&book_id)
        {
            return false;
        }
        self.reindex_queued
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(book_id)
    }

    /// 作业收尾时退号；失败只在本进程记住，避免每次开书重复全量重扫。
    pub fn note_reindex(&self, book_id: i64, ok: bool) {
        self.reindex_queued
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&book_id);
        if !ok {
            self.reindex_failed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(book_id);
        }
    }

    /// 检查源文件是否可读，保留书目、进度和批注；文件系统探测在库锁外执行。
    pub fn unavailable_sources(&self) -> Result<Vec<UnavailableSource>> {
        Ok(self
            .list_books()?
            .into_iter()
            .filter_map(|book| {
                source_unavailable(Path::new(&book.path)).map(|reason| UnavailableSource {
                    book_id: book.id,
                    reason,
                })
            })
            .collect())
    }
}
