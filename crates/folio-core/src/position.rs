use serde::{Deserialize, Serialize};

/// 阅读位置。**存字符数而不是页码或行号**：字号、行距、窗口宽度、
/// 分页还是连续滚动，都会让页码失效，但一章里的第 N 个字不会变。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Locator {
    pub chapter: u32,
    pub offset: usize,
}

impl Locator {
    pub fn start() -> Self {
        Self {
            chapter: 0,
            offset: 0,
        }
    }

    /// 字符数 → 字节索引，越界时夹到文本末尾。
    pub fn byte_of(&self, source: &str) -> usize {
        source
            .char_indices()
            .nth(self.offset)
            .map(|(b, _)| b)
            .unwrap_or(source.len())
    }

    /// 字节索引 → Locator，并把不在字符边界上的值往前挪，
    /// 否则后续 `&source[range]` 会 panic（中文与 emoji 一定会踩到）。
    pub fn from_byte(chapter: u32, source: &str, byte: usize) -> Self {
        let mut byte = byte.min(source.len());
        while byte > 0 && !source.is_char_boundary(byte) {
            byte -= 1;
        }
        Self {
            chapter,
            offset: source[..byte].chars().count(),
        }
    }

    pub fn clamp_to_chapter(&self, chapter: u32, source: &str) -> Locator {
        let max = source.chars().count();
        Self {
            chapter: self.chapter.min(chapter),
            offset: self.offset.min(max),
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| r#"{"chapter":0,"offset":0}"#.into())
    }

    pub fn from_json(json: &str) -> Locator {
        serde_json::from_str(json).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    //! 阅读位置：字符偏移 ↔ 字节索引的互转与越界钳制。只依赖公开 API。

    use crate::Locator;

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
}
