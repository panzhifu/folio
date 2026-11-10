//! 出网与入网：folio 与外部世界打交道的两条路。
//!
//! - [`remote`] —— 远端传输的**协议与编排**。传输抽象与 WebDAV 已独立成
//!   [`folio_net`] crate（「换 crate 不换形状」：这里沿用旧路径，消费方一行不改）。
//!   协议规则与实现分开，所以它们离线可测。
//! - [`translate`] —— 选区翻译：引擎 trait 与三只引擎（Google 免 key、OpenAI 兼容、
//!   DeepL）的请求/响应解析。机制已独立成 [`folio_ai`] crate（与 [`ai`] 同 crate，
//!   「同一件事的两张脸」）；走的也是 [`remote`] 那个 `Transport`，所以它一样离线可测
//!   （假传输 + fixture）。
//! - [`ai`] —— 选区问 AI 与切章规则草案：**一层** provider（OpenAI 兼容的
//!   `chat/completions`），地址与密钥共用 [`translate`] 那一档，错误分档与响应解析的形状
//!   也照它，所以同样离线可测（假传输 + 固定应答）。
//! - [`update`] —— 检查更新：问发布仓要「最新那版」，比版本、挑这一端的包、验摘要。
//!   走的也是 [`remote`] 那个 `Transport`，所以它一样离线可测（假传输 + fixture JSON）。
//! - [`redeem`] —— 会员兑换：把一个爱发电订单号换回会员天数。付没付过钱只能由服务端判
//!   （爱发电的 api token 不能进客户端），所以这一层只管形状、一次 POST、以及读回应答。
//! - [`transfer`] —— **Wi-Fi 传书**：设备起一个 HTTP 服务器，局域网浏览器上传书。
//! - [`lansync`] —— **局域网同步**的协议形状：同账号的两台设备怎么互相发现、怎么把书架
//!   清单与快照递给对面。它自己不起服务器（挂在 [`transfer`] 上）、不拨连接（走
//!   [`remote`] 那个 `Transport`），所以这一层离线可测。
//! - [`cloud`] —— **网盘取书**：把「另一头存着书的存储」抽象成一个 `Drive`
//!   （列一层、流式下载），WebDAV 与百度各一份实现，给浏览界面与导入来源共用。
//!   走的也是 [`remote`] 那个 `Transport`，离线可测。
//!
//! **`tokio` 只许出现在 [`transfer`] 里**。core 其余部分是同步形状，公共签名不漏
//! `Future`；传书服务器要「少量线程轮转服务大量连接」的现成调度与 multipart 的
//! 现成流式解析，才把它关在这一个模块内。这条边界写在 [`transfer`] 的模块文档里，
//! 并由本模块的文档再钉一次。

pub mod ai;
pub mod cloud;
pub mod lansync;
pub mod opds;
pub mod redeem;
pub mod transfer;
pub mod translate;
pub mod update;

// 传输层已独立成 `folio-net` crate（Transport / RemoteStore / Sink / WebDAV）。
// 旧路径沿用：`folio_core::net::remote::*` 与 crate 内的 `crate::net::remote::*`
// 都解析到它，消费方（含 `error.rs` 的 `#[from]`）一行不改。
pub use folio_net as remote;
