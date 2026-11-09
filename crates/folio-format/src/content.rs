//! 内容两族（可重排 / 定版式）的**类型**住进了 folio-model（`folio_model::content`）。
//!
//! 这里沿用旧路径：`folio_format::content::X` 与根上的 `folio_format::X` 照旧可用，
//! 调用方不需要知道类型搬了家。与「手上有没有正文」有关的两条方法（字数合计回退
//! 现算、页文字换锚点坐标表）是行为，住在 [`crate::ext`] 的扩展 trait 里。

pub use folio_model::content::{
    Content, ContentKind, CoverRef, FixedDoc, FixedPage, FlowDoc, PageChar, PageImage, PageRect,
    PageRegion, PageSource, PageText,
};
