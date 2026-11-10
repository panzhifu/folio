//! 架面那两块「要读者答一句话」的浮层的**容器**：建组、扫描结果。
//!
//! 同一份内容，两种容器：桌面与平板上是一张 [`Popover`]，手机的 compact 档上是同一块
//! 内容压进 [`BottomSheet`]。换容器换的不是好看，是那张卡钉不钉得住：手机上 ⋮ 那颗按钮
//! 离屏幕右边只有十几像素，从它下面开一张 420 宽的卡有一半要掉到屏幕外，而底部弹层
//! 天生满幅。
//!
//! 面板本体（表单里那几行、清单那几列）留在各自的文件里（[`super::view`] / [`super::scan`]），
//! 这一层只管两件事：装进哪一块容器、读者自己关掉时派回哪儿。
//!
//! 内容一律是**在外头搭好的元素**而不是闭包：`Popover` 的 `.content()` 每帧重跑一遍，
//! 而搭那一屏要 `&mut Context<ShelfView>`（按钮的回调要架面的句柄），弹窗自己的绘制
//! 回调里只有 `&mut App`。`.child()` 收的是搭完的那一块，一次搭好、挂在弹窗层里，
//! 两件事就都成立。
//!
//! 两块弹层各握一份 [`BottomSheetState`]，不是共用一份：共用一份的话，A 屏正在往下沉的
//! 那半程被 B 屏接管，读者看到的是 A 的内容原地换成 B。退场动画各归各的那一份才对得上
//! 「我刚关掉的是哪一屏」。

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::popover::Popover;
use gpui_kit::{
    Anchor, AnyElement, Context, ElementId, Entity, InteractiveElement as _, IntoElement,
    ParentElement, Styled, div, px,
};

use crate::ui::components::{BottomSheet, BottomSheetState, scroll_guard};

use super::view::ShelfView;

/// `Popover` 那一圈写死的内边距（组件里的 `p_3`，默认 rem 下 12 逻辑像素）。
///
/// 居中要往上抬的是**整张卡**的一半，而卡的高 = 内容高 + 上下这一圈，所以这个数要在
/// 这里出现一次。它由组件写死、不由主题给：改了组件那一处而没改这里，浮层会偏出半个
/// 内边距（24 像素的一半）。
const POPOVER_PAD: f32 = 12.0;

/// 「答一句话」的表单卡（建组）的宽与内容高。卡片本身按 `min_h` 给，
/// 提示那一行在某些语言里会多折一行，长出来的那一点只让卡偏离正中几像素。
pub(in crate::ui::features::shelf) const FORM_WIDTH: f32 = 420.0;
pub(in crate::ui::features::shelf) const FORM_HEIGHT: f32 = 152.0;

/// 扫描结果那张。高是**定死**的而不是「至少」：里面是一条虚拟列表，列表要滚就得有一个
/// 能滚的框，「至少多高」给不出这个框。
pub(in crate::ui::features::shelf) const SCAN_WIDTH: f32 = 460.0;
pub(in crate::ui::features::shelf) const SCAN_HEIGHT: f32 = 460.0;

/// 导入记录那张卡的**上限**宽与高。
///
/// 宽是上限而不是定值：420 是桌面与平板那一档，手机上视口只有 384 逻辑像素，照搬会把卡
/// 顶成满幅还溢出 —— 右边那两颗钮（关闭与清空）就掉到屏幕外，读者看到的是「这张卡关不掉」。
/// 所以调用方按视口现算（`status::record_width`），这里只给上限。
///
/// 高同理：卡要贴着内容长（一本书就是一本书的高），只有清单长到封顶才开始滚。
pub(in crate::ui::features::shelf) const RECORD_MAX_WIDTH: f32 = 420.0;
pub(in crate::ui::features::shelf) const RECORD_MAX_HEIGHT: f32 = 420.0;

/// 底部弹层展开那一档占视口高的比例。
///
/// 比目录那一屏（九成）矮一档：这三块是「答一句话就走」的层，压掉九成屏幕等于把架面
/// 整个藏起来，而读者关掉它之后要看的就是那片架面。扫描那一屏要看得下几行清单，
/// 所以它走 [`SCAN_SHEET_EXPANDED`] 那一档。
pub(in crate::ui::features::shelf) const SHEET_EXPANDED: f32 = 0.6;
pub(in crate::ui::features::shelf) const SCAN_SHEET_EXPANDED: f32 = 0.85;
/// 一张浮层。`anchor`/`offset` 决定它钉在哪儿，`close` 只在**读者自己关掉**那一刻派回去
/// （点卡外面、按返回）—— 关掉之后把架面那份状态清掉的是调用方，不是这里。
///
/// 那颗零尺寸的触发器不是省事：`Popover` 要拿一个元素的 bounds 当锚点，而这三屏的开关
/// 握在架面的状态里，不是握在「点了这颗按钮」里 —— 长按菜单那条路开出来的建组面板，
/// 触发它的那个元素下一帧就不在了。与阅读页脚注那一屏同一条路
/// （`reader::page` 的 `footnote_popover`）。
///
/// `offset` 沿锚轴方向，可以为负：居中那两张靠它往上抬掉半张卡的高。
pub(in crate::ui::features::shelf) fn popover(
    key: &'static str,
    anchor: Anchor,
    offset: f32,
    shelf: &Entity<ShelfView>,
    close: impl Fn(&mut ShelfView, &mut Context<ShelfView>) + 'static,
    body: AnyElement,
) -> AnyElement {
    let dismiss = shelf.clone();
    Popover::new(key)
        .anchor(anchor)
        .offset(px(offset))
        .open(true)
        .on_open_change(move |open, _, cx| {
            if *open {
                return;
            }
            dismiss.update(cx, |this, cx| close(this, cx));
        })
        .trigger(
            Button::new(format!("{key}-trigger"))
                .ghost()
                .p_0()
                .w(px(0.))
                .h(px(0.))
                .min_w(px(0.))
                .min_h(px(0.)),
        )
        .child(body)
        .into_any_element()
}

/// 给**居中浮层**的内容套一层「滚动到此为止」：`occlude` 挡点击、`scroll_guard` 吞滚动。
///
/// 两块必须一起给：`occlude` 只让身后的 hitbox 不进命中测试，而滚动归谁看的是
/// `should_handle_scroll`；没有 `scroll_guard` 那一格，读者在卡上滑一下，身后的
/// 架面照样跟着滚（真机上读者报的原话）。id 由调用方的 key 派生，同一屏两张卡
/// 不会撞。
///
/// 收在这里而不是每张卡自己写：架面上这几张浮层全是「答一句话」的卡，内容本身
/// 不滚，一件件补漏迟早漏掉某一张（网络导入那张就这么漏的）。
///
/// **只给 [`centered`] 那一档用**：它多包的一层是普通块，不参与尺寸计算，而浮层里的
/// 卡片都是定高或 `min_h` 的。底部弹层不能走这一层 —— 那边的内容是 `flex_1` 的树（扫描
/// 清单要靠父层给出确定高度），再套一个普通块就把 `flex_1` 挡在自动高度里，虚拟列表塌成
/// 0 高：手机上只看得到标题与底部按钮，中间一行候选都没有（见 [`sheet`]）。
fn guarded(key: &'static str, body: AnyElement) -> AnyElement {
    div()
        .relative()
        .occlude()
        .child(scroll_guard(ElementId::Name(
            format!("{key}-scroll").into(),
        )))
        .child(body)
        .into_any_element()
}

/// 把一张浮层挂在**内容区的正中央**。
///
/// 一层铺满的 flex 容器把那颗零尺寸的触发器推到正中，卡片本体由 `Popover` 画在弹窗层里、
/// 不占这一层的任何位置 —— 所以这一层既不给卡片定尺寸，也不接点击（它没有事件监听，
/// gpui 的命中测试不收它，底下那片格子照旧点得动）。
///
/// `height` 是调用方**报上来**的整张卡高，不是画完再量的：`Popover` 开完之后不回调报尺寸，
/// 而「往上抬半张高」需要一个数。报的数与真实高对不上时卡会偏上或偏下几像素 —— 不截断、
/// 不挡别的东西，这是换来「不必等一帧才定得准」的代价。
pub(in crate::ui::features::shelf) fn centered(
    key: &'static str,
    height: f32,
    shelf: &Entity<ShelfView>,
    close: impl Fn(&mut ShelfView, &mut Context<ShelfView>) + 'static,
    body: AnyElement,
) -> AnyElement {
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .child(div().size(px(0.)).child(popover(
            key,
            Anchor::TopCenter,
            -(height + POPOVER_PAD * 2.0) / 2.0,
            shelf,
            close,
            guarded(key, body),
        )))
        .into_any_element()
}

/// 移动端的一块底部弹层。
///
/// 收稳了那一程**照样要交元素**（弹簧的采样在里面），只是不给内容 —— 判据是
/// [`BottomSheetState::is_idle`]，不是 `is_open()`：退场那半程 `is_open()` 已经是 `false`
/// 而卡片还浮在屏幕上，那时断掉内容就等于看着它空着沉下去。判内容留不留的活儿在调用方
/// （`ShelfView` 那一侧握着内容），这里只负责装。
///
/// `lift_ime` 是「键盘出来时整层往上抬不让位」（[`BottomSheet::ime_lift`]）的开关：
/// 输入框在弹层**底部或中部**的表单（建组、从链接导入）开着 —— 键盘真的会盖住输入框；
/// 输入框钉在弹层**顶部**的那几张（扫描清单）关掉 —— 展开档又高，键盘本来就够不到它，
/// 再抬一次只是让整屏内容往上跳一下（阅读页目录那一屏同一形状、同一取舍）。
///
/// 守卫与内容**平级**摆进弹层，不套那一层 [`guarded`]：扫描清单的根是 `flex_1` 的树，
/// 中间夹一个普通块就把它挡在自动高度里，清单塌成 0 高。挡点击由弹层表面自己的
/// `occlude` 负责，这一格只补「滚动到此为止」。
pub(in crate::ui::features::shelf) fn sheet(
    key: &'static str,
    state: &Entity<BottomSheetState>,
    expanded: f32,
    lift_ime: bool,
    body: Option<AnyElement>,
) -> AnyElement {
    let sheet = BottomSheet::new(state, key)
        .expanded(expanded)
        .ime_lift(lift_ime);
    match body {
        // 守卫必须排在内容**之前**：它先于内容绘制，冒泡相里就排在内容自己的滚动处理
        // 之后（见 `components::scroll_guard` 模块头）。
        Some(body) => sheet
            .child(scroll_guard(ElementId::Name(
                format!("{key}-scroll").into(),
            )))
            .child(body)
            .into_any_element(),
        None => sheet.into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    //! 这一层只钉一条：**弹层的内容根必须自己拿到弹层的高度**。
    //!
    //! 手机档（Compact）在宿主的单测里走不到 —— `SizeClass::of` 把非移动端的 Compact
    //! 夹回 Medium，于是这套弹层不进 `shell/tests` 的窗口。而它坏掉的样子只有手机上看得见：
    //! 扫描清单的 `flex_1` 被中间一层普通块挡在自动高度里，虚拟列表塌成 0 高，屏幕上只剩
    //! 标题与底部按钮。所以这条门直接量布局，不绕视口档位。

    use std::time::Duration;

    use gpui_kit::component::Root;
    use gpui_kit::component::v_flex;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
        ParentElement as _, Render, Styled as _, TestAppContext, TestSupportExt as _, Window, div,
        px, size,
    };

    use super::sheet;
    use crate::ui::components::BottomSheetState;

    /// 底下一块弹层，内容照扫描那一屏的形状搭：一条定高的工具行、一棵 `flex_1` 的清单、
    /// 一条定高的底栏。清单那格挂个名，量它拿到了多高。
    struct Harness {
        state: Entity<BottomSheetState>,
    }

    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().relative().child(sheet(
                "harness-sheet",
                &self.state,
                0.85,
                false,
                Some(
                    v_flex()
                        .w_full()
                        .flex_1()
                        .min_h_0()
                        .child(div().w_full().h(px(40.)))
                        .child(
                            div()
                                .id("sheet-content-fill")
                                .test_support()
                                .w_full()
                                .flex_1(),
                        )
                        .child(div().w_full().h(px(40.)))
                        .into_any_element(),
                ),
            ))
        }
    }

    /// 推进弹簧：测试时钟不自己走，一帧一帧推 16ms 直到弹层停稳。
    fn settle(cx: &mut TestAppContext, handle: AnyWindowHandle) {
        for _ in 0..60 {
            cx.executor().advance_clock(Duration::from_millis(16));
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .expect("窗口应能渲染");
        }
    }

    #[gpui_kit::test]
    fn the_sheet_body_keeps_its_flex_one(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let state = cx.new(BottomSheetState::closed);
        let state_for_view = state.clone();
        let handle: AnyWindowHandle = cx
            .open_window(size(px(400.), px(600.)), |window, cx| {
                let view = cx.new(|_| Harness {
                    state: state_for_view,
                });
                Root::new(view, window, cx)
            })
            .into();
        cx.update(|cx| state.update(cx, |state, cx| state.show(cx)));
        settle(cx, handle);

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let surface = window
                .try_find("bottom-sheet-surface")
                .expect("弹层表面该挂着")
                .bounds();
            let fill = window
                .try_find("sheet-content-fill")
                .expect("清单那一格该在树上")
                .bounds();
            assert!(
                f32::from(fill.size.height) > f32::from(surface.size.height) * 0.5,
                "内容没拿到弹层的高度（弹层 {}、清单 {}）：`flex_1` 被中间一层挡在自动高度里，\
                 手机上扫描清单会塌成 0 高，只剩标题与底部按钮",
                f32::from(surface.size.height),
                f32::from(fill.size.height),
            );
        })
        .expect("窗口应能渲染");
    }
}
