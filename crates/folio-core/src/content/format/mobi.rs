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
//! 这一趟的价钱与「解完整本」同价，实测（合成 600 章、正文不压缩那一档，所以是**下界**）：
//! 整本 109 ms，**翻一章 107 ms** —— 顺着读完整本就是 600 个 107 ms。所以那一次的结果由
//! `BookDocument` 的整本 memo 收下（`content::document::BookDocument::section_texts`）：
//! 一次会话一本书只解一遍，之后取章是一次下标取值。驻留是整本改写后的 XHTML，实测
//! 600 章 405 KB，关书即走、不落库（口径见 `plans/text-out-of-db.md`）。
//! 换来的仍是「不改 locator 形状、不动库 schema」。
//!
//! ## 图片地址
//!
//! MOBI 的图**没有文件名**，只有记录号。正文里 `recindex="N"`（KF7）或
//! `kindle:embed:NNNN`（KF8）按 `记录号 = first_resource + N - 1` 定位。我们把它改写成
//! `<img src="mobi-img/<记录号>"/>`，`image_bytes` 再按记录号回容器取字节 —— 与
//! FB2 用 `<binary>` 的 id 当 href 是同一条思路（href 只是一个键，不是文件名）。

#[allow(dead_code)] // 装配那一步（KF8/KF7 分岔与 provider）由下一轮接上。
mod codec;
#[allow(dead_code)]
mod encoding;
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

use super::palmdb::PalmDb;
use super::{
    Chapter, ChapterLocator, Content, DocMeta, DocumentProvider, FlowDoc, FontBytes, Format,
    ImageRes, shrink_image, stem_of,
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
            let rec = rawml::reconstruct(
                &read,
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
        let Ok(opened) = open(path) else {
            return Vec::new();
        };
        let read = rawml::palm_records(&opened.db);
        let Some(rec) = rawml::reconstruct(
            &read,
            &opened.header,
            opened.kf8_offset,
            opened.first_resource,
        ) else {
            return Vec::new();
        };
        rec.resources
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

/// 索引 / 全文共用的一趟。
fn read_with(path: &Path, with_text: bool) -> Result<(DocMeta, Content)> {
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

    let read = rawml::palm_records(&opened.db);
    let rec = rawml::reconstruct(&read, header, opened.kf8_offset, opened.first_resource)
        .ok_or_else(|| {
            crate::Error::UnsupportedFormat(
                "MOBI 正文解压失败（记录或 HUFF/CDIC 表坏了）".to_string(),
            )
        })?;

    let is_kf8 = header.is_kf8();
    let first_resource = opened.first_resource;
    let mut chapters: Vec<Chapter> = Vec::new();

    if is_kf8 {
        // 先把每章的锚点表扫出来，再逐章改写 —— `kindle:pos:fid` 的跨章目标要查别的章。
        let ids: Vec<Vec<(usize, String)>> =
            rec.markup.iter().map(|html| id_positions(html)).collect();
        let titles = kf8_titles(&read, header, opened.kf8_offset, rec.markup.len());
        for (i, html) in rec.markup.iter().enumerate() {
            let title = titles
                .get(i)
                .cloned()
                .unwrap_or_else(|| format!("第 {} 节", i + 1));
            let ctx = Rewrite {
                first_resource,
                ids: &ids,
                chapter: i,
            };
            chapters.push(Chapter::inline(title, rewrite_html(html, &ctx)));
        }
    } else if let Some(first) = rec.markup.first() {
        let ranges = rawml::split_kf7_ranges(first);
        let titles = kf7_titles(&read, header, opened.kf8_offset, &ranges);
        for (i, (_, part)) in ranges.into_iter().enumerate() {
            let title = titles
                .get(i)
                .cloned()
                .unwrap_or_else(|| format!("第 {} 节", i + 1));
            let ctx = Rewrite {
                first_resource,
                ids: &[],
                chapter: i,
            };
            chapters.push(Chapter::inline(title, rewrite_html(&part, &ctx)));
        }
    }
    // 一章都没有时留一章空的：让「打开了一本没内容的书」与「打不开」分开。
    if chapters.is_empty() {
        chapters.push(Chapter::inline(stem_of(path), "<body></body>".to_string()));
    }

    // 图片元数据：记录资源 + KF8 的 svg flow 段。
    let mut images: Vec<ImageRes> = Vec::new();
    for r in &rec.resources {
        if r.kind != rawml::ResourceKind::Image {
            continue;
        }
        let href = format!("{IMAGE_PREFIX}{}", r.record);
        let (width, height) = opened
            .db
            .read(r.record)
            .map(|bytes| super::image_dims(&bytes))
            .unwrap_or((0, 0));
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
    let cover = cover_bytes(&opened);

    // 索引那一趟：章表只留标题与序号，字数在这里白拿（正文已解出来了）。
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
        if let Some(slot) = titles.get_mut(fid as usize) {
            if slot.is_empty() && !title.is_empty() {
                *slot = title.to_string();
            }
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
        if let Some(i) = slot {
            if titles[i].is_empty() && !item.title.is_empty() {
                titles[i] = item.title.clone();
            }
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
    let mut i = 0usize;
    while i < html.len() {
        let rest = &html[i..];
        if rest.starts_with('<') {
            if let Some(end) = rest.find('>') {
                let tag = &rest[..=end];
                match rewrite_tag(tag, ctx) {
                    TagAction::Keep(text) => out.push_str(&text),
                    TagAction::Drop => {}
                }
                i += end + 1;
                continue;
            }
        }
        let ch = rest.chars().next().expect("非空");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

enum TagAction {
    Keep(String),
    Drop,
}

/// 单个标签的改写决定。
fn rewrite_tag(tag: &str, ctx: &Rewrite) -> TagAction {
    let name = tag_name(tag);
    let lower = name.to_ascii_lowercase();
    // MOBI 专有标签与 `<link>`：整条去掉（样式已另收，页分隔符在切章时用过了）。
    if lower.starts_with("mbp:")
        || lower.starts_with("mbh:")
        || lower.starts_with("mobi:")
        || lower == "link"
        || lower == "meta"
        || lower == "style"
    {
        return TagAction::Drop;
    }
    if lower != "img" && lower != "image" {
        // 其余标签原样保留，但把里面的 `kindle:pos:fid` 锚点改写成可解析的 href。
        return TagAction::Keep(rewrite_posfid(tag, ctx));
    }
    // 图：先看 recindex（KF7），再看 src/href 里的 kindle:embed（KF8）。
    let first_resource = ctx.first_resource;
    if let Some(record) = attr_value(tag, "recindex")
        .and_then(parse_index)
        .and_then(|n| first_resource.map(|base| base + n.saturating_sub(1)))
    {
        return TagAction::Keep(format!("<img src=\"{IMAGE_PREFIX}{record}\"/>"));
    }
    let src = attr_value(tag, "src").or_else(|| attr_value(tag, "xlink:href"));
    if let Some(value) = src {
        if let Some(rest) = value.strip_prefix("kindle:embed:") {
            let (num, _) = leading_digits(rest);
            if let (Some(n), Some(base)) = (num.parse::<usize>().ok(), first_resource) {
                return TagAction::Keep(format!(
                    "<img src=\"{IMAGE_PREFIX}{}\"/>",
                    base + n.saturating_sub(1)
                ));
            }
        }
        if let Some(rest) = value.strip_prefix("kindle:flow:") {
            // `kindle:flow:NNNN?mime=...`：段号取到 `?` 之前。
            let (num, _) = leading_digits(rest);
            if let Ok(n) = num.parse::<usize>() {
                return TagAction::Keep(format!("<img src=\"{SVG_PREFIX}{n}\"/>"));
            }
        }
    }
    // 指不到任何资源的图：丢掉（排版层会给一块占位，别留一个空 `<img>`）。
    TagAction::Drop
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
fn last_id_at_or_before(list: &[(usize, String)], off: usize) -> Option<&str> {
    list.iter()
        .rev()
        .find(|(pos, _)| *pos <= off)
        .map(|(_, id)| id.as_str())
}

/// 一章正文里所有 `id="…"` / `aid="…"` 的 `(字节偏移, id)`，按出现顺序（即偏移升序）。
fn id_positions(html: &str) -> Vec<(usize, String)> {
    let bytes = html.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 3 <= bytes.len() {
        // 按**字节**比对，别按 `str` 切片：正文是 UTF-8，逐字节推进时下标会落在
        // 多字节字符中间，`&html[i..]` 会 panic。
        let at_id = bytes[i..].starts_with(b"id=\"");
        let at_aid = bytes[i..].starts_with(b"aid=\"");
        let boundary = i == 0 || !is_attr_char(bytes[i - 1]);
        if boundary && (at_id || at_aid) {
            // `id="` 是 4 个字符、`aid="` 是 5 个（含那个引号）。
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
    let mut i = 0usize;
    while i + name.len() <= tag.len() {
        // 属性名要在一个边界上：前面是空白。
        let at_boundary = i == 0 || bytes[i - 1].is_ascii_whitespace();
        if at_boundary
            && tag[i..].len() >= name.len()
            && tag[i..i + name.len()].eq_ignore_ascii_case(name)
        {
            let after = i + name.len();
            let mut j = after;
            while j < tag.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < tag.len() && bytes[j] == b'=' {
                j += 1;
                while j < tag.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < tag.len() {
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

    use super::{MobiAdapter, rewrite_html};
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

    /// 量「翻一章到底付多少钱」：现在的实现每章都重解**整本**，所以这一趟是 O(N²)。
    ///
    /// 跑法：`cargo test --release -p folio-core --lib -- --ignored --nocapture mobi_reparse`
    ///
    /// 三列对照着看才站得住：**索引**那一趟（开书付一次）、**全文**那一趟（整本解一遍的
    /// 下界）、**单步**（每章一次 `chapter_source`，只为了拿一章）。`单步/全文` 接近 1.0
    /// 就证明「重解整本」不是推测，而是每次翻章都在付的钱。
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
}
