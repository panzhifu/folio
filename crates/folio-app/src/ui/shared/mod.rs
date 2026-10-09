//! 两端共用的界面地基：配置、文案、主题、账号展示、图片索引、字体度量、章节排版状态、
//! 书库管理。
//!
//! 这一层的存在理由只有一条：**同一本书在桌面和手机上必须排出一样的行**。
//! 断行累加用的是单字 advance、行盒宽度用的是整段 shaping —— 这两处只要有一端自己
//! 重写一遍，两端就会出现「桌面居中标题正、手机上歪出版心」这种查起来极慢的差别。
//!
//! 所以这里的每个模块都不带 `cfg` 分支：它依赖的 `gpui_kit` 与 `folio_core`
//! 在两个 target 上是同一份实现（`gpui_kit` 顶层 `pub use ::gpui::*` 转出的就是
//! 移动端那份 `gpui-pre`）。平台差异一律留给 `super::host` 与 `super::platform`。

pub mod about;
pub mod account;
pub mod app_store;
pub mod config_model;
pub mod document;
pub mod images;
pub mod l10n;
pub mod library;
pub mod measure;
pub mod theme;
