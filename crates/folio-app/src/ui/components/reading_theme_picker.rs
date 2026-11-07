//! 正文主题的选择卡：一排「纸上是什么颜色」的小卡，首卡是「跟随软件」。
//!
//! 壳主题的选择卡（设置外观页那套 `theme_card`）预览的是**整本主题**的样子；这里的卡
//! 只回答一个问题 —— **纸是什么颜色、墨是什么颜色、链接是什么颜色**。日 / 夜是同一套
//! 主题的两半（翻面翻的是深侧），所以每张卡画当前生效那一侧的样色，日夜轴由 `dark`
//! 这一格说，不在卡上重复。
//!
//! 「跟随软件」那张卡不用文案解释自己在跟谁：它的预览块直接铺**当下壳主题**的纸与墨
//! —— 选它就是「正文纸色回到壳主题那张纸」。
//!
//! 卡片只交出**名字**，写配置是调用方的事（阅读面板写本书差量、设置页写全局默认）——
//! 这一层不知道配置长什么样。

use std::rc::Rc;

use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement as _, Styled, div, px,
};

use crate::ui::foundation::reading_theme;
use crate::ui::services::l10n::tr;

/// 一张卡在两个家（阅读面板的横滑条、设置页的网格）里都要的信息。
pub struct ReadingThemePicker<'a> {
    /// 当前选中的主题名；`None` = 「跟随软件」那张卡选中。
    pub selected: Option<&'a str>,
    /// 样卡画哪一侧（日夜轴现在的位置）。
    pub dark: bool,
    /// 桌面卡略大、带「Aa」字样；手机卡只有色带。
    pub desktop: bool,
    /// 点了一张卡。参数是**主题名**；「跟随软件」给 `None`。
    pub on_pick: Rc<dyn Fn(Option<String>, &mut App)>,
}

impl ReadingThemePicker<'_> {
    pub fn render(self, cx: &App) -> AnyElement {
        let Self {
            selected,
            dark,
            desktop,
            on_pick,
        } = self;
        let follow_label = tr(cx, "reading_theme.follow_app");
        let shell = cx.theme();
        let follow_swatch = Swatch {
            background: shell.background,
            fg: shell.foreground,
            link: shell.blue,
        };
        let mut row = h_flex()
            .id("reading-theme-cards")
            .w_full()
            .gap_2()
            .overflow_x_scroll();
        // 首卡：跟随软件。
        let follow_selected = selected.is_none();
        row = row.child(reading_theme_card(
            cx,
            SharedString::from("reading-theme-card-follow"),
            follow_label,
            follow_swatch,
            follow_selected,
            desktop,
            {
                let on_pick = on_pick.clone();
                move |cx| on_pick(None, cx)
            },
        ));
        for name in reading_theme::built_in_names() {
            let Some(colors) = reading_theme::swatch(name, dark) else {
                continue;
            };
            let label = tr(cx, &format!("reading_theme.{name}"));
            let is_selected = selected == Some(name);
            let name_string = name.to_string();
            row = row.child(reading_theme_card(
                cx,
                SharedString::from(format!("reading-theme-card-{name}")),
                label,
                Swatch {
                    background: colors.bg,
                    fg: colors.fg,
                    link: colors.link,
                },
                is_selected,
                desktop,
                {
                    let on_pick = on_pick.clone();
                    move |cx| on_pick(Some(name_string.clone()), cx)
                },
            ));
        }
        row.into_any_element()
    }
}

/// 一张卡要画的三格颜色。壳主题那张卡也用同一份形状（来源是 `cx.theme()`）。
#[derive(Clone, Copy)]
struct Swatch {
    background: Hsla,
    fg: Hsla,
    link: Hsla,
}

/// 卡片：预览块 + 名字。选中态同时有**主色描边**与**右上角勾**，与壳主题的卡同一条
/// 规矩 —— 不只靠颜色传义（选中的那张卡往往就是纸色最浅的那张，描边撞色是常事）。
///
/// 预览块里「Aa」用的是这一套主题自己的墨色：字压在**它自己的纸**上，而不是压在界面
/// 的底色上 —— 读者要看的正是这一格关系。
fn reading_theme_card(
    cx: &App,
    id: SharedString,
    label: SharedString,
    swatch: Swatch,
    selected: bool,
    desktop: bool,
    on_pick: impl Fn(&mut App) + 'static,
) -> AnyElement {
    // 卡的**外壳**照界面走：描边、底、名字的墨色都是当前主题的那几格 —— 卡是界面的一
    // 部分，只有预览块里那张「纸」是被预览的配色。选中圈用**界面**的主色，不用被预览
    // 那套的任何一格：深色纸的卡压在浅色界面上，拿自己的浅墨当描边等于没有描边。
    let theme = cx.theme();
    let border = if selected {
        theme.primary
    } else {
        theme.border
    };
    let hover = theme.muted_foreground;
    let ink = theme.foreground;
    let preview_h = if desktop { px(56.) } else { px(40.) };
    let preview_w = if desktop { px(96.) } else { px(76.) };

    div()
        .id(id)
        .w(preview_w + px(16.))
        .p(px(8.))
        .gap_1()
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
        .on_click(move |_, _, cx| on_pick(cx))
        .child(preview_block(
            swatch, selected, desktop, preview_w, preview_h,
        ))
        .child(
            div()
                .min_w_0()
                .w_full()
                .text_xs()
                .text_ellipsis()
                .text_color(ink)
                .child(label),
        )
        .into_any_element()
}

/// 预览块：这一套配色的纸/墨/链接。桌面多一个「Aa」，手机只有色带 —— 卡窄，
/// 字会跟名字抢同一条视线。
fn preview_block(
    swatch: Swatch,
    selected: bool,
    desktop: bool,
    w: gpui_kit::Pixels,
    h: gpui_kit::Pixels,
) -> AnyElement {
    div()
        .relative()
        .w(w)
        .h(h)
        .overflow_hidden()
        .rounded(px(6.))
        .bg(swatch.background)
        .p(px(8.))
        .child(
            v_flex()
                .gap(px(4.))
                .when(desktop, |this| {
                    this.child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(swatch.fg)
                            .child("Aa"),
                    )
                })
                .child(bar(swatch.link, 30.))
                .child(bar(swatch.fg.opacity(0.30), 46.)),
        )
        .when(selected, |this| this.child(check_badge(swatch)))
        .into_any_element()
}

fn bar(color: Hsla, width: f32) -> impl IntoElement {
    div()
        .w(px(width))
        .h(px(3.))
        .flex_shrink_0()
        .rounded_full()
        .bg(color)
}

/// 右上角那颗勾。底色用**链接色**而不是主色：预览块里主色没有格子，而链接色正是
/// 这一排版面上「点得出来的颜色」，勾压在纸上要的就是这个对比。
fn check_badge(swatch: Swatch) -> impl IntoElement {
    div()
        .absolute()
        .top(px(4.))
        .right(px(4.))
        .size(px(14.))
        .rounded_full()
        .bg(swatch.link)
        .flex()
        .items_center()
        .justify_center()
        .child(
            Icon::new(gpui_kit::assets::IconName::Check)
                .with_size(px(9.))
                .text_color(swatch.background),
        )
}
