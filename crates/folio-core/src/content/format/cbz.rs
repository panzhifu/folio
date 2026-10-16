//! CBZ（漫画）：一个 zip，里面按顺序排着图片页。
//!
//! 它属于**定版式**那一族（KOReader 里由 MuPDF 负责）：页是既定的，缩放与裁剪只是
//! 显示变换，排版引擎不参与。与 EPUB 共用同一条「字节不入库、按 href 现取」的规矩。
//!
//! 页序按**自然序**排：`page2.jpg` 必须排在 `page10.jpg` 前面。字典序会把第 10 页提到
//! 第 2 页前面，漫画一翻开就跳页 —— 这是漫画包最常见的坑，所以排序不能省。

use std::cmp::Ordering;
use std::path::Path;

use super::zip::Zip;
use super::{
    Content, DocMeta, DocumentProvider, FixedDoc, FixedPage, Format, ImageRes, PageSource,
    extension_of, image_dims, media_type_of, shrink_image, stem_of,
};
use crate::Result;

/// 封面缩略图上限，与 EPUB / FB2 同一条口径。
const COVER_MAX_EDGE: u32 = 512;

pub struct CbzAdapter;

impl DocumentProvider for CbzAdapter {
    fn format(&self) -> Format {
        Format::Cbz
    }

    fn supports(&self, path: &Path) -> bool {
        matches!(extension_of(path).as_deref(), Some("cbz"))
    }

    /// 定版式没有「章」也没有正文：索引就是它的全部（页表 + 封面 + 图片元数据）。
    fn index(&self, path: &Path) -> Result<(DocMeta, Content)> {
        let mut zip = Zip::open(path)?;
        let mut names: Vec<String> = zip
            .names()
            .into_iter()
            .filter(|name| is_page_image(name))
            .collect();
        names.sort_by(|a, b| natural_cmp(a, b));
        if names.is_empty() {
            return Err(unsupported(path, "CBZ 里没有图片页"));
        }

        let mut pages: Vec<FixedPage> = Vec::with_capacity(names.len());
        let mut images: Vec<ImageRes> = Vec::with_capacity(names.len());
        let mut cover = None;
        for (index, name) in names.iter().enumerate() {
            let Some(bytes) = zip.read(name) else {
                // 单页读不出来（压缩坏了）就跳过这一页，不让整本打不开 ——
                // 与 EPUB「坏一处不整本失败」同一条立场。
                tracing::warn!("cbz: {} 里的 {name} 读不出来，跳过这一页", path.display());
                continue;
            };
            let ext = extension_of(Path::new(name)).unwrap_or_default();
            // SVG 页没有位图头，尺寸从矢量声明里读；其余读位图头。
            let (width, height) = if ext == "svg" {
                super::svg::intrinsic_size(&bytes).unwrap_or((0, 0))
            } else {
                image_dims(&bytes)
            };
            images.push(ImageRes {
                href: name.clone(),
                // 漫画包没有「声明了但解不动」这一档：条目名就是字节所在，键与去处同一条。
                source: name.clone(),
                media_type: media_type_of(&ext).to_string(),
                width,
                height,
            });
            pages.push(FixedPage {
                source: PageSource::Image { href: name.clone() },
                width,
                height,
                // 漫画包一页一张图（多图页只出现在 EPUB 定版式里）。
                extra_images: Vec::new(),
            });
            if index == 0 {
                cover = Some(match shrink_image(&bytes, COVER_MAX_EDGE) {
                    Some((small, _)) => small,
                    None => bytes,
                });
            }
        }
        if pages.is_empty() {
            return Err(unsupported(path, "CBZ 里的图片页一页都读不出来"));
        }

        let (width, height) = (pages[0].width, pages[0].height);
        let meta = DocMeta {
            title: stem_of(path),
            authors: Vec::new(),
            format: Format::Cbz,
            size: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
            ..DocMeta::default()
        };
        let fixed = FixedDoc {
            pages,
            toc: Vec::new(),
            cover,
            images,
            width,
            height,
        };
        tracing::debug!("cbz: {} 读到 {} 页", path.display(), fixed.page_count());
        Ok((meta, Content::Fixed(fixed)))
    }

    fn image_bytes(&self, path: &Path, href: &str) -> Option<Vec<u8>> {
        let mut zip = Zip::open(path).ok()?;
        let name = zip.locate(href)?;
        zip.read(&name)
    }
}

fn unsupported(path: &Path, why: &str) -> crate::Error {
    crate::Error::UnsupportedFormat(format!(
        "{}：{why}",
        path.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    ))
}

/// 容器里根本不该看一眼的噪音：目录、以点开头的文件、`__MACOSX/` 那一类。
///
/// 单独立一条而不是并进 [`is_page_image`]，因为判定层要的是两个不同的问题：
/// 「这一条算不算一页」与「这一条该不该参与判断」。混在一起的话，一个装着
/// `__MACOSX/._001.jpg` 的正常漫画包会因为那条噪音被评成「不是漫画包」。
pub(crate) fn is_container_noise(name: &str) -> bool {
    if name.ends_with('/') || name.starts_with('.') || name.contains("__MACOSX/") {
        return true;
    }
    name.rsplit('/')
        .next()
        .is_some_and(|file| file.starts_with('.'))
}

/// 这一层里算不算一页。漫画包里常混着 `__MACOSX/`、`.DS_Store`、`thumbs/` 这些噪音。
///
/// `pub(crate)` 的理由是**只有一份**：判定层（`format::sniff`）要按同一条规则回答
/// 「这个 zip 里全是图吗」，两处各写一份就会在同一本漫画上分叉。
///
/// SVG 也算一页：逐页 SVG 的漫画包真实存在（矢量页面，缩放到任何屏幕都清晰），
/// 栅格化由 `format::svg` 在绘制时做。
pub(crate) fn is_page_image(name: &str) -> bool {
    if is_container_noise(name) {
        return false;
    }
    matches!(
        extension_of(Path::new(name)).as_deref(),
        Some("jpg" | "jpeg" | "jpe" | "png" | "gif" | "webp" | "bmp" | "svg")
    )
}

/// 自然序比较：数字段按数值比，其余按小写字符比。
///
/// `pub(crate)` 是因为**这一族不只漫画包会用**：CHM 拿不到正文清单时也要按名字给
/// 一组 topic 定阅读顺序（见 [`super::chm`]），而 `page2` 排在 `page10` 前面这件事
/// 没有第二种正确答案 —— 各写一遍迟早会漂。
pub(crate) fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut left = a.chars().peekable();
    let mut right = b.chars().peekable();
    loop {
        match (left.peek().copied(), right.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                if x.is_ascii_digit() && y.is_ascii_digit() {
                    let xn = take_number(&mut left);
                    let yn = take_number(&mut right);
                    match xn.cmp(&yn) {
                        Ordering::Equal => continue,
                        other => return other,
                    }
                }
                left.next();
                right.next();
                match x.to_ascii_lowercase().cmp(&y.to_ascii_lowercase()) {
                    Ordering::Equal => continue,
                    other => return other,
                }
            }
        }
    }
}

fn take_number(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> u64 {
    let mut value = 0u64;
    while let Some(digit) = chars.peek().and_then(|c| c.to_digit(10)) {
        value = value.saturating_mul(10).saturating_add(digit as u64);
        chars.next();
    }
    value
}

#[cfg(test)]
mod tests {
    //! CBZ：页序（自然序）、尺寸登记、封面、按需取字节、噪音条目过滤。

    use std::io::Write as _;
    use std::path::Path;

    use super::{CbzAdapter, natural_cmp};
    use crate::content::format::{Content, DocumentProvider, Format, PageSource};

    fn png(width: u32, height: u32) -> Vec<u8> {
        use image::ImageEncoder as _;
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba([7, 8, 9, 255]));
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), width, height, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    /// 造一个 CBZ：`pages` 是 (名字, 宽, 高)，外加几个噪音条目。
    fn cbz(dir: &Path, pages: &[(&str, u32, u32)]) -> std::path::PathBuf {
        let path = dir.join("comic.cbz");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("__MACOSX/._page1.png", opts).unwrap();
        zip.write_all(b"junk").unwrap();
        zip.start_file(".hidden.png", opts).unwrap();
        zip.write_all(b"junk").unwrap();
        zip.start_file("readme.txt", opts).unwrap();
        zip.write_all(b"not a page").unwrap();
        for (name, w, h) in pages {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(&png(*w, *h)).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    fn fixed(
        path: &Path,
    ) -> (
        crate::content::format::DocMeta,
        crate::content::format::FixedDoc,
    ) {
        let (meta, content) = CbzAdapter.read(path).unwrap();
        let Content::Fixed(fixed) = content else {
            panic!("CBZ 该读成定版式");
        };
        (meta, fixed)
    }

    #[test]
    fn it_claims_cbz_and_reports_the_format() {
        assert_eq!(CbzAdapter.format(), Format::Cbz);
        assert!(CbzAdapter.supports(Path::new("a.cbz")));
        assert!(CbzAdapter.supports(Path::new("A.CBZ")));
        assert!(!CbzAdapter.supports(Path::new("a.zip")));
    }

    #[test]
    fn pages_come_out_in_natural_order() {
        let dir = tempfile::tempdir().unwrap();
        // 故意按字典序会错的名字放进去。
        let path = cbz(
            dir.path(),
            &[("p10.png", 10, 20), ("p2.png", 30, 40), ("p1.png", 50, 60)],
        );
        let (meta, fixed) = fixed(&path);
        let names: Vec<String> = fixed
            .pages
            .iter()
            .map(|p| p.image_href().unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["p1.png", "p2.png", "p10.png"]);
        assert_eq!(meta.title, "comic");
        assert_eq!(meta.format, Format::Cbz);
        assert_eq!(fixed.page_count(), 3);
        assert_eq!((fixed.width, fixed.height), (50, 60), "首屏尺寸取第一页");
    }

    #[test]
    fn noise_entries_are_not_pages_and_sizes_are_registered() {
        let dir = tempfile::tempdir().unwrap();
        let path = cbz(dir.path(), &[("pages/001.png", 64, 96)]);
        let (_, fixed) = fixed(&path);
        assert_eq!(fixed.page_count(), 1, "只有真图片才算页");
        assert_eq!(fixed.pages[0].width, 64);
        assert_eq!(fixed.pages[0].height, 96);
        assert_eq!(fixed.images[0].href, "pages/001.png");
        assert_eq!(fixed.images[0].media_type, "image/png");
    }

    #[test]
    fn the_cover_is_a_thumbnail_of_the_first_page() {
        let dir = tempfile::tempdir().unwrap();
        // 第一页够大，缩过之后应该更小；后面那一页小，原样留着。
        let path = cbz(dir.path(), &[("1.png", 2048, 1024), ("2.png", 8, 8)]);
        let (_, fixed) = fixed(&path);
        let cover = fixed.cover.expect("该有封面");
        let decoded = image::load_from_memory(&cover).unwrap();
        assert_eq!(
            decoded.width().max(decoded.height()),
            super::COVER_MAX_EDGE,
            "封面缩到存储尺寸上限"
        );
    }

    #[test]
    fn image_bytes_are_read_from_the_container_on_demand() {
        let dir = tempfile::tempdir().unwrap();
        let path = cbz(dir.path(), &[("a.png", 4, 4), ("b.png", 5, 5)]);
        let want = png(5, 5);
        assert_eq!(CbzAdapter.image_bytes(&path, "b.png").unwrap(), want);
        assert!(CbzAdapter.image_bytes(&path, "nope.png").is_none());
    }

    #[test]
    fn a_cbz_without_images_is_rejected_instead_of_opening_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.cbz");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("readme.txt", opts).unwrap();
        zip.write_all(b"nothing").unwrap();
        zip.finish().unwrap();

        assert!(
            CbzAdapter.read(&path).is_err(),
            "一页都没有的书该当场失败，而不是打开成空白"
        );
    }

    #[test]
    fn natural_order_beats_lexicographic() {
        assert_eq!(natural_cmp("p2.png", "p10.png"), std::cmp::Ordering::Less);
        assert_eq!(
            natural_cmp("p10.png", "p2.png"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(natural_cmp("p2.png", "p2.png"), std::cmp::Ordering::Equal);
        assert_eq!(natural_cmp("a2b", "a2a"), std::cmp::Ordering::Greater);
        // 前导零也要按数值走。
        assert_eq!(natural_cmp("p007.png", "p8.png"), std::cmp::Ordering::Less);
    }

    /// 页表落进 `FixedDoc` 的 `PageSource` 形状要对，否则 store 那一列存不住。
    #[test]
    fn pages_carry_an_image_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = cbz(dir.path(), &[("only.png", 3, 3)]);
        let (_, fixed) = fixed(&path);
        assert!(matches!(fixed.pages[0].source, PageSource::Image { .. }));
    }
}
