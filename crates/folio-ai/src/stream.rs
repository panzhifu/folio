//! Bounded OpenAI-compatible SSE decoding; JSON fallback has the same wire budget.

use std::io;

use folio_net::Sink;
use serde_json::Value;

use crate::ai::{AiError, Answer, MAX_ANSWER_CHARS, answer_from_body, clip};

pub(crate) const MAX_STREAM_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_EVENT_BYTES: usize = 64 * 1024;
pub(crate) const MAX_EVENTS: usize = 8192;

pub(crate) struct SseSink<'a> {
    on: &'a mut (dyn FnMut(&str) + Send),
    buf: Vec<u8>,
    text: String,
    chars: usize,
    received: usize,
    events: usize,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    mode: Mode,
    stopped: bool,
    terminal: bool,
    error: Option<String>,
}

enum Mode {
    Undecided,
    Sse,
    Json,
}

impl<'a> SseSink<'a> {
    pub(crate) fn new(on: &'a mut (dyn FnMut(&str) + Send)) -> Self {
        Self {
            on,
            buf: Vec::new(),
            text: String::new(),
            chars: 0,
            received: 0,
            events: 0,
            prompt_tokens: None,
            completion_tokens: None,
            mode: Mode::Undecided,
            stopped: false,
            terminal: false,
            error: None,
        }
    }

    pub(crate) fn answer(&self) -> Result<Answer, AiError> {
        if let Some(reason) = &self.error {
            return Err(AiError::Malformed(reason.clone()));
        }
        match self.mode {
            Mode::Undecided | Mode::Json => answer_from_body(&self.buf),
            Mode::Sse if !self.terminal => Err(AiError::Malformed("事件流未收到完成标记".into())),
            Mode::Sse => Ok(Answer {
                text: clip(self.text.trim(), MAX_ANSWER_CHARS),
                prompt_tokens: self.prompt_tokens,
                completion_tokens: self.completion_tokens,
            }),
        }
    }

    fn feed(&mut self, chunk: &[u8]) -> io::Result<()> {
        if chunk.len() > MAX_STREAM_BYTES.saturating_sub(self.received) {
            return self.reject("事件流超过容量上限");
        }
        self.received += chunk.len();
        match self.mode {
            Mode::Undecided => {
                self.buf.extend_from_slice(chunk);
                match self.buf.iter().find(|b| !b.is_ascii_whitespace()) {
                    Some(b'{') => self.mode = Mode::Json,
                    Some(_) => {
                        self.mode = Mode::Sse;
                        let bytes = std::mem::take(&mut self.buf);
                        self.feed_sse(&bytes)?;
                    }
                    None if self.buf.len() > MAX_EVENT_BYTES => {
                        return self.reject("事件流前缀超过上限");
                    }
                    None => {}
                }
            }
            Mode::Json => self.buf.extend_from_slice(chunk),
            Mode::Sse => self.feed_sse(chunk)?,
        }
        Ok(())
    }

    fn reject<T>(&mut self, reason: &'static str) -> io::Result<T> {
        self.error = Some(reason.into());
        Err(io::Error::new(io::ErrorKind::InvalidData, reason))
    }

    fn feed_sse(&mut self, mut chunk: &[u8]) -> io::Result<()> {
        while !chunk.is_empty() {
            let end = chunk
                .iter()
                .position(|b| *b == b'\n')
                .map(|at| at + 1)
                .unwrap_or(chunk.len());
            if end > MAX_EVENT_BYTES.saturating_sub(self.buf.len()) {
                return self.reject("单条事件超过容量上限");
            }
            self.buf.extend_from_slice(&chunk[..end]);
            chunk = &chunk[end..];
            if self.buf.last() == Some(&b'\n') {
                let mut line = std::mem::take(&mut self.buf);
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if !self.stopped {
                    self.eat_line(&line)?;
                }
            }
        }
        Ok(())
    }

    fn eat_line(&mut self, line: &[u8]) -> io::Result<()> {
        let line = std::str::from_utf8(line)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "事件不是 UTF-8"))?
            .trim();
        let Some(payload) = line.strip_prefix("data:") else {
            return Ok(());
        };
        let payload = payload.trim();
        if payload.is_empty() {
            return Ok(());
        }
        self.events += 1;
        if self.events > MAX_EVENTS {
            return self.reject("事件数量超过上限");
        }
        if payload == "[DONE]" {
            self.stopped = true;
            self.terminal = true;
            return Ok(());
        }
        let value: Value = match serde_json::from_str(payload) {
            Ok(value) => value,
            Err(_) => return self.reject("事件 JSON 不完整"),
        };
        if value.get("error").is_some() {
            self.error = Some("服务在流里回了错误".into());
            self.stopped = true;
            return Ok(());
        }
        if value
            .pointer("/choices/0/finish_reason")
            .is_some_and(|v| v.is_string())
        {
            self.terminal = true;
        }
        if let Some(usage) = value.get("usage").filter(|v| v.is_object()) {
            self.prompt_tokens = usage
                .get("prompt_tokens")
                .and_then(Value::as_u64)
                .or(self.prompt_tokens);
            self.completion_tokens = usage
                .get("completion_tokens")
                .and_then(Value::as_u64)
                .or(self.completion_tokens);
        }
        if let Some(piece) = value
            .pointer("/choices/0/delta/content")
            .or_else(|| value.pointer("/choices/0/message/content"))
            .and_then(Value::as_str)
        {
            let remaining = MAX_ANSWER_CHARS.saturating_sub(self.chars);
            let piece: String = piece.chars().take(remaining).collect();
            if !piece.is_empty() {
                self.chars += piece.chars().count();
                self.text.push_str(&piece);
                (self.on)(&piece);
            }
        }
        Ok(())
    }
}

impl Sink for SseSink<'_> {
    fn write(&mut self, chunk: &[u8]) -> io::Result<()> {
        self.feed(chunk)
    }

    fn finish(&mut self) -> io::Result<()> {
        if matches!(self.mode, Mode::Sse) && !self.buf.is_empty() && !self.stopped {
            let tail = std::mem::take(&mut self.buf);
            self.eat_line(&tail)?;
        }
        Ok(())
    }

    fn abort(&mut self) {
        self.buf.clear();
        self.text.clear();
        self.error = Some("事件流已中止".into());
    }
}
#[cfg(test)]
mod tests {
    //! 喂分块字节钉三件事：增量按 `data:` 事件出、`[DONE]` 之后不再吃、
    //! 整份 JSON 的回退路走得通。

    use crate::stream::{MAX_EVENT_BYTES, MAX_EVENTS, MAX_STREAM_BYTES, SseSink};
    use folio_net::Sink;

    /// 喂一组块，收下每一次增量。
    fn run(chunks: &[&[u8]]) -> (String, Vec<String>) {
        let mut deltas = Vec::new();
        let mut collect = |piece: &str| deltas.push(piece.to_string());
        let mut sink = SseSink::new(&mut collect);
        for chunk in chunks {
            sink.write(chunk).unwrap();
        }
        sink.finish().unwrap();
        let answer = sink.answer().expect("该解得出");
        (answer.text, deltas)
    }

    /// 事件跨块不丢字：`data:` 行被从中间切开也能拼回来。
    #[test]
    fn events_split_across_chunks_are_reassembled() {
        let (text, deltas) = run(&[
            "data: {\"choices\":[{\"delta\":{\"content\":\"你\"}}]}\n\ndata: {\"choi".as_bytes(),
            "ces\":[{\"delta\":{\"content\":\"好\"}}]}\n\ndata: [DONE]\n".as_bytes(),
        ]);
        assert_eq!(text, "你好");
        assert_eq!(deltas, vec!["你".to_string(), "好".to_string()]);
    }

    /// usage 与 `[DONE]`：token 数照实收，DONE 之后的块不再吃。
    #[test]
    fn usage_is_collected_and_done_stops_the_stream() {
        let (text, _) = run(&[
            "data: {\"choices\":[{\"delta\":{\"content\":\"答\"}}]}\n".as_bytes(),
            concat!(
                "data: {\"choices\":[{\"delta\":{}}],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":2}}\n",
                "data: [DONE]\n"
            )
            .as_bytes(),
            "data: {\"choices\":[{\"delta\":{\"content\":\"晚到的\"}}]}\n".as_bytes(),
        ]);
        assert_eq!(text, "答", "[DONE] 之后来的块不该进正文");
    }

    /// 前导空白 + 注释行 + 非 data 行都当噪音：有的网关爱先送一个 keepalive。
    #[test]
    fn comments_and_leading_whitespace_are_noise() {
        let (text, _) = run(&[
            "\n\n: keepalive\nevent: delta\ndata: {\"choices\":[{\"delta\":{\"content\":\"好\"}}]}\n\ndata: [DONE]\n".as_bytes(),
        ]);
        assert_eq!(text, "好");
    }

    /// 网关不理 `stream: true`：回一整个 JSON —— 嗅探到 `{` 走整份解析。
    #[test]
    fn a_json_body_falls_back_to_whole_answer() {
        let body = r#"{"choices":[{"message":{"content":"整段"}}]}"#.as_bytes();
        let (text, deltas) = run(&[body]);
        assert_eq!(text, "整段");
        assert!(
            deltas.is_empty(),
            "JSON 回退没有事件增量，打字机交给默认档（整段一次性）"
        );
    }

    /// 流里裹过来的错误要爆出来，而不是攒成一份空答案装成功。
    #[test]
    fn an_error_event_is_surfaced() {
        let mut deltas = Vec::new();
        let mut collect = |piece: &str| deltas.push(piece.to_string());
        let mut sink = SseSink::new(&mut collect);
        sink.write(b"data: {\"error\":{\"message\":\"quota\"}}\n")
            .unwrap();
        sink.finish().unwrap();
        assert!(matches!(sink.answer(), Err(super::AiError::Malformed(_))));
        assert!(deltas.is_empty());
    }

    #[test]
    fn stream_limits_reject_unbounded_lines_bodies_and_event_counts() {
        let mut on = |_: &str| {};
        let mut sink = SseSink::new(&mut on);
        assert!(sink.write(&vec![b'x'; MAX_EVENT_BYTES + 1]).is_err());
        let mut sink = SseSink::new(&mut on);
        assert!(sink.write(&vec![b'{'; MAX_STREAM_BYTES + 1]).is_err());
        let mut sink = SseSink::new(&mut on);
        for _ in 0..MAX_EVENTS {
            sink.write(b"data: {}\n").unwrap();
        }
        assert!(sink.write(b"data: {}\n").is_err());
    }

    #[test]
    fn incomplete_stream_is_not_a_successful_answer_and_output_is_bounded() {
        let mut output = String::new();
        let mut on = |text: &str| output.push_str(text);
        let mut sink = SseSink::new(&mut on);
        let piece = "界".repeat(crate::ai::MAX_ANSWER_CHARS + 1);
        let data = format!(
            "data: {}\n",
            serde_json::json!({"choices":[{"delta":{"content":piece}}]})
        );
        sink.write(data.as_bytes()).unwrap();
        sink.finish().unwrap();
        assert!(sink.answer().is_err());
        sink.write(b"data: [DONE]").unwrap();
        sink.finish().unwrap();
        assert_eq!(
            sink.answer().unwrap().text.chars().count(),
            crate::ai::MAX_ANSWER_CHARS
        );
        drop(sink);
        assert_eq!(output.chars().count(), crate::ai::MAX_ANSWER_CHARS);
    }
}
