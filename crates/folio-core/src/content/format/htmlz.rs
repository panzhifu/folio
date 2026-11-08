//! Calibre HTMLZ：一个 ZIP 里装着 `index.html`、章节 HTML 与图片。
//!
//! 它补的是现有 `html` provider 的一个缺口：单文件 HTML 只能读一份，而 HTMLZ 里的
//! 章节是**多份 HTML**，图片与 CSS 又躺在同一个 ZIP 里。如果按「ZIP 里逐条抽书」处理，
//! 只会带走 HTML、把图片留在包外；所以这里把整个 ZIP 当一本书读。
//!
//! 每一份 HTML 是一个 [`Chapter`]（`ChapterLocator::Entry`），章节内的相对链接、图片
//! 与 CSS 都相对**那一份 HTML 自己的目录**解析 —— 与 EPUB 的 spine 项同一套坐标。

use std::collections::HashSet;
use std::path::Path;

use super::absolutize_css_urls;
use super::zip::Zip;
use super::{
    Chapter, Content, DocMeta, DocumentProvider, FlowDoc, Format, ImageRes, asset_href, dir_of,
    image_dims, media_type_of, resolve_href, shrink_image, stem_of,
};
use crate::Result;
use crate::content::dom::Dom;

/// 封面缩略的长边上限。与 EPUB / CBZ / FB2 同一个数：封面落盘是缩略图，
/// 原图留在容器里随正文那一路取。
const COVER_MAX_EDGE: u32 = 512;

pub struct HtmlzAdapter;

impl DocumentProvider for HtmlzAdapter {
    fn format(&self) -> Format {
        Format::Htmlz
    }

    fn supports(&self, path: &Path) -> bool {
        matches!(super::extension_of(path).as_deref(), Some("htmlz"))
    }

    fn index(&self, path: &Path) -> Result<(DocMeta, Content)> {
        let mut zip = Zip::open(path)?;
        let entries = html_entries(&zip);
        if entries.is_empty() {
            return Err(unsupported(path, "ZIP 里一份 HTML 都没有"));
        }
        let title = document_title_of(&entries[0], &mut zip).unwrap_or_else(|| stem_of(path));
        let mut chapters = Vec::with_capacity(entries.len());
        let mut images: Vec<ImageRes> = Vec::new();
        let mut seen_images: HashSet<String> = HashSet::new();
        let mut css = String::new();
        let mut seen_css: HashSet<String> = HashSet::new();

        for entry in &entries {
            let Some(bytes) = zip.read(entry) else {
                continue;
            };
            let raw = super::charset::decode(&bytes, None);
            // 书名那一问与单文件 HTML 同一份实现（`<title>` 优先、退到第一个非空 `<h1>`）：
            // 从前这里抄了两份，两份在「谁算书名」上迟早分叉。
            let title = super::html::document_title(&raw).unwrap_or_else(|| file_stem(entry));
            let (chapter_css, chapter_images) = styles_and_images(&raw, entry, &mut zip);
            if seen_css.insert(entry.clone()) {
                css.push_str(&chapter_css);
                if !css.ends_with('\n') {
                    css.push('\n');
                }
            }
            for image in chapter_images {
                if seen_images.insert(image.href.clone()) {
                    images.push(image);
                }
            }
            let per_chapter = image_uses(&raw, entry);
            chapters.push(
                Chapter::entry(title, entry.clone())
                    .with_char_len(super::visible_chars(&raw))
                    .with_images(per_chapter),
            );
        }
        if chapters.is_empty() {
            return Err(unsupported(path, "ZIP 里的 HTML 一份都读不出来"));
        }

        let cover = cover_of(&mut zip);
        let meta = DocMeta {
            title,
            format: Format::Htmlz,
            size: std::fs::metadata(path)
                .map(|metadata| metadata.len())
                .unwrap_or(0),
            ..DocMeta::default()
        };
        Ok((
            meta,
            Content::Reflow(FlowDoc {
                chapters,
                toc: Vec::new(),
                css,
                images,
                cover,
                ..Default::default()
            }),
        ))
    }

    fn entry_text(&self, path: &Path, entry: &str) -> Option<String> {
        let mut zip = Zip::open(path).ok()?;
        let bytes = zip.read(entry)?;
        Some(super::charset::decode(&bytes, None))
    }

    fn read_texts(&self, path: &Path) -> Option<Vec<String>> {
        let mut zip = Zip::open(path).ok()?;
        let entries = html_entries(&zip);
        let mut out = Vec::with_capacity(entries.len());
        for entry in entries {
            let bytes = zip.read(&entry)?;
            out.push(super::charset::decode(&bytes, None));
        }
        Some(out)
    }
}

/// ZIP 里的 HTML 条目：`index.html` 第一，其余按自然序。
fn html_entries(zip: &Zip) -> Vec<String> {
    let mut entries: Vec<String> = zip
        .names()
        .into_iter()
        .filter(|name| is_html(name))
        .collect();
    entries.sort_by(|a, b| {
        let index = |name: &str| {
            name.eq_ignore_ascii_case("index.html") || name.eq_ignore_ascii_case("index.htm")
        };
        index(b)
            .cmp(&index(a))
            .then_with(|| super::cbz::natural_cmp(a, b))
    });
    entries
}

fn is_html(name: &str) -> bool {
    matches!(
        super::extension_of(Path::new(name)).as_deref(),
        Some("html" | "htm" | "xhtml")
    )
}

fn file_stem(entry: &str) -> String {
    let name = entry.rsplit('/').next().unwrap_or(entry);
    name.rsplit_once('.')
        .map_or_else(|| name.to_string(), |(stem, _)| stem.to_string())
}

fn document_title_of(entry: &str, zip: &mut Zip) -> Option<String> {
    let bytes = zip.read(entry)?;
    super::html::document_title(&super::charset::decode(&bytes, None))
}

/// 一份 HTML 里的 `<style>` 与 `<link rel="stylesheet">`；外链 CSS 从 ZIP 读并按它自己的
/// 目录绝对化 `url(...)`。
fn styles_and_images(raw: &str, entry: &str, zip: &mut Zip) -> (String, Vec<ImageRes>) {
    let dom = Dom::parse(raw.as_bytes());
    let base = dir_of(entry);
    let mut css = String::new();
    let mut images = Vec::new();
    let mut seen = HashSet::new();

    for (id, _) in dom.elements() {
        match dom.tag_name(id) {
            Some("style") => {
                // 与 `html.rs`、宿主排版、量缺口的工具同一份出口（见 `css::style_text_of`）。
                if let Some(text) = super::css::style_text_of(&dom, id) {
                    css.push_str(&absolutize_css_urls(&text, &base));
                    css.push('\n');
                }
            }
            Some("link") => {
                // 「算不算样式表」只有 [`crate::content::scan::is_stylesheet_rel`] 一份：
                // CHM 那一族不建 DOM、走标记扫描，判据必须逐字相同。
                if !crate::content::scan::is_stylesheet_rel(dom.attr(id, "rel").unwrap_or_default())
                {
                    continue;
                }
                // `asset_href` 已经把空值、`data:`、带协议的外链折成空串，所以这里
                // 只问「是不是空」。
                let Some(href) = dom.attr(id, "href").map(asset_href) else {
                    continue;
                };
                if href.is_empty() {
                    continue;
                }
                let path = resolve_href(&base, &href);
                let Some(text) = zip.read_text(&path) else {
                    continue;
                };
                css.push_str(&absolutize_css_urls(&text, &dir_of(&path)));
                css.push('\n');
            }
            Some("img") => {
                let Some(src) = dom.attr(id, "src").map(asset_href) else {
                    continue;
                };
                if src.is_empty() {
                    continue;
                }
                let href = resolve_href(&base, &src);
                if !seen.insert(href.clone()) {
                    continue;
                }
                let Some(bytes) = zip.read(&href) else {
                    continue;
                };
                let ext = super::extension_of(Path::new(&href)).unwrap_or_default();
                let (width, height) = image_dims(&bytes);
                images.push(ImageRes {
                    href: href.clone(),
                    source: href,
                    media_type: media_type_of(&ext).to_string(),
                    width,
                    height,
                });
            }
            _ => {}
        }
    }
    (css, images)
}

/// 章内的图片引用（偏移来自可见文本扫描，`src` 换成容器绝对路径）。
fn image_uses(raw: &str, entry: &str) -> Vec<crate::content::scan::ImageUse> {
    let base = dir_of(entry);
    crate::content::scan::chapter_summary(raw)
        .images
        .into_iter()
        .map(|image| crate::content::scan::ImageUse {
            src: resolve_href(&base, &asset_href(&image.src)),
            offset: image.offset,
        })
        .collect()
}

/// OPF 声明的那张封面：先认 `<meta name="cover">` 指的那一条，没有就认 EPUB3 的
/// `properties="cover-image"`。读出字节缩到 [`COVER_MAX_EDGE`] 以内，缩不动（SVG、
/// 已经够小）就原样留 —— 与 EPUB 的 `parse_cover` 同一条口径。
///
/// HTMLZ 的容器形状是 Calibre 定的：`META-INF/container.xml` 指到 `OEBPS/content.opf`。
/// container 解不出来或指空时退到「第一个 `.opf` 条目」，两处都落空（ZIP 里根本没有
/// OPF）就是没有封面 —— **不猜图**：把正文第一张 `<img>` 当封面，书架很快会变成垃圾场。
///
/// 判定与解析都走 [`super::epub::parse_opf`]（HTMLZ 转出来的就是同一份 OPF），声明
/// 指到容器里不存在的条目（真书里有：Calibre 转换会留幽灵 manifest 条目）按读不到
/// 处理，同「没声明」。
fn cover_of(zip: &mut Zip) -> Option<Vec<u8>> {
    let opf_path = opf_path_of(zip)?;
    let text = zip.read_text(&opf_path)?;
    let opf = super::epub::parse_opf(&text);
    let base = dir_of(&opf_path);
    let item = opf
        .cover_id
        .as_deref()
        .and_then(|id| opf.items.iter().find(|i| i.id == id))
        .or_else(|| {
            opf.items
                .iter()
                .find(|i| i.properties.iter().any(|p| p == "cover-image"))
        })?;
    let href = resolve_href(&base, &item.href);
    let bytes = zip.read(&href)?;
    Some(match shrink_image(&bytes, COVER_MAX_EDGE) {
        Some((small, _mime)) => small,
        None => bytes,
    })
}

/// OPF 条目的容器路径：先按 container.xml 的 `rootfile full-path` 走，指空或缺席时
/// 退到第一个 `.opf` 条目（按名字自然序，Calibre 的形状 `OEBPS/content.opf` 排最前
/// 没有歧义；真有多个 .opf 的书取哪个都算猜测，取第一个并且不再兜第二层）。
fn opf_path_of(zip: &mut Zip) -> Option<String> {
    if let Some(container) = zip.read_text("META-INF/container.xml") {
        let dom = Dom::parse(container.as_bytes());
        for (id, _) in dom.elements() {
            if dom
                .tag_name(id)
                .is_some_and(|t| t.eq_ignore_ascii_case("rootfile"))
                && let Some(path) = dom
                    .attr(id, "full-path")
                    .map(str::trim)
                    .filter(|path| !path.is_empty())
                && zip.names().iter().any(|name| name == path)
            {
                return Some(path.to_string());
            }
        }
    }
    zip.names()
        .into_iter()
        .find(|name| name.to_ascii_lowercase().ends_with(".opf"))
}

fn unsupported(path: &Path, why: &str) -> crate::Error {
    crate::Error::UnsupportedFormat(format!(
        "{}：{why}",
        path.file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
    ))
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::path::Path;

    use super::HtmlzAdapter;
    use crate::content::format::{Content, DocumentProvider, Format};

    fn png(width: u32, height: u32) -> Vec<u8> {
        use image::ImageEncoder as _;
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba([4, 5, 6, 255]));
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), width, height, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    fn htmlz(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("书.htmlz");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        for (name, body) in [
            (
                "index.html",
                r#"<html><head><title>样书</title><link rel="stylesheet" href="style.css"></head><body><h1>开始</h1><p>导言</p><img src="Images/pic.png"/><a href="chapter1.html">下一章</a></body></html>"#
                    .as_bytes()
                    .to_vec(),
            ),
            (
                "chapter1.html",
                r#"<html><head><title>第二章</title></head><body><h1>第二章</h1><p>正文二</p></body></html>"#
                    .as_bytes()
                    .to_vec(),
            ),
            (
                "style.css",
                br#"body { background: url(Images/pic.png); }"#.to_vec(),
            ),
            ("Images/pic.png", png(12, 6)),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(&body).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    #[test]
    fn every_html_entry_is_a_chapter_and_images_resolve_inside_the_zip() {
        let dir = tempfile::tempdir().unwrap();
        let path = htmlz(dir.path());
        let (meta, content) = HtmlzAdapter.read(&path).unwrap();
        assert_eq!(meta.format, Format::Htmlz);
        assert_eq!(meta.title, "样书");
        let Content::Reflow(flow) = content else {
            panic!("HTMLZ 该读成可重排");
        };
        assert_eq!(flow.chapters.len(), 2);
        assert_eq!(flow.chapters[0].title, "样书", "title 优先于 h1");
        assert_eq!(flow.chapters[1].title, "第二章");
        assert_eq!(flow.chapters[0].spine_href, "index.html");
        assert_eq!(flow.chapters[1].spine_href, "chapter1.html");
        assert_eq!(flow.images.len(), 1);
        assert_eq!(flow.images[0].href, "Images/pic.png");
        assert_eq!((flow.images[0].width, flow.images[0].height), (12, 6));
        assert_eq!(flow.chapters[0].images.len(), 1);
        assert_eq!(flow.chapters[0].images[0].src, "Images/pic.png");
        assert!(
            flow.css.contains("url(\"Images/pic.png\")"),
            "CSS: {}",
            flow.css
        );
    }

    #[test]
    fn entry_text_returns_the_raw_html_for_the_engine() {
        let dir = tempfile::tempdir().unwrap();
        let path = htmlz(dir.path());
        let text = HtmlzAdapter.entry_text(&path, "chapter1.html").unwrap();
        assert!(text.contains("<h1>第二章</h1>"));
        assert!(HtmlzAdapter.entry_text(&path, "nope.html").is_none());
    }

    #[test]
    fn chapter_locator_is_an_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = htmlz(dir.path());
        let (_, content) = HtmlzAdapter.index(&path).unwrap();
        let Content::Reflow(flow) = content else {
            panic!("该读成可重排");
        };
        assert!(matches!(
            flow.chapters[1].locator(),
            Some(crate::content::format::ChapterLocator::Entry(entry)) if entry == "chapter1.html"
        ));
    }

    /// 一份带 OPF 的 HTMLZ（Calibre 形状：container.xml → OEBPS/content.opf）。
    /// `declared` 控封面怎么声明：`meta` / `property` / 不声明。
    fn htmlz_with_opf(dir: &Path, declared: &str) -> std::path::PathBuf {
        let path = dir.join("书.htmlz");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        let cover_item = match declared {
            "meta" => r#"<item id="cover" href="Images/cover.jpg" media-type="image/jpeg"/>"#,
            "property" => {
                r#"<item id="c0" href="Images/cover.jpg" media-type="image/jpeg" properties="cover-image"/>"#
            }
            _ => "",
        };
        let meta_line = match declared {
            "meta" => r#"<meta name="cover" content="cover"/>"#,
            _ => "",
        };
        for (name, body) in [
            ("META-INF/container.xml", br#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#.to_vec()),
            (
                "OEBPS/content.opf",
                format!(
                    r#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>样书</dc:title>{meta_line}</metadata><manifest>{cover_item}<item id="c1" href="Text/ch1.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="c1"/></spine></package>"#
                )
                .into_bytes(),
            ),
            (
                "OEBPS/Text/ch1.xhtml",
                r#"<html><head><title>第一章</title></head><body><p>正文</p></body></html>"#.as_bytes().to_vec(),
            ),
            ("OEBPS/Images/cover.jpg", png(1000, 2000)),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(&body).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    fn max_edge(bytes: &[u8]) -> u32 {
        let decoded = image::load_from_memory(bytes).unwrap();
        let (w, h) = (decoded.width(), decoded.height());
        w.max(h)
    }

    /// `<meta name="cover">` 指的那一张进 `FlowDoc.cover`，且已经缩到长边上限以内 ——
    /// 封面落盘是缩略图，1000×2000 的原图不该原样进库。
    #[test]
    fn a_meta_declared_cover_is_read_and_shrunk() {
        let dir = tempfile::tempdir().unwrap();
        let path = htmlz_with_opf(dir.path(), "meta");
        let (_, content) = HtmlzAdapter.index(&path).unwrap();
        let Content::Reflow(flow) = content else {
            panic!("该读成可重排");
        };
        let cover = flow.cover.expect("OPF 声明了封面");
        assert!(
            flow.images
                .iter()
                .all(|image| image.href != "OEBPS/Images/cover.jpg"),
            "封面没进正文 <img> 清单，就该只走封面那一路"
        );
        assert!(
            max_edge(&cover) <= 512,
            "封面该缩到 512 以内，实测 {}",
            max_edge(&cover)
        );
    }

    /// EPUB3 的 `properties="cover-image"` 是同一件事的另一种写法，同样认。
    #[test]
    fn a_property_declared_cover_is_read_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = htmlz_with_opf(dir.path(), "property");
        let (_, content) = HtmlzAdapter.index(&path).unwrap();
        let Content::Reflow(flow) = content else {
            panic!("该读成可重排");
        };
        assert!(flow.cover.is_some(), "properties 声明的封面也该进库");
    }

    /// 没声明就是没有：`cover` 是 `None`，导入照常成功 —— 不把正文首图当封面猜。
    #[test]
    fn an_undeclared_cover_leaves_the_field_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = htmlz_with_opf(dir.path(), "none");
        let (_, content) = HtmlzAdapter.index(&path).unwrap();
        let Content::Reflow(flow) = content else {
            panic!("该读成可重排");
        };
        assert!(flow.cover.is_none(), "没声明时不该猜一张图当封面");
    }

    /// 连 OPF 都没有的 HTMLZ（只有散装 HTML）照旧能导 —— 封面那一路只是空手而归，
    /// 不该把整本拖死。
    #[test]
    fn a_zip_without_an_opf_still_imports_without_a_cover() {
        let dir = tempfile::tempdir().unwrap();
        let path = htmlz(dir.path());
        let (_, content) = HtmlzAdapter.index(&path).unwrap();
        let Content::Reflow(flow) = content else {
            panic!("该读成可重排");
        };
        assert!(flow.cover.is_none());
        assert_eq!(flow.chapters.len(), 2);
    }

    /// 真书对拍：Calibre 转出的《藩屏》—— `<meta name="cover" content="f0">` 与
    /// `properties="cover-image"` 双声明都在，封面 1230×1756 的 progressive JPEG；
    /// manifest 里还挂着一条容器里不存在的幽灵条目（`Images/f2.jpg`）。三件事一起钉：
    /// 封面读得到、缩到上限以内且解得动；幽灵条目不拖垮导入。
    ///
    /// 样本不入库（63 MB），开发机上手动跑：
    ///
    /// ```sh
    /// cargo test -p folio-core --lib -- --ignored the_real_calibre_htmlz_cover
    /// ```
    #[test]
    #[ignore = "需要 tests/fixtures/local/formats/htmlz 下的本机真样本，来源见 tests/fixtures/README.md"]
    fn the_real_calibre_htmlz_cover_is_read_and_shrunk() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/local/formats/htmlz/calibre-shape.htmlz");
        assert!(
            path.is_file(),
            "真样本不存在：{}（手机书库拉取，见 tests/fixtures/README.md）",
            path.display()
        );
        let (meta, content) = HtmlzAdapter.index(&path).unwrap();
        assert_eq!(meta.format, Format::Htmlz);
        assert_eq!(meta.title, "藩屏：明代藩王的艺术与权力");
        let Content::Reflow(flow) = content else {
            panic!("该读成可重排");
        };
        let cover = flow.cover.expect("双声明都在，封面该读出来");
        let decoded = image::load_from_memory(&cover).expect("缩完的封面必须解得动");
        let edge = decoded.width().max(decoded.height());
        assert!(edge <= 512, "封面该缩到 512 以内，实测 {edge}");
        assert!(
            !flow.images.is_empty(),
            "正文图片清单与封面是两路，正文那路照旧要全"
        );
    }
}
