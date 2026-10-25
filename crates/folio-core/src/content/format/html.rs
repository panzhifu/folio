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
    absolutize_css_urls, asset_href, dir_of, extension_of, image_dims, media_type_of, stem_of,
};
use crate::Result;
use crate::content::dom::{Dom, NodeId};

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
            ChapterLocator::Section(_) => super::charset::read_markup(path),
            other => Err(crate::Error::ChapterUnavailable {
                path: path.to_path_buf(),
                index: 0,
                at: format!("{other:?}"),
            }),
        }
    }

    fn index(&self, path: &Path) -> Result<(DocMeta, Content)> {
        // 走 `charset::read_markup` 而不是 `read_to_string`：这一族的正文按规范**可以**不是
        // UTF-8，只要文件自己声明了码页（`<meta http-equiv=… charset=gb2312>` 或 XML 的
        // `encoding=`），中文网页另存出来的单文件 HTML 满地是那种。声明那一档早就写好了，
        // 以前只是没接进来。
        let raw = super::charset::read_markup(path)?;
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
///
/// 不只单文件 `.html` 用它：PalmDOC 那一族里正文写着标记的那些本子（`format::pdb`）也问
/// 同一个问题，两份实现在「`<title>` 与 `<h1>` 谁算书名」上迟早会分叉。
pub(crate) fn document_title(raw: &str) -> Option<String> {
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

/// 一条作者写下的引用**够不够得着**：绝对路径、UNC、带盘符的一律不收。
///
/// 单文件 HTML 的「容器」就是磁盘上那一个目录，而 `Path::join` 有个众所周知的性质：
/// 右侧是绝对路径（`/etc/passwd`）或带盘符（`C:/Windows/win.ini`）时**整个赢**，
/// join 出来的早不在书那个目录里。所以书里一句 `<link rel="stylesheet" href="/etc/hostname">`
/// 不该让阅读器把宿主的内容当样式表读进去，`<img src="/…">` 同理。
/// `..` 有意留着：把 CSS 放在书的隔壁目录（`../css/a.css`）是手工转出来的单文件书的
/// 合法形状，而它仍落在「这本书附近」这条线上 —— 越界的是绝对形式，不是相对形式。
/// 这与 `format::comic_cache` 那条「解压只写书目录内、拒绝绝对路径与盘符」是同一口径的两半。
fn reachable_from_the_book(href: &str) -> bool {
    let bytes = href.as_bytes();
    let drive_letter =
        bytes.first().is_some_and(|c| c.is_ascii_alphabetic()) && href.get(1..2) == Some(":");
    !(href.starts_with('/') || href.starts_with('\\') || drive_letter)
}

/// `<style>` 文本 + `<link rel=stylesheet>` 拼成作者样式；`<img src>` 解析成图片元数据。
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
                // 「这一份 `<style>` 的正文」只有 [`super::css::style_text_of`] 一份，
                // 宿主排版与量 CSS 缺口的工具都读它，三处不许各长一个样。
                if let Some(text) = super::css::style_text_of(&dom, id) {
                    css.push_str(&text);
                    css.push('\n');
                }
            }
            // 外链样式表。这一族从前只认内联 `<style>`，「手工转出来的 book.html + css/
            // 那一堆」整包样式看不见 —— 与 CHM 那个入口缺口是同一条判据，所以「算不算
            // 样式表」共用 [`crate::content::scan::is_stylesheet_rel`] 那一份（CHM 不建
            // DOM、走标记扫描，两边判据不许长得不一样）。
            //
            // 取不到就跳过这一份：少一份样式正文照排，与图片缺失同一立场；塞一条空规则
            // 才是难查的那种错。
            Some("link") => {
                if !crate::content::scan::is_stylesheet_rel(dom.attr(id, "rel").unwrap_or_default())
                {
                    continue;
                }
                let Some(href) = dom.attr(id, "href").map(asset_href) else {
                    continue;
                };
                if href.is_empty() || !reachable_from_the_book(&href) {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(dir.join(&href)) else {
                    continue;
                };
                // 每一**份**外链在它自己的目录里：`css/a.css` 写 `url(../img/x.png)`
                // 指的是 html 隔壁那个 `img/`，按 html 的目录折就折错了。内联那块不必折
                // —— 它的基准本来就是这一份 html 的目录，而图片登记也按那一个目录 join。
                css.push_str(&absolutize_css_urls(&text, &dir_of(&href)));
                css.push('\n');
            }
            Some("img") => {
                let Some(src) = dom.attr(id, "src") else {
                    continue;
                };
                let href = asset_href(src);
                if href.is_empty()
                    || !reachable_from_the_book(&href)
                    || images.iter().any(|i| i.href == href)
                {
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
/// 走 [`Dom::raw_text_of_subtree`]：[`Dom::text_of_subtree`] 是给锚点坐标系用的，它故意
/// 剔掉这些「非内容」元素；而这里要的恰恰是它们（书名、样式表）。
fn raw_text(dom: &Dom, root: NodeId) -> String {
    dom.raw_text_of_subtree(root)
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

    /// 声明那一档以前对单文件 HTML **根本不生效**：入口是 `read_to_string`，GBK 的字节
    /// 在那一步就整本打不开。中文网页另存出来的文件满地是这个形态 —— 它自带的
    /// `charset=gbk` 本来就是够的，缺的只是把这条链接上。
    #[test]
    fn a_declared_gbk_html_is_decoded_from_its_own_declaration() {
        let dir = tempfile::tempdir().unwrap();
        let doc = "<html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=gbk\"><title>书名</title></head><body><p>大雪封山的那一夜</p></body></html>";
        let bytes = encoding_rs::GBK.encode(doc).0.to_vec();
        assert!(
            std::str::from_utf8(&bytes).is_err(),
            "这份字节必须真的不是 UTF-8，否则这条测试没测到东西"
        );
        let path = write(dir.path(), "gbk.html", &bytes);
        let (meta, flow) = flow_of(&path);
        assert_eq!(meta.title, "书名", "标题要从 GBK 的字节里按声明解出来");
        let body = &flow.chapters[0].source();
        assert!(body.contains("大雪封山"), "正文不能是一排问号：{body}");
        assert!(!body.contains('\u{fffd}'), "解得通就不该留洞：{body}");
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

    /// 单文件 HTML 认 `<link rel="stylesheet">`。从前这里只认内联 `<style>`，
    /// 「book.html + 旁边那个 css/」这种手工转出来的书**整包样式看不见** ——
    /// 屏上不显示异常，`css-gap` 那张表也不算落差，因为压根没有声明进过层叠。
    ///
    /// 两条一起钉：`rel="icon"` 是诱饵（按 rel 的令牌列表它不是样式表），
    /// 而 href 里的 `%20` 必须解得开（真书名里有空格是常态）。
    #[test]
    fn a_linked_stylesheet_comes_in_while_the_icon_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("css")).unwrap();
        write(
            dir.path(),
            "css/a b.css",
            b"p { text-indent: 2em } h1 { font-weight: 900 }",
        );
        write(dir.path(), "css/icon-only.css", b"p { color: #ff0000 }");
        let book = write(
            dir.path(),
            "book.html",
            r#"<html><head><link rel="stylesheet" href="css/a%20b.css">
               <link rel="icon" href="css/icon-only.css"></head>
               <body><p>文</p></body></html>"#
                .as_bytes(),
        );
        let (_, flow) = flow_of(&book);
        assert!(
            flow.css.contains("text-indent: 2em"),
            "带空格的 href 要解得开：{}",
            flow.css
        );
        assert!(
            !flow.css.contains("#ff0000"),
            "rel=\"icon\" 不是样式表：{}",
            flow.css
        );
    }

    /// 绝对路径与带盘符的引用**不碰磁盘**。
    ///
    /// 这条不是洁癖：`Path::join` 在右侧是绝对路径时整个赢，所以书里一句
    /// `<link rel="stylesheet" href="/etc/hostname">` 就会让阅读器把宿主上任意一个文本
    /// 文件当作者样式读进层叠。判据写在 [`reachable_from_the_book`]，与
    /// `format::comic_cache` 那条「解压只写书目录内、拒绝绝对路径与盘符」同一口径。
    #[test]
    fn an_absolute_reference_never_reaches_the_filesystem() {
        let dir = tempfile::tempdir().unwrap();
        let outside = write(dir.path(), "outside.css", b"p { color: #112233 }");
        let href = outside.to_str().expect("tempdir 路径是 UTF-8").to_string();
        let html = format!(
            "<html><head><link rel=\"stylesheet\" href=\"{href}\"></head>\
             <body><img src=\"{href}\"><p>文</p></body></html>"
        );
        let book = write(dir.path(), "book.html", html.as_bytes());
        let (_, flow) = flow_of(&book);
        assert!(
            !flow.css.contains("112233"),
            "绝对路径那条 link 不该被读进来：{}",
            flow.css
        );
        assert!(
            flow.images.is_empty(),
            "绝对路径的 img 不该登记：{:?}",
            flow.images
        );
    }
}
