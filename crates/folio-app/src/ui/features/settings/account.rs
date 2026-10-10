//! 账号：本地占位（一个可改的读者名），顶部是头像预览。
//!
//! 文件名与 `ui/shared/account.rs`（头像与显示名那对小工具）同名但不同层：
//! 这里是一**页**设置，共享模块只提供画头像的函数。

use gpui_kit::{ParentElement, Styled, div};

use super::*;
use crate::ui::shared::account::{account_avatar, display_name};

pub(super) fn account_page(cx: &App) -> SettingPage {
    // 头像那一行没有标签：它展示的就是下面那一项填出来的东西。
    let preview = SettingItem::render(move |_, _, cx| {
        let name = display_name(cx);
        div()
            .flex()
            .items_center()
            .gap_3()
            .child(account_avatar(&name, 44.0, cx))
            .child(div().text_lg().child(name))
    });
    let name = SettingItem::new(
        tr(cx, "field.account_name"),
        SettingField::input(
            |cx: &App| SharedString::from(ConfigHolder::account(cx).name),
            |val: SharedString, cx| {
                ConfigHolder::edit_account(cx, |a| a.name = val.trim().to_string());
            },
        ),
    )
    .description(tr(cx, "field.account_name.desc"))
    .keywords(["account", "name", "账号", "读者"]);

    page(
        tr(cx, "page.account"),
        IconName::CircleUser,
        vec![group(tr(cx, "group.reader"), vec![preview, name])],
    )
}
