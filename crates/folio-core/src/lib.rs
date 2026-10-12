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

pub mod anchor;
pub mod annotation;
pub mod budget;
pub mod config;
pub mod css;
pub mod diagnostics;
pub mod document;
pub mod dom;
pub mod error;
pub mod format;
pub mod group;
pub mod hit;
pub mod i18n;
pub mod identity;
pub mod import;
pub mod scan;
pub mod watch;
pub mod transfer;
pub mod layout;
pub mod logging;
pub mod metrics;
pub mod paths;
pub mod position;
pub mod remote;
pub mod services;
pub mod store;
pub mod task;
pub mod time;
pub mod validate;

pub use anchor::{
    Confidence, Relocated, TextAnchor, TextMap, line_at_offset, line_at_y, line_chars, line_offset,
    relocate,
};
pub use annotation::{
    ResolvedAnnotation, annotations_by_chapter, annotations_markdown, resolve_annotations,
};
pub use budget::{EvictStep, MemoryBudget, Pressure};
pub use config::{
    Appearance, CONFIG_VERSION, Config, Env, IGNORE_CONFIG_ENV, Language, Layer, Library, LOG_OFF_ENV,
    Logging, Mode, Provenance, Reader, Resolved, TocDensity,
};
pub use document::{BookDocument, ContentRef, Document, LocatorKind};
pub use error::{Error, ImportReject, Result};
pub use format::{
    Chapter, ChapterLocator, ChapterText, Content, ContentKind, DocMeta, DocumentProvider,
    FixedDoc, FixedPage, FlowDoc, Format, PageSource,
};
pub use group::{GroupBy, ShelfGroup, UNGROUPED, UNKNOWN_AUTHOR, group_shelf};
pub use i18n::Locale;
pub use scan::{ScanReport, ScanSource, find};
pub use import::{
    BackfillReport, Candidate, ImportKind, ImportOutcome, ImportReport, Imported, Origin,
    PathsSource, Registered, Source, UrlSource, backfill_copies, forward, import_from, reindex,
    submit_import, sweep_partials,
};
pub use services::Services;
pub use store::StaleReason;
pub use paths::{config_dir, config_path, data_dir, db_path, log_dir, themes_dir};
pub use position::{LOCATOR_FORMAT_VERSION, Locator};
pub use store::{
    ANNOTATION_COLORS, Annotation, BookReading, Collection, DailyReading, ReadingRank,
    ReadingSession, ReadingSummary, ReadingTotals, ShelfItem, Store,
};
pub use task::{ByteProgress, Cancel, Priority, StageCtx, TaskEvent, TaskId, TaskManager};
pub use time::{
    DurationParts, civil_date, day_start, duration_parts, local_day, local_hour,
};
