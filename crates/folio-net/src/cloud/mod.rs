//! 网盘取书：把「另一头存着书的存储」抽象成一个能列目录、能流式下载的东西。
//!
//! 这一层的读者是导入管线与书架上的浏览界面（都在 folio-core / app 那侧，经
//! `folio_core::net::cloud` 的 shim 沿旧路径）——两边都不该知道对面是 WebDAV 还是
//! 百度，所以规则收进 [`Drive`]，各服务商一份实现，全部走 [`crate::Transport`] 出网，
//! 离线可测（假传输 + fixture）。原住 folio-core::net::cloud（2026-11-12 阶段三扩容
//! 搬来）；「按配置挑哪家网盘」的契约（`CloudProvider`，吃 `config::Cloud`）留在
//! folio-core——那是配置到参数的入口层，与 folio-ai 的 `configured` 同一条画法。
//!
//! 错误词表沿用 [`crate::Error`]：取书撞到的失败（连不上、认证被拒、路径不存在）与
//! 同步撞到的失败是同一批，界面上照同一套「读者能做什么」的分档去翻，不另起炉灶。
//!
//! 这一节与配置里的「账号凭据」、同步那节互不相干：取书与同步可以指同一家服务商，
//! 但谁也不假设对方存在。

pub mod aliyun;
pub mod aliyun_auth;
pub mod auth;
pub mod baidu;
pub mod baidu_auth;
pub mod dav;
pub mod upload;

pub use crate::dav::Entry;

/// 一种网盘。仓库能力（列一层、取一个、写一个、建目录、下载）全在
/// [`RemoteStore`](crate::RemoteStore) 里；这一层只补一个给人看的名字。
///
/// `path` 的语义与 [`Entry::path`] 对齐：相对这一台网盘的根、用 `/` 分隔、不带尾斜杠；
/// 空串就是根。实现收到什么路径就请求什么路径，不做改写 —— 导航（进目录、回上级）
/// 是界面那一侧的责任，它手里拿的就是 [`Entry::path`]。
pub trait Drive: crate::RemoteStore {
    /// 这是哪家网盘，进日志与导入候选的出处。只给人看与查日志用，不参与任何分支
    /// 逻辑；界面上的显示名走 locales，不在这。
    fn label(&self) -> &'static str;
}
