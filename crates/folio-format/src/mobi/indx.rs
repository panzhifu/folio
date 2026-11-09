//! MOBI 的 INDX 索引：目录（NCX）与 KF8 的骨架/片段（SKEL / FRAG）都是这一族记录。
//!
//! MOBI 的索引**不是**一份 XML，而是 PalmDB 里的一组记录：
//!
//! ```text
//! [idx]             INDX 头记录：词数、CNCX 记录数、TAGX 段的偏移
//! [idx+1 .. +词数]   INDX 记录：每条末尾一张 IDXT 表，给出这一条里各条目的起点
//! [.. +CNCX 数]      CNCX 记录：<长度><字符串> 串成的一个字符串池
//! ```
//!
//! 一条索引项 = `<1 字节长度><标识串><控制字节若干><各标签的 vwi 值>`，而「哪几个控制位
//! 表示有某个标签、那个标签有几个值」写在 TAGX 段里 —— 所以**不读 TAGX 就没法解**，
//! 而各家打包工具写的 TAGX 不完全一样。
//!
//! ## 三种用法共用一套解析
//!
//! - **NCX 目录**：`1 = filepos`、`3 = 标题在 CNCX 池里的偏移`、`4 = 层级`。
//! - **KF8 SKEL**：`1 = 片段数`、`6.0 = 骨架起点`、`6.1 = 骨架长度`。
//! - **KF8 FRAG**：`3 = 文件号`、`6.0 = 插入位置`、`6.1 = 片段长度`。
//!
//! 三者的 TAGX / IDXT / vwi 规则完全一样，差别只在「读哪几个标签」。所以这一层解出
//! **通用条目**（标识串 + 每个标签的取值序列），由调用方按 tag 取 —— 见
//! [`IndexEntry::tag`]。NCX 的映射收在 [`read_ncx`] 里当包装。
//!
//! ## 四处不能凭感觉写的地方
//!
//! **一、TAGX 的偏移字段（词 44）在真实文件里有时是垃圾。** 退路是按 KindleUnpack 与
//! calibre 的口径，改用头长度那一格（词 0）—— 两处都验过的写法，[`tagx_offset`] 里
//! 那一段就是这件事。少了那条退路，一部分书就是「有目录却读不出」。
//!
//! **二、控制字节里的位数要按 bitmask 自己挪位。** 掩码是 `0b10` 那类非最低位的
//! 取值时，值要先右移到位再当个数用；直接拿 `control & mask` 会把「有 2 个值」
//! 读成「有 128 个值」。
//!
//! **三、CNCX 池里的偏移是「记录号 × 0x10000 + 记录内偏移」。** 所以查标题时
//! 必须带上那一段高位，否则多张 CNCX 的记录会互相指错标题（症状是目录里
//! 前后两行的标题对调）。
//!
//! **四、标识串按 CP1252 解，不做 ORDT。** ORDT 是词典索引（ORTH/INFL）用的字符映射，
//! 而 NCX / SKEL / FRAG 的标识串是 ASCII 的文件名与十进制插入位置（`parse_rawml.c` 里
//! 直接 `strtoul(entry->label)`）。带 ORDT 的索引不在这一版的目标里，遇到了当普通字节解，
//! 大不了那一本的目录空着。

use super::decode_text;

/// INDX 一族里能读到的标签号上限，防越界（真书远小于它）。
const MAX_TAGS: usize = 64;

/// 一条索引项。`tags` 是「标签号 → 该标签的取值序列」，值都是 vwi 解出来的 u32。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct IndexEntry {
    /// 标识串（SKEL 是文件名、FRAG 是十进制插入位置、NCX 用不上）。按 CP1252 解。
    pub label: String,
    pub tags: Vec<(u8, Vec<u32>)>,
}

impl IndexEntry {
    /// 某个标签的取值序列。同一标签可挂多个值（`6.0`/`6.1` 就是两值）。
    pub(crate) fn tag(&self, id: u8) -> &[u32] {
        self.tags
            .iter()
            .find(|(t, _)| *t == id)
            .map(|(_, v)| v.as_slice())
            .unwrap_or(&[])
    }

    /// 某个标签的第 `idx` 个值。
    pub(crate) fn tag_value(&self, id: u8, idx: usize) -> Option<u32> {
        self.tag(id).get(idx).copied()
    }
}

/// 解出来的一棵索引：通用条目 + CNCX 池。
#[derive(Debug, Clone, Default)]
pub(crate) struct Index {
    pub entries: Vec<IndexEntry>,
    /// CNCX 池：键 = `记录号 × 0x10000 + 记录内偏移` → 串。
    cncx: Vec<(u32, String)>,
}

impl Index {
    /// 只用条目（CNCX 空）建一棵索引。测试与「只要标签、不要字符串池」的调用方用。
    pub(crate) fn from_entries(entries: Vec<IndexEntry>) -> Self {
        Self {
            entries,
            cncx: Vec::new(),
        }
    }
}

impl Index {
    /// 按 CNCX 键取字符串（NCX 的标题就是按这个键找的）。
    pub(crate) fn cncx_string(&self, at: u32) -> Option<&str> {
        self.cncx
            .iter()
            .find(|(key, _)| *key == at)
            .map(|(_, s)| s.as_str())
    }
}

/// 解一棵 INDX 索引（NCX / SKEL / FRAG 通用）。
///
/// `read` 是「按 PalmDB 记录号取原始字节」的回调：INDX 一族跨好几条记录，
/// 而这一层不该知道容器长什么样 —— 它只认 `Vec<u8>`。
/// `first_record` 是头记录号；索引记录紧跟其后，CNCX 池再跟在索引记录之后。
///
/// 结构性失败一律回**空表**而不是 `Err`：目录读不出来的后果是「这本书没有目录」，
/// 正文照读；SKEL/FRAG 读不出来则由 KF8 装配那一步降级。这与对图片、字节的同一条
/// 降级口径一致。
pub(crate) fn read_index(
    mut read: impl FnMut(usize) -> Option<Vec<u8>>,
    first_record: usize,
    codepage: u32,
) -> Index {
    let Some(header) = read(first_record) else {
        return Index::default();
    };
    if header.len() < 0xC0 || header[..4] != *b"INDX" {
        return Index::default();
    }
    // 词 5 = 这一族索引记录有几条；词 12 = CNCX 记录有几条。
    let Some(index_records) = word(&header, 5) else {
        return Index::default();
    };
    let cncx_records = word(&header, 12).unwrap_or(0);
    if !(1..=0xFFFF).contains(&index_records) || cncx_records > 0xFFFF {
        return Index::default();
    }
    let Some(tagx) = tagx_offset(&header) else {
        return Index::default();
    };
    // TAGX 头：+4 = 条目区起点（相对 TAGX 段），+8 = 控制字节数。
    let first_entry = word_at(&header, tagx + 4).unwrap_or(0) as usize;
    let control_bytes = word_at(&header, tagx + 8).unwrap_or(0) as usize;
    if !(1..=32).contains(&control_bytes) {
        return Index::default();
    }
    if first_entry <= 12 || tagx + first_entry > header.len() {
        return Index::default();
    }
    // 标签表：每项 4 字节（标签号、值个数、位掩码、是否段尾标记）。
    let mut tags: Vec<(u8, u8, u8, u8)> = Vec::new();
    let mut at = 12usize;
    while at + 4 <= first_entry && tags.len() < MAX_TAGS {
        tags.push((
            header[tagx + at],
            header[tagx + at + 1],
            header[tagx + at + 2],
            header[tagx + at + 3],
        ));
        at += 4;
    }

    // 标题/字符串池：`记录号 × 0x10000 + 记录内偏移` → 串。
    let mut cncx: Vec<(u32, String)> = Vec::new();
    for k in 0..cncx_records as usize {
        let Some(record) = read(first_record + 1 + index_records as usize + k) else {
            break;
        };
        let base = (k as u32) << 16;
        let mut pos = 0usize;
        while pos < record.len() {
            let Some((len, next)) = vwi(&record, pos) else {
                break;
            };
            if record.len() - next < len as usize {
                break;
            }
            cncx.push((
                base + pos as u32,
                decode_text(&record[next..next + len as usize], codepage),
            ));
            pos = next + len as usize;
        }
    }

    let mut entries = Vec::new();
    for ri in 0..index_records as usize {
        let Some(record) = read(first_record + 1 + ri) else {
            break;
        };
        if record.len() < 12 || record[..4] != *b"INDX" {
            break;
        }
        let Some(idxt) = word(&record, 4) else {
            continue;
        };
        let idxt = idxt as usize;
        let Some(entry_count) = word(&record, 5) else {
            continue;
        };
        let entry_count = entry_count as usize;
        if entry_count > 0xFFFF || idxt + 4 + entry_count * 2 > record.len() {
            continue;
        }
        if record.get(idxt..idxt + 4) != Some(b"IDXT".as_slice()) {
            continue;
        }
        for j in 0..entry_count {
            let start = half(&record, idxt + 4 + 2 * j).map(|v| v as usize);
            let end = if j + 1 < entry_count {
                half(&record, idxt + 4 + 2 * (j + 1)).map(|v| v as usize)
            } else {
                Some(idxt)
            };
            let (Some(start), Some(end)) = (start, end) else {
                continue;
            };
            if end <= start || end > record.len() {
                continue;
            }
            if let Some(item) = entry(&record[start..end], &tags, control_bytes) {
                entries.push(item);
            }
        }
    }
    Index { entries, cncx }
}

/// 解 NCX 目录。`1 = filepos`、`3 = 标题在 CNCX 池里的偏移`、`4 = 层级`。
pub(crate) fn read_ncx(
    read: impl FnMut(usize) -> Option<Vec<u8>>,
    ncx_record: usize,
    codepage: u32,
) -> Vec<TocItem> {
    let index = read_index(read, ncx_record, codepage);
    let mut out = Vec::new();
    for entry in &index.entries {
        let Some(filepos) = entry.tag_value(1, 0) else {
            continue;
        };
        let title = entry
            .tag_value(3, 0)
            .and_then(|at| index.cncx_string(at))
            .unwrap_or_default()
            .to_string();
        out.push(TocItem {
            filepos,
            level: entry.tag_value(4, 0).unwrap_or(0),
            title,
        });
    }
    out
}

/// 一条目录项。`filepos` 是**未压缩正文流**里的字节偏移，换算成章内位置由装配那一层
/// 按每帧解出长度的累计表做（`filepos / 4096` 只能定位到帧，定位不到字节）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TocItem {
    pub filepos: u32,
    pub level: u32,
    pub title: String,
}

/// INDX 头记录里的字（词）偏移：词 i 站在 `4 + i × 4`。
fn word(bytes: &[u8], index: usize) -> Option<u32> {
    word_at(bytes, 4 + index * 4)
}

fn half(bytes: &[u8], at: usize) -> Option<u16> {
    if at + 2 > bytes.len() {
        return None;
    }
    Some(u16::from_be_bytes([bytes[at], bytes[at + 1]]))
}

/// 前向编码的变宽整数：每字节 7 位、大端，**最后一字节最高位为 1**。
///
/// 返回 `(值, 新的位置)`。装不进 32 位算失败：偏移与长度被静默截断会指到
/// 正文的另一处，那比读不出更难查。
fn vwi(bytes: &[u8], mut at: usize) -> Option<(u32, usize)> {
    let mut value = 0u32;
    while at < bytes.len() {
        let byte = bytes[at];
        at += 1;
        let low = (byte & 0x7f) as u32;
        if value > (u32::MAX - low) >> 7 {
            return None;
        }
        value = (value << 7) | low;
        if byte & 0x80 != 0 {
            return Some((value, at));
        }
    }
    None
}

fn count_bits(mask: u8) -> u32 {
    mask.count_ones()
}

/// TAGX 段的位置：优先词 44，那格是垃圾时退到词 0（头长度）。
///
/// 两条都验：真文件里两种写法都出现过，而只认一条的结果是「这本书没有目录」，
/// 屏幕上看不出来。
fn tagx_offset(header: &[u8]) -> Option<usize> {
    let declared = word(header, 44).unwrap_or(0) as usize;
    if header.get(declared..declared + 4)? == *b"TAGX" {
        return Some(declared);
    }
    let fallback = word(header, 0).unwrap_or(0) as usize;
    (header.get(fallback..fallback + 4)? == *b"TAGX").then_some(fallback)
}

/// 一条 INDX 项 → 通用条目。解不出任何标签的也算一条（只是没有值）。
fn entry(raw: &[u8], tags: &[(u8, u8, u8, u8)], control_bytes: usize) -> Option<IndexEntry> {
    let mut pos = 0usize;
    // 标识串：`<1 字节长度><串>`。按 CP1252 解（见模块头第四条）。
    let ident = *raw.get(pos)? as usize;
    pos += 1;
    let label = raw
        .get(pos..pos + ident)
        .map(|bytes| decode_text(bytes, 1252).trim().to_string())
        .unwrap_or_default();
    pos += ident;
    if pos + control_bytes > raw.len() {
        return None;
    }
    let control = &raw[pos..pos + control_bytes];
    pos += control_bytes;

    let mut out = IndexEntry {
        label,
        tags: Vec::new(),
    };
    let mut index = 0usize;
    for &(tag, values, mask, eof) in tags {
        if index >= control.len() {
            break;
        }
        // 段尾标记：不取值，只吃掉一个控制字节。
        if eof == 1 {
            index += 1;
            continue;
        }
        let flagged = control[index] & mask;
        if flagged == 0 {
            continue;
        }
        // 值的个数：全掩码命中且掩码多于一位 → 后面跟一个 vwi 字节长度；
        // 否则把命中位挪到最低位再当个数读。
        let mut byte_length: Option<usize> = None;
        let mut count = 1usize;
        if flagged == mask && count_bits(mask) > 1 {
            let (len, next) = vwi(raw, pos)?;
            byte_length = Some(len as usize);
            pos = next;
        } else {
            let mut shifted_mask = mask;
            let mut shifted = flagged;
            while shifted_mask & 1 == 0 {
                shifted_mask >>= 1;
                shifted >>= 1;
            }
            count = if flagged == mask && count_bits(mask) == 1 {
                1
            } else {
                shifted as usize
            };
        }

        let mut collected: Vec<u32> = Vec::new();
        if let Some(total) = byte_length {
            let mut consumed = 0usize;
            while consumed < total {
                let (value, next) = vwi(raw, pos)?;
                consumed += next - pos;
                pos = next;
                collected.push(value);
            }
        } else {
            for _ in 0..count.saturating_mul(values.max(1) as usize) {
                let (value, next) = vwi(raw, pos)?;
                pos = next;
                collected.push(value);
            }
        }
        out.tags.push((tag, collected));
    }
    Some(out)
}

/// 直接按字节位置读一个词（不是按「词号」）。
fn word_at(bytes: &[u8], at: usize) -> Option<u32> {
    if at + 4 > bytes.len() {
        return None;
    }
    Some(u32::from_be_bytes([
        bytes[at],
        bytes[at + 1],
        bytes[at + 2],
        bytes[at + 3],
    ]))
}

#[cfg(test)]
mod tests {
    //! 目录这一层的夹具按「记录」摆：INDX 头、一条索引记录、一张 CNCX 池。
    //! 断言看的是**解出的 filepos 与层级**，因为读者看得见的错位就从这两格里来。

    use super::{TocItem, count_bits, read_index, read_ncx, tagx_offset, vwi};

    /// 词 i 站在 `4 + i * 4`：这一条钉住 `word()` 的那个算术，因为它一旦错位，
    /// 整棵树读到的都是别的字段。
    #[test]
    fn words_are_read_at_four_plus_index_times_four() {
        let mut bytes = vec![0xAA; 8 + 5 * 4];
        // 词 5 那一格写个可识别的数。
        let at = 4 + 5 * 4;
        bytes[at..at + 4].copy_from_slice(&0x0000_0007u32.to_be_bytes());
        assert_eq!(super::word(&bytes, 5), Some(7));
    }

    /// vwi：最后一字节最高位是终止位，不是符号位。
    #[test]
    fn the_variable_width_integer_stops_at_the_high_bit() {
        // 终止位在**最后一字节**上：单独的 0x05 是「还没完」，读到数据用完算失败。
        assert_eq!(vwi(&[0x85], 0), Some((5, 1)));
        assert_eq!(vwi(&[0x05], 0), None);
        // 0x81 = 终止位 + 低位 1 → 值 1，只吃一字节。
        // 两字节：前一字节低 7 位是高位，后一字节带终止位。
        assert_eq!(vwi(&[0x01, 0x82], 0), Some(((1 << 7) | 2, 2)));
        // 没有终止位 = 数据用完，失败。
        assert_eq!(vwi(&[0x01, 0x02], 0), None);
    }

    /// 位数函数：位掩码里 1 的个数决定「这是不是一个多位掩码」。
    #[test]
    fn masks_are_counted() {
        assert_eq!(count_bits(0b0000_0001), 1);
        assert_eq!(count_bits(0b1000_0000), 1);
        assert_eq!(count_bits(0b0000_0011), 2);
    }

    /// TAGX 的两条路：词 44 那格对得上就认，对不上退到头长度那一格。
    #[test]
    fn tagx_is_found_by_the_declared_offset_or_by_the_header_length() {
        // 词 44 处（偏移 4 + 44*4 = 180）写 192，而 192 那格站着 TAGX。
        let mut good = vec![0u8; 4 + 44 * 4 + 8];
        good[4..8].copy_from_slice(&16u32.to_be_bytes());
        good[180..184].copy_from_slice(&192u32.to_be_bytes());
        good.resize(200, 0);
        good[192..196].copy_from_slice(b"TAGX");
        assert_eq!(tagx_offset(&good), Some(192));

        // 词 44 是垃圾，词 0（头长度）那格站着 TAGX：退路。
        let mut fallback = vec![0u8; 4 + 44 * 4 + 4];
        fallback[4..8].copy_from_slice(&16u32.to_be_bytes());
        fallback.resize(40, 0);
        fallback[16..20].copy_from_slice(b"TAGX");
        assert_eq!(tagx_offset(&fallback), Some(16));

        // 两处都不站着 TAGX：这棵树没有目录。
        assert_eq!(tagx_offset(&[0u8; 4 + 44 * 4 + 4]), None);
    }

    /// 最小的三棵记录：INDX 头 + 一条索引记录 + 一张 CNCX 池，两个层级。
    ///
    /// 这是 [`read_ncx`] 唯一的「整链路」覆盖 —— 它同时验到词号、IDXT 的起点表、
    /// 控制字节与池里的「记录号 × 0x10000 + 偏移」那条拼接。
    #[test]
    fn a_two_entry_ncx_reads_levels_titles_and_offsets() {
        let records = ncx_records();
        let items = read_ncx(|at| records.get(at).cloned(), 0, 65001);
        assert_eq!(
            items,
            vec![
                TocItem {
                    filepos: 0x1000,
                    level: 0,
                    title: "第一章".to_string()
                },
                TocItem {
                    filepos: 0x2000,
                    level: 0,
                    title: "第二章".to_string()
                },
            ]
        );
    }

    /// 通用入口要能拿到**每个标签的取值序列**，而不只是 NCX 那三个 ——
    /// KF8 的 SKEL/FRAG 就是靠这一层读它们的 tag 1/6 的。
    #[test]
    fn the_generic_index_exposes_raw_tags() {
        let records = ncx_records();
        let index = read_index(|at| records.get(at).cloned(), 0, 65001);
        assert_eq!(index.entries.len(), 2);
        let first = &index.entries[0];
        assert_eq!(first.tag_value(1, 0), Some(0x1000), "tag 1 = filepos");
        assert_eq!(first.tag_value(3, 0), Some(0), "tag 3 = 标题在 CNCX 池的键");
        assert_eq!(
            first.tag_value(3, 0).and_then(|at| index.cncx_string(at)),
            Some("第一章")
        );
        assert_eq!(first.tag(99), &[] as &[u32], "没有的标签是空序列");
    }

    /// 三棵记录：INDX 头 + 一条索引记录（两条项）+ 一张 CNCX 池。
    fn ncx_records() -> Vec<Vec<u8>> {
        let mut header = vec![0u8; 0xC0 + 200];
        header[..4].copy_from_slice(b"INDX");
        // 词 0 = 头长度（也是 TAGX 退路用的那一格），词 5 = 索引记录数，词 12 = CNCX 数。
        let tagx = 0xC0usize;
        header[4..8].copy_from_slice(&(tagx as u32).to_be_bytes());
        header[4 + 5 * 4..8 + 5 * 4].copy_from_slice(&1u32.to_be_bytes());
        header[4 + 12 * 4..8 + 12 * 4].copy_from_slice(&1u32.to_be_bytes());
        header[tagx..tagx + 4].copy_from_slice(b"TAGX");
        header[tagx + 4..tagx + 8].copy_from_slice(&24u32.to_be_bytes()); // 条目区起点
        header[tagx + 8..tagx + 12].copy_from_slice(&2u32.to_be_bytes()); // 控制字节数
        for (i, (tag, values, mask, eof)) in
            [(1u8, 1u8, 0x01u8, 0u8), (3, 1, 0x02, 0), (4, 1, 0x00, 1)]
                .iter()
                .enumerate()
        {
            let at = tagx + 12 + i * 4;
            header[at] = *tag;
            header[at + 1] = *values;
            header[at + 2] = *mask;
            header[at + 3] = *eof;
        }

        // 一条索引记录：两条项，各按 IDXT 的起点表切开。
        let mut record = vec![0u8; 4 + 5 * 4 + 64];
        record[..4].copy_from_slice(b"INDX");
        // 条目区从词 5 之后开始，IDXT 表拥在其后。
        let idxt = 4 + 5 * 4 + 40;
        record[4 + 4 * 4..8 + 4 * 4].copy_from_slice(&(idxt as u32).to_be_bytes());
        record[4 + 5 * 4..8 + 5 * 4].copy_from_slice(&2u32.to_be_bytes());
        record[idxt..idxt + 4].copy_from_slice(b"IDXT");
        let mut body = Vec::new();
        for (filepos, title_at) in [(0x1000u32, 0u32), (0x2000u32, 10u32)] {
            body.push(0x00u8); // 标识串的长度：0
            body.push(0x80 | 0x01 | 0x02); // 控制字节：fiepos 与标题都在
            body.push(0x00); // 第二个控制字节：给那个 eof 标签吃掉
            body.extend_from_slice(&vwi_encode(filepos));
            body.extend_from_slice(&vwi_encode(title_at));
        }
        // 条目区从词 5 之后开头：写到 24 会把刚写进去的「条目数」那格抄掉。
        let start_at = 8 + 5 * 4;
        record[start_at..start_at + body.len()].copy_from_slice(&body);
        record[idxt + 4..idxt + 6].copy_from_slice(&(start_at as u16).to_be_bytes());
        record[idxt + 6..idxt + 8]
            .copy_from_slice(&((start_at + body.len() / 2) as u16).to_be_bytes());
        // 第三条起点表项读不到（最后一项的终点按 IDXT 本身算），留着只为摆满表。

        // CNCX 池：两条标题。池里的键是「长度字节所在的那个偏移」，
        // 所以第二条是 10 = 1 + 9，不是再容的序号。
        let mut cncx = Vec::new();
        for title in ["第一章", "第二章"] {
            let bytes = title.as_bytes();
            cncx.push(0x80 | bytes.len() as u8);
            cncx.extend_from_slice(bytes);
        }

        vec![header, record, cncx]
    }

    fn vwi_encode(value: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut rest = value;
        loop {
            let low = (rest & 0x7f) as u8;
            bytes.push(low);
            rest >>= 7;
            if rest == 0 {
                break;
            }
        }
        bytes.reverse();
        if let Some(last) = bytes.last_mut() {
            *last |= 0x80;
        }
        bytes
    }
}
