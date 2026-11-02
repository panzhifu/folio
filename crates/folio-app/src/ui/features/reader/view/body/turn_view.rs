//! 翻页那一层：折面与投影、三张捕获纹理的请求、卷曲网格。
//!
//! 这五块都只往正文那棵树上压 `canvas`，而且共用同一把尺：网格与副本的坐标是
//! **阅读页局部**，`canvas` 回调拿到的 `bounds.origin` 是**窗口坐标**，所以每一块
//! 都先把这一层自己的平移（`CapturePass::layer` = 外壳进场横移 + 浮层顶）还掉，
//! 再与「烘那一刻」记下的原点对齐。两处差多少像素，纹理边上就有多少像素采在
//! 没写过的区域 —— 读起来是纸边一条黑带。

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::{
    Background, Bounds, Div, Hsla, PageCurlMesh, PageCurlUniforms, PageCurlVertex, ParentElement,
    Rgba, Styled, canvas, div, hsla, linear_color_stop, linear_gradient, point, px, size,
};

use crate::ui::features::reader::model::chrome;
use crate::ui::features::reader::model::session::PageRef;
use crate::ui::features::reader::model::session::{
    BackCommit, CaptureCommit, CurlMesh, IncomingCommit, TurnRun,
};
use crate::ui::features::reader::motion::turn_animation::{self, Tint};

use super::capture::{
    CaptureView, capture_layer, capture_region, flat_capture_mesh, in_viewport, shape_in_window,
};
use super::decor::paint_fold;
use super::paper::{Paper, PaperCapture};
use super::TurnFrame;

/// 一份捕获副本要挂进哪一层、横向偏移多少。
pub(super) struct CaptureLayer {
    /// 捕获层的号：1 = 正面，2 = 背面，3 = 进场页。
    pub(super) order: u8,
    /// 这一层的横向偏移（已经含 `-enter_shift`）：副本按它退回**最终位置**。
    pub(super) dx: f32,
    pub(super) capture: PaperCapture,
}

/// 三份捕获副本入树。
///
/// 主 pass 只画 `order < CAPTURE_ORDER_BASE`，层里的东西不上屏、只被捕获 pass 取走，
/// 所以副本可以画在与上屏那一份不同的位置上（外壳进场那 280 ms 的横移要在这一层抵消）。
///
/// 入树必须排在字形元素**之前**：`log_paint` 会把本帧那一份账 `take()` 走，副本晚了就
/// 并进下一帧去（见 `perf::log_capture_paint`）。三份也不会同帧都出现 —— 正面只在定住
/// 那一帧烘，背面只在动画在飞且目标还没烘过时烘。
///
/// 每份都走 [`capture_layer`]：切层与画子树在同一个元素的那一趟 `paint` 里，**不靠
/// 兄弟元素的绘制顺序**。从前这里是「闸 → 内容 → 闸」三个兄弟，闸那一步只要被跳过
/// （`canvas` 没上屏、重放把 `SetCaptureLayer` 丢了），副本就落进主 pass，与正文逐字
/// 重合地再画一遍 —— 读起来就是「整页字印了两遍」。
pub(super) fn mount_capture_layers(column: Div, layers: Vec<CaptureLayer>) -> Div {
    layers.into_iter().fold(column, |column, layer| {
        let CaptureLayer {
            order,
            dx,
            capture: PaperCapture {
                element,
                under,
                over,
                bands,
            },
        } = layer;
        column.child(capture_layer(
            order,
            div()
                .absolute()
                .left(px(dx))
                .top(px(0.0))
                .size_full()
                .children(under)
                .child(element)
                .children(over)
                .children(bands),
        ))
    })
}

/// 一块翻页元素这一帧的坐标口径。
#[derive(Clone, Copy)]
pub(super) struct CapturePass {
    /// `(外壳进场横移, 浮层顶)`：`canvas` 的 `bounds.origin` 减掉它才回到阅读页局部。
    pub(super) layer: (f32, f32),
    /// 窗口尺寸（逻辑像素）：捕获矩形出窗就这一帧不请求。
    pub(super) viewport: (f32, f32),
    /// 捕获区清屏色 = 纸张底色。取 uv 落在捕获矩形外半像素时，透明黑就是
    /// 页边那条黑带的来源（参照实现的 `glClearColor(mBackgroundColor)` 同一口径）。
    pub(super) clear: Rgba,
}

/// 盖在两张纸**之上**的那几块：折页的纸面与它的四道阴影，覆盖 / 层叠的前缘投影与纸边光。
///
/// 它们都不属于任何一张纸 —— 两张纸的字形 runs 烘在同一个元素里，纸与纸之间没有可插入的
/// 绘制层 —— 所以只能作为一组盖上去的多边形来画。形状、渐变方向、浓淡全由
/// [`turn_animation`] 算好，这一头一个几何都不做。
///
/// 层级是字形之上、图片之下：这些影子本来就落在纸面上，而图是纸面上的另一层内容。
///
/// `mesh_on` 是这一帧有没有走 GPU 卷曲网格：网格自己画纸面与影子，这一层就让开。
pub(super) fn paint_shades(
    column: Div,
    shades: turn_animation::Shades,
    mesh_on: bool,
    layer: (f32, f32),
    paper: Hsla,
) -> Div {
    if mesh_on || shades.items().is_empty() {
        return column;
    }
    column.child(
        canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                // 折面与投影的坐标全是**阅读页局部**，而 `bounds.origin` 是窗口坐标：
                // 把这一层自己的平移还掉。
                let origin = point(bounds.origin.x - px(layer.0), bounds.origin.y - px(layer.1));
                for shade in shades.items() {
                    // 零面积与零墨都是「这一帧这一块不存在」：折到尽头折面镜像到
                    // 页外去、投影走到页的左缘自己被裁没，两处都靠这一步收干净。
                    if shade.shape.is_empty() || shade.ink <= 0.0 {
                        continue;
                    }
                    let base = match shade.tint {
                        // 纸面是唯一不透明的一块。有它在那儿，那一片才读成「纸折
                        // 过去了」，而不是「字一排排消失了」。
                        Tint::Paper => paper,
                        Tint::Dark => hsla(0., 0., 0., shade.ink),
                        Tint::Light => hsla(0., 0., 1., shade.ink),
                    };
                    let color: Background = match shade.angle {
                        // 渐变方向也由那一头给：它指的是「离开折线（或离开前缘）
                        // 的那一侧」，所以浓的那端总正好贴在自己的边上。
                        Some(angle) => linear_gradient(
                            angle,
                            linear_color_stop(base, 0.),
                            linear_color_stop(base.opacity(0.), 1.),
                        ),
                        None => base.into(),
                    };
                    paint_fold(window, &shade.shape, origin, color);
                }
            },
        )
        .absolute()
        .inset_0(),
    )
}

/// 请求烘正面那张整页纹理（捕获层 1），并把提交写回 `committed`。
///
/// 无论读者当前选的是哪一档都烘整张纸：仿真折页可以随时被选上，而那一刻不该要求
/// 先等一次重烘；其余几档只是多烘两条带子的字，代价在捕获那一趟里可以忽略。
///
/// `page_key` 是**本次请求**的那一页：绘制回调把它记进 `committed`，下一帧 `render`
/// 才把 `captured_page` 推成它 —— 回调里那份页号是上一次 `render` 读到的，中间若已经
/// 翻过一页，回调会把旧页写成「已烘」。
pub(super) fn request_front_capture(
    column: Div,
    sheet_area: turn_animation::Area,
    page_key: (usize, usize),
    committed: Rc<Cell<Option<CaptureCommit>>>,
    bake_origin: Rc<Cell<Option<(f32, f32)>>>,
    pass: CapturePass,
) -> Div {
    // 捕获矩形按**最终位置**取：副本按 `-enter_shift` 画在最终位置上，两处必须同一个数。
    let region = capture_region(sheet_area, pass.layer.0);
    let CapturePass { layer, viewport, clear } = pass;
    column.child(
        canvas(|_, _, _| {}, move |bounds, _, window, _| {
            // `region` 是阅读页坐标，`bounds.origin` 是窗口坐标：先把这一层自己的
            // 平移（进场横移）还掉，两者才同一套尺。
            let shaped = shape_in_window(bounds.origin, layer, region);
            let at = shaped.origin;
            // 捕获纹理只有整窗大：页框有一截在窗外，那一截就永远烘不进去，而网格仍按
            // 完整页宽采 UV，边上便采到矩形外（真机读数是一条黑色横带）。此时**不请求
            // 捕获**，下一帧重试。进场那一段的横移已经在 `region` 里抵消掉了，走到这儿
            // 还不合身的只剩「窗口本身比这一页窄」（分屏、桌面把窗口拖小）。
            if !in_viewport(shaped, viewport) {
                tracing::warn!(
                    target: "folio_app::debug",
                    x = f32::from(at.x),
                    y = f32::from(at.y),
                    w = f32::from(shaped.size.width),
                    h = f32::from(shaped.size.height),
                    vw = viewport.0,
                    vh = viewport.1,
                    layer_y = layer.1,
                    bounds_y = f32::from(bounds.origin.y),
                    "正面捕获被拒：矩形出窗"
                );
                return;
            }
            // 记下这一刻的原点：画网格那一趟取 uv 要用它，而不是本帧的。
            bake_origin.set(Some((f32::from(at.x), f32::from(at.y))));
            // 捕获矩形落在窗口哪儿 —— 这一格与网格取 uv 用的 `page_origin` 必须是
            // 同一个数，差多少像素，那条边上就有多少像素是采在未写区域（读成黑）。
            tracing::debug!(
                target: "folio_app::perf",
                x = f32::from(at.x),
                y = f32::from(at.y),
                w = f32::from(shaped.size.width),
                h = f32::from(shaped.size.height),
                "烘这一页：捕获矩形"
            );
            window.capture_page_region_with_color(shaped, clear);
            committed.set(Some((page_key, viewport)));
            // 提交落在**下一帧**的 `render` 开头，所以发出请求的这一帧之后还得再有
            // 一帧。这一帧没人买：`capture_page_region_with_color` 只把矩形写进
            // `next_frame.scene`，gpui 也不为触摸或滚动事件重画，而翻页落定那一帧
            // 补的 `notify` 只买得到「发出请求」的这一帧。少了下面这一发，提交一直
            // 挂着落不进 `captured_page` ⇒ 仿真折页那道门（`curl_capture_ready`）
            // 永远不开 ⇒ 读者的下一划在 `claim_swipe` 里整趟被吞。与
            // `chapter_layout` 那条「作业在飞就必须再要一帧」同一个口径。
            window.request_animation_frame();
        })
        .absolute()
        .inset_0(),
    )
}

/// 请求烘背面那张纹理（捕获层 2）：揭起来那张纸的背面 = 目标跨页的另一半（双页）/
/// 目标页整张（单页）。
///
/// 内容与请求都排在**这一帧**：捕获 pass 在主 pass 之前编码，所以同一帧画进层 2 的东西，
/// 这一帧的网格就采得到，不必为它多等一帧。
///
/// 目标章还没排完版那一趟排不出内容（装配那一趟里 `span_doc` 给 `None` 就跳过了），
/// 这一帧仍然请求捕获 —— 纹理被清成纸色，背面是一张干净的纸，而不是上一趟留下的字。
pub(super) fn request_back_capture(
    column: Div,
    leaf: turn_animation::Area,
    key: (usize, usize, usize),
    dx: f32,
    committed: Rc<Cell<Option<BackCommit>>>,
    pass: CapturePass,
) -> Div {
    let region = capture_region(leaf, pass.layer.0);
    let CapturePass { layer, viewport, clear } = pass;
    column.child(
        canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                // 与正面同一把尺：`region` 是阅读页坐标，`bounds.origin` 是窗口坐标。
                let shaped = shape_in_window(bounds.origin, layer, region);
                let at = shaped.origin;
                if !in_viewport(shaped, viewport) {
                    return;
                }
                tracing::debug!(
                    target: "folio_app::perf",
                    chapter = key.0,
                    page = key.1,
                    column = key.2,
                    dx = dx,
                    x = f32::from(at.x),
                    y = f32::from(at.y),
                    w = f32::from(shaped.size.width),
                    h = f32::from(shaped.size.height),
                    "烘背面那一页：捕获矩形"
                );
                window.capture_page_region_back_with_color(shaped, clear);
                committed.set(Some((key, viewport)));
            },
        )
        .absolute()
        .inset_0(),
    )
}

/// 请求烘进场页那张纹理（捕获层 3）。
///
/// 烘焙那一帧与副本同一帧发出请求，矩形按**静止位置**取（与正面同一把尺
/// [`capture_region`]）。动画在飞，帧自然会来 —— 不像正面那张要为「静止帧之后没有
/// 下一帧」补一发 `request_animation_frame`。
pub(super) fn request_incoming_capture(
    column: Div,
    sheet_area: turn_animation::Area,
    key: (usize, usize),
    committed: Rc<Cell<Option<IncomingCommit>>>,
    bake: Rc<Cell<Option<(f32, f32)>>>,
    pass: CapturePass,
) -> Div {
    let region = capture_region(sheet_area, pass.layer.0);
    let CapturePass { layer, viewport, clear } = pass;
    column.child(
        canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let shaped = shape_in_window(bounds.origin, layer, region);
                if !in_viewport(shaped, viewport) {
                    tracing::warn!(
                        target: "folio_app::debug",
                        chapter = key.0,
                        page = key.1,
                        "进场页捕获被拒：矩形出窗"
                    );
                    return;
                }
                bake.set(Some((
                    f32::from(shaped.origin.x),
                    f32::from(shaped.origin.y),
                )));
                tracing::debug!(
                    target: "folio_app::perf",
                    chapter = key.0,
                    page = key.1,
                    x = f32::from(shaped.origin.x),
                    y = f32::from(shaped.origin.y),
                    w = f32::from(shaped.size.width),
                    h = f32::from(shaped.size.height),
                    "烘进场页：捕获矩形"
                );
                window.capture_page_region_incoming_with_color(shaped, clear);
                committed.set(Some((key, viewport)));
            },
        )
        .absolute()
        .inset_0(),
    )
}

/// C 方案的卷曲网格：盖在新页与它的图片之上，UI 浮层之下。
///
/// 旧页的字、图、背景都在捕获纹理里，网格一次画完；这里只把窗口坐标补齐。
///
/// `page_area` 是这一趟翻页取的那块纸面，`leaf` 是被卷起来的那一格。
pub(super) fn paint_curl_mesh(
    column: Div,
    mesh: PageCurlMesh,
    page_area: turn_animation::Area,
    leaf: Option<turn_animation::Area>,
    bake_origin: Rc<Cell<Option<(f32, f32)>>>,
    pass: CapturePass,
) -> Div {
    let CapturePass { layer, .. } = pass;
    column.child(
        canvas(|_, _, _| {}, {
            // 正面纹理按整张纸（top = 0）烘，网格只卷其中一张 leaf：采样原点要往右让出
            // leaf 的起点（单页那一档 leaf 就是整张纸，横纵都是 0；双页是半张、且背面
            // 那块仍按正文上沿烘）。两个偏移各管一轴。
            let leaf_dx = leaf.map_or(0.0, |leaf| leaf.left - page_area.left);
            let leaf_top = leaf.map_or(0.0, |leaf| leaf.top);
            move |bounds, _, window, _| {
                // 走到这里必须已经切回主场景：漏了收尾那道闸，网格的 primitive 会排进
                // 捕获那一段、主 pass 一个都不画 —— 翻页就读成「字消失了但没有纸在卷」，
                // 而且不报错。
                debug_assert_eq!(
                    window.capture_layer(),
                    0,
                    "捕获层没切回主场景就画了网格"
                );
                let mut mesh = mesh.clone();
                // `page_bounds` 是从 `page_area` 出来的**阅读页局部**坐标，而
                // `bounds.origin` 是窗口坐标；采样原点与几何原点都按窗口坐标算，
                // 所以这里也走同一套：先把这一层自己的平移还掉。
                let origin = point(bounds.origin.x - px(layer.0), bounds.origin.y - px(layer.1));
                mesh.bounds.origin = point(mesh.bounds.origin.x + origin.x, mesh.bounds.origin.y + origin.y);
                mesh.page_bounds.origin = point(
                    mesh.page_bounds.origin.x + origin.x,
                    mesh.page_bounds.origin.y + origin.y,
                );
                mesh.uniforms.page_origin[0] += f32::from(origin.x);
                mesh.uniforms.page_origin[1] += f32::from(origin.y);
                mesh.uniforms.capture_origin[0] += f32::from(origin.x);
                mesh.uniforms.capture_origin[1] += f32::from(origin.y);
                // 采样原点换成**烘那一刻**的那一个：那张纹理里被写过的只有当时那一块。
                // 没烘过（`None`）就与几何同源 —— 那一帧渲染器本来也会因为缺纹理
                // 跳过这一趟。
                if let Some((x, y)) = bake_origin.get() {
                    // `capture_origin` 是**页左上角**在窗口里的位置，不是捕获矩形
                    // 的左上角。正面烘的是整张纸，而被卷的那一页在双页里只是半张：
                    // 页原点要从捕获矩形原点加上 `leaf` 的偏移。横 = 半页宽，纵 =
                    // 正文上沿（单页时两个都是 0）。
                    mesh.uniforms.capture_origin = [x + leaf_dx, y + leaf_top];
                    // 背面纹理只烘 `leaf` 那一格，页左上角就是那一格的左上角；
                    // 与正面用的是同一个偏移，所以同一个数。
                    mesh.uniforms.back_capture_origin = [x + leaf_dx, y + leaf_top];
                }
                // 两条原点并排打：它们不等就是层在动，而这正是那条黑带的成因。
                tracing::debug!(
                    target: "folio_app::perf",
                    x = mesh.uniforms.page_origin[0],
                    y = mesh.uniforms.page_origin[1],
                    sample_x = mesh.uniforms.capture_origin[0],
                    sample_y = mesh.uniforms.capture_origin[1],
                    w = mesh.uniforms.page_size[0],
                    h = mesh.uniforms.page_size[1],
                    "画网格：页框原点与采样原点"
                );
                window.paint_page_curl_mesh(mesh);
            }
        })
        .absolute()
        .inset_0(),
    )
}

/// 卷曲网格这一帧的装配输入。
pub(super) struct CurlAsk {
    /// 这一趟走了多少（已经过完缓动）。
    pub(super) progress: f32,
    /// 走的是松手那一腿、以及是不是弹回那一腿：只进途中那份读数日志。
    pub(super) released: bool,
    pub(super) revert: bool,
    /// 自由边在哪一侧（往回翻与从右往左读的那本各反一次）。
    pub(super) free_right: bool,
    pub(super) pose: turn_animation::Pose,
    /// 被卷起来的那一格：双页是靠手指那半张 leaf，单页是整张纸（含页眉/页脚）。
    pub(super) curl_area: turn_animation::Area,
    /// 正文那一块：双页那张 leaf 要越过中缝落到另一半上，裁剪框必须按它给，
    /// 按 leaf 自己剪就把越过中缝的那一片切掉。
    pub(super) content_area: turn_animation::Area,
    pub(super) viewport: (f32, f32),
    /// 今天要烘的那一页 `(章, 页)`，与上一次烘成功那一刻的 `(章, 页, 宽, 高)`：
    /// 都只进读数日志，哪一处不同源，日志里的数字就差哪一处。
    pub(super) page_key: (usize, usize),
    pub(super) captured: (usize, usize, f32, f32),
    /// 出场页那张捕获纹理是否完整（调用方的闸，同一个数记进日志）。
    pub(super) capture_valid: bool,
    /// 清屏色与背面色：闭包里的 `window` 拿不到主题，先取好带进来。
    pub(super) paper: Rgba,
}

/// 卷曲网格：圆柱几何 + 顶点缓存 + uniform。
///
/// 只在仿真折页那一档、且出场页的捕获纹理确实完整时调用。纹理可能因为进场横移
/// 还没停、尺寸刚变或偏好刚重排而不可用；那时强行用半张 / 旧的纹理，右侧就会采到
/// 捕获矩形外。没有可用纹理时退回 CPU 裁页，宁可不卷，也不要那条黑带。
///
/// `cache` 是会话里那一格顶点缓存：顶点与索引只由页宽页高决定，进度改的是 uniform。
/// 尺寸没变就接着用缓存那一份 —— 每帧重造是 9409 个顶点加 55296 个索引（近 1 MB）的
/// 分配与拷贝，全在这一帧的绘制里跑。
pub(super) fn build_curl_mesh(
    ask: &CurlAsk,
    cache: &mut Option<CurlMesh>,
) -> Option<PageCurlMesh> {
    let cylinder = turn_animation::cylinder_for(ask.progress, ask.curl_area, ask.pose)?;
    // 途中那一帧的读数。「页边有时候一条黑带」这种抱怨没法用眼睛验，也没法在
    // headless 里画出来，只能把这几组同源约束一起落进日志：几何（轴 / 卷径）、
    // 版心那块矩形、今天的窗口尺寸与烘纹理那一刻的页号。目标名与 `perf` 那几条
    // 同一个：设备上的 `logging.targets` 里 `folio_app::perf=debug` 早开着。
    tracing::debug!(
        target: "folio_app::perf",
        free_right = ask.free_right,
        progress = ask.progress,
        released = ask.released,
        revert = ask.revert,
        area_left = ask.content_area.left,
        area_top = ask.content_area.top,
        area_right = ask.content_area.right,
        area_bottom = ask.content_area.bottom,
        window_w = ask.viewport.0,
        window_h = ask.viewport.1,
        page = ask.page_key.1,
        chapter = ask.page_key.0,
        captured_ch = ask.captured.0,
        captured_pg = ask.captured.1,
        captured_w = ask.captured.2,
        captured_h = ask.captured.3,
        capture_valid = ask.capture_valid,
        nx = cylinder.nx,
        ny = cylinder.ny,
        axis_d = cylinder.d,
        radius = cylinder.radius,
        "折页途中那一帧的读数"
    );
    let page_width = ask.curl_area.width();
    let page_height = ask.curl_area.height();
    if page_width <= 0.0 || page_height <= 0.0 {
        return None;
    }
    let cached = cache
        .clone()
        .filter(|mesh| mesh.width == page_width && mesh.height == page_height);
    let mesh = match cached {
        Some(mesh) => mesh,
        None => {
            let (vertices, indices) = folio_curl::build_page_mesh(page_width, page_height);
            let built = CurlMesh {
                width: page_width,
                height: page_height,
                vertices: vertices
                    .into_iter()
                    .map(|vertex| PageCurlVertex {
                        position: vertex.position,
                        uv: vertex.uv,
                    })
                    .collect::<Vec<_>>()
                    .into(),
                indices: indices.into(),
            };
            *cache = Some(built.clone());
            built
        }
    };
    let page = Bounds::new(
        point(px(ask.curl_area.left), px(ask.curl_area.top)),
        size(px(page_width), px(page_height)),
    );
    // 裁剪框按**整张纸**给：单页折页时就是含页眉/页脚的那一整页，双页时仍按整块版心。
    // fork 里 `wgpu_renderer/page_curl.rs` 拿 `mesh.bounds` 算出一个矩形再
    // `set_scissor_rect`。取样不受影响：shader 的 uv 由页面局部坐标算，纸盖到屏幕哪儿
    // 都不换采样点，背面那张纹理仍按 leaf 那一格烘。
    let clip_area = if ask.curl_area.spread_leaf {
        ask.content_area
    } else {
        ask.curl_area
    };
    let clip = Bounds::new(
        point(px(clip_area.left), px(clip_area.top)),
        size(px(clip_area.width()), px(clip_area.height())),
    );
    Some(PageCurlMesh {
        bounds: clip,
        page_bounds: page,
        vertices: mesh.vertices,
        indices: mesh.indices,
        uniforms: PageCurlUniforms {
            page_origin: [ask.curl_area.left, ask.curl_area.top],
            // 采样原点：先按本帧的页框填，画那一趟的回调里再换成「烘那一刻」的那一个
            // （`curl_bake_origin`）。层在动时两者不同，混用就会采到纹理里未写的区域。
            capture_origin: [ask.curl_area.left, ask.curl_area.top],
            back_capture_origin: [ask.curl_area.left, ask.curl_area.top],
            // 卷曲网格永远采正面那张（出场页）；进场页仍实时画。
            incoming: 0.0,
            page_size: [page_width, page_height],
            // `window_size` 与 `scale_factor` 由 `paint_page_curl_mesh` 按真实窗口与
            // DPI 覆盖，这里填什么都不参与成像。
            window_size: [0.0, 0.0],
            axis_normal: [cylinder.nx, cylinder.ny],
            axis_d: cylinder.d,
            radius: cylinder.radius,
            camera_distance: (page_width.max(page_height) * 3.0).max(600.0),
            scale_factor: 1.0,
            back_color: [ask.paper.r, ask.paper.g, ask.paper.b, 1.0],
            // 光从**卷起那一侧**的斜上方来（`+n` 是已经卷走的那一半），影子才会落在
            // 还平摊着的那半边上；固定一个世界方向的 `[-0.35, -0.25, …]` 只对正向翻页
            // 成立，往回翻时影子会落到已经卷走的那一边、屏上一片干净。
            //
            // 水平分量只给 0.10：影子的偏移量是 `水平分量 × 纸抬起来的高度`，给到 0.25
            // 时柱顶那一段的影还会从折痕外探出六十来像素；0.10 只探出二十来像素 —— 影
            // 基本压在纸底下，只在折痕那一线露出一点。竖直分量稍大一点，影往下方沉。
            light_dir: [
                cylinder.nx * 0.10,
                cylinder.ny * 0.10 - 0.08,
                1.0,
                0.0,
            ],
            cone_a: [
                cylinder.cone_apex_s,
                cylinder.cone_far_s,
                cylinder.cone_weight,
                cylinder.cone_min,
            ],
            cone_b: [cylinder.cone_max, 0.0, 0.0, 0.0],
        },
    })
}

/// 一张纸这一帧的几何计划：折线姿态、被卷那一格有多宽、自由边在哪一侧。
pub(super) struct SheetPlan {
    /// 「这一屏有多宽」按档答（判据与理由在 [`turn_animation::paper_area`]）：双页里只有
    /// 仿真折页揭靠手指那半张 leaf（它有背面），其余几档是单面的一张纸、带不动两栏的新
    /// 内容，就按整块版心走。纸的位移、手的归一化、逐栏取舍、网格的裁剪框四件事都从这
    /// 一个数出来。单页折页时 `page_area` 已经是整张纸（含页眉/页脚），所以这里拿到的
    /// 就是那一整张。
    pub(super) curl_area: turn_animation::Area,
    /// 自由边到底在哪一侧（往回翻与「从右往左读的那一本」各反一次）：折页那一档的几何
    /// 全按它判，别的档不看。
    pub(super) free_right: bool,
    /// 摆位层（逐字裁剪）与 GPU 网格（判哪一半已经卷走）从这里**共走一个**入口：
    /// `cylinder_for` 一份几何两处用，「圆柱的轴就是那条折线」不可能在两处走样。
    pub(super) pose: turn_animation::Pose,
}

/// 按档算出 [`SheetPlan`]。两条臂（可重排与定版式）共走这一份，翻页几何才只有一套尺。
pub(super) fn plan_sheets(
    frame: &super::TurnFrame,
    mode: chrome::PageMode,
    page_area: turn_animation::Area,
    area: turn_animation::Area,
) -> SheetPlan {
    let style = turn_animation::of(frame.style);
    let free_right = turn_animation::Pose::free_edge_right(frame.forward, frame.rtl);
    let curl_area = turn_animation::paper_area(style, mode, page_area, free_right);
    // 手是在 `page_area` 里归一化的；双页那张 leaf 要映进它自己的 0..1，
    // 否则从右缘划到中缝只走了半张 leaf，纸却已经该翻完了。
    let frame_grip = frame.grip.map(|grip| {
        if curl_area.spread_leaf {
            grip.remap(area, curl_area)
        } else {
            grip
        }
    });
    // 手与弹回的时钟：走完那一腿把这一笔沿**它自己的方向**拉长到落点，弹回那一腿不借手
    // —— 手指停在松手那一刻那一格，改由 `Pose::revert` 那一个数让折线自己退出去。两条腿
    // 的第一帧都逐字等于松手那一刻，所以都不跳；这一腿自己走了几成由
    // [`turn_animation::release_clock`] 给（两边的尺不一样：那一腿 `progress` 是往 0 退的）。
    let (grip, revert) = match frame.run {
        TurnRun::Released { from, revert, .. } => {
            let u = turn_animation::release_clock(frame.progress, from, revert);
            if revert {
                (frame_grip, Some(u))
            } else {
                (
                    frame_grip.map(|grip| {
                        turn_animation::release_grip(grip, u, free_right, curl_area)
                    }),
                    None,
                )
            }
        }
        _ => (frame_grip, None),
    };
    SheetPlan {
        curl_area,
        free_right,
        pose: turn_animation::Pose {
            forward: frame.forward,
            rtl: frame.rtl,
            grip,
            revert,
        },
    }
}

/// 平摊三档这一帧要不要改用整页纹理，以及哪一页用哪一张。
pub(super) struct FlatPlan {
    /// 出场页那张（捕获层 1 烘好的）。`None` = 这一帧仍实时画。
    pub(super) leaving: Option<PageCurlMesh>,
    /// 进场页那张（捕获层 3 烘好的）。`None` = 仍实时画。
    pub(super) arriving: Option<PageCurlMesh>,
    /// 这一帧要烘进场页的那一格 `(章, 页)` 与它自己的位移；`None` = 不烘。
    pub(super) incoming_bake: Option<((usize, usize), (f32, f32))>,
    /// 有没有一页走了平摊网格：网格纹理里已含读数带，末端就不补全局兜底带。
    pub(super) mesh_drawn: bool,
}

/// 出场页这一帧改用静止时烘好的整页纹理。左右平移 / 上下平移 / 覆盖三档从前逐帧重画
/// 字形，文字会随子像素档位在帧间换位图（读者看到的是「图上的字在闪」）；纹理搬过去
/// 就没有这一层重栅格化。
///
/// 只换出场页：捕获纹理只有正面这一张，进场页还是实时画。仿真折页那张由卷曲网格自己
/// 画，不走这里（`eligible` 由调用方按档与内容判，两臂各判各的排除项）。
///
/// `leaving_ready` / `arriving_ready` 回答「这一页的内容烘进去是有东西的」：整页画
/// （`background-attachment: fixed` / 单图页）铺在元素树外面，烘进纹理只有纸色；目标章
/// 还没排完版时也是空的。那种页退回实时画，下一帧再试。
/// 平摊装配这一帧的输入。
pub(super) struct FlatAsk<'a> {
    pub(super) turn: Option<TurnFrame>,
    /// 这一帧能不能走平摊纹理（档与内容的排除项由调用方判：仿真折页自己画、整页画不进
    /// 捕获层，两条臂各判各的）。
    pub(super) eligible: bool,
    /// 定住时出场页就是「当前这一页」，`TurnFrame::leaving` 缺着时用它兜。
    pub(super) current: usize,
    pub(super) page_no: usize,
    pub(super) papers: &'a [Paper; 2],
    pub(super) count: usize,
    pub(super) sheet_area: turn_animation::Area,
    pub(super) area: turn_animation::Area,
    pub(super) capture: &'a CaptureView,
    pub(super) viewport: (f32, f32),
    /// 清屏色 = 纸张底色，烘进去的那张纹理没写过的地方就是它。
    pub(super) paper: Rgba,
    pub(super) leaving_ready: &'a dyn Fn(PageRef) -> bool,
    pub(super) arriving_ready: &'a dyn Fn(PageRef) -> bool,
}

pub(super) fn plan_flat(ask: &FlatAsk<'_>) -> FlatPlan {
    let rest = FlatPlan {
        leaving: None,
        arriving: None,
        incoming_bake: None,
        mesh_drawn: false,
    };
    let Some(frame) = ask.turn.filter(|_| ask.eligible) else {
        return rest;
    };
    let sheet_of = |page: PageRef| {
        ask.papers
            .iter()
            .take(ask.count)
            .find(|paper| paper.identity == page)
            .map(|paper| paper.sheet)
    };
    let leaving = frame.leaving.unwrap_or(PageRef {
        chapter: ask.current,
        page: ask.page_no,
    });
    let mut flat_leaving = None;
    if ask.capture.front_valid(leaving, ask.viewport)
        && (ask.leaving_ready)(leaving)
        && let Some(at) = sheet_of(leaving)
    {
        flat_leaving = Some(flat_capture_mesh(
            ask.sheet_area,
            ask.area,
            at.offset,
            at.cut,
            ask.paper,
            0.0,
        ));
    }
    let mut incoming_bake = None;
    let mut flat_arriving = None;
    if let Some(arriving) = frame.arriving {
        let ready = (ask.arriving_ready)(arriving);
        let at = sheet_of(arriving);
        if ask.capture.incoming_valid(arriving, ask.viewport) {
            // 已经烘好那一格：整页跳过，改画平摊网格。
            if ready && let Some(at) = at {
                flat_arriving = Some(flat_capture_mesh(
                    ask.sheet_area,
                    ask.area,
                    at.offset,
                    at.cut,
                    ask.paper,
                    1.0,
                ));
            }
        } else if ready && let Some(at) = at {
            // 还没烘：这一帧照旧实时画，同时把副本交给捕获层 3 烘一张。
            incoming_bake = Some(((arriving.chapter, arriving.page), at.offset));
        }
    }
    // 有没有一页走网格要先问，再把两张网格交出去。
    let mesh_drawn = flat_leaving.is_some() || flat_arriving.is_some();
    FlatPlan {
        leaving: flat_leaving,
        arriving: flat_arriving,
        incoming_bake,
        mesh_drawn,
    }
}
