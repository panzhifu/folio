//! OpenAI 兼容引擎：`POST {base}/chat/completions`。
//!
//! 一份配置覆盖所有说这套协议的服务 —— 官方、DeepSeek、火山 ARK、本地 Ollama /
//! LM Studio。译文质量由模型决定，这一层只保证两件事：请求形状对，响应读得懂。
//!
//! 提示词要求「只回译文」：弹层只画一段正文，模型顺带写的解释会把卡片撑成一篇文章。
//! 不给 `temperature` 之类的采样参数 —— 新一代推理模型会直接拒掉不认识的字段，
//! 而翻译这件事本来也不靠调温。

use serde_json::{Value, json};

use super::{TranslateError, Translation, Translator, parse_json, source_slice};
use crate::ingest::identity::sha256_of_bytes;
use crate::net::remote::{Method, Request, Transport};

/// 一只 OpenAI 兼容引擎。字段都是建的那一刻的快照；会话把整只搬进后台作业。
pub struct OpenAiTranslator {
    base_url: String,
    model: String,
    api_key: String,
}

impl OpenAiTranslator {
    pub const NAME: &'static str = "openai";

    pub fn new(base_url: &str, model: &str, api_key: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
            model: model.to_string(),
            api_key: api_key.to_string(),
        }
    }

    /// 补 `/chat/completions`。读者把地址写到方法那一层（有的服务文档就这么写）也认。
    fn endpoint(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        if base.ends_with("/chat/completions") {
            base.to_string()
        } else {
            format!("{base}/chat/completions")
        }
    }

    /// 拼请求。必填项为空时在**出网前**就报出来：让读者去设置页补地址，比让
    /// 服务端回一句 `404 page not found` 有用。
    fn request(&self, text: &str, from: &str, to: &str) -> Result<Request, TranslateError> {
        if self.base_url.is_empty() {
            return Err(TranslateError::NotConfigured("服务地址"));
        }
        if self.model.is_empty() {
            return Err(TranslateError::NotConfigured("模型"));
        }
        let body = json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": prompt(from, to)},
                {"role": "user", "content": text},
            ],
        });
        let mut req = Request::new(Method::Post, self.endpoint())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .body(body.to_string().into_bytes());
        // 本地 Ollama / LM Studio 不认 Authorization：空 key 就不带头，由服务自己
        // 决定要不要拒。界面不猜「这只引擎要不要 key」。
        if !self.api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", self.api_key));
        }
        Ok(req)
    }
}

impl Translator for OpenAiTranslator {
    fn cache_key(&self) -> String {
        // 地址也进指纹：同一个模型名在两台服务上可能不是同一个模型（网关会改路由）。
        // key 不进 —— 换账号不改变同一模型的答案，而密钥写进数据库只是多一处泄漏面。
        let base = sha256_of_bytes(self.base_url.as_bytes()).to_hex();
        format!("{}:{}@{}", Self::NAME, self.model, &base[..8])
    }

    fn translate(
        &self,
        http: &dyn Transport,
        text: &str,
        from: &str,
        to: &str,
    ) -> Result<Translation, TranslateError> {
        let source = source_slice(text);
        let response = http.request(self.request(&source, from, to)?)?;
        parse_response(&response.body)
    }
}

/// 提示词。`from` 是 `auto` 时不提源语 —— 让模型自己认；指定了才说，省一次猜测。
fn prompt(from: &str, to: &str) -> String {
    let target = language_name(to);
    if from.eq_ignore_ascii_case("auto") {
        format!("Translate the text into {target}. Reply with the translation only.")
    } else {
        format!(
            "Translate the text from {} into {target}. Reply with the translation only.",
            language_name(from)
        )
    }
}

/// BCP-47 码 → 提示词里的语言名。认不出的码原样递过去：模型对 `yue` 这类标签的
/// 理解通常好过我们临时编一个名字。
fn language_name(code: &str) -> &str {
    match code {
        "zh-CN" | "zh-Hans" => "Simplified Chinese",
        "zh-TW" | "zh-Hant" => "Traditional Chinese",
        "en" => "English",
        "ja" => "Japanese",
        "ko" => "Korean",
        "de" => "German",
        "fr" => "French",
        "es" => "Spanish",
        "ru" => "Russian",
        other => other,
    }
}

/// 解析 `chat/completions` 的响应体。`content` 既可能是字符串，也可能是
/// `[{type:"text", text:"…"}]` 的段表（新一代模型与网关会这么回），两种都拼。
fn parse_response(body: &[u8]) -> Result<Translation, TranslateError> {
    let json = parse_json(body, "openai")?;
    let content = json.pointer("/choices/0/message/content").ok_or_else(|| {
        TranslateError::Malformed("openai: 没有 choices[0].message.content".into())
    })?;
    let text = match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => {
            let mut out = String::new();
            for part in parts {
                let piece = part
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| TranslateError::Malformed("openai: 内容段里没有 text".into()))?;
                out.push_str(piece);
            }
            out
        }
        _ => {
            return Err(TranslateError::Malformed(
                "openai: 内容既不是字符串也不是段表".into(),
            ));
        }
    };
    let text = text.trim();
    if text.is_empty() {
        return Err(TranslateError::Malformed("openai: 空译文".into()));
    }
    Ok(Translation {
        text: text.to_string(),
        // 这套协议不回检测到的源语；界面只在「自动」那一路显那一格，没有就不显。
        detected: None,
    })
}

#[cfg(test)]
mod tests {
    //! 请求形状、地址拼接、响应解析与缓存键。全部离线。

    use std::sync::Mutex;

    use serde_json::Value;

    use super::{OpenAiTranslator, language_name, parse_response};
    use crate::net::remote::{Error, Method, Request, Response, Transport};
    use crate::net::translate::{MAX_SOURCE_CHARS, TranslateError, Translator};

    const OK: &str = r#"{"choices":[{"message":{"role":"assistant","content":"你好"}}]}"#;

    fn translator() -> OpenAiTranslator {
        OpenAiTranslator::new("https://api.example.test/v1/", "gpt-4o-mini", "sk-test")
    }

    fn header<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
        req.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn the_request_is_a_post_with_the_readers_model_and_prompt() {
        let req = translator()
            .request("Hello & 你好", "auto", "zh-CN")
            .expect("地址与模型都在，拼得出来");
        assert_eq!(req.method, Method::Post);
        assert_eq!(req.url, "https://api.example.test/v1/chat/completions");
        assert_eq!(header(&req, "Authorization"), Some("Bearer sk-test"));
        assert_eq!(header(&req, "Content-Type"), Some("application/json"));

        let body: Value = serde_json::from_slice(&req.body).expect("请求体该是 JSON");
        assert_eq!(body["model"], "gpt-4o-mini");
        assert_eq!(body["messages"][0]["role"], "system");
        assert!(
            body["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("Simplified Chinese"),
            "目标语言要写进提示词"
        );
        assert_eq!(body["messages"][1]["content"], "Hello & 你好");
        assert!(
            body.get("temperature").is_none(),
            "推理模型会拒掉采样参数，别带"
        );
    }

    /// 地址写到方法那一层也认；空 key 不发 Authorization（本地服务不认它）。
    #[test]
    fn the_endpoint_is_completed_and_an_empty_key_sends_no_auth_header() {
        let direct = OpenAiTranslator::new(
            "https://example.test/v1/chat/completions/",
            "qwen2.5:7b",
            "",
        );
        assert_eq!(
            direct.endpoint(),
            "https://example.test/v1/chat/completions"
        );
        let req = direct.request("hi", "auto", "en").unwrap();
        assert_eq!(header(&req, "Authorization"), None);
    }

    /// 必填项为空：出网前就报「还没填」，别让服务端回 404 让读者猜。
    #[test]
    fn empty_settings_are_reported_before_any_request() {
        let no_base = OpenAiTranslator::new("", "m", "");
        assert!(matches!(
            no_base.request("hi", "auto", "en"),
            Err(TranslateError::NotConfigured("服务地址"))
        ));
        let no_model = OpenAiTranslator::new("https://example.test/v1", "", "");
        assert!(matches!(
            no_model.request("hi", "auto", "en"),
            Err(TranslateError::NotConfigured("模型"))
        ));
    }

    #[test]
    fn the_cache_key_tracks_model_and_endpoint_but_never_the_key() {
        let a = OpenAiTranslator::new("https://one.test/v1", "m1", "sk-a");
        let b = OpenAiTranslator::new("https://one.test/v1", "m1", "sk-b");
        assert_eq!(a.cache_key(), b.cache_key(), "换 key 不该换缓存");
        assert!(!a.cache_key().contains("sk-a"), "密钥不许进缓存键");

        let other_model = OpenAiTranslator::new("https://one.test/v1", "m2", "sk-a");
        assert_ne!(a.cache_key(), other_model.cache_key(), "换模型该换缓存");
        let other_base = OpenAiTranslator::new("https://two.test/v1", "m1", "sk-a");
        assert_ne!(a.cache_key(), other_base.cache_key(), "换地址该换缓存");
    }

    #[test]
    fn a_string_content_is_read_and_trimmed() {
        let got = parse_response(OK.as_bytes()).expect("fixture 该解析成功");
        assert_eq!(got.text, "你好");
        assert_eq!(got.detected, None);
    }

    /// 新一代模型 / 网关把 content 回成段表：按 text 拼回去，顺序不变。
    #[test]
    fn an_array_content_is_joined_in_order() {
        let body = r#"{"choices":[{"message":{"content":[
            {"type":"text","text":"你"},{"type":"text","text":"好"}
        ]}}]}"#;
        let got = parse_response(body.as_bytes()).expect("段表该拼得回一句");
        assert_eq!(got.text, "你好");
    }

    #[test]
    fn a_broken_response_is_a_malformed_error_not_a_panic() {
        for broken in [
            r#"{}"#,
            r#"{"choices":[]}"#,
            r#"{"choices":[{"message":{}}]}"#,
            r#"{"choices":[{"message":{"content":42}}]}"#,
            r#"{"choices":[{"message":{"content":[]}}]}"#,
            r#"{"choices":[{"message":{"content":"   "}}]}"#,
        ] {
            let got = parse_response(broken.as_bytes());
            assert!(
                matches!(got, Err(TranslateError::Malformed(_))),
                "{broken} 该报读不懂"
            );
        }
    }

    /// 超长源文在发出去之前截断，并原样落到 user 那一格。
    #[test]
    fn an_oversized_source_is_truncated_before_it_goes_out() {
        let capture = Capture {
            last: Mutex::new(None),
            body: OK,
        };
        let got = translator()
            .translate(&capture, &"字".repeat(MAX_SOURCE_CHARS + 100), "auto", "en")
            .expect("假传输回合法响应");
        assert_eq!(got.text, "你好");

        let req = capture.last.lock().unwrap().take().expect("该发出一次请求");
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let sent = body["messages"][1]["content"].as_str().unwrap();
        assert_eq!(sent.chars().count(), MAX_SOURCE_CHARS);
    }

    /// 认不出的语言码原样递进提示词。
    #[test]
    fn an_unknown_language_code_passes_through() {
        assert_eq!(language_name("yue"), "yue");
        assert_eq!(language_name("ja"), "Japanese");
    }

    struct Capture {
        last: Mutex<Option<Request>>,
        body: &'static str,
    }

    impl Transport for Capture {
        fn request(&self, req: Request) -> Result<Response, Error> {
            *self.last.lock().unwrap() = Some(req);
            Ok(Response::new(200, self.body.as_bytes().to_vec()))
        }

        fn download(
            &self,
            _req: Request,
            _sink: &mut dyn crate::net::remote::Sink,
            _cancel: &crate::net::remote::Cancel,
        ) -> Result<u64, Error> {
            unreachable!("翻译不发流式请求")
        }
    }
}
