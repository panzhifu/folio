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

use super::types::{BookRow, ShelfItem};
use super::{Store, now};
use crate::Result;
use crate::format::DocMeta;
use crate::group::{GroupBy, ShelfGroup, UNGROUPED, group_shelf};
use crate::position::Locator;

pub(super) fn book_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<BookRow> {
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
     books.size, books.last_opened_at, books.cover, \
     (SELECT SUM(char_len) FROM chapters c WHERE c.book_id = books.id) AS total_chars, \
     (SELECT COUNT(*) FROM chapters c \
       WHERE c.book_id = books.id AND c.char_len IS NULL) AS unknown_chapters, \
     (SELECT SUM(c.char_len) FROM chapters c \
       WHERE c.book_id = books.id AND c.char_len IS NOT NULL \
         AND c.idx < json_extract(positions.locator_json, '$.chapter')) AS read_chars, \
     (SELECT COUNT(*) FROM chapters c \
       WHERE c.book_id = books.id AND c.char_len IS NULL \
         AND c.idx < json_extract(positions.locator_json, '$.chapter')) AS unknown_read, \
     positions.locator_json";

/// 书架那套排序，几处查询共用一条常量。
///
/// **末尾的 `books.id DESC` 不是装饰，是必需的终结符**：入库时间戳是**秒**
/// （`store::now`），一次批量导入的十几本会拿到同一个 `added_at`，于是 `added_at DESC`
/// 全是并列 —— 并列之间 SQLite 不保证任何顺序，实测同一份数据能给出两种先后（本项目的
/// `store::books::grouping::shelf_grouped_splits_by_author` 就是这么翻车的）。后果不只是
/// 难看：书架与「未分组」那一栏在两次查询之间换序，读者的眼睛跟着丢一行。
///
/// 按 `id DESC` 收尾 = 「同秒入库的，后落库的在先」，与 `added_at DESC` 同向，也和
/// `shelf_in_collection_in` 那条早就在用的 `cb.added_at DESC, books.id DESC` 一致。
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
/// 封面那一行的四格：字节、副本路径、封面哈希、封面 MIME。
///
/// 单独立一个名字是为了 `query_row` 的闭包签名 —— 五元组摊在参数里会让
/// 「哪一格是什么」只存在于 `SELECT` 那一行的顺序里，改列序时编译器帮不上忙。
type CoverRow = (Option<Vec<u8>>, String, Option<String>, Option<String>);

pub(crate) fn book_root(copy_path: &Path) -> std::path::PathBuf {
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
    // 「算不出分母」与「没读过」都回 `None` —— 界面两者都该按「没有进度可显示」
    // 处理，而不是画一条 0% 的进度条（那会让「还没算过」看起来像「一字未读」）。
    let total: Option<i64> = r.get(8)?;
    let unknown_chapters: i64 = r.get(9)?;
    let read_chars: Option<i64> = r.get(10)?;
    let unknown_read: i64 = r.get(11)?;
    let locator: Option<String> = r.get(12)?;
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
        progress: progress_of(
            total,
            unknown_chapters,
            read_chars,
            unknown_read,
            locator.as_deref(),
        ),
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
            "INSERT INTO books (path, title, authors, format, size, added_at, mtime)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(path) DO UPDATE SET
               title = excluded.title, authors = excluded.authors,
               format = excluded.format, size = excluded.size,
               mtime = excluded.mtime",
            params![
                path_s,
                meta.title,
                meta.authors.join("\n"),
                meta.format.as_str(),
                meta.size as i64,
                now(),
                mtime_of(path)
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

    /// 单拿这一本的封面字节（生成时按 `cover_spec` 那档规格缩过）。
    ///
    /// 为什么不在 [`Store::book`] 里一起给：`BookRow` 被桌面端整库握在手里当标签页列表用，
    /// 那一用不需要字节（见 [`BookRow`] 的说明）；而「点开一本书的信息」这一屏要的正好
    /// 只是这一本的图，一次一条语句取回，比把整库的图读进内存便宜得多。
    /// 没有封面（Markdown / TXT，或 EPUB 里没声明）时是 `None`，界面要画占位而不是空白。
    ///
    /// **磁盘优先、BLOB 兜底**（`plans/导入管线.md` §八 第 4 条）：新导入的书封面落在
    /// `<书库>/covers/<哈希>.<扩展名>`，库里那两列只是文件名；而老库（v15 之前入库、
    /// 还没被重扫过）与落盘失败的那些只有 BLOB。两条路都要走，顺序不能反 ——
    /// 文件是这一版的正主，BLOB 是历史遗留。
    pub fn book_cover(&self, id: i64) -> Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        // 两层 `Option` 各说一件事，别把它们混成一层：外层是「库里没有这一行」（书被删了），
        // 由 `.optional()` 把 `QueryReturnedNoRows` 收成 `None`；内层是「这一行的封面列是空的」
        // （Markdown / TXT，或 EPUB 里没声明），由 `get::<Option<Vec<u8>>>` 给。
        // 读到「这本书」而它没封面，与根本没这本书，界面都要画占位，所以这里摊平成一层。
        //
        // `path` 也要取：书库根从**副本自己的路径**推（副本躺在书库根上），
        // 库层因此不需要持有书库位置。
        let row: Option<CoverRow> = conn
            .query_row(
                "SELECT cover, path, cover_hash, cover_mime FROM books WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((blob, path, hash, mime)) = row else {
            return Ok(None);
        };
        // 1) 封面文件在，就用它。
        if let (Some(hash), Some(mime)) = (hash.as_deref(), mime.as_deref()) {
            let root = book_root(Path::new(&path));
            if let Some(found) = crate::format::cover::resolve(&root, hash, mime) {
                match std::fs::read(&found) {
                    Ok(bytes) => return Ok(Some(bytes)),
                    // 文件在但读不动（权限、坏扇区）：往下一步退，别报错 ——
                    // 库里那份字节可能还在，而「封面读不出来」不该让这一屏打不开。
                    Err(err) => tracing::warn!(
                        path = %found.display(),
                        "store: 封面文件读不动，退回库里那份：{err}"
                    ),
                }
            } else {
                // 描述在、文件不在（covers/ 被清过、或落盘那一步失败过）。
                tracing::debug!("store: 第 {id} 本的封面文件不在了，退回库里那份");
            }
        }
        // 2) 兜底：库里的 BLOB（老库，或落盘失败）。
        Ok(blob)
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
    /// 位置（`Cleanup` 档，见 `crate::task::Priority` 与 `crate::import::backfill_copies`）。
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
        Ok(Some(crate::format::cover::cover_path(
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
    /// **空集合也留着**：读者刚建的空组如果在分组视图里消失，他会以为没建成。
    /// 组名用 [`UNGROUPED`] 而不是空串，理由见那里的说明。
    pub fn collection_groups(&self) -> Result<Vec<ShelfGroup>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut groups = Vec::new();
        for c in Self::collections_in(&conn)? {
            groups.push(ShelfGroup {
                name: c.name,
                items: Self::shelf_in_collection_in(&conn, c.id)?,
            });
        }
        let loose = Self::shelf_in_no_collection_in(&conn)?;
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
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Self::shelf_in_collection_in(&conn, collection_id)
    }

    /// [`Store::shelf`] 的不上锁版本；只由已经持了锁的一方调用。
    fn shelf_in(conn: &Connection, query: &str) -> Result<Vec<ShelfItem>> {
        let trimmed = query.trim();
        let sql = format!(
            r#"SELECT {SHELF_COLUMNS} FROM books
             LEFT JOIN positions ON positions.book_id = books.id
             WHERE (?1 = ''
                    OR books.title LIKE ?2 ESCAPE '\'
                    OR books.authors LIKE ?2 ESCAPE '\')
             {SHELF_ORDER}"#
        );
        let mut stmt = conn.prepare(&sql)?;
        let pattern = like_pattern(trimmed);
        Ok(stmt
            .query_map(params![trimmed, pattern], shelf_from_row)?
            .collect::<std::result::Result<_, _>>()?)
    }

    /// [`Store::shelf_in_collection`] 的不上锁版本；只由已经持了锁的一方调用。
    fn shelf_in_collection_in(conn: &Connection, collection_id: i64) -> Result<Vec<ShelfItem>> {
        let sql = format!(
            "SELECT {SHELF_COLUMNS} FROM books
             LEFT JOIN positions ON positions.book_id = books.id
             JOIN collection_books cb ON cb.book_id = books.id
             WHERE cb.collection_id = ?1
             ORDER BY cb.added_at DESC, books.id DESC"
        );
        let mut stmt = conn.prepare(&sql)?;
        Ok(stmt
            .query_map(params![collection_id], shelf_from_row)?
            .collect::<std::result::Result<_, _>>()?)
    }

    /// 还没进任何集合的书，排序与 [`Store::shelf`] 同一条（同一个 `SHELF_ORDER` 常量）。
    /// [`Store::collection_groups`] 专属，不上锁。
    fn shelf_in_no_collection_in(conn: &Connection) -> Result<Vec<ShelfItem>> {
        let sql = format!(
            "SELECT {SHELF_COLUMNS} FROM books
             LEFT JOIN positions ON positions.book_id = books.id
             WHERE books.id NOT IN (SELECT book_id FROM collection_books)
             {SHELF_ORDER}"
        );
        let mut stmt = conn.prepare(&sql)?;
        Ok(stmt
            .query_map([], shelf_from_row)?
            .collect::<std::result::Result<_, _>>()?)
    }
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

/// 一本书的索引为什么不可信。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaleReason {
    /// 源文件不在了 —— 正文取不到，这本书读不了。
    Missing,
    /// 源文件换了（大小或修改时间对不上）—— 位置还在，指的却是别的内容。
    Changed,
    /// 索引是旧算法算的（分章规则 / 位置方案改过）。
    OldIndex,
    /// 封面是按旧规格（尺寸 / 编码）生成的（见 [`crate::format::COVER_SPEC_VERSION`]）。
    OldCover,
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
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        // 封面的两格：`length(cover)` 为 NULL = 没有封面（`cover` 那一列可能是 MB 级
        // 的字节，所以只问长度、不把整块图读出来）。
        let row: Option<(String, i64, i64, i64, Option<i64>, i64)> = conn
            .query_row(
                "SELECT path, size, mtime, index_version, length(cover), cover_spec
                   FROM books WHERE id = ?1",
                params![book_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((path, size, mtime, version, cover_len, cover_spec)) = row else {
            return Ok(None);
        };
        // 顺序即优先级：文件不在最严重（要删数据），其次才是「算法换了」与「文件换了」。
        let meta = std::fs::metadata(&path);
        let Ok(meta) = meta else {
            return Ok(Some(StaleReason::Missing));
        };
        if version != crate::format::INDEX_VERSION {
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
        // 有封面、但记下的是旧规格（或迁移前的 0）：值得重扫一次把它换成新规格。
        // 没有封面的书不受这条影响 —— 本来就没有东西要重生成。
        if cover_len.is_some() && cover_spec != i64::from(crate::format::COVER_SPEC_VERSION) {
            return Ok(Some(StaleReason::OldCover));
        }
        let _ = size;
        Ok(None)
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
