use std::path::Path;

use super::{Chapter, ChapterLocator, Content, DocMeta, DocumentProvider, FlowDoc, Format};
use crate::Result;
use crate::infra::error::IoResultExt as _;

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

    /// 只分章、**不包装**：正文等翻开那一章再取。
    fn index(&self, path: &Path) -> Result<(DocMeta, Content)> {
        read(path, false, false)
    }

    /// 单文件容器里的第 N 章：重读一遍、重分一次章，取第 N 段。
    /// 分章规则是确定的，所以同一个序号每次都指到同一章。
    fn chapter_source(&self, path: &Path, at: &ChapterLocator) -> Result<String> {
        section_source(path, false, at)
    }

    fn read(&self, path: &Path) -> Result<(DocMeta, Content)> {
        read(path, false, true)
    }
}

impl DocumentProvider for PlainTextAdapter {
    fn format(&self) -> Format {
        Format::Text
    }

    fn supports(&self, path: &Path) -> bool {
        matches!(extension(path).as_deref(), Some("txt"))
    }

    fn index(&self, path: &Path) -> Result<(DocMeta, Content)> {
        read(path, true, false)
    }

    fn chapter_source(&self, path: &Path, at: &ChapterLocator) -> Result<String> {
        section_source(path, true, at)
    }

    fn read(&self, path: &Path) -> Result<(DocMeta, Content)> {
        read(path, true, true)
    }
}

/// 读一遍、规范化、分章。返回 (书目, 规范化后的全文, 章表[纯文本])。
///
/// 分章本来就得把整份文本过一遍（标题在行里），所以「只读索引」这一趟省下的是
/// **包装**（`from_plain_text`）与把正文搬来搬去，不是读盘 —— 单文件格式的
/// I/O 是躲不掉的。
fn load(path: &Path, plain: bool) -> Result<(DocMeta, String, Vec<Chapter>)> {
    let raw = std::fs::read_to_string(path).io_path(path)?;
    let source = normalize(&raw);
    let chapters = if plain {
        vec![Chapter::inline(stem(path), source.clone())]
    } else {
        split_chapters(&source, &stem(path))
    };
    let title = first_heading(&source).unwrap_or_else(|| stem(path));
    let meta = DocMeta {
        title,
        authors: Vec::new(),
        format: if plain {
            Format::Text
        } else {
            Format::Markdown
        },
        size: source.len() as u64,
    };
    Ok((meta, source, chapters))
}

/// 把一章的纯文本包成引擎就绪的 XHTML。
///
/// 包装放在这里而不是界面里：正文的契约是「引擎就绪」，界面就不必知道哪个格式
/// 需要包一层（见 [`super::xhtml`] 的模块说明）。
fn wrap(chapter: Chapter) -> Chapter {
    let wrapped = super::xhtml::from_plain_text(chapter.source());
    Chapter::inline(chapter.title.clone(), wrapped)
}

/// 索引那一趟：章表只留标题与位置，正文等翻开那一章再包。
///
/// **字数在这一趟算**（`chapters.char_len`，见 `plans/导入管线.md` §八 第 3 条）：
/// 单文件格式的 I/O 本来就躲不掉（分章要把整份文本过一遍），所以这一趟的正文
/// 已经在手上，数一遍是白拿的。数的是**包装之后**那份 XHTML 的可见文本 ——
/// 与 `Chapter::inline` 同一条口径，也与引擎真正吃到的东西同一份。
fn read(path: &Path, plain: bool, with_text: bool) -> Result<(DocMeta, Content)> {
    let (meta, _source, chapters) = load(path, plain)?;
    let chapters = chapters
        .into_iter()
        .enumerate()
        .map(|(ix, chapter)| {
            if with_text {
                wrap(chapter)
            } else {
                let chars = chapter.char_len();
                Chapter::section(chapter.title.clone(), ix).with_char_len(chars)
            }
        })
        .collect();
    Ok((
        meta,
        Content::Reflow(FlowDoc {
            chapters,
            ..Default::default()
        }),
    ))
}

/// [`ChapterLocator::Section`] 那一路：重读整本、重分一次章，取第 N 段并包装。
///
/// 每次翻章都要过一遍全文件 —— 阅读会话按章缓存，所以一本书里一章只付一次
/// （见 `reader::Book::chapter_text`）。
fn section_source(path: &Path, plain: bool, at: &ChapterLocator) -> Result<String> {
    let index = match at {
        ChapterLocator::Section(index) => *index,
        other => {
            return Err(crate::Error::ChapterUnavailable {
                path: path.to_path_buf(),
                index: 0,
                at: format!("{other:?}"),
            });
        }
    };
    let (_, _, chapters) = load(path, plain)?;
    let text = chapters
        .get(index)
        .map(|c| super::xhtml::from_plain_text(c.source()))
        .ok_or_else(|| crate::Error::ChapterUnavailable {
            path: path.to_path_buf(),
            index,
            at: format!("{at:?}"),
        })?;
    Ok(text)
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
            flush(&mut buf, &mut title, "前言", &mut chapters);
            title = Some(heading_title);
            saw_heading = true;
        }
        buf.push_str(line);
        buf.push('\n');
    }
    // 收尾：全文一个标题都没有时，章名回落到文件名。
    let tail_default = if saw_heading {
        "前言"
    } else {
        fallback_title
    };
    flush(&mut buf, &mut title, tail_default, &mut chapters);

    if chapters.is_empty() {
        chapters.push(Chapter::inline(fallback_title, source));
    }
    chapters
}

fn flush(
    buf: &mut String,
    title: &mut Option<String>,
    default_title: &str,
    chapters: &mut Vec<Chapter>,
) {
    let text = std::mem::take(buf);
    if text.trim().is_empty() {
        return;
    }
    chapters.push(Chapter::inline(
        title.take().unwrap_or_else(|| default_title.to_string()),
        text,
    ));
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

    use crate::content::anchor::TextMap;
    use crate::content::dom::Dom;
    use crate::content::format::{Chapter, DocMeta, FlowDoc};

    fn opened(name: &str, body: &str) -> (DocMeta, FlowDoc) {
        let dir = tempfile::tempdir().unwrap();
        let path: PathBuf = dir.path().join(name);
        std::fs::write(&path, body).unwrap();
        let doc = crate::content::document::open(&path).expect("临时 .md 应能读入");
        // `open` 只给索引；这一层要断言的绝大多数是正文，按位置取回来。
        let mut flow = doc.flow().expect("该读成可重排").clone();
        for (ix, chapter) in flow.chapters.iter_mut().enumerate() {
            let text = doc.chapter_source(ix).expect("正文该取得到");
            *chapter = Chapter::inline(chapter.title.clone(), text);
        }
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
        assert_eq!(flow.chapters[0].source(), "<p>a</p>\n<p>b</p>\n");
    }

    #[test]
    fn splits_on_h1_and_h2() {
        let src = "# 书名\n前言\n## 第一章\n正文一\n## 第二章\n正文二\n";
        let (_, flow) = opened("d.md", src);
        assert_eq!(flow.chapters.len(), 3);
        assert_eq!(flow.chapters[0].title, "书名");
        assert_eq!(flow.chapters[1].title, "第一章");
        assert!(
            flow.chapters[1].source().starts_with("<h2>第一章</h2>"),
            "标题要包成 `<h2>`：{}",
            flow.chapters[1].source()
        );
        assert!(flow.chapters[1].source().contains("正文一"));
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
        assert!(flow.chapters[0].source().contains("# not a heading"));
    }

    /// 分章不能丢字符，也不能凭空造字符 —— 把各章各自的可见文本拼起来，
    /// 去掉空白后必须等于原文。包装成 XHTML 之后这条不变量只能这样验。
    #[test]
    fn chapter_sources_reconcatenate_losslessly() {
        let src = "# A\ntext\n## B\nmore\n";
        let (_, flow) = opened("d.md", src);
        let joined: String = flow.chapters.iter().map(|c| visible(&c.source())).collect();
        let expected = crate::content::format::xhtml::atx_markers_removed(src);
        assert_eq!(without_whitespace(&joined), without_whitespace(&expected));
    }

    /// `spine_href` 是**包装之前**那份规范化纯文本里的字节偏移（分章时算的），
    /// 所以它指向的是原文里这一章自己的标题行。
    #[test]
    fn each_chapter_starts_at_its_own_heading() {
        // 以前这条靠 `spine_href` 里塞的字节偏移看切点（`#<offset>`）。
        // 正文出库之后那个字段不再外带偏移，而分章要保证的那件事本来就是
        // 「这一章的正文以它自己的标题开头」—— 直接从正文看。
        let src = "# A\ntext\n## B\nmore\n";
        let (_, flow) = opened("d.md", src);
        assert_eq!(flow.chapters.len(), 2);
        for c in &flow.chapters {
            let first = c.source().lines().next().unwrap_or_default();
            assert!(
                first.contains(&c.title),
                "每一章该以它自己的标题开头：{first:?} vs {:?}",
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
