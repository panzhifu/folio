use super::Config;
use std::collections::{BTreeMap, BTreeSet};
/// 配置的来源层，优先级从低到高。合并粒度是**字段级**（§2.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// 内置默认（代码里的 `Default`）。
    Default,
    /// 用户的 `config.json`。
    File,
    /// 环境变量覆盖。
    Env,
    /// 当前挂着的那本书自己的差量（[`BookSettings`](crate::BookSettings)），最高一档。
    ///
    /// 它必须在这一表里有名字：诊断 dump 打的是**生效值**，而「我在设置屏改了字号怎么
    /// 这本书没变」正是这张表存在的理由 —— 少了这一档，那一格会被报成来自 `file`，
    /// 而那恰好是错的。
    Book,
}

impl Layer {
    /// 诊断 dump 里显示的名字。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::File => "file",
            Self::Env => "env",
            Self::Book => "book",
        }
    }
}

/// 按点分键记录配置来源；不侵入持久化 Config。
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Provenance {
    layers: BTreeMap<String, Layer>,
}

impl Provenance {
    /// 这一项由哪一层定下来；没记到的按 [`Layer::Default`]（内置默认）。
    pub fn layer_of(&self, key: &str) -> Layer {
        self.layers.get(key).copied().unwrap_or(Layer::Default)
    }

    /// 遍历（按点分键排序）。
    pub fn entries(&self) -> impl Iterator<Item = (&str, Layer)> {
        self.layers
            .iter()
            .map(|(key, layer)| (key.as_str(), *layer))
    }

    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    /// 记一项来自哪一层。
    ///
    /// 公开是因为**第四层住在 UI 那边**：一本书的差量是开书那一刻由阅读会话叠上去的
    /// （见 `folio_app::ui::services::ConfigHolder`），core 的读盘那一路看不见它。
    /// 键仍是点分路径，与诊断 dump 里那份生效值 JSON 对得上。
    pub fn mark(&mut self, key: impl Into<String>, layer: Layer) {
        self.layers.insert(key.into(), layer);
    }
}

/// 忽略配置文件的应急开关（§2.1）：配置读坏了起不来时，用它空跑一次默认值。
pub const IGNORE_CONFIG_ENV: &str = "FOLIO_IGNORE_CONFIG";

/// 日志总开关（§2.1）：压过配置里的落盘与 stderr。
pub const LOG_OFF_ENV: &str = "FOLIO_LOG_OFF";

/// 环境覆盖；FOLIO_LOG 保留给过滤器使用，不能原样写入诊断。
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Env {
    /// [`IGNORE_CONFIG_ENV`]：跳过用户配置文件。
    pub ignore_file: bool,
    /// [`LOG_OFF_ENV`]：日志总开关。
    pub log_off: bool,
    /// `FOLIO_LOG`（[`crate::infra::logging::LEVEL_ENV`]）的原文，没设就是 `None`。
    ///
    /// 它照 `RUST_LOG` 的写法解释，**还能连依赖库一起开**，落不进 `logging.level` 那一格
    /// （那一格只管 folio 自己），所以这里只留原文、供 provenance 记「它压着哪几格」；
    /// 真正生效的地方是 [`crate::infra::logging`]。
    pub log: Option<String>,
}

impl Env {
    /// 从进程环境读一份。
    pub fn from_env() -> Self {
        Self {
            ignore_file: env_flag(std::env::var_os(IGNORE_CONFIG_ENV)),
            log_off: env_flag(std::env::var_os(LOG_OFF_ENV)),
            log: std::env::var(crate::infra::logging::LEVEL_ENV)
                .ok()
                .filter(|spec| !spec.trim().is_empty()),
        }
    }

    /// 把这一层盖到配置上（就地改）。**字段级**：只碰它负责的那几格。
    ///
    /// `FOLIO_LOG` 不在这里盖值：它按 `RUST_LOG` 写法解释、还能连依赖库一起开，落不进
    /// `logging.level`（只管 folio 自己）那一格 —— 它真正生效的地方是 [`crate::infra::logging`]。
    /// 这里只在 provenance 上记「它压着 `logging.level` / `logging.targets`」。
    pub fn apply(&self, config: &mut Config) {
        if self.log_off {
            config.logging.file = false;
            config.logging.stderr = false;
        }
    }

    /// 这一层改了哪些键（给 provenance 用）。
    pub(super) fn touched_keys(&self) -> Vec<&'static str> {
        let mut keys = Vec::new();
        if self.log_off {
            keys.push("logging.file");
            keys.push("logging.stderr");
        }
        if self.log.is_some() {
            keys.push("logging.level");
            keys.push("logging.targets");
        }
        keys
    }
}

/// 非空且不为 `0` 即为开（与 [`crate::infra::diagnostics`] 那条开关同口径）。
pub(crate) fn env_flag(value: Option<std::ffi::OsString>) -> bool {
    value.is_some_and(|raw| {
        let text = raw.to_string_lossy();
        let text = text.trim();
        !text.is_empty() && text != "0"
    })
}

/// 配置读取快照。写回只用 user，effective 仅供运行期读取。
#[derive(Debug, Clone)]
pub struct Resolved {
    /// 用户层：内置默认 ⊕ 用户文件（没叠环境变量）。**落盘写的是它。**
    pub user: Config,
    /// 生效值：用户层再叠环境变量。界面读的是它。
    pub effective: Config,
    /// 每一项由哪一层定下来。
    pub provenance: Provenance,
}

/// 把 JSON 摊成点分键，用来判「文件里到底写了哪几项」（provenance 的 File 一档）。
pub(super) fn flatten_keys(value: &serde_json::Value, prefix: &str, out: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten_keys(child, &path, out);
            }
        }
        _ => {
            if !prefix.is_empty() {
                out.insert(prefix.to_string());
            }
        }
    }
}
