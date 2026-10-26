//! 应用配置：软件里所有「用户能改、改完要记住」的东西都在这一个模块里。
//!
//! 分成六节，前五节各自对应设置界面里的一类（或排版引擎的输入），第六节是给调试用的、
//! 界面上没有：
//! - [`Reader`] —— 排版输入（字号、行距、缩进、版心、字体），排版引擎只认这一节；
//! - [`Appearance`] —— 浅深三态和两侧各用哪套主题，属于界面外观而不是排版输入；
//! - [`Library`] —— 书库目录；
//! - [`Language`] —— 界面语言；
//! - [`Speech`] —— 听书（合成朗读）的语速、音高与自动翻页；
//! - [`Logging`] —— 日志落到哪儿、说到多细（改完重启生效，界面上没有这一页）。
//!
//! 落盘位置由 [`crate::infra::paths`] 决定（`~/.config/folio/config.json`）。
//!
//! 为什么是配置文件而不是继续放 SQLite：配置是**应用级**数据、跨书存在，
//! 放成一个可读可备份的小 JSON 比塞进书库数据库更符合直觉，也方便用户手改。
//! 反过来说，一切需要「按书存」的东西（进度、标注、缓存的章节）都在 `store` 里，
//! 两边不重叠。

use std::collections::{BTreeMap, BTreeSet};
use std::ops::RangeInclusive;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::infra::error::IoResultExt as _;
use crate::infra::i18n::Locale;
use crate::infra::paths;
use crate::store::AnnotationStyle;

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

/// 翻页时那一张纸怎么走：沿哪根轴、要不要把旧页留在原地、还是干脆不走。
///
/// 存**全局**而不是存进 [`Reader`]：`Reader` 整份是排版失效键（见它自己那条注释 ——
/// 那里按序列化后的整份取哈希），把动画档位塞进去就等于「换一种翻页动画触发整章重排」。
/// 一张纸怎么搬与一行字折在哪里毫无关系。
///
/// 「禁用」必须是应用自己的一档，不能指望系统位：`gpui-pre 0.3.9` 里
/// `App::reduce_motion` 初始为 `false`，`set_reduce_motion` 唯一的调用者是它自己的两条
/// 测试；`gpui-mobile` 那份 fork 里 `reduce_motion` 零命中。也就是说两端都**不会**有人把
/// 它置真，「系统已经归零」这条理由不成立（从前那一版据此不设无动画档）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageTurn {
    /// 左右平移：两张纸同速反向走，读起来像一张长纸被抽过去。
    Slide,
    /// 上下平移：同一趟沿竖轴走，像把一叠纸往上抽。
    SlideUp,
    /// 左右覆盖：旧页停在原地，新页整张盖上来。
    Cover,
    /// 仿真翻页：一张纸被**折走**。纸当一张不会弯的硬片，绕一条直线（折线）做刚体翻转 ——
    /// 动的是旧页，新页平躺在它下面被一点点露出来；折线的倾角由起折点那一个常数给出，
    /// 随行程自己变直（几何与阴影见 `reader::turn_animation::Curl`）。
    Curl,
    /// 上下滚动：不摆第二张纸，翻页就是画出来的位置补间过去。这一档同时决定摆法 ——
    /// 选中它，界面那一层会把这本书带进连续滚动；选任何一张纸的走法则把它带回分页 /
    /// 双页（见 `reader::chrome::reconcile`）。
    Scroll,
    /// 不走动画：当场落定，也就不必为这一帧多摆一张纸。**初始默认是这一档** ——
    /// 一本没动过设置的书就是左右翻页、当场落定，不摆第二张纸。动画与连续滚动都是
    /// 读者加上去的东西，不是我们替他先选好的东西。
    #[default]
    None,
}

impl PageTurn {
    /// 全部档位，按设置界面该出现的顺序。加一档忘了在这里挂，界面上就永远选不到它。
    pub const ALL: &'static [Self] = &[
        Self::Slide,
        Self::SlideUp,
        Self::Cover,
        Self::Curl,
        Self::Scroll,
        Self::None,
    ];

    /// 配置文件与设置界面用的键名。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Slide => "slide",
            Self::SlideUp => "slide_up",
            Self::Cover => "cover",
            Self::Curl => "curl",
            Self::Scroll => "scroll",
            Self::None => "none",
        }
    }

    /// 认不出的值回落 [`Self::default`]：手改坏一个键不该让整节外观退回默认，而「没有有效
    /// 值」这一格全书只该有一个答案 —— 与一份新配置落进同一档。
    ///
    /// `horizontal` / `vertical` 是这一档只有两格时写进配置的老键名，照读：
    /// 读者换过一次软件版本就发现动画选项回到默认，是没道理的。
    pub fn parse(value: &str) -> Self {
        match value {
            // `slide` 从前是兜底那一格，所以自己不需要臂；兜底换成「不走动画」之后它就得
            // 明写，否则配置里手写 "slide" 会落错档。
            "slide" | "horizontal" => Self::Slide,
            "slide_up" | "vertical" => Self::SlideUp,
            "cover" => Self::Cover,
            "curl" => Self::Curl,
            "scroll" => Self::Scroll,
            "none" => Self::None,
            _ => Self::default(),
        }
    }
}

/// 自己实现序列化，理由与 [`Mode`]、[`TocDensity`] 那两条一样：derive 碰到认不出的字符串
/// 会报错，而报错的代价是**整节** `Appearance` 退回默认（见 [`salvage`]）—— 浅深两侧选过的
/// 主题名会跟着一起没，而那只是这一档写坏了。
impl Serialize for PageTurn {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PageTurn {
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
///
/// 只 `PartialEq` 而不带 `Eq`：`brightness` 是浮点数，`Eq` 在 f32 上本来就不成立
/// （与 [`Config`] 那条同一口径）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub mode: Mode,
    /// 解析到浅色时用的主题名。
    pub light: Option<String>,
    /// 解析到深色时用的主题名。
    pub dark: Option<String>,
    /// 正文翻页沿哪根轴平移（见 [`PageTurn`] 为什么这一格不在 `Reader` 里）。
    pub page_turn: PageTurn,
    /// 一次翻页动画走多久，单位**毫秒**（跟手那一截不算：它 1:1 跟着手指，没有时长可言）。
    ///
    /// 存**时长**而不是「速度倍数」：两者互为倒数，倍数那一头要在界面上反着摆才符合
    /// 「往右 = 更慢」的直觉，读数还得换算一次；而毫秒这个数读者有参照物（280 与 700 一眼
    /// 能分清），「1.7×」没有。
    ///
    /// 与 [`page_turn`](Self::page_turn) 同一个理由存在这一节而不是 [`Reader`]：一张纸走
    /// 多久与一行字折在哪里毫无关系，放进 `Reader` 就等于「调一次动画速度触发整章重排」。
    ///
    /// 管到的那两腿：点屏 / 按键 / 滚轮起的那一趟，以及松手惯性那一腿的**时长上限**
    /// （那一腿自己按剩余行程与手指速度算，只被这一个数夹住上端）。
    pub turn_duration: f32,
    /// 新划的一条用哪种画法（底色带 / 实线 / 波浪）。
    ///
    /// 存的是**这一档**而不是「读者划的每一条」：每一条款式在库里自己那一行上
    /// （[`AnnotationStyle`](crate::AnnotationStyle)，v21 起有 `annotations.style` 这一列），
    /// 这一格只回答「下一条款长什么样」。挑中库里已有一条时，菜单里那三格改的是**那一条**，
    /// 不碰这一格 —— 读者的默认档不该被一次改色动作顺手改掉。
    ///
    /// 与 [`page_turn`](Self::page_turn) 同一个理由存在这一节而不是 [`Reader`]：一条痕迹
    /// 怎么画与一行字折在哪里毫无关系，塞进 `Reader` 就是「换一种画法触发整本书重排」。
    ///
    /// 默认档是 [`AnnotationStyle::Highlight`]，而**库里那一列的默认是 `line`**，两个默认值
    /// 各答各的问题、不冲突：那一列答「v21 之前划的那些条当时长什么样」（答案确定是实线），
    /// 这一格答「往后的新条目长什么样」。见 `store::schema` 的 v21 那条。
    pub highlight_style: AnnotationStyle,
    /// 阅读界面的亮度，`None` = **不接管**（跟随系统）。
    ///
    /// 与 [`page_turn`](Self::page_turn) 同一个理由存在这一节而不是 [`Reader`]：`Reader`
    /// 整份是排版失效键，而把画面压暗与一行字折在哪里毫无关系。
    ///
    /// 同一个数在两端各落到自己那一手上：Android 写窗口自己的 `screenBrightness`
    /// （`None` 传 `-1.0`，即 `BRIGHTNESS_OVERRIDE_NONE`，背光交还系统）；桌面上背光没有口，
    /// 压暗的是应用盖在阅读层上的那一层（透明度由 `ui::features::reader::chrome::dim_alpha`
    /// 从这一个数算出来）。所以「亮度」在两端的**读数**是同一个，只是被暗的方式不同。
    ///
    /// 上限不写成数：滑条推到顶（`1.0`）由 [`Appearance::normalized`] 归成 `None`，
    /// 于是「跟随系统」在这份配置里只有一个表示法。
    pub brightness: Option<f32>,
}

impl Appearance {
    /// 亮度滑条的下限：再往下手机屏幕基本黑了，而「黑屏」不是一种读法。
    /// 界面那一条滑条直接引用这个数，不在 UI 里抄第二份。
    pub const BRIGHTNESS_MIN: f32 = 0.05;

    /// 翻页时长的默认档。这一档与外壳整屏进出的那一个数同一档
    /// （`folio_app::ui::shell::NAV_TRANSITION`，它是照底部弹层那根弹簧定的）：同一屏里
    /// 换两张纸与换一整屏若被同一只手读出两种快慢，就该有一边是错的。
    ///
    /// 这里没法直接引用那个常数（core 不依赖 app），所以靠 app 那侧一条门钉住这个等式 ——
    /// 不是靠这条注释里的那个数不走偏。
    pub const TURN_DURATION_DEFAULT: f32 = 280.0;
    /// 下限：短到这一档以下，纸怎么走已经看不清了，落定读成「啪」的一声。
    pub const TURN_DURATION_MIN: f32 = 120.0;
    /// 上限：长到这一档以上就不是「慢一点」而是「纸自己在磨」。
    pub const TURN_DURATION_MAX: f32 = 700.0;

    /// 空串按「没设」处理：`""` 既不是合法主题名，又会让下拉框显示成空白。
    pub fn normalized(mut self) -> Self {
        for name in [&mut self.light, &mut self.dark] {
            *name = name
                .take()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
        }
        // 顶格与非有限值都归成「不接管」：读回来的 `1.0`、手改成 `NaN` 的数，都不该在
        // 配置里留下第二个「最大亮度」的表示法。
        self.brightness = match self.brightness {
            Some(v) if v >= 1.0 || !v.is_finite() => None,
            Some(v) => Some(v.clamp(Self::BRIGHTNESS_MIN, 1.0)),
            None => None,
        };
        // 非有限值退回**默认档**而不是夹到某个端点：`NaN` 被读成「推到最慢」等于把坏数据
        // 当成一个选择。区间两端则照亮度那条老规矩夹住，手改 `config.json` 越界不改语义。
        self.turn_duration = if !self.turn_duration.is_finite() {
            Self::TURN_DURATION_DEFAULT
        } else {
            self.turn_duration
                .clamp(Self::TURN_DURATION_MIN, Self::TURN_DURATION_MAX)
        };
        self
    }
}

/// 手写而不是 derive：`turn_duration` 的默认档不是 `f32` 的零值。derive 出来的那份会把
/// 翻页读成**零时长**（纸当场落定），而配置里那个 `0.0` 没人看得出它从哪来。
///
/// 这里不能写 `..Default::default()` —— 那是自己调自己。少写一个字段编译器直接报，
/// 正是这一格想要的：新增字段时默认值必须显式想过一遍。
impl Default for Appearance {
    fn default() -> Self {
        Self {
            mode: Mode::default(),
            light: None,
            dark: None,
            page_turn: PageTurn::default(),
            turn_duration: Self::TURN_DURATION_DEFAULT,
            highlight_style: AnnotationStyle::default(),
            brightness: None,
        }
    }
}

/// 听书（合成朗读）的偏好。
///
/// 语速与音高存**语义值**（常速 1.0），换算到各引擎那一档住在 `crate::speech`：
/// Android 0.1–10.0 恒等、Linux Speech Dispatcher −100–100 线性（`speech::gate`
/// 里那对换算函数）。区间不问引擎 —— `Tts` 的 `min_*`/`max_*` 那一族没有
/// `Features` 闸门，Android 的 `min_volume` 是 `todo!()`，问了就崩，所以上下限
/// 在这里自己定、界面直接引用，不抄第二份。
///
/// 没有音色（`voice`）这一格：Android 上 `voices()` 直接回 `Err`，桌面那份清单在
/// 手机上没有对应物 —— 第一版不选音色，配置里就不留空挂的字段。
///
/// 只 `PartialEq` 而不带 `Eq`：浮点字段与 [`Config`] 那条同一口径。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Speech {
    /// 语速，常速 1.0。
    pub rate: f32,
    /// 音高，常速 1.0。
    pub pitch: f32,
    /// 念到哪一段就翻到哪一段。关掉是「念它的，我翻我的」—— 屏上的高亮仍跟着
    /// 念的那段走，只是不动页。
    pub auto_scroll: bool,
}

impl Speech {
    /// 常速原点，也是归一里非有限值的退路。
    pub const NORMAL: f32 = 1.0;
    /// 语速的语义区间。设置页那条滑条直接引用，两端共用同一份。
    pub const RATE_RANGE: RangeInclusive<f32> = 0.5..=2.0;
    /// 音高的语义区间，与语速同一档。
    pub const PITCH_RANGE: RangeInclusive<f32> = 0.5..=2.0;

    /// 非有限值退回**常速**而不是夹到端点：手改成 `inf`/坏出来的 `NaN` 被读成
    /// 「最快」等于把坏数据当成一个选择（与 [`Appearance::normalized`] 里
    /// `turn_duration` 那条同一口径）；区间两端照常夹住。
    pub fn normalized(mut self) -> Self {
        self.rate = clamp_semantic(self.rate, &Self::RATE_RANGE);
        self.pitch = clamp_semantic(self.pitch, &Self::PITCH_RANGE);
        self
    }
}

/// 手写而不是 derive（与各节同一条规矩）：`rate`/`pitch` 的默认是**常速 1.0**，
/// 不是浮点的零值 —— derive 出来的那份会把听书念成引擎档的最慢速。
impl Default for Speech {
    fn default() -> Self {
        Self {
            rate: Self::NORMAL,
            pitch: Self::NORMAL,
            auto_scroll: true,
        }
    }
}

/// 语义值（0.5–2.0）的归一：非有限退常速，其余夹进区间。
fn clamp_semantic(value: f32, range: &RangeInclusive<f32>) -> f32 {
    if !value.is_finite() {
        Speech::NORMAL
    } else {
        value.clamp(*range.start(), *range.end())
    }
}

/// 翻译：选区翻译那一格的行为。
///
/// 语言管「译成什么」，引擎管「谁译」。三档引擎的私有参数各占一节，而不是摊成同一层：
/// `openai.base_url` 与 `deepl.api_key` 互不相干，摊平之后每一格都得在注释里解释
/// 「这格只对谁有用」，读配置的人还得自己记。
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
    /// 地址 / 模型清空时**不**在这里补默认值：设置页的文本行边打边存，补默认值会把
    /// 读者刚删空的那一格当场填回去。空值该不该有默认由引擎在发请求前判。
    pub fn normalized(mut self) -> Self {
        self.target_lang = self.target_lang.trim().to_string();
        self.openai.base_url = self.openai.base_url.trim().to_string();
        self.openai.model = self.openai.model.trim().to_string();
        self.openai.api_key = self.openai.api_key.trim().to_string();
        self.deepl.api_key = self.deepl.api_key.trim().to_string();
        self
    }
}

/// 正文按哪根轴排。
///
/// `Auto` 认书自己的 `writing-mode`，是默认那档；另两档由读者层用
/// `!important` 盖过作者声明（落点在 `folio_app::...::chapter_layout::Document::reader_rules`）。
/// 竖排**只在分页单栏那一档落地**：连续滚动整条坐标链都按「沿 y 走」建着，
/// 转轴等于重做一遍（见 `folio_core::layout::LayoutOptions::allow_vertical`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextDirection {
    /// 书怎么写就怎么排。
    #[default]
    Auto,
    /// 强制竖排（分页单栏时生效，滚动退回横排）。
    Vertical,
    /// 强制横排，书的竖排声明也压掉。
    Horizontal,
}

impl TextDirection {
    /// 全部档位，按设置界面该出现的顺序。
    pub const ALL: &'static [Self] = &[Self::Auto, Self::Vertical, Self::Horizontal];

    /// 配置文件与设置界面用的键名。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Vertical => "vertical",
            Self::Horizontal => "horizontal",
        }
    }

    /// 认不出的值回落 `Auto`：手改坏一个键不该让整节 `Reader` 退回默认。
    pub fn parse(value: &str) -> Self {
        match value {
            "vertical" => Self::Vertical,
            "horizontal" => Self::Horizontal,
            _ => Self::Auto,
        }
    }
}

/// 自己实现序列化，理由与 [`Mode`] 那条一样：derive 碰到认不出的字符串会报错，
/// 而报错的代价是**整节** `Reader` 退回默认 —— 字号、行距、字体家族会跟着一起没。
impl Serialize for TextDirection {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for TextDirection {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Ok(Self::parse(&text))
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

/// OpenAI 兼容服务的参数。
///
/// key 允许为空：本地 Ollama / LM Studio 不认 `Authorization`，而官方与各家云服务
/// 非填不可。空 key 时请求不带头，由引擎自己回 401 —— 比在界面层猜「这只引擎要不要
/// key」诚实。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenAi {
    /// API 根地址（如 `https://api.openai.com/v1`）。请求时补 `/chat/completions`；
    /// 已经写到 `/chat/completions` 的也认。
    pub base_url: String,
    /// 模型名（如 `gpt-4o-mini`、`qwen2.5:7b`）。
    pub model: String,
    pub api_key: String,
}

impl Default for OpenAi {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".into(),
            model: "gpt-4o-mini".into(),
            api_key: String::new(),
        }
    }
}

/// DeepL 官方 API 的参数。免费 / 付费档只看 key 的后缀（`:fx`），不单列开关。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeepL {
    pub api_key: String,
}

/// WebDAV 同步。只同步**进度 / 批注 / 书签**，不传书文件，也不碰这份配置本身。
///
/// 密码与翻译 key 同住 `config.json`：落盘已经收在 0600（见 [`Config::save_to`]），
/// 再拆一个秘密文件只会让「备份/恢复」多一处要记得的地方。`device_id` 由 app 在第一次
/// 同步时生成（见 `folio_core::sync::new_device_id`），它不是读者要填的东西。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sync {
    pub enabled: bool,
    /// WebDAV 入口根地址（建议 https）。同步文件挂在它下面的 `remote_dir`。
    pub url: String,
    pub username: String,
    pub password: String,
    /// 远端目录，默认 `folio-sync`；空串表示直接写在根下。
    pub remote_dir: String,
    /// 这台安装的稳定 id，也是它在远端那份快照的文件名（`<device_id>.json`）。
    pub device_id: String,
}

impl Default for Sync {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            username: String::new(),
            password: String::new(),
            remote_dir: "folio-sync".into(),
            device_id: String::new(),
        }
    }
}

impl Sync {
    /// 与其它节同一条：手改过的值先去掉误带的首尾空白。**密码除外** —— 空格可能是密码
    /// 的一部分；远端目录去掉两端斜杠，拼路径时只由同步层补一次。
    pub fn normalized(mut self) -> Self {
        self.url = self.url.trim().to_string();
        self.username = self.username.trim().to_string();
        self.remote_dir = self.remote_dir.trim().trim_matches('/').to_string();
        self.device_id = self.device_id.trim().to_string();
        self
    }

    /// 有地址且打开了开关，才谈得上同步。
    pub fn configured(&self) -> bool {
        self.enabled && !self.url.is_empty()
    }
}

/// OPDS 书源列表。与 `Sync` 同一条：密码住在 0600 的 `config.json` 里，
/// 不单开秘密文件。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Opds {
    pub catalogs: Vec<OpdsCatalog>,
}

impl Opds {
    /// 名称、地址、用户名去掉误带空白；密码原样保留。地址不改写（feed 的相对链接
    /// 要按它解析，擅自补斜杠会让 `../` 的基准变一格）。
    pub fn normalized(mut self) -> Self {
        for catalog in &mut self.catalogs {
            catalog.name = catalog.name.trim().to_string();
            catalog.url = catalog.url.trim().to_string();
            catalog.username = catalog.username.trim().to_string();
        }
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpdsCatalog {
    pub name: String,
    pub url: String,
    pub username: String,
    pub password: String,
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
    /// 字与字之间额外的空隙，单位是字号倍（排版时折成 `字号 × 这一格` 像素）。
    /// 负值收紧，`0` 是不加不减。
    pub letter_spacing_em: f32,
    /// 正文栏最大宽度（像素）。**`0` 表示不设上限、随窗口铺满**，
    /// 所以拿到 `0` 时**不能**当成「宽度上限是 0」去夹任何东西。
    pub max_measure: f32,
    /// 正文栏两侧留白（像素），左右各一份。
    ///
    /// 定在 24 是按手机量的：那块屏一般 360–400 逻辑像素宽，两侧各 48 就吃掉 25%，
    /// 一行只剩十几个字，读起来像窄栏报纸；24 留 12% 上下，边到字仍够拇指按不到。
    /// 桌面上这一条只是最小留白，行长由 `max_measure` 管，所以这个数变小不影响宽窗口。
    pub margin_x: f32,
    /// 正文栏上下留白（像素），顶、底各一份。
    ///
    /// **`0` 表示跟随书的 `@page` 页边距**（没写就是 0）；一旦离开 0，读者的值整个覆盖
    /// 书里那一条。它不改折行，只改一页能装多少行（分页切片高与滚动可视带各让出这么多）。
    pub margin_y: f32,
    pub justify: bool,
    /// 正文的字重（100–900 的 CSS 数值）。设备上没有对应字面时排版层按最接近的一面挑，
    /// 600 以上还会叠一遍合成粗体（见 `reader/view/paint.rs` 的 `needs_synthetic_bold`）。
    pub font_weight: u16,
    /// 正文按哪根轴排（见 [`TextDirection`]）。进 `Reader` 是因为它会改变行盒几何。
    pub text_direction: TextDirection,
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
            letter_spacing_em: 0.0,
            // 0 = 随窗口铺满（全屏时正文栏跟着变宽）。
            max_measure: 0.0,
            margin_x: 24.0,
            margin_y: 0.0,
            justify: true,
            font_weight: 400,
            text_direction: TextDirection::Auto,
            font_family: "LXGW WenKai".into(),
            mono_font_family: "monospace".into(),
            toc_density: TocDensity::default(),
            toc_max_title_lines: 2,
            toc_dim_read: true,
        }
    }
}

impl Reader {
    /// 字号的合法区间。排版拿到超出这一段的数要么算出零高一行、要么把一栏撑到没有字，
    /// 所以入库前夹住（[`Reader::clamped`]），而界面那头读的是同一对数：设置屏的数字框
    /// 与阅读卡片里的滑条都不该再抄第二份「最小/最大字号」。
    pub const FONT_SIZE_MIN: f32 = 9.0;
    pub const FONT_SIZE_MAX: f32 = 72.0;
    /// 行高 = 字号 × 这一倍数。下界 1.0 是「行挨行」，再小字就互相压住；上界 4.0 一屏
    /// 只剩两行，已经不是一种读法。
    pub const LINE_HEIGHT_MIN: f32 = 1.0;
    pub const LINE_HEIGHT_MAX: f32 = 4.0;
    /// 段间距，单位是字号倍。`0` 是合法档（不空行，靠首行缩进分段）。
    pub const PARAGRAPH_GAP_MIN: f32 = 0.0;
    pub const PARAGRAPH_GAP_MAX: f32 = 4.0;
    /// 首行缩进，单位是字符。`0` 是合法档（不缩进）。
    pub const INDENT_MIN: f32 = 0.0;
    pub const INDENT_MAX: f32 = 6.0;
    /// 字距，单位是字号倍。下界留住一点收紧的余地（负字距排版层本来就支持），
    /// 上界再大就成了一行散沙。
    pub const LETTER_SPACING_MIN: f32 = -0.05;
    pub const LETTER_SPACING_MAX: f32 = 0.2;
    /// 正文栏最大宽度（像素）。`0` 是「随窗口铺满」那个约定值，所以下界就是 0；上界之外
    /// 的宽窗口本来也被窗口自己限住了，写更大只是骗读者说这格能调。
    pub const MAX_MEASURE_MIN: f32 = 0.0;
    pub const MAX_MEASURE_MAX: f32 = 4000.0;
    /// 两侧留白（像素，左右各一份）。上界那一档在手机上已经把正文压成一条，摆到更宽只会
    /// 让读者以为版心还在。
    pub const MARGIN_X_MIN: f32 = 0.0;
    pub const MARGIN_X_MAX: f32 = 400.0;
    /// 上下留白（像素，顶底各一份）。与左右同一段：再宽正文就被压没了。
    ///
    /// 下界 `0` 是「跟随书的 `@page`」，不是「没有留白」。
    pub const MARGIN_Y_MIN: f32 = 0.0;
    pub const MARGIN_Y_MAX: f32 = 400.0;
    /// 字重的合法区间。CSS 的 100–900，但只取整百（字体匹配本来就没有更细的档）。
    pub const FONT_WEIGHT_MIN: u16 = 100;
    pub const FONT_WEIGHT_MAX: u16 = 900;
    /// 设置界面摆出来的字重档位。三档而不是 100–900 铺满：字体匹配只认「同族里最接近
    /// 的那一面」，档位再密，落到屏幕上还是这几个。
    pub const FONT_WEIGHT_PRESETS: [u16; 3] = [300, 400, 700];

    /// 配置里存的字重 → 界面上该选中哪一档。
    ///
    /// 配置可以是手改出来的任何 100–900（[`Reader::clamped`] 只管区间），下拉里却只有三档；
    /// 取最接近的那一档，按钮念的就是这一刻实际会走的那一档。
    pub fn nearest_font_weight(weight: u16) -> u16 {
        Self::FONT_WEIGHT_PRESETS
            .into_iter()
            .min_by_key(|preset| preset.abs_diff(weight))
            .unwrap_or(400)
    }

    /// 从磁盘读回来的值不可信（手改过、旧版本、别的机器同步来的），
    /// 越界值会让排版算出零高或无限宽，所以入库前一律夹住。
    pub fn clamped(mut self) -> Self {
        self.font_size = self
            .font_size
            .clamp(Self::FONT_SIZE_MIN, Self::FONT_SIZE_MAX);
        self.line_height = self
            .line_height
            .clamp(Self::LINE_HEIGHT_MIN, Self::LINE_HEIGHT_MAX);
        self.paragraph_gap_em = self
            .paragraph_gap_em
            .clamp(Self::PARAGRAPH_GAP_MIN, Self::PARAGRAPH_GAP_MAX);
        self.indent_em = self.indent_em.clamp(Self::INDENT_MIN, Self::INDENT_MAX);
        self.letter_spacing_em = self
            .letter_spacing_em
            .clamp(Self::LETTER_SPACING_MIN, Self::LETTER_SPACING_MAX);
        // 0 是合法的「铺满」，所以下界就是 0。
        self.max_measure = self
            .max_measure
            .clamp(Self::MAX_MEASURE_MIN, Self::MAX_MEASURE_MAX);
        self.margin_x = self.margin_x.clamp(Self::MARGIN_X_MIN, Self::MARGIN_X_MAX);
        self.margin_y = self.margin_y.clamp(Self::MARGIN_Y_MIN, Self::MARGIN_Y_MAX);
        self.font_weight = self
            .font_weight
            .clamp(Self::FONT_WEIGHT_MIN, Self::FONT_WEIGHT_MAX);
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
    pub speech: Speech,
    pub translation: Translation,
    pub sync: Sync,
    pub opds: Opds,
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
            speech: Speech::default(),
            translation: Translation::default(),
            sync: Sync::default(),
            opds: Opds::default(),
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
        self.speech = self.speech.normalized();
        self.translation = self.translation.normalized();
        self.sync = self.sync.normalized();
        self.opds = self.opds.normalized();
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
    if let Some(speech) = section(value, "speech") {
        config.speech = speech;
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

    use crate::infra::config::{Config, Env, Layer, TranslationEngine, WatchDir};
    use crate::infra::i18n::Locale;
    use crate::{AnnotationStyle, Appearance, Language, Library, Mode, PageTurn, Reader};
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

    /// 引擎与它的私有参数要读得回来，且各格的首尾空白在归一里去掉 ——
    /// 手抄地址时多带一个空格，拼出来的 URL 就是另一种东西。
    #[test]
    fn translation_engine_and_its_private_fields_round_trip() {
        let got = load(
            r#"{"translation":{
                "target_lang":" ja ",
                "engine":"openai",
                "openai":{"base_url":" https://example.test/v1 ","model":" qwen2.5:7b ","api_key":" sk-1 "},
                "deepl":{"api_key":" fx-1 "}
            }}"#,
        );
        assert_eq!(got.translation.target_lang, "ja");
        assert_eq!(got.translation.engine, TranslationEngine::OpenAi);
        assert_eq!(got.translation.openai.base_url, "https://example.test/v1");
        assert_eq!(got.translation.openai.model, "qwen2.5:7b");
        assert_eq!(got.translation.openai.api_key, "sk-1");
        assert_eq!(got.translation.deepl.api_key, "fx-1");
    }

    /// 老配置里只有 `target_lang`：缺的两节各自取自己的默认值，引擎落到免 key 档。
    #[test]
    fn a_first_phase_config_gains_the_engine_defaults() {
        let got = load(r#"{"translation":{"target_lang":"en"}}"#);
        assert_eq!(got.translation.engine, TranslationEngine::Google);
        assert_eq!(
            got.translation.openai.base_url,
            crate::infra::config::OpenAi::default().base_url
        );
        assert!(got.translation.openai.api_key.is_empty());
        assert!(got.translation.deepl.api_key.is_empty());
    }

    /// 认不出的引擎名回落 Google，**不连累同一节里的地址、模型与密钥**：
    /// 手改坏一个字符串的代价不该是整份翻译配置归零。
    #[test]
    fn an_unknown_engine_name_falls_back_without_taking_the_section_with_it() {
        let got = load(
            r#"{"translation":{
                "target_lang":"ja",
                "engine":"polyglot",
                "deepl":{"api_key":" k "}
            }}"#,
        );
        assert_eq!(got.translation.engine, TranslationEngine::Google);
        assert_eq!(
            got.translation.target_lang, "ja",
            "写坏引擎名不该带走别的格"
        );
        assert_eq!(got.translation.deepl.api_key, "k");
    }

    /// 清空地址 / 模型**不能**在归一里被默认值填回去：设置页边打边存，
    /// 填回去等于读者删不空这一格。空值该不该有默认由引擎发请求前判。
    #[test]
    fn clearing_openai_fields_keeps_them_empty() {
        let got = load(r#"{"translation":{"openai":{"base_url":"","model":""}}}"#);
        assert!(got.translation.openai.base_url.is_empty());
        assert!(got.translation.openai.model.is_empty());
    }

    /// 配置文件里住着 API Key：默认 umask 下新建文件是 0644，落盘必须收到 0600。
    #[cfg(unix)]
    #[test]
    fn a_saved_config_is_readable_only_by_its_owner() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        Config::default().save_to(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "配置文件带着 API Key，权限该是 0600");
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
                // 故意不是默认值：这一趟要把新键真写出去再读回来，填默认值等于没测到它。
                page_turn: PageTurn::SlideUp,
                turn_duration: 420.0,
                highlight_style: AnnotationStyle::Wavy,
                brightness: Some(0.6),
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
            r#"{"reader":{"font_size":9000,"line_height":0,"indent_em":-5,"margin_x":1e9,"letter_spacing_em":5,"margin_y":-20,"font_weight":9999},"language":{"locale":"ja"}}"#,
        );
        assert_eq!(got.reader.font_size, 72.0);
        assert_eq!(got.reader.line_height, 1.0);
        assert_eq!(got.reader.indent_em, 0.0);
        assert_eq!(got.reader.margin_x, 400.0);
        assert_eq!(got.reader.letter_spacing_em, Reader::LETTER_SPACING_MAX);
        assert_eq!(got.reader.margin_y, 0.0);
        assert_eq!(got.reader.font_weight, 900);
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

    /// 手改进配置的字重（100–900 之间任何数）在设置界面要落回最近的一档，
    /// 否则下拉按钮上没有可念的字。
    #[test]
    fn a_hand_edited_weight_snaps_to_the_nearest_preset() {
        assert_eq!(Reader::nearest_font_weight(400), 400);
        assert_eq!(Reader::nearest_font_weight(600), 700, "600 离加粗更近");
        assert_eq!(
            Reader::nearest_font_weight(100),
            300,
            "比最细还细的落到最细"
        );
        assert_eq!(
            Reader::nearest_font_weight(900),
            700,
            "比最粗还粗的落到最粗"
        );
    }

    /// 亮度这一格只有「不接管」一个顶格写法，而越界的数夹住而不是整格丢掉。
    /// 界面那条滑条的两端读的就是这两个数（`Appearance::BRIGHTNESS_MIN` 与 `1.0`），
    /// 所以夹取规则钉在这里：滑条与配置之间不许有第二套「最大亮度」的表示法。
    #[test]
    fn the_brightness_top_end_is_the_same_thing_as_following_the_system() {
        for written in ["1.0", "3.5"] {
            let got = load(&format!(r#"{{"appearance":{{"brightness":{written}}}}}"#));
            assert_eq!(
                got.appearance.brightness, None,
                "{written} 是推到顶，该归成「跟随系统」而不是留一个 1.0"
            );
        }
        for written in ["0.0", "0.001", "-2.0"] {
            let got = load(&format!(r#"{{"appearance":{{"brightness":{written}}}}}"#));
            assert_eq!(
                got.appearance.brightness,
                Some(Appearance::BRIGHTNESS_MIN),
                "{written} 越界了，夹到下限而不是丢掉整格"
            );
        }
        assert_eq!(
            load(r#"{"appearance":{"brightness":0.6}}"#)
                .appearance
                .brightness,
            Some(0.6),
            "区间内的数原样留着"
        );
        assert_eq!(
            load(r#"{"appearance":{"mode":"dark"}}"#)
                .appearance
                .brightness,
            None,
            "没写过这一格就是交还系统"
        );
    }

    /// 翻页时长那一条滑条的两端与默认档就是这里这三个数，所以夹取规则钉在同一格：
    /// 界面与配置之间不许有第二套「最慢/最快」的表示法，而 `NaN` 退回**默认档**而不是
    /// 某个端点 —— 坏数据不该被读成读者的一个选择。
    #[test]
    fn the_turn_duration_clamps_to_the_slider_ends() {
        for (written, want) in [
            ("-1.0", Appearance::TURN_DURATION_MIN),
            ("60.0", Appearance::TURN_DURATION_MIN),
            ("300.0", 300.0),
            ("1800.0", Appearance::TURN_DURATION_MAX),
        ] {
            let got = load(&format!(
                r#"{{"appearance":{{"turn_duration":{written}}}}}"#
            ));
            assert_eq!(
                got.appearance.turn_duration, want,
                "{written} 该读成 {want}，实得 {}",
                got.appearance.turn_duration
            );
        }
        // 缺键走容器的 `#[serde(default)]`：取整份 `Default`，于是没写过这一格就是默认档，
        // 而不是零时长（那等于纸当场落定）。
        let got = load(r#"{"appearance":{"mode":"dark"}}"#);
        assert_eq!(
            got.appearance.turn_duration,
            Appearance::TURN_DURATION_DEFAULT,
            "没写过这一格就是默认档"
        );
        // 非有限值 → 退回默认档而不是夹到最慢那一头。这一条不走 JSON：`NaN` 不是合法
        // JSON 字面量，而 `1e400` 在 serde_json 那侧先炸成读不出键，测不到这一格。
        let nan = Appearance {
            turn_duration: f32::NAN,
            ..Default::default()
        }
        .normalized();
        assert_eq!(
            nan.turn_duration,
            Appearance::TURN_DURATION_DEFAULT,
            "NaN 该退回默认档，而不是被当成「推到最慢」"
        );
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

    /// 认不出的 `page_turn` 回落**初始那一档**（不走动画），而且**同一节里别的键一个都不许
    /// 跟着没**。
    /// 这一格的存在理由是：`PageTurn` 的 `Deserialize` 是自己实现的（同 [`Mode`]、
    /// [`TocDensity`] 那两条），derive 在这里会让整节 `appearance` 退回默认 ——
    /// 浅、深两侧选过的主题名就成了一个拼错的动画键的代价。
    #[test]
    fn an_unknown_page_turn_falls_back_without_taking_the_section_with_it() {
        let got = load(r#"{"appearance":{"mode":"dark","light":"Gruvbox","page_turn":"spin"}}"#);
        assert_eq!(got.appearance.page_turn, PageTurn::None);
        assert_eq!(got.appearance.mode, Mode::Dark);
        assert_eq!(got.appearance.light.as_deref(), Some("Gruvbox"));
        // 认得出的档各自落对：兜底不许把认得出的那几个吃掉。`horizontal` / `vertical` 是
        // 这一档只有两格时写进配置的旧键名，换过版本不该让读者的选项忽然回到默认。
        for (text, want) in [
            ("slide", PageTurn::Slide),
            ("slide_up", PageTurn::SlideUp),
            ("cover", PageTurn::Cover),
            ("curl", PageTurn::Curl),
            ("scroll", PageTurn::Scroll),
            ("none", PageTurn::None),
            ("horizontal", PageTurn::Slide),
            ("vertical", PageTurn::SlideUp),
        ] {
            assert_eq!(
                load(&format!(r#"{{"appearance":{{"page_turn":"{text}"}}}}"#))
                    .appearance
                    .page_turn,
                want,
                "配置里的 {text:?} 落错了档"
            );
        }
        assert_eq!(PageTurn::default(), PageTurn::None);
    }

    /// 认不出的 `highlight_style` 回落默认那档（底色带），而**同一节里别的键一个都不许跟着没**。
    /// 这条与上面那条 `page_turn` 是同一条规矩的第二次发生：`AnnotationStyle` 的
    /// `Deserialize` 也是自己实现的，derive 会让一个拼错的画法键吃掉浅深两侧选过的主题名。
    ///
    /// 顺带钉住那个**两个默认值不许互相串**的分工：配置里缺这一格时读成 `highlight`
    /// （「往后的新条目」），而库里那一列缺值时读成 `line`（「v21 之前那些条」），
    /// 见 `store::annotations` 的 `a_row_written_before_the_style_column_reads_back_as_line`。
    #[test]
    fn an_unknown_highlight_style_falls_back_without_taking_the_section_with_it() {
        let got =
            load(r#"{"appearance":{"mode":"dark","light":"Gruvbox","highlight_style":"zigzag"}}"#);
        assert_eq!(got.appearance.highlight_style, AnnotationStyle::Highlight);
        assert_eq!(got.appearance.mode, Mode::Dark);
        assert_eq!(got.appearance.light.as_deref(), Some("Gruvbox"));
        for (text, want) in [
            ("highlight", AnnotationStyle::Highlight),
            ("line", AnnotationStyle::Line),
            ("wavy", AnnotationStyle::Wavy),
        ] {
            assert_eq!(
                load(&format!(
                    r#"{{"appearance":{{"highlight_style":"{text}"}}}}"#
                ))
                .appearance
                .highlight_style,
                want,
                "配置里的 {text:?} 落错了档"
            );
        }
        // 一份没有这一格的旧配置：读成默认档，而不是让整节 appearance 退回默认。
        let old = load(r#"{"appearance":{"light":"Gruvbox"}}"#);
        assert_eq!(old.appearance.highlight_style, AnnotationStyle::default());
        assert_eq!(old.appearance.light.as_deref(), Some("Gruvbox"));
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

    /// 听书那一节：越界值夹回语义区间、缺节用默认（老配置文件没有这一节），
    /// 关掉的 auto_scroll 原样读回。
    #[test]
    fn the_speech_section_clamps_and_falls_back_to_defaults() {
        let got = load(r#"{"speech":{"rate":9.0,"pitch":0.1,"auto_scroll":false}}"#);
        assert_eq!(
            got.speech.rate, 2.0,
            "语速上限是语义区间的 2.0，不是引擎档的 10.0"
        );
        assert_eq!(got.speech.pitch, 0.5);
        assert!(!got.speech.auto_scroll);

        let missing = load(r#"{"reader":{"font_size":21.0}}"#);
        assert_eq!(
            missing.speech,
            crate::Speech::default(),
            "老文件没有这一节，整节退默认"
        );

        // 整节类型不对（手滑写成字符串）：整节退默认，其余各节照旧。
        let broken = load(r#"{"speech":"快一点"}"#);
        assert_eq!(broken.speech, crate::Speech::default());
        assert!(broken.speech.auto_scroll, "退默认时自动翻页是开着的");
    }
}

#[cfg(test)]
mod sync_config_tests {
    use super::{Config, Sync};

    #[test]
    fn sync_trims_the_address_and_dir_but_not_the_password() {
        let config = Config {
            sync: Sync {
                enabled: true,
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

    #[test]
    fn a_sync_section_round_trips_through_json() {
        let config = Config {
            sync: Sync {
                enabled: true,
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
}
