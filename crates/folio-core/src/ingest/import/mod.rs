//! 导入契约与作业胶水：来源捕获、格式解析、独占版本发布和同书更新。
//! 首次导入与更新共用 versioned；文件/哈希/解析在库锁外，身份与索引由 Store 单事务登记。
//! 来源绑定与内容 hash 分离；Legacy 清理必须保留所有登记版本的字节引用。

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::error::ImportReject;
use crate::error::Result;
use crate::infra::task::Priority;
use crate::infra::task::StageCtx;
use crate::infra::task::TaskEvent;
use crate::infra::task::TaskHandle;
use crate::infra::task::TaskManager;
use crate::store::Store;

mod capture;
mod indexing;
mod maintenance;
mod pipeline;
mod projections;
mod publication;
pub use projections::refresh_anchor_projections;
mod reclamation;
mod snapshot;
pub use reclamation::prune_revision_history;
mod sources;
mod versioned;
pub use versioned::reconcile_source_ids;
pub use versioned::{
    SourceUpdate, SourceUpdateState, prepare_legacy_revision, reconcile_sources_once,
    relink_source, update_from_path,
};
pub use versioned::{reconcile_source_ids_with_access, update_from_link};

pub use maintenance::BackfillReport;
pub use maintenance::CoverBackfillReport;
pub use maintenance::{COVER_BACKFILL_VERSION, backfill_copies, backfill_covers};
// 测试要能抹掉记号来验「下一版升级再跑一趟」；生产编译里这一行不存在。
pub use maintenance::sweep_orphan_copies;
pub use maintenance::sweep_partials;
#[cfg(test)]
use maintenance::{COVER_BACKFILL_CURSOR_KEY, COVER_BACKFILL_KEY, COVER_BACKFILL_LEDGER_KEY};
pub use pipeline::import_from;
pub use pipeline::reindex;
pub use pipeline::schedule_reindex;
pub use sources::CloudItem;
pub use sources::CloudSource;
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
    /// 网盘里勾选的条目（浏览 → 勾选 → 下载）。
    Cloud,
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
    /// 网盘里的一个条目。它**没有**可以单独重放的直链 —— 再取一次要凭据与协议
    /// 客户端（[`crate::net::cloud::Drive`]），所以不落 `url`：`drive` 是哪家的
    /// （日志与报表给人看），`path` 是网盘内路径。
    Cloud {
        drive: &'static str,
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

    /// 本地实现可在复制时计算哈希；默认保留既有来源的 stage 契约。
    fn stage_with_hash(
        &self,
        candidate: &Candidate,
        staging_dir: &Path,
        ctx: &StageCtx,
    ) -> Result<(PathBuf, Option<String>)> {
        self.stage(candidate, staging_dir, ctx)
            .map(|path| (path, None))
    }

    /// 可以持续访问的本地原件；临时选择器副本与传书文件不得登记为长期来源。
    fn persistent_local(&self, candidate: &Candidate) -> Option<PathBuf> {
        match (&candidate.origin, self.kind()) {
            (
                Origin::Local(path),
                ImportKind::Picked | ImportKind::Paths | ImportKind::Scan | ImportKind::Watch,
            ) => Some(path.clone()),
            _ => None,
        }
    }

    fn source_link(
        &self,
        candidate: &Candidate,
    ) -> Option<crate::ingest::external_source::SourceLink> {
        self.persistent_local(candidate)
            .map(|path| crate::ingest::external_source::SourceLink {
                kind: "local".into(),
                locator: path.to_string_lossy().into_owned(),
            })
    }
}

/// 落库成功的一条结果。界面拿 `book_id` 清封面缓存、重查架面；
/// `dest` 与 `title` 只为日志与测试 —— 章节表已经落库，正文按位置回容器取
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
///
/// `duration` 是跑完**它那一格**花了多久（见 `Batch::import_at`：一个包算一趟，包里那几条
/// 记同一个数）。交的是时长而不是「8.8s」那样一句话 —— 屏幕上怎么说由界面按 locale 组。
#[derive(Debug)]
pub struct ImportOutcome {
    pub candidate: Candidate,
    pub result: Result<Imported>,
    pub duration: Duration,
}

/// 一批导入的分类计数。界面上那句「成功 N / 已存在 M / 拒绝 K（按类别）」读它，
/// 而不是逐条 toast（批量作业的错误要能聚合报表）。
///
/// 类别取自 [`crate::error::Error::import_reject`]，是类型而不是文案 —— 改中文措辞
/// 不会把报表的分组改散。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub added: usize,
    pub duplicate: usize,
    pub unsupported: usize,
    pub encrypted: usize,
    /// 要读者手上那份口令。与 `encrypted` 分开只为一句话：**这一档可以再交一次**。
    /// 界面据它决定要不要问口令并把同一批重跑；`encrypted` 那一档永远没有这个按钮。
    pub needs_password: usize,
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
                    Some(ImportReject::NeedsPassword) => report.needs_password += 1,
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
        self.unsupported + self.encrypted + self.needs_password + self.corrupt + self.unreadable
    }

    /// 一句给日志看的话。屏幕上那句「几本没进来、为什么」不在 core 组：那是一张浮层卡，
    /// 标题说总数、逐类各占一行，各条按语言的复数规则现挑臂
    /// （`folio_app::ui::features::shelf::view::status`）。
    pub fn summary(&self) -> String {
        format!(
            "成功 {} / 已存在 {} / 拒绝 {}（要口令 {}、不支持 {}、加密 {}、坏包 {}、读不动 {}）",
            self.added,
            self.duplicate,
            self.rejected(),
            self.needs_password,
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
/// 两处同一条纪律。
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
mod cover_backfill_tests;
#[cfg(test)]
mod tests;
