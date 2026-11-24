//! `background-clip` 裁剪盒与 `background-origin` 定位盒的用例：默认、padding-box、
//! content-box。从 `tests.rs` 分出，只因为那个文件贴着行数上限；共享 helper 经 `super` 取用。

use super::{Mono, find};
use crate::{LayoutOptions, NoImages, layout};
use folio_format::css::computed::{ComputedStyle, cascade};
use folio_format::css::{Origin, RuleSet};
use folio_format::dom::Dom;

/// 排出那块带边框与内边距的盒子，返回（边框盒，裁剪盒，定位盒）。
fn boxes_of(
    css: &str,
) -> (
    (f32, f32, f32, f32),
    (f32, f32, f32, f32),
    (f32, f32, f32, f32),
) {
    let html = "<div class='a'>甲</div>";
    let dom = Dom::parse(html.as_bytes());
    let styles = cascade(
        &dom,
        &RuleSet::parse(css, Origin::Author),
        &ComputedStyle::default(),
        "",
    );
    let laid = layout(
        &dom,
        &styles,
        find(&dom, "body"),
        LayoutOptions {
            width: 200.0,
            light: false,
            allow_vertical: false,
            page_height: None,
            screen_width: None,
            screen_height: None,
        },
        &Mono,
        &NoImages,
    );
    let seeds = laid.decor_seeds(&styles);
    assert_eq!(seeds.len(), 1, "只有这一块带装饰：{seeds:?}");
    let seed = &seeds[0];
    (
        (seed.x, seed.y, seed.width, seed.height),
        seed.background_rect,
        seed.origin_rect,
    )
}

/// 默认 `border-box`：裁剪盒与边框盒逐字相同；定位盒初值 `padding-box`，缩掉边框。
#[test]
fn background_clip_defaults_to_the_border_box() {
    let css = ".a { border: 4px solid; padding: 10px; background-color: #fff }";
    let (border, rect, origin) = boxes_of(css);
    assert_eq!(rect, border, "默认裁剪盒就是边框盒：{rect:?}");
    assert_eq!(origin.0, border.0 + 4.0, "定位盒初值缩掉边框宽：{origin:?}");
    assert_eq!(origin.2, border.2 - 8.0, "定位盒宽缩掉两边：{origin:?}");
}

/// `padding-box`：四边向内缩掉边框宽。
#[test]
fn background_clip_padding_box_insets_by_the_border() {
    let css = ".a { border: 4px solid; padding: 10px; background-color: #fff; background-clip: padding-box }";
    let (border, rect, _) = boxes_of(css);
    assert_eq!(rect.0, border.0 + 4.0, "左缘缩进边框宽：{rect:?}");
    assert_eq!(rect.2, border.2 - 8.0, "宽缩掉两边：{rect:?}");
}

/// `content-box`：再缩掉内边距。
#[test]
fn background_clip_content_box_insets_by_border_and_padding() {
    let css = ".a { border: 4px solid; padding: 10px; background-color: #fff; background-clip: content-box }";
    let (border, rect, _) = boxes_of(css);
    assert_eq!(rect.0, border.0 + 14.0, "左缘缩进边框 + 内边距：{rect:?}");
    assert_eq!(
        rect.2,
        border.2 - 28.0,
        "宽缩掉两边的边框与内边距：{rect:?}"
    );
}

/// `background-origin: content-box` 只动定位盒，裁剪盒仍按各自的声明走。
#[test]
fn background_origin_insets_independently_of_the_clip_box() {
    let css = ".a { border: 4px solid; padding: 10px; background-color: #fff; \
               background-origin: content-box }";
    let (border, rect, origin) = boxes_of(css);
    assert_eq!(rect, border, "clip 没写就还是边框盒：{rect:?}");
    assert_eq!(
        (origin.0, origin.1),
        (border.0 + 14.0, border.1 + 14.0),
        "定位盒缩进边框 + 内边距：{origin:?}"
    );
    assert_eq!(origin.2, border.2 - 28.0, "定位盒宽缩掉两边：{origin:?}");
}
