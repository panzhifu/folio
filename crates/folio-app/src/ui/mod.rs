//! 界面层。
//!
//! 分层（判据只有一条：**这段代码需不需要和平台不同**）：
//!
//! - [`shared`] —— 领域与地基：配置、文案、主题、排版状态、图片、书库、度量。
//! - [`widgets`] —— 无状态小件（发丝线、次要文字、空状态）。
//! - [`features`] —— 按功能纵切：每个功能自带 view / state / 小组件。
//! - [`shell`] —— 外壳：自适应导航（Material 3）+ 每个目的地一个导航栈。
//! - [`host`] —— 平台服务边界（安全区、输入法、正文字体）。
//! - [`platform`] —— 入口与原生桥。
//!
//! `cfg(mobile)` **只允许出现在 [`host`] 与 [`platform`]**。阅读界面以前是第三个：桌面一套
//! 渲染、移动一套，现在两端共用逐字形那一份，平台之间只剩「导航栏占的是宽还是高」这种
//! 数据差别，它走 [`Reserve`](features::reader::Reserve) 传进去。
//!
//! 两端用的是同一个 `gpui-pre` 快照（`gpui_kit` 顶层 `pub use ::gpui::*` 把它整个转出），
//! 所以视图统一 `use gpui_kit::…`，不直接引 `gpui`。

pub mod shared;

pub mod features;
pub mod shell;
pub mod widgets;

pub mod host;
pub mod platform;
