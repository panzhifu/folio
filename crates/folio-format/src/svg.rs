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
    if !looks_like_svg(bytes) {
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
    if !looks_like_svg(bytes) {
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
