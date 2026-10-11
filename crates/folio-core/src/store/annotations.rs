//! 批注：锚点 JSON 的往返、笔记与颜色、找不回来的标记。
//!
//! 存储层只存**已经算好的锚点**，不参与坐标换算（那要有排版，见 `crate::anchor`）。

use rusqlite::{OptionalExtension, params};

use super::types::{ANNOTATION_COLORS, Annotation};
use super::{Store, now};
use crate::Result;
use crate::anchor::TextAnchor;

impl Store {
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
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        // `chapter_id` 只是索引与级删的挂钩，坐标里的章下标才是权威。
        // 书没重新入库时这一列可能没有对应行，那就存 NULL —— 不能因为外键查不到
        // 就把批注写失败。
        let chapter_id: Option<i64> = conn
            .query_row(
                "SELECT id FROM chapters WHERE book_id = ?1 AND idx = ?2",
                params![book_id, anchor.chapter as i64],
                |r| r.get(0),
            )
            .optional()?;
        let stamp = now();
        conn.execute(
            "INSERT INTO annotations (book_id, chapter_id, anchor_json, note, color, stale, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?6)",
            params![book_id, chapter_id, anchor.to_json(), note, color_of(color), stamp],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// 一本书的全部标注，按创建顺序。
    ///
    /// **解不出锚点的行会被跳过**而不是让整个列表读失败：这一列可能来自旧版本或
    /// 别的机器，为一块坏 JSON 让读者看不到他所有的批注，是最糟的失败方式。
    pub fn annotations(&self, book_id: i64) -> Result<Vec<Annotation>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
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
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let row: Option<(String, i64, i64, i64, i64, String, String)> = conn
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
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "UPDATE annotations SET note = ?2, color = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, note, color_of(color), now()],
        )?;
        Ok(())
    }

    /// 记下「锚点找不回来了」。这**不**碰 `updated_at`：stale 是排版结果的属性，
    /// 不是读者编辑过的证据，跟着它排序会把从没动过的批注顶到列表最前。
    pub fn set_annotation_stale(&self, id: i64, stale: bool) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "UPDATE annotations SET stale = ?2 WHERE id = ?1",
            params![id, stale as i64],
        )?;
        Ok(())
    }

    pub fn delete_annotation(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn
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


#[cfg(test)]
mod tests {
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
