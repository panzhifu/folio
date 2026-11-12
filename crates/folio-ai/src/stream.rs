//! SSE（Server-Sent Events）流的解析：OpenAI 兼容服务的**流式问答**骑在
//! [`Transport::download`](folio_net::Transport::download) 上 —— 传输层已经给好了
//! 「非 2xx 状态门 + 分块读 + 块间取消检查点」，本模块只补一件事：把块里的字节按
//! `data:` 行拆开，取 `choices[0].delta.content` 交给调用方，攒成一份完整答案。
//!
//! **嗅探回退**：有的网关不认 `stream: true`，回一整个 JSON 对象而不是事件流。
//! 第一块去掉空白后以 `{` 开头就按整份 JSON 解析（读完再解），否则按 SSE 走 ——
//! 调用方（`ChatCompletion::ask_stream`）不用知道对面是哪一种。
//!
//! 取消不是这里的事：`download` 在块与块之间看旗标，取消时把 `Err(Cancelled)`
//! 抛回调用方；已经交出去的增量留在调用方手里（打字机不回滚）。

use std::io;

use folio_net::Sink;
use serde_json::Value;

use crate::ai::{AiError, Answer, MAX_ANSWER_CHARS, answer_from_body, clip};

/// 喂字节、交增量的桶。实现 [`Sink`] 是为了直接骑在 `Transport::download` 上；
/// 流结束后用 [`SseSink::answer`](Self::answer) 取攒好的那份。
pub(crate) struct SseSink<'a> {
    /// 增量出口：每收到一段正文就被调一次（打字机的那一笔）。
    on: &'a mut (dyn FnMut(&str) + Send),
    /// 还没凑成整行的字节（SSE）或整份 body（JSON 回退）。
    buf: Vec<u8>,
    /// 攒到现在的正文。
    text: String,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    /// 对面是哪一种应答：第一块见了才定（嗅探回退，见模块文档）。
    mode: Mode,
    /// 见了 `[DONE]` 或错误事件：之后的块不再吃。
    stopped: bool,
    /// 事件流里裹过来的错误（错误也包成一条 `data:` 事件的服务存在）。
    error: Option<String>,
}

enum Mode {
    /// 第一块还没到。
    Undecided,
    /// 正常的 `data:` 事件流。
    Sse,
    /// 网关不理 `stream: true`，回了一整个 JSON。
    Json,
}

impl<'a> SseSink<'a> {
    pub(crate) fn new(on: &'a mut (dyn FnMut(&str) + Send)) -> Self {
        Self {
            on,
            buf: Vec::new(),
            text: String::new(),
            prompt_tokens: None,
            completion_tokens: None,
            mode: Mode::Undecided,
            stopped: false,
            error: None,
        }
    }

    /// 流结束（`download` 回了 `Ok`）后取答案。JSON 回退在这里解整份 body；
    /// SSE 攒好的直接交。事件里裹过错误时在这里爆出来。
    pub(crate) fn answer(&self) -> Result<Answer, AiError> {
        if let Some(reason) = &self.error {
            return Err(AiError::Malformed(reason.clone()));
        }
        match self.mode {
            // 咬不准就按 JSON 试：空体两路都解不开，报错口径一致。
            Mode::Undecided | Mode::Json => answer_from_body(&self.buf),
            Mode::Sse => Ok(Answer {
                text: clip(self.text.trim(), MAX_ANSWER_CHARS),
                prompt_tokens: self.prompt_tokens,
                completion_tokens: self.completion_tokens,
            }),
        }
    }

    fn feed(&mut self, chunk: &[u8]) {
        match self.mode {
            Mode::Undecided => self.sniff(chunk),
            Mode::Json => self.buf.extend_from_slice(chunk),
            Mode::Sse => self.feed_sse(chunk),
        }
    }

    /// 第一块定模式。前导空白（有的网关爱带换行）不参与判断；全空白就等下一块。
    fn sniff(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
        let first = self
            .buf
            .iter()
            .position(|b| !b.is_ascii_whitespace())
            .map(|at| self.buf[at]);
        match first {
            Some(b'{') => self.mode = Mode::Json,
            Some(_) => {
                self.mode = Mode::Sse;
                let buffered = std::mem::take(&mut self.buf);
                self.feed_sse(&buffered);
            }
            None => {}
        }
    }

    fn feed_sse(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
        // 只吃整行：半行（事件跨块）留在 buf 里等下一块。
        while let Some(at) = self.buf.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=at).collect();
            line.pop(); // 那个 \n
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if self.stopped {
                continue;
            }
            self.eat_line(&line);
        }
    }

    /// 一条完整的 SSE 行。只认 `data:`：`event:`/`id:`/`retry:` 与 `:` 开头的
    /// keepalive 注释都与正文无关。
    fn eat_line(&mut self, line: &[u8]) {
        let line = match std::str::from_utf8(line) {
            Ok(line) => line.trim(),
            Err(_) => return,
        };
        let Some(payload) = line.strip_prefix("data:") else {
            return;
        };
        let payload = payload.trim();
        if payload == "[DONE]" {
            self.stopped = true;
            return;
        }
        if payload.is_empty() {
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(payload) else {
            return; // 半截 JSON（理论上不会：事件以行为界）按噪音丢掉。
        };
        if value.get("error").is_some() {
            self.error = Some("服务在流里回了错误".to_string());
            self.stopped = true;
            return;
        }
        if let Some(usage) = value.get("usage").filter(|usage| usage.is_object()) {
            self.prompt_tokens = usage
                .get("prompt_tokens")
                .and_then(Value::as_u64)
                .or(self.prompt_tokens);
            self.completion_tokens = usage
                .get("completion_tokens")
                .and_then(Value::as_u64)
                .or(self.completion_tokens);
        }
        // 正文在 delta.content；有的网关在最后一条里用 message.content 复述整段，
        // 两种都认，拼的时候不去重 —— 重复的代价是显示层多几行，漏掉的代价是缺一段。
        let piece = value
            .pointer("/choices/0/delta/content")
            .or_else(|| value.pointer("/choices/0/message/content"))
            .and_then(Value::as_str);
        if let Some(piece) = piece
            && !piece.is_empty()
        {
            self.text.push_str(piece);
            (self.on)(piece);
        }
    }
}

impl Sink for SseSink<'_> {
    fn write(&mut self, chunk: &[u8]) -> io::Result<()> {
        self.feed(chunk);
        Ok(())
    }

    /// 契约要求的收尾：答案不从这一步出（见 [`SseSink::answer`]），这里没有要清的东西。
    fn finish(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn abort(&mut self) {}
}

#[cfg(test)]
mod tests {
    //! 喂分块字节钉三件事：增量按 `data:` 事件出、`[DONE]` 之后不再吃、
    //! 整份 JSON 的回退路走得通。

    use super::SseSink;
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
}
