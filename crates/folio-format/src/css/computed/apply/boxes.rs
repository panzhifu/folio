//! 多栏容器一族（`columns` 简写与 `column-count` / `column-width`）的取值解析。
//!
//! 从 `apply` 那一张 match 里分出来只因为 apply.rs 贴着行数上限；
//! 与 `values.rs` 同一条路：仅供 `apply` 内部调用，对外路径不变。

use super::super::ComputedStyle;
use crate::css::value::edge_length;

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
