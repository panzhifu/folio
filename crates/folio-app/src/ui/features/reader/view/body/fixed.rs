//! 定版式的页栈摆位、页文本取回与整臂渲染。
//!
//! 前面的辅助层只回答「这一页的每一格摆在哪儿、字层什么时候去取」；
//! [`ReaderSession::render_fixed`] 把定版式这一臂从 `body::render` 里接过来，
//! 仍然只消费会话状态与帧内输入，不另造一套页模型。

use std::time::Instant;

use folio_core::PageTurn;
use folio_core::layout::fragment::{column_offset, column_width};

use crate::ui::features::reader::components::image_viewer::ImageViewer;
use crate::ui::features::reader::components::selection::{self, SelectionVisual};
use crate::ui::features::reader::model::annotations::Palette;
use crate::ui::features::reader::model::chrome;
use crate::ui::features::reader::model::fixed_book::{FixedBook, PageTile};
use crate::ui::features::reader::model::page_geometry::PageBox;
use crate::ui::features::reader::view::bands;
use crate::ui::features::reader::model::fixed_zoom::{FixedFrame, FixedZoom, max_scale};
use crate::ui::features::reader::model::image_atlas::ImageAtlas;
use crate::ui::features::reader::model::prefetch;
use crate::ui::features::reader::model::session::{
    LaidOut, PageRef, ReaderSession, ScrollMotion, TurnRun,
};
use crate::ui::features::reader::motion::turn_animation;
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, EntityId, ElementId,
    InteractiveElement as _, IntoElement, PageCurlMesh, ParentElement, Pixels, Styled,
    TestSupportExt as _, WeakEntity, div, point, px, size,
};

use super::PREFETCH_RADIUS;
use super::capture::CaptureView;
use super::images::{
    ImageSpot, ImageTap, RegionAsk, page_image, painted, painted_tile, pinned_region,
    prepare_tile, zoom_source_width,
};
use super::marks::{HIGHLIGHT_ALPHA, fixed_marks, marks_above};
use super::paper::{FixedPaper, Paper, PaperBody, PaperCapture};
use super::turn_view;

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

/// 这一页的字层：缓存里有就给，没有就记一笔待取。
///
/// 只查缓存 —— PDF 一页字层桌面要 1.5 s，在绘制帧里同步取就是打开书卡十秒
/// （见 [`FixedBook::page_text_cached`]）；没到的页由调用方循环走完统一交给后台。
fn page_text_or_pending(
    book: &FixedBook,
    page: usize,
    pending: &mut Vec<usize>,
) -> (
    Option<std::rc::Rc<folio_core::content::format::PageText>>,
    Option<u64>,
) {
    match book.page_text_cached(page) {
        Some((text, hash)) => {
            let key = text.as_ref().map(|_| hash);
            (text, key)
        }
        None => {
            pending.push(page);
            (None, None)
        }
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

/// 这一页能不能烘一张捕获纹理：每一格该画的像素都已经在图集里落地。
///
/// 判据落在**格**上而不是「页号对上了」：一页可能摆好几格，只烘到一半时那张纹理里
/// 有一半是纸色；而捕获纹理一旦记成「已烘」就不会自己重来，读者卷起来的是一张缺角
/// 的纸。`failed` 也算落地 —— 那一格本来就画不出来，纹理里那一块空着才是屏上的实情。
/// 还在解（`Loading`）与从没碰过（`None`）都不算：前者下一帧会到，后者这一帧才刚排上。
pub(super) fn fixed_capture_ready(
    book: &FixedBook,
    page: usize,
    atlas: &Entity<ImageAtlas>,
    cx: &App,
) -> bool {
    let tiles = book.page_tiles(page);
    !tiles.is_empty()
        && tiles.iter().all(|tile| match tile.request.as_ref() {
            Some(request) => atlas.read_with(cx, |atlas, _| {
                atlas.ready(&request.key) || atlas.failed(&request.key)
            }),
            // 这一格没有解码请求（页表读不出这一格）：绘制侧本来就跳过它。
            None => true,
        })
}

/// 整页翻 / 双页这一屏要摆哪几页、每页占哪一段。
///
/// 双页就是**相邻两页各占半张**（合订本摊开）：页仍是既定的位图，不重排、不分栏。
/// `pages` 第二格在只剩一页时是越界页号，`count` 才是真摆几页。
pub(super) struct FixedSlots {
    pub(super) pages: [usize; 2],
    /// 每页那一格在版心里的左缘（已含页边距与中缝）。
    pub(super) lefts: [f32; 2],
    /// 每页可用的宽：单页是整块版心，双页是半块。
    pub(super) width: f32,
    pub(super) count: usize,
    pub(super) spread: bool,
}

pub(super) fn fixed_slots(
    page: usize,
    page_count: usize,
    spread: bool,
    usable: f32,
    gap: f32,
    margin_x: f32,
) -> FixedSlots {
    if spread {
        let width = column_width(usable, 2, gap);
        let first = page & !1;
        let second = first + 1;
        let left_at = |index: usize| margin_x + column_offset(width, gap, index);
        let count = if first >= page_count {
            0
        } else if second < page_count {
            2
        } else {
            1
        };
        FixedSlots {
            pages: [first, second],
            lefts: [left_at(0), left_at(1)],
            width,
            count,
            spread: true,
        }
    } else {
        FixedSlots {
            pages: [page, page],
            lefts: [margin_x, margin_x],
            width: usable,
            count: 1,
            spread: false,
        }
    }
}

/// 这一屏的每页读数带（页眉 / 页脚）。定版式的页图是一整张，带子只是浮在上面的两行字。
///
/// `shift` 是这张纸这一帧的位移：静止帧是 0，翻页时跟着 `Sheet::offset` 走。单页给整条；
/// 双页左右各半条，与可重排分页同一套切法（左页念位置、右页念百分比）。
fn fixed_page_bands(
    page_box: &PageBox,
    slots: &FixedSlots,
    shift: (f32, f32),
    viewport_height: f32,
    id_prefix: &'static str,
    reader: &ReaderSession,
    cx: &App,
) -> Vec<AnyElement> {
    let mut out = Vec::with_capacity(slots.count);
    let paper_width = page_box.paper.w;
    let half = paper_width * 0.5;
    for index in 0..slots.count {
        let page = slots.pages[index];
        let mut readouts = bands::readouts_at(reader, 0, page, cx);
        let (left, width) = if slots.spread {
            if index == 0 {
                readouts.percent = Default::default();
            } else {
                readouts.chapter = Default::default();
                readouts.position = Default::default();
            }
            (half * index as f32, half)
        } else {
            (0.0, paper_width)
        };
        out.push(
            div()
                .absolute()
                .left(px(left + shift.0))
                .top(px(shift.1))
                .w(px(width))
                .h(px(viewport_height))
                .id(ElementId::from((id_prefix, page as u64)))
                .test_support()
                .child(bands::chrome(
                    page_box,
                    &readouts,
                    true,
                    bands::Width::Parent,
                    cx,
                ))
                .into_any_element(),
        );
    }
    out
}

/// 定版式这一臂交回公共装配的那几格。
///
/// 与 [`super::PagedArm`] 同一条形状：捕获副本、卷曲网格、折页 leaf 与读数都从这里
/// 交回去，`body::render` 后半段那一套装配（捕获层 / 网格 / 带子）两条臂共用。
/// 定版式这一臂的帧内输入。
///
/// 只留三格本臂独有的一次性输入；其余全是 [`super::RenderFrame`] 的同一份快照。
pub(super) struct FixedArm<'a> {
    pub(super) frame: &'a super::RenderFrame<'a>,
    pub(super) usable: f32,
    pub(super) motion: ScrollMotion,
    pub(super) now: Instant,
    pub(super) paper_layers: &'a mut Vec<AnyElement>,
    /// 这一帧的翻页产物（与可重排两臂同一份）。
    pub(super) paint: &'a mut super::TurnPaint,
    /// 这一帧的非纸面产物（读数带旗、选中几何、章界落点）。
    pub(super) out: &'a mut super::FrameOut,
}

/// 一页这一帧摆好的样子：页号、每格的位置、字层与命中尺。
struct FixedPageAt {
    page: usize,
    tiles: Vec<FixedTileAt>,
    text: Option<std::rc::Rc<folio_core::content::format::PageText>>,
    text_key: Option<u64>,
    placed: Option<Bounds<Pixels>>,
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
    pub(super) viewer: Option<Entity<ImageViewer>>,
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

/// 一臂一帧共用的那几格。`FixedPaint` 的五处构造只有 viewer / 手势 / 位移 / 倍率
/// 逐趟不同，其余都从这一份出发。
struct FixedPass<'a> {
    book: &'a FixedBook,
    atlas: &'a Entity<ImageAtlas>,
    view: EntityId,
    reader: &'a WeakEntity<ReaderSession>,
    palette: &'a Palette,
    now: Instant,
    viewport: (f32, f32, f32, f32),
    pivot: (f32, f32),
}

impl<'a> FixedPass<'a> {
    /// 实时上屏那一趟的底：不缩放、不带位移、不挂手势。
    fn live(&self) -> FixedPaint<'a> {
        FixedPaint {
            book: self.book,
            atlas: self.atlas,
            view: self.view,
            reader: self.reader,
            viewer: None,
            palette: self.palette,
            long_press: false,
            scrolling: false,
            now: self.now,
            viewport: self.viewport,
            shift: (0.0, 0.0),
            zoom: FixedZoom::default(),
            pivot: self.pivot,
        }
    }

    /// 捕获副本那一趟：手势、放大重画块都不进纹理，只留页格位图。
    fn bake(&self, scrolling: bool) -> FixedPaint<'a> {
        FixedPaint {
            scrolling,
            ..self.live()
        }
    }
}

impl ReaderSession {
    pub(super) fn render_fixed(
        &mut self,
        book: &mut FixedBook,
        arm: FixedArm<'_>,
        overlays: &mut Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) {
        let FixedArm {
            frame,
            usable,
            motion,
            now,
            paper_layers,
            paint,
            out,
        } = arm;
        let super::RenderFrame {
            mode,
            viewport_height,
            page_box,
            content_top,
            gap,
            layer_top,
            capture_front,
            turn,
            view,
            viewer,
            palette,
            long_press,
            atlas,
            reader_weak,
            ..
        } = frame;
        let mode = *mode;
        let viewport_height = *viewport_height;
        let viewport_width = frame.viewport_width();
        let viewport = frame.viewport();
        let page_box = *page_box;
        let content_top = *content_top;
        let area = frame.area();
        let sheet_area = frame.sheet_area();
        let gap = *gap;
        let layer_top = *layer_top;
        let capture_front = *capture_front;
        let turn = *turn;
        let view = *view;
        let palette = *palette;
        let long_press = *long_press;
        let viewer = viewer.clone();
        let atlas = atlas.clone();
        let reader_weak = reader_weak.clone();
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
            let pass = FixedPass {
                book,
                atlas: &atlas,
                view,
                reader: &reader_weak,
                palette,
                now,
                viewport: (0.0, content_top, viewport_width, viewport_height),
                pivot: (0.0, content_top),
            };
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
                // 字层没到就先空着，循环走完统一交给后台取；取回来再补一帧。
                let (text, text_key) = if has_text {
                    page_text_or_pending(book, page, &mut pending_text)
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
                        viewer: viewer.clone(),
                        long_press,
                        scrolling: motion.scrolling,
                        zoom: self.fixed_zoom,
                        ..pass.live()
                    },
                    overlays,
                    &mut boxes,
                    &mut out.fixed_visual,
                    None,
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
                    out.fixed_visual.clone().unwrap_or_default(),
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
            let page_count = pages;
            let spread = mode == chrome::PageMode::Spread;
            // 双页的页码落在**左边那一页**：目录 / 书签 / 恢复跳到奇数页时，摊开的是
            // 它所在的那一对（0/1、2/3……）。页码本身仍按读者要去的那一页落库，
            // 只是这一屏把它折到左页上。
            if spread && book.page() & 1 == 1 {
                book.set_page(book.page() & !1);
            }
            // 恢复落点：定版式的 `locator.chapter` 就是页号。换页那一帧把纹理作废 ——
            // 捕获副本这一帧画的是新页，而 `page_key` 还是打开这一帧之前那个数
            // （`body::render` 在进这一臂之前就算好了），不作废会把它记成「旧页已烘」。
            if let Some(locator) = self.pending_restore.take() {
                book.set_page(locator.chapter as usize);
                if spread {
                    book.set_page(book.page() & !1);
                }
                self.capture_dirty = true;
                self.captured_back = None;
                self.captured_incoming = None;
            }
            self.page = book.page();
            self.content_height = 0.0;
            let current = book.page();
            let slots = fixed_slots(
                current,
                page_count,
                spread,
                usable,
                gap,
                self.prefs.margin_x,
            );
            // 这一帧真正摆上屏的是哪一页：**跟手**那一趟页号还没翻（页号只在松手那一刻
            // 落定），而读者铅笔底下已经该露出目标页了。点屏那趟 `set_page` 先走过，
            // 两者是同一页。捕获副本只在静止帧烘，那时 `turn` 是 `None`，也同一页。
            let shown_page = turn
                .and_then(|frame| frame.arriving)
                .map_or(current, |page| page.page);
            let shown_slots = fixed_slots(
                shown_page,
                page_count,
                spread,
                usable,
                gap,
                self.prefs.margin_x,
            );
            self.laid_out = LaidOut {
                mode,
                columns: mode.columns(),
                pages: page_count,
                viewport: viewport_height,
            };
            // 定版式的下一页先解好：PDF 的一页要回引擎栅格化、CBZ 的一页要读字节 +
            // 解码，从前都要等读者翻过去那一下才开始。静止帧里每帧 prepare 一次，
            // 图集按「这一帧碰过」留住那一格；翻页动画期间不抢这笔活儿。
            if turn.is_none() {
                let mut targets = match motion.direction {
                    Some(direction) => {
                        prefetch::fixed_targets(current, page_count, direction, PREFETCH_RADIUS)
                    }
                    None => prefetch::fixed_target(current, page_count)
                        .into_iter()
                        .collect(),
                };
                // 双页里同屏的另一页、以及下一跨页的左页：它们不在 `page ± 1/2` 这条
                // 数轴上（右页是 `page + 1`），单独补进来。
                for page in slots.pages[..slots.count].iter().copied() {
                    if !targets.contains(&page) {
                        targets.push(page);
                    }
                }
                for target in targets {
                    for tile in book.page_tiles(target) {
                        prepare_tile(&atlas, &tile, view, cx);
                    }
                }
            }
            // 这一屏摆的每一页：页格、字层与命中尺。双页就是相邻两页各占半张，
            // 页仍是既定的位图（不重排、不分栏）。
            let mut pages_at: Vec<FixedPageAt> = Vec::with_capacity(shown_slots.count);
            let mut pending_text: Vec<usize> = Vec::new();
            for (index, page) in shown_slots.pages[..shown_slots.count]
                .iter()
                .copied()
                .enumerate()
            {
                // 这一页有没有字：PDF / AZW4 / DjVu 那一族才有（漫画的一页就是一张图，
                // EPUB 的定版式字在 XHTML 里而不在页上）。字层只查缓存，没到就先不画、
                // 交给后台（理由与竖滚那一臂同一条：绘制帧里同步取一页就是 1.5 s）。
                let has_text = book.page_tiles(page).iter().any(|tile| tile.renders);
                let (text, text_key) = if has_text {
                    page_text_or_pending(book, page, &mut pending_text)
                } else {
                    (None, None)
                };
                let (tiles, placed) = fixed_page_tiles(
                    book,
                    page,
                    shown_slots.width,
                    viewport_height,
                    content_top,
                    shown_slots.lefts[index],
                    text.is_some(),
                );
                pages_at.push(FixedPageAt {
                    page,
                    tiles,
                    text,
                    text_key,
                    placed,
                });
            }
            if !pending_text.is_empty() {
                request_page_texts(book, pending_text, &reader_weak, cx);
            }
            // 缩放几何：整页翻看的是**这一屏摆出来的那几页合起来**的那一块；双页绕着
            // 整跨页的左上角放大，两页才不会各按各的轴心长开。
            let mut bounds: Option<(f32, f32, f32, f32)> = None;
            for at in &pages_at {
                if let Some(((x, y), (w, h))) = fixed_row_bounds(&at.tiles) {
                    bounds = Some(match bounds {
                        None => (x, y, x + w, y + h),
                        Some((left, top, right, bottom)) => {
                            (left.min(x), top.min(y), right.max(x + w), bottom.max(y + h))
                        }
                    });
                }
            }
            let pivot = bounds.map_or((0.0, content_top), |(left, top, _, _)| (left, top));
            let page_size = bounds.map_or((slots.width, viewport_height), |(l, t, r, b)| {
                (r - l, b - t)
            });
            let source_width = zoom_source_width(
                pages_at
                    .iter()
                    .flat_map(|at| at.tiles.iter().map(|tile| &tile.tile)),
            );
            self.fixed_frame = FixedFrame {
                origin: (0.0, content_top),
                pivot,
                page: page_size,
                area: (viewport_width, viewport_height),
                row: (self.prefs.margin_x, slots.width),
                total: 0.0,
                max: max_scale(source_width, page_size.0),
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
            // 选区挂在相邻两页中的哪一页都算「还在这一屏上」；双页里右页选中不该被
            // 当成「翻走了」清掉。不清理会留着一份屏上看不见的选中 —— 菜单条还开着，
            // 而它锚的那一段字已经不在这一帧里了。
            if self.selection.as_ref().is_some_and(|selected| {
                !shown_slots.pages[..shown_slots.count].contains(&selected.chapter)
            }) {
                self.clear_selection();
            }
            let capture = CaptureView::new(
                self.capture_dirty,
                self.captured_size,
                self.captured_page,
                self.captured_incoming,
            );
            let rest = turn_animation::Sheet {
                offset: (0.0, 0.0),
                z: 0,
                cut: None,
            };
            let mut incoming_shift = (0.0, 0.0);
            let mut outgoing_shift = (0.0, 0.0);
            let mut leaving_page: Option<usize> = None;
            let mut curl_mesh: Option<PageCurlMesh> = None;
            let mut flat_leaving: Option<PageCurlMesh> = None;
            let mut back_page: Option<usize> = None;
            let mut back_dx = 0.0f32;
            let pass = FixedPass {
                book,
                atlas: &atlas,
                view,
                reader: &reader_weak,
                palette,
                now,
                viewport: (0.0, content_top, viewport_width, viewport_height),
                pivot: self.fixed_frame.pivot,
            };
            if let Some(frame) = turn {
                let style = turn_animation::of(frame.style);
                // 折线姿态、「一屏有多宽」与两张纸的位移两条臂共走一份计划
                // （`turn_view::plan_sheets`）。定版式的 `page_area` 就是整张纸
                // （`content == paper`），所以单页折页的 `curl_area` 天然是整页；
                // 双页里折页揭的是靠手指那半张 leaf。
                let plan = turn_view::plan_sheets(&frame, mode, area, area);
                if frame.style == PageTurn::Curl {
                    paint.leaf = Some(plan.curl_area);
                }
                paint.shades = style.shades_at(frame.progress, plan.curl_area, plan.pose);
                let fallback = PageRef {
                    chapter: 0,
                    page: current,
                };
                let leaving = frame.leaving.unwrap_or(fallback);
                let arriving = frame.arriving.unwrap_or(fallback);
                leaving_page = Some(leaving.page);
                paint.arriving = Some(arriving);
                // 只有这张「出场页」的捕获确实完整时才开 GPU 卷曲。捕获也可能因为
                // 尺寸刚变或偏好刚重排而不可用；此时强行用旧的纹理，右侧就会采到
                // 捕获矩形外。没有可用纹理时退回实时画，宁可不卷，也不要那条黑带。
                if frame.style == PageTurn::Curl && capture.front_valid(leaving, viewport) {
                    let captured = self.captured_page.unwrap_or((usize::MAX, usize::MAX));
                    let captured_size = self.captured_size.unwrap_or((-1.0, -1.0));
                    curl_mesh = turn_view::build_curl_mesh(
                        &turn_view::CurlAsk {
                            progress: frame.progress,
                            released: matches!(frame.run, TurnRun::Released { .. }),
                            revert: matches!(frame.run, TurnRun::Released { revert: true, .. }),
                            free_right: plan.free_right,
                            pose: plan.pose,
                            curl_area: plan.curl_area,
                            content_area: area,
                            viewport,
                            page_key: (0, current),
                            captured: (captured.0, captured.1, captured_size.0, captured_size.1),
                            capture_valid: true,
                            paper: cx.theme().background.to_rgb(),
                        },
                        &mut self.curl_mesh,
                    );
                }
                let [incoming, outgoing] =
                    style.sheets_at(frame.progress, plan.curl_area, plan.pose);
                incoming_shift = incoming.offset;
                outgoing_shift = outgoing.offset;
                // 卷起来那张纸的背面：单页是目标页整张；双页是目标跨页里**落在 leaf
                // 那一侧**的那一页（往前翻折右半张，背面就是下一跨页的左页）。
                // 没烘过才烘，跨帧留着。
                if curl_mesh.is_some() {
                    back_page = Some(if slots.spread && !plan.free_right {
                        arriving.page + 1
                    } else {
                        arriving.page
                    });
                }
                // 平摊三档：出场页改用静止帧烘好的整页纹理。定版式与可重排走同一个
                // 计划，只是「哪一页不许进捕获层」两个闭包答的内容不同：定版式的页图
                // 就是纹理的全部内容，目标页这一轮不烘（见文档的必须/可省表）。
                if frame.style != PageTurn::Curl {
                    let papers = [
                        Paper::empty(
                            PageRef {
                                chapter: 0,
                                page: arriving.page,
                            },
                            incoming,
                        ),
                        Paper::empty(
                            PageRef {
                                chapter: 0,
                                page: leaving.page,
                            },
                            outgoing,
                        ),
                    ];
                    let count = if leaving == arriving { 1 } else { 2 };
                    let flat = turn_view::plan_flat(&turn_view::FlatAsk {
                        turn: Some(frame),
                        eligible: true,
                        current: 0,
                        page_no: current,
                        papers: &papers,
                        count,
                        sheet_area,
                        area,
                        capture: &capture,
                        viewport,
                        paper: cx.theme().background.to_rgb(),
                        leaving_ready: &|_| true,
                        arriving_ready: &|_| false,
                    });
                    flat_leaving = flat.leaving;
                }
            }
            let flat_capture_on = flat_leaving.is_some();
            if let Some(mesh) = flat_leaving {
                paper_layers.push(super::capture::flat_mesh_canvas(
                    "reader-flat-capture",
                    mesh,
                    (self.enter_shift, layer_top),
                    self.curl_bake_origin.clone(),
                ));
            }
            // 出场那一屏：走捕获纹理 / 卷曲网格时不再摆一次位图（主 pass 里那张纹理由
            // 网格画）；纹理不可用那一帧退回实时画 —— 宁可看着没动，也不把两页叠在一起。
            let outgoing_live = turn.is_some() && !flat_capture_on && curl_mesh.is_none();
            if let (true, Some(leaving)) = (outgoing_live, leaving_page) {
                let leaving_slots = fixed_slots(
                    leaving,
                    page_count,
                    spread,
                    usable,
                    gap,
                    self.prefs.margin_x,
                );
                for (index, page) in leaving_slots.pages[..leaving_slots.count]
                    .iter()
                    .copied()
                    .enumerate()
                {
                    let (tiles, _) = fixed_page_tiles(
                        book,
                        page,
                        leaving_slots.width,
                        viewport_height,
                        content_top,
                        leaving_slots.lefts[index],
                        false,
                    );
                    let paper = Paper::fixed(
                        FixedPaper {
                            page,
                            tiles,
                            text: None,
                            text_key: None,
                            placed: None,
                        },
                        turn_animation::Sheet {
                            offset: outgoing_shift,
                            z: 0,
                            cut: None,
                        },
                        None,
                        (viewport_width, viewport_height),
                    );
                    self.paint_fixed_paper(
                        &paper,
                        &FixedPaint {
                            long_press,
                            scrolling: motion.scrolling,
                            shift: outgoing_shift,
                            ..pass.live()
                        },
                        overlays,
                        &mut Vec::new(),
                        &mut out.fixed_visual,
                        None,
                        cx,
                    );
                }
                let bands = fixed_page_bands(
                    &page_box,
                    &leaving_slots,
                    outgoing_shift,
                    viewport_height,
                    "fixed-page-band",
                    self,
                    cx,
                );
                if !bands.is_empty() {
                    overlays.extend(bands);
                    out.bands_drawn = true;
                }
            }
            // 进场 / 当前那一屏：每页一张纸，双页左右两半同屏。命中表收这一屏的所有页。
            let mut boxes = Vec::new();
            let mut drew_text = false;
            for at in pages_at {
                let paper = Paper::fixed(
                    FixedPaper {
                        page: at.page,
                        tiles: at.tiles,
                        text: at.text,
                        text_key: at.text_key,
                        placed: at.placed,
                    },
                    turn_animation::Sheet {
                        offset: incoming_shift,
                        z: 1,
                        cut: None,
                    },
                    Some(self.fixed_zoom),
                    (viewport_width, viewport_height),
                );
                drew_text |= self.paint_fixed_paper(
                    &paper,
                    &FixedPaint {
                        viewer: viewer.clone(),
                        long_press,
                        scrolling: motion.scrolling,
                        shift: incoming_shift,
                        zoom: self.fixed_zoom,
                        ..pass.live()
                    },
                    overlays,
                    &mut boxes,
                    &mut out.fixed_visual,
                    None,
                    cx,
                );
            }
            let bands = fixed_page_bands(
                &page_box,
                &shown_slots,
                incoming_shift,
                viewport_height,
                "fixed-page-band",
                self,
                cx,
            );
            if !bands.is_empty() {
                overlays.extend(bands);
                out.bands_drawn = true;
            }
            *self.fixed_text_boxes.borrow_mut() = boxes;
            if drew_text {
                overlays.push(selection::overlay(
                    out.fixed_visual.clone().unwrap_or_default(),
                    &reader_weak,
                ));
            }
            // 正面捕获副本：静止帧把这一屏的页图收进捕获层 1。只收**位图**；痕迹与
            // 选中留在实时层 —— 定版式的字在那张栅格图里，痕迹必须压在页图之上，
            // 烘进纹理等于把一次交互状态钉死在纸上（与可重排相反）。
            if capture_front {
                let mut over = Vec::new();
                for (index, page) in shown_slots.pages[..shown_slots.count]
                    .iter()
                    .copied()
                    .enumerate()
                {
                    let (tiles, _) = fixed_page_tiles(
                        book,
                        page,
                        shown_slots.width,
                        viewport_height,
                        content_top,
                        shown_slots.lefts[index],
                        false,
                    );
                    let paper = Paper::fixed(
                        FixedPaper {
                            page,
                            tiles,
                            text: None,
                            text_key: None,
                            placed: None,
                        },
                        rest,
                        None,
                        (viewport_width, viewport_height),
                    );
                    self.paint_fixed_paper(
                        &paper,
                        &pass.bake(false),
                        &mut Vec::new(),
                        &mut Vec::new(),
                        &mut None,
                        Some(&mut over),
                        cx,
                    );
                }
                // 正面纹理也带一份带子：平摊 / 折页时旧页的页眉页脚跟着纸走，
                // 不是停在窗口上下。
                let bands = fixed_page_bands(
                    &page_box,
                    &shown_slots,
                    (0.0, 0.0),
                    viewport_height,
                    "fixed-page-band-capture",
                    self,
                    cx,
                );
                let bands = (!bands.is_empty())
                    .then(|| div().absolute().inset_0().children(bands).into_any_element());
                paint.front = Some(PaperCapture {
                    element: super::paper::empty_capture_element(),
                    under: Vec::new(),
                    over,
                    bands,
                });
            }
            // 背面捕获副本：卷曲那一趟把目标页摆进捕获层 2。双页时先把它按**到达那一
            // 跨页**里的位置画，再由 `back_dx` 挪进 leaf 那一格（网格按 leaf 烘、
            // 也按 leaf 采）。单页的 leaf 就是整张纸，不挪。
            if let Some(back) = back_page
                && self.captured_back != Some(((0, back, 0), viewport))
            {
                let key = (0, back, 0);
                let back_slots =
                    fixed_slots(back, page_count, spread, usable, gap, self.prefs.margin_x);
                let index = if spread && back_slots.pages[0] != back {
                    1
                } else {
                    0
                };
                let left = back_slots.lefts[index];
                if spread && let Some(leaf) = paint.leaf {
                    back_dx = leaf.left - left;
                }
                paint.back_dx = back_dx;
                let (tiles, _) = fixed_page_tiles(
                    book,
                    back,
                    back_slots.width,
                    viewport_height,
                    content_top,
                    left,
                    false,
                );
                // 这一页本来就没有可画的格（越界的尾页）：背面该是一张干净的纸。
                // 与「像素还没到」分开 —— 后者下一帧再试，前者现在就把纹理清成纸色，
                // 否则采到的是上一趟留下的那一页。
                let expects_images = tiles.iter().any(|at| at.tile.request.is_some());
                let paper = Paper::fixed(
                    FixedPaper {
                        page: back,
                        tiles,
                        text: None,
                        text_key: None,
                        placed: None,
                    },
                    rest,
                    None,
                    (viewport_width, viewport_height),
                );
                let mut over = Vec::new();
                // 背面只在卷曲那一趟采：高清没在手就用低清那把，别在动画帧里
                // 起一趟几百毫秒的栅格化（与 `painted_tile` 的滑动手势同一档）。
                self.paint_fixed_paper(
                    &paper,
                    &pass.bake(true),
                    &mut Vec::new(),
                    &mut Vec::new(),
                    &mut None,
                    Some(&mut over),
                    cx,
                );
                // 一张图都没收进来、而这一页本该有图的背面不交：记成「已烘」之后它
                // 不会再试，卷起来的背面就永远是一张纸色。下一帧预览到了再烘。
                if !over.is_empty() || !expects_images {
                    paint.back_key = Some(key);
                    paint.back = Some(PaperCapture {
                        element: super::paper::empty_capture_element(),
                        under: Vec::new(),
                        over,
                        bands: None,
                    });
                }
            }
            paint.mesh = curl_mesh;
        }
    }

    /// 画一张定版式纸：页格、放大重画块、痕迹与选中块。
    ///
    /// 连续竖滚与整页翻只在 `paint` 的位移 / 轴心 / 倍率上不同；页格怎么摆、
    /// 「看大图」入口归谁、痕迹压在页图之上，两档只有这一条路。
    ///
    /// `capture` 是捕获副本那一趟要收的地方：只收**页格位图**（放大重画块与
    /// 痕迹 / 选中都不收 —— 前者两样极少同时用，后者是交互状态，烘进纹理等于
    /// 把它钉在纸上）。`None` = 正常上屏那一趟。
    ///
    /// 返回这一页有没有真的取出字层（调用方据此决定摆不摆选中件）。
    #[allow(clippy::too_many_arguments)]
    fn paint_fixed_paper(
        &mut self,
        paper: &Paper,
        paint: &FixedPaint<'_>,
        overlays: &mut Vec<AnyElement>,
        boxes: &mut Vec<(usize, Bounds<Pixels>)>,
        fixed_visual: &mut Option<SelectionVisual>,
        capture: Option<&mut Vec<AnyElement>>,
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
        let baking = capture.is_some();
        // 这一帧真摆上了几张纸：卷曲那一趟旧页在捕获纹理里，只该摆一张（门的读数）。
        if !baking {
            self.fixed_sheets_drawn += 1;
        }
        let no_entry: Option<Entity<ImageViewer>> = None;
        {
            let target: &mut Vec<AnyElement> = match capture {
                Some(list) => list,
                None => overlays,
            };
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
                // 捕获副本一份手势都不挂：同一屏第二份监听会把点击吃两遍。
                let entry = if baking || (selected && paint.long_press) {
                    &no_entry
                } else {
                    &paint.viewer
                };
                let index = target.len();
                target.push(page_image(
                    ImageSpot {
                        x: f32::from(rect.origin.x),
                        y: f32::from(rect.origin.y),
                        width: f32::from(rect.size.width),
                        height: f32::from(rect.size.height),
                    },
                    image,
                    key,
                    ImageTap {
                        viewer: entry,
                        long_press: paint.long_press && !baking,
                        index,
                        reader: paint.reader,
                    },
                ));
                if baking {
                    self.fixed_capture_images += 1;
                    continue;
                }
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
                        let index = target.len();
                        target.push(page_image(
                            ImageSpot {
                                x: f32::from(zoomed.origin.x),
                                y: f32::from(zoomed.origin.y),
                                width: f32::from(zoomed.size.width),
                                height: f32::from(zoomed.size.height),
                            },
                            image,
                            key,
                            ImageTap {
                                viewer: &no_entry,
                                long_press: paint.long_press,
                                index,
                                reader: paint.reader,
                            },
                        ));
                    }
                }
            }
        }
        if baking {
            return false;
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
