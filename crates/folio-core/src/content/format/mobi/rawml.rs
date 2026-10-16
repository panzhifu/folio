//! MOBI 正文的重建：解压文本记录 → （KF8）按 FDST 切段、按 SKEL/FRAG 拼回「文件」→
//! 扫出资源记录。产物是**引擎就绪的 HTML 片段**（本层不做 `recindex` 重写与 mobi 标签
//! 剥除 —— 那是 provider 的格式知识，见 [`super`]）。
//!
//! 与 [`super::indx`] 同一条分层：这一层不认容器，只认「按号取字节」的回调
//! （[`Records`]）。`PalmDb` 在 provider 那一层接到这个回调上，测试则接一张内存表。
//!
//! ## KF7 与 KF8 是两条路
//!
//! **KF7**（老 mobi）：全部文本记录解压后连成**一条** HTML 流，章由流里的
//! `<mbp:pagebreak/>` 分隔（[`split_kf7`]）。
//!
//! **KF8**（AZW3）：解压后是一条 `flow` 字节区，先按 FDST 表切成若干「段」（html 段、
//! css 段、svg 段），再用 SKEL（骨架：一段段的区间）与 FRAG（片段：插进骨架里的字节）
//! 把 html 段拼回**多个** markup 文件 —— 每一个就是我们的一章。算法照
//! `reference/libmobi/src/parse_rawml.c` 的 `mobi_reconstruct_flow` /
//! `mobi_reconstruct_parts`（只读不抄，LGPL）。
//!
//! ## 三条降级口径
//!
//! **一、解压不出就 `None`**：HUFF/CDIC 表缺、正文记录读不到、FDST 区间越界 ——
//! 都是「这本书读不动」，而不是「读出一半」。半截正文在屏幕上是一本看着正常、
//! 打开是乱码的书，比一次拒绝坏得多。
//!
//! **二、资源、目录、CSS 缺了不算错**：它们是锦上添花。缺图就是那块地方空着，
//! 缺目录就是侧栏少一栏，缺 CSS 就是回到默认样式 —— 正文照读（与 epub 对图片、
//! 样式、目录同一条口径）。
//!
//! **三、KF8 重建失败退回整段**：SKEL/FRAG 读不出时把整个 html 段当一章交出去，
//! 而不是整本作废。这一点与 libmobi 一致（`skel == NULL` 时直接拷整段）。

use super::codec::{self, HuffCdic};
use super::header::{COMPRESSION_HUFFCDIC, COMPRESSION_NONE, COMPRESSION_PALMDOC, Header, NOTSET};
use super::indx;
use crate::content::format::palmdb::PalmDb;

/// 「按 PalmDB 记录号取原始字节」的回调。与 [`super::indx`] 同一个形状。
pub(crate) type Records<'a> = &'a dyn Fn(usize) -> Option<Vec<u8>>;

/// 一个资源记录的种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResourceKind {
    Image,
    Font,
    Audio,
    Video,
}

impl ResourceKind {
    /// 这一类要不要在重建时收下（音频、视频这一版不解，只认图与字体）。
    fn kept(self) -> bool {
        matches!(self, Self::Image | Self::Font)
    }
}

/// 重排正文之外的一条资源记录。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Resource {
    /// 绝对记录号（PalmDB 序号）。
    pub record: usize,
    pub kind: ResourceKind,
    pub media_type: &'static str,
}

/// KF8 的 `flow` 段类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlowType {
    Html,
    Css,
    Svg,
    Unknown,
}

/// 重建的产物。
#[derive(Debug, Clone, Default)]
pub(crate) struct Reconstructed {
    /// 章（引擎就绪的 HTML 片段）。KF7 是一条（由 [`split_kf7`] 再切）；KF8 是多条。
    pub markup: Vec<String>,
    /// 作者样式（KF8 的 css flow 段拼接；KF7 为空）。
    pub css: String,
    /// 图与字体资源。
    pub resources: Vec<Resource>,
    /// KF8 的 svg flow 段：`(段号, 字节)`，供 provider 登记成图片。
    pub flow_svgs: Vec<(usize, Vec<u8>)>,
}

/// 一步到位：解压 + 重建。
///
/// `kf8_offset` 是 hybrid 文件里 KF8 那一半的偏移（非 hybrid 是 0）；`first_resource`
/// 是首个资源记录的绝对号（`None` 时不扫资源）。
pub(crate) fn reconstruct(
    read: Records<'_>,
    header: &Header,
    kf8_offset: usize,
    first_resource: Option<usize>,
) -> Option<Reconstructed> {
    let text = decompress_text(read, header, kf8_offset)?;
    let fdst = read_fdst(read, header, kf8_offset);
    let flow = flow_parts(&text, fdst.as_deref());

    let mut out = Reconstructed {
        resources: scan_resources(read, first_resource),
        ..Default::default()
    };

    // flow[0] 是 html；其余段按 KF8 的 `kindle:flow:NNNN?mime=` 归类。
    let first_html = flow
        .first()
        .map(|part| String::from_utf8_lossy(part).into_owned());
    for (n, part) in flow.iter().enumerate().skip(1) {
        match first_html
            .as_deref()
            .map(|html| flow_type(html, n))
            .unwrap_or(FlowType::Unknown)
        {
            FlowType::Css => {
                out.css.push_str(&String::from_utf8_lossy(part));
                out.css.push('\n');
            }
            FlowType::Svg => out.flow_svgs.push((n, part.clone())),
            // html 段只该有一个头；其余未归类的段丢掉（不是我们认得的内容）。
            _ => {}
        }
    }

    // markup：KF8 用 SKEL/FRAG 重建多个文件；否则整段当一章。
    let skel = if header.skeleton_index != NOTSET {
        Some(indx::read_index(
            read,
            header.skeleton_index as usize + kf8_offset,
            header.encoding,
        ))
    } else {
        None
    };
    let frag = if header.fragment_index != NOTSET {
        Some(indx::read_index(
            read,
            header.fragment_index as usize + kf8_offset,
            header.encoding,
        ))
    } else {
        None
    };

    out.markup = match (flow.first(), skel.as_ref(), frag.as_ref()) {
        (Some(html), Some(skel), Some(frag)) if !skel.entries.is_empty() => {
            match reconstruct_markup(html, skel, frag) {
                // 重建失败退回整段（降级口径三）。
                Some(parts) if !parts.is_empty() => parts,
                _ => vec![String::from_utf8_lossy(html).into_owned()],
            }
        }
        (Some(html), _, _) => vec![String::from_utf8_lossy(html).into_owned()],
        (None, _, _) => Vec::new(),
    };

    Some(out)
}

/// 解压全部文本记录，拼成一整段字节。
///
/// HUFF/CDIC 的表按需建一次；解压出的总量以头里声明的 `text_length` 为上界
/// （HUFF 码流尾部有补齐位，没上界会多吐一截没人看得懂的尾巴）。
pub(crate) fn decompress_text(
    read: Records<'_>,
    header: &Header,
    kf8_offset: usize,
) -> Option<Vec<u8>> {
    let count = header.text_record_count as usize;
    if count == 0 {
        return None;
    }
    let start = 1 + kf8_offset;
    let huff = if header.compression == COMPRESSION_HUFFCDIC {
        Some(build_huffcdic(read, header, kf8_offset)?)
    } else {
        None
    };
    let cap = header.text_length as usize;
    let mut out = Vec::with_capacity(cap.min(1 << 20));
    for i in 0..count {
        // 记录读不到就到此为止（与 libmobi 的 `curr == NULL` 同口径），
        // 但不为一条缺失记录把整本判死。
        let Some(rec) = read(start + i) else {
            break;
        };
        let extra = codec::extras_len(&rec, header.extra_flags)?;
        if extra > rec.len() {
            return None;
        }
        let body = &rec[..rec.len() - extra];
        if body.is_empty() {
            continue;
        }
        match header.compression {
            COMPRESSION_NONE => out.extend_from_slice(body),
            COMPRESSION_PALMDOC => codec::palmdoc_decompress(body, &mut out),
            COMPRESSION_HUFFCDIC => {
                let limit = cap.saturating_sub(out.len());
                huff.as_ref()?.decompress(body, limit, &mut out)?;
            }
            _ => return None,
        }
    }
    Some(out)
}

/// 按 HUFF 头 + 其后连续的 CDIC 记录建表。
fn build_huffcdic(read: Records<'_>, header: &Header, kf8_offset: usize) -> Option<HuffCdic> {
    let index = header.huff_rec_index;
    let count = header.huff_rec_count;
    if index == NOTSET || !(2..=0xFFFF).contains(&count) {
        return None;
    }
    let first = index as usize + kf8_offset;
    let huff = read(first)?;
    let mut cdics = Vec::with_capacity(count as usize - 1);
    for i in 0..(count as usize - 1) {
        cdics.push(read(first + 1 + i)?);
    }
    let refs: Vec<&[u8]> = cdics.iter().map(Vec::as_slice).collect();
    HuffCdic::build(&huff, &refs)
}

/// 读 KF8 的 FDST 段表：`(段起点, 段终点)`。没有 / 段数 ≤ 1 时 `None`。
fn read_fdst(read: Records<'_>, header: &Header, kf8_offset: usize) -> Option<Vec<(u32, u32)>> {
    if header.fdst_index == NOTSET {
        return None;
    }
    let rec = read(header.fdst_index as usize + kf8_offset)?;
    if rec.len() < 12 || rec.get(..4) != Some(b"FDST") {
        return None;
    }
    let data_offset = u32_at(&rec, 4)? as usize;
    let count = u32_at(&rec, 8)? as usize;
    if count <= 1 || count > 0xFFFF {
        return None;
    }
    // 段表要装得下 count 对 (start, end)。
    if data_offset + count * 8 > rec.len() {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let at = data_offset + i * 8;
        out.push((u32_at(&rec, at)?, u32_at(&rec, at + 4)?));
    }
    Some(out)
}

/// 把一整段 rawml 按 FDST 切成若干 flow 段；没有 FDST 时就是**一段**。
fn flow_parts(text: &[u8], fdst: Option<&[(u32, u32)]>) -> Vec<Vec<u8>> {
    match fdst {
        Some(sections) if sections.len() > 1 => {
            let mut out = Vec::with_capacity(sections.len());
            for &(start, end) in sections {
                let (start, end) = (start as usize, end as usize);
                if start > end || end > text.len() {
                    // 一段越界：宁可少一段，别 panic。
                    break;
                }
                out.push(text[start..end].to_vec());
            }
            out
        }
        _ => vec![text.to_vec()],
    }
}

/// KF8 的 flow 段归类：在 html 段里找 `kindle:flow:NNNN?mime=<type>`。
fn flow_type(html: &str, part: usize) -> FlowType {
    let needle = format!("kindle:flow:{part:04}?mime=");
    let Some(at) = html.find(&needle) else {
        return FlowType::Unknown;
    };
    let rest = &html[at + needle.len()..];
    let value = rest
        .split(|c: char| c == '"' || c == '\'' || c == '?' || c == '&' || c.is_whitespace())
        .next()
        .unwrap_or("");
    if value.contains("text/css") {
        FlowType::Css
    } else if value.contains("image/svg+xml") {
        FlowType::Svg
    } else {
        FlowType::Unknown
    }
}

/// 按 SKEL/FRAG 把 html 段重建成多个 markup 文件。
///
/// 返回 `None` 表示**结构不成**（片段数对不上、区间越界）；调用方退回整段。
fn reconstruct_markup(flow: &[u8], skel: &indx::Index, frag: &indx::Index) -> Option<Vec<String>> {
    let mut parts = Vec::with_capacity(skel.entries.len());
    // 已重建出来的总长 —— FRAG 的插入位置是相对它的（见 parse_rawml.c）。
    let mut curr_position = 0usize;
    let mut frag_cursor = 0usize;
    for (i, entry) in skel.entries.iter().enumerate() {
        let fragments_count = entry.tag_value(1, 0)? as usize;
        let skel_position = entry.tag_value(6, 0)? as usize;
        let skel_length = entry.tag_value(6, 1)? as usize;
        if skel_position + skel_length > flow.len() {
            return None;
        }
        // 一条「骨架片段 + 若干插入片段」组成的列表：按插入位置排序的 (位置, 字节)。
        let mut pieces: Vec<(usize, Vec<u8>)> = Vec::with_capacity(fragments_count + 1);
        pieces.push((0, flow[skel_position..skel_position + skel_length].to_vec()));
        let mut total = skel_length;

        for _ in 0..fragments_count {
            let frag_entry = frag.entries.get(frag_cursor)?;
            frag_cursor += 1;
            // FRAG 的插入位置按标签 6.0 给（十进制写在 label 里的是老写法，两者都试）。
            let insert_abs = frag_entry
                .tag_value(6, 0)
                .map(|v| v as usize)
                .or_else(|| frag_entry.label.parse::<usize>().ok())?;
            let file_number = frag_entry.tag_value(3, 0)? as usize;
            if file_number != i {
                return None;
            }
            let frag_length = frag_entry.tag_value(6, 1)? as usize;
            // 片段的字节紧跟在本段的骨架/前一帧之后，按游标顺序取。
            let frag_start = skel_position + total;
            if frag_start + frag_length > flow.len() {
                return None;
            }
            let bytes = flow[frag_start..frag_start + frag_length].to_vec();
            let insert = insert_abs.saturating_sub(curr_position).min(total);
            pieces.push((insert, bytes));
            total += frag_length;
        }

        // 按插入位置稳定排序后拼接。
        pieces.sort_by_key(|(pos, _)| *pos);
        let mut text = Vec::with_capacity(total);
        for (_, bytes) in pieces {
            text.extend_from_slice(&bytes);
        }
        parts.push(String::from_utf8_lossy(&text).into_owned());
        curr_position += total;
    }
    Some(parts)
}

/// 扫资源记录：从 `first_resource` 起，按魔数认图 / 字体，遇到 EOF / BOUNDARY 终止。
///
/// 认不出的记录跳过（KindleGen 会在资源之间塞对齐/填充）。`first_resource` 为 `None`
/// 时**不扫**（宁可没有图，也别把正文记录当图）。
fn scan_resources(read: Records<'_>, first_resource: Option<usize>) -> Vec<Resource> {
    let Some(start) = first_resource else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut record = start;
    while let Some(bytes) = read(record) {
        if is_end_marker(&bytes) {
            break;
        }
        if let Some((kind, media_type)) = classify_resource(&bytes)
            && kind.kept()
        {
            out.push(Resource {
                record,
                kind,
                media_type,
            });
        }
        record += 1;
        if record > start + 100_000 {
            break;
        }
    }
    out
}

/// 资源记录的魔数分类。
fn classify_resource(bytes: &[u8]) -> Option<(ResourceKind, &'static str)> {
    if bytes.len() >= 3 && bytes[..3] == [0xff, 0xd8, 0xff] {
        return Some((ResourceKind::Image, "image/jpeg"));
    }
    if bytes.len() >= 4 && bytes.get(..4) == Some(b"GIF8".as_slice()) {
        return Some((ResourceKind::Image, "image/gif"));
    }
    if bytes.len() >= 8 && bytes.get(..8) == Some(b"\x89PNG\r\n\x1a\n".as_slice()) {
        return Some((ResourceKind::Image, "image/png"));
    }
    if bytes.len() >= 6
        && bytes.get(..2) == Some(b"BM".as_slice())
        && u32_at_le(bytes, 2).is_some_and(|size| size as usize == bytes.len())
    {
        return Some((ResourceKind::Image, "image/bmp"));
    }
    if bytes.len() >= 4 && bytes.get(..4) == Some(b"FONT".as_slice()) {
        return Some((ResourceKind::Font, "application/x-font"));
    }
    if bytes.len() >= 4 && bytes.get(..4) == Some(b"AUDI".as_slice()) {
        return Some((ResourceKind::Audio, "audio/mpeg"));
    }
    if bytes.len() >= 4 && bytes.get(..4) == Some(b"VIDE".as_slice()) {
        return Some((ResourceKind::Video, "video/mp4"));
    }
    None
}

/// 资源区的终止标记：`EOF_MAGIC`（`\xe9\x8e\r\n`）或 `BOUNDARY_MAGIC`（`BOUNDARY`）。
fn is_end_marker(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\xe9\x8e\r\n") || bytes.starts_with(b"BOUNDARY")
}

/// KF7 的章切分：按 `<mbp:pagebreak/>`（含不带斜杠、大小写不一）切开。
///
/// 没有分页符时整段就是一章。空白片段丢掉，但**全空时留一段空的**，好让
/// 「打开了一本没内容的书」与「打不开这本书」在界面上能分开（同 FB2 的处理）。
pub(crate) fn split_kf7(html: &str) -> Vec<String> {
    split_kf7_ranges(html)
        .into_iter()
        .map(|(_, part)| part)
        .collect()
}

/// 同 [`split_kf7`]，但每段带上它在原串里的**字节起点** —— 供按 `filepos`
/// 给章配标题（NCX 的 filepos 就是原串里的字节偏移）。
pub(crate) fn split_kf7_ranges(html: &str) -> Vec<(usize, String)> {
    let mut parts: Vec<(usize, String)> = Vec::new();
    let mut rest = html;
    let mut base = 0usize;
    loop {
        match find_pagebreak(rest) {
            Some((start, end)) => {
                parts.push((base, rest[..start].to_string()));
                base += end;
                rest = &rest[end..];
            }
            None => {
                parts.push((base, rest.to_string()));
                break;
            }
        }
    }
    parts.retain(|(_, p)| !p.trim().is_empty());
    if parts.is_empty() {
        parts.push((0, html.to_string()));
    }
    parts
}

/// 找下一个 `<mbp:pagebreak...>` 标签，返回它在串里的 `(起点, 终点)`。
///
/// 大小写不敏感（真实文件两种都写），属性（`pagebreak` 之后到 `>` 之前）整段吃掉，
/// 所以 `<mbp:pagebreak/>` 与 `<mbp:pagebreak style="x">` 都认。
fn find_pagebreak(text: &str) -> Option<(usize, usize)> {
    let lower = text.to_ascii_lowercase();
    let at = lower.find("<mbp:pagebreak")?;
    let close = text[at..].find('>')?;
    Some((at, at + close + 1))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn u32_at_le(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// `PalmDb` → [`Records`] 的适配，provider 用。
pub(crate) fn palm_records(db: &PalmDb) -> impl Fn(usize) -> Option<Vec<u8>> + '_ {
    move |i| db.read(i)
}

#[cfg(test)]
mod tests {
    //! 这一层的断言只看**解出的字节与重建出的 markup**：把记录表摆好，看结果。

    use super::FlowType;
    use super::{
        ResourceKind, classify_resource, decompress_text, flow_type, reconstruct, split_kf7,
    };
    use crate::content::format::mobi::header::{COMPRESSION_NONE, COMPRESSION_PALMDOC, Header};

    /// 一张记录表 → [`super::Records`] 回调。
    fn reader(table: &[Vec<u8>]) -> impl Fn(usize) -> Option<Vec<u8>> + '_ {
        move |i| table.get(i).cloned()
    }

    /// 一份只填了「压缩 / 正文记录数 / 正文长度」的头，够 `decompress_text` 用。
    fn plain_header(compression: u16, count: u16, length: u32) -> Header {
        let mut rec = vec![0u8; 0x120];
        rec[0..2].copy_from_slice(&compression.to_be_bytes());
        rec[4..8].copy_from_slice(&length.to_be_bytes());
        rec[8..10].copy_from_slice(&count.to_be_bytes());
        rec[10..12].copy_from_slice(&4096u16.to_be_bytes());
        rec[16..20].copy_from_slice(b"MOBI");
        rec[20..24].copy_from_slice(&232u32.to_be_bytes());
        rec[28..32].copy_from_slice(&65001u32.to_be_bytes());
        Header::parse(&rec).unwrap()
    }

    #[test]
    fn kf7_splits_on_pagebreaks_in_any_spelling() {
        let html = "a<mbp:pagebreak/>b<MBP:PAGEBREAK>c";
        assert_eq!(split_kf7(html), vec!["a", "b", "c"]);
    }

    #[test]
    fn kf7_without_pagebreaks_is_one_chapter() {
        assert_eq!(split_kf7("<p>只有一段</p>"), vec!["<p>只有一段</p>"]);
    }

    /// 全是分页符（没有任何正文）时仍要留一段，好让「空书」与「打不开」分开。
    #[test]
    fn an_all_pagebreak_stream_still_yields_one_part() {
        let parts = split_kf7("<mbp:pagebreak/><mbp:pagebreak/>");
        assert_eq!(parts.len(), 1);
    }

    #[test]
    fn flow_parts_are_typed_by_the_kindle_flow_hint() {
        let html = r#"<link href="kindle:flow:0001?mime=text/css"/><image xlink:href="kindle:flow:0002?mime=image/svg+xml"/>"#;
        assert_eq!(flow_type(html, 1), FlowType::Css);
        assert_eq!(flow_type(html, 2), FlowType::Svg);
        assert_eq!(flow_type(html, 9), FlowType::Unknown);
    }

    #[test]
    fn resources_are_recognized_by_magic() {
        assert_eq!(
            classify_resource(&[0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10]),
            Some((ResourceKind::Image, "image/jpeg"))
        );
        assert_eq!(
            classify_resource(b"\x89PNG\r\n\x1a\n\x00\x00"),
            Some((ResourceKind::Image, "image/png"))
        );
        assert_eq!(
            classify_resource(&[b'F', b'O', b'N', b'T', 1, 2, 3, 4]),
            Some((ResourceKind::Font, "application/x-font"))
        );
        // 认不出的记录（对齐填充、正文残留）安静地跳过。
        assert_eq!(classify_resource(b"random bytes"), None);
    }

    #[test]
    fn end_markers_stop_the_resource_scan() {
        assert!(super::is_end_marker(b"BOUNDARY----"));
        assert!(super::is_end_marker(b"\xe9\x8e\r\n"));
        assert!(!super::is_end_marker(b"\x89PNG\r\n\x1a\n"));
    }

    /// 完整链路：记录 1 = 两章（不压）→ 解压 → 按分页符切章。
    #[test]
    fn a_plain_text_book_decompresses_and_splits() {
        let body = b"<p>one</p><mbp:pagebreak/><p>two</p>".to_vec();
        let table = vec![Vec::new(), body.clone()]; // 记录 0 空，记录 1 = 正文
        let header = plain_header(COMPRESSION_NONE, 1, body.len() as u32);
        let text = decompress_text(&reader(&table), &header, 0).unwrap();
        let markup = String::from_utf8(text).unwrap();
        let parts = split_kf7(&markup);
        assert_eq!(parts.len(), 2);
        assert!(parts[0].contains("one"));
        assert!(parts[1].contains("two"));
    }

    /// PalmDOC 压缩的正文也要解对（钉一下这条链是通的）。
    #[test]
    fn palmdoc_records_decompress_through_the_same_path() {
        // 未压的合法 PalmDOC 帧：ASCII 原样（0x09..=0x7f 直接是字节）。
        let body = b"<p>hello</p>".to_vec();
        let table = vec![Vec::new(), body.clone()];
        let header = plain_header(COMPRESSION_PALMDOC, 1, body.len() as u32);
        let text = decompress_text(&reader(&table), &header, 0).unwrap();
        assert_eq!(text, body);
    }

    /// `reconstruct` 在没有 SKEL（KF7）时把整段当一个 markup，并扫出资源。
    #[test]
    fn kf7_reconstruct_yields_one_markup_and_scans_resources() {
        let body = b"<p>x</p>".to_vec();
        // 记录 0 空、记录 1 正文、记录 2 一张 PNG、记录 3 EOF。
        let png = b"\x89PNG\r\n\x1a\n0123".to_vec();
        let eof = b"\xe9\x8e\r\n".to_vec();
        let table = vec![Vec::new(), body.clone(), png, eof];
        let header = plain_header(COMPRESSION_NONE, 1, body.len() as u32);
        let out = reconstruct(&reader(&table), &header, 0, Some(2)).unwrap();
        assert_eq!(out.markup, vec!["<p>x</p>".to_string()]);
        assert_eq!(out.resources.len(), 1);
        assert_eq!(out.resources[0].record, 2);
        assert_eq!(out.resources[0].kind, ResourceKind::Image);
    }

    /// KF8 的整条重建：两段骨架，第一段插一个片段 —— 拼出来的两份 markup 要对得上，
    /// 且片段落在**插入位置**而不是尾巴上。
    #[test]
    fn kf8_skel_frag_reconstructs_two_markup_files() {
        use super::reconstruct_markup;
        use crate::content::format::mobi::indx::{Index, IndexEntry};

        // flow = [skel0][frag0][skel1]，逐段量出字节数再切片：
        //   "<p>A</p>\n" = 9、 "<b>x</b>" = 8、 "<p>B</p>\n" = 9
        let flow = b"<p>A</p>\n<b>x</b><p>B</p>\n";
        // 骨架：段 0 有 1 个片段（区间 0..9 = "<p>A</p>\n"），
        //       段 1 没有片段（区间 17..26 = "<p>B</p>\n"）。
        let skel = Index::from_entries(vec![
            IndexEntry {
                label: String::new(),
                tags: vec![(1, vec![1]), (6, vec![0, 9])],
            },
            IndexEntry {
                label: String::new(),
                tags: vec![(1, vec![0]), (6, vec![17, 9])],
            },
        ]);
        // 片段：文件号 0、插入位置 9（段 0 末尾）、长度 8（"<b>x</b>"）。
        let frag = Index::from_entries(vec![IndexEntry {
            label: String::new(),
            tags: vec![(3, vec![0]), (6, vec![9, 8])],
        }]);

        let parts = reconstruct_markup(flow, &skel, &frag).expect("结构成，该重建出两个文件");
        assert_eq!(
            parts,
            vec!["<p>A</p>\n<b>x</b>".to_string(), "<p>B</p>\n".to_string()]
        );
    }

    /// 片段数对不上（骨架说 1 个、片段表是空的）时返回 `None` —— 调用方退回整段。
    #[test]
    fn kf8_mismatched_fragment_counts_fall_back() {
        use super::reconstruct_markup;
        use crate::content::format::mobi::indx::{Index, IndexEntry};

        let flow = b"<p>A</p>\n";
        let skel = Index::from_entries(vec![IndexEntry {
            label: String::new(),
            tags: vec![(1, vec![1]), (6, vec![0, 8])],
        }]);
        let frag = Index::from_entries(vec![]);
        assert!(reconstruct_markup(flow, &skel, &frag).is_none());
    }
}
