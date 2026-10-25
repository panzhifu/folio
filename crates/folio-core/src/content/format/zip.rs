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

use crate::infra::error::{IoResultExt, Result};
use crate::infra::metrics::Metric;

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

/// 容器读的缓冲大小。
///
/// 取一个条目是「seek 到它的偏移，然后一路顺序读」，而解压层每次向这个缓冲要一块
/// （flate2 的 bufread 走 `fill_buf`，一次 fill 就是一个 `read(2)`）：默认 8 KiB 意味着
/// 《斗战狂潮》那 6.9 MB 压缩正文要八百多次进内核。取 128 KiB 就是把同样的字节数摊到
/// 约十六分之一那么多次调用上；代价是每个 worker 各持一份缓冲（并行上限 4 路，几百 KB），
/// 插图那一趟的 32 KiB 前缀也顺带一次填完而不是四次。
///
/// **时间上本机没量到差别**：8 / 64 / 128 KiB 三档交替各跑三轮，`document::open` 的差
/// 全在 ±8% 的摆动以内（64 KiB 那档还全线略慢 —— 没有单调关系，说明是噪声不是趋势）。
/// 所以这一条的依据是**系统调用次数**那个确定量，不是计时；真正指望得上的是冷读闪存
/// 那一侧（手机），那台机器上没测过，别把它当成已证的提速。
const READ_BUFFER: usize = 128 * 1024;

impl Zip {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path).io_path(path)?;
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
        let mut buf = Vec::with_capacity(entry.size().min(1 << 24) as usize);
        entry.read_to_end(&mut buf).ok().map(|_| buf)
    }

    /// 把一条**流式**写进 `dest`，最多 `max` 个解压后字节；多一个都不写。
    ///
    /// 存在的理由是压缩包那一族的经典坑：中央目录声明的解压长度会说谎（写 0、解出
    /// 4 GB 的 zip 炸弹），而整机扫描会把读者机器上**任意**一个 zip 交给我们。所以
    /// 两件事都在这里钉死：**不物化整条**（`io::copy` 走 8 KB 一块，不抱几个 GB 的
    /// `Vec`），**上限按实际写出的字节数判**（声明值那一格正是会撒谎的那一个）。
    ///
    /// `Err` 是给人看的那一句：条目读不到、是目录、超出上限、或写盘挂了。失败时
    /// `dest` 可能留下半份文件，**删它是调用方的事**（它才知道那份半成品在暂存区的
    /// 哪一个名字上）。多取一位（`max + 1`）就够分辨「正好 `max`」与「超过 `max`」。
    ///
    /// **压缩方法不支持时要说人话**：`zip` 只开了纯 Rust 的那几族（见根 `Cargo.toml`），
    /// 撞到 zstd 那一类会给出一句通用的 `Unsupported compression method`，直接报给读者
    /// 等于什么都没说 —— 所以这里把那一类统一换成「这一版不支持这个条目用的压缩方法」。
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
        self.index_of(want).map(|at| self.entries[at].clone())
    }

    /// [`locate`] 的另一头：只要条目下标、不要名字。
    ///
    /// 主线程把「名字 → 区间」这一步做掉之后，worker 拿的是 [`EntryRange`]，一次字符串
    /// 都不用复制；这一格就是那条路的第一步。
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

    /// 解出第 `at` 个条目的压缩区间。
    ///
    /// 这一步 zip-rs 会读该条目的**本地头**（载荷起点要在本地头之后算，zip64 的 extra
    /// 字段还会改它），所以它不是纯查表 —— 但一次一本书每条目只读一次，而不是每个 worker
    /// 每次读都重读一遍。
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

/// 一条压缩数据的区间与算法：主线程从中央目录 + 本地头解一次，worker 按它定位读。
///
/// 存在的理由：`ZipArchive::new` 要读中央目录并建它自己的名字表（实测 1233 条目的书
/// 1.35 ms，我们那两张表再 +0.18 ms），而并行索引现在开四份容器 = 四份这样的钱。
/// 名字只有主线程要（清单里写的是路径），worker 要的从来就是「在哪儿、多长、怎么压的」。
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
    /// 这条能不能走「自己按偏移 inflate」的快路。
    ///
    /// 快路只认 stored 与 deflate：它们是真书里的绝大多数，而 bzip2 / LZMA / XZ / PPMd /
    /// Deflate64 在本仓库的语料里是少数。为少数方法把 inflate 的零拷贝路径改复杂不划算，
    /// 所以那一档回落到 zip-rs（代价是要重新打开一次中央目录，见 [`EntryReader::pump_zip`]）。
    pub(crate) fn is_inline(&self) -> bool {
        matches!(
            self.method,
            zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated
        )
    }
}

/// 一块解压产物的交付大小。
///
/// inflate 每吃一块输入就往这里写一块，`with` 拿到的永远是它的前缀。取 32 KiB 是照
/// flate2 自己的中间缓冲来的（`deflate/read.rs` 那句 `vec![0; 32 * 1024]`），而它是
/// **每个条目拨一次**、这里是**每个 worker 拨一次** —— 那正是累积分配回头的那条。
const CHUNK: usize = 32 * 1024;

/// 一个 worker 的取字节通道：自己的 `File` + 主线程给的 [`EntryRange`]。
///
/// 与 [`Zip`] 的分工是「有没有名字」：`Zip` 读中央目录、建两张表、按名字取；这一层
/// 只知道偏移，于是开一次只是一个 `File::open`（几微秒）。CRC 也跟着自己校 ——
/// 那原本是 zip-rs 在流末尾替我们做的事，接管数据读取就等于接手它。
///
/// 三块状态**全部跨条目复用**（句柄、压缩侧缓冲、解压会话），因为「每条目新建一次」是
/// 实测付过的钱：`flate2::read::DeflateDecoder::new` 每次调用都要新拨一块 32 KiB 中间
/// 缓冲，一千多章的书因此多分配 90 MB。而 `Decompress::reset` 的文档写明它
/// 「may not allocate memory, though, and attempts to reuse any previously existing
/// resources」，所以逐条目重置会话、不新建会话。
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

/// 从 `at` 起、最多 `left` 字节的位置读适配器。
///
/// unix 走 `read_at`：不动文件游标，也不为每次读多发一个 seek；Android 与桌面都是 unix。
/// 别的平台退回 `seek + read` —— 一个 `EntryReader` 只归一个线程，游标没有被抢的风险。
/// 读到 `left` 就回 `Ok(0)`，所以「压缩块喂完了」这个判断不需要额外一次系统调用。
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

    /// 把 `range` 那条的解压产物一块一块交给 `with`，回 `(产出的字节数, 走到流末尾没)`。
    ///
    /// `limit` 是**解压后**的产出上限，到数就停。停在那里意味着没读到流末尾，而 zip 的
    /// CRC 只在末尾校，所以前缀读不校 —— 尾部坏掉、头部完好的插图仍然给得出尺寸，
    /// 而不是整张从清单里消失（与 zip-rs 同口径）。
    /// `with` 回 `false` 也提前停，那种停在 [`EntryReader::stream`] 里要和「流断了」分得开，
    /// 所以由调用方自己记着是不是主动停的。
    ///
    /// 驱动方式照 `flate2::zio::read`：输入喂空且源到末尾才用 `Finish`，否则 `None`。
    /// 一处不同：那边「没产出但还没到 EOF」是继续要输入，而我们已经在循环开头灌过一次了，
    /// 手里既还有输入、又还有产出位置却推不动，就是真推不动 —— 直接判读不出来，
    /// 而不是把这里转成死循环。
    fn pump(
        &mut self,
        range: &EntryRange,
        limit: Option<usize>,
        with: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Option<(u64, bool)> {
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
                // 到这里输入已经灌过（或在末尾），仍然一字节没吃掉、一字节没产出 ——
                // 再转下去没有任何东西会变。flate2 在这一步给的是 `UnexpectedEof
                // "incomplete deflate stream"`，这里同口径：整条当读不出来，
                // 而不是把循环转成死循环。
                return None;
            }
            if wrote > 0 {
                produced += wrote as u64;
                if !with(&chunk[..wrote]) {
                    return Some((produced, false));
                }
            }
            ended = done;
        }
        Some((produced, ended))
    }

    /// 非 stored/deflate 的条目：交给 zip-rs 自己的解码器。
    ///
    /// 这条支路要重新打开一次容器（中央目录），所以只在 `EntryRange::is_inline` 为假时
    /// 才会发生。真书里这几族是少数，为它们重写 inflate 的零拷贝路径不划算；同一条通道
    /// 解过多条之后 `slow` 复用，不再重开。
    ///
    /// CRC 与长度仍由调用方（[`EntryReader::read`] / [`EntryReader::stream`]）校，
    /// 这里只负责把解压产物一块块交出去。
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
            if !with(&chunk[..got]) {
                return Some((produced, false));
            }
        }
    }

    /// 整条读出来，末尾校 CRC。长度对不上、CRC 不对、读挂 —— 都回 `None`，
    /// 与 `Zip::read` 在同一种坏书上给不出字节这件事一致。
    pub(crate) fn read(&mut self, range: &EntryRange) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(range.raw_len.min(1 << 24) as usize);
        let (produced, ended) = {
            let out = &mut out;
            self.pump(range, None, &mut |chunk| {
                out.extend_from_slice(chunk);
                true
            })?
        };
        if !ended || produced != range.raw_len || !crc_ok(range.crc, &out) {
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

    /// 只取前 `max` 个**解压后**字节，不校 CRC（没读到流末尾）。
    ///
    /// 存在的理由：只要一张图的固有像素尺寸，需要的是解压流最前面那几个字节的头，
    /// 整条读等于把 400 KB 全解出来只为拿 4 个数。
    ///
    /// 回 `Some` 不保证读到了 `max` 字节：条目本来就更短、或头部那段就解不动了。
    /// 尺寸能不能定出来交回调用方判（见 `format::image_dims_from_head` 那三档结局）。
    pub(crate) fn read_head(&mut self, range: &EntryRange, max: u64) -> Option<Vec<u8>> {
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

    /// 边解边交出去，不物化整条。
    ///
    /// 索引一章只要「可见字数 + 第一个标题」，把整章字符串留在手上等于一次 inflate
    /// 的产物再多挂一份（《斗战狂潮》最大那章 103 KB，四个 worker 就是几百 KB 常驻，
    /// 而《蛇结》那种一本 0.9 MB 的书里它是逐章分配逐章释放的碎片）。
    ///
    /// `with` 回 `false` 提前停，那一趟不校 CRC（没读到流末尾，也没得校）。
    /// 读挂、流断了、长度或 CRC 对不上都回 `None`：调用方按「这条整条读不出来」处理
    /// （与 [`EntryReader::read`] 失败同档），不许把已经喂进去的那半截当成一章的字数。
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

/// zip-rs 的压缩方法错误 → 给人看的那一句。
///
/// 这一版只开纯 Rust 的几族（见根 `Cargo.toml`），zstd 一类会给出
/// `Unsupported compression method`。原样报给读者等于什么都没说，所以统一换成
/// 一句能指向「换个工具重打包」的话。
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

    use crate::content::format::zip::{EntryReader, Zip, codec_reason};
    use crate::infra::metrics::{Metric, take_local};

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

    /// 「存而不压缩」那条支路要和 deflate 走同一套承诺：整条读、流式读、前缀读三样
    /// 都取到同样的字节，且两样走到末尾的都在末尾校 CRC。
    ///
    /// 这一条以前不需要测 —— 那三样都住在 zip-rs 里，它对两种方法给的是同一个 `Read`。
    /// 接管数据读取之后末尾的 CRC 是我们自己校的、支路也是自己写的，所以断言得自己立。
    /// 漫画（CBZ）的页普遍就是 stored，这里红了等于那一整本书的页取不出来。
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

        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt as _;
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .expect("可写句柄");
            file.write_at(&[0u8], range.at + 10).expect("改一个字节");
        }
        let mut reader = EntryReader::open(&path).expect("连句柄都开不出来");

        assert_eq!(reader.read(&range), None, "CRC 不对就不该交出整条字节");
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

    /// bzip2 / XZ / PPMd 这一档走 zip-rs 的回退路：整条读、流式读、前缀读
    /// 三样都要与 `Zip::read` 逐字节相同。LZMA 的写入口在 zip-rs 里是明确不支持
    /// （`LZMA isn't supported for compression`），所以它跟 Deflate64 一样用夹具，
    /// 见下面那条。
    ///
    /// 这一条守的是「开了 feature 但快路不认」这件事：`EntryRange::is_inline` 为假时
    /// 若没有人接住，症状是这几族压缩的整本书所有条目都读不出来 —— 而判定层、页表、
    /// 封面又都还在，屏幕上是「书在、内容空」。
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
