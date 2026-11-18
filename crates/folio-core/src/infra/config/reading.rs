use super::ImageScale;
use serde::{Deserialize, Serialize};
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
    /// 正文栏左右留白，各自的像素数；max_measure 另限制行长。
    pub margin_x: f32,
    /// 正文上下留白，各自的像素数；0 跟随书的 @page，非零覆盖作者值。
    pub margin_y: f32,
    pub justify: bool,
    /// 正文 CSS 字重（100–900）；实际字面按最近档匹配。
    pub font_weight: u16,
    /// 排版方向；会改变行盒几何。
    pub text_direction: TextDirection,
    /// 图片尺寸策略；影响行盒，参与排版失效键。
    pub image_scale: ImageScale,
    pub font_family: String,
    pub mono_font_family: String,
    /// 目录那一屏的行高档位。只影响目录，不影响正文 —— 正文的松紧是 `line_height`。
    pub toc_density: TocDensity,
    /// 目录标题最大行数，超出省略。
    pub toc_max_title_lines: u8,
    /// 到过的章在目录里画成浅色。关掉它，目录就只剩「当前在哪」一个信号。
    pub toc_dim_read: bool,
    /// 图集软上限字节数；0 跟随设备分档，当前页可受保护而超限。
    /// 随整份 Reader 进入 LayoutKey，改变此值也会触发重排。
    pub image_cache_bytes: u64,
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
            image_scale: ImageScale::default(),
            font_family: "LXGW WenKai".into(),
            mono_font_family: "monospace".into(),
            toc_density: TocDensity::default(),
            toc_max_title_lines: 2,
            toc_dim_read: true,
            // 0 = 按设备分档（见字段那条注释）。
            image_cache_bytes: 0,
        }
    }
}

impl Reader {
    /// 字号合法区间；设置 UI 与 Reader::clamped 共用。
    pub const FONT_SIZE_MIN: f32 = 9.0;
    pub const FONT_SIZE_MAX: f32 = 72.0;
    /// 行高为字号倍数，至少 1.0。
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
    /// 左右留白合法区间，单位像素。
    pub const MARGIN_X_MIN: f32 = 0.0;
    pub const MARGIN_X_MAX: f32 = 400.0;
    /// 上下留白合法区间，单位像素；0 表示跟随作者 @page。
    pub const MARGIN_Y_MIN: f32 = 0.0;
    pub const MARGIN_Y_MAX: f32 = 400.0;
    /// 字重的合法区间。CSS 的 100–900，但只取整百（字体匹配本来就没有更细的档）。
    pub const FONT_WEIGHT_MIN: u16 = 100;
    pub const FONT_WEIGHT_MAX: u16 = 900;
    /// 设置界面提供的字重档位，实际字体按最近字面匹配。
    pub const FONT_WEIGHT_PRESETS: [u16; 3] = [300, 400, 700];
    /// 图集软上限最多 1 GiB；0 保留为跟随设备分档。
    pub const IMAGE_CACHE_MAX_BYTES: u64 = 1024 * 1024 * 1024;

    /// 将配置字重映射到最近的设置档位，不改变存储值。
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
        // 图集那一档只夹上界：`0` 是「跟随设备」那条约定，不是要抬起来的最小值。
        self.image_cache_bytes = self.image_cache_bytes.min(Self::IMAGE_CACHE_MAX_BYTES);
        self
    }
}
