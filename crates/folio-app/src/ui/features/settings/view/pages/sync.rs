//! 同步：WebDAV 地址与账号、手动测试与立即同步。
//!
//! 一页两组：上面是**连哪儿**（地址、账号、远端目录），下面是**现在什么状态**与两颗钮。
//! 同步状态读 [`crate::ui::services::sync`] 的实体，所以后台那趟回来会自己重绘 ——
//! 观察者挂在设置两屏的构造上（与检查更新那条链同形）。
//!
//! 密码按普通文本行摆：本机 `config.json` 本来就以 0600 保存着它，遮罩只防肩窥；
//! 桌面那套设置行没有密码档，为它单独造一种只在一端生效的样子不值。

use super::super::*;
use crate::ui::components::muted_label;
use crate::ui::services::sync::{self, Failure, Status};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Disableable as _, Sizable as _, h_flex, v_flex};

pub(in crate::ui::features::settings) fn sync_page(cx: &App) -> Page {
    page(
        tr(cx, "page.sync"),
        IconName::ArrowDownUp,
        vec![server_group(cx), action_group(cx)],
    )
}

fn server_group(cx: &App) -> Group {
    group(
        tr(cx, "group.sync_server"),
        vec![
            Row::toggle(
                tr(cx, "field.sync_enabled"),
                |cx: &App| ConfigHolder::sync(cx).enabled,
                |on, cx| ConfigHolder::edit_sync(cx, |sync| sync.enabled = on),
            )
            .description(tr(cx, "field.sync_enabled.desc"))
            .keywords(&["sync", "webdav", "同步", "备份"]),
            Row::text(
                tr(cx, "field.sync_url"),
                |cx: &App| SharedString::from(ConfigHolder::sync(cx).url),
                |value: SharedString, cx| {
                    ConfigHolder::edit_sync(cx, |sync| sync.url = value.to_string())
                },
            )
            .description(tr(cx, "field.sync_url.desc"))
            .keywords(&["url", "webdav", "server", "地址", "服务器"]),
            Row::text(
                tr(cx, "field.sync_user"),
                |cx: &App| SharedString::from(ConfigHolder::sync(cx).username),
                |value: SharedString, cx| {
                    ConfigHolder::edit_sync(cx, |sync| sync.username = value.to_string())
                },
            )
            .description(tr(cx, "field.sync_user.desc"))
            .keywords(&["user", "username", "账号", "用户名"]),
            Row::text(
                tr(cx, "field.sync_password"),
                |cx: &App| SharedString::from(ConfigHolder::sync(cx).password),
                |value: SharedString, cx| {
                    ConfigHolder::edit_sync(cx, |sync| sync.password = value.to_string())
                },
            )
            .description(tr(cx, "field.sync_password.desc"))
            .keywords(&["password", "密码", "口令"]),
            Row::text(
                tr(cx, "field.sync_dir"),
                |cx: &App| SharedString::from(ConfigHolder::sync(cx).remote_dir),
                |value: SharedString, cx| {
                    ConfigHolder::edit_sync(cx, |sync| sync.remote_dir = value.to_string())
                },
            )
            .description(tr(cx, "field.sync_dir.desc"))
            .keywords(&["dir", "folder", "目录", "远端"]),
        ],
    )
}

fn action_group(cx: &App) -> Group {
    group(
        tr(cx, "group.sync_state"),
        vec![
            Row::custom(move |cx| {
                let entity = sync::handle(cx);
                let (status, busy) = entity
                    .as_ref()
                    .map(|entity| {
                        let state = entity.read(cx);
                        (state.status().clone(), state.busy())
                    })
                    .unwrap_or((Status::Idle, false));
                let enabled = ConfigHolder::sync(cx).enabled;
                let text = status_text(cx, &status);
                let detail = status_detail(cx, &status);

                let mut column = v_flex()
                    .min_w_0()
                    .flex_1()
                    .gap_0p5()
                    .child(div().text_base().text_ellipsis().child(text));
                if let Some(detail) = detail {
                    column = column.child(muted_label(detail, cx));
                }

                h_flex()
                    .w_full()
                    .items_center()
                    .gap_3()
                    .child(column)
                    .child(
                        Button::new("settings-sync-test")
                            .ghost()
                            .small()
                            .label(tr(cx, "action.sync_test").to_string())
                            .disabled(busy)
                            .on_click(|_, _, cx| sync::test_now(cx)),
                    )
                    .child(
                        Button::new("settings-sync-now")
                            .ghost()
                            .small()
                            .label(tr(cx, "action.sync_now").to_string())
                            .disabled(busy || !enabled)
                            .on_click(|_, _, cx| sync::sync_now(cx)),
                    )
                    .into_any_element()
            })
            .keywords(&["sync", "webdav", "同步", "状态", "测试", "连接"]),
        ],
    )
}

fn status_text(cx: &App, status: &Status) -> SharedString {
    match status {
        Status::Idle => tr(cx, "sync.idle"),
        Status::Testing => tr(cx, "sync.testing"),
        Status::Tested => tr(cx, "sync.test_ok"),
        Status::Syncing => tr(cx, "sync.running"),
        Status::Done(report) => {
            let positions = report.applied_positions.to_string();
            let annotations = report.applied_annotations.to_string();
            let bookmarks = report.applied_bookmarks.to_string();
            let deleted = (report.deleted_annotations + report.deleted_bookmarks).to_string();
            tf(
                cx,
                "sync.done",
                &[
                    ("positions", &positions),
                    ("annotations", &annotations),
                    ("bookmarks", &bookmarks),
                    ("deleted", &deleted),
                ],
            )
            .into()
        }
        Status::Failed(failure) => tr(cx, failure_key(*failure)),
    }
}

fn status_detail(cx: &App, status: &Status) -> Option<SharedString> {
    let Status::Done(report) = status else {
        return None;
    };
    if report.skipped_books > 0 {
        let skipped = report.skipped_books.to_string();
        return Some(tf(cx, "sync.skipped", &[("n", &skipped)]).into());
    }
    None
}

fn failure_key(failure: Failure) -> &'static str {
    match failure {
        Failure::Config => "sync.fail_config",
        Failure::Auth => "sync.fail_auth",
        Failure::Network => "sync.fail_network",
        Failure::Remote => "sync.fail_remote",
        Failure::Store => "sync.fail_store",
    }
}
