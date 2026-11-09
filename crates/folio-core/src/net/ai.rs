//! AI 问答：一层 provider（OpenAI 兼容的 `chat/completions`）与三档预设问法的提示词。
//!
//! 形状照 [`crate::net::translate`] 立：同一套 [`Transport`]、同一套错误分档、同一条
//! 「出网前先判必填项」的纪律，所以请求形状与响应解析都能在**无网络、假传输**的条件下
//! 逐字段钉死（见文件末的测试）。翻译与 AI 是同一件事的两张脸 —— 都是「把一段书本文
//! 交给一个外部模型，拿回一段文字」，两份实现各长一套错误词表与解析，迟早在一处修了
//! 另一处忘。
//!
//! ## 地址与密钥共用翻译那一档
//!
//! [`configured`] 吃的是 `Translation` + `Ai` 两份配置：服务地址与密钥住在
//! [`crate::infra::config::OpenAi`]（翻译那档），AI 这一节只回答两件事 —— **要不要用**
//! （[`Ai::enabled`]，默认关）与**用哪只模型**（可留空，留空就是与翻译同一只模型）。
//! 密钥不复制第二份：多一处落盘面就多一处泄漏面，而读者刚填过一遍的东西不该问第二遍。
//!
//! ## 四种「回 None」与它们的画法
//!
//! [`configured`] 回 `None` 的四种情况：没开、地址空、模型两处都空、只有出厂那份地址
//! 且 key 没填（读者从没打开过这一格，见 [`configured`] 的文档）。界面对 `None` 的画法
//! 一律是**这一格不出现**（与听书在没有后端的那些平台上同一条判据），而不是摆一颗按下
//! 去报错的按钮。
//!
//! ## 日志纪律
//!
//! 正文与答案**都不进日志**。要出声的只有模型名、字符数、耗时、错误档位 ——
//! 读者选中的那段书本文不是诊断信息，而它可能正是他正在读的那一页。

use serde_json::{Value, json};

use crate::infra::config::{Ai, OpenAi, Translation as TranslationConfig};
use crate::infra::identity::sha256_of_bytes;
use crate::net::remote::{Error, Method, Request, Response, Transport};
use crate::net::translate::{TranslateError, language_name};

/// 一次请求送出去的文本上限（字符）。选区那一档实际用的是 [`CONTEXT_BUDGET`] 那一圈，
/// 这个数是最后一道闸：草案那一刀要送几十行样本，比选区长，但也不该长过这个量 ——
/// 再长的输入本来就不是「问一句」的形状，而是把整本书递出去。
pub const MAX_INPUT_CHARS: usize = 12_000;

/// 选区那一档的上下文预算（字符）：选中那段**前**与**后**各取这么多，各自收顶。
///
/// 这个数是 `doc/AI 计划.md` 里定的那一个，不是工程上凑的：它同时是成本（一次问答
/// ≈1000 token 输入）和「这次离开设备的有多少」。**改它要连那份文档一起改**，
/// 不然文档里那句「只送选中的那段加前后各一截」就成了假话。
pub const CONTEXT_BUDGET: usize = 300;

/// 答案的显示上限（字符）。弹层是一张卡，不是一篇文章：模型顺带写的长篇要收得住。
pub const MAX_ANSWER_CHARS: usize = 4_000;

/// 两道闸的**先后**关系要成立：输入比答案宽，否则长答案在发出去之前就被自己收掉了，
/// 而界面上看起来是「模型只回了一句」。写在 const 块里（编译期就判），不占一次运行。
const _: () = assert!(MAX_INPUT_CHARS > MAX_ANSWER_CHARS);

/// 三档预设问法。自由输入框**故意不做**（第一批）：三档覆盖了「读不懂」的三种原因，
/// 而自由输入要连提示词编辑、长度上限与注入防护一起做，那是另一刀。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    /// 这段在说什么。默认档。
    Explain,
    /// 这段里的人名、地名、典故、术语各是什么。
    Terms,
    /// 这段的原文（古文、方言、翻译腔）用现在的说法是怎么讲。
    Plain,
}

impl Ask {
    /// 全部三档，按界面上该出现的顺序。加一档忘了在这里挂，界面上就永远选不到它。
    pub const ALL: [Ask; 3] = [Ask::Explain, Ask::Terms, Ask::Plain];

    /// 配置与日志里的那个子键。
    pub fn key(self) -> &'static str {
        match self {
            Self::Explain => "explain",
            Self::Terms => "terms",
            Self::Plain => "plain",
        }
    }

    /// 系统提示词。三条都收在「只回答案、不要前言后语」这一档：弹层只画一段正文，
    /// 模型顺带写的「当然可以，下面我来解释」会把一张卡撑成一篇文章。
    ///
    /// 回答语言**由调用方给**（界面语言），不写死中文：这条链要能跟着 `Locale` 换，
    /// 不然换到韩语界面的读者拿到的是英文解释。
    pub fn system(self, answer_lang: &str) -> String {
        let language = language_name(answer_lang);
        let task = match self {
            Self::Explain => {
                "Explain what the quoted passage says and why it matters, in plain prose."
            }
            Self::Terms => {
                "List the names, places, allusions and technical terms in the quoted passage, each with a one-line gloss."
            }
            Self::Plain => {
                "Retell the quoted passage in ordinary modern language, keeping the meaning."
            }
        };
        format!(
            "You are a reading assistant inside an ebook reader. {task} Answer in {language}. \
             Quote nothing longer than a sentence. Reply with the answer only — no preamble, \
             no restating the question."
        )
    }
}

/// 一次问答的载荷：答案 + 模型说它自己用了多少 token（很多网关不给，所以是 `Option`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub text: String,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
}

/// 一次 AI 请求失败的档位。**与 [`TranslateError`] 共用一份词表的意思**：分档的判据是
/// 「读者能不能自己处理」，混成一句「AI 失败了」就只剩重试一条路。
#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("还没有填 API Key")]
    MissingKey,

    #[error("还没有填{0}")]
    NotConfigured(&'static str),

    #[error("连不上：{0}")]
    Transport(String),

    #[error("密钥无效或没有权限")]
    Unauthorized,

    #[error("请求太频繁或配额已用完")]
    Quota,

    #[error("服务返回 {0}")]
    Status(u16),

    #[error("应答读不懂：{0}")]
    Malformed(String),

    #[error("已取消")]
    Cancelled,
}

impl From<Error> for AiError {
    fn from(err: Error) -> Self {
        match err {
            Error::Transport(reason) => Self::Transport(reason),
            Error::DigestRequired | Error::Unauthorized { .. } | Error::Forbidden { .. } => {
                Self::Unauthorized
            }
            Error::NotFound => Self::Status(404),
            Error::NotWebDav => Self::Status(405),
            Error::Unexpected { status } => match status {
                429 => Self::Quota,
                other => Self::Status(other),
            },
            Error::Malformed(reason) => Self::Malformed(reason),
            Error::Cancelled => Self::Cancelled,
            Error::Io(err) => Self::Transport(err.to_string()),
        }
    }
}

/// 「这本书该怎么切」那一趟的系统提示词（`.txt` 分章规则草案）。
///
/// 三条要紧的话都写在词面上，因为每一条都对应一种会**静悄悄改坏一整本书**的坏答案：
///
/// - **只回一个 JSON 对象**：草案要机器解，一段带前言的答案解不开就是白问一次；
/// - **模式按单行 `is_match` 测，所以要锚 `^`**：引擎对每一行跑一次 `is_match`，不锚
///   就会在正文中间撞到「第三章」那三个字然后把正文切成碎片（`novel::draft` 的预览
///   拦得住，但拦得住不等于该发出去）；
/// - **号放捕获组一**：递增校验（真章的号要连续）读的就是那一格，没有它整本的号都读不出。
///
/// `answer_lang` 只作用于 `reason` 那一句 —— 规则本体（`pattern`）不是给人读的文本，
/// 而界面语言换到韩语时那句理由该跟着换。
pub fn draft_system(answer_lang: &str) -> String {
    let language = language_name(answer_lang);
    format!(
        r#"You are a chapter-splitting assistant for plain-text novels. From the sampled lines
         below, propose ONE rule that matches this book's chapter-heading lines. Reply with a
         single JSON object and nothing else:
         {{"name": string, "kind": "regex" or "literal", "pattern": string, "text": string,
          "numbering": "none" or "chinese" or "arabic" or "either", "reason": string}}
         The engine tests `pattern` against one line at a time with Rust regex `is_match`, so
         anchor it at `^` and keep it tight enough that ordinary prose never matches it. Put the
         chapter number in capture group 1 when the headings are numbered. Use "kind": "literal"
         with `text` only when every heading is the exact same line. Match only shapes visible in
         the sample - never invent a heading style. Write `name` as a short label and `reason`
         as one sentence in {language}."#
    )
}

/// 那两份样本 → 这一趟的正文。两份都要标出来是哪一份：候选行是**挑出来的**（不连续、
/// 按位置铺开），与头部那 60 行连成的样子不同，混在一起模型会以为书里全是标题。
pub fn draft_prompt(head: &[String], candidates: &[String]) -> String {
    let block = |lines: &[String]| lines.join("\n");
    format!(
        "First lines of the file, in order:\n<<<\n{}\n>>>\nLines elsewhere in the file that \
         look like headings, spread across the whole book:\n<<<\n{}\n>>>",
        block(head),
        block(candidates)
    )
}

/// 一层 provider。名字与配置那一节 [`crate::infra::config::Ai`] 分开：那一节说的是
/// 「要不要用、用哪只模型」，这一格说的是「怎么问一句」。实现要 `Send + Sync`：
/// 它跑在后台线程的池子里。
pub trait Assistant: Send + Sync {
    /// 缓存键里的 provider 名。**同一份配置必须给出同一个键，答案会变的差异必须出现在
    /// 键里** —— 与翻译那条一模一样，所以这里带上模型与地址的指纹。密钥不进键。
    fn cache_key(&self) -> String;

    /// 弹层头部那行给人看的名字（模型名在内的品牌串）。
    fn label(&self) -> String;

    /// 问一句。`system` 是 [`Ask::system`] 那一档的提示词，`user` 是拼好的上下文与原文。
    fn ask(&self, http: &dyn Transport, system: &str, user: &str) -> Result<Answer, AiError>;
}

/// OpenAI 兼容的一层：`POST {base}/chat/completions`。一份配置覆盖所有说这套协议的服务
/// （官方、DeepSeek、火山 ARK、本地 Ollama / LM Studio）。
pub struct ChatCompletion {
    base_url: String,
    model: String,
    api_key: String,
}

impl ChatCompletion {
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

    /// 拼请求。必填项为空时在**出网前**就报出来：让读者去设置页补地址，比让服务端
    /// 回一句 `404 page not found` 有用。
    fn request(&self, system: &str, user: &str) -> Result<Request, AiError> {
        if self.base_url.trim().is_empty() {
            return Err(AiError::NotConfigured("服务地址"));
        }
        if self.model.trim().is_empty() {
            return Err(AiError::NotConfigured("模型"));
        }
        let body = json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
        });
        let mut req = Request::new(Method::Post, self.endpoint())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .body(body.to_string().into_bytes());
        // 本地 Ollama / LM Studio 不认 Authorization：空 key 就不带头，由服务自己决定
        // 要不要拒。这一层不猜「这个地址要不要 key」。
        if !self.api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", self.api_key));
        }
        Ok(req)
    }
}

impl Assistant for ChatCompletion {
    fn cache_key(&self) -> String {
        // 地址也进指纹：同一个模型名在两台服务上可能不是同一个模型（网关会改路由）。
        let base = sha256_of_bytes(self.base_url.as_bytes()).to_hex();
        format!("{}:{}@{}", Self::NAME, self.model, &base[..8])
    }

    fn label(&self) -> String {
        if self.model.is_empty() {
            "AI".to_string()
        } else {
            format!("AI · {}", self.model)
        }
    }

    fn ask(&self, http: &dyn Transport, system: &str, user: &str) -> Result<Answer, AiError> {
        let user = clip(user, MAX_INPUT_CHARS);
        let response = http.request(self.request(system, &user)?)?;
        parse_response(&response)
    }
}

/// 按两份配置现建 provider。**没开 / 地址空 / 两处模型都空 / 只有出厂那份地址且没填
/// key** 都回 `None`：那几种情况下界面对「这一格不存在」的画法已经写好了（与听书同一
/// 条判据），不在这里替读者编一个端点。
///
/// 第四条不是多余的谨慎：出厂把官方地址与模型填进了翻译那一档（见 `config::OpenAi` 的
/// `Default`，读者只贴 key 就能用），所以「地址 == 出厂那个 + key 空着」唯一的解释是
/// **读者从没打开过这一格**。这时候把选中的正文 POST 出去，是替读者做了一个他没做过的
/// 决定，而它只会换一个 401。反过来，本地 Ollama / LM Studio 那类不认 key 的服务地址
/// 必然是读者自己填的，那条路照常通 —— 所以这里问的是「这个值是读者给的还是我们给的」，
/// 不是「这个服务要不要 key」（那一格归 [`ChatCompletion::request`]，它不猜）。
pub fn configured(translation: &TranslationConfig, ai: &Ai) -> Option<Box<dyn Assistant>> {
    if !ai.enabled {
        return None;
    }
    let OpenAi {
        base_url,
        model,
        api_key,
    } = &translation.openai;
    let model = if ai.model.trim().is_empty() {
        model.trim().to_string()
    } else {
        ai.model.trim().to_string()
    };
    if base_url.trim().is_empty() || model.is_empty() {
        return None;
    }
    if api_key.trim().is_empty() && base_url == &OpenAi::default().base_url {
        return None;
    }
    Some(Box::new(ChatCompletion::new(base_url, &model, api_key)))
}

/// 解析应答。`content` 既可能是字符串，也可能是 `[{type:"text", text:"…"}]` 的段表
/// （新一代模型与网关会这么回），两种都拼 —— 与翻译那档同一条，因为说这套协议的服务
/// 两种都有人在用。
fn parse_response(response: &Response) -> Result<Answer, AiError> {
    let json: Value = serde_json::from_slice(&response.body)
        .map_err(|err| AiError::Malformed(format!("openai: {err}")))?;
    let content = json
        .pointer("/choices/0/message/content")
        .ok_or_else(|| AiError::Malformed("openai: 没有 choices[0].message.content".to_string()))?;
    let text = match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => {
            let mut out = String::new();
            for part in parts {
                let piece = part
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| AiError::Malformed("openai: 内容段里没有 text".to_string()))?;
                out.push_str(piece);
            }
            out
        }
        _ => {
            return Err(AiError::Malformed(
                "openai: 内容既不是字符串也不是段表".to_string(),
            ));
        }
    };
    let usage = json.get("usage");
    Ok(Answer {
        text: clip(text.trim(), MAX_ANSWER_CHARS),
        prompt_tokens: usage
            .and_then(|u| u.get("prompt_tokens"))
            .and_then(Value::as_u64),
        completion_tokens: usage
            .and_then(|u| u.get("completion_tokens"))
            .and_then(Value::as_u64),
    })
}

/// 按**字符**收长度（不是字节）：中文一个字三字节，按字节切会把半个字留在结尾，
/// 那半字在界面上就是一个 `?`。
pub fn clip(text: &str, limit: usize) -> String {
    let mut out: String = text.chars().take(limit).collect();
    if text.chars().count() > limit {
        out.push('…');
    }
    out
}

/// 选区那一档送出去的那一份正文 —— **就是会离开设备的字的全集**。
///
/// 书名与章名是给模型的坐标：没有出处，「这段在说什么」问的是一段无头引文，模型只能
/// 猜（人名认成同名演员、地名认成另一个）。它们是元数据，不是读者的私密输入，而正文
/// 只取选区那一段加前后各一截 —— `doc/AI 计划.md` 的隐私那一节列的就是这五项，
/// **加一格等于改口径**，要连那份文档一起改。
pub struct Asked<'a> {
    pub book: &'a str,
    pub chapter: &'a str,
    /// 读者挑中的那段。
    pub selected: &'a str,
    pub before: &'a str,
    pub after: &'a str,
}

/// 选区那一档送给模型的正文：出处 + 前文 + 选中的那段 + 后文。
///
/// 前文与后文是**同一段排版文本**的两端（调用方给的是章内字符偏移两侧现取的），
/// 所以拼接要标出来哪一段是读者挑中的 —— 不标，模型会把前文也当成「被问的东西」。
/// 两侧各收 [`CONTEXT_BUDGET`]；选区与两个标题不收（选区最长也就一页，标题来自元数据，
/// 整段仍在 [`MAX_INPUT_CHARS`] 那道总闸之内）。
pub fn selection_prompt(asked: &Asked) -> String {
    let mark = |text: &str| clip(text.trim(), CONTEXT_BUDGET);
    let book = asked.book.trim();
    let chapter = asked.chapter.trim();
    let mut out = String::new();
    if !book.is_empty() || !chapter.is_empty() {
        out.push_str(&format!(
            "From the book \"{book}\", chapter \"{chapter}\".\n\n"
        ));
    }
    out.push_str(&format!(
        "Passage being asked about:\n<<<\n{}\n>>>\nText just before it:\n<<<\n{}\n>>>\n\
         Text just after it:\n<<<\n{}\n>>>",
        mark(asked.selected),
        mark(asked.before),
        mark(asked.after)
    ));
    out
}

/// 翻译那层的错误词表与这一份是同一套意思，但**类型不能共用**：翻译的
/// [`TranslateError::NotConfigured("服务地址")`] 出现在 AI 的失败卡片上会说出「引擎」
/// 这种不属于这一屏的词。这一句只是把两条链的对应关系记下来。
impl From<TranslateError> for AiError {
    fn from(err: TranslateError) -> Self {
        match err {
            TranslateError::MissingKey => Self::MissingKey,
            TranslateError::NotConfigured(field) => Self::NotConfigured(field),
            TranslateError::Transport(reason) => Self::Transport(reason),
            TranslateError::Unauthorized => Self::Unauthorized,
            TranslateError::Quota => Self::Quota,
            TranslateError::Status(status) => Self::Status(status),
            TranslateError::Malformed(reason) => Self::Malformed(reason),
            TranslateError::Cancelled => Self::Cancelled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AiError, Ask, Asked, Assistant, CONTEXT_BUDGET, ChatCompletion, MAX_ANSWER_CHARS,
        configured, draft_prompt, draft_system, parse_response, selection_prompt,
    };
    use crate::infra::config::{
        Ai as AiConfig, OpenAi, Translation as TranslationConfig, TranslationEngine,
    };
    use crate::net::remote::{Error, Response, Transport};
    use std::sync::Mutex;

    /// 假传输：记下最后一个请求，回一段固定 body。与 `net::translate` 那一族同一个形状，
    /// 所以这一层的测试也不需要网络。
    struct Capture {
        last: Mutex<Option<crate::net::remote::Request>>,
        body: &'static str,
    }

    impl Capture {
        fn new(body: &'static str) -> Self {
            Self {
                last: Mutex::new(None),
                body,
            }
        }

        fn last(&self) -> crate::net::remote::Request {
            self.last.lock().unwrap().clone().expect("该发过一次请求")
        }
    }

    impl Transport for Capture {
        fn request(&self, req: crate::net::remote::Request) -> Result<Response, Error> {
            *self.last.lock().unwrap() = Some(req);
            Ok(Response::new(200, self.body.as_bytes().to_vec()))
        }

        fn download(
            &self,
            _req: crate::net::remote::Request,
            _sink: &mut dyn crate::net::remote::Sink,
            _cancel: &crate::net::remote::Cancel,
        ) -> Result<u64, Error> {
            unreachable!("AI 问答不发流式请求")
        }
    }

    /// 翻译那档的地址与模型（AI 共用它们），密钥照填。
    fn translation(base: &str, model: &str) -> TranslationConfig {
        arm(base, model, "k")
    }

    /// OpenAI 兼容那一档的三个值：地址、模型、密钥。
    fn arm(base: &str, model: &str, key: &str) -> TranslationConfig {
        TranslationConfig {
            engine: TranslationEngine::OpenAi,
            openai: OpenAi {
                base_url: base.to_string(),
                model: model.to_string(),
                api_key: key.to_string(),
            },
            ..Default::default()
        }
    }

    /// 开了、模型跟随翻译那一档。
    fn on() -> AiConfig {
        AiConfig {
            enabled: true,
            model: String::new(),
        }
    }

    /// 没开、地址空、两处模型都空 —— 三种「这一格不存在」都回 `None`。
    /// 这条钉的是「不许替读者编一个默认端点」：编了就等于悄悄把书本文发出去。
    #[test]
    fn unconfigured_never_invents_an_endpoint() {
        let off = AiConfig {
            enabled: false,
            model: String::new(),
        };
        assert!(configured(&translation("https://x", "m"), &off).is_none());
        assert!(configured(&translation("", "m"), &on()).is_none());
        assert!(
            configured(&translation("https://x", ""), &on()).is_none(),
            "翻译那档没填模型、AI 这档也没填 ⇒ 不知道该问哪只模型"
        );
        assert!(configured(&translation("https://x", "m"), &on()).is_some());
    }

    /// 出厂那份地址 + 没填 key =「读者从没打开过这一格」⇒ 一个字节都不发。
    ///
    /// 这条不是形式测试：官方地址与模型在 `config::OpenAi` 的 `Default` 里是现成填好的，
    /// 所以「把开关一拨就能用」看着理所当然，而按下那一格会把选中的正文 POST 到一个他
    /// 没配过的服务上，只换一个 401 —— 还是拿他的书本文去换。
    #[test]
    fn the_shipped_endpoint_without_a_key_is_not_a_configuration() {
        let shipped_base = OpenAi::default().base_url;
        let shipped_model = OpenAi::default().model;
        assert!(
            configured(&arm(&shipped_base, &shipped_model, ""), &on()).is_none(),
            "地址还是出厂那份、key 空着 ⇒ 这一格不该出现"
        );
        assert!(
            configured(&arm(&shipped_base, &shipped_model, "sk-1"), &on()).is_some(),
            "贴上 key 就是一条真配置"
        );
        // 本地 Ollama / LM Studio 不认 Authorization：地址是读者自己填的，空 key 照常。
        assert!(
            configured(&arm("http://localhost:11434/v1", "qwen2.5:7b", ""), &on()).is_some(),
            "本地服务不填 key 也要能用"
        );
    }

    #[test]
    fn the_ai_model_overrides_only_the_model() {
        let shared = translation("https://x", "translate-model");
        let same = configured(&shared, &on()).expect("该建得出");
        let other = configured(
            &shared,
            &AiConfig {
                enabled: true,
                model: "big-model".to_string(),
            },
        )
        .expect("该建得出");
        assert!(same.cache_key().contains("translate-model"));
        assert!(other.cache_key().contains("big-model"));
        assert_ne!(same.cache_key(), other.cache_key(), "换模型必须换缓存键");
    }

    /// 请求形状：地址补 `/chat/completions`、两条 message、Bearer 头。
    #[test]
    fn the_request_carries_system_then_user() {
        let http = Capture::new(
            r#"{"choices":[{"message":{"content":"好"}}],"usage":{"prompt_tokens":7,"completion_tokens":1}}"#,
        );
        let provider = ChatCompletion::new("https://api.example/v1", "m", "secret");
        let answer = provider.ask(&http, "SYS", "USER").expect("这一趟该成");
        let req = http.last();
        assert_eq!(req.url, "https://api.example/v1/chat/completions");
        assert_eq!(
            req.headers
                .iter()
                .find(|(name, _)| name == "Authorization")
                .map(|(_, value)| value.as_str()),
            Some("Bearer secret")
        );
        let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "SYS");
        assert_eq!(body["messages"][1]["content"], "USER");
        assert_eq!(body["model"], "m");
        // usage 读进来：它是「这一次花了多少」唯一的来源。
        assert_eq!(answer.prompt_tokens, Some(7));
        assert_eq!(answer.completion_tokens, Some(1));
    }

    /// 空 key 不带 Authorization：本地 Ollama / LM Studio 那类服务不认这个头。
    #[test]
    fn an_empty_key_sends_no_authorization_header() {
        let http = Capture::new(r#"{"choices":[{"message":{"content":"x"}}]}"#);
        ChatCompletion::new("https://localhost:11434/v1", "m", "")
            .ask(&http, "s", "u")
            .expect("本地服务没有 key 也要能问");
        assert!(
            http.last()
                .headers
                .iter()
                .all(|(name, _)| name != "Authorization")
        );
    }

    /// `content` 两种形状都要吃得下（字符串 / 段表），其余一律「读不懂」而不是 panic。
    #[test]
    fn the_answer_is_read_from_both_content_shapes() {
        let pieces = Response::new(
            200,
            r#"{"choices":[{"message":{"content":[{"type":"text","text":"一"},{"type":"text","text":"二"}]}}]}"#
                .as_bytes()
                .to_vec(),
        );
        assert_eq!(parse_response(&pieces).unwrap().text, "一二");
        let odd = Response::new(200, br#"{"choices":[{"message":{"content":5}}]}"#.to_vec());
        assert!(matches!(parse_response(&odd), Err(AiError::Malformed(_))));
        let empty = Response::new(200, b"{}".to_vec());
        assert!(matches!(parse_response(&empty), Err(AiError::Malformed(_))));
    }

    /// 答案与输入各有一道长度闸：长过上限收掉并留一个省略号，**不许按字节切**。
    #[test]
    fn long_text_is_clipped_by_character_not_by_byte() {
        let long = "啊".repeat(MAX_ANSWER_CHARS + 10);
        let clipped = super::clip(&long, MAX_ANSWER_CHARS);
        assert_eq!(clipped.chars().count(), MAX_ANSWER_CHARS + 1);
        assert!(clipped.ends_with('…'));
        assert!(
            std::str::from_utf8(clipped.as_bytes()).is_ok(),
            "切出来还得是合法 UTF-8"
        );
    }

    /// 提示词里要标得出「哪一段是读者挑中的」：不标，模型会把前文也当成被问的东西。
    /// 出处那两格（书名与章名）也要在：无头引文只能靠模型猜。
    #[test]
    fn the_selection_prompt_marks_what_is_being_asked_about() {
        let prompt = selection_prompt(&Asked {
            book: "猫城记",
            chapter: "第二章",
            selected: "被问的那句",
            before: "它前面",
            after: "它后面",
        });
        assert!(prompt.contains("Passage being asked about:\n<<<\n被问的那句\n>>>"));
        assert!(prompt.contains("just before"));
        assert!(prompt.contains("just after"));
        assert!(
            prompt.contains("猫城记"),
            "书名没进载荷：模型在猜这是哪本书"
        );
        assert!(prompt.contains("第二章"));
        // 没有元数据的书（裸 `.txt` 还没扫出标题）不该摆一串空引号。
        let bare = selection_prompt(&Asked {
            book: "",
            chapter: "",
            selected: "x",
            before: "",
            after: "",
        });
        assert!(
            !bare.contains("From the book"),
            "两个标题都空 ⇒ 那一行整个不摆"
        );
    }

    /// 上下文按**两侧各一份预算**收顶，选区本身不收（它最长也就一页）。
    /// 这条钉的是「这次离开设备的有多少」有上限 —— 预算那个数住在 [`CONTEXT_BUDGET`]，
    /// 界面上取多少也按它来，所以这里只验拼接后的形状，不把那个数抄第二遍。
    ///
    /// 样本的**第 `budget` 个字之后要换一个字母**：一整串同一个字的话，收顶长度改成两倍
    /// 也能从尾巴上滑出一段一样的来匹配（这条断言实测拦不住任何改动，两次都栽在这里）。
    #[test]
    fn the_context_is_clipped_on_each_side_not_pooled() {
        // 前 `budget` 个字是标记，后面接一段别的：收顶收得准，末尾才正好是省略号。
        let sample = |mark: char| -> String {
            let mut text = mark.to_string().repeat(CONTEXT_BUDGET);
            text.push_str(&"x".repeat(CONTEXT_BUDGET * 2));
            text
        };
        let prompt = selection_prompt(&Asked {
            book: "",
            chapter: "",
            selected: "问这句",
            before: &sample('前'),
            after: &sample('后'),
        });
        for mark in ['前', '后'] {
            let clipped = format!("{}\n>>>", mark.to_string().repeat(CONTEXT_BUDGET) + "…");
            assert_eq!(
                prompt.matches(&clipped).count(),
                1,
                "{mark}那一侧该正好收到预算长度再加一个省略号"
            );
        }
        assert!(prompt.contains("问这句\n>>>"), "选区本身不该被收掉");
    }

    /// 草案那一趟的提示词要钉死三件事：只回一个 JSON 对象、模式按单行 `is_match` 测所以要
    /// 锚 `^`、号放捕获组一。漏掉任何一件的坏答案都会**静悄悄**改坏一整本书的分章，
    /// 而章序号是落库的东西。理由那一句要跟着界面语言走。
    #[test]
    fn the_draft_prompt_pins_the_json_and_the_anchoring() {
        let system = draft_system("ja");
        assert!(system.contains("single JSON object"), "没要求只回 JSON");
        assert!(system.contains("is_match") && system.contains("anchor it at `^`"));
        assert!(system.contains("capture group 1"));
        assert!(
            system.contains("never invent"),
            "没挡住样本里看不见的写法：模型会照「常见的章标题」编一条"
        );
        assert!(system.contains("Japanese"), "理由那一句没跟着界面语言走");

        let user = draft_prompt(&["书名：猫城记".to_string()], &["== 01 ==".to_string()]);
        assert!(user.contains("First lines of the file, in order:\n<<<\n书名：猫城记\n>>>"));
        assert!(
            user.contains("spread across the whole book:\n<<<\n== 01 ==\n>>>"),
            "两份样本没标出各自是什么：混在一起模型会以为书里全是标题"
        );
    }

    /// 三档问法都要说「用哪种语言回答」并把「只回答案」写死：弹层是一张卡，
    /// 模型顺带写的前言会把卡撑成一篇文章，而界面语言换到韩语时答案该跟着换。
    #[test]
    fn every_ask_prompts_for_one_language_and_no_preamble() {
        for ask in Ask::ALL {
            let system = ask.system("ko");
            assert!(system.contains("Korean"), "{ask:?} 没说回答语言");
            assert!(
                system.contains("Reply with the answer only"),
                "{ask:?} 没挡住前言"
            );
            assert!(!ask.key().is_empty());
        }
        assert!(Ask::Explain.system("zh-CN").contains("Simplified Chinese"));
    }
}
