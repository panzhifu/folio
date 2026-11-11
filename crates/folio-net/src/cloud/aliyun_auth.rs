//! 阿里云盘（Alipan）的 OAuth 2.0：授权码换 token、刷新、问云盘编号与账号名。
//!
//! 端点全在 `open.aliyundrive.com`；与 xpan / 百度那一套分开 —— 这一节管「凭据
//! 怎么来、怎么续」，[`super::aliyun`] 那份管「凭据怎么用」。走同一把
//! [`Transport`]，全部离线可测（假传输 + fixture JSON）。
//!
//! 与百度的 [`super::baidu_auth`] 有几处**实质不同**，照抄会错：
//!
//! - 授权页在 `openapi.aliyundrive.com/oauth/authorize`，而取 token 在
//!   `open.aliyundrive.com/oauth/access_token`（两个主机名）；
//! - 取 token 是 **POST + JSON 体**，不是百度那种 GET + query；
//! - 应答里带 `drive_id` —— 阿里云盘的每个文件请求都要它，所以换 token 那一步
//!   就把它一起落盘（少这一格，后面所有请求都是 `InvalidParameter`）；
//! - `redirect_uri` **必须**与开放平台里登记的那一个逐字相同，不能像百度那样
//!   填 `oob`（阿里云盘不支持 oob）。所以这一格是可配的，默认指应用自己的深链。
//!
//! 应用凭据（`client_id` / `client_secret`）是开发者侧的事：编译期内置
//! （[`credentials`]，构建时可用 `FOLIO_ALIYUN_CLIENT_ID` / `FOLIO_ALIYUN_CLIENT_SECRET`
//! 环境变量喂）。没配置时界面上「去授权」那一路收起，读者仍走手贴 token 的老路 ——
//! 这一层不猜、也不装。

use std::sync::Arc;

use super::baidu::query_encode;
use crate::{Error, Method, Request, Result, Transport};

const API: &str = "https://open.aliyundrive.com";
const AUTHORIZE: &str = "https://openapi.aliyundrive.com/oauth/authorize";

/// 开放平台注册的那一对。**发布前填上**；空着时 [`credentials`] 返回 `None`，
/// 界面据此收起授权入口。构建时可用环境变量盖：
/// `FOLIO_ALIYUN_CLIENT_ID` / `FOLIO_ALIYUN_CLIENT_SECRET`。
pub const CLIENT_ID: &str = "";
pub const CLIENT_SECRET: &str = "";

/// 授权后回跳的那个地址。必须与开放平台登记的逐字相同。
///
/// 默认是应用自己的深链：`folio://aliyun/callback`。桌面与 Android 都由外壳接这个
/// scheme（Android 那边是 `intent-filter`），拿到 `code` 之后交给 [`exchange_code`]。
/// 换一个地址只需要改这一处 —— 它与开放平台后台那一格是一对，不该分散在两个地方。
pub const REDIRECT_URI: &str = "folio://aliyun/callback";

/// 要的权限：读全部文件、写全部文件、读账号基本信息。
///
/// `file:all:write` 是同步与传书要的（快照要落盘）；只取书的话读就够，但一份应用
/// 只有一个 scope 组合，分开申请会让读者在「要不要同步」之间做一次选择 —— 不值。
const SCOPE: &str = "user:base,file:all:read,file:all:write";

/// 内置的应用凭据；没配就是 `None`。
pub fn credentials() -> Option<(&'static str, &'static str)> {
    let id = option_env!("FOLIO_ALIYUN_CLIENT_ID").unwrap_or(CLIENT_ID);
    let secret = option_env!("FOLIO_ALIYUN_CLIENT_SECRET").unwrap_or(CLIENT_SECRET);
    (!id.is_empty() && !secret.is_empty()).then_some((id, secret))
}

/// 授权页地址。与百度那条相比多两件事：`redirect_uri` 是要登记的那一个（不是 `oob`），
/// `scope` 用逗号分隔。
pub fn authorize_url(client_id: &str) -> String {
    format!(
        "{AUTHORIZE}?client_id={}&redirect_uri={}&response_type=code&scope={}",
        query_encode(client_id),
        query_encode(REDIRECT_URI),
        query_encode(SCOPE),
    )
}

/// 一对 token：换出来的与刷新回来的是这个形状。
///
/// `drive_id` 只在**授权**那一次的应答里有（刷新时也可能带，但别依赖它：阿里云盘
/// 的刷新应答里这一格是可选的）。调用方在换 token 时把它存下来，之后每次请求都用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliyunTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
    /// 云盘编号；刷新那一趟可能给空串（沿用已存的那一份）。
    pub drive_id: String,
}

impl From<AliyunTokens> for super::auth::Token {
    /// `expires_in` 折成绝对时刻；`drive_id` **不进** [`super::auth::Token`] ——
    /// 刷新应答里它可能缺席，沿用 config 里已存的那一份是调用方（`cloud::view`）的事。
    fn from(tokens: AliyunTokens) -> Self {
        Self {
            access: tokens.access_token,
            refresh: tokens.refresh_token,
            expires_at: folio_base::time::now() + tokens.expires_in.max(0),
        }
    }
}

/// 阿里云盘那一支的 [`super::auth::Refresher`]：包住 [`refresh_tokens`]。
pub struct AliyunRefresher;

impl super::auth::Refresher for AliyunRefresher {
    fn refresh(
        &self,
        refresh_token: &str,
        transport: Arc<dyn Transport>,
    ) -> Result<super::auth::Token> {
        refresh_tokens(refresh_token, transport).map(Into::into)
    }
}

/// 授权码换 token。
pub fn exchange_code(code: &str, transport: Arc<dyn Transport>) -> Result<AliyunTokens> {
    let (client_id, client_secret) = credentials()
        .ok_or_else(|| Error::Malformed("阿里云盘应用凭据没有内置，走不了授权那一路".into()))?;
    exchange_code_with(client_id, client_secret, code, transport)
}

/// [`exchange_code`] 的可测芯：凭据显式递进来。
fn exchange_code_with(
    client_id: &str,
    client_secret: &str,
    code: &str,
    transport: Arc<dyn Transport>,
) -> Result<AliyunTokens> {
    token_request(
        &format!(
            r#"{{"client_id":"{client_id}","client_secret":"{client_secret}","grant_type":"authorization_code","code":"{code}","redirect_uri":"{REDIRECT_URI}"}}"#,
            client_id = json_escape(client_id),
            client_secret = json_escape(client_secret),
            code = json_escape(code),
        ),
        transport,
    )
}

/// 刷新 token。应答里的 `refresh_token` 可能是新的（阿里云盘会轮换），必须落盘。
pub fn refresh_tokens(refresh_token: &str, transport: Arc<dyn Transport>) -> Result<AliyunTokens> {
    let (client_id, client_secret) = credentials()
        .ok_or_else(|| Error::Malformed("阿里云盘应用凭据没有内置，走不了授权那一路".into()))?;
    refresh_tokens_with(client_id, client_secret, refresh_token, transport)
}

/// [`refresh_tokens`] 的可测芯。
fn refresh_tokens_with(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    transport: Arc<dyn Transport>,
) -> Result<AliyunTokens> {
    token_request(
        &format!(
            r#"{{"client_id":"{client_id}","client_secret":"{client_secret}","grant_type":"refresh_token","refresh_token":"{refresh}"}}"#,
            client_id = json_escape(client_id),
            client_secret = json_escape(client_secret),
            refresh = json_escape(refresh_token),
        ),
        transport,
    )
}

/// JSON 字符串里的转义。凭据是开发者填的、code 是机器签发的，正常都不含引号，
/// 但拼字符串时留一道口子比「一定不会」可靠 —— 一次坏输入就是一整条链路失败。
fn json_escape(raw: &str) -> String {
    raw.replace('\\', r"\\").replace('"', "\\\"")
}

/// 授权 / 刷新共用的那一步：POST JSON、解应答。
///
/// 阿里云盘的错误也是 JSON（`{"code": "AccessTokenInvalid", "message": "…"}`），
/// 而且**常伴 HTTP 4xx**。先看 `code`：与认证相关的那几个归「认证被拒」，
/// 界面上都是「重新授权」这一个动作；其余归 `Malformed` 带上原话，至少能查。
fn token_request(body: &str, transport: Arc<dyn Transport>) -> Result<AliyunTokens> {
    let req = Request::new(Method::Post, format!("{API}/oauth/access_token"))
        .header("Content-Type", "application/json")
        .body(body.as_bytes().to_vec());
    let resp = transport.request(req)?;
    let parsed: TokenResp = parse_json(&resp.body)?;
    if let Some(code) = parsed.code.as_deref()
        && !code.trim().is_empty()
    {
        return Err(auth_error_of(code, parsed.message.as_deref()));
    }
    if !(200..300).contains(&resp.status) {
        return Err(auth_error_of(&format!("http_{}", resp.status), None));
    }
    if parsed.access_token.is_empty() {
        return Err(Error::Malformed("token 应答里没有 access_token".into()));
    }
    Ok(AliyunTokens {
        access_token: parsed.access_token,
        refresh_token: parsed.refresh_token.unwrap_or_default(),
        expires_in: parsed.expires_in.unwrap_or(0),
        drive_id: parsed.drive_id.unwrap_or_default(),
    })
}

/// 错误码 → 我们那一套。认证相关的那几个（token 过期 / 无效 / 被撤销）都归
/// [`Error::Unauthorized`]，界面上的动作只有一个：重新授权。
fn auth_error_of(code: &str, message: Option<&str>) -> Error {
    match code {
        "AccessTokenInvalid"
        | "AccessTokenExpired"
        | "InvalidParameter.RefreshToken"
        | "I400JD"
        | "TooManyRequests" => Error::Unauthorized { status: 401 },
        other => Error::Malformed(match message {
            Some(message) => format!("阿里云盘 {other}: {message}"),
            None => format!("阿里云盘 {other}"),
        }),
    }
}

/// 这台云盘是谁的：昵称优先，没有就退用户名 / 手机号。
///
/// `getDriveInfo` 同时回 `default_drive_id` —— [`drive_info`] 一次问两样。
pub fn account_name(access_token: &str, transport: Arc<dyn Transport>) -> Result<String> {
    let info = drive_info(access_token, transport)?;
    let name = if info.nick_name.trim().is_empty() {
        if info.user_name.trim().is_empty() {
            info.phone
        } else {
            info.user_name
        }
    } else {
        info.nick_name
    };
    if name.trim().is_empty() {
        return Err(Error::Malformed("getDriveInfo 没回账号名".into()));
    }
    Ok(name.trim().to_string())
}

/// 云盘编号与主人的名字一起要回来（一次请求）。
///
/// 换 token 那一步就把 `drive_id` 存下来是主路；这一问是**兜底** —— 老配置里
/// 只有 token 没有 drive_id 的，补这一趟就能继续用，不必请读者重新授权一遍。
pub fn drive_info(access_token: &str, transport: Arc<dyn Transport>) -> Result<DriveInfo> {
    let req = Request::new(Method::Post, format!("{API}/adrive/v1.0/user/getDriveInfo"))
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Content-Type", "application/json")
        .body(b"{}".to_vec());
    let resp = transport.request(req)?;
    let parsed: DriveInfoResp = parse_json(&resp.body)?;
    if let Some(code) = parsed.code.as_deref()
        && !code.trim().is_empty()
    {
        return Err(auth_error_of(code, parsed.message.as_deref()));
    }
    if !(200..300).contains(&resp.status) {
        return Err(auth_error_of(&format!("http_{}", resp.status), None));
    }
    if parsed.default_drive_id.trim().is_empty() {
        return Err(Error::Malformed(
            "getDriveInfo 没回 default_drive_id".into(),
        ));
    }
    Ok(DriveInfo {
        drive_id: parsed.default_drive_id,
        nick_name: parsed.nick_name,
        user_name: parsed.user_name,
        phone: parsed.phone,
    })
}

/// [`drive_info`] 的产物。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveInfo {
    pub drive_id: String,
    pub nick_name: String,
    pub user_name: String,
    pub phone: String,
}

fn parse_json<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T> {
    serde_json::from_slice(body).map_err(|err| Error::Malformed(err.to_string()))
}

#[derive(Debug, serde::Deserialize)]
struct TokenResp {
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    drive_id: Option<String>,
    /// 错误体：`{"code": "…", "message": "…"}`
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct DriveInfoResp {
    #[serde(default)]
    default_drive_id: String,
    #[serde(default)]
    nick_name: String,
    #[serde(default)]
    user_name: String,
    #[serde(default)]
    phone: String,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[cfg(test)]
mod tests {
    //! 假传输 + fixture：授权 URL 的形状、换 token 的 JSON 体、刷新轮换、错误码分档、
    //! drive_id 与账号名。凭据走 `_with` 那条芯显式递，不依赖构建环境。

    use std::sync::Arc;

    use super::*;
    use crate::Response;
    use crate::dav::Mock;

    const TOKEN_JSON: &str = r#"{"token_type":"Bearer","access_token":"ali.at","refresh_token":"ali.rt","expires_in":7200,"drive_id":"88"}"#;
    const ROTATED_JSON: &str = r#"{"token_type":"Bearer","access_token":"ali.at2","refresh_token":"ali.rt2","expires_in":7200}"#;
    const EXPIRED_JSON: &str = r#"{"code":"AccessTokenExpired","message":"token expired"}"#;
    const DRIVE_INFO_JSON: &str = r#"{"default_drive_id":"88","nick_name":"山间的读客","user_name":"reader","phone":"13800000000"}"#;

    fn ok(body: &str) -> Response {
        Response::new(200, body.as_bytes().to_vec())
    }

    fn transport(responses: Vec<Response>) -> Arc<Mock> {
        Arc::new(Mock::new(responses))
    }

    /// 授权页：client_id、登记的那个 redirect_uri、三个 scope 都在，且**不是** `oob`。
    #[test]
    fn the_authorize_url_carries_the_registered_redirect() {
        let url = authorize_url("ak");
        assert!(
            url.starts_with("https://openapi.aliyundrive.com/oauth/authorize?"),
            "{url}"
        );
        assert!(url.contains("client_id=ak"), "{url}");
        assert!(url.contains("response_type=code"), "{url}");
        assert!(
            url.contains("redirect_uri=folio%3A%2F%2Faliyun%2Fcallback"),
            "阿里云盘不认 oob，必须是登记过的地址：{url}"
        );
        assert!(!url.contains("oob"), "{url}");
        assert!(url.contains("file%3Aall%3Aread"), "{url}");
    }

    /// 换 token：POST + JSON 体，应答解出四格（含 drive_id）。
    #[test]
    fn exchanging_a_code_posts_json_and_keeps_the_drive_id() {
        let t = transport(vec![ok(TOKEN_JSON)]);
        let tokens = exchange_code_with("ak", "sk", "the-code", t.clone() as Arc<dyn Transport>)
            .expect("换到");
        assert_eq!(tokens.access_token, "ali.at");
        assert_eq!(tokens.refresh_token, "ali.rt");
        assert_eq!(tokens.expires_in, 7200);
        assert_eq!(tokens.drive_id, "88", "drive_id 要跟着 token 一起落盘");

        let req = t.last_request();
        assert_eq!(req.method, Method::Post);
        assert!(req.url.ends_with("/oauth/access_token"), "{}", req.url);
        let body = String::from_utf8_lossy(&req.body);
        assert!(
            body.contains(r#""grant_type":"authorization_code""#),
            "{body}"
        );
        assert!(body.contains(r#""code":"the-code""#), "{body}");
        assert!(
            req.headers
                .iter()
                .any(|(k, v)| k == "Content-Type" && v.contains("json")),
            "取 token 是 POST JSON，不是 GET query"
        );
    }

    /// 刷新：grant_type 是 refresh_token；轮换出来的新 refresh_token 原样交出去。
    #[test]
    fn refreshing_rotates_the_refresh_token() {
        let t = transport(vec![ok(ROTATED_JSON)]);
        let tokens = refresh_tokens_with("ak", "sk", "ali.rt", t.clone() as Arc<dyn Transport>)
            .expect("刷新到");
        assert_eq!(tokens.access_token, "ali.at2");
        assert_eq!(
            tokens.refresh_token, "ali.rt2",
            "轮换出来的那一支要交回去落盘"
        );
        assert_eq!(tokens.drive_id, "", "刷新应答可能不带 drive_id，沿用已存的");
        let req = t.last_request();
        let body = String::from_utf8_lossy(&req.body).to_string();
        assert!(body.contains(r#""grant_type":"refresh_token""#), "{body}");
        assert!(body.contains(r#""refresh_token":"ali.rt""#), "{body}");
    }

    /// 错误体分档：token 过期归认证被拒，别的一律带原话归 Malformed。
    #[test]
    fn an_expired_token_is_an_auth_failure_but_others_keep_their_message() {
        let t = transport(vec![Response::new(400, EXPIRED_JSON.as_bytes().to_vec())]);
        assert!(matches!(
            exchange_code_with("ak", "sk", "bad", t as Arc<dyn Transport>),
            Err(Error::Unauthorized { status: 401 })
        ));

        let t = transport(vec![ok(
            r#"{"code":"InvalidParameter","message":"drive_id 缺失"}"#,
        )]);
        let err = refresh_tokens_with("ak", "sk", "x", t as Arc<dyn Transport>).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("InvalidParameter"), "{message}");
        assert!(message.contains("drive_id 缺失"), "原话要留住：{message}");
    }

    /// 账号名与云盘编号一次问回来；昵称优先，空则退用户名再退手机号。
    #[test]
    fn the_drive_info_names_the_account_and_its_drive() {
        let t = transport(vec![ok(DRIVE_INFO_JSON)]);
        let info = drive_info("ali.at", t.clone() as Arc<dyn Transport>).expect("拿到");
        assert_eq!(info.drive_id, "88");
        assert_eq!(info.nick_name, "山间的读客");
        let req = t.last_request();
        assert!(
            req.url.ends_with("/adrive/v1.0/user/getDriveInfo"),
            "{}",
            req.url
        );
        assert!(
            req.headers
                .iter()
                .any(|(k, v)| k == "Authorization" && v == "Bearer ali.at"),
            "要带 Bearer"
        );

        let t = transport(vec![ok(
            r#"{"default_drive_id":"88","nick_name":"","user_name":"reader","phone":"138"}"#,
        )]);
        assert_eq!(
            account_name("ali.at", t as Arc<dyn Transport>).unwrap(),
            "reader",
            "昵称空了退用户名"
        );

        let t = transport(vec![ok(
            r#"{"default_drive_id":"88","nick_name":"","user_name":"","phone":"138"}"#,
        )]);
        assert_eq!(
            account_name("ali.at", t as Arc<dyn Transport>).unwrap(),
            "138",
            "用户名也空了退手机号"
        );
    }

    /// 应答缺 access_token 是 Malformed，不是一只空 token 静默落盘。
    #[test]
    fn a_body_without_a_token_is_malformed() {
        let t = transport(vec![ok(r#"{"expires_in":7200}"#)]);
        assert!(matches!(
            refresh_tokens_with("ak", "sk", "x", t as Arc<dyn Transport>),
            Err(Error::Malformed(_))
        ));
    }

    /// 拼 JSON 时引号与反斜杠被转义：凭据里偶尔有一个引号，不该把整份请求弄坏。
    #[test]
    fn the_json_body_escapes_quotes() {
        let t = transport(vec![ok(TOKEN_JSON)]);
        let _ = exchange_code_with("a\"b", "c\\d", "e\"f", t.clone() as Arc<dyn Transport>);
        let req = t.last_request();
        let body = String::from_utf8_lossy(&req.body).to_string();
        assert!(body.contains(r#""client_id":"a\"b""#), "{body}");
        assert!(body.contains(r#""client_secret":"c\\d""#), "{body}");
    }
}
