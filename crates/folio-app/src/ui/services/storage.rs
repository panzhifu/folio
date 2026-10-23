//! 设置里「存储」那一屏的读数，与那几颗按钮的落点。
//!
//! 为什么这是一个**实体**而不是设置页里的一格：静态设置页每帧重建、拿不住 `Task`
//! （见 `features/settings/view/pages/library.rs` 那句 `detach()` 的注释），而这一屏要的是
//! 「扫盘跑在后台线程上，跑完把账交回这一屏并重画」。gpui 里能同时做到这两件事的形状只有
//! 一个被观察的实体：写入方 `update` 它，视图对它 `observe`。这与 `ConfigHolder` 装的是
//! 同一个东西 —— 差别只在配置人人都要，而这一份只有那一屏要，所以它的句柄是**惰性**建的
//! （[`entity`]），外壳装一次、点按钮的人拿到 `&mut App` 时也能自己建。
//!
//! 三条纪律：
//!
//! 1. **盘不在界面线程上走**。一次遍历是几百到几万个 `stat`，真机上（书库在 SD 卡 emulation
//!    那头的 Meizu 20）几十到几百毫秒。所以 [`Job`] 全部交给 [`resident::pool`]，桥的形状照
//!    `features/shelf/view/scan.rs`：`mpsc` 阻塞读的那一头丢进 `background_spawn`，
//!    `async_channel` 这头像 `cx.spawn` 那样 await。
//! 2. **每个动作之后都重扫一遍**，所以那几颗按钮交回来的还是同一份 [`Snapshot`]。屏幕上的
//!    数是**现在**的数，不是「本来该变成多少」的推算 —— 设计 guides 那条「反馈要贴合现实」。
//! 3. **一次只跑一条**。`busy` 在飞时 [`StorageState::start`] 直接回，所以「连点两下清掉」
//!    不会把同一批文件删两遍，也不会让两条遍历互相插队。
//!
//! 「删除」这一侧只有四档，各自的可再生性都写在 [`Job`] 上：缩略图与漫画解压是缓存（删了
//! 下次重生成／重解），`.part` 是导入没走完的半成品，轮转旧日志是上一轮的现场。**当前那份
//! 日志、库里的行、读者的书，这里一颗按钮都没有** —— 那三样要另做一轮带逐条确认的界面。

use std::path::PathBuf;

use folio_core::content::format::{comic_cache, thumb};
use folio_core::infra::logging::{log_file, purge_rotated};
use folio_core::infra::usage::{self, Area, Areas, Usage};
use folio_core::{Priority, Store, TaskEvent, sweep_partials};
use gpui_kit::{App, AppContext as _, Context, Entity, Global, SharedString, Task, WeakEntity};

use super::app_store;
use super::config::ConfigHolder;
use super::library::target_dir;
use super::resident;
use crate::ui::services::l10n;

/// 一次读数。跑在后台线程上，所以里面全是路径与整数。
#[derive(Clone, Debug)]
pub struct Snapshot {
    /// 按去处分的那本账（`folio_core::infra::usage`）。
    pub usage: Usage,
    /// 库里空闲页折成字节。书库没装上或查询失败时 `None`（那是「没去看」，不是「零」）。
    pub freeable: Option<u64>,
}

/// 这一屏能发起的作业。四个删除档 + 一次重扫 + 一次整库重写。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Job {
    /// 只重扫，不动任何东西。
    Rescan,
    /// 删掉整棵缩略图缓存。**可再生**：书架下次要哪一档就重生成哪一档。
    ClearThumbs,
    /// 删掉整棵漫画解压缓存。**可再生**：下次打开那本重解一遍（慢一点，不是错）。
    ClearComics,
    /// 删掉书库 `.staging` 里的 `.part` 半成品。导入没走完的东西本来就没进库。
    ClearPartials,
    /// 删掉轮转出来的旧日志。当前正在写的那一份**不在这一档里**（fd 还攥着它）。
    ClearLogs,
    /// `VACUUM`：整库重写，把没人用的页与碎片吐回文件。慢，且过程临时多占一份。
    Vacuum,
}

impl Job {
    /// 稳定的英文小名。元素 id 用它：界面文案有九门且会随语言变，而元素身份不能跟着变
    /// （换了语言就换 id，等于每帧换一套 hover 与焦点状态）。
    pub fn key(self) -> &'static str {
        match self {
            Job::Rescan => "rescan",
            Job::ClearThumbs => "thumbs",
            Job::ClearComics => "comics",
            Job::ClearPartials => "partials",
            Job::ClearLogs => "logs",
            Job::Vacuum => "vacuum",
        }
    }

    /// 这一颗按钮管的是哪一类的字节。`Rescan` 哪一类都不管。
    ///
    /// 界面用它做两件事：那一行的按钮只在**那一类真有东西**时才摆（没东西可清就不该有一颗
    /// 按下去什么也不变的钮），以及确认话里点名的就是那一类。
    pub fn area(self) -> Option<Area> {
        match self {
            Job::Rescan => None,
            Job::ClearThumbs => Some(Area::Thumbnails),
            Job::ClearComics => Some(Area::Comics),
            Job::ClearPartials => Some(Area::Staging),
            Job::ClearLogs => Some(Area::RotatedLogs),
            Job::Vacuum => Some(Area::Database),
        }
    }
}

/// 快照 + 这一屏的状态机。视图 `observe` 它，跑完作业由它 `notify`。
#[derive(Default)]
pub struct StorageState {
    /// `None` = 还没扫过（或扫失败过且从没成功过）。这一屏那时只有一句「还没统计」。
    snapshot: Option<Snapshot>,
    /// 正在跑的那一条作业。`None` = 空闲。
    busy: Option<Job>,
    /// 等读者确认的那一颗钮。同一时刻只可能有一张确认面板。
    pending: Option<Job>,
    /// 上一次失败的原因（技术话，不译）。界面上那句「统计没跑成」后面跟着它。
    reason: Option<SharedString>,
    /// 在飞那条线的事件泵。跟着实体一起掉，所以这一屏被换掉时不会留下一个无人认领的循环。
    _pump: Option<Task<()>>,
}

impl StorageState {
    /// 最近一次读数。还没扫过时 `None`。
    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.snapshot.as_ref()
    }

    /// 有没有作业在飞（界面上那颗钮要变成「正在统计」而不是可以再点一次）。
    pub fn busy(&self) -> bool {
        self.busy.is_some()
    }

    /// 在飞的那一条是哪一条。界面上只有**那一个位置**该显示进度。
    pub fn busy_job(&self) -> Option<Job> {
        self.busy
    }

    /// 现在摊开的那张确认面板属于哪一颗钮。`None` = 没有面板。
    pub fn pending(&self) -> Option<Job> {
        self.pending
    }

    /// 失败的技术原因；`None` = 没失败过。
    pub fn reason(&self) -> Option<SharedString> {
        self.reason.clone()
    }

    /// 测试用：直接把一份账摆上这一屏。
    ///
    /// 有了它，界面那一层的测试不必为了「有一笔已知大小的账」而去真跑一遍池子
    /// （那条桥由 [`tests::a_finished_job_lands_its_ledger_back_on_the_screen`] 单独钉）。
    #[cfg(test)]
    pub fn set_snapshot_for_test(&mut self, snapshot: Snapshot) {
        self.snapshot = Some(snapshot);
    }

    /// 测试用：读回夹具摆上去的那份账，好再改一格接着验。
    #[cfg(test)]
    pub fn snapshot_for_test(&self) -> Option<Snapshot> {
        self.snapshot.clone()
    }

    /// 点「清掉」那一类：先摊开确认面板，不动磁盘。
    pub fn ask(&mut self, job: Job, cx: &mut Context<Self>) {
        if self.busy.is_some() || job.area().is_none() {
            return;
        }
        self.pending = Some(job);
        cx.notify();
    }

    /// 确认面板上点「取消」。
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        if self.pending.take().is_some() {
            cx.notify();
        }
    }

    /// 确认面板上点那颗真删的钮：收起面板并起活。
    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        let Some(job) = self.pending.take() else {
            return;
        };
        self.start(job, cx);
    }

    /// 起一条作业：交进池子，把事件泵到这一屏。已经有作业在飞时这一句是空转。
    pub fn start(&mut self, job: Job, cx: &mut Context<Self>) {
        // 先拦再取配置：忙时这一句该是什么也不做，连「读一遍生效配置」都不该有。
        if self.busy.is_some() {
            return;
        }
        self.start_at(job, areas(cx), cx);
    }

    /// 同 [`start`](Self::start)，但**去处由调用方给**。
    ///
    /// 这一层不为测试开后门地绕开了两个进程级的旋钮：`FOLIO_DATA_DIR` 是环境变量（并发用例
    /// 会互相读到对方那份），`paths::set_app_dirs` 是 `OnceLock`（第一个赢家定死整进程）。
    /// 所以要验「真作业跑完 → 账落回这一屏」这条桥，只能把 [`Areas`] 整个交进来指到临时目录。
    fn start_at(&mut self, job: Job, areas: Areas, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let store = app_store::handle(cx);
        let handle = resident::pool().submit_with(Priority::Interactive, move |_ctx| {
            execute(job, &areas, store.as_deref())?;
            Ok(read(&areas, store.as_deref()))
        });
        self.busy = Some(job);
        // 与整机扫描同款的桥：`handle.events` 是一条会阻塞读者的 `mpsc`，转进能 await 的
        // `async_channel` 再交给 `cx.spawn`。
        let (forward, events) = async_channel::unbounded::<TaskEvent<Snapshot>>();
        cx.background_spawn(async move {
            for event in handle.events.iter() {
                if forward.send_blocking(event).is_err() {
                    break;
                }
            }
        })
        .detach();
        self._pump = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            while let Ok(event) = events.recv().await {
                if this.update(cx, |this, cx| this.finish(event, cx)).is_err() {
                    return;
                }
            }
        }));
        cx.notify();
    }

    /// 后端的一条事件 → 这一屏的一次变化。
    fn finish(&mut self, event: TaskEvent<Snapshot>, cx: &mut Context<Self>) {
        match event {
            TaskEvent::Started | TaskEvent::Progress(_) | TaskEvent::Bytes(_) => {}
            TaskEvent::Finished(Ok(snapshot)) => {
                self.snapshot = Some(snapshot);
                self.busy = None;
                self.reason = None;
                cx.notify();
            }
            TaskEvent::Finished(Err(err)) => {
                // 旧的账**留着**：清一次失败了，屏幕不该变成空白，那比一个过期的数更没用。
                self.busy = None;
                self.reason = Some(err.to_string().into());
                tracing::warn!("storage: 作业失败：{err:#}");
                cx.notify();
            }
            TaskEvent::Cancelled => {
                self.busy = None;
                cx.notify();
            }
        }
    }
}

/// 挂这一屏读数的 `Global`。
pub struct StorageHolder(pub Entity<StorageState>);

impl Global for StorageHolder {}

/// 读数实体的句柄；还没建过时 `None`。视图与每一格读数都按这一条走，所以「没装」那一档
/// 只有一句要处理的话，而不是每一处各 `expect` 一次。
pub fn handle(cx: &App) -> Option<Entity<StorageState>> {
    cx.try_global::<StorageHolder>()
        .map(|holder| holder.0.clone())
}

/// 取句柄，没有就先建一个。
///
/// 「建」需要 `&mut App`，而**按钮的回调给的正是 `&mut App`**，所以这一屏在没有外壳装过的
/// 情况下（测试、以及从阅读页压进来的那一屏早于外壳的那些路径）也能自己把句柄立起来。
/// 外壳那头仍要在建两屏设置视图**之前**调一次：观察者挂在构造上，先有句柄才观察得到。
pub fn entity(cx: &mut App) -> Entity<StorageState> {
    if let Some(entity) = handle(cx) {
        return entity;
    }
    let entity = cx.new(|_cx| StorageState::default());
    cx.set_global(StorageHolder(entity.clone()));
    entity
}

/// 交一条作业给后台。拿不到实体时先建一个 —— 这一句是那一屏唯一的起活入口。
pub fn request(cx: &mut App, job: Job) {
    let entity = entity(cx);
    entity.update(cx, |state, cx| state.start(job, cx));
}

/// 点「清掉」那一类：摊开确认面板。
pub fn ask(cx: &mut App, job: Job) {
    let entity = entity(cx);
    entity.update(cx, |state, cx| state.ask(job, cx));
}

/// 确认面板上那两个动作。
pub fn answer(cx: &mut App, accept: bool) {
    let entity = entity(cx);
    entity.update(cx, |state, cx| {
        if accept {
            state.confirm(cx);
        } else {
            state.dismiss(cx);
        }
    });
}

/// 现在该量哪几处。两处依配置而定的入参在这里折：书库位置（读者确认过的那个）与日志落在
/// 哪儿（`logging.dir` 可以指到别处，关掉文件日志时根本没有那一份）。
pub fn areas(cx: &App) -> Areas {
    let config = ConfigHolder::snapshot(cx);
    Areas::from_app(target_dir(&config.library), log_file(&config.logging))
}

/// 某一类字节躺在哪儿。「打开所在文件夹」那一颗与路径那一格都问这一句，所以「哪一类在
/// 哪一处」这件事在仓库里只有这一份回答（与 [`areas`] 那份前缀规则同一个来源）。
///
/// 书库没确认过时那四类（书、封面、缩略图、半成品）没有去处，回 `None`：那一行界面上
/// 本来也不该摆一颗开目录的钮。
pub fn location(area: Area, cx: &App) -> Option<PathBuf> {
    use folio_core::infra::paths::{covers_dir, staging_dir, thumbs_dir};

    let areas = areas(cx);
    match area {
        Area::Books => areas.library.clone(),
        Area::Covers => areas.library.as_deref().map(covers_dir),
        Area::Thumbnails => areas.library.as_deref().map(thumbs_dir),
        Area::Staging => areas.library.as_deref().map(staging_dir),
        Area::Database => areas.db.parent().map(|dir| dir.to_path_buf()),
        Area::Logs | Area::RotatedLogs => Some(areas.logs.clone()),
        Area::Comics => Some(areas.comics.clone()),
        Area::Inbox => Some(areas.inbox.clone()),
        Area::Config => Some(areas.config.clone()),
        // 「其它」正是「我不知道这些字节属于哪儿」，所以它没有一个能打开的去处。
        Area::Other => None,
    }
}

/// 动作本身。跑在池子的工作线程上，所以只碰路径与那条库连接，不碰 `App`。
///
/// **两档不同**：清缓存那四档失败只记日志、照样把账重扫一遍 —— 删不掉的缓存下次还在，
/// 界面上那一格不会降，读者看得见「没删成」，这比弹一个错误框有用；而 `VACUUM` 那一档
/// 往上抛：它要么整库重写成功、要么一行都没动，重扫出来的库文件大小也一样，屏幕上没有
/// 任何线索会说「这一步失败了」。而它失败恰恰最常发生在**盘满了**的时候 —— 那正是这一屏
/// 要回答的问题，所以那句原因得交到界面上去（[`StorageState::reason`]）。
fn execute(
    job: Job,
    areas: &Areas,
    store: Option<&Store>,
) -> Result<(), folio_core::infra::task::TaskError> {
    match job {
        Job::Rescan => {}
        Job::ClearThumbs => {
            if let Some(root) = &areas.library {
                match thumb::purge(root) {
                    Ok(removed) => tracing::info!("storage: 缩略图缓存清掉 {removed} 张"),
                    Err(err) => tracing::warn!("storage: 缩略图缓存没清成：{err:#}"),
                }
            }
        }
        Job::ClearComics => {
            let removed = comic_cache::purge_all();
            tracing::info!("storage: 漫画解压缓存清掉 {removed} 本");
        }

        Job::ClearPartials => {
            if let Some(root) = &areas.library {
                let removed = sweep_partials(root);
                tracing::info!("storage: 半成品清掉 {removed} 个");
            }
        }
        Job::ClearLogs => {
            if let Some(active) = &areas.log_active {
                let (removed, bytes) = purge_rotated(active);
                tracing::info!("storage: 旧日志清掉 {removed} 份（{bytes} 字节）");
            }
        }
        Job::Vacuum => {
            let Some(store) = store else {
                return Err("书库没装上".into());
            };
            let bytes = store.vacuum()?;
            tracing::info!("storage: 整库重写还回 {bytes} 字节");
        }
    }
    Ok(())
}

/// 现读一次账：遍历 + 库里的空闲页。
fn read(areas: &Areas, store: Option<&Store>) -> Snapshot {
    Snapshot {
        usage: usage::measure(areas),
        freeable: store.and_then(|store| store.freeable_bytes().ok()),
    }
}

/// 把字节念成「1.2 GB」这样的一格，小数点与千位按**当前界面语言**写。
///
/// 除数是 1024 而标签写 KB/MB/GB：这台设备上系统文件管理器与 Android 自己那条存储栏都这么
/// 念，读者拿这两处一比才对得上。真要按 1000 进制得改这一处，别在调用点各折一次。
///
/// 小数一位、四舍五入；进到这一档时那个数必然 `>= 1`（循环只在 `>= 1024` 时才降一档），
/// 所以「一个非零的东西念成 0」这一种情况不存在，不需要为它兜底。
///
/// **四舍五入撞到满值要进位**：1 048 575 字节是 1023.99 KB，念成「1024 KB」就是一句自相
/// 矛盾的话（下一格本来就叫 1 MB）。进位之后再按这一门语言的写法落小数点 —— 德语那一版
/// 是「1,5 MB」，不是「1.5 MB」。
pub fn human_bytes(cx: &App, bytes: u64) -> SharedString {
    const KILO: f64 = 1024.0;
    let units = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= KILO && unit + 1 < units.len() {
        value /= KILO;
        unit += 1;
    }
    if unit == 0 {
        return SharedString::from(format!("{} B", l10n::integer(cx, bytes)));
    }
    // KB 这一档整数：读者比的是「几百 K 还是几 K」，那一位小数没有用处。
    let mut shown = if unit == 1 {
        value.round()
    } else {
        (value * 10.0).round() / 10.0
    };
    if shown >= KILO && unit + 1 < units.len() {
        shown = 1.0;
        unit += 1;
    }
    let decimals = if shown.fract() == 0.0 { 0 } else { 1 };
    SharedString::from(format!(
        "{} {}",
        l10n::number(cx, shown, decimals),
        units[unit]
    ))
}

#[cfg(test)]
mod tests {
    //! 这一层能钉的两件事：**字节念出来对不对**（那是一格文字，界面上没有第二处会暴露它），
    //! 以及**状态机不走空**（连点两次不该起两条线，确认面板不该在忙时摊开）。
    //! 遍历本身在 `folio_core::infra::usage` 里有自己的七条门。

    use super::{Area, Job, Snapshot, StorageState, TaskEvent, human_bytes};
    use crate::ui::services::config::ConfigHolder;
    use folio_core::Config;
    use folio_core::infra::usage::Usage;
    use gpui_kit::{AppContext as _, Entity, TestAppContext};

    /// 单位与进位按英文那一门验；分隔符那两门（德语、俄语）另起一条。
    #[gpui_kit::test]
    fn bytes_are_read_out_the_way_a_file_manager_says_them(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| ConfigHolder::install(Config::default(), cx));
        let shown = |bytes: u64| cx.read(|cx| human_bytes(cx, bytes)).to_string();
        assert_eq!(shown(0), "0 B");
        assert_eq!(shown(512), "512 B");
        // B 与 KB 两档是整数计数，所以千位**照这一门语言的规矩分组**：英语是 `1,023 B`。
        assert_eq!(shown(1023), "1,023 B");
        // 1024 进一格，B / KB 两档是整数。
        assert_eq!(shown(1024), "1 KB");
        assert_eq!(shown(1536), "2 KB", "KB 这一档四舍五入，不该念成 1.5");
        assert_eq!(shown(1024 * 1024), "1 MB");
        // MB 以上一位小数；正好是整数时那截 `.0` 不摆。
        assert_eq!(shown(1_572_864), "1.5 MB");
        assert_eq!(shown(100 * 1024 * 1024), "100 MB");
        assert_eq!(shown(3u64 * 1024 * 1024 * 1024 + 1), "3 GB");
        // 刚过一格的那点零头不该被抬成「多出一档」：3 GiB 零 1 个字节念作 3 GB。
        assert_eq!(shown(1024 * 1024 + 1), "1 MB");
        // 四舍五入撞到满值时要**进位**，不是念出「1024 KB」这种自相矛盾的话。
        assert_eq!(shown(1024 * 1024 - 1), "1 MB");
        assert_eq!(shown(1023 * 1024 + 1023), "1 MB", "1023.99 KB 进位成 1 MB");
        assert_eq!(shown(1024 * 1024 * 1024 - 1), "1 GB");
        assert_eq!(shown(1023 * 1024), "1,023 KB", "还没到满值的那一版不许进位");
        assert_eq!(shown(1u64 << 50), "1 PB");
    }

    /// 同一笔字节，在德语与俄语里念出来是**两个不同的字符串**：小数点与千位分隔跟着语言走。
    ///
    /// 这条存在的理由是一个很具体的坏状态：屏幕上写「1.5 MB」，而德语读者写「1,5 MB」——
    /// 数字没跟着翻，读者第一眼看懂的是「这门语言是顺便支持的」。分隔符那张表在
    /// `services::l10n`，这里验它确实落到了这一格文字上。
    #[gpui_kit::test]
    fn the_decimal_point_follows_the_language(cx: &mut TestAppContext) {
        use crate::ui::services::config::ConfigHolder;
        use folio_core::infra::i18n::Locale;
        use folio_core::{Config, Language};

        cx.update(gpui_kit::init);
        let with = |locale: Locale, bytes: u64| {
            cx.update(|cx| {
                ConfigHolder::install(
                    Config {
                        language: Language { locale },
                        ..Default::default()
                    },
                    cx,
                );
            });
            cx.read(|cx| human_bytes(cx, bytes)).to_string()
        };
        assert_eq!(with(Locale::En, 1_572_864), "1.5 MB");
        assert_eq!(with(Locale::ZhHans, 1_572_864), "1.5 MB");
        assert_eq!(with(Locale::De, 1_572_864), "1,5 MB", "德语是小数逗号");
        assert_eq!(with(Locale::Ru, 1_572_864), "1,5 MB");
        assert_eq!(with(Locale::Fr, 1_572_864), "1,5 MB");
        // 分组那一格：B 这一档过千就要看这一门的分隔符。
        assert_eq!(with(Locale::De, 1023), "1.023 B", "德语千位是点");
        assert_eq!(with(Locale::Ru, 1023), "1\u{a0}023 B", "俄语是不换行空格");
        assert_eq!(
            with(Locale::Fr, 1023),
            "1\u{202f}023 B",
            "法语是窄不换行空格"
        );
    }

    /// `busy` 在飞时既不再起第二条线，也不该摊开确认面板。
    ///
    /// 这两条都是「按下去什么都变」的反面：起两条线会让同一批文件被删两遍、两份账互相覆盖；
    /// 忙时摊面板则让读者点下「确认」去跑第三条作业。恒写成不拦，这一屏在真机上就是
    /// 「连点两下然后数字乱跳」。
    #[gpui_kit::test]
    fn the_state_machine_refuses_to_run_two_lines_at_once(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        // 没有配置实体也没有书库：`start` 照样能起（两处都回「没确认过 / 没装上」那一档），
        // 但这一条只问状态机，不去等真作业跑完。
        let view: Entity<StorageState> = cx.update(|cx| cx.new(|_| StorageState::default()));
        let busy = |cx: &mut TestAppContext| cx.read(|cx| view.read(cx).busy());
        let pending = |cx: &mut TestAppContext| cx.read(|cx| view.read(cx).pending());

        cx.update(|cx| view.update(cx, |state, cx| state.ask(Job::ClearThumbs, cx)));
        assert_eq!(
            pending(cx),
            Some(Job::ClearThumbs),
            "没在忙时那一张确认面板该摊开"
        );

        cx.update(|cx| {
            view.update(cx, |state, cx| {
                state.busy = Some(Job::Rescan);
                state.pending = None;
                cx.notify();
            })
        });
        assert!(busy(cx), "夹具把作业置成在飞");
        cx.update(|cx| view.update(cx, |state, cx| state.ask(Job::ClearComics, cx)));
        assert_eq!(pending(cx), None, "忙时不该摊开确认面板");
        cx.update(|cx| {
            view.update(cx, |state, cx| state.start(Job::ClearLogs, cx));
        });
        assert_eq!(
            cx.read(|cx| view.read(cx).busy_job()),
            Some(Job::Rescan),
            "在飞的那一条不该被第二颗按钮顶掉"
        );
        // `Rescan` 不管任何一类字节，所以它没有「确认」这一档。
        cx.update(|cx| {
            view.update(cx, |state, cx| {
                state.busy = None;
                state.ask(Job::Rescan, cx);
            })
        });
        assert_eq!(pending(cx), None, "重扫不该摊开确认面板");
    }

    /// 失败那一档：旧账留着、`busy` 归位、原因交出去。
    ///
    /// 这条钉的是「统计没跑成」那句话真的有一条能走到它的通路（`VACUUM` 撞盘满是最现实的
    /// 一种）。写成吞掉错误的话，屏幕上是一列悄悄停在旧值的数，而那颗「重新统计」看起来
    /// 像刚跑成功过。
    #[gpui_kit::test]
    fn a_failed_job_keeps_the_old_ledger_and_says_why(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let view: Entity<StorageState> = cx.update(|cx| cx.new(|_| StorageState::default()));
        cx.update(|cx| {
            view.update(cx, |state, cx| {
                state.set_snapshot_for_test(Snapshot {
                    usage: Usage::default(),
                    freeable: None,
                });
                state.busy = Some(Job::Vacuum);
                state.finish(TaskEvent::Finished(Err("disk full".to_string().into())), cx);
            })
        });
        let (busy, reason, kept) = cx.read(|cx| {
            let state = view.read(cx);
            (state.busy(), state.reason(), state.snapshot().is_some())
        });
        assert!(!busy, "失败也该把这一条线收掉，不然那颗钮永远按不动");
        assert_eq!(
            reason.map(|text| text.to_string()),
            Some("disk full".into())
        );
        assert!(kept, "失败时旧的账要留着：一屏空白比一个过期的数更难查");
    }

    /// 那条桥真走得通：作业在池子的线程上跑完，账落回实体，`busy` 归位。
    ///
    /// 这一条不验分类账（`folio_core::infra::usage` 自己有七门），验的是**接线**：
    /// `mpsc` → `background_spawn` → `async_channel` → `cx.spawn` → 实体。中间任何一环接错
    /// （比如忘了 `send_blocking`、或泵没有 `detach`），屏幕上的表现是「统计」转圈转到永远。
    ///
    /// 轮询而不是 `run_until_parked()` 一次就收工：真正干活的是池子那条 OS 线程，而等事件的
    /// 那条 future 处在 parked 状态 —— 「执行器没活干」与「作业跑完了」不是一件事。
    #[gpui_kit::test]
    async fn a_finished_job_lands_its_ledger_back_on_the_screen(cx: &mut TestAppContext) {
        use folio_core::infra::usage::Areas;
        use std::io::Write as _;

        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(data.join("logs")).unwrap();
        std::fs::File::create(data.join("folio.db"))
            .unwrap()
            .write_all(&[b'x'; 4096])
            .unwrap();
        std::fs::File::create(data.join("logs/folio.log.1"))
            .unwrap()
            .write_all(&[b'x'; 64])
            .unwrap();
        let areas = Areas {
            config: dir.path().join("config"),
            data: data.clone(),
            db: data.join("folio.db"),
            logs: data.join("logs"),
            log_active: Some(data.join("logs/folio.log")),
            inbox: data.join("inbox"),
            comics: data.join("cache/comics"),
            library: None,
        };

        let view: Entity<StorageState> = cx.update(|cx| cx.new(|_| StorageState::default()));
        cx.update(|cx| {
            view.update(cx, |state, cx| {
                state.start_at(Job::Rescan, areas.clone(), cx)
            })
        });
        assert!(
            cx.read(|cx| view.read(cx).busy()),
            "刚起活该是在飞，屏幕那一头要摆『正在统计』"
        );
        assert!(cx.read(|cx| view.read(cx).snapshot().is_none()));

        for _ in 0..400 {
            cx.run_until_parked();
            if !cx.read(|cx| view.read(cx).busy()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let (busy, reason, snapshot) = cx.read(|cx| {
            let state = view.read(cx);
            (state.busy(), state.reason(), state.snapshot().cloned())
        });
        assert!(!busy, "作业跑完了，`busy` 该归位");
        assert_eq!(reason, None, "这一趟不该失败");
        let snapshot = snapshot.expect("账该落回实体");
        assert_eq!(snapshot.usage.get(Area::Database).bytes, 4096);
        assert_eq!(snapshot.usage.get(Area::RotatedLogs).bytes, 64);
        // 库没装上时那一格是「没去看」，不是零。
        assert_eq!(snapshot.freeable, None);

        // 删掉一份旧日志再重扫：数字跟着掉，这一屏不靠推算。
        cx.update(|cx| view.update(cx, |state, cx| state.start_at(Job::ClearLogs, areas, cx)));
        for _ in 0..400 {
            cx.run_until_parked();
            if !cx.read(|cx| view.read(cx).busy()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let after = cx
            .read(|cx| view.read(cx).snapshot().cloned())
            .expect("第二次也该有账");
        assert_eq!(after.usage.get(Area::RotatedLogs).bytes, 0);
        assert!(
            !std::path::Path::new(&data.join("logs/folio.log.1")).exists(),
            "那颗钮该真的删掉轮转出来的那一份"
        );
    }

    /// 每一档删除动作都只对一个「去处」说话。
    ///
    /// 界面按这一条决定那一行的按钮摆不摆、确认话点名点谁。写错一格（比如 `ClearLogs`
    /// 指向当前那份日志而不是轮转那几份），那颗钮删的就是**正在写的那一份**。
    #[test]
    fn each_clear_job_names_exactly_one_area() {
        assert_eq!(Job::Rescan.area(), None);
        assert_eq!(Job::ClearThumbs.area(), Some(Area::Thumbnails));
        assert_eq!(Job::ClearComics.area(), Some(Area::Comics));
        assert_eq!(Job::ClearPartials.area(), Some(Area::Staging));
        assert_eq!(Job::ClearLogs.area(), Some(Area::RotatedLogs));
        assert_eq!(Job::Vacuum.area(), Some(Area::Database));
        // 每一档都对得上一个真实存在的类别，没有一个按钮管着一格没人认领的字节。
        let named: Vec<Area> = [
            Job::ClearThumbs,
            Job::ClearComics,
            Job::ClearPartials,
            Job::ClearLogs,
            Job::Vacuum,
        ]
        .into_iter()
        .filter_map(Job::area)
        .collect();
        assert_eq!(named.len(), 5, "五档动作五种类，不该有重复或漏项");
        assert_eq!(
            named.iter().collect::<std::collections::HashSet<_>>().len(),
            5,
            "两档动作撞进同一类，界面上就是两颗都管同一格的按钮"
        );
    }
}
