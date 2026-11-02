//! 定版式的页栈摆位、页文本取回与整臂渲染。
//!
//! 前面的辅助层只回答「这一页的每一格摆在哪儿、字层什么时候去取」；
//! [`ReaderSession::render_fixed`] 把定版式这一臂从 `body::render` 里接过来，
//! 仍然只消费会话状态与帧内输入，不另造一套页模型。

use std::time::Instant;

use folio_core::PageTurn;

use crate::ui::features::reader::components::image_viewer::ImageViewer;
use crate::ui::features::reader::components::selection::{self, SelectionVisual};
use crate::ui::features::reader::model::annotations::Palette;
use crate::ui::features::reader::model::chrome;
use crate::ui::features::reader::model::fixed_book::{FixedBook, PageTile};
use crate::ui::features::reader::model::fixed_zoom::{FixedFrame, FixedZoom, max_scale};
use crate::ui::features::reader::model::image_atlas::ImageAtlas;
use crate::ui::features::reader::model::prefetch;
use crate::ui::features::reader::model::session::{LaidOut, ReaderSession, ScrollMotion};
use crate::ui::features::reader::motion::turn_animation;
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    AnyElement, AppContext as _, Bounds, Context, Entity, EntityId, Pixels, WeakEntity, point, px,
    size,
};

use super::TurnFrame;
use super::paper::{FixedPaper, Paper, PaperBody};
use super::PREFETCH_RADIUS;
use super::images::{
    RegionAsk, painted, painted_tile, page_image, pinned_region, prepare_tile, zoom_source_width,
};
use super::marks::{HIGHLIGHT_ALPHA, fixed_marks, marks_above};

/// 定版式一页里的一格：像素活儿 + 它在这一页里的最终摆位（不含动画位移）。
pub(super) struct FixedTileAt {
    pub(super) tile: PageTile,
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) width: f32,
    pub(super) height: f32,
}

/// 连续竖滚里两页之间的缝（逻辑像素）。
///
/// **`0`**：PDF / 漫画的页要连成一条，页界由页面自己的白边给出；再垫一条 12 的空隙，
/// 一屏里就会出现两道黑带（上下各一页时尤其明显）。缝仍参与页栈的坐标 —— 调它只改
/// 页与页的距离，不改每页的高度（页高只由版心宽与页面自己的宽高比决定）。
pub(super) const FIXED_PAGE_GAP: f32 = 0.0;

/// 把一页的每一格摆成一条共享高度的横排，整体居中。返回每格的最终位置，以及
/// 「带字那一格」的矩形（页内字序那把尺记在它上面）。
///
/// 一页可能摆好几格（多图页 / 对开跨页，或渲染型的一整页）：一格时这条式子退化成
/// 「等比缩进视口」，与普通漫画的旧表现逐像素相同。尺寸未知（0）时按视口那一格的
/// 宽高比补 —— 一格等于铺满，多格共享高度则必须有个比例，视口比例是最不失真的猜。
/// 字层没到的那几页丢给后台，取回来写进会话并重画一帧。
///
/// 为什么不在绘制帧里等：PDF 一页字层要重开一次文档再抽整页字（桌面实测 1.5 s/页，
/// 手机上 3–5 s），打开一本 210 页的 PDF 时那几页串起来就是界面卡住十秒
/// （见 `model::fixed_book` 的 `text` 那一栏）。这一帧先空着字层 —— 页图照画 ——
/// 后台取回来再补，观感是「页先出来，选中与批注痕迹晚几帧」。
pub(super) fn request_page_texts(
    book: &FixedBook,
    pages: impl IntoIterator<Item = usize>,
    reader: &WeakEntity<ReaderSession>,
    cx: &mut Context<ReaderSession>,
) {
    for page in pages {
        // 缓存已有 / 已在飞 / 页号越界时 `request_page_text` 给 `None`，这一帧不重复排活儿。
        let Some(doc) = book.request_page_text(page) else {
            continue;
        };
        let fetch = {
            let doc = doc.clone();
            cx.background_spawn(async move { doc.page_text(page) })
        };
        let reader = reader.clone();
        cx.spawn(async move |_, cx| {
            let text = fetch.await;
            let _ = reader.update(cx, |session, cx| {
                // `doc` 已经不是当前这本（期间换过书）时 `store_page_text` 回 false，
                // 这一帧不必为旧书重画。
                if session.accept_page_text(&doc, page, text) {
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

pub(super) fn fixed_page_tiles(
    book: &FixedBook,
    page: usize,
    usable: f32,
    viewport_height: f32,
    header_reserve: f32,
    left: f32,
    has_text: bool,
) -> (Vec<FixedTileAt>, Option<Bounds<Pixels>>) {
    let tiles = book.page_tiles(page);
    let box_aspect = usable / viewport_height.max(1.0);
    let row_aspect = fixed_row_aspect(&tiles, box_aspect);
    if row_aspect <= 0.0 {
        return (Vec::new(), None);
    }
    let height = viewport_height.min(usable / row_aspect);
    let top = header_reserve + (viewport_height - height).max(0.0) / 2.0;
    fixed_page_tiles_at(tiles, usable, height, top, left, box_aspect, has_text)
}

/// 一条页行在 1x 里的 `(左上角, 宽高)`：一页摆几格时它们是同一高度横排的。
///
/// 空表给 `None`（那一帧一格的像素都还没解出来），缩放几何于是退回「放不大」。
pub(super) fn fixed_row_bounds(tiles: &[FixedTileAt]) -> Option<((f32, f32), (f32, f32))> {
    let first = tiles.first()?;
    let last = tiles.last()?;
    Some((
        (first.x, first.y),
        (last.x + last.width - first.x, first.height),
    ))
}

/// 一条页栈里的一页：高度已经由 [`crate::ui::features::reader::model::fixed_book::FixedStack`]
/// 按版心宽算好，这里只把每一格按同一高度横排、摆到 `top` 上。
#[allow(clippy::too_many_arguments)]
pub(super) fn fixed_page_tiles_at(
    tiles: Vec<PageTile>,
    usable: f32,
    height: f32,
    top: f32,
    left: f32,
    fallback_aspect: f32,
    has_text: bool,
) -> (Vec<FixedTileAt>, Option<Bounds<Pixels>>) {
    let row_aspect = fixed_row_aspect(&tiles, fallback_aspect);
    if row_aspect <= 0.0 {
        return (Vec::new(), None);
    }
    // 整行在**版心**里居中：版心左缘是 `left`（页边距），不是窗口的 0。页栈那一档
    // `row_aspect * height` 正好等于版心宽，这里退化成从 `left` 开始 —— 右边因此与
    // 左边留下同一个页边距，而不是把整条行贴在窗口左缘、把两倍页边距全丢在右边。
    let mut x = left + (usable - row_aspect * height) / 2.0;
    let mut placed = None;
    let mut out = Vec::with_capacity(tiles.len());
    for tile in tiles {
        let width = fixed_tile_aspect(&tile, fallback_aspect) * height;
        if has_text && tile.renders && tile.request.is_some() {
            placed = Some(Bounds::new(
                point(px(x), px(top)),
                size(px(width), px(height)),
            ));
        }
        out.push(FixedTileAt {
            tile,
            x,
            y: top,
            width,
            height,
        });
        x += width;
    }
    (out, placed)
}

fn fixed_tile_aspect(tile: &PageTile, fallback: f32) -> f32 {
    if tile.width == 0 || tile.height == 0 {
        fallback
    } else {
        tile.width as f32 / tile.height as f32
    }
}

fn fixed_row_aspect(tiles: &[PageTile], fallback: f32) -> f32 {
    tiles
        .iter()
        .map(|tile| fixed_tile_aspect(tile, fallback))
        .sum()
}

/// 定版式这一臂的帧内输入。
///
/// `book` 由调用处从 `Source::Fixed` 借出后单独传入；其余是 `body::render` 在进
/// match 之前采好的只读快照。`overlays` 与 `fixed_visual` 是这一臂交回公共装配的两份产物。
pub(super) struct FixedArm<'a> {
    pub(super) mode: chrome::PageMode,
    pub(super) usable: f32,
    pub(super) viewport_height: f32,
    pub(super) content_top: f32,
    pub(super) viewport_width: f32,
    pub(super) area: turn_animation::Area,
    pub(super) turn: Option<TurnFrame>,
    pub(super) motion: ScrollMotion,
    pub(super) now: Instant,
    pub(super) view: EntityId,
    pub(super) viewer: Option<Entity<ImageViewer>>,
    pub(super) palette: &'a Palette,
    pub(super) long_press: bool,
    pub(super) atlas: Entity<ImageAtlas>,
    pub(super) reader_weak: WeakEntity<ReaderSession>,
}

/// 定版式一张纸这一帧的公共画法输入。
///
/// 连续竖滚与整页翻只在位移、轴心与倍率上不同；「每格铺哪张图、长按归谁、
/// 痕迹压在页图之上」两档共用 [`ReaderSession::paint_fixed_paper`]。
pub(super) struct FixedPaint<'a> {
    pub(super) book: &'a FixedBook,
    pub(super) atlas: &'a Entity<ImageAtlas>,
    pub(super) view: EntityId,
    pub(super) reader: &'a WeakEntity<ReaderSession>,
    pub(super) viewer: &'a Option<Entity<ImageViewer>>,
    pub(super) palette: &'a Palette,
    pub(super) long_press: bool,
    pub(super) scrolling: bool,
    pub(super) now: Instant,
    /// `(left, top, width, height)`：放大重画块按这块视口决定看不看得见。
    pub(super) viewport: (f32, f32, f32, f32),
    /// 这一帧纸面的动画位移（`Sheet::offset`）。
    pub(super) shift: (f32, f32),
    pub(super) zoom: FixedZoom,
    pub(super) pivot: (f32, f32),
}

impl ReaderSession {
    pub(super) fn render_fixed(
        &mut self,
        book: &mut FixedBook,
        arm: FixedArm<'_>,
        overlays: &mut Vec<AnyElement>,
        fixed_visual: &mut Option<SelectionVisual>,
        cx: &mut Context<Self>,
    ) {
        let FixedArm {
            mode,
            usable,
            viewport_height,
            content_top,
            viewport_width,
            area,
            turn,
            motion,
            now,
            view,
            viewer,
            palette,
            long_press,
            atlas,
            reader_weak,
        } = arm;
        // 定版式：页是既定的，没有排版链。两档共用同一张页表：
        //
        // - 连续竖滚：页按版心宽铺开、纵向排成一条栈，位置是栈上的像素；
        // - 整页翻：一屏一页，位置是页号。
        //
        // 连续那一档的做法与可重排那一族的「拼接链」同源：`scroll_y` 是权威坐标，
        // 页号只是「视口顶落在哪一页」的读数。
        let pages = book.page_count();
        let fallback_aspect = usable / viewport_height.max(1.0);
        if mode.is_continuous() {
            // 宽度变了要重建页栈。重建之前先把「读者眼前是第几页、页内几成」
            // 记下来，建完按新页高搬回同一处 —— 否则转屏 / 分屏会让屏上跳一页。
            // 挂着一份恢复请求时不算：那一份才是目标，旧坐标属于上一本书 / 上一档。
            let anchor = self
                .pending_restore
                .is_none()
                .then(|| {
                    book.stack()
                        .and_then(|s| s.page_and_fraction(self.scroll_y))
                })
                .flatten();
            if book.ensure_stack(usable, FIXED_PAGE_GAP, fallback_aspect)
                && let Some((page, fraction)) = anchor
                && let Some(fresh) = book.stack()
            {
                let top = fresh.top_of(page).unwrap_or(0.0);
                let height = fresh.height_of(page).unwrap_or(0.0);
                self.scroll_y = top + fraction * height;
            }
            let total = book.stack().map_or(0.0, |stack| stack.total());
            self.laid_out = LaidOut {
                mode,
                columns: 1,
                pages,
                viewport: viewport_height,
            };
            self.content_height = total;
            // 恢复落点：定版式的 `locator.chapter` 就是页号，落在那一页的页首。
            if let Some(locator) = self.pending_restore.take() {
                book.set_page(locator.chapter as usize);
                self.scroll_y = book.stack_top_of(book.page()).unwrap_or(0.0);
            }
            // 这一帧的缩放几何先写下来（捏合与拖动发生在事件里，几何只有这里量得出来），
            // 再把倍率与平移收进这一页允许的范围：转屏 / 分屏之后上一帧的倍率可能
            // 已经越过新的上限。
            //
            // 页栈是按**版心宽**铺的，所以上下滚动这一档的轴心是版心左上角，
            // 每行在 1x 里就是 `margin_x .. margin_x + usable`。
            let current = book.page();
            let page_height = book
                .stack()
                .and_then(|stack| stack.height_of(current))
                .unwrap_or(0.0);
            let source_width = zoom_source_width(book.page_tiles(current).iter());
            self.fixed_frame = FixedFrame {
                origin: (0.0, content_top),
                pivot: (0.0, content_top),
                page: (usable, page_height),
                area: (viewport_width, viewport_height),
                row: (self.prefs.margin_x, usable),
                total,
                max: max_scale(source_width, usable),
                continuous: true,
            };
            // 按本帧的几何收一次倍率与平移（这一步与整页翻那一臂同一段，而这里
            // `book` 还借着 `source`，调不了要整块 `&mut self` 的方法，所以就地做）。
            {
                let frame = self.fixed_frame;
                self.fixed_zoom
                    .set_scale(self.fixed_zoom.scale(), frame.max);
                let pan = self.fixed_zoom.pan();
                let scale = self.fixed_zoom.scale();
                self.fixed_zoom.set_pan(frame.clamp_pan(pan, scale));
            }
            // 视口在 1x 内容坐标里只占 `viewport / scale`：页是按 1x 铺的，
            // 放大之后一屏压着的字变少，能滚到的底也跟着变。
            let band = viewport_height / self.fixed_zoom.scale().max(1.0);
            let max_scroll = self.fixed_frame.max_scroll(self.fixed_zoom.scale());
            self.scroll_y = self.scroll_y.clamp(0.0, max_scroll);
            // 画出来的位置：点屏 / 方向键那一趟在补间，手指与滚轮那一趟直接跟
            // `scroll_y`。命中与绘制都读这一个数（与可重排连续那一臂同一条规矩）。
            let painted_y = match turn {
                Some(frame) => frame.scroll.map_or(self.scroll_y, |(from, to)| {
                    chrome::tween(from, to, frame.progress)
                }),
                None => self.scroll_y,
            };
            self.scroll_painted = painted_y.clamp(0.0, max_scroll);
            // 页号由**输入**那一头改写（滚轮 / 落页 / 恢复），渲染帧只把它抄回来：
            // 在这里再按 `scroll_y` 推一次，会把「目录跳到最后一页」那个被夹到
            // `max_scroll` 的落点读回成上一页 —— 最后一页常常比一屏矮，页首永远
            // 到不了视口顶。
            self.page = book.page();
            // 这一帧要画的页：按页栈切出可见区间，先抄成一个小表再画 ——
            // `stack()` 借着 `book`，抄完就放开，下面还要问字层与图集。
            let rows: Vec<(usize, f32, f32)> = book
                .stack()
                .map(|stack| {
                    stack
                        .visible(self.scroll_painted, band)
                        .filter_map(|page| {
                            Some((page, stack.top_of(page)?, stack.height_of(page)?))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let mut boxes: Vec<(usize, Bounds<Pixels>)> = Vec::new();
            let mut visible_pages: Vec<usize> = Vec::with_capacity(rows.len());
            // 字层还没取到的页：这一帧先不画，循环走完统一交给后台
            // （见 `request_page_texts` 与 `FixedBook::page_text_cached`）。
            let mut pending_text: Vec<usize> = Vec::new();
            for (page, top, height) in rows {
                visible_pages.push(page);
                let page_tiles = book.page_tiles(page);
                let has_text = page_tiles.iter().any(|tile| tile.renders);
                // 页栈已经把这一页的高度算好了（按版心宽与宽高比），这里只把
                // 每一格按同一高度横排；顶边是栈坐标减掉画出来的滚动量。
                let (tiles, placed) = fixed_page_tiles_at(
                    page_tiles,
                    usable,
                    height,
                    content_top + top - self.scroll_painted,
                    self.prefs.margin_x,
                    fallback_aspect,
                    has_text,
                );
                // 只查缓存：PDF 一页字层桌面要 1.5 s，在绘制帧里同步取就是
                // 打开书卡十秒（见 `FixedBook::page_text_cached` 那一段）。
                // 没到的页记下来，循环走完丢给后台，取回来重画。
                let (text, text_key) = if has_text {
                    match book.page_text_cached(page) {
                        Some((text, hash)) => {
                            let key = text.as_ref().map(|_| hash);
                            (text, key)
                        }
                        None => {
                            pending_text.push(page);
                            (None, None)
                        }
                    }
                } else {
                    (None, None)
                };
                let paper = Paper::fixed(
                    FixedPaper {
                        page,
                        tiles,
                        text,
                        text_key,
                        placed,
                    },
                    turn_animation::Sheet {
                        offset: (0.0, 0.0),
                        z: 0,
                        cut: None,
                    },
                    Some(self.fixed_zoom),
                    (viewport_width, viewport_height),
                );
                // 页格仍然按 1x 算，再映射到屏上：轴心是版心左上角，所以放大看
                // 起来是「从版心顶长出来」，纵向那一段由 `scroll_painted` 先减掉。
                self.paint_fixed_paper(
                    &paper,
                    &FixedPaint {
                        book,
                        atlas: &atlas,
                        view,
                        reader: &reader_weak,
                        viewer: &viewer,
                        palette,
                        long_press,
                        scrolling: motion.scrolling,
                        now,
                        viewport: (0.0, content_top, viewport_width, viewport_height),
                        shift: (0.0, 0.0),
                        zoom: self.fixed_zoom,
                        pivot: (0.0, content_top),
                    },
                    overlays,
                    &mut boxes,
                    fixed_visual,
                    cx,
                );
            }
            *self.fixed_text_boxes.borrow_mut() = boxes;
            // 字层没到的那几页：这一帧先空着（页图照画），后台取回来再重画。
            request_page_texts(book, pending_text, &reader_weak, cx);
            // 下一页先解好（PDF 要栅格化、CBZ 要读字节），取**可见页的后一页**：
            // 连续竖滚一屏可能压着两页，从视口顶那一页往后数会把它漏掉。
            // 预取：往读者滑的那边取两页（停下那一帧仍是「后一页优先」，
            // 由 `fixed_target` 决定）。滑动中只排预览 —— 见 `prepare_tile`。
            if turn.is_none()
                && let Some(last) = visible_pages.last().copied()
            {
                let targets = match motion.direction {
                    Some(direction) => {
                        prefetch::fixed_targets(last, pages, direction, PREFETCH_RADIUS)
                    }
                    None => prefetch::fixed_target(last, pages).into_iter().collect(),
                };
                for target in targets {
                    for tile in book.page_tiles(target) {
                        prepare_tile(&atlas, &tile, view, cx);
                    }
                }
            }
            // 选中件只摆一份（它铺满整块版心，监听也只该挂一份）；但「能不能从
            // 这一页起手选」由上面那张多页表回答。没有带字的页就整层不摆。
            if !self.fixed_text_boxes.borrow().is_empty() {
                overlays.push(selection::overlay(
                    fixed_visual.clone().unwrap_or_default(),
                    &reader_weak,
                ));
            }
            // 选区挂的页滚出可见区间就是「翻走了」：与分页那一族同一条规矩。
            // 这一问放在所有 `book` 借用之后，才能调 `&mut self` 的方法。
            if self
                .selection
                .as_ref()
                .is_some_and(|selected| !visible_pages.contains(&selected.chapter))
            {
                self.clear_selection();
            }
        } else {
            self.laid_out = LaidOut {
                mode: chrome::PageMode::Paged,
                columns: 1,
                pages,
                viewport: viewport_height,
            };
            // 恢复落点：定版式的 `locator.chapter` 就是页号。
            if let Some(locator) = self.pending_restore.take() {
                book.set_page(locator.chapter as usize);
            }
            self.page = book.page();
            self.content_height = 0.0;
            let current = book.page();
            // 定版式的下一页先解好：PDF 的一页要回引擎栅格化、CBZ 的一页要读字节 +
            // 解码，从前都要等读者翻过去那一下才开始。静止帧里每帧 prepare 一次，
            // 图集按「这一帧碰过」留住那一格；翻页动画期间不抢这笔活儿。
            if turn.is_none() {
                let targets = match motion.direction {
                    Some(direction) => {
                        prefetch::fixed_targets(current, pages, direction, PREFETCH_RADIUS)
                    }
                    None => prefetch::fixed_target(current, pages).into_iter().collect(),
                };
                for target in targets {
                    for tile in book.page_tiles(target) {
                        prepare_tile(&atlas, &tile, view, cx);
                    }
                }
            }
            // 这一页有没有字：PDF / AZW4 / DjVu 那一族才有（漫画的一页就是一张图，
            // EPUB 的定版式字在 XHTML 里而不在页上）。字层只查缓存，没到就先不画、
            // 交给后台（理由与竖滚那一臂同一条：绘制帧里同步取一页就是 1.5 s）。
            let has_text = book.page_tiles(current).iter().any(|tile| tile.renders);
            let cached_text = if has_text {
                book.page_text_cached(current)
            } else {
                None
            };
            let text_pending = has_text && cached_text.is_none();
            let (text, text_key) = match cached_text {
                Some((text, hash)) => {
                    let key = text.as_ref().map(|_| hash);
                    (text, key)
                }
                None => (None, None),
            };
            if text_pending {
                request_page_texts(book, [current], &reader_weak, cx);
            }
            // 定版式的翻页动画与流式共用同一套纸面位移；折页在 `begin_turn` 里
            // 已经换成平移，这里再收一次口，配置从别处带进来的怪值也不会去卷一张位图。
            let mut incoming_shift = (0.0, 0.0);
            let mut outgoing_page = None;
            if let Some(frame) = turn {
                let style = chrome::fixed_turn_style(frame.style);
                if style != PageTurn::None {
                    let pose = turn_animation::Pose {
                        forward: frame.forward,
                        rtl: frame.rtl,
                        grip: None,
                        revert: None,
                    };
                    let [incoming, outgoing] =
                        turn_animation::of(style).sheets_at(frame.progress, area, pose);
                    incoming_shift = incoming.offset;
                    if let Some(leaving) = frame.leaving {
                        outgoing_page = Some((leaving.page, outgoing.offset));
                    }
                }
            }
            // 出场页先入树、进场页后入树：覆盖档靠后画的那张压住停着的旧页，
            // 平移 / 上下平移两张不重叠，顺序无所谓。出场那一页只画像素，不挂
            // 选中 / 痕迹 / 放大入口 —— 它正在离开，那三样都属于目标页。
            if let Some((page, shift)) = outgoing_page {
                let (tiles, _) = fixed_page_tiles(
                    book,
                    page,
                    usable,
                    viewport_height,
                    content_top,
                    self.prefs.margin_x,
                    false,
                );
                let outgoing_paper = Paper::fixed(
                    FixedPaper {
                        page,
                        tiles,
                        text: None,
                        text_key: None,
                        placed: None,
                    },
                    turn_animation::Sheet {
                        offset: shift,
                        z: 0,
                        cut: None,
                    },
                    None,
                    (viewport_width, viewport_height),
                );
                let no_viewer: Option<Entity<ImageViewer>> = None;
                self.paint_fixed_paper(
                    &outgoing_paper,
                    &FixedPaint {
                        book,
                        atlas: &atlas,
                        view,
                        reader: &reader_weak,
                        viewer: &no_viewer,
                        palette,
                        long_press,
                        scrolling: motion.scrolling,
                        now,
                        viewport: (0.0, content_top, viewport_width, viewport_height),
                        shift,
                        zoom: FixedZoom::default(),
                        pivot: (0.0, content_top),
                    },
                    overlays,
                    &mut Vec::new(),
                    fixed_visual,
                    cx,
                );
            }
            // 一页可能摆好几格（多图页 / 对开跨页，或渲染型的一整页）：按**共享高度**
            // 横成一行，整体居中。一格时这条式子退化成「等比缩进视口」，与普通漫画的
            // 旧表现逐像素相同。
            // 命中那把尺只在**字层真的取到**时记：字层还没回来与这一页没字
            // 在屏上是两件事 —— 前者等后台到货后重画这一帧再记，后者永远不记
            // （没有字就选不出东西，记了只会让长按落到一块空盒上）。
            let (tiles, placed) = fixed_page_tiles(
                book,
                current,
                usable,
                viewport_height,
                content_top,
                self.prefs.margin_x,
                text.is_some(),
            );
            // 这一帧的缩放几何（轴心 = 铺满那一页的左上角），再按上限收一次。
            // 翻页动画在飞的时候倍率已经在 `turn()` 里清回 1x，所以下面两张纸
            // 都按铺满画；动画之外才真按手指给的倍率映射。
            let fit = fixed_row_bounds(&tiles);
            let source_width = zoom_source_width(tiles.iter().map(|at| &at.tile));
            self.fixed_frame = FixedFrame {
                origin: (0.0, content_top),
                pivot: fit.map_or((0.0, content_top), |(origin, _)| origin),
                page: fit.map_or((usable, viewport_height), |(_, size)| size),
                area: (viewport_width, viewport_height),
                row: (self.prefs.margin_x, usable),
                total: 0.0,
                max: fit.map_or(1.0, |(_, size)| max_scale(source_width, size.0)),
                continuous: false,
            };
            // 与连续那一臂同一步：本帧几何收一次，再拿收过的倍率去映射。
            {
                let frame = self.fixed_frame;
                self.fixed_zoom
                    .set_scale(self.fixed_zoom.scale(), frame.max);
                let pan = self.fixed_zoom.pan();
                let scale = self.fixed_zoom.scale();
                self.fixed_zoom.set_pan(frame.clamp_pan(pan, scale));
            }
            // 纸在几何收口之后才装配：`paper.zoom` 就是这一帧真正上屏的那一份。
            let current_paper = Paper::fixed(
                FixedPaper {
                    page: current,
                    tiles,
                    text,
                    text_key,
                    placed,
                },
                turn_animation::Sheet {
                    offset: incoming_shift,
                    z: 1,
                    cut: None,
                },
                Some(self.fixed_zoom),
                (viewport_width, viewport_height),
            );
            // 一屏就是一页：选区挂在别的页上就是「翻走了」，按分页那一族同一条规矩
            // 收掉（那里的选区也必须落在锚章）。不收会留着一份屏上看不见的选中 ——
            // 菜单条还开着，而它锚的那一段字已经不在这一帧里了。
            if self
                .selection
                .as_ref()
                .is_some_and(|selected| selected.chapter != current)
            {
                self.clear_selection();
            }
            // 命中换算读的那一把尺，与纸里摆出去的那一格同源。每一帧都写：
            // 没有字的那一页必须把它清干净，否则留着上一页的矩形，长按会按新页的
            // 字序去解旧页的盒子 —— 症状是色块整片画在别处。
            let mut boxes = Vec::new();
            let drew_text = self.paint_fixed_paper(
                &current_paper,
                &FixedPaint {
                    book,
                    atlas: &atlas,
                    view,
                    reader: &reader_weak,
                    viewer: &viewer,
                    palette,
                    long_press,
                    scrolling: motion.scrolling,
                    now,
                    viewport: (0.0, content_top, viewport_width, viewport_height),
                    shift: incoming_shift,
                    zoom: self.fixed_zoom,
                    pivot: self.fixed_frame.pivot,
                },
                overlays,
                &mut boxes,
                fixed_visual,
                cx,
            );
            *self.fixed_text_boxes.borrow_mut() = boxes;
            // 选中的那一块：选区挂在它自己那一页上，翻到别页就不再画。
            if drew_text {
                overlays.push(selection::overlay(
                    fixed_visual.clone().unwrap_or_default(),
                    &reader_weak,
                ));
            }
        }
    }

    /// 画一张定版式纸：页格、放大重画块、痕迹与选中块。
    ///
    /// 连续竖滚与整页翻只在 `paint` 的位移 / 轴心 / 倍率上不同；页格怎么摆、
    /// 「看大图」入口归谁、痕迹压在页图之上，两档只有这一条路。
    ///
    /// 返回这一页有没有真的取出字层（调用方据此决定摆不摆选中件）。
    fn paint_fixed_paper(
        &mut self,
        paper: &Paper,
        paint: &FixedPaint<'_>,
        overlays: &mut Vec<AnyElement>,
        boxes: &mut Vec<(usize, Bounds<Pixels>)>,
        fixed_visual: &mut Option<SelectionVisual>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Paper {
            body: PaperBody::Fixed(fixed),
            zoom,
            ..
        } = paper
        else {
            unreachable!("定版式纸")
        };
        let zoom = zoom.unwrap_or(paint.zoom);
        let page = fixed.page;
        let no_entry: Option<Entity<ImageViewer>> = None;
        for at in &fixed.tiles {
            // 解不出像素的那一格空着，但仍然占位：多图页的排版不该因为
            // 少一张图就把剩下的往左挤。
            let Some(request) = at.tile.request.as_ref() else {
                continue;
            };
            let key = request.key.clone();
            let Some(image) =
                painted_tile(paint.atlas, &at.tile, paint.view, cx, paint.scrolling)
            else {
                continue;
            };
            let rect = zoom.map(
                Bounds::new(
                    point(px(at.x + paint.shift.0), px(at.y + paint.shift.1)),
                    size(px(at.width), px(at.height)),
                ),
                paint.pivot,
            );
            let selected = fixed.text.is_some() && at.tile.renders;
            // 「看大图」那一格在这一族里按端分岔（`page_image` 顶上写着）：
            // 手机上它吃的是**长按**，而那正是选中要的手势 —— 于是有字的
            // 那一页不再挂它（入口挪进浮层那颗按钮），没字的页照旧。
            // 桌面那一端挂的是**单击**，与拖选不打架（拖过阈值那一下的合成
            // 点击由 `open_on_tap` 里那一条吃掉），所以原样留着。
            let entry = if selected && paint.long_press {
                &no_entry
            } else {
                paint.viewer
            };
            let index = overlays.len();
            overlays.push(page_image(
                f32::from(rect.origin.x),
                f32::from(rect.origin.y),
                f32::from(rect.size.width),
                f32::from(rect.size.height),
                image,
                key,
                entry,
                paint.long_press,
                index,
                paint.reader,
            ));
            // 放大停稳那一档：看得见的那一块按 `REGION_EDGE` 重画，叠在这一页那一张上面。
            let block = pinned_region(
                RegionAsk {
                    book: paint.book,
                    atlas: paint.atlas,
                    page,
                    tile: &at.tile,
                    rect: (at.x, at.y, at.width, at.height),
                    pivot: paint.pivot,
                    viewport: paint.viewport,
                    zoom,
                    now: paint.now,
                },
                &self.region_moved_at,
                &self.region_pinned,
                paint.view,
                cx,
            );
            if let Some(pin) = block {
                let key = pin.request.key.clone();
                if let Some(image) = painted(paint.atlas, pin.request, paint.view, cx) {
                    let zoomed = zoom.map(
                        Bounds::new(
                            point(px(pin.rect.0), px(pin.rect.1)),
                            size(px(pin.rect.2), px(pin.rect.3)),
                        ),
                        paint.pivot,
                    );
                    overlays.push(page_image(
                        f32::from(zoomed.origin.x),
                        f32::from(zoomed.origin.y),
                        f32::from(zoomed.size.width),
                        f32::from(zoomed.size.height),
                        image,
                        key,
                        &no_entry,
                        paint.long_press,
                        overlays.len(),
                        paint.reader,
                    ));
                }
            }
        }
        let Some((text, at)) = fixed.text.as_deref().zip(fixed.placed) else {
            return false;
        };
        // 带字的那一格才有选中：它的矩形就是「屏幕点 ↔ 页内字序」那一把尺
        // 唯一该记的地方。`placed` 是**最终位置**（不含动画位移）：动画期间
        // 手势让位，落定后位移正好归零，命中永远读这一份。
        boxes.push((page, zoom.map(at, paint.pivot)));
        // 留在书上的那几条痕迹，与这一帧挑中的那一块。这两层在定版式上必须
        // **盖在页图之上**：这一族的字就在那张栅格出来的图里，压在它下面等于没画
        // （可重排那一族正相反 —— 高亮压在字形之下才不吃掉笔画）。
        //
        // 动画期间痕迹跟着**进场页**一起走：页图在滑，痕迹停在终点就会读成
        // 「色块浮在纸外面」。`boxes` 仍记最终位置，手势在动画期间让位，
        // 落定后位移归零，两边自然重合。
        let drawn = zoom.map(
            Bounds::new(
                point(
                    at.origin.x + px(paint.shift.0),
                    at.origin.y + px(paint.shift.1),
                ),
                at.size,
            ),
            paint.pivot,
        );
        let marks = fixed_marks(
            &self.annotation_spans,
            &self.annotations,
            page,
            fixed.text_key.unwrap_or(0),
            text,
            drawn,
            paint.palette,
        );
        if !marks.is_empty() {
            overlays.push(marks_above(marks));
        }
        if let Some(selected) = self
            .selection
            .as_ref()
            .filter(|selected| selected.chapter == page)
        {
            *fixed_visual = Some(selection::page_visual(
                text,
                selected.range.clone(),
                drawn,
                cx.theme().selection,
                cx.theme().selection.opacity(HIGHLIGHT_ALPHA),
            ));
        }
        true
    }
}
