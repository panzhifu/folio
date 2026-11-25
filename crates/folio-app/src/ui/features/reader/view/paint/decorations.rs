//! 装饰线、文字阴影与着重号的绘制原语。
//!
//! 从 `paint.rs` 拆出来只为一件事：那一份已经贴着行数上限，而这些函数只吃
//! [`GlyphRun`] 与 shaping 结果，不碰元素树，也不改绘制顺序。

use folio_core::content::format::css::computed::{
    DecorationStyle, EmphasisPosition, FontSmoothing, TextShadow, Transform,
};
use gpui_kit::{
    Bounds, Hsla, LineLayout, Pixels, Point, StrikethroughStyle, TextRenderingMode, UnderlineStyle,
    Window, fill, point, px, size,
};

use super::{GlyphRun, for_each_glyph, shape};

/// `transform: translate(...)` 的绘制位移：只挪墨迹，不改断行与命中。
/// 百分比按 run 自身的宽 / inline box 高折；`scale` 仍只建立层叠上下文，不缩放字形。
pub(super) fn transformed_origin(
    mut origin: Point<Pixels>,
    run: &GlyphRun,
    shaped: &LineLayout,
) -> Point<Pixels> {
    if let Some(Transform::Translate(dx, dy)) = run.transform {
        origin.x += px(dx.resolve(f32::from(shaped.width)));
        origin.y += px(dy.resolve(run.box_height));
    }
    origin
}

/// CSS 的字体平滑档 → gpui 的逐字形渲染模式。`Auto` 保留窗口级默认。
pub(super) fn rendering_mode(smoothing: FontSmoothing) -> TextRenderingMode {
    match smoothing {
        FontSmoothing::Auto => TextRenderingMode::PlatformDefault,
        FontSmoothing::Antialiased => TextRenderingMode::Grayscale,
        FontSmoothing::SubpixelAntialiased => TextRenderingMode::Subpixel,
        FontSmoothing::None => TextRenderingMode::Aliased,
    }
}

/// 两条装饰线的 y，全按 gpui `paint_line` 的式子来，不自定比例。
///
/// 输入是「这一 run 的基线 y、inline box 高、shaping 报的 ascent/descent」。gpui 里
/// 盒顶在 `origin.y`、基线落在 `盒顶 + padding_top + ascent`，其中
/// `padding_top = (line_height − ascent − descent) / 2`；下划线再往下一条
/// `descent × 0.618`（gpui 的 `UNDERLINE_DESCENT_OFFSET_FACTOR`，它把那个数包在公开函数
/// [`gpui::underline_y_offset`] 里），删除线落在 `盒顶 + (ascent/2 + padding_top + ascent) / 2`。
/// 自己猜一个「基线下 2px」那种数，换字号就会露出：线与字之间的缝会跟着字号涨缩，
/// 而 gpui 自己画的那条不会 —— 两条线就会在不同字号的段落里对不齐。
pub(super) fn decoration_y(
    baseline: Pixels,
    box_height: Pixels,
    ascent: Pixels,
    descent: Pixels,
) -> (Pixels, Pixels) {
    let padding_top = (box_height - ascent - descent) / 2.;
    let baseline_offset = padding_top + ascent;
    let top = baseline - baseline_offset;
    let underline = top + baseline_offset + descent * 0.618;
    let strikethrough = top + (ascent * 0.5 + baseline_offset) * 0.5;
    (underline, strikethrough)
}

/// 一层 `text-shadow` 要画几份、各偏移多少、各用多少不透明度（相对文字色的 alpha）。
///
/// 模糊半径没有对应的绘制原语（gpui 只有纯色四边形与字形），所以按半径摊成对角副本：
/// `blur = 1px` 时是中心一份加四份 0.5px 的淡边，这个尺寸下与真高斯在屏上分不出来。
/// 副本数封顶 5 份、半径封顶 3px —— 真书里这个值普遍是 1，作者写大了也只是多画几遍，
/// 不会把一帧画成几十份。
pub(super) fn shadow_copies(shadow: TextShadow) -> Vec<(f32, f32, f32)> {
    if shadow.blur < 0.5 {
        return vec![(shadow.dx, shadow.dy, 1.0)];
    }
    let r = (shadow.blur / 2.0).min(3.0);
    let alpha = 0.6;
    vec![
        (shadow.dx, shadow.dy, alpha),
        (shadow.dx + r, shadow.dy + r, alpha),
        (shadow.dx + r, shadow.dy - r, alpha),
        (shadow.dx - r, shadow.dy + r, alpha),
        (shadow.dx - r, shadow.dy - r, alpha),
    ]
}

/// 把一条 run 的 `text-shadow` 铺到当前的绘制位置（通常是**字形那一层之前**）。
///
/// 逐字「先影子后字」的写法不行：后一个字的影子会压住前一个字的笔画 —— 偏移只有一两 px，
/// 而字形本来就互相压边。所以整条 run 的影子先铺完，字再统一画一遍。
///
/// 调用点必须在 `paint_layer` **外面**，原因见 [`super::GlyphsElement::paint`] 里那段注释：
/// 层里所有精灵共用一个 `order`，场景会按图集 tile 号再排一次，白影可能排到字后面去。
pub(super) fn paint_shadows(window: &mut Window, origin: Point<Pixels>, run: &GlyphRun) {
    if run.text.is_empty() || run.text_shadows.is_empty() {
        return;
    }
    let font_size = px(run.font_size);
    let shaped = match &run.shaped {
        Some(layout) => layout.clone(),
        None => shape(window, run),
    };
    let origin = transformed_origin(origin, run, &shaped);
    for shadow in run.text_shadows.iter() {
        let base =
            crate::ui::foundation::style::run_color(shadow.color, run.color).opacity(run.opacity);
        for (dx, dy, alpha) in shadow_copies(*shadow) {
            let shade = Hsla {
                a: base.a * alpha,
                ..base
            };
            for_each_glyph(run, &shaped, |x, y, shaped_run, glyph| {
                let _ = window.paint_glyph_with_mode(
                    point(origin.x + px(x + dx), origin.y + px(y + dy)),
                    shaped_run.font_id,
                    glyph.id,
                    font_size,
                    shade,
                    rendering_mode(run.font_smoothing),
                );
            });
        }
    }
}

/// 横排的一条装饰线：实线/波浪走 gpui 自带的线原语，双线与点/虚线切成四边形。
///
/// `underline` 用来分下划线与删除线两条：gpui 的 `paint_strikethrough` 没有波浪档，
/// 删除线配 `wavy` 时按实线画（浏览器也不承诺删除线能波浪）。
fn paint_decor_line(
    window: &mut Window,
    at: Point<Pixels>,
    width: Pixels,
    color: Hsla,
    style: DecorationStyle,
    underline: bool,
) {
    let length = f32::from(width);
    if length <= 0.0 {
        return;
    }
    match style {
        DecorationStyle::Solid => {
            if underline {
                window.paint_underline(
                    at,
                    width,
                    &UnderlineStyle {
                        thickness: px(1.),
                        color: Some(color),
                        wavy: false,
                    },
                );
            } else {
                window.paint_strikethrough(
                    at,
                    width,
                    &StrikethroughStyle {
                        thickness: px(1.),
                        color: Some(color),
                    },
                );
            }
        }
        DecorationStyle::Wavy if underline => {
            window.paint_underline(
                at,
                width,
                &UnderlineStyle {
                    thickness: px(1.),
                    color: Some(color),
                    wavy: true,
                },
            );
        }
        DecorationStyle::Wavy => window.paint_strikethrough(
            at,
            width,
            &StrikethroughStyle {
                thickness: px(1.),
                color: Some(color),
            },
        ),
        DecorationStyle::Double => {
            for offset in [0.0, 2.0] {
                window.paint_quad(fill(
                    Bounds::new(point(at.x, at.y + px(offset)), size(px(length), px(1.))),
                    color,
                ));
            }
        }
        DecorationStyle::Dotted | DecorationStyle::Dashed => {
            let (dash, gap): (f32, f32) = if style == DecorationStyle::Dotted {
                (1.0, 2.0)
            } else {
                (3.0, 3.0)
            };
            let mut x = 0.0f32;
            while x < length {
                let len = dash.min(length - x);
                window.paint_quad(fill(
                    Bounds::new(point(at.x + px(x), at.y), size(px(len), px(1.))),
                    color,
                ));
                x += dash + gap;
            }
        }
    }
}

/// `text-emphasis` 着重号：逐字形在基线**上方**（over）或**下方**（under）点一个小方块近似圆点。
///
/// 近似说明（如实记录）：`ShapedGlyph` 不带字形前进量，这里按字符类别估每字中心
/// （CJK ~1em、拉丁/空格 ~0.5em），空白字符不点。`open dot` / `triangle` / `sesame`
/// 这些变体统一画成实心小方块 —— 形状区分留待后续，先保证「着重号出现在该在的位置」。
/// 横竖两轴经 `flow.project_delta` 折成物理位移，竖排不会画到错的一侧。
pub(super) fn paint_emphasis(
    window: &mut Window,
    origin: Point<Pixels>,
    run: &GlyphRun,
    shaped: &LineLayout,
) {
    let fs = run.font_size;
    let color = run.emphasis_color.unwrap_or(run.color).opacity(run.opacity);
    let dot = (fs * 0.16).max(1.0);
    let depth = match run.emphasis_position {
        EmphasisPosition::Over => -fs * 0.85,
        EmphasisPosition::Under => fs * 0.15,
    };
    for_each_glyph(run, shaped, |x, y, _run, glyph| {
        let Some(ch) = run.text.get(glyph.index..).and_then(|s| s.chars().next()) else {
            return;
        };
        if ch.is_whitespace() {
            return;
        }
        let advance = if (ch as u32) >= 0x2e80 { fs } else { fs * 0.5 };
        let (ddx, ddy) = run.flow.project_delta(advance * 0.5, depth);
        let center = point(origin.x + px(x + ddx), origin.y + px(y + ddy));
        window.paint_quad(fill(
            Bounds::new(
                point(center.x - px(dot / 2.0), center.y - px(dot / 2.0)),
                size(px(dot), px(dot)),
            ),
            color,
        ));
    });
}

/// 横排那一支调用的装饰线入口：名字单独一层，`paint_run` 不必知道实线/波浪的分支。
pub(super) fn paint_rule(
    window: &mut Window,
    at: Point<Pixels>,
    width: Pixels,
    color: Hsla,
    style: DecorationStyle,
    underline: bool,
) {
    paint_decor_line(window, at, width, color, style, underline);
}

#[cfg(test)]
mod tests {
    use super::rendering_mode;
    use folio_core::content::format::css::computed::FontSmoothing;
    use gpui_kit::TextRenderingMode;

    #[test]
    fn css_font_smoothing_maps_to_glyph_rendering_mode() {
        assert_eq!(
            rendering_mode(FontSmoothing::Auto),
            TextRenderingMode::PlatformDefault
        );
        assert_eq!(
            rendering_mode(FontSmoothing::Antialiased),
            TextRenderingMode::Grayscale
        );
        assert_eq!(
            rendering_mode(FontSmoothing::SubpixelAntialiased),
            TextRenderingMode::Subpixel
        );
        assert_eq!(
            rendering_mode(FontSmoothing::None),
            TextRenderingMode::Aliased
        );
    }
}
