//! 同步：WebDAV / 百度 / 阿里云盘三档配置，最上面一颗胶囊按钮切换，下面是对应的地址与账号。
//!
//! 一页两组：**连哪儿**（顶部胶囊 + 该档的字段）、**现在什么状态**与两颗钮。收费门禁只有
//! WebDAV 快照同步这一条（判据是 [`activation::premium`]）；局域网同步的入口不再摆在这一屏，
//! 它的服务与状态留在 `services::lansync` 里，将来另找入口。
//!
//! 密码与密钥按普通文本行摆：本机 `config.json` 本来就以 0600 保存着它们，遮罩只防肩窥；
//! 桌面那套设置行没有密码档，为它单独造一种只在一端生效的样子不值。
//!
//! 点输入框外面那一击会收掉焦点（见 `compact.rs` 的 `dismiss_on_outside_tap`）：设置页
//! 这一层没有「提交」钮，键盘不主动收就会一直顶着。

use super::super::*;
use super::membership;
use crate::ui::components::muted_label;
use crate::ui::services::activation;
use crate::ui::services::sync::{self, Failure, Status};
use folio_core::SyncProvider;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, InteractiveElement as _, ParentElement as _, StatefulInteractiveElement as _,
    Styled as _, TestSupportExt as _, div, px,
};

pub(in crate::ui::features::settings) fn sync_page(cx: &App) -> Page {
    // 门禁：WebDAV 快照同步是会员功能（判据只有 activation::premium 一条）。非会员这一路
    // 整段换成一句引导；局域网同步不再摆在这一屏，见模块头。
    let groups = if activation::premium(cx) {
        vec![server_group(cx), action_group(cx)]
    } else {
        vec![membership::gate_group("membership.gate_sync", cx)]
    };
    page(tr(cx, "page.sync"), IconName::ArrowDownUp, groups)
}

fn server_group(cx: &App) -> Group {
    let provider = ConfigHolder::sync(cx).provider;
    let mut rows = vec![
        // 最上面那颗胶囊：三档配置一眼可见，点哪档就切哪档，下面的字段跟着换。
        Row::custom(move |_, cx| provider_switch(cx)).keywords(&[
            "provider", "webdav", "baidu", "aliyun", "网盘", "同步", "后端",
        ]),
        Row::toggle(
            tr(cx, "field.sync_enabled"),
            |cx: &App| ConfigHolder::sync(cx).enabled,
            |on, cx| ConfigHolder::edit_sync(cx, |sync| sync.enabled = on),
        )
        .description(tr(cx, "field.sync_enabled.desc"))
        .keywords(&["sync", "webdav", "同步", "备份"]),
    ];
    match provider {
        SyncProvider::WebDav => {
            rows.push(
                Row::text(
                    tr(cx, "field.sync_url"),
                    |cx: &App| SharedString::from(ConfigHolder::sync(cx).url),
                    |value: SharedString, cx| {
                        ConfigHolder::edit_sync(cx, |sync| sync.url = value.to_string())
                    },
                )
                .description(tr(cx, "field.sync_url.desc"))
                .keywords(&["url", "webdav", "server", "地址", "服务器"]),
            );
            rows.push(
                Row::text(
                    tr(cx, "field.sync_user"),
                    |cx: &App| SharedString::from(ConfigHolder::sync(cx).username),
                    |value: SharedString, cx| {
                        ConfigHolder::edit_sync(cx, |sync| sync.username = value.to_string())
                    },
                )
                .description(tr(cx, "field.sync_user.desc"))
                .keywords(&["user", "username", "账号", "用户名"]),
            );
            rows.push(
                Row::text(
                    tr(cx, "field.sync_password"),
                    |cx: &App| SharedString::from(ConfigHolder::sync(cx).password),
                    |value: SharedString, cx| {
                        ConfigHolder::edit_sync(cx, |sync| sync.password = value.to_string())
                    },
                )
                .description(tr(cx, "field.sync_password.desc"))
                .keywords(&["password", "密码", "口令"]),
            );
        }
        SyncProvider::Baidu => {
            // 百度那一头的凭据不在这里填：与「网盘取书」共用一份登录，少一处要维护的
            // token。这一行只说清它落在哪个目录。
            rows.push(
                Row::custom(move |_, cx| {
                    muted_label(tr(cx, "field.sync_baidu_hint"), cx).into_any_element()
                })
                .keywords(&["baidu", "百度", "网盘", "登录", "token"]),
            );
        }
        SyncProvider::Aliyun => {
            // 与百度同一条：凭据在「网盘取书」那一屏，这里只说落哪个目录。
            rows.push(
                Row::custom(move |_, cx| {
                    muted_label(tr(cx, "field.sync_aliyun_hint"), cx).into_any_element()
                })
                .keywords(&["aliyun", "阿里云盘", "网盘", "登录", "token"]),
            );
        }
    }
    rows.push(
        Row::text(
            tr(cx, "field.sync_dir"),
            |cx: &App| SharedString::from(ConfigHolder::sync(cx).remote_dir),
            |value: SharedString, cx| {
                ConfigHolder::edit_sync(cx, |sync| sync.remote_dir = value.to_string())
            },
        )
        .description(tr(cx, "field.sync_dir.desc"))
        .keywords(&["dir", "folder", "目录", "远端"]),
    );
    group(tr(cx, "group.sync_server"), rows)
}

/// 顶部那颗胶囊：WebDAV / 百度 / 阿里云盘三档，选中那半用主色填实。
///
/// 形状照听书屏那颗「封面 / 文稿」与自动阅读那颗分段胶囊：外层 `muted` 全圆角，
/// 三段等宽；选中态同时给颜色与 `aria_selected`，读屏也能分出来。
fn provider_switch(cx: &App) -> AnyElement {
    let current = ConfigHolder::sync(cx).provider;
    let primary = cx.theme().primary;
    let primary_fg = cx.theme().primary_foreground;
    let muted_fg = cx.theme().muted_foreground;
    let mut row = h_flex()
        .w_full()
        .items_center()
        .gap_1()
        .p_0p5()
        .rounded_full()
        .bg(cx.theme().muted);
    for (id, key, provider) in [
        (
            "sync-provider-webdav",
            "sync.provider_webdav",
            SyncProvider::WebDav,
        ),
        (
            "sync-provider-baidu",
            "sync.provider_baidu",
            SyncProvider::Baidu,
        ),
        (
            "sync-provider-aliyun",
            "sync.provider_aliyun",
            SyncProvider::Aliyun,
        ),
    ] {
        let on = current == provider;
        let label = tr(cx, key);
        let mut segment = div()
            .id(id)
            .test_support()
            .flex_1()
            .px(px(14.))
            .py(px(6.))
            .rounded_full()
            .text_sm()
            .text_center()
            .aria_selected(on)
            .aria_label(label.clone())
            .child(label);
        segment = if on {
            segment.bg(primary).text_color(primary_fg)
        } else {
            segment.text_color(muted_fg)
        };
        row = row.child(segment.on_click(move |_, _, cx| {
            ConfigHolder::edit_sync(cx, |sync| sync.provider = provider)
        }));
    }
    row.into_any_element()
}

fn action_group(cx: &App) -> Group {
    group(
        tr(cx, "group.sync_state"),
        vec![
            Row::custom(move |_, cx| {
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

#[cfg(test)]
mod tests {
    //! 顶部胶囊是 `Row::custom`：内容由闭包现渲染，只有**真开一次窗**才走得到
    //! （`spec_pages` 那一层只把闭包装起来，不叫它）。这一条钉的是三档都在，以及
    //! 局域网那一组真的从这一屏退场了。

    use folio_core::Config;
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AnyWindowHandle, AppContext as _, TestAppContext, px, size};

    use crate::ui::features::settings::view::SettingsPage;
    use crate::ui::features::settings::view::desktop::SettingsView;
    use crate::ui::services::config::ConfigHolder;

    /// 渲染一帧，问那一颗钮在不在树里。
    fn present(handle: AnyWindowHandle, id: &'static str, cx: &mut TestAppContext) -> bool {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.try_find(id).is_some()
        })
        .expect("窗口应能渲染")
    }

    #[gpui_kit::test]
    async fn the_provider_capsules_render_and_the_lan_group_is_gone(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let handle = cx
            .open_window(size(px(900.), px(1400.)), |window, cx| {
                ConfigHolder::install(Config::default(), cx);
                // 同步是会员功能：先把永久位摆上，绕开门禁。
                ConfigHolder::edit_membership(cx, |membership| membership.lifetime = true);
                let view = cx.new(|cx| SettingsView::new(SettingsPage::Sync, cx));
                Root::new(view, window, cx)
            })
            .into();

        for id in [
            "sync-provider-webdav",
            "sync-provider-baidu",
            "sync-provider-aliyun",
        ] {
            assert!(present(handle, id, cx), "顶部胶囊该有三档：{id}");
        }
        assert!(present(handle, "settings-sync-now", cx), "同步那颗钮还在");
        assert!(
            !present(handle, "settings-lan-sync", cx),
            "局域网同步那一组已从这一屏删去"
        );
        assert!(
            !present(handle, "settings-lan-generate", cx),
            "局域网密钥那一行也删去了"
        );
    }
}
