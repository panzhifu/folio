//! 内容与文档：一本书读进来之后，「读者看到什么」这件事的全套。
//!
//! 这一区回答的是**内容**问题，不是**存储**问题（那是 [`crate::store`]），
//! 也不是**书怎么进来**（那是 [`crate::ingest`]）：
//!
//! - [`document`] —— 统一文档接口：`Document` trait（可重排 / 定版式）+ 门面。
//! - [`format`] —— provider 注册表与各格式的解析（EPUB / FB2 / HTML / Markdown / TXT / CBZ），
//!   以及容器层（zip / container / cover / thumb）。
//! - [`dom`] —— html5ever 的 arena DOM。
//! - [`anchor`] —— 可见文本坐标系（`TextMap` / `TextAnchor`）与四级降级重定位。
//! - [`hit`] —— 引擎查询接口：命中、选区矩形、词边界吸附。
//! - [`annotation`] —— 批注读侧：批量解算、按章分组、Markdown 导出。
//! - [`validate`] —— 解析产物的结构完整性检查：「读出来不对」只写日志，不阻塞阅读。
//!
//! **分区的判据是「谁依赖谁」**：这一区可以依赖 [`crate::infra`]，但不依赖
//! [`crate::store`] 与 [`crate::ingest`] —— 所以它能被 `cargo test` 全量覆盖，
//! 也是「排版可以在没有数据库的情况下跑」这条的落点。

pub mod anchor;
pub mod annotation;
pub mod document;
pub mod dom;
pub mod format;
pub mod hit;
pub mod scan;
pub mod validate;
