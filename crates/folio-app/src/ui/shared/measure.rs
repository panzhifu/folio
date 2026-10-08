//! `Measure` 的 gpui 实现：断行走 App 级 `TextSystem::line_wrapper`，
//! 整行度量走窗口级 `WindowTextSystem::layout_line`。
//!
//! 两者的分工是 spike① 实测出来的，不是猜的：`line_wrapper` / `advance` 挂在
//! `Arc<TextSystem>` 上（不要求 `&mut Window`，所以能进后台线程），
//! `shape_line` / `layout_line` 挂在 `Arc<WindowTextSystem>` 上。
//!
//! 一个必须同源的细节：**断行用的宽度累加**与**行盒报告的宽度**都来自同一套
//! 单字 advance，否则会出现「按 A 算说放得下、按 B 量却溢出」。
//!
//! 桌面与移动**共用这一份**。原先移动端有另一份逐行照抄的实现，唯一的区别是
//! `f32::from(x)` 还是 `x.as_f32()`；两件事因此成立：一处改了断行口径两端同时生效，
//! 以及同一本书在两端折出的行数可以逐行比对（`tests/mobile.rs` 就靠这个前提）。

use std::sync::Arc;

use folio_core::layout::{FontKey, LineMetrics, Measure};
use gpui_kit::*;

/// CSS 的泛族名。它们在 gpui 里**解析不出真实字体**：`TextSystem::resolve_font`
/// 找不到就叫平台默认（一个拉丁字体，汉字量到的是 .notdef 的宽度），
/// 而绘制路径会按字符串走 fallback 找到真的中文字体 —— 于是「按 A 宽居中、
/// 按 B 宽绘制」，标题就歪到版心外了。
/// 阅读器也不该让书的 `serif` 盖掉读者选的字体，所以一律换成偏好字体。
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

/// `FontKey` → gpui `Font`。**度量与绘制必须走这一个函数**：
/// 两边各自拼 Font 的话，一次字段改动就会让居中偏移与实际字宽对不上。
pub fn gpui_font(key: &FontKey, default_family: &SharedString) -> Font {
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

pub struct GpuiMeasure {
    /// 断行用的单字 advance：挂在 App 级 `TextSystem` 上。
    texts: Arc<TextSystem>,
    /// 整段绘制宽度：`layout_line` 只在窗口级文本系统上。
    window_texts: Arc<WindowTextSystem>,
    /// 样式没写 font-family 时用哪个族。来自读者偏好，不是书的 UA 值。
    default_family: SharedString,
}

impl GpuiMeasure {
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
        gpui_font(key, &self.default_family)
    }

    /// 一个 run 的 shaping 描述。`color` 对度量没有影响，但 `TextRun` 要求填，
    /// 这里统一给黑：绘制时会按行样式重新传颜色。
    fn run(&self, text: &str, key: &FontKey) -> TextRun {
        TextRun {
            len: text.len(),
            font: self.font(key),
            color: black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }
    }
}

impl Measure for GpuiMeasure {
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
        let layout =
            self.window_texts
                .layout_line(text, px(font_size), &[self.run(text, font)], None);
        LineMetrics {
            width: f32::from(layout.width),
            ascent: f32::from(layout.ascent),
            descent: f32::from(layout.descent),
        }
    }
}
