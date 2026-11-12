//! 胶囊滑条：两端标注、圆钮上直接写当前值的那一种（阅读设置那一族）。
//!
//! 与 [`gpui_kit::component::slider::Slider`](裸轨道) 的差别在**信息摆放**：
//! 语义刻度画在胶囊两端（两颗太阳、大 A 与小 A、大 / 小的边距），当前值不再
//! 单独占一行，而是写进圆钮 —— 眼睛不用在滑条与数值之间来回跳。
//!
//! 交互**全部复用** gpui-base 的无样式滑条原语：[`SliderIndicator`] 在
//! `on_prepaint` 里记录位置→值的映射边界，[`SliderTrack`] 提供点击跳值、鼠标
//! 拖与触摸拖（GPUI 的 `on_drag` + `TouchDragEvent` 两族都接），[`SliderThumb`]
//! 自带拖动管线且 `children` 可放内容 —— 圆钮上的数值文本就是它的 child。
//! 状态仍是 [`SliderState`]：`SliderEvent::Change` 的订阅方（阅读设置那张表）
//! 一行不改，这一层只画皮。
//!
//! 几何与 base 的定位映射同一条：圆钮中心落在 `percentage × 宽度` 上
//! （`.left(relative(p))` + `ml(-半径)`），指针位置与圆钮数值的对应关系和裸
//! 滑条完全一致；两端不留内缩，圆钮在极值处探出半个身位 —— 设计稿同形。

use gpui_kit::assets::IconName;
use gpui_kit::base::slider::{SliderIndicator, SliderState, SliderThumb, SliderTrack};
use gpui_kit::component::{ActiveTheme as _, Icon};
use gpui_kit::{
    App, Div, ElementId, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _,
    Window, div, px, relative,
};

/// 胶囊轨道的高度。圆钮比它大一圈（上下各探出一点），设计稿同形。
const TRACK: f32 = 32.0;
/// 圆钮直径。
const THUMB: f32 = 44.0;
/// 圆钮相对容器的上探量：`(TRACK - THUMB) / 2`，让圆钮与轨道垂直居中。
const THUMB_TOP: f32 = (TRACK - THUMB) / 2.0;

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
/// `80%`、`120 ms`、或「跟随系统」），两端标注见 [`PillMark`]。
#[derive(IntoElement)]
pub struct PillSlider {
    id: ElementId,
    state: Entity<SliderState>,
    value_text: SharedString,
    start: PillMark,
    end: PillMark,
    /// 可读名（屏幕阅读器念的这一条是什么）。落在交互面上。
    label: Option<SharedString>,
}

impl PillSlider {
    pub fn new(
        id: impl Into<ElementId>,
        state: Entity<SliderState>,
        value_text: impl Into<SharedString>,
        start: PillMark,
        end: PillMark,
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
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
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
                    .top(-px(THUMB_TOP))
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
                    .child(
                        self.start
                            .element(cx)
                            .absolute()
                            .left(px(10.0))
                            .top_0()
                            .h_full()
                            .flex()
                            .items_center(),
                    )
                    .child(
                        self.end
                            .element(cx)
                            .absolute()
                            .right(px(10.0))
                            .top_0()
                            .h_full()
                            .flex()
                            .items_center(),
                    )
                    // 交互面：点击跳值、鼠标拖、触摸拖。透明，只管事件。
                    .child(SliderTrack::new(&state).absolute().inset_0()),
            )
            .child(
                SliderThumb::new(&state)
                    .absolute()
                    .top(px(THUMB_TOP))
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
    }
}
