//! **引擎输入契约**：`Chapter::source` 永远是引擎就绪的 XHTML。
//!
//! 这一层以前是破的 —— EPUB / FB2 / HTML 的章源是 XHTML，Markdown / TXT 是规范化
//! 纯文本，于是界面必须知道「哪个格式还要包一层」（`Format::source_is_xhtml` +
//! `chapter_xhtml`），`Format` 就这样渗进了排版链。包装下沉到 provider 之后，
//! 这条契约成为 provider 的**唯一输出约定**，界面与引擎都不再问格式。
//!
//! 这里用真实 provider 跑（不是构造 `Chapter`），因为要钉住的正是「provider 有没有
//! 真的守约」。三种源都覆盖：本来就是标记的（HTML / FB2 / EPUB）、纯文本的
//! （Markdown / TXT）、以及不进引擎的定版式（CBZ）。

use std::path::{Path, PathBuf};

use crate::anchor::TextMap;
use crate::dom::Dom;
use crate::format::{Content, Format, provider_for};

/// 一段 XHTML 的可见文本。契约里真正要保的是**内容**，不是标记 —— 断言只能看它。
fn visible(xhtml: &str) -> String {
    let dom = Dom::parse(xhtml.as_bytes());
    let body = dom
        .elements()
        .find(|(id, _)| dom.tag_name(*id) == Some("body"))
        .map_or(dom.document, |(id, _)| id);
    TextMap::build(&dom, body).text().to_string()
}

fn without_whitespace(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    path
}

/// EPUB 用仓库里那份合成书（`tests/fixtures/oss/minimal-zh.epub`），
/// 它是唯一一个「真容器」样本，其余格式临时造一份就够。
fn epub_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/oss/minimal-zh.epub")
}

fn fb2(dir: &Path) -> PathBuf {
    write(
        dir,
        "a.fb2",
        r#"<?xml version="1.0" encoding="utf-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0">
<description><title-info><book-title>契约</book-title></title-info></description>
<body><section><title><p>第一章</p></title><p>正文</p></section></body>
</FictionBook>"#,
    )
}

/// 可重排的每一种 provider：产物必须能直接进引擎。
///
/// 断言是「能解析 + body 里有可见文本」，不是「标签在白名单里」—— 引擎对不认识的
/// 标签本来就有 block 降级（`css::ua`），把清单写死会把合法的新标签误判成违约，
/// 而这条契约真正要保的是**引擎拿到的东西是标记、且内容没丢**。
#[test]
fn every_reflow_provider_hands_the_engine_parseable_xhtml() {
    let dir = tempfile::tempdir().unwrap();
    let cases: Vec<(&str, PathBuf)> = vec![
        (
            "markdown",
            write(
                dir.path(),
                "a.md",
                "# 第一章\n\n正文一\n\n## 第二章\n\n正文二\n",
            ),
        ),
        ("text", write(dir.path(), "a.txt", "第一段\n\n第二段\n")),
        (
            "html",
            write(
                dir.path(),
                "a.html",
                "<html><body><h1>标题</h1><p>正文</p></body></html>",
            ),
        ),
        ("fb2", fb2(dir.path())),
        ("epub", epub_fixture()),
    ];

    for (name, path) in cases {
        let provider = provider_for(&path).unwrap_or_else(|| panic!("{name} 该有 provider"));
        let (_, content) = provider
            .read(&path)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let Content::Reflow(flow) = content else {
            panic!("{name} 该读成可重排");
        };
        assert!(
            provider.format().as_str() == name,
            "{name} 的 format 对不上"
        );
        assert!(!flow.chapters.is_empty(), "{name} 一章都没有");
        for (ix, chapter) in flow.chapters.iter().enumerate() {
            assert!(
                chapter.source().contains('<'),
                "{name} 第 {ix} 章的 source 不是标记：{:?}",
                &chapter.source()[..chapter.source().len().min(120)]
            );
            let text = visible(&chapter.source());
            assert!(
                !text.trim().is_empty(),
                "{name} 第 {ix} 章解析出来没有可见文本"
            );
        }
    }
}

/// 纯文本那一支的核心不变量：**包装不增删一个可见字符**。
///
/// 这条断了，读者的进度与批注坐标会整体漂移 —— 而屏幕上什么都看不出来。
#[test]
fn the_plain_text_wrapper_preserves_visible_text() {
    let dir = tempfile::tempdir().unwrap();
    for (name, body) in [
        (
            "a.md",
            "# 第一章\n\n正文一\n第二行\n\n## 第二章\n\n正文二\n",
        ),
        ("a.txt", "第一段\n\n第二段\n"),
    ] {
        let path = write(dir.path(), name, body);
        let provider = provider_for(&path).unwrap();
        let (_, content) = provider.read(&path).unwrap();
        let Content::Reflow(flow) = content else {
            panic!("{name} 该读成可重排");
        };
        let joined: String = flow.chapters.iter().map(|c| visible(&c.source())).collect();
        let expected = super::xhtml::atx_markers_removed(body);
        assert_eq!(
            without_whitespace(&joined),
            without_whitespace(&expected),
            "{name} 的包装改了内容"
        );
    }
}

/// 定版式走另一族：产出的是页表，不是章源。它压根不该有 `Chapter`。
#[test]
fn the_fixed_family_hands_pages_not_chapters() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.cbz");
    let file = std::fs::File::create(&path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default();
    use std::io::Write as _;
    for (name, w, h) in [("1.png", 4u32, 6u32), ("10.png", 4, 6)] {
        zip.start_file(name, opts).unwrap();
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([1, 2, 3, 255]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        zip.write_all(&bytes).unwrap();
    }
    zip.finish().unwrap();

    let provider = provider_for(&path).unwrap();
    assert_eq!(provider.format(), Format::Cbz);
    let (_, content) = provider.read(&path).unwrap();
    let Content::Fixed(fixed) = content else {
        panic!("CBZ 该读成定版式");
    };
    assert_eq!(fixed.page_count(), 2);
    for page in &fixed.pages {
        assert!(page.image_href().is_some(), "CBZ 的页都是图源");
    }
}
