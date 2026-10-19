//! 书目与书架：入库、查询、删除，以及「读到哪儿了」「到过哪些章」和分组。
//!
//! 书的**身份**是内容哈希（`books.content_hash`，v12 起，见 `plans/导入管线.md` §六），
//! 判重走 [`Store::book_by_hash`]；`books.path` 从那一刻起只回答「副本躺在哪儿」，
//! 查与删仍然按它走，但它不再是「是不是同一本书」的判据。
//! 这一列此刻**可为空**（懒补：老行由补齐作业逐本填），所以「按路径认」那条路还要留着
//! —— [`Store::book_by_path`] 现在的用处是「这条副本路径被哪一行认领着」，不是判存在性。
//!
//! 分组查询（[`Store::shelf_grouped`] / [`Store::collection_groups`]）也住在这里：
//! 它们不是另一张表的事，只是同一个书架换一种摆法。

use std::collections::HashSet;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use super::types::{BookMeta, BookRow, Collection, ShelfItem};
use super::{Store, now};
use crate::Result;
use crate::content::format::{DocMeta, Series};
use crate::group::{GroupBy, ShelfGroup, UNGROUPED, group_shelf};
use crate::position::Locator;

/// 多值的那几格（作者、贡献者、主题词）在库里是**换行拼接**的一列。
///
/// 不另引一种 JSON 编码：同一件事两种写法，早晚长出两种读法。`join("\n")` 与
/// `lines()` 就是这一列的全部规则，写侧与读侧各一处。
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

/// 书架投影：书目那几列，加上算已读比例要的四格。
///
/// 后四格（下标 8–11）都可空，因为「算不出来」是常态而不是异常：
///
/// - `total_chars` —— 全书规范化字数（`SUM` 跳过 NULL）。
/// - `unknown_chapters` —— 有几章的字数还没算过。**有它就整个不报进度**：
///   `SUM` 跳过 NULL 会让分母偏小、百分比偏大，而那种错是安静的
///   （读者读到一半进度条已经满了）。
/// - `read_chars` —— 读者当前位置**之前**各章的字数之和。
/// - `unknown_read` —— 那些章里有几章字数不知道（理由同上）。
///
/// SQL 只读 `json_extract(positions.locator_json, '$.chapter')` **一个整数**，
/// 用来跟 `chapters.idx` 比大小；位置串本身仍由 Rust 侧的 [`Locator`] 反序列化
/// （见 `progress_of`）。之所以敢在 SQL 里碰 JSON，是因为这里把 `chapter` 当
/// **不透明的位置指示**用：这个键从 v1 的位置格式起就没改过，改动它本来就要
/// 升 `LOCATOR_FORMAT_VERSION` 并走迁移，那时这里会跟着一起改（有测试盯着）。
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

/// 书架那套排序，几处查询共用一条常量。
///
/// **末尾的 `books.id DESC` 不是装饰，是必需的终结符**：入库时间戳是**秒**
/// （`store::now`），一次批量导入的十几本会拿到同一个 `added_at`，于是 `added_at DESC`
/// 全是并列 —— 并列之间 SQLite 不保证任何顺序，实测同一份数据能给出两种先后（本项目的
/// `store::books::grouping::shelf_grouped_splits_by_author` 就是这么翻车的）。后果不只是
/// 难看：整架那一片与某一组那一页在两次查询之间换序，读者的眼睛跟着丢一行。
///
/// 按 `id DESC` 收尾 = 「同秒入库的，后落库的在先」，与 `added_at DESC` 同向。
///
/// 整架、某一组那一页、还没进组的那一批三条查询共用这一条常量 —— 分组只回答「摆哪几本」，
/// 「先看见哪一本」不归它管（见 [`Store::shelf_in_collection`]）。
const SHELF_ORDER: &str =
    "ORDER BY last_opened_at IS NULL, last_opened_at DESC, added_at DESC, books.id DESC";

/// 一本副本所在的书库根 —— 用于拼 `covers/` 的位置。
///
/// 副本直接躺在书库根上（`plans/导入管线.md` 的施工标注里那条偏离：
/// 文档写 `<书库>/books/<哈希名>`，实现是「根就是那一层 `books/`」），所以
/// **父目录就是书库根**。唯一的例外是导入途中的那份：它在 `<书库>/.staging/` 底下，
/// 父目录是 `.staging` —— 退回一层。
///
/// 为什么从路径推而不是让 `Store` 记住书库位置：书库位置是**读者确认过的目录**，
/// 属于界面/配置那一层；`Store` 只是「这些行存在哪儿」，把它绑到一个绝对路径上
/// 会让测试与「换书库位置」都变难。而副本路径本来就在库里那一行上。
///
/// 推出来的目录不存在时也不报错：`cover::resolve` 会回 `None`，读那一侧退回 BLOB。
/// `stale_reason` 要的那几格：路径、大小、mtime、索引版本、封面哈希。
///
/// 与 [`CoverRow`] 同一个理由：七元组摊在 `query_row` 闭包里的话，
/// 「哪一格是什么」只存在于 `SELECT` 那一行的顺序里，改列序时编译器帮不上忙。
type StaleRow = (String, i64, i64, i64, Option<String>);

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
    // 第 7 格是 `books.cover` 留下的空位（v16 删了那一列，字节改走 `covers/`），
    // 所以下面这几格的**下标比看上去小一号**。改 `SHELF_COLUMNS` 时这几行要一起改 ——
    // 这一层是位置对应的，编译器帮不上忙。
    //
    // 「算不出分母」与「没读过」都回 `None` —— 界面两者都该按「没有进度可显示」
    // 处理，而不是画一条 0% 的进度条（那会让「还没算过」看起来像「一字未读」）。
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
            Some(crate::content::format::cover::CoverRef { hash, mime })
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
        // 第 7 格（`books.cover`）已经不存在（v16：字节不能进库）。架上那一格
        // 画的封面由 `book_card` 按 `cover_ref` 去 `covers/` 取文件，
        // 所以 `ShelfItem.cover` 恒为 `None` —— 它留着只是为了让读的人看出
        // 「封面不走这一条路」，而不是一个被忘掉的字段。
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

/// **已读比例**：`(前面各章字数之和 + 章内偏移) ÷ 全书总字数`。
///
/// 五个入参就是 [`SHELF_COLUMNS`] 那几格：全书字数、还没算过字数的章数、
/// 当前位置之前各章的字数之和、那些章里有几章不知道、以及位置 JSON 原文。
///
/// 四种情况回 `None`，每一种都是「不该显示进度」而不是「显示 0%」：
///
/// - **没打开过**（没有位置行）—— 「从没打开」在界面上通常要画成另一种样子，
///   所以这里不替界面决定，把判断权交出去。
/// - **有章字数不知道** —— 无论不知道的那章在当前位置之前还是之后，分母都可能
///   偏小、百分比偏大。这是安静的错误，所以宁可整本不报；`reindex` 一次就会补上。
/// - **总字数为 0** —— 空书，除法没有意义。
/// - **位置串读不出来** —— 坏 JSON、或来自更新版本的位置格式。
///
/// `offset` 要夹：它可能来自改过字号之前的旧串，越界不该让进度条冲出 100%。
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

impl Store {
    /// 落一行书目：同一**路径**重复入库是更新，不是第二本书。
    ///
    /// 「这本书是不是已经在了」不由这里判——管线在落地之前就按内容哈希判过重了
    /// （[`Store::book_by_hash`]）。这里只管把那一行写成现在这个样子。
    ///
    /// `content_hash` 不在本语句里写：管线随后单独调 [`Store::set_content_hash`]。
    /// 中间崩溃留下的是一条**没有身份的行**，而懒补本来就有这个状态（NULL），
    /// 补齐作业会重算哈希把它填上——所以这两步不需要包在一个事务里。
    pub fn add_book(&self, path: &Path, meta: &DocMeta) -> Result<i64> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let path_s = path.to_string_lossy().into_owned();
        conn.execute(
            "INSERT INTO books (path, title, authors, format, size, added_at, mtime,
                                contributors, subjects, description, publisher, published,
                                language, isbn, dctype, rights, series, series_position)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
             ON CONFLICT(path) DO UPDATE SET
               title = excluded.title, authors = excluded.authors,
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

    /// 书架：整架或按**书名 / 作者**过滤，排序与 [`Store::list_books`] 同一条
    /// （最近在读在前，从没打开过的按导入时间倒序）。
    ///
    /// 空查询走 `?1 = ''` 那个短路分支而不是拼两条 SQL：一条语句两种绑定，
    /// 免得两处各写一遍排序而写歪。
    ///
    /// 这里**不**查正文。全文检索是另一件事，而书架上那颗框在读者眼里是
    /// 「找我那本书」，输入两个字就该立刻列出书名 —— 把两者混在一个入口会让短查询
    /// 静默返回空。
    ///
    /// 这一条数的是**全部**书，不看集合名分 —— 所以架面带搜索词时走的是它而不是
    /// [`Store::shelf_ungrouped`]：挂进过组的书也要搜得出来。
    ///
    /// （正文的全文检索推后做。真要接的时候，索引该是**可重建的派生数据**，
    /// 而不是现在这样跟正文一起当权威数据存，见 `plans/text-compressed.md`。）
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

    /// 一本**书**的全部书目（Dublin Core 那一组）。
    ///
    /// 按 id 单查一条，而不是给 `BookRow` 加几列：架面一次要过几百行，而简介是长文本。
    /// 同一条理由当年把封面字节挪出了库（v16），也同一条理由不让 `shelf()` 顺手带上它们。
    /// 值全部来自导入那一趟 —— 这一格为空就是「这本书的书目区里没写」，不是「没读」。
    pub fn book_meta(&self, id: i64) -> Result<Option<BookMeta>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let sql = format!("SELECT {META_COLUMNS} FROM books WHERE id = ?1");
        Ok(conn
            .query_row(&sql, params![id], meta_from_row)
            .optional()?)
    }

    /// 单拿这一本的封面字节。
    ///
    /// **只认 `covers/` 里那个文件**（v16：字节不再进库）。文件不在了就回 `None`，
    /// 界面画占位 —— 不再退回任何库里存的副本，因为库里已经没有字节了。
    ///
    /// 这个取舍是有意的（`plans/导入管线.md` §八 第 4 条）：封面字节是 MB 级的东西，
    /// 而 `books` 表每次列书目都要过一遍；把它留在库里等于让库随书数线性膨胀，
    /// 只为架上那一格画什么。代价是**封面不会自己回来** —— `covers/` 被清掉、
    /// 或这一行是别处拷来的库，就要重新导入那一本。封面是装饰，不是内容的入口。
    ///
    /// 为什么不在 [`Store::book`] 里一起给：`BookRow` 被桌面端整库握在手里当标签页
    /// 列表用，那一用不需要字节；而「点开一本书的信息」那一屏要的正好只是这一本的图，
    /// 一次一条语句取回，比把整库的图读进内存便宜得多。
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
        let Some(found) = crate::content::format::cover::resolve(&root, &hash, &mime) else {
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

    /// 按**内容哈希**找那一行：`plans/导入管线.md` §六 的判重入口，同哈希 = 同一本书。
    ///
    /// 走这一列而不是 `path`：书库搬家、读者改文件名、同一本书从三个来源进来，都不该
    /// 因此多出第二行（而第二行意味着第二套进度与批注）。
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

    /// 把那一行的「副本在哪儿」改成新位置。身份那一列不动 —— 内容没变，变的是它躺在哪。
    ///
    /// 两个用处：补齐作业把旧名字副本改成哈希名之后回写；以及判重命中**但旧副本文件已经不
    /// 在了**那次自愈 —— 记录重新接上一份内容相同的文件（§六「判重的语义」）。
    /// `size` 与 `mtime` 一起刷：文件换了就该被 [`Store::stale_reason`] 认出来，留着旧数
    /// 等于让自愈过的书一直报「文件被外部改过」。
    ///
    /// 新位置撞上别的行的 `path`（`UNIQUE`）时这里返回 `Err`：那种情况只能是两行在往同一个
    /// 哈希名落地，判重本该先把它们合成一行，所以不静默覆盖，报出去让人看见。
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

    /// 改掉读者**自己认**的书名与作者：只动 `title` 与 `authors` 两列。
    ///
    /// 为什么只有这两列改得动：`format` / `size` / `path` 是那份文件的事实，读者说了不算；
    /// 而入库时从文件里抠出来的书目经常是错的 —— EPUB 的 `<dc:title>` 写着 `Untitled`、
    /// 作者栏塞的是出版社，架面上认错一本靠的正是这两条。正文、进度、批注一概不碰。
    ///
    /// `authors` 按 `\n` 存，读回时 [`book_from_row`] 用 `.lines()` 拆 —— 与
    /// [`Store::add_book`] 写的那份是同一个编码，两侧不必再约定一遍分隔符。
    /// 空串与只有空白的作者**不存**：`authors = ''` 读回来是空 `Vec`，界面上那一行随之消失
    /// （`shelf::book_info` 的「作者可能缺」那条规则）。
    ///
    /// **同一路径再导一次会把它盖掉**：[`Store::add_book`] 的 `ON CONFLICT DO UPDATE`
    /// 用文件里的书目重写这两列。这里不留「读者改过」的标记 —— 此刻认文件与内容是权威，
    /// 手动改的名字属于「重导就没了」那一类，界面不另加解释。
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

    /// 还没算过内容身份的行（`content_hash IS NULL`），整行交出去。
    ///
    /// 这是懒补的那半张清单：补齐作业按它逐本算哈希、把旧名字的副本改成哈希名、回写
    /// 位置（`Cleanup` 档，见 `crate::infra::task::Priority` 与 `crate::ingest::import::backfill_copies`）。
    /// 空表 = 全库都已认得自己的书。
    /// 按 `id` 排是为了断言可复现，也让一次中断之后剩下的那批是确定的。
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

    /// 还有多少行指着同一个副本路径（不算 `except_id` 自己）。
    ///
    /// 删一本书的副本之前问这一句：内容寻址之后「两条记录共用一个文件」是一个**可能**的
    /// 状态（自愈会把记录接到一份已存在的副本上），连带删掉另一本的正文与批注不可接受
    /// （`plans/导入管线.md` §六 末段、§十一 已决第 3 条）。计数从库里查，不另设计数表 ——
    /// 计数表与记录本身迟早会漂开。
    pub fn rows_sharing_path(&self, path: &str, except_id: i64) -> Result<usize> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM books WHERE path = ?1 AND id != ?2",
            params![path, except_id],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// 还有几条**别的**记录指着同一张封面 —— 删书时决定那个封面文件能不能删。
    ///
    /// 与 [`Store::rows_sharing_path`] 同一条规矩（`plans/导入管线.md` §八 第 4 条
    /// 末段）：**封面是它自己字节的身份，不是这本书的**。两本书共用一张封面
    /// （同一套系列设计）时磁盘上只有一个文件，所以删掉其中一本不能连带删掉它 ——
    /// 那会让另一本的封面凭空消失，而且没有任何报错。
    ///
    /// 计数从库里查，不另设计数表：一张表要与 `books.cover_hash` 保持一致，
    /// 而它本来就是派生量。
    pub fn rows_sharing_cover(&self, hash: &str, except_id: i64) -> Result<usize> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM books WHERE cover_hash = ?1 AND id != ?2",
            params![hash, except_id],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// 这一本的封面文件路径（**在库里那一行还活着的时候问**）。
    ///
    /// 删书那一步要用它：先问出文件名，删完行之后才发现「还有别人指着」就晚了。
    /// 行删掉之后 `cover_hash` 就问不出来，所以顺序不能反。
    ///
    /// `None` 有两种来路，删书那一侧都该当「没有文件可删」：
    ///
    /// - **老库那一档**：封面还只在 `books.cover` 那个 BLOB 里，磁盘上没有文件。
    /// - **还有别人指着同一份字节**：两本书共用一张封面时磁盘上只有一个文件
    ///   （`plans/导入管线.md` §八 第 4 条），删掉其中一本不能把它带走 ——
    ///   那会让另一本的封面凭空消失，而且没有任何报错。
    ///
    /// 把「共用」这一问收在这里，是因为它要读 `payload` 之外的列（`cover_hash` 的
    /// 完整十六进制），而调用方手上只有文件名那一格（它是哈希的前 20 字）。
    /// 让调用方自己去比完整哈希，就是两处各答一遍「谁跟谁共用」。
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
        Ok(Some(crate::content::format::cover::cover_path(
            &book_root(Path::new(&path)),
            &hash,
            &mime,
        )))
    }

    pub fn remove_book(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let rows = conn.execute("DELETE FROM books WHERE id = ?1", params![id])?;
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
        conn.execute(
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
    /// 分组本身是纯函数（[`crate::group::group_shelf`]），这里只负责先取书架。
    /// 集合那种「组是读者建的、存在库里」的情形不走这里，见
    /// [`Store::collection_groups`]。
    pub fn shelf_grouped(&self, query: &str, by: GroupBy) -> Result<Vec<ShelfGroup>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Ok(group_shelf(Self::shelf_in(&conn, query)?, by))
    }

    /// 每个集合一组，末尾追一个「未分组」（装了但还没进任何集合的书）。
    ///
    /// 这里**空集合也留着**：这一条是「按组清点」那份数据形状（一组一串书），不是架面顶部
    /// 那一排组卡的读法 —— 组卡按 [`Collection::is_shown`] 筛掉 0 本的。判据不同是有意的：
    /// 清点要看得到空组，而一张点进去是空白的卡不该占架面顶部那一格。
    /// 组名用 [`UNGROUPED`] 而不是空串，理由见那里的说明。
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

    /// 每个**摆得出来**的组带前 `limit` 本，按 [`Store::collections`] 那条序排（组内有过
    /// 打开的在前）。架面顶部那一排组卡走这一条。
    ///
    /// 只取前 `limit` 本而不是整组：一张卡装得下的就是那几格，而一个组可能有几百本 ——
    /// 一次查全部会把画不出来的那些行也读一遍。取的是组内那条「最近在先」（与
    /// [`Store::shelf_in_collection`] 同一个排序），所以卡上那几张封面就是读者在这一组里
    /// 最后翻开的那几本 —— 认一组靠的是「这里我在读什么」，不是「最后丢进来的是什么」。
    ///
    /// 0 本的组**不给卡**（[`Collection::is_shown`]）：那条入口点进去是一屏空，而它占的
    /// 那一格正压着「我刚在读的那一组在哪儿」。留着空组原本的顾虑是「刚建的组消失会像没
    /// 建成」，而架面建组那一刻必然同时挂进一本书（`ShelfView::create_group`），那条走不到。
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

    /// 一个集合里的书，可再按书名 / 作者筛一道。架面点开某一组那一页走这一条。
    ///
    /// 排序**与整架同一条**（同一个 `SHELF_ORDER` 常量）：分组只回答「摆哪几本」，不改
    /// 「先看见哪一本」。按进组先后排的话，刚丢进来的那本会把读到一半的书挤到下面，
    /// 而读者点开一组要找的正是「我上次读到哪儿了」。
    pub fn shelf_in_collection(&self, collection_id: i64, query: &str) -> Result<Vec<ShelfItem>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Self::shelf_in_collection_in(&conn, collection_id, query)
    }

    /// 还没进任何集合的书，同一套书名 / 作者过滤。**架面整架那一片走这一条**：一本挂进了
    /// 某个组就从整架消失，只有走进那一组才看得见（界面侧只有组卡与那颗「去处」菜单两处
    /// 入口，没有叫「未分组」的那一页 —— 整架那一片本身就是它）。
    ///
    /// 「未分组」是筛出来的、不是存出来的：库里没有一个叫这个名字的组，它是
    /// [`Store::collection_groups`] 末尾追加的那一格（名字用 [`crate::group::UNGROUPED`]）。
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

    /// [`Store::shelf_in_collection`] 的不上锁版本；只由已经持了锁的一方调用。
    ///
    /// 名分用 `IN (SELECT …)` 问而不是 join `collection_books`：join 进来会带一张也有
    /// `added_at` 的表，那条共用的 `SHELF_ORDER` 里的裸列名就此变成歧义列（SQLite 当场
    /// 报错，不是静默选错），与 [`Store::shelf_in_no_collection_in`] 同一写法。
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

/// 「词为空就全放，否则书名或作者命中」那一段 WHERE。整架、某组、未分组三条查询共用
/// 同一份措辞 —— 少一处 `ESCAPE '\\'` 或把列名写歪，不会报错，只会让三处搜出三套结果。
///
/// 占位符序号由调用方给：整架那条词是 `?1`、模式串是 `?2`；按组筛的那条前面还占了一个
/// 集合 id，序号就往后挪一位。
fn query_clause(word: &str, pattern: &str) -> String {
    format!(
        r#"({word} = ''
                    OR books.title LIKE {pattern} ESCAPE '\'
                    OR books.authors LIKE {pattern} ESCAPE '\')"#
    )
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
mod grouping {
    //! 书架分组：按作者/格式分，以及集合那一种「组存在库里」的分组。
    //!
    //! 组内先后**是**可以断言的：`SHELF_ORDER` 末尾带 `books.id DESC` 那个终结符，
    //! 同秒入库的书因此有确定顺序（接线前没有，那时这几条只能比集合，见
    //! `shelf_order_breaks_the_import_tie`）。组名、成员、先后三样都钉。

    use std::path::{Path, PathBuf};

    use crate::Store;
    use crate::content::format::{DocMeta, Format};
    use crate::group::{GroupBy, UNGROUPED};
    use crate::store::ShelfItem;

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

    /// 同秒入库必须有终结符，否则书架的顺序是不确定的。
    ///
    /// 入库时间戳按**秒**存（`store::now`），所以这三本 `added_at` 完全相同，
    /// `added_at DESC` 排不出先后；SQLite 对并列行不保证顺序，实测**同一份数据在同一次
    /// 运行里就能给出两种先后**（这条测试在加 `books.id DESC` 之前大约每五六次翻一次红，
    /// 而它每次都是新开的 `:memory:` 库，与别的测试无关 —— 那不是 flaky 的借口，是缺终结符）。
    /// 生产上的后果更实在：批量导入完刷新书架，同一本书换了一格。
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

    /// 组里那一页的序与整架同一条：**翻开过的那本在先**，不是「最近丢进组的在先」。
    ///
    /// 这一条钉的是把 `cb.added_at` 换成 `SHELF_ORDER` 那一手。旧写法下这两句会是
    /// `[b, a]`（b 后进组），读者「读到一半的那本」被一次分组操作挤到第二排。门要的是
    /// 「组只决定摆哪几本，不决定先看见哪一本」这条不改的序。
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

    /// 还没进组的那一批跟着名分变 —— **那一片就是架面的整架**（`ShelfScope::Shelf` 不带
    /// 搜索词时查的正是这一条），所以「进组」在读者手里等于从整架消失、「移出」等于回到整架。
    ///
    /// 四句各自都能单独坏掉而别的门还是绿的：名分那行写重了会让同一本在架上出现两次；
    /// 移出没删行会让一本书从整架与那一组两头一起消失；删书不清 `collection_books` 会让
    /// 某一组数着一本已经不存在的书。
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
    /// 索引是旧算法算的（分章规则 / 位置方案改过）。
    OldIndex,
    // 曾经还有一档 `OldCover`（封面按旧规格生成）：v16 之后封面是一个文件，
    // 换规格只要重新生成缩略图（`format::thumb`），不必重扫全书 —— 所以这一档
    // 连同它的判据（`cover_spec` 那一列）一起作废了。
}

impl Store {
    /// 这本书记下的索引还可信吗？`None` = 可信。
    ///
    /// 正文只在文件里（见 `plans/text-out-of-db.md`），所以「文件还是那份文件吗」从
    /// 锦上添花变成了**能不能读**的前提。四种不可信各自有后果：
    ///
    /// - 文件不在 → 这一章取不到，重扫也没用；
    /// - 文件换了 → 位置指到别的内容上，进度与批注也跟着漂；
    /// - 索引旧了 → 序号（`Section(n)`）按旧规则算的，可能整体错位；
    /// - 封面旧了 → 只影响书架那张缩略图，重扫一次就按新规格重生成。
    ///
    /// 第一种可以直接删数据（`prune_missing`），其余该重扫 —— 调用方选。
    pub fn stale_reason(&self, book_id: i64) -> Result<Option<StaleReason>> {
        // 锁内只问一句 SELECT，出锁再碰文件系统。`fs::metadata` 留在锁里时，界面线程为了
        // 判「这本还可信吗」既要排在一次后台写库之后，还要替它等一次磁盘 ——
        // `plans/阅读管线.md` §十.4 第 1 条点名的「锁内做了文件系统操作」就是这一处。
        let row: Option<StaleRow> = {
            let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
            // `cover_hash` 只用来在下面判「这一本有没有封面」——v16 之后封面规格那一档
            // （`cover_spec`）连同 BLOB 一起没了：封面现在是一个文件，换规格只要重新生成
            // 缩略图（`format::thumb::purge` + 懒生成），不必重扫全书。
            conn.query_row(
                "SELECT path, size, mtime, index_version, cover_hash
                   FROM books WHERE id = ?1",
                params![book_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?
        };
        let Some((path, size, mtime, version, cover_hash)) = row else {
            return Ok(None);
        };
        let _has_cover = cover_hash.is_some_and(|h| !h.is_empty());
        // 顺序即优先级：文件不在最严重（要删数据），其次才是「算法换了」与「文件换了」。
        let meta = std::fs::metadata(&path);
        let Ok(meta) = meta else {
            return Ok(Some(StaleReason::Missing));
        };
        if version != crate::content::format::INDEX_VERSION {
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
        let changed = mtime != 0 && now_mtime != 0 && mtime != now_mtime;
        if changed {
            return Ok(Some(StaleReason::Changed));
        }
        let _ = size;
        Ok(None)
    }

    /// 领一张重扫的号：这一本此刻既没在飞、也没在今天这个进程里失败过时返回 `true`，
    /// 并把它记成在飞。返回 `false` 时调用方**不该**提交作业。
    ///
    /// 「同一本只允许一趟」与「失败过就别再自动试」两道门都收在这里，不留在调用方：
    /// 两处各答一遍「这本该不该重扫」必然漂，同 `layout::key` 只留一把键的理由。
    /// 号由作业那头 [`Store::note_reindex`] 释放，所以领到号就必须真的把作业投出去。
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

    /// 重扫作业收尾：退号；失败的那本另记一档，本进程内不再自动重投。
    ///
    /// 成功要**从在飞里摘掉**而不是留着 —— `index_version` 下次被 bump 时那一本还得能再投
    /// 一趟，留着号等于把「扫过」变成「永不重扫」。失败留着，因为重扫必然读完整本书
    /// （`plans/导入管线.md` §八 第 3 条：要读完全书才知道的数全在导入这一趟算），没有这一格
    /// 的话一本注定失败的书会让每次点开都白付一次全量重读。只在本进程内记：下次启动
    /// 该再给它一次机会，而「坏在磁盘上」那种状态库里没有地方放。
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

    /// 源文件已经不在的那些书：**连数据一起删掉**。
    ///
    /// 判据是「文件不在」+「上一层目录读得到」：目录本身读不到多半是没挂载 /
    /// 权限没给（SD 卡拔了、SAF 授权被撤），那种情况下把读者的库删空是灾难。
    ///
    /// 返回删掉的那几本（书名），调用方拿去说一句。
    pub fn prune_missing(&self) -> Result<Vec<String>> {
        let books = self.list_books()?;
        let mut gone = Vec::new();
        for book in books {
            let path = Path::new(&book.path);
            if path.exists() {
                continue;
            }
            // 目录读不到就放过：那是「现在看不见」，不是「没了」。
            let parent_readable = path.parent().is_none_or(|dir| dir.exists());
            if !parent_readable {
                tracing::warn!(
                    "store: 第 {} 本《{}》的文件看不见，但上一层目录也读不到 —— 先不删",
                    book.id,
                    book.title
                );
                continue;
            }
            tracing::warn!(
                "store: 第 {} 本《{}》的源文件不在了（{}），连带数据一起删",
                book.id,
                book.title,
                book.path
            );
            self.remove_book(book.id)?;
            gone.push(book.title);
        }
        Ok(gone)
    }
}
