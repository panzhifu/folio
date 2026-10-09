//! 「统计」目的地。还没有内容，先给一个说清「这里缺什么」的空状态 ——
//! 「暂无数据」和「这台设备上还没有这个功能」必须能分开说，见 `widgets::empty_state`。

use gpui_kit::component::ActiveTheme;
use gpui_kit::{Context, IntoElement, ParentElement, Render, Styled, Window, div};

use crate::ui::shared::l10n::tr;
use crate::ui::widgets::empty_state;

#[derive(Default)]
pub struct StatsView;

impl StatsView {
    pub fn new() -> Self {
        Self
    }
}

impl Render for StatsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(empty_state(
                tr(cx, "nav.stats"),
                tr(cx, "nav.pending.stats"),
                cx,
            ))
    }
}
