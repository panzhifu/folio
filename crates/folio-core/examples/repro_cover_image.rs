//! 临时复现：真机（Meizu 20）上《斗战狂潮》封面章整页空白，封面图画不出来。
//!
//! 用**数据库里那份真实的封面 XHTML**（含 `<table><tr><td>` 包裹的行内 `<img>` 与
//! `float`/`height:50%` 的章节 CSS）造一个迷你 EPUB，在核心层走完
//! `open → images() → image_bytes → layout → fragments` 全链，看 `ImageRun`
//! 在整章排版与分片两个阶段是否存活。
use folio_core::content::document::{BookDocument, Document as _};
use folio_core::content::format::{ContentKind, dir_of, resolve_href};
use folio_core::layout::fragment::{FragmentOptions, fragments};
use folio_core::layout::{
    FontKey, ImageProvider, LayoutOptions, LineFragment, LineMetrics, Measure, ResolvedImage,
    layout,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// 每字 1em 的假度量（与 repro_sapiens 同款）——图片路径不依赖字体度量。
struct StdMeasure;

impl Measure for StdMeasure {
    fn char_width(&self, _ch: char, _f: &FontKey, s: f32) -> f32 {
        s
    }
    fn run_metrics(&self, text: &str, _f: &FontKey, s: f32) -> LineMetrics {
        LineMetrics {
            width: text.chars().count() as f32 * s,
            ascent: s * 0.8,
            descent: s * 0.2,
        }
    }
}

/// 模仿 app 侧 `ChapterImages`：按章目录解析相对路径后查表。
struct TableImages {
    entries: HashMap<String, (f32, f32)>,
    base_dir: String,
}

impl ImageProvider for TableImages {
    fn resolve(&self, src: &str) -> Option<ResolvedImage> {
        let href = resolve_href(&self.base_dir, src);
        let (w, h) = self.entries.get(&href)?;
        Some(ResolvedImage {
            href,
            width: *w,
            height: *h,
        })
    }
}

/// 数据库里第 1 本、第 0 章的原文（封面页）。
const COVER_HTML: &str = r#"<?xml version="1.0" encoding="utf-8" ?>
<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.1//EN" "http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd">
<html xmlns="http://www.w3.org/1999/xhtml" xml:lang="zh-CN">
<head>
<meta http-equiv="Content-Type" content="application/xhtml+xml; charset=utf-8" />
<meta name="generator" content="EasyPub v1.50" />
<title>
Cover
</title>
<style type="text/css">
html,body {height:100%; margin:0; padding:0;}
.wedge {float:left; height:50%; margin-bottom: -360px;}
.container {clear:both; height:0em; position: relative;}
table, tr, th {height: 720px; width:100%; text-align:center;}
</style>
<link rel="stylesheet" href="style.css" type="text/css"/>
</head>
<body>
<div class="wedge"></div>
<div class="container">
<table><tr><td>
<img src="cover.jpg" alt="《斗战狂潮》（校对版全本）作者：骷髅精灵" />
</td></tr></table>
</div>
</body>
</html>"#;

fn build_epub(dir: &Path) -> std::path::PathBuf {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir.join("META-INF")).unwrap();
    std::fs::create_dir_all(dir.join("OEBPS")).unwrap();
    std::fs::write(dir.join("mimetype"), b"application/epub+zip").unwrap();
    std::fs::write(
        dir.join("META-INF/container.xml"),
        br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
    )
    .unwrap();
    std::fs::write(dir.join("OEBPS/cover.html"), COVER_HTML).unwrap();
    std::fs::write(dir.join("OEBPS/style.css"), b"").unwrap();
    // 一张真的 538×720 JPEG（与库里那条 images 行同尺寸）。
    let img = image::RgbaImage::from_pixel(538, 720, image::Rgba([12, 34, 56, 255]));
    image::DynamicImage::ImageRgba8(img)
        .to_rgb8()
        .save(dir.join("OEBPS/cover.jpg"))
        .unwrap();
    std::fs::write(
        dir.join("OEBPS/content.opf"),
        r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>斗战狂潮</dc:title>
    <dc:creator>骷髅精灵</dc:creator>
    <dc:language>zh-CN</dc:language>
    <dc:identifier id="id">test</dc:identifier>
  </metadata>
  <manifest>
    <item id="cover-image" href="cover.jpg" media-type="image/jpeg"/>
    <item id="css" href="style.css" media-type="text/css"/>
    <item id="cover" href="cover.html" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="cover"/></spine>
</package>"#,
    )
    .unwrap();
    let zipped = dir.join("repro.epub");
    let _ = std::fs::remove_file(&zipped);
    let file = std::fs::File::create(&zipped).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    // mimetype 必须第一条且不压缩，是 EPUB 的约定。
    zip.start_file(
        "mimetype",
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
    )
    .unwrap();
    std::io::Write::write_all(&mut zip, b"application/epub+zip").unwrap();
    for name in [
        "META-INF/container.xml",
        "OEBPS/content.opf",
        "OEBPS/style.css",
        "OEBPS/cover.jpg",
        "OEBPS/cover.html",
    ] {
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut zip, &std::fs::read(dir.join(name)).unwrap()).unwrap();
    }
    zip.finish().unwrap();
    zipped
}

fn main() {
    let dir = std::path::Path::new("/tmp/cover-repro");
    let epub = build_epub(dir);

    // ① 打开 → 图片元数据
    let doc = BookDocument::open(&epub).unwrap();
    let images = doc.images();
    println!(
        "① images() = {:?}",
        images.iter().map(|i| &i.href).collect::<Vec<_>>()
    );

    // ② 字节按需现取
    let bytes = doc.image_bytes("OEBPS/cover.jpg");
    println!(
        "② image_bytes(OEBPS/cover.jpg) = {} 字节",
        bytes.as_ref().map_or(0, Vec::len)
    );

    // ③ 与 Book::new 相同的拆法：flow + provider + 章目录基准
    let flow = doc.flow().expect("EPUB 应是可重排");
    let chapter = &flow.chapters[0];
    let provider = TableImages {
        entries: images
            .iter()
            .map(|i| (i.href.clone(), (i.width as f32, i.height as f32)))
            .collect(),
        base_dir: dir_of(&chapter.spine_href),
    };
    println!(
        "③ spine_href = {:?}  base_dir = {:?}",
        chapter.spine_href, provider.base_dir
    );

    // ④ 层叠 + 完整排版（StdMeasure 假度量）
    let css = folio_core::content::format::css::RuleSet::parse(
        &flow.css,
        folio_core::content::format::css::Origin::Author,
    );
    // `flow()` 是**索引**（正文按位置回容器取），`chapter.source()` 在这里是空的；
    // 排版要的是真正那一章的字节。
    let text = doc.chapter_source(0).expect("封面章正文应能取回");
    let dom = folio_core::content::dom::Dom::parse(text.as_bytes());
    let styles =
        folio_core::content::format::css::computed::cascade(&dom, &css, &Default::default(), "");
    let root = dom
        .elements()
        .find(|(id, _)| dom.tag_name(*id) == Some("body"))
        .map(|(id, _)| id)
        .unwrap_or(dom.document);
    let laid = layout(
        &dom,
        &styles,
        root,
        LayoutOptions {
            width: 370.0,
            light: false,
            allow_vertical: false,
            page_height: None,
        },
        &StdMeasure,
        &provider,
    );
    let mut image_runs = 0;
    for (ix, line) in laid.lines.iter().enumerate() {
        for frag in &line.fragments {
            if let LineFragment::Image(img) = frag {
                image_runs += 1;
                println!(
                    "④ 行 {ix}: Image href={} {}×{} @y={}",
                    img.href, img.width, img.height, line.y
                );
            }
        }
    }
    if image_runs == 0 {
        println!("④ 整章排版没有产出任何 ImageRun —— 图片在排版层就丢了");
    }

    // ⑤ 分片（分页模式）之后图片是否还在
    let all = fragments(
        &laid,
        &styles,
        FragmentOptions {
            height: 640.0,
            ..Default::default()
        },
    );
    let in_slices: usize = all
        .iter()
        .map(|f| {
            f.lines
                .clone()
                .filter(|ix| {
                    laid.lines[*ix]
                        .fragments
                        .iter()
                        .any(|fr| matches!(fr, LineFragment::Image(_)))
                })
                .count()
        })
        .sum();
    println!(
        "⑤ 分片 {} 片，带图片的行 {} 行（排版层 image_runs = {image_runs}）",
        all.len(),
        in_slices
    );
    let _ = Arc::new(&doc);
    let _ = ContentKind::Reflow;
}
