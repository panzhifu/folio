//! 测试用的合成 EPUB 夹具。
//!
//! 只给跨 crate 的测试用（core 的搜索那一组要一本 28 章的书），不属于公开 API。
//! 生产路径不会调到这里；`publish = false` 所以也不进发布物。

use std::path::{Path, PathBuf};

pub fn write_epub(path: &Path, entries: &[(&str, &[u8])]) {
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

pub const CONTAINER: &str = r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;

/// 出版书形状的那一份：28 章、25 条目录、开头带 `@charset` 的一份 CSS，
/// 导航只有 `toc.ncx`（EPUB 2.0，calibre 导出就是这一档）。
///
/// 这些数字是在本机那本不入库的样本里量出来的（`tests/fixtures/README.md`），
/// 但容器本身**当场合成** —— 一条要每次 `cargo test` 与 CI 都真跑的测试，
/// 输入必须在仓库里。它验的不是「能不能打开」，而是合成小样本装不下的那几个形状：
/// manifest 与 spine 反着排、几十条 NCX 条目、条目里的 `#fragment`、章数多于目录数。
///
/// `pub(crate)` 是给 `content::scan` 那一层的逐章对拍用的：同一份合成书只写一处。
pub fn published_shape_epub(dir: &Path) -> PathBuf {
    const CHAPTERS: usize = 28;
    const TOC_ENTRIES: usize = 25;
    let path = dir.join("published-shape.epub");

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
        ("OEBPS/content.opf".to_string(), opf.as_bytes().to_vec()),
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
