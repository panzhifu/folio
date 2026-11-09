//! 局域网同步的**协议形状与客户端那一趟**：路由、报文、缺书对账、一次完整往返。
//!
//! 这一层只管「两端怎么说得上话」，不管谁去拨那个连接：
//!
//! - 服务端那一半挂在 [`super::transfer`] 那个 axum 服务器上（`tokio` 只许住在
//!   [`super::transfer`] 的边界里，见 [`super`] 的模块文档），它把这里的 DTO 解出来、
//!   用 [`crate::account`] 验过账号再答；
//! - 客户端的**编排在 [`sync_one`]**，出网口是 [`super::remote::Transport`]，与 WebDAV、
//!   翻译、检查更新同一个，于是整趟往返能在测试里塞一只假传输离线跑完；
//! - 向本机要的那几件事（书架清单、导入一本、合并写回）收在 [`LanLocal`] 这一个出口里，
//!   由装配那一层实现 —— 书库句柄、导入管线、作业池归它管，不是这一层的事。
//!
//! 与 [`crate::sync`] 的分工：那里是「一台设备的全量快照长什么样、几份快照怎么合成一份」，
//! 这里是「快照与书怎么在局域网里递给对面那台」。合并规则两边共用，一条都没重写。
//!
//! **账号 id 是句声称，不是凭据**：`/hello` 里那串 id 任何设备都能广播，所以它只用来
//! 让对方决定「值不值得跟你握手」；真正的准入是之后那对 nonce 与双向证明
//! （见 [`crate::account`]）。证明没过，任何一条数据路由都不答。

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;

use super::remote::{self, Method, Request, Response, Transport};
use crate::account::{AccountKey, Nonce, Proof, Side};
use crate::sync::{SNAPSHOT_VERSION, Snapshot};

// 清单条目的数据形状归 sync（评审稿 Stage 2.2）：读侧要它的是 `store` 的
// 「本机有哪些可同步的书」，而 store 不许认识网络模块。这里沿用旧路径，
// `net::lansync::BookEntry` 的既有写法（`net/transfer/lan.rs`、界面侧）一行不改。
pub use crate::sync::BookEntry;

/// 这一版局域网协议。两端各带在自己每一条报文里，`version` 比本机高的对端直接拒：
/// 猜字段比连不上更难查。
pub const PROTOCOL_VERSION: u32 = 1;

/// 一次局域网同步要走的几条路由。都挂在传书那个服务器上，共用那一个端口与那一道
/// 每 IP 退避的账本。
pub const ROUTE_HELLO: &str = "/api/folio/v1/hello";
pub const ROUTE_AUTH: &str = "/api/folio/v1/auth";
pub const ROUTE_MANIFEST: &str = "/api/folio/v1/manifest";
pub const ROUTE_FILE: &str = "/api/folio/v1/file";
pub const ROUTE_SNAPSHOT: &str = "/api/folio/v1/snapshot";

/// 一次连接的会话凭据时长。局域网里一次往返（握手 + 书架清单 + 拉几本缺的书 + 两份快照）
/// 在秒级就完了，给到十分钟只是为了让「拉书拉到一半屏幕熄了」不必重来。
pub const SESSION_TTL_SECS: i64 = 600;

/// 一台设备愿意在一次同步里从对面拉多少本书。超过就是对面那台把整个库摊开了，
/// 而这一次下拉不是「把我这几本补上」的意思。
pub const MAX_PULL_BOOKS: usize = 200;

/// 一本书能从对面送过来的上限。字节是流式读的，所以这一格**不是**内存的界
/// （一 GiB 的书也只是一次 256 KiB），它管的是「一次下拉愿意为一本书等多久」：
/// 家庭 WiFi 上 30–50 MB/s，一 GiB 是二三十秒，已经是「读者会以为卡住了」的那一头。
/// 与传书那条 1 GiB 的上传上限同值 —— 两个方向不该有一个比另一个更宽松。
pub const MAX_PUSH_BYTES: u64 = 1024 * 1024 * 1024;

/// `POST /hello` 的请求体：我是谁（声称）、我这一半的 nonce、我说哪一版协议。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub version: u32,
    /// 本机账号 id 的十六进制。对面拿自己算出来的那一个比。
    pub account: String,
    /// 发起方的 nonce，十六进制。与对端那个一起折成会话密钥。
    pub nonce: String,
}

/// `POST /hello` 的响应：对端那一半的 nonce、它出示的证明、这次连接的会话凭据，
/// 还有它此刻的钟。
///
/// `now` 不是装饰：两端各自的表对不上是常态（飞行模式几天、手机自己同步过一次），
/// 而局域网给了唯一一次「当场对表」的机会 —— 拿它对 [`crate::sync::Snapshot::shift_clock`]，
/// 比对那些隔着云端写下来的时间戳可靠。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloReply {
    pub version: u32,
    pub account: String,
    pub nonce: String,
    /// 对端作为接收方出示的证明（[`crate::account::Side::Responder`] 那一句）。
    pub proof: String,
    /// 之后每一条请求都要带的会话凭据。
    pub token: String,
    /// 对端的 UTC 秒。
    pub now: i64,
}

/// `POST /auth` 的请求体：发起方出示它那一句证明。过了这道门才谈数据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Auth {
    pub token: String,
    pub proof: String,
}

/// `GET /manifest` 的响应：对面那台机器上**有副本可拉**的书。
///
/// 只带 `content_hash`、大小与书名。`hash` 就是请求 `/file` 用的键，路径由对端按自己的
/// 数据库解 —— 书库目录在哪儿、副本叫什么名，都是各台自己的事，不该跨机说。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub books: Vec<BookEntry>,
}

/// 本机缺哪几本。`local_hashes` 是本机 `books.content_hash` 的集合，
/// 缺的那几本按对面给的顺序原样返回。
///
/// 大小不认识的一律不要：对面报 0 字节的条目要么是它自己算错，要么是被改过的报文，
/// 两种都不值得为它开一次连接。
///
/// 一本书的副本在库里就叫 `<哈希前缀>.<扩展名>`，内容相同即同一本 —— 所以这一格
/// 不需要书名、作者之类的模糊匹配，也不会出现「两本书合成一条」。
pub fn missing(local_hashes: &HashSet<String>, remote: &Manifest) -> Vec<BookEntry> {
    remote
        .books
        .iter()
        .filter(|entry| entry.size > 0 && !local_hashes.contains(&entry.hash))
        .take(MAX_PULL_BOOKS)
        .cloned()
        .collect()
}

/// 一次局域网同步的报表。下拉刷新结束后屏幕上那句「补了 2 本、合了 5 条」从这些数来。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// 因为不在本机而拉过来的书。
    pub pulled: usize,
    /// 拉过来但导入拒了（不是书、坏了、带 DRM）。
    pub failed: usize,
    /// 落进本机的进度 / 批注 / 书签条数，与 [`crate::sync::Report`] 同一口径。
    pub applied_positions: usize,
    pub applied_annotations: usize,
    pub applied_bookmarks: usize,
}

impl Report {
    /// 这一次有没有动过任何东西。屏幕上「没有可同步的」与「同步失败」是两句话，
    /// 靠这一格分开。
    pub fn touched_anything(&self) -> bool {
        self.pulled > 0
            || self.failed > 0
            || self.applied_positions > 0
            || self.applied_annotations > 0
            || self.applied_bookmarks > 0
    }
}

/// 书的身份那一格长什么样：64 个十六进制字符（`books.content_hash`）。
///
/// 服务端在开库之前按这一条挡（形状不对的 hash 不可能是本机任何一本书的身份），客户端在下拉
/// 之前按同一条挡（对面报进来的清单里，认不出的身份不该换来一次连接）。判据写在一处，两端共用。
pub fn is_book_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit())
}

/// 读者填进来的那一句地址 → 请求前缀 `scheme://host:port`（不带尾斜杠）。
///
/// 局域网里没人会打协议前缀：屏幕上摆的就是 `192.168.1.5:8410`。所以没写协议时补 `http` ——
/// 这一趟本来就在局域网里走明文（ [`crate::account`] 的校验码就是为这一段明文准备的）。
/// 带路径的一律不认：那说明填过来的是传书那页**上传页**的地址（后面挂着 `/?pin=…`），
/// 我们的路由不在那底下，报错比猜意图便宜。
pub fn normalize_peer(input: &str) -> Option<String> {
    let text = input.trim().trim_end_matches('/');
    if text.is_empty() {
        return None;
    }
    let (scheme, rest) = match text.split_once("://") {
        Some((scheme, rest)) => (scheme.to_ascii_lowercase(), rest),
        None => ("http".to_string(), text),
    };
    if scheme != "http" && scheme != "https" {
        return None;
    }
    // 到这儿 `rest` 里不该还有 `/`：那就是路径的开头，上面刚说了不认。
    if rest.is_empty() || rest.contains('/') || rest.contains('\\') || rest.contains('@') {
        return None;
    }
    if rest.chars().any(char::is_whitespace) {
        return None;
    }
    Some(format!("{scheme}://{rest}"))
}

/// 客户端一趟要向本机要的那几件事。与 [`crate::net::transfer::LanProvider`] 是一对：
/// 那一个管「被对面读」，这一个管「我们去读对面」。
///
/// 实现住在装配那一层（`folio-app`）：书库句柄、导入管线、作业池、每本书的副本躺在哪儿，
/// 都是它的事。这一层只按这个形状编排，于是整趟往返能在测试里拿一只假的跑完。
pub trait LanLocal: Send + Sync + 'static {
    /// 本机已有副本的书（`books.content_hash` 的集合）。
    ///
    /// 要一整份集合而不是一本一问：清单可能有几百本，而「库里有没有这一行」是一次库查，
    /// 不该是几百次。
    fn local_hashes(&self) -> HashSet<String>;

    /// 把 `url` 指向的那一本拉进本机。**导入管线归实现者**：下载只是前半趟，判重、算哈希、
    /// 落库、失败留档都是它的事。回 `true` = 这一次本机真的多了一本。
    fn pull_book(&self, url: &str) -> bool;

    /// 收下读回来的这些快照，与本机合一次、写回本机，并回**合并后的本机快照 JSON**。
    ///
    /// 那一份要当场推回对面：两台设备于是在这一趟里都收齐，不必等对面哪天也下拉一次。
    /// `remotes` 里的时间**已经搬到本机钟面上**（见 [`Peer::retime`]），实现只管合并。
    fn merge_and_apply(
        &self,
        remotes: &[Snapshot],
        now: i64,
    ) -> std::result::Result<Applied, String>;
}

/// 合并写回本机之后要交回去的那一份，连同给界面念的几个数。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    /// 合并后的本机快照，JSON 文本，原样推给对面。
    pub json: String,
    pub positions: usize,
    pub annotations: usize,
    pub bookmarks: usize,
}

/// 一条局域网往返里的失败。分档按**读者能做什么**：对面没开服务、两边的密钥不同步、
/// 版本对不上、本机这趟没走完。「连不上」那一档不细分 DNS / TLS / 超时，因为局域网里
/// 那三件事的读者动作是同一个：核对地址、走过去看一眼那台设备。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("连不上那台设备：{0}")]
    Transport(#[from] remote::Error),

    /// 那五条路由不在（404）。最常见是那一台压根没开着传书服务 —— 局域网同步寄生在它上面。
    #[error("那个地址上没有局域网同步：对面那台要把 Wi-Fi 传书开着")]
    NoService,

    #[error("对面不是同一个账号：核对两边的校验码，密钥不同步")]
    AccountMismatch,

    #[error("对面没认出我们出示的证明：两边的密钥不同步")]
    ProofRejected,

    /// 数据路由上的 403：token 不对、来源地址换了、或 `/auth` 那一步没过。
    #[error("对面不认这一次连接：再点一次同步")]
    Denied,

    #[error("对面说第 {got} 版协议，本机说第 {want} 版：两边都要升级")]
    Version { got: u32, want: u32 },

    /// 会话十分钟到期（410）。局域网里一次往返在秒级，走到这一档说明中间真的隔了很久。
    #[error("这一次连接的会话过期了：再点一次同步")]
    Expired,

    /// 429：证明反复没过，那个地址被记进退避账本（与猜配对码同一个待遇）。
    #[error("对面暂时挡住了我们这个地址：过一会儿再试")]
    Locked,

    #[error("对面的回话读不懂：{0}")]
    BadReply(String),

    #[error("本机这一趟没走完：{0}")]
    Local(String),
}

type Result<T> = std::result::Result<T, Error>;

/// 403 在三条路由上是三件不同的事，所以由调用方点名走的哪一条门。
#[derive(Clone, Copy)]
enum Door {
    /// `/hello`：声称的账号 id 对不上。
    Hello,
    /// `/auth`：我们出示的证明没过。
    Auth,
    /// 那三条数据路由：这一次连接的凭据不认。
    Data,
}

impl Door {
    fn denied(self) -> Error {
        match self {
            Door::Hello => Error::AccountMismatch,
            Door::Auth => Error::ProofRejected,
            Door::Data => Error::Denied,
        }
    }
}

/// 传输层的错 → 这一趟的错。状态码的**含义**按路由分，所以那一步交给 `door`。
fn refused(err: remote::Error, door: Door) -> Error {
    match err {
        remote::Error::Forbidden { .. } => door.denied(),
        remote::Error::NotFound => Error::NoService,
        remote::Error::Unexpected { status: 410 } => Error::Expired,
        remote::Error::Unexpected { status: 429 } => Error::Locked,
        other => Error::Transport(other),
    }
}

/// 一条回话过不过门，不过的话是哪一种失败。
///
/// 两种形状都要认：[`crate::net::remote::ureq::UreqTransport`] 把非 2xx 摊进
/// `Err(remote::Error)`，而测试里的假传输可以直接给一个 404 的 `Response`。只认一边的话，
/// 用例走的路径与真机上走的是两条，测过了也不算数。
///
/// 403 在三条路由上是三件不同的事（不认我们的声称 / 不认我们的证明 / 不认这次连接），
/// 所以由 `door` 点名是哪一条。
fn gate(response: std::result::Result<Response, remote::Error>, door: Door) -> Result<Response> {
    let response = match response {
        Ok(response) => response,
        Err(err) => return Err(refused(err, door)),
    };
    match response.status {
        200..=299 => Ok(response),
        403 => Err(door.denied()),
        404 => Err(Error::NoService),
        410 => Err(Error::Expired),
        429 => Err(Error::Locked),
        // 其余状态码带上对面的短句：`PUT /snapshot` 合并失败回 422 时，正文就是那台设备
        // 自己写的实话（同一个账号那侧的错误文案，不是外来输入）。只取 200 个字符 ——
        // 一句话的长度不该由对端的正文决定。
        other => Err(Error::BadReply(reply_text(&response, other))),
    }
}

fn reply_text(response: &Response, status: u16) -> String {
    let body = String::from_utf8_lossy(&response.body);
    let brief: String = body.trim().chars().take(200).collect();
    if brief.is_empty() {
        return format!("对面回了 {status}");
    }
    format!("对面回了 {status}：{brief}")
}

/// 一台对端：握手之后这一段连接的凭据。
pub struct Peer {
    transport: Arc<dyn Transport>,
    /// `scheme://host:port`，不带尾斜杠（由 [`normalize_peer`] 保证）。
    base: String,
    /// `/hello` 发下来的会话凭据，之后每一条都要带。
    token: String,
    /// 对面写下的时间搬到本机钟面要加的秒数：`本机钟 - 对端钟`（与
    /// [`crate::sync::Snapshot::shift_clock`] 的 `delta` 同一个定义）。
    clock_shift: i64,
}

impl Peer {
    /// 握手两趟：`/hello`（对面出示它那一句证明）+ `/auth`（我们出示我们那一句）。
    ///
    /// 没过就不往下走：数据路由在对面那一侧本来也不答，而这里每一趟都要开一次连接，
    /// 没必要拿书架清单去试探一把不同步的密钥。`now` 是本机此刻的 UTC 秒，用来当场对表。
    pub fn open(
        transport: Arc<dyn Transport>,
        base: &str,
        key: &AccountKey,
        now: i64,
    ) -> Result<Self> {
        let id = key.id().to_hex();
        let ours = Nonce::generate()
            .ok_or_else(|| Error::Local("OS 熵不可用，这次连接开不出来".into()))?;
        let hello = Hello {
            version: PROTOCOL_VERSION,
            account: id.clone(),
            nonce: ours.to_hex(),
        };
        let reply: HelloReply = post_json(
            transport.as_ref(),
            &format!("{base}{ROUTE_HELLO}"),
            &hello,
            Door::Hello,
        )?;
        if reply.version != PROTOCOL_VERSION {
            return Err(Error::Version {
                got: reply.version,
                want: PROTOCOL_VERSION,
            });
        }
        // 对面报回来的 id 必须就是本机这一把密钥派生的那一个。早一步停不是为了省一次往返
        // （会话密钥已经算出来了），是为了把「两边的密钥不同步」说成一句人话，而不是让后面
        // 那两句证明对不上时只剩一个「读不懂」。
        if reply.account != id {
            return Err(Error::AccountMismatch);
        }
        let theirs = Nonce::from_hex(&reply.nonce)
            .ok_or_else(|| Error::BadReply("nonce 不是十六进制的 16 字节".into()))?;
        let proof = Proof::from_hex(&reply.proof)
            .ok_or_else(|| Error::BadReply("证明不是十六进制的 32 字节".into()))?;
        // 派生顺序两端必须一致：**先发起方那一半**（我们的 nonce），后接收方那一半。
        let session = key.session(&ours, &theirs);
        if !session.verify(Side::Responder, &proof) {
            return Err(Error::ProofRejected);
        }
        let auth = Auth {
            token: reply.token.clone(),
            proof: session.own_proof(Side::Initiator).to_hex(),
        };
        // `/auth` 只回一个状态码：过了就 200，所以这里**不解析**回包 —— 空 body 解成
        // JSON 会报「读不懂」，那是我们自己的错，不是对面的。
        post(
            transport.as_ref(),
            &format!("{base}{ROUTE_AUTH}"),
            &auth,
            Door::Auth,
        )?;
        Ok(Self {
            transport,
            base: base.to_string(),
            token: reply.token,
            clock_shift: now.saturating_sub(reply.now),
        })
    }

    /// 对面那一份快照 → 搬到本机钟面上的那一份。
    ///
    /// 偏移量取的是 `/hello` 那一刻当场量到的那一对读数，比快照自己带的 `exported_at` 新
    /// 一两个网络往返的时间 —— 对表要的就是「现在这一刻两边各读到自己是多少」。
    pub fn retime(&self, remote: &Snapshot) -> Snapshot {
        let mut copy = remote.clone();
        copy.shift_clock(self.clock_shift);
        copy
    }

    /// `GET /manifest`：对面那台有副本可拉的书。
    pub fn manifest(&self) -> Result<Manifest> {
        let manifest: Manifest = self.get(&format!("{ROUTE_MANIFEST}?token={}", self.token))?;
        if manifest.version != PROTOCOL_VERSION {
            return Err(Error::Version {
                got: manifest.version,
                want: PROTOCOL_VERSION,
            });
        }
        Ok(manifest)
    }

    /// 那一本书的下载地址。
    ///
    /// token 与 hash 都是十六进制，拼进查询串不需要再转义；而 token 在查询里，日志侧
    /// [`crate::net::remote::strip_query`] 会掐掉整个查询串 —— 抓日志的人拿不到它。
    pub fn file_url(&self, hash: &str) -> String {
        format!(
            "{}{ROUTE_FILE}?token={}&hash={}",
            self.base, self.token, hash
        )
    }

    /// `GET /snapshot`：对面那台的全量快照（已经搬到本机钟面）。
    pub fn snapshot(&self) -> Result<Snapshot> {
        let remote: Snapshot = self.get(&format!("{ROUTE_SNAPSHOT}?token={}", self.token))?;
        if remote.version != SNAPSHOT_VERSION {
            // 更高的版本意味着字段语义可能是两套，合错了没人看得出来 —— 不猜，直说。
            return Err(Error::BadReply(format!(
                "对面的快照是第 {} 版，本机只认第 {SNAPSHOT_VERSION} 版",
                remote.version
            )));
        }
        Ok(self.retime(&remote))
    }

    /// `PUT /snapshot`：把合并后的本机快照推给对面，对面当场合进它自己那一份。
    ///
    /// 回包只有状态码：对面合进来的条数在本机那一步已经数过（见 [`LanLocal`]），
    /// 所以这一句不解析 body，只问「收下了没有」。
    pub fn push_snapshot(&self, json: &str) -> Result<()> {
        let url = self.url(&format!("{ROUTE_SNAPSHOT}?token={}", self.token));
        let req = Request::new(Method::Put, url)
            .header("Content-Type", "application/json")
            .body(json.as_bytes().to_vec());
        self.send(req)?;
        Ok(())
    }

    /// 发一条数据路由上的请求：token 之外没有别的内容要带，而 403 在这里的意思是
    /// 「这一趟连接不算数了」，不是「密钥不同步」。
    fn send(&self, req: Request) -> Result<Response> {
        gate(self.transport.request(req), Door::Data)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = self.send(Request::new(Method::Get, self.url(path)))?;
        json_of(response)
    }
}

/// 与一台对端走完整趟：握手 → 补本机缺的书 → 双向换快照。
///
/// 顺序要紧的是**先拉书、后合快照**：对面那份快照里本来就有那几本书的进度与批注，
/// 书落进本机之后合并才认得出它们的 `content_hash` —— 反过来的话这一趟白合，
/// 那些记录要等下一次同步才落。
///
/// 书是单向的：这一趟只补**本机缺的**那几本。对面缺的要等它自己下拉一次
/// （或者把这一趟在对面那台上再点一遍）。快照是两个方向的，一次就收敛。
pub fn sync_one(
    local: &dyn LanLocal,
    transport: Arc<dyn Transport>,
    peer: &str,
    key: &AccountKey,
    now: i64,
) -> Result<Report> {
    let base = normalize_peer(peer).ok_or_else(|| Error::BadReply("那个地址读不出来".into()))?;
    let peer = Peer::open(transport, &base, key, now)?;
    let mut report = Report::default();

    let manifest = peer.manifest()?;
    let owned = local.local_hashes();
    for entry in missing(&owned, &manifest) {
        if !is_book_hash(&entry.hash) {
            continue;
        }
        if local.pull_book(&peer.file_url(&entry.hash)) {
            report.pulled += 1;
        } else {
            report.failed += 1;
        }
    }

    let remote = peer.snapshot()?;
    let applied = local
        .merge_and_apply(&[remote], now)
        .map_err(Error::Local)?;
    report.applied_positions = applied.positions;
    report.applied_annotations = applied.annotations;
    report.applied_bookmarks = applied.bookmarks;
    peer.push_snapshot(&applied.json)?;
    Ok(report)
}

/// POST 一条 JSON，回**过了门的响应**。
///
/// 只有握手那两条走这一句，而它们的 403 各有含义（一个不认声称、一个不认证明），所以 `door`
/// 由调用方点名。
fn post(
    transport: &dyn Transport,
    url: &str,
    body: &impl Serialize,
    door: Door,
) -> Result<Response> {
    let payload =
        serde_json::to_vec(body).map_err(|err| Error::BadReply(format!("报文拼不出来：{err}")))?;
    let req = Request::new(Method::Post, url.to_string())
        .header("Content-Type", "application/json")
        .body(payload);
    gate(transport.request(req), door)
}

/// POST 一条 JSON 并把回包解成 `T`。回包形状解不出来就是协议对不上（`BadReply`），
/// 不是「网络不好」—— 这两件事在界面上必须是两句话。
fn post_json<T: DeserializeOwned>(
    transport: &dyn Transport,
    url: &str,
    body: &impl Serialize,
    door: Door,
) -> Result<T> {
    json_of(post(transport, url, body, door)?)
}

fn json_of<T: DeserializeOwned>(response: Response) -> Result<T> {
    serde_json::from_slice(&response.body).map_err(|err| Error::BadReply(err.to_string()))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};
    use std::sync::{Arc, Mutex};

    use crate::account::{AccountKey, Nonce, Proof, Side};
    use crate::net::remote::{self, Method, Request, Response, Sink, Transport};
    use crate::position::Locator;
    use crate::sync::{BookSnapshot, PositionRecord, SNAPSHOT_VERSION, Snapshot};

    use super::{
        Applied, Auth, BookEntry, Error, Hello, HelloReply, LanLocal, MAX_PULL_BOOKS, Manifest,
        PROTOCOL_VERSION, ROUTE_AUTH, ROUTE_FILE, ROUTE_HELLO, ROUTE_MANIFEST, ROUTE_SNAPSHOT,
        Report, is_book_hash, missing, normalize_peer, sync_one,
    };

    fn manifest(entries: Vec<BookEntry>) -> Manifest {
        Manifest {
            version: 1,
            books: entries,
        }
    }

    fn entry(hash: &str, size: u64) -> BookEntry {
        BookEntry {
            hash: hash.to_string(),
            size,
            title: format!("《{hash}》"),
        }
    }

    /// 身份就是内容：本机有那一条哈希就不该再拉一遍，哪怕对面那本的名字完全不同。
    #[test]
    fn a_book_the_local_library_already_has_is_not_pulled_again() {
        let local: HashSet<String> = ["aaaa".to_string(), "bbbb".to_string()]
            .into_iter()
            .collect();
        let remote = manifest(vec![entry("aaaa", 10), entry("cccc", 20)]);

        let missing = missing(&local, &remote);
        assert_eq!(
            missing.iter().map(|e| e.hash.as_str()).collect::<Vec<_>>(),
            vec!["cccc"]
        );
    }

    #[test]
    fn an_empty_library_takes_everything_the_peer_offers() {
        let local = HashSet::new();
        let remote = manifest(vec![entry("aaaa", 10), entry("bbbb", 20)]);
        assert_eq!(
            missing(&local, &remote).len(),
            2,
            "对账不该在本机为空时漏掉任何一本"
        );
    }

    /// 报 0 字节的条目不认：要么对面算错，要么报文被改过，两种都不值得开连接。
    #[test]
    fn a_zero_size_entry_is_not_a_book() {
        let local = HashSet::new();
        let remote = manifest(vec![entry("aaaa", 0), entry("bbbb", 1)]);
        let got = missing(&local, &remote);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].hash, "bbbb");
    }

    /// 对面把整库摊开时封顶，别让一次下拉变成搬完一本书库；截的是顺序后面的，
    /// 剩下的下一轮再来。
    #[test]
    fn a_huge_manifest_is_cut_at_the_cap() {
        let local = HashSet::new();
        let many: Vec<BookEntry> = (0..MAX_PULL_BOOKS + 50)
            .map(|i| entry(&format!("{i:04}"), 1))
            .collect();
        let got = missing(&local, &manifest(many));
        assert_eq!(got.len(), MAX_PULL_BOOKS);
        assert_eq!(got.first().unwrap().hash, "0000", "该留的是前面的那些");
    }

    #[test]
    fn the_report_distinguishes_nothing_to_do_from_failure() {
        assert!(
            !Report::default().touched_anything(),
            "全零的报表就是「这轮没动任何东西」"
        );
        let pulled = Report {
            pulled: 1,
            ..Default::default()
        };
        assert!(pulled.touched_anything());
        // 「拉过来但导入拒了」也算动过话：屏幕上那句得说出这趟有书没进来，
        // 而不是安静地报「没有可同步的」。
        let refused_import = Report {
            failed: 1,
            ..Default::default()
        };
        assert!(refused_import.touched_anything());
        // 而对侧握手没过**不进报表**：那是一条 `Err`，走「同步失败」那一句话。
        // 报表里再也没有「被挡掉的地址数」这一格，因为它不再是这一层的输出。
    }

    /// 报文要能原样过 JSON：两端各写各的，解不回来就是协议对不上。
    #[test]
    fn the_wire_shapes_round_trip_through_json() {
        let hello = super::Hello {
            version: super::PROTOCOL_VERSION,
            account: "0123456789abcdef".into(),
            nonce: "beef".into(),
        };
        let text = serde_json::to_string(&hello).unwrap();
        assert_eq!(
            text,
            r#"{"version":1,"account":"0123456789abcdef","nonce":"beef"}"#
        );

        let reply = super::HelloReply {
            version: super::PROTOCOL_VERSION,
            account: "ff".into(),
            nonce: "aa".into(),
            proof: "bb".into(),
            token: "cc".into(),
            now: 1_800_000_000,
        };
        let back: super::HelloReply =
            serde_json::from_str(&serde_json::to_string(&reply).unwrap()).unwrap();
        assert_eq!(back, reply);
    }

    /// 读对面报文的容错：多出来的字段（将来加了新东西的老对端）不能把这一端顶掉。
    #[test]
    fn a_manifest_with_unknown_fields_still_parses() {
        let got: Manifest = serde_json::from_str(
            r#"{"version":1,"books":[{"hash":"aa","size":7,"title":"x","future":true}],"extra":1}"#,
        )
        .unwrap();
        assert_eq!(got.books.len(), 1);
        assert_eq!(got.books[0].size, 7);
    }

    /// 地址那一格容许的几种写法与不认的那几种。
    ///
    /// 屏幕上摆出来的是 `192.168.1.5:8410`（传书那一行就是这个），所以没人会打协议前缀；
    /// 而**带路径的一律不认** —— 那说明抄过来的是上传页的地址（后面挂着 `/?pin=…`），
    /// 我们的路由不在那底下，报错比猜意图便宜。
    #[test]
    fn a_peer_address_is_read_the_way_a_reader_would_type_it() {
        assert_eq!(
            normalize_peer("192.168.1.5:8410").as_deref(),
            Some("http://192.168.1.5:8410"),
            "没写协议就补 http：这一趟本来在局域网里走明文"
        );
        assert_eq!(
            normalize_peer("  http://10.0.0.9:8410/  ").as_deref(),
            Some("http://10.0.0.9:8410"),
            "首尾空白与尾巴上的斜杠都是手误，不是另一种地址"
        );
        assert_eq!(
            normalize_peer("https://nas.local:8420").as_deref(),
            Some("https://nas.local:8420"),
            "写了协议就照它来，不许悄悄换成 http"
        );
        for rejected in [
            "",
            "   ",
            "192.168.1.5:8410/?pin=12345678",
            "ftp://192.168.1.5",
            "http://user:pass@192.168.1.5:8410",
            "http://192.168.1.5:841 0",
        ] {
            assert_eq!(
                normalize_peer(rejected),
                None,
                "「{rejected}」不该被读成一个对端地址"
            );
        }
    }

    /// 书的身份那一格的形状：服务端与客户端共用这一条判据，所以只写一处、两处都测。
    #[test]
    fn a_books_identity_is_sixty_four_hex_characters() {
        assert!(is_book_hash(&"a".repeat(64)));
        assert!(is_book_hash(&"0123456789abcdef".repeat(4)));
        assert!(!is_book_hash(&"a".repeat(63)), "短一格就不是 SHA-256");
        assert!(!is_book_hash(&"a".repeat(65)), "长一格也不是");
        assert!(!is_book_hash(""), "空串不是身份");
        assert!(
            !is_book_hash(&format!("{}z", "a".repeat(63))),
            "非十六进制字符把整串作废，而不是容忍一位"
        );
        assert!(
            !is_book_hash("../../etc/passwd"),
            "路径成分到不了任何地方：这一串要先过形状这一关"
        );
    }

    /// 一次完整往返：握手 → 拉走本机缺的那一本 → 读回对面快照并**搬到本机钟面** →
    /// 把合并后的那一份推回去。三条请求的 URL 都带着这次的 token，本机这一侧只看见四件事。
    #[test]
    fn a_full_round_pulls_the_missing_book_and_pushes_the_merged_snapshot() {
        let key = key();
        let owned = hash_of(OWNED);
        let peer = Arc::new(FakePeer::new(key, PEER_CLOCK));
        let local = Arc::new(FakeLocal::new([owned.clone()], "MERGED-JSON"));
        // 本机钟比对面慢一千秒。
        let report = sync_one(
            local.as_ref(),
            clone_transport(&peer),
            "192.168.1.9:8410",
            &key,
            OUR_CLOCK,
        )
        .expect("一次正常的往返不该失败");

        assert_eq!(report.pulled, 1, "对面有而本机没有的那一本该拉过来");
        assert_eq!(report.failed, 0);
        assert_eq!(report.applied_bookmarks, 3, "计数从本机合并那一步原样交回");

        // 下拉的 URL：主机与端口来自填的那一句，路由与 token 来自这次握手，而 hash 是对面报的。
        let pulls = local.pulls.lock().unwrap();
        assert_eq!(pulls.len(), 1);
        assert!(
            pulls[0].starts_with("http://192.168.1.9:8410"),
            "{}",
            pulls[0]
        );
        assert!(pulls[0].contains(ROUTE_FILE), "{}", pulls[0]);
        assert!(
            pulls[0].contains(&format!("token={}", peer.token())),
            "{}",
            pulls[0]
        );
        assert!(
            pulls[0].contains(&format!("hash={}", hash_of(MISSING))),
            "{}",
            pulls[0]
        );
        // 本机已经有的那一本不该再问一次 —— 身份是内容，书名不同也一样。
        assert!(
            !pulls.iter().any(|url| url.contains(&owned)),
            "清单里那一本本机有的书被重复拉了"
        );
        drop(pulls);

        // 推回去的是**合并后的本机那一份**，而不是原样转发的对面那一份。
        let pushed = peer.pushed.lock().unwrap();
        assert_eq!(pushed.len(), 1, "一趟只推一次");
        assert_eq!(pushed[0], "MERGED-JSON");
        assert!(
            peer.seen
                .lock()
                .unwrap()
                .iter()
                .any(|req| req.method == Method::Put
                    && req.url.contains(ROUTE_SNAPSHOT)
                    && req.url.contains(&peer.token())),
            "PUT 也要带这次的会话凭据"
        );
    }

    /// 对表：对面写下的时间在交到本机合并之前，必须已经搬到本机钟面上。
    ///
    /// 这一格错了，合并挑出来的赢者就是「谁的钟快」而不是「谁改得晚」，而屏幕上看不出来。
    #[test]
    fn remote_timestamps_are_moved_onto_the_local_clock_before_merging() {
        let key = key();
        let peer = Arc::new(FakePeer::new(key, PEER_CLOCK));
        let local = Arc::new(FakeLocal::new([hash_of(OWNED)], "MERGED-JSON"));
        sync_one(
            local.as_ref(),
            clone_transport(&peer),
            "192.168.1.9:8410",
            &key,
            OUR_CLOCK,
        )
        .unwrap();

        let merged = local.merged.lock().unwrap();
        assert_eq!(merged.len(), 1, "本机这一侧只该收到一份对面快照");
        let book = &merged[0].books[&hash_of(MISSING)];
        assert_eq!(
            book.position.as_ref().unwrap().updated_at,
            900,
            "对面钟上的一千九百，搬到慢一千秒的本机钟面上是九百"
        );
        assert_eq!(
            merged[0].exported_at, OUR_CLOCK,
            "导出的读数也跟着搬：它接下来要参加合并的比较"
        );
        // `now` 交回本机合并那一步的，也是本机这一个钟。
        assert_eq!(local.merged_at.lock().unwrap().as_slice(), &[OUR_CLOCK]);
    }

    /// 两边密钥不同步时，握手第一步就停：任何一条数据路由都不该被问一次。
    ///
    /// 两种形状都测：对面**声称**了别的 id（回包里那一格不是本机能算出来的），以及对面
    /// 干脆按我们的声称回了 403（它自己算出来的 id 与本机的不同）。
    #[test]
    fn a_peer_that_is_not_the_same_account_stops_at_the_first_door() {
        for (shape, peer) in [
            (
                "回的是别人的账号 id",
                FakePeer::new(key(), PEER_CLOCK).wrong_account(),
            ),
            ("手里是另一把密钥", FakePeer::new(other_key(), PEER_CLOCK)),
        ] {
            let key = key();
            let peer = Arc::new(peer);
            let local = Arc::new(FakeLocal::new(Vec::new(), "MERGED-JSON"));
            let err = sync_one(
                local.as_ref(),
                clone_transport(&peer),
                "192.168.1.9:8410",
                &key,
                OUR_CLOCK,
            )
            .expect_err("id 不是凭据，但它对不上就没有必要往下走（{shape}）");
            assert!(matches!(err, Error::AccountMismatch), "{shape}：{err}");
            assert!(
                local.pulls.lock().unwrap().is_empty(),
                "{shape}：不是同一个账号就不能拉走对面任何一本书"
            );
        }
    }

    /// 证明没过就连 `/auth` 都过不去：只走了 `/hello` 那一条。
    #[test]
    fn an_unproven_peer_never_reaches_the_data_routes() {
        let key = key();
        let peer = Arc::new(FakePeer::new(key, PEER_CLOCK).wrong_proof());
        let local = Arc::new(FakeLocal::new(Vec::new(), "MERGED-JSON"));
        let err = sync_one(
            local.as_ref(),
            clone_transport(&peer),
            "192.168.1.9:8410",
            &key,
            OUR_CLOCK,
        )
        .expect_err("对面出的是别的密钥算的证明");
        assert!(matches!(err, Error::ProofRejected), "{err}");

        let seen = peer.seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "只走了 /hello 那一条：证明没过就不该有下文");
        assert!(seen[0].url.contains(ROUTE_HELLO));
    }

    /// 那一台压根没开着局域网同步（路由不在）：说清楚该去开什么，而不是报「读不懂」。
    #[test]
    fn a_peer_without_the_routes_is_not_listening_for_sync() {
        let key = key();
        let peer = Arc::new(FakePeer::new(key, PEER_CLOCK).without_routes());
        let local = Arc::new(FakeLocal::new(Vec::new(), "MERGED-JSON"));
        let err = sync_one(
            local.as_ref(),
            clone_transport(&peer),
            "192.168.1.9:8410",
            &key,
            OUR_CLOCK,
        )
        .expect_err("404 是一条答复，不是一次成功");
        assert!(matches!(err, Error::NoService), "{err}");
        assert!(local.pulls.lock().unwrap().is_empty());
    }

    /// 对面报进来的清单里，形状不对的身份换不到一次连接：它不可能是本机认识的任何一本书。
    #[test]
    fn a_manifest_entry_that_is_not_a_hash_is_not_pulled() {
        let key = key();
        let peer = Arc::new(FakePeer::new(key, PEER_CLOCK).with_manifest(vec![
            entry(&hash_of(MISSING), 10),
            entry("../../etc/passwd", 10),
            entry("短", 10),
        ]));
        let local = Arc::new(FakeLocal::new(Vec::new(), "MERGED-JSON"));
        let report = sync_one(
            local.as_ref(),
            clone_transport(&peer),
            "192.168.1.9:8410",
            &key,
            OUR_CLOCK,
        )
        .unwrap();

        assert_eq!(report.pulled, 1, "只该拉走那一个真的身份");
        assert_eq!(report.failed, 0, "形状不对的那两条根本没进下拉，不算失败");
    }

    /// 会话过期（410）与那个地址被暂时挡下（429）是两句不同的话：前者重来一次就好，
    /// 后者重来一次只会更快进罚期。
    #[test]
    fn a_gone_session_and_a_locked_address_say_different_things() {
        let key = key();
        for (status, expected) in [(410, "过期"), (429, "挡住")] {
            let peer = Arc::new(FakePeer::new(key, PEER_CLOCK).data_replies(status));
            let local = Arc::new(FakeLocal::new(Vec::new(), "MERGED-JSON"));
            let err = sync_one(
                local.as_ref(),
                clone_transport(&peer),
                "192.168.1.9:8410",
                &key,
                OUR_CLOCK,
            )
            .expect_err("状态码 {status} 不是一次成功");
            let text = err.to_string();
            assert!(
                text.contains(expected),
                "状态码 {status} 要说成含「{expected}」的那一句话，实际是：{text}"
            );
        }
    }

    // --- 测试用的两只假：一台对端（真算证明）、一台本机（记下被要求做的事） -------------

    const OUR_CLOCK: i64 = 1_000;
    /// 对端自己的钟：故意比本机快一千秒，对表那一格才有东西可测。
    const PEER_CLOCK: i64 = 2_000;
    /// 对面有而本机没有的那一本（[`hash_of`] 之后才是真形状）。
    const MISSING: &str = "b";
    /// 两边都有的那一本：换了书名也不该被再拉一遍。
    const OWNED: &str = "k";

    fn key() -> AccountKey {
        AccountKey::from_hex("00112233445566778899aabbccddeeff").expect("测试里的密钥形状必定合法")
    }

    fn other_key() -> AccountKey {
        AccountKey::from_hex("ffeeddccbbaa99887766554433221100").expect("同上")
    }

    fn clone_transport(peer: &Arc<FakePeer>) -> Arc<dyn Transport> {
        Arc::clone(peer) as Arc<dyn Transport>
    }

    /// 把测试用的短哈希补成 64 个十六进制字符：`is_book_hash` 认的是真形状，用例里写 `"b"`
    /// 只是为了读得出是哪一本。
    fn hash_of(short: &str) -> String {
        format!("{:0<64}", short)
    }

    /// 一台**真的会算**的对端：握手按 [`crate::account`] 那套数学现出回包，数据路由照手里的
    /// 清单与快照答。
    ///
    /// 现算而不是手抄一串字节：证明、nonce、派生顺序是协议本身，抄死了等于把实现与测试钉在
    /// 同一份字节上 —— 改了域分隔串或换nonce 的顺序都不会红，而那正是两端连不上的第一号成因。
    struct FakePeer {
        key: AccountKey,
        /// 对端自己的钟，故意与 [`OUR_CLOCK`] 差一千秒：对表那一格要有东西可测。
        clock: i64,
        manifest: Vec<BookEntry>,
        snapshot: Snapshot,
        seen: Mutex<Vec<Request>>,
        pushed: Mutex<Vec<String>>,
        /// 这次握手的会话密钥，`/auth` 那一趟用它验对面出示的证明。
        session: Mutex<Option<crate::account::Session>>,
        token: Mutex<Option<String>>,
        proven: Mutex<bool>,
        /// 三档故意使坏：回不过的证明、回别人的 id、压根没有这几条路由。
        wrong_proof: bool,
        wrong_account: bool,
        /// 数据路由直接回这个状态码（404 = 没开局域网同步，410/429 = 门开着但不认）。
        data_status: Option<u16>,
    }

    impl FakePeer {
        fn new(key: AccountKey, clock: i64) -> Self {
            let mut books = BTreeMap::new();
            books.insert(
                hash_of(MISSING),
                BookSnapshot {
                    position: Some(PositionRecord {
                        mode: "paged".into(),
                        locator: Locator {
                            chapter: 3,
                            offset: 40,
                        },
                        // 对面钟上的「刚刚」。
                        updated_at: clock - 100,
                    }),
                    ..BookSnapshot::default()
                },
            );
            Self {
                key,
                clock,
                manifest: vec![entry(&hash_of(MISSING), 10), entry(&hash_of(OWNED), 10)],
                snapshot: Snapshot {
                    version: SNAPSHOT_VERSION,
                    device_id: "dev-peer".into(),
                    exported_at: clock,
                    books,
                },
                seen: Mutex::new(Vec::new()),
                pushed: Mutex::new(Vec::new()),
                session: Mutex::new(None),
                token: Mutex::new(None),
                proven: Mutex::new(false),
                wrong_proof: false,
                wrong_account: false,
                data_status: None,
            }
        }

        /// 回一句别的密钥算出来的证明：形状全对，验不过。
        fn wrong_proof(mut self) -> Self {
            self.wrong_proof = true;
            self
        }

        /// 回一个别人那把密钥派生的账号 id：它手里不是同一把钥匙。
        fn wrong_account(mut self) -> Self {
            self.wrong_account = true;
            self
        }

        /// 那台机器上没有这几条路由（传书开着、局域网同步没开）。
        fn without_routes(mut self) -> Self {
            self.data_status = Some(404);
            self
        }

        /// 门开着，但每一条数据路由都回这一个状态码。
        fn data_replies(mut self, status: u16) -> Self {
            self.data_status = Some(status);
            self
        }

        fn with_manifest(mut self, manifest: Vec<BookEntry>) -> Self {
            self.manifest = manifest;
            self
        }

        fn token(&self) -> String {
            self.token
                .lock()
                .unwrap()
                .clone()
                .expect("用例应当在握手之后再问 token")
        }

        fn json<T: serde::Serialize>(value: &T) -> Response {
            Response::new(
                200,
                serde_json::to_vec(value).expect("测试里的报文必定能序列化"),
            )
        }

        fn hello(&self, body: Hello) -> Response {
            if self.data_status == Some(404) {
                // 「那五条路由一概 404」就是这一种：没开局域网同步的传书服务器。
                return Response::new(404, Vec::new());
            }
            if body.version != PROTOCOL_VERSION {
                return Response::new(400, Vec::new());
            }
            if body.account != self.key.id().to_hex() {
                return Response::new(403, Vec::new());
            }
            let Some(initiator) = Nonce::from_hex(&body.nonce) else {
                return Response::new(400, Vec::new());
            };
            let responder = Nonce::generate().expect("OS 熵在测试里必定可用");
            let account = self.key.session(&initiator, &responder);
            let proof = if self.wrong_proof {
                // 另一把密钥算出来的那一句：长度与形状都对，就是验不过。
                other_key()
                    .session(&initiator, &responder)
                    .own_proof(Side::Responder)
            } else {
                account.own_proof(Side::Responder)
            };
            let token = responder.to_hex();
            *self.session.lock().unwrap() = Some(account);
            *self.token.lock().unwrap() = Some(token.clone());
            let reply = HelloReply {
                version: PROTOCOL_VERSION,
                account: if self.wrong_account {
                    other_key().id().to_hex()
                } else {
                    self.key.id().to_hex()
                },
                nonce: responder.to_hex(),
                proof: proof.to_hex(),
                token,
                now: self.clock,
            };
            Self::json(&reply)
        }

        fn auth(&self, body: Auth) -> Response {
            let sessions = self.session.lock().unwrap();
            let Some(session) = sessions.as_ref() else {
                return Response::new(403, Vec::new());
            };
            let proven = Proof::from_hex(&body.proof)
                .is_some_and(|proof| session.verify(Side::Initiator, &proof));
            if proven {
                *self.proven.lock().unwrap() = true;
                return Response::new(200, Vec::new());
            }
            Response::new(403, Vec::new())
        }

        /// 三条数据路由：先认 token 与「`/auth` 过了没有」，再答内容。
        fn data(&self, req: &Request) -> Response {
            if let Some(status) = self.data_status {
                return Response::new(status, Vec::new());
            }
            let wanted = format!("token={}", self.token());
            let carried = req.url.split('?').nth(1).unwrap_or_default().to_string();
            if carried != wanted || !*self.proven.lock().unwrap() {
                return Response::new(403, Vec::new());
            }
            if req.url.contains(ROUTE_MANIFEST) {
                return Self::json(&Manifest {
                    version: PROTOCOL_VERSION,
                    books: self.manifest.clone(),
                });
            }
            if req.method == Method::Get && req.url.contains(ROUTE_SNAPSHOT) {
                return Self::json(&self.snapshot);
            }
            if req.method == Method::Put && req.url.contains(ROUTE_SNAPSHOT) {
                self.pushed
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&req.body).to_string());
                return Response::new(200, Vec::new());
            }
            Response::new(404, Vec::new())
        }
    }

    impl Transport for FakePeer {
        fn request(&self, req: Request) -> remote::Result<Response> {
            self.seen.lock().unwrap().push(req.clone());
            let response = if req.url.contains(ROUTE_HELLO) {
                serde_json::from_slice::<Hello>(&req.body)
                    .map(|body| self.hello(body))
                    .unwrap_or_else(|_| Response::new(400, Vec::new()))
            } else if req.url.contains(ROUTE_AUTH) {
                serde_json::from_slice::<Auth>(&req.body)
                    .map(|body| self.auth(body))
                    .unwrap_or_else(|_| Response::new(400, Vec::new()))
            } else {
                self.data(&req)
            };
            // 假传输把状态码原样交出去（`UreqTransport` 会把非 2xx 摊进 `Err`），所以这里
            // 测的是 [`gate`] 认得的**另一种**形状。
            Ok(response)
        }

        /// 这一趟不经传输层拉书：客户端只交一个 URL 出去，下载归本机的导入管线。
        fn download(
            &self,
            _req: Request,
            _sink: &mut dyn Sink,
            _cancel: &remote::Cancel,
        ) -> remote::Result<u64> {
            Err(remote::Error::Transport(
                "假对端不直接送字节：下拉归导入管线".to_string(),
            ))
        }
    }

    /// 一台「本机」：记下被要求做的事，合并那一步交出手里现成的一份 JSON 与几个计数。
    struct FakeLocal {
        /// 本机**已经有副本**的那些书的内容哈希 —— 不是文件名，不是本地行 id。
        owned: HashSet<String>,
        json: String,
        merged: Mutex<Vec<Snapshot>>,
        merged_at: Mutex<Vec<i64>>,
        pulls: Mutex<Vec<String>>,
    }

    impl FakeLocal {
        fn new(owned: impl IntoIterator<Item = String>, json: &str) -> Self {
            Self {
                owned: owned.into_iter().collect(),
                json: json.to_string(),
                merged: Mutex::new(Vec::new()),
                merged_at: Mutex::new(Vec::new()),
                pulls: Mutex::new(Vec::new()),
            }
        }
    }

    impl LanLocal for FakeLocal {
        fn local_hashes(&self) -> HashSet<String> {
            self.owned.clone()
        }

        fn pull_book(&self, url: &str) -> bool {
            self.pulls.lock().unwrap().push(url.to_string());
            true
        }

        fn merge_and_apply(
            &self,
            remotes: &[Snapshot],
            now: i64,
        ) -> std::result::Result<Applied, String> {
            *self.merged.lock().unwrap() = remotes.to_vec();
            *self.merged_at.lock().unwrap() = vec![now];
            Ok(Applied {
                json: self.json.clone(),
                positions: 1,
                annotations: 2,
                bookmarks: 3,
            })
        }
    }
}
