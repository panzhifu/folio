//! 云端批量上传：把一批书按内容哈希传进同步仓库的 `books/`。
//!
//! 与同步共用同一台远端仓库（WebDAV 或网盘）与同一个远端目录；书文件落在
//! `<remote_dir>/books/<content_hash>.<ext>` —— 另一台设备按哈希认书，与局域网补书
//! 那条路同一个键。协议在 [`crate::net::remote`]，这一层只编排与报结果：
//! 建目录、逐本传、单本失败不拦后面 —— 一批十本因为一本读不开就全停，
//! 读者只知道「失败了」，不知道成功了几本。

use crate::UploadBook;
use crate::net::remote::{Cancel, Error as RemoteError, RemoteStore};

/// 这一批传到哪儿了。`Running` 只知道总本数：每本传完的通知要从后台线程递回界面，
/// 为一条进度条再开一条通道不值 —— 结果在完成时一次说清。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    Running {
        total: usize,
    },
    Done {
        uploaded: usize,
        failed: usize,
        /// 选中的书里没有 `content_hash` 的，没资格上传（远端认不回来）。
        skipped: usize,
    },
    Failed(Failure),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// 同步没配好：WebDAV 没地址，或网盘没登录。
    Config,
    /// 书库打不开 / 读不出书目。
    Store,
    /// 远端目录建不起来（连不上、没权限）。
    Remote,
}

/// 后台那一趟：建目录、逐本传。单本失败不拦后面。
pub fn upload_batch(
    remote: &dyn RemoteStore,
    dir: &str,
    books: Vec<UploadBook>,
    skipped: usize,
) -> Progress {
    let dir = dir.trim_matches('/');
    let books_dir = if dir.is_empty() {
        "books".to_string()
    } else {
        format!("{dir}/books")
    };
    if let Err(err) = remote.mkdir_all(&books_dir) {
        tracing::warn!("upload: 建远端目录 {books_dir:?} 失败：{err}");
        return Progress::Failed(Failure::Remote);
    }
    let mut uploaded = 0usize;
    let mut failed = 0usize;
    for book in books {
        let target = format!("{books_dir}/{}.{}", book.hash, extension(&book));
        match upload_one(remote, &target, &book) {
            Ok(()) => uploaded += 1,
            Err(err) => {
                tracing::warn!("upload: 《{}》失败：{err}", book.title);
                failed += 1;
            }
        }
    }
    tracing::info!("upload: 一批传完 —— 成功 {uploaded}，失败 {failed}，跳过 {skipped}");
    Progress::Done {
        uploaded,
        failed,
        skipped,
    }
}

fn upload_one(
    remote: &dyn RemoteStore,
    target: &str,
    book: &UploadBook,
) -> crate::net::remote::Result<()> {
    let mut file = std::fs::File::open(&book.path).map_err(RemoteError::Io)?;
    let size = file.metadata().map(|meta| meta.len()).unwrap_or(book.size);
    remote.put_file(target, &mut file, size, &Cancel::new())?;
    Ok(())
}

/// 远端扩展名：本地副本的扩展名优先，没有就退回书目的格式名。
fn extension(book: &UploadBook) -> String {
    std::path::Path::new(&book.path)
        .extension()
        .and_then(|ext| ext.to_str())
        .filter(|ext| !ext.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| book.format.clone())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use crate::net::remote::dav::Entry;

    use super::*;

    /// 只记账的远端：目录建了哪些、文件传到了哪个键。
    #[derive(Default)]
    struct FakeRemote {
        dirs: Mutex<Vec<String>>,
        puts: Mutex<Vec<String>>,
    }

    impl RemoteStore for FakeRemote {
        fn list(&self, _path: &str) -> crate::net::remote::Result<Vec<Entry>> {
            Ok(Vec::new())
        }

        fn get(&self, _path: &str) -> crate::net::remote::Result<Vec<u8>> {
            Err(RemoteError::Malformed("不读".into()))
        }

        fn put(&self, _path: &str, _body: Vec<u8>) -> crate::net::remote::Result<()> {
            Ok(())
        }

        fn mkdir(&self, path: &str) -> crate::net::remote::Result<()> {
            self.dirs.lock().unwrap().push(path.to_string());
            Ok(())
        }

        fn download(
            &self,
            _path: &str,
            _sink: &mut dyn crate::net::remote::Sink,
            _cancel: &Cancel,
        ) -> crate::net::remote::Result<u64> {
            Err(RemoteError::Malformed("不下载".into()))
        }

        fn put_file(
            &self,
            path: &str,
            source: &mut dyn crate::net::remote::Source,
            _size: u64,
            _cancel: &Cancel,
        ) -> crate::net::remote::Result<u64> {
            let mut body = Vec::new();
            let mut buf = [0u8; 64];
            loop {
                let n = source.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&buf[..n]);
            }
            self.puts.lock().unwrap().push(path.to_string());
            Ok(body.len() as u64)
        }
    }

    fn book(hash: &str, path: &std::path::Path, title: &str) -> UploadBook {
        UploadBook {
            id: 1,
            hash: hash.into(),
            path: path.display().to_string(),
            title: title.into(),
            format: "epub".into(),
            size: 4,
        }
    }

    /// 一批里读不开的那本只记失败，不拦后面；远端键是 `<目录>/books/<哈希>.<扩展名>`。
    #[test]
    fn batch_uploads_hashed_files_and_counts_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("a.epub");
        std::fs::write(&good, b"book").unwrap();
        let missing = dir.path().join("missing.epub");
        let remote = FakeRemote::default();

        let progress = upload_batch(
            &remote,
            "folio-sync",
            vec![
                book("hash-a", &good, "书甲"),
                book("hash-b", &missing, "书乙"),
            ],
            1,
        );

        assert_eq!(
            progress,
            Progress::Done {
                uploaded: 1,
                failed: 1,
                skipped: 1
            }
        );
        assert_eq!(
            remote.puts.lock().unwrap().as_slice(),
            ["folio-sync/books/hash-a.epub"],
            "远端键按内容哈希命名"
        );
        assert_eq!(
            remote.dirs.lock().unwrap().as_slice(),
            ["folio-sync", "folio-sync/books"],
            "两层目录都要建"
        );
    }

    /// 远端目录为空（WebDAV 根）：书落在 `books/`，不带前导斜杠。
    #[test]
    fn an_empty_remote_dir_puts_books_under_books() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("b.epub");
        std::fs::write(&good, b"book").unwrap();
        let remote = FakeRemote::default();
        let progress = upload_batch(&remote, "", vec![book("h", &good, "书")], 0);
        assert_eq!(
            progress,
            Progress::Done {
                uploaded: 1,
                failed: 0,
                skipped: 0
            }
        );
        assert_eq!(remote.puts.lock().unwrap().as_slice(), ["books/h.epub"]);
    }
}
