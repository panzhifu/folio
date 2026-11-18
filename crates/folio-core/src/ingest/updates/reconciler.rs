use crate::infra::task::StageCtx;
use crate::ingest::source_monitor::SourceMonitor;
use crate::ingest::updates::{SourceUpdate, SourceUpdateState};
use crate::store::{AdoptResult, Store};
use std::path::Path;

/// 每个 App/Store 生命周期持有一份；普通轮询只捕获 monitor 选中的有界来源集合。
pub struct SourceReconciler {
    monitor: SourceMonitor,
    access: std::sync::Arc<dyn crate::ingest::external_source::ExternalSourceAccess>,
    external_cursor: usize,
}

impl Default for SourceReconciler {
    fn default() -> Self {
        Self::new()
    }
}
impl SourceReconciler {
    pub fn new() -> Self {
        Self::with_access(std::sync::Arc::new(
            crate::ingest::external_source::NoExternalSources,
        ))
    }
    pub fn with_access(
        access: std::sync::Arc<dyn crate::ingest::external_source::ExternalSourceAccess>,
    ) -> Self {
        Self {
            monitor: SourceMonitor::new(),
            access,
            external_cursor: 0,
        }
    }
    pub fn watcher_status(&self) -> crate::ingest::source_monitor::MonitorStatus {
        self.monitor.watcher_status()
    }
    pub fn reconcile(
        &mut self,
        store: &Store,
        root: &Path,
        ctx: &StageCtx,
        force: bool,
    ) -> Vec<SourceUpdate> {
        let sources = match store.sources_for_reconcile() {
            Ok(s) => s,
            Err(error) => {
                tracing::warn!("读取原件列表失败：{error}");
                return Vec::new();
            }
        };
        let external: Vec<_> = sources.iter().filter(|s| s.kind != "local").collect();
        for offset in 0..external.len().min(128) {
            let source = external[(self.external_cursor + offset) % external.len()];
            let stamp = self
                .access
                .metadata(&source.locator)
                .ok()
                .map(|m| format!("{:?}:{:?}", m.size, m.modified));
            self.monitor.set_external_stamp(source.id, stamp);
        }
        if !external.is_empty() {
            self.external_cursor =
                (self.external_cursor + external.len().min(128)) % external.len();
        }
        for source in &sources {
            if source.kind != "local" {
                continue;
            }
            let Ok(Some(active)) = store.active_revision(source.book_id) else {
                continue;
            };
            if let Ok(manifest) =
                serde_json::from_str::<Vec<serde_json::Value>>(&active.resource_manifest)
            {
                let parent = Path::new(&source.locator)
                    .parent()
                    .unwrap_or_else(|| Path::new("."));
                self.monitor.set_dependencies(
                    source.id,
                    manifest
                        .iter()
                        .filter_map(|r| r["path"].as_str())
                        .map(|p| parent.join(p))
                        .collect(),
                );
            }
        }
        let ids = self.monitor.select(&sources, force);
        let mut updates = crate::ingest::import::reconcile_source_ids_with_access(
            store,
            root,
            Some(&ids),
            ctx,
            self.access.as_ref(),
        );
        // 会话刚关闭时，稳定 Ready 无需重新捕获即可尝试采用。
        for source in &sources {
            if ids.contains(&source.id) {
                continue;
            }
            let Ok(Some(active)) = store.active_revision(source.book_id) else {
                continue;
            };
            let Ok(ready) = store.ready_revisions(source.book_id) else {
                continue;
            };
            let Some(ready) = ready.into_iter().find(|r| {
                r.source_id == Some(source.id) && r.source_generation == source.generation
            }) else {
                continue;
            };
            if let Ok(AdoptResult::Adopted) =
                store.adopt_revision(source.book_id, ready.fence, Some(active.fence), false)
            {
                updates.push(SourceUpdate {
                    source_id: source.id,
                    book_id: source.book_id,
                    state: SourceUpdateState::Adopted,
                    revision: Some(ready.fence),
                    message: None,
                });
            }
        }
        let adopted: std::collections::BTreeSet<_> = updates
            .iter()
            .filter(|r| r.state == SourceUpdateState::Adopted)
            .map(|r| r.book_id)
            .collect();
        for book_id in adopted {
            if let Err(error) = crate::ingest::updates::prune_revision_history(store, book_id, root)
            {
                tracing::warn!("历史版本字节回收未完成：{error}");
            }
        }
        updates
    }
}

#[cfg(test)]
mod tests {
    use crate::infra::task::StageCtx;
    use crate::ingest::external_source::{ExternalSourceAccess, SourceLink, SourceMetadata};
    use crate::ingest::import::{
        Imported, PathsSource, SourceUpdateState, import_from, reconcile_sources_once,
    };
    use crate::ingest::updates::{SourceReconciler, open_revision_document};
    use crate::store::{AdoptResult, Store};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    struct Fixture {
        dir: tempfile::TempDir,
        path: PathBuf,
        library: PathBuf,
        store: Arc<Store>,
        book: i64,
    }
    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.md");
        let library = dir.path().join("library");
        std::fs::write(&path, "# Book\n\nOriginal paragraph with enough text.").unwrap();
        let store = Arc::new(Store::in_memory().unwrap());
        let outcomes = import_from(
            &PathsSource::picked(vec![path.clone()]),
            &library,
            &store,
            &StageCtx::detached(),
        );
        let Imported::Added(row) = outcomes[0].result.as_ref().unwrap() else {
            panic!("首次导入应新增");
        };
        let book = row.book_id;
        Fixture {
            dir,
            path,
            library,
            store,
            book,
        }
    }

    #[test]
    fn source_update_keeps_book_identity_and_old_reading_bytes() {
        let f = fixture();
        let old = f.store.active_revision(f.book).unwrap().unwrap();
        let uuid = old.book_uuid.clone();
        std::fs::write(&f.path, "# New content\n\nA different paragraph is ready.").unwrap();
        let result = reconcile_sources_once(&f.store, &f.library, &StageCtx::detached());
        assert_eq!(result[0].state, SourceUpdateState::Adopted);
        assert_eq!(f.store.list_books().unwrap().len(), 1);
        let new = f.store.active_revision(f.book).unwrap().unwrap();
        assert_eq!(new.book_uuid, uuid);
        assert_ne!(new.fence, old.fence);
        assert_eq!(
            std::fs::read_to_string(&old.managed_path).unwrap(),
            "# Book\n\nOriginal paragraph with enough text."
        );
        let doc =
            open_revision_document(f.store.revision_snapshot(f.book, old.fence).unwrap(), None)
                .unwrap();
        assert!(
            doc.chapter_source(0)
                .unwrap()
                .contains("Original paragraph")
        );
        assert!(f.path.ends_with("book.md"));
    }

    #[test]
    fn source_update_ready_is_reused_while_leased_and_adopted_after_close() {
        let f = fixture();
        let lease = f.store.acquire_revision(f.book).unwrap();
        let old = lease.snapshot.revision.fence;
        std::fs::write(&f.path, "# Book\n\nNew version with sufficient text.").unwrap();
        let first = reconcile_sources_once(&f.store, &f.library, &StageCtx::detached());
        assert_eq!(first[0].state, SourceUpdateState::Ready);
        let again = reconcile_sources_once(&f.store, &f.library, &StageCtx::detached());
        assert_eq!(again[0].revision, first[0].revision);
        assert_eq!(f.store.revisions(f.book).unwrap().len(), 2);
        assert_eq!(f.store.active_revision(f.book).unwrap().unwrap().fence, old);
        let mut monitor = SourceReconciler::new();
        let _ = monitor.reconcile(&f.store, &f.library, &StageCtx::detached(), false);
        drop(lease);
        let adopted = monitor.reconcile(&f.store, &f.library, &StageCtx::detached(), false);
        assert!(
            adopted
                .iter()
                .any(|r| r.state == SourceUpdateState::Adopted)
        );
        assert_eq!(
            f.store.active_revision(f.book).unwrap().unwrap().fence,
            first[0].revision.unwrap()
        );
        assert_eq!(f.store.revisions(f.book).unwrap().len(), 2);
    }

    #[test]
    fn source_update_latest_generation_rejects_an_older_ready_candidate() {
        let f = fixture();
        let lease = f.store.acquire_revision(f.book).unwrap();
        let old = lease.snapshot.revision.fence;
        std::fs::write(&f.path, "# First\n\nFirst newer version.").unwrap();
        let first = reconcile_sources_once(&f.store, &f.library, &StageCtx::detached())[0]
            .revision
            .unwrap();
        std::fs::write(&f.path, "# Second\n\nSecond newest version.").unwrap();
        let newest = reconcile_sources_once(&f.store, &f.library, &StageCtx::detached())[0]
            .revision
            .unwrap();
        assert_ne!(first, newest);
        drop(lease);
        assert_eq!(
            f.store
                .adopt_revision(f.book, first, Some(old), false)
                .unwrap(),
            AdoptResult::Stale
        );
        assert_eq!(
            f.store
                .adopt_revision(f.book, newest, Some(old), false)
                .unwrap(),
            AdoptResult::Adopted
        );
    }

    #[test]
    fn source_update_image_only_change_is_a_new_bundle_revision() {
        let f = fixture();
        let image = f.dir.path().join("image.png");
        std::fs::write(&image, b"image-one").unwrap();
        std::fs::write(&f.path, "# Book\n\n![image](image.png)").unwrap();
        assert_eq!(
            reconcile_sources_once(&f.store, &f.library, &StageCtx::detached())[0].state,
            SourceUpdateState::Adopted
        );
        let first = f.store.active_revision(f.book).unwrap().unwrap();
        std::fs::write(image, b"image-two").unwrap();
        assert_eq!(
            reconcile_sources_once(&f.store, &f.library, &StageCtx::detached())[0].state,
            SourceUpdateState::Adopted
        );
        let second = f.store.active_revision(f.book).unwrap().unwrap();
        assert_eq!(first.content_hash, second.content_hash);
        assert_ne!(first.resource_hash, second.resource_hash);
    }

    #[test]
    fn source_update_bad_epub_preserves_the_previous_readable_revision() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("book.epub");
        let library = dir.path().join("library");
        let store = Store::in_memory().unwrap();
        let original = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/oss/minimal-zh.epub");
        std::fs::copy(original, &source).unwrap();
        let rows = import_from(
            &PathsSource::picked(vec![source.clone()]),
            &library,
            &store,
            &StageCtx::detached(),
        );
        let Imported::Added(row) = rows[0].result.as_ref().unwrap() else {
            panic!("EPUB导入");
        };
        let old = store.active_revision(row.book_id).unwrap().unwrap();
        std::fs::write(source, b"broken zip").unwrap();
        assert_eq!(
            reconcile_sources_once(&store, &library, &StageCtx::detached())[0].state,
            SourceUpdateState::Failed
        );
        assert_eq!(
            store.active_revision(row.book_id).unwrap().unwrap().fence,
            old.fence
        );
        assert!(
            open_revision_document(
                store.revision_snapshot(row.book_id, old.fence).unwrap(),
                None
            )
            .unwrap()
            .chapter_source(0)
            .is_ok()
        );
    }

    #[test]
    fn source_update_one_shot_replacement_pauses_the_old_primary() {
        let f = fixture();
        let replacement = f.dir.path().join("replacement.md");
        std::fs::write(
            &replacement,
            "# Replacement\n\nA manual replacement without persistent access.",
        )
        .unwrap();
        let result = crate::ingest::updates::update_from_path(
            &f.store,
            f.book,
            &replacement,
            &f.library,
            &StageCtx::detached(),
        )
        .unwrap();
        assert_eq!(result.state, SourceUpdateState::Adopted);
        assert_eq!(
            f.store.sources_for_book(f.book).unwrap()[0].state,
            "relink-required"
        );
        let adopted = f.store.active_revision(f.book).unwrap().unwrap().fence;
        std::fs::write(
            &f.path,
            "# Old source\n\nThis must not replace the manual content.",
        )
        .unwrap();
        assert!(reconcile_sources_once(&f.store, &f.library, &StageCtx::detached()).is_empty());
        assert_eq!(
            f.store.active_revision(f.book).unwrap().unwrap().fence,
            adopted
        );
    }

    struct External {
        bytes: Mutex<Vec<u8>>,
    }
    impl ExternalSourceAccess for External {
        fn metadata(&self, _: &str) -> std::io::Result<SourceMetadata> {
            Ok(SourceMetadata {
                size: Some(self.bytes.lock().unwrap().len() as u64),
                modified: None,
                display_name: "original.md".into(),
            })
        }
        fn open(&self, _: &str) -> std::io::Result<Box<dyn std::io::Read + Send>> {
            Ok(Box::new(std::io::Cursor::new(
                self.bytes.lock().unwrap().clone(),
            )))
        }
    }

    #[test]
    fn source_update_persistent_uri_uses_capability_instead_of_the_cache_path() {
        let dir = tempfile::tempdir().unwrap();
        let cached = dir.path().join("cache.md");
        let library = dir.path().join("library");
        let store = Store::in_memory().unwrap();
        std::fs::write(&cached, "# URI\n\nOld content.").unwrap();
        let link = SourceLink {
            kind: "android-document".into(),
            locator: "content://selected/document/1".into(),
        };
        let source = PathsSource::with_links(
            vec![cached.clone()],
            vec![(cached.clone(), link.clone())],
            true,
        );
        let rows = import_from(&source, &library, &store, &StageCtx::detached());
        let Imported::Added(row) = rows[0].result.as_ref().unwrap() else {
            panic!("URI导入");
        };
        let book = row.book_id;
        drop(source);
        assert!(!cached.exists());
        assert_eq!(
            store.sources_for_book(book).unwrap()[0].locator,
            link.locator
        );
        let access = Arc::new(External {
            bytes: Mutex::new(b"# URI\n\nUpdated through provider capability.".to_vec()),
        });
        let mut monitor = SourceReconciler::with_access(access);
        let results = monitor.reconcile(&store, &library, &StageCtx::detached(), true);
        assert_eq!(results[0].state, SourceUpdateState::Adopted);
        assert_eq!(store.list_books().unwrap().len(), 1);
    }

    #[test]
    fn fixed_chapter_artifacts_survive_a_changed_section_locator_and_fail_closed_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("chapters.md");
        let library = dir.path().join("library");
        let store = Store::in_memory().unwrap();
        std::fs::write(
            &source,
            "# First\n\nOriginal first section.\n\n## Second\n\nOriginal second section.",
        )
        .unwrap();
        let outcomes = import_from(
            &PathsSource::picked(vec![source]),
            &library,
            &store,
            &StageCtx::detached(),
        );
        let Imported::Added(row) = outcomes[0].result.as_ref().unwrap() else {
            panic!("首次导入");
        };
        let revision = store.active_revision(row.book_id).unwrap().unwrap();
        let mut snapshot = store
            .revision_snapshot(row.book_id, revision.fence)
            .unwrap();
        let folio_format::Content::Reflow(flow) = &mut snapshot.content else {
            panic!("可重排文档");
        };
        flow.chapters[0].text =
            folio_format::ChapterText::At(folio_format::ChapterLocator::Section(999));
        let doc = open_revision_document(snapshot, None).unwrap();
        assert!(
            doc.chapter_source(0)
                .unwrap()
                .contains("Original first section")
        );
        let artifact = std::path::Path::new(&revision.managed_path)
            .parent()
            .unwrap()
            .join(".folio-chapters/0.xhtml");
        std::fs::remove_file(artifact).unwrap();
        assert!(
            doc.chapter_source(0).is_err(),
            "固定产物丢失不能退回当前规则重分原文件"
        );
    }
}
