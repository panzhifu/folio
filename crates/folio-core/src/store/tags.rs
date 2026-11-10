//! 阅读状态标签：一本书挂着哪几个标记，以及「清除阅读记录」这一把总闸。
//!
//! 标签是**固定四档**（folio-model 的 [`Tag`]，存名字不存序号），不是读者自建的词汇 ——
//! 自建词汇那件事库里有现成的答案：[`collections`](super::collections)。两者分开的理由
//! 是语义：分组回答「这几本归一堆」，标签回答「这一本读到哪儿了」，前者要多对多的
//! 自由名字，后者要一套人人认得的档位。
//!
//! 不发同步墓碑：标签是本机的阅读管理，同步链（`store::sync`）不认识这张表，
//! 换设备不同步标签 —— 这是刻意的取舍，不是漏了。

use std::collections::HashMap;

use rusqlite::params;

use super::Store;
use super::types::Tag;
use crate::Result;

impl Store {
    /// 一本书挂着的标签，按 [`Tag::ALL`] 的顺序（阅读的一生）排，库里那套字典序在这里归位。
    pub fn tags_of_book(&self, book_id: i64) -> Result<Vec<Tag>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Self::tags_of_book_in(&conn, book_id)
    }

    /// 把这本书的标签**整组替换**成 `tags`。空切片 = 全部摘掉。
    ///
    /// 整组写而不是逐个 toggle：标记浮层是一组开关，读者一口气点完才算定，
    /// 一条事务写一遍比来回 N 趟往返少一半的库锁。幂等由主键 `(book_id, tag)` 保。
    pub fn set_book_tags(&self, book_id: i64, tags: &[Tag]) -> Result<()> {
        let mut conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM book_tags WHERE book_id = ?1", params![book_id])?;
        for tag in tags {
            tx.execute(
                "INSERT INTO book_tags (book_id, tag) VALUES (?1, ?2)",
                params![book_id, tag.as_str()],
            )?;
        }
        tx.commit()?;
        let names: Vec<&str> = tags.iter().map(|tag| tag.as_str()).collect();
        tracing::debug!("store: 第 {book_id} 本的标签写成 {names:?}");
        Ok(())
    }

    /// 整架的标签表：`book_id → 挂着的标签`。书架重查时**一次**取全给卡片画角标，
    /// 逐卡再查一次等于几百本就是几百趟。
    ///
    /// 键只含**挂着至少一个标签**的书：没挂的那本查无此键，界面按「没有角标」处理
    /// —— HashMap 的缺省就是这一格的正确答案，不值得为它存空 `Vec`。
    pub fn tags_map(&self) -> Result<HashMap<i64, Vec<Tag>>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Self::tags_map_in(&conn)
    }

    /// 把这本书的阅读痕迹清干净：进度、已读章、全部会话与「最近打开」。
    ///
    /// 读者点「清除阅读记录」要的是这张白纸 —— 长按菜单里那一格的语义就是「当作
    /// 从来没读过」。批注与书签**不算**阅读痕迹（那是读者写下来的东西），一概不碰。
    ///
    /// 不发同步墓碑，局限要明说：另一台设备上还留着这本书的进度与统计，
    /// 下一次同步会把它们**原样合并回来** —— 与「删书不给批注发墓碑」是同一条立场
    /// （[`Store::remove_book`] 的文档），只是这里清的东西更少，回来的也更少。
    pub fn clear_reading_history(&self, book_id: i64) -> Result<()> {
        let mut conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM positions WHERE book_id = ?1", params![book_id])?;
        tx.execute(
            "DELETE FROM chapter_read WHERE book_id = ?1",
            params![book_id],
        )?;
        tx.execute(
            "DELETE FROM reading_sessions WHERE book_id = ?1",
            params![book_id],
        )?;
        tx.execute(
            "UPDATE books SET last_opened_at = NULL WHERE id = ?1",
            params![book_id],
        )?;
        tx.commit()?;
        tracing::info!("store: 第 {book_id} 本的阅读记录已清空（进度、已读章、会话、最近打开）");
        Ok(())
    }

    /// [`Self::tags_of_book`] 的不上锁版本；只由已经持了锁的一方调用。
    pub(super) fn tags_of_book_in(conn: &rusqlite::Connection, book_id: i64) -> Result<Vec<Tag>> {
        let mut stmt = conn.prepare("SELECT tag FROM book_tags WHERE book_id = ?1 ORDER BY tag")?;
        let mut tags: Vec<Tag> = stmt
            .query_map(params![book_id], |r| r.get::<_, String>(0))?
            .filter_map(|name| name.ok())
            .filter_map(|name| Tag::parse(&name))
            .collect();
        tags.sort_by_key(|tag| {
            Tag::ALL
                .iter()
                .position(|known| known == tag)
                .unwrap_or(usize::MAX)
        });
        Ok(tags)
    }

    /// [`Self::tags_map`] 的不上锁版本；只由已经持了锁的一方调用。
    pub(super) fn tags_map_in(conn: &rusqlite::Connection) -> Result<HashMap<i64, Vec<Tag>>> {
        let mut stmt = conn.prepare("SELECT book_id, tag FROM book_tags ORDER BY book_id, tag")?;
        let mut map: HashMap<i64, Vec<Tag>> = HashMap::new();
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (book_id, name) = row?;
            // 认不出的名字直接跳过（老版本的键、手改坏的键），与 `tags_of_book_in` 同一处置。
            if let Some(tag) = Tag::parse(&name) {
                map.entry(book_id).or_default().push(tag);
            }
        }
        Ok(map)
    }
}

#[cfg(test)]
mod tests {
    //! 标签的往返、整组替换、清除阅读记录清到哪几处，以及级联。

    use std::path::{Path, PathBuf};

    use crate::Store;
    use crate::content::format::{DocMeta, Format};
    use folio_model::Tag;

    fn meta(title: &str) -> DocMeta {
        DocMeta {
            title: title.into(),
            authors: vec!["作者甲".into()],
            format: Format::Epub,
            size: 1,
            ..DocMeta::default()
        }
    }

    fn path(name: &str) -> PathBuf {
        Path::new("/books").join(name)
    }

    fn book(store: &Store, name: &str) -> i64 {
        store.add_book(&path(name), &meta(name)).unwrap()
    }

    /// 挂上再读回，顺序归到 `Tag::ALL` 那条线上 —— 库里按名字存、按字典序出，
    /// 界面要的是「阅读的一生」那条序。
    #[test]
    fn tags_round_trip_in_reading_order() {
        let store = Store::in_memory().unwrap();
        let a = book(&store, "a.epub");
        store
            .set_book_tags(a, &[Tag::Finished, Tag::Unread, Tag::Reading])
            .unwrap();

        let got = store.tags_of_book(a).unwrap();
        assert_eq!(
            got,
            vec![Tag::Unread, Tag::Reading, Tag::Finished],
            "按 Tag::ALL 的序归位，不按写入序也不按字典序"
        );
        assert!(
            store.tags_map().unwrap().contains_key(&a),
            "tags_map 里要有这一本"
        );
    }

    /// 整组替换：上一次挂的整组让位，空切片 = 全部摘掉。
    #[test]
    fn setting_tags_replaces_the_whole_set() {
        let store = Store::in_memory().unwrap();
        let a = book(&store, "a.epub");
        store.set_book_tags(a, &[Tag::Reading]).unwrap();
        store
            .set_book_tags(a, &[Tag::Finished, Tag::Abandoned])
            .unwrap();
        assert_eq!(
            store.tags_of_book(a).unwrap(),
            vec![Tag::Finished, Tag::Abandoned],
            "上一次的 Reading 不该残留"
        );

        store.set_book_tags(a, &[]).unwrap();
        assert!(store.tags_of_book(a).unwrap().is_empty(), "空切片就是全摘");
        assert!(
            !store.tags_map().unwrap().contains_key(&a),
            "一本不挂标签的书不该在 tags_map 里占一个空位"
        );
    }

    /// 重复挂同一组标签不报错也不出重复行（整组替换先删后插，主键兜底）。
    #[test]
    fn setting_the_same_set_twice_leaves_one_row_per_tag() {
        let store = Store::in_memory().unwrap();
        let a = book(&store, "a.epub");
        let set = [Tag::Reading, Tag::Unread];
        store.set_book_tags(a, &set).unwrap();
        store.set_book_tags(a, &set).unwrap();
        assert_eq!(store.tags_of_book(a).unwrap().len(), 2);
    }

    /// 删书带走它的标签（`book_tags` 上那条级联）。
    #[test]
    fn removing_a_book_clears_its_tags() {
        let store = Store::in_memory().unwrap();
        let a = book(&store, "a.epub");
        store.set_book_tags(a, &[Tag::Reading]).unwrap();
        store.remove_book(a).unwrap();
        assert!(store.tags_of_book(a).unwrap().is_empty());
    }

    /// 清除阅读记录清到哪几处：进度、已读章、会话、最近打开；**不碰**批注与标签
    /// —— 那是读者写下来或亲手挂的，不是「读到哪儿了」的痕迹。
    #[test]
    fn clearing_reading_history_wipes_progress_but_not_annotations() {
        let store = Store::in_memory().unwrap();
        let a = book(&store, "a.epub");

        // 灌满痕迹：进度一行、已读章、两次会话、最近打开、一条批注、一个标签。
        store
            .set_position_at(a, "paged", &crate::Locator::start(), 12345)
            .unwrap();
        store.mark_chapter_read(a, 3).unwrap();
        store.record_session(a, 100, 200, 500).unwrap();
        store.record_session(a, 300, 480, 300).unwrap();
        store.touch_opened(a).unwrap();
        store
            .add_annotation(
                a,
                &crate::TextAnchor::default(),
                "一条笔记",
                "amber",
                crate::AnnotationStyle::Highlight,
            )
            .unwrap();
        store.set_book_tags(a, &[Tag::Reading]).unwrap();

        store.clear_reading_history(a).unwrap();

        assert!(store.position(a).unwrap().is_none(), "进度要没");
        assert!(store.read_chapters(a).unwrap().is_empty(), "已读章要没");
        assert!(
            store.sessions(a, 10).unwrap().is_empty(),
            "会话要连根没（统计页与热力图一起清零）"
        );
        assert_eq!(
            store.book(a).unwrap().unwrap().last_opened_at,
            None,
            "「最近打开」要没 —— 架面那条序回到导入时的样子"
        );
        assert_eq!(
            store.annotations(a).unwrap().len(),
            1,
            "批注是读者写的，不清"
        );
        assert_eq!(
            store.tags_of_book(a).unwrap(),
            vec![Tag::Reading],
            "标签是读者挂的，不清"
        );
    }

    /// 标签筛选取的是「挂着这一个的所有书」：一本挂两个标签不出两行，序照旧最近在先，
    /// 搜索词照常叠在上面。没挂任何标签的那本不出现。
    #[test]
    fn shelf_by_tag_returns_each_book_once_in_shelf_order() {
        let store = Store::in_memory().unwrap();
        let a = book(&store, "a.epub");
        let b = book(&store, "b.epub");
        book(&store, "丙.epub");
        store
            .set_book_tags(a, &[Tag::Reading, Tag::Unread])
            .unwrap();
        store.set_book_tags(b, &[Tag::Reading]).unwrap();

        let got = store.shelf_by_tag(Tag::Reading, "").unwrap();
        assert_eq!(
            got.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![b, a],
            "挂了 Reading 的两本各出一行，序照旧最近在先（同秒入库后落库在先）；丙没挂标签，不出现"
        );
        assert_eq!(
            store.shelf_by_tag(Tag::Reading, "丙").unwrap().len(),
            0,
            "搜索词叠在标签之上"
        );
        assert_eq!(
            store.shelf_by_tag(Tag::Abandoned, "").unwrap().len(),
            0,
            "没人挂的标签筛出来是空的"
        );
    }

    /// 标签的键认不出就跳过：老版本写进库的坏键不该在界面上冒出一档说不清的东西。
    /// 文件库 + 裸连接按**库文件契约**插一行坏键，再走公开 API 读。
    #[test]
    fn an_unknown_tag_name_is_skipped_on_read() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("folio.db");
        let store = Store::open_at(&db).unwrap();
        let a = book(&store, "a.epub");
        drop(store);

        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute(
                "INSERT INTO book_tags (book_id, tag) VALUES (?1, 'archived')",
                rusqlite::params![a],
            )
            .unwrap();
        }

        let store = Store::open_at(&db).unwrap();
        assert!(store.tags_of_book(a).unwrap().is_empty());
        assert!(
            !store.tags_map().unwrap().contains_key(&a),
            "tags_map 也跳过：没有半档说不清的标签"
        );
    }
}
