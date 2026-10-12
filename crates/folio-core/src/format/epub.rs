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
            ChapterLocator::Entry(entry) => {
                self.entry_text(path, entry)
                    .ok_or_else(|| crate::Error::ChapterUnavailable {
                        path: path.to_path_buf(),
                        index: 0,
                        at: entry.clone(),
                    })
            }
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
        // 正文这一趟**一定要解出来一次**，但只有「全文那一趟」才留下来。
        //
        // 留下来的是正文，丢掉的只是那份 `String`：字数（`chapters.char_len`）必须
        // 在索引这一趟算准 —— 它是全局字符进度的分母，而「算不出来就不要估」是硬要求
        // （`plans/导入管线.md` §八 第 3 条）。所以这里付的是**解压**的钱，
        // 省下的是「把正文搬进库里再丢掉」的钱。
        //
        // 注意别为了这一格改成按章回调 `chapter_source`：那会每章重开一次容器
        // （EPUB 无随机读，只能整本 inflate），于是导入从 O(章数) 掉成 O(章数²)。
        let text = zip.read_text(&name);
        let source = if with_text { text.clone() } else { None };
        let char_len = text.as_deref().and_then(super::visible_chars);
        let title = toc
            .iter()
            .find(|e| e.href == name)
            .map(|e| e.title.clone())
            .filter(|t| !t.is_empty())
            // `heading_of` 要读正文才拿得到。以前只有「全文那一趟」有正文，
            // 现在索引这一趟也解出来了，所以两种路径拿到的标题**一致**了 ——
            // 顺手修掉一处「同一本书索引说「第 3 节」、全文说真标题」的漂移。
            .or_else(|| text.as_deref().and_then(heading_of))
            .unwrap_or_else(|| format!("第 {} 节", idx + 1));
        chapters.push(
            match source {
                Some(text) => Chapter::inline(title, text).with_spine_href(name),
                // 位置就是条目名，正是打开时 `Entry` 那一路要的东西。
                None => Chapter::entry(title, name),
            }
            .with_char_len(char_len),
        );
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
        // 封面落盘是导入管线的事：provider 只交字节，不知道书库在哪儿。
        cover_ref: None,
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
mod tests;
