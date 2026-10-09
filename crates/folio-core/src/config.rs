//! 应用配置：软件里所有「用户能改、改完要记住」的东西都在这一个模块里。
//!
//! 分成五节，每节对应设置界面里的一类：
//! - [`Reader`] —— 排版输入（字号、行距、缩进、版心、字体），排版引擎只认这一节；
//! - [`Appearance`] —— 浅深三态和两侧各用哪套主题，属于界面外观而不是排版输入；
//! - [`Library`] —— 书库目录；
//! - [`Language`] —— 界面语言；
//! - [`Account`] —— 本地占位账号。
//!
//! 落盘位置由 [`crate::paths`] 决定（`~/.config/folio/config.json`）。
//!
//! 为什么是配置文件而不是继续放 SQLite：配置是**应用级**数据、跨书存在，
//! 放成一个可读可备份的小 JSON 比塞进书库数据库更符合直觉，也方便用户手改。
//! 反过来说，一切需要「按书存」的东西（进度、标注、缓存的章节）都在 `store` 里，
//! 两边不重叠。

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::i18n::Locale;
use crate::paths;

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
    pub margin_x: f32,
    pub justify: bool,
    pub font_family: String,
    pub mono_font_family: String,
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
            margin_x: 48.0,
            justify: true,
            font_family: "LXGW WenKai".into(),
            mono_font_family: "monospace".into(),
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
        self
    }
}

/// 书库：读者放在哪里的书算「本地藏书」。
///
/// 目前只有目录列表 —— 真正的按目录浏览要等 M5 的书架视图，
/// 但这一节先占好位置，配置文件的形状就不用再改了。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Library {
    pub dirs: Vec<String>,
}

/// 界面语言。换设备同步配置时它应当跟着走，而它不属于任何一本书，
/// 也不影响书的正文语言，所以单独一节。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Language {
    #[serde(default = "Locale::detect")]
    pub locale: Locale,
}

impl Default for Language {
    fn default() -> Self {
        // 首次启动跟随系统；之后读回来的是用户选过的那个。
        Self {
            locale: Locale::detect(),
        }
    }
}

/// 占位账号：目前只有一个可改的读者名，将来接真正的账号/同步时再扩展。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Account {
    /// 空串表示没设过，界面用兜底名显示。
    pub name: String,
}

/// 一份完整的应用配置。
///
/// 只 `PartialEq`：`Reader` 里是浮点数，`Eq` 在 f32 上本来就不成立。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub reader: Reader,
    pub appearance: Appearance,
    pub library: Library,
    pub language: Language,
    pub account: Account,
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
        self
    }

    /// 读默认位置的配置。文件不存在或内容坏了都退回默认值 —— 配置绝不该让程序起不来。
    pub fn load() -> Config {
        Self::load_from(&paths::config_path())
    }

    pub fn load_from(path: &Path) -> Config {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Config::default();
        };
        // 手改过 / 旧版本 / 别的机器同步来的值都不可信，读进来先规整一遍。
        if let Ok(config) = serde_json::from_str::<Config>(&text) {
            return config.normalized();
        }
        salvage(&text).normalized()
    }

    /// 写默认位置的配置。
    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&paths::config_path())
    }

    /// 写文件：先建父目录，写临时文件再 rename，避免写到一半崩掉留下半份坏 JSON。
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        paths::prepare_dir(path)?;
        let json = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into());
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    }
}

/// 整份读不出来时的补救：**按节**退回默认值。
///
/// 为什么需要它：`serde_json::from_str::<Config>` 是一根绳上的五个节，任何一个节的
/// 任何一个字段类型不对（`"font_size":"big"`、`"mode":3`、手滑多打的一个引号），
/// 整个调用就返回 `Err`，于是全部配置归零 —— 读者看到的是字号、主题、书库目录、
/// 账号一起消失，而且完全不知道原因。分节解析后，坏的那一节归零，其余各节照旧。
///
/// 只处理「JSON 本身合法、某一节的内容不合结构」这一类；连 JSON 都解析不了
/// （截断、少括号）时没有可逐节尝试的载体，仍然整份退回默认值。
fn salvage(text: &str) -> Config {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Config::default();
    };
    let mut config = Config::default();
    if let Some(reader) = section(&value, "reader") {
        config.reader = reader;
    }
    if let Some(appearance) = section(&value, "appearance") {
        config.appearance = appearance;
    }
    if let Some(library) = section(&value, "library") {
        config.library = library;
    }
    if let Some(language) = section(&value, "language") {
        config.language = language;
    }
    if let Some(account) = section(&value, "account") {
        config.account = account;
    }
    config
}

/// 取一节。缺这一节、或者这一节自己解不出来，都返回 `None` 让调用方保留默认值 ——
/// 具体类型由赋值目标推断，所以每个节的 `#[serde(default)]`（少键时用**本节**的默认）
/// 在这里同样成立。
fn section<T: serde::de::DeserializeOwned>(value: &serde_json::Value, key: &str) -> Option<T> {
    value
        .get(key)
        .and_then(|raw| serde_json::from_value::<T>(raw.clone()).ok())
}
