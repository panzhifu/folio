//! 应用配置：软件里所有「用户能改、改完要记住」的东西都在这一个模块里。
//!
//! 分成五节，前四节各自对应设置界面里的一类，第五节是给调试用的、界面上没有：
//! - [`Reader`] —— 排版输入（字号、行距、缩进、版心、字体），排版引擎只认这一节；
//! - [`Appearance`] —— 浅深三态和两侧各用哪套主题，属于界面外观而不是排版输入；
//! - [`Library`] —— 书库目录；
//! - [`Language`] —— 界面语言；
//! - [`Logging`] —— 日志落到哪儿、说到多细（改完重启生效，界面上没有这一页）。
//!
//! 落盘位置由 [`crate::infra::paths`] 决定（`~/.config/folio/config.json`）。
//!
//! 为什么是配置文件而不是继续放 SQLite：配置是**应用级**数据、跨书存在，
//! 放成一个可读可备份的小 JSON 比塞进书库数据库更符合直觉，也方便用户手改。
//! 反过来说，一切需要「按书存」的东西（进度、标注、缓存的章节）都在 `store` 里，
//! 两边不重叠。

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::infra::error::IoResultExt as _;
use crate::infra::i18n::Locale;
use crate::infra::paths;

/// 配置文件的**格式版本**。加字段、改某个键的含义时 +1。
///
/// 为什么要有它（`plans/基础设施.md` §2.6）：格式会演进，而读者会跨版本回退 —— 降级回
/// 旧版程序读一份新版写的文件，得能认出「这份比我新」，而不是把认不出的字段当没有就完事。
/// 它只描述**文件格式**，与环境变量覆盖无关。
pub const CONFIG_VERSION: u32 = 1;

// ── §2.1 三层来源：内置默认 < 用户配置文件 < 环境变量 ────────────────────────────

/// 配置的来源层，优先级从低到高。合并粒度是**字段级**（§2.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// 内置默认（代码里的 `Default`）。
    Default,
    /// 用户的 `config.json`。
    File,
    /// 环境变量覆盖。
    Env,
}

impl Layer {
    /// 诊断 dump 里显示的名字。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::File => "file",
            Self::Env => "env",
        }
    }
}

/// 「每一项由哪一层定下来」的旁路表（§2.1 的 provenance）。
///
/// 形状是旁路表而不是给每个字段挂标签（§六待决 #9 推荐的那一种）：不侵入 `Config` 的
/// 结构，键是点分路径（`logging.level`、`reader.font_size`…）。这是「我明明改了配置怎么
/// 没生效」这类事故唯一的解药 —— 诊断入口能打印「生效值 + 来源层」。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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

    fn mark(&mut self, key: impl Into<String>, layer: Layer) {
        self.layers.insert(key.into(), layer);
    }
}

/// 忽略配置文件的应急开关（§2.1）：配置读坏了起不来时，用它空跑一次默认值。
pub const IGNORE_CONFIG_ENV: &str = "FOLIO_IGNORE_CONFIG";

/// 日志总开关（§2.1）：压过配置里的落盘与 stderr。
pub const LOG_OFF_ENV: &str = "FOLIO_LOG_OFF";

/// 环境变量覆盖层。数量按 §2.1 压到最少：数据根在 [`crate::infra::paths`] 里（那是「覆盖根」，
/// 不是配置的一项）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    fn touched_keys(&self) -> Vec<&'static str> {
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

/// 一次读盘的结果：用户层、生效值、以及每一项的来源。
///
/// **写回只写 [`Resolved::user`]**（§2.1 第 3 条）：把生效值整体烤回文件，等于把环境变量
/// 的值和默认值固化成用户的选择，之后默认值的升级就对他永远失效了。
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
fn flatten_keys(value: &serde_json::Value, prefix: &str, out: &mut BTreeSet<String>) {
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

/// 浅深怎么选。`System` 跟着操作系统走，也是默认值。
///
/// 存的是字符串而不是布尔：一个「跟随系统」的选项无法用「深色 on/off」表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    System,
    Light,
    Dark,
}

impl Mode {
    /// 配置文件与设置界面用的键名。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// 认不出的值回落 `System`：手改坏的一个键不该让界面进入未知状态。
    pub fn parse(value: &str) -> Self {
        match value {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::System,
        }
    }
}

/// 自己实现序列化而不是 derive：derive 遇到 `"mode":"anything"` 会直接报错，而报错的
/// 后果是**整节**外观退回默认值（见 [`salvage`]）—— 用户手改坏一个字符串，代价不该是
/// 浅、深两侧选过的主题名一起没了。`parse` 认不出来时自己回落 `System`，连错误都不发。
impl Serialize for Mode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Mode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Ok(Self::parse(&text))
    }
}

/// 目录每一行的松紧。
///
/// 两档的行高是在真机上量出来的（紧凑 38dp / 宽松 46dp，只有后者过了 Android 那颗
/// 48dp 的触控下限），所以这里是**枚举而不是一个行高数**：读者选的是「挤一点还是
/// 松一点」，不该让他去调一个自己看不出意义的浮点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TocDensity {
    /// 一屏多看十几条，代价是指尖要按得更准。
    Compact,
    /// 拇指能稳定命中的那一档。默认它。
    #[default]
    Comfortable,
}

impl TocDensity {
    /// 一行的行高（逻辑像素）。
    pub fn row_height(self) -> f32 {
        match self {
            Self::Compact => 38.0,
            Self::Comfortable => 46.0,
        }
    }

    /// 配置文件与设置界面用的键名。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Comfortable => "comfortable",
        }
    }

    /// 认不出的值回落 `Comfortable`：手改坏一个键不该让整节偏好退回默认。
    pub fn parse(value: &str) -> Self {
        match value {
            "compact" => Self::Compact,
            _ => Self::Comfortable,
        }
    }
}

/// 自己实现序列化，理由与 [`Mode`] 那条一样：derive 碰到认不出的字符串会报错，
/// 而报错的代价是**整节** `Reader` 退回默认（见 [`salvage`]）—— 字号、行距、字体家族
/// 会跟着一起没，而那只是目录的一档写坏了。
impl Serialize for TocDensity {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for TocDensity {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Ok(Self::parse(&text))
    }
}

/// 界面外观：浅深怎么选，以及解析到浅、深时各用哪套主题。
///
/// 主题**名字**存在这里，主题**本身**不存：配色来自 UI 框架的主题集和用户
/// 自己放进 `themes/` 的 JSON（见 `folio_app::theme`）。所以这一节只回答
/// 「选哪套」，`None` 表示用框架自带的那套。两侧都要记，即使当前模式只用到一侧 ——
/// 之后换模式时读者的选择还在。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub mode: Mode,
    /// 解析到浅色时用的主题名。
    pub light: Option<String>,
    /// 解析到深色时用的主题名。
    pub dark: Option<String>,
}

impl Appearance {
    /// 空串按「没设」处理：`""` 既不是合法主题名，又会让下拉框显示成空白。
    pub fn normalized(mut self) -> Self {
        for name in [&mut self.light, &mut self.dark] {
            *name = name
                .take()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
        }
        self
    }
}

/// 排版输入。全部是纯数据、不含任何 UI 类型，因此同一份配置既能在桌面端用，
/// 也能给移动端宿主用。
///
/// 它是排版失效键的一格（`layout::LayoutKey::prefs_hash`），那里按**序列化后的整份**
/// 取哈希而不是手挑字段 —— 手挑会漏，漏的那个字段改了不触发重排，而屏幕上看不出来。
/// 所以这里不 derive `Hash`：derive 出来的实现要 `f32: Hash`，只能把浮点改成按位哈希，
/// 那就又回到一张需要人肉维护的清单。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reader {
    pub font_size: f32,
    pub line_height: f32,
    pub paragraph_gap_em: f32,
    pub indent_em: f32,
    /// 正文栏最大宽度（像素）。**`0` 表示不设上限、随窗口铺满**，
    /// 所以拿到 `0` 时**不能**当成「宽度上限是 0」去夹任何东西。
    pub max_measure: f32,
    /// 正文栏两侧留白（像素），左右各一份。
    ///
    /// 定在 24 是按手机量的：那块屏一般 360–400 逻辑像素宽，两侧各 48 就吃掉 25%，
    /// 一行只剩十几个字，读起来像窄栏报纸；24 留 12% 上下，边到字仍够拇指按不到。
    /// 桌面上这一条只是最小留白，行长由 `max_measure` 管，所以这个数变小不影响宽窗口。
    pub margin_x: f32,
    pub justify: bool,
    pub font_family: String,
    pub mono_font_family: String,
    /// 目录那一屏的行高档位。只影响目录，不影响正文 —— 正文的松紧是 `line_height`。
    pub toc_density: TocDensity,
    /// 目录标题最多排几行，超了截成省略号。
    ///
    /// 默认 2 而不是 1：书名里「第 3.2 节 论……」这类长标题在手机上会占两行，
    /// 只给一行的话目录会退化成一片省略号，读者没法靠三个点认路。
    pub toc_max_title_lines: u8,
    /// 到过的章在目录里画成浅色。关掉它，目录就只剩「当前在哪」一个信号。
    pub toc_dim_read: bool,
}

impl Default for Reader {
    fn default() -> Self {
        Self {
            font_size: 17.0,
            line_height: 1.75,
            paragraph_gap_em: 0.75,
            indent_em: 2.0,
            // 0 = 随窗口铺满（全屏时正文栏跟着变宽）。
            max_measure: 0.0,
            margin_x: 24.0,
            justify: true,
            font_family: "LXGW WenKai".into(),
            mono_font_family: "monospace".into(),
            toc_density: TocDensity::default(),
            toc_max_title_lines: 2,
            toc_dim_read: true,
        }
    }
}

impl Reader {
    /// 从磁盘读回来的值不可信（手改过、旧版本、别的机器同步来的），
    /// 越界值会让排版算出零高或无限宽，所以入库前一律夹住。
    pub fn clamped(mut self) -> Self {
        self.font_size = self.font_size.clamp(9.0, 72.0);
        self.line_height = self.line_height.clamp(1.0, 4.0);
        self.paragraph_gap_em = self.paragraph_gap_em.clamp(0.0, 4.0);
        self.indent_em = self.indent_em.clamp(0.0, 6.0);
        // 0 是合法的「铺满」，所以下界就是 0。
        self.max_measure = self.max_measure.clamp(0.0, 4000.0);
        self.margin_x = self.margin_x.clamp(0.0, 400.0);
        if self.font_family.trim().is_empty() {
            self.font_family = Reader::default().font_family;
        }
        // 0 行是「什么都不画」，4 行会让一条目录占掉小半屏 —— 两头的错法都收进这一格。
        self.toc_max_title_lines = self.toc_max_title_lines.clamp(1, 3);
        self
    }
}

/// 书库：导入的书**复制**到哪儿。
///
/// 只有一个目录，而且必须由读者确认过才算数 —— 空串就是「还没确认」，导入要拦住。
/// 理由是导入不再是引用原文件而是往里拷一份：不先说清楚拷到哪儿，读者只会发现
/// 硬盘上多了一批自己没打算要的文件。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Library {
    /// 确认过的书库目录。空串 = 没确认过。
    pub dir: String,
    /// 文件夹监听：`Some` = 开着。盯一个「收件箱」目录，新出现的书自动入库
    /// （`folio_core::ingest::watch`）。监听书库目录自己没有意义 —— 导进来的就是从
    /// 那里拷出去的，会自我循环，normalize 把它当没开。
    pub watch: Option<WatchDir>,
}

/// 监听的目标与节奏。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WatchDir {
    /// 盯住的目录。
    pub dir: String,
    /// 兜底轮询间隔（秒）。事件驱动（inotify）的平台上不起作用，只在
    /// 没有系统级监听 API 的平台上是真实的节奏。
    pub interval_secs: u32,
}

impl Default for WatchDir {
    fn default() -> Self {
        Self {
            dir: String::new(),
            interval_secs: 30,
        }
    }
}

impl Library {
    /// 位置确认过了没有。认过之后导入才放行。
    pub fn is_confirmed(&self) -> bool {
        !self.dir.trim().is_empty()
    }
}

/// 界面语言。换设备同步配置时它应当跟着走，而它不属于任何一本书，
/// 也不影响书的正文语言，所以单独一节。
///
/// 默认是简体中文（[`Locale`] 的 `Default`）：这个项目的读者以中文书为第一目标；
/// 之后读回来的是用户选过的那个。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Language {
    pub locale: Locale,
}

/// 日志：调试时要不要落盘、写到哪儿、每一支代码说到多细。
///
/// 这一节存在理由是**不改代码就能调级**：真机上出问题往往是「这次什么都不显示」，
/// 而重新编一遍 Android 是十几分钟加一次签名重装。手改 `config.json` 里的一行、重启，
/// 就拿到现场了。（要连依赖库一起打开，仍然有 `FOLIO_LOG` 那条环境变量，见 [`crate::infra::logging`]。）
///
/// 默认值分平台：桌面 stderr 本来就开着，而移动平台上没人对着控制台。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Logging {
    /// folio 自己的代码（`folio_core` / `folio_app`）说到多细：
    /// `off` `error` `warn` `info` `debug` `trace`。**依赖库不跟着走**，它们固定在
    /// `warn` 以上 —— 否则「打开 debug」等于把整个 GPU 栈的日志一起打开，两秒就写满一份文件。
    /// 认不出的值回落 `info`，不是整档关掉。
    pub level: String,
    /// 按模块覆盖，写法同 `RUST_LOG` 的逗号分隔那一段：`"folio_core::layout=trace"`。
    /// 前缀要**整段对上模块边界**，最深的那一条赢。
    pub targets: Vec<String>,
    /// 落盘到 `<数据根>/logs/folio.log`（或 [`Self::dir`] 指的地点）。
    pub file: bool,
    /// 同时写 stderr。桌面默认开（是终端才带颜色），移动平台默认关。
    pub stderr: bool,
    /// 日志目录。空串 = 用默认的 `<数据根>/logs`。
    ///
    /// 单独给一格是因为设备上的数据根常常拿不到：Android 上把它指到
    /// `/storage/emulated/0/Android/data/dev.folio.reader/files` 那一类位置，
    /// `adb pull` 直接就能取走。
    pub dir: String,
    /// 一次运行最多写多少字节，写满就停手（`0` = 不限）。
    ///
    /// 上限是「本轮」的，不是文件的：上一次运行那份在开机时被挪成 `.1`，
    /// 所以磁盘上总量是这个数乘以 [`Self::keep`] 加一。
    pub max_bytes: u64,
    /// 保留几份历史运行（`.1`、`.2`……）。`0` 就只留当前这一份。
    pub keep: usize,
}

impl Default for Logging {
    fn default() -> Self {
        Self {
            // debug 构建默认开到 debug：本地跑一次就该看见排版与容器里发生了什么。
            // release 默认 info：只留生命周期与「静默降级」那几条，不刷正文。
            level: if cfg!(debug_assertions) {
                "debug"
            } else {
                "info"
            }
            .into(),
            targets: Vec::new(),
            file: true,
            stderr: !cfg!(any(
                target_os = "android",
                target_os = "ios",
                target_env = "ohos"
            )),
            dir: String::new(),
            // 4 MiB 一份：一次长会话把 trace 全开也够用，而 `adb pull` 不用等。
            max_bytes: 4 * 1024 * 1024,
            keep: 3,
        }
    }
}

impl Logging {
    /// 去掉各处的手滑空白。`level` 的**取值**不在这里判：认不出是 [`crate::infra::logging`]
    /// 装配过滤器时的事，那一处已经有「回落 info 并在 stderr 说一句」的口径，
    /// 在这里再判一遍就成了两份会各自漂移的规则。
    pub fn normalized(mut self) -> Self {
        self.level = self.level.trim().to_ascii_lowercase();
        self.dir = self.dir.trim().to_string();
        self.targets.retain(|piece| !piece.trim().is_empty());
        self.targets
            .iter_mut()
            .for_each(|piece| *piece = piece.trim().to_string());
        self
    }
}

/// 一份完整的应用配置。
///
/// 只 `PartialEq`：`Reader` 里是浮点数，`Eq` 在 f32 上本来就不成立。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 这份配置的格式版本（[`CONFIG_VERSION`]）。老文件里没有这一格 → serde 缺省成 `0`，
    /// 于是 `load` 能认出「这是没版本戳的老文件」并迁移（见 [`Config::migrated`]）。
    pub version: u32,
    pub reader: Reader,
    pub appearance: Appearance,
    pub library: Library,
    pub language: Language,
    pub logging: Logging,
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
            logging: Logging::default(),
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
        self
    }

    /// 读默认位置的**生效值**。文件不存在或内容坏了都退回默认值 —— 配置绝不该让程序起不来。
    pub fn load() -> Config {
        Self::load_resolved().effective
    }

    /// 读默认位置，并保留用户层与 provenance（§2.1）。入口用它，好知道「落盘该写哪一层」。
    pub fn load_resolved() -> Resolved {
        Self::resolve(&paths::config_path(), &Env::from_env())
    }

    /// 读一次配置：内置默认 ← 用户文件 ← 环境变量，**字段级**合并，并记下每一项的来源。
    ///
    /// 只读一次磁盘（§2.3）；环境变量是叠在已读进来的文件之上的，运行期不再重读。
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
                    config.migrated().normalized()
                } else {
                    tracing::warn!("config: {} 整份解不开，按节抢救", path.display());
                    salvage(value).migrated().normalized()
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

    /// 按已有签名读一份**生效值**（不带环境变量覆盖）—— 测试与旧调用方用。
    pub fn load_from(path: &Path) -> Config {
        Self::resolve(path, &Env::default()).effective
    }

    /// 读文件并解成 `Value`；读不到 / 不是合法 JSON 都给 `None`（各留一句日志）。
    fn read_value(path: &Path) -> Option<serde_json::Value> {
        let Ok(text) = std::fs::read_to_string(path) else {
            // 「没有配置文件」与「有但读不进去」是两件事，日志里要分得开：
            // 前者是新机器，后者是权限或路径出了问题。
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

    /// 把版本戳对齐到本程序：老文件（没有 `version`，serde 缺省成 `0`）迁到当前版本；
    /// 比本程序**新**的按认得的部分读、并说一声。**读到旧版本 = 迁移，不是崩溃，
    /// 也不是静默当新版用**（§2.6 / §5.1）。
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

    /// 写默认位置的配置。
    pub fn save(&self) -> crate::Result<()> {
        self.save_to(&paths::config_path())
    }

    /// 写文件：先建父目录，写临时文件再 rename，避免写到一半崩掉留下半份坏 JSON。
    ///
    /// 序列化失败时**什么都不要写**。原来这里 `unwrap_or_else(|_| "{}")`：一次编码失败
    /// 会把读者全部偏好换成一坨空配置落盘，而那正是下一次启动读进去的东西 ——
    /// 配置编码不出问题是「不可能的」，真出了就报错，别拿写坏文件当兜底。
    pub fn save_to(&self, path: &Path) -> crate::Result<()> {
        paths::prepare_dir(path)?;
        let json = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &json).io_path(&tmp)?;
        std::fs::rename(&tmp, path).io_path(path)
    }
}

/// 整份读不出来时的补救：**按节**退回默认值。
///
/// 为什么需要它：`serde_json::from_str::<Config>` 是一根绳上的几个节，任何一个节的
/// 任何一个字段类型不对（`"font_size":"big"`、`"mode":3`、手滑多打的一个引号），
/// 整个调用就返回 `Err`，于是全部配置归零 —— 读者看到的是字号、主题、书库目录、
/// 界面语言一起消失，而且完全不知道原因。分节解析后，坏的那一节归零，其余各节照旧。
///
/// 只处理「JSON 本身合法、某一节的内容不合结构」这一类；连 JSON 都解析不了
/// （截断、少括号）时没有可逐节尝试的载体，仍然整份退回默认值（那一步在 [`Config::load_from`]）。
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
    if let Some(logging) = section(value, "logging") {
        config.logging = logging;
    }
    config
}

/// 有文件、但缺了某些节：各节照常用默认值，但**留一条 WARN**（§2.6）——静默会让
/// 「我的设置没保存住」变成无法诊断的问题。只在文件已经存在时才会走到这儿，
/// 新机器（读不到文件）不会因此报一句。
fn warn_missing_sections(value: &serde_json::Value) {
    let Some(object) = value.as_object() else {
        return;
    };
    const SECTIONS: [&str; 5] = ["reader", "appearance", "library", "language", "logging"];
    let missing: Vec<&str> = SECTIONS
        .into_iter()
        .filter(|key| !object.contains_key(*key))
        .collect();
    if !missing.is_empty() {
        tracing::warn!("config: 缺这些节，各用默认值：{missing:?}");
    }
}

/// 取一节。缺这一节、或者这一节自己解不出来，都返回 `None` 让调用方保留默认值 ——
/// 具体类型由赋值目标推断，所以每个节的 `#[serde(default)]`（少键时用**本节**的默认）
/// 在这里同样成立。
fn section<T: serde::de::DeserializeOwned>(value: &serde_json::Value, key: &str) -> Option<T> {
    value
        .get(key)
        .and_then(|raw| serde_json::from_value::<T>(raw.clone()).ok())
}

#[cfg(test)]
mod tests {
    //! 应用配置：分节、JSON 往返、缺文件/坏文件回落、读入夹取、语言键。只依赖公开 API。
    //!
    //! 这些测试盯的是「用户手改这个 JSON 之后程序会怎样」，所以全部从 JSON 文本出发，
    //! 不去直接改结构体 —— 结构体改不出坏输入。

    use std::path::Path;

    use crate::infra::config::{Config, Env, Layer, WatchDir};
    use crate::infra::i18n::Locale;
    use crate::{Appearance, Language, Library, Mode, Reader};
    use serde_json::json;

    fn load(text: &str) -> Config {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, text).unwrap();
        Config::load_from(&path)
    }

    /// 老文件没有版本戳（serde 缺省成 0）：按当前版本读，字段照旧，版本对齐到当前。
    /// §2.6：读到旧版本是**迁移**，不是崩溃。
    #[test]
    fn a_versionless_file_is_read_and_realigned_to_the_current_version() {
        let got = load(r#"{"reader":{"font_size":21.0}}"#);
        assert_eq!(got.reader.font_size, 21.0, "老文件的字段照旧读出来");
        assert_eq!(got.version, crate::infra::config::CONFIG_VERSION);
    }

    /// 比本程序**新**的文件：认得的部分照读、未知字段忽略，版本对齐到本程序
    /// （不是崩溃，也不是静默当新版用 —— 有一条 WARN）。
    #[test]
    fn a_newer_file_version_is_read_not_rejected() {
        let got = load(r#"{"version":999,"reader":{"font_size":21.0},"future_thing":true}"#);
        assert_eq!(got.reader.font_size, 21.0, "认得的字段要留下");
        assert_eq!(got.version, crate::infra::config::CONFIG_VERSION);
    }

    /// 写出去再读回来，版本戳要保住（往返相等已覆盖，这里把「版本」单点钉一下）。
    #[test]
    fn the_version_is_written_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        Config::default().save_to(&path).unwrap();
        assert_eq!(
            Config::load_from(&path).version,
            crate::infra::config::CONFIG_VERSION
        );
    }

    /// §2.1：文件里写了的项记成 `File`，没写的是 `Default`。
    #[test]
    fn provenance_tells_file_from_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"reader":{"font_size":21.0}}"#).unwrap();

        let resolved = Config::resolve(&path, &Env::default());
        assert_eq!(
            resolved.provenance.layer_of("reader.font_size"),
            Layer::File
        );
        assert_eq!(
            resolved.provenance.layer_of("reader.line_height"),
            Layer::Default,
            "文件里没写的项是内置默认"
        );
        assert_eq!(resolved.effective.reader.font_size, 21.0);
    }

    /// §2.1：环境变量只盖它负责的键、并记成 `Env`；**用户层不动**——落盘写的是用户层，
    /// 所以环境变量的值不会被烤回文件。
    #[test]
    fn env_overrides_do_not_leak_into_the_user_layer() {
        let env = Env {
            ignore_file: false,
            log_off: true,
            log: None,
        };
        let resolved = Config::resolve(Path::new("/nonexistent/folio.json"), &env);
        assert!(!resolved.effective.logging.file, "环境变量把日志关掉了");
        assert!(
            resolved.user.logging.file,
            "用户层没被环境变量改到（它才是落盘内容）"
        );
        assert_eq!(resolved.provenance.layer_of("logging.file"), Layer::Env);
    }

    /// §2.1：`FOLIO_LOG` 压着 logging 那两格 —— provenance 必须说实话。它不在 `resolve`
    /// 里盖值（RUST_LOG 写法落不进 `logging.level`），但「这两格归环境变量说了算」要标出来，
    /// 否则读者会以为 `logging.level` 的「生效值」就是最终生效的。
    #[test]
    fn the_log_env_marks_the_logging_keys_as_env() {
        let env = Env {
            ignore_file: false,
            log_off: false,
            log: Some("debug,wgpu_core=error".into()),
        };
        let resolved = Config::resolve(Path::new("/nonexistent/folio.json"), &env);
        assert_eq!(resolved.provenance.layer_of("logging.level"), Layer::Env);
        assert_eq!(resolved.provenance.layer_of("logging.targets"), Layer::Env);
        assert_eq!(
            resolved.provenance.layer_of("reader.font_size"),
            Layer::Default,
            "只管日志那两格，不碰别的"
        );
    }

    /// §2.1：忽略配置文件的应急开关跳过文件层，全用默认 + 环境变量。
    #[test]
    fn the_emergency_switch_skips_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"reader":{"font_size":40.0}}"#).unwrap();

        let env = Env {
            ignore_file: true,
            log_off: false,
            log: None,
        };
        let resolved = Config::resolve(&path, &env);
        assert_eq!(
            resolved.effective.reader.font_size,
            Reader::default().font_size,
            "文件被跳过"
        );
        assert_eq!(
            resolved.provenance.layer_of("reader.font_size"),
            Layer::Default
        );
    }

    #[test]
    fn config_roundtrips_through_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folio/config.json");

        let config = Config {
            reader: Reader {
                font_size: 21.0,
                ..Default::default()
            },
            appearance: Appearance {
                mode: Mode::Light,
                light: Some("Gruvbox".into()),
                dark: Some("Molokai".into()),
            },
            language: Language { locale: Locale::Ja },
            ..Default::default()
        };
        config.save_to(&path).unwrap();

        let got = Config::load_from(&path);
        assert_eq!(got, config, "写出去再读回来必须逐字段相等");
    }

    #[test]
    fn a_missing_file_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let got = Config::load_from(&dir.path().join("does-not-exist.json"));
        assert_eq!(got, Config::default());
    }

    #[test]
    fn a_corrupt_file_yields_defaults_instead_of_failing() {
        assert_eq!(load("{ not json at all"), Config::default());
        assert_eq!(load(""), Config::default());
    }

    /// 缺哪个键就用哪个键的默认值：加一节不该让老配置文件读不出来。
    #[test]
    fn missing_sections_and_keys_fall_back_per_key() {
        let got = load(r#"{"reader":{"font_size":21.0},"appearance":{"mode":"dark"}}"#);
        assert_eq!(got.reader.font_size, 21.0);
        assert_eq!(got.appearance.mode, Mode::Dark);
        assert_eq!(
            got.appearance.light, None,
            "没选过主题名就是用框架自带的那套，不是空串"
        );
        assert_eq!(got.reader.line_height, 1.75);
        assert!(
            got.library.dir.is_empty(),
            "没确认过书库位置就是空串，不是某个默认目录"
        );
    }

    /// 老版本的 `reader` 一节里有 `theme`；新版在 `appearance`。
    /// 认不出来的字段 serde 会直接忽略，于是主题回到框架默认而不是报错。
    #[test]
    fn a_stale_theme_key_is_ignored_instead_of_failing() {
        let got = load(r#"{"reader":{"theme":"Folio Sepia"}}"#);
        assert_eq!(got.appearance.light, None);
        assert_eq!(got.appearance.mode, Mode::System);
    }

    /// 老版本的 `reader` 一节里有 `locale`；新版在 language 一节。
    /// 认不出来的字段 serde 会直接忽略，于是语言回到默认（简体中文）而不是报错。
    #[test]
    fn an_unknown_locale_key_falls_back_to_the_default_language() {
        let got = load(r#"{"reader":{"locale":"fr"}}"#);
        assert_eq!(got.language.locale, Locale::ZhHans);
    }

    /// `language` 一节缺 locale 键时用的是缺省语言（简体中文），所以这里显式挑一个
    /// 与缺省不同的值，让「读到了」和「回落了」不可能同值。
    ///
    /// JSON 用 `json!` 现搭，不手写转义花括号：这一条测试曾经因为多打的一个引号
    /// 变成了「整份 JSON 无效」，于是要验的 locale 从没被解过一遍。
    #[test]
    fn the_locale_survives_the_json_round_trip() {
        let different = Locale::En;

        // 界面上看到的就是 `code()`，读者照它手改，所以这一种写法是主路径。
        let got = load(&json!({ "language": { "locale": different.code() } }).to_string());
        assert_eq!(got.language.locale, different);

        // 老配置（derive serde 时代）里存的是变体名。这条同时钉住另一件事：
        // 认不出来时只该坏掉 language 一节，`font_size` 必须还在 —— 如果 locale
        // 让整份解析失败，退回默认的就是 17.0。
        let legacy = load(
            &json!({
                "language": { "locale": format!("{different:?}") },
                "reader": { "font_size": 21.0 },
            })
            .to_string(),
        );
        assert_eq!(
            legacy.language.locale, different,
            "derive 时代写出去的变体名必须继续认得"
        );
        assert_eq!(legacy.reader.font_size, 21.0);
    }

    /// 一处类型写错的字段只该毁掉**它自己那一节**：读者手改 `"font_size":"big"` 之后，
    /// 不该看到主题名、书库目录一起消失，而且没有任何提示。
    #[test]
    fn one_broken_section_does_not_take_the_others_down() {
        let got = load(
            r#"{"reader":{"font_size":"big","line_height":2.5},
            "appearance":{"mode":"dark","light":"Gruvbox"},
            "library":{"dir":"~/Books"},
            "language":{"locale":"ja"}}"#,
        );
        assert_eq!(
            got.reader,
            Reader::default(),
            "坏的是 reader 一节，就只有这一节退回默认"
        );
        assert_eq!(got.appearance.mode, Mode::Dark);
        assert_eq!(got.appearance.light.as_deref(), Some("Gruvbox"));
        assert_eq!(got.library.dir, "~/Books");
        assert_eq!(got.language.locale, Locale::Ja);
    }

    /// 补救的粒度是「一节」而不是「一个键」：`line_height` 没写错，但它的同节邻居错了，
    /// 于是整节一起退回。serde 的字段级默认只在整节解成功时才有机会生效，
    /// 这里把它明确成预期行为，而不是让人以为是 bug。
    #[test]
    fn the_good_keys_in_a_broken_section_fall_back_too() {
        let got = load(r#"{"reader":{"font_size":"big","line_height":2.5}}"#);
        assert_eq!(got.reader.line_height, Reader::default().line_height);
    }

    /// 连 JSON 本身都解析不了（截断、括号不配对）时没有「节」可以逐节尝试，
    /// 仍然整份退回默认值。
    #[test]
    fn unparseable_json_still_falls_back_as_a_whole() {
        assert_eq!(load(r#"{"reader":{"font_size":21.0}"#), Config::default());
    }

    /// 手改过的越界值在读入时要被夹住（否则排版会算出零高或无限宽）。
    #[test]
    fn out_of_range_values_are_clamped_on_load() {
        let got = load(
            r#"{"reader":{"font_size":9000,"line_height":0,"indent_em":-5,"margin_x":1e9},"language":{"locale":"ja"}}"#,
        );
        assert_eq!(got.reader.font_size, 72.0);
        assert_eq!(got.reader.line_height, 1.0);
        assert_eq!(got.reader.indent_em, 0.0);
        assert_eq!(got.reader.margin_x, 400.0);
        assert_eq!(
            got.language.locale,
            Locale::Ja,
            "夹取只管排版数值，不碰别的节"
        );
    }

    /// 空白主题名被当成「没设」；0 的 max_measure 是合法的铺满，不能被夹成别的值。
    #[test]
    fn blank_theme_names_are_dropped_and_fill_width_survives_clamping() {
        let got =
            load(r#"{"reader":{"max_measure":0},"appearance":{"light":"   ","dark":"Molokai"}}"#);
        assert_eq!(
            got.appearance.light, None,
            "全空白的主题名不该存成一个显示不出来的选项"
        );
        assert_eq!(got.appearance.dark.as_deref(), Some("Molokai"));
        assert_eq!(got.reader.max_measure, 0.0, "0 = 随窗口铺满");
    }

    /// 认不出的 mode 字符串不该让配置读失败或直接变成深色。
    #[test]
    fn an_unknown_mode_falls_back_to_following_the_system() {
        assert_eq!(
            load(r#"{"appearance":{"mode":"nonsense"}}"#)
                .appearance
                .mode,
            Mode::System
        );
        assert_eq!(Mode::parse("dark"), Mode::Dark);
        assert_eq!(Mode::default(), Mode::System);
    }

    #[test]
    fn an_empty_font_family_falls_back_instead_of_nuking_the_text() {
        let got = load(r#"{"reader":{"font_family":"   "}}"#);
        assert_eq!(got.reader.font_family, Reader::default().font_family);
    }

    /// 监听目录为空、或就是书库目录自己：前者没得盯、后者会自我循环
    /// （书库里出现的书就是刚从那里拷进来的），normalize 都当没开。
    #[test]
    fn the_watch_section_needs_a_distinct_directory() {
        let config = Config {
            library: Library {
                dir: "/books".into(),
                watch: Some(WatchDir {
                    dir: "/books".into(),
                    interval_secs: 30,
                }),
            },
            ..Default::default()
        };
        assert!(config.normalized().library.watch.is_none());

        let config = Config {
            library: Library {
                dir: "/books".into(),
                watch: Some(WatchDir {
                    dir: "  ".into(),
                    interval_secs: 30,
                }),
            },
            ..Default::default()
        };
        assert!(config.normalized().library.watch.is_none());

        // 节奏越界夹回界内；正经目录原样保留。
        let config = Config {
            library: Library {
                dir: "/books".into(),
                watch: Some(WatchDir {
                    dir: " /inbox ".into(),
                    interval_secs: 1,
                }),
            },
            ..Default::default()
        };
        let watch = config.normalized().library.watch.expect("正经目录该留着");
        assert_eq!(watch.dir, "/inbox");
        assert_eq!(watch.interval_secs, 5);
    }

    #[test]
    fn the_library_section_holds_its_dir() {
        let config = Config {
            library: Library {
                dir: "~/Books".into(),
                watch: None,
            },
            ..Default::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        config.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path).library.dir, config.library.dir);
    }

    /// 写文件是「临时文件 + rename」：中途失败不该留下半份坏 JSON。
    /// 这里能验证的部分是目标目录不存在时会自动建出来。
    #[test]
    fn saving_creates_the_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep/nested/config.json");
        Config::default().save_to(&path).unwrap();
        assert!(path.exists());
    }
}
