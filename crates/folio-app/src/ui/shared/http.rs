//! 装一个真能发请求的 HTTP client。
//!
//! gpui 默认的这份是 `NullHttpClient` —— 任何请求都会直接失败。folio 以前完全不联网，
//! 从来没装过它，所以这一步是「远程书库」所有功能的前置。
//!
//! 装的位置是启动回调里的 `App::set_http_client`，不是接在 `Application` 的构造链上：
//! iOS 与 HarmonyOS 的 `Application` 由框架内部建，那条链够不着，而四个平台的启动回调
//! 都拿得到 `&mut App`。
//!
//! 底层是依赖图里**已经有**的那份 reqwest（`gpui-pre-reqwest`，走 rustls 而不是 OpenSSL），
//! 所以这里不存在第二份 HTTP 栈。它自带一个小号 tokio runtime 并在其中驱动请求 —— 这一条
//! 是必需的：gpui 的 executor 不是 tokio，把 reqwest 的 future 直接丢进
//! `cx.background_spawn` 会因为拿不到 reactor 而 panic。

use std::sync::Arc;

use gpui_kit::App;

/// User-Agent。服务器日志要能认出这是 folio，而不是一个匿名客户端 ——
/// 网盘类服务出问题时要靠这一行定位是谁在打它。
const USER_AGENT: &str = concat!("folio/", env!("CARGO_PKG_VERSION"));

/// 失败只记日志，不中断启动：装不上 HTTP client 时本地书库照常能用，只是远程那一栏不可用。
/// 这与「书库打不开时界面照起、只是进度落不了盘」是同一种立场。
pub fn install(cx: &mut App) {
    match reqwest_client::ReqwestClient::user_agent(USER_AGENT) {
        Ok(client) => cx.set_http_client(Arc::new(client)),
        Err(err) => tracing::error!("http: HTTP client 装不上，远程书库不可用：{err:#}"),
    }
}
