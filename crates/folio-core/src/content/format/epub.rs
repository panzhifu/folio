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
    Chapter, ChapterLocator, Content, DocMeta, DocumentProvider, FixedDoc, FixedPage, FlowDoc,
    FontBytes, Format, ImageRes, PageImage, PageSource, ParseBudget, Series, TocEntry,
};
use super::{
    HeadDims, dir_of, extension_of, fragment_of, image_dims, image_dims_from_head,
    is_decodable_bitmap, resolve_href, shrink_image, stem_of,
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
    /// `<item fallback="<另一个 item 的 id>">`：这一份解不动时该读哪一份。
    ///
    /// EPUB3 的降级声明，用在作者知道读者不一定解得动的场合（SVG 是主流那一个）。
    /// 规范上链子每跳都指向 manifest 里的另一个 id，末端那个不再挂 fallback。
    fallback: Option<String>,
}

/// spine 里的一项：`<itemref idref="…" linear="no">`。
#[derive(Debug)]
struct SpineRef {
    id: String,
    /// `linear="no"` 之外都算线性。规范里默认是 yes，所以属性缺席就是 true。
    linear: bool,
}

#[derive(Debug, Default)]
struct Opf {
    title: String,
    authors: Vec<String>,
    items: Vec<Item>,
    spine: Vec<SpineRef>,
    /// `<meta name="cover" content="<item id>">`
    cover_id: Option<String>,
    /// Dublin Core 的其余各项。采集在 `parse_opf`，归置在 [`place_metadata`]。
    ///
    /// 为什么先攒成一串再归置而不是边采边填：有三条规则**要看完全部条目才定得下来** ——
    /// 一串 `dc:identifier` 里挑哪条是 ISBN、系列的三种载体（`belongs-to-collection`、
    /// `calibre:series`、`dc:relation`）谁先赢、以及 EPUB3 挂在 `refines` 上的
    /// `group-position`（它写在主项之后，甚至写在下一项之后）。
    contributors: Vec<String>,
    subjects: Vec<String>,
    description: String,
    publisher: String,
    published: String,
    language: String,
    isbn: String,
    dctype: String,
    rights: String,
    series: Option<Series>,
    /// `<spine page-progression-direction="rtl">`：整本往哪边翻。
    ///
    /// 与 `dir="rtl"`（正文往哪边流）是两件事，各读各的，别从一个推另一个。
    /// 规范里的 `default` 与属性缺席**都折成 `false`**：那一档要求「由内容自己定」，
    /// 而开书那一趟只读索引、不回头嗅第一章的首强字符。后果与理由写在
    /// [`crate::content::format::FlowDoc::progression_rtl`] 上。
    progression_rtl: bool,
    /// `<meta property="rendition:layout">pre-paginated</meta>`：EPUB3 定版式。
    ///
    /// 有它时这本书**不是**可重排的 —— 页是既定的（漫画、绘本、逐页 SVG 那一类），
    /// 正文走 `Content::Fixed` 而不是 `FlowDoc`（见 `read_fixed`）。
    pre_paginated: bool,
}

/// spine 上 `linear="no"` 的那几节默认**不**当一章。
///
/// 它们是封面那版隐藏 alt 文本、脚注弹层、nav 文档自己 —— 收进来就是在正文里插一页
/// 读者从没打算读的东西，还会把后面每一章的序号往后顶一格。留这一颗开关而不是写死在
/// 循环里，是因为读者侧确实有「显示非线性内容」这一档（KOReader 把它做成了界面开关
/// `hide_nonlinear_flows`，`frontend/apps/reader/modules/readerrolling.lua:248`）；
/// **改了它要连 [`super::INDEX_VERSION`] 一起改** —— 章数与章号都变了，老库的位置会错位。
const KEEP_NONLINEAR: bool = false;

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

/// EPUB 规范允许、且**不是一道授权门**的那两种算法：内嵌字节的 XOR 扰动。
///
/// 抄参考实现那张表 —— `reference/koreader/base/thirdparty/kpvcrlib/crengine/crengine/src/epubfmt.cpp:862`
/// 往后只有这两条走「自己解掉」分支，第三条起一律记成 DRM。表里这两条之外的东西
/// 每家都不一样（Adobe、Readium LCP、Kindle），我们不去认它们的名字。
const FONT_OBFUSCATION: [&str; 2] = [
    "http://ns.adobe.com/pdf/enc#RC",
    "http://www.idpf.org/2008/embedding",
];

/// 这本书里第一条**不是**字体去混淆的加密方法，也就是 DRM。`None` = 那些字节读得动。
///
/// 认不出来的一律算 DRM，而不是算「不认识所以不管」：放一本加密书进来的下场是架上
/// 多一本能翻页、每页都是乱码的书，而它永远重扫不好 —— 少收一本的代价只是读者换本书。
fn drm_method(zip: &mut Zip) -> Option<String> {
    let text = zip
        .locate("META-INF/encryption.xml")
        .and_then(|name| zip.read_text(&name))?;
    let mut reader = Reader::from_str(&text);
    // 读不出下一个事件就到此为止（畸形 XML 与读完是同一档）：已经采到的方法照样算。
    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Start(e) | Event::Empty(e)
                if local_name(e.name().as_ref()) == "EncryptionMethod" =>
            {
                if let Some(method) = attr(&e, "Algorithm")
                    && !FONT_OBFUSCATION.contains(&method.as_str())
                {
                    return Some(method);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    None
}

/// `metadata` 段里采到的一条原始条目。归置在 [`place_metadata`]。
#[derive(Debug)]
struct RawMeta {
    /// 元素的本地名：`title` / `creator` / … 或 `meta`。收尾时靠它配对栈顶，
    /// 属性写坏或嵌套反常时宁可让那一层留在栈里，也不错位地把文本记到别的名下。
    elem: String,
    /// `dc:` 元素就是它的本地名；`<meta>` 是它的 `property`（EPUB3）或 `name`（EPUB2），
    /// 一律小写。
    key: String,
    /// 元素的 `id`：EPUB3 用 `refines="#这个 id"` 把修饰项挂回主项（系列序号就是这么来的）。
    id: String,
    /// `opf:scheme="ISBN"` 与 `scheme="ISBN"` 两种拼法（`attr` 已去前缀，所以一处认两个）。
    scheme: String,
    refines: String,
    text: String,
}

/// `metadata` 段里要采的 `dc:` 元素本地名。清单外的（`<reference>` 那类）不采。
///
/// 按本地名匹配与 `title` / `creator` 一直是同一口径：OPF 的 metadata 段里这些名字不会
/// 与别的命名空间撞车，`section` 那道闸已经把范围收到 `<metadata>` 之内。
const DC_KEYS: &[&str] = &[
    "title",
    "creator",
    "contributor",
    "subject",
    "description",
    "publisher",
    "date",
    "language",
    "identifier",
    "type",
    "rights",
    "relation",
];

/// `<meta>` 这一条记在哪个键下：EPUB3 的 `property`、EPUB2 的 `name`。两个都没有就不采
/// （那种 `<meta>` 是 `dcterms:modified` 之类的时间戳声明，书目里没有它的位置）。
fn meta_key(start: &quick_xml::events::BytesStart<'_>) -> Option<String> {
    attr(start, "property")
        .or_else(|| attr(start, "name"))
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty())
}

/// 把采下来的一串条目归置成 `Opf` 的那几格。
///
/// 三条规则**必须看完全部条目才定得下来**，所以采集与归置分两趟：一串 identifier 里
/// 挑哪条是 ISBN；系列的三种载体谁先赢；挂在 `refines` 上的 `group-position` 写在主项
/// 之后（甚至隔了一条别的条目之后）。
fn place_metadata(out: &mut Opf, metas: &[RawMeta]) {
    let one = |key: &str| -> String {
        metas
            .iter()
            .find(|m| m.key == key)
            .map(|m| m.text.trim().to_string())
            .unwrap_or_default()
    };
    let many = |key: &str| -> Vec<String> {
        metas
            .iter()
            .filter(|m| m.key == key)
            .map(|m| m.text.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect()
    };
    // 同名多条时「第一条赢」是 CSS/OPF 里 `dc:title` 的既有行为（原来那份实现就是这样），
    // 多值的那几项（creator / contributor / subject）才全收。
    if out.title.is_empty() {
        out.title = one("title");
    }
    if out.authors.is_empty() {
        out.authors = many("creator");
    }
    out.contributors = many("contributor");
    out.subjects = many("subject");
    out.description = one("description");
    out.publisher = one("publisher");
    out.published = one("date");
    out.language = one("language");
    out.dctype = one("type");
    out.rights = one("rights");
    out.isbn = pick_isbn(metas);
    out.series = pick_series(metas);
    // `<meta property="rendition:layout">pre-paginated</meta>`：定版式声明。
    out.pre_paginated = metas.iter().any(|m| {
        m.key == "rendition:layout" && m.text.trim().eq_ignore_ascii_case("pre-paginated")
    });
}

/// 一串 identifier 里挑 ISBN。**只认明确标着的**四种写法：`urn:isbn:` 前缀、
/// `ISBN:` 前缀、`opf:scheme="ISBN"` / `scheme="ISBN"` 标着的、以及 Calibre 的
/// `meta name="calibre:isbn"`。
///
/// 不认「长得像 13 位数字」的那一条：同一本书的 `dc:identifier` 里通常还躺着一条 uuid，
/// 而 uuid 也是数字与字母混着的一串 —— 猜错的代价是把书目里那条 ISBN 永远认错。
fn pick_isbn(metas: &[RawMeta]) -> String {
    for m in metas
        .iter()
        .filter(|m| m.key.ends_with("identifier") || m.key == "calibre:isbn")
    {
        let text = m.text.trim();
        if text.is_empty() {
            continue;
        }
        // Calibre 那一格的名字本身就说它是 ISBN。
        if m.key == "calibre:isbn" {
            return text.to_string();
        }
        for prefix in ["urn:isbn:", "urn:ISBN:", "ISBN:", "isbn:", "ISBN "] {
            if let Some(rest) = text.strip_prefix(prefix) {
                let rest = rest.trim();
                if !rest.is_empty() {
                    return rest.to_string();
                }
            }
        }
        if m.scheme == "isbn" {
            return text.to_string();
        }
    }
    String::new()
}

/// 系列的三种载体，**先碰到的赢**：EPUB3 的 `belongs-to-collection`（配 `refines` 上来的
/// `group-position`）→ Calibre 私有的 `calibre:series`（配 `calibre:series_index`）→
/// 裸的 `dc:relation`。
///
/// `relation` 排最后是因为它字面意思更宽（「与另一资源有关」），认错了比认不出更难查；
/// 但把它当系列仍是 EPUB3 之前的规范读法，所以不收掉。
fn pick_series(metas: &[RawMeta]) -> Option<Series> {
    for (host_key, position_key) in [
        ("belongs-to-collection", "group-position"),
        ("calibre:series", "calibre:series_index"),
        ("relation", ""),
    ] {
        let Some(host) = metas
            .iter()
            .find(|m| m.key == host_key && !m.text.trim().is_empty())
        else {
            continue;
        };
        let position = if position_key.is_empty() {
            String::new()
        } else {
            refine(host, metas, position_key)
        };
        return Some(Series {
            name: host.text.trim().to_string(),
            position,
        });
    }
    None
}

/// 挂在主项上的那一格：先找 `refines="#主项 id"` 的那条；没有 `refines`（老写法把它省了）
/// 就退而取第一条同键的。两头都找不到给空串。
fn refine(host: &RawMeta, metas: &[RawMeta], key: &str) -> String {
    let wanted = format!("#{}", host.id);
    if !host.id.is_empty()
        && let Some(m) = metas.iter().find(|m| m.key == key && m.refines == wanted)
    {
        return m.text.trim().to_string();
    }
    metas
        .iter()
        .find(|m| m.key == key && m.refines.is_empty())
        .map(|m| m.text.trim().to_string())
        .unwrap_or_default()
}

fn parse_opf(xml: &str) -> Opf {
    let mut out = Opf::default();
    let mut reader = Reader::from_str(xml);
    let mut section = String::new();
    // 正在采文本的元素栈。为什么要一层栈而不是一个 `Option`：EPUB3 的系列写法是
    // `<meta property="belongs-to-collection" id="c1">系列名
    //    <meta refines="#c1" property="group-position">3</meta>
    //  </meta>` —— 主项里套修饰项。文本只归到**最里面那一层**，所以主项的 text 不会
    // 把序号一起吃进去。
    let mut open: Vec<RawMeta> = Vec::new();
    // 采完的条目，文档序。归置在 [`place_metadata`]。
    let mut metas: Vec<RawMeta> = Vec::new();
    // 读不出下一个事件就到此为止（畸形 XML 与读完是同一档）：
    // 已经采到的部分照样有用，一本书因为某处编码坏了就整本打不开是不可接受的。
    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Start(e) => {
                let n = e.name();
                let name = local_name(n.as_ref());
                match name {
                    "metadata" | "manifest" => section = name.to_string(),
                    "spine" => {
                        section = name.to_string();
                        // 只认显式写了的 `rtl`。`ltr` / `default` / 没写 三种都落 `false`，
                        // 其中 `default` 那一档是明知不完整的取舍，理由见字段上的注释。
                        // 枚举值的属性按规范要先去首尾空白（`" rtl "` 是合法写法），
                        // 比 `dir` 那一格同一口径。
                        out.progression_rtl = attr(&e, "page-progression-direction")
                            .is_some_and(|v| v.trim().eq_ignore_ascii_case("rtl"));
                    }
                    "meta" if section == "metadata" => {
                        // `<meta>` 的两种拼法：EPUB3 写 `property`，EPUB2 写 `name`。
                        // `rendition:layout` 那一定版式声明也走这一条路（归置在下面）。
                        if let Some(key) = meta_key(&e) {
                            open.push(RawMeta {
                                elem: name.to_string(),
                                key,
                                id: attr(&e, "id").unwrap_or_default(),
                                scheme: attr(&e, "scheme").unwrap_or_default().to_ascii_lowercase(),
                                refines: attr(&e, "refines").unwrap_or_default(),
                                text: String::new(),
                            });
                        }
                    }
                    name if section == "metadata" && DC_KEYS.contains(&name) => {
                        open.push(RawMeta {
                            elem: name.to_string(),
                            key: name.to_string(),
                            id: attr(&e, "id").unwrap_or_default(),
                            scheme: attr(&e, "scheme").unwrap_or_default().to_ascii_lowercase(),
                            refines: attr(&e, "refines").unwrap_or_default(),
                            text: String::new(),
                        });
                    }
                    _ => {}
                }
            }
            Event::Text(e) => {
                if let Some(top) = open.last_mut() {
                    top.text.push_str(&text_of(e.as_ref()));
                }
            }
            Event::End(e) => {
                let n = e.name();
                let name = local_name(n.as_ref());
                if name == "metadata" {
                    section = String::new();
                    // 走到这里还没闭合的条目属于 XML 本身坏了那一档：丢掉，已采到的照用。
                    open.clear();
                    continue;
                }
                if section == "metadata"
                    && open.last().is_some_and(|top| top.elem == name)
                    && let Some(done) = open.pop()
                {
                    metas.push(done);
                }
                if matches!(name, "manifest" | "spine") {
                    section = String::new();
                }
            }
            Event::Empty(e) => {
                let n = e.name();
                let name = local_name(n.as_ref());
                // 自闭的 `<meta name="calibre:series" content="…"/>`：EPUB2 那一路的
                // Calibre 导出就是这么写的（EPUB3 用带文本的 `property` 形式）。
                // 值在 `content` 属性里，所以这一条不走文本采集那一套。
                if section == "metadata"
                    && name == "meta"
                    && let Some(key) = meta_key(&e)
                {
                    metas.push(RawMeta {
                        elem: name.to_string(),
                        key,
                        id: attr(&e, "id").unwrap_or_default(),
                        scheme: attr(&e, "scheme").unwrap_or_default().to_ascii_lowercase(),
                        refines: attr(&e, "refines").unwrap_or_default(),
                        text: attr(&e, "content").unwrap_or_default(),
                    });
                }
                match (section.as_str(), name) {
                    ("manifest", "item") => {
                        if let Some(item) = read_item(&e) {
                            out.items.push(item);
                        }
                    }
                    ("spine", "itemref") => {
                        if let Some(id) = attr(&e, "idref") {
                            let linear =
                                attr(&e, "linear").is_none_or(|v| !v.eq_ignore_ascii_case("no"));
                            out.spine.push(SpineRef { id, linear });
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
    place_metadata(&mut out, &metas);
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
        // 写成空串的 fallback 按「没有」算：拿它去查 manifest 只会得到一条「指着一个
        // 不在表里的 id」的误导日志，而那一格本来就不该有下一步。
        fallback: attr(e, "fallback").filter(|id| !id.is_empty()),
    })
}

/// nav 文档里那个 `<nav>` 归哪一档。规范用 `epub:type` 标，实际出版书也常写在 `class`
/// 上，两个都认（沿用的是这一层一直以来的判法）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NavKind {
    Toc,
    Landmarks,
    PageList,
    /// 其余（`toc-figures`、`toc-illu`、`cover`…）：整词比不中这三档，解出来也不参与目录。
    Other,
}

/// 一个 `<nav>` 的归类。规范允许一个 nav 挂多个 type（`toc bibref` 很常见），所以
/// 三档按优先级判而不是取第一个 token。
fn nav_kind(type_attr: Option<String>, class_attr: Option<String>) -> NavKind {
    // 两个都要先落成变量再借：`unwrap_or_default()` 造的是临时串，挂在迭代器上活不过一句。
    let types = type_attr.unwrap_or_default();
    let classes = class_attr.unwrap_or_default();
    let tokens = types.split_whitespace().chain(classes.split_whitespace());
    let (mut toc, mut landmarks, mut page_list) = (false, false, false);
    for token in tokens {
        match token {
            "toc" => toc = true,
            "landmarks" => landmarks = true,
            "page-list" => page_list = true,
            _ => {}
        }
    }
    if toc {
        NavKind::Toc
    } else if landmarks {
        NavKind::Landmarks
    } else if page_list {
        NavKind::PageList
    } else {
        NavKind::Other
    }
}

/// 一份 nav 文档解出的三棵树。
#[derive(Debug, Default)]
struct NavTrees {
    toc: Vec<TocEntry>,
    landmarks: Vec<TocEntry>,
    page_list: Vec<TocEntry>,
}

impl NavTrees {
    /// 拿得出目录的那一棵：**toc → landmarks → page-list**。
    ///
    /// 这个次序抄参考实现（`reference/koreader/base/thirdparty/kpvcrlib/crengine/crengine/src/epubfmt.cpp:1760`）。
    /// 后两档不是凑数，它们各自救一类真书：只写了 landmarks 的（cover / start / toc 那三跳
    /// 至少是入口），和只有 page-list 的教科书 —— 共同点是**没有 toc**，而读者那一边界面上
    /// 不该是一栏空白。
    fn picked(&self) -> &[TocEntry] {
        if !self.toc.is_empty() {
            return &self.toc;
        }
        if !self.landmarks.is_empty() {
            return &self.landmarks;
        }
        &self.page_list
    }

    /// 收一棵树。**同一档有两份 nav 时留第一个**：nav 文档里重复的 type 后面那份多半是
    /// 嵌套的页码表，覆盖了会把目录换成页码。
    fn insert(&mut self, kind: NavKind, mut roots: Vec<TocEntry>) {
        if roots.is_empty() {
            return;
        }
        let slot = match kind {
            NavKind::Toc if self.toc.is_empty() => Some(&mut self.toc),
            NavKind::Landmarks if self.landmarks.is_empty() => Some(&mut self.landmarks),
            NavKind::PageList if self.page_list.is_empty() => Some(&mut self.page_list),
            _ => None,
        };
        if let Some(slot) = slot {
            fill_hrefs(&mut roots);
            *slot = roots;
        }
    }
}

/// 一个还没收尾的 `<li>`。
#[derive(Default)]
struct LiFrame {
    /// 它自己那一条目（`</a>` 收尾时填）。`None` = 纯容器，只有 `<span>` 那一档。
    entry: Option<TocEntry>,
    /// 没有 `<a>` 时的标题来源：直接挂在这个 `<li>` 上的 `<span>` 文本。
    label: String,
    /// 在这一层已经收尾的子孙，`</li>` 时整体挂到自己名下。
    done: Vec<TocEntry>,
}

/// 一个还没收尾的 `<nav>`。nav 在畸形书里会互相套着写，所以外层是一个栈。
struct NavFrame {
    kind: NavKind,
    lis: Vec<LiFrame>,
    roots: Vec<TocEntry>,
}

/// nav 文档（EPUB3 的 `nav.xhtml`）解出的树。
///
/// 嵌套靠 `<li>` 栈算：`</a>` 把条目交给当前那个 `<li>`，`</li>` 收尾时这条目连同它攒下的
/// 子孙才归位。
///
/// **只有 `<span>` 没有 `<a>` 的那个 `<li>` 是一个标题，不是一章**（「上卷 / 下卷」「第一部分」
/// 就是这么写的，出版书里非常常见）。这一层以前把它**丢掉**、子条目整体上一格 —— 层级就在
/// 那一卷那里断掉，目录上看不出「上卷」下面管着哪几章。现在它留成一个没有自己位置的条目，
/// 位置从第一个解得开的子孙回填（点它会跳到那一卷的第一节，与参考实现同一处理，
/// `epubfmt.cpp:254-284`）。
fn parse_nav(xml: &str) -> NavTrees {
    let mut out = NavTrees::default();
    let mut reader = Reader::from_str(xml);
    let mut navs: Vec<NavFrame> = Vec::new();
    // 正在采文本的那条 `<a>`：href、采到的标签、`title` 属性（图当条目用的书只有它）。
    let mut link: Option<(String, String, String)> = None;
    // 开着几个 `<span>`。只在没有 `<a>` 开着时才往容器标签上累加，否则
    // `<a href><span>第一章</span></a>` 的文本会被 span 抢走、条目反而没了标题。
    let mut span = 0usize;
    // 读不出下一个事件就到此为止（畸形 XML 与读完是同一档）：
    // 已经采到的部分照样有用，一本书因为某处编码坏了就整本打不开是不可接受的。
    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Start(e) => match local_name(e.name().as_ref()) {
                "nav" => navs.push(NavFrame {
                    kind: nav_kind(attr(&e, "type"), attr(&e, "class")),
                    lis: Vec::new(),
                    roots: Vec::new(),
                }),
                // 一个 `<a>` 里套 `<a>` 是畸形；后开的当它不存在，标签不许被吞两次。
                "a" if !navs.is_empty() && link.is_none() => {
                    link = Some((
                        attr(&e, "href").unwrap_or_default(),
                        String::new(),
                        attr(&e, "title").unwrap_or_default(),
                    ));
                }
                "li" if !navs.is_empty() => {
                    if let Some(nav) = navs.last_mut() {
                        nav.lis.push(LiFrame::default());
                    }
                }
                "span" => span += 1,
                _ => {}
            },
            Event::Text(e) => {
                let text = text_of(e.as_ref());
                if let Some((_, label, _)) = link.as_mut() {
                    label.push_str(&text);
                } else if span > 0
                    && let Some(nav) = navs.last_mut()
                    && let Some(li) = nav.lis.last_mut()
                {
                    li.label.push_str(&text);
                }
            }
            Event::End(e) => {
                let n = e.name();
                let name = local_name(n.as_ref());
                if name == "span" {
                    span = span.saturating_sub(1);
                    continue;
                }
                // `<a>` 与 `<li>` 都要落到「最内层那个还没收尾的 nav」上，取不到就整段跳过。
                match name {
                    "nav" => {
                        if let Some(nav) = navs.pop() {
                            out.insert(nav.kind, nav.roots);
                        }
                    }
                    "a" => {
                        if let (Some((href, label, title_attr)), Some(nav)) =
                            (link.take(), navs.last_mut())
                        {
                            let title = tidy(&label).or_else(|| tidy(&title_attr));
                            // 没有标签的链接不成条目（与这一层一直以来的口径一致）：
                            // 目录里一行空白比一行缺的字更难解释。
                            if let Some(title) = title {
                                let entry = TocEntry {
                                    title,
                                    href,
                                    // 采下来的是原始 href；换成绝对路径时（`rebase`）才拆出 fragment。
                                    fragment: None,
                                    children: Vec::new(),
                                };
                                match nav.lis.last_mut() {
                                    Some(li) if li.entry.is_none() => li.entry = Some(entry),
                                    // `<a>` 没被任何 `<li>` 包住（不规范但真有这么写的）：
                                    // 至少条目本身别丢。
                                    _ => nav.roots.push(entry),
                                }
                            }
                        }
                    }
                    "li" => {
                        if let Some(nav) = navs.last_mut()
                            && let Some(frame) = nav.lis.pop()
                        {
                            let LiFrame { entry, label, done } = frame;
                            // 三种结局，`done` 在每一支里只交出去一次（写进 `children`、
                            // 或作为要上提的那一批）：编译器证不出 match 各支互斥，
                            // 所以不能两支都直接 move 同一个 `done` 再在外面用一次。
                            let (item, promoted): (Option<TocEntry>, Vec<TocEntry>) = match entry {
                                Some(mut e) => {
                                    e.children = done;
                                    (Some(e), Vec::new())
                                }
                                None if !done.is_empty() => match tidy(&label) {
                                    Some(title) => (
                                        Some(TocEntry {
                                            title,
                                            // 纯容器没有自己的位置：下面 `fill_hrefs` 会从
                                            // 第一个解得开的子孙回填一个。
                                            href: String::new(),
                                            fragment: None,
                                            children: done,
                                        }),
                                        Vec::new(),
                                    ),
                                    None => (None, done),
                                },
                                None => (None, done),
                            };
                            match item {
                                Some(item) => match nav.lis.last_mut() {
                                    Some(parent) => parent.done.push(item),
                                    None => nav.roots.push(item),
                                },
                                // 这一层既没标题也没条目（一个光包住 `<ol>` 的 `<li>`）：
                                // 子条目整体上一格，目录宁可少一层也不要丢条目。
                                None => match nav.lis.last_mut() {
                                    Some(parent) => parent.done.extend(promoted),
                                    None => nav.roots.extend(promoted),
                                },
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    // 文档在 nav 还没收尾时就断了：把已经采到的收进树，而不是整栏目录作废。
    while let Some(nav) = navs.pop() {
        let mut roots = nav.roots;
        for frame in nav.lis {
            roots.extend(frame.done);
            roots.extend(frame.entry);
        }
        out.insert(nav.kind, roots);
    }
    out
}

/// 首尾空白去掉、中间连续的空白压成一个空格。全空回 `None` —— 「能不能成一个条目」
/// 这一层就是用它判的。
fn tidy(text: &str) -> Option<String> {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!collapsed.is_empty()).then_some(collapsed)
}

/// 纯容器条目（只有标题、没有位置）从自己的子孙回填一个位置。
///
/// 回的是**第一个解得开的子孙**而不是留空：界面上点「上卷」这一行该跳到这一卷的第一节，
/// 而不是什么都不发生。返回这棵树里第一个非空 href，供上一层继续回填。
fn fill_hrefs(entries: &mut [TocEntry]) -> Option<String> {
    let mut first: Option<String> = None;
    for entry in entries.iter_mut() {
        if !entry.children.is_empty() {
            let inner = fill_hrefs(&mut entry.children);
            if entry.href.is_empty() {
                entry.href = inner.clone().unwrap_or_default();
            }
        }
        if first.is_none() && !entry.href.is_empty() {
            first = Some(entry.href.clone());
        }
    }
    first
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
    /// 解析后的容器绝对路径，进 `ImageRes::href` —— 排版与绘制查表用的那个键，
    /// 它必须等于正文 `<img src>` 按本章目录解出来的那一条。
    href: String,
    /// 字节真实所在的那条目，进 `ImageRes::source`。
    ///
    /// 没有降级路时它就是 `href` 的副本：worker 只认这一格去哪儿读，不在两处各判一次
    /// 「这张要不要换」。换的时候两处不同（`decodable_fallback`）。
    source: String,
    /// 这张图在容器里的压缩区间（主线程已经按 `source` 定位过）。
    range: EntryRange,
    /// 与 `source` 同源的那一份声明的类型：解得动的位图那一串。
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
        source: job.source.clone(),
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

/// spine 上出现这些 media-type 时它**不是读物**：样式、字体、以及导航文档自己
/// （NCX、OPF）常常被写在 spine 里，收进来就是往正文里插一章读者从没打算读的东西。
///
/// 与「是内容但这一版解不动」分开处理（见 [`unreadable_chapter`]）：那一档留一章说明，
/// 这一档安静跳过 —— 一章「这一节是 text/css」的目录条目是噪声，不是信息。
///
/// 插图（`image/*`）**不在这一档**：它按规范只该出现在 manifest 里被正文引用，
/// 而写在 spine 上的那一张是一页内容 —— 逐页 SVG 的漫画书就是这么写的，
/// 那种书按这一档丢掉就是「凭空少几页」，所以让它走 [`unreadable_chapter`]。
fn is_resource(media_type: &str) -> bool {
    let mime = media_type
        .split(';')
        .next()
        .unwrap_or(media_type)
        .trim()
        .to_ascii_lowercase();
    mime == "text/css"
        || mime.starts_with("font/")
        || mime.starts_with("application/font-")
        || mime.starts_with("application/x-font-")
        || mime == "application/vnd.ms-opentype"
        // NCX（EPUB2 目录）与两种 legacy 清单。
        || mime.ends_with("ncx+xml")
        || mime == "application/x-dtbentity-manager.xml"
        || mime == "application/oebps-package+xml"
        // 脚本不是读物（`dom` 那一层也把它整段跳过）。
        || mime.contains("javascript")
        // SMIL 媒体叠加（音频+文本同步）是 sidecar，不是一章内容。
        || mime.contains("smil")
}

/// 沿 `<item fallback="…">` 那条链，找**第一份这一版解得动的位图**。
///
/// EPUB3 让作者给「读者不一定解得动的那一份」挂一条降级路，主流那一个用法就是 SVG：
/// 正文里 `<img src="chart.svg">`，manifest 上 `href="chart.svg" media-type="image/svg+xml"
/// fallback="chart_png"`，而 `chart_png` 是同一张图的 PNG。我们这一版没有 SVG 栅格化器，
/// 所以这条链是那些图**唯一**画得出来的路 —— 不走它，读者看到的是一块空白，而字节明明
/// 就躺在同一个容器里。
///
/// 原条目自己就解得动时回 `None`（= 照原条目读），调用方因此不必区分「没有挂链」与
/// 「链上什么都没有」，那两种是同一件事。三档畸形各给一条说法：
///
/// - **环**：`a` 挂 `b`、`b` 挂 `a` 的真书是存在的，所以每个 id 只许经过一次；
/// - **指着 manifest 里没有的 id**：与没挂链同一档，但那本降级路是作者写错的，值得留一行；
/// - **长过 [`FALLBACK_HOPS`] 跳**：规范上不该有这么长的链，走满就按没有处理 —— 一本书
///   不该在导入那一趟里绕不完。
const FALLBACK_HOPS: usize = 8;

fn decodable_fallback<'a>(start: &'a Item, by_id: &HashMap<&'a str, &'a Item>) -> Option<&'a Item> {
    if is_decodable_bitmap(&start.media_type) {
        return None;
    }
    let mut current = start;
    let mut seen: Vec<&str> = vec![current.id.as_str()];
    for _ in 0..FALLBACK_HOPS {
        let next_id = current.fallback.as_deref()?;
        if seen.contains(&next_id) {
            tracing::debug!(
                "epub: item {:?} 的降级链绕回 {:?}，按没有降级路处理",
                current.id,
                next_id
            );
            return None;
        }
        seen.push(next_id);
        let Some(&next) = by_id.get(next_id) else {
            tracing::debug!(
                "epub: item {:?} 声明降级到 {:?}，manifest 里没有这一条",
                current.id,
                next_id
            );
            return None;
        };
        if is_decodable_bitmap(&next.media_type) {
            return Some(next);
        }
        current = next;
    }
    tracing::debug!(
        "epub: item {:?} 的降级链长过 {FALLBACK_HOPS} 跳，按没有降级路处理",
        start.id
    );
    None
}

/// 是内容、但这一版解不动的那一节：留一章写明读不了。
///
/// 走 [`Chapter::inline`] 是刻意的 —— 它没有能回容器取字节的位置（取回来也是解不动的那堆
/// 字节），所以正文就在手上，`chapters.char_len` 也照样数得出这段说明的字数。
/// media-type 经过 `from_plain_text` 全量 XML 转义：畸形 OPF 里那串是别人写的数据，
/// 不该变成读者屏幕上的一段真标记。
fn unreadable_chapter(title: String, media_type: &str) -> Chapter {
    Chapter::inline(
        title,
        super::xhtml::from_plain_text(&format!("这一节是 {media_type}，这一版还读不了它。")),
    )
}

fn read_with(path: &Path, with_text: bool) -> Result<(DocMeta, Content)> {
    read_with_impl(path, with_text, ParseBudget::Auto)
}

/// `read_with` 的实现，多收一份 [`ParseBudget`]：测试与基准要一个与机器核数无关的
/// 确定输入（`AtMost(1)` 强制顺序、`AtMost(4)` 压到 4 路），而导入一批书时那个数是
/// 外层分好的（`ingest::import::split_budget`）。
fn read_with_impl(path: &Path, with_text: bool, budget: ParseBudget) -> Result<(DocMeta, Content)> {
    let mut zip = Zip::open(path)?;
    // 加密书在门口就该拒。三层判定看的是容器本身（zip 完好、`mimetype` 也对），只有读到
    // `META-INF/encryption.xml` 才知道那些正文**永远**解不开。放它进来的下场是实打实的：
    // 架上多一本能翻页、每一页都是乱码的书，而它重扫一万次也不会变好。
    if let Some(method) = drm_method(&mut zip) {
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        tracing::warn!("epub: {name} 带 DRM 加密（{method}），拒掉");
        return Err(crate::Error::Encrypted { name, method });
    }
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

    // 定版式（EPUB3 `rendition:layout=pre-paginated`）：页是既定的，走 `FixedDoc` 那一族
    // —— 逐页 SVG 的漫画、绘本都是这一档。页表在别处建，这里分流。
    if opf.pre_paginated {
        return read_fixed(path, &mut zip, &opf, &by_id, &base_dir);
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
    // 没有 nav 就回落到 EPUB2 的 NCX。nav 文档内部还有一层回落：`toc` → `landmarks` →
    // `page-list`（见 [`NavTrees::picked`]）。
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
            parse_nav(&text).picked().to_vec()
        } else {
            parse_ncx(&text)
        };
        toc = entries
            .into_iter()
            .map(|e| rebase(e, &dir, &base_dir, &zip))
            .collect();
        if !toc.is_empty() {
            break;
        }
    }
    if toc.is_empty() {
        // 没目录只是侧栏少一栏，但它常常是「容器结构不合常规」的第一个信号。
        tracing::debug!("epub: nav 与 NCX 都没解出目录条目，这本书没有目录");
    }

    // 封面：先认 `<meta name="cover">` 指的那一条，没有就认 EPUB3 的 `properties="cover-image"`。
    let cover_item: Option<&Item> = opf
        .cover_id
        .as_deref()
        .and_then(|id| by_id.get(id).copied())
        .or_else(|| {
            opf.items
                .iter()
                .find(|i| i.properties.iter().any(|p| p == "cover-image"))
        });
    // 声明的那一条与**实际读**的那一条可以不同：SVG 封面在架子上就是一块空白，而作者常常
    // 已经把位图那一份挂在 `fallback=` 上了。封面落盘按它自己字节的哈希命名
    // （`plans/导入管线.md` §八第 4 条），所以这里换的只是「去哪儿读」，没有第二处键要跟着改
    // —— 这一点和图片那条路正相反，那条的键来自正文 `<img src>`，动不得。
    let cover_at = cover_item.map(|item| {
        let declared = resolve(&base_dir, &item.href);
        match decodable_fallback(item, &by_id) {
            Some(alt) => {
                let at = resolve(&base_dir, &alt.href);
                tracing::debug!("epub: 封面声明的是 {declared}，按降级链读 {at}");
                at
            }
            None => declared,
        }
    });
    // 封面在这里**只定位**：定位要读 OPF（主线程本来就在读），而读字节 + 解码 + 缩图
    // 是队列里的一件活（见 `parse_cover` 与 `Job::Cover`），排第一被认领。
    let cover_job = cover_at
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
        // 声明的那一份这一版解不动（`image/svg+xml` 是主流那一个）就沿 `fallback=` 换一份。
        // **换的只是去哪儿读**：`href` 那格仍是正文 `<img src>` 解出来的那一条，因为排版与
        // 绘制都按它查表，而正文一个字都不许改（锚点坐标建在源文本上，`plans/Locator.md`）。
        // 尺寸也跟着换成从位图那份的头部读，所以这类书第一次知道这张图占多大 ——
        // 不换的话拿到的是 `(0, 0)`，排版按 1:1 兜底，字节也画不出来。
        let (source, media_type) = match decodable_fallback(item, &by_id) {
            Some(alt) => (resolve(&base_dir, &alt.href), alt.media_type.clone()),
            None => (href.clone(), item.media_type.clone()),
        };
        let Some(range) = zip.range_at(&source) else {
            // 插图缺失是按页降级的一种（排版会留一块占位），一条一张，所以放到 trace。
            tracing::trace!("epub: 插图 {source} 不在容器里");
            continue;
        };
        image_jobs.push(ImageJob {
            href,
            source,
            range,
            media_type,
        });
    }

    // 章节：spine 顺序就是阅读顺序；`locate` 只定位条目，真正的解压与 DOM 解析
    // 交给 worker。每个 spine 条目都要解一次正文，因为 `chapters.char_len` 是全局
    // 字符进度的分母，必须在导入时算准（`plans/导入管线.md` §八第 3 条）。
    //
    // 目录查标题的表在这里建一次（真章的空壳回落与 worker 用的是同一份），
    // 交给每个 worker 查；`toc` 之后要 move 进 `FlowDoc`。
    let toc_by_href = toc_index(&toc);
    let mut chapter_jobs = Vec::new();
    // 读不动但是**内容**的那几节留下的那一章：`(spine 下标, 章)`。它们不进并行队列 ——
    // 没有条目要解压，塞进去只是让 worker 多跑一趟空。
    let mut notices: Vec<(usize, Chapter)> = Vec::new();
    // 标题回落要按 spine 下标算「第 N 节」，而下标得是**过滤之前**的那一个：
    // spine 里被跳过的 nav 与资源不该改变后面章节的编号（与 `ChapterJob::spine` 同一条理由）。
    for (spine, node) in opf.spine.iter().enumerate() {
        let Some(item) = by_id.get(node.id.as_str()) else {
            // spine 指着一个 manifest 里没有的 id：这一节整个没了，而且界面上看不出来。
            tracing::warn!(
                "epub: spine 第 {} 项指向的 id {:?} 不在 manifest 里",
                spine + 1,
                node.id
            );
            continue;
        };
        if item.properties.iter().any(|p| p == "nav") || is_resource(&item.media_type) {
            continue;
        }
        // 非线性那一档的口径与那颗开关都收在 [`KEEP_NONLINEAR`]。
        if !node.linear && !KEEP_NONLINEAR {
            tracing::debug!("epub: spine 第 {} 项写的非线性内容，不当成一章", spine + 1);
            continue;
        }
        let href = resolve(&base_dir, &item.href);
        let titled = || {
            toc_by_href
                .get(href.as_str())
                .map(|e| e.title.clone())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| format!("第 {} 节", spine + 1))
        };
        if !item.media_type.contains("html") {
            // 是内容、但这一版解不动（spine 上逐页的 SVG、smil 时序、pdf、音视频）。
            // 留一章写明读不了，而不是让它从章数与目录里凭空消失 —— 读者的下场本来是
            // 「这本书少了一章」，而那件事故意查不出原因。
            notices.push((spine, unreadable_chapter(titled(), &item.media_type)));
            continue;
        }
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
        chapters: parsed_chapters,
        images,
        cover: cover_bytes,
    } = parsed;
    // 真空壳与真章合成一条阅读顺序，按 **spine 下标**排。分两处算会让「第几章」这一格漂：
    // `ordered` 排的是队列下标，而队列里没有那些空壳。
    // `chapter_jobs[i]` 与 `parsed_chapters[i]` 一一对应（`ordered` 就是按它排的）。
    let mut chapters: Vec<(usize, Chapter)> = chapter_jobs
        .iter()
        .zip(parsed_chapters)
        .map(|(job, chapter)| (job.spine, chapter))
        .chain(notices)
        .collect();
    chapters.sort_unstable_by_key(|(spine, _)| *spine);
    let chapters: Vec<Chapter> = chapters.into_iter().map(|(_, chapter)| chapter).collect();

    let meta = DocMeta {
        title: if opf.title.is_empty() {
            path.file_stem()
                .map_or_else(String::new, |s| s.to_string_lossy().into_owned())
        } else {
            opf.title
        },
        authors: opf.authors,
        contributors: opf.contributors,
        subjects: opf.subjects,
        description: opf.description,
        publisher: opf.publisher,
        published: opf.published,
        language: opf.language,
        isbn: opf.isbn,
        dctype: opf.dctype,
        rights: opf.rights,
        series: opf.series,
        format: Format::Epub,
        size: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
    };
    let flow = FlowDoc {
        chapters,
        css,
        css_hrefs,
        toc,
        progression_rtl: opf.progression_rtl,
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

/// 定版式 EPUB：把 spine 的每一项变成页表里的一页。
///
/// 两种页都认（真实漫画与绘本两种写法都有）：
/// - **spine 项本身就是一张图**（`media-type` 是 `image/*`，常见的是逐页 SVG）；
/// - **spine 项是一页 XHTML，里面包一张图**（`<svg><image xlink:href="…"/></svg>` 或
///   `<img src="…"/>`）—— 取第一个图片引用当这一页。
///
/// 这一支不做排版、不建章：`Content::Fixed` 的页表就是它的全部（与 CBZ 同族）。
fn read_fixed(
    path: &Path,
    zip: &mut Zip,
    opf: &Opf,
    by_id: &HashMap<&str, &Item>,
    base_dir: &str,
) -> Result<(DocMeta, Content)> {
    let mut pages: Vec<FixedPage> = Vec::new();
    let mut images: Vec<ImageRes> = Vec::new();
    let mut cover: Option<Vec<u8>> = None;

    for node in &opf.spine {
        // `linear="no"` 与可重排同一条：非线性内容不当一页。
        if !node.linear {
            continue;
        }
        let Some(item) = by_id.get(node.id.as_str()) else {
            continue;
        };
        // SMIL 媒体叠加文档（音频+文本同步）不是内容页：跳过，不当一页、也不留空页。
        if item.media_type.contains("smil") {
            continue;
        }
        // 这一页的全部图（多图页 / 对开跨页）：第一张当主图，其余进 `extra_images`。
        let mut page_images: Vec<PageImage> = Vec::new();
        for (target, media_type) in fixed_page_refs(zip, base_dir, item) {
            let Some(bytes) = zip.read(&target) else {
                continue;
            };
            let (width, height) = if media_type.contains("svg") {
                super::svg::intrinsic_size(&bytes).unwrap_or((0, 0))
            } else {
                image_dims(&bytes)
            };
            if cover.is_none() {
                cover = Some(match shrink_image(&bytes, COVER_MAX_EDGE) {
                    Some((small, _)) => small,
                    None => bytes.clone(),
                });
            }
            images.push(ImageRes {
                href: target.clone(),
                source: target.clone(),
                media_type,
                width,
                height,
            });
            page_images.push(PageImage {
                href: target,
                width,
                height,
            });
        }
        let Some(main) = page_images.first().cloned() else {
            continue;
        };
        let (width, height) = (main.width, main.height);
        pages.push(FixedPage {
            source: PageSource::Image { href: main.href },
            width,
            height,
            // 主图之外的图；单图页时为空。
            extra_images: page_images.split_off(1),
        });
    }

    if pages.is_empty() {
        return Err(unsupported(path, "定版式 EPUB 一页都取不出来"));
    }
    let (width, height) = (pages[0].width, pages[0].height);
    let meta = DocMeta {
        title: if opf.title.is_empty() {
            stem_of(path)
        } else {
            opf.title.clone()
        },
        authors: opf.authors.clone(),
        contributors: opf.contributors.clone(),
        subjects: opf.subjects.clone(),
        description: opf.description.clone(),
        publisher: opf.publisher.clone(),
        published: opf.published.clone(),
        language: opf.language.clone(),
        isbn: opf.isbn.clone(),
        dctype: opf.dctype.clone(),
        rights: opf.rights.clone(),
        series: opf.series.clone(),
        format: Format::Epub,
        size: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
    };
    let fixed = FixedDoc {
        pages,
        toc: Vec::new(),
        cover,
        images,
        width,
        height,
    };
    tracing::debug!(
        "epub: {} 是定版式，读到 {} 页",
        path.display(),
        fixed.page_count()
    );
    Ok((meta, Content::Fixed(fixed)))
}

/// 一个 spine 项对应这一页的**全部**图（容器绝对路径 + media-type，按出现顺序）。
///
/// - `media-type` 是 `image/*`：这一项本身就是图（常见的是逐页 SVG），一张。
/// - 是一页 XHTML：取里面**所有**图片引用（单图漫画一张，对开跨页两张）。
/// - 其余（SMIL、音频等）：空 —— 调用方跳过这一项。
fn fixed_page_refs(zip: &mut Zip, base_dir: &str, item: &Item) -> Vec<(String, String)> {
    let name = resolve(base_dir, &item.href);
    if item.media_type.starts_with("image/") {
        return match zip.locate(&name) {
            Some(found) => vec![(found, item.media_type.clone())],
            None => Vec::new(),
        };
    }
    if item.media_type.contains("html") {
        let Some(found) = zip.locate(&name) else {
            return Vec::new();
        };
        let Some(text) = zip.read_text(&found) else {
            return Vec::new();
        };
        let dir = dir_of(&found);
        return all_image_refs(&text)
            .into_iter()
            .filter_map(|reference| {
                let target = resolve(&dir, &reference);
                let located = zip.locate(&target)?;
                let ext = extension_of(Path::new(&located)).unwrap_or_default();
                Some((located, super::media_type_of(&ext).to_string()))
            })
            .collect();
    }
    Vec::new()
}

/// 一页 XHTML 里**所有**图片引用的 href（`<image xlink:href="…">` 与 `<img src="…">`）。
///
/// 轻扫描，不解析 DOM：定版式的页 XHTML 就是「若干张图」，解析一棵完整 DOM 不值。
/// 认不出就空（那一页跳过）。
fn all_image_refs(xhtml: &str) -> Vec<String> {
    let mut out = Vec::new();
    for marker in ["<image", "<img"] {
        let mut haystack = xhtml;
        while let Some(at) = haystack.find(marker) {
            let rest = &haystack[at..];
            let Some(end) = rest.find('>') else {
                break;
            };
            let tag = &rest[..end];
            for key in ["xlink:href", "href", "src"] {
                if let Some(value) = attr_in_tag(tag, key)
                    && !value.is_empty()
                    && !value.starts_with("http")
                {
                    let owned = value.to_string();
                    if !out.contains(&owned) {
                        out.push(owned);
                    }
                    break;
                }
            }
            haystack = &haystack[at + marker.len()..];
        }
    }
    out
}

/// 从一个标签串里取属性值（双/单引号，属性名大小写不敏感）。
fn attr_in_tag<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let lower = tag.to_ascii_lowercase();
    let at = lower.find(&name.to_ascii_lowercase())?;
    // 属性名后要紧跟 `=`（跳过空白）；避免 `href` 命中 `xlink:href` 之外的词。
    let after = &tag[at + name.len()..];
    let after = after.trim_start();
    let after = after.strip_prefix('=')?.trim_start();
    let quote = after.chars().next()?;
    if quote == '"' || quote == '\'' {
        let rest = &after[1..];
        let end = rest.find(quote)?;
        Some(&rest[..end])
    } else {
        let end = after
            .find(|c: char| c.is_whitespace() || c == '>')
            .unwrap_or(after.len());
        Some(&after[..end])
    }
}

/// 目录文档（nav 或 NCX）里的 href 相对**它自己**，统一换成容器绝对路径。
///
/// `#` 后面那一截要在 `resolve_href` 剥掉它之前先取下来存进 `fragment`：剥掉之后只剩
/// 「哪一章」，而 nav 里最常见的形态恰恰是同一个章文件挂着一串不同小节的锚点。
///
/// **按目录文档解出来那条不在容器里时，再按 OPF 的基准目录解一次。** 规范说链接相对目录
/// 文档自己，出版工具却常常按 OPF 那一层写 —— nav 放进 `Nav/` 子目录，href 仍写
/// `Text/chap1.xhtml`。按规范解出来的结果是容器里根本没有的一个名字，症状是目录那一栏
/// 每一行都点不动，而 `toc_by_href` 也配不上标题，于是章节标题回落成「第 N 节」。
/// 回落到 OPF 目录解一次就命中。**只在头一次解不出时才回落**：`../Text/chap1.xhtml`
/// 那种规范写法两条路都解得出，留下的仍是规范那一条。
fn rebase(entry: TocEntry, doc_dir: &str, opf_dir: &str, zip: &Zip) -> TocEntry {
    // 纯容器条目（只有标题、位置是从子孙回填来的）在回填失败时留空：空的就该一直空，
    // 拿它去 `resolve` 会得到基准目录本身，于是一行「跳不到任何地方」的目录项被当成位置。
    let (href, fragment) = if entry.href.is_empty() {
        (String::new(), None)
    } else {
        let at = resolve(doc_dir, &entry.href);
        let href = if zip.locate(&at).is_some() {
            at
        } else {
            resolve(opf_dir, &entry.href)
        };
        (href, fragment_of(&entry.href))
    };
    TocEntry {
        title: entry.title,
        href,
        fragment,
        children: entry
            .children
            .into_iter()
            .map(|c| rebase(c, doc_dir, opf_dir, zip))
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
