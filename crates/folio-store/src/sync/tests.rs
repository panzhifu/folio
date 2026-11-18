use std::path::Path;

use crate::{AnnotationStyle, Store};
use folio_format::{DocMeta, Format};
use folio_model::TextAnchor;
use folio_model::position::Locator;
use folio_model::reading::BookSettings;
use folio_model::sync::{
    AnnotationRecord, BookmarkRecord, DeleteRecord, Plan, PositionRecord, SettingsRecord,
    TombstoneKind,
};

#[test]
fn sync_equal_versions_persist_the_same_winner_and_local_edits_advance() {
    let (left, left_id) = store_with_book();
    let (right, right_id) = store_with_book();
    let low = PositionRecord {
        mode: "paged".into(),
        locator: Locator {
            chapter: 0,
            offset: 2,
        },
        updated_at: 20,
    };
    let high = PositionRecord {
        locator: Locator {
            chapter: 0,
            offset: 9,
        },
        ..low.clone()
    };
    for store in [&left, &right] {
        assert!(
            store
                .collect_sync("dev", 0)
                .unwrap()
                .snapshot
                .books
                .contains_key("hash-a")
        );
    }
    for record in [&low, &high, &low] {
        left.apply_sync(&Plan {
            positions: vec![("hash-a".into(), record.clone())],
            ..Default::default()
        })
        .unwrap();
    }
    for record in [&high, &low, &high] {
        right
            .apply_sync(&Plan {
                positions: vec![("hash-a".into(), record.clone())],
                ..Default::default()
            })
            .unwrap();
    }
    assert_eq!(
        left.position_with_time(left_id).unwrap(),
        right.position_with_time(right_id).unwrap()
    );
    let future = folio_base::time::now() + 3600;
    left.set_position_at(left_id, "paged", &Locator::start(), future)
        .unwrap();
    left.set_position(
        left_id,
        "paged",
        &Locator {
            chapter: 0,
            offset: 1,
        },
    )
    .unwrap();
    assert_eq!(
        left.position_with_time(left_id).unwrap().unwrap().2,
        future + 1
    );
}

#[test]
fn sync_tombstone_blocks_stale_resurrection_and_undo_beats_observed_delete() {
    let (store, book) = store_with_book();
    let id = store
        .add_annotation(
            book,
            &anchor(0, 0, 3),
            "keep",
            "amber",
            AnnotationStyle::Highlight,
        )
        .unwrap();
    let saved = store.annotation(id).unwrap().unwrap();
    let snapshot = store.collect_sync("dev", 0).unwrap().snapshot;
    let record = snapshot.books["hash-a"].annotations[0].clone();
    let future = record.updated_at + 3600;
    store
        .apply_sync(&Plan {
            annotation_deletes: vec![DeleteRecord {
                book_hash: "hash-a".into(),
                uid: record.uid.clone(),
                deleted_at: future,
            }],
            ..Default::default()
        })
        .unwrap();
    store
        .apply_sync(&Plan {
            annotation_upserts: vec![("hash-a".into(), record.clone())],
            ..Default::default()
        })
        .unwrap();
    assert!(store.annotation(id).unwrap().is_none());
    assert_eq!(
        store.collect_sync("dev", 0).unwrap().snapshot.books["hash-a"].tombstones[0].deleted_at,
        future
    );
    store.restore_annotation(&saved).unwrap();
    assert_eq!(
        store.annotation(id).unwrap().unwrap().updated_at,
        future + 1
    );
    store.delete_annotation(id).unwrap();
    assert_eq!(
        store.collect_sync("dev", 0).unwrap().snapshot.books["hash-a"].tombstones[0].deleted_at,
        future + 2
    );
}

/// 一份只盖了字号的差量。直接造那一格（`record_reader` 怎么算差量的门在
/// folio-core 的 book_settings 测试里；这里要的是「同步带过来一份差量」这个形状）。
fn font_patch(font_size: f32) -> BookSettings {
    BookSettings {
        reader: std::collections::BTreeMap::from([(
            "font_size".to_string(),
            serde_json::json!(font_size),
        )]),
        appearance: Default::default(),
    }
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

/// 传书清单只收认得出身份的：没有 `content_hash` 的行传上去对面也认不回，
/// 不存在的 id 安静跳过（多选与删除并发时会出现）。
#[test]
fn upload_books_returns_only_rows_with_an_identity() {
    let (store, book) = store_with_book();
    let unnamed = store
        .add_book(Path::new("/books/b.epub"), &meta("书乙"))
        .unwrap();
    let books = store.upload_books(&[book, unnamed, 9999]).unwrap();
    assert_eq!(books.len(), 1);
    assert_eq!(books[0].id, book);
    assert_eq!(books[0].hash, "hash-a");
    assert_eq!(books[0].path, "/books/a.epub");
    assert_eq!(books[0].title, "书甲");
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

    let future = folio_base::time::now() + 100;
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

/// 局域网同步的书架清单：只交得出**有内容身份**的那些书，一格不多一格不少。
///
/// `content_hash` 是对面认书与当 `/file` 的键的，没有它的行递过去只会让对面白开一次连接，
/// 而那一本在本机这边也不该被当成「有副本可拉」。
#[test]
fn the_lan_manifest_carries_only_books_with_a_content_identity() {
    let (store, _) = store_with_book();
    let second = store
        .add_book(Path::new("/books/b.epub"), &meta("书乙"))
        .unwrap();
    // 这一本还没算过身份（懒补的那一列可为空）。
    let unnamed = store
        .add_book(Path::new("/books/c.epub"), &meta("书丙"))
        .unwrap();

    let mut books = store.lan_manifest().unwrap();
    assert_eq!(
        books.iter().map(|b| b.hash.as_str()).collect::<Vec<_>>(),
        vec!["hash-a"],
        "没有身份的那一本不该出现在清单里"
    );
    assert_eq!(books[0].size, 4096, "对面按大小决定要不要开这一次连接");
    assert_eq!(books[0].title, "书甲", "报表要说得出是哪一本");

    store.set_content_hash(second, "hash-b", "sha256").unwrap();
    books = store.lan_manifest().unwrap();
    assert_eq!(books.len(), 2, "补上身份的那一本下一轮就该在清单里");
    assert!(books.iter().any(|book| book.hash == "hash-b"));

    // 空串也算「没有身份」：老库里那一列可能留的是空串而不是 NULL。
    store.set_content_hash(unnamed, "", "sha256").unwrap();
    assert_eq!(
        store.lan_manifest().unwrap().len(),
        2,
        "空串不是身份，它进不了清单"
    );
}
