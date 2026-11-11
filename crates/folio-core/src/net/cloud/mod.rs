//! 网盘取书：机制已随阶段三扩容搬去 folio-net（`folio_net::cloud`，2026-11-12），
//! 这里 re-export 沿旧路径；留下的是 [`provider`] —— 「按配置挑哪家网盘」的契约，
//! 它吃 [`crate::infra::config::Cloud`]，是配置到参数的入口层（与 folio-ai 的
//! `configured` 同一条画法），实现住在 app 侧。

pub mod provider;

pub use folio_net::cloud::{
    Drive, Entry, aliyun, aliyun_auth, auth, baidu, baidu_auth, dav, upload,
};
pub use provider::CloudProvider;
