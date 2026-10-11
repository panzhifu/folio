//! 书库：导入的书**拷到哪儿**，以及在哪儿改这个位置。
//!
//! 这一页存在的理由是导入门禁的另一头：导入第一次会拦下来问位置（见
//! `desktop::app` 里的 `import`，移动端是书架上那块内联面板），问过一次就存进配置、
//! 之后不再拦 —— 所以必须有个地方能改它，否则读者答错一次就再也改不回来了。
//!
//! 填成空串等于「没确认过」：下一次导入会重新问一遍。清掉这一格和第一次用这个应用
//! 是同一个状态，不需要额外的开关。

use gpui_kit::component::button::Button;
use gpui_kit::{ParentElement, SharedString, Styled, div};

use gpui_kit::{App, FontWeight, IntoElement as _};

use super::*;
use crate::ui::shared::library::{choose_dir, pick_dir, suggested_dir, suggested_watch_dir};
use crate::ui::shared::transfer as wifi;
use crate::ui::widgets::muted_label;

pub(super) fn library_page(cx: &App) -> SettingPage {
    let dir = SettingItem::new(
        tr(cx, "field.library_dir"),
        SettingField::input(
            |cx: &App| SharedString::from(ConfigHolder::library(cx).dir),
            |val: SharedString, cx| {
                ConfigHolder::edit_library(cx, |library| library.dir = val.to_string());
            },
        ),
    )
    .description(tr(cx, "field.library_dir.desc"))
    .keywords(["library", "书库", "目录", "folder"]);

    // 用系统文件夹选择器挑任意目录。静态设置页每帧重建、拿不住 `Task`，所以 `detach()`
    // 让这次选择跑完（取消或选区结束都不需要回调界面）。
    let pick = SettingItem::render(move |_, _, cx| {
        Button::new("library-pick-dir")
            .label(tr(cx, "field.library_dir.pick").to_string())
            .on_click(|_, _, cx| {
                pick_dir(cx).detach();
            })
    })
    .keywords(["library", "folder", "选择", "pick"]);

    // 建议位置照实写出来：读者要的是知道不管填没填、默认会去哪儿，
    // 而不是自己去猜一个路径。
    let suggested = SettingItem::render(move |_, _, cx| {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(muted_label(tr(cx, "field.library_dir.suggested"), cx))
            .child(div().child(suggested_dir().display().to_string()))
    });

    // 文件夹监听：往一个「收件箱」目录里放书，书自己进库。开关打开时还没选过
    // 目录就落到建议位置，读者马上能看见去哪儿放书。
    let watch = SettingItem::new(
        tr(cx, "field.watch"),
        SettingField::switch(
            |cx: &App| ConfigHolder::library(cx).watch.is_some(),
            |on: bool, cx| {
                ConfigHolder::edit_library(cx, |library| {
                    library.watch = on.then(|| folio_core::config::WatchDir {
                        dir: suggested_watch_dir().display().to_string(),
                        ..Default::default()
                    });
                });
            },
        ),
    )
    .description(tr(cx, "field.watch.desc"))
    .keywords(["watch", "监听", "inbox", "收件箱", "自动"]);

    let watch_pick = SettingItem::render(move |_, _, cx| {
        let dir = ConfigHolder::library(cx)
            .watch
            .map(|w| w.dir)
            .unwrap_or_default();
        Button::new("library-pick-watch-dir")
            .label(
                if dir.is_empty() {
                    tr(cx, "field.watch.dir.pick")
                } else {
                    dir.into()
                }
                .to_string(),
            )
            .on_click(|_, _, cx| {
                choose_dir(cx, |cx, dir| {
                    let path = dir.display().to_string();
                    tracing::info!("watch: 监听目录选为 {path}");
                    ConfigHolder::edit_library(cx, |library| {
                        let watch = library.watch.get_or_insert_with(|| {
                            folio_core::config::WatchDir {
                                dir: String::new(),
                                ..Default::default()
                            }
                        });
                        watch.dir = path;
                    });
                })
                .detach();
            })
    })
    .keywords(["watch", "inbox", "收件箱", "目录"]);

    let watch_suggested = SettingItem::render(move |_, _, cx| {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(muted_label(tr(cx, "field.watch.suggested"), cx))
            .child(div().child(suggested_watch_dir().display().to_string()))
    });

    // Wi-Fi 传书：设备起个 HTTP 服务器，局域网里的浏览器打开网页直接上传。
    // 服务**不写进配置**：开关开了活到关掉或进程退出，重启不自动开 ——
    // 「我在局域网里裸着」这个状态不该瞒着读者自己恢复。
    let wifi_switch = SettingItem::new(
        tr(cx, "field.transfer"),
        SettingField::switch(
            |_cx: &App| wifi::running().is_some(),
            |on: bool, cx| wifi::set_enabled(on, cx),
        ),
    )
    .description(tr(cx, "field.transfer.desc"))
    .keywords(["wifi", "transfer", "传书", "上传", "网页"]);

    let wifi_status = SettingItem::render(move |_, _, cx| match wifi::running() {
        None => muted_label(tr(cx, "field.transfer.off"), cx).into_any_element(),
        Some(status) => {
            let mut col = div().flex().flex_col().gap_1();
            col = col.child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tf(cx, "field.transfer.pin", &[("pin", &status.pin)])),
            );
            if status.urls.is_empty() {
                col = col.child(muted_label(tr(cx, "field.transfer.no_url"), cx));
            } else {
                for url in &status.urls {
                    col = col.child(div().child(url.clone()));
                }
            }
            col.into_any_element()
        }
    })
    .keywords(["wifi", "pin", "配对码", "地址"]);

    page(
        tr(cx, "page.library"),
        IconName::FolderOpen,
        vec![
            group(tr(cx, "group.location"), vec![dir, pick, suggested]),
            group(tr(cx, "group.watch"), vec![watch, watch_pick, watch_suggested]),
            group(tr(cx, "group.transfer"), vec![wifi_switch, wifi_status]),
        ],
    )
}
