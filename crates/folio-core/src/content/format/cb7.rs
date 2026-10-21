//! CB7（7z 漫画包）：一个 7z，里面按页排着图片。
//!
//! 与 CBZ 同族，差别只在容器：7z 只有顺序解压（要第 100 页得从第 1 页一路解过来），
//! 所以第一次打开时把图片条目解到磁盘缓存，之后按文件名现取。页表、自然序、封面、
//! `ImageRes` 形状与 CBZ 共用 [`super::cbz::build_fixed`]。
//!
//! 缓存是可丢的派生数据，落在 `<数据根>/cache/comics/`；路径校验与「只解图片」在
//! [`super::comic_cache`] 里。

use std::path::Path;

use super::cbz::{Extras, build_fixed, split_cover};
use super::comic_cache;
use super::{Content, DocMeta, DocumentProvider, Format};
use crate::Result;

pub struct Cb7Adapter;

impl DocumentProvider for Cb7Adapter {
    fn format(&self) -> Format {
        Format::Cb7
    }

    fn supports(&self, path: &Path) -> bool {
        matches!(super::extension_of(path).as_deref(), Some("cb7"))
    }

    fn index(&self, path: &Path) -> Result<(DocMeta, Content)> {
        let cache = comic_cache::ensure_sevenz(path)?;
        // 两张便条都跟着同一次解压落在缓存里（`comic_cache::kept`），这里只是把它们读回来。
        let mut names = comic_cache::pages(&cache);
        let cover_name = split_cover(&mut names);
        let cover = cover_name
            .as_deref()
            .and_then(|name| comic_cache::read_page(&cache, name));
        let info = comic_cache::comic_info(&cache);

        let mut pages = Vec::new();
        for name in names {
            let Some(bytes) = comic_cache::read_page(&cache, &name) else {
                tracing::warn!("cb7: {} 里的 {name} 读不出来，跳过这一页", path.display());
                continue;
            };
            pages.push((name, bytes));
        }
        build_fixed(
            path,
            Format::Cb7,
            pages,
            &Extras {
                comic_info: &info,
                cover: cover.as_deref(),
            },
        )
    }

    /// 正常热路径由 `BookDocument` 的共享 `Container` 直接读缓存；这一支是兜底
    /// （缓存被清掉、或 `Container` 的家族探测没开起来）。
    fn image_bytes(&self, path: &Path, href: &str) -> Option<Vec<u8>> {
        let cache = comic_cache::ensure_sevenz(path).ok()?;
        comic_cache::read_page(&cache, href)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::Cb7Adapter;
    use crate::content::format::comic_cache::{self, CacheGuard};
    use crate::content::format::{Content, DocumentProvider, Format, PageSource};

    fn png(width: u32, height: u32) -> Vec<u8> {
        use image::ImageEncoder as _;
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba([3, 4, 5, 255]));
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), width, height, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    fn cb7(dir: &Path) -> std::path::PathBuf {
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("p10.png"), png(10, 20)).unwrap();
        std::fs::write(src.join("p2.png"), png(2, 3)).unwrap();
        std::fs::write(src.join("p1.png"), png(40, 30)).unwrap();
        std::fs::write(src.join("readme.txt"), b"not a page").unwrap();
        let path = dir.join("comic.cb7");
        sevenz_rust2::compress_to_path(&src, &path).unwrap();
        path
    }

    /// 7z / RAR / tar 只能顺序解压，所以那张书目便条必须在**第一次解压**时就一起落到
    /// 磁盘缓存里（`comic_cache::kept`）。留着它的代价是一份几百字节的文件；不留的代价是
    /// 之后每次要为它把整本再倒一遍 —— 那比解压本身还贵。
    ///
    /// 同一条也钉住反面：便条**不许被当成一页**，而别的非图片条目（`readme.txt`）照旧不落盘。
    #[test]
    fn the_sidecar_survives_the_disk_cache() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CacheGuard::new(dir.path().join("cache"));
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("p1.png"), png(40, 30)).unwrap();
        std::fs::write(src.join("info.txt"), b"not a page").unwrap();
        std::fs::write(
            src.join("ComicInfo.xml"),
            r#"<ComicInfo><Title>缓存里的书名</Title><Writer>某作者</Writer></ComicInfo>"#,
        )
        .unwrap();
        let path = dir.path().join("cache-me.cb7");
        sevenz_rust2::compress_to_path(&src, &path).unwrap();

        let (meta, content) = Cb7Adapter.read(&path).unwrap();
        assert_eq!(meta.title, "缓存里的书名", "便条要从解压缓存里读回来");
        assert_eq!(meta.authors, ["某作者"]);
        let Content::Fixed(fixed) = content else {
            panic!("CB7 该读成定版式");
        };
        assert_eq!(fixed.page_count(), 1, "便条不是页");
        let cache = comic_cache::dir_for(&path);
        assert!(cache.join("ComicInfo.xml").is_file());
        assert!(
            !cache.join("info.txt").exists(),
            "与便条无关的文本条目照旧不落盘"
        );
    }

    /// 封面那张在 7z 这一路走的是**磁盘缓存**：解压时它作为图片落了盘，所以要由同一个
    /// `split_cover` 从页清单里摘走。这一条钉的不是「规则对不对」（那条在 CBZ 测），
    /// 而是「摘走这一步在这一族也接上了」—— 四个 provider 各接一次，漏一个就有一族多一页。
    #[test]
    fn the_cover_sidecar_is_picked_out_of_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CacheGuard::new(dir.path().join("cache"));
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("01.png"), png(8, 8)).unwrap();
        std::fs::write(src.join("02.png"), png(8, 8)).unwrap();
        std::fs::write(src.join("folder.png"), png(30, 20)).unwrap();
        let path = dir.path().join("cover.cb7");
        sevenz_rust2::compress_to_path(&src, &path).unwrap();

        let (_, content) = Cb7Adapter.read(&path).unwrap();
        let Content::Fixed(fixed) = content else {
            panic!("CB7 该读成定版式");
        };
        let names: Vec<&str> = fixed
            .pages
            .iter()
            .map(|page| page.image_href().unwrap())
            .collect();
        assert_eq!(names, ["01.png", "02.png"]);
        let decoded = image::load_from_memory(&fixed.cover.unwrap()).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (30, 20));
    }

    #[test]
    fn pages_come_out_in_natural_order() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CacheGuard::new(dir.path().join("cache"));
        let path = cb7(dir.path());
        let (meta, content) = Cb7Adapter.read(&path).unwrap();
        assert_eq!(meta.format, Format::Cb7);
        let Content::Fixed(fixed) = content else {
            panic!("CB7 该读成定版式");
        };
        let names: Vec<&str> = fixed
            .pages
            .iter()
            .map(|page| page.image_href().unwrap())
            .collect();
        assert_eq!(names, vec!["p1.png", "p2.png", "p10.png"]);
        assert_eq!(fixed.page_count(), 3);
        assert_eq!((fixed.pages[0].width, fixed.pages[0].height), (40, 30));
        assert!(fixed.cover.is_some());
        assert!(matches!(fixed.pages[0].source, PageSource::Image { .. }));
    }

    #[test]
    fn image_bytes_are_read_from_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CacheGuard::new(dir.path().join("cache"));
        let path = cb7(dir.path());
        let _ = Cb7Adapter.index(&path).unwrap();
        assert_eq!(Cb7Adapter.image_bytes(&path, "p2.png").unwrap(), png(2, 3));
        assert!(Cb7Adapter.image_bytes(&path, "nope.png").is_none());
    }

    #[test]
    fn a_cb7_without_images_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CacheGuard::new(dir.path().join("cache"));
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("readme.txt"), b"nothing").unwrap();
        let path = dir.path().join("empty.cb7");
        sevenz_rust2::compress_to_path(&src, &path).unwrap();
        assert!(Cb7Adapter.read(&path).is_err());
    }

    #[test]
    fn the_cache_is_reused_across_opens() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CacheGuard::new(dir.path().join("cache"));
        let path = cb7(dir.path());
        let _ = Cb7Adapter.index(&path).unwrap();
        let cache = comic_cache::ensure_sevenz(&path).unwrap();
        let marker = cache.join("marker");
        std::fs::write(&marker, b"x").unwrap();
        let _ = Cb7Adapter.index(&path).unwrap();
        assert!(marker.exists(), "第二次打开不该把缓存目录重建");
    }
}
