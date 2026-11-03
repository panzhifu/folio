//! 图集像素、页图预览/高清、放大区域与钉住的区域。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use crate::ui::features::reader::components::image_viewer::ImageViewer;
use crate::ui::features::reader::model::fixed_book::{FixedBook, PageTile};
use crate::ui::features::reader::model::fixed_zoom::{FixedZoom, region_output, visible_region};
use crate::ui::features::reader::model::image_atlas::ImageAtlas;
use crate::ui::features::reader::model::session::{
    ImageHit, PinnedRegion, REGION_SETTLE, ReaderSession, StackHit,
};
use crate::ui::features::reader::view::scene::VisibleImage;
use crate::ui::foundation::images::DecodeRequest;
use folio_core::content::format::{PageRegion, pdf::RENDER_EDGE};
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    AnyElement, App, ClickEvent, Div, Entity, EntityId, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderImage, StatefulInteractiveElement as _, Styled, TestSupportExt as _,
    WeakEntity, Window, canvas, div, img, px,
};

use super::gestures::long_press_to_open;

/// 捕获层里的一张图：与 [`image_child`] 同一块矩形，但**不带**任何手势与元素 id。
///
/// 同一屏摆两个同 id 的元素，命中区就叠了一份 —— 「看大图」那一击可能落到纹理的副本上。
/// 而层里这一份根本不上屏（主 pass 跳过它的 order），它只要被捕获 pass 取走，画出来就够。
pub(super) fn plain_image(
    x: f32,
    top: f32,
    width: f32,
    height: f32,
    rendered: Arc<RenderImage>,
) -> AnyElement {
    div()
        .absolute()
        .left(px(x))
        .top(px(top))
        .w(px(width))
        .h(px(height))
        .child(img(rendered).size_full())
        .into_any_element()
}

/// 这一帧这张图画得出来吗 —— 顺手把「这一帧在场」记进图集（帧末的回收按这个数放人）。
///
/// 还在解 / 解不出来的给 `None`：那一块空着，正文照排（与从前容器丢失时同一个表现），
/// 解完那一趟会 `notify(view)` 自己逼出一帧，不需要这里去轮询。
pub(super) fn painted(
    atlas: &Entity<ImageAtlas>,
    request: DecodeRequest,
    view: EntityId,
    cx: &mut App,
) -> Option<Arc<RenderImage>> {
    let __t = crate::ui::features::reader::model::perf::Timer::start();
    let image = atlas.update(cx, |atlas, cx| atlas.painted(&request, view, cx));
    let __image = __t.ns();
    crate::ui::features::reader::model::perf::add(|frame| frame.image_ns += __image);
    image
}

/// 一格的像素：先画低清预览，高清到了再换。
///
/// 两档**不同时起**：高清那一趟的回开容器 + 栅格化要几秒，先起它会把低清挤在同一批
/// 核上（实测手机上低清也跟着拖到三四秒才出来）。这里按顺序排：预览先到先画，
/// 预览在手之后才排高清 —— 首图从「等一整趟 2048」变成「等一趟 720」。
///
/// 替换时排版不动：两张图的声明尺寸相同，只有像素变了。低清那格在高清到货后不再被
/// 问，两帧后由图集回收。
pub(super) fn painted_tile(
    atlas: &Entity<ImageAtlas>,
    tile: &PageTile,
    view: EntityId,
    cx: &mut App,
    scrolling: bool,
) -> Option<Arc<RenderImage>> {
    if let Some(preview) = tile.preview.as_ref() {
        if let Some(low) = painted(atlas, preview.clone(), view, cx) {
            // **快速滑动时只要高清已经在手就换，但不去起它。** 高清那一趟是几十到几百
            // ms 的栅格化，滑动中起它等于把「下一帧」的钱挪到「这一帧」上 —— 读者看见
            // 的是滑动卡顿，而他要的那张图本来也还在下面好几屏外。停下 150 ms 之后
            // 那一帧 `scrolling` 为假，这里才排高清（`atlas.prepare` 自己会去重）。
            let high = tile.request.as_ref();
            if scrolling
                && !high.is_some_and(|request| {
                    atlas.read_with(cx, |atlas, _| atlas.ready(&request.key))
                })
            {
                return Some(low);
            }
            if let Some(image) = tile
                .request
                .as_ref()
                .and_then(|request| painted(atlas, request.clone(), view, cx))
            {
                return Some(image);
            }
            return Some(low);
        }
        // 预览解不出来（坏页）就退回高清；还只是没到就继续等，**不碰高清**（理由见上）。
        if !atlas.read_with(cx, |atlas, _| atlas.failed(&preview.key)) {
            return None;
        }
    }
    // 没有预览的格子（容器里本来就有图）：直接走它自己那一趟。
    tile.request
        .as_ref()
        .and_then(|request| painted(atlas, request.clone(), view, cx))
}

/// 正文插图（与 CSS 背景图）那一格：先画低清预览，高清到了再换。
///
/// 这是 [`painted_tile`] 给定版式那一族早就在做的事，2026-10-31 搬到可重排正文上：
/// 那一头的书里插图没有预览档，于是「解完之前什么都不画」在快滑时读成一片白纸
/// （`view::body` 从前那个 `continue`）。顺序照旧是**预览先到、高清后起**，理由也照旧：
/// 两趟同时排进池子会挤在同一批核上，实测手机上低清跟着拖到三四秒。
///
/// 与 `painted_tile` 的一处分别是这里没有 `scrolling` 那一档闸门：高清只在**它的预览
/// 到货之后**才起，而预览到货时这一格还在屏上 —— 快滑里一页只停两三帧，那条闸门
/// 实际挡不住什么，却要把它一路塞进 `decoration_child`。滑动期间的预取深度另有一刀
/// （`doc/图片回收.md` 刀 3）。
///
/// 替换时排版不动：两张图的声明尺寸相同，只有像素变了。低清那格在高清到货后不再被问，
/// 两帧后由图集回收（它记在低清那一档的账上，不占高清的额度）。
pub(super) fn painted_picture(
    atlas: &Entity<ImageAtlas>,
    request: &DecodeRequest,
    view: EntityId,
    cx: &mut App,
) -> Option<Arc<RenderImage>> {
    let Some(preview) = request.preview() else {
        // 这张图本来就不大（或尺寸没记在索引里，见 `DecodeRequest::preview` 那三条）：
        // 直接走它自己那一趟，与从前一模一样。
        return painted(atlas, request.clone(), view, cx);
    };
    if let Some(low) = painted(atlas, preview.clone(), view, cx) {
        if let Some(high) = painted(atlas, request.clone(), view, cx) {
            return Some(high);
        }
        return Some(low);
    }
    // 预览还只是没到：这一格继续空着，**不碰高清**（理由与 `painted_tile` 那一段相同）。
    // 预览解不出来（坏图）才退回高清，那一页不该因为低清失败而永远白着。
    if !atlas.read_with(cx, |atlas, _| atlas.failed(&preview.key)) {
        return None;
    }
    painted(atlas, request.clone(), view, cx)
}

/// 预取一格：只排**预览**那一趟，而且排进 [`Priority::Prewarm`] 档。
///
/// 高清是翻到那一页时才值得花的钱 —— 预取高清会把下一趟翻页的首帧又拖回几百毫秒
/// （手机上是几秒），而预取低清足够让那一页先出来。
///
/// 档位降一档是为了**池子里的取活顺序**：读者手上这一页的预览/高清是 `Interactive`，
/// 预取两页是 `Prewarm`，于是「翻过去那一下」不会被排在两页预取后面（见
/// `folio_core::Priority` 的档序）。
pub(super) fn prepare_tile(
    atlas: &Entity<ImageAtlas>,
    tile: &PageTile,
    view: EntityId,
    cx: &mut App,
) {
    if let Some(request) = tile.preview.as_ref().or(tile.request.as_ref()) {
        let request = request.clone().with_priority(folio_core::Priority::Prewarm);
        atlas.update(cx, |atlas, cx| atlas.prepare(&request, view, cx));
    }
}

/// 放大上限要的「源宽」。
///
/// **矢量页不按手里这一张算**：那种页捏稳之后会把看得见的那一块按 `REGION_EDGE`
/// 重画一遍，细节上限是 `RENDER_EDGE → REGION_EDGE` 那一倍，所以宽也按那一倍报给
/// [`max_scale`]。扫描件与容器图那一族手里这一张就是全部 —— 按它们自己的宽度封顶，
/// 于是「放到 8 倍」对那一族仍然是糊的，而那正是源图的极限（计划 §六 写明的限制）。
pub(super) fn zoom_source_width<'a>(tiles: impl IntoIterator<Item = &'a PageTile>) -> f32 {
    let tiles: Vec<&PageTile> = tiles.into_iter().collect();
    let base: f32 = tiles.iter().map(|tile| tile.width as f32).sum();
    let boost = if tiles.iter().any(|tile| tile.vector) {
        folio_core::content::format::pdf::REGION_EDGE as f32 / RENDER_EDGE as f32
    } else {
        1.0
    };
    base * boost
}

/// 页表像素那一块 → **1x 内容坐标**里的那一块（同一张纸的同一个地方，两把尺）。
pub(super) fn region_rect(
    tile: (f32, f32, f32, f32),
    region: PageRegion,
    source: (u32, u32),
) -> (f32, f32, f32, f32) {
    let (tx, ty, tw, th) = tile;
    let frac = |from: u32, span: u32| from as f32 / span.max(1) as f32;
    let (x0, y0) = (frac(region.x0, source.0), frac(region.y0, source.1));
    let (x1, y1) = (frac(region.x1, source.0), frac(region.y1, source.1));
    (tx + x0 * tw, ty + y0 * th, (x1 - x0) * tw, (y1 - y0) * th)
}

/// 一次「放大停稳了没有、该重画哪一块」的输入。
///
/// 合成一个结构而不是十三个参数：连续竖滚与整页翻两臂递的是同一组量，摊成名字之后
/// 调用点自己说清「哪一格、哪一块屏、哪一个轴心、哪一刻」。
pub(super) struct RegionAsk<'a> {
    pub(super) book: &'a FixedBook,
    pub(super) atlas: &'a Entity<ImageAtlas>,
    pub(super) page: usize,
    pub(super) tile: &'a PageTile,
    /// 那一格在 **1x 内容坐标**里的矩形（dp）。
    pub(super) rect: (f32, f32, f32, f32),
    /// 缩放轴心（dp）：两臂不同 —— 连续滚是版心左上角，整页翻是铺满那一页的左上角。
    pub(super) pivot: (f32, f32),
    pub(super) viewport: (f32, f32, f32, f32),
    pub(super) zoom: FixedZoom,
    pub(super) now: std::time::Instant,
}

/// 放大那一档：这一帧该不该排「看得见的那一块按 `REGION_EDGE` 重画」，以及钉着的那一张
/// 该叠在哪。交回 `Some(PinnedRegion)` 只表示**这一帧该去问图集要那一块的像素**
/// （还没到货时 `painted` 给 `None`，那一页就只画它手里那张 2048 —— 叠层是加细节，
/// 永远不该让屏上出现空洞）。
///
/// 三条规矩：
/// - 只有**矢量的渲染页**问得出东西（`FixedBook::region_request` 对扫描件与容器图回
///   `None`，那一趟栅格化根本不会起）；
/// - 只有**停稳** [`REGION_SETTLE`] 之后才排：手在动就一帧帧把表推后（与「滑动中只铺
///   预览」同一条理由 —— 那一趟是几百 ms 的活，边滑边排等于把下一帧的钱花在这一帧）；
/// - 叠层画在**它自己那一刻**的矩形上：读者随后平移，那一张跟着自己的矩形走，
///   新的那一块要等下一次停稳。这就是 [`PinnedRegion`] 存在的唯一理由。
pub(super) fn pinned_region(
    ask: RegionAsk<'_>,
    moved_at: &Cell<Option<std::time::Instant>>,
    pinned: &RefCell<Option<PinnedRegion>>,
    view: EntityId,
    cx: &mut App,
) -> Option<PinnedRegion> {
    let RegionAsk {
        book,
        atlas,
        page,
        tile,
        rect,
        pivot,
        viewport,
        zoom,
        now,
    } = ask;
    if !tile.vector || !zoom.is_zoomed() {
        // 撤大回铺满：钉着的那一张不再属于这一屏（它占的图集名额由帧末回收按「没在场」放掉）。
        moved_at.set(None);
        pinned.borrow_mut().take();
        return None;
    }
    let source = (tile.width, tile.height);
    let region = visible_region(zoom, pivot, rect, source, viewport)?;
    let scale = zoom.scale();
    // 这一块已经钉住了而且倍率没变：一帧一趟都不多排。
    if let Some(pin) = pinned.borrow().as_ref()
        && pin.region == region
        && (pin.scale - scale).abs() < 0.01
    {
        return Some(pin.clone());
    }
    let Some(last_moved) = moved_at.get() else {
        // 第一次看见放大后的这一屏：起表，这一帧不排。
        moved_at.set(Some(now));
        return None;
    };
    if now.duration_since(last_moved).as_millis() < REGION_SETTLE {
        // 还在动（或刚停下）：把表推后，旧的那一张继续按它自己的矩形画。
        moved_at.set(Some(now));
        return pinned.borrow().clone();
    }
    // 停够了：算输出尺寸 → 排进图集（`Interactive`：读者正看着这一块）→ 钉住。
    let (width, height) = region_output(region, source)?;
    let request = book.region_request(page, region, width, height)?;
    atlas.update(cx, |atlas, cx| atlas.prepare(&request, view, cx));
    let pin = PinnedRegion {
        region,
        scale,
        rect: region_rect(rect, region, source),
        request,
    };
    *pinned.borrow_mut() = Some(pin.clone());
    Some(pin)
}

/// 一张图在屏上的摆位矩形（定版式那一臂按格给，正文插图自带在 `VisibleImage` 里）。
#[derive(Clone, Copy)]
pub(super) struct ImageSpot {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) width: f32,
    pub(super) height: f32,
}

/// 「点这张图」要的那几格：入口实体、按端分岔、命中表序号、会话句柄。
pub(super) struct ImageTap<'a> {
    pub(super) viewer: &'a Option<Entity<ImageViewer>>,
    pub(super) long_press: bool,
    pub(super) index: usize,
    pub(super) reader: &'a WeakEntity<ReaderSession>,
}

/// 拖动看大图那一侧的记账：平移偏移与这一帧的命中表。
pub(super) struct ImageDrag<'a> {
    pub(super) pan: (f32, f32),
    pub(super) hits: &'a Rc<RefCell<Vec<ImageHit>>>,
    pub(super) frame_no: u64,
    pub(super) chapter: usize,
    pub(super) stack_hits: &'a Rc<RefCell<Vec<StackHit>>>,
}

/// 一片里的一张图：绝对摆位交给 gpui 的 `img`，外加「点一下摊开看」。
///
/// 一张图若是**脚注标记**（老书把标记做成一张小「注」图），那一击归脚注 —— 抬起注文，
/// 不进图片查看器。判据是排版期就解好的 `image.footnote`（见 [`VisibleImage::footnote`]），
/// 这一层只按结果分流。
///
/// 「看大图」的入口与定版式那一页同一套（见 [`page_image`]）：按得住长按的那一端用**长按**
/// —— 单击留给翻页，插图上一下点按不该把正文顶开一块浮层；按不住长按的那一端（桌面鼠标）
/// 退回单击。
///
/// `rendered` 是图集那一层交回来的像素：`image` 里只有摆位与那条活儿（`DecodeRequest`），
/// 像素不随场景走（见 [`crate::ui::features::reader::model::image_atlas`]）。
pub(super) fn image_child(
    image: VisibleImage,
    rendered: Arc<RenderImage>,
    tap: ImageTap<'_>,
    drag: ImageDrag<'_>,
) -> AnyElement {
    let ImageTap {
        viewer,
        long_press,
        index,
        reader,
    } = tap;
    let ImageDrag {
        pan,
        hits,
        frame_no,
        chapter,
        stack_hits,
    } = drag;
    // `object-fit`：图按哪一块区域采样进这个盒。`Fill`（初值）与前一字不差的
    // `size_full()`；`Contain` 留底、`Cover` 裁、`None` 原尺寸 —— 三种都要先算出一个
    // 可能超出盒的矩形，再靠盒的 `overflow_hidden` 裁。
    let source = rendered.size(0);
    let (source_w, source_h) = (source.width.0 as f32, source.height.0 as f32);
    let (ox, oy, w, h) = image
        .fit
        .placement(source_w, source_h, image.width, image.height);
    // 这一张比框大出多少：`cover` 按比例盖满盒子，多出来的那一圈被 `overflow_hidden`
    // 裁掉 —— 那一圈正是读者想拖着看的内容。`slack` 为 0 的图不进命中表（拖它没意义）。
    let slack = ((w - image.width).max(0.0), (h - image.height).max(0.0));
    let draggable = slack.0 > 0.5 || slack.1 > 0.5;
    // 偏移是**当前**拖到哪儿（负值 = 内容往上/往左露出来的那一截）。
    let (px_off, py_off) = if draggable { pan } else { (0.0, 0.0) };
    let key = image.image.key.clone();
    // 叠里的图带一点圆角（Reeden 图册那个形状）；单张图仍按原样直角。
    let radius = if image.stack.is_some() { 8.0 } else { 0.0 };
    let frame = div()
        .absolute()
        .left(px(image.x))
        .top(px(image.top))
        .w(px(image.width))
        .h(px(image.height))
        .rounded(px(radius))
        .overflow_hidden()
        .child(
            img(rendered)
                .absolute()
                .left(px(ox + px_off))
                .top(px(oy + py_off))
                .w(px(w))
                .h(px(h)),
        );
    // 脚注标记优先：它是「注」图，不是插图，点它要的是注文而不是放大。
    if let Some(note) = image.footnote {
        return footnote_marker(frame, note, reader.clone(), index);
    }
    // 可拖动的那些：把框登记进本帧的命中表（`on_scroll` 按按下点查它，决定这一笔
    // 翻页还是拖图）。只登记、不拦事件 —— 拦下来就再没有翻页手势了。
    let frame = if draggable {
        let hits = hits.clone();
        let key = key.clone();
        frame.child(image_pan_zone(key, slack, hits, frame_no))
    } else {
        frame
    };
    // 一叠图：登记这一格给横滑切图认领，并在角上标 `k/n`。
    //
    // 角标不是装饰 —— 一叠只占一格，屏上看着就是一张图，不标一下读者不知道
    // 这里还有四张 —— 没有出口，折叠就等于把 8 张压成 1 张。
    let frame = match image.stack {
        Some(mark) if mark.count > 1 => frame
            .child(stack_zone(mark, chapter, stack_hits.clone(), frame_no))
            .child(
                div()
                    .id("reader-stack-badge")
                    .test_support()
                    .absolute()
                    .top(px(6.0))
                    .right(px(6.0))
                    .px(px(6.0))
                    .rounded(px(8.0))
                    .text_xs()
                    .child(format!("{} / {}", mark.index + 1, mark.count)),
            ),
        _ => frame,
    };
    match viewer {
        Some(viewer) if long_press => frame
            .child(long_press_to_open(viewer.clone(), reader.clone(), key))
            .into_any_element(),
        Some(viewer) => {
            open_on_tap(frame, viewer.clone(), reader.clone(), key, index).into_any_element()
        }
        None => frame.into_any_element(),
    }
}

/// 叠图的「下一张」卡片边：在图下面露出 6px 的一条，提示还能往左滑。
///
/// 宽度取图的 92%、水平居中，颜色是前景色的 12%（暗底亮底都看得出，又不抢图）。
/// 它画在图**之前**，上面那一截被图盖住，露出来的就是一条圆角的边 —— 不提前解下一张
/// 的图，也不改排版。
pub(super) fn stack_peek(x: f32, bottom: f32, width: f32, cx: &App) -> AnyElement {
    let inset = width * 0.04;
    div()
        .id("reader-stack-peek")
        .test_support()
        .absolute()
        .left(px(x + inset))
        .top(px(bottom - 4.0))
        .w(px(width - inset * 2.0))
        .h(px(12.0))
        .rounded(px(8.0))
        .bg(cx.theme().foreground.opacity(0.12))
        .into_any_element()
}

/// 一叠图底下那排小圆点：`count` 个，当前那张实心。
///
/// 与图右上角那格 `k / n` 角标**同时存在**：角标说「第几张、一共几张」，这排点说的是
/// 「还能往两边走」。样式照着 Reeden 的图册来 —— 5px、间距 4、居中、当前那张用前景色，
/// 其余用弱化的前景色（不抢图的注意力）。
pub(super) fn stack_dots(
    x: f32,
    bottom: f32,
    width: f32,
    mark: folio_core::layout::StackMark,
    cx: &App,
) -> AnyElement {
    let on = cx.theme().foreground;
    let off = cx.theme().muted_foreground.opacity(0.35);
    div()
        .id("reader-stack-dots")
        .test_support()
        .absolute()
        .left(px(x))
        .top(px(bottom + 5.0))
        .w(px(width))
        .flex()
        .justify_center()
        .gap(px(4.0))
        .children((0..mark.count).map(move |i| {
            div()
                .w(px(5.0))
                .h(px(5.0))
                .rounded(px(2.5))
                .bg(if i == mark.index { on } else { off })
        }))
        .into_any_element()
}

/// 一张可拖动插图的**命中区**：只把这一帧它在窗口里的矩形登记下来，不拦任何事件。
///
/// 为什么用 `canvas` 而不是 `on_click`/`on_mouse_down`：拖动这一笔必须交给
/// `on_scroll`（识别器把触摸折成滚动事件，`TouchDragEvent` 只在按下那一下递一次探针，
/// 接了就把点屏翻页与长按吸词一起吃掉 —— 那一条写在 `swipe.rs` 的模块注释里）。
/// 这里要的只是「这张图这一帧在窗口的哪一块」，而绘制回调给的 `bounds` 正是窗口坐标。
pub(super) fn image_pan_zone(
    key: String,
    slack: (f32, f32),
    hits: Rc<RefCell<Vec<ImageHit>>>,
    frame_no: u64,
) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, _window, _cx| {
            let mut hits = hits.borrow_mut();
            // 同一帧里重放可能再来一次：同一个 key 只留一条（后写的框是最终位置）。
            if let Some(existing) = hits.iter_mut().find(|hit| hit.key == key) {
                existing.rect = bounds;
                existing.slack = slack;
                existing.frame = frame_no;
            } else {
                hits.push(ImageHit {
                    key: key.clone(),
                    rect: bounds,
                    slack,
                    frame: frame_no,
                });
            }
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

/// 一叠图那一格的**命中区**：只登记矩形，不拦事件（与 [`image_pan_zone`] 同一条理由 ——
/// 拦下来就把点屏翻页与长按吸词一起吃了）。切图那一笔由 `swipe_scroll` 按起手点认领。
pub(super) fn stack_zone(
    mark: folio_core::layout::StackMark,
    chapter: usize,
    hits: Rc<RefCell<Vec<StackHit>>>,
    frame_no: u64,
) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, _window, _cx| {
            let mut hits = hits.borrow_mut();
            if let Some(existing) = hits
                .iter_mut()
                .find(|hit| hit.chapter == chapter && hit.id == mark.id)
            {
                existing.rect = bounds;
                existing.count = mark.count;
                existing.frame = frame_no;
            } else {
                hits.push(StackHit {
                    chapter,
                    id: mark.id,
                    count: mark.count,
                    rect: bounds,
                    frame: frame_no,
                });
            }
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

/// 给一张脚注标记图挂上「点开看注」。
///
/// 与 `open_on_tap` 同一条取舍（`block_mouse_except_scroll` 把这一击从整屏正文的
/// 分区里拿回来），只是去向换成会话的脚注状态：注文 + 点击位置一起交出去，弹层由
/// 阅读页画（见 `page::ReadingPage::footnote_popover`）。
pub(super) fn footnote_marker(
    frame: Div,
    note: String,
    reader: WeakEntity<ReaderSession>,
    index: usize,
) -> AnyElement {
    frame
        .id(("reader-footnote-image", index))
        .block_mouse_except_scroll()
        .on_click(move |event: &ClickEvent, _window, cx| {
            let at = event.position();
            let note = note.clone();
            let _ = reader.update(cx, |session, cx| session.show_footnote(note, at, cx));
        })
        .test_support()
        .into_any_element()
}

/// 定版式一页的元素：绝对定位到 `(x, y)`、撑满 `w × h`，外加「看大图」那一条入口。
///
/// 这一臂的入口与正文插图不同，而且是**按端不同**：定版式的页几乎占满整屏，单击就是
/// 翻页本身。按得住长按的那一端把入口挂在长按上（单击一个字都不动，翻页照常）；
/// 按不住的那一端（桌面鼠标根本没有长按）挂单击，翻页让给页外的留白与音量键。
/// 判据在 [`crate::ui::host::supports_long_press`]，这一层不写 `cfg`。
pub(super) fn page_image(
    spot: ImageSpot,
    image: Arc<RenderImage>,
    key: String,
    tap: ImageTap<'_>,
) -> AnyElement {
    let ImageSpot {
        x,
        y,
        width: w,
        height: h,
    } = spot;
    let ImageTap {
        viewer,
        long_press,
        index,
        reader,
    } = tap;
    let frame = div()
        .absolute()
        .left(px(x))
        .top(px(y))
        .w(px(w))
        .h(px(h))
        .child(img(image).size_full());
    let Some(viewer) = viewer.clone() else {
        return frame.into_any_element();
    };
    let reader = reader.clone();
    if long_press {
        frame
            .child(long_press_to_open(viewer, reader, key))
            .into_any_element()
    } else {
        open_on_tap(frame, viewer, reader, key, index).into_any_element()
    }
}

/// 把「正文里的这一张」翻成「整本书那一列里的第几条」，然后开那一层。
///
/// 为什么按**图集的键**去问而不是把手上的像素递过去：查看器摆的现在是那一列里的第几张 ——
/// 左右滑要能接着往下走、计数要念得出来、定位原文要有出处，三样都在那一列里，
/// 光有一份像素哪一样都给不出。
///
/// 问不到（那一列里没有这一条）时这一击什么都不做：开一层「0 / 0」、既滑不动也定不了位，
/// 比这一下没反应更难向读者解释。
pub(in crate::ui::features::reader) fn open_picture(
    viewer: &Entity<ImageViewer>,
    reader: &WeakEntity<ReaderSession>,
    key: &str,
    window: &mut Window,
    cx: &mut App,
) {
    let viewport = viewport_size(window);
    // 下标与那一列一起取回来：`Arc` 的克隆只是引用计数，查看器因此不必每帧回会话要。
    let ask = reader.update(cx, |session, _| {
        session
            .image_index_of_key(key)
            .map(|index| (index, session.image_sequence().clone()))
    });
    // 会话已经随阅读页收掉了（`WeakEntity` 升级不上来）时这一击什么都不做。
    let Some((index, sequence)) = ask.ok().flatten() else {
        tracing::trace!("图片查看器: {key} 不在整本书那一列里，这一击不开");
        return;
    };
    viewer.update(cx, |viewer, cx| {
        viewer.open(index, viewport, sequence);
        cx.notify();
    });
}

/// 给一张图挂上「点开放大」。
///
/// `block_mouse_except_scroll` 而不是 `occlude`：这一击要从正文那三层分区里拿回来
/// （gpui 的一击会发给每一个 hitbox 含按下点的 handler，而正文那层是整屏的 —— 见
/// `page.rs` 里顶栏那条同一取舍），但读者**从图上起手**也要能把正文滚走，所以只挡鼠标
/// 不挡滚动。
///
/// 带 id 才挂得住点击，`index` 就是这一帧里它是第几张被摆上屏的图（按 `overlays` 当时的
/// 长度取，跨章连续滚动时也不会撞名）。名字化的 id 还留了个口子：宿主的测试可以按
/// `("reader-image", 0)` 直接点在那张图上。
pub(super) fn open_on_tap(
    frame: Div,
    viewer: Entity<ImageViewer>,
    reader: WeakEntity<ReaderSession>,
    key: String,
    index: usize,
) -> impl IntoElement {
    frame
        .id(("reader-image", index))
        .block_mouse_except_scroll()
        .on_click(
            move |event: &ClickEvent, window: &mut Window, cx: &mut App| {
                let at = event.position();
                // 这一击先问「选中」那一套，再谈放大 —— 走的是正文那一臂同一个入口
                // （`ReaderSession::tap_on_body`，四条判据与顺序都写在那儿）。为什么要在
                // 这一层问：`block_mouse_except_scroll` 把这一击从正文那三层里抢了过来，
                // 正文那一臂就再也收不到它 —— 于是桌面上读 PDF 的人点在已划的那条线上，
                // 会被这张图整页放大抢走，「挑回去改一条」那条路在定版式上断掉。
                // 三条判据都不成立（没有选区、没压在划线上、不在拖选的合成 click 里）才开
                // 大图：普通单击的行为与从前逐字相同。
                let claimed = reader
                    .update(cx, |session, cx| session.tap_on_body(window, cx, at))
                    .unwrap_or(false);
                if claimed {
                    return;
                }
                open_picture(&viewer, &reader, &key, window, cx);
            },
        )
        // 挂在最外层：宿主的测试要按 `("reader-image", 序)` 点在这张图上。
        .test_support()
}

/// 窗口视口，两个轴都是 CSS 像素。查看器第一帧拿它当摆位的格子，之后由那一层自己
/// 量出来的宽高校正（见 `image_viewer::overlay` 里那块 `canvas`）。
pub(super) fn viewport_size(window: &Window) -> (f32, f32) {
    let size = window.viewport_size();
    (f32::from(size.width), f32::from(size.height))
}
