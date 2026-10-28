//! Medium / Expanded 档的侧栏：同一串入口，两种表面。
//!
//! * **Medium（600–840）**：一张 96 宽的卡片，四边留 [`NAV_MARGIN`]、竖向居中 ——
//!   华为这一档给的是「侧边导航栏的悬浮显示」，卡片就是那个「浮」的载体。
//! * **Expanded（≥840）**：同一串入口嵌进一条 96 宽的整列，贴顶排，右边一条发丝线。
//!   触屏端走不到这里 —— [`super::SizeClass::of`] 把 ≥840 夹回 Medium，所以这条整列
//!   实际上是桌面窗口专属的那一种表面。
//!
//! 两档都**占位**（[`super::reserved_width`]）：卡片压上去会挡住书架第一列，而华为的
//! 悬浮侧栏本来还能滑出收起 —— 我们没做那个手势，就不该拿内容去换那点浮起来的错觉。
//! 浮起来这件事由表面自己说清（圆角、描边、模糊）。

use std::time::Duration;

use gpui_kit::base::motion::{Transition, transition};
use gpui_kit::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement, Styled,
    TestSupportExt as _, Window, div, px,
};

use super::surface::{Shape, embedded, floating, nav_item, nav_stack};
use super::{Destination, NAV_MARGIN, NAV_WIDTH};

/// 嵌入列里第一格离状态栏的那点空：与卡片的内缩同一档。
const TOP_GAP: f32 = 8.0;

/// 卡片收起/放回的那一程（与底栏同一档缓动）。
const FOLD_DURATION: Duration = Duration::from_millis(240);

pub(super) fn side_nav<F>(
    cx: &mut App,
    current: Destination,
    card: bool,
    safe_top: f32,
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
                Shape::Tile,
                on_select.clone(),
            )
            .into_any_element()
        })
        .collect();

    if card {
        // 收起 = 卡片整张滑到列外左侧。占位列**不动**：滚动中改列宽会让右侧内容每帧
        // 重排，而这里买的正是「内容获得更多视线」，不是「内容重新排版」。
        let target = if folded {
            -(NAV_WIDTH + NAV_MARGIN * 2.0)
        } else {
            0.0
        };
        let shift = transition(
            "folio-nav-fold-card",
            target,
            Transition::new(FOLD_DURATION),
            window,
            cx,
        );
        // 列比卡片宽两倍 margin：卡片左右居中落在列里，整列的高度归内容让出来。
        div()
            .id("folio-nav")
            .w(px(NAV_WIDTH + NAV_MARGIN * 2.0))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                floating(cx)
                    .relative()
                    .left(px(shift))
                    .w(px(NAV_WIDTH))
                    .child(nav_stack(items)),
            )
            .test_support()
    } else {
        embedded(cx)
            .id("folio-nav")
            .w(px(NAV_WIDTH))
            .h_full()
            .pt(px(safe_top + TOP_GAP))
            .child(nav_stack(items))
            .test_support()
    }
}
