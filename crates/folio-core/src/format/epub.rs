//! EPUB 容器读取：`META-INF/container.xml` → OPF → spine / manifest / nav(NCX)。
//!
//! 一期只做到「把每章 XHTML 原文、全书 CSS、目录、封面拿出来」。XHTML 直接进
//! 我们自己的 `Dom`，不再转 Markdown —— 锚点坐标系是**我们存进去的那份源文本**，
//! 所以原样保存比转换更安全，也省掉一层会丢信息的中间态。
//!
//! 正文图片只存元数据（`href` + 固有尺寸），字节按容器路径现取，见
//! `Document::image_bytes` 那条路；封面例外，缩成缩略图后入库，见 [`COVER_MAX_EDGE`]。

use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::Event;

use super::zip::Zip;
use super::{
    Chapter, ChapterLocator, Content, DocMeta, DocumentProvider, FlowDoc, FontBytes, Format,
    ImageRes, TocEntry,
};
use super::{dir_of, fragment_of, image_dims, resolve_href, shrink_image};
use crate::error::Result;

pub struct EpubAdapter;

impl DocumentProvider for EpubAdapter {
    fn format(&self) -> Format {
        Format::Epub
    }

    fn supports(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("epub"))
    }

    /// 只读索引：**一个 zip 条目都不解压**（正文、封面、图片、CSS 除外 ——
    /// 那几样本来就要进库，见 `save_document`）。
    fn index(&self, path: &Path) -> Result<(DocMeta, Content)> {
        read_with(path, false)
    }

    /// 一章的正文：解它自己那一个条目。这是打开一本书时主要付的那一笔钱。
    fn chapter_source(&self, path: &Path, at: &ChapterLocator) -> Result<String> {
        match at {
            ChapterLocator::Entry(entry) => self.entry_text(path, entry).ok_or_else(|| {
                crate::Error::ChapterUnavailable {
                    path: path.to_path_buf(),
                    index: 0,
                    at: entry.clone(),
                }
            }),
            other => Err(crate::Error::ChapterUnavailable {
                path: path.to_path_buf(),
                index: 0,
                at: format!("{other:?}"),
            }),
        }
    }

    /// 全文那一趟（索引 + 每一章的正文）。只给测试与「我就是要全文」的调用方。
    fn read(&self, path: &Path) -> Result<(DocMeta, Content)> {
        read_with(path, true)
    }

    /// `@font-face` 声明的那几个路径 → 字节。回空而不是报错的理由见 [`FontBytes`]。
    fn fonts(&self, path: &Path, wanted: &[String]) -> Vec<FontBytes> {
        font_bytes(path, wanted)
    }

    /// 容器绝对路径 → 图片字节。每次调用重开容器，见 [`crate::format::ImageRes`]。
    fn image_bytes(&self, path: &Path, href: &str) -> Option<Vec<u8>> {
        let mut zip = Zip::open(path).ok()?;
        let name = zip.locate(href)?;
        zip.read(&name)
    }

    /// 容器绝对路径 → 那一章的 XHTML。与 `image_bytes` 同一条路：库里只有位置，
    /// 打开时现取（见 `ChapterLocator::Entry`）。
    fn entry_text(&self, path: &Path, entry: &str) -> Option<String> {
        let mut zip = Zip::open(path).ok()?;
        let name = zip.locate(entry)?;
        zip.read_text(&name)
    }
}

/// 单本书取字节的三道闸：face 数、单文件、总量。
///
/// 不是优化，是护栏 —— 一套完整的中文字体是 20 MB 量级，而内嵌字体在现在的定位里只
/// 用来**补缺字形**（见 `ui::reader::measure::gpui_font`）。读者不会为了几百个生僻字
/// 预付 30 MB 内存，超出的部分按声明顺序取，取满即停。
const MAX_FACES: usize = 32;
const MAX_ONE: usize = 8 << 20;
const MAX_TOTAL: usize = 32 << 20;

/// 封面的长边上限（**存储**尺寸，非显示尺寸）。
///
/// 书架卡片宽 100–200 逻辑像素、详情页也就 400 上下，512 留了高 dpi 与「点开看大图」
/// 的余量。以前是 1024 且「不超过就不动」，于是 800×1200 的 PNG 原样进库 ——
/// 实测一本《蛇结》封面 404 KB，占掉整个库的 8 成。正文出库之后封面就是库里最大的一项，
/// 这里按显示尺寸重编（见 [`shrink_image`]）。
const COVER_MAX_EDGE: u32 = 512;

/// 认得哪些 media-type 是字体，以及要不要先过一道 WOFF。
///
/// `woff2` **认得出但不取**：gpui 那侧的 fontdb 用 ttf-parser 解字节，只吃 TTF/OTF，
/// WOFF2 还要 brotli，一期不引这个依赖。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FontKind {
    Sfnt,
    Woff,
    Woff2,
    Other,
}

fn font_kind(media_type: &str) -> FontKind {
    // `application/x-font-otf`、`font/otf`、带参数的 `font/ttf;charset=utf-8` 都归到同一类。
    let mime = media_type
        .split(';')
        .next()
        .unwrap_or(media_type)
        .trim()
        .to_ascii_lowercase();
    match mime.as_str() {
        "font/ttf"
        | "font/otf"
        | "font/opentype"
        | "application/font-sfnt"
        | "application/vnd.ms-opentype"
        | "application/x-font-ttf"
        | "application/x-font-opentype"
        | "application/x-font-otf" => FontKind::Sfnt,
        "font/woff" | "application/font-woff" | "application/x-font-woff" => FontKind::Woff,
        "font/woff2" | "application/font-woff2" => FontKind::Woff2,
        _ => FontKind::Other,
    }
}

/// 容器路径 → 字体字节。
///
/// 与 [`read`] 走同一条定位链（container.xml → OPF → manifest），因为 `src` 里那个路径
/// 是**相对 OPF 目录**解出来的，只有再读一遍 manifest 才能把它对上 zip 里的条目名。
/// 这一步只在开书时做一次，几个小文件，不值当为它加一层缓存。
fn font_bytes(path: &Path, wanted: &[String]) -> Vec<FontBytes> {
    let mut zip = match Zip::open(path) {
        Ok(zip) => zip,
        Err(_) => return Vec::new(),
    };
    let Some(opf_path) = zip
        .locate("META-INF/container.xml")
        .and_then(|n| zip.read_text(&n))
        .and_then(|c| container_path(&c))
    else {
        return Vec::new();
    };
    let Some(opf_text) = zip
        .read_text(&opf_path)
        .or_else(|| zip.locate(&opf_path).and_then(|n| zip.read_text(&n)))
    else {
        return Vec::new();
    };
    let opf = parse_opf(&opf_text);
    let base_dir = dir_of(&opf_path);

    let mut out = Vec::new();
    let mut total = 0usize;
    for item in &opf.items {
        let kind = font_kind(&item.media_type);
        if matches!(kind, FontKind::Other | FontKind::Woff2) {
            continue;
        }
        // `src` 解出来的是容器绝对路径，`locate` 那一步已经把大小写与反斜杠抹平了，
        // 两边同形才能直接比。
        let name = resolve(&base_dir, &item.href);
        if !wanted.iter().any(|w| w == &name) {
            continue;
        }
        let Some(bytes) = zip.locate(&name).and_then(|n| zip.read(&n)) else {
            continue;
        };
        // 单个文件过大只丢它自己（后面那份可能是能用的）；face 数或总量到顶才整批停。
        if bytes.len() > MAX_ONE {
            tracing::debug!(
                "epub: 字体 {name} 有 {} 字节，超过单份上限，丢掉",
                bytes.len()
            );
            continue;
        }
        if out.len() >= MAX_FACES || total + bytes.len() > MAX_TOTAL {
            tracing::debug!(
                "epub: 内嵌字体到量（已取 {} 份 / {total} 字节），剩下的不读了",
                out.len()
            );
            break;
        }
        // WOFF 先解回 sfnt；解不开就当这个 item 不存在，继续找下一个声明的字体。
        let bytes = match kind {
            FontKind::Woff => crate::format::woff::decode(&bytes),
            _ => Some(bytes),
        };
        let Some(bytes) = bytes else {
            tracing::debug!("epub: 字体 {name} 声明是 WOFF，但解不回 sfnt");
            continue;
        };
        total += bytes.len();
        out.push(FontBytes {
            href: name,
            media_type: item.media_type.clone(),
            bytes,
        });
    }
    out
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
                                // 采下来的是原始 href；换成绝对路径时（`rebase`）才拆出 fragment。
                                fragment: None,
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
                            // 采下来的是原始 src；换成绝对路径时（`rebase`）才拆出 fragment。
                            fragment: None,
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

fn read_with(path: &Path, with_text: bool) -> Result<(DocMeta, Content)> {
    let mut zip = Zip::open(path)?;
    let container = zip
        .locate("META-INF/container.xml")
        .and_then(|n| zip.read_text(&n))
        .ok_or_else(|| unsupported(path, "容器里没有 META-INF/container.xml"))?;
    let opf_path = container_path(&container)
        .ok_or_else(|| unsupported(path, "container.xml 里没写出 OPF 路径"))?;
    let opf_text = zip
        .read_text(&opf_path)
        .or_else(|| zip.locate(&opf_path).and_then(|n| zip.read_text(&n)))
        .ok_or_else(|| unsupported(path, &format!("container.xml 指的 OPF「{opf_path}」读不到")))?;
    let opf = parse_opf(&opf_text);
    let base_dir = match opf_path.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/"),
        None => String::new(),
    };

    let by_id = |id: &str| opf.items.iter().find(|i| i.id == id);

    // CSS 按 manifest 里的声明顺序记下**位置清单**：正文出库之后样式表同样不入库，
    // 打开时按这份清单回容器现取（见 `Document::stylesheet`）。顺序 = manifest 顺序
    // = 层叠顺序，真实书里后一份覆盖前一份是常态。全文路径（`with_text`）才把字节
    // 读出来拼好 —— 那是测试与「我就是要全文」的调用方。
    let mut css_hrefs = Vec::new();
    let mut css = String::new();
    for item in opf.items.iter().filter(|i| i.media_type == "text/css") {
        let name = resolve(&base_dir, &item.href);
        // `locate` 返回容器里**真实**那条目名（大小写与分隔符都可能和 OPF 写的不一样），
        // 位置清单要按它记：按 OPF 那串解出来的 `url()` 会在书名大小写混用时指错地方。
        let Some(found) = zip.locate(&name) else {
            tracing::debug!("epub: manifest 声明的样式 {name} 不在容器里");
            continue;
        };
        css_hrefs.push(found.clone());
        if !with_text {
            continue;
        }
        let Some(text) = zip.read_text(&found) else {
            continue;
        };
        css.push_str(&super::absolutize_css_urls(&text, &dir_of(&found)));
        css.push('\n');
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
            tracing::debug!(
                "epub: 目录文档不在容器里（{} 声明的那份）",
                if is_nav { "nav" } else { "NCX" }
            );
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
    if toc.is_empty() {
        // 没目录只是侧栏少一栏，但它常常是「容器结构不合常规」的第一个信号。
        tracing::debug!("epub: nav 与 NCX 都没解出目录条目，这本书没有目录");
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
        .and_then(|n| zip.read(&n))
        // 封面是**故意**存下来的那一份：书架不打开容器就要画出那张卡片，所以它进
        // `books.cover`。但存的是缩略图不是原图 —— 对应 KOReader
        // 的 `bookinfo_cache.sqlite3` 里那份按显示尺寸缩完再 zstd 压的 blitbuffer。
        // 一张 1500×2200 的封面原图就有 400 KB，而书架上它只显示几十像素宽。
        .map(|bytes| match shrink_image(&bytes, COVER_MAX_EDGE) {
            Some((small, _mime)) => small,
            None => bytes,
        });

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
            // 插图缺失是按页降级的一种（排版会留一块占位），一条一张，所以放到 trace。
            tracing::trace!("epub: 插图 {name} 不在容器里");
            continue;
        };
        let (width, height) = image_dims(&bytes);
        images.push(ImageRes {
            href: name,
            media_type: item.media_type.clone(),
            width,
            height,
        });
    }

    let mut chapters = Vec::new();
    for (idx, id) in opf.spine.iter().enumerate() {
        let Some(item) = by_id(id) else {
            // spine 指着一个 manifest 里没有的 id：这一节整个没了，而且界面上看不出来。
            tracing::warn!(
                "epub: spine 第 {} 项指向的 id {id:?} 不在 manifest 里",
                idx + 1
            );
            continue;
        };
        if !item.media_type.contains("html") || item.properties.iter().any(|p| p == "nav") {
            continue;
        }
        let name = resolve(&base_dir, &item.href);
        // 索引那一趟**只核一下条目在不在**（`locate` 走中央目录，不解压），
        // 正文等打开这一章时再取 —— 这就是「导入不再解压全本」的全部。
        if zip.locate(&name).is_none() {
            // 正文少一章在界面上的表现是「翻到那里是空的」，说不清原因。
            tracing::warn!(
                "epub: 容器里没有 {name}（spine 第 {} 项），这一章读不到",
                idx + 1
            );
            continue;
        }
        let source = if with_text {
            zip.read_text(&name)
        } else {
            None
        };
        let title = toc
            .iter()
            .find(|e| e.href == name)
            .map(|e| e.title.clone())
            .filter(|t| !t.is_empty())
            // `heading_of` 要读正文才拿得到 —— 只有「全文那一趟」才试它。
            .or_else(|| source.as_deref().and_then(heading_of))
            .unwrap_or_else(|| format!("第 {} 节", idx + 1));
        chapters.push(match source {
            Some(text) => Chapter::inline(title, text).with_spine_href(name),
            // 位置就是条目名，正是打开时 `Entry` 那一路要的东西。
            None => Chapter::entry(title, name),
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
        css_hrefs,
        toc,
        cover: cover_bytes,
        images,
    };
    // 一本「读出来不对」的书，第一件要知道的事是哪一节空了：正文、样式、目录还是插图。
    tracing::debug!(
        "epub: {} 读到 {} 章（spine {} 项）、样式 {} 份（{} 字节）、目录 {} 项、插图 {} 张、封面 {}",
        path.display(),
        flow.chapters.len(),
        opf.spine.len(),
        flow.css_hrefs.len(),
        flow.css.len(),
        flow.toc.len(),
        flow.images.len(),
        flow.cover.as_ref().map_or("无", |_| "有"),
    );
    Ok((meta, Content::Reflow(flow)))
}

/// nav 里的 href 相对 nav 文档自己，统一换成容器绝对路径。
///
/// `#` 后面那一截要在 `resolve_href` 剥掉它之前先取下来存进 `fragment`：剥掉之后只剩
/// 「哪一章」，而 nav 里最常见的形态恰恰是同一个章文件挂着一串不同小节的锚点。
fn rebase(entry: TocEntry, base: &str) -> TocEntry {
    let fragment = fragment_of(&entry.href);
    let href = resolve(base, &entry.href);
    TocEntry {
        title: entry.title,
        href,
        fragment,
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

/// 容器读不出来的那几条路，收成一条错。
///
/// `why` 只进日志、不进错误文案：界面上给读者看的仍然是「不支持的格式」（那三件事在
/// 读者眼里是同一件事：这本书打不开），而开发者要的恰好是**哪一步**断了。
fn unsupported(path: &Path, why: &str) -> crate::Error {
    tracing::warn!("epub: {} 打不开 —— {why}", path.display());
    crate::Error::UnsupportedFormat(
        path.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
    )
}

#[cfg(test)]
mod tests {
    //! EPUB 容器读取。测的是一份**合成**的书（`tests/fixtures/oss/minimal-zh.epub`），
    //! 故意把 manifest 顺序、spine 顺序、文件名里的空格、nav 里的 fragment
    //! 都设成会出错的样子。只依赖公开的 `document::open` / `resolve_href` 等。

    use std::path::{Path, PathBuf};

    use super::read_with;
    use crate::css::computed::{ComputedStyle, TextAlign, cascade};
    use crate::css::{Origin, RuleSet};
    use crate::dom::Dom;
    use crate::format::detect;
    use crate::format::{Chapter, Content, DocMeta, FlowDoc, Format, dir_of, resolve_href};

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/oss/minimal-zh.epub")
    }

    /// 打开一个容器，返回**统一接口** —— 测试也走 `document::open` 这条 app 唯一的
    /// 「文件 → 文档」入口（`format` 那一层不再有 `open`）。
    fn doc_of(path: &Path) -> Box<dyn crate::document::Document> {
        crate::document::open(path).expect("合成容器应能打开")
    }

    /// 可重排的那一半：绝大多数用例只关心 `FlowDoc`，而且是**正文已经取回来**的那种。
    ///
    /// `document::open` 现在只给索引（库里就存这个，见 `plans/text-out-of-db.md`），
    /// 而这一层要断言的是正文本身，所以按位置把每一章取回来。
    fn flow_of(path: &Path) -> FlowDoc {
        let doc = doc_of(path);
        let mut flow = doc.flow().expect("这个容器该读成可重排").clone();
        for (ix, chapter) in flow.chapters.iter_mut().enumerate() {
            let text = doc.chapter_source(ix).expect("正文该取得到");
            *chapter = Chapter::inline(chapter.title.clone(), text)
                .with_spine_href(chapter.spine_href.clone());
        }
        flow
    }

    fn opened() -> (DocMeta, FlowDoc) {
        let doc = doc_of(&fixture());
        let flow = flow_of(&fixture());
        (doc.meta().clone(), flow)
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
        assert!(flow.chapters[0].source().contains("夜色从花园那头压过来"));
        // nav 文档本身是 spine 里的一节，但它不该变成「一章」。
        assert_eq!(
            flow.chapters
                .iter()
                .filter(|c| c.spine_href.contains("Nav"))
                .count(),
            0
        );
    }

    /// 两份 CSS 都在 manifest 里：索引记下的是**位置清单**，顺序就是声明顺序；
    /// 全文路径才把文本按同一顺序拼好（正文出库之后样式表同样不入库）。
    #[test]
    fn css_is_concatenated_in_manifest_order() {
        let (_, flow) = opened();
        assert!(flow.css.is_empty(), "索引只给位置清单，不该带样式文本");
        assert_eq!(flow.css_hrefs.len(), 2, "{:?}", flow.css_hrefs);
        assert!(
            flow.css_hrefs[0].ends_with("cover.css"),
            "清单按 manifest 顺序：{:?}",
            flow.css_hrefs
        );
        assert!(flow.css_hrefs[1].ends_with("style.css"), "{:?}", flow.css_hrefs);

        let (_, full) = read_with(&fixture(), true).unwrap();
        let Content::Reflow(full) = full else {
            panic!("该读成可重排");
        };
        let extra = full.css.find("body.cover").expect("应有 cover.css 的内容");
        let style = full.css.find("text-indent").expect("应有 style.css 的内容");
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
        // 去掉的同时它得留在 `fragment` 里：合成书第二条写的就是 `chap%202.xhtml#court`，
        // 一个章文件挂多个小节是 nav 最常见的形态，丢了这截它们会全并成同一个落点。
        assert_eq!(flow.toc[1].fragment.as_deref(), Some("court"));
        assert_eq!(flow.toc[0].fragment, None, "没有 # 的条目就是章首");
        assert_eq!(
            flow.toc[2].title, "第三章 空白",
            "nav 标签里的多余空格要压掉"
        );
    }

    /// 中文书的排版地基：这三条样式必须能从合成书的 CSS 里落到元素上。
    /// 层叠吃的是**拼好的文本** —— 全文路径（`read`）给的那一份。
    #[test]
    fn the_books_own_css_reaches_the_elements() {
        let (_, full) = read_with(&fixture(), true).unwrap();
        let Content::Reflow(flow) = full else {
            panic!("该读成可重排");
        };
        let rules = RuleSet::parse(&flow.css, Origin::Author);
        assert_eq!(rules.ignored, 1, "只有 @charset 是清单外的语句型 at-rule");
        let dom = Dom::parse(flow.chapters[0].source().as_bytes());
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
            crate::document::open(&bad).is_err(),
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

    /// 出版书形状的那一份：28 章、25 条目录、开头带 `@charset` 的一份 CSS，
    /// 导航只有 `toc.ncx`（EPUB 2.0，calibre 导出就是这一档）。
    ///
    /// 这些数字是在本机那本不入库的样本里量出来的（`tests/fixtures/README.md`），
    /// 但容器本身**当场合成** —— 一条要每次 `cargo test` 与 CI 都真跑的测试，
    /// 输入必须在仓库里。它验的不是「能不能打开」，而是合成小样本装不下的那几个形状：
    /// manifest 与 spine 反着排、几十条 NCX 条目、条目里的 `#fragment`、章数多于目录数。
    fn published_shape_epub(dir: &tempfile::TempDir) -> PathBuf {
        const CHAPTERS: usize = 28;
        const TOC_ENTRIES: usize = 25;
        let path = dir.path().join("published-shape.epub");

        // manifest 倒着写、ncx 与 css 垫底：出版书就是这么排的，而读成 spine 顺序才对。
        let mut manifest = String::new();
        for n in (1..=CHAPTERS).rev() {
            manifest.push_str(&format!(
                "    <item id=\"c{n}\" href=\"Text/chap{n}.xhtml\" media-type=\"application/xhtml+xml\"/>\n"
            ));
        }
        manifest.push_str(
            "    <item id=\"ncx\" href=\"toc.ncx\" media-type=\"application/x-dtbncx+xml\"/>\n",
        );
        manifest
            .push_str("    <item id=\"css\" href=\"Styles/style.css\" media-type=\"text/css\"/>\n");

        let mut spine = String::from("    <spine toc=\"ncx\">\n");
        for n in 1..=CHAPTERS {
            spine.push_str(&format!("      <itemref idref=\"c{n}\"/>\n"));
        }
        spine.push_str("    </spine>\n");

        let opf = format!(
            r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>合成出版书</dc:title>
    <dc:creator>某人</dc:creator>
  </metadata>
  <manifest>
{manifest}  </manifest>
{spine}</package>"#
        );

        // 元素名按规范的大小写（`navMap` / `navPoint`）：NCX 是大小写敏感的 XML，
        // 全小写的解析器在这份文件上会读出零条目录，而在合成小样本上不一定看得出来。
        // 头两条指着章内锚点，那是出版书目录把「章 + 小节」一起列出来时的写法。
        let mut nav_points = String::new();
        for n in 1..=TOC_ENTRIES {
            let src = match n {
                1 => "Text/chap1.xhtml#opening".to_string(),
                2 => "Text/chap2.xhtml#second".to_string(),
                _ => format!("Text/chap{n}.xhtml"),
            };
            nav_points.push_str(&format!(
                "    <navPoint id=\"p{n}\" playOrder=\"{n}\"><navLabel><text>第{n}章</text></navLabel><content src=\"{src}\"/></navPoint>\n"
            ));
        }
        let ncx = format!(
            r#"<?xml version="1.0"?>
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1">
  <head><meta name="dtb:uid" content="synth-published"/></head>
  <docTitle><text>合成出版书</text></docTitle>
  <navMap>
{nav_points}  </navMap>
</ncx>"#
        );

        // 开头那条 `@charset` 是出版书的标配；它曾是「把后一条规则整条吃掉」的现场
        // （解析侧的回归由 `css` 那一层的测试钉住，这里保住它出现在容器里）。
        let css = "@charset \"utf-8\";\nbody { line-height: 1.8 }\nh1 { text-align: center }\n";

        let mut entries: Vec<(String, Vec<u8>)> = vec![
            ("mimetype".to_string(), b"application/epub+zip".to_vec()),
            (
                "META-INF/container.xml".to_string(),
                CONTAINER.as_bytes().to_vec(),
            ),
            ("OEBPS/content.opf".to_string(), opf.into_bytes()),
            ("OEBPS/toc.ncx".to_string(), ncx.into_bytes()),
            (
                "OEBPS/Styles/style.css".to_string(),
                css.as_bytes().to_vec(),
            ),
        ];
        for n in 1..=CHAPTERS {
            // 前两章把目录点到的那两个锚放下来，其余章没有 —— 目录数少于章数是常态。
            let anchor = match n {
                1 => " id=\"opening\"",
                2 => " id=\"second\"",
                _ => "",
            };
            entries.push((
                format!("OEBPS/Text/chap{n}.xhtml"),
                format!(
                    "<html><head><title>第{n}章</title></head><body><h1>第{n}章</h1><p{anchor}>第{n}章的正文。</p></body></html>"
                )
                    .into_bytes(),
            ));
        }
        let refs: Vec<(&str, &[u8])> = entries
            .iter()
            .map(|(name, data)| (name.as_str(), data.as_slice()))
            .collect();
        write_epub(&path, &refs);
        path
    }

    /// 章标题的回落：目录里没有这一章时，**全文那一趟**读章内第一个 h1/h2/h3。
    ///
    /// 这一档是「只读索引」的代价：标题的回落要读正文才拿得到，而导入不再读正文
    /// （见 `plans/text-out-of-db.md`）。所以索引那一趟的标题只能来自目录，
    /// 目录里没有就是「第 N 节」。故意写成两条断言，把那个差别钉在那里。
    #[test]
    fn heading_fallback_only_exists_on_the_full_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = synth_epub(
            &dir,
            "<html><body><h2>第二章 庭院</h2><p>正文</p></body></html>",
        );
        // 索引：目录里没有 → 第 N 节（导入看到的，也是进库的那个）。
        let indexed = doc_of(&path);
        assert_eq!(
            indexed.flow().unwrap().chapters[0].title,
            "第 1 节",
            "索引那一趟读不到正文，也就无从回落"
        );
        // 全文：读得到正文，回落成立。
        let provider = crate::format::provider_for(&path).unwrap();
        let (_, content) = provider.read(&path).unwrap();
        let crate::format::Content::Reflow(flow) = content else {
            panic!("该读成可重排");
        };
        assert_eq!(flow.chapters[0].title, "第二章 庭院");
    }

    /// 导入那一趟（`index`）**不产出正文**：章表里每一章都是「位置」。
    ///
    /// 这条钉的是「导入不再解压全本」本身 —— 以前 `read()` 会在导入时把每一章的
    /// XHTML 都解出来，然后被 `save_flow_in` 丢掉。
    #[test]
    fn the_index_pass_produces_no_chapter_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = synth_epub(
            &dir,
            "<html><body><h2>标题</h2><p>正文</p></body></html>",
        );
        let provider = crate::format::provider_for(&path).unwrap();

        let (_, content) = provider.index(&path).unwrap();
        let crate::format::Content::Reflow(indexed) = content else {
            panic!("该读成可重排");
        };
        assert!(
            indexed.chapters.iter().all(|c| c.source().is_empty()),
            "索引那一趟不该有正文"
        );
        assert!(
            indexed
                .chapters
                .iter()
                .all(|c| matches!(c.locator(), Some(crate::format::ChapterLocator::Entry(_)))),
            "每一章都该只带一个容器条目"
        );

        let (_, content) = provider.read(&path).unwrap();
        let crate::format::Content::Reflow(full) = content else {
            panic!("该读成可重排");
        };
        assert!(
            full.chapters.iter().all(|c| !c.source().is_empty()),
            "全文那一趟才有正文"
        );
    }

    /// 连标题都没有时，章名回落成「第 N 节」。
    #[test]
    fn a_chapter_without_a_heading_falls_back_to_a_numbered_title() {
        let dir = tempfile::tempdir().unwrap();
        let path = synth_epub(&dir, "<html><body><p>没有标题</p></body></html>");
        let flow = flow_of(&path);
        assert_eq!(flow.chapters[0].title, "第 1 节");
    }

    /// M3-E3：manifest 里的 `image/*` 要按容器路径读出固有尺寸；字节**不入** `FlowDoc`，
    /// 而是按容器路径现取（学 KOReader）。
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

        let flow = flow_of(&epub);
        assert_eq!(flow.images.len(), 1, "只应收到那一张图");
        let img = &flow.images[0];
        assert_eq!(img.href, "OEBPS/Images/pic.png");
        assert_eq!(img.media_type, "image/png");
        assert_eq!((img.width, img.height), (3, 2), "固有尺寸来自解码 header");
        // 字节不在 FlowDoc 里，按容器路径现取 —— 这正是 KOReader 的
        // `getObjectImageStream()` 那条路。
        let bytes = doc_of(&epub)
            .image_bytes(&img.href)
            .expect("容器里那张图该能按 href 取回来");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(bytes, png, "取回来的就是写进去的那份字节");
        assert!(
            doc_of(&epub).image_bytes("OEBPS/Images/nope.png").is_none(),
            "容器里没有的路径要给 None，不是空 Vec"
        );

        // 排版层解析 `src` 用的就是这一对函数，这里顺带锁死行为。
        assert_eq!(
            resolve_href(&dir_of("OEBPS/Text/chap1.xhtml"), "../Images/pic.png"),
            "OEBPS/Images/pic.png"
        );
    }

    /// `@font-face` 的 `src` 相对的是**那份 CSS 自己的目录**，而 `FlowDoc.css` 是多份
    /// CSS 拼成的一个字符串（SQLite 里 `books.css` 那一列就是它）。拼接前不把 `url()`
    /// 解成容器绝对路径，读字节那一步就会拿 `Fonts/body.otf` 去要一个其实躺在
    /// `OEBPS/Styles/` 下面的文件 —— 表现是「书里明明有字体，就是读不到」，
    /// 而屏幕上一点痕迹都没有。
    #[test]
    fn css_urls_are_absolutized_before_concatenation() {
        let dir = tempfile::tempdir().unwrap();
        let epub = dir.path().join("with-font.epub");
        // 故意写成 `../Fonts/`：解错的目录是最常见的真实形态，也最容易被自己的测试放过去。
        let css = "@charset \"utf-8\";\n@font-face { font-family: \"Book Body\"; src: url(../Fonts/body.otf) format(\"opentype\"), local(\"Noto Serif\"); }\nbody { font-family: \"Book Body\", serif }\n";
        let opf = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>带字体</dc:title></metadata>
  <manifest>
    <item id="c1" href="Text/chap1.xhtml" media-type="application/xhtml+xml"/>
    <item id="css" href="Styles/style.css" media-type="text/css"/>
    <item id="f1" href="Fonts/body.otf" media-type="font/opentype"/>
  </manifest>
  <spine><itemref idref="c1"/></spine>
</package>"#;
        let fake = b"\x00\x01\x00\x00 fake sfnt";
        write_epub(
            &epub,
            &[
                ("mimetype", b"application/epub+zip"),
                ("META-INF/container.xml", CONTAINER.as_bytes()),
                ("OEBPS/content.opf", opf.as_bytes()),
                (
                    "OEBPS/Text/chap1.xhtml",
                    "<html><body><p>正文</p></body></html>".as_bytes(),
                ),
                ("OEBPS/Styles/style.css", css.as_bytes()),
                ("OEBPS/Fonts/body.otf", fake),
            ],
        );

        let (_, full) = read_with(&epub, true).unwrap();
        let Content::Reflow(flow) = full else {
            panic!("该读成可重排");
        };
        assert!(
            flow.css.contains("url(\"OEBPS/Fonts/body.otf\")"),
            "src 要解成容器绝对路径、带引号、括号配对：{}",
            flow.css
        );
        // 索引路径记下的是位置清单，文本一个字节都不读。
        let (_, indexed) = read_with(&epub, false).unwrap();
        let Content::Reflow(indexed) = indexed else {
            panic!("该读成可重排");
        };
        assert_eq!(indexed.css_hrefs, vec!["OEBPS/Styles/style.css".to_string()]);
        assert!(indexed.css.is_empty(), "索引不该带样式文本");

        // 层叠那一步拿到的就是能直接查容器的路径。
        let rules = RuleSet::parse(&flow.css, Origin::Author);
        assert_eq!(rules.font_faces.len(), 1, "@font-face 该留下来");
        assert_eq!(rules.font_faces[0].family, "Book Body");
        assert_eq!(
            rules.font_faces[0].src,
            vec![
                crate::css::FontSrc::Url("OEBPS/Fonts/body.otf".into()),
                crate::css::FontSrc::Local("Noto Serif".into()),
            ]
        );

        // 正文那条 `font-family` 的第一候选就是声明名 —— app 侧靠整串同名把它认出来。
        let dom = Dom::parse(flow.chapters[0].source().as_bytes());
        let body = dom
            .elements()
            .find(|(_, n)| n.tag.as_ref().map(|t| t.local.to_string()) == Some("body".into()))
            .map(|(id, _)| id)
            .unwrap();
        let styles = cascade(&dom, &rules, &ComputedStyle::default());
        assert_eq!(styles.get(body).unwrap().font_family, "Book Body");
        assert!(crate::css::font_face_matches(
            &styles.get(body).unwrap().font_family,
            &rules.font_faces[0].family
        ));

        // 取字节：声明过的路径给字节，没声明的不给。
        let got = doc_of(&epub).fonts(&[
            "OEBPS/Fonts/body.otf".to_string(),
            "OEBPS/Fonts/nope.otf".to_string(),
        ]);
        assert_eq!(got.len(), 1, "只取 wanted 里真在 manifest 的那一份");
        assert_eq!(got[0].href, "OEBPS/Fonts/body.otf");
        assert_eq!(got[0].bytes, fake.to_vec());
        assert!(
            doc_of(&epub)
                .fonts(&["OEBPS/Fonts/nope.otf".to_string()])
                .is_empty(),
            "声明了一个不存在的路径不该读到别的东西"
        );
    }

    /// WOFF2 认得出但不取（gpui 那侧的 ttf-parser 不吃，一期不引 brotli）；
    /// 单个文件过大只丢它自己，后面那份正常的还得照常拿到。
    #[test]
    fn woff2_is_skipped_and_an_oversized_font_only_costs_itself() {
        let dir = tempfile::tempdir().unwrap();
        let epub = dir.path().join("odd-fonts.epub");
        let opf = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>怪字体</dc:title></metadata>
  <manifest>
    <item id="c1" href="Text/chap1.xhtml" media-type="application/xhtml+xml"/>
    <item id="big" href="Fonts/big.otf" media-type="font/opentype"/>
    <item id="w2" href="Fonts/x.woff2" media-type="font/woff2"/>
    <item id="ok" href="Fonts/ok.otf" media-type="font/opentype"/>
  </manifest>
  <spine><itemref idref="c1"/></spine>
</package>"#;
        let huge = vec![0u8; super::MAX_ONE + 1];
        write_epub(
            &epub,
            &[
                ("mimetype", b"application/epub+zip"),
                ("META-INF/container.xml", CONTAINER.as_bytes()),
                ("OEBPS/content.opf", opf.as_bytes()),
                (
                    "OEBPS/Text/chap1.xhtml",
                    "<html><body><p>正文</p></body></html>".as_bytes(),
                ),
                ("OEBPS/Fonts/big.otf", &huge),
                ("OEBPS/Fonts/x.woff2", b"wOF2 whatever"),
                ("OEBPS/Fonts/ok.otf", b"\x00\x01\x00\x00 ok"),
            ],
        );
        let wanted = [
            "OEBPS/Fonts/big.otf",
            "OEBPS/Fonts/x.woff2",
            "OEBPS/Fonts/ok.otf",
        ]
        .map(String::from)
        .to_vec();
        let got = doc_of(&epub).fonts(&wanted);
        assert_eq!(
            got.iter().map(|f| f.href.as_str()).collect::<Vec<_>>(),
            ["OEBPS/Fonts/ok.otf"],
            "过大那份与 woff2 都不该出现，且不能把排在它们后面的那份也带走"
        );
    }

    /// 一本整书规模的容器读出来是什么样子：章数、正文、CSS、目录能不能对上 spine。
    ///
    /// 合成书里三章的小样本测的是**单个结构**，这一条测的是**规模**：几十条目录、
    /// 目录数少于章数、manifest 与 spine 反序、`@charset` 打头的一份 CSS。
    /// 输入是 `published_shape_epub` 当场写进临时目录的那一份，不读任何本机文件，
    /// 所以它跟着默认套件与 CI 每次真跑 —— 一条需要「某人手上那本书」的测试做不到这点，
    /// 而把它改成「文件不在就算通过」又是一盏永远绿的假灯（见 `tests/fixtures/README.md`）。
    #[test]
    fn a_whole_published_shaped_book_reads_back_chapters_css_and_toc() {
        let dir = tempfile::tempdir().unwrap();
        let path = published_shape_epub(&dir);
        let doc = doc_of(&path);
        let meta = doc.meta();
        // 正文要按位置取回来（索引里只有位置）。
        let flow = flow_of(&path);

        assert_eq!(meta.title, "合成出版书", "书名从 OPF 的 dc:title 来");
        assert_eq!(
            flow.chapters.len(),
            28,
            "一章都不该在 spine 到源文件的路上丢掉"
        );
        assert!(
            flow.chapters
                .iter()
                .all(|c| c.source().contains("<p") && c.source().contains("</p>")),
            "每一章都要带得回它的正文"
        );
        assert!(
            !flow.css_hrefs.is_empty(),
            "出版书一定带 CSS（清单上至少一份）"
        );
        // 目录条目要能对上章节，否则侧栏点击无处可跳。带着 `#fragment` 的那两条也算：
        // 对上说明 fragment 在解 href 时被剥掉了，而不是留在路径里比对不上。
        let matched = flow
            .toc
            .iter()
            .filter(|e| flow.chapters.iter().any(|c| c.spine_href == e.href))
            .count();
        assert_eq!(
            matched,
            flow.toc.len(),
            "有目录条目对不上章节：href 解析有 bug"
        );
        // 标题：目录里有的那条用它。
        assert_eq!(flow.chapters[0].title, "第1章", "章名来自目录");
        // 目录排到 25 之后的那几章：**导入看到的**是「第 N 节」（索引那一趟不读正文，
        // 回落无从谈起），**全文那一趟**才读得到章内的 `<h1>`。这个差别是
        // 「导入不再解压全本」的代价，故意两条都钉住。
        assert_eq!(flow.chapters[27].title, "第 28 节", "索引那一趟只能给编号");
        let provider = crate::format::provider_for(&path).unwrap();
        let (_, content) = provider.read(&path).unwrap();
        let crate::format::Content::Reflow(full) = content else {
            panic!("该读成可重排");
        };
        assert_eq!(full.chapters[27].title, "第28章", "全文那一趟回落到 h1");
    }
}
