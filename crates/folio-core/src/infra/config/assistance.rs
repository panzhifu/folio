use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
/// 选区翻译配置；OpenAI 端点由 Vendor 引用共享。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Translation {
    /// 译成哪种语言（BCP-47 码，如 `zh-CN`）。**空串 = 跟随界面语言** —— 「跟随」
    /// 是一个选择，不是「还没选」：写死某个默认语等于替读者决定他没说的偏好，
    /// 而空串交给 app 侧在用的时候按 `Locale` 现解析（core 不认识「界面」）。
    pub target_lang: String,
    /// 用哪只引擎。默认 Google 免 key 档 —— 新装的机器不填任何东西也能翻译。
    pub engine: TranslationEngine,
    pub openai: OpenAi,
    pub deepl: DeepL,
}

impl Translation {
    /// 归一：首尾空白去掉。语言码本身信不过的写法（大小写错位）在引擎那头都能
    /// 容忍，这里不做第二处「纠正」。
    ///
    /// 模型清空时**不**在这里补默认值：设置页的文本行边打边存，补默认值会把
    /// 读者刚删空的那一格当场填回去。空值该不该有默认由引擎在发请求前判。
    pub fn normalized(mut self) -> Self {
        self.target_lang = self.target_lang.trim().to_string();
        self.openai.vendor = self.openai.vendor.trim().to_string();
        self.openai.model = self.openai.model.trim().to_string();
        self.deepl.api_key = self.deepl.api_key.trim().to_string();
        self
    }
}

/// AI 问答开关与模型引用，默认关闭；每次请求读取当前配置。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ai {
    /// 允许不允许用 AI。见上。
    pub enabled: bool,
    /// 空 = 跟随翻译那档的供应商；填了就是这一节自己的选择（档案 id）。
    pub vendor: String,
    /// 空 = 用翻译那一档填的模型；填了就只覆盖模型。
    /// 允许覆盖是因为「翻译用小模型、解释用大模型」是真需求，而重写一遍地址不是。
    pub model: String,
}

impl Ai {
    pub fn normalized(mut self) -> Self {
        self.vendor = self.vendor.trim().to_string();
        self.model = self.model.trim().to_string();
        self
    }
}

/// 本地 StarDict 词典。
///
/// `installed` 是**界面列表的真相源**：导入完成时写一条，删除时摘一条；启动时不扫
/// 数据目录。这样设置页与选区分词条看到的是同一份数，测试也不会因为开发机上恰好放了
/// 词典而变宽 / 多出一行。
///
/// 文件落在 `dir/<id>/`；`dir` 为空表示还没导入过，用 [`crate::infra::paths::dicts_dir`]。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Dictionary {
    /// 导入根目录；空串 = 还没导入过。
    pub dir: String,
    /// 已导入的词典。
    pub installed: Vec<DictionaryEntry>,
    /// 读者关掉的那几本（按 id）。默认全开 —— 导入就是为了查。
    pub disabled: Vec<String>,
}

/// 一本已导入词典在配置里的书目信息。文件本身在 `<dir>/<id>/`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DictionaryEntry {
    /// 目录名，也是界面开关与删除操作用的键。
    pub id: String,
    /// `.ifo` 里的书名；空时回落到 id。
    pub name: String,
    /// `.ifo` 声明的词条数；读不出来时是 0。
    pub words: u64,
}

/// 读者自己装进来的字体（在线下载或本地导入）。
///
/// 列表的**真相源是配置**，不是磁盘扫描 —— 与 [`Dictionary`] 同一条：设置页与字体
/// 选择器读的是同一份，开发机上恰好放了什么不会多出一行。文件落在
/// [`crate::infra::paths::fonts_dir`] 的 `<id>/<file>`；`family` 是注册进文本系统后
/// 设备报回来的家族名（不是下载时清单里那个展示名），选择器按它出条目。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Fonts {
    /// 已安装的字体。
    pub installed: Vec<FontEntry>,
}

/// 一份已装字体在配置里的条目。文件本身在 `<fonts 根>/<id>/<file>`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FontEntry {
    /// 目录名，也是删除操作用的键。
    pub id: String,
    /// 注册后设备报回来的家族名；空表示这份还没注册成功过（重启前没有家族名）。
    pub family: String,
    /// 目录里的字体文件名。
    pub file: String,
    /// 字节数；设置页展示与占用统计用。
    pub size_bytes: u64,
}

impl Fonts {
    pub fn normalized(mut self) -> Self {
        let mut seen = BTreeSet::new();
        let mut installed = Vec::with_capacity(self.installed.len());
        for mut entry in self.installed {
            entry.id = entry.id.trim().to_string();
            entry.family = entry.family.trim().to_string();
            entry.file = entry.file.trim().to_string();
            if entry.id.is_empty() || entry.file.is_empty() || !seen.insert(entry.id.clone()) {
                continue;
            }
            installed.push(entry);
        }
        self.installed = installed;
        self
    }

    /// 这一份现在装着吗（按 id）。
    pub fn has(&self, id: &str) -> bool {
        self.installed.iter().any(|entry| entry.id == id)
    }
}

impl Dictionary {
    pub fn normalized(mut self) -> Self {
        self.dir = self.dir.trim().to_string();
        let mut seen = BTreeSet::new();
        let mut installed = Vec::with_capacity(self.installed.len());
        for mut entry in self.installed {
            entry.id = entry.id.trim().to_string();
            entry.name = entry.name.trim().to_string();
            if entry.id.is_empty() || !seen.insert(entry.id.clone()) {
                continue;
            }
            if entry.name.is_empty() {
                entry.name = entry.id.clone();
            }
            installed.push(entry);
        }
        self.installed = installed;
        let installed_ids: BTreeSet<&str> = self
            .installed
            .iter()
            .map(|entry| entry.id.as_str())
            .collect();
        let mut seen = BTreeSet::new();
        self.disabled = self
            .disabled
            .into_iter()
            .filter_map(|id| {
                let id = id.trim();
                (!id.is_empty() && installed_ids.contains(id) && seen.insert(id.to_string()))
                    .then(|| id.to_string())
            })
            .collect();
        self
    }

    /// 词典文件落在哪儿：读者确认过的那个根，或数据目录下的 `dicts/`。
    pub fn root(&self) -> std::path::PathBuf {
        if self.dir.is_empty() {
            crate::infra::paths::dicts_dir()
        } else {
            std::path::PathBuf::from(&self.dir)
        }
    }

    /// 这一本现在能不能查。
    pub fn is_enabled(&self, id: &str) -> bool {
        self.installed.iter().any(|entry| entry.id == id) && !self.disabled.iter().any(|d| d == id)
    }
}

/// 翻译引擎。
///
/// 序列化名与 [`Self::as_str`] 同一形态：设置页下拉的值、`config.json` 里的写法、
/// 缓存表 `provider` 列的前缀都从这一处来 —— 三处各写一份就会各自漂移。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TranslationEngine {
    /// 免 key 的 Google 公开端点（`client=gtx`）。零配置可用，但它是非合同接口。
    #[default]
    Google,
    /// 任何说 OpenAI `chat/completions` 协议的服务：官方、DeepSeek、火山 ARK、
    /// 本地 Ollama / LM Studio。地址与模型都由读者填。
    OpenAi,
    /// DeepL 官方 API。免费档的 key 以 `:fx` 结尾，端点据此现选。
    DeepL,
}

impl TranslationEngine {
    /// 全部档位，按设置页该出现的顺序。
    pub const ALL: &'static [Self] = &[Self::Google, Self::OpenAi, Self::DeepL];

    /// 配置文件与设置界面用的键名。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::OpenAi => "openai",
            Self::DeepL => "deepl",
        }
    }

    /// 认不出的值回落 Google：免 key 档是唯一不需要读者再补参数的引擎。
    pub fn parse(value: &str) -> Self {
        match value {
            "openai" => Self::OpenAi,
            "deepl" => Self::DeepL,
            _ => Self::Google,
        }
    }
}

/// 自己实现序列化，理由与 [`Mode`]、[`PageTurn`] 那几条一样：derive 碰到认不出的
/// 引擎名会报错，而报错的代价是**整节** `Translation` 退回默认 —— 读者填好的地址、
/// 模型与密钥会跟着一起没，而那只是这一格写坏了。
impl Serialize for TranslationEngine {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for TranslationEngine {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Ok(Self::parse(&text))
    }
}

/// 出厂预填的官方地址（旧版 `OpenAi::default().base_url`）。现在它只活在迁移里：
/// 旧配置带着这个地址**且没填 key** 时，唯一的解释是读者从没打开过这一格，
/// 迁移不替他造档案（见 [`Config::migrate_vendors`]）。
pub const FACTORY_BASE_URL: &str = "https://api.openai.com/v1";

/// OpenAI 兼容端点。API key 可为空；地址和密钥由翻译与 AI 共享。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct Vendor {
    /// 引用键（`new_vendor_id` 生成）。界面不摆它；丢了引用就回退第一条。
    pub id: String,
    /// 给人看的名（「DeepSeek」「自建网关」）。空串时界面回落显示 host。
    pub name: String,
    /// API 根地址。请求时补 `/chat/completions`；写到方法那一层的也认。
    pub base_url: String,
    pub api_key: String,
}

impl Vendor {
    /// 界面那一行显示的名字：档案名优先，空了回落 host，再空回落一个统称。
    /// 回落两级的 `String`（host 是现抠的），调用方拿去当显示标签。
    pub fn display_name(&self) -> String {
        let name = self.name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
        let host = host_of(&self.base_url);
        if host.is_empty() {
            "供应商".to_string()
        } else {
            host
        }
    }

    pub fn normalized(mut self) -> Self {
        self.id = self.id.trim().to_string();
        self.name = self.name.trim().to_string();
        self.base_url = self.base_url.trim().to_string();
        self.api_key = self.api_key.trim().to_string();
        self
    }
}

/// 解析供应商引用；悬空引用回退第一条，空注册表返回 None。
pub fn resolve_vendor<'a>(vendors: &'a [Vendor], id: &str) -> Option<&'a Vendor> {
    if vendors.is_empty() {
        return None;
    }
    vendors
        .iter()
        .find(|vendor| vendor.id == id)
        .or_else(|| vendors.first())
}

/// 提取设置地址的 host，用于展示；不是 URL 安全校验。
pub fn host_of(base_url: &str) -> String {
    let rest = base_url
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    rest.split('/').next().unwrap_or_default().to_string()
}

static VENDOR_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 时间戳与进程内序号组成的本机引用键，不用于认证。
pub fn new_vendor_id() -> String {
    use std::sync::atomic::Ordering::Relaxed;
    let seq = VENDOR_SEQ.fetch_add(1, Relaxed);
    format!("v{}-{}", folio_base::time::now(), seq)
}

/// OpenAI 供应商引用与模型；旧内联端点由 migrate_vendors 迁移。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenAi {
    /// 引用 [`Config::vendors`] 里哪一条。空 = 回退第一条（[`resolve_vendor`]）。
    pub vendor: String,
    /// 模型名（如 `gpt-4o-mini`、`deepseek-chat`）。
    pub model: String,
    /// （迁移期暂留）旧版的内联地址。读进来后由 [`Config::migrate_vendors`]
    /// 折成档案并清空；新写的配置里恒为空串。
    pub base_url: String,
    /// （迁移期暂留）旧版的内联密钥。同上。
    pub api_key: String,
}

impl Default for OpenAi {
    fn default() -> Self {
        Self {
            vendor: String::new(),
            // 出厂模型保留：读者添加好档案、贴上 key、拨开开关就能用，
            // 不必再知道「模型名」是什么（要换随时换）。
            model: "gpt-4o-mini".into(),
            base_url: String::new(),
            api_key: String::new(),
        }
    }
}

/// DeepL 官方 API 的参数。免费 / 付费档只看 key 的后缀（`:fx`），不单列开关。
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeepL {
    pub api_key: String,
}
