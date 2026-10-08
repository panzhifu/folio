use std::path::Path;

use super::{Chapter, Content, DocMeta, FlowDoc, Format, FormatAdapter};
use crate::Result;
use crate::error::IoResultExt as _;

pub struct MarkdownAdapter;

impl FormatAdapter for MarkdownAdapter {
    fn format(&self) -> Format {
        Format::Markdown
    }

    fn supports(&self, path: &Path) -> bool {
        matches!(extension(path).as_deref(), Some("md" | "markdown" | "txt"))
    }

    fn read(&self, path: &Path) -> Result<(DocMeta, Content)> {
        let raw = std::fs::read_to_string(path).io_path(path)?;
        let source = normalize(&raw);
        let plain = matches!(extension(path).as_deref(), Some("txt"));
        let chapters = if plain {
            vec![Chapter {
                title: stem(path),
                spine_href: String::new(),
                source: source.clone(),
            }]
        } else {
            split_chapters(&source, &stem(path))
        };
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
