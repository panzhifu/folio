//! MOBI 的录 0（record 0）：PalmDOC 头 + MOBI 头 + EXTH。
//!
//! 这是整族的**入口结构**：压缩方式、正文记录数、码页、图片记录起点、HUFF/CDIC 表
//! 位置、KF8 的 SKEL/FRAG/FDST/NCX 索引号、以及书目元信息（EXTH）全在这里。
//! 字段偏移来自 `reference/libmobi/src/mobi.h`（见那里的 `MOBIRecord0Header` /
//! `MOBIMobiHeader` / `MOBIExthTag` 三段注释），大端读。
//!
//! ## 三处不凭感觉写
//!
//! **一、字段偏移是「相对录 0 开头」的绝对值，不是相对 MOBI 头。** 头从偏移 16 起，
//! 但 `image_index` 就在 108、`extra_flags` 在 242 —— 拿「MOBI 头内偏移」去读会整体
//! 错 16 字节，症状是「图片起点指到别处」这种最难查的错位。
//!
//! **二、短头老书有的字段根本不存在。** 老文件的 MOBI 头声明得很短（甚至写 0），
//! 越界那几格要当「没有」（`MOBI_NOTSET`），而不是报错整本读不动 —— 只有真正要用的
//! 字段（压缩、正文记录数）缺了才算坏文件。libmobi 也是这个口径
//! （`mobi_parse_mobiheader` 里按 `header_length` 收窄可读范围）。
//!
//! **三、EXTH 紧跟在 MOBI 头之后**，起点是 `16 + header_length`。它的 magic 是 `EXTH`，
//! 不是靠 `exth_flags` 的位 6 判 —— 那位是「有没有 EXTH」，而真正的判据是那四个字节
//! （libmobi 同样以 magic 为准）。

use crate::Result;

use super::encoding;

/// 未设置的 32 位字段（MOBI 自己用 `0xFFFFFFFF` 表示「没有」）。
pub(crate) const NOTSET: u32 = u32::MAX;

/// 录 0 里 PalmDOC 头的固定长度。
const PALMDOC_HEADER: usize = 16;
/// MOBI 头的 magic（`MOBI`）在这四位。
const MOBI_MAGIC_AT: usize = 16;

/// 压缩方式（PalmDOC 头偏移 0）。
pub(crate) const COMPRESSION_NONE: u16 = 1;
pub(crate) const COMPRESSION_PALMDOC: u16 = 2;
pub(crate) const COMPRESSION_HUFFCDIC: u16 = 17480;

/// 认得的 EXTH 标签。
///
/// 名字后面的注释是**每条标签对应哪一条 Dublin Core**，抄的是 libmobi 的
/// `src/mobi.h`（`/home/noke/tmp/libmobi` 那份 `public` 分支）—— MOBI 的书目区就是
/// OPF 的扁平版，逐条对得上才敢直接填进 [`crate::content::format::DocMeta`]。
pub(crate) const EXTH_TITLE: u32 = 99;
pub(crate) const EXTH_AUTHOR: u32 = 100;
/// `dc:publisher`。
pub(crate) const EXTH_PUBLISHER: u32 = 101;
/// `dc:description`（内容简介）。
pub(crate) const EXTH_DESCRIPTION: u32 = 103;
/// `dc:identifier opf:scheme="ISBN"`。
pub(crate) const EXTH_ISBN: u32 = 104;
/// `dc:subject`，一条标签可以重复出现多次。
pub(crate) const EXTH_SUBJECT: u32 = 105;
/// `dc:date`（KindleGen 写的是出版日期）。
pub(crate) const EXTH_PUBLISHINGDATE: u32 = 106;
/// `dc:contributor`。
pub(crate) const EXTH_CONTRIBUTOR: u32 = 108;
/// `dc:rights`。
pub(crate) const EXTH_RIGHTS: u32 = 109;
/// `dc:type`。
pub(crate) const EXTH_TYPE: u32 = 111;
/// `dc:language`（`en` / `zh-CN` 这一类字符串）。
pub(crate) const EXTH_LANGUAGE: u32 = 524;
pub(crate) const EXTH_KF8BOUNDARY: u32 = 121;
pub(crate) const EXTH_RESCOFFSET: u32 = 131;
pub(crate) const EXTH_COVEROFFSET: u32 = 201;
pub(crate) const EXTH_THUMBOFFSET: u32 = 202;
pub(crate) const EXTH_UPDATEDTITLE: u32 = 503;

/// EXTH 记录的条数上限。真书几十条，给足余量；越界的表先挡掉，别让下游按一个
/// 荒唐的条数去分配。
const MAX_EXTH: u32 = 4096;

/// 录 0 解出来的头。
#[derive(Debug, Clone)]
pub(crate) struct Header {
    pub compression: u16,
    /// 全书正文解压后的字节数（PalmDOC 头偏移 4）。
    pub text_length: u32,
    /// 正文记录的**条数**（偏移 8）。
    pub text_record_count: u16,
    /// 每条正文记录的上限（偏移 10，正常是 4096）。
    pub text_record_size: u16,
    /// 加密方式（偏移 12）：0 = 无。非 0 由 provider 报 `Encrypted`。
    pub encryption_type: u16,
    /// 码页（MOBI 头偏移 28）。
    pub encoding: u32,
    /// MOBI 版本（偏移 36）：≥ 8 是 KF8。
    pub version: u32,
    /// EXTH 标志（偏移 128）。位 6（0x40）表示有 EXTH。
    pub exth_flags: u32,
    /// 尾部 extra data 标志（偏移 242，见 `codec::extras_len`）。
    pub extra_flags: u16,
    /// 首个图片记录号（偏移 108）。`NOTSET` = 没有。
    pub image_index: u32,
    /// HUFF/CDIC 的 HUFF 记录起点与条数（偏移 112 / 116）。
    pub huff_rec_index: u32,
    pub huff_rec_count: u32,
    /// NCX 目录的 INDX 记录号（偏移 244）。
    pub ncx_index: u32,
    /// KF8 的 FRAG / SKEL / FDST 索引号（偏移 248 / 252 / 192）。
    pub fragment_index: u32,
    pub skeleton_index: u32,
    pub fdst_index: u32,
    /// KF8 的 FDST 段数（偏移 196）。
    pub fdst_section_count: u32,
    /// KF8 的 guide 索引号（偏移 260）。
    pub guide_index: u32,
    /// 书名兜底（MOBI 头偏移 84 / 88 指的录 0 内字节）。空 = 没有。
    pub full_name: String,
    /// EXTH 原始记录（tag → 数据字节），按文件顺序。
    exth: Vec<(u32, Vec<u8>)>,
}

impl Header {
    /// 从录 0 的原始字节解出头。
    ///
    /// 只在**结构真的不成**时返回 `Err`：录 0 太短、压缩方式不认得、MOBI magic 缺失。
    /// 老书缺几个可选字段（图片索引、KF8 索引）不算错，那几格落成 `NOTSET`。
    pub(crate) fn parse(rec0: &[u8]) -> Result<Self> {
        if rec0.len() < PALMDOC_HEADER {
            return Err(crate::Error::UnsupportedFormat(
                "MOBI 录 0 太短（连 PalmDOC 头都不全）".to_string(),
            ));
        }
        let compression = u16_at(rec0, 0).unwrap_or(0);
        if !matches!(
            compression,
            COMPRESSION_NONE | COMPRESSION_PALMDOC | COMPRESSION_HUFFCDIC
        ) {
            return Err(crate::Error::UnsupportedFormat(format!(
                "MOBI 的压缩方式是 {compression}（不认得）"
            )));
        }
        if rec0.get(MOBI_MAGIC_AT..MOBI_MAGIC_AT + 4) != Some(b"MOBI") {
            return Err(crate::Error::UnsupportedFormat(
                "MOBI 头缺失（录 0 偏移 16 不是 MOBI）".to_string(),
            ));
        }
        let header_length = u32_at(rec0, 20).unwrap_or(0) as usize;

        let encoding = u32_at(rec0, 28).unwrap_or(0);
        let version = u32_at(rec0, 36).unwrap_or(0);
        let full_name_offset = u32_at(rec0, 84).unwrap_or(NOTSET);
        let full_name_length = u32_at(rec0, 88).unwrap_or(NOTSET);

        let full_name = read_full_name(rec0, full_name_offset, full_name_length, encoding);

        // EXTH 紧跟 MOBI 头之后。头长度写 0 的老书按最小头（24）算，与 libmobi 同口径。
        let exth_at = MOBI_MAGIC_AT + header_length.max(24);
        let exth = parse_exth(rec0, exth_at);

        Ok(Self {
            compression,
            text_length: u32_at(rec0, 4).unwrap_or(0),
            text_record_count: u16_at(rec0, 8).unwrap_or(0),
            text_record_size: u16_at(rec0, 10).unwrap_or(4096),
            encryption_type: u16_at(rec0, 12).unwrap_or(0),
            encoding,
            version,
            exth_flags: u32_at(rec0, 128).unwrap_or(0),
            extra_flags: u16_at(rec0, 242).unwrap_or(0),
            image_index: u32_at(rec0, 108).unwrap_or(NOTSET),
            huff_rec_index: u32_at(rec0, 112).unwrap_or(NOTSET),
            huff_rec_count: u32_at(rec0, 116).unwrap_or(NOTSET),
            ncx_index: u32_at(rec0, 244).unwrap_or(NOTSET),
            fragment_index: u32_at(rec0, 248).unwrap_or(NOTSET),
            skeleton_index: u32_at(rec0, 252).unwrap_or(NOTSET),
            fdst_index: u32_at(rec0, 192).unwrap_or(NOTSET),
            fdst_section_count: u32_at(rec0, 196).unwrap_or(0),
            guide_index: u32_at(rec0, 260).unwrap_or(NOTSET),
            full_name,
            exth,
        })
    }

    /// 版本 ≥ 8 = KF8（AZW3）。
    pub(crate) fn is_kf8(&self) -> bool {
        self.version >= 8
    }

    /// 有 EXTH 段（那位被置起来，或真的解到过记录）。
    pub(crate) fn has_exth(&self) -> bool {
        self.exth_flags & 0x40 != 0 || !self.exth.is_empty()
    }

    /// EXTH 里某个标签的原始字节。
    pub(crate) fn exth_bytes(&self, tag: u32) -> Option<&[u8]> {
        self.exth
            .iter()
            .find(|(t, _)| *t == tag)
            .map(|(_, data)| data.as_slice())
    }

    /// 某个标签的所有值（作者、标题都可能多条）。
    pub(crate) fn exth_all_bytes(&self, tag: u32) -> Vec<&[u8]> {
        self.exth
            .iter()
            .filter(|(t, _)| *t == tag)
            .map(|(_, data)| data.as_slice())
            .collect()
    }

    /// 数字标签（大端整数，取前 4 字节；不足按有多少算多少）。
    pub(crate) fn exth_uint(&self, tag: u32) -> Option<u32> {
        let data = self.exth_bytes(tag)?;
        (data.len() >= 4).then(|| u32::from_be_bytes([data[0], data[1], data[2], data[3]]))
    }

    /// 字符串标签，按录 0 声明的码页解。
    pub(crate) fn exth_string(&self, tag: u32) -> Option<String> {
        let data = self.exth_bytes(tag)?;
        let text = encoding::decode_text(data, self.encoding);
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_string())
    }

    /// 某个字符串标签的**全部**值（作者多条的常见）。
    pub(crate) fn exth_strings(&self, tag: u32) -> Vec<String> {
        self.exth_all_bytes(tag)
            .into_iter()
            .map(|data| {
                encoding::decode_text(data, self.encoding)
                    .trim()
                    .to_string()
            })
            .filter(|s| !s.is_empty())
            .collect()
    }
}

/// 录 0 内 `full_name_offset/length` 指的那段名字（CP1252/UTF-8 都按声明的码页解）。
fn read_full_name(rec0: &[u8], offset: u32, length: u32, encoding: u32) -> String {
    if offset == NOTSET || length == NOTSET || length == 0 {
        return String::new();
    }
    let start = offset as usize;
    let end = start.saturating_add(length as usize);
    let Some(bytes) = rec0.get(start..end) else {
        return String::new();
    };
    encoding::decode_text(bytes, encoding).trim().to_string()
}

/// 解 EXTH：`EXTH` magic + 长度 + 条数，接着每条 `tag(4) size(4) data(size-8)`。
///
/// 结构不成（magic 不对、条数荒唐、越界）时返回**空表**而不是 `Err`：EXTH 只提供
/// 书名作者，缺了它正文照读（书名回落到 PalmDB 头那 32 字节的名字）。这与「图片、
/// 目录读不出不整本失败」是同一条降级口径。
fn parse_exth(rec0: &[u8], at: usize) -> Vec<(u32, Vec<u8>)> {
    let Some(magic) = rec0.get(at..at + 4) else {
        return Vec::new();
    };
    if magic != b"EXTH" {
        return Vec::new();
    }
    let (Some(_length), Some(count)) = (u32_at(rec0, at + 4), u32_at(rec0, at + 8)) else {
        return Vec::new();
    };
    if count > MAX_EXTH {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(count as usize);
    let mut cursor = at + 12;
    for _ in 0..count {
        let (Some(tag), Some(size)) = (u32_at(rec0, cursor), u32_at(rec0, cursor + 4)) else {
            break;
        };
        if size < 8 {
            break;
        }
        let data_start = cursor + 8;
        let data_end = data_start + (size as usize - 8);
        let Some(data) = rec0.get(data_start..data_end) else {
            break;
        };
        out.push((tag, data.to_vec()));
        cursor = data_end;
    }
    out
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    bytes
        .get(at..at + 2)
        .map(|s| u16::from_be_bytes([s[0], s[1]]))
}

/// PalmDB 一族全线大端。
fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

#[cfg(test)]
mod tests {
    //! 夹具是手摆字节摆出来的：这一层唯一的契约就是「按偏移读出那几个数」，
    //! 所以断言看的也是那几个数，不是任何一本书。

    use super::{
        COMPRESSION_PALMDOC, EXTH_AUTHOR, EXTH_COVEROFFSET, EXTH_THUMBOFFSET, EXTH_TITLE, Header,
        NOTSET,
    };

    /// 摆一份录 0：PalmDOC 头 + MOBI 头（写到 extra_flags/ncx 那几格够用）+ EXTH。
    fn record0() -> Vec<u8> {
        let mut rec = vec![0u8; 0x160];
        // PalmDOC 头。
        rec[0..2].copy_from_slice(&COMPRESSION_PALMDOC.to_be_bytes());
        rec[4..8].copy_from_slice(&1234u32.to_be_bytes()); // text_length
        rec[8..10].copy_from_slice(&7u16.to_be_bytes()); // text_record_count
        rec[10..12].copy_from_slice(&4096u16.to_be_bytes()); // text_record_size
        rec[12..14].copy_from_slice(&0u16.to_be_bytes()); // encryption_type
        // MOBI 头。
        rec[16..20].copy_from_slice(b"MOBI");
        rec[20..24].copy_from_slice(&232u32.to_be_bytes()); // header_length
        rec[28..32].copy_from_slice(&65001u32.to_be_bytes()); // encoding
        rec[36..40].copy_from_slice(&6u32.to_be_bytes()); // version
        rec[84..88].copy_from_slice(&0x80u32.to_be_bytes()); // full_name_offset
        rec[88..92].copy_from_slice(&12u32.to_be_bytes()); // full_name_length（"书名甲乙" 12 字节）
        rec[108..112].copy_from_slice(&9u32.to_be_bytes()); // image_index
        rec[112..116].copy_from_slice(&NOTSET.to_be_bytes()); // huff_rec_index
        rec[128..132].copy_from_slice(&0x40u32.to_be_bytes()); // exth_flags
        rec[242..244].copy_from_slice(&3u16.to_be_bytes()); // extra_flags
        rec[244..248].copy_from_slice(&20u32.to_be_bytes()); // ncx_index
        // full name。
        rec[0x80..0x8c].copy_from_slice("书名甲乙".as_bytes());

        // EXTH：紧跟 MOBI 头（16 + 232 = 248）。表长度与条数要写对。
        let exth_at = 16 + 232;
        let mut exth = Vec::new();
        for (tag, value) in [
            (EXTH_TITLE, "真正的书名".as_bytes().to_vec()),
            (EXTH_AUTHOR, "作者甲".as_bytes().to_vec()),
            (EXTH_COVEROFFSET, 5u32.to_be_bytes().to_vec()),
        ] {
            exth.extend_from_slice(&tag.to_be_bytes());
            exth.extend_from_slice(&((value.len() + 8) as u32).to_be_bytes());
            exth.extend_from_slice(&value);
        }
        let exth_len = 12 + exth.len();
        rec[exth_at..exth_at + 4].copy_from_slice(b"EXTH");
        rec[exth_at + 4..exth_at + 8].copy_from_slice(&(exth_len as u32).to_be_bytes());
        rec[exth_at + 8..exth_at + 12].copy_from_slice(&3u32.to_be_bytes());
        rec[exth_at + 12..exth_at + 12 + exth.len()].copy_from_slice(&exth);
        rec
    }

    #[test]
    fn the_offsets_line_up() {
        let header = Header::parse(&record0()).unwrap();
        assert_eq!(header.compression, COMPRESSION_PALMDOC);
        assert_eq!(header.text_length, 1234);
        assert_eq!(header.text_record_count, 7);
        assert_eq!(header.text_record_size, 4096);
        assert_eq!(header.encryption_type, 0);
        assert_eq!(header.encoding, 65001);
        assert_eq!(header.version, 6);
        assert!(!header.is_kf8());
        assert_eq!(header.extra_flags, 3);
        assert_eq!(header.image_index, 9);
        assert_eq!(header.ncx_index, 20);
        assert_eq!(header.huff_rec_index, NOTSET);
        assert_eq!(header.full_name, "书名甲乙");
    }

    #[test]
    fn exth_values_come_out_in_order_and_decoded() {
        let header = Header::parse(&record0()).unwrap();
        assert!(header.has_exth());
        assert_eq!(
            header.exth_string(EXTH_TITLE).as_deref(),
            Some("真正的书名")
        );
        assert_eq!(header.exth_strings(EXTH_AUTHOR), vec!["作者甲".to_string()]);
        assert_eq!(header.exth_uint(EXTH_COVEROFFSET), Some(5));
        // 没有的标签安静地回「没有」。
        assert_eq!(header.exth_uint(EXTH_THUMBOFFSET), None);
    }

    /// 版本 ≥ 8 就是 KF8，`is_kf8` 是整条 KF7/KF8 分岔的总闸。
    #[test]
    fn version_eight_is_kf8() {
        let mut rec = record0();
        rec[36..40].copy_from_slice(&8u32.to_be_bytes());
        assert!(Header::parse(&rec).unwrap().is_kf8());
    }

    /// 坏结构要在 `parse` 就定性，别交给下游去猜。
    #[test]
    fn broken_records_are_rejected_but_missing_optional_fields_are_not() {
        // 连 PalmDOC 头都不全。
        assert!(Header::parse(&[0u8; 8]).is_err());
        // 压缩方式不认得。
        let mut bad = record0();
        bad[0..2].copy_from_slice(&99u16.to_be_bytes());
        assert!(Header::parse(&bad).is_err());
        // MOBI magic 缺失。
        let mut bad = record0();
        bad[16..20].copy_from_slice(b"XXXX");
        assert!(Header::parse(&bad).is_err());

        // 可选字段缺失（老书短头 + 无 EXTH）：不算错，落 NOTSET / 空。
        let mut short = vec![0u8; 32];
        short[0..2].copy_from_slice(&1u16.to_be_bytes()); // 不压
        short[8..10].copy_from_slice(&3u16.to_be_bytes());
        short[16..20].copy_from_slice(b"MOBI");
        short[20..24].copy_from_slice(&24u32.to_be_bytes());
        let header = Header::parse(&short).unwrap();
        assert_eq!(header.image_index, NOTSET, "越界的字段当没有");
        assert_eq!(header.ncx_index, NOTSET);
        assert!(header.full_name.is_empty());
        assert!(!header.has_exth());
    }
}
