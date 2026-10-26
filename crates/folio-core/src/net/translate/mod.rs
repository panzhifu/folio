//! 翻译：把一段文字交给外部引擎，换回目标语言的文本。
//!
//! 与 [`crate::net::remote`] 同一条分工：本模块定**该发什么、回来的东西怎么读**
//! （`Translator` trait 与三只引擎的请求/响应规则），字节怎么发出去归 [`Transport`]。
//! 于是响应解析那一半能在无网络、假传输的条件下逐字段钉死（见各文件测试）。
//!
//! ## 三只引擎
//!
//! - [`GoogleTranslator`] —— 免 key 的公开端点（`client=gtx`），零配置可用；
//!   它是**非合同**接口，限流（429）与改版都不在掌握之内。
//! - [`openai::OpenAiTranslator`] —— 任何说 OpenAI `chat/completions` 协议的服务
//!   （官方、DeepSeek、火山 ARK、本地 Ollama / LM Studio），地址与模型由读者填。
//! - [`deepl::DeepLTranslator`] —— DeepL 官方 API，免费 / 付费档按 key 后缀现选端点。
//!
//! 形状上从一开始就按「可换引擎」立起来：trait 只有 [`Translator::translate`] 与
//! [`Translator::cache_key`] 两个方法，缓存键里带引擎名（OpenAI 还带模型与地址的指纹），
//! 所以换引擎、换模型都不会把另一份答案认回来。
//!
//! ## 出网纪律
//!
//! 同步 [`Transport`] + 后台线程，与全仓同一条（`net` 模块文档）；**一次只发一个
//! 请求**由调用方（app 侧的会话）保证 —— 缓存命中就不出网，未命中排队等上一发落地。
//! 源文超长先截断（见 [`MAX_SOURCE_CHARS`]）。
//!
//! ## 错误分档
//!
//! [`TranslateError`] 把传输层的 WebDAV 词表翻成翻译场景能说的话：缺 key、密钥被拒、
//! 限流 / 配额、引擎返回的状态、响应读不懂。界面直接把这一句摆进失败卡片。

pub mod deepl;
pub mod openai;

use crate::infra::config::{Translation as TranslationConfig, TranslationEngine};
use crate::ingest::identity::sha256_of_bytes;
use crate::net::remote::{Error, Method, Request, Transport};

/// 源文超过多长就截断（字符）。Google 那档走 GET，查询串有总长上限；一整页选中
/// （约千字）要能整体过去，再长的选中（整章）本来就不是弹窗能读下的形状，截断是
/// 诚实的降级。三只引擎共用同一条上限：换引擎不该悄悄改变「多长算一段」。
pub const MAX_SOURCE_CHARS: usize = 4000;

/// 界面与引擎共用的目标语言短清单。标签是各语言自己的名字（不随界面语言翻译），
/// 所以不进 `locales/`；翻译弹层与设置页都读这一份，别处再抄就是两处真相。
pub const TARGET_LANGUAGES: &[(&str, &str)] = &[
    ("zh-CN", "简体中文"),
    ("zh-TW", "繁體中文"),
    ("en", "English"),
    ("ja", "日本語"),
    ("ko", "한국어"),
    ("de", "Deutsch"),
    ("fr", "Français"),
    ("es", "Español"),
    ("ru", "Русский"),
];

/// 一段译文：答案 + 引擎顺手说出的源语（自动检测时才有）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Translation {
    pub text: String,
    /// 引擎检测出的源语（BCP-47 码，如 `en`）。`from` 明确指定时引擎多半原样回它，
    /// 界面只在「自动检测」那一路显这一格。
    pub detected: Option<String>,
}

/// 一次翻译失败的档位。分档的判据是**读者能不能自己处理**：缺 key / key 被拒 /
/// 配额与限流各有各的动作，混成一句「翻译失败」就只剩重试一条路。
#[derive(Debug, thiserror::Error)]
pub enum TranslateError {
    #[error("还没有填 API Key")]
    MissingKey,

    /// 引擎的必填参数是空的（OpenAI 兼容那一档的服务地址 / 模型）。
    #[error("还没有填{0}")]
    NotConfigured(&'static str),

    /// 请求根本没成为一次 HTTP 往返（DNS、连接、TLS、超时都在这一档）。
    #[error("连不上：{0}")]
    Transport(String),

    /// 401 / 403：key 错了、过期了，或者这个账号没有权限。
    #[error("密钥无效或没有权限")]
    Unauthorized,

    /// 429 限流、DeepL 的 456 配额用完 —— 动作都是「过一会儿再来」。
    #[error("请求太频繁或配额已用完")]
    Quota,

    #[error("引擎返回 {0}")]
    Status(u16),

    #[error("响应读不懂：{0}")]
    Malformed(String),

    #[error("已取消")]
    Cancelled,
}

/// 传输层的错误 → 翻译场景的说法。WebDAV 那套词（「这个地址不接受 WebDAV 请求」）
/// 摆进翻译弹层只会误导读者，所以在这一层翻掉，别把原文漏到界面上。
impl From<Error> for TranslateError {
    fn from(err: Error) -> Self {
        match err {
            Error::Transport(reason) => Self::Transport(reason),
            // 传输层把 401 的 Digest 挑战单列了一支；对翻译而言它同样是「key 不对」。
            Error::DigestRequired | Error::Unauthorized { .. } | Error::Forbidden { .. } => {
                Self::Unauthorized
            }
            Error::NotFound => Self::Status(404),
            Error::NotWebDav => Self::Status(405),
            Error::Unexpected { status } => match status {
                429 | 456 => Self::Quota,
                other => Self::Status(other),
            },
            Error::Malformed(reason) => Self::Malformed(reason),
            Error::Cancelled => Self::Cancelled,
            Error::Io(err) => Self::Transport(err.to_string()),
        }
    }
}

/// 一个翻译引擎。实现要 `Send + Sync`：它跑在后台线程的池子里。
pub trait Translator: Send + Sync {
    /// 缓存表 `provider` 列里的键。**同一份配置必须给出同一个键，答案会变的配置
    /// 差异必须出现在键里** —— Google 是固定名；OpenAI 兼容那一档还要带模型与
    /// 地址指纹，否则换了模型会把上一只模型的答案认回来。API Key 不进键：换账号
    /// 不改变同一模型给出的答案，而把密钥写进数据库只会多一处泄漏面。
    fn cache_key(&self) -> String;

    /// 译一段话。`from` 给 `"auto"` 表示让引擎自己检测；`to` 是目标语言的
    /// BCP-47 码（`zh-CN`、`en`…）。网络、鉴权与响应形状的问题都以
    /// [`TranslateError`] 报。
    fn translate(
        &self,
        http: &dyn Transport,
        text: &str,
        from: &str,
        to: &str,
    ) -> Result<Translation, TranslateError>;
}

/// 按配置现建一只引擎。返回装箱 trait object：会话把它整只搬进后台作业，
/// 请求发出去之后即使配置又改了，这一趟用的仍是出发时那份配置。
pub fn configured(config: &TranslationConfig) -> Box<dyn Translator> {
    match config.engine {
        TranslationEngine::Google => Box::new(GoogleTranslator),
        TranslationEngine::OpenAi => Box::new(openai::OpenAiTranslator::new(
            &config.openai.base_url,
            &config.openai.model,
            &config.openai.api_key,
        )),
        TranslationEngine::DeepL => Box::new(deepl::DeepLTranslator::new(&config.deepl.api_key)),
    }
}

/// 源文的缓存键：SHA-256 的十六进制（`ingest::identity` 同一个函数、同一种编码）。
///
/// 放在 core 而不是调用方：缓存键的口径（什么算「同一段」）是数据的事 ——
/// 截断、空白折叠这些会改变键的预处理将来若要调整，改动只该发生在这一处。
pub fn source_hash(text: &str) -> String {
    sha256_of_bytes(text.as_bytes()).to_hex()
}

/// 每一趟真正送去引擎的源文：超过 [`MAX_SOURCE_CHARS`] 的部分截掉。
pub(crate) fn source_slice(text: &str) -> String {
    text.chars().take(MAX_SOURCE_CHARS).collect()
}

/// 查询串 / 表单体里的一个值。`urlencoding` 不在依赖里，而这三只引擎只需要
/// 「百分号编码」这一档：逐字节最保守也最对 —— 中文、空格、`&`、`=`、`+` 全部进去，
/// 引擎那边原样解回来。
pub(crate) fn encode_component(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// 三只引擎共用的响应解码：JSON 读不懂就报「读不懂」，交给界面的失败卡片。
pub(crate) fn parse_json(body: &[u8], engine: &str) -> Result<serde_json::Value, TranslateError> {
    serde_json::from_slice(body)
        .map_err(|err| TranslateError::Malformed(format!("{engine}: {err}")))
}

/// Google 翻译的免 key 档：`translate.googleapis.com/translate_a/single`，
/// GET，`client=gtx`。参数形状照 KOReader `translator.lua` 与 Foliate
/// `translate.html` 两处核对过的那份 —— `client` 用 `gtx`（上游注明用 `t` 会 403），
/// `dt=t` 只要主译文。
pub struct GoogleTranslator;

impl GoogleTranslator {
    /// 免 key 档的入口。名字就是缓存键里那个 `"google"`。
    pub const NAME: &'static str = "google";

    /// 把一段源文拼成 gtx 的请求（转义在这一步做完，[`Translator::translate`]
    /// 只管发）。单独一条是为了测试能钉住「源文里有什么字符都不至于拼坏 URL」。
    fn request(text: &str, from: &str, to: &str) -> Request {
        let url = format!(
            "https://translate.googleapis.com/translate_a/single\
             ?client=gtx&ie=UTF-8&oe=UTF-8&sl={from}&tl={to}&dt=t&q={}",
            encode_component(text)
        );
        Request::new(Method::Get, url)
            // gtx 对不带浏览器样的客户端并不挑头（429 与并发、节奏的关系更大），
            // 但明说我们是谁比装成浏览器诚实 —— 限流来了也怪不到伪装被拆穿。
            .header("Accept", "application/json")
    }
}

impl Translator for GoogleTranslator {
    fn cache_key(&self) -> String {
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
        let response = http.request(Self::request(&source, from, to))?;
        parse_gtx(&response.body, from)
    }
}

/// 解析 gtx 的响应体。成功形状是一个数组：`[0]` 是若干 `[译文, 原文, …]` 小数组
/// （长文被引擎切段，要拼回来），`[2]` 是检测出的源语。两处都做**形状防御** ——
/// 这是非合同接口，哪天它变了，错误要说「响应读不懂」，而不是 panic。
pub fn parse_gtx(body: &[u8], from: &str) -> Result<Translation, TranslateError> {
    let json = parse_json(body, "gtx")?;
    let segments = json
        .get(0)
        .and_then(|parts| parts.as_array())
        .ok_or_else(|| TranslateError::Malformed("gtx: 第 0 格不是段表".into()))?;
    let mut text = String::new();
    for part in segments {
        let piece = part
            .get(0)
            .and_then(|piece| piece.as_str())
            .ok_or_else(|| TranslateError::Malformed("gtx: 译文段不是字符串开头".into()))?;
        text.push_str(piece);
    }
    if text.is_empty() {
        return Err(TranslateError::Malformed("gtx: 没有译文段".into()));
    }
    let detected = if from.eq_ignore_ascii_case("auto") {
        json.get(2)
            .and_then(|value| value.as_str())
            .map(str::to_string)
    } else {
        None
    };
    Ok(Translation { text, detected })
}

#[cfg(test)]
mod tests {
    //! gtx 的响应解析、请求拼装、错误分档与引擎分发。全部离线：解析吃 fixture
    //! 字符串，请求只查形状。

    use super::{
        GoogleTranslator, MAX_SOURCE_CHARS, TranslateError, Translator, configured, parse_gtx,
        source_hash,
    };
    use crate::infra::config::{OpenAi, Translation as TranslationConfig, TranslationEngine};
    use crate::net::remote::{Error, Method, Transport};

    /// 三段切好的长文（引擎对长句分段的形状）+ 检测出的源语。
    const MULTI_SEGMENT: &str = r#"[
        [["first piece","原文一",null,null,10],[" second piece","原文二",null,null,10]],
        null,
        "en"
    ]"#;

    #[test]
    fn segments_are_joined_and_detection_is_read() {
        let got = parse_gtx(MULTI_SEGMENT.as_bytes(), "auto").expect("fixture 该解析成功");
        assert_eq!(got.text, "first piece second piece", "切段要拼回一句");
        assert_eq!(got.detected.as_deref(), Some("en"));
    }

    /// `from` 明确指定时不去读检测格：界面只在「自动」那一路显「检测到 xx」，
    /// 指定了还显就是自相矛盾。
    #[test]
    fn detection_is_skipped_when_the_source_is_pinned() {
        let got = parse_gtx(MULTI_SEGMENT.as_bytes(), "en").expect("fixture 该解析成功");
        assert_eq!(got.detected, None);
    }

    #[test]
    fn a_broken_response_is_a_malformed_error_not_a_panic() {
        for broken in [
            r#""just a string""#,
            r#"[]"#,
            r#"[null, null, "en"]"#,
            r#"[["not-a-pair"]]"#,
            r#"[[], null, "en"]"#,
        ] {
            let got = parse_gtx(broken.as_bytes(), "auto");
            assert!(
                matches!(got, Err(TranslateError::Malformed(_))),
                "{broken} 该报读不懂"
            );
        }
    }

    /// 源文里的特殊字符必须进得了查询串：`&` 会把参数切开、中文会进不了 URL，
    /// 两者都该被百分号编码。请求只查形状（前缀 + 参数齐全 + 无裸特殊字符）。
    #[test]
    fn the_request_escapes_everything_that_cuts_a_query() {
        let req = GoogleTranslator::request("甲 & 乙=丙?d", "auto", "zh-CN");
        assert_eq!(req.method, Method::Get);
        assert!(
            req.url
                .starts_with("https://translate.googleapis.com/translate_a/single?"),
        );
        assert!(req.url.contains("client=gtx"), "上游注明 client=t 会 403");
        assert!(req.url.contains("dt=t"), "只要主译文");
        assert!(req.url.contains("sl=auto"));
        assert!(req.url.contains("tl=zh-CN"));
        let encoded = &req.url[req.url.find("&q=").expect("q 参数该在") + 3..];
        for banned in ['&', '=', ' ', '甲', '乙', '?'] {
            assert!(!encoded.contains(banned), "q 里不该有裸的 {banned}");
        }
    }

    /// 超长源文在引擎那一层被截断 —— 一整章的选中不把查询串顶过 URL 上限。
    #[test]
    fn an_oversized_source_is_truncated_by_the_engine() {
        use std::sync::atomic::Ordering;
        let transport = CountingTransport {
            received_chars: std::sync::atomic::AtomicUsize::new(0),
        };
        let got = GoogleTranslator
            .translate(
                &transport,
                &"字".repeat(MAX_SOURCE_CHARS + 100),
                "auto",
                "en",
            )
            .expect("假传输直接回合法响应");
        assert_eq!(got.text, "ok");
        assert_eq!(
            transport.received_chars.load(Ordering::Relaxed),
            MAX_SOURCE_CHARS
        );
    }

    /// 只回一句合法 gtx、并记下实际收到了几个字符的假传输。
    struct CountingTransport {
        received_chars: std::sync::atomic::AtomicUsize,
    }
    impl CountingTransport {
        const BODY: &'static str = r#"[[["ok","原文",null,null,10]], null, "zh-CN"]"#;
    }
    impl Transport for CountingTransport {
        fn request(
            &self,
            req: crate::net::remote::Request,
        ) -> Result<crate::net::remote::Response, Error> {
            use std::sync::atomic::Ordering;
            // q 参数里是逐字节百分号编码：反解回去数**字符**，就是引擎实际收到的源文长度。
            let q = req.url.split("&q=").nth(1).expect("q 参数该在");
            self.received_chars
                .store(percent_decoded(q).chars().count(), Ordering::Relaxed);
            Ok(crate::net::remote::Response::new(
                200,
                Self::BODY.as_bytes().to_vec(),
            ))
        }

        fn download(
            &self,
            _req: crate::net::remote::Request,
            _sink: &mut dyn crate::net::remote::Sink,
            _cancel: &crate::net::remote::Cancel,
        ) -> Result<u64, Error> {
            unreachable!("翻译不发流式请求")
        }
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

    /// 缓存键是源文的 SHA-256 十六进制：同文同键、一字之差不同键。
    #[test]
    fn the_hash_keys_on_the_exact_source_text() {
        let a = source_hash("于是我们奋力向前划");
        assert_eq!(a, source_hash("于是我们奋力向前划"));
        assert_ne!(a, source_hash("于是我们奋力向前划。"));
        assert_eq!(a.len(), 64);
    }

    /// 传输层的错误翻成翻译场景的话：401/403 是密钥问题，429/456 是配额与限流，
    /// 其余状态照实报。WebDAV 那几句（405 →「不接受 WebDAV 请求」）不许漏到界面。
    #[test]
    fn transport_errors_are_translated_into_translation_terms() {
        assert!(matches!(
            TranslateError::from(Error::Unauthorized { status: 401 }),
            TranslateError::Unauthorized
        ));
        assert!(matches!(
            TranslateError::from(Error::Forbidden { status: 403 }),
            TranslateError::Unauthorized
        ));
        assert!(matches!(
            TranslateError::from(Error::DigestRequired),
            TranslateError::Unauthorized
        ));
        for status in [429, 456] {
            assert!(
                matches!(
                    TranslateError::from(Error::Unexpected { status }),
                    TranslateError::Quota
                ),
                "{status} 该报配额或限流"
            );
        }
        assert!(matches!(
            TranslateError::from(Error::NotWebDav),
            TranslateError::Status(405)
        ));
        assert!(matches!(
            TranslateError::from(Error::Unexpected { status: 503 }),
            TranslateError::Status(503)
        ));
        assert!(matches!(
            TranslateError::from(Error::Transport("dns".into())),
            TranslateError::Transport(_)
        ));
    }

    /// 分发只按配置里的引擎走：默认那档就是 Google，OpenAI 档带上读者填的模型。
    #[test]
    fn the_configured_engine_follows_the_engine_field() {
        let google = TranslationConfig::default();
        assert_eq!(configured(&google).cache_key(), "google");

        let openai = TranslationConfig {
            engine: TranslationEngine::OpenAi,
            openai: OpenAi {
                base_url: "https://example.test/v1".into(),
                model: "qwen2.5:7b".into(),
                api_key: "sk-secret".into(),
            },
            ..Default::default()
        };
        let key = configured(&openai).cache_key();
        assert!(key.starts_with("openai:qwen2.5:7b@"), "{key}");
        assert!(!key.contains("sk-secret"), "密钥不许进缓存键");

        let deepl = TranslationConfig {
            engine: TranslationEngine::DeepL,
            ..Default::default()
        };
        assert_eq!(configured(&deepl).cache_key(), "deepl");
    }
}
