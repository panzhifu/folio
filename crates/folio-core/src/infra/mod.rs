//! core 的横切模块。地基已经搬进 `folio-base`，这里只留两类：
//!
//! - **配置与它带出来的一层**：[`config`]（三层来源字段级合并 + provenance）、
//!   [`book_settings`]（每本书的阅读设置差量）、[`logging`]（`init_from_config` 与
//!   反馈快照的薄壳）、[`diagnostics`]（诊断 dump）。
//! - **业务侧的横切**：[`error`]（后端的错误词汇表）、[`i18n`]（文案表与 `Locale`）、
//!   [`backgrounds`]（正文主题背景图的落盘）、[`reading_theme`]（外观 → 主题名解析）、
//!   [`services`]（常驻服务的组合根）、[`metrics`]（格式层的计数型量具，来自 folio-format）。
//!
//! 下面这几条是 re-export，实现在 `folio-base`：`crate::infra::time::…` 这类旧写法不改。
//! 判据是它们不认识任何业务类型。`config` 之所以没跟着下去，是因为它的 schema 引了
//! `store::AnnotationStyle`、`format::novel::Edits` 与 `account::AccountKey` —— 那属于
//! 第二步 `settings` 层，不在这一次的刀口上。

pub mod backgrounds;
pub mod book_settings;
pub mod config;
pub mod diagnostics;
pub mod error;
pub mod i18n;
pub mod logging;
pub use folio_format::metrics;
pub mod reading_theme;
pub mod services;

pub use folio_base::{budget, identity, paths, task, time, usage};
