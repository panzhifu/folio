//! MOBI 正文记录的两种解压：PalmDOC（GCR/LZ77 变体）与 HUFF/CDIC。
//!
//! MOBI 头里那格 `compression`（记录 0 偏移 0）只有三个值认得：
//!
//! ```text
//! 1      不压
//! 2      PalmDOC：逐记录的 LZ77 变体，字典就是已经解出来的那部分正文本身
//! 17480  HUFF/CDIC：符号 Huffman 码 + 词尾替换字典（KindleGen 的默认档）
//! ```
//!
//! **每一条记录都是独立的一帧**：PalmDOC 的回指距离不跨记录，HUFF 的比特流也是
//! 每条记录从整字节重新起算。所以装配正文流就是「逐条解、拼起来」，跨记录的
//! `filepos` 换算由 [`super`](crate::content::format::mobi) 那一层按每帧解出的
//! 长度表做。
//!
//! 为什么在树里手写而不引库：这一族只有两种码，PalmDOC 那份是十几个分支的状态机，
//! HUFF/CDIC 的代价全在**建树**（一张 256 项快查表 + 一张码长表 + 一张符号偏移表），
//! 解码本身就是查表加移位。而 PalmDB 是本仓库第一个非 ZIP 容器，引一个 C 库进来
//! 要多养一条 Android NDK 与 iOS 的构建线，换不来任何东西。
//!
//! ## 三处最容易记错的地方
//!
//! **一、HUFF 的码字是「先查快查表，表的终止位没置时再走码长表」。** 少写后一支的
//! 解码器只在**一部分**书上输出对不齐的垃圾 —— 不是全部书，所以症状是「有的书能读」，
//! 属于最难查的那一类。
//!
//! **二、`code_length == 0` 不是一种码长，是表没建对。** 快查表项的低位就是码长，
//! 拿 0 去除 `32 - length` 那个移位会直接算错，所以下面在移位之前先挡掉。
//!
//! **三、CDIC 交出来的符号自己可能还是压过的。** 那个 2 字节长度字段的最高位是
//! 「已是明文」的标记；为 0 时这一格还要再走一遍同一套解码（递归），
//! 而一份坏 CDIC 完全可以把自己写进自己的符号表里 —— 所以有 [`MAX_DEPTH`]。

/// HUFF/CDIC 的码表按 32 位窗口比对，码长最长 32 位，所以表是 `0..=32` 共 33 项
/// （第 0 项是「无码」那一格）。
const CODETABLE_SIZE: usize = 33;
/// 快查表（data1）的项数：按码字最高 8 位索引。
const TABLE1_SIZE: usize = 256;
const HUFF_MAGIC: [u8; 4] = *b"HUFF";
const CDIC_MAGIC: [u8; 4] = *b"CDIC";
/// HUFF 记录**声明**的头长度下限：magic 4 + 头长 4 + data1 偏移 4 + data2 偏移 4，
/// 再加那两个偏移的小端副本 8。声明得比这还短就连必读的四格都不全。
const HUFF_MIN_HEADER: usize = 24;
/// CDIC 头：magic 4 + 头长 4 + 索引总数 4 + 位数 4。索引表从第 16 个字节起。
const CDIC_HEADER: usize = 16;
/// 符号自指的递归上限。
///
/// 真书的嵌套比「一层就够」深：一个词尾符号自己也可以是码流。这个数只是把
/// 「成环的 CDIC」挡住的闸门，取 20 与 libmobi 的 `MOBI_HUFFMAN_MAXDEPTH` 同档 ——
/// 定得比参照实现小而真书偏深时，症状是「这本书解到一半停了」，最难查。
const MAX_DEPTH: usize = 20;

/// PalmDOC（GCR）一帧的解压。
///
/// 分支按字节值划分，顺序不能换：`0x01..=0x08` 是「后面照抄 n 个字节」，
/// `0x09..=0x7f` 是「本字节原样」，`0x80..=0xbf` 是「回指 (距离, 长度) 对」，
/// `0xc0..=0xff` 是「一个空格 + 本字节去掉最高位」。`0x00` 单独一支：它就是 NUL，
/// 不是「抄 0 个字节」。
///
/// 回指越界（距离比已解出的部分还长）**补 `?` 而不是失败**：真书里见过坏的长度-
/// 距离对，为一格坏数据炸掉整本能读的书不值得；而补出来的问号在屏幕上是看得见的错，
/// 静默截断不是。
pub(crate) fn palmdoc_decompress(input: &[u8], out: &mut Vec<u8>) {
    let mut i = 0usize;
    while i < input.len() {
        let byte = input[i];
        i += 1;
        match byte {
            0x00 => out.push(0),
            0x01..=0x08 => {
                // 尾巴不够长就照实抄到这里：剩余长度不是错误，是截断的文件。
                let take = (byte as usize).min(input.len() - i);
                out.extend_from_slice(&input[i..i + take]);
                i += take;
            }
            0x09..=0x7f => out.push(byte),
            0x80..=0xbf => {
                if i >= input.len() {
                    break;
                }
                let pair = ((byte as u32 & 0x3f) << 8) | input[i] as u32;
                i += 1;
                let distance = (pair >> 3) as usize;
                let length = ((pair & 0x07) + 3) as usize;
                if distance == 0 || distance > out.len() {
                    out.extend(std::iter::repeat_n(b'?', length));
                    continue;
                }
                // 逐字节复制：长度可以跨过刚写出的那段（RLE 就是这么表达的），
                // 所以不能 memcpy 一段现成的区间。
                let start = out.len() - distance;
                for offset in 0..length {
                    let byte = out[start + offset];
                    out.push(byte);
                }
            }
            _ => {
                out.push(b' ');
                out.push(byte ^ 0x80);
            }
        }
    }
}

/// 建好的 HUFF/CDIC 表，可以服务整本书的每一条正文记录。
pub(crate) struct HuffCdic {
    /// 按码字最高 8 位索引的快查表：低 5 位是码长，位 7 是终止位，`>>8` 是 maxcode 基数。
    table1: [u32; TABLE1_SIZE],
    /// 码长 → 该长度最小的码字（已按 32 位窗口左对齐）。
    mincode: [u32; CODETABLE_SIZE],
    /// 码长 → 该长度最大的码字（同样左对齐）。
    maxcode: [u32; CODETABLE_SIZE],
    /// 符号偏移表：解出的 index 用它再映到字典里的位置。
    symbol_offsets: Vec<u32>,
    /// 每张 CDIC 记录头之后的字节，按 `symbol_offsets` 里那个偏移索引。
    dictionaries: Vec<Vec<u8>>,
    /// CDIC 声明的位数：`index >> code_length` 就是选第几张字典。
    code_length: u32,
    index_count: usize,
}

impl HuffCdic {
    /// 从 HUFF 记录 + 其后若干条 CDIC 记录建表。
    ///
    /// 结构门全在这一趟，解码那一路只管查表：一张错表解出来的是**看着合法**的垃圾
    /// 字节，而结构错在这里还能说清是哪一格不对。
    pub(crate) fn build(huff: &[u8], cdics: &[&[u8]]) -> Option<Self> {
        if huff.len() < HUFF_MIN_HEADER || huff[..4] != HUFF_MAGIC {
            return None;
        }
        let declared_header = u32_at(huff, 4) as usize;
        let data1 = u32_at(huff, 8) as usize;
        let data2 = u32_at(huff, 12) as usize;
        // 声明的头长度短于必读的那四格 + 两份小端副本，就谈不上后面两张表。
        if declared_header < HUFF_MIN_HEADER {
            return None;
        }
        if data1.checked_add(TABLE1_SIZE * 4)? > huff.len() {
            return None;
        }
        let mut table1 = [0u32; TABLE1_SIZE];
        for (i, item) in table1.iter_mut().enumerate() {
            *item = u32_at(huff, data1 + i * 4);
        }

        if data2.checked_add((CODETABLE_SIZE - 1) * 8)? > huff.len() {
            return None;
        }
        // data2 是 32 对 (mincode, maxcode)，码长从 1 到 32。码长 0 那格是「无码」，
        // 摆成「比任何码字都小 / 都比不过」，好让上面的查表一路不会误命中它。
        let mut mincode = [0u32; CODETABLE_SIZE];
        let mut maxcode = [0u32; CODETABLE_SIZE];
        mincode[0] = 0;
        maxcode[0] = u32::MAX;
        for length in 1..CODETABLE_SIZE {
            let at = data2 + (length - 1) * 8;
            // 表里那两个数是**按那一个码长**给的（右对齐），窗口是 32 位左对齐的，
            // 所以两边都要移位才可比。移位用 wrapping：`maxcode + 1` 在
            // maxcode = 0xFFFFFFFF 时会溢出，那是「这一档兜住全部」的写法而不是错。
            mincode[length] = u32_at(huff, at).wrapping_shl(32 - length as u32);
            maxcode[length] = u32_at(huff, at + 4)
                .wrapping_add(1)
                .wrapping_shl(32 - length as u32)
                .wrapping_sub(1);
        }

        if cdics.is_empty() {
            return None;
        }
        let mut code_length: Option<u32> = None;
        let mut index_count: Option<usize> = None;
        let mut symbol_offsets: Vec<u32> = Vec::new();
        let mut dictionaries: Vec<Vec<u8>> = Vec::new();
        for cdic in cdics {
            if cdic.len() < CDIC_HEADER + 2 || cdic[..4] != CDIC_MAGIC {
                return None;
            }
            let count = u32_at(cdic, 8) as usize;
            let length = u32_at(cdic, 12);
            // 各张 CDIC 必须说同一个位数、同一个总数：不然 `index >> code_length`
            // 会选错字典，而选错字典的输出一本正经。
            if code_length.is_some_and(|prev| prev != length)
                || index_count.is_some_and(|prev| prev != count)
            {
                return None;
            }
            if length == 0 || length > 16 || count == 0 {
                return None;
            }
            code_length = Some(length);
            index_count = Some(count);
            // 一张 CDIC 只携带它那一截索引：按位数分下来就是它该读几项。
            let remaining = count.saturating_sub(symbol_offsets.len());
            let take = remaining.min(1usize << length);
            let mut at = CDIC_HEADER;
            for _ in 0..take {
                if at + 2 > cdic.len() {
                    return None;
                }
                let offset = u16_at(cdic, at) as usize;
                at += 2;
                // 偏移是相对字典头给的，那里先站着 2 个字节的长度字段。
                // 不在这里验长度：验一次要多读两字节，而解码那一路按同一个口径读。
                if CDIC_HEADER + offset + 2 > cdic.len() {
                    return None;
                }
                symbol_offsets.push(offset as u32);
            }
            dictionaries.push(cdic[CDIC_HEADER..].to_vec());
        }
        let (Some(code_length), Some(index_count)) = (code_length, index_count) else {
            return None;
        };
        // 索引没读满是「带的 CDIC 比声明的少」—— 坏文件，别带着半张表去解码。
        if symbol_offsets.len() != index_count || dictionaries.is_empty() {
            return None;
        }
        Some(Self {
            table1,
            mincode,
            maxcode,
            symbol_offsets,
            dictionaries,
            code_length,
            index_count,
        })
    }

    /// 解一条正文记录，追加到 `out`。
    ///
    /// `limit` 是这一帧最多还能吐出多少字节（调用方按整本书声明的 `text_length`
    /// 扣着算）：Huffman 码流按位走，最后那几格可能是补位而不是符号，
    /// 没有上限就会多吐一截谁也看不懂的尾巴。
    pub(crate) fn decompress(&self, input: &[u8], limit: usize, out: &mut Vec<u8>) -> Option<()> {
        if input.is_empty() {
            return Some(());
        }
        let before = out.len();
        self.decode(input, limit, out, 0)?;
        (out.len() - before <= limit).then_some(())
    }

    fn decode(&self, input: &[u8], limit: usize, out: &mut Vec<u8>, depth: usize) -> Option<()> {
        if depth > MAX_DEPTH {
            return None;
        }
        let total_bits = input.len() * 8;
        let mut bitpos = 0usize;
        while bitpos < total_bits {
            if out.len() > limit {
                return None;
            }
            let code = window32(input, bitpos);
            let t1 = self.table1[(code >> 24) as usize];
            let mut length = (t1 & 0x1f) as usize;
            if length > 32 {
                return None;
            }
            if length == 0 {
                // 那一格快查表没定义码长：既可能是补齐位撞上了空槽（帧尾就是这样的
                // 一串 0），也可能是坏码字。**当帧结束**而不是报错 —— 码长 0 会让
                // `bitpos` 永远不前进，报错与停帧之间只有一个不死人的选项。
                // 「少解了」这一头由装配那一步拿声明的 `text_length` 兜：
                // 长度对不上就是这本书读不动，那里才是说得出话的地方。
                break;
            }
            let mut maxcode = (t1 >> 8)
                .wrapping_add(1)
                .wrapping_shl(32 - length as u32)
                .wrapping_sub(1);
            // 位 7 是终止位。没置的那一支说明真实码长比 24 还长，往上逐项比 mincode。
            if t1 & 0x80 == 0 {
                while length < CODETABLE_SIZE && code < self.mincode[length] {
                    length += 1;
                }
                if length >= CODETABLE_SIZE {
                    return None;
                }
                maxcode = self.maxcode[length];
            }
            if bitpos + length > total_bits {
                // 帧尾的补齐位凑不出下一个完整码字：这是**填充**，不是坏记录。
                // 当成失败的话每一条正文记录的最后一格都会报错
                // （拿 libmobi 的 `-7` 导出对拍真书时就是这么暴露的）。
                break;
            }
            bitpos += length;
            let index = ((maxcode.wrapping_sub(code)) >> (32 - length)) as usize;
            if index >= self.index_count {
                return None;
            }
            let dictionary = index >> self.code_length;
            let dict = self.dictionaries.get(dictionary)?;
            let offset = self.symbol_offsets[index] as usize;
            if offset + 2 > dict.len() {
                return None;
            }
            let raw = u16_at(dict, offset) as usize;
            // 最高位：这一格已经是明文；为 0 时它自己还是一帧 Huffman 码流。
            let is_plain = raw >> 15 == 1;
            let symbol_len = raw & 0x7fff;
            let symbol = dict.get(offset + 2..offset + 2 + symbol_len)?;
            if is_plain {
                out.extend_from_slice(symbol);
            } else {
                self.decode(symbol, limit, out, depth + 1)?;
            }
        }
        Some(())
    }
}

/// 从 `bitpos` 起取 32 位，输入用完的部分在低位补 0。
///
/// 码表是按「32 位窗口最高位在前」比对的，所以超出输入的那几格**必须**是 0：
/// 补 1 会把最后一格码字比到别的码长上去。最后一帧的尾部就是这么处理的 ——
/// 因此这里不返回 `Option`，字节不够就是补零，不是失败。
fn window32(input: &[u8], bitpos: usize) -> u32 {
    let byte = bitpos / 8;
    let shift = bitpos % 8;
    let mut window = [0u8; 4];
    for (i, slot) in window.iter_mut().enumerate() {
        *slot = input.get(byte + i).copied().unwrap_or(0);
    }
    let mut value = u32::from_be_bytes(window);
    if shift > 0 {
        let next = input.get(byte + 4).copied().unwrap_or(0) as u32;
        value = (value << shift) | (next >> (8 - shift));
    }
    value
}

/// 正文记录末尾那段 extra data 的总长（**含各段自己的长度字段**）。
///
/// MOBI 头偏移 242 那格 `extra_flags` 的每一个置位，都对应记录尾部的一段：
///
/// ```text
/// 位 15..1   每段以一个「反向变宽长度」结尾：从记录末尾往前读，
///            低 7 位拼成值、最高位是终止位，值本身就包含这几个长度字节
/// 位 0       多字节字符跨记录的溢出：末尾那一字节的低 2 位 + 1 个字节
/// ```
///
/// 顺序是**先高位后低位**：最高位那一段站在记录最末尾，往前的每一段依次属于更低的位。
/// 这一条不是推测 —— 拿 `reference/libmobi/tests/samples` 里五本真书对拍过：
/// 按这套剥完再解压，吐出的字节与 `mobitool -7 -d` 导出的 rawml **一字不差**
/// （含一本每条记录 4101 → 4096 的不压样本）。
///
/// 返回 `None` 只有一种情况：长度字段读到记录开头还没遇到终止位（结构坏了）。
/// 调用方该把那一格当「这本书读不动」而不是「当没这段剥」：少剥会在正文里留下
/// 一截二进制垃圾，而那正是「书架上看着正常、打开是乱码」的形状。
pub(crate) fn extras_len(record: &[u8], extra_flags: u16) -> Option<usize> {
    if extra_flags == 0 || record.is_empty() {
        return Some(0);
    }
    // `at` 的含义与 libmobi 的缓冲游标一致：**下一个要往前读的字节**的下标。
    let mut at = record.len() - 1;
    let mut extra = 0usize;
    for bit in (1..16).rev() {
        if extra_flags & (1 << bit) == 0 {
            continue;
        }
        let mut value = 0u32;
        let mut shift = 0;
        let mut count = 0usize;
        let mut terminated = false;
        // 反向变宽长度最多四字节。
        let available = 4usize.min(at + 1);
        while count < available {
            let byte = record[at - count];
            value |= ((byte & 0x7f) as u32) << shift;
            shift += 7;
            count += 1;
            if byte & 0x80 != 0 {
                terminated = true;
                break;
            }
        }
        if !terminated {
            return None;
        }
        let value = value as usize;
        // 值 = 长度字节 + 它下面的数据；两者都归这一段。
        if value < count || at < count + (value - count) {
            return None;
        }
        extra += value;
        at -= value;
    }
    if extra_flags & 1 != 0 {
        let overflow = (record[at] & 0x03) as usize + 1;
        if at + 1 < overflow {
            return None;
        }
        extra += overflow;
    }
    (extra <= record.len()).then_some(extra)
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([bytes[at], bytes[at + 1]])
}

/// PalmDB 一族的结构字段全线大端。
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

#[cfg(test)]
mod tests {
    //! 解压这一层的断言全部围绕一件事：**给定的字节必须解出设计的那几个字节**。
    //! 所以码表、字典、码流是一格格手摆的；用「自己写的编码器再自己解一遍」当证据
    //! 只会证明两边错得一致。

    use super::{
        CDIC_HEADER, CODETABLE_SIZE, HUFF_MIN_HEADER, HuffCdic, TABLE1_SIZE, extras_len,
        palmdoc_decompress, u16_at, u32_at, window32,
    };

    fn out(input: &[u8]) -> Vec<u8> {
        let mut dst = Vec::new();
        palmdoc_decompress(input, &mut dst);
        dst
    }

    /// 四个分支各钉一条，按字节值区间分。
    #[test]
    fn palmdoc_branches_by_byte_range() {
        assert_eq!(out(b"<p>hi</p>"), b"<p>hi</p>");
        assert_eq!(out(&[0x03, b'a', b'b', b'c']), b"abc");
        assert_eq!(out(&[0xc0 | b'x']), b" x");
        assert_eq!(out(&[0x00]), b"\0");
    }

    /// 回指对的位切法：`(距离 << 3) | (长度 - 3)`，两字节大端拼完后**右移 3** 取距离。
    ///
    /// 这一条单独钉是因为距离是 11 位而不是 12 位、长度下限是 3。记错的解码器
    /// 在短距离上会解出错位的字符，屏幕上看到的是「个别字成了乱码」而不是「整本读不出」。
    #[test]
    fn palmdoc_backreference_copies_from_earlier_output() {
        for (distance, length, want) in [
            (8usize, 3usize, "abcdefghabc"),
            (8, 5, "abcdefghabcde"),
            (1, 4, "aaaaa"),
        ] {
            let mut input = b"abcdefgh"[..if distance == 1 { 1 } else { 8 }].to_vec();
            let pair = ((distance << 3) | (length - 3)) as u32;
            input.push(0x80 | (pair >> 8) as u8);
            input.push((pair & 0xff) as u8);
            assert_eq!(
                out(&input),
                want.as_bytes(),
                "距离 {distance} 长度 {length}"
            );
        }
    }

    /// 回指越界补 `?` 而**不丢后面的可读部分**：坏一帧不该把整条记录带走。
    #[test]
    fn palmdoc_out_of_range_distance_is_visible_rather_than_fatal() {
        let mut input = b"x".to_vec();
        let pair: u32 = 40 << 3;
        input.push(0x80 | (pair >> 8) as u8);
        input.push((pair & 0xff) as u8);
        input.extend_from_slice(b"y");
        assert_eq!(out(&input), b"x???y");
    }

    /// 抄写分支的尾巴被截断：抄到这里为止，不越界读。
    #[test]
    fn palmdoc_literal_run_clamps_to_what_is_left() {
        assert_eq!(out(&[0x08, b'a', b'b']), b"ab");
    }

    /// 一份最小可用的 HUFF/CDIC：码长 8、两个明文符号、一张字典。
    ///
    /// 快查表按**码字最高 8 位**索引，所以码字取 `0x01` 与 `0x02`：输入 `[0x01, 0x02]`
    /// 第一格窗口的最高字节是 `0x01`，消费 8 位之后是 `0x02` —— 两格落在表里不同的位置，
    /// 才真的验到「查表」这件事。
    ///
    /// 表项里 `>>8` 那一格写的是**这一码长里最后一个码字**（不是符号号）：解码按
    /// `index = (maxcode - code) >> (32 - length)` 取符号，所以码字小的那个 index 大 ——
    /// `0x01` 拿到符号 1，`0x02` 拿到符号 0。这一格最容易记反，记反的症状是
    /// 「正文里的字符成对调了」。
    fn huffcdic(terminating: bool) -> (Vec<u8>, Vec<Vec<u8>>) {
        let data1 = HUFF_MIN_HEADER;
        let data2 = data1 + TABLE1_SIZE * 4;
        let mut huff = vec![0u8; data2 + (CODETABLE_SIZE - 1) * 8];
        huff[..4].copy_from_slice(b"HUFF");
        huff[4..8].copy_from_slice(&(HUFF_MIN_HEADER as u32).to_be_bytes());
        huff[8..12].copy_from_slice(&(data1 as u32).to_be_bytes());
        huff[12..16].copy_from_slice(&(data2 as u32).to_be_bytes());
        let base = 0x02u32; // 码长 8 这一档的最后一个码字
        for code in [0x01u32, 0x02u32] {
            let term = if terminating { 0x80 } else { 0 };
            let item = (base << 8) | term | 8;
            let at = data1 + (code as usize) * 4;
            huff[at..at + 4].copy_from_slice(&item.to_be_bytes());
        }
        // data2 里码长 8 那一对：原始值是右对齐的，左对齐的移位由 `build` 做。
        let at = data2 + (8 - 1) * 8;
        huff[at..at + 4].copy_from_slice(&0x01u32.to_be_bytes());
        huff[at + 4..at + 8].copy_from_slice(&base.to_be_bytes());

        let mut cdic = vec![0u8; CDIC_HEADER];
        cdic[..4].copy_from_slice(b"CDIC");
        cdic[4..8].copy_from_slice(&(CDIC_HEADER as u32).to_be_bytes());
        cdic[8..12].copy_from_slice(&2u32.to_be_bytes());
        cdic[12..16].copy_from_slice(&1u32.to_be_bytes());
        // 符号偏移是**相对于头之后那段**的，而索引表自己也站在那段里
        // （两个 u16 = 4 个字节），所以第一个符号的偏移不是 0 而是 4。
        let index_table = 2 * 2;
        let mut payload = Vec::new();
        // 字典顺序就是符号号顺序：0 → 「甲」，1 → 「b」。
        for text in ["甲", "b"] {
            cdic.extend_from_slice(&((payload.len() + index_table) as u16).to_be_bytes());
            payload.extend_from_slice(&((text.len() as u16) | 0x8000).to_be_bytes());
            payload.extend_from_slice(text.as_bytes());
        }
        cdic.extend_from_slice(&payload);
        (huff, vec![cdic])
    }

    fn decode(huff: &[u8], cdics: &[Vec<u8>], input: &[u8], limit: usize) -> Option<Vec<u8>> {
        let refs: Vec<&[u8]> = cdics.iter().map(Vec::as_slice).collect();
        let codec = HuffCdic::build(huff, &refs)?;
        let mut out = Vec::new();
        codec.decompress(input, limit, &mut out).map(|_| out)
    }

    /// 快查表**终止位置着**那一支。
    #[test]
    fn huffcdic_reads_symbols_through_the_fast_table() {
        let (huff, cdics) = huffcdic(true);
        assert_eq!(
            decode(&huff, &cdics, &[0x01, 0x02], 64).as_deref(),
            Some("b甲".as_bytes())
        );
    }

    /// 快查表**终止位没置**那一支：码长得从 mincode/maxcode 那两张表往上找。
    ///
    /// 这是全模块最容易被「合成夹具碰巧过了」掩盖的一段 —— 只测终止位那一支的
    /// 解码器在真书上会挑中一部分失败，所以这里把同一份字典用另一条路再解一遍。
    #[test]
    fn huffcdic_falls_back_to_the_code_length_tables_when_the_termination_bit_is_clear() {
        let (huff, cdics) = huffcdic(false);
        assert_eq!(
            decode(&huff, &cdics, &[0x01, 0x02], 64).as_deref(),
            Some("b甲".as_bytes())
        );
    }

    /// 帧尾的补齐位**不是**坏记录：码字凑不满一帧就在这里停，把已经解出的东西交回去。
    ///
    /// 这条是真书上撞出来的：当成失败时每一条正文记录都会在最后报错，
    /// 而整本书看起来「解得出前 268 个字节然后停」—— 比读不出更难查。
    #[test]
    fn trailing_padding_bits_end_the_frame_rather_than_failing() {
        let (huff, cdics) = huffcdic(true);
        // 一个完整码字（0x01 → 符号 1）之后剩三个 1 位：凑不出下一个码长 8 的码字。
        let got = decode(&huff, &cdics, &[0b0000_0001, 0b1110_0000], 64);
        assert_eq!(got.as_deref(), Some("b".as_bytes()));
    }

    /// 撞上快查表里没定义码长那一格：**当帧结束**，已解出的东西照样交回去。
    ///
    /// 不能报错：报错的下游只能报「这本书读不动」，而帧尾的补齐位天天长这样。
    /// 也不能继续：码长 0 意味着 `bitpos` 不前进，那一格是死循环。
    /// 「是不是少解了」由装配那一步比 `text_length` 去判。
    #[test]
    fn an_undefined_table_slot_ends_the_frame_without_losing_what_was_read() {
        let (huff, cdics) = huffcdic(true);
        // 先解一个真符号，再撞上空槽。
        assert_eq!(
            decode(&huff, &cdics, &[0x01, 0x00], 64).as_deref(),
            Some("b".as_bytes())
        );
        // 整帧都是空槽：解出零个字节，而不是失败。
        assert_eq!(decode(&huff, &cdics, &[0x00], 64).as_deref(), Some(&[][..]));
    }

    /// 符号偏移指到字典外面：坏 CDIC 要说「解不出」，不能把邻居那格的字节当正文吐出去。
    #[test]
    fn huffcdic_refuses_a_symbol_offset_beyond_the_dictionary() {
        let (huff, mut cdics) = huffcdic(true);
        // 把第一个符号的偏移改成「字典末尾之后」。
        let at = CDIC_HEADER;
        let bad = (cdics[0].len() as u16) + 8;
        cdics[0][at..at + 2].copy_from_slice(&bad.to_be_bytes());

        assert_eq!(decode(&huff, &cdics, &[0x02], 64), None);
    }

    /// 两张 CDIC 说不同的位数 = 表建不起来：选错字典的输出比失败更难查。
    #[test]
    fn huffcdic_refuses_cdics_that_disagree_on_the_code_length() {
        let (huff, mut cdics) = huffcdic(true);
        let mut second = cdics[0].clone();
        second[12..16].copy_from_slice(&2u32.to_be_bytes());
        cdics.push(second);
        let refs: Vec<&[u8]> = cdics.iter().map(Vec::as_slice).collect();
        assert!(HuffCdic::build(&huff, &refs).is_none());
    }

    /// 32 位窗口：未对齐时把下一字节的低位接上，超出输入就补 0。
    ///
    /// 移位那一步是整个解码唯一「差一位就全错」的地方，所以它单独钉一条，
    /// 而不是让它藏在符号断言背后。
    #[test]
    fn the_bit_window_shifts_in_the_next_byte_and_pads_with_zero() {
        let input = [0b1000_0000u8, 0b0100_0000, 0, 0];
        assert_eq!(window32(&input, 0), 0x8040_0000);
        // 起位 1：`1000000` + 下一字节前一位 `0` → 高位那串是 0x0080_...。
        assert_eq!(window32(&input, 1), 0x0080_0000);
        // 只剩 8 位可用时低位补 0。
        assert_eq!(window32(&[0b1111_0000], 0), 0xF000_0000);
        // 输入比窗口短三格也是补零，不是失败：最后一帧的码字就是这么读完的。
        assert_eq!(window32(&[0xff, 0xff, 0xff], 0), 0xFFFFFF00);
    }

    /// 大端读法本身也要有门：整棵 MOBI 解析都靠这两格，读反了会得到
    /// 「记录数是一千六百万」这种一路错到底的数。
    #[test]
    fn the_big_endian_readers_read_two_and_four_bytes() {
        assert_eq!(u32_at(&[0x12, 0x34, 0x56, 0x78], 0), 0x1234_5678);
        assert_eq!(u16_at(&[0x00, 0x01, 0xff, 0xfe], 2), 0xfffe);
    }

    /// `extra_flags == 0`：尾部没有 extra data，一个字节都不该被剥。
    #[test]
    fn no_extra_flags_means_no_stripping() {
        assert_eq!(extras_len(b"body", 0), Some(0));
        assert_eq!(extras_len(b"", 3), Some(0));
    }

    /// 一条记录末尾摆两段：位 1 那一段（3 个字节的数据 + 1 个长度字节）与位 0
    /// 那一格多字节溢出。合起来 5 个字节 —— 与真书上量到的那个形状同档。
    #[test]
    fn extra_blocks_are_stripped_from_the_end_backwards() {
        let record = [b'B', b'O', b'D', b'Y', 0x00, 0xAA, 0xBB, 0xCC, 0x84];
        assert_eq!(extras_len(&record, 0b10), Some(4), "只算位 1 那一段");
        assert_eq!(extras_len(&record, 0b11), Some(5), "位 1 + 位 0");
        let keep = record.len() - extras_len(&record, 0b11).unwrap();
        assert_eq!(&record[..keep], b"BODY", "剥完剩下的就是正文");
    }

    /// 高位的段站在更末尾：两段并存时先读位 2，再读位 1。
    #[test]
    fn higher_flag_bits_are_closer_to_the_end() {
        //        [正文][位1 段: 数据2 + 长度3][位2 段: 数据1 + 长度2]
        let record = [b'A', 0x11, 0x22, 0x83, 0x33, 0x82];
        assert_eq!(extras_len(&record, 0b110), Some(5));
        assert_eq!(&record[..record.len() - 5], b"A");
    }

    /// 长度字段读到记录头还没终止位：`None`。
    ///
    /// 这一格必须是失败而不是「当没这段剥」：少剥会把二进制尾巴当正文排出去，
    /// 而那正是「架上看着正常、打开是乱码」的形状。
    #[test]
    fn an_unterminated_length_field_is_an_error_not_a_silent_pass_through() {
        // 只有三个可用字节，全都不带终止位。
        assert_eq!(extras_len(&[0x01, 0x02, 0x03], 1 << 15), None);
        // 段长声明得比记录还长。
        assert_eq!(extras_len(&[0x00, 0xf0], 0b10), None);
    }
}
