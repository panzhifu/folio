//! 远程书库：书在另一台机器上时要走的那一层。2026-11-12 阶段三扩容后是**网络面**：
//! 出网的传输与各协议（OPDS / 更新检查 / 会员兑换 / 网盘引擎），加局域网的**入网**
//! 两族（传书的 axum 服务器、同步的 TCP 协议）。
//!
//! 这个 crate 定义**传输**（[`Transport`]）与请求/响应形状，加各协议自己的机制；
//! 不含任何 `gpui` 类型，也不认识书库与配置 —— 消费方（folio-core 的入口层）各自把
//! 自己的参数递进来。**出网实现在这里**（[`ureq::UreqTransport`]），它只需要一个阻塞的
//! HTTP 客户端；app 侧只是持有它、把调用放到后台线程上。所以这里的口径是
//! 「这一层既定义形状也发请求，app 只决定什么时候发」—— `App::http_client()` 那条路
//! **没有**被用上，别再照旧注释去 app 里找实现。
//!
//! `tokio` / `axum` 随传书那族进来（原 folio-core::net::transfer，服务器那一半本来
//! 就站在它们上面），被 [`transfer`] 关在自己家里：出网的那一半照旧全同步，不需要
//! 任何执行器。
//!
//! 拆成 trait 之后，协议里那些真正容易踩空的规则 —— 路径逐段转义、`Depth: 1` 的语义
//! 不齐、207 多状态里每个资源自己的状态码、`href` 的基准各家不同 —— 都能在
//! 无网络、无窗口、假传输的条件下测（见 [`dav`] 的测试）。

pub mod cloud;
pub mod dav;
pub mod lansync;
pub mod opds;
pub mod redeem;
pub mod retry;
pub mod transfer;
pub mod update;
pub mod ureq;

use std::io::{Read, Seek};

/// 一层「把请求发出去、把响应拿回来」的能力。
///
/// 方法只有两个，差别在响应体去哪儿：小东西（PROPFIND 的 XML、备份的 JSON）整个进内存，
/// 一本几百多万字的全本 epub 不能。
pub trait Transport: Send + Sync + 'static {
    fn request(&self, req: Request) -> Result<Response>;

    /// 流式下载，响应体按块交给 `sink`。
    ///
    /// 契约：无论成功、失败还是取消，`Sink` 的 `finish` 与 `abort` **恰好调一个** ——
    /// 临时文件要么变成正式文件，要么被清掉，不会留下第三种状态。
    /// 取消不是抢占，只在块与块之间检查，所以最多晚一个块生效。
    ///
    /// **同步签名是有意的**：管线的作业跑在后台线程上，「一个请求流到底」的形状
    /// 用阻塞读最直白（见 folio-core 的 `ingest::import`）；[`UreqTransport`] 就是为此而来的实现。
    fn download(&self, req: Request, sink: &mut dyn Sink, cancel: &Cancel) -> Result<u64>;

    /// 带请求体上传。默认把源整个读进内存再走 [`Transport::request`]；能流式发的实现
    /// 应覆盖它 —— 一本几百 MB 的书不该先变成一块 `Vec<u8>` 才出网。
    fn upload(&self, req: Request, source: &mut dyn Read, _len: u64) -> Result<Response> {
        let mut body = Vec::new();
        source.read_to_end(&mut body)?;
        self.request(req.body(body))
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 请求根本没成为一次 HTTP 往返（DNS、连接、TLS、超时都在这一档）。
    #[error("连不上：{0}")]
    Transport(String),

    /// 401 且 `WWW-Authenticate` 里只给了 Digest。
    ///
    /// 一期只发 Basic：接 Digest 要把 401 挑战原样带回去再算一次 `md5` 摘要，是另一套
    /// 往返逻辑，还得为它多挂一个依赖。这类服务器（群晖、老 Apache 的默认配置）确实存在，
    /// 所以给它一条**单独**的错，而不是混进「认证被拒」里让用户对着密码框发呆。
    #[error("这台服务器要求 Digest 认证，当前只支持 Basic（请把地址换成 https，并使用应用密码）")]
    DigestRequired,

    #[error("认证被拒（{status}）：请核对地址、用户名与密码")]
    Unauthorized { status: u16 },

    #[error("没有权限（{status}）")]
    Forbidden { status: u16 },

    #[error("这个路径不存在（404）")]
    NotFound,

    /// 服务器回了 405：它接了连接，但不认 WebDAV 这套方法。
    ///
    /// 单独一条是因为最常见的成因和「没权限」「找不到」完全不同 —— Base 填成了网页地址或
    /// API 根。报成 404 会让人去翻目录，而真正该翻的是地址栏。
    #[error("这个地址不接受 WebDAV 请求（405）：请确认它指向 WebDAV 入口，而不是网页或 API 根")]
    NotWebDav,

    #[error("服务器返回 {status}")]
    Unexpected { status: u16 },

    #[error("响应读不懂：{0}")]
    Malformed(String),

    #[error("已取消")]
    Cancelled,

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Propfind,
    Mkcol,
    Delete,
}

impl Method {
    pub fn as_str(&self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            // WebDAV 的方法名不在 HTTP 标准方法里，得原样发出去。
            Method::Propfind => "PROPFIND",
            Method::Mkcol => "MKCOL",
            Method::Delete => "DELETE",
        }
    }
}

/// 一条已经拼好的请求：URL 已逐段转义，头已带齐。实现只管发，不再碰字符串。
///
/// `Debug` 是**手写**的（见下面的 `impl`）：这条结构里三处都可能带着凭据 —— URL 查询串
/// （百度那套把 `access_token` 拼在 query 上）、`Authorization` 头（WebDAV 的 Basic）、
/// 请求体（阿里云盘换 token 是 JSON POST，里面就有 `client_secret` 与 `code`）。
/// 派生出来的 `Debug` 会把它们原样交给任何一句 `tracing::debug!("{req:?}")`。
#[derive(Clone)]
pub struct Request {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn new(method: Method, url: String) -> Self {
        Self {
            method,
            url,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }

    /// 打日志用的 URL：查询串里的凭据换成 `***`，其余原样。
    pub fn redacted_url(&self) -> String {
        redact_url(&self.url)
    }
}

/// 手写的 `Debug`：**只给形状，不给凭据**。
///
/// 三条规矩：
/// - URL 过 [`redact_url`]（username:password@ 与 `access_token=` 一类参数都换成 `***`）；
/// - 头部的值一律不进输出（`Authorization` 的值是 base64 的 `user:password`，最不该进日志），
///   只留名字；
/// - 请求体只报字节数 —— 它是 JSON 的时候里面就有 `client_secret`。
impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let headers: Vec<&str> = self.headers.iter().map(|(name, _)| name.as_str()).collect();
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("url", &redact_url(&self.url))
            .field("headers", &headers)
            .field("body", &format_args!("<{} bytes>", self.body.len()))
            .finish()
    }
}

/// 响应体同样不进 `Debug`：换 token 的应答里就有 `access_token` / `refresh_token`。
#[derive(Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let headers: Vec<&str> = self.headers.iter().map(|(name, _)| name.as_str()).collect();
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("headers", &headers)
            .field("body", &format_args!("<{} bytes>", self.body.len()))
            .finish()
    }
}

impl Response {
    pub fn new(status: u16, body: Vec<u8>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body,
        }
    }

    /// 取一个响应头。HTTP 头名不区分大小写，而各家实现的写法并不统一
    /// （`WWW-Authenticate` / `www-authenticate` 都会遇到），所以这里按小写比。
    pub fn header(&self, name: &str) -> Option<&str> {
        let want = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k.to_ascii_lowercase() == want)
            .map(|(_, v)| v.as_str())
    }
}

/// 会出现在查询串里的敏感参数名（比较时不分大小写）。
///
/// 一张**白名单式的黑名单**：只有明确知道是凭据的那些才遮。宁可少遮一个不认识的参数，
/// 也不要按前缀猜（`--data` 这类参数会被误伤，日志反而不像真的）。
const SECRET_QUERY_KEYS: &[&str] = &[
    "access_token",
    "refresh_token",
    "client_secret",
    "token",
    "code",
    "sign",
    "signature",
    "password",
];

fn is_secret_query_key(name: &str) -> bool {
    SECRET_QUERY_KEYS
        .iter()
        .any(|key| name.eq_ignore_ascii_case(key))
}

/// 打日志用的 URL：**两处凭据都换成 `***`**，其余原样。
///
/// - authority 里的 userinfo：`https://user:pass@host/x` → `https://***@host/x`
///   （WebDAV 的账号密码会出现在这里，见 [`dav::Endpoint::url`]）；
/// - 查询串里 [`SECRET_QUERY_KEYS`] 那些参数的值：`?access_token=121.abc` → `?access_token=***`
///   （百度那套把 token 拼在 URL 上）。
///
/// 路径、其余参数、片段都原样保留 —— 日志要能看出「请求的是哪个文件」，
/// 遮过头就查不动了。
pub fn redact_url(url: &str) -> String {
    let mut out = String::with_capacity(url.len());
    // —— authority 里的 userinfo ——
    match url.split_once("://") {
        Some((scheme, rest)) => {
            out.push_str(scheme);
            out.push_str("://");
            // `@` 只在 authority 里、下一个 `/` 之前才算凭据。
            let head_len = rest.find('/').unwrap_or(rest.len());
            let (head, tail) = rest.split_at(head_len);
            match head.split_once('@') {
                Some((_, host)) => {
                    out.push_str("***@");
                    out.push_str(host);
                }
                None => out.push_str(head),
            }
            out.push_str(tail);
        }
        None => out.push_str(url),
    }
    // —— 查询串里的敏感参数 ——
    let Some((head, query)) = out.split_once('?') else {
        return out;
    };
    let mut redacted = String::with_capacity(out.len());
    redacted.push_str(head);
    redacted.push('?');
    for (index, pair) in query.split('&').enumerate() {
        if index > 0 {
            redacted.push('&');
        }
        match pair.split_once('=') {
            Some((name, _)) if is_secret_query_key(name) => {
                redacted.push_str(name);
                redacted.push_str("=***");
            }
            _ => redacted.push_str(pair),
        }
    }
    redacted
}

/// 只留「协议 + 主机 + 路径」的 URL：**整个查询串与片段都扔掉**。
///
/// 与 [`redact_url`] 的差别是保守程度，用途也不同：
///
/// - [`redact_url`] 用在**我们自己拼的** URL 上（token 是哪几个参数名我们说了算），
///   所以只遮白名单那几个，其余参数留着便于排查；
/// - 这一条用在**别人给的** URL 上（分享直链、预签名地址），那些地址的签名参数名
///   五花八门（`sign` / `auth_key` / `X-Amz-Signature` / 一长串十六进制），
///   白名单挡不住，所以整段丢掉。
pub fn strip_query(url: &str) -> String {
    url.split(['?', '#']).next().unwrap_or(url).to_string()
}

/// 远端仓库：同步快照与书文件共用的一层。
///
/// 五个动作就是「远端存着书与阅读状态」要的全部；WebDAV（[`dav::Client`]）与百度网盘
/// （folio-core `net::cloud::baidu` 的 `BaiduDrive`）各一份实现。`path` 与 [`dav::Entry`] 同义：
/// 相对这一台仓库的根、用 `/` 分隔、不带首尾斜杠；空串就是根。
pub trait RemoteStore: Send + Sync + 'static {
    /// 列一层。
    fn list(&self, path: &str) -> Result<Vec<dav::Entry>>;
    /// 读一个小对象（同步快照）。整本书走 [`RemoteStore::download`]。
    fn get(&self, path: &str) -> Result<Vec<u8>>;
    /// 写一个小对象。父目录必须已存在，所以流程上是 [`RemoteStore::mkdir`] 在前。
    fn put(&self, path: &str, body: Vec<u8>) -> Result<()>;
    /// 建一层目录。已经存在算成功。
    fn mkdir(&self, path: &str) -> Result<()>;
    /// 流式下载。字节怎么落地是 [`Sink`] 的事。
    fn download(&self, path: &str, sink: &mut dyn Sink, cancel: &Cancel) -> Result<u64>;

    /// 逐层建目录：`a/b/c` 的三段各建一次。深度只有同步目录那几层，空段跳过。
    fn mkdir_all(&self, dir: &str) -> Result<()> {
        let mut prefix = String::new();
        for part in dir.split('/').filter(|part| !part.is_empty()) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            self.mkdir(&prefix)?;
        }
        Ok(())
    }

    /// 从可定位的源上传。默认整份读进内存再 [`RemoteStore::put`]；百度那一头
    /// 覆盖成三步分片，不把一本几百 MB 的书整个装进内存。
    fn put_file(
        &self,
        path: &str,
        source: &mut dyn Source,
        _size: u64,
        _cancel: &Cancel,
    ) -> Result<u64> {
        let mut body = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = source.read(&mut buf)?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&buf[..n]);
        }
        let len = body.len() as u64;
        self.put(path, body)?;
        Ok(len)
    }
}

/// [`RemoteStore::put_file`] 的源：分片上传要能倒回去再读一遍 —— 先算每片 MD5，
/// 再逐片发出去。读与定位显式列出来，而不是挂 `Read + Seek` 两个超 trait：`dyn
/// Source` 上转 `dyn Read` 要 trait upcasting，当前这条链上不依赖它。
pub trait Source {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize>;
    fn seek_start(&mut self, pos: u64) -> std::io::Result<u64>;
}

impl<T: Read + Seek> Source for T {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        Read::read(self, buf)
    }

    fn seek_start(&mut self, pos: u64) -> std::io::Result<u64> {
        Seek::seek(self, std::io::SeekFrom::Start(pos))
    }
}

/// 把 [`Source`] 借成只读流，交给 [`Transport::upload`]（它不需要倒回去）。
pub struct SourceReader<'a>(pub &'a mut dyn Source);

impl Read for SourceReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

/// 下载的分块落点。谁实现它，谁就拥有「写到哪儿、临时文件怎么命名、下完怎么改名」
/// 这套本地策略；本层只保证 [`Transport::download`] 那条 finish/abort 契约。
pub trait Sink: Send {
    fn write(&mut self, chunk: &[u8]) -> std::io::Result<()>;
    fn finish(&mut self) -> std::io::Result<()>;
    fn abort(&mut self);
}

/// 取消标志。界面上的「取消」按钮置位，传输在下一次块边界看见它。
///
/// 实现住在 `folio-base`：导入、排版预热与网络共用同一个旗标，它不属于任何一家。
pub use folio_base::cancel::Cancel;

#[cfg(test)]
mod tests {
    //! 脱敏那一条：日志文件会被贴进工单、传给别人看，凭据不该跟着走。

    use super::{Method, Request, Response, redact_url};

    /// 查询串里的凭据值换成 `***`，其余参数与路径原样。
    #[test]
    fn a_token_in_the_query_is_masked() {
        let url = "https://pan.baidu.com/rest/2.0/xpan/file?method=list&dir=%2F%E4%B9%A6&access_token=121.abc";
        let got = redact_url(url);
        assert!(!got.contains("121.abc"), "token 还在：{got}");
        assert!(got.contains("access_token=***"), "{got}");
        assert!(got.contains("method=list"), "其余参数不该动：{got}");
        assert!(got.contains("dir=%2F%E4%B9%A6"), "路径不该动：{got}");
    }

    /// 换 token 的那两条 URL：`client_secret` / `code` / `refresh_token` 一起遮。
    #[test]
    fn the_oauth_body_parameters_are_masked_too() {
        let got = redact_url(
            "https://openapi.baidu.com/oauth/2.0/token?grant_type=authorization_code&code=THE-CODE&client_id=id&client_secret=SECRET&redirect_uri=oob",
        );
        assert!(!got.contains("THE-CODE"), "{got}");
        assert!(!got.contains("SECRET"), "{got}");
        assert!(got.contains("code=***"), "{got}");
        assert!(got.contains("client_secret=***"), "{got}");
        assert!(got.contains("client_id=id"), "client_id 不是秘密：{got}");
    }

    /// authority 里的 `user:password@` 也遮 —— WebDAV 允许把账号写进地址。
    #[test]
    fn userinfo_in_the_authority_is_masked() {
        let got = redact_url("https://noke:app-pass@dav.example.com/remote.php/dav/");
        assert!(!got.contains("app-pass"), "{got}");
        assert_eq!(got, "https://***@dav.example.com/remote.php/dav/");
    }

    /// `@` 出现在路径里（不是 authority）时不许误遮。
    #[test]
    fn an_at_sign_in_the_path_is_not_credentials() {
        let url = "https://dav.example.com/dav/a@b.epub";
        assert_eq!(redact_url(url), url);
    }

    /// `Request` 的 `Debug`：URL 遮过、头部只留名字、请求体只报字节数。
    ///
    /// 这一条是「任何一句 `debug!("{req:?}")` 都不会泄漏凭据」的总门 ——
    /// 三处泄漏面一次钉住。
    #[test]
    fn the_request_debug_prints_shape_not_secrets() {
        let req = Request::new(
            Method::Get,
            "https://pan.baidu.com/x?access_token=121.abc".to_string(),
        )
        .header("Authorization", "Basic bm9rZTphcHAtcGFzcw==")
        .header("User-Agent", "pan.baidu.com")
        .body(br#"{"client_secret":"S3CR3T","code":"THE-CODE"}"#.to_vec());
        let printed = format!("{req:?}");
        assert!(!printed.contains("121.abc"), "{printed}");
        assert!(
            !printed.contains("bm9rZTphcHAtcGFzcw=="),
            "Basic 的 base64 漏了：{printed}"
        );
        assert!(!printed.contains("S3CR3T"), "请求体漏了：{printed}");
        assert!(!printed.contains("THE-CODE"), "{printed}");
        // 形状还得在：方法（`Debug` 打的是枚举名）、域名、头名、字节数。
        assert!(printed.contains("method: Get"), "{printed}");
        assert!(printed.contains("pan.baidu.com/x"), "{printed}");
        assert!(printed.contains("Authorization"), "头名要留：{printed}");
        assert!(
            printed.contains("<44 bytes>"),
            "请求体只报字节数：{printed}"
        );
    }

    /// 应答体同样不进 `Debug`：换 token 的应答里就有 `access_token`。
    #[test]
    fn the_response_debug_prints_shape_not_the_body() {
        let resp = Response::new(
            200,
            br#"{"access_token":"121.abc","refresh_token":"122.cd"}"#.to_vec(),
        );
        let printed = format!("{resp:?}");
        assert!(!printed.contains("121.abc"), "{printed}");
        assert!(!printed.contains("122.cd"), "{printed}");
        assert!(printed.contains("200"), "{printed}");
    }

    /// `redacted_url()` 与 `Debug` 走的是同一条规则：日志里手写 URL 也用这一份。
    #[test]
    fn the_accessor_matches_the_debug_rule() {
        let req = Request::new(
            Method::Get,
            "https://dav.example.com/x?refresh_token=abc".to_string(),
        );
        assert_eq!(
            req.redacted_url(),
            "https://dav.example.com/x?refresh_token=***"
        );
        assert!(format!("{req:?}").contains("refresh_token=***"));
    }
}
