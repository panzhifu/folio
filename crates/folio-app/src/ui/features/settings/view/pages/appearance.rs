//! 外观：界面语言、浅深怎么选（跟随系统 / 钉死浅 / 钉死深），阅读页另有一档，
//! 解析到浅、深时各用哪套主题，以及往哪儿放读者自己的配色。
//!
//! 主题**不靠下拉框挑**：这一页把每一套都画成一张卡，浅深两档各一组。卡片里那一小块
//! 预览用的是**那套主题自己的颜色**（从 `ThemeConfig` 解析出来，不动全局 `Theme`），
//! 所以读者不用先选中才知道它长什么样。桌面一格三列、手机上两列；两边卡片装的东西
//! 不一样 —— 桌面多一行浅深标签与一块 `Aa` 字样，手机只留名字，把高度让给可点区域。
//!
//! 界面语言从「关于」搬到这一页（2026-11-05）：它是**要改的东西**，不是「关于这个应用」
//! 的一条事实；放在外观这一屏的第一组，读者换语言不用先翻到末尾那一页。
//!
//! 主题**名**是这里的选项，主题**本身**不在这里定义：候选来自 UI 框架的主题集
//! 加上读者自己放进 `themes/` 的 JSON（见 `ui/foundation/theme.rs`）。每一侧都记名字，
//! 即使当前模式只用到其中一侧 —— 之后换模式时读者的选择还在。

use std::rc::Rc;

use folio_core::infra::i18n::Locale;
use gpui_kit::component::h_flex;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, Theme, ThemeColor, ThemeConfig, ThemeRegistry, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, FontWeight, Hsla, InteractiveElement as _, IntoElement as _, ParentElement,
    StatefulInteractiveElement as _, Styled, TestSupportExt as _, div, px,
};

use crate::ui::components::muted_label;

use super::super::*;

pub(in crate::ui::features::settings) fn appearance_page(cx: &App) -> Page {
    page(
        tr(cx, "page.appearance"),
        IconName::Palette,
        vec![
            language_group(cx),
            mode_group(cx),
            themes_group(cx),
            custom_group(cx),
        ],
    )
}

/// 「界面语言」这一格的搜索词：通用几个，加上**每一门在选的语言**的本族名与标签。
///
/// 这一份是 [`Locale::ALL`] 的第二份副本，会有漂移风险，所以 `tests` 里有一条测试
/// 逐个核对 `code()` 与 `endonym()` 都在列 —— 加一门语言忘了补词，搜索框就搜不到这一格。
const LANGUAGE_KEYWORDS: &[&str] = &[
    "language",
    "locale",
    "语言",
    "中文",
    "简体",
    "繁体",
    "简体中文",
    "繁體中文",
    "English",
    "日本語",
    "한국어",
    "Français",
    "Deutsch",
    "Español",
    "Русский",
    "zh-CN",
    "zh-TW",
    "en",
    "ja",
    "ko",
    "fr",
    "de",
    "es",
    "ru",
];

/// 界面语言。选项用**该语言自己的写法**标注：用户切换语言往往正是在看不懂
/// 当前界面的时候，把 "English" 翻成「英语」反而认不出来。
fn language_group(cx: &App) -> Group {
    group(
        tr(cx, "field.language"),
        vec![
            Row::dropdown(
                tr(cx, "field.language"),
                language_options(),
                false,
                |cx: &App| SharedString::from(ConfigHolder::language(cx).locale.code()),
                |val: SharedString, cx| {
                    // 认不出的值不改：下拉框给不出认不出的值，
                    // 真出现时保留当前语言比切成未知语言好。
                    if let Some(locale) = Locale::from_label(&val) {
                        ConfigHolder::edit_language(cx, |l| l.locale = locale);
                    }
                },
            )
            .description(tr(cx, "field.language.desc"))
            .keywords(LANGUAGE_KEYWORDS),
        ],
    )
}

/// 语言下拉框的选项：`(值, 标签)`，值是 BCP-47 码（配置里存的就是它），
/// 标签是语言自己的名字。
///
/// **值必须在第一位**：下拉框的约定是元组首项作为选中值（与取值闭包给出的
/// `locale.code()` 对得上）与写回 setter 的实参，次项才是按钮和菜单里显示的文字。
/// 写反了会出现两重故障：当前语言对不上任何一个选项值，按钮只能显示裸代码；
/// 点菜单项传给 setter 的是语言名，`from_label` 之外的路径认不出来，改动被静默
/// 丢弃 —— 读者点什么语言都不生效。
fn language_options() -> Vec<(SharedString, SharedString)> {
    Locale::ALL
        .iter()
        .map(|locale| {
            (
                SharedString::from(locale.code()),
                SharedString::from(locale.endonym()),
            )
        })
        .collect()
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
        (Mode::Ink, tr(cx, "appearance.ink_wash")),
    ]
    .into_iter()
    .map(|(mode, label)| (SharedString::from(mode.as_str()), label))
    .collect();
    // 阅读页那一档多一个「跟随软件」：`reading_mode` 是 `Option<Mode>`，而选项值只有
    // 字符串一种形状，`None` 需要一个自己的写法（`Mode::reading_value` 与
    // `Mode::parse_reading` 是这一对唯一的翻译处）。
    let reading_options: Vec<(SharedString, SharedString)> = [
        (None, tr(cx, "appearance.follow_app")),
        (Some(Mode::System), tr(cx, "appearance.follow_system")),
        (Some(Mode::Light), tr(cx, "appearance.light")),
        (Some(Mode::Dark), tr(cx, "appearance.dark")),
        (Some(Mode::Ink), tr(cx, "appearance.ink_wash")),
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
                "ink",
                "外观",
                "浅深",
                "深色",
                "水墨",
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
            .keywords(&["reading", "night", "阅读", "阅读浅深", "夜里", "深色"]),
        ],
    )
}

/// 两侧各用哪一套。每一套画成一张卡，浅深各一组 —— 读者要先看见颜色再决定，
/// 下拉框只给名字，名字里读不出「这本主题长什么样」。
fn themes_group(cx: &App) -> Group {
    group(
        tr(cx, "group.themes"),
        vec![
            theme_cards_row(cx, ThemeMode::Light),
            theme_cards_row(cx, ThemeMode::Dark),
        ],
    )
}

/// 一侧（浅或深）的主题网格。标题与说明照旧用 `field.theme_*` 那两条文案：
/// 卡片只说「哪一套」，这些话说的是「这一侧什么时候生效」。
fn theme_cards_row(cx: &App, mode: ThemeMode) -> Row {
    let (title, hint) = if mode.is_dark() {
        ("field.theme_dark", "field.theme_dark.desc")
    } else {
        ("field.theme_light", "field.theme_light.desc")
    };
    Row::custom_block(tr(cx, title), move |cx| theme_grid(cx, mode))
        .description(tr(cx, hint))
        .keywords(&["theme", "color", "主题", "配色", "外观"])
}

/// 一套主题在卡片里要用的四种颜色。只留 `Copy` 的小东西，`ThemeConfig` 那一大坨
/// 不跟着元素走。
#[derive(Clone, Copy, Debug)]
struct Swatch {
    background: Hsla,
    primary: Hsla,
    primary_foreground: Hsla,
    foreground: Hsla,
}

/// 把一个主题配置解析成具体颜色，**不动全局 `Theme`**：照框架主题选择器自己的做法，
/// 从浅 / 深的基础色起一份临时主题，`apply_config` 之后取它算出来的 `colors`。
fn swatch_of(config: &Rc<ThemeConfig>) -> Swatch {
    let base = if config.mode.is_dark() {
        ThemeColor::dark()
    } else {
        ThemeColor::light()
    };
    let mut theme = Theme::from(base.as_ref());
    theme.apply_config(config);
    Swatch {
        background: theme.colors.background,
        primary: theme.colors.primary,
        primary_foreground: theme.colors.primary_foreground,
        foreground: theme.colors.foreground,
    }
}

/// 这一侧现在算哪一套。存的名字可能已经被删掉：与下拉框时代同一条回落，
/// 显示框架默认那套，而不是让所有卡都没选中。
fn selected_name(cx: &App, mode: ThemeMode) -> SharedString {
    let appearance = ConfigHolder::default_appearance(cx);
    let stored = if mode.is_dark() {
        appearance.dark
    } else {
        appearance.light
    };
    let known = theme::theme_names(cx, mode);
    stored
        .filter(|name| {
            known
                .iter()
                .any(|candidate| candidate.as_ref() == name.as_str())
        })
        .map(SharedString::from)
        .unwrap_or_else(|| SharedString::from(theme::default_name(mode)))
}

/// 一套主题的网格。桌面三列、手机两列 —— 手机那块 393dp 的宽度放三列会把名字挤成
/// 省略号，而名字正是让人分辨两套相近配色的最后一格。
fn theme_grid(cx: &App, mode: ThemeMode) -> AnyElement {
    let themes: Vec<(SharedString, Swatch)> = {
        let registry = ThemeRegistry::global(cx);
        registry
            .sorted_themes()
            .into_iter()
            .filter(|config| config.mode == mode)
            .map(|config| (config.name.clone(), swatch_of(config)))
            .collect()
    };
    if themes.is_empty() {
        return muted_label(tr(cx, "appearance.no_themes"), cx).into_any_element();
    }
    let selected = selected_name(cx, mode);
    let desktop = crate::ui::host::supports_overlay_controls(cx);
    let mut grid = div().w_full().grid().gap_2();
    grid = if desktop {
        grid.grid_cols(3)
    } else {
        grid.grid_cols(2)
    };
    for (name, swatch) in themes {
        let is_selected = name == selected;
        grid = grid.child(theme_card(cx, name, mode, swatch, is_selected, desktop));
    }
    grid.into_any_element()
}

/// 一张主题卡。两端的差别只在**卡片里装什么**：
///
/// - 桌面：预览里多一块 `Aa` 字样（看得出这套主题的字色与底色关系），名字右边挂
///   「浅色 / 深色」标签，方便在一屏混排时认；
/// - 手机：预览只留三条色带，名字一行，浅深由上方那一组标题说 —— 同一句话在 172 宽的
///   卡里再写一遍，只会把名字挤掉。
///
/// 两侧共同的是：选中态同时有**主色描边**与**右上角勾**，不只靠颜色传义。
fn theme_card(
    cx: &App,
    name: SharedString,
    mode: ThemeMode,
    swatch: Swatch,
    selected: bool,
    desktop: bool,
) -> AnyElement {
    let theme = cx.theme();
    // 选中圈用**当前界面**的主色，不用被预览那套的主色：深色主题的主色往往是浅色，
    // 压在浅色界面的卡上等于没有描边。
    let border = if selected {
        theme.primary
    } else {
        theme.border
    };
    let hover = theme.muted_foreground;
    let ink = theme.foreground;
    let muted = theme.muted_foreground;
    let mode_label = if mode.is_dark() {
        tr(cx, "appearance.dark")
    } else {
        tr(cx, "appearance.light")
    };
    let id = format!("theme-card-{name}");
    let aria = format!("{name} · {mode_label}");
    let click_name = name.clone();

    let preview = if desktop {
        desktop_preview(swatch, selected)
    } else {
        phone_preview(swatch, selected)
    };

    let mut card = v_flex()
        .id(id)
        .test_support()
        .aria_label(aria)
        .aria_selected(selected)
        .w_full()
        .gap_1()
        .p_2()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(border)
        .bg(theme.popover)
        .cursor_pointer()
        .hover(move |style| {
            if selected {
                style
            } else {
                style.border_color(hover)
            }
        })
        .on_click(move |_, _, cx| choose_theme(cx, mode, click_name.clone()))
        .child(preview);

    if desktop {
        card = card.child(
            h_flex()
                .w_full()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .text_sm()
                        .text_ellipsis()
                        .text_color(ink)
                        .child(name),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(muted)
                        .child(mode_label),
                ),
        );
    } else {
        card = card.child(
            div()
                .min_w_0()
                .w_full()
                .text_sm()
                .text_ellipsis()
                .text_color(ink)
                .child(name),
        );
    }
    card.into_any_element()
}

/// 桌面的预览：一块 `Aa` 加上两条色带，够看出字色、主色与底色的关系。
fn desktop_preview(swatch: Swatch, selected: bool) -> AnyElement {
    div()
        .relative()
        .w_full()
        .h(px(56.))
        .overflow_hidden()
        .rounded(px(6.))
        .bg(swatch.background)
        .p(px(8.))
        .child(
            v_flex()
                .gap(px(4.))
                .child(
                    div()
                        .text_base()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(swatch.foreground)
                        .child("Aa"),
                )
                .child(
                    h_flex()
                        .gap(px(4.))
                        .child(bar(swatch.primary, 30.))
                        .child(bar(swatch.foreground.opacity(0.30), 46.)),
                )
                .child(bar(swatch.foreground.opacity(0.18), 34.)),
        )
        .when(selected, |this| this.child(check_badge(swatch, 16.)))
        .into_any_element()
}

/// 手机的预览：只有三条色带。卡片窄，`Aa` 会跟名字抢同一条视线。
fn phone_preview(swatch: Swatch, selected: bool) -> AnyElement {
    div()
        .relative()
        .w_full()
        .h(px(36.))
        .overflow_hidden()
        .rounded(px(4.))
        .bg(swatch.background)
        .p(px(6.))
        .child(
            v_flex()
                .gap(px(3.))
                .child(bar(swatch.primary, 40.))
                .child(bar(swatch.foreground.opacity(0.30), 58.))
                .child(bar(swatch.foreground.opacity(0.18), 32.)),
        )
        .when(selected, |this| this.child(check_badge(swatch, 14.)))
        .into_any_element()
}

fn bar(color: Hsla, width: f32) -> impl IntoElement {
    div()
        .w(px(width))
        .h(px(4.))
        .flex_shrink_0()
        .rounded_full()
        .bg(color)
}

/// 选中态右上角那一枚勾：描边之外的第二条通路，且不依赖当前主题的色值。
fn check_badge(swatch: Swatch, size: f32) -> AnyElement {
    div()
        .absolute()
        .top(px(4.))
        .right(px(4.))
        .size(px(size))
        .rounded_full()
        .bg(swatch.primary)
        .flex()
        .items_center()
        .justify_center()
        .child(
            Icon::new(IconName::Check)
                .with_size(px(size * 0.6))
                .text_color(swatch.primary_foreground),
        )
        .into_any_element()
}

/// 点一张卡：先写对应那一侧的名字。只有读者已经把浅深钉死在某一侧时，才顺手切到
/// 卡片那侧 —— 「跟随系统」是读者自己定的，不该被一次挑主题悄悄改掉。
fn choose_theme(cx: &mut App, mode: ThemeMode, name: SharedString) {
    ConfigHolder::edit_appearance(cx, |appearance| {
        apply_choice(appearance, mode, name.to_string());
    });
}

/// [`choose_theme`] 的纯函数部分，测试只钉这一格。
fn apply_choice(appearance: &mut Appearance, mode: ThemeMode, name: String) {
    let slot = if mode.is_dark() {
        &mut appearance.dark
    } else {
        &mut appearance.light
    };
    *slot = Some(name);
    if appearance.mode != Mode::System {
        appearance.mode = if mode.is_dark() {
            Mode::Dark
        } else {
            Mode::Light
        };
    }
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

#[cfg(test)]
mod tests {
    use super::{LANGUAGE_KEYWORDS, apply_choice, language_options, swatch_of};
    use folio_core::infra::config::{Appearance, Mode};
    use folio_core::infra::i18n::Locale;
    use gpui_kit::TestAppContext;
    use gpui_kit::component::{ThemeMode, ThemeRegistry};

    /// 每一套主题的卡都要用它自己的颜色：浅色主题的底色比深色主题亮。
    /// 这一条同时钉住「不碰全局 Theme」那半边 —— 只解析一份临时 `Theme`。
    #[gpui_kit::test]
    fn a_card_previews_the_theme_itself_not_the_active_one(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(crate::ui::foundation::theme::register_builtin_themes);
        cx.update(|cx| {
            let registry = ThemeRegistry::global(cx);
            let light = registry
                .themes()
                .get(crate::ui::foundation::theme::DEFAULT_LIGHT)
                .expect("框架默认浅色主题要在 registry 里")
                .clone();
            let dark = registry
                .themes()
                .get(crate::ui::foundation::theme::DEFAULT_DARK)
                .expect("框架默认深色主题要在 registry 里")
                .clone();
            let light = swatch_of(&light);
            let dark = swatch_of(&dark);
            assert!(
                light.background.l > dark.background.l,
                "浅色主题的预览底色该比深色亮：{light:?} / {dark:?}"
            );
        });
    }

    /// 点一张卡只写它自己那一侧。浅深钉死时顺手切过去；跟随系统时**不**替读者改主意。
    #[test]
    fn a_card_fills_its_side_and_only_pins_an_explicit_mode() {
        let mut appearance = Appearance {
            mode: Mode::System,
            ..Appearance::default()
        };
        apply_choice(&mut appearance, ThemeMode::Dark, "Theme Dark".into());
        assert_eq!(appearance.dark.as_deref(), Some("Theme Dark"));
        assert_eq!(appearance.light, None);
        assert_eq!(appearance.mode, Mode::System);

        appearance.mode = Mode::Light;
        apply_choice(&mut appearance, ThemeMode::Dark, "Theme Dark 2".into());
        assert_eq!(appearance.mode, Mode::Dark, "钉死浅色时点深色卡该切过去");
        assert_eq!(appearance.dark.as_deref(), Some("Theme Dark 2"));

        apply_choice(&mut appearance, ThemeMode::Light, "Theme Light".into());
        assert_eq!(appearance.mode, Mode::Light);
        assert_eq!(appearance.light.as_deref(), Some("Theme Light"));
    }

    /// 每个选项的值都要能被写回路径（`Locale::from_label`）认出来，
    /// 且认出来的就是标签写的那个语言。
    #[test]
    fn every_language_option_round_trips_through_its_own_value() {
        for (value, label) in language_options() {
            let locale = Locale::from_label(&value)
                .unwrap_or_else(|| panic!("选项值 {value:?} 必须被写回路径认出来"));
            assert_eq!(
                label,
                locale.endonym(),
                "{value:?} 的标签应是这个语言自己的名字"
            );
        }
    }

    /// 取值闭包给出的是 `locale.code()`，它必须对得上某个选项值。
    #[test]
    fn the_current_locale_is_always_representable_as_an_option_value() {
        let values: Vec<_> = language_options().into_iter().map(|(v, _)| v).collect();
        for locale in Locale::ALL {
            assert!(
                values.iter().any(|value| *value == locale.code()),
                "{} 应该作为选项值出现",
                locale.code()
            );
        }
    }

    /// 每一门在选的语言都要能被搜索词命中：`LANGUAGE_KEYWORDS` 是 `Locale::ALL`
    /// 的第二份副本，加一门语言时最容易漏的就是这里。
    #[test]
    fn every_language_on_offer_is_findable_in_the_search_box() {
        for locale in Locale::ALL {
            assert!(
                LANGUAGE_KEYWORDS.contains(&locale.code()),
                "{} 的标签没进搜索词",
                locale.code()
            );
            assert!(
                LANGUAGE_KEYWORDS.contains(&locale.endonym()),
                "{} 的本族名没进搜索词",
                locale.code()
            );
        }
    }
}
