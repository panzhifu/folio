//! 内容与文档：一本书读进来之后，「读者看到什么」这件事的全套。
//!
//! 这一区回答的是**内容**问题，不是**存储**问题（那是 [`crate::store`]），
//! 也不是**书怎么进来**（那是 [`crate::ingest`]）：
//!
//! - [`document`] —— 统一文档接口：`Document` trait（可重排 / 定版式）+ 门面。
//!   已随 2026-11-12 阶段五收尾下沉 `folio-format`（它的错误面 `ChapterUnavailable` /
//!   `UnsupportedFormat` 与 `CoverRef` 本就是那边的词表，且书内搜索与字体装载要站在
//!   它上面）；这里重导出，`content::document::…` 的路径不变。
//! - [`format`] —— 已独立成 `folio-format`：provider 注册表、各格式解析与容器层；
//!   这里只是重导出，`content::format::…` 的路径不变。
//! - [`assets`] —— 书库资产：封面与缩略图的落盘约定（不是格式解析）。
//! - [`dom`] —— html5ever 的 arena DOM（来自 `folio-format`）。
//! - [`scan`] —— 一趟 tokenizer 出章摘要（来自 `folio-format`）。
//! - [`anchor`] —— 可见文本坐标系（`TextMap` / `TextAnchor`）与四级降级重定位。
//! - [`hit`] —— 引擎查询接口：命中、选区矩形、词边界吸附。
//! - [`annotation`] —— 批注读侧：批量解算、按章分组、Markdown 导出。
//! - [`validate`] —— 解析产物的结构完整性检查（在 `folio-format`，判据全是 Content
//!   的形状；随 [`document`] 一并下沉）。这里重导出，路径不变。
//!
//! **分区的判据是「谁依赖谁」**：这一区可以依赖 [`crate::infra`]，但不依赖
//! [`crate::store`] 与 [`crate::ingest`] —— 所以它能被 `cargo test` 全量覆盖，
//! 也是「排版可以在没有数据库的情况下跑」这条的落点。

pub mod anchor;
pub mod annotation;
// 书库资产的落盘约定已随 store 搬去 folio-store（2026-11-12 阶段四）——它本来就是
// 「封面写到哪儿、缩略图挂在哪」的书库规则；这里 re-export 沿旧路径（ingest 与 app）。
pub use folio_format as format;
pub use folio_format::document;
pub use folio_format::dom;
pub use folio_format::scan;
pub use folio_store::assets;
pub mod hit;
pub use folio_format::validate;
