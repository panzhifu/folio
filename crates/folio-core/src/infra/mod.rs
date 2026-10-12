//! 横切基础设施：不解决任何业务问题，但每一条业务线都要用。
//!
//! - [`config`] —— 三层来源（内置默认 < 用户文件 < 环境变量）字段级合并 + provenance。
//! - [`logging`] —— `tracing` subscriber 的装配：文件 / stderr / 宿主三处出口。
//! - [`paths`] —— **唯一的路径来源**：数据根、库文件、日志目录、封面与缩略图目录。
//!   别处不许拼路径（`plans/基础设施.md` §2.7）。
//! - [`error`] —— 后端的错误枚举（`thiserror`，每个变体自带上下文、底层原因留在
//!   `source` 链上）。
//! - [`task`] —— 后台作业管线：可取消、可报进度、四档优先级。
//! - [`services`] —— 常驻服务的生命周期与关闭次序（服务 → 作业池 → 库连接）。
//! - [`budget`] —— 内存预算与降级顺序。
//! - [`metrics`] —— 三个计数器的量具（测试断言用计数而不是计时）。
//! - [`diagnostics`] —— 诊断 dump（`FOLIO_DUMP`）。
//! - [`i18n`] —— 文案表的语言标签与占位符校验（表在 crate 根的 `locales/`）。
//! - [`time`] —— 日历换算：Unix 秒 ↔ 本地日 ↔ `YYYY-MM-DD`。**其余地方不许自己除 86400。**
//!
//! 这一区**不依赖 [`crate::store`]**（[`services`] 收的是调用方给的句柄），
//! 也不依赖 [`crate::layout`]：它服务所有人，所以谁都不许反过来要求它懂业务。

pub mod budget;
pub mod config;
pub mod diagnostics;
pub mod error;
pub mod i18n;
pub mod logging;
pub mod metrics;
pub mod paths;
pub mod services;
pub mod task;
pub mod time;
