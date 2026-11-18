//! 原件编辑、阅读租约、笔记归属、同步与备份恢复的组合契约。

use std::sync::Arc;

use folio_core::backup::{Options, RestoreLimits, Roots, create, restore_into};
use folio_core::infra::task::StageCtx;
use folio_core::ingest::import::{PathsSource, import_from};
use folio_core::ingest::updates::{
    SourceUpdateState, open_revision_document, reconcile_sources_once,
};
use folio_core::store::AdoptResult;
use folio_core::{AnnotationStyle, Config, Store, TextAnchor};

#[test]
fn editing_a_linked_original_preserves_reader_notes_sync_and_restored_versions() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let original = root.join("我的原件.md");
    let library = root.join("library");
    let before = "# Book\n\nA unique sentence for the preserved annotation.\n";
    let after =
        "# Book\n\nA newly added paragraph.\n\nA unique sentence for the preserved annotation.\n";
    std::fs::write(&original, before).unwrap();
    let store = Arc::new(Store::open_at(&root.join("folio.db")).unwrap());
    let imported = import_from(
        &PathsSource::picked(vec![original.clone()]),
        &library,
        &store,
        &StageCtx::detached(),
    );
    assert!(imported[0].result.is_ok(), "{:?}", imported[0].result);
    let book = store.list_books().unwrap().remove(0);
    assert_eq!(std::fs::read_to_string(&original).unwrap(), before);
    assert_eq!(
        std::path::Path::new(&book.path).file_name().unwrap(),
        original.file_name().unwrap()
    );
    let first = store.active_revision(book.id).unwrap().unwrap();
    let quote = "A unique sentence for the preserved annotation.";
    let anchor = TextAnchor {
        chapter: 0,
        start: 0,
        end: quote.chars().count(),
        exact: quote.into(),
        ..Default::default()
    };
    let annotation = store
        .add_annotation_checked(
            book.id,
            first.fence,
            &anchor,
            "keep this note",
            "lime",
            AnnotationStyle::Highlight,
        )
        .unwrap();
    let bookmark = store
        .add_bookmark_checked(book.id, first.fence, &anchor)
        .unwrap();
    let leased = store.acquire_revision(book.id).unwrap();
    let old_document = open_revision_document(leased.snapshot, None).unwrap();
    let old_text = old_document.chapter_source(0).unwrap();
    std::fs::write(&original, after).unwrap();
    let checked = reconcile_sources_once(&store, &library, &StageCtx::detached());
    assert_eq!(checked.len(), 1);
    assert_eq!(checked[0].state, SourceUpdateState::Ready, "{checked:?}");
    assert_eq!(old_document.chapter_source(0).unwrap(), old_text);
    assert_eq!(
        store.active_revision(book.id).unwrap().unwrap().fence,
        first.fence
    );
    let next = checked[0].revision.unwrap();
    drop(old_document);
    drop(leased.lease);
    assert_eq!(
        store
            .adopt_revision(book.id, next, Some(first.fence), false)
            .unwrap(),
        AdoptResult::Adopted
    );
    assert_eq!(store.list_books().unwrap().len(), 1);
    let current = store.active_revision(book.id).unwrap().unwrap();
    assert_eq!(current.book_uuid, first.book_uuid);
    assert_ne!(current.content_hash, first.content_hash);
    assert_eq!(
        store.annotation(annotation).unwrap().unwrap().anchor,
        anchor
    );
    assert_eq!(store.bookmark(bookmark).unwrap().unwrap().anchor, anchor);
    let visible = store.annotations_for_revision(book.id, next).unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].anchor.exact, quote);
    let sync = store.collect_sync("local", 1).unwrap().snapshot;
    assert_eq!(sync.books[&first.content_hash].annotations.len(), 1);
    assert!(sync.books[&current.content_hash].annotations.is_empty());
    let mut config = Config::default();
    config.library.dir = library.to_string_lossy().into();
    let roots = Roots {
        backups: root.join("backups"),
        data: root.into(),
        library: Some(library),
        dicts: None,
        fonts: None,
        themes: None,
    };
    let archive = create(
        &store,
        &config,
        &roots,
        Options {
            include_files: true,
        },
        &mut |_, _| {},
    )
    .unwrap();
    let restored = restore_into(
        &archive.path,
        &root.join("restored"),
        RestoreLimits::default(),
    )
    .unwrap();
    let restored_store = Store::open_at(&restored.database).unwrap();
    assert!(restored_store.sources_for_reconcile().unwrap().is_empty());
    let doc = open_revision_document(
        restored_store.revision_snapshot(book.id, next).unwrap(),
        None,
    )
    .unwrap();
    assert!(
        doc.chapter_source(0)
            .unwrap()
            .contains("A newly added paragraph.")
    );
    assert_eq!(
        restored_store.annotation(annotation).unwrap().unwrap().note,
        "keep this note"
    );
    assert_eq!(restored_store.revisions(book.id).unwrap().len(), 2);
}
