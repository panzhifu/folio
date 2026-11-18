//! 来源观察只安排核对，不捕获正文。通知、stat 与轮转审计共享有界调度。

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::time::{Duration, Instant, SystemTime};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::store::BookSource;

const EVENT_CAPACITY: usize = 256;
const EVENT_PATHS: usize = 32;
const WATCH_LIMIT: usize = 128;
const SOURCE_PROBES: usize = 128;
const FILE_PROBES: usize = 256;
const DISPATCH_LIMIT: usize = 8;
const AUDIT_INTERVAL: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorStatus {
    Native,
    Partial,
    MetadataOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Stamp {
    File(u64, Option<SystemTime>),
    Missing,
    NotFile,
    Unreadable(ErrorKind),
}

fn stamp(path: &Path) -> Stamp {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => Stamp::File(meta.len(), meta.modified().ok()),
        Ok(_) => Stamp::NotFile,
        Err(error) if error.kind() == ErrorKind::NotFound => Stamp::Missing,
        Err(error) => Stamp::Unreadable(error.kind()),
    }
}

struct Observed {
    locator: String,
    kind: String,
    state: String,
}

struct Policy {
    sources: BTreeMap<i64, Observed>,
    dirty: BTreeSet<i64>,
    dispatch_cursor: i64,
    audit_cursor: i64,
    last_audit: Duration,
}

impl Policy {
    fn new() -> Self {
        Self {
            sources: BTreeMap::new(),
            dirty: BTreeSet::new(),
            dispatch_cursor: 0,
            audit_cursor: 0,
            last_audit: Duration::ZERO,
        }
    }

    fn sync(&mut self, sources: &[BookSource]) {
        let live: BTreeSet<_> = sources
            .iter()
            .filter(|s| eligible(s))
            .map(|s| s.id)
            .collect();
        self.sources.retain(|id, _| live.contains(id));
        self.dirty.retain(|id| live.contains(id));
        for source in sources.iter().filter(|s| eligible(s)) {
            let changed = self.sources.get(&source.id).is_none_or(|old| {
                old.locator != source.locator
                    || old.kind != source.kind
                    || (source.state == "available"
                        && matches!(
                            old.state.as_str(),
                            "missing" | "unreadable" | "failed" | "relink-required"
                        ))
            });
            if changed {
                self.dirty.insert(source.id);
            }
            // generation/fingerprint 是捕获任务自己的回写，不是新的文件变化。
            self.sources.insert(
                source.id,
                Observed {
                    locator: source.locator.clone(),
                    kind: source.kind.clone(),
                    state: source.state.clone(),
                },
            );
        }
    }

    fn all_dirty(&mut self) {
        self.dirty.extend(self.sources.keys().copied());
    }

    fn take(&mut self, now: Duration, force: bool) -> Vec<i64> {
        if force {
            self.all_dirty();
        }
        if now.saturating_sub(self.last_audit) >= AUDIT_INTERVAL {
            self.last_audit = now;
            if let Some(id) = next_id(self.sources.keys().copied(), self.audit_cursor) {
                self.audit_cursor = id;
                self.dirty.insert(id);
            }
        }
        let mut selected = Vec::with_capacity(DISPATCH_LIMIT);
        while selected.len() < DISPATCH_LIMIT {
            let Some(id) = next_id(self.dirty.iter().copied(), self.dispatch_cursor) else {
                break;
            };
            self.dirty.remove(&id);
            self.dispatch_cursor = id;
            selected.push(id);
        }
        selected
    }
}

fn next_id(ids: impl Iterator<Item = i64>, after: i64) -> Option<i64> {
    let mut first = None;
    for id in ids {
        first.get_or_insert(id);
        if id > after {
            return Some(id);
        }
    }
    first
}

fn eligible(source: &BookSource) -> bool {
    source.is_primary && !matches!(source.state.as_str(), "suppressed" | "relink-required")
}

fn enqueue(
    sender: &SyncSender<Vec<PathBuf>>,
    invalidated: &AtomicBool,
    result: notify::Result<notify::Event>,
) {
    let event = match result {
        Ok(event) if matches!(event.kind, EventKind::Access(_)) => return,
        Ok(event) => event,
        Err(_) => {
            invalidated.store(true, Ordering::Release);
            return;
        }
    };
    if event.need_rescan() || event.paths.is_empty() || event.paths.len() > EVENT_PATHS {
        invalidated.store(true, Ordering::Release);
        return;
    }
    match sender.try_send(event.paths) {
        Ok(()) | Err(TrySendError::Disconnected(_)) => {}
        Err(TrySendError::Full(_)) => invalidated.store(true, Ordering::Release),
    }
}

/// 每个应用/书库拥有自己的观察器；回调仅入队，溢出退为全来源失效。
pub struct SourceMonitor {
    watcher: Option<RecommendedWatcher>,
    events: Receiver<Vec<PathBuf>>,
    invalidated: Arc<AtomicBool>,
    watcher_fault: Arc<AtomicBool>,
    watched: BTreeSet<PathBuf>,
    status: MonitorStatus,
    policy: Policy,
    dependencies: HashMap<i64, Vec<PathBuf>>,
    facts: HashMap<(i64, PathBuf), Stamp>,
    external_facts: HashMap<i64, Option<String>>,
    source_cursor: i64,
    dependency_cursors: HashMap<i64, usize>,
    started: Instant,
}

impl Default for SourceMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl SourceMonitor {
    pub fn new() -> Self {
        let (sender, events) = sync_channel(EVENT_CAPACITY);
        let invalidated = Arc::new(AtomicBool::new(false));
        let overflow = invalidated.clone();
        let watcher_fault = Arc::new(AtomicBool::new(false));
        let fault = watcher_fault.clone();
        let watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
            if result.is_err() {
                fault.store(true, Ordering::Release);
            }
            enqueue(&sender, &overflow, result)
        })
        .ok();
        let status = if watcher.is_some() {
            MonitorStatus::Native
        } else {
            MonitorStatus::MetadataOnly
        };
        Self {
            watcher,
            events,
            invalidated,
            watcher_fault,
            watched: BTreeSet::new(),
            status,
            policy: Policy::new(),
            dependencies: HashMap::new(),
            facts: HashMap::new(),
            external_facts: HashMap::new(),
            source_cursor: 0,
            dependency_cursors: HashMap::new(),
            started: Instant::now(),
        }
    }

    pub fn watcher_status(&self) -> MonitorStatus {
        self.status
    }
    /// URI 元数据由平台适配器在后台读取；未知或授权失效用 None。
    pub fn set_external_stamp(&mut self, id: i64, fingerprint: Option<String>) {
        if self
            .external_facts
            .insert(id, fingerprint.clone())
            .is_some_and(|previous| previous != fingerprint)
        {
            self.policy.dirty.insert(id);
        }
    }
    pub fn mark_dirty(&mut self, id: i64) {
        self.policy.dirty.insert(id);
    }

    /// 依赖由 core 在授权范围内解析并转为原件路径；不是阅读快照中的副本路径。
    pub fn set_dependencies(&mut self, source_id: i64, mut paths: Vec<PathBuf>) {
        paths.sort();
        paths.dedup();
        if self
            .dependencies
            .get(&source_id)
            .is_some_and(|old| old != &paths)
        {
            self.policy.dirty.insert(source_id);
        }
        let main = self.source_path(source_id);
        self.facts.retain(|(id, path), _| {
            *id != source_id || main.as_ref() == Some(path) || paths.contains(path)
        });
        self.dependencies.insert(source_id, paths);
    }

    pub fn select(&mut self, sources: &[BookSource], force: bool) -> Vec<i64> {
        let changed: BTreeSet<_> = sources
            .iter()
            .filter(|s| {
                self.policy
                    .sources
                    .get(&s.id)
                    .is_some_and(|old| old.locator != s.locator || old.kind != s.kind)
            })
            .map(|s| s.id)
            .collect();
        self.facts.retain(|(id, _), _| !changed.contains(id));
        self.policy.sync(sources);
        if self.watcher_fault.swap(false, Ordering::AcqRel) {
            self.watcher = None;
            self.watched.clear();
            self.status = MonitorStatus::MetadataOnly;
        }
        self.external_facts
            .retain(|id, _| self.policy.sources.contains_key(id));
        self.dependencies
            .retain(|id, _| self.policy.sources.contains_key(id));
        self.dependency_cursors
            .retain(|id, _| self.policy.sources.contains_key(id));
        self.facts
            .retain(|(id, _), _| self.policy.sources.contains_key(id));
        self.sync_watches();
        if self.invalidated.swap(false, Ordering::AcqRel) {
            self.policy.all_dirty();
        }
        for _ in 0..EVENT_CAPACITY {
            let Ok(paths) = self.events.try_recv() else {
                break;
            };
            self.dirty_paths(&paths);
        }
        self.probe_files();
        self.policy.take(self.started.elapsed(), force)
    }

    fn source_path(&self, id: i64) -> Option<PathBuf> {
        let source = self.policy.sources.get(&id)?;
        if source.kind != "local" || source.locator.contains("://") {
            return None;
        }
        let path = PathBuf::from(&source.locator);
        path.is_absolute().then_some(path)
    }

    fn sync_watches(&mut self) {
        let mut wanted = BTreeSet::new();
        for id in self.policy.sources.keys().copied() {
            for path in self
                .source_path(id)
                .into_iter()
                .chain(self.dependencies.get(&id).into_iter().flatten().cloned())
            {
                if let Some(parent) = path.parent() {
                    wanted.insert(parent.to_path_buf());
                }
            }
        }
        let limited: BTreeSet<_> = wanted.iter().take(WATCH_LIMIT).cloned().collect();
        let Some(watcher) = self.watcher.as_mut() else {
            self.status = MonitorStatus::MetadataOnly;
            return;
        };
        let mut unwatch_failed = false;
        for removed in self.watched.difference(&limited) {
            unwatch_failed |= watcher.unwatch(removed).is_err();
        }
        if unwatch_failed {
            self.watcher = None;
            self.watched.clear();
            self.status = MonitorStatus::MetadataOnly;
            self.policy.all_dirty();
            return;
        }
        self.watched.retain(|path| limited.contains(path));
        let mut failed = false;
        let added: Vec<_> = limited.difference(&self.watched).cloned().collect();
        // 失败的根下一轮重试；状态不将“有 watcher 对象”冒充全部监听成功。
        for path in added {
            // 已知依赖各自监听父目录，不递归遍历用户整个目录树。
            if watcher.watch(&path, RecursiveMode::NonRecursive).is_ok() {
                self.watched.insert(path);
            } else {
                failed = true;
            }
        }
        self.status = if self.watched.is_empty()
            && (!limited.is_empty() || !self.policy.sources.is_empty())
        {
            MonitorStatus::MetadataOnly
        } else if failed || self.watched.len() < wanted.len() {
            MonitorStatus::Partial
        } else {
            MonitorStatus::Native
        };
    }

    fn dirty_paths(&mut self, paths: &[PathBuf]) {
        let affected: Vec<_> = self
            .policy
            .sources
            .keys()
            .copied()
            .filter(|id| {
                self.source_path(*id)
                    .into_iter()
                    .chain(self.dependencies.get(id).into_iter().flatten().cloned())
                    .any(|source| {
                        paths
                            .iter()
                            .any(|event| source == *event || source.starts_with(event))
                    })
            })
            .collect();
        self.policy.dirty.extend(affected);
    }

    fn observe_fact(&mut self, id: i64, path: PathBuf, current: Stamp) {
        if self
            .facts
            .insert((id, path), current.clone())
            .is_some_and(|previous| previous != current)
        {
            self.policy.dirty.insert(id);
        }
    }

    fn probe_files(&mut self) {
        self.probe_files_with(stamp);
    }
    fn probe_files_with(&mut self, mut read: impl FnMut(&Path) -> Stamp) {
        let ids: Vec<_> = self.policy.sources.keys().copied().collect();
        if ids.is_empty() {
            return;
        }
        let start = ids.partition_point(|id| *id <= self.source_cursor) % ids.len();
        let mut file_budget = FILE_PROBES;
        for offset in 0..ids.len().min(SOURCE_PROBES) {
            if file_budget == 0 {
                break;
            }
            let id = ids[(start + offset) % ids.len()];
            self.source_cursor = id;
            if let Some(path) = self.source_path(id) {
                let current = read(&path);
                self.observe_fact(id, path, current);
                file_budget -= 1;
            }
            let dependencies = self.dependencies.get(&id).cloned().unwrap_or_default();
            if dependencies.is_empty() {
                continue;
            }
            let cursor = *self.dependency_cursors.get(&id).unwrap_or(&0) % dependencies.len();
            // 大资源包每轮也只占少数 stat，不能饿死其它来源。
            let count = dependencies.len().min(4).min(file_budget);
            for n in 0..count {
                let path = dependencies[(cursor + n) % dependencies.len()].clone();
                let current = read(&path);
                self.observe_fact(id, path, current);
            }
            self.dependency_cursors
                .insert(id, (cursor + count) % dependencies.len());
            file_budget -= count;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ingest::source_monitor::{
        DISPATCH_LIMIT, FILE_PROBES, Policy, SOURCE_PROBES, SourceMonitor, Stamp, enqueue,
    };
    use crate::store::BookSource;
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::sync_channel;
    use std::time::{Duration, SystemTime};

    fn source(id: i64) -> BookSource {
        BookSource {
            id,
            book_id: id,
            locator: std::env::temp_dir()
                .join("folio-monitor-policy")
                .join(format!("book-{id}.epub"))
                .to_string_lossy()
                .into_owned(),
            kind: "local".into(),
            is_primary: true,
            generation: 0,
            fingerprint: "content".into(),
            state: "available".into(),
        }
    }
    fn monitor() -> SourceMonitor {
        let mut monitor = SourceMonitor::new();
        monitor.watcher = None;
        monitor
    }

    #[test]
    fn unchanged_sources_and_capture_generation_writes_do_not_copy_the_library_every_poll() {
        let mut policy = Policy::new();
        let mut sources = vec![source(1), source(2)];
        policy.sync(&sources);
        assert_eq!(policy.take(Duration::ZERO, false), [1, 2]);
        for round in 1..8 {
            for s in &mut sources {
                s.generation += 2;
            }
            policy.sync(&sources);
            assert!(
                policy
                    .take(Duration::from_secs(round * 15), false)
                    .is_empty()
            );
        }
    }

    #[test]
    fn events_with_identical_stat_and_clock_rollback_or_shrinking_files_trigger_checks() {
        let mut monitor = monitor();
        let sources = [source(1)];
        monitor.policy.sync(&sources);
        monitor.policy.take(Duration::ZERO, false);
        let path = PathBuf::from(&sources[0].locator);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        monitor.observe_fact(1, path.clone(), Stamp::File(100, Some(now)));
        monitor.observe_fact(1, path.clone(), Stamp::File(100, Some(now)));
        assert!(
            monitor
                .policy
                .take(Duration::from_secs(15), false)
                .is_empty()
        );
        monitor.dirty_paths(std::slice::from_ref(&path));
        assert_eq!(monitor.policy.take(Duration::from_secs(30), false), [1]);
        monitor.observe_fact(1, path.clone(), Stamp::File(99, Some(now)));
        assert_eq!(monitor.policy.take(Duration::from_secs(45), false), [1]);
        monitor.observe_fact(1, path, Stamp::File(99, Some(now - Duration::from_secs(1))));
        assert_eq!(monitor.policy.take(Duration::from_secs(60), false), [1]);
    }

    #[test]
    fn bounded_dispatch_keeps_dirty_work_and_audits_rotate_one_source_per_interval() {
        let mut policy = Policy::new();
        let sources: Vec<_> = (1..=20).map(source).collect();
        policy.sync(&sources);
        let mut selected = Vec::new();
        for n in 0..3 {
            let batch = policy.take(Duration::from_secs(n), false);
            assert!(batch.len() <= DISPATCH_LIMIT);
            selected.extend(batch);
        }
        selected.sort();
        assert_eq!(selected, (1..=20).collect::<Vec<_>>());
        assert!(policy.take(Duration::from_secs(119), false).is_empty());
        for n in 1..=20 {
            assert_eq!(policy.take(Duration::from_secs(n * 120), false), [n as i64]);
            assert!(
                policy
                    .take(Duration::from_secs(n * 120 + 1), false)
                    .is_empty()
            );
        }
        policy.all_dirty();
        let batch = policy.take(Duration::from_secs(2401), false);
        assert_eq!(batch.len(), DISPATCH_LIMIT);
        assert_eq!(policy.dirty.len(), 12);
    }

    #[test]
    fn dependency_changes_reconnection_and_removal_remain_local_to_the_source() {
        let mut monitor = monitor();
        let mut sources = vec![source(1), source(2)];
        monitor.policy.sync(&sources);
        monitor.policy.take(Duration::ZERO, false);
        let image = PathBuf::from("/sources/pictures/cover.png");
        monitor.set_dependencies(1, vec![image.clone()]);
        monitor.dirty_paths(&[image]);
        assert_eq!(monitor.policy.take(Duration::from_secs(1), false), [1]);
        sources[1].state = "missing".into();
        monitor.policy.sync(&sources);
        monitor.policy.take(Duration::from_secs(2), false);
        sources[1].state = "available".into();
        monitor.policy.sync(&sources);
        assert_eq!(monitor.policy.take(Duration::from_secs(3), false), [2]);
        sources[0].state = "suppressed".into();
        monitor.policy.sync(&sources);
        monitor.policy.all_dirty();
        assert_eq!(monitor.policy.take(Duration::from_secs(4), false), [2]);
    }

    #[test]
    fn event_queue_overflow_invalidates_all_sources_without_blocking_the_callback() {
        let (sender, receiver) = sync_channel(1);
        let invalidated = AtomicBool::new(false);
        let event = notify::Event::new(notify::EventKind::Any).add_path(PathBuf::from("changed"));
        enqueue(&sender, &invalidated, Ok(event.clone()));
        enqueue(&sender, &invalidated, Ok(event));
        assert!(invalidated.load(Ordering::Acquire));
        assert!(receiver.try_recv().is_ok());
        assert!(receiver.try_recv().is_err());
        let mut monitor = monitor();
        let sources: Vec<_> = (1..=20).map(source).collect();
        monitor.policy.sync(&sources);
        while !monitor.policy.take(Duration::ZERO, false).is_empty() {}
        monitor.policy.all_dirty();
        let selected = monitor.policy.take(Duration::from_secs(1), false);
        assert_eq!(selected.len(), DISPATCH_LIMIT);
        assert_eq!(monitor.policy.dirty.len(), 12);
    }

    #[test]
    fn metadata_probes_are_bounded_and_rotate_without_rechecking_unchanged_content() {
        let mut monitor = monitor();
        let sources: Vec<_> = (1..=(SOURCE_PROBES * 3) as i64).map(source).collect();
        monitor.policy.sync(&sources);
        while !monitor.policy.take(Duration::ZERO, false).is_empty() {}
        let mut visited = BTreeSet::new();
        for _ in 0..3 {
            let mut probes = 0;
            monitor.probe_files_with(|path| {
                probes += 1;
                visited.insert(path.to_path_buf());
                Stamp::File(10, Some(SystemTime::UNIX_EPOCH))
            });
            assert!(probes <= FILE_PROBES);
            assert!(
                monitor
                    .policy
                    .take(Duration::from_secs(15), false)
                    .is_empty()
            );
        }
        assert_eq!(visited.len(), sources.len());
    }

    #[test]
    fn uri_metadata_changes_and_permission_recovery_queue_checks_without_path_probing() {
        let mut monitor = monitor();
        let mut source = source(1);
        source.kind = "android-document".into();
        source.locator = "content://provider/document/book".into();
        monitor.policy.sync(&[source]);
        monitor.policy.take(Duration::ZERO, false);
        assert!(monitor.source_path(1).is_none());
        monitor.set_external_stamp(1, Some("10:20".into()));
        monitor.set_external_stamp(1, Some("10:20".into()));
        assert!(
            monitor
                .policy
                .take(Duration::from_secs(15), false)
                .is_empty()
        );
        monitor.set_external_stamp(1, Some("9:19".into()));
        assert_eq!(monitor.policy.take(Duration::from_secs(30), false), [1]);
        monitor.set_external_stamp(1, None);
        assert_eq!(monitor.policy.take(Duration::from_secs(45), false), [1]);
        monitor.set_external_stamp(1, Some("9:19".into()));
        assert_eq!(monitor.policy.take(Duration::from_secs(60), false), [1]);
    }
}
