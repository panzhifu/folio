//! 仅回收登记为可释放、无会话/备份引用的独占版本目录；身份和笔记不随字节删除。

use crate::error::{IoResultExt as _, Result};
use crate::store::{BookRevision, Store};
use std::io::Read as _;
use std::path::Path;

pub fn prune_revision_history(store: &Store, book_id: i64, root: &Path) -> Result<usize> {
    let mut removed = 0;
    for revision in store.prunable_revisions(book_id)? {
        let Some(directory) = owned_directory(&revision, root)? else {
            continue;
        };
        let Some(detached) = store.detach_revision_file(book_id, revision.fence)? else {
            continue;
        };
        if detached != revision.managed_path {
            continue;
        }
        std::fs::remove_dir_all(&directory).io_path(&directory)?;
        removed += 1;
    }
    Ok(removed)
}

fn owned_directory(revision: &BookRevision, root: &Path) -> Result<Option<std::path::PathBuf>> {
    if revision.managed_path.is_empty() || revision.book_uuid.is_empty() {
        return Ok(None);
    }
    if !revision
        .book_uuid
        .bytes()
        .all(|b| b.is_ascii_hexdigit() || b == b'-')
    {
        return Ok(None);
    }
    let expected = root
        .join("versions")
        .join(&revision.book_uuid)
        .join(format!(
            "{}-{}",
            revision.fence.rev_no, revision.fence.index_generation
        ));
    let path = Path::new(&revision.managed_path);
    if path.parent() != Some(expected.as_path()) {
        return Ok(None);
    }
    let root = root.canonicalize().io_path(root)?;
    if !expected.exists() {
        return Ok(None);
    }
    let actual = expected.canonicalize().io_path(&expected)?;
    if !actual.starts_with(&root) {
        return Ok(None);
    }
    let mut parent = Some(expected.as_path());
    while let Some(dir) = parent {
        let metadata = std::fs::symlink_metadata(dir).io_path(dir)?;
        if linked(&metadata) {
            return Ok(None);
        }
        if dir == root {
            break;
        }
        parent = dir.parent();
        if dir == Path::new(&revision.library_root) {
            break;
        }
    }
    let marker = expected.join(".folio-revision.json");
    let metadata = std::fs::symlink_metadata(&marker).io_path(&marker)?;
    if linked(&metadata) || !metadata.is_file() {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&marker)
        .io_path(&marker)?
        .take((2 << 20) + 1)
        .read_to_end(&mut bytes)
        .io_path(&marker)?;
    if bytes.len() > 2 << 20 {
        return Ok(None);
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    if value["book_uuid"].as_str() != Some(revision.book_uuid.as_str())
        || value["rev_no"].as_i64() != Some(revision.fence.rev_no)
        || value["index_generation"].as_i64() != Some(revision.fence.index_generation)
        || value["content_hash"].as_str() != Some(revision.content_hash.as_str())
    {
        return Ok(None);
    }
    Ok(Some(expected))
}

fn linked(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

#[cfg(test)]
mod tests {
    use crate::infra::task::StageCtx;
    use crate::ingest::import::{
        Imported, PathsSource, import_from, prune_revision_history, reconcile_sources_once,
    };
    use crate::store::Store;

    #[test]
    fn revision_reclamation_keeps_current_previous_and_pinned_bytes_and_all_identity() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("library");
        let source = dir.path().join("book.md");
        let store = std::sync::Arc::new(Store::in_memory().unwrap());
        std::fs::write(&source, "# First\n\nFirst version.").unwrap();
        let rows = import_from(
            &PathsSource::picked(vec![source.clone()]),
            &library,
            &store,
            &StageCtx::detached(),
        );
        let Imported::Added(row) = rows[0].result.as_ref().unwrap() else {
            panic!("首次导入");
        };
        let book = row.book_id;
        let first = store.active_revision(book).unwrap().unwrap();
        let pins = store.retain_all_revision_files().unwrap();
        let mut old_candidate = None;
        for text in ["# Second\n\nSecond version.", "# Third\n\nThird version."] {
            std::fs::write(&source, text).unwrap();
            assert!(
                reconcile_sources_once(&store, &library, &StageCtx::detached())[0]
                    .revision
                    .is_some()
            );
            if old_candidate.is_none() {
                let revision = store.ready_revisions(book).unwrap().remove(0);
                let pin = store.retain_revision(book, revision.fence).unwrap();
                old_candidate = Some((revision, pin));
            }
        }
        let latest = store.ready_revisions(book).unwrap().remove(0);
        let (obsolete, old_candidate_pin) = old_candidate.unwrap();
        // 过期候选也先受 pin 保护；释放后只回收失效字节，当前与最新 Ready 保留。
        assert_eq!(prune_revision_history(&store, book, &library).unwrap(), 0);
        drop(old_candidate_pin);
        assert_eq!(prune_revision_history(&store, book, &library).unwrap(), 1);
        assert!(!PathForTest::new(&obsolete.managed_path).exists());
        assert!(PathForTest::new(&first.managed_path).exists());
        assert!(PathForTest::new(&latest.managed_path).exists());
        drop(pins);
        reconcile_sources_once(&store, &library, &StageCtx::detached());
        std::fs::write(&source, "# Fourth\n\nFourth version.").unwrap();
        reconcile_sources_once(&store, &library, &StageCtx::detached());
        assert!(prune_revision_history(&store, book, &library).unwrap() >= 1);
        assert!(!PathForTest::new(&first.managed_path).exists());
        assert_eq!(store.list_books().unwrap().len(), 1);
        assert!(
            store
                .revisions(book)
                .unwrap()
                .iter()
                .any(|r| r.content_hash == first.content_hash)
        );
    }
    use std::path::Path as PathForTest;
}
