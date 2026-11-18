//! 坐标写入在同一连接锁内验证版本，避免旧会话写进新版。

use folio_model::revision::RevisionFence;
use folio_model::sync::PositionRecord;
use folio_model::types::AnnotationStyle;
use folio_model::{Locator, TextAnchor};
use rusqlite::params;

use crate::record_revisions::{remember_origin_in, write_position_in};
use crate::revisions::{check_revision_in, current_revision_in};
use crate::{Result, Store, now};

impl Store {
    pub fn clear_chapter_read(&self, book_id: i64) -> Result<()> {
        self.clear_read_at(book_id, None)
    }

    pub fn clear_chapter_read_checked(&self, book_id: i64, fence: RevisionFence) -> Result<()> {
        self.clear_read_at(book_id, Some(fence))
    }

    fn clear_read_at(&self, book_id: i64, expected: Option<RevisionFence>) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let fence = current_revision_in(&conn, book_id)?;
        if let Some(expected) = expected {
            check_revision_in(&conn, book_id, expected)?;
        }
        let tx = conn.unchecked_transaction()?;
        tx.execute("DELETE FROM revision_chapter_read WHERE book_id=?1 AND rev_no=?2 AND index_generation=?3",params![book_id,fence.rev_no,fence.index_generation])?;
        tx.execute("DELETE FROM chapter_read WHERE book_id=?1", [book_id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn add_annotation(
        &self,
        book_id: i64,
        anchor: &TextAnchor,
        note: &str,
        color: &str,
        style: AnnotationStyle,
    ) -> Result<i64> {
        self.add_annotation_at(book_id, None, anchor, note, color, style)
    }

    pub fn add_annotation_checked(
        &self,
        book_id: i64,
        fence: RevisionFence,
        anchor: &TextAnchor,
        note: &str,
        color: &str,
        style: AnnotationStyle,
    ) -> Result<i64> {
        self.add_annotation_at(book_id, Some(fence), anchor, note, color, style)
    }

    fn add_annotation_at(
        &self,
        book_id: i64,
        fence: Option<RevisionFence>,
        anchor: &TextAnchor,
        note: &str,
        color: &str,
        style: AnnotationStyle,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(fence) = fence {
            check_revision_in(&conn, book_id, fence)?;
        }
        let tx = conn.unchecked_transaction()?;
        let stamp = crate::sync_order::next_version(0, now())?;
        tx.execute("INSERT INTO annotations(id,uid,book_id,anchor_json,note,color,style,stale,created_at,updated_at) VALUES((SELECT max(high_water,coalesce((SELECT max(id) FROM annotations),0))+1 FROM record_id_clocks WHERE kind='annotation'),lower(hex(randomblob(16))),?1,?2,?3,?4,?5,0,?6,?6)",params![book_id,anchor.to_json(),note,crate::annotations::color_of(color),style.as_str(),stamp])?;
        let id = tx.last_insert_rowid();
        let uid: String = tx.query_row("SELECT uid FROM annotations WHERE id=?1", [id], |r| {
            r.get(0)
        })?;
        remember_origin_in(&tx, "annotation", &uid, book_id)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn add_bookmark(&self, book_id: i64, anchor: &TextAnchor) -> Result<i64> {
        self.add_bookmark_at(book_id, None, anchor)
    }

    pub fn add_bookmark_checked(
        &self,
        book_id: i64,
        fence: RevisionFence,
        anchor: &TextAnchor,
    ) -> Result<i64> {
        self.add_bookmark_at(book_id, Some(fence), anchor)
    }

    fn add_bookmark_at(
        &self,
        book_id: i64,
        fence: Option<RevisionFence>,
        anchor: &TextAnchor,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(fence) = fence {
            check_revision_in(&conn, book_id, fence)?;
        }
        let tx = conn.unchecked_transaction()?;
        let stamp = crate::sync_order::next_version(0, now())?;
        tx.execute("INSERT INTO bookmarks(id,uid,book_id,anchor_json,created_at,updated_at) VALUES((SELECT max(high_water,coalesce((SELECT max(id) FROM bookmarks),0))+1 FROM record_id_clocks WHERE kind='bookmark'),lower(hex(randomblob(16))),?1,?2,?3,?3)",params![book_id,anchor.to_json(),stamp])?;
        let id = tx.last_insert_rowid();
        let uid: String =
            tx.query_row("SELECT uid FROM bookmarks WHERE id=?1", [id], |r| r.get(0))?;
        remember_origin_in(&tx, "bookmark", &uid, book_id)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn set_position(&self, book_id: i64, mode: &str, locator: &Locator) -> Result<()> {
        self.write_position_at(book_id, None, mode, locator, None)
    }

    pub fn set_position_checked(
        &self,
        book_id: i64,
        fence: RevisionFence,
        mode: &str,
        locator: &Locator,
    ) -> Result<()> {
        self.write_position_at(book_id, Some(fence), mode, locator, None)
    }

    /// 同步兼容入口保持来源版本，不制造本地新编辑。
    pub fn set_position_at(
        &self,
        book_id: i64,
        mode: &str,
        locator: &Locator,
        updated_at: i64,
    ) -> Result<()> {
        self.write_position_at(book_id, None, mode, locator, Some(updated_at))
    }

    fn write_position_at(
        &self,
        book_id: i64,
        fence: Option<RevisionFence>,
        mode: &str,
        locator: &Locator,
        stamp: Option<i64>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let current = current_revision_in(&conn, book_id)?;
        if let Some(fence) = fence {
            check_revision_in(&conn, book_id, fence)?;
        }
        let stamp = match stamp {
            Some(stamp) => stamp,
            None => crate::sync_order::next_version(
                crate::sync_order::position(&conn, book_id)?.map_or(0, |p| p.updated_at),
                now(),
            )?,
        };
        let tx = conn.unchecked_transaction()?;
        write_position_in(
            &tx,
            book_id,
            current,
            &PositionRecord {
                mode: mode.into(),
                locator: *locator,
                updated_at: stamp,
            },
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn mark_chapter_read(&self, book_id: i64, idx: usize) -> Result<()> {
        self.mark_read_at(book_id, None, idx)
    }

    pub fn mark_chapter_read_checked(
        &self,
        book_id: i64,
        fence: RevisionFence,
        idx: usize,
    ) -> Result<()> {
        self.mark_read_at(book_id, Some(fence), idx)
    }

    fn mark_read_at(&self, book_id: i64, fence: Option<RevisionFence>, idx: usize) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let current = current_revision_in(&conn, book_id)?;
        if let Some(fence) = fence {
            check_revision_in(&conn, book_id, fence)?;
        }
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO chapter_read(book_id,idx,read_at) VALUES(?1,?2,?3)",
            params![book_id, idx as i64, now()],
        )?;
        tx.execute("INSERT OR IGNORE INTO revision_chapter_read(book_id,rev_no,index_generation,idx,read_at) VALUES(?1,?2,?3,?4,?5)",params![book_id,current.rev_no,current.index_generation,idx as i64,now()])?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use folio_format::document::{Document, from_content};
    use folio_format::ext::ChapterExt as _;
    use folio_format::{Chapter, Content, DocMeta, FlowDoc, Format};
    use folio_model::sync::{DeleteRecord, Plan, PositionRecord};
    use folio_model::{Locator, TextAnchor};

    use crate::{AdoptResult, AnnotationStyle, ImportRegistration, RevisionReady, Store};

    fn document(text: &str) -> Box<dyn Document> {
        from_content(
            DocMeta {
                title: "Book".into(),
                format: Format::Markdown,
                ..Default::default()
            },
            Content::Reflow(FlowDoc {
                chapters: vec![Chapter::inline("One", format!("<p>{text}</p>"))],
                ..Default::default()
            }),
        )
    }

    #[test]
    fn revision_records_keep_original_buckets_and_reject_stale_writes() {
        let store = Arc::new(Store::in_memory().unwrap());
        let old_doc = document("old text");
        let old = store
            .register_import(ImportRegistration {
                book_uuid: "book-identity",
                doc: old_doc.as_ref(),
                library_root: Path::new("/library"),
                managed_path: Path::new("/library/versions/book/1/book.md"),
                content_hash: "old-hash",
                resource_hash: "",
                resource_manifest: "[]",
                source_locator: None,
                source_kind: "local",
                source_fingerprint: "",
            })
            .unwrap();
        let id = old.book_id;
        let anchor = TextAnchor {
            chapter: 0,
            start: 0,
            end: 8,
            exact: "old text".into(),
            ..Default::default()
        };
        let note_id = store
            .add_annotation_checked(
                id,
                old.fence,
                &anchor,
                "kept",
                "lime",
                AnnotationStyle::Highlight,
            )
            .unwrap();
        let note = store.annotation(note_id).unwrap().unwrap();
        let bookmark_id = store.add_bookmark_checked(id, old.fence, &anchor).unwrap();
        store
            .set_position_checked(
                id,
                old.fence,
                "paged",
                &Locator {
                    chapter: 0,
                    offset: 4,
                },
            )
            .unwrap();
        let lease = store.acquire_revision(id).unwrap();
        let next = store.reserve_revision(id).unwrap();
        let new_doc = document("prefix old text");
        store
            .ready_revision(RevisionReady {
                book_id: id,
                fence: next,
                expected_active: Some(old.fence),
                source_id: None,
                source_generation: 0,
                content_hash: "new-hash",
                resource_hash: "",
                library_root: Path::new("/library"),
                managed_path: Path::new("/library/versions/book/2/book.md"),
                resource_manifest: "[]",
                doc: new_doc.as_ref(),
            })
            .unwrap();
        assert_eq!(
            store
                .adopt_revision(id, next, Some(old.fence), false)
                .unwrap(),
            AdoptResult::ReadersActive
        );
        drop(lease);
        assert_eq!(
            store
                .adopt_revision(id, next, Some(old.fence), false)
                .unwrap(),
            AdoptResult::Adopted
        );
        assert!(
            store
                .set_position_checked(id, old.fence, "paged", &Locator::start())
                .is_err()
        );
        assert!(store.add_bookmark_checked(id, old.fence, &anchor).is_err());
        assert!(
            store
                .add_annotation_checked(
                    id,
                    old.fence,
                    &anchor,
                    "late",
                    "lime",
                    AnnotationStyle::Highlight
                )
                .is_err()
        );
        assert!(
            store.position(id).unwrap().is_none(),
            "新版不继承未经定位的旧偏移"
        );
        assert!(store.annotations_for_revision(id, next).unwrap().is_empty());
        let mut projected = anchor.clone();
        projected.start = 7;
        projected.end = 15;
        store
            .set_anchor_projection(id, "annotation", &note.uid, next, Some(&projected))
            .unwrap();
        assert_eq!(
            store.annotations_for_revision(id, next).unwrap()[0].anchor,
            projected
        );
        assert_eq!(store.annotation(note_id).unwrap().unwrap().anchor, anchor);
        store
            .set_position_checked(
                id,
                next,
                "scroll",
                &Locator {
                    chapter: 0,
                    offset: 12,
                },
            )
            .unwrap();
        let snapshot = store.collect_sync("local", 1).unwrap().snapshot;
        assert_eq!(snapshot.books["old-hash"].annotations[0].anchor, anchor);
        assert!(snapshot.books["new-hash"].annotations.is_empty());
        let mut incoming = Plan::default();
        incoming.positions.push((
            "old-hash".into(),
            PositionRecord {
                mode: "paged".into(),
                locator: Locator {
                    chapter: 0,
                    offset: 2,
                },
                updated_at: i64::MAX - 10,
            },
        ));
        store.apply_sync(&incoming).unwrap();
        assert_eq!(store.position(id).unwrap().unwrap().1.offset, 12);
        assert_eq!(
            store
                .position_for_revision(id, old.fence)
                .unwrap()
                .unwrap()
                .1
                .offset,
            2
        );
        store.delete_bookmark(bookmark_id).unwrap();
        store.delete_annotation(note_id).unwrap();
        let snapshot = store.collect_sync("local", 2).unwrap().snapshot;
        assert_eq!(snapshot.books["old-hash"].tombstones.len(), 2);
        assert!(snapshot.books["new-hash"].tombstones.is_empty());
        store.restore_annotation(&note).unwrap();
        assert_eq!(
            store.collect_sync("local", 3).unwrap().snapshot.books["old-hash"]
                .annotations
                .len(),
            1
        );
        let mut wrong = Plan::default();
        wrong.annotation_deletes.push(DeleteRecord {
            book_hash: "new-hash".into(),
            uid: note.uid,
            deleted_at: i64::MAX - 1,
        });
        assert!(store.apply_sync(&wrong).is_err());
        assert!(store.annotation(note_id).unwrap().is_some());
    }

    #[test]
    fn revision_backup_relocates_all_versions_and_requires_source_relink() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("original");
        let destination = temporary.path().join("restored");
        let snapshot = temporary.path().join("backup.sqlite");
        let store = Store::in_memory().unwrap();
        let doc = document("content");
        let old = store
            .register_import(ImportRegistration {
                book_uuid: "restore-book",
                doc: doc.as_ref(),
                library_root: &root,
                managed_path: &root.join("versions/restore-book/1/book.md"),
                content_hash: "before",
                resource_hash: "",
                resource_manifest: "[]",
                source_locator: Some("/external/author/book.md"),
                source_kind: "local",
                source_fingerprint: "initial",
            })
            .unwrap();
        let anchor = TextAnchor {
            exact: "content".into(),
            end: 7,
            ..Default::default()
        };
        store
            .add_bookmark_checked(old.book_id, old.fence, &anchor)
            .unwrap();
        let next = store.reserve_revision(old.book_id).unwrap();
        store
            .ready_revision(RevisionReady {
                book_id: old.book_id,
                fence: next,
                expected_active: Some(old.fence),
                source_id: None,
                source_generation: 0,
                content_hash: "after",
                resource_hash: "",
                library_root: &root,
                managed_path: &root.join("versions/restore-book/2/book.md"),
                resource_manifest: "[]",
                doc: doc.as_ref(),
            })
            .unwrap();
        store
            .adopt_revision(old.book_id, next, Some(old.fence), false)
            .unwrap();
        let pins = store.backup_to_pinned(&snapshot).unwrap();
        assert_eq!(pins.len(), 2);
        let restored = Store::open_at(&snapshot).unwrap();
        restored
            .relocate_backup_library(&root.to_string_lossy(), &destination)
            .unwrap();
        for revision in restored.revisions(old.book_id).unwrap() {
            assert!(Path::new(&revision.managed_path).starts_with(&destination));
            assert_eq!(revision.library_root, destination.to_string_lossy());
            assert_eq!(revision.book_uuid, "restore-book");
        }
        assert!(restored.sources_for_reconcile().unwrap().is_empty());
        assert_eq!(
            restored
                .lookup_source("/external/author/book.md")
                .unwrap()
                .unwrap()
                .state,
            "relink-required"
        );
        let exchanged = restored.collect_sync("restored", 1).unwrap().snapshot;
        assert_eq!(exchanged.books["before"].bookmarks[0].anchor, anchor);
        assert!(exchanged.books["after"].bookmarks.is_empty());
    }

    #[test]
    fn unverified_legacy_notes_never_inherit_the_new_file_hash_for_sync() {
        let store = Store::in_memory().unwrap();
        let doc = document("legacy text");
        let book = store
            .add_book(Path::new("/old/book.md"), doc.meta())
            .unwrap();
        store.save_document(book, doc.as_ref()).unwrap();
        let note = store
            .add_annotation(
                book,
                &TextAnchor::point(0, 3),
                "old note",
                "lime",
                AnnotationStyle::Highlight,
            )
            .unwrap();
        let old = store.active_revision(book).unwrap().unwrap();
        assert!(old.content_hash.is_empty());
        let next = store.reserve_revision(book).unwrap();
        let changed = document("different content");
        store
            .ready_revision(RevisionReady {
                book_id: book,
                fence: next,
                expected_active: Some(old.fence),
                source_id: None,
                source_generation: 0,
                content_hash: "new-real-hash",
                resource_hash: "",
                library_root: Path::new("/new"),
                managed_path: Path::new("/new/versions/new/book.md"),
                resource_manifest: "[]",
                doc: changed.as_ref(),
            })
            .unwrap();
        store
            .adopt_revision(book, next, Some(old.fence), false)
            .unwrap();
        let exported = store.collect_sync("local", 0).unwrap();
        assert_eq!(exported.skipped_books, 1);
        assert!(
            exported.snapshot.books["new-real-hash"]
                .annotations
                .is_empty()
        );
        assert_eq!(store.annotation(note).unwrap().unwrap().note, "old note");
        store.delete_annotation(note).unwrap();
        assert!(
            store.collect_sync("local", 1).unwrap().snapshot.books["new-real-hash"]
                .tombstones
                .is_empty()
        );
    }

    #[test]
    fn replacement_source_link_rolls_back_when_revision_token_cannot_be_committed() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("library.sqlite");
        let store = Store::open_at(&database).unwrap();
        let doc = document("old");
        let first = store
            .register_import(ImportRegistration {
                book_uuid: "source-transaction",
                doc: doc.as_ref(),
                library_root: dir.path(),
                managed_path: &dir.path().join("first.md"),
                content_hash: "first",
                resource_hash: "",
                resource_manifest: "[]",
                source_locator: Some("/old/source.md"),
                source_kind: "local",
                source_fingerprint: "first",
            })
            .unwrap();
        let fence = store.reserve_revision(first.book_id).unwrap();
        store
            .ready_revision(RevisionReady {
                book_id: first.book_id,
                fence,
                expected_active: Some(first.fence),
                source_id: None,
                source_generation: 0,
                content_hash: "second",
                resource_hash: "",
                library_root: dir.path(),
                managed_path: &dir.path().join("second.md"),
                resource_manifest: "[]",
                doc: document("new").as_ref(),
            })
            .unwrap();
        let observer = rusqlite::Connection::open(&database).unwrap();
        observer.execute_batch("CREATE TRIGGER deny_link BEFORE UPDATE OF source_id ON book_revisions BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
        assert!(
            store
                .link_ready_source(
                    first.book_id,
                    fence,
                    first.fence,
                    "/new/source.md",
                    "local",
                    "second"
                )
                .is_err()
        );
        assert!(
            store
                .lookup_source("/old/source.md")
                .unwrap()
                .unwrap()
                .is_primary
        );
        assert!(store.lookup_source("/new/source.md").unwrap().is_none());
        observer.execute_batch("DROP TRIGGER deny_link;").unwrap();
        let linked = store
            .link_ready_source(
                first.book_id,
                fence,
                first.fence,
                "/new/source.md",
                "local",
                "second",
            )
            .unwrap();
        assert!(linked.is_primary);
        assert!(
            !store
                .lookup_source("/old/source.md")
                .unwrap()
                .unwrap()
                .is_primary
        );
        assert_eq!(
            store.ready_revisions(first.book_id).unwrap()[0].source_id,
            Some(linked.id)
        );
    }

    #[test]
    fn deleted_record_handles_never_target_new_local_or_synced_records() {
        use folio_model::sync::{AnnotationRecord, BookmarkRecord};

        for synced in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let database = dir.path().join("library.sqlite");
            let store = Store::open_at(&database).unwrap();
            let book = store
                .add_book(Path::new("book.md"), document("text").meta())
                .unwrap();
            store.set_content_hash(book, "book-hash", "sha256").unwrap();
            let anchor = TextAnchor::point(0, 1);
            let note_id = store
                .add_annotation(book, &anchor, "old", "lime", AnnotationStyle::Highlight)
                .unwrap();
            let mark_id = store.add_bookmark(book, &anchor).unwrap();
            let old_note = store.annotation(note_id).unwrap().unwrap();
            let old_mark = store.bookmark(mark_id).unwrap().unwrap();
            store.delete_annotation(note_id).unwrap();
            store.delete_bookmark(mark_id).unwrap();
            drop(store);
            let store = Store::open_at(&database).unwrap();
            if synced {
                store
                    .apply_sync(&Plan {
                        annotation_upserts: vec![(
                            "book-hash".into(),
                            AnnotationRecord {
                                uid: "remote-note".into(),
                                anchor: anchor.clone(),
                                note: "new".into(),
                                color: "lime".into(),
                                style: "highlight".into(),
                                created_at: 1,
                                updated_at: 1,
                            },
                        )],
                        bookmark_upserts: vec![(
                            "book-hash".into(),
                            BookmarkRecord {
                                uid: "remote-mark".into(),
                                anchor: anchor.clone(),
                                created_at: 1,
                                updated_at: 1,
                            },
                        )],
                        ..Default::default()
                    })
                    .unwrap();
            } else {
                store
                    .add_annotation(book, &anchor, "new", "lime", AnnotationStyle::Highlight)
                    .unwrap();
                store.add_bookmark(book, &anchor).unwrap();
            }
            let new_note = store.annotations(book).unwrap().remove(0);
            let new_mark = store.bookmarks(book).unwrap().remove(0);
            assert!(
                new_note.id > note_id,
                "annotation ID reused, synced={synced}"
            );
            assert!(new_mark.id > mark_id, "bookmark ID reused, synced={synced}");
            store
                .update_annotation(note_id, "stale edit", "lime", AnnotationStyle::Highlight)
                .unwrap();
            store.delete_annotation(note_id).unwrap();
            store.delete_bookmark(mark_id).unwrap();
            store.restore_annotation(&old_note).unwrap();
            store.restore_bookmark(&old_mark).unwrap();
            assert_eq!(store.annotation(new_note.id).unwrap().unwrap().note, "new");
            assert_eq!(
                store.bookmark(new_mark.id).unwrap().unwrap().uid,
                new_mark.uid
            );
            assert_eq!(store.annotations(book).unwrap().len(), 2);
            assert_eq!(store.bookmarks(book).unwrap().len(), 2);
        }
    }

    #[test]
    fn undo_rejects_numeric_id_collision_without_replacing_other_records() {
        let store = Store::in_memory().unwrap();
        let book = store
            .add_book(Path::new("book.md"), document("text").meta())
            .unwrap();
        let anchor = TextAnchor::point(0, 1);
        let note_id = store
            .add_annotation(book, &anchor, "kept", "lime", AnnotationStyle::Highlight)
            .unwrap();
        let mark_id = store.add_bookmark(book, &anchor).unwrap();
        let mut note = store.annotation(note_id).unwrap().unwrap();
        let mut mark = store.bookmark(mark_id).unwrap().unwrap();
        let note_uid = note.uid.clone();
        let mark_uid = mark.uid.clone();
        note.uid = "other-note".into();
        mark.uid = "other-mark".into();
        assert!(store.restore_annotation(&note).is_err());
        assert!(store.restore_bookmark(&mark).is_err());
        assert_eq!(store.annotation(note_id).unwrap().unwrap().uid, note_uid);
        assert_eq!(store.bookmark(mark_id).unwrap().unwrap().uid, mark_uid);
    }
}
