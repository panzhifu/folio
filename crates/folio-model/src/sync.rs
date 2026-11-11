//! 同步的 DTO：快照与各记录的 **serde 形状**（原 folio-core::sync 的数据半边，
//! 2026-11-12 阶段二下沉——「换 crate 不换形状」：core 经模块别名沿用旧路径）。
//!
//! 这里只有**数据**与数据上纯函数的操作（[`Snapshot::shift_clock`]，把一份快照整体
//! 搬到另一个钟面上）：合并算法（`merge`）、快照的 JSON 编解码、编排（`sync_once`）
//! 都留在 folio-core 的 `sync` —— 它们是行为，且 `merge` 的墓碑优先级、`sync_once`
//! 的远端握手各有各的测试盯着。
//!
//! ## serde 表示是契约
//!
//! 快照 JSON 要跨设备、跨版本地来回走：字段名与 `skip_serializing_if` 的省略规则
//! 一格都不许随手改 —— 空字段不写进 JSON，绝大多数「只读过几页」的书只有一条
//! position，对面**旧版本**的设备也要读得回来。

use serde::{Deserialize, Serialize};
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

    /// 把这份快照里的**每一个时间**整体平移到另一个钟面上。
    ///
    /// 合并按「每条记录的时间谁新」取胜者，而那些时间是**各自那台设备的钟**写下的：
    /// 一台飞行模式几小时的手机、一台从没对过表的桌面，都能让「谁改得新」变成「谁的钟快」。
    /// 局域网是唯一能当场对表的场合 —— 握手的响应里带着对端的 UTC 秒，所以读回对面那份快照
    /// 时先按 `本机钟 - 对端钟` 平移，再去合并。WebDAV 那条路没有这个条件，只能照旧。
    ///
    /// `delta` 的符号由这一条定盘：`exported_at` 就是**对端钟**在导出那一刻的读数，搬完它必须
    /// 等于本机那一刻的读数。于是 `本机钟 - 对端钟` 是唯一说得通的方向，反着搬会把对端的
    /// 一小时快钟放大成两小时。
    ///
    /// 只平移**读回来**的那一份；本机这一份永远写自己的钟。平移过的记录落库后就是本机的时间，
    /// 下次传出去时由对面按它自己算出的偏移再搬一次。
    ///
    /// 每一步都是饱和加：`delta` 来自对面报文里的一个 `i64`，一个坏值不该把时间绕回 1970 之前。
    pub fn shift_clock(&mut self, delta: i64) {
        fn shift(value: &mut i64, delta: i64) {
            *value = value.saturating_add(delta);
        }

        shift(&mut self.exported_at, delta);
        for book in self.books.values_mut() {
            if let Some(position) = &mut book.position {
                shift(&mut position.updated_at, delta);
            }
            for annotation in &mut book.annotations {
                shift(&mut annotation.created_at, delta);
                shift(&mut annotation.updated_at, delta);
            }
            for bookmark in &mut book.bookmarks {
                shift(&mut bookmark.created_at, delta);
                shift(&mut bookmark.updated_at, delta);
            }
            for tombstone in &mut book.tombstones {
                shift(&mut tombstone.deleted_at, delta);
            }
            if let Some(settings) = &mut book.settings {
                shift(&mut settings.updated_at, delta);
            }
        }
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
