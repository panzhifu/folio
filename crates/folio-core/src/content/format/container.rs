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

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use libchm::ChmFile;

use super::chm::lookup_names;
use super::zip::{EntryRange, EntryReader, Zip};
use crate::infra::metrics::Metric;

/// 一种容器家族的字节寻址。键一律是**归一化条目名**（清单里怎么写就怎么给）。
enum Family {
    /// ZIP 一族（EPUB / CBZ / `.fb2.zip`）：名字来自中央目录。
    Zip(Zip),
    /// ITSF 一族（CHM）：名字来自 PMGL 目录，那里**恒带前导斜杠**，所以取的时候要
    /// 过 [`lookup_names`] 那几种写法（我们的 href 不带，见 `chm::ours`）。
    Itsf(ChmFile),
}

/// 一个开一次的容器句柄。
pub struct Container {
    path: PathBuf,
    /// 第一次要字节时才真的开。开失败**不留痕** —— 下次调用会再试一次。
    open: OnceLock<Mutex<Family>>,
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
    fn get(&self) -> Option<&Mutex<Family>> {
        if let Some(open) = self.open.get() {
            return Some(open);
        }
        // 竞态：两个线程同时第一次取字节。家族探测会跑两遍，而
        // `set` 只有一个能成功 —— 输的那个用赢家那份，两遍的代价是白付一次，
        // 比在热路径上加锁便宜，也不会给出两个不同的索引。
        let opened = family_for(&self.path)?;
        let _ = self.open.set(Mutex::new(opened));
        self.open.get()
    }

    /// 按**归一化容器路径**取字节（清单里怎么写就怎么给）。
    ///
    /// `None` 覆盖：容器开不出来、名字查不到（精确与大小写不敏感都不中）、
    /// 条目是目录、解压失败。调用方一律当「这块地方空着」处理。
    ///
    /// **ZIP 那一族的读不在锁里**：锁只盖「名字 → 压缩区间」那一步（一次查表 +
    /// 一次本地头），解压产物由自己的句柄在锁外读。理由是一条实测：八条各 4 MiB 的
    /// 条目，八线程一起读与一个一个读**比值 0.94**（20 核机器）—— 从前 `read` 把整把
    /// 锁按在 inflate 上，于是「一屏同时进三张图」那三趟后台活儿其实是排队的。
    /// 现在每趟各持一个 [`EntryReader`]（一次 `File::open` + 两块缓冲，几十微秒），
    /// 同一台机器同一支仪表读到的是 **130 ms 串行 / 20 ms 八线程 = 比值 6.50**
    /// （八线程的上界是 8，剩下的那一截是 inflate 之外的分配与调度）。
    /// 仪表是 `eight_entries_read_one_by_one_versus_eight_at_once`（`#[ignore]`：
    /// 它量墙钟，只在改并发形状时手动跑，不当回归门用）。
    pub fn read(&self, href: &str) -> Option<Vec<u8>> {
        match self.address(href) {
            Found::Range(range) => EntryReader::open(&self.path)?.read(&range),
            // ITSF 那一族拆不开：libchm 自己持有解码状态，只能在锁内读。
            Found::Itsf => {
                let mut family = self.get()?.lock().unwrap_or_else(|e| e.into_inner());
                let Family::Itsf(file) = &mut *family else {
                    return None;
                };
                for name in lookup_names(href) {
                    if let Ok(bytes) = file.read_path(&name) {
                        return Some(bytes);
                    }
                }
                None
            }
            Found::Absent => None,
        }
    }

    /// [`Container::read`] 的文本版。
    ///
    /// **ITSF 那一族在这里回 `None`**，不是漏了一半：把字节解成文本要问码页，而码页
    /// 是格式知识（CHM 的那个 LCID 在 `#SYSTEM` 里），这一层的立场是「一个格式知识都不
    /// 知道」。回 `None` 让调用方落到 provider —— 那条路带得上码页
    /// （见 [`crate::content::format::chm`]）。代价是每章多一次打开，实测 55 µs，
    /// 比一次 LZX 解压便宜三个数量级。
    ///
    /// 这一条有测试钉住：拿 CHM 走 `read_text` 必须得到 `None`。要是哪天有人为了
    /// 「省一次打开」在这里加一个 lossy UTF-8，那本 GBK 的书会整本变成问号**且不报错** ——
    /// 那是这一族最难查的一种坏法。
    pub fn read_text(&self, href: &str) -> Option<String> {
        match self.address(href) {
            Found::Range(range) => EntryReader::open(&self.path)?.read_text(&range),
            Found::Itsf | Found::Absent => None,
        }
    }

    /// 「名字 → 压缩区间」。**锁只在这一段里持有**，所以它必须不做任何解压。
    fn address(&self, href: &str) -> Found {
        let Some(slot) = self.get() else {
            return Found::Absent;
        };
        let mut family = slot.lock().unwrap_or_else(|e| e.into_inner());
        match &mut *family {
            Family::Zip(zip) => zip.range_at(href).map_or(Found::Absent, Found::Range),
            Family::Itsf(_) => Found::Itsf,
        }
    }
}

/// [`Container::address`] 的结果：能不能把「读」搬到锁外，取决于哪一族。
enum Found {
    /// ZIP 一族：压缩区间拿到了，读在锁外用自己的句柄做。
    Range(EntryRange),
    /// ITSF 一族：libchm 自己持有解码状态，只能在锁内读。
    Itsf,
    /// 这一族里没有这个名字，或容器开不出来。
    Absent,
}

/// 按**头部字节**认容器家族 —— 名字不参与，与判定层（`format::sniff`）同一口径。
///
/// 家族比格式少：`PK\x03\x04` 那一族里 EPUB / CBZ / `.fb2.zip` 用的是同一份寻址；
/// `ITSF` 那一族就是 CHM。认不出家族的（裸文件、PalmDB 那种键不是名字的容器）
/// 回 `None`：这一层只服务「按条目名取字节」这件事，谈不上家族。
fn family_for(path: &Path) -> Option<Family> {
    let mut head = [0u8; 4];
    let mut file = std::fs::File::open(path).ok()?;
    file.read_exact(&mut head).ok()?;
    if head == *b"PK\x03\x04" {
        // `Zip::open` 自己记 `ContainerOpens`（一次打开 = 一次计数）。
        return Some(Family::Zip(Zip::open(path).ok()?));
    }
    if head == *b"ITSF" {
        let opened = ChmFile::open(path).ok()?;
        // libchm 不替我们记这一笔，而「读一本书该只开一次容器」那条断言数的是它。
        Metric::ContainerOpens.count();
        return Some(Family::Itsf(opened));
    }
    None
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::path::Path;

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

    /// ITSF 那一族（CHM）也一样：**一百次取字节只开一次容器**。这一条就是把 CHM 接进
    /// 这一层的全部理由 —— 不接的话翻一章、滚过一张图都要重开一次、重解析一遍目录。
    ///
    /// 同时钉住另一条：`read_text` 对 CHM **必须是 `None`**。文本解码要问码页，而码页是
    /// 格式知识，这一层一个都不该知道。哪天有人为了省一次打开在这里塞一个 lossy
    /// UTF-8，那本 GBK 的书会整本变成问号且不报错 —— 这条断言会先红。
    #[test]
    fn a_chm_opens_once_and_leaves_text_to_the_provider() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/oss/chm/minimal-zh-utf8.chm");
        let container = Container::new(&path);
        assert!(!container.is_open(), "构造时不该开容器");
        take_local(Metric::ContainerOpens);

        let names = ["img/fig.png", "index.htm", "c1.htm"];
        for i in 0..100 {
            let href = names[i % names.len()];
            assert!(
                container.read(href).is_some(),
                "第 {i} 次取 {href} 该有字节"
            );
        }
        assert_eq!(
            take_local(Metric::ContainerOpens),
            1,
            "一百次取字节只该开一次容器"
        );
        assert!(
            container.read("IMG/FIG.PNG").is_some(),
            "清单里的大小写与引用不一致也要取到"
        );
        assert!(container.read("没有这一条").is_none());
        assert!(
            container.read_text("index.htm").is_none(),
            "CHM 的文本解码不归这一层，要落到 provider"
        );
    }

    /// 家族认不出时**安静地没有**：裸文件（TXT / MD / 单文件 HTML）与 PalmDB 那种
    /// 「键不是名字」的容器都走不到这一层，调用方按「这块地方空着」处理。
    #[test]
    fn a_file_that_is_not_a_container_family_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "正文\n").unwrap();
        let container = Container::new(&path);
        assert!(container.read("whatever").is_none());
        assert!(!container.is_open(), "认不出家族就不该留下一个开着的句柄");
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

    /// **并发仪表**（`#[ignore]`：它量墙钟，不当断言用）。
    ///
    /// 问的是：一屏同时进三张图，那三趟「取字节 + inflate」是真并行，还是被容器那把锁
    /// 排成一队？跑法 `cargo test -p folio-core --lib -- --ignored --nocapture`。
    /// 八条各 4 MiB 的合成条目，串行一遍与八线程各读一条对照；比值贴着 1 就是串行，
    /// 贴着核数才是并行。
    #[test]
    #[ignore = "量的是墙钟，不是确定的数；只在改并发形状时手动跑"]
    fn eight_entries_read_one_by_one_versus_eight_at_once() {
        use std::sync::Arc;
        use std::time::Instant;

        const SIZE: usize = 4 * 1024 * 1024;
        // 可压缩的图案：文件本身很小，但解压出来的活是真的。
        let block: Vec<u8> = (0..1024).map(|i| (i % 251) as u8).collect();
        let big: Vec<u8> = block.repeat(SIZE / block.len());
        let names: Vec<String> = (0..8).map(|i| format!("pic-{i}.bin")).collect();
        let entries: Vec<(&str, &[u8])> =
            names.iter().map(|n| (n.as_str(), big.as_slice())).collect();
        let dir = archive_with(&entries);
        let container = Arc::new(Container::new(&dir.path().join("case.zip")));

        let at = Instant::now();
        for name in &names {
            assert_eq!(
                container.read(name).unwrap().len(),
                SIZE,
                "串行那一趟读错了"
            );
        }
        let serial = at.elapsed();

        let at = Instant::now();
        let mut workers = Vec::new();
        for name in &names {
            let container = Arc::clone(&container);
            let name = name.clone();
            workers.push(std::thread::spawn(move || {
                let got = container.read(&name).unwrap();
                got.len()
            }));
        }
        for worker in workers {
            assert_eq!(worker.join().unwrap(), SIZE, "并行那一趟读错了");
        }
        let parallel = at.elapsed();

        println!(
            "串行 {serial:?}｜八线程 {parallel:?}｜比值 {:.2}（本机核数 {}）",
            serial.as_secs_f64() / parallel.as_secs_f64(),
            std::thread::available_parallelism().map_or(0, |n| n.get()),
        );
    }
}
