//! 百度开放平台的 OAuth 2.0 那一段：授权码换 token、刷新、问一句账号是谁。
//!
//! 端点全在 `openapi.baidu.com`，与 xpan 那台（[`super::baidu`]）分开 —— 这一节管
//! 「凭据怎么来、怎么续」，那一份管「凭据怎么用」。走同一把 [`Transport`]，
//! 全部离线可测（假传输 + fixture）。
//!
//! 流程是**简化授权（oob）**：应用打开授权页（[`authorize_url`]），读者在百度登录并
//! 同意后，页面上直接显示一串授权码（`redirect_uri=oob`，不回跳），他把它粘回应用，
//! [`exchange_code`] 拿它换 token。不需要回调服务器，也不需要注册深链 —— 手机、桌面
//! 走的是同一条路。token 过期后 [`refresh_tokens`] 拿 `refresh_token` 换一对新的
//! （刷新会**轮换** `refresh_token`，旧的那支作废，新的一定要落盘）。
//!
//! 应用凭据（`client_id` / `client_secret`）是开发者侧的事：编译期内置
//! （[`credentials`]，构建时可用 `FOLIO_BAIDU_CLIENT_ID` / `FOLIO_BAIDU_CLIENT_SECRET`
//! 环境变量喂）。没配置时界面上「去授权」那一路收起，读者仍走手贴 token 的老路 ——
//! 这一层不猜、也不装。

use std::sync::Arc;

use super::baidu::{USER_AGENT, query_encode};
use crate::{Error, Method, Request, Response, Result, Transport};

const OAUTH: &str = "https://openapi.baidu.com/oauth/2.0";

/// 开放平台注册的那一对。**发布前填上**；空着时 [`credentials`] 返回 `None`，
/// 界面据此收起授权入口。构建时可用环境变量盖：
/// `FOLIO_BAIDU_CLIENT_ID` / `FOLIO_BAIDU_CLIENT_SECRET`。
pub const CLIENT_ID: &str = "";
pub const CLIENT_SECRET: &str = "";

/// 内置的应用凭据；没配就是 `None`。
pub fn credentials() -> Option<(&'static str, &'static str)> {
    let id = option_env!("FOLIO_BAIDU_CLIENT_ID").unwrap_or(CLIENT_ID);
    let secret = option_env!("FOLIO_BAIDU_CLIENT_SECRET").unwrap_or(CLIENT_SECRET);
    (!id.is_empty() && !secret.is_empty()).then_some((id, secret))
}

/// 授权页地址。`display=mobile` 让手机上出移动版页面；`scope` 只要 basic + netdisk
/// （读目录与下载就在这两格里）。
pub fn authorize_url(client_id: &str) -> String {
    format!(
        "{OAUTH}/authorize?response_type=code&client_id={}&redirect_uri=oob&scope=basic,netdisk&display=mobile",
        query_encode(client_id),
    )
}

/// 一对 token：换出来的与刷新回来的都是这个形状。
///
/// `expires_in` 是官方给的秒数（约 30 天），调用方落盘时自己折成到期时刻 ——
/// 这一节不给「现在几点」留位置，时钟归调用方。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaiduTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
}

impl From<BaiduTokens> for super::auth::Token {
    /// `expires_in` 折成绝对时刻（Unix 秒）—— [`super::auth::Token`] 的口径。
    fn from(tokens: BaiduTokens) -> Self {
        Self {
            access: tokens.access_token,
            refresh: tokens.refresh_token,
            expires_at: folio_base::time::now() + tokens.expires_in.max(0),
        }
    }
}

/// 百度那一支的 [`super::auth::Refresher`]：包住 [`refresh_tokens`]。
///
/// 轮换出来的新 refresh_token 原样进 [`super::auth::Token`] —— 落盘是
/// `TokenStore` 的事，这里只管换。
pub struct BaiduRefresher;

impl super::auth::Refresher for BaiduRefresher {
    fn refresh(
        &self,
        refresh_token: &str,
        transport: Arc<dyn Transport>,
    ) -> Result<super::auth::Token> {
        refresh_tokens(refresh_token, transport).map(Into::into)
    }
}

/// 授权码换 token。`code` 是读者从授权页粘回来的那一串。
pub fn exchange_code(code: &str, transport: Arc<dyn Transport>) -> Result<BaiduTokens> {
    let (client_id, client_secret) = credentials()
        .ok_or_else(|| Error::Malformed("百度应用凭据没有内置，走不了授权那一路".into()))?;
    exchange_code_with(client_id, client_secret, code, transport)
}

/// [`exchange_code`] 的可测芯：凭据显式递进来。
fn exchange_code_with(
    client_id: &str,
    client_secret: &str,
    code: &str,
    transport: Arc<dyn Transport>,
) -> Result<BaiduTokens> {
    token_request(
        &format!(
            "{OAUTH}/token?grant_type=authorization_code&code={}&client_id={}&client_secret={}&redirect_uri=oob",
            query_encode(code),
            query_encode(client_id),
            query_encode(client_secret),
        ),
        transport,
    )
}

/// 刷新 token。成功时 `refresh_token` **可能轮换** —— 返回值里的那一支就是今后唯一
/// 有效的，调用方必须落盘。
pub fn refresh_tokens(refresh_token: &str, transport: Arc<dyn Transport>) -> Result<BaiduTokens> {
    let (client_id, client_secret) = credentials()
        .ok_or_else(|| Error::Malformed("百度应用凭据没有内置，走不了授权那一路".into()))?;
    refresh_tokens_with(client_id, client_secret, refresh_token, transport)
}

/// [`refresh_tokens`] 的可测芯。
fn refresh_tokens_with(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    transport: Arc<dyn Transport>,
) -> Result<BaiduTokens> {
    token_request(
        &format!(
            "{OAUTH}/token?grant_type=refresh_token&refresh_token={}&client_id={}&client_secret={}",
            query_encode(refresh_token),
            query_encode(client_id),
            query_encode(client_secret),
        ),
        transport,
    )
}

/// 授权/刷新共用的那一步：GET token 端点、解 OAuth 形状的应答。
///
/// 失败时端点回的是标准 OAuth 错误体（`{"error": "invalid_grant", …}`，**常伴 HTTP
/// 400**）：先看体，有 `error` 就归「认证被拒」；解不出体（HTML 错误页之类）再按
/// 状态归档 —— 界面上都是「重新授权」这一个动作。
fn token_request(url: &str, transport: Arc<dyn Transport>) -> Result<BaiduTokens> {
    let req = Request::new(Method::Get, url.to_string());
    tracing::debug!(
        "baidu_auth: {}",
        req.url.split('?').next().unwrap_or(&req.url)
    );
    let resp = transport.request(req)?;
    if !(200..300).contains(&resp.status) {
        if let Ok(parsed) = serde_json::from_slice::<TokenResp>(&resp.body) {
            parsed.into_result()?;
        }
        check_oauth(&resp)?;
        unreachable!("check_oauth 对非 2xx 必给一个错");
    }
    let parsed: TokenResp = parse_json(&resp.body)?;
    let parsed = parsed.into_result()?;
    if parsed.access_token.is_empty() {
        return Err(Error::Malformed("token 应答里没有 access_token".into()));
    }
    Ok(BaiduTokens {
        access_token: parsed.access_token,
        refresh_token: parsed.refresh_token.unwrap_or_default(),
        expires_in: parsed.expires_in.unwrap_or(0),
    })
}

/// 这台网盘是谁的：uinfo 的 `netdisk_name`，没有就退 `baidu_id`。
///
/// 换 token 成功后问一次，账号列表上给读者看「这一格是谁」；不是每次请求都问。
pub fn account_name(access_token: &str, transport: Arc<dyn Transport>) -> Result<String> {
    // token 的拼法走 `baidu::with_token`：日志脱敏只认那一种形状（`access_token=`），
    // 这里自己拼一遍就多一处遮不住的地方。
    let url = super::baidu::with_token(
        "https://pan.baidu.com/rest/2.0/xpan/nas?method=uinfo".to_string(),
        access_token,
    );
    let req = Request::new(Method::Get, url).header("User-Agent", USER_AGENT);
    let resp = transport.request(req)?;
    check_oauth(&resp)?;
    let parsed: Uinfo = parse_json(&resp.body)?;
    let name = if parsed.netdisk_name.trim().is_empty() {
        parsed.baidu_id
    } else {
        parsed.netdisk_name
    };
    if name.trim().is_empty() {
        return Err(Error::Malformed("uinfo 没回账号名".into()));
    }
    Ok(name.trim().to_string())
}

/// OAuth 端点的状态挡板：与 xpan 那边的 [`super::baidu::check`] 同一口，401 归认证。
fn check_oauth(resp: &Response) -> Result<()> {
    if (200..300).contains(&resp.status) {
        return Ok(());
    }
    match resp.status {
        401 => Err(Error::Unauthorized { status: 401 }),
        403 => Err(Error::Forbidden { status: 403 }),
        other => Err(Error::Unexpected { status: other }),
    }
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
    /// OAuth 错误体长这样：`{"error": "invalid_grant", "error_description": "…"}`
    /// （常伴 HTTP 400，但端点偶尔也给 200 + 错误体，所以状态与体两处都挡）。
    #[serde(default)]
    error: Option<String>,
}

impl TokenResp {
    /// 有 `error` 就不当成成功应答解。
    fn into_result(self) -> Result<Self> {
        match self.error.as_deref() {
            Some(err) if !err.trim().is_empty() => Err(Error::Unauthorized { status: 401 }),
            _ => Ok(self),
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct Uinfo {
    #[serde(default)]
    baidu_id: String,
    #[serde(default)]
    netdisk_name: String,
}

#[cfg(test)]
mod tests {
    //! 假传输 + fixture：授权 URL 的形状、换 token 的请求与应答、错误体分档、
    //! 刷新轮换、账号名回退。凭据走 `_with` 那条芯显式递，不依赖构建环境。

    use std::sync::Arc;

    use super::*;
    use crate::dav::Mock;

    const TOKEN_JSON: &str =
        r#"{"access_token":"121.ab","refresh_token":"122.cd","expires_in":2592000}"#;
    const ROTATED_JSON: &str =
        r#"{"access_token":"121.ef","refresh_token":"123.gh","expires_in":2592000}"#;
    const OAUTH_ERROR_JSON: &str =
        r#"{"error":"invalid_grant","error_description":"expired authorization code"}"#;
    const UINFO_JSON: &str =
        r#"{"baidu_id":"id-9000","netdisk_name":"山间的读客","avatar_url":"https://a/b.png"}"#;

    fn ok(body: &str) -> Response {
        Response::new(200, body.as_bytes().to_vec())
    }

    fn transport(responses: Vec<Response>) -> Arc<Mock> {
        Arc::new(Mock::new(responses))
    }

    /// 授权页带上 client_id 与 oob，display 是移动版。
    #[test]
    fn the_authorize_url_carries_the_client_and_oob() {
        let url = authorize_url("fAk3ApIK3y");
        assert!(url.starts_with("https://openapi.baidu.com/oauth/2.0/authorize?"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("client_id=fAk3ApIK3y"));
        assert!(url.contains("redirect_uri=oob"));
        assert!(url.contains("scope=basic,netdisk"));
        assert!(url.contains("display=mobile"));
    }

    /// 换 token：grant_type / code / client_id / redirect_uri 都在请求上，
    /// 应答解出三个数。
    #[test]
    fn exchanging_a_code_sends_the_grant_and_parses_the_pair() {
        let t = transport(vec![ok(TOKEN_JSON)]);
        let tokens = exchange_code_with(
            "fAk3ApIK3y",
            "s3cr3t",
            "oob-code",
            t.clone() as Arc<dyn Transport>,
        )
        .expect("换到");
        assert_eq!(
            tokens,
            BaiduTokens {
                access_token: "121.ab".into(),
                refresh_token: "122.cd".into(),
                expires_in: 2_592_000,
            }
        );
        let url = &t.last_request().url;
        assert!(url.contains("grant_type=authorization_code"), "{url}");
        assert!(url.contains("code=oob-code"), "{url}");
        assert!(url.contains("client_id=fAk3ApIK3y"), "{url}");
        assert!(url.contains("client_secret=s3cr3t"), "{url}");
        assert!(url.contains("redirect_uri=oob"), "{url}");
    }

    /// 刷新：grant_type 是 refresh_token；**轮换**回的新 refresh_token 原样交出去。
    #[test]
    fn refreshing_rotates_the_refresh_token() {
        let t = transport(vec![ok(ROTATED_JSON)]);
        let tokens = refresh_tokens_with(
            "fAk3ApIK3y",
            "s3cr3t",
            "122.cd",
            t.clone() as Arc<dyn Transport>,
        )
        .expect("刷新到");
        assert_eq!(tokens.access_token, "121.ef");
        assert_eq!(
            tokens.refresh_token, "123.gh",
            "轮换出来的那一支要交回去落盘"
        );
        let url = &t.last_request().url;
        assert!(url.contains("grant_type=refresh_token"), "{url}");
        assert!(url.contains("refresh_token=122.cd"), "{url}");
    }

    /// OAuth 错误体（含 HTTP 400 与 200 + 错误体两种来路）归「认证被拒」，不是 Malformed。
    #[test]
    fn an_oauth_error_body_is_an_auth_failure() {
        let t = transport(vec![
            Response::new(400, OAUTH_ERROR_JSON.as_bytes().to_vec()),
            ok(OAUTH_ERROR_JSON),
        ]);
        assert!(matches!(
            exchange_code_with("id", "secret", "bad", t.clone() as Arc<dyn Transport>),
            Err(Error::Unauthorized { status: 401 })
        ));
        assert!(matches!(
            exchange_code_with("id", "secret", "bad", t as Arc<dyn Transport>),
            Err(Error::Unauthorized { status: 401 })
        ));
    }

    /// HTTP 401 也归认证被拒 —— 状态挡板与 xpan 那边同一口。
    #[test]
    fn an_http_401_is_an_auth_failure() {
        let t = transport(vec![Response::new(401, Vec::new())]);
        assert!(matches!(
            refresh_tokens_with("id", "secret", "122.cd", t as Arc<dyn Transport>),
            Err(Error::Unauthorized { status: 401 })
        ));
    }

    /// 账号名：netdisk_name 优先，空了退 baidu_id；uinfo 带上 token 与那个 UA。
    #[test]
    fn the_account_name_prefers_the_netdisk_name() {
        let t = transport(vec![ok(UINFO_JSON)]);
        let name = account_name("121.ab", t.clone() as Arc<dyn Transport>).expect("拿到账号名");
        assert_eq!(name, "山间的读客");
        let req = t.last_request();
        assert!(req.url.contains("method=uinfo"), "{}", req.url);
        assert!(req.url.contains("access_token=121.ab"), "{}", req.url);
        assert!(req.headers.iter().any(|(k, _)| k == "User-Agent"));

        let t = transport(vec![ok(r#"{"baidu_id":"id-9000","netdisk_name":""}"#)]);
        assert_eq!(
            account_name("121.a", t as Arc<dyn Transport>).unwrap(),
            "id-9000",
            "netdisk_name 空了退 baidu_id"
        );
    }

    /// 应答缺 access_token 是 Malformed，不是一只空 token 静默落盘。
    #[test]
    fn a_body_without_a_token_is_malformed() {
        let t = transport(vec![ok(r#"{"expires_in":2592000}"#)]);
        assert!(matches!(
            refresh_tokens_with("id", "secret", "122.cd", t as Arc<dyn Transport>),
            Err(Error::Malformed(_))
        ));
    }
}
