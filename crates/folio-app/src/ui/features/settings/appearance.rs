//! 外观：浅深怎么选（跟随系统 / 钉死浅 / 钉死深），以及解析到浅、深时各用哪套主题。
//!
//! 主题**名**是这里的选项，主题**本身**不在这里定义：候选来自 UI 框架的主题集
//! 加上读者自己放进 `themes/` 的 JSON（见 `ui/shared/theme.rs`）。每一侧都记名字，
//! 即使当前模式只用到其中一侧 —— 之后换模式时读者的选择还在。

use super::*;

pub(super) fn appearance_page(cx: &App) -> SettingPage {
    let mode_options: Vec<(SharedString, SharedString)> = [
        (Mode::System, tr(cx, "appearance.follow_system")),
        (Mode::Light, tr(cx, "appearance.light")),
        (Mode::Dark, tr(cx, "appearance.dark")),
    ]
    .into_iter()
    .map(|(mode, label)| (SharedString::from(mode.as_str()), label))
    .collect();

    page(
        tr(cx, "page.appearance"),
        IconName::Palette,
        vec![
            SettingItem::new(
                tr(cx, "field.appearance_mode"),
                SettingField::dropdown(
                    mode_options,
                    |cx: &App| SharedString::from(ConfigHolder::appearance(cx).mode.as_str()),
                    |val: SharedString, cx| {
                        ConfigHolder::edit_appearance(cx, |a| a.mode = Mode::parse(&val))
                    },
                ),
            )
            .description(tr(cx, "field.appearance_mode.desc"))
            .keywords([
                "appearance",
                "mode",
                "dark",
                "light",
                "外观",
                "浅深",
                "深色",
            ]),
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

/// 一侧（浅或深）的主题选择。选项按该侧过滤，所以浅色列不出深色主题。
fn theme_item(cx: &App, mode: ThemeMode, title: &str, hint: &str) -> SettingItem {
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

    SettingItem::new(
        tr(cx, title),
        SettingField::scrollable_dropdown(
            options,
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
        ),
    )
    .description(tr(cx, hint))
    .keywords(["theme", "color", "主题", "配色"])
}
