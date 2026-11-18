//! 应用配置入口：领域字段分文件，序列化形状和公开路径保持稳定。

mod account;
mod appearance;
mod assistance;
mod cloud;
mod environment;
mod library;
mod reading;
mod speech;

pub use account::{Account, Claim, Membership, MembershipTier, Sync, SyncProvider};
pub use appearance::{Appearance, Mode, PageTurn, ThemeBackground};
pub use assistance::{
    Ai, DeepL, Dictionary, DictionaryEntry, FACTORY_BASE_URL, FontEntry, Fonts, OpenAi,
    Translation, TranslationEngine, Vendor, host_of, new_vendor_id, resolve_vendor,
};
pub use cloud::{AliyunDrive, BaiduDrive, Cloud, Opds, OpdsCatalog, WebDavDrive};
use environment::flatten_keys;
pub use environment::{Env, IGNORE_CONFIG_ENV, LOG_OFF_ENV, Layer, Provenance, Resolved};
pub use folio_base::logging::Logging;
pub use folio_model::reading::ImageScale;
pub use library::{Backup, Language, Library, Novel, WatchDir};
pub use reading::{Reader, TextDirection, TocDensity};
pub use speech::{Speech, SpeechRole};

use crate::error::IoResultExt as _;
use crate::infra::paths;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const CONFIG_VERSION: u32 = 1;

/// 应用配置。Serialize 保留完整持久化语义，Debug 不输出凭据。
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 这份配置的格式版本（[`CONFIG_VERSION`]）。老文件里没有这一格 → serde 缺省成 `0`，
    /// 于是 `load` 能认出「这是没版本戳的老文件」并迁移（见 [`Config::migrated`]）。
    pub version: u32,
    pub reader: Reader,
    pub appearance: Appearance,
    pub library: Library,
    pub language: Language,
    pub speech: Speech,
    pub translation: Translation,
    pub ai: Ai,
    /// OpenAI 兼容端点档案的注册表。翻译与 AI 各自按 id 引用其中一条
    /// （地址与密钥只存这一份），见 [`Vendor`]。
    pub vendors: Vec<Vendor>,
    pub dictionary: Dictionary,
    /// 读者自己装进来的字体（下载或导入），见 [`Fonts`]。
    pub fonts: Fonts,
    pub backup: Backup,
    pub sync: Sync,
    pub account: Account,
    pub opds: Opds,
    /// 网盘取书的账号（WebDAV + 百度）。与 [`Sync`] 独立成节，理由见 [`Cloud`]。
    pub cloud: Cloud,
    pub novel: Novel,
    pub membership: Membership,
    pub logging: Logging,
    /// 正文主题的背景图，按**主题名**索引（见 [`ThemeBackground`]）。
    ///
    /// 存在根节而不在 [`Appearance`] 里：Appearance 整节是按书差量的底表，图配置
    /// 挂进去就会被逐格比进每本书的差量 —— 图是主题的属性，不是某一本书的。
    pub reading_backgrounds: BTreeMap<String, ThemeBackground>,
}

impl Default for Config {
    /// 手写而不是 derive：`version` 的默认是 [`CONFIG_VERSION`]，不是 `0` —— derive 会把
    /// 整数给 0，那样「新装的机器」（读不到文件，用默认）与「老文件缺版本戳」就分不开了。
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            reader: Reader::default(),
            appearance: Appearance::default(),
            library: Library::default(),
            language: Language::default(),
            speech: Speech::default(),
            translation: Translation::default(),
            ai: Ai::default(),
            vendors: Vec::new(),
            dictionary: Dictionary::default(),
            fonts: Fonts::default(),
            backup: Backup::default(),
            sync: Sync::default(),
            account: Account::default(),
            opds: Opds::default(),
            cloud: Cloud::default(),
            novel: Novel::default(),
            membership: Membership::default(),
            logging: Logging::default(),
            reading_backgrounds: BTreeMap::new(),
        }
    }
}

impl Config {
    /// 把读回来的、或界面改过的值规整一遍：排版数值夹住越界项、外观去掉空主题名。
    /// 所有写入方都过一次这里，规整规则就只有一处。
    ///
    /// 界面语言**不**在这里推给全局：那是 `folio_app` 的 `ConfigHolder` 在装载配置和
    /// 每次改动时做的事，核心模块不该在读配置时顺手改全局状态。
    pub fn normalized(mut self) -> Self {
        self.reader = self.reader.clamped();
        self.appearance = self.appearance.normalized();
        self.speech = self.speech.normalized();
        self.translation = self.translation.normalized();
        self.ai = self.ai.normalized();
        self.vendors = self.vendors.into_iter().map(Vendor::normalized).collect();
        self.dictionary = self.dictionary.normalized();
        self.fonts = self.fonts.normalized();
        self.sync = self.sync.normalized();
        self.account = self.account.normalized();
        self.opds = self.opds.normalized();
        self.cloud = self.cloud.normalized();
        self.novel = self.novel.normalized();
        // 书库位置去掉首尾空白：填成 "  /x  " 会让 `is_confirmed` 说「确认过」，
        // 而后面按这个串去找目录会找不到。
        self.library.dir = self.library.dir.trim().to_string();
        if let Some(watch) = &mut self.library.watch {
            watch.dir = watch.dir.trim().to_string();
            watch.interval_secs = watch.interval_secs.clamp(5, 3600);
            // 空目录、或监听的就是书库目录自己：前者没得盯，后者会自我循环
            // （书库里出现的书就是刚从那里拷进来的）。都当没开。
            if watch.dir.is_empty() || watch.dir == self.library.dir {
                self.library.watch = None;
            }
        }
        self.logging = self.logging.normalized();
        // 背景图配置：不透明度归一（坏值不当成选择），空文件名按「没配」处理 ——
        // 与 Appearance 里主题名那两条同一口径。
        for background in self.reading_backgrounds.values_mut() {
            background.opacity = ThemeBackground::normalized_opacity(background.opacity);
            for slot in [&mut background.light, &mut background.dark] {
                *slot = slot
                    .take()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
            }
        }
        self
    }

    /// 读默认位置的**生效值**。文件不存在或内容坏了都退回默认值 —— 配置绝不该让程序起不来。
    pub fn load() -> Config {
        Self::load_resolved().effective
    }

    /// 读取默认位置，保留用户层、生效层与字段来源。
    pub fn load_resolved() -> Resolved {
        Self::resolve(&paths::config_path(), &Env::from_env())
    }

    /// 按默认、文件、环境合并；损坏配置按节抢救。
    pub fn resolve(path: &Path, env: &Env) -> Resolved {
        // 文件层：`Env::ignore_file` 时整层跳过（应急开关）。
        let file_value = if env.ignore_file {
            tracing::warn!("config: {IGNORE_CONFIG_ENV} 开着，跳过 {}", path.display());
            None
        } else {
            Self::read_value(path)
        };

        // 用户层 = 内置默认 ⊕ 文件。
        let user = match &file_value {
            Some(value) => {
                // 手改过 / 旧版本 / 别的机器同步来的值都不可信，读进来先规整一遍。
                if let Ok(config) = serde_json::from_value::<Config>(value.clone()) {
                    warn_missing_sections(value);
                    config.migrate_vendors().migrated().normalized()
                } else {
                    tracing::warn!("config: {} 整份解不开，按节抢救", path.display());
                    salvage(value).migrate_vendors().migrated().normalized()
                }
            }
            None => Config::default(),
        };

        // provenance：文件里出现过的键标成 File，其余留 Default。
        let mut provenance = Provenance::default();
        if let Some(value) = &file_value {
            let mut keys = BTreeSet::new();
            flatten_keys(value, "", &mut keys);
            for key in keys {
                provenance.mark(key, Layer::File);
            }
        }

        // 生效值 = 用户层 ⊕ 环境变量；被环境盖到的键标成 Env。
        let mut effective = user.clone();
        env.apply(&mut effective);
        for key in env.touched_keys() {
            provenance.mark(key, Layer::Env);
        }

        Resolved {
            user,
            effective,
            provenance,
        }
    }

    /// 读取指定路径，不叠加进程环境。
    pub fn load_from(path: &Path) -> Config {
        Self::resolve(path, &Env::default()).effective
    }

    /// 读取配置为 JSON；读不出或 JSON 非法时回默认，不记录原文。
    fn read_value(path: &Path) -> Option<serde_json::Value> {
        let Ok(text) = std::fs::read_to_string(path) else {
            tracing::debug!("config: 没有 {}，用默认值", path.display());
            return None;
        };
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(value) => Some(value),
            Err(_) => {
                tracing::warn!("config: {} 不是合法 JSON，整份退回默认值", path.display());
                None
            }
        }
    }

    /// 迁移版本戳；新版文件只读取认识的字段并报告版本差异。
    fn migrated(mut self) -> Self {
        if self.version == CONFIG_VERSION {
            return self;
        }
        if self.version > CONFIG_VERSION {
            tracing::warn!(
                "config: 文件版本 v{} 比本程序（v{CONFIG_VERSION}）新，多出来的字段已忽略",
                self.version
            );
        } else {
            tracing::info!("config: 版本 v{} → v{CONFIG_VERSION}", self.version);
        }
        self.version = CONFIG_VERSION;
        self
    }

    /// 把旧版的**内联端点**折进供应商注册表：`translation.openai` 自带的地址与密钥
    /// 变成第一条档案，引用指过去，`ai.vendor` 留空（跟随翻译 = 还是原来那个端点）。
    ///
    /// **幂等**：折完就把 deprecated 的两格清空，重跑一次什么也找不到。两条不加档案的
    /// 出路 —— 旧地址本来就是空的（没配置过）；或者地址还是出厂那份**且 key 空着**
    /// （读者只被出厂值摆过一道，从没打开过这一格 —— 替他造档案，等于替他决定
    /// 「我的正文可以发去官方 API」，那正是「只有出厂地址不算配置」要拦的事）。
    fn migrate_vendors(mut self) -> Self {
        let openai = &mut self.translation.openai;
        let base = openai.base_url.trim().to_string();
        let key = openai.api_key.trim().to_string();
        if !base.is_empty() && openai.vendor.is_empty() {
            let making = !(base == FACTORY_BASE_URL && key.is_empty());
            if making {
                let id = new_vendor_id();
                let name = host_of(&base);
                self.vendors.push(Vendor {
                    id: id.clone(),
                    name,
                    base_url: base,
                    api_key: key,
                });
                openai.vendor = id;
            } else {
                tracing::debug!("config: 出厂地址且没填 key，迁移不造供应商档案");
            }
        }
        self.translation.openai.base_url.clear();
        self.translation.openai.api_key.clear();
        self
    }

    /// 写默认位置的配置。
    pub fn save(&self) -> crate::Result<()> {
        self.save_to(&paths::config_path())
    }

    /// 先序列化，再写临时文件并替换；失败不以空配置覆盖原文件。
    pub fn save_to(&self, path: &Path) -> crate::Result<()> {
        paths::prepare_dir(path)?;
        let json = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &json).io_path(&tmp)?;
        // 这份 JSON 里住着翻译引擎的 API Key。默认 umask 下新建文件是 0644，
        // 同机其他用户能读走密钥 —— 落盘这一刻就收到 0600，rename 保留权限位。
        // Android 上配置本来就在应用私有目录里，这一档没有副作用。
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).io_path(&tmp)?;
        }
        std::fs::rename(&tmp, path).io_path(path)
    }
}

/// 合法 JSON 中某节损坏时只回退该节；非法 JSON 由读文件入口处理。
fn salvage(value: &serde_json::Value) -> Config {
    let mut config = Config::default();
    // 版本戳能读出来就先留着，交给 `migrated` 对齐 —— 抢救的是坏掉的那一节，
    // 不该顺手把文件声明的版本也抹成默认。
    if let Some(version) = value.get("version").and_then(serde_json::Value::as_u64) {
        config.version = version as u32;
    }
    if let Some(reader) = section(value, "reader") {
        config.reader = reader;
    }
    if let Some(appearance) = section(value, "appearance") {
        config.appearance = appearance;
    }
    if let Some(library) = section(value, "library") {
        config.library = library;
    }
    if let Some(language) = section(value, "language") {
        config.language = language;
    }
    if let Some(speech) = section(value, "speech") {
        config.speech = speech;
    }
    if let Some(logging) = section(value, "logging") {
        config.logging = logging;
    }
    config
}

/// 已存在的配置缺少节时记录缺失节名，不回显内容。
fn warn_missing_sections(value: &serde_json::Value) {
    let Some(object) = value.as_object() else {
        return;
    };
    const SECTIONS: [&str; 6] = [
        "reader",
        "appearance",
        "library",
        "language",
        "speech",
        "logging",
    ];
    let missing: Vec<&str> = SECTIONS
        .into_iter()
        .filter(|key| !object.contains_key(*key))
        .collect();
    if !missing.is_empty() {
        tracing::warn!("config: 缺这些节，各用默认值：{missing:?}");
    }
}

/// 独立解析一节，缺失或解析失败时让调用方保留默认值。
fn section<T: serde::de::DeserializeOwned>(value: &serde_json::Value, key: &str) -> Option<T> {
    value
        .get(key)
        .and_then(|raw| serde_json::from_value::<T>(raw.clone()).ok())
}

// 凭据与用户提供的端点不参与 Debug；Serialize/Deserialize 仍保存完整配置。
macro_rules! redacted_debug {
    ($($kind:ty),+ $(,)?) => { $(
        impl std::fmt::Debug for $kind {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct(stringify!($kind)).finish_non_exhaustive()
            }
        }
    )+ };
}

redacted_debug!(
    Vendor,
    OpenAi,
    DeepL,
    Sync,
    Account,
    Claim,
    OpdsCatalog,
    WebDavDrive,
    BaiduDrive,
    AliyunDrive,
    Env,
    Provenance,
);

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("version", &self.version)
            .field("locale", &self.language.locale)
            .field("vendor_count", &self.vendors.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "config/tests_preferences.rs"]
mod preference_tests;
#[cfg(test)]
#[path = "config/tests_services.rs"]
mod service_tests;
#[cfg(test)]
#[path = "config/tests_load.rs"]
mod tests;
#[cfg(test)]
mod sync_config_tests {
    use super::{Config, Sync, SyncProvider};

    #[test]
    fn sync_trims_the_address_and_dir_but_not_the_password() {
        let config = Config {
            sync: Sync {
                enabled: true,
                provider: SyncProvider::WebDav,
                url: "  https://dav.example/dav/  ".into(),
                username: "  reader  ".into(),
                password: "  pass with spaces  ".into(),
                remote_dir: "  /folio/sync/  ".into(),
                device_id: "  dev-1  ".into(),
            },
            ..Config::default()
        }
        .normalized();
        assert_eq!(config.sync.url, "https://dav.example/dav/");
        assert_eq!(config.sync.username, "reader");
        assert_eq!(config.sync.password, "  pass with spaces  ");
        assert_eq!(config.sync.remote_dir, "folio/sync");
        assert_eq!(config.sync.device_id, "dev-1");
        assert!(config.sync.configured());
    }

    #[test]
    fn sync_requires_both_the_switch_and_an_address() {
        let mut sync = Sync::default();
        assert!(!sync.configured());
        sync.enabled = true;
        assert!(!sync.configured(), "有开关没地址不算配置好");
        sync.url = "https://example.test/".into();
        assert!(sync.configured());
    }

    /// 后端名按界面那两个字写（`webdav` / `baidu`），不按变体名拆出 `web_dav`。
    #[test]
    fn the_sync_provider_uses_lowercase_names() {
        let sync = Sync {
            provider: SyncProvider::Baidu,
            ..Sync::default()
        };
        let json = serde_json::to_string(&sync).unwrap();
        assert!(json.contains("\"provider\":\"baidu\""), "{json}");
        let sync = Sync {
            provider: SyncProvider::WebDav,
            ..Sync::default()
        };
        let json = serde_json::to_string(&sync).unwrap();
        assert!(json.contains("\"provider\":\"webdav\""), "{json}");
        let back: Sync = serde_json::from_str(&json).unwrap();
        assert_eq!(back.provider, SyncProvider::WebDav);
    }

    #[test]
    fn a_sync_section_round_trips_through_json() {
        let config = Config {
            sync: Sync {
                enabled: true,
                provider: SyncProvider::WebDav,
                url: "https://dav.example/".into(),
                username: "u".into(),
                password: "p".into(),
                remote_dir: "folio".into(),
                device_id: "dev".into(),
            },
            ..Config::default()
        };
        let text = serde_json::to_string(&config).unwrap();
        let back: Config = serde_json::from_str(&text).unwrap();
        assert_eq!(back.sync, config.sync);
    }
}

#[cfg(test)]
mod account_config_tests {
    use super::{Account, Config};

    /// 这一段是后加的：老文件里根本没有它，读进来必须落成「没开、也没有密钥」，
    /// 而不是把整份配置判成坏掉。
    #[test]
    fn an_older_config_without_the_account_section_reads_as_no_account() {
        let config: Config = serde_json::from_str(r#"{"version":1}"#).unwrap();
        assert!(!config.account.enabled);
        assert!(config.account.key.is_empty());
        assert!(config.account.peer.is_empty());
    }

    #[test]
    fn account_trims_the_key_it_stores() {
        let config = Config {
            account: Account {
                enabled: true,
                key: "  0123456789abcdef  ".into(),
                peer: "  192.168.1.5:8410  ".into(),
            },
            ..Config::default()
        }
        .normalized();
        assert_eq!(config.account.key, "0123456789abcdef");
        assert_eq!(config.account.peer, "192.168.1.5:8410");
    }

    /// 「有账号」的判据是**解得开**，不是那一格非空：手抄进来少一位的密钥存得住，
    /// 而后面每一趟都只会撞在一个解不开的形状上 —— 界面得在那一刻就说「还没有账号」。
    #[test]
    fn an_unparsable_key_is_not_an_account() {
        let mut account = Account {
            enabled: true,
            key: "0123456789abcdef".into(),
            peer: "192.168.1.5:8410".into(),
        };
        assert!(!account.lan_ready(), "16 个字符不是那把 128-bit 密钥");
        account.key = "0123456789abcdef0123456789abcdef".into();
        assert!(account.lan_ready(), "32 个十六进制字符才是");
        account.enabled = false;
        assert!(
            !account.lan_ready(),
            "有密钥但没开：这台不参与，也不该被读到"
        );
    }

    #[test]
    fn the_account_section_round_trips_through_json() {
        let config = Config {
            account: Account {
                enabled: true,
                key: "0123456789abcdef0123456789abcdef".into(),
                peer: "192.168.1.5:8410".into(),
            },
            ..Config::default()
        };
        let text = serde_json::to_string(&config).unwrap();
        let back: Config = serde_json::from_str(&text).unwrap();
        assert_eq!(back.account, config.account);
    }
}

#[cfg(test)]
mod opds_config_tests {
    use super::{Config, Opds, OpdsCatalog};

    #[test]
    fn opds_catalogs_trim_fields_but_keep_the_password() {
        let config = Config {
            opds: Opds {
                catalogs: vec![OpdsCatalog {
                    name: "  我的书源  ".into(),
                    url: "  https://catalog.test/opds/  ".into(),
                    username: "  reader  ".into(),
                    password: "  pass with spaces  ".into(),
                }],
            },
            ..Config::default()
        }
        .normalized();
        let catalog = &config.opds.catalogs[0];
        assert_eq!(catalog.name, "我的书源");
        assert_eq!(catalog.url, "https://catalog.test/opds/");
        assert_eq!(catalog.username, "reader");
        assert_eq!(catalog.password, "  pass with spaces  ");
    }

    use super::{AliyunDrive, BaiduDrive, Cloud, WebDavDrive};

    #[test]
    fn cloud_trims_fields_but_keeps_the_secrets() {
        let config = Config {
            cloud: Cloud {
                webdav: WebDavDrive {
                    url: "  https://dav.jianguoyun.com/dav/  ".into(),
                    username: "  reader  ".into(),
                    password: "  pass with spaces  ".into(),
                    dir: "  /我的书/  ".into(),
                },
                baidu: BaiduDrive {
                    access_token: "  121.xyz789  ".into(),
                    refresh_token: "  122.r  ".into(),
                    expires_at: 0,
                    account: "  山间的读客  ".into(),
                },
                aliyun: AliyunDrive {
                    access_token: "  ali.tok  ".into(),
                    refresh_token: "  ali.r  ".into(),
                    expires_at: -5,
                    drive_id: "  1234567  ".into(),
                    account: "  阿里的读者  ".into(),
                },
            },
            ..Config::default()
        }
        .normalized();
        assert_eq!(config.cloud.webdav.url, "https://dav.jianguoyun.com/dav/");
        assert_eq!(config.cloud.webdav.username, "reader");
        assert_eq!(config.cloud.webdav.password, "  pass with spaces  ");
        assert_eq!(config.cloud.webdav.dir, "我的书");
        // token 与刷新凭据是机器签发的密文，粘贴带进来的首尾空白清掉 —— 与密码相反的一条；
        // 账号名照地址那条修剪。到期时刻 0（未知）原样过。
        assert_eq!(config.cloud.baidu.access_token, "121.xyz789");
        assert_eq!(config.cloud.baidu.refresh_token, "122.r");
        assert_eq!(config.cloud.baidu.account, "山间的读客");
        assert_eq!(config.cloud.baidu.expires_at, 0);
        // 阿里云盘同一条：token / drive_id / 账号名修剪，负的到期时刻归 0。
        assert_eq!(config.cloud.aliyun.access_token, "ali.tok");
        assert_eq!(config.cloud.aliyun.drive_id, "1234567");
        assert_eq!(config.cloud.aliyun.account, "阿里的读者");
        assert_eq!(config.cloud.aliyun.expires_at, 0);
    }

    #[test]
    fn cloud_configured_needs_an_identity() {
        let webdav = WebDavDrive::default();
        assert!(!webdav.configured());
        let webdav = WebDavDrive {
            url: "https://dav.example/".into(),
            ..WebDavDrive::default()
        };
        assert!(!webdav.configured(), "有地址没用户名不算配置好");
        assert!(
            WebDavDrive {
                url: "https://dav.example/".into(),
                username: "reader".into(),
                ..WebDavDrive::default()
            }
            .configured()
        );

        assert!(!BaiduDrive::default().configured());
        assert!(
            BaiduDrive {
                access_token: "121.abc".into(),
                ..BaiduDrive::default()
            }
            .configured()
        );

        // 阿里云盘要两样：token 与 drive_id。少一样就不算配好 —— 缺 drive_id 的
        // 请求一定被拒，让读者在这儿看见「没配好」比让他去猜 `InvalidParameter` 强。
        assert!(!AliyunDrive::default().configured());
        assert!(
            !AliyunDrive {
                access_token: "ali.tok".into(),
                ..AliyunDrive::default()
            }
            .configured(),
            "只有 token 没有 drive_id，请求必被拒"
        );
        assert!(
            AliyunDrive {
                access_token: "ali.tok".into(),
                drive_id: "123".into(),
                ..AliyunDrive::default()
            }
            .configured()
        );
    }

    #[test]
    fn a_cloud_section_defaults_in_when_the_json_lacks_it() {
        // 老配置文件没有 cloud 节：serde 缺省整节，别让它拖垮整份配置。
        let config: Config = serde_json::from_str(r#"{"version":1,"reader":{}}"#).unwrap();
        assert_eq!(config.cloud, Cloud::default());
    }
}
