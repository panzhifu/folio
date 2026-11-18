//! 批次预算、并行调度与归档退层。每份实际书字节交给共享版本登记管线。
//! 哈希占号只串行同内容，独立候选保持并行；重索引生成新 index generation。

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

#[cfg(test)]
pub(crate) use super::indexing::fail_landing;
pub(super) use super::indexing::write_cover;
use crate::content::format::epub::MAX_PARSE_WORKERS;
use crate::content::format::sniff;
use crate::error::Error;
use crate::error::ImportReject;
use crate::error::Result;
use crate::infra::paths;
use crate::infra::task::Priority;
use crate::infra::task::StageCtx;
use crate::infra::task::TaskError;
use crate::infra::task::TaskHandle;
use crate::infra::task::TaskManager;

use crate::store::Store;

use super::Candidate;
use super::ImportOutcome;
use super::ImportReport;
use super::Imported;
use super::Source;
use crate::ingest::import::sources::staged_name;

/// 管线本体：候选 → 暂存 → 算哈希 → 判格式 → 判重 → 落地 → 解析 → 落库，
/// **一本一条完整旅程，逐本报数**。
///
/// - **顺序是硬的**：`stage → hash → sniff → dedup → rename → parse → register`；
///   中间任何一本坏了只让它自己那条 `Err`，其余的照走。
/// - **取消**在书与书之间检查（准确说在「领下一本的号」之前，见 [`Batch::run`]）；置位就
///   停下，**已经落库的那些是真的** —— 界面收到 `Cancelled` 重查一次架面就能看见它们。
///   同时在飞几本由 [`split_budget`] 定，所以这条承诺的延迟上限是「每路各一本」而不是
///   「一本」：只有一个候选、或 ≤3 核的机器（那儿预算切成 1 本）才退回「恰好停在两本
///   之间」那条最严的形状。
///   **落地之后、落库之前不设检查点**：那一段里停一次就会留下「副本在、库里没有行」的
///   状态，而 §九 要的正是挡住它。
/// - `candidates` 交不出（整个来源坏了，比如根目录读不了）返回空批 —— 逐本各自成败的
///   立场在这里够不着，一条 `Err` 没有候选可挂，只能进日志。
/// - 暂存区建不起来时也返回空批：宁可一本都不导，也不悄悄退回「直接写进书库目录」那条
///   老路 —— 判定排在落地之前要挡住的正是那个形状。
///
/// 整个批次一个 span（`plans/基础设施.md` §4.1 第 4 条）：导入慢在哪一步（列目录 /
/// 拷贝 / 哈希 / 解析 / 落库）看 `close time.busy` 这一段最直接。并行时 worker 不会
/// 自动带上这一格 span，[`import_batch`] 里那句 `in_scope` 就是补这一笔的。

#[tracing::instrument(
    level = "debug",
    name = "import",
    skip_all,
    fields(kind = ?source.kind())
)]
pub fn import_from(
    source: &dyn Source,
    book_dir: &Path,
    store: &Store,
    ctx: &StageCtx,
) -> Vec<ImportOutcome> {
    let kind = source.kind();
    let candidates = match source.candidates(book_dir, ctx) {
        Ok(candidates) => candidates,
        Err(err) => {
            tracing::error!("import: {kind:?} 交不出候选：{err:#}");
            return Vec::new();
        }
    };
    let staging = paths::staging_dir(book_dir);
    if let Err(err) = paths::prepare_dir(&staging) {
        tracing::error!("import: {kind:?} 暂存区 {staging:?} 建不起来：{err:#}");
        return Vec::new();
    }
    let total = candidates.len() as u64;
    tracing::info!("import: {kind:?} 一批 {total} 本 → {book_dir:?}");
    let outcomes = import_batch(
        source,
        book_dir,
        store,
        ctx,
        &staging,
        &candidates,
        process_budget(),
    );
    ctx.item(total, total);
    let report = ImportReport::tally(&outcomes);
    tracing::info!("import: {kind:?} 一批结清 —— {}", report.summary());
    outcomes
}

/// 一批的线程预算怎么分：**同时几本**与**每本几路**一次算完，共用一份预算。
///
/// 要同时定这两个数的理由是那个乘积 —— 真正抢核的是 `books × per_book`。以前两层各猜
/// 各的：内层只看得见整机核数（`content::format::epub::parse_worker_count`），外层每多
/// 一本就把总量再乘一份，于是「一本书内部几路」这件事实际上由**两台机器各自的核数**决定，
/// 而导入那边不知道。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Budget {
    /// 一批里同时在导几本。
    pub(crate) books: usize,
    /// 每一本内部**最多**起几路解析 worker。是上限不是配额：条目少到不值得起线程时
    /// 照样走顺序，判据留在 provider 那一层（只有它知道有几个条目）。
    pub(crate) per_book: usize,
}

/// 一批最多同时导几本。再往上量不到东西：8 本合成书（共 4.7 MB）那一批量到的是
/// 1 本 ≈ 55 ms、4 本 ≈ 25 ms、5 本 ≈ 23 ms —— 第 5 本只换来 2 ms，却多一份解析内存，
/// 所以封顶在 4。
pub(crate) const MAX_IMPORT_BOOKS: usize = 4;
/// 每本至少要几路才算「值得再开一本」：低于这个数就把核留给同批的别的本。
const MIN_WORKERS_PER_BOOK: usize = 2;

/// 把 `cores` 切成一个 [`Budget`]。`cores` 是这台机器给**本进程**用的核数
/// （`available_parallelism` —— 手机上受亲和性掩码影响，在册 8 核可能只报 6）。
///
/// 「同时几本」= 核数 ÷ [`MIN_WORKERS_PER_BOOK`]，封顶 [`MAX_IMPORT_BOOKS`]；每本拿
/// **剩下的**那份，封顶在内层的 `MAX_PARSE_WORKERS`。`books × per_book ≤ cores` 是
/// 构造出来的：两处都只封顶、不放大。
///
/// | 核数 | 同时几本 | 每本几路 | 总线程 |
/// | --- | --- | --- | --- |
/// | 1 | 1 | 1 | 1 |
/// | 2 | 1 | 2 | 2 |
/// | 4 | 2 | 2 | 4 |
/// | 6 | 3 | 2 | 6 |
/// | 8 | 4 | 2 | 8 |
/// | 12 | 4 | 3 | 12 |
/// | 20 | 4 | 4（封顶） | 16 |
///
/// **实测**（同一批 4 本真书共 37 MB；每档都是「钉死 `(books, per_book)` 的二进制」交替跑、
/// 取 6 个中位样本；RSS 那格读的是 `VmHWM`，预热已改成不抱整本书 —— 早先那版用
/// `fs::read` 预热，把 29 MB 的缓冲算进了并行那一档的成本里，数因此虚高约 25 MB）：
///
/// | 机器 | 切法 | 批次墙钟 | 峰值 RSS |
/// | --- | --- | --- | --- |
/// | 20 核桌面 | 1 本 × 4 路（接并行之前这台机器的实际形状） | ≈ 483 ms | ≈ 34 MB |
/// | 20 核桌面 | **4 本 × 4 路（这一格给出的切法）** | ≈ 202 ms（**2.4×**） | ≈ 54 MB（+59%） |
/// | Meizu 20（`available_parallelism` 报 **6**） | 1 本 × 4 路 | ≈ 1080 ms | 19 MB |
/// | Meizu 20 | 2 本 × 3 路 | ≈ 744 ms（1.45×） | 23 MB |
/// | Meizu 20 | 4 本 × 1 路 | ≈ 645 ms（1.67×） | 27 MB |
/// | Meizu 20 | **3 本 × 2 路（这一格给出的切法）** | ≈ 520 ms（**2.07×**） | 25 MB（+32%） |
///
/// 手机上那五行是这次量出来的重点两件事：**以前那一版（`核数 ÷ 内层自己那 4 路`）在 6 核上
/// 整除成「1 本」，接了并行却仍然逐本串行**，就是表里第一行；而「并行只在一层」那条路
/// （4 本 × 1 路）比「3 本 × 2 路」慢 24% —— 内层那两路没有白花，所以预算要在两层之间分，
/// 而不是全给一层。顺带一条：并行那一档的**摆动也小了**（1 本 × 4 路是 945–1275 ms，
/// 3 本 × 2 路是 477–658 ms）—— 一本特别长的书不再能决定整批的时间。
///
/// 一条没覆盖的：读者正打开一本书时读那一本用的是 `ParseBudget::Auto`（整机那一档），
/// 不在这笔乘法里。所以**导一批的同时点开一本**，总线程数会超过 `cores`。要把那一格也
/// 锁住，得让两条路共用一个进程内的令牌池而不是各自算 —— 那是另一件事，没在这里做。
pub(crate) fn split_budget(cores: usize) -> Budget {
    let books = (cores / MIN_WORKERS_PER_BOOK).clamp(1, MAX_IMPORT_BOOKS);
    let per_book = (cores / books).clamp(1, MAX_PARSE_WORKERS);
    Budget { books, per_book }
}

/// 这一趟的预算：把这台机器的核数一次切成两个数（[`split_budget`]）。
fn process_budget() -> Budget {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    split_budget(cores)
}

/// 一趟批次共享的东西。合成一个结构不是为了整齐，是因为「领编号 → 导一本 → 记账」
/// 这一小段要同时被调用线程和每条 worker 用（9 个参数的手写函数会被 clippy 挡下，
/// 而把引用与三个计数器放一处，两边就都只拿 `&Batch`）。
struct Batch<'a> {
    source: &'a dyn Source,
    book_dir: &'a Path,
    staging: &'a Path,
    store: &'a Store,
    ctx: &'a StageCtx,
    candidates: &'a [Candidate],
    /// 每一本内部最多起几路解析 worker（[`Budget::per_book`]）。
    per_book: usize,
    /// 下一个待领的候选下标。
    next: AtomicUsize,
    /// 已经结清的本数，只为那句整批读数。
    done: AtomicUsize,
    /// `(候选下标, 结果)`。谁导完一本就当场记进来：worker 崩了也**不丢已经落好的那些**。
    landed: Mutex<Vec<(usize, ImportOutcome)>>,
}

impl Batch<'_> {
    /// 领一本导一本，到队列见底或取消置位。
    ///
    /// 取消的检查点落在**领号之前**：已经领走的那本跑完（与逐本串行那一路同一条纪律 ——
    /// `plans/基础设施.md` §1.2 第 5 条要的就是「单元之间」而不是单元中间）。
    /// 并行之后「置位后不再导新的」这条承诺的**延迟上限**从「一本」变成「每路各一本」，
    /// 这是并行的全部代价，界面那句「已取消」因此仍指「架面上看得见的那些是真的」。
    fn run(&self) {
        loop {
            if self.ctx.cancel().triggered() {
                return;
            }
            let at = self.next.fetch_add(1, Ordering::Relaxed);
            if at >= self.candidates.len() {
                return;
            }
            let (books, duration) = self.import_at(at);
            let mut landed = self.landed.lock().unwrap_or_else(|e| e.into_inner());
            self.record(&mut landed, at, books, duration);
        }
    }

    /// 走完第 `at` 号候选的整趟旅程，顺手把「这一格结清了」的读数报出去。
    ///
    /// 读数按**结清**的数报，不按领号报：并行时领号在前、结清在后，
    /// 用领号数会画出一条会后退的进度。一个包算一格 —— 总数数的是候选，
    /// 不是抽出来的本数，否则一个 900 条目的合集包会把进度条顶过 100%。
    ///
    /// 一并交回的是**这一趟**花了多久。一个包算一趟：包里抽出来的那几条记的是同一个数
    /// —— 逐条各计一次要往 `import_candidate` 里面铺表，而那层现在的返回形状
    /// （`Vec<(String, Result<Imported>)>`）连不上「哪一段是谁花的」。屏幕上把这一格
    /// 说成「这个包用了 8.8 秒」是对的，把同一个数说成每条 8.8 秒就不是。
    fn import_at(&self, at: usize) -> (Vec<(String, Result<Imported>)>, Duration) {
        let started = Instant::now();
        // 一本把自己那一路弄崩了（来源或 provider 里一句 `unwrap` 就是这个形状）：只让它
        // 自己那一条 Err。**不接这一句整批跟着崩** —— `thread::scope` 结束时会把 scoped
        // 线程的 panic 再传回调用线程（实测：钉 1 路与钉 2 路都整批红），那时候 `landed` 里
        // 已经落好的那些一条都回不去：架上那些行是真的，而报表整批丢，界面只看得见「没了」。
        //
        // `AssertUnwindSafe` 在这里是诚实的：崩完唯一可能坏掉的状态是被弄脏的那几把锁，
        // 而这一带每一处 `lock()` 都写的 `unwrap_or_else(|e| e.into_inner)` —— 含 `Hold`
        // 与 `StagingArea` 的 `Drop`，所以判重的号与暂存区的小屋都不会漏在半路。
        let attempted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            import_candidate(
                self.source,
                &self.candidates[at],
                self.book_dir,
                self.staging,
                self.store,
                self.ctx,
                self.per_book,
            )
        }));
        let books = match attempted {
            Ok(books) => books,
            Err(panic) => {
                let why = panic_text(&panic);
                let name = self.candidates[at].name.clone();
                tracing::error!("import: 《{name}》把跑它的那一路弄崩了，只算这一本失败：{why}");
                vec![(
                    name.clone(),
                    Err(Error::Rejected {
                        name,
                        why,
                        // 算「读不动」而不是新立一档：读者那边的动作与坏包同一种（换一本
                        // 再试）。崩在本本里的那条 `error!` 才是给人查的那一句。
                        reject: ImportReject::Unreadable,
                        source: None,
                    }),
                )]
            }
        };
        let elapsed = started.elapsed();
        let done = self.done.fetch_add(1, Ordering::Relaxed) as u64 + 1;
        self.ctx.item(done, self.candidates.len() as u64);
        (books, elapsed)
    }

    /// 把一本的结论记进表里。每条结果顶着自己那个名字（包里的条目名），
    /// 报表与日志上「哪一本没进来」才说得出是哪一条，而不是全都顶着外面那个包名。
    ///
    /// 记进哪张表由调用方决定：并行那一路记进锁里的 `landed`（worker 崩了也不丢
    /// 已经落好的那些），补跑那一路记进本地那份。
    fn record(
        &self,
        into: &mut Vec<(usize, ImportOutcome)>,
        at: usize,
        books: Vec<(String, Result<Imported>)>,
        duration: Duration,
    ) {
        for (name, result) in books {
            let mut from = self.candidates[at].clone();
            from.name = name;
            into.push((
                at,
                ImportOutcome {
                    candidate: from,
                    result,
                    duration,
                },
            ));
        }
    }

    /// 收干结果并按候选原序交回。`workers > 1` 时顺手把**没有结果的那些**在调用线程上
    /// 补跑一遍：崩掉的 worker 手上那一本不许从报表里静默消失。
    ///
    /// 这是**第二道**：一本自己崩的那种（来源或 provider 里一句 `unwrap`）已经由
    /// [`Batch::import_at`] 在更里面接住了，走到这里还剩的只有「整条 worker 没了」
    /// —— 比如崩在记账或读数那两句上。
    ///
    /// 「有没有结果」按**候选下标**问，不按条数比大小 —— 一个包会交回好几条，
    /// `rows.len() == candidates.len()` 那种算法在合集包面前既会少补也会多补。
    fn finish(&self) -> Vec<ImportOutcome> {
        let total = self.candidates.len();
        let mut rows = std::mem::take(&mut *self.landed.lock().unwrap_or_else(|e| e.into_inner()));
        let mut present: HashSet<usize> = rows.iter().map(|(at, _)| *at).collect();
        if present.len() < total && !self.ctx.cancel().triggered() {
            // 缺下标的就是某条 worker 手上正拿着的那本（它崩在了记进表之前）。
            for at in 0..total {
                // 与 [`Batch::run`] 同一个检查点：取消之后不再补新的。
                if self.ctx.cancel().triggered() {
                    break;
                }
                if present.contains(&at) {
                    continue;
                }
                let (books, duration) = self.import_at(at);
                self.record(&mut rows, at, books, duration);
                present.insert(at);
            }
        }
        // 排这一遍：并行时 `run()` 记进来的是**结清顺序**，而报表要的是候选原序。
        // 稳定排序，所以同一个下标的那几本（一个包里抽出来的）保持它们自己进来的顺序 ——
        // 那一句「900 章的合集按章节顺序摆」就是靠它。补跑的那几本下标可能比表里
        // 已有的小，所以补完之后也归这一遍管。
        rows.sort_by_key(|(at, _)| *at);
        rows.into_iter().map(|(_, outcome)| outcome).collect()
    }
}

/// panic 带出来的那句给人看的话。`panic!("字面量")` 交回 `&str`、`panic!("{x}")` 交回
/// `String`，两种都收；其余只说一句「没带话」—— 为了把日志写好看而在这里再崩一次，
/// 正是这一路要避免的事。
fn panic_text(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = panic.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = panic.downcast_ref::<String>() {
        text.clone()
    } else {
        "来源或解析里一处 panic（没带话）".to_string()
    }
}

/// 一批候选 → 书库，按 `budget` 分：**同时导几本**与**每本内部几路**一起给进来。
///
/// 并行切在「一本书的完整旅程」这一整块上（[`import_candidate`] 从 stage 到落库），
/// 里面不再切细：那一段里 CPU（解析、缩图）与 IO（拷贝、哈希、落库）本来就交错着，
/// 切细了只是多几处要对齐的计数器。
///
/// `Store` 的连接锁仍然把落库那一小段串行着（`add_book` + `save_document`）：在
/// 《斗战狂潮》那一本上实测是端到端的 4.4%（6.2 / 140.3 ms），并行省的就是剩下那一大块。
/// 书越小这一格占比越大 —— 一批小书的并行收益会比一批大书低，别按大书那条外推。
///
/// `budget.books == 1` 时**不起线程**，整批就在调用线程上走（一个候选的批就是这一档，
/// 而 `per_book` 照样传下去 —— 那时它就是整机那一档，与并行之前的形状一致）。这一路留着
/// 的理由不是省几次线程创建，而是「恰好停在两本之间的取消语义」只有这一路能给，测试把它
/// 单独钉着（`cancelling_between_books_keeps_what_already_landed` 把并行度钉成 1）。
pub(crate) fn import_batch(
    source: &dyn Source,
    book_dir: &Path,
    store: &Store,
    ctx: &StageCtx,
    staging: &Path,
    candidates: &[Candidate],
    budget: Budget,
) -> Vec<ImportOutcome> {
    let batch = Batch {
        source,
        book_dir,
        staging,
        store,
        ctx,
        candidates,
        per_book: budget.per_book,
        next: AtomicUsize::new(0),
        done: AtomicUsize::new(0),
        landed: Mutex::new(Vec::with_capacity(candidates.len())),
    };
    let workers = budget.books.clamp(1, candidates.len().max(1));
    if workers > 1 {
        // 批次的 span 要跟着进 worker：`thread::scope` 起的线程**不**继承调用线程的当前
        // span，不接一句的话「整个批次一个 span」那条诊断（§4.1 第 4 条）就只看得到调用
        // 线程上那一小部分，慢在哪一步的账目会散掉。
        let span = tracing::Span::current();
        std::thread::scope(|scope| {
            // 两个都是借出去引用：`Span` 不是 `Copy`，按值捕获的话 `map` 那个 `FnMut`
            // 闭包只能把它的 span 给一路 worker。
            let batch = &batch;
            let span = &span;
            let handles: Vec<_> = (0..workers)
                .map(|_| scope.spawn(move || span.in_scope(|| batch.run())))
                .collect();
            for handle in handles {
                if handle.join().is_err() {
                    tracing::error!("import: 一条导入线程崩了，缺的那几本在调用线程上补跑");
                }
            }
        });
    } else {
        batch.run();
    }
    batch.finish()
}

/// 「这一份内容正在被哪一本落地」那张表：一张 `Mutex<HashSet>` 加一条 `Condvar`。
///
/// 为什么非要它：判重那条「库里已有同一内容就只挂来源、不再落副本」的门（§九）在串行时
/// 由**顺序本身**保证 —— 同内容的第二本必然看见第一本刚落好的那一行。按书并行把这条隐含
/// 顺序拿走了：两本同时 `book_by_hash`、同时查到「没有」，就各落一行、各存一份副本。
/// 数据库挡不住这件事，`content_hash` 上建的是**普通索引**而不是 UNIQUE
/// （`store/schema.rs` 的 `CREATE INDEX idx_books_content_hash`），而两行同内容就是两套
/// 进度、两套批注。
///
/// 号从「查库」一直握到「哈希写进那一行」：于是第二次查与第一次查之间插不进来第二本。
/// 占不到号的**等**持有者收手 —— 不能直接判「已存在」：持有者可能失败（坏包、落库挂），
/// 那样号空出来，等的这本要自己走一遍，而串行时它本来就是会正常落地的。
///
/// 与 [`Store::claim_reindex`] 是同一类门（一个号只被占走一次），但两张表各管自己的寿命：
/// 那张挂在 `Arc<Store>` 上跟着书库活，这张跟着进程活 —— 号只在「正在落」这一段里有意义，
/// 落完之后「这本书已存在」由库里那一行自己说，不需要这里记着。
#[derive(Default)]
struct Claims {
    /// 正在被落地的内容哈希。
    taken: Mutex<HashSet<String>>,
    /// 等在某一个号上的那些本。退号时叫醒全部，理由见 [`Hold`]。
    ready: Condvar,
}

static CLAIMS: LazyLock<Claims> = LazyLock::new(Claims::default);
pub(super) fn claim_hash(hash: &str) -> Hold {
    CLAIMS.hold(hash)
}

impl Claims {
    /// 占到 `hash` 这一格，回一枚把它守到作用域结束的令牌。
    ///
    /// 已被占就在 `ready` 上等，等它空出来再占 —— 占号与「已空出来」这两件事在同一把锁里
    /// 完成，所以醒来之后不会有第三本插到前面去。
    fn hold(&'static self, hash: &str) -> Hold {
        let mut taken = self.taken.lock().unwrap_or_else(|e| e.into_inner());
        while taken.contains(hash) {
            taken = self.ready.wait(taken).unwrap_or_else(|e| e.into_inner());
        }
        taken.insert(hash.to_string());
        Hold {
            hash: hash.to_string(),
        }
    }
}

/// 占着的一个号。离开作用域时退号 —— 含提前 `return Err` 与 panic。
///
/// 用它而不是在 [`import_one`] 里手写退号，理由是那函数有十来条退出路径：漏掉任何一条
/// 就是「这一格被永远占着」，而后面每一本同内容的书都会等在一个没人会放的号上。
pub(super) struct Hold {
    hash: String,
}

impl Drop for Hold {
    fn drop(&mut self) {
        CLAIMS
            .taken
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.hash);
        // **`notify_all` 是必需的，不是保守**：等在一条 `Condvar` 上的是**好几个不同的哈希**，
        // `notify_one` 叫醒的那条如果等的是别的号，它就又回去睡，而本号上真正该醒的那条
        // 一次都没被叫醒 —— 于是它等在一个已经空出来的号上。
        // 这一条与 `infra::task::Queue::enqueue` 那次丢唤醒是同一个形状。
        CLAIMS.ready.notify_all();
    }
}

/// 一个候选的完整旅程。**一个归档候选可能是几本**：里面每一条像书的条目各走一遍
/// [`import_one`]，各得一条结论。交回的是 `(书名, 结果)` 的清单，非包的那一族就是一条。
///
/// 为什么要退那一层，而且退在**算身份之前**：身份、判定、落地全都算在解出来的那一份字节上，
/// 于是同一本书裸着送来与打包着送来是同一本（判重命中），而书库里躺着的那一份**永远能直接
/// 打开** —— provider 那一边不需要认识容器，`Format` 也不用为「躺在 zip 里」多立一档。
/// EPUB 与 CBZ 自己就是书，[`sniff::archive_shape`] 给的是 `Book`，不退（把 EPUB 的条目
/// 挨个抽出来，那本书就散了）。
///
/// **四种归档共用这一条退层的路**：ZIP、RAR、7z、tar。**只退一层**，且只按内容退：
/// 解出来的那一份不再退一次 —— 里面是 EPUB 的话它自己就是容器（上面那条），里面再套一个
/// `合集.zip` 的话就当不是书拒掉，不替读者猜他想要的是哪一本。名字在这整步里没有任何
/// 作用：`x.zip`、`书.fb2.zip`、`合集.rar` 走的是同一条路。
fn import_candidate(
    source: &dyn Source,
    candidate: &Candidate,
    book_dir: &Path,
    staging: &Path,
    store: &Store,
    ctx: &StageCtx,
    per_book: usize,
) -> Vec<(String, Result<Imported>)> {
    let name = candidate.name.clone();
    if matches!(
        source.kind(),
        super::ImportKind::Watch | super::ImportKind::Scan
    ) && let Some(path) = source
        .persistent_local(candidate)
        .and_then(|p| p.canonicalize().ok())
    {
        match store.source_is_suppressed(&path.to_string_lossy()) {
            Ok(true) => return Vec::new(),
            Ok(false) => {}
            Err(error) => return vec![(name, Err(error.into()))],
        }
    }

    // 步 2：来源把字节写进暂存区，交回那份 `.part`。
    let (staged, captured_hash) = match source.stage_with_hash(candidate, staging, ctx) {
        Ok(staged) => staged,
        Err(err) => return vec![(name, Err(err))],
    };
    // 契约守一句：写出了暂存区就不收下。也**不删**它 —— 那可能是读者自己的文件，
    // 我们只是没收下这本书而已。
    if staged.parent() != Some(staging) {
        tracing::error!("import: 《{name}》被写到了暂存区之外（{staged:?}），拒绝收下");
        return vec![(
            name,
            Err(Error::Io {
                path: staged,
                source: "stage 写出了暂存区之外".to_string().into(),
            }),
        )];
    }

    // 步 2 后半：是包就退一层，逐条各算一本。不是包（或开不出目录、或里面没有像书的
    // 条目）就原样走下去 —— 「坏包」与「这不是书」那两句话由判定层说，不在这里编第三套。
    let sniff::ArchiveShape::Holds(books) = sniff::archive_shape(&staged) else {
        let original = match &candidate.origin {
            super::Origin::Local(path) => Some(path.as_path()),
            _ => None,
        };
        let persistent = source.persistent_local(candidate);
        let one = super::versioned::import_staged(
            store,
            super::versioned::StagedImport {
                name: &name,
                staged,
                book_dir,
                staging,
                per_book,
                original,
                persistent: persistent.as_deref(),
                hash: captured_hash,
                source_link: source.source_link(candidate),
            },
            ctx,
        );
        return vec![(name, one)];
    };
    tracing::info!(
        "import: 《{name}》是一个包，逐条抽出 {} 本（其余条目不算书，留在外面那一层）",
        books.len()
    );
    let mut outcomes = Vec::with_capacity(books.len());
    for book in books {
        // 取消检查点落在**本与本之间**：一个包里的每一条就是一本，与逐本串行那一路同一条
        // 纪律（`plans/基础设施.md` §1.2 第 5 条要的是单元之间，不是单元中间）。已经落下
        // 的那些是真的，界面那句「已取消」重查一次架面就看得见。
        if ctx.cancel().triggered() {
            break;
        }
        let shown = book.name;
        let part = staging.join(staged_name(&shown));
        match extract_entry(&staged, book.family, &book.entry, &part) {
            Ok(written) => {
                // 局部变量别叫 `display`：tracing 的消息模板里那会把宏自己的辅助函数
                // 遮掉，报出来是一条看不懂的 `doesn't implement std::fmt::Display`。
                tracing::debug!("import: 抽出《{}》{} 字节 → 走一遍管线", shown, written);
                let one = super::versioned::import_staged(
                    store,
                    super::versioned::StagedImport {
                        name: &shown,
                        staged: part,
                        book_dir,
                        staging,
                        per_book,
                        original: None,
                        persistent: None,
                        hash: None,
                        source_link: None,
                    },
                    ctx,
                );
                outcomes.push((shown, one));
            }
            Err(why) => {
                // 抽不出来的那一条只让它自己失败。剩下的条目照走，而外面那一层仍然退掉 ——
                // 「判定或解析失败的候选不能在库里留下记录」这一条靠 [`import_one`] 自己的
                // 回滚保证，不靠这里少抽一条。
                abandon(&part);
                outcomes.push((
                    shown.clone(),
                    Err(Error::Rejected {
                        name: shown,
                        why,
                        reject: ImportReject::CorruptContainer,
                        source: None,
                    }),
                ));
            }
        }
    }
    // 外面那一层用完了：库里没有它，暂存区也不该留着它。
    abandon(&staged);
    outcomes
}

/// 从包里抽出一本时允许写下的最大字节数（解压**后**）。
///
/// 必须有这么一个数，因为来路是读者机器上任意一个归档（整机扫描、传书、监听都交得出），
/// 而目录里声明的解压长度会说谎。取 512 MiB 是拍的：真书里最宽的那一族（一个包里
/// 装着整本漫画）量不到更高，而它同时是磁盘与内存的上限。超过就当场拒那一条，
/// 不猜它是炸弹还是大书 —— 两者对读者的动作是同一件事（把这个包自己解开再导）。
const MAX_UNWRAP_BYTES: u64 = 512 << 20;

/// 从包里抽出一本：写进暂存区，交回写了几个字节。失败是 `(String)` —— 那句给人看的话,
/// 由调用方包成一次带类别的拒绝，而不是在这里猜该算哪一类。
///
/// 上限封的是**实际写出的字节数**，不是目录声明的那一个：声明值正是会说谎的那一格
/// （zip 炸弹写 0、解出几个 GB），而整机扫描会把读者机器上任意一个归档交给我们。
/// 四种容器都由 [`crate::content::format::archive::extract_to`] 走流式（一块一块写），
/// 所以这个数同时是磁盘与内存的上限 —— 一批最多四本在飞，每本各有一份这样的缓冲。
fn extract_entry(
    pack: &Path,
    family: crate::content::format::archive::Family,
    entry: &str,
    dest: &Path,
) -> std::result::Result<u64, String> {
    crate::content::format::archive::extract_to(pack, family, entry, dest, MAX_UNWRAP_BYTES)
}

/// 删掉管线自己刚写下的一份文件（暂存区里那份 `.part`，或刚 rename 出去的副本）。
///
/// 两处调用都确定它**没有**任何库行认领：`.part` 还没进过库；副本那条路上，新增的行
/// 已经先删掉、自愈那一行已经退回原位置。旧的 `discard_copy` 要问「还有别的行指着它吗」
/// 是因为那时副本按建议名落在书库里、重名覆盖会让上一本正在用它 —— 哈希名之下没有
/// 那种撞名，那一句问话现在由「先删行、再退位置、最后删副本」这个顺序代劳。
///
/// 删失败只记日志，不改调用方那条返回值：**拒绝的理由比一个没删掉的文件重要**，
/// 而没删掉的那份下次启动会被 [`sweep_partials`] 收掉。
pub(super) fn abandon(path: &Path) {
    if let Err(err) = std::fs::remove_file(path) {
        tracing::warn!("import: 该删的 {path:?} 没删掉：{err}");
    }
}

/// 重扫一本**已经入库**的书：文件换了，或者索引是旧算法算的。
///
/// 与导入共用「解析 → 落库」那一段（就是 `ingest` / `index` 那道分界后面的两步），
/// 唯一不同的是**不改 `book_id`** —— 进度、批注、阅读统计都挂在它上面。章序按同一套
/// 规则重算，所以多数情况下位置还对得上；对不上的由 `Locator` 的夹回本章那条路兜住。
///
/// 顺手把内容身份补上（懒补的触点之一）。**不改文件名**：§九 那条门要的是「`reindex`
/// 不重读来源、不改文件名、不新增记录」，把旧名字的副本改成哈希名是补齐作业的活，
/// 它做完 rename 才回写 `path`（见 [`backfill_copies`](crate::ingest::import::backfill_copies)）。
pub fn reindex(store: &Store, book_id: i64) -> Result<()> {
    super::versioned::reindex_snapshot(store, book_id, &StageCtx::detached())
}

/// 把 [`reindex`] 投进作业池的 [`Cleanup`](Priority::Cleanup) 档 —— 那一档的说明里
/// 本来就写着「重索引」（`plans/基础设施.md` §1.1），封顶 1，界面空闲才轮到它。
///
/// 存在的理由是一条已决：**开书那一屏不等重扫**（`plans/阅读管线.md` §七）。旧索引是能读的，
/// 三处兜底早写好了 —— `INDEX_VERSION` 那条注释自己记着「重扫前旧文本当内联样式兜底」
/// （`content::format::INDEX_VERSION`），`char_len` 为 NULL 时全书进度那条 SQL 走 unknown
/// 计数（`store::Store::progress_of`），位置认不出来当作「回容器重读一遍」而不是空章
/// （`store::content::load_flow_in`）。所以判据说「这份索引是旧的」之后的正确处置是
/// 照旧打开 + 投一趟后台，而不是原地把整本书重读一遍 —— 重扫必然全量重读，
/// 见 `plans/导入管线.md` §八 第 3 条。
///
/// 返回的句柄给测试等完成用；生产调用方直接丢（同界面那句「补齐这一趟没有界面读数」）：
/// **新索引下次开书才生效**，正看着的这一屏不重建，读者的位置不会因为一次后台维护而跳。
/// 领不到号也返回 `None`：同一本只允许一趟在飞，试过且失败的在本进程内不再自动重投
/// （两道门都在 [`Store::claim_reindex`]）。号由作业自己退，所以投出去的那一趟无论成败
/// 都不会把号漏在飞。
pub fn schedule_reindex(
    manager: &TaskManager,
    store: Arc<Store>,
    book_id: i64,
) -> Option<TaskHandle<()>> {
    if !store.claim_reindex(book_id) {
        return None;
    }
    Some(manager.submit_with(Priority::Cleanup, move |_| {
        let outcome = reindex(&store, book_id);
        store.note_reindex(book_id, outcome.is_ok());
        outcome.map_err(|err| -> TaskError { Box::new(err) })
    }))
}
