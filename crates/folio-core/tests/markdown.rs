//! Markdown 适配器：规范化、分章、标题回落。改为黑盒——把内容写进临时 `.md`，
//! 经公开的 `open()` 读回来断言。只依赖公开 API。

use std::path::PathBuf;

use folio_core::format::{Content, DocMeta, FlowDoc};
use folio_core::open;

fn opened(name: &str, body: &str) -> (DocMeta, FlowDoc) {
    let dir = tempfile::tempdir().unwrap();
    let path: PathBuf = dir.path().join(name);
    std::fs::write(&path, body).unwrap();
    let (meta, content) = open(&path).expect("临时 .md 应能读入");
    let Content::Reflow(flow) = content;
    (meta, flow)
}

#[test]
fn normalize_unifies_crlf_and_blank_runs() {
    let (_, flow) = opened("d.md", "a\r\n\r\n\r\n\r\nb\n\n");
    assert_eq!(flow.chapters.len(), 1);
    assert_eq!(flow.chapters[0].source, "a\n\nb\n");
}

#[test]
fn splits_on_h1_and_h2() {
    let src = "# 书名\n前言\n## 第一章\n正文一\n## 第二章\n正文二\n";
    let (_, flow) = opened("d.md", src);
    assert_eq!(flow.chapters.len(), 3);
    assert_eq!(flow.chapters[0].title, "书名");
    assert_eq!(flow.chapters[1].title, "第一章");
    assert!(flow.chapters[1].source.starts_with("## 第一章"));
    assert!(flow.chapters[1].source.contains("正文一"));
    assert_eq!(flow.chapters[2].title, "第二章");
}

#[test]
fn h3_does_not_split() {
    let (_, flow) = opened("d.md", "# A\n### deep\nbody\n");
    assert_eq!(flow.chapters.len(), 1);
}

#[test]
fn fence_content_is_not_a_heading() {
    let (_, flow) = opened("d.md", "# A\n```\n# not a heading\n```\nstill A\n");
    assert_eq!(flow.chapters.len(), 1);
    assert!(flow.chapters[0].source.contains("# not a heading"));
}

#[test]
fn chapter_sources_reconcatenate_losslessly() {
    let src = "# A\ntext\n## B\nmore\n";
    let (_, flow) = opened("d.md", src);
    let joined: String = flow.chapters.iter().map(|c| c.source.clone()).collect();
    assert_eq!(joined, src);
}

#[test]
fn spine_offsets_land_on_char_boundaries_and_their_own_heading() {
    let src = "# A\ntext\n## B\nmore\n";
    let (_, flow) = opened("d.md", src);
    assert_eq!(flow.chapters.len(), 2);
    for c in &flow.chapters {
        let byte: usize = c.spine_href.trim_start_matches('#').parse().unwrap();
        assert!(src.is_char_boundary(byte));
        assert!(src[byte..].starts_with(&c.source[..c.source.find('\n').unwrap()]));
    }
}

#[test]
fn doc_without_heading_is_one_chapter_named_after_the_file() {
    let (meta, flow) = opened("mydoc.md", "just a paragraph\n");
    assert_eq!(flow.chapters.len(), 1);
    assert_eq!(flow.chapters[0].title, "mydoc");
    assert_eq!(meta.title, "mydoc");
}

#[test]
fn content_before_the_first_heading_is_a_preface() {
    let (_, flow) = opened("d.md", "题记\n# 第一章\n正文\n");
    assert_eq!(flow.chapters.len(), 2);
    assert_eq!(flow.chapters[0].title, "前言");
    assert_eq!(flow.chapters[1].title, "第一章");
}

#[test]
fn the_document_title_ignores_fenced_content() {
    let (meta, _) = opened("d.md", "```\n# fake\n```\n# real\n");
    assert_eq!(meta.title, "real");
}
