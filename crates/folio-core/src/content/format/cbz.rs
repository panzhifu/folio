//! CBZ（漫画）：一个 zip，里面按顺序排着图片页。
//!
//! 它属于**定版式**那一族（KOReader 里由 MuPDF 负责）：页是既定的，缩放与裁剪只是
//! 显示变换，排版引擎不参与。与 EPUB 共用同一条「字节不入库、按 href 现取」的规矩。
//!
//! 书目这一格来自 [`super::comicinfo`]（那份 `ComicInfo.xml` 便条）—— 没有它，这一族的
//! 书名只能是文件名。
//!
//! 页序按**自然序**排：`page2.jpg` 必须排在 `page10.jpg` 前面。字典序会把第 10 页提到
//! 第 2 页前面，漫画一翻开就跳页 —— 这是漫画包最常见的坑，所以排序不能省。

use std::cmp::Ordering;
use std::path::Path;

use super::comicinfo::FILE;
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

    /// 定版式没有「章」也没有正文：索引就是它的全部（页表 + 封面 + 图片元数据 + 书目便条）。
    fn index(&self, path: &Path) -> Result<(DocMeta, Content)> {
        let mut zip = Zip::open(path)?;
        // 那份便条的字节单独取一次：`locate` 认大小写两种写法（真包里 `comicinfo.xml` 常见），
        // 而它不是页、不该混进页表那一步的过滤里。
        let info = zip
            .locate(FILE)
            .and_then(|name| zip.read(&name))
            .unwrap_or_default();
        let mut names: Vec<String> = zip
            .names()
            .into_iter()
            .filter(|name| is_page_image(name))
            .collect();
        let cover_name = split_cover(&mut names);
        if names.is_empty() {
            return Err(unsupported(path, "CBZ 里没有图片页"));
        }
        names.sort_by(|a, b| natural_cmp(a, b));
        // 封面那张读不到（坏条目）就当没有：仍已从页表里剔掉，退回首页缩略图那条路。
        let cover = cover_name
            .as_deref()
            .and_then(|name| zip.locate(name))
            .and_then(|name| zip.read(&name));

        let mut pages: Vec<(String, Vec<u8>)> = Vec::with_capacity(names.len());
        for name in names {
            let Some(bytes) = zip.read(&name) else {
                // 单页读不出来（压缩坏了）就跳过这一页，不让整本打不开 ——
                // 与 EPUB「坏一处不整本失败」同一条立场。
                tracing::warn!("cbz: {} 里的 {name} 读不出来，跳过这一页", path.display());
                continue;
            };
            pages.push((name, bytes));
        }
        build_fixed(
            path,
            Format::Cbz,
            pages,
            &Extras {
                comic_info: &info,
                cover: cover.as_deref(),
            },
        )
    }

    fn image_bytes(&self, path: &Path, href: &str) -> Option<Vec<u8>> {
        let mut zip = Zip::open(path).ok()?;
        let name = zip.locate(href)?;
        zip.read(&name)
    }
}

/// 一串「条目名 + 图片字节」→ 定版式页表。
///
/// CBZ 与 CB7 / CBR / CBT 的差别只在「字节从 ZIP 还是从解压缓存里来」；页序、尺寸、封面、
/// `ImageRes` 的形状、以及那两张便条的读法必须同源，所以全收在这一处。
pub(crate) fn build_fixed(
    path: &Path,
    format: Format,
    pages: Vec<(String, Vec<u8>)>,
    extras: &Extras,
) -> Result<(DocMeta, Content)> {
    if pages.is_empty() {
        return Err(unsupported(path, "漫画包里一页图片都读不出来"));
    }
    let mut table = Vec::with_capacity(pages.len());
    let mut images = Vec::with_capacity(pages.len());
    let mut cover = None;
    for (index, (name, bytes)) in pages.into_iter().enumerate() {
        let ext = extension_of(Path::new(&name)).unwrap_or_default();
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
        table.push(FixedPage {
            source: PageSource::Image { href: name },
            width,
            height,
            // 漫画包一页一张图（多图页只出现在 EPUB 定版式里）。
            extra_images: Vec::new(),
        });
        if index == 0 && extras.cover.is_none() {
            // 没有那张封面缓存才退到首页 —— 与 EPUB「`<guide type="cover">` 优先，
            // 没有才拿第一页」是同一条次序。
            cover = Some(thumbnail(&bytes));
        }
    }

    // 包里那张封面缓存也缩到同一档：它是「缓存」，本来就常是原页尺寸，不缩就是几百 KB
    // 进库再进显存（`shrink_image` 那条 atlas 上限的理由在这同样成立）。
    let cover = cover.or_else(|| extras.cover.map(thumbnail));
    let (width, height) = (table[0].width, table[0].height);
    let mut meta = DocMeta {
        title: stem_of(path),
        authors: Vec::new(),
        format,
        size: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        ..DocMeta::default()
    };
    // 便条排在兜底**之后**：它写了什么就用什么，没写的格子留着文件名主名。
    super::comicinfo::apply(extras.comic_info, &mut meta);
    let fixed = FixedDoc {
        pages: table,
        toc: Vec::new(),
        cover,
        // 封面的落盘描述由导入管线填（`ingest::import::land_cover`）：provider 只交字节，
        // 它不知道书库在哪儿 —— 与可重排那一路同一条分工。
        cover_ref: None,
        images,
        width,
        height,
    };
    tracing::debug!(
        "{}: {} 读到 {} 页",
        format.as_str(),
        path.display(),
        fixed.page_count()
    );
    Ok((meta, Content::Fixed(fixed)))
}

/// 封面/缩略那一步：缩到 [`COVER_MAX_EDGE`] 以内，缩不动（SVG、已经够小）就原样留。
fn thumbnail(bytes: &[u8]) -> Vec<u8> {
    match shrink_image(bytes, COVER_MAX_EDGE) {
        Some((small, _)) => small,
        None => bytes.to_vec(),
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
        Some("jpg" | "jpeg" | "jpe" | "png" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "svg")
    )
}

/// 漫画包里那两份额外的文件。两样都**不是一页**，所以都不进页表。
pub(crate) struct Extras<'a> {
    /// `ComicInfo.xml` 的字节。空 = 这本没带便条（书目只剩文件名，与从前一致）。
    pub comic_info: &'a [u8],
    /// 包里那张「封面缓存」的字节（`folder.jpg` / `cover.jpg`）。`None` = 没摆。
    pub cover: Option<&'a [u8]>,
}

/// 这一条是不是「封面缓存」那一张。是的话交出它的主名（`folder` / `cover`，保留原大小写）。
///
/// 两名是整理工具与阅读器的**约定**，不是内容的一部分：CDisplayEx / Kavita / YACReader
/// 往包里写 `folder.jpg` 当缩略图缓存，Calibre 导出写 `cover.jpg`。从前没人认它们，
/// 于是它们被自然序排进页表 —— **同一张封面在包里出现两次**：一次在书架上，一次在正文里
/// 多出来的那一页（数字命名的页排在字母之前，所以多半落在书的末尾，读者要翻到最后
/// 才看到「凭空多一页封面」）。
pub(crate) fn cover_sidecar_stem(name: &str) -> Option<&str> {
    let file = name.rsplit(['/', '\\']).next()?;
    if !matches!(
        extension_of(Path::new(file)).as_deref(),
        Some("jpg" | "jpeg" | "png" | "webp" | "gif")
    ) {
        return None;
    }
    // 主名取**最后一个点之前**：`folder.jpg` 认，`folder.png.bak` 不认（那一格本来也不是图）。
    let (stem, _) = file.rsplit_once('.')?;
    if stem.eq_ignore_ascii_case("folder") || stem.eq_ignore_ascii_case("cover") {
        Some(stem)
    } else {
        None
    }
}

/// 从一串条目名里挑出那张封面缓存：**`folder.*` 优先于 `cover.*`**（前者是阅读器写的
/// 缓存，与封面同义得更彻底），同类之间按自然序取第一个。
pub(crate) fn pick_cover<'a>(names: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let mut folder: Option<&'a str> = None;
    let mut cover: Option<&'a str> = None;
    for name in names {
        let Some(stem) = cover_sidecar_stem(name) else {
            continue;
        };
        let slot = if stem.eq_ignore_ascii_case("folder") {
            &mut folder
        } else {
            &mut cover
        };
        if slot.is_none_or(|seen| natural_cmp(name, seen) == Ordering::Less) {
            *slot = Some(name);
        }
    }
    folder.or(cover)
}

/// 把那张封面缓存从页清单里**摘掉**并交出它的名字；没摆就回 `None`。
///
/// `names.len() < 2` 那一格是故意的：包里只有这一张图时，摘掉它就等于「一页都没有」，
/// 而那一格当场判打不开。留着当页不过是书架上那格与正文那一页是同一张图 —— 比打不开好。
pub(crate) fn split_cover(names: &mut Vec<String>) -> Option<String> {
    if names.len() < 2 {
        return None;
    }
    let picked = pick_cover(names.iter().map(String::as_str))?.to_string();
    names.retain(|name| *name != picked);
    Some(picked)
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

    /// 在 [`cbz`] 那份夹具里加一条 `ComicInfo.xml`。
    fn cbz_with_info(dir: &Path, pages: &[(&str, u32, u32)], xml: &str) -> std::path::PathBuf {
        cbz_named_info(dir, "ComicInfo.xml", xml, pages)
    }

    fn cbz_named_info(
        dir: &Path,
        name: &str,
        xml: &str,
        pages: &[(&str, u32, u32)],
    ) -> std::path::PathBuf {
        let path = dir.join("comic.cbz");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file(name, opts).unwrap();
        zip.write_all(xml.as_bytes()).unwrap();
        for (page, w, h) in pages {
            zip.start_file(*page, opts).unwrap();
            zip.write_all(&png(*w, *h)).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    /// 造一个完全由「条目名 + 字节」决定的 CBZ：页序、封面那张摆在哪一格都由调用方说。
    fn cbz_raw(dir: &Path, entries: &[(&str, &[u8])]) -> std::path::PathBuf {
        let path = dir.join("comic.cbz");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        for (name, bytes) in entries {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(bytes).unwrap();
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

    /// 那份便条是这一族**唯一**的元数据来源：包里没有 OPF 那样的书目区。
    ///
    /// 三条一起钉：角色落到哪一格（著者与其余角色分开，按 `DocMeta` 那两条契约）、
    /// 期号取 `Number` 而不是 `Volume`、以及 `<Chapters>` 那一族**故意不读**
    /// （`Page` 的基数没找到可信权威，猜错是每条目录都跳到隔壁那一页 —— 见
    /// `comicinfo` 的模块头）。
    #[test]
    fn the_comicinfo_sidecar_fills_the_shelf_fields() {
        let dir = tempfile::tempdir().unwrap();
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
        <ComicInfo>
          <Title>城市之光 第一话</Title>
          <Series>城市之光</Series>
          <Number>12A</Number>
          <Volume>3</Volume>
          <Writer>安·诺森蒂</Writer>
          <Writer>马克·西德威克</Writer>
          <Penciller>路易斯·西蒙森</Penciller>
          <Inker>乔·西蒙森, 字稿某人</Inker>
          <Translator>译者甲</Translator>
          <Publisher>某社</Publisher>
          <Summary>一个城市的&#38;与它的灯。</Summary>
          <Year>1987</Year>
          <Month>5</Month>
          <LanguageISO>en</LanguageISO>
          <Genre>科幻</Genre>
          <Tags>都市, 漫画, 科幻</Tags>
          <GTIN>9781234567890</GTIN>
          <PageCount>2</PageCount>
          <Chapters><Chapter Chapter="1" Page="0" Title="不该读的一章"/></Chapters>
        </ComicInfo>"#;
        let path = cbz_with_info(dir.path(), &[("p1.png", 8, 8), ("p2.png", 8, 8)], xml);
        let (meta, content) = CbzAdapter.read(&path).unwrap();
        assert_eq!(meta.title, "城市之光 第一话");
        assert_eq!(
            meta.authors,
            ["安·诺森蒂", "马克·西德威克"],
            "多条 Writer 合成一串再切"
        );
        assert_eq!(
            meta.contributors,
            ["路易斯·西蒙森", "乔·西蒙森", "字稿某人", "译者甲"],
            "绘者 / 墨线 / 字稿 / 译者在 ComicInfo 里各是一格，全部落进 dc:contributor"
        );
        assert_eq!(
            meta.series
                .as_ref()
                .map(|s| (s.name.as_str(), s.position.as_str())),
            Some(("城市之光", "12A")),
            "期号用 Number（它是字符串，「12A」这种写法真存在），不是 Volume"
        );
        assert_eq!(meta.publisher, "某社");
        assert_eq!(meta.description, "一个城市的&与它的灯。", "实体要折回来");
        assert_eq!(meta.published, "1987-05", "Year + Month 拼成 YYYY-MM");
        assert_eq!(meta.language, "en");
        assert_eq!(
            meta.subjects,
            ["科幻", "都市", "漫画"],
            "Genre 在前、Tags 补在后面且去重"
        );
        assert_eq!(meta.isbn, "9781234567890", "2.5 起那一格叫 GTIN");
        let Content::Fixed(fixed) = content else {
            panic!("CBZ 该读成定版式");
        };
        assert_eq!(fixed.page_count(), 2, "便条不是页");
        assert!(
            fixed.toc.is_empty(),
            "<Chapters> 故意不读：那条属性的基数没有权威，猜错比缺目录更坏"
        );
    }

    /// 便条的名字**大小写两种都认**（真包里 `comicinfo.xml` 常见），而它仍然不许变成一页。
    #[test]
    fn the_sidecar_is_found_whatever_its_case() {
        let dir = tempfile::tempdir().unwrap();
        let path = cbz_named_info(
            dir.path(),
            "comicinfo.xml",
            "<ComicInfo><Title>小写名</Title></ComicInfo>",
            &[("only.png", 4, 4)],
        );
        let (meta, content) = CbzAdapter.read(&path).unwrap();
        assert_eq!(meta.title, "小写名");
        let Content::Fixed(fixed) = content else {
            panic!("CBZ 该读成定版式");
        };
        assert_eq!(fixed.page_count(), 1, "便条那一格不许当页");
    }

    /// 那张封面缓存是**书架上**的那一格，不是正文里凭空多出来的一页。
    ///
    /// 三条一起钉：它不进页表、它当封面（不是首页缩略图）、以及它也不进整本的图片表。
    /// 封面那两张故意用「已经比上限小」的尺寸：那样交回来的字节就是原图，从尺寸就能
    /// 认出封面用的是哪一张（`folder` 是 30×20，`cover` 是 40×10）。
    #[test]
    fn the_cover_sidecar_is_the_cover_not_an_extra_page() {
        let dir = tempfile::tempdir().unwrap();
        let path = cbz_raw(
            dir.path(),
            &[
                ("01.png", &png(8, 8)[..]),
                ("02.png", &png(8, 8)[..]),
                ("folder.png", &png(30, 20)[..]),
                ("cover.png", &png(40, 10)[..]),
            ],
        );
        let (_, fixed) = fixed(&path);
        let names: Vec<&str> = fixed
            .pages
            .iter()
            .map(|page| page.image_href().unwrap())
            .collect();
        assert_eq!(
            names,
            ["01.png", "02.png", "cover.png"],
            "只有被选中的那张被摘走，另一张仍是一页"
        );
        assert_eq!(fixed.images.len(), 3, "图片表跟着页表走");
        let cover = fixed.cover.expect("该有封面");
        let decoded = image::load_from_memory(&cover).unwrap();
        assert_eq!(
            (decoded.width(), decoded.height()),
            (30, 20),
            "封面用的是那张缓存，不是首页缩略图"
        );
    }

    /// 主名与大小写、以及它藏在哪个子目录里都不影响：那两名是**约定**，不是路径写法。
    #[test]
    fn the_sidecar_is_matched_whatever_the_case_or_folder() {
        let dir = tempfile::tempdir().unwrap();
        let path = cbz_raw(
            dir.path(),
            &[
                ("01.png", &png(8, 8)[..]),
                ("02.png", &png(8, 8)[..]),
                ("Images/FOLDER.PNG", &png(30, 20)[..]),
            ],
        );
        let (_, fixed) = fixed(&path);
        let names: Vec<&str> = fixed
            .pages
            .iter()
            .map(|page| page.image_href().unwrap())
            .collect();
        assert_eq!(names, ["01.png", "02.png"]);
    }

    /// 包里只有那一张封面缓存（真存在：单话的本有时就这么打包）时**留着当页**。
    ///
    /// 摘掉它就是「一页都没有」，而那一格当场判打不开 —— 比书架与正文共用同一张图糟得多。
    #[test]
    fn a_package_that_is_only_a_cover_keeps_it_as_a_page() {
        let dir = tempfile::tempdir().unwrap();
        let path = cbz_raw(dir.path(), &[("cover.png", &png(30, 20)[..])]);
        let (_, fixed) = fixed(&path);
        assert_eq!(fixed.page_count(), 1, "唯一的一张不许被摘走");
        assert_eq!(fixed.pages[0].image_href().unwrap(), "cover.png");
        assert!(fixed.cover.is_some(), "首页缩略图那条兜底照旧走");
    }

    /// 没有便条时照旧用文件名主名兜底，而且**不当成错误**：那只是这本没带。
    #[test]
    fn a_package_without_the_sidecar_still_opens() {
        let dir = tempfile::tempdir().unwrap();
        let path = cbz(dir.path(), &[("only.png", 4, 4)]);
        let (meta, _) = CbzAdapter.read(&path).unwrap();
        assert_eq!(meta.title, "comic");
        assert!(meta.authors.is_empty());
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
