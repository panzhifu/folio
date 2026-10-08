//! Folio 的 gpui-kit 侧：应用壳、阅读视图与排版引擎的绘制/度量实现。
//!
//! 作为库暴露模块，是为了让 `tests/` 里的 UI 集成测试能直接用生产视图，
//! 而不是在测试里另搭一套近似视图。

pub mod app;
pub mod prefs_model;
pub mod reader;
pub mod settings_view;
