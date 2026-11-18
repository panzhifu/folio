use std::path::Path;
use std::sync::Arc;

use folio_format::document::{Document, from_content};
use folio_format::{Chapter, ChapterExt as _, Content, DocMeta, FlowDoc, Format};

use crate::{AdoptResult, ImportRegistration, RevisionFence, RevisionReady, Store};

fn document(title: &str, chapters: usize) -> Box<dyn Document> {
    from_content(
        DocMeta {
            title: title.into(),
            authors: vec!["A".into(), "B".into()],
            format: Format::Markdown,
            size: chapters as u64,
            description: "full metadata".into(),
            ..Default::default()
        },
        Content::Reflow(FlowDoc {
            chapters: (0..chapters)
                .map(|i| Chapter::inline(format!("Chapter {i}"), format!("<p>text {i}</p>")))
                .collect(),
            ..Default::default()
        }),
    )
}

fn register(store: &Store) -> crate::BookRevision {
    store
        .register_import(ImportRegistration {
            book_uuid: "book-uuid",
            doc: document("original", 2).as_ref(),
            managed_path: Path::new("private/version-1.md"),
            library_root: Path::new("private"),
            content_hash: "hash-1",
            resource_hash: "",
            resource_manifest: "[]",
            source_locator: Some("/original/book.md"),
            source_kind: "local",
            source_fingerprint: "hash-1",
        })
        .unwrap()
}

fn ready(store: &Store, id: i64, hash: &str, generation: i64) -> crate::BookRevision {
    let fence = store.reserve_revision(id).unwrap();
    let source = store.lookup_source("/original/book.md").unwrap().unwrap();
    store
        .ready_revision(RevisionReady {
            book_id: id,
            fence,
            expected_active: Some(store.active_revision(id).unwrap().unwrap().fence),
            source_id: Some(source.id),
            source_generation: generation,
            content_hash: hash,
            resource_hash: "",
            managed_path: Path::new(hash),
            library_root: Path::new("private"),
            resource_manifest: "[]",
            doc: document(hash, 1).as_ref(),
        })
        .unwrap()
}

#[test]
fn revisions_prepare_independently_and_adoption_obeys_reader_leases_and_fences() {
    let store = Arc::new(Store::in_memory().unwrap());
    let first = register(&store);
    store
        .set_book_meta(first.book_id, "reader title", &["reader author".into()])
        .unwrap();
    let session = store.acquire_revision(first.book_id).unwrap();
    assert_eq!(session.snapshot.meta.title, "reader title");
    assert_eq!(
        session.snapshot.content.as_flow().unwrap().chapters.len(),
        2
    );
    let candidate = ready(&store, first.book_id, "hash-2", 0);
    assert_eq!(
        store
            .load_content(first.book_id)
            .unwrap()
            .unwrap()
            .as_flow()
            .unwrap()
            .chapters
            .len(),
        2
    );
    assert_eq!(
        store
            .adopt_revision(first.book_id, candidate.fence, Some(first.fence), false)
            .unwrap(),
        AdoptResult::ReadersActive
    );
    drop(session);
    assert_eq!(
        store
            .adopt_revision(first.book_id, candidate.fence, Some(first.fence), false)
            .unwrap(),
        AdoptResult::Adopted
    );
    assert_eq!(
        store
            .adopt_revision(first.book_id, candidate.fence, Some(first.fence), false)
            .unwrap(),
        AdoptResult::AlreadyActive
    );
    let next = store.acquire_revision(first.book_id).unwrap();
    assert_eq!(next.snapshot.revision.book_uuid, first.book_uuid);
    assert_eq!(next.snapshot.meta.title, "reader title");
    assert_eq!(next.snapshot.meta.authors, ["reader author"]);
    assert_eq!(next.snapshot.content.as_flow().unwrap().chapters.len(), 1);
    assert_eq!(
        store
            .revision_snapshot(first.book_id, first.fence)
            .unwrap()
            .content
            .as_flow()
            .unwrap()
            .chapters
            .len(),
        2
    );
    let conn = store.conn.lock().unwrap();
    assert!(crate::revisions::check_revision_in(&conn, first.book_id, first.fence).is_err());
    assert!(crate::revisions::check_revision_in(&conn, first.book_id, candidate.fence).is_ok());
}

#[test]
fn source_generations_block_late_candidates_and_suppression_survives_book_removal() {
    let store = Store::in_memory().unwrap();
    let first = register(&store);
    let source = store.lookup_source("/original/book.md").unwrap().unwrap();
    let newer = store.mark_source_dirty(source.id).unwrap();
    assert!(
        store
            .observe_source_at(source.id, source.generation, "old")
            .unwrap()
            .is_none()
    );
    let observed = store
        .observe_source_at(source.id, newer.generation, "new")
        .unwrap()
        .unwrap();
    let candidate = ready(&store, first.book_id, "hash-2", observed.generation);
    store.mark_source_dirty(source.id).unwrap();
    assert_eq!(
        store
            .adopt_revision(first.book_id, candidate.fence, Some(first.fence), false)
            .unwrap(),
        AdoptResult::Stale
    );
    store.suppress_book_sources(first.book_id).unwrap();
    assert!(store.sources_for_reconcile().unwrap().is_empty());
    store.remove_book(first.book_id).unwrap();
    assert!(store.source_is_suppressed("/original/book.md").unwrap());
}

#[test]
fn new_registration_rolls_back_all_rows_and_rejects_cross_book_identity_conflicts() {
    let store = Store::in_memory().unwrap();
    {
        let conn = store.conn.lock().unwrap();
        conn.execute_batch("CREATE TRIGGER refuse_chapters BEFORE INSERT ON chapters BEGIN SELECT RAISE(ABORT,'no'); END;").unwrap();
    }
    let result = store.register_import(ImportRegistration {
        book_uuid: "book-uuid",
        doc: document("first", 1).as_ref(),
        managed_path: Path::new("private/version.md"),
        library_root: Path::new("private"),
        content_hash: "hash-1",
        resource_hash: "",
        resource_manifest: "[]",
        source_locator: Some("/original/book.md"),
        source_kind: "local",
        source_fingerprint: "hash-1",
    });
    assert!(result.is_err());
    assert!(store.list_books().unwrap().is_empty());
    assert!(store.lookup_source("/original/book.md").unwrap().is_none());
    {
        let conn = store.conn.lock().unwrap();
        conn.execute_batch("DROP TRIGGER refuse_chapters").unwrap();
    }
    let first = register(&store);
    let conflict = store.register_import(ImportRegistration {
        book_uuid: "other",
        doc: document("other", 1).as_ref(),
        managed_path: Path::new("private/other.md"),
        library_root: Path::new("private"),
        content_hash: "hash-1",
        resource_hash: "",
        resource_manifest: "[]",
        source_locator: None,
        source_kind: "local",
        source_fingerprint: "",
    });
    assert!(conflict.is_err());
    assert_eq!(
        store.books_for_content_hash("hash-1").unwrap(),
        [first.book_id]
    );
    assert!(
        store
            .bind_source(first.book_id + 1, "/original/book.md", "local", true)
            .is_err()
    );
}

#[test]
fn legacy_migration_keeps_registered_identity_without_reading_or_trusting_missing_bytes() {
    let store = Arc::new(Store::in_memory().unwrap());
    let id = store
        .add_book(Path::new("/absent/book.md"), document("legacy", 2).meta())
        .unwrap();
    store
        .save_document(id, document("legacy", 2).as_ref())
        .unwrap();
    store
        .set_content_hash(id, "old-registered-hash", "sha256")
        .unwrap();
    let legacy = store.active_revision(id).unwrap().unwrap();
    assert_eq!(legacy.fence, RevisionFence::default());
    assert_eq!(legacy.state, "legacy-unverified");
    assert_eq!(legacy.content_hash, "old-registered-hash");
    assert!(!legacy.book_uuid.is_empty());
    assert!(store.acquire_revision(id).is_err());
    assert_eq!(
        store
            .revision_snapshot(id, legacy.fence)
            .unwrap()
            .content
            .as_flow()
            .unwrap()
            .chapters
            .len(),
        2
    );
}

#[test]
fn reindex_coordinates_have_a_new_fence_and_history_gc_keeps_previous_and_leased_versions() {
    let store = Arc::new(Store::in_memory().unwrap());
    let first = register(&store);
    let old_reader = store.acquire_revision(first.book_id).unwrap();
    let second = ready(&store, first.book_id, "hash-2", 0);
    assert_eq!(
        store
            .adopt_revision(first.book_id, second.fence, Some(first.fence), true)
            .unwrap(),
        AdoptResult::Adopted
    );
    let third = ready(&store, first.book_id, "hash-3", 0);
    store
        .adopt_revision(first.book_id, third.fence, Some(second.fence), true)
        .unwrap();
    assert!(store.prunable_revisions(first.book_id).unwrap().is_empty());
    assert!(
        store
            .detach_revision_file(first.book_id, second.fence)
            .unwrap()
            .is_none()
    );
    drop(old_reader);
    let backup = tempfile::tempdir().unwrap();
    let pins = store
        .backup_to_pinned(&backup.path().join("folio.db"))
        .unwrap();
    assert!(
        store.prunable_revisions(first.book_id).unwrap().is_empty(),
        "备份持有的历史不能回收"
    );
    assert!(
        store
            .detach_revision_file(first.book_id, first.fence)
            .unwrap()
            .is_none()
    );
    drop(pins);
    assert_eq!(
        store
            .prunable_revisions(first.book_id)
            .unwrap()
            .iter()
            .map(|r| r.fence)
            .collect::<Vec<_>>(),
        [first.fence]
    );
    assert!(
        store
            .detach_revision_file(first.book_id, first.fence)
            .unwrap()
            .is_some()
    );
    assert!(
        store.retain_revision(first.book_id, first.fence).is_err(),
        "已回收字节不能重新申请读取租约"
    );
    assert_eq!(
        store.books_for_content_hash("hash-1").unwrap(),
        [first.book_id]
    );
    let reindexed = store.reserve_reindex(first.book_id).unwrap();
    assert_eq!(reindexed.rev_no, third.fence.rev_no);
    assert!(reindexed.index_generation > third.fence.index_generation);
    store
        .ready_revision(RevisionReady {
            book_id: first.book_id,
            fence: reindexed,
            expected_active: Some(third.fence),
            source_id: None,
            source_generation: 0,
            content_hash: "hash-3",
            resource_hash: "",
            managed_path: Path::new("hash-3"),
            library_root: Path::new("private"),
            resource_manifest: "[]",
            doc: document("new parser", 3).as_ref(),
        })
        .unwrap();
    store
        .adopt_revision(first.book_id, reindexed, Some(third.fence), false)
        .unwrap();
    assert_eq!(
        store
            .load_content(first.book_id)
            .unwrap()
            .unwrap()
            .as_flow()
            .unwrap()
            .chapters
            .len(),
        3
    );
    let conn = store.conn.lock().unwrap();
    assert!(crate::revisions::check_revision_in(&conn, first.book_id, third.fence).is_err());
}

#[test]
fn v31_upgrade_binds_old_records_to_registered_hash_without_hashing_changed_files() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("old.db");
    let file = temp.path().join("changed.md");
    std::fs::write(&file, b"changed after old import").unwrap();
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        for sql in &crate::schema::MIGRATIONS[..31] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute("INSERT INTO books(id,path,title,format,size,added_at,content_hash) VALUES(7,?1,'old','markdown',23,1,'registered-old-hash')",[file.to_string_lossy().as_ref()]).unwrap();
        conn.execute("INSERT INTO bookmarks(id,uid,book_id,anchor_json,created_at,updated_at) VALUES(3,'old-mark',7,?1,2,2)",[folio_model::TextAnchor::point(0,2).to_json()]).unwrap();
        conn.execute(
            "INSERT INTO positions VALUES(7,'paged',?1,3)",
            [folio_model::Locator {
                chapter: 0,
                offset: 2,
            }
            .to_json()],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 31).unwrap();
    }
    let store = Store::open_at(&db).unwrap();
    let legacy = store.active_revision(7).unwrap().unwrap();
    assert_eq!(legacy.content_hash, "registered-old-hash");
    assert_eq!(legacy.state, "legacy-unverified");
    assert_eq!(legacy.fence, RevisionFence::default());
    let conn = store.conn.lock().unwrap();
    let origin:(String,i64,i64)=conn.query_row("SELECT content_hash,rev_no,index_generation FROM record_origins WHERE kind='bookmark' AND uid='old-mark'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(origin, ("registered-old-hash".into(), 0, 0));
    let pos:i64=conn.query_row("SELECT updated_at FROM revision_positions WHERE book_id=7 AND rev_no=0 AND index_generation=0",[],|r|r.get(0)).unwrap();
    assert_eq!(pos, 3);
    assert_eq!(std::fs::read(&file).unwrap(), b"changed after old import");
}

#[test]
fn failed_adoption_restores_active_index_position_and_revision_states() {
    let store = Store::in_memory().unwrap();
    let first = register(&store);
    store
        .set_position(
            first.book_id,
            "paged",
            &folio_model::Locator {
                chapter: 1,
                offset: 4,
            },
        )
        .unwrap();
    let position = store.position_with_time(first.book_id).unwrap();
    let candidate = ready(&store, first.book_id, "hash-2", 0);
    {
        let conn = store.conn.lock().unwrap();
        conn.execute_batch("CREATE TRIGGER fail_adopt BEFORE INSERT ON chapters BEGIN SELECT RAISE(ABORT,'fail replacement index'); END;").unwrap();
    }
    assert!(
        store
            .adopt_revision(first.book_id, candidate.fence, Some(first.fence), false)
            .is_err()
    );
    assert_eq!(
        store.active_revision(first.book_id).unwrap().unwrap(),
        first
    );
    assert_eq!(store.position_with_time(first.book_id).unwrap(), position);
    assert_eq!(
        store
            .load_content(first.book_id)
            .unwrap()
            .unwrap()
            .as_flow()
            .unwrap()
            .chapters
            .len(),
        2
    );
    assert_eq!(
        store.ready_revisions(first.book_id).unwrap()[0].fence,
        candidate.fence
    );
}

#[test]
fn switching_primary_source_invalidates_an_already_prepared_candidate_without_churning_idempotent_binds()
 {
    let store = Store::in_memory().unwrap();
    let first = register(&store);
    let source = store.lookup_source("/original/book.md").unwrap().unwrap();
    assert_eq!(
        store
            .bind_source(first.book_id, &source.locator, "local", true)
            .unwrap()
            .generation,
        source.generation
    );
    let candidate = ready(&store, first.book_id, "hash-2", source.generation);
    let other = store
        .bind_source(first.book_id, "/original/new.md", "local", true)
        .unwrap();
    let demoted = store.lookup_source(&source.locator).unwrap().unwrap();
    assert!(!demoted.is_primary);
    assert!(demoted.generation > source.generation);
    assert!(other.is_primary);
    assert_eq!(
        store
            .adopt_revision(first.book_id, candidate.fence, Some(first.fence), false)
            .unwrap(),
        AdoptResult::Stale
    );
    assert_eq!(
        store.active_revision(first.book_id).unwrap().unwrap().fence,
        first.fence
    );
}
#[test]
fn an_older_unbound_ready_revision_cannot_replace_a_newer_active_revision() {
    let store = Store::in_memory().unwrap();
    let first = register(&store);
    let prepare = |hash: &str| {
        let fence = store.reserve_revision(first.book_id).unwrap();
        store
            .ready_revision(RevisionReady {
                book_id: first.book_id,
                fence,
                expected_active: Some(first.fence),
                source_id: None,
                source_generation: 0,
                content_hash: hash,
                resource_hash: "",
                managed_path: Path::new(hash),
                library_root: Path::new("private"),
                resource_manifest: "[]",
                doc: document(hash, 1).as_ref(),
            })
            .unwrap()
    };
    let older = prepare("hash-2");
    let newer = prepare("hash-3");
    assert_eq!(
        store
            .ready_revisions(first.book_id)
            .unwrap()
            .iter()
            .map(|r| r.fence)
            .collect::<Vec<_>>(),
        [newer.fence, older.fence]
    );
    store
        .adopt_revision(first.book_id, newer.fence, Some(first.fence), false)
        .unwrap();
    assert_eq!(
        store
            .adopt_revision(first.book_id, older.fence, Some(newer.fence), false)
            .unwrap(),
        AdoptResult::Stale
    );
    assert!(store.ready_revisions(first.book_id).unwrap().is_empty());
    assert_eq!(
        store.active_revision(first.book_id).unwrap().unwrap().fence,
        newer.fence
    );
}
#[test]
fn deleting_the_highest_book_id_never_lets_an_old_reader_write_into_a_new_book() {
    let store = Arc::new(Store::in_memory().unwrap());
    let old = register(&store);
    let _reader = store.acquire_revision(old.book_id).unwrap();
    store.remove_book(old.book_id).unwrap();
    let new = store
        .register_import(ImportRegistration {
            book_uuid: "new-book",
            doc: document("new book", 1).as_ref(),
            managed_path: Path::new("private/new.md"),
            library_root: Path::new("private"),
            content_hash: "new-hash",
            resource_hash: "",
            resource_manifest: "[]",
            source_locator: None,
            source_kind: "local",
            source_fingerprint: "",
        })
        .unwrap();
    let old_position = store.set_position_checked(
        old.book_id,
        old.fence,
        "paged",
        &folio_model::Locator {
            chapter: 0,
            offset: 99,
        },
    );
    assert!(
        old_position.is_err(),
        "最高id被删除后，旧阅读会话不得通过相同数字id/fence写入另一张新书卡"
    );
    assert_ne!(old.book_id, new.book_id);
    assert!(store.position(new.book_id).unwrap().is_none());
}
#[test]
fn deleted_source_ids_cannot_be_reused_by_an_unrelated_book_or_old_status_callback() {
    let store = Store::in_memory().unwrap();
    let old = register(&store);
    let previous = store.lookup_source("/original/book.md").unwrap().unwrap();
    store.remove_book(old.book_id).unwrap();
    let new = store
        .register_import(ImportRegistration {
            book_uuid: "new-book",
            doc: document("new book", 1).as_ref(),
            managed_path: Path::new("private/new.md"),
            library_root: Path::new("private"),
            content_hash: "new-hash",
            resource_hash: "",
            resource_manifest: "[]",
            source_locator: Some("/different/source.md"),
            source_kind: "local",
            source_fingerprint: "new-hash",
        })
        .unwrap();
    assert!(
        !store
            .set_source_state_at(previous.id, previous.generation, "failed")
            .unwrap(),
        "旧来源回调不能修改新来源状态"
    );
    let current = store
        .lookup_source("/different/source.md")
        .unwrap()
        .unwrap();
    assert_eq!(current.book_id, new.book_id);
    assert_eq!(current.state, "available");
    assert_ne!(current.id, previous.id);
}
#[test]
fn book_id_high_water_survives_reopen_and_legacy_path_upserts_keep_the_existing_identity() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("library.db");
    let highest = {
        let store = Store::open_at(&db).unwrap();
        let book = register(&store);
        store.remove_book(book.book_id).unwrap();
        book.book_id
    };
    let store = Store::open_at(&db).unwrap();
    let legacy = store
        .add_book(Path::new("private/legacy.md"), document("legacy", 1).meta())
        .unwrap();
    assert!(legacy > highest, "关闭重开不能重用已移除的书目id");
    assert_eq!(
        store
            .add_book(
                Path::new("private/legacy.md"),
                document("changed metadata", 2).meta()
            )
            .unwrap(),
        legacy,
        "同路径兼容upsert必须沿用原id"
    );
    store.remove_book(legacy).unwrap();
    let fresh = register(&store);
    assert!(fresh.book_id > legacy);
}

#[test]
fn two_store_connections_allocate_book_ids_atomically_without_reusing_deleted_ids() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("shared.db");
    let left = Store::open_at(&db).unwrap();
    let right = Store::open_at(&db).unwrap();
    let first = left
        .add_book(Path::new("private/left.md"), document("left", 1).meta())
        .unwrap();
    left.remove_book(first).unwrap();
    let second = right
        .add_book(Path::new("private/right.md"), document("right", 1).meta())
        .unwrap();
    right.remove_book(second).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let ids = std::thread::scope(|scope| {
        let barrier_left = barrier.clone();
        let a = scope.spawn(move || {
            barrier_left.wait();
            left.add_book(Path::new("private/new-left.md"), document("left", 1).meta())
        });
        let b = scope.spawn(move || {
            barrier.wait();
            right.add_book(
                Path::new("private/new-right.md"),
                document("right", 1).meta(),
            )
        });
        [a.join().unwrap(), b.join().unwrap()]
    });
    let mut allocated: Vec<_> = ids.into_iter().map(|result| result.unwrap()).collect();
    allocated.sort();
    assert_eq!(allocated, [second + 1, second + 2]);
}
#[test]
fn permanently_stale_ready_from_an_old_source_generation_is_prunable_after_its_pin_releases() {
    let store = Store::in_memory().unwrap();
    let first = register(&store);
    let reader = store.acquire_revision(first.book_id).unwrap();
    let source = store.lookup_source("/original/book.md").unwrap().unwrap();
    let older = ready(&store, first.book_id, "hash-older-ready", source.generation);
    let pin = store.retain_revision(first.book_id, older.fence).unwrap();
    let changed = store.mark_source_dirty(source.id).unwrap();
    let newer = ready(
        &store,
        first.book_id,
        "hash-newer-ready",
        changed.generation,
    );
    assert!(
        store
            .prunable_revisions(first.book_id)
            .unwrap()
            .iter()
            .all(|r| r.fence != older.fence),
        "固定的旧候选不可回收"
    );
    assert_eq!(
        store
            .revision_snapshot(first.book_id, older.fence)
            .unwrap()
            .revision
            .state,
        "failed",
        "旧来源代的Ready必须退休，不能永久占用目录"
    );
    assert!(
        store
            .detach_revision_file(first.book_id, older.fence)
            .unwrap()
            .is_none()
    );
    for temporary in ["missing", "relink-required"] {
        store.set_source_state(source.id, temporary).unwrap();
        store.prunable_revisions(first.book_id).unwrap();
        assert_eq!(
            store
                .revision_snapshot(first.book_id, newer.fence)
                .unwrap()
                .revision
                .state,
            "ready",
            "同代暂时不可读或待重关联不是永久失效"
        );
    }
    store.set_source_state(source.id, "available").unwrap();
    assert_eq!(
        store
            .ready_revisions(first.book_id)
            .unwrap()
            .iter()
            .map(|r| r.fence)
            .collect::<Vec<_>>(),
        [newer.fence]
    );
    drop(pin);
    drop(reader);
    store
        .adopt_revision(first.book_id, newer.fence, Some(first.fence), false)
        .unwrap();
    let prunable = store.prunable_revisions(first.book_id).unwrap();
    assert!(
        prunable
            .iter()
            .any(|r| r.fence == older.fence && r.state == "failed")
    );
    assert!(
        store
            .detach_revision_file(first.book_id, older.fence)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        store
            .revision_snapshot(first.book_id, older.fence)
            .unwrap()
            .revision
            .content_hash,
        "hash-older-ready",
        "回收不删除版本身份和索引"
    );
}

#[test]
fn permanently_stale_ready_without_a_source_is_retired_only_after_a_newer_version_is_active() {
    let store = Store::in_memory().unwrap();
    let first = register(&store);
    let prepare = |hash: &str, expected| {
        let fence = store.reserve_revision(first.book_id).unwrap();
        store
            .ready_revision(RevisionReady {
                book_id: first.book_id,
                fence,
                expected_active: Some(expected),
                source_id: None,
                source_generation: 0,
                content_hash: hash,
                resource_hash: "",
                managed_path: Path::new(hash),
                library_root: Path::new("private"),
                resource_manifest: "[]",
                doc: document(hash, 1).as_ref(),
            })
            .unwrap()
    };
    let old = prepare("old-manual-ready", first.fence);
    let active = prepare("new-active", first.fence);
    assert!(store.prunable_revisions(first.book_id).unwrap().is_empty());
    assert_eq!(
        store
            .revision_snapshot(first.book_id, old.fence)
            .unwrap()
            .revision
            .state,
        "ready"
    );
    store
        .adopt_revision(first.book_id, active.fence, Some(first.fence), false)
        .unwrap();
    let latest = prepare("latest-ready", active.fence);
    let stale = store.prunable_revisions(first.book_id).unwrap();
    assert!(
        stale
            .iter()
            .any(|r| r.fence == old.fence && r.state == "failed")
    );
    assert_eq!(
        store
            .revision_snapshot(first.book_id, latest.fence)
            .unwrap()
            .revision
            .state,
        "ready"
    );
    assert_eq!(
        store.active_revision(first.book_id).unwrap().unwrap().fence,
        active.fence
    );
}
