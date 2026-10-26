//! 出网与入网：folio 与外部世界打交道的两条路。
//!
//! - [`remote`] —— 远端传输的**协议与编排**（WebDAV 的 PROPFIND / GET / PUT 规则，
//!   以及一个同步 `Transport` 抽象）。协议规则与实现分开，所以它们离线可测。
//! - [`translate`] —— 选区翻译：引擎 trait 与三只引擎（Google 免 key、OpenAI 兼容、
//!   DeepL）的请求/响应解析。走的也是 [`remote`] 那个 `Transport`，所以它一样离线可测
//!   （假传输 + fixture）。
//! - [`update`] —— 检查更新：问发布仓要「最新那版」，比版本、挑这一端的包、验摘要。
//!   走的也是 [`remote`] 那个 `Transport`，所以它一样离线可测（假传输 + fixture JSON）。
//! - [`transfer`] —— **Wi-Fi 传书**：设备起一个 HTTP 服务器，局域网浏览器上传书。
//!
//! **`tokio` 只许出现在 [`transfer`] 里**。core 其余部分是同步形状，公共签名不漏
//! `Future`；传书服务器要「少量线程轮转服务大量连接」的现成调度与 multipart 的
//! 现成流式解析，才把它关在这一个模块内。这条边界写在 [`transfer`] 的模块文档里，
//! 并由本模块的文档再钉一次。

pub mod opds;
pub mod remote;
pub mod transfer;
pub mod translate;
pub mod update;
