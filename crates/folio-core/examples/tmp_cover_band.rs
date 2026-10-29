//! 临时探针：真实《基督山伯爵》封面章在核心层排出来多高、背景层怎么摆。
//! 用完即删。
use folio_core::content::document::{BookDocument, Document as _};
use folio_core::layout::{
    FontKey, ImageProvider, LayoutOptions, LineMetrics, Measure, ResolvedImage, layout,
};
use std::path::Path;

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

struct NoImages;

impl ImageProvider for NoImages {
    fn resolve(&self, _src: &str) -> Option<ResolvedImage> {
        None
    }
}

fn build(dir: &Path) -> std::path::PathBuf {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir.join("META-INF")).unwrap();
    std::fs::create_dir_all(dir.join("OEBPS/Text")).unwrap();
    std::fs::create_dir_all(dir.join("OEBPS/Styles")).unwrap();
    std::fs::create_dir_all(dir.join("OEBPS/Images")).unwrap();
    std::fs::write(dir.join("mimetype"), b"application/epub+zip").unwrap();
    std::fs::write(
        dir.join("META-INF/container.xml"),
        br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
    )
    .unwrap();
    std::fs::copy("/tmp/Style0001.css", dir.join("OEBPS/Styles/Style0001.css")).unwrap();
    std::fs::copy("/tmp/fonts.css", dir.join("OEBPS/Styles/fonts.css")).unwrap();
    std::fs::copy(
        "/tmp/cover-phone.png",
        dir.join("OEBPS/Images/cover-phone.png"),
    )
    .unwrap();
    std::fs::write(
        dir.join("OEBPS/Text/cover.xhtml"),
        r#"<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.1//EN"
  "http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd">
<html xmlns="http://www.w3.org/1999/xhtml" xml:lang="zh-CN">
<head>
  <title></title>
  <style>
 html, body {
     height: 100%;
     margin: 0;
     padding: 0;
     overflow: hidden;
}
  </style>
  <link href="../Styles/Style0001.css" type="text/css" rel="stylesheet"/>
  <link href="../Styles/fonts.css" type="text/css" rel="stylesheet"/>
</head>
<body class="cover-page">
     <div class="fm">
<p>&#160;</p>
</div>
<h2 class="none">封面</h2>
</body>
</html>"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("OEBPS/content.opf"),
        r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>基督山伯爵</dc:title>
    <dc:creator>大仲马</dc:creator>
    <dc:language>zh-CN</dc:language>
    <dc:identifier id="id">test</dc:identifier>
    <meta name="cover" content="cover-phone.png"/>
  </metadata>
  <manifest>
    <item id="cover-phone.png" href="Images/cover-phone.png" media-type="image/png"/>
    <item id="css" href="Styles/Style0001.css" media-type="text/css"/>
    <item id="fonts" href="Styles/fonts.css" media-type="text/css"/>
    <item id="cover" href="Text/cover.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="cover"/></spine>
</package>"#,
    )
    .unwrap();
    let zipped = dir.join("cover-band.epub");
    let file = std::fs::File::create(&zipped).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    zip.start_file(
        "mimetype",
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
    )
    .unwrap();
    std::io::Write::write_all(&mut zip, b"application/epub+zip").unwrap();
    for name in [
        "META-INF/container.xml",
        "OEBPS/content.opf",
        "OEBPS/Styles/Style0001.css",
        "OEBPS/Styles/fonts.css",
        "OEBPS/Images/cover-phone.png",
        "OEBPS/Text/cover.xhtml",
    ] {
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut zip, &std::fs::read(dir.join(name)).unwrap()).unwrap();
    }
    zip.finish().unwrap();
    zipped
}

fn main() {
    let dir = Path::new("/tmp/cover-band-repro");
    let epub = build(dir);
    let doc = BookDocument::open(&epub).unwrap();
    let flow = doc.flow().expect("reflow");
    let css = folio_core::content::format::css::RuleSet::parse(
        &flow.css,
        folio_core::content::format::css::Origin::Author,
    );
    println!(
        "flow.css {} 字节，含 .cover-page: {}",
        flow.css.len(),
        flow.css.contains(".cover-page")
    );
    let text = doc.chapter_source(0).unwrap();
    let dom = folio_core::content::dom::Dom::parse(text.as_bytes());
    let styles =
        folio_core::content::format::css::computed::cascade(&dom, &css, &Default::default(), "");
    for (id, _) in dom.elements() {
        println!(
            "element {:?} class={:?} style={:?}",
            dom.tag_name(id),
            dom.attr(id, "class"),
            dom.attr(id, "style")
        );
    }
    let root = dom
        .elements()
        .find(|(id, _)| dom.tag_name(*id) == Some("body"))
        .map(|(id, _)| id)
        .unwrap();
    for (page_w, page_h) in [(384.0_f32, 720.0_f32), (1080.0, 2000.0)] {
        let laid = layout(
            &dom,
            &styles,
            root,
            LayoutOptions {
                width: page_w,
                light: false,
                allow_vertical: false,
                page_height: Some(page_h),
                screen_width: Some(page_w),
                screen_height: Some(page_h),
            },
            &StdMeasure,
            &NoImages,
        );
        let root_block = &laid.blocks[0];
        let root_style = styles.get(root).unwrap();
        println!(
            "viewport {page_w}×{page_h}: layout.height={} root box={}×{} height={:?} min_height={:?} bg_layers={}",
            laid.height,
            root_block.width,
            root_block.height,
            root_style.height,
            root_style.min_height,
            root_style.background_layers.len()
        );
        for p in &root_style.background_layers {
            println!(
                "  bg href={:?} size={:?} attachment={:?}",
                p.image, p.size, p.attachment
            );
        }
        // 按 CSS `cover` 与实际盒高算：一张 1080×2400 的图铺进 版心×root_box 高 的盒
        let box_w = root_block.width;
        let box_h = root_block.height;
        let (iw, ih) = (1080.0_f32, 2400.0_f32);
        let cover = (box_w / iw).max(box_h / ih);
        println!(
            "  cover 后图尺寸 {:.1}×{:.1}，盒 {:.1}×{:.1} → 可见纵向 {:.0}px / 整图 {:.0}px",
            iw * cover,
            ih * cover,
            box_w,
            box_h,
            box_h,
            ih * cover
        );
    }
}
