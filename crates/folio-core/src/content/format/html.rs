//! 单个 HTML / XHTML 文件当「只有一章的书」读进来。
//!
//! 覆盖的是别人手工转出来的单文件书、网页存档、以及随手丢进来的 `.html`。
//! **多文件的网站目录不在范围内**：那需要「哪份是主文档」的启发式，而猜错的代价是
//! 整本书只读到一半 —— 宁可让用户明确地把正文存成一个文件。
//!
//! `<style>` 里的规则抽出来当作者样式，`<img>` 按相对路径登记成图片元数据；
//! 图片字节走 [`HtmlAdapter::image_bytes`] 从磁盘现读，与 EPUB 同一套「不入库」的规矩。

use std::path::{Path, PathBuf};

use super::{
    Chapter, ChapterLocator, Content, DocMeta, DocumentProvider, FlowDoc, Format, ImageRes,
    extension_of, image_dims, media_type_of, percent_decode, stem_of,
};
use crate::Result;
use crate::content::dom::{Dom, NodeId, NodeKind};
use crate::infra::error::IoResultExt as _;

pub struct HtmlAdapter;

impl DocumentProvider for HtmlAdapter {
    fn format(&self) -> Format {
        Format::Html
    }

    fn supports(&self, path: &Path) -> bool {
        matches!(
            extension_of(path).as_deref(),
            Some("html" | "htm" | "xhtml")
        )
    }

    /// 整篇一章：索引这一步读到的东西（标题 / CSS / 图片 / 封面）本来就要进库，
    /// 正文等打开时再取。
    /// 整篇就是那一章：重读一遍文件。
    fn chapter_source(&self, path: &Path, at: &ChapterLocator) -> Result<String> {
        match at {
            ChapterLocator::Section(_) => std::fs::read_to_string(path).io_path(path),
            other => Err(crate::Error::ChapterUnavailable {
                path: path.to_path_buf(),
                index: 0,
                at: format!("{other:?}"),
            }),
        }
    }

    fn index(&self, path: &Path) -> Result<(DocMeta, Content)> {
        let raw = std::fs::read_to_string(path).io_path(path)?;
        let title = document_title(&raw).unwrap_or_else(|| stem_of(path));
        let (css, images) = styles_and_images(&raw, path);
        let meta = DocMeta {
            title: title.clone(),
            authors: Vec::new(),
            format: Format::Html,
            size: raw.len() as u64,
            ..DocMeta::default()
        };
        let flow = FlowDoc {
            // 字数在索引这一趟数出来（`chapters.char_len`）：文件已经整份读过一遍，
            // 而 `chapter_source` 交回来的正文与这里数的是同一份原始内容 ——
            // `visible_chars` 走 DOM，标记不计入，只有可见文本进那根尺。
            //
            // 图片清单另走一趟 `chapter_summary`（它才是那一族的判据）：单文件只有
            // 一章、文件已在内存，多扫一遍是几微秒，不值得为此换掉上面那把尺。
            chapters: vec![{
                let images = crate::content::scan::chapter_summary(&raw).images;
                Chapter::section(title, 0)
                    .with_char_len(super::visible_chars(&raw))
                    .with_images(images)
            }],
            css,
            images,
            ..Default::default()
        };
        Ok((meta, Content::Reflow(flow)))
    }

    fn image_bytes(&self, path: &Path, href: &str) -> Option<Vec<u8>> {
        // 单文件 HTML 的图是磁盘上挨着它的另一个文件：`src` 相对本文件所在目录。
        // 不让 `resolve_href` 来过一遍 —— 它会把 `../` 折叠掉，那个语义是给容器内路径的，
        // 落到文件系统上就指错地方了。
        std::fs::read(file_dir(path).join(href)).ok()
    }
}

/// HTML 文件所在目录。没有父目录（根下、或路径怪）时退到当前目录，
/// 让 `join("a.png")` 仍是个合法相对路径而不是空串。
fn file_dir(path: &Path) -> PathBuf {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(PathBuf::new, Path::to_path_buf)
}

/// `<title>` 优先，退到第一个非空 `<h1>`；都没有就交给调用方用文件名。
fn document_title(raw: &str) -> Option<String> {
    let dom = Dom::parse(raw.as_bytes());
    let mut fallback = None;
    for (id, _) in dom.elements() {
        match dom.tag_name(id) {
            Some("title") => {
                let text = raw_text(&dom, id).trim().to_string();
                if !text.is_empty() {
                    return Some(text);
                }
            }
            Some("h1") if fallback.is_none() => {
                let text = raw_text(&dom, id).trim().to_string();
                if !text.is_empty() {
                    fallback = Some(text);
                }
            }
            _ => {}
        }
    }
    fallback
}

/// `<style>` 文本拼成作者样式；`<img src>` 解析成图片元数据。
///
/// 图片要读磁盘拿固有尺寸，读不到的（路径错、格式不认）就**不登记**：登记成 0×0
/// 会让排版按容器宽度硬拉，反而把版面撑坏。
fn styles_and_images(raw: &str, path: &Path) -> (String, Vec<ImageRes>) {
    let dom = Dom::parse(raw.as_bytes());
    let dir = file_dir(path);
    let mut css = String::new();
    let mut images: Vec<ImageRes> = Vec::new();
    for (id, _) in dom.elements() {
        match dom.tag_name(id) {
            Some("style") => {
                let text = raw_text(&dom, id);
                if !text.trim().is_empty() {
                    css.push_str(&text);
                    css.push('\n');
                }
            }
            Some("img") => {
                let Some(src) = dom.attr(id, "src") else {
                    continue;
                };
                let href = image_href(src);
                if href.is_empty() || images.iter().any(|i| i.href == href) {
                    continue;
                }
                let Ok(bytes) = std::fs::read(dir.join(&href)) else {
                    continue;
                };
                let (width, height) = image_dims(&bytes);
                let ext = extension_of(Path::new(&href)).unwrap_or_default();
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

/// 该元素子树里的**原始**文本，不跳过 `title` / `style` / `script`。
///
/// [`Dom::text_of_subtree`] 是给锚点坐标系用的，它故意剔掉这些「非内容」元素；
/// 而这里要的恰恰是它们（书名、样式表），所以自己走一遍。
fn raw_text(dom: &Dom, root: NodeId) -> String {
    let mut out = String::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        let node = dom.get(id);
        if node.kind == NodeKind::Text {
            out.push_str(&node.text);
        }
        stack.extend(node.children.iter().rev().copied());
    }
    out
}

/// `src` → 磁盘相对路径：去掉 query / fragment，百分号解码，其余原样（含 `..`）。
/// `data:` 与绝对 URL 一律空串 —— 前者不是文件，后者不该由单文件适配器去取。
fn image_href(src: &str) -> String {
    let src = src.trim();
    if src.is_empty() || src.starts_with("data:") || src.contains("://") {
        return String::new();
    }
    let cut = src.split(['?', '#']).next().unwrap_or(src);
    percent_decode(cut)
}

#[cfg(test)]
mod tests {
    //! 单文件 HTML：标题回落、样式抽取、图片登记与按需读字节。

    use std::path::{Path, PathBuf};

    use super::HtmlAdapter;
    use crate::content::format::{Content, DocumentProvider, Format};

    fn png(width: u32, height: u32) -> Vec<u8> {
        use image::ImageEncoder as _;
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba([1, 2, 3, 255]));
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), width, height, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn flow_of(
        path: &Path,
    ) -> (
        crate::content::format::DocMeta,
        crate::content::format::FlowDoc,
    ) {
        let (meta, content) = HtmlAdapter.read(path).unwrap();
        let Content::Reflow(flow) = content else {
            panic!("这个容器该读成可重排");
        };
        (meta, flow)
    }

    #[test]
    fn it_claims_html_extensions_and_reports_the_format() {
        assert_eq!(HtmlAdapter.format(), Format::Html);
        assert!(HtmlAdapter.supports(Path::new("a.html")));
        assert!(HtmlAdapter.supports(Path::new("A.HTM")));
        assert!(HtmlAdapter.supports(Path::new("b.xhtml")));
        assert!(!HtmlAdapter.supports(Path::new("c.md")));
    }

    #[test]
    fn the_title_comes_from_title_then_h1_then_the_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let with_title = write(
            dir.path(),
            "a.html",
            b"<html><head><title>\xe4\xb9\xa6\xe5\x90\x8d</title></head><body><h1>H</h1></body></html>",
        );
        assert_eq!(flow_of(&with_title).0.title, "书名");

        let with_h1 = write(
            dir.path(),
            "b.html",
            b"<html><body><h1>\xe5\xa4\xa7\xe6\xa0\x87\xe9\xa2\x98</h1></body></html>",
        );
        assert_eq!(flow_of(&with_h1).0.title, "大标题");

        let bare = write(dir.path(), "c.html", b"<html><body><p>x</p></body></html>");
        assert_eq!(flow_of(&bare).0.title, "c");
    }

    #[test]
    fn style_blocks_become_the_author_stylesheet() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "a.html",
            b"<html><head><style>p { text-indent: 2em }</style></head><body><p>x</p></body></html>",
        );
        let (_, flow) = flow_of(&path);
        assert!(flow.css.contains("text-indent: 2em"), "{}", flow.css);
        // 正文原样透传：坐标系的稳定性靠「不转换」。
        assert_eq!(flow.chapters.len(), 1);
        assert!(flow.chapters[0].source().contains("<p>x</p>"));
    }

    #[test]
    fn images_are_registered_with_their_intrinsic_size_and_read_on_demand() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("img")).unwrap();
        write(&dir.path().join("img"), "fig.png", &png(40, 20));
        let path = write(
            dir.path(),
            "a.html",
            b"<html><body><img src=\"img/fig.png\"><img src=\"img/missing.png\"><img src=\"data:image/png;base64,AA\"></body></html>",
        );
        let (_, flow) = flow_of(&path);
        assert_eq!(flow.images.len(), 1, "只登记读得到的那张");
        assert_eq!(flow.images[0].href, "img/fig.png");
        assert_eq!(flow.images[0].media_type, "image/png");
        assert_eq!((flow.images[0].width, flow.images[0].height), (40, 20));

        let bytes = HtmlAdapter.image_bytes(&path, "img/fig.png").unwrap();
        assert_eq!(bytes.len(), png(40, 20).len());
        assert!(HtmlAdapter.image_bytes(&path, "nope.png").is_none());
    }

    /// 同一张图在正文里出现两次只登记一条；百分号编码的路径也要能读。
    #[test]
    fn a_percent_encoded_and_repeated_image_is_deduped_and_decoded() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a b.png", &png(8, 8));
        let path = write(
            dir.path(),
            "a.html",
            b"<html><body><img src=\"a%20b.png\"><img src=\"a%20b.png\"></body></html>",
        );
        let (_, flow) = flow_of(&path);
        assert_eq!(flow.images.len(), 1);
        assert_eq!(
            flow.images[0].href, "a b.png",
            "百分号解码后才是磁盘上的名字"
        );
        assert!(HtmlAdapter.image_bytes(&path, "a b.png").is_some());
    }
}
