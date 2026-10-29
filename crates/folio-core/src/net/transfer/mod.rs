//! Wi-Fi 传书：设备起一个 HTTP 服务器，同一局域网的浏览器打开网页直接上传书。
//!
//! **tokio 只住在这个模块里。** 传书服务器是 core 里唯一的长驻并发服务：几十个
//! 上传连接各自大部分时间在等网络，加上浏览器上传格式（multipart）的流式解析
//! 是安全洞高发区，这两件事都站在现成的 axum/multer 上最稳 —— 而它们站在 tokio
//! 上。于是这里的边界是：Runtime 自建、跑在自己的线程上、对外只交**同步** API；
//! core 的其余部分（管线、任务、存储）照旧是 std 形状，公共签名不漏 `Future`。
//! 「core 除 transfer 外不依赖异步运行时」是模块纪律，将来嫌软可以拆成独立 crate。
//!
//! 流程：浏览器开 `http://<设备IP>:<port>/?pin=XXXX` → 单页（拖拽 / 多选，XHR
//! 逐文件）→ multipart **流式**落进 `inbox_dir`（块进块写，不进内存）→ 每个文件
//! 完成即 `on_file(path)` → app 侧把路径包成 [`crate::ingest::import::PathsSource`] 提交
//! 导入队列 → 导入成功后清掉收件箱里的原件，失败保留待查。
//!
//! 安全：八位配对码，URL 不带对一律 403；同一个地址连错配额用完后按指数退避锁起来
//! （429 + `Retry-After`），锁着的时候连对的码也挡；比较走常数时间；配对码不落日志。
//! 局域网里的陌生设备因此最多能看见一个上传页，放不进一个字节，也试不出几个码。

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt as _;

use axum::Router;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Multipart, Query, State};
use axum::http::StatusCode;
use axum::http::header;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;

use crate::infra::error::{Error, IoResultExt as _, Result};
use crate::infra::paths;

/// 上传页。内嵌而非资产：这页只有一个用途，跟着二进制走，部署永远不缺它。
const UPLOAD_PAGE: &str = include_str!("transfer_page.html");

/// 单个文件的上传上限。1 GiB 是「没人真的传比这更大的书」的界，挡住的是
/// 恶意或失控的上传把磁盘灌满，不是正常读者。
const MAX_UPLOAD: usize = 1024 * 1024 * 1024;

/// 配对码的位数。八位是「读者能手打进浏览器」与「局域网里没人试得完」之间的那一档：
/// 一亿个组合，再叠上下面那套退避，一台陌生设备一天也摸不到万分之几。
const PIN_DIGITS: u32 = 8;

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

/// 传书服务的配置。`pin` 为空就现场生成八位 —— 每次启动都换，界面上大字显示。
#[derive(Debug, Clone)]
pub struct TransferConfig {
    /// 期望的端口。被占就顺延（最多试 10 个），真实的以 [`TransferServer::addr`] 为准。
    pub port: u16,
    pub pin: String,
    /// 收件箱：上传的字节先落这里。
    pub inbox_dir: PathBuf,
}

/// 一条活着的传书服务。`stop()` 关停（拒绝新连接、收完手上那批）；`Drop` 兜底。
pub struct TransferServer {
    addr: SocketAddr,
    pin: String,
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
        let pin = if config.pin.trim().is_empty() {
            generate_pin()
        } else {
            config.pin.trim().to_string()
        };
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
        let addr = listener.local_addr().io_path(Path::new("0.0.0.0"))?;
        std::fs::create_dir_all(&config.inbox_dir).io_path(&config.inbox_dir)?;

        let (notify, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let inbox = config.inbox_dir.clone();
        let thread_pin = pin.clone();
        let thread = std::thread::Builder::new()
            .name("folio-transfer".to_string())
            .spawn(move || serve(listener, inbox, thread_pin, on_file, shutdown_rx))
            .map_err(|err| Error::Io {
                path: PathBuf::from("folio-transfer"),
                source: Box::new(err),
            })?;
        tracing::info!("transfer: 传书服务起在 {addr}");
        Ok(Self {
            addr,
            pin,
            notify: Some(notify),
            thread: Some(thread),
        })
    }

    /// 实际监听的地址（端口可能与请求的不同 —— 被占就顺延过）。
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// 本次服务的配对码。
    pub fn pin(&self) -> &str {
        &self.pin
    }

    /// 局域网里能打开这个服务的 URL（探测默认路由的出口地址；一个都探不到就空 ——
    /// 界面只好提示读者自己输设备的 IP）。
    pub fn urls(&self) -> Vec<String> {
        lan_addresses()
            .into_iter()
            .map(|ip| format!("http://{ip}:{}/?pin={}", self.addr.port(), self.pin))
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
    pin: String,
    on_file: Arc<dyn Fn(PathBuf) + Send + Sync>,
    notify: tokio::sync::oneshot::Receiver<()>,
) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("传书服务的 runtime 不该建不起来");
    let app = Router::new()
        .route("/", get(upload_page))
        .route("/upload", post(upload))
        .layer(DefaultBodyLimit::max(MAX_UPLOAD))
        .with_state(Arc::new(TransferState {
            inbox_dir,
            pin,
            guard: PinGuard::default(),
            on_file,
        }));
    runtime.block_on(async move {
        // tokio 只收非阻塞的 fd：迁进去之前翻成非阻塞（accept/read 由 runtime 管）。
        listener
            .set_nonblocking(true)
            .expect("listener 翻非阻塞不该失败");
        let listener =
            tokio::net::TcpListener::from_std(listener).expect("listener 迁进 tokio 不该失败");
        // 限速按**对端地址**记，所以要把 connect info 接进服务里 —— 没有这一层，
        // 处理器只拿得到 URL，而 URL 里的配对码正是攻击者要猜的东西。
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
    pin: String,
    guard: PinGuard,
    on_file: Arc<dyn Fn(PathBuf) + Send + Sync>,
}

#[derive(Deserialize)]
struct PinQuery {
    pin: Option<String>,
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

/// 配对码与限速一起判。两个入口（页面与上传）共用这一份，免得哪一侧漏挂。
fn authorize(
    state: &TransferState,
    from: IpAddr,
    given: Option<&str>,
) -> std::result::Result<(), Refusal> {
    if let Gate::Locked(remaining) = state.guard.gate(from, Instant::now()) {
        return Err(Refusal::Locked(remaining));
    }
    if secret_ok(given.unwrap_or(""), &state.pin) {
        state.guard.note_right(from);
        Ok(())
    } else {
        state.guard.note_wrong(from, Instant::now());
        Err(Refusal::BadPin)
    }
}

/// 两种拒绝：错的码，和被锁着。
enum Refusal {
    BadPin,
    Locked(Duration),
}

impl Refusal {
    fn into_response(self) -> Response {
        match self {
            Self::BadPin => StatusCode::FORBIDDEN.into_response(),
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

/// 比配对码：**常数时间**，不短路。
///
/// 先比长度泄的是**长度**，而长度是格式本身（界面上就摆着八位），不是秘密；逐字节的
/// `|=` 而不是 `==` 才是重点 —— 短路比较的耗时能告诉攻击者前几位猜对了。
fn secret_ok(given: &str, want: &str) -> bool {
    let (given, want) = (given.as_bytes(), want.as_bytes());
    if given.len() != want.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in given.iter().zip(want) {
        diff |= a ^ b;
    }
    diff == 0
}

/// 上传页：配对码对、而且这个地址没在罚期里，才给得出这一页。
async fn upload_page(
    State(state): State<Arc<TransferState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(query): Query<PinQuery>,
) -> Response {
    match authorize(&state, peer.ip(), query.pin.as_deref()) {
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
    Query(query): Query<PinQuery>,
    mut multipart: Multipart,
) -> Response {
    if let Err(refusal) = authorize(&state, peer.ip(), query.pin.as_deref()) {
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

/// 现场生成配对码，每次启动一个。
///
/// 熵的来源与 `sync::new_device_id` 同一套：`RandomState` 的种子由 OS 给。不能拿
/// `subsec_nanos` 直接当码 —— Linux 上系统时钟是微秒粒度，末尾三位恒为 0，而
/// `% 10^8` 摊不开那个分布，界面上相邻两次启动的码就成了可推的（旧实现是
/// `% 10^4`，实际只有几千个可能值）。
fn generate_pin() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u128(nanos);
    hasher.write_u128(!nanos ^ u128::from(std::process::id()));
    // 取模的偏置在 2^64 → 10^8 这一档是亿分之一的量级，不值得为它换一套算法。
    let space = 10u64.checked_pow(PIN_DIGITS).unwrap_or(u64::MAX);
    format!(
        "{:0width$}",
        hasher.finish() % space,
        width = PIN_DIGITS as usize
    )
}

/// 探测局域网里能访问本机的地址：往公共 DNS 发一次 **UDP connect**（不发包，
/// 只让内核选路由出口），本端地址就是同网段读者能访问的那个。两个探测目标
/// 跨国内外各一个，谁路由得通用谁；都失败就交空 —— 读者自己输 IP 的提示
/// 由界面出。
fn lan_addresses() -> Vec<String> {
    let mut out = Vec::new();
    for probe in ["223.5.5.5:80", "8.8.8.8:80"] {
        let Ok(socket) = UdpSocket::bind("[::]:0") else {
            break;
        };
        if socket.connect(probe).is_ok()
            && let Ok(local) = socket.local_addr()
        {
            let ip = local.ip().to_string();
            if !out.contains(&ip) {
                out.push(ip);
            }
        }
    }
    out
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
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::{Duration, Instant};

    use tracing_subscriber::Registry;
    use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

    use super::{
        FIRST_LOCK, FREE_MISTAKES, Gate, MAX_LOCK, MAX_TRACKED, PIN_DIGITS, PinGuard, Refusal,
        TransferConfig, TransferServer, TransferState, authorize, generate_pin, penalty,
        sanitize_file_name, secret_ok,
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

    /// 端到端：`port 0` 起服务，ureq 手拼 multipart 上传 —— 收件箱落盘、
    /// 回调收到、错配对码 403。
    #[test]
    fn the_server_receives_uploads_behind_the_pin() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox = tmp.path().join("inbox");
        let received: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
        let got = Arc::clone(&received);
        let server = TransferServer::start(
            TransferConfig {
                port: 0,
                pin: "2468".into(),
                inbox_dir: inbox.clone(),
            },
            Arc::new(move |path| got.lock().unwrap().push(path)),
        )
        .unwrap();
        let base = format!("http://{}", server.addr());

        let client = ureq::AgentBuilder::new().build();
        // 错配对码：页面 403。
        assert!(
            matches!(
                client.get(&format!("{base}/?pin=0000")).call(),
                Err(ureq::Error::Status(403, _))
            ),
            "页面也不给错的配对码看"
        );

        let boundary = "----foliotestboundary";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"三体.epub\"\r\nContent-Type: application/octet-stream\r\n\r\n{}\r\n--{boundary}--\r\n",
            "这是假装的 epub 内容"
        );
        // 闭包只回状态码。`ureq::Error::Status` 里揣着整个 `Response`，照原样把它当 `Err`
        // 交回来就是 clippy::result_large_err（那一格的 `Ok` 侧本来也是拿来数状态码的）；
        // 而这条测试从头到尾要的只是「几号状态」，所以把两条路折成一个 `u16`。
        let post = |pin: &str| -> u16 {
            match client
                .post(&format!("{base}/upload?pin={pin}"))
                .set(
                    "Content-Type",
                    &format!("multipart/form-data; boundary={boundary}"),
                )
                .send(body.as_bytes())
            {
                Ok(resp) => resp.status(),
                Err(ureq::Error::Status(code, _)) => code,
                Err(other) => panic!("上传不该走到传输错误：{other}"),
            }
        };
        // 错配对码 403，一个字节都不落。
        assert_eq!(post("0000"), 403);
        assert!(std::fs::read_dir(&inbox).unwrap().next().is_none());
        // 对配对码：落盘 + 回调。
        assert_eq!(post("2468"), 200);
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

        // 页面在对的配对码下能打开。
        assert!(
            client
                .get(&format!("{base}/?pin=2468"))
                .call()
                .unwrap()
                .into_string()
                .unwrap()
                .contains("input"),
            "上传页是一张带文件选择的表单"
        );
        server.stop();
    }

    /// 配对码逐位比，不是前缀比：短一位、长一位、末尾不同都不算对。
    #[test]
    fn a_prefix_of_the_pin_is_not_the_pin() {
        assert!(secret_ok("12345678", "12345678"));
        assert!(
            !secret_ok("1234567", "12345678"),
            "少一位不是同一个码，前缀不算通过"
        );
        assert!(!secret_ok("123456789", "12345678"), "多一位也不算");
        assert!(!secret_ok("12345670", "12345678"), "最后一位不同要能查出来");
        assert!(!secret_ok("", "12345678"), "URL 不带 pin 一律不算对");
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

    /// 同一个意思在**接线**上再钉一遍：放行对码的那一趟要真的去销账。
    ///
    /// 上面那条只叫 `PinGuard` 自己的方法，把 `authorize` 里的 `note_right` 删掉它也不红；
    /// 这一条走的是处理器用的那同一个函数。
    #[test]
    fn a_right_code_clears_the_debt_through_the_authorizing_path() {
        let state = state("12345678");
        let from: IpAddr = "10.0.0.7".parse().unwrap();
        for n in 1..=FREE_MISTAKES {
            assert!(
                matches!(
                    authorize(&state, from, Some("00000000")),
                    Err(Refusal::BadPin)
                ),
                "第 {n} 次手滑只该给 403"
            );
        }
        assert!(
            authorize(&state, from, Some("12345678")).is_ok(),
            "还没进锁的对码要能过"
        );
        for n in 1..=FREE_MISTAKES + 1 {
            assert!(
                matches!(
                    authorize(&state, from, Some("00000000")),
                    Err(Refusal::BadPin)
                ),
                "销过账之后第 {n} 次还该只给 403：上一轮那几次不该跟着算"
            );
        }
        assert!(
            matches!(
                authorize(&state, from, Some("00000000")),
                Err(Refusal::Locked(_))
            ),
            "重新数满才进锁"
        );
    }

    /// 一对处理器共用的那份状态，只为在测试里直接调 `authorize`。
    fn state(pin: &str) -> TransferState {
        TransferState {
            inbox_dir: PathBuf::new(),
            pin: pin.to_string(),
            guard: PinGuard::default(),
            on_file: Arc::new(|_| {}),
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

    /// 端到端的爆破：20 趟错码里大部分是 429，而**对的码**在罚期里同样被挡。
    #[test]
    fn a_storm_of_wrong_pins_gets_locked_out() {
        let tmp = tempfile::tempdir().unwrap();
        let server = TransferServer::start(
            TransferConfig {
                port: 0,
                pin: "12345678".into(),
                inbox_dir: tmp.path().join("inbox"),
            },
            Arc::new(|_| {}),
        )
        .unwrap();
        let base = format!("http://{}", server.addr());
        let client = ureq::AgentBuilder::new().build();

        let code = |resp: std::result::Result<ureq::Response, ureq::Error>| -> u16 {
            match resp {
                Ok(response) => response.status(),
                Err(ureq::Error::Status(status, response)) => {
                    if status == 429 {
                        assert!(
                            response.header("retry-after").is_some(),
                            "429 必须带 Retry-After"
                        );
                    }
                    status
                }
                Err(other) => panic!("不该走到传输错误：{other}"),
            }
        };

        let mut codes = Vec::new();
        for n in 0..20 {
            let pin = format!("{:08}", 10_000_000 + n);
            codes.push(code(client.get(&format!("{base}/?pin={pin}")).call()));
        }
        let compared = codes.iter().filter(|c| **c == 403).count();
        let locked = codes.iter().filter(|c| **c == 429).count();
        assert!(
            compared <= (FREE_MISTAKES as usize) + 3,
            "真的去比配对码的次数要有封顶，实际 {compared} 趟：{codes:?}"
        );
        assert!(
            locked >= 10,
            "20 趟里大半该被锁掉，实际 {locked} 趟：{codes:?}"
        );
        // 上传那一趟挂的是同一个限速，不能只挡页面。
        codes.push(code(
            client
                .post(&format!("{base}/upload?pin=00000000"))
                .set("Content-Type", "multipart/form-data; boundary=----x")
                .send(b"----x--".as_slice()),
        ));
        assert_eq!(*codes.last().unwrap(), 429, "上传也要吃锁");
        assert!(
            std::fs::read_dir(tmp.path().join("inbox"))
                .unwrap()
                .next()
                .is_none(),
            "被锁住的那趟一个字节都不该落进收件箱"
        );
        assert_eq!(
            code(client.get(&format!("{base}/?pin=12345678")).call()),
            429,
            "锁着的时候放行对的码，等于给爆破者递「这条通了」的信号"
        );
        server.stop();
    }

    /// 启动那一行只报地址：**配对码不落日志**。日志会进 feedback 快照、会被读者
    /// 原样贴出来，那等于把门口钥匙挂在门外。
    #[test]
    fn starting_the_server_does_not_log_the_pin() {
        const PIN: &str = "20193746";
        let tmp = tempfile::tempdir().unwrap();
        let seen = captured(|| {
            let server = TransferServer::start(
                TransferConfig {
                    port: 0,
                    pin: PIN.into(),
                    inbox_dir: tmp.path().join("inbox"),
                },
                Arc::new(|_| {}),
            )
            .unwrap();
            // 对照：同一趟里发一条**故意**带码的日志，确认这支探针真的收得到字。
            // 少了这一条，下面的断言在「探针什么都没收到」时也是绿的。
            tracing::info!("对照组：配对码 {PIN}");
            server.stop();
        });
        assert!(
            seen.iter().any(|line| line.contains("传书服务起在")),
            "启动那一行该在账上：{seen:?}"
        );
        assert!(
            seen.iter().any(|line| line.contains("对照组")),
            "探针什么都没收到，那后面那条断言是空的：{seen:?}"
        );
        for line in seen.iter().filter(|line| !line.contains("对照组")) {
            assert!(!line.contains(PIN), "日志漏出配对码：{line}");
        }
    }

    /// 生成的码是八位数字，而且不是「时钟低位」那种一猜就中的东西。
    #[test]
    fn generated_pins_are_eight_digits_and_differ() {
        let mut codes: Vec<String> = (0..20).map(|_| generate_pin()).collect();
        for code in &codes {
            assert_eq!(
                code.len(),
                PIN_DIGITS as usize,
                "位数是界面上能手打的那几格：{code}"
            );
            assert!(
                code.bytes().all(|byte| byte.is_ascii_digit()),
                "只出数字：{code}"
            );
        }
        codes.sort();
        codes.dedup();
        assert_eq!(codes.len(), 20, "20 次生成撞了重：熵大概只来自时钟");
    }

    /// 装一支只收消息文本的探针，跑一遍 `probes`，返回真落了笔的那些字。
    ///
    /// 用 `set_default`（线程局部），不碰进程级 subscriber：并发用例互不影响，
    /// 也不会把日志格式层抢走。
    fn captured(probes: impl FnOnce()) -> Vec<String> {
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let subscriber = Registry::default().with(Echo(seen.clone()));
        {
            let _guard = tracing::subscriber::set_default(subscriber);
            probes();
        }
        std::mem::take(
            &mut *seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// 只记账、不输出的那层。
    struct Echo(Arc<Mutex<Vec<String>>>);

    impl<S: tracing::Subscriber> Layer<S> for Echo {
        fn on_event(&self, event: &tracing::Event<'_>, _cx: Context<'_, S>) {
            let mut line = String::new();
            event.record(&mut EchoMessage(&mut line));
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(line);
        }
    }

    struct EchoMessage<'a>(&'a mut String);

    impl tracing::field::Visit for EchoMessage<'_> {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.0.push_str(&format!("{value:?}"));
            }
        }
    }
}
