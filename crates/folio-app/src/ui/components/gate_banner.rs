//! 会员引导卡：一处画「这个功能要会员」的那句话与那一颗开通钮。
//!
//! 门禁的口径全仓库只有一条：[`crate::ui::services::activation::premium`]。
//! 这件组件只管**长相**：一个皇冠章、一句「这是什么功能、开了能得到什么」、
//! 一颗把读者交给爱发电的钮。摆它的地方（各设置页与网盘浏览器）自己决定
//! 非会员时整页内容收不收 —— 判据在那边，句子的措辞也在那边。

use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme, Icon, Sizable as _, h_flex};
use gpui_kit::{AnyElement, App, IntoElement, ParentElement as _, SharedString, Styled as _, div};

use crate::ui::platform;
use crate::ui::services::activation::AFDIAN_PAGE;
use crate::ui::services::l10n::tr;

/// 一句「要会员」的说明 + 一颗开通钮。放进设置行的卡里、或整页空态里都站得住。
///
/// 钮打开的是爱发电主页而不是会员页：门禁卡的读者要的是**去买到**，兑换那一步在
/// 会员页上等他回来。`open_url` 的返回值只影响一行日志，同会员页那一颗。
pub fn gate_banner(text: impl Into<SharedString>, cx: &App) -> AnyElement {
    let text: SharedString = text.into();
    h_flex()
        .w_full()
        .items_center()
        .gap_3()
        .child(
            div()
                .size_8()
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(cx.theme().primary.opacity(0.12))
                .child(
                    Icon::new(IconName::Crown)
                        .small()
                        .text_color(cx.theme().primary),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .child(text),
        )
        .child(
            Button::new("gate-banner-open")
                .small()
                .label(tr(cx, "action.membership_open").to_string())
                .on_click(|_, _, _| {
                    if !platform::open_url(AFDIAN_PAGE) {
                        tracing::warn!("打不开爱发电主页：{AFDIAN_PAGE}");
                    }
                }),
        )
        .into_any_element()
}
