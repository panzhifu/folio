//! 合成粗体（假粗体）的判据：浏览器会做、gpui 不做的那一格。
//!
//! 单开一间是因为 `paint.rs` 贴着行数基线；判据的理由跟着函数走。

use std::collections::HashMap;

use gpui_kit::{Font, FontWeight, Window};

/// 假粗体：700 已传进 `Font`，但 fontdb 只挑同族最接近的真实面；家族只有 400 时
/// 700 仍落回 400 —— 浏览器会做合成粗体，gpui 不做。判据是「700 与 400 resolve 到
/// 同一个 FontId」：同一个面就叠印一遍，真有粗体面时那一格不同，绝不重复画。
pub(in crate::ui::features::reader) fn needs_synthetic_bold(
    window: &Window,
    font: &Font,
    cache: &mut HashMap<Font, bool>,
) -> bool {
    if font.weight.0 < 600.0 {
        return false;
    }
    if let Some(known) = cache.get(font) {
        return *known;
    }
    let normal = Font {
        weight: FontWeight::NORMAL,
        ..font.clone()
    };
    let bold_id = window.text_system().resolve_font(font);
    let normal_id = window.text_system().resolve_font(&normal);
    let needed = wants_synthetic_bold_when(font, bold_id == normal_id);
    cache.insert(font.clone(), needed);
    needed
}

/// 请求了粗体，而且这个家族只有 regular 那一面。
pub(in crate::ui::features::reader) fn wants_synthetic_bold_when(
    font: &Font,
    same_face: bool,
) -> bool {
    wants_synthetic_bold(font) && same_face
}

fn wants_synthetic_bold(font: &Font) -> bool {
    font.weight.0 >= 600.0
}
