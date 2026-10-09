//! Compact 档的导航栏：底部四格。
//!
//! gpui-kit 没有 bottom navigation 组件，所以自绘。每格用组件的 `Button` 而不是裸
//! `div`：它自带 hover / pressed / selected / disabled / focus 五档状态与可访问名。
//! 选中态靠那格自己的表面与文字重量表达，不额外画一条标记 —— 加标记会打破它的圆角轮廓。

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{Selectable, Sizable, h_flex};
use gpui_kit::{App, IntoElement, ParentElement, Styled, Window, px};

use super::Destination;
use crate::ui::shared::l10n::tr;

/// 底栏高度（逻辑像素）。Material 底栏标准高度，也是拇指能稳定命中的最小格高。
pub const BAR_HEIGHT: f32 = 56.0;

pub(super) fn bottom_bar<F>(cx: &App, current: Destination, on_select: F) -> impl IntoElement
where
    F: Fn(Destination, &mut Window, &mut App) + Clone + 'static,
{
    let mut bar = h_flex().w_full().h(px(BAR_HEIGHT)).items_center();
    for destination in Destination::ALL {
        let active = current == destination;
        let label = tr(cx, destination.label_key());
        let on_select = on_select.clone();
        bar = bar.child(
            Button::new(destination.slug())
                .ghost()
                .small()
                .flex_1()
                .w_full()
                .justify_center()
                .selected(active)
                .icon(destination.icon())
                .label(label.clone().to_string())
                .accessibility_label(label)
                .on_click(move |_, window, cx| {
                    on_select(destination, window, cx);
                }),
        );
    }
    bar
}
