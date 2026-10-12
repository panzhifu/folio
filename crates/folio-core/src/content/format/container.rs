//! 一个**长期持有**的容器句柄：开一次，之后按名字取字节。
//!
//! `plans/导入管线.md` §十 把这一层单列出来，理由是三个数量事实：
//!
//! - **容器家族比格式少**：EPUB、CBZ、`.fb2.zip` 都是 ZIP，它们的寻址代码该是同一份；
//!   TXT / Markdown / 裸 XHTML 是「就一个文件」。让每个 provider 各写一遍寻址，
//!   就会得到 N 份各自漂移的实现。
//! - **打开一次很贵**：ZIP 的中央目录在**文件末尾**，所以每次取字节都重开一次容器，
//!   就是每次把整个中央目录再解析一遍、再建一遍条目名索引。
//! - **取字节很频繁**：滚动一屏要读十几张图，翻章要读一章正文。
//!
//! 把这两件事合起来就是这一层存在的全部理由：**索引建一次，之后只查表**。
//! 改之前 `image_bytes` 每调用一次都 `Zip::open`，实测「读同一本书的一百张图」
//! 要开一百次容器（`format/epub/tests.rs` 里那条目标断言数的就是这个数）。
//!
//! ## 三条设计约束
//!
//! **一、懒开**：`BookDocument` 只拿到一个路径就构造出来了，而绝大多数书在
//! 这次会话里根本不会被取字节（不开图、不换章）。所以第一句 `read` 才真的开容器。
//!
//! **二、`&self` 而不是 `&mut self`**：`Document: Send + Sync`，字节源要在排版
//! 后台线程与绘制之间共享（`ui/shared/images.rs` 的 `ByteSource` 是
//! `Arc<dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync>`）。所以内部可变性只能用
//! `Mutex` 与 `OnceLock`，不能用 `RefCell`。
//!
//! **三、开不出来就安静地回 `None`**：文件被删了、权限变了、容器坏了 —— 都不是
//! 「这本书打不开」，而是「这张图没有」。降级在取字节这一层完成，正文照排。
//! 打开失败**不缓存成永久失败**：文件可能在这次会话中途被同步回来。

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use super::zip::Zip;

/// 一种容器家族的字节寻址。目前只有 ZIP 一族（EPUB / CBZ），
/// 将来 PalmDB（MOBI / AZW3）与裸文件的 `FileRange` 各加一条分支。
pub struct Container {
    path: PathBuf,
    /// 第一次要字节时才真的开。开失败**不留痕** —— 下次调用会再试一次。
    open: OnceLock<Mutex<Zip>>,
}

impl std::fmt::Debug for Container {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Container")
            .field("path", &self.path)
            // 不把锁里的 `Zip` 摊开：它几千个条目名，打出来没人看。
            .field("open", &self.open.get().is_some())
            .finish()
    }
}

impl Container {
    /// 认下一个容器的路径，但**先不开**（见模块头的「懒开」）。
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            open: OnceLock::new(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 容器此刻是不是已经开着（诊断用；不去触发打开）。
    pub fn is_open(&self) -> bool {
        self.open.get().is_some()
    }

    /// 拿那个已打开的容器；没开就开一次。
    ///
    /// 锁中毒（另一个线程在持有它时 panic）时**继续用**而不是往外抛：
    /// 里面的 `Zip` 是个只读索引，半途 panic 不会让它变成半个状态，
    /// 而在这里拒绝服务等于把一次后台线程的 panic 升级成「图全都不显示」。
    fn get(&self) -> Option<&Mutex<Zip>> {
        if let Some(open) = self.open.get() {
            return Some(open);
        }
        // 竞态：两个线程同时第一次取字节。`Zip::open` 会跑两遍，而
        // `set` 只有一个能成功 —— 输的那个用赢家那份，两遍的代价是白付一次，
        // 比在热路径上加锁便宜，也不会给出两个不同的索引。
        let opened = Zip::open(&self.path).ok()?;
        let _ = self.open.set(Mutex::new(opened));
        self.open.get()
    }

    /// 按**归一化容器路径**取字节（清单里怎么写就怎么给）。
    ///
    /// `None` 覆盖：容器开不出来、名字查不到（精确与大小写不敏感都不中）、
    /// 条目是目录、解压失败。调用方一律当「这块地方空着」处理。
    pub fn read(&self, href: &str) -> Option<Vec<u8>> {
        let mut zip = self.get()?.lock().unwrap_or_else(|e| e.into_inner());
        zip.read_at(href)
    }

    /// [`Container::read`] 的文本版。
    pub fn read_text(&self, href: &str) -> Option<String> {
        let mut zip = self.get()?.lock().unwrap_or_else(|e| e.into_inner());
        zip.read_text_at(href)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::Container;
    use crate::infra::metrics::{Metric, take_local};

    /// 一个装了若干条目的 zip，交回它的临时目录（目录活得比测试久）。
    fn archive_with(entries: &[(&str, &[u8])]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let file = std::fs::File::create(dir.path().join("case.zip")).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        for (name, data) in entries {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
        dir
    }

    /// **这一层存在的全部理由**：反复取字节只开一次容器。
    ///
    /// 改之前每次取字节都 `Zip::open`，于是「读一本书的一百张图」要开一百次容器 ——
    /// 每次都要重新解析中央目录、重建几千个条目名的索引，而那份开销整个落在
    /// 滚动的热路径上。这条数的是次数不是时间，所以在任何机器上都一样。
    #[test]
    fn many_reads_open_the_container_once() {
        let dir = archive_with(&[
            ("mimetype", b"application/epub+zip"),
            ("OEBPS/Images/a.png", b"aaa"),
            ("OEBPS/Images/b.png", b"bbb"),
        ]);
        let container = Container::new(&dir.path().join("case.zip"));
        assert!(!container.is_open(), "构造时不该开容器");

        take_local(Metric::ContainerOpens);
        take_local(Metric::EntryNameTablePasses);

        // 交替取两张图，一百次 —— 模拟滚动经过一批插图。
        for i in 0..100 {
            let href = if i % 2 == 0 {
                "OEBPS/Images/a.png"
            } else {
                "OEBPS/Images/b.png"
            };
            assert!(container.read(href).is_some(), "第 {i} 次取图该有字节");
        }

        assert_eq!(
            take_local(Metric::ContainerOpens),
            1,
            "一百次取字节只该开一次容器"
        );
        assert_eq!(
            take_local(Metric::EntryNameTablePasses),
            1,
            "条目名索引也只该建一遍"
        );
    }

    /// 名字大小写与清单写的不一致时要查得到 —— 宽松匹配在 `Zip` 那一层，
    /// 这一层只保证不把它丢了。
    #[test]
    fn lookup_is_case_insensitive_and_returns_the_bytes() {
        let dir = archive_with(&[("OEBPS/Images/Cover.PNG", b"png-bytes")]);
        let container = Container::new(&dir.path().join("case.zip"));
        assert_eq!(
            container.read("OEBPS/Images/cover.png").as_deref(),
            Some(&b"png-bytes"[..]),
            "大小写不一致也该取到"
        );
        assert!(container.read("OEBPS/Images/nope.png").is_none());
    }

    /// **取不到字节不等于这本书坏了**：文件被删掉之后，每一次取字节都安静地回
    /// `None`，不 panic、也不把「开不出来」缓存成永久失败（文件可能被同步回来）。
    #[test]
    fn a_missing_file_degrades_quietly_and_recovers() {
        let dir = archive_with(&[("a.png", b"aaa")]);
        let path = dir.path().join("case.zip");
        let container = Container::new(&path);

        // 先正常取一次，把容器开着。
        assert!(container.read("a.png").is_some());
        assert!(container.is_open());

        // 换一份「容器开不出来」的：一个还没开的 Container 指向不存在的文件。
        let gone = Container::new(&dir.path().join("nope.zip"));
        assert!(gone.read("a.png").is_none(), "文件不在就回 None");
        assert!(!gone.is_open(), "开失败不留痕：下次还会再试");

        // 文件回来了（同一个路径），下一次取字节就该成功 —— 这就是「不缓存失败」。
        std::fs::copy(&path, dir.path().join("nope.zip")).unwrap();
        assert!(
            gone.read("a.png").is_some(),
            "文件回来之后要能取到，失败不许被记成永久"
        );
    }

    /// 文本条目走零拷贝那条路，非法字节降级成替换字符（与 `Zip::read_text` 同口径）。
    #[test]
    fn text_entries_read_back_and_bad_bytes_degrade() {
        let dir = archive_with(&[
            ("good.xhtml", "<html>中文</html>".as_bytes()),
            ("bad.xhtml", &[0xff, 0xfe, b'a']),
        ]);
        let container = Container::new(&dir.path().join("case.zip"));
        assert_eq!(
            container.read_text("good.xhtml").as_deref(),
            Some("<html>中文</html>")
        );
        assert!(
            container
                .read_text("bad.xhtml")
                .expect("非法字节也该读出东西")
                .starts_with('\u{fffd}')
        );
    }
}
