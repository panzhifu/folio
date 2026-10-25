//! Compact 档的导航栏：一条**浮在内容之上**的胶囊底栏。
//!
//! 它不占布局行：绝对定位、左右各留 [`NAV_MARGIN`]、底下让开手势条。内容照旧铺满整窗，
//! 滚动区自己带尾部留白（`navigation::clearance`），于是一边滚一边从栏底下穿过去 ——
//! 这正是悬浮与「一条贴底的色块」的区别。
//!
//! 一格的内容、选中态、hover 全在 [`super::surface::nav_item`]；这里只管把四格摆进胶囊。
//! 换页的提示只有颜色那一处（与三档一致），不额外画标记条 —— 那会打破整片表面。

use gpui_kit::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement, Styled,
    TestSupportExt as _, Window, px,
};

use super::surface::{Shape, floating, nav_item};
use super::{BAR_HEIGHT, Destination, NAV_MARGIN};

pub(super) fn bottom_bar<F>(
    cx: &App,
    current: Destination,
    bottom: f32,
    on_select: F,
) -> impl IntoElement
where
    F: Fn(Destination, &mut Window, &mut App) + Clone + 'static,
{
    let items: Vec<AnyElement> = Destination::ALL
        .into_iter()
        .map(|destination| {
            nav_item(
                cx,
                destination,
                current == destination,
                Shape::Capsule,
                on_select.clone(),
            )
            .into_any_element()
        })
        .collect();

    floating(cx)
        .id("folio-bar")
        .absolute()
        .left(px(NAV_MARGIN))
        .right(px(NAV_MARGIN))
        .bottom(px(bottom))
        .h(px(BAR_HEIGHT))
        .flex()
        .items_center()
        // 四格内侧再收 4：选中那颗胶囊不贴到圆角上。
        .px(px(4.0))
        // 压在内容上就得真的挡住：落在四格之间的一击不该再打到底下那张书卡。
        .occlude()
        .children(items)
        // 宿主进不到 Compact 档（[`super::SizeClass::of`] 在桌面把窄窗口夹到 Medium），
        // 这一条浮栏的几何只能在专门的用例里直接问 `navigation` —— 那一处按这个 id 认。
        .test_support()
}
