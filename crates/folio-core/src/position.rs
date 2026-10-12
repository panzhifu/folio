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

    /// 字符数 → 字节索引，越界时夹到文本末尾。
    pub fn byte_of(&self, source: &str) -> usize {
        source
            .char_indices()
            .nth(self.offset)
            .map(|(b, _)| b)
            .unwrap_or(source.len())
    }

    /// 字节索引 → Locator，并把不在字符边界上的值往前挪，
    /// 否则后续 `&source[range]` 会 panic（中文与 emoji 一定会踩到）。
    pub fn from_byte(chapter: u32, source: &str, byte: usize) -> Self {
        let mut byte = byte.min(source.len());
        while byte > 0 && !source.is_char_boundary(byte) {
            byte -= 1;
        }
        Self {
            chapter,
            offset: source[..byte].chars().count(),
        }
    }

    pub fn clamp_to_chapter(&self, chapter: u32, source: &str) -> Locator {
        let max = source.chars().count();
        Self {
            chapter: self.chapter.min(chapter),
            offset: self.offset.min(max),
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

    /// 读不出字段时**不崩也不猜**：回 `None`，让调用方自己决定当作「没有位置」
    /// 还是「章首」。
    ///
    /// 与 [`Locator::from_json`] 的分工：那个是**恢复阅读位置**那条路，它必须交回
    /// 一个能用的坐标（退回章首是对的 —— 总比打不开书好）。而报告进度这条路上
    /// 「读不出来」与「读到章首」是两件事：前者该没有进度条，后者该显示 0%。
    /// 合成一个数会让一段坏位置串显示成「一字未读」。
    pub fn try_from_json(json: &str) -> Option<Locator> {
        let wire: LocatorWire = serde_json::from_str(json).ok()?;
        if wire.v > LOCATOR_FORMAT_VERSION {
            tracing::warn!(
                from = wire.v,
                to = LOCATOR_FORMAT_VERSION,
                "locator: 位置串比本程序新，认得的字段照读"
            );
        }
        Some(Locator {
            chapter: wire.chapter,
            offset: wire.offset,
        })
    }

    pub fn from_json(json: &str) -> Locator {
        match serde_json::from_str::<LocatorWire>(json) {
            Ok(wire) => {
                if wire.v > LOCATOR_FORMAT_VERSION {
                    tracing::warn!(
                        from = wire.v,
                        to = LOCATOR_FORMAT_VERSION,
                        "locator: 位置串比本程序新，认得的字段照读"
                    );
                } else if wire.v < LOCATOR_FORMAT_VERSION {
                    tracing::debug!(
                        from = wire.v,
                        to = LOCATOR_FORMAT_VERSION,
                        "locator: 旧格式位置串，按当前字段迁移"
                    );
                }
                Locator {
                    chapter: wire.chapter,
                    offset: wire.offset,
                }
            }
            // 连字段都对不上（被截断、被别的程序写过）：退回章首，不崩。
            Err(_) => Locator::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    //! 阅读位置：字符偏移 ↔ 字节索引的互转与越界钳制。只依赖公开 API。

    use crate::Locator;

    const SRC: &str = "中文 abc 表情😀收尾";

    #[test]
    fn char_and_byte_roundtrip() {
        let loc = Locator {
            chapter: 2,
            offset: 5,
        };
        let byte = loc.byte_of(SRC);
        assert_eq!(SRC[..byte].chars().count(), 5);
        assert_eq!(Locator::from_byte(2, SRC, byte), loc);
    }

    #[test]
    fn from_byte_snaps_back_off_a_multibyte_boundary() {
        // 「中」占 3 个字节：byte 1、2 都落在它内部，只能退回 0。
        assert_eq!(Locator::from_byte(0, SRC, 1).offset, 0);
        assert_eq!(Locator::from_byte(0, SRC, 2).offset, 0);
        assert_eq!(Locator::from_byte(0, SRC, 3).offset, 1);
    }

    #[test]
    fn emoji_counts_as_one_char_but_four_bytes() {
        let emoji_at = Locator {
            chapter: 0,
            offset: 9,
        };
        let byte = emoji_at.byte_of(SRC);
        assert_eq!(SRC[byte..].chars().next().unwrap(), '😀');
        assert_eq!(SRC[byte..].chars().next().unwrap().len_utf8(), 4);
        assert_eq!(Locator::from_byte(0, SRC, byte + 1).offset, 9);
    }

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

    #[test]
    fn out_of_range_offsets_clamp() {
        assert_eq!(
            Locator::from_byte(0, "abc", 99),
            Locator {
                chapter: 0,
                offset: 3
            }
        );
        assert_eq!(
            Locator {
                chapter: 9,
                offset: 99
            }
            .clamp_to_chapter(1, "abc"),
            Locator {
                chapter: 1,
                offset: 3
            }
        );
    }
}
