//! 集合（分组）：新建 / 改名 / 删除，书的进出，以及与书的级联。
//!
//! 名字是读者输入的东西、跨书存在，所以它跟着库走而不是配置。

use rusqlite::{Connection, OptionalExtension, params};

use super::books::{BOOK_COLUMNS, book_from_row};
use super::types::{BookRow, Collection};
use super::{Store, now};
use crate::Result;

impl Store {
    /// 新建一个集合；**同名即复用**（`UNIQUE` 上的 `DO NOTHING` 再回查 id）。
    ///
    /// 做成幂等而不是报「名字已存在」，是照 [`Store::add_book`] 的同一套判据：
    /// 集合在用户眼里是文件夹，重复点「新建」时他要的是「进到那个文件夹」，
    /// 不是一条报错。真要拦重名，那是界面在提交前该提示的事。
    pub fn create_collection(&self, name: &str) -> Result<i64> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let name = name.trim();
        if name.is_empty() {
            return Err(crate::Error::EmptyCollectionName);
        }
        conn.execute(
            "INSERT INTO collections (name) VALUES (?1) ON CONFLICT(name) DO NOTHING",
            params![name],
        )?;
        let id: i64 = conn.query_row(
            "SELECT id FROM collections WHERE name = ?1",
            params![name],
            |r| r.get(0),
        )?;
        tracing::debug!("store: 集合「{name}」→ id {id}");
        Ok(id)
    }

    /// 全部集合，按名字排序，每条带上书数。
    pub fn collections(&self) -> Result<Vec<Collection>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Self::collections_in(&conn)
    }

    pub fn collection(&self, id: i64) -> Result<Option<Collection>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Ok(conn
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
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let name = name.trim();
        if name.is_empty() {
            return Err(crate::Error::EmptyCollectionName);
        }
        let rows = conn.execute(
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
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let rows = conn.execute("DELETE FROM collections WHERE id = ?1", params![id])?;
        if rows == 0 {
            tracing::warn!("store: 删集合 {id}，但没有这一行");
        } else {
            tracing::debug!("store: 删集合 {id}");
        }
        Ok(())
    }

    /// 把一本书放进集合。返回「这次真的放进去没有」——重复调用是 `false`，不是错。
    pub fn add_to_collection(&self, collection_id: i64, book_id: i64) -> Result<bool> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let rows = conn.execute(
            "INSERT OR IGNORE INTO collection_books (collection_id, book_id, added_at)
             VALUES (?1, ?2, ?3)",
            params![collection_id, book_id, now()],
        )?;
        Ok(rows > 0)
    }

    /// 把一本书移出集合。返回「这次真的移出了没有」。
    pub fn remove_from_collection(&self, collection_id: i64, book_id: i64) -> Result<bool> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let rows = conn.execute(
            "DELETE FROM collection_books WHERE collection_id = ?1 AND book_id = ?2",
            params![collection_id, book_id],
        )?;
        Ok(rows > 0)
    }

    /// 一本书属于哪几个集合，按名字排序。书详情页要显示「在哪些分组里」。
    pub fn collections_of_book(&self, book_id: i64) -> Result<Vec<Collection>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
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
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let sql = format!(
            "SELECT {BOOK_COLUMNS} FROM books
             JOIN collection_books cb ON cb.book_id = books.id
             WHERE cb.collection_id = ?1
             ORDER BY cb.added_at DESC, books.id DESC"
        );
        let mut stmt = conn.prepare(&sql)?;
        Ok(stmt
            .query_map(params![collection_id], book_from_row)?
            .collect::<std::result::Result<_, _>>()?)
    }

    /// [`Store::collections`] 的不上锁版本；只由已经持了锁的一方调用。
    /// 分组查询要按同一份定义取集合，所以它对 `store` 内可见。
    pub(super) fn collections_in(conn: &Connection) -> Result<Vec<Collection>> {
        let mut stmt = conn.prepare(
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
}

fn collection_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Collection> {
    Ok(Collection {
        id: r.get(0)?,
        name: r.get(1)?,
        book_count: r.get::<_, i64>(2)? as usize,
    })
}

#[cfg(test)]
mod tests {
    //! 集合（分组）：新建/改名/删除、书的进出、与书的级联。
    //!
    //! 与本文件其他测试同一套办法：只用公开 API；要看「API 之下那一列」时用裸
    //! `rusqlite` 连接按**库文件契约**查，不碰私有字段。

    use std::path::{Path, PathBuf};

    use crate::Store;
    use crate::content::format::{DocMeta, Format};

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
