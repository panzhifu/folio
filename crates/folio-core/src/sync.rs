//! 跨设备同步：远端快照的形状、合并规则与一次往返。
//!
//! **每台设备写自己的一份**：远端的 `<dir>/<device_id>.json`。同步时读全部设备的文件 +
//! 本机快照，按每条记录的时间戳合并，把结果写回本机，再覆盖自己那一份。没有用
//! `If-Match` / CAS：一台设备只写自己的文件，两台同时同步也不会互相覆盖。
//!
//! 合并口径是**每条记录最新一次写入胜**：
//!
//! - 进度的 `updated_at`、批注的 `updated_at`、书签的 `updated_at`；
//! - 删除在 `sync_tombstones` 里留一条 `deleted_at`，同样参加比较 —— 没有墓碑的话，
//!   一台设备删了、另一台设备下一次上传又会把它带回来；
//! - 时间相同时本机优先；本机与远端都不是本机时，删除优先（不让同秒的删除被复活）。
//!
//! 只同步**本机已有的书**：远端那本如果这台设备还没导入，记录留在远端，等书到了
//! 下一次同步再落。书本身不在这里传（那是导入管线的事，见 `doc/readers-vs-folio.md`）。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::content::anchor::TextAnchor;
use crate::net::remote::{self, dav};
use crate::position::Locator;
use crate::store::Store;

/// 远端 JSON 的格式版本。读到更高版本的文件跳过，不猜它的字段。
pub const SNAPSHOT_VERSION: u32 = 1;

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
}

impl Stats {
    pub fn applied(&self) -> usize {
        self.positions + self.annotations + self.bookmarks
    }
}

/// 一次合并的结果：该上传的自己那份、该写回本机的动作、给界面看的计数。
#[derive(Debug, Clone, PartialEq)]
pub struct Merged {
    pub snapshot: Snapshot,
    pub plan: Plan,
    pub stats: Stats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Local,
    Remote,
}

impl Source {
    fn is_local(self) -> bool {
        self == Self::Local
    }
}

/// 一条记录在合并里的候选：活跃记录或墓碑，带时间和来源。
enum Event<T> {
    Active {
        record: T,
        at: i64,
        source: Source,
    },
    Deleted {
        tombstone: Tombstone,
        source: Source,
    },
}

impl<T> Event<T> {
    fn at(&self) -> i64 {
        match self {
            Self::Active { at, .. } => *at,
            Self::Deleted { tombstone, .. } => tombstone.deleted_at,
        }
    }

    fn source(&self) -> Source {
        match self {
            Self::Active { source, .. } | Self::Deleted { source, .. } => *source,
        }
    }

    fn deleted(&self) -> bool {
        matches!(self, Self::Deleted { .. })
    }
}

/// 同一条 uid 只留下最新那次事件。
///
/// 时间相同：本机优先；两边都不是本机时删除优先。后一条是为了避免「同一秒里一台设备
/// 删、另一台设备还没收到」时把记录复活 —— 删除比复活更难逆转。
fn keep_event<T>(events: &mut HashMap<String, Event<T>>, uid: String, event: Event<T>) {
    let replace = match events.get(&uid) {
        None => true,
        Some(old) => {
            if event.at() != old.at() {
                event.at() > old.at()
            } else if event.source().is_local() != old.source().is_local() {
                event.source().is_local()
            } else {
                event.deleted() && !old.deleted()
            }
        }
    };
    if replace {
        events.insert(uid, event);
    }
}

/// 合并本机与其余设备。`remotes` 不含本机自己那份（自己的旧快照以本机数据库为准）。
pub fn merge(local: &Snapshot, remotes: &[Snapshot], now: i64) -> Merged {
    let mut out = Snapshot::new(local.device_id.clone(), now);
    let mut plan = Plan::default();
    let mut stats = Stats::default();

    for (hash, local_book) in &local.books {
        let remote_books: Vec<&BookSnapshot> = remotes
            .iter()
            .filter_map(|snapshot| snapshot.books.get(hash))
            .collect();

        let mut out_book = BookSnapshot::default();

        // 进度：新的赢；同时间本机优先。
        let mut best_position = local_book.position.clone().map(|p| (p, Source::Local));
        for remote in &remote_books {
            let Some(candidate) = &remote.position else {
                continue;
            };
            let replace = match &best_position {
                None => true,
                Some((current, _)) => candidate.updated_at > current.updated_at,
            };
            if replace {
                best_position = Some((candidate.clone(), Source::Remote));
            }
        }
        if let Some((winner, source)) = &best_position {
            let changed = local_book
                .position
                .as_ref()
                .is_none_or(|local| local.mode != winner.mode || local.locator != winner.locator);
            if !source.is_local() && changed {
                plan.positions.push((hash.clone(), winner.clone()));
                stats.positions += 1;
            }
            out_book.position = Some(winner.clone());
        }

        // 批注：活跃记录与墓碑放进同一张表按 uid 比时间。
        let mut annotation_events: HashMap<String, Event<AnnotationRecord>> = HashMap::new();
        let mut local_annotation_uids = HashSet::new();
        for record in &local_book.annotations {
            local_annotation_uids.insert(record.uid.clone());
            keep_event(
                &mut annotation_events,
                record.uid.clone(),
                Event::Active {
                    at: record.updated_at,
                    record: record.clone(),
                    source: Source::Local,
                },
            );
        }
        for tombstone in local_book
            .tombstones
            .iter()
            .filter(|t| t.kind == TombstoneKind::Annotation)
        {
            keep_event(
                &mut annotation_events,
                tombstone.uid.clone(),
                Event::Deleted {
                    tombstone: tombstone.clone(),
                    source: Source::Local,
                },
            );
        }
        for remote in &remote_books {
            for record in &remote.annotations {
                keep_event(
                    &mut annotation_events,
                    record.uid.clone(),
                    Event::Active {
                        at: record.updated_at,
                        record: record.clone(),
                        source: Source::Remote,
                    },
                );
            }
            for tombstone in remote
                .tombstones
                .iter()
                .filter(|t| t.kind == TombstoneKind::Annotation)
            {
                keep_event(
                    &mut annotation_events,
                    tombstone.uid.clone(),
                    Event::Deleted {
                        tombstone: tombstone.clone(),
                        source: Source::Remote,
                    },
                );
            }
        }
        for (uid, event) in annotation_events {
            match event {
                Event::Active { record, source, .. } => {
                    if !source.is_local() {
                        plan.annotation_upserts.push((hash.clone(), record.clone()));
                        stats.annotations += 1;
                    }
                    out_book.annotations.push(record);
                }
                Event::Deleted {
                    tombstone, source, ..
                } => {
                    let existed_locally = local_annotation_uids.contains(&uid);
                    if !source.is_local() && existed_locally {
                        plan.annotation_deletes.push(DeleteRecord {
                            book_hash: hash.clone(),
                            uid: uid.clone(),
                            deleted_at: tombstone.deleted_at,
                        });
                        stats.annotations_deleted += 1;
                    }
                    out_book.tombstones.push(tombstone);
                }
            }
        }
        out_book.annotations.sort_by(|a, b| a.uid.cmp(&b.uid));

        // 书签：同一套；书签不编辑，时间取 updated_at（撤销拔除会把它抬到现在）。
        let mut bookmark_events: HashMap<String, Event<BookmarkRecord>> = HashMap::new();
        let mut local_bookmark_uids = HashSet::new();
        for record in &local_book.bookmarks {
            local_bookmark_uids.insert(record.uid.clone());
            keep_event(
                &mut bookmark_events,
                record.uid.clone(),
                Event::Active {
                    at: record.updated_at,
                    record: record.clone(),
                    source: Source::Local,
                },
            );
        }
        for tombstone in local_book
            .tombstones
            .iter()
            .filter(|t| t.kind == TombstoneKind::Bookmark)
        {
            keep_event(
                &mut bookmark_events,
                tombstone.uid.clone(),
                Event::Deleted {
                    tombstone: tombstone.clone(),
                    source: Source::Local,
                },
            );
        }
        for remote in &remote_books {
            for record in &remote.bookmarks {
                keep_event(
                    &mut bookmark_events,
                    record.uid.clone(),
                    Event::Active {
                        at: record.updated_at,
                        record: record.clone(),
                        source: Source::Remote,
                    },
                );
            }
            for tombstone in remote
                .tombstones
                .iter()
                .filter(|t| t.kind == TombstoneKind::Bookmark)
            {
                keep_event(
                    &mut bookmark_events,
                    tombstone.uid.clone(),
                    Event::Deleted {
                        tombstone: tombstone.clone(),
                        source: Source::Remote,
                    },
                );
            }
        }
        for (uid, event) in bookmark_events {
            match event {
                Event::Active { record, source, .. } => {
                    if !source.is_local() {
                        plan.bookmark_upserts.push((hash.clone(), record.clone()));
                        stats.bookmarks += 1;
                    }
                    out_book.bookmarks.push(record);
                }
                Event::Deleted {
                    tombstone, source, ..
                } => {
                    let existed_locally = local_bookmark_uids.contains(&uid);
                    if !source.is_local() && existed_locally {
                        plan.bookmark_deletes.push(DeleteRecord {
                            book_hash: hash.clone(),
                            uid: uid.clone(),
                            deleted_at: tombstone.deleted_at,
                        });
                        stats.bookmarks_deleted += 1;
                    }
                    out_book.tombstones.push(tombstone);
                }
            }
        }
        out_book.bookmarks.sort_by(|a, b| a.uid.cmp(&b.uid));
        out_book.tombstones.sort_by(|a, b| {
            (tombstone_kind_rank(a.kind), a.uid.as_str())
                .cmp(&(tombstone_kind_rank(b.kind), b.uid.as_str()))
        });

        if out_book.position.is_some()
            || !out_book.annotations.is_empty()
            || !out_book.bookmarks.is_empty()
            || !out_book.tombstones.is_empty()
        {
            out.books.insert(hash.clone(), out_book);
        }
    }

    plan.positions.sort_by(|a, b| a.0.cmp(&b.0));
    plan.annotation_upserts
        .sort_by(|a, b| a.1.uid.cmp(&b.1.uid));
    plan.annotation_deletes.sort_by(|a, b| a.uid.cmp(&b.uid));
    plan.bookmark_upserts.sort_by(|a, b| a.1.uid.cmp(&b.1.uid));
    plan.bookmark_deletes.sort_by(|a, b| a.uid.cmp(&b.uid));

    Merged {
        snapshot: out,
        plan,
        stats,
    }
}

fn tombstone_kind_rank(kind: TombstoneKind) -> u8 {
    match kind {
        TombstoneKind::Annotation => 0,
        TombstoneKind::Bookmark => 1,
    }
}

/// 一次手动同步的计数，给设置页念。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub remote_devices: usize,
    pub skipped_files: usize,
    pub skipped_books: usize,
    pub applied_positions: usize,
    pub applied_annotations: usize,
    pub applied_bookmarks: usize,
    pub deleted_annotations: usize,
    pub deleted_bookmarks: usize,
    pub uploaded_bytes: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("远端：{0}")]
    Remote(#[from] remote::Error),
    #[error("书库：{0}")]
    Store(#[from] crate::Error),
    #[error("快照 JSON：{0}")]
    Json(#[from] serde_json::Error),
    #[error("同步配置不完整：{0}")]
    Config(&'static str),
}

/// 远端目录的清洗：去掉首尾空白与斜杠，拒绝 `.` / `..` 段。
pub fn normalize_remote_dir(dir: &str) -> Result<String, Error> {
    let mut parts = Vec::new();
    for part in dir.trim().split('/') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if part == "." || part == ".." {
            return Err(Error::Config("远端目录不能含 . 或 .."));
        }
        parts.push(part);
    }
    Ok(parts.join("/"))
}

/// 设备 id 会成为远端文件名。只允许字母数字、`-`、`_`，免得一台设备用斜杠
/// 把自己写到别的目录去。
pub fn valid_device_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// 新装机器第一次同步时生成。不引 uuid：`RandomState` 的随机种子由 OS 给，
/// 两个 hasher 各 64 位，够区分同一台机器上的两套安装。
pub fn new_device_id() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let mut left = RandomState::new().build_hasher();
    let mut right = RandomState::new().build_hasher();
    left.write_u128(nanos);
    right.write_u128(!nanos ^ (std::process::id() as u128));
    format!("{:016x}{:016x}", left.finish(), right.finish())
}

/// 一次完整往返。**同步**：调用方负责放到后台线程。
pub fn sync_once(
    store: &Store,
    client: &dav::Client,
    remote_dir: &str,
    device_id: &str,
    now: i64,
) -> Result<Report, Error> {
    if !valid_device_id(device_id) {
        return Err(Error::Config("设备 id 不合法"));
    }
    let dir = normalize_remote_dir(remote_dir)?;
    ensure_dir(client, &dir)?;
    let own_name = format!("{device_id}.json");
    let entries = client.list(&dir)?;

    let mut remotes = Vec::new();
    let mut skipped_files = 0usize;
    for entry in entries {
        // 用路径末段而不是 `entry.name`：后者优先取服务器的 `displayname`，有些服务端
        // 会把它换成书名或去掉扩展名，拿它判「这是不是我自己那份」会漏掉自己的旧文件。
        let file_name = entry.path.rsplit('/').next().unwrap_or(&entry.path);
        if entry.is_dir || !file_name.ends_with(".json") || file_name == own_name {
            continue;
        }
        let bytes = client.get(&entry.path)?;
        match serde_json::from_slice::<Snapshot>(&bytes) {
            Ok(snapshot) if snapshot.version == SNAPSHOT_VERSION => remotes.push(snapshot),
            Ok(snapshot) => {
                tracing::warn!(
                    "sync: 跳过版本 {} 的远端快照 {}（本机只认 v{SNAPSHOT_VERSION}）",
                    snapshot.version,
                    entry.path
                );
                skipped_files += 1;
            }
            Err(error) => {
                tracing::warn!("sync: 跳过读不懂的远端快照 {}：{error}", entry.path);
                skipped_files += 1;
            }
        }
    }

    let local = store.collect_sync(device_id, now)?;
    let merged = merge(&local.snapshot, &remotes, now);
    store.apply_sync(&merged.plan)?;

    let body = serde_json::to_vec(&merged.snapshot)?;
    let path = if dir.is_empty() {
        own_name
    } else {
        format!("{dir}/{own_name}")
    };
    client.put(&path, body.clone())?;
    tracing::info!(
        "sync: {} 台远端设备 → 应用 {} 条，删除 {} 条，上传 {} 字节",
        remotes.len(),
        merged.stats.applied(),
        merged.stats.annotations_deleted + merged.stats.bookmarks_deleted,
        body.len()
    );

    Ok(Report {
        remote_devices: remotes.len(),
        skipped_files,
        skipped_books: local.skipped_books,
        applied_positions: merged.stats.positions,
        applied_annotations: merged.stats.annotations,
        applied_bookmarks: merged.stats.bookmarks,
        deleted_annotations: merged.stats.annotations_deleted,
        deleted_bookmarks: merged.stats.bookmarks_deleted,
        uploaded_bytes: body.len(),
    })
}

fn ensure_dir(client: &dav::Client, dir: &str) -> Result<(), Error> {
    let mut prefix = String::new();
    for part in dir.split('/').filter(|part| !part.is_empty()) {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(part);
        client.mkdir(&prefix)?;
    }
    Ok(())
}

/// 只连一下、建一下远端目录：设置页的「测试连接」。不读不写任何书籍数据。
pub fn test_connection(client: &dav::Client, remote_dir: &str) -> Result<(), Error> {
    let dir = normalize_remote_dir(remote_dir)?;
    ensure_dir(client, &dir)?;
    client.list(&dir)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::content::anchor::TextAnchor;
    use crate::position::Locator;
    use crate::sync::{
        AnnotationRecord, BookSnapshot, BookmarkRecord, Snapshot, Tombstone, TombstoneKind, merge,
        new_device_id, normalize_remote_dir, valid_device_id,
    };

    fn annotation(uid: &str, at: i64) -> AnnotationRecord {
        AnnotationRecord {
            uid: uid.into(),
            anchor: TextAnchor::default(),
            note: "笔记".into(),
            color: "amber".into(),
            style: "highlight".into(),
            created_at: at,
            updated_at: at,
        }
    }

    fn bookmark(uid: &str, at: i64) -> BookmarkRecord {
        BookmarkRecord {
            uid: uid.into(),
            anchor: TextAnchor::default(),
            created_at: at,
            updated_at: at,
        }
    }

    fn tombstone(kind: TombstoneKind, uid: &str, at: i64) -> Tombstone {
        Tombstone {
            kind,
            uid: uid.into(),
            deleted_at: at,
        }
    }

    fn with_book(mut snapshot: Snapshot, hash: &str, book: BookSnapshot) -> Snapshot {
        snapshot.books.insert(hash.into(), book);
        snapshot
    }

    fn position(offset: usize, at: i64) -> crate::sync::PositionRecord {
        crate::sync::PositionRecord {
            mode: "paged".into(),
            locator: Locator { chapter: 0, offset },
            updated_at: at,
        }
    }

    /// 远端更新的一页进度与一条批注会写回本机；本机更旧的那份不覆盖远端。
    #[test]
    fn newer_remote_records_win_and_are_planned_locally() {
        let local = with_book(
            Snapshot::new("dev-local", 10),
            "hash",
            BookSnapshot {
                position: Some(position(10, 10)),
                annotations: vec![annotation("a", 10)],
                ..BookSnapshot::default()
            },
        );
        let remote = with_book(
            Snapshot::new("dev-remote", 20),
            "hash",
            BookSnapshot {
                position: Some(position(20, 20)),
                annotations: vec![annotation("a", 20), annotation("b", 20)],
                ..BookSnapshot::default()
            },
        );

        let merged = merge(&local, &[remote], 30);
        assert_eq!(merged.stats.positions, 1);
        assert_eq!(merged.stats.annotations, 2);
        assert_eq!(
            merged.snapshot.books["hash"]
                .position
                .as_ref()
                .unwrap()
                .locator
                .offset,
            20
        );
        assert_eq!(merged.snapshot.books["hash"].annotations.len(), 2);
        assert_eq!(merged.plan.positions.len(), 1);
        assert_eq!(merged.plan.annotation_upserts.len(), 2);
        assert!(merged.plan.annotation_deletes.is_empty());
    }

    /// 远端墓碑比本机记录新：本机那条要被删掉，且墓碑继续往别的设备传。
    #[test]
    fn a_newer_tombstone_deletes_the_local_row() {
        let local = with_book(
            Snapshot::new("dev-local", 10),
            "hash",
            BookSnapshot {
                annotations: vec![annotation("a", 10)],
                ..BookSnapshot::default()
            },
        );
        let remote = with_book(
            Snapshot::new("dev-remote", 20),
            "hash",
            BookSnapshot {
                tombstones: vec![tombstone(TombstoneKind::Annotation, "a", 20)],
                ..BookSnapshot::default()
            },
        );

        let merged = merge(&local, &[remote], 30);
        assert_eq!(merged.stats.annotations_deleted, 1);
        assert_eq!(merged.plan.annotation_deletes.len(), 1);
        assert_eq!(merged.plan.annotation_deletes[0].uid, "a");
        assert!(merged.snapshot.books["hash"].annotations.is_empty());
        assert_eq!(merged.snapshot.books["hash"].tombstones.len(), 1);
    }

    /// 撤销删除把 updated_at 抬到删除之后，于是本机记录压过远端旧墓碑，不会被再删一次。
    #[test]
    fn a_restored_row_beats_an_older_tombstone() {
        let local = with_book(
            Snapshot::new("dev-local", 40),
            "hash",
            BookSnapshot {
                annotations: vec![annotation("a", 40)],
                ..BookSnapshot::default()
            },
        );
        let remote = with_book(
            Snapshot::new("dev-remote", 20),
            "hash",
            BookSnapshot {
                tombstones: vec![tombstone(TombstoneKind::Annotation, "a", 20)],
                ..BookSnapshot::default()
            },
        );

        let merged = merge(&local, &[remote], 50);
        assert!(merged.plan.annotation_deletes.is_empty());
        assert_eq!(merged.snapshot.books["hash"].annotations.len(), 1);
        assert!(merged.snapshot.books["hash"].tombstones.is_empty());
    }

    /// 书签按 updated_at 合并；撤销拔除同样压过旧墓碑。
    #[test]
    fn bookmarks_merge_on_updated_at_and_follow_tombstones() {
        let local = with_book(
            Snapshot::new("dev-local", 10),
            "hash",
            BookSnapshot {
                bookmarks: vec![bookmark("left", 10), bookmark("restored", 40)],
                ..BookSnapshot::default()
            },
        );
        let remote = with_book(
            Snapshot::new("dev-remote", 20),
            "hash",
            BookSnapshot {
                bookmarks: vec![bookmark("right", 20)],
                tombstones: vec![
                    tombstone(TombstoneKind::Bookmark, "left", 20),
                    tombstone(TombstoneKind::Bookmark, "restored", 20),
                ],
                ..BookSnapshot::default()
            },
        );

        let merged = merge(&local, &[remote], 50);
        assert_eq!(merged.stats.bookmarks, 1);
        assert_eq!(merged.stats.bookmarks_deleted, 1);
        assert_eq!(merged.plan.bookmark_upserts[0].1.uid, "right");
        assert_eq!(merged.plan.bookmark_deletes[0].uid, "left");
        let uids: Vec<&str> = merged.snapshot.books["hash"]
            .bookmarks
            .iter()
            .map(|bookmark| bookmark.uid.as_str())
            .collect();
        assert_eq!(uids, ["restored", "right"]);
    }

    /// 远端有、本机没有的书不动：记录留在远端，等这本书被导入后再同步。
    #[test]
    fn remote_books_missing_locally_are_left_alone() {
        let local = Snapshot::new("dev-local", 10);
        let remote = with_book(
            Snapshot::new("dev-remote", 20),
            "not-here",
            BookSnapshot {
                annotations: vec![annotation("a", 20)],
                ..BookSnapshot::default()
            },
        );

        let merged = merge(&local, &[remote], 30);
        assert!(merged.snapshot.books.is_empty());
        assert!(merged.plan.annotation_upserts.is_empty());
    }

    /// 同一秒里远端一台删、一台留：删除优先，不让同秒复活。
    #[test]
    fn a_remote_tombstone_wins_a_same_second_tie_against_remote_record() {
        let local = with_book(
            Snapshot::new("dev-local", 10),
            "hash",
            BookSnapshot::default(),
        );
        let a = with_book(
            Snapshot::new("dev-a", 20),
            "hash",
            BookSnapshot {
                annotations: vec![annotation("a", 20)],
                ..BookSnapshot::default()
            },
        );
        let b = with_book(
            Snapshot::new("dev-b", 20),
            "hash",
            BookSnapshot {
                tombstones: vec![tombstone(TombstoneKind::Annotation, "a", 20)],
                ..BookSnapshot::default()
            },
        );

        let merged = merge(&local, &[a, b], 30);
        assert!(merged.snapshot.books["hash"].annotations.is_empty());
        assert_eq!(merged.snapshot.books["hash"].tombstones.len(), 1);
    }

    #[test]
    fn device_ids_are_safe_file_names_and_differ() {
        let a = new_device_id();
        let b = new_device_id();
        assert_eq!(a.len(), 32);
        assert!(valid_device_id(&a), "{a}");
        assert_ne!(a, b);
        assert!(!valid_device_id(""));
        assert!(!valid_device_id("a/b"));
        assert!(!valid_device_id("设备"));
    }

    #[test]
    fn remote_dir_is_normalized_and_rejects_parent_segments() {
        assert_eq!(
            normalize_remote_dir(" /folio//sync/ ").unwrap(),
            "folio/sync"
        );
        assert_eq!(normalize_remote_dir("").unwrap(), "");
        assert!(normalize_remote_dir("folio/../别的").is_err());
    }

    #[test]
    fn a_snapshot_round_trips_through_json() {
        let snapshot = with_book(
            Snapshot::new("dev", 10),
            "hash",
            BookSnapshot {
                position: Some(position(3, 10)),
                annotations: vec![annotation("a", 10)],
                bookmarks: vec![bookmark("b", 10)],
                tombstones: vec![tombstone(TombstoneKind::Annotation, "c", 10)],
            },
        );
        let bytes = serde_json::to_vec(&snapshot).unwrap();
        let back: Snapshot = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, snapshot);
        assert!(bytes.len() < 4096);
    }

    #[test]
    fn an_empty_book_snapshot_omits_its_fields() {
        let mut books = BTreeMap::new();
        books.insert("hash".to_string(), BookSnapshot::default());
        let snapshot = Snapshot {
            version: 1,
            device_id: "dev".into(),
            exported_at: 10,
            books,
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(json.contains("\"books\":{\"hash\":{}}"), "{json}");
    }
}

#[cfg(test)]
mod wire_tests {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use crate::content::anchor::TextAnchor;
    use crate::content::format::{DocMeta, Format};
    use crate::net::remote::dav;
    use crate::net::remote::{Cancel, Method, Request, Response, Sink, Transport};
    use crate::position::Locator;
    use crate::store::{AnnotationStyle, Store};
    use crate::sync::{
        AnnotationRecord, BookSnapshot, PositionRecord, Snapshot, Tombstone, TombstoneKind,
        sync_once,
    };

    struct MockDav {
        base: String,
        files: Mutex<HashMap<String, Vec<u8>>>,
        seen: Mutex<Vec<(Method, String)>>,
    }

    impl MockDav {
        fn new(base: &str, files: HashMap<String, Vec<u8>>) -> Self {
            Self {
                base: base.into(),
                files: Mutex::new(files),
                seen: Mutex::new(Vec::new()),
            }
        }

        fn rel(&self, url: &str) -> String {
            url.strip_prefix(&self.base)
                .unwrap_or(url)
                .trim_matches('/')
                .to_string()
        }

        fn path_prefix(&self) -> &str {
            let after = self.base.split_once("://").unwrap().1;
            &after[after.find('/').unwrap()..]
        }

        fn listing(&self, dir: &str) -> String {
            let prefix = if dir.is_empty() {
                String::new()
            } else {
                format!("{dir}/")
            };
            let mut entries: Vec<String> = self
                .files
                .lock()
                .unwrap()
                .keys()
                .filter(|path| path.starts_with(&prefix))
                .filter_map(|path| {
                    let rest = path.strip_prefix(&prefix)?;
                    (!rest.contains('/')).then(|| rest.to_string())
                })
                .collect();
            entries.sort();
            let mut xml = String::from(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?><d:multistatus xmlns:d=\"DAV:\">",
            );
            for entry in entries {
                // displayname 故意与文件名不同：同步层必须认路径末段，不能拿 displayname
                // 判「这是不是我自己那份」。
                xml.push_str(&format!(
                    "<d:response><d:href>{}{}{}</d:href><d:propstat><d:prop><d:resourcetype/><d:displayname>shown</d:displayname></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>",
                    self.path_prefix(),
                    prefix,
                    entry
                ));
            }
            xml.push_str("</d:multistatus>");
            xml
        }
    }

    impl Transport for MockDav {
        fn request(&self, req: Request) -> crate::net::remote::Result<Response> {
            self.seen
                .lock()
                .unwrap()
                .push((req.method, req.url.clone()));
            let rel = self.rel(&req.url);
            let response = match req.method {
                Method::Mkcol => Response {
                    status: 201,
                    headers: Vec::new(),
                    body: Vec::new(),
                },
                Method::Put => {
                    self.files.lock().unwrap().insert(rel, req.body);
                    Response {
                        status: 201,
                        headers: Vec::new(),
                        body: Vec::new(),
                    }
                }
                Method::Get => match self.files.lock().unwrap().get(&rel) {
                    Some(body) => Response {
                        status: 200,
                        headers: Vec::new(),
                        body: body.clone(),
                    },
                    None => Response {
                        status: 404,
                        headers: Vec::new(),
                        body: Vec::new(),
                    },
                },
                Method::Propfind => {
                    let xml = self.listing(rel.trim_end_matches('/'));
                    Response {
                        status: 207,
                        headers: Vec::new(),
                        body: xml.into_bytes(),
                    }
                }
                _ => Response {
                    status: 405,
                    headers: Vec::new(),
                    body: Vec::new(),
                },
            };
            Ok(response)
        }

        fn download(
            &self,
            _req: Request,
            _sink: &mut dyn Sink,
            _cancel: &Cancel,
        ) -> crate::net::remote::Result<u64> {
            Err(crate::net::remote::Error::Cancelled)
        }
    }

    fn meta() -> DocMeta {
        DocMeta {
            title: "书".into(),
            format: Format::Epub,
            size: 1,
            ..DocMeta::default()
        }
    }

    fn position(offset: usize, at: i64) -> PositionRecord {
        PositionRecord {
            mode: "paged".into(),
            locator: Locator { chapter: 0, offset },
            updated_at: at,
        }
    }

    /// 一次完整往返：读别人的快照、应用远端记录与墓碑、写回自己的那份；自己的旧文件
    /// 即使读不懂也**不许被读**（它不是事实来源，本机库才是）。
    #[test]
    fn a_wire_round_trip_applies_remote_state_and_replaces_our_snapshot() {
        let store = Store::in_memory().unwrap();
        let book = store.add_book(Path::new("/books/a.epub"), &meta()).unwrap();
        store.set_content_hash(book, "hash-a", "sha256").unwrap();
        store
            .set_position_at(
                book,
                "paged",
                &Locator {
                    chapter: 0,
                    offset: 5,
                },
                10,
            )
            .unwrap();
        store
            .add_annotation(
                book,
                &TextAnchor::default(),
                "本机",
                "amber",
                AnnotationStyle::default(),
            )
            .unwrap();
        let local_uid = store.annotations(book).unwrap()[0].uid.clone();

        // 本机那条批注的时间戳是真实的「现在」；远端要压过它，就得给未来的时间。
        let future = crate::infra::time::now() + 100;
        let mut remote_book = BookSnapshot {
            position: Some(position(9, future)),
            annotations: vec![AnnotationRecord {
                uid: "remote-a".into(),
                anchor: TextAnchor::default(),
                note: "远".into(),
                color: "rose".into(),
                style: "wavy".into(),
                created_at: future,
                updated_at: future,
            }],
            ..BookSnapshot::default()
        };
        remote_book.tombstones.push(Tombstone {
            kind: TombstoneKind::Annotation,
            uid: local_uid.clone(),
            deleted_at: future,
        });
        let mut remote = Snapshot::new("dev-a", future);
        remote.books.insert("hash-a".into(), remote_book);

        let mut files = HashMap::new();
        files.insert(
            "folio-sync/dev-a.json".to_string(),
            serde_json::to_vec(&remote).unwrap(),
        );
        // 自己的旧文件故意写成坏 JSON：这趟都不该打开它，更不该因此失败。
        files.insert(
            "folio-sync/dev-local.json".to_string(),
            b"not json".to_vec(),
        );
        let mock = Arc::new(MockDav::new("https://dav.test/dav/", files));
        let client = dav::Client::new(
            dav::Endpoint::new("https://dav.test/dav/", "u", "p"),
            mock.clone(),
        );

        let report = sync_once(&store, &client, "folio-sync", "dev-local", future + 1).unwrap();
        assert_eq!(report.remote_devices, 1);
        assert_eq!(report.applied_positions, 1);
        assert_eq!(report.applied_annotations, 1);
        assert_eq!(report.deleted_annotations, 1);

        let (mode, locator, updated_at) = store.position_with_time(book).unwrap().unwrap();
        assert_eq!(mode, "paged");
        assert_eq!(locator.offset, 9, "远端更新的进度要落到本机");
        assert_eq!(updated_at, future, "要保留远端时间戳，不能写成现在");
        let annotations = store.annotations(book).unwrap();
        assert_eq!(annotations.len(), 1);
        assert_eq!(annotations[0].uid, "remote-a", "远端批注要进来");
        assert!(
            annotations
                .iter()
                .all(|annotation| annotation.uid != local_uid),
            "远端墓碑要删掉本机那条"
        );

        {
            let seen = mock.seen.lock().unwrap();
            assert!(
                !seen
                    .iter()
                    .any(|(method, url)| *method == Method::Get && url.ends_with("dev-local.json")),
                "自己的旧快照不该被读取"
            );
        }

        let uploaded = mock
            .files
            .lock()
            .unwrap()
            .get("folio-sync/dev-local.json")
            .cloned()
            .expect("自己的快照要写回去");
        let uploaded: Snapshot = serde_json::from_slice(&uploaded).unwrap();
        assert_eq!(uploaded.version, 1);
        assert_eq!(uploaded.device_id, "dev-local");
        let book_snapshot = &uploaded.books["hash-a"];
        assert_eq!(book_snapshot.annotations.len(), 1);
        assert_eq!(book_snapshot.annotations[0].uid, "remote-a");
        assert_eq!(book_snapshot.tombstones.len(), 1);
        assert_eq!(book_snapshot.tombstones[0].uid, local_uid);
    }
}
