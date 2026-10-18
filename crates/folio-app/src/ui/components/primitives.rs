//! 通用小件：发丝线、次要说明文字、空状态。
//!
//! 它们只回答「这一块长什么样」，不知道自己在哪一屏上。颜色全从 `cx.theme()` 取。

use gpui_kit::component::{ActiveTheme, v_flex};
use gpui_kit::{App, IntoElement, ParentElement, SharedString, Styled, div, px};

/// 一条发丝线（0.5 物理像素），颜色取主题的 `border`。
///
/// 只在**两个同为 `background` 色的区域**之间用 —— 同色块之间没有边界感，
/// 靠这一条线分出上下。区域自己带边框时优先用组件的 `border_b_1()`，
/// 因为那条线归边界拥有者画，两处各画一遍会变成两条并排的实线。
pub fn hairline(cx: &App) -> impl IntoElement {
    div().w_full().h(px(0.5)).bg(cx.theme().border)
}

/// 一行次要说明文字。用于「这一条是元数据，不是主角」的位置：标题栏里的书名、
/// 状态行、空状态的补充说明。
pub fn muted_label(text: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    let text: SharedString = text.into();
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

/// 空状态：一行的目的地名 + 一行「这里还缺什么」。
///
/// 摆一组假数字是错的，只写一句「暂无」也是错的 —— 读者分不清「还没导进来」和
/// 「这台设备上根本不支持」。说什么由调用方给，因为只有那一屏知道缺的是哪一样。
pub fn empty_state(
    title: impl Into<SharedString>,
    note: impl Into<SharedString>,
    cx: &App,
) -> impl IntoElement {
    let title: SharedString = title.into();
    let note: SharedString = note.into();
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .child(div().text_color(cx.theme().foreground).child(title))
        .child(muted_label(note, cx))
}
