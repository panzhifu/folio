//! 后台任务管线：把「可取消、可报进度」的作业丢到后台线程上跑，事件经通道回流。
//!
//! 只依赖 `std`（`thread` + `mpsc` + `Condvar`）—— 与 `folio-core` 其余部分一致，
//! 不引 tokio / crossbeam，也不依赖 gpui。界面侧收到 [`TaskEvent`] 后自己决定怎么画。
//!
//! 形状是「**一个作业一条通道**」：每次 [`TaskManager::submit`] 都新开一条 `mpsc` 通道，
//! 事件不共用、不带 id 路由 —— 交出去的那个 [`TaskHandle`] 就是这条作业的全部接口
//! （看进度、取消、等在完成上）。
//!
//! 取消沿用远程下载那个标志 [`Cancel`](crate::net::remote::Cancel)：它是「置位、由作业在
//! 检查点自己看见」的协作式取消，不是抢占，所以最多晚一个检查点生效。作业应当把
//! `cancel.triggered()` 当作「尽快返回」的信号，取消的最终定性由本模块给出：
//! 作业返回时若 `cancel` 已置位，一律落成 [`TaskEvent::Cancelled`]，哪怕它其实跑完了。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

pub use crate::net::remote::Cancel;

/// 作业的失败原因。用装箱错误而不是某个具体枚举：core 这一层不该规定上层怎么建模错误，
/// 而 `anyhow::Error` 本身就是这个类型，界面侧收成因果链时不用再包一层。
pub type TaskError = Box<dyn std::error::Error + Send + Sync>;

/// 作业的身份，日志与「这套管理器上跑过几件事」用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TaskId(pub u64);

/// 作业的优先级。**声明顺序即优先级**：数组里越靠前的越先被取（`Ord` 越小越优先）。
///
/// 四类来自 `plans/基础设施.md` §1.1，是**防饥饿的唯一手段**：打开一本千章的书会把
/// 一条队列占满，导入作业就会饿死；两类共用一个池而不分档，就一定互相拖累。
/// 取值时按「第一个非空的档」拿，同档内仍是先进先出。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Priority {
    /// 交互：当前可见带的排版、shaping、命中。读者正看着，最急。
    #[default]
    Interactive,
    /// 预热：邻章与更远章节的取字节 + 解析 + 匹配。可被交互作业抢占。
    Prewarm,
    /// 落库：位置写入、批注写入、统计累计。量小，但要排在不挡人的位置。
    Persist,
    /// 清理：缩略图重建、孤儿文件回收、重索引。低到「界面空闲才跑」。
    Cleanup,
}

impl Priority {
    /// 在队列数组里的下标（声明顺序即优先级）。
    fn index(self) -> usize {
        self as usize
    }

    /// 声明顺序的那份清单（下标同 [`Self::index`]）。
    const ALL: [Priority; CLASSES] = [
        Priority::Interactive,
        Priority::Prewarm,
        Priority::Persist,
        Priority::Cleanup,
    ];

    /// 这一档**同时在跑**的作业封顶数（`plans/基础设施.md` §1.1 那张表的「上限」列）。
    ///
    /// 交互 / 预热各 4：够把当前这本书跑起来，又不至于让一个作业把线程池占满；落库 / 清理
    /// 各 1 —— 前者「串行一条即可」，后者「低到界面空闲才跑」。封顶只在池里跑**多类**作业、
    /// 且工作线程数够多时才真的挡得住（单线程池里它自然恒为 1）。
    fn cap(self) -> usize {
        match self {
            Self::Interactive | Self::Prewarm => 4,
            Self::Persist | Self::Cleanup => 1,
        }
    }
}

/// 队列分几档（[`Priority`] 的变体数）。
const CLASSES: usize = 4;

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
    /// [`Self::detached`] 的公开版本：给「不在作业池里跑、但作业体要求一个读数口」
    /// 的调用方（界面那一侧就地跑一个作业时）。
    pub fn detached_for_tests() -> Self {
        Self::detached()
    }

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

/// 把 panic 载荷折成一条错误。载荷的常见形态就 `&'static str` 与 `String` 两种，
/// 两种都不是时给一句实话，不假装知道 panic 的内容 —— 调用方拿到的是「这个作业炸了」，
/// 具体为什么炸由 panic 自己打进 stderr 那一条负责。
fn panic_error(payload: Box<dyn std::any::Any + Send>) -> TaskError {
    let text = payload
        .downcast_ref::<&'static str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "作业 panic，载荷没有可读信息".to_string());
    TaskError::from(text)
}

/// 作业队列：`Mutex<VecDeque>` 加 `Condvar`，够多条工作线程公平取活。
///
/// **唤醒纪律**：工作线程拿着 `jobs` 这把锁读判据，读到「该等」就 `ready.wait` 松开锁入睡。
/// 所以凡是**只在被唤醒时才会再读一次**的判据，改动必须也在 `jobs` 锁里，改完再 `notify`；
/// 否则那一发 notify 会落进「已读到旧值、还没入睡」的窗口里永久丢掉，睡着的线程不再有任何
/// 事件能叫醒它。[`Queue::closed`] 属于这一类：置位之后不会再有入队来补一发唤醒，
/// 于是 `TaskManager::shutdown` 的 `join` 永远不返回。
/// 而「这一档跑满封顶」那一类不需要：腾名额的那条线程自己会回到循环顶端重查队列，
/// 真有新活时 `enqueue` 也一定在锁里推进队列再 `notify_all`，两条路都能把它捞回来。
///
/// 实测（3000 轮 × 10 个进程，`TaskManager::new(3)` 起停）：`closed` 写在锁外 **10/10 个进程
/// 卡死**，中位数第 500 轮命中；写进锁里 **0/10**。封顶那一档（2 条线程 + 两个落库作业）
/// 同压 12 个进程，「队里的作业没人取」这条断言**一次都没红过**（7 次失败全部落在关停那条
/// 断言上），所以它确实只是延迟，不是卡死。
///
/// 不用 std 的 `mpsc` 当队列：它的接收端不是 `Sync`，多线程只能有一个 `recv`，
/// 那样工作线程数会退化成 1。`Mutex<VecDeque>` + `Condvar` 是 std 范围内的标准做法，
/// 取活那一下的锁竞争相对作业本身可忽略。
struct Queue {
    /// 按 [`Priority`] 分档，`jobs[0]` 最高。取活时从头找第一个非空的档。
    jobs: Mutex<[VecDeque<Job>; CLASSES]>,
    /// 每一档**正在跑**的作业数（取活 +1、跑完 -1）。封顶判据看它。
    running: [AtomicUsize; CLASSES],
    ready: Condvar,
    /// 关停标志：置位后不再等新活，跑完手上与队里的就退出。
    closed: AtomicBool,
}

impl Queue {
    fn enqueue(&self, priority: Priority, job: Job) {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        jobs[priority.index()].push_back(job);
        drop(jobs);
        // **`notify_all` 不是保守，是必需的**（这里曾经是 `notify_one`，会丢唤醒）。
        //
        // 场景：空队列上睡着 4 条工作线程，突然来 4 个作业。每次 `enqueue` 只唤醒一条，
        // 但**先醒的那条取走活之后不会叫醒别人**（它只是去 `job()` 了）—— 于是剩下 3 条
        // 一直睡着，而队里还有 3 个没人取的作业。四个都睡着 = 队列里躺着活、没人跑。
        //
        // 这条路径在满负载下才现形（本机实测：单独跑 `infra::task` 十次全绿，
        // 跑整包时偶发挂死十几分钟），所以它躲过了很久 —— 症状是「测试卡住」，
        // 而不是「测试失败」，更容易被当成环境问题。
        //
        // 为什么不能靠「取活之后补一次 notify」：那要把唤醒逻辑散到两个地方
        // （取活处与实际去做事处），而漏掉任何一处就又丢一次唤醒。唤醒所有人是
        // 一笔很小的开销（醒来的线程发现没活会立刻回去睡），换个结构上不会漏。
        self.ready.notify_all();
    }

    /// 还排着队、没被取走的作业数（诊断用，见 `crate::infra::diagnostics`）。
    fn depth(&self) -> usize {
        let jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        jobs.iter().map(VecDeque::len).sum()
    }
}

/// 后台线程池。**不带类型参数**：结果类型 `R` 只活在 [`submit`](Self::submit) 的签名与
/// 闭包捕获里，队列与工作线程一概不需要它。于是同一个池能跑结果类型各异的作业
/// （导入产出 `Vec<ImportOutcome>`、扫描产出 `ScanReport`、排版产出 `Typeset`…），
/// 这正是 `plans/基础设施.md` §1.1「一条队列」要的形状。
pub struct TaskManager {
    queue: Arc<Queue>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    next_id: AtomicU64,
}

impl TaskManager {
    /// 起 `workers` 条工作线程（至少 1 条）。线程名带 `folio-task-` 前缀，`top` / 日志里好认。
    pub fn new(workers: usize) -> Self {
        let queue = Arc::new(Queue {
            jobs: Mutex::new(Default::default()),
            running: std::array::from_fn(|_| AtomicUsize::new(0)),
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
        }
    }

    /// 提交一个作业，拿到它的 [`TaskHandle`]（默认档 [`Priority::Interactive`]）。
    ///
    /// 闭包在后台线程上跑，经 [`StageCtx`] 报进度、看取消（[`TaskHandle::cancel`]
    /// 与它共享同一面旗）。它的返回值就是 [`TaskEvent::Finished`] 里那份结果。
    pub fn submit<R: Send + 'static, F>(&self, job: F) -> TaskHandle<R>
    where
        F: FnOnce(&StageCtx) -> Result<R, TaskError> + Send + 'static,
    {
        self.submit_with(Priority::default(), job)
    }

    /// 同 [`submit`](Self::submit)，但指定 [`Priority`]（§1.1 的四类）。
    ///
    /// 分档只在**一个池里跑不止一类作业**时才有意义：同档先进先出，跨档高优先级先出，
    /// 于是在跑的互动作业不会被后面灌进来的一批量预热/清理作业压在后面。
    pub fn submit_with<R: Send + 'static, F>(&self, priority: Priority, job: F) -> TaskHandle<R>
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
            // 作业里 panic 不能带走这条工作线程和它占着的并发名额：接住它、折成一条错误，
            // 下面的取消判定与事件发送照旧走，于是线程数不掉、`running` 计数也不掉。
            // `AssertUnwindSafe` 的理由：`job` 是 FnOnce，`stage` 只有两条发送闭包与一面旗，
            // 展开结束后没有「半改好、之后还要回读」的状态留在手里。
            // 注意 release 档接不住：workspace 根 `[profile.release] panic = "abort"`，那条路上
            // 进程直接终止。这里挡的是 dev/test 那种静默形状 —— 线程死了没人报，界面永远等结果。
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&stage)))
                .unwrap_or_else(|payload| Err(panic_error(payload)));
            let event = if finish_cancel.triggered() {
                TaskEvent::Cancelled
            } else {
                TaskEvent::Finished(result)
            };
            let _ = tx.send(event);
        });
        self.queue.enqueue(priority, wrapped);

        TaskHandle { id, cancel, events }
    }

    /// 现在排着队、还没被取走的作业数（诊断用）。
    pub fn queued(&self) -> usize {
        self.queue.depth()
    }

    /// 关停：不再等新活，把手上与队里已排的全部跑完，然后回收线程。幂等。
    ///
    /// 会**阻塞**到所有工作线程退出 —— 所以别在正被工作线程占用的路径上调它。
    pub fn shutdown(&self) {
        // `closed` 要**在 `jobs` 锁里**写。工作线程是「持着这把锁读 closed，读到没活就
        // 入睡」；锁外写再 notify，唤醒就落进「已读到 false、还没入睡」那段窗口里永久丢掉，
        // 下面的 join 于是永远不返回。锁里写之后，两种时序都收敛：要么工作线程读到已置位
        // 直接退出，要么它已入睡、这一发 notify_all 一定送得到一个睡着的线程。
        {
            let _guard = self.queue.jobs.lock().unwrap_or_else(|e| e.into_inner());
            self.queue.closed.store(true, Ordering::SeqCst);
            self.queue.ready.notify_all();
        }
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

impl Drop for TaskManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 一条工作线程：有活就取来跑，没活就等；关停且队空就退出。
fn worker(queue: Arc<Queue>) {
    loop {
        let taken = {
            let mut jobs = queue.jobs.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                // 关停时无视封顶：§1.3 要求「跑完已排的」，不能因为某一档到达封顶就把
                // 还在队里的作业丢掉。
                let draining = queue.closed.load(Ordering::SeqCst);
                // 取活顺序：从高到低找第一个「非空、且这一档还没跑满封顶」的档（同档 FIFO）。
                let picked = jobs.iter_mut().enumerate().find_map(|(class, deque)| {
                    deque.front()?; // 这一档空 → 看下一档
                    if !draining
                        && queue.running[class].load(Ordering::SeqCst) >= Priority::ALL[class].cap()
                    {
                        return None; // 这一档跑满了 → 看下一档
                    }
                    let job = deque.pop_front()?;
                    queue.running[class].fetch_add(1, Ordering::SeqCst);
                    Some((class, job))
                });
                if picked.is_some() {
                    break picked;
                }
                if draining {
                    break None;
                }
                // 队列里可能还有活，只是都被封顶挡住了 —— 等有人腾出名额。
                jobs = queue.ready.wait(jobs).unwrap_or_else(|e| e.into_inner());
            }
        };
        match taken {
            Some((class, job)) => {
                job();
                // 腾出一个名额：唤醒可能正卡在「这一档满了」上的线程。
                // 这一步不在 `jobs` 锁里做，也不必：取走这份作业的正是马上要回到循环顶端
                // 重查队列的本线程，就算这一发唤醒落进别的线程「读到满、还没入睡」的窗口，
                // 下一次入队会在锁里 `notify_all`（见 `Queue` 的说明），活不会没人取。
                // 实测过它不是卡死点：未修的写法上，「队里的作业没人取」这条断言在
                // 3000 轮 × 12 个进程里一次都没红过，红的全是 `closed` 那一条。
                queue.running[class].fetch_sub(1, Ordering::SeqCst);
                queue.ready.notify_one();
            }
            None => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::Receiver;
    use std::time::Duration;

    use super::{Priority, TaskEvent, TaskManager};

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

    /// 在预算内收一条作业的事件流，预算用尽就把手里收到的交出去。有界收干是为了让
    /// 「永远等不到结果」读成一条红的断言，而不是一场要人来掐的挂死。
    fn drain_within<R>(events: Receiver<TaskEvent<R>>, budget: Duration) -> Vec<TaskEvent<R>> {
        let deadline = std::time::Instant::now() + budget;
        let mut saw = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                break;
            }
            match events.recv_timeout(left) {
                Ok(event) => saw.push(event),
                Err(_) => break,
            }
        }
        saw
    }

    /// 作业跑起来、进度回流、结果落进 `Finished`。
    #[test]
    fn a_job_runs_and_reports_progress_and_result() {
        let manager = TaskManager::new(1);
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
        let manager = TaskManager::new(1);
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

    /// 作业里 panic 不该带走工作线程、也不该带走它占着的并发名额：接住它、折成
    /// `Finished(Err(..))` 递回调用方，随后那一件照常跑。挑 `new(1)` 是最狠的形状 ——
    /// 线程一死整个后台就停，而症状是「界面永远等不到结果」，不是任何一条报错。
    /// 不接住的写法：炸掉的作业只留下 `Started` 就通道关闭（结果谁也拿不到），
    /// 第二件躺在队里没人取；这条门在 2 秒预算内红，不是挂死。
    #[test]
    fn a_panicking_job_does_not_cost_the_pool_a_worker() {
        let manager = TaskManager::new(1);
        let boom = manager.submit(|_stage| -> Result<u8, super::TaskError> {
            panic!("作业里炸了");
        });
        let events = drain_within(boom.events, Duration::from_secs(2));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, TaskEvent::Finished(Err(_)))),
            "panic 该折成一条错误事件回来，实际收到 {events:?}"
        );

        let next = manager.submit(|_stage| Ok(7u8));
        let got = drain_within(next.events, Duration::from_secs(2));
        assert!(
            matches!(got.last(), Some(TaskEvent::Finished(Ok(7)))),
            "前一件炸了，后一件也该照常被跑起来，实际收到 {got:?}"
        );
    }

    /// 作业失败：`Finished(Err(..))` 带着错原文回来。
    #[test]
    fn a_failing_job_surfaces_its_error() {
        let manager = TaskManager::new(1);
        let handle = manager.submit::<(), _>(|_stage| Err("炸了".into()));
        let events = drain(handle.events);
        match finished(&events) {
            Some(Err(e)) => assert_eq!(e.to_string(), "炸了"),
            other => panic!("应以 Finished(Err) 收尾，实际 {other:?}"),
        }
    }

    /// 取消：作业在检查点看见标志就收手，最终落 `Cancelled`。
    #[test]
    fn cancel_lands_as_cancelled() {
        let manager = TaskManager::new(1);
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
        let manager = TaskManager::new(4);
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

    /// 空队列上睡着的线程必须都在有活时被叫醒；作业躺队里没人跑要**失败而不是挂死**。
    ///
    /// `Queue::enqueue` 里那句 `notify_all` 的背景：原先它是 `notify_one`，会丢唤醒
    /// —— 空队列上睡着 N 条线程、一口气来 N 个作业时，先醒的那条取走活之后不会叫醒
    /// 别人，于是剩下的作业没人取。实测症状是**整包测试偶发挂死十几分钟**（单独跑
    /// `infra::task` 十次全绿），所以它躲过了很久：挂死比失败更容易被当成环境问题。
    ///
    /// **这条闸门是不完全的，得说清楚**：真要把那个竞态稳定复现，需要在「队里有活」
    /// 与「工作线程真正睡下」之间精确落一刀，而 `Queue` 是私有的、测试里拿不到那个
    /// 时刻。我试过用「先睡 50ms 再一口气提交」造它，结果在坏代码上三次全绿 ——
    /// 所以它**证明不了** `notify_one` 有问题。留着它的价值是：万一哪天丢唤醒真的
    /// 命中这条形状，它**失败**而不是挂死（有 5 秒上限 + 一条说得清的panic），
    /// 比让 CI 卡到 job 超时好查。真正的保证来自 `notify_all` 那个结构本身。
    #[test]
    fn every_sleeper_is_woken_when_work_arrives() {
        use std::sync::mpsc::RecvTimeoutError;
        use std::time::Duration;

        let workers = 4;
        let manager = TaskManager::new(workers);

        // 先把 workers 条线程都赶到「空队列上等活」的状态：睡够一次调度。
        std::thread::sleep(Duration::from_millis(50));

        // 一口气提交 workers 个作业 —— 关键在**不是逐个提交再逐个等**。
        let handles: Vec<_> = (0..workers)
            .map(|i| manager.submit(move |_stage| Ok(i as u64)))
            .collect();

        let mut sum = 0u64;
        for handle in handles {
            loop {
                match handle.events.recv_timeout(Duration::from_secs(5)) {
                    Ok(TaskEvent::Finished(Ok(v))) => {
                        sum += v;
                        break;
                    }
                    Ok(_) => continue,
                    // 通道关闭 = 作业结束了。
                    Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) => {
                        panic!("有作业躺在队里没人跑：丢了唤醒（enqueue 该用 notify_all）")
                    }
                }
            }
        }
        assert_eq!(sum, (0..workers as u64).sum::<u64>(), "每个作业都该跑完");
    }

    /// 关停回收线程；再提交的作业不再被执行（通道在没有工作线程时会一直挂着，
    /// 所以这里只断言线程被回收）。
    #[test]
    fn shutdown_reclaims_workers() {
        let manager = TaskManager::new(3);
        assert_eq!(manager.workers(), 3);
        manager.shutdown();
        assert_eq!(manager.workers(), 0, "关停后线程该被回收");
    }

    /// 关停那一发唤醒不能丢。工作线程是「拿着 `jobs` 锁读 `closed` → 读到没活就
    /// `ready.wait` 松开锁入睡」，所以 `closed` 也要在 `jobs` 锁里写；写在锁外时那一发
    /// `notify_all` 可能落进「已读到 false、还没入睡」的窗口里永久丢掉，`shutdown` 里的
    /// `join` 再也不返回 —— 症状不是某条测试失败，是整包测试卡住不动。
    ///
    /// 每轮把关停放到另一条线程上、只给它 500 ms：一轮正常是微秒级，所以这条读不出「慢」，
    /// 只读得出「有没有人再也不醒了」。未修的写法压 3000 轮 × 10 个进程是 **10/10 个进程红**
    /// （中位数第 500 轮命中），写进锁里 0/10。
    #[test]
    fn shutdown_wakes_every_idle_worker() {
        use std::sync::mpsc;

        for round in 0..3000u32 {
            let manager = TaskManager::new(3);
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                drop(manager);
                let _ = tx.send(());
            });
            assert!(
                rx.recv_timeout(Duration::from_millis(500)).is_ok(),
                "第 {round} 轮：关停后有工作线程没被叫醒，join 不返回"
            );
        }
    }

    /// §1.1：跨档按优先级取活，高优先级先出（同档仍先进先出）。
    #[test]
    fn higher_priority_jobs_are_dequeued_first() {
        use std::sync::{Arc, Condvar, Mutex};

        let manager = TaskManager::new(1);
        let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        // 闸门：先占住唯一的工作线程，让后面三类排在队里，再一起放行。
        let gate = Arc::new((Mutex::new(false), Condvar::new()));

        let g = Arc::clone(&gate);
        let blocker = manager.submit(move |_stage| {
            let (lock, cv) = &*g;
            let mut open = lock.lock().unwrap();
            while !*open {
                open = cv.wait(open).unwrap();
            }
            Ok(())
        });

        // 低优先级先进队、高优先级后进 —— 取的时候顺序要反过来。
        let queued: Vec<_> = [
            (Priority::Cleanup, "cleanup"),
            (Priority::Persist, "persist"),
            (Priority::Prewarm, "prewarm"),
        ]
        .into_iter()
        .map(|(priority, tag)| {
            let order = Arc::clone(&order);
            manager.submit_with(priority, move |_stage| {
                order.lock().unwrap().push(tag);
                Ok(())
            })
        })
        .collect();

        {
            let (lock, cv) = &*gate;
            *lock.lock().unwrap() = true;
            cv.notify_all();
        }

        blocker.cancel_and_wait();
        for handle in queued {
            handle.cancel_and_wait();
        }
        assert_eq!(
            *order.lock().unwrap(),
            vec!["prewarm", "persist", "cleanup"],
            "高优先级要先被取走"
        );
    }

    /// §1.1 的「上限」列：一档的在跑数有封顶。落库（封顶 1）即便有两个空闲 worker，
    /// 也不会有第二条同时开跑。
    #[test]
    fn a_class_respects_its_concurrency_cap() {
        use std::sync::mpsc;
        use std::sync::{Arc, Condvar, Mutex};

        let manager = TaskManager::new(2);
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (started_tx, started_rx) = mpsc::channel::<()>();

        let handles: Vec<_> = (0..2)
            .map(|_| {
                let gate = Arc::clone(&gate);
                let started = started_tx.clone();
                manager.submit_with(Priority::Persist, move |_stage| {
                    started.send(()).ok();
                    let (lock, cv) = &*gate;
                    let mut open = lock.lock().unwrap();
                    while !*open {
                        open = cv.wait(open).unwrap();
                    }
                    Ok(())
                })
            })
            .collect();
        drop(started_tx);

        // 第一条该跑起来。
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("至少一条落库作业该跑起来");
        // 闸门没开、第一条还占着名额：第二条**不该**开始。
        assert!(
            started_rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "落库档封顶 1，不该有第二条同时跑"
        );

        // 放行，收干。
        {
            let (lock, cv) = &*gate;
            *lock.lock().unwrap() = true;
            cv.notify_all();
        }
        for handle in handles {
            handle.cancel_and_wait();
        }
    }
}
