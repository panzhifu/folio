//! 漫画包的磁盘解压缓存（CB7 / CBR / CBT）。
//!
//! 7z、RAR 与 tar 都只有**顺序解压**：要取第 100 页，得从第 1 页一路解过来。滚动阅读时
//! 每一页都重解一遍是 O(n²)，所以第一次打开时把图片条目整体解到磁盘，之后按文件名
//! 现取。缓存是派生数据，删掉只会触发下一次重解 —— **不进数据库、不进书库根**。
//!
//! 目录：`<数据根>/cache/comics/<sha256(路径+长度+mtime)>/`。
//!
//! 解压回调自己校验条目名：7z 里可以写 `../../etc/passwd`，而 `decompress_file` 的
//! 默认实现会照单全收。这里只写「相对、不含 `..`、扩展名是图片」的条目。
//!
//! 键里带着**路径**，而导入那一步是先解在 `.staging` 的名字上、再把它改名成哈希名，
//! 所以 [`relocate`] 是那条改名的配套：不搬，导入那一趟解压就白付，开书时整本再解一遍。
//!
//! 这一层同时是**唯一**会往磁盘上写「比书本身还大」的东西的地方，所以它带两条清理口：
//! 删书时 [`drop_for`] 随手带走那一本，每次解压之后 [`prune`] 把整棵缓存压回上限内
//! （源文件已经不在了的目录先删 —— 那是改名前留下的孤儿与手工拷走的书）。

use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

use super::cbz::is_page_image;
use super::comicinfo::is_comic_info;
use crate::Result;

/// 一次解压的完成标记，内容同时是「这本解出来多少字节」与「它是哪本书解出来的」。
///
/// 两格都记在**标记本身**里有两个用处：[`prune`] 不必为算总量把每本书的目录再走一遍
/// （那是 O(全部页数) 的一次 stat），而源文件消失之后的孤儿也只有这一处认得出来。
const DONE: &str = ".folio-done";

/// 整棵缓存的磁盘上限。**这是一条政策，不是实测**：它要挡的是「没人删书就永远涨」，
/// 而涨的单位是一本几十到几百 MB 的解压产物。真机上该留多少要按设备可用空间量过再定，
/// 量之前就按这一格 —— 改它只需改这一行，代价是多解或少解几本。
const MAX_CACHED_BYTES: u64 = 512 << 20;

/// 这三种容器在磁盘上的形状。解压只有「按顺序把条目倒出来」这一种，其余差别都在
/// 各自的回调里。
#[derive(Clone, Copy)]
enum Container {
    SevenZ,
    Rar,
    Tar,
}

fn cache_root() -> PathBuf {
    #[cfg(test)]
    if let Some(path) = TEST_ROOT.with(|root| root.borrow().clone()) {
        return path;
    }
    crate::infra::paths::data_dir().join("cache").join("comics")
}

/// 缓存目录的键：路径 + 长度 + mtime。内容哈希在导入时算过，但 provider 拿不到它，
/// 而重算整本哈希比解压本身还贵。
fn key_for(path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    if let Ok(meta) = std::fs::metadata(path) {
        hasher.update(meta.len().to_le_bytes());
        if let Ok(modified) = meta.modified()
            && let Ok(since) = modified.duration_since(std::time::UNIX_EPOCH)
        {
            hasher.update(since.as_nanos().to_le_bytes());
        }
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 这本漫画的缓存目录（不保证已经解好）。
pub(crate) fn dir_for(path: &Path) -> PathBuf {
    cache_root().join(key_for(path))
}

/// 确保 7z 里的图片已经解到缓存，返回缓存目录。
pub(crate) fn ensure_sevenz(path: &Path) -> Result<PathBuf> {
    ensure(path, &dir_for(path), Container::SevenZ)
}

/// 确保 RAR（CBR）里的图片已经解到缓存，返回缓存目录。
pub(crate) fn ensure_rar(path: &Path) -> Result<PathBuf> {
    ensure(path, &dir_for(path), Container::Rar)
}

/// 确保 tar（CBT）里的图片已经解到缓存，返回缓存目录。
pub(crate) fn ensure_tar(path: &Path) -> Result<PathBuf> {
    ensure(path, &dir_for(path), Container::Tar)
}

/// 三种容器共用那一步：已解好就认领这份并把它按「最近用过」记一笔，没解过就解一遍、
/// 落标记，然后把整棵缓存压回上限。
fn ensure(path: &Path, dir: &Path, kind: Container) -> Result<PathBuf> {
    if dir.join(DONE).is_file() {
        // 命中的也要盖一次 mtime：[`prune`] 那条 LRU 认的就是它，不盖的话「最近在读的
        // 一本」会被先丢掉（标记只在第一次解压时写过）。
        touch(dir, path);
        return Ok(dir.to_path_buf());
    }
    std::fs::create_dir_all(dir).map_err(|err| {
        crate::Error::UnsupportedFormat(format!(
            "{}：漫画缓存目录建不出来（{err}）",
            file_name(path)
        ))
    })?;
    let extracted: std::result::Result<u64, String> = match kind {
        Container::SevenZ => {
            extract_sevenz(path, dir).map_err(|err| format!("7z 解不出来（{err}）"))
        }
        // rars 的回调只交出一个 `Write`，条目的字节数拿不到，所以解完之后按目录数一遍
        // （一次 O(页数) 的 stat，与解压本身记在同一次事件里，不在读页的热路径上）。
        Container::Rar => extract_rar(path, dir)
            .map_err(|err| format!("RAR 解不出来（{err}）"))
            .map(|()| dir_bytes(dir)),
        Container::Tar => extract_tar(path, dir).map_err(|err| format!("tar 解不出来（{err}）")),
    };
    let bytes = match extracted {
        Ok(bytes) => bytes,
        Err(why) => {
            // 半本缓存比没有缓存更坏：下一次会把它当「已经解好」用。整棵删掉重来。
            let _ = std::fs::remove_dir_all(dir);
            return Err(crate::Error::UnsupportedFormat(format!(
                "{}：{why}",
                file_name(path)
            )));
        }
    };
    crate::infra::metrics::Metric::ComicExtracts.count();
    seal(dir, path, bytes);
    prune();
    Ok(dir.to_path_buf())
}

/// 把这本的缓存目录**搬**到改名之后的那一格（导入的最后一步 rename 前后各问一次）。
///
/// 不搬的后果是安静的：缓存键里带着路径，改名那一趟让它对不上任何一本，于是开书时
/// 整本重解一遍 —— 屏幕上一个字都不差，只是那笔解压又付了一次。
///
/// `from` 是**目录**而不是书的路径，而且必须在改名之前算：键里带着长度与 mtime，
/// 旧名字改名之后已经 stat 不到了，那时再算出来的是第三个键。
pub(crate) fn relocate(from: &Path, to: &Path) {
    let before = from.to_path_buf();
    let after = dir_for(to);
    if before == after || !before.join(DONE).is_file() {
        return;
    }
    if let Some(parent) = after.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::rename(&before, &after) {
        Ok(()) => touch(&after, to),
        Err(_) => {
            // 跨设备（缓存根与副本不在同一个分区）就搬不动。这一份是**派生**数据，
            // 搬不动就当没解过：删掉旧的，让下一次开书重解，别留一个没人认领的目录。
            let _ = std::fs::remove_dir_all(&before);
        }
    }
}

/// 删掉这本书的解压缓存。界面上「删书」连着副本文件一起清的那一步调它。
///
/// 失败只记日志：这是可丢弃的缓存，删不掉的最坏结果是多占一点空间，而下一次
/// [`ensure`] 会发现标记还在、根本不会重做（与 `format::thumb::purge` 同一条纪律）。
pub fn drop_for(path: &Path) {
    let dir = dir_for(path);
    if !dir.exists() {
        return;
    }
    if let Err(err) = std::fs::remove_dir_all(&dir) {
        tracing::warn!("comic: {} 的解压缓存没删掉：{err}", path.display());
    }
}

/// 把整棵缓存压回 [`MAX_CACHED_BYTES`]。
///
/// 两步，各自管一种「不该留」：**源文件已经不在了**的目录（改名前留下的孤儿、被读者
/// 自己拷走的书）先删，那与总量无关；剩下的按标记的 mtime 从最旧的开始丢，直到不超上限。
///
/// 只在「刚解完一本」时跑：那是唯一会让总量涨的事件，而它本来就贵，多走一遍目录不算钱。
///
/// **正在读的那一本没有豁免**：盖 mtime 的只有 [`ensure`]（开书那一次），逐页取字节那一路
/// 不写盘。所以「开着旧漫画 + 导入并解压一本新的 + 总量正好超上限」这一种组合下，被丢掉
/// 的那一本再取字节会拿到空 —— 表现是那一页空着，重新打开这本书会重解一遍。上限那一档
/// （[`MAX_CACHED_BYTES`]）就是为了让这一种组合尽量不发生；真要让「同时开着几本」也豁免，
/// 得给这一层加一个「谁开着」的注册表，那是另一轮的事。
pub(crate) fn prune() {
    let root = cache_root();
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    let mut kept: Vec<(PathBuf, u64, std::time::SystemTime)> = Vec::new();
    let mut total = 0u64;
    for entry in entries.flatten() {
        let dir = entry.path();
        let marker = dir.join(DONE);
        // 没有标记的目录 = 解到一半被杀掉的那一次留下的，下一次 ensure 会自己清。
        let Ok(text) = std::fs::read_to_string(&marker) else {
            continue;
        };
        let (bytes, source) = match text.split_once('\t') {
            Some((bytes, source)) => (bytes.parse::<u64>().unwrap_or(0), source),
            None => (0, ""),
        };
        if !source.is_empty() && !Path::new(source).exists() {
            if std::fs::remove_dir_all(&dir).is_ok() {
                tracing::debug!("comic: {source} 已经不在了，缓存目录跟着清掉");
            }
            continue;
        }
        let at = std::fs::metadata(&marker)
            .and_then(|meta| meta.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        total += bytes;
        kept.push((dir, bytes, at));
    }
    if total <= MAX_CACHED_BYTES {
        return;
    }
    kept.sort_by_key(|(_, _, at)| *at);
    for (dir, bytes, _) in kept {
        if total <= MAX_CACHED_BYTES {
            break;
        }
        if std::fs::remove_dir_all(&dir).is_ok() {
            total = total.saturating_sub(bytes);
            tracing::debug!("comic: 缓存超过上限，丢掉 {dir:?}");
        }
    }
}

/// 写下完成标记：`<解出来的字节>\t<是哪一本书>`。
fn seal(dir: &Path, path: &Path, bytes: u64) {
    let marker = dir.join(DONE);
    if let Err(err) = std::fs::write(&marker, format!("{bytes}\t{}", path.display())) {
        // 标记写不进去 = 这一趟白解（下一次会重解）。解压本身已经成功过，
        // 不值得为一个标记把这本书判死，所以说一句就好。
        tracing::warn!("comic: {} 的缓存标记写不进去（{err}）", path.display());
    }
}

/// 命中时把标记的 mtime 推到「现在」，内容照旧（LRU 认的是 mtime）。
fn touch(dir: &Path, path: &Path) {
    let marker = dir.join(DONE);
    if let Ok(text) = std::fs::read_to_string(&marker) {
        // 老格式的标记（这一格还没记来源时写的空文件）在这里顺手补上：
        // 有了来源，`prune` 才认得出这本是不是孤儿。
        let text = if text.contains('\t') {
            text
        } else {
            format!("0\t{}", path.display())
        };
        let _ = std::fs::write(&marker, text);
    }
}

/// 7z：`decompress_file_with_extract_fn` 给的回调是「一条条目 + 一个读它的流」。
fn extract_sevenz(path: &Path, dir: &Path) -> std::io::Result<u64> {
    let mut written = 0u64;
    // 回调里要累加字节，而它是个 `FnMut`，所以把计数放在外面按引用借进去。
    let mut count = |bytes: u64| written += bytes;
    sevenz_rust2::decompress_file_with_extract_fn(path, dir, |entry, reader, _dest| {
        if entry.is_directory() {
            return Ok(true);
        }
        let name = entry.name();
        let Some(relative) = sanitize_relative(name) else {
            drain(reader)?;
            return Ok(true);
        };
        let href = relative.to_string_lossy().replace('\\', "/");
        if !kept(&href) {
            // 不要的条目也要把这一条读干净，否则同一 solid 块里的下一条会对不齐。
            drain(reader)?;
            return Ok(true);
        }
        let target = dir.join(&relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(&target)?;
        let bytes = std::io::copy(reader, &mut file)?;
        count(bytes);
        Ok(true)
    })
    .map_err(|err| std::io::Error::other(err.to_string()))?;
    Ok(written)
}

/// RAR：`rars` 的 `extract_to` 只要一个「这条写到哪儿」的回调。
fn extract_rar(path: &Path, dir: &Path) -> std::result::Result<(), rars::Error> {
    let archive = rars::ArchiveReader::read_path(path)?;
    archive.extract_to(None, |meta| {
        if meta.is_directory {
            return Ok(Box::new(std::io::sink()));
        }
        let Ok(name) = std::str::from_utf8(&meta.name) else {
            return Ok(Box::new(std::io::sink()));
        };
        let Some(relative) = sanitize_relative(name) else {
            return Ok(Box::new(std::io::sink()));
        };
        let href = relative.to_string_lossy().replace('\\', "/");
        if !kept(&href) {
            return Ok(Box::new(std::io::sink()));
        }
        let target = dir.join(&relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Box::new(std::fs::File::create(target)?))
    })
}

/// tar：顺序过一遍条目，只收图片。
fn extract_tar(path: &Path, dir: &Path) -> std::io::Result<u64> {
    let file = std::fs::File::open(path)?;
    let mut archive = tar::Archive::new(file);
    let mut written = 0u64;
    for entry in archive.entries()? {
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path()?;
        let name = path.to_string_lossy();
        let Some(relative) = sanitize_relative(&name) else {
            continue;
        };
        let href = relative.to_string_lossy().replace('\\', "/");
        if !kept(&href) {
            continue;
        }
        let target = dir.join(&relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(target)?;
        written += std::io::copy(&mut entry, &mut out)?;
    }
    Ok(written)
}

/// 解出来的字节总数：rar 那一路的回调只交出一个 `Write`，拿不到条目的字节数，
/// 所以那一族解完之后按目录数一遍（一次 O(页数) 的 stat，与解压本身同一次事件）。
fn dir_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        total += if path.is_dir() {
            dir_bytes(&path)
        } else if path.file_name().is_some_and(|name| name == DONE) {
            0
        } else {
            std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0)
        };
    }
    total
}

/// 这一条要不要落到磁盘上。**页之外只留一份东西**：`ComicInfo.xml` 那张书目便条。
///
/// 理由是这一族的元数据只有那一个来源（包本身就是「一堆图」，没有 OPF 那种区），而
/// 7z / RAR 只能顺序解压 —— 不在这一步带出来，之后要为一份几百字节的 XML 再把整本倒一遍。
fn kept(href: &str) -> bool {
    is_page_image(href) || is_comic_info(href)
}

/// 缓存里那张书目便条的字节；没有就回空串（调用方按「这本没带便条」处理）。
///
/// 走一遍目录而不是直接 `dir.join("ComicInfo.xml")`：条目名允许带子目录（`info/ComicInfo.xml`
/// 这种包真存在），而解压时保留的是**原始相对路径**。
pub(crate) fn comic_info(dir: &Path) -> Vec<u8> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .eq_ignore_ascii_case(super::comicinfo::FILE)
            }) {
                return std::fs::read(&path).unwrap_or_default();
            }
        }
    }
    Vec::new()
}

/// 缓存目录里的图片页（相对路径，`/` 分隔，自然序）。
pub(crate) fn pages(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    collect(dir, "", &mut out);
    out.sort_by(|a, b| super::cbz::natural_cmp(a, b));
    out
}

fn collect(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == DONE {
            continue;
        }
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let path = entry.path();
        if path.is_dir() {
            collect(&path, &relative, out);
        } else if is_page_image(&relative) {
            out.push(relative);
        }
    }
}

/// 按缓存里的相对路径取一页。
pub(crate) fn read_page(dir: &Path, href: &str) -> Option<Vec<u8>> {
    let relative = sanitize_relative(href)?;
    std::fs::read(dir.join(relative)).ok()
}

/// 7z 条目名 → 安全的相对路径。绝对路径、`..`、盘符一律拒绝。
fn sanitize_relative(name: &str) -> Option<PathBuf> {
    if name.starts_with(['/', '\\']) {
        return None;
    }
    let mut out = PathBuf::new();
    for part in name.replace('\\', "/").split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." || part.contains(':') {
            return None;
        }
        out.push(part);
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

fn drain(reader: &mut dyn std::io::Read) -> std::io::Result<()> {
    std::io::copy(reader, &mut std::io::sink()).map(|_| ())
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
}

#[cfg(test)]
thread_local! {
    static TEST_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// 测试用：把缓存根临时指到一个临时目录。每个 `#[test]` 在自己的线程上跑。
#[cfg(test)]
pub(crate) struct CacheGuard;

#[cfg(test)]
impl CacheGuard {
    pub(crate) fn new(root: PathBuf) -> Self {
        TEST_ROOT.with(|cell| *cell.borrow_mut() = Some(root));
        Self
    }
}

#[cfg(test)]
impl Drop for CacheGuard {
    fn drop(&mut self) {
        TEST_ROOT.with(|cell| *cell.borrow_mut() = None);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::{
        CacheGuard, DONE, dir_for, ensure_rar, ensure_sevenz, ensure_tar, pages, read_page,
        sanitize_relative,
    };

    fn png(width: u32, height: u32) -> Vec<u8> {
        use image::ImageEncoder as _;
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba([7, 7, 7, 255]));
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), width, height, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    #[test]
    fn path_traversal_is_rejected() {
        assert!(sanitize_relative("../../etc/passwd").is_none());
        assert!(sanitize_relative("/etc/passwd").is_none());
        assert!(sanitize_relative("C:/x.png").is_none());
        assert_eq!(
            sanitize_relative("pages/../a.png"),
            None,
            "相对路径里的 .. 也拒"
        );
        assert_eq!(
            sanitize_relative("pages/a.png").unwrap().to_string_lossy(),
            "pages/a.png"
        );
    }

    #[test]
    fn only_image_entries_are_extracted_and_the_cache_is_reused() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("p1.png"), png(4, 5)).unwrap();
        std::fs::write(src.join("p2.png"), png(6, 7)).unwrap();
        std::fs::write(src.join("readme.txt"), b"not a page").unwrap();
        let archive = dir.path().join("a.cb7");
        sevenz_rust2::compress_to_path(&src, &archive).unwrap();

        let cache = dir.path().join("cache");
        let _guard = CacheGuard::new(cache.clone());
        let extracted = ensure_sevenz(&archive).unwrap();
        assert_eq!(pages(&extracted), vec!["p1.png", "p2.png"]);
        assert_eq!(
            read_page(&extracted, "p1.png").as_deref(),
            Some(&png(4, 5)[..])
        );
        assert!(!extracted.join("readme.txt").exists());

        // 第二次不再解：标记在，文件也在。
        let again = ensure_sevenz(&archive).unwrap();
        assert_eq!(again, extracted);
        let marker = std::fs::read_to_string(extracted.join(DONE)).unwrap();
        assert!(
            marker.contains(&archive.display().to_string()),
            "标记里要认得出是哪本书：{marker}"
        );
        assert!(
            marker.split('\t').next().unwrap().parse::<u64>().unwrap() > 0,
            "解出来的字节要记进去，`prune` 那条总量就靠它：{marker}"
        );
    }

    /// 导入的最后一步会把副本改名，而缓存键里带着路径。不搬，那一趟解压就白付：
    /// 下一次开书按新名字算出的键对不上任何东西，整本再解一遍。
    #[test]
    fn a_renamed_copy_takes_its_cache_with_it() {
        use crate::infra::metrics::{Metric, take_local};

        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("p1.png"), png(4, 5)).unwrap();
        let staged = dir.path().join("书.cb7");
        sevenz_rust2::compress_to_path(&src, &staged).unwrap();

        let cache = dir.path().join("cache");
        let _guard = CacheGuard::new(cache.clone());
        take_local(Metric::ComicExtracts);
        let extracted = ensure_sevenz(&staged).unwrap();
        assert_eq!(take_local(Metric::ComicExtracts), 1);

        let final_path = dir.path().join("tuqddpm7nrheuceu.cb7");
        let before = dir_for(&staged);
        assert_eq!(extracted, before, "ensure 交回来的就是那一格");
        std::fs::rename(&staged, &final_path).unwrap();
        super::relocate(&before, &final_path);

        // 搬完了：按新名字 ensure 一次都不该再解，而页还在。
        let after = ensure_sevenz(&final_path).unwrap();
        assert_ne!(before, after, "键里带着路径，改名前后本来就是两格");
        assert_eq!(pages(&after), vec!["p1.png"]);
        assert_eq!(
            take_local(Metric::ComicExtracts),
            0,
            "改名把缓存一起搬过去了，第二次开不许再解一遍"
        );
        assert!(
            !before.join(DONE).exists(),
            "旧那一格不能留下没人认领的目录"
        );
    }

    /// 删书要连着带走它那一格；别人那一格不许碰。
    #[test]
    fn dropping_a_book_removes_only_its_own_cache() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("p1.png"), png(4, 5)).unwrap();
        let one = dir.path().join("一本.cb7");
        let other = dir.path().join("二本.cb7");
        sevenz_rust2::compress_to_path(&src, &one).unwrap();
        std::fs::rename(src.join("p1.png"), src.join("p2.png")).unwrap();
        sevenz_rust2::compress_to_path(&src, &other).unwrap();

        let cache = dir.path().join("cache");
        let _guard = CacheGuard::new(cache.clone());
        let one_dir = ensure_sevenz(&one).unwrap();
        let other_dir = ensure_sevenz(&other).unwrap();
        super::drop_for(&one);
        assert!(!one_dir.exists(), "删书把它的解压产物一起带走");
        assert!(other_dir.join(DONE).is_file(), "另一本不该受影响");
    }

    /// 源文件已经不在了的那一格（改名前留下的、被读者拷走的）在下次解压时被清掉。
    #[test]
    fn a_cache_whose_book_is_gone_is_cleaned_on_the_next_extract() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("p1.png"), png(4, 5)).unwrap();
        let ghost = dir.path().join("已经不在了.cb7");
        let archive = dir.path().join("a.cb7");
        sevenz_rust2::compress_to_path(&src, &archive).unwrap();

        let cache = dir.path().join("cache");
        let _guard = CacheGuard::new(cache.clone());
        // 先造一个「书没了但缓存还在」的格子：直接按那本书的键写标记。
        let ghost_dir = dir_for(&ghost);
        std::fs::create_dir_all(&ghost_dir).unwrap();
        std::fs::write(ghost_dir.join(DONE), format!("10\t{}", ghost.display())).unwrap();
        std::fs::write(ghost_dir.join("p1.png"), png(4, 5)).unwrap();

        ensure_sevenz(&archive).unwrap();
        assert!(!ghost_dir.exists(), "源文件没了的格子该在下次解压时清掉");
    }

    /// RAR 与 tar 两条路共用同一套「只收图片 + 完成标记」的规矩。
    #[test]
    fn rar_and_tar_share_the_same_rules() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CacheGuard::new(dir.path().join("cache"));
        let png = png(4, 5);

        let mut builder = rars::Builder::new(rars::ArchiveVersion::Rar50);
        builder
            .add_bytes(b"p1.png".to_vec(), png.clone(), None, None)
            .unwrap();
        // 越境的条目名 `rars` 的**写入口**自己就拒了（`UnsafePath`），所以那条防线
        // 由 `path_traversal_is_rejected` 单独钉，这里只测「不是图片的不落地」。
        builder
            .add_bytes(b"readme.txt".to_vec(), b"not a page".to_vec(), None, None)
            .unwrap();
        let cbr = dir.path().join("comic.cbr");
        builder.write_to_path(&cbr, None).unwrap();
        let rar = ensure_rar(&cbr).unwrap();
        assert_eq!(pages(&rar), vec!["p1.png"], "不是图片的条目不该落地");
        assert!(!rar.join("readme.txt").exists());

        let tar_dir = dir.path().join("tarpages");
        std::fs::create_dir_all(tar_dir.join("pages")).unwrap();
        std::fs::write(tar_dir.join("pages/p1.png"), png.clone()).unwrap();
        std::fs::write(tar_dir.join("info.txt"), b"not a page").unwrap();
        let cbt = dir.path().join("comic.cbt");
        let file = std::fs::File::create(&cbt).unwrap();
        let mut builder = tar::Builder::new(file);
        builder.append_dir_all(".", &tar_dir).unwrap();
        builder.finish().unwrap();
        let tar = ensure_tar(&cbt).unwrap();
        assert_eq!(pages(&tar), vec!["pages/p1.png"]);
        assert!(!tar.join("info.txt").exists());
        // 再开一次不重解。
        assert_eq!(ensure_tar(&cbt).unwrap(), tar);
    }

    /// 解到一半失败（这条流读挂了）不能留下半本缓存当成完整的一份。
    #[test]
    fn a_failed_extract_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        let _guard = CacheGuard::new(cache.clone());
        // 头四个字节是 7z 的签名，后面全是垃圾：`decompress_file_with_extract_fn` 会失败。
        let broken = dir.path().join("broken.cb7");
        let mut file = std::fs::File::create(&broken).unwrap();
        file.write_all(b"7z\xBC\xAF\x27\x1C garbage").unwrap();
        drop(file);

        assert!(ensure_sevenz(&broken).is_err());
        assert!(
            !dir_for(&broken).exists(),
            "半本缓存下次会被当成解好的那一份"
        );
    }
}
