use serde::{Deserialize, Serialize};

/// 位置串（`positions.locator_json` 那一段文本）的**格式版本**。
///
/// `plans/基础设施.md` §5.1：一切持久化格式带版本戳。这串文本换个写法时 +1；读到
/// **没有 `v`** 的老串 = 版本 0（信封还没加），字段照旧读得出来并迁到当前 ——
/// 「读到旧版本 = 迁移，不是崩溃，也不是静默当新版用」。
pub const LOCATOR_FORMAT_VERSION: u32 = 1;

/// 阅读位置。**存字符数而不是页码或行号**：字号、行距、窗口宽度、
/// 分页还是连续滚动，都会让页码失效，但一章里的第 N 个字不会变。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Locator {
    pub chapter: u32,
    pub offset: usize,
}

/// 落库时套在 [`Locator`] 外面的信封：加一个版本戳，字段名保持稳定。
///
/// 单独一个类型而不是给 `Locator` 直接加字段：`Locator` 是 `Copy` 的小结构、到处按
/// `{ chapter, offset }` 字面量构造，多一个字段会让每一处构造点都要改。版本是**存储格式**
/// 的事，压在这一层即可。
#[derive(Serialize, Deserialize)]
struct LocatorWire {
    /// 缺省 `0`：老串没有这一格，正好被认成「版本 0」。
    #[serde(default)]
    v: u32,
    chapter: u32,
    #[serde(default)]
    offset: usize,
}

impl Locator {
    pub fn start() -> Self {
        Self {
            chapter: 0,
            offset: 0,
        }
    }

    pub fn to_json(&self) -> String {
        let wire = LocatorWire {
            v: LOCATOR_FORMAT_VERSION,
            chapter: self.chapter,
            offset: self.offset,
        };
        // 编码出错不该发生；真出了给一串**带版本戳**的零值，别退回一个没版本的老形状。
        serde_json::to_string(&wire).unwrap_or_else(|_| {
            format!(r#"{{"v":{LOCATOR_FORMAT_VERSION},"chapter":0,"offset":0}}"#)
        })
    }

    /// 两个公开入口共用的解码：读信封、按版本落日志、交回坐标。
    /// `legacy_log`：只有**恢复阅读位置**那条路（`from_json`）要为「旧格式迁移」落
    /// 一条 debug；报告进度那条路（`try_from_json`）读不出就不显示进度条，不该为一条
    /// 老串出声。三条日志的判据与合并前逐字一致。
    fn decode(json: &str, legacy_log: bool) -> Option<Locator> {
        let wire: LocatorWire = serde_json::from_str(json).ok()?;
        if wire.v > LOCATOR_FORMAT_VERSION {
            tracing::warn!(
                from = wire.v,
                to = LOCATOR_FORMAT_VERSION,
                "locator: 位置串比本程序新，认得的字段照读"
            );
        } else if legacy_log && wire.v < LOCATOR_FORMAT_VERSION {
            tracing::debug!(
                from = wire.v,
                to = LOCATOR_FORMAT_VERSION,
                "locator: 旧格式位置串，按当前字段迁移"
            );
        }
        Some(Locator {
            chapter: wire.chapter,
            offset: wire.offset,
        })
    }

    /// 读不出字段时**不崩也不猜**：回 `None`，让调用方自己决定当作「没有位置」
    /// 还是「章首」。
    ///
    /// 与 [`Locator::from_json`] 的分工：那个是**恢复阅读位置**那条路，它必须交回
    /// 一个能用的坐标（退回章首是对的 —— 总比打不开书好）。而报告进度这条路上
    /// 「读不出来」与「读到章首」是两件事：前者该没有进度条，后者该显示 0%。
    /// 合成一个数会让一段坏位置串显示成「一字未读」。
    pub fn try_from_json(json: &str) -> Option<Locator> {
        Self::decode(json, false)
    }

    pub fn from_json(json: &str) -> Locator {
        // 连字段都对不上（被截断、被别的程序写过）：退回章首，不崩。
        Self::decode(json, true).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    //! 阅读位置串的落盘与兼容：版本戳、旧串迁移、坏串兜底。只依赖公开 API。

    use crate::Locator;

    /// §5.1：老的位置串没有版本戳，照样按当前字段读出来 —— 旧版本是**迁移**不是崩溃。
    #[test]
    fn a_versionless_position_string_is_read_as_legacy() {
        assert_eq!(
            Locator::from_json(r#"{"chapter":2,"offset":5}"#),
            Locator {
                chapter: 2,
                offset: 5
            }
        );
    }

    /// 写出去的串带版本戳，且往返一致。
    #[test]
    fn the_position_string_carries_the_format_version() {
        let loc = Locator {
            chapter: 3,
            offset: 7,
        };
        let json = loc.to_json();
        assert!(json.contains("\"v\":1"), "要带版本戳，实际 {json}");
        assert_eq!(Locator::from_json(&json), loc);
    }

    /// 版本比本程序新的串：认得的字段照读，不崩（也不是静默当新版用）。
    #[test]
    fn a_newer_position_string_is_read_not_rejected() {
        assert_eq!(
            Locator::from_json(r#"{"v":99,"chapter":4,"offset":1,"future":true}"#),
            Locator {
                chapter: 4,
                offset: 1
            }
        );
    }

    /// 完全解析不了的串退回章首。
    #[test]
    fn a_garbage_position_string_falls_back_to_start() {
        assert_eq!(Locator::from_json("不是 json"), Locator::default());
    }
}
