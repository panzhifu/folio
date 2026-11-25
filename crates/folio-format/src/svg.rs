//! SVG 栅格化：把矢量图（SVG 封面、逐页 SVG 漫画、正文里内联的 `<img src="x.svg">`）
//! 变成一张位图。
//!
//! 为什么要这一层：`image` crate 不认 SVG，`.svg` 的字节头也不是任何位图魔数 —— 在它
//! 之前，一本用 SVG 做封面或整页漫画的书，要么走 EPUB3 的 `<item fallback="…">` 换一份
//! 位图，要么在屏幕上就是一块空白（`ui::shared::images` 那条会退回 `ImageFormat::Png`，
//! 解不动、静默不画）。中文出版书与漫画包里 SVG 是真实存在的一类资源，所以补上。
//!
//! ## 为什么是 resvg，且为什么不是新依赖树
//!
//! `resvg` **已经在依赖图里且本来就在编译** —— `gpui-pre → gpui-base` 用它把图标
//! SVG 画成位图（`cargo tree -i resvg@0.46.0` 可验）。这里只是把图上已有的一份转成
//! `folio-core` 的直接依赖，与 `flate2` / `sha2` / `libchm` 完全相同的一条口径。
//! `resvg::usvg` 与 `resvg::tiny_skia` 都经它再导出，所以**不需要**再直接依赖
//! `usvg` / `tiny-skia` / `fontdb`。
//!
//! ## 三条降级口径
//!
//! **一、字体不扫系统。** `usvg::Options::default()` 的 fontdb 是**空的**，我们不填它：
//! Android 上没有 fontconfig，而「同一份 SVG 因设备系统字体不同画出不同东西」比缺字形
//! 更糟。SVG 里的文字要么自带字体（书内嵌字体在别处注册），要么画不出 —— 与 KOReader
//! 同一条降级路子。
//!
//! **二、认不出就是 `None`，不猜。** 非 SVG 字节、XML 坏、尺寸为 0 —— 一律 `None`，
//! 调用方按「这块地方空着」处理（与图片缺失同一立场）。**绝不**退回按 PNG 试一把：
//! 那会把一段假字节喂给解码器，得到的是一块和「真没有」长得一样的空白。
//!
//! **三、长边封顶。** 栅格化的像素进 gpui 的 sprite atlas，单张纹理上限是设备的
//! `max_texture_dimension_2d`（见 `ui::shared::images::MAX_IMAGE_EDGE`）。SVG 一缩放
//! 就能声明出任意大的画布，所以这里自己先按 [`MAX_EDGE`] 夹住，别把超大像素丢给 atlas。

/// 栅格化的长边上限。与 `ui::shared::images::MAX_IMAGE_EDGE` 同档：那是能安全进
/// sprite atlas 的经验值，这里保持同一个数，免得同一张 SVG 在两条路上得到两种尺寸。
const MAX_EDGE: u32 = 2048;

use crate::css::computed::{ComputedStyle, StyledDom, SvgLineCap, SvgLineJoin, SvgPaint};
use crate::dom::{Dom, NodeId};

/// 把一段内联 `<svg>` 子树序列化成 `data:image/svg+xml;base64,…` URI。
///
/// 出版书里的矢量花边（如 epub-style-demo 的 `19-border-shadow-notes`）把
/// `fill` / `stroke` 写在 class 的 CSS 里，而 CSS 已经层叠进 `ComputedStyle` ——
/// 所以呈现属性**从样式取**，不读 DOM 上可能并不存在的 presentation attribute。
/// 这样 `fill: none` / `stroke-width: 1.8` 这些刚刚接住的声明才真的画得出来，
/// 而不只是「层叠认得」。
///
/// 只认形状节点（`path` / `line` / `circle` / `rect` / `ellipse` / `polyline` /
/// `polygon`）；含 `<image>` 的 Calibre 封面仍走位图那条路，不在这一层冒充。
pub fn inline_svg_data_uri(dom: &Dom, node: NodeId, styles: &StyledDom) -> Option<String> {
    if !dom.is_element(node, "svg") {
        return None;
    }
    // `<image>` 是位图封面：留给 picture_src 那条路。
    if dom
        .element_children(node)
        .any(|c| dom.is_element(c, "image"))
    {
        return None;
    }
    let mut body = String::new();
    let mut any = false;
    walk_svg(dom, node, styles, &mut body, &mut any, 0);
    if !any {
        return None;
    }
    let mut out = String::from("<svg xmlns=\"http://www.w3.org/2000/svg\"");
    if let Some(view) = dom.attr(node, "viewbox") {
        out.push_str(" viewBox=\"");
        out.push_str(&xml_escape(view));
        out.push('"');
    }
    if let Some(par) = dom.attr(node, "preserveaspectratio") {
        out.push_str(" preserveAspectRatio=\"");
        out.push_str(&xml_escape(par));
        out.push('"');
    }
    out.push_str(" width=\"100%\" height=\"100%\">");
    out.push_str(&body);
    out.push_str("</svg>");
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(out.as_bytes());
    Some(format!("data:image/svg+xml;base64,{encoded}"))
}

/// 递归收集一个 `<svg>` 子树里的形状节点。属性的**几何**部分照抄 DOM（作者写在
/// 标签上的 `d` / `cx` 等），**呈现**部分由 `ComputedStyle` 决定（见
/// [`inline_svg_data_uri`]）。深度上限比照 DOM 的一般上限收一点，防病态输入。
fn walk_svg(
    dom: &Dom,
    node: NodeId,
    styles: &StyledDom,
    out: &mut String,
    any: &mut bool,
    depth: usize,
) {
    if depth > 64 {
        return;
    }
    let Some(tag) = dom.tag_name(node) else {
        return;
    };
    const SHAPES: &[&str] = &[
        "path", "line", "circle", "rect", "ellipse", "polyline", "polygon",
    ];
    if SHAPES.contains(&tag) {
        // 取一份克隆而不是借：`styles.get` 没有的节点（很少见）要用默认值，
        // 而 `&ComputedStyle::default()` 的临时活不到本函数末尾。
        let style = styles
            .get(node)
            .cloned()
            .unwrap_or_else(ComputedStyle::default);
        out.push('<');
        out.push_str(tag);
        // 几何属性：只抄形状真正用的那几个，避免把 class/id 这类无关属性带过去。
        const GEOMETRY: &[&str] = &[
            "d",
            "x",
            "y",
            "x1",
            "y1",
            "x2",
            "y2",
            "cx",
            "cy",
            "r",
            "rx",
            "ry",
            "width",
            "height",
            "points",
            "transform",
        ];
        for name in GEOMETRY {
            if let Some(value) = dom.attr(node, name) {
                out.push(' ');
                // DOM 的属性名一律小写；这里把 `viewBox` 那种首字母大写的 SVG 拼写补回来。
                out.push_str(if *name == "viewbox" { "viewBox" } else { name });
                out.push_str("=\"");
                out.push_str(&xml_escape(value));
                out.push('"');
            }
        }
        push_paint(out, "fill", style.svg_fill);
        push_paint(out, "stroke", style.svg_stroke);
        out.push_str(" stroke-width=\"");
        out.push_str(&style.svg_stroke_width.to_string());
        out.push_str("\" stroke-linecap=\"");
        out.push_str(match style.svg_stroke_linecap {
            SvgLineCap::Round => "round",
            SvgLineCap::Square => "square",
            SvgLineCap::Butt => "butt",
        });
        out.push_str("\" stroke-linejoin=\"");
        out.push_str(match style.svg_stroke_linejoin {
            SvgLineJoin::Round => "round",
            SvgLineJoin::Bevel => "bevel",
            SvgLineJoin::Miter => "miter",
        });
        out.push_str("\"/>");
        *any = true;
    }
    for child in dom.get(node).children.clone() {
        walk_svg(dom, child, styles, out, any, depth + 1);
    }
}

fn push_paint(out: &mut String, name: &str, paint: SvgPaint) {
    out.push(' ');
    out.push_str(name);
    out.push_str("=\"");
    match paint {
        SvgPaint::None => out.push_str("none"),
        SvgPaint::Color(color) if color.3 == 255 => {
            out.push_str(&format!("#{:02x}{:02x}{:02x}", color.0, color.1, color.2));
        }
        SvgPaint::Color(color) => {
            out.push_str(&format!(
                "rgba({},{},{},{})",
                color.0,
                color.1,
                color.2,
                color.3 as f32 / 255.0
            ));
        }
    }
    out.push('"');
}

/// XML 属性 escaping：`&` / `<` / `>` / `"` 都得转，否则 path 的 `d` 里出现
/// `&` 会让 usvg 把整棵 SVG 判成坏 XML，花边整块消失。
fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod inline_tests {
    use super::{inline_svg_data_uri, rasterize};
    use crate::css::computed::{ComputedStyle, cascade};
    use crate::css::{Origin, RuleSet};
    use crate::dom::Dom;
    use base64::Engine as _;

    /// demo 的花边：<svg> 下两条 <path>，颜色/线宽全写在 class 的 CSS 里。
    /// 序列化后必须把这三条声明带进 SVG 字节，且字节能被现有栅格化路解出位图。
    #[test]
    fn inline_svg_carries_css_paint_and_rasterizes() {
        let dom = Dom::parse(
            b"<div><svg class='art' viewBox='0 0 100 40'>              <path class='main' d='M5 5 L95 5'/>              <path class='line' d='M5 20 L95 20'/></svg></div>",
        );
        let rules = RuleSet::parse(
            ".main { fill: none; stroke: #4f4539; stroke-width: 1.8; stroke-linecap: round }             .line { fill: none; stroke: #4f4539; stroke-width: 1.15 }",
            Origin::Author,
        );
        let styles = cascade(&dom, &rules, &ComputedStyle::default(), "");
        let svg = dom
            .elements()
            .find(|(id, _)| dom.is_element(*id, "svg"))
            .map(|(id, _)| id)
            .expect("应有内联 svg");
        let uri = inline_svg_data_uri(&dom, svg, &styles).expect("应序列出 data URI");
        assert!(uri.starts_with("data:image/svg+xml;base64,"));
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(
                uri.strip_prefix("data:image/svg+xml;base64,")
                    .unwrap()
                    .as_bytes(),
            )
            .unwrap();
        let xml = String::from_utf8(bytes.clone()).unwrap();
        assert!(xml.contains("fill=\"none\""), "fill:none 要进字节：{xml}");
        assert!(
            xml.contains("stroke=\"#4f4539\""),
            "stroke 颜色要进字节：{xml}"
        );
        assert!(xml.contains("stroke-width=\"1.8\""), "第一行的线宽：{xml}");
        assert!(xml.contains("stroke-width=\"1.15\""), "第二行的线宽：{xml}");
        assert!(xml.contains("stroke-linecap=\"round\""), "线端形状：{xml}");
        let (png, mime) = rasterize(&bytes, Some((400, 160))).expect("应能栅格化");
        assert_eq!(mime, "image/png");
        assert!(png.len() > 100, "该画出非空位图");
    }

    #[test]
    fn inline_svg_with_only_bitmap_image_is_not_claimed() {
        let dom = Dom::parse(b"<svg><image href='cover.jpg'/></svg>");
        let styles = cascade(
            &dom,
            &RuleSet::parse("", Origin::Author),
            &ComputedStyle::default(),
            "",
        );
        let svg = dom
            .elements()
            .find(|(id, _)| dom.is_element(*id, "svg"))
            .map(|(id, _)| id)
            .unwrap();
        assert!(
            inline_svg_data_uri(&dom, svg, &styles).is_none(),
            "位图封面不冒充矢量"
        );
    }
}

/// 认一认这段字节是不是 SVG。
///
/// **不解析**，只看头部有没有 `<svg`：`usvg` 对非 XML 字节是要报错的，而这条判据落在
/// 每张图「要不要去解」的热路径上，先挡一道省一次白解。空白、BOM、XML 声明、注释都
/// 允许在 `<svg` 之前（`usvg` 自己会处理），所以这里只在开头这一小段里找子串，且
/// 大小写不敏感 —— `<?xml` 与 `<SVG` 都真的见过。
pub fn looks_like_svg(bytes: &[u8]) -> bool {
    // 声明 + 注释一般几十字节，读 1 KiB 足够；UTF-16 的 SVG 不在支持范围（解不出就 None）。
    let head = &bytes[..bytes.len().min(1024)];
    let text = String::from_utf8_lossy(head);
    let lower = text.to_ascii_lowercase();
    lower.contains("<svg")
}

/// SVG 的**固有尺寸**（逻辑像素，按 96 dpi 换算），读不到时 `None`。
///
/// 给图片元数据用（`ImageRes.width/height`）：排版期要按它定占位，而排版是同步的，
/// 不可能等到真要画时再栅格一次。
pub fn intrinsic_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() > crate::limits::MAX_DOM_BYTES || !looks_like_svg(bytes) {
        return None;
    }
    let tree = resvg::usvg::Tree::from_data(bytes, &resvg::usvg::Options::default()).ok()?;
    let size = tree.size();
    let (w, h) = (size.width(), size.height());
    round_dims(w, h)
}

/// 把一份 SVG 栅格化成 PNG 字节。成功返回 `(png 字节, "image/png")`。
///
/// `target` 是「最多画多大」的方框（宽, 高）：
/// - `Some((w, h))`：等比缩进这个框（`fill` 到框内最大）。SVG 是矢量的，**放大小图也画**，
///   所以漫画整页给视口尺寸、封面给缩略图上限都由它一次到位；
/// - `None`：按 SVG 固有尺寸。
///
/// 无论哪一档，结果的长边都不会超过 [`MAX_EDGE`]。
pub fn rasterize(bytes: &[u8], target: Option<(u32, u32)>) -> Option<(Vec<u8>, &'static str)> {
    if bytes.len() > crate::limits::MAX_DOM_BYTES || !looks_like_svg(bytes) {
        return None;
    }
    let tree = resvg::usvg::Tree::from_data(bytes, &resvg::usvg::Options::default()).ok()?;
    let size = tree.size();
    let (sw, sh) = (size.width(), size.height());
    let (width, height) = target_dims(sw, sh, target)?;

    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)?;
    let transform = resvg::tiny_skia::Transform::from_scale(width as f32 / sw, height as f32 / sh);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    // `encode_png` 处理了 tiny-skia 的预乘 alpha，别自己去动 RGBA 字节（那会把透明区
    // 重新乘一遍）。`png-format` feature 随 tiny-skia 默认开着。
    let png = pixmap.encode_png().ok()?;
    Some((png, "image/png"))
}

/// 固有尺寸 → 像素尺寸，含 `NaN`/非正/超大的一律当「读不出」。
fn round_dims(w: f32, h: f32) -> Option<(u32, u32)> {
    if !(w.is_finite() && h.is_finite()) || w <= 0.0 || h <= 0.0 {
        return None;
    }
    let (w, h) = clamp_longest(w, h);
    Some((w.ceil().max(1.0) as u32, h.ceil().max(1.0) as u32))
}

/// 目标方框 + 固有尺寸 → 最终像素尺寸（长边封顶）。
fn target_dims(sw: f32, sh: f32, target: Option<(u32, u32)>) -> Option<(u32, u32)> {
    if !(sw.is_finite() && sh.is_finite()) || sw <= 0.0 || sh <= 0.0 {
        return None;
    }
    let (w, h) = match target {
        Some((tw, th)) if tw > 0 && th > 0 => {
            // 等比缩进框内。矢量图放大小图也画，所以不另设「不许放大」。
            let scale = (tw as f32 / sw).min(th as f32 / sh);
            let scale = if scale.is_finite() && scale > 0.0 {
                scale
            } else {
                return None;
            };
            (sw * scale, sh * scale)
        }
        _ => (sw, sh),
    };
    let (w, h) = clamp_longest(w, h);
    // 极小尺寸（scale 算出来小于一个像素）也至少给 1×1，别造出 0 宽的 pixmap。
    Some((w.ceil().max(1.0) as u32, h.ceil().max(1.0) as u32))
}

/// 把 (w, h) 等比缩到长边不超过 [`MAX_EDGE`]。
fn clamp_longest(w: f32, h: f32) -> (f32, f32) {
    let longest = w.max(h);
    if longest > MAX_EDGE as f32 {
        let s = MAX_EDGE as f32 / longest;
        (w * s, h * s)
    } else {
        (w, h)
    }
}

/// 一份能在一帧里画完的小 SVG。测试与调用方都能拿它当「确实栅格得出东西」的样本。
pub fn sample_svg(width: u32, height: u32) -> Vec<u8> {
    format!(
        r##"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">
  <rect width="{width}" height="{height}" fill="#3366cc"/>
  <circle cx="{cx}" cy="{cy}" r="{r}" fill="#ffffff"/>
</svg>"##,
        cx = width as f32 / 2.0,
        cy = height as f32 / 2.0,
        r = (width.min(height) as f32 / 4.0).max(1.0),
    )
    .into_bytes()
}

/// 一张位图（PNG）字节，用来验「非 SVG 一律 `None`」。
#[cfg(test)]
fn sample_png() -> Vec<u8> {
    use image::ImageEncoder as _;
    let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([1, 2, 3, 255]));
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(img.as_raw(), 4, 4, image::ExtendedColorType::Rgba8)
        .expect("合成 PNG 应能编码");
    out
}

#[cfg(test)]
mod tests {
    //! 断言只围绕一件事：**栅格化真的产出一张解得开的位图，且尺寸是设计的那一个**。
    //! 用「自己画自己解」当证据，等于证明像素没丢；尺寸则直接量解出来的纵横比。

    use super::{MAX_EDGE, intrinsic_size, looks_like_svg, rasterize, sample_png, sample_svg};

    fn decoded(bytes: &[u8]) -> (u32, u32) {
        let img = image::load_from_memory(bytes).expect("栅格化产物该是一张解得开的位图");
        (img.width(), img.height())
    }

    #[test]
    fn only_svg_bytes_are_claimed() {
        assert!(looks_like_svg(&sample_svg(10, 10)));
        // 前导空白与大小写都不影响判定。
        assert!(looks_like_svg(b"  \n<SVG width='1'/>"));
        assert!(!looks_like_svg(&sample_png()));
        assert!(!looks_like_svg(b"<html><body>not svg</body></html>"));
        assert!(!looks_like_svg(b""));
    }

    #[test]
    fn rasterize_produces_a_decodable_bitmap_at_the_intrinsic_size() {
        let svg = sample_svg(120, 80);
        let (png, mime) = rasterize(&svg, None).expect("合法 SVG 该能栅格化");
        assert_eq!(mime, "image/png");
        assert_eq!(decoded(&png), (120, 80), "无目标框时用固有尺寸");
    }

    #[test]
    fn a_target_box_fits_the_ratio_and_is_never_upscaled_past_the_cap() {
        let svg = sample_svg(1000, 500);
        // 缩进 200×200 的框：等比到 200×100。
        let (png, _) = rasterize(&svg, Some((200, 200))).unwrap();
        assert_eq!(decoded(&png), (200, 100));

        // 目标框大过长边上限：夹到 MAX_EDGE，而不是照 10000 画。
        let (png, _) = rasterize(&svg, Some((10_000, 10_000))).unwrap();
        let (w, h) = decoded(&png);
        assert_eq!(w, MAX_EDGE, "长边要夹到上限");
        assert_eq!(w, h * 2, "纵横比要保住");
    }

    #[test]
    fn intrinsic_size_reads_the_declared_dimensions() {
        assert_eq!(intrinsic_size(&sample_svg(64, 32)), Some((64, 32)));
        assert_eq!(intrinsic_size(&sample_png()), None);
        assert_eq!(intrinsic_size(b"not svg at all"), None);
    }

    #[test]
    fn malformed_and_non_svg_bytes_are_none_not_a_guess() {
        assert!(
            rasterize(&sample_png(), None).is_none(),
            "位图不该走 SVG 那条"
        );
        // 有 `<svg` 但 XML 坏（未闭合且非法）：usvg 解不开 → None。
        assert!(rasterize(b"<svg><rect", None).is_none());
        // 宽高为 0 是「读不出尺寸」，不该造出 0×0 的 pixmap。
        assert!(rasterize(b"<svg width='0' height='0'/>", None).is_none());
    }
}
