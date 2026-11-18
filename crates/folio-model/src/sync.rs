//! 同步的 DTO：快照与各记录的 **serde 形状**（原 folio-core::sync 的数据半边，
//! 2026-11-12 阶段二下沉——「换 crate 不换形状」：core 经模块别名沿用旧路径）。
//!
//! 这里只有**数据**与纯操作：[`Snapshot::shift_clock`] 只调整导出时间，记录版本保持原样。
//! 合并算法（`merge`）、快照的 JSON 编解码、编排（`sync_once`）
//! 都留在 folio-core 的 `sync` —— 它们是行为，且 `merge` 的墓碑优先级、`sync_once`
//! 的远端握手各有各的测试盯着。
//!
//! ## serde 表示是契约
//!
//! 快照 JSON 要跨设备、跨版本地来回走：字段名与 `skip_serializing_if` 的省略规则
//! 一格都不许随手改 —— 空字段不写进 JSON，绝大多数「只读过几页」的书只有一条
//! position，对面**旧版本**的设备也要读得回来。

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BTreeMap;

use super::anchor::TextAnchor;
use super::position::Locator;
use super::reading::BookSettings;

/// 快照结构版本。记录形状一改就要加一 —— 读到版本对不上的快照时，core 那侧的
/// `snapshot_from_json` 会拒绝，而不是把新字段静默丢掉。
pub const SNAPSHOT_VERSION: u32 = 1;

/// 局域网清单里**一条可拉的书**：`GET /manifest` 响应里 `books` 的元素。
///
/// 只带 `content_hash`、大小与书名。`hash` 就是请求 `/file` 用的键，路径由对端按自己的
/// 数据库解 —— 书库目录在哪儿、副本叫什么名，都是各台自己的事，不该跨机说。
///
/// 数据形状归同步而不是 `net::lansync`：读侧要它的是 `store` 的「本机有哪些可同步的书」
/// （`Store::collect_sync` 那一族），而 store 不许认识网络模块 —— DTO 住在两边都依赖的
/// 这一层，协议侧 re-export 沿旧路径。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookEntry {
    /// `books.content_hash` 那一格：64 个小写十六进制字符。
    pub hash: String,
    pub size: u64,
    /// 只用于日志与报表，让「这次拉了 3 本」说得出具体的书。
    pub title: String,
}

/// 一台设备的全量快照。书以 `content_hash` 为键 —— 本地 id 跨设备没有意义。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub device_id: String,
    pub exported_at: i64,
    #[serde(default)]
    pub books: BTreeMap<String, BookSnapshot>,
}

impl Snapshot {
    pub fn new(device_id: impl Into<String>, exported_at: i64) -> Self {
        Self {
            version: SNAPSHOT_VERSION,
            device_id: device_id.into(),
            exported_at,
            books: BTreeMap::new(),
        }
    }

    /// 对齐快照导出时间；记录版本保持原样，避免转发与重试改变冲突顺序。
    pub fn shift_clock(&mut self, delta: i64) {
        self.exported_at = self.exported_at.saturating_add(delta);
    }
}

/// 一本书在快照里的全部内容。空字段不写进 JSON（`skip_serializing_if`），
/// 于是绝大多数「只读过几页」的书只有一条 position。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BookSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<PositionRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<AnnotationRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bookmarks: Vec<BookmarkRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tombstones: Vec<Tombstone>,
    /// 这一本的阅读设置差量。`None` = 这台设备从来没为它写过差量（没话说），
    /// `Some(空差量)` = 写过并且清干净了（说的就是「跟默认」）—— 两件事必须分得开，
    /// 否则「恢复本书默认」同步过去会变成「另一台设备上那份旧差量又回来了」。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<SettingsRecord>,
}

/// 差量连同「这台设备什么时候写的」。合并按整本差量比这一个时间取新的那一份 ——
/// 差量本身没有逐格时间，逐格合并要先给每一格配一个钟，那一层这次不做。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingsRecord {
    pub updated_at: i64,
    pub settings: BookSettings,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionRecord {
    pub mode: String,
    pub locator: Locator,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnnotationRecord {
    pub uid: String,
    pub anchor: TextAnchor,
    pub note: String,
    pub color: String,
    /// `AnnotationStyle::as_str()` 那一套键名。不在这里存枚举，远端写坏一格时
    /// 合并仍能把它交给应用层按默认档兜住。
    pub style: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BookmarkRecord {
    pub uid: String,
    pub anchor: TextAnchor,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 同一条记录的确定性全序；不依赖接收方或快照转发设备。
pub trait SyncOrder {
    fn sync_cmp(&self, other: &Self) -> Ordering;
}

fn anchor_key(anchor: &TextAnchor) -> (u32, usize, usize, &str, &str, &str, &str) {
    (
        anchor.chapter,
        anchor.start,
        anchor.end,
        &anchor.exact,
        &anchor.prefix,
        &anchor.suffix,
        &anchor.path,
    )
}

impl SyncOrder for PositionRecord {
    fn sync_cmp(&self, other: &Self) -> Ordering {
        (
            self.updated_at,
            &self.mode,
            self.locator.chapter,
            self.locator.offset,
        )
            .cmp(&(
                other.updated_at,
                &other.mode,
                other.locator.chapter,
                other.locator.offset,
            ))
    }
}

impl SyncOrder for SettingsRecord {
    fn sync_cmp(&self, other: &Self) -> Ordering {
        self.updated_at
            .cmp(&other.updated_at)
            .then_with(|| self.settings.to_column().cmp(&other.settings.to_column()))
    }
}

impl SyncOrder for AnnotationRecord {
    fn sync_cmp(&self, other: &Self) -> Ordering {
        (
            self.updated_at,
            self.created_at,
            anchor_key(&self.anchor),
            &self.note,
            &self.color,
            &self.style,
        )
            .cmp(&(
                other.updated_at,
                other.created_at,
                anchor_key(&other.anchor),
                &other.note,
                &other.color,
                &other.style,
            ))
    }
}

impl SyncOrder for BookmarkRecord {
    fn sync_cmp(&self, other: &Self) -> Ordering {
        (self.updated_at, self.created_at, anchor_key(&self.anchor)).cmp(&(
            other.updated_at,
            other.created_at,
            anchor_key(&other.anchor),
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TombstoneKind {
    Annotation,
    Bookmark,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tombstone {
    pub kind: TombstoneKind,
    pub uid: String,
    pub deleted_at: i64,
}

/// 本机快照 + 因为还没有 `content_hash` 而没进来的书数。
///
/// 阶段二落地时它留在 core（消费方只有 sync 与 store 两处）；阶段四 store 独立成
/// crate 后它是第三个消费方，按 folio-model 的规矩进来。不是 serde：它是**内存里**
/// 的收集结果，上不网。
#[derive(Debug, Clone, PartialEq)]
pub struct LocalSnapshot {
    pub snapshot: Snapshot,
    pub skipped_books: usize,
}

/// 合并之后要写回本机的动作。按书哈希定位本地那一行；远端书不在本机时不会进这里。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub positions: Vec<(String, PositionRecord)>,
    pub annotation_upserts: Vec<(String, AnnotationRecord)>,
    pub annotation_deletes: Vec<DeleteRecord>,
    pub bookmark_upserts: Vec<(String, BookmarkRecord)>,
    pub bookmark_deletes: Vec<DeleteRecord>,
    /// 该写回本机的差量（整本一份，按 `content_hash` 定位那一行）。
    pub settings: Vec<(String, SettingsRecord)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteRecord {
    pub book_hash: String,
    pub uid: String,
    pub deleted_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    pub positions: usize,
    pub annotations: usize,
    pub bookmarks: usize,
    pub annotations_deleted: usize,
    pub bookmarks_deleted: usize,
    /// 写回本机的那一本书的差量（整本算一次，不逐格数）。
    pub settings: usize,
}

impl Stats {
    pub fn applied(&self) -> usize {
        self.positions + self.annotations + self.bookmarks + self.settings
    }
}

/// 一次合并的结果：该上传的自己那份、该写回本机的动作、给界面看的计数。
#[derive(Debug, Clone, PartialEq)]
pub struct Merged {
    pub snapshot: Snapshot,
    pub plan: Plan,
    pub stats: Stats,
}
