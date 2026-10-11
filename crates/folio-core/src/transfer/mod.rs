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
//! 完成即 `on_file(path)` → app 侧把路径包成 [`crate::import::PathsSource`] 提交
//! 导入队列 → 导入成功后清掉收件箱里的原件，失败保留待查。
//!
//! 安全：四位配对码，URL 不带对一律 403 —— 局域网里的陌生设备最多能看见一个
//! 上传页，放不进一个字节。

use tokio::io::AsyncWriteExt as _;
use std::net::{SocketAddr, TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{DefaultBodyLimit, Multipart, Query, State};
use axum::http::StatusCode;
use axum::response::Html;
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;

use crate::error::{Error, IoResultExt as _, Result};

/// 上传页。内嵌而非资产：这页只有一个用途，跟着二进制走，部署永远不缺它。
const UPLOAD_PAGE: &str = include_str!("transfer_page.html");

/// 单个文件的上传上限。1 GiB 是「没人真的传比这更大的书」的界，挡住的是
/// 恶意或失控的上传把磁盘灌满，不是正常读者。
const MAX_UPLOAD: usize = 1024 * 1024 * 1024;

/// 传书服务的配置。`pin` 为空就现场生成四位 —— 每次启动都换，界面上大字显示。
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
    pub fn start(config: TransferConfig, on_file: Arc<dyn Fn(PathBuf) + Send + Sync>) -> Result<Self> {
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
        tracing::info!("transfer: 传书服务起在 {addr}（配对码 {pin}）");
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
            on_file,
        }));
    runtime.block_on(async move {
        // tokio 只收非阻塞的 fd：迁进去之前翻成非阻塞（accept/read 由 runtime 管）。
        listener
            .set_nonblocking(true)
            .expect("listener 翻非阻塞不该失败");
        let listener =
            tokio::net::TcpListener::from_std(listener).expect("listener 迁进 tokio 不该失败");
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = notify.await;
            })
            .await;
    });
}

struct TransferState {
    inbox_dir: PathBuf,
    pin: String,
    on_file: Arc<dyn Fn(PathBuf) + Send + Sync>,
}

#[derive(Deserialize)]
struct PinQuery {
    pin: Option<String>,
}

/// 配对码对不对。四位数字、服务器只往收件箱里写文件 —— 在线爆破没有回报，
/// 不必上常数时间比较那一套。
fn pin_ok(query: &PinQuery, pin: &str) -> bool {
    query.pin.as_deref() == Some(pin)
}

async fn upload_page(
    State(state): State<Arc<TransferState>>,
    Query(query): Query<PinQuery>,
) -> std::result::Result<Html<&'static str>, StatusCode> {
    if !pin_ok(&query, &state.pin) {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(Html(UPLOAD_PAGE))
}

/// 收一个 multipart 上传：每个文件字段流式落进收件箱，落完回调 `on_file`。
/// 一个字段失败就整个请求报 400 —— 浏览器那侧是一文件一请求，颗粒度对得上。
async fn upload(
    State(state): State<Arc<TransferState>>,
    Query(query): Query<PinQuery>,
    multipart: Multipart,
) -> std::result::Result<StatusCode, (StatusCode, String)> {
    if !pin_ok(&query, &state.pin) {
        return Err((StatusCode::FORBIDDEN, "配对码不对".to_string()));
    }
    let mut multipart = multipart;
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
    for path in saved {
        (state.on_file)(path);
    }
    Ok(StatusCode::OK)
}

/// 浏览器给的文件名 → 收件箱里的安全文件名：剥掉任何路径成分、换掉控制字符
/// 与分隔符，空了就给时间戳名。**路径穿越在这一步被掐死** —— `../../x` 到不了
/// 收件箱外面。
fn sanitize_file_name(raw: &str) -> Option<String> {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let cleaned: String = last
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
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

fn generate_pin() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{:04}", nanos % 10_000)
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
    use std::sync::Mutex;

    use super::{TransferConfig, TransferServer, sanitize_file_name};

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
        assert_eq!(sanitize_file_name("三体.epub").as_deref(), Some("三体.epub"));
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
        let post = |pin: &str| {
            client
                .post(&format!("{base}/upload?pin={pin}"))
                .set("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
                .send(body.as_bytes())
        };
        // 错配对码 403，一个字节都不落。
        assert!(matches!(post("0000"), Err(ureq::Error::Status(403, _))));
        assert!(std::fs::read_dir(&inbox).unwrap().next().is_none());
        // 对配对码：落盘 + 回调。
        let resp = post("2468").unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(
            std::fs::read(inbox.join("三体.epub")).unwrap(),
            "这是假装的 epub 内容".as_bytes(),
            "上传的内容要原样落进收件箱"
        );
        assert_eq!(received.lock().unwrap().len(), 1, "on_file 每个文件回调一次");

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

    use std::path::PathBuf;
    use std::sync::Arc;
}

