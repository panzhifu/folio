//! 阅读设置：分成「文字 / 版面 / 字体 / 外观 / 语言 / 账号 / 关于」多页，交给组件的
//! `Settings` 容器渲染（它自带可调宽的页面导航与搜索框）。
//!
//! 这里只负责**描述**每一页：标题、说明、控件、取值与赋值。放到哪儿是宿主的事 ——
//! 它是一个整页视图，不是对话框，理由见 `app.rs` 里的 `settings_page`。
//!
//! 字段一律走 `SettingField` 的 (取值, 赋值) 对，两端都通过 `ConfigHolder`
//! 触碰同一个实体，所以改完立刻反映到已打开的书上并落盘。

use folio_core::Mode;
use folio_core::i18n::Locale;
use folio_core::paths::config_path;
use gpui_kit::assets::IconName;
use gpui_kit::component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage,
};
use gpui_kit::component::{ActiveTheme, ThemeMode};
use gpui_kit::{App, ParentElement, SharedString, Styled, div};

use crate::account::{account_avatar, display_name};
use crate::config_model::ConfigHolder;
use crate::l10n::tr;

/// 各页在 `Settings` 里的固定下标。顺序必须与 [`pages`] 一致 ——
/// 工具条的「设置」和侧栏的「账号」都按这里的常量定位到某一页。
pub const PAGE_TYPOGRAPHY: usize = 0;
pub const PAGE_LAYOUT: usize = 1;
pub const PAGE_FONTS: usize = 2;
pub const PAGE_APPEARANCE: usize = 3;
pub const PAGE_LANGUAGE: usize = 4;
pub const PAGE_ACCOUNT: usize = 5;
pub const PAGE_ABOUT: usize = 6;

/// 按固定顺序组装全部页面。
pub fn pages(cx: &App) -> Vec<SettingPage> {
    vec![
        typography_page(cx),
        layout_page(cx),
        fonts_page(cx),
        appearance_page(cx),
        language_page(cx),
        account_page(cx),
        about_page(cx),
    ]
}

/// 一页 = 标题 + 图标 + 一组（不带组标题，页面标题已经说明了一切）。
fn page(title: impl Into<SharedString>, icon: IconName, items: Vec<SettingItem>) -> SettingPage {
    SettingPage::new(title)
        .icon(icon)
        .default_open(true)
        .groups(vec![SettingGroup::new().items(items)])
}

#[allow(clippy::too_many_arguments)]
fn number(
    cx: &App,
    title: &str,
    hint: &str,
    keywords: &[&'static str],
    min: f64,
    max: f64,
    step: f64,
    get: fn(&App) -> f32,
    set: fn(f32, &mut App),
) -> SettingItem {
    SettingItem::new(
        tr(cx, title),
        SettingField::number_input(
            NumberFieldOptions { min, max, step },
            move |cx: &App| get(cx) as f64,
            move |val: f64, cx| set(val as f32, cx),
        ),
    )
    .description(tr(cx, hint))
    .keywords(keywords.iter().map(|s| (*s).to_string()))
}

pub fn typography_page(cx: &App) -> SettingPage {
    page(
        tr(cx, "page.typography"),
        IconName::Type,
        vec![
            number(
                cx,
                "field.font_size",
                "field.font_size.desc",
                &["font", "size", "字号"],
                9.0,
                72.0,
                1.0,
                |cx| ConfigHolder::reader(cx).font_size,
                |v, cx| ConfigHolder::edit_reader(cx, |p| p.font_size = v),
            ),
            number(
                cx,
                "field.line_height",
                "field.line_height.desc",
                &["line-height", "行距", "行高"],
                1.0,
                4.0,
                0.05,
                |cx| ConfigHolder::reader(cx).line_height,
                |v, cx| ConfigHolder::edit_reader(cx, |p| p.line_height = v),
            ),
            number(
                cx,
                "field.paragraph_gap",
                "field.paragraph_gap.desc",
                &["paragraph", "段距", "段间距"],
                0.0,
                4.0,
                0.05,
                |cx| ConfigHolder::reader(cx).paragraph_gap_em,
                |v, cx| ConfigHolder::edit_reader(cx, |p| p.paragraph_gap_em = v),
            ),
            number(
                cx,
                "field.indent",
                "field.indent.desc",
                &["indent", "缩进"],
                0.0,
                6.0,
                0.25,
                |cx| ConfigHolder::reader(cx).indent_em,
                |v, cx| ConfigHolder::edit_reader(cx, |p| p.indent_em = v),
            ),
            SettingItem::new(
                tr(cx, "field.justify"),
                SettingField::switch(
                    |cx: &App| ConfigHolder::reader(cx).justify,
                    |val: bool, cx| ConfigHolder::edit_reader(cx, |p| p.justify = val),
                ),
            )
            .description(tr(cx, "field.justify.desc"))
            .keywords(["justify", "align", "对齐", "两端"]),
        ],
    )
}

pub fn layout_page(cx: &App) -> SettingPage {
    page(
        tr(cx, "page.layout"),
        IconName::Columns3,
        vec![
            number(
                cx,
                "field.max_measure",
                "field.max_measure.desc",
                &["width", "measure", "行长", "版心", "铺满"],
                0.0,
                4000.0,
                20.0,
                |cx| ConfigHolder::reader(cx).max_measure,
                |v, cx| ConfigHolder::edit_reader(cx, |p| p.max_measure = v),
            ),
            number(
                cx,
                "field.margin_x",
                "field.margin_x.desc",
                &["margin", "页边距", "留白"],
                0.0,
                400.0,
                8.0,
                |cx| ConfigHolder::reader(cx).margin_x,
                |v, cx| ConfigHolder::edit_reader(cx, |p| p.margin_x = v),
            ),
        ],
    )
}

pub fn fonts_page(cx: &App) -> SettingPage {
    page(
        tr(cx, "page.fonts"),
        IconName::ALargeSmall,
        vec![
            SettingItem::new(
                tr(cx, "field.font_family"),
                SettingField::input(
                    |cx: &App| SharedString::from(ConfigHolder::reader(cx).font_family),
                    |val: SharedString, cx| {
                        if !val.trim().is_empty() {
                            ConfigHolder::edit_reader(cx, |p| p.font_family = val.to_string());
                        }
                    },
                ),
            )
            .description(tr(cx, "field.font_family.desc"))
            .keywords(["font", "family", "字体"]),
        ],
    )
}

/// 外观：浅深怎么选（跟随系统 / 钉死浅 / 钉死深），以及解析到浅、深时各用哪套主题。
///
/// 主题**名**是这里的选项，主题**本身**不在这里定义：候选来自 UI 框架的主题集
/// 加上读者自己放进 `themes/` 的 JSON（见 `crate::theme`）。每一侧都记名字，
/// 即使当前模式只用到其中一侧 —— 之后换模式时读者的选择还在。
pub fn appearance_page(cx: &App) -> SettingPage {
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
    let options: Vec<(SharedString, SharedString)> = crate::theme::theme_names(cx, mode)
        .into_iter()
        .map(|name| (name.clone(), name))
        .collect();
    let fallback: &'static str = if dark {
        crate::theme::DEFAULT_DARK
    } else {
        crate::theme::DEFAULT_LIGHT
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
                let known = crate::theme::theme_names(cx, mode)
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

/// 界面语言。选项用**该语言自己的写法**标注：用户切换语言往往正是在看不懂
/// 当前界面的时候，把 "English" 翻成「英语」反而认不出来。
pub fn language_page(cx: &App) -> SettingPage {
    let options: Vec<(SharedString, SharedString)> = Locale::ALL
        .iter()
        .map(|locale| {
            (
                SharedString::from(locale.endonym()),
                SharedString::from(locale.code()),
            )
        })
        .collect();
    page(
        tr(cx, "page.language"),
        IconName::Languages,
        vec![
            SettingItem::new(
                tr(cx, "field.language"),
                SettingField::dropdown(
                    options,
                    |cx: &App| SharedString::from(ConfigHolder::language(cx).locale.code()),
                    |val: SharedString, cx| {
                        // 认不出的值不改：下拉框给不出认不出的值，
                        // 真出现时保留当前语言比切成未知语言好。
                        if let Some(locale) = Locale::parse(&val) {
                            ConfigHolder::edit_language(cx, |l| l.locale = locale);
                        }
                    },
                ),
            )
            .description(tr(cx, "field.language.desc"))
            .keywords(["language", "locale", "语言"]),
        ],
    )
}

/// 账号：本地占位（一个可改的读者名），顶部是头像预览。
pub fn account_page(cx: &App) -> SettingPage {
    page(
        tr(cx, "page.account"),
        IconName::CircleUser,
        vec![
            SettingItem::render(move |_, _, cx| {
                let name = display_name(cx);
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(account_avatar(&name, 44.0, cx))
                    .child(div().text_lg().child(name))
            }),
            SettingItem::new(
                tr(cx, "field.account_name"),
                SettingField::input(
                    move |cx: &App| SharedString::from(ConfigHolder::account(cx).name),
                    |val: SharedString, cx| {
                        ConfigHolder::edit_account(cx, |a| a.name = val.trim().to_string());
                    },
                ),
            )
            .description(tr(cx, "field.account_name.desc"))
            .keywords(["account", "name", "账号", "读者"]),
        ],
    )
}

pub fn about_page(cx: &App) -> SettingPage {
    let muted = cx.theme().muted_foreground;
    page(
        tr(cx, "page.about"),
        IconName::Info,
        vec![
            SettingItem::new(
                tr(cx, "field.config_path"),
                SettingField::render(move |_, _, _cx| {
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child(config_path().display().to_string())
                }),
            )
            .description(tr(cx, "field.config_path.desc"))
            .keywords(["config", "path", "配置", "文件"]),
        ],
    )
}
