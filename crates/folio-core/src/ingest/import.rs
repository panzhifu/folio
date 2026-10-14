//! 导入管线：把「一本书的出处」抽象成 [`Source`]，一条管线通吃所有来源。
//!
//! 抽象边界只圈一件事：**候选 → 书库里那一份按内容命名的副本**。写暂存、算哈希、
//! 判格式、判重、落地、解析、落库都是管线自己的事，在后台线程上逐本完成 ——
//! [`crate::store::Store`] 现在是 `Send + Sync` 的（连接锁在内部 `Mutex` 上），落库
//! 不必再回界面线程。锁纪律见 `store` 的模块文档：**stage / hash / parse 绝不持锁**，
//! 只有 register 那一小段持，后台导一本书时界面最多等几十毫秒。
//!
//! 顺序照 `plans/导入管线.md` 第一节那七步，**ingest / index 的分界在判完重之后**：
//!
//! ```text
//! 1 来源分派      candidates() —— 列目录 / PROPFIND 都在这一步，是整批的慢
//! 2 写暂存        stage() → <书库>/.staging/.<名>.<序号>.part
//! 3 算哈希 + 判定   对那份本地 .part：sha256 → 三层格式判定（否掉排在落地之前）
//! 4 判重          按完整哈希；命中要先 stat 旧副本，不在就走自愈
//! ────────────────── ingest 与 index 的分界 ──────────────────
//! 5 定名落地      先在暂存区换成「建议名 + 判定出的扩展名」去解析，成功后 rename
//!                 → <书库>/<哈希前缀>.<格式>（同分区、原子，不进根的没有半成品）
//! 6 解析          document::open → 章节表 / 资源表 / 目录 / 封面（实际排在 5 后半之前）
//! 7 落库          add_book + set_content_hash + save_document，库里从此有这一行
//! ```
//!
//! 前四步与格式无关、失败就删该删的那一份；后三步完全依赖格式。**两步之间唯一的磁盘
//! 动作是一次 rename**，所以崩溃时书库里只可能出现没人认领的 `.part`（由
//! [`sweep_partials`] 清），不会出现「库里有行而文件不在」之外的那种半成品。
//!
//! 七种来源的差别只剩 `candidates` 与 `stage` 两个方法，管线只有 [`import_from`] 一写。
//! 共同的那半件事（写进暂存）现在**契约上**仍归来源，但只能落在 `.staging` 里：
//!
//! | 来源 | candidates | stage |
//! | --- | --- | --- |
//! | 选择文件 `Picked` | 选择器交回的路径 | 分块拷进 `.staging`，报字节数 |
//! | 批量路径 `Paths` | 调用方给的一批（清单勾选 / 传书落盘） | 同上 |
//! | 整机扫描 `Scan` | 走目录只收认识的扩展名 | 同上 |
//! | 文件夹监听 `Watch` | inotify 事件攒出的新文件 | 同上 |
//! | Wi-Fi 传书 `Transfer` | 传书服务器收到的每份上传 | 同上 |
//! | 链接下载 `Url` | 一条 URL | 流式 GET 写进 `.staging` |
//! | WebDAV `WebDav` | PROPFIND ∩ 需要下载的条目 | DAV 下载写进 `.staging` |
//!
//! **与文档的偏离一处**（第七节差额表第 4 行剩下的那半）：文档要求来源只交
//! `stream: Box<dyn Read>`、写盘完全归管线。那一步要先给 `remote` 加一个返回流的方法
//! （网络那半边现在是 `Sink` 驱动的），留到下一轮。现在这个形状已经买到文档要的全部
//! **效果**：判定排在落地之前、身份算在自己写下的字节上、副本名由内容决定。
//!
//! 监听、传书、WebDAV 同步自己都不是管线的一部分 —— 它们是往队列塞活的**驱动者**
//! （见 `crate::ingest::watch`），把 [`Source`] 交给 [`submit_import`] 就行。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex};

use crate::content::document;
use crate::content::format::Format;
use crate::content::format::ParseBudget;
use crate::content::format::epub::MAX_PARSE_WORKERS;
use crate::content::format::sniff;
use crate::infra::error::ImportReject;
use crate::infra::error::{Error, IoResultExt as _, Result};
use crate::infra::paths;
use crate::infra::task::{Priority, StageCtx, TaskError, TaskEvent, TaskHandle, TaskManager};
use crate::ingest::identity;
use crate::net::remote::{Method, Request, Sink, Transport};
use crate::store::BookRow;
use crate::store::Store;

/// 这次导入属于哪一类来源。日志与「这批是谁交的」用，不参与任何分支逻辑。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportKind {
    /// 读者在系统选择器里亲手挑的。
    Picked,
    /// 调用方直接给的一批本地路径（扫描清单勾选、传书落盘、监听攒批）。
    Paths,
    /// 整机扫描找出来的。
    Scan,
    /// 监听文件夹时事件攒出来的。
    Watch,
    /// 传书服务器收到的。
    Transfer,
    /// 一条网络直链。
    Url,
    /// WebDAV 同步的下载半边。
    WebDav,
}

/// 一个候选的出处。本地是一份路径；远端是「能下载到它的 URL」加上
/// 「它在自己那一层里的路径」（同步状态表按后者记账）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Local(PathBuf),
    Remote {
        url: String,
        path: String,
        size: Option<u64>,
    },
}

/// 一个候选 = 一本书的出处，加上「它叫什么」。
///
/// `name` 只是**建议名**：它进判定层当扩展名的线索（`sniff::identify` 的第一层），
/// 进日志，进界面那句「《书名》已收进」。副本最终叫什么由内容哈希决定，不由它决定 ——
/// 这正是身份从路径换成哈希的全部意义（§六）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub origin: Origin,
    pub name: String,
    pub size: Option<u64>,
}

/// 一个来源。一个实现管一种出处；实现整个住在后台作业里，所以必须 `Send + Sync`。
///
/// 来源的职责只有一句话：**把字节送到**（§二 不变量三）。它不算哈希、不判格式、
/// 不碰数据库 —— 那三件事对所有来源完全一样，写进任何一个来源里就必然漂移。
pub trait Source: Send + Sync + 'static {
    /// 这一类来源是哪个。日志与「这批是谁交的」用，不参与任何分支逻辑。
    fn kind(&self) -> ImportKind;

    /// 交出这一批候选。列目录（扫描）、PROPFIND（远端）都发生在这一步，
    /// 所以它拿得到两级读数与取消 —— 这一慢是**整批的**慢。
    ///
    /// `dest_dir` 是书库目录，只用来让来源能算出自己的相对路径与勾选清单，
    /// **不许往里写文件**（写盘归下一步，且只许写进暂存区）。
    fn candidates(&self, dest_dir: &Path, ctx: &StageCtx) -> Result<Vec<Candidate>>;

    /// 把一个候选写进 `staging_dir`（`<书库>/.staging`），交回那个临时文件的路径。
    ///
    /// 三条规矩，都是为了让管线后面几步只面对一个可 seek、可重读的本地文件：
    ///
    /// - 只许落在管线给的这个目录里。写到别处管线会拒绝收下（也不删它 —— 那可能是
    ///   读者自己的文件）。
    /// - 临时名自己定，但要带 `.part` 尾巴并且以点开头：进程被杀时留下的就是这种名字，
    ///   启动时 [`sweep_partials`] 按这一条认出来清掉，而 `scan` 那一条「目录名以点开
    ///   头就跳过」顺带保证它们永远不会被当成一本书。
    /// - 半途而废要自己把写了一半的东西清掉（与 [`crate::net::remote::Sink`] 的契约同一句话）：
    ///   临时文件要么交给管线，要么消失，不留第三种状态。
    ///
    /// 字节级读数从这里报（拷贝与下载都是）。
    fn stage(&self, candidate: &Candidate, staging_dir: &Path, ctx: &StageCtx) -> Result<PathBuf>;
}

/// 落库成功的一条结果。界面拿 `book_id` 清封面缓存、重查架面；
/// `dest` 与 `title` 只为日志与测试 —— 章节表已经落库，正文按位置回容器取
/// （见 `plans/text-out-of-db.md`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registered {
    pub book_id: i64,
    pub dest: PathBuf,
    pub title: String,
}

/// 一个候选的成功结局，两样之一（§三 那个 `Imported`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Imported {
    /// 新落地的一本。`dest` 是它现在的副本位置（那个名字由内容哈希定），`title` 来自解析产物。
    Added(Registered),
    /// 内容哈希已经有那一行了：这次**没有**产生新副本、**没有**新增记录，来源挂到那一行上。
    /// 报表里它与 `Added` 分开算 —— 「成功 4997 / 已存在 3」是两栏，不是一栏
    /// （§九 第一条门要的就是这个区别）。
    Duplicate { book_id: i64 },
}

/// 一个候选的结果。`Err` = 这一本坏了，不拖累整批。
#[derive(Debug)]
pub struct ImportOutcome {
    pub candidate: Candidate,
    pub result: Result<Imported>,
}

/// 一批导入的分类计数。界面上那句「成功 N / 已存在 M / 拒绝 K（按类别）」读它，
/// 而不是逐条 toast（`plans/基础设施.md` §3.2：批量作业的错误要能聚合报表）。
///
/// 类别取自 [`crate::infra::error::Error::import_reject`]，是类型而不是文案 —— 改中文措辞
/// 不会把报表的分组改散。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub added: usize,
    pub duplicate: usize,
    pub unsupported: usize,
    pub encrypted: usize,
    pub corrupt: usize,
    pub unreadable: usize,
}

impl ImportReport {
    /// 把一批结果摊成计数。
    pub fn tally(outcomes: &[ImportOutcome]) -> Self {
        let mut report = Self::default();
        for outcome in outcomes {
            match &outcome.result {
                Ok(Imported::Added(_)) => report.added += 1,
                Ok(Imported::Duplicate { .. }) => report.duplicate += 1,
                Err(err) => match err.import_reject() {
                    Some(ImportReject::UnsupportedFormat) => report.unsupported += 1,
                    Some(ImportReject::Encrypted) => report.encrypted += 1,
                    Some(ImportReject::CorruptContainer) => report.corrupt += 1,
                    // 认不出类别的也算「读不动」：它确实没能变成一行记录，而报表不许
                    // 悄悄少算一本 —— 少算的那本读者只会看见「少了几本」。
                    Some(ImportReject::Unreadable) | None => report.unreadable += 1,
                },
            }
        }
        report
    }

    /// 被拒的总数，报表那句「拒绝 K」。
    pub fn rejected(&self) -> usize {
        self.unsupported + self.encrypted + self.corrupt + self.unreadable
    }

    /// 一句给人看的话（进日志；屏幕上那句要按 locale 组，core 的字符串只进日志）。
    pub fn summary(&self) -> String {
        format!(
            "成功 {} / 已存在 {} / 拒绝 {}（不支持 {}、加密 {}、坏包 {}、读不动 {}）",
            self.added,
            self.duplicate,
            self.rejected(),
            self.unsupported,
            self.encrypted,
            self.corrupt,
            self.unreadable
        )
    }
}

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
    dest_dir: &Path,
    store: &Store,
    ctx: &StageCtx,
) -> Vec<ImportOutcome> {
    let kind = source.kind();
    let candidates = match source.candidates(dest_dir, ctx) {
        Ok(candidates) => candidates,
        Err(err) => {
            tracing::error!("import: {kind:?} 交不出候选：{err:#}");
            return Vec::new();
        }
    };
    let staging = paths::staging_dir(dest_dir);
    if let Err(err) = paths::prepare_dir(&staging) {
        tracing::error!("import: {kind:?} 暂存区 {staging:?} 建不起来：{err:#}");
        return Vec::new();
    }
    let total = candidates.len() as u64;
    tracing::info!("import: {kind:?} 一批 {total} 本 → {dest_dir:?}");
    let outcomes = import_batch(
        source,
        dest_dir,
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
struct Budget {
    /// 一批里同时在导几本。
    books: usize,
    /// 每一本内部**最多**起几路解析 worker。是上限不是配额：条目少到不值得起线程时
    /// 照样走顺序，判据留在 provider 那一层（只有它知道有几个条目）。
    per_book: usize,
}

/// 一批最多同时导几本。再往上量不到东西：8 本合成书（共 4.7 MB）那一批量到的是
/// 1 本 ≈ 55 ms、4 本 ≈ 25 ms、5 本 ≈ 23 ms —— 第 5 本只换来 2 ms，却多一份解析内存，
/// 所以封顶在 4。
const MAX_IMPORT_BOOKS: usize = 4;
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
fn split_budget(cores: usize) -> Budget {
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
    dest_dir: &'a Path,
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
        let total = self.candidates.len() as u64;
        loop {
            if self.ctx.cancel().triggered() {
                return;
            }
            let at = self.next.fetch_add(1, Ordering::Relaxed);
            if at >= self.candidates.len() {
                return;
            }
            let candidate = &self.candidates[at];
            let result = import_candidate(
                self.source,
                candidate,
                self.dest_dir,
                self.staging,
                self.store,
                self.ctx,
                self.per_book,
            );
            // 读数按**结清**的数报，不按领号报：并行时领号在前、结清在后，
            // 用领号数会画出一条会后退的进度。
            let done = self.done.fetch_add(1, Ordering::Relaxed) as u64 + 1;
            self.ctx.item(done, total);
            self.landed.lock().unwrap_or_else(|e| e.into_inner()).push((
                at,
                ImportOutcome {
                    candidate: candidate.clone(),
                    result,
                },
            ));
        }
    }

    /// 收干结果并按候选原序交回。`workers > 1` 时顺手把**没有结果的那些**在调用线程上
    /// 补跑一遍：崩掉的 worker 手上那一本不许从报表里静默消失。
    fn finish(self) -> Vec<ImportOutcome> {
        let total = self.candidates.len();
        let mut rows = self.landed.into_inner().unwrap_or_else(|e| e.into_inner());
        rows.sort_by_key(|(at, _)| *at);
        if rows.len() < total && !self.ctx.cancel().triggered() {
            // 缺下标的就是某条 worker 手上正拿着的那本（它崩在了记进表之前）。
            let present: HashSet<usize> = rows.iter().map(|(at, _)| *at).collect();
            for at in 0..total {
                // 与 [`Batch::run`] 同一个检查点：取消之后不再补新的。
                if self.ctx.cancel().triggered() {
                    break;
                }
                if present.contains(&at) {
                    continue;
                }
                let candidate = &self.candidates[at];
                let result = import_candidate(
                    self.source,
                    candidate,
                    self.dest_dir,
                    self.staging,
                    self.store,
                    self.ctx,
                    self.per_book,
                );
                let done = self.done.fetch_add(1, Ordering::Relaxed) as u64 + 1;
                self.ctx.item(done, total as u64);
                rows.push((
                    at,
                    ImportOutcome {
                        candidate: candidate.clone(),
                        result,
                    },
                ));
            }
            // 补跑的那几本下标可能比表里已有的小，所以要再排一次才谈得上「候选原序」。
            rows.sort_by_key(|(at, _)| *at);
        }
        rows.into_iter().map(|(_, outcome)| outcome).collect()
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
fn import_batch(
    source: &dyn Source,
    dest_dir: &Path,
    store: &Store,
    ctx: &StageCtx,
    staging: &Path,
    candidates: &[Candidate],
    budget: Budget,
) -> Vec<ImportOutcome> {
    let batch = Batch {
        source,
        dest_dir,
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
/// 用它而不是在 [`import_candidate`] 里手写退号，理由是那函数有十来条退出路径：漏掉任何一条
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

/// 一本书的完整旅程，就是 §一 那张图。
///
/// 失败路径上的三个去处要说清，因为它们是「库里有这一行就等于能打开」那句话的全部内容：
///
/// - 判定或哈希这一步失败 → 删掉**暂存区里**那份 `.part`，书库目录里什么都没发生过。
/// - 落地之后失败（解析、落库）→ 删掉刚 rename 出去的那份副本；如果这一支是自愈
///   （改已有那一行的位置），位置也退回原来那个不存在的文件，等于回到动手之前。
/// - 成功 → 库里一行、书库里一份按内容命名的副本，两者同时存在。
fn import_candidate(
    source: &dyn Source,
    candidate: &Candidate,
    dest_dir: &Path,
    staging: &Path,
    store: &Store,
    ctx: &StageCtx,
    per_book: usize,
) -> Result<Imported> {
    let name = candidate.name.as_str();

    // 步 2：来源把字节写进暂存区，交回那份 `.part`。
    let staged = source.stage(candidate, staging, ctx)?;
    // 契约守一句：写出了暂存区就不收下。也**不删**它 —— 那可能是读者自己的文件，
    // 我们只是没收下这本书而已。
    if staged.parent() != Some(staging) {
        tracing::error!("import: 《{name}》被写到了暂存区之外（{staged:?}），拒绝收下");
        return Err(Error::Io {
            path: staged,
            source: "stage 写出了暂存区之外".to_string().into(),
        });
    }

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
    // 查之前先把这一格的号占到，握到函数结束（见 [`Claims`]）：并行导同一份内容的两本
    // 就是靠它变回「一本先导、一本等完再看结果」。
    let hash_hex = hash.to_hex();
    let _claim = CLAIMS.hold(&hash_hex);
    let heal = match store.book_by_hash(&hash_hex)? {
        Some(row) if Path::new(&row.path).exists() => {
            abandon(&staged);
            tracing::info!("import: 《{name}》与第 {} 本同内容，来源挂到那一行", row.id);
            return Ok(Imported::Duplicate { book_id: row.id });
        }
        // 命中但旧副本已经不在了 → 继续落地，并把那一行的位置改指过来。这是**自愈**：
        // 把一个已经坏掉的库修回来，而不是又拒读者一次。
        Some(row) => {
            tracing::warn!(
                "import: 第 {} 本记的副本 {} 已经不在了，用这份同内容的文件接回去",
                row.id,
                row.path
            );
            Some(row)
        }
        None => None,
    };

    // 步 5 的前半：先把那份字节改成一个**人看得懂的名字**再解析。
    //
    // 为什么这一步不能省：单文件格式的书名与第一章名回落成**文件主名**
    // （`format::markdown` 里那两处 `unwrap_or_else(|| stem(path))`），而最终副本按内容
    // 命名 —— 直接拿哈希名去解析，架子上每一本 TXT 都显示成 `tuqddpm7nrheuceu`，
    // 目录里第一章也叫这个。这条是实测翻出来的（哈希名一接上，标题就跟着变成哈希）。
    // 顺带把失败回滚变干净了：解析没过的东西**根本没进过书库根**。
    let readable = staging.join(format!(
        "{}.{}",
        readable_stem(name, &hash),
        format.copy_extension()
    ));
    if let Err(err) = std::fs::rename(&staged, &readable).io_path(&readable) {
        abandon(&staged);
        return Err(err);
    }

    // 步 6：解析。失败只退掉暂存区里那一份，书库根从来没被碰过。
    // 这一本内部起几路是外面分好的（`Budget::per_book`）：一本自己按整机核数猜的话，
    // 一批同时飞 M 本就是 M 份整机预算。
    let document = match document::open_with(&readable, ParseBudget::AtMost(per_book)) {
        Ok(document) => document,
        Err(err) => {
            abandon(&readable);
            let why = format!("{err:#}");
            return Err(Error::Rejected {
                name: name.to_string(),
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
    //    路径太长）就只是「这次没有封面文件」，库里那两列留空，读的时候退回
    //    `books.cover` 那个 BLOB —— 老库与新失败走的是同一条兜底。
    //
    // 书库根从**这本副本自己的路径**推（它就是 `dest_dir`，而副本躺在书库根上），
    // 所以库层不需要持有书库位置 —— 那是读者确认过的目录，core 不知道也不该知道。
    let mut document = document;
    land_cover(dest_dir, document.as_mut());

    // 步 5 的后半：定名落地。`.staging` 就在书库底下 → 同分区，rename 原子且 O(1)。
    // 扩展名来自**判定**（不是来源那个名字），而且要问注册表要它认得的那一个：
    // 副本的再一次识别走的就是那张表（`Format::copy_extension`）。
    let dest = dest_dir.join(hash.file_name(format.copy_extension()));
    if let Err(err) = std::fs::rename(&readable, &dest).io_path(&dest) {
        abandon(&readable);
        return Err(err);
    }

    // 步 7：落库。自愈那一支**不新增行** —— 同一份内容有两行就是两套进度、两套批注，
    // 而读者要的只是原来那一本回来了。
    let title = document.meta().title.clone();
    let book_id = match &heal {
        None => store.add_book(&dest, document.meta())?,
        Some(row) => {
            store.retarget_book_path(row.id, &dest)?;
            row.id
        }
    };
    if let Err(err) = store.save_document(book_id, document.as_ref()) {
        if heal.is_none() {
            let _ = store.remove_book(book_id);
        } else if let Some(row) = &heal {
            let _ = store.retarget_book_path(row.id, Path::new(&row.path));
        }
        abandon(&dest);
        return Err(err);
    }
    // 身份列**最后**写：前面任何一步失败都不会留下一条「有身份却没内容」的行。
    // 这一句自己失败不算这本没进来 —— 行与章节都在、书能打开，只是哈希还空着，
    // 而空着是懒补本来就允许的状态（补齐作业会重算它）。
    if let Err(err) = store.set_content_hash(book_id, &hash_hex, identity::HASH_ALGO) {
        tracing::warn!("import: 第 {book_id} 本的内容哈希没写进去，交给补齐：{err:#}");
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
/// - **失败只记日志，不回 Err**：封面是装饰。写不成时 `cover_ref` 留 `None`，
///   读那一侧退回 `books.cover` 那个 BLOB，读者照旧看得见封面。
///
/// 只处理可重排那一支：定版式（CBZ）的封面是它第一页的缩略图，走 `save_fixed`，
/// 与这里这条「容器里抽出来的字节」不是同一件事。
fn land_cover(root: &Path, doc: &mut dyn crate::content::document::Document) {
    let crate::content::document::ContentRef::Reflow(_) = doc.content() else {
        return;
    };
    // 字节与 MIME 都要：前者定哈希，后者定扩展名。
    let Some(bytes) = doc.cover().map(<[u8]>::to_vec) else {
        return;
    };
    if bytes.is_empty() {
        return;
    }
    let mime = cover_mime(&bytes, doc);
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

/// 封面的 MIME：manifest 声明优先，没有就看字节头。
///
/// 与 `ui/shared/images.rs` 认图片格式同一条口径（真书里 `image/jpg`、
/// `IMAGE/PNG` 这类写错比字节头写错常见得多，所以**字节头优先**在那里；
/// 而封面这一格只影响文件扩展名与 `image/*` 这种粗判，两者都试一遍即可）。
fn cover_mime(bytes: &[u8], _doc: &dyn crate::content::document::Document) -> String {
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
        .filter(|c| {
            !c.is_control() && !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
        .take(60)
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        return hash.file_stem();
    }
    cleaned.to_string()
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
/// 它做完 rename 才回写 `path`（见 [`backfill_copies`]）。
pub fn reindex(store: &Store, book_id: i64) -> Result<()> {
    let row = store.book(book_id)?.ok_or(Error::NoSuchBook(book_id))?;
    let path = PathBuf::from(&row.path);
    let mut document = document::open(&path)?;
    // 封面也补一次（与导入那条路同一步）：v15 之前入库的行只有 BLOB、没有
    // `covers/` 那个文件，而重扫正是把它们补上的时机。已存在就不写，
    // 所以对已经补过的书这一步是白跑一次 `exists`。
    land_cover(&crate::store::book_root(&path), document.as_mut());
    // 顺手刷新 `size` / `mtime`：判定「文件换没换」靠的就是这两个数。
    store.add_book(&path, document.meta())?;
    store.save_document(book_id, document.as_ref())?;
    match identity::sha256_of_path(&path) {
        Ok(hash) => store.set_content_hash(book_id, &hash.to_hex(), identity::HASH_ALGO)?,
        // 算不出身份不拦住重扫：这一本照样能打开，只是还没有身份。读不动文件是
        // 自愈档（`plans/基础设施.md` §3.1），记一句就够，别报成「重扫失败」。
        Err(err) => tracing::warn!("import: 第 {book_id} 本算不出内容哈希，交给补齐：{err}"),
    }
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

/// 一趟补齐：给「还没有内容身份」的行算哈希，并把旧名字的副本改成哈希名。
///
/// 这是懒补的另一半（`plans/导入管线.md` §六 与 §一 步 5）。为什么不塞进迁移里：
/// 几百本的一次性顺序读不该压在开库那一下（`plans/基础设施.md` §1.2 第 1 条），
/// 可它又必须真的跑完 —— 不然「书库目录可以整目录 GC」「同名即同书」这些结论一直不成立。
/// 所以它是 `Cleanup` 档的活（界面空闲才跑的那一档，见 [`crate::infra::task::Priority`]），
/// 取消检查点落在书与书之间：**已经改过名的那些是真的**，剩下的一趟再来。
///
/// 四种情形分开处置，各自计数：
///
/// - **文件不在**（被删、没权限）→ 跳过。**不删行**：那条记录指向不存在的文件是
///   [`Store::stale_reason`] 早就认得出的状态，而这里没有资格替读者决定丢书。
/// - **副本躺在书库之外**（老的一就地登记那批，或读者中途改过书库位置）→ 只写身份列，
///   不动文件：那个目录不是应用自己的，搬它是越界（判据与界面的 `owned_copy` 同一条）。
/// - **目标名已经被占**（同哈希的另一行早就落过地）→ 不改名，把这一行改指那个在的位置。
///   两行共用一个文件是允许的状态，删书时由 [`Store::rows_sharing_path`] 兜住。
/// - 其余 → rename 成 `<哈希前缀>.<格式>`，回写位置与身份。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackfillReport {
    /// 写进身份列的行数（含只写身份没搬文件的）。
    pub hashed: usize,
    /// 真的改了名的行数。
    pub renamed: usize,
    /// 因为目标名已在而改指别处的行数。
    pub retargeted: usize,
    /// 读不到文件 / 算不出哈希 / rename 失败而跳过的行数。
    pub skipped: usize,
}

/// 跑一趟补齐：给 `content_hash IS NULL` 的行认身份，并把书库里的旧名字副本改成哈希名。
///
/// 情形怎么分、为什么单独一趟，写在 [`Backfill_copies 的类型文档`](`BackfillReport`) 里。
/// 返回值只用于日志与测试断言 —— 这一趟没有「失败」一说：跳过的行留着 NULL，
/// 下一趟还会再来（自愈档，`plans/基础设施.md` §3.1）。
pub fn backfill_copies(store: &Store, dest_dir: &Path, ctx: &StageCtx) -> BackfillReport {
    let mut report = BackfillReport::default();
    let rows = match store.rows_missing_hash() {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!("import: 补齐问不到「哪些行还没身份」：{err:#}");
            return report;
        }
    };
    let total = rows.len() as u64;
    for (done, row) in rows.iter().enumerate() {
        if ctx.cancel().triggered() {
            tracing::info!("import: 补齐被取消，已处理 {done} / {total} 行");
            break;
        }
        ctx.item(done as u64, total);
        match backfill_one(store, dest_dir, row) {
            Some((renamed, retargeted)) => {
                report.hashed += 1;
                if renamed {
                    report.renamed += 1;
                }
                if retargeted {
                    report.retargeted += 1;
                }
            }
            None => report.skipped += 1,
        }
    }
    ctx.item(total, total);
    if total > 0 {
        tracing::info!(
            "import: 补齐一趟 —— 认身份 {} 行（改名 {}、改指 {}），跳过 {}",
            report.hashed,
            report.renamed,
            report.retargeted,
            report.skipped
        );
    }
    report
}

/// 一行。`Some((改过名没有, 改指过没有))` = 身份写进去了；`None` = 这一行跳过。
fn backfill_one(store: &Store, dest_dir: &Path, row: &BookRow) -> Option<(bool, bool)> {
    let path = Path::new(&row.path);
    let hash = match identity::sha256_of_path(path) {
        Ok(hash) => hash,
        Err(err) => {
            tracing::debug!("import: 第 {} 本读不出内容（{path:?}）：{err}", row.id);
            return None;
        }
    };
    if let Err(err) = store.set_content_hash(row.id, &hash.to_hex(), identity::HASH_ALGO) {
        tracing::warn!("import: 第 {} 本的身份没写进去：{err:#}", row.id);
        return None;
    }
    // 书库之外的副本只写身份：搬它是越界。
    if !path.starts_with(dest_dir) {
        return Some((false, false));
    }
    let dest = dest_dir.join(hash.file_name(&extension_of(row, path)));
    if dest == path {
        return Some((false, false)); // 已经是哈希名了。
    }
    // 那个位置已经有行占着（`books.path` 上有 UNIQUE）：只认身份、不改名、不改指。
    // 两条记录共用一个文件在当下是**造不出来**的状态，要等「内容变了但认得出是同一本」
    // 那条更新路（§十一 已决第 2 条 + 待决第 3 条）落地才会出现；那时这一句改指才是正解，
    // 现在硬改只会撞在 UNIQUE 上。留这一支而不直接 rename，是为了不把另一行的正文盖掉。
    if dest.exists() {
        // 这一问自己失败（库锁不住、语句出错）按「有人占着」处理：不改名是安全的一侧。
        let occupied = match store.rows_sharing_path(&dest.to_string_lossy(), row.id) {
            Ok(n) => n > 0,
            Err(err) => {
                tracing::warn!(
                    "import: 第 {} 本问不出 {dest:?} 被谁占着，先不改名：{err:#}",
                    row.id
                );
                true
            }
        };
        if occupied {
            tracing::info!(
                "import: 第 {} 本的哈希名位置 {dest:?} 已被别的行占着，只认身份不改名",
                row.id
            );
            return Some((false, false));
        }
        // 文件在而没有任何行认领（上一次崩溃留下的孤儿）：这一行改指过去，白丢一份 IO。
        if let Err(err) = store.retarget_book_path(row.id, &dest) {
            tracing::warn!("import: 第 {} 本改指 {dest:?} 失败：{err:#}", row.id);
            return None;
        }
        return Some((false, true));
    }
    if let Err(err) = std::fs::rename(path, &dest).io_path(&dest) {
        tracing::warn!(
            "import: 第 {} 本的副本 {path:?} 没改成 {dest:?}：{err:#}",
            row.id
        );
        return None;
    }
    if let Err(err) = store.retarget_book_path(row.id, &dest) {
        // 名字已经改了而位置没写回：这条记录指向不存在的旧名。当场说清并把文件放回去，
        // 比留下一本「点不开」的书好。
        tracing::warn!(
            "import: 第 {} 本改了名却写不回位置：{err:#}，把副本放回 {path:?}",
            row.id
        );
        let _ = std::fs::rename(&dest, path);
        return None;
    }
    Some((true, false))
}

/// 补齐时给副本用的扩展名：库里那一行 `format` 说是哪个格式就用哪个（那是当初判定过的
/// 结论），认不出来就沿用现在这个文件的扩展名，什么都剩不下时留空（文件名就只有哈希）。
/// 返回 `String` 而不是 `&str`：两条来源的生命周期不同（一条是 `'static` 的格式名、
/// 一条借自路径），拼成一个拥有自己数据的串最省事，也最容易看清没有借错。
fn extension_of(row: &BookRow, path: &Path) -> String {
    match Format::parse(&row.format) {
        Some(format) => format.copy_extension().to_string(),
        None => path
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

/// 把一个来源包成一个后台作业交给 `manager`。返回的 [`TaskHandle`] 就是这批导入的
/// 全部接口：两级进度、取消、等完成。
pub fn submit_import(
    manager: &TaskManager,
    source: Box<dyn Source>,
    dest_dir: PathBuf,
    store: Arc<Store>,
) -> TaskHandle<Vec<ImportOutcome>> {
    // 导入归落库那一档：封顶 1 挡的是「**多批**同时在飞」（§1.1），一批之内同时导几本由
    // [`process_budget`] 定。磁盘那一侧的账要连着看：一批里同时有几本在拷贝 + 算哈希 + 落库，
    // 那个数就是并行度，而跨批仍然只有一批在走。
    manager.submit_with(Priority::Persist, move |ctx| {
        Ok(import_from(source.as_ref(), &dest_dir, &store, ctx))
    })
}

/// 把一条导入作业的事件在专用线程上收干，逐条交给 `on_event`。
///
/// 监听与传书这类「驱动者」提交完作业就回去了，没有界面可接：而事件通道是
/// `std::sync::mpsc` 的接收端，只会阻塞读它的那条线程，所以必须有人把事件流读干净 ——
/// 这条线程随作业结束（通道关闭）自然退出。`on_event` 里再怎么阻塞都在它自己的线程上。
///
/// 线程创建失败才 panic：起不来确实无法继续，与 [`crate::ingest::watch`] / [`crate::net::transfer`]
/// 两处同一条纪律（`plans/基础设施.md` §3.3）。
pub fn forward<F>(handle: TaskHandle<Vec<ImportOutcome>>, mut on_event: F)
where
    F: FnMut(TaskEvent<Vec<ImportOutcome>>) + Send + 'static,
{
    std::thread::Builder::new()
        .name("folio-import-forward".to_string())
        .spawn(move || {
            for event in handle.events.iter() {
                on_event(event);
            }
        })
        .expect("起导入事件转发线程");
}

/// 扫掉上一次留下的半成品。
///
/// `.{名字}.part` 这种临时名在正常路径上要么被 rename 成正式副本，要么被 `abort` /
/// [`abandon`] 删掉，只有进程被杀才会留在原地。清两处：暂存区 `<书库>/.staging` 是
/// 现在的去处，书库根也照扫 —— 改成这个形状之前，半成品就留在根上。
///
/// 返回清掉几个。目录读不了就返回 0：这是启动路径上的自愈动作，失败不该让程序起不来
/// （`plans/基础设施.md` §1.3 末条，级别按 §3.1 的「自愈」那一档走）。
/// 两个区的名字都以点开头，而 `scan` 那条「以点开头的目录跳过」顺带保证了半成品
/// 永远不会被当成一本书 —— 这一条不靠这里删得干净。
pub fn sweep_partials(dest_dir: &Path) -> usize {
    sweep_partials_in(dest_dir) + empty_dir(&paths::staging_dir(dest_dir))
}

/// 清一个目录里没人认领的 `.part`（**只删这一种名字**）。
///
/// 书库根上那一层放着正经副本，判据必须窄：`.{名字}.part` 是旧形状留下的半成品，
/// 别的都可能是读者自己的文件或者已经入库的一本。目录不存在（从没导过书）算 0 个
/// 而不是错误 —— 这是启动路径上的自愈动作。
fn sweep_partials_in(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let partial = name.starts_with('.') && name.ends_with(".part");
        if partial && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// 把暂存区整个清空，返回清掉几项。
///
/// 与上面那条判据不同，这里**不需要挑名字**：`.staging` 按定义就是中间态的去处
/// （`plans/导入管线.md` §一 步 2），里面任何一样东西都不可能是权威数据。而且现在
/// 管线在里面对副本改过一次名（`readable_stem` 那一步），留下的中间文件不带 `.part`
/// 尾巴 —— 按 `.part` 挑着删就会一次次漏掉。目录本身留着，下次导入还要用。
fn empty_dir(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        // 里面可能还套着目录（一次导入留一层），整棵子树都是垃圾。
        let gone = if path.is_dir() {
            std::fs::remove_dir_all(&path).is_ok()
        } else {
            std::fs::remove_file(&path).is_ok()
        };
        if gone {
            removed += 1;
        }
    }
    removed
}

/// 本地来源共用的 `stage`：把候选那份文件**分块拷进暂存区**，交回那份 `.part`。
///
/// 自己写循环而不用 `fs::copy`，为的是字节级读数（`ctx.bytes`）：一本几十 MB 的书拷多久
/// 界面要报得出来，而 `fs::copy` 中间什么都看不见。**循环里不看取消旗** ——
/// `plans/基础设施.md` §1.2 第 5 条要求检查点落在单元之间而不是函数调用中间：一次拷贝
/// 是一个单元，半途停下来的结果是「谁也没认领、但确实写了一半」的那个文件。
pub(crate) fn stage_local(
    candidate: &Candidate,
    staging_dir: &Path,
    ctx: &StageCtx,
) -> Result<PathBuf> {
    let Origin::Local(source_path) = &candidate.origin else {
        return Err(Error::Io {
            path: PathBuf::from(&candidate.name),
            source: "本地 stage 不该接到远端候选".to_string().into(),
        });
    };
    std::fs::create_dir_all(staging_dir).io_path(staging_dir)?;
    let dest = staging_dir.join(staged_name(&candidate.name));
    let total = candidate.size.unwrap_or(0);
    copy_into(source_path, &dest, &candidate.name, total, ctx)
}

/// 分块拷贝并报字节数。交回的是暂存区里那份 `.part` 的路径；改成哈希名是管线第 5 步的事。
fn copy_into(from: &Path, to: &Path, display: &str, total: u64, ctx: &StageCtx) -> Result<PathBuf> {
    use std::io::{Read as _, Write as _};
    let mut reader = std::fs::File::open(from).io_path(from)?;
    let mut file = std::fs::File::create(to).io_path(to)?;
    const CHUNK: usize = 64 * 1024;
    let mut buf = vec![0u8; CHUNK];
    let mut done = 0u64;
    loop {
        let read = reader.read(&mut buf).io_path(from)?;
        if read == 0 {
            break;
        }
        file.write_all(&buf[..read]).io_path(to)?;
        done += read as u64;
        // 总数拿不到就报 `None`：界面画不定进度，而不是编一个会后退的数。
        ctx.bytes(done, (total > 0).then_some(total), display);
    }
    Ok(to.to_path_buf())
}

/// 暂存区里的临时名：`.<建议名前 40 字>.<序号>.part`。
///
/// 带序号是因为同一份文件**可以从同一个来源同时来两次**（选择器里连点、监听攒批里同一
/// 路径出现两轮）。两个候选若写到同一个名字，后写的那一份会盖掉前一份的半成品，之后
/// 算出的哈希属于一份「混合过的」文件 —— 那是最坏的一种错，因为它看起来是成功的。
/// 建议名留在前面只为人看日志时认得出是哪一本；它对最终文件名没有任何影响。
fn staged_name(display_name: &str) -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let cleaned: String = display_name
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | ':' | '*'))
        .take(40)
        .collect();
    let cleaned = cleaned.trim();
    let stem = if cleaned.is_empty() { "book" } else { cleaned };
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!(".{stem}.{n}.part")
}

/// 一批**现成的本地路径** —— 七种来源里六种最终都落成它：选择器交回的算
/// [`ImportKind::Picked`]，其余（清单勾选、传书、监听）由调用方带着自己的 [`ImportKind`] 进来。
///
/// 旧形状里那份 `Policy`（立场）现在整条不需要了：副本叫什么由内容哈希定，所以没有
/// 「重名怎么办」这一问；在不在库里由哈希判重定，所以没有「要不要跳过」这一问。
/// 来源之间剩下的唯一差别只有「日志里这是哪一类」。
pub struct PathsSource {
    paths: Vec<PathBuf>,
    kind: ImportKind,
}

impl PathsSource {
    /// 选择器选的文件。
    pub fn picked(paths: Vec<PathBuf>) -> Self {
        Self {
            paths,
            kind: ImportKind::Picked,
        }
    }

    /// 调用方直接给的一批路径，`kind` 说清这是谁交的（扫描 / 监听 / 传书各报各自的名）。
    pub fn new(paths: Vec<PathBuf>, kind: ImportKind) -> Self {
        Self { paths, kind }
    }

    pub(crate) fn candidates_from(paths: &[PathBuf]) -> Vec<Candidate> {
        paths
            .iter()
            .filter_map(|path| {
                let name = path.file_name()?.to_string_lossy().into_owned();
                let size = std::fs::metadata(path).ok().map(|m| m.len());
                Some(Candidate {
                    origin: Origin::Local(path.clone()),
                    name,
                    size,
                })
            })
            .collect()
    }
}

impl Source for PathsSource {
    fn kind(&self) -> ImportKind {
        self.kind
    }

    fn candidates(&self, _dest_dir: &Path, _ctx: &StageCtx) -> Result<Vec<Candidate>> {
        Ok(Self::candidates_from(&self.paths))
    }

    fn stage(&self, candidate: &Candidate, staging_dir: &Path, ctx: &StageCtx) -> Result<PathBuf> {
        stage_local(candidate, staging_dir, ctx)
    }
}

/// 下载的落盘策略：先写**临时名**，下完（`finish`）改名成正式副本；任何一半途
/// 而废（`abort`）都把临时文件清掉 —— 临时文件要么变成正式文件，要么消失，
/// 不会留下第三种状态（与 [`Transport::download`] 的契约是同一句话）。
struct FileSink {
    dest: PathBuf,
    partial: PathBuf,
    file: Option<std::fs::File>,
}

impl FileSink {
    /// 在 `dest` 旁边开一个 `.{文件名}.part`。同目录保证 `rename` 是原子的。
    fn start(dest: PathBuf) -> Result<Self> {
        let mut partial = dest.clone();
        let name = dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        partial.set_file_name(format!(".{name}.part"));
        let file = std::fs::File::create(&partial).io_path(&partial)?;
        Ok(Self {
            dest,
            partial,
            file: Some(file),
        })
    }
}

impl Sink for FileSink {
    fn write(&mut self, chunk: &[u8]) -> std::io::Result<()> {
        use std::io::Write as _;
        self.file
            .as_mut()
            .expect("finish/abort 之后不该再写")
            .write_all(chunk)
    }

    fn finish(&mut self) -> std::io::Result<()> {
        let file = self.file.take();
        if let Some(file) = file {
            file.sync_all()?;
        }
        std::fs::rename(&self.partial, &self.dest)
    }

    fn abort(&mut self) {
        self.file.take(); // 关掉句柄，删除才有意义
        let _ = std::fs::remove_file(&self.partial);
    }
}

/// 从一条 URL 取「这本书该叫什么」：路径末段、百分号解码、换掉文件系统
/// 不认的字符。查询串与锚点不算名字的一部分（签名直链的末段通常还是书名）。
/// 什么都剩不下就给一个带时间戳的占位名 —— 解析失败时错误里至少有它。
fn url_name(url: &str) -> String {
    // 剥掉 scheme:// 与 authority（到第一个 / 为止），末段才是路径里的东西；
    // 查询串与锚点不算名字。剥不出路径（URL 就是个裸域名）→ 占位名。
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let after_authority = after_scheme
        .split_once('/')
        .map(|(_, rest)| rest)
        .unwrap_or("");
    let path = after_authority
        .split(['?', '#'])
        .next()
        .unwrap_or(after_authority);
    let last = path
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or_default();
    let decoded = percent_decode(last);
    let cleaned: String = decoded
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim().to_string();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        format!("download-{}", now_secs())
    } else {
        trimmed
    }
}

/// 最小百分号解码：`%XX` 解回字节，其余原样。解码失败（半个 `%`）原样保留 ——
/// 名字是给人看的，不是给文件系统解析的。
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() + 1 && i + 2 <= bytes.len() - 1 + 1 {
            let hex = bytes.get(i + 1..i + 3);
            if let Some(hex) = hex
                && let Ok(byte) = u8::from_str_radix(&String::from_utf8_lossy(hex), 16)
            {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 一条网络直链：粘贴一个 http(s) 地址，下载、落成副本、照常解析入库。
///
/// `stage` 是流式的：响应体按块交给 [`FileSink`]，取消在块与块之间生效 ——
/// 半截的下载留不下半个文件。名字取自 URL 末段（见 [`url_name`]）；
/// 签名直链（`?token=…`）不在查询串里找名字，所以末段通常还是书名。
pub struct UrlSource {
    url: String,
    transport: Arc<dyn Transport>,
}

impl UrlSource {
    pub fn new(url: String, transport: Arc<dyn Transport>) -> Self {
        Self { url, transport }
    }
}

impl Source for UrlSource {
    fn kind(&self) -> ImportKind {
        ImportKind::Url
    }

    fn candidates(&self, _dest_dir: &Path, ctx: &StageCtx) -> Result<Vec<Candidate>> {
        let name = url_name(&self.url);
        tracing::debug!("import: URL 候选 {name} ← {}", url_for_log(&self.url));
        ctx.message(format!("下载《{name}》"));
        Ok(vec![Candidate {
            origin: Origin::Remote {
                url: self.url.clone(),
                path: self.url.clone(),
                size: None,
            },
            name,
            size: None,
        }])
    }

    fn stage(&self, candidate: &Candidate, staging_dir: &Path, ctx: &StageCtx) -> Result<PathBuf> {
        let Origin::Remote { url, .. } = &candidate.origin else {
            return Err(Error::Io {
                path: PathBuf::from(&candidate.name),
                source: "URL 候选不该是本地出处".to_string().into(),
            });
        };
        std::fs::create_dir_all(staging_dir).io_path(staging_dir)?;
        // 下载也只有一个去处：暂存区。`FileSink` 在那里面写 `.{名}.part`、下完改名成
        // 暂存区里的这一份，再由管线第 5 步 rename 进根 —— 全程书库根看不到半成品。
        let dest = staging_dir.join(staged_name(&candidate.name));
        let mut sink = FileSink::start(dest.clone())?;
        let req = Request::new(Method::Get, url.clone());
        let total = self.transport.download(req, &mut sink, ctx.cancel())?;
        ctx.bytes(total, Some(total), &candidate.name);
        Ok(dest)
    }
}

/// 日志里的 URL：查询串里常有凭证（签名直链的 token），打进日志前掐掉。
fn url_for_log(url: &str) -> String {
    url.split(['?', '#']).next().unwrap_or(url).to_string()
}

#[cfg(test)]
mod tests;
