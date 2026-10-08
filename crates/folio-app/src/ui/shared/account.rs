//! 账号展示的两个小工具：显示名与圆形头像。
//!
//! 目前是本地占位账号（一个可改的读者名），没有登录。头像不依赖任何图片资产，
//! 用名字首字画一个圆 —— 换机器、缺字体也不会变成空白。

use gpui_kit::component::ActiveTheme;
use gpui_kit::{App, IntoElement, ParentElement, Styled, div, px};

use super::config_model::ConfigHolder;
use super::l10n::tr;

/// 侧栏/设置里显示的读者名；没设过时用「读者」。
pub fn display_name(cx: &App) -> String {
    let name = ConfigHolder::account(cx).name;
    let trimmed = name.trim();
    if trimmed.is_empty() {
        tr(cx, "account.fallback_name").to_string()
    } else {
        trimmed.to_string()
    }
}

/// 圆形头像：背景取主题主色，内容为名字首字。
pub fn account_avatar(name: &str, size: f32, cx: &App) -> impl IntoElement {
    let initial = name
        .trim()
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".into());
    div()
        .size(px(size))
        .flex_shrink_0()
        .rounded_full()
        .bg(cx.theme().primary)
        .text_color(cx.theme().primary_foreground)
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(size * 0.42))
        .child(initial)
}
