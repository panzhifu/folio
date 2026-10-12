//! zip 容器取字节。EPUB 与 CBZ 都是 zip，共用这一层。
//!
//! 单独拎出来不是为了少写几行，而是「路径大小写与分隔符和清单写的不一致」这条
//! 宽松匹配只该有一份 —— 两边各写一次，迟早会在同一本书上分叉。
//!
//! ## 条目名索引：一次建表，之后查表
//!
//! 一个容器对象在被用完之前会被查很多次：读封面、读 CSS、读每一章、读章里每一张图。
//! 所以 [`Zip::open`] 时扫一遍中央目录建两张表（原始名数组 + 归一化名到位置的哈希），
//! 之后每次查名是一次哈希命中。
//!
//! 改之前是每次查名把条目表从头扫两遍（先精确、再大小写不敏感），且**每个条目名都分配
//! 一个 String**：一本 300 章的书查 100 张图 = 两万多次分配花在"这个文件叫什么"上。
//! 建表的代价是同样的分配但只做一次，所以只查一次的容器略贵、查很多次的便宜得多
//! —— 而真实用法全是后者。
//!
//! ## 返回的是 zip 里的原始名，不是归一化名
//!
//! 这一点顺带修了一个 bug：归一化把 `\` 换成 `/`，而 `by_name` 要的是 zip 里那个原始串。
//! 少数用反斜杠分隔的容器过去会"查到了但读不出"。现在 [`Zip::locate`] 把内部归一化名
//! 映射回原始名再交出去。

use std::cell::OnceCell;
use std::collections::HashMap;
use std::io::{BufReader, Read};
use std::path::Path;

use zip::ZipArchive;

use crate::error::{IoResultExt, Result};
use crate::metrics::Metric;

/// 一个已打开的 zip 容器。
pub(crate) struct Zip {
    archive: ZipArchive<BufReader<std::fs::File>>,
    /// 中央目录里的原始条目名，按容器内顺序。查名要靠它取回原始串。
    entries: Vec<String>,
    /// 归一化条目名 → `entries` 下标。
    index: HashMap<String, usize>,
    /// 大小写不敏感的兜底表。**只在精确匹配落空时建**：真实书目里绝大多数引用是精确的，
    /// 每次 open 都预先折一遍大小写是白付一倍分配。
    folded: OnceCell<HashMap<String, usize>>,
}

impl Zip {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path).io_path(path)?;
        let archive = ZipArchive::new(BufReader::new(file)).io_path(path)?;
        // 容器打开一次记一次：「读一本书该只开一次容器」那条断言数的是这个。
        Metric::ContainerOpens.count();
        // 下面这三处是全表扫描的唯一入口。一处一次计数，所以一个容器开一次 = 1 遍。
        Metric::EntryNameTablePasses.count();

        let entries: Vec<String> = archive.file_names().map(str::to_string).collect();
        let index: HashMap<String, usize> = entries
            .iter()
            .enumerate()
            .map(|(at, name)| (normalize(name), at))
            .collect();

        Ok(Zip {
            archive,
            entries,
            index,
            folded: OnceCell::new(),
        })
    }

    pub(crate) fn read(&mut self, name: &str) -> Option<Vec<u8>> {
        let mut entry = self.archive.by_name(name).ok()?;
        if entry.is_dir() {
            return None;
        }
        let mut buf = Vec::with_capacity(entry.size().min(1 << 24) as usize);
        entry.read_to_end(&mut buf).ok().map(|_| buf)
    }

    /// 条目文本。合法 UTF-8 时**不复制**：直接吃掉那个 `Vec<u8>`。
    ///
    /// 之前用 `from_utf8_lossy(...).into_owned()`，那在"本来就是 UTF-8"这条最常见的路上
    /// 多付一次整章拷贝。非法字节仍退回替换处理，行为与过去一致。
    pub(crate) fn read_text(&mut self, name: &str) -> Option<String> {
        let bytes = self.read(name)?;
        Some(match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(err) => String::from_utf8_lossy(err.as_bytes()).into_owned(),
        })
    }

    /// 容器里的路径大小写与分隔符都可能和清单写的不一致，做一次宽松匹配。
    /// 返回 zip 里的**原始条目名**，可直接交给 [`Zip::read`]。
    pub(crate) fn locate(&self, want: &str) -> Option<String> {
        if let Some(at) = self.index.get(want) {
            return Some(self.entries[*at].clone());
        }
        let folded = self.folded.get_or_init(|| {
            Metric::EntryNameTablePasses.count();
            self.entries
                .iter()
                .enumerate()
                .map(|(at, name)| (normalize(name).to_ascii_lowercase(), at))
                .collect()
        });
        let key = want.to_ascii_lowercase();
        folded.get(&key).map(|at| self.entries[*at].clone())
    }

    /// 容器里全部条目的规范路径（分隔符统一成 `/`）。CBZ 按它列页。
    ///
    /// 这是一次全表遍历（每个名字都要归一化并分配），所以它记一次表扫描：调用方应当
    /// 一次取完自己排序，不要在循环里调它。
    pub(crate) fn names(&self) -> Vec<String> {
        Metric::EntryNameTablePasses.count();
        self.entries.iter().map(|name| normalize(name)).collect()
    }

    /// 第 0 个条目的**原始**名，加上它是否「存而不压缩」。
    ///
    /// 只有判定层要这个口子（`format::sniff` 认 EPUB：规范把第一个条目定死成
    /// `mimetype`）。`names()` 给的是归一化后的名、也不带顺序语义，而 `read` 不报
    /// 压缩方法，所以单开一条而不是把那两处改形状。
    pub(crate) fn head_entry(&mut self) -> Option<(String, bool)> {
        let file = self.archive.by_index(0).ok()?;
        let stored = file.compression() == zip::CompressionMethod::Stored;
        Some((file.name().to_string(), stored))
    }
}

fn normalize(name: &str) -> String {
    name.replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use crate::format::zip::Zip;
    use crate::metrics::{Metric, take_local};

    /// 合成一个 zip 并交回它的临时目录（目录活得比测试久，所以并行测试不会互相覆盖
    /// 同一个文件名）。条目名故意混进反斜杠与大小写不一致的两类——那正是宽松匹配存在的原因。
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

    /// **量具的第一条断言**：一个容器对象的条目名全表只扫一遍，之后查名走索引。
    ///
    /// 这条数的是次数不是时间，所以在任何机器上结果都一样。它守的是"退回逐次扫描"：
    /// 每一次查名扫一遍全表意味着几百条目的书查 100 张图要分配几万次条目名字符串，
    /// 而那份开销完全落在滚动的热路径上。
    #[test]
    fn one_open_scans_the_name_table_once_no_how_many_times_you_look_up() {
        let dir = archive_with(&[
            ("mimetype", b"application/epub+zip"),
            (
                "OEBPS/Text/ch1.xhtml",
                "<html><body>一</body></html>".as_bytes(),
            ),
            ("OEBPS/Images/cover.png", b"png"),
        ]);
        let path = dir.path().join("case.zip");

        take_local(Metric::ContainerOpens);
        take_local(Metric::EntryNameTablePasses);

        let zip = Zip::open(&path).unwrap();
        for _ in 0..50 {
            assert!(zip.locate("OEBPS/Text/ch1.xhtml").is_some());
        }

        assert_eq!(take_local(Metric::ContainerOpens), 1, "open 只该发生一次");
        assert_eq!(
            take_local(Metric::EntryNameTablePasses),
            1,
            "50 次查名只扫了一遍全表：其余都该走索引"
        );
    }

    /// 清单写小写、容器里是大写这类不一致必须还能查到——这是宽松匹配存在的全部理由。
    /// 那张大小写折叠表只在精确匹配真落空时才建，所以这里会多出**一遍**全表：
    /// 兜底的成本照付，但不为没遇到的情况预付费。
    #[test]
    fn case_insensitive_lookup_pays_for_the_fallback_table_only_when_needed() {
        let dir = archive_with(&[("OEBPS/Images/Cover.PNG", b"png")]);
        let path = dir.path().join("case.zip");
        let zip = Zip::open(&path).unwrap();

        take_local(Metric::EntryNameTablePasses);
        assert!(zip.locate("OEBPS/Images/cover.png").is_some());
        assert_eq!(
            take_local(Metric::EntryNameTablePasses),
            1,
            "第一次不中时建折叠表，扫一遍"
        );

        assert!(zip.locate("OEBPS/Images/cover.png").is_some());
        assert_eq!(
            take_local(Metric::EntryNameTablePasses),
            0,
            "折叠表已存在，第二次查询不再扫全表"
        );

        // 完全不存在的别给个假命中。
        assert!(zip.locate("OEBPS/Images/nope.png").is_none());
    }

    /// 反斜杠分隔的条目：归一化后能查到，而取字节必须用回 zip 里的原始串。
    /// 这条是 `locate` 改返回原始名的理由——归一化名喂给 `by_name` 会"查到了却读不出"。
    #[test]
    fn backslash_entries_are_found_and_still_readable() {
        let dir = archive_with(&[("Text\\ch1.xhtml", "<html><body>一</body></html>".as_bytes())]);
        let path = dir.path().join("case.zip");
        let mut zip = Zip::open(&path).unwrap();

        let found = zip
            .locate("Text/ch1.xhtml")
            .expect("反斜杠条目应当归一化后命中");
        assert_eq!(found, "Text\\ch1.xhtml", "交出去的必须是 zip 里的原始串");
        assert_eq!(
            zip.read_text(&found).as_deref(),
            Some("<html><body>一</body></html>")
        );
    }

    /// 合法 UTF-8 走零拷贝那条路；非法字节仍要能读出东西（替换字符），不许报错——
    /// 一本坏了一章的书要能降级，不能整本打不开。
    #[test]
    fn text_is_taken_over_without_copy_when_bytes_are_valid_utf8() {
        let dir = archive_with(&[
            ("good.xhtml", "<html>中文</html>".as_bytes()),
            ("bad.xhtml", &[0xff, 0xfe, b'a']),
        ]);
        let path = dir.path().join("case.zip");
        let mut zip = Zip::open(&path).unwrap();

        assert_eq!(
            zip.read_text("good.xhtml").as_deref(),
            Some("<html>中文</html>")
        );
        let lossy = zip.read_text("bad.xhtml").expect("非法字节也该读出东西");
        assert!(
            lossy.starts_with('\u{fffd}'),
            "替换字符应当出现在非法字节的位置"
        );
    }
}
