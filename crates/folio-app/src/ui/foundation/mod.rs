//! 地基：无业务、无 IO、无实体。
//!
//! 判据一条：**这一层不 import 上面任何一层**。主题、文案、错误约定、图片索引与颜色换算
//! 都在这里，它们只依赖 `gpui_kit` 与 `folio_core`，谁都能用，自己谁也不认识。
//!
//! 两端共用这件事的理由：**同一本书在桌面和手机上必须排出一样的行**。断行累加用的是单字
//! advance、行盒宽度用的是整段 shaping —— 这两处只要有一端自己重写一遍，两端就会出现
//! 「桌面居中标题正、手机上歪出版心」这种查起来极慢的差别。
//!
//! 所以这里的每个模块都不带 `cfg` 分支：它依赖的 `gpui_kit` 与 `folio_core`
//! 在两个 target 上是同一份实现（`gpui_kit` 顶层 `pub use ::gpui::*` 转出的就是
//! 移动端那份 `gpui-pre`）。平台差异一律留给 [`super::host`] 与 [`super::platform`]。
//!
//! 只属于阅读器的排版链（`chapter_layout` / `measure` / `book_fonts`）住在
//! `features/reader/` —— 书架拿的只有 `images::cover_image` 一个入口：导入的解析在
//! 后端（`folio_core::ingest::import`），真正的排版状态没有理由对别的屏可见。

pub mod error;
pub mod file_stem;
pub mod images;
pub mod style;
pub mod theme;
