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

use std::collections::HashMap;
use std::sync::Arc;

use folio_core::Locator;
use folio_core::content::document::Document as OpenedDocument;
use folio_core::content::format::FixedPage;
use folio_core::content::format::PageSource;
use folio_core::content::format::pdf::RENDER_EDGE;
use gpui_kit::SharedString;

use crate::ui::features::reader::components::image_viewer::{ImageSlot, Place, file_name};
use crate::ui::foundation::images::{
    BookImages, ByteSource, ChapterImages, DecodeRequest, document_source,
};

/// 定版式一页上要摆的一格。
///
/// 尺寸是**页表里那格说的原始像素**（图源页 = 那张图的固有尺寸；渲染页 = 按
/// `RENDER_EDGE` 那一档算出来的尺寸），绘制侧按 `width / height` 定比例。
/// `request` 是「怎么拿到像素」，`None` = 这本书的图片表里没有这条路径 —— 那一格
/// 仍然占位，只是画不出来。
///
/// `href` / `mime` / `export` 三格**不是绘制要的东西**，是整本书那一列图（图片查看器）
/// 要的：一列里的每一条都要说得出「这是哪一张、什么类型、原字节从哪儿来」。摆在这里的
/// 理由是「一页上有哪几格」这件事只能有一本账 —— 分两处列，绘制那一趟与查看器那一列
/// 就会在多图页或渲染页上数得不一样。
pub struct PageTile {
    pub request: Option<DecodeRequest>,
    pub width: u32,
    pub height: u32,
    /// 这一张的地址：图源页是容器路径，渲染型的一页是 `render:<引擎页号>`
    /// （它没有容器路径可言，那一格就是它自己的身份）。
    pub href: String,
    /// 递给系统时按它给 intent 的类型。渲染型的一页是**已知**的 PNG（不必猜字节头）。
    pub mime: String,
    /// 分享 / 下载那一条取的字节。
    pub export: ByteSource,
}

/// 一本打开着的定版式书。
pub struct FixedBook {
    title: SharedString,
    pages: Vec<FixedPage>,
    /// 页图片视图（`href` → 这一张的活儿）。字节与解码都不在这里：那一趟由
    /// [`crate::ui::features::reader::model::image_atlas`] 在后台线程上跑（SVG 页也在那条路上被栅格化，见
    /// `ui::foundation::images`）。
    images: ChapterImages,
    /// 留住文档：字节要按 `href` 现取，渲染型的页要按页号现画，只有它知道容器怎么开。
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

    /// 按 href 取这张图的活儿（多图页逐张取）。索引里没有这条路径才给 `None`。
    ///
    /// 「缺字节、坏图」在这里**判不出来**：字节要到后台那一趟才去取（`image_atlas`），
    /// 而那两种的表现与从前一样 —— 那块地方空着，页的摆位不变。
    pub fn image_request(&self, href: &str) -> Option<DecodeRequest> {
        self.images.decode_request(href)
    }

    /// 第 `page` 页**这一帧要摆的每一格**：怎么拿到像素，加上它声明的原始尺寸。
    ///
    /// 每一格带的是「拿像素的那条活儿」加它自己的身份：图源页按 `href` 回容器读，
    /// 渲染型（PDF）的一页按 [`RENDER_EDGE`] 那一档现画。
    /// 两族合进同一个列表，绘制侧就不必知道这一页属于哪一族 —— 与 core 那边
    /// 「格式差异到 `Content` 为止」同一立场。
    ///
    /// `request` 为 `None` 的那一格**仍然占位**：多图页的排版不该因为索引里少一条路径
    /// 就把剩下的往左挤（这一条与从前逐像素相同）。
    pub fn page_tiles(&self, page: usize) -> Vec<PageTile> {
        let Some(entry) = self.pages.get(page) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(1 + entry.extra_images.len());
        match &entry.source {
            PageSource::Image { href } => out.push(PageTile {
                request: self.image_request(href),
                width: entry.width,
                height: entry.height,
                href: href.clone(),
                mime: self.images.media_type(href).unwrap_or("").to_string(),
                export: self.images.source(),
            }),
            PageSource::Render { index } => {
                // 页号是**引擎那一侧**的号（页表里存的就是它），与阅读序一致由
                // `format::pdf` 保证：那一本不跳页、不重排。
                let index = *index as usize;
                let render = self.render_bytes(index);
                out.push(PageTile {
                    request: Some(self.images.render_request(
                        index,
                        RENDER_EDGE,
                        entry.width,
                        entry.height,
                        render.clone(),
                    )),
                    width: entry.width,
                    height: entry.height,
                    href: format!("render:{index}"),
                    // 栅格化的产物按定义就是 PNG（`provider::render_page` 那条契约）。
                    mime: "image/png".to_string(),
                    // 这一条不看传进来的地址：渲染页的「原字节」就是按那一档画出来的
                    // 那张 PNG —— 没有「容器里另存着一份更大的原图」这回事。
                    export: Arc::new(move |_| render()),
                });
            }
        }
        for image in &entry.extra_images {
            out.push(PageTile {
                request: self.image_request(&image.href),
                width: image.width,
                height: image.height,
                href: image.href.clone(),
                mime: self
                    .images
                    .media_type(&image.href)
                    .unwrap_or("")
                    .to_string(),
                export: self.images.source(),
            });
        }
        out
    }

    /// 渲染型那一页「现画一张」的那条链子（页号与 [`RENDER_EDGE`] 都关在闭包里）。
    ///
    /// 摊开成一格而不是就地一个 `move ||`：绘制那条活儿与分享 / 下载那条要拿到的是
    /// **同一份**定义，两处各写一遍就会有一处先改掉档位。
    fn render_bytes(&self, index: usize) -> Arc<dyn Fn() -> Option<Vec<u8>> + Send + Sync> {
        let document = self.doc.clone();
        Arc::new(move || document.render_page(index, RENDER_EDGE))
    }

    /// 这一本的目录：**文档自带的书签**（PDF 的 `/Outlines`）。
    ///
    /// 漫画包（CBZ）与 EPUB 的定版式都没有书签，回的是空表 —— 与从前一样，目录那一屏
    /// 在那些书上没有行可画。落点是**页号**（`TocEntry::page`），所以那张「容器路径 →
    /// 章号」的表在这里是空的：`toc_tree::build` 认页号那一格优先。
    pub fn toc_rows(&self) -> Vec<crate::ui::features::reader::model::toc_tree::TocRow> {
        crate::ui::features::reader::model::toc_tree::build(self.doc.toc(), &HashMap::new())
    }

    /// 整本书的那一列图：**页序就是阅读序**，每页主图在前、附加图在后。
    ///
    /// 与可重排那一支不一样，这里的清单不用回引擎查：页表本身就在库里
    /// （`store::content` 的 `page_images` 那一路），一张图属于第几页是页表说的事。
    /// 「定位原文」在这一支就是「跳到第 n 页」—— 那一页就是那张图。
    ///
    /// 渲染型那一页（PDF / AZW4 / DjVu）**也在这一列里**：它的像素与图源页那一份是
    /// 同一条活儿（`page_tiles` 里那个 `render_request`），正文里画得出来的东西
    /// 摊开看也一定画得出来。以前它被排除在列外，理由是「拿不到字节」—— 那句话在
    /// `PageSource::Render` 接上栅格化之后就不成立了，代价变成读者点那一页没任何反应
    /// （没有全屏放大、没有分享与下载那一页），而漫画的页都有。
    pub(in crate::ui::features::reader) fn build_image_sequence(&self) -> Arc<[ImageSlot]> {
        let mut out: Vec<ImageSlot> = Vec::new();
        for page in 0..self.pages.len() {
            for tile in self.page_tiles(page) {
                let Some(request) = tile.request else {
                    continue;
                };
                let index = out.len() + 1;
                // 渲染页在递给系统的那个名字上说的是「第几页」（读者看得懂的那一格），
                // 而 `href` 那一格是它在这一本里的地址，两者不必同形。
                let stem = match tile.href.strip_prefix("render:") {
                    Some(_) => format!("page-{}.png", page + 1),
                    None => tile.href.clone(),
                };
                out.push(ImageSlot {
                    href: tile.href,
                    request,
                    natural: (tile.width as f32, tile.height as f32),
                    name: file_name(index, &stem, &tile.mime),
                    mime: tile.mime,
                    place: Place::Fixed { page },
                    export: tile.export,
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
    /// 渲染型那一页（PDF / AZW4 / DjVu）也占一条 —— 它的地址是 `render:<引擎页号>`。
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
            [
                ("p0.png", 0),
                ("p0r.png", 0),
                ("render:1", 1),
                ("p2.png", 2)
            ],
            "跨页的两张都算、按主图在前附加图在后，而渲染型那一页**也算**（它有自己的活儿）"
        );
        // 文件名上的序号跟着这一列走号：同一本里 `p0.png` 与 `p2.png` 之外，
        // 一本画册里成打的重名页全靠这一格不互相盖掉。渲染页那一格说的是第几页。
        let names: Vec<&str> = column.iter().map(|slot| slot.name.as_str()).collect();
        assert_eq!(
            names,
            ["001_p0.jpg", "002_p0r.jpg", "003_page-2.png", "004_p2.jpg"]
        );
    }

    /// 渲染型的一页给的是**怎么画**，不是容器里哪一条：`page_tiles` 里那一格带着
    /// 一条能跑的活儿（键 = 本书 token + 页号 + 档位）。
    #[test]
    fn a_render_page_comes_with_a_job_not_an_href() {
        let fixed = FixedBook::new(
            Arc::from(folio_core::content::document::from_content(
                DocMeta {
                    title: "一本 PDF".into(),
                    authors: Vec::new(),
                    format: Format::Pdf,
                    size: 0,
                    ..DocMeta::default()
                },
                Content::Fixed(FixedDoc {
                    pages: vec![FixedPage {
                        source: PageSource::Render { index: 3 },
                        width: 1447,
                        height: 2048,
                        extra_images: Vec::new(),
                    }],
                    ..Default::default()
                }),
            )),
            Locator::start(),
        );
        let tiles = fixed.page_tiles(0);
        assert_eq!(tiles.len(), 1, "渲染型的一页就是一格");
        let tile = &tiles[0];
        assert_eq!((tile.width, tile.height), (1447, 2048), "尺寸照页表说");
        let request = tile.request.as_ref().expect("该有一条活儿");
        assert!(
            request.key.ends_with("/render/3@2048"),
            "键要说清是哪一页、哪一档：{}",
            request.key
        );
        assert_eq!(
            request.tile_bytes,
            1447 * 2048 * 4,
            "回收那笔账按声明的像素算"
        );
        // 这份假文档没有容器 —— 与「容器丢了」同一表现：活儿在，跑出来是空。
        assert!(request.decode().is_none());
        // 整本书那一列**有**这一条：正文里画得出来的页，摊开看也画得出来。
        let column = fixed.build_image_sequence();
        assert_eq!(column.len(), 1, "渲染型的一页也进那一列");
        assert_eq!(column[0].href, "render:3", "它没有容器路径，地址就是它自己");
        assert_eq!(column[0].mime, "image/png", "栅格化的产物按定义是 PNG");
        assert!(matches!(column[0].place, super::Place::Fixed { page: 0 }));
    }

    /// 真夹具走通整条链：**核心算得出像素，界面拿得到**。
    ///
    /// 这一条是「能读能翻」那句话在测试里的形状。用入库的那本 PDF（`tests/fixtures/oss/pdf`，
    /// 形状由 poppler 验过）而不是合成的：`from_content` 造出来的文档没有容器，
    /// `render_page` 永远回 `None`，那样的断言只能证明「代码写了」而不能证明「跑得通」。
    ///
    /// `decode()` 在这里**当场跑一趟栅格化**（真在生产路径上也是这么跑的，只是由图集
    /// 派到后台线程）：一页按 2048 那一档实测 30–90 ms，测试套件里算慢的一格，
    /// 但换到的是端到端，值得。
    #[test]
    fn a_real_pdf_hands_the_screen_pixels() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/oss/pdf/three-pages.pdf");
        let document = folio_core::content::document::open(&path)
            .unwrap_or_else(|err| panic!("{} 该开得开：{err}", path.display()));
        let fixed = FixedBook::new(Arc::from(document), Locator::start());
        assert_eq!(fixed.page_count(), 3, "pdfinfo 说 3 页");
        assert_eq!(fixed.page(), 0);

        let tiles = fixed.page_tiles(0);
        assert_eq!(tiles.len(), 1);
        let request = tiles[0].request.clone().expect("有容器就该有活儿");
        // 第 2 页是被裁过的那一页：页表说 `1229 × 2048`，画出来也要是那么大。
        // （量的单位是**解出来的位图**：gpui 的 `Image` 只带 `format` 与 `bytes`，
        // 尺寸要到解码那一层才知道，所以这里用 dev-dependency 里的 `image` 解一次。）
        let cropped = fixed.page_tiles(1)[0].request.clone().unwrap();
        let decoded = cropped.decode().expect("被裁的一页也该画得出来");
        let image = image::load_from_memory(&decoded.bytes).expect("交回的该是 PNG");
        let (w, h) = (image.width(), image.height());
        assert!(
            w.abs_diff(1229) <= 2 && h.abs_diff(2048) <= 2,
            "页表说 1229×2048，画出来 {w}×{h}"
        );
        let first = request.decode().expect("第 1 页也该画得出来");
        let first = image::load_from_memory(&first.bytes).expect("第 1 页也该是 PNG");
        assert!(
            first.width().abs_diff(1447) <= 2 && first.height().abs_diff(2048) <= 2,
            "第 1 页 {}×{}",
            first.width(),
            first.height()
        );

        // 点开那一击走的是整本书那一列：按图集的键问得到条目，才谈得上放大、
        // 左右张与「定位原文」（见 `view::body::open_picture`）。
        let column = fixed.build_image_sequence();
        let listed: Vec<&str> = column.iter().map(|slot| slot.href.as_str()).collect();
        assert_eq!(listed, ["render:0", "render:1", "render:2"], "三页三条");
        assert_eq!(
            column[0].request.key, request.key,
            "列里那条与正文摆出去的那条是同一个键 —— 键不等，点在页上就找不到那一列"
        );
        // 分享 / 下载那一趟取的是**画出来的**那份 PNG，不再「从容器里读不出字节」。
        let png = (column[1].export)(&column[1].href).expect("渲染页的字节就是它自己");
        assert_eq!(&png[..4], b"\x89PNG".as_slice(), "递给系统的该是一张 PNG");
        assert_eq!(column[1].name, "002_page-2.png", "名字要说得出是第几页");
    }

    /// 定版式的目录就是文档自带的书签，落点是页号（`TocEntry::page` → `TocRow::chapter`）。
    #[test]
    fn a_pdf_hands_out_its_bookmarks_with_pages_as_targets() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/oss/pdf/three-pages.pdf");
        let document = folio_core::content::document::open(&path).unwrap();
        let fixed = FixedBook::new(Arc::from(document), Locator::start());
        let rows = fixed.toc_rows();
        let got: Vec<(&str, Option<usize>)> = rows
            .iter()
            .map(|row| (row.title.as_str(), row.chapter))
            .collect();
        assert_eq!(
            got,
            vec![("第一章", Some(0)), ("第二章", Some(1))],
            "书签两条、落点是页号；没有 /Dest 的那条在 core 就被丢了"
        );
        // 漫画一本没有书签：空表，与从前逐字相同。
        assert!(book(3).toc_rows().is_empty(), "没有书签就该是空表");
    }

    #[test]
    fn paging_is_clamped_and_starts_at_the_stored_page() {
        let mut fixed = book(5);
        assert_eq!(fixed.page_count(), 5);
        assert_eq!(fixed.page(), 1, "从 locator.chapter 起");
        assert_eq!(fixed.page_tiles(0)[0].href, "p0.png");
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
        let tiles = fixed.page_tiles(0);
        let listed: Vec<(&str, u32, u32)> = tiles
            .iter()
            .map(|tile| (tile.href.as_str(), tile.width, tile.height))
            .collect();
        assert_eq!(
            listed,
            vec![("left.png", 800, 1200), ("right.png", 800, 1200)],
            "两张都在，主图在前"
        );
        assert!(
            tiles[0].request.is_none(),
            "索引里没有这张图（这份假文档不带图片表），页表照旧"
        );
        assert!(fixed.page_tiles(1).is_empty(), "越界页是空表，不是 panic");
    }
}
