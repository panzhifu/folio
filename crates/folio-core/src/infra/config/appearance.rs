use folio_model::types::AnnotationStyle;
use serde::{Deserialize, Serialize};
/// 浅深模式；System 跟随操作系统。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    System,
    Light,
    Dark,
    /// 水墨：宣纸底、近黑的墨、去彩度的插图，**不**跟随系统翻面。
    ///
    /// 它排在浅/深旁边而不是「浅侧的一本书」：浅深那一档只答「取哪一侧」，而水墨是一档
    /// 固定的样子 —— 落在主题名上就选不到它、也进不了按书差量那一格。
    Ink,
}

impl Mode {
    /// 配置文件与设置界面用的键名。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
            Self::Ink => "ink",
        }
    }

    /// 认不出的值回落 `System`：手改坏的一个键不该让界面进入未知状态。
    pub fn parse(value: &str) -> Self {
        match value {
            "light" => Self::Light,
            "dark" => Self::Dark,
            "ink" => Self::Ink,
            _ => Self::System,
        }
    }

    /// 「这一档没被单独选过、跟随上一层」在界面选项里的写法。
    ///
    /// 下拉与菜单的选项值只有字符串一种形状，而 [`Appearance::reading_mode`] 是
    /// `Option<Mode>` —— `None` 需要一个自己的键名，否则「跟随软件」在界面上就没处放。
    /// 这个名字**不与配置文件的 JSON 值混用**：落盘那一侧 `None` 就是不写这一格。
    pub const FOLLOW_APP: &'static str = "follow_app";

    /// 界面选项的值 → 阅读浅深那一格。`Self::FOLLOW_APP` 以外都按 [`parse`](Self::parse)
    /// 解释（认不出的落到 `Some(System)`，与配置文件那一侧同一口径）。
    pub fn parse_reading(value: &str) -> Option<Self> {
        (value != Self::FOLLOW_APP).then(|| Self::parse(value))
    }

    /// 阅读浅深那一格 → 界面选项的值。与 [`parse_reading`](Self::parse_reading) 互为逆。
    pub fn reading_value(mode: Option<Self>) -> &'static str {
        match mode {
            None => Self::FOLLOW_APP,
            Some(mode) => mode.as_str(),
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
    /// 水墨那一档实际走哪一种动效：任何「一张纸在走」的档都落成**当场落定**。
    ///
    /// `Scroll` 与 `None` 原样返回 —— 前者是摆法（连续滚动），不是翻页动效；
    /// 后者本来就不动。这一问不碰配置，读者选的档位照旧存着，退出水墨就回来。
    pub fn still(self) -> Self {
        match self {
            Self::Scroll | Self::None => self,
            _ => Self::None,
        }
    }

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

/// 应用外观与阅读页覆盖；版式参数由 Reader 持有。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub mode: Mode,
    /// 阅读页浅深：None 跟随软件，Some(System) 跟随系统；只影响外观，不触发重排。
    /// 此项全局有效，旧书内差量由 APPEARANCE_GLOBAL_ONLY 排除。
    pub reading_mode: Option<Mode>,
    /// 解析到浅色时用的主题名。
    pub light: Option<String>,
    /// 解析到深色时用的主题名。
    pub dark: Option<String>,
    /// 正文主题；None 跟随壳主题。主题自带日夜配色，此项不参与排版。
    /// 全局设置，旧书内覆盖由 APPEARANCE_GLOBAL_ONLY 排除。
    pub reading_theme: Option<String>,
    /// 正文翻页沿哪根轴平移（见 [`PageTurn`] 为什么这一格不在 `Reader` 里）。
    pub page_turn: PageTurn,
    /// 翻页动画毫秒数；跟手阶段为 1:1，松手惯性阶段以此为时长上限。
    pub turn_duration: f32,
    /// 新批注的默认画法，不改既有批注；不参与排版。
    /// 默认 Highlight；数据库旧记录默认 line 是迁移语义，二者不可混用。
    pub highlight_style: AnnotationStyle,
    /// 自动前进速度，单位页/屏每分钟；连续模式换算为像素速度，分页模式换算为间隔。
    pub auto_advance_speed: f32,
    /// 阅读亮度；None 跟随系统。经 host 调节，不参与排版失效键。
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

    /// 自动前进默认速度，单位页/屏每分钟。
    pub const AUTO_ADVANCE_DEFAULT: f32 = 12.0;
    /// 下限：再慢就不是「在前进」了，读者会忘了它开着。
    pub const AUTO_ADVANCE_MIN: f32 = 2.0;
    /// 上限：一秒半一页已经读不成句子，再快不如自己翻。
    pub const AUTO_ADVANCE_MAX: f32 = 40.0;

    /// 空串按「没设」处理：`""` 既不是合法主题名，又会让下拉框显示成空白。
    pub fn normalized(mut self) -> Self {
        for name in [&mut self.light, &mut self.dark, &mut self.reading_theme] {
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
        // 自动前进：非有限值退回默认档，越界夹进量程 —— 与翻页时长同一条规矩。
        self.auto_advance_speed = if !self.auto_advance_speed.is_finite() {
            Self::AUTO_ADVANCE_DEFAULT
        } else {
            self.auto_advance_speed
                .clamp(Self::AUTO_ADVANCE_MIN, Self::AUTO_ADVANCE_MAX)
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
            reading_mode: None,
            light: None,
            dark: None,
            reading_theme: None,
            page_turn: PageTurn::default(),
            auto_advance_speed: Self::AUTO_ADVANCE_DEFAULT,
            turn_duration: Self::TURN_DURATION_DEFAULT,
            highlight_style: AnnotationStyle::default(),
            brightness: None,
        }
    }
}

/// 正文主题背景图；按主题名保存，不进入书内差量。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeBackground {
    /// 日侧（浅色那半边）的背景图文件名。
    pub light: Option<String>,
    /// 夜侧（深色那半边）的背景图文件名。
    pub dark: Option<String>,
    /// 图铺到纸上的不透明度，0.0–1.0。纸纹类的小图通常配个低不透明度，
    /// 让它垫在墨色底下而不是盖在墨色上面。
    pub opacity: f32,
}

impl ThemeBackground {
    /// 满不透明度。滑条与归一共用这一个原点。
    pub const OPACITY_FULL: f32 = 1.0;
    /// 不透明度滑条的下限：再往下就是「看不见」，那一档由「清掉图」表达。
    pub const OPACITY_MIN: f32 = 0.05;

    /// 非有限值退回满不透明度（与 `turn_duration` 那条同一口径：坏数据不当成选择），
    /// 其余夹进区间。
    pub fn normalized_opacity(value: f32) -> f32 {
        if !value.is_finite() {
            Self::OPACITY_FULL
        } else {
            value.clamp(Self::OPACITY_MIN, Self::OPACITY_FULL)
        }
    }
}

impl Default for ThemeBackground {
    /// 手写而不是 derive：`opacity` 的默认是**满不透明**，不是浮点的零值 ——
    /// derive 出来的那份会把每张背景图读成全透明，读者只会觉得「这功能是坏的」。
    fn default() -> Self {
        Self {
            light: None,
            dark: None,
            opacity: Self::OPACITY_FULL,
        }
    }
}
