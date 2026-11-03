//! 局域网同步在 app 侧的装配：**账号密钥从配置里来**、本机书架那几件事、一趟后台往返。
//!
//! core 那两层（`folio_core::net::lansync` 的客户端编排、`net::transfer::lan` 的服务端路由）
//! 只管协议与合并，向本机要的东西一律走出口：服务端是 `LanProvider`（被对面读），客户端是
//! `LanLocal`（我们去读对面）。这一模块就是那两个出口的实现，加上两件装配的事：
//!
//! - **密钥**：`config.account.key` 那一串十六进制。判「这台有没有账号」用
//!   `Account::lan_ready`（它按能不能解开判，不按那一格空不空判），这里不再写第二份形状；
//! - **书库句柄与导入管线**：`app_store` 的那一份 `Arc<Store>`、`resident::pool()` 那一个作业池、
//!   书库目录在哪儿 —— 三样都是装配层的事，core 不该知道。
//!
//! **同步不在界面线程上跑**：`Transport` 是同步的，一次往返（握手 + 清单 + 拉几本书 + 两份快照）
//! 在局域网里也要几秒，而那几本书里可能有一本是漫画 PDF。与 WebDAV 那一条同一口径：活儿进
//! `background_spawn`，屏幕只读实体里的状态。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use folio_core::account::AccountKey;
use folio_core::net::lansync::{self, Applied, LanLocal, Report};
use folio_core::net::transfer::{LanConfig, LanProvider};
use folio_core::sync::{self, Snapshot};
use folio_core::{Imported, Store, TaskEvent, UrlSource, submit_import};
use gpui_kit::{App, AppContext as _, Context, Entity, Global};

use crate::ui::services::config::ConfigHolder;
use crate::ui::services::{app_store, import_queue, library, resident, sync as webdav, update};

/// 屏幕上那一格该念什么。`Done` 里带着这一趟的计数。
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Status {
    #[default]
    Idle,
    Syncing,
    Done(Report),
    Failed(Failure),
}

/// 失败只分到「读者能采取行动」的那几档；原文进日志，不上屏。
///
/// 直接借用 WebDAV 那一层的枚举：那五档在两条路里指的是同一件事（地址没填、认证没过、
/// 连不上、对面答得读不懂、本机库读写失败），写第二份只会让屏幕上两句话各自漂移。
pub use crate::ui::services::sync::Failure;

#[derive(Debug, Default)]
pub struct LanState {
    status: Status,
}

impl LanState {
    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn busy(&self) -> bool {
        matches!(self.status, Status::Syncing)
    }

    fn begin(&mut self, status: Status, cx: &mut Context<Self>) {
        self.status = status;
        cx.notify();
    }

    fn settle(&mut self, outcome: Result<Report, Failure>, cx: &mut Context<Self>) {
        self.status = match outcome {
            Ok(report) => Status::Done(report),
            Err(failure) => Status::Failed(failure),
        };
        cx.notify();
    }
}

pub struct LanHolder(pub Entity<LanState>);

impl Global for LanHolder {}

pub fn handle(cx: &App) -> Option<Entity<LanState>> {
    cx.try_global::<LanHolder>().map(|holder| holder.0.clone())
}

/// 取句柄，没有就建一个（与 [`super::sync::entity`] 同一条理由：设置页构造时手里就是
/// `&mut App`，不必等外壳先装）。
pub fn entity(cx: &mut App) -> Entity<LanState> {
    if let Some(entity) = handle(cx) {
        return entity;
    }
    let entity = cx.new(|_| LanState::default());
    cx.set_global(LanHolder(entity.clone()));
    entity
}

/// 这台设备此刻**能不能**走局域网同步：开着这一格，且手里有一把解得开的密钥。
///
/// 判据用 [`Account::lan_ready`]（按解不解得开判，不按那一格空不空判），不在这里再写一份形状。
pub fn key(cx: &App) -> Option<AccountKey> {
    let account = ConfigHolder::account(cx);
    account
        .lan_ready()
        .then(|| AccountKey::from_hex(&account.key))?
}

/// 那一格里存的密钥，**不看开关**。
///
/// 屏幕上「账号校验码」那一行与那颗「生成本机密钥」读的是它：读者按下生成之后就该当场看见
/// 自己的密钥与那两个码 —— 关着开关也得看见，否则那一步看着像没生效，而他接下来要抄到对面去
/// 的正是这一串。开关只管「这台肯不肯被读、会不会主动去连」，不管密钥本身存没存。
pub fn stored_key(cx: &App) -> Option<AccountKey> {
    AccountKey::from_hex(&ConfigHolder::account(cx).key)
}

/// 给读者核对的那 8 个字符：两台设备各摆一次，对一眼就知道中间没有第三台设备塞进这一段
/// 局域网。它是密钥的派生值，**不落盘**（存的份数越多，「密钥换了、校验码还是旧的」那种
/// 组合越可能出现）。
pub fn check_code(cx: &App) -> Option<String> {
    stored_key(cx).map(|key| key.check_code())
}

/// 那一格地址读不读得出来。判据与发起那一趟用的是**同一条**（`lansync::normalize_peer`），
/// 所以按钮灰掉的原因与真点下去以后报错的原因不会长成两件事。
pub fn peer_usable(peer: &str) -> bool {
    lansync::normalize_peer(peer).is_some()
}

/// 「生成本机密钥」：只在**这台还没有账号**时落一把新的，回那一串要抄到对面去的十六进制。
///
/// 已有密钥时什么都不做并回 `None`：重新生成等于把这一台从账号里摘出去（对面那台立刻不再
/// 认得它），而界面上那颗钮不该承诺一件说不出后果的事 —— 要加第二台，就在新机上填这一台
/// 已经有的那一串。
pub fn generate_key(cx: &mut App) -> Option<String> {
    if !ConfigHolder::account(cx).key.trim().is_empty() {
        return None;
    }
    let key = AccountKey::generate()?;
    let hex = key.to_hex();
    ConfigHolder::edit_account(cx, |account| account.key = hex.clone());
    Some(hex)
}

/// 传书服务器该不该带着局域网同步那五条路由起来。
///
/// `None` 就是那五条路由一概 404 —— 传书与局域网同步是两个决定：前者只收书，后者还要把本机
/// 书架与进度交出去，所以这一格由 `account.enabled` 说了算。但服务只有一台：**要能被对面
/// 同步到，得先把 Wi-Fi 传书开着**（这一句写在设置那一行的说明里）。
pub fn server_config(cx: &mut App) -> Option<LanConfig> {
    // 「开着且解得开」这一条与「立即同步」那颗钮用的是同一个出口（[`key`]）：
    // 屏幕上摆得出按钮的原因，和被同步的那一头肯答话的原因，必须是同一件事。
    let key = key(cx)?;
    let store = app_store::handle(cx)?;
    // 快照上的署名：局域网这一条不用它当文件名，但一份署着空串的快照在日志里没法认。
    // 与 WebDAV 共用同一个 id，因为「这台安装」是同一件事。id 不成形状时不开这几条路由 ——
    // 宁可这一台今天同步不了，也不把一份署名说不清的快照递出去（`ensure_device_id` 那一条）。
    let device_id = webdav::ensure_device_id(cx)?;
    Some(LanConfig {
        key,
        provider: Arc::new(Local {
            store,
            device_id,
            book_dir: None,
        }),
    })
}

/// 出发前要凑齐的那几样。
struct Departure {
    local: Arc<Local>,
    /// 已经清洗过的对端前缀（`scheme://host:port`）。
    base: String,
    key: AccountKey,
    /// 本机这一刻的 UTC 秒：对表与快照署名都用这同一个读数。
    now: i64,
}

/// 凑齐一趟要用的东西，缺哪样就报哪一档。
///
/// 「缺密钥 / 地址读不出来 / 书库位置没确认过」一律算 `Config`：读者能做的动作是回这一屏改
/// 那一格，而不是重试。这一句与按钮 `disabled` 用的是同一批判据（见 `pages::sync` 里那颗钮），
/// 所以灰掉的理由与真点下去报的理由不会长成两件事。
fn assemble(cx: &mut App) -> Result<Departure, Failure> {
    let store = app_store::handle(cx).ok_or(Failure::Store)?;
    let key = key(cx).ok_or(Failure::Config)?;
    // 地址先在这里判形状：填进来的那句读不出来是配置问题（该去核对一眼对面屏幕上摆的那个
    // 地址），不该等 `/hello` 发不出去才报「连不上」。
    let base = lansync::normalize_peer(&ConfigHolder::account(cx).peer).ok_or(Failure::Config)?;
    // 书库位置没确认过：拉进来的书没地方放，这一趟从一开始就完不成。
    let book_dir = library::target_dir(&ConfigHolder::library(cx)).ok_or(Failure::Config)?;
    let device_id = webdav::ensure_device_id(cx).ok_or(Failure::Config)?;
    Ok(Departure {
        local: Arc::new(Local {
            store,
            device_id,
            book_dir: Some(book_dir),
        }),
        base,
        key,
        now: folio_core::infra::time::now(),
    })
}

/// 「立即同步（局域网）」：与配置里那一台对端走一趟。已在飞时什么都不做（按钮同时禁用）。
pub fn sync_now(cx: &mut App) {
    let state = entity(cx);
    if state.read(cx).busy() {
        return;
    }
    let departure = match assemble(cx) {
        Ok(departure) => departure,
        Err(failure) => {
            state.update(cx, |state, cx| state.begin(Status::Failed(failure), cx));
            return;
        }
    };
    let Departure {
        local,
        base,
        key,
        now,
    } = departure;
    let transport = update::transport();

    state.update(cx, |state, cx| state.begin(Status::Syncing, cx));
    let work = cx.background_spawn(async move {
        lansync::sync_one(local.as_ref(), transport, &base, &key, now).map_err(failure_of)
    });
    let weak = state.downgrade();
    cx.spawn(async move |cx| {
        let outcome = work.await;
        if let Err(failure) = &outcome {
            tracing::warn!("局域网同步失败：{failure:?}");
        }
        let _ = weak.update(cx, |state, cx| state.settle(outcome, cx));
    })
    .detach();
}

/// core 那一层的错 → 屏幕上那五档。
///
/// 「那个地址上没有这几条路由」归 `Network` 而不是 `Remote`：读者的动作与连不上是同一个
/// —— 核对地址、走到那台设备跟前看一眼它的 Wi-Fi 传书开没开 —— 而 `Remote` 那一句说的是
/// 「对面答不上来」，会把人支去翻自己这边的设置。
fn failure_of(error: lansync::Error) -> Failure {
    match error {
        // 「不是同一个账号」与「证明没过」读者能做同一件事：核对两边的校验码。
        lansync::Error::AccountMismatch | lansync::Error::ProofRejected => Failure::Auth,
        lansync::Error::Transport(_) | lansync::Error::NoService => Failure::Network,
        lansync::Error::Denied
        | lansync::Error::Expired
        | lansync::Error::Locked
        | lansync::Error::Version { .. }
        | lansync::Error::BadReply(_) => Failure::Remote,
        lansync::Error::Local(_) => Failure::Store,
    }
}

/// 本机这一侧：core 那两个出口（`LanProvider` 被读、`LanLocal` 去读对面）的同一份实现。
///
/// 三个字段都是装配层的东西：共用的那一条库连接、这台安装的署名、书库目录（只有下拉那一趟
/// 用得到 —— 被读的那一侧不需要往库里放东西，所以服务端那份装配给 `None`）。
struct Local {
    store: Arc<Store>,
    device_id: String,
    book_dir: Option<PathBuf>,
}

impl Local {
    /// 把若干份**读回来的**快照合并进本机，并回合并后的那一份（要推回对面那台）。
    ///
    /// `remotes` 里的时间应当已经搬到本机钟面上：`/hello` 当场对表那一条在
    /// [`lansync::Peer::retime`]，`PUT` 那一条在 [`LanProvider::apply_snapshot`]。
    fn merge_into_local(&self, remotes: &[Snapshot], now: i64) -> Result<Applied, String> {
        let local = self
            .store
            .collect_sync(&self.device_id, now)
            .map_err(|err| format!("读本机快照失败：{err}"))?;
        let merged = sync::merge(&local.snapshot, remotes, now);
        self.store
            .apply_sync(&merged.plan)
            .map_err(|err| format!("写回本机失败：{err}"))?;
        Ok(Applied {
            json: sync::snapshot_to_json(&merged.snapshot).map_err(|err| err.to_string())?,
            positions: merged.stats.positions,
            annotations: merged.stats.annotations,
            bookmarks: merged.stats.bookmarks,
        })
    }
}

impl LanProvider for Local {
    fn manifest(&self) -> Vec<lansync::BookEntry> {
        // 只查库、不看盘：副本此刻还在不在由 `file` 现答，那一次是真的要开文件。
        match self.store.lan_manifest() {
            Ok(books) => books,
            Err(err) => {
                tracing::warn!("局域网同步：读本机书架清单失败：{err}");
                Vec::new()
            }
        }
    }

    fn file(&self, hash: &str) -> Option<PathBuf> {
        // 按内容身份认那一行，路径只是「副本躺在哪儿」—— 书库搬过家也不影响这一句。
        let row = self
            .store
            .book_by_hash(hash)
            .ok()
            .flatten()
            .filter(|row| row.size > 0)?;
        Some(PathBuf::from(row.path))
    }

    fn snapshot(&self) -> Result<String, String> {
        let now = folio_core::infra::time::now();
        let local = self
            .store
            .collect_sync(&self.device_id, now)
            .map_err(|err| format!("读本机快照失败：{err}"))?;
        sync::snapshot_to_json(&local.snapshot).map_err(|err| err.to_string())
    }

    /// 收下对面推过来的那一份并**就地合并进本机**。
    ///
    /// 对表这一刀必须在这里补：body 里那些时间是**对面那台的钟**写的，而这一侧没有 `/hello`
    /// 那一对当场读数的条件 —— 拿得到的只有快照自己带的 `exported_at`（导出方的读数）。
    /// 忘了搬，对面那台只要钟快一小时，它一小时前的改动就会盖掉本机刚写下的那一条。
    fn apply_snapshot(&self, json: &[u8]) -> Result<(), String> {
        let now = folio_core::infra::time::now();
        let incoming = sync::snapshot_from_json(json).map_err(|err| err.to_string())?;
        let remotes = [sync::retimed(&incoming, incoming.exported_at, now)];
        self.merge_into_local(&remotes, now).map(|_| ())
    }
}

impl LanLocal for Local {
    fn local_hashes(&self) -> HashSet<String> {
        self.store
            .lan_manifest()
            .unwrap_or_default()
            .into_iter()
            .map(|entry| entry.hash)
            .collect()
    }

    /// 把对面那一本拉进本机：走的仍是导入管线（判重、算哈希、落库、失败留档都在它那里），
    /// 而 URL 已经带着这次的会话 token，所以这条下载不需要第二个凭据。
    ///
    /// 这里**阻塞等它跑完**，两件事要求它：调用方已经在后台线程上；而「先有书、后合快照」那一条
    /// 顺序要求这一本已经落库 —— 对面那份快照里关于它的进度与批注才认得出它的 `content_hash`。
    ///
    /// 事件同时往自动导入那条桥上送一份（与监听、传书同一个出口）：拉来的书要出现在架面上，
    /// 而不是等读者下次重查。
    fn pull_book(&self, url: &str) -> bool {
        let Some(book_dir) = &self.book_dir else {
            tracing::error!("局域网同步：书库位置没确认过，拉进来的书没地方放");
            return false;
        };
        let source = UrlSource::new(url.to_string(), update::transport());
        let handle = submit_import(
            &resident::pool(),
            Box::new(source),
            book_dir.clone(),
            Arc::clone(&self.store),
        );
        let sender = import_queue::auto_import_sender();
        let mut landed = false;
        while let Ok(event) = handle.events.recv() {
            if let TaskEvent::Finished(Ok(outcomes)) = &event {
                for outcome in outcomes {
                    // `Duplicate` 也算成功：这一本已经在本机里了 —— 通常是它的 `content_hash`
                    // 还没懒补上，于是对账时报「缺」，而导入按内容认出了它。
                    landed |= matches!(
                        outcome.result,
                        Ok(Imported::Added(_)) | Ok(Imported::Duplicate { .. })
                    );
                }
            }
            // 接收端没了：没有要的人，这一条丢掉即可（后面的同样丢）。
            let _ = sender.send_blocking(event);
        }
        landed
    }

    fn merge_and_apply(&self, remotes: &[Snapshot], now: i64) -> Result<Applied, String> {
        self.merge_into_local(remotes, now)
    }
}

#[cfg(test)]
mod tests {
    use folio_core::{Config, Store};
    use gpui_kit::TestAppContext;

    use crate::ui::services::app_store;
    use crate::ui::services::config::ConfigHolder;
    use crate::ui::services::lansync::{
        Failure, assemble, check_code, generate_key, key, peer_usable, stored_key,
    };

    /// 一把解得开的密钥（32 个十六进制字符）。
    const HEX: &str = "0123456789abcdef0123456789abcdef";

    /// 「生成本机密钥」只在**这台还没有账号**时落一把新的：第二下什么都不做。
    ///
    /// 重新生成等于把这一台从账号里摘出去 —— 对面那台立刻不再认得它，而这一屏上没有第二个
    /// 地方能把旧的那一串找回来。按钮的消失与 `None` 是同一句实话的两个说法。
    #[gpui_kit::test]
    fn generating_a_key_happens_once_and_the_second_press_keeps_the_first(cx: &mut TestAppContext) {
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            assert!(key(cx).is_none(), "空配置就是没有账号");
            assert!(check_code(cx).is_none(), "没密钥时那一格说「还没有」");

            let first = generate_key(cx).expect("这台还没有账号，该落一把新的");
            assert_eq!(first.len(), 32, "那一串要能原样抄到对面去");
            assert_eq!(first, ConfigHolder::account(cx).key, "生成与落配置是同一步");

            // 开关还关着：这一台**有**密钥（屏幕上当场看得见、抄得走），但**不参加**同步。
            assert!(stored_key(cx).is_some());
            assert!(
                check_code(cx).is_some(),
                "按了生成就该当场看见校验码，否则像没生效"
            );
            assert!(key(cx).is_none(), "没打开那一格就不出门，也不被人读");

            assert!(
                generate_key(cx).is_none(),
                "已经有密钥时第二下不许换一把 —— 那会把这一台从账号里摘出去"
            );
            assert_eq!(ConfigHolder::account(cx).key, first);
        });
    }

    /// 「能不能同步」按**开着且解得开**判；「这台有没有密钥」只按解不解得开判。
    /// 两件事分不开的话，关掉开关就会让屏幕上那串要抄走的东西一起消失。
    #[gpui_kit::test]
    fn the_switch_decides_participation_while_the_key_stays(cx: &mut TestAppContext) {
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            ConfigHolder::edit_account(cx, |account| {
                account.key = HEX.into();
                account.enabled = true;
            });
            assert!(key(cx).is_some());
            let code = check_code(cx).expect("有密钥就该说得出校验码");
            assert_eq!(code.chars().count(), 8, "屏幕上摆的是 8 个字符那一串");

            ConfigHolder::edit_account(cx, |account| account.enabled = false);
            assert!(key(cx).is_none(), "关掉这一格：这台既不被读，也不主动去连");
            assert!(
                check_code(cx).is_some(),
                "关着也要摆得出那两个码与那一串密钥：抄到对面去靠的就是它"
            );
            assert_eq!(
                ConfigHolder::account(cx).key,
                HEX,
                "关掉不抹密钥 —— 重新打开还是同一个账号"
            );
        });
    }

    /// 从屏幕上抄过来的一串可能带首尾空白（复制时多带一个空格是常态）。
    /// 判据与 `AccountKey::from_hex` 同一条：trim 得开就算有账号，别让读者重抄一遍。
    #[gpui_kit::test]
    fn a_key_copied_with_spaces_around_it_is_still_a_key(cx: &mut TestAppContext) {
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            ConfigHolder::edit_account(cx, |account| {
                account.key = format!("  {HEX} ");
                account.enabled = true;
            });
            assert!(
                key(cx).is_some(),
                "那一串解得开就是有一把密钥，不要求读者先手工删掉空格"
            );

            // 而少一位是真的解不开：那种情况下屏幕上说的是「还没有账号」。
            ConfigHolder::edit_account(cx, |account| account.key = HEX[..31].into());
            assert!(key(cx).is_none(), "31 个字符不是那把 128-bit 密钥");
            assert!(check_code(cx).is_none());
        });
    }

    /// 那一格地址读不读得出来。判据与发起那一趟用的是**同一条**
    /// （`lansync::normalize_peer`），所以按钮灰掉的原因与真点下去以后报错的原因不会长成两件事。
    #[gpui_kit::test]
    fn an_address_is_usable_by_the_same_rule_that_reads_it(cx: &mut TestAppContext) {
        assert!(peer_usable("192.168.1.5:8410"), "屏幕上摆的就是这个形状");
        assert!(peer_usable("http://192.168.1.5:8410/"));
        assert!(
            !peer_usable("192.168.1.5:8410/?pin=12345678"),
            "那是上传页的地址，那五条路由不在它底下"
        );
        assert!(!peer_usable(""), "空着就是没填");
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            // 没填地址时，即便有密钥也出不了门。
            ConfigHolder::edit_account(cx, |account| {
                account.key = HEX.into();
                account.enabled = true;
                account.peer = String::new();
            });
            assert!(key(cx).is_some());
            assert!(!peer_usable(&ConfigHolder::account(cx).peer));
        });
    }

    /// 出发那一趟报的是**缺了哪一样**：书库没装 ≠ 配置没填，屏幕上那是两句话、两个动作。
    ///
    /// 四样凑齐时顺带钉两件事：地址被清洗成 `scheme://host:port`，而这台安装的署名（设备 id）
    /// 在出发前就落好了 —— 局域网这一条不用它当文件名，但一份署着空串的快照在日志里没法认。
    #[gpui_kit::test]
    fn a_departure_names_the_piece_that_is_missing(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            assert_eq!(
                assemble(cx).err(),
                Some(Failure::Store),
                "书库没装：读者该看到的是本机读不到库，而不是「先去填那一格」"
            );

            app_store::install(Store::in_memory().unwrap(), cx);
            assert_eq!(
                assemble(cx).err(),
                Some(Failure::Config),
                "没有密钥就是配置没填"
            );

            ConfigHolder::edit_account(cx, |account| {
                account.enabled = true;
                account.key = HEX.into();
            });
            assert_eq!(
                assemble(cx).err(),
                Some(Failure::Config),
                "对面那台的地址还没填"
            );

            ConfigHolder::edit_account(cx, |account| account.peer = "192.168.1.5:8410".into());
            assert_eq!(
                assemble(cx).err(),
                Some(Failure::Config),
                "书库位置还没确认过"
            );

            ConfigHolder::edit_library(cx, |library| library.dir = "/tmp/folio-测试书库".into());
            let departure = assemble(cx).expect("四样凑齐就该走得出去");
            assert_eq!(departure.base, "http://192.168.1.5:8410");
            assert_eq!(
                ConfigHolder::sync(cx).device_id.chars().count(),
                32,
                "出发前把这台安装的署名生成并落下"
            );
        });
    }
}
