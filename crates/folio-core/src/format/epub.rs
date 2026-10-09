//! EPUB 容器读取：`META-INF/container.xml` → OPF → spine / manifest / nav(NCX)。
//!
//! 一期只做到「把每章 XHTML 原文、全书 CSS、目录、封面拿出来」。XHTML 直接进
//! 我们自己的 `Dom`，不再转 Markdown —— 锚点坐标系是**我们存进去的那份源文本**，
//! 所以原样保存比转换更安全，也省掉一层会丢信息的中间态。
//!
//! 明确留在 M3 的事：`<img>` 的 src 不改写、图片不入库。排版层现在还不产出
//! 行内图片片段，先不碰这条路。

use std::io::{BufReader, Read};
use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::Event;
use zip::ZipArchive;

use super::{Chapter, Content, DocMeta, FlowDoc, Format, FormatAdapter, ImageRes, TocEntry};
use super::{dir_of, resolve_href};
use crate::error::{IoResultExt, Result};

pub struct EpubAdapter;

impl FormatAdapter for EpubAdapter {
    fn format(&self) -> Format {
        Format::Epub
    }

    fn supports(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("epub"))
    }

    fn read(&self, path: &Path) -> Result<(DocMeta, Content)> {
        read(path)
    }
}

#[derive(Debug, Default)]
struct Item {
    id: String,
    href: String,
    media_type: String,
    properties: Vec<String>,
}

#[derive(Debug, Default)]
struct Opf {
    title: String,
    authors: Vec<String>,
    items: Vec<Item>,
    spine: Vec<String>,
    /// `<meta name="cover" content="<item id>">`
    cover_id: Option<String>,
}

/// 取命名空间前缀之后的部分：`dc:title` → `title`，`epub:type` → `type`。
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

/// 文本事件里的内容，反转义后拿。
fn text_of(raw: &str) -> String {
    quick_xml::escape::unescape(raw)
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| raw.to_string())
}

/// 把 `href` 相对 `base_dir` 解析成容器内规范路径。实现收在 `format::resolve_href`，
/// 与 app 侧图片 resolver 共用同一份（见那里的说明）。
fn resolve(base_dir: &str, href: &str) -> String {
    resolve_href(base_dir, href)
}

fn container_path(xml: &str) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    loop {
        match reader.read_event().ok()? {
            Event::Start(e) | Event::Empty(e) => {
                let n = e.name();
                if local_name(n.as_ref()) == "rootfile" {
                    return attr(&e, "full-path").map(|p| p.replace('\\', "/"));
                }
            }
            Event::Eof => return None,
            _ => {}
        }
    }
}

fn parse_opf(xml: &str) -> Opf {
    let mut out = Opf::default();
    let mut reader = Reader::from_str(xml);
    let mut section = String::new();
    // 正在采集文本的元素（dc:title / dc:creator）。
    let mut pending: Option<(String, String)> = None;
    // 读不出下一个事件就到此为止（畸形 XML 与读完是同一档）：
    // 已经采到的部分照样有用，一本书因为某处编码坏了就整本打不开是不可接受的。
    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Start(e) => {
                let n = e.name();
                let name = local_name(n.as_ref());
                match name {
                    "metadata" | "manifest" | "spine" => section = name.to_string(),
                    "title" | "creator" => {
                        pending = Some((name.to_string(), String::new()));
                    }
                    _ => {}
                }
            }
            Event::Text(e) => {
                if let Some((_, buf)) = pending.as_mut() {
                    buf.push_str(&text_of(e.as_ref()));
                }
            }
            Event::End(e) => {
                let n = e.name();
                let name = local_name(n.as_ref());
                match name {
                    "title" | "creator" => {
                        if let Some((kind, buf)) = pending.take() {
                            let text = buf.trim().to_string();
                            if text.is_empty() {
                                continue;
                            }
                            if kind == "title" && out.title.is_empty() {
                                out.title = text;
                            } else {
                                out.authors.push(text);
                            }
                        }
                    }
                    "metadata" | "manifest" | "spine" => section = String::new(),
                    _ => {}
                }
            }
            Event::Empty(e) => {
                let n = e.name();
                let name = local_name(n.as_ref());
                match (section.as_str(), name) {
                    ("manifest", "item") => {
                        if let Some(item) = read_item(&e) {
                            out.items.push(item);
                        }
                    }
                    ("spine", "itemref") => {
                        if let Some(id) = attr(&e, "idref") {
                            out.spine.push(id);
                        }
                    }
                    ("metadata", "meta") if attr(&e, "name").as_deref() == Some("cover") => {
                        out.cover_id = attr(&e, "content");
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

fn read_item(e: &quick_xml::events::BytesStart<'_>) -> Option<Item> {
    Some(Item {
        id: attr(e, "id")?,
        href: attr(e, "href")?,
        media_type: attr(e, "media-type").unwrap_or_default(),
        properties: attr(e, "properties")
            .map(|p| p.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default(),
    })
}

/// nav 文档里 `epub:type="toc"` 那个 `<nav>` 的树。
///
/// 嵌套靠 `<li>` 栈算：`<a>` 结束时开一个条目，它所在 `<li>` 结束时条目才归位，
/// 期间关闭的子条目挂到它下面。没有 `<a>` 的纯容器 `<li>` 不会偷走子条目。
fn parse_nav(xml: &str) -> Vec<TocEntry> {
    let mut reader = Reader::from_str(xml);
    let mut roots: Vec<TocEntry> = Vec::new();
    // 等待归位的条目栈，栈顶是最近的祖先。
    let mut open: Vec<TocEntry> = Vec::new();
    // 每个未闭合 `<li>` 是否自己开过条目。
    let mut li_has_entry: Vec<bool> = Vec::new();
    let mut in_toc = 0usize;
    let mut link: Option<(String, String)> = None;
    // 读不出下一个事件就到此为止（畸形 XML 与读完是同一档）：
    // 已经采到的部分照样有用，一本书因为某处编码坏了就整本打不开是不可接受的。
    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Start(e) => {
                let n = e.name();
                let name = local_name(n.as_ref());
                if name == "nav" {
                    let ty = attr(&e, "type").unwrap_or_default();
                    let class = attr(&e, "class").unwrap_or_default();
                    let toc = ty
                        .split_whitespace()
                        .chain(class.split_whitespace())
                        .any(|t| t == "toc");
                    if toc {
                        in_toc += 1;
                    }
                } else if in_toc > 0 && name == "a" {
                    link = Some((attr(&e, "href").unwrap_or_default(), String::new()));
                } else if in_toc > 0 && name == "li" {
                    li_has_entry.push(false);
                }
            }
            Event::Text(e) => {
                if let Some((_, buf)) = link.as_mut() {
                    buf.push_str(&text_of(e.as_ref()));
                }
            }
            Event::End(e) => {
                let n = e.name();
                let name = local_name(n.as_ref());
                if name == "nav" {
                    in_toc = in_toc.saturating_sub(1);
                } else if name == "a" && in_toc > 0 {
                    if let Some((href, label)) = link.take() {
                        let title = label.split_whitespace().collect::<Vec<_>>().join(" ");
                        if !title.is_empty() {
                            if let Some(top) = li_has_entry.last_mut() {
                                *top = true;
                            }
                            open.push(TocEntry {
                                title,
                                href,
                                children: Vec::new(),
                            });
                        }
                    }
                } else if name == "li"
                    && in_toc > 0
                    && li_has_entry.pop().unwrap_or(false)
                    && let Some(entry) = open.pop()
                {
                    match open.last_mut() {
                        Some(parent) => parent.children.push(entry),
                        None => roots.push(entry),
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if roots.is_empty() {
        // li 结构不合常规时退化成平铺：目录宁可少层级也不要丢条目。
        return open;
    }
    roots
}

/// EPUB2 的 NCX。navMap 本身是树，但阅读器只用来跳章，这里拍平成一层。
///
/// NCX 是**大小写敏感**的 XML，元素名按规范写成 `navMap` / `navPoint` / `navLabel`，
/// 而我们要比对的是小写形式 —— 这里每个事件都过一次 `to_ascii_lowercase`。
/// nav 文档（XHTML）本来就全小写，不需要这一层。
fn parse_ncx(xml: &str) -> Vec<TocEntry> {
    let mut reader = Reader::from_str(xml);
    let mut out: Vec<TocEntry> = Vec::new();
    let mut label = String::new();
    let mut href = String::new();
    let mut in_text = false;
    let mut in_navmap = false;
    // 读不出下一个事件就到此为止（畸形 XML 与读完是同一档）：
    // 已经采到的部分照样有用，一本书因为某处编码坏了就整本打不开是不可接受的。
    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Start(e) => {
                let n = e.name();
                match local_name(n.as_ref()).to_ascii_lowercase().as_str() {
                    "navmap" => in_navmap = true,
                    "text" if in_navmap => in_text = true,
                    _ => {}
                }
            }
            Event::Text(e) if in_text => label.push_str(&text_of(e.as_ref())),
            Event::Empty(e) if in_navmap => {
                let n = e.name();
                if local_name(n.as_ref()).eq_ignore_ascii_case("content") {
                    href = attr(&e, "src").unwrap_or_default();
                }
            }
            Event::End(e) => {
                let n = e.name();
                let name = local_name(n.as_ref()).to_ascii_lowercase();
                if name == "text" {
                    in_text = false;
                } else if name == "navpoint" && in_navmap {
                    let title = label.trim().to_string();
                    if !title.is_empty() || !href.is_empty() {
                        out.push(TocEntry {
                            title,
                            href: std::mem::take(&mut href),
                            children: Vec::new(),
                        });
                    }
                    label = String::new();
                } else if name == "navmap" {
                    in_navmap = false;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

struct Zip {
    archive: ZipArchive<BufReader<std::fs::File>>,
}

impl Zip {
    fn open(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path).io_path(path)?;
        let archive = ZipArchive::new(BufReader::new(file)).io_path(path)?;
        Ok(Zip { archive })
    }

    fn read(&mut self, name: &str) -> Option<Vec<u8>> {
        let mut entry = self.archive.by_name(name).ok()?;
        if entry.is_dir() {
            return None;
        }
        let mut buf = Vec::with_capacity(entry.size().min(1 << 24) as usize);
        entry.read_to_end(&mut buf).ok().map(|_| buf)
    }

    fn read_text(&mut self, name: &str) -> Option<String> {
        self.read(name)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    /// 容器里的路径大小写与分隔符都可能和 OPF 写的不一致，做一次宽松匹配。
    fn locate(&self, want: &str) -> Option<String> {
        if self.archive.file_names().any(|n| normalize(n) == want) {
            return Some(want.to_string());
        }
        self.archive
            .file_names()
            .find(|n| normalize(n).eq_ignore_ascii_case(want))
            .map(normalize)
    }
}

fn normalize(name: &str) -> String {
    name.replace('\\', "/")
}

fn read(path: &Path) -> Result<(DocMeta, Content)> {
    let mut zip = Zip::open(path)?;
    let container = zip
        .locate("META-INF/container.xml")
        .and_then(|n| zip.read_text(&n))
        .ok_or_else(|| unsupported(path))?;
    let opf_path = container_path(&container).ok_or_else(|| unsupported(path))?;
    let opf_text = zip
        .read_text(&opf_path)
        .or_else(|| zip.locate(&opf_path).and_then(|n| zip.read_text(&n)))
        .ok_or_else(|| unsupported(path))?;
    let opf = parse_opf(&opf_text);
    let base_dir = match opf_path.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/"),
        None => String::new(),
    };

    let by_id = |id: &str| opf.items.iter().find(|i| i.id == id);

    // CSS 按 manifest 里的声明顺序拼接：真实书里后一份覆盖前一份是常态。
    let mut css = String::new();
    for item in opf.items.iter().filter(|i| i.media_type == "text/css") {
        let name = resolve(&base_dir, &item.href);
        if let Some(text) = zip.locate(&name).and_then(|n| zip.read_text(&n)) {
            css.push_str(&text);
            css.push('\n');
        }
    }

    // 目录：EPUB3 靠 manifest 的 `properties="nav"` 认（nav 文档常常不在 spine 里），
    // 没有 nav 就回落到 EPUB2 的 NCX。
    let nav_item = opf
        .items
        .iter()
        .find(|i| i.properties.iter().any(|p| p == "nav"));
    let ncx_item = opf.items.iter().find(|i| i.media_type.ends_with("ncx+xml"));
    let mut toc = Vec::new();
    for (item, is_nav) in nav_item
        .iter()
        .map(|i| (*i, true))
        .chain(ncx_item.iter().map(|i| (*i, false)))
    {
        let Some(found) = zip.locate(&resolve(&base_dir, &item.href)) else {
            continue;
        };
        let Some(text) = zip.read_text(&found) else {
            continue;
        };
        let dir = dir_of(&found);
        let entries = if is_nav {
            parse_nav(&text)
        } else {
            parse_ncx(&text)
        };
        toc = entries.into_iter().map(|e| rebase(e, &dir)).collect();
        if !toc.is_empty() {
            break;
        }
    }

    let mut cover = opf
        .cover_id
        .as_deref()
        .and_then(by_id)
        .map(|i| resolve(&base_dir, &i.href));
    if cover.is_none() {
        cover = opf
            .items
            .iter()
            .find(|i| i.properties.iter().any(|p| p == "cover-image"))
            .map(|i| resolve(&base_dir, &i.href));
    }
    let cover_bytes = cover
        .as_deref()
        .and_then(|n| zip.locate(n))
        .and_then(|n| zip.read(&n));

    // 全书图片：manifest 里所有 image/* 的 item。封面若在 manifest 里会一并被收录，
    // 排版层按解析后的容器路径查表，所以这里存的 href 必须是 `resolve` 过的。
    let mut images = Vec::new();
    for item in opf
        .items
        .iter()
        .filter(|i| i.media_type.starts_with("image/"))
    {
        let name = resolve(&base_dir, &item.href);
        let Some(bytes) = zip.locate(&name).and_then(|n| zip.read(&n)) else {
            continue;
        };
        let (width, height) = image_dims(&bytes);
        images.push(ImageRes {
            href: name,
            media_type: item.media_type.clone(),
            width,
            height,
            bytes,
        });
    }

    let mut chapters = Vec::new();
    for (idx, id) in opf.spine.iter().enumerate() {
        let Some(item) = by_id(id) else { continue };
        if !item.media_type.contains("html") || item.properties.iter().any(|p| p == "nav") {
            continue;
        }
        let name = resolve(&base_dir, &item.href);
        let Some(found) = zip.locate(&name).and_then(|n| zip.read_text(&n)) else {
            continue;
        };
        let source = found;
        let title = toc
            .iter()
            .find(|e| e.href == name)
            .map(|e| e.title.clone())
            .filter(|t| !t.is_empty())
            .or_else(|| heading_of(&source))
            .unwrap_or_else(|| format!("第 {} 节", idx + 1));
        chapters.push(Chapter {
            title,
            source,
            spine_href: name,
        });
    }

    let meta = DocMeta {
        title: if opf.title.is_empty() {
            path.file_stem()
                .map_or_else(String::new, |s| s.to_string_lossy().into_owned())
        } else {
            opf.title
        },
        authors: opf.authors,
        format: Format::Epub,
        size: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
    };
    let flow = FlowDoc {
        chapters,
        css,
        toc,
        cover: cover_bytes,
        images,
    };
    Ok((meta, Content::Reflow(flow)))
}

/// 图片固有像素尺寸。只读 header，不解码像素。SVG / 未知格式等取不到时返回 0，
/// 由排版层按容器的降级规则处理（用指定宽度或容器宽度兜底）。
fn image_dims(bytes: &[u8]) -> (u32, u32) {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok();
    reader
        .and_then(|r| r.into_dimensions().ok())
        .unwrap_or((0, 0))
}

/// nav 里的 href 相对 nav 文档自己，统一换成容器绝对路径。
fn rebase(entry: TocEntry, base: &str) -> TocEntry {
    let href = resolve(base, &entry.href);
    TocEntry {
        title: entry.title,
        href,
        children: entry
            .children
            .into_iter()
            .map(|c| rebase(c, base))
            .collect(),
    }
}

/// 章内第一个 h1/h2/h3 的纯文本，用现成的 DOM 取。
fn heading_of(source: &str) -> Option<String> {
    let dom = crate::dom::Dom::parse(source.as_bytes());
    let id = dom.elements().find(|(_, n)| {
        matches!(
            n.tag.as_ref().map(|t| t.local.to_string()).as_deref(),
            Some("h1") | Some("h2") | Some("h3")
        )
    })?;
    let text = dom
        .text_of_subtree(id.0)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!text.is_empty()).then_some(text)
}

fn unsupported(path: &Path) -> crate::Error {
    crate::Error::UnsupportedFormat(
        path.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
    )
}

#[cfg(test)]
mod tests {
    //! EPUB 容器读取。测的是一份**合成**的书（`tests/fixtures/oss/minimal-zh.epub`），
    //! 故意把 manifest 顺序、spine 顺序、文件名里的空格、nav 里的 fragment
    //! 都设成会出错的样子。只依赖公开的 `open()` / `resolve_href` 等。

    use std::path::{Path, PathBuf};

    use crate::css::computed::{ComputedStyle, TextAlign, cascade};
    use crate::css::{Origin, RuleSet};
    use crate::dom::Dom;
    use crate::format::detect;
    use crate::format::{Content, DocMeta, FlowDoc, Format, dir_of, resolve_href};
    use crate::open;

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/oss/minimal-zh.epub")
    }

    fn opened() -> (DocMeta, FlowDoc) {
        let (meta, content) = open(&fixture()).unwrap();
        // Content 目前只有 Reflow 一个分支，模式是不可反驳的。
        let Content::Reflow(flow) = content;
        (meta, flow)
    }

    #[test]
    fn detected_by_extension() {
        assert_eq!(detect(&fixture()), Some(Format::Epub));
        assert_eq!(detect(Path::new("/x/y.epub")), Some(Format::Epub));
        assert_eq!(detect(Path::new("/x/y.EPUB")), Some(Format::Epub));
        assert_eq!(detect(Path::new("/x/y.md")), Some(Format::Markdown));
    }

    #[test]
    fn href_resolution_collapses_dot_segments_and_decodes() {
        assert_eq!(
            resolve_href("OEBPS/Text/", "../Styles/style.css"),
            "OEBPS/Styles/style.css"
        );
        assert_eq!(
            resolve_href("OEBPS/", "./Text/chap1.xhtml#a"),
            "OEBPS/Text/chap1.xhtml"
        );
        assert_eq!(
            resolve_href("OEBPS/Text/", "chap%202.xhtml"),
            "OEBPS/Text/chap 2.xhtml"
        );
        assert_eq!(resolve_href("", "/OEBPS/a.xhtml"), "OEBPS/a.xhtml");
        // 逃出容器的 ../ 只会把栈弹空，不会造出绝对路径。
        assert_eq!(
            resolve_href("OEBPS/", "../../../../etc/passwd"),
            "etc/passwd"
        );
    }

    #[test]
    fn metadata_comes_from_the_opf() {
        let (meta, _) = opened();
        assert_eq!(meta.title, "合成测试书");
        assert_eq!(meta.authors, vec!["莫里亚克", "顾琪静"]);
        assert_eq!(meta.format, Format::Epub);
        assert!(meta.size > 0);
    }

    /// manifest 里 c3 排在最前，spine 才是阅读顺序。
    #[test]
    fn chapters_follow_spine_not_manifest() {
        let (_, flow) = opened();
        let hrefs: Vec<&str> = flow
            .chapters
            .iter()
            .map(|c| c.spine_href.as_str())
            .collect();
        assert_eq!(
            hrefs,
            vec![
                "OEBPS/Text/chap1.xhtml",
                "OEBPS/Text/chap 2.xhtml",
                "OEBPS/Text/chap3.xhtml"
            ]
        );
        assert_eq!(flow.chapters[0].title, "第一章 蛇结");
        assert!(flow.chapters[0].source.contains("夜色从花园那头压过来"));
        // nav 文档本身是 spine 里的一节，但它不该变成「一章」。
        assert_eq!(
            flow.chapters
                .iter()
                .filter(|c| c.spine_href.contains("Nav"))
                .count(),
            0
        );
    }

    /// 两份 CSS 都在 manifest 里，拼接顺序就是声明顺序。
    #[test]
    fn css_is_concatenated_in_manifest_order() {
        let (_, flow) = opened();
        let extra = flow.css.find("body.cover").expect("应有 cover.css 的内容");
        let style = flow.css.find("text-indent").expect("应有 style.css 的内容");
        assert!(
            extra < style,
            "cover.css 在 manifest 里先出现，拼接顺序要保住"
        );
    }

    #[test]
    fn toc_hrefs_are_absolute_and_labels_are_trimmed() {
        let (_, flow) = opened();
        assert_eq!(flow.toc.len(), 3, "nav 里三个条目");
        assert_eq!(flow.toc[0].href, "OEBPS/Text/chap1.xhtml");
        assert_eq!(
            flow.toc[1].href, "OEBPS/Text/chap 2.xhtml",
            "fragment 要去掉"
        );
        assert_eq!(
            flow.toc[2].title, "第三章 空白",
            "nav 标签里的多余空格要压掉"
        );
    }

    /// 中文书的排版地基：这三条样式必须能从合成书的 CSS 里落到元素上。
    #[test]
    fn the_books_own_css_reaches_the_elements() {
        let (_, flow) = opened();
        let rules = RuleSet::parse(&flow.css, Origin::Author);
        assert_eq!(rules.ignored, 1, "只有 @charset 是清单外的语句型 at-rule");
        let dom = Dom::parse(flow.chapters[0].source.as_bytes());
        let body = dom
            .elements()
            .find(|(_, n)| n.tag.as_ref().map(|t| t.local.to_string()) == Some("body".into()))
            .map(|(id, _)| id)
            .unwrap();
        let styles = cascade(
            &dom,
            &rules,
            &ComputedStyle {
                font_size: 18.0,
                ..Default::default()
            },
        );
        let p = dom
            .elements()
            .find(|(_, n)| n.tag.as_ref().map(|t| t.local.to_string()) == Some("p".into()))
            .map(|(id, _)| id)
            .unwrap();
        assert_eq!(
            styles.get(p).unwrap().text_indent,
            36.0,
            "2em 按 18px 偏好字号"
        );
        let h1 = dom
            .elements()
            .find(|(_, n)| n.tag.as_ref().map(|t| t.local.to_string()) == Some("h1".into()))
            .map(|(id, _)| id)
            .unwrap();
        // f32 乘法：18 × 1.8 = 32.399998，按位相等是测试的错而不是引擎的错。
        assert!(
            (styles.get(h1).unwrap().font_size - 32.4).abs() < 1e-3,
            "h1 = 1.8em"
        );
        assert_eq!(styles.get(h1).unwrap().text_align, TextAlign::Center);
        assert_eq!(
            styles.get(body).unwrap().font_family,
            "serif",
            "@charset 之后的第一条规则要命中 body"
        );
    }

    #[test]
    fn cover_bytes_survive_the_round_trip() {
        let (_, flow) = opened();
        let cover = flow.cover.expect("meta name=cover 指向的 PNG 应被读出来");
        assert_eq!(&cover[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn a_corrupt_container_fails_instead_of_returning_an_empty_book() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.epub");
        std::fs::write(&bad, b"not a zip at all").unwrap();
        assert!(
            open(&bad).is_err(),
            "读不出 container.xml 必须报错，不能给一本空书"
        );
    }

    /// 合成一张真实 PNG：`image` 只读 header 就能拿到尺寸，用真字节才测得准。
    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        use image::ImageEncoder as _;
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([10, 20, 30, 255]));
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    fn write_epub(path: &Path, entries: &[(&str, &[u8])]) {
        use std::io::Write as _;
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        for (name, data) in entries {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }

    const CONTAINER: &str = r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;

    /// 合成一份只有一章、没有 nav 的 EPUB（用来测标题回落）。
    fn synth_epub(dir: &tempfile::TempDir, chapter_xhtml: &str) -> PathBuf {
        let path = dir.path().join("synth.epub");
        let opf = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>无目录</dc:title></metadata>
  <manifest>
    <item id="c1" href="Text/chap1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="c1"/></spine>
</package>"#;
        write_epub(
            &path,
            &[
                ("mimetype", b"application/epub+zip"),
                ("META-INF/container.xml", CONTAINER.as_bytes()),
                ("OEBPS/content.opf", opf.as_bytes()),
                ("OEBPS/Text/chap1.xhtml", chapter_xhtml.as_bytes()),
            ],
        );
        path
    }

    /// 章标题的回落：没有目录条目时读章内第一个 h1/h2/h3。
    #[test]
    fn heading_fallback_reads_the_first_heading_of_a_chapter() {
        let dir = tempfile::tempdir().unwrap();
        let path = synth_epub(
            &dir,
            "<html><body><h2>第二章 庭院</h2><p>正文</p></body></html>",
        );
        let (_, content) = open(&path).unwrap();
        let Content::Reflow(flow) = content;
        assert_eq!(flow.chapters[0].title, "第二章 庭院");
    }

    /// 连标题都没有时，章名回落成「第 N 节」。
    #[test]
    fn a_chapter_without_a_heading_falls_back_to_a_numbered_title() {
        let dir = tempfile::tempdir().unwrap();
        let path = synth_epub(&dir, "<html><body><p>没有标题</p></body></html>");
        let (_, content) = open(&path).unwrap();
        let Content::Reflow(flow) = content;
        assert_eq!(flow.chapters[0].title, "第 1 节");
    }

    /// M3-E3：manifest 里的 `image/*` 要按容器路径读出、带上固有尺寸与字节。
    #[test]
    fn images_are_read_with_dimensions_and_resolved_hrefs() {
        let dir = tempfile::tempdir().unwrap();
        let epub = dir.path().join("with-image.epub");
        let png = png_bytes(3, 2);

        let opf = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>图册</dc:title></metadata>
  <manifest>
    <item id="c1" href="Text/chap1.xhtml" media-type="application/xhtml+xml"/>
    <item id="img1" href="Images/pic.png" media-type="image/png"/>
    <item id="nav" href="Nav/nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
  </manifest>
  <spine><itemref idref="c1"/></spine>
</package>"#;
        let chap = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p>文字<img src="../Images/pic.png" alt="图"/></p></body></html>"#;
        let nav = r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="../Text/chap1.xhtml">第一章</a></li></ol></nav></body></html>"#;

        write_epub(
            &epub,
            &[
                ("mimetype", b"application/epub+zip"),
                ("META-INF/container.xml", CONTAINER.as_bytes()),
                ("OEBPS/content.opf", opf.as_bytes()),
                ("OEBPS/Nav/nav.xhtml", nav.as_bytes()),
                ("OEBPS/Text/chap1.xhtml", chap.as_bytes()),
                ("OEBPS/Images/pic.png", &png),
            ],
        );

        let (_, content) = open(&epub).unwrap();
        let Content::Reflow(flow) = content;
        assert_eq!(flow.images.len(), 1, "只应收到那一张图");
        let img = &flow.images[0];
        assert_eq!(img.href, "OEBPS/Images/pic.png");
        assert_eq!(img.media_type, "image/png");
        assert_eq!((img.width, img.height), (3, 2), "固有尺寸来自解码 header");
        assert_eq!(&img.bytes[..8], b"\x89PNG\r\n\x1a\n");

        // 排版层解析 `src` 用的就是这一对函数，这里顺带锁死行为。
        assert_eq!(
            resolve_href(&dir_of("OEBPS/Text/chap1.xhtml"), "../Images/pic.png"),
            "OEBPS/Images/pic.png"
        );
    }

    /// 真实出版书的校验。默认 `--ignored`：CI 上没有这份受版权保护的文件，
    /// 一个「文件不在就算通过」的测试迟早会变成假绿灯。
    /// 本机跑法：`cargo test -p folio-core -- --ignored real_book`
    #[test]
    #[ignore = "需要 tests/fixtures/local 里的本机版权书，不入库"]
    fn real_book_opens_and_has_readable_chapters() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/local/shejie-mauriac.epub");
        let Ok(path) = path.canonicalize() else {
            panic!("没有本机测试书：{path:?}");
        };
        let (meta, content) = open(&path).unwrap();
        let Content::Reflow(flow) = content;
        eprintln!(
            "{} ｜ {} 章 ｜ CSS {} 字节 ｜ 目录 {} 条 ｜ 封面 {:?} ｜ 图片 {} 张",
            meta.title,
            flow.chapters.len(),
            flow.css.len(),
            flow.toc.len(),
            flow.cover.as_ref().map(|c| c.len()),
            flow.images.len()
        );
        assert!(flow.chapters.len() >= 5, "一本正常书不该只切出几章");
        assert!(
            flow.chapters
                .iter()
                .any(|c| c.source.contains("charset") || c.source.contains("<p")),
            "章节源文本里应有正文标记"
        );
        assert!(!flow.css.is_empty(), "出版书一定带 CSS");
        // 目录条目要能对上章节，否则侧栏点击无处可跳。
        let matched = flow
            .toc
            .iter()
            .filter(|e| flow.chapters.iter().any(|c| c.spine_href == e.href))
            .count();
        eprintln!("目录 {} 条，其中 {} 条对上 spine", flow.toc.len(), matched);
        assert!(
            matched * 2 >= flow.toc.len(),
            "过半目录条目对不上章节：href 解析有 bug"
        );
    }
}
