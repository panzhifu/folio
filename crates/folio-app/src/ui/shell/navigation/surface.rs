//! 导航栏三档共用的两样东西：**表面**与**入口格**。
//!
//! 表面只差两件事：浮起来的圆角胶囊 / 嵌入的整列。视觉上其余全部一致 —— 三档统一
//! 说的就是这个：同一种底色、同一种描边、同一种字号；选中的表达按形状来（底栏换色，
//! 侧栏再洗一层）。
//!
//! 这里的「磨砂」在走 wgpu 渲染器的那两端（Android / Linux 桌面）是**真**背景模糊：
//! `.backdrop_blur()` 让渲染器把玻璃矩形底下那一块从屏幕上拷走、降采样、两趟可分离高斯，
//! 再采回来；tint 是压在玻璃上的另一层，所以「糊多少」与「透多少」各自可调。Metal 与
//! DirectX 没有这条路径，那两端的表面退回「半透明 + 描边 + 投影」的近似。
//!
//! 入口格**手绘**而不是用 gpui-kit 的 `Button` / `SidebarMenuItem`：前者把内容排成一条
//! 横的 `h_flex` 并强制 `h_6`，后者是横排图标 + 文字、只有 48 / 260 两档宽。两种都塞不进
//! 「96 宽、图标在上文字在下」这一格。

use gpui_kit::component::{ActiveTheme, Icon, Sizable, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, BoxShadow, Div, Hsla, InteractiveElement as _, IntoElement, ParentElement,
    StatefulInteractiveElement as _, Styled, Window, div, hsla, px,
};

use super::{Destination, NAV_RADIUS};
use crate::ui::services::l10n::tr;

/// 入口格里的图标边长。华为底栏与侧边页签都是 24。
const ICON: f32 = 24.0;
/// 玻璃的模糊半径（逻辑像素）。参照的是系统级毛玻璃那一档：够把封面与字压成一片色块，
/// 又不至于让底栏看起来像一块磨砂塑料。
const BLUR_RADIUS: f32 = 28.0;
/// tint 的不透明度。有模糊兜着才敢压到 0.90 以下；再往下 12px 的标签就开始吃底下的对比度。
const GLASS_TINT: f32 = 0.62;
/// 侧栏那一格的尺寸。
const TILE_W: f32 = 80.0;
const TILE_H: f32 = 56.0;
/// 卡片内缩：四项上下各留 8，左右那 8 让 80 宽的格子居中到 96 里。
const CARD_PAD: f32 = 8.0;

/// 悬浮档的表面：圆角、**真背景模糊**、半透明 tint、描边、一层柔影。
///
/// 调用方自己给宽高与定位 —— 底栏是「左右拉满、56 高」，侧栏是「96 宽、内容高」。
///
/// 玻璃与 tint 是两层：`.backdrop_blur()` 把底下已经画上屏的那一块糊掉，`.bg()` 那层
/// 半透明色再压在它上面。所以「糊多少」与「透多少」各自可调，互不牵制。有了模糊兜底，
/// tint 才敢从 0.90 压到 [`GLASS_TINT`] —— 从前那版没有模糊，压到 0.7 以下 12px 的字
/// 就会在封面图上糊成一片。
///
/// 只有走 wgpu 渲染器的那两端（Android 与 Linux 桌面）真做模糊；Metal 与 DirectX 跳过
/// 玻璃那一层，屏幕上退回「半透明 + 描边 + 投影」的近似，也就是这一格改动之前的样子。
pub(super) fn floating(cx: &App) -> Div {
    div()
        .rounded(px(NAV_RADIUS))
        .backdrop_blur(px(BLUR_RADIUS))
        .bg(cx.theme().sidebar.opacity(GLASS_TINT))
        .border_1()
        .border_color(cx.theme().sidebar_border)
        .shadow(vec![
            BoxShadow::new(px(0.), px(4.), hsla(0., 0., 0., 0.16)).blur_radius(px(12.)),
            BoxShadow::new(px(0.), px(1.), hsla(0., 0., 0., 0.06)).blur_radius(px(2.)),
        ])
}

/// 嵌入档的表面：整列不透明，右边一条发丝线把它与内容分开。
///
/// 不透明是对的：嵌入态没有「内容从底下穿过」这回事，半透明只会让底下的正文色透上来，
/// 在阅读器里那是最难看的一种脏。
pub(super) fn embedded(cx: &App) -> Div {
    div()
        .bg(cx.theme().sidebar)
        .border_r_1()
        .border_color(cx.theme().sidebar_border)
}

/// 入口格两种形状。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Shape {
    /// 底栏那一格：占满一份 `flex_1`；选中只换图标与文字的颜色，不画底。
    Tab,
    /// 侧栏那一格：80 × 56 的方块，选中时整格洗一层主色。
    Tile,
}

/// 一个入口：图标在上、文案在下。`on_select` 在点击时收到这个目的地。
///
/// 换页的提示都用**颜色**表达：底栏只换图标与文字的色，侧栏那一档再洗一层主色。
/// 不额外画标记条 —— 那会打破整片表面。
pub(super) fn nav_item<F>(
    cx: &App,
    destination: Destination,
    active: bool,
    shape: Shape,
    on_select: F,
) -> impl IntoElement
where
    F: Fn(Destination, &mut Window, &mut App) + Clone + 'static,
{
    let label = tr(cx, destination.label_key());
    let ink = if active {
        cx.theme().primary
    } else {
        cx.theme().muted_foreground
    };
    let wash: Hsla = cx.theme().primary.opacity(0.12);
    let hover_wash: Hsla = cx.theme().sidebar_accent.opacity(0.60);

    let glyph = Icon::new(destination.icon())
        .with_size(px(ICON))
        .text_color(ink);

    let mut item = v_flex()
        .id(destination.slug())
        .aria_label(label.clone())
        .items_center()
        .justify_center()
        .gap(px(2.0))
        .cursor_pointer();

    // 一格的「图标 + 文字」：两种形状都要它，只是外面那层高亮不一样。
    let cell = |glyph: AnyElement| {
        v_flex()
            .items_center()
            .justify_center()
            .gap(px(2.0))
            .child(glyph)
            .child(
                div()
                    .text_xs()
                    .whitespace_nowrap()
                    .text_color(ink)
                    .child(label.clone()),
            )
    };

    item = match shape {
        // 选中只换颜色：不画底色、不加标记条。底色一圈会把「选中」从颜色变成一块形状，
        // 中文两字那一格宽一点就会读成一颗圆 —— Anx Reader 的底栏也是只换颜色。
        Shape::Tab => item.flex_1().h_full().child(cell(glyph.into_any_element())),
        Shape::Tile => item
            .w(px(TILE_W))
            .h(px(TILE_H))
            .rounded(px(16.0))
            .when(active, |el| el.bg(wash))
            .child(cell(glyph.into_any_element())),
    };

    item
        // 底栏没有 hover，按下态是手指按到那一格唯一的即时反馈；侧栏有 hover，不再叠一层。
        .when(shape == Shape::Tab, |el| {
            el.active(|style| style.opacity(0.75))
        })
        // hover 只给侧栏那一档：底栏是手指的界面，指针划过不该有东西亮起来；
        // 而桌面上侧栏是鼠标每天要扫过的列，不给反馈会读成「这一格不能点」。
        .when(shape == Shape::Tile && !active, |el| {
            el.hover(move |style| style.bg(hover_wash).rounded(px(16.0)))
        })
        .on_click(move |_, window, cx| {
            on_select(destination, window, cx);
        })
}

/// 卡片（或整列）里那一串入口：竖排、上下各 [`CARD_PAD`]。
pub(super) fn nav_stack(children: Vec<AnyElement>) -> Div {
    v_flex()
        .items_center()
        .py(px(CARD_PAD))
        .gap(px(4.0))
        .children(children)
}
