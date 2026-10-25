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

use std::cell::{Ref, RefCell};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use folio_core::Locator;
use folio_core::content::document::Document as OpenedDocument;
use folio_core::content::format::pdf::RENDER_EDGE;
use folio_core::content::format::{FixedPage, PageSource, PageText};
use gpui_kit::SharedString;

use crate::ui::features::reader::components::image_viewer::{ImageSlot, Place, file_name};
use crate::ui::foundation::images::{
    BookImages, ByteSource, ChapterImages, DecodeRequest, document_source,
};

/// 定版式一页上要摆的一格。
///
/// 尺寸是**页表里那格说的原始像素**（图源页 = 那张图的固有尺寸；渲染页 = 下标那一趟
/// 算出来的尺寸：矢量页是 `RENDER_EDGE`、图片页是源图密度封顶后的档位），绘制侧按
/// `width / height` 定比例。
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
    /// 这一格是**栅格化出来**的一页（PDF / AZW4 / DjVu），不是容器里的一条图。
    ///
    /// 绘制侧不看这一格（两格拿像素的路已经分在 `request` 里了），看它的是**选中那一条
    /// 链**：只有栅格化出来的那一页有文字层，于是「屏幕点 ↔ 页内字序」那把尺必须记在
    /// 这一格摆出去的矩形上，而不是记在多图页的第一张上。用 `href` 的前缀猜这件事等于
    /// 把一种身份同时挂在两个地方。
    pub renders: bool,
}

/// 文字层备忘的一条：`(哪一页, 那一页的字层, 那一份串的哈希)`。
///
/// 内层是 `Option`，因为「这一页没有字」也是一份**答案**，而它必须被记下来 —— 不记的
/// 那一版每帧都回引擎重开一次容器（一份扫描版 PDF 实测重开一趟 61–374 ms）。
type PageTextMemo = (usize, Option<Rc<PageText>>, u64);

/// 文字层备忘能留几页。
///
/// 分页时屏上只有一页，从前只留一格。连续竖滚里屏上常常同时有一页半 —— 上游那一条
/// 还在视口里露着、下一条已经进来 —— 只留一格就会在两页之间每帧来回重开容器
/// （PDF 一页 61–374 ms）。四格足够盖住一次翻页前后看得见的那几张，又不会把整本书
/// 的字层攒在内存里。
const PAGE_TEXT_CACHE: usize = 4;

/// 定版式的纵向页栈：在**某个版心宽**下，每页多高、从哪开始、整本多高。
///
/// 连续竖滚把它当成一条拼接链读：`scroll_y` 是栈上的像素，页号只是「视口里占得最多的
/// 那一页」的读数。与可重排那一族的章基线同一个角色，只是这里不必排版 —— 页的宽高比
/// 在页表里。
#[derive(Clone, Debug, Default)]
pub(in crate::ui::features::reader) struct FixedStack {
    /// 建这条栈时的版心宽与页缝。宽度变了整条作废（见 [`FixedBook::ensure_stack`]）。
    width: f32,
    gap: f32,
    /// 每页的高度与顶边（栈坐标，第一页顶边是 0）。
    heights: Vec<f32>,
    tops: Vec<f32>,
    /// 最后一页底边，也就是整条栈的总高。
    total: f32,
}

impl FixedStack {
    /// 按页表建一条栈。`fallback_aspect` 给「页表没说宽高」的那几格兜底：
    /// 用版心宽除以视口高，退化成一个铺满视口的页，而不是零面积。
    fn build(pages: &[FixedPage], width: f32, gap: f32, fallback_aspect: f32) -> Self {
        let width = width.max(1.0);
        let fallback = if fallback_aspect > 0.0 {
            fallback_aspect
        } else {
            1.0
        };
        let mut heights = Vec::with_capacity(pages.len());
        let mut tops = Vec::with_capacity(pages.len());
        let mut y = 0.0;
        for (index, page) in pages.iter().enumerate() {
            if index > 0 {
                y += gap.max(0.0);
            }
            let aspect = page
                .extra_images
                .iter()
                .fold(aspect(page.width, page.height, fallback), |sum, image| {
                    sum + aspect(image.width, image.height, fallback)
                });
            let height = (width / aspect.max(0.001)).max(1.0);
            tops.push(y);
            heights.push(height);
            y += height;
        }
        Self {
            width,
            gap: gap.max(0.0),
            heights,
            tops,
            total: y,
        }
    }

    pub(in crate::ui::features::reader) fn len(&self) -> usize {
        self.tops.len()
    }

    pub(in crate::ui::features::reader) fn is_empty(&self) -> bool {
        self.tops.is_empty()
    }

    pub(in crate::ui::features::reader) fn total(&self) -> f32 {
        self.total
    }

    pub(in crate::ui::features::reader) fn height_of(&self, page: usize) -> Option<f32> {
        self.heights.get(page).copied()
    }

    pub(in crate::ui::features::reader) fn top_of(&self, page: usize) -> Option<f32> {
        self.tops.get(page).copied()
    }

    /// 栈坐标 `y` 落在哪一页。落在页缝里时按「离哪一页更近」归给前/后一页 —— 页缝
    /// 只是视觉上的呼吸，不该让页号在正中间空出一个「没有页」的区间。
    pub(in crate::ui::features::reader) fn page_at_y(&self, y: f32) -> Option<usize> {
        if self.is_empty() {
            return None;
        }
        let y = y.max(0.0);
        let index = self
            .tops
            .partition_point(|top| *top <= y)
            .saturating_sub(1)
            .min(self.len() - 1);
        let bottom = self.tops[index] + self.heights[index];
        if y >= bottom + self.gap * 0.5 && index + 1 < self.len() {
            Some(index + 1)
        } else {
            Some(index)
        }
    }

    /// 栈坐标 `y` 在它那一页里的比例（0..1）。宽度变化后按它把位置搬回新栈。
    pub(in crate::ui::features::reader) fn page_and_fraction(
        &self,
        y: f32,
    ) -> Option<(usize, f32)> {
        let page = self.page_at_y(y)?;
        let top = self.tops[page];
        let height = self.heights[page];
        let fraction = if height > 0.0 {
            ((y.max(0.0) - top) / height).clamp(0.0, 1.0)
        } else {
            0.0
        };
        Some((page, fraction))
    }

    /// 视口 `[top, top + viewport)` 盖到的页区间。
    ///
    /// 起点要往回收一页：`page_at_y` 在页缝里会归给下一页，而上一页的末尾还露在视口里。
    /// 长页可以比视口高得多，所以不能假设「一屏最多两页」—— 只能按顶边扫到视口底。
    pub(in crate::ui::features::reader) fn visible(&self, top: f32, viewport: f32) -> Range<usize> {
        if self.is_empty() {
            return 0..0;
        }
        let first = self.page_at_y(top).unwrap_or(0);
        let mut start = first;
        while start > 0 && self.tops[start] > top {
            start -= 1;
        }
        let bottom = top + viewport.max(0.0);
        let mut end = start;
        while end < self.len() && self.tops[end] < bottom {
            end += 1;
        }
        start..end
    }
}

fn aspect(width: u32, height: u32, fallback: f32) -> f32 {
    if width == 0 || height == 0 {
        fallback
    } else {
        width as f32 / height as f32
    }
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
    /// 最近问过的几页文字层（选中、色块、批注回显都问它），连同页号与那份串的哈希。
    ///
    /// **必须留一份备忘**：`Document::page_text` 那一头每次问都要重开一次容器 —— PDF
    /// 实测一份 3.6 MB、45 页的书重开一趟 374 ms（`format::pdf` 模块头那笔账）。
    /// 而选中这一套在一帧里要问好几趟（命中、吸词、色块、落批注、回显），手指拖着走
    /// 就是每来一个事件一趟。一页的字层只有一副样子，重开第二次买不到任何新信息。
    ///
    /// 只留最近几页而不是整本：这与 core 那边「文字层不入库、按页现取」是同一条立场
    /// （`format::provider::DocumentProvider::page_text` 写的理由）。连续竖滚里视口
    /// 可能同时压着两页，所以容量见 [`PAGE_TEXT_CACHE`]；翻走之后第一次回来会重付一趟，
    /// 那一趟不在绘制热路径上。
    ///
    /// 内层 `None` = **问过引擎，这一页没有字**（那也是一份答案，见
    /// [`Self::cached_page_text`]）。那个哈希是**批注回显那一份缓存的键**
    /// （`annotations::resolved_for` 要一把尺问「这一页的字还是刚才那些字吗」）：
    /// 算一次存在这里，而不是每帧把整页的字重数一遍。
    text: RefCell<Vec<PageTextMemo>>,
    /// 连续竖滚的页栈。`None` = 还没量到版心宽，或者这本书此刻走的是整页翻。
    stack: RefCell<Option<FixedStack>>,
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
            text: RefCell::new(Vec::new()),
            stack: RefCell::new(None),
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

    /// 保证页栈按当前版心宽建好；宽度或页缝变了才重建（返回 `true`）。
    ///
    /// 页栈只在渲染帧里建 —— 它是「这一屏多宽」的函数，而窗口宽只有渲染帧手上有。
    /// 事件那一头（滚轮、落页）只读已经建好的那条，读到 `None` 就什么都不做：
    /// 第一帧还没画出来时本来也没有可滚的像素。
    pub(in crate::ui::features::reader) fn ensure_stack(
        &self,
        width: f32,
        gap: f32,
        fallback_aspect: f32,
    ) -> bool {
        let mut slot = self.stack.borrow_mut();
        if let Some(stack) = slot.as_ref()
            && (stack.width - width).abs() < 0.5
            && (stack.gap - gap).abs() < 0.5
        {
            return false;
        }
        *slot = Some(FixedStack::build(&self.pages, width, gap, fallback_aspect));
        true
    }

    /// 这一帧的页栈。还没建过（第一帧、整页翻那一档）时 `None`。
    pub(in crate::ui::features::reader) fn stack(&self) -> Option<Ref<'_, FixedStack>> {
        let stack = self.stack.borrow();
        Ref::filter_map(stack, Option::as_ref).ok()
    }

    /// 第 `page` 页在栈上的顶边。页栈还没建时 `None`。
    pub(in crate::ui::features::reader) fn stack_top_of(&self, page: usize) -> Option<f32> {
        self.stack()?.top_of(page)
    }

    /// 连续竖滚里「读者读到第几页」：**视口里占得最多**的那一页。
    ///
    /// 为什么不是「视口顶那一页」：最后一页常常比一屏矮，页首永远滚不到视口顶 ——
    /// 只按顶边算，读者已经看到全书末尾，页号与进度却停在倒数第二页；而在书尾往回
    /// 滚，页号又会在第一个像素上从末页跳回上一页。按可见面积取最大，换页落在两页
    /// 各露一半的位置，页脚与进度的读数跟屏上看到的一致。
    ///
    /// 面积相等时归前面那一页；整本一屏装得下时因此仍从第 0 页读起。
    pub(in crate::ui::features::reader) fn page_at_scroll(
        &self,
        y: f32,
        viewport: f32,
    ) -> Option<usize> {
        let stack = self.stack()?;
        if stack.is_empty() {
            return None;
        }
        let top = y.max(0.0);
        let bottom = top + viewport.max(1.0);
        let mut best = None;
        let mut best_overlap = 0.0;
        for page in stack.visible(top, viewport) {
            let page_top = stack.top_of(page)?;
            let page_bottom = page_top + stack.height_of(page)?;
            let overlap = page_bottom.min(bottom) - page_top.max(top);
            if overlap > best_overlap + 0.5 {
                best = Some(page);
                best_overlap = overlap;
            }
        }
        best.or_else(|| stack.page_at_y(top))
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
    /// 渲染型（PDF）的一页按 [`RENDER_EDGE`] 那一档现画 —— 那一档是**上限**：图片页
    /// 在 core 那一侧按源图密度封到更低一档（`format::pdf::native_edge`），所以页表里
    /// 的 `entry.width / height` 已经是封顶之后的数，画出来的像素与它一致。
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
                renders: false,
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
                    renders: true,
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
                renders: false,
            });
        }
        out
    }

    /// 渲染型那一页「现画一张」的那条链子（页号与 [`RENDER_EDGE`] 那个上限都关在闭包里）。
    ///
    /// 档位不在这里算：页表里存的宽高就是「这一页画出来多大」，core 的 `render_edge` 在
    /// 栅格化那一侧按同一句判断封顶，两处因此只有一个定义。
    ///
    /// 摊开成一格而不是就地一个 `move ||`：绘制那条活儿与分享 / 下载那条要拿到的是
    /// **同一份**定义，两处各写一遍就会有一处先改掉档位。
    fn render_bytes(&self, index: usize) -> Arc<dyn Fn() -> Option<Vec<u8>> + Send + Sync> {
        let document = self.doc.clone();
        Arc::new(move || document.render_page(index, RENDER_EDGE))
    }

    /// 当前这一页在**整本书那一列图**里的身份（图集的键）。
    ///
    /// 存在的理由只有一条：底栏那颗「整页」不在正文的绘制帧里，手上没有那张图的键，
    /// 而放大那一层认的就是这个键（`ReaderSession::image_index_of_key`）。取**第一格
    /// 有活儿的**那张：多图页的主图在前，而那一层一次只看一张。
    ///
    /// 一格都取不到（空页表、这一张不在图片索引里）时 `None` —— 按下去什么都不做，
    /// 比开一层「0 / 0」、既滑不动也定不了位诚实（同 [`Self::build_image_sequence`] 那条
    /// 「问不到那一列就这一击什么都不做」的立场）。
    pub(in crate::ui::features::reader) fn current_page_key(&self) -> Option<String> {
        self.page_tiles(self.page)
            .into_iter()
            .find_map(|tile| tile.request)
            .map(|request| request.key)
    }

    /// 第 `page` 页的**文字层**（页内字序 + 逐字盒）。`None` = 这一页没有可读的字：
    /// 漫画包的一页就是一张图，EPUB 的定版式字在 XHTML 里而不是在页上，扫描件没带 OCR。
    ///
    /// 「没有字」与「取字取失败」在这里**故意不分开**：两样在屏上是同一件事 —— 那一页
    /// 选不出字，页的摆位不变（与 core 那条 `page_text` 的契约同一条立场）。
    ///
    /// 这一格按页备忘，理由见 [`Self::text`] 那一栏。
    pub(in crate::ui::features::reader) fn page_text(&self, page: usize) -> Option<Rc<PageText>> {
        self.cached_page_text(page).map(|(text, _)| text)
    }

    /// 第 `page` 页那一份字层此刻的**内容哈希**：批注回显那一份缓存的键
    /// （`annotations::resolved_for` 按它问「这一页的字还是刚才那些字吗」）。
    ///
    /// 与 [`Self::page_text`] 同一次备忘，所以第二问不重开容器。这一页没有字时给
    /// `None`：调用方要的正是「这一页有没有字要画」，而不是「哈希是多少」。
    pub(in crate::ui::features::reader) fn page_text_key(&self, page: usize) -> Option<u64> {
        self.cached_page_text(page).map(|(_, hash)| hash)
    }

    /// 备忘的那几页本身：命中就直接给，没问过才回引擎取一次。
    ///
    /// **「这一页没有字」也要记下来**。不记的话那一族每帧都回引擎重开一次容器：一份
    /// 扫描版 PDF 实测重开一趟 61–374 ms（`format::pdf` 模块头那笔账），画十帧就是半秒，
    /// 而那本书本来一个字也选不中、屏上什么都不该变 —— 付的完全是冤枉钱。缓存里那一格
    /// 因此是 `Option`：`None` 是一份**答案**，不是「还没问过」。
    ///
    /// 留 [`PAGE_TEXT_CACHE`] 页而不是一页：连续竖滚里视口同时压着两页时，一页的缓存
    /// 会让两页每帧互相挤掉对方。命中把那一页挪到末尾，淘汰只从最旧那一头收。
    fn cached_page_text(&self, page: usize) -> Option<(Rc<PageText>, u64)> {
        let mut cache = self.text.borrow_mut();
        if let Some(index) = cache.iter().position(|(at, _, _)| *at == page) {
            let entry = cache.remove(index);
            let out = entry.1.as_ref().map(|text| (text.clone(), entry.2));
            cache.push(entry);
            return out;
        }
        let (text, hash) = match self.doc.page_text(page) {
            Some(taken) => {
                let hash = page_text_hash(&taken);
                (Some(Rc::new(taken)), hash)
            }
            // 没字的那一页哈希记 0：它不参与批注回显那一份缓存（没有字就没有痕迹可解），
            // 所以那个数在这一档没有读者。
            None => (None, 0),
        };
        cache.push((page, text.clone(), hash));
        if cache.len() > PAGE_TEXT_CACHE {
            cache.remove(0);
        }
        text.map(|text| (text, hash))
    }

    /// 留住的那份文档（页图要按 `href` 现取、渲染型的一页要按页号现画，都从它走）。
    ///
    /// 书内搜索那一趟也要它：那一趟按页取文字层，取的就是这一份（`Document::page_texts`）。
    /// 交 `Arc` 而不是引用，是因为那一趟在作业线程上跑，活不过这一屏的借用。
    pub(in crate::ui::features::reader) fn opened(&self) -> Arc<dyn OpenedDocument> {
        self.doc.clone()
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

/// 一页文字层的内容哈希 —— 批注回显那一份缓存的键。
///
/// 数的是**字序**（那一串字，按页内阅读序），不数盒子也不数行号：缓存里存的是
/// 「第几个字到第几个字」这一份解锚结果，而它只跟字有关。同一页换个 `RENDER_EDGE`
/// 档位重画、或把窗口从手机挪到桌面，盒子在屏幕上的位置全变了，但那一段偏移该落在
/// 哪几个字上一格不动 —— 拿盒子当键就会每帧重解一次，白付一遍整页的子串搜索。
///
/// 可重排那一族的同一把尺是 `Document::chapter_hash`（那一章文本的内容哈希）。两族
/// 的算法不同、职责同一个：**键只能是「锚点坐标系所依据的那份内容」**，不能是排版代数。
fn page_text_hash(text: &PageText) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for item in &text.chars {
        item.ch.hash(&mut hasher);
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    //! 定版式状态本身（页表 + 当前页 + 那一格按页的文字层）。不需要窗口：翻页只是一次夹取。
    use std::rc::Rc;
    use std::sync::Arc;

    use folio_core::Locator;
    use folio_core::content::format::{
        Content, DocMeta, FixedDoc, FixedPage, Format, PageChar, PageImage, PageRect, PageSource,
        PageText,
    };

    use super::{FixedBook, PAGE_TEXT_CACHE};

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

    /// 一份**数得出来**的定版式文档：只为了问「回引擎取字层取了几趟、取的是第几页」。
    ///
    /// 计数住在测试这一侧是因为那一笔账没有别的可见症状：漏缓存不报错、不改变屏上的
    /// 任何东西，只让每一帧多付一趟容器。
    struct Counted {
        meta: DocMeta,
        content: Content,
        /// 哪几页给得出字层。不在清单里的那些交 `None` —— 扫描本与漫画那一族的形状。
        text_pages: Vec<usize>,
        /// `Mutex` 而不是 `RefCell`：统一文档接口是 `Send + Sync` 的，而这一份计数只在
        /// 测试里被同一根线程读写 —— 挑一个不会让整本书变得不能跨线程的容器。
        asks: std::sync::Mutex<Vec<usize>>,
    }

    impl folio_core::content::document::Document for Counted {
        fn meta(&self) -> &DocMeta {
            &self.meta
        }

        fn kind(&self) -> folio_core::content::format::ContentKind {
            self.content.kind()
        }

        fn content(&self) -> folio_core::content::document::ContentRef<'_> {
            match &self.content {
                Content::Reflow(flow) => folio_core::content::document::ContentRef::Reflow(flow),
                Content::Fixed(fixed) => folio_core::content::document::ContentRef::Fixed(fixed),
            }
        }

        fn image_bytes(&self, _href: &str) -> Option<Vec<u8>> {
            None
        }

        fn stylesheet(&self) -> String {
            String::new()
        }

        fn chapter_source(&self, index: usize) -> folio_core::Result<String> {
            Err(folio_core::Error::NoSuchChapter(index as i64))
        }

        fn page_text(&self, page: usize) -> Option<PageText> {
            self.asks.lock().unwrap().push(page);
            self.text_pages.contains(&page).then(|| PageText {
                width: 100.0,
                height: 60.0,
                chars: vec![PageChar {
                    ch: 'A',
                    rect: PageRect {
                        x: 0.0,
                        y: 0.0,
                        width: 10.0,
                        height: 12.0,
                    },
                    line: 0,
                }],
            })
        }
    }

    fn counted(pages: usize, text_pages: &[usize]) -> (Arc<Counted>, FixedBook) {
        let fixed = FixedDoc {
            pages: (0..pages)
                .map(|i| FixedPage {
                    source: PageSource::Render { index: i as u32 },
                    width: 100,
                    height: 60,
                    extra_images: Vec::new(),
                })
                .collect(),
            ..Default::default()
        };
        let doc = Arc::new(Counted {
            meta: DocMeta {
                title: "一本 PDF".into(),
                authors: Vec::new(),
                format: Format::Pdf,
                size: 0,
                ..DocMeta::default()
            },
            content: Content::Fixed(fixed),
            text_pages: text_pages.to_vec(),
            asks: std::sync::Mutex::new(Vec::new()),
        });
        let book = FixedBook::new(doc.clone(), Locator::start());
        (doc, book)
    }

    /// 那一格按页备忘，而且**「这一页没有字」也要把答案记下来**。
    ///
    /// 两条各钉一段冤枉钱：
    ///
    /// - **有的那一页不缓存**：选中那一套一帧里要问好几趟（命中、吸词、色块、落批注、
    ///   回显），而 provider 那一头每次问都重开一次容器 —— PDF 实测一份 3.6 MB、45 页的
    ///   书重开一趟 374 ms（`format::pdf` 模块头那笔账）。
    /// - **没有的那一页不缓存**：绘制那一臂每帧问一次，于是扫描本与漫画那一族**每一帧**
    ///   白跑一趟容器 —— 十帧就是半秒，而那一本书屏上本来一个字都不会变。症状是「翻页
    ///   卡」，线索却指向绘制，最难查的那种。
    ///
    /// `Rc::ptr_eq` 钉「第二次拿到的是同一份」，那份计数钉「引擎只被回过一趟」。
    #[test]
    fn the_text_layer_is_memoized_per_page_including_a_missing_one() {
        let (doc, book) = counted(3, &[0]);
        let first = book.page_text(0).expect("第 0 页写着字");
        let again = book.page_text(0).expect("第二次问也该有");
        assert!(Rc::ptr_eq(&first, &again), "同一页两次问到的是同一份");
        assert_eq!(
            doc.asks.lock().unwrap().as_slice(),
            &[0],
            "第二趟不该回引擎"
        );

        // 没字的那一页：答案是 `None`，而这一问也要记账（只回引擎一次）。
        assert!(book.page_text(1).is_none(), "第 1 页没字");
        assert!(book.page_text(1).is_none(), "再问还是没字");
        assert_eq!(
            doc.asks.lock().unwrap().as_slice(),
            &[0, 1],
            "「没有字」也要缓存 —— 不记就是每帧白跑一趟容器"
        );
        // 键与字层共用同一份备忘。渲染帧每帧就是这一串问句（取字、取键、再取字），
        // 而它们问的都是**同一页** —— 所以整串只该回引擎一趟。
        assert!(book.page_text_key(1).is_none(), "没字的那一页没有键可给");
        assert_eq!(
            doc.asks.lock().unwrap().len(),
            2,
            "同一页上问键不该再回引擎"
        );
        // 连续竖滚里两页同时在屏：前一页的字层不能被后一页挤掉，回问仍然拿备忘。
        assert!(
            book.page_text_key(0).is_some(),
            "有字的那一页该给得出缓存的键"
        );
        assert_eq!(
            doc.asks.lock().unwrap().len(),
            2,
            "两页同时在屏时不该来回重开容器"
        );
        // 容量是 [`PAGE_TEXT_CACHE`]：多翻几页之后，最旧的那一页才被淘汰。
        for page in 2..PAGE_TEXT_CACHE {
            assert!(book.page_text(page).is_none(), "第 {page} 页没字");
        }
        let asked = doc.asks.lock().unwrap().len();
        assert!(book.page_text(0).is_some(), "四格之内的页还留着");
        assert_eq!(
            doc.asks.lock().unwrap().len(),
            asked,
            "四格之内的页不该被挤出去"
        );
        // 第 0 页刚被问过键、排在最新一头：再翻满一整格才不会把它挤出去。
        for page in PAGE_TEXT_CACHE..PAGE_TEXT_CACHE * 2 {
            assert!(book.page_text(page).is_none(), "第 {page} 页没字");
        }
        let asked = doc.asks.lock().unwrap().len();
        assert!(book.page_text(0).is_some(), "被挤出去的那页要回引擎重取");
        assert_eq!(doc.asks.lock().unwrap().len(), asked + 1);
    }

    /// 连续竖滚的页栈：页高按版心宽与页面宽高比算，页缝只占位置、不算进页高；
    /// 视口顶落在哪一页、视口盖住哪几页、宽度变了怎么把位置搬回去，都从这一条栈读。
    #[test]
    fn the_fixed_stack_lays_pages_out_by_width_and_maps_y_back_to_pages() {
        let book = book_with(vec![
            FixedPage {
                source: PageSource::Image {
                    href: "tall.png".into(),
                },
                width: 100,
                height: 200,
                extra_images: Vec::new(),
            },
            FixedPage {
                source: PageSource::Image {
                    href: "wide.png".into(),
                },
                width: 200,
                height: 100,
                extra_images: Vec::new(),
            },
            FixedPage {
                source: PageSource::Image {
                    href: "square.png".into(),
                },
                width: 100,
                height: 100,
                extra_images: Vec::new(),
            },
        ]);
        assert!(book.ensure_stack(300.0, 10.0, 0.5), "第一次要建栈");
        assert!(!book.ensure_stack(300.0, 10.0, 0.5), "同一把尺不重建");
        assert!(book.ensure_stack(200.0, 10.0, 0.5), "宽度变了要重建");
        assert!(book.ensure_stack(300.0, 10.0, 0.5), "搬回去也要重建");

        let stack = book.stack().expect("刚建过");
        assert_eq!(stack.len(), 3);
        assert_eq!(stack.height_of(0), Some(600.0), "300 宽 ÷ 0.5 = 600 高");
        assert_eq!(stack.height_of(1), Some(150.0));
        assert_eq!(stack.height_of(2), Some(300.0));
        assert_eq!(stack.top_of(1), Some(610.0), "页缝 10");
        assert_eq!(stack.top_of(2), Some(770.0));
        assert_eq!(stack.total(), 1070.0);
        assert_eq!(book.stack_top_of(1), Some(610.0));

        // 视口顶：页内归本页，页缝一半以前归上一页、之后归下一页。
        assert_eq!(stack.page_at_y(0.0), Some(0));
        assert_eq!(stack.page_at_y(604.0), Some(0));
        assert_eq!(stack.page_at_y(606.0), Some(1));
        assert_eq!(stack.page_at_y(650.0), Some(1));
        assert_eq!(stack.page_at_y(5000.0), Some(2), "越过书尾夹在最后一页");
        assert_eq!(stack.page_and_fraction(300.0), Some((0, 0.5)));
        assert_eq!(
            stack.page_and_fraction(610.0),
            Some((1, 0.0)),
            "页缝里归下一页时从页首算"
        );

        // 视口盖住的页：长页可以一页占满一屏，起点要能收到上一页。
        assert_eq!(stack.visible(0.0, 700.0), 0..2);
        assert_eq!(stack.visible(600.0, 700.0), 0..3);
        assert_eq!(stack.visible(770.0, 100.0), 2..3);
        assert_eq!(stack.visible(1070.0, 100.0), 2..3, "滚到底仍停在最后一页");
    }

    /// 整本装得下一屏时没有「滚到底」：页号仍按视口顶读，不从第一页直接跳成最后一页。
    #[test]
    fn a_book_that_fits_one_screen_keeps_reading_from_the_top_page() {
        let book = book_with(vec![
            FixedPage {
                source: PageSource::Image {
                    href: "a.png".into(),
                },
                width: 100,
                height: 100,
                extra_images: Vec::new(),
            },
            FixedPage {
                source: PageSource::Image {
                    href: "b.png".into(),
                },
                width: 100,
                height: 100,
                extra_images: Vec::new(),
            },
        ]);
        book.ensure_stack(100.0, 12.0, 0.5);
        let stack = book.stack().expect("该有栈");
        assert_eq!(stack.total(), 212.0, "两页加一条缝");
        assert_eq!(
            book.page_at_scroll(0.0, 800.0),
            Some(0),
            "一屏装得下时，视口顶是第 0 页"
        );
    }

    /// 页表没说宽高的那几格按视口比例兜底：整页铺满宽，而不是零面积。
    #[test]
    fn an_unknown_page_size_falls_back_to_the_viewport_aspect() {
        let book = book_with(vec![FixedPage {
            source: PageSource::Image {
                href: "unknown.png".into(),
            },
            width: 0,
            height: 0,
            extra_images: Vec::new(),
        }]);
        book.ensure_stack(400.0, 12.0, 400.0 / 800.0);
        let stack = book.stack().expect("该有栈");
        assert_eq!(stack.height_of(0), Some(800.0), "兜底成一屏高");
        assert_eq!(stack.total(), 800.0);
    }

    /// 底栏那颗「放大整页」用的身份：**当前在读的那一页**那一格，而不是第一格。
    ///
    /// 多图页取主图在前（与那一列图的顺序同一条），空页表取不到 —— 那一格按下去就
    /// 什么都不做，比开一层「0 / 0」诚实。
    #[test]
    fn the_page_key_follows_the_page_being_read() {
        let (_, book) = counted(2, &[0]);
        let first = book.current_page_key().expect("这一页该有身份");
        assert!(
            first.ends_with("/render/0@2048"),
            "身份要说清是哪一页: {first}"
        );
        let mut book = book;
        book.set_page(1);
        let second = book.current_page_key().expect("第二页也该有身份");
        assert_ne!(first, second, "在读的那一页换了，身份跟着换");

        // 一格都没有的书（空页表）取不出身份。
        let empty = FixedBook::new(
            Arc::new(Counted {
                meta: DocMeta {
                    title: "空".into(),
                    authors: Vec::new(),
                    format: Format::Pdf,
                    size: 0,
                    ..DocMeta::default()
                },
                content: Content::Fixed(FixedDoc::default()),
                text_pages: Vec::new(),
                asks: std::sync::Mutex::new(Vec::new()),
            }),
            Locator::start(),
        );
        assert_eq!(
            empty.current_page_key(),
            None,
            "没有可摆的一格就没有身份可问"
        );
    }
}
