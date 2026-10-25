//! Medium / Expanded 档的导航栏：左侧竖栏。
//!
//! 包 gpui-kit 的 `Sidebar`：展开态（Expanded）是一个 260px 的侧栏，
//! 图标态（Medium）折到 48px 只剩图标 —— 也就是 Material 的 navigation rail。

use gpui_kit::component::sidebar::{Sidebar, SidebarCollapsible, SidebarMenu, SidebarMenuItem};
use gpui_kit::{App, IntoElement, Styled, Window, px};

use super::{Destination, RAIL_WIDTH, SIDEBAR_WIDTH};
use crate::ui::services::l10n::tr;

pub(super) fn side_nav<F>(
    cx: &App,
    current: Destination,
    collapsed: bool,
    on_select: F,
) -> impl IntoElement
where
    F: Fn(Destination, &mut Window, &mut App) + Clone + 'static,
{
    let mut menu = SidebarMenu::new();
    for destination in Destination::ALL {
        let active = current == destination;
        let label = tr(cx, destination.label_key());
        let on_select = on_select.clone();
        menu = menu.child(
            SidebarMenuItem::new(label.clone())
                .icon(destination.icon())
                .active(active)
                .collapsed(collapsed)
                .on_click(move |_, window, cx| {
                    on_select(destination, window, cx);
                }),
        );
    }

    // rail 是**常驻**折叠态（没有展开/收起按钮）；展开态忽略 collapse 状态。
    let (width, collapsible) = if collapsed {
        (RAIL_WIDTH, SidebarCollapsible::Icon)
    } else {
        (SIDEBAR_WIDTH, SidebarCollapsible::None)
    };

    // 两档都不给品牌头：应用名在桌面由自绘标题栏承担（`shell/mod.rs` 的 `TitleBar`），
    // 侧栏是导航，不是放标的地方。
    Sidebar::new("folio-nav")
        .w(px(width))
        .collapsed(collapsed)
        .collapsible(collapsible)
        .child(menu)
}
