//! FictionBook 2（`.fb2`）：一个 XML 文件装下元数据、正文与内嵌图片。
//!
//! 适配器做两件事：把 `<section>` 树转成排版层认的 XHTML 片段，以及把 `<binary>` 里的
//! base64 图片登记成**元数据**（字节按需再解一次，与 EPUB 同一套「不入库」的规矩）。
//!
//! 不追求把 FB2 的每种元素都翻译过来：**认不得的标签只丢标签、留着内容**。真实书里总有
//! 些用不上的结构，为一两个元素让整本排版失败是最糟的失败方式。
//!
//! 只读主正文那一个 `<body>`：带 `name` 属性的（`notes` / `comments`）是脚注与注释，
//! 把它们混进正文会让每一章后面凭空多出一堆注释文字。脚注渲染是 M8 的事。

use std::path::Path;

use base64::Engine as _;
use quick_xml::Reader;
use quick_xml::events::Event;

use super::{
    Chapter, ChapterLocator, Content, DocMeta, DocumentProvider, FlowDoc, Format, ImageRes,
    image_dims, shrink_image, stem_of,
};
use crate::Result;
use crate::infra::error::IoResultExt as _;

/// 封面上限与 EPUB 同一条口径（见 `epub` 里的 `COVER_MAX_EDGE`）：书架用缩略图，
/// 原图随时能从文件里再解一次，不该在库里留第二份。
const COVER_MAX_EDGE: u32 = 512;

pub struct Fb2Adapter;

/// `with_text == false` 的那一趟：章表只有标题与序号，正文等打开时再扫。
fn read_with(path: &Path, with_text: bool) -> Result<(DocMeta, Content)> {
    let raw = std::fs::read_to_string(path).io_path(path)?;
    let parsed = parse_metadata(&raw);

    let images: Vec<ImageRes> = parsed
        .binaries
        .iter()
        .map(|b| {
            let (width, height) = image_dims(&b.bytes);
            ImageRes {
                href: b.id.clone(),
                media_type: b.media_type.clone(),
                width,
                height,
            }
        })
        .collect();
    let cover = parsed
        .cover_id
        .as_ref()
        .and_then(|id| parsed.binaries.iter().find(|b| &b.id == id))
        .map(|b| match shrink_image(&b.bytes, COVER_MAX_EDGE) {
            Some((bytes, _)) => bytes,
            None => b.bytes.clone(),
        });

    let title = if parsed.title.is_empty() {
        stem_of(path)
    } else {
        parsed.title
    };
    let mut chapters = parse_chapters(&raw);
    if chapters.is_empty() {
        // 结构怪到一段正文都没抽出来时，仍然给一章空白，让「打开了一本没内容的书」
        // 与「打不开这本书」在界面上能分开。
        chapters.push(Chapter::inline(title.clone(), "<body></body>"));
    }
    let meta = DocMeta {
        title,
        authors: parsed.authors,
        format: Format::Fb2,
        size: raw.len() as u64,
    };
    // 索引那一趟：章表只留标题与序号，**但字数要留下来**。
    //
    // `parse_chapters` 这一趟已经把每章的 XHTML 拼出来了（单文件 XML，标题与正文
    // 出自同一趟扫描），所以字数在这里是白拿的。换成 `Chapter::section` 会把那份
    // 正文丢掉，而丢掉之后「这一章有多少字」就再也算不出来 ——
    // `Chapter::section` 那一支只留位置，正文要等翻开才回容器取。
    let chapters = if with_text {
        chapters
    } else {
        chapters
            .into_iter()
            .enumerate()
            .map(|(ix, chapter)| {
                let chars = chapter.char_len();
                Chapter::section(chapter.title.clone(), ix).with_char_len(chars)
            })
            .collect()
    };
    let flow = FlowDoc {
        chapters,
        css: String::new(),
        css_hrefs: Vec::new(),
        toc: Vec::new(),
        cover,
        images,
        // 同上：落盘之后由管线填。
        cover_ref: None,
    };
    tracing::debug!(
        "fb2: {} 读到 {} 章、{} 张内嵌图、封面 {}",
        path.display(),
        flow.chapters.len(),
        flow.images.len(),
        flow.cover.as_ref().map_or("无", |_| "有"),
    );
    Ok((meta, Content::Reflow(flow)))
}

impl DocumentProvider for Fb2Adapter {
    fn format(&self) -> Format {
        Format::Fb2
    }

    fn supports(&self, path: &Path) -> bool {
        matches!(super::extension_of(path).as_deref(), Some("fb2"))
    }

    /// 只读索引：章表（标题 + 序号）、封面、内嵌图元数据。
    ///
    /// FB2 是**单文件 XML**，标题与正文出自同一趟扫描，所以这一趟仍然要过一遍
    /// XML —— 省下的是把每一段的 XHTML 拼出来（`parse_chapters` 里那部分），
    /// 不是读盘。
    fn index(&self, path: &Path) -> Result<(DocMeta, Content)> {
        read_with(path, false)
    }

    /// 单文件容器里的第 N 章：重新扫一遍 XML 取第 N 段。
    fn chapter_source(&self, path: &Path, at: &ChapterLocator) -> Result<String> {
        let index = match at {
            ChapterLocator::Section(index) => *index,
            other => {
                return Err(crate::Error::ChapterUnavailable {
                    path: path.to_path_buf(),
                    index: 0,
                    at: format!("{other:?}"),
                });
            }
        };
        let (_, content) = read_with(path, true)?;
        let Content::Reflow(flow) = content else {
            return Err(crate::Error::ChapterUnavailable {
                path: path.to_path_buf(),
                index,
                at: format!("{at:?}"),
            });
        };
        flow.chapters
            .get(index)
            .map(|c| c.source().to_string())
            .ok_or_else(|| crate::Error::ChapterUnavailable {
                path: path.to_path_buf(),
                index,
                at: format!("{at:?}"),
            })
    }

    fn read(&self, path: &Path) -> Result<(DocMeta, Content)> {
        read_with(path, true)
    }

    fn image_bytes(&self, path: &Path, href: &str) -> Option<Vec<u8>> {
        // 图片在文件里是 base64 的 `<binary>`，每次按 id 现解一次 —— 与 EPUB 每次
        // 重开容器同形，字节不常驻。
        let raw = std::fs::read_to_string(path).ok()?;
        parse_metadata(&raw)
            .binaries
            .into_iter()
            .find(|b| b.id == href)
            .map(|b| b.bytes)
    }
}

#[derive(Default)]
struct Parsed {
    title: String,
    authors: Vec<String>,
    binaries: Vec<Binary>,
    cover_id: Option<String>,
}

struct Binary {
    id: String,
    media_type: String,
    bytes: Vec<u8>,
}

fn local_name(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn attr(start: &quick_xml::events::BytesStart<'_>, want: &str) -> Option<String> {
    for a in start.attributes().flatten() {
        if local_name(a.key.as_ref()) == want {
            return Some(
                quick_xml::escape::unescape(&a.value)
                    .map(|c| c.into_owned())
                    .unwrap_or_else(|_| a.value.to_string()),
            );
        }
    }
    None
}

fn text_of(raw: &str) -> String {
    quick_xml::escape::unescape(raw)
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| raw.to_string())
}

/// `<image xlink:href="#cover.jpg"/>` → `cover.jpg`。
fn image_id(start: &quick_xml::events::BytesStart<'_>) -> Option<String> {
    let href = attr(start, "href")?;
    let id = href.strip_prefix('#').unwrap_or(&href).trim();
    (!id.is_empty()).then(|| id.to_string())
}

/// 元数据与内嵌图片：书名、作者、`<binary>` 表、封面 id。
fn parse_metadata(xml: &str) -> Parsed {
    let mut out = Parsed::default();
    let mut reader = Reader::from_str(xml);
    // 正在采集文本的元素：书名，或作者的一个名字部件。
    let mut want_title = false;
    let mut want_author_part = false;
    let mut in_author = false;
    let mut in_coverpage = false;
    let mut buf = String::new();
    let mut author_parts: Vec<String> = Vec::new();
    let mut binary: Option<(String, String, String)> = None;

    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Start(e) => {
                let n = e.name();
                let name = local_name(n.as_ref());
                match name {
                    "book-title" => {
                        want_title = true;
                        buf.clear();
                    }
                    "author" => {
                        in_author = true;
                        author_parts.clear();
                    }
                    "first-name" | "middle-name" | "last-name" | "nickname" if in_author => {
                        want_author_part = true;
                        buf.clear();
                    }
                    "coverpage" => in_coverpage = true,
                    "binary" => {
                        binary = Some((
                            attr(&e, "id").unwrap_or_default(),
                            attr(&e, "content-type").unwrap_or_default(),
                            String::new(),
                        ));
                    }
                    _ => {}
                }
            }
            Event::Empty(e) => {
                let n = e.name();
                if in_coverpage
                    && local_name(n.as_ref()) == "image"
                    && let Some(id) = image_id(&e)
                {
                    out.cover_id = Some(id);
                }
            }
            Event::Text(e) => {
                let text = text_of(e.as_ref());
                if let Some((_, _, body)) = binary.as_mut() {
                    body.push_str(&text);
                } else if want_title || want_author_part {
                    buf.push_str(&text);
                }
            }
            Event::End(e) => {
                let n = e.name();
                let name = local_name(n.as_ref());
                match name {
                    "book-title" => {
                        if out.title.is_empty() {
                            out.title = buf.trim().to_string();
                        }
                        want_title = false;
                    }
                    "author" => {
                        in_author = false;
                        let joined = author_parts
                            .iter()
                            .filter(|s| !s.is_empty())
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(" ");
                        if !joined.is_empty() {
                            out.authors.push(joined);
                        }
                    }
                    "first-name" | "middle-name" | "last-name" | "nickname" => {
                        if want_author_part {
                            author_parts.push(buf.trim().to_string());
                        }
                        want_author_part = false;
                    }
                    "coverpage" => in_coverpage = false,
                    "binary" => {
                        if let Some((id, media_type, text)) = binary.take()
                            && let Some(bytes) = decode_base64(&text)
                        {
                            out.binaries.push(Binary {
                                id,
                                media_type,
                                bytes,
                            });
                        }
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// `<binary>` 里的 base64。FB2 常按 76 列折行，所以先把所有空白去掉再解；
/// 解不出（截断、混进了非 base64 字符）就当没有这张图，不让它带崩整本书。
fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(compact)
        .ok()
}

/// 正文：每个顶层 `<section>` 一章，嵌套 section 的内容并进父章。
fn parse_chapters(xml: &str) -> Vec<Chapter> {
    let mut reader = Reader::from_str(xml);
    let mut depth = 0usize;
    // 主正文那一个 `<body>` 的深度；带 `name` 的 body（notes）不进这里。
    let mut body_depth: Option<usize> = None;
    // 每个已开 tag 对应的闭合串；`None` 表示这个 tag 不产出标记（只透传子内容）。
    let mut stack: Vec<Option<String>> = Vec::new();
    let mut section_depths: Vec<usize> = Vec::new();
    let mut buf = String::new();
    let mut chapter_title = String::new();
    let mut chapters: Vec<Chapter> = Vec::new();
    let mut in_title = false;
    let mut heading_level = 1u8;
    let mut title_text = String::new();
    // `<binary>` 的正文是 base64，绝不能漏进章节。
    let mut skip_depth: Option<usize> = None;

    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Start(e) => {
                depth += 1;
                let n = e.name();
                let name = local_name(n.as_ref());
                if skip_depth.is_some() || name == "binary" {
                    if name == "binary" && skip_depth.is_none() {
                        skip_depth = Some(depth);
                    }
                    stack.push(None);
                    continue;
                }
                if name == "body" && body_depth.is_none() && attr(&e, "name").is_none() {
                    body_depth = Some(depth);
                    stack.push(None);
                    continue;
                }
                let Some(bd) = body_depth else {
                    stack.push(None);
                    continue;
                };

                if name == "section" {
                    if depth == bd + 1 {
                        flush(&mut chapters, &mut buf, &mut chapter_title);
                    }
                    section_depths.push(depth);
                    buf.push_str("<div>");
                    stack.push(Some("</div>".to_string()));
                    continue;
                }
                if name == "title" {
                    in_title = true;
                    title_text.clear();
                    let section = section_depths.last().copied().unwrap_or(bd + 1);
                    heading_level = section.saturating_sub(bd).clamp(1, 6) as u8;
                    stack.push(None);
                    continue;
                }
                if name == "p" {
                    if in_title {
                        buf.push_str(&format!("<h{heading_level}>"));
                        stack.push(Some(format!("</h{heading_level}>")));
                    } else {
                        buf.push_str("<p>");
                        stack.push(Some("</p>".to_string()));
                    }
                    continue;
                }
                if name == "empty-line" {
                    buf.push_str("<p>&#160;</p>");
                    stack.push(None);
                    continue;
                }
                if name == "image" {
                    push_image(&mut buf, &e);
                    stack.push(None);
                    continue;
                }
                match inline_tag(name) {
                    Some(tag) => {
                        buf.push('<');
                        buf.push_str(tag);
                        if name == "a"
                            && let Some(href) = attr(&e, "href")
                        {
                            buf.push_str(&format!(" href=\"{}\"", escape_attr(&href)));
                        }
                        buf.push('>');
                        stack.push(Some(format!("</{tag}>")));
                    }
                    None => stack.push(None),
                }
            }
            Event::Empty(e) => {
                if skip_depth.is_some() || body_depth.is_none() {
                    continue;
                }
                let n = e.name();
                match local_name(n.as_ref()) {
                    "empty-line" => buf.push_str("<p>&#160;</p>"),
                    "image" => push_image(&mut buf, &e),
                    _ => {}
                }
            }
            Event::Text(e) => {
                if skip_depth.is_some() || body_depth.is_none() {
                    continue;
                }
                let text = text_of(e.as_ref());
                if in_title {
                    title_text.push_str(&text);
                }
                escape_into(&mut buf, &text);
            }
            Event::End(e) => {
                let n = e.name();
                let name = local_name(n.as_ref());
                if let Some(sd) = skip_depth {
                    if depth == sd {
                        skip_depth = None;
                    }
                } else if let Some(bd) = body_depth {
                    if name == "title" {
                        in_title = false;
                        if chapter_title.is_empty() {
                            let taken = title_text.trim();
                            if !taken.is_empty() {
                                chapter_title = taken.to_string();
                            }
                        }
                    }
                    if let Some(close) = stack.pop().flatten() {
                        buf.push_str(&close);
                    }
                    if name == "section" {
                        section_depths.pop();
                        if depth == bd + 1 {
                            flush(&mut chapters, &mut buf, &mut chapter_title);
                        }
                    }
                    if name == "body" && depth == bd {
                        flush(&mut chapters, &mut buf, &mut chapter_title);
                        body_depth = None;
                    }
                } else {
                    stack.pop();
                }
                depth -= 1;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    // 文件在半截处断了（缺闭合标签）时，读到的内容不能跟着一起丢。
    flush(&mut chapters, &mut buf, &mut chapter_title);
    chapters
}

/// FB2 的行内元素 → XHTML 行内标签。`table` 那几个块级容器在排版层没有对应物，
/// 映射成 `div` / `p` 让内容照样读得到（表格线不是阅读的必需品）。
fn inline_tag(name: &str) -> Option<&'static str> {
    match name {
        "emphasis" => Some("em"),
        "strong" => Some("strong"),
        "strikethrough" => Some("s"),
        "sub" => Some("sub"),
        "sup" => Some("sup"),
        "code" => Some("code"),
        "style" => Some("span"),
        "a" => Some("a"),
        "cite" | "epigraph" | "poem" | "stanza" | "annotation" | "table" => Some("div"),
        "v" | "subtitle" | "date" | "tr" => Some("p"),
        _ => None,
    }
}

fn push_image(buf: &mut String, e: &quick_xml::events::BytesStart<'_>) {
    if let Some(id) = image_id(e) {
        buf.push_str(&format!("<img src=\"{}\"/>", escape_attr(&id)));
    }
}

/// 收尾当前缓冲：有内容就成一章。标题缺省给「第 N 节」——
/// 目录里出现一个空标题比一个编号难看得多。
fn flush(chapters: &mut Vec<Chapter>, buf: &mut String, chapter_title: &mut String) {
    if !has_content(buf) {
        buf.clear();
        chapter_title.clear();
        return;
    }
    let source = std::mem::take(buf);
    let title = std::mem::take(chapter_title);
    let index = chapters.len();
    chapters.push(Chapter::inline(
        if title.is_empty() {
            format!("第 {} 节", index + 1)
        } else {
            title
        },
        // 包一层 body：排版层从 body 往下找正文，缺了它只能退到文档根。
        format!("<body>{source}</body>"),
    ));
}

/// 去掉标签与实体之后还有没有字，或者有没有一张图。
fn has_content(buf: &str) -> bool {
    let mut text = String::new();
    let mut in_tag = false;
    for ch in buf.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    !text.trim().is_empty() || buf.contains("<img")
}

fn escape_into(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
}

fn escape_attr(text: &str) -> String {
    text.replace('&', "&amp;").replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    //! FB2：元数据、章节切分、嵌套小节、内嵌 base64 图片。

    use std::path::Path;

    use super::Fb2Adapter;
    use crate::content::format::{Content, DocumentProvider, Format};

    fn png(width: u32, height: u32) -> Vec<u8> {
        use image::ImageEncoder as _;
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba([9, 9, 9, 255]));
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), width, height, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    fn b64(bytes: &[u8]) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    fn write_fb2(dir: &Path, name: &str, xml: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, xml).unwrap();
        path
    }

    fn flow(path: &Path) -> (crate::content::format::DocMeta, crate::content::format::FlowDoc) {
        let (meta, content) = Fb2Adapter.read(path).unwrap();
        let Content::Reflow(flow) = content else {
            panic!("这个容器该读成可重排");
        };
        (meta, flow)
    }

    const HEAD: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0" xmlns:l="http://www.w3.org/1999/xlink">
<description><title-info>
<book-title>测试之书</book-title>
<author><first-name>列夫</first-name><last-name>托尔斯泰</last-name></author>
<author><nickname>另一个作者</nickname></author>
</title-info></description>"#;

    #[test]
    fn it_claims_fb2_and_reads_the_metadata() {
        assert_eq!(Fb2Adapter.format(), Format::Fb2);
        assert!(Fb2Adapter.supports(Path::new("a.fb2")));
        assert!(Fb2Adapter.supports(Path::new("A.FB2")));
        assert!(!Fb2Adapter.supports(Path::new("a.epub")));

        let dir = tempfile::tempdir().unwrap();
        let path = write_fb2(
            dir.path(),
            "a.fb2",
            &format!("{HEAD}<body><section><p>正文</p></section></body></FictionBook>"),
        );
        let (meta, flow) = flow(&path);
        assert_eq!(meta.title, "测试之书");
        assert_eq!(meta.authors, vec!["列夫 托尔斯泰", "另一个作者"]);
        assert_eq!(meta.format, Format::Fb2);
        assert_eq!(flow.chapters.len(), 1);
    }

    #[test]
    fn each_top_level_section_is_a_chapter_and_titles_become_headings() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fb2(
            dir.path(),
            "a.fb2",
            &format!(
                "{HEAD}<body>\
                 <section><title><p>第一章</p></title><p>甲</p></section>\
                 <section><title><p>第二章</p></title><p>乙</p></section>\
                 </body></FictionBook>"
            ),
        );
        let (_, flow) = flow(&path);
        assert_eq!(flow.chapters.len(), 2);
        assert_eq!(flow.chapters[0].title, "第一章");
        assert!(flow.chapters[0].source().contains("<h1>第一章</h1>"));
        assert!(flow.chapters[0].source().contains("<p>甲</p>"));
        assert_eq!(flow.chapters[1].title, "第二章");
    }

    /// 嵌套 section 的内容并进父章，但它的标题降一级 —— 一层一章会把目录撑爆。
    #[test]
    fn a_nested_section_stays_in_its_parent_with_a_lower_heading() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fb2(
            dir.path(),
            "a.fb2",
            &format!(
                "{HEAD}<body><section>\
                 <title><p>卷一</p></title><p>开头</p>\
                 <section><title><p>小节</p></title><p>里面</p></section>\
                 </section></body></FictionBook>"
            ),
        );
        let (_, flow) = flow(&path);
        assert_eq!(flow.chapters.len(), 1, "嵌套小节不另起一章");
        let src = &flow.chapters[0].source();
        assert!(src.contains("<h1>卷一</h1>"), "{src}");
        assert!(src.contains("<h2>小节</h2>"), "{src}");
        assert!(src.contains("<p>里面</p>"), "{src}");
    }

    #[test]
    fn inline_markup_and_empty_lines_are_translated() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fb2(
            dir.path(),
            "a.fb2",
            &format!(
                "{HEAD}<body><section><p>前<emphasis>着重</emphasis>后</p>\
                 <empty-line/><p>下一段</p></section></body></FictionBook>"
            ),
        );
        let (_, flow) = flow(&path);
        let src = &flow.chapters[0].source();
        assert!(src.contains("前<em>着重</em>后"), "{src}");
        assert!(src.contains("<p>&#160;</p>"), "空行要留出空白：{src}");
    }

    /// 带 `name` 的 body（脚注）不进正文 —— 混进去会让每章后面多一堆注释。
    #[test]
    fn a_named_body_is_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fb2(
            dir.path(),
            "a.fb2",
            &format!(
                "{HEAD}<body><section><p>正文</p></section></body>\
                 <body name=\"notes\"><section><p>这是脚注不该出现</p></section></body>\
                 </FictionBook>"
            ),
        );
        let (_, flow) = flow(&path);
        assert_eq!(flow.chapters.len(), 1);
        assert!(!flow.chapters[0].source().contains("脚注"));
    }

    #[test]
    fn embedded_images_are_registered_and_decoded_on_demand() {
        let dir = tempfile::tempdir().unwrap();
        let image = png(30, 10);
        let path = write_fb2(
            dir.path(),
            "a.fb2",
            &format!(
                "{HEAD}<body><section><p>x</p></section></body>\
                 <binary id=\"pic.png\" content-type=\"image/png\">{}</binary>\
                 </FictionBook>",
                b64(&image)
            ),
        );
        let (_, flow) = flow(&path);
        assert_eq!(flow.images.len(), 1);
        assert_eq!(flow.images[0].href, "pic.png");
        assert_eq!(flow.images[0].media_type, "image/png");
        assert_eq!((flow.images[0].width, flow.images[0].height), (30, 10));

        let got = Fb2Adapter.image_bytes(&path, "pic.png").unwrap();
        assert_eq!(got, image, "按 id 现取要拿回原样的字节");
        assert!(Fb2Adapter.image_bytes(&path, "nope").is_none());
    }

    #[test]
    fn an_image_inside_a_section_becomes_an_img_tag_pointing_at_the_binary() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fb2(
            dir.path(),
            "a.fb2",
            &format!(
                "{HEAD}<body><section><image l:href=\"#pic.png\"/></section></body>\
                 <binary id=\"pic.png\" content-type=\"image/png\">{}</binary></FictionBook>",
                b64(&png(4, 4))
            ),
        );
        let (_, flow) = flow(&path);
        assert!(
            flow.chapters[0].source().contains("<img src=\"pic.png\"/>"),
            "{}",
            flow.chapters[0].source()
        );
    }

    /// 畸形 XML 不能 panic：读到哪算哪，与 epub 那条「坏一处不整本打不开」同一立场。
    #[test]
    fn malformed_xml_still_yields_what_it_can() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fb2(
            dir.path(),
            "a.fb2",
            "<FictionBook><description><title-info><book-title>半个书名</book-title></title-info></description>\
             <body><section><p>正文",
        );
        let (meta, flow) = flow(&path);
        assert_eq!(meta.title, "半个书名");
        assert_eq!(flow.chapters.len(), 1, "半截文件也要给出一章");
    }

    /// 书名缺失时退回文件名，别让书架上出现一本无名之书。
    #[test]
    fn a_book_without_a_title_falls_back_to_the_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fb2(
            dir.path(),
            "无题.fb2",
            "<FictionBook><body><section><p>x</p></section></body></FictionBook>",
        );
        let (meta, _) = flow(&path);
        assert_eq!(meta.title, "无题");
    }
}
