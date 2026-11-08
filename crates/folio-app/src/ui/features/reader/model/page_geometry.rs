//! 阅读页屏幕几何的薄壳。实现在
//! [`folio_core::reader::geometry`](folio_core::reader::geometry)
//! （§3.6 下沉：纸、安全区、两条带、正文可排区在 core 离线可测）；
//! app 里 `model::page_geometry::PageRect` 等旧路径照旧可用。

pub use folio_core::reader::geometry::{BandPolicy, ChromeBands, PageBox, PageInsets, PageRect};
