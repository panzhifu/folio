//! 读者对图片尺寸那一档。

use serde::{Deserialize, Serialize};

/// 一张图往哪一块尺寸折。
///
/// 三档的差别只在「作者的话与读者的手谁说了算」：
/// - [`Follow`](Self::Follow) 是**作者说了算**：写了 `object-fit` 才允许放大到屏幕宽
///   （`cover` 直接放、够大的 `contain` 也放），多看的 `duokan-bleed` 在这一档生效 ——
///   那本来就是作者说「这张图要铺到屏幕边上」。窄于版心的图不动。
/// - [`Screen`](Self::Screen) 是**默认那一档**，读者主动要大图：**独占一块的图**不看
///   `object-fit` 一律按屏幕宽铺（行内的小图与夹在字里的那张不在此列 —— 把它们拉成
///   满屏会把整段顶走，那不是「大图」而是「排版坏了」）。出血仍生效。
/// - [`Original`](Self::Original) 是「别替我改这张图」：不放大、不出血，
///   只在**超出屏幕**时等比缩回来（不缩就会被切掉，屏幕装不下）。
///
/// 上限那一格在三档里都至少是版心宽：桌面窗口拖窄、外接屏那种「屏比栏还窄」的形状
/// 由 `layout::ports::image_size` 自己取 `max` 兜住。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageScale {
    /// 跟书走：作者说铺满才铺满。
    Follow,
    /// 满屏：**默认那一档**。整块的图一律按屏幕宽铺（小图仍然不动，判据见
    /// `layout::collect::fills_the_screen`）。
    ///
    /// 2026-10-31 从 `Follow` 换过来：读者要的是「书里的照片就该铺满」，而 `Follow`
    /// 把这一格交给作者 —— 公版书与工具导出的 EPUB 绝大多数根本没写 `object-fit`，
    /// 于是整屏照片在阅读器里只剩半屏。「铺满」这一档从来不动小图，换的只是默认值。
    #[default]
    Screen,
    /// 原始尺寸：不放大、不出血，只保证不溢出屏幕。
    Original,
}

impl ImageScale {
    /// 全部档位，按设置界面该出现的顺序。
    pub const ALL: &'static [Self] = &[Self::Follow, Self::Screen, Self::Original];

    /// 配置文件与设置界面用的键名。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Follow => "follow",
            Self::Screen => "screen",
            Self::Original => "original",
        }
    }

    /// 认不出的值回落**初始那一档**（`Self::default()`，不写死第二份）：手改坏
    /// 一个键不该让整节 `Reader` 退回默认。
    pub fn parse(value: &str) -> Self {
        match value {
            "follow" => Self::Follow,
            "screen" => Self::Screen,
            "original" => Self::Original,
            _ => Self::default(),
        }
    }
}

/// 自己实现序列化：derive 碰到认不出的字符串会让**整节**退回默认。
impl Serialize for ImageScale {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ImageScale {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Ok(Self::parse(&text))
    }
}
