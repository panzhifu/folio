//! 捕获相关的纯判定与坐标换算。
//!
//! 这一层不碰元素树：只回答「这一帧要不要烘」「矩形是不是整块落在窗口内」
//! 「阅读页局部坐标落到窗口的哪儿」。真正发捕获请求的仍是 `body` 的绘制回调。

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use crate::ui::features::reader::model::session::PageRef;
use crate::ui::features::reader::motion::turn_animation;
use gpui_kit::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId,
    InteractiveElement as _, IntoElement, LayoutId, PageCurlMesh, PageCurlUniforms, PageCurlVertex,
    ParentElement as _, Pixels, Point, Styled, TestSupportExt as _, Window, canvas, div, point, px,
    size,
};

/// 这一帧页面捕获的状态快照。
///
/// 从会话里先把四个数抄出来：分页臂里 `self.source` 已被可变借用，闭包里再借 `self`
/// 过不去；抄成值以后三处「这张纹理是不是这一页的」共用同一条判据。
#[derive(Clone, Copy)]
pub(in crate::ui::features::reader) struct CaptureView {
    dirty: bool,
    size: Option<(f32, f32)>,
    front: Option<(usize, usize)>,
    incoming: Option<((usize, usize), (f32, f32))>,
}

impl CaptureView {
    pub(in crate::ui::features::reader) fn new(
        dirty: bool,
        size: Option<(f32, f32)>,
        front: Option<(usize, usize)>,
        incoming: Option<((usize, usize), (f32, f32))>,
    ) -> Self {
        Self {
            dirty,
            size,
            front,
            incoming,
        }
    }

    pub(in crate::ui::features::reader) fn front_valid(
        self,
        page: PageRef,
        viewport: (f32, f32),
    ) -> bool {
        !self.dirty && self.size == Some(viewport) && self.front == Some((page.chapter, page.page))
    }

    pub(in crate::ui::features::reader) fn incoming_valid(
        self,
        page: PageRef,
        viewport: (f32, f32),
    ) -> bool {
        !self.dirty && self.incoming == Some(((page.chapter, page.page), viewport))
    }
}

/// 这一帧要不要（重新）烘那张旧页纹理。
///
/// 五个输入各自挡掉一种情况的：
///
/// - `animating`：动画期间场景里已经多了一张新页，烘出来的就不是正在被翻的那张。
/// - `continuous`：连续滚动根本不是「一页」，也没有卷起来的那张纸要采它。
/// - `content_ready`：**这一章的排版已经是当前键那一代**（`Document::is_current`）。
///   没排完时画出来的只有纸色，而那张纸色一旦被记成「已烘」就不会再烘第二次 ——
///   第一次进书划那一下仿真翻页整页白，就是这一格缺着（见 `view/body` 里 `page_key`
///   那一段注释）。它与 `dirty` 是**并列**的两条：`dirty` 说「手里那张过期了」，
///   这一条说「手里那张（或马上要烘的那张）根本不会有内容」。
/// - `dirty` / `captured`：偏好、主题、尺寸、摆法变了要重烘；同一页且不脏就不重复跑
///   整趟捕获 pass。
pub(in crate::ui::features::reader) fn capture_wanted(
    animating: bool,
    continuous: bool,
    content_ready: bool,
    dirty: bool,
    captured: bool,
) -> bool {
    !animating && !continuous && content_ready && (dirty || !captured)
}

/// 捕获矩形（**阅读页局部**坐标）：整张纸那一块，横向抵消掉外壳进场那一段的横移。
///
/// 副本画在最终位置、矩形也按最终位置取，于是进书那 280 ms 里就能烘：页框滑出窗口的那一截
/// 是永远烘不进纹理的（纹理只有整窗大），而网格随后按整页 UV 去采，边上一条黑带。
///
/// **这块矩形与 `page_area` 同一把尺**，也就是阅读页自己的坐标（y = 0 是这一屏的顶边）。
/// 绘制回调里 `bounds.origin` 已经是**窗口**坐标 —— 阅读层从窗口 0 起，
/// 那块 canvas 的原点因此是 `(enter_shift, 0)`。取捕获矩形要先把进场横移还掉
/// （[`shape_in_window`]），否则每一条边都多出一个 `enter_shift`：矩形下沿越过窗口底，
/// `in_viewport` 每一帧都拒，仿真折页的门永远不开，点屏幕什么都不发生。
pub(in crate::ui::features::reader) fn capture_region(
    area: turn_animation::Area,
    enter_shift: f32,
) -> Bounds<Pixels> {
    Bounds::new(
        point(px(area.left - enter_shift), px(area.top)),
        size(px(area.width()), px(area.height())),
    )
}

/// 把一块**阅读页局部**矩形落到窗口坐标上。
///
/// `origin` 是那块 canvas 在窗口里的原点（`bounds.origin`，已经把进场横移吃掉了一部分），
/// `layer` 是外壳报给这一屏的那两格（`ReaderSession::enter_shift` / 层顶 0）—— 两者同源，所以
/// 「这一块自己在窗口里的原点」就是 `origin - layer`。用同源的两个数相减而不是把
/// `top_inset` 写死成 0：外壳哪天改了推法，两边仍然对得上。
pub(in crate::ui::features::reader) fn shape_in_window(
    origin: Point<Pixels>,
    layer: (f32, f32),
    local: Bounds<Pixels>,
) -> Bounds<Pixels> {
    Bounds::new(
        point(
            origin.x - px(layer.0) + local.origin.x,
            origin.y - px(layer.1) + local.origin.y,
        ),
        local.size,
    )
}

/// 捕获矩形与窗口边界比较时容的浮点误差（逻辑像素）。
///
/// 整张纸的下沿就是屏幕底：几百个像素加减出来的 f32 可能比窗口高多出小数末位的一点点，
/// 而 `in_viewport` 是一条「差一点就整趟不烘」的硬闸。容半像素，只吞舍入误差。
const CAPTURE_EDGE_TOLERANCE: f32 = 0.5;

/// 整张纸这一帧该占的那块矩形（**阅读页局部**坐标）：从阅读页顶铺到窗口底，宽是整窗。
///
/// 只按**窗口自己的高度**量，不把两条系统边 / 两条带子的高加回去：
/// 阅读层从窗口 0 起（`shell::reading_overlay`），
/// 阅读页坐标里的 0 就是那块盒的顶边，`viewport_size().height` 就是它的高 —— 挖孔那一条
/// 与手势条那一条本来就在这之外。从前这里是「字那几格之和」，而且逐项各自可能被重复计
/// （`set_reserve` 已经是顶上 + 底下两条之和）：算出来的纸比窗口高，捕获矩形于是每一帧都
/// 被 [`in_viewport`] 拒掉。拒掉之后捕获永远不落地 ⇒ `curl_capture_ready()` 恒假 ⇒
/// 仿真折页那一趟被排在门外无限期等待，点屏幕没有任何反应。
pub(in crate::ui::features::reader) fn sheet_area_in_page(
    viewport: (f32, f32),
) -> turn_animation::Area {
    turn_animation::Area::new(0.0, 0.0, viewport.0.max(0.0), viewport.1.max(0.0))
}

/// 这一块是否完整落在窗口内 —— 出了一截就**不要**请求捕获（那一截永远烘不进来，
/// 而网格仍按整页采 UV；真机读数就是页边那条黑色横带）。
pub(in crate::ui::features::reader) fn in_viewport(
    rect: Bounds<Pixels>,
    viewport: (f32, f32),
) -> bool {
    f32::from(rect.origin.x) >= -CAPTURE_EDGE_TOLERANCE
        && f32::from(rect.origin.y) >= -CAPTURE_EDGE_TOLERANCE
        && f32::from(rect.origin.x) + f32::from(rect.size.width)
            <= viewport.0 + CAPTURE_EDGE_TOLERANCE
        && f32::from(rect.origin.y) + f32::from(rect.size.height)
            <= viewport.1 + CAPTURE_EDGE_TOLERANCE
}

/// 把一整棵子树只画进捕获层 `layer`（1 = 旧页正面，2 = 旧页背面）的壳。
///
/// 从前这是三个兄弟元素：`canvas` 闸 → 内容 → `canvas` 闸，靠**绘制顺序**保证「内容在层里」。
/// 那条不变量只要有一处被跳过（闸那个 `canvas` 没上屏、重放时把 `SetCaptureLayer` 丢了），
/// 内容就落进主 pass：克隆出来的那一页字形与正文逐字重合地再画一遍 —— 读起来是「整页字印了
/// 两遍」，而它只在「这一页还没烘好」的那几帧出现（见 [`capture_wanted`]），所以是「有的页面
/// 有、有的页面没有」。
///
/// 做成一个元素之后，切层与画子树在**同一趟 `paint`** 里：不存在「层切了但内容没画」或
/// 「内容画了但层没切」的姿势，层号也不可能从这一帧漏进主场景。
struct CaptureLayer {
    layer: u8,
    child: AnyElement,
}

impl Element for CaptureLayer {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        // 壳没有自己的状态，也没有自己的布局盒：`request_layout` 直接把子树那一格交出去。
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        // 进层之前必须停在主场景：层里再套一层，外层那一段会整块丢字。
        let before = window.capture_layer();
        debug_assert_eq!(
            before, 0,
            "捕获层套层了：切到 {} 之前场景在 {before}",
            self.layer
        );
        window.set_capture_layer(self.layer);
        self.child.paint(window, cx);
        // 无论子树画了什么都切回来：漏了这一步，后面所有内容都会排进捕获段、主 pass 一个都不画。
        window.set_capture_layer(0);
    }
}

impl IntoElement for CaptureLayer {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// [`CaptureLayer`] 的构造口。`layer` 只能是 1 或 2 —— 0 是主场景，没有要夹的东西。
pub(in crate::ui::features::reader) fn capture_layer(
    layer: u8,
    child: impl IntoElement,
) -> AnyElement {
    debug_assert!(layer != 0, "capture_layer(0) 没有意义：主场景不需要壳");
    CaptureLayer {
        layer,
        child: child.into_any_element(),
    }
    .into_any_element()
}

/// 把一张平摊捕获网格画到这一帧。
///
/// 网格的坐标是**阅读页局部**，`bounds.origin` 是窗口坐标：先把这一层自己的平移还掉，
/// 再把采样原点换成烘那一刻的那一个（纹理里被写过的只有当时那一块，层在动时两者不同）。
pub(in crate::ui::features::reader) fn flat_mesh_canvas(
    id: &'static str,
    mesh: PageCurlMesh,
    layer: (f32, f32),
    bake_origin: Rc<Cell<Option<(f32, f32)>>>,
) -> AnyElement {
    div()
        .id(ElementId::from(id))
        .test_support()
        .absolute()
        .inset_0()
        .child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    let mut mesh = mesh.clone();
                    let origin =
                        point(bounds.origin.x - px(layer.0), bounds.origin.y - px(layer.1));
                    mesh.bounds.origin = point(
                        mesh.bounds.origin.x + origin.x,
                        mesh.bounds.origin.y + origin.y,
                    );
                    mesh.page_bounds.origin = point(
                        mesh.page_bounds.origin.x + origin.x,
                        mesh.page_bounds.origin.y + origin.y,
                    );
                    mesh.uniforms.page_origin[0] += f32::from(origin.x);
                    mesh.uniforms.page_origin[1] += f32::from(origin.y);
                    mesh.uniforms.capture_origin[0] += f32::from(origin.x);
                    mesh.uniforms.capture_origin[1] += f32::from(origin.y);
                    if let Some((x, y)) = bake_origin.get() {
                        mesh.uniforms.capture_origin = [x, y];
                    }
                    window.paint_page_curl_mesh(mesh);
                },
            )
            .absolute()
            .inset_0(),
        )
        .into_any_element()
}

/// 平面捕获网格：把烘好的整页纹理当一张不会变形的纸搬。
///
/// 左右平移 / 上下平移 / 覆盖三档没有 GPU 卷曲，从前是逐帧重画字形的 —— 文字在移动中
/// 会随子像素档位换位图，看起来发虚/闪。这里把静止帧已经烘好的整页纹理用卷曲网格的
/// **平摊分支**画出来：`axis_d` 放到页宽的四倍，所有点的 `past` 都 < 0，顶点与片元
/// 都走「原样采样纸面」那一支。网格只用四个角，不建曲面。
///
/// `cut` 只在覆盖档有：两张纸会重叠，旧页只画它还没被盖住的那一侧。网格的裁剪框只能
/// 是矩形，而覆盖的折线正好是竖直/水平，所以这里把矩形收到折线那一侧；斜折线只有
/// 仿真折页有，不走这一路。
pub(in crate::ui::features::reader) fn flat_capture_mesh(
    sheet: turn_animation::Area,
    cut_area: turn_animation::Area,
    offset: (f32, f32),
    cut: Option<turn_animation::Cut>,
    paper: gpui_kit::Rgba,
    incoming: f32,
) -> PageCurlMesh {
    let width = sheet.width().max(1.0);
    let height = sheet.height().max(1.0);
    let page_origin = (sheet.left + offset.0, sheet.top + offset.1);
    let mut left = page_origin.0;
    let mut top = page_origin.1;
    let mut right = left + width;
    let mut bottom = top + height;
    if let Some(cut) = cut {
        let crease = cut.crease;
        if crease.ny.abs() < 1e-3 {
            // 覆盖档：折线竖直。`d` 是页面局部坐标，先折回窗口。
            let edge = cut_area.left + crease.d / crease.nx;
            if cut.keeps(cut_area.left, cut_area.top) {
                right = right.min(edge);
            } else {
                left = left.max(edge);
            }
        } else if crease.nx.abs() < 1e-3 {
            let edge = cut_area.top + crease.d / crease.ny;
            if cut.keeps(cut_area.left, cut_area.top) {
                bottom = bottom.min(edge);
            } else {
                top = top.max(edge);
            }
        }
    }
    let bounds = Bounds::new(
        point(px(left), px(top)),
        size(px((right - left).max(0.0)), px((bottom - top).max(0.0))),
    );
    let vertices: Arc<[PageCurlVertex]> = Arc::from(vec![
        PageCurlVertex {
            position: [0.0, 0.0],
            uv: [0.0, 0.0],
        },
        PageCurlVertex {
            position: [width, 0.0],
            uv: [1.0, 0.0],
        },
        PageCurlVertex {
            position: [width, height],
            uv: [1.0, 1.0],
        },
        PageCurlVertex {
            position: [0.0, height],
            uv: [0.0, 1.0],
        },
    ]);
    let indices: Arc<[u32]> = Arc::from(vec![0, 1, 2, 0, 2, 3]);
    PageCurlMesh {
        bounds,
        page_bounds: bounds,
        vertices,
        indices,
        uniforms: PageCurlUniforms {
            page_origin: [page_origin.0, page_origin.1],
            page_size: [width, height],
            window_size: [0.0, 0.0],
            axis_normal: [1.0, 0.0],
            // 页内 x ∈ [0, width]，取 4 倍宽：所有点都落在平摊那一侧。
            axis_d: width * 4.0,
            radius: width.max(height),
            camera_distance: width.max(height) * 3.0,
            scale_factor: 1.0,
            back_color: [paper.r, paper.g, paper.b, paper.a],
            light_dir: [0.0, 0.0, 1.0, 0.0],
            cone_a: [0.0; 4],
            cone_b: [0.0; 4],
            capture_origin: [sheet.left, sheet.top],
            // `0` = 采出场页那张（正面捕获），`1` = 采进场页那张（捕获层 3 烘出来的）。
            incoming,
            back_capture_origin: [sheet.left, sheet.top],
        },
    }
}
