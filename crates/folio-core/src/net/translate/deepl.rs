//! DeepL 官方 API：`POST /v2/translate`，表单体。
//!
//! 免费档与付费档只差域名：key 以 `:fx` 结尾走 `api-free.deepl.com`，其余走
//! `api.deepl.com`。不单列一个「免费档」开关 —— 那等于让读者把同一件事声明两遍，
//! 而两处对不上时没有任何一边能赢。
//!
//! 目标语言名不是 BCP-47：DeepL 要 `ZH-HANS` / `EN-US` 这种大写码，且中文必须
//! 区分简繁。映射写在这里，界面与配置继续用统一的 BCP-47。

use serde_json::Value;

use super::{TranslateError, Translation, Translator, encode_component, parse_json, source_slice};
use crate::net::remote::{Method, Request, Transport};

/// 一只 DeepL 引擎。key 是建的那一刻的快照。
pub struct DeepLTranslator {
    api_key: String,
}

impl DeepLTranslator {
    pub const NAME: &'static str = "deepl";
    pub const FREE_ENDPOINT: &'static str = "https://api-free.deepl.com/v2/translate";
    pub const PRO_ENDPOINT: &'static str = "https://api.deepl.com/v2/translate";

    pub fn new(api_key: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
        }
    }

    /// 免费档的 key 以 `:fx` 结尾（DeepL 自己的约定）。判不出来就按付费档走：
    /// 付费档的 key 在免费域名上一定被拒，而免费档的 key 在付费域名上同样被拒 ——
    /// 两种猜错都只是 403，读者能在失败卡片里看到「密钥无效」。
    fn endpoint(&self) -> &'static str {
        if self.api_key.ends_with(":fx") {
            Self::FREE_ENDPOINT
        } else {
            Self::PRO_ENDPOINT
        }
    }

    fn request(&self, text: &str, from: &str, to: &str) -> Result<Request, TranslateError> {
        if self.api_key.is_empty() {
            return Err(TranslateError::MissingKey);
        }
        let mut form = format!(
            "text={}&target_lang={}",
            encode_component(text),
            encode_component(&target_code(to))
        );
        // 不给 source_lang = 自动检测；指定了才带上。
        if !from.eq_ignore_ascii_case("auto") {
            form.push_str(&format!(
                "&source_lang={}",
                encode_component(&source_code(from))
            ));
        }
        Ok(Request::new(Method::Post, self.endpoint().to_string())
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .header("Authorization", format!("DeepL-Auth-Key {}", self.api_key))
            .body(form.into_bytes()))
    }
}

impl Translator for DeepLTranslator {
    fn cache_key(&self) -> String {
        // 免费 / 付费档给出的译文是同一套模型；key 不进缓存键（理由同 OpenAI 那档）。
        Self::NAME.to_string()
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
        let mut got = parse_response(&response.body)?;
        // 与 gtx 同一条口径：只有「自动检测」那一路显源语。
        if !from.eq_ignore_ascii_case("auto") {
            got.detected = None;
        }
        Ok(got)
    }
}

/// BCP-47 → DeepL 的 `target_lang`。中文必须分简繁；英文默认美式（DeepL 也认
/// `EN-GB`，但读者只给了 `en`，挑一个写成明确的）。
fn target_code(code: &str) -> String {
    match code {
        "zh-CN" | "zh-Hans" => "ZH-HANS",
        "zh-TW" | "zh-Hant" => "ZH-HANT",
        "en" => "EN-US",
        "ja" => "JA",
        "ko" => "KO",
        "de" => "DE",
        "fr" => "FR",
        "es" => "ES",
        "ru" => "RU",
        other => return other.to_ascii_uppercase(),
    }
    .to_string()
}

/// BCP-47 → DeepL 的 `source_lang`。源语这一侧不分简繁：DeepL 只收 `ZH`。
fn source_code(code: &str) -> String {
    match code {
        "zh-CN" | "zh-TW" | "zh-Hans" | "zh-Hant" => "ZH",
        "en" => "EN",
        other => return other.to_ascii_uppercase(),
    }
    .to_string()
}

/// 解析 `translations[0]`。DeepL 的检测源语是大写（`EN`），这里统一成小写，
/// 与 gtx 那一路在弹层里显同一个样子。
fn parse_response(body: &[u8]) -> Result<Translation, TranslateError> {
    let json = parse_json(body, "deepl")?;
    let item = json
        .pointer("/translations/0")
        .ok_or_else(|| TranslateError::Malformed("deepl: 没有 translations[0]".into()))?;
    let text = item
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| TranslateError::Malformed("deepl: 译文不是字符串".into()))?;
    let text = text.trim();
    if text.is_empty() {
        return Err(TranslateError::Malformed("deepl: 空译文".into()));
    }
    let detected = item
        .get("detected_source_language")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase);
    Ok(Translation {
        text: text.to_string(),
        detected,
    })
}

#[cfg(test)]
mod tests {
    //! 端点选择、表单体、语言映射与响应解析。全部离线。

    use std::sync::Mutex;

    use super::{DeepLTranslator, parse_response, source_code, target_code};
    use crate::net::remote::{Error, Method, Request, Response, Transport};
    use crate::net::translate::{MAX_SOURCE_CHARS, TranslateError, Translator};

    const OK: &str = r#"{"translations":[
        {"detected_source_language":"EN","text":"你好"}
    ]}"#;

    fn header<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
        req.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// 免费档的 key 选免费域名，其余按付费域名。
    #[test]
    fn the_endpoint_follows_the_key_suffix() {
        assert_eq!(
            DeepLTranslator::new("abc:fx").endpoint(),
            DeepLTranslator::FREE_ENDPOINT
        );
        assert_eq!(
            DeepLTranslator::new("abc").endpoint(),
            DeepLTranslator::PRO_ENDPOINT
        );
    }

    #[test]
    fn the_request_is_a_form_post_without_a_source_language_when_auto() {
        let req = DeepLTranslator::new("k:fx")
            .request("Hello & 你好", "auto", "zh-CN")
            .expect("key 在，拼得出来");
        assert_eq!(req.method, Method::Post);
        assert_eq!(req.url, DeepLTranslator::FREE_ENDPOINT);
        assert_eq!(header(&req, "Authorization"), Some("DeepL-Auth-Key k:fx"));
        assert_eq!(
            header(&req, "Content-Type"),
            Some("application/x-www-form-urlencoded")
        );
        let form = String::from_utf8(req.body.clone()).unwrap();
        assert!(form.contains("target_lang=ZH-HANS"), "{form}");
        assert!(!form.contains("source_lang"), "自动检测时不带源语");
        // 逐字节百分号编码：`&` 与中文都不许裸着进表单体。
        assert!(!form.contains(" & "));
        assert!(!form.contains('你'));
        assert!(!form.contains('&') || form.split('&').count() == 2);
    }

    /// 指定源语时才带 `source_lang`，且中文源语统一成 `ZH`。
    #[test]
    fn a_pinned_source_language_is_mapped_and_sent() {
        let req = DeepLTranslator::new("k")
            .request("hi", "zh-CN", "en")
            .unwrap();
        let form = String::from_utf8(req.body).unwrap();
        assert!(form.contains("source_lang=ZH"), "{form}");
        assert!(form.contains("target_lang=EN-US"), "{form}");
    }

    #[test]
    fn every_target_language_maps_to_deepls_spelling() {
        for (code, expected) in [
            ("zh-CN", "ZH-HANS"),
            ("zh-TW", "ZH-HANT"),
            ("en", "EN-US"),
            ("ja", "JA"),
            ("ko", "KO"),
            ("de", "DE"),
            ("fr", "FR"),
            ("es", "ES"),
            ("ru", "RU"),
        ] {
            assert_eq!(target_code(code), expected, "{code}");
        }
        assert_eq!(target_code("yue"), "YUE", "认不出的码大写递过去");
        assert_eq!(source_code("zh-TW"), "ZH");
        assert_eq!(source_code("en"), "EN");
    }

    /// 没填 key：出网前就报，别让 DeepL 回 403 让读者猜。
    #[test]
    fn an_empty_key_is_reported_before_any_request() {
        let got = DeepLTranslator::new("").request("hi", "auto", "en");
        assert!(matches!(got, Err(TranslateError::MissingKey)));
    }

    #[test]
    fn a_translation_is_read_and_the_detected_language_is_lowercased() {
        let got = parse_response(OK.as_bytes()).expect("fixture 该解析成功");
        assert_eq!(got.text, "你好");
        assert_eq!(got.detected.as_deref(), Some("en"), "统一成小写");
    }

    /// 指定源语时检测格不显（与 gtx 同一条口径）。
    #[test]
    fn a_pinned_source_hides_the_detection() {
        let capture = Capture {
            last: Mutex::new(None),
            body: OK,
        };
        let got = DeepLTranslator::new("k")
            .translate(&capture, "hi", "en", "zh-CN")
            .expect("假传输回合法响应");
        assert_eq!(got.detected, None);
    }

    #[test]
    fn a_broken_response_is_a_malformed_error_not_a_panic() {
        for broken in [
            r#"{}"#,
            r#"{"translations":[]}"#,
            r#"{"translations":[{}]}"#,
            r#"{"translations":[{"text":42}]}"#,
            r#"{"translations":[{"text":"  "}]}"#,
        ] {
            let got = parse_response(broken.as_bytes());
            assert!(
                matches!(got, Err(TranslateError::Malformed(_))),
                "{broken} 该报读不懂"
            );
        }
    }

    /// 超长源文在发出去之前截断。
    #[test]
    fn an_oversized_source_is_truncated_before_it_goes_out() {
        let capture = Capture {
            last: Mutex::new(None),
            body: OK,
        };
        DeepLTranslator::new("k")
            .translate(&capture, &"字".repeat(MAX_SOURCE_CHARS + 100), "auto", "en")
            .expect("假传输回合法响应");

        let req = capture.last.lock().unwrap().take().expect("该发出一次请求");
        let form = String::from_utf8(req.body).unwrap();
        let text = form
            .strip_prefix("text=")
            .and_then(|rest| rest.split('&').next())
            .expect("表单体该以 text 开头");
        assert_eq!(percent_decoded(text).chars().count(), MAX_SOURCE_CHARS);
    }

    fn percent_decoded(text: &str) -> String {
        let bytes = text.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' {
                out.push(
                    u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap(), 16)
                        .unwrap(),
                );
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        String::from_utf8(out).unwrap()
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
