//! Medium / Expanded 档的导航栏：左侧竖栏。
//!
//! 包 gpui-kit 的 `Sidebar`：展开态（Expanded）是一个 260px 的侧栏，
//! 图标态（Medium）折到 48px 只剩图标 —— 也就是 Material 的 navigation rail。

use gpui_kit::component::sidebar::{Sidebar, SidebarCollapsible, SidebarMenu, SidebarMenuItem};
use gpui_kit::{App, FontWeight, IntoElement, ParentElement, Styled, Window, div, px};

use super::{Destination, RAIL_WIDTH, SIDEBAR_WIDTH};
use crate::ui::shared::l10n::tr;

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

    let mut sidebar = Sidebar::new("folio-nav")
        .w(px(width))
        .collapsed(collapsed)
        .collapsible(collapsible);

    // 展开态给一个品牌头；折叠态只有 48px，放不下文字，留空。
    if !collapsed {
        sidebar = sidebar.header(
            div()
                .w_full()
                .px_2()
                .py_1()
                .font_weight(FontWeight::SEMIBOLD)
                .child("Folio"),
        );
    }

    sidebar.child(menu)
}
