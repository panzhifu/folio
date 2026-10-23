//! 管线本体：§一 那七步的代码形状。
//!
//! 顺序就是文档那张图：`stage`（字节由 [`sources`](super::sources) 那一半送到暂存区）→ 算身份
//! → 判定 → 判重占号 → 换名进小屋 → [`ingest`] 与 [`index`] 的分界 → 落库。批次引擎
//! （[`Batch`]）只管「同时导几本、谁领哪一本、结清的记在哪」，一本的完整旅程在
//! [`import_candidate`] 与 [`import_one`]。
//!
//! [`reindex`] 也住在这里，因为它吃的正是 [`index`] 那同一段 —— 分界后面那半份只有一份代码。

use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use crate::content::document;
use crate::content::format::Format;
use crate::content::format::ParseBudget;
use crate::content::format::epub::MAX_PARSE_WORKERS;
use crate::content::format::sniff;
use crate::infra::error::Error;
use crate::infra::error::ImportReject;
use crate::infra::error::IoResultExt as _;
use crate::infra::error::Result;
use crate::infra::paths;
use crate::infra::task::Priority;
use crate::infra::task::StageCtx;
use crate::infra::task::TaskError;
use crate::infra::task::TaskHandle;
use crate::infra::task::TaskManager;
use crate::ingest::identity;
use crate::store::BookRow;
use crate::store::Store;

use super::Candidate;
use super::ImportOutcome;
use super::ImportReport;
use super::Imported;
use super::Registered;
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
            let books = self.import_at(at);
            let mut landed = self.landed.lock().unwrap_or_else(|e| e.into_inner());
            self.record(&mut landed, at, books);
        }
    }

    /// 走完第 `at` 号候选的整趟旅程，顺手把「这一格结清了」的读数报出去。
    ///
    /// 读数按**结清**的数报，不按领号报：并行时领号在前、结清在后，
    /// 用领号数会画出一条会后退的进度。一个包算一格 —— 总数数的是候选，
    /// 不是抽出来的本数，否则一个 900 条目的合集包会把进度条顶过 100%。
    fn import_at(&self, at: usize) -> Vec<(String, Result<Imported>)> {
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
        let done = self.done.fetch_add(1, Ordering::Relaxed) as u64 + 1;
        self.ctx.item(done, self.candidates.len() as u64);
        books
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
    ) {
        for (name, result) in books {
            let mut from = self.candidates[at].clone();
            from.name = name;
            into.push((
                at,
                ImportOutcome {
                    candidate: from,
                    result,
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
                let books = self.import_at(at);
                self.record(&mut rows, at, books);
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
struct Hold {
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

/// 一个候选的完整旅程。**一个 ZIP 候选可能是几本**：里面每一条像书的条目各走一遍
/// [`import_one`]，各得一条结论。交回的是 `(书名, 结果)` 的清单，非包的那一族就是一条。
///
/// 为什么要退那一层，而且退在**算身份之前**：身份、判定、落地全都算在解出来的那一份字节上，
/// 于是同一本书裸着送来与打包着送来是同一本（判重命中），而书库里躺着的那一份**永远能直接
/// 打开** —— provider 那一边不需要认识容器，`Format` 也不用为「躺在 zip 里」多立一档。
/// EPUB 与 CBZ 自己就是书，[`sniff::zip_shape`] 给的是 `Book`，不退（把 EPUB 的条目挨个
/// 抽出来，那本书就散了）。
///
/// **只退一层**，且只按内容退：解出来的那一份不再退一次 —— 里面是 EPUB 的话它自己就是
/// 容器（上面那条），里面再套一个 `合集.zip` 的话就当不是书拒掉，不替读者猜他想要的
/// 是哪一本。名字在这整步里没有任何作用：`x.zip` 与 `书.fb2.zip` 走的是同一条路。
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

    // 步 2：来源把字节写进暂存区，交回那份 `.part`。
    let staged = match source.stage(candidate, staging, ctx) {
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

    // 步 2 后半：是包就退一层，逐条各算一本。不是包（或开不出中央目录、或里面没有像书的
    // 条目）就原样走下去 —— 「坏包」与「这不是书」那两句话由判定层说，不在这里编第三套。
    let sniff::ZipShape::Holds(books) = sniff::zip_shape(&staged) else {
        let one = import_one(&name, staged, book_dir, staging, store, per_book);
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
        match extract_entry(&staged, &book.entry, &part) {
            Ok(written) => {
                // 局部变量别叫 `display`：tracing 的消息模板里那会把宏自己的辅助函数
                // 遮掉，报出来是一条看不懂的 `doesn't implement std::fmt::Display`。
                tracing::debug!("import: 抽出《{}》{} 字节 → 走一遍管线", shown, written);
                let one = import_one(&shown, part, book_dir, staging, store, per_book);
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
/// 必须有这么一个数，因为来路是读者机器上任意一个 zip（整机扫描、传书、监听都交得出），
/// 而中央目录里声明的解压长度会说谎。取 512 MiB 是拍的：真书里最宽的那一族（一个包里
/// 装着整本漫画 CBZ）量不到更高，而它同时是磁盘与内存的上限。超过就当场拒那一条，
/// 不猜它是炸弹还是大书 —— 两者对读者的动作是同一件事（把这个包自己解开再导）。
const MAX_UNWRAP_BYTES: u64 = 512 << 20;

/// 从包里抽出一本：写进暂存区，交回写了几个字节。失败是 `(String)` —— 那句给人看的话,
/// 由调用方包成一次带类别的拒绝，而不是在这里猜该算哪一类。
///
/// 上限封的是**实际写出的字节数**，不是中央目录声明的那一个：声明值正是会说谎的那一格
/// （zip 炸弹写 0、解出几个 GB），而整机扫描会把读者机器上任意一个 zip 交给我们。
/// 走的是流式（`io::copy` 一块一块写），所以这个数同时是磁盘与内存的上限 —— 一批最多四本
/// 在飞，每本各有一份这样的缓冲。
fn extract_entry(pack: &Path, entry: &str, dest: &Path) -> std::result::Result<u64, String> {
    let mut zip = crate::content::format::zip::Zip::open(pack)
        .map_err(|err| format!("包读不出来：{err:#}"))?;
    zip.extract_to(entry, dest, MAX_UNWRAP_BYTES)
        .ok_or_else(|| format!("那一条读不出来，或解压后超过 {} MB", MAX_UNWRAP_BYTES >> 20))
}

/// 一本书的完整旅程，就是 §一 那张图 —— 而**那道分界在这里是两次调用**：[`ingest`]
/// （步 3–5 前半）交回一份已经安全进暂存区的字节，[`index`]（步 6–7）把它变成库里那一行。
/// `reindex` 走的是同一个 [`index`]，所以「解析 → 封面 → 落库 → 身份」这一段只有一份代码
/// —— 这两段从前各写一遍，已经漂过一次（身份列那一格：一路容错、一路往外抛）。
///
/// 失败路径上的三个去处要说清，因为它们是「库里有这一行就等于能打开」那句话的全部内容：
///
/// - 判定或哈希这一步失败 → 删掉**暂存区里**那份 `.part`，书库目录里什么都没发生过。
/// - 落地之后失败（解析、落库）→ 删掉刚 rename 出去的那份副本；如果这一支是自愈
///   （改已有那一行的位置），位置也退回原来那个不存在的文件，等于回到动手之前。
/// - 成功 → 库里一行、书库里一份按内容命名的副本，两者同时存在。
///
/// `name` 是这一本对读者显示的名字（包里的条目就用它自己那个文件名），`staged` 是
/// 管线刚写下的一份**可以直接读**的字节 —— 是包的话上面那一步已经退开了。
fn import_one(
    name: &str,
    staged: PathBuf,
    book_dir: &Path,
    staging: &Path,
    store: &Store,
    per_book: usize,
) -> Result<Imported> {
    let pending = match ingest(name, staged, staging, store)? {
        Ingested::Duplicate { book_id } => return Ok(Imported::Duplicate { book_id }),
        Ingested::Ready(pending) => pending,
    };
    index(
        store,
        Indexing {
            name: &pending.name,
            path: &pending.readable,
            hash: Some(&pending.hash),
            // 这一本内部起几路是外面分好的（[`Budget::per_book`]）：一本自己按整机核数
            // 猜的话，一批同时飞 M 本就是 M 份整机预算。
            budget: ParseBudget::AtMost(per_book),
            landing: Landing::New {
                book_dir,
                format: pending.format,
                heal: pending.heal.as_ref(),
            },
        },
    )
}

/// [`ingest`] 的两种结论 —— §一 那张图在第 4 步就分岔了。
enum Ingested {
    /// 库里已经有同一份内容、而它记的副本还在：这次**不落地、不新增行**。
    Duplicate { book_id: i64 },
    /// 分界后面那半段要的全部东西。（`Box`：`Pending` 背着 `BookRow` 与两份路径，
    /// 直接装进枚举会让每一个「已存在」的结论也按最大那格付钱。）
    Ready(Box<Pending>),
}

/// §一 那道分界**之前**的产物：字节已经进这一本自己的小屋、身份算过、格式判过、
/// 判重也问过了。[`index`] 只需要这一格东西。
///
/// 两样东西的所有权留在这里，因为它们都得活过 [`index`]：小屋（`Drop` 收壳）与判重
/// 那一格的号。号要握到落库结束是因为它守的是「同内容的第二本不许各落一行」，而那件事
/// 要到库里那一行带上身份之后才算成（见 [`Claims`]）。
struct Pending {
    /// 这一本对读者显示的名字（进日志，也进拒绝的理由）。
    name: String,
    /// 小屋里那个「人看得懂的名字」——解析吃它。
    readable: PathBuf,
    /// 内容身份：副本名与库里那一列都从它来。
    hash: identity::ContentHash,
    /// 三层判定给出的格式：副本的扩展名问它要。
    format: Format,
    /// 判重问出来的那一格：`Some` = 命中的那一行、而它记的副本已经不在了（自愈）；
    /// `None` = 库里没有这一本，落库要新增一行。
    heal: Option<BookRow>,
    /// 这一本在暂存区里那间只有它自己的小屋。
    _area: StagingArea,
    /// 占着的判重号，作用域就是这一格。
    _claim: Hold,
}

/// 落库那几句的**测试内注错口**：`which` 是这一句的名字，`call` 是真的那一句。
///
/// 为什么非要它：§一 步 7 那条「库里有这一行就等于能打开」全靠失败时把上一步退回去，
/// 而那几种失败全都是「库说失败」—— `Store` 自己没有注错入口（`add_book` 在一个事务里，
/// 从外面造不出半截状态），所以不假造就钉不住「这一段确实调了 [`unwind_landing`]」。
/// 生产编译里 `#[cfg(not(test))]` 那一份就是直接调用，一句多余的话都不留。
#[cfg(not(test))]
fn landing<T>(which: &'static str, call: impl FnOnce() -> Result<T>) -> Result<T> {
    let _ = which;
    call()
}

// [`landing`] 的测试那一版把旗子按**线程**放：一批并行时真正导书的是 worker 线程，
// 而进程级的旗会把同时在跑的别的测试一起带红（本仓库的并行污染有先例）。钉这一格的
// 测试因此都把并行度设成 1 —— 那时 [`Batch::run`] 就在调用线程上跑，这一格才够得着。
#[cfg(test)]
thread_local! {
    static FAIL_LANDING: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
}

/// 让落库的某一句在下一次假失败（`None` = 收回去）。**只在测试里存在**，
/// 而且只对调用这条线程有效（见上面那段）。
#[cfg(test)]
pub(crate) fn fail_landing(which: Option<&'static str>) {
    FAIL_LANDING.with(|slot| slot.set(which));
}

#[cfg(test)]
fn landing<T>(which: &'static str, call: impl FnOnce() -> Result<T>) -> Result<T> {
    if FAIL_LANDING.with(|slot| slot.get()) == Some(which) {
        return Err(Error::Io {
            path: PathBuf::from(which),
            source: "测试按下去的假失败".to_string().into(),
        });
    }
    call()
}

/// §一 步 3、4、5 的前半：算身份 → 三层判定 → 判重 → 在暂存区里换成看得懂的名字。
///
/// 这一段与格式无关（除了问注册表要一个扩展名），所以它失败的时候只碰暂存区，
/// 书库根从来没被碰过 —— §五 那条「判不成的候选不能在库里留下记录」就是这一段的职责。
/// 交回 `Ok` 就等于这份字节已经排到了队：号占着、小屋开着、名字也换好了。
fn ingest(name: &str, staged: PathBuf, staging: &Path, store: &Store) -> Result<Ingested> {
    // 步 3 前半：身份算在**管线自己刚写下的字节**上。来源那份文件在读与写之间被改过
    // （同步盘正在写、下载器重传）都不影响这里的结论 —— 这就是把写盘收在一处的全部理由。
    let hash = match identity::sha256_of_path(&staged) {
        Ok(hash) => hash,
        Err(err) => {
            abandon(&staged);
            return Err(err).io_path(&staged);
        }
    };

    // 步 3 后半：三层判定排在落地之前（§五 那条硬规则）。判据是内容，`name` 只贡献
    // 扩展名那一条线索 —— 一个叫 `a.zip` 的真 EPUB 会在这里被收下。
    let verdict = sniff::identify(name, &staged);
    let Some(format) = verdict.format() else {
        let (reject, why) = verdict
            .rejection()
            .expect("判定拿不出 format 时必定给得出拒绝的理由");
        abandon(&staged);
        return Err(Error::Rejected {
            name: name.to_string(),
            why,
            reject,
            source: None,
        });
    };

    // 步 4：判重按完整哈希。**命中之后还要 stat 那条已有记录的副本**：不验证就命中，
    // 等于把一条指向不存在文件的记录再确认一次（读者手工删过文件、或一次只同步了库
    // 没同步文件，都会造成这种状态）。
    //
    // 查之前先把这一格的号占到，握到落库结束（见 [`Claims`]）：并行导同一份内容的两本
    // 就是靠它变回「一本先导、一本等完再看结果」。
    let hash_hex = hash.to_hex();
    let claim = CLAIMS.hold(&hash_hex);
    let heal = match landing("book_by_hash", || store.book_by_hash(&hash_hex)) {
        // 问不出「库里有没有这一本」就当这本没进来 —— 但**先把那份字节退掉**：§五 那条
        // 硬规则要的是「判不成就只清暂存」，库出错不等于暂存区有权留着东西。
        Err(err) => {
            abandon(&staged);
            return Err(err);
        }
        Ok(Some(row)) if Path::new(&row.path).exists() => {
            abandon(&staged);
            tracing::info!("import: 《{name}》与第 {} 本同内容，来源挂到那一行", row.id);
            return Ok(Ingested::Duplicate { book_id: row.id });
        }
        // 命中但旧副本已经不在了 → 继续落地，并把那一行的位置改指过来。这是**自愈**：
        // 把一个已经坏掉的库修回来，而不是又拒读者一次。
        Ok(Some(row)) => {
            tracing::warn!(
                "import: 第 {} 本记的副本 {} 已经不在了，用这份同内容的文件接回去",
                row.id,
                row.path
            );
            Some(row)
        }
        Ok(None) => None,
    };

    // 步 5 的前半：先把那份字节改成一个**人看得懂的名字**再解析。
    //
    // 为什么这一步不能省：单文件格式的书名与第一章名回落成**文件主名**（书名那一格在
    // `format::text` 的 `TEXT.title` 与 `format::markdown` 的 `first_heading_or_stem`，
    // 章名那一格在两族的 `split` 里各有一处回落到主名），而最终副本按内容
    // 命名 —— 直接拿哈希名去解析，架子上每一本 TXT 都显示成 `tuqddpm7nrheuceu`，
    // 目录里第一章也叫这个。这条是实测翻出来的（哈希名一接上，标题就跟着变成哈希）。
    // 顺带把失败回滚变干净了：解析没过的东西**根本没进过书库根**。
    //
    // 那个名字**必须由这一本自己的小屋来装**：它只由「建议名 + 扩展名」定，一批里两本
    // 不同内容、同一个文件名的候选（整机扫描最容易凑出来）直接撞在同一个路径上 ——
    // 甲拿着它在解析，乙把同名文件盖进来，甲再把**乙的字节**改名成**甲的哈希名**。
    // 库里那一行于是记着甲的身份、躺着乙的内容，而下次真导乙会被判重认成「已存在」，
    // 那份字节再也进不了库。门牌号用哈希前缀：不同内容必不同门牌，同一份内容的那两本
    // 在 [`Claims`] 上本来就排过队；名字里不含哈希之外的东西，书名回落照样是干净的。
    let area = match StagingArea::open(staging, &hash) {
        Ok(area) => area,
        Err(err) => {
            abandon(&staged);
            return Err(err).io_path(&staged);
        }
    };
    let readable = area.dir.join(format!(
        "{}.{}",
        readable_stem(name, &hash),
        format.copy_extension()
    ));
    if let Err(err) = std::fs::rename(&staged, &readable).io_path(&readable) {
        abandon(&staged);
        return Err(err);
    }

    Ok(Ingested::Ready(Box::new(Pending {
        name: name.to_string(),
        readable,
        hash,
        format,
        heal,
        _area: area,
        _claim: claim,
    })))
}

/// [`index`] 要吃的一格：一份能读的字节，加上它要落进哪一格。
///
/// 全是借用，所有权留在调用方 —— 导入那一路是小屋与判重的号（归 [`Pending`]），
/// 重扫那一路是库里那一行与它的副本。
struct Indexing<'a> {
    /// 这一本对读者显示的名字（进日志，也进拒绝的理由）。
    name: &'a str,
    /// 解析吃的那份字节：导入时是小屋里的可读名，重扫时是库里那一行记着的副本。
    path: &'a Path,
    /// 内容身份。`None` = 这一趟算不出（文件读不动），那一列就不写、交给补齐。
    hash: Option<&'a identity::ContentHash>,
    /// 这一本内部起几路解析：导入用批分好的那一档，重扫用整机那一档。
    budget: ParseBudget,
    landing: Landing<'a>,
}

/// 解析产物落进哪一格。两格的差别是**那份字节是谁的**，所以失败的处置正好相反：
///
/// - `New`：字节是管线刚写下的，失败要退掉它（它没进过库，没人认领）。
/// - `Rescan`：文件是**权威副本**，失败也不许删 —— 删了就等于把一本本来能打开的书弄没
///   了；§九 那条门要的是「`reindex` 不重读来源、不改文件名、不新增记录」。
enum Landing<'a> {
    /// 库里还没有这一行：解析过了才 rename 进书库根（同分区、原子），再新增一行 ——
    /// 或把自愈命中的那一行改指过来。
    New {
        book_dir: &'a Path,
        format: Format,
        heal: Option<&'a BookRow>,
    },
    /// 行已经在：只把这一份解析产物写回那一行。
    Rescan { book_id: i64 },
}

/// §一 那道分界**后面**那半段：解析 → 封面 →（新落地才）定名落地 → 落库 → 身份。
///
/// 导入与重扫共用这一个函数。交回的 `Imported` 对重扫那一路只是「那一行的产物写好了」，
/// [`reindex`] 不看它（它要的就是 `Result<()>`）。
fn index(store: &Store, idx: Indexing<'_>) -> Result<Imported> {
    // 步 6：解析。
    let mut document = match document::open_with(idx.path, idx.budget) {
        Ok(document) => document,
        Err(err) => {
            // 新落地那一路只退掉暂存区里那一份，书库根从来没被碰过；重扫那一路不动文件。
            if let Landing::New { .. } = idx.landing {
                abandon(idx.path);
            }
            let why = format!("{err:#}");
            return Err(Error::Rejected {
                name: idx.name.to_string(),
                why,
                // 加密那一档由 provider 判得出来（它读到了 `encryption.xml`），别把它压成
                // 「读不动」：前者读者的动作是换一本无 DRM 的书，后者是「重新导入一次试试」。
                // 其余仍算「读不动」—— `UnsupportedFormat` 那一路走的是**容器结构**读不出，
                // 到这里书已经被判成 EPUB 了，说「格式不支持」会把人引去换格式。
                reject: match &err {
                    crate::Error::Encrypted { .. } => ImportReject::Encrypted,
                    _ => ImportReject::Unreadable,
                },
                source: Some(Box::new(err)),
            });
        }
    };

    // 封面落盘（`plans/导入管线.md` §八 第 4 条）。排在这里的两个理由：
    //
    // 1. **必须在 `save_document` 之前**：`save_flow_in` 要把 `cover_hash` /
    //    `cover_mime` 两列写进库，而那两个数是这一步算出来的。
    // 2. **失败不阻断**：封面是装饰，不是内容的入口。写不成（磁盘满、没权限、
    //    路径太长）就只是「这次没有封面文件」，库里那两列留空。
    //    **没有另一条兜底**：v16 把 `books.cover` 那个 BLOB 整列删了，封面只活在
    //    `covers/` 那个文件里，所以这一步失败的结果就是这本书没有封面、界面画占位
    //    （取舍写在 `plans/导入管线.md`「封面字节彻底出库（v16）」那一节）。
    //
    // 书库根从**这本副本自己的路径**推，所以库层不需要持有书库位置 —— 那是读者确认过的
    // 目录，core 不知道也不该知道。新落地那一路例外：那份字节还在暂存区的小屋里，
    // 从它自己往上推会推进 `.staging/<哈希>` 那一层，所以根得由调用方给。
    let root = match &idx.landing {
        Landing::New { book_dir, .. } => (*book_dir).to_path_buf(),
        Landing::Rescan { .. } => crate::store::book_root(idx.path),
    };
    land_cover(&root, document.as_mut());

    // 步 5 的后半：定名落地。**只有新落地那一路有这一步**（重扫不改文件名）。
    // `.staging` 就在书库底下 → 同分区，rename 原子且 O(1)。扩展名来自**判定**
    // （不是来源那个名字），而且要问注册表要它认得的那一个：副本的再一次识别
    // 走的就是那张表（`Format::copy_extension`）。
    let mut dest = idx.path.to_path_buf();
    if let Landing::New {
        book_dir, format, ..
    } = &idx.landing
    {
        let hash = idx
            .hash
            .expect("新落地那一路必定带着算好的身份（判重与副本名都靠它）");
        dest = book_dir.join(hash.file_name(format.copy_extension()));
        // 漫画那一族在步 6 已经把整本解到磁盘缓存了，而**缓存的键里带着这个路径**。
        // 所以旧键要在改名之前算，改完名把那一格跟着搬过去 —— 不搬就是白解一遍：
        // 开书时按新名字算出的键对不上任何东西，几百页的图片再倒出来一次。
        let comic_cache_dir = crate::content::format::comic_cache::dir_for(idx.path);
        if let Err(err) = std::fs::rename(idx.path, &dest).io_path(&dest) {
            abandon(idx.path);
            return Err(err);
        }
        crate::content::format::comic_cache::relocate(&comic_cache_dir, &dest);
    }

    // 步 7：落库。自愈那一支**不新增行** —— 同一份内容有两行就是两套进度、两套批注，
    // 而读者要的只是原来那一本回来了。
    let title = document.meta().title.clone();
    let landed = match &idx.landing {
        Landing::New { heal: None, .. } => {
            landing("add_book", || store.add_book(&dest, document.meta()))
        }
        Landing::New {
            heal: Some(row), ..
        } => landing("retarget_book_path", || {
            store.retarget_book_path(row.id, &dest)
        })
        .map(|_| row.id),
        // 重扫：行早就在，这里只是顺手刷新 `size` / `mtime`（判定「文件换没换」靠的就是
        // 这两个数），发回来的新行号不用 —— 进度、批注、统计都挂在原来那一格上。
        Landing::Rescan { book_id } => {
            landing("add_book", || store.add_book(&dest, document.meta())).map(|_| *book_id)
        }
    };
    let book_id = match landed {
        Ok(book_id) => book_id,
        Err(err) => {
            // 行没能站住：那一行要么整个在（同一个事务里插的），要么根本没有。
            match &idx.landing {
                Landing::New { heal, .. } => unwind_landing(store, *heal, None, &dest),
                // 重扫那一路两边都不许动：行本来就在，副本是权威的。
                Landing::Rescan { .. } => {}
            }
            return Err(err);
        }
    };
    let saved = landing("save_document", || {
        store.save_document(book_id, document.as_ref())
    });
    if let Err(err) = saved {
        match &idx.landing {
            Landing::New { heal, .. } => unwind_landing(store, *heal, Some(book_id), &dest),
            // 重扫失败留旧索引：那一行还能打开，比「什么行都没了」好。
            Landing::Rescan { .. } => {}
        }
        return Err(err);
    }
    // 身份列**最后**写：前面任何一步失败都不会留下一条「有身份却没内容」的行。
    // 这一句自己失败不算这本没进来 —— 行与章节都在、书能打开，只是哈希还空着，
    // 而空着是懒补本来就允许的状态（补齐作业会重算它）。报成失败还会连带一条后果：
    // `note_reindex(false)` 让重扫过的那一本在本进程内不再自动重投。
    if let Some(hash) = idx.hash {
        let hash_hex = hash.to_hex();
        let written = landing("set_content_hash", || {
            store.set_content_hash(book_id, &hash_hex, identity::HASH_ALGO)
        });
        if let Err(err) = written {
            tracing::warn!("import: 第 {book_id} 本的内容哈希没写进去，交给补齐：{err:#}");
        }
    }
    tracing::info!("import: 《{title}》→ 第 {book_id} 本（{dest:?}）");
    Ok(Imported::Added(Registered {
        book_id,
        dest,
        title,
    }))
}

/// 把一份文档的封面写进 `<书库>/covers/`，并把描述填回 `doc.cover_ref`。
///
/// 三条规矩（`plans/导入管线.md` §八 第 4 条）：
///
/// - **按封面自己的字节哈希命名**，不是书的哈希：两本书共用一张封面时磁盘上
///   只有一个文件；重新解析或换 provider 也不该让封面改名。
/// - **已存在就不写**：重导同一本书是常见事（系列书、重扫整库），
///   哈希相同 = 字节相同，重写一遍只是白付一次 IO。这条让「重扫整库」安全。
/// - **失败只记日志，不回 Err**：封面是装饰。写不成时 `cover_ref` 留 `None`，而库里
///   再也没有字节可退（v16 删了 `books.cover` 那一列），结果就是这本书没有封面、
///   界面画占位。
///
/// 两族都走这一条。从前它开头挡了一句「只处理可重排」，理由写的是「定版式的封面走
/// `save_fixed`」—— 而 `save_fixed_in` 里根本没有那两列，于是**整族的封面算完就丢**：
/// 漫画一直是这样，PDF 接上首页栅格之后也会一样。现在两族写的是同一张
/// `covers/<hash>.<ext>`、同一对 `cover_hash` / `cover_mime` 列，界面不再问格式。
fn land_cover(root: &Path, doc: &mut dyn crate::content::document::Document) {
    let Some(bytes) = doc.cover().map(<[u8]>::to_vec) else {
        return;
    };
    if bytes.is_empty() {
        return;
    }
    let mime = cover_mime(&bytes);
    let written = match crate::content::format::cover::write(root, &bytes, &mime) {
        Ok(hash) => hash.to_hex(),
        Err(err) => {
            // 字节不进库（v16），所以这一次失败就是「这本书没有封面」——
            // 留着 `cover_ref` 为 `None`，界面画占位。不阻断导入：封面是装饰。
            tracing::warn!(
                path = %root.display(),
                "import: 封面没写进 covers/，这本书将没有封面：{err}"
            );
            return;
        }
    };
    let cover = crate::content::format::cover::CoverRef {
        hash: written,
        mime,
    };
    // **缩略图在导入时一起生成**（`plans/导入管线.md` §八「缩略图」那一段推荐的正是
    // 这一档）。这一版能这么选，是因为字节不再进库：封面只有一个来源（`covers/` 那个
    // 文件），所以「导入时生成」不会与「懒生成」抢同一份输入。
    //
    // 为什么放导入：导入是读者**唯一愿意等待**的时刻，而缩略图是「每本一次」的活。
    // 懒生成的代价文档写得很清楚 —— 第一次进一个空书架时几十张图同时开工，
    // 而那正是最要 CPU 的时刻（书架滚动）。
    //
    // 失败不阻断，也不记 warn：做不出来的那些格自己回 `None`，
    // 而它失败的原因（原图是坏字节）对读者来说与「这本书本来就没有封面」同一种表现。
    //
    // 用 `ensure_all` 而不是逐档 `ensure`：同一份封面字节只该解一次图（实测省下的
    // 那一次解码约 3-7 ms/本，而两档的产出逐字节不变）。
    crate::content::format::thumb::ensure_all(root, &cover, &bytes);
    doc.set_cover_ref(Some(cover));
}

/// 封面的 MIME：**只看字节头**。
///
/// 这一格只决定封面文件叫什么扩展名，不参与任何判读，所以不值得为它去问 manifest：
/// 真书里 `image/jpg`、`IMAGE/PNG` 这类声明写错比字节头写错常见得多（`ui/shared/images.rs`
/// 认图片格式就是因为这个才把字节头排在声明前面）。判据复用既有那一份
/// （`format::guess_media_type`），认不出就按 JPEG —— 它是封面最常见的编码，
/// 而认错扩展名最坏的后果是文件管理器双击用错程序打开。
fn cover_mime(bytes: &[u8]) -> String {
    // 复用既有的那份判据（`format::guess_media_type`，与 `ui/shared/images.rs`
    // 认图片格式同源）：封面这一格只影响文件扩展名，认不出就按 JPEG ——
    // 它是封面最常见的编码，而认错扩展名最坏的后果是文件管理器双击用错程序打开。
    crate::content::format::guess_media_type(bytes)
        .unwrap_or("image/jpeg")
        .to_string()
}

/// 暂存区里那份「给人看的路径」的主名：来源建议名去掉扩展名、剥掉文件系统不认的字符。
///
/// 洗不出任何东西时（纯符号的名字、空串）退回哈希前缀 —— 总得有个名字，而那本书确实
/// 只能靠内容认。截到 60 个字符：Android 与 Windows 对整条路径有上限，读者手里那种
/// 「第一章-最终修订版-备份-3.epub」这类长名不该把整条路径顶爆。
fn readable_stem(display_name: &str, hash: &identity::ContentHash) -> String {
    let stem = Path::new(display_name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let cleaned: String = stem
        .chars()
        .filter(|c| !paths::is_unsafe_name_char(*c))
        .take(60)
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        return hash.file_stem();
    }
    cleaned.to_string()
}

/// 一本在暂存区里那间只属于它的小屋（`.staging/<哈希前缀>/`）。
///
/// 第 5 步前半要把那份字节换成一个「人看得懂的名字」再解析，而那个名字由
/// 「来源建议名 + 判定出的扩展名」定，**不含任何唯一化成分**：一批里两本不同内容、
/// 同一个文件名的候选（并行度 ≥2 时，整机扫描的勾选清单最容易凑出这种一批）会抢同一个
/// 路径 —— 甲正拿着它解析，乙把同名文件盖进去，甲再把乙的字节改名成甲的哈希名。
/// 库里那一行于是记着甲的身份、躺着乙的内容。一间小屋挡住这件事，而屋里的文件名
/// 保持干净（单文件格式的书名回落成 `stem(path)`，屋名混进文件名的话标题就变脏了）。
///
/// 门牌号取内容哈希的前缀：不同内容必定不同门牌，同一份内容的两本在 [`Claims`]
/// 那一格上本来就已经排过队，所以这个门牌既防撞又不需要另发一个序号。
///
/// 走出作用域时把小屋收掉（含提前 `return Err` 与 panic —— 与 [`Hold`] 同一个理由）。
/// 只收得掉空壳：里面还有东西说明哪一步没退干净，那一处的账由 `abandon` 自己欠着，
/// 这里不越权删；启动时 `sweep_partials` 那条会把暂存区整棵子树当垃圾清掉。
struct StagingArea {
    dir: PathBuf,
}

impl StagingArea {
    /// 在 `staging` 下为这一本开一间小屋。
    fn open(staging: &Path, hash: &identity::ContentHash) -> std::io::Result<Self> {
        let dir = staging.join(hash.file_stem());
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }
}

impl Drop for StagingArea {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.dir);
    }
}

/// 把「已经 rename 进书库根」那一步退回去：删掉那份副本，并把库里那一行也摆回动手之前
/// 的样子 —— 新增那一支删行（行还在而副本没了就是「库里有行却打不开」，§一 步 7 那句
/// 「有这行就等于能打开」正是这么破的），自愈那一支把位置改回原来那个**不存在**的文件。
///
/// 四种失败走的都是这一段（`add_book`、`retarget_book_path`、`save_document` 的自增与
/// 自愈两支），所以收在一处：手写四遍的话，漏掉那一次留下的是书库根上一份没人认领的
/// 副本，而**根上的半成品没有 [`sweep_partials`] 兜底**（它只认 `.{名字}.part` 那种名字）。
///
/// `book_id` = `Some` 表示那一行已经站在库里了、需要一并撤掉；`None` 表示行还没落地，
/// 只需退文件。撤行本身失败只记日志由 `Store` 那侧说清，这里不让一次回滚再盖掉
/// **原始的失败理由**（与 [`abandon`] 同一条纪律）。
pub(crate) fn unwind_landing(
    store: &Store,
    heal: Option<&BookRow>,
    book_id: Option<i64>,
    dest: &Path,
) {
    match (heal, book_id) {
        (None, Some(id)) => {
            let _ = store.remove_book(id);
        }
        (Some(row), Some(id)) => {
            let _ = store.retarget_book_path(id, Path::new(&row.path));
        }
        _ => {}
    }
    abandon(dest);
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
fn abandon(path: &Path) {
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
    let row = store.book(book_id)?.ok_or(Error::NoSuchBook(book_id))?;
    let path = PathBuf::from(&row.path);
    // 内容身份顺手补一次（懒补的触点之一）。**算不出不拦住重扫**：这一本照样能打开，
    // 只是还没有身份，而读不动文件是自愈档（`plans/基础设施.md` §3.1），记一句就够。
    // 那一列**写**不进去同样不拦住 —— 判据与口径都在 [`index`] 里，与导入那一路同一份。
    let hash = match identity::sha256_of_path(&path) {
        Ok(hash) => Some(hash),
        Err(err) => {
            tracing::warn!("import: 第 {book_id} 本算不出内容哈希，交给补齐：{err}");
            None
        }
    };
    // 走的是导入那同一个 [`index`]：解析 → 封面 → 落库 → 身份。差别只有三件 ——
    // 行已经在（`Rescan`，所以不换文件名、不新增行、失败也不许碰那份副本）、
    // 预算用整机那一档（这一本是唯一在跑的）、封面顺手补一次（老行没有 `covers/`
    // 那个文件：字节当年在库里，而 `books.cover` 那一列在 v16 已经删掉，重扫是从副本
    // 重新抽一遍，正是补上的时机；已存在就不写，所以补过的书白跑一次 `exists`）。
    index(
        store,
        Indexing {
            name: &row.title,
            path: &path,
            hash: hash.as_ref(),
            budget: ParseBudget::Auto,
            landing: Landing::Rescan { book_id },
        },
    )?;
    tracing::info!("import: 重扫第 {book_id} 本《{}》", row.title);
    Ok(())
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
