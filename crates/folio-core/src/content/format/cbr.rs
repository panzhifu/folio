//! CBR（RAR 漫画包）：一个 RAR，里面按页排着图片。
//!
//! 与 CBZ / CB7 同族，差别只在容器。RAR 的正文是 LZSS + PPMd + 滤波器，solid
//! 归档还要从第一卷一路解到目标页；所以第一次打开时把图片条目整体解到磁盘缓存，
//! 页表/自然序/封面与 CBZ 共用 [`super::cbz::build_fixed`]。
//!
//! 解码器是纯 Rust 的 `rars`，不是 libunrar / unrar C++。加密 RAR 这一版不接：
//! `rars` 的 encryption feature 没有开，密码也没有界面入口 —— 它会明确报错，而不是
//! 静默读出一本乱码书。

use std::path::Path;

use super::cbz::{Extras, build_fixed, split_cover};
use super::comic_cache;
use super::{Content, DocMeta, DocumentProvider, Format};
use crate::Result;

pub struct CbrAdapter;

impl DocumentProvider for CbrAdapter {
    fn format(&self) -> Format {
        Format::Cbr
    }

    fn supports(&self, path: &Path) -> bool {
        matches!(super::extension_of(path).as_deref(), Some("cbr"))
    }

    fn index(&self, path: &Path) -> Result<(DocMeta, Content)> {
        let cache = comic_cache::ensure_rar(path)?;
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
                tracing::warn!("cbr: {} 里的 {name} 读不出来，跳过这一页", path.display());
                continue;
            };
            pages.push((name, bytes));
        }
        build_fixed(
            path,
            Format::Cbr,
            pages,
            &Extras {
                comic_info: &info,
                cover: cover.as_deref(),
            },
        )
    }

    /// 正常热路径由 `BookDocument` 的共享 `Container` 直接读缓存；这一支是兜底。
    fn image_bytes(&self, path: &Path, href: &str) -> Option<Vec<u8>> {
        let cache = comic_cache::ensure_rar(path).ok()?;
        comic_cache::read_page(&cache, href)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::CbrAdapter;
    use crate::content::format::comic_cache::CacheGuard;
    use crate::content::format::{Content, DocumentProvider, Format};

    fn png(width: u32, height: u32) -> Vec<u8> {
        use image::ImageEncoder as _;
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba([8, 9, 10, 255]));
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), width, height, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    /// 用 `rars` 的 writer（dev-only feature）合成一个 CBR。
    fn cbr(dir: &Path) -> std::path::PathBuf {
        let mut builder = rars::Builder::new(rars::ArchiveVersion::Rar50);
        for (name, bytes) in [
            ("p10.png", png(10, 20)),
            ("p2.png", png(2, 3)),
            ("p1.png", png(40, 30)),
        ] {
            builder
                .add_bytes(name.as_bytes().to_vec(), bytes, None, None)
                .unwrap();
        }
        builder
            .add_bytes(b"readme.txt".to_vec(), b"not a page".to_vec(), None, None)
            .unwrap();
        let path = dir.join("comic.cbr");
        builder.write_to_path(&path, None).unwrap();
        path
    }

    #[test]
    fn pages_come_out_in_natural_order() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CacheGuard::new(dir.path().join("cache"));
        let path = cbr(dir.path());
        let (meta, content) = CbrAdapter.read(&path).unwrap();
        assert_eq!(meta.format, Format::Cbr);
        let Content::Fixed(fixed) = content else {
            panic!("CBR 该读成定版式");
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
        let path = cbr(dir.path());
        let _ = CbrAdapter.index(&path).unwrap();
        assert_eq!(CbrAdapter.image_bytes(&path, "p2.png").unwrap(), png(2, 3));
    }

    /// 一个由 rarlab 的 unrar 测试集提供的小样本：证明 `rars` 不只读得动自己写的包。
    #[test]
    fn a_third_party_rar_is_readable() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/oss/rar/version.rar");
        let archive = rars::ArchiveReader::read_path(&path).expect("真实 RAR 该打得开");
        let bytes = archive
            .read_member_at(0, None)
            .expect("读第一条")
            .expect("第一条该是文件");
        assert!(!bytes.is_empty());
    }

    #[test]
    fn a_rar_without_images_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CacheGuard::new(dir.path().join("cache"));
        let mut builder = rars::Builder::new(rars::ArchiveVersion::Rar50);
        builder
            .add_bytes(b"readme.txt".to_vec(), b"nothing".to_vec(), None, None)
            .unwrap();
        let path = dir.path().join("empty.cbr");
        builder.write_to_path(&path, None).unwrap();
        assert!(CbrAdapter.read(&path).is_err());
    }
}
