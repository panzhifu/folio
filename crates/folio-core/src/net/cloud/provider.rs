//! 网盘 provider 的**契约**：一台网盘要回答的五个问题，与实现该往哪儿放。
//!
//! 加一家网盘此前要碰的分支有四处（`cloud::view` 的 `configured` / 开表单 / 默认目录 /
//! `build_drive` 四个 `match`），全靠 `Kind` 枚举撑着 —— 新增一家就是四处各加一臂，
//! 漏一臂的症状各不相同（按钮点不开、目录从上一家接着走、造出来的客户端是别的家的）。
//!
//! 这份 trait 把**行为**那四处收成一个接口：实现交出「你是谁、配好了没有、浏览从哪里
//! 开始、怎么造一台客户端」，调用方只认 [`providers()`] 那张表（住在 app 侧 ——
//! 百度 / 阿里云盘的 [`AuthedStore`] 包装要接 app 的凭据落盘通道）。
//!
//! **留在外面**的是界面那一半：各家的账号表单本来就是不同的屏（WebDAV 三格地址、
//! 两家 OAuth 各一张表单），按钮与副标题也是逐家手摆的 —— 把「表单长什么样」也塞进
//! trait 等于把 gpui 类型带进 core，不干。

use std::sync::Arc;

use super::Drive;
use crate::infra::config::Cloud;
use crate::net::remote::Transport;

/// 一台网盘要回答的五个问题。
///
/// 实现住在 app 侧（`services::cloud_providers`，一张静态表）；这一层只认 core 类型
/// （[`Cloud`] / [`Drive`] / [`Entry`]），所以契约本身离线可测。
pub trait CloudProvider: Send + Sync + 'static {
    /// 稳定标识：进日志、provenance 与测试查找。**不要**拿它做界面文案。
    fn kind(&self) -> &'static str;

    /// 界面显示名的 locales 键（`cloud.webdav` / `cloud.baidu` / `cloud.aliyun` 一族）。
    /// 翻译在 app 侧，这里只交键。
    fn label_key(&self) -> &'static str;

    /// [`Cloud`] 里对应的那一节配好了没有。没配好时 [`Self::build_drive`] 也回 `None`，
    /// 这里单独问是因为界面要在**开表单**之前就知道（没配 → 带去表单，而不是报错）。
    fn configured(&self, cloud: &Cloud) -> bool;

    /// 浏览的起点目录。WebDAV 的默认目录是配置里那一格；两家网盘没有这格，从根开始。
    fn default_dir(&self, cloud: &Cloud) -> String;

    /// 造一台浏览用的客户端。没配好回 `None`。
    ///
    /// 两家网盘的实现套着 [`AuthedStore`](crate::net::cloud::auth::AuthedStore)
    /// （过期先续、401 强刷重放）；WebDAV 是 Basic 认证、没有可续的 token，直连。
    fn build_drive(&self, cloud: &Cloud, transport: Arc<dyn Transport>) -> Option<Box<dyn Drive>>;
}
