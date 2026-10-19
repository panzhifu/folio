//! href 的 fragment 拆分、目录条目在 SQLite 那一列上的往返，以及分章位置的存法。

use super::{
    Chapter, ChapterLocator, ChapterText, Format, HeadDims, TocEntry, fragment_of,
    image_dims_from_head, locator_for, shrink_image,
};

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

/// 竖版与不成整除的比例都要守住：**长边正好到上限、宽高比不变**。
///
/// 这条不是给「Triangle 换成 `thumbnail`」准备的回归断言 —— 两种滤波在这几种形状上
/// 出来的像素尺寸本来就一样（实测 1535×2200 都是 357×512），换的是清晰度与耗时。
/// 它守的是这一层对封面形状的真正承诺：横的竖的都不许被拉变形。
#[test]
fn the_shrunk_cover_hits_the_cap_on_its_long_edge_and_keeps_the_ratio() {
    for (src, cap) in [
        ((1024u32, 512u32), 256u32),
        ((512, 1024), 256),
        ((333, 777), 512),
        ((400, 600), 200),
    ] {
        let (small, mime) = shrink_image(&noise_png(src.0, src.1), cap)
            .unwrap_or_else(|| panic!("{src:?} 缩到长边 {cap} 该有产出"));
        let decoded = image::load_from_memory(&small).expect("缩过的字节要还能解");
        let (w, h) = (decoded.width(), decoded.height());
        assert_eq!(w.max(h), cap, "{src:?} 的长边该正好是 {cap}，实际 {w}×{h}");
        let want = src.0 as f64 / src.1 as f64;
        let got = w as f64 / h as f64;
        assert!(
            (got - want).abs() / want < 0.02,
            "{src:?} 缩完比例该仍是 {want:.3}，实际 {got:.3}"
        );
        assert_eq!(mime, "image/jpeg");
    }
}

/// 一张压不出多少的图：纯色 PNG 缩完可能比原字节还小得离谱，那条「重编反而更大就退回
/// 原图」的分支会抢先命中，量的就不是缩放本身了。
fn noise_png(width: u32, height: u32) -> Vec<u8> {
    use image::ImageEncoder as _;
    let img = image::RgbaImage::from_fn(width, height, |x, y| {
        image::Rgba([
            ((x * 7 + y * 13) % 256) as u8,
            (((x * 37) ^ (y * 91)) % 256) as u8,
            (((x + y) * 3) % 256) as u8,
            255,
        ])
    });
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(img.as_raw(), width, height, image::ExtendedColorType::Rgba8)
        .expect("合成 PNG 应能编码");
    out
}

/// 只喂前缀时，「没读出尺寸」必须分得清是**哪一种**：这两种的下一步完全相反。
///
/// 这条断的是 [`HeadDims`] 的三档分类本身。EPUB 的插图清单按它决定要不要多读一整条
/// （见 `epub::parse_image`），分错一档就是白花一次解压，或者把能定尺寸的图记成 0×0。
#[test]
fn a_short_buffer_tells_whether_reading_more_would_buy_anything() {
    let whole = png(64, 32);
    // IHDR 本身在第 8-33 字节，但解码器要多读一点才肯认这段头（实测 48 字节起才行）：
    // 只有头、不够解码器起步的字节，仍然是「再读才有意义」。
    match image_dims_from_head(&whole[..33]) {
        HeadDims::NeedsMore => {}
        other => panic!("只有 IHDR 的前缀该是 NeedsMore，实际 {other:?}"),
    }
    match image_dims_from_head(&whole[..48]) {
        HeadDims::Sized(dims) => assert_eq!(dims, (64, 32), "头齐的前缀就该定出尺寸"),
        other => panic!("48 字节的前缀该是 Sized，实际 {other:?}"),
    }
    // 整条在手也是同一个答案。
    match image_dims_from_head(&whole) {
        HeadDims::Sized(dims) => assert_eq!(dims, (64, 32)),
        other => panic!("完整字节该是 Sized，实际 {other:?}"),
    }
    // 不是位图：SVG 与非图像字节，读完整条也拿不到尺寸，别再为它解压。
    let svg = br#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg"/>"#;
    for not_a_bitmap in [&svg[..], b"not an image"] {
        match image_dims_from_head(not_a_bitmap) {
            HeadDims::NotABitmap => {}
            other => panic!("{not_a_bitmap:?} 该是 NotABitmap，实际 {other:?}"),
        }
    }
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

/// 名单式容器按**条目名**存位置：EPUB 的 spine 项与 CHM 的 topic 条目同一条规则。
/// 写歪的那一档是 `Inline("")` —— 症状是「书在架上而每一章空白」，且一路不报错。
///
/// 这里用 `Chapter` 的手搓构造，因为 provider 的正常路径（`Chapter::entry`）已经把
/// locator 填好了、`locator_for` 会原样保留它 —— 这一格测的是**规则本身**。
#[test]
fn list_style_containers_are_located_by_entry_name_including_chm() {
    let with_href = Chapter {
        title: "第一章".into(),
        spine_href: "c1.htm".into(),
        text: ChapterText::Inline(String::new()),
        char_len: None,
        images: Vec::new(),
    };
    for format in [Format::Epub, Format::Chm] {
        assert_eq!(
            locator_for(format, &with_href, 0),
            ChapterLocator::Entry("c1.htm".into()),
            "{format:?} 的章要按条目名存位置"
        );
    }
    // 没有条目名的（单文件那一族）按序号存。
    let bare = Chapter {
        spine_href: String::new(),
        ..with_href.clone()
    };
    assert_eq!(
        locator_for(Format::Text, &bare, 2),
        ChapterLocator::Section(2),
        "单文件按第几章存"
    );
    // 带了条目名却撞上没有条目名那一档的格式：只能说正文在手上，不许凭空编一个序号。
    assert!(
        matches!(
            locator_for(Format::Chm, &bare, 2),
            ChapterLocator::Inline(_)
        ),
        "没有条目名时 CHM 退到 Inline —— 正常路径走不到这一档"
    );
}

/// 一本书递给系统那侧时，收件应用是按 media type 筛的：九种格式各报一个说得清的书的类型。
///
/// 钉的是两条会静默坏掉的形状 —— 空串（Android 的 `GpuiSystemUi.shareImage` 把空 mime 退成
/// `image/*`，一本 `.epub` 顶着它出去，弹出来的是整排相册），以及 `application/` 那种
/// 副型空白的半截串（写进 intent-filter 没人认领）。真正的漏项由 `Format::mime` 那个
/// 不写 `_ =>` 的 match 兜：加一种格式而忘了给它类型，编译就过不去。
#[test]
fn every_format_reports_a_book_media_type() {
    for format in [
        Format::Markdown,
        Format::Text,
        Format::Epub,
        Format::Fb2,
        Format::Html,
        Format::Cbz,
        Format::Rtf,
        Format::Chm,
        Format::Mobi,
    ] {
        let mime = format.mime();
        let (top, sub) = mime
            .split_once('/')
            .unwrap_or_else(|| panic!("{format:?} 报的不是 type/subtype：{mime:?}"));
        assert!(!top.is_empty() && !sub.is_empty(), "{format:?} → {mime:?}");
        assert_ne!(top, "image", "{format:?} 不能报成图片：{mime:?}");
    }
    // 两条抽出来钉死：EPUB 是 IANA 注册过的那个，MOBI 是各家文件管理器都在认的那个。
    assert_eq!(Format::Epub.mime(), "application/epub+zip");
    assert_eq!(Format::Mobi.mime(), "application/x-mobipocket-ebook");
}
