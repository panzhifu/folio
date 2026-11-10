//! 传输层的重试装饰器：429 / 5xx / 连不上时退避重发。
//!
//! 这一层只做**幂等重放**：[`Transport::request`] 的请求体是一块 `Vec<u8>`，
//! 重发不会读到半截；小体积的 `upload` 先缓冲成同一形状再走这条路。流式下载与
//! 大体量上传**不重试** —— 前者的 `Sink` 已经写进了字节、没有回卷口，后者的
//! `&mut dyn Read` 走过一次就回不到开头，盲目重发只会把半本书再写一遍。那两类
//! 要重试，得先有断点续传的语义。
//!
//! 退避是 `base * 2^(n-1)`，封顶 `max_delay`。不带抖动：本仓库的并发量是个位数，
//! 抖动解决的是「一群客户端同时醒来」，这里没有那一群。

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use super::{Cancel, Error, Method, Request, Response, Result, Sink, Transport};

/// 小体积上传的缓冲上限：在这一档以内先读进内存，换一次可重放的机会。
///
/// 4 MiB 足够覆盖翻译 / 检查更新 / JSON 换 token 这一类请求，而一本几百 MB 的书
/// 仍走流式、不重试 —— 把整本书读进内存去换「可能重试一次」不划算。
const BUFFERED_UPLOAD_LIMIT: u64 = 4 * 1024 * 1024;

/// 重试的档位。
#[derive(Clone, Copy, Debug)]
pub struct RetryPolicy {
    /// 总共试几次（含第一次）。`1` = 不重试。
    pub attempts: u32,
    /// 第一次退避；之后每次翻倍。
    pub base: Duration,
    /// 单次退避的上限。
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: 3,
            base: Duration::from_millis(300),
            max_delay: Duration::from_secs(3),
        }
    }
}

impl RetryPolicy {
    /// 第 `attempt` 次失败后该等多久（`attempt` 从 1 起）。
    fn backoff(&self, attempt: u32) -> Duration {
        let shift = attempt.saturating_sub(1).min(20);
        self.base.saturating_mul(1u32 << shift).min(self.max_delay)
    }
}

/// 哪些错误值得再试一次。
///
/// - `Transport`：DNS、连接、TLS、读超时 —— 下一次可能就好了。但**只对幂等方法**：
///   POST 可能已经在对面执行了，重发会重复建文件 / 重复计费；
/// - 408 / 429 / 5xx：服务器明确回了状态（限流、过载、网关抽风），POST 也照重发 ——
///   它已经说完「这次没收下」，再发一次不会把同一件事做两遍。
///
/// 401 / 403 / 404 这类是**说得清的结果**，重发只会把同一个答案再拿一遍；`Cancelled`
/// 与本地 `Io` 同理。401 的那次续期已经由 `AuthedStore` 单独处理，不在这里叠。
fn retryable(error: &Error, method: Method) -> bool {
    match error {
        Error::Transport(_) => matches!(
            method,
            Method::Get | Method::Put | Method::Propfind | Method::Mkcol | Method::Delete
        ),
        Error::Unexpected { status } => {
            *status == 408 || *status == 429 || (500..=599).contains(status)
        }
        _ => false,
    }
}

/// 给任意 [`Transport`] 套一层退避重发。
pub struct Retrying<T> {
    inner: T,
    policy: RetryPolicy,
    sleep: Arc<dyn Fn(Duration) + Send + Sync>,
}

impl<T> Retrying<T> {
    pub fn new(inner: T, policy: RetryPolicy) -> Self {
        Self {
            inner,
            policy,
            sleep: Arc::new(std::thread::sleep),
        }
    }

    /// 测试用：把「等」换成一个记账的闭包，断言退避序列而不真的睡。
    pub fn with_sleep(
        inner: T,
        policy: RetryPolicy,
        sleep: impl Fn(Duration) + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner,
            policy,
            sleep: Arc::new(sleep),
        }
    }

    /// 把一次可重放的操作跑到底：`op` 每次拿到同一个请求。
    fn attempt<F>(&self, method: Method, mut op: F) -> Result<Response>
    where
        F: FnMut() -> Result<Response>,
    {
        let attempts = self.policy.attempts.max(1);
        let mut tried = 1;
        loop {
            match op() {
                Ok(response) => return Ok(response),
                Err(error) if tried < attempts && retryable(&error, method) => {
                    (self.sleep)(self.policy.backoff(tried));
                    tried += 1;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

impl<T: Transport> Transport for Retrying<T> {
    fn request(&self, req: Request) -> Result<Response> {
        let method = req.method;
        self.attempt(method, || self.inner.request(req.clone()))
    }

    /// 流式下载不重试：`Sink` 可能已经写进了字节，而契约里没有「回到开头」这一口。
    /// 真要重试，得让调用方在开始之前决定，并由它自己清掉半截文件。
    fn download(&self, req: Request, sink: &mut dyn Sink, cancel: &Cancel) -> Result<u64> {
        self.inner.download(req, sink, cancel)
    }

    /// 小体积上传先缓冲，换一次可重放；大体积仍走流式、不重试。
    fn upload(&self, req: Request, source: &mut dyn Read, len: u64) -> Result<Response> {
        if len > BUFFERED_UPLOAD_LIMIT {
            return self.inner.upload(req, source, len);
        }
        let mut body = Vec::with_capacity(len as usize);
        source.read_to_end(&mut body)?;
        self.request(req.body(body))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::Cursor;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::{RetryPolicy, Retrying, retryable};
    use crate::{Cancel, Error, Method, Request, Response, Result, Transport};

    /// 按剧本返回的假传输：剧本空了就一直成功。
    struct Scripted {
        calls: AtomicUsize,
        script: Mutex<VecDeque<Option<u16>>>,
    }

    impl Scripted {
        fn new(script: &[Option<u16>]) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                script: Mutex::new(script.iter().copied().collect()),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }
    }

    impl Transport for Scripted {
        fn request(&self, _req: Request) -> Result<Response> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            match self.script.lock().unwrap().pop_front().flatten() {
                None => Ok(Response::new(200, Vec::new())),
                // 0 = 连接根本没成为一次往返（`Transport`）。
                Some(0) => Err(Error::Transport("timeout".into())),
                // 与 `UreqTransport` 一样：404 是独立的一支，不是 `Unexpected`。
                Some(404) => Err(Error::NotFound),
                Some(status) => Err(Error::Unexpected { status }),
            }
        }

        fn download(
            &self,
            _req: Request,
            sink: &mut dyn super::super::Sink,
            _cancel: &Cancel,
        ) -> Result<u64> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            sink.abort();
            Err(Error::Transport("下载失败".into()))
        }
    }

    fn request() -> Request {
        Request::new(Method::Get, "https://example.test/x".into())
    }

    /// 500 两次、第三次成功：一共三次，退避是 300 ms → 600 ms。
    #[test]
    fn a_transient_failure_is_retried_with_exponential_backoff() {
        let sleeps = Arc::new(Mutex::new(Vec::new()));
        let inner = Scripted::new(&[Some(500), Some(503)]);
        let recorded = sleeps.clone();
        let transport = Retrying::with_sleep(
            inner,
            RetryPolicy {
                attempts: 3,
                base: Duration::from_millis(300),
                max_delay: Duration::from_secs(3),
            },
            move |delay| recorded.lock().unwrap().push(delay),
        );
        assert!(transport.request(request()).is_ok());
        assert_eq!(transport.inner.calls(), 3);
        assert_eq!(
            *sleeps.lock().unwrap(),
            vec![Duration::from_millis(300), Duration::from_millis(600)]
        );
    }

    /// 404 是终局：不重试，也不睡。
    #[test]
    fn a_definitive_failure_is_not_retried() {
        let sleeps = Arc::new(Mutex::new(Vec::new()));
        let inner = Scripted::new(&[Some(404)]);
        let recorded = sleeps.clone();
        let transport = Retrying::with_sleep(inner, RetryPolicy::default(), move |delay| {
            recorded.lock().unwrap().push(delay)
        });
        let result = transport.request(request());
        assert!(matches!(result, Err(Error::NotFound)));
        assert_eq!(transport.inner.calls(), 1);
        assert!(sleeps.lock().unwrap().is_empty());
    }

    /// 次数用完就交回最后一次的错，不无限重试。
    #[test]
    fn attempts_are_bounded() {
        let inner = Scripted::new(&[Some(500), Some(500), Some(500), Some(500)]);
        let transport = Retrying::with_sleep(
            inner,
            RetryPolicy {
                attempts: 2,
                base: Duration::from_millis(1),
                max_delay: Duration::from_millis(1),
            },
            |_| {},
        );
        let result = transport.request(request());
        assert!(matches!(result, Err(Error::Unexpected { status: 500 })));
        assert_eq!(transport.inner.calls(), 2);
    }

    /// 退避封顶，不会指数爆炸到几分钟。
    #[test]
    fn backoff_is_capped() {
        let policy = RetryPolicy {
            attempts: 10,
            base: Duration::from_millis(300),
            max_delay: Duration::from_secs(2),
        };
        assert_eq!(policy.backoff(1), Duration::from_millis(300));
        assert_eq!(policy.backoff(2), Duration::from_millis(600));
        assert_eq!(policy.backoff(3), Duration::from_millis(1200));
        assert_eq!(policy.backoff(4), Duration::from_secs(2));
        assert_eq!(policy.backoff(10), Duration::from_secs(2));
    }

    /// 小上传被缓冲后走同一条重试路；大上传原样 delegate（流式，不重试）。
    #[test]
    fn small_uploads_are_replayable_but_large_ones_are_not() {
        let inner = Scripted::new(&[Some(500)]);
        let transport = Retrying::with_sleep(inner, RetryPolicy::default(), |_| {});
        let mut body = Cursor::new(b"hello".to_vec());
        let response = transport
            .upload(request(), &mut body, 5)
            .expect("小上传该在重试后成功");
        assert_eq!(response.status, 200);
        assert_eq!(transport.inner.calls(), 2);

        let inner = Scripted::new(&[Some(500)]);
        let transport = Retrying::with_sleep(inner, RetryPolicy::default(), |_| {});
        let mut big = Cursor::new(vec![0u8; 8]);
        // `len` 是调用方给的：8 字节本身很小，但这里把「长度」报到上限之上，
        // 钉住分支走的是流式、不缓冲。
        let result = transport.upload(request(), &mut big, super::BUFFERED_UPLOAD_LIMIT + 1);
        assert!(result.is_err());
        assert_eq!(transport.inner.calls(), 1);
    }

    /// 分类表本身：值得重试的与不该重试的各就各位；POST 的连接失败不重试。
    #[test]
    fn only_transient_errors_are_retryable() {
        assert!(retryable(&Error::Transport("timeout".into()), Method::Get));
        assert!(retryable(&Error::Transport("timeout".into()), Method::Put));
        assert!(
            !retryable(&Error::Transport("timeout".into()), Method::Post),
            "POST 可能已经在对面执行了：连接超时不能盲目重发"
        );
        for status in [408, 429, 500, 502, 503, 504] {
            assert!(
                retryable(&Error::Unexpected { status }, Method::Post),
                "{status} 是服务器明确没收下，POST 也该重试"
            );
        }
        for status in [400, 401, 403, 404, 405, 409, 422] {
            assert!(
                !retryable(&Error::Unexpected { status }, Method::Get),
                "{status} 不该重试"
            );
        }
        assert!(!retryable(&Error::Cancelled, Method::Get));
        assert!(!retryable(&Error::Malformed("x".into()), Method::Get));
    }

    /// POST 的连接超时：一次就交回，不睡也不重发。
    #[test]
    fn a_post_that_never_got_through_is_not_replayed() {
        let sleeps = Arc::new(Mutex::new(Vec::new()));
        let inner = Scripted::new(&[Some(0)]);
        let recorded = sleeps.clone();
        let transport = Retrying::with_sleep(inner, RetryPolicy::default(), move |delay| {
            recorded.lock().unwrap().push(delay)
        });
        let result = transport.request(Request::new(
            Method::Post,
            "https://example.test/token".into(),
        ));
        assert!(matches!(result, Err(Error::Transport(_))));
        assert_eq!(transport.inner.calls(), 1);
        assert!(sleeps.lock().unwrap().is_empty());
    }
}
