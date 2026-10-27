//! MOBI / AZW / AZW3 这一族的格式知识。
//!
//! 容器字节不在这里：PalmDB 的记录寻址在 [`super::palmdb`]，这一族只管「记录里的字节
//! 是什么意思」。分家的理由是 `plans/导入管线.md` 第十节那一条 —— 容器家族比格式少，
//! 寻址只该写一份。
//!
//! | 模块 | 干什么 |
//! | --- | --- |
//! | [`codec`] | PalmDOC 与 HUFF/CDIC 两种解压，加上记录尾部 `extra_flags` 那段的剥除 |
//! | [`indx`] | INDX / TAGX / vwi / CNCX 一套索引机制（NCX 目录 + KF8 的 SKEL/FRAG） |
//! | [`encoding`] | 头里 `text encoding`（记录 0 偏移 28）那三档解码 |
//! | [`header`] | 录 0 的 PalmDOC 头 + MOBI 头 + EXTH |
//! | [`rawml`] | 解压 + FDST 切段 + SKEL/FRAG 重建 + 扫资源 |
//!
//! 本文件是**装配层**：把上面几块接成一个 provider，并把 MOBI 专有的正文标记
//! （`recindex="N"` 的图、`kindle:embed:`/`kindle:flow:` 引用、`<mbp:…>` 标签）
//! 改写成引擎认的 XHTML。
//!
//! ## KF7 / KF8 与 hybrid
//!
//! - **KF7**（老 mobi，版本 < 8）：整条正文流是一条 HTML，章按 `<mbp:pagebreak/>` 切。
//! - **KF8**（AZW3，版本 ≥ 8）：正文由 SKEL/FRAG 拼回多个 markup 文件，一文件一章。
//! - **hybrid**：一个文件里两半都有 —— EXTH 121 的边界（**要减一**）那条记录以
//!   `BOUNDARY` 开头，KF8 的第二个录 0 紧随其后。有 KF8 半边就用它（读者要的是新排版），
//!   所有索引字段与首条正文记录都加 `kf8_offset = boundary + 1`，**只有 `image_index`
//!   不加**（hybrid 的图共用 KF7 头里那一格）。
//!
//! ## 位置形状
//!
//! KC8 没有条目名、KF7 也没有 —— 所以这一族走 [`ChapterLocator::Section`]：库里只存
//! 「第几章」，打开时**重解一遍**再取那一章（与 FB2 / RTF 同一条路）。
//!
//! 那一趟的价钱就是「解完整本」，所以按「这一趟要产出什么」分了三条路，共用同一段装配
//! （开档 → 解压 → 重建 → 切章 → 改写）：
//!
//! | 谁在调 | 要什么 | 不算什么 |
//! | --- | --- | --- |
//! | `index`（导入） | 章表 + 每章字数 + 封面 / 图片 / CSS | 正文改写（字数不看属性） |
//! | `read`（测试与校验） | 全部 | —— |
//! | `read_texts`（开书 memo） | 只有每章正文 | 封面、图片尺寸、CSS、每章字数 |
//!
//! 实测（release、热页缓存、每档取 3 轮最小值；Linux 桌面一台）：
//! 合成 600 章 / 驻留 405 KB 那一本，`index` 1.9 ms、`read` 1.8 ms、取一章 1.2 ms；
//! 由真中文小说正文包的 2.7 MB / 122 章那一本，`index` 26 ms、`read_texts` 13 ms；
//! 5 MB / 13588 章那一本（libmobi 的词典样本，形状与「章极多的中文书」同档），
//! `index` 88 ms、`read_texts` 53 ms。
//!
//! 所以翻章不许每章都付那一趟：第一次解完就由 `BookDocument` 的整本 memo 收下
//! （`content::document::BookDocument::section_texts`），之后取章是一次下标取值；
//! 驻留是整本改写后的 XHTML（600 章实测 405 KB），关书即走、不落库
//! （口径见 `plans/text-out-of-db.md`）。换来的仍是「不改 locator 形状、不动库 schema」。
//!
//! ## 图片地址
//!
//! MOBI 的图**没有文件名**，只有记录号。正文里 `recindex="N"`（KF7）或
//! `kindle:embed:NNNN`（KF8）按 `记录号 = first_resource + N - 1` 定位。我们把它改写成
//! `<img src="mobi-img/<记录号>"/>`，`image_bytes` 再按记录号回容器取字节 —— 与
//! FB2 用 `<binary>` 的 id 当 href 是同一条思路（href 只是一个键，不是文件名）。

// `codec` 与 `encoding` 以前也压着这一条（理由是「装配那一步由下一轮接上」），
// 那句已经过期：`MobiAdapter` 早在注册表里了。撤掉这两个 `allow` 实测**一条告警都不冒**，
// 所以它们是压空气，删了。
pub(crate) mod codec;
mod encoding;
// 下面三个仍有真东西压着（撤掉就红，逐条列在这里，别当「以后会用」的挡箭牌）：
//   header —— `text_record_size` / `exth_flags` / `fdst_section_count` / `guide_index`
//             四格没人读，`has_exth()` 没人调（PalmDOC 头那张表是按规范整张抄的，
//             抄全才好对拍；要删就删干净，别留一个 `allow` 遮着）
//   indx    —— `Index::from_entries`
//   rawml   —— `Kind::Html` 这一支没人构造（KF7 的分岔还没走到它）、`split_kf7`
#[allow(dead_code)]
mod header;
#[allow(dead_code)]
mod indx;
#[allow(dead_code)]
mod rawml;

pub(crate) use encoding::decode_text;

use std::path::Path;

use header::{
    EXTH_AUTHOR, EXTH_CONTRIBUTOR, EXTH_COVEROFFSET, EXTH_DESCRIPTION, EXTH_ISBN, EXTH_KF8BOUNDARY,
    EXTH_LANGUAGE, EXTH_PUBLISHER, EXTH_PUBLISHINGDATE, EXTH_RESCOFFSET, EXTH_RIGHTS, EXTH_SUBJECT,
    EXTH_TITLE, EXTH_TYPE, EXTH_UPDATEDTITLE, Header, NOTSET,
};
use rawml::Records;

use super::epub::IMAGE_HEAD_BYTES;
use super::palmdb::PalmDb;
use super::{
    Chapter, ChapterLocator, Content, DocMeta, DocumentProvider, FlowDoc, FontBytes, Format,
    HeadDims, ImageRes, image_dims, image_dims_from_head, shrink_image, stem_of,
};
use crate::Result;

/// 封面上限与 EPUB / FB2 同一条口径。
const COVER_MAX_EDGE: u32 = 512;

/// 图片 href 的前缀（见模块头「图片地址」）。
const IMAGE_PREFIX: &str = "mobi-img/";
/// SVG flow 段的前缀（KF8 的 `kindle:flow` svg）。
const SVG_PREFIX: &str = "mobi-svg/";
/// 内嵌字体的前缀。
const FONT_PREFIX: &str = "mobi-font/";

pub struct MobiAdapter;

impl DocumentProvider for MobiAdapter {
    fn format(&self) -> Format {
        Format::Mobi
    }

    fn supports(&self, path: &Path) -> bool {
        matches!(
            super::extension_of(path).as_deref(),
            Some("mobi" | "azw" | "azw3" | "prc")
        )
    }

    /// 只读索引：MOBI 没有独立索引 —— 要知道章数与字数就得解压全文。所以这一趟仍然
    /// 解压、重建、算 `char_len`，只把每章的 source 换成位置（[`ChapterLocator::Section`]）。
    fn index(&self, path: &Path) -> Result<(DocMeta, Content)> {
        read_with(path, false)
    }

    /// 全文那一趟（索引 + 每一章正文）。给测试与「我就是要全文」的调用方。
    fn read(&self, path: &Path) -> Result<(DocMeta, Content)> {
        read_with(path, true)
    }

    /// 开书 memo 那一趟：只要每一章改写后的正文。
    ///
    /// 与 `read_with(path, true)` 共用同一段装配（`shape` + `rewrite_chapters`），所以
    /// 两条路交出的串**逐字节相同**（`read_texts_matches_the_full_pass_chapter_for_chapter`
    /// 钉着这件事）；差别只在不算那几格没人看的：封面（实测一张 731×817 解码 + 缩 + 重编
    /// 要 29 ms）、每张插图整条读回来量尺寸、CSS 改写、逐章可见字数。
    fn read_texts(&self, path: &Path) -> Option<Vec<String>> {
        texts_of(path).ok()
    }

    /// 单文件容器里的第 N 章：重解一遍取第 N 章（与 FB2 / RTF 同一条路）。
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
        // 这一路是 memo 建不起来时的兜底：每一章都要重解一遍整本，就更不该顺手缩一次
        // 封面、再逐章数一遍字数（实测 2.7 MB 那一本这两格是 21 ms）。
        let texts = texts_of(path)?;
        texts
            .into_iter()
            .nth(index)
            .ok_or_else(|| crate::Error::ChapterUnavailable {
                path: path.to_path_buf(),
                index,
                at: format!("{at:?}"),
            })
    }

    /// 按记录号（或 svg 段号）取图片字节。
    fn image_bytes(&self, path: &Path, href: &str) -> Option<Vec<u8>> {
        let db = PalmDb::open(path).ok()?;
        if let Some(record) = href.strip_prefix(IMAGE_PREFIX).and_then(parse_index) {
            return db.read(record);
        }
        // SVG flow 段：重跑重建取那一份（罕见，付一次解压）。KF8 的逐页 SVG 才走这里。
        if let Some(part) = href.strip_prefix(SVG_PREFIX).and_then(parse_index) {
            let opened = open(path).ok()?;
            let read = rawml::palm_records(&opened.db);
            let heads = rawml::palm_heads(&opened.db);
            let rec = rawml::reconstruct(
                &read,
                &heads,
                &opened.header,
                opened.kf8_offset,
                opened.first_resource,
            )?;
            return rec
                .flow_svgs
                .into_iter()
                .find(|(n, _)| *n == part)
                .map(|(_, bytes)| bytes);
        }
        None
    }

    /// `@font-face` **点名要的那几份**内嵌字体：解 FONT 记录（zlib + xor 去混淆）。
    ///
    /// 按 `wanted` 过滤与 EPUB 同一条口径（`epub.rs::font_bytes`）：一本 AZW3 常带七八份
    /// FONT 记录，而 CSS 里声明的只有一两份；全取等于把没被点名的那几份也解压、拷成
    /// `Vec<u8>`，交上去还在 `book_fonts.rs` 那道 `declared_of` 查表里被丢掉。
    ///
    /// 比的两侧**必须同形**：`wanted` 是 CSS `src:` 解出来的 href，而 `rewrite_css` 把
    /// `kindle:embed:N` 改写成 `mobi-font/<记录号>` 就是为了让它和这里拼出来的串一字不差。
    /// 两边写成不同的前缀，表现就是「内嵌字体静悄悄不生效」。
    fn fonts(&self, path: &Path, wanted: &[String]) -> Vec<FontBytes> {
        if wanted.is_empty() {
            return Vec::new();
        }
        let Ok(opened) = open(path) else {
            return Vec::new();
        };
        // 加密的书在 `read_with` 那一趟就说「读不动」，这里也不该从资源记录里吐出字节。
        if opened.header.encryption_type != 0 {
            return Vec::new();
        }
        // 这一趟只要「哪几条记录是 FONT」。从前它走 `reconstruct`：为了一张记录表
        // 把整本正文解压 + 重建一遍，实测 2.7 MB 那本 9.2 ms、5 MB 那本 38.9 ms。
        let heads = rawml::palm_heads(&opened.db);
        let resources = rawml::list_resources(&heads, opened.first_resource);
        resources
            .iter()
            .filter(|r| r.kind == rawml::ResourceKind::Font)
            .map(|r| (format!("{FONT_PREFIX}{}", r.record), r.record))
            .filter(|(href, _)| wanted.iter().any(|w| w == href))
            .filter_map(|(href, record)| {
                let raw = opened.db.read(record)?;
                let bytes = decode_font(&raw)?;
                Some(FontBytes {
                    href,
                    media_type: "application/x-font".to_string(),
                    crc32: crc32fast::hash(&bytes),
                    raw_len: bytes.len() as u64,
                    bytes,
                })
            })
            .collect()
    }
}

/// 开一本书：容器 + 选定的头（hybrid 时是 KF8 半边）+ 偏移 + 首个资源记录号。
struct Opened {
    db: PalmDb,
    header: Header,
    /// hybrid 的 KF8 半边偏移（非 hybrid 是 0）。
    kf8_offset: usize,
    first_resource: Option<usize>,
}

/// 打开并解出头，判定 hybrid。
fn open(path: &Path) -> Result<Opened> {
    let db = PalmDb::open(path)?;
    // PalmDB 类型不是 `BOOK` / `TEXt` 的（`DATA`、词典那些）不该当书读。
    let kind = db.db_type();
    if kind != b"BOOK" && kind != b"TEXt" {
        return Err(crate::Error::UnsupportedFormat(format!(
            "PalmDB 类型 {:?} 不是书",
            String::from_utf8_lossy(kind)
        )));
    }
    let rec0 = db
        .read(0)
        .ok_or_else(|| crate::Error::UnsupportedFormat("MOBI 缺少录 0".to_string()))?;
    let base = Header::parse(&rec0)?;

    // hybrid：EXTH 121 的值减一那条记录要以 BOUNDARY 开头。
    let boundary = base
        .exth_uint(EXTH_KF8BOUNDARY)
        .and_then(|n| n.checked_sub(1))
        .map(|n| n as usize)
        .filter(|&b| db.read(b).is_some_and(|r| r.starts_with(b"BOUNDARY")));

    let (header, kf8_offset) = match boundary {
        Some(b) => {
            let at = b + 1;
            match db.read(at).and_then(|r| Header::parse(&r).ok()) {
                Some(kf8) => (kf8, at),
                // KF8 半边解不出：退回 KF7（老半边能读就行）。
                None => (base.clone(), 0),
            }
        }
        None => (base.clone(), 0),
    };

    // hybrid 的图片共用 KF7 头里那一格 `image_index`；非 hybrid 时 base 就是本头。
    let first_resource = (base.image_index != NOTSET)
        .then_some(base.image_index as usize)
        .or_else(|| base.exth_uint(EXTH_RESCOFFSET).map(|v| v as usize))
        .or_else(|| header.exth_uint(EXTH_RESCOFFSET).map(|v| v as usize));

    Ok(Opened {
        db,
        header,
        kf8_offset,
        first_resource,
    })
}

/// 切好章、还没改写的那一本书。三条装配路（导入 / 全文 / 开书 memo）共用这一段，
/// 所以「哪一本算读不动」那两道门与「章怎么切」只此一份 —— 各写一份的下场是
//「导入拒了的书，开书却给了正文」这种两边说不通。
struct Shaped {
    opened: Opened,
    /// 资源表、CSS、svg flow 段（`parts` 搬走了 markup，其余都还在）。
    rec: rawml::Reconstructed,
    /// 每一章的标题。NCX 查不到的那一格是**空串**（与改写前同：占位名由 `title_at` 兜）。
    titles: Vec<String>,
    /// 每一章**未改写**的正文。
    parts: Vec<String>,
}

/// 开档 → 两道门（DRM / 码页）→ 解压 → 重建 → 切章。
fn shape(path: &Path) -> Result<Shaped> {
    let opened = open(path)?;
    let header = &opened.header;

    // DRM：加过密的正文**永远**解不开（不是「这一版还没写」），单列一类。
    if header.encryption_type != 0 {
        return Err(crate::Error::Encrypted {
            name: stem_of(path),
            method: format!("mobipocket-encryption-{}", header.encryption_type),
        });
    }
    // 码页只认 cp1252 / UTF-8 / UTF-16；GBK 那类要报「读不动」而不是猜成 UTF-8。
    if header.encoding != 0 && !encoding::is_supported(header.encoding) {
        return Err(crate::Error::UnsupportedFormat(format!(
            "MOBI 正文码页 {} 这一版读不动",
            header.encoding
        )));
    }

    // 两条取字节的回调只是 `opened.db` 的借用，所以它们必须在这个块里结清 ——
    // 块外的 `opened` 要整个搬进 `Shaped` 交出去。
    let (rec, titles, parts) = {
        let read = rawml::palm_records(&opened.db);
        let heads = rawml::palm_heads(&opened.db);
        let mut rec = rawml::reconstruct(
            &read,
            &heads,
            header,
            opened.kf8_offset,
            opened.first_resource,
        )
        .ok_or_else(|| {
            crate::Error::UnsupportedFormat(
                "MOBI 正文解压失败（记录或 HUFF/CDIC 表坏了）".to_string(),
            )
        })?;
        let (titles, parts) = chapter_shapes(&read, &mut rec, header, opened.kf8_offset);
        (rec, titles, parts)
    };
    Ok(Shaped {
        opened,
        rec,
        titles,
        parts,
    })
}

/// 索引 / 全文共用的一趟。
fn read_with(path: &Path, with_text: bool) -> Result<(DocMeta, Content)> {
    let shaped = shape(path)?;
    let opened = &shaped.opened;
    let header = &opened.header;
    let rec = &shaped.rec;
    let titles = &shaped.titles;
    let parts = &shaped.parts;
    let is_kf8 = header.is_kf8();
    let first_resource = opened.first_resource;

    // 正文是不是这一趟的产物，决定要不要改写：
    // - 全文那一趟：每章交改写后的 XHTML。
    // - 索引那一趟（导入走的）：只要「这一章有几个可见字」。改写动的是属性里的 href
    //   与空标签的壳，**文字节点一格都不动**，所以两条路的字数逐章相同（对拍见
    //   `the_index_pass_counts_the_same_visible_chars`）—— 省下的是 2.7 MB 那本实测
    //   6.8 ms 的改写 + 一章一次 `String` 分配。
    let mut chapters: Vec<Chapter> = if with_text {
        rewrite_chapters(parts, first_resource, is_kf8)
            .into_iter()
            .enumerate()
            .map(|(i, source)| Chapter::inline(title_at(titles, i), source))
            .collect()
    } else {
        parts
            .iter()
            .enumerate()
            .map(|(i, part)| {
                let summary = crate::content::scan::chapter_summary(part);
                Chapter::section(title_at(titles, i), i).with_char_len(Some(summary.visible_chars))
            })
            .collect()
    };
    // 一章都没有时留一章空的：让「打开了一本没内容的书」与「打不开」分开。
    if chapters.is_empty() {
        let placeholder = Chapter::inline(stem_of(path), "<body></body>".to_string());
        chapters.push(if with_text {
            placeholder
        } else {
            Chapter::section(placeholder.title.clone(), 0).with_char_len(placeholder.char_len())
        });
    }

    // 图片元数据：记录资源 + KF8 的 svg flow 段。
    let mut images: Vec<ImageRes> = Vec::new();
    for r in &rec.resources {
        if r.kind != rawml::ResourceKind::Image {
            continue;
        }
        let href = format!("{IMAGE_PREFIX}{}", r.record);
        let (width, height) = image_size(&opened.db, r.record);
        images.push(ImageRes {
            href: href.clone(),
            source: href,
            media_type: r.media_type.to_string(),
            width,
            height,
        });
    }
    for (part, bytes) in &rec.flow_svgs {
        let href = format!("{SVG_PREFIX}{part}");
        let (width, height) = super::svg::intrinsic_size(bytes).unwrap_or((0, 0));
        images.push(ImageRes {
            href: href.clone(),
            source: href,
            media_type: "image/svg+xml".to_string(),
            width,
            height,
        });
    }

    // 封面：EXTH 201 的偏移 → `first_resource + offset` 那条记录。
    let cover = cover_bytes(opened);

    let title = book_title(header, path);
    // MOBI 的书目区就是 OPF 的扁平版：哪条 EXTH 标签对应哪一条 `dc:*` 写在
    // [`header`] 那些常数上（出处是 libmobi 的 `src/mobi.h`，逐条抄下来的）。
    //
    // 两格这一路拿不到，都写在这里而不是留空让人猜：
    // - **系列**：Kindle 的 collection 记在设备/边车里，不在书本文件内，所以恒 `None`。
    // - **语言的另一条来路**：MOBI 头偏移 92 那个打包字节也表语言，但要 libmobi 那两张
    //   92 项的表才解得成 `zh-CN`。为这一格引一份表不划算，所以只认 EXTH 524 那一条字符串。
    let meta = DocMeta {
        title,
        authors: header.exth_strings(EXTH_AUTHOR),
        contributors: header.exth_strings(EXTH_CONTRIBUTOR),
        subjects: header.exth_strings(EXTH_SUBJECT),
        description: header.exth_string(EXTH_DESCRIPTION).unwrap_or_default(),
        publisher: header.exth_string(EXTH_PUBLISHER).unwrap_or_default(),
        published: header.exth_string(EXTH_PUBLISHINGDATE).unwrap_or_default(),
        language: header.exth_string(EXTH_LANGUAGE).unwrap_or_default(),
        isbn: header.exth_string(EXTH_ISBN).unwrap_or_default(),
        dctype: header.exth_string(EXTH_TYPE).unwrap_or_default(),
        rights: header.exth_string(EXTH_RIGHTS).unwrap_or_default(),
        series: None,
        format: Format::Mobi,
        size: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
    };
    let flow = FlowDoc {
        chapters,
        // 作者样式只从 KF8 的 css flow 段来；KF7 为空。逐章的 `<link kindle:flow>` 已被
        // `rewrite_html` 去掉，样式在这里统一给（与 EPUB 的 `css` 同一条路）。
        // 里面的 `kindle:embed` 引用（内嵌字体与背景图）按资源类型改写成我们的 href ——
        // 不改写的话 `@font-face` 的 src 与 `fonts()` 对不上，内嵌字体永远注册不上。
        css: rewrite_css(&rec.css, &rec.resources, opened.first_resource),
        css_hrefs: Vec::new(),
        toc: Vec::new(),
        cover,
        images,
        progression_rtl: false,
        cover_ref: None,
    };
    tracing::debug!(
        "mobi: {} 读到 {} 章、{} 张图、CSS {} 字节、封面 {}",
        path.display(),
        flow.chapters.len(),
        flow.images.len(),
        flow.css.len(),
        flow.cover.as_ref().map_or("无", |_| "有"),
    );
    Ok((meta, Content::Reflow(flow)))
}

/// 「只要每一章正文」那一趟。`read_texts` 与 `chapter_source` 共用：一个把它折成
/// `Option`（读不出就是没有），一个要把「为什么读不出」原话说出来。
///
/// 与 `read_with(path, true)` 的差别**只有不算没人看的那几格**，章的切法与改写走的
/// 是同一段代码，所以每一串都逐字节相同。
fn texts_of(path: &Path) -> Result<Vec<String>> {
    let shaped = shape(path)?;
    let mut texts = rewrite_chapters(
        &shaped.parts,
        shaped.opened.first_resource,
        shaped.opened.header.is_kf8(),
    );
    // 与 `read_with` 同一条「空书也要有一章」的口径：界面上「没内容的书」与
    // 「打不开」得是两件事，那一格 placeholder 的正文在这里也得是同一串。
    if texts.is_empty() {
        texts.push("<body></body>".to_string());
    }
    Ok(texts)
}

/// 章的形状：每一章的**标题**与**未改写**的正文。
///
/// KF8 是一个 markup 文件一章（SKEL/FRAG 已经切好），KF7 是一条流按 `<mbp:pagebreak/>` 切。
/// 三条装配路（导入 / 全文 / 开书 memo）共用这一份切法，免得「导入看到的章」与
/// 「开书看到的章」不是同一件事。
///
/// `rec.markup` 是**搬走**的：KF8 的 markup 本身就是那一章的正文，再拷一份没有意义。
fn chapter_shapes(
    read: Records<'_>,
    rec: &mut rawml::Reconstructed,
    header: &Header,
    kf8_offset: usize,
) -> (Vec<String>, Vec<String>) {
    let markups = std::mem::take(&mut rec.markup);
    if header.is_kf8() {
        let titles = kf8_titles(read, header, kf8_offset, markups.len());
        return (titles, markups);
    }
    let Some(first) = markups.first() else {
        return (Vec::new(), Vec::new());
    };
    let ranges = rawml::split_kf7_ranges(first);
    let titles = kf7_titles(read, header, kf8_offset, &ranges);
    (titles, ranges.into_iter().map(|(_, part)| part).collect())
}

/// 第 `i` 章的标题。NCX 没给名字的那几章用「第 N 节」占位。
///
/// 只在**下标越界**时占位：NCX 里查到了但那一格是空串的，仍然交空串（与改写前逐字同，
/// 那里 `titles.get(i).cloned()` 给的是 `Some("")`）。
fn title_at(titles: &[String], i: usize) -> String {
    titles
        .get(i)
        .cloned()
        .unwrap_or_else(|| format!("第 {} 节", i + 1))
}

/// 逐章改写成引擎认的 XHTML（全文那一趟与开书 memo 共用这一条）。
fn rewrite_chapters(parts: &[String], first_resource: Option<usize>, is_kf8: bool) -> Vec<String> {
    // KF8 才有 `kindle:pos:fid`：跨章的锚点要查**别的章**，所以先把每章的锚点表扫出来。
    // KF7 那一族没有这一格，`ids` 给空表（与改写前同）。
    let ids: Vec<Vec<(usize, String)>> = if is_kf8 {
        parts.iter().map(|html| id_positions(html)).collect()
    } else {
        Vec::new()
    };
    parts
        .iter()
        .enumerate()
        .map(|(i, part)| {
            let ctx = Rewrite {
                first_resource,
                ids: &ids,
                chapter: i,
            };
            rewrite_html(part, &ctx)
        })
        .collect()
}

/// 一张图进清单：只要固有像素尺寸，所以**只读前缀**，不把整条记录扛回来。
///
/// 三档结局与 EPUB 那一路同一条口径（见 [`HeadDims`] 与 `epub::parse_image`）：前缀里就
/// 定得出尺寸 → 完事；认不出是解得动的位图 → 到此为止，多读也没用；是位图但尺寸头排在
/// 更后面（带大 EXIF 缩略图的 JPEG）→ 才补一次整条读。前缀的字节数共用 EPUB 那个数。
fn image_size(db: &PalmDb, record: usize) -> (u32, u32) {
    let Some(head) = db.read_head(record, IMAGE_HEAD_BYTES as usize) else {
        return (0, 0);
    };
    match image_dims_from_head(&head) {
        HeadDims::Sized(dims) => dims,
        HeadDims::NotABitmap => (0, 0),
        HeadDims::NeedsMore => match db.read(record) {
            Some(bytes) => image_dims(&bytes),
            // 整条读不出来：留住「这张图存在，尺寸不知道」，比从清单里消失好。
            None => (0, 0),
        },
    }
}

/// 书名：EXTH 503（更新名）→ EXTH 99 → MOBI 头的 full_name → 文件名。
fn book_title(header: &Header, path: &Path) -> String {
    header
        .exth_string(EXTH_UPDATEDTITLE)
        .or_else(|| header.exth_string(EXTH_TITLE))
        .filter(|t| !t.trim().is_empty())
        .or_else(|| {
            let name = header.full_name.trim();
            (!name.is_empty()).then(|| name.to_string())
        })
        .unwrap_or_else(|| stem_of(path))
}

/// KF8 的章标题：NCX 里 `tag 6.0 = 骨架号` 的那条 → 标题。
fn kf8_titles(read: Records<'_>, header: &Header, kf8_offset: usize, count: usize) -> Vec<String> {
    let mut titles = vec![String::new(); count];
    if header.ncx_index == NOTSET {
        return titles;
    }
    let index = indx::read_index(
        read,
        header.ncx_index as usize + kf8_offset,
        header.encoding,
    );
    for entry in &index.entries {
        let Some(fid) = entry.tag_value(6, 0) else {
            continue;
        };
        let title = entry
            .tag_value(3, 0)
            .and_then(|at| index.cncx_string(at))
            .unwrap_or_default();
        if let Some(slot) = titles.get_mut(fid as usize)
            && slot.is_empty()
            && !title.is_empty()
        {
            *slot = title.to_string();
        }
    }
    titles
}

/// KF7 的章标题：NCX 的 `filepos` 落在哪一段的字节区间里，就用哪条标题。
fn kf7_titles(
    read: Records<'_>,
    header: &Header,
    kf8_offset: usize,
    ranges: &[(usize, String)],
) -> Vec<String> {
    let mut titles = vec![String::new(); ranges.len()];
    if header.ncx_index == NOTSET {
        return titles;
    }
    let toc = indx::read_ncx(
        read,
        header.ncx_index as usize + kf8_offset,
        header.encoding,
    );
    for item in toc {
        let pos = item.filepos as usize;
        // 段落起点升序，找最后一个起点 ≤ pos 的那一段。
        let slot = ranges
            .iter()
            .enumerate()
            .rev()
            .find(|(_, (start, _))| *start <= pos)
            .map(|(i, _)| i);
        if let Some(i) = slot
            && titles[i].is_empty()
            && !item.title.is_empty()
        {
            titles[i] = item.title.clone();
        }
    }
    titles
}

/// 封面字节。
fn cover_bytes(opened: &Opened) -> Option<Vec<u8>> {
    let offset = opened
        .header
        .exth_uint(EXTH_COVEROFFSET)
        .or_else(|| opened.header.exth_uint(header::EXTH_THUMBOFFSET))?;
    let record = opened.first_resource? + offset as usize;
    let raw = opened.db.read(record)?;
    match shrink_image(&raw, COVER_MAX_EDGE) {
        Some((bytes, _)) => Some(bytes),
        None => Some(raw),
    }
}

/// 解一条 FONT 资源记录：magic 4 + 解出尺寸 4 + flags 4 + 数据偏移 4 + xor 键长 4 +
/// xor 键偏移 4。按 flags 去混淆（位 1）与 zlib 解压（位 0）。
fn decode_font(raw: &[u8]) -> Option<Vec<u8>> {
    /// 去混淆时最多覆盖的字节数（按 xor 键长度 ×52，与 libmobi 同口径）。
    const OBFUSCATED_BUFFER_COUNT: usize = 52;
    if raw.len() < 24 || raw.get(..4) != Some(b"FONT".as_slice()) {
        return None;
    }
    let decoded_size = u32_at(raw, 4)? as usize;
    let flags = u32_at(raw, 8)?;
    let data_offset = u32_at(raw, 12)? as usize;
    let xor_key_len = u32_at(raw, 16)? as usize;
    let xor_key_offset = u32_at(raw, 20)? as usize;
    if decoded_size == 0 || decoded_size > 50 * 1024 * 1024 || data_offset > raw.len() {
        return None;
    }
    let mut data = raw.to_vec();
    if flags & 2 != 0 && xor_key_len > 0 {
        if xor_key_offset + xor_key_len > data.len() {
            return None;
        }
        let key = data[xor_key_offset..xor_key_offset + xor_key_len].to_vec();
        let limit = xor_key_len * OBFUSCATED_BUFFER_COUNT;
        let mut i = 0;
        let mut at = data_offset;
        while at < data.len() && i < limit {
            data[at] ^= key[i % xor_key_len];
            at += 1;
            i += 1;
        }
    }
    let encoded = &data[data_offset..];
    if flags & 1 != 0 {
        use std::io::Read as _;
        let mut out = Vec::with_capacity(decoded_size);
        flate2::read::ZlibDecoder::new(encoded)
            .take(decoded_size as u64 + 1)
            .read_to_end(&mut out)
            .ok()?;
        (!out.is_empty()).then_some(out)
    } else {
        Some(encoded.to_vec())
    }
}

/// 改写一章正文时要用的上下文。
struct Rewrite<'a> {
    /// `recindex` / `kindle:embed` 的基准（`记录 = first_resource + N - 1`）。
    first_resource: Option<usize>,
    /// 每一章的 `(字节偏移, id)`，按偏移升序。只有 KF8 会填（KF7 没有 `kindle:pos:fid`）。
    ids: &'a [Vec<(usize, String)>],
    /// 正在改写的这一章的章号（判断 `kindle:pos:fid` 是章内还是跨章）。
    chapter: usize,
}

/// 把 MOBI 的正文标记改写成引擎认的 XHTML：
/// - `<img recindex="N">` / `src="kindle:embed:N"` → `<img src="mobi-img/<记录>"/>`；
/// - `src="kindle:flow:N"`（SVG 段）→ `<img src="mobi-svg/N"/>`；
/// - 保留标签里 `kindle:pos:fid:F:off:O` 的 href → 章内 `#id` / 跨章 `mobi-part/F#id`；
/// - `<link …>` 与 `<mbp:…>` / `<mbh:…>` / `<mobi:…>` 标签整条去掉。
fn rewrite_html(html: &str, ctx: &Rewrite) -> String {
    let mut out = String::with_capacity(html.len());
    let mut at = 0usize;
    // 标签**之间**的文字整段搬。从前这一趟一个字符 push 一次（2.7 MB 的正文 = 九十多万次
    // push），标签还要各配一个 `String`：实测整本改写 29 ms，其中真正要改的只有 `<img>`
    // 与带 `kindle:pos:fid` 的那几个。
    while let Some(rel) = html[at..].find('<') {
        let start = at + rel;
        out.push_str(&html[at..start]);
        match html[start..].find('>') {
            Some(end) => {
                rewrite_tag_into(&html[start..start + end + 1], ctx, &mut out);
                at = start + end + 1;
            }
            // 起了头却没有尾：那个 `<` 就是正文里的一个字符（与逐字符那一版同口径）。
            None => {
                out.push('<');
                at = start + 1;
            }
        }
    }
    out.push_str(&html[at..]);
    out
}

/// 整条去掉的那几族标签：MOBI 专有标记的前缀，与本来就没有正文的那三个空元素。
///
/// 样式已另收（`FlowDoc::css`），页分隔符在切章那一步已经用过了。
const DROPPED_TAG_PREFIXES: [&str; 3] = ["mbp:", "mbh:", "mobi:"];
const DROPPED_TAGS: [&str; 3] = ["link", "meta", "style"];

/// 单个标签改写后的样子，直接写进 `out`。
///
/// 「原样搬」那一支**一个分配都不起** —— 那是绝大多数标签走的分支。
fn rewrite_tag_into(tag: &str, ctx: &Rewrite, out: &mut String) {
    use std::fmt::Write as _;

    let name = tag_name(tag);
    if DROPPED_TAGS.iter().any(|t| name.eq_ignore_ascii_case(t))
        || DROPPED_TAG_PREFIXES.iter().any(|p| {
            // 逐字节**大小写无关**地比前缀：`<MBP:pagebreak/>` 这种全大写写法真在文件里
            // 出现（切章那一步也按大小写两种认）。多字节的标签名前四格一定是 ASCII 字节，
            // 折不动它，也就不会误判成这一族。
            let head = &name.as_bytes()[..p.len().min(name.len())];
            head.len() == p.len() && head.eq_ignore_ascii_case(p.as_bytes())
        })
    {
        return;
    }
    if !name.eq_ignore_ascii_case("img") && !name.eq_ignore_ascii_case("image") {
        // 其余标签原样保留，只把里面的 `kindle:pos:fid` 锚点改成可解析的 href。
        if tag.contains(POS_FID) {
            out.push_str(&rewrite_posfid(tag, ctx));
        } else {
            out.push_str(tag);
        }
        return;
    }
    // 图：先看 recindex（KF7），再看 src/href 里的 kindle:embed / kindle:flow（KF8）。
    let first_resource = ctx.first_resource;
    if let Some(record) = attr_value(tag, "recindex")
        .and_then(parse_index)
        .and_then(|n| first_resource.map(|base| base + n.saturating_sub(1)))
    {
        let _ = write!(out, "<img src=\"{IMAGE_PREFIX}{record}\"/>");
        return;
    }
    let src = attr_value(tag, "src").or_else(|| attr_value(tag, "xlink:href"));
    if let Some(value) = src {
        if let Some(rest) = value.strip_prefix("kindle:embed:") {
            let (num, _) = leading_digits(rest);
            if let (Some(n), Some(base)) = (num.parse::<usize>().ok(), first_resource) {
                let _ = write!(
                    out,
                    "<img src=\"{IMAGE_PREFIX}{}\"/>",
                    base + n.saturating_sub(1)
                );
                return;
            }
        }
        if let Some(rest) = value.strip_prefix("kindle:flow:") {
            // `kindle:flow:NNNN?mime=...`：段号取到 `?` 之前。
            let (num, _) = leading_digits(rest);
            if let Ok(n) = num.parse::<usize>() {
                let _ = write!(out, "<img src=\"{SVG_PREFIX}{n}\"/>");
            }
        }
    }
    // 指不到任何资源的图：丢掉（排版层会给一块占位，别留一个空 `<img>`）。
}

/// `kindle:pos:fid:` 这个前缀。
const POS_FID: &str = "kindle:pos:fid:";

/// 把标签里所有的 `kindle:pos:fid:…` 改写成可解析的锚点 href。
fn rewrite_posfid(tag: &str, ctx: &Rewrite) -> String {
    if !tag.contains(POS_FID) {
        return tag.to_string();
    }
    let mut out = String::with_capacity(tag.len());
    let mut rest = tag;
    while let Some(at) = rest.find(POS_FID) {
        out.push_str(&rest[..at]);
        let tail = &rest[at + POS_FID.len()..];
        match resolve_posfid(tail, ctx) {
            Some((target, consumed)) => {
                out.push_str(&target);
                rest = &tail[consumed..];
            }
            None => {
                out.push_str(POS_FID);
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// 解析 `kindle:pos:fid:F:off:O` 的尾段（`F`/`O` 是 base32），返回 `(改写后的 href, 吃掉多少字符)`。
///
/// `F` = 骨架号 = 我们的章号；`O` = 该章重建文本里的**字节偏移**。目标锚点 = 该章里
/// 字节位置 ≤ `O` 的**最后一个** `id` / `aid`（与 libmobi `mobi_get_id_by_posoff` 同口径）。
/// 同章给 `#id`，跨章给 `mobi-part/F#id`；该章没有可用 id 时退回章首 `mobi-part/F`。
fn resolve_posfid(tail: &str, ctx: &Rewrite) -> Option<(String, usize)> {
    let fid_str = tail.get(..4)?;
    if tail.get(4..9)? != ":off:" {
        return None;
    }
    let off_str = tail.get(9..19)?;
    let fid = decode_base32(fid_str)? as usize;
    let off = decode_base32(off_str)? as usize;
    let mut consumed = 19;
    // 尾部可能挂 `?…`（少见）：一并吃掉，别让它跟着 href 走。
    if tail[consumed..].starts_with('?') {
        let rest = &tail[consumed + 1..];
        let end = rest
            .find(|c: char| c == '"' || c == '\'' || c == ')' || c.is_whitespace())
            .unwrap_or(rest.len());
        consumed += 1 + end;
    }
    let id = ctx
        .ids
        .get(fid)
        .and_then(|list| last_id_at_or_before(list, off));
    let target = match (fid == ctx.chapter, id) {
        (true, Some(id)) => format!("#{id}"),
        (false, Some(id)) => format!("mobi-part/{fid}#{id}"),
        (_, None) => format!("mobi-part/{fid}"),
    };
    Some((target, consumed))
}

/// 升序表里最后一个偏移 ≤ `off` 的 id。
///
/// 表是升序的（[`id_positions`] 排过），所以二分。从前是 `rev().find()`：一条挂着几百个
/// 脚注的章里，每一次跳转都要从表尾扫回来。
fn last_id_at_or_before(list: &[(usize, String)], off: usize) -> Option<&str> {
    let at = list
        .partition_point(|(pos, _)| *pos <= off)
        .checked_sub(1)?;
    Some(list[at].1.as_str())
}

/// 一章正文里所有 `id="…"` / `aid="…"` 的 `(字节偏移, id)`，按偏移升序。
///
/// 两条针各自用 `find` 走（std 的 `find` 里面是 memchr），而不是逐字节试 `starts_with`：
/// 后者实测扫 2.7 MB 的正文要 47 ms，而那本书**一个锚点都没有**。
///
/// `aid="` 里也含一个 `id="`，靠**前一个字节**的边界规则分开：前面是 `a` 的那一次不算
/// `id=`，它由 `aid=` 那一趟收走。两条趟各自跳过一个完整属性，所以产出的表与逐字节
/// 那一版逐格相同。
fn id_positions(html: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    collect_anchors(html, "id=\"", &mut out);
    collect_anchors(html, "aid=\"", &mut out);
    out.sort_unstable_by_key(|(pos, _)| *pos);
    out
}

/// 按一条针扫 `html`，把**站在属性名开头**的那些命中连同取值收进 `out`。
fn collect_anchors(html: &str, needle: &str, out: &mut Vec<(usize, String)>) {
    let bytes = html.as_bytes();
    let mut at = 0usize;
    while let Some(rel) = html[at..].find(needle) {
        let start = at + rel;
        // 先按「不是边界」往前挪一格，别再从同一处重试。
        at = start + 1;
        if start != 0 && is_attr_char(bytes[start - 1]) {
            continue;
        }
        let value_at = start + needle.len();
        let Some(end) = bytes[value_at..].iter().position(|&b| b == b'"') else {
            // 值没有收尾引号：这一格不当锚点，也不整段跳（那是坏标签）。
            continue;
        };
        if let Ok(value) = std::str::from_utf8(&bytes[value_at..value_at + end]) {
            out.push((start, value.to_string()));
        }
        at = value_at + end + 1;
    }
}

/// 能构成属性名一部分的字符。用来判 `id="` 前面是不是 `aid=` 的 `a`。
fn is_attr_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

/// MOBI 的 base32：`0-9` → 0-9，`A-V` → 10-31，**前导 0 剥掉**，最多 6 位
/// （与 `parse_rawml.c::mobi_base32_decode` 对拍）。
fn decode_base32(text: &str) -> Option<u32> {
    let s = text.trim_start_matches('0');
    if s.len() > 6 {
        return None;
    }
    let mut value: u32 = 0;
    for c in s.bytes() {
        let digit = match c {
            b'0'..=b'9' => (c - b'0') as u32,
            b'A'..=b'V' => (c - b'A') as u32 + 10,
            _ => return None,
        };
        value = value.checked_mul(32)?.checked_add(digit)?;
    }
    Some(value)
}

/// 标签名（`<` 之后、`/` 与空白之前）。
fn tag_name(tag: &str) -> &str {
    let inner = tag.trim_start_matches('<').trim_start_matches('/');
    let end = inner
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .unwrap_or(inner.len());
    &inner[..end]
}

/// 取标签里某个属性的值（双引号 / 单引号 / 裸值都认，属性名大小写不敏感）。
fn attr_value<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let bytes = tag.as_bytes();
    let needle = name.as_bytes();
    let mut i = 0usize;
    // 这一趟**按字节**推进，所以比对走 `bytes` 而不是 `&tag[i..]`：后者在下标落到
    // 多字节字符中间时会 panic，而带中文 `alt` 的标签是常态。边界判据（前一个字节
    // 是空白）保证后面切 `&tag[j..]` 那几个位置一定站在字符开头。
    while i + needle.len() <= bytes.len() {
        // 属性名要在一个边界上：前面是空白。
        let at_boundary = i == 0 || bytes[i - 1].is_ascii_whitespace();
        if at_boundary && bytes[i..i + needle.len()].eq_ignore_ascii_case(needle) {
            let after = i + needle.len();
            let mut j = after;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'=' {
                j += 1;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < bytes.len() {
                    let quote = bytes[j];
                    if quote == b'"' || quote == b'\'' {
                        let start = j + 1;
                        let end = tag[start..].find(quote as char).map(|e| start + e)?;
                        return Some(&tag[start..end]);
                    }
                    let end = tag[j..]
                        .find(|c: char| c.is_whitespace() || c == '>')
                        .map(|e| j + e)
                        .unwrap_or(tag.len());
                    return Some(&tag[j..end]);
                }
            }
        }
        i += 1;
    }
    None
}

/// 解析一个十进制序号，容忍前后空白。
fn parse_index(text: &str) -> Option<usize> {
    text.trim().parse::<usize>().ok()
}

/// 把 CSS 里的资源引用改写成我们的 href 方案。
///
/// KindleGen 把作者 CSS 的 `src: url(fonts/x.ttf)` 编译进 KF8 的 **CSS flow 段**，
/// 指向 `kindle:embed:NNNN`（字体与图片都是这一形），`kindle:flow:NNNN` 引 SVG flow 段。
/// 不收这一步，`@font-face` 的 `src` 与 [`MobiAdapter::fonts`] 返回的 `mobi-font/<记录>`
/// 就对不上 —— 内嵌字体永远注册不上（`FONTSIGNATURE` 不参与这件事）。
///
/// 记录号 = `first_resource + N - 1`；是不是字体查资源表（[`resource_kind`]），
/// 字体记 `mobi-font/`、其余（图片）记 `mobi-img/`。
fn rewrite_css(css: &str, resources: &[rawml::Resource], first_resource: Option<usize>) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(at) = rest.find("kindle:") {
        out.push_str(&rest[..at]);
        let tail = &rest[at + "kindle:".len()..];
        if let Some(after) = tail.strip_prefix("embed:") {
            let (num, remainder) = leading_digits(after);
            let replacement = match (num.parse::<usize>().ok(), first_resource) {
                (Some(n), Some(base)) if !num.is_empty() => {
                    let record = base + n.saturating_sub(1);
                    let prefix = match resource_kind(resources, record) {
                        Some(rawml::ResourceKind::Font) => FONT_PREFIX,
                        _ => IMAGE_PREFIX,
                    };
                    format!("{prefix}{record}")
                }
                _ => format!("kindle:embed:{num}"),
            };
            out.push_str(&replacement);
            rest = strip_query(remainder);
        } else if let Some(after) = tail.strip_prefix("flow:") {
            let (num, remainder) = leading_digits(after);
            let replacement = match num.parse::<usize>() {
                Ok(n) if !num.is_empty() => format!("{SVG_PREFIX}{n}"),
                _ => format!("kindle:flow:{num}"),
            };
            out.push_str(&replacement);
            rest = strip_query(remainder);
        } else {
            // 不是我们处理的那两种：`kindle:` 原样保留，从它之后继续扫。
            out.push_str("kindle:");
            rest = tail;
        }
    }
    out.push_str(rest);
    out
}

/// 取出开头的连续数字，返回 `(数字, 剩余)`。
fn leading_digits(text: &str) -> (&str, &str) {
    let end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    (&text[..end], &text[end..])
}

/// 跳过 `kindle:` URL 尾部的查询串（`?mime=…`），停在引号 / 括号 / 空白处。
fn strip_query(text: &str) -> &str {
    let Some(rest) = text.strip_prefix('?') else {
        return text;
    };
    let end = rest
        .find(|c: char| c == '"' || c == '\'' || c == ')' || c.is_whitespace())
        .unwrap_or(rest.len());
    &rest[end..]
}

/// 某条记录在资源表里的种类。
fn resource_kind(resources: &[rawml::Resource], record: usize) -> Option<rawml::ResourceKind> {
    resources
        .iter()
        .find(|r| r.record == record)
        .map(|r| r.kind)
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

#[cfg(test)]
mod tests {
    //! 夹具是一本**合成 MOBI**：手摆 PalmDB（录 0 + 正文 + 一张图 + EOF）。
    //! 断言走的是完整那条路：判定数据 → provider → 章 / 图 / 书名。

    use std::io::Write as _;
    use std::path::{Path, PathBuf};

    use super::{MobiAdapter, Rewrite, header, rawml, rewrite_html};
    use crate::content::format::{Content, DocumentProvider, Format};

    /// 一张真 PNG（喂 `image_dims` 与封面缩放用）。
    fn png(width: u32, height: u32) -> Vec<u8> {
        use image::ImageEncoder as _;
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba([7, 8, 9, 255]));
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), width, height, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    /// 摆一份录 0：PalmDOC 头 + MOBI 头 + EXTH（书名 / 作者 / 封面偏移）。
    fn record0(text_len: u32, text_count: u16, image_index: u32, cover: u32) -> Vec<u8> {
        let mut rec = vec![0u8; 0x200];
        // PalmDOC：不压。
        rec[0..2].copy_from_slice(&1u16.to_be_bytes());
        rec[4..8].copy_from_slice(&text_len.to_be_bytes());
        rec[8..10].copy_from_slice(&text_count.to_be_bytes());
        rec[10..12].copy_from_slice(&4096u16.to_be_bytes());
        rec[12..14].copy_from_slice(&0u16.to_be_bytes());
        // MOBI 头：版本 6（KF7）。
        rec[16..20].copy_from_slice(b"MOBI");
        rec[20..24].copy_from_slice(&232u32.to_be_bytes());
        rec[28..32].copy_from_slice(&65001u32.to_be_bytes());
        rec[36..40].copy_from_slice(&6u32.to_be_bytes());
        rec[108..112].copy_from_slice(&image_index.to_be_bytes());
        rec[128..132].copy_from_slice(&0x40u32.to_be_bytes());
        // EXTH 在 16 + 232 = 248。条目多了之后录 0 要跟着长，别按定长跑。
        let exth_at = 248;
        let items: Vec<(u32, Vec<u8>)> = vec![
            (99u32, "合成 MOBI 之书".as_bytes().to_vec()),
            (100, "作者乙".as_bytes().to_vec()),
            (201, cover.to_be_bytes().to_vec()),
            // 书目区：每条对应哪一条 Dublin Core 写在 `header::EXTH_*` 上。
            (101, "某某出版社".as_bytes().to_vec()),
            (103, "一段简介".as_bytes().to_vec()),
            (104, "978-7-02-000000-0".as_bytes().to_vec()),
            (105, "科幻".as_bytes().to_vec()),
            (105, "冒险".as_bytes().to_vec()),
            (106, "2020-01".as_bytes().to_vec()),
            (108, "译者丙".as_bytes().to_vec()),
            (109, "版权所有".as_bytes().to_vec()),
            (111, "EBOK".as_bytes().to_vec()),
            (524, "zh-CN".as_bytes().to_vec()),
        ];
        let mut exth = Vec::new();
        for (tag, value) in &items {
            exth.extend_from_slice(&tag.to_be_bytes());
            exth.extend_from_slice(&((value.len() + 8) as u32).to_be_bytes());
            exth.extend_from_slice(value);
        }
        let exth_len = 12 + exth.len();
        rec[exth_at..exth_at + 4].copy_from_slice(b"EXTH");
        rec[exth_at + 4..exth_at + 8].copy_from_slice(&(exth_len as u32).to_be_bytes());
        // 条数从那张表数出来：写死一个数，加一条就悄悄错位。
        rec[exth_at + 8..exth_at + 12].copy_from_slice(&(items.len() as u32).to_be_bytes());
        rec[exth_at + 12..exth_at + 12 + exth.len()].copy_from_slice(&exth);
        rec
    }

    /// 把若干条记录摆成一个 PalmDB 文件（头 78 + 表 + 内容）。
    fn write_palmdb(dir: &Path, name: &str, records: &[Vec<u8>]) -> PathBuf {
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        let mut head = vec![0u8; 78];
        head[60..64].copy_from_slice(b"BOOK");
        head[64..68].copy_from_slice(b"MOBI");
        head[76..78].copy_from_slice(&(records.len() as u16).to_be_bytes());
        file.write_all(&head).unwrap();
        let mut start = 78 + records.len() * 8;
        let mut table = Vec::with_capacity(records.len() * 8);
        for rec in records {
            table.extend_from_slice(&(start as u32).to_be_bytes());
            table.push(0);
            table.extend_from_slice(&[0, 0, 0]);
            start += rec.len();
        }
        file.write_all(&table).unwrap();
        for rec in records {
            file.write_all(rec).unwrap();
        }
        path
    }

    /// 一本合成的 KF7 MOBI：两章（分页符隔开）、一张图、EOF。
    fn sample(dir: &Path) -> PathBuf {
        let text = b"<p>one</p><mbp:pagebreak/><p>two<img recindex=\"1\"/></p>".to_vec();
        let image = png(30, 10);
        let eof = b"\xe9\x8e\r\n".to_vec();
        let r0 = record0(text.len() as u32, 1, 2, 0);
        write_palmdb(dir, "a.mobi", &[r0, text, image, eof])
    }

    #[test]
    fn it_claims_the_mobi_family() {
        assert_eq!(MobiAdapter.format(), Format::Mobi);
        for ext in ["mobi", "azw", "azw3", "prc", "MOBI"] {
            assert!(
                MobiAdapter.supports(Path::new(&format!("a.{ext}"))),
                ".{ext} 该被认领"
            );
        }
        assert!(!MobiAdapter.supports(Path::new("a.epub")));
    }

    /// EXTH 的书目区一路走到 `DocMeta` 的每一格。
    ///
    /// 单开一条而不是塞进上面那条读取测试：某一条标签的号数错一位，症状是「那一格空着
    /// 而别格多出一串没人写的字」，只有逐格断言才说得出是哪一条。
    #[test]
    fn the_exth_metadata_lands_in_every_dublin_core_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = sample(dir.path());
        let (meta, _) = MobiAdapter.index(&path).unwrap();
        assert_eq!(meta.title, "合成 MOBI 之书");
        assert_eq!(meta.authors, vec!["作者乙"]);
        assert_eq!(meta.contributors, vec!["译者丙"]);
        assert_eq!(
            meta.subjects,
            vec!["科幻", "冒险"],
            "同一标签出现两次要都收到"
        );
        assert_eq!(meta.description, "一段简介");
        assert_eq!(meta.publisher, "某某出版社");
        assert_eq!(meta.published, "2020-01");
        assert_eq!(meta.language, "zh-CN");
        assert_eq!(meta.isbn, "978-7-02-000000-0");
        assert_eq!(meta.dctype, "EBOK");
        assert_eq!(meta.rights, "版权所有");
        assert_eq!(
            meta.series, None,
            "Kindle 的 collection 不在书本文件里，别造一个"
        );
    }

    #[test]
    fn a_book_reads_with_title_author_chapters_and_image() {
        let dir = tempfile::tempdir().unwrap();
        let path = sample(dir.path());
        let (meta, content) = MobiAdapter.read(&path).unwrap();
        assert_eq!(meta.title, "合成 MOBI 之书");
        assert_eq!(meta.authors, vec!["作者乙".to_string()]);
        assert_eq!(meta.format, Format::Mobi);

        let Content::Reflow(flow) = content else {
            panic!("MOBI 该读成可重排");
        };
        assert_eq!(flow.chapters.len(), 2, "分页符该切出两章");
        assert!(flow.chapters[0].source().contains("one"));
        // 正文里的 <img recindex="1"> 该被改写成指向记录 2 的 href。
        assert!(
            flow.chapters[1].source().contains("mobi-img/2"),
            "{}",
            flow.chapters[1].source()
        );
        // 图片元数据：href = 记录号，尺寸从 PNG 头读到。
        assert_eq!(flow.images.len(), 1);
        assert_eq!(flow.images[0].href, "mobi-img/2");
        assert_eq!((flow.images[0].width, flow.images[0].height), (30, 10));
        assert_eq!(flow.images[0].media_type, "image/png");
        // 封面：EXTH 201 = 0 → 记录 2（同一张图）。
        assert!(flow.cover.is_some());

        // 按记录号现取字节要拿回原图。
        let got = MobiAdapter.image_bytes(&path, "mobi-img/2").unwrap();
        assert_eq!(got, png(30, 10));
        assert!(MobiAdapter.image_bytes(&path, "mobi-img/99").is_none());
    }

    /// 索引那一趟只留位置与字数：每章是 `Section`，正文不在手上。
    #[test]
    fn index_keeps_only_positions_and_char_counts() {
        let dir = tempfile::tempdir().unwrap();
        let path = sample(dir.path());
        let (_, content) = MobiAdapter.index(&path).unwrap();
        let Content::Reflow(flow) = content else {
            panic!("该读成可重排");
        };
        assert_eq!(flow.chapters.len(), 2);
        for (i, chapter) in flow.chapters.iter().enumerate() {
            assert!(
                matches!(chapter.locator(), Some(crate::ChapterLocator::Section(n)) if *n == i),
                "该按序号存位置"
            );
            assert!(chapter.source().is_empty(), "索引那一趟不该带正文");
            assert!(chapter.char_len().unwrap_or(0) > 0, "字数要留下来");
        }
        // 按位置回容器取，拿到的要与全文那一趟一致。
        let reopened = MobiAdapter
            .chapter_source(&path, &crate::ChapterLocator::Section(1))
            .unwrap();
        assert!(reopened.contains("two"));
    }

    /// 一条不压的 FONT 记录：magic + 解出尺寸 + flags=0 + 数据偏移 + 负载。
    fn font_record(payload: &[u8]) -> Vec<u8> {
        let mut rec = vec![0u8; 24];
        rec[..4].copy_from_slice(b"FONT");
        rec[4..8].copy_from_slice(&(payload.len() as u32).to_be_bytes());
        rec[8..12].copy_from_slice(&0u32.to_be_bytes());
        rec[12..16].copy_from_slice(&(24u32).to_be_bytes());
        rec.extend_from_slice(payload);
        rec
    }

    /// 一本带两条 FONT 记录的合成 MOBI（首资源 = 录 2，所以是 `mobi-font/2` 与 `-3`）。
    fn two_fonts(dir: &Path) -> PathBuf {
        let text = "<p>正文</p>".as_bytes().to_vec();
        let records = vec![
            record0(text.len() as u32, 1, 2, 0),
            text,
            font_record(b"FONT-A-PAYLOAD"),
            font_record(b"FONT-B"),
            b"\xe9\x8e\r\n".to_vec(),
        ];
        write_palmdb(dir, "fonts.mobi", &records)
    }

    /// `fonts()` 只给 CSS **点名的那几份**：一本 AZW3 常带七八份 FONT 记录，全取等于把
    /// 没被声明的那几份也解压、拷成 `Vec<u8>`，交上去还在 `book_fonts.rs` 那道查表里丢掉。
    ///
    /// 三份断言各钉一件事：点一个只回一个（不是全书）、`wanted` 空就一条不给、
    /// 点名两个时按资源记录顺序回。
    #[test]
    fn fonts_returns_only_the_records_css_named() {
        let dir = tempfile::tempdir().unwrap();
        let path = two_fonts(dir.path());

        let one = MobiAdapter.fonts(&path, &["mobi-font/2".to_string()]);
        assert_eq!(
            one.iter().map(|f| f.href.clone()).collect::<Vec<_>>(),
            vec!["mobi-font/2".to_string()],
            "点名的那一份之外不许搭别的进来"
        );
        assert_eq!(one[0].bytes, b"FONT-A-PAYLOAD");

        let none = MobiAdapter.fonts(&path, &[]);
        assert!(none.is_empty(), "没人点名就一条都不解");

        let both = MobiAdapter.fonts(
            &path,
            &["mobi-font/3".to_string(), "mobi-font/2".to_string()],
        );
        assert_eq!(
            both.iter().map(|f| f.href.clone()).collect::<Vec<_>>(),
            vec!["mobi-font/2".to_string(), "mobi-font/3".to_string()],
            "给的顺序跟着记录，不跟着 `wanted`"
        );
        assert_eq!(both[1].bytes, b"FONT-B");
    }

    #[test]
    fn rewrite_html_maps_recindex_and_strips_mobi_tags() {
        let html = r#"<p>x<img recindex="00001"/></p><mbp:pagebreak/><link href="kindle:flow:0001?mime=text/css"/>"#;
        let ctx = super::Rewrite {
            first_resource: Some(10),
            ids: &[],
            chapter: 0,
        };
        let out = rewrite_html(html, &ctx);
        assert!(out.contains("<img src=\"mobi-img/10\"/>"), "{out}");
        assert!(!out.contains("mbp:"), "mobi 专有标签该去掉：{out}");
        assert!(!out.contains("<link"), "样式引用该去掉：{out}");
    }

    #[test]
    fn rewrite_html_maps_kf8_embed() {
        let html = r#"<img src="kindle:embed:0002"/>"#;
        let ctx = super::Rewrite {
            first_resource: Some(5),
            ids: &[],
            chapter: 0,
        };
        let out = rewrite_html(html, &ctx);
        // 记录 = base(5) + 2 - 1 = 6。
        assert_eq!(out, "<img src=\"mobi-img/6\"/>");
    }

    /// KF8 的 `kindle:pos:fid` 改写成章内 / 跨章锚点：`fid` 是章号、`off` 是字节偏移，
    /// 目标 = 该章里偏移 ≤ `off` 的最后一个 `id`/`aid`。
    #[test]
    fn rewrite_html_maps_posfid_to_anchors() {
        // 章 0：偏移 0 处有个 id="top"，随后在某处有个 id="mid"。
        let chapter0 = r#"<p id="top">甲</p>……很长……<p id="mid">乙</p>"#;
        let ids = vec![super::id_positions(chapter0)];
        // 同章：off 落在 "mid" 之后 → #mid。
        let off_at = chapter0.find("mid").unwrap() + 1;
        let same = format!(
            r##"<a href="kindle:pos:fid:{}:off:{}">同章</a>"##,
            b32(0, 4),
            b32(off_at as u32, 10),
        );
        let ctx = super::Rewrite {
            first_resource: None,
            ids: &ids,
            chapter: 0,
        };
        let out = rewrite_html(&same, &ctx);
        assert!(out.contains("href=\"#mid\""), "{out}");

        // 跨章：fid=1（另一章），当前在章 0 → mobi-part/1。
        let cross = format!(
            r##"<a href="kindle:pos:fid:{}:off:{}">跨章</a>"##,
            b32(1, 4),
            b32(0, 10),
        );
        let out = rewrite_html(&cross, &ctx);
        assert!(out.contains("href=\"mobi-part/1\""), "{out}");
    }

    /// base32 编码（测试用）：`0-9`→0-9、`A-V`→10-31，定宽补前导 0。
    fn b32(mut value: u32, width: usize) -> String {
        let mut digits = Vec::new();
        if value == 0 {
            digits.push(b'0');
        }
        while value > 0 {
            let d = (value % 32) as u8;
            digits.push(if d < 10 { b'0' + d } else { b'A' + d - 10 });
            value /= 32;
        }
        digits.reverse();
        let s = String::from_utf8(digits).unwrap();
        format!("{s:0>width$}")
    }

    #[test]
    fn base32_decodes_like_the_reference() {
        assert_eq!(super::decode_base32("0"), Some(0));
        assert_eq!(super::decode_base32("00000"), Some(0));
        assert_eq!(super::decode_base32("A"), Some(10));
        assert_eq!(super::decode_base32("V"), Some(31));
        assert_eq!(super::decode_base32("10"), Some(32));
        assert_eq!(super::decode_base32("0001"), Some(1));
        assert_eq!(super::decode_base32("Z"), None, "超过 V 的字母不合法");
    }

    /// CSS 里的 `kindle:embed` 按资源类型改写：字体 → `mobi-font/`，图片 → `mobi-img/`；
    /// `kindle:flow` 的 SVG 段 → `mobi-svg/`。这是内嵌字体能不能生效的那一步。
    #[test]
    fn css_kindle_refs_are_rewritten_by_resource_kind() {
        let resources = vec![
            super::rawml::Resource {
                record: 10,
                kind: super::rawml::ResourceKind::Font,
                media_type: "application/x-font",
            },
            super::rawml::Resource {
                record: 11,
                kind: super::rawml::ResourceKind::Image,
                media_type: "image/png",
            },
        ];
        let css = concat!(
            "@font-face{font-family:X;src:url(kindle:embed:0001)}",
            ".bg{background:url(\"kindle:embed:0002\")}",
            ".s{background:url(kindle:flow:0003?mime=image/svg+xml)}"
        );
        let out = super::rewrite_css(css, &resources, Some(10));
        assert!(out.contains("url(mobi-font/10)"), "{out}");
        assert!(out.contains("url(\"mobi-img/11\")"), "{out}");
        assert!(out.contains("url(mobi-svg/3)"), "{out}");
        assert!(!out.contains("kindle:embed"), "不该留原始引用：{out}");
        // 没有资源基准时原样保留（宁可样式里留一个无效 url，也别改成错的记录号）。
        let kept = super::rewrite_css(css, &resources, None);
        assert!(kept.contains("kindle:embed:0001"), "{kept}");
    }

    /// FONT 记录解码：magic + 尺寸 + flags=0（不压不去混淆）时，数据段原样给出。
    #[test]
    fn font_records_decode() {
        let mut rec = vec![0u8; 24];
        rec[..4].copy_from_slice(b"FONT");
        rec[4..8].copy_from_slice(&4u32.to_be_bytes()); // 解出尺寸
        rec[8..12].copy_from_slice(&0u32.to_be_bytes()); // flags：不 xor、不 zlib
        rec[12..16].copy_from_slice(&24u32.to_be_bytes()); // data_offset
        rec.extend_from_slice(b"TTF!");
        let got = super::decode_font(&rec).expect("不压字体该直接给出字节");
        assert_eq!(got, b"TTF!");
        // 坏 magic 不算字体。
        assert!(super::decode_font(b"XXXX....").is_none());
    }

    /// 一本合成的 KF7 MOBI：`chapters` 章（分页符隔开），每章约 `chars` 个字符，正文按
    /// 4096 切成多条记录（真书的切法），末尾一张图与 EOF。
    fn book_of(dir: &Path, name: &str, chapters: usize, chars: usize) -> PathBuf {
        const FILLER: &str = "这里是一段用来占位的中文正文，讲一件不值一提的事，够长才量得准。";
        let mut text = String::new();
        for i in 0..chapters {
            if i > 0 {
                text.push_str("<mbp:pagebreak/>");
            }
            text.push_str("<p>");
            let mut body = format!("第{}节。", i + 1);
            while body.chars().count() < chars {
                body.push_str(FILLER);
            }
            text.push_str(&body);
            text.push_str("</p>");
        }
        let bytes = text.into_bytes();
        let text_len = bytes.len() as u32;
        let mut records: Vec<Vec<u8>> = bytes.chunks(4096).map(|c| c.to_vec()).collect();
        let text_count = records.len() as u16;
        let image_index = 1 + u32::from(text_count);
        records.push(png(30, 10));
        records.push(b"\xe9\x8e\r\n".to_vec());
        records.insert(0, record0(text_len, text_count, image_index, 0));
        write_palmdb(dir, name, &records)
    }

    /// 量「翻一章到底付多少钱」：memo 之外的那条路（`chapter_source`）每章都重解**整本**，
    /// 所以顺着读完整本就是「章数 × 一整本」—— 这一条钉的是 memo 存在的理由。
    ///
    /// 跑法：`cargo test --release -p folio-core --lib -- --ignored --nocapture mobi_reparse`
    ///
    /// 三列对照着看才站得住：**索引**那一趟（导入付一次）、**全文**那一趟（索引之外还要
    /// 每章正文、封面、图片尺寸与 CSS）、**单步**（每章一次 `chapter_source`，只为了拿一章）。
    /// 单步走的是「只要正文」那一趟，所以它比全文**便宜**（2026-10-21 拆出 `read_texts`
    /// 之后实测 0.6 倍），但仍是「整本重解」的量级：单步那一列随章数线性涨，就是
    /// memo 断了的样子（`Metric::WholeBookRereads` 同一条判据）。
    ///
    /// 只测 `COMPRESSION_NONE` 那一档：`codec` 里只有解压没有压缩，合成不出 PalmDOC /
    /// HUFF/CDIC 的字节，所以下面所有毫秒都是**下界** —— 真书的正文记录还要过一遍解码。
    #[test]
    #[ignore = "性能基准：要 --release --ignored --nocapture"]
    fn a_chapter_step_costs_a_whole_book() {
        use std::time::Instant;

        let dir = tempfile::tempdir().unwrap();
        println!("MOBI 重解基准（release、正文不压缩 = 下界）");
        println!("  章数    驻留KB   索引ms   全文ms   单步ms  单步/全文");
        for chapters in [50usize, 200usize, 600usize] {
            let path = book_of(dir.path(), &format!("b{chapters}.mobi"), chapters, 220);
            // 夹具自检：章数对不上说明记录表摆错了，后面的数一个都不必信。
            let Content::Reflow(flow) = MobiAdapter.index(&path).unwrap().1 else {
                panic!("MOBI 该读成可重排");
            };
            assert_eq!(flow.chapters.len(), chapters, "夹具章数不对");

            let started = Instant::now();
            let _ = MobiAdapter.index(&path).unwrap();
            let index_ms = started.elapsed().as_secs_f64() * 1000.0;

            let started = Instant::now();
            let Content::Reflow(full) = MobiAdapter.read(&path).unwrap().1 else {
                panic!("MOBI 该读成可重排");
            };
            let full_ms = started.elapsed().as_secs_f64() * 1000.0;
            let resident_kb = full
                .chapters
                .iter()
                .map(|c| c.source().len())
                .sum::<usize>() as f64
                / 1024.0;

            let steps = chapters.min(20);
            let started = Instant::now();
            for i in 0..steps {
                let at = crate::ChapterLocator::Section(i);
                assert!(!MobiAdapter.chapter_source(&path, &at).unwrap().is_empty());
            }
            let step_ms = started.elapsed().as_secs_f64() * 1000.0 / steps as f64;

            println!(
                "{chapters:>6}  {resident_kb:>8.0}  {index_ms:>6.1}  {full_ms:>7.1}  \
                 {step_ms:>7.2}  {:>8.2}",
                step_ms / full_ms
            );
        }
    }

    /// MOBI 一趟读一本的分阶段账：**哪一步占大头** + **每一步从容器扛了多少字节**。
    ///
    /// 跑法：
    /// `cargo test --release -p folio-core --lib -- --ignored --nocapture mobi_stage_costs`
    /// 换一批书：`FOLIO_MOBI_DIR=/path/to/dir`（默认 `reference/libmobi/tests/samples`，
    /// 外加一本由真中文小说正文包的 KF7 —— 那是「几百万字、上百章」那档形状，样本里没有）。
    ///
    /// 两栏一起看才站得住：**毫秒**只回答「这一趟里谁占大头」，会被调度与频率牵着走；
    /// **字节**是确定量（同一本书同一趟取几条记录、多少字节有唯一答案），所以「这一步在
    /// 白扛容器字节」这种判断只由它下。取字节走的是 `Meter`，包的就是 `PalmDb::read`。
    ///
    /// 每阶段取 3 轮最小值，字节取最后一轮。
    #[test]
    #[ignore = "性能基准：要 --release --ignored --nocapture"]
    fn mobi_stage_costs_on_a_real_book() {
        // 合成那几本住在临时目录里，目录必须在整趟基准期间活着。
        let scratch = tempfile::tempdir().expect("合成 KF7 的临时目录");
        let mut corpus = stage_corpus();
        corpus.extend(synthetic_kf7_books(scratch.path()));
        for path in &corpus {
            stage_probe(path);
        }
        split_scaling(scratch.path());
    }

    /// 重复轮数（取最小值 = 热页缓存那一档）。
    const STAGE_RUNS: usize = 3;

    /// 语料：目录里的每本 `.mobi` / `.azw` / `.azw3` / `.prc`。
    fn stage_corpus() -> Vec<PathBuf> {
        let dir = std::env::var("FOLIO_MOBI_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| Path::new("../../reference/libmobi/tests/samples").to_path_buf());
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .filter(|n| {
                        matches!(
                            n.rsplit('.').next(),
                            Some("mobi") | Some("azw") | Some("azw3") | Some("prc")
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names.into_iter().map(|n| dir.join(n)).collect()
    }

    /// 一本「几百万字、上百章」的 KF7：正文是 `tests/fixtures/local/txt` 里的真小说。
    ///
    /// 样本目录里没有这个形状（最大的那本 3 MB 还是词典），而中文书花的正是这一档 ——
    /// 正文不压缩，所以所有毫秒都是**下界**（真书的正文记录还要过一遍解码）。
    fn synthetic_kf7_books(dir: &Path) -> Vec<PathBuf> {
        let novel = Path::new("../../tests/fixtures/local/txt/hongloumeng.txt");
        let Ok(text) = std::fs::read_to_string(novel) else {
            return Vec::new();
        };
        vec![kf7_of_novel(dir, &text, 121)]
    }

    /// 把一段正文包成 KF7 MOBI（不压缩）：`breaks` 个 `<mbp:pagebreak/>` 均匀插进正文。
    ///
    /// 切点按**字符**取，不按字节：落在多字节中间会被 `from_utf8_lossy` 换成 U+FFFD，
    /// 那是夹具在改正文，不是形状样本。
    fn kf7_of_novel(dir: &Path, novel: &str, breaks: usize) -> PathBuf {
        let clean: String = novel
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("</p><p>");
        let chars: Vec<char> = clean.chars().collect();
        let per = (chars.len() / breaks.max(1)).max(1);
        let mut text = String::with_capacity(clean.len() + 64 * breaks + 32);
        text.push_str("<html><body><p>");
        for (i, piece) in chars.chunks(per).enumerate() {
            if i > 0 {
                text.push_str("</p><mbp:pagebreak/><p>");
            }
            text.extend(piece);
        }
        text.push_str("</p></body></html>");

        let bytes = text.into_bytes();
        let text_len = bytes.len() as u32;
        let mut records: Vec<Vec<u8>> = bytes.chunks(4096).map(<[u8]>::to_vec).collect();
        let text_count = records.len() as u16;
        let image_index = 1 + u32::from(text_count);
        records.push(png(30, 10));
        records.push(b"\xe9\x8e\r\n".to_vec());
        records.insert(0, record0(text_len, text_count, image_index, 0));
        write_palmdb(dir, &format!("novel{breaks}.mobi"), &records)
    }

    /// 切章那一步的**标度**：同一份正文（长度不变）、分页符从 1 到 484。
    ///
    /// 这一条是给「切章有没有随章数走平方」这句话准备的：固定文本长下，线性应该几乎不涨，
    /// 平方级则跟着分页符个数成比例涨。断言放在打印之后 —— 章数最多的一档至少要能看出
    /// 与 1 档同一量级，否则这一趟就是在白切。
    fn split_scaling(dir: &Path) {
        let Ok(text) =
            std::fs::read_to_string(Path::new("../../tests/fixtures/local/txt/hongloumeng.txt"))
        else {
            return;
        };
        println!(
            "\n── 切章标度（同一份正文 {} KB，只动分页符个数）",
            text.len() / 1024
        );
        let mut costs = Vec::new();
        for breaks in [1usize, 30, 121, 484] {
            let path = kf7_of_novel(dir, &text, breaks);
            let (ms, parts) = split_only(&path);
            println!("   分页符 {breaks:>4}  →  {ms:>8.1} ms  （{parts} 段）");
            costs.push(ms);
        }
        // 平方级的话 484 档是 1 档的几百倍；线性留 20× 的余量当门。
        assert!(
            costs[3] < costs[0].max(0.5) * 20.0,
            "切章从 1 到 484 个分页符涨了 {:.1}× —— 超线性，切章在重扫已扫过的部分",
            costs[3] / costs[0].max(0.5)
        );
    }

    /// 只量 `split_kf7_ranges` 本身：解压与转字符串先做完，不进计时那一圈。
    fn split_only(path: &Path) -> (f64, usize) {
        let opened = super::open(path).expect("合成 KF7 该开得了");
        let meter = Meter {
            db: &opened.db,
            calls: std::cell::Cell::new(0),
            bytes: std::cell::Cell::new(0),
        };
        let read = |i: usize| meter.read(i);
        let recs: &dyn Fn(usize) -> Option<Vec<u8>> = &read;
        let text = super::rawml::decompress_text(recs, &opened.header, opened.kf8_offset)
            .expect("不压缩的正文该解得出");
        let html = String::from_utf8_lossy(&text).into_owned();
        let (ms, parts) = stage_ms(STAGE_RUNS, || super::rawml::split_kf7_ranges(&html));
        (ms, parts.len())
    }

    /// 包住 `PalmDb` 的量具：数清「取了几条记录、多少字节」。
    struct Meter<'a> {
        db: &'a super::PalmDb,
        calls: std::cell::Cell<usize>,
        bytes: std::cell::Cell<usize>,
    }

    impl Meter<'_> {
        fn read(&self, i: usize) -> Option<Vec<u8>> {
            let got = self.db.read(i);
            if let Some(b) = &got {
                self.calls.set(self.calls.get() + 1);
                self.bytes.set(self.bytes.get() + b.len());
            }
            got
        }
        /// 与 `rawml::palm_heads` 同一条口径：头 16 个字节 + 整条长度（认魔数用）。
        fn head(&self, i: usize) -> Option<(Vec<u8>, usize)> {
            let len = self.db.record(i)?.len;
            let got = self.db.read_head(i, 16)?;
            self.calls.set(self.calls.get() + 1);
            self.bytes.set(self.bytes.get() + got.len());
            Some((got, len))
        }
        fn reset(&self) {
            self.calls.set(0);
            self.bytes.set(0);
        }
        fn shot(&self) -> (usize, usize) {
            (self.calls.get(), self.bytes.get())
        }
    }

    /// 跑 `runs` 轮，回 `(最小毫秒, 最后一轮的产物)`。
    fn stage_ms<R>(runs: usize, mut f: impl FnMut() -> R) -> (f64, R) {
        use std::time::Instant;
        let mut best = f64::INFINITY;
        let mut last = None;
        for _ in 0..runs {
            let started = Instant::now();
            let got = f();
            best = best.min(started.elapsed().as_secs_f64() * 1000.0);
            last = Some(got);
        }
        (best, last.expect("至少跑了一轮"))
    }

    /// 一本书的分阶段读数。
    fn stage_probe(path: &Path) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let file_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let opened = match super::open(path) {
            Ok(o) => o,
            Err(e) => {
                println!("── {name}  开不了：{e}");
                return;
            }
        };
        let header = &opened.header;
        if header.encryption_type != 0 {
            println!(
                "── {name}  {} KB  跳过：加密 type {}",
                file_bytes / 1024,
                header.encryption_type
            );
            return;
        }

        let meter = Meter {
            db: &opened.db,
            calls: std::cell::Cell::new(0),
            bytes: std::cell::Cell::new(0),
        };
        let read = |i: usize| meter.read(i);
        let recs: &dyn Fn(usize) -> Option<Vec<u8>> = &read;
        let head = |i: usize| meter.head(i);
        let heads: &dyn Fn(usize) -> Option<(Vec<u8>, usize)> = &head;

        // 资源区的形状：按记录表算，不取字节。
        let mut img_records = 0usize;
        let mut img_bytes = 0usize;
        if let Some(start) = opened.first_resource {
            let mut i = start;
            while let Some(rec) = opened.db.record(i) {
                let Some(head) = opened.db.read_head(i, 8) else {
                    break;
                };
                if head.starts_with(b"\xe9\x8e\r\n") || head.starts_with(b"BOUNDARY") {
                    break;
                }
                if head.starts_with(b"FONT") {
                    i += 1;
                    continue;
                }
                if matches!(head.get(..3), Some([0xff, 0xd8, 0xff]))
                    || head.starts_with(b"GIF8")
                    || head.starts_with(b"\x89PNG\r\n\x1a\n")
                    || head.starts_with(b"BM")
                {
                    img_records += 1;
                    img_bytes += rec.len;
                }
                i += 1;
                if i > start + 100_000 {
                    break;
                }
            }
        }

        let compression = match header.compression {
            header::COMPRESSION_NONE => "不压",
            header::COMPRESSION_PALMDOC => "PalmDOC",
            header::COMPRESSION_HUFFCDIC => "HUFF/CDIC",
            _ => "不认得",
        };
        println!(
            "── {name}  {} KB  {} v{}  {compression}  正文 {} KB / {} 条记录  图 {img_records} 张 / {} KB",
            file_bytes / 1024,
            if header.is_kf8() { "KF8" } else { "KF7" },
            header.version,
            header.text_length as usize / 1024,
            header.text_record_count,
            img_bytes / 1024,
        );

        // ① 解压整本正文流。
        meter.reset();
        let (de_ms, text) = stage_ms(STAGE_RUNS, || {
            super::rawml::decompress_text(recs, header, opened.kf8_offset).unwrap_or_default()
        });
        let (de_calls, de_bytes) = meter.shot();
        println!(
            "   {:<34}{:>8.1} ms  {:>6} 次  {:>8} KB   ← 吐出 {} KB / 声明 {} KB{}",
            "解压正文流 decompress_text",
            de_ms,
            de_calls,
            de_bytes / 1024,
            text.len() / 1024,
            header.text_length as usize / 1024,
            if text.is_empty() {
                "  ← 一条都没解出来"
            } else if (text.len() as u32) < header.text_length {
                "  ← 比声明的短"
            } else {
                ""
            }
        );

        // ② 整条 `reconstruct`：含 ①，外加 FDST 切段、SKEL/FRAG 重建、资源扫描、INDX。
        meter.reset();
        let (rc_ms, rebuilt) = stage_ms(STAGE_RUNS, || {
            super::rawml::reconstruct(
                recs,
                heads,
                header,
                opened.kf8_offset,
                opened.first_resource,
            )
        });
        let (rc_calls, rc_bytes) = meter.shot();
        let Some(rec) = rebuilt else {
            let why = MobiAdapter
                .index(path)
                .err()
                .map(|e| e.to_string())
                .unwrap_or_else(|| "不知为何 provider 反倒成了".to_string());
            println!("   reconstruct 整条  {rc_ms:>8.1} ms —— 失败；provider 说的是：{why}");
            return;
        };
        let markup_kb: usize = rec.markup.iter().map(|m| m.len()).sum::<usize>() / 1024;
        println!(
            "   {:<34}{:>8.1} ms  {:>6} 次  {:>8} KB   ← markup {} KB、资源 {} 条",
            "reconstruct 整条（含上行）",
            rc_ms,
            rc_calls,
            rc_bytes / 1024,
            markup_kb,
            rec.resources.len()
        );
        println!(
            "     差 = 切段 + SKEL/FRAG 重建 + 扫资源：{:>6.1} ms  {:>5} 次  {:>7} KB",
            rc_ms - de_ms,
            rc_calls - de_calls,
            (rc_bytes - de_bytes) / 1024
        );

        // ③ 章：KF7 在这里切，KF8 已由 SKEL/FRAG 切好。
        let is_kf8 = header.is_kf8();
        let ranges = if is_kf8 {
            Vec::new()
        } else {
            let (sp_ms, got) = stage_ms(STAGE_RUNS, || {
                rec.markup
                    .first()
                    .map(|html| super::rawml::split_kf7_ranges(html))
                    .unwrap_or_default()
            });
            println!(
                "   {:<34}{:>8.1} ms   {:>6} 段",
                "切章 split_kf7_ranges",
                sp_ms,
                got.len()
            );
            got
        };
        let parts: Vec<&str> = if is_kf8 {
            rec.markup.iter().map(|m| m.as_str()).collect()
        } else {
            ranges.iter().map(|(_, p)| p.as_str()).collect()
        };

        // ④ 锚点表 → ⑤ 改写 → ⑥ 章节摘要（字数 + 图清单）。
        let (ids_ms, ids) = stage_ms(STAGE_RUNS, || {
            parts
                .iter()
                .map(|p| super::id_positions(p))
                .collect::<Vec<_>>()
        });
        let id_total: usize = ids.iter().map(|v| v.len()).sum();
        let (rw_ms, rewritten) = stage_ms(STAGE_RUNS, || {
            parts
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let ctx = Rewrite {
                        first_resource: opened.first_resource,
                        ids: &ids,
                        chapter: i,
                    };
                    super::rewrite_html(p, &ctx)
                })
                .collect::<Vec<String>>()
        });
        let out_kb: usize = rewritten.iter().map(|s| s.len()).sum::<usize>() / 1024;
        let (su_ms, sums) = stage_ms(STAGE_RUNS, || {
            rewritten
                .iter()
                .map(|s| crate::content::scan::chapter_summary(s))
                .collect::<Vec<_>>()
        });
        let chars: usize = sums.iter().map(|s| s.visible_chars).sum();
        println!(
            "   {:<34}{:>8.1} ms   {:>6} 个锚点",
            "扫锚点 id_positions", ids_ms, id_total
        );
        println!(
            "   {:<34}{:>8.1} ms   {:>6} KB 输出",
            "改写 rewrite_html", rw_ms, out_kb
        );
        println!(
            "   {:<34}{:>8.1} ms   {:>6} 可见字",
            "章节摘要 chapter_summary", su_ms, chars
        );
        // 索引那一趟能不能**不改写**直接算字数：把两条路的字数逐章对拍。
        // 改写的全部动作是「换属性里的 href」「丢掉空标签的壳」，两者都不该动文字节点；
        // 一旦哪一章对不上，那一趟就不能省（省了库里存的是另一个数）。
        let raw_chars: Vec<usize> = parts
            .iter()
            .map(|p| crate::content::scan::chapter_summary(p).visible_chars)
            .collect();
        let diff = raw_chars
            .iter()
            .zip(sums.iter())
            .filter(|(a, b)| **a != b.visible_chars)
            .count();
        println!(
            "     字数对拍 原始 {} vs 改写 {}：{}/{} 章不一致{}",
            raw_chars.iter().sum::<usize>(),
            chars,
            diff,
            parts.len(),
            if diff == 0 {
                "  ← 索引那一趟可以不改写"
            } else {
                "  ← 不改写会存下另一个数"
            }
        );

        // ⑦ 图片元数据：现读整条记录 + 读尺寸头（`read_with` 里那一条）。
        meter.reset();
        let (im_ms, _) = stage_ms(STAGE_RUNS, || {
            let mut n = 0usize;
            for r in &rec.resources {
                if r.kind == rawml::ResourceKind::Image {
                    n += read(r.record)
                        .map(|b| crate::content::format::image_dims(&b))
                        .is_some() as usize;
                }
            }
            n
        });
        let (im_calls, im_bytes) = meter.shot();
        println!(
            "   {:<34}{:>8.1} ms  {:>6} 次  {:>8} KB",
            "图片元数据（整条记录 + 尺寸头）",
            im_ms,
            im_calls,
            im_bytes / 1024
        );

        // ⑧ 封面：整条记录读进来 + 缩到 512。
        meter.reset();
        let (cv_ms, _) = stage_ms(STAGE_RUNS, || super::cover_bytes(&opened));
        let (cv_calls, cv_bytes) = meter.shot();
        println!(
            "   {:<34}{:>8.1} ms  {:>6} 次  {:>8} KB",
            "封面 cover_bytes",
            cv_ms,
            cv_calls,
            cv_bytes / 1024
        );
        cover_split(&opened);

        // ⑨ provider 三条整趟：索引（导入）、全文（开书 memo）、字体。
        //
        // `fonts` 点一个不存在的 href：要量的正是「为了列字体表付了多少」，
        // 与能不能拿到字节无关。`&[]` 那一档对照「没人点名时它也解一遍整本」。
        let (ix_ms, _) = stage_ms(STAGE_RUNS, || MobiAdapter.index(path));
        let (fu_ms, _) = stage_ms(STAGE_RUNS, || MobiAdapter.read(path));
        let (fo_ms, _) = stage_ms(STAGE_RUNS, || {
            MobiAdapter.fonts(path, &["mobi-font/1".to_string()])
        });
        let (fe_ms, _) = stage_ms(STAGE_RUNS, || MobiAdapter.fonts(path, &[]));
        let (te_ms, texts) = stage_ms(STAGE_RUNS, || MobiAdapter.read_texts(path));
        let (ib_ms, _) = stage_ms(STAGE_RUNS, || MobiAdapter.image_bytes(path, "mobi-img/99"));
        println!(
            "   {:<34}{:>8.1} ms",
            "provider::index（导入那一趟）", ix_ms
        );
        println!("   {:<34}{:>8.1} ms", "provider::read（全文那一趟）", fu_ms);
        println!(
            "   {:<34}{:>8.1} ms  （{} 章；开书 memo 走这一条）",
            "provider::read_texts",
            te_ms,
            texts.map_or(0, |v| v.len())
        );
        println!(
            "   {:<34}{:>8.1} ms  （点名 1 个）/ {:>6.1} ms  （点名 0 个）",
            "provider::fonts", fo_ms, fe_ms
        );
        println!(
            "   {:<34}{:>8.1} ms  （取不到的号，只开容器）",
            "provider::image_bytes", ib_ms
        );
    }

    /// 一整本读出来的**确定性摘要**（不含任何计时）。
    ///
    /// 为什么要有它：这一轮的性能改动全都声称「产物一个字节都不变」。毫秒会被调度与
    /// 频率牵着走，这份摘要不会 —— 改前跑一遍、改后再跑一遍，两行摘要逐字相同才算数。
    ///
    /// 跑法：`cargo test --release -p folio-core --lib -- --ignored --nocapture mobi_digest`
    /// （重定向到文件、别接 `tail`：改动前后各存一份再 `diff`。）
    #[test]
    #[ignore = "改动前后的产物对拍：要 --release --ignored --nocapture"]
    fn mobi_read_digest() {
        let scratch = tempfile::tempdir().expect("合成 KF7 的临时目录");
        let mut corpus = stage_corpus();
        corpus.extend(synthetic_kf7_books(scratch.path()));
        for path in &corpus {
            digest_one(path);
        }
    }

    /// 把一段东西压成一个稳定读数（同一次构建里可比）。
    fn digest_of(text: &str) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut hasher);
        hasher.finish()
    }

    /// 一本书的摘要：全文那一趟（`read`）与索引那一趟（`index`）各一条。
    fn digest_one(path: &Path) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let flow: crate::content::format::FlowDoc = match MobiAdapter.read(path) {
            Ok((_, Content::Reflow(flow))) => flow,
            Ok(_) => {
                println!("{name}  read 回来的不是可重排");
                return;
            }
            Err(err) => {
                println!("{name}  read 失败：{err}");
                return;
            }
        };
        // 每一章「标题 + 正文 + 字数」一起进摘要：切章切错一位、改写少换一处，
        // 都会在这一格露出来。
        let per_chapter: Vec<String> = flow
            .chapters
            .iter()
            .map(|c| format!("{}\u{1}{}\u{1}{:?}", c.title, c.source(), c.char_len))
            .collect();
        let body: String = flow.chapters.iter().map(|c| c.source()).collect();
        let images: Vec<String> = flow
            .images
            .iter()
            .map(|img| {
                format!(
                    "{}\u{1}{}\u{1}{}x{}",
                    img.href, img.media_type, img.width, img.height
                )
            })
            .collect();
        let images = images.join("\u{2}");
        println!(
            "{name}  read  章 {} 正文 {} B 摘要 {:016x}｜每章 {:016x}｜图 {} 张 {:016x}｜CSS {} B {:016x}｜封面 {} B",
            flow.chapters.len(),
            body.len(),
            digest_of(&body),
            digest_of(&per_chapter.join("\u{2}")),
            flow.images.len(),
            digest_of(&images),
            flow.css.len(),
            digest_of(&flow.css),
            flow.cover.as_ref().map_or(0, |c| c.len()),
        );

        // 索引那一趟落进库里的东西：章表（标题 + 位置 + 字数）、图片清单、书目。
        let indexed: crate::content::format::FlowDoc = match MobiAdapter.index(path) {
            Ok((_, Content::Reflow(flow))) => flow,
            Ok(_) => {
                println!("{name}  index 回来的不是可重排");
                return;
            }
            Err(err) => {
                println!("{name}  index 失败：{err}");
                return;
            }
        };
        let rows: Vec<String> = indexed
            .chapters
            .iter()
            .map(|c| {
                format!(
                    "{}\u{1}{:?}\u{1}{:?}\u{1}{}",
                    c.title,
                    c.char_len,
                    c.locator(),
                    c.images
                        .iter()
                        .map(|u| format!("{}@{}", u.src, u.offset))
                        .collect::<Vec<_>>()
                        .join(","),
                )
            })
            .collect();
        println!(
            "{name}  index 章 {}｜每章 {:016x}｜字数 {}｜图 {} 张｜toc {}｜css {} B",
            indexed.chapters.len(),
            digest_of(&rows.join("\u{2}")),
            indexed
                .chapters
                .iter()
                .filter_map(|c| c.char_len)
                .sum::<usize>(),
            indexed.images.len(),
            indexed.toc.len(),
            indexed.css.len(),
        );
    }

    /// **开书 memo 敢用「只要正文」那一趟的前提**：它给的每一串与全文那一趟逐字节相同。
    ///
    /// 少了这一条，便宜的那一问就可能在读者看不见的地方给出另一个东西 —— 进度、锚点、
    /// 划线都建在正文那一串上，而字数是按另一趟算的。第三条断言钉的是 memo 建不起来时
    /// 的兜底（`chapter_source`）：那一趟从前也走全文，现在走同一份装配。
    #[test]
    fn read_texts_matches_the_full_pass_chapter_for_chapter() {
        let dir = tempfile::tempdir().unwrap();
        let books = [
            sample(dir.path()),
            two_fonts(dir.path()),
            book_of(dir.path(), "t50.mobi", 50, 220),
            book_of(dir.path(), "t1.mobi", 1, 40),
        ];
        for path in books {
            let Content::Reflow(full) = MobiAdapter.read(&path).unwrap().1 else {
                panic!("MOBI 该读成可重排");
            };
            let texts = MobiAdapter
                .read_texts(&path)
                .expect("合成书该有『只要正文』那一趟");
            assert_eq!(texts.len(), full.chapters.len(), "{}", path.display());
            for (i, (text, chapter)) in texts.iter().zip(&full.chapters).enumerate() {
                assert_eq!(
                    text,
                    chapter.source(),
                    "第 {i} 章的正文两条路不一样（{}）",
                    path.display()
                );
            }
            for (i, text) in texts.iter().enumerate() {
                let at = crate::ChapterLocator::Section(i);
                assert_eq!(
                    MobiAdapter.chapter_source(&path, &at).unwrap(),
                    *text,
                    "第 {i} 章的兜底那一路不一样"
                );
            }
        }
    }

    /// 快版改写与「改写前那一版」**逐字节**对拍（形状在测试里摆出来，不依赖任何一本书）。
    #[test]
    fn the_fast_rewrite_writes_exactly_what_the_reference_wrote() {
        let ctx = super::Rewrite {
            first_resource: Some(10),
            ids: &[],
            chapter: 0,
        };
        let cases = [
            // 图：recindex（KF7）/ kindle:embed / kindle:flow，以及指不到资源的那一张。
            r#"<p>x<img recindex="00001"/></p>"#,
            r#"<img src="kindle:embed:0002?mime=image/jpeg">"#,
            r#"<img src='kindle:flow:0003?mime=image/svg+xml'>"#,
            r#"<img alt="没有来源的一张">"#,
            // 整条要去掉的那几族，含全大写写法。
            r#"<MBP:PAGEBREAK/><mbp:section/><link href="a"/><meta x="1"/><style/></mobi:xxx/>"#,
            // 属性里有中文：`<` 之外的多字节位置曾经是 `&tag[i..]` 的panic 风险点。
            r#"<img recindex="3" alt="一幅图，很大">"#,
            r#"<p lang="zh">中文<b>粗</b></p>"#,
            // 起了头没有尾的那个 `<`：它得留在正文里，而不是吞掉后半段。
            "1 < 2 与 3 > 4 之间<b>粗</b>",
            "<b>没收尾",
            // 空表与首尾。
            "",
            "<p></p>",
        ];
        for html in cases {
            assert_eq!(
                rewrite_html(html, &ctx),
                rewrite_html_reference(html, &ctx),
                "形状：{html}"
            );
        }
        // 带 `kindle:pos:fid` 的那一支走的是另一段代码（锚点表要查），单独摆一条。
        let chapter0 = r#"<p id="top">甲</p>"#;
        let ids = vec![super::id_positions(chapter0)];
        let linked = super::Rewrite {
            first_resource: Some(1),
            ids: &ids,
            chapter: 0,
        };
        for tag in [
            r##"<a href="kindle:pos:fid:0000:off:0000">同章</a>"##,
            r##"<a href="kindle:pos:fid:0001:off:000A?x=1">跨章</a>"##,
            r##"<a href="#top">普通锚点</a>"##,
        ] {
            assert_eq!(
                rewrite_html(tag, &linked),
                rewrite_html_reference(tag, &linked),
                "形状：{tag}"
            );
        }
    }

    /// 改写前那一版：逐字符 push、每个标签现造一个 `String`。
    ///
    /// 它在这里活着只为一件事 —— 给快版一个不用人算的对照。
    fn rewrite_html_reference(html: &str, ctx: &super::Rewrite) -> String {
        let mut out = String::with_capacity(html.len());
        let mut i = 0usize;
        while i < html.len() {
            let rest = &html[i..];
            if rest.starts_with('<')
                && let Some(end) = rest.find('>')
            {
                if let Some(text) = rewrite_tag_reference(&rest[..=end], ctx) {
                    out.push_str(&text);
                }
                i += end + 1;
                continue;
            }
            let ch = rest.chars().next().expect("非空");
            out.push(ch);
            i += ch.len_utf8();
        }
        out
    }

    /// 慢版的单标签决定：`Some(要写的)` / `None(丢掉)`。
    fn rewrite_tag_reference(tag: &str, ctx: &super::Rewrite) -> Option<String> {
        let lower = super::tag_name(tag).to_ascii_lowercase();
        if lower.starts_with("mbp:")
            || lower.starts_with("mbh:")
            || lower.starts_with("mobi:")
            || lower == "link"
            || lower == "meta"
            || lower == "style"
        {
            return None;
        }
        if lower != "img" && lower != "image" {
            return Some(super::rewrite_posfid(tag, ctx));
        }
        let first_resource = ctx.first_resource;
        if let Some(record) = super::attr_value(tag, "recindex")
            .and_then(super::parse_index)
            .and_then(|n| first_resource.map(|base| base + n.saturating_sub(1)))
        {
            return Some(format!("<img src=\"{}{record}\"/>", super::IMAGE_PREFIX));
        }
        let src = super::attr_value(tag, "src").or_else(|| super::attr_value(tag, "xlink:href"));
        if let Some(value) = src {
            if let Some(rest) = value.strip_prefix("kindle:embed:") {
                let (num, _) = super::leading_digits(rest);
                if let (Some(n), Some(base)) = (num.parse::<usize>().ok(), first_resource) {
                    return Some(format!(
                        "<img src=\"{}{}\"/>",
                        super::IMAGE_PREFIX,
                        base + n.saturating_sub(1)
                    ));
                }
            }
            if let Some(rest) = value.strip_prefix("kindle:flow:") {
                let (num, _) = super::leading_digits(rest);
                if let Ok(n) = num.parse::<usize>() {
                    return Some(format!("<img src=\"{}{n}\"/>", super::SVG_PREFIX));
                }
            }
        }
        None
    }

    /// 锚点表：快版（两条 `find`）与慢版（逐字节试针）对拍。
    #[test]
    fn the_fast_anchor_scan_finds_the_same_ids_as_the_reference() {
        let cases = [
            r#"<p id="a">x</p>"#,
            r#"<p aid="b">x</p>"#,
            r##"<p id="a" aid="b">x<a href="#a">跳</a></p>"##,
            r#"<p data-id="c">这个不算锚点</p>"#,
            r#"<p xid="d">也不算</p>"#,
            "<p>中文<p id=\"甲\">乙</p>",
            r#"<p id=">没有收尾引号</p>"#,
            r#"<p id="a">id="b" 在正文里也算一次</p>"#,
            "",
            "id",
            "id=",
            r#"<img src="kindle:embed:0001" alt="id=1">"#,
        ];
        for html in cases {
            assert_eq!(
                super::id_positions(html),
                id_positions_reference(html),
                "形状：{html}"
            );
        }
    }

    /// 改写前那一版锚点扫描（逐字节 `starts_with`）。
    fn id_positions_reference(html: &str) -> Vec<(usize, String)> {
        let bytes = html.as_bytes();
        let mut out = Vec::new();
        let mut i = 0usize;
        while i + 3 <= bytes.len() {
            let at_id = bytes[i..].starts_with(b"id=\"");
            let at_aid = bytes[i..].starts_with(b"aid=\"");
            let boundary = i == 0 || !super::is_attr_char(bytes[i - 1]);
            if boundary && (at_id || at_aid) {
                let key_len = if at_aid { 5 } else { 4 };
                let start = i + key_len;
                if let Some(end) = bytes[start..].iter().position(|&b| b == b'"') {
                    if let Ok(value) = std::str::from_utf8(&bytes[start..start + end]) {
                        out.push((i, value.to_string()));
                    }
                    i = start + end + 1;
                    continue;
                }
            }
            i += 1;
        }
        out
    }

    /// 二分找「偏移 ≤ off 的最后一个 id」与线性回扫对拍。
    #[test]
    fn the_binary_lookup_agrees_with_the_linear_scan() {
        let list: Vec<(usize, String)> = vec![
            (0, "a".into()),
            (5, "b".into()),
            (5, "dup".into()),
            (9, "c".into()),
            (1000, "far".into()),
        ];
        for off in 0..40 {
            let linear = list
                .iter()
                .rev()
                .find(|(pos, _)| *pos <= off)
                .map(|(_, id)| id.as_str());
            assert_eq!(super::last_id_at_or_before(&list, off), linear, "off {off}");
        }
        assert_eq!(super::last_id_at_or_before(&list, 0), Some("a"));
        assert_eq!(super::last_id_at_or_before(&[], 0), None);
    }

    /// **索引那一趟的字数与全文那一趟逐章相同** —— 这是「导入时不必改写正文」那条判据的门。
    ///
    /// 省下的那一趟是整本改写 + 每章一个 `String`（实测 2.7 MB 那本 29 ms）。它成立因为
    /// 改写只动属性里的 href 与整条去掉的空标签，**文字节点一格都不动**；一旦哪天改写
    /// 学会了删字（比如真去处理 `<style>` 的内容），这一条就红，而那正是库里字数与屏上
    /// 字数分家的开始。
    #[test]
    fn the_index_pass_counts_the_same_visible_chars_as_the_full_pass() {
        let dir = tempfile::tempdir().unwrap();
        let books = [
            sample(dir.path()),
            two_fonts(dir.path()),
            book_of(dir.path(), "s50.mobi", 50, 220),
            book_of(dir.path(), "s3.mobi", 3, 40),
        ];
        for path in books {
            let Content::Reflow(full) = MobiAdapter.read(&path).unwrap().1 else {
                panic!("MOBI 该读成可重排");
            };
            let Content::Reflow(indexed) = MobiAdapter.index(&path).unwrap().1 else {
                panic!("MOBI 该读成可重排");
            };
            assert_eq!(full.chapters.len(), indexed.chapters.len());
            for (i, (full, indexed)) in full.chapters.iter().zip(&indexed.chapters).enumerate() {
                assert_eq!(
                    full.char_len(),
                    indexed.char_len,
                    "第 {i} 章的字数两条路不一样（{}）",
                    path.display()
                );
            }
        }
    }

    /// 同上，跑在真书上（语料不在仓库里，所以这条默认不跑）。
    ///
    /// 跑法：`cargo test --release -p folio-core --lib -- --ignored --nocapture the_index_pass_on_real`
    #[test]
    #[ignore = "真书语料门：reference/ 不在仓库里"]
    fn the_index_pass_counts_the_same_visible_chars_on_real_books() {
        let mut checked = 0usize;
        for path in stage_corpus() {
            let (Ok(full), Ok(indexed)) = (MobiAdapter.read(&path), MobiAdapter.index(&path))
            else {
                continue;
            };
            let (Content::Reflow(full), Content::Reflow(indexed)) = (full.1, indexed.1) else {
                continue;
            };
            for (i, (a, b)) in full.chapters.iter().zip(&indexed.chapters).enumerate() {
                assert_eq!(
                    a.char_len(),
                    b.char_len,
                    "{} 第 {i} 章字数两条路不一样",
                    path.display()
                );
            }
            checked += 1;
        }
        assert!(checked >= 3, "只跑了 {checked} 本真书，这条门等于没跑");
        println!("真书字数对拍：{checked} 本一致");
    }

    /// 切章快版在**真书**上的对拍（语料不在仓库里，默认不跑）。
    #[test]
    #[ignore = "真书语料门：reference/ 不在仓库里"]
    fn the_fast_rewrite_matches_the_reference_on_real_books() {
        let mut books = 0usize;
        let mut bytes = 0usize;
        for path in stage_corpus() {
            let Ok(opened) = super::open(&path) else {
                continue;
            };
            if opened.header.encryption_type != 0 {
                continue;
            }
            let read = rawml::palm_records(&opened.db);
            let heads = rawml::palm_heads(&opened.db);
            let Some(mut rec) = rawml::reconstruct(
                &read,
                &heads,
                &opened.header,
                opened.kf8_offset,
                opened.first_resource,
            ) else {
                continue;
            };
            let (_, parts) =
                super::chapter_shapes(&read, &mut rec, &opened.header, opened.kf8_offset);
            // 逐章对拍要用**同一张**锚点表：KF8 的跨章引用要查别的章，每章各建一份就不是
            // 同一件事了（`rewrite_chapters` 里建的正是整本那一张）。
            let ids: Vec<Vec<(usize, String)>> = if opened.header.is_kf8() {
                parts.iter().map(|p| super::id_positions(p)).collect()
            } else {
                Vec::new()
            };
            for (i, part) in parts.iter().enumerate() {
                let ctx = super::Rewrite {
                    first_resource: opened.first_resource,
                    ids: &ids,
                    chapter: i,
                };
                assert_eq!(
                    rewrite_html(part, &ctx),
                    rewrite_html_reference(part, &ctx),
                    "{} 第 {i} 章改写结果不一样",
                    path.display()
                );
                assert_eq!(
                    super::id_positions(part),
                    id_positions_reference(part),
                    "{} 第 {i} 章锚点表不一样",
                    path.display()
                );
                bytes += part.len();
            }
            books += 1;
        }
        assert!(books >= 3, "只跑了 {books} 本真书，这条门等于没跑");
        println!("真书改写 + 锚点对拍：{books} 本 / {} KB 一致", bytes / 1024);
    }

    /// 封面那一笔贵事拆开：读字节 / 解码 / 缩 / 重编，各多少毫秒。
    ///
    /// 拆开是因为 `shrink_image` 的文档自己说它贵（整份解码 + 一律重编 JPEG），而这一趟
    /// **开书也在付**（`read` → `cover_bytes`），可开书那一方根本不看这一格。
    fn cover_split(opened: &super::Opened) {
        use std::time::Instant;
        let Some(offset) = opened
            .header
            .exth_uint(header::EXTH_COVEROFFSET)
            .or_else(|| opened.header.exth_uint(header::EXTH_THUMBOFFSET))
        else {
            return;
        };
        let Some(record) = opened.first_resource.map(|base| base + offset as usize) else {
            return;
        };
        let started = Instant::now();
        let Some(raw) = opened.db.read(record) else {
            return;
        };
        let read_ms = started.elapsed().as_secs_f64() * 1000.0;
        let (w, h) = crate::content::format::image_dims(&raw);
        let started = Instant::now();
        let decoded = image::ImageReader::new(std::io::Cursor::new(&raw))
            .with_guessed_format()
            .ok()
            .and_then(|r| r.decode().ok());
        let decode_ms = started.elapsed().as_secs_f64() * 1000.0;
        let Some(img) = decoded else {
            println!(
                "     封面拆开 {} KB @记录 {record}：读 {read_ms:.1} ms · 解不开（{w}×{h}）",
                raw.len() / 1024
            );
            return;
        };
        let started = Instant::now();
        let small = img.thumbnail(512, 512);
        let scale_ms = started.elapsed().as_secs_f64() * 1000.0;
        let started = Instant::now();
        let rgb = small.to_rgb8();
        let mut out = Vec::new();
        let encoded = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 82)
            .encode_image(&rgb)
            .is_ok();
        let encode_ms = started.elapsed().as_secs_f64() * 1000.0;
        println!(
            "     封面拆开 {} KB {}×{}：读 {:>5.1} · 解 {:>5.1} · 缩 {:>5.1} · 编 {:>5.1} ms → {} KB{}",
            raw.len() / 1024,
            img.width(),
            img.height(),
            read_ms,
            decode_ms,
            scale_ms,
            encode_ms,
            out.len() / 1024,
            if encoded { "" } else { "  （编失败）" }
        );
    }
}
