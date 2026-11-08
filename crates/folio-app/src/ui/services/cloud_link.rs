//! 网盘 OAuth 的**回跳深链**：授权页回到应用时带来的那个 `code`。
//!
//! 两家的授权形态不同，这一层只认阿里云盘那一条：
//!
//! - 百度是简化授权（`redirect_uri=oob`），授权码直接显示在页面上，读者手抄 —— 不需要
//!   这一层；
//! - 阿里云盘**必须**回跳到登记过的地址，所以授权页关掉之后会落到
//!   `folio://aliyun/callback?code=…`。Android 侧那条 `intent-filter` 已经注册了
//!   `folio` scheme（见 `AndroidManifest.xml`），平台层把它转成一次
//!   [`gpui_mobile::packages::deeplink`] 回调，这里再接一手收下。
//!
//! 为什么只排队、不直接改界面：回跳发生在 **JNI 线程**上，手上没有 `App`、更没有窗口；
//! 而把 `code` 写进输入框（`InputState::set_value`）需要窗口。所以这里只做「收下、
//! 排队」，由网盘那一屏在它自己的渲染帧里取用 —— 那一帧同时握着窗口与 `&mut self`
//! （见 `cloud::view` 的 `take_pending_aliyun_code`）。
//!
//! 用一把锁 + 一条队列而不是通道：这一层**没有**消费者线程（取用发生在渲染帧里），
//! 而多一条常驻的接收任务就要处理「页面被换掉之后谁还在收」——那正是会丢链接的地方。
//!
//! 其它平台（桌面 / iOS）没有回跳：桌面要读者自己复制 `code`，iOS 的 universal link
//! 还没接。这一层在那儿只收不到东西，不引入任何新依赖。

use std::sync::{Mutex, OnceLock};

/// 还没被取走的回跳 URL。一次授权只来一条，重复点「打开授权页」最多叠几条。
static PENDING: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

fn slot() -> &'static Mutex<Vec<String>> {
    PENDING.get_or_init(|| Mutex::new(Vec::new()))
}

/// 装上深链回调。**在应用启动、平台起来之后调一次**（见 `platform::android`）。
///
/// 重复调用是安全的：队列只建一次，回调只是被重新注册（平台那边只有一个槽）。
pub fn install() {
    let _ = slot();
    register_platform_handler();
}

/// 平台层入口：把一条 URL 收下。队列没建过也照收（`slot()` 会就地建）。
pub fn deliver(url: &str) {
    match slot().lock() {
        Ok(mut queue) => {
            // 只留最近几条：一个坏循环把队列撑大没有意义，而正常一次授权只有一条。
            if queue.len() < 8 {
                queue.push(url.to_string());
            }
        }
        Err(err) => tracing::warn!("cloud_link: 回跳队列的锁中毒：{err}"),
    }
}

/// 取走**全部**已收到的 URL（先进先出）。渲染帧里调，拿到的交给
/// [`parse_code`] 过滤一遍。
pub fn drain() -> Vec<String> {
    match slot().lock() {
        Ok(mut queue) => std::mem::take(&mut *queue),
        Err(err) => {
            tracing::warn!("cloud_link: 回跳队列的锁中毒，丢弃：{err}");
            Vec::new()
        }
    }
}

/// 从回跳 URL 里取授权码。只认 `folio://aliyun/callback?code=…` 这一条。
///
/// 认不出来（别的深链、缺 `code`、空 `code`）回 `None` —— 上层据此当作「不是给
/// 网盘的」，不做任何事。
pub fn parse_code(url: &str) -> Option<String> {
    let rest = url.strip_prefix("folio://")?;
    let (path, query) = rest.split_once('?')?;
    if !path.eq_ignore_ascii_case("aliyun/callback") {
        return None;
    }
    for pair in query.split('&') {
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        if name == "code" {
            let code = percent_decode(value);
            return (!code.trim().is_empty()).then(|| code.trim().to_string());
        }
    }
    None
}

/// `%XX` 还原。授权码里通常只有字母数字与 `-` `_`，但查询串是转义过的，
/// 还原一次才拿到页面上那串原值。
///
/// `+` **不**当空格：查询串里 `+` 是表单编码的约定，OAuth 的 `code` 用的是
/// `%XX`，把 `+` 折成空格反而会改坏一个合法的 code。
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &raw[i + 1..i + 3];
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 把上面那条队列接上平台。只有 Android 有 `deeplink` 包（feature 在
/// `crates/folio-app/Cargo.toml` 里打开）。
fn register_platform_handler() {
    #[cfg(target_os = "android")]
    {
        gpui_mobile::packages::deeplink::set_deep_link_handler(deliver);
        // 冷启动那一条：应用是**被这条链接拉起**的（而不是先开着再回跳）。
        if let Ok(Some(initial)) = gpui_mobile::packages::deeplink::get_initial_link() {
            deliver(&initial);
        }
    }
}

#[cfg(test)]
mod tests {
    //! 只钉解析这一段：它是唯一有分支的地方，平台回调无法在无头窗口里跑。

    use super::parse_code;

    #[test]
    fn the_callback_code_is_picked_out() {
        assert_eq!(
            parse_code("folio://aliyun/callback?code=abc123").as_deref(),
            Some("abc123")
        );
        // 前面还带着 state 之类的参数：只要 code。
        assert_eq!(
            parse_code("folio://aliyun/callback?state=x&code=q-1_2").as_deref(),
            Some("q-1_2")
        );
        // 转义过的值要还原。
        assert_eq!(
            parse_code("folio://aliyun/callback?code=a%2Fb%3Dc").as_deref(),
            Some("a/b=c")
        );
    }

    #[test]
    fn other_links_are_ignored() {
        assert!(parse_code("folio://video/xyz").is_none(), "别的深链不认");
        assert!(
            parse_code("folio://aliyun/callback").is_none(),
            "没有查询串"
        );
        assert!(
            parse_code("folio://aliyun/callback?state=1").is_none(),
            "没有 code"
        );
        assert!(
            parse_code("folio://aliyun/callback?code=").is_none(),
            "空 code 不算拿到"
        );
        assert!(
            parse_code("https://example.com/?code=x").is_none(),
            "不是我们那条"
        );
    }

    /// `+` 不改写：OAuth 的 code 用 `%XX`，把 `+` 折成空格会改坏一个合法值。
    #[test]
    fn plus_is_not_a_space() {
        assert_eq!(
            parse_code("folio://aliyun/callback?code=a+b").as_deref(),
            Some("a+b")
        );
    }
}
