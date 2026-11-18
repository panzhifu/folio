//! 平台持久来源的只读能力；核心捕获管线不认识JNI或系统URI授权。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{Error, IoResultExt as _, Result};
use crate::infra::task::StageCtx;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLink {
    pub kind: String,
    pub locator: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceMetadata {
    pub size: Option<u64>,
    pub modified: Option<i64>,
    pub display_name: String,
}

pub trait ExternalSourceAccess: Send + Sync {
    fn metadata(&self, locator: &str) -> std::io::Result<SourceMetadata>;
    /// 返回独占读句柄；释放Box后平台句柄也关闭。
    fn open(&self, locator: &str) -> std::io::Result<Box<dyn Read + Send>>;
}

pub struct NoExternalSources;
impl ExternalSourceAccess for NoExternalSources {
    fn metadata(&self, _: &str) -> std::io::Result<SourceMetadata> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "当前平台不能读取此持久来源",
        ))
    }
    fn open(&self, _: &str) -> std::io::Result<Box<dyn Read + Send>> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "当前平台不能读取此持久来源",
        ))
    }
}

pub(crate) struct ExternalStaged {
    pub path: PathBuf,
    pub name: String,
    pub hash: String,
    file: Option<std::fs::File>,
}
impl Drop for ExternalStaged {
    fn drop(&mut self) {
        self.file.take();
        let _ = std::fs::remove_file(&self.path);
    }
}

const MAX_SOURCE_BYTES: u64 = 8 << 30;

pub(crate) fn capture_external(
    access: &dyn ExternalSourceAccess,
    locator: &str,
    staging: &Path,
    ctx: &StageCtx,
) -> Result<ExternalStaged> {
    let before = access.metadata(locator).io_path("持久来源")?;
    if before.size.is_some_and(|s| s > MAX_SOURCE_BYTES) {
        return Err(failure("来源超过捕获容量"));
    }
    std::fs::create_dir_all(staging).io_path(staging)?;
    let path = staging.join(format!(".external-{}.part", crate::sync::new_device_id()));
    let output = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .io_path(&path)?;
    let mut staged = ExternalStaged {
        path,
        name: before.display_name.clone(),
        hash: String::new(),
        file: Some(output),
    };
    let mut input = access.open(locator).io_path("持久来源")?;
    let mut digest = Sha256::new();
    let mut bytes = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        if ctx.cancel().triggered() {
            return Err(failure("来源捕获已取消"));
        }
        let count = input.read(&mut bytes).io_path("持久来源")?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| failure("来源长度溢出"))?;
        if total > MAX_SOURCE_BYTES {
            return Err(failure("来源超过捕获容量"));
        }
        staged
            .file
            .as_mut()
            .expect("捕获完成前持有文件")
            .write_all(&bytes[..count])
            .io_path(&staged.path)?;
        digest.update(&bytes[..count]);
        ctx.bytes(total, before.size, "捕获原件");
    }
    staged
        .file
        .as_ref()
        .expect("捕获完成前持有文件")
        .sync_all()
        .io_path(&staged.path)?;
    staged.file.take();
    drop(input);
    let after = access.metadata(locator).io_path("持久来源")?;
    if before.size.is_some_and(|s| s != total)
        || before.size != after.size
        || before.modified != after.modified
    {
        return Err(failure("捕获期间原件发生变化，请稍后重试"));
    }
    staged.hash = digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(staged)
}

fn failure(why: &str) -> Error {
    Error::Io {
        path: "持久来源".into(),
        source: why.to_string().into(),
    }
}

#[cfg(test)]
mod tests {
    use crate::infra::task::StageCtx;
    use crate::ingest::external_source::{ExternalSourceAccess, SourceMetadata, capture_external};
    use std::io::{Cursor, Read};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Source {
        reads: AtomicUsize,
        change: bool,
    }
    impl ExternalSourceAccess for Source {
        fn metadata(&self, _: &str) -> std::io::Result<SourceMetadata> {
            Ok(SourceMetadata {
                size: Some(5),
                modified: Some(if self.change {
                    self.reads.fetch_add(1, Ordering::Relaxed) as i64
                } else {
                    1
                }),
                display_name: "原件.txt".into(),
            })
        }
        fn open(&self, _: &str) -> std::io::Result<Box<dyn Read + Send>> {
            Ok(Box::new(Cursor::new(b"hello".to_vec())))
        }
    }

    #[test]
    fn persistent_source_capture_hashes_owned_bytes_and_rejects_mid_read_change() {
        let dir = tempfile::tempdir().unwrap();
        let stable = Source {
            reads: AtomicUsize::new(0),
            change: false,
        };
        let staged = capture_external(
            &stable,
            "content://selected",
            dir.path(),
            &StageCtx::detached(),
        )
        .unwrap();
        assert_eq!(std::fs::read(&staged.path).unwrap(), b"hello");
        assert_eq!(
            staged.hash,
            crate::infra::identity::sha256_of_bytes(b"hello").to_hex()
        );
        drop(staged);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        let changing = Source {
            reads: AtomicUsize::new(0),
            change: true,
        };
        assert!(
            capture_external(
                &changing,
                "content://selected",
                dir.path(),
                &StageCtx::detached()
            )
            .is_err()
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
