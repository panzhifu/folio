//! 书籍图片索引：把 `<img src>` 解析成容器路径后查表，并按 href 给出可绘制的
//! gpui `Image`。
//!
//! 分三层是为了少复制：`BookImages` 在打开一本书时建一次，每张图只存**元数据**
//! （href、媒体类型、固有尺寸）与一个按需读字节的来源 [`ByteSource`]；
//! **到第一次真要画时才去容器读、解码、缩小、建 `Arc<Image>`**（见
//! [`ImageEntry::rendered`]）；`ChapterImages` 只按当前章的目录解析相对路径，
//! 换章时重建，成本是一张 `HashMap` 的键拷贝。
//!
//! 字节不随 `FlowDoc` 常驻内存，也不进数据库 —— 对应 KOReader 的
//! `NodeImageProxy`：只持 `refName` 与宽高，`Decode()` 时才 `container->OpenStream`
//! 现取（`reference/koreader/base/thirdparty/kpvcrlib/crengine/crengine/src/lvtinydom.cpp:22178`）。

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use folio_core::content::document::Document;
use folio_core::content::format::{ImageRes, dir_of, guess_media_type, resolve_href, shrink_image};
use folio_core::layout::{ImageProvider, ResolvedImage};
use gpui_kit::{Image, ImageFormat};

/// 容器绝对路径 → 图片字节。
///
/// 拿 `Arc<dyn Fn>` 而不是一个具体类型，是为了让「真实容器」与「测试里的内存表」
/// 共用一个入口：`rendered()` 只管问要字节，不问它从哪来。
///
/// `Sync` 是硬要求：排版会在后台线程拿着这条链子。
pub type ByteSource = Arc<dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync>;

/// 一张图都不来的来源（Markdown / TXT，或「容器丢掉了」那条降级路）。
#[must_use]
pub fn no_images() -> ByteSource {
    Arc::new(|_href| None)
}

/// 把一本书接成字节来源：问 [`Document::image_bytes`] 要那一条。
///
/// 字节从哪来（解 ZIP、单文件、书库、什么都没有）是 `Document` 的事，这一层只转发 ——
/// 所以界面里不再有「按格式再查一次 provider」那一步（那正是 `Format` 渗进排版链的入口）。
///
/// 每次调用重开容器是刻意的，跟 KOReader 对齐（`NodeImageProxy::Decode()` 也是重开）；
/// 一张图只读一次这件事由 [`ImageEntry::rendered`] 的 `OnceLock` 保证。
pub fn document_source(doc: Arc<dyn Document>) -> ByteSource {
    Arc::new(move |href| doc.image_bytes(href))
}

/// 内嵌位图长边的上限（图像原始像素）。
///
/// 选 2048 不是随便挑的：gpui 把图放进 sprite atlas，单张 atlas 纹理的上限是设备的
/// `max_texture_dimension_2d`（GLES 上常见 4096，部分只有 2048），而 tile 用的是
/// **图像原始像素**（不是显示尺寸）。一张 1200×1600 的插图在 2.8 倍屏手机上显示出来
/// 是 3360×4480，超过了就只写一条日志、图静默消失 —— 手机上「书里的图看不见」
/// 十有八九是这一条。KOReader 的 `ImageWidget` 同样先按显示尺寸缩一遍再缓存
/// （`reference/koreader/frontend/ui/widget/imagewidget.lua` 的 `scale_factor` 与 `ImageCache`）。
///
/// 上限只影响超大的图：常见插图（几百到一千多像素）连解码都不会做，
/// 所以打开一本书的首帧不会为它付代价。
const MAX_IMAGE_EDGE: u32 = 2048;

struct ImageEntry {
    href: String,
    width: f32,
    height: f32,
    /// manifest 声明的媒体类型。字节不在手里，这是格式认不出的最后一道参考
    /// （第一道是字节头，见 [`ImageEntry::rendered`]）。
    media_type: String,
    /// 真正交给 gpui 的那一份：第一次要画时才去容器读、解码、缩小、建 `Image`。
    ///
    /// `OnceLock` 保证一张图只付一次代价，且跨帧、跨章复用；`None` 也缓下来，
    /// 容器丢了的那张图不会被每帧重试。
    rendered: OnceLock<Option<Arc<Image>>>,
}

impl ImageEntry {
    /// 按需产出可绘制的 `Image`。
    ///
    /// 三步都在这里，且都只在第一次调用时发生：
    ///
    /// 1. **取字节**：从 `bytes` 那条来源按容器路径现读。读不到（文件被移走 /
    ///    容器坏了 / 路径不在了）就回 `None`，调用方不画那块 —— 与 KOReader
    ///    容器丢失时的降级一致；
    /// 2. **缩小**：超过 [`MAX_IMAGE_EDGE`] 的先缩到上限内（见那条常量的理由）；
    /// 3. **认格式**：优先信字节头，其次信 manifest 声明的 media-type。真书里
    ///    `image/jpg`、`IMAGE/PNG`、`image/octet-stream` 这类写错比字节头写错常见得多，
    ///    而 gpui 按格式挑解码器，给错了就是一片静默空白。两者都不认才按 PNG 试一把。
    fn rendered(&self, bytes: &ByteSource) -> Option<Arc<Image>> {
        self.rendered
            .get_or_init(|| {
                let raw = bytes(&self.href)?;
                tracing::info!(
                    "ImageEntry::rendered: {} 现取到 {} 字节",
                    self.href,
                    raw.len()
                );
                let (data, declared) = match shrink_image(&raw, MAX_IMAGE_EDGE) {
                    Some((small, mime)) => (small, mime),
                    None => (raw, self.media_type.as_str()),
                };
                let format = guess_media_type(&data)
                    .or(Some(declared))
                    .and_then(format_of_mime)
                    .unwrap_or(ImageFormat::Png);
                tracing::info!(
                    "ImageEntry::rendered: {} 解码格式 {:?}，{} 字节",
                    self.href,
                    format,
                    data.len()
                );
                Some(Arc::new(Image::from_bytes(format, data)))
            })
            .clone()
    }
}

/// 一本书的全部图片，按容器路径索引。
///
/// `bytes` 是这份索引的「容器」那一半：元数据在 `entries` 里，字节要画的时候才从它读。
/// 分开之后，打开一本书拿到的是一张很轻的表，而不是整本图的字节。
pub struct BookImages {
    entries: Vec<ImageEntry>,
    index: HashMap<String, usize>,
    bytes: ByteSource,
}

impl BookImages {
    pub fn new(images: &[ImageRes], bytes: ByteSource) -> Self {
        let mut entries = Vec::with_capacity(images.len());
        let mut index = HashMap::with_capacity(images.len());
        for r in images {
            let idx = entries.len();
            index.entry(r.href.clone()).or_insert(idx);
            entries.push(ImageEntry {
                href: r.href.clone(),
                width: r.width as f32,
                height: r.height as f32,
                media_type: r.media_type.clone(),
                rendered: OnceLock::new(),
            });
        }
        Self {
            entries,
            index,
            bytes,
        }
    }

    fn get(&self, href: &str) -> Option<&ImageEntry> {
        self.index.get(href).map(|i| &self.entries[*i])
    }
}

/// mime → gpui 的 `ImageFormat`。认不出时返回 `None`，由调用方决定最后的降级 ——
/// 不在这里默认成某个格式，是因为那个 `unwrap_or` 的位置决定“默认”算谁的。
fn format_of_mime(media_type: &str) -> Option<ImageFormat> {
    let mime = media_type.split(';').next().unwrap_or("").trim();
    ImageFormat::from_mime_type(mime)
}

/// mime → gpui 的 `ImageFormat`。认不出时按 PNG 试（最常见，且解不出时只是不显示，
/// 不会 panic）。**只给封面这条「只有字节、没有 manifest 媒体类型」的路用**：
/// 正文里的图片走 [`ImageEntry::rendered`]，那条会拿字节头当第一判据。
fn format_of(media_type: &str) -> ImageFormat {
    format_of_mime(media_type).unwrap_or(ImageFormat::Png)
}

/// 封面字节 → 可画的 `Image`；格式认不出时返回 `None`。
///
/// 返回 `None` 而不是按 PNG 试一把，是因为 `Image::from_bytes` 之后 gpui 就按那个
/// 格式挑解码器（`platform.rs` 里 `to_image_data` 是 `match self.format`），
/// 给错格式的结果是**静默画空**：书架上是一块和「没有封面」长得一样的空白，
/// 而界面已经分不清这两件事了。认不出时调用方要画显式的占位块。
pub fn cover_image(bytes: &[u8]) -> Option<Arc<Image>> {
    let media_type = folio_core::content::format::guess_media_type(bytes)?;
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
            book: Arc::new(BookImages::new(&[], no_images())),
            base_dir: String::new(),
        }
    }

    /// 绘制用：容器路径 → 可渲染的 `Image`。容器丢了或是字节读不出来时回 `None`，
    /// 调用方把那一块空着（正文照排）。
    pub fn render_source(&self, href: &str) -> Option<Arc<Image>> {
        self.book.get(href)?.rendered(&self.book.bytes)
    }
}

impl ImageProvider for ChapterImages {
    fn resolve(&self, src: &str) -> Option<ResolvedImage> {
        let href = resolve_href(&self.base_dir, src);
        let entry = self.book.get(&href)?;
        tracing::info!(
            "ChapterImages::resolve: {src:?} → {href:?} 命中 {}×{}",
            entry.width,
            entry.height
        );
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
    use folio_core::layout::ImageProvider as _;

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

    /// 正文插图认格式时要**先信字节头**：真书里 manifest 的 media-type 写错比字节头错得多。
    /// 以前只信 media-type，写成 `image/jpg` 的书里那张 PNG 就按 JPEG 去解，
    /// 结果是静默画空 —— 与「这本没有图」在屏幕上长得一样。
    #[test]
    fn a_book_image_takes_its_format_from_the_bytes_not_the_declared_mime() {
        use folio_core::content::format::ImageRes;
        use gpui_kit::ImageFormat;

        use crate::ui::shared::images::{BookImages, ChapterImages};

        let book = BookImages::new(
            &[ImageRes {
                href: "OEBPS/Images/pic.png".into(),
                // 故意写错：这正是真书里会出现的几种。
                media_type: "image/jpg".into(),
                width: 1,
                height: 1,
            }],
            // 字节不由 `BookImages` 持有，而是从这条来源现读 —— 测试里用一个内存表
            // 代替容器。
            std::sync::Arc::new(|_href: &str| Some(PNG.to_vec())),
        );
        let chapter = ChapterImages::new(std::sync::Arc::new(book), "OEBPS/Text/chap.xhtml");
        let image = chapter
            .render_source("OEBPS/Images/pic.png")
            .expect("容器路径该命中");
        assert_eq!(
            image.format(),
            ImageFormat::Png,
            "字节头是 PNG，就不该被 `image/jpg` 带到 JPEG 去"
        );
    }

    /// 容器丢了（或那张图不在里面）时，`render_source` 必须给 `None` 而不是 panic
    /// 或空盒 —— 正文照排，图片那块地方空着。
    ///
    /// 对应 KOReader 的 `NodeImageProxy::IsInvalid()`：容器读不到就不画，不把
    /// 「这本书没这张图」与「容器坏了」拆成两种表现。
    #[test]
    fn a_missing_container_yields_no_image_instead_of_a_panic() {
        use folio_core::content::format::ImageRes;

        use crate::ui::shared::images::{BookImages, ChapterImages, no_images};

        let book = BookImages::new(
            &[ImageRes {
                href: "OEBPS/Images/pic.png".into(),
                media_type: "image/png".into(),
                width: 1,
                height: 1,
            }],
            no_images(),
        );
        let chapter = ChapterImages::new(std::sync::Arc::new(book), "OEBPS/Text/chap.xhtml");
        assert!(
            chapter.render_source("OEBPS/Images/pic.png").is_none(),
            "字节来源给不出东西时，图片就该不画"
        );
        // 元数据仍在：排版依旧知道那儿有一张图（尺寸 / 位置），只是画不出像素。
        assert!(
            chapter.resolve("../Images/pic.png").is_some(),
            "排版侧的尺寸信息不依赖字节"
        );
    }
}
