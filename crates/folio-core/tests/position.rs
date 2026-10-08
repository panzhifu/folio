//! 阅读位置：字符偏移 ↔ 字节索引的互转与越界钳制。只依赖公开 API。

use folio_core::Locator;

const SRC: &str = "中文 abc 表情😀收尾";

#[test]
fn char_and_byte_roundtrip() {
    let loc = Locator {
        chapter: 2,
        offset: 5,
    };
    let byte = loc.byte_of(SRC);
    assert_eq!(SRC[..byte].chars().count(), 5);
    assert_eq!(Locator::from_byte(2, SRC, byte), loc);
}

#[test]
fn from_byte_snaps_back_off_a_multibyte_boundary() {
    // 「中」占 3 个字节：byte 1、2 都落在它内部，只能退回 0。
    assert_eq!(Locator::from_byte(0, SRC, 1).offset, 0);
    assert_eq!(Locator::from_byte(0, SRC, 2).offset, 0);
    assert_eq!(Locator::from_byte(0, SRC, 3).offset, 1);
}

#[test]
fn emoji_counts_as_one_char_but_four_bytes() {
    let emoji_at = Locator {
        chapter: 0,
        offset: 9,
    };
    let byte = emoji_at.byte_of(SRC);
    assert_eq!(SRC[byte..].chars().next().unwrap(), '😀');
    assert_eq!(SRC[byte..].chars().next().unwrap().len_utf8(), 4);
    assert_eq!(Locator::from_byte(0, SRC, byte + 1).offset, 9);
}

#[test]
fn out_of_range_offsets_clamp() {
    assert_eq!(
        Locator::from_byte(0, "abc", 99),
        Locator {
            chapter: 0,
            offset: 3
        }
    );
    assert_eq!(
        Locator {
            chapter: 9,
            offset: 99
        }
        .clamp_to_chapter(1, "abc"),
        Locator {
            chapter: 1,
            offset: 3
        }
    );
}
