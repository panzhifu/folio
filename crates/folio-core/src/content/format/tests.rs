//! href 的 fragment 拆分，以及目录条目在 SQLite 那一列上的往返。

use super::{TocEntry, fragment_of, shrink_image};

/// 造一张真位图 —— `shrink_image` 要真的解码+重编，够 header 的假字节过不了。
fn png(width: u32, height: u32) -> Vec<u8> {
    use image::ImageEncoder as _;
    let img = image::RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255]));
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(img.as_raw(), width, height, image::ExtendedColorType::Rgba8)
        .expect("合成 PNG 应能编码");
    out
}

/// 大图缩下来、小图原样：这是手机上图片能不能上屏的那一条。
#[test]
fn covers_are_bounded_and_re_encoded_as_jpeg() {
    // 解不出来的字节不动：不能把「不是图」当成「要缩的图」。
    assert!(shrink_image(b"not an image", 256).is_none());
    // 0 是「不设上限」的哨兵，不能被当成要缩到 0。
    assert!(shrink_image(&png(100, 80), 0).is_none());
    // 无字可压缩的极小图：重编反而更大，退回原图（`None` = 用原字节）。
    assert!(shrink_image(&png(2, 2), 256).is_none());

    // 超长边：缩到上限内，纵横比保住，而且**一律 JPEG** —— 带 alpha 的封面
    // 铺白底再编，不然透明区会变黑。
    let (small, mime) = shrink_image(&png(1024, 512), 256).expect("大图该被缩");
    let decoded = image::load_from_memory(&small).expect("缩过的字节要还能解");
    assert_eq!(
        (decoded.width(), decoded.height()),
        (256, 128),
        "纵横比要保住"
    );
    assert_eq!(mime, "image/jpeg");

    // **没超过上限**的也要重编：封面的问题是「按原格式存」，不是「太大」——
    // 一张 800×1200 的 PNG 原样进库就是 400 KB。
    let (_, mime) = shrink_image(&png(400, 600), 512).expect("没过上限也要重编");
    assert_eq!(mime, "image/jpeg");
}

#[test]
fn fragments_are_taken_after_the_first_hash() {
    assert_eq!(
        fragment_of("Text/chap2.xhtml#court").as_deref(),
        Some("court")
    );
    assert_eq!(fragment_of("Text/chap1.xhtml"), None, "没有 # 就是章首");
    assert_eq!(
        fragment_of("Text/chap1.xhtml#"),
        None,
        "空 fragment 也是章首"
    );
    // fragment 是第一个 # 之后的全部：它自己可以含 #（`a#b#c` 的落点是 `b#c`）。
    assert_eq!(fragment_of("a.xhtml#b#c").as_deref(), Some("b#c"));
    assert_eq!(fragment_of("a.xhtml#%20x").as_deref(), Some(" x"));
    // 只有第一段是元素 id，后面的 xpointer 部分丢掉。
    assert_eq!(
        fragment_of("a.xhtml#sec,xpointer()").as_deref(),
        Some("sec")
    );
}

/// 认不出来的两套 fragment 语法都当「没有」，也就是落到章首，而不是跳到一个错的位置。
#[test]
fn unsupported_fragments_read_as_chapter_start() {
    assert_eq!(fragment_of("a.xhtml#epubcfi(/6/8[chap2]!4:12)"), None);
}

/// 库里已有的 `toc_json` 没有 fragment 这一项。解不开旧数据比少一个落点严重得多 ——
/// 那是一整本书的目录凭空消失。
#[test]
fn toc_entries_without_a_fragment_field_still_load() {
    let old = r#"[{"title":"第一章","href":"OEBPS/Text/chap1.xhtml","children":[]}]"#;
    let entries: Vec<TocEntry> = serde_json::from_str(old).expect("旧 toc_json 要能解");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].fragment, None);
    assert_eq!(entries[0].href, "OEBPS/Text/chap1.xhtml");
}
