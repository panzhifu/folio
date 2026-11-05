//! 设置里「备份」那一屏的作业与状态。
//!
//! 与「存储」那一屏同一个形状：静态设置页每帧重建、拿不住 `Task`，所以备份的列表与
//! 打包进度收在一个被观察的实体里，后台线程跑完 `notify`，两端的设置屏跟着重画。
//!
//! 三条纪律：
//!
//! 1. **打包不在界面线程上走**。一个几 GB 的书库要读几百上千个文件，还要让 SQLite
//!    先 `VACUUM INTO` 一份副本。全部交给 [`resident::pool`]，桥照 `storage.rs`：
//!    `mpsc` 阻塞读的那一头丢进 `background_spawn`，`async_channel` 这头像 `cx.spawn`
//!    那样 await。
//! 2. **一次只跑一条**。打包在飞时「再打一份」与「删一份」都直接回：读者看到的那颗
//!    按钮已经变成「正在备份…」，再来一条只会让两个作业抢同一批文件。
//! 3. **跑完重列一遍**。列表不是「本地拼一条」而是重新读目录 —— 屏幕上那份清单就是
//!    磁盘上现在的那一份，不是我们以为它该变成的样子。

use std::path::PathBuf;

use folio_core::Store;
use folio_core::backup::{self, Created, Entry, Options, Roots};
use folio_core::{Priority, TaskEvent};
use gpui_kit::{App, AppContext as _, Context, Entity, Global, SharedString, Task, WeakEntity};

use super::app_store;
use super::config::ConfigHolder;
use super::resident;

/// 这一屏能发起的作业。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Job {
    /// 只读目录，不动任何东西。
    List,
    /// 打一份新包。
    Create,
    /// 删掉列表里那一条。
    Delete,
}

impl Job {
    /// 稳定的英文小名：元素 id 用它，切语言不该把 hover 与焦点状态换一套。
    pub fn key(self) -> &'static str {
        match self {
            Job::List => "list",
            Job::Create => "create",
            Job::Delete => "delete",
        }
    }
}

/// 一条作业交回来的东西：新的清单，外加刚打好的那一份（只有 `Create` 有）。
struct Outcome {
    entries: Vec<Entry>,
    created: Option<Created>,
}

/// 列表 + 这一屏的状态机。视图 `observe` 它，跑完作业由它 `notify`。
#[derive(Default)]
pub struct BackupState {
    /// `None` = 还没读过目录。第一帧就是这个状态，界面上是一句「正在读取备份…」。
    entries: Option<Vec<Entry>>,
    /// 正在跑的那一条作业。
    busy: Option<Job>,
    /// 最近一次成功打包的结果。列表刷新时**不清**：它是「刚刚那一下」的反馈，
    /// 不是目录的派生值。
    last: Option<Created>,
    /// 上一次失败的原因（技术话，不译）。
    reason: Option<SharedString>,
    /// 等读者确认要删的那一份。同一时刻只可能有一张确认面板。
    pending_delete: Option<PathBuf>,
    /// 在飞那条线的事件泵。跟着实体一起掉。
    _pump: Option<Task<()>>,
}

impl BackupState {
    /// 最近一次读到的清单；还没读过时 `None`。
    pub fn entries(&self) -> Option<&[Entry]> {
        self.entries.as_deref()
    }

    /// 有没有作业在飞。
    pub fn busy(&self) -> bool {
        self.busy.is_some()
    }

    /// 在飞的那一条是哪一条。
    pub fn busy_job(&self) -> Option<Job> {
        self.busy
    }

    /// 最近一次成功打包的结果。
    pub fn last(&self) -> Option<&Created> {
        self.last.as_ref()
    }

    /// 失败的技术原因。
    pub fn reason(&self) -> Option<SharedString> {
        self.reason.clone()
    }

    /// 现在摊开的那张确认面板属于哪一份包。
    pub fn pending_delete(&self) -> Option<&PathBuf> {
        self.pending_delete.as_ref()
    }

    /// 测试用：直接把一份清单摆上这一屏。
    #[cfg(test)]
    pub fn set_entries_for_test(&mut self, entries: Vec<Entry>) {
        self.entries = Some(entries);
    }

    /// 点某一行的「删除」：先摊开确认面板，不动磁盘。
    pub fn ask_delete(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.pending_delete = Some(path);
        cx.notify();
    }

    /// 确认面板上点「取消」。
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        if self.pending_delete.take().is_some() {
            cx.notify();
        }
    }

    /// 确认面板上点「删除」：收起面板并起活。
    pub fn confirm_delete(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.pending_delete.take() else {
            return;
        };
        self.start(Job::Delete, Some(path), cx);
    }

    /// 起一条作业。已经有作业在飞时这一句是空转。
    pub fn start(&mut self, job: Job, path: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.start_at(
            job,
            path,
            backup::Roots::from_config(&ConfigHolder::user_config(cx)),
            cx,
        );
    }

    /// 同 [`start`](Self::start)，但根目录由调用方给 —— 测试用它把作业指到临时目录，
    /// 不碰进程级的 `FOLIO_DATA_DIR`（理由与 `storage.rs` 的 `start_at` 同一条）。
    fn start_at(&mut self, job: Job, path: Option<PathBuf>, roots: Roots, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let store = app_store::handle(cx);
        let config = ConfigHolder::user_config(cx);
        let options = Options::from(config.backup);
        let handle = resident::pool().submit_with(Priority::Interactive, move |ctx| {
            run(
                job,
                path.as_deref(),
                &roots,
                store.as_deref(),
                &config,
                options,
                ctx,
            )
        });
        self.busy = Some(job);
        let (forward, events) = async_channel::unbounded::<TaskEvent<Outcome>>();
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
    fn finish(&mut self, event: TaskEvent<Outcome>, cx: &mut Context<Self>) {
        match event {
            TaskEvent::Started | TaskEvent::Progress(_) | TaskEvent::Bytes(_) => {}
            TaskEvent::Finished(Ok(outcome)) => {
                self.entries = Some(outcome.entries);
                if let Some(created) = outcome.created {
                    self.last = Some(created);
                }
                self.busy = None;
                self.reason = None;
                cx.notify();
            }
            TaskEvent::Finished(Err(err)) => {
                // 旧清单**留着**：删一份失败时屏幕变成空白，比一份过期目录更没用。
                self.busy = None;
                self.reason = Some(err.to_string().into());
                tracing::warn!("backup: 作业失败：{err:#}");
                cx.notify();
            }
            TaskEvent::Cancelled => {
                self.busy = None;
                cx.notify();
            }
        }
    }
}

/// 作业本体。跑在池子的工作线程上，只碰路径、库连接与这份配置。
fn run(
    job: Job,
    path: Option<&std::path::Path>,
    roots: &Roots,
    store: Option<&Store>,
    config: &folio_core::Config,
    options: Options,
    ctx: &folio_core::StageCtx,
) -> Result<Outcome, folio_core::TaskError> {
    match job {
        Job::List => Ok(Outcome {
            entries: backup::list(roots)?,
            created: None,
        }),
        Job::Create => {
            let Some(store) = store else {
                return Err("书库没装上，打不了备份".into());
            };
            let created = backup::create(store, config, roots, options, &mut |done, total| {
                ctx.item(done, total);
            })?;
            Ok(Outcome {
                entries: backup::list(roots)?,
                created: Some(created),
            })
        }
        Job::Delete => {
            let Some(path) = path else {
                return Err("删除备份时没给路径".into());
            };
            backup::remove(roots, path)?;
            Ok(Outcome {
                entries: backup::list(roots)?,
                created: None,
            })
        }
    }
}

/// 挂这一屏的 `Global`。
pub struct BackupHolder(pub Entity<BackupState>);

impl Global for BackupHolder {}

/// 实体句柄；还没建过时 `None`。
pub fn handle(cx: &App) -> Option<Entity<BackupState>> {
    cx.try_global::<BackupHolder>()
        .map(|holder| holder.0.clone())
}

/// 取句柄，没有就先建一个。与 `storage::entity` 同一理由：按钮回调手里就是 `&mut App`，
/// 设置页不必等外壳先装过。
pub fn entity(cx: &mut App) -> Entity<BackupState> {
    if let Some(entity) = handle(cx) {
        return entity;
    }
    let entity = cx.new(|_cx| BackupState::default());
    cx.set_global(BackupHolder(entity.clone()));
    entity
}

/// 起一条作业。拿不到实体时先建一个。
pub fn request(cx: &mut App, job: Job) {
    let entity = entity(cx);
    entity.update(cx, |state, cx| state.start(job, None, cx));
}

/// 点某一行的「删除」：摊开确认面板。
pub fn ask_delete(cx: &mut App, path: PathBuf) {
    let entity = entity(cx);
    entity.update(cx, |state, cx| state.ask_delete(path, cx));
}

/// 确认面板上那两个动作。
pub fn answer(cx: &mut App, accept: bool) {
    let entity = entity(cx);
    entity.update(cx, |state, cx| {
        if accept {
            state.confirm_delete(cx);
        } else {
            state.dismiss(cx);
        }
    });
}

/// 把字节念成「1.2 GB」那一格，交给「存储」那一屏的同一份实现 —— 两屏报的是同一类
/// 读数，读者不该在两处看到两种小数写法。
pub use super::storage::human_bytes;

#[cfg(test)]
mod tests {
    //! 这一层钉两件事：状态机不走空（忙时不能再起一条），以及**真作业跑完把清单交回
    //! 这一屏**。后者走 `start_at` 把根指到临时目录，不碰进程级环境。

    use super::*;
    use folio_core::Config;
    use gpui_kit::TestAppContext;

    fn roots(dir: &std::path::Path) -> (Roots, Config) {
        let data = dir.join("data");
        let library = data.join("books");
        std::fs::create_dir_all(&library).unwrap();
        let mut config = Config::default();
        config.library.dir = library.to_string_lossy().into_owned();
        config.backup.include_files = false;
        let roots = Roots {
            backups: data.join("backups"),
            data,
            library: Some(library),
            dicts: None,
            themes: None,
        };
        (roots, config)
    }

    #[gpui_kit::test]
    fn a_finished_create_lands_its_entry_back_on_the_screen(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let (roots, config) = roots(dir.path());
        let store = Store::open_at(&roots.data.join("folio.db")).unwrap();
        cx.update(|cx| app_store::install(store, cx));
        cx.update(|cx| ConfigHolder::install(config.clone(), cx));

        let state = cx.update(entity);
        cx.update(|cx| {
            state.update(cx, |state, cx| {
                state.start_at(Job::Create, None, roots.clone(), cx)
            });
        });
        for _ in 0..400 {
            cx.background_executor.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(5));
            if cx.update(|cx| !state.read(cx).busy() && state.read(cx).entries().is_some()) {
                break;
            }
        }
        let entries = cx.update(|cx| state.read(cx).entries().map(|e| e.to_vec()));
        let entries = entries.expect("作业跑完后该有清单");
        assert_eq!(entries.len(), 1, "刚打的那一份应当在列表里");
        assert!(
            cx.update(|cx| state.read(cx).last().is_some()),
            "成功那一趟要留下「刚刚打的这一份」"
        );
    }

    #[gpui_kit::test]
    fn a_busy_screen_refuses_a_second_job(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let (roots, _config) = roots(dir.path());
        let state = cx.update(entity);
        state.update(cx, |state, cx| {
            // 直接占住忙位：这一条不需要真起线程，验的是入口那道闸。
            state.busy = Some(Job::Create);
            state.start_at(Job::List, None, roots, cx);
            assert_eq!(state.busy_job(), Some(Job::Create));
        });
    }
}
