//! `@media` 宽度条件：解析、判定、视口筛选。
//!
//! 从 `css::mod` 切出来只因为 mod.rs 贴着行数上限；对外路径经 `pub use` 保持不变。

use crate::css::split_top_level;

/// 一条 `@media` 里**能判**的那部分：逗号列表任一成立即命中。
///
/// 只判 `width` / `min-width` / `max-width`（`device-width` 同义）。`orientation`、
/// `color`、`hover` 这些判不了的条件按成立处理 —— 与从前「判不了照收」同一条降级，
/// 不能因为手机上没有陀螺仪就把整块版面丢掉。
///
/// `alternatives` 为空表示这条 `@media` 里没有可判条件（`screen`、`all`、`print`
/// 已在解析期处理完），永远成立。
#[derive(Debug, Clone, PartialEq)]
pub struct MediaCondition {
    alternatives: Vec<MediaRange>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct MediaRange {
    min: Option<f32>,
    max: Option<f32>,
}

impl MediaCondition {
    /// 视口宽（CSS px，逻辑像素）下这条条件成不成立。边界按 CSS 的闭区间。
    pub fn matches(&self, viewport_px: f32) -> bool {
        self.alternatives.is_empty()
            || self.alternatives.iter().any(|range| {
                range.min.is_none_or(|min| viewport_px >= min)
                    && range.max.is_none_or(|max| viewport_px <= max)
            })
    }

    /// 嵌套 `@media` 是交集：两条条件的可选区间两两组合。
    pub(crate) fn and(&self, other: &Self) -> Self {
        if self.alternatives.is_empty() {
            return other.clone();
        }
        if other.alternatives.is_empty() {
            return self.clone();
        }
        let mut alternatives = Vec::new();
        for a in &self.alternatives {
            for b in &other.alternatives {
                alternatives.push(MediaRange {
                    min: match (a.min, b.min) {
                        (Some(x), Some(y)) => Some(x.max(y)),
                        (x, y) => x.or(y),
                    },
                    max: match (a.max, b.max) {
                        (Some(x), Some(y)) => Some(x.min(y)),
                        (x, y) => x.or(y),
                    },
                });
            }
        }
        Self { alternatives }
    }
}

/// `@media` 的判定结果。宽度条件不能在解析期判定，只能原样挂到规则上。
pub(crate) enum MediaDecision {
    /// 全是要打印的查询：整块丢掉（与从前一样记入诊断）。
    Skip,
    /// 没有可判条件（或含 `screen` / `all`）：照收，不挂条件。
    Always,
    Conditional(MediaCondition),
}

/// 把 `@media` 后面的查询串折成「跳过 / 无条件 / 宽度条件」。
///
/// 查不了的类型与特性一律按成立处理：本机没有哪个阅读器能判 `hover` / `pointer`，
/// 按不成立会把一整块响应式版面直接删掉。真正能判的只有宽度。
pub(crate) fn media_decision(query: &str) -> MediaDecision {
    let mut alternatives = Vec::new();
    let mut unconditional = false;
    for part in split_top_level(query, ',') {
        let low = part.trim().to_ascii_lowercase();
        if low.is_empty() {
            unconditional = true;
            continue;
        }
        // `not screen` 是「非屏幕」，`not print` 才是要给阅读器的；`only` 只是
        // 给老浏览器的噪音。
        let (negated, rest) = match low.strip_prefix("not ") {
            Some(rest) => (true, rest.trim_start()),
            None => (false, low.as_str()),
        };
        let rest = rest.strip_prefix("only ").unwrap_or(rest).trim_start();
        let type_end = rest
            .find(|c: char| c.is_whitespace() || c == '(')
            .unwrap_or(rest.len());
        let media_type = &rest[..type_end];
        let rest = rest[type_end..].trim_start();
        match media_type {
            "print" => {
                if !negated {
                    continue;
                }
                unconditional = true;
                continue;
            }
            // 不认识的媒体类型（`speech` 之类）判不了，照收。
            "" | "screen" | "all" => {
                // `not` 后面跟了真类型才是「非这个媒体」；`not (min-width…)`
                // 是否定整个条件，本版不判，落回「照收」。
                if negated && !media_type.is_empty() {
                    continue;
                }
            }
            _ => {
                unconditional = true;
                continue;
            }
        }

        let mut range = MediaRange {
            min: None,
            max: None,
        };
        let mut saw_width = false;
        let mut tail = rest;
        while let Some(open) = tail.find('(') {
            let Some(close) = tail[open..].find(')').map(|at| open + at) else {
                break;
            };
            let feature = &tail[open + 1..close];
            tail = &tail[close + 1..];
            let Some((name, value)) = feature.split_once(':') else {
                continue;
            };
            let Some(px) = media_length_px(value.trim()) else {
                continue;
            };
            match name.trim() {
                "min-width" | "min-device-width" => {
                    range.min = Some(range.min.map_or(px, |known| known.max(px)));
                    saw_width = true;
                }
                "max-width" | "max-device-width" => {
                    range.max = Some(range.max.map_or(px, |known| known.min(px)));
                    saw_width = true;
                }
                "width" | "device-width" => {
                    range.min = Some(px);
                    range.max = Some(px);
                    saw_width = true;
                }
                _ => {}
            }
        }
        if saw_width {
            alternatives.push(range);
        } else {
            // 这一段只剩判不了的条件（`orientation` 等），按成立处理。
            unconditional = true;
        }
    }
    if unconditional {
        MediaDecision::Always
    } else if alternatives.is_empty() {
        MediaDecision::Skip
    } else {
        MediaDecision::Conditional(MediaCondition { alternatives })
    }
}

/// `@media` 里的长度：只收 px / em / rem。`em` 在媒体查询里相对**初始字号**
/// （16px），不是正文当前字号 —— 用它折正文的 `em` 会随读者调字号把断点一起拖走。
fn media_length_px(value: &str) -> Option<f32> {
    let value = value.trim();
    if let Some(v) = value.strip_suffix("px") {
        return v.trim().parse().ok();
    }
    if let Some(v) = value.strip_suffix("rem") {
        return v.trim().parse::<f32>().ok().map(|n| n * 16.0);
    }
    if let Some(v) = value.strip_suffix("em") {
        return v.trim().parse::<f32>().ok().map(|n| n * 16.0);
    }
    None
}
