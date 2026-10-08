//! `Measure` 的 gpui 实现：断行走 App 级 `TextSystem::line_wrapper`，
//! 整行度量走窗口级 `WindowTextSystem::layout_line`。
//!
//! 两者的分工是 spike① 实测出来的，不是猜的：`line_wrapper` / `advance` 挂在
//! `Arc<TextSystem>` 上（不要求 `&mut Window`，所以能进后台线程），
//! `shape_line` / `layout_line` 挂在 `Arc<WindowTextSystem>` 上。
//!
//! 一个必须同源的细节：**断行用的宽度累加**与**行盒报告的宽度**都来自同一套
//! 单字 advance，否则会出现「按 A 算说放得下、按 B 量却溢出」。

use std::ops::Range;
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
    texts: Arc<TextSystem>,
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
    fn split(
        &self,
        text: &str,
        font: &FontKey,
        font_size: f32,
        avail: f32,
        first_indent: f32,
    ) -> Vec<Range<usize>> {
        if text.is_empty() {
            return vec![0..0];
        }
        let size = px(font_size);
        // `IndentAdjustment` 没有「显式像素缩进」这一档（只有 NoIndent / SameIndent /
        // ExtraColumns），所以首行让出的宽度只能自己扣：每次只重排**剩下的部分**，
        // 第一行按 avail - indent 断，后续行按满宽断。
        // `wrap_line` 是惰性的，取第一个边界只扫到该行末尾，整体仍是线性。
        let mut wrapper = self.texts.line_wrapper(self.font(font), size);
        let mut out = Vec::new();
        let mut start = 0usize;
        let mut indent = first_indent.max(0.0);
        while start < text.len() {
            let rest = &text[start..];
            let width = px((avail - indent).max(1.0));
            let fragments = [LineFragment::text(rest)];
            let first_break = wrapper
                .wrap_line(&fragments, width, IndentAdjustment::NoIndent)
                .next()
                .map(|b| start + b.ix);
            match first_break {
                Some(ix) if ix > start => {
                    out.push(start..ix);
                    start = ix;
                }
                // 一个断点都没有（超长单词或整行放得下）：整段收进最后一行。
                _ => {
                    out.push(start..text.len());
                    break;
                }
            }
            indent = 0.0;
        }
        out
    }

    fn metrics(&self, text: &str, font: &FontKey, font_size: f32) -> LineMetrics {
        if text.is_empty() {
            return LineMetrics::default();
        }
        let layout =
            self.window_texts
                .layout_line(text, px(font_size), &[self.run(text, font)], None);
        LineMetrics {
            width: layout.width.as_f32(),
            ascent: layout.ascent.as_f32(),
            descent: layout.descent.as_f32(),
        }
    }
}
