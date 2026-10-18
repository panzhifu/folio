//! 定版式阅读状态：一本书的页表 + 当前页号。
//!
//! 定版式（CBZ、EPUB 的 fixed-layout、逐页 SVG 漫画）与可重排走**完全不同的两条**
//! 阅读链：这里没有章、没有行盒、没有排版 —— 页是既定的，缩放与裁剪只是显示变换
//! （见 `folio_core::content::format::Content` 的两族划分）。
//!
//! 所以它不塞进 [`crate::ui::features::reader::model::book::Book`]（那个带着章节窗口、Span、排版状态），而是单列
//! 一个轻状态：页表、当前页、以及一个按 href 现取字节的图片视图（[`ChapterImages`]，
//! 基准目录为空 —— 页的 `image_href()` 本来就是容器绝对路径）。绘制时按视口等比缩放。
//!
//! 位置坐标与可重排共用 [`folio_core::Locator`]：定版式里 `chapter` 就是**页号**、
//! `offset` 恒为 0（`LocatorKind::Page` 那条口径，见 `content::document`）。

use std::sync::Arc;

use folio_core::Locator;
use folio_core::content::document::Document as OpenedDocument;
use folio_core::content::format::{FixedPage, PageSource};
use gpui_kit::SharedString;

use crate::ui::features::reader::components::image_viewer::{ImageSlot, Place, file_name};
use crate::ui::foundation::images::{
    BookImages, ByteSource, ChapterImages, DecodeRequest, document_source,
};

/// 一本打开着的定版式书。
pub struct FixedBook {
    title: SharedString,
    pages: Vec<FixedPage>,
    /// 页图片视图（`href` → 这一张的活儿）。字节与解码都不在这里：那一趟由
    /// [`crate::ui::features::reader::model::image_atlas`] 在后台线程上跑（SVG 页也在那条路上被栅格化，见
    /// `ui::foundation::images`）。
    images: ChapterImages,
    /// 留住文档：字节要按 `href` 现取，只有它知道容器怎么开。
    #[allow(dead_code)]
    doc: Arc<dyn OpenedDocument>,
    page: usize,
}

impl FixedBook {
    /// 从一份已打开的定版式文档建状态，定位到 `locator.chapter` 那一页。
    pub fn new(doc: Arc<dyn OpenedDocument>, locator: Locator) -> Self {
        let pages = doc
            .fixed()
            .map(|fixed| fixed.pages.clone())
            .unwrap_or_default();
        let images = ChapterImages::new(
            Arc::new(BookImages::new(doc.images(), document_source(doc.clone()))),
            "",
        );
        let page = (locator.chapter as usize).min(pages.len().saturating_sub(1));
        Self {
            title: doc.meta().title.clone().into(),
            pages,
            images,
            doc,
            page,
        }
    }

    pub fn title(&self) -> &SharedString {
        &self.title
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn page(&self) -> usize {
        self.page
    }

    /// 翻到第 `page` 页（越界夹到最后一页）。
    pub fn set_page(&mut self, page: usize) {
        self.page = page.min(self.pages.len().saturating_sub(1));
    }

    /// 第 `page` 页的**全部**图：`(href, 原始宽, 原始高)`，主图在前、附加图在后。
    ///
    /// 只查页表、不碰字节 —— 「这一页有几张、各多大」在页表里就定了，取不到字节的那张
    /// 由绘制侧自己空着。单图页时这条就是一个元素，与 [`Self::page_size`] 时代等价。
    pub fn page_images(&self, page: usize) -> Vec<(&str, u32, u32)> {
        let Some(page) = self.pages.get(page) else {
            return Vec::new();
        };
        let main = match &page.source {
            PageSource::Image { href } => Some((href.as_str(), page.width, page.height)),
            PageSource::Render { .. } => None,
        };
        main.into_iter()
            .chain(
                page.extra_images
                    .iter()
                    .map(|image| (image.href.as_str(), image.width, image.height)),
            )
            .collect()
    }

    /// 按 href 取这张图的活儿（多图页逐张取）。索引里没有这条路径才给 `None`。
    ///
    /// 「缺字节、坏图」在这里**判不出来**：字节要到后台那一趟才去取（`image_atlas`），
    /// 而那两种的表现与从前一样 —— 那块地方空着，页的摆位不变。
    pub fn image_request(&self, href: &str) -> Option<DecodeRequest> {
        self.images.decode_request(href)
    }

    /// 取字节那条链子（分享 / 下载那一趟要在后台线程上拿它读原图）。
    pub(in crate::ui::features::reader) fn image_source(&self) -> ByteSource {
        self.images.source()
    }

    /// 整本书的那一列图：**页序就是阅读序**，每页主图在前、附加图在后。
    ///
    /// 与可重排那一支不一样，这里的清单不用回引擎查：页表本身就在库里
    /// （`store::content` 的 `page_images` 那一路），一张图属于第几页是页表说的事。
    /// 「定位原文」在这一支就是「跳到第 n 页」—— 那一页就是那张图。
    ///
    /// `PageSource::Render`（整页是栅格化产物、没有 href 的那些）不进这一列：
    /// 拿不到字节就没法摊开看，摆进列里是一张画不出来的黑纸。
    pub(in crate::ui::features::reader) fn build_image_sequence(&self) -> Arc<[ImageSlot]> {
        let mut out: Vec<ImageSlot> = Vec::new();
        for page in 0..self.pages.len() {
            for (href, width, height) in self.page_images(page) {
                let Some(request) = self.image_request(href) else {
                    continue;
                };
                let mime = self.images.media_type(href).unwrap_or("").to_string();
                let index = out.len() + 1;
                out.push(ImageSlot {
                    href: href.to_string(),
                    request,
                    natural: (width as f32, height as f32),
                    name: file_name(index, href, &mime),
                    mime,
                    place: Place::Fixed { page },
                });
            }
        }
        tracing::debug!("图片列: 这本定版式书有 {} 页里的图", out.len());
        out.into()
    }
}

#[cfg(test)]
mod tests {
    //! 定版式状态本身（页表 + 当前页）。不需要窗口：翻页只是一次夹取。

    use std::sync::Arc;

    use folio_core::Locator;
    use folio_core::content::format::{
        Content, DocMeta, FixedDoc, FixedPage, Format, PageImage, PageSource,
    };

    use super::FixedBook;

    fn book_with(pages: Vec<FixedPage>) -> FixedBook {
        let fixed = FixedDoc {
            pages,
            ..Default::default()
        };
        let meta = DocMeta {
            title: "漫画".into(),
            authors: Vec::new(),
            format: Format::Cbz,
            size: 0,
            ..DocMeta::default()
        };
        let doc = folio_core::content::document::from_content(meta, Content::Fixed(fixed));
        FixedBook::new(
            Arc::from(doc),
            Locator {
                chapter: 1,
                offset: 0,
            },
        )
    }

    fn book(pages: usize) -> FixedBook {
        book_with(
            (0..pages)
                .map(|i| FixedPage {
                    source: PageSource::Image {
                        href: format!("p{i}.png"),
                    },
                    width: 100,
                    height: 200,
                    extra_images: Vec::new(),
                })
                .collect(),
        )
    }

    /// 整本书那一列（定版式那一支）：**页序就是阅读序**，多图页主图在前、附加图在后，
    /// 而渲染型那一页（没有 href，字节拿不到）不进列。
    ///
    /// 每一条目带的出处是**页号**（`Place::Fixed`），「定位原文」在这一支就是
    /// 「跳到第 n 页」—— 那一页就是那张图，没有「页里的第几个字」那一根轴。
    #[test]
    fn the_image_column_follows_the_page_order() {
        use folio_core::content::format::ImageRes;

        let page = |href: &str, extras: &[&str]| FixedPage {
            source: PageSource::Image {
                href: href.to_string(),
            },
            width: 100,
            height: 200,
            extra_images: extras
                .iter()
                .map(|h| PageImage {
                    href: h.to_string(),
                    width: 100,
                    height: 200,
                })
                .collect(),
        };
        let pages = vec![
            page("p0.png", &["p0r.png"]),
            // 渲染型的一页（PDF 那一族）：页表里有这一页，图片表里没有这一条。
            FixedPage {
                source: PageSource::Render { index: 1 },
                width: 100,
                height: 200,
                extra_images: Vec::new(),
            },
            page("p2.png", &[]),
        ];
        let table = |href: &str| ImageRes {
            href: href.into(),
            source: href.into(),
            media_type: "image/jpeg".into(),
            width: 100,
            height: 200,
        };
        let fixed = FixedDoc {
            pages,
            images: vec![table("p0.png"), table("p0r.png"), table("p2.png")],
            ..Default::default()
        };
        let meta = DocMeta {
            title: "漫画".into(),
            authors: Vec::new(),
            format: Format::Cbz,
            size: 0,
            ..DocMeta::default()
        };
        let book = FixedBook::new(
            Arc::from(folio_core::content::document::from_content(
                meta,
                Content::Fixed(fixed),
            )),
            Locator::start(),
        );
        let column = book.build_image_sequence();
        let listed: Vec<(&str, usize)> = column
            .iter()
            .map(|slot| {
                (
                    slot.href.as_str(),
                    match slot.place {
                        super::Place::Fixed { page } => page,
                        super::Place::Reflow { .. } => {
                            panic!("定版式这一支不该出现章 + 字符偏移那一条")
                        }
                    },
                )
            })
            .collect();
        assert_eq!(
            listed,
            [("p0.png", 0), ("p0r.png", 0), ("p2.png", 2)],
            "跨页的两张都算、按主图在前附加图在后，渲染型那一页不算"
        );
        // 文件名上的序号跟着这一列走号：同一本里 `p0.png` 与 `p2.png` 之外，
        // 一本画册里成打的重名页全靠这一格不互相盖掉。
        let names: Vec<&str> = column.iter().map(|slot| slot.name.as_str()).collect();
        assert_eq!(names, ["001_p0.jpg", "002_p0r.jpg", "003_p2.jpg"]);
    }

    #[test]
    fn paging_is_clamped_and_starts_at_the_stored_page() {
        let mut fixed = book(5);
        assert_eq!(fixed.page_count(), 5);
        assert_eq!(fixed.page(), 1, "从 locator.chapter 起");
        assert_eq!(fixed.page_images(0), vec![("p0.png", 100, 200)]);
        fixed.set_page(9);
        assert_eq!(fixed.page(), 4, "越界夹到最后一页");
        fixed.set_page(0);
        assert_eq!(fixed.page(), 0);
    }

    /// 页表比 locator 短时也要落在合法页上（库里存的是老进度、书被重扫小了的场景）。
    #[test]
    fn a_stored_page_beyond_the_table_clamps() {
        let fixed = book(2);
        assert_eq!(fixed.page(), 1, "chapter=1 在范围内");
        let fixed = FixedBook::new(
            Arc::from(folio_core::content::document::from_content(
                DocMeta {
                    title: "短".into(),
                    authors: Vec::new(),
                    format: Format::Cbz,
                    size: 0,
                    ..DocMeta::default()
                },
                Content::Fixed(FixedDoc::default()),
            )),
            Locator {
                chapter: 9,
                offset: 0,
            },
        );
        assert_eq!(fixed.page_count(), 0);
        assert_eq!(fixed.page(), 0, "空页表时页号是 0，不是越界");
    }

    /// 多图页 / 对开跨页：主图在前、附加图在后，两张都数得出来 —— 绘制侧照这个顺序横排。
    ///
    /// 页表与字节是两件事：这一本没有容器，所以一张也解不出，但「有几张、各多大」仍然
    /// 必须在。少这一条，多图页在界面上就只剩主图（正是这次要修的行为）。
    #[test]
    fn a_multi_image_page_lists_the_main_image_then_the_extras() {
        let fixed = book_with(vec![FixedPage {
            source: PageSource::Image {
                href: "left.png".into(),
            },
            width: 800,
            height: 1200,
            extra_images: vec![PageImage {
                href: "right.png".into(),
                width: 800,
                height: 1200,
            }],
        }]);
        assert_eq!(
            fixed.page_images(0),
            vec![("left.png", 800, 1200), ("right.png", 800, 1200)],
            "两张都在，主图在前"
        );
        assert!(
            fixed.image_request("left.png").is_none(),
            "索引里没有这张图（这份假文档不带图片表），页表照旧"
        );
        assert!(fixed.page_images(1).is_empty(), "越界页是空表，不是 panic");
    }
}
