//! 同步：WebDAV 的地址与账号、局域网的账号与对端，两边各一颗「现在就来一趟」。
//!
//! 一页三组：**连哪儿**（WebDAV 地址、账号、远端目录）、**现在什么状态**与两颗钮、**局域网同步**
//! （账号密钥、对端地址、那颗「现在就同步」）。两条路各自的状态读各自的实体 —— WebDAV 那一趟是
//! [`crate::ui::services::sync`]，局域网那一趟是 [`crate::ui::services::lansync`]，所以后台回来
//! 都会自己重绘；观察者挂在设置两屏的构造上（与检查更新那条链同形）。
//!
//! 两条路彼此独立：WebDAV 走读者自己的存储服务器，局域网走同一网段里另一台设备上的 folio，
//! 共用同一份合并规则（`folio_core::sync`）但互不假设对方存在。
//!
//! 密码与密钥按普通文本行摆：本机 `config.json` 本来就以 0600 保存着它们，遮罩只防肩窥；
//! 桌面那套设置行没有密码档，为它单独造一种只在一端生效的样子不值。

use super::super::*;
use super::membership;
use crate::ui::components::muted_label;
use crate::ui::services::activation;
use crate::ui::services::lansync::{self, Status as LanStatus};
use crate::ui::services::sync::{self, Failure, Status};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{AnyElement, FontWeight};

pub(in crate::ui::features::settings) fn sync_page(cx: &App) -> Page {
    // 门禁：WebDAV 快照同步是会员功能（判据只有 activation::premium 一条）。非会员这一路
    // 整段换成一句引导，局域网同步照旧 —— 收费的是「进度批注跟着走」那一档，
    // 不是「局域网里两台设备」本身。
    let mut groups = if activation::premium(cx) {
        vec![server_group(cx), action_group(cx)]
    } else {
        vec![membership::gate_group("membership.gate_sync", cx)]
    };
    // 顺序按「一条路连着摆」：WebDAV 的字段紧接它的状态与两颗钮，局域网那一组整体排在
    // 最后。把新东西插在中间会把读者已经熟悉的「测试连接 / 立即同步」往下推 —— 首屏
    // 放不下时那就是「按钮不见了」。
    groups.push(lan_group(cx));
    page(tr(cx, "page.sync"), IconName::ArrowDownUp, groups)
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

/// 局域网同步：账号密钥、对端地址、校验码，加一颗「现在就去那一台走一趟」。
///
/// 这一组里只有最后那颗钮会出门：填地址、生成本机密钥都只是写配置（`config.json`，0600）。
/// 而**被同步的那一头**要求对面那台把 Wi-Fi 传书开着 —— 那五条路由寄生在同一台服务器上，
/// 服务又不写进配置（重启不自开），所以这句话写在开关的说明里，别让人以为点了就行。
fn lan_group(cx: &App) -> Group {
    group(
        tr(cx, "group.sync_lan"),
        vec![
            Row::toggle(
                tr(cx, "field.lan_enabled"),
                |cx: &App| ConfigHolder::account(cx).enabled,
                |on: bool, cx| ConfigHolder::edit_account(cx, |account| account.enabled = on),
            )
            .description(tr(cx, "field.lan_enabled.desc"))
            .keywords(&["lan", "局域网", "wifi", "同步", "设备", "account"]),
            Row::custom_titled(tr(cx, "field.lan_check"), lan_check_row),
            Row::text(
                tr(cx, "field.lan_key"),
                |cx: &App| SharedString::from(ConfigHolder::account(cx).key),
                |value: SharedString, cx| {
                    ConfigHolder::edit_account(cx, |account| account.key = value.to_string())
                },
            )
            .description(tr(cx, "field.lan_key.desc"))
            .keywords(&["key", "密钥", "账号", "配对", "lan"]),
            Row::text(
                tr(cx, "field.lan_peer"),
                |cx: &App| SharedString::from(ConfigHolder::account(cx).peer),
                |value: SharedString, cx| {
                    ConfigHolder::edit_account(cx, |account| account.peer = value.to_string())
                },
            )
            .description(tr(cx, "field.lan_peer.desc"))
            .keywords(&["peer", "地址", "ip", "对端", "lan"]),
            Row::custom(lan_action_row)
                .description(tr(cx, "field.lan_state.desc"))
                .keywords(&["lan", "局域网", "同步", "状态", "立即同步"]),
        ],
    )
}

/// 校验码那一行：8 个字符，两台设备各摆一次，对一眼就知道中间没有第三台设备塞进这一段局域网。
///
/// 没有密钥时这一格摆的是「本机还没有账号」与那颗生成钮 —— 有了密钥之后那颗钮就消失：
/// 重新生成等于把这一台从账号里摘出去，而这一屏上没有别的地方能把旧的那一串找回来。
fn lan_check_row(cx: &App) -> AnyElement {
    match lansync::check_code(cx) {
        None => h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .child(muted_label(tr(cx, "sync.lan_no_key"), cx)),
            )
            .child(
                Button::new("settings-lan-generate")
                    .ghost()
                    .small()
                    .label(tr(cx, "action.lan_generate").to_string())
                    .on_click(|_, _, cx| {
                        lansync::generate_key(cx);
                    }),
            )
            .into_any_element(),
        Some(code) => h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_ellipsis()
                    .child(code),
            )
            .into_any_element(),
    }
}

/// 状态那一行与「立即同步（局域网）」。按钮在飞的时候禁用，屏幕上的那句话由实体里的状态来。
fn lan_action_row(cx: &App) -> AnyElement {
    let entity = lansync::handle(cx);
    let (status, busy) = entity
        .as_ref()
        .map(|entity| {
            let state = entity.read(cx);
            (state.status().clone(), state.busy())
        })
        .unwrap_or((LanStatus::Idle, false));
    // 能出门的三件事缺一不可：开关、一把解得开的密钥、一个读得出来的对端地址。
    let ready = lansync::key(cx).is_some() && lansync::peer_usable(&ConfigHolder::account(cx).peer);
    let text = lan_status_text(cx, &status);
    let detail = lan_status_detail(cx, &status);

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
            Button::new("settings-lan-sync")
                .ghost()
                .small()
                .label(tr(cx, "action.lan_sync").to_string())
                .disabled(busy || !ready)
                .on_click(|_, _, cx| lansync::sync_now(cx)),
        )
        .into_any_element()
}

fn lan_status_text(cx: &App, status: &LanStatus) -> SharedString {
    match status {
        LanStatus::Idle => tr(cx, "sync.lan_idle"),
        LanStatus::Syncing => tr(cx, "sync.running"),
        LanStatus::Done(report) => {
            if !report.touched_anything() {
                return tr(cx, "sync.lan_none");
            }
            let books = (report.pulled + report.failed).to_string();
            let positions = report.applied_positions.to_string();
            let annotations = report.applied_annotations.to_string();
            let bookmarks = report.applied_bookmarks.to_string();
            tf(
                cx,
                "sync.lan_done",
                &[
                    ("books", &books),
                    ("positions", &positions),
                    ("annotations", &annotations),
                    ("bookmarks", &bookmarks),
                ],
            )
            .into()
        }
        LanStatus::Failed(failure) => tr(cx, lan_failure_key(*failure)),
    }
}

/// 局域网那一趟的失败措辞。**不复用 WebDAV 那几句**：那几句里的主角是「服务器」与
/// 「用户名/密码」，而这一条路上没有服务器、也没有用户名 —— 照着念会把读者支去翻路由器，
/// 真正该看的是对面那台的屏幕。`Store` 那一档两边说的是同一件事，所以仍然共用。
fn lan_failure_key(failure: Failure) -> &'static str {
    match failure {
        Failure::Config => "sync.lan_fail_config",
        Failure::Auth => "sync.lan_fail_auth",
        Failure::Network => "sync.lan_fail_network",
        Failure::Remote => "sync.lan_fail_remote",
        Failure::Store => "sync.fail_store",
    }
}

/// 拉过来的书单独说一句：屏幕上那句「同步了 N 条」不该把「补了两本书」这件事吃掉。
fn lan_status_detail(cx: &App, status: &LanStatus) -> Option<SharedString> {
    let LanStatus::Done(report) = status else {
        return None;
    };
    if report.pulled == 0 && report.failed == 0 {
        return None;
    }
    let pulled = report.pulled.to_string();
    if report.failed > 0 {
        let failed = report.failed.to_string();
        return Some(
            tf(
                cx,
                "sync.lan_books_failed",
                &[("pulled", &pulled), ("failed", &failed)],
            )
            .into(),
        );
    }
    Some(tf(cx, "sync.lan_books", &[("pulled", &pulled)]).into())
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
    //! 局域网那一组里两行是 `Row::custom`：它们的内容由闭包现渲染，所以只有**真开一次窗**
    //! 才走得到（`spec_pages` 那一层只把闭包装起来，不叫它）。这一条钉的是「装得出来」与
    //! 「那颗生成钮在有密钥之后真的不在了」，不是像素。

    use folio_core::Config;
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AnyWindowHandle, AppContext as _, TestAppContext, px, size};

    use crate::ui::features::settings::view::SettingsPage;
    use crate::ui::features::settings::view::desktop::SettingsView;
    use crate::ui::services::config::ConfigHolder;

    const KEY: &str = "0123456789abcdef0123456789abcdef";

    /// 渲染一帧，问那一颗钮在不在树里。
    fn present(handle: AnyWindowHandle, id: &'static str, cx: &mut TestAppContext) -> bool {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.try_find(id).is_some()
        })
        .expect("窗口应能渲染")
    }

    #[gpui_kit::test]
    async fn the_local_rows_render_and_the_generate_button_retires_itself(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let handle = cx
            // 窗口给到 1400 高：这一页有三组共 11 行，700 高的话最后一组落在折叠线以下、
            // 虚拟列表根本不摆它出来 —— 那时 `try_find` 说「没有」，而那句不是关于代码的。
            .open_window(size(px(900.), px(1400.)), |window, cx| {
                ConfigHolder::install(Config::default(), cx);
                // WebDAV 那一路是会员功能：这条测的是行渲染，先把永久位摆上绕开门禁。
                ConfigHolder::edit_membership(cx, |membership| membership.lifetime = true);
                // 落点直接给「同步」那一页：那两行自定义才会进树。
                let view = cx.new(|cx| SettingsView::new(SettingsPage::Sync, cx));
                Root::new(view, window, cx)
            })
            .into();

        assert!(
            present(handle, "settings-sync-now", cx),
            "WebDAV 那一组原有的一颗钮不该因为多了一组就不在"
        );
        assert!(
            present(handle, "settings-lan-sync", cx),
            "局域网那一组的状态行与那颗「现在就同步」该画出来"
        );
        assert!(
            present(handle, "settings-lan-generate", cx),
            "这台还没有密钥：校验码那一格该摆出那颗生成钮，而不是空白"
        );

        // 有了密钥之后那颗钮要消失 —— 留着它就是递给读者一把能把自己从账号里摘出去的钮。
        cx.update(|cx| {
            ConfigHolder::edit_account(cx, |account| {
                account.enabled = true;
                account.key = KEY.into();
                account.peer = "192.168.1.5:8410".into();
            });
        });
        assert!(
            !present(handle, "settings-lan-generate", cx),
            "已经有密钥了，那颗生成钮不该还在"
        );
        assert!(present(handle, "settings-lan-sync", cx), "同步那颗钮一直在");
    }
}
