//! 内容、导入与服务编排，不依赖 GPUI；机制 crate 经重导出保持公共路径稳定。
//! `content` 可依赖 `infra`，不能依赖 `store`/`ingest`；`infra` 不依赖业务分区。
//! [`Error`] 保留底层因果链，应用层负责展示。

// 文案编译期嵌入；fallback 与 GPUI Kit 组件层一致。
rust_i18n::i18n!("locales", fallback = "en");

pub mod backup;
pub mod cache;
pub mod content;
pub mod error;
pub mod infra;
pub mod ingest;
pub mod net;
pub mod reader;
pub mod services;
pub mod sync;

pub use folio_render as layout;

pub use folio_speech as speech;

pub use folio_translate::dict;

pub use folio_fonts::{fonts, text};

pub use folio_store as store;

pub use folio_account as account;

pub use folio_search as search;

pub use content::anchor::{
    Confidence, Relocated, TextAnchor, TextAnchorExt, TextMap, line_at_offset, line_at_y,
    line_chars, line_offset, relocate,
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

pub use folio_model::position;

pub use error::{Error, ImportReject, Result};
// 解析行为由扩展 trait 提供，folio-model 只保留公共数据。
pub use folio_format::ext::{ChapterExt, FlowDocExt, FormatExt, PageTextExt};
use std::sync::atomic::{AtomicU64, Ordering};

/// 封面补齐后递增，使书架中缓存的“无封面”结果失效。
/// 只作变更标记，不发布其它内存数据，因此使用 Relaxed。
static COVER_EPOCH: AtomicU64 = AtomicU64::new(0);

pub fn cover_epoch() -> u64 {
    COVER_EPOCH.load(Ordering::Relaxed)
}

pub(crate) fn bump_cover_epoch() {
    COVER_EPOCH.fetch_add(1, Ordering::Relaxed);
}

pub use folio_model::Tag;
pub use folio_model::group::{GroupBy, ShelfGroup, UNGROUPED, UNKNOWN_AUTHOR, group_shelf};

// BookSettings 的配置合并行为留在 core，序列化数据住在 model。
pub use infra::book_settings::{APPEARANCE_GLOBAL_ONLY, BookSettings, BookSettingsExt};
pub use infra::budget::{EvictStep, MemoryBudget, Pressure};
pub use infra::config::{
    Account, Ai, AliyunDrive, Appearance, Backup, BaiduDrive, CONFIG_VERSION, Cloud, Config,
    Dictionary, DictionaryEntry, Env, FontEntry, Fonts, IGNORE_CONFIG_ENV, ImageScale, LOG_OFF_ENV,
    Language, Layer, Library, Logging, Membership, MembershipTier, Mode, Novel, Opds, OpdsCatalog,
    PageTurn, Provenance, Reader, Resolved, Speech, SpeechRole, Stealth, StealthMode, Sync,
    SyncProvider, TextDirection, TocDensity, Translation, TranslationEngine, Vendor, WebDavDrive,
    host_of, new_vendor_id, resolve_vendor,
};
pub use infra::i18n::Locale;
pub use infra::paths::{
    AppDirs, app_dirs, backups_dir, cache_dir, config_dir, config_path, data_dir, db_path,
    dicts_dir, fonts_dir, log_dir, themes_dir,
};
pub use infra::task::{
    ByteProgress, Cancel, Priority, StageCtx, TaskError, TaskEvent, TaskHandle, TaskId, TaskManager,
};
pub use infra::time::{
    CivilDate, DurationParts, civil_date, civil_from_days, day_start, days_from_civil,
    days_in_month, duration_parts, local_day, local_hour, weekday_monday0,
};
pub use ingest::import::{
    BackfillReport, COVER_BACKFILL_VERSION, Candidate, CloudItem, CloudSource, CoverBackfillReport,
    ImportKind, ImportOutcome, ImportReport, Imported, Origin, PathsSource, Registered, Source,
    UrlSource, backfill_copies, backfill_covers, forward, import_from, reindex, schedule_reindex,
    submit_import, sweep_orphan_copies, sweep_partials,
};
pub use ingest::scan::{ScanFilter, ScanReport, ScanSource, find, format_of};
pub use position::{LOCATOR_FORMAT_VERSION, Locator};
pub use services::Services;
pub use speech::{Command, Engine, Event};
pub use store::StaleReason;
pub use store::{
    ANNOTATION_COLORS, Annotation, AnnotationStyle, BookReading, Collection, DailyReading,
    IMPORT_LOG_KEEP, ImportLanding, ImportLogEntry, ReadingRank, ReadingSession, ReadingSummary,
    ReadingTotals, ShelfItem, Store, UploadBook,
};
