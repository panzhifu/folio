//! 设置整页：主窗口里的一屏，不是独立窗口，也不是对话框。
//!
//! 这一屏以前是一份独立的开-or-聚焦小窗口（`settings/window.rs`）。删掉它的理由：
//! `Settings` 组件内部是一个 `h_resizable`（页面导航 + 正文两栏），它要一个**确定的**盒子 ——
//! 而主窗口的内容区正是这样一个盒子，不必再开一个 OS 窗口去拿。少一个窗口也少一处
//! 「这功能那端有没有」的问题，`open` / `open_at` / `close` 和那份记窗口句柄的 `Global`
//! 跟着一起没了：这一屏和书架、统计、笔记是同一格里并排的四个目的地之一。
//!
//! 页面导航那一栏比框架默认（250px）窄，也比参照的那份（190px）窄：这一页旁边还站着应用
//! 自己的导航栏（48 或 260px），而独立窗口的旁边没有它。正文窄到 480px 以下时组件会把
//! 每项字段竖着堆起来（框架自己的窄档排版，不是坏了），窗口拉宽就回到一行一项。

use gpui_kit::component::group_box::GroupBoxVariant;
use gpui_kit::component::setting::{SelectIndex, Settings};
use gpui_kit::component::{ActiveTheme, v_flex};
use gpui_kit::{
    Context, ElementId, InteractiveElement, IntoElement, ParentElement, Render, Styled,
    TestSupportExt, Window, px,
};

use super::{SettingsPage, pages};

/// 页面导航那一栏的宽与可调范围。
const SIDEBAR_WIDTH: f32 = 170.0;
const SIDEBAR_MIN: f32 = 140.0;
const SIDEBAR_MAX: f32 = 220.0;

/// 「我的」那一个目的地，以及阅读页「设置」那一格所指向的一屏。
pub struct SettingsView {
    /// 要定位到的那一页。**只在第一次渲染时取用** —— 组件的选择状态挂在
    /// `window.use_keyed_state(id)` 上，之后再传 `default_selected_index` 也没用，
    /// 所以取走之后就置 `None`，屏内换页由组件自己管。
    initial: Option<usize>,
}

impl SettingsView {
    pub fn new(page: SettingsPage) -> Self {
        Self {
            initial: Some(page.index()),
        }
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 组件的键按实例一份：同一扇窗口里可以同时站着两屏（「我的」那一格的根，
        // 和从阅读页压进来的那一屏），共用键就是共用选择状态与共用那条可拖拽的分隔线 ——
        // 于是第二屏会无视它自己带来的那一页，停在第一屏停过的地方。
        let settings = Settings::new(ElementId::from(("folio-settings", cx.entity().entity_id())))
            .with_group_variant(GroupBoxVariant::Fill)
            .sidebar_width(px(SIDEBAR_WIDTH))
            .sidebar_size_range(px(SIDEBAR_MIN)..px(SIDEBAR_MAX))
            .pages(pages(cx));
        // `take` 一次：第二帧起这一页的选择就交给组件里的 keyed state。
        let settings = match self.initial.take() {
            Some(page_ix) => settings.default_selected_index(SelectIndex {
                page_ix,
                group_ix: None,
            }),
            None => settings,
        };
        v_flex()
            .id("folio-settings")
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // 宿主测试按这一格确认「我的」那一个目的地画的确实是设置整页。
            .test_support()
            .child(settings)
    }
}
