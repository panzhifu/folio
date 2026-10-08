//! EPUB 容器读取。测的是一份**合成**的书（`tests/fixtures/oss/minimal-zh.epub`），
//! 故意把 manifest 顺序、spine 顺序、文件名里的空格、nav 里的 fragment
//! 都设成会出错的样子。

use std::path::{Path, PathBuf};

use super::*;
use crate::css::computed::{ComputedStyle, TextAlign, cascade};
use crate::css::{Origin, RuleSet};
use crate::format::detect;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/oss/minimal-zh.epub")
}

fn opened() -> (DocMeta, FlowDoc) {
    let (meta, content) = read(&fixture()).unwrap();
    // Content 目前只有 Reflow 一个分支，模式是不可反驳的。
    let Content::Reflow(flow) = content;
    (meta, flow)
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
        resolve("OEBPS/Text/", "../Styles/style.css"),
        "OEBPS/Styles/style.css"
    );
    assert_eq!(
        resolve("OEBPS/", "./Text/chap1.xhtml#a"),
        "OEBPS/Text/chap1.xhtml"
    );
    assert_eq!(
        resolve("OEBPS/Text/", "chap%202.xhtml"),
        "OEBPS/Text/chap 2.xhtml"
    );
    assert_eq!(resolve("", "/OEBPS/a.xhtml"), "OEBPS/a.xhtml");
    // 逃出容器的 ../ 只会把栈弹空，不会造出绝对路径。
    assert_eq!(resolve("OEBPS/", "../../../../etc/passwd"), "etc/passwd");
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
    assert!(flow.chapters[0].source.contains("夜色从花园那头压过来"));
    // nav 文档本身是 spine 里的一节，但它不该变成「一章」。
    assert_eq!(
        flow.chapters
            .iter()
            .filter(|c| c.spine_href.contains("Nav"))
            .count(),
        0
    );
}

/// 两份 CSS 都在 manifest 里，拼接顺序就是声明顺序。
#[test]
fn css_is_concatenated_in_manifest_order() {
    let (_, flow) = opened();
    let extra = flow.css.find("body.cover").expect("应有 cover.css 的内容");
    let style = flow.css.find("text-indent").expect("应有 style.css 的内容");
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
    assert_eq!(
        flow.toc[2].title, "第三章 空白",
        "nav 标签里的多余空格要压掉"
    );
}

/// 中文书的排版地基：这三条样式必须能从合成书的 CSS 里落到元素上。
#[test]
fn the_books_own_css_reaches_the_elements() {
    let (_, flow) = opened();
    let rules = RuleSet::parse(&flow.css, Origin::Author);
    assert_eq!(rules.ignored, 1, "只有 @charset 是清单外的语句型 at-rule");
    let dom = crate::dom::Dom::parse(flow.chapters[0].source.as_bytes());
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
fn heading_fallback_reads_the_first_heading_of_a_chapter() {
    assert_eq!(
        heading_of("<html><body><h2>第二章 庭院</h2><p>正文</p></body></html>"),
        Some("第二章 庭院".into())
    );
    assert_eq!(
        heading_of("<html><body><p>没有标题</p></body></html>"),
        None
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
        read(&bad).is_err(),
        "读不出 container.xml 必须报错，不能给一本空书"
    );
}

/// 真实出版书的校验。默认 `--ignored`：CI 上没有这份受版权保护的文件，
/// 一个「文件不在就算通过」的测试迟早会变成假绿灯。
/// 本机跑法：`cargo test -p folio-core -- --ignored real_book`
#[test]
#[ignore = "需要 tests/fixtures/local 里的本机版权书，不入库"]
fn real_book_opens_and_has_readable_chapters() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/local/shejie-mauriac.epub");
    let Ok(path) = path.canonicalize() else {
        panic!("没有本机测试书：{path:?}");
    };
    let (meta, content) = read(&path).unwrap();
    let Content::Reflow(flow) = content;
    eprintln!(
        "{} ｜ {} 章 ｜ CSS {} 字节 ｜ 目录 {} 条 ｜ 封面 {:?}",
        meta.title,
        flow.chapters.len(),
        flow.css.len(),
        flow.toc.len(),
        flow.cover.as_ref().map(|c| c.len())
    );
    assert!(flow.chapters.len() >= 5, "一本正常书不该只切出几章");
    assert!(
        flow.chapters
            .iter()
            .any(|c| c.source.contains("charset") || c.source.contains("<p")),
        "章节源文本里应有正文标记"
    );
    assert!(!flow.css.is_empty(), "出版书一定带 CSS");
    // 目录条目要能对上章节，否则侧栏点击无处可跳。
    let matched = flow
        .toc
        .iter()
        .filter(|e| flow.chapters.iter().any(|c| c.spine_href == e.href))
        .count();
    eprintln!("目录 {} 条，其中 {} 条对上 spine", flow.toc.len(), matched);
    assert!(
        matched * 2 >= flow.toc.len(),
        "过半目录条目对不上章节：href 解析有 bug"
    );
}
