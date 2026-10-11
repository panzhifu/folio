//! 书目与书架：入库、查询、删除，以及「读到哪儿了」和分组。
//!
//! 判据都围着 `books.path` 一条：**路径是书的身份**（同一路径重复导入是更新，不是
//! 第二本书），所以查、删、跳过都按它走。
//!
//! 分组查询（[`Store::shelf_grouped`] / [`Store::collection_groups`]）也住在这里：
//! 它们不是另一张表的事，只是同一个书架换一种摆法。

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
    /// 同一路径重复导入是更新，不是第二本书。
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
        let sql = format!(
            "SELECT {BOOK_COLUMNS} FROM books
             ORDER BY last_opened_at IS NULL, last_opened_at DESC, added_at DESC"
        );
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

    pub fn book_by_path(&self, path: &Path) -> Result<Option<BookRow>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let sql = format!("SELECT {BOOK_COLUMNS} FROM books WHERE path = ?1");
        let s = path.to_string_lossy().into_owned();
        Ok(conn
            .query_row(&sql, params![s], book_from_row)
            .optional()?)
    }

    pub fn remove_book(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let rows = conn
            .execute("DELETE FROM books WHERE id = ?1", params![id])?;
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
             WHERE (?1 = ''
                    OR title LIKE ?2 ESCAPE '\'
                    OR authors LIKE ?2 ESCAPE '\')
             ORDER BY last_opened_at IS NULL, last_opened_at DESC, added_at DESC"#
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
             JOIN collection_books cb ON cb.book_id = books.id
             WHERE cb.collection_id = ?1
             ORDER BY cb.added_at DESC, books.id DESC"
        );
        let mut stmt = conn.prepare(&sql)?;
        Ok(stmt
            .query_map(params![collection_id], shelf_from_row)?
            .collect::<std::result::Result<_, _>>()?)
    }

    /// 还没进任何集合的书，排序与 [`Store::shelf`] 同一条。
    /// [`Store::collection_groups`] 专属，不上锁。
    fn shelf_in_no_collection_in(conn: &Connection) -> Result<Vec<ShelfItem>> {
        let sql = format!(
            "SELECT {SHELF_COLUMNS} FROM books
             WHERE id NOT IN (SELECT book_id FROM collection_books)
             ORDER BY last_opened_at IS NULL, last_opened_at DESC, added_at DESC"
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

/// 一本书的索引为什么不可信。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaleReason {
    /// 源文件不在了 —— 正文取不到，这本书读不了。
    Missing,
    /// 源文件换了（大小或修改时间对不上）—— 位置还在，指的却是别的内容。
    Changed,
    /// 索引是旧算法算的（分章规则 / 位置方案改过）。
    OldIndex,
}

impl Store {
    /// 这本书记下的索引还可信吗？`None` = 可信。
    ///
    /// 正文只在文件里（见 `plans/text-out-of-db.md`），所以「文件还是那份文件吗」从
    /// 锦上添花变成了**能不能读**的前提。三种不可信各自有后果：
    ///
    /// - 文件不在 → 这一章取不到，重扫也没用；
    /// - 文件换了 → 位置指到别的内容上，进度与批注也跟着漂；
    /// - 索引旧了 → 序号（`Section(n)`）按旧规则算的，可能整体错位。
    ///
    /// 前两种可以直接删数据（`prune_missing`），第三种该重扫 —— 调用方选。
    pub fn stale_reason(&self, book_id: i64) -> Result<Option<StaleReason>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let row: Option<(String, i64, i64, i64)> = conn
            .query_row(
                "SELECT path, size, mtime, index_version FROM books WHERE id = ?1",
                params![book_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((path, size, mtime, version)) = row else {
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
