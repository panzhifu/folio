//! 书籍图片索引：把 `<img src>` 解析成容器路径后查表，并按 href 给出可绘制的
//! gpui `Image`。
//!
//! 分两层是为了少复制：`BookImages` 在打开一本书时建一次，把每张图解码成
//! `Arc<Image>`（gpui 按字节哈希缓存，不会重复解码）；`ChapterImages` 只按当前章
//! 的目录解析相对路径，换章时重建，成本是一张 `HashMap` 的键拷贝。

use std::collections::HashMap;
use std::sync::Arc;

use folio_core::format::{ImageRes, dir_of, resolve_href};
use folio_core::layout::{ImageProvider, ResolvedImage};
use gpui_kit::{Image, ImageFormat};

struct ImageEntry {
    href: String,
    width: f32,
    height: f32,
    image: Arc<Image>,
}

/// 一本书的全部图片，按容器路径索引。
pub struct BookImages {
    entries: Vec<ImageEntry>,
    index: HashMap<String, usize>,
}

impl BookImages {
    pub fn new(images: &[ImageRes]) -> Self {
        let mut entries = Vec::with_capacity(images.len());
        let mut index = HashMap::with_capacity(images.len());
        for r in images {
            let idx = entries.len();
            index.entry(r.href.clone()).or_insert(idx);
            entries.push(ImageEntry {
                href: r.href.clone(),
                width: r.width as f32,
                height: r.height as f32,
                image: Arc::new(Image::from_bytes(format_of(&r.media_type), r.bytes.clone())),
            });
        }
        Self { entries, index }
    }

    fn get(&self, href: &str) -> Option<&ImageEntry> {
        self.index.get(href).map(|i| &self.entries[*i])
    }
}

/// mime → gpui 的 `ImageFormat`。认不出时按 PNG 试（最常见，且解不出时只是不显示，
/// 不会 panic）。
fn format_of(media_type: &str) -> ImageFormat {
    let mime = media_type.split(';').next().unwrap_or("").trim();
    ImageFormat::from_mime_type(mime).unwrap_or(ImageFormat::Png)
}

/// 一章的图片视图：`src` 相对本章目录解析成容器路径后查 `BookImages`。
pub struct ChapterImages {
    book: Arc<BookImages>,
    base_dir: String,
}

impl ChapterImages {
    pub fn new(book: Arc<BookImages>, spine_href: &str) -> Self {
        Self {
            book,
            base_dir: dir_of(spine_href),
        }
    }

    /// 绘制用：容器路径 → 可渲染的 `Image`。
    pub fn render_source(&self, href: &str) -> Option<Arc<Image>> {
        self.book.get(href).map(|e| e.image.clone())
    }
}

impl ImageProvider for ChapterImages {
    fn resolve(&self, src: &str) -> Option<ResolvedImage> {
        let href = resolve_href(&self.base_dir, src);
        let entry = self.book.get(&href)?;
        Some(ResolvedImage {
            href: entry.href.clone(),
            width: entry.width,
            height: entry.height,
        })
    }
}
