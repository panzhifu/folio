//! 发布应用独占的暂存副本；目录项只可新建，绝不替换已有字节。

use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

#[cfg(test)]
use crate::error::{Error, IoResultExt as _, Result};
#[cfg(test)]
use crate::infra::identity;

#[cfg(test)]
pub(super) struct Published {
    pub path: PathBuf,
    pub created: bool,
}

#[cfg(test)]
pub(super) fn publish(staged: &Path, preferred: &Path, hash: &str) -> Result<Published> {
    publish_with(staged, preferred, hash, true)
}

#[cfg(test)]
fn publish_with(staged: &Path, preferred: &Path, hash: &str, use_links: bool) -> Result<Published> {
    for attempt in 0..32 {
        let target = if attempt == 0 {
            preferred.to_path_buf()
        } else {
            let ext = preferred
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("book");
            let suffix = if attempt == 1 {
                hash.to_string()
            } else {
                format!("{hash}-{attempt}")
            };
            preferred.with_file_name(format!("{suffix}.{ext}"))
        };
        // 这里只链接管线自己的完整暂存文件，不链接外部原件；成功后立即断开暂存名。
        match create_copy(staged, &target, use_links) {
            Ok(()) => {
                if let Err(err) = std::fs::remove_file(staged) {
                    let _ = std::fs::remove_file(&target);
                    return Err(err).io_path(staged);
                }
                return Ok(Published {
                    path: target,
                    created: true,
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                let meta = std::fs::symlink_metadata(&target).io_path(&target)?;
                if meta.file_type().is_symlink() || !meta.is_file() {
                    continue;
                }
                if identity::sha256_of_path(&target).io_path(&target)?.to_hex() == hash {
                    std::fs::remove_file(staged).io_path(staged)?;
                    return Ok(Published {
                        path: target,
                        created: false,
                    });
                }
            }
            Err(err) => return Err(err).io_path(&target),
        }
    }
    Err(Error::Io {
        path: preferred.into(),
        source: "发布目标持续冲突，已有文件已保留".to_string().into(),
    })
}

/// create_new 的副本只在完整写入并 fsync 后交给登记方；链接仅是受控暂存的可选优化。
pub(super) fn create_copy(staged: &Path, target: &Path, use_links: bool) -> std::io::Result<()> {
    if use_links {
        match std::fs::hard_link(staged, target) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Err(error),
            Err(_) => {}
        }
    }
    let mut source = std::fs::File::open(staged)?;
    let mut target_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    let result = std::io::copy(&mut source, &mut target_file).and_then(|_| target_file.sync_all());
    if result.is_err() {
        drop(target_file);
        let _ = std::fs::remove_file(target);
    }
    result
}

#[cfg(test)]
mod tests {
    use crate::infra::identity::sha256_of_bytes;
    use crate::infra::task::StageCtx;
    use crate::ingest::import::publication::{publish, publish_with};
    use crate::ingest::import::{PathsSource, import_from, reindex};
    use crate::store::Store;

    #[test]
    fn occupied_publication_never_replaces_different_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let stage = dir.path().join("stage");
        let target = dir.path().join("occupied.md");
        std::fs::write(&stage, b"incoming").unwrap();
        std::fs::write(&target, b"edited").unwrap();
        let published = publish(&stage, &target, &sha256_of_bytes(b"incoming").to_hex()).unwrap();
        assert!(published.created);
        assert_ne!(published.path, target);
        assert_eq!(std::fs::read(target).unwrap(), b"edited");
        assert_eq!(std::fs::read(published.path).unwrap(), b"incoming");
    }

    #[test]
    fn existing_identical_publication_is_not_owned_by_the_new_operation() {
        let dir = tempfile::tempdir().unwrap();
        let stage = dir.path().join("stage");
        let target = dir.path().join("same.md");
        std::fs::write(&stage, b"same").unwrap();
        std::fs::write(&target, b"same").unwrap();
        let published = publish(&stage, &target, &sha256_of_bytes(b"same").to_hex()).unwrap();
        assert!(!published.created);
        assert_eq!(published.path, target);
        assert!(!stage.exists());
    }

    #[test]
    fn publication_copy_fallback_keeps_conflicts_and_completes_owned_files() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("stage");
        let target = dir.path().join("book.md");
        std::fs::write(&staged, b"new complete bytes").unwrap();
        std::fs::write(&target, b"owned by someone else").unwrap();
        let published = publish_with(
            &staged,
            &target,
            &sha256_of_bytes(b"new complete bytes").to_hex(),
            false,
        )
        .unwrap();
        assert!(published.created);
        assert_eq!(std::fs::read(&target).unwrap(), b"owned by someone else");
        assert_eq!(
            std::fs::read(published.path).unwrap(),
            b"new complete bytes"
        );
        assert!(!staged.exists());
    }

    #[test]
    fn importing_old_bytes_after_reindex_never_overwrites_the_edited_copy() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.md");
        let library = dir.path().join("library");
        std::fs::write(&original, "# Original\n\noriginal text").unwrap();
        let store = Store::in_memory().unwrap();
        let first = import_from(
            &PathsSource::picked(vec![original.clone()]),
            &library,
            &store,
            &StageCtx::detached(),
        );
        assert!(first[0].result.is_ok());
        let row = store.list_books().unwrap().remove(0);
        std::fs::write(&row.path, "# Edited\n\nedited text must survive").unwrap();
        let _ = reindex(&store, row.id);
        let _ = import_from(
            &PathsSource::picked(vec![original]),
            &library,
            &store,
            &StageCtx::detached(),
        );
        assert_eq!(
            std::fs::read_to_string(&row.path).unwrap(),
            "# Edited\n\nedited text must survive"
        );
    }
}
