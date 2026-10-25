//! RAR / 7z / tar 的共同读取层：**列条目**与**按条目抽一份到磁盘**。
//!
//! 与 [`zip`](super::zip) 的分工：ZIP 的中央目录在文件末尾，能先建一张名字表再随机取；
//! 这三种只有顺序解压，所以这一层不提供「按偏移读」，只服务两个地方：
//!
//! - 判定层问「这个包是漫画还是书包」时要先看条目名；
//! - 导入管线退一层时，要把里面那一本书抽到暂存区。
//!
//! 漫画整包解压仍归 [`comic_cache`](super::comic_cache)：那边产出的是**整本缓存**
//! （页 + `ComicInfo.xml`），与这里「只抽一条」是两件事。
//!
//! ## 三条约束
//!
//! **一、只读、不猜密码**：加密的 RAR / 7z 在列目录时还能给出名字，取字节时会明确失败；
//! 这一版没有密码入口，也不在这层偷偷试。
//!
//! **二、上限按实际写出的字节数判**：与 [`Zip::extract_to`](super::zip::Zip) 同一条
//! 纪律 —— 归档里声明的长度会说谎，而整机扫描会把读者机器上任意一个包交给我们。
//!
//! **三、失败只回 `None`**：读不出来、超过上限、写盘挂了都是 `None`；半份文件由调用方
//! 删（它才知道那份半成品在暂存区的哪一个名字上）。

use std::io::Read as _;
use std::path::Path;

use super::zip::Zip;

/// 能塞得下书的四种归档容器。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Family {
    Zip,
    Rar,
    SevenZ,
    Tar,
}

impl Family {
    /// 按**头部字节**认家族。名字不参与，与判定层同一口径。
    ///
    /// 512 是 tar 头一整个块的大小：`ustar` 在偏移 257，只读 8 字节看不到。
    pub(crate) fn of(head: &[u8]) -> Option<Self> {
        if head.starts_with(b"PK\x03\x04") {
            return Some(Self::Zip);
        }
        if head.starts_with(b"Rar!\x1A\x07") {
            return Some(Self::Rar);
        }
        if head.starts_with(b"7z\xBC\xAF\x27\x1C") {
            return Some(Self::SevenZ);
        }
        if head.len() >= 262 && &head[257..262] == b"ustar" {
            return Some(Self::Tar);
        }
        None
    }

    /// 这个家族的「漫画页」格式名。四种容器都各有一档：ZIP 的 CBZ、RAR 的 CBR、
    /// 7z 的 CB7、tar 的 CBT。
    pub(crate) fn comic_format(self) -> super::Format {
        match self {
            Self::Zip => super::Format::Cbz,
            Self::Rar => super::Format::Cbr,
            Self::SevenZ => super::Format::Cb7,
            Self::Tar => super::Format::Cbt,
        }
    }
}

/// 列条目：只交**文件**，目录与重定向条目跳过；名字统一成 `/` 分隔。
///
/// `None` = 容器打不开或目录读坏了。条目名本身在这一层不做安全校验：这一层只把名字
/// 交给「按名抽一条」，抽出来的字节写在调用方自己的目标路径上，不拿条目名拼路径。
pub(crate) fn names(path: &Path, family: Family) -> Option<Vec<String>> {
    match family {
        Family::Zip => {
            let zip = Zip::open(path).ok()?;
            Some(
                zip.names()
                    .into_iter()
                    .filter(|name| !name.ends_with('/'))
                    .collect(),
            )
        }
        Family::Rar => {
            let archive = rars::ArchiveReader::read_path(path).ok()?;
            let mut out = Vec::new();
            for member in archive.members() {
                if member.meta.is_directory || member.meta.is_redirection {
                    continue;
                }
                out.push(display_name(&member.meta.name));
            }
            Some(out)
        }
        Family::SevenZ => {
            let reader =
                sevenz_rust2::ArchiveReader::open(path, sevenz_rust2::Password::empty()).ok()?;
            Some(
                reader
                    .archive()
                    .files
                    .iter()
                    .filter(|file| !file.is_directory())
                    .map(|file| normalize(file.name()))
                    .collect(),
            )
        }
        Family::Tar => {
            let file = std::fs::File::open(path).ok()?;
            let mut archive = tar::Archive::new(file);
            let mut out = Vec::new();
            for entry in archive.entries().ok()? {
                let entry = entry.ok()?;
                if !entry.header().entry_type().is_file() {
                    continue;
                }
                out.push(normalize(&entry.path().ok()?.to_string_lossy()));
            }
            Some(out)
        }
    }
}

/// 把 `name` 那一条流式写到 `dest`，最多 `max` 个解压后字节；多一个都不写。
///
/// 契约与 [`Zip::extract_to`](super::zip::Zip) 相同：`Err` 是给人看的那一句（条目读不到、
/// 超过上限、或写盘挂了）；失败时 `dest` 可能留下半份文件，删它是调用方的事。
pub(crate) fn extract_to(
    path: &Path,
    family: Family,
    name: &str,
    dest: &Path,
    max: u64,
) -> std::result::Result<u64, String> {
    match family {
        Family::Zip => {
            let mut zip = Zip::open(path).map_err(|err| format!("包读不出来：{err:#}"))?;
            zip.extract_to(name, dest, max)
        }
        Family::Rar => extract_rar(path, name, dest, max),
        Family::SevenZ => extract_sevenz(path, name, dest, max),
        Family::Tar => extract_tar(path, name, dest, max),
    }
}

/// RAR：`extract_with_control` 让我们能跳过不要的条目、找到目标之后立刻停。
///
/// **solid 包里不能 `Skip`**：后面的条目依赖前面的解码历史，库里会报
/// `CannotSkipSolidMember`。所以非目标条目在 solid 包里退到 `Extract(sink)`（解码但
/// 不落盘），独立条目才真的跳过。
fn extract_rar(path: &Path, want: &str, dest: &Path, max: u64) -> std::result::Result<u64, String> {
    let archive =
        rars::ArchiveReader::read_path(path).map_err(|err| format!("RAR 读不出来：{err}"))?;
    let mut found = false;
    let outcome = archive.extract_with_control(rars::ArchiveReadOptions::new(), |member| {
        if member.meta.is_directory || member.meta.is_redirection {
            return Ok(rars::ExtractionDecision::Skip);
        }
        if found {
            return Ok(rars::ExtractionDecision::Stop);
        }
        if display_name(&member.meta.name) != want {
            return Ok(if member.is_solid() {
                rars::ExtractionDecision::Extract(Box::new(std::io::sink()))
            } else {
                rars::ExtractionDecision::Skip
            });
        }
        let file = std::fs::File::create(dest)?;
        found = true;
        Ok(rars::ExtractionDecision::Extract(Box::new(Limited {
            file,
            written: 0,
            max,
        })))
    });
    outcome.map_err(|err| format!("RAR 解不出来：{err}"))?;
    if !found {
        return Err("RAR 里没有这一条".to_string());
    }
    let bytes = std::fs::metadata(dest)
        .map_err(|err| format!("RAR 那条写不出来：{err}"))?
        .len();
    if bytes > max {
        return Err(format!("解压后超过 {} MB", max >> 20));
    }
    Ok(bytes)
}

/// 7z：按块顺序解码，回调里只把目标那一条写进 `dest`，其余读干净推进（solid 块
/// 不能跳过前面的条目）。找到目标后回调返回 `false`，库不再解后面的条目。
fn extract_sevenz(
    path: &Path,
    want: &str,
    dest: &Path,
    max: u64,
) -> std::result::Result<u64, String> {
    let mut reader = sevenz_rust2::ArchiveReader::open(path, sevenz_rust2::Password::empty())
        .map_err(|err| format!("7z 读不出来：{err}"))?;
    let mut written = None;
    reader
        .for_each_entries(|entry, data| {
            let name = normalize(entry.name());
            if entry.is_directory() || name != want {
                std::io::copy(data, &mut std::io::sink())?;
                return Ok(true);
            }
            let mut file = std::fs::File::create(dest)?;
            let got = std::io::copy(&mut data.take(max + 1), &mut file)?;
            written = Some(got);
            Ok(false)
        })
        .map_err(|err| format!("7z 解不出来：{err}"))?;
    match written {
        Some(got) if got <= max => Ok(got),
        Some(_) => Err(format!("解压后超过 {} MB", max >> 20)),
        None => Err("7z 里没有这一条".to_string()),
    }
}

/// tar：条目顺序过一遍，名字对上就把这一条流式抄进 `dest`。
fn extract_tar(path: &Path, want: &str, dest: &Path, max: u64) -> std::result::Result<u64, String> {
    let file = std::fs::File::open(path).map_err(|err| format!("tar 读不出来：{err}"))?;
    let mut archive = tar::Archive::new(file);
    let entries = archive
        .entries()
        .map_err(|err| format!("tar 目录读不出来：{err}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|err| format!("tar 条目读不出来：{err}"))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let name = normalize(
            &entry
                .path()
                .map_err(|err| format!("tar 条目名读不出来：{err}"))?
                .to_string_lossy(),
        );
        if name != want {
            continue;
        }
        let mut out = std::fs::File::create(dest).map_err(|err| format!("写不进暂存区：{err}"))?;
        let got = std::io::copy(&mut entry.by_ref().take(max + 1), &mut out)
            .map_err(|err| format!("tar 解不出来：{err}"))?;
        if got > max {
            return Err(format!("解压后超过 {} MB", max >> 20));
        }
        return Ok(got);
    }
    Err("tar 里没有这一条".to_string())
}

/// 一个带解压后字节上限的写口。超过上限就报错，让 rars 把这一趟判失败。
struct Limited {
    file: std::fs::File,
    written: u64,
    max: u64,
}

impl std::io::Write for Limited {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.written + buf.len() as u64 > self.max {
            return Err(std::io::Error::other("解压后超过上限"));
        }
        let got = self.file.write(buf)?;
        self.written += got as u64;
        Ok(got)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// 归档里的原始名字 → 用来比对/显示的那一串。
fn display_name(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw).replace('\\', "/")
}

fn normalize(name: &str) -> String {
    name.replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{Family, extract_to, names};

    /// 四种容器各造一份，条目名与字节都一样：`(family, 归档路径)`。
    fn fixtures(dir: &Path) -> Vec<(Family, PathBuf)> {
        let body: &[u8] = b"0123456789";

        let zip_path = dir.join("a.zip");
        {
            let file = std::fs::File::create(&zip_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let opts = zip::write::SimpleFileOptions::default();
            zip.start_file("book.fb2", opts).unwrap();
            std::io::Write::write_all(&mut zip, body).unwrap();
            zip.finish().unwrap();
        }

        let rar_path = dir.join("a.rar");
        {
            let mut builder = rars::Builder::new(rars::ArchiveVersion::Rar50);
            builder
                .add_bytes(b"book.fb2".to_vec(), body.to_vec(), None, None)
                .unwrap();
            builder.write_to_path(&rar_path, None).unwrap();
        }

        let sevenz_path = dir.join("a.7z");
        {
            let src = dir.join("sevenz-src");
            std::fs::create_dir_all(&src).unwrap();
            std::fs::write(src.join("book.fb2"), body).unwrap();
            sevenz_rust2::compress_to_path(&src, &sevenz_path).unwrap();
        }

        let tar_path = dir.join("a.tar");
        {
            let file = std::fs::File::create(&tar_path).unwrap();
            let mut builder = tar::Builder::new(file);
            let mut header = tar::Header::new_ustar();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "book.fb2", std::io::Cursor::new(body))
                .unwrap();
            builder.finish().unwrap();
        }

        vec![
            (Family::Zip, zip_path),
            (Family::Rar, rar_path),
            (Family::SevenZ, sevenz_path),
            (Family::Tar, tar_path),
        ]
    }

    /// 四种容器的「列条目 → 按名抽一条」必须是同一套承诺：名字一样、字节一样、
    /// 回写的字节数一样。判定层与管线都押在这条上。
    #[test]
    fn every_family_lists_and_extracts_the_same_entry() {
        let tmp = tempfile::tempdir().unwrap();
        for (family, path) in fixtures(tmp.path()) {
            assert_eq!(
                names(&path, family).as_deref(),
                Some(&["book.fb2".to_string()][..]),
                "{family:?} 的条目"
            );
            let dest = tmp.path().join(format!("out-{family:?}"));
            assert_eq!(
                extract_to(&path, family, "book.fb2", &dest, 1024),
                Ok(10),
                "{family:?} 的回写字节数"
            );
            assert_eq!(std::fs::read(&dest).unwrap(), b"0123456789");
        }
    }

    /// 上限按**实际写出**的字节判：四种容器多一个字节都不给。这一条守的是
    /// 「归档里声明的长度会说谎」与「整机扫描会把任意一个包交给我们」那两句话。
    #[test]
    fn the_byte_cap_is_enforced_on_every_family() {
        let tmp = tempfile::tempdir().unwrap();
        for (family, path) in fixtures(tmp.path()) {
            let dest = tmp.path().join(format!("cap-{family:?}"));
            assert!(
                extract_to(&path, family, "book.fb2", &dest, 9).is_err(),
                "{family:?} 超过上限应当整条拒掉"
            );
        }
    }
}
