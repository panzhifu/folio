//! 两端共用的界面地基：配置、文案、主题、图片索引、书库管理、错误约定。
//!
//! 这一层的存在理由只有一条：**同一本书在桌面和手机上必须排出一样的行**。
//! 断行累加用的是单字 advance、行盒宽度用的是整段 shaping —— 这两处只要有一端自己
//! 重写一遍，两端就会出现「桌面居中标题正、手机上歪出版心」这种查起来极慢的差别。
//!
//! 所以这里的每个模块都不带 `cfg` 分支：它依赖的 `gpui_kit` 与 `folio_core`
//! 在两个 target 上是同一份实现（`gpui_kit` 顶层 `pub use ::gpui::*` 转出的就是
//! 移动端那份 `gpui-pre`）。平台差异一律留给 `super::host` 与 `super::platform`。
//!
//! 只属于阅读器的排版链（`document` / `measure` / `book_fonts`）住在
//! `features/reader/` —— 书架拿的只有 `images::cover_image` 一个入口：导入的解析在
//! 后端（`folio_core::import`），真正的排版状态没有理由对别的屏可见。

pub mod about;
pub mod app_store;
pub mod config_model;
pub mod error;
pub mod import_queue;
pub mod transfer;
pub mod watch;
pub mod images;
pub mod l10n;
pub mod library;
pub mod theme;
