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
    image_dims, media_type_of, resolve_href, stem_of,
};
use crate::Result;
use crate::content::dom::Dom;

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
}
