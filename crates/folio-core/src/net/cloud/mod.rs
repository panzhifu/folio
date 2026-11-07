//! 网盘取书：把「另一头存着书的存储」抽象成一个能列目录、能流式下载的东西。
//!
//! 这一层的读者有两个：导入管线（[`crate::ingest::import`] 只需要一个 `Source`，
//! 把字节送进暂存区）与书架上的浏览界面（只需要一层目录清单）。两边都不该知道
//! 对面是 WebDAV 还是百度 —— 所以规则收进 [`Drive`]，各服务商一份实现，全部走
//! [`crate::net::remote::Transport`] 出网，离线可测（假传输 + fixture）。
//!
//! 错误词表沿用 [`crate::net::remote::Error`]：取书撞到的失败（连不上、认证被拒、
//! 路径不存在）与同步撞到的失败是同一批，界面上照同一套「读者能做什么」的分档
//! 去翻，不另起炉灶。
//!
//! 这一节与配置里的 [`crate::infra::config::Cloud`]（账号凭据）、同步那节的
//! [`crate::infra::config::Sync`] 互不相干：取书与同步可以指同一家服务商，但谁也不
//! 假设对方存在。

pub mod baidu;
pub mod dav;

use crate::net::remote::{Cancel, Result, Sink};

pub use crate::net::remote::dav::Entry;

/// 一种网盘。实现只管「列一层、下一本」；浏览树、判重、入库都是别人的事。
///
/// `path` 的语义与 [`Entry::path`] 对齐：相对网盘根、用 `/` 分隔、不带尾斜杠；
/// 空串就是根。实现收到什么路径就请求什么路径，不做改写 —— 导航（进目录、回上级）
/// 是界面那一侧的责任，它手里拿的就是 [`Entry::path`]。
pub trait Drive: Send + Sync + 'static {
    /// 这是哪家网盘，进日志与导入候选的出处（[`crate::ingest::import::Origin::Cloud`]）。
    /// 只给人看与查日志用，不参与任何分支逻辑；界面上的显示名走 locales，不在这。
    fn label(&self) -> &'static str;

    /// 列一层。`path` 为空就是根。
    fn list(&self, path: &str) -> Result<Vec<Entry>>;

    /// 把一个条目流式送进 `sink`。只对文件条目调它 —— 目录没有可下的字节。
    fn download(&self, path: &str, sink: &mut dyn Sink, cancel: &Cancel) -> Result<u64>;
}
