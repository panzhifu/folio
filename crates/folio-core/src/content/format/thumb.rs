//! 封面缩略图：**一次解码顺带产出两档**，落在可丢弃的磁盘缓存里。
//!
//! `plans/导入管线.md` §八「缩略图」那一段。要点与代价：
//!
//! - 封面原图（常见 800×1200）不能直接进网格：卡片档与列表档差着一个数量级，
//!   而**主要成本是解码那一次，不是缩放** —— 所以一次解码顺手产出两档，
//!   比显示时各缩一遍便宜得多。
//! - 落在 `<书库>/.thumbs/<尺寸>/<封面哈希>.<扩展名>`，**整目录删掉只需重跑生成**。
//!   库里不记图像字节，也不记这一格 —— 它是纯派生数据，没有任何读者数据在里面。
//! - 文档把「导入时生成还是懒生成」列为待决第 2 条，要求先实测 5000 本一批多花
//!   多少秒。所以这一层做成**懒生成**（第一次真要显示时才做），理由是那条实测
//!   数字还不存在：懒生成不拖慢导入，而导入恰恰是读者唯一愿意等待的时刻。
//!   代价照文档写明的认下来：第一次滚动书架时几十张图会同时开工，
//!   而那正是最要 CPU 的时刻 —— 缓解的办法在界面侧（只为看得见的那几格做）。
//!
//! 失败一律回 `None` 而不是 `Err`：缩略图是**可丢弃的缓存**，做不出来时
//! 调用方画原图就好（原图总是拿得到，因为它是缩略图的输入）。

use std::path::Path;

use crate::Result;
use crate::content::format::cover::CoverRef;

/// 卡片档：书架上那一格的大图。与 `paths::THUMB_CARD_PX` 是同一个数。
const CARD: u32 = crate::infra::paths::THUMB_CARD_PX;
/// 列表档：一行一本那种小图。与 `paths::THUMB_LIST_PX` 是同一个数。
const LIST: u32 = crate::infra::paths::THUMB_LIST_PX;

/// 两档尺寸，按「先大后小」排：一次解码要**从小到大**做，缩过的图再缩会累积误差。
pub const SIZES: [u32; 2] = [LIST, CARD];

/// 缩略图的扩展名固定 `jpg`：缩略图是**缓存的中间产物**，不是给人直接打开的文件，
/// 所以不为「保留原格式」付代价（WebP/GIF 的编码器不一定都在图上，
/// 而 JPEG 对照片类的封面最合适）。原格式仍由原图保留着。
const EXT: &str = "jpg";

/// 一份缩略图在磁盘上的位置。
pub fn thumb_path(root: &Path, size: u32, cover: &CoverRef) -> std::path::PathBuf {
    crate::infra::paths::thumb_dir(root, size).join(format!("{}.{EXT}", stem(&cover.hash)))
}

/// 确保某一档缩略图在磁盘上；已经在了就直接交回路径。
///
/// `bytes` 是封面原图的字节（缩略图的输入）。这个函数**不读库也不读原图** ——
/// 输入交给调用方，因为它已经从 `Store::book_cover` 拿到了那份字节（磁盘优先、
/// BLOB 兜底那一条路），再让它自己去取一遍就是第二次真相。
///
/// 回 `None` 覆盖：尺寸不认识、原图解不出来、写不进去。都是「没有缩略图可用」，
/// 调用方退回原图。
///
/// 只要**同一份输入要做两档**，用 [`ensure_all`]：这一条每次调用都解一遍图，
/// 两档就是两次解码。
pub fn ensure(
    root: &Path,
    size: u32,
    cover: &CoverRef,
    bytes: &[u8],
) -> Option<std::path::PathBuf> {
    if !SIZES.contains(&size) {
        return None;
    }
    let dest = thumb_path(root, size, cover);
    if dest.is_file() {
        return Some(dest);
    }
    let image = decode(bytes)?;
    let encoded = encode_from(&image, size)?;
    write_thumb(root, size, cover, &encoded)
}

/// 一次解码产出**所有缺的档**。
///
/// 与逐个 `ensure` 的唯一区别是解码次数：像素只解一遍，两档都从同一份 `DynamicImage`
/// 缩。缩放与编码一次都省不掉（那两档是两个不同的文件），所以这一条的收益就是
/// 少掉的那次解码 —— 实测封面那一档尺寸上约 3-7 ms，缩略图的产出**逐字节不变**。
///
/// 已经在磁盘上的档不重做，也不为它解码；两档都在时整条不碰图像。
/// 回数组与 [`SIZES`] 同序，取不到的那格是 `None`。
pub fn ensure_all(
    root: &Path,
    cover: &CoverRef,
    bytes: &[u8],
) -> [Option<std::path::PathBuf>; SIZES.len()] {
    let missing = SIZES
        .iter()
        .filter(|size| !dest_exists(root, **size, cover))
        .count();
    // 一幅都不缺就一趟图像都不做 —— 重导同一本书是常见事。
    let image = match missing {
        0 => {
            return std::array::from_fn(|i| Some(thumb_path(root, SIZES[i], cover)));
        }
        _ => match decode(bytes) {
            Some(image) => image,
            // 解不出来：缺的那些格回 `None`，在位的那些照旧交回路径。
            None => {
                return std::array::from_fn(|i| {
                    dest_exists(root, SIZES[i], cover).then(|| thumb_path(root, SIZES[i], cover))
                });
            }
        },
    };
    std::array::from_fn(|i| {
        let size = SIZES[i];
        if dest_exists(root, size, cover) {
            return Some(thumb_path(root, size, cover));
        }
        let encoded = encode_from(&image, size)?;
        write_thumb(root, size, cover, &encoded)
    })
}

/// 某一档是否已经在磁盘上。
fn dest_exists(root: &Path, size: u32, cover: &CoverRef) -> bool {
    SIZES.contains(&size) && thumb_path(root, size, cover).is_file()
}

/// 写一档缩略图：先写临时名再 rename（[`ensure`] 那边说了为什么）。
fn write_thumb(
    root: &Path,
    size: u32,
    cover: &CoverRef,
    encoded: &[u8],
) -> Option<std::path::PathBuf> {
    let dest = thumb_path(root, size, cover);
    let dir = crate::infra::paths::thumb_dir(root, size);
    std::fs::create_dir_all(&dir).ok()?;
    // 半写完的缩略图会被当成一张完好的图画出来，而内容是截断的 ——
    // 那种「图是花的」比「没有缩略图」难查得多。
    let tmp = dir.join(format!(".{}.part", stem(&cover.hash)));
    std::fs::write(&tmp, encoded).ok()?;
    std::fs::rename(&tmp, &dest).ok()?;
    Some(dest)
}

// 解码次数。只随测试编进来：这一层「一份输入解一次图」的承诺该由一个整数来守，
// 而不是计时（计时在 CI 里被调度噪声牵着走，见 `infra::metrics` 那段）。
#[cfg(test)]
thread_local! {
    static DECODES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// 解一遍封面字节。整层里只有这一步是「同一份输入做多次没意义」的那种贵操作，
/// 所以它带一个只在测试下存在的计数器（见测试里的 `decodes`）。
fn decode(bytes: &[u8]) -> Option<image::DynamicImage> {
    #[cfg(test)]
    DECODES.with(|c| c.set(c.get() + 1));
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()
}

/// 把**已解开的**图缩到 `size` 以内并编码成 JPEG。
///
/// `size` 是**长边上限**，不是目标宽度：封面有横有竖，按长边缩才能保证
/// 「两档的高度差别」在横竖两种封面上都成立。
fn encode_from(image: &image::DynamicImage, size: u32) -> Option<Vec<u8>> {
    // `thumbnail` 而不是 `resize_exact`：前者按长边缩进 `size` 的方框、自己保住长宽比，
    // 后者要把目标宽高算一遍（算错就是把封面拉变形）。几何上等价的另一条是
    // `resize(size, size, filter)` —— 它同样保比例（两者都走 `resize_dimensions` 的
    // `fill = false` 那一支），区别只在采样：`thumbnail` 是整数块平均，`resize` 是卷积滤波。
    let scaled = image.thumbnail(size, size);
    let mut out = Vec::new();
    // 质量 80：缩略图放在网格里看，这个档位在「几 KB」与「看得出糊」之间。
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80);
    // JPEG 不吃带 alpha 的像素，转成 RGB8 再编 —— 不转的话 PNG 封面（很常见）
    // 会在这一步直接失败。
    let rgb = scaled.to_rgb8();
    encoder
        .encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .ok()?;
    Some(out)
}

/// 哈希的文件名主名 —— 与 `format::cover` 同一个截断口径（前 20 字）。
///
/// 两处必须一致：缩略图是按封面哈希命名的，口径漂了就会「同一张封面各缩一份」。
fn stem(hash: &str) -> String {
    hash.chars().take(20).collect()
}

/// 清掉整棵缩略图缓存树。读到旧版本、或换了尺寸档位时重跑一遍就行。
///
/// 返回删掉的文件数。删失败只记日志：这是可丢弃的缓存，
/// **删不掉不是错误**（最坏是多占一点空间，下次 `ensure` 会发现文件已存在）。
pub fn purge(root: &Path) -> Result<usize> {
    use crate::infra::error::IoResultExt as _;

    let dir = crate::infra::paths::thumbs_dir(root);
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut removed = 0usize;
    // 删失败**不算错**：这是可丢弃的缓存，最坏是多占一点空间，而下次 `ensure`
    // 会发现文件已存在、根本不重做。所以这里逐条 `is_ok()` 而不是把错误往上抛 ——
    // 一个删不掉的权限问题不该让「清缓存」整个失败。
    for entry in std::fs::read_dir(&dir).io_path(&dir)?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            for inner in std::fs::read_dir(&path).io_path(&path)?.flatten() {
                if std::fs::remove_file(inner.path()).is_ok() {
                    removed += 1;
                }
            }
        } else if std::fs::remove_file(&path).is_ok() {
            // 顶层也扫一遍：`.part` 残留与手工放进去的东西。
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::{DECODES, SIZES, ensure, ensure_all, purge, thumb_path};
    use crate::content::format::cover::CoverRef;

    /// 读并清零解码计数。每条测试开头先清一次：cargo 让同一条线程跑过多条测试，
    /// 不清就会读到上一条的账。
    fn decodes() -> u32 {
        DECODES.with(|c| c.replace(0))
    }

    /// 一张真的 PNG（`image` 造得出来），交回它的字节。
    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut buf = Vec::new();
        let img = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    fn cover_ref() -> CoverRef {
        CoverRef {
            hash: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".into(),
            mime: "image/png".into(),
        }
    }

    fn decode_size(path: &std::path::Path) -> (u32, u32) {
        let img = image::ImageReader::open(path)
            .unwrap()
            .with_guessed_format()
            .unwrap()
            .decode()
            .unwrap();
        (img.width(), img.height())
    }

    /// **这一层的核心承诺**：两档都做得出、都真的缩到长边上限以内，
    /// 而且**原比例没变**（封面被拉变形是肉眼可见的事故）。
    #[test]
    fn both_sizes_are_produced_and_keep_the_aspect_ratio() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // 一张竖版封面，比两档都大。
        let bytes = png(1200, 1800);
        let cover = cover_ref();

        for size in SIZES {
            let path = ensure(root, size, &cover, &bytes).expect("该做得出来");
            assert!(path.is_file(), "{size} 档该落盘");
            let (w, h) = decode_size(&path);
            assert!(
                w <= size && h <= size,
                "{size} 档的长边该在 {size} 以内，实际 {w}×{h}"
            );
            // 原比例 1200:1800 = 2:3。
            let want = 2.0 / 3.0;
            let got = w as f32 / h as f32;
            assert!(
                (got - want).abs() < 0.02,
                "{size} 档比例该仍是 2:3，实际 {got}"
            );
        }
        // 两档落在各自的目录下，互不覆盖。
        assert_ne!(
            thumb_path(root, SIZES[0], &cover),
            thumb_path(root, SIZES[1], &cover)
        );
    }

    /// **这一层的承诺：一份封面只解一次图。**
    ///
    /// 数的是解码次数而不是毫秒 —— 计时在 CI 里被调度噪声牵着走，而「做两档该解几次图」
    /// 有确定的整数答案。退回逐档 `ensure` 时这一条立刻翻红。
    #[test]
    fn both_sizes_share_a_single_decode() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let bytes = png(1200, 1800);
        let cover = cover_ref();
        decodes();

        let made = ensure_all(root, &cover, &bytes);
        for (size, path) in SIZES.iter().zip(made.iter()) {
            let path = path
                .as_ref()
                .unwrap_or_else(|| panic!("{size} 档该做得出来"));
            assert!(path.is_file(), "{size} 档该落盘");
        }
        assert_eq!(decodes(), 1, "两档合起来只该解一次图");
    }

    /// 复用解码**不许改变产出**：两档的字节与逐个 `ensure` 做出来的逐字节相同。
    ///
    /// 这条是那次改动的护栏 —— 少解一次图不该让任何一档的像素变一个点，
    /// 而「看着一样」不是判据，字节相等才是。
    #[test]
    fn reusing_the_decode_leaves_the_files_byte_identical() {
        let one_by_one = tempfile::tempdir().unwrap();
        let shared = tempfile::tempdir().unwrap();
        let bytes = png(900, 1400);
        let cover = cover_ref();

        for size in SIZES {
            ensure(one_by_one.path(), size, &cover, &bytes).unwrap();
        }
        ensure_all(shared.path(), &cover, &bytes);

        for size in SIZES {
            let a = std::fs::read(thumb_path(one_by_one.path(), size, &cover)).unwrap();
            let b = std::fs::read(thumb_path(shared.path(), size, &cover)).unwrap();
            assert_eq!(a.len(), b.len(), "{size} 档的产出字节数变了");
            assert_eq!(a, b, "{size} 档的产出字节变了");
        }
    }

    /// 两档都在位时一趟图像都不做：重导同一本书、重扫整库都走这一支。
    #[test]
    fn ensure_all_skips_the_image_work_when_both_sizes_exist() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let bytes = png(400, 600);
        let cover = cover_ref();
        ensure_all(root, &cover, &bytes);
        decodes();

        ensure_all(root, &cover, &bytes);
        assert_eq!(decodes(), 0, "都在位时不该碰图像");
    }

    /// 已经在了就不重做：这条是「第一次滚动书架时才付一次」的前提。
    /// 数的是 mtime 而不是时间，所以不依赖机器快慢。
    #[test]
    fn an_existing_thumbnail_is_not_rebuilt() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let bytes = png(300, 450);
        let cover = cover_ref();

        let first = ensure(root, SIZES[1], &cover, &bytes).unwrap();
        let before = std::fs::metadata(&first).unwrap().modified().unwrap();
        let again = ensure(root, SIZES[1], &cover, &bytes).unwrap();
        assert_eq!(first, again);
        let after = std::fs::metadata(&first).unwrap().modified().unwrap();
        assert_eq!(before, after, "第二次不该重做");
    }

    /// 不认识的尺寸档直接回 `None`（而不是默默做一张错的）：
    /// 档位是缓存目录名的一部分，做错了就是「多一个没人读的目录」。
    #[test]
    fn an_unknown_size_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = png(100, 150);
        assert!(ensure(dir.path(), 999, &cover_ref(), &bytes).is_none());
    }

    /// 原图是坏字节时回 `None` 而不是 panic：缩略图是可丢弃的缓存，
    /// 做不出来时调用方画原图（或占位）就好。
    #[test]
    fn broken_bytes_degrade_to_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ensure(dir.path(), SIZES[0], &cover_ref(), b"not-an-image").is_none());
        assert!(
            ensure(dir.path(), SIZES[0], &cover_ref(), b"").is_none(),
            "空字节也该安静地回 None"
        );
    }

    /// 带 alpha 的 PNG 也要能做出缩略图：JPEG 不吃 alpha，转 RGB 那一步不能漏
    /// （漏了就是「PNG 封面全都没有缩略图」，而 PNG 封面很常见）。
    #[test]
    fn png_with_alpha_still_encodes() {
        let dir = tempfile::tempdir().unwrap();
        let mut buf = Vec::new();
        let img = image::RgbaImage::from_fn(200, 300, |_, _| image::Rgba([10, 20, 30, 128]));
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        assert!(
            ensure(dir.path(), SIZES[0], &cover_ref(), &buf).is_some(),
            "带 alpha 的 PNG 该做得出缩略图"
        );
    }

    /// 整目录删掉之后重跑生成：缓存是**可丢弃**的，这是它叫缓存的全部意义。
    #[test]
    fn purging_the_cache_just_means_regenerating() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let bytes = png(300, 450);
        let cover = cover_ref();
        for size in SIZES {
            ensure(root, size, &cover, &bytes).unwrap();
        }
        let removed = purge(root).unwrap();
        assert_eq!(removed, SIZES.len(), "两档都该被清掉，实际 {removed}");
        assert!(
            !crate::infra::paths::thumb_dir(root, SIZES[0])
                .join("x.jpg")
                .exists()
        );
        // 重跑生成照旧成功。
        assert!(ensure(root, SIZES[0], &cover, &bytes).is_some());
        // 没有缓存目录时 purge 不报错（幂等）。
        purge(root).unwrap();
        std::fs::remove_dir_all(crate::infra::paths::thumbs_dir(root)).unwrap();
        assert_eq!(purge(root).unwrap(), 0, "目录不在时回 0");
    }
}
