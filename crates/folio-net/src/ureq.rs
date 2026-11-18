//! 同步 HTTP 实现；请求容量、空闲等待和总时限由 RequestLimits 控制。

use std::io::Read;
use std::sync::Arc;

use super::{Cancel, Error, Request, RequestLimits, Response, Result, Sink, Transport};

pub struct UreqTransport {
    agent: ureq::Agent,
    ai_agent: ureq::Agent,
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl UreqTransport {
    pub fn new() -> Self {
        Self {
            agent: agent(RequestLimits::default()),
            ai_agent: agent(RequestLimits::ai()),
        }
    }

    fn agent_for(&self, limits: RequestLimits) -> ureq::Agent {
        let standard = RequestLimits::default();
        let ai = RequestLimits::ai();
        if limits.connect == standard.connect && limits.idle == standard.idle {
            self.agent.clone()
        } else if limits.connect == ai.connect && limits.idle == ai.idle {
            self.ai_agent.clone()
        } else {
            agent(limits)
        }
    }

    fn call_with(
        &self,
        req: &Request,
        source: Option<&mut dyn Read>,
        len: u64,
        limits: RequestLimits,
    ) -> Result<ureq::Response> {
        if limits.connect.is_zero() || limits.idle.is_zero() || limits.total.is_zero() {
            return Err(Error::Malformed("请求等待预算必须大于零".into()));
        }
        let client = self.agent_for(limits);
        let call = client
            .request(req.method.as_str(), &req.url)
            .timeout(limits.total);
        let call = req
            .headers
            .iter()
            .fold(call, |call, (name, value)| call.set(name, value));
        let call = match source {
            Some(source) => call.set("Content-Length", &len.to_string()).send(source),
            None if req.body.is_empty() => call.call(),
            None => call.send(&req.body[..]),
        };
        match call {
            Ok(response) | Err(ureq::Error::Status(_, response)) => Ok(response),
            // 底层 Display 可包含原始 URL；只保留不含凭据的错误类别。
            Err(ureq::Error::Transport(error)) => {
                Err(Error::Transport(format!("HTTP {:?}", error.kind())))
            }
        }
    }

    fn finish(call: ureq::Response, max: u64) -> Result<Response> {
        ensure_status(&call)?;
        let status = call.status();
        let headers = collect_headers(&call);
        let mut body = Vec::new();
        call.into_reader()
            .take(max.saturating_add(1))
            .read_to_end(&mut body)
            .map_err(read_error)?;
        if body.len() as u64 > max {
            return Err(Error::Malformed("响应超过容量上限".into()));
        }
        Ok(Response {
            status,
            headers,
            body,
        })
    }

    fn stream(&self, req: Request, sink: &mut dyn Sink, cancel: &Cancel) -> Result<u64> {
        if cancel.triggered() {
            return Err(Error::Cancelled);
        }
        let limits = req.limits.unwrap_or_else(RequestLimits::download);
        let call = self.call_with(&req, None, 0, limits).map_err(|error| {
            if cancel.triggered() {
                Error::Cancelled
            } else {
                error
            }
        })?;
        ensure_status(&call)?;
        let mut reader = call.into_reader();
        let mut buf = [0u8; 64 * 1024];
        let mut written = 0u64;
        loop {
            if cancel.triggered() {
                return Err(Error::Cancelled);
            }
            let count = reader.read(&mut buf).map_err(|error| {
                if cancel.triggered() {
                    Error::Cancelled
                } else {
                    read_error(error)
                }
            })?;
            if cancel.triggered() {
                return Err(Error::Cancelled);
            }
            if count == 0 {
                break;
            }
            if (count as u64) > limits.response_bytes.saturating_sub(written) {
                return Err(Error::Malformed("下载超过容量上限".into()));
            }
            sink.write(&buf[..count])?;
            written += count as u64;
        }
        sink.finish()?;
        Ok(written)
    }
}

fn agent(limits: RequestLimits) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(limits.connect)
        .timeout_read(limits.idle)
        .timeout_write(limits.idle)
        .user_agent(&format!("folio/{}", env!("CARGO_PKG_VERSION")))
        .build()
}

fn read_error(error: std::io::Error) -> Error {
    Error::Transport(format!("HTTP 读写失败：{:?}", error.kind()))
}

impl Transport for UreqTransport {
    fn request(&self, req: Request) -> Result<Response> {
        let limits = req.limits.unwrap_or_default();
        Self::finish(
            self.call_with(&req, None, 0, limits)?,
            limits.response_bytes,
        )
    }

    fn upload(&self, req: Request, source: &mut dyn Read, len: u64) -> Result<Response> {
        let limits = req.limits.unwrap_or_else(RequestLimits::download);
        Self::finish(
            self.call_with(&req, Some(source), len, limits)?,
            limits
                .response_bytes
                .min(RequestLimits::default().response_bytes),
        )
    }

    fn download(&self, req: Request, sink: &mut dyn Sink, cancel: &Cancel) -> Result<u64> {
        let outcome = self.stream(req, sink, cancel);
        if outcome.is_err() {
            sink.abort();
        }
        outcome
    }
}

fn ensure_status(response: &ureq::Response) -> Result<()> {
    if !(200..300).contains(&response.status()) {
        return Err(status_error(
            response.status(),
            response.header("WWW-Authenticate"),
        ));
    }
    Ok(())
}

fn status_error(status: u16, challenge: Option<&str>) -> Error {
    match status {
        401 => {
            let digest_only = challenge
                .map(str::to_ascii_lowercase)
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

pub fn shared() -> Arc<dyn Transport> {
    Arc::new(super::retry::Retrying::new(
        UreqTransport::new(),
        super::retry::RetryPolicy::default(),
    ))
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use crate::ureq::{UreqTransport, status_error};
    use crate::{Cancel, Error, Method, Request, RequestLimits, Sink, Transport};

    #[derive(Default)]
    struct Bucket {
        bytes: Vec<u8>,
        finished: usize,
        aborted: usize,
        fail_finish: bool,
    }
    impl Sink for Bucket {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
            self.bytes.extend_from_slice(bytes);
            Ok(())
        }
        fn finish(&mut self) -> std::io::Result<()> {
            self.finished += 1;
            if self.fail_finish {
                Err(std::io::Error::other("disk full"))
            } else {
                Ok(())
            }
        }
        fn abort(&mut self) {
            self.aborted += 1;
            self.bytes.clear();
        }
    }

    fn server(reply: &'static [u8]) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let task = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut bytes = [0; 4096];
            let _ = stream.read(&mut bytes);
            stream.write_all(reply).unwrap();
        });
        (url, task)
    }

    fn limits(bytes: u64) -> RequestLimits {
        RequestLimits {
            connect: Duration::from_millis(100),
            idle: Duration::from_millis(100),
            total: Duration::from_secs(2),
            response_bytes: bytes,
        }
    }

    #[test]
    fn bounded_responses_abort_on_chunked_overflow_and_finish_failure() {
        let (url, task) = server(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n8\r\n12345678\r\n0\r\n\r\n");
        let result = UreqTransport::new().request(Request::new(Method::Get, url).limits(limits(4)));
        assert!(matches!(result, Err(Error::Malformed(_))));
        task.join().unwrap();
        for fail_finish in [false, true] {
            let (url, task) = server(
                b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\n12345678",
            );
            let mut sink = Bucket {
                fail_finish,
                ..Bucket::default()
            };
            let max = if fail_finish { 8 } else { 4 };
            assert!(
                UreqTransport::new()
                    .download(
                        Request::new(Method::Get, url).limits(limits(max)),
                        &mut sink,
                        &Cancel::new()
                    )
                    .is_err()
            );
            assert_eq!(sink.aborted, 1);
            assert!(sink.bytes.is_empty());
            assert_eq!(sink.finished, usize::from(fail_finish));
            task.join().unwrap();
        }
    }

    #[test]
    fn cancelled_silent_download_finishes_within_idle_budget() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let cancel = Cancel::new();
        let (stop, stopped) = mpsc::channel();
        let trigger = cancel.clone();
        let task = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = [0; 4096];
            let _ = stream.read(&mut bytes);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n")
                .unwrap();
            std::thread::sleep(Duration::from_millis(30));
            trigger.trigger();
            let _ = stopped.recv_timeout(Duration::from_secs(2));
        });
        let start = Instant::now();
        let mut sink = Bucket::default();
        let result = UreqTransport::new().download(
            Request::new(Method::Get, url).limits(limits(8)),
            &mut sink,
            &cancel,
        );
        let _ = stop.send(());
        task.join().unwrap();
        assert!(matches!(result, Err(Error::Cancelled)));
        assert!(start.elapsed() < Duration::from_secs(3));
        assert_eq!((sink.finished, sink.aborted), (0, 1));
    }

    #[test]
    fn connection_failures_abort_without_echoing_credentials() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let mut sink = Bucket::default();
        let result = UreqTransport::new().download(
            Request::new(
                Method::Get,
                format!("http://SECRET:SECRET@{addr}/?token=SECRET"),
            )
            .limits(limits(8)),
            &mut sink,
            &Cancel::new(),
        );
        assert!(!format!("{:?}", result.unwrap_err()).contains("SECRET"));
        assert_eq!((sink.finished, sink.aborted), (0, 1));
    }

    #[test]
    fn digest_only_challenges_get_their_own_error() {
        assert!(matches!(
            status_error(401, Some("Digest realm=\"folio\"")),
            Error::DigestRequired
        ));
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
