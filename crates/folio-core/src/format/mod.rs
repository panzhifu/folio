use std::path::Path;

use serde::{Deserialize, Serialize};

pub mod epub;
pub mod markdown;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Format {
    Markdown,
    Text,
    Epub,
}

impl Format {
    pub fn as_str(self) -> &'static str {
        match self {
            Format::Markdown => "markdown",
            Format::Text => "text",
            Format::Epub => "epub",
        }
    }

    pub fn parse(s: &str) -> Option<Format> {
        match s {
            "markdown" => Some(Format::Markdown),
            "text" => Some(Format::Text),
            "epub" => Some(Format::Epub),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocMeta {
    pub title: String,
    pub authors: Vec<String>,
    pub format: Format,
    pub size: u64,
}

/// 可重排文档的一章。`source` 是规范化后的源文本，
/// 锚点与阅读进度都以它的 **字符偏移** 为坐标系（见 `crate::Locator`），
/// 因此字号、行距、窗口宽度怎么变，位置都不会失效。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chapter {
    pub title: String,
    pub source: String,
    pub spine_href: String,
}

impl Chapter {
    pub fn char_len(&self) -> usize {
        self.source.chars().count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TocEntry {
    pub title: String,
    /// 解析后的绝对容器路径（已百分号解码、去 fragment），与 `Chapter::spine_href` 同形。
    pub href: String,
    pub children: Vec<TocEntry>,
}

/// 可重排文档。`css` 是全书作者样式按 manifest 顺序拼起来的一份，
/// `toc` 与 `cover` 来自 OPF；这三项都是**文档级**数据，所以和章节分开存。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FlowDoc {
    pub chapters: Vec<Chapter>,
    pub css: String,
    pub toc: Vec<TocEntry>,
    pub cover: Option<Vec<u8>>,
}

impl FlowDoc {
    pub fn char_len(&self) -> usize {
        self.chapters.iter().map(Chapter::char_len).sum()
    }
}

/// 定版式文档（PDF / 漫画）在 M7 接入时新增 `Fixed` 分支。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    Reflow(FlowDoc),
}

pub trait FormatAdapter: Send + Sync {
    fn format(&self) -> Format;

    fn supports(&self, path: &Path) -> bool;

    fn read(&self, path: &Path) -> crate::Result<(DocMeta, Content)>;
}

pub fn adapters() -> Vec<Box<dyn FormatAdapter>> {
    vec![
        Box::new(epub::EpubAdapter),
        Box::new(markdown::MarkdownAdapter),
    ]
}

pub fn detect(path: &Path) -> Option<Format> {
    adapters()
        .iter()
        .find(|a| a.supports(path))
        .map(|a| a.format())
}

pub fn open(path: &Path) -> crate::Result<(DocMeta, Content)> {
    let adapter = adapters()
        .into_iter()
        .find(|a| a.supports(path))
        .ok_or_else(|| {
            crate::Error::UnsupportedFormat(
                path.file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            )
        })?;
    adapter.read(path)
}
