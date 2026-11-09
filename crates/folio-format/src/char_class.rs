//! 字符分类住进了 folio-model（`folio_model::char_class`）。
//!
//! 这里沿用旧路径。分类表只有一份的立场没变：定版式那一族（PDF / DjVu 的文字层）
//! 与可重排那一侧收的是同一个边，判据在 [`folio_model::char_class::CharClass`]。

pub use folio_model::char_class::CharClass;
