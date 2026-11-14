//! 胶囊滑条：两端标注、圆钮上直接写当前值的那一种（阅读设置那一族）。
//!
//! 与 [`gpui_kit::component::slider::Slider`](裸轨道) 的差别在**信息摆放**：
//! 语义刻度画在胶囊两端（两颗太阳、大 A 与小 A、大 / 小的边距），当前值不再
//! 单独占一行，而是写进圆钮 —— 眼睛不用在滑条与数值之间来回跳。
//!
//! 两端标注是**可选**的：设置那族每一格都是「一个量程两个语义端」，而底栏
//! 进度那条没有这样的端 —— 起止方向由它两侧的上 / 下一章箭头说，格子里再
//! 摆两个图标就是两份重复。没有标注的那条只少两个绝对定位的元素，其余同形。
//!
//! 交互**全部复用** gpui-base 的无样式滑条原语：[`SliderIndicator`] 在
//! `on_prepaint` 里记录位置→值的映射边界，[`SliderTrack`] 提供点击跳值、鼠标
//! 拖与触摸拖（GPUI 的 `on_drag` + `TouchDragEvent` 两族都接），[`SliderThumb`]
//! 自带拖动管线且 `children` 可放内容 —— 圆钮上的数值文本就是它的 child。
//! 状态仍是 [`SliderState`]：`SliderEvent::Change` 的订阅方（阅读设置那张表）
//! 一行不改，这一层只画皮。
//!
//! **松手**也是这一层的事：裸轨道只在触摸层兜住 Ended / Cancelled，鼠标的
//! mouse-up 原本挂在 base 的 `Slider` 行为根上，而胶囊不摆那个根 —— 这里补上
//! 同款的两条监听（up 在内 / up 在外都归 [`SliderState::handle_release`]）。
//! 进度条把「落点」押在 `Release` 上（拖动只挪圆钮，松手才 seek），缺了这两条
//! 就是桌面上拖一回永远松不了的手。
//!
//! 几何与 base 的定位映射同一条：圆钮中心落在 `percentage × 宽度` 上
//! （`.left(relative(p))` + `ml(-半径)`），指针位置与圆钮数值的对应关系和裸
//! 滑条完全一致；两端不留内缩，圆钮在极值处探出半个身位 —— 设计稿同形。

use gpui_kit::assets::IconName;
use gpui_kit::base::slider::{SliderIndicator, SliderState, SliderThumb, SliderTrack};
use gpui_kit::component::{ActiveTheme as _, Icon};
use gpui_kit::{
    App, Div, ElementId, Entity, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, Window, div, px, relative,
};

/// 胶囊轨道的高度。圆钮比它大一圈（上下各探出一点），设计稿同形。
const TRACK: f32 = 32.0;
/// 圆钮直径。
const THUMB: f32 = 44.0;
/// 胶囊轨道相对容器的下沉量：容器就是圆钮那一格（THUMB 高），轨道比它瘦一圈，
/// 上下各让出 `(THUMB - TRACK) / 2` 才垂直居中 —— 圆钮铺满容器，谁也不悬出去。
const TRACK_TOP: f32 = (THUMB - TRACK) / 2.0;

/// 一端的标注：图标（亮度的两颗太阳）或文字（大 A / 小 A、大 / 小的边距）。
#[derive(Clone, Debug)]
pub enum PillMark {
    Icon(IconName),
    /// `size` 是字号（px）：字号那一条的两端是同一个 "A"，靠大小说话。
    Text {
        text: SharedString,
        size: f32,
    },
}

impl PillMark {
    /// 一端的元素：竖直居中交给外层容器，这里只管颜色与内容。
    fn element(self, cx: &App) -> Div {
        let muted = cx.theme().muted_foreground;
        match self {
            PillMark::Icon(icon) => div()
                .text_color(muted)
                .child(Icon::new(icon).size_4().text_color(muted)),
            PillMark::Text { text, size } => div()
                .text_color(muted)
                .text_size(px(size))
                .whitespace_nowrap()
                .child(text),
        }
    }
}

/// 一条胶囊滑条。`state` 是调用方持有的 [`SliderState`]（阅读设置那张表每格
/// 一份），`value_text` 是**已经格式化好的**当前值（各格口径不同：`22.0`、
/// `80%`、`120 ms`、或「跟随系统」），两端标注见 [`PillMark`] —— 没有
/// 语义刻度的条（底栏进度）传 `None`。
#[derive(IntoElement)]
pub struct PillSlider {
    id: ElementId,
    state: Entity<SliderState>,
    value_text: SharedString,
    start: Option<PillMark>,
    end: Option<PillMark>,
    /// 可读名（屏幕阅读器念的这一条是什么）。落在交互面上。
    label: Option<SharedString>,
}

impl PillSlider {
    pub fn new(
        id: impl Into<ElementId>,
        state: Entity<SliderState>,
        value_text: impl Into<SharedString>,
        start: Option<PillMark>,
        end: Option<PillMark>,
    ) -> Self {
        Self {
            id: id.into(),
            state,
            value_text: value_text.into(),
            start,
            end,
            label: None,
        }
    }

    /// 可读名：屏幕阅读器念「哪一条」用的是它（旧滑条的同名口径）。
    pub fn accessibility_label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }
}

impl RenderOnce for PillSlider {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let state = self.state.clone();
        // 圆钮中心落在 pct × 宽度上：`left(relative(p))` 把左缘放到 p × 宽，
        // `ml(-半径)` 抵消回来 —— 与 base 组件滑条同一条算式。
        let pct = state.read(cx).percentage().end;

        // 容器转 stateful 并挂可读名：`aria_label` 在 `StatefulInteractiveElement`
        // 上（先 `.id()`），屏幕阅读器念的「哪一条」就是它。
        div()
            .id(self.id)
            .test_support()
            .aria_label(self.label.unwrap_or_default())
            .relative()
            .w_full()
            .h(px(THUMB))
            .child(
                // bounds 记录者：`update_value_by_position` 按它的矩形把指针位置
                // 折算成值。填充段与两端标注都挂它的 children。
                SliderIndicator::new(&state)
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(TRACK_TOP))
                    .h(px(TRACK))
                    .rounded_full()
                    .bg(theme.muted)
                    .overflow_hidden()
                    // 填充段：从左端到圆钮底下。圆钮盖住它的右缘，接缝不用管。
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .top_0()
                            .h_full()
                            .w(relative(pct))
                            .rounded_full()
                            .bg(theme.slider_bar),
                    )
                    .children(self.start.map(|mark| {
                        mark.element(cx)
                            .absolute()
                            .left(px(10.0))
                            .top_0()
                            .h_full()
                            .flex()
                            .items_center()
                    }))
                    .children(self.end.map(|mark| {
                        mark.element(cx)
                            .absolute()
                            .right(px(10.0))
                            .top_0()
                            .h_full()
                            .flex()
                            .items_center()
                    }))
                    // 交互面：点击跳值、鼠标拖、触摸拖。透明，只管事件。
                    .child(SliderTrack::new(&state).absolute().inset_0()),
            )
            .child(
                SliderThumb::new(&state)
                    .absolute()
                    .top_0()
                    .left(relative(pct))
                    .ml(-px(THUMB / 2.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(THUMB))
                    .rounded_full()
                    .bg(theme.background)
                    .shadow_md()
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(theme.foreground)
                            .whitespace_nowrap()
                            .child(self.value_text),
                    ),
            )
            // 鼠标的松手：与 base `Slider` 行为根同款的两条监听（up 在内 / up 在外），
            // 触摸那半边由轨道的 touch 层兜（Ended / Cancelled 都走 handle_release）。
            // 见模块头「松手」那段。
            .on_mouse_up(
                MouseButton::Left,
                window.listener_for(&state, |state, _, _, cx| state.handle_release(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                window.listener_for(&state, |state, _, _, cx| state.handle_release(cx)),
            )
    }
}

#[cfg(test)]
mod tests {
    //! 钉的是**接线**，不是样子：样子只有真机量得出来，而这里要守的是「一按一提必须是
    //! 完整的一对事件」—— 进度条把落点押在 `Release` 上（拖动只挪圆钮、松手才 seek），
    //! 胶囊要是漏发 Release，桌面上拖一回就是一次永远松不了的手，sync 从此不再把
    //! 正文位置推回来。触摸那一半的松手（Ended / Cancelled）归 base 轨道自己的测试。

    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui_kit::base::slider::{SliderEvent, SliderState};
    use gpui_kit::test::TestWindowExt as _;
    #[allow(unused_imports)] // `#[gpui_kit::test]` 展开要它，unused 分析看不见宏里的使用
    use gpui_kit::{AppContext as _, Modifiers, TestAppContext};
    use gpui_kit::{
        Context, Entity, IntoElement, MouseButton, ParentElement as _, Render, Styled as _, Window,
        div, point, px,
    };

    use super::PillSlider;

    /// 300×44 的固定座：胶囊是 `w_full`，铺满它就是 300 宽 —— 指示器的位置→值映射
    /// 边界在真画一帧时由 on_prepaint 记上，指针位置才有可信的折算。
    struct Holder {
        state: Entity<SliderState>,
    }

    impl Render for Holder {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(px(300.)).h(px(44.)).child(PillSlider::new(
                "pill-under-test",
                self.state.clone(),
                "50%",
                None,
                None,
            ))
        }
    }

    #[gpui_kit::test]
    fn a_mouse_gesture_on_the_pill_ends_with_a_release(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        // 返回的 `&mut VisualTestContext` 遮住外层的 `cx`（base 滑条测试同一形状）：
        // 后面读实体、挂订阅、画帧、派事件都走它，借用来回不打架。
        let (view, cx) = cx.add_window_view(|_, cx| Holder {
            state: cx.new(|_| {
                SliderState::new()
                    .min(0.0)
                    .max(1.0)
                    .step(0.001)
                    .default_value(0.0)
            }),
        });
        let state = view.read_with(cx, |holder, _| holder.state.clone());
        let events = Rc::new(RefCell::new(Vec::new()));
        cx.update(|_, cx| {
            let events = events.clone();
            cx.subscribe(&state, move |_, event: &SliderEvent, _| {
                events.borrow_mut().push(match event {
                    SliderEvent::Change(value) => ("change", value.end()),
                    SliderEvent::Release(value) => ("release", value.end()),
                });
            })
            .detach();
        });
        // 真画一帧：指示器的位置→值映射边界要靠 on_prepaint 记上（render_frame 失败
        // 会自己 panic，不返回 Result）。
        cx.update(|window, cx| window.render_frame(cx));

        // 按下去：点击跳值，Change 当场来；150 / 300 = 0.5（step 0.001 的网格上有个
        // 末位误差，tolerance 放到千分之一）。
        let middle = point(px(150.), px(20.));
        cx.simulate_mouse_down(middle, MouseButton::Left, Modifiers::default());
        // 松手：Release 必须跟上来。这条挂在胶囊的根上（up 在内 / up 在外两条），
        // 没有它就是这条测试的红。
        cx.simulate_mouse_up(middle, MouseButton::Left, Modifiers::default());

        let events = std::mem::take(&mut *events.borrow_mut());
        assert_eq!(events.len(), 2, "一按一提该是完整的一对：{events:?}");
        assert_eq!(events[0].0, "change");
        assert!(
            (events[0].1 - 0.5).abs() < 1e-3,
            "change 的值该是 0.5：{:?}",
            events
        );
        assert_eq!(events[1].0, "release");
        assert!(
            (events[1].1 - 0.5).abs() < 1e-3,
            "release 的值该是 0.5：{:?}",
            events
        );
    }
}
