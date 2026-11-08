//! [`CloudProvider`] 的三份实现与那张静态表。
//!
//! 实现住在这里而不是 core 的理由：百度 / 阿里云盘的 [`AuthedStore`] 包装要接 app 的
//! 凭据落盘通道（`cloud_auth::Pending`），core 不能反向依赖 app。trait 本身在 core
//! （`net::cloud::provider`），契约与 core 类型一起离线可测；这一层只做转发，
//! 没有任何分支逻辑。
//!
//! 加一家网盘时动这里：一份 impl + `PROVIDERS` 表里添一条。`cloud::view` 的行为
//! 分支（配没配好、从哪儿开始、造哪台客户端）全部走这张表，不再按家写 `match`。

use std::sync::Arc;

use folio_core::Cloud;
use folio_core::net::cloud::dav::DavDrive;
use folio_core::net::cloud::{CloudProvider, Drive};
use folio_core::net::remote::Transport;

use crate::ui::services::cloud_auth;

/// WebDAV：Basic 认证，没有可续的 token，直连 [`DavDrive`]。
pub struct WebDav;

impl CloudProvider for WebDav {
    fn kind(&self) -> &'static str {
        "webdav"
    }

    fn label_key(&self) -> &'static str {
        "cloud.webdav"
    }

    fn configured(&self, cloud: &Cloud) -> bool {
        cloud.webdav.configured()
    }

    /// 浏览的起点是配置里那一格（`dir`）；`normalized` 已经掐过头尾斜杠。
    fn default_dir(&self, cloud: &Cloud) -> String {
        cloud.webdav.dir.clone()
    }

    fn build_drive(&self, cloud: &Cloud, transport: Arc<dyn Transport>) -> Option<Box<dyn Drive>> {
        cloud.webdav.configured().then(|| {
            let webdav = &cloud.webdav;
            Box::new(DavDrive::open(
                webdav.url.clone(),
                webdav.username.clone(),
                webdav.password.clone(),
                transport,
            )) as Box<dyn Drive>
        })
    }
}

/// 百度网盘：token 续期套在 [`AuthedStore`] 上（浏览根为空，整个网盘）。
pub struct Baidu;

impl CloudProvider for Baidu {
    fn kind(&self) -> &'static str {
        "baidu"
    }

    fn label_key(&self) -> &'static str {
        "cloud.baidu"
    }

    fn configured(&self, cloud: &Cloud) -> bool {
        cloud.baidu.configured()
    }

    /// 没有默认目录：浏览从根开始。
    fn default_dir(&self, _cloud: &Cloud) -> String {
        String::new()
    }

    fn build_drive(&self, cloud: &Cloud, transport: Arc<dyn Transport>) -> Option<Box<dyn Drive>> {
        cloud
            .baidu
            .configured()
            .then(|| Box::new(cloud_auth::baidu_browse(&cloud.baidu, transport)) as Box<dyn Drive>)
    }
}

/// 阿里云盘：同百度那一条，token 续期套在 [`AuthedStore`] 上。
pub struct Aliyun;

impl CloudProvider for Aliyun {
    fn kind(&self) -> &'static str {
        "aliyun"
    }

    fn label_key(&self) -> &'static str {
        "cloud.aliyun"
    }

    fn configured(&self, cloud: &Cloud) -> bool {
        cloud.aliyun.configured()
    }

    fn default_dir(&self, _cloud: &Cloud) -> String {
        String::new()
    }

    fn build_drive(&self, cloud: &Cloud, transport: Arc<dyn Transport>) -> Option<Box<dyn Drive>> {
        cloud.aliyun.configured().then(|| {
            Box::new(cloud_auth::aliyun_browse(&cloud.aliyun, transport)) as Box<dyn Drive>
        })
    }
}

/// 全部的网盘。`cloud::view` 的行为分支按这张表走；UI（按钮、表单、副标题）仍是
/// 逐家手摆的 —— 那一半留在界面上是有意的（见 trait 的模块注释）。
pub static WEBDAV: WebDav = WebDav;
pub static BAIDU: Baidu = Baidu;
pub static ALIYUN: Aliyun = Aliyun;

/// 那张表本身也要是静态的：`providers()` 回的是切片引用，背后不能是一个临时数组。
static PROVIDERS: [&'static dyn CloudProvider; 3] = [&WEBDAV, &BAIDU, &ALIYUN];

pub fn providers() -> &'static [&'static dyn CloudProvider] {
    &PROVIDERS
}

/// 按稳定标识查一家。查无此名回 `None`（配置里手改坏的键不让它 panic）。
pub fn by_kind(kind: &str) -> Option<&'static dyn CloudProvider> {
    providers()
        .iter()
        .copied()
        .find(|provider| provider.kind() == kind)
}

#[cfg(test)]
mod tests {
    //! 三份实现的行为门：配没配好、浏览起点、造出来的客户端。全离线 ——
    //! `Cloud` 是普通结构体，`build_drive` 不碰 gpui 也不碰网络。

    use std::sync::Arc;

    use folio_core::infra::config::{AliyunDrive, BaiduDrive, Cloud, WebDavDrive};
    use folio_core::net::cloud::CloudProvider;
    use folio_core::net::remote::Transport;

    use super::{Aliyun, Baidu, WebDav, by_kind, providers};

    fn transport() -> Arc<dyn Transport> {
        crate::ui::services::update::transport()
    }

    fn configured_cloud() -> Cloud {
        Cloud {
            webdav: WebDavDrive {
                url: "https://dav.example.com/dav".into(),
                username: "noke".into(),
                password: "app-pass".into(),
                dir: "我的书".into(),
            },
            baidu: BaiduDrive {
                access_token: "121.abc".into(),
                refresh_token: "122.cd".into(),
                expires_at: 1,
                account: "山间的读客".into(),
            },
            aliyun: AliyunDrive {
                access_token: "ali.tok".into(),
                refresh_token: "ali.rt".into(),
                expires_at: 1,
                drive_id: "88".into(),
                account: "阿里的读者".into(),
            },
        }
    }

    /// 配好时三家都造得出客户端，`label` 原样转自内层。
    #[test]
    fn a_configured_provider_builds_a_drive() {
        let cloud = configured_cloud();
        for provider in providers() {
            let drive = provider
                .build_drive(&cloud, transport())
                .unwrap_or_else(|| panic!("{} 配好了却造不出", provider.kind()));
            assert!(
                matches!(drive.label(), "webdav" | "baidu" | "aliyun"),
                "{} 的 label 不认识：{}",
                provider.kind(),
                drive.label()
            );
        }
    }

    /// 没配好就回 `None`：`configured` 与 `build_drive` 对同一份配置给同一个答案。
    #[test]
    fn an_unconfigured_provider_builds_nothing() {
        let cloud = Cloud::default();
        for provider in providers() {
            assert!(
                !provider.configured(&cloud),
                "{} 默认就算配好",
                provider.kind()
            );
            assert!(
                provider.build_drive(&cloud, transport()).is_none(),
                "{} 没配好还造得出客户端",
                provider.kind()
            );
        }
    }

    /// 各家只认自己的那一节：百度没配不该让阿里云盘跟着说没配好。
    #[test]
    fn each_provider_reads_only_its_own_section() {
        let mut cloud = configured_cloud();
        cloud.baidu.access_token = String::new();
        assert!(WebDav.configured(&cloud));
        assert!(!Baidu.configured(&cloud), "只清了百度，别家不该受牵连");
        assert!(Aliyun.configured(&cloud));

        // 阿里云盘要多一样：没有 drive_id 的请求必被拒，所以不算配好。
        let mut cloud = configured_cloud();
        cloud.aliyun.drive_id = String::new();
        assert!(!Aliyun.configured(&cloud), "缺 drive_id 不算配好");
    }

    /// 浏览起点：WebDAV 用配置里那格目录，两家网盘从根开始。
    #[test]
    fn the_default_dir_follows_the_provider() {
        let cloud = configured_cloud();
        assert_eq!(WebDav.default_dir(&cloud), "我的书");
        assert_eq!(Baidu.default_dir(&cloud), "");
        assert_eq!(Aliyun.default_dir(&cloud), "");
    }

    /// 表里每一家都查得到，键互不相同（查错家等于把书传到别人的盘上）。
    #[test]
    fn the_registry_covers_every_provider_by_unique_kind() {
        let keys: Vec<&str> = providers().iter().map(|p| p.kind()).collect();
        let unique: std::collections::HashSet<_> = keys.iter().copied().collect();
        assert_eq!(keys.len(), unique.len(), "kind 撞了：{keys:?}");

        for key in &keys {
            assert!(by_kind(key).is_some(), "by_kind 查不到 {key}");
        }
        assert!(by_kind("没有这家").is_none());
    }
}
