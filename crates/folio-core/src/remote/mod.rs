//! 远程书库：书在另一台机器上时要走的那一层。
//!
//! 这个模块只定义**传输**（[`Transport`]）和它的请求/响应形状，不含任何 `gpui` 类型。
//! 原因是 `folio-core` 与 `folio-app` 之间那条硬边界：core 不许依赖 gpui（见 `lib.rs`
//! 顶部），而真正能把字节发出去的那份实现要用 `App::http_client()`，只能住在 app 侧。
//! 于是 core 负责「该发什么、回来的东西怎么读」，app 负责「怎么发」。
//!
//! 拆成 trait 之后，协议里那些真正容易踩空的规则 —— 路径逐段转义、`Depth: 1` 的语义
//! 不齐、207 多状态里每个资源自己的状态码、`href` 的基准各家不同 —— 都能在
//! 无网络、无窗口、假传输的条件下测（见 [`dav`] 的测试）。

pub mod dav;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// 一层「把请求发出去、把响应拿回来」的能力。
///
/// 方法只有两个，差别在响应体去哪儿：小东西（PROPFIND 的 XML、备份的 JSON）整个进内存，
/// 一本几百多万字的全本 epub 不能。
pub trait Transport: Send + Sync + 'static {
    fn request(&self, req: Request) -> BoxFuture<Result<Response>>;

    /// 流式下载，响应体按块交给 `sink`。
    ///
    /// 契约：无论成功、失败还是取消，`Sink` 的 `finish` 与 `abort` **恰好调一个** ——
    /// 临时文件要么变成正式文件，要么被清掉，不会留下第三种状态。
    /// 取消不是抢占，只在块与块之间检查，所以最多晚一个块生效。
    fn download(&self, req: Request, sink: Box<dyn Sink>, cancel: Cancel)
    -> BoxFuture<Result<u64>>;
}

/// 传输层方法的返回类型。用 `std` 的 `Pin<Box<dyn Future>>` 而不是 `futures::BoxFuture`，
/// 这样 core 不必为了一个别名多挂一条依赖。
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

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
    Put,
    Propfind,
    Mkcol,
    Delete,
}

impl Method {
    pub fn as_str(&self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Put => "PUT",
            // WebDAV 的方法名不在 HTTP 标准方法里，得原样发出去。
            Method::Propfind => "PROPFIND",
            Method::Mkcol => "MKCOL",
            Method::Delete => "DELETE",
        }
    }
}

/// 一条已经拼好的请求：URL 已逐段转义，头已带齐。实现只管发，不再碰字符串。
#[derive(Debug, Clone)]
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
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
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

/// 下载的分块落点。谁实现它，谁就拥有「写到哪儿、临时文件怎么命名、下完怎么改名」
/// 这套本地策略；本层只保证 [`Transport::download`] 那条 finish/abort 契约。
pub trait Sink: Send {
    fn write(&mut self, chunk: &[u8]) -> std::io::Result<()>;
    fn finish(&mut self) -> std::io::Result<()>;
    fn abort(&mut self);
}

/// 取消标志。界面上的「取消」按钮置位，传输在下一次块边界看见它。
#[derive(Debug, Default, Clone)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn trigger(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn triggered(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
}
