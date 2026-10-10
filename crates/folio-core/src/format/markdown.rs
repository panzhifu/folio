use std::path::Path;

use super::{Chapter, Content, DocMeta, DocumentProvider, FlowDoc, Format};
use crate::Result;
use crate::error::IoResultExt as _;

/// Markdown 与纯文本是两种东西（[`Format::Markdown`] / [`Format::Text`]），但共用
/// 「读成字符串 → 规范化 → 分章（或不分）」这条链，所以住同一个模块。
///
/// 分成两个 provider、而不是一个 provider 按扩展名返回不同格式，是因为
/// `DocumentProvider::format()` 是**身份**：一个 provider 只能有一个 `Format`，
/// 而 `books.format` 那一列要能把 Markdown 与纯文本分开（它们的排版预处理其实不同：
/// 一个按 `#` 分章，一个整篇一章）。两边共用的是同一份 `read()`。
pub struct MarkdownAdapter;
pub struct PlainTextAdapter;

impl DocumentProvider for MarkdownAdapter {
    fn format(&self) -> Format {
        Format::Markdown
    }

    fn supports(&self, path: &Path) -> bool {
        matches!(extension(path).as_deref(), Some("md" | "markdown"))
    }

    fn read(&self, path: &Path) -> Result<(DocMeta, Content)> {
        read(path, false)
    }
}

impl DocumentProvider for PlainTextAdapter {
    fn format(&self) -> Format {
        Format::Text
    }

    fn supports(&self, path: &Path) -> bool {
        matches!(extension(path).as_deref(), Some("txt"))
    }

    fn read(&self, path: &Path) -> Result<(DocMeta, Content)> {
        read(path, true)
    }
}

fn read(path: &Path, plain: bool) -> Result<(DocMeta, Content)> {
    let raw = std::fs::read_to_string(path).io_path(path)?;
    let source = normalize(&raw);
    let mut chapters = if plain {
        vec![Chapter {
            title: stem(path),
            spine_href: String::new(),
            source: source.clone(),
        }]
    } else {
        split_chapters(&source, &stem(path))
    };
    // 归一化收到的是一份**纯文本**（分章也是按纯文本的行做的），而引擎只吃 XHTML。
    // 包装放在这里而不是界面里：`Chapter::source` 的契约是「引擎就绪」，
    // 界面就不必知道哪个格式需要包一层（见 `super::xhtml` 的模块说明）。
    for chapter in &mut chapters {
        chapter.source = super::xhtml::from_plain_text(&chapter.source);
    }
    let title = first_heading(&source).unwrap_or_else(|| stem(path));
    Ok((
        DocMeta {
            title,
            authors: Vec::new(),
            format: if plain {
                Format::Text
            } else {
                Format::Markdown
            },
            size: source.len() as u64,
        },
        Content::Reflow(FlowDoc {
            chapters,
            ..Default::default()
        }),
    ))
}

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "未命名".to_string())
}

/// 统一行尾、压掉多余空行。规范化必须在分章之前，
/// 因为锚点坐标系就是这份规范化文本，改动它会让已存标注整体漂移。
fn normalize(src: &str) -> String {
    let mut out = String::with_capacity(src.len() + 1);
    let mut blanks = 0usize;
    for line in src.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
        } else {
            blanks = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// 在每个 1–2 级 ATX 标题前分章，标题归属于其后的内容。
/// 每章的 `spine_href` 记下该章起始的字节偏移，供跳转与目录显示用。
fn split_chapters(source: &str, fallback_title: &str) -> Vec<Chapter> {
    let mut chapters = Vec::new();
    let mut buf = String::new();
    let mut title: Option<String> = None;
    let mut chapter_start = 0usize;
    let mut line_start = 0usize;
    let mut fence = false;
    let mut saw_heading = false;

    for line in source.lines() {
        let trimmed = line.trim_start();
        let is_fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        let heading = if !is_fence && !fence {
            atx_heading(line, 2)
        } else {
            None
        };
        if is_fence {
            fence = !fence;
        }

        if let Some(heading_title) = heading {
            // 由标题触发的收尾：这一段没有标题，说明它位于首个标题之前，是「前言」。
            flush(&mut buf, &mut title, chapter_start, "前言", &mut chapters);
            title = Some(heading_title);
            chapter_start = line_start;
            saw_heading = true;
        }
        buf.push_str(line);
        buf.push('\n');
        line_start += line.len() + 1;
    }
    // 收尾：全文一个标题都没有时，章名回落到文件名。
    let tail_default = if saw_heading {
        "前言"
    } else {
        fallback_title
    };
    flush(
        &mut buf,
        &mut title,
        chapter_start,
        tail_default,
        &mut chapters,
    );

    if chapters.is_empty() {
        chapters.push(Chapter {
            title: fallback_title.to_string(),
            source: source.to_string(),
            spine_href: String::new(),
        });
    }
    chapters
}

fn flush(
    buf: &mut String,
    title: &mut Option<String>,
    chapter_start: usize,
    default_title: &str,
    chapters: &mut Vec<Chapter>,
) {
    let text = std::mem::take(buf);
    if text.trim().is_empty() {
        return;
    }
    chapters.push(Chapter {
        title: title.take().unwrap_or_else(|| default_title.to_string()),
        spine_href: format!("#{chapter_start}"),
        source: text,
    });
}

/// `max_level` 以内的 ATX 标题返回其文字。
fn atx_heading(line: &str, max_level: usize) -> Option<String> {
    let hashes = line.trim_start().chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > max_level {
        return None;
    }
    let rest = &line.trim_start()[hashes..];
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    let text = rest.trim().trim_end_matches('#').trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn first_heading(source: &str) -> Option<String> {
    let mut fence = false;
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = !fence;
            continue;
        }
        if !fence && let Some(h) = atx_heading(line, 1) {
            return Some(h);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    //! Markdown 适配器：规范化、分章、标题回落。改为黑盒——把内容写进临时 `.md`，
    //! 经公开的 `open()` 读回来断言。只依赖公开 API。

    use std::path::PathBuf;

    use crate::anchor::TextMap;
    use crate::dom::Dom;
    use crate::format::{DocMeta, FlowDoc};

    fn opened(name: &str, body: &str) -> (DocMeta, FlowDoc) {
        let dir = tempfile::tempdir().unwrap();
        let path: PathBuf = dir.path().join(name);
        std::fs::write(&path, body).unwrap();
        let doc = crate::document::open(&path).expect("临时 .md 应能读入");
        let flow = doc.flow().expect("该读成可重排").clone();
        (doc.meta().clone(), flow)
    }

    /// 一段 XHTML 的可见文本。契约里最要紧的一条是「包装不改内容」，断言就得看可见文本，
    /// 而不是看标记 —— 后者一改包装口径就红，前者才钉得住真正的不变量。
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

    #[test]
    fn normalize_unifies_crlf_and_blank_runs() {
        let (_, flow) = opened("d.md", "a\r\n\r\n\r\n\r\nb\n\n");
        assert_eq!(flow.chapters.len(), 1);
        assert_eq!(flow.chapters[0].source, "<p>a</p>\n<p>b</p>\n");
    }

    #[test]
    fn splits_on_h1_and_h2() {
        let src = "# 书名\n前言\n## 第一章\n正文一\n## 第二章\n正文二\n";
        let (_, flow) = opened("d.md", src);
        assert_eq!(flow.chapters.len(), 3);
        assert_eq!(flow.chapters[0].title, "书名");
        assert_eq!(flow.chapters[1].title, "第一章");
        assert!(
            flow.chapters[1].source.starts_with("<h2>第一章</h2>"),
            "标题要包成 `<h2>`：{}",
            flow.chapters[1].source
        );
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

    /// 分章不能丢字符，也不能凭空造字符 —— 把各章各自的可见文本拼起来，
    /// 去掉空白后必须等于原文。包装成 XHTML 之后这条不变量只能这样验。
    #[test]
    fn chapter_sources_reconcatenate_losslessly() {
        let src = "# A\ntext\n## B\nmore\n";
        let (_, flow) = opened("d.md", src);
        let joined: String = flow.chapters.iter().map(|c| visible(&c.source)).collect();
        let expected = crate::format::xhtml::atx_markers_removed(src);
        assert_eq!(without_whitespace(&joined), without_whitespace(&expected));
    }

    /// `spine_href` 是**包装之前**那份规范化纯文本里的字节偏移（分章时算的），
    /// 所以它指向的是原文里这一章自己的标题行。
    #[test]
    fn spine_offsets_land_on_char_boundaries_and_their_own_heading() {
        let src = "# A\ntext\n## B\nmore\n";
        let (_, flow) = opened("d.md", src);
        assert_eq!(flow.chapters.len(), 2);
        for c in &flow.chapters {
            let byte: usize = c.spine_href.trim_start_matches('#').parse().unwrap();
            assert!(src.is_char_boundary(byte));
            let heading = src[byte..].lines().next().unwrap();
            assert!(heading.starts_with('#'), "{heading:?}");
            assert!(
                heading.contains(&c.title),
                "{heading:?} 该是 {} 的标题",
                c.title
            );
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
}
