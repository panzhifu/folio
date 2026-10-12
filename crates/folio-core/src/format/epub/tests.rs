//! EPUB 容器读取。测的是一份**合成**的书（`tests/fixtures/oss/minimal-zh.epub`），
//! 故意把 manifest 顺序、spine 顺序、文件名里的空格、nav 里的 fragment
//! 都设成会出错的样子。只依赖公开的 `document::open` / `resolve_href` 等。

use std::path::{Path, PathBuf};

use super::read_with;
use crate::css::computed::{ComputedStyle, TextAlign, cascade};
use crate::css::{Origin, RuleSet};
use crate::dom::Dom;
use crate::format::detect;
use crate::format::{Chapter, Content, DocMeta, FlowDoc, Format, dir_of, resolve_href};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/oss/minimal-zh.epub")
}

/// 打开一个容器，返回**统一接口** —— 测试也走 `document::open` 这条 app 唯一的
/// 「文件 → 文档」入口（`format` 那一层不再有 `open`）。
fn doc_of(path: &Path) -> Box<dyn crate::document::Document> {
    crate::document::open(path).expect("合成容器应能打开")
}

/// 可重排的那一半：绝大多数用例只关心 `FlowDoc`，而且是**正文已经取回来**的那种。
///
/// `document::open` 现在只给索引（库里就存这个，见 `plans/text-out-of-db.md`），
/// 而这一层要断言的是正文本身，所以按位置把每一章取回来。
fn flow_of(path: &Path) -> FlowDoc {
    let doc = doc_of(path);
    let mut flow = doc.flow().expect("这个容器该读成可重排").clone();
    for (ix, chapter) in flow.chapters.iter_mut().enumerate() {
        let text = doc.chapter_source(ix).expect("正文该取得到");
        *chapter = Chapter::inline(chapter.title.clone(), text)
            .with_spine_href(chapter.spine_href.clone());
    }
    flow
}

fn opened() -> (DocMeta, FlowDoc) {
    let doc = doc_of(&fixture());
    let flow = flow_of(&fixture());
    (doc.meta().clone(), flow)
}

#[test]
fn detected_by_extension() {
    assert_eq!(detect(&fixture()), Some(Format::Epub));
    assert_eq!(detect(Path::new("/x/y.epub")), Some(Format::Epub));
    assert_eq!(detect(Path::new("/x/y.EPUB")), Some(Format::Epub));
    assert_eq!(detect(Path::new("/x/y.md")), Some(Format::Markdown));
}

#[test]
fn href_resolution_collapses_dot_segments_and_decodes() {
    assert_eq!(
        resolve_href("OEBPS/Text/", "../Styles/style.css"),
        "OEBPS/Styles/style.css"
    );
    assert_eq!(
        resolve_href("OEBPS/", "./Text/chap1.xhtml#a"),
        "OEBPS/Text/chap1.xhtml"
    );
    assert_eq!(
        resolve_href("OEBPS/Text/", "chap%202.xhtml"),
        "OEBPS/Text/chap 2.xhtml"
    );
    assert_eq!(resolve_href("", "/OEBPS/a.xhtml"), "OEBPS/a.xhtml");
    // 逃出容器的 ../ 只会把栈弹空，不会造出绝对路径。
    assert_eq!(
        resolve_href("OEBPS/", "../../../../etc/passwd"),
        "etc/passwd"
    );
}

#[test]
fn metadata_comes_from_the_opf() {
    let (meta, _) = opened();
    assert_eq!(meta.title, "合成测试书");
    assert_eq!(meta.authors, vec!["莫里亚克", "顾琪静"]);
    assert_eq!(meta.format, Format::Epub);
    assert!(meta.size > 0);
}

/// manifest 里 c3 排在最前，spine 才是阅读顺序。
#[test]
fn chapters_follow_spine_not_manifest() {
    let (_, flow) = opened();
    let hrefs: Vec<&str> = flow
        .chapters
        .iter()
        .map(|c| c.spine_href.as_str())
        .collect();
    assert_eq!(
        hrefs,
        vec![
            "OEBPS/Text/chap1.xhtml",
            "OEBPS/Text/chap 2.xhtml",
            "OEBPS/Text/chap3.xhtml"
        ]
    );
    assert_eq!(flow.chapters[0].title, "第一章 蛇结");
    assert!(flow.chapters[0].source().contains("夜色从花园那头压过来"));
    // nav 文档本身是 spine 里的一节，但它不该变成「一章」。
    assert_eq!(
        flow.chapters
            .iter()
            .filter(|c| c.spine_href.contains("Nav"))
            .count(),
        0
    );
}

/// 两份 CSS 都在 manifest 里：索引记下的是**位置清单**，顺序就是声明顺序；
/// 全文路径才把文本按同一顺序拼好（正文出库之后样式表同样不入库）。
#[test]
fn css_is_concatenated_in_manifest_order() {
    let (_, flow) = opened();
    assert!(flow.css.is_empty(), "索引只给位置清单，不该带样式文本");
    assert_eq!(flow.css_hrefs.len(), 2, "{:?}", flow.css_hrefs);
    assert!(
        flow.css_hrefs[0].ends_with("cover.css"),
        "清单按 manifest 顺序：{:?}",
        flow.css_hrefs
    );
    assert!(
        flow.css_hrefs[1].ends_with("style.css"),
        "{:?}",
        flow.css_hrefs
    );

    let (_, full) = read_with(&fixture(), true).unwrap();
    let Content::Reflow(full) = full else {
        panic!("该读成可重排");
    };
    let extra = full.css.find("body.cover").expect("应有 cover.css 的内容");
    let style = full.css.find("text-indent").expect("应有 style.css 的内容");
    assert!(
        extra < style,
        "cover.css 在 manifest 里先出现，拼接顺序要保住"
    );
}

#[test]
fn toc_hrefs_are_absolute_and_labels_are_trimmed() {
    let (_, flow) = opened();
    assert_eq!(flow.toc.len(), 3, "nav 里三个条目");
    assert_eq!(flow.toc[0].href, "OEBPS/Text/chap1.xhtml");
    assert_eq!(
        flow.toc[1].href, "OEBPS/Text/chap 2.xhtml",
        "fragment 要去掉"
    );
    // 去掉的同时它得留在 `fragment` 里：合成书第二条写的就是 `chap%202.xhtml#court`，
    // 一个章文件挂多个小节是 nav 最常见的形态，丢了这截它们会全并成同一个落点。
    assert_eq!(flow.toc[1].fragment.as_deref(), Some("court"));
    assert_eq!(flow.toc[0].fragment, None, "没有 # 的条目就是章首");
    assert_eq!(
        flow.toc[2].title, "第三章 空白",
        "nav 标签里的多余空格要压掉"
    );
}

/// 中文书的排版地基：这三条样式必须能从合成书的 CSS 里落到元素上。
/// 层叠吃的是**拼好的文本** —— 全文路径（`read`）给的那一份。
#[test]
fn the_books_own_css_reaches_the_elements() {
    let (_, full) = read_with(&fixture(), true).unwrap();
    let Content::Reflow(flow) = full else {
        panic!("该读成可重排");
    };
    let rules = RuleSet::parse(&flow.css, Origin::Author);
    assert_eq!(rules.ignored, 1, "只有 @charset 是清单外的语句型 at-rule");
    let dom = Dom::parse(flow.chapters[0].source().as_bytes());
    let body = dom
        .elements()
        .find(|(_, n)| n.tag.as_ref().map(|t| t.local.to_string()) == Some("body".into()))
        .map(|(id, _)| id)
        .unwrap();
    let styles = cascade(
        &dom,
        &rules,
        &ComputedStyle {
            font_size: 18.0,
            ..Default::default()
        },
    );
    let p = dom
        .elements()
        .find(|(_, n)| n.tag.as_ref().map(|t| t.local.to_string()) == Some("p".into()))
        .map(|(id, _)| id)
        .unwrap();
    assert_eq!(
        styles.get(p).unwrap().text_indent,
        36.0,
        "2em 按 18px 偏好字号"
    );
    let h1 = dom
        .elements()
        .find(|(_, n)| n.tag.as_ref().map(|t| t.local.to_string()) == Some("h1".into()))
        .map(|(id, _)| id)
        .unwrap();
    // f32 乘法：18 × 1.8 = 32.399998，按位相等是测试的错而不是引擎的错。
    assert!(
        (styles.get(h1).unwrap().font_size - 32.4).abs() < 1e-3,
        "h1 = 1.8em"
    );
    assert_eq!(styles.get(h1).unwrap().text_align, TextAlign::Center);
    assert_eq!(
        styles.get(body).unwrap().font_family,
        "serif",
        "@charset 之后的第一条规则要命中 body"
    );
}

#[test]
fn cover_bytes_survive_the_round_trip() {
    let (_, flow) = opened();
    let cover = flow.cover.expect("meta name=cover 指向的 PNG 应被读出来");
    assert_eq!(&cover[..8], b"\x89PNG\r\n\x1a\n");
}

#[test]
fn a_corrupt_container_fails_instead_of_returning_an_empty_book() {
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("bad.epub");
    std::fs::write(&bad, b"not a zip at all").unwrap();
    assert!(
        crate::document::open(&bad).is_err(),
        "读不出 container.xml 必须报错，不能给一本空书"
    );
}

/// 合成一张真实 PNG：`image` 只读 header 就能拿到尺寸，用真字节才测得准。
fn png_bytes(w: u32, h: u32) -> Vec<u8> {
    use image::ImageEncoder as _;
    let img = image::RgbaImage::from_pixel(w, h, image::Rgba([10, 20, 30, 255]));
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgba8)
        .unwrap();
    out
}

fn write_epub(path: &Path, entries: &[(&str, &[u8])]) {
    use std::io::Write as _;
    let file = std::fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default();
    for (name, data) in entries {
        zip.start_file(*name, opts).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap();
}

const CONTAINER: &str = r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;

/// 合成一份只有一章、没有 nav 的 EPUB（用来测标题回落）。
fn synth_epub(dir: &tempfile::TempDir, chapter_xhtml: &str) -> PathBuf {
    let path = dir.path().join("synth.epub");
    let opf = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>无目录</dc:title></metadata>
  <manifest>
    <item id="c1" href="Text/chap1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="c1"/></spine>
</package>"#;
    write_epub(
        &path,
        &[
            ("mimetype", b"application/epub+zip"),
            ("META-INF/container.xml", CONTAINER.as_bytes()),
            ("OEBPS/content.opf", opf.as_bytes()),
            ("OEBPS/Text/chap1.xhtml", chapter_xhtml.as_bytes()),
        ],
    );
    path
}

/// 出版书形状的那一份：28 章、25 条目录、开头带 `@charset` 的一份 CSS，
/// 导航只有 `toc.ncx`（EPUB 2.0，calibre 导出就是这一档）。
///
/// 这些数字是在本机那本不入库的样本里量出来的（`tests/fixtures/README.md`），
/// 但容器本身**当场合成** —— 一条要每次 `cargo test` 与 CI 都真跑的测试，
/// 输入必须在仓库里。它验的不是「能不能打开」，而是合成小样本装不下的那几个形状：
/// manifest 与 spine 反着排、几十条 NCX 条目、条目里的 `#fragment`、章数多于目录数。
fn published_shape_epub(dir: &tempfile::TempDir) -> PathBuf {
    const CHAPTERS: usize = 28;
    const TOC_ENTRIES: usize = 25;
    let path = dir.path().join("published-shape.epub");

    // manifest 倒着写、ncx 与 css 垫底：出版书就是这么排的，而读成 spine 顺序才对。
    let mut manifest = String::new();
    for n in (1..=CHAPTERS).rev() {
        manifest.push_str(&format!(
                "    <item id=\"c{n}\" href=\"Text/chap{n}.xhtml\" media-type=\"application/xhtml+xml\"/>\n"
            ));
    }
    manifest.push_str(
        "    <item id=\"ncx\" href=\"toc.ncx\" media-type=\"application/x-dtbncx+xml\"/>\n",
    );
    manifest.push_str("    <item id=\"css\" href=\"Styles/style.css\" media-type=\"text/css\"/>\n");

    let mut spine = String::from("    <spine toc=\"ncx\">\n");
    for n in 1..=CHAPTERS {
        spine.push_str(&format!("      <itemref idref=\"c{n}\"/>\n"));
    }
    spine.push_str("    </spine>\n");

    let opf = format!(
        r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>合成出版书</dc:title>
    <dc:creator>某人</dc:creator>
  </metadata>
  <manifest>
{manifest}  </manifest>
{spine}</package>"#
    );

    // 元素名按规范的大小写（`navMap` / `navPoint`）：NCX 是大小写敏感的 XML，
    // 全小写的解析器在这份文件上会读出零条目录，而在合成小样本上不一定看得出来。
    // 头两条指着章内锚点，那是出版书目录把「章 + 小节」一起列出来时的写法。
    let mut nav_points = String::new();
    for n in 1..=TOC_ENTRIES {
        let src = match n {
            1 => "Text/chap1.xhtml#opening".to_string(),
            2 => "Text/chap2.xhtml#second".to_string(),
            _ => format!("Text/chap{n}.xhtml"),
        };
        nav_points.push_str(&format!(
                "    <navPoint id=\"p{n}\" playOrder=\"{n}\"><navLabel><text>第{n}章</text></navLabel><content src=\"{src}\"/></navPoint>\n"
            ));
    }
    let ncx = format!(
        r#"<?xml version="1.0"?>
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1">
  <head><meta name="dtb:uid" content="synth-published"/></head>
  <docTitle><text>合成出版书</text></docTitle>
  <navMap>
{nav_points}  </navMap>
</ncx>"#
    );

    // 开头那条 `@charset` 是出版书的标配；它曾是「把后一条规则整条吃掉」的现场
    // （解析侧的回归由 `css` 那一层的测试钉住，这里保住它出现在容器里）。
    let css = "@charset \"utf-8\";\nbody { line-height: 1.8 }\nh1 { text-align: center }\n";

    let mut entries: Vec<(String, Vec<u8>)> = vec![
        ("mimetype".to_string(), b"application/epub+zip".to_vec()),
        (
            "META-INF/container.xml".to_string(),
            CONTAINER.as_bytes().to_vec(),
        ),
        ("OEBPS/content.opf".to_string(), opf.into_bytes()),
        ("OEBPS/toc.ncx".to_string(), ncx.into_bytes()),
        (
            "OEBPS/Styles/style.css".to_string(),
            css.as_bytes().to_vec(),
        ),
    ];
    for n in 1..=CHAPTERS {
        // 前两章把目录点到的那两个锚放下来，其余章没有 —— 目录数少于章数是常态。
        let anchor = match n {
            1 => " id=\"opening\"",
            2 => " id=\"second\"",
            _ => "",
        };
        entries.push((
                format!("OEBPS/Text/chap{n}.xhtml"),
                format!(
                    "<html><head><title>第{n}章</title></head><body><h1>第{n}章</h1><p{anchor}>第{n}章的正文。</p></body></html>"
                )
                    .into_bytes(),
            ));
    }
    let refs: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(name, data)| (name.as_str(), data.as_slice()))
        .collect();
    write_epub(&path, &refs);
    path
}

/// 章标题的回落：目录里没有这一章时，读章内第一个 h1/h2/h3 —— 两趟**给同一个答案**。
///
/// 这里曾经钉的是一个差别：标题回落要读正文才拿得到，而「只读索引」那一趟不读正文
/// （`plans/text-out-of-db.md`），所以索引只能给「第 N 节」，而全文给真标题。
/// 同一个字段两条路两个答案，是漂 —— 症状是「导入时书签里叫『第 3 节』，
/// 打开之后叫『第三章 庭院』」。
///
/// 现在两趟一致了，因为**索引那一趟本来就要读正文**：`chapters.char_len` 是
/// 全局字符进度的分母，必须在导入时算准（`plans/导入管线.md` §八 第 3 条、
/// 已决第 11 条）。正文既然已经在手上，标题回落就是白拿的，没有理由不用它。
///
/// 代价写明白：条目标题不再来自目录就能定，导入要多付一遍解压。
#[test]
fn heading_fallback_gives_the_same_title_on_both_passes() {
    let dir = tempfile::tempdir().unwrap();
    let path = synth_epub(
        &dir,
        "<html><body><h2>第二章 庭院</h2><p>正文</p></body></html>",
    );
    // 索引：目录里没有 → 回落读正文（导入看到的，也是进库的那个）。
    let indexed = doc_of(&path);
    assert_eq!(
        indexed.flow().unwrap().chapters[0].title,
        "第二章 庭院",
        "索引那一趟有正文（要数字数），标题回落因此成立"
    );
    // 全文：同一个答案。
    let provider = crate::format::provider_for(&path).unwrap();
    let (_, content) = provider.read(&path).unwrap();
    let crate::format::Content::Reflow(flow) = content else {
        panic!("该读成可重排");
    };
    assert_eq!(flow.chapters[0].title, "第二章 庭院");
}

/// 导入那一趟（`index`）**不产出正文**：章表里每一章都是「位置」。
///
/// 这条钉的是「导入不再解压全本」本身 —— 以前 `read()` 会在导入时把每一章的
/// XHTML 都解出来，然后被 `save_flow_in` 丢掉。
#[test]
fn the_index_pass_produces_no_chapter_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = synth_epub(&dir, "<html><body><h2>标题</h2><p>正文</p></body></html>");
    let provider = crate::format::provider_for(&path).unwrap();

    let (_, content) = provider.index(&path).unwrap();
    let crate::format::Content::Reflow(indexed) = content else {
        panic!("该读成可重排");
    };
    assert!(
        indexed.chapters.iter().all(|c| c.source().is_empty()),
        "索引那一趟不该有正文"
    );
    assert!(
        indexed
            .chapters
            .iter()
            .all(|c| matches!(c.locator(), Some(crate::format::ChapterLocator::Entry(_)))),
        "每一章都该只带一个容器条目"
    );

    let (_, content) = provider.read(&path).unwrap();
    let crate::format::Content::Reflow(full) = content else {
        panic!("该读成可重排");
    };
    assert!(
        full.chapters.iter().all(|c| !c.source().is_empty()),
        "全文那一趟才有正文"
    );
}

/// 连标题都没有时，章名回落成「第 N 节」。
#[test]
fn a_chapter_without_a_heading_falls_back_to_a_numbered_title() {
    let dir = tempfile::tempdir().unwrap();
    let path = synth_epub(&dir, "<html><body><p>没有标题</p></body></html>");
    let flow = flow_of(&path);
    assert_eq!(flow.chapters[0].title, "第 1 节");
}

/// M3-E3：manifest 里的 `image/*` 要按容器路径读出固有尺寸；字节**不入** `FlowDoc`，
/// 而是按容器路径现取（学 KOReader）。
#[test]
fn images_are_read_with_dimensions_and_resolved_hrefs() {
    let dir = tempfile::tempdir().unwrap();
    let epub = dir.path().join("with-image.epub");
    let png = png_bytes(3, 2);

    let opf = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>图册</dc:title></metadata>
  <manifest>
    <item id="c1" href="Text/chap1.xhtml" media-type="application/xhtml+xml"/>
    <item id="img1" href="Images/pic.png" media-type="image/png"/>
    <item id="nav" href="Nav/nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
  </manifest>
  <spine><itemref idref="c1"/></spine>
</package>"#;
    let chap = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p>文字<img src="../Images/pic.png" alt="图"/></p></body></html>"#;
    let nav = r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="../Text/chap1.xhtml">第一章</a></li></ol></nav></body></html>"#;

    write_epub(
        &epub,
        &[
            ("mimetype", b"application/epub+zip"),
            ("META-INF/container.xml", CONTAINER.as_bytes()),
            ("OEBPS/content.opf", opf.as_bytes()),
            ("OEBPS/Nav/nav.xhtml", nav.as_bytes()),
            ("OEBPS/Text/chap1.xhtml", chap.as_bytes()),
            ("OEBPS/Images/pic.png", &png),
        ],
    );

    let flow = flow_of(&epub);
    assert_eq!(flow.images.len(), 1, "只应收到那一张图");
    let img = &flow.images[0];
    assert_eq!(img.href, "OEBPS/Images/pic.png");
    assert_eq!(img.media_type, "image/png");
    assert_eq!((img.width, img.height), (3, 2), "固有尺寸来自解码 header");
    // 字节不在 FlowDoc 里，按容器路径现取 —— 这正是 KOReader 的
    // `getObjectImageStream()` 那条路。
    let bytes = doc_of(&epub)
        .image_bytes(&img.href)
        .expect("容器里那张图该能按 href 取回来");
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(bytes, png, "取回来的就是写进去的那份字节");
    assert!(
        doc_of(&epub).image_bytes("OEBPS/Images/nope.png").is_none(),
        "容器里没有的路径要给 None，不是空 Vec"
    );

    // 排版层解析 `src` 用的就是这一对函数，这里顺带锁死行为。
    assert_eq!(
        resolve_href(&dir_of("OEBPS/Text/chap1.xhtml"), "../Images/pic.png"),
        "OEBPS/Images/pic.png"
    );
}

/// `@font-face` 的 `src` 相对的是**那份 CSS 自己的目录**，而 `FlowDoc.css` 是多份
/// CSS 拼成的一个字符串（SQLite 里 `books.css` 那一列就是它）。拼接前不把 `url()`
/// 解成容器绝对路径，读字节那一步就会拿 `Fonts/body.otf` 去要一个其实躺在
/// `OEBPS/Styles/` 下面的文件 —— 表现是「书里明明有字体，就是读不到」，
/// 而屏幕上一点痕迹都没有。
#[test]
fn css_urls_are_absolutized_before_concatenation() {
    let dir = tempfile::tempdir().unwrap();
    let epub = dir.path().join("with-font.epub");
    // 故意写成 `../Fonts/`：解错的目录是最常见的真实形态，也最容易被自己的测试放过去。
    let css = "@charset \"utf-8\";\n@font-face { font-family: \"Book Body\"; src: url(../Fonts/body.otf) format(\"opentype\"), local(\"Noto Serif\"); }\nbody { font-family: \"Book Body\", serif }\n";
    let opf = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>带字体</dc:title></metadata>
  <manifest>
    <item id="c1" href="Text/chap1.xhtml" media-type="application/xhtml+xml"/>
    <item id="css" href="Styles/style.css" media-type="text/css"/>
    <item id="f1" href="Fonts/body.otf" media-type="font/opentype"/>
  </manifest>
  <spine><itemref idref="c1"/></spine>
</package>"#;
    let fake = b"\x00\x01\x00\x00 fake sfnt";
    write_epub(
        &epub,
        &[
            ("mimetype", b"application/epub+zip"),
            ("META-INF/container.xml", CONTAINER.as_bytes()),
            ("OEBPS/content.opf", opf.as_bytes()),
            (
                "OEBPS/Text/chap1.xhtml",
                "<html><body><p>正文</p></body></html>".as_bytes(),
            ),
            ("OEBPS/Styles/style.css", css.as_bytes()),
            ("OEBPS/Fonts/body.otf", fake),
        ],
    );

    let (_, full) = read_with(&epub, true).unwrap();
    let Content::Reflow(flow) = full else {
        panic!("该读成可重排");
    };
    assert!(
        flow.css.contains("url(\"OEBPS/Fonts/body.otf\")"),
        "src 要解成容器绝对路径、带引号、括号配对：{}",
        flow.css
    );
    // 索引路径记下的是位置清单，文本一个字节都不读。
    let (_, indexed) = read_with(&epub, false).unwrap();
    let Content::Reflow(indexed) = indexed else {
        panic!("该读成可重排");
    };
    assert_eq!(
        indexed.css_hrefs,
        vec!["OEBPS/Styles/style.css".to_string()]
    );
    assert!(indexed.css.is_empty(), "索引不该带样式文本");

    // 层叠那一步拿到的就是能直接查容器的路径。
    let rules = RuleSet::parse(&flow.css, Origin::Author);
    assert_eq!(rules.font_faces.len(), 1, "@font-face 该留下来");
    assert_eq!(rules.font_faces[0].family, "Book Body");
    assert_eq!(
        rules.font_faces[0].src,
        vec![
            crate::css::FontSrc::Url("OEBPS/Fonts/body.otf".into()),
            crate::css::FontSrc::Local("Noto Serif".into()),
        ]
    );

    // 正文那条 `font-family` 的第一候选就是声明名 —— app 侧靠整串同名把它认出来。
    let dom = Dom::parse(flow.chapters[0].source().as_bytes());
    let body = dom
        .elements()
        .find(|(_, n)| n.tag.as_ref().map(|t| t.local.to_string()) == Some("body".into()))
        .map(|(id, _)| id)
        .unwrap();
    let styles = cascade(&dom, &rules, &ComputedStyle::default());
    assert_eq!(styles.get(body).unwrap().font_family, "Book Body");
    assert!(crate::css::font_face_matches(
        &styles.get(body).unwrap().font_family,
        &rules.font_faces[0].family
    ));

    // 取字节：声明过的路径给字节，没声明的不给。
    let got = doc_of(&epub).fonts(&[
        "OEBPS/Fonts/body.otf".to_string(),
        "OEBPS/Fonts/nope.otf".to_string(),
    ]);
    assert_eq!(got.len(), 1, "只取 wanted 里真在 manifest 的那一份");
    assert_eq!(got[0].href, "OEBPS/Fonts/body.otf");
    assert_eq!(got[0].bytes, fake.to_vec());
    assert!(
        doc_of(&epub)
            .fonts(&["OEBPS/Fonts/nope.otf".to_string()])
            .is_empty(),
        "声明了一个不存在的路径不该读到别的东西"
    );
}

/// WOFF2 认得出但不取（gpui 那侧的 ttf-parser 不吃，一期不引 brotli）；
/// 单个文件过大只丢它自己，后面那份正常的还得照常拿到。
#[test]
fn woff2_is_skipped_and_an_oversized_font_only_costs_itself() {
    let dir = tempfile::tempdir().unwrap();
    let epub = dir.path().join("odd-fonts.epub");
    let opf = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>怪字体</dc:title></metadata>
  <manifest>
    <item id="c1" href="Text/chap1.xhtml" media-type="application/xhtml+xml"/>
    <item id="big" href="Fonts/big.otf" media-type="font/opentype"/>
    <item id="w2" href="Fonts/x.woff2" media-type="font/woff2"/>
    <item id="ok" href="Fonts/ok.otf" media-type="font/opentype"/>
  </manifest>
  <spine><itemref idref="c1"/></spine>
</package>"#;
    let huge = vec![0u8; super::MAX_ONE + 1];
    write_epub(
        &epub,
        &[
            ("mimetype", b"application/epub+zip"),
            ("META-INF/container.xml", CONTAINER.as_bytes()),
            ("OEBPS/content.opf", opf.as_bytes()),
            (
                "OEBPS/Text/chap1.xhtml",
                "<html><body><p>正文</p></body></html>".as_bytes(),
            ),
            ("OEBPS/Fonts/big.otf", &huge),
            ("OEBPS/Fonts/x.woff2", b"wOF2 whatever"),
            ("OEBPS/Fonts/ok.otf", b"\x00\x01\x00\x00 ok"),
        ],
    );
    let wanted = [
        "OEBPS/Fonts/big.otf",
        "OEBPS/Fonts/x.woff2",
        "OEBPS/Fonts/ok.otf",
    ]
    .map(String::from)
    .to_vec();
    let got = doc_of(&epub).fonts(&wanted);
    assert_eq!(
        got.iter().map(|f| f.href.as_str()).collect::<Vec<_>>(),
        ["OEBPS/Fonts/ok.otf"],
        "过大那份与 woff2 都不该出现，且不能把排在它们后面的那份也带走"
    );
}

/// 一本整书规模的容器读出来是什么样子：章数、正文、CSS、目录能不能对上 spine。
///
/// 合成书里三章的小样本测的是**单个结构**，这一条测的是**规模**：几十条目录、
/// 目录数少于章数、manifest 与 spine 反序、`@charset` 打头的一份 CSS。
/// 输入是 `published_shape_epub` 当场写进临时目录的那一份，不读任何本机文件，
/// 所以它跟着默认套件与 CI 每次真跑 —— 一条需要「某人手上那本书」的测试做不到这点，
/// 而把它改成「文件不在就算通过」又是一盏永远绿的假灯（见 `tests/fixtures/README.md`）。
#[test]
fn a_whole_published_shaped_book_reads_back_chapters_css_and_toc() {
    let dir = tempfile::tempdir().unwrap();
    let path = published_shape_epub(&dir);
    let doc = doc_of(&path);
    let meta = doc.meta();
    // 正文要按位置取回来（索引里只有位置）。
    let flow = flow_of(&path);

    assert_eq!(meta.title, "合成出版书", "书名从 OPF 的 dc:title 来");
    assert_eq!(
        flow.chapters.len(),
        28,
        "一章都不该在 spine 到源文件的路上丢掉"
    );
    assert!(
        flow.chapters
            .iter()
            .all(|c| c.source().contains("<p") && c.source().contains("</p>")),
        "每一章都要带得回它的正文"
    );
    assert!(
        !flow.css_hrefs.is_empty(),
        "出版书一定带 CSS（清单上至少一份）"
    );
    // 目录条目要能对上章节，否则侧栏点击无处可跳。带着 `#fragment` 的那两条也算：
    // 对上说明 fragment 在解 href 时被剥掉了，而不是留在路径里比对不上。
    let matched = flow
        .toc
        .iter()
        .filter(|e| flow.chapters.iter().any(|c| c.spine_href == e.href))
        .count();
    assert_eq!(
        matched,
        flow.toc.len(),
        "有目录条目对不上章节：href 解析有 bug"
    );
    // 标题：目录里有的那条用它。
    assert_eq!(flow.chapters[0].title, "第1章", "章名来自目录");
    // 目录排到 25 之后的那几章：两趟**给同一个答案**（索引那一趟也读正文，
    // 因为 `chapters.char_len` 要在导入时算准，见 `heading_fallback_gives_the_same_title_on_both_passes`）。
    assert_eq!(flow.chapters[27].title, "第28章", "索引那一趟也回落到 h1");
    let provider = crate::format::provider_for(&path).unwrap();
    let (_, content) = provider.read(&path).unwrap();
    let crate::format::Content::Reflow(full) = content else {
        panic!("该读成可重排");
    };
    assert_eq!(full.chapters[27].title, "第28章", "全文那一趟同答案");
}

/// **接线那一步的验收线**：读同一本书的一百张图，容器只该被打开一次。
///
/// 这条曾经挂着 `#[ignore]`：`image_bytes` 每次调用都 `Zip::open`（重读一遍中央
/// 目录、重建几千个条目名的索引），一百次取图就是一百次开容器，跑出来
/// `left: 100, right: 1`。它当时是一句**写好的承诺** —— 目标形状是把容器索引留成
/// 一份共享的不可变结果（`plans/导入管线.md` §十 的 `Container`）。
///
/// 那个形状落地之后（`format::container`，`BookDocument` 持有它并按需懒开），
/// 这行属性就去掉了：它从承诺变成了守门员。谁把 `BookDocument::image_bytes`
/// 改回「每次问 provider 要」，这条会立刻红。
#[test]
fn a_hundred_image_fetches_should_open_the_container_once() {
    use crate::metrics::{Metric, take_local};

    let dir = tempfile::tempdir().unwrap();
    let epub = dir.path().join("many-images.epub");
    let png = png_bytes(3, 2);
    let opf = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>图册</dc:title></metadata>
  <manifest>
    <item id="c1" href="Text/chap1.xhtml" media-type="application/xhtml+xml"/>
    <item id="img1" href="Images/pic.png" media-type="image/png"/>
  </manifest>
  <spine><itemref idref="c1"/></spine>
</package>"#;
    let chap = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p>文字<img src="../Images/pic.png" alt="图"/></p></body></html>"#;

    write_epub(
        &epub,
        &[
            ("mimetype", b"application/epub+zip"),
            ("META-INF/container.xml", CONTAINER.as_bytes()),
            ("OEBPS/content.opf", opf.as_bytes()),
            ("OEBPS/Text/chap1.xhtml", chap.as_bytes()),
            ("OEBPS/Images/pic.png", &png),
        ],
    );

    let doc = doc_of(&epub);
    take_local(Metric::ContainerOpens);
    for _ in 0..100 {
        assert!(doc.image_bytes("OEBPS/Images/pic.png").is_some());
    }
    assert_eq!(
        take_local(Metric::ContainerOpens),
        1,
        "整本书读图只该开一次容器；每取一张开一次就是每次读一遍中央目录"
    );
}
