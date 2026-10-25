//! Medium / Expanded 档的导航栏：左侧竖栏。
//!
//! 包 gpui-kit 的 `Sidebar`：展开态（Expanded）是一个 260px 的侧栏，
//! 图标态（Medium）折到 48px 只剩图标 —— 也就是 Material 的 navigation rail。

use gpui_kit::base::component_traits::Collapsible;
use gpui_kit::component::sidebar::{
    Sidebar, SidebarCollapsible, SidebarItem, SidebarMenu, SidebarMenuItem,
};
use gpui_kit::{App, IntoElement, Styled, Window, px};

use super::{Destination, RAIL_WIDTH, SIDEBAR_WIDTH};
use crate::ui::services::l10n::tr;

pub(super) fn side_nav<F>(
    window: &mut Window,
    cx: &mut App,
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

    // 整组走 kit 的 `footer` 槽：`content` 是 `flex_1`，footer 排在它之后，是这条列的最后
    // 一格 —— 四格因此贴底，上方整段留空。贴底顺带避开状态栏：侧栏整列不吃 `safe.top`
    // （`shell/mod.rs:1200-1211` 只给正文加了顶边距），贴顶时第一格压在时钟下面。
    // `SidebarMenu` 只实现 `SidebarItem`、不是 `IntoElement`，所以这里自己 render 一份；
    // 折叠标记也自己给（kit 在 content 那条路上是替调用方 `collapsed` 的）。
    Sidebar::<SidebarMenu>::new("folio-nav")
        .w(px(width))
        .collapsed(collapsed)
        .collapsible(collapsible)
        .footer(menu.collapsed(collapsed).render("folio-nav-menu", window, cx))
}
