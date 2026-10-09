//! 设置窗口：独立窗口、开-or-聚焦，不是主窗口里的一个页面，也不是对话框。
//!
//! 为什么是独立窗口：`Settings` 组件内部是一个 `h_resizable`（自己的页面导航 + 正文
//! 两栏），它和它的正文都要一个**确定的**盒子。组件默认的对话框只有 448px，扣掉侧栏后
//! 正文不到 480px 的堆叠阈值，每个字段会竖着堆成一列没法看的窄条。按设计指南，
//! 短决策用对话框，要看的字段多用整页 —— 整页在这里落成一个小窗口，
//! 改设置时不占用主窗口那条书名 / 进度 / 标签的线。
//!
//! 为什么是「开-or-聚焦」而不是每次新建：连点两下工具条的「设置」不该叠出两个窗口，
//! 而两个窗口各自记着不同的改动是无从合并的。

use gpui_kit::component::Root;
use gpui_kit::component::setting::{SelectIndex, Settings};
use gpui_kit::component::{TitleBar, h_flex, v_flex};
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Context, FontWeight, Global, IntoElement, ParentElement,
    Render, SharedString, Styled, TitlebarOptions, Window, WindowBounds, div, px, size,
};

use super::{SettingsPage, pages};
use crate::ui::shared::l10n::tr;

/// 设置窗口的初始与最小尺寸。
///
/// 最小尺寸是「每个控件都还画得出来」的下限：框架给横向字段大部分行宽做标签、
/// 给文本框留固定宽度，正文列必须同时容得下这两样。这个窗口还是下拉菜单量位置的盒子，
/// 窗口太窄就容不下贴着行末展开的菜单。
const WINDOW_START: (f32, f32) = (1000.0, 680.0);
const WINDOW_MIN: (f32, f32) = (760.0, 480.0);

/// 开着的那个设置窗口的句柄，用来让 [`open_at`] 聚焦而不是叠窗口。
///
/// 窗口被关掉之后这里留着的句柄就过期了：对它 `update` 会失败，于是同一趟里
/// 直接开一个新的，不需要谁去监听关闭事件把这里清空。
#[derive(Default)]
struct SettingsWindowState(Option<AnyWindowHandle>);

impl Global for SettingsWindowState {}

/// 打开设置窗口（停在第一页），或者聚焦已经开着的那个。
pub fn open(cx: &mut App) {
    open_at(SettingsPage::Typography, cx);
}

/// 打开设置窗口并定位到 `page`，或者聚焦已经开着的那个。
///
/// 只有**新开**窗口时 `page` 才起作用：已经开着的窗口停在它自己那一页，
/// 把它拽到别处会打断读者正在改的那一项。
pub fn open_at(page: SettingsPage, cx: &mut App) {
    let existing = cx.try_global::<SettingsWindowState>().and_then(|s| s.0);
    let focused = existing.is_some_and(|handle| {
        handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    });
    if focused {
        return;
    }

    let mut options = TitleBar::window_options();
    options.titlebar = Some(TitlebarOptions {
        title: Some(tr(cx, "settings.title")),
        ..TitleBar::title_bar_options()
    });
    options.window_bounds = Some(WindowBounds::centered(
        size(px(WINDOW_START.0), px(WINDOW_START.1)),
        cx,
    ));
    options.window_min_size = Some(size(px(WINDOW_MIN.0), px(WINDOW_MIN.1)));

    let opened = cx.open_window(options, move |window, cx| {
        // 记下句柄要趁闭包还没返回：下一次点「设置」用的就是它。
        cx.set_global(SettingsWindowState(Some(window.window_handle())));
        let view = cx.new(|_| SettingsView::new(page));
        cx.new(|cx| Root::new(view, window, cx))
    });
    if let Err(err) = opened {
        // 与主窗口打不开同一档事实：不该发生，也没有可恢复的东西。
        eprintln!("打开设置窗口失败：{err}");
    }
}

/// 关掉设置窗口（如果开着）。
///
/// 设置是主窗口的同伴，不是独立的一层：主窗口关掉（或退出）时这条也该跟着走，
/// 而不是飘在桌面上。之后再开就是新窗口 —— 里面没有值得留下的状态，
/// 每一项改动都当场落盘了。
pub fn close(cx: &mut App) {
    if let Some(handle) = cx.try_global::<SettingsWindowState>().and_then(|s| s.0) {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }
    cx.set_global(SettingsWindowState(None));
}

/// 设置窗口的根视图：窗口 chrome + `Settings` 整页。
struct SettingsView {
    /// 要定位到的那一页。**只在第一次渲染时取用** —— 组件的选择状态挂在
    /// `window.use_keyed_state(id)` 上，之后再传 `default_selected_index` 也没用，
    /// 所以取走之后就置 `None`，窗口内换页由组件自己管。
    initial: Option<usize>,
}

impl SettingsView {
    fn new(page: SettingsPage) -> Self {
        Self {
            initial: Some(page.index()),
        }
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title: SharedString = tr(cx, "settings.title");
        // 侧栏保持可调宽：它是这一页的导航，页面多于四屏时靠它跳，
        // 但正文才是读者真正在改的东西。
        let settings = Settings::new("folio-settings").pages(pages(cx));
        // `take` 一次：第二帧起这一页的选择就交给组件里的 keyed state。
        let settings = match self.initial.take() {
            Some(page_ix) => settings.default_selected_index(SelectIndex {
                page_ix,
                group_ix: None,
            }),
            None => settings,
        };
        v_flex()
            .size_full()
            .child(
                TitleBar::new().child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(title)),
                ),
            )
            .child(div().flex_1().min_h_0().child(settings))
    }
}
