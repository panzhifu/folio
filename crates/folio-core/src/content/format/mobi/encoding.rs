//! MOBI 头里那一格 `text encoding`（记录 0 偏移 28）的解码。
//!
//! 这一格是**唯一的**编码声明，而它只有三个值真的被用过：
//!
//! ```text
//! 1252   cp1252（WinLatin1）：Mobipocket 时代欧美书默认档
//! 65001  UTF-8
//! 65002  UTF-16
//! ```
//!
//! 别的取值是真存在的（936 / 950 那类中文代码页，出现在 Mobipocket 早期的中文书里），
//! 而这里**不解**：`is_supported` 说false，由调用方当场把「这种编码这一版读不动」说清楚。
//! 判据是 [`super::super::sniff`] 那一条的重复出现 —— 认得出编码却解不动，要报成
//! 「读不动」而不是「不是书」，更不能**猜成 cp1252 硬解**：猜出来的 GBK 正文在屏幕上
//! 是一整本欧洲字母汤，架子上看着正常、打开才发现读不了，比一次拒绝坏得多。
//!
//! cp1252 的表只写 `0x80..=0x9F` 那 32 格：0..0x7F 与 ASCII 同，0xA0..0xFF 与
//! Latin-1 同，两族的全部差别就在中间那 32 格。手写而不是引 `encoding_rs`：为一张
//! 32 项的表把整张代码页数据库拉进依赖树，换不来任何东西。
//!
//! 五个 undefined 槽（`0x81`、`0x8D`、`0x8F`、`0x90`、`0x9D`）映成 `U+FFFD`：
//! 它们在标准里就没有字符，留着原样会往下传一个 C1 控制字符，屏幕上是一片空白，
//! 而替换字符至少说的是「这里本来就缺一个字符」。

const CP1252_UPPER: [char; 32] = [
    '€', '\u{fffd}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{fffd}', 'Ž',
    '\u{fffd}', '\u{fffd}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{fffd}',
    'ž', '\u{fffd}',
];

/// 这一版的解码器认得的代码页。
pub(crate) fn is_supported(codepage: u32) -> bool {
    matches!(codepage, 1252 | 65001 | 65002)
}

/// 按头里声明的那一格解成 UTF-8。
///
/// 非法字节一律降级成 `U+FFFD` 而不是失败：正文坏一格与整本读不出来是两件事，
/// 读者看到的应该是前者（`plans/导入管线.md` 第五节：判定失败的候选不能在库里留记录，
/// 但**已经收进来的书**坏了一个字节不该翻脸不认）。
pub(crate) fn decode_text(bytes: &[u8], codepage: u32) -> String {
    match codepage {
        65002 => {
            // 带 BOM 的先剥掉：BOM 不是正文，留着会让第一章开头多出一个不可见字符，
            // 而那一格正好处在进度与锚点的零点。
            let (body, little_endian) = match bytes {
                [0xff, 0xfe, rest @ ..] => (rest, true),
                [0xfe, 0xff, rest @ ..] => (rest, false),
                other => (other, true),
            };
            let units: Vec<u16> = if little_endian {
                body.as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| u16::from_le_bytes(*pair))
                    .collect()
            } else {
                body.as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| u16::from_be_bytes(*pair))
                    .collect()
            };
            String::from_utf16_lossy(&units)
        }
        1252 => bytes
            .iter()
            .map(|&byte| match byte {
                0x80..=0x9f => CP1252_UPPER[(byte - 0x80) as usize],
                // 0..0x7F 与 0xA0..0xFF 都是「字节值就是码点」那一段。
                _ => char::from_u32(byte as u32).unwrap_or('\u{fffd}'),
            })
            .collect(),
        // UTF-8 是缺省档：`is_supported` 已经挡掉了别的显式取值，
        // 这里只兜「声明缺失/为 0」那几种。
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

#[cfg(test)]
mod tests {
    //! 编码这一层的断言只看一件事：**解出来的字符是设计的那一个**，
    //! 而不是「长度对得上」。cp1252 中间那 32 格是最容易整段记错的。

    use super::{CP1252_UPPER, decode_text, is_supported};

    #[test]
    fn only_the_three_declared_codepages_are_decodable() {
        assert!(is_supported(1252));
        assert!(is_supported(65001));
        assert!(is_supported(65002));
        assert!(!is_supported(936), "GBK 要报「读不动」，不能当 cp1252 硬解");
        assert!(!is_supported(0));
    }

    /// cp1252 的三段：ASCII 不动、中间 32 格查表、上半区与 Latin-1 同。
    #[test]
    fn cp1252_maps_only_the_middle_thirty_two() {
        assert_eq!(decode_text(b"abc", 1252), "abc");
        assert_eq!(decode_text(&[0x80], 1252), "€");
        assert_eq!(decode_text(&[0x91], 1252), "‘");
        assert_eq!(decode_text(&[0xA0], 1252), "\u{a0}");
        assert_eq!(decode_text(&[0xE4], 1252), "ä");
    }

    /// 五个 undefined 槽必须是 `U+FFFD`：留着原样会把一个 C1 控制字符送进排版。
    #[test]
    fn cp1252_undefined_slots_are_visible_instead_of_invisible() {
        for byte in [0x81u8, 0x8D, 0x8F, 0x90, 0x9D] {
            assert_eq!(
                decode_text(&[byte], 1252),
                "\u{fffd}",
                "0x{byte:02X} 在标准里没有字符"
            );
        }
        // 表本身也不能含可打印以外的意外：长度 32 是被索引方式钉死的。
        assert_eq!(CP1252_UPPER.len(), 32);
    }

    #[test]
    fn utf8_is_passed_through_and_bad_bytes_degrade() {
        assert_eq!(decode_text("中文".as_bytes(), 65001), "中文");
        assert!(
            decode_text(&[0xe4, 0xb8, 0xad, 0xff], 65001).starts_with('中'),
            "坏字节只该毁那一格"
        );
    }

    /// UTF-16：BOM 剥掉、按大小端读、末尾落单的那个字节丢掉而不是补出一个怪字符。
    #[test]
    fn utf16_strips_the_bom_and_pairs_by_endianness() {
        // 「文」是 U+6587：小端就是 87 65；「e」是 U+0065：小端 65 00。
        assert_eq!(
            decode_text(&[0xff, 0xfe, 0x87, 0x65, 0x65, 0x00], 65002),
            "文e"
        );
        assert_eq!(
            decode_text(&[0xfe, 0xff, 0x65, 0x87, 0x00, 0x65], 65002),
            "文e"
        );
        // 末尾落单的那个字节被 chunks_exact 丢掉：凑不出一个码点就不该编出字符。
        assert_eq!(decode_text(&[0x87, 0x65, 0x00], 65002), "文");
    }

    /// 声明缺失（0）按 UTF-8 兜底 —— 与 `is_supported` 不冲突：那一格问的是
    /// 「这本书声明了什么」，这里管的是「拿到没声明的字节怎么读」。
    #[test]
    fn an_undeclared_codepage_falls_back_to_utf8() {
        assert_eq!(decode_text("書".as_bytes(), 0), "書");
    }
}
