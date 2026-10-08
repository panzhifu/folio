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

use super::{Chapter, Content, DocMeta, FlowDoc, Format, FormatAdapter, TocEntry};
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

/// 把 `href` 相对 `base_dir`（OPF 所在目录）解析成容器内的规范路径。
/// 去掉 `#fragment`，做百分号解码，折叠 `./` 与 `../`。
fn resolve(base_dir: &str, href: &str) -> String {
    let href = href.split('#').next().unwrap_or(href).trim();
    let decoded = percent_decode(href);
    let merged = if decoded.starts_with('/') {
        decoded.trim_start_matches('/').to_string()
    } else if base_dir.is_empty() {
        decoded
    } else {
        format!("{base_dir}{decoded}")
    };
    let mut stack: Vec<&str> = Vec::new();
    for seg in merged.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    stack.join("/")
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(n) = u8::from_str_radix(&input[i + 1..i + 3], 16) {
                out.push(n);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    // href 按 UTF-8 百分号编码；解出来的字节流不一定落在字符边界上，
    // 那种情况保留替换字符而不是整本读失败。
    String::from_utf8_lossy(&out).into_owned()
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
    loop {
        let ev = match reader.read_event() {
            Ok(ev) => ev,
            Err(_) => break,
        };
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
                    ("metadata", "meta") => {
                        if attr(&e, "name").as_deref() == Some("cover") {
                            out.cover_id = attr(&e, "content");
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
    loop {
        let ev = match reader.read_event() {
            Ok(ev) => ev,
            Err(_) => break,
        };
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
                } else if name == "li" && in_toc > 0 {
                    if li_has_entry.pop().unwrap_or(false) {
                        if let Some(entry) = open.pop() {
                            match open.last_mut() {
                                Some(parent) => parent.children.push(entry),
                                None => roots.push(entry),
                            }
                        }
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
    loop {
        let ev = match reader.read_event() {
            Ok(ev) => ev,
            Err(_) => break,
        };
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
            .map(|n| normalize(n))
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
    };
    Ok((meta, Content::Reflow(flow)))
}

fn dir_of(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/"),
        None => String::new(),
    }
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
mod tests;
