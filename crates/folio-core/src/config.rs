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
}

impl Library {
    /// 位置确认过了没有。认过之后导入才放行。
    pub fn is_confirmed(&self) -> bool {
        !self.dir.trim().is_empty()
    }
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
        // 书库位置去掉首尾空白：填成 "  /x  " 会让 `is_confirmed` 说「确认过」，
        // 而后面按这个串去找目录会找不到。
        self.library.dir = self.library.dir.trim().to_string();
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

#[cfg(test)]
mod tests {
    //! 应用配置：分节、JSON 往返、缺文件/坏文件回落、读入夹取、语言键。只依赖公开 API。
    //!
    //! 这些测试盯的是「用户手改这个 JSON 之后程序会怎样」，所以全部从 JSON 文本出发，
    //! 不去直接改结构体 —— 结构体改不出坏输入。

    use crate::config::Config;
    use crate::i18n::Locale;
    use crate::{Account, Appearance, Language, Library, Mode, Reader};
    use serde_json::json;

    fn load(text: &str) -> Config {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, text).unwrap();
        Config::load_from(&path)
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
            account: Account {
                name: "阿遥".into(),
            },
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
        assert!(got.account.name.is_empty());
    }

    /// 老版本的 `reader` 一节里有 `theme`；新版在 `appearance`。
    /// 认不出来的字段 serde 会直接忽略，于是主题回到框架默认而不是报错。
    #[test]
    fn a_stale_theme_key_is_ignored_instead_of_failing() {
        let got = load(r#"{"reader":{"theme":"Folio Sepia"}}"#);
        assert_eq!(got.appearance.light, None);
        assert_eq!(got.appearance.mode, Mode::System);
    }

    /// 老版本把界面语言存在 reader 里；新版在 language 一节。
    /// 认不出来的字段 serde 会直接忽略，于是语言回到「跟随系统」而不是报错。
    #[test]
    fn an_unknown_locale_key_falls_back_to_the_system_language() {
        let got = load(r#"{"reader":{"locale":"fr"}}"#);
        assert_eq!(got.language.locale, Locale::detect());
    }

    /// 这一条必须**与运行环境无关**。`language` 一节缺 locale 键时用的是
    /// `Locale::detect()`，而 detect 读 `LANGUAGE`/`LC_ALL`/`LANG`，所以原来那句
    /// 「`"en"` 读回来是 `En`」在 `LANG=en_US.UTF-8` 的机器上，即便 JSON 里的语言
    /// 完全没被读到（整节退回 detect）也一样通过 —— 本机全绿、CI 上红的就是它。
    /// 这里显式挑一个**与本机系统语言不同**的值，让「读到了」和「回落了」不可能同值。
    ///
    /// JSON 用 `json!` 现搭，不手写转义花括号：这一条测试曾经因为多打的一个引号
    /// 变成了「整份 JSON 无效」，于是要验的 locale 从没被解过一遍。
    #[test]
    fn the_locale_survives_the_json_round_trip() {
        let different = Locale::ALL
            .iter()
            .copied()
            .find(|locale| *locale != Locale::detect())
            .expect("三种语言里总能找到一个和系统语言不同的");

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
    /// 不该看到主题名、书库目录、读者名一起消失，而且没有任何提示。
    #[test]
    fn one_broken_section_does_not_take_the_others_down() {
        let got = load(
            r#"{"reader":{"font_size":"big","line_height":2.5},
            "appearance":{"mode":"dark","light":"Gruvbox"},
            "library":{"dir":"~/Books"},
            "account":{"name":"阿遥"}}"#,
        );
        assert_eq!(
            got.reader,
            Reader::default(),
            "坏的是 reader 一节，就只有这一节退回默认"
        );
        assert_eq!(got.appearance.mode, Mode::Dark);
        assert_eq!(got.appearance.light.as_deref(), Some("Gruvbox"));
        assert_eq!(got.library.dir, "~/Books");
        assert_eq!(got.account.name, "阿遥");
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
            r#"{"reader":{"font_size":9000,"line_height":0,"indent_em":-5,"margin_x":1e9},"account":{"name":"x"}}"#,
        );
        assert_eq!(got.reader.font_size, 72.0);
        assert_eq!(got.reader.line_height, 1.0);
        assert_eq!(got.reader.indent_em, 0.0);
        assert_eq!(got.reader.margin_x, 400.0);
        assert_eq!(got.account.name, "x", "夹取只管排版数值，不碰别的节");
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

    #[test]
    fn the_library_section_holds_its_dir() {
        let config = Config {
            library: Library {
                dir: "~/Books".into(),
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
