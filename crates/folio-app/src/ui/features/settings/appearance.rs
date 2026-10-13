//! 外观：浅深怎么选（跟随系统 / 钉死浅 / 钉死深），解析到浅、深时各用哪套主题，
//! 以及往哪儿放读者自己的配色。
//!
//! 主题**名**是这里的选项，主题**本身**不在这里定义：候选来自 UI 框架的主题集
//! 加上读者自己放进 `themes/` 的 JSON（见 `ui/shared/theme.rs`）。每一侧都记名字，
//! 即使当前模式只用到其中一侧 —— 之后换模式时读者的选择还在。

use gpui_kit::{IntoElement as _, ParentElement, Styled, div};

use super::*;

pub(super) fn appearance_page(cx: &App) -> Page {
    page(
        tr(cx, "page.appearance"),
        IconName::Palette,
        vec![mode_group(cx), themes_group(cx), custom_group(cx)],
    )
}

/// 第一组只管一件事：浅深由谁定。
fn mode_group(cx: &App) -> Group {
    let options: Vec<(SharedString, SharedString)> = [
        (Mode::System, tr(cx, "appearance.follow_system")),
        (Mode::Light, tr(cx, "appearance.light")),
        (Mode::Dark, tr(cx, "appearance.dark")),
    ]
    .into_iter()
    .map(|(mode, label)| (SharedString::from(mode.as_str()), label))
    .collect();

    group(
        tr(cx, "field.appearance_mode"),
        vec![
            Row::dropdown(
                tr(cx, "field.appearance_mode"),
                options,
                false,
                |cx: &App| SharedString::from(ConfigHolder::appearance(cx).mode.as_str()),
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
            let appearance = ConfigHolder::appearance(cx);
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

/// 读者自己的配色往哪个目录放。
///
/// 只报路径、不给「打开这个目录」那颗按钮：本应用没有调起文件管理器的平台代码，
/// 摆一颗点了没反应的按钮比少一颗更糟。也不会当场重扫 —— 那一份是启动时读的
/// （`theme::register_user_themes`），所以说明写的是「下次启动」。
fn custom_group(cx: &App) -> Group {
    let dir = folio_core::infra::paths::themes_dir().display().to_string();
    group(
        tr(cx, "group.custom_themes"),
        vec![
            Row::custom_titled(tr(cx, "field.theme_dir"), move |cx| {
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(dir.clone())
                    .into_any_element()
            })
            .description(tr(cx, "field.theme_dir.desc"))
            .keywords(&["theme", "folder", "目录", "配色"]),
        ],
    )
}
