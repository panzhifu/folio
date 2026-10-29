//! 同步用的库读写：把本机状态收成快照、把合并结果写回。
//!
//! SQL 全在这里，`crate::sync` 只做纯数据的合并 —— 于是合并规则可以在没有库、没有网络
//! 的条件下单测。两边共享的只有快照类型。

use std::collections::{BTreeMap, HashMap};

use rusqlite::{OptionalExtension, params};

use super::annotations::color_of;
use super::{AnnotationStyle, Store};
use crate::Result;
use crate::content::anchor::TextAnchor;
use crate::infra::book_settings::BookSettings;
use crate::position::Locator;
use crate::sync::{
    AnnotationRecord, BookSnapshot, BookmarkRecord, LocalSnapshot, Plan, PositionRecord,
    SNAPSHOT_VERSION, SettingsRecord, Snapshot, Tombstone, TombstoneKind,
};

impl Store {
    /// 本机全部可同步的书。没有 `content_hash` 的书进不来 —— 那是跨设备认书的唯一钥匙；
    /// 数量记在 [`LocalSnapshot::skipped_books`] 里，设置页要说得出「有几本没同步」。
    ///
    /// 一次持锁；坏锚点 / 坏 JSON 的行走「跳过一条」而不是让整趟同步失败，与各个列表屏
    /// 同一条口径。
    pub fn collect_sync(&self, device_id: &str, exported_at: i64) -> Result<LocalSnapshot> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let skipped_books: i64 = conn.query_row(
            "SELECT count(*) FROM books WHERE content_hash IS NULL OR content_hash = ''",
            [],
            |r| r.get(0),
        )?;

        let mut tombstones: HashMap<String, Vec<Tombstone>> = HashMap::new();
        {
            let mut stmt =
                conn.prepare("SELECT kind, book_hash, uid, deleted_at FROM sync_tombstones")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?;
            for row in rows {
                let (kind, hash, uid, deleted_at) = row?;
                let Some(kind) = tombstone_kind(&kind) else {
                    tracing::warn!("store: 认不出的同步墓碑类型 {kind}，跳过");
                    continue;
                };
                tombstones.entry(hash).or_default().push(Tombstone {
                    kind,
                    uid,
                    deleted_at,
                });
            }
        }

        let book_rows: Vec<(i64, String)> = {
            let mut stmt = conn.prepare(
                "SELECT id, content_hash FROM books
                 WHERE content_hash IS NOT NULL AND content_hash <> ''",
            )?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };

        let mut books = BTreeMap::new();
        for (book_id, hash) in book_rows {
            let mut book = BookSnapshot::default();

            if let Some((mode, json, updated_at)) = conn
                .query_row(
                    "SELECT mode, locator_json, updated_at FROM positions WHERE book_id = ?1",
                    params![book_id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get(2)?)),
                )
                .optional()?
            {
                book.position = Some(PositionRecord {
                    mode,
                    locator: Locator::from_json(&json),
                    updated_at,
                });
            }

            // 这一本的阅读设置差量。从来没写过（时间戳 0）就不带这一格 —— 它在快照里
            // 没话说；写过之后哪怕差量已经空了也要带，那说的是「这一本恢复过默认」，
            // 不带的另一台设备会拿着旧差量继续赢。
            let written: Option<(String, i64)> = conn
                .query_row(
                    "SELECT settings_json, settings_updated_at FROM books WHERE id = ?1",
                    params![book_id],
                    |r| Ok((r.get::<_, String>(0)?, r.get(1)?)),
                )
                .optional()?
                .filter(|(_, at)| *at > 0);
            if let Some((json, updated_at)) = written {
                book.settings = Some(SettingsRecord {
                    updated_at,
                    settings: BookSettings::from_column(&json),
                });
            }

            {
                let mut stmt = conn.prepare(
                    "SELECT uid, anchor_json, note, color, style, created_at, updated_at
                     FROM annotations WHERE book_id = ?1",
                )?;
                let rows = stmt.query_map(params![book_id], |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, i64>(6)?,
                    ))
                })?;
                for row in rows {
                    let (uid, json, note, color, style, created_at, updated_at) = row?;
                    let Some(uid) = uid.filter(|uid| !uid.is_empty()) else {
                        continue;
                    };
                    let Some(anchor) = TextAnchor::from_json(&json) else {
                        continue;
                    };
                    book.annotations.push(AnnotationRecord {
                        uid,
                        anchor,
                        note,
                        color,
                        style,
                        created_at,
                        updated_at,
                    });
                }
            }

            {
                let mut stmt = conn.prepare(
                    "SELECT uid, anchor_json, created_at, updated_at
                     FROM bookmarks WHERE book_id = ?1",
                )?;
                let rows = stmt.query_map(params![book_id], |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                })?;
                for row in rows {
                    let (uid, json, created_at, updated_at) = row?;
                    let Some(uid) = uid.filter(|uid| !uid.is_empty()) else {
                        continue;
                    };
                    let Some(anchor) = TextAnchor::from_json(&json) else {
                        continue;
                    };
                    book.bookmarks.push(BookmarkRecord {
                        uid,
                        anchor,
                        created_at,
                        updated_at,
                    });
                }
            }

            if let Some(mut list) = tombstones.remove(&hash) {
                list.sort_by(sort_tombstones);
                book.tombstones = list;
            }
            book.annotations.sort_by(|a, b| a.uid.cmp(&b.uid));
            book.bookmarks.sort_by(|a, b| a.uid.cmp(&b.uid));

            if book.position.is_some()
                || !book.annotations.is_empty()
                || !book.bookmarks.is_empty()
                || !book.tombstones.is_empty()
                || book.settings.is_some()
            {
                books.insert(hash, book);
            }
        }

        Ok(LocalSnapshot {
            snapshot: Snapshot {
                version: SNAPSHOT_VERSION,
                device_id: device_id.to_string(),
                exported_at,
                books,
            },
            skipped_books: skipped_books as usize,
        })
    }

    /// 把合并结果写回本机。整批一个事务：写了一半就失败时不会留下「进度到了、
    /// 但批注没到」的半套状态。
    pub fn apply_sync(&self, plan: &Plan) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.unchecked_transaction()?;

        for (hash, record) in &plan.positions {
            let Some(book_id) = book_id(&tx, hash)? else {
                tracing::warn!("store: 同步进度找不到本地书 {hash}，跳过");
                continue;
            };
            tx.execute(
                "INSERT INTO positions (book_id, mode, locator_json, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(book_id) DO UPDATE SET
                   mode = excluded.mode, locator_json = excluded.locator_json,
                   updated_at = excluded.updated_at
                 WHERE excluded.updated_at > positions.updated_at",
                params![
                    book_id,
                    record.mode,
                    record.locator.to_json(),
                    record.updated_at
                ],
            )?;
        }

        for (hash, record) in &plan.settings {
            let Some(book_id) = book_id(&tx, hash)? else {
                tracing::warn!("store: 同步阅读设置找不到本地书 {hash}，跳过");
                continue;
            };
            // 时间守卫与进度、批注那几条同一写法：更旧的那一份不该盖掉更新的。
            tx.execute(
                "UPDATE books
                    SET settings_json = ?2, settings_updated_at = ?3
                  WHERE id = ?1 AND ?3 > settings_updated_at",
                params![book_id, record.settings.to_column(), record.updated_at],
            )?;
        }

        for (hash, record) in &plan.annotation_upserts {
            let Some(book_id) = book_id(&tx, hash)? else {
                tracing::warn!("store: 同步批注找不到本地书 {hash}，跳过");
                continue;
            };
            let color = color_of(&record.color);
            let style = AnnotationStyle::parse(&record.style);
            tx.execute(
                "INSERT INTO annotations
                 (book_id, uid, anchor_json, note, color, style, stale, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8)
                 ON CONFLICT(uid) DO UPDATE SET
                   book_id = excluded.book_id, anchor_json = excluded.anchor_json,
                   note = excluded.note, color = excluded.color, style = excluded.style,
                   created_at = excluded.created_at, updated_at = excluded.updated_at
                 WHERE excluded.updated_at > annotations.updated_at",
                params![
                    book_id,
                    record.uid,
                    record.anchor.to_json(),
                    record.note,
                    color,
                    style.as_str(),
                    record.created_at,
                    record.updated_at
                ],
            )?;
            tx.execute(
                "DELETE FROM sync_tombstones WHERE kind = 'annotation' AND uid = ?1",
                params![record.uid],
            )?;
        }

        for delete in &plan.annotation_deletes {
            // 本机在采集之后又改过这一条时，记录已经新过墓碑，不能删。
            tx.execute(
                "DELETE FROM annotations WHERE uid = ?1 AND updated_at <= ?2",
                params![delete.uid, delete.deleted_at],
            )?;
            write_tombstone(&tx, TombstoneKind::Annotation, delete)?;
        }

        for (hash, record) in &plan.bookmark_upserts {
            let Some(book_id) = book_id(&tx, hash)? else {
                tracing::warn!("store: 同步书签找不到本地书 {hash}，跳过");
                continue;
            };
            tx.execute(
                "INSERT INTO bookmarks (id, uid, book_id, anchor_json, created_at, updated_at)
                 VALUES (NULL, ?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(uid) DO UPDATE SET
                   book_id = excluded.book_id, anchor_json = excluded.anchor_json,
                   created_at = excluded.created_at, updated_at = excluded.updated_at
                 WHERE excluded.updated_at > bookmarks.updated_at",
                params![
                    record.uid,
                    book_id,
                    record.anchor.to_json(),
                    record.created_at,
                    record.updated_at
                ],
            )?;
            tx.execute(
                "DELETE FROM sync_tombstones WHERE kind = 'bookmark' AND uid = ?1",
                params![record.uid],
            )?;
        }

        for delete in &plan.bookmark_deletes {
            tx.execute(
                "DELETE FROM bookmarks WHERE uid = ?1 AND updated_at <= ?2",
                params![delete.uid, delete.deleted_at],
            )?;
            write_tombstone(&tx, TombstoneKind::Bookmark, delete)?;
        }

        tx.commit()?;
        Ok(())
    }
}

fn tombstone_kind(value: &str) -> Option<TombstoneKind> {
    match value {
        "annotation" => Some(TombstoneKind::Annotation),
        "bookmark" => Some(TombstoneKind::Bookmark),
        _ => None,
    }
}

fn tombstone_kind_str(kind: TombstoneKind) -> &'static str {
    match kind {
        TombstoneKind::Annotation => "annotation",
        TombstoneKind::Bookmark => "bookmark",
    }
}

fn sort_tombstones(a: &Tombstone, b: &Tombstone) -> std::cmp::Ordering {
    let rank = |kind| match kind {
        TombstoneKind::Annotation => 0u8,
        TombstoneKind::Bookmark => 1,
    };
    (rank(a.kind), a.uid.as_str()).cmp(&(rank(b.kind), b.uid.as_str()))
}

fn book_id(conn: &rusqlite::Connection, hash: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM books WHERE content_hash = ?1",
            params![hash],
            |r| r.get(0),
        )
        .optional()?)
}

fn write_tombstone(
    conn: &rusqlite::Connection,
    kind: TombstoneKind,
    delete: &crate::sync::DeleteRecord,
) -> Result<()> {
    record_tombstone(
        conn,
        kind,
        &delete.uid,
        &delete.book_hash,
        delete.deleted_at,
    )
}

/// 记一条墓碑。**写它的有三处**（删批注、删书签、同步合并带进来的删除），全走这一个
/// 函数：分头写就会有一处先漏掉下面那条「迟到的墓碑不许把删除时间改早」。
pub(crate) fn record_tombstone(
    conn: &rusqlite::Connection,
    kind: TombstoneKind,
    uid: &str,
    book_hash: &str,
    deleted_at: i64,
) -> Result<()> {
    if uid.is_empty() || book_hash.is_empty() {
        // 没有 `uid` 或没有内容哈希的行**传不到任何地方**（跨设备靠这两样认身份），
        // 记一条对不上号的墓碑只会让对端把「这里删过一件不存在的事」读成删除。
        return Ok(());
    }
    // `WHERE excluded.deleted_at > 现有那条` 是 ST-3 那一刀：一条迟到的旧墓碑（另一台
    // 设备离线时带的旧时间）把 `deleted_at` 改**早**之后，合并时那条更早的活跃记录就能
    // 翻盘 —— 症状是读者删掉的东西过几天自己回来，而两处各自都还自洽。
    conn.execute(
        "INSERT INTO sync_tombstones (kind, book_hash, uid, deleted_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(kind, uid) DO UPDATE SET
           book_hash = excluded.book_hash, deleted_at = excluded.deleted_at
         WHERE excluded.deleted_at > sync_tombstones.deleted_at",
        params![tombstone_kind_str(kind), book_hash, uid, deleted_at],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::content::anchor::TextAnchor;
    use crate::content::format::{DocMeta, Format};
    use crate::position::Locator;
    use crate::store::{AnnotationStyle, Store};
    use crate::sync::{
        AnnotationRecord, BookmarkRecord, DeleteRecord, Plan, PositionRecord, SettingsRecord,
        TombstoneKind,
    };
    use crate::{BookSettings, Reader};

    /// 一份只盖了字号的差量。用生产的 `record_reader` 造，不在测试里手抄 JSON。
    fn font_patch(font_size: f32) -> BookSettings {
        let base = Reader::default();
        let mut wanted = base.clone();
        wanted.font_size = font_size;
        let mut settings = BookSettings::default();
        settings.record_reader(&base, &wanted);
        settings
    }

    fn meta(title: &str) -> DocMeta {
        DocMeta {
            title: title.into(),
            format: Format::Epub,
            size: 4096,
            ..DocMeta::default()
        }
    }

    fn anchor(chapter: u32, start: usize, end: usize) -> TextAnchor {
        TextAnchor {
            chapter,
            start,
            end,
            exact: "正文".into(),
            prefix: String::new(),
            suffix: String::new(),
            path: String::new(),
        }
    }

    fn store_with_book() -> (Store, i64) {
        let store = Store::in_memory().unwrap();
        let book = store
            .add_book(Path::new("/books/a.epub"), &meta("书甲"))
            .unwrap();
        store.set_content_hash(book, "hash-a", "sha256").unwrap();
        (store, book)
    }

    #[test]
    fn collect_round_trips_progress_annotations_and_bookmarks() {
        let (store, book) = store_with_book();
        store
            .set_position(
                book,
                "paged",
                &Locator {
                    chapter: 1,
                    offset: 100,
                },
            )
            .unwrap();
        let annotation = store
            .add_annotation(
                book,
                &anchor(0, 0, 4),
                "一",
                "amber",
                AnnotationStyle::Highlight,
            )
            .unwrap();
        let bookmark = store.add_bookmark(book, &anchor(0, 10, 10)).unwrap();

        let local = store.collect_sync("dev", 100).unwrap();
        assert_eq!(local.skipped_books, 0);
        let book_snapshot = &local.snapshot.books["hash-a"];
        let position = book_snapshot.position.as_ref().unwrap();
        assert_eq!(position.mode, "paged");
        assert_eq!(position.locator.offset, 100);
        assert!(position.updated_at > 0);
        assert_eq!(book_snapshot.annotations.len(), 1);
        assert!(!book_snapshot.annotations[0].uid.is_empty());
        assert_eq!(book_snapshot.bookmarks.len(), 1);
        assert!(!book_snapshot.bookmarks[0].uid.is_empty());

        // 两条本地 id 只是用来确认它们确实写进了快照，不参与跨设备身份。
        assert!(annotation > 0);
        assert!(bookmark > 0);
    }

    #[test]
    fn apply_writes_remote_records_and_propagates_deletes() {
        let (store, book) = store_with_book();
        store
            .add_annotation(
                book,
                &anchor(0, 0, 4),
                "本机",
                "amber",
                AnnotationStyle::default(),
            )
            .unwrap();
        let local = store.collect_sync("dev", 100).unwrap();
        let local_uid = local.snapshot.books["hash-a"].annotations[0].uid.clone();

        let future = crate::infra::time::now() + 100;
        let plan = Plan {
            positions: vec![(
                "hash-a".into(),
                PositionRecord {
                    mode: "scroll".into(),
                    locator: Locator {
                        chapter: 2,
                        offset: 7,
                    },
                    updated_at: future,
                },
            )],
            annotation_upserts: vec![(
                "hash-a".into(),
                AnnotationRecord {
                    uid: "remote-a".into(),
                    anchor: anchor(1, 5, 9),
                    note: "远".into(),
                    color: "rose".into(),
                    style: "wavy".into(),
                    created_at: future,
                    updated_at: future,
                },
            )],
            annotation_deletes: vec![DeleteRecord {
                book_hash: "hash-a".into(),
                uid: local_uid.clone(),
                deleted_at: future + 1,
            }],
            bookmark_upserts: vec![(
                "hash-a".into(),
                BookmarkRecord {
                    uid: "remote-b".into(),
                    anchor: anchor(3, 1, 1),
                    created_at: future,
                    updated_at: future,
                },
            )],
            bookmark_deletes: Vec::new(),
            settings: vec![(
                "hash-a".into(),
                SettingsRecord {
                    updated_at: future,
                    settings: font_patch(26.0),
                },
            )],
        };
        store.apply_sync(&plan).unwrap();

        assert_eq!(
            store
                .book_settings(book)
                .unwrap()
                .reader
                .get("font_size")
                .cloned(),
            Some(serde_json::json!(26.0)),
            "同步来的整本差量该落进这一本的行"
        );
        // 更旧的那一份不许盖回来（时间守卫与进度、批注那几条同一写法）。
        store
            .apply_sync(&Plan {
                settings: vec![(
                    "hash-a".into(),
                    SettingsRecord {
                        updated_at: 1,
                        settings: font_patch(12.0),
                    },
                )],
                ..Plan::default()
            })
            .unwrap();
        assert_eq!(
            store
                .book_settings(book)
                .unwrap()
                .reader
                .get("font_size")
                .cloned(),
            Some(serde_json::json!(26.0)),
            "更旧的同步结果不该把更新的盖掉"
        );

        let (mode, locator, updated_at) = store.position_with_time(book).unwrap().unwrap();
        assert_eq!(mode, "scroll");
        assert_eq!(locator.offset, 7);
        assert_eq!(updated_at, future);
        let annotations = store.annotations(book).unwrap();
        assert_eq!(annotations.len(), 1);
        assert_eq!(annotations[0].uid, "remote-a");
        assert_eq!(annotations[0].note, "远");
        assert_eq!(annotations[0].style.as_str(), "wavy");
        let bookmarks = store.bookmarks(book).unwrap();
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].uid, "remote-b");

        let after = store.collect_sync("dev", future + 2).unwrap();
        let book_snapshot = &after.snapshot.books["hash-a"];
        assert_eq!(book_snapshot.annotations[0].uid, "remote-a");
        assert_eq!(book_snapshot.tombstones.len(), 1);
        assert_eq!(book_snapshot.tombstones[0].uid, local_uid);
        assert_eq!(book_snapshot.tombstones[0].kind, TombstoneKind::Annotation);
    }

    #[test]
    fn deleting_and_restoring_keeps_the_tombstone_in_step() {
        let (store, book) = store_with_book();
        let id = store
            .add_annotation(
                book,
                &anchor(0, 0, 4),
                "一",
                "amber",
                AnnotationStyle::default(),
            )
            .unwrap();
        let row = store.annotation(id).unwrap().unwrap();
        assert!(!row.uid.is_empty());

        store.delete_annotation(id).unwrap();
        let after_delete = store.collect_sync("dev", 100).unwrap();
        assert_eq!(after_delete.snapshot.books["hash-a"].tombstones.len(), 1);
        assert_eq!(
            after_delete.snapshot.books["hash-a"].tombstones[0].uid,
            row.uid
        );

        store.restore_annotation(&row).unwrap();
        let after_restore = store.collect_sync("dev", 200).unwrap();
        let book_snapshot = &after_restore.snapshot.books["hash-a"];
        assert_eq!(book_snapshot.annotations.len(), 1);
        assert_eq!(book_snapshot.annotations[0].uid, row.uid);
        assert!(book_snapshot.tombstones.is_empty(), "撤销要清掉墓碑");
    }

    #[test]
    fn books_without_a_content_hash_are_counted_not_synced() {
        let (store, book) = store_with_book();
        store
            .set_position(
                book,
                "paged",
                &Locator {
                    chapter: 0,
                    offset: 1,
                },
            )
            .unwrap();
        store
            .add_book(Path::new("/books/b.epub"), &meta("没有哈希"))
            .unwrap();
        let local = store.collect_sync("dev", 100).unwrap();
        assert_eq!(local.skipped_books, 1);
        assert_eq!(local.snapshot.books.len(), 1);
        assert!(local.snapshot.books.contains_key("hash-a"));
    }

    /// ST-2：**那一行与那条墓碑同生同死**。
    ///
    /// 注入办法是 SQLite 的 `RAISE`：在 `sync_tombstones` 上装一枚 `BEFORE INSERT` 触发器，
    /// 让墓碑那一写当场失败 —— 正好落在「DELETE 已成、墓碑还没记」的那一步。没有事务的
    /// 那一版：批注行没了、墓碑没记上，跨设备那边永远不知道删过 ⇒ 下次同步它在另一台
    /// 设备上活着回来，而读者的屏上早就没有这条了。
    ///
    /// 这里不用 `Store::in_memory`：内存库的 schema 只活在那一条连接里，注入用的触发器
    /// 必须由**第二条连接**装到同一个库文件上才生效（`Store` 自己不暴露连接，走文件库）。
    #[test]
    fn a_failed_tombstone_write_rolls_back_the_delete() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("folio.db");
        let store = Store::open_at(&db).unwrap();
        let book = store
            .add_book(Path::new("/books/a.epub"), &meta("书甲"))
            .unwrap();
        store.set_content_hash(book, "hash-a", "sha256").unwrap();
        let annotation = store
            .add_annotation(
                book,
                &anchor(0, 0, 4),
                "本机",
                "amber",
                AnnotationStyle::default(),
            )
            .unwrap();
        let bookmark = store.add_bookmark(book, &anchor(0, 10, 10)).unwrap();

        let injector = rusqlite::Connection::open(&db).unwrap();
        injector
            .execute_batch(
                "CREATE TRIGGER inject_no_tombstone BEFORE INSERT ON sync_tombstones
                 BEGIN SELECT RAISE(ABORT, '注入：墓碑写不下去'); END;",
            )
            .unwrap();

        assert!(
            store.delete_annotation(annotation).is_err(),
            "注入没生效：墓碑那一写本该失败"
        );
        assert_eq!(
            store.annotations(book).unwrap().len(),
            1,
            "墓碑记不上就不该把读者的批注删掉：那一行必须原地还在"
        );
        assert!(
            store.delete_bookmark(bookmark).is_err(),
            "注入没生效：书签那条也一样"
        );
        assert_eq!(
            store.bookmarks(book).unwrap().len(),
            1,
            "删书签与记墓碑是同一笔：缺一笔就当没发生过"
        );

        // 拆掉注入之后，同一条删除要正常走通（挡住「事务包错了方向、什么时候都失败」）。
        injector
            .execute_batch("DROP TRIGGER inject_no_tombstone")
            .unwrap();
        drop(injector);
        store.delete_annotation(annotation).unwrap();
        assert!(
            store.annotations(book).unwrap().is_empty(),
            "撤了注入就该删得动"
        );
        assert_eq!(
            store.collect_sync("dev", i64::MAX).unwrap().snapshot.books["hash-a"]
                .tombstones
                .len(),
            1,
            "删成的那一条要留下墓碑（另一台设备才知道要删）"
        );
    }

    /// ST-3：**迟到的墓碑不许把删除时间改早**。
    ///
    /// 另一台设备离线时带的旧时间戳合并进来，如果无条件覆盖 `deleted_at`，删除时间被改早
    /// 之后，合并就可能让一条**更旧的活跃记录**翻盘 —— 症状是读者删掉的东西过几天自己回来。
    #[test]
    fn a_late_tombstone_never_moves_the_deletion_back() {
        let (store, book) = store_with_book();
        let uid = store
            .add_annotation(
                book,
                &anchor(0, 0, 4),
                "本机",
                "amber",
                AnnotationStyle::default(),
            )
            .map(|_| ())
            .and_then(|_| {
                store
                    .collect_sync("dev", i64::MAX)
                    .map(|local| local.snapshot.books["hash-a"].annotations[0].uid.clone())
            })
            .unwrap();
        store
            .delete_annotation(
                store
                    .annotations(book)
                    .unwrap()
                    .first()
                    .map(|row| row.id)
                    .unwrap(),
            )
            .unwrap();
        let kept = || {
            store.collect_sync("dev", i64::MAX).unwrap().snapshot.books["hash-a"]
                .tombstones
                .iter()
                .find(|tomb| tomb.uid == uid)
                .map(|tomb| tomb.deleted_at)
        };
        let local = kept().expect("删成的那一条该有墓碑");

        // 迟到的一条（时间更早）合并进来：不许动。
        store
            .apply_sync(&Plan {
                annotation_deletes: vec![DeleteRecord {
                    book_hash: "hash-a".into(),
                    uid: uid.clone(),
                    deleted_at: local - 50,
                }],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            kept(),
            Some(local),
            "迟到的旧墓碑把删除时间改早了：下一次合并会让那条更旧的活跃记录翻盘"
        );

        // 而更新的一条照常推进（不许把 WHERE 写成永远不成立）。
        store
            .apply_sync(&Plan {
                annotation_deletes: vec![DeleteRecord {
                    book_hash: "hash-a".into(),
                    uid: uid.clone(),
                    deleted_at: local + 50,
                }],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(kept(), Some(local + 50), "更新的一条该收下");
    }
}
