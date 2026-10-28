//! Compact 档的导航栏：一条**浮在内容之上**的胶囊底栏。
//!
//! 它不占布局行：绝对定位、左右各留 [`NAV_MARGIN`]、底下让开手势条。内容照旧铺满整窗，
//! 滚动区自己带尾部留白（`navigation::clearance`），于是一边滚一边从栏底下穿过去 ——
//! 这正是悬浮与「一条贴底的色块」的区别。
//!
//! 一格的内容、选中态、hover 全在 [`super::surface::nav_item`]；这里只管把四格摆进胶囊。
//! 换页的提示只有颜色那一处（与三档一致），不额外画标记条 —— 那会打破整片表面。
//!
//! 内容往下读时整条**滑进屏幕下沿**（`folded`），往回拨或回到顶上再滑回来。位移走
//! `gpui_kit::base::motion::spring`：按元素 id 存状态，反向拨动会从当前值接着走，
//! 而不是重头再来；减小动态效果时它自己退化为直接到位。

use std::time::Duration;

use gpui_kit::base::motion::{Transition, transition};
use gpui_kit::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement, Styled,
    TestSupportExt as _, Window, px,
};

use super::surface::{Shape, floating, nav_item};
use super::{BAR_HEIGHT, Destination, NAV_MARGIN};

/// 收起/放回的那一程。
///
/// 用定时缓动（`Transition` 默认的 ease-out-cubic）而不是弹簧：这一条是**跟手滚动**
/// 触发的，弹簧的减速段在回滚到底时会被读成「又弹了一下」；定时曲线到点即停，干脆。
const FOLD_DURATION: Duration = Duration::from_millis(240);

pub(super) fn bottom_bar<F>(
    cx: &mut App,
    current: Destination,
    bottom: f32,
    folded: bool,
    window: &mut Window,
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
                Shape::Tab,
                on_select.clone(),
            )
            .into_any_element()
        })
        .collect();

    // 收起 = 整条滑到屏幕下沿之外：`bottom` 是**距下沿的距离**，变负才往下走
    // （变大是往上抬）。栏顶边越过屏幕下沿需要 `bottom = -BAR_HEIGHT`，再退一圈留白
    // 让圆角不挂在边上 —— 不能拿 `bottom` 去减，它里面已经含了手势条那段。
    let target = if folded {
        -(BAR_HEIGHT + NAV_MARGIN)
    } else {
        bottom
    };
    let bottom = transition(
        "folio-nav-fold-bar",
        target,
        Transition::new(FOLD_DURATION),
        window,
        cx,
    );

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
