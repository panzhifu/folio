//! 「笔记」目的地。同 [`super::stats`]：还没有内容，先给一个说清缺什么的空状态。

use gpui_kit::component::ActiveTheme;
use gpui_kit::{Context, IntoElement, ParentElement, Render, Styled, Window, div};

use crate::ui::shared::l10n::tr;
use crate::ui::widgets::empty_state;

pub struct NotesView;

impl NotesView {
    pub fn new() -> Self {
        Self
    }
}

impl Render for NotesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(empty_state(
                tr(cx, "nav.notes"),
                tr(cx, "nav.pending.notes"),
                cx,
            ))
    }
}
