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

/// 一本书里的一张图片。`href` 是解析后的**容器绝对路径**（与 `Chapter::spine_href`
/// 同形），排版与绘制都用它当键；`width`/`height` 是解码前的固有像素尺寸，
/// 读取时算好，布局期只查表。SVG 等取不到尺寸的格式为 0，由排版层降级。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRes {
    pub href: String,
    pub media_type: String,
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

/// 从字节头猜一张图的 mime。
///
/// 存在的理由是 `books.cover` 那一列**只有字节、没有媒体类型**：封面是从容器里按
/// OPF 的 cover id 捞出来的，那条记录不带 manifest 的 `media-type`。而 gpui 的
/// `Image::from_bytes` 必须显式给格式（它按格式挑解码器，给错了解不出来，
/// 一片空白且不报错），所以只能在画之前按 magic bytes 反推。
///
/// 只认根 `Cargo.toml` 给 `image` 开的那几种 feature；猜不出或没编译进来的格式返回
/// `None`，由调用方决定降级成什么占位，而不是假装它是 PNG 去试一次。
pub fn guess_media_type(bytes: &[u8]) -> Option<&'static str> {
    let guessed = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .format()?;
    match guessed {
        image::ImageFormat::Png => Some("image/png"),
        image::ImageFormat::Jpeg => Some("image/jpeg"),
        image::ImageFormat::WebP => Some("image/webp"),
        image::ImageFormat::Gif => Some("image/gif"),
        image::ImageFormat::Bmp => Some("image/bmp"),
        _ => None,
    }
}

/// 可重排文档。`css` 是全书作者样式按 manifest 顺序拼起来的一份，
/// `toc` 与 `cover` 来自 OPF；这三项都是**文档级**数据，所以和章节分开存。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FlowDoc {
    pub chapters: Vec<Chapter>,
    pub css: String,
    pub toc: Vec<TocEntry>,
    pub cover: Option<Vec<u8>>,
    /// 全书图片，按容器路径索引。封面若也在 manifest 里会一并收录。
    pub images: Vec<ImageRes>,
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

/// 把 `href` 相对 `base_dir`（通常是 OPF 或某章所在的目录）解析成容器内的规范路径。
/// 去掉 `#fragment`，做百分号解码，折叠 `./` 与 `../`。
///
/// 这是 epub 适配器与 app 侧图片 resolver 共用的同一份实现 —— 两边各写一次
/// 迟早会在「文件名里有空格 / 百分号编码」这类书上分叉。
pub fn resolve_href(base_dir: &str, href: &str) -> String {
    let href = href.split('#').next().unwrap_or(href).trim();
    let decoded = percent_decode(href);
    let merged = if decoded.starts_with('/') {
        decoded.trim_start_matches('/').to_string()
    } else if base_dir.is_empty() {
        decoded
    } else {
        format!("{base_dir}{decoded}")
    };
    let mut stack: Vec<&str> = Vec::new();
    for seg in merged.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    stack.join("/")
}

/// 路径所在目录，带结尾 `/`；没有目录时是空串。
pub fn dir_of(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/"),
        None => String::new(),
    }
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(n) = u8::from_str_radix(&input[i + 1..i + 3], 16)
        {
            out.push(n);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    // href 按 UTF-8 百分号编码；解出来的字节流不一定落在字符边界上，
    // 那种情况保留替换字符而不是整本读失败。
    String::from_utf8_lossy(&out).into_owned()
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
