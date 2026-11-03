//! 可重排书的两条渲染臂：连续滚动与分页 / 双页。
//!
//! 两条臂仍然各自装配元素树，共用输入 / 产物结构体暂留在 `body`；
//! 下一步把它们收进 `Paper` / `PageScene` 后，这一层只保留按模式选臂。


use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use folio_core::PageTurn;
use folio_core::content::format::css::computed::BackgroundSize;
use folio_core::layout::fragment::column_offset;
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    AnyElement, App, Context, Div, Entity, EntityId, InteractiveElement as _, IntoElement,
    ParentElement, PageCurlMesh, SharedString, Styled, TestSupportExt as _, WeakEntity, Window,
    div, px,
};

use crate::ui::features::reader::components::image_viewer::ImageViewer;
use crate::ui::features::reader::model::annotations::Palette;
use crate::ui::features::reader::model::book::Book;
use crate::ui::features::reader::model::chapter_layout::{Document, Viewport};
use crate::ui::features::reader::model::chrome;
use crate::ui::features::reader::model::image_atlas::ImageAtlas;
use crate::ui::features::reader::model::page_geometry::{BandPolicy, PageBox};
use crate::ui::features::reader::model::prefetch;
use crate::ui::features::reader::model::session::{
    ImageHit, PageRef, ReaderSession, StackHit, Turn, TurnRun, land_at_anchor, start_turn,
};
use crate::ui::features::reader::motion::turn_animation::{self, Sheet};
use crate::ui::features::reader::view::bands;
use crate::ui::features::reader::view::paint::GlyphsElement;
use crate::ui::features::reader::view::scene::{BlockDecor, Scene, VisibleImage};

use super::a11y::spoken_lines;
use super::art::{
    current_page_art, page_art_contain, page_art_element_id, page_art_for_doc,
    page_backdrop_index, scroll_art_pictures,
};
use super::capture::{CaptureView, flat_mesh_canvas};
use super::turn_view;
use super::decor::{decoration_child, decoration_ready};
use super::images::{
    ImageDrag, ImageTap, image_child, painted_picture, plain_image, stack_dots, stack_peek,
};
use super::marks::{MarkSpot, annotation_fills, speech_fill};
use super::paper::{
    CaptureRole, PageScene, Paper, PaperBody, PaperCapture, Picture,
};
use super::shape::shape_runs;
use super::{
    ContinuousArm, FrameOut, PagedArm, RenderFrame, TurnFrame, TurnPaint, column_cut, local_band,
};

/// 一栏渲染要往里写的产物。
struct PageSceneTargets<'a> {
    element: &'a mut GlyphsElement,
    overlays: &'a mut Vec<AnyElement>,
    column: &'a mut Div,
    backdrops: &'a mut Vec<AnyElement>,
    page_bands: &'a mut Vec<AnyElement>,
    out: &'a mut FrameOut,
}

impl<'a> PageSceneTargets<'a> {
    fn reborrow(&mut self) -> PageSceneTargets<'_> {
        PageSceneTargets {
            element: &mut *self.element,
            overlays: &mut *self.overlays,
            column: &mut *self.column,
            backdrops: &mut *self.backdrops,
            page_bands: &mut *self.page_bands,
            out: &mut *self.out,
        }
    }
}

/// 一次纸渲染里捕获副本的收集器；每个分组一份，渲染完再封成 [`PaperCapture`]。
struct CaptureSink {
    front_decor: Vec<AnyElement>,
    front_images: Vec<AnyElement>,
    back_element: GlyphsElement,
    back_children: Vec<AnyElement>,
    incoming_element: GlyphsElement,
    incoming_children: Vec<AnyElement>,
    /// 收进进场副本的正文插图张数（背景图不数，它走 `decoration_ready` 那条判据）。
    incoming_images: u32,
    /// 这一组里有图还没进图集（且图集没判失败）：捕获副本不完整，这一帧不交。
    ///
    /// 纹理一旦记成「已烘」就不会自己重来，缺一张图就一直缺；滑动时读者看到的是
    /// 正文在、图没了。判据与定版式的 `fixed_capture_ready` 同源：拿到像素或已失败
    /// 才算落地。
    front_missing: bool,
    back_missing: bool,
    incoming_missing: bool,
}

impl CaptureSink {
    fn new() -> Self {
        Self {
            front_decor: Vec::new(),
            front_images: Vec::new(),
            back_element: GlyphsElement::new(),
            back_children: Vec::new(),
            incoming_element: GlyphsElement::new(),
            incoming_children: Vec::new(),
            incoming_images: 0,
            front_missing: false,
            back_missing: false,
            incoming_missing: false,
        }
    }
}

/// 一张纸渲染完的产物：已经裁好、已经带一次纸层位移的显示层，外加正面捕获元素。
struct PaperRenderOut {
    layers: Vec<AnyElement>,
    /// 这一张纸自己的读数带（页眉 / 页脚）。**不放进 `layers`**：那一层裁到
    /// `paper.content`（正文区），带子会被裁掉；返回给 `body` 挂在裁切层外面，
    /// 静止时看得见，翻页时按 `Sheet::offset` 跟着纸走。
    page_bands: Vec<AnyElement>,
    front_capture: Option<PaperCapture>,
    back_capture: Option<PaperCapture>,
    incoming_capture: Option<PaperCapture>,
    bands_drawn: bool,
}

/// 一页读数带要的那几格输入。做成结构体而不是七个参数：两个调用点（逐栏实时画、
/// 只画带子的进场/出场页）本来就在不同深度上，传值比传一串位置参数好对。
struct PageBandAsk<'a> {
    book: &'a Book,
    doc: &'a Document,
    page: PageRef,
    /// 物理栏序：双页里左半 0、右半 1。
    column: usize,
    columns: usize,
    page_box: &'a PageBox,
    page_percent: &'a SharedString,
    /// 这一页自己有整页画时读数带透明（画铺到了状态栏那一条）。
    bare: bool,
}

/// 一页的读数带（页眉 / 页脚），坐标已经按这一栏摆好。元素本体的层由调用方决定：
/// 实时页把它挂进每页一份的带子层，只画带子的纸把它包进那张纸的位移里。
fn page_band_element(ask: PageBandAsk<'_>, cx: &App) -> AnyElement {
    let PageBandAsk {
        book,
        doc,
        page,
        column,
        columns,
        page_box,
        page_percent,
        bare,
    } = ask;
    let readouts = bands::readouts_for(
        doc,
        book.chapter_title_at(page.chapter).into(),
        page.page,
        page_percent.clone(),
        cx,
    );
    let page_w = page_box.paper.w / columns.max(1) as f32;
    let page_x = page_w * column as f32;
    let mut half = readouts;
    if columns > 1 {
        if column == 0 {
            half.percent = SharedString::default();
        } else {
            half.chapter = SharedString::default();
            half.position = SharedString::default();
        }
    }
    div()
        .absolute()
        .left(px(page_x))
        .top(px(0.0))
        .w(px(page_w))
        .h_full()
        .id(super::paper::band_id("reader-band-live", page, column))
        .test_support()
        .child(bands::chrome(
            page_box,
            &half,
            bare,
            bands::Width::Parent,
            cx,
        ))
        .into_any_element()
}

/// 一张只画读数带的纸：出场页改由平摊纹理画之后，文字与图都在纹理里，页眉页脚
/// 仍要跟同一张纸走。两张（出场 / 进场）只有网格 id 与采样原点不同。
struct BandOnlyPaper<'a> {
    book: &'a Book,
    page: PageRef,
    sheet: Sheet,
    frame: &'a RenderFrame<'a>,
    mesh_id: &'static str,
    mesh: PageCurlMesh,
    bake_origin: std::rc::Rc<std::cell::Cell<Option<(f32, f32)>>>,
}


impl ReaderSession {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_continuous(
        &mut self,
        book: &mut Book,
        frame: RenderFrame<'_>,
        element: &mut GlyphsElement,
        column: &mut Div,
        overlays: &mut Vec<AnyElement>,
        arm: ContinuousArm<'_>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let render_frame = frame.clone();
        let columns = frame.columns();
        let viewport_width = frame.viewport_width();
        let window_height = frame.window_height();
        let RenderFrame {
            mode,
            viewport_height,
            content_top,
            chapter_gap,
            page_box,
            turn,
            speaking,
            family,
            foreground,
            link,
            atlas,
            view,
            ..
        } = frame;
        let ContinuousArm { backdrops, out } = arm;
        let font_cache = &mut self.font_cache;
        // —— 连续滚动：锚章 ± 窗口拼成一条全局坐标 ——
        // 0. 章盒里正文之前那一截（页眉带 + 安全区）先报进链：`placements` 的
        //    章高要按**画出来的**那一段量，否则每章末尾都会漏出这一截字，
        //    下一章的整页底图正好盖上去（见 `Book::chapter_top`）。
        book.set_chapter_top(content_top);
        // 1. 邻章补齐入库（0 高占位）：排版作业这就能排进后台队列，
        //    狂飙滚动时接缝提前两章就铺好了。窗口铺多远要看**视口顶**
        //    （读者停在长章中后段时，前方那几章也得够一屏多才滚得动）。
        let __t = crate::ui::features::reader::model::perf::Timer::start();
        let __before = book.docs_mut().count();
        book.ensure_window_spans(&self.prefs, viewport_height, self.scroll_y);
        let __new = book.docs_mut().count().saturating_sub(__before) as u32;
        let __spans = __t.ns();
        crate::ui::features::reader::model::perf::add(|frame| {
            frame.spans_ns += __spans;
            frame.spans_new += __new;
        });
        // 2. 按当前（上一帧的）坐标链记下视口顶：排版收完结果好补偿。
        let snapshot = book.spine_snapshot(self.scroll_y, chapter_gap);
        // 3. 排版：锚章先提交（恢复落点等它），邻居随后。连续模式只排版
        //    不切片（一片高 0 是「不分片」的哨兵值）。
        //
        // **递给排版的纵轴基准是整窗高**：连续滚动这一档
        // `Viewport.viewport_height` 只当排版的 `page_height` 用（切片高是 0，
        // 不参与），而它决定「整章只有一张图」那一档把章撑到多高。
        //
        // 从前这里是「纸高」= 窗高 − `reserve.height`（状态栏 + 手势条），
        // 理由是背板也按同一个数铺。但背板是**出血图**：它要盖住状态栏与手势条
        // （`bare_bands` 已经把读数带让开），盒子却是安全区以内那一条 —— 而挑图
        // 与 `cover/contain` 又都按**整窗**比例算。`cover` 按宽放大之后图比盒子
        // 高出 `reserve.height`，`background-position: center` 于是上下各裁一半：
        // 封面顶部被切掉约半个状态栏那么高（读者报的「封面上面被裁切一部分」）。
        //
        // 盒子、挑图、`cover/contain` 判定三处从此同一个数（整窗），封面
        // `0~slim` 那种与屏同比例的一张就是严丝合缝。章高也跟着整窗走，
        // 「撑到哪一格就铺到哪一格」仍然成立，不会压到下一章头上。
        let window_h = f32::from(window.viewport_size().height);
        let __t = crate::ui::features::reader::model::perf::Timer::start();
        for doc in book.docs_mut() {
            doc.ensure_viewport(
                window,
                cx,
                Viewport {
                    width: page_box.content.w,
                    height: 0.0,
                    // 连续滚动的 `%` 基准是**整窗**：封面/扉页靠
                    // `height:100%` 撑满一屏，让开页眉页脚会把满版图缩一截。
                    // 分页那一档才是正文盒的高。
                    page_height: Some(window_h),
                    columns: 1,
                },
            );
        }
        let __layout = __t.ns();
        crate::ui::features::reader::model::perf::add(|frame| {
            frame.layout_ns += __layout
        });
        // 4–7. 坐标链那一段：补偿、恢复落点、锚章迁移、总高与夹取。
        let __spine = crate::ui::features::reader::model::perf::Timer::start();
        let before_compensate = self.scroll_y;
        book.compensate_spine(snapshot, &mut self.scroll_y, chapter_gap);
        // 补偿把整条链挪了多少，正在走的那一趟两端就跟着挪多少：两端留在
        // 补偿前的坐标里，补间走完落的就是**跳了那一跳之后**的位置，读者看见
        // 内容突然闪一下。挪完这一笔，动画跟着坐标链一起走。
        let shift = self.scroll_y - before_compensate;
        if let Some(t) = self.turn.as_mut()
            && let Some((from, to)) = t.scroll
        {
            t.scroll = Some((from + shift, to + shift));
        }
        self.laid_out = crate::ui::features::reader::model::session::LaidOut {
            mode,
            columns,
            pages: 1,
            viewport: viewport_height,
        };
        // 5. 恢复落点：全局 y = 章基线 + 章内行顶。请求可能指向邻章
        //    （跨章进度、换模式、跳章），先把锚章挪过去。
        if let Some(target) =
            self.pending_restore.map(|locator| locator.chapter as usize)
            && target != book.chapter_index()
            && target < book.chapter_count()
        {
            book.set_anchor(target, &self.prefs);
        }
        if self.pending_restore.is_some() && book.document().is_current() {
            let locator = self.pending_restore.take().expect("上面刚判过 Some");
            let locator = land_at_anchor(self.pending_anchor.take(), locator, book);
            self.scroll_y = book
                .global_y_of_offset(
                    locator.chapter as usize,
                    locator.offset,
                    chapter_gap,
                )
                .unwrap_or_else(|| book.document().y_at_offset(locator.offset));
            // 恢复是一次跳转（开书、跳目录、换摆法），不是翻一页：让一张从旧位置
            // 起点的纸平移过来，读者看见的就是自己刚选的那一秒在飞。
            //
            // 这里写字段而不调 [`ReaderSession::cancel_turn`]：那一臂里
            // `font_cache` 还从 `self` 上借着自己，任何 `&mut self` 的方法都过不去，
            // 只有逐字段的写法合法（`land_at_anchor` 那条注释记的是同一件事）。
            self.turn = None;
        }
        // 6. 锚章迁移：视口顶跨进了邻章（锚章自己排完才有资格答）。
        let before_migrate = self.scroll_y;
        book.migrate_spine(&mut self.scroll_y, chapter_gap, &self.prefs);
        // 迁移就是「视口顶换了一章」：补间那两端是换手之前那条链上的数，让它
        // 继续走完等于把读者送到隔壁一章的中间去。当场落定，这一帧就给他目标屏。
        if (self.scroll_y - before_migrate).abs() >= 0.5 {
            self.turn = None;
        }
        // 整页画（固定底图章 / 单图页）：判定放在锚章迁移之后，读的就是这一帧
        // 视口顶所在的那一章。宽高比用整窗：同一本书里 `0.jpg` / `0~slim.jpg`
        // 谁更像这块屏，就挑谁。
        let want_aspect =
            viewport_width / window_height.max(1.0);
        out.page_art = current_page_art(book, want_aspect);
        // 7. 总高与夹取：全局坐标里「滚到底」是整条链的末端。
        self.content_height = book.content_height(chapter_gap);
        let max_scroll = (self.content_height - viewport_height).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, max_scroll);
        // 这一帧**画在**哪一个全局 y：动画在走就落在补间上，没有动画就是
        // `scroll_y` 自己。它是绘制与命中共用的唯一一个数 —— `to_layout` 读的
        // 就是 `scroll_painted`，两处各读各的话，补间那两百多毫秒里按在字上的
        // 手指会指到隔壁那一行去。终点可能因为总高在动画期间缩水而越界，夹一下。
        let painted_y = match turn {
            Some(frame) => frame
                .scroll
                .map_or(self.scroll_y, |(from, to)| {
                    chrome::tween(from, to, frame.progress)
                })
                .clamp(0.0, max_scroll),
            None => self.scroll_y,
        };
        self.scroll_painted = painted_y;
        // 整页画的背板：**每一章各一张**，跟着这一章滚，铺满整窗。只画当前
        // 锚章的话，短章/章界会先空出一截白；逐章画、后一章盖前一章，图与图
        // 之间就不会有缝。固定底图章也走这条 —— `background-attachment:
        // fixed` 在滚动阅读里要的是「这张图跟着这一章走」，钉在窗口上反而不
        // 动（读者报的「底图不会跟着滚」）。
        let size = window.viewport_size();
        let anchor = book.chapter_index();
        // 背板铺的是**这一章自己的盒子**：盒子从章基线起，里面先是页眉带那
        // 一截（`Book::chapter_top`），再是正文。图顶因此就是章盒顶，不让开
        // `content_top` —— 上一章的正文现在也不会漏进这一格（章高把它算进去了），
        // 让开只会让两张满版图之间空出一条缝（读者报的「图片之间有间隙」）。
        let art_top = |base: f32| base - painted_y;
        // 图高：章盒与「整窗 + 页眉带」里矮的那个。整页画那一档的盒子正好是
        // 这个数（排版侧撑到整窗，盒子再加 `content_top`），两张满版图于是首尾
        // 相接；比屏还高的插图章只铺一屏多一点，不溢进下一章。这两个数从前
        // 都按整窗取，与「章盒里还有一截页眉」差着一个 `content_top` —— 章的
        // 正文被算进链里之后，铺贴这一侧才跟着对齐。
        let art_h = |height: f32| height.min(window_h + content_top);
        // 邻章先画、**锚章最后画**：每张背板都盖住自己这一格，后画的会压住
        // 前一张的下半截。锚章最后入树，它这一屏的底图才压得住下一章从章首
        // 开始铺上来的那张（读者报的「封面下半截被下一章的黑底盖住」）。滚动
        // 换锚时下一章自然接上。
        let mut anchor_h = 0.0;
        for placed in book.placements(chapter_gap) {
            if placed.index == anchor {
                // 锚章那一张排在最后画，高度先记下来。
                anchor_h = art_h(placed.height);
                continue;
            }
            let base = placed.base_y;
            let Some(doc) = book.span_doc(placed.index) else {
                continue;
            };
            let Some(art) = page_art_for_doc(placed.index, doc, want_aspect) else {
                continue;
            };
            let height = art_h(placed.height);
            if height <= 0.0 {
                continue;
            }
            let top = art_top(base);
            if top > window_h || top + height < 0.0 {
                continue;
            }
            let pictures =
                scroll_art_pictures(&art, f32::from(size.width), height, doc.images());
            if let Some(decor) =
                crate::ui::features::reader::view::scene::backdrop_decor(
                    &pictures,
                    0.0,
                    top,
                    f32::from(size.width),
                    height,
                    doc.images(),
                )
            {
                backdrops.extend(decoration_child(decor, &atlas, view, cx));
                out.neighbour_backdrop = true;
            }
        }
        if let Some(art) = &out.page_art
            && let Some(doc) = book.span_doc(art.chapter)
        {
            let base = book.base_of(art.chapter, chapter_gap).unwrap_or(0.0);
            // 锚章与邻章同一口径：图顶 = 这一节盒子的顶（章基线 − 滚量），
            // 图高 = 盒子与「整窗 + 页眉带」里矮的那个。锚章还没进链时
            // （`anchor_h == 0`）退回整窗，与从前一样先铺一屏。
            let height = if anchor_h > 0.0 {
                anchor_h
            } else {
                art_h(window_h)
            };
            let pictures =
                scroll_art_pictures(art, f32::from(size.width), height, doc.images());
            if let Some(decor) =
                crate::ui::features::reader::view::scene::backdrop_decor(
                    &pictures,
                    0.0,
                    art_top(base),
                    f32::from(size.width),
                    height,
                    doc.images(),
                )
            {
                backdrops.extend(decoration_child(decor, &atlas, view, cx));
            }
        }
        // 整页画（单图页或固定底图章）一律让读数带透明：底图要铺到状态栏与
        // 手势条，不透明的带子会在上下各切掉一条。压在画上的字属于这张画，
        // 不为它留夹在两条带子之间的那一档（判据见下面那一句）。
        // 连续滚动里「下一张纸」是下面那一屏：只要这一章还没滚到底，就往下预解；
        // 滚到底之后往回看上一屏。判据只看坐标链的两端，不看章 —— 章界那一头
        // 由 `prefetch::scroll_targets` 按全局带换到邻章去。
        out.prefetch_side = prefetch::side_for_scroll(
            self.scroll_y,
            (self.content_height - viewport_height).max(0.0),
        );
        let __spine = __spine.ns();
        crate::ui::features::reader::model::perf::add(|frame| {
            frame.spine_ns += __spine
        });
        // 8. 可见带 → 逐章切场景：每章用自己的排版与图片索引，
        //    换算只差一个章基线。没排完的章不进链，自然一帧都不画。
        let band = painted_y..(painted_y + viewport_height);
        // 读数带只给**没有可读正文**的整页画让路（封面、整页插图页）：这种
        // 页上的字与底图是同一张画，把画夹进两条带子之间会切掉上下两条。
        //
        // 有正文的章反过来 —— 底图只铺内容区、两条带子照常不透明
        // （2026-11-01 深夜读者改的口径）。否则《诡秘之主》`body.fm1` 那一族
        // （固定底图 + 二十来屏人物志）滚到哪一屏都全屏：页眉那行章名直接压在
        // 正文上、正文压进页脚，全屏档的规矩被套到了正文页头上。
        // 判据是 `Document::has_text`（有非空白字符），不是「有没有字宽」——
        // 卷首常摆一行 `&nbsp;` 把根撑开，那种仍算没有正文。
        // 整页画一律让读数带透明：底图铺到状态栏与手势条，字压在画上是
        // 作者自己的摆法（2026-11-01 深夜读者重新拍板：诡秘分页也要全屏）。
        out.bare_bands = out.page_art.is_some();
        // 整页画那一章按**窗口**坐标画，可见的不是内容区那 719，而是整扇窗：
        // 上沿多安全区、下沿多页脚那一段。它的裁剔带要跟着加长，否则页底
        // 那一截（人物志那几行字正落在那里）会被悄悄裁掉。
        let window_extra = (window_height - viewport_height).max(0.0);
        let mut scenes: Vec<PageScene> = Vec::new();
        for placed in book.placements(chapter_gap) {
            // 这一章的整页画（固定底图 / 单图页）。固定底图还要画正文，
            // 只有 picture page 才整章跳过；两者的根底图都交给背板，不许在
            // 版心里再画一遍 —— 那会把整窗背板裁成章高那一块，章界留白。
            let page_art_here =
                out.page_art.as_ref().filter(|art| art.chapter == placed.index);
            let picture_only = page_art_here.is_some_and(|art| art.picture_only);
            if picture_only {
                continue;
            }
            let scene_end = if page_art_here.is_some() {
                band.end + window_extra
            } else {
                band.end
            };
            if placed.base_y + placed.height < band.start || placed.base_y > scene_end {
                continue;
            }
            let Some(doc) = book.span_doc(placed.index) else {
                continue;
            };
            // 这一章有没有根上的整页底图（判据见 [`page_backdrop`]）。块里那
            // 一份一律跳过：整窗背板已经由上面的逐章循环画过，再在版心里画一遍
            // 就是「图多出来一截」。
            let taken_over = page_backdrop_index(doc);
            // 整页画的正文按窗口坐标画（背板也是）：顶边从窗口顶起，而不是
            // 页眉带下面那一格 —— 作者把字摆在画上，两个坐标系差着一个页眉
            // 加一条安全区。普通页仍是页眉带。
            let top = if picture_only { 0.0 } else { content_top };
            // 这一章在排版坐标里的可见带：**只减章基线**，不要再减 `top`。
            //
            // 绘制那一句是 `屏 = base + top + line.y − scroll`，所以屏上
            // `[top, top + 视口高]` 那一段正好对应排版坐标 `[scroll − base,
            // scroll − base + 视口高]` —— `top` 在两边同时出现，一减就重复了。
            // 减它的后果是可见带整体上移一截：图片行高、只是被吃掉下半截看不
            // 出来，文字行矮，屏底就空出两行（2026-11-01 真机「下边距变大」）。
            let local = local_band(band.start..scene_end, placed.base_y);
            // 可见行区间向引擎要一次，画与念共用同一个数：两处各要一次，
            // 念出来的与画出来的就会差一行。
            let lines = doc.visible_lines(local.clone());
            if speaking {
                out.spoken.extend(spoken_lines(doc, lines.clone()));
            }
            let __t = crate::ui::features::reader::model::perf::Timer::start();
            let mut scene = Scene::build(
                doc,
                font_cache,
                family,
                lines,
                local.clone(),
                band.start - placed.base_y,
                // 连续滚动不分片，也就永远不走竖排（`allow_vertical` 那一档），
                // 长度这一格在横排那条路里根本不参与，递整章的长只为读起来不心虚。
                doc.height(),
                self.prefs.margin_x,
                top,
                foreground,
                link,
                taken_over,
            );
            let __scene = __t.ns();
            let __sh = crate::ui::features::reader::model::perf::Timer::start();
            shape_runs(&mut scene, doc, window);
            let __sh = __sh.ns();
            crate::ui::features::reader::model::perf::add(|frame| {
                frame.shape_runs_ns += __sh
            });
            let (__runs, __images) =
                (scene.runs.len() as u32, scene.images.len() as u32);
            crate::ui::features::reader::model::perf::add(|frame| {
                frame.scene_ns += __scene;
                frame.scenes += 1;
                frame.scene_runs += __runs;
                frame.images += __images;
            });
            out.upcoming.append(&mut scene.upcoming);
            scenes.push(PageScene {
                identity: PageRef {
                    chapter: placed.index,
                    page: 0,
                },
                column: 0,
                origin_y: band.start - placed.base_y,
                height: band.end - band.start,
                range: local,
                role: CaptureRole::Live,
                back_copy: false,
                incoming_copy: false,
                x: self.prefs.margin_x,
                top,
                cut: None,
                picture: None,
                art_drawn: false,
                scene,
            });
        }
        let paper = Paper::continuous(
            PageRef {
                chapter: book.chapter_index(),
                page: 0,
            },
            frame.page_box,
            Sheet {
                offset: (0.0, 0.0),
                z: 0,
                cut: None,
            },
            scenes,
        );
        let mut page_bands = Vec::new();
        let targets = PageSceneTargets {
            element: &mut *element,
            overlays: &mut *overlays,
            column: &mut *column,
            backdrops,
            page_bands: &mut page_bands,
            out,
        };
        let _ = self.render_paper(&*book, paper, &render_frame, targets, window, cx);
    }
}

impl ReaderSession {
    /// 画一张只有读数带的纸：出场页 / 进场页改由平摊纹理画之后，文字与图都在纹理里，
    /// 页眉页脚仍要跟同一张纸走。两张只有网格 id 与采样原点不同。
    fn render_band_only_paper(
        &self,
        ask: BandOnlyPaper<'_>,
        paper_layers: &mut Vec<AnyElement>,
        page_bands: &mut Vec<AnyElement>,
        bands_drawn: &mut bool,
        cx: &App,
    ) {
        let BandOnlyPaper {
            book,
            page,
            sheet,
            frame,
            mesh_id,
            mesh,
            bake_origin,
        } = ask;
        paper_layers.push(flat_mesh_canvas(
            mesh_id,
            mesh,
            (self.enter_shift, frame.layer_top),
            bake_origin,
        ));
        let Some(doc) = book.span_doc(page.chapter) else {
            return;
        };
        let columns = frame.columns();
        for column in 0..columns {
            let band = page_band_element(
                PageBandAsk {
                    book,
                    doc,
                    page,
                    column,
                    columns,
                    page_box: &frame.page_box,
                    page_percent: &frame.page_percent,
                    bare: false,
                },
                cx,
            );
            page_bands.push(
                div()
                    .absolute()
                    .left(px(sheet.offset.0))
                    .top(px(sheet.offset.1))
                    .w(px(frame.page_box.paper.w))
                    .h(px(frame.page_box.paper.h))
                    .child(band)
                    .into_any_element(),
            );
            *bands_drawn = true;
        }
    }
}

impl ReaderSession {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_paged(
        &mut self,
        book: &mut Book,
        frame: RenderFrame<'_>,
        element: &mut GlyphsElement,
        overlays: &mut Vec<AnyElement>,
        arm: PagedArm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let render_frame = frame.clone();
        let columns = frame.columns();
        let viewport_width = frame.viewport_width();
        let window_height = frame.window_height();
        let area = frame.area();
        let sheet_area = frame.sheet_area();
        let viewport = frame.viewport();
        let RenderFrame {
            mode,
            viewport_height,
            content_top,
            page_box,
            turn,
            page_turn,
            turn_over,
            gap,
            fixed_layout,
            family,
            foreground,
            link,
            atlas,
            view,
            speaking,
            ..
        } = frame;
        let PagedArm {
            backdrops,
            paint,
            paper_layers,
            page_bands,
            out: frame_out,
        } = arm;
        // —— 分页 / 双页：当前页 +（动画时）同章邻页或跨章目标页 ——
        // 这一档不拼坐标链：章高按「一页」量，页表自己就带着页眉页脚。
        // 清掉上一档留下的那个数，免得 `placements` 被谁拿去读时多算一截。
        book.set_chapter_top(0.0);
        let capture = CaptureView::new(
            self.capture_dirty,
            self.captured_size,
            self.captured_page,
            self.captured_incoming,
        );
        let metrics = page_box.metrics().expect("可重排分页必须有 PageMetrics");
        let paged = Viewport {
            width: metrics.column_width,
            height: viewport_height,
            page_height: Some(viewport_height),
            columns,
        };
        let column_width = book.document_mut().ensure_viewport(window, cx, paged);
        let pages = book.document().page_count();
        // 整页图按整窗比例挑源（单页与双页分别铺满一帧 / 半帧）。
        let want_aspect =
            viewport_width / window_height.max(1.0);
        // `background-attachment: fixed` 的底图章按**这一页**摊（见下面 `papers`
        // 那一趟）：翻页途中两张纸各带各的底图走，平移 / 覆盖才像一整张纸在动，
        // 而不是「字在滑、画留在原地」。从前这一块只在 `turn.is_none()` 时画，
        // 于是除了仿真折页（底图烘进了捕获纹理），其余几档一开翻底图就整块消失
        // ——读者那句「只有仿真翻页是真把一整张屏幕当成一张纸」量的就是它。
        self.laid_out = crate::ui::features::reader::model::session::LaidOut {
            mode,
            columns,
            pages,
            viewport: viewport_height,
        };

        // 上次读到第几个字 → 该摆在哪一页。恢复请求可能指向邻章
        // （跨章进度）：先把锚章挪过去，等它的排版做完那一帧再落地。
        if let Some(target) =
            self.pending_restore.map(|locator| locator.chapter as usize)
            && target != book.chapter_index()
            && target < book.chapter_count()
        {
            book.set_anchor(target, &self.prefs);
        }
        if self.pending_restore.is_some() && book.document().is_current() {
            let locator = self.pending_restore.take().expect("上面刚判过 Some");
            let locator = land_at_anchor(self.pending_anchor.take(), locator, book);
            let line = book.document().line_at_offset(locator.offset);
            self.page = book
                .document()
                .page_of_line(line)
                .min(pages.saturating_sub(1));
            // 与连续臂同一句：恢复是一次跳转，不是翻一页，不该让一张从旧页号
            // 起点的纸平移过来。
            self.turn = None;
            self.pending_chapter_turn = None;
        }
        self.page = self.page.min(pages.saturating_sub(1));
        // 图预解：这一屏上下紧挨着的那一张纸（章末那一头换到邻章去，见
        // `prefetch::paged_targets`）。`pages` 双页时是张数，这里不与它挂钩：
        // 预解只关心「还有没有下一次翻页」。
        frame_out.prefetch_side = prefetch::side_for_paged(
            self.page,
            pages,
            book.chapter_index(),
            book.chapter_count(),
        );

        // 邻章预排版：跨章滑动要两张纸同时上屏，目标章必须先有排版与切页。
        // 当前章排完才铺邻章，避免刚换章时把三章的重排一起压进队列。
        let current = book.chapter_index();
        if book.document().is_current() {
            for delta in [-1isize, 1] {
                let target = current as isize + delta;
                if target < 0 || target as usize >= book.chapter_count() {
                    continue;
                }
                let target = target as usize;
                book.ensure_paged_span(target, &self.prefs);
                if let Some(doc) = book.span_doc_mut(target) {
                    doc.ensure_viewport(window, cx, paged);
                }
            }
        }

        // 读者在章界按了一下：目标章一排好就起滑。排版没落地时保持当前页
        // 不动 —— 先切章再等排版会留下白屏，正是跨章瞬切最难看的那个空档。
        if let Some(delta) = self.pending_chapter_turn {
            let target = current as isize + delta;
            if target >= 0
                && (target as usize) < book.chapter_count()
                && let Some(doc) = book.span_doc(target as usize)
                && doc.is_current()
            {
                let target = target as usize;
                let target_pages = doc.page_count().max(1);
                let target_page = if delta > 0 {
                    0
                } else {
                    target_pages.saturating_sub(1)
                };
                // 起这一趟走 [`start_turn`]，不在这里手摆：从前这里写死
                // `seq: 0` 再把计数器加一，于是**每一趟**跨章用的都是同一把时钟键
                // —— 第二次跨章时那把键上还挂着上一趟「已经走完」的状态，
                // `Presence` 第一帧就给回 1.0，章当场落定。
                self.pending_chapter_turn = None;
                let arriving = PageRef {
                    chapter: target,
                    page: target_page,
                };
                let started = start_turn(
                    &mut self.turn,
                    &mut self.turn_seq,
                    page_turn,
                    turn_over,
                    mode,
                    Turn {
                        leaving: Some(PageRef {
                            chapter: current,
                            page: self.page,
                        }),
                        arriving: Some(arriving),
                        forward: delta > 0,
                        ..Default::default()
                    },
                );
                // 这一档不挂时钟（「不走动画」就是它）：换章只能当场落。没有这一句，
                // pending 已经清掉、又没有动画走完那一帧去调 `finish_chapter_turn`
                // —— 这一步就静默丢了，读者读到的是「章末点两侧没反应」。
                if !started {
                    frame_out.land_chapter = Some(arriving);
                }
                // 这一帧的 `turn` 已经在上面采过样了；再要一帧才轮到这次滑动。
                window.request_animation_frame();
            }
        }

        let TurnPapers {
            papers,
            count,
            leaf_turn,
            leaf_column,
            other_column,
            turn_leaving,
        } = self.plan_turn_papers(&render_frame, &capture, paint, cx);

        // 入树顺序就是绘制层级（后入树的压在上面）。两张同层时保持上面的
        // 原顺序，所以平移仍然读成一条纸带整体在走。
        let mut order = [0usize, 1];
        if count == 2 && papers[0].sheet.z > papers[1].sheet.z {
            order.swap(0, 1);
        }
        // 出场页这一帧改用静止时烘好的整页纹理。左右平移 / 上下平移 / 覆盖
        // 三档从前逐帧重画字形，文字会随子像素档位在帧间换位图（读者看到的
        // 是「图上的字在闪」）；纹理搬过去就没有这一层重栅格化。装配两臂共用
        // （见 `turn_view::plan_flat`），这里只交上「哪些页不许进捕获层」。
        let page_no = self.page;
        let flat = turn_view::plan_flat(&turn_view::FlatAsk {
            turn,
            // 三处平摊捕获共用同一条排除：折页自己画（网格那张由它自己演）、定版式
            // 还没有捕获链、整页画铺在元素树外面，烘进去只有纸色。
            eligible: turn.is_some_and(|frame| frame.style != PageTurn::Curl)
                && !fixed_layout
                && frame_out.page_art.is_none(),
            current,
            page_no,
            papers: &papers,
            count,
            sheet_area,
            area,
            capture: &capture,
            viewport,
            paper: cx.theme().background.to_rgb(),
            leaving_ready: &|page| {
                book.span_doc(page.chapter)
                    .is_some_and(|doc| page_art_for_doc(page.chapter, doc, want_aspect).is_none())
            },
            arriving_ready: &|page| {
                book.span_doc(page.chapter).is_some_and(|doc| {
                    doc.is_current()
                        && page_art_for_doc(page.chapter, doc, want_aspect).is_none()
                })
            },
        });
        let flat_capture = flat.leaving;
        let flat_incoming = flat.arriving;
        paint.incoming_bake = flat.incoming_bake;
        paint.mesh_page_drawn = flat.mesh_drawn;
        let mut target_column = div().size_full().relative();
        let mut target_page_bands = Vec::new();
        for which in order.into_iter().take(count) {
            let (page_ref, at) = (papers[which].identity, papers[which].sheet);
            let mut body = Vec::new();
            if flat_capture.is_some() && turn_leaving == Some(page_ref) {
                let mesh = flat_capture.clone().expect("上一行刚判过 Some");
                // 出场页的纹理只管正文；页眉/页脚仍实时画一份放在纹理**之上**，
                // 跟着同一张纸的 `at` 走。从前只靠正面纹理里烘的那一条，纹理那一趟
                // 没把它带上（或采样偏了）时，滑动中的旧页就没有页眉。
                self.render_band_only_paper(
                    BandOnlyPaper {
                        book: &*book,
                        page: page_ref,
                        sheet: at,
                        frame: &render_frame,
                        mesh_id: "reader-flat-capture",
                        mesh,
                        bake_origin: self.curl_bake_origin.clone(),
                    },
                    paper_layers,
                    page_bands,
                    &mut frame_out.bands_drawn,
                    cx,
                );
                continue;
            }
            if flat_incoming.is_some() && paint.arriving == Some(page_ref) {
                let mesh = flat_incoming.clone().expect("上一行刚判过 Some");
                self.render_band_only_paper(
                    BandOnlyPaper {
                        book: &*book,
                        page: page_ref,
                        sheet: at,
                        frame: &render_frame,
                        mesh_id: "reader-flat-incoming",
                        mesh,
                        bake_origin: self.incoming_bake_origin.clone(),
                    },
                    paper_layers,
                    page_bands,
                    &mut frame_out.bands_drawn,
                    cx,
                );
                continue;
            }
            let Some(doc) = book.span_doc(page_ref.chapter) else {
                continue;
            };
            // 这一页的整页底图（`background-attachment: fixed` 那一档）。**按页**
            // 摊而不是按屏摊：平移 / 上下平移 / 覆盖这几档翻页时两张纸同时在屏上，
            // 各带各的底图才是一整张纸在走。仿真折页例外 —— 旧页那张已经在捕获
            // 纹理里（网格自己把它卷走），主场景只铺目标页那张，否则卷起来底下
            // 还压着一张不动的旧底图。
            //
            // 只认排好版的那一帧（`is_current`）：跨章目标还没排完时它既没有
            // 种子也没有行盒，铺出来是一张上一版留下的画。
            let art = if doc.is_current() {
                page_art_for_doc(page_ref.chapter, doc, want_aspect)
            } else {
                None
            };
            let art_drawn = art.as_ref().is_some_and(|art| !art.picture_only)
                && (paint.mesh.is_none() || paint.arriving == Some(page_ref));
            // 这一页的根底图由阅读器当整窗背板画（就是下面这一块），版心里那份
            // 别再画一遍 —— 位置还差着一个页眉与安全区。**按页**判，不是按屏：
            // 翻页途中两张纸是两章。
            let taken_over = page_backdrop_index(doc);
            if art_drawn && let Some(art) = &art {
                let size = window.viewport_size();
                let width = f32::from(size.width);
                let height = f32::from(size.height);
                if let Some(decor) =
                    crate::ui::features::reader::view::scene::backdrop_decor(
                        &art.pictures,
                        0.0,
                        0.0,
                        width,
                        height,
                        doc.images(),
                    )
                {
                    // 位移挂在这一层上、里面那张画按 0 画：平移 / 覆盖的位移
                    // 与正文用的是同一个数（`Sheet::offset`），宿主测试也量得到
                    // 这张画到底走没走 —— 从前它只在静止帧画，翻页一开始就整个
                    // 消失，读者看到的是「字在滑，纸没动」。
                    backdrops.push(
                        div()
                            .absolute()
                            .left(px(at.offset.0))
                            .top(px(at.offset.1))
                            .w(px(width))
                            .h(px(height))
                            .id(page_art_element_id(page_ref.chapter, page_ref.page))
                            .test_support()
                            .children(decoration_child(decor, &atlas, view, cx))
                            .into_any_element(),
                    );
                }
                frame_out.page_picture = true;
                // 底图铺到了状态栏与手势条那两条：读数带得透明，不然会在画上
                // 盖出两块底色（这一页自己的带子按 `art_drawn` 判）。
                frame_out.bare_bands = true;
            } else if count == 2 && art.is_none() {
                // 另一张纸的整页画已经铺上整窗（`page_picture` 让正文盒的底色
                // 让开了），这一页自己却没有画：给它垫一块不透明的纸色，否则
                // 另一章那张底图会从这一页的字底下透上来（跨章翻页的现场）。
                let size = window.viewport_size();
                backdrops.push(
                    div()
                        .absolute()
                        .left(px(at.offset.0))
                        .top(px(at.offset.1))
                        .w(px(f32::from(size.width)))
                        .h(px(f32::from(size.height)))
                        .bg(cx.theme().background)
                        .into_any_element(),
                );
            }
            let pages = doc.pages();
            let Some(page) = pages.get(page_ref.page) else {
                continue;
            };
            for index in 0..columns {
                let Some(slice) = page.column(index) else {
                    continue;
                };
                if slice.empty {
                    continue;
                }
                // 分片的“页高”与 `PageBox.content` 必须是同一个数。真机上
                // 出现过「正文落到页脚下面」，这一条在 release 里只记一笔，
                // 不改变绘制；debug 宿主测试里它会把越界的那一页指出来。
                if page_ref.chapter == book.chapter_index()
                    && slice.height > page_box.content.h + 1.0
                {
                    tracing::warn!(
                        target: "folio_app::perf",
                        page = page_ref.page,
                        slice_h = slice.height,
                        content_h = page_box.content.h,
                        start = slice.lines.start,
                        end = slice.lines.end,
                        "page slice taller than the content box"
                    );
                }
                // 这一栏在**物理**上靠左还是靠右：判据一律读 `column`，不读
                // `index` —— RTL 的书 `slices()` 按逻辑栏序给，两者是反的。
                let is_leaf = leaf_turn && slice.column == leaf_column;
                let is_other = leaf_turn && slice.column == other_column;
                // 双页里新跨页的**另一半**这一趟不进主场景：那是被揭起来的叶子落下去
                // 之后才出现的一面，由背面那张纹理演（下面 `back_copy` 把它烘进层 2）。
                // 单面那几档（平移 / 上下平移 / 覆盖）不再有这一格 —— 它们在双页里按
                // 整块版心走，两栏同属那张移动的纸，缺任何一栏读者就看到「一半先变」。
                // 折页那一趟这一栏归谁：
                //  - 揭起来那张纸的正面（leaf 那一栏）在 GPU 路径由网格从捕获纹理里
                //    取，主场景不再画它；CPU 兜底（纹理不可用那一帧）仍画，按折线裁字。
                //  - 背面那一栏（目标跨页的另一半）任何情况下都不进主场景 —— 它是纸
                //    的背面，只该出现在卷起来的那一片上。
                let role = if leaf_turn && paint.arriving == Some(page_ref) && is_other {
                    CaptureRole::Back
                } else if leaf_turn
                    && paint.mesh.is_some()
                    && turn_leaving == Some(page_ref)
                    && is_leaf
                {
                    CaptureRole::Front
                } else if leaf_turn && is_other {
                    CaptureRole::StaticOther
                } else if paint.arriving == Some(page_ref) {
                    CaptureRole::Incoming
                } else {
                    CaptureRole::Live
                };
                // 背面 / 进场页的捕获还要看这一帧有没有活儿要烘：角色说这一栏属于哪一面，
                // `back_key` / `incoming_bake` 说这一面这一帧要不要真写。
                // 单页折页没有「另一半」那一栏：背面就是目标页整张，所以进场页
                // 自己也要写一份背面副本（从前只在 `leaf_turn` 的 `Back` 栏收，
                // 单页那一趟背面因此一直是空的）。双页仍只由 `Back` 那一栏写。
                let back_copy = paint.back_key.is_some()
                    && (role.writes_back()
                        || (!leaf_turn && paint.arriving == Some(page_ref)));
                let incoming_copy = role.writes_incoming() && paint.incoming_bake.is_some();
                if !role.draws_main() && !back_copy {
                    continue;
                }
                // 这一栏的字按哪一条折线裁（判据与它的理由在 [`column_cut`]）。
                let cut = column_cut(leaf_turn, is_leaf, &at);
                // 栏位从 `PageBox` 来：`content.x` 已经让过页边距，
                // `column(column)` 与引擎分片用的是同一对除法/位移。
                let x = page_box.column(slice.column).x;
                let top = content_top;
                // 这一栏是整页图吗：是的话图源按屏比例挑，图本身不排进
                // 版心，改成铺满这一页（单页整窗、双页半窗）的纸面图。
                // 整页图这一档与连续滚动统一：只有**这一章没有可读正文**时
                // 才把那一页拉满屏。正文页里「这一页恰好只有一张图」也按
                // 内容区摆（滚动档就是那样），两边观感一致。
                let page_w = viewport_width / columns as f32;
                let page_x = page_w * slice.column as f32;
                let picture = slice.picture_page.first().map(|base| {
                    let href = doc
                        .images()
                        .closest_portrait_variant(base, want_aspect)
                        .unwrap_or_else(|| base.clone());
                    let contain = page_art_contain(doc, &href, want_aspect);
                    (href, page_x, page_w, 0.0, contain)
                });
                // 背面那一栏要横向挪进 leaf 那一格：背面纹理按 leaf 那块矩形烘、网格
                // 也按 leaf 的尺寸采，而这一栏算出来的 x 是它自己那一栏的位置。
                if back_copy && leaf_turn {
                    paint.back_dx = column_offset(column_width, gap, leaf_column)
                        - column_offset(column_width, gap, slice.column);
                }
                // 分页/双页这一臂的「在屏」就是这一片自己的行区间（片是引擎切的，
                // 一片一栏）。两栏按 `index` 依次接上去：`slices()` 的顺序是
                // 「页优先、页内按栏」的**逻辑**栏序，所以 RTL 的书念出来也是
                // 逻辑顺序 —— 与那一栏画在左边还是右边无关。只为**上屏**的那一栏
                // 收集：背面那一栏不在屏上，念出来就是把同一句话念两遍。
                if speaking && role.draws_main() {
                    frame_out.spoken.extend(spoken_lines(doc, slice.lines.clone()));
                }
                let font_cache = &mut self.font_cache;
                let mut scene = Scene::build(
                    doc,
                    font_cache,
                    family,
                    slice.lines.clone(),
                    slice.origin_y..(slice.origin_y + slice.height + 1.0),
                    slice.origin_y,
                    slice.height,
                    x,
                    top,
                    foreground,
                    link,
                    taken_over,
                );
                let __sh = crate::ui::features::reader::model::perf::Timer::start();
                shape_runs(&mut scene, doc, window);
                let __sh = __sh.ns();
                crate::ui::features::reader::model::perf::add(|f| {
                    f.shape_runs_ns += __sh
                });
                // 整页图：清掉排版层那张流内图，换成铺满这一页的纸面图；
                // 外面再压一层同图的背板，专门把状态栏 / 手势条那两条也铺到。
                // 背板只在这一页上屏时加（leaf 被捕获带走的那一帧不再画它，
                // 否则纸已经卷起来了、顶上那条还留着原图）。
                let page_scene = PageScene {
                    identity: page_ref,
                    column: index,
                    origin_y: slice.origin_y,
                    height: slice.height,
                    range: slice.origin_y..(slice.origin_y + slice.height + 1.0),
                    role,
                    back_copy,
                    incoming_copy,
                    x,
                    top,
                    cut,
                    picture,
                    art_drawn,
                    scene,
                };
                body.push(page_scene);
            }
            let paper = Paper::reflowable(
                page_ref,
                frame.page_box,
                at,
                body,
            );

            let targets = PageSceneTargets {
                element: &mut *element,
                overlays: &mut *overlays,
                column: &mut target_column,
                backdrops: &mut *backdrops,
                page_bands: &mut target_page_bands,
                out: &mut *frame_out,
            };
            let out = self.render_paper(&*book, paper, &render_frame, targets, &mut *window, &mut *cx);
            paper_layers.extend(out.layers);
            page_bands.extend(out.page_bands);
            if paint.front.is_none() {
                paint.front = out.front_capture;
            }
            if paint.back.is_none() {
                paint.back = out.back_capture;
            }
            if paint.incoming.is_none() {
                paint.incoming = out.incoming_capture;
            }
            frame_out.bands_drawn |= out.bands_drawn;
        }
        // 背面目标章还没排完版时排不出内容：给一份**空的副本**，请求仍会把纹理
        // 清成纸色 —— 背面是一张干净的纸，而不是上一趟留下的字。目标页在、只是
        // 图还没到时 **不** 走这一支（`back_missing` 让这一帧不交，下一帧重试）。
        if paint.back_key.is_some()
            && paint.back.is_none()
            && let Some((chapter, _, _)) = paint.back_key
            && book.span_doc(chapter).is_none()
        {
            paint.back = Some(PaperCapture {
                element: super::paper::empty_capture_element(),
                under: Vec::new(),
                over: Vec::new(),
                bands: None,
            });
        }
    }
}

/// 这一趟要摆哪两张纸、各自带哪张 `Sheet`，以及双页折页的逐栏取舍。
struct TurnPapers {
    papers: [Paper; 2],
    count: usize,
    leaf_turn: bool,
    leaf_column: usize,
    other_column: usize,
    turn_leaving: Option<PageRef>,
}

impl ReaderSession {
    /// 把「这一趟怎么摆」从渲染臂里分出来：只读 `RenderFrame` 与捕获状态，
    /// 写回 `paint`（网格 / 阴影 / 目标页）与 `page_shift`，其余交给逐纸绘制。
    fn plan_turn_papers(
        &mut self,
        frame: &RenderFrame<'_>,
        capture: &CaptureView,
        paint: &mut TurnPaint,
        cx: &App,
    ) -> TurnPapers {
        let current = self.chapter_index();
        let page = self.page;
        let mode = frame.mode;
        let page_area = frame.page_area();
        let area = frame.area();
        let viewport = frame.viewport();
        let page_key = frame.page_key;
        let (captured_ch, captured_pg) = self
            .captured_page
            .unwrap_or((usize::MAX, usize::MAX));
        let (captured_w, captured_h) = self.captured_size.unwrap_or((-1.0, -1.0));
        // 要摆的那几页：定住时只有当前这一页；动画在走时**多出另一张纸**
        // （同章邻页，或跨章的目标页），两张各自怎么摆由这一趟带上来的那一档
        // 决定 —— 位移与层级都来自 [`turn_animation`]。
        let rest = Sheet {
            offset: (0.0, 0.0),
            z: 0,
            cut: None,
        };
        let mut papers: [Paper; 2] = std::array::from_fn(|_| {
            Paper::empty(
                PageRef {
                    chapter: current,
                    page: self.page,
                },
                rest,
            )
        });
        let mut count = 1usize;
        let mut cross_chapter = false;
        // 双页折页这一趟的逐栏取舍要在 `if let Some(frame)` 之外读，先把这几格提出来。
        let mut leaf_turn = false;
        let mut leaf_column = 0usize;
        let mut other_column = 1usize;
        let mut turn_leaving: Option<PageRef> = None;
        if let Some(turn_frame) = frame.turn {
            let style = turn_animation::of(turn_frame.style);
            // 折线姿态与「这一屏有多宽」两条臂共走一份计划（见 `turn_view::plan_sheets`）。
            let turn_view::SheetPlan {
                curl_area,
                free_right,
                pose,
            } = turn_view::plan_sheets(&turn_frame, mode, page_area, area);
            if turn_frame.style == PageTurn::Curl {
                paint.leaf = Some(curl_area);
            }
            // 只有这张「出场页」的捕获确实完整时才开 GPU 卷曲。捕获也可能因为
            // 进场横移还没停、尺寸刚变或偏好刚重排而不可用；此时强行用半张/旧的
            // 纹理，右侧就会采到捕获矩形外。没有可用纹理时退回 CPU 裁页，宁可不
            // 卷，也不要那条黑带。
            let leaving_ref = turn_frame.leaving.unwrap_or(PageRef {
                chapter: current,
                page,
            });
            // 只有这张「出场页」的捕获确实完整时才开 GPU 卷曲；几何、顶点缓存与
            // uniform 的装配两条臂共用（见 `turn_view::build_curl_mesh`）。
            if turn_frame.style == PageTurn::Curl && capture.front_valid(leaving_ref, viewport) {
                paint.mesh = turn_view::build_curl_mesh(
                    &turn_view::CurlAsk {
                        progress: turn_frame.progress,
                        released: matches!(turn_frame.run, TurnRun::Released { .. }),
                        revert: matches!(turn_frame.run, TurnRun::Released { revert: true, .. }),
                        free_right,
                        pose,
                        curl_area,
                        content_area: area,
                        viewport,
                        page_key,
                        captured: (captured_ch, captured_pg, captured_w, captured_h),
                        capture_valid: true,
                        paper: cx.theme().background.to_rgb(),
                    },
                    &mut self.curl_mesh,
                );
            }
            // 折页那一档不再给 CPU 影子：纸面与圆柱受光都在 GPU 那张网格里
            // （见 `curl.rs`、`page_curl.wgsl`）。`shades_at` 的默认实现给回空清单，
            // 所以这一句对别的几档就是「照旧」。`pose` 是上面拼的那一份：带着**补间
            // 之后**的手与弹回的时钟，网格与摆位层读的都是它。
            paint.shades = style.shades_at(turn_frame.progress, curl_area, pose);
            let [incoming, outgoing] = style.sheets_at(turn_frame.progress, curl_area, pose);
            let leaving_ref = turn_frame.leaving.unwrap_or(PageRef {
                chapter: current,
                page,
            });
            let arriving_ref = turn_frame.arriving.unwrap_or(PageRef {
                chapter: current,
                page,
            });
            // 同一张纸既当出场又当进场时不拆栏：正常走不到（`turn()` 只按
            // `TurnPlan` 里真会换页那几支起趟），但真走到了，拆栏会把两栏都判给
            // 捕获层，主场景一片空白。
            // 这一趟是不是「双页里只揭靠手指那一张 leaf」—— 只有仿真折页是，判据在
            // [`turn_animation::paper_area`]，这里读它盖上的那一格。它管的是**哪一栏
            // 在纸上**：其余几档在双页里两栏同属那张移动的纸，谁都不许停在原地。
            leaf_turn = curl_area.spread_leaf && leaving_ref != arriving_ref;
            leaf_column = if free_right { 1 } else { 0 };
            other_column = 1 - leaf_column;
            turn_leaving = Some(leaving_ref);
            paint.arriving = Some(arriving_ref);
            // 这一趟要烘的背面（捕获层 2）：揭起来那张纸的背面 = 目标跨页的另一半
            // （双页）/ 目标页整张（单页）。网格真画出来了才有采样它的人；同一趟里
            // 目标不变、纹理跨帧留着，所以只对**没烘过**的那一格烘一趟。
            // 跨章那一趟的目标章可能还没排完版：那一栏排不出内容，背面就只是一张
            // 清成纸色的空纹理（与同一帧底下那张目标页同样空着是一致的）。
            if paint.mesh.is_some() {
                let key = (
                    arriving_ref.chapter,
                    arriving_ref.page,
                    if leaf_turn { other_column } else { 0 },
                );
                if self.captured_back != Some((key, viewport)) {
                    paint.back_key = Some(key);
                }
            }
            if arriving_ref.chapter != current {
                // 跨章：源页往外走、目标页往里走，各用各的章。
                cross_chapter = true;
                papers[0] = Paper::empty(leaving_ref, outgoing);
                papers[1] = Paper::empty(arriving_ref, incoming);
                count = 2;
            } else {
                // 同章：`slots[0]` 恒是进场那一张 —— 命中与 `page_shift` 归它。
                // 入树顺序另按 `z` 排（见下），两者不必一致：覆盖档要把这张
                // 抬到停着的旧页之上，而它始终是「这一屏的当前页」。
                papers[0] = Paper::empty(arriving_ref, incoming);
                if leaving_ref != arriving_ref {
                    papers[1] = Paper::empty(leaving_ref, outgoing);
                    count = 2;
                }
            }
            // 单页 C 方案只摆进场那一张：旧页已经在捕获纹理里，由 GPU 网格卷。
            // 双页不能只摆一张：静态页（当前跨页的另一半）与底层新页（下一跨页的
            // 同侧半页）都得留着，网格只盖住被揭起来的那一张 leaf。两张纸都不裁，
            // 由下面的逐栏跳过把 leaf 正面与背面取掉。
            if paint.mesh.is_some() && mode != chrome::PageMode::Spread {
                let arriving_slot = if papers[0].identity == arriving_ref { 0 } else { 1 };
                papers.swap(0, arriving_slot);
                count = 1;
            }
        }
        // 当前页这一帧偏了多少。命中换算读这一格（`to_layout` 里减掉它）：
        // 跨章时两张纸的章不同，命中在 [`Self::turn_in_flight`] 已经让位，
        // 这里给 0 是让旧坐标不再被减第二次。
        self.page_shift = if cross_chapter {
            (0.0, 0.0)
        } else {
            papers[0].sheet.offset
        };

        TurnPapers {
            papers,
            count,
            leaf_turn,
            leaf_column,
            other_column,
            turn_leaving,
        }
    }
}

impl ReaderSession {
    fn render_paper(
        &mut self,
        book: &Book,
        paper: Paper,
        frame: &RenderFrame<'_>,
        targets: PageSceneTargets<'_>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PaperRenderOut {
        let (scenes, continuous) = match paper.body {
            PaperBody::Flow { scenes, packed } => (scenes, packed),
            // 定版式纸由 `fixed::paint_fixed_paper` 画：它要页表（`FixedBook`）
            // 与缩放几何，不走这趟按栏拆分的字形装配。
            PaperBody::Fixed(_) => unreachable!("定版式纸由 paint_fixed_paper 画"),
        };
        let offset = paper.sheet.offset;
        if continuous {
            let PageSceneTargets {
                element,
                overlays,
                column,
                backdrops,
                page_bands,
                out: frame_out,
            } = targets;
            let mut capture = CaptureSink::new();
            let mut group_targets = PageSceneTargets {
                element: &mut *element,
                overlays: &mut *overlays,
                column: &mut *column,
                backdrops: &mut *backdrops,
                page_bands: &mut *page_bands,
                out: &mut *frame_out,
            };
            for scene in scenes {
                let Some(doc) = book.span_doc(scene.identity.chapter) else {
                    continue;
                };
                self.render_page_scene(
                    book,
                    doc,
                    scene,
                    frame.clone(),
                    group_targets.reborrow(),
                    &mut capture,
                    window,
                    cx,
                );
            }
            return PaperRenderOut {
                layers: Vec::new(),
                page_bands: Vec::new(),
                front_capture: None,
                back_capture: None,
                incoming_capture: None,
                bands_drawn: !page_bands.is_empty(),
            };
        }
        let PageSceneTargets {
            out: frame_out, ..
        } = targets;
        let mut moving = Vec::new();
        let mut still = Vec::new();
        let mut capture_only = Vec::new();
        for scene in scenes {
            if scene.role == CaptureRole::Front {
                continue;
            }
            if scene.role.draws_main() {
                if scene.role.moves_with_sheet() {
                    moving.push(scene);
                } else {
                    still.push(scene);
                }
            } else {
                capture_only.push(scene);
            }
        }
        let mut out = PaperRenderOut {
            layers: Vec::new(),
            page_bands: Vec::new(),
            front_capture: None,
            back_capture: None,
            incoming_capture: None,
            bands_drawn: false,
        };
        for (scenes, offset, front, append) in [
            (moving, offset, true, true),
            (still, (0.0, 0.0), false, true),
            (capture_only, (0.0, 0.0), false, false),
        ] {
            if scenes.is_empty() {
                continue;
            }
            let mut element = GlyphsElement::new();
            let mut column = div().size_full().relative();
            let mut overlays = Vec::new();
            let mut backdrops = Vec::new();
            let mut page_bands = Vec::new();
            // 收谁进捕获副本按场景自己的旗子判，不按角色：单页折页的进场页是
            // `Incoming` 角色，但它同时要写一份背面（目标页整张）。
            let has_back = scenes.iter().any(|scene| scene.back_copy);
            let has_incoming = scenes.iter().any(|scene| scene.incoming_copy);
            let mut capture = CaptureSink::new();
            let mut group_targets = PageSceneTargets {
                element: &mut element,
                overlays: &mut overlays,
                column: &mut column,
                backdrops: &mut backdrops,
                page_bands: &mut page_bands,
                out: &mut *frame_out,
            };
            for scene in scenes {
                let Some(doc) = book.span_doc(scene.identity.chapter) else {
                    continue;
                };
                self.render_page_scene(
                    book,
                    doc,
                    scene,
                    frame.clone(),
                    group_targets.reborrow(),
                    &mut capture,
                    window,
                    cx,
                );
            }
            let has_bands = !page_bands.is_empty();
            out.bands_drawn |= has_bands;
            // 捕获副本**任何一组**里都可能出现：进场页照常画在主场景（`append` 那组），
            // 它的副本只能在这里收。从前只在 `!append`（只进捕获、不上屏）那组收，
            // 进场页的副本就被丢掉了 —— 纹理照样请求、照样记成「已烘」，里面却只有
            // 纸色，下一帧用它替掉实时页就是滑动时的白屏。
            let CaptureSink {
                front_decor,
                front_images,
                back_element,
                back_children,
                incoming_element,
                incoming_children,
                incoming_images,
                front_missing,
                back_missing,
                incoming_missing,
            } = capture;
            if has_back && !back_missing && out.back_capture.is_none() {
                out.back_capture = Some(PaperCapture {
                    element: back_element.capture(),
                    under: back_children,
                    over: Vec::new(),
                    bands: None,
                });
            }
            if has_incoming && !incoming_missing && out.incoming_capture.is_none() {
                let capture = PaperCapture {
                    element: incoming_element.capture(),
                    under: incoming_children,
                    over: Vec::new(),
                    bands: None,
                };
                self.incoming_capture_runs += capture.element.run_count() as u32;
                self.incoming_capture_images += incoming_images;
                out.incoming_capture = Some(capture);
            }
            if !append {
                continue;
            }
            if front && frame.capture_front && !front_missing {
                let readouts = bands::readouts_of(self, cx);
                let bands = bands::chrome(
                    &frame.page_box,
                    &readouts,
                    frame_out.bare_bands,
                    bands::Width::Paper,
                    cx,
                );
                // 捕获副本的带子单独挂一个 id，测试要量它在捕获层里的位置。
                let bands = div()
                    .id("reader-front-capture-band")
                    .test_support()
                    .absolute()
                    .inset_0()
                    .child(bands)
                    .into_any_element();
                self.front_capture_images += front_images.len() as u32;
                out.front_capture = Some(PaperCapture {
                    element: element.capture_copy(),
                    under: front_decor,
                    over: front_images,
                    bands: Some(bands),
                });
            }
            let clip = if frame.turn.is_some() && paper.bands == BandPolicy::Reserved {
                paper.paper
            } else {
                paper.content
            };
            let viewport = frame.page_box.paper;
            let inner = div()
                .absolute()
                .left(px(-clip.x))
                .top(px(-clip.y))
                .w(px(viewport.w))
                .h(px(viewport.h));
            let paper_layer = div()
                .absolute()
                .left(px(offset.0))
                .top(px(offset.1))
                .size_full()
                .child(element)
                .child(column)
                .children(backdrops)
                .children(overlays);
            let clipped = div()
                .absolute()
                .left(px(clip.x))
                .top(px(clip.y))
                .w(px(clip.w))
                .h(px(clip.h))
                .overflow_hidden()
                .id("reader-paper-clip")
                .test_support()
                .child(inner.child(paper_layer));
            out.layers.push(clipped.into_any_element());
            // 带子挂在裁切层**外面**，但仍按这一张纸的位移摆：静止时 offset 是 0，
            // 翻页时跟着纸走。宽度取整张纸，带子自己的 `Width::Parent` 再按栏切。
            for band in page_bands {
                out.page_bands.push(
                    div()
                        .absolute()
                        .left(px(offset.0))
                        .top(px(offset.1))
                        .w(px(viewport.w))
                        .h(px(viewport.h))
                        .child(band)
                        .into_any_element(),
                );
            }
        }
        out
    }
}

impl ReaderSession {
    #[allow(clippy::too_many_arguments)]
    fn render_page_scene(
        &mut self,
        book: &Book,
        doc: &Document,
        page: PageScene,
        ctx: RenderFrame<'_>,
        targets: PageSceneTargets<'_>,
        capture: &mut CaptureSink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let PageScene {
            identity: page_ref,
            column: scene_column,
            origin_y,
            height,
            range,
            role,
            back_copy,
            incoming_copy,
            x,
            top,
            cut,
            picture,
            art_drawn,
            mut scene,
        } = page;
        let columns = ctx.columns();
        let paper_bands = ctx.paper_bands();
        let curl_turn = ctx.curl_turn();
        let window_height = ctx.window_height();
        let RenderFrame {
            capture_front,
            palette,
            speech_span,
            atlas,
            view,
            reader_weak,
            viewer,
            long_press,
            image_hits,
            stack_hits,
            image_hits_frame,
            image_pans,
            page_percent,
            page_box,
            turn,
            ..
        } = ctx;
        let PageSceneTargets {
            element,
            overlays,
            column,
            backdrops,
            page_bands,
            out: frame_out,
        } = targets;
        let CaptureSink {
            front_decor,
            front_images,
            back_element,
            back_children,
            incoming_element,
            incoming_children,
            incoming_images,
            front_missing,
            back_missing,
            incoming_missing,
        } = capture;
        let is_picture = picture.is_some();
        if let Some(picture) = picture {
            paint_picture_page(
                PicturePageAsk {
                    doc,
                    picture,
                    window_height,
                    role,
                    turn,
                },
                &mut scene,
                &mut *backdrops,
                &mut *frame_out,
                &atlas,
                view,
                cx,
            );
        }
        // 读数带：一页一份，跟着这一页的位移走。单页时就是一整条；
        // 双页时左右各半条 —— 左页念位置，右页念百分比，章名只留在
        // 左页，避免同一组数在跨页上印两遍。走捕获网格的出场页带子在
        // 正面捕获纹理里（`PaperCapture.bands`）；走平摊网格的进场页由那张
        // 只有带子的 `Paper` 画；Curl 的带子由 `whole_sheet_curl`
        // 那一层负责。这里只画实时页，不重复。
        if role.draws_main() && !curl_turn && paper_bands {
            page_bands.push(page_band_element(
                PageBandAsk {
                    book,
                    doc,
                    page: page_ref,
                    column: scene_column,
                    columns,
                    page_box: &page_box,
                    page_percent: &page_percent,
                    // 这一页自己有整页画时读数带透明：画铺到了状态栏
                    // 那一条，不透明的带子会在顶上盖出一块底色。
                    bare: is_picture || art_drawn,
                },
                cx,
            ));
        }
        // 背面那一份不裁：整张纸的背面都得在纹理里，卷到哪儿就采到哪儿。
        // 克隆只到 run 这一层（`shaped` 是 `Arc<LineLayout>`），不重跑 shaping。
        if back_copy {
            back_element.extend(scene.runs.iter().cloned());
        }
        // 进场页那一份同口径：烘焙那一帧整页收进层 3，之后帧由网格采。
        if incoming_copy {
            incoming_element.extend(scene.runs.iter().cloned());
        }
        if role.draws_main() {
            // 折页：这一张纸按它**自己那一条**裁剪收字 —— 进场那一张只画
            // 已经空出来的那一侧，出场那一张只画还平摊着的那一侧。裁剪与
            // 折面用的是同一条折线，所以影子收掉的那一刻字也正好收干净，
            // 不会出现「影子没了、字还压着走」的那一帧。
            if let Some(cut) = cut {
                for run in scene.runs.iter_mut() {
                    run.cut = Some(cut);
                }
            }
            element.extend(scene.runs);
        }
        paint_scene_decorations(
            &atlas,
            view,
            std::mem::take(&mut scene.decorations),
            SceneDecorFacts {
                cut,
                role,
                capture_front,
                back_copy,
                incoming_copy,
            },
            &mut *column,
            SceneDecorCapture {
                front_decor: &mut *front_decor,
                back_children: &mut *back_children,
                incoming_children: &mut *incoming_children,
                front_missing: &mut *front_missing,
                back_missing: &mut *back_missing,
                incoming_missing: &mut *incoming_missing,
            },
            cx,
        );
        let images = std::mem::take(&mut scene.images);
        paint_scene_images(
            SceneImagesCtx {
                atlas: &atlas,
                view,
                viewer: &viewer,
                long_press,
                reader_weak: &reader_weak,
                image_pans,
                image_hits: &image_hits,
                image_hits_frame,
                stack_hits: &stack_hits,
                capture_front,
            },
            SceneImagesFacts {
                page: page_ref,
                cut,
                role,
                back_copy,
                incoming_copy,
            },
            images,
            &mut PageSceneTargets {
                element: &mut *element,
                overlays: &mut *overlays,
                column: &mut *column,
                backdrops: &mut *backdrops,
                page_bands: &mut *page_bands,
                out: &mut *frame_out,
            },
            SceneImageCapture {
                front_images: &mut *front_images,
                back_children: &mut *back_children,
                incoming_children: &mut *incoming_children,
                incoming_images: &mut *incoming_images,
                front_missing: &mut *front_missing,
                back_missing: &mut *back_missing,
                incoming_missing: &mut *incoming_missing,
            },
            cx,
        );
        let spot = MarkSpot {
            doc,
            window: &*window,
            cx,
            x,
            origin_y,
            flow_len: height,
            header: top,
            band: range,
        };
        let __fills = crate::ui::features::reader::model::perf::Timer::start();
        let speech = speech_span
            .as_ref()
            .filter(|(chapter, _)| *chapter == page_ref.chapter)
            .map(|(_, range)| range.clone());
        if role.draws_main() {
            self.paint_marks(element, &spot, page_ref.chapter, palette, speech.clone(), cut);
        }
        if back_copy {
            self.paint_marks(
                back_element,
                &spot,
                page_ref.chapter,
                palette,
                speech.clone(),
                None,
            );
        }
        if incoming_copy {
            self.paint_marks(
                incoming_element,
                &spot,
                page_ref.chapter,
                palette,
                speech,
                None,
            );
        }
        let __fills = __fills.ns();
        crate::ui::features::reader::model::perf::add(|f| {
            f.fills_ns += __fills
        });
    }
}

/// 整页图这一栏要的那几格输入。
struct PicturePageAsk<'a> {
    doc: &'a Document,
    picture: Picture,
    window_height: f32,
    role: CaptureRole,
    turn: Option<TurnFrame>,
}

/// 整页图：把排版层那张流内图换成铺满这一页的纸面图，外面再压一层同图的背板
/// （专门把状态栏 / 手势条那两条也铺到）。背板只在这一页上屏时加 —— 被捕获
/// 带走的那一帧不再画它，否则纸已经卷起来了、顶上那条还留着原图。
fn paint_picture_page(
    ask: PicturePageAsk<'_>,
    scene: &mut Scene,
    backdrops: &mut Vec<AnyElement>,
    out: &mut FrameOut,
    atlas: &Entity<ImageAtlas>,
    view: EntityId,
    cx: &mut App,
) {
    let PicturePageAsk {
        doc,
        picture,
        window_height,
        role,
        turn,
    } = ask;
    let (href, page_x, page_w, art_top, contain) = picture;
    out.page_picture = true;
    out.bare_bands = true;
    scene.images.clear();
    if let Some(image) = doc.images().decode_request(&href) {
        let page_h = window_height;
        let fit = if contain {
            crate::ui::foundation::images::ObjectFit::Contain
        } else {
            crate::ui::foundation::images::ObjectFit::Cover
        };
        scene.images.push(VisibleImage {
            x: page_x,
            top: art_top,
            width: page_w,
            height: page_h,
            image,
            footnote: None,
            fit,
            // 这一张是「铺满这一页的整页图」，不是哪一叠里的一张。
            stack: None,
        });
        if role.draws_main() && turn.is_none() {
            let size = if contain {
                BackgroundSize::Contain
            } else {
                BackgroundSize::Cover
            };
            let picture =
                crate::ui::features::reader::view::scene::full_page_picture(&href, size);
            if let Some(decor) = crate::ui::features::reader::view::scene::backdrop_decor(
                &[picture],
                page_x,
                art_top,
                page_w,
                page_h,
                doc.images(),
            ) {
                backdrops.extend(decoration_child(decor, atlas, view, cx));
            }
        }
    }
}

/// 一栏的装饰这一帧归谁、按哪条折线裁。
struct SceneDecorFacts {
    cut: Option<turn_animation::Cut>,
    role: CaptureRole,
    capture_front: bool,
    back_copy: bool,
    incoming_copy: bool,
}

/// 装饰要写的那几格捕获副本。
struct SceneDecorCapture<'a> {
    front_decor: &'a mut Vec<AnyElement>,
    back_children: &'a mut Vec<AnyElement>,
    incoming_children: &'a mut Vec<AnyElement>,
    front_missing: &'a mut bool,
    back_missing: &'a mut bool,
    incoming_missing: &'a mut bool,
}

/// 块级装饰（`background-color`、`border`、`<hr>`）也跟着那张纸走：中心落在
/// 裁掉那一侧就整块不画。字是逐点裁的，而一个装饰块是一个矩形、没有「裁一半」
/// 的画法 —— 逐像素的多边形裁剪要的是 gpui 没有的东西（`ContentMask` 只有矩形）。
/// 留白块比伸到隔壁那张纸上的色块好读。捕获层里那两份副本照单全收（不裁）。
///
/// 只有这一帧真要写捕获副本时才逐块查背景图；实时页那一趟多查一遍
/// `painted_picture` 是白花钱。
fn paint_scene_decorations(
    atlas: &Entity<ImageAtlas>,
    view: EntityId,
    decorations: Vec<BlockDecor>,
    facts: SceneDecorFacts,
    column: &mut Div,
    capture: SceneDecorCapture<'_>,
    cx: &mut App,
) {
    let SceneDecorFacts {
        cut,
        role,
        capture_front,
        back_copy,
        incoming_copy,
    } = facts;
    let capturing = capture_front || back_copy || incoming_copy;
    for decor in decorations {
        // 背景图没全到就先把捕获副本标成不完整：缺一张的纹理一旦被记成
        // 「已烘」，滑动时旧页就永久少一张图。实时那一份照画（该到的会到）。
        if capturing && !decoration_ready(&decor, atlas, view, cx) {
            if capture_front {
                *capture.front_missing = true;
            }
            if back_copy {
                *capture.back_missing = true;
            }
            if incoming_copy {
                *capture.incoming_missing = true;
            }
        }
        if capture_front {
            capture
                .front_decor
                .extend(decoration_child(decor.clone(), atlas, view, cx));
        }
        if back_copy {
            capture
                .back_children
                .extend(decoration_child(decor.clone(), atlas, view, cx));
        }
        if incoming_copy {
            capture
                .incoming_children
                .extend(decoration_child(decor.clone(), atlas, view, cx));
        }
        if !role.draws_main() {
            continue;
        }
        if cut.is_some_and(|cut| {
            !cut.keeps(
                decor.x + decor.width * 0.5,
                decor.top + decor.height * 0.5,
            )
        }) {
            continue;
        }
        column.extend(decoration_child(decor, atlas, view, cx));
    }
}

/// 一屏插图要的那几格只读输入（从 `RenderFrame` 里拆出来，避免整包借用打架）。
struct SceneImagesCtx<'a> {
    atlas: &'a Entity<ImageAtlas>,
    view: EntityId,
    viewer: &'a Option<Entity<ImageViewer>>,
    long_press: bool,
    reader_weak: &'a WeakEntity<ReaderSession>,
    image_pans: &'a HashMap<String, (f32, f32)>,
    image_hits: &'a Rc<RefCell<Vec<ImageHit>>>,
    image_hits_frame: u64,
    stack_hits: &'a Rc<RefCell<Vec<StackHit>>>,
    capture_front: bool,
}

/// 一栏的图这一帧归谁、按哪条折线裁。
struct SceneImagesFacts {
    page: PageRef,
    cut: Option<turn_animation::Cut>,
    role: CaptureRole,
    back_copy: bool,
    incoming_copy: bool,
}

/// 一栏的图要写的那几格捕获副本（从 `CaptureSink` 里按字段借出来，
/// 与 `render_page_scene` 后面的装饰 / 文字副本共用同一个 sink）。
struct SceneImageCapture<'a> {
    front_images: &'a mut Vec<AnyElement>,
    back_children: &'a mut Vec<AnyElement>,
    incoming_children: &'a mut Vec<AnyElement>,
    incoming_images: &'a mut u32,
    front_missing: &'a mut bool,
    back_missing: &'a mut bool,
    incoming_missing: &'a mut bool,
}

/// 一栏里的图：实时层、三份捕获副本与叠图提示。
///
/// 「这一帧在场」的记账（`painted_keys`）与捕获副本缺图的标记必须在同一趟里做：
/// 前者撑住图集回收，后者挡住一张缺图的纹理被记成「已烘」。中心落在折线哪一侧
/// 决定实时层画不画；捕获副本那一份照收，否则折走的图会在回收里被判离场。
fn paint_scene_images(
    ctx: SceneImagesCtx<'_>,
    facts: SceneImagesFacts,
    images: Vec<VisibleImage>,
    targets: &mut PageSceneTargets<'_>,
    capture: SceneImageCapture<'_>,
    cx: &mut App,
) {
    let SceneImagesCtx {
        atlas,
        view,
        viewer,
        long_press,
        reader_weak,
        image_pans,
        image_hits,
        image_hits_frame,
        stack_hits,
        capture_front,
    } = ctx;
    let SceneImagesFacts {
        page,
        cut,
        role,
        back_copy,
        incoming_copy,
    } = facts;
    for image in images {
        // 中心落在折线哪一侧，就按那一侧决定画不画。整张一起收而不
        // 是切一半：切一半要逐像素的多边形裁剪，而 gpui 的 `ContentMask`
        // 只有矩形（gpui-pre-0.3.9 `src/window.rs:2226` 那条注释）。
        // 但 `painted` 照问 —— 那一问就是「这一帧在场」的记账，跳过它
        // 会让被折走的图在 17 帧里被判离场，露出来时再白上传一趟。
        let shown = cut.is_none_or(|cut| {
            cut.keeps(
                image.x + image.width * 0.5,
                image.top + image.height * 0.5,
            )
        });
        targets.out.painted_keys.push(image.image.key.clone());
        let Some(rendered) = painted_picture(atlas, &image.image, view, cx) else {
            // 图还没到（也没判失败）时捕获副本不完整；失败算落地 —— 实时页
            // 上它本来也不会出现。
            if !atlas.read_with(cx, |atlas, _| atlas.failed(&image.image.key)) {
                if capture_front {
                    *capture.front_missing = true;
                }
                if back_copy {
                    *capture.back_missing = true;
                }
                if incoming_copy {
                    *capture.incoming_missing = true;
                }
            }
            continue;
        };
        // 捕获层里那两份用不带手势的纯 `img`：同一屏摆两个同 id 的元素，
        // 命中区就叠了一份，「看大图」那一击会落到纹理的副本上。
        if capture_front {
            capture.front_images.push(plain_image(
                image.x,
                image.top,
                image.width,
                image.height,
                rendered.clone(),
            ));
        }
        if back_copy {
            capture.back_children.push(plain_image(
                image.x,
                image.top,
                image.width,
                image.height,
                rendered.clone(),
            ));
        }
        if incoming_copy {
            capture.incoming_children.push(plain_image(
                image.x,
                image.top,
                image.width,
                image.height,
                rendered.clone(),
            ));
            *capture.incoming_images += 1;
        }
        if !role.draws_main() || !shown {
            continue;
        }
        let index = targets.overlays.len();
        let pan = image_pans
            .get(&image.image.key)
            .copied()
            .unwrap_or((0.0, 0.0));
        // 叠图那一叠的「下面还有一张」提示与一排小点：与连续滚动
        // 那一档同一套（`stack_peek` / `stack_dots`）。分页这一臂
        // 从前只画当前那张，叠起来的几张看起来就是一整张 ——
        // 读者在分页档根本看不出还能横滑。
        let dots = image
            .stack
            .filter(|mark| mark.count > 1)
            .map(|mark| (image.x, image.top + image.height, image.width, mark));
        if let Some((x, bottom, w, mark)) = dots
            && mark.index + 1 < mark.count
        {
            targets.overlays.push(stack_peek(x, bottom, w, cx));
        }
        targets.overlays.push(image_child(
            image,
            rendered,
            ImageTap {
                viewer,
                long_press,
                index,
                reader: reader_weak,
            },
            ImageDrag {
                pan,
                hits: image_hits,
                frame_no: image_hits_frame,
                chapter: page.chapter,
                stack_hits,
            },
        ));
        if let Some((x, bottom, w, mark)) = dots {
            targets.overlays.push(stack_dots(x, bottom, w, mark, cx));
        }
    }
}

impl ReaderSession {
    fn paint_marks(
        &self,
        target: &mut GlyphsElement,
        spot: &MarkSpot<'_>,
        page: usize,
        palette: &Palette,
        speech: Option<std::ops::Range<usize>>,
        cut: Option<turn_animation::Cut>,
    ) {
        annotation_fills(
            target,
            spot,
            &self.annotation_spans,
            &self.annotations,
            palette,
            page,
            cut,
        );
        speech_fill(target, spot, speech, cut);
    }
}
