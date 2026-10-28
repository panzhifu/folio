//! Folio 的书本模型、格式解析、标注锚点与存储层。
//!
//! 这里不放任何 UI 代码，也不引入 `gpui`。
//!
//! 错误一律是 [`Error`]（`thiserror` 的枚举，每个变体自带上下文、底层原因留在
//! [`Error::source`] 上）；界面侧怎么把它们收成一条因果链，见
//! `folio_app::ui::shared::error`。

// 界面文案表。`locales/` 下一门语言一份 TOML（文件名去掉扩展名就是语言标签），编译期嵌进来，
// 取文案走 `i18n::t` / `i18n::t_with`；
// `fallback = "en"` 与 gpui-kit 组件层一致，这样缺一条日文文案时框架和我们回落到同一种语言。
rust_i18n::i18n!("locales", fallback = "en");

// ── 四个区。分区的判据是「谁依赖谁」，写在各自的 mod.rs 里 ──
//
//     content/   内容与文档：DOM / CSS / 排版 / 文档接口 / 锚点 / 命中 / 批注
//     layout/    排版引擎（行盒、分片、失效键与两级缓存）
//     store/     SQLite：书目、进度、批注、集合、统计
//     ingest/    书怎么进来：导入管线、扫描、监听、身份、校验
//     net/       出网与入网：WebDAV、Wi-Fi 传书
//     infra/     横切：配置、日志、路径、错误、作业、服务、量具、时间
//
// 依赖方向是单向的：`content` 可以依赖 `infra`，但不依赖 `store` / `ingest`；
// `ingest` 同时碰容器、文件系统与数据库（那是它的职责）；`infra` 谁都不依赖。
// 这条方向是「引擎能在没有数据库的情况下跑」与「core 能被 cargo test 全量覆盖」的前提。
pub mod content;
pub mod group;
pub mod infra;
pub mod ingest;
pub mod layout;
pub mod net;
pub mod position;
pub mod search;
pub mod speech;
pub mod store;
pub mod sync;

pub use content::anchor::{
    Confidence, Relocated, TextAnchor, TextMap, line_at_offset, line_at_y, line_chars, line_offset,
    relocate,
};
pub use content::annotation::{
    AnnotationExportFormat, ResolvedAnnotation, annotations_by_chapter, annotations_export,
    annotations_markdown, resolve_annotations,
};
pub use content::document::{BookDocument, ContentRef, Document, LocatorKind};
pub use content::format::{
    Chapter, ChapterLocator, ChapterText, Content, ContentKind, DocMeta, DocumentProvider,
    FixedDoc, FixedPage, FlowDoc, Format, PageSource, Series,
};
pub use group::{GroupBy, ShelfGroup, UNGROUPED, UNKNOWN_AUTHOR, group_shelf};
pub use infra::book_settings::{APPEARANCE_GLOBAL_ONLY, BookSettings};
pub use infra::budget::{EvictStep, MemoryBudget, Pressure};
pub use infra::config::{
    Appearance, CONFIG_VERSION, Config, Env, IGNORE_CONFIG_ENV, LOG_OFF_ENV, Language, Layer,
    Library, Logging, Mode, Opds, OpdsCatalog, PageTurn, Provenance, Reader, Resolved, Speech,
    Sync, TextDirection, TocDensity, Translation, TranslationEngine,
};
pub use infra::error::{Error, ImportReject, Result};
pub use infra::i18n::Locale;
pub use infra::paths::{config_dir, config_path, data_dir, db_path, log_dir, themes_dir};
pub use infra::services::Services;
pub use infra::task::{
    ByteProgress, Cancel, Priority, StageCtx, TaskError, TaskEvent, TaskId, TaskHandle, TaskManager,
};
pub use infra::time::{
    CivilDate, DurationParts, civil_date, civil_from_days, day_start, days_from_civil,
    days_in_month, duration_parts, local_day, local_hour, weekday_monday0,
};
pub use ingest::import::{
    BackfillReport, Candidate, ImportKind, ImportOutcome, ImportReport, Imported, Origin,
    PathsSource, Registered, Source, UrlSource, backfill_copies, forward, import_from, reindex,
    schedule_reindex, submit_import, sweep_partials,
};
pub use ingest::scan::{ScanReport, ScanSource, find};
pub use position::{LOCATOR_FORMAT_VERSION, Locator};
pub use speech::{Command, Engine, Event};
pub use store::StaleReason;
pub use store::{
    ANNOTATION_COLORS, Annotation, AnnotationStyle, BookReading, Collection, DailyReading,
    ReadingRank, ReadingSession, ReadingSummary, ReadingTotals, ShelfItem, Store,
};
