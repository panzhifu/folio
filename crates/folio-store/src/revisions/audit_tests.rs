use std::path::Path;

use folio_format::document::{Document, from_content};
use folio_format::{Chapter, ChapterExt as _, Content, DocMeta, FlowDoc, Format};
use folio_model::TextAnchor;
use folio_model::sync::{AnnotationRecord, Plan};

use crate::{AnnotationStyle, ImportRegistration, RevisionReady, Store};

fn doc(chapters: usize) -> Box<dyn Document> {
    from_content(
        DocMeta {
            title: "book".into(),
            format: Format::Markdown,
            size: 100,
            ..Default::default()
        },
        Content::Reflow(FlowDoc {
            chapters: (0..chapters)
                .map(|i| Chapter::inline(format!("chapter {i}"), format!("<p>text {i}</p>")))
                .collect(),
            ..Default::default()
        }),
    )
}
fn initial(store: &Store, bundle: bool) -> crate::BookRevision {
    store
        .register_import(ImportRegistration {
            book_uuid: "audit",
            doc: doc(1).as_ref(),
            managed_path: Path::new("private/first.md"),
            library_root: Path::new("private"),
            content_hash: "hash-original",
            resource_hash: if bundle { "bundle" } else { "" },
            resource_manifest: "[]",
            source_locator: None,
            source_kind: "local",
            source_fingerprint: "",
        })
        .unwrap()
}
fn next(store: &Store, first: &crate::BookRevision, same_bytes: bool) -> crate::BookRevision {
    let fence = if same_bytes {
        store.reserve_reindex(first.book_id).unwrap()
    } else {
        store.reserve_revision(first.book_id).unwrap()
    };
    let next = store
        .ready_revision(RevisionReady {
            book_id: first.book_id,
            fence,
            expected_active: Some(first.fence),
            source_id: None,
            source_generation: 0,
            content_hash: if same_bytes {
                "hash-original"
            } else {
                "hash-next"
            },
            resource_hash: "",
            managed_path: Path::new("private/next.md"),
            library_root: Path::new("private"),
            resource_manifest: "[]",
            doc: doc(3).as_ref(),
        })
        .unwrap();
    store
        .adopt_revision(first.book_id, fence, Some(first.fence), false)
        .unwrap();
    next
}

#[test]
fn accepted_remote_reanchor_invalidates_previously_located_projection() {
    let store = Store::in_memory().unwrap();
    let first = initial(&store, false);
    let id = store
        .add_annotation_checked(
            first.book_id,
            first.fence,
            &TextAnchor::point(0, 2),
            "note",
            "amber",
            AnnotationStyle::Highlight,
        )
        .unwrap();
    let saved = store.annotation(id).unwrap().unwrap();
    let next = next(&store, &first, false);
    store
        .set_anchor_projection(
            first.book_id,
            "annotation",
            &saved.uid,
            next.fence,
            Some(&TextAnchor::point(1, 4)),
        )
        .unwrap();
    let remote = AnnotationRecord {
        uid: saved.uid.clone(),
        anchor: TextAnchor::point(0, 99),
        note: saved.note,
        color: saved.color,
        style: saved.style.as_str().into(),
        created_at: saved.created_at,
        updated_at: saved.updated_at + 100,
    };
    store
        .apply_sync(&Plan {
            annotation_upserts: vec![(first.content_hash, remote)],
            ..Default::default()
        })
        .unwrap();
    assert!(
        store
            .annotations_for_revision(first.book_id, next.fence)
            .unwrap()
            .is_empty(),
        "已接受原锚点变化后，旧投影不能仍显示在旧推导位置"
    );
}

#[test]
fn same_bytes_reindex_does_not_send_new_chapter_coordinates_as_canonical_v1_coordinates() {
    let store = Store::in_memory().unwrap();
    let first = initial(&store, false);
    let next = next(&store, &first, true);
    let bookmark = store
        .add_bookmark_checked(first.book_id, next.fence, &TextAnchor::point(2, 3))
        .unwrap();
    let uid = store.bookmark(bookmark).unwrap().unwrap().uid;
    let exported = store.collect_sync("audit", 0).unwrap();
    assert!(
        exported.snapshot.books[&first.content_hash]
            .bookmarks
            .iter()
            .all(|r| r.uid != uid || r.anchor.chapter < 1),
        "v1 hash不携带index_generation，不能把新版第2章坐标当旧版单章坐标发出"
    );
}

#[test]
fn v1_file_transfer_does_not_advertise_a_bundle_as_a_complete_single_file() {
    let store = Store::in_memory().unwrap();
    let first = initial(&store, true);
    assert_eq!(store.collect_sync("audit", 0).unwrap().skipped_books, 1);
    assert!(
        store
            .lan_manifest()
            .map_or(true, |entries| entries.is_empty()),
        "资源bundle不能仅靠主文件冒充完整局域网书文件"
    );
    assert!(
        store
            .upload_books(&[first.book_id])
            .map_or(true, |entries| entries.is_empty())
    );
}
