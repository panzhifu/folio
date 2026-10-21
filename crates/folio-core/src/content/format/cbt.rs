//! CBT（tar 漫画包）：一个 tar，里面按页排着图片。
//!
//! 与 CBZ / CB7 / CBR 同族；tar 本身没有压缩，但条目是顺序流，所以同样先整体解到
//! [`super::comic_cache`]，页表/自然序/封面继续共用 [`super::cbz::build_fixed`]。
//!
//! 只认 POSIX/GNU tar 的 `ustar` 魔数（偏移 257），不把任意二进制文件当 CBT。

use std::path::Path;

use super::cbz::{Extras, build_fixed, split_cover};
use super::comic_cache;
use super::{Content, DocMeta, DocumentProvider, Format};
use crate::Result;

pub struct CbtAdapter;

impl DocumentProvider for CbtAdapter {
    fn format(&self) -> Format {
        Format::Cbt
    }

    fn supports(&self, path: &Path) -> bool {
        matches!(super::extension_of(path).as_deref(), Some("cbt"))
    }

    fn index(&self, path: &Path) -> Result<(DocMeta, Content)> {
        let cache = comic_cache::ensure_tar(path)?;
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
                tracing::warn!("cbt: {} 里的 {name} 读不出来，跳过这一页", path.display());
                continue;
            };
            pages.push((name, bytes));
        }
        build_fixed(
            path,
            Format::Cbt,
            pages,
            &Extras {
                comic_info: &info,
                cover: cover.as_deref(),
            },
        )
    }

    fn image_bytes(&self, path: &Path, href: &str) -> Option<Vec<u8>> {
        let cache = comic_cache::ensure_tar(path).ok()?;
        comic_cache::read_page(&cache, href)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::CbtAdapter;
    use crate::content::format::comic_cache::CacheGuard;
    use crate::content::format::{Content, DocumentProvider, Format};

    fn png(width: u32, height: u32) -> Vec<u8> {
        use image::ImageEncoder as _;
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba([2, 4, 6, 255]));
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), width, height, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    fn cbt(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("comic.cbt");
        let file = std::fs::File::create(&path).unwrap();
        let mut builder = tar::Builder::new(file);
        for (name, bytes) in [
            ("p10.png", png(10, 20)),
            ("p2.png", png(2, 3)),
            ("p1.png", png(40, 30)),
            ("readme.txt", b"not a page".to_vec()),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_path(name).unwrap();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, bytes.as_slice()).unwrap();
        }
        builder.finish().unwrap();
        path
    }

    #[test]
    fn pages_come_out_in_natural_order() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CacheGuard::new(dir.path().join("cache"));
        let path = cbt(dir.path());
        let (meta, content) = CbtAdapter.read(&path).unwrap();
        assert_eq!(meta.format, Format::Cbt);
        let Content::Fixed(fixed) = content else {
            panic!("CBT 该读成定版式");
        };
        let names: Vec<&str> = fixed
            .pages
            .iter()
            .map(|page| page.image_href().unwrap())
            .collect();
        assert_eq!(names, vec!["p1.png", "p2.png", "p10.png"]);
        assert_eq!((fixed.pages[0].width, fixed.pages[0].height), (40, 30));
        assert!(fixed.cover.is_some());
    }

    #[test]
    fn image_bytes_are_read_from_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CacheGuard::new(dir.path().join("cache"));
        let path = cbt(dir.path());
        let _ = CbtAdapter.index(&path).unwrap();
        assert_eq!(CbtAdapter.image_bytes(&path, "p2.png").unwrap(), png(2, 3));
    }

    #[test]
    fn a_tar_without_images_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CacheGuard::new(dir.path().join("cache"));
        let path = dir.path().join("empty.cbt");
        let file = std::fs::File::create(&path).unwrap();
        let mut builder = tar::Builder::new(file);
        let mut header = tar::Header::new_gnu();
        header.set_path("readme.txt").unwrap();
        header.set_size(4);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, &b"none"[..]).unwrap();
        builder.finish().unwrap();
        assert!(CbtAdapter.read(&path).is_err());
    }
}
