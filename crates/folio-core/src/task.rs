//! 后台任务管线：把「可取消、可报进度」的作业丢到后台线程上跑，事件经通道回流。
//!
//! 只依赖 `std`（`thread` + `mpsc` + `Condvar`）—— 与 `folio-core` 其余部分一致，
//! 不引 tokio / crossbeam，也不依赖 gpui。界面侧收到 [`TaskEvent`] 后自己决定怎么画。
//!
//! 形状是「**一个作业一条通道**」：每次 [`TaskManager::submit`] 都新开一条 `mpsc` 通道，
//! 事件不共用、不带 id 路由 —— 交出去的那个 [`TaskHandle`] 就是这条作业的全部接口
//! （看进度、取消、等在完成上）。
//!
//! 取消沿用远程下载那个标志 [`Cancel`](crate::remote::Cancel)：它是「置位、由作业在
//! 检查点自己看见」的协作式取消，不是抢占，所以最多晚一个检查点生效。作业应当把
//! `cancel.triggered()` 当作「尽快返回」的信号，取消的最终定性由本模块给出：
//! 作业返回时若 `cancel` 已置位，一律落成 [`TaskEvent::Cancelled`]，哪怕它其实跑完了。

use std::collections::VecDeque;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

pub use crate::remote::Cancel;

/// 作业的失败原因。用装箱错误而不是某个具体枚举：core 这一层不该规定上层怎么建模错误，
/// 而 `anyhow::Error` 本身就是这个类型，界面侧收成因果链时不用再包一层。
pub type TaskError = Box<dyn std::error::Error + Send + Sync>;

/// 作业的身份，日志与「这套管理器上跑过几件事」用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TaskId(pub u64);

/// 一条进度读数。`total == 0` 表示**不定进度**（只知道在跑，不知道还剩多少）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressEvent {
    pub done: u64,
    pub total: u64,
    pub message: String,
}

/// 字节级进度的一条读数：下载与拷贝用。`total == None` 表示总大小未知
/// （服务器没给 `Content-Length` 的那种），界面要画不定进度。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteProgress {
    pub done: u64,
    pub total: Option<u64>,
    /// 正在传的那个东西的名字（「《某本》 41%」里的《某本》）。core 不组文案，
    /// 只交事实 —— 屏幕上的一句话由界面按 locale 组（core 的字符串只进日志）。
    pub name: String,
}

/// 后台作业流回界面的事件。
#[derive(Debug)]
pub enum TaskEvent<R> {
    /// 被工作线程取到、开始跑（排队阶段没有事件）。
    Started,
    Progress(ProgressEvent),
    /// 字节级读数（下载 / 拷贝）。与 [`TaskEvent::Progress`] 是两级独立的读数，
    /// 各画各的，不互相覆盖。
    Bytes(ByteProgress),
    /// 跑完了（成功或失败）。
    Finished(Result<R, TaskError>),
    /// 被取消（见模块文档：作业返回时 `cancel` 已置位就落这里）。
    Cancelled,
}

/// 作业用来报进度与看取消的把手。**非泛型** —— 事件通道的类型随结果 `R` 走，而
/// `dyn Source` 这类 trait object 里写不出泛型参数，所以读数口收成两个装箱闭包，
/// 取消旗标也收在里面，`R` 不出现在签名上。一份作业一个 `StageCtx`，作业内部传引用。
pub struct StageCtx {
    item_tx: Box<dyn Fn(ProgressEvent) + Send + Sync>,
    bytes_tx: Box<dyn Fn(ByteProgress) + Send + Sync>,
    cancel: Cancel,
}

impl StageCtx {
    /// 协作式取消的旗标：作业在检查点自己看见（见模块文档的取消语义）。
    pub fn cancel(&self) -> &Cancel {
        &self.cancel
    }

    /// 报一次整批读数：「第 `done` 本 / 共 `total` 本」。`total == 0` 表示不定进度。
    pub fn item(&self, done: u64, total: u64) {
        (self.item_tx)(ProgressEvent {
            done,
            total,
            message: String::new(),
        });
    }

    /// 只发一句文案，进度不动（`done` / `total` 都是 0）。core 组的文案只进日志：
    /// 屏幕上的一句话要走 locale，由界面自己组。
    pub fn message(&self, message: impl Into<String>) {
        (self.item_tx)(ProgressEvent {
            done: 0,
            total: 0,
            message: message.into(),
        });
    }

    /// 报一次字节级读数（下载与拷贝）。与 [`Self::item`] 是两级独立的读数，
    /// 界面上各画各的，不互相覆盖。
    pub fn bytes(&self, done: u64, total: Option<u64>, name: &str) {
        (self.bytes_tx)(ByteProgress {
            done,
            total,
            name: name.to_string(),
        });
    }
}

impl StageCtx {
    /// 造一个**没有接收端**、取消未置位的读数口（发出去的静默丢弃）。给两类
    /// 调用方：测试里直接测管线本体，以及「没人看进度」的后台驱动（监听的初扫）。
    pub(crate) fn detached() -> Self {
        Self {
            item_tx: Box::new(|_| {}),
            bytes_tx: Box::new(|_| {}),
            cancel: Cancel::new(),
        }
    }
}

/// 一条已提交作业的接口：取消它，或读它的事件流。
pub struct TaskHandle<R> {
    pub id: TaskId,
    /// 置位即请求取消（作业在下一个检查点看见）。
    pub cancel: Cancel,
    /// 事件流：`Started` → 若干 `Progress` → `Finished` / `Cancelled`。
    /// 通道随作业结束而关闭，`recv` 在事件耗尽后返回 `Err`。
    pub events: Receiver<TaskEvent<R>>,
}

impl<R> TaskHandle<R> {
    /// 取消并等它收尾（阻塞到通道关闭）。返回是否**有过**事件 —— 纯排队没跑就取消的
    /// 作业也会落一条 `Cancelled`，所以正常是 `true`。
    pub fn cancel_and_wait(&self) -> bool {
        self.cancel.trigger();
        let mut saw = false;
        while self.events.recv().is_ok() {
            saw = true;
        }
        saw
    }
}

/// 工作线程里排队的那件事。作业本体 + 把结果落进通道的包装都塞进这个闭包 ——
/// 结果类型 `R` 在包装闭包的捕获里，不体现在这个类型上，所以它不带泛型。
type Job = Box<dyn FnOnce() + Send>;

/// 作业队列：`Mutex<VecDeque>` 加 `Condvar`，够多条工作线程公平取活。
///
/// 不用 std 的 `mpsc` 当队列：它的接收端不是 `Sync`，多线程只能有一个 `recv`，
/// 那样工作线程数会退化成 1。`Mutex<VecDeque>` + `Condvar` 是 std 范围内的标准做法，
/// 取活那一下的锁竞争相对作业本身可忽略。
struct Queue {
    jobs: Mutex<VecDeque<Job>>,
    ready: Condvar,
    /// 关停标志：置位后不再等新活，跑完手上与队里的就退出。
    closed: AtomicBool,
}

impl Queue {
    fn enqueue(&self, job: Job) {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        jobs.push_back(job);
        drop(jobs);
        self.ready.notify_one();
    }
}

/// 后台线程池。`R` 是一类作业共同的结果类型（比如「导入」产出的一批书）；它只体现在
/// [`submit`](Self::submit) 的签名上，运行期不需要，所以用 `PhantomData` 挂着。
pub struct TaskManager<R: Send + 'static> {
    queue: Arc<Queue>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    next_id: AtomicU64,
    _marker: PhantomData<R>,
}

impl<R: Send + 'static> TaskManager<R> {
    /// 起 `workers` 条工作线程（至少 1 条）。线程名带 `folio-task-` 前缀，`top` / 日志里好认。
    pub fn new(workers: usize) -> Self {
        let queue = Arc::new(Queue {
            jobs: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
            closed: AtomicBool::new(false),
        });
        let mut handles = Vec::with_capacity(workers.max(1));
        for index in 0..workers.max(1) {
            let queue = Arc::clone(&queue);
            let handle = std::thread::Builder::new()
                .name(format!("folio-task-{index}"))
                .spawn(move || worker(queue))
                .expect("起后台工作线程");
            handles.push(handle);
        }
        Self {
            queue,
            workers: Mutex::new(handles),
            next_id: AtomicU64::new(1),
            _marker: PhantomData,
        }
    }

    /// 提交一个作业，拿到它的 [`TaskHandle`]。
    ///
    /// 闭包在后台线程上跑，经 [`StageCtx`] 报进度、看取消（[`TaskHandle::cancel`]
    /// 与它共享同一面旗）。它的返回值就是 [`TaskEvent::Finished`] 里那份结果。
    pub fn submit<F>(&self, job: F) -> TaskHandle<R>
    where
        F: FnOnce(&StageCtx) -> Result<R, TaskError> + Send + 'static,
    {
        let (tx, events) = mpsc::channel();
        let cancel = Cancel::new();
        let id = TaskId(self.next_id.fetch_add(1, Ordering::Relaxed));

        let item_tx = tx.clone();
        let bytes_tx = tx.clone();
        let job_cancel = cancel.clone();
        // 收尾判定用自己的一份：`cancel` 本体要活着走出这个闭包，进 [`TaskHandle`]。
        let finish_cancel = cancel.clone();
        let wrapped: Job = Box::new(move || {
            let _ = tx.send(TaskEvent::Started);
            let stage = StageCtx {
                item_tx: Box::new(move |event| {
                    let _ = item_tx.send(TaskEvent::Progress(event));
                }),
                bytes_tx: Box::new(move |event| {
                    let _ = bytes_tx.send(TaskEvent::Bytes(event));
                }),
                cancel: job_cancel,
            };
            let result = job(&stage);
            let event = if finish_cancel.triggered() {
                TaskEvent::Cancelled
            } else {
                TaskEvent::Finished(result)
            };
            let _ = tx.send(event);
        });
        self.queue.enqueue(wrapped);

        TaskHandle { id, cancel, events }
    }

    /// 关停：不再等新活，把手上与队里已排的全部跑完，然后回收线程。幂等。
    ///
    /// 会**阻塞**到所有工作线程退出 —— 所以别在正被工作线程占用的路径上调它。
    pub fn shutdown(&self) {
        self.queue.closed.store(true, Ordering::SeqCst);
        self.queue.ready.notify_all();
        let mut workers = self.workers.lock().unwrap_or_else(|e| e.into_inner());
        for handle in workers.drain(..) {
            let _ = handle.join();
        }
    }

    /// 当前工作线程数（已回收则 0）。
    pub fn workers(&self) -> usize {
        self.workers.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

impl<R: Send + 'static> Drop for TaskManager<R> {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 一条工作线程：有活就取来跑，没活就等；关停且队空就退出。
fn worker(queue: Arc<Queue>) {
    loop {
        let job = {
            let mut jobs = queue.jobs.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                // 先取活再看关停：这样关停时**已经在队里的**作业仍会被跑完。
                if let Some(job) = jobs.pop_front() {
                    break Some(job);
                }
                if queue.closed.load(Ordering::SeqCst) {
                    break None;
                }
                jobs = queue.ready.wait(jobs).unwrap_or_else(|e| e.into_inner());
            }
        };
        match job {
            Some(job) => job(),
            None => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::Receiver;
    use std::time::Duration;

    use super::{TaskEvent, TaskManager};

    /// 把一条作业的事件流收干（作业结束、通道关闭后返回）。
    fn drain<R>(events: Receiver<TaskEvent<R>>) -> Vec<TaskEvent<R>> {
        events.into_iter().collect()
    }

    fn finished<R>(events: &[TaskEvent<R>]) -> Option<&Result<R, super::TaskError>> {
        events.iter().find_map(|e| match e {
            TaskEvent::Finished(r) => Some(r),
            _ => None,
        })
    }

    /// 作业跑起来、进度回流、结果落进 `Finished`。
    #[test]
    fn a_job_runs_and_reports_progress_and_result() {
        let manager = TaskManager::<u32>::new(1);
        let handle = manager.submit(|stage| {
            stage.item(1, 3);
            stage.message("三步走");
            stage.item(3, 3);
            Ok(42)
        });
        let events = drain(handle.events);

        assert!(
            matches!(events.first(), Some(TaskEvent::Started)),
            "该先来 Started"
        );
        let done: Vec<u64> = events
            .iter()
            .filter_map(|e| match e {
                TaskEvent::Progress(p) => Some(p.done),
                _ => None,
            })
            .collect();
        assert_eq!(done, vec![1, 0, 3], "三次读数都该到（中间那条只是文案）");
        match finished(&events) {
            Some(Ok(42)) => {}
            other => panic!("应以 Finished(Ok(42)) 收尾，实际 {other:?}"),
        }
    }

    /// 两级读数是两条独立的流：整批读数不盖掉字节级，反过来也一样。
    #[test]
    fn item_and_bytes_are_two_independent_channels() {
        let manager = TaskManager::<()>::new(1);
        let handle = manager.submit(|stage| {
            stage.item(1, 12);
            stage.bytes(41, Some(100), "某本");
            stage.item(2, 12);
            stage.bytes(100, Some(100), "某本");
            Ok(())
        });
        let events = drain(handle.events);
        let items: Vec<u64> = events
            .iter()
            .filter_map(|e| match e {
                TaskEvent::Progress(p) => Some(p.done),
                _ => None,
            })
            .collect();
        let bytes: Vec<u64> = events
            .iter()
            .filter_map(|e| match e {
                TaskEvent::Bytes(b) => Some(b.done),
                _ => None,
            })
            .collect();
        assert_eq!(items, vec![1, 2], "整批读数不被字节级打断");
        assert_eq!(bytes, vec![41, 100], "字节级读数不被整批盖掉");
    }

    /// 作业失败：`Finished(Err(..))` 带着错原文回来。
    #[test]
    fn a_failing_job_surfaces_its_error() {
        let manager = TaskManager::<()>::new(1);
        let handle = manager.submit(|_stage| Err("炸了".into()));
        let events = drain(handle.events);
        match finished(&events) {
            Some(Err(e)) => assert_eq!(e.to_string(), "炸了"),
            other => panic!("应以 Finished(Err) 收尾，实际 {other:?}"),
        }
    }

    /// 取消：作业在检查点看见标志就收手，最终落 `Cancelled`。
    #[test]
    fn cancel_lands_as_cancelled() {
        let manager = TaskManager::<()>::new(1);
        let handle = manager.submit(|stage| {
            let mut i = 0u64;
            while !stage.cancel().triggered() {
                stage.item(i, 0);
                i += 1;
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(())
        });
        // 让它至少跑起来，再置取消。
        std::thread::sleep(Duration::from_millis(20));
        handle.cancel.trigger();
        let events = drain(handle.events);
        assert!(
            matches!(events.last(), Some(TaskEvent::Cancelled)),
            "取消的作业该以 Cancelled 收尾"
        );
    }

    /// 一次提交一批，结果一个都不少（多工作线程并发取活）。
    #[test]
    fn many_jobs_all_finish() {
        let manager = TaskManager::<u64>::new(4);
        let handles: Vec<_> = (0..12)
            .map(|i| manager.submit(move |_stage| Ok(i * 2)))
            .collect();
        let mut sum = 0;
        for handle in handles {
            for event in drain(handle.events) {
                if let TaskEvent::Finished(Ok(v)) = event {
                    sum += v;
                }
            }
        }
        assert_eq!(sum, (0..12u64).map(|i| i * 2).sum::<u64>());
    }

    /// 关停回收线程；再提交的作业不再被执行（通道在没有工作线程时会一直挂着，
    /// 所以这里只断言线程被回收）。
    #[test]
    fn shutdown_reclaims_workers() {
        let manager = TaskManager::<()>::new(3);
        assert_eq!(manager.workers(), 3);
        manager.shutdown();
        assert_eq!(manager.workers(), 0, "关停后线程该被回收");
    }
}
