//! zip 容器取字节。EPUB 与 CBZ 都是 zip，共用这一层。
//!
//! 单独拎出来不是为了少写几行，而是「路径大小写与分隔符和清单写的不一致」这条
//! 宽松匹配只该有一份 —— 两边各写一次，迟早会在同一本书上分叉。

use std::io::{BufReader, Read};
use std::path::Path;

use zip::ZipArchive;

use crate::error::{IoResultExt, Result};

/// 一个已打开的 zip 容器。
pub(crate) struct Zip {
    archive: ZipArchive<BufReader<std::fs::File>>,
}

impl Zip {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path).io_path(path)?;
        let archive = ZipArchive::new(BufReader::new(file)).io_path(path)?;
        Ok(Zip { archive })
    }

    pub(crate) fn read(&mut self, name: &str) -> Option<Vec<u8>> {
        let mut entry = self.archive.by_name(name).ok()?;
        if entry.is_dir() {
            return None;
        }
        let mut buf = Vec::with_capacity(entry.size().min(1 << 24) as usize);
        entry.read_to_end(&mut buf).ok().map(|_| buf)
    }

    pub(crate) fn read_text(&mut self, name: &str) -> Option<String> {
        self.read(name)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    /// 容器里的路径大小写与分隔符都可能和清单写的不一致，做一次宽松匹配。
    pub(crate) fn locate(&self, want: &str) -> Option<String> {
        if self.archive.file_names().any(|n| normalize(n) == want) {
            return Some(want.to_string());
        }
        self.archive
            .file_names()
            .find(|n| normalize(n).eq_ignore_ascii_case(want))
            .map(normalize)
    }

    /// 容器里全部条目的规范路径（分隔符统一成 `/`）。CBZ 按它列页。
    pub(crate) fn names(&self) -> Vec<String> {
        self.archive.file_names().map(normalize).collect()
    }
}

fn normalize(name: &str) -> String {
    name.replace('\\', "/")
}
