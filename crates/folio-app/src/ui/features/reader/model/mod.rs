//! 阅读会话的状态与状态迁移。
//!
//! 这一层**不建元素树**：不返回 `AnyElement`、不写 `impl Render`、不摸 `Window`。
//! 可以有 `Context<Self>`（要 `notify`、要落库），这里放的是「一次动作把哪些数改成什么」。
//!
//! 两条例外：`measure` 因为排版要量字而碰 `TextSystem`；
//! `session` 会持 `Entity<NoteEditor>` 这类组件句柄（只持有、不 `render`）。

pub(in crate::ui::features::reader) mod ai;
pub(in crate::ui::features::reader) mod annotations;
pub(in crate::ui::features::reader) mod audio;
#[cfg(not(any(target_os = "android", target_os = "ios", target_env = "ohos")))]
pub(in crate::ui::features::reader) mod audio_desktop;
pub(in crate::ui::features::reader) mod book;
pub(in crate::ui::features::reader) mod book_fonts;
pub(in crate::ui::features::reader) mod bookmarks;
pub(in crate::ui::features::reader) mod chapter_layout;
pub(in crate::ui::features::reader) mod chrome;
pub(in crate::ui::features::reader) mod content_update;
pub(in crate::ui::features::reader) mod dictionary;
pub(in crate::ui::features::reader) mod fixed_book;
pub(in crate::ui::features::reader) mod fixed_zoom;
pub(in crate::ui::features::reader) mod footnotes;
pub(in crate::ui::features::reader) mod image_atlas;
pub(in crate::ui::features::reader) mod links;
pub(in crate::ui::features::reader) mod measure;
pub(in crate::ui::features::reader) mod memory;
pub(in crate::ui::features::reader) mod page_geometry;
pub(in crate::ui::features::reader) mod page_types;
pub(in crate::ui::features::reader) mod paging;
pub(in crate::ui::features::reader) mod perf;
pub(in crate::ui::features::reader) mod position;
pub(in crate::ui::features::reader) mod prefetch;
pub(in crate::ui::features::reader) mod reading_time;
pub(in crate::ui::features::reader) mod rules_draft;
pub(in crate::ui::features::reader) mod session;
pub(in crate::ui::features::reader) mod speech;
pub(in crate::ui::features::reader) mod speech_remote;
pub(in crate::ui::features::reader) mod speech_routing;
pub(in crate::ui::features::reader) mod swipe;
pub(in crate::ui::features::reader) mod toc;
pub(in crate::ui::features::reader) mod toc_tree;
pub(in crate::ui::features::reader) mod translation;
pub(in crate::ui::features::reader) mod turn;
pub(in crate::ui::features::reader) mod undo;

pub use session::ReaderSession;
