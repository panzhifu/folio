//! 落在绘制/界面层的开关：OpenType 特性、毛玻璃、指针形状、分栏配平。
//!
//! 从 `value.rs` / `apply.rs` 切出来只因为那两个文件贴着行数上限；它们既不是排版期
//! 的几何，也不是层叠本身，单放一间更好读。

use super::computed::{
    ColumnFill, ComputedStyle, Cursor, FontKerning, FontStretch, TextCombine, TextRendering,
};
use super::value::edge_px;

/// 把「解析出来的一档」写进字段；`None` = 这条声明不认识。
///
/// 六个开关的写法都是同一句，抽出来是为了让 `apply.rs` 的 match 各占一行。
pub(super) fn set<T: Copy>(slot: &mut T, value: Option<T>) -> bool {
    match value {
        Some(value) => {
            *slot = value;
            true
        }
        None => false,
    }
}

/// 这一组属性统一在这里分发；`apply.rs` 的 match 因此只留一条兜底臂。
pub(super) fn apply(
    style: &mut ComputedStyle,
    name: &str,
    value: &str,
    own: f32,
    root_px: f32,
) -> Option<bool> {
    Some(match name {
        "font-kerning" => set(&mut style.font_kerning, parse_font_kerning(value)),
        "font-stretch" => set(&mut style.font_stretch, parse_font_stretch(value)),
        "text-combine-upright" | "-webkit-text-combine" => {
            set(&mut style.text_combine, parse_text_combine(value))
        }
        "font-feature-settings" => {
            set_features(&mut style.font_features, parse_font_features(value))
        }
        "text-rendering" => set(&mut style.text_rendering, parse_text_rendering(value)),
        "column-fill" => set(&mut style.column_fill, parse_column_fill(value)),
        "cursor" => set(&mut style.cursor, parse_cursor(value)),
        // 栏间分隔线：与边框三要素同构，解析在 `computed::apply::boxes`。
        name if name.starts_with("column-rule") => {
            super::computed::apply_column_rule_parts(style, name, value, own, root_px)
        }
        "column-span" => super::computed::apply_column_span(style, value),
        "backdrop-filter" | "-webkit-backdrop-filter" => set(
            &mut style.backdrop_blur,
            parse_backdrop_blur(value, own, root_px),
        ),
        _ => return None,
    })
}

/// 特性表是 `Arc<[(String, u32)]>`：这里替调用方做那一步转换。
pub(super) fn set_features(
    slot: &mut std::sync::Arc<[(String, u32)]>,
    value: Option<Vec<(String, u32)>>,
) -> bool {
    match value {
        Some(list) => {
            *slot = list.into();
            true
        }
        None => false,
    }
}

/// `font-stretch`：关键字按档收，百分比按 CSS 的关键字边界就近折档。
///
/// 连续 `wdth` 轴不在这一层表达；折档记在 [`FontStretch`] 的文档上。
pub(super) fn parse_font_stretch(value: &str) -> Option<FontStretch> {
    Some(match value.trim().to_ascii_lowercase().as_str() {
        "ultra-condensed" => FontStretch::UltraCondensed,
        "extra-condensed" => FontStretch::ExtraCondensed,
        "condensed" => FontStretch::Condensed,
        "semi-condensed" => FontStretch::SemiCondensed,
        "normal" => FontStretch::Normal,
        "semi-expanded" => FontStretch::SemiExpanded,
        "expanded" => FontStretch::Expanded,
        "extra-expanded" => FontStretch::ExtraExpanded,
        "ultra-expanded" => FontStretch::UltraExpanded,
        other => {
            let percent = other.strip_suffix('%')?.parse::<f32>().ok()?;
            if !(0.0..=200.0).contains(&percent) {
                return None;
            }
            match percent {
                p if p < 62.5 => FontStretch::UltraCondensed,
                p if p < 75.0 => FontStretch::ExtraCondensed,
                p if p < 87.5 => FontStretch::Condensed,
                p if p < 100.0 => FontStretch::SemiCondensed,
                p if p < 112.5 => FontStretch::Normal,
                p if p <= 125.0 => FontStretch::SemiExpanded,
                p if p <= 150.0 => FontStretch::Expanded,
                p if p < 200.0 => FontStretch::ExtraExpanded,
                _ => FontStretch::UltraExpanded,
            }
        }
    })
}

/// `text-combine-upright` / `-webkit-text-combine`：`none | all | horizontal | digits <n>`。
///
/// 一期只把连续两位 ASCII 数字并格；`digits 3/4` 先记下来，排版仍按两位。
pub(super) fn parse_text_combine(value: &str) -> Option<TextCombine> {
    let value = value.trim().to_ascii_lowercase();
    if value == "none" {
        return Some(TextCombine::None);
    }
    if value == "all" || value == "horizontal" {
        return Some(TextCombine::All);
    }
    let mut parts = value.split_whitespace();
    if parts.next() != Some("digits") {
        return None;
    }
    match parts.next() {
        None => Some(TextCombine::Digits(2)),
        Some(n) => {
            let n = n.parse::<u8>().ok()?;
            (2..=4).contains(&n).then_some(TextCombine::Digits(n))
        }
    }
}

/// `font-kerning: auto | normal | none`。
pub(super) fn parse_font_kerning(value: &str) -> Option<FontKerning> {
    Some(match value.trim().to_ascii_lowercase().as_str() {
        "auto" => FontKerning::Auto,
        "normal" => FontKerning::Normal,
        "none" => FontKerning::None,
        _ => return None,
    })
}

/// `text-rendering`：只有 `optimizeSpeed` 改变塑造行为，其余三档都按默认。
pub(super) fn parse_text_rendering(value: &str) -> Option<TextRendering> {
    Some(match value.trim().to_ascii_lowercase().as_str() {
        "auto" => TextRendering::Auto,
        "optimizespeed" => TextRendering::OptimizeSpeed,
        "optimizelegibility" => TextRendering::OptimizeLegibility,
        "geometricprecision" => TextRendering::GeometricPrecision,
        _ => return None,
    })
}

/// `column-fill: auto | balance`。
pub(super) fn parse_column_fill(value: &str) -> Option<ColumnFill> {
    Some(match value.trim().to_ascii_lowercase().as_str() {
        "auto" => ColumnFill::Auto,
        "balance" => ColumnFill::Balance,
        _ => return None,
    })
}

/// `cursor`：只收 gpui 的 `CursorStyle` 真能兑现的关键字。
pub(super) fn parse_cursor(value: &str) -> Option<Cursor> {
    Some(match value.trim().to_ascii_lowercase().as_str() {
        "auto" => Cursor::Auto,
        "default" => Cursor::Default,
        "pointer" => Cursor::Pointer,
        "text" => Cursor::Text,
        "crosshair" => Cursor::Crosshair,
        "grab" => Cursor::Grab,
        "grabbing" => Cursor::Grabbing,
        "not-allowed" => Cursor::NotAllowed,
        "context-menu" => Cursor::ContextMenu,
        "copy" => Cursor::Copy,
        "alias" => Cursor::Alias,
        "col-resize" => Cursor::ColResize,
        "row-resize" => Cursor::RowResize,
        "vertical-text" => Cursor::VerticalText,
        "e-resize" => Cursor::EResize,
        "w-resize" => Cursor::WResize,
        "n-resize" => Cursor::NResize,
        "s-resize" => Cursor::SResize,
        "ew-resize" => Cursor::EwResize,
        "ns-resize" => Cursor::NsResize,
        "nesw-resize" => Cursor::NeswResize,
        "nwse-resize" => Cursor::NwseResize,
        _ => return None,
    })
}

/// `font-feature-settings`：逗号分隔的 `<string|ident> [<integer>]`，`normal` → 空表。
///
/// 标签必须正好四个 ASCII 字符（OpenType 的 tag）；省略数值按 1（CSS 的默认）。
/// 任一子项解不出就整条作废 —— 悄悄丢掉其中一项，作者会以为它生效了。
pub(super) fn parse_font_features(value: &str) -> Option<Vec<(String, u32)>> {
    let v = value.trim();
    if v.eq_ignore_ascii_case("normal") {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    for part in v.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        let quote = match part.as_bytes()[0] {
            b'"' => Some('"'),
            b'\'' => Some('\''),
            _ => None,
        };
        let (tag, rest) = if let Some(quote) = quote {
            let end = part[1..].find(quote)? + 1;
            (&part[1..end], part[end + 1..].trim())
        } else {
            let mut it = part.splitn(2, char::is_whitespace);
            let tag = it.next()?.trim();
            (tag, it.next().unwrap_or("").trim())
        };
        if tag.len() != 4 || !tag.is_ascii() {
            return None;
        }
        let value = if rest.is_empty() {
            1
        } else {
            rest.parse::<u32>().ok()?
        };
        out.push((tag.to_string(), value));
    }
    Some(out)
}

/// `backdrop-filter`：`none` 或**单个** `blur(<长度>)`。
///
/// 外层 `None` = 整条不认识；内层 `None` = 明确的 `none`（清掉已有值）。
/// 百分比与其它滤镜都不接。
pub(super) fn parse_backdrop_blur(
    value: &str,
    font_size: f32,
    root_font_size: f32,
) -> Option<Option<f32>> {
    let v = value.trim();
    if v.eq_ignore_ascii_case("none") {
        return Some(None);
    }
    let low = v.to_ascii_lowercase();
    let inner = low.strip_prefix("blur(")?.strip_suffix(')')?;
    let width = edge_px(inner.trim(), font_size, root_font_size)?;
    Some(Some(width.max(0.0)))
}
