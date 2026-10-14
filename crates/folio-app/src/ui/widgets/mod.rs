//! 两端共用的小件：把「同一段排版结果在两个平台上都长一样」这件事里，
//! 真正只有颜色与措辞差别的部分收在一处。
//!
//! 判据是**这段逻辑两端会不会各写一遍然后写歪**：`folio-core` 的 `Color` 是 0–255 的
//! u8 三元组，gpui 要 0–1 的浮点 `Rgba`，这个换算一旦有两份，某一端的作者样式颜色
//! 就会整体偏亮或偏暗，而这种差别只有在并排看两本书时才发现。
//!
//! 这里的一切颜色都从 `cx.theme()` 取，不留裸色值：设计规范要求应用层不出现
//! hex / `rgb(...)`，缺哪个语义角色就在主题层补，而不是在调用点嵌一个调色板值。

mod bottom_sheet;
mod search_box;
mod top_bar;

pub use bottom_sheet::{BottomSheet, BottomSheetEvent, BottomSheetState};
pub use search_box::{SearchBox, SearchBoxEvent};
pub use top_bar::TopBar;

use folio_core::content::format::css::computed::Color;
use gpui_kit::component::{ActiveTheme, v_flex};
use gpui_kit::{App, Hsla, IntoElement, ParentElement, SharedString, Styled, div, px};

/// 行样式的颜色 → gpui 颜色。样式没写 `color` 时用调用方给的正文色 ——
/// 也就是**当前主题**的那个，而不是一个写死的墨色。
pub fn run_color(color: Option<Color>, fallback: Hsla) -> Hsla {
    color.map_or(fallback, |c| {
        gpui_kit::Rgba {
            r: c.0 as f32 / 255.0,
            g: c.1 as f32 / 255.0,
            b: c.2 as f32 / 255.0,
            a: 1.0,
        }
        .into()
    })
}

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
