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

/// 封面字节 → 可画的 `Image`；格式认不出时返回 `None`。
///
/// 返回 `None` 而不是按 PNG 试一把，是因为 `Image::from_bytes` 之后 gpui 就按那个
/// 格式挑解码器（`platform.rs` 里 `to_image_data` 是 `match self.format`），
/// 给错格式的结果是**静默画空**：书架上是一块和「没有封面」长得一样的空白，
/// 而界面已经分不清这两件事了。认不出时调用方要画显式的占位块。
pub fn cover_image(bytes: &[u8]) -> Option<Arc<Image>> {
    let media_type = folio_core::format::guess_media_type(bytes)?;
    Some(Arc::new(Image::from_bytes(
        format_of(media_type),
        bytes.to_vec(),
    )))
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

    /// 一张图都没有时的索引（Markdown / TXT 恒如此，EPUB 无图文时也成立）。
    /// 任何 `src` 都解不出来，排版因此跳过每个 `<img>`，不会塌成空盒。
    pub fn empty() -> Self {
        Self {
            book: Arc::new(BookImages::new(&[])),
            base_dir: String::new(),
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

#[cfg(test)]
mod tests {
    //! 共用图片层里「封面那一小块」在宿主上跑得动的部分。
    //!
    //! 为什么单独一个文件：`ui/shared/images.rs` 排版侧的索引（`<img src>` → 容器路径）
    //! 由 `tests/shared_layout.rs` 管，而封面那条走的是另一件事 —— 从一列**没有媒体类型**
    //! 的字节里把格式猜出来。它错了不会报错，只会让书架上那张卡片永远画不出来，
    //! 所以按契约钉住：认得出的要给出图，认不出的要给 `None` 而不是「按 PNG 试一把」。

    use crate::ui::shared::images::cover_image;

    /// 够 `with_guessed_format` 探测的一段 PNG 头。
    const PNG: &[u8] = &[
        0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0x0D, b'I', b'H', b'D', b'R',
    ];
    const JPEG: &[u8] = &[
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, b'J', b'F', b'I', b'F', 0, 0, 0, 0, 0, 0, 0,
    ];

    #[test]
    fn recognised_cover_bytes_become_a_drawable_image() {
        assert!(cover_image(PNG).is_some(), "PNG 封面要能画");
        assert!(cover_image(JPEG).is_some(), "JPEG 封面要能画");
    }

    /// 认不出格式时给 `None`。
    ///
    /// 按 PNG 硬试的后果是**静默画空**：gpui 后面就照你给的那个格式挑解码器，
    /// 解不出来不报错，界面上「没有封面」和「封面坏了」会长成同一块空白 ——
    /// 而这两种情况要说的话不一样（见 `shelf_view` 的两个空状态）。
    #[test]
    fn unrecognised_cover_bytes_are_reported_as_absent_not_as_a_blank_image() {
        assert!(
            cover_image(b"<svg xmlns='x'></svg>").is_none(),
            "SVG 封面不该被当成能画的位图"
        );
        assert!(cover_image(b"").is_none());
        assert!(cover_image(b"not an image at all, just bytes").is_none());
    }
}
