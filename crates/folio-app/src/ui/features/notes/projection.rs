use std::collections::HashMap;

use folio_core::Store;
use folio_core::store::notes::BookGroup;
use gpui_kit::App;

use crate::ui::services::l10n::tr;

/// 全局笔记始终保留原文；出处只能取当前坐标代的已验证映射。
pub(super) fn apply(store: &Store, groups: &mut [BookGroup], cx: &App) {
    for group in groups {
        let Some(revision) = store.active_revision(group.book_id).ok().flatten() else {
            continue;
        };
        let mut anchors = HashMap::new();
        for row in store
            .annotations_for_revision(group.book_id, revision.fence)
            .unwrap_or_default()
        {
            anchors.insert((false, row.id), row.anchor);
        }
        for row in store
            .bookmarks_for_revision(group.book_id, revision.fence)
            .unwrap_or_default()
        {
            anchors.insert((true, row.id), row.anchor);
        }
        let content = store.load_content(group.book_id).ok().flatten();
        for mark in &mut group.marks {
            if let Some(anchor) = anchors.get(&(mark.is_bookmark, mark.id)) {
                mark.chapter = anchor.chapter as usize;
                mark.offset = anchor.start;
                mark.place = content
                    .as_ref()
                    .and_then(|content| content.as_flow())
                    .and_then(|flow| flow.chapters.get(mark.chapter))
                    .map(|chapter| chapter.title.clone())
                    .unwrap_or_default();
            } else {
                mark.place = tr(cx, "source_update.unlocated").to_string();
                mark.stale = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ui::features::notes::projection::apply;
    use crate::ui::services::{config::ConfigHolder, l10n::tr};
    use folio_core::content::document::from_content;
    use folio_core::store::{ImportRegistration, RevisionReady, notes::load_groups};
    use folio_core::{
        AnnotationStyle, Chapter, ChapterExt, Config, Content, DocMeta, FlowDoc, Format, Store,
        TextAnchor,
    };
    use gpui_kit::TestAppContext;

    #[gpui_kit::test]
    fn source_revision_notes_list_keeps_original_text_and_labels_unmapped_anchors(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let temp = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let content = |title| {
            from_content(
                DocMeta {
                    title: "Book".into(),
                    format: Format::Markdown,
                    ..Default::default()
                },
                Content::Reflow(FlowDoc {
                    chapters: vec![Chapter::inline(title, "<p>content</p>")],
                    ..Default::default()
                }),
            )
        };
        let old = content("Original chapter");
        let old_path = temp.path().join("old.md");
        let active = store
            .register_import(ImportRegistration {
                book_uuid: "22222222222222222222222222222222",
                doc: old.as_ref(),
                managed_path: &old_path,
                library_root: temp.path(),
                content_hash: &"a".repeat(64),
                resource_hash: "",
                resource_manifest: "[]",
                source_locator: None,
                source_kind: "one-shot",
                source_fingerprint: "",
            })
            .unwrap();
        let id = store
            .add_annotation_checked(
                active.book_id,
                active.fence,
                &TextAnchor::point(0, 3),
                "Original note",
                "amber",
                AnnotationStyle::Highlight,
            )
            .unwrap();
        let target = store.reserve_revision(active.book_id).unwrap();
        let next = content("Current chapter");
        let next_path = temp.path().join("new.md");
        store
            .ready_revision(RevisionReady {
                book_id: active.book_id,
                fence: target,
                expected_active: Some(active.fence),
                source_id: None,
                source_generation: 0,
                content_hash: &"b".repeat(64),
                resource_hash: "",
                managed_path: &next_path,
                library_root: temp.path(),
                resource_manifest: "[]",
                doc: next.as_ref(),
            })
            .unwrap();
        store
            .adopt_revision(active.book_id, target, Some(active.fence), false)
            .unwrap();
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            let mut groups = load_groups(&store).unwrap();
            apply(&store, &mut groups, cx);
            assert_eq!(groups[0].marks[0].note, "Original note");
            assert_eq!(
                groups[0].marks[0].place,
                tr(cx, "source_update.unlocated").to_string()
            );
            let uid = store.annotation(id).unwrap().unwrap().uid;
            store
                .set_anchor_projection(
                    active.book_id,
                    "annotation",
                    &uid,
                    target,
                    Some(&TextAnchor::point(0, 7)),
                )
                .unwrap();
            let mut located = load_groups(&store).unwrap();
            apply(&store, &mut located, cx);
            assert_eq!(located[0].marks[0].place, "Current chapter");
            assert_eq!(located[0].marks[0].offset, 7);
            assert_eq!(store.annotation(id).unwrap().unwrap().anchor.start, 3);
        });
    }
}
