//! 翻译：把一段文字交给外部引擎，换回目标语言的文本。
//!
//! 与 [`crate::net::remote`] 同一条分工：本模块定**该发什么、回来的东西怎么读**
//! （`Translator` trait 与 Google 那台的响应解析），字节怎么发出去归 [`Transport`]。
//! 于是响应解析那一半能在无网络、假传输的条件下逐字段钉死（见本文件测试）。
//!
//! ## 为什么引擎走 trait 而不是写死 Google
//!
//! 免 key 的非官方端点（`translate.googleapis.com`，`client=gtx`）是四家上游共用的
//! 那条路（KOReader `translator.lua`、Foliate `translate.html`），它让读者零配置就有
//! 翻译可用；但它是**非合同**的接口，限流（429）与改版都不在掌握之内。所以形状上
//! 一开始就按「可换引擎」立起来：trait 只有 [`Translator::translate`] 一个方法，
//! 缓存键里带引擎名 —— 第 2 期接自配 OpenAI 兼容 / DeepL 时，app 侧与缓存都不用动。
//!
//! ## 出网纪律
//!
//! 同步 [`Transport`] + 后台线程，与全仓同一条（`net` 模块文档）；**一次只发一个
//! 请求**由调用方（app 侧的会话）保证 —— 缓存命中就不出网，未命中排队等上一发落地。
//! 请求体走 GET 的查询串（gtx 只收 GET），源文要经过 URL 转义，超长先截断（见
//! [`MAX_SOURCE_CHARS`]）。

use crate::ingest::identity::sha256_of_bytes;
use crate::net::remote::{Error, Method, Request, Transport};

/// 源文超过多长就截断（字符）。gtx 走 GET，查询串有总长上限；一整页选中（约千字）
/// 要能整体过去，再长的选中（整章）本来就不是弹窗能读下的形状，截断是诚实的降级。
pub const MAX_SOURCE_CHARS: usize = 4000;

/// 一段译文：答案 + 引擎顺手说出的源语（自动检测时才有）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Translation {
    pub text: String,
    /// 引擎检测出的源语（BCP-47 码，如 `en`）。`from` 明确指定时引擎多半原样回它，
    /// 界面只在「自动检测」那一路显这一格。
    pub detected: Option<String>,
}

/// 一个翻译引擎。实现要 `Send + Sync`：它跑在后台线程的池子里。
pub trait Translator: Send + Sync {
    /// 引擎名，同时是缓存表 `provider` 列的键 —— **换实现不许换这个名字**，
    /// 否则同一份缓存就认不回来了。
    fn name(&self) -> &'static str;

    /// 译一段话。`from` 给 `"auto"` 表示让引擎自己检测；`to` 是目标语言的
    /// BCP-47 码（`zh-CN`、`en`…）。网络与响应形状的问题都以 [`Error`] 报。
    fn translate(
        &self,
        http: &dyn Transport,
        text: &str,
        from: &str,
        to: &str,
    ) -> Result<Translation, Error>;
}

/// 源文的缓存键：SHA-256 的十六进制（`ingest::identity` 同一个函数、同一种编码）。
///
/// 放在 core 而不是调用方：缓存键的口径（什么算「同一段」）是数据的事 ——
/// 截断、空白折叠这些会改变键的预处理将来若要调整，改动只该发生在这一处。
pub fn source_hash(text: &str) -> String {
    sha256_of_bytes(text.as_bytes()).to_hex()
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
        // `urlencoding` 不在依赖里，而 gtx 只需要「查询串安全」这一档转义：
        // 逐字节百分号编码最保守也最对 —— 中文、空格、`&`、`=`、`+` 全部进去，
        // 引擎那边原样解回来。长度是已知的，一次分配到位。
        let mut query = String::with_capacity(text.len() * 3 + 96);
        for byte in text.as_bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    query.push(*byte as char)
                }
                _ => query.push_str(&format!("%{byte:02X}")),
            }
        }
        let url = format!(
            "https://translate.googleapis.com/translate_a/single\
             ?client=gtx&ie=UTF-8&oe=UTF-8&sl={from}&tl={to}&dt=t&q={query}"
        );
        Request::new(Method::Get, url)
            // gtx 对不带浏览器样的客户端并不挑头（429 与并发、节奏的关系更大），
            // 但明说我们是谁比装成浏览器诚实 —— 限流来了也怪不到伪装被拆穿。
            .header("Accept", "application/json")
    }
}

impl Translator for GoogleTranslator {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn translate(
        &self,
        http: &dyn Transport,
        text: &str,
        from: &str,
        to: &str,
    ) -> Result<Translation, Error> {
        let source: String = text.chars().take(MAX_SOURCE_CHARS).collect();
        let response = http.request(Self::request(&source, from, to))?;
        parse_gtx(&response.body, from)
    }
}

/// 解析 gtx 的响应体。成功形状是一个数组：`[0]` 是若干 `[译文, 原文, …]` 小数组
/// （长文被引擎切段，要拼回来），`[2]` 是检测出的源语。两处都做**形状防御** ——
/// 这是非合同接口，哪天它变了，错误要说「响应读不懂」，而不是 panic。
pub fn parse_gtx(body: &[u8], from: &str) -> Result<Translation, Error> {
    let json: serde_json::Value =
        serde_json::from_slice(body).map_err(|err| Error::Malformed(err.to_string()))?;
    let segments = json
        .get(0)
        .and_then(|parts| parts.as_array())
        .ok_or_else(|| Error::Malformed("gtx: 第 0 格不是段表".into()))?;
    let mut text = String::new();
    for part in segments {
        let piece = part
            .get(0)
            .and_then(|piece| piece.as_str())
            .ok_or_else(|| Error::Malformed("gtx: 译文段不是字符串开头".into()))?;
        text.push_str(piece);
    }
    if text.is_empty() {
        return Err(Error::Malformed("gtx: 没有译文段".into()));
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
    //! gtx 的响应解析与请求拼装。全部离线：解析吃 fixture 字符串，请求只查形状。

    use super::{GoogleTranslator, MAX_SOURCE_CHARS, Translator, parse_gtx, source_hash};
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
            assert!(matches!(got, Err(Error::Malformed(_))), "{broken} 该报读不懂");
        }
    }

    /// 源文里的特殊字符必须进得了查询串：`&` 会把参数切开、中文会进不了 URL，
    /// 两者都该被百分号编码。请求只查形状（前缀 + 参数齐全 + 无裸特殊字符）。
    #[test]
    fn the_request_escapes_everything_that_cuts_a_query() {
        let req = GoogleTranslator::request("甲 & 乙=丙?d", "auto", "zh-CN");
        assert_eq!(req.method, Method::Get);
        assert!(req.url.starts_with("https://translate.googleapis.com/translate_a/single?"),);
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
            .translate(&transport, &"字".repeat(MAX_SOURCE_CHARS + 100), "auto", "en")
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
                out.push(u8::from_str_radix(
                    std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap(),
                    16,
                )
                .unwrap());
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
}
