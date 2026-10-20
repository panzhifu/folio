//! Markdown 那一族（`.md` / `.markdown`）：按 ATX 标题切章，书名取第一个一级标题。
//!
//! 这一族比纯文本多出来的只有「怎么切」与「书名从哪儿来」两处，其余那条链
//! （读成文本 → 规范化 → 折成章表 → 包装）住在 [`super::text`] 且**只有一份** ——
//! 那里面 `normalize` 的产物就是锚点坐标系，各写一份迟早会漂，漂的症状是已存批注
//! 与进度整体错位。分成两个 provider 而不是一个 provider 按扩展名返回不同格式，
//! 是因为 `DocumentProvider::format()` 是**身份**：一个 provider 只能有一个 `Format`，
//! 而 `books.format` 那一列要把 Markdown 与纯文本分开。

use std::path::Path;

use super::{Chapter, ChapterLocator, Content, DocMeta, DocumentProvider, Format};
use crate::Result;
use crate::content::format::text::{PlainFamily, read_with, section_source};

pub struct MarkdownAdapter;

impl DocumentProvider for MarkdownAdapter {
    fn format(&self) -> Format {
        Format::Markdown
    }

    fn supports(&self, path: &Path) -> bool {
        matches!(
            super::extension_of(path).as_deref(),
            Some("md" | "markdown")
        )
    }

    /// 只分章、**不包装**：正文等翻开那一章再取。
    fn index(&self, path: &Path) -> Result<(DocMeta, Content)> {
        read_with(path, &MARKDOWN, false)
    }

    /// 单文件容器里的第 N 章：重读一遍、重分一次章，取第 N 段。
    /// 分章规则是确定的，所以同一个序号每次都指到同一章。
    fn chapter_source(&self, path: &Path, at: &ChapterLocator) -> Result<String> {
        section_source(path, &MARKDOWN, at)
    }

    fn read(&self, path: &Path) -> Result<(DocMeta, Content)> {
        read_with(path, &MARKDOWN, true)
    }
}

/// Markdown 相对那条链的两处差异：按 1–2 级 ATX 标题切，书名取第一个一级标题。
///
/// 目录那一格交回空表：这一族的章表**就是**结构（章名即标题），再抄一份进 `FlowDoc.toc`
/// 会多出两份「这本书有哪些章」的说法，而它们迟早要漂。
const MARKDOWN: PlainFamily = PlainFamily {
    format: Format::Markdown,
    split: split_on_headings,
    info: book_info_from_heading,
    toc: |_| Vec::new(),
};

/// Markdown 的书目只有书名那一格（第一个一级标题），作者与简介**这族没有地方可说**：
/// 单个 `.md` 文件里既没有 OPF 也没有 `<meta>`，硬猜只会造出一条不存在的事实。
fn book_info_from_heading(
    source: &str,
    stem: &str,
) -> crate::content::format::novel::detect::BookInfo {
    crate::content::format::novel::detect::BookInfo {
        title: first_heading(source).unwrap_or_else(|| stem.to_owned()),
        authors: Vec::new(),
        description: String::new(),
    }
}

/// 在每个 1–2 级 ATX 标题前分章，标题归属于其后的内容。
fn split_on_headings(source: &str, fallback_title: &str) -> Vec<Chapter> {
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
    //! Markdown 适配器：分章、标题回落、代码围栏里的 `#` 不算标题。黑盒 ——
    //! 把内容写进临时 `.md`，经公开的 `open()` 读回来断言，只依赖公开 API。

    use crate::content::format::text::test_support::{
        opened, opened_bytes, utf16le, visible, without_whitespace,
    };

    /// UTF-16 这一档通到 Markdown 的正文：解出来的章表与同一份内容的 UTF-8 版
    /// **逐字相同**，包括分章、章名与 `#` 有没有被剥掉。
    #[test]
    fn a_utf16_markdown_lands_on_the_same_chapters_as_its_utf8_twin() {
        let src = "# 书名\n前言\n## 第一章\n正文一\n## 第二章\n正文二\n";
        let (utf8_meta, utf8) = opened("d.md", src);
        let (u16_meta, u16) = opened_bytes("d.md", &utf16le(src));
        assert_eq!(u16_meta.title, utf8_meta.title, "书名要一样");
        assert_eq!(
            u16.chapters
                .iter()
                .map(|c| c.title.as_str())
                .collect::<Vec<_>>(),
            utf8.chapters
                .iter()
                .map(|c| c.title.as_str())
                .collect::<Vec<_>>(),
            "章数与每一章的章名都该与 UTF-8 那份相同"
        );
        for (a, b) in u16.chapters.iter().zip(&utf8.chapters) {
            assert_eq!(a.source(), b.source(), "正文要逐字相同：{:?}", a.title);
        }
        // 独立验一次内容而不是只验「两趟一致」：两趟都坏也会一致。
        assert!(
            !u16.chapters[1].source().contains('\u{fffd}')
                && u16.chapters[1].source().contains("正文一"),
            "UTF-16 不该退成一排问号：{}",
            u16.chapters[1].source()
        );
    }

    /// **纯文本没有声明位，所以那一档对它故意不生效**（`format::charset` 里那条）。
    /// 一本合法 UTF-8 的技术书在前 1 KB 里出现 `charset=gbk` 那样一串字（真书里就长在
    /// 代码清单里），按 HTML 那套「文件自己说的优先」会把整本书解成 GBK —— 症状是通屏
    /// 像汉字的乱码，而判定与导入全程报「成功」。这一条走 `.md`，与 `.txt` 那条同判据。
    #[test]
    fn a_plain_file_does_not_obey_a_charset_line_inside_it() {
        let src = "```html\n<meta http-equiv=\"Content-Type\" content=\"text/html; charset=gbk\">\n```\n\n# 第一章\n这一行是合法 UTF-8 的正文：大雪封山的那一夜\n";
        let (meta, flow) = opened("note.md", src);
        assert_eq!(meta.title, "第一章", "标题要按 UTF-8 解出来，不是乱码");
        let body = flow
            .chapters
            .iter()
            .map(|c| visible(c.source()))
            .collect::<String>();
        assert!(
            body.contains("大雪封山的那一夜") && !body.contains('\u{fffd}'),
            "被声明劫持过的 UTF-8 文本会在这里露出来：{body}"
        );
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
        let joined: String = flow.chapters.iter().map(|c| visible(c.source())).collect();
        let expected = crate::content::format::xhtml::atx_markers_removed(src);
        assert_eq!(without_whitespace(&joined), without_whitespace(&expected));
    }

    /// 分章要保证的那件事本来就是「这一章的正文以它自己的标题开头」——
    /// 直接从正文看，不看位置（`spine_href` 里那个字节偏移早已不外带）。
    #[test]
    fn each_chapter_starts_at_its_own_heading() {
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

    /// 这条只是让上面那句「与 `.txt` 同判据」不是一句空话：两族共用同一份规范化，
    /// 所以 CRLF 与多余空行在 `.md` 上的结果与在 `.txt` 上一字不差。
    #[test]
    fn normalization_is_the_same_on_both_families() {
        let body = "a\r\n\r\n\r\n\r\nb\n\n";
        let (_, md) = opened("d.md", body);
        let (_, txt) = opened("d.txt", body);
        assert_eq!(
            visible(md.chapters[0].source()),
            visible(txt.chapters[0].source()),
            "规范化只有一份，两族的结果必须相同"
        );
    }
}
