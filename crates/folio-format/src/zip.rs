//! EPUB 与 CBZ 共用的 ZIP 取字节与宽松路径匹配。
//! [`Zip::open`] 一次建立原始名和归一化索引，大小写折叠表在首次未命中时懒建。
//! [`Zip::locate`] 返回原始条目名；归一化名不能直接交给 zip-rs 的 `by_name`。

use std::cell::OnceCell;
use std::collections::HashMap;
use std::io::{BufReader, Read};
use std::path::Path;

use zip::ZipArchive;

use crate::metrics::Metric;
use crate::{IoResultExt, Result};

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

/// 每通道的读取缓冲，单位字节；较大的块用于减少顺序解压的系统调用。
/// 代价随 worker 数增长，不代表已有跨设备耗时收益。
const READ_BUFFER: usize = 128 * 1024;

impl Zip {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let mut file = std::fs::File::open(path).io_path(path)?;
        preflight_directory(&mut file).io_path(path)?;
        let archive = ZipArchive::new(BufReader::with_capacity(READ_BUFFER, file)).io_path(path)?;
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
        if entry.size() > crate::limits::MAX_ENTRY_BYTES {
            return None;
        }
        crate::limits::read_bounded(&mut entry, crate::limits::MAX_ENTRY_BYTES).ok()
    }

    /// 流式解压到 `dest`，按实际写入字节判断是否超过 `max`，不信任中央目录声明。
    /// 为探测超限最多写入 `max + 1` 字节；失败后的半成品由调用方删除。
    /// 条目缺失、目录、超限或 I/O 失败返回可展示的错误；不支持的压缩方法经 [`codec_reason`] 转换。
    pub(crate) fn extract_to(
        &mut self,
        name: &str,
        dest: &Path,
        max: u64,
    ) -> std::result::Result<u64, String> {
        let mut entry = self
            .archive
            .by_name(name)
            .map_err(|err| codec_reason(&err.to_string()))?;
        if entry.is_dir() {
            return Err("容器里那一条是目录".to_string());
        }
        let mut file = std::fs::File::create(dest).map_err(|err| format!("写不进暂存区：{err}"))?;
        let written = std::io::copy(&mut entry.by_ref().take(max + 1), &mut file)
            .map_err(|err| codec_reason(&err.to_string()))?;
        if written > max {
            return Err(format!("解压后超过 {} MB", max >> 20));
        }
        Ok(written)
    }

    /// 合法 UTF-8 接管字节缓冲而不复制；非法字节以替换字符降级。
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
        self.index_of(want).map(|at| self.entries[at].clone())
    }

    /// 按宽松路径查下标，供主线程生成 [`EntryRange`]，避免 worker 复制条目名。
    pub(crate) fn index_of(&self, want: &str) -> Option<usize> {
        if let Some(at) = self.index.get(want) {
            return Some(*at);
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
        folded.get(&key).copied()
    }

    /// 读取本地头以解析载荷起点（含 ZIP64 extra），不是纯查表；区间供各 worker 复用。
    pub(crate) fn range_of(&mut self, at: usize) -> Option<EntryRange> {
        let file = self.archive.by_index(at).ok()?;
        if file.is_dir() {
            return None;
        }
        Some(EntryRange {
            index: at,
            at: file.data_start()?,
            comp_len: file.compressed_size(),
            raw_len: file.size(),
            method: file.compression(),
            crc: file.crc32(),
        })
    }

    /// 归一化名 → 区间：[`index_of`] 与 [`range_of`] 合成一步，给「清单里的一项」。
    pub(crate) fn range_at(&mut self, want: &str) -> Option<EntryRange> {
        let at = self.index_of(want)?;
        self.range_of(at)
    }

    /// 容器里全部条目的规范路径（分隔符统一成 `/`）。CBZ 按它列页。
    ///
    /// 这是一次全表遍历（每个名字都要归一化并分配），所以它记一次表扫描：调用方应当
    /// 一次取完自己排序，不要在循环里调它。
    pub(crate) fn names(&self) -> Vec<String> {
        Metric::EntryNameTablePasses.count();
        self.entries.iter().map(|name| normalize(name)).collect()
    }

    /// 第 0 个条目的原始名及是否 stored，供 EPUB 判定检查首条 `mimetype`。
    pub(crate) fn head_entry(&mut self) -> Option<(String, bool)> {
        let file = self.archive.by_index(0).ok()?;
        let stored = file.compression() == zip::CompressionMethod::Stored;
        Some((file.name().to_string(), stored))
    }
}

fn preflight_directory(file: &mut (impl Read + std::io::Seek)) -> std::io::Result<()> {
    use std::io::SeekFrom;
    let len = file.seek(SeekFrom::End(0))?;
    let tail_size = len.min(65_557);
    file.seek(SeekFrom::End(-(tail_size as i64)))?;
    let mut tail = vec![0; tail_size as usize];
    file.read_exact(&mut tail)?;
    let eocd = tail
        .windows(4)
        .enumerate()
        .rev()
        .find_map(|(at, sig)| {
            if sig != b"PK\x05\x06" || at + 22 > tail.len() {
                return None;
            }
            let comment = u16::from_le_bytes([tail[at + 20], tail[at + 21]]) as usize;
            (at + 22 + comment == tail.len()).then_some(at)
        })
        .ok_or_else(crate::limits::exceeded)?;
    let entries16 = u16::from_le_bytes([tail[eocd + 10], tail[eocd + 11]]);
    let directory32 = u32::from_le_bytes(
        tail[eocd + 12..eocd + 16]
            .try_into()
            .map_err(|_| crate::limits::exceeded())?,
    );
    let (entries, directory_bytes) = if entries16 == u16::MAX || directory32 == u32::MAX {
        let end_offset = len - tail_size + eocd as u64;
        if end_offset < 20 {
            return Err(crate::limits::exceeded());
        }
        file.seek(SeekFrom::Start(end_offset - 20))?;
        let mut locator = [0; 20];
        file.read_exact(&mut locator)?;
        if &locator[..4] != b"PK\x06\x07" {
            return Err(crate::limits::exceeded());
        }
        let zip64_offset = u64::from_le_bytes(
            locator[8..16]
                .try_into()
                .map_err(|_| crate::limits::exceeded())?,
        );
        file.seek(SeekFrom::Start(zip64_offset))?;
        let mut record = [0; 56];
        file.read_exact(&mut record)?;
        if &record[..4] != b"PK\x06\x06" {
            return Err(crate::limits::exceeded());
        }
        (
            u64::from_le_bytes(
                record[32..40]
                    .try_into()
                    .map_err(|_| crate::limits::exceeded())?,
            ),
            u64::from_le_bytes(
                record[40..48]
                    .try_into()
                    .map_err(|_| crate::limits::exceeded())?,
            ),
        )
    } else {
        (u64::from(entries16), u64::from(directory32))
    };
    if entries > crate::limits::MAX_ARCHIVE_ENTRIES as u64
        || directory_bytes > crate::limits::MAX_ARCHIVE_DIRECTORY_BYTES
    {
        return Err(crate::limits::exceeded());
    }
    Ok(())
}

/// 主线程从中央目录与本地头解析的区间和算法，worker 无需重建名字表即可按偏移读取。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EntryRange {
    /// 中央目录里的下标。只有非 stored/deflate 的回退路要用它（zip-rs 的 `by_index`）。
    index: usize,
    /// 压缩载荷在容器文件里的起点。
    at: u64,
    /// 压缩后的长度。
    comp_len: u64,
    /// 解压后的长度：整条读的预分配、流式读的「读完了没」都看它。
    raw_len: u64,
    /// 压缩方法。stored / deflate 走本模块的快路，其余交给 zip-rs 的解码器。
    method: zip::CompressionMethod,
    /// 中央目录记的 CRC32。整条读要在末尾校它；前缀读不校（读不到流末尾，也没得校）。
    crc: u32,
}

impl EntryRange {
    /// 中央目录记的解压后长度。字体身份用它是为了不读字节就能分区。
    pub(crate) fn raw_len(&self) -> u64 {
        self.raw_len
    }

    /// 中央目录记的 CRC32。字体身份用它是为了不读字节就能分区。
    pub(crate) fn crc(&self) -> u32 {
        self.crc
    }

    /// stored / deflate 可按偏移直接读取，其余方法由 [`EntryReader::pump_zip`] 回落到 zip-rs。
    pub(crate) fn is_inline(&self) -> bool {
        matches!(
            self.method,
            zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated
        )
    }
}

/// 解压产物缓冲大小，单位字节；每 worker 分配一次，跨条目复用。
const CHUNK: usize = 32 * 1024;

/// 单 worker 的按偏移读取通道；名字解析归 [`Zip`]，完整读取的 CRC 校验归本通道。
/// 文件句柄、缓冲和解压会话跨条目复用，以 `Decompress::reset` 避免逐条重新分配。
pub(crate) struct EntryReader {
    /// 回退路要按名字重开容器；快路只用一个 `File`，不碰它。
    path: std::path::PathBuf,
    file: std::fs::File,
    /// 压缩侧缓冲：一次 `read_at` 灌满，逐块喂给 inflate。
    src: Vec<u8>,
    /// 解压产物缓冲。
    chunk: Vec<u8>,
    decompress: flate2::Decompress,
    /// 非 stored/deflate 条目回落用的 zip-rs 容器。**懒开**，开一次之后本条通道复用。
    slow: Option<zip::ZipArchive<BufReader<std::fs::File>>>,
}

/// 从 `at` 起最多读 `left` 字节，耗尽即返回 `Ok(0)`。
/// Unix 使用 `read_at`，其它平台退回 `seek + read`；每通道只归一个线程，无游标竞争。
struct At<'a> {
    file: &'a mut std::fs::File,
    at: u64,
    left: u64,
}

impl Read for At<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.left == 0 {
            return Ok(0);
        }
        let want = buf.len().min(self.left as usize);
        #[cfg(unix)]
        let got = {
            use std::os::unix::fs::FileExt as _;
            (*self.file).read_at(&mut buf[..want], self.at)?
        };
        #[cfg(not(unix))]
        let got = {
            use std::io::Seek as _;
            self.file.seek(std::io::SeekFrom::Start(self.at))?;
            self.file.read(&mut buf[..want])?
        };
        self.at += got as u64;
        self.left -= got as u64;
        Ok(got)
    }
}

impl EntryReader {
    /// 只要一个句柄：不读中央目录、不建表。两块缓冲与会话各拨一次，之后逐条复用。
    pub(crate) fn open(path: &Path) -> Option<Self> {
        Some(Self {
            path: path.to_path_buf(),
            file: std::fs::File::open(path).ok()?,
            src: vec![0u8; READ_BUFFER],
            chunk: vec![0u8; CHUNK],
            decompress: flate2::Decompress::new(false),
            slow: None,
        })
    }

    /// 逐块交付解压产物，返回 `(产出字节数, 是否到流末尾)`；`limit` 是解压后上限。
    /// 到上限或 `with` 返回 false 均提前停止；前缀不校 CRC，调用方须另记是否主动停止。
    /// 仅在输入耗尽且源到末尾时用 `Finish`；已有输入仍无进展须失败，避免死循环。
    fn pump(
        &mut self,
        range: &EntryRange,
        limit: Option<usize>,
        with: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Option<(u64, bool)> {
        if limit.is_none() && range.raw_len > crate::limits::MAX_ENTRY_BYTES {
            return None;
        }
        let limit = Some(limit.unwrap_or(crate::limits::MAX_ENTRY_BYTES as usize + 1));
        if !range.is_inline() {
            return self.pump_zip(range, limit, with);
        }
        // 四个字段分开借出：`pump` 要同时用句柄、缓冲和会话，整体 `&mut self` 传不进去。
        let Self {
            file,
            src,
            chunk,
            decompress,
            ..
        } = self;
        let mut input = At {
            file,
            at: range.at,
            left: range.comp_len,
        };
        // 原始 deflate（zip 的 deflate 载荷不带 zlib 头）。
        decompress.reset(false);
        // `src[..filled]` 是缓冲里有效的字节，`src[..held]` 是已经喂给 inflate 的那段。
        let mut held = 0usize;
        let mut filled = 0usize;
        let mut eof = false;
        let mut produced = 0u64;
        let mut ended = false;
        while !ended {
            let room = match limit {
                Some(max) if produced >= max as u64 => break,
                Some(max) => chunk
                    .len()
                    .min((max as u64).saturating_sub(produced) as usize),
                None => chunk.len(),
            };
            if held == filled && !eof {
                match input.read(src) {
                    Ok(0) => eof = true,
                    Ok(got) => {
                        held = 0;
                        filled = got;
                    }
                    Err(_) => return None,
                }
            }
            let out = &mut chunk[..room];
            let (done, wrote, ate) = if matches!(range.method, zip::CompressionMethod::Stored) {
                // 存而不压缩：没有 inflate，也就没有流末尾，压缩块喂完就算走完。
                let got = room.min(filled - held);
                out[..got].copy_from_slice(&src[held..held + got]);
                held += got;
                (got == 0, got, got)
            } else {
                let before_in = decompress.total_in();
                let before_out = decompress.total_out();
                let flush = if eof && held == filled {
                    flate2::FlushDecompress::Finish
                } else {
                    flate2::FlushDecompress::None
                };
                let status = decompress.decompress(&src[held..filled], out, flush).ok()?;
                let ate = (decompress.total_in() - before_in) as usize;
                let got = (decompress.total_out() - before_out) as usize;
                held += ate;
                (status == flate2::Status::StreamEnd, got, ate)
            };
            if !done && wrote == 0 && ate == 0 {
                // 已尝试补输入仍无进展，按截断流失败，不能继续空转。
                return None;
            }
            if wrote > 0 {
                produced += wrote as u64;
                if produced > crate::limits::MAX_ENTRY_BYTES {
                    return None;
                }
                if !with(&chunk[..wrote]) {
                    return Some((produced, false));
                }
            }
            ended = done;
        }
        Some((produced, ended))
    }

    /// 非 stored/deflate 条目交给 zip-rs，首次懒开中央目录后复用 `slow`。
    /// CRC 与长度仍由 [`EntryReader::read`] / [`EntryReader::stream`] 校验。
    fn pump_zip(
        &mut self,
        range: &EntryRange,
        limit: Option<usize>,
        with: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Option<(u64, bool)> {
        let Self {
            path, chunk, slow, ..
        } = self;
        if slow.is_none() {
            let file = std::fs::File::open(path.as_path()).ok()?;
            *slow = Some(zip::ZipArchive::new(BufReader::with_capacity(READ_BUFFER, file)).ok()?);
        }
        let mut entry = slow.as_mut()?.by_index(range.index).ok()?;
        let mut produced = 0u64;
        loop {
            let room = match limit {
                Some(max) if produced >= max as u64 => return Some((produced, false)),
                Some(max) => chunk
                    .len()
                    .min((max as u64).saturating_sub(produced) as usize),
                None => chunk.len(),
            };
            let got = entry.read(&mut chunk[..room]).ok()?;
            if got == 0 {
                return Some((produced, true));
            }
            produced += got as u64;
            if produced > crate::limits::MAX_ENTRY_BYTES {
                return None;
            }
            if !with(&chunk[..got]) {
                return Some((produced, false));
            }
        }
    }

    /// 整条读出来，末尾校 CRC。长度对不上、CRC 不对、读挂 —— 都回 `None`，
    /// 与 `Zip::read` 在同一种坏书上给不出字节这件事一致。
    pub(crate) fn read(&mut self, range: &EntryRange) -> Option<Vec<u8>> {
        if range.raw_len > crate::limits::MAX_ENTRY_BYTES {
            return None;
        }
        let mut out = Vec::with_capacity(range.raw_len.min(1 << 24) as usize);
        let (produced, ended) = {
            let out = &mut out;
            self.pump(range, None, &mut |chunk| {
                out.extend_from_slice(chunk);
                true
            })?
        };
        if !ended
            || produced > crate::limits::MAX_ENTRY_BYTES
            || produced != range.raw_len
            || !crc_ok(range.crc, &out)
        {
            return None;
        }
        Some(out)
    }

    /// 整条读成文本。非法字节按 lossy 换（与 `Zip::read_text` 同口径 ——
    /// 章节摘要那条对拍吃的就是这种串）。
    pub(crate) fn read_text(&mut self, range: &EntryRange) -> Option<String> {
        let bytes = self.read(range)?;
        Some(match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(err) => String::from_utf8_lossy(err.as_bytes()).into_owned(),
        })
    }

    /// 最多取 `max` 个解压后字节，不校 CRC，供图片头尺寸探测。
    /// `Some` 不保证达到 `max`，尺寸是否可用由调用方判断。
    pub(crate) fn read_head(&mut self, range: &EntryRange, max: u64) -> Option<Vec<u8>> {
        let max = max.min(crate::limits::MAX_ENTRY_BYTES);
        let mut out = Vec::with_capacity(max as usize);
        let (produced, _) = {
            let out = &mut out;
            self.pump(range, Some(max as usize), &mut |chunk| {
                out.extend_from_slice(chunk);
                true
            })?
        };
        debug_assert!(produced <= max);
        Some(out)
    }

    /// 边解边交付，不保留整章；`with` 返回 false 时提前停止且不校 CRC。
    /// 读取、流完整性、长度或 CRC 失败返回 `None`；调用方不得把已交付的半截当完整结果。
    pub(crate) fn stream(
        &mut self,
        range: &EntryRange,
        with: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Option<()> {
        let mut hasher = crc32fast::Hasher::new();
        let mut stopped = false;
        let (produced, ended) = {
            let (hasher, stopped) = (&mut hasher, &mut stopped);
            self.pump(range, None, &mut |chunk| {
                hasher.update(chunk);
                let keep = with(chunk);
                *stopped |= !keep;
                keep
            })?
        };
        if stopped {
            return Some(());
        }
        let got = hasher.finalize();
        if !ended || produced != range.raw_len || got != range.crc {
            tracing::warn!(
                "zip: 条目 CRC/长度对不上（中央目录 {:x}），整条当读不出来",
                range.crc
            );
            return None;
        }
        Some(())
    }
}

/// CRC32 对得上吗。坏书在这里被挡下来，而不是把半截内容当成一章。
fn crc_ok(want: u32, bytes: &[u8]) -> bool {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(bytes);
    if hasher.finalize() == want {
        return true;
    }
    tracing::warn!("zip: 条目 CRC 对不上（中央目录 {want:#x}），当作读不出来");
    false
}

/// 将不支持的压缩或加密错误转为可展示文案，其余错误保留原文。
fn codec_reason(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("encrypt") || lower.contains("password") {
        "这一版不支持加密的压缩包".to_string()
    } else if lower.contains("unsupported compression") || lower.contains("unsupportedarchive") {
        "这一版不支持这个条目用的压缩方法（zstd 等）".to_string()
    } else {
        raw.to_string()
    }
}

fn normalize(name: &str) -> String {
    name.replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::path::Path;

    use crate::metrics::{Metric, take_local};
    use crate::zip::{EntryReader, Zip, codec_reason};

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

    /// 以次数而非耗时验证：容器只建一次名字表，反复查名不退回全表扫描。
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

    /// 合成一个 zip，逐条决定「存而不压缩」还是 deflate。
    fn archive_with_methods(entries: &[(&str, &[u8], bool)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let file = std::fs::File::create(dir.path().join("case.zip")).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        for (name, data, stored) in entries {
            let options = zip::write::SimpleFileOptions::default().compression_method(if *stored {
                zip::CompressionMethod::Stored
            } else {
                zip::CompressionMethod::Deflated
            });
            zip.start_file(*name, options).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
        dir
    }

    /// stored 与 deflate 的整条、流式、前缀读取应交付相同字节；完整读取必须校验 CRC。
    #[test]
    fn stored_entries_take_the_same_three_paths_as_deflated_ones() {
        // 一段压得动的文本，好让 deflate 那条确实在跑（stored 那本给它同样的字节）。
        let text = "<h1>章</h1><p>正文</p>".repeat(4000).into_bytes();
        let dir = archive_with_methods(&[
            ("deflated.xhtml", &text, false),
            ("stored.xhtml", &text, true),
        ]);
        let path = dir.path().join("case.zip");
        let mut zip = Zip::open(&path).unwrap();
        let mut reader = EntryReader::open(&path).expect("连句柄都开不出来");

        for name in ["deflated.xhtml", "stored.xhtml"] {
            let range = zip.range_at(name).expect("区间");
            let want = zip.read(name).expect("zip-rs 那一路该给出同样的字节");
            assert!(!want.is_empty());

            assert_eq!(
                reader.read(&range).as_deref(),
                Some(want.as_slice()),
                "{name} 整条读"
            );

            let mut got: Vec<u8> = Vec::new();
            let streamed = reader.stream(&range, &mut |chunk| {
                got.extend_from_slice(chunk);
                true
            });
            assert_eq!(streamed, Some(()), "{name} 流式读到末尾并校 CRC");
            assert_eq!(got, want, "{name} 流式的字节必须与整条读逐字节相同");

            assert_eq!(
                reader.read_head(&range, 3).as_deref(),
                Some(&want[..3]),
                "{name} 前缀读"
            );
        }
    }

    /// stored 条目被打坏载荷中间一个字节：整条读与流式读都要挡下来（长度没变，
    /// 认得出这件事的只有末尾的 CRC），而前缀读照给 —— 它本来就不校，那是插图那一支
    /// 「尾部坏掉仍给得出尺寸」的同一条口径。
    #[test]
    fn a_damaged_stored_entry_is_refused_by_the_paths_that_verify() {
        let dir = archive_with_methods(&[("page.png", &[7u8; 4096], true)]);
        let path = dir.path().join("case.zip");
        let mut zip = Zip::open(&path).unwrap();
        let range = zip.range_at("page.png").expect("区间");
        assert_eq!(zip.read("page.png").as_deref(), Some(&[7u8; 4096][..]));

        {
            use std::io::{Seek as _, SeekFrom};
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .expect("可写句柄");
            file.seek(SeekFrom::Start(range.at + 10)).unwrap();
            file.write_all(&[0u8]).expect("改一个字节");
        }
        let mut zip = Zip::open(&path).unwrap();
        assert!(
            zip.read("page.png").is_none(),
            "按名整条读取也须拒绝损坏的 CRC"
        );
        for max in [4096, 4097] {
            assert!(
                zip.extract_to("page.png", &dir.path().join("out"), max)
                    .is_err(),
                "精确上限与宽松上限都须读到 CRC 校验"
            );
        }
        let mut reader = EntryReader::open(&path).expect("连句柄都开不出来");

        assert!(reader.read(&range).is_none(), "CRC 不对就不该交出整条字节");
        let mut counted = 0usize;
        assert_eq!(
            reader.stream(&range, &mut |chunk| {
                counted += chunk.len();
                true
            }),
            None
        );
        assert_eq!(counted, 4096, "流式是走到末尾才发现 CRC 不对的");
        assert_eq!(
            reader.read_head(&range, 8).as_deref(),
            Some(&[7u8; 8][..]),
            "前缀读不校 CRC，头部完好就该给"
        );
    }

    /// 合成一个用指定压缩方法的 zip（LZMA / XZ / PPMd / bzip2 都靠它）。
    fn archive_with_codecs(entries: &[(&str, &[u8], zip::CompressionMethod)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let file = std::fs::File::create(dir.path().join("case.zip")).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        for (name, data, method) in entries {
            let options = zip::write::SimpleFileOptions::default().compression_method(*method);
            zip.start_file(*name, options).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
        dir
    }

    /// bzip2 / XZ / PPMd 的三种读取走回退路，须与 `Zip::read` 逐字节相同。
    /// zip-rs 不支持写入的 LZMA 与 Deflate64 另用下方夹具验证。
    #[test]
    fn bzip2_xz_and_ppmd_take_the_same_three_paths() {
        let text = "<h1>章</h1><p>正文</p>".repeat(4000).into_bytes();
        let dir = archive_with_codecs(&[
            ("bzip2.xhtml", &text, zip::CompressionMethod::Bzip2),
            ("xz.xhtml", &text, zip::CompressionMethod::Xz),
            ("ppmd.xhtml", &text, zip::CompressionMethod::Ppmd),
        ]);
        let path = dir.path().join("case.zip");
        let mut zip = Zip::open(&path).unwrap();
        let mut reader = EntryReader::open(&path).expect("连句柄都开不出来");

        for name in ["bzip2.xhtml", "xz.xhtml", "ppmd.xhtml"] {
            let range = zip.range_at(name).expect("区间");
            assert!(!range.is_inline(), "{name} 不该被快路认成 stored/deflate");
            let want = zip.read(name).expect("zip-rs 那一路该给出字节");
            assert_eq!(
                want, text,
                "{name} 合成时写进去的字节要先自己兑现（否则下面三条是空转）"
            );

            assert_eq!(
                reader.read(&range).as_deref(),
                Some(want.as_slice()),
                "{name} 整条读"
            );

            let mut got: Vec<u8> = Vec::new();
            assert_eq!(
                reader.stream(&range, &mut |chunk| {
                    got.extend_from_slice(chunk);
                    true
                }),
                Some(()),
                "{name} 流式读到末尾并校 CRC"
            );
            assert_eq!(got, want, "{name} 流式的字节必须与整条读逐字节相同");

            assert_eq!(
                reader.read_head(&range, 3).as_deref(),
                Some(&want[..3]),
                "{name} 前缀读"
            );
        }
    }

    /// 一条写不出来的方法（或 7-Zip 最省事的那一种）：夹具是 7-Zip 生成的，
    /// 生成命令见 `tests/fixtures/oss/zip-codecs/README.md`。它同样走回退路。
    fn assert_fixture_round_trips(file: &str, name: &str, len: usize, needle: &[u8]) {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/oss/zip-codecs/");
        let path = path.join(file);
        let mut zip = Zip::open(&path).expect("夹具该打得开");
        let range = zip.range_at(name).expect("区间");
        assert!(!range.is_inline(), "{file} 不是快路认的方法");
        let want = zip.read(name).expect("zip-rs 该解得出夹具");
        assert_eq!(want.len(), len, "夹具的解压长度（README 里记的数）");
        assert!(
            want.windows(needle.len()).any(|window| window == needle),
            "解出来的必须是夹具里那段正文"
        );

        let mut reader = EntryReader::open(&path).expect("连句柄都开不出来");
        assert_eq!(
            reader.read(&range).as_deref(),
            Some(want.as_slice()),
            "整条读"
        );
        let mut got: Vec<u8> = Vec::new();
        assert_eq!(
            reader.stream(&range, &mut |chunk| {
                got.extend_from_slice(chunk);
                true
            }),
            Some(())
        );
        assert_eq!(got, want);
        assert_eq!(
            reader.read_head(&range, 8).as_deref(),
            Some(&want[..8]),
            "前缀读"
        );
    }

    #[test]
    fn deflate64_entries_use_the_zip_rs_fallback() {
        assert_fixture_round_trips("deflate64.zip", "page.xhtml", 8627, b"deflate64 fixture");
    }

    /// zip-rs 的写入口对 LZMA 明确说不（`LZMA isn't supported for compression`），
    /// 所以这一族也只能摆一份 7-Zip 写出来的字节。
    #[test]
    fn lzma_entries_use_the_zip_rs_fallback() {
        assert_fixture_round_trips("lzma.zip", "page.xhtml", 7627, b"lzma fixture");
    }

    /// zip-rs 的压缩方法错误要换成一句读者看得懂的话；其余错误原样留着
    /// （CRC、截断那些本来就说得清）。
    #[test]
    fn unsupported_codec_errors_say_so_in_plain_words() {
        assert_eq!(
            codec_reason("UnsupportedArchive(\"Unsupported compression method 93\")"),
            "这一版不支持这个条目用的压缩方法（zstd 等）"
        );
        assert_eq!(codec_reason("CRC mismatch"), "CRC mismatch");
        assert_eq!(
            codec_reason("UnsupportedArchive(\"AES encryption is not supported\")"),
            "这一版不支持加密的压缩包"
        );
    }
}
