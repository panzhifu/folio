//! Compact 档的导航栏：底部四格，每格**图标在上、文字在下**。
//!
//! 不用组件的 `Button`：它把内容排成一条横的 `h_flex`（`label` 与 `icon` 并排），
//! 而且非 link/text 变体还强制 `h_6`，塞不下上下两行。手机底栏要的正是竖排 + 大命中区，
//! 所以这里自绘一格：`v_flex` 里放一个 `Icon` 和一行小字，整格 `.id()` + `.on_click()`
//! 接住点击（`div` 自带这套交互，见 `account_view` 的行）。
//!
//! **换页的提示只有一处：颜色。** 选中那格图标与文字用主题的 `primary`，其余降到
//! `muted_foreground`。不额外画标记条 —— 那会打破底栏整片的表面。

use gpui_kit::component::{ActiveTheme, Icon, Sizable, h_flex, v_flex};
use gpui_kit::{
    App, InteractiveElement as _, IntoElement, ParentElement, StatefulInteractiveElement as _,
    Styled, Window, div, px,
};

use super::Destination;
use crate::ui::services::l10n::tr;

/// 底栏高度（逻辑像素）。够放下「图标 + 一行小字」两行，又贴着 Material 底栏的档位。
pub const BAR_HEIGHT: f32 = 56.0;

/// 图标边长（逻辑像素）。比 `Size::Small`（14）大一档，让底栏的图标一眼看得清。
const ICON: f32 = 22.0;

pub(super) fn bottom_bar<F>(cx: &App, current: Destination, on_select: F) -> impl IntoElement
where
    F: Fn(Destination, &mut Window, &mut App) + Clone + 'static,
{
    let mut bar = h_flex().w_full().h(px(BAR_HEIGHT)).items_center();
    for destination in Destination::ALL {
        let active = current == destination;
        let label = tr(cx, destination.label_key());
        let on_select = on_select.clone();
        // 换页提示全在这一个值上：选中取主题主色，未选中降到次要前景色。
        let ink = if active {
            cx.theme().primary
        } else {
            cx.theme().muted_foreground
        };
        bar = bar.child(
            v_flex()
                .id(destination.slug())
                .flex_1()
                .h_full()
                .items_center()
                .justify_center()
                .gap(px(2.0))
                .child(
                    Icon::new(destination.icon())
                        .with_size(px(ICON))
                        .text_color(ink),
                )
                .child(div().text_xs().text_color(ink).child(label))
                .on_click(move |_, window, cx| {
                    on_select(destination, window, cx);
                }),
        );
    }
    bar
}
