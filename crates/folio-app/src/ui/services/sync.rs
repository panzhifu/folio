//! WebDAV 同步的应用侧：配置 → 客户端 → 后台一趟 → 设置页重画。
//!
//! 状态挂 `Entity` 而不是 plain `Global`，理由与 [`super::update`] 同一份：设置页的取值
//! 闭包只在重绘时被调，后台那趟回来若不 `notify`，那一格会永远停在「正在同步…」。
//!
//! 这一层只做编排：协议与合并都在 `folio_core::sync` / `net::remote::dav`，书库读写都在
//! `Store`。**同步不在界面线程上跑** —— `Transport` 是同步的，一次远端往返能卡住窗口。

use std::sync::Arc;

use folio_core::Sync as SyncConfig;
use folio_core::net::remote::{self, Transport, dav};
use folio_core::sync::{self, Report};
use gpui_kit::{App, AppContext as _, Context, Entity, Global};

use crate::ui::services::config::ConfigHolder;
use crate::ui::services::{app_store, update};

/// 设置页那一格该念什么。`Done` 里带着这次同步的计数。
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Status {
    #[default]
    Idle,
    Testing,
    /// 连接成功。下一次点「测试」或「同步」会覆盖它。
    Tested,
    Syncing,
    Done(Report),
    Failed(Failure),
}

/// 失败只分到「读者能采取行动」的那几档；原文进日志，不上屏。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// 地址没填 / 不是 http(s)。
    Config,
    /// 用户名、密码或服务器要求的认证方式不接受。
    Auth,
    /// 连不上（DNS、TLS、超时）。
    Network,
    /// 连上了，但服务器拒绝或返回读不懂的内容。
    Remote,
    /// 本机书库读写失败。
    Store,
}

#[derive(Debug, Default)]
pub struct SyncState {
    status: Status,
}

impl SyncState {
    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn busy(&self) -> bool {
        matches!(self.status, Status::Testing | Status::Syncing)
    }

    fn begin(&mut self, status: Status, cx: &mut Context<Self>) {
        self.status = status;
        cx.notify();
    }

    fn settle(&mut self, outcome: Result<Report, sync::Error>, cx: &mut Context<Self>) {
        match outcome {
            Ok(report) => {
                self.status = Status::Done(report);
            }
            Err(error) => {
                tracing::warn!("同步失败：{error}");
                self.status = Status::Failed(failure_of(&error));
            }
        }
        cx.notify();
    }

    fn settle_test(&mut self, outcome: Result<(), sync::Error>, cx: &mut Context<Self>) {
        self.status = match outcome {
            Ok(()) => Status::Tested,
            Err(error) => {
                tracing::warn!("同步连接测试失败：{error}");
                Status::Failed(failure_of(&error))
            }
        };
        cx.notify();
    }
}

fn failure_of(error: &sync::Error) -> Failure {
    use remote::Error as Remote;
    use sync::Error as Sync;
    match error {
        Sync::Config(_) => Failure::Config,
        Sync::Store(_) => Failure::Store,
        Sync::Json(_) => Failure::Remote,
        Sync::Remote(Remote::Transport(_)) => Failure::Network,
        Sync::Remote(
            Remote::Unauthorized { .. } | Remote::Forbidden { .. } | Remote::DigestRequired,
        ) => Failure::Auth,
        Sync::Remote(_) => Failure::Remote,
    }
}

pub struct SyncHolder(pub Entity<SyncState>);

impl Global for SyncHolder {}

pub fn handle(cx: &App) -> Option<Entity<SyncState>> {
    cx.try_global::<SyncHolder>().map(|holder| holder.0.clone())
}

/// 取句柄，没有就建一个（与 `update::entity` / `storage::entity` 同一条理由：
/// 设置页构造时手里就是 `&mut App`，不必等外壳先装）。
pub fn entity(cx: &mut App) -> Entity<SyncState> {
    if let Some(entity) = handle(cx) {
        return entity;
    }
    let entity = cx.new(|_| SyncState::default());
    cx.set_global(SyncHolder(entity.clone()));
    entity
}

/// 进程共用的出网传输：与检查更新、书架 URL 导入同一池。
fn transport() -> Arc<dyn Transport> {
    update::transport()
}

fn client(config: &SyncConfig) -> Result<dav::Client, Failure> {
    let url = config.url.trim();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(Failure::Config);
    }
    let endpoint = dav::Endpoint::new(url, config.username.clone(), config.password.clone());
    Ok(dav::Client::new(endpoint, transport()))
}

/// 「立即同步」。已经在飞时什么都不做（按钮那边同时会禁用）。
pub fn sync_now(cx: &mut App) {
    let state = entity(cx);
    if state.read(cx).busy() {
        return;
    }
    let Some(store) = app_store::handle(cx) else {
        state.update(cx, |state, cx| {
            state.begin(Status::Failed(Failure::Store), cx)
        });
        return;
    };

    let mut config = ConfigHolder::sync(cx);
    if !config.enabled {
        state.update(cx, |state, cx| {
            state.begin(Status::Failed(Failure::Config), cx)
        });
        return;
    }
    let client = match client(&config) {
        Ok(client) => client,
        Err(failure) => {
            state.update(cx, |state, cx| state.begin(Status::Failed(failure), cx));
            return;
        }
    };
    if config.device_id.is_empty() {
        let id = sync::new_device_id();
        ConfigHolder::edit_sync(cx, |sync| sync.device_id = id.clone());
        config.device_id = id;
    }
    if !sync::valid_device_id(&config.device_id) {
        state.update(cx, |state, cx| {
            state.begin(Status::Failed(Failure::Config), cx)
        });
        return;
    }

    state.update(cx, |state, cx| state.begin(Status::Syncing, cx));
    let dir = config.remote_dir.clone();
    let device = config.device_id.clone();
    let now = folio_core::infra::time::now();
    let work =
        cx.background_spawn(
            async move { sync::sync_once(store.as_ref(), &client, &dir, &device, now) },
        );
    let weak = state.downgrade();
    cx.spawn(async move |cx| {
        let outcome = work.await;
        let _ = weak.update(cx, |state, cx| state.settle(outcome, cx));
    })
    .detach();
}

/// 「测试连接」：连上并确保远端目录可建，不读不写任何书籍数据。
pub fn test_now(cx: &mut App) {
    let state = entity(cx);
    if state.read(cx).busy() {
        return;
    }
    let config = ConfigHolder::sync(cx);
    let client = match client(&config) {
        Ok(client) => client,
        Err(failure) => {
            state.update(cx, |state, cx| state.begin(Status::Failed(failure), cx));
            return;
        }
    };
    state.update(cx, |state, cx| state.begin(Status::Testing, cx));
    let dir = config.remote_dir.clone();
    let work = cx.background_spawn(async move { sync::test_connection(&client, &dir) });
    let weak = state.downgrade();
    cx.spawn(async move |cx| {
        let outcome = work.await;
        let _ = weak.update(cx, |state, cx| state.settle_test(outcome, cx));
    })
    .detach();
}
