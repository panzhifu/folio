//! 纸面上的绘制原语：实心块、圆角块、背景图、边框（含虚线/3D）、折面填充。

use std::sync::Arc;

use crate::ui::features::reader::motion::turn_animation;
use gpui_kit::component::StyledExt as _;

use super::images::painted_picture;
use crate::ui::features::reader::model::image_atlas::ImageAtlas;
use crate::ui::features::reader::view::scene::{BlockDecor, BlockPicture};
use folio_core::content::format::css::computed::BorderStyle;
use gpui_kit::{
    AnyElement, App, Background, Corners, Entity, EntityId, Hsla, IntoElement,
    ParentElement as _, PathBuilder, Pixels, Point, RenderImage, Styled, Window, div, img, point,
    px,
};

/// 把要盖在纸上的那一个多边形填进场景。顶点已经带在元素坐标里（与字形的 `run.x` /
/// `run.baseline` 同一套），所以这里只加一个 `origin` 就完事，不必再补页边距 —— 补了就是
/// 旧版那一处「影子按 `margin + at` 摆、裁剪按 `at` 判」，两处差着一个页边距。
///
/// 为什么不用 `paint_quad`：quad 只能轴对齐，而这一档要的全部就是那条**斜的**折线（以及
/// 层叠那条跟着纸一起沉的上缘）；用 quad 画出来的就是旧版那一根竖棍。`build()` 失败只可能
/// 是退化到零面积的那一片，画不出东西不该让整帧失败 —— 与 `paint.rs` 里单个字形画不出去时
/// 同一个取舍。
pub(super) fn paint_fold(
    window: &mut Window,
    poly: &turn_animation::Poly,
    origin: Point<Pixels>,
    color: impl Into<Background>,
) {
    let mut path = PathBuilder::fill();
    let mut first = true;
    poly.each(|x, y| {
        let at = point(origin.x + px(x), origin.y + px(y));
        if first {
            path.move_to(at);
            first = false;
        } else {
            path.line_to(at);
        }
    });
    path.close();
    if let Ok(built) = path.build() {
        window.paint_path(built, color);
    }
}


/// 一块背景图：外壳是那一块的边框盒、带矩形裁剪，里面按 `scene` 算好的格子摆图。
///
/// 为什么要外壳那一层 `overflow_hidden`：`cover` 折出来的格子**本来就比盒大**
/// （缩到盖满，多出来的一截要裁掉），而 `repeat` 那一格会从定位那一点向两头溢出。
/// gpui 的 `ContentMask` 只有矩形 —— 这里要的正好是矩形，够用。
///
/// 坐标：外壳落在 `decor.x/top`（章内量，平移已由 `place_decorations` 加过），
/// 里面的格子相对外壳左上角，与 [`BlockPicture::tiles`] 的定义一致。
pub(super) fn background_child(
    decor: &BlockDecor,
    picture: &BlockPicture,
    rendered: Arc<RenderImage>,
) -> AnyElement {
    div()
        .absolute()
        .left(px(decor.x))
        .top(px(decor.top))
        .w(px(decor.width))
        .h(px(decor.height))
        .overflow_hidden()
        .children(picture.tiles.iter().map(|(left, top, width, height)| {
            div()
                .absolute()
                .left(px(*left))
                .top(px(*top))
                .w(px(*width))
                .h(px(*height))
                // 格子已经是「按比例算好的那一个矩形」，所以这里要的是铺满而不是再保比例：
                // 再套一层 `object_fit` 会在格子与比例对不上时**悄悄改小**，露出底色。
                .child(img(rendered.clone()).size_full())
        }))
        .into_any_element()
}


/// 一块实心矩形（背景或一条边框）。
fn fill_quad(x: f32, top: f32, width: f32, height: f32, color: Hsla) -> AnyElement {
    fill_rounded_quad(x, top, width, height, color, [0.0; 4])
}


/// 同 [`fill_quad`]，多给四个圆角半径（`[左上, 右上, 右下, 左下]`，px）。
///
/// 圆角走 gpui 自己那条通道（`corner_radii`），不是自己拼路径：四边形是唯一轴对齐的图元，
/// 而 `paint_quad` 的圆角与它同一份几何，自己拼一条圆角路径只会多一套对不齐的边。
pub(super) fn fill_rounded_quad(
    x: f32,
    top: f32,
    width: f32,
    height: f32,
    color: Hsla,
    radius: [f32; 4],
) -> AnyElement {
    div()
        .absolute()
        .left(px(x))
        .top(px(top))
        .w(px(width))
        .h(px(height))
        .bg(color)
        .corner_radii(Corners {
            top_left: px(radius[0]),
            top_right: px(radius[1]),
            bottom_right: px(radius[2]),
            bottom_left: px(radius[3]),
        })
        .into_any_element()
}


/// 沿一条边框带子的长边切出「实、空、实…」，返回每段的 `(起点, 长度)`。
///
/// **不平分**：从起点按 `dash + gap` 一路铺，最后一段按剩余长度截短 —— CSS 的虚线本来
/// 就是从一端铺到另一端、尾巴可能是半段，浏览器同样这么做。`dash` 非正或 `gap` 为负时
/// 给空表，防止调用方给一个除零的线宽时把这一帧卡在这条循环里。
pub(super) fn dash_spans(length: f32, dash: f32, gap: f32) -> Vec<(f32, f32)> {
    // 三个数都得是有限正数（`gap` 可以是 0，那是实线）：NaN 落进下面这条 `while`
    // 会一直为真，所以先一次判完再进循环。
    let usable = length > 0.0 && dash > 0.0 && gap >= 0.0 && length.is_finite();
    if !usable {
        return Vec::new();
    }
    let mut spans = Vec::new();
    let mut at = 0.0f32;
    while at < length {
        let len = dash.min(length - at);
        spans.push((at, len));
        at += dash + gap;
    }
    spans
}


/// 3D 边框的亮/暗两档：从作者的边框色派生。
///
/// 浏览器在 HSL 空间里调明度；这里同一条思路，但两档都写成**朝目标补**而不是加减常数 ——
/// 加减会把纯黑压成纯黑（亮档看不见）、把纯白顶成纯白。纯黑因此只能变亮、纯白只能变暗，
/// 与浏览器观察到的行为一致。
pub(super) fn three_d_tone(color: Hsla, light: bool) -> Hsla {
    let l = if light {
        color.l + (1.0 - color.l) * 0.35
    } else {
        color.l * 0.7
    };
    Hsla { l, ..color }
}


/// 把一条边框带子沿**厚度**方向切成外、内两半 → `(外, 内)` 两个盒子。
///
/// 「外」= 离盒子外缘更近的那一半：上边与左边是前一半，下边与右边是后一半。
fn split_outer_inner(x: f32, y: f32, width: f32, height: f32, side: usize) -> ([f32; 4], [f32; 4]) {
    match side {
        0 => (
            [x, y, width, height / 2.0],
            [x, y + height / 2.0, width, height / 2.0],
        ),
        2 => (
            [x, y + height / 2.0, width, height / 2.0],
            [x, y, width, height / 2.0],
        ),
        1 => (
            [x + width / 2.0, y, width / 2.0, height],
            [x, y, width / 2.0, height],
        ),
        _ => (
            [x, y, width / 2.0, height],
            [x + width / 2.0, y, width / 2.0, height],
        ),
    }
}


/// 一条边框带子。`side` 是 `[上, 右, 下, 左]` 的下标 —— 3D 那四档要靠它分「哪条边暗」，
/// `dotted` / `dashed` 靠它分切段方向。
///
/// 各档画法：`solid` 一整条；`dotted` / `dashed` 按 [`dash_spans`] 切段；`double` 三段等分
/// （外线 1/3、缝 1/3、内线 1/3）；`inset` / `outset` 上/左与下/右各一档明暗；
/// `groove` / `ridge` 每条边再拆成外、内两半。
#[allow(clippy::too_many_arguments)]
pub(super) fn push_border(
    parts: &mut Vec<AnyElement>,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    color: Hsla,
    style: BorderStyle,
    side: usize,
) {
    // 上/下边是横带子（沿纵向拆厚度），左/右边是竖带子。
    let split_y = matches!(side, 0 | 2);
    match style {
        BorderStyle::Inset | BorderStyle::Outset => {
            // `inset`：上/左暗、下/右亮；`outset` 相反。
            let dark = (style == BorderStyle::Inset) == matches!(side, 0 | 3);
            parts.push(fill_quad(x, y, width, height, three_d_tone(color, !dark)));
            return;
        }
        BorderStyle::Groove | BorderStyle::Ridge => {
            // `groove` 外暗内亮；`ridge` 相反。
            let outer_light = style == BorderStyle::Ridge;
            let (outer, inner) = split_outer_inner(x, y, width, height, side);
            for (rect, light) in [(outer, outer_light), (inner, !outer_light)] {
                parts.push(fill_quad(
                    rect[0],
                    rect[1],
                    rect[2],
                    rect[3],
                    three_d_tone(color, light),
                ));
            }
            return;
        }
        _ => {}
    }
    if matches!(style, BorderStyle::Dotted | BorderStyle::Dashed) {
        // 带子的**长边**按段切开：`dotted` 方点边长 = 线宽、隙 = 线宽；
        // `dashed` 段 = 3×线宽、隙 = 3×线宽（浏览器那套比例，CSS 本身没规定）。
        // 点是方的不是圆的：绘制原语只有轴对齐四边形，1px 的点在屏上分不出圆方。
        let (long, thickness) = if split_y {
            (width, height)
        } else {
            (height, width)
        };
        let (dash, gap) = if style == BorderStyle::Dotted {
            (thickness, thickness)
        } else {
            (thickness * 3.0, thickness * 3.0)
        };
        for (at, len) in dash_spans(long, dash, gap) {
            parts.push(if split_y {
                fill_quad(x + at, y, len, thickness, color)
            } else {
                fill_quad(x, y + at, thickness, len, color)
            });
        }
        return;
    }
    if style != BorderStyle::Double {
        parts.push(fill_quad(x, y, width, height, color));
        return;
    }
    let seg = if split_y { height } else { width } / 3.0;
    if split_y {
        // 上/下边：两条横线落在这条带子的两端（3px 带 = 1px 线 / 1px 缝 / 1px 线）。
        parts.push(fill_quad(x, y, width, seg, color));
        parts.push(fill_quad(x, y + 2.0 * seg, width, seg, color));
    } else {
        // 左/右边：两条竖线同理。
        parts.push(fill_quad(x, y, seg, height, color));
        parts.push(fill_quad(x + 2.0 * seg, y, seg, height, color));
    }
}

/// 一块盒子的块级装饰：背景一整块 + 背景图 + 四边边框各一条实心条。
/// 几何与 [`image_child`] 同一坐标系；边条在角上互相压边，与 CSS 的 45° 拼角
/// 差一个斜切，肉眼分不出来。
///
/// **三段的先后就是 CSS 的层次**：底色在最下、图盖在底色上、边框再盖在图上
/// （`background-clip` 我们一律按边框盒，所以那道边正好压在底图边上，与浏览器一致）。
/// 图层倒着摆：`background-image` 列表里最后写的那一层垫底，而 gpui 的子元素是
/// 「后入树的压在上面」，两个次序正好反。
/// 这一块背景图是否都能画出来：每张都拿到了像素，或图集已判它失败。
///
/// 捕获副本要等它 —— 缺一张就烘出来的纹理永远缺那一张，而纹理一旦记成「已烘」
/// 不会再自己重来。失败算落地：那种图在实时页上也不会出现，纹理里空着才是实情。
pub(super) fn decoration_ready(
    decor: &BlockDecor,
    atlas: &Entity<ImageAtlas>,
    view: EntityId,
    cx: &mut App,
) -> bool {
    decor.pictures.iter().all(|picture| {
        painted_picture(atlas, &picture.image, view, cx).is_some()
            || atlas.read_with(cx, |atlas, _| atlas.failed(&picture.image.key))
    })
}

pub(super) fn decoration_child(
    decor: BlockDecor,
    atlas: &Entity<ImageAtlas>,
    view: EntityId,
    cx: &mut App,
) -> Vec<AnyElement> {
    let mut parts = Vec::new();
    if let Some(background) = decor.background {
        parts.push(fill_rounded_quad(
            decor.x,
            decor.top,
            decor.width,
            decor.height,
            background,
            decor.radius,
        ));
    }
    for picture in decor.pictures.iter().rev() {
        // 问一次 `painted` 就是替这一格记一笔「这一帧在场」，帧末的回收按这个数放人 ——
        // 与正文插图同一本账，扉页那张底图不会被单独饿掉。
        let Some(rendered) = painted_picture(atlas, &picture.image, view, cx) else {
            continue;
        };
        parts.push(background_child(&decor, picture, rendered));
    }
    let [top, right, bottom, left] = decor.border;
    if top.0 > 0.0 {
        push_border(
            &mut parts,
            decor.x,
            decor.top,
            decor.width,
            top.0,
            top.1,
            top.2,
            0,
        );
    }
    if bottom.0 > 0.0 {
        push_border(
            &mut parts,
            decor.x,
            decor.top + decor.height - bottom.0,
            decor.width,
            bottom.0,
            bottom.1,
            bottom.2,
            2,
        );
    }
    if left.0 > 0.0 {
        push_border(
            &mut parts,
            decor.x,
            decor.top,
            left.0,
            decor.height,
            left.1,
            left.2,
            3,
        );
    }
    if right.0 > 0.0 {
        push_border(
            &mut parts,
            decor.x + decor.width - right.0,
            decor.top,
            right.0,
            decor.height,
            right.1,
            right.2,
            1,
        );
    }
    parts
}

