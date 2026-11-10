//! Folio 的书本模型、标注锚点与存储层；文件格式解析在 `folio-format`。
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

// ── 分区。判据是「谁依赖谁」，写在各自的 mod.rs 里 ──
//
//     content/   内容与文档：文档接口 / 锚点 / 命中 / 批注（DOM 与 CSS 来自 folio-format）
//     store/     SQLite：书目、进度、批注、集合、统计
//     ingest/    书怎么进来：导入管线、扫描、监听、身份、校验
//     net/       出网与入网：网盘、OPDS、传书、同步（传输层已独立成 folio-net；
//                翻译/AI 的机制在 folio-translate / folio-ai，旧路径经 net/ 下的 shim 沿用）
//     infra/     横切：配置、日志、路径、错误、作业、服务、量具、时间
//
// 排版引擎已独立成 folio-render，听书引擎在 folio-speech，翻译与词典在
// folio-translate，字体域（随书装载 + 用户字体库）在 folio-fonts（旧路径
// `folio_core::layout` / `folio_core::speech` / `folio_core::dict` /
// `folio_core::text` / `folio_core::fonts` 经模块别名沿用）。
// 依赖方向是单向的：`content` 可以依赖 `infra`，但不依赖 `store` / `ingest`；
// `ingest` 同时碰容器、文件系统与数据库（那是它的职责）；`infra` 谁都不依赖。
// 这条方向是「引擎能在没有数据库的情况下跑」与「core 能被 cargo test 全量覆盖」的前提。
pub mod account;
pub mod backup;
pub mod cache;
pub mod content;
pub mod error;
pub mod group;
pub mod infra;
pub mod ingest;
pub mod net;
pub mod reader;
pub mod services;
pub mod store;
pub mod sync;

// 排版引擎已独立成 `folio-render` crate（「换 crate 不换形状」）。
// 旧路径沿用：`folio_core::layout::*` 与 crate 内的 `crate::layout::*`（content 的
// anchor/hit、reader 的几何、它们各自的测试）都解析到它，消费方一行不改。
pub use folio_render as layout;

// 听书引擎已独立成 `folio-speech` crate（2026-11-11，阶段五拆分）。旧路径沿用：
// `folio_core::speech::*`（settings 页的语义档注释、会话层的 `engine()` 调用）都
// 解析到它，消费方一行不改。
pub use folio_speech as speech;

// 翻译与词典已独立成 `folio-translate` crate（2026-11-11，阶段五拆分；翻译机制从
// folio-ai::translate、词典从本 crate 的 dict/ 原样搬去）。旧路径沿用：
// `folio_core::dict::*`（app 的词典服务与阅读器弹层）都解析到它，消费方一行不改。
pub use folio_translate::dict;

// 字体域已独立成 `folio-fonts` crate（2026-11-12，阶段五收尾；随书字体的装载管线
// 从本 crate 的 text/、用户字体库从 fonts.rs 原样搬去）。旧路径沿用：
// `folio_core::text::*`（app 的度量 / 系统字体 / 书内字体）与 `folio_core::fonts::*`
// （设置页的字体库、平台注册）都解析到它，消费方一行不改；错误词表跟着机制走
// （`FontsError` 自持，本 crate 的 `Error::Font` 变体删除）。
pub use folio_fonts::{fonts, text};

// 书内搜索已独立成 `folio-search` crate（2026-11-12，阶段五收尾；原 search.rs 原样搬去）。
// 旧路径沿用：`folio_core::search::*`（app 的搜索框、阅读会话与整架搜索）都解析到它，
// 消费方一行不改。它零错误词表：开不了书回 None、取不到章跳过，不 fail 整趟。
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
// 阅读位置与书库记录的**类型**住进了 folio-model（公共词汇表）；这里沿用旧路径，
// 生产消费点（`crate::position::Locator`、`super::types::{…}`）一行都不用改。
pub use folio_model::position;
// folio-model 类型上与解析侧绑定的行为（`Chapter::inline` / `char_len`、
// `copy_extension`、`PageText::text_map`）是 folio-format 的扩展 trait；core 与 app
// 从这里拿，不必直接依赖 folio-format。
pub use error::{Error, ImportReject, Result};
pub use folio_format::ext::{ChapterExt, FlowDocExt, FormatExt, PageTextExt};
pub use folio_model::Tag;
pub use group::{GroupBy, ShelfGroup, UNGROUPED, UNKNOWN_AUTHOR, group_shelf};
pub use infra::book_settings::{APPEARANCE_GLOBAL_ONLY, BookSettings};
pub use infra::budget::{EvictStep, MemoryBudget, Pressure};
pub use infra::config::{
    Account, Ai, AliyunDrive, Appearance, Backup, BaiduDrive, CONFIG_VERSION, Cloud, Config,
    Dictionary, DictionaryEntry, Env, FontEntry, Fonts, IGNORE_CONFIG_ENV, ImageScale, LOG_OFF_ENV,
    Language, Layer, Library, Logging, Membership, MembershipTier, Mode, Novel, Opds, OpdsCatalog,
    PageTurn, Provenance, Reader, Resolved, Speech, Sync, SyncProvider, TextDirection, TocDensity,
    Translation, TranslationEngine, WebDavDrive,
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
    BackfillReport, Candidate, CloudItem, CloudSource, ImportKind, ImportOutcome, ImportReport,
    Imported, Origin, PathsSource, Registered, Source, UrlSource, backfill_copies, forward,
    import_from, reindex, schedule_reindex, submit_import, sweep_orphan_copies, sweep_partials,
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
