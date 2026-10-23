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
//! 5 定名落地      先在暂存区里**属于这一本自己的那间小屋**换成「建议名 + 判定出的
//!                 扩展名」去解析，成功后 rename → <书库>/<哈希前缀>.<格式>
//!                 （同分区、原子，不进根的没有半成品；小屋防的是同名候选互相盖）
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

//! **这一区现在分四个文件**（`import/`）：
//!
//! - `pipeline` —— 管线本体：线程预算、批次引擎、判重占号、`ingest` / `index`
//!   那两步、暂存区的小屋与失败回滚，以及 `reindex` / `schedule_reindex`（它们走的正是
//!   同一个 `index`）。
//! - `sources` —— 七个来源里「把字节送到」那一半：本地分块拷贝、URL 下载落盘、
//!   临时名与洗名。
//! - `maintenance` —— 补齐（给还没身份的行认身份、把旧名字的副本改成哈希名）
//!   与启动时扫半成品这两趟维护。
//! - 这一份（`mod.rs`）—— `Source` 契约与结果的**类型**，加上 [`submit_import`] / [`forward`]
//!   这一对作业胶水。类型住在这里是因为七个来源之外（`scan` / `watch` / 界面）也都认它们。
//!
//! 分界只有一条：**这一趟会不会碰那份字节**。会碰的在 `pipeline` 与 `sources`，只动库的在
//! `maintenance`。

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use crate::infra::error::ImportReject;
use crate::infra::error::Result;
use crate::infra::task::Priority;
use crate::infra::task::StageCtx;
use crate::infra::task::TaskEvent;
use crate::infra::task::TaskHandle;
use crate::infra::task::TaskManager;
use crate::store::Store;

mod maintenance;
mod pipeline;
mod sources;

pub use maintenance::BackfillReport;
pub use maintenance::backfill_copies;
pub use maintenance::sweep_partials;
pub use pipeline::import_from;
pub use pipeline::reindex;
pub use pipeline::schedule_reindex;
pub use sources::PathsSource;
pub use sources::UrlSource;
// `scan` 与 `watch` 都靠这一句把本地候选落进暂存区。
pub(crate) use sources::stage_local;

/// 导入来源
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportKind {
    /// 系统选择器，单个文件
    Picked,
    /// 多个文件路径
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

/// 本地和远端
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
    /// `book_dir` 是书库目录，只用来让来源能算出自己的相对路径与勾选清单，
    /// **不许往里写文件**（写盘归下一步，且只许写进暂存区）。
    fn candidates(&self, book_dir: &Path, ctx: &StageCtx) -> Result<Vec<Candidate>>;

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

/// 把一个来源包成一个后台作业交给 `manager`。返回的 [`TaskHandle`] 就是这批导入的
/// 全部接口：两级进度、取消、等完成。
pub fn submit_import(
    manager: &TaskManager,
    source: Box<dyn Source>,
    book_dir: PathBuf,
    store: Arc<Store>,
) -> TaskHandle<Vec<ImportOutcome>> {
    // 导入归落库那一档：封顶 1 挡的是「**多批**同时在飞」（§1.1），一批之内同时导几本由
    // [`process_budget`] 定。磁盘那一侧的账要连着看：一批里同时有几本在拷贝 + 算哈希 + 落库，
    // 那个数就是并行度，而跨批仍然只有一批在走。
    manager.submit_with(Priority::Persist, move |ctx| {
        Ok(import_from(source.as_ref(), &book_dir, &store, ctx))
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

#[cfg(test)]
mod tests;
