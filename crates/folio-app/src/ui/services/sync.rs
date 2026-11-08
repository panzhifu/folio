//! WebDAV 同步的应用侧：配置 → 客户端 → 后台一趟 → 设置页重画。
//!
//! 状态挂 `Entity` 而不是 plain `Global`，理由与 [`super::update`] 同一份：设置页的取值
//! 闭包只在重绘时被调，后台那趟回来若不 `notify`，那一格会永远停在「正在同步…」。
//!
//! 这一层只做编排：协议与合并都在 `folio_core::sync` / `net::remote::dav`，书库读写都在
//! `Store`。**同步不在界面线程上跑** —— `Transport` 是同步的，一次远端往返能卡住窗口。

use std::sync::Arc;

use folio_core::Sync as SyncConfig;
use folio_core::SyncProvider;
use folio_core::net::remote::{self, RemoteStore, Transport, dav};
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

/// 书架传书要的「按当前配置现造一台仓库」。`None` = 没配好（地址缺失、百度没登录）。
/// 返回的第二格是这一台的远端目录 —— 书文件落在它下面的 `books/`。
pub fn remote_store(cx: &App) -> Option<(Box<dyn RemoteStore>, String)> {
    let config = ConfigHolder::sync(cx);
    let remote = client(&config, cx).ok()?;
    Some((remote, config.remote_dir))
}

/// 按配置现造一台远端仓库。WebDAV 走地址与账号；百度网盘复用「网盘取书」那一份
/// token，根钉在应用目录（`/apps/<应用名>/`，第三方应用唯一可写的地方）。
fn client(config: &SyncConfig, cx: &App) -> Result<Box<dyn RemoteStore>, Failure> {
    match config.provider {
        SyncProvider::WebDav => {
            let url = config.url.trim();
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                return Err(Failure::Config);
            }
            let endpoint =
                dav::Endpoint::new(url, config.username.clone(), config.password.clone());
            Ok(Box::new(dav::Client::new(endpoint, transport())))
        }
        // 两家网盘都套上凭据续期（`AuthedStore`）：同步那趟跑在后台，token 过期时
        // 自己续、401 强刷重放一次，轮换结果经通道写回 config。手贴 token（没有
        // refresh_token）续不了就交回原 401 —— 设置页的「登录被拒」照旧翻得出来。
        SyncProvider::Baidu => {
            let cloud = ConfigHolder::cloud(cx);
            if !cloud.baidu.configured() {
                return Err(Failure::Config);
            }
            Ok(Box::new(super::cloud_auth::baidu_sync(
                &cloud.baidu,
                transport(),
            )))
        }
        SyncProvider::Aliyun => {
            let cloud = ConfigHolder::cloud(cx);
            if !cloud.aliyun.configured() {
                return Err(Failure::Config);
            }
            Ok(Box::new(super::cloud_auth::aliyun_sync(
                &cloud.aliyun,
                transport(),
            )))
        }
    }
}

/// 这台安装的稳定 id：第一次要用到时生成并留下，之后每一趟都读同一个。
///
/// 它在 WebDAV 那一侧是远端那份快照的文件名（`<device_id>.json`），在局域网那一侧是快照上的署名
/// —— 「这台安装」是同一件事，所以两条路共用一份 id、只生成一次。
///
/// 回 `None` 只在**配置里那一格已经写了东西、而它不成形状**时：人改过的文件不该被我们悄悄换成
/// 一个新 id（那等于把读者在远端留下的那一份孤儿化，从此对面读不到它），也不该继续用它 —— 这一格
/// 要当文件名，一个斜杠就能把一台设备写到别的目录去。报错比猜意图便宜。
pub fn ensure_device_id(cx: &mut App) -> Option<String> {
    let current = ConfigHolder::sync(cx).device_id;
    if !current.is_empty() {
        return sync::valid_device_id(&current).then_some(current);
    }
    let id = sync::new_device_id();
    ConfigHolder::edit_sync(cx, |config| config.device_id = id.clone());
    Some(id)
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
    let client = match client(&config, cx) {
        Ok(client) => client,
        Err(failure) => {
            state.update(cx, |state, cx| state.begin(Status::Failed(failure), cx));
            return;
        }
    };
    let Some(id) = ensure_device_id(cx) else {
        state.update(cx, |state, cx| {
            state.begin(Status::Failed(Failure::Config), cx)
        });
        return;
    };
    config.device_id = id;

    state.update(cx, |state, cx| state.begin(Status::Syncing, cx));
    let dir = config.remote_dir.clone();
    let device = config.device_id.clone();
    let now = folio_core::infra::time::now();
    let work = cx.background_spawn(async move {
        sync::sync_once(store.as_ref(), client.as_ref(), &dir, &device, now)
    });
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
    let client = match client(&config, cx) {
        Ok(client) => client,
        Err(failure) => {
            state.update(cx, |state, cx| state.begin(Status::Failed(failure), cx));
            return;
        }
    };
    state.update(cx, |state, cx| state.begin(Status::Testing, cx));
    let dir = config.remote_dir.clone();
    let work = cx.background_spawn(async move { sync::test_connection(client.as_ref(), &dir) });
    let weak = state.downgrade();
    cx.spawn(async move |cx| {
        let outcome = work.await;
        let _ = weak.update(cx, |state, cx| state.settle_test(outcome, cx));
    })
    .detach();
}
