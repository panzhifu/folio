//! 文件夹监听：盯住一个「收件箱」目录，新出现的书自动进库。
//!
//! 形状是「**事件进、批出**」：[`notify`]（Linux 上是 inotify）把文件系统事件交给
//! 一条专用线程，[`Collector`] 把事件攒成候选 —— 大文件要拷好几百毫秒甚至更久，
//! inotify 在拷贝过程中会连珠炮一样地报 MODIFY，所以放行判据是**两轮 tick 尺寸一致**
//! （拷完了）加**库里还没有**（`books.path` 按副本路径判，与扫描同一套）。
//! 攒出一批就包成 [`crate::ingest::import::PathsSource`]（kind=Watch）提交进导入队列，
//! 界面那行读数照常出现。
//!
//! 初扫与监听共用 [`crate::ingest::scan::find`]：服务一起就把目录里**现有的**书导进来，
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

use crate::infra::task::{StageCtx, TaskEvent, TaskManager};
use crate::ingest::import::{
    ImportKind, ImportOutcome, ImportReport, Imported, PathsSource, forward, submit_import,
};
use crate::ingest::scan;
use crate::store::Store;

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
    /// 递归深度上限 —— 与 [`crate::ingest::scan`] 的深度语义同一套。
    pub max_depth: usize,
}

/// 监听作业的事件往哪儿再送一份。
///
/// 日志由服务自己记全（谁入库了、哪本被拒了、这批怎么结清的），但**界面读数不在
/// 日志里**：app 侧要让书架跟着自动导入刷新，就把这条口传进来 —— 事件在记完日志
/// 之后按所有权交出，接的一方爱转成通道还是爱弹 toast 都随它。
///
/// `None` = 没人接（测试、命令行、架子没开）。日志照记，行为与这条口存在时一致。
pub type WatchEvents = dyn Fn(TaskEvent<Vec<ImportOutcome>>) + Send + Sync;

/// 一条活着的监听。`stop()` 收线程、收 watcher；`Drop` 兜底。
pub struct WatchService {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl WatchService {
    /// 起一条监听。服务的职责到「把批次提交进 `queue`」为止 —— 落库在管线里，
    /// 进度与结果由提交出去的那条作业自己回流（接去哪儿看 `on_event`）。
    pub fn start(
        config: WatchConfig,
        store: Arc<Store>,
        queue: Arc<TaskManager>,
        dest_dir: PathBuf,
        on_event: Option<Arc<WatchEvents>>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("folio-watch".to_string())
            .spawn(move || run(config, dest_dir, store, queue, thread_stop, on_event))
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
    on_event: Option<Arc<WatchEvents>>,
) {
    // 先初扫：目录里现有的书交一次管线（库里已有的由哈希判重认下，不会多一行）。
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
        submit_batch(
            &paths_of(&initial.candidates),
            &dest_dir,
            &store,
            &queue,
            on_event.as_ref(),
        );
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
        tracing::error!(
            "watch: 挂不上 {}：{err:#} —— 本轮只靠初扫",
            config.dir.display()
        );
    }

    let mut collector = Collector::new(config.dir.clone(), config.max_depth);
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
        let batch = collector.tick();
        if !batch.is_empty() {
            tracing::info!("watch: 攒出一批 {} 本", batch.len());
            submit_batch(&batch, &dest_dir, &store, &queue, on_event.as_ref());
        }
    }
}

/// 候选 → 本地路径（监听只交本地候选）。
fn paths_of(candidates: &[crate::ingest::import::Candidate]) -> Vec<PathBuf> {
    candidates
        .iter()
        .filter_map(|c| match &c.origin {
            crate::ingest::import::Origin::Local(path) => Some(path.clone()),
            _ => None,
        })
        .collect()
}

/// 把一批路径包成 Watch 语义的导入作业：库里已有就跳过，同名不同大小落 `-2`。
///
/// `on_event` 是界面侧的读数口（见 [`WatchEvents`]）：日志先记，事件再交出所有权。
fn submit_batch(
    paths: &[PathBuf],
    dest_dir: &Path,
    store: &Arc<Store>,
    queue: &TaskManager,
    on_event: Option<&Arc<WatchEvents>>,
) {
    // 只报「这是监听交的」：重扫同一本由管线的哈希判重认出来（`Imported::Duplicate`），
    // 副本名由内容决定，所以旧形状那份 `Policy`（skip_existing / 后缀改名）在这里没用了。
    let source = PathsSource::new(paths.to_vec(), ImportKind::Watch);
    let handle = submit_import(
        queue,
        Box::new(source),
        dest_dir.to_path_buf(),
        store.clone(),
    );
    let sink = on_event.cloned();
    forward(handle, move |event| {
        log_event(&event);
        if let Some(sink) = &sink {
            sink(event);
        }
    });
}

/// 一批监听导入的日志。与「有没有人接事件」无关，所以单独成一个函数。
fn log_event(event: &TaskEvent<Vec<ImportOutcome>>) {
    match event {
        TaskEvent::Finished(Ok(outcomes)) => {
            for outcome in outcomes {
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
                ImportReport::tally(outcomes).summary()
            );
        }
        TaskEvent::Finished(Err(err)) => {
            tracing::warn!("watch: 自动导入作业失败：{err:#}");
        }
        _ => {}
    }
}

/// 一本文件在这一轮的身份：尺寸 + 最后修改时间。
///
/// 为什么不能只认尺寸：实测过一本 7.2 MB 的书在被管线自己读的那二十秒里，
/// `Collector` 交了它 43 次，而同一目录里一个 91 字节的稳定文件只交一次 ——
/// 差别不在事件多，而在「读到的尺寸变了样」被当成了「内容换了」。而**读**改不动
/// `mtime`：写才改。所以判「这是另一本」要两者一起看，单靠尺寸会把噪声放大成回环。
///
/// `mtime` 取不到（某些 FUSE 不给）时退回只看尺寸，行为不比原来差。
type Stamp = (u64, Option<std::time::SystemTime>);

/// 现在这一本的身份。读不到、或是个空文件 → `None`（空文件不值得交回管线）。
fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    let size = meta.len();
    if size == 0 {
        return None;
    }
    Some((size, meta.modified().ok()))
}

/// 这一身份值不值得再交回管线。
///
/// 只有两件事算「新的一步」：**长大了**（还在拷的那一本又多了字节 —— 上一次交出去
/// 的是半个文件，正该重试）与**写得更晚**（读者真覆盖了那一本）。
/// 反过来，读数**退回**去（重 IO 之下 `stat` 报出更小的尺寸）或原样重复都不算 ——
/// 那是一本正在导入的书被反复喂回管线的来源，实测过 24 秒里再交八次。
fn worth_handing(handed: Option<&Stamp>, now: &Stamp) -> bool {
    let Some((handed_size, handed_mtime)) = handed else {
        return true;
    };
    if now.0 > *handed_size {
        return true;
    }
    match (now.1, handed_mtime) {
        (Some(now), Some(handed)) => now > *handed,
        // mtime 拿不到（某些 FUSE 不给）：退回只看尺寸，不比交过的那次大就不再交。
        _ => false,
    }
}

/// 把文件系统事件攒成「身份稳定且这一身份没交过」的批次。
///
/// 独立成一个类型是为了能直接测：不需要真 watcher，喂路径、拨 tick 就能钉住
/// 「半拷贝不进批、拷完进批、交过的不再交」这三条。
///
/// 「已经入库」这一格**不在这儿判**：副本按内容哈希落地（`import` 那一路的
/// `hash.file_name`），源文件名在书库里根本不存在，按名字查库永远查不到 ——
/// 那条判据曾经就是这样让监听器把同一本反复交回管线的。库里有没有这本，
/// 由管线自己的哈希判重说（`Imported::Duplicate`），它比这里准且只要付一次钱。
pub(crate) struct Collector {
    dir: PathBuf,
    max_depth: usize,
    /// 上一轮看到的身份。`None` = 还没见过（或那会儿读不到）；与这一轮相等才算稳定。
    seen: HashMap<PathBuf, Option<Stamp>>,
    /// 已经交回管线的那一本，以及交出去时的身份。身份变了才算另一本（读者覆盖了
    /// 同名文件），否则同一个事件会把它反复喂回管线。
    submitted: HashMap<PathBuf, Stamp>,
}

impl Collector {
    pub(crate) fn new(dir: PathBuf, max_depth: usize) -> Self {
        Self {
            dir,
            max_depth,
            seen: HashMap::new(),
            submitted: HashMap::new(),
        }
    }

    /// 一条文件系统事件带来的路径。不在这里判文件/目录 —— 判不了的（正被拷的
    /// 半个文件）留给 tick。
    pub(crate) fn observe(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        for path in paths {
            if self.eligible(&path) {
                self.seen.entry(path).or_insert(None);
            }
        }
    }

    /// 一轮 tick：身份与上一轮一致、且这一身份**没交过、也不是退回上一步**的进批；
    /// 还在变的更新读数，交过的从账上摘掉（等下一个事件再武装）。
    pub(crate) fn tick(&mut self) -> Vec<PathBuf> {
        let mut ready = Vec::new();
        let mut next = HashMap::new();
        let entries: Vec<(PathBuf, Option<Stamp>)> = self.seen.drain().collect();
        for (path, previous) in entries {
            let current = stamp(&path);
            if current.is_none() && previous.is_none() {
                continue; // 没了（或一直读不到），不留在账上
            }
            if current != previous {
                next.insert(path, current); // 第一次见、或还在长：下一轮再判
                continue;
            }
            let Some(now) = current else { continue };
            // 身份稳住了，而这一身份值得交 —— 才进批。交过的从账上摘掉。
            if worth_handing(self.submitted.get(&path), &now) {
                self.submitted.insert(path.clone(), now);
                ready.push(path);
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
        crate::content::format::looks_like_a_book(path)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::{Collector, WatchConfig, WatchEvents, WatchService};
    use crate::infra::task::{TaskEvent, TaskManager};
    use crate::ingest::import::Imported;
    use crate::store::Store;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        path
    }

    use std::path::Path;
    use std::path::PathBuf;

    /// 半拷贝不进批；两轮尺寸一致才进；**进过一次就不再进**。
    ///
    /// 最后那一条是监听器不再自激的关键：副本按内容哈希落地，按源文件名查库
    /// 永远查不到，于是旧判据判不出「已入库」，同一本被反复交回管线
    /// （真机实测每秒十几轮，直到那本被移出收件箱）。
    #[test]
    fn collector_releases_a_stable_file_once_and_never_again() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox = tmp.path().join("收件箱");

        let fresh = write(&inbox, "新书.txt", "正文");
        let mut collector = Collector::new(inbox.clone(), 3);
        collector.observe([fresh.clone()]);

        // 第一轮：记尺寸，不放行。
        assert!(collector.tick().is_empty(), "只见过一轮，还不算稳定");
        // 第二轮：尺寸没变 —— 放行。
        assert_eq!(collector.tick(), vec![fresh.clone()], "稳定了的才放行");

        // 交过之后事件再来（真 watcher 就是会反复报同一本）：不许再交。
        collector.observe([fresh.clone()]);
        assert!(collector.tick().is_empty(), "重新见到：先记一轮尺寸");
        assert!(
            collector.tick().is_empty(),
            "尺寸没变、又已经交过 —— 不该再喂回管线"
        );

        // 读者改了那一本：尺寸变了，重新武装、再交一次。
        std::fs::write(&fresh, "正文加了一节").unwrap();
        collector.observe([fresh.clone()]);
        assert!(collector.tick().is_empty(), "先看一轮新尺寸");
        assert_eq!(collector.tick(), vec![fresh.clone()], "内容变了该再交一次");

        // 半拷贝：事件来了、每轮都在长 —— 永远不放行。
        let growing = write(&inbox, "在长.txt", "一半");
        collector.observe([growing.clone()]);
        assert!(collector.tick().is_empty());
        std::fs::write(&growing, "一半多一点").unwrap();
        assert!(collector.tick().is_empty(), "还在长的不放行");
        // 拷完了：两轮一致，放行。
        assert_eq!(collector.tick(), vec![growing.clone()]);
    }

    /// 同名覆盖成**同样大小**的另一本：尺寸没变，靠 `mtime` 认出来，该再交一次。
    ///
    /// 这一条钉的是身份里 `mtime` 那一半。只看尺寸时这种覆盖会被当成没变（漏收），
    /// 而反过来把读数抖动当成变了就是回环 —— 两头都要靠这个判据分开。
    #[test]
    fn a_same_size_overwrite_is_a_new_book_by_its_mtime() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox = tmp.path().join("收件箱");
        let path = write(&inbox, "同名覆盖.txt", "第一本正文！！");
        let mut collector = Collector::new(inbox, 3);
        collector.observe([path.clone()]);
        assert!(collector.tick().is_empty(), "只见过一轮，还不算稳定");
        assert_eq!(collector.tick(), vec![path.clone()], "稳定了先交一次");

        // 同样长度、不同内容，并把 mtime 明确往前推一格。
        std::fs::write(&path, "换了的另一本！").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(4);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(later))
            .unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            21,
            "尺寸得一样才考得出 mtime"
        );

        collector.observe([path.clone()]);
        assert!(collector.tick().is_empty(), "先记一轮新身份");
        assert_eq!(collector.tick(), vec![path.clone()], "mtime 变了就是另一本");
    }

    /// 读数**退回**（尺寸变小而 mtime 没变）不算另一本：那是重 IO 下的噪声。
    ///
    /// 这一条钉住回环的另一半 —— 只认「身份变了」时，一次读数抖动就会再多交一整本
    /// （实测：24 秒的导入里同一本被交了八次）。长大才重试，抖动不重试。
    #[test]
    fn a_shrinking_reading_does_not_re_hand_the_same_book() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox = tmp.path().join("收件箱");
        let path = write(&inbox, "读数会抖.txt", "一整本正文内容");
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();

        let mut collector = Collector::new(inbox, 3);
        collector.observe([path.clone()]);
        assert!(collector.tick().is_empty(), "只见过一轮，还不算稳定");
        assert_eq!(collector.tick(), vec![path.clone()], "稳定了交一次");

        // 截短，并把 mtime 按回原值：尺寸退了、时间没新 —— 不该再交。
        std::fs::write(&path, "一整本").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(mtime))
            .unwrap();
        collector.observe([path.clone()]);
        assert!(collector.tick().is_empty(), "先记一轮新读数");
        assert!(
            collector.tick().is_empty(),
            "读数退回而 mtime 没变 —— 不该再交"
        );

        // 而它要是又长回去（甚至更长），上次那个半个文件就该重试。
        std::fs::write(&path, "一整本正文内容补了一节").unwrap();
        collector.observe([path.clone()]);
        assert!(collector.tick().is_empty(), "先记一轮新尺寸");
        assert_eq!(collector.tick(), vec![path.clone()], "长大了该再交一次");
    }

    /// 深度外与不认识的扩展名不进账。
    #[test]
    fn collector_ignores_out_of_depth_and_unknown_extensions() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox = tmp.path().join("收件箱");
        let deep = write(&inbox, "a/b/c/d/e/太深.txt", "正文");
        let unknown = write(&inbox, "不是书.xyz", "正文");

        let mut collector = Collector::new(inbox, 3);
        collector.observe([deep, unknown]);
        assert!(collector.tick().is_empty());
    }

    /// 监听把作业事件**交出去** —— 界面靠这一条才知道架子要重读。
    ///
    /// 这条是「监听导入不回流界面」那个 bug 的护栏：日志一直有，缺的只是把事件递给
    /// 接的那一方。把 `submit_batch` 退回只记日志时，这里 10 秒收不到东西。
    #[test]
    fn the_service_hands_the_batch_events_to_the_sink() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox = tmp.path().join("收件箱");
        std::fs::create_dir_all(&inbox).unwrap();
        let store = Arc::new(Store::in_memory().unwrap());

        let (tx, rx) = std::sync::mpsc::channel();
        let sink: Arc<WatchEvents> = Arc::new(move |event| {
            if matches!(event, TaskEvent::Finished(Ok(_))) {
                let _ = tx.send(event);
            }
        });
        let service = WatchService::start(
            WatchConfig {
                dir: inbox.clone(),
                poll_interval: Duration::from_millis(200),
                max_depth: 3,
            },
            store,
            Arc::new(TaskManager::new(1)),
            tmp.path().join("books"),
            Some(sink),
        );

        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(inbox.join("交出去的书.txt"), "正文\n").unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let got = loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(event) => break Some(event),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "10 秒内该把结清事件交给挂上去的那条口"
                    );
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break None,
            }
        };
        let Some(TaskEvent::Finished(Ok(outcomes))) = got else {
            panic!("接到的该是一批结清的事件");
        };
        assert!(
            matches!(outcomes[0].result, Ok(Imported::Added(_))),
            "那本该是新增入库"
        );
        service.stop();
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
            None,
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
