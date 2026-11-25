//! 多栏容器一族（`columns` 简写与 `column-count` / `column-width`）的取值解析。
//!
//! 从 `apply` 那一张 match 里分出来只因为 apply.rs 贴着行数上限；
//! 与 `values.rs` 同一条路：仅供 `apply` 内部调用，对外路径不变。
//! `column-rule` 一族也在这里 —— 它和边框共用三要素的解析，只是落在栏缝上。

use super::super::{BorderEdge, BorderStyle, ColumnSpan, ComputedStyle};
use crate::css::value::edge_length;
use crate::css::value::{border_width_px, parse_border_style, parse_color, split_value_tokens};

/// `columns: <'column-width'> || <'column-count'>` 简写：一段栏宽、一段栏数，
/// 次序随意，`auto` 是「这一段没写」。任一段出现两次整条作废。
pub(super) fn apply_columns(
    style: &mut ComputedStyle,
    value: &str,
    own: f32,
    root_px: f32,
) -> bool {
    let mut count = None;
    let mut width = None;
    for token in value.split_whitespace() {
        let low = token.to_ascii_lowercase();
        if low == "auto" {
            continue;
        }
        if let Some(n) = low.parse::<u16>().ok().filter(|n| (1..=1000).contains(n)) {
            if count.is_some() {
                return false;
            }
            count = Some(n);
            continue;
        }
        match edge_length(&low, own, root_px, false) {
            Some(l) if width.is_none() => width = Some(l),
            _ => return false,
        }
    }
    // 整条 `auto` / 空值与初值同义，但一条声明至少要认出一段形态才算接住。
    if count.is_none() && width.is_none() {
        return false;
    }
    style.column_count = count;
    style.column_width = width;
    true
}

/// `column-count: <integer>`。CSS 只收正整数；`auto` 拉回「没写」。
pub(super) fn apply_column_count(style: &mut ComputedStyle, value: &str) -> bool {
    let low = value.trim().to_ascii_lowercase();
    if low == "auto" {
        style.column_count = None;
        return true;
    }
    match low.parse::<u16>() {
        Ok(n) if (1..=1000).contains(&n) => {
            style.column_count = Some(n);
            true
        }
        _ => false,
    }
}

/// `column-rule` 一族的属性名分派。四臂合成一条 `starts_with` 守卫，
/// 让 `apply.rs` 那张已经贴着行数上限的 match 只多占一行。
pub(in crate::css) fn apply_column_rule_parts(
    style: &mut ComputedStyle,
    name: &str,
    value: &str,
    own: f32,
    root_px: f32,
) -> bool {
    match name {
        "column-rule" => apply_column_rule(style, value, own, root_px),
        "column-rule-width" => apply_column_rule_width(style, value, own, root_px),
        "column-rule-style" => apply_column_rule_style(style, value),
        "column-rule-color" => apply_column_rule_color(style, value),
        _ => false,
    }
}

/// `column-span: none | all`。只收这两个值，`all` 让元素跨过所有栏。
pub(in crate::css) fn apply_column_span(style: &mut ComputedStyle, value: &str) -> bool {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" => {
            style.column_span = ColumnSpan::None;
            true
        }
        "all" => {
            style.column_span = ColumnSpan::All;
            true
        }
        _ => false,
    }
}

/// `column-rule: <width> || <style> || <color>` 简写。三要素顺序随意，缺的按 CSS
/// 初值（`medium none currentColor`）重置 —— 与 `border` 简写同一条规矩，不保留旧值。
/// 条数上限 3（三要素各一次）；重复或认不出的 token 整条作废。
pub(super) fn apply_column_rule(
    style: &mut ComputedStyle,
    value: &str,
    own: f32,
    root_px: f32,
) -> bool {
    let parts = split_value_tokens(value);
    if parts.is_empty() || parts.len() > 3 {
        return false;
    }
    let mut width = 3.0f32; // `medium`
    let mut line = BorderStyle::None;
    let mut color = None;
    let mut seen_width = false;
    let mut seen_style = false;
    let mut seen_color = false;
    for part in &parts {
        if !seen_width && let Some(px) = border_width_px(part, own, root_px) {
            width = px;
            seen_width = true;
        } else if !seen_style && let Some(kind) = parse_border_style(part) {
            line = kind;
            seen_style = true;
        } else if !seen_color
            && (part.eq_ignore_ascii_case("transparent")
                || part.eq_ignore_ascii_case("currentcolor"))
        {
            seen_color = true;
        } else if !seen_color && let Some(parsed) = parse_color(part) {
            color = Some(parsed);
            seen_color = true;
        } else {
            return false;
        }
    }
    style.column_rule = BorderEdge {
        width,
        style: line,
        color,
    };
    true
}

/// `column-rule-width`：`thin|medium|thick` 或长度，不收百分比（CSS 同 border-width）。
pub(super) fn apply_column_rule_width(
    style: &mut ComputedStyle,
    value: &str,
    own: f32,
    root_px: f32,
) -> bool {
    match border_width_px(value.trim(), own, root_px) {
        Some(px) => {
            style.column_rule.width = px;
            true
        }
        None => false,
    }
}

/// `column-rule-style`：边框那八档线型，`none` 是初值。
pub(super) fn apply_column_rule_style(style: &mut ComputedStyle, value: &str) -> bool {
    match parse_border_style(value.trim()) {
        Some(kind) => {
            style.column_rule.style = kind;
            true
        }
        None => false,
    }
}

/// `column-rule-color`：`transparent` / `currentColor` 落 `None`（绘制侧取本块字色）。
pub(super) fn apply_column_rule_color(style: &mut ComputedStyle, value: &str) -> bool {
    let v = value.trim();
    if v.eq_ignore_ascii_case("transparent") || v.eq_ignore_ascii_case("currentcolor") {
        style.column_rule.color = None;
        return true;
    }
    match parse_color(v) {
        Some(color) => {
            style.column_rule.color = Some(color);
            true
        }
        None => false,
    }
}

/// `column-width: <length> | auto`。`none` 不是这一族的值，照旧判不认识。
pub(super) fn apply_column_width(
    style: &mut ComputedStyle,
    value: &str,
    own: f32,
    root_px: f32,
) -> bool {
    let low = value.trim().to_ascii_lowercase();
    if low == "auto" {
        style.column_width = None;
        return true;
    }
    match edge_length(&low, own, root_px, false) {
        Some(l) => {
            style.column_width = Some(l);
            true
        }
        None => false,
    }
}
