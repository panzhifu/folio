//! 阅读版本租约与显式切换；原件更新不改变正在阅读的 Document。

use std::sync::Arc;

use folio_core::Locator;
use folio_core::content::document::Document as OpenedDocument;
use folio_core::store::{AdoptResult, RevisionFence, RevisionLease, Store};
use gpui_kit::{AppContext as _, Context, Subscription};

use crate::ui::features::reader::model::book::Book;
use crate::ui::features::reader::model::chrome::PageMode;
use crate::ui::features::reader::model::fixed_book::FixedBook;
use crate::ui::features::reader::model::session::{ReaderSession, Source};
use crate::ui::services::source_updates;

#[derive(Default)]
pub(in crate::ui::features::reader) struct ContentVersion {
    fence: Option<RevisionFence>,
    lease: Option<RevisionLease>,
    watch: Option<Subscription>,
    pub pending: Option<RevisionFence>,
    pub failure: Option<String>,
    pub opening: Option<i64>,
    jump_record: Option<(bool, i64)>,
    observed_epoch: u64,
}

impl ContentVersion {
    pub fn for_book(store: Option<&Arc<Store>>, book_id: Option<i64>) -> Self {
        let leased = store
            .zip(book_id)
            .and_then(|(store, id)| store.acquire_revision(id).ok());
        match leased {
            Some(leased) => Self {
                fence: Some(leased.snapshot.revision.fence),
                lease: Some(leased.lease),
                ..Self::default()
            },
            None => Self::default(),
        }
    }
}

impl ReaderSession {
    pub(in crate::ui::features::reader) fn coordinate_fence(&self) -> Option<RevisionFence> {
        self.book_id
            .map(|_| self.content_version.fence.unwrap_or_default())
    }

    pub(in crate::ui::features::reader) fn release_content_lease(&mut self) {
        self.content_version.lease = None;
        self.content_version.watch = None;
    }

    fn watch_content_updates(&mut self, cx: &mut Context<Self>) {
        if self.content_version.watch.is_none()
            && let Some(updates) = source_updates::handle(cx)
        {
            self.content_version.watch = Some(cx.observe(&updates, |this, updates, cx| {
                if !source_updates::belongs_to_current_store(&updates, cx) {
                    return;
                }
                if let Some(book_id) = this.content_version.opening {
                    let active = this
                        .store
                        .as_ref()
                        .and_then(|store| store.active_revision(book_id).ok().flatten());
                    if active.is_some_and(|revision| revision.state != "legacy-unverified") {
                        this.open_book(book_id, cx);
                        return;
                    }
                    if !updates.read(cx).is_busy() {
                        let status = updates.read(cx).book(book_id);
                        if matches!(
                            status.state,
                            Some(
                                folio_core::ingest::updates::SourceUpdateState::Unavailable
                                    | folio_core::ingest::updates::SourceUpdateState::Failed
                            )
                        ) {
                            this.content_version.failure = status.message;
                            cx.notify();
                            return;
                        } else if status.ready.is_some() {
                            this.content_version.pending = status.ready;
                            this.content_version.failure = None;
                            cx.notify();
                            return;
                        } else {
                            source_updates::verify_legacy(book_id, cx);
                        }
                    }
                }
                let epoch = updates.read(cx).epoch();
                if this.content_version.observed_epoch == epoch {
                    cx.notify();
                    return;
                }
                this.content_version.observed_epoch = epoch;
                this.load_annotations();
                this.load_bookmarks();
                this.refresh_content_update(cx);
            }));
        }
    }

    pub(in crate::ui::features::reader) fn refresh_content_update(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        let (Some(store), Some(book_id)) = (self.store.as_ref(), self.book_id) else {
            return;
        };
        let active = store.active_revision(book_id).ok().flatten();
        self.content_version.pending = active
            .filter(|revision| Some(revision.fence) != self.content_version.fence)
            .map(|revision| revision.fence)
            .or_else(|| {
                store
                    .ready_revisions(book_id)
                    .ok()?
                    .first()
                    .map(|revision| revision.fence)
            });
        cx.notify();
    }

    /// 先在同一Store协调点租下索引快照，再构造Document，避免打开与自动采用交错。
    pub fn open_book(&mut self, book_id: i64, cx: &mut Context<Self>) {
        self.watch_content_updates(cx);
        let Some(store) = self.store.clone() else {
            return;
        };
        if store
            .active_revision(book_id)
            .ok()
            .flatten()
            .is_some_and(|revision| revision.state == "legacy-unverified")
        {
            self.content_version.opening = Some(book_id);
            self.content_version.failure = Some("source_update.verifying".into());
            source_updates::verify_legacy(book_id, cx);
            cx.notify();
            return;
        }
        let leased = match store.acquire_revision(book_id) {
            Ok(leased) => leased,
            Err(error) => {
                self.content_version.failure = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        if leased.snapshot.revision.state == "legacy-unverified" {
            self.content_version.opening = Some(book_id);
            self.content_version.failure = Some("source_update.verifying".into());
            drop(leased);
            source_updates::verify_legacy(book_id, cx);
            cx.notify();
            return;
        }
        let snapshot = leased.snapshot;
        let fence = snapshot.revision.fence;
        let (mode, locator) = store
            .position_for_revision(book_id, fence)
            .ok()
            .flatten()
            .unwrap_or((String::new(), Locator::start()));
        let passphrase = store.passphrase_for_book(book_id).ok().flatten();
        let page_bg = snapshot
            .content
            .as_flow()
            .and_then(|flow| folio_core::content::format::css::page_background_color(&flow.css))
            .map(crate::ui::foundation::reading_theme::to_hsla);
        let doc: Arc<dyn OpenedDocument> =
            match folio_core::ingest::updates::open_revision_document(snapshot, passphrase) {
                Ok(doc) => Arc::from(doc),
                Err(error) => {
                    self.content_version.failure = Some(error.to_string());
                    if self.book_id.is_none() {
                        self.content_version.opening = Some(book_id);
                    }
                    if self.content_version.lease.is_none() {
                        self.content_version.lease = self
                            .book_id
                            .zip(self.content_version.fence)
                            .and_then(|(id, fence)| store.retain_revision(id, fence).ok());
                    }
                    cx.notify();
                    return;
                }
            };
        self.persist_position();
        self.stop_speech();
        self.flush_book_settings(cx);
        self.settle_reading();
        self.content_version.lease = Some(leased.lease);
        self.content_version.fence = Some(fence);
        self.content_version.pending = None;
        self.content_version.failure = None;
        self.content_version.opening = None;
        let _ = store.touch_opened(book_id);
        self.apply_book_settings(book_id, cx);
        crate::ui::foundation::reading_theme::set_page_bg(page_bg);
        let warm = doc.clone();
        cx.background_spawn(async move { warm.warm() }).detach();
        self.source = if doc.is_fixed() {
            Source::Fixed(Box::new(FixedBook::new(doc, locator)))
        } else {
            Source::Book(Box::new(Book::new(doc, self.prefs.clone(), locator)))
        };
        self.atlas.update(cx, |atlas, cx| atlas.clear(cx));
        self.font_cache.reset(cx.text_system().all_font_names());
        self.book_fonts_rev = self.book().map(|book| book.fonts_revision()).unwrap_or(0);
        self.clear_selection();
        self.footnote = None;
        self.jump_back = None;
        self.mode = PageMode::from_storage_name(&mode);
        self.mode_undecided = mode.is_empty();
        self.reset_fixed_zoom();
        self.turn = None;
        self.pending_turn = None;
        self.pending_chapter_turn = None;
        self.swipe = None;
        self.capture_dirty = true;
        self.capture_box_dirty = true;
        self.captured_page = None;
        self.captured_size = None;
        self.captured_back = None;
        self.captured_incoming = None;
        self.capture_committed = Default::default();
        self.capture_back_committed = Default::default();
        self.capture_incoming_committed = Default::default();
        self.curl_bake_origin = Default::default();
        self.incoming_bake_origin = Default::default();
        self.book_id = Some(book_id);
        self.read = store
            .read_chapters_for_revision(book_id, fence)
            .unwrap_or_default();
        self.begin_reading(cx);
        self.load_annotations();
        self.load_bookmarks();
        self.undo = None;
        self.undo_seq = self.undo_seq.wrapping_add(1);
        self.translation = None;
        self.translation_seq = self.translation_seq.wrapping_add(1);
        self.ai = None;
        self.ai_seq = self.ai_seq.wrapping_add(1);
        self.dictionary = None;
        self.dictionary_seq = self.dictionary_seq.wrapping_add(1);
        self.rules_draft = None;
        self.rules_draft_seq = self.rules_draft_seq.wrapping_add(1);
        self.close_image_viewer(cx);
        self.rebuild_image_sequence();
        self.stack_motion = None;
        self.scroll_y = 0.0;
        self.page = 0;
        self.wheel_accum = 0.0;
        self.content_height = 0.0;
        self.saved_chapter = locator.chapter as usize;
        self.saved_offset = locator.offset;
        self.pend_restore((self.mode.is_continuous() || locator.offset > 0).then_some(locator));
        self.refresh_content_update(cx);
        if let Some((bookmark, id)) = self.content_version.jump_record.take() {
            self.goto_record(bookmark, id, cx);
        }
        source_updates::poll_now(cx);
    }

    pub(in crate::ui::features::reader) fn adopt_content_update(&mut self, cx: &mut Context<Self>) {
        let (Some(store), Some(book_id), Some(target)) = (
            self.store.clone(),
            self.content_version.opening.or(self.book_id),
            self.content_version.pending,
        ) else {
            return;
        };
        if self.note_editor.is_some() {
            self.content_version.failure = Some("source_update.finish_note".into());
            cx.notify();
            return;
        }
        // 旧坐标代仍持有租约；先结清旧位置，Store再原子验证目标与来源代。
        self.persist_position();
        self.stop_speech();
        self.flush_book_settings(cx);
        let old_fence = self.content_version.fence;
        self.content_version.lease = None;
        let expected = store
            .active_revision(book_id)
            .ok()
            .flatten()
            .map(|revision| revision.fence);
        match store.adopt_revision(book_id, target, expected, false) {
            Ok(AdoptResult::Adopted | AdoptResult::AlreadyActive) => {
                self.open_book(book_id, cx);
                source_updates::adopted(book_id, cx);
            }
            Ok(result) => {
                self.content_version.lease =
                    old_fence.and_then(|fence| store.retain_revision(book_id, fence).ok());
                self.refresh_content_update(cx);
                self.content_version.failure = Some(
                    if result == AdoptResult::ReadersActive {
                        "source_update.other_reader"
                    } else {
                        "source_update.changed_again"
                    }
                    .into(),
                );
                cx.notify();
            }
            Err(error) => {
                self.content_version.lease =
                    old_fence.and_then(|fence| store.retain_revision(book_id, fence).ok());
                self.content_version.failure = Some(error.to_string());
                cx.notify();
            }
        }
    }

    pub(in crate::ui::features::reader) fn goto_record(
        &mut self,
        bookmark: bool,
        id: i64,
        cx: &mut Context<Self>,
    ) {
        if self.content_version.opening.is_some() {
            self.content_version.jump_record = Some((bookmark, id));
            return;
        }
        let anchor = if bookmark {
            self.bookmarks
                .iter()
                .find(|row| row.id == id)
                .map(|row| row.anchor.clone())
        } else {
            self.annotations
                .iter()
                .find(|row| row.id == id)
                .map(|row| row.anchor.clone())
        };
        if let Some(anchor) = anchor {
            self.goto_offset(anchor.chapter as usize, anchor.start, cx);
        } else {
            self.content_version.failure = Some("source_update.unlocated".into());
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use folio_core::content::document::{Document, from_content};
    use folio_core::store::{AdoptResult, ImportRegistration, RevisionReady, Store};
    use folio_core::{Chapter, ChapterExt, Config, Content, DocMeta, FlowDoc, Format, Locator};
    use gpui_kit::base::NavStackState;
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AppContext as _, TestAppContext, px, size};

    use crate::ui::features::reader::model::session::ReaderSession;
    use crate::ui::features::reader::view::reading_page::ReadingPage;
    use crate::ui::services::{app_store, config::ConfigHolder};

    fn document(text: &str) -> Box<dyn Document> {
        from_content(
            DocMeta {
                title: text.into(),
                format: Format::Markdown,
                ..Default::default()
            },
            Content::Reflow(FlowDoc {
                chapters: vec![Chapter::inline("chapter", format!("<p>{text}</p>"))],
                ..Default::default()
            }),
        )
    }

    fn register(store: &Store, root: &Path) -> folio_core::store::BookRevision {
        let directory = root.join("first");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("first.md");
        std::fs::write(&path, "First revision").unwrap();
        let doc = document("First revision");
        let revision = store
            .register_import(ImportRegistration {
                book_uuid: "11111111111111111111111111111111",
                doc: doc.as_ref(),
                managed_path: &path,
                library_root: root,
                content_hash: &"a".repeat(64),
                resource_hash: "",
                resource_manifest: "[]",
                source_locator: None,
                source_kind: "one-shot",
                source_fingerprint: "",
            })
            .unwrap();
        write_marker(
            &directory,
            "first.md",
            &revision.book_uuid,
            revision.fence,
            &revision.content_hash,
        );
        revision
    }

    fn write_marker(
        root: &Path,
        main: &str,
        uuid: &str,
        fence: folio_core::store::RevisionFence,
        hash: &str,
    ) {
        std::fs::write(root.join(".folio-revision.json"), serde_json::to_vec(&serde_json::json!({
            "version":1,"book_uuid":uuid,"rev_no":fence.rev_no,"index_generation":fence.index_generation,
            "main":main,"content_hash":hash,"resource_hash":"","chapter_sources":[]
        })).unwrap()).unwrap();
    }

    fn ready(
        store: &Store,
        root: &Path,
        first: &folio_core::store::BookRevision,
    ) -> folio_core::store::RevisionFence {
        let target = store.reserve_revision(first.book_id).unwrap();
        let directory = root.join(format!(
            "second-{}-{}",
            target.rev_no, target.index_generation
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("second.md");
        write_marker(
            &directory,
            "second.md",
            &first.book_uuid,
            target,
            &"b".repeat(64),
        );
        std::fs::write(&path, "Second revision").unwrap();
        let doc = document("Second revision");
        store
            .ready_revision(RevisionReady {
                book_id: first.book_id,
                fence: target,
                expected_active: Some(first.fence),
                source_id: None,
                source_generation: 0,
                content_hash: &"b".repeat(64),
                resource_hash: "",
                managed_path: &path,
                library_root: root,
                resource_manifest: "[]",
                doc: doc.as_ref(),
            })
            .unwrap();
        target
    }

    #[gpui_kit::test]
    fn source_revision_old_mark_buttons_cannot_delete_after_switching_books_with_the_same_fence(
        cx: &mut TestAppContext,
    ) {
        use crate::ui::features::reader::components::toc_panel::TocPanel;
        use folio_core::ingest::import::{Imported, PathsSource, import_from};
        use gpui_kit::{InputEvent as _, MouseDownEvent, MouseMoveEvent, MouseUpEvent};

        cx.update(gpui_kit::init);
        let files = tempfile::tempdir().unwrap();
        let store = cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            app_store::install(Store::in_memory().unwrap(), cx);
            cx.set_reduce_motion(true);
            app_store::handle(cx).unwrap()
        });
        let import = |name: &str| {
            let path = files.path().join(format!("{name}.md"));
            std::fs::write(
                &path,
                format!("# {name}\n\nA readable paragraph for this book."),
            )
            .unwrap();
            let rows = import_from(
                &PathsSource::picked(vec![path]),
                &files.path().join("library"),
                &store,
                &folio_core::StageCtx::detached(),
            );
            let Imported::Added(row) = rows[0].result.as_ref().unwrap() else {
                panic!("new book");
            };
            row.book_id
        };
        let (a, b) = (import("First"), import("Second"));
        let fence = store.active_revision(a).unwrap().unwrap().fence;
        assert_eq!(store.active_revision(b).unwrap().unwrap().fence, fence);
        let annotation = store
            .add_annotation_checked(
                a,
                fence,
                &folio_core::TextAnchor::point(0, 2),
                "Keep A",
                "amber",
                folio_core::AnnotationStyle::Highlight,
            )
            .unwrap();
        let bookmark = store
            .add_bookmark_checked(a, fence, &folio_core::TextAnchor::point(0, 2))
            .unwrap();
        let removed = store
            .add_bookmark_checked(a, fence, &folio_core::TextAnchor::point(0, 6))
            .unwrap();
        let mut reader_entity = None;
        let window = cx.open_window(size(px(900.0), px(600.0)), |window, cx| {
            let reader = cx.new(|cx| {
                let mut reader = ReaderSession::boot(cx);
                reader.open_book(a, cx);
                reader.remove_bookmark(removed, cx);
                reader
            });
            reader_entity = Some(reader.clone());
            let panel = cx.new(|cx| TocPanel::new(reader, cx));
            panel.update(cx, |panel, cx| panel.toggle(cx));
            Root::new(panel, window, cx)
        });
        let reader = reader_entity.unwrap();
        let positions = cx
            .update_window(window.into(), |_, window, cx| {
                window.click("toc-screen-marks", cx);
                let buttons = window.find_all("mark-delete");
                assert_eq!(buttons.len(), 2);
                buttons
                    .into_iter()
                    .map(|button| button.bounds().center())
                    .collect::<Vec<_>>()
            })
            .unwrap();
        assert!(reader.read_with(cx, |reader, _| reader.undo.is_some()));
        reader.update(cx, |reader, cx| reader.open_book(b, cx));
        assert!(reader.read_with(cx, |reader, _| reader.undo.is_none()));
        // 故意不渲染B：向A的真实上一帧按钮派发输入，验证旧回调的归属。
        cx.update_window(window.into(), |_, window, cx| {
            for position in positions {
                window.dispatch_event(
                    MouseMoveEvent {
                        position,
                        ..Default::default()
                    }
                    .to_platform_input(),
                    cx,
                );
                window.dispatch_event(
                    MouseDownEvent {
                        position,
                        click_count: 1,
                        ..Default::default()
                    }
                    .to_platform_input(),
                    cx,
                );
                window.dispatch_event(
                    MouseUpEvent {
                        position,
                        click_count: 1,
                        ..Default::default()
                    }
                    .to_platform_input(),
                    cx,
                );
            }
        })
        .unwrap();
        reader.update(cx, |reader, cx| {
            assert!(!reader.remove_annotation(annotation, cx));
            reader.remove_bookmark(bookmark, cx);
            reader.undo_delete(cx);
        });
        assert!(store.annotation(annotation).unwrap().is_some());
        assert!(store.bookmark(bookmark).unwrap().is_some());
        assert!(store.bookmark(removed).unwrap().is_none());
        assert_eq!(reader.read_with(cx, |reader, _| reader.book_id()), Some(b));
    }

    #[gpui_kit::test]
    fn source_revision_pending_selects_the_latest_ready_version(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::in_memory().unwrap());
        let first = register(&store, temp.path());
        let reader = cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            cx.new(|cx| {
                ReaderSession::with_document(
                    Arc::from(document("First revision")),
                    folio_core::Reader::default(),
                    Some(store.clone()),
                    Some(first.book_id),
                    Locator::start(),
                    cx,
                )
            })
        });
        let older = ready(&store, temp.path(), &first);
        let latest = ready(&store, temp.path(), &first);
        assert_ne!(older, latest);
        reader.update(cx, |reader, cx| reader.refresh_content_update(cx));
        assert_eq!(
            reader.read_with(cx, |reader, _| reader.content_version.pending),
            Some(latest)
        );
    }

    #[gpui_kit::test]
    fn source_revision_switch_button_rebuilds_the_reader_and_fences_old_coordinates(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let temp = tempfile::tempdir().unwrap();
        let store = cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            app_store::install(Store::in_memory().unwrap(), cx);
            app_store::handle(cx).unwrap()
        });
        let first = register(&store, temp.path());
        let mut reader_entity = None;
        let stack = cx.update(|cx| cx.new(|_| NavStackState::new()));
        let window = cx.open_window(size(px(800.0), px(600.0)), |window, cx| {
            let reader = cx.new(|cx| {
                let mut reader = ReaderSession::boot(cx);
                reader.open_book(first.book_id, cx);
                reader
            });
            reader_entity = Some(reader.clone());
            let page = cx.new(|cx| ReadingPage::new(reader, stack.downgrade(), cx));
            Root::new(page, window, cx)
        });
        let reader = reader_entity.unwrap();
        assert!(store.revision_is_leased(first.book_id, first.fence));
        let target = ready(&store, temp.path(), &first);
        assert_eq!(
            store
                .adopt_revision(first.book_id, target, Some(first.fence), false)
                .unwrap(),
            AdoptResult::ReadersActive
        );
        reader.update(cx, |reader, cx| reader.refresh_content_update(cx));
        assert_eq!(
            reader.read_with(cx, |reader, _| reader.coordinate_fence()),
            Some(first.fence)
        );
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("reader-adopt-update").is_some());
            window.click("reader-adopt-update", cx);
        })
        .unwrap();
        assert_eq!(
            reader.read_with(cx, |reader, _| reader.coordinate_fence()),
            Some(target)
        );
        assert!(!store.revision_is_leased(first.book_id, first.fence));
        assert!(store.revision_is_leased(first.book_id, target));
        assert!(
            store
                .set_position_checked(first.book_id, first.fence, "paged", &Locator::start())
                .is_err()
        );
        assert!(
            store
                .set_position_checked(first.book_id, target, "paged", &Locator::start())
                .is_ok()
        );
        assert!(
            reader.read_with(cx, |reader, _| reader.translation.is_none()
                && reader.ai.is_none()
                && reader.undo.is_none())
        );
        reader.update(cx, |reader, _| reader.release_content_lease());
        assert!(!store.revision_is_leased(first.book_id, target));
    }

    #[gpui_kit::test]
    fn source_revision_replaces_capture_commit_cells_so_old_paint_cannot_commit(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::in_memory().unwrap());
        let first = register(&store, temp.path());
        let reader = cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            cx.new(|cx| {
                ReaderSession::with_document(
                    Arc::from(document("First revision")),
                    folio_core::Reader::default(),
                    Some(store.clone()),
                    Some(first.book_id),
                    Locator::start(),
                    cx,
                )
            })
        });
        let old_commit = reader.read_with(cx, |reader, _| reader.capture_committed.clone());
        let target = ready(&store, temp.path(), &first);
        reader.update(cx, |reader, cx| {
            reader.refresh_content_update(cx);
            reader.adopt_content_update(cx);
        });
        old_commit.set(Some(((99, 99), (100.0, 100.0))));
        assert_eq!(
            reader.read_with(cx, |reader, _| reader.coordinate_fence()),
            Some(target)
        );
        assert!(reader.read_with(cx, |reader, _| reader.capture_committed.get().is_none()));
    }

    #[gpui_kit::test]
    fn source_revision_switch_waits_for_another_window_and_keeps_the_old_lease(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::in_memory().unwrap());
        let first = register(&store, temp.path());
        let other_window = store.acquire_revision(first.book_id).unwrap();
        let reader = cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            cx.new(|cx| {
                ReaderSession::with_document(
                    Arc::from(document("First revision")),
                    folio_core::Reader::default(),
                    Some(store.clone()),
                    Some(first.book_id),
                    Locator::start(),
                    cx,
                )
            })
        });
        let target = ready(&store, temp.path(), &first);
        reader.update(cx, |reader, cx| {
            reader.refresh_content_update(cx);
            reader.adopt_content_update(cx);
        });
        assert_eq!(
            reader.read_with(cx, |reader, _| reader.coordinate_fence()),
            Some(first.fence)
        );
        assert_eq!(
            reader.read_with(cx, |reader, _| reader.content_version.failure.clone()),
            Some("source_update.other_reader".into())
        );
        assert!(store.revision_is_leased(first.book_id, first.fence));
        assert!(
            store
                .set_position_checked(first.book_id, first.fence, "paged", &Locator::start())
                .is_ok()
        );
        drop(other_window);
        reader.update(cx, |reader, cx| reader.adopt_content_update(cx));
        assert_eq!(
            reader.read_with(cx, |reader, _| reader.coordinate_fence()),
            Some(target)
        );
    }

    #[gpui_kit::test]
    fn source_revision_old_notes_without_a_projection_remain_readable_without_wrong_navigation(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::in_memory().unwrap());
        let first = register(&store, temp.path());
        let id = store
            .add_annotation_checked(
                first.book_id,
                first.fence,
                &folio_core::TextAnchor::point(0, 8),
                "Original note",
                "amber",
                folio_core::AnnotationStyle::Highlight,
            )
            .unwrap();
        let reader = cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            cx.new(|cx| {
                ReaderSession::with_document(
                    Arc::from(document("First revision")),
                    folio_core::Reader::default(),
                    Some(store.clone()),
                    Some(first.book_id),
                    Locator::start(),
                    cx,
                )
            })
        });
        let target = ready(&store, temp.path(), &first);
        reader.update(cx, |reader, cx| {
            reader.refresh_content_update(cx);
            reader.adopt_content_update(cx);
            reader.goto_record(false, id, cx);
        });
        assert_eq!(
            reader.read_with(cx, |reader, _| reader.coordinate_fence()),
            Some(target)
        );
        assert!(reader.read_with(cx, |reader, _| reader.annotations.is_empty()));
        assert_eq!(
            reader.read_with(cx, |reader, _| reader.content_version.failure.clone()),
            Some("source_update.unlocated".into())
        );
        assert_eq!(store.annotation(id).unwrap().unwrap().note, "Original note");
    }
}
