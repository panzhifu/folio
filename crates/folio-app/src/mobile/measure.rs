//! `folio_core::layout::Measure` 的移动端实现。
//!
//! 语义照着桌面端的 `crates/folio-app/src/measure.rs` 抄：断行按**单字 advance**
//! 累加，行宽/基线按**整段 shaping** 报告，两者必须同源，否则「按 A 算说放得下、
//! 按 B 量却溢出」。
//!
//! 与桌面端唯一的差别是 API 挂在哪：这份 GPUI 快照里 `shape_line` / `layout_line`
//! 只在 `WindowTextSystem` 上（`text_system.rs:402` / `:650`），而 `resolve_font` /
//! `advance` 在 App 级 `TextSystem` 上（`text_system.rs:65` 起的 impl 块）。
//! 所以还是两份句柄，分工与桌面端一字不差。
//! （`WindowTextSystem` 自己 `#[deref]` 到 `Arc<TextSystem>`，理论上能少拿一份，
//! 但显式传参更像是「这就是桌面端那台实现」，也避免以后 deref 目标一变就静默改语义。）

use std::sync::Arc;

use folio_core::layout::{FontKey, LineMetrics, Measure};
use gpui::{
    Font, FontStyle, FontWeight, SharedString, TextRun, TextSystem, WindowTextSystem, black, px,
};

/// CSS 的泛族名。它们解析不出真实字体：`resolve_font` 找不到就叫平台默认，
/// 而绘制路径会按字符串走 fallback 找到真的中文字体 —— 于是「按 A 宽居中、
/// 按 B 宽绘制」。与桌面端同一个结论，所以一律换成读者偏好字体。
const GENERIC_FAMILIES: &[&str] = &[
    "serif",
    "sans-serif",
    "monospace",
    "cursive",
    "fantasy",
    "system-ui",
    "ui-serif",
    "ui-sans-serif",
    "ui-monospace",
    "initial",
    "default",
];

/// `FontKey` → gpui `Font`。**度量与绘制必须走这一个函数**，否则行内偏移与实际字宽
/// 会对不上（桌面端 `measure.rs` 的同名函数就是为这个教训留下的）。
pub fn mobile_font(key: &FontKey, default_family: &SharedString) -> Font {
    let generic = key.family.is_empty()
        || GENERIC_FAMILIES
            .iter()
            .any(|g| key.family.eq_ignore_ascii_case(g));
    let family = if generic {
        default_family.clone()
    } else {
        SharedString::from(key.family.clone())
    };
    Font {
        family,
        weight: FontWeight(key.weight as f32),
        style: if key.italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        },
        ..Default::default()
    }
}

/// 一个 run 的 shaping 描述。`color` 对度量没有影响，但 `TextRun` 要求填，
/// 这里统一给黑：绘制时会按行样式重新传颜色。
fn text_run(text: &str, font: Font) -> TextRun {
    TextRun {
        len: text.len(),
        font,
        color: black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

pub struct MobileMeasure {
    /// 断行用的单字 advance：挂在 App 级 `TextSystem` 上。
    texts: Arc<TextSystem>,
    /// 整段绘制宽度：`layout_line` 只在窗口级文本系统上。
    window_texts: Arc<WindowTextSystem>,
    /// 样式没写 font-family 时用哪个族。来自读者偏好，不是书的 UA 值。
    default_family: SharedString,
}

impl MobileMeasure {
    pub fn new(
        texts: Arc<TextSystem>,
        window_texts: Arc<WindowTextSystem>,
        default_family: impl Into<SharedString>,
    ) -> Self {
        Self {
            texts,
            window_texts,
            default_family: default_family.into(),
        }
    }

    fn font(&self, key: &FontKey) -> Font {
        mobile_font(key, &self.default_family)
    }
}

impl Measure for MobileMeasure {
    fn char_width(&self, ch: char, font: &FontKey, font_size: f32) -> f32 {
        let font_id = self.texts.resolve_font(&self.font(font));
        // 找不到字形时 advance 会报错；给一个 em 宽的占位，让断行继续而不是整段失败。
        self.texts
            .advance(font_id, px(font_size), ch)
            .map(|size| f32::from(size.width))
            .unwrap_or(font_size)
    }

    fn run_metrics(&self, text: &str, font: &FontKey, font_size: f32) -> LineMetrics {
        if text.is_empty() {
            return LineMetrics::default();
        }
        let layout = self.window_texts.layout_line(
            text,
            px(font_size),
            &[text_run(text, self.font(font))],
            None,
        );
        LineMetrics {
            width: f32::from(layout.width),
            ascent: f32::from(layout.ascent),
            descent: f32::from(layout.descent),
        }
    }
}
