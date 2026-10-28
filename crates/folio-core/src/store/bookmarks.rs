//! 书签：某一章里的一个位置，独立于批注的存储。
//!
//! 与批注分表的理由见 `schema.rs` 的 v23。这一层与 `annotations` 的形状几乎同构，
//! 只少三格（note / color / style）与 stale 回写 —— 书签不被编辑，生存方式只有
//! 钉下、跳回、拔掉，所以也没有「最近改动」可排序。
//!
//! 与批注同一条规矩也在这里成立：**解不出锚点的行被静默跳过**（`anchor_json` 可能
//! 来自旧版本或别的机器），坏一条不该让整份书签列表打不开。

use rusqlite::{OptionalExtension, params};

use super::types::{Bookmark, BookmarkCard};
use super::{Store, now};
use crate::Result;
use crate::content::anchor::TextAnchor;

impl Store {
    /// 钉一枚书签。`anchor` 是调用方从可见文本坐标建好的（见 `crate::content::anchor`），
    /// 存储层不参与坐标换算 —— 与 [`Store::add_annotation`] 同一条分工。
    pub fn add_bookmark(&self, book_id: i64, anchor: &TextAnchor) -> Result<i64> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let stamp = now();
        conn.execute(
            "INSERT INTO bookmarks (uid, book_id, anchor_json, created_at, updated_at)
             VALUES (lower(hex(randomblob(16))), ?1, ?2, ?3, ?3)",
            params![book_id, anchor.to_json(), stamp],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// 一本书的全部书签，按钉下的顺序。
    pub fn bookmarks(&self, book_id: i64) -> Result<Vec<Bookmark>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, uid, book_id, anchor_json, created_at, updated_at
             FROM bookmarks WHERE book_id = ?1 ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map(params![book_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, uid, book_id, json, created_at, updated_at) = row?;
            let Some(anchor) = TextAnchor::from_json(&json) else {
                continue;
            };
            out.push(Bookmark {
                id,
                uid: uid.unwrap_or_default(),
                book_id,
                chapter: anchor.chapter,
                anchor,
                created_at,
                updated_at,
            });
        }
        Ok(out)
    }

    /// 全书库的书签，跨书带书名，按钉下时间倒序 —— 「笔记」那一屏最上面该是最近的。
    pub fn all_bookmarks(&self) -> Result<Vec<BookmarkCard>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT s.id, s.uid, s.book_id, s.anchor_json, s.created_at, s.updated_at, b.title
             FROM bookmarks s JOIN books b ON b.id = s.book_id
             ORDER BY s.created_at DESC, s.id DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, String>(6)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, uid, book_id, json, created_at, updated_at, book_title) = row?;
            let Some(anchor) = TextAnchor::from_json(&json) else {
                continue;
            };
            out.push(BookmarkCard {
                bookmark: Bookmark {
                    id,
                    uid: uid.unwrap_or_default(),
                    book_id,
                    chapter: anchor.chapter,
                    anchor,
                    created_at,
                    updated_at,
                },
                book_title,
            });
        }
        Ok(out)
    }

    /// 拔一枚书签，并记一条同步墓碑（理由与 [`Store::delete_annotation`] 同）。
    pub fn delete_bookmark(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let row: Option<(Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT s.uid, b.content_hash
                 FROM bookmarks s JOIN books b ON b.id = s.book_id
                 WHERE s.id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((uid, hash)) = row else {
            return Ok(());
        };
        // 同 [`Store::delete_annotation`]：两写一账，必须一笔事务。
        let tx = conn.unchecked_transaction()?;
        tx.execute("DELETE FROM bookmarks WHERE id = ?1", params![id])?;
        if let (Some(uid), Some(hash)) = (uid, hash) {
            super::sync::record_tombstone(
                &tx,
                crate::sync::TombstoneKind::Bookmark,
                &uid,
                &hash,
                now(),
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// 撤销拔除：按原行插回，`id`、`uid`、`created_at` 都不变。
    ///
    /// `updated_at` 抬到现在并清掉墓碑 —— 与 [`Store::restore_annotation`] 同一条理由：
    /// 删除可能已经同步出去，照抄旧时间戳会让下一次同步又把这一枚拔掉。
    pub fn restore_bookmark(&self, row: &Bookmark) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        // 同 [`Store::restore_annotation`]：插回与撤墓碑一笔事务。
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO bookmarks (id, uid, book_id, anchor_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                row.id,
                row.uid,
                row.book_id,
                row.anchor.to_json(),
                row.created_at,
                now()
            ],
        )?;
        tx.execute(
            "DELETE FROM sync_tombstones WHERE kind = 'bookmark' AND uid = ?1",
            params![row.uid],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// 某一处是不是已经钉过书签：先按章筛，再把锚点落回当前坐标对位置。
    ///
    /// 判据在调用方那头 —— 这一层不知道排版，只有库里那一行的原样。
    pub fn bookmark(&self, id: i64) -> Result<Option<Bookmark>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let row: Option<(Option<String>, i64, String, i64, i64)> = conn
            .query_row(
                "SELECT uid, book_id, anchor_json, created_at, updated_at
                 FROM bookmarks WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        Ok(
            row.and_then(|(uid, book_id, json, created_at, updated_at)| {
                TextAnchor::from_json(&json).map(|anchor| Bookmark {
                    id,
                    uid: uid.unwrap_or_default(),
                    book_id,
                    chapter: anchor.chapter,
                    anchor,
                    created_at,
                    updated_at,
                })
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    //! 书签存储：往返、跨书隔离、级联删除、按原行恢复、坏数据的影响面。
    //! 与 `annotations` 那组测试同一套办法。

    use std::path::{Path, PathBuf};

    use crate::Store;
    use crate::content::anchor::TextAnchor;
    use crate::content::format::{DocMeta, Format};

    fn meta(title: &str) -> DocMeta {
        DocMeta {
            title: title.into(),
            authors: vec!["作者甲".into()],
            format: Format::Epub,
            size: 4096,
            ..DocMeta::default()
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

    /// 一条有指纹的行锚点（可重排书签的形状）。
    fn line_anchor(chapter: u32, start: usize, end: usize) -> TextAnchor {
        TextAnchor {
            chapter,
            start,
            end,
            exact: "正在读的那一行".into(),
            prefix: "前面的字".into(),
            suffix: "后面的字".into(),
            path: "/html[1]/body[1]/p[1]#0".into(),
        }
    }

    #[test]
    fn a_bookmark_round_trips_through_the_database() {
        let (_dir, store) = store();
        let book = store.add_book(&path("a.epub"), &meta("书甲")).unwrap();
        let want = line_anchor(3, 120, 128);
        let id = store.add_bookmark(book, &want).unwrap();

        let got = store.bookmark(id).unwrap().expect("刚钉的书签该读得回来");
        assert_eq!(got.id, id);
        assert_eq!(got.book_id, book);
        assert_eq!(got.anchor, want, "锚点必须逐字段回来");
        assert_eq!(got.chapter, 3, "章以锚点里的为准");
        assert!(got.created_at > 0);

        let list = store.bookmarks(book).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0], got);
    }

    /// 定版式那一族的形状：点锚点，`chapter` 就是页号。
    #[test]
    fn a_point_anchor_on_a_fixed_page_round_trips() {
        let (_dir, store) = store();
        let book = store.add_book(&path("a.cbz"), &meta("漫画")).unwrap();
        let id = store.add_bookmark(book, &TextAnchor::point(17, 0)).unwrap();
        let got = store.bookmark(id).unwrap().unwrap();
        assert_eq!(got.chapter, 17, "页号就是地址");
        assert_eq!(got.anchor.exact, "", "点锚点没有选中的文字");
    }

    /// 书签只属于那一本书 —— 与批注同一条数据边界。
    #[test]
    fn bookmarks_do_not_leak_across_books() {
        let (_dir, store) = store();
        let a = store.add_book(&path("a.epub"), &meta("书甲")).unwrap();
        let b = store.add_book(&path("b.epub"), &meta("书乙")).unwrap();
        store.add_bookmark(a, &line_anchor(0, 0, 4)).unwrap();
        assert!(store.bookmarks(b).unwrap().is_empty(), "乙那儿该是空的");
        assert!(
            store.all_bookmarks().unwrap().len() == 1,
            "跨书的入口只该看见这一条"
        );
    }

    /// 删书连书签一起删 —— 级联挂在 `books` 上（不是 v13 之前那个会误伤的章外键），
    /// 但级联只在连接开了外键时才生效，所以必须实测。
    #[test]
    fn removing_a_book_takes_its_bookmarks_with_it() {
        let (_dir, store) = store();
        let a = store.add_book(&path("a.epub"), &meta("书甲")).unwrap();
        let b = store.add_book(&path("b.epub"), &meta("书乙")).unwrap();
        store.add_bookmark(a, &line_anchor(0, 0, 4)).unwrap();
        store.add_bookmark(b, &line_anchor(0, 0, 4)).unwrap();

        store.remove_book(a).unwrap();
        assert!(store.bookmarks(a).unwrap().is_empty(), "删掉的书不该留书签");
        assert_eq!(store.bookmarks(b).unwrap().len(), 1);
    }

    /// 撤销拔除 = 原行插回：id 与时间戳都照旧。恢复之后那一条还按原顺序排在原地。
    #[test]
    fn restoring_puts_the_very_row_back() {
        let (_dir, store) = store();
        let book = store.add_book(&path("a.epub"), &meta("书甲")).unwrap();
        let first = store.add_bookmark(book, &line_anchor(0, 0, 4)).unwrap();
        let second = store.add_bookmark(book, &line_anchor(0, 10, 14)).unwrap();

        let row = store.bookmark(first).unwrap().unwrap();
        store.delete_bookmark(first).unwrap();
        assert!(store.bookmark(first).unwrap().is_none());

        store.restore_bookmark(&row).unwrap();
        let back = store.bookmark(first).unwrap().expect("原 id 要回来");
        assert_eq!(back.id, row.id);
        assert_eq!(back.uid, row.uid, "撤销要沿用跨设备身份");
        assert_eq!(back.created_at, row.created_at, "钉下的时间不动");
        assert_eq!(back.anchor, row.anchor);
        assert!(
            back.updated_at >= row.updated_at,
            "撤销要把 updated_at 抬到现在，下一次同步才压得过远端墓碑"
        );
        let ids: Vec<i64> = store
            .bookmarks(book)
            .unwrap()
            .iter()
            .map(|b| b.id)
            .collect();
        assert_eq!(ids, vec![first, second], "顺序不因一次拔插而变");
    }

    /// 坏锚点只藏一条，不掀整份列表 —— 与批注同一条口径。
    #[test]
    fn one_corrupt_anchor_json_hides_that_row_not_the_whole_list() {
        let (dir, store) = store();
        let book = store.add_book(&path("a.epub"), &meta("书甲")).unwrap();
        let good = store.add_bookmark(book, &line_anchor(0, 0, 4)).unwrap();
        let broken = store.add_bookmark(book, &line_anchor(0, 20, 24)).unwrap();

        let db = dir.path().join("folio.db");
        drop(store);
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute(
                "UPDATE bookmarks SET anchor_json = '{ 这不是 JSON' WHERE id = ?1",
                rusqlite::params![broken],
            )
            .unwrap();
        }
        let store = Store::open_at(&db).unwrap();
        let list = store.bookmarks(book).unwrap();
        assert_eq!(
            list.iter().map(|b| b.id).collect::<Vec<_>>(),
            vec![good],
            "坏那一条要静默跳过"
        );
        assert!(store.bookmark(broken).unwrap().is_none(), "单条也当没有");
    }
}
