//! 外观：界面语言、浅深怎么选（跟随系统 / 钉死浅 / 钉死深），阅读页另有一档，
//! 解析到浅、深时各用哪套主题，以及往哪儿放读者自己的配色。
//!
//! 「界面浅深」与主题一样**不靠下拉框挑**：四档各一张预览缩略图卡，样张是一页
//! 竖版的书页（阅读应用的页本来就是竖的），选之前就能看见每一档长什么样；
//! 「跟随系统」那张劈成左浅右深两半。样张用固定色而不取当前主题 —— 这一格
//! 回答「取哪一侧」，下面的主题网格才回答「那一侧用哪套」，跟着换色两排会互相抢话。
//!
//! 卡片的版式按**窗口宽窄**分档（`SizeClass::of`），不是按编译目标：四档
//! **一排四列**手机桌面同排（2026-11-12 起改的 —— 原来手机两列的大卡把一屏
//! 占掉一半），窄档样张更小且无头条，桌面与平板同档样张带头条 —— 平板不该被
//! 当成手机。
//!
//! 主题网格两侧各有一条**可折叠**的头：浅深钉在哪侧，哪侧的网格摊开、另一侧收起
//! （水墨穿浅色主题，照浅侧算）；手动折过以后以手动为准。收起的网格不进元素树，
//! 一屏装得下更多内容。
//!
//! 界面语言从「关于」搬到这一页（2026-11-05）：它是**要改的东西**，不是「关于这个应用」
//! 的一条事实；放在外观这一屏的第一组，读者换语言不用先翻到末尾那一页。
//!
//! 主题**名**是这里的选项，主题**本身**不在这里定义：候选来自 UI 框架的主题集
//! 加上读者自己放进 `themes/` 的 JSON（见 `ui/foundation/theme.rs`）。每一侧都记名字，
//! 即使当前模式只用到其中一侧 —— 之后换模式时读者的选择还在。

use std::rc::Rc;

use folio_core::infra::i18n::Locale;
use gpui_kit::component::collapsible::Collapsible;
use gpui_kit::component::h_flex;
use gpui_kit::component::label::Label;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, Theme, ThemeColor, ThemeConfig, ThemeRegistry, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, FontWeight, Hsla, InteractiveElement as _, IntoElement as _, ParentElement,
    StatefulInteractiveElement as _, Styled, TestSupportExt as _, Window, div, px, rgb,
};

use crate::ui::components::muted_label;
use crate::ui::shell::navigation::SizeClass;

use super::super::*;

pub(in crate::ui::features::settings) fn appearance_page(cx: &App) -> Page {
    // 顺序照改外观的目光走：浅深与主题是这一页的正事，语言一年也碰不上一回，
    // 压到最后 —— 别让它占着第一格。
    page(
        tr(cx, "page.appearance"),
        IconName::Palette,
        vec![
            mode_group(cx),
            themes_group(cx),
            reading_theme_group(cx),
            language_group(cx),
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
            Row::custom_block(tr(cx, "field.appearance_mode"), move |window, cx| {
                mode_cards_row(window, cx)
            })
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

/// 「界面浅深」这一行：每一档画成一张**预览缩略图卡**（浅 / 深 / 水墨 / 跟随系统），
/// 照 OpenLogi 外观页与 macOS 系统设置的做法 —— 选之前就能看见每一档长什么样，
/// 不用先选中再反悔。四档一排四列，手机桌面同排：手机上两列的大卡把一屏占掉
/// 一半（2026-11-12 读者反馈），缩成一排小卡后这一行只占一条；窄窄一格里
/// 预览照旧说话，选中态仍走描边 + 勾两条通路。
fn mode_cards_row(window: &mut Window, cx: &mut App) -> AnyElement {
    let compact = SizeClass::of(window).is_compact();
    let current = ConfigHolder::default_appearance(cx).mode;
    let cards = [
        (Mode::Light, "mode-card-light", tr(cx, "appearance.light")),
        (Mode::Dark, "mode-card-dark", tr(cx, "appearance.dark")),
        (Mode::Ink, "mode-card-ink", tr(cx, "appearance.ink_wash")),
        (
            Mode::System,
            "mode-card-system",
            tr(cx, "appearance.follow_system"),
        ),
    ];
    let mut grid = div().w_full().grid().gap_2().grid_cols(4);
    for (mode, id, label) in cards {
        let selected = current == mode;
        grid = grid.child(mode_card(cx, mode, id, label, selected, compact));
    }
    grid.into_any_element()
}

/// 一张浅深卡：预览缩略图 + 名字。壳照同页 `theme_card` —— 主色描边与右上角勾
/// 两条通路传选中态，不只靠颜色；缩略图里画的是这一档**自己的样子**（固定色，
/// 见 [`mode_preview`]）。
fn mode_card(
    cx: &App,
    mode: Mode,
    id: &'static str,
    label: SharedString,
    selected: bool,
    compact: bool,
) -> AnyElement {
    let theme = cx.theme();
    // 选中圈用**当前界面**的主色：卡是界面的一部分，被预览的那一档不管哪一侧，
    // 拿它自己的浅墨当浅色界面上的描边都等于没有描边。
    let border = if selected {
        theme.primary
    } else {
        theme.border
    };
    let hover = theme.muted_foreground;
    let ink = theme.foreground;
    let click_mode = mode;
    let aria = format!("{label} · {}", mode.as_str());

    div()
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
        .on_click(move |_, _, cx| choose_mode(cx, click_mode))
        .child(mode_preview(mode, selected, compact))
        .child(
            div()
                .min_w_0()
                .w_full()
                // 一排四格之后窄卡装不下 text_sm 的「跟随系统」（四字 56px 贴边），
                // 手机档落到 text_xs，桌面仍与主题卡的标签同档。
                .map(|style| {
                    if compact {
                        style.text_xs()
                    } else {
                        style.text_sm()
                    }
                })
                .text_ellipsis()
                .text_color(ink)
                .child(label),
        )
        .into_any_element()
}

/// 一张卡的预览缩略图：这一档下书页长什么样 —— 壁纸底上**居中一页竖样张**
/// （[`mini_page`]）。阅读应用的页是竖的，样张也画竖的才说得中那一档的样子；
/// 「跟随系统」劈成左右两半，各半一页窄样张。
///
/// 颜色是**固定的**，不取当前主题（[`PreviewPalette`]）：这一格回答「取哪一侧」，
/// 下面的主题网格才回答「那一侧用哪套」。样张若跟着主题换色，两排会互相抢话；
/// 固定色让这一行在任何主题下读起来都一样。裸色值在这里放行是 design guides 里
/// 「颜色本身是数据」的那条例外 —— 预览画的就是那几格固定的明暗。
fn mode_preview(mode: Mode, selected: bool, compact: bool) -> AnyElement {
    // 窄卡档（一排四列的手机行）样张矮一截、不带头条（纯书页）：预览在 ~70px 宽的
    // 卡里只说「这一侧明还是暗」，再大就把一行顶成两屏；桌面档高一些、带一条头条
    // 小横条，让它读作「应用里的一页」。
    let (frame_h, page_w, page_h, page_bar) = if compact {
        (px(64.), 36., 52., false)
    } else {
        (px(120.), 72., 104., true)
    };
    let badge = if compact { 12. } else { 16. };
    // 「跟随系统」的半幅页随档缩：窄卡那格两半各剩 ~30px，页样张再按全幅的比画
    // 就糊成一团。
    let (half_w, half_h) = if compact { (24., 36.) } else { (52., 80.) };
    // 壁纸框：整幅预览的底，样张居中。
    let frame = |wallpaper: Hsla| {
        div()
            .relative()
            .w_full()
            .h(frame_h)
            .overflow_hidden()
            .rounded(px(6.))
            .bg(wallpaper)
            .flex()
            .items_center()
            .justify_center()
    };
    // 「跟随系统」的左右两半：各半自己的壁纸 + 一页窄样张。
    let half = |pal: PreviewPalette| {
        div()
            .flex_1()
            .h_full()
            .overflow_hidden()
            .bg(pal.wallpaper)
            .flex()
            .items_center()
            .justify_center()
            .child(mini_page(pal, half_w, half_h, page_bar))
    };
    match mode {
        Mode::Light => frame(light_preview().wallpaper)
            .child(mini_page(light_preview(), page_w, page_h, page_bar))
            .when(selected, |this| {
                this.child(mode_check_badge(light_preview(), badge))
            })
            .into_any_element(),
        Mode::Dark => frame(dark_preview().wallpaper)
            .child(mini_page(dark_preview(), page_w, page_h, page_bar))
            .when(selected, |this| {
                this.child(mode_check_badge(dark_preview(), badge))
            })
            .into_any_element(),
        Mode::Ink => {
            let pal = ink_preview();
            frame(pal.wallpaper)
                .child(mini_page(pal, page_w, page_h, page_bar))
                .when(selected, |this| this.child(mode_check_badge(pal, badge)))
                .into_any_element()
        }
        // 不用文案解释在跟谁：左半浅右半深各一页，一眼即知（OpenLogi 的 split
        // 同款思路，搬到竖样张上）。勾压在右上角，底色取浅侧的强调色 ——
        // 两半的强调色本来就是同一个蓝。
        Mode::System => h_flex()
            .w_full()
            .h(frame_h)
            .overflow_hidden()
            .rounded(px(6.))
            .relative()
            .child(half(light_preview()))
            .child(half(dark_preview()))
            .when(selected, |this| {
                this.child(mode_check_badge(light_preview(), badge))
            })
            .into_any_element(),
    }
}

/// 一页竖版样张：纸底、几条按**页宽比例**走的文本线（首条强调色），桌面档多一条
/// 头条小横条让它读作应用里的一页。行数由页高推出 —— 同一个函数画全幅桌面页、
/// 窄卡的小页与「跟随系统」的半幅页，内边距与线宽都随页宽缩放，哪一档都装得下。
fn mini_page(pal: PreviewPalette, w: f32, h: f32, bar: bool) -> AnyElement {
    // 内边距跟页宽走：全幅页 7px、窄卡的 24px 小页压到 3px —— 固定 7px 会把
    // 小页的内容挤没。
    let pad = (w / 8.).round().clamp(3., 7.);
    let header = if bar { 8. } else { 0. };
    // h 3 的线 + 4 的行距；留一行的余量防凑整越界。
    let line_count = (((h - pad * 2. - header) + 1.) / 7.).floor().max(3.) as usize;
    v_flex()
        .w(px(w))
        .h(px(h))
        .overflow_hidden()
        .rounded(px(4.))
        .bg(pal.window)
        .when(bar, |this| {
            this.child(div().h(px(8.)).w_full().flex_shrink_0().bg(pal.bar))
        })
        .child(
            v_flex()
                .p(px(pad))
                .gap(px(4.))
                .children((0..line_count).map(|i| {
                    // 首条是这一档里「点得出来的颜色」；其余是正文墨色的浓淡交替，
                    // 隔三行淡一档，读作一段排出来的文字。
                    let color = if i == 0 {
                        pal.accent
                    } else if i % 3 == 2 {
                        pal.bar.opacity(0.55)
                    } else {
                        pal.bar
                    };
                    let t = match i % 4 {
                        0 => 0.55,
                        1 => 0.82,
                        2 => 0.64,
                        _ => 0.42,
                    };
                    line(color, w * t)
                })),
        )
        .into_any_element()
}

/// 样张里的一条文本线，比色带细一档（h 3）。
fn line(color: Hsla, width: f32) -> impl IntoElement {
    div()
        .w(px(width))
        .h(px(3.))
        .flex_shrink_0()
        .rounded_full()
        .bg(color)
}

/// 选中态右上角那一枚勾。底色用这一档**自己的**强调色（固定蓝 / 水墨的灰），
/// 勾取窗纸色 —— 与正文主题的卡同一格思路：勾压在预览上要的是「这一档里点得出来
/// 的颜色」对「这一档的纸」的对比，而不是当前主题的任何一格。
fn mode_check_badge(pal: PreviewPalette, size: f32) -> AnyElement {
    div()
        .absolute()
        .top(px(4.))
        .right(px(4.))
        .size(px(size))
        .rounded_full()
        .bg(pal.accent)
        .flex()
        .items_center()
        .justify_center()
        .child(
            Icon::new(IconName::Check)
                .with_size(px(size * 0.6))
                .text_color(pal.window),
        )
        .into_any_element()
}

/// 点一张浅深卡：只写档位。换装在壳层渲染里发生 —— `AppShell` 每帧比对
/// `(appearance, resolved)`，变旧即 `theme::repaint`，这里不需要手工刷主题
/// （与下拉框时代同一条写入路径）。
fn choose_mode(cx: &mut App, mode: Mode) {
    ConfigHolder::edit_appearance(cx, |appearance| appearance.mode = mode);
}

/// 样张的一组固定色：壁纸、纸底、头条、强调色（文本线的主线）。
#[derive(Clone, Copy)]
struct PreviewPalette {
    wallpaper: Hsla,
    window: Hsla,
    bar: Hsla,
    accent: Hsla,
}

/// 浅色档的样子。数值照搬 OpenLogi 的缩略图 —— 那组色在浅界面的卡上验过读得清。
fn light_preview() -> PreviewPalette {
    PreviewPalette {
        wallpaper: rgb(0xdf_e4ec).into(),
        window: rgb(0xff_ffff).into(),
        bar: rgb(0xcc_d2db).into(),
        accent: rgb(0x3b_82f6).into(),
    }
}

/// 深色档的样子。
fn dark_preview() -> PreviewPalette {
    PreviewPalette {
        wallpaper: rgb(0x26_262b).into(),
        window: rgb(0x1b_1b1e).into(),
        bar: rgb(0x3a_3a42).into(),
        accent: rgb(0x3b_82f6).into(),
    }
}

/// 水墨档 = 浅色那组**过一遍去饱和**。水墨的机制就是「读者选的那本浅色主题压掉
/// 饱和度」（`foundation::theme` 的 `desaturate_active_theme`，同一把 Rec. 709 尺），
/// 样张照实画同一件事：预览里那点蓝也一并变灰 —— 全是彩色的水墨样张会让读者
/// 以为水墨档还能出颜色。
fn ink_preview() -> PreviewPalette {
    let gray = crate::ui::foundation::style::desaturate;
    let light = light_preview();
    PreviewPalette {
        wallpaper: gray(light.wallpaper),
        window: gray(light.window),
        bar: gray(light.bar),
        accent: gray(light.accent),
    }
}

/// 两侧各用哪一套。每一套画成一张卡，浅深各一组 —— 读者要先看见颜色再决定，
/// 名字里读不出「这本主题长什么样」。
fn themes_group(cx: &App) -> Group {
    group(
        tr(cx, "group.themes"),
        vec![
            theme_cards_row(ThemeMode::Light),
            theme_cards_row(ThemeMode::Dark),
        ],
    )
}

/// 折叠状态那格的键。状态存在**窗口的 keyed state** 里：规格每帧重建，这里存不得；
/// 页面切走再回来状态自然复位，自动规则重新接管 —— 那正是想要的行为。
const THEME_FOLD_KEY: &str = "appearance-theme-fold";

/// 没被手动折过时，一侧的主题网格跟着浅深档走：钉了哪侧就先看见哪侧的主题，
/// 「跟随系统」与水墨（穿浅色主题）两边都摊开。[`theme_section`] 的纯函数半边，
/// 测试钉这一格。
fn theme_side_auto_open(mode: Mode, dark: bool) -> bool {
    match mode {
        Mode::Dark => dark,
        Mode::Light | Mode::Ink => !dark,
        Mode::System => true,
    }
}

/// 一侧（浅或深）的主题网格，连着一条**可点的折叠头**。标题与说明沿用
/// `field.theme_*` 文案；说明只上桌面档（手机那一档行内不摆灰字）。浅深钉在哪侧，
/// 哪侧摊开、另一侧收起；手动折过以后以手动为准 —— 一屏装得下更多内容，
/// 收起的那侧随时点开头就回来。
fn theme_cards_row(mode: ThemeMode) -> Row {
    Row::custom(move |window, cx| theme_section(window, cx, mode))
        .keywords(&["theme", "color", "主题", "配色", "外观", "折叠"])
}

/// 折叠头 + 网格那一行。折叠状态是 `None = 跟随浅深档自动 / Some = 读者手动折过`，
/// 两侧各一格。
fn theme_section(window: &mut Window, cx: &mut App, mode: ThemeMode) -> AnyElement {
    let compact = SizeClass::of(window).is_compact();
    let fold = window.use_keyed_state(THEME_FOLD_KEY, cx, |_, _| [None::<bool>; 2]);
    let side: usize = if mode.is_dark() { 1 } else { 0 };
    let mode_now = ConfigHolder::default_appearance(cx).mode;
    let auto = theme_side_auto_open(mode_now, mode.is_dark());
    let open = fold.read(cx)[side].unwrap_or(auto);
    let (title, hint) = if mode.is_dark() {
        (tr(cx, "field.theme_dark"), tr(cx, "field.theme_dark.desc"))
    } else {
        (
            tr(cx, "field.theme_light"),
            tr(cx, "field.theme_light.desc"),
        )
    };
    let fold_state = fold.clone();

    // 头：标题（+ 桌面档的灰说明）在左，chevron 在右 —— 合上时朝右、摊开时朝下，
    // 与阅读目录那棵树同一套指向。
    let mut head_label = v_flex()
        .flex_1()
        .min_w_0()
        .child(Label::new(title).text_sm());
    if !compact {
        head_label = head_label.child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(hint),
        );
    }
    let head = h_flex()
        .id(SharedString::from(format!("theme-fold-{side}")))
        .w_full()
        .items_center()
        .gap_2()
        .py_1()
        .cursor_pointer()
        .on_click(move |_, _, cx| {
            // 对**当前有效状态**取反：自动收着的侧，点一下是展开；手动展开的侧，
            // 再点一下是折回。
            fold_state.update(cx, |sides, _| sides[side] = Some(!open));
        })
        .child(head_label)
        .child(
            Icon::new(if open {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .small()
            .flex_none()
            .text_color(cx.theme().muted_foreground),
        );

    Collapsible::new()
        .open(open)
        .content(theme_grid(cx, mode, compact))
        .child(head)
        .into_any_element()
}

/// 「正文主题」这一组：阅读界面（纸/墨/链接）自己的配色，与上面那两排**壳**主题分离
/// —— 壳管设置页与书架画哪套，这一格只管正文纸上是什么颜色。写入的是全局默认，
/// 阅读面板里那一格写的是本书差量（可覆盖全局）。
fn reading_theme_group(cx: &App) -> Group {
    group(
        tr(cx, "group.reading_theme"),
        vec![
            Row::custom_block(tr(cx, "reading_theme.title"), move |window, cx| {
                reading_theme_row(window, cx)
            })
            .description(tr(cx, "reading_theme.desc"))
            .keywords(&[
                "theme", "paper", "sepia", "纸", "纸色", "正文", "羊皮", "夜间", "背景",
            ]),
        ],
    )
}

/// 全局默认的正文主题卡条。样卡画**全局默认**解析出来的那一侧：软件浅深钉在哪侧
/// 就画哪侧，「跟随系统」按这台机器当下的外观折（与壳主题网格同一条读法）。
/// 卡的大小按窗口宽窄分档 —— 平板与桌面同档，不按编译目标。
fn reading_theme_row(window: &mut Window, cx: &mut App) -> AnyElement {
    let appearance = ConfigHolder::default_appearance(cx);
    let current = appearance
        .reading_theme
        .clone()
        .filter(|name| folio_core::infra::reading_theme::find(name).is_some());
    let dark = crate::ui::foundation::reading_theme::dark_side(&appearance, cx);
    let desktop = !SizeClass::of(window).is_compact();
    crate::ui::components::ReadingThemePicker {
        selected: current.as_deref(),
        dark,
        desktop,
        on_pick: std::rc::Rc::new(|name, cx| {
            ConfigHolder::edit_appearance(cx, |appearance| {
                appearance.reading_theme = name.filter(|name| !name.is_empty());
            });
        }),
    }
    .render(cx)
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
fn swatch_of(config: &Rc<ThemeConfig>, ink: bool) -> Swatch {
    let base = if config.mode.is_dark() {
        ThemeColor::dark()
    } else {
        ThemeColor::light()
    };
    let mut theme = Theme::from(base.as_ref());
    theme.apply_config(config);
    // 水墨那一档整个应用都去饱和，预览卡也不例外：不这么写，读者会在「全是黑白」
    // 的外壳里看见一屏彩色小方块，以为哪些主题还能出颜色。
    let gray = crate::ui::foundation::style::desaturate;
    let mut swatch = Swatch {
        background: theme.colors.background,
        primary: theme.colors.primary,
        primary_foreground: theme.colors.primary_foreground,
        foreground: theme.colors.foreground,
    };
    if ink {
        swatch = Swatch {
            background: gray(swatch.background),
            primary: gray(swatch.primary),
            primary_foreground: gray(swatch.primary_foreground),
            foreground: gray(swatch.foreground),
        };
    }
    swatch
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

/// 一套主题的网格。桌面与平板三列、手机两列（按窗口宽窄分档）—— 手机那块
/// 393dp 的宽度放三列会把名字挤成省略号，而名字正是让人分辨两套相近配色的最后一格。
fn theme_grid(cx: &App, mode: ThemeMode, compact: bool) -> AnyElement {
    let ink = ConfigHolder::default_appearance(cx).mode == folio_core::infra::config::Mode::Ink;
    let themes: Vec<(SharedString, Swatch)> = {
        let registry = ThemeRegistry::global(cx);
        registry
            .sorted_themes()
            .into_iter()
            .filter(|config| config.mode == mode)
            .map(|config| (config.name.clone(), swatch_of(config, ink)))
            .collect()
    };
    if themes.is_empty() {
        return muted_label(tr(cx, "appearance.no_themes"), cx).into_any_element();
    }
    let selected = selected_name(cx, mode);
    let mut grid = div().w_full().grid().gap_2();
    grid = if compact {
        grid.grid_cols(2)
    } else {
        grid.grid_cols(3)
    };
    for (name, swatch) in themes {
        let is_selected = name == selected;
        grid = grid.child(theme_card(cx, name, mode, swatch, is_selected, compact));
    }
    grid.into_any_element()
}

/// 一张主题卡。两端的差别只在**卡片里装什么**（按窗口宽窄分档，平板随桌面）：
///
/// - 桌面档：预览是一页**竖样张**，页里一块 `Aa` 字样（看得出这套主题的字色与底色
///   关系），名字右边挂「浅色 / 深色」标签，方便在一屏混排时认；
/// - 手机档：预览只有几条色带，名字一行，浅深由上方那一组标题说 —— 同一句话在
///   172 宽的卡里再写一遍，只会把名字挤掉。
///
/// 两侧共同的是：选中态同时有**主色描边**与**右上角勾**，不只靠颜色传义。
fn theme_card(
    cx: &App,
    name: SharedString,
    mode: ThemeMode,
    swatch: Swatch,
    selected: bool,
    compact: bool,
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

    let preview = if compact {
        phone_preview(swatch, selected)
    } else {
        desktop_preview(swatch, selected)
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

    if compact {
        card = card.child(
            div()
                .min_w_0()
                .w_full()
                .text_sm()
                .text_ellipsis()
                .text_color(ink)
                .child(name),
        );
    } else {
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
    }
    card.into_any_element()
}

/// 桌面档的预览：一页**竖样张**，页里一块 `Aa` 与一栏墨色浓淡交替的文本线 ——
/// 这套主题自己的纸、墨、主色三格关系一眼可读。纸就是这套主题自己的底色，
/// 整页铺满（模式卡才有壁纸环）。
fn desktop_preview(swatch: Swatch, selected: bool) -> AnyElement {
    div()
        .relative()
        .w_full()
        .h(px(120.))
        .overflow_hidden()
        .rounded(px(6.))
        .bg(swatch.background)
        .p(px(10.))
        .child(
            v_flex()
                .gap(px(5.))
                .child(
                    div()
                        .text_base()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(swatch.foreground)
                        .child("Aa"),
                )
                .child(bar(swatch.primary, 44.))
                .child(bar(swatch.foreground.opacity(0.30), 66.))
                .child(bar(swatch.foreground.opacity(0.18), 52.))
                .child(bar(swatch.foreground.opacity(0.12), 60.))
                .child(bar(swatch.foreground.opacity(0.30), 40.))
                .child(bar(swatch.foreground.opacity(0.18), 58.))
                .child(bar(swatch.foreground.opacity(0.12), 46.)),
        )
        .when(selected, |this| this.child(check_badge(swatch, 16.)))
        .into_any_element()
}

/// 手机档的预览：只有一栏细色带。卡片窄，`Aa` 会跟名字抢同一条视线。
fn phone_preview(swatch: Swatch, selected: bool) -> AnyElement {
    div()
        .relative()
        .w_full()
        .h(px(96.))
        .overflow_hidden()
        .rounded(px(4.))
        .bg(swatch.background)
        .p(px(8.))
        .child(
            v_flex()
                .gap(px(4.))
                .child(bar(swatch.primary, 36.))
                .child(bar(swatch.foreground.opacity(0.30), 52.))
                .child(bar(swatch.foreground.opacity(0.18), 44.))
                .child(bar(swatch.foreground.opacity(0.12), 56.))
                .child(bar(swatch.foreground.opacity(0.30), 34.))
                .child(bar(swatch.foreground.opacity(0.18), 50.))
                .child(bar(swatch.foreground.opacity(0.12), 40.)),
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

#[cfg(test)]
mod tests {
    use super::{
        LANGUAGE_KEYWORDS, apply_choice, dark_preview, ink_preview, language_options,
        light_preview, swatch_of,
    };
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
            let light = swatch_of(&light, false);
            let dark = swatch_of(&dark, false);
            assert!(
                light.background.l > dark.background.l,
                "浅色主题的预览底色该比深色亮：{light:?} / {dark:?}"
            );
        });
    }

    /// 水墨预览的固定色必须是浅色那组**去饱和**的结果：水墨档的机制就是「浅色主题
    /// 压掉饱和度」，预览照实画同一件事 —— 预览里若还留着彩色，读者会以为水墨档
    /// 还能出颜色。钉两条：色相拿干净；观感亮度不挪（Rec. 709 的 luma，不是 HSL 的
    /// lightness —— 蓝色的这两个值差得远，量错了等式就钉不住机制）。
    #[test]
    fn the_ink_preview_is_the_light_one_desaturated() {
        let luma = |color: gpui_kit::Hsla| {
            let rgba = gpui_kit::Rgba::from(color);
            0.2126 * rgba.r + 0.7152 * rgba.g + 0.0722 * rgba.b
        };
        let light = light_preview();
        let ink = ink_preview();
        for (name, raw, gray) in [
            ("壁纸", light.wallpaper, ink.wallpaper),
            ("窗纸", light.window, ink.window),
            ("头条", light.bar, ink.bar),
            ("强调色", light.accent, ink.accent),
        ] {
            assert!(
                gray.s.abs() < 1e-4,
                "水墨预览的{name}还留着饱和度：{gray:?}"
            );
            assert!(
                (luma(raw) - luma(gray)).abs() < 1e-4,
                "水墨预览的{name}亮度不该变：{raw:?} → {gray:?}"
            );
        }
    }

    /// 浅色档的预览底色要比深色档亮：两张缩略图并排摆着，「浅」「深」的名字
    /// 必须跟画出来的明暗对得上。
    #[test]
    fn the_light_preview_is_brighter_than_the_dark_one() {
        assert!(
            light_preview().wallpaper.l > dark_preview().wallpaper.l,
            "浅色预览的壁纸该比深色亮"
        );
    }

    /// 折展的自动规则：钉了哪侧就先看见哪侧的主题网格，「跟随系统」与水墨
    /// （穿浅色主题）两边都摊开。手动折过的那一格走 `Some`，轮不到这条规则。
    #[test]
    fn the_theme_grids_follow_the_pinned_side_until_overridden() {
        use super::theme_side_auto_open;
        // (模式, 是否深侧) → 该侧摊开吗
        let cases = [
            (Mode::System, false, true),
            (Mode::System, true, true),
            (Mode::Light, false, true),
            (Mode::Light, true, false),
            (Mode::Dark, false, false),
            (Mode::Dark, true, true),
            (Mode::Ink, false, true),
            (Mode::Ink, true, false),
        ];
        for (mode, dark, open) in cases {
            assert_eq!(
                theme_side_auto_open(mode, dark),
                open,
                "{mode:?} 档下{}侧的网格该{}",
                if dark { "深" } else { "浅" },
                if open { "摊开" } else { "收起" }
            );
        }
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
