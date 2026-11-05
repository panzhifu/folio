//! 「没拿到所有文件访问」那一问：Android 上挑目录之前的一道门。
//!
//! 为什么摆在这里而不是各屏写一遍：按到这一格的入口有三处（设置的「书库」、首次启动那屏、
//! 书架的导入面板），问的是同一件事、答的也是同样三条。三个纵切各写一遍的话，将来加一条
//! 回答就会只加进一屏。
//!
//! 三条回答都直接落到 [`crate::ui::services::library`]，组件自己不留状态：待答的是哪一格
//! 连着当初那个回调一起存在服务那份实体里（`Pending`），所以这里不需要知道按下的是谁。

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};
use gpui_kit::{AnyElement, App, FontWeight, IntoElement, ParentElement as _, Styled as _, div};

use crate::ui::services::l10n::tr;
use crate::ui::services::library;

/// 那一问的面板：一句条件、一句后果、三颗钮。
///
/// 排法照规范那条「一屏一个默认承诺」：`取消` 与 `照样挑` 都是 ghost，`去开启` 是这一组里
/// 默认要走的那一条（primary）。`取消` 只收面板、什么都不做（`dismiss`）；另两颗各重跑一次
/// 那道门，差别只在**没拿到授权时开不开选择器**（`answer` 的 `grant_first`）。
pub fn access_question(cx: &App) -> AnyElement {
    v_flex()
        .w_full()
        .gap_2()
        .child(
            div()
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .child(tr(cx, "library.access.title")),
        )
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(tr(cx, "library.access.body")),
        )
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .child(
                    Button::new("access-question-cancel")
                        .ghost()
                        .label(tr(cx, "shelf.cancel").to_string())
                        .on_click(|_, _, cx| library::dismiss(cx)),
                )
                .child(
                    Button::new("access-question-anyway")
                        .ghost()
                        .label(tr(cx, "action.pick_anyway").to_string())
                        .on_click(|_, _, cx| library::answer(cx, false)),
                )
                .child(
                    Button::new("access-question-grant")
                        .primary()
                        .label(tr(cx, "action.grant_access").to_string())
                        .on_click(|_, _, cx| library::answer(cx, true)),
                ),
        )
        .into_any_element()
}

/// 这一问现在是不是在等这一格回答。调用方拿它决定摆面板还是摆自己的按钮。
pub fn waiting_for(pick: library::Pick, cx: &App) -> bool {
    library::pending_pick(cx) == Some(pick)
}
