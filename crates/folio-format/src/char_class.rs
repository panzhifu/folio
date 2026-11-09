//! 词 / 空白 / 换行 / 其他：划词吸附用的字符分类。
//!
//! 只有一份：定版式那一族（PDF / DjVu 的文字层）与可重排那一侧必须收同一个边，
//! 两处各写一遍，就会有一处的双击在中文里选中一整句、另一处只选一个字。

/// 与 gpui-kit `text/selection.rs` 同一套分类（词 / 空白 / 换行 / 其他）。
///
/// `pub` 的理由是**只有一份**：定版式那一族（PDF / DjVu 的文字层，
/// [`crate::PageText::snap`]）要收的是同一个边 —— 中文与标点单字自为界、
/// 拉丁连成词。两处各写一遍分类表，就会有一处的双击在中文里选中一整句、另一处只选一个字。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharClass {
    Word,
    Whitespace,
    Newline,
    Other,
}

impl CharClass {
    pub fn of(ch: char) -> Self {
        if is_word_char(ch) {
            Self::Word
        } else if ch == '\n' || ch == '\r' {
            Self::Newline
        } else if ch.is_whitespace() {
            Self::Whitespace
        } else {
            Self::Other
        }
    }

    /// 同类字符连成一段。换行与 CJK/标点不连。
    pub fn is_connectable(self) -> bool {
        matches!(self, Self::Word | Self::Whitespace)
    }
}

/// 词字符的集合。抄自 gpui-kit 的 `is_word_char`：ASCII 字母数字与下划线之外，
/// 把拉丁扩展（法德西等）、西里尔、越南文与组合记号也算进来 ——
/// 这几种文种的词同样靠空格分界，长按应该吸出整个词。
fn is_word_char(c: char) -> bool {
    matches!(c, '_')
        || c.is_ascii_alphanumeric()
        || matches!(c, '\u{00C0}'..='\u{00FF}')
        || matches!(c, '\u{0100}'..='\u{017F}')
        || matches!(c, '\u{0180}'..='\u{024F}')
        || matches!(c, '\u{0400}'..='\u{04FF}')
        || matches!(c, '\u{1E00}'..='\u{1EFF}')
        || matches!(c, '\u{0300}'..='\u{036F}')
}
