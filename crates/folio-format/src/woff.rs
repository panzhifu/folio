//! WOFF1 → sfnt：把逐表 zlib 压缩的字体解回 `ttf-parser` 能直接吃的字节。
//!
//! 存在的理由是**注册那一步不认 WOFF**：gpui 的 `add_fonts` 把字节交给 fontdb，
//! fontdb 用 ttf-parser 解析，只吃 TTF/OTF 与其集合。而 EPUB 里嵌 WOFF1 的书是真实
//! 存在的（Calibre 的老输出），不解一层就等于对着一份本来能用的字体装看不见。
//! WOFF2 需要 brotli，一期不引，由调用方按「这本书没有可用内嵌字体」降级。
//!
//! 格式按 W3C WOFF 1.0 的版式：44 字节头 + `numTables` 条 20 字节的表目录，
//! 每条记着「压缩后在哪、多长」与「解压后多长」；输出是标准 sfnt —— 12 字节头 +
//! 每条 16 字节的表记录 + 按 4 字节对齐排开的表数据。
//!
//! **任何一处不成立都返回 `None`**，不 panic、也不半套：调用方拿不到字节时的表现
//! 与「这本书没有内嵌字体」逐字节相同，而那正是今天的行为，不构成回归。

use flate2::bufread::ZlibDecoder;
use std::io::Read;

/// 头 44 字节 + 每条表目录 20 字节。
const HEADER: usize = 44;
const ENTRY: usize = 20;
/// sfnt 头 12 字节 + 每条表记录 16 字节，是解出来的那一份的版式。
const SFNT_HEADER: usize = 12;
const SFNT_ENTRY: usize = 16;

/// 是不是 WOFF1（只看签名，签名之后的完整性交给 [`decode`]）。
/// 容器层用它决定「这串字节要不要先过一道」，不必把整份解一遍才认出来。
pub fn is_woff(data: &[u8]) -> bool {
    data.starts_with(b"wOFF")
}

/// WOFF1 字节 → sfnt 字节。认不出签名、表目录越界、flavor 不支持、任何一表解不出
/// 声明长度时都是 `None`。
pub fn decode(data: &[u8]) -> Option<Vec<u8>> {
    if !is_woff(data) || data.len() < HEADER {
        return None;
    }
    // u32 从 4 起（签名占 0..4）：必须还是 TTF / OTF 那一层，WOFF 允许套 Apple 的
    // `true` 轮廓，也允许 `OTTO`（CFF 轮廓）；ttf-parser 三种都吃。
    let flavor = u32_at(data, 4)?;
    if !matches!(flavor, 0x0001_0000 | 0x7472_7565 | 0x4F54_544F) {
        return None;
    }
    let num_tables = u16_at(data, 12)? as usize;
    if num_tables == 0 {
        return None;
    }
    let dir_end = HEADER.checked_add(num_tables.checked_mul(ENTRY)?)?;
    if data.len() < dir_end {
        return None;
    }

    struct Entry {
        tag: [u8; 4],
        offset: usize,
        comp_len: usize,
        orig_len: usize,
        checksum: u32,
    }
    let mut entries = Vec::with_capacity(num_tables);
    for i in 0..num_tables {
        let at = HEADER + i * ENTRY;
        let mut tag = [0u8; 4];
        tag.copy_from_slice(&data[at..at + 4]);
        entries.push(Entry {
            tag,
            offset: u32_at(data, at + 4)? as usize,
            comp_len: u32_at(data, at + 8)? as usize,
            orig_len: u32_at(data, at + 12)? as usize,
            checksum: u32_at(data, at + 16)?,
        });
    }

    // 先排一遍偏移，确认声明的表数据真在文件里 —— 表与表可以在 WOFF 里重叠存放，
    // 但越界的读数一律不接受（`checked_add` + 长度比较两处都过才算）。
    for e in &entries {
        let end = e.offset.checked_add(e.comp_len)?;
        if end > data.len() {
            return None;
        }
    }

    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(&flavor.to_be_bytes());
    out.extend_from_slice(&(num_tables as u16).to_be_bytes());
    // 这三项是 sfnt 头里的二分查找加速值。ttf-parser 不依赖它们正确，
    // 但输出该是一份规规矩矩的 sfnt，就按规范算。
    let depth = usize::BITS - 1 - num_tables.leading_zeros(); // floor(log2(n))
    let search_range = (1u32 << depth) as usize * SFNT_ENTRY;
    out.extend_from_slice(&(search_range as u16).to_be_bytes());
    out.extend_from_slice(&(depth as u16).to_be_bytes());
    out.extend_from_slice(&((num_tables * SFNT_ENTRY - search_range) as u16).to_be_bytes());

    // 表数据紧跟在「头 + 表记录」之后，按 4 字节对齐依次排开。
    let mut offsets = Vec::with_capacity(num_tables);
    let mut cursor = SFNT_HEADER + num_tables * SFNT_ENTRY;
    for e in &entries {
        offsets.push(cursor);
        cursor = cursor.checked_add(e.orig_len)?.next_multiple_of(4);
    }

    for (i, e) in entries.iter().enumerate() {
        let at = offsets[i];
        out.extend_from_slice(&e.tag);
        out.extend_from_slice(&e.checksum.to_be_bytes());
        out.extend_from_slice(&(at as u32).to_be_bytes());
        out.extend_from_slice(&(e.orig_len as u32).to_be_bytes());
    }
    debug_assert_eq!(out.len(), SFNT_HEADER + num_tables * SFNT_ENTRY);

    for (i, e) in entries.iter().enumerate() {
        // 表内容不解析，只搬运：这一层的职责是「把字节排成 sfnt 的版式」，
        // 认不认得这张表是 ttf-parser 的事。
        debug_assert_eq!(out.len(), offsets[i]);
        if e.comp_len == e.orig_len {
            // 未压缩存放（WOFF 允许：压了反而变长时就这么留着）。
            out.extend_from_slice(&data[e.offset..e.offset + e.comp_len]);
        } else {
            out.extend_from_slice(&inflate(
                &data[e.offset..e.offset + e.comp_len],
                e.orig_len,
            )?);
        }
        let pad = out.len().next_multiple_of(4) - out.len();
        out.extend_from_slice(&[0u8; 3][..pad]);
    }
    Some(out)
}

/// zlib（RFC1950）解压，并且**只取声明的那 `want` 字节**：WOFF 编码器常在流尾留填充，
/// 而解出比声明多或少都说明这份数据不可信，直接判废。
fn inflate(compressed: &[u8], want: usize) -> Option<Vec<u8>> {
    let mut reader = ZlibDecoder::new(std::io::BufReader::new(compressed));
    let mut buf = vec![0u8; want];
    let mut got = 0usize;
    loop {
        let read = reader.read(&mut buf[got..]).ok()?;
        if read == 0 {
            break;
        }
        got += read;
        if got == want {
            break;
        }
    }
    (got == want).then_some(buf)
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    Some(u32::from_be_bytes(data.get(at..end)?.try_into().ok()?))
}

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    let end = at.checked_add(2)?;
    Some(u16::from_be_bytes(data.get(at..end)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    //! 只用公开 API：`is_woff` / `decode`。测试数据是**自己按版式搭出来的**一份 WOFF，
    //! 表内容是无意义的字节 —— 本模块不解析表，只做「逐表解压 + 重排成 sfnt」这一件事，
    //! 所以要钉住的正是偏移、对齐、flavor 与长度校验，不是字形。

    use super::decode;
    use flate2::{Compression, write::ZlibEncoder};
    use std::io::Write;

    /// 一表 WOFF：`name` 表放 `text`，其余头字段按规范填。
    fn woff(flavor: u32, tables: &[([u8; 4], &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"wOFF");
        out.extend_from_slice(&flavor.to_be_bytes());
        out.extend_from_slice(&[0u8; 4]); // length：编码器写的是什么不重要，decode 不读它
        out.extend_from_slice(&(tables.len() as u16).to_be_bytes());
        out.extend_from_slice(&[0u8; 2]); // reserved
        out.extend_from_slice(&[0u8; 4]); // totalSfntSize
        out.extend_from_slice(&[0u8; 4]); // major/minor
        out.extend_from_slice(&[0u8; 20]); // meta 与 private 那五项

        let dir_end = 44 + tables.len() * 20;
        let mut offset = dir_end;
        let mut dir = Vec::new();
        let mut body = Vec::new();
        for (tag, raw) in tables {
            let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
            enc.write_all(raw).unwrap();
            let packed = enc.finish().unwrap();
            // 压完反而更长的表按 WOFF 的写法原样放着（comp_len == orig_len）。
            let (stored, comp_len) = if packed.len() >= raw.len() {
                (raw.to_vec(), raw.len())
            } else {
                let len = packed.len();
                (packed, len)
            };
            dir.extend_from_slice(tag);
            dir.extend_from_slice(&(offset as u32).to_be_bytes());
            dir.extend_from_slice(&(comp_len as u32).to_be_bytes());
            dir.extend_from_slice(&(raw.len() as u32).to_be_bytes());
            dir.extend_from_slice(&0u32.to_be_bytes()); // checksum
            body.extend_from_slice(&stored);
            offset += comp_len;
        }
        out.extend_from_slice(&dir);
        out.extend_from_slice(&body);
        out
    }

    fn read_u32(data: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(data[at..at + 4].try_into().unwrap())
    }
    fn read_u16(data: &[u8], at: usize) -> u16 {
        u16::from_be_bytes(data[at..at + 2].try_into().unwrap())
    }

    /// 一表 7 字节：解出来的 sfnt 必须是「12 字节头 + 16 字节表记录 + 补到 4 的倍数」。
    /// 7 不对齐正是这条测试要钉的东西 —— 偏移错了 ttf-parser 会解出一张空表，
    /// 而屏幕上只是「字体没生效」，看不出原因。
    #[test]
    fn decodes_one_table_and_pads_to_four() {
        let raw = *b"abcdefg";
        let packed = woff(0x0001_0000, &[(*b"name", &raw)]);
        let sfnt = decode(&packed).expect("一份合法的 WOFF 该解得开");

        assert_eq!(read_u32(&sfnt, 0), 0x0001_0000, "flavor 要原样带到 sfnt 头");
        assert_eq!(read_u16(&sfnt, 4), 1, "numTables");
        let (tag, checksum, offset, length) = (
            &sfnt[12..16],
            read_u32(&sfnt, 16),
            read_u32(&sfnt, 20) as usize,
            read_u32(&sfnt, 24) as usize,
        );
        assert_eq!(tag, b"name");
        assert_eq!(checksum, 0);
        assert_eq!(offset, 28, "表数据紧跟在 12 + 1 * 16 之后");
        assert_eq!(length, 7);
        assert_eq!(&sfnt[offset..offset + length], &raw[..]);
        assert_eq!(sfnt.len(), 36, "7 字节补零到 4 的倍数：28 + 8");
    }

    /// 两表：第二张的偏移必须跟着第一张的补齐走，二分查找那三项要按规范算。
    #[test]
    fn second_table_offset_follows_the_first_pads() {
        let a = *b"aaa";
        let b = *b"bbbbbbb";
        let packed = woff(0x0001_0000, &[(*b"head", &a), (*b"cmap", &b)]);
        let sfnt = decode(&packed).unwrap();

        assert_eq!(read_u16(&sfnt, 4), 2);
        assert_eq!(read_u32(&sfnt, 20) as usize, 44, "head 在 12 + 32");
        assert_eq!(read_u32(&sfnt, 36) as usize, 48, "cmap 跟在补齐后的下一格");
        assert_eq!(&sfnt[44..47], &a[..]);
        assert_eq!(&sfnt[48..55], &b[..]);
        // numTables=2 ⇒ entrySelector=1, searchRange=32, rangeShift=0。
        assert_eq!(read_u16(&sfnt, 6), 32);
        assert_eq!(read_u16(&sfnt, 8), 1);
        assert_eq!(read_u16(&sfnt, 10), 0);
    }

    #[test]
    fn cff_and_apple_flavors_are_both_accepted() {
        for flavor in [0x4F54_544Fu32, 0x7472_7565] {
            let packed = woff(flavor, &[(*b"name", b"x")]);
            assert_eq!(
                read_u32(&decode(&packed).unwrap(), 0),
                flavor,
                "OTTO / true 这两种轮廓 ttf-parser 都吃，不该在签名这一步就丢"
            );
        }
    }

    /// 认不出的 flavor（比如套了个集合）与坏签名：一律 `None`，不猜。
    #[test]
    fn rejects_bad_signature_and_unsupported_flavor() {
        assert!(decode(b"not a font at all").is_none());
        assert!(decode(&woff(u32::from_be_bytes(*b"ttcf"), &[(*b"name", b"x")])).is_none());
        // 头都不够 44 字节
        let short = woff(0x0001_0000, &[(*b"name", b"x")]);
        assert!(decode(&short[..20]).is_none());
    }

    /// 表目录写着数据在文件外面 —— 恶意或坏掉的容器都可能这样，读数必须先验边界。
    /// 这一条是本模块唯一的安全面。
    #[test]
    fn rejects_table_data_outside_the_file() {
        let mut packed = woff(0x0001_0000, &[(*b"name", b"abcdefg")]);
        let offset_at = 44 + 4; // 第一条表目录的 offset 字段
        packed[offset_at..offset_at + 4].copy_from_slice(&(999_999u32).to_be_bytes());
        assert!(decode(&packed).is_none());
    }

    /// 声明的解压长度与实际不符：`inflate` 拿不满 `orig_len` 就该判废，
    /// 而不是把半张表交给 ttf-parser。
    #[test]
    fn rejects_short_decompressed_length() {
        let mut packed = woff(0x0001_0000, &[(*b"name", b"abcdefg")]);
        let orig_len_at = 44 + 12; // 第一条表目录的 origLength 字段
        packed[orig_len_at..orig_len_at + 4].copy_from_slice(&(1u32 << 20).to_be_bytes());
        assert!(decode(&packed).is_none());
    }

    #[test]
    fn empty_directory_is_not_a_font() {
        let mut packed = woff(0x0001_0000, &[(*b"name", b"x")]);
        packed[12..14].copy_from_slice(&0u16.to_be_bytes());
        assert!(decode(&packed).is_none());
    }
}
