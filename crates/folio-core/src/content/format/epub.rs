//! EPUB 容器读取：`META-INF/container.xml` → OPF → spine / manifest / nav(NCX)。
//!
//! 一期只做到「把每章 XHTML 原文、全书 CSS、目录、封面拿出来」。XHTML 直接进
//! 我们自己的 `Dom`，不再转 Markdown —— 锚点坐标系是**我们存进去的那份源文本**，
//! 所以原样保存比转换更安全，也省掉一层会丢信息的中间态。
//!
//! 正文图片只存元数据（`href` + 固有尺寸），字节按容器路径现取，见
//! `Document::image_bytes` 那条路；封面例外，缩成缩略图后入库，见 [`COVER_MAX_EDGE`]。

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use quick_xml::Reader;
use quick_xml::events::Event;

use super::zip::{EntryRange, EntryReader, Zip};
use super::{
    Chapter, ChapterLocator, Content, DocMeta, DocumentProvider, FlowDoc, FontBytes, Format,
    ImageRes, ParseBudget, TocEntry,
};
use super::{
    HeadDims, dir_of, fragment_of, image_dims, image_dims_from_head, resolve_href, shrink_image,
};
use crate::infra::error::Result;

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

    /// 同上，但这一本内部起几路**由调用方分**（[`ParseBudget`]）。
    ///
    /// 为什么单开一条：一批同时导 M 本、每本内部 I 路，真正抢核的是 M × I，而 `index`
    /// 那一路的 I 只能按整机核数猜（[`parse_worker_count`]）—— 外层每多一本就把总量再乘
    /// 一份。分这个数的是 `ingest::import::split_budget`，因为只有它知道有几个兄弟在飞。
    fn index_with(&self, path: &Path, budget: ParseBudget) -> Result<(DocMeta, Content)> {
        read_with_impl(path, false, budget)
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

    /// 容器绝对路径 → 图片字节。每次调用重开容器，见 [`crate::content::format::ImageRes`]。
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

/// 一张插图为了定尺寸最多解出多少字节。
///
/// 尺寸头都在解压流最前面，而且要的不多：PNG 从第 48 字节起就定得出尺寸（IHDR 在
/// 第 8-33 字节，解码器要多读几字节才肯起步），GIF/BMP/WebP 在第一个块里，JPEG 要走过
/// APPn/COM 那些段才到 SOFn。实测 259 张（两本真书 + 三本合成漫画）里 32 KiB 全部够，
/// 一张都没掉进整条兜底那一路。
///
/// 为什么不干脆不兜底：带大 EXIF 缩略图或长注释段的 JPEG 会把 SOFn 推到更后面，
/// 那种图给个 0×0 就是版面按容器宽度硬拉。宁可多读一张，不多错一张。
const IMAGE_HEAD_BYTES: u64 = 32 * 1024;

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
            FontKind::Woff => crate::content::format::woff::decode(&bytes),
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

/// 一份书内索引解析作业的坐标。
///
/// 章节与图片共用一条工作队列，worker 动态认领：章长和图片大小分布不均，
/// 静态分片会让最长的那个章节决定整本书的墙钟。
#[derive(Debug, Clone)]
struct ChapterJob {
    /// 这一章在 spine 里的**原始**下标：标题回落的「第 N 节」与结果排序都用它。
    /// 不能拿过滤后的下标 —— spine 里被跳过的 nav 不该改变后面章节的编号。
    spine: usize,
    /// 解析后的容器绝对路径，进 `Chapter::spine_href`，也是目录比对键。
    href: String,
    /// 该章正文在容器里的压缩区间（主线程已经按名字定位过）。
    range: EntryRange,
}

#[derive(Debug, Clone)]
struct ImageJob {
    /// 解析后的容器绝对路径，进 `ImageRes::href`。
    href: String,
    /// 这张图在容器里的压缩区间（主线程已经按名字定位过）。
    range: EntryRange,
    media_type: String,
}

/// 封面那件活：读要用区间，记要用名字。
///
/// `href` 不参与取字节，只给日志与那条「封面在哪个线程上做的」守卫用 —— 守卫要能认出
/// 这条记录出自哪张封面，不然并行跑测试时别的书的封面会把它搅浑。
#[derive(Debug, Clone, Copy)]
struct Cover<'a> {
    href: &'a str,
    range: EntryRange,
}

/// 队列里的一件活。**封面排在最前**：它是单本书里最长的一件（实测缩一张 1535×2200
/// 的封面要 44-90 ms，比那本书其余条目加起来还长），排第一个被认领，才谈得上被其余
/// 条目的时间盖住。章排在图前面只是因为 chapter 条目普遍更长（同一条动态队列的道理）。
#[derive(Debug, Clone, Copy)]
enum Job {
    Cover,
    Chapter(usize),
    Image(usize),
}

#[derive(Default)]
struct ParsedEntries {
    /// `(job 下标, 产物)`：worker 按动态队列领活，完成顺序是乱的，收齐后再排回去。
    chapters: Vec<(usize, Chapter)>,
    images: Vec<(usize, ImageRes)>,
    /// 封面那一件的产物。队列里最多一个 `Job::Cover`，且每个下标只会被一个 worker 领走，
    /// 所以这里不会有两个 worker 各写一份。
    cover: Option<Vec<u8>>,
}

/// 一本书索引里最多并行几路。
///
/// 上限取 4：手机端真正可用的大核通常就 2–4 个，桌面几十核也没有必要把一本书铺满；
/// 再高只会和绘制、其它后台作业抢核。条目少时不起线程 —— 线程创建/收拢与每个
/// worker 多开一次容器的成本会盖过并行收益。
///
/// `pub(crate)` 是给 `ingest::import::split_budget` 看的：一批同时导几本 × 每本几路
/// 要**一次算完**，那个上限必须只有一份，不然两层又各猜各的。
pub(crate) const MAX_PARSE_WORKERS: usize = 4;
const MIN_PARALLEL_ITEMS: usize = 16;

/// 这一本起几路：**条目数、这台机器的并行度、外层分下来的上限**三者取最小。
///
/// `budget` 是**上限不是配额**：少条目那条守卫（[`MIN_PARALLEL_ITEMS`]）照旧生效 —— 否则
/// 一本 5 章的小书在批量导入时会为 5 件活开 4 条线程，而那正是这条守卫当初存在的理由
/// （线程创建/收拢与每路一份读缓冲，都比省下的时间贵）。
fn parse_worker_count(items: usize, budget: ParseBudget) -> usize {
    // `Exactly` 是测试与基准的口，绕过条目数那道门；`AtMost` 与 `Auto` 都不绕。
    if !matches!(budget, ParseBudget::Exactly(_)) && items < MIN_PARALLEL_ITEMS {
        return 1;
    }
    let auto = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, MAX_PARSE_WORKERS);
    match budget {
        ParseBudget::Auto => auto,
        // 上限只往下压，且至少留 1 路。
        ParseBudget::AtMost(n) => auto.min(n.max(1)),
        ParseBudget::Exactly(n) => n.clamp(1, MAX_PARSE_WORKERS),
    }
    .min(items.max(1))
}

/// 目录按 `href` 建的查找表。每解一章都要拿这一章的容器路径回目录取一次标题，
/// 逐条 `iter().find` 在 1225 项目录的书上是每章一遍全表。
///
/// 只收**顶层**条目 —— 与换表之前那次 `find` 同一套语义（子条目不参与标题回落），
/// 换的是查找方式，不是内容。重复 href 保留第一个，也是 `find` 的语义。
type TocIndex<'a> = HashMap<&'a str, &'a TocEntry>;

fn toc_index(toc: &[TocEntry]) -> TocIndex<'_> {
    let mut index: TocIndex = HashMap::with_capacity(toc.len());
    for entry in toc {
        index.entry(entry.href.as_str()).or_insert(entry);
    }
    index
}

/// 一批作业共享的只读输入。
///
/// 收成一份不是为了少打几个字：加一类活（封面就是这次加的）不该再动三处函数签名，
/// 而 worker 拿到它就齐了。参数堆到第八个，读的人也得跟着数。
struct ParseCtx<'a> {
    /// 队列：下标就是认领顺序，产物按它排回原序。
    jobs: &'a [Job],
    chapters: &'a [ChapterJob],
    images: &'a [ImageJob],
    /// 封面那件活。`None` = 这本书没有可定位的封面，队列里也就没有 `Job::Cover`。
    cover: Option<Cover<'a>>,
    toc: &'a TocIndex<'a>,
}

/// 一个 worker 处理条目直到队列见底。
///
/// worker 拿的是 [`EntryReader`] —— 自己的文件句柄，但**不**再各自解析中央目录：
/// 「名字 → 区间」由主线程做一次，交下来的 [`EntryRange`] 只有偏移和长度。一份中央目录
/// 在 1233 条目的书上是 1.35 ms，四路并行就是四份这样的钱，而 worker 从来不需要名字。
fn parse_entries(
    reader: &mut EntryReader,
    ctx: &ParseCtx<'_>,
    with_text: bool,
    next: &AtomicUsize,
) -> ParsedEntries {
    let mut out = ParsedEntries {
        chapters: Vec::with_capacity(ctx.chapters.len()),
        images: Vec::with_capacity(ctx.images.len()),
        cover: None,
    };
    while let Some(at) = ctx.jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
        match *at {
            // `Job::Cover` 只在定位得到封面时才进队列，所以这里给得出区间。
            Job::Cover => {
                out.cover = parse_cover(reader, ctx.cover.expect("队列里有 Cover 就该有活"))
            }
            Job::Chapter(n) => {
                out.chapters.push((
                    n,
                    parse_chapter(reader, &ctx.chapters[n], ctx.toc, with_text),
                ));
            }
            Job::Image(n) => {
                if let Some(image) = parse_image(reader, &ctx.images[n]) {
                    out.images.push((n, image));
                }
            }
        }
    }
    out
}

// 本线程最近一次定下的解析路数。**只在测试下存在**。
//
// 要守的是一条**接线**：[`ParseBudget::AtMost`] 从导入那边传下来，要是半路被丢掉（退回
// `Auto`），读出来的书一个字都不差、时间也差不多 —— 没有任何产物会不对劲。只有这个数
// 能揭穿「预算没生效」，而它正是「两层共用一份预算」那件事的全部落点。
//
// 线程局部而不是全局表：`cargo test` 并行跑，全局那一份会把**别的测试**开的书混进来
// （`COVER_WORK` 那条为这件事踩过一次，只能靠一个本测试独有的条目名筛）。这个数是在
// 调 `read_with_impl` 的那条线程上定的，所以线程局部刚好圈得住。
#[cfg(test)]

thread_local! {
    static LAST_WORKERS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// 读走本线程最近一次定的路数（没定过是 0）。
#[cfg(test)]
pub(crate) fn last_workers() -> usize {
    LAST_WORKERS.with(|c| c.get())
}

/// 封面被缩了几次、各在哪个线程上，按**条目名**记。**只在测试下存在**。
///
/// 这一层对封面的承诺是「一本书一次，而且在 worker 上做」，那就用线程号加次数来守，
/// 不用计时（并行段的计时被调度噪声牵着走，见 `infra::metrics` 那段）。
///
/// 为什么是全局加条目名而不是线程局部：要看的恰恰是**别的线程**上发生过什么，线程局部
/// 读不到 worker；而 cargo 并行跑测试，全局量必须按一个本测试独有的条目名筛出来，
/// 否则别的书（都有封面）会把计数混进来 —— 这条一开始就是这么红的。
#[cfg(test)]
static COVER_WORK: std::sync::Mutex<Vec<(String, std::thread::ThreadId)>> =
    std::sync::Mutex::new(Vec::new());

/// 封面那一件：**整条读出来再按显示尺寸缩过重编**，跟插图不一样 —— 插图只要尺寸头，
/// 封面要的恰恰是那堆像素本身。
///
/// 它排在队列最前面而不是留在主线程：一张 1535×2200 的封面从读到缩要 44-90 ms，
/// 实测比一本书其余条目加起来还长，留在调用线程上就是把这一段整个摊进墙钟，
/// 而后面的 worker 们正闲着。
///
/// 存的是缩略图不是原图 —— 对应 KOReader 的 `bookinfo_cache.sqlite3` 里那份按显示尺寸
/// 缩完再 zstd 压的 blitbuffer。书架不打开容器就要画出那张卡片，所以它进 `books.cover`，
/// 而一张原图 400 KB 的封面在书架上只显示几十像素宽。
fn parse_cover(reader: &mut EntryReader, cover: Cover<'_>) -> Option<Vec<u8>> {
    #[cfg(test)]
    COVER_WORK
        .lock()
        .unwrap()
        .push((cover.href.to_string(), std::thread::current().id()));
    let Some(bytes) = reader.read(&cover.range) else {
        tracing::warn!(
            "epub: 封面条目 {} 读不出来，这本书按没有封面入库",
            cover.href
        );
        return None;
    };
    Some(match shrink_image(&bytes, COVER_MAX_EDGE) {
        Some((small, _mime)) => small,
        None => bytes,
    })
}

/// 一张插图进清单：只要固有像素尺寸，所以**只解前缀**，不整条解压。
///
/// 三档结局各自的去向（见 [`super::HeadDims`]）：前缀里就定出尺寸 → 完事；认不出是
/// 解得动的位图（SVG 那一类，读完整条也是 `(0, 0)`）→ 到此为止，不再花一趟解压；
/// 认得出是位图但尺寸头在更后面 → 才补一次整条读。
///
/// 一条与旧写法的差别是**好的**那种：整条读会因为尾部坏掉而整条失败（zip 在流末尾校
/// CRC），于是这张图从清单里消失、版面不知道它占多大；前缀读只要头部完好就定得出尺寸。
fn parse_image(reader: &mut EntryReader, job: &ImageJob) -> Option<ImageRes> {
    let head = reader.read_head(&job.range, IMAGE_HEAD_BYTES)?;
    let (width, height) = match image_dims_from_head(&head) {
        HeadDims::Sized(dims) => dims,
        HeadDims::NotABitmap => (0, 0),
        HeadDims::NeedsMore => match reader.read(&job.range) {
            Some(bytes) => image_dims(&bytes),
            // 整条读不出来（尾部损坏、容器被改过）：留住前缀那段已经读到的判断，
            // 也就是「这张图存在，但尺寸不知道」，比整张从清单里消失好。
            None => (0, 0),
        },
    };
    Some(ImageRes {
        href: job.href.clone(),
        media_type: job.media_type.clone(),
        width,
        height,
    })
}

fn parse_chapter(
    reader: &mut EntryReader,
    job: &ChapterJob,
    toc: &TocIndex<'_>,
    with_text: bool,
) -> Chapter {
    // 正文这一趟一定要解出来一次：`char_len` 是全局字符进度的分母，必须在导入时
    // 算准（`plans/导入管线.md` §八第 3 条）。留下来与否只看 `with_text`。
    // 两条读法按「正文本身是不是产物」分开：
    // - `with_text`：整条读出来，正文与两样数都在那份字符串上拿。
    // - 只做索引（导入走这一档）：正文不是产物，边解压边扫，整章字符串不进内存
    //   （实测少 11.4% 的累计分配，见 [`EntryReader::stream`]）。读挂了当作整条失败 ——
    //   已经喂进去那半截不许充作一章的分母。
    let (text, summary) = if with_text {
        let text = reader.read_text(&job.range);
        let summary = text.as_deref().map(crate::content::scan::chapter_summary);
        (text, summary)
    } else {
        let mut scan = crate::content::scan::ChapterScan::new();
        let read = reader.stream(&job.range, &mut |bytes| {
            scan.push(bytes);
            true
        });
        (None, read.map(|()| scan.finish()))
    };
    let char_len = summary.as_ref().map(|s| s.visible_chars);
    let title = toc
        .get(job.href.as_str())
        .map(|e| e.title.clone())
        .filter(|t| !t.is_empty())
        // 标题回落要读了正文才拿得到；正文已经在手上，这条回落是白拿的。
        .or_else(|| summary.and_then(|s| s.heading))
        .unwrap_or_else(|| format!("第 {} 节", job.spine + 1));
    let source = if with_text { text } else { None };
    match source {
        Some(text) => Chapter::inline(title, text)
            .with_spine_href(job.href.clone())
            .with_char_len(char_len),
        // 位置就是条目名，正是打开时 `Entry` 那一路要的东西。
        None => Chapter::entry(title, job.href.clone()).with_char_len(char_len),
    }
}

/// 一批作业跑完的产物，已经按队列下标排回原序。
struct ParsedDoc {
    chapters: Vec<Chapter>,
    images: Vec<ImageRes>,
    cover: Option<Vec<u8>>,
}

/// 并行跑一批解析作业；任何一个 worker 连容器都开不出来时回 `None`，
/// 由调用方退回顺序路径 —— 解析产物不许因为线程开不起来就静默少章。
fn parse_entries_parallel(
    path: &Path,
    ctx: &ParseCtx<'_>,
    with_text: bool,
    workers: usize,
) -> Option<ParsedDoc> {
    let next = AtomicUsize::new(0);
    let mut chapters_out = Vec::with_capacity(ctx.chapters.len());
    let mut images_out = Vec::with_capacity(ctx.images.len());
    let mut cover_out = None;
    let mut opened_all = true;

    std::thread::scope(|scope| {
        // 计数器先借出去再交给各路：`AtomicUsize` 不是 Copy，闭包按值捕获它就只能给一路。
        // 其余参数（`path`、`ctx`）本来就是引用，`move` 带走引用即可，不必再 `let x = x;` 绑一遍。
        let next = &next;
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            handles.push(scope.spawn(move || {
                // 区间表是主线程解好的，worker 只要一个句柄：开一次 `File` 是几微秒，
                // 而 `Zip::open` 那 1.5 ms（1233 条目的书，实测）要乘上 worker 数。
                let Some(mut reader) = EntryReader::open(path) else {
                    return (false, ParsedEntries::default());
                };
                (true, parse_entries(&mut reader, ctx, with_text, next))
            }));
        }
        for handle in handles {
            match handle.join() {
                Ok((opened, parsed)) => {
                    opened_all &= opened;
                    chapters_out.extend(parsed.chapters);
                    images_out.extend(parsed.images);
                    // 封面只有一个下标、只会被一个 worker 领走，所以这里是「填上」而不是「合并」。
                    if parsed.cover.is_some() {
                        cover_out = parsed.cover;
                    }
                }
                // 工作线程 panic 当作这一路不可用：回退到顺序解析，
                // 而不是让一部分章凭空消失。
                Err(_) => opened_all = false,
            }
        }
    });

    if !opened_all {
        return None;
    }
    Some(ParsedDoc {
        chapters: ordered(chapters_out),
        images: ordered(images_out),
        cover: cover_out,
    })
}

/// 顺序跑完同一批作业。少条目、单核、并行起不来时走这一条。
fn parse_entries_sequential(
    reader: &mut EntryReader,
    ctx: &ParseCtx<'_>,
    with_text: bool,
) -> ParsedDoc {
    let next = AtomicUsize::new(0);
    let parsed = parse_entries(reader, ctx, with_text, &next);
    ParsedDoc {
        chapters: ordered(parsed.chapters),
        images: ordered(parsed.images),
        cover: parsed.cover,
    }
}

fn ordered<T>(mut entries: Vec<(usize, T)>) -> Vec<T> {
    entries.sort_unstable_by_key(|(order, _)| *order);
    entries.into_iter().map(|(_, item)| item).collect()
}

fn read_with(path: &Path, with_text: bool) -> Result<(DocMeta, Content)> {
    read_with_impl(path, with_text, ParseBudget::Auto)
}

/// `read_with` 的实现，多收一份 [`ParseBudget`]：测试与基准要一个与机器核数无关的
/// 确定输入（`AtMost(1)` 强制顺序、`AtMost(4)` 压到 4 路），而导入一批书时那个数是
/// 外层分好的（`ingest::import::split_budget`）。
fn read_with_impl(path: &Path, with_text: bool, budget: ParseBudget) -> Result<(DocMeta, Content)> {
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

    // manifest 按 id 建一次表。spine 每一项都要回表取一次自己的条目，逐条 `iter().find`
    // 在按节分包的书上是 O(节数 × 条目数) 次字符串比较 —— 实测《斗战狂潮》1225 节 ×
    // 1233 条目 ≈ 150 万次比较，全花在「这个 id 对应哪一项」上。
    // 重复 id 保留**第一个**，与 `iter().find` 同语义：畸形书不该因为换了查找结构就变内容。
    let mut by_id: HashMap<&str, &Item> = HashMap::new();
    for item in &opf.items {
        by_id.entry(item.id.as_str()).or_insert(item);
    }

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
        .and_then(|id| by_id.get(id))
        .map(|i| resolve(&base_dir, &i.href));
    if cover.is_none() {
        cover = opf
            .items
            .iter()
            .find(|i| i.properties.iter().any(|p| p == "cover-image"))
            .map(|i| resolve(&base_dir, &i.href));
    }
    // 封面在这里**只定位**：定位要读 OPF（主线程本来就在读），而读字节 + 解码 + 缩图
    // 是队列里的一件活（见 `parse_cover` 与 `Job::Cover`），排第一被认领。
    let cover_job = cover
        .as_deref()
        .and_then(|n| zip.range_at(n).map(|range| Cover { href: n, range }));

    // 全文图片：manifest 里所有 image/* 的 item。封面若在 manifest 里会一并被收录，
    // 排版层按解析后的容器路径查表，所以这里存的 href 必须是 `resolve` 过的。
    //
    // 这里只做清单和容器定位；解压与读尺寸交给 `parse_entries` 的 worker。图片与
    // 章节混在同一队列里，是因为一本 30 MB 的书里这两类开销同量级。
    let mut image_jobs = Vec::new();
    for item in opf
        .items
        .iter()
        .filter(|i| i.media_type.starts_with("image/"))
    {
        let href = resolve(&base_dir, &item.href);
        let Some(range) = zip.range_at(&href) else {
            // 插图缺失是按页降级的一种（排版会留一块占位），一条一张，所以放到 trace。
            tracing::trace!("epub: 插图 {href} 不在容器里");
            continue;
        };
        image_jobs.push(ImageJob {
            href,
            range,
            media_type: item.media_type.clone(),
        });
    }

    // 章节：spine 顺序就是阅读顺序；`locate` 只定位条目，真正的解压与 DOM 解析
    // 交给 worker。每个 spine 条目都要解一次正文，因为 `chapters.char_len` 是全局
    // 字符进度的分母，必须在导入时算准（`plans/导入管线.md` §八第 3 条）。
    let mut chapter_jobs = Vec::new();
    for (spine, id) in opf.spine.iter().enumerate() {
        let Some(item) = by_id.get(id.as_str()) else {
            // spine 指着一个 manifest 里没有的 id：这一节整个没了，而且界面上看不出来。
            tracing::warn!(
                "epub: spine 第 {} 项指向的 id {id:?} 不在 manifest 里",
                spine + 1
            );
            continue;
        };
        if !item.media_type.contains("html") || item.properties.iter().any(|p| p == "nav") {
            continue;
        }
        let href = resolve(&base_dir, &item.href);
        let Some(range) = zip.range_at(&href) else {
            // 正文少一章在界面上的表现是「翻到那里是空的」，说不清原因。
            tracing::warn!(
                "epub: 容器里没有 {href}（spine 第 {} 项），这一章读不到",
                spine + 1
            );
            continue;
        };
        chapter_jobs.push(ChapterJob { spine, href, range });
    }

    // 队列的次序就是认领的次序：封面排最前（单本里最长的一件），再章、再图。
    // 下标只用来把产物排回原序，谁领走哪一件由 `next` 抢。
    let mut jobs = Vec::with_capacity(chapter_jobs.len() + image_jobs.len() + 1);
    if cover_job.is_some() {
        jobs.push(Job::Cover);
    }
    jobs.extend((0..chapter_jobs.len()).map(Job::Chapter));
    jobs.extend((0..image_jobs.len()).map(Job::Image));

    let total = jobs.len();
    // 目录查标题的表建一次，交给每个 worker 查（建表在 `toc` 还活着的时候，
    // `toc` 之后要 move 进 `FlowDoc`）。
    let toc_by_href = toc_index(&toc);
    let workers = parse_worker_count(total, budget).min(total.max(1));
    #[cfg(test)]
    LAST_WORKERS.with(|c| c.set(workers));
    let ctx = ParseCtx {
        jobs: &jobs,
        chapters: &chapter_jobs,
        images: &image_jobs,
        cover: cover_job,
        toc: &toc_by_href,
    };
    // 顺序那一路也用同一个读法：一个句柄 + 区间表，与并行只差在不开线程。
    let parsed = if workers <= 1 {
        match EntryReader::open(path) {
            Some(mut reader) => parse_entries_sequential(&mut reader, &ctx, with_text),
            // 句柄都开不出来：容器读不出来，整本书本来也打不开，回一条错而不是静默少章。
            None => return Err(unsupported(path, "连容器句柄都开不出来")),
        }
    } else {
        match parse_entries_parallel(path, &ctx, with_text, workers) {
            Some(parsed) => parsed,
            // 并行起不来或某个 worker 打不开容器：退回顺序路径，结果不许静默变少。
            None => match EntryReader::open(path) {
                Some(mut reader) => parse_entries_sequential(&mut reader, &ctx, with_text),
                None => return Err(unsupported(path, "连容器句柄都开不出来")),
            },
        }
    };
    let ParsedDoc {
        chapters,
        images,
        cover: cover_bytes,
    } = parsed;

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
