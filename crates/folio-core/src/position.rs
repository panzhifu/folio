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
