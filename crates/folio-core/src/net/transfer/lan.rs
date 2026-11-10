//! 局域网同步挂在传书那台服务器上的那一半：会话表、同账号的门、五条路由。
//!
//! **为什么寄生在 [`super`] 里而不是另起一台服务器**：那一处已经有端口占用顺延、按对端
//! 地址记的失败账本、优雅关停，以及「tokio 只住在这里」那条纪律。再来一份就是两个端口、
//! 两套账本、两处可能漏挂限流的地方；而读者在设置页上看到的仍然是同一个地址。
//!
//! **这里的门与传书那条不一样，别混用判据**：传书是一条 URL 配一个八位码，服务的是
//! 「手上没有账号的人」；这里只认账号，而账号 id 抄得来（它就是 `HMAC(密钥)` 的前 8 字节，
//! 本来就准备在局域网里广播），所以它**不是凭据**，只回答「值不值得跟你握手」。真正放行
//! 读权限的是那对 nonce 上的双向证明（见 [`crate::account`]）：
//!
//! ```text
//! POST /hello   {version, account, nonceA}   → {nonceB, proofB, token, now}
//! POST /auth    {token, proofA}              → 200
//! GET  /manifest|file|snapshot?token=        → 书架清单 / 一本书的字节 / 本机快照
//! PUT  /snapshot?token=                      → body 是对面那台的全量快照，当场合并
//! ```
//!
//! `/hello` 里 id 对不上**不记过错**：局域网里扫描器和不是 folio 的设备天天都有，把整段邻居
//! 锁进罚期会连读者自己那台一起锁掉；而 id 不对也换不到会话，等于什么都没读到。要罚的是
//! **证明没过**的那些 —— 那才是拿着别的密钥撞门的行为。它记进 [`super::PinGuard`] 同一本账，
//! 撞多了连上传页一起进罚期：同一台设备在猜我们的门，不必分开记。
//!
//! 会话绑来源地址：`/hello` 是谁来的，后面每一条就得还是谁。token 本身是 128 位随机数，
//! 绑地址把「抓到 token 再来用」那条路一起堵掉。

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Query, State};
use axum::http::StatusCode;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use tokio::io::AsyncReadExt as _;

use super::{Gate, Refusal, TransferState};
use crate::account::{AccountKey, Nonce, Proof, Side};
use crate::net::lansync::{
    Auth, BookEntry, Hello, HelloReply, MAX_PUSH_BYTES, Manifest, PROTOCOL_VERSION, ROUTE_AUTH,
    ROUTE_FILE, ROUTE_HELLO, ROUTE_MANIFEST, ROUTE_SNAPSHOT, is_book_hash,
};

/// 一次局域网同步会话的存活时长。局域网里一趟往返在秒级，给到十分钟只为了让「拉书拉到一半
/// 屏幕熄了」不必从头握手。
const SESSION_TTL: Duration = Duration::from_secs(600);

/// 同时在座的会话数上限。同账号的设备本来就是个位数，这一格挡的是「有人拿对的 id 猛发
/// `/hello`」把表撑大 —— 满了先挤掉到期最早的那一条，新来的照样拿到会话。
const MAX_SESSIONS: usize = 8;

/// 送出去时一次读多少字节。256 KiB 是「一次系统调用划算」与「别在手上压太多」之间那一档。
const FILE_CHUNK: usize = 256 * 1024;

/// 一台设备推过来的快照能有多大。快照只有进度 / 批注 / 书签 / 设置，正常一两 MB 封顶；
/// 给到 32 MiB 是为了把「一本书真的批注了几千条」放过去，把「对面把整库内容塞进这一格」挡住。
const MAX_SNAPSHOT: usize = 32 * 1024 * 1024;

/// 局域网同步的配置。**没有它这几条路由一概 404**：传书可以单独开着，局域网同步是另一个决定。
#[derive(Clone)]
pub struct LanConfig {
    /// 本机那把账号密钥。它不进日志、也不进 `Debug`（见 [`crate::account`]）。
    pub key: AccountKey,
    /// 本机书架与本机进度怎么读怎么写 —— 由装配这个服务的那一层给（见 [`LanProvider`]）。
    pub provider: Arc<dyn LanProvider>,
}

impl std::fmt::Debug for LanConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 会话密钥由这把密钥算出来，所以 `Debug` 里不能带它：把整份 `TransferConfig`
        // 打进日志是排错时最自然的一个动作。
        f.write_str("LanConfig(账号密钥已隐去)")
    }
}

/// 服务器要向本机要的四件事。
///
/// 与 [`super::TransferServer::start`] 那个 `on_file` 回调同一条理由：书库目录在哪儿、
/// 数据库连接归谁、导入管线怎么提交，都是装配那一层的事，这里只交形状 —— 于是这些处理器
/// 能拿一只假的实现离线测完，不必真有一块屏、一个库。
pub trait LanProvider: Send + Sync + 'static {
    /// 本机有副本可拉的书。
    fn manifest(&self) -> Vec<BookEntry>;

    /// 那本书的副本在哪儿。`content_hash`（64 位十六进制）认不出来的回 `None`。
    fn file(&self, hash: &str) -> Option<PathBuf>;

    /// 本机这一份全量快照的 JSON。对面拉走它去合自己的。
    fn snapshot(&self) -> Result<String, String>;

    /// 收下对面推过来的那份并**就地合并进本机**。合并规则一条都不重写，见 [`crate::sync`]。
    fn apply_snapshot(&self, json: &[u8]) -> Result<(), String>;
}

/// 一条还在有效期里的会话。
struct Session {
    /// `/hello` 那一趟的来源地址。后面每一条都得是它。
    peer: IpAddr,
    /// 从对面那半 nonce 与本机这半 nonce 算出来的会话密钥。
    ///
    /// 本机那一半的 nonce 不单独存：它只在 `/hello` 的响应里用一次（对面要拿两半拼出同一个
    /// 会话密钥），而这里的这一格已经是拼好的结果。存两样能算出对方的东西就是第二个
    /// 可以不一致的副本。
    account: crate::account::Session,
    /// 什么时候作废。
    until: Instant,
    /// `/auth` 过了没有。没过之前任何数据路由都不答。
    proven: bool,
}

/// 局域网同步那侧的运行状态。挂在 [`TransferState`] 上，`None` 就是没开。
pub(super) struct LanState {
    key: AccountKey,
    /// 本机账号 id 的十六进制。`/hello` 每一条都要比它，现算一次存下来。
    id: String,
    provider: Arc<dyn LanProvider>,
    sessions: Mutex<HashMap<String, Session>>,
}

impl LanState {
    pub(super) fn new(config: LanConfig) -> Self {
        let key = config.key;
        Self {
            id: key.id().to_hex(),
            key,
            provider: config.provider,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// 发一条新会话，回 `(token, 本机的 nonce, 会话密钥)`。会话密钥是给调用方算证明用的，
    /// 表里那份是同一把。
    ///
    /// 表满先清过期的，还满就挤掉到期最早的那一条 —— 被挤掉的那条只是要重新握手，
    /// 而这里的握手一趟是毫秒级。
    fn open(
        &self,
        peer: IpAddr,
        peer_nonce: &Nonce,
    ) -> Option<(String, Nonce, crate::account::Session)> {
        let my_nonce = Nonce::generate()?;
        let token = Nonce::generate()?;
        // 派生顺序两端必须一致：**先发起方那半，后接收方那半**。这里是服务器，所以对面那半
        // 就是发起方的那一半。
        let account = self.key.session(peer_nonce, &my_nonce);
        let mut sessions = self.locked();
        let now = Instant::now();
        sessions.retain(|_, session| session.until > now);
        if sessions.len() >= MAX_SESSIONS {
            let oldest = sessions
                .iter()
                .min_by_key(|(_, session)| session.until)
                .map(|(token, _)| token.clone());
            if let Some(oldest) = oldest {
                sessions.remove(&oldest);
            }
        }
        sessions.insert(
            token.to_hex(),
            Session {
                peer,
                account: account.clone(),
                until: now + SESSION_TTL,
                proven: false,
            },
        );
        Some((token.to_hex(), my_nonce, account))
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, HashMap<String, Session>> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// 挂进传书那台服务器的路由。状态仍共用那一份 [`TransferState`]，所以这里只交路由本身。
///
/// 请求体上限挂在**这一组**路由上：传书那一侧要收一 GiB 的书，而这里的快照只该收
/// [`MAX_SNAPSHOT`]，两边共用一个上限等于把这边放大到那边那么大。
pub(super) fn routes() -> Router<Arc<TransferState>> {
    Router::new()
        .route(ROUTE_HELLO, post(hello))
        .route(ROUTE_AUTH, post(auth))
        .route(ROUTE_MANIFEST, get(manifest))
        .route(ROUTE_FILE, get(file))
        .route(ROUTE_SNAPSHOT, get(snapshot_get).put(snapshot_put))
        .layer(DefaultBodyLimit::max(MAX_SNAPSHOT))
}

/// `?token=` 那一条参数，数据路由每条都带。
#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

#[derive(Deserialize)]
struct FileQuery {
    token: Option<String>,
    hash: Option<String>,
}

/// 没开局域网同步：一概 404。它不给「这里本该有一条路由」的信号，因为那信号本身就是信息。
fn not_found() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

/// 这两条握手路由也吃传书那一本账：反复出不了证明的设备与猜配对码的设备是同一类嫌疑犯，
/// 分成两本账只会让其中一本漏挂限流。回 `Some` 就是这一趟到此为止（429 + `Retry-After`）。
///
/// 数据路由不吃：它们要 token，而 token 只能从 `/hello` 拿到 —— 门已经在上游关掉了。
fn locked_out(state: &TransferState, from: IpAddr) -> Option<Response> {
    match state.guard.gate(from, Instant::now()) {
        Gate::Locked(remaining) => Some(Refusal::Locked(remaining).into_response()),
        Gate::Proceed => None,
    }
}

/// 数据路由的门：token 认得、没过期、来源地址没换、`/auth` 已经过了。
///
/// 回 `None` 才是放行；缺 token、坏 token、没验过、过期、换了地址一律给一句答复。
fn check_token(state: &LanState, given: Option<&str>, from: IpAddr) -> Option<Response> {
    let Some(token) = given.filter(|token| !token.is_empty()) else {
        return Some(StatusCode::FORBIDDEN.into_response());
    };
    let mut sessions = state.locked();
    let Some(session) = sessions.get_mut(token) else {
        return Some(StatusCode::FORBIDDEN.into_response());
    };
    // 过期就地删：这一条路上没有别的清扫时机，不删就等表满时被挤掉。
    if session.until <= Instant::now() {
        sessions.remove(token);
        return Some(StatusCode::GONE.into_response());
    }
    if session.peer != from || !session.proven {
        return Some(StatusCode::FORBIDDEN.into_response());
    }
    None
}

/// `POST /hello`：声称的账号 id 对得上才发会话，并当场出示接收方那一句证明。
async fn hello(
    State(state): State<Arc<TransferState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<Hello>,
) -> Response {
    let Some(lan) = &state.lan else {
        return not_found();
    };
    if let Some(refusal) = locked_out(&state, peer.ip()) {
        return refusal;
    }
    if body.version != PROTOCOL_VERSION {
        // 版本不对就直说：局域网里两端各自升级是常态，猜字段那种错最难查，而对面上
        // 「本机说哪一版」是一句能直接读出的话。
        return (
            StatusCode::BAD_REQUEST,
            format!("协议版本不一样：本机说第 {PROTOCOL_VERSION} 版"),
        )
            .into_response();
    }
    if body.account != lan.id {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(peer_nonce) = Nonce::from_hex(&body.nonce) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some((token, my_nonce, account)) = lan.open(peer.ip(), &peer_nonce) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    Json(HelloReply {
        version: PROTOCOL_VERSION,
        account: lan.id.clone(),
        // 回的是**本机**那一半的 nonce：对面那半是它自己发来的，而会话密钥要按
        // 「先发起方、后接收方」的顺序拼，两边不能各猜一半。
        nonce: my_nonce.to_hex(),
        proof: account.own_proof(Side::Responder).to_hex(),
        token,
        now: unix_now(),
    })
    .into_response()
}

/// `POST /auth`：验发起方出示的那一句证明。过了它才有后面的数据路由。
///
/// 这一趟不是多余的：会话密钥里两个 nonce 都算进去了，而 `/hello` 那一趟只证了「本机持有
/// 密钥」这一件事的一半 —— 对面不出示自己那一句，本机就知道的只是「有人声称同一个 id」。
async fn auth(
    State(state): State<Arc<TransferState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<Auth>,
) -> Response {
    let Some(lan) = &state.lan else {
        return not_found();
    };
    if let Some(refusal) = locked_out(&state, peer.ip()) {
        return refusal;
    }
    let Some(given) = Proof::from_hex(&body.proof) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let mut sessions = lan.locked();
    let Some(session) = sessions.get_mut(&body.token) else {
        return StatusCode::FORBIDDEN.into_response();
    };
    if session.peer != peer.ip() || session.until <= Instant::now() {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !session.account.verify(Side::Initiator, &given) {
        drop(sessions);
        // 记在地址上，与传书同一本账：证明反复不过就是在猜门，该和猜配对码一个待遇。
        state.guard.note_wrong(peer.ip(), Instant::now());
        return StatusCode::FORBIDDEN.into_response();
    }
    session.proven = true;
    state.guard.note_right(peer.ip());
    StatusCode::OK.into_response()
}

/// `GET /manifest`：本机有哪些书可以被对面拉走。
async fn manifest(
    State(state): State<Arc<TransferState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(query): Query<TokenQuery>,
) -> Response {
    let Some(lan) = &state.lan else {
        return not_found();
    };
    if let Some(refusal) = check_token(lan, query.token.as_deref(), peer.ip()) {
        return refusal;
    }
    Json(Manifest {
        version: PROTOCOL_VERSION,
        books: lan.provider.manifest(),
    })
    .into_response()
}

/// `GET /file?token=&hash=`：那本书的字节，流式送出。
async fn file(
    State(state): State<Arc<TransferState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(query): Query<FileQuery>,
) -> Response {
    let Some(lan) = &state.lan else {
        return not_found();
    };
    if let Some(refusal) = check_token(lan, query.token.as_deref(), peer.ip()) {
        return refusal;
    }
    let Some(hash) = query.hash.filter(|hash| is_book_hash(hash)) else {
        // 形状不对的 hash 连库都不必问：它不可能是本机任何一本书的身份。
        return not_found();
    };
    let Some(path) = lan.provider.file(&hash) else {
        return not_found();
    };
    let Ok(metadata) = tokio::fs::metadata(&path).await else {
        // 清单报了它、盘上却没有了 —— 库和目录对不上，按「没有这本书」答，让对面下一轮再问。
        return not_found();
    };
    let size = metadata.len();
    if size > MAX_PUSH_BYTES {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let Ok(file) = tokio::fs::File::open(&path).await else {
        return not_found();
    };

    // 一块读一块发，全程不在内存里攒整本书：一本漫画 PDF 也是一次 256 KiB。
    // 读错就断在这条流上 —— 响应带着 `Content-Length`，对面拿到的字节数不够自己会算出来，
    // 所以这里不需要也不该把一次读失败伪装成「书读完了」。
    let stream = futures_util::stream::try_unfold(file, |mut file| async move {
        let mut chunk = Vec::with_capacity(FILE_CHUNK);
        match file.read_buf(&mut chunk).await {
            Ok(0) => Ok(None),
            Ok(_) => Ok(Some((chunk, file))),
            Err(err) => Err(err),
        }
    });

    (
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (header::CONTENT_LENGTH, size.to_string()),
        ],
        Body::from_stream(stream),
    )
        .into_response()
}

/// `GET /snapshot`：本机这一份全量快照，对面拿去合它自己的。
async fn snapshot_get(
    State(state): State<Arc<TransferState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(query): Query<TokenQuery>,
) -> Response {
    let Some(lan) = &state.lan else {
        return not_found();
    };
    if let Some(refusal) = check_token(lan, query.token.as_deref(), peer.ip()) {
        return refusal;
    }
    match lan.provider.snapshot() {
        Ok(json) => ([(header::CONTENT_TYPE, "application/json")], json).into_response(),
        Err(why) => (StatusCode::INTERNAL_SERVER_ERROR, why).into_response(),
    }
}

/// `PUT /snapshot`：收下对面那一份并就地合并进本机。
///
/// 一次下拉里两个方向都走完：本机读了对面的快照合进自己，对面那一份也当场落进本机。
/// 于是**两台这一次都收齐了**，不必等对面哪天也下拉一遍 —— 这是局域网比「各自往云端写一份」
/// 强的地方，也是这一条路由存在的理由。
async fn snapshot_put(
    State(state): State<Arc<TransferState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(query): Query<TokenQuery>,
    body: String,
) -> Response {
    let Some(lan) = &state.lan else {
        return not_found();
    };
    if let Some(refusal) = check_token(lan, query.token.as_deref(), peer.ip()) {
        return refusal;
    }
    match lan.provider.apply_snapshot(body.as_bytes()) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(why) => (StatusCode::UNPROCESSABLE_ENTITY, why).into_response(),
    }
}

/// 此刻的 UTC 秒。对面拿它和自己这一个相减就是钟差，用来平移读回来的那份快照
/// （见 [`crate::sync::Snapshot::shift_clock`]）。
fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::account::{AccountKey, Nonce, Proof, Side};
    use crate::net::lansync::{
        Auth, BookEntry, Hello, HelloReply, Manifest, PROTOCOL_VERSION, ROUTE_AUTH, ROUTE_FILE,
        ROUTE_HELLO, ROUTE_MANIFEST, ROUTE_SNAPSHOT,
    };
    use crate::net::transfer::{LanConfig, LanProvider, TransferConfig, TransferServer};
    use serde::Serialize;

    /// 一本书的身份在报文里是 64 个十六进制字符，测试里就用两个好读的前缀补齐。
    fn hash_of(letter: char) -> String {
        letter.to_string().repeat(64)
    }

    /// 这一台被问了几次书、代收了几份快照。测试拿这两个数说话，而不是翻日志。
    #[derive(Default)]
    struct Log {
        asked: usize,
        applied: Vec<Vec<u8>>,
    }

    /// 一台假设备：书架上两本真文件（要真的能从盘上流式读出去），快照是固定的一串。
    struct Fake {
        books: Vec<(String, PathBuf)>,
        snapshot: String,
        log: Arc<Mutex<Log>>,
    }

    impl LanProvider for Fake {
        fn manifest(&self) -> Vec<BookEntry> {
            self.books
                .iter()
                .map(|(hash, path)| BookEntry {
                    hash: hash.clone(),
                    size: std::fs::metadata(path)
                        .map(|meta| meta.len())
                        .unwrap_or_default(),
                    title: format!("《{hash}》"),
                })
                .collect()
        }

        fn file(&self, hash: &str) -> Option<PathBuf> {
            self.log.lock().unwrap().asked += 1;
            self.books
                .iter()
                .find(|(want, _)| want.eq_ignore_ascii_case(hash))
                .map(|(_, path)| path.clone())
        }

        fn snapshot(&self) -> Result<String, String> {
            Ok(self.snapshot.clone())
        }

        fn apply_snapshot(&self, json: &[u8]) -> Result<(), String> {
            self.log.lock().unwrap().applied.push(json.to_vec());
            Ok(())
        }
    }

    /// 测试台：真服务器、真连接、`port 0` 让内核挑端口。
    struct Rig {
        server: TransferServer,
        /// 那两本假书与收件箱都住在这个目录里，它必须比服务器活得久。
        _tmp: tempfile::TempDir,
        log: Arc<Mutex<Log>>,
        client: ureq::Agent,
    }

    impl Rig {
        /// `lan` 给密钥就是开了局域网同步的那一台；给 `None` 就是只开传书的那一台。
        fn start(lan: Option<AccountKey>) -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let first = tmp.path().join("first");
            let second = tmp.path().join("second");
            std::fs::write(&first, "这是第一本书的字节").unwrap();
            std::fs::write(&second, "这是第二本书的字节，长一点：××××").unwrap();
            let log = Arc::new(Mutex::new(Log::default()));
            let config = TransferConfig {
                port: 0,
                inbox_dir: tmp.path().join("inbox"),
                lan: lan.map(|key| LanConfig {
                    key,
                    provider: Arc::new(Fake {
                        books: vec![
                            (hash_of('a'), first.clone()),
                            (hash_of('b'), second.clone()),
                        ],
                        snapshot: r#"{"version":1,"device_id":"dev-a","exported_at":7,"books":{}}"#
                            .to_string(),
                        log: Arc::clone(&log),
                    }),
                }),
            };
            let server = TransferServer::start(config, Arc::new(|_| {})).unwrap();
            Self {
                server,
                _tmp: tmp,
                log,
                client: ureq::AgentBuilder::new().build(),
            }
        }

        fn url(&self, route: &str) -> String {
            format!("http://{}{route}", self.server.addr())
        }

        fn post_json<T: Serialize>(&self, route: &str, body: &T) -> (u16, String) {
            self.settled(
                self.client
                    .post(&self.url(route))
                    .set("Content-Type", "application/json")
                    .send_string(&serde_json::to_string(body).unwrap()),
            )
        }

        fn get(&self, route_with_query: &str) -> (u16, String) {
            self.settled(self.client.get(&self.url(route_with_query)).call())
        }

        fn put(&self, route_with_query: &str, body: &str) -> (u16, String) {
            self.settled(
                self.client
                    .put(&self.url(route_with_query))
                    .set("Content-Type", "application/json")
                    .send_string(body),
            )
        }

        /// 错误响应也带正文，一并取回来 —— 「400 那句说得出口吗」是要断的东西。
        fn settled(&self, result: Result<ureq::Response, ureq::Error>) -> (u16, String) {
            match result {
                Ok(response) => {
                    let status = response.status();
                    (status, response.into_string().unwrap_or_default())
                }
                Err(ureq::Error::Status(status, response)) => {
                    (status, response.into_string().unwrap_or_default())
                }
                Err(err) => panic!("局域网里的连接本身就该成：{err}"),
            }
        }

        /// 只发 `/hello`，回对面那份响应。`claimed` 是打算声称的账号 id。
        fn hello(&self, claimed: &str, my_nonce: &Nonce) -> (u16, String) {
            self.post_json(
                ROUTE_HELLO,
                &Hello {
                    version: PROTOCOL_VERSION,
                    account: claimed.to_string(),
                    nonce: my_nonce.to_hex(),
                },
            )
        }

        /// 走完整条握手，回会话凭据；`/auth` 没过就回 `None`。
        ///
        /// `key` 是**手上真有**的那把密钥，`claimed` 是要声称的 id —— 两者对不上就是
        /// 「抄到了 id、没有密钥」那一台，正是这条路要挡在门外的东西。
        fn handshake(&self, key: &AccountKey, claimed: &str) -> Option<String> {
            let my_nonce = Nonce::generate().unwrap();
            let (status, body) = self.hello(claimed, &my_nonce);
            assert_eq!(status, 200, "`/hello` 该过，实际：{body}");
            let reply: HelloReply = serde_json::from_str(&body).unwrap();
            let peer_nonce = Nonce::from_hex(&reply.nonce).unwrap();
            // 顺序两端必须一致：先发起方那一半，后接收方那一半。
            let session = key.session(&my_nonce, &peer_nonce);
            assert!(
                session.verify(Side::Responder, &Proof::from_hex(&reply.proof).unwrap()),
                "对面那句证明验不过，后面每一条都是白跑"
            );
            let (status, _) = self.post_json(
                ROUTE_AUTH,
                &Auth {
                    token: reply.token.clone(),
                    proof: session.own_proof(Side::Initiator).to_hex(),
                },
            );
            (status == 200).then_some(reply.token)
        }
    }

    /// 端到端一条整路：握手 → 书架清单 → 一本书的字节 → 本机快照 → 收下对面推来的快照。
    ///
    /// 这一条是整套协议的闸门：**nonce 的先后顺序、两侧证明各签自己那一句、token 在
    /// 两条请求间接手**，任何一环写反都会在这里断掉，而写反的东西在读代码时看不出来。
    #[test]
    fn one_account_shakes_hands_and_then_reads_and_writes() {
        let key = AccountKey::generate().unwrap();
        let rig = Rig::start(Some(key));

        let token = rig.handshake(&key, &key.id().to_hex()).expect("握手该过");

        // 清单：两本书，大小与盘上的一致。
        let (status, body) = rig.get(&format!("{ROUTE_MANIFEST}?token={token}"));
        assert_eq!(status, 200);
        let manifest: Manifest = serde_json::from_str(&body).unwrap();
        assert_eq!(manifest.books.len(), 2);
        assert_eq!(manifest.version, PROTOCOL_VERSION);

        // 一本书：字节原样、`Content-Length` 报的数与读到的一致。
        let first = std::fs::read(rig._tmp.path().join("first")).unwrap();
        let response = rig
            .client
            .get(&rig.url(&format!("{ROUTE_FILE}?token={token}&hash={}", hash_of('a'))))
            .call()
            .unwrap();
        assert_eq!(
            response.header("content-length").map(str::to_string),
            Some(first.len().to_string()),
            "流式送出时长度要提前说死，对面才知道少读了"
        );
        assert_eq!(response.into_string().unwrap().into_bytes(), first);

        // 本机快照读得回来。
        let (status, body) = rig.get(&format!("{ROUTE_SNAPSHOT}?token={token}"));
        assert_eq!(status, 200);
        assert!(body.contains("dev-a"), "该是 provider 写的那一份：{body}");

        // 对面推来的一份当场收下并合并（这一台只是记下来，真合并是上层的事）。
        let (status, _) = rig.put(
            &format!("{ROUTE_SNAPSHOT}?token={token}"),
            r#"{"version":1}"#,
        );
        assert_eq!(status, 200);
        assert_eq!(
            rig.log.lock().unwrap().applied.len(),
            1,
            "推来的那一份要落到本机"
        );

        // 对表：`now` 与本机钟差在一秒内，这就是 `shift_clock` 要的偏移量的来源。
        let (_, body) = rig.hello(&key.id().to_hex(), &Nonce::generate().unwrap());
        let reply: HelloReply = serde_json::from_str(&body).unwrap();
        let local = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!(
            (reply.now - local).abs() <= 1,
            "对端报的钟是 {}，本机是 {local}",
            reply.now
        );
    }

    /// 另一把密钥的设备连会话都拿不到：它声称的 id 对不上。
    #[test]
    fn a_different_account_is_turned_away_at_hello() {
        let mine = AccountKey::generate().unwrap();
        let theirs = AccountKey::generate().unwrap();
        let rig = Rig::start(Some(mine));

        let (status, _) = rig.hello(&theirs.id().to_hex(), &Nonce::generate().unwrap());
        assert_eq!(status, 403, "不是同一个账号就该挡在握手之前");
        // 挡下它不该留下任何可以接着用的东西。
        assert_eq!(rig.log.lock().unwrap().asked, 0, "没握手就不该有人来问书");
    }

    /// 抄到了 id、没有密钥：`/hello` 会给会话（id 本来就准备公开），但两侧都对不上 ——
    /// 它验不过本机的证明，本机也验不过它那句。双向都挡得住才算门。
    #[test]
    fn a_stolen_id_without_the_key_fails_in_both_directions() {
        let mine = AccountKey::generate().unwrap();
        let rig = Rig::start(Some(mine));
        let intruder = AccountKey::generate().unwrap();

        let my_nonce = Nonce::generate().unwrap();
        let (status, body) = rig.hello(&mine.id().to_hex(), &my_nonce);
        assert_eq!(status, 200, "id 对得上就该给到会话，门不在这一格");
        let reply: HelloReply = serde_json::from_str(&body).unwrap();
        let session = intruder.session(&my_nonce, &Nonce::from_hex(&reply.nonce).unwrap());

        // 它验不过本机出示的那一句：拿着别的密钥算不出同一个会话密钥。
        assert!(
            !session.verify(Side::Responder, &Proof::from_hex(&reply.proof).unwrap()),
            "别的密钥该验不过对面那句证明"
        );
        // 本机也不认它那句。
        let (status, _) = rig.post_json(
            ROUTE_AUTH,
            &Auth {
                token: reply.token.clone(),
                proof: session.own_proof(Side::Initiator).to_hex(),
            },
        );
        assert_eq!(status, 403, "没有密钥就过不了 `/auth`");
        assert_eq!(
            rig.get(&format!("{ROUTE_MANIFEST}?token={}", reply.token))
                .0,
            403,
            "会话拿到了也读不到任何东西"
        );
    }

    /// `/hello` 之后跳过 `/auth` 直接要数据：一条也不给。
    #[test]
    fn a_session_that_skipped_the_proof_reads_nothing() {
        let key = AccountKey::generate().unwrap();
        let rig = Rig::start(Some(key));
        let my_nonce = Nonce::generate().unwrap();
        let (_, body) = rig.hello(&key.id().to_hex(), &my_nonce);
        let reply: HelloReply = serde_json::from_str(&body).unwrap();

        for route in [ROUTE_MANIFEST, ROUTE_SNAPSHOT] {
            let (status, _) = rig.get(&format!("{route}?token={}", reply.token));
            assert_eq!(status, 403, "过了 `/hello` 不等于过了 `/auth`：{route}");
        }
        // 连不带 token 也一样拒，别把「缺参数」和「没门」分成两种可分辨的信号。
        assert_eq!(rig.get(ROUTE_MANIFEST).0, 403);
    }

    /// 反复出不了证明 = 在猜门，与猜配对码一个待遇：共用那一本按地址记的账，
    /// 连错到数之后连 `/hello` 都进 429。
    #[test]
    fn repeated_bad_proofs_lock_the_address_out_of_everything() {
        let mine = AccountKey::generate().unwrap();
        let rig = Rig::start(Some(mine));
        let intruder = AccountKey::generate().unwrap();
        let claimed = mine.id().to_hex();

        let mut statuses = Vec::new();
        for _ in 0..6 {
            let my_nonce = Nonce::generate().unwrap();
            let (_, body) = rig.hello(&claimed, &my_nonce);
            let Ok(reply) = serde_json::from_str::<HelloReply>(&body) else {
                break;
            };
            // 用别的密钥算出来的证明送过去，就是一次猜门。
            let session = intruder.session(&my_nonce, &Nonce::from_hex(&reply.nonce).unwrap());
            let (status, _) = rig.post_json(
                ROUTE_AUTH,
                &Auth {
                    token: reply.token,
                    proof: session.own_proof(Side::Initiator).to_hex(),
                },
            );
            statuses.push(status);
        }
        assert!(
            statuses.iter().all(|status| *status == 403),
            "前六次都该只是被拒，罚期从下一次请求才生效：{statuses:?}"
        );

        let (status, _) = rig.hello(&claimed, &Nonce::generate().unwrap());
        assert_eq!(status, 429, "猜够了次数就该连握手都不给，与传书同一条账");
    }

    /// 版本不对要说得出口：局域网里两端各自升级是常态，猜字段的错最难查。
    #[test]
    fn another_protocol_version_is_refused_with_a_readable_reason() {
        let key = AccountKey::generate().unwrap();
        let rig = Rig::start(Some(key));
        let (status, body) = rig.post_json(
            ROUTE_HELLO,
            &Hello {
                version: PROTOCOL_VERSION + 1,
                account: key.id().to_hex(),
                nonce: Nonce::generate().unwrap().to_hex(),
            },
        );
        assert_eq!(status, 400);
        assert!(body.contains("协议版本"), "该有一句能读的话：{body}");
    }

    /// 形状不对的 hash 连库都不必问 —— 它不可能是本机任何一本书的身份。
    #[test]
    fn a_hash_that_is_not_a_hash_never_reaches_the_library() {
        let key = AccountKey::generate().unwrap();
        let rig = Rig::start(Some(key));
        let token = rig.handshake(&key, &key.id().to_hex()).unwrap();

        let bad_hashes = vec![
            "../../etc/passwd".to_string(),
            hash_of('a').replacen('a', "z", 1),
            "a".to_string(),
            hash_of('a')[..62].to_string(),
        ];
        for bad in bad_hashes {
            let (status, _) = rig.get(&format!("{ROUTE_FILE}?token={token}&hash={bad}"));
            assert_eq!(status, 404, "坏形状的 hash 该在门口就被拒：{bad}");
        }
        assert_eq!(
            rig.log.lock().unwrap().asked,
            0,
            "三条都没问到 provider，等于一条也没进数据库"
        );

        // 对形状的则问得到，而且大小写都认（这一串可能被人从屏幕上抄过）。
        let upper = hash_of('a').to_uppercase();
        let (status, body) = rig.get(&format!("{ROUTE_FILE}?token={token}&hash={upper}"));
        assert_eq!(status, 200, "大写也该发得出书：{body}");
    }

    /// 只开传书、没开局域网同步的那一台：这几条路由一概 404，而上传页照旧。
    #[test]
    fn the_lan_routes_are_dark_when_the_account_is_not_switched_on() {
        let key = AccountKey::generate().unwrap();
        let rig = Rig::start(None);

        let (status, _) = rig.hello(&key.id().to_hex(), &Nonce::generate().unwrap());
        assert_eq!(status, 404, "没开就没有「这里本该有一条路由」的信号");
        assert_eq!(rig.get(ROUTE_MANIFEST).0, 404);
        assert_eq!(rig.get(ROUTE_SNAPSHOT).0, 404);
        // 传书那一侧不受影响：不带配对码也直接给页面。
        assert_eq!(rig.get("/").0, 200, "局域网同步没开不该把传书一起关掉");
    }

    /// **客户端那一半与这台上的一半真在一根套接头上遇一次。**
    ///
    /// 上面那几条测的都是「手写的请求打得过服务器」，而界面将来发的是
    /// [`crate::net::lansync::sync_one`] 拼出来的请求 —— 两边各写各的时，查询串里叫
    /// `token` 还是 `tok`、PUT 的 body 是不是那份合并后的快照，都只有跑通一次才看得见。
    #[test]
    fn the_client_half_meets_the_server_half_over_a_real_socket() {
        use std::sync::Mutex;

        use super::unix_now;
        use crate::net::lansync::{self, Applied, LanLocal};
        use crate::net::remote::ureq;

        /// 发起那一台：记下被要求下拉的 URL，合并那一步只交出一样东西 —— 一句「这是本机
        /// 合完的那一份」，好让断言落在**推回去的到底是哪一份**上。
        struct ClientLocal {
            pulls: Mutex<Vec<String>>,
            /// 交到合并那一步的对面快照，序列化成串存下来（断言 `dev-a` 与那两个时间戳）。
            merged: Mutex<Vec<String>>,
        }

        impl LanLocal for ClientLocal {
            fn local_hashes(&self) -> std::collections::HashSet<String> {
                // 本机只有 `a` 那一本：所以对面清单里的 `b` 是该拉的，而 `a` 不该再问。
                [hash_of('a')].into_iter().collect()
            }

            fn pull_book(&self, url: &str) -> bool {
                self.pulls.lock().unwrap().push(url.to_string());
                true
            }

            fn merge_and_apply(
                &self,
                remotes: &[crate::sync::Snapshot],
                _now: i64,
            ) -> std::result::Result<Applied, String> {
                let mut dumped = self.merged.lock().unwrap();
                for remote in remotes {
                    dumped.push(serde_json::to_string(remote).expect("测试里的快照解得开"));
                }
                Ok(Applied {
                    json: r#"{"marker":"from-client"}"#.to_string(),
                    positions: 1,
                    annotations: 0,
                    bookmarks: 0,
                })
            }
        }

        let key = AccountKey::generate().unwrap();
        let rig = Rig::start(Some(key));
        let local = ClientLocal {
            pulls: Mutex::new(Vec::new()),
            merged: Mutex::new(Vec::new()),
        };
        let base = format!("http://{}", rig.server.addr());
        // 本机的钟故意报成「比这台服务器快一千秒」：对表那一步要在那份快照上留下这一千秒。
        let now = unix_now() + 1_000;
        let report = lansync::sync_one(&local, ureq::shared(), &base, &key, now)
            .expect("客户端那一半该与这台服务器走通整趟");

        assert_eq!(report.pulled, 1, "对面两本、本机有一本，就该只补那缺的一本");

        // 客户端拼出来的下载地址，在这台真服务器上读得出**那本书的字节**：
        // 查询串里那两个名字、token 的来源、hash 的传法，全在这一句里对上。
        let pulls = local.pulls.lock().unwrap();
        assert_eq!(pulls.len(), 1);
        assert!(
            !pulls[0].contains(&hash_of('a')),
            "本机有的那一本不该出现在下拉里：{}",
            pulls[0]
        );
        let wanted = std::fs::read(rig._tmp.path().join("second")).unwrap();
        let served = rig
            .client
            .get(&pulls[0])
            .call()
            .expect("客户端给出的 URL 就该打得通");
        assert_eq!(served.into_string().unwrap().into_bytes(), wanted);
        drop(pulls);

        // GET /snapshot 走通了：交到合并那一步的，是这台写的那一份。而它 `exported_at` 是
        // 对面钟上的数（夹具手写的 7），搬到本机这一千秒快的钟面上应当变成 1007 ——
        // 差一秒是往返本身，所以放开两秒。
        let merged = local.merged.lock().unwrap();
        assert_eq!(merged.len(), 1, "一趟只合对面那一份");
        assert!(
            merged[0].contains("dev-a"),
            "该读到服务器的快照：{}",
            merged[0]
        );
        let rebased = serde_json::from_str::<crate::sync::Snapshot>(&merged[0])
            .unwrap()
            .exported_at;
        assert!(
            (rebased - 1_007).abs() <= 2,
            "对表要把对面那份搬到本机钟面上，实际搬到 {rebased}（该在 1007 上下）"
        );
        drop(merged);

        // 推回去的是**客户端合并后的那一份**（那一句 marker），不是原样转发的服务器快照。
        let applied = rig.log.lock().unwrap().applied.clone();
        assert_eq!(applied.len(), 1, "一趟只推一次");
        assert_eq!(applied[0], br#"{"marker":"from-client"}"#.to_vec());
    }
}
