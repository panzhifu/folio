//! 文件夹监听：盯住一个「收件箱」目录，新出现的书自动进库。
//!
//! 形状是「**事件进、批出**」：[`notify`]（Linux 上是 inotify）把文件系统事件交给
//! 一条专用线程，[`Collector`] 把事件攒成候选 —— 大文件要拷好几百毫秒甚至更久，
//! inotify 在拷贝过程中会连珠炮一样地报 MODIFY，所以放行判据是**两轮 tick 尺寸一致**
//! （拷完了）加**库里还没有**（`books.path` 按副本路径判，与扫描同一套）。
//! 攒出一批就包成 [`crate::import::PathsSource`]（kind=Watch）提交进导入队列，
//! 界面那行读数照常出现。
//!
//! 初扫与监听共用 [`crate::scan::find`]：服务一起就把目录里**现有的**书导进来，
//! 之后才是事件。兜底是 [`notify::PollWatcher`] —— 没有系统级监听 API 的平台
//! （iOS 的沙盒、部分鸿蒙卷）照样能跑，只是延迟到 `poll_interval`。
//!
//! 本模块在 core、不依赖 gpui；启停的编排（读配置、活过界面切换）在 app 侧
//! `ui::shared::watch`。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::import::{ImportKind, ImportReport, Imported, PathsSource, forward, submit_import};
use crate::scan;
use crate::store::Store;
use crate::task::{StageCtx, TaskEvent, TaskManager};

/// 相邻两轮 tick 的间隔，也是「尺寸稳定」的判据窗口。1 秒：本地拷贝早已结束，
/// 网络盘的大文件拖过几轮也只会晚几秒入库。
const TICK: Duration = Duration::from_millis(1000);

/// 监听服务的配置。
#[derive(Debug, Clone)]
pub struct WatchConfig {
    /// 盯住的目录。与书库目录相同没有意义（导进来的就是从那里拷的），配置层会挡。
    pub dir: PathBuf,
    /// 兜底轮询间隔（`PollWatcher` 用）。事件驱动的平台上它不起作用。
    pub poll_interval: Duration,
    /// 递归深度上限 —— 与 [`crate::scan`] 的深度语义同一套。
    pub max_depth: usize,
}

/// 一条活着的监听。`stop()` 收线程、收 watcher；`Drop` 兜底。
pub struct WatchService {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl WatchService {
    /// 起一条监听。服务的职责到「把批次提交进 `queue`」为止 —— 落库在管线里，
    /// 进度与结果由提交出去的那条作业自己回流（app 侧负责把事件接上界面）。
    pub fn start(
        config: WatchConfig,
        store: Arc<Store>,
        queue: Arc<TaskManager>,
        dest_dir: PathBuf,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("folio-watch".to_string())
            .spawn(move || run(config, dest_dir, store, queue, thread_stop))
            .expect("起监听线程");
        Self {
            stop,
            thread: Some(thread),
        }
    }

    /// 停下并等线程退出。阻塞 —— 但监听线程每轮 tick 都醒，等它最多一秒。
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for WatchService {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(
    config: WatchConfig,
    dest_dir: PathBuf,
    store: Arc<Store>,
    queue: Arc<TaskManager>,
    stop: Arc<AtomicBool>,
) {
    // 先初扫：目录里现有的书先进库（跳过判定开着，已有的不会动）。
    // 初扫没有要看进度的人，detached 读数口把读数丢掉。
    let limits = scan::Limits {
        max_depth: config.max_depth,
        ..scan::Limits::default()
    };
    let initial = scan::find_with(
        std::slice::from_ref(&config.dir),
        std::slice::from_ref(&dest_dir),
        &[],
        limits,
        &StageCtx::detached(),
    );
    if !initial.candidates.is_empty() {
        tracing::info!("watch: 初扫 {} 本", initial.candidates.len());
        submit_batch(&paths_of(&initial.candidates), &dest_dir, &store, &queue);
    }

    // watcher 就挂在这条线程上：它自己的内部线程负责把事件推进 `event_tx`，
    // 这条线程只管超时地收 —— 超时醒来就是一轮 tick。服务收摊时 `watcher`
    // 随作用域 drop，监听注销，不用单独收。
    let (event_tx, event_rx) = std::sync::mpsc::channel();
    let mut watcher = match notify::recommended_watcher(event_tx.clone()) {
        Ok(watcher) => Box::new(watcher) as Box<dyn notify::Watcher>,
        Err(err) => {
            tracing::info!("watch: 系统级监听不可用（{err}），退回轮询");
            let poll = notify::Config::default().with_poll_interval(config.poll_interval);
            Box::new(notify::PollWatcher::new(event_tx, poll).expect("轮询 watcher 不该起不来"))
                as Box<dyn notify::Watcher>
        }
    };
    if let Err(err) = watcher.watch(&config.dir, notify::RecursiveMode::Recursive) {
        tracing::error!("watch: 挂不上 {}：{err:#} —— 本轮只靠初扫", config.dir.display());
    }

    let mut collector = Collector::new(config.dir.clone(), dest_dir.clone(), config.max_depth);
    while !stop.load(Ordering::SeqCst) {
        match event_rx.recv_timeout(TICK) {
            Ok(Ok(event)) => collector.observe(event.paths),
            Ok(Err(err)) => {
                // watcher 报错（目录被删、句柄失效）：记一条日志，本轮照走 ——
                // 目录回来时事件会再来，就算不来，服务重启那一路也会补上。
                tracing::warn!("watch: 文件系统事件读不出来：{err}");
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
        let batch = collector.tick(&store);
        if !batch.is_empty() {
            tracing::info!("watch: 攒出一批 {} 本", batch.len());
            submit_batch(&batch, &dest_dir, &store, &queue);
        }
    }
}

/// 候选 → 本地路径（监听只交本地候选）。
fn paths_of(candidates: &[crate::import::Candidate]) -> Vec<PathBuf> {
    candidates
        .iter()
        .filter_map(|c| match &c.origin {
            crate::import::Origin::Local(path) => Some(path.clone()),
            _ => None,
        })
        .collect()
}

/// 把一批路径包成 Watch 语义的导入作业：库里已有就跳过，同名不同大小落 `-2`。
fn submit_batch(
    paths: &[PathBuf],
    dest_dir: &Path,
    store: &Arc<Store>,
    queue: &TaskManager,
) {
    // 只报「这是监听交的」：重扫同一本由管线的哈希判重认出来（`Imported::Duplicate`），
    // 副本名由内容决定，所以旧形状那份 `Policy`（skip_existing / 后缀改名）在这里没用了。
    let source = PathsSource::new(paths.to_vec(), ImportKind::Watch);
    let handle = submit_import(queue, Box::new(source), dest_dir.to_path_buf(), store.clone());
    // 事件不回流（服务没有界面可接）：作业的成败都在日志里。
    forward(handle, |event| match event {
        TaskEvent::Finished(Ok(outcomes)) => {
            for outcome in &outcomes {
                match &outcome.result {
                    Ok(Imported::Added(registered)) => tracing::info!(
                        "watch: 自动入库《{}》（第 {} 本）",
                        registered.title,
                        registered.book_id
                    ),
                    // 同一本书再出现在收件箱里不是新闻：判重认得它，也没再落一份副本。
                    Ok(Imported::Duplicate { .. }) => tracing::debug!(
                        "watch: 《{}》与库里某一本同内容，没收下第二份",
                        outcome.candidate.name
                    ),
                    Err(err) => {
                        tracing::warn!("watch: 自动导入《{}》失败：{err:#}", outcome.candidate.name)
                    }
                }
            }
            // 一批一句总结。自动导入不打扰读者，但日志里要查得出这批是怎么结清的
            // （`plans/基础设施.md` §3.2：批量作业要出报表）。
            tracing::info!(
                "watch: 一批 {} 本结清 —— {}",
                outcomes.len(),
                ImportReport::tally(&outcomes).summary()
            );
        }
        TaskEvent::Finished(Err(err)) => {
            tracing::warn!("watch: 自动导入作业失败：{err:#}");
        }
        _ => {}
    });
}

/// 把文件系统事件攒成「尺寸稳定且没入库」的批次。
///
/// 独立成一个类型是为了能直接测：不需要真 watcher，喂路径、拨 tick 就能钉住
/// 「半拷贝不进批、拷完进批、库里的不进批」这三条。
pub(crate) struct Collector {
    dir: PathBuf,
    dest_dir: PathBuf,
    max_depth: usize,
    /// 上一次 tick 看到的大小。`size` 与它相等才算稳定；事件里出现过、后来
    /// 消失的条目会被清掉。
    seen: HashMap<PathBuf, u64>,
}

impl Collector {
    pub(crate) fn new(dir: PathBuf, dest_dir: PathBuf, max_depth: usize) -> Self {
        Self {
            dir,
            dest_dir,
            max_depth,
            seen: HashMap::new(),
        }
    }

    /// 一条文件系统事件带来的路径。不在这里判文件/目录 —— 判不了的（正被拷的
    /// 半个文件）留给 tick。
    pub(crate) fn observe(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        for path in paths {
            if self.eligible(&path) {
                self.seen.entry(path).or_insert(0);
            }
        }
    }

    /// 一轮 tick：与上一轮尺寸一致、`dest_dir` 里还没这本书的进批；
    /// 其他的更新尺寸（还在长）或清掉（没了）。
    pub(crate) fn tick(&mut self, store: &Store) -> Vec<PathBuf> {
        let mut ready = Vec::new();
        let mut next = HashMap::new();
        let entries: Vec<(PathBuf, u64)> = self.seen.drain().collect();
        for (path, seen_size) in entries {
            let current = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            if current == 0 && seen_size == 0 {
                continue; // 没了（或还没出现），不留在账上
            }
            if current == seen_size && current > 0 && !self.in_library(&path, store) {
                ready.push(path);
            } else {
                next.insert(path, current);
            }
        }
        self.seen = next;
        ready
    }

    /// 这份路径要不要进账：是 `dir` 底下（深度内）的书文件。
    fn eligible(&self, path: &Path) -> bool {
        let Ok(rest) = path.strip_prefix(&self.dir) else {
            return false;
        };
        let depth = rest.components().count();
        if depth == 0 || depth > self.max_depth {
            return false;
        }
        match path.extension().and_then(|e| e.to_str()) {
            Some(ext) => crate::format::supports_extension(ext),
            None => false,
        }
    }

    /// 副本路径已在库里 —— 与管线的 skip_existing 同一条判据。
    fn in_library(&self, path: &Path, store: &Store) -> bool {
        let dest = self.dest_dir.join(
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
        store
            .book_by_path(&dest)
            .map(|row| row.is_some())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::{Collector, WatchConfig, WatchService};
    use crate::store::Store;
    use crate::task::TaskManager;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        path
    }

    use std::path::Path;
    use std::path::PathBuf;

    /// 半拷贝不进批；两轮尺寸一致才进；库里的不进。
    #[test]
    fn collector_releases_only_stable_unregistered_files() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox = tmp.path().join("收件箱");
        let dest = tmp.path().join("books");
        let store = Store::in_memory().unwrap();

        let fresh = write(&inbox, "新书.txt", "正文");
        // 一本「已在库里」的：先按书库路径造一行。
        let known_copy = dest.join("已有的.txt");
        let known_source = write(&inbox, "已有的.txt", "库里那份\n");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(&known_copy, "库里那份\n").unwrap();
        store
            .add_book(&known_copy, &crate::format::DocMeta {
                title: "已有的".into(),
                authors: vec![],
                format: crate::format::Format::Text,
                size: 5,
            })
            .unwrap();

        let mut collector = Collector::new(inbox.clone(), dest, 3);
        collector.observe([fresh.clone(), known_source]);

        // 第一轮：记尺寸，不放行。
        assert!(collector.tick(&store).is_empty(), "只见过一轮，还不算稳定");
        // 第二轮：尺寸没变 —— 新书进批，库里的不进。
        let ready = collector.tick(&store);
        assert_eq!(ready, vec![fresh.clone()], "稳定且没入库的才放行");

        // 半拷贝：事件来了、每轮都在长 —— 永远不放行。
        let growing = write(&inbox, "在长.txt", "一半");
        collector.observe([growing.clone()]);
        assert!(collector.tick(&store).is_empty());
        std::fs::write(&growing, "一半多一点").unwrap();
        assert!(collector.tick(&store).is_empty(), "还在长的不放行");
        // 拷完了：两轮一致，放行。
        assert_eq!(collector.tick(&store), vec![growing.clone()]);
    }

    /// 深度外与不认识的扩展名不进账。
    #[test]
    fn collector_ignores_out_of_depth_and_unknown_extensions() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox = tmp.path().join("收件箱");
        let dest = tmp.path().join("books");
        let store = Store::in_memory().unwrap();
        let deep = write(&inbox, "a/b/c/d/e/太深.txt", "正文");
        let unknown = write(&inbox, "不是书.xyz", "正文");

        let mut collector = Collector::new(inbox, dest, 3);
        collector.observe([deep, unknown]);
        assert!(collector.tick(&store).is_empty());
    }

    /// 真 watcher 的端到端：服务起在临时目录上，往里放一本书，
    /// 架上（内存库）要自己多出那一本。事件驱动 + 尺寸稳定判据全在链上。
    #[test]
    fn the_service_imports_new_files_automatically() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox = tmp.path().join("收件箱");
        std::fs::create_dir_all(&inbox).unwrap();
        let dest = tmp.path().join("books");
        let store = Arc::new(Store::in_memory().unwrap());
        let queue = Arc::new(TaskManager::new(1));

        let service = WatchService::start(
            WatchConfig {
                dir: inbox.clone(),
                poll_interval: Duration::from_millis(200),
                max_depth: 3,
            },
            store.clone(),
            queue,
            dest,
        );

        // 等服务起来（初扫 + watcher 挂上）再放书：避免初扫把它当成存量。
        std::thread::sleep(Duration::from_millis(300));
        let book = inbox.join("空降的书.txt");
        std::fs::write(&book, "正文\n").unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while store.shelf("").unwrap().is_empty() {
            assert!(
                std::time::Instant::now() < deadline,
                "10 秒内该自动入库（事件 → 攒批 → 管线）"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        let shelf = store.shelf("").unwrap();
        assert_eq!(shelf.len(), 1);
        assert!(
            store.load_flow(shelf[0].id).unwrap().is_some(),
            "章节要跟着落库"
        );
        service.stop();
    }
}
