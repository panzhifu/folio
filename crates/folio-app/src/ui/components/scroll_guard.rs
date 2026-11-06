//! 一块**自己吃滚动**的区域：面板里的清单一滚，身后的架面不许跟着滚。
//!
//! ## 为什么不能只靠 `occlude`
//!
//! `occlude`（`HitboxBehavior::BlockMouse`）挡的是命中测试：身后的 hitbox 不进
//! `mouse_hit_test.ids`，于是 `uniform_list` / `overflow_scroll` 那句
//! `hitbox.should_handle_scroll(window)` 判假。它在**指针确实落在面板上**时够用，但它是
//! 「不让身后收到」的**唯一**一道闸 —— 一旦面板的 hitbox 因为布局、绘制层或某个中间层的
//! 裁剪没把那一格算进去，同一笔滚动就会同时喂给面板里的清单与身后的架面，两边各滚各的。
//!
//! ## Flutter 是怎么做的
//!
//! Flutter 把一次手势交给 **gesture arena**（`GestureArenaManager`）：命中的几个
//! `Scrollable` 里只有一个「赢」，从手指按下到抬起都是它，其余的根本收不到；鼠标滚轮走
//! `PointerSignalResolver`，同样是「命中的最内层注册者消费掉、外层不再收到」。所以内层
//! 清单滚到底也不会把同一笔继续交给外层 —— 要么内层滚，要么不动，**不会两个一起动**。
//! 而且这个归属在**起手那一刻**就定了，手指之后划出内层的框也不会换人。
//!
//! GPUI 这边没有 arena：一次 `ScrollEvent` 会沿着绘制顺序冒泡给**每一个**
//! `should_handle_scroll` 判真的监听者。这一格补的就是 arena 缺的那半句 ——
//! 「内层处理完就到此为止」。
//!
//! ## 怎么用
//!
//! 放在一个**自带可滚区**的面板里，作为**第一个孩子**（`absolute().inset_0()` 盖住整块
//! 面板，不参与布局、不画任何东西）：
//!
//! ```ignore
//! v_flex()
//!     .id("my-panel")
//!     .occlude()
//!     .child(scroll_guard("my-panel-scroll"))   // 必须在清单之前
//!     .child(v_virtual_list(..))
//! ```
//!
//! **顺序是有意义的**：鼠标监听按绘制顺序登记，冒泡相倒着走 —— 这一格先于面板内容绘制，
//! 所以它在冒泡里排在面板自己的滚动处理**之后**（清单先滚），又排在身后面板、列表的滚动
//! 处理**之前**（它们被这一格挡下）。摆在内容后面就反过来：清单还没滚就被掐掉。
//!
//! 它**无条件**吞掉落在面板上的滚动：面板里没有可滚的东西时也一样 —— 一块浮层盖住的那片
//! 格子不该因为读者在浮层上滑了一下而移动。这是 Flutter 里 `Scrollable` 边缘不回传的
//! 同一取舍（没有 scroll chaining）。
//!
//! ## 还有一半：窗口级监听要先知道「这一格有人管」
//!
//! 外壳的导航栏让位（`shell::nav_watch`）走的是 `window.on_mouse_event` 的**捕获相** ——
//! 它比面板里的任何监听都早跑，`stop_propagation` 在它之后已经来不及了。所以面板上的
//! 一笔滚动还会被它当成「读者在翻这一屏」，顶栏照样收起来。它要能先问一句「这一格有人
//! 管吗」，答案就是 [`ScrollShields`]：每帧由外壳记一次帧号，`scroll_guard` 绘制时把自己
//! 那一块报进去 —— 面板关掉就不再报，那一格也就跟着过期。

use gpui_kit::{
    App, Bounds, ElementId, Global, InteractiveElement as _, IntoElement, ParentElement as _,
    Pixels, Point, ScrollEvent, Styled as _, canvas, div,
};

/// 本帧被 [`scroll_guard`] 盖住的几块（窗口坐标）。
///
/// 这是给**窗口级**监听用的：它们跑在捕获相，早于面板自己的滚动监听，只能靠这份名单
/// 判断「这一笔是不是落在某块自己吃滚动的面板上」。每帧由外壳开头 [`Self::begin_frame`]
/// 记一次（外壳画得比架面早），各块在绘制时 [`Self::report`]。
#[derive(Default)]
pub struct ScrollShields {
    /// 这一帧的序号。`scroll_guard` 填报时记下它，名单按它判新鲜度。
    frame: u64,
    /// 按 id 存：同一块面板每帧重报一次只替换，不叠。留一帧的宽限（见 [`Self::covers`]）：
    /// 外壳偶尔只重画自己那一层（目的地视图没脏、走的是缓存绘制），那一帧里面板不会重新
    /// 填报 —— 一帧的宽限让这种帧不至于把名单当成空的。
    rects: Vec<(ElementId, Bounds<Pixels>, u64)>,
}

impl Global for ScrollShields {}

impl ScrollShields {
    /// 新的一帧开始收名单。由外壳的根（`nav_watch`）在绘制时调用 —— 它排在内容之前。
    pub fn begin_frame(cx: &mut App) {
        cx.default_global::<ScrollShields>().advance();
    }

    fn advance(&mut self) {
        self.frame += 1;
        // 两帧以上没再报的就不属于当前画面：面板已经收掉了，那块矩形不许继续吞滚动。
        let now = self.frame;
        self.rects.retain(|(_, _, last)| *last + 2 > now);
    }

    /// 这一笔落在哪块由 `scroll_guard` 管着的区域上。
    pub fn covers(&self, position: Point<Pixels>) -> bool {
        self.rects
            .iter()
            .any(|(_, bounds, last)| *last + 1 >= self.frame && bounds.contains(&position))
    }

    fn report(&mut self, id: ElementId, bounds: Bounds<Pixels>) {
        let frame = self.frame;
        match self.rects.iter_mut().find(|(known, _, _)| *known == id) {
            Some((_, rect, last)) => {
                *rect = bounds;
                *last = frame;
            }
            None => self.rects.push((id, bounds, frame)),
        }
    }
}

/// 让这一块区域的滚动**到此为止**。
///
/// 见模块头：放在自带可滚区的面板里、作为第一个孩子。
pub fn scroll_guard(id: impl Into<ElementId>) -> impl IntoElement {
    let id: ElementId = id.into();
    let reported = id.clone();
    div()
        .id(id)
        // 盖住整块面板：命中区就是面板本身。它不参与布局，也不插 `BlockMouse` ——
        // 挡点击是 `occlude` 的活儿，这一格只管滚动归谁。
        .absolute()
        .inset_0()
        .on_scroll(|_: &ScrollEvent, _: &mut gpui_kit::Window, cx: &mut App| {
            // 冒泡相里这一句之后，身后的滚动容器（架面那一列、弹层外框）再也收不到这一笔。
            cx.stop_propagation();
        })
        // 把自己盖住的那一块报给窗口级监听（见 [`ScrollShields`]）。这一格不画东西，
        // 只在这一帧的绘制里留个坐标。
        .child(
            canvas(
                |_, _, _| {},
                move |bounds, _, _, cx| {
                    cx.default_global::<ScrollShields>()
                        .report(reported.clone(), bounds);
                },
            )
            .absolute()
            .inset_0(),
        )
}

#[cfg(test)]
mod tests {
    //! 这一格的判据只有一条：**面板自己滚完，身后那块就不许再滚**。
    //!
    //! 两个用例一组：不带闸的那条先证明这套架子真的会「两个一起滚」（否则带闸那条是空的），
    //! 带闸的那条再钉住「内层滚、外层一动不动」。

    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Context, InteractiveElement as _, IntoElement,
        ParentElement as _, PlatformInput, Render, ScrollDelta, ScrollEvent, ScrollHandle,
        StatefulInteractiveElement as _, Styled as _, TestAppContext, Window, div, point, px, size,
    };

    use gpui_kit::Bounds;

    use super::{ScrollShields, scroll_guard};

    /// 一整窗的可滚区，上面（绘制顺序在后）压一块自带清单一列的面板。
    struct Harness {
        outer: ScrollHandle,
        inner: ScrollHandle,
        /// 面板里摆不摆 `scroll_guard`。
        guarded: bool,
    }

    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let outer = self.outer.clone();
            let inner = self.inner.clone();
            let panel = div()
                .absolute()
                .left(px(20.))
                .top(px(20.))
                .w(px(200.))
                .h(px(200.))
                .children(self.guarded.then(|| scroll_guard("panel-scroll")))
                .child(
                    div()
                        .id("panel-inner")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&inner)
                        .child(div().w_full().h(px(1200.))),
                );
            div()
                .size_full()
                .relative()
                // 身后那一片：铺满整窗，够长，滚得动。
                .child(
                    div()
                        .id("behind")
                        .absolute()
                        .inset_0()
                        .overflow_y_scroll()
                        .track_scroll(&outer)
                        .child(div().w_full().h(px(2000.))),
                )
                .child(panel)
        }
    }

    fn open(
        cx: &mut TestAppContext,
        guarded: bool,
    ) -> (AnyWindowHandle, ScrollHandle, ScrollHandle) {
        cx.update(gpui_kit::init);
        let outer = ScrollHandle::new();
        let inner = ScrollHandle::new();
        let (outer_for_view, inner_for_view) = (outer.clone(), inner.clone());
        let handle = cx
            .open_window(size(px(400.), px(500.)), move |window, cx| {
                let view = cx.new(|_| Harness {
                    outer: outer_for_view,
                    inner: inner_for_view,
                    guarded,
                });
                Root::new(view, window, cx)
            })
            .into();
        for _ in 0..3 {
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .expect("窗口应能渲染");
        }
        (handle, outer, inner)
    }

    /// 在面板正中滚一把（向下 120，gpui 的约定是向下为负）。
    fn scroll_panel(cx: &mut TestAppContext, handle: AnyWindowHandle) {
        cx.update_window(handle, |_, window, cx| {
            window.dispatch_event(
                PlatformInput::Scroll(ScrollEvent {
                    position: point(px(120.), px(120.)),
                    delta: ScrollDelta::Pixels(point(px(0.), px(-120.))),
                    ..Default::default()
                }),
                cx,
            );
            window.render_frame(cx);
        })
        .expect("窗口应能渲染");
    }

    /// 名单的寿命：面板停报之后还活一帧（外壳只重画自己那一层时走缓存绘制），再往后
    /// 就不许继续吞滚动 —— 收起来的面板不能留一块看不见的矩形在那儿。
    #[test]
    fn the_shield_register_forgets_a_panel_after_it_stops_reporting() {
        let mut shields = ScrollShields::default();
        let bounds = Bounds {
            origin: point(px(10.), px(10.)),
            size: size(px(20.), px(20.)),
        };
        shields.report("panel".into(), bounds);
        assert!(
            shields.covers(point(px(15.), px(15.))),
            "刚报上来的那一块要算数"
        );
        assert!(
            !shields.covers(point(px(50.), px(50.))),
            "框外的那一笔不归它管"
        );

        shields.advance();
        assert!(
            shields.covers(point(px(15.), px(15.))),
            "停报一帧内还算数（缓存绘制那一帧不重新填报）"
        );

        shields.advance();
        assert!(
            !shields.covers(point(px(15.), px(15.))),
            "两帧没再报就该忘掉那块矩形"
        );
    }

    /// 不带闸：同一笔滚轮会**同时**喂给面板里的清单与身后的那一片 —— 这正是手机上看
    /// 到的「滑扫描结果卡片，书架跟着动」。这一条是下面那条的对照，别删。
    #[gpui_kit::test]
    fn without_a_guard_the_scroll_also_reaches_the_list_behind(cx: &mut TestAppContext) {
        let (handle, outer, inner) = open(cx, false);
        scroll_panel(cx, handle);
        assert!(
            inner.offset().y < px(0.),
            "前置：面板里的清单该滚，实测 {}",
            inner.offset().y
        );
        assert!(
            outer.offset().y < px(0.),
            "对照：没有闸时身后那一片本来就会跟着滚；这里没滚说明这套架子没复现出那个 bug"
        );
    }

    /// 带闸：清单滚，身后一动不动。整笔滚动在面板那一层就结束了（Flutter 的 arena 里
    /// 「内层 Scrollable 赢、外层收不到」）。
    #[gpui_kit::test]
    fn the_guard_stops_the_scroll_at_the_panel(cx: &mut TestAppContext) {
        let (handle, outer, inner) = open(cx, true);
        scroll_panel(cx, handle);
        assert!(
            inner.offset().y < px(0.),
            "面板里那一列该照旧滚得动，实测 {}",
            inner.offset().y
        );
        assert_eq!(
            outer.offset().y,
            px(0.),
            "面板上的滚动漏到了身后 —— 这一格要把它挡在面板里"
        );
    }
}
