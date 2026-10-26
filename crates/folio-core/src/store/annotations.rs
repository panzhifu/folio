//! 批注：锚点 JSON 的往返、笔记与颜色、画法、找不回来的标记。
//!
//! 存储层只存**已经算好的锚点**，不参与坐标换算（那要有排版，见 `crate::content::anchor`）。

use rusqlite::{OptionalExtension, params};

use super::types::{ANNOTATION_COLORS, Annotation, AnnotationCard, AnnotationStyle};
use super::{Store, now};
use crate::Result;
use crate::content::anchor::TextAnchor;

/// [`Store::annotation`] 一次从库里读回来的形状：
/// `(锚点 JSON, 书 id, stale, 创建, 更新, 笔记, 颜色, 画法)`。
///
/// 起这个名字是因为「画法」那一格加上来之后，第八元素让 clippy 的
/// `type_complexity` 直接报错。而它报得对：裸的八个元组在这里既读不出哪一格是什么，
/// 换一次 `SELECT` 的顺序也不会被编译器抓住 —— 颜色与画法同为 `String`，错位照样编得过。
type AnnotationRow = (String, i64, i64, i64, i64, String, String, String);

impl Store {
    /// 记一条标注。`anchor` 是调用方从可见文本坐标建好的（见 `crate::content::anchor`），
    /// 存储层不参与坐标换算 —— 它不知道排版，也不该知道。
    ///
    /// 颜色不认识时回落到默认那条，而不是拒写：写失败会让读者刚划的批注直接丢掉，
    /// 那比颜色不对严重得多。画法这一档由 [`AnnotationStyle`] 本身兜（它是枚举，
    /// 递不进来一个不存在的值）。
    ///
    /// **章只在 `anchor_json` 里，表里没有第二个地方记它**（v13 之前那一列
    /// `chapter_id` 是外键挂到 `chapters(id)` 上的，重扫一次就被级联删光，
    /// 见 `store/schema.rs` 的 v13）。所以这一条语句不查任何别的表：
    /// 批注能不能活下来与章表当时有没有那一行彻底无关。
    pub fn add_annotation(
        &self,
        book_id: i64,
        anchor: &TextAnchor,
        note: &str,
        color: &str,
        style: AnnotationStyle,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let stamp = now();
        conn.execute(
            "INSERT INTO annotations (book_id, anchor_json, note, color, style, stale, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?6)",
            params![
                book_id,
                anchor.to_json(),
                note,
                color_of(color),
                style.as_str(),
                stamp
            ],
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
            "SELECT id, book_id, anchor_json, note, color, style, stale, created_at, updated_at
             FROM annotations WHERE book_id = ?1 ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map(params![book_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)? != 0,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, book, json, note, color, style, stale, created_at, updated_at) = row?;
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
                style: AnnotationStyle::parse(&style),
                stale,
                created_at,
                updated_at,
            });
        }
        Ok(out)
    }

    pub fn annotation(&self, id: i64) -> Result<Option<Annotation>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let row: Option<AnnotationRow> = conn
            .query_row(
                "SELECT anchor_json, book_id, stale, created_at, updated_at, note, color, style
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
                        r.get(7)?,
                    ))
                },
            )
            .optional()?;
        Ok(row.and_then(
            |(json, book_id, stale, created_at, updated_at, note, color, style)| {
                TextAnchor::from_json(&json).map(|anchor| Annotation {
                    id,
                    book_id,
                    chapter: anchor.chapter,
                    anchor,
                    note,
                    color,
                    style: AnnotationStyle::parse(&style),
                    stale: stale != 0,
                    created_at,
                    updated_at,
                })
            },
        ))
    }

    /// 改批注文字、颜色与画法。`updated_at` 跟着走 —— 界面的「最近改动」排序用它。
    ///
    /// **这是整条覆盖**：三格都要带齐当时那一格的值。只想改颜色而不带画法（或反过来），
    /// 就会把读者另一格改的东西悄悄抹回默认 —— 换色与换画法两个动作共用这一条语句，
    /// 各自都要先从内存里那份读出另一格的原值再递进来。
    pub fn update_annotation(
        &self,
        id: i64,
        note: &str,
        color: &str,
        style: AnnotationStyle,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "UPDATE annotations SET note = ?2, color = ?3, style = ?4, updated_at = ?5 WHERE id = ?1",
            params![id, note, color_of(color), style.as_str(), now()],
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

    /// 库里一共有几条批注。统计那一屏要的那个数，不需要把批注本身读出来。
    ///
    /// 不拿 [`Store::annotations`] 的返回值去 `.len()`：那个入口会把每一条的锚点 JSON
    /// 解析一遍，而统计只要一个数。
    pub fn annotation_count(&self) -> Result<i64> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Ok(conn.query_row("SELECT count(*) FROM annotations", [], |r| r.get(0))?)
    }

    pub fn delete_annotation(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute("DELETE FROM annotations WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// 全书库的批注，跨书带书名，按创建时间倒序 —— 「笔记」那一屏最上面该是最近的。
    ///
    /// 与 [`Store::annotations`] 同一条坏数据口径：解不出锚点的行静默跳过。
    pub fn all_annotations(&self) -> Result<Vec<AnnotationCard>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT a.id, a.book_id, a.anchor_json, a.note, a.color, a.style, a.stale,
                    a.created_at, a.updated_at, b.title
             FROM annotations a JOIN books b ON b.id = a.book_id
             ORDER BY a.created_at DESC, a.id DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)? != 0,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, String>(9)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, book_id, json, note, color, style, stale, created_at, updated_at, book_title) =
                row?;
            let Some(anchor) = TextAnchor::from_json(&json) else {
                continue;
            };
            out.push(AnnotationCard {
                annotation: Annotation {
                    id,
                    book_id,
                    chapter: anchor.chapter,
                    anchor,
                    note,
                    color,
                    style: AnnotationStyle::parse(&style),
                    stale,
                    created_at,
                    updated_at,
                },
                book_title,
            });
        }
        Ok(out)
    }

    /// 撤销删除：按原样插回 —— id、时间戳、stale 都用库里那条原来的值。
    ///
    /// 撤销要还原的是「删之前的那一条」，不是「现在新划的一条」：id 变了，撤销槽之外
    /// 引着它的界面状态就会指空。`INSERT OR REPLACE` 兜的是那个几乎不会发生的边角：
    /// 删掉最大 id 的行之后立刻新划一条，SQLite 的 rowid 可能复用被删的那个号 ——
    /// 那一刻「恢复原行」压过「占住新行」是正确的小败。
    pub fn restore_annotation(&self, row: &Annotation) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT OR REPLACE INTO annotations
             (id, book_id, anchor_json, note, color, style, stale, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                row.id,
                row.book_id,
                row.anchor.to_json(),
                row.note,
                row.color,
                row.style.as_str(),
                row.stale as i64,
                row.created_at,
                row.updated_at
            ],
        )?;
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

    use crate::content::anchor::TextAnchor;
    use crate::content::format::{DocMeta, Format};
    use crate::{ANNOTATION_COLORS, AnnotationStyle, Store};

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
            .add_annotation(
                book,
                &want,
                "这一段写得真好",
                "rose",
                AnnotationStyle::default(),
            )
            .unwrap();

        let got = store.annotation(id).unwrap().expect("刚写的标注该读得回来");
        assert_eq!(got.id, id);
        assert_eq!(got.book_id, book);
        assert_eq!(
            got.anchor, want,
            "锚点必须逐字段回来，坐标与四个指纹都是数据"
        );
        // 章以锚点里的为准 —— 表里没有第二个地方记它（v13 摘掉了 `chapter_id`）。
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
            .add_annotation(
                book,
                &anchor(0, 0, 4),
                "大写与空白",
                "  ROSE  ",
                AnnotationStyle::default(),
            )
            .unwrap();
        let odd = store
            .add_annotation(
                book,
                &anchor(0, 0, 4),
                "没这个颜色",
                "chartreuse",
                AnnotationStyle::default(),
            )
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
            .add_annotation(
                book,
                &anchor(1, 10, 14),
                "初稿",
                "sky",
                AnnotationStyle::default(),
            )
            .unwrap();
        store
            .update_annotation(id, "改过的批注", "lime", AnnotationStyle::Wavy)
            .unwrap();
        let got = store.annotation(id).unwrap().expect("改过之后还在");
        assert_eq!(got.note, "改过的批注");
        assert_eq!(got.color, "lime");
    }

    /// 三档画法各写一条、各自原样读回，而且存进库的是**名字**而不是序号 ——
    /// 那一列一旦按「菜单里第几格」落库，往中间插一档就把老行读成别的画法。
    #[test]
    fn every_style_round_trips_under_its_own_name() {
        let (dir, store) = store();
        let book = store.add_book(&path("a.epub"), &meta("书甲")).unwrap();
        for (ix, style) in AnnotationStyle::ALL.iter().copied().enumerate() {
            let id = store
                .add_annotation(book, &anchor(0, ix, ix + 4), "同一条", "amber", style)
                .unwrap();
            let got = store.annotation(id).unwrap().expect("刚写的该读得回来");
            assert_eq!(got.style, style, "{} 落库后读成了另一档", style.as_str());
            // 整表那条路也得带上画法：绘制与菜单认的就是 `annotations()` 这一份，
            // 只钉单条那个入口等于没钉读者看得见的这一条。
            assert_eq!(
                store.annotations(book).unwrap()[ix].style,
                style,
                "{} 从整表读回时丢了画法",
                style.as_str()
            );
        }
        // 那一列里此刻躺着什么，用一条裸连接按库文件契约问一遍（同本模块别的测试）。
        let db = dir.path().join("folio.db");
        drop(store);
        let conn = rusqlite::Connection::open(&db).unwrap();
        let mut stmt = conn.prepare("SELECT style FROM annotations").unwrap();
        let names: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|v| v.unwrap())
            .collect();
        assert_eq!(
            names,
            vec!["highlight", "line", "wavy"],
            "库里那一列写的该是名字，按 ALL 的顺序 —— 落成序号就等于把「菜单里第几格」写进历史"
        );
    }

    /// 认不出的写法读回默认档，而不是把整条批注弄没 —— 与颜色那一条同一口径。
    /// 走的是裸连接改列：这一格没有别的入口能写出脏值（`add_annotation` 收的是枚举）。
    #[test]
    fn an_unreadable_style_name_reads_as_the_default_row_not_a_missing_one() {
        let (dir, store) = store();
        let book = store.add_book(&path("a.epub"), &meta("书甲")).unwrap();
        let id = store
            .add_annotation(
                book,
                &anchor(0, 0, 4),
                "手改过的行",
                "amber",
                AnnotationStyle::Wavy,
            )
            .unwrap();
        drop(store);
        {
            let conn = rusqlite::Connection::open(dir.path().join("folio.db")).unwrap();
            conn.execute(
                "UPDATE annotations SET style = 'dotted' WHERE id = ?1",
                rusqlite::params![id],
            )
            .unwrap();
        }
        let store = Store::open_at(&dir.path().join("folio.db")).unwrap();
        let got = store.annotation(id).unwrap().expect("脏值不该把这一条吞掉");
        assert_eq!(got.style, AnnotationStyle::default());
        assert_eq!(
            got.note, "手改过的行",
            "另一格读得到才算真的只是画法那一格兜住"
        );
    }

    /// 换画法与换颜色共用一条 `update_annotation`，而它是**整条覆盖**（note + color + style
    /// 三格一起写）。所以那两个动作各自都得先从内存里那份读出另两格的原值再递进来 ——
    /// 只递新画法、另两格填空，读者刚写的笔记当场就没了，而这在界面上看不出来。
    /// 这条钉的就是「带着另两格去改」这件事在存储层成立。
    #[test]
    fn changing_the_style_keeps_the_note_and_color_of_the_same_row() {
        let (_dir, store) = store();
        let book = store.add_book(&path("a.epub"), &meta("书甲")).unwrap();
        let id = store
            .add_annotation(
                book,
                &anchor(0, 0, 4),
                "这一段的笔记",
                "rose",
                AnnotationStyle::Line,
            )
            .unwrap();
        let row = store.annotation(id).unwrap().unwrap();
        store
            .update_annotation(id, &row.note, &row.color, AnnotationStyle::Wavy)
            .unwrap();
        let got = store.annotation(id).unwrap().unwrap();
        assert_eq!(got.style, AnnotationStyle::Wavy, "改的那一格要落下去");
        assert_eq!(got.note, "这一段的笔记", "笔记不该跟着画法一起没");
        assert_eq!(got.color, "rose", "颜色不该跟着画法一起没");
    }

    /// stale 是「锚点现在落不落了回去」这件事，不是读者编辑过的证据。
    /// 它跟着 `updated_at` 走的话，每次改字号都会把从没动过的批注顶到列表最前。
    #[test]
    fn marking_stale_does_not_touch_the_timestamps() {
        let (_dir, store) = store();
        let book = store.add_book(&path("a.epub"), &meta("书甲")).unwrap();
        let id = store
            .add_annotation(
                book,
                &anchor(0, 0, 4),
                "批注",
                "amber",
                AnnotationStyle::default(),
            )
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
            .add_annotation(
                book,
                &anchor(0, 0, 4),
                "一",
                "amber",
                AnnotationStyle::default(),
            )
            .unwrap();
        let second = store
            .add_annotation(
                book,
                &anchor(0, 10, 14),
                "二",
                "rose",
                AnnotationStyle::default(),
            )
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
            .add_annotation(
                a,
                &anchor(0, 0, 4),
                "甲的",
                "amber",
                AnnotationStyle::default(),
            )
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
            .add_annotation(
                book,
                &anchor(0, 0, 4),
                "读得到的",
                "amber",
                AnnotationStyle::default(),
            )
            .unwrap();
        let broken = store
            .add_annotation(
                book,
                &anchor(0, 20, 24),
                "读不到的",
                "rose",
                AnnotationStyle::default(),
            )
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

    /// **批注与章表的存亡无关**：没有章节行时记得进去，章节行被删光重插之后还在。
    ///
    /// 这条是 v13 摘掉 `annotations.chapter_id` 的正面理由，也是那条丢数据路的
    /// 最小复现 —— 旧形状下第二半会红（外键 `ON DELETE CASCADE` 把批注整批带走）。
    /// 完整的那条（`reindex` 之后批注仍在）在 `store/tests.rs`，因为那里才有文件与格式。
    #[test]
    fn an_annotation_survives_a_missing_chapter_row() {
        let (dir, store) = store();
        let book = store.add_book(&path("a.epub"), &meta("书甲")).unwrap();
        let id = store
            .add_annotation(
                book,
                &anchor(7, 0, 4),
                "没有章节行也能记",
                "amber",
                AnnotationStyle::default(),
            )
            .unwrap();

        // 当时根本没有第 7 章那一行，批注照样读得回来，章号从锚点里报。
        assert_eq!(store.annotation(id).unwrap().unwrap().chapter, 7);

        // 章表整批被重扫换掉（`save_flow` 落库的第一句就是这个形状）。
        // 从同一个库文件再开一条连接删：走的是 SQLite 自己那条路，与连接上的外键开关
        // 无关 —— 因为这一列已经不在了（v13），级联根本没有挂钩可挂。
        {
            let conn = rusqlite::Connection::open(dir.path().join("folio.db")).unwrap();
            conn.execute("DELETE FROM chapters", []).unwrap();
        }

        let left = store.annotations(book).unwrap();
        assert_eq!(left.len(), 1, "章表被清掉不该带走批注");
        assert_eq!(left[0].chapter, 7, "章号仍然从锚点里报");
    }

    /// 删书要连带删掉它的批注 —— 这一条走的是 schema 里的 `ON DELETE CASCADE`，
    /// 但级联只在连接开了外键时才生效，所以必须实测。
    #[test]
    fn removing_a_book_takes_its_annotations_with_it() {
        let (_dir, store) = store();
        let a = store.add_book(&path("a.epub"), &meta("书甲")).unwrap();
        let b = store.add_book(&path("b.epub"), &meta("书乙")).unwrap();
        store
            .add_annotation(
                a,
                &anchor(0, 0, 4),
                "甲的",
                "amber",
                AnnotationStyle::default(),
            )
            .unwrap();
        store
            .add_annotation(
                b,
                &anchor(0, 0, 4),
                "乙的",
                "amber",
                AnnotationStyle::default(),
            )
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

    /// 「笔记」那一屏的入口：跨书、带书名、按创建时间倒序。
    #[test]
    fn the_cross_book_entry_lists_every_annotation_with_its_title_newest_first() {
        let (_dir, store) = store();
        let a = store.add_book(&path("a.epub"), &meta("书甲")).unwrap();
        let b = store.add_book(&path("b.epub"), &meta("书乙")).unwrap();
        store
            .add_annotation(
                a,
                &anchor(0, 0, 4),
                "早的",
                "amber",
                AnnotationStyle::default(),
            )
            .unwrap();
        // 同秒创建按 id 定序（ORDER BY created_at DESC, id DESC）。
        store
            .add_annotation(
                b,
                &anchor(0, 0, 4),
                "晚一",
                "rose",
                AnnotationStyle::default(),
            )
            .unwrap();
        store
            .add_annotation(
                b,
                &anchor(1, 0, 4),
                "晚二",
                "sky",
                AnnotationStyle::default(),
            )
            .unwrap();

        let cards = store.all_annotations().unwrap();
        let notes: Vec<&str> = cards.iter().map(|c| c.annotation.note.as_str()).collect();
        assert_eq!(
            notes,
            vec!["晚二", "晚一", "早的"],
            "新的在最前，同秒按 id 倒序"
        );
        let titles: Vec<&str> = cards.iter().map(|c| c.book_title.as_str()).collect();
        assert_eq!(titles, vec!["书乙", "书乙", "书甲"], "书名跟着每一条走");
    }

    /// 撤销删除 = 原行插回：id、笔记、时间戳、stale 都照旧，列表顺序不因一次拔插而变。
    #[test]
    fn restoring_puts_the_very_row_back() {
        let (_dir, store) = store();
        let book = store.add_book(&path("a.epub"), &meta("书甲")).unwrap();
        let first = store
            .add_annotation(
                book,
                &anchor(0, 0, 4),
                "第一条",
                "amber",
                AnnotationStyle::Highlight,
            )
            .unwrap();
        let second = store
            .add_annotation(
                book,
                &anchor(0, 10, 14),
                "第二条",
                "rose",
                AnnotationStyle::default(),
            )
            .unwrap();
        store.set_annotation_stale(first, true).unwrap();

        let row = store.annotation(first).unwrap().unwrap();
        store.delete_annotation(first).unwrap();
        assert!(store.annotation(first).unwrap().is_none());

        store.restore_annotation(&row).unwrap();
        let back = store.annotation(first).unwrap().expect("原 id 要回来");
        assert_eq!(back, row, "整行照原样回来：笔记、颜色、画法、stale、时间戳");
        assert!(back.stale);
        let ids: Vec<i64> = store
            .annotations(book)
            .unwrap()
            .iter()
            .map(|a| a.id)
            .collect();
        assert_eq!(ids, vec![first, second]);
    }
}
