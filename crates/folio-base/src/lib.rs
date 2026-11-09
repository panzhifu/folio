//! Folio 的地基：不解决任何业务问题，但每条业务线都要用。
//!
//! 边界由依赖图强制：这里不认识书本、格式、SQLite、网络与界面，也不依赖
//! `folio-format` / `folio-core`。往这里加东西之前先问一句：它与「一本书」有没有关系？
//!
//! - [`budget`] —— 内存预算与降级顺序。
//! - [`cancel`] —— 协作式取消标志（作业与网络共用）。
//! - [`error`] —— 地层错误：读盘与日志装配。
//! - [`identity`] —— 内容寻址：字节 → SHA-256 身份 → 书库副本文件名。
//! - [`logging`] —— `tracing` subscriber 的装配、轮转与日志配置。
//! - [`paths`] —— **唯一的路径来源**：配置根、数据根、书库各区。
//! - [`task`] —— 后台作业管线：可取消、可报进度、四档优先级。
//! - [`time`] —— 日历换算：Unix 秒 ↔ 本地日 ↔ `YYYY-MM-DD`。
//! - [`usage`] —— 磁盘占用按去处分类数一遍。

pub mod budget;
pub mod cancel;
pub mod error;
pub mod identity;
pub mod logging;
pub mod paths;
pub mod task;
pub mod time;
pub mod usage;
