//! 功能纵切：每个功能自带 view / state / 小组件。
//!
//! 这里**一个 `cfg(mobile)` 都没有**：平台差异只允许出现在 `super::host`（平台服务）与
//! `super::platform`（入口与原生桥）。阅读界面以前是第三处（桌面一套渲染、移动一套），
//! 现在两端共用逐字形那一份，见 [`reader`]。

pub mod notes;
pub mod reader;
pub mod settings;
pub mod shelf;
pub mod stats;
pub mod welcome;
