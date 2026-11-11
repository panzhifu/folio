//! Wi-Fi 传书：设备起一个 HTTP 服务器，同一局域网的浏览器打开网页直接上传书。
//!
//! **tokio 只住在这个模块里。** 传书服务器是 core 里唯一的长驻并发服务：几十个
//! 上传连接各自大部分时间在等网络，加上浏览器上传格式（multipart）的流式解析
//! 是安全洞高发区，这两件事都站在现成的 axum/multer 上最稳 —— 而它们站在 tokio
//! 上。于是这里的边界是：Runtime 自建、跑在自己的线程上、对外只交**同步** API；
//! core 的其余部分（管线、任务、存储）照旧是 std 形状，公共签名不漏 `Future`。
//! 「core 除 transfer 外不依赖异步运行时」是模块纪律，将来嫌软可以拆成独立 crate。
//!
//! 流程：浏览器开 `http://<设备IP>:<port>/` → 单页（拖拽 / 多选，XHR
//! 逐文件）→ multipart **流式**落进 `inbox_dir`（块进块写，不进内存）→ 每个文件
//! 完成即 `on_file(path)` → app 侧把路径包成 `ingestcrate::ingest::import::PathsSource` 提交
//! 导入队列 → 导入成功后清掉收件箱里的原件，失败保留待查。
//!
//! 安全：**不设配对码** —— 同一 Wi-Fi 里能连上这个端口的设备就能传书，所以这台服务
//! 只该在读者亲手打开的那段时间开着（启停在 app 侧，不写进配置、重启不自动开）。
//! 按对端地址记的那本失败账仍留着，但只服务下面的局域网同步握手；传书侧只在那个
//! 地址已经进了罚期时挡一下（429 + `Retry-After`）。
//!
//! 同一台服务器还挂着**局域网同步**的另一半（同账号的两台设备互拉缺的书与进度），
//! 它住在 [`lan`] 里：那条路要的是「读」，而它的门是账号证明 —— id 在局域网里
//! 是广播出去的、抄得来的，所以「是不是同一个账号」必须由那对 nonce 上的双向证明来回答。
//! 两组路由共用一个端口、共用一本按地址记的失败账，读者在设置页上看到的仍是同一个地址。

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt as _;

use axum::Router;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Multipart, State};
use axum::http::StatusCode;
use axum::http::header;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};

use folio_base::paths;

mod lan;

pub use lan::{LanConfig, LanProvider};

/// 传书本地这半的错误：**带标签的 I/O**。原来住在 core 的伞错误里（`Error::Io`），
/// 随阶段三搬迁自持 —— 监听端口的绑定失败、收件目录建不出来、服务线程起不来，
/// 读者看到的每一句都该说得出是哪一格出的事，标签就是那个格子（文件路径，
/// 或 `0.0.0.0:8080` 这类套接字地址）。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("读 {path} 失败：{source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
}

impl Error {
    /// 一条 I/O 错误套上出事的格子。原始错误留在 `source` 链上。
    fn io_at(path: &std::path::Path, err: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source: Box::new(err),
        }
    }
}

/// 传书这半的 `Result`。
pub type Result<T> = std::result::Result<T, Error>;

/// 上传页。内嵌而非资产：这页只有一个用途，跟着二进制走，部署永远不缺它。
const UPLOAD_PAGE: &str = include_str!("transfer_page.html");

/// 单个文件的上传上限。1 GiB 是「没人真的传比这更大的书」的界，挡住的是
/// 恶意或失控的上传把磁盘灌满，不是正常读者。
const MAX_UPLOAD: usize = 1024 * 1024 * 1024;

/// 一个地址连错几次以内不罚锁。前几次是手滑输错，罚它等待等于惩罚读者。
const FREE_MISTAKES: u32 = 5;

/// 进锁之后的第一档罚多久，往后每错一次翻倍。
const FIRST_LOCK: Duration = Duration::from_secs(1);

/// 罚锁的上限。再多不锁 —— 这服务是读者自己开的，把他自己的地址永久锁死不是
/// 防住了谁，只是让他重开一次服务。
const MAX_LOCK: Duration = Duration::from_secs(15 * 60);

/// 记账的地址数上限：账本不能跟着扫描器的源地址数一起长。
const MAX_TRACKED: usize = 256;

/// 多久没再出错过的那个地址，从账上抹掉。
const STALE: Duration = Duration::from_secs(60 * 60);

/// 传书服务的配置。同一 Wi-Fi 内不设口令：浏览器打开地址就能传书（见模块头那条
/// 安全说明），因此只该在读者亲手打开的那段时间开着。
#[derive(Debug, Clone)]
pub struct TransferConfig {
    /// 期望的端口。被占就顺延（最多试 10 个），真实的以 [`TransferServer::addr`] 为准。
    pub port: u16,
    /// 收件箱：上传的字节先落这里。
    pub inbox_dir: PathBuf,
    /// 局域网同步开不开。**`None` 就是那五条路由一概 404** —— 传书与局域网同步是两个决定，
    /// 前者只收书、后者还要把本机书架与进度交出去。
    pub lan: Option<LanConfig>,
}

/// 一条活着的传书服务。`stop()` 关停（拒绝新连接、收完手上那批）；`Drop` 兜底。
pub struct TransferServer {
    addr: SocketAddr,
    notify: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl TransferServer {
    /// 起服务。占着调用线程的只有「绑定端口」这一次系统调用；之后整个服务器
    /// 活在返回值背后自己那条线程上。
    pub fn start(
        config: TransferConfig,
        on_file: Arc<dyn Fn(PathBuf) + Send + Sync>,
    ) -> Result<Self> {
        // 端口在同步侧定下来：占用顺延，真实的地址从 listener 上读，不靠猜。
        let mut listener = None;
        let mut last_err = None;
        for port in config.port..config.port + 10 {
            match TcpListener::bind(("0.0.0.0", port)) {
                Ok(l) => {
                    listener = Some(l);
                    break;
                }
                Err(err) => last_err = Some(err),
            }
        }
        let listener = listener.ok_or_else(|| Error::Io {
            path: PathBuf::from(format!("0.0.0.0:{}", config.port)),
            source: format!(
                "端口全被占：{}",
                last_err.map(|e| e.to_string()).unwrap_or_default()
            )
            .into(),
        })?;
        let addr = listener
            .local_addr()
            .map_err(|e| Error::io_at(Path::new("0.0.0.0"), e))?;
        std::fs::create_dir_all(&config.inbox_dir)
            .map_err(|e| Error::io_at(&config.inbox_dir, e))?;

        let (notify, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let inbox = config.inbox_dir.clone();
        let lan = config.lan.clone();
        let thread = std::thread::Builder::new()
            .name("folio-transfer".to_string())
            .spawn(move || serve(listener, inbox, on_file, lan, shutdown_rx))
            .map_err(|err| Error::io_at(Path::new("folio-transfer"), err))?;
        tracing::info!("transfer: 传书服务起在 {addr}");
        // 顺手把探到的局域网地址记一行：真机上「地址点不开」最常见的原因就是这一格
        // 空着（或者报错了网卡），日志里有它，下一次不必再靠猜。
        tracing::info!("transfer: 局域网地址 {:?}", lan_address());
        Ok(Self {
            addr,
            notify: Some(notify),
            thread: Some(thread),
        })
    }

    /// 实际监听的地址（端口可能与请求的不同 —— 被占就顺延过）。
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// 局域网里能打开这个服务的 URL；一个都探不到就空 —— 界面只好提示读者自己输
    /// 设备的 IP。**只报一条**（Wi-Fi / 有线那条）：多网卡时其余通常是蜂窝或虚拟
    /// 网桥，摆出来读者点到的是打不开的链接。不带查询参数：扫二维码或手输都省一步。
    pub fn urls(&self) -> Vec<String> {
        lan_address()
            .into_iter()
            .map(|ip| format!("http://{ip}:{}/", self.addr.port()))
            .collect()
    }

    /// 关停：拒绝新连接，正在收的那份收完。阻塞到线程退出。
    pub fn stop(mut self) {
        if let Some(notify) = self.notify.take() {
            let _ = notify.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        tracing::info!("transfer: 传书服务已停");
    }
}

impl Drop for TransferServer {
    fn drop(&mut self) {
        if let Some(notify) = self.notify.take() {
            let _ = notify.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(
    listener: TcpListener,
    inbox_dir: PathBuf,
    on_file: Arc<dyn Fn(PathBuf) + Send + Sync>,
    lan: Option<LanConfig>,
    notify: tokio::sync::oneshot::Receiver<()>,
) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("传书服务的 runtime 不该建不起来");
    // 那一 GiB 的请求体上限挂在**上传这一组**路由上，不挂整台服务器：局域网同步的快照只该
    // 收它自己那一个上限，两组共用一个等于把小的那组放大到大的那组那么大。
    let upload_routes = Router::new()
        .route("/", get(upload_page))
        .route("/upload", post(upload))
        .layer(DefaultBodyLimit::max(MAX_UPLOAD));
    let app = Router::new()
        .merge(upload_routes)
        .merge(lan::routes())
        .with_state(Arc::new(TransferState {
            inbox_dir,
            guard: PinGuard::default(),
            on_file,
            // 开局域网同步就在这一刻把会话表建起来；没开就是 `None`，那五条路由一概 404。
            lan: lan.map(|config| Arc::new(lan::LanState::new(config))),
        }));
    runtime.block_on(async move {
        // tokio 只收非阻塞的 fd：迁进去之前翻成非阻塞（accept/read 由 runtime 管）。
        listener
            .set_nonblocking(true)
            .expect("listener 翻非阻塞不该失败");
        let listener =
            tokio::net::TcpListener::from_std(listener).expect("listener 迁进 tokio 不该失败");
        // 限速按**对端地址**记，所以要把 connect info 接进服务里 —— 没有这一层，
        // 处理器只拿得到 URL，而局域网同步握手判罚期要用的正是对端地址。
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let _ = notify.await;
        })
        .await;
    });
}

struct TransferState {
    inbox_dir: PathBuf,
    guard: PinGuard,
    on_file: Arc<dyn Fn(PathBuf) + Send + Sync>,
    /// 局域网同步那一半的运行状态。`None` = 没开，那五条路由一概 404。
    ///
    /// 与 `guard` 同一本账：握手反复出不了证明的设备由那一侧记账，传书侧只在同一
    /// 地址已经进了罚期时跟着挡一下。
    lan: Option<Arc<lan::LanState>>,
}

/// 一个地址的配对码账：**连错几个**加**上一次错是多久以前**。
#[derive(Debug)]
struct Account {
    wrong: u32,
    last: Instant,
}

/// 配对码的失败计数与指数退避，按对端地址记账。
///
/// 为什么需要它：服务绑的是 `0.0.0.0`，同一局域网里任何一台设备都能连，而界面上那个
/// 码是**明晃晃摆在设置页里**的。限速之前爆破的代价只是发包，之后是等 —— 八位码配上
/// 每错一次翻倍，试完全空间要按年算。
#[derive(Default)]
struct PinGuard {
    accounts: Mutex<HashMap<IpAddr, Account>>,
}

/// [`PinGuard::gate`] 的两种结果。
enum Gate {
    /// 可以去比配对码。
    Proceed,
    /// 还在罚，`Duration` 是还要等多久（进 `Retry-After`）。
    Locked(Duration),
}

impl PinGuard {
    /// 这一趟能不能验配对码。
    ///
    /// 锁着的时候**连对的码也挡**：放行对的码等于给爆破者一个「这条通了」的信号，
    /// 而限速要买的正是这个信息量。罚期最长 [`MAX_LOCK`]，读者自己被锁掉只需等一等，
    /// 或者把服务关掉重开。
    fn gate(&self, from: IpAddr, now: Instant) -> Gate {
        let accounts = self.locked();
        let Some(account) = accounts.get(&from) else {
            return Gate::Proceed;
        };
        match penalty(account.wrong) {
            // `last + 罚期` 在未来才叫还锁着；`checked_add` 挡住的是测试里注入的钟。
            Some(penalty)
                if account
                    .last
                    .checked_add(penalty)
                    .is_some_and(|until| until > now) =>
            {
                let until = account.last + penalty;
                Gate::Locked(until.saturating_duration_since(now))
            }
            _ => Gate::Proceed,
        }
    }

    /// 记一次不对的配对码。
    fn note_wrong(&self, from: IpAddr, now: Instant) {
        let mut accounts = self.locked();
        // 账本不能跟着扫描器的源地址数一起长：先清久没动静的，还满就淘汰错得最少的那格
        // —— 留着的是最该被记住的嫌疑犯。淘汰别人只是让那个地址重新拿到免费额度，
        // 而 256 格以下的正常读者永远排不进被淘汰的那一批。
        if accounts.len() >= MAX_TRACKED && !accounts.contains_key(&from) {
            accounts.retain(|_, account| {
                now.checked_duration_since(account.last)
                    .is_none_or(|idle| idle < STALE)
            });
            if accounts.len() >= MAX_TRACKED {
                let weakest = accounts
                    .iter()
                    .min_by_key(|(_, account)| account.wrong)
                    .map(|(ip, _)| *ip);
                if let Some(ip) = weakest {
                    accounts.remove(&ip);
                }
            }
        }
        match accounts.get_mut(&from) {
            Some(account) => {
                account.wrong = account.wrong.saturating_add(1);
                account.last = now;
            }
            None => {
                accounts.insert(
                    from,
                    Account {
                        wrong: 1,
                        last: now,
                    },
                );
            }
        }
    }

    /// 对了就把这个地址从账上划掉：限速记的是**连着**错，中间成功一次就该重新数。
    fn note_right(&self, from: IpAddr) {
        self.locked().remove(&from);
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, HashMap<IpAddr, Account>> {
        self.accounts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// 连错 `wrong` 次要罚多久；`FREE_MISTAKES` 次以内不罚，之后每多错一次翻倍，封顶
/// [`MAX_LOCK`]。
fn penalty(wrong: u32) -> Option<Duration> {
    if wrong <= FREE_MISTAKES {
        return None;
    }
    // 第一次进罚是第一档，所以指数从 0 起；移位超了就直接取上限。
    let steps = (wrong - FREE_MISTAKES - 1).min(20);
    Some(
        FIRST_LOCK
            .checked_mul(1u32 << steps)
            .unwrap_or(MAX_LOCK)
            .min(MAX_LOCK),
    )
}

/// 罚期一起判：局域网同步那本账（`PinGuard`）锁着的地址，传书这边也别放进来。
/// 两个入口（页面与上传）共用这一份，免得哪一侧漏挂。
fn authorize(state: &TransferState, from: IpAddr) -> std::result::Result<(), Refusal> {
    match state.guard.gate(from, Instant::now()) {
        Gate::Locked(remaining) => Err(Refusal::Locked(remaining)),
        Gate::Proceed => Ok(()),
    }
}

/// 唯一的拒绝：这个地址还在罚期里。
enum Refusal {
    Locked(Duration),
}

impl Refusal {
    fn into_response(self) -> Response {
        match self {
            // 秒数向上取整，且至少一秒：报 0 等于叫客户端立刻再打一趟。
            Self::Locked(remaining) => (
                StatusCode::TOO_MANY_REQUESTS,
                [(
                    header::RETRY_AFTER,
                    remaining.as_millis().div_ceil(1_000).max(1).to_string(),
                )],
            )
                .into_response(),
        }
    }
}

/// 拒绝一个上传之前最多读掉多少请求体。
///
/// 读掉是为了让服务端 `close()` 时接收队列是空的 —— 队列里还压着没读的字节时，
/// 内核回的是 RST 而不是 FIN，客户端那边还在写，于是它看到的是 `broken pipe`
/// 而不是那句 429（测试里偶发的红就是它）。上限是给「拿着 1 GB 文件撞进罚期」兜底的：
/// 超过它就随连接一起丢掉，那种量级本来也不该为了一句 429 再读一遍。
const REFUSAL_DRAIN: u64 = 8 * 1024 * 1024;

/// 把请求体读掉（最多 `limit` 字节），只为让连接能干净地关。
///
/// 读失败（身体是坏的）就停 —— 反正接下来要回的是拒绝。
async fn drain_at_most(multipart: &mut Multipart, limit: u64) {
    let mut left = limit;
    loop {
        let Ok(Some(mut field)) = multipart.next_field().await else {
            return;
        };
        // 读失败（身体坏了）就停 —— 反正接下来要回的是拒绝。
        while let Ok(Some(chunk)) = field.chunk().await {
            let len = chunk.len() as u64;
            if len >= left {
                return;
            }
            left -= len;
        }
    }
}

/// 上传页：这个地址没在罚期里就给。
async fn upload_page(
    State(state): State<Arc<TransferState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Response {
    match authorize(&state, peer.ip()) {
        Ok(()) => Html(UPLOAD_PAGE).into_response(),
        Err(refusal) => refusal.into_response(),
    }
}

/// 收一个 multipart 上传：每个文件字段流式落进收件箱，落完回调 `on_file`。
///
/// 两个返回类型都是 `Response` 而不是 `Result<_, Response>`：`Response` 128 字节起，
/// 拿它当 `Err` 侧会招来 clippy::result_large_err，而这里的失败本来就只是
/// 「几号状态 + 一句话」。
async fn upload(
    State(state): State<Arc<TransferState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    mut multipart: Multipart,
) -> Response {
    if let Err(refusal) = authorize(&state, peer.ip()) {
        // 拒绝之前先把请求体读掉（见 [`REFUSAL_DRAIN`]）：不读就答，连接会被 RST 掉，
        // 客户端拿不到这句 429、只看到 broken pipe。
        drain_at_most(&mut multipart, REFUSAL_DRAIN).await;
        return refusal.into_response();
    }
    match collect_upload(&state, &mut multipart).await {
        Ok(saved) => {
            for path in saved {
                (state.on_file)(path);
            }
            StatusCode::OK.into_response()
        }
        Err((code, why)) => (code, why).into_response(),
    }
}

/// 把这一个 multipart 请求里的字段流式落进收件箱，回给调用方收下的路径。
/// 一个字段失败就整个请求报错 —— 浏览器那侧是一文件一请求，颗粒度对得上。
async fn collect_upload(
    state: &TransferState,
    multipart: &mut Multipart,
) -> std::result::Result<Vec<PathBuf>, (StatusCode, String)> {
    let mut saved = Vec::new();
    loop {
        let Some(mut field) = multipart
            .next_field()
            .await
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?
        else {
            break;
        };
        // 名字先定下来才开文件：没有名字的字段不是上传（页面上传的都是 file 字段）。
        let Some(name) = field.file_name().and_then(sanitize_file_name) else {
            continue;
        };
        let dest = state.inbox_dir.join(&name);
        let mut file = tokio::fs::File::create(&dest)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let mut received = 0u64;
        loop {
            let Some(chunk) = field
                .chunk()
                .await
                .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?
            else {
                break;
            };
            received += chunk.len() as u64;
            file.write_all(&chunk)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }
        file.flush()
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        drop(file);
        if received == 0 {
            // 空字段（浏览器偶尔会带）不进收件箱、不回调。
            let _ = std::fs::remove_file(&dest);
            continue;
        }
        tracing::info!("transfer: 收下 {name}（{received} 字节）");
        saved.push(dest);
    }
    Ok(saved)
}

/// 浏览器给的文件名 → 收件箱里的安全文件名：剥掉任何路径成分、换掉控制字符
/// 与分隔符，空了就给时间戳名。**路径穿越在这一步被掐死** —— `../../x` 到不了
/// 收件箱外面。
fn sanitize_file_name(raw: &str) -> Option<String> {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let cleaned: String = last
        .chars()
        .map(|c| {
            if paths::is_unsafe_name_char(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').to_string();
    if trimmed.is_empty() {
        Some(format!("upload-{}.bin", now_secs()))
    } else {
        Some(trimmed)
    }
}

/// 探测局域网里能访问本机的那个地址。
///
/// 两条腿都要：
///
/// - **网卡枚举**：Android 上 Wi-Fi 与蜂窝同时在线时，默认网络可能是蜂窝 —— 只靠
///   「connect 一个公网地址看走哪张卡」探到的是运营商的 `10.x`，同一 Wi-Fi 里的设备
///   根本连不上。所以枚举每张网卡的 IPv4，**Wi-Fi / 以太网排最前**，其它垫后。
/// - **路由出口**：UDP connect 到公共 DNS（不发包，只让内核选路由出口）。桌面与
///   单网卡机器上这一条最准，作为兜底。
///
/// **只交最优的那一条**：多网卡时其余通常是蜂窝 / 虚拟网桥，报出去只会让读者点到
/// 一个打不开的链接（真机上第二行那条就是这么坏的）。一个都探不到就交 `None` ——
/// 界面据此提示「读者自己输 IP」。
fn lan_address() -> Option<String> {
    // (优先级, 地址)。0 = 有线 / 无线网卡，1 = 其它网卡，2 = 路由探到的出口。
    let mut ranked: Vec<(u8, String)> = Vec::new();

    for (name, ip) in interface_ipv4_addrs() {
        ranked.push((rank_of(&name), ip));
    }

    for probe in ["223.5.5.5:80", "8.8.8.8:80"] {
        // socket 的族要跟目标一致：AF_INET6 的 socket 去 connect IPv4 地址，Linux /
        // Android 直接回 `EAFNOSUPPORT`（dual-stack 只对 v4-mapped 地址成立，而
        // `ToSocketAddrs` 把 "223.5.5.5:80" 解析成的是裸 v4）。
        let Ok(socket) = UdpSocket::bind("0.0.0.0:0") else {
            continue;
        };
        if socket.connect(probe).is_ok()
            && let Ok(local) = socket.local_addr()
        {
            let ip = local.ip().to_string();
            // 没有默认路由时 `local_addr` 可能还是全零：那不是能打开的地址。
            if ip != "0.0.0.0" {
                ranked.push((2, ip));
            }
        }
    }

    // 稳定排序：同优先级里保持枚举顺序。
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked
        .into_iter()
        .map(|(_, ip)| ip)
        .find(|ip| !ip.is_empty())
}

/// 网卡名的优先级：有线 / 无线在前，蜂窝与虚拟网卡垫后。
fn rank_of(name: &str) -> u8 {
    const LAN: &[&str] = &["wlan", "wl", "eth", "en", "usb", "rndis", "ap"];
    const WAN: &[&str] = &["rmnet", "ccmni", "pdp", "tun", "ppp", "clat"];
    if LAN.iter().any(|prefix| name.starts_with(prefix)) {
        0
    } else if WAN.iter().any(|prefix| name.starts_with(prefix)) {
        2
    } else {
        1
    }
}

/// 本机所有可用的 IPv4 地址 `(接口名, 点分地址)`：环回、全零、链路本地都不要。
///
/// 用 `getifaddrs` 而不是「connect 一个公网地址看走哪张卡」：后者在 Android 的
/// 双网卡下答的是「默认网络是谁」，而读者要的是「同一个 Wi-Fi 里该连哪个 IP」。
#[cfg(unix)]
fn interface_ipv4_addrs() -> Vec<(String, String)> {
    use std::ffi::CStr;

    let mut out = Vec::new();
    unsafe {
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut head) != 0 {
            return out;
        }
        let mut cur = head;
        while !cur.is_null() {
            let item = &*cur;
            if !item.ifa_addr.is_null() && (*item.ifa_addr).sa_family as i32 == libc::AF_INET {
                let addr = &*(item.ifa_addr as *const libc::sockaddr_in);
                let ip = std::net::Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr));
                if !ip.is_loopback() && !ip.is_unspecified() && !ip.is_link_local() {
                    let name = CStr::from_ptr(item.ifa_name).to_string_lossy().into_owned();
                    out.push((name, ip.to_string()));
                }
            }
            cur = item.ifa_next;
        }
        libc::freeifaddrs(head);
    }
    out
}

/// Windows 没有 `getifaddrs`：只留路由出口那一条（桌面单网卡够用）。
#[cfg(not(unix))]
fn interface_ipv4_addrs() -> Vec<(String, String)> {
    Vec::new()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use super::{
        FIRST_LOCK, FREE_MISTAKES, Gate, MAX_LOCK, MAX_TRACKED, PinGuard, Refusal, TransferConfig,
        TransferServer, TransferState, authorize, penalty, sanitize_file_name,
    };

    /// 路径穿越与花式名字到不了收件箱外面；正经名字原样保留。
    #[test]
    fn uploaded_names_cannot_escape_the_inbox() {
        assert_eq!(
            sanitize_file_name("../../etc/passwd").as_deref(),
            Some("passwd"),
            "路径成分整个剥掉"
        );
        assert_eq!(
            sanitize_file_name("C:\\Users\\me\\三体.epub").as_deref(),
            Some("三体.epub")
        );
        let dots = sanitize_file_name("..").unwrap();
        assert!(!dots.contains(".."), "「..」到不了任何地方，实际 {dots:?}");
        assert_eq!(
            sanitize_file_name("三体.epub").as_deref(),
            Some("三体.epub")
        );
    }

    /// 端到端：`port 0` 起服务，ureq 手拼 multipart 上传 —— 不带任何配对码就能
    /// 收件箱落盘、回调收到、页面能打开。
    #[test]
    fn the_server_receives_uploads_without_a_pin() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox = tmp.path().join("inbox");
        let received: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
        let got = Arc::clone(&received);
        let server = TransferServer::start(
            TransferConfig {
                port: 0,
                inbox_dir: inbox.clone(),
                lan: None,
            },
            Arc::new(move |path| got.lock().unwrap().push(path)),
        )
        .unwrap();
        let base = format!("http://{}", server.addr());

        let client = ureq::AgentBuilder::new().build();
        let boundary = "----foliotestboundary";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"三体.epub\"\r\nContent-Type: application/octet-stream\r\n\r\n{}\r\n--{boundary}--\r\n",
            "这是假装的 epub 内容"
        );
        // 闭包只回状态码。`ureq::Error::Status` 里揣着整个 `Response`，照原样把它当 `Err`
        // 交回来就是 clippy::result_large_err（那一格的 `Ok` 侧本来也是拿来数状态码的）；
        // 而这条测试从头到尾要的只是「几号状态」，所以把两条路折成一个 `u16`。
        let status = match client
            .post(&format!("{base}/upload"))
            .set(
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            )
            .send(body.as_bytes())
        {
            Ok(resp) => resp.status(),
            Err(ureq::Error::Status(code, _)) => code,
            Err(other) => panic!("上传不该走到传输错误：{other}"),
        };
        assert_eq!(status, 200, "不带配对码也该收得下");
        assert_eq!(
            std::fs::read(inbox.join("三体.epub")).unwrap(),
            "这是假装的 epub 内容".as_bytes(),
            "上传的内容要原样落进收件箱"
        );
        assert_eq!(
            received.lock().unwrap().len(),
            1,
            "on_file 每个文件回调一次"
        );

        // 页面也能直接打开。
        assert!(
            client
                .get(&format!("{base}/"))
                .call()
                .unwrap()
                .into_string()
                .unwrap()
                .contains("input"),
            "上传页是一张带文件选择的表单"
        );
        server.stop();
    }

    /// 罚锁的阶梯：免费额度内不罚，进锁后每多错一次翻倍，封顶一档。
    #[test]
    fn the_penalty_doubles_and_then_caps() {
        assert_eq!(penalty(0), None);
        assert_eq!(penalty(FREE_MISTAKES), None, "手滑输错的那几次不该等待");
        assert_eq!(penalty(FREE_MISTAKES + 1), Some(FIRST_LOCK));
        assert_eq!(penalty(FREE_MISTAKES + 2), Some(FIRST_LOCK * 2));
        assert_eq!(penalty(FREE_MISTAKES + 3), Some(FIRST_LOCK * 4));
        assert_eq!(penalty(FREE_MISTAKES + 40), Some(MAX_LOCK), "再多也到顶");
        assert_eq!(penalty(u32::MAX), Some(MAX_LOCK), "计数溢出也不能panic");
    }

    /// 锁按注入的钟判：罚期内一律挡，一过就放行；放行那趟再错一次，罚期翻倍。
    #[test]
    fn a_locked_address_waits_and_then_gets_its_turn_again() {
        let guard = PinGuard::default();
        let from: IpAddr = "10.0.0.1".parse().unwrap();
        let t0 = Instant::now();
        for n in 1..=FREE_MISTAKES + 1 {
            assert!(
                matches!(guard.gate(from, t0), Gate::Proceed),
                "第 {n} 趟还该放行（罚期是从第 {n} 趟失败之后才装上的）"
            );
            guard.note_wrong(from, t0);
        }
        assert!(
            matches!(
                guard.gate(from, t0 + FIRST_LOCK - Duration::from_millis(1)),
                Gate::Locked(_)
            ),
            "差一毫秒还在锁里"
        );
        assert!(
            matches!(
                guard.gate(from, t0 + FIRST_LOCK + Duration::from_millis(1)),
                Gate::Proceed
            ),
            "罚期一过要放行"
        );
        let again = t0 + FIRST_LOCK + Duration::from_millis(1);
        guard.note_wrong(from, again);
        assert!(
            matches!(
                guard.gate(from, again + FIRST_LOCK * 2 - Duration::from_millis(1)),
                Gate::Locked(_)
            ),
            "第二档要两秒，不是还是一秒"
        );
    }

    /// 中间成功一次就把账清掉：限速数的是**连着**错。
    #[test]
    fn one_right_code_clears_the_debt() {
        let guard = PinGuard::default();
        let from: IpAddr = "10.0.0.2".parse().unwrap();
        let t0 = Instant::now();
        for _ in 0..FREE_MISTAKES {
            guard.note_wrong(from, t0);
        }
        guard.note_right(from);
        // 清零之后又要一整轮才装得上锁。
        for n in 1..=FREE_MISTAKES + 1 {
            assert!(
                matches!(guard.gate(from, t0), Gate::Proceed),
                "第 {n} 趟该放行：上一趟成功过"
            );
            guard.note_wrong(from, t0);
        }
        assert!(matches!(guard.gate(from, t0), Gate::Locked(_)));
    }
    /// 罚期一起判：握手那本账锁着的地址，传书侧的两个处理器也该挡下来。
    #[test]
    fn a_locked_address_is_refused_by_authorize() {
        let state = state();
        let from: IpAddr = "10.0.0.7".parse().unwrap();
        assert!(authorize(&state, from).is_ok(), "没进罚期一律放行");
        let t0 = Instant::now();
        for _ in 0..=FREE_MISTAKES {
            state.guard.note_wrong(from, t0);
        }
        assert!(
            matches!(authorize(&state, from), Err(Refusal::Locked(_))),
            "罚期里的地址传书也别放进来"
        );
    }

    /// 一对处理器共用的那份状态，只为在测试里直接调 `authorize`。
    fn state() -> TransferState {
        TransferState {
            inbox_dir: PathBuf::new(),
            guard: PinGuard::default(),
            on_file: Arc::new(|_| {}),
            lan: None,
        }
    }

    /// 账本不跟着扫描器的源地址数长；淘汰的是错得最少的，重犯留在账上。
    #[test]
    fn the_ledger_does_not_grow_with_the_address_space() {
        let guard = PinGuard::default();
        let t0 = Instant::now();
        let repeat: IpAddr = "10.9.9.9".parse().unwrap();
        for n in 0..(FREE_MISTAKES + 6) {
            guard.note_wrong(repeat, t0 + Duration::from_secs(u64::from(n) * 60));
        }
        // 灌两倍于上限的源地址。钟要停在重犯那格还在罚期内的当口 —— 它连错 11 次，
        // 按阶梯要罚 32 秒，所以取它最后一次失败之后的第 10 秒。
        let last_now = t0 + Duration::from_secs(610);
        for n in 0..(MAX_TRACKED * 2) {
            let from: IpAddr = format!("172.16.{}.{}", n / 256, n % 256).parse().unwrap();
            guard.note_wrong(from, last_now);
        }
        assert!(
            guard.accounts.lock().unwrap().len() <= MAX_TRACKED,
            "格子数要有封顶"
        );
        assert!(
            matches!(guard.gate(repeat, last_now), Gate::Locked(_)),
            "被挤出去的该是错得最少的那些，不是重犯"
        );
    }

    /// 被锁的那一趟给的是 `429 + Retry-After`，而且不足一秒也报一秒。
    #[test]
    fn the_lock_tells_the_client_how_long_to_wait() {
        let response = Refusal::Locked(Duration::from_millis(1)).into_response();
        assert_eq!(response.status(), axum::http::StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("1"),
            "报 0 等于叫客户端立刻再打一趟"
        );
    }
}
