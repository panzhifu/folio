//! 网络的**入口层**：配置到参数的挑选都收在这里，协议与机制住在别处。
//!
//! - [`remote`] —— 传输抽象与 WebDAV，实现在 [`folio_net`] crate（「换 crate 不换
//!   形状」：这里沿用旧路径，消费方一行不改）。
//! - [`translate`] —— 选区翻译的入口：按配置挑引擎的 `configured`。引擎机制在
//!   [`folio_translate`]（词典也在那边），走的也是 [`remote`] 那个 `Transport`。
//! - [`ai`] —— 选区问 AI 的入口：按配置挑引擎与判「这一格存不存在」（地址空 / 没填
//!   key 不出网）。机制在 [`folio_ai`]，形状照 [`translate`] 立，两条错误链的对应
//!   关系由那一侧的 `From` 记着。
//! - [`cloud`] —— 网盘的入口：[`provider`] 契约吃 [`crate::infra::config::Cloud`]，
//!   实现住在 app 侧。引擎机制（`Drive` 与各网盘实现）在 `folio_net::cloud`，
//!   这里 re-export 沿旧路径。
//!
//! 其余的网络机制已随阶段三扩容搬去 [`folio_net`]（2026-11-12）：OPDS、更新检查、
//! 会员兑换、Wi-Fi 传书（axum 服务器）与局域网同步的协议——本模块 re-export 沿
//! 旧路径，`crate::net::opds::*` 这类写法一行不改。`tokio` / `axum` 跟着传书那族
//! 进了 folio-net，被关在那两族里；core 侧从此是纯同步形状，公共签名不漏 `Future`。

pub mod ai;
pub mod cloud;
pub mod translate;

// 协议与机制已随阶段三扩容搬去 folio-net（2026-11-12）：
// - [`cloud`] 的引擎半边（Drive trait 与各网盘实现）在 `folio_net::cloud`，这里留
//   provider 契约（吃 config::Cloud 的入口层）；
// - OPDS / 更新检查 / 会员兑换 / 局域网两族在 `folio_net::{opds, update, redeem,
//   lansync, transfer}`（transfer 的 axum 服务器与 lansync 的服务端同进同退）。
// 旧路径沿用：`crate::net::opds::*` 这类写法（app 各屏与服务）一行不改。
pub use folio_net::{lansync, opds, redeem, transfer, update};

// 传输层已独立成 `folio-net` crate（Transport / RemoteStore / Sink / WebDAV）。
// 旧路径沿用：`folio_core::net::remote::*` 与 crate 内的 `crate::net::remote::*`
// 都解析到它，消费方（含 `error.rs` 的 `#[from]`）一行不改。
pub use folio_net as remote;
