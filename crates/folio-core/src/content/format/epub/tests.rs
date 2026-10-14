//! EPUB 容器读取。测的是一份**合成**的书（`tests/fixtures/oss/minimal-zh.epub`），
//! 故意把 manifest 顺序、spine 顺序、文件名里的空格、nav 里的 fragment
//! 都设成会出错的样子。只依赖公开的 `document::open` / `resolve_href` 等。

use std::path::{Path, PathBuf};

use super::read_with;
use crate::content::dom::Dom;
use crate::content::format::css::computed::{ComputedStyle, TextAlign, cascade};
use crate::content::format::css::{Origin, RuleSet};
use crate::content::format::detect;
use crate::content::format::{
    Chapter, Content, DocMeta, FlowDoc, Format, ParseBudget, dir_of, resolve_href,
};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/oss/minimal-zh.epub")
}

/// 打开一个容器，返回**统一接口** —— 测试也走 `document::open` 这条 app 唯一的
/// 「文件 → 文档」入口（`format` 那一层不再有 `open`）。
fn doc_of(path: &Path) -> Box<dyn crate::content::document::Document> {
    crate::content::document::open(path).expect("合成容器应能打开")
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
        crate::content::document::open(&bad).is_err(),
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
    let provider = crate::content::format::provider_for(&path).unwrap();
    let (_, content) = provider.read(&path).unwrap();
    let crate::content::format::Content::Reflow(flow) = content else {
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
    let provider = crate::content::format::provider_for(&path).unwrap();

    let (_, content) = provider.index(&path).unwrap();
    let crate::content::format::Content::Reflow(indexed) = content else {
        panic!("该读成可重排");
    };
    assert!(
        indexed.chapters.iter().all(|c| c.source().is_empty()),
        "索引那一趟不该有正文"
    );
    assert!(
        indexed.chapters.iter().all(|c| matches!(
            c.locator(),
            Some(crate::content::format::ChapterLocator::Entry(_))
        )),
        "每一章都该只带一个容器条目"
    );

    let (_, content) = provider.read(&path).unwrap();
    let crate::content::format::Content::Reflow(full) = content else {
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
            crate::content::format::css::FontSrc::Url("OEBPS/Fonts/body.otf".into()),
            crate::content::format::css::FontSrc::Local("Noto Serif".into()),
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
    assert!(crate::content::format::css::font_face_matches(
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
    let provider = crate::content::format::provider_for(&path).unwrap();
    let (_, content) = provider.read(&path).unwrap();
    let crate::content::format::Content::Reflow(full) = content else {
        panic!("该读成可重排");
    };
    assert_eq!(full.chapters[27].title, "第28章", "全文那一趟同答案");
}

/// 强制顺序与强制并行必须逐字段等价。
///
/// 并行路径把「读章节正文」和「读图片尺寸」放进同一条动态队列，worker 各自开
/// 一份 zip；完成顺序是乱的。这一条用一本 20 章 + 20 图的合成书把 `Some(1)` 与
/// `Some(4)` 各跑一遍，钉住三件事：章序仍按 spine、图片表仍按 manifest、每个条目
/// 的解压/字数/DOM 解析一个都不少。最后再单独核一遍 20 张图的宽高与顺序 ——
/// 少了某几张时 `assert_eq!` 的全量比较在条目数相等的情况下也可能漏掉。
#[test]
fn forced_parallel_indexing_matches_the_sequential_result() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("parallel.epub");
    const CHAPTERS: usize = 20;

    let mut manifest = String::new();
    for n in 1..=CHAPTERS {
        manifest.push_str(&format!(
            "    <item id=\"c{n}\" href=\"Text/chap{n}.xhtml\" media-type=\"application/xhtml+xml\"/>\n"
        ));
    }
    for n in 1..=CHAPTERS {
        manifest.push_str(&format!(
            "    <item id=\"img{n}\" href=\"Images/pic{n}.png\" media-type=\"image/png\"/>\n"
        ));
    }
    let mut spine = String::new();
    for n in 1..=CHAPTERS {
        spine.push_str(&format!("      <itemref idref=\"c{n}\"/>\n"));
    }
    let opf = format!(
        r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>并行合成书</dc:title></metadata>
  <manifest>
{manifest}  </manifest>
  <spine>
{spine}  </spine>
</package>"#
    );

    let mut entries: Vec<(String, Vec<u8>)> = vec![
        ("mimetype".to_string(), b"application/epub+zip".to_vec()),
        (
            "META-INF/container.xml".to_string(),
            CONTAINER.as_bytes().to_vec(),
        ),
        ("OEBPS/content.opf".to_string(), opf.into_bytes()),
    ];
    for n in 1..=CHAPTERS {
        entries.push((
            format!("OEBPS/Text/chap{n}.xhtml"),
            format!("<html><body><h1>第{n}章</h1><p>第{n}章正文。</p></body></html>").into_bytes(),
        ));
    }
    for n in 1..=CHAPTERS {
        // 宽 = n + 1，高固定 2：结果里第几张图对应哪个尺寸一眼能验。
        entries.push((
            format!("OEBPS/Images/pic{n}.png"),
            png_bytes(n as u32 + 1, 2),
        ));
    }
    let refs: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    write_epub(&path, &refs);

    for with_text in [false, true] {
        let sequential = super::read_with_impl(&path, with_text, ParseBudget::Exactly(1)).unwrap();
        let parallel = super::read_with_impl(&path, with_text, ParseBudget::Exactly(4)).unwrap();
        assert_eq!(
            sequential, parallel,
            "with_text={with_text} 时顺序与并行产出不一致"
        );
    }

    let (_, content) = super::read_with_impl(&path, false, ParseBudget::Exactly(4)).unwrap();
    let Content::Reflow(flow) = content else {
        panic!("该读成可重排");
    };
    assert_eq!(flow.images.len(), CHAPTERS, "并行解析不许丢图");
    for (i, image) in flow.images.iter().enumerate() {
        assert_eq!(image.href, format!("OEBPS/Images/pic{}.png", i + 1));
        assert_eq!(
            (image.width, image.height),
            (i as u32 + 2, 2),
            "第 {} 张图的尺寸与顺序对不上",
            i + 1
        );
    }
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
    use crate::infra::metrics::{Metric, take_local};

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

// ── 插图清单只解前缀（见 `super::parse_image` 与 `super::IMAGE_HEAD_BYTES`）──────
//
// 三条测试各守一条分支，都能在该分支被拆掉时变红：
// 1. 前缀读 + 不整条读 → 尾部坏掉的图仍定得出尺寸；
// 2. 「认不出是位图就不再读」→ SVG 不配占一次整条解压；
// 3. 「认得出但头在更后面 → 补整条」→ 长注释段的 JPEG 不许记成 0×0。

/// 一本 1 章 + 1 张插图的合成容器，**插图是最后写进去的那个条目**（尾部损坏那两条测试
/// 要把动手的位置落在它的压缩数据里）。回 `(路径, 图片的容器条目名)`。
fn one_image_epub(
    dir: &tempfile::TempDir,
    name: &str,
    mime: &str,
    bytes: &[u8],
) -> (PathBuf, String) {
    let path = dir.path().join(name);
    let ext = match mime {
        "image/svg+xml" => "svg",
        "image/jpeg" => "jpg",
        _ => "png",
    };
    let entry = format!("Images/pic.{ext}");
    let opf = format!(
        r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>一张图</dc:title></metadata>
  <manifest>
    <item id="c1" href="Text/chap1.xhtml" media-type="application/xhtml+xml"/>
    <item id="img1" href="{entry}" media-type="{mime}"/>
  </manifest>
  <spine><itemref idref="c1"/></spine>
</package>"#
    );
    let chap = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p>文字</p></body></html>"#;
    write_epub(
        &path,
        &[
            ("mimetype", b"application/epub+zip"),
            ("META-INF/container.xml", CONTAINER.as_bytes()),
            ("OEBPS/content.opf", opf.as_bytes()),
            ("OEBPS/Text/chap1.xhtml", chap.as_bytes()),
            (&format!("OEBPS/{entry}"), bytes),
        ],
    );
    (path, format!("OEBPS/{entry}"))
}

/// 把容器里**最后一个条目**压缩数据的尾部若干字节翻掉：模拟一本下载不全、
/// 或者被人动过一角的书。
///
/// 动手的位置由中央目录的起点定（EOCD 记录 +16 处那个 4 字节小端偏移），
/// 它前面那段一定属于最后写进去的条目。两条长度断言是给测试自己的：条目短到
/// 前缀就读得完、或压缩后本来就比要坏掉的那段还短时，这条就没在测它声称的东西。
fn damage_tail_of_last_entry(file: &Path, name: &str, count: usize) {
    let mut data = std::fs::read(file).unwrap();
    let eocd = data.len() - 22;
    assert_eq!(
        &data[eocd..eocd + 4],
        b"PK\x05\x06",
        "合成容器的 EOCD 该在末尾"
    );
    let cdir = u32::from_le_bytes(data[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
    assert!(cdir < eocd, "中央目录的偏移该在 EOCD 之前");

    let head = super::IMAGE_HEAD_BYTES as usize;
    let (size, compressed) = {
        let mut arch = zip::ZipArchive::new(std::fs::File::open(file).unwrap()).unwrap();
        let entry = arch.by_name(name).unwrap();
        (entry.size() as usize, entry.compressed_size() as usize)
    };
    assert!(
        size > 4 * head,
        "条目解压后 {size} 字节，前缀窗口 {head} 都盖不住它，测不出前缀读"
    );
    assert!(
        compressed > 2 * head,
        "条目压缩后 {compressed} 字节：压得太狠，坏掉的尾部会落进前缀读到的那段里"
    );
    assert!(cdir > count, "要坏掉的 {count} 字节比整个数据区还长");
    for byte in &mut data[cdir - count..cdir] {
        *byte = !*byte;
    }
    std::fs::write(file, &data).unwrap();
}

/// 压不动的字节：deflate 对它们几乎不缩，所以条目解压后与压缩后都远超前缀那 32 KiB。
/// 用哈希而不是真随机，测试才可重复。
fn incompressible(count: usize) -> Vec<u8> {
    (0..count)
        .map(|i| {
            let mut v = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xDEAD_BEEF;
            v ^= v >> 29;
            v = v.wrapping_mul(0xBF58_476D_1CE4_E5B9);
            b'!' + (v % 90) as u8
        })
        .collect()
}

/// 噪声 PNG：同样的理由（压不动）用它当「一张真的大位图」。
/// 纯色图会被压成几百字节，那样「坏在尾部」就落进前缀窗口里了。
fn noisy_png_bytes(w: u32, h: u32) -> Vec<u8> {
    use image::ImageEncoder as _;
    let img = image::RgbaImage::from_fn(w, h, |x, y| {
        let mix = |k: u32| {
            let mut v = ((x + 1) as u64).wrapping_mul(2654435761)
                ^ ((y + 3) as u64).wrapping_mul(1597334677)
                ^ (k as u64).wrapping_mul(40503);
            v ^= v >> 15;
            (v.wrapping_mul(2246822519) >> 24) as u8
        };
        image::Rgba([mix(1), mix(2), mix(3), 255])
    });
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgba8)
        .expect("合成噪声 PNG 该编得出来");
    out
}

/// 把一张真 JPEG 的**尺寸头推到前缀之外**：在 SOI 之后插一段超长 COM 注释段，
/// SOFn 就被顶到 `IMAGE_HEAD_BYTES` 之后。其余字节一个没动，整条读仍然解得开。
fn jpeg_with_late_header(w: u32, h: u32) -> Vec<u8> {
    use image::ImageEncoder as _;
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        image::Rgb([
            ((x * 7 + y * 11) % 256) as u8,
            (((x * 41) ^ (y * 17)) % 256) as u8,
            (((x + y) * 5) % 256) as u8,
        ])
    });
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 70)
        .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgb8)
        .expect("合成 JPEG 该编得出来");
    assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "JPEG 该以 SOI 开头");

    // COM 段 = FFFE + 2 字节长度（含长度字段自己）+ 负载；解码器按长度跳过它。
    // 长度字段是 u16，所以一次最多垫 65533 字节；不够就多插几段。
    let target = super::IMAGE_HEAD_BYTES as usize + 8192;
    let mut out = Vec::with_capacity(jpeg.len() + target);
    out.extend_from_slice(&jpeg[..2]);
    let mut padded = 0usize;
    while padded < target {
        let payload = (target - padded).min(u16::MAX as usize - 2);
        out.extend_from_slice(&[0xFF, 0xFE, ((payload + 2) >> 8) as u8, (payload + 2) as u8]);
        out.resize(out.len() + payload, b' ');
        padded += payload + 2;
    }
    out.extend_from_slice(&jpeg[2..]);
    out
}

/// **前缀读的直接后果**：一张尾部坏掉的插图仍然进得了清单、仍然有正确尺寸。
///
/// 整条解压那套旧写法在这里会失败（zip 在流末尾校 CRC，解到坏字节也会错），
/// 于是 `parse_image` 回 `None` —— 这张图从清单里**整个消失**，版面于是不知道它占多大，
/// 读者看到的是文字被一张不存在的图挤开，或者图压在文字上。
/// 尺寸头在最前面，前缀读只要头部完好就定得出来。
#[test]
fn an_image_with_a_damaged_tail_still_reports_its_size() {
    let dir = tempfile::tempdir().unwrap();
    let (epub, entry) = one_image_epub(
        &dir,
        "damaged.epub",
        "image/png",
        &noisy_png_bytes(300, 400),
    );
    damage_tail_of_last_entry(&epub, &entry, 4096);

    let (_, content) =
        super::read_with_impl(&epub, false, ParseBudget::Exactly(1)).expect("书该照样打得开");
    let Content::Reflow(flow) = content else {
        panic!("该读成可重排");
    };
    assert_eq!(flow.chapters.len(), 1, "正文不受插图损坏的影响");
    assert_eq!(flow.images.len(), 1, "尾部坏掉的图不该从清单里消失");
    assert_eq!(
        (flow.images[0].width, flow.images[0].height),
        (300, 400),
        "尺寸头在前缀里，该照样定得出来"
    );
}

/// SVG 那一类「读完整条也拿不到尺寸」的条目，一次整条解压都不该花。
///
/// 与前一条同一个观察手段：把条目尾部弄坏。如果代码真的读了整条，读会失败、
/// 这一项就从清单里消失；只读前缀则走到「认不出是位图 → 不再读」那一档，
/// 于是它**带着 0×0 出现在清单里** —— 那正是排版留占位要的键。
#[test]
fn an_svg_item_does_not_cost_a_whole_entry_inflate() {
    let dir = tempfile::tempdir().unwrap();
    // 一份 160 KiB 的 SVG，注释段用压不动的字节：条目压缩后仍远超前缀窗口，
    // 「坏在尾部」才落在前缀读不到的地方。SVG 的内容没人解析，字符随便是什么都行。
    let body = String::from_utf8(incompressible(160 * 1024)).expect("填充字节该是合法 ASCII");
    let svg = format!(
        "<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"><!--{body}--></svg>"
    );
    let (epub, entry) = one_image_epub(&dir, "svg.epub", "image/svg+xml", svg.as_bytes());
    damage_tail_of_last_entry(&epub, &entry, 4096);

    let (_, content) =
        super::read_with_impl(&epub, false, ParseBudget::Exactly(1)).expect("SVG 书该打得开");
    let Content::Reflow(flow) = content else {
        panic!("该读成可重排");
    };
    assert_eq!(flow.images.len(), 1, "SVG 也要进清单（排版按 href 查它）");
    assert_eq!(
        (flow.images[0].width, flow.images[0].height),
        (0, 0),
        "SVG 定不出固有尺寸，由排版的降级规则兜底"
    );
}

/// 尺寸头排在更后面的 JPEG 必须补一次整条读，而不是记成 0×0。
///
/// 带大 EXIF 缩略图或长注释段的真实出版物 JPEG 就是这个形状。前缀读不出来时
/// `HeadDims::NeedsMore` 那条分支的存在，就是为了不让这一类书的插图没有尺寸。
#[test]
fn a_jpeg_whose_header_sits_past_the_prefix_falls_back_to_the_whole_entry() {
    let dir = tempfile::tempdir().unwrap();
    let late = jpeg_with_late_header(64, 32);
    let (epub, _entry) = one_image_epub(&dir, "late-header.epub", "image/jpeg", &late);

    let (_, content) = super::read_with_impl(&epub, false, ParseBudget::Exactly(1))
        .expect("带长注释的 JPEG 书该打得开");
    let Content::Reflow(flow) = content else {
        panic!("该读成可重排");
    };
    assert_eq!(flow.images.len(), 1);
    assert_eq!(
        (flow.images[0].width, flow.images[0].height),
        (64, 32),
        "尺寸头在前缀之外时该补整条读，而不是降级成 0×0"
    );
}

/// 封面那一件该由 **worker** 领走，而且一本书只做一次。
///
/// 数的是「在哪个线程上做的 + 做了几次」而不是毫秒：并行段的计时被调度噪声牵着走，
/// 而这两格有确定的整数与确定的不等式答案。
///
/// - 并行（4 路）：记到一次，且那个线程号**不是**调用线程 → 封面确实在队列里。
///   把它退回主线程串行做，这里记不到号，`len() == 1` 立刻红。
/// - 顺序（1 路）：也做一次，而且就在调用线程上 → 证明是「进了队列」而不是「换了个地方做」。
/// - 两次跑完总共两次：并行失败退回顺序时不该把封面**再做一遍**。
#[test]
fn the_cover_is_a_queue_job_not_main_thread_work() {
    // 计数是全局的（worker 上做这件事只有全局量看得见），所以这张封面用**本测试独有的
    // 条目名**，读出来再按它筛：cargo 并行跑测试，别的书也都有封面，不筛就是随机翻红。
    const COVER_ENTRY: &str = "OEBPS/Images/queue-job-only-cover.png";
    let dir = tempfile::tempdir().unwrap();
    let epub = dir.path().join("cover-job.epub");
    let opf = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>有封面的书</dc:title>
    <meta name="cover" content="img1"/>
  </metadata>
  <manifest>
    <item id="c1" href="Text/chap1.xhtml" media-type="application/xhtml+xml"/>
    <item id="img1" href="Images/queue-job-only-cover.png" media-type="image/png"/>
  </manifest>
  <spine><itemref idref="c1"/></spine>
</package>"#;
    let chap = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p>文字</p></body></html>"#;
    write_epub(
        &epub,
        &[
            ("mimetype", b"application/epub+zip"),
            ("META-INF/container.xml", CONTAINER.as_bytes()),
            ("OEBPS/content.opf", opf.as_bytes()),
            ("OEBPS/Text/chap1.xhtml", chap.as_bytes()),
            (COVER_ENTRY, &png_bytes(600, 800)),
        ],
    );

    let mine = || -> Vec<std::thread::ThreadId> {
        super::COVER_WORK
            .lock()
            .unwrap()
            .iter()
            .filter(|(entry, _)| entry == COVER_ENTRY)
            .map(|(_, id)| *id)
            .collect()
    };
    let me = std::thread::current().id();

    // 封面 + 一章 = 两件活，4 路上限被条目数压到 2 路：仍然是并行路径。
    let (_, content) =
        super::read_with_impl(&epub, false, ParseBudget::Exactly(4)).expect("并行该打得开");
    let Content::Reflow(flow) = content else {
        panic!("该读成可重排");
    };
    assert!(flow.cover.is_some(), "封面字节该照样产得出来");
    let done = mine();
    assert_eq!(done.len(), 1, "并行这一趟封面该做一次");
    assert_ne!(done[0], me, "并行时封面该在 worker 上做，不是调用线程");

    let (_, _) =
        super::read_with_impl(&epub, false, ParseBudget::Exactly(1)).expect("顺序也该打得开");
    let done = mine();
    assert_eq!(done.len(), 2, "顺序那一趟再做一次，不该和并行那趟重复");
    assert_eq!(done[1], me, "顺序路径的封面就在调用线程上做");
}

/// **这一改动的主信号**：索引一趟一次 DOM 都不解析。
///
/// 数的是次数而不是毫秒：`char_len` 与标题回落只要「可见字符数」和「第一个标题的文本」，
/// 走 DOM 就得给每个节点拼一条路径字符串，而导入时一次都不读它。退回 `Dom::parse`
/// 那一趟会立刻把这一格从 0 变成章数，计时噪声再大也骗不了这条。
///
/// 用 `Some(1)` 强制顺序：`DomParses` 是线程局部计数，并行时解析发生在 worker 线程上，
/// 主线程读到的局部值会漏算（基准里读全局量就是为了这个）。顺序那一趟与并行走的是
/// 同一段代码，所以断这一条就够。
#[test]
fn indexing_a_book_parses_no_dom_at_all() {
    use crate::infra::metrics::{Metric, take_local};

    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/local/shejie-mauriac.epub");
    for with_text in [false, true] {
        take_local(Metric::DomParses);
        let (_, content) =
            super::read_with_impl(&path, with_text, ParseBudget::Exactly(1)).expect("该打得开");
        let parses = take_local(Metric::DomParses);
        let Content::Reflow(flow) = content else {
            panic!("该读成可重排");
        };
        assert!(flow.chapters.len() >= 20, "样本太小，这条会假绿");
        assert_eq!(
            parses,
            0,
            "with_text={with_text}：索引 {} 章该一次 DOM 都不解析",
            flow.chapters.len()
        );
    }
}

/// **这一改动的主信号**：索引一趟只解一次中央目录。
///
/// worker 现在拿的是「条目区间 + 自己的句柄」（`EntryReader`），不再各自 `Zip::open`
/// 把中央目录读一遍、两张表再建一遍（实测 1233 条目的书那一次是 1.5 ms，四个 worker
/// 就是四份）。退回每 worker 开一次容器，这一格就从 1 变成 1 + worker 数。
#[test]
fn an_indexing_pass_opens_the_container_once() {
    use crate::infra::metrics::{Metric, take_local};

    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/local/shejie-mauriac.epub");
    take_local(Metric::ContainerOpens);
    let (_, content) =
        super::read_with_impl(&path, false, ParseBudget::Exactly(4)).expect("并行索引该打得开");
    let opens = take_local(Metric::ContainerOpens);
    let Content::Reflow(flow) = content else {
        panic!("该读成可重排");
    };
    assert!(flow.chapters.len() >= 20, "样本太小，这条会假绿");
    assert_eq!(
        opens, 1,
        "一本书的索引只该解一次中央目录；其余四份是 worker 各自开容器的钱"
    );
}

/// 外层分下来的预算**真的**落到内层，而且只是**上限**。
///
/// 三条断言一起才有意义：
/// * `AtMost(1)` 必须 1 路 —— 这一条在任何机器上都成立，是「预算被丢掉」的靶子
///   （丢掉之后单核机上是 1、多核机上是 4，前者由下面那条兜住）。
/// * 压到比这台机器自己能定的还低（`auto - 1`），就必须正好是那个数 —— 这是「外层的数
///   说了算」的本体。
/// * 给一个比 `auto` 还高的上限，结果不许超过 `auto`：预算是往下压，不是往上抬。
///
/// 用的是条目过了「少条目不起线程」那道门的书（`shejie-mauriac.epub`，53 个条目），
/// 否则三条都会退化成 1 而假绿。拿掉 `EpubAdapter::index_with` 里把 `budget` 传下去那一步，
/// 第二条立刻变红。
#[test]
fn the_budget_handed_down_from_the_batch_caps_the_parser() {
    use crate::content::document;

    let book = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/local/shejie-mauriac.epub");
    // 这台机器上不被外层压限时自己会定几路。
    let auto = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, super::MAX_PARSE_WORKERS);

    let picked = |n| {
        let doc = document::open_with(&book, ParseBudget::AtMost(n)).expect("该打得开");
        (super::last_workers(), doc)
    };

    let (one, doc_one) = picked(1);
    assert_eq!(one, 1, "给了 1 路就必须只起 1 路");

    let below = auto - 1;
    let (capped, doc_capped) = picked(below);
    assert_eq!(
        capped, below,
        "外层压到 {below} 路，内层就得是 {below} 路（这台机器的 auto 是 {auto}）"
    );

    let (loose, _) = picked(super::MAX_PARSE_WORKERS + 4);
    assert_eq!(loose, auto, "上限比这台机器自己能定还高时，不许越过 auto");

    // 路数不同不许改读出来的内容：并行只换取字节的排程，不换读法。
    let a = doc_one.flow().expect("可重排");
    let b = doc_capped.flow().expect("可重排");
    assert_eq!(a.chapters.len(), b.chapters.len(), "两趟的章数不许有差别");
    for (x, y) in a.chapters.iter().zip(&b.chapters) {
        assert_eq!(
            (x.title.as_str(), x.char_len),
            (y.title.as_str(), y.char_len)
        );
    }
}

// ——————————————————————————————————————————————————————————————
// EPUB3 的这一批：nav 的三个档与纯容器、spine 的 linear、读不动的那一档、加密容器。
// 夹具全部当场合成（`tests/fixtures/README.md`：输入只在本机存在的测试要么永远
// `#[ignore]`，要么是一盏永远绿的假灯）。
// ——————————————————————————————————————————————————————————————

/// 一份只有一章正文的 EPUB3，nav 文档由参数给。
fn epub_nav_only(dir: &tempfile::TempDir, nav: &str) -> PathBuf {
    let path = dir.path().join("nav.epub");
    let opf = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>带 nav 的书</dc:title></metadata>
  <manifest>
    <item id="c1" href="Text/chap1.xhtml" media-type="application/xhtml+xml"/>
    <item id="nav" href="Nav/nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
  </manifest>
  <spine><itemref idref="c1"/></spine>
</package>"#;
    write_epub(
        &path,
        &[
            ("mimetype", b"application/epub+zip"),
            ("META-INF/container.xml", CONTAINER.as_bytes()),
            ("OEBPS/content.opf", opf.as_bytes()),
            ("OEBPS/Nav/nav.xhtml", nav.as_bytes()),
            (
                "OEBPS/Text/chap1.xhtml",
                "<html><body><p>正文</p></body></html>".as_bytes(),
            ),
        ],
    );
    path
}

/// 「上卷 / 下卷」这种只有 `<span>` 没有 `<a>` 的卷级标题，出版书里非常常见。
///
/// 这一条钉的是它**别从目录里消失**：以前它被丢掉、子条目整体上一格，于是目录看不出
/// 「上卷」管着哪两章；现在它留成一个标题，位置从第一个解得开的子孙回填（点它该跳到
/// 那一卷的第一节，不是一行按了没反应的死字）。
#[test]
fn a_span_only_nav_heading_stays_in_the_toc() {
    let nav = r#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml"><body><nav epub:type="toc"><ol>
  <li><span>上卷</span><ol>
    <li><a href="Text/chap1.xhtml">第一章</a></li>
  </ol></li>
</ol></nav></body></html>"#;
    let dir = tempfile::tempdir().unwrap();
    let flow = flow_of(&epub_nav_only(&dir, nav));
    let toc = &flow.toc;
    assert_eq!(toc.len(), 1, "顶层就是那一卷，不该被展平");
    assert_eq!(toc[0].title, "上卷");
    assert_eq!(toc[0].children.len(), 1, "第一章挂在卷下面");
    assert_eq!(
        toc[0].children[0].href, "OEBPS/Text/chap1.xhtml",
        "子条目该解成容器绝对路径"
    );
    assert_eq!(
        toc[0].href, "OEBPS/Text/chap1.xhtml",
        "纯容器的位置从第一个子孙回填：{:?}",
        toc[0]
    );
}

/// 上一条那个回落**只在按 nav 自己解不出时**才发生：两个地方都有同名条目时，
/// 留下的仍是按 nav 文档解出来的那一个（规范那一读法优先）。
///
/// 这一条是回落那一步的反证：把它改成「永远按 OPF 目录解」，只有 `OEBPS/Text/` 一棵的
/// 书（包括上一条）看不出差别，两棵都在的书才分得开 —— 而那才是会把读者指错地方的
/// 那种书：nav 目录里真放着一份和正文同名的 alt 版本。
#[test]
fn the_nav_resolves_against_itself_when_both_places_have_the_file() {
    let nav = r#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml"><body><nav epub:type="toc"><ol>
  <li><a href="Text/chap1.xhtml">第一章</a></li>
</ol></nav></body></html>"#;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("both-shapes.epub");
    let opf = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>两棵都有</dc:title></metadata>
  <manifest>
    <item id="c1" href="Text/chap1.xhtml" media-type="application/xhtml+xml"/>
    <item id="nav" href="Nav/nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
  </manifest>
  <spine><itemref idref="c1"/></spine>
</package>"#;
    write_epub(
        &path,
        &[
            ("mimetype", b"application/epub+zip"),
            ("META-INF/container.xml", CONTAINER.as_bytes()),
            ("OEBPS/content.opf", opf.as_bytes()),
            ("OEBPS/Nav/nav.xhtml", nav.as_bytes()),
            (
                "OEBPS/Nav/Text/chap1.xhtml",
                "<html><body><p>nav 旁边那一份</p></body></html>".as_bytes(),
            ),
            (
                "OEBPS/Text/chap1.xhtml",
                "<html><body><p>OPF 那一棵</p></body></html>".as_bytes(),
            ),
        ],
    );
    let flow = flow_of(&path);
    assert_eq!(
        flow.toc[0].href, "OEBPS/Nav/Text/chap1.xhtml",
        "两条路都解得出时不许换成 OPF 那一棵"
    );
}

/// nav 里三档各是什么用途时，目录取哪一棵：**toc → landmarks → page-list**。
///
/// 只有 landmarks 或只有 page-list 的书（教科书尤其常见后者）不该是一栏空白。
/// 反过来，toc 在场时后面两档不许抢 —— 页码表当目录用会把读者淹死。
#[test]
fn the_nav_falls_back_to_landmarks_then_page_list() {
    let landmarks = r#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml"><body>
  <nav epub:type="landmarks"><ol><li><a href="Text/chap1.xhtml">正文起点</a></li></ol></nav>
</body></html>"#;
    let dir = tempfile::tempdir().unwrap();
    let flow = flow_of(&epub_nav_only(&dir, landmarks));
    assert_eq!(flow.toc.len(), 1, "只有 landmarks 时它顶上");
    assert_eq!(flow.toc[0].title, "正文起点");

    let page_list = r#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml"><body>
  <nav epub:type="page-list"><ol><li><a href="Text/chap1.xhtml">1</a></li></ol></nav>
</body></html>"#;
    let flow = flow_of(&epub_nav_only(&dir, page_list));
    assert_eq!(flow.toc.len(), 1, "连 landmarks 都没有时 page-list 兜住");

    // toc 在场时，后面两档不许盖过它。
    let all_three = r#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml"><body>
  <nav epub:type="landmarks"><ol><li><a href="Text/chap1.xhtml">正文起点</a></li></ol></nav>
  <nav epub:type="toc"><ol><li><a href="Text/chap1.xhtml">第一章</a></li></ol></nav>
  <nav epub:type="page-list"><ol><li><a href="Text/chap1.xhtml">1</a></li></ol></nav>
</body></html>"#;
    let flow = flow_of(&epub_nav_only(&dir, all_three));
    assert_eq!(flow.toc.len(), 1);
    assert_eq!(flow.toc[0].title, "第一章", "toc 优先于另两档");
}

/// spine 上 `linear="no"` 的那几节不是读物：封面的隐藏 alt 文本、脚注弹层都写在里面。
///
/// 收进来会往正文里插一页读者从没打算读的东西，还会把后面每一章的序号顶后一格。
#[test]
fn a_non_linear_spine_item_is_not_a_chapter() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nonlinear.epub");
    let opf = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>有非线性项的书</dc:title></metadata>
  <manifest>
    <item id="alt" href="Text/alt.xhtml" media-type="application/xhtml+xml"/>
    <item id="c1" href="Text/chap1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>
    <itemref idref="alt" linear="no"/>
    <itemref idref="c1"/>
  </spine>
</package>"#;
    write_epub(
        &path,
        &[
            ("mimetype", b"application/epub+zip"),
            ("META-INF/container.xml", CONTAINER.as_bytes()),
            ("OEBPS/content.opf", opf.as_bytes()),
            (
                "OEBPS/Text/alt.xhtml",
                "<html><body><p>这段是封面的替代文本</p></body></html>".as_bytes(),
            ),
            (
                "OEBPS/Text/chap1.xhtml",
                "<html><body><p>正文</p></body></html>".as_bytes(),
            ),
        ],
    );
    let flow = flow_of(&path);
    assert_eq!(flow.chapters.len(), 1, "非线性那项不该成为一章");
    assert_eq!(flow.chapters[0].spine_href, "OEBPS/Text/chap1.xhtml");
    // 标题回落数的是**spine 上的位置**而不是过滤后的序号，这是这一层一直以来的口径
    // （见 `ChapterJob::spine` 的说明：被跳过的 nav 不该改变后面章节的编号）。
    // 非线性项按同一条走，于是这一章就叫「第 2 节」。看着别扭，但换成过滤后的序号
    // 会要每次改动都重算全书编号 —— 那条更贵，而读者真读到的是标题而不是这个回落。
    assert_eq!(flow.chapters[0].title, "第 2 节", "{:?}", flow.chapters[0]);
}

/// spine 上写着读不动的内容（逐页 SVG 那一类）时，留一章说明而不是让章凭空少一页。
///
/// 读者的下场本来是「这本书少了一章」，而那一件查不出原因。
#[test]
fn an_unreadable_spine_item_leaves_a_chapter_that_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("svgspine.epub");
    let opf = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>逐页 SVG 的书</dc:title></metadata>
  <manifest>
    <item id="c1" href="Text/chap1.xhtml" media-type="application/xhtml+xml"/>
    <item id="p1" href="Images/page1.svg" media-type="image/svg+xml"/>
    <item id="css" href="Styles/style.css" media-type="text/css"/>
  </manifest>
  <spine>
    <itemref idref="c1"/>
    <itemref idref="p1"/>
    <itemref idref="css"/>
  </spine>
</package>"#;
    write_epub(
        &path,
        &[
            ("mimetype", b"application/epub+zip"),
            ("META-INF/container.xml", CONTAINER.as_bytes()),
            ("OEBPS/content.opf", opf.as_bytes()),
            (
                "OEBPS/Text/chap1.xhtml",
                "<html><body><p>正文</p></body></html>".as_bytes(),
            ),
            ("OEBPS/Images/page1.svg", b"<svg/>".as_slice()),
            ("OEBPS/Styles/style.css", b"p{margin:0}".as_slice()),
        ],
    );
    let flow = flow_of(&path);
    // 样式是纯资源（不该成为一章），SVG 在 spine 上就是一页内容（该留一章说明）。
    assert_eq!(flow.chapters.len(), 2, "正文一章 + 读不了的一页");
    let notice = &flow.chapters[1];
    assert!(
        notice.source().contains("image/svg+xml"),
        "那一章要说出它是什么：{}",
        notice.source()
    );
    assert!(
        notice.spine_href.is_empty(),
        "说明文字在手上，不该有一个回容器取字节的位置"
    );
    assert!(notice.char_len().is_some(), "说明那一段的字数照样数得出");
}

/// 加密书在门口拒，并说出是哪一家 DRM。
///
/// 三层判定看的是容器本身（zip 完好、`mimetype` 也对），只有读到
/// `META-INF/encryption.xml` 才知道那些字节永远解不开 —— 放它进来的下场是架上多一本
/// 能翻页、每页都是乱码的书，而它重扫一万次也不会变好。
#[test]
fn an_encrypted_epub_is_refused_and_says_which_method() {
    let dir = tempfile::tempdir().unwrap();
    let encrypted = |file: &str, algorithm: &str| {
        let path = dir.path().join(file);
        let opf = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>加密书</dc:title></metadata>
  <manifest><item id="c1" href="Text/chap1.xhtml" media-type="application/xhtml+xml"/></manifest>
  <spine><itemref idref="c1"/></spine>
</package>"#;
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <EncryptedData><EncryptionMethod Algorithm="{algorithm}"/></EncryptedData>
</encryption>"#
        );
        write_epub(
            &path,
            &[
                ("mimetype", b"application/epub+zip"),
                ("META-INF/container.xml", CONTAINER.as_bytes()),
                ("OEBPS/content.opf", opf.as_bytes()),
                ("META-INF/encryption.xml", xml.as_bytes()),
                (
                    "OEBPS/Text/chap1.xhtml",
                    "<html><body><p>正文</p></body></html>".as_bytes(),
                ),
            ],
        );
        path
    };

    let drm = encrypted(
        "drm.epub",
        "http://www.adobe.com/DRM/method/2008.09.adbe.xmldsig-hmac#sha256",
    );
    let err = read_with(&drm, false).expect_err("DRM 的书要拒掉");
    let crate::Error::Encrypted { method, .. } = &err else {
        panic!("该是 Encrypted，实际 {err:?}");
    };
    assert!(method.contains("adobe.com/DRM"), "要说出是哪一家：{method}");
    // 报表按类别分组靠的是这个（`ImportReject`），不是文案匹配。
    use crate::infra::error::ImportReject;
    assert_eq!(err.import_reject(), Some(ImportReject::Encrypted));

    // 字体去混淆不是 DRM：EPUB 规范允许，任何读者都解得开，正文照读。
    for (name, algorithm) in [
        ("font-idpf.epub", "http://www.idpf.org/2008/embedding"),
        ("font-adobe.epub", "http://ns.adobe.com/pdf/enc#RC"),
    ] {
        let book = encrypted(name, algorithm);
        let flow = flow_of(&book);
        assert_eq!(
            flow.chapters.len(),
            1,
            "{name}：只是字体扰动，不该拒掉整本书"
        );
    }

    // 写了 encryption.xml 但一条方法都没有：没有要解的东西，照读。
    let empty = {
        let path = dir.path().join("empty-enc.epub");
        write_epub(
            &path,
            &[
                ("mimetype", b"application/epub+zip"),
                ("META-INF/container.xml", CONTAINER.as_bytes()),
                (
                    "OEBPS/content.opf",
                    r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>空加密表</dc:title></metadata>
  <manifest><item id="c1" href="Text/chap1.xhtml" media-type="application/xhtml+xml"/></manifest>
  <spine><itemref idref="c1"/></spine>
</package>"#
                        .as_bytes(),
                ),
                (
                    "META-INF/encryption.xml",
                    r#"<?xml version="1.0"?><encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container"/>"#
                        .as_bytes(),
                ),
                (
                    "OEBPS/Text/chap1.xhtml",
                    "<html><body><p>正文</p></body></html>".as_bytes(),
                ),
            ],
        );
        path
    };
    assert_eq!(flow_of(&empty).chapters.len(), 1);
}

// ——————————————————————————————————————————————————————————————
// EPUB3 的 `<item fallback="…">`：作者把「这一版解不动的那一份」和同一张图的位图版本
// 一起放进容器，读者就该看到位图那一份。
// ——————————————————————————————————————————————————————————————

/// 一份最小 SVG：合法图片，但工作区没给 `image` 开 SVG 解码器，所以它是「解不动」那一档。
const SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 4 2"><rect/></svg>"#;

/// 正文里挂着 `<img src="../Images/chart.svg">` 的一章书，manifest 的图片那几条由参数给。
///
/// 键（`ImageRes::href`）来自正文那串 `src`，去处（`source`）由 manifest 的降级链决定 ——
/// 夹具把两侧分开给，才测得出「换了去处而键没换」这一件真正要钉的事：正文一个字都不许改
/// （锚点坐标建在源文本上，`plans/Locator.md`）。
fn epub_chart(dir: &tempfile::TempDir, items: &str, entries: &[(&str, &[u8])]) -> PathBuf {
    let path = dir.path().join("chart.epub");
    let opf = format!(
        r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>带图的书</dc:title></metadata>
  <manifest>
    <item id="c1" href="Text/chap1.xhtml" media-type="application/xhtml+xml"/>
{items}
  </manifest>
  <spine><itemref idref="c1"/></spine>
</package>"#
    );
    let chapter = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p>前<img src="../Images/chart.svg" alt="图"/>后</p></body></html>"#;
    let mut all: Vec<(&str, &[u8])> = vec![
        ("mimetype", b"application/epub+zip"),
        ("META-INF/container.xml", CONTAINER.as_bytes()),
        ("OEBPS/content.opf", opf.as_bytes()),
        ("OEBPS/Text/chap1.xhtml", chapter.as_bytes()),
    ];
    all.extend_from_slice(entries);
    write_epub(&path, &all);
    path
}

/// 表里按正文那串 `src` 解出来的键去找那一条，找不到就把这一档的名字打出来。
fn row_for<'a>(
    images: &'a [crate::content::format::ImageRes],
    tail: &str,
    kind: &str,
) -> &'a crate::content::format::ImageRes {
    images
        .iter()
        .find(|i| i.href.ends_with(tail))
        .unwrap_or_else(|| panic!("{kind}：{tail} 不许从图片清单里消失"))
}

/// SVG 那份解不动、PNG 那份就在同一个容器里 —— 读者看到的就该是 PNG，而查表的键不许变。
///
/// 四处各钉一件事：键仍是正文的 `src`（排版与绘制都按它问表）、去处换成 PNG、
/// 声明的类型跟着换成 PNG（绘制那边拿它当字节头之外的第二判据）、尺寸从 PNG 的头部读
/// （不换的话拿到 `(0, 0)`，排版按 1:1 兜底，而字节根本解不出来 —— 那块地方全看不见）。
#[test]
fn an_svg_image_reads_the_png_it_falls_back_to() {
    let dir = tempfile::tempdir().unwrap();
    let png = png_bytes(40, 20);
    let path = epub_chart(
        &dir,
        r#"<item id="svg1" href="Images/chart.svg" media-type="image/svg+xml" fallback="png1"/>
    <item id="png1" href="Images/chart.png" media-type="image/png"/>"#,
        &[
            ("OEBPS/Images/chart.svg", SVG),
            ("OEBPS/Images/chart.png", &png),
        ],
    );
    let doc = doc_of(&path);
    let row = row_for(doc.images(), "chart.svg", "挂了两份的");
    assert_eq!(
        row.source, "OEBPS/Images/chart.png",
        "去处是降级链上那份位图：{row:?}"
    );
    assert_eq!(row.media_type, "image/png", "{row:?}");
    assert_eq!(
        (row.width, row.height),
        (40, 20),
        "尺寸从那份位图读：{row:?}"
    );
    // 界面只有这一条问法：按正文那串 `src` 解出来的键要字节（`ui/shared/images.rs`）。
    assert_eq!(
        doc.image_bytes("OEBPS/Images/chart.svg").as_deref(),
        Some(png.as_slice()),
        "按 svg 那条名字要到的就该是 PNG 的字节"
    );
}

/// 链可以经过一份同样解不动的：SVG → TIFF（工作区没开解码器）→ PNG。
///
/// 钉的是「找的是链上**第一份解得动的**」而不是「只看一跳」。真书里这条两跳的链常来自
/// 出版流水线：先给 SVG 挂一份 TIFF 当印刷源，再往下才是网页用的 PNG。
#[test]
fn the_fallback_chain_walks_past_another_undecodable_item() {
    let dir = tempfile::tempdir().unwrap();
    let png = png_bytes(8, 6);
    let path = epub_chart(
        &dir,
        r#"<item id="svg1" href="Images/chart.svg" media-type="image/svg+xml" fallback="tiff1"/>
    <item id="tiff1" href="Images/chart.tiff" media-type="image/tiff" fallback="png1"/>
    <item id="png1" href="Images/chart.png" media-type="image/png"/>"#,
        &[
            ("OEBPS/Images/chart.svg", SVG),
            ("OEBPS/Images/chart.tiff", b"II\x2a\x00 whatever"),
            ("OEBPS/Images/chart.png", &png),
        ],
    );
    let doc = doc_of(&path);
    let row = row_for(doc.images(), "chart.svg", "两跳链的");
    assert_eq!(row.source, "OEBPS/Images/chart.png", "{row:?}");
    assert_eq!((row.width, row.height), (8, 6), "{row:?}");
}

/// 三条「没有可用的降级路」各按原条目读，一条都不许把这张图从清单里丢掉。
///
/// 丢掉的症状比留一块空白更糟：版面不知道它占多大，于是那行字会在图到位时跳一下，
/// 而读者正读到那一行。
#[test]
fn an_unusable_fallback_leaves_the_declared_item_alone() {
    let cases = [
        (
            "环",
            r#"<item id="svg1" href="Images/chart.svg" media-type="image/svg+xml" fallback="svg2"/>
    <item id="svg2" href="Images/other.svg" media-type="image/svg+xml" fallback="svg1"/>"#,
        ),
        (
            "指着 manifest 里没有的 id",
            r#"<item id="svg1" href="Images/chart.svg" media-type="image/svg+xml" fallback="ghost"/>"#,
        ),
        (
            "根本没挂链",
            r#"<item id="svg1" href="Images/chart.svg" media-type="image/svg+xml"/>"#,
        ),
    ];
    for (name, items) in cases {
        let dir = tempfile::tempdir().unwrap();
        let path = epub_chart(
            &dir,
            items,
            &[
                ("OEBPS/Images/chart.svg", SVG),
                ("OEBPS/Images/other.svg", SVG),
            ],
        );
        let doc = doc_of(&path);
        let row = row_for(doc.images(), "chart.svg", name);
        assert_eq!(row.source, row.href, "{name}：没有可用的降级路就照原条目");
        assert_eq!(
            row.media_type, "image/svg+xml",
            "{name}：声明的类型不许被改写"
        );
        assert_eq!(
            (row.width, row.height),
            (0, 0),
            "{name}：SVG 拿不到固有尺寸，交给排版那条降级"
        );
        assert!(
            doc.image_bytes("OEBPS/Images/chart.svg").is_some(),
            "{name}：原条目那份字节照样取得到"
        );
    }
}

/// 整本的**翻页方向**来自 `<spine page-progression-direction>`，与正文往哪边流是两件事。
///
/// 四个 case 各钉一处：显式 `rtl` 要翻；`ltr` 与**属性缺席**都是左翻；而规范里的
/// `default` 这一档我们明知不完整也照样折成左翻 —— 它要求「由内容自己的方向定」，
/// 而开书那一趟只读索引、不回头嗅第一章的首强字符（`FlowDoc::progression_rtl` 那条
/// 注释写了代价：一本没写这属性的 RTL 书，正文照旧右起、点按区却还按左翻）。
/// 属性值的大小写与首尾空格也不该成为第二道门槛。
#[test]
fn the_spine_page_progression_direction_lands_on_the_flow_doc() {
    for (spine_attrs, want, why) in [
        (r#"page-progression-direction="rtl""#, true, "显式写了 rtl"),
        (
            r#"page-progression-direction="RTL""#,
            true,
            "值大小写不敏感",
        ),
        (
            "page-progression-direction=\" rtl \"",
            true,
            "首尾空格不算第二个门槛",
        ),
        (r#"page-progression-direction="ltr""#, false, "显式左翻"),
        (
            r#"page-progression-direction="default""#,
            false,
            "default 折成左翻（已知不完整）",
        ),
        ("", false, "属性缺席"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ppd.epub");
        let opf = format!(
            r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>翻页方向</dc:title></metadata>
  <manifest><item id="c1" href="Text/chap1.xhtml" media-type="application/xhtml+xml"/></manifest>
  <spine {spine_attrs}><itemref idref="c1"/></spine>
</package>"#
        );
        write_epub(
            &path,
            &[
                ("mimetype", b"application/epub+zip"),
                ("META-INF/container.xml", CONTAINER.as_bytes()),
                ("OEBPS/content.opf", opf.as_bytes()),
                (
                    "OEBPS/Text/chap1.xhtml",
                    "<html><body><p>正文</p></body></html>".as_bytes(),
                ),
            ],
        );
        let flow = flow_of(&path);
        assert_eq!(
            flow.progression_rtl, want,
            "{why}（spine 写的是 {spine_attrs:?}）"
        );
    }
}
