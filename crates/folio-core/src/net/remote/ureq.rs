//! [`Transport`] 的同步实现：[`ureq`]，TLS 走 rustls + 打包的 Mozilla 根证书。
//!
//! **同步是有意的**：管线的作业跑在后台线程上（`crate::infra::task`），「一个请求流到底」
//! 的形状用阻塞读最直白 —— core 里因此不需要任何执行器。打包根证书（webpki-roots）
//! 而不是系统信任库：Android 上没有 `/etc/ssl` 那种文件，带一套 Mozilla 根证书是
//! 唯一不需要平台配合的路子。
//!
//! 覆盖面：自定义方法（PROPFIND / MKCOL）、Basic 认证（头由调用方自己带）、
//! 超时、流式响应体（逐块交给 [`Sink`]，块间查取消）。Digest 认证不支持 ——
//! 那条 [`Error::DigestRequired`] 单独一支，遇上时界面有明确的话可说。

use std::io::Read as _;
use std::sync::Arc;
use std::time::Duration;

use super::{Cancel, Error, Request, Response, Result, Sink, Transport};

/// 连接 / 首字节的等待上限。书是大文件，读的**总**时长不该被它掐死 ——
/// 整体超时只设在「连不上」这一档，正文读取靠取消旗标与用户。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

pub struct UreqTransport {
    agent: ureq::Agent,
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl UreqTransport {
    pub fn new() -> Self {
        let agent: ureq::Agent = ureq::AgentBuilder::new()
            .timeout_connect(CONNECT_TIMEOUT)
            .user_agent(&format!("folio/{}", env!("CARGO_PKG_VERSION")))
            .build();
        Self { agent }
    }
}

impl Transport for UreqTransport {
    fn request(&self, req: Request) -> Result<Response> {
        let call = self.call(&req)?;
        if !(200..300).contains(&call.status()) {
            return Err(status_error(call.status(), call.header("WWW-Authenticate")));
        }
        let status = call.status();
        let headers = collect_headers(&call);
        let mut body = Vec::new();
        call.into_reader()
            .read_to_end(&mut body)
            .map_err(|err| Error::Transport(err.to_string()))?;
        Ok(Response {
            status,
            headers,
            body,
        })
    }

    fn download(&self, req: Request, sink: &mut dyn Sink, cancel: &Cancel) -> Result<u64> {
        let call = self.call(&req)?;
        if !(200..300).contains(&call.status()) {
            // 状态错了就一块都不写：sink 的 finish/abort 契约在这里结清。
            sink.abort();
            return Err(status_error(call.status(), call.header("WWW-Authenticate")));
        }
        let mut reader = call.into_reader();
        let mut buf = vec![0u8; 64 * 1024];
        let mut written = 0u64;
        loop {
            // 取消只在块与块之间检查（契约如此）：最多晚一个 64 KB 生效。
            if cancel.triggered() {
                sink.abort();
                return Err(Error::Cancelled);
            }
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if let Err(err) = sink.write(&buf[..n]) {
                        sink.abort();
                        return Err(Error::Io(err));
                    }
                    written += n as u64;
                }
                Err(err) => {
                    sink.abort();
                    return Err(Error::Transport(err.to_string()));
                }
            }
        }
        match sink.finish() {
            Ok(()) => Ok(written),
            Err(err) => {
                sink.abort();
                Err(Error::Io(err))
            }
        }
    }
}

impl UreqTransport {
    /// 发一次请求。`ureq` 把非 2xx 当 `Err(Status(..))` 返回 —— 响应还在错误里，
    /// 统一摊平成「拿得到状态的响应」，让 [`Self::request`] 与 [`Self::download`]
    /// 走同一条错误路。
    fn call(&self, req: &Request) -> Result<ureq::Response> {
        let call = self
            .agent
            .request(req.method.as_str(), &req.url)
            .timeout(std::time::Duration::from_secs(600));
        let call = req
            .headers
            .iter()
            .fold(call, |call, (name, value)| call.set(name, value));
        // 空体 = 没有 body 的请求（GET / PROPFIND）；非空体一律按字节发。
        let call = if req.body.is_empty() {
            call.call()
        } else {
            call.send(&req.body[..])
        };
        match call {
            Ok(response) => Ok(response),
            Err(ureq::Error::Status(_, response)) => Ok(response),
            Err(ureq::Error::Transport(err)) => Err(Error::Transport(err.to_string())),
        }
    }
}

/// 非 2xx 的响应 → [`Error`]。401 且挑战只给了 Digest 的那支单独走 ——
/// 「这台服务器要求 Digest」是读者能自己解决的事（换 https、用应用密码），
/// 混进「认证被拒」就只会让他对着密码框发呆。
fn status_error(status: u16, challenge: Option<&str>) -> Error {
    match status {
        401 => {
            let digest_only = challenge
                .map(|c| c.to_ascii_lowercase())
                .is_some_and(|c| c.contains("digest") && !c.contains("basic"));
            if digest_only {
                Error::DigestRequired
            } else {
                Error::Unauthorized { status }
            }
        }
        403 => Error::Forbidden { status },
        404 => Error::NotFound,
        405 => Error::NotWebDav,
        _ => Error::Unexpected { status },
    }
}

fn collect_headers(response: &ureq::Response) -> Vec<(String, String)> {
    response
        .headers_names()
        .into_iter()
        .filter_map(|name| {
            response
                .header(&name)
                .map(|value| (name.clone(), value.to_string()))
        })
        .collect()
}

/// 供 app 侧共享（`Arc<dyn Transport>` 的形状在这里定）。
pub fn shared() -> Arc<dyn Transport> {
    Arc::new(UreqTransport::new())
}

#[cfg(test)]
mod tests {
    use super::status_error;
    use crate::net::remote::Error;

    /// 401 且挑战只给了 Digest：单独一支，界面有明确的话可说。
    #[test]
    fn digest_only_challenges_get_their_own_error() {
        assert!(matches!(
            status_error(401, Some("Digest realm=\"folio\"")),
            Error::DigestRequired
        ));
        // Basic 在场就是普通的认证被拒 —— 服务器两条都收时读者改密码就行。
        assert!(matches!(
            status_error(401, Some("Basic realm=\"folio\", Digest")),
            Error::Unauthorized { status: 401 }
        ));
        assert!(matches!(
            status_error(401, None),
            Error::Unauthorized { status: 401 }
        ));
        assert!(matches!(
            status_error(503, None),
            Error::Unexpected { status: 503 }
        ));
    }
}
