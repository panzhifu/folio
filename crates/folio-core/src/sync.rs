//! 跨设备同步：记录版本优先，同版本按删除/规范载荷建立确定性全序。
//! 每台设备只写自己的快照；合并不依赖接收方或转发设备身份。

use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::net::remote::{self, RemoteStore};
use crate::store::Store;
use folio_model::sync::SyncOrder;

// 快照与各记录的 **serde 形状**已下沉 folio-model（2026-11-12 阶段二，「换 crate 不换
// 形状」）：它们是多消费方的纯数据（store 收快照、lansync 传清单、app 读 Report 都认
// 这几份类型），而 folio-model 不认识网络与书库，正好当两边的会合点。旧路径沿用，
// `crate::sync::Snapshot` 这类写法（store/sync、net/lansync、本文件）一行不改。
// 这里留下的是**行为**：合并（`merge`）、快照 JSON 的编解码与一次往返的编排。
pub use folio_model::sync::{
    AnnotationRecord, BookEntry, BookSnapshot, BookmarkRecord, PositionRecord, SNAPSHOT_VERSION,
    SettingsRecord, Snapshot, Tombstone, TombstoneKind,
};

/// 对齐导出时间，仅供握手展示；记录冲突版本不随时钟平移。
pub fn retimed(snapshot: &Snapshot, peer_clock: i64, now: i64) -> Snapshot {
    let mut copy = snapshot.clone();
    copy.shift_clock(now.saturating_sub(peer_clock));
    copy
}

// 合并的**产出**形状（Plan / Merged / Stats / DeleteRecord / LocalSnapshot）已随
// 阶段四下沉 folio-model：store 独立成 crate 后它们有了第三个消费方，按 model 的
// 第二条规矩进来。这里 re-export 沿旧路径。
pub use folio_model::sync::{DeleteRecord, LocalSnapshot, Merged, Plan, Stats};

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

    fn deleted(&self) -> bool {
        matches!(self, Self::Deleted { .. })
    }
}

/// 同一条 uid 只留下最新那次事件。
///
/// 同版本删除优先，其余按规范载荷排序，避免接收方各自保留自己的值。
fn keep_event<T: SyncOrder>(events: &mut HashMap<String, Event<T>>, uid: String, event: Event<T>) {
    let replace = match events.get(&uid) {
        None => true,
        Some(old) => {
            if event.at() != old.at() {
                event.at() > old.at()
            } else if event.deleted() != old.deleted() {
                event.deleted()
            } else if let (
                Event::Active { record: next, .. },
                Event::Active {
                    record: previous, ..
                },
            ) = (&event, old)
            {
                next.sync_cmp(previous).is_gt()
            } else {
                false
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

        // 同版本按内容排序，避免两个接收方各自留下不同进度。
        let mut best_position = local_book.position.clone().map(|p| (p, Source::Local));
        for remote in &remote_books {
            let Some(candidate) = &remote.position else {
                continue;
            };
            let replace = match &best_position {
                None => true,
                Some((current, _)) => candidate.sync_cmp(current).is_gt(),
            };
            if replace {
                best_position = Some((candidate.clone(), Source::Remote));
            }
        }
        if let Some((winner, source)) = &best_position {
            let changed = local_book
                .position
                .as_ref()
                .is_none_or(|local| local != winner);
            if !source.is_local() && changed {
                plan.positions.push((hash.clone(), winner.clone()));
                stats.positions += 1;
            }
            out_book.position = Some(winner.clone());
        }

        // 阅读设置按整份差量取确定性胜者。
        // 不做逐格合并 —— 差量里每一格没有自己的钟，要逐格就得先给每一格配一个时间，
        // 那一层这一轮不买。
        let mut best_settings = local_book.settings.clone().map(|s| (s, Source::Local));
        for remote in &remote_books {
            let Some(candidate) = &remote.settings else {
                continue;
            };
            let replace = match &best_settings {
                None => true,
                Some((current, _)) => candidate.sync_cmp(current).is_gt(),
            };
            if replace {
                best_settings = Some((candidate.clone(), Source::Remote));
            }
        }
        if let Some((winner, source)) = &best_settings {
            let changed = local_book
                .settings
                .as_ref()
                .is_none_or(|local| local != winner);
            if !source.is_local() && changed {
                plan.settings.push((hash.clone(), winner.clone()));
                stats.settings += 1;
            }
            out_book.settings = Some(winner.clone());
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
                    let known = local_book.tombstones.iter().any(|t| {
                        t.kind == TombstoneKind::Annotation
                            && t.uid == uid
                            && t.deleted_at >= tombstone.deleted_at
                    });
                    if !source.is_local() && (existed_locally || !known) {
                        plan.annotation_deletes.push(DeleteRecord {
                            book_hash: hash.clone(),
                            uid: uid.clone(),
                            deleted_at: tombstone.deleted_at,
                        });
                        stats.annotations_deleted += usize::from(existed_locally);
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
                    let known = local_book.tombstones.iter().any(|t| {
                        t.kind == TombstoneKind::Bookmark
                            && t.uid == uid
                            && t.deleted_at >= tombstone.deleted_at
                    });
                    if !source.is_local() && (existed_locally || !known) {
                        plan.bookmark_deletes.push(DeleteRecord {
                            book_hash: hash.clone(),
                            uid: uid.clone(),
                            deleted_at: tombstone.deleted_at,
                        });
                        stats.bookmarks_deleted += usize::from(existed_locally);
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
            || out_book.settings.is_some()
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

/// 快照 ⇄ JSON 的两个出口。
///
/// 装配层（`folio-app`）为「线上长什么样」再引一份 `serde_json` 不值：形状的真相源是这个
/// 模块，编解码也就住在这里。两条都只走 `serde_json` 的默认约定，不自定字段名 ——
/// 远端那些文件要能被以后版本的 folio 读，字段名一改就再也对不上了。
pub fn snapshot_to_json(snapshot: &Snapshot) -> Result<String, Error> {
    Ok(serde_json::to_string(snapshot)?)
}

/// 解一份**别人写的**快照。
///
/// 版本高于本机的直接拒：它的字段语义可能是两套，合错了没人看得出来（WebDAV 那条路对读不懂
/// 的文件也是跳过而不是猜，见 [`sync_once`]）。
pub fn snapshot_from_json(bytes: &[u8]) -> Result<Snapshot, Error> {
    let snapshot: Snapshot = serde_json::from_slice(bytes)?;
    if snapshot.version != SNAPSHOT_VERSION {
        use serde::de::Error as _;
        return Err(Error::Json(serde_json::Error::custom(format!(
            "快照是第 {} 版，本机只认第 {SNAPSHOT_VERSION} 版",
            snapshot.version
        ))));
    }
    Ok(snapshot)
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
    remote: &dyn RemoteStore,
    remote_dir: &str,
    device_id: &str,
    now: i64,
) -> Result<Report, Error> {
    if !valid_device_id(device_id) {
        return Err(Error::Config("设备 id 不合法"));
    }
    let dir = normalize_remote_dir(remote_dir)?;
    remote.mkdir_all(&dir)?;
    let own_name = format!("{device_id}.json");
    let entries = remote.list(&dir)?;

    let mut remotes = Vec::new();
    let mut skipped_files = 0usize;
    for entry in entries {
        // 用路径末段而不是 `entry.name`：后者优先取服务器的 `displayname`，有些服务端
        // 会把它换成书名或去掉扩展名，拿它判「这是不是我自己那份」会漏掉自己的旧文件。
        let file_name = entry.path.rsplit('/').next().unwrap_or(&entry.path);
        if entry.is_dir || !file_name.ends_with(".json") || file_name == own_name {
            continue;
        }
        let bytes = remote.get(&entry.path)?;
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

    let local = store
        .collect_sync(device_id, now)
        .map_err(|e| Error::Store(e.into()))?;
    let merged = merge(&local.snapshot, &remotes, now);
    store
        .apply_sync(&merged.plan)
        .map_err(|e| Error::Store(e.into()))?;
    refresh_projections(store, &merged.plan);

    let body = serde_json::to_vec(&merged.snapshot)?;
    let path = if dir.is_empty() {
        own_name
    } else {
        format!("{dir}/{own_name}")
    };
    remote.put(&path, body.clone())?;
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

/// 同步后的旧版记录重新计算当前版显示位置；派生失败不回滚已提交的用户数据。
pub fn refresh_projections(store: &Store, plan: &Plan) {
    let hashes = plan
        .annotation_upserts
        .iter()
        .map(|(hash, _)| hash)
        .chain(plan.bookmark_upserts.iter().map(|(hash, _)| hash));
    let mut books = HashSet::new();
    for hash in hashes {
        if let Ok(ids) = store.books_for_content_hash(hash) {
            books.extend(ids);
        }
    }
    for book in books {
        if let Err(error) = crate::ingest::updates::refresh_anchor_projections(
            store,
            book,
            &crate::infra::task::StageCtx::detached(),
        ) {
            tracing::warn!(book,error=%error,"同步记录已保留，当前版本位置待重新定位");
        }
    }
}

/// 只连一下、建一下远端目录：设置页的「测试连接」。不读不写任何书籍数据。
pub fn test_connection(remote: &dyn RemoteStore, remote_dir: &str) -> Result<(), Error> {
    let dir = normalize_remote_dir(remote_dir)?;
    remote.mkdir_all(&dir)?;
    remote.list(&dir)?;
    Ok(())
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod wire_tests;
