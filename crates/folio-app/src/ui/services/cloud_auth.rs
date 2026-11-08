//! 网盘凭据的**接线**：把 core 的 [`TokenSource`] / [`AuthedStore`] 与这台设备的
//! config 接到一起。
//!
//! 三件事：
//!
//! 1. **轮换落盘**：续期发生在后台线程，config 的写回要走主线程（`ConfigHolder` 是
//!    gpui 的 `Entity`）。`install` 在开机时起一条**常驻主线程任务**，把通道里排队的
//!    轮换结果逐条写回配置 —— `edit` 会 `notify`，落盘由外壳那份观察者接手。
//! 2. **凭据来源**：`source_*` 从 config 现折一份初始 `Token`（那一刻在主线程上），
//!    包成进程内单飞的 `SharedTokenSource`。
//! 3. **仓库装饰**：`drive_*` 用 [`AuthedStore`] 包住两家客户端 —— 过期先续、401
//!    强刷一次重放。浏览（`cloud::view`）与同步（`services::sync`）两条路都从这里拿，
//!    「401 之后怎么办」从此只有一份答案。
//!
//! 手贴 token 那条路（没有 refresh_token）也走同一个入口：`SharedTokenSource` 对
//! 空的 refresh_token 永远不主动续，`AuthedStore` 的重放失败会把原来的 401 原样
//! 交回界面 —— 与从前「过期请重贴」同一句话，只是不再需要界面自己兜。

use std::sync::{Arc, OnceLock};

use async_channel::TrySendError;
use folio_core::net::cloud::aliyun::AliyunDrive;
use folio_core::net::cloud::aliyun_auth::AliyunRefresher;
use folio_core::net::cloud::auth::{AuthedStore, SharedTokenSource, Token, TokenStore};
use folio_core::net::cloud::baidu::BaiduDrive;
use folio_core::net::cloud::baidu_auth::BaiduRefresher;
use folio_core::net::remote::Transport;
use gpui_kit::App;

use crate::ui::services::config::ConfigHolder;

/// 轮换结果的落点：续期发生在后台线程，这里排个队，主线程那条常驻任务来消化。
static PENDING: OnceLock<async_channel::Sender<Rotation>> = OnceLock::new();

/// 哪一家网盘轮换了。写回 config 时按它落到对应的节。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Baidu,
    Aliyun,
}

/// 一次轮换：新的 token 对 + 它属于谁。
struct Rotation {
    provider: Provider,
    token: Token,
}

/// 收轮换结果的 [`TokenStore`]：`save` 在后台线程上被调，这里只入队。
struct Pending {
    provider: Provider,
}

impl TokenStore for Pending {
    fn save(&self, token: Token) {
        let tx = PENDING.get_or_init(|| {
            // 通道只在 `install` 里被真正接上；没装过（测试）就现造一条没人收的 ——
            // 轮换结果进队即弃，本会话还能用，重启后才需要盘上那份。
            let (tx, _rx) = async_channel::unbounded();
            tx
        });
        match tx.try_send(Rotation {
            provider: self.provider,
            token,
        }) {
            Ok(()) => {}
            Err(TrySendError::Closed(_)) => tracing::warn!("cloud_auth: 轮换队列已关，丢弃"),
            Err(TrySendError::Full(_)) => unreachable!("无界通道不会满"),
        }
    }
}

/// 开机装配：起那条常驻主线程任务。**桌面与移动两个 boot 各调一次。**
///
/// 任务活着，通道里的轮换结果才会落盘 —— 它是 `detach` 的，进程活多久它活多久。
pub fn install(cx: &mut App) {
    let (tx, rx) = async_channel::unbounded::<Rotation>();
    if PENDING.set(tx).is_err() {
        tracing::warn!("cloud_auth: 重复 install，忽略");
        return;
    }
    cx.spawn(async move |cx: &mut gpui_kit::AsyncApp| {
        while let Ok(rotation) = rx.recv().await {
            // `AsyncApp::update` 把主线程的 `&mut App` 借出来：config 的写回必须在这一侧。
            cx.update(|app| persist(rotation, app));
        }
    })
    .detach();
}

/// 把一次轮换写回 config。**只动 token 三格** —— 账号名、drive_id 这些不是这次续期
/// 换来的，写回时原样保留（webdav / 其余节碰都不碰）。
fn persist(rotation: Rotation, app: &mut App) {
    let Token {
        access,
        refresh,
        expires_at,
    } = rotation.token;
    ConfigHolder::edit_cloud(app, |cloud| match rotation.provider {
        Provider::Baidu => {
            cloud.baidu.access_token = access;
            cloud.baidu.refresh_token = refresh;
            cloud.baidu.expires_at = expires_at;
        }
        Provider::Aliyun => {
            cloud.aliyun.access_token = access;
            cloud.aliyun.refresh_token = refresh;
            cloud.aliyun.expires_at = expires_at;
        }
    });
}

/// 从 config 的百度那一节折一份初始 `Token`。
fn baidu_token(baidu: &folio_core::BaiduDrive) -> Token {
    Token {
        access: baidu.access_token.clone(),
        refresh: baidu.refresh_token.clone(),
        expires_at: baidu.expires_at,
    }
}

/// 从 config 的阿里云盘那一节折一份初始 `Token`。
fn aliyun_token(aliyun: &folio_core::AliyunDrive) -> Token {
    Token {
        access: aliyun.access_token.clone(),
        refresh: aliyun.refresh_token.clone(),
        expires_at: aliyun.expires_at,
    }
}

/// 浏览用的百度仓库：根为空（整个网盘），token 续期接上。
pub fn baidu_browse(
    cloud: &folio_core::BaiduDrive,
    transport: Arc<dyn Transport>,
) -> AuthedStore<BaiduDrive> {
    AuthedStore::new(
        {
            let transport = Arc::clone(&transport);
            move |access: &str| BaiduDrive::new(access, Arc::clone(&transport))
        },
        Arc::new(SharedTokenSource::new(
            baidu_token(cloud),
            Arc::new(Pending {
                provider: Provider::Baidu,
            }),
            Arc::new(BaiduRefresher),
            transport,
        )),
    )
}

/// 同步用的百度仓库：根钉在应用目录（第三方应用唯一可写的地方），token 续期接上。
pub fn baidu_sync(
    cloud: &folio_core::BaiduDrive,
    transport: Arc<dyn Transport>,
) -> AuthedStore<BaiduDrive> {
    AuthedStore::new(
        {
            let transport = Arc::clone(&transport);
            move |access: &str| {
                BaiduDrive::with_root(
                    access,
                    Arc::clone(&transport),
                    folio_core::net::cloud::baidu::APP_ROOT,
                )
            }
        },
        Arc::new(SharedTokenSource::new(
            baidu_token(cloud),
            Arc::new(Pending {
                provider: Provider::Baidu,
            }),
            Arc::new(BaiduRefresher),
            transport,
        )),
    )
}

/// 浏览用的阿里云盘仓库。
pub fn aliyun_browse(
    aliyun: &folio_core::AliyunDrive,
    transport: Arc<dyn Transport>,
) -> AuthedStore<AliyunDrive> {
    AuthedStore::new(
        {
            let transport = Arc::clone(&transport);
            let drive_id = aliyun.drive_id.clone();
            move |access: &str| AliyunDrive::new(access, drive_id.clone(), Arc::clone(&transport))
        },
        Arc::new(SharedTokenSource::new(
            aliyun_token(aliyun),
            Arc::new(Pending {
                provider: Provider::Aliyun,
            }),
            Arc::new(AliyunRefresher),
            transport,
        )),
    )
}

/// 同步用的阿里云盘仓库：根钉在应用目录。
pub fn aliyun_sync(
    aliyun: &folio_core::AliyunDrive,
    transport: Arc<dyn Transport>,
) -> AuthedStore<AliyunDrive> {
    AuthedStore::new(
        {
            let transport = Arc::clone(&transport);
            let drive_id = aliyun.drive_id.clone();
            move |access: &str| {
                AliyunDrive::with_root(
                    access,
                    drive_id.clone(),
                    Arc::clone(&transport),
                    folio_core::net::cloud::aliyun::APP_ROOT,
                )
            }
        },
        Arc::new(SharedTokenSource::new(
            aliyun_token(aliyun),
            Arc::new(Pending {
                provider: Provider::Aliyun,
            }),
            Arc::new(AliyunRefresher),
            transport,
        )),
    )
}
