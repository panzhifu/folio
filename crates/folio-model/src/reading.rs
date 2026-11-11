//! 阅读偏好词汇：读者在设置里选的那一档档位，与每本书自己的设置差量。
//!
//! 这里只放**多个消费方共用的数据**：`ImageScale` 由 format 的样式计算、core 的配置与
//! app 的设置页共用；[`BookSettings`] 由 core 的书库（落库列）、同步（快照记录）与
//! app 的书内设置共用 —— 都按 folio-model 的规矩进来。
//! **serde 表示是契约**：配置文件里那格键名（`"follow"` / `"screen"` / `"original"`）
//! 逐字节不动。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

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
/// 由排版侧（folio-core 的 `layout`）自己取 `max` 兜住。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageScale {
    /// 跟书走：作者说铺满才铺满。
    Follow,
    /// 满屏：**默认那一档**。整块的图一律按屏幕宽铺（小图仍然不动）。
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

// 自己实现序列化：derive 碰到认不出的字符串会让**整节**退回默认。
// 与 `AnnotationStyle` 共用 lib.rs 里 `serde_as_string!` 那一份口径。
serde_as_string!(ImageScale);

/// 一本书的阅读设置**差量**（原 folio-core::infra::book_settings 的数据半边，
/// 2026-11-12 阶段二随同步记录下沉——快照的 `SettingsRecord` 要带着它走）。
///
/// 存的是「这一本与全局默认不同的那几格」，不是整份设置：没动过的格子永远读默认，
/// 于是设置屏里改一次默认，能传导到所有没单独设过的书；读者真调过的那几格才留在
/// 那一本上。抹掉差量就是「恢复本书默认」，不需要知道这本书曾经设过哪些格。
///
/// 这里只有数据与库列的编解码；「差量盖到全局默认上」「记下这一本现在的生效值」
/// 那些**咬着 `Reader` / `Appearance` 的行为**留在 folio-core（settings 层在 core），
/// 经 `BookSettingsExt` 扩展 trait 挂回这个类型 —— 与 folio-format::ext 同一条画法。
///
/// 用 `BTreeMap` 而不是 `serde_json::Map`：同一份设置要能稳定序列化成同样的字节
/// （同步比对、测试断言都靠这个），而 `serde_json::Map` 默认保留插入顺序 ——
/// 读者先拖字号再推行距，与反过来拖，落库的字符串本该是同一个。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BookSettings {
    /// 与默认不同的 `Reader` 格子。
    pub reader: BTreeMap<String, Value>,
    /// 与默认不同的 `Appearance` 格子（哪些格只归全局由 core 那侧的排除表管）。
    pub appearance: BTreeMap<String, Value>,
}

impl BookSettings {
    /// 这本书完全跟随默认。
    pub fn is_empty(&self) -> bool {
        self.reader.is_empty() && self.appearance.is_empty()
    }

    /// 整份抹掉 —— 阅读弹层里「恢复本书默认」那一格走的就是这个。
    pub fn clear(&mut self) {
        self.reader.clear();
        self.appearance.clear();
    }

    /// 落库那一列的写法。空差量存**空串**，与 `books.css` / `books.toc_json` 那几列
    /// 「没有内容就留空」同一口径，也让「这本书没单独设过」在 SQL 里一眼可见
    /// （`settings_json = ''` 就能数出来有几本被单独调过）。
    pub fn to_column(&self) -> String {
        if self.is_empty() {
            String::new()
        } else {
            // 序列化自家这个结构不会失败（只有 map 与 Value）；真失败就当没设过，
            // 但不能安静 —— 那会把「读者调过的版心」变成「从来没调过」。
            serde_json::to_string(self).unwrap_or_else(|err| {
                tracing::warn!("book_settings: 差量写不成 JSON，按没设过存：{err}");
                String::new()
            })
        }
    }

    /// 读回库里那一列。空串与解不开的垃圾都是「这一本没单独设过」，正文照读
    /// —— 与 `toc_json` 解不开只当没有目录同一口径，但日志必须留一句。
    pub fn from_column(raw: &str) -> Self {
        if raw.is_empty() {
            return Self::default();
        }
        match serde_json::from_str(raw) {
            Ok(settings) => settings,
            Err(err) => {
                tracing::warn!("book_settings: 那一列不是差量 JSON，按没设过读：{err}");
                Self::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{BookSettings, ImageScale};

    /// serde 表示是契约：配置文件里那三个键名一个都不能变，认不出的值回落
    /// 初始档而不是让整节配置作废。
    #[test]
    fn config_keys_round_trip_and_unknown_falls_back() {
        for (key, scale) in [
            ("follow", ImageScale::Follow),
            ("screen", ImageScale::Screen),
            ("original", ImageScale::Original),
        ] {
            let parsed: ImageScale = serde_json::from_value(key.into()).unwrap();
            assert_eq!(parsed, scale);
            assert_eq!(serde_json::to_value(scale).unwrap(), key);
        }
        let broken: ImageScale = serde_json::from_value("cinemascope".into()).unwrap();
        assert_eq!(broken, ImageScale::default(), "坏键回落默认档");
    }

    /// 设置界面的档位顺序：`ALL` 是「界面上该出现的顺序」的唯一事实。
    #[test]
    fn all_lists_the_three_scales_in_ui_order() {
        assert_eq!(
            ImageScale::ALL,
            &[ImageScale::Follow, ImageScale::Screen, ImageScale::Original]
        );
    }

    /// 库里那一列：空串 = 没设过，垃圾 = 没设过，非空差量原样回来。
    /// （差量的**内容怎么算**在 core 那侧验；这里钉的是编解码本身的口径。）
    #[test]
    fn the_column_round_trips_and_reads_garbage_as_unset() {
        let settings = BookSettings {
            reader: BTreeMap::from([("font_size".to_string(), serde_json::json!(19.0))]),
            appearance: BTreeMap::new(),
        };

        let raw = settings.to_column();
        assert!(!raw.is_empty());
        assert_eq!(BookSettings::from_column(&raw), settings);

        assert!(BookSettings::from_column("").is_empty());
        assert!(BookSettings::from_column("{ not json").is_empty());
        assert!(
            BookSettings::default().to_column().is_empty(),
            "空差量该存空串"
        );
    }

    /// 同一份设置不分先后：先拖字号还是先推行距，落库的字节是同一个。
    #[test]
    fn the_column_is_order_independent() {
        let a = BookSettings {
            reader: BTreeMap::from([
                ("font_size".to_string(), serde_json::json!(20.0)),
                ("line_height".to_string(), serde_json::json!(2.0)),
            ]),
            appearance: BTreeMap::new(),
        };
        let b = BookSettings {
            reader: BTreeMap::from([
                ("line_height".to_string(), serde_json::json!(2.0)),
                ("font_size".to_string(), serde_json::json!(20.0)),
            ]),
            appearance: BTreeMap::new(),
        };
        assert_eq!(a.to_column(), b.to_column());
    }
}
