//! 外观：浅深怎么选（跟随系统 / 钉死浅 / 钉死深），阅读页另有一档，解析到浅、深时各用
//! 哪套主题，以及往哪儿放读者自己的配色。
//!
//! 主题**名**是这里的选项，主题**本身**不在这里定义：候选来自 UI 框架的主题集
//! 加上读者自己放进 `themes/` 的 JSON（见 `ui/shared/theme.rs`）。每一侧都记名字，
//! 即使当前模式只用到其中一侧 —— 之后换模式时读者的选择还在。

use gpui_kit::component::h_flex;
use gpui_kit::{IntoElement as _, ParentElement, Styled, div};

use crate::ui::components::muted_label;

use super::super::*;

pub(in crate::ui::features::settings) fn appearance_page(cx: &App) -> Page {
    page(
        tr(cx, "page.appearance"),
        IconName::Palette,
        vec![mode_group(cx), themes_group(cx), custom_group(cx)],
    )
}

/// 浅深由谁定：软件整体一档，阅读页另有一档。
///
/// 阅读页那一格是**默认档** —— 某一本书在自己那一行上设过之后，那一本盖住这里
/// （见 `folio_core::BookSettings`）；没单独设过的书跟着这一档走。
fn mode_group(cx: &App) -> Group {
    let options: Vec<(SharedString, SharedString)> = [
        (Mode::System, tr(cx, "appearance.follow_system")),
        (Mode::Light, tr(cx, "appearance.light")),
        (Mode::Dark, tr(cx, "appearance.dark")),
    ]
    .into_iter()
    .map(|(mode, label)| (SharedString::from(mode.as_str()), label))
    .collect();
    // 阅读页那一档多一个「跟随软件」：`reading_mode` 是 `Option<Mode>`，而选项值只有
    // 字符串一种形状，`None` 需要一个自己的写法（`Mode::reading_value` 与
    // `Mode::parse_reading` 是这一对唯一的翻译处）。
    let reading_options: Vec<(SharedString, SharedString)> = [
        (None, tr(cx, "appearance.follow_app")),
        (
            Some(Mode::System),
            tr(cx, "appearance.follow_system"),
        ),
        (Some(Mode::Light), tr(cx, "appearance.light")),
        (Some(Mode::Dark), tr(cx, "appearance.dark")),
    ]
    .into_iter()
    .map(|(mode, label)| (SharedString::from(Mode::reading_value(mode)), label))
    .collect();

    group(
        tr(cx, "field.appearance_mode"),
        vec![
            Row::dropdown(
                tr(cx, "field.appearance_mode"),
                options,
                false,
                |cx: &App| SharedString::from(ConfigHolder::default_appearance(cx).mode.as_str()),
                |val: SharedString, cx| {
                    ConfigHolder::edit_appearance(cx, |a| a.mode = Mode::parse(&val))
                },
            )
            .description(tr(cx, "field.appearance_mode.desc"))
            .keywords(&[
                "appearance",
                "mode",
                "dark",
                "light",
                "外观",
                "浅深",
                "深色",
            ]),
            Row::dropdown(
                tr(cx, "field.reading_mode"),
                reading_options,
                false,
                |cx: &App| {
                    SharedString::from(Mode::reading_value(
                        ConfigHolder::default_appearance(cx).reading_mode,
                    ))
                },
                |val: SharedString, cx| {
                    ConfigHolder::edit_appearance(cx, |a| {
                        a.reading_mode = Mode::parse_reading(&val)
                    })
                },
            )
            .description(tr(cx, "field.reading_mode.desc"))
            .keywords(&[
                "reading",
                "night",
                "阅读",
                "阅读浅深",
                "夜里",
                "深色",
            ]),
        ],
    )
}

/// 两侧各用哪一套：选项按该侧过滤，所以浅色列不出深色主题。
fn themes_group(cx: &App) -> Group {
    group(
        tr(cx, "group.themes"),
        vec![
            theme_item(
                cx,
                ThemeMode::Light,
                "field.theme_light",
                "field.theme_light.desc",
            ),
            theme_item(
                cx,
                ThemeMode::Dark,
                "field.theme_dark",
                "field.theme_dark.desc",
            ),
        ],
    )
}

/// 一侧（浅或深）的主题选择。
fn theme_item(cx: &App, mode: ThemeMode, title: &str, hint: &str) -> Row {
    let dark = mode.is_dark();
    let options: Vec<(SharedString, SharedString)> = theme::theme_names(cx, mode)
        .into_iter()
        .map(|name| (name.clone(), name))
        .collect();
    let fallback: &'static str = if dark {
        theme::DEFAULT_DARK
    } else {
        theme::DEFAULT_LIGHT
    };

    Row::dropdown(
        tr(cx, title),
        options,
        true,
        move |cx: &App| {
            let appearance = ConfigHolder::default_appearance(cx);
            // 没选过（或选的那套已被删掉）就显示框架默认那套的名字，
            // 而不是留一个空白选项。
            let stored = if dark {
                appearance.dark
            } else {
                appearance.light
            };
            let known = theme::theme_names(cx, mode)
                .into_iter()
                .any(|name| Some(name.as_ref()) == stored.as_deref());
            let shown = stored
                .filter(|_| known)
                .unwrap_or_else(|| fallback.to_string());
            SharedString::from(shown)
        },
        move |val: SharedString, cx| {
            ConfigHolder::edit_appearance(cx, |appearance| {
                let slot = if dark {
                    &mut appearance.dark
                } else {
                    &mut appearance.light
                };
                *slot = Some(val.to_string());
            });
        },
    )
    .description(tr(cx, hint))
    .keywords(&["theme", "color", "主题", "配色"])
}

/// 读者自己的配色往哪个目录放，加一颗「打开所在文件夹」—— 与配置文件那一格同一个动作
/// （共用 [`super::super::open_folder_button`]）。
///
/// 点了它**不会**当场重扫：那一份是启动时读的（`theme::register_user_themes`），所以说明
/// 写的仍是「下次启动」。按钮只负责把人送到那个目录跟前，放 JSON 进去还是要读者自己做。
fn custom_group(cx: &App) -> Group {
    let dir = folio_core::infra::paths::themes_dir();
    group(
        tr(cx, "group.custom_themes"),
        vec![
            Row::custom_titled(tr(cx, "field.theme_dir"), move |cx| {
                let dir = dir.clone();
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(muted_label(dir.display().to_string(), cx)),
                    )
                    .child(super::super::open_folder_button(
                        "appearance-open-themes-dir",
                        dir,
                        cx,
                    ))
                    .into_any_element()
            })
            .description(tr(cx, "field.theme_dir.desc"))
            .keywords(&["theme", "folder", "目录", "配色", "打开"]),
        ],
    )
}
