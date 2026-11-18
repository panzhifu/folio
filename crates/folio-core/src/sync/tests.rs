use std::collections::BTreeMap;

use crate::content::anchor::TextAnchor;
use crate::position::Locator;
use crate::sync::{
    AnnotationRecord, BookSnapshot, BookmarkRecord, SettingsRecord, Snapshot, Tombstone,
    TombstoneKind, merge, new_device_id, normalize_remote_dir, retimed, valid_device_id,
};
use crate::{BookSettings, BookSettingsExt, Reader};

fn annotation(uid: &str, at: i64) -> AnnotationRecord {
    AnnotationRecord {
        uid: uid.into(),
        anchor: TextAnchor::default(),
        note: "笔记".into(),
        color: "amber".into(),
        style: "highlight".into(),
        created_at: at,
        updated_at: at,
    }
}

fn bookmark(uid: &str, at: i64) -> BookmarkRecord {
    BookmarkRecord {
        uid: uid.into(),
        anchor: TextAnchor::default(),
        created_at: at,
        updated_at: at,
    }
}

fn tombstone(kind: TombstoneKind, uid: &str, at: i64) -> Tombstone {
    Tombstone {
        kind,
        uid: uid.into(),
        deleted_at: at,
    }
}

/// 一份只盖了字号的差量，连同「这台设备什么时候写的」。
///
/// 用真的 `record_reader` 造而不是手抄 JSON：同步带的是整本差量，样件走的就是
/// 生产那一条换算，差量算法改坏时这里跟着红。
fn settings(at: i64, font_size: f32) -> SettingsRecord {
    let base = Reader::default();
    let mut wanted = base.clone();
    wanted.font_size = font_size;
    let mut patch = BookSettings::default();
    patch.record_reader(&base, &wanted);
    SettingsRecord {
        updated_at: at,
        settings: patch,
    }
}

/// 一份「已经清干净」的差量（`Some(空)`，不是 `None`）。
fn cleared(at: i64) -> SettingsRecord {
    SettingsRecord {
        updated_at: at,
        settings: BookSettings::default(),
    }
}

fn with_book(mut snapshot: Snapshot, hash: &str, book: BookSnapshot) -> Snapshot {
    snapshot.books.insert(hash.into(), book);
    snapshot
}

fn position(offset: usize, at: i64) -> crate::sync::PositionRecord {
    crate::sync::PositionRecord {
        mode: "paged".into(),
        locator: Locator { chapter: 0, offset },
        updated_at: at,
    }
}

/// 远端更新的一页进度与一条批注会写回本机；本机更旧的那份不覆盖远端。
#[test]
fn newer_remote_records_win_and_are_planned_locally() {
    let local = with_book(
        Snapshot::new("dev-local", 10),
        "hash",
        BookSnapshot {
            position: Some(position(10, 10)),
            annotations: vec![annotation("a", 10)],
            ..BookSnapshot::default()
        },
    );
    let remote = with_book(
        Snapshot::new("dev-remote", 20),
        "hash",
        BookSnapshot {
            position: Some(position(20, 20)),
            annotations: vec![annotation("a", 20), annotation("b", 20)],
            ..BookSnapshot::default()
        },
    );

    let merged = merge(&local, &[remote], 30);
    assert_eq!(merged.stats.positions, 1);
    assert_eq!(merged.stats.annotations, 2);
    assert_eq!(
        merged.snapshot.books["hash"]
            .position
            .as_ref()
            .unwrap()
            .locator
            .offset,
        20
    );
    assert_eq!(merged.snapshot.books["hash"].annotations.len(), 2);
    assert_eq!(merged.plan.positions.len(), 1);
    assert_eq!(merged.plan.annotation_upserts.len(), 2);
    assert!(merged.plan.annotation_deletes.is_empty());
}

/// 远端墓碑比本机记录新：本机那条要被删掉，且墓碑继续往别的设备传。
#[test]
fn a_newer_tombstone_deletes_the_local_row() {
    let local = with_book(
        Snapshot::new("dev-local", 10),
        "hash",
        BookSnapshot {
            annotations: vec![annotation("a", 10)],
            ..BookSnapshot::default()
        },
    );
    let remote = with_book(
        Snapshot::new("dev-remote", 20),
        "hash",
        BookSnapshot {
            tombstones: vec![tombstone(TombstoneKind::Annotation, "a", 20)],
            ..BookSnapshot::default()
        },
    );

    let merged = merge(&local, &[remote], 30);
    assert_eq!(merged.stats.annotations_deleted, 1);
    assert_eq!(merged.plan.annotation_deletes.len(), 1);
    assert_eq!(merged.plan.annotation_deletes[0].uid, "a");
    assert!(merged.snapshot.books["hash"].annotations.is_empty());
    assert_eq!(merged.snapshot.books["hash"].tombstones.len(), 1);
}

/// 撤销删除把 updated_at 抬到删除之后，于是本机记录压过远端旧墓碑，不会被再删一次。
#[test]
fn a_restored_row_beats_an_older_tombstone() {
    let local = with_book(
        Snapshot::new("dev-local", 40),
        "hash",
        BookSnapshot {
            annotations: vec![annotation("a", 40)],
            ..BookSnapshot::default()
        },
    );
    let remote = with_book(
        Snapshot::new("dev-remote", 20),
        "hash",
        BookSnapshot {
            tombstones: vec![tombstone(TombstoneKind::Annotation, "a", 20)],
            ..BookSnapshot::default()
        },
    );

    let merged = merge(&local, &[remote], 50);
    assert!(merged.plan.annotation_deletes.is_empty());
    assert_eq!(merged.snapshot.books["hash"].annotations.len(), 1);
    assert!(merged.snapshot.books["hash"].tombstones.is_empty());
}

/// 书签按 updated_at 合并；撤销拔除同样压过旧墓碑。
#[test]
fn bookmarks_merge_on_updated_at_and_follow_tombstones() {
    let local = with_book(
        Snapshot::new("dev-local", 10),
        "hash",
        BookSnapshot {
            bookmarks: vec![bookmark("left", 10), bookmark("restored", 40)],
            ..BookSnapshot::default()
        },
    );
    let remote = with_book(
        Snapshot::new("dev-remote", 20),
        "hash",
        BookSnapshot {
            bookmarks: vec![bookmark("right", 20)],
            tombstones: vec![
                tombstone(TombstoneKind::Bookmark, "left", 20),
                tombstone(TombstoneKind::Bookmark, "restored", 20),
            ],
            ..BookSnapshot::default()
        },
    );

    let merged = merge(&local, &[remote], 50);
    assert_eq!(merged.stats.bookmarks, 1);
    assert_eq!(merged.stats.bookmarks_deleted, 1);
    assert_eq!(merged.plan.bookmark_upserts[0].1.uid, "right");
    assert_eq!(merged.plan.bookmark_deletes[0].uid, "left");
    let uids: Vec<&str> = merged.snapshot.books["hash"]
        .bookmarks
        .iter()
        .map(|bookmark| bookmark.uid.as_str())
        .collect();
    assert_eq!(uids, ["restored", "right"]);
}

/// 远端有、本机没有的书不动：记录留在远端，等这本书被导入后再同步。
#[test]
fn remote_books_missing_locally_are_left_alone() {
    let local = Snapshot::new("dev-local", 10);
    let remote = with_book(
        Snapshot::new("dev-remote", 20),
        "not-here",
        BookSnapshot {
            annotations: vec![annotation("a", 20)],
            ..BookSnapshot::default()
        },
    );

    let merged = merge(&local, &[remote], 30);
    assert!(merged.snapshot.books.is_empty());
    assert!(merged.plan.annotation_upserts.is_empty());
}

/// 同一秒里远端一台删、一台留：删除优先，不让同秒复活。
#[test]
fn a_remote_tombstone_wins_a_same_second_tie_against_remote_record() {
    let local = with_book(
        Snapshot::new("dev-local", 10),
        "hash",
        BookSnapshot::default(),
    );
    let a = with_book(
        Snapshot::new("dev-a", 20),
        "hash",
        BookSnapshot {
            annotations: vec![annotation("a", 20)],
            ..BookSnapshot::default()
        },
    );
    let b = with_book(
        Snapshot::new("dev-b", 20),
        "hash",
        BookSnapshot {
            tombstones: vec![tombstone(TombstoneKind::Annotation, "a", 20)],
            ..BookSnapshot::default()
        },
    );

    let merged = merge(&local, &[a, b], 30);
    assert!(merged.snapshot.books["hash"].annotations.is_empty());
    assert_eq!(merged.snapshot.books["hash"].tombstones.len(), 1);
}

#[test]
fn device_ids_are_safe_file_names_and_differ() {
    let a = new_device_id();
    let b = new_device_id();
    assert_eq!(a.len(), 32);
    assert!(valid_device_id(&a), "{a}");
    assert_ne!(a, b);
    assert!(!valid_device_id(""));
    assert!(!valid_device_id("a/b"));
    assert!(!valid_device_id("设备"));
}

#[test]
fn remote_dir_is_normalized_and_rejects_parent_segments() {
    assert_eq!(
        normalize_remote_dir(" /folio//sync/ ").unwrap(),
        "folio/sync"
    );
    assert_eq!(normalize_remote_dir("").unwrap(), "");
    assert!(normalize_remote_dir("folio/../别的").is_err());
}

#[test]
fn a_snapshot_round_trips_through_json() {
    let snapshot = with_book(
        Snapshot::new("dev", 10),
        "hash",
        BookSnapshot {
            position: Some(position(3, 10)),
            annotations: vec![annotation("a", 10)],
            bookmarks: vec![bookmark("b", 10)],
            tombstones: vec![tombstone(TombstoneKind::Annotation, "c", 10)],
            // 新字段也要真走一遍 JSON：不填它等于没测到它写得出去读得回来。
            settings: Some(settings(10, 26.0)),
        },
    );
    let bytes = serde_json::to_vec(&snapshot).unwrap();
    let back: Snapshot = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(back, snapshot);
    assert!(bytes.len() < 4096);
}

/// 阅读设置按**整本差量**比时间：远端更新的写回本机，远端更旧的留着本机那份。
#[test]
fn newer_remote_settings_win_whole_patch_while_older_ones_are_dropped() {
    let local = with_book(
        Snapshot::new("dev-local", 10),
        "hash",
        BookSnapshot {
            settings: Some(settings(20, 19.0)),
            ..BookSnapshot::default()
        },
    );
    let newer = with_book(
        Snapshot::new("dev-remote", 20),
        "hash",
        BookSnapshot {
            settings: Some(settings(30, 26.0)),
            ..BookSnapshot::default()
        },
    );
    let merged = merge(&local, std::slice::from_ref(&newer), 40);
    assert_eq!(merged.plan.settings.len(), 1, "更新的远端差量该写回本机");
    assert_eq!(merged.stats.settings, 1);
    assert_eq!(
        merged.snapshot.books["hash"]
            .settings
            .as_ref()
            .unwrap()
            .settings,
        settings(30, 26.0).settings,
        "上传的那份该是赢了的那个"
    );

    let older = with_book(
        Snapshot::new("dev-remote", 20),
        "hash",
        BookSnapshot {
            settings: Some(settings(5, 17.0)),
            ..BookSnapshot::default()
        },
    );
    let merged = merge(&local, std::slice::from_ref(&older), 40);
    assert!(merged.plan.settings.is_empty(), "更旧的远端差量不该盖本机");
    assert_eq!(
        merged.snapshot.books["hash"]
            .settings
            .as_ref()
            .unwrap()
            .settings,
        settings(20, 19.0).settings,
        "本机那份该留着"
    );
}

/// 「恢复本书默认」同步得出去：`Some(空差量)` 带着更新的时间要盖掉旧的差量。
///
/// 这一条盯 `Option<SettingsRecord>` 的两义性 —— `None` 是「这台设备没话说」，
/// `Some(空)` 是「这台设备把它清干净了」。把后者当没话说丢掉，症状是读者在另一台
/// 设备上看到「我明明清过了，那套设置又回来了」。
#[test]
fn a_cleared_patch_beats_an_older_non_empty_one() {
    let local = with_book(
        Snapshot::new("dev-local", 10),
        "hash",
        BookSnapshot {
            settings: Some(cleared(30)),
            ..BookSnapshot::default()
        },
    );
    let remote = with_book(
        Snapshot::new("dev-remote", 20),
        "hash",
        BookSnapshot {
            settings: Some(settings(10, 26.0)),
            ..BookSnapshot::default()
        },
    );
    let merged = merge(&local, std::slice::from_ref(&remote), 40);
    assert!(merged.plan.settings.is_empty(), "远端更旧，不该往本机写");
    let uploaded = merged.snapshot.books["hash"].settings.clone().unwrap();
    assert!(
        uploaded.settings.is_empty(),
        "上传的那份该是「已经清干净」，而不是把那两格又带回去"
    );
    assert_eq!(uploaded.updated_at, 30);
}

#[test]
fn an_empty_book_snapshot_omits_its_fields() {
    let mut books = BTreeMap::new();
    books.insert("hash".to_string(), BookSnapshot::default());
    let snapshot = Snapshot {
        version: 1,
        device_id: "dev".into(),
        exported_at: 10,
        books,
    };
    let json = serde_json::to_string(&snapshot).unwrap();
    assert!(json.contains("\"books\":{\"hash\":{}}"), "{json}");
}

#[test]
fn retiming_never_changes_record_versions_or_reorders_conflicts() {
    let a = with_book(
        Snapshot::new("a", 10),
        "hash",
        BookSnapshot {
            position: Some(position(1, 20)),
            annotations: vec![annotation("a", 30)],
            bookmarks: vec![bookmark("b", 40)],
            tombstones: vec![tombstone(TombstoneKind::Bookmark, "c", 50)],
            settings: Some(settings(60, 22.0)),
        },
    );
    let adjusted = retimed(&a, 10, 100);
    assert_eq!(adjusted.books, a.books);
    assert_eq!(adjusted.exported_at, 100);
    let mut overflow = a.clone();
    overflow.shift_clock(i64::MAX);
    assert_eq!(overflow.exported_at, i64::MAX);
    assert_eq!(overflow.books, a.books);
    let stale = retimed(&a, -5000, 9000);
    assert_eq!(stale.books, a.books);
}

#[test]
fn equal_second_conflicts_converge_across_receivers_order_and_retries() {
    let mut first_note = annotation("same", 100);
    first_note.note = "alpha".into();
    let mut second_note = first_note.clone();
    second_note.note = "zeta".into();
    let a = with_book(
        Snapshot::new("a", 100),
        "hash",
        BookSnapshot {
            position: Some(position(2, 100)),
            annotations: vec![first_note],
            settings: Some(settings(100, 18.0)),
            ..Default::default()
        },
    );
    let b = with_book(
        Snapshot::new("b", 100),
        "hash",
        BookSnapshot {
            position: Some(position(3, 100)),
            annotations: vec![second_note],
            settings: Some(settings(100, 22.0)),
            ..Default::default()
        },
    );
    let c = with_book(
        Snapshot::new("c", 100),
        "hash",
        BookSnapshot {
            tombstones: vec![tombstone(TombstoneKind::Annotation, "same", 100)],
            ..Default::default()
        },
    );
    let ab = merge(&a, std::slice::from_ref(&b), 101);
    let ba = merge(&b, std::slice::from_ref(&a), 101);
    assert_eq!(ab.snapshot.books, ba.snapshot.books);
    let abc = merge(&a, &[b.clone(), c.clone()], 102);
    let cba = merge(&c, &[b.clone(), a.clone()], 102);
    let acb = merge(&a, &[c, b], 102);
    assert_eq!(abc.snapshot.books, cba.snapshot.books);
    assert_eq!(abc.snapshot.books, acb.snapshot.books);
    assert!(abc.snapshot.books["hash"].annotations.is_empty());
    let again = merge(&abc.snapshot, &[abc.snapshot.clone(), a], 103);
    assert_eq!(again.snapshot.books, abc.snapshot.books);
    assert_eq!(again.stats.applied(), 0);
    assert!(again.plan.annotation_deletes.is_empty());
}
