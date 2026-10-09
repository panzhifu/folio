//! 功能纵切：每个功能自带 view / state / 小组件。
//!
//! 平台差异只允许出现在 [`reader`]（两套渲染实现）与 `super::host`（平台服务）里；
//! 其余功能一律不写 `cfg(mobile)`。

pub mod notes;
pub mod reader;
pub mod settings;
pub mod shelf;
pub mod stats;
