//! 图片放大查看：把一张图摊满整屏，能缩放、能拖着看，点空白处关掉。
//!
//! 入口有两条，各按内容的形态定：
//!
//! - **可重排正文里的插图**：单击开。那一击原本只是「开合浮层」（`chrome::zone` 中间
//!   那一档），让给放大更值钱；图外的字与图行之间的空白照旧开合浮层、照旧翻页。
//! - **定版式那一页**（CBZ / 逐页 SVG 漫画）：页几乎占满屏，单击就是翻页本身，所以
//!   移动端**长按**开、桌面**单击图内**开（桌面没有长按这一档，判据在
//!   `crate::ui::host::supports_long_press`；翻页让给页外的留白与音量键）。
//!
//! 状态挂在一个独立实体上，而不是塞进 [`ReaderSession`]：这一层由阅读页画在最上层
//! （要盖住顶栏底栏与那两块弹层），而 gpui 里非视图实体的 `cx.notify()` 只有订阅方
//! 收得到 —— 阅读页 `cx.observe` 它，于是手势改的是同一个数、重画的是整屏，
//! 形状与 `widgets::BottomSheetState` 一致。
//!
//! 清晰度就是正文那份像素：`ui::shared::images` 把字节缩到长边 2048 才交出去
//! （sprite atlas 的纹理上限，超了手机上静默画空），所以放大倍率的上限按「一个源像素
//! 对一个 CSS 像素」算，不多给一分糊。摊开看的那一张与正文里的是**同一份像素**
//! （[`super::image_atlas`] 按容器路径存着），放大不额外占一格图集。
//!
//! 这一层**画在**哪儿的宽高由 `canvas` 的绘制回调回写（与 `selection_menu.rs` 同一条路）：
//! 推算出来的数与安全区、外壳内缩都有关，量出来的才是准的。回写落在 `Cell` 而不是实体里
//! —— 绘制期改实体是另一码事（见 `paint.rs` 里那条「监听只活到本帧」的纪律）。

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::{
    AnyElement, App, ClickEvent, Entity, InteractiveElement as _, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit, ParentElement, PinchEvent,
    RenderImage, ScrollDelta, ScrollWheelEvent, StatefulInteractiveElement as _, Styled,
    StyledImage, TestSupportExt as _, Window, canvas, div, hsla, img, px,
};

use crate::ui::features::reader::session::ReaderSession;

/// 缩小的下限：`1.0` 就是等比铺满那一格，再小没有意义。
const MIN_SCALE: f32 = 1.0;
/// 固有尺寸未知（SVG、manifest 没写宽高）时给的可放倍数。
///
/// 这时候算不出「一个源像素对一个 CSS 像素」在哪，只能给一个不会让读者撞上硬顶的经验值。
const UNKNOWN_MAX_SCALE: f32 = 3.0;
/// 桌面滚轮一格折算多少 CSS 像素（`ScrollDelta::Lines` 那一种）。
const WHEEL_STEP: f32 = 24.0;
/// `ctrl` + 滚轮一格的放大倍数。触控板的捏合在 X11 上要 compositor 支持，这条是兜底。
const ZOOM_STEP: f32 = 0.15;
/// 按下之后移动多少算「在拖」（CSS 像素）。手指在图上抖一下不该把查看器关掉。
const DRAG_SLOP: f32 = 8.0;

/// 一张图等比缩进 `area` 后的尺寸（居中由摆位那边负责，这里只算大小）。
///
/// 固有尺寸未知（0）时铺满那一格：与定版式页表缺宽高时同一条兜底（`view.rs` 里 `aspect`
/// 那条式子）——猜视口比例总比算不出框强。视口还没量出来时给 0，那一帧不画。
pub fn fit_size(area: (f32, f32), natural: (f32, f32)) -> (f32, f32) {
    let (area_w, area_h) = area;
    if area_w <= 0.0 || area_h <= 0.0 {
        return (0.0, 0.0);
    }
    let (natural_w, natural_h) = natural;
    if natural_w <= 0.0 || natural_h <= 0.0 {
        return (area_w, area_h);
    }
    let k = (area_w / natural_w).min(area_h / natural_h);
    (natural_w * k, natural_h * k)
}

/// 放大倍率的上限：图按**原像素**铺开那一刻（一个源像素对一个 CSS 像素）。
///
/// 再往上就是插值出来的糊，而放大查看的用途正是看清细节 —— 顶在原像素处，读者拖到顶
/// 就停住，比让他一路放到满屏马赛克再自己猜「够了没有」诚实。
/// 适配框不保比例（尺寸未知那条兜底）时按宽这一轴算，那是横向细节的瓶颈。
pub fn max_scale(fit: (f32, f32), natural: (f32, f32)) -> f32 {
    if fit.0 <= 0.0 || natural.0 <= 0.0 || natural.1 <= 0.0 {
        return UNKNOWN_MAX_SCALE;
    }
    (natural.0 / fit.0).max(MIN_SCALE)
}

/// 一轴上的平移夹取：图在这一轴铺得下视口，就不许拖出底色；铺不满，就钉死在中间。
///
/// `room` 因此是「缩放后的尺寸比视口多出来那一半」。夹在两端之间，读者拖到边就停住，
/// 而不是把图推走留下一片黑。
pub fn clamp_pan(pan: f32, scaled: f32, area: f32) -> f32 {
    let room = ((scaled - area) / 2.0).max(0.0);
    pan.clamp(-room, room)
}

/// 一张正被放大了看的图。`ImageViewer::looking` 是 `None` 就是这一层没开。
#[derive(Clone)]
struct Looking {
    /// 正文那张图解好了的**像素**（同一个 `Arc`，见 [`super::image_atlas`]）。
    ///
    /// 这一份是强引用，而它**故意的**：图集那条回收规矩是「只放手里只剩一份引用的格子」
    /// （`image_atlas::end_frame`）。摊开看着的这张图正被每一帧画着，那一层放掉它
    /// 只会让它下一帧自己传回来 —— 而传回来之后没人握句柄，那一格就再也放不掉了。
    /// 关掉这一层、这个 `Arc` 一 drop，图集那边下一帧就把它收走。
    image: Arc<RenderImage>,
    /// 固有像素尺寸，决定放大上限。0 = 不知道。
    natural: (f32, f32),
    scale: f32,
    /// 图心相对 `area` 中心的位移（CSS 像素），已经过 [`clamp_pan`]。
    pan: (f32, f32),
    /// 这一下是**拖**出来的，不是点。松手后紧跟的那一击要吃掉，否则拖完一张图就关掉。
    dragged: bool,
}

/// 阅读页持有的查看器状态。
#[derive(Default)]
pub struct ImageViewer {
    looking: Option<Looking>,
    /// 这一层画出来的宽高，由 [`overlay`] 里那块 `canvas` 每帧回写。
    ///
    /// 开那一层的那一次先拿窗口视口顶上（[`ImageViewer::open`] 的入参），于是第一次
    /// 落位就不差多少；真尺寸那一帧回来再校正。
    area: Rc<Cell<(f32, f32)>>,
    /// 桌面鼠标拖动的起点：按下时的指针位置 + 那一刻的 `pan`。
    drag: Option<((f32, f32), (f32, f32))>,
}

impl ImageViewer {
    pub fn is_open(&self) -> bool {
        self.looking.is_some()
    }

    /// 正在看的那张图的**固有尺寸**；没开着时 `None`。
    ///
    /// 这一格值得单独问得出：排版那一路有没有把尺寸带过来，错了没有任何可见症状 ——
    /// 查看器会静默退到经验上限（[`UNKNOWN_MAX_SCALE`]），读者只看得见「放不大」，
    /// 而那条线索指向的是排版，不是查看器。
    ///
    /// 只由测试问：界面的这一格在屏幕上看不出来，能钉住它的就是这一问。
    #[cfg(test)]
    pub(super) fn seen_natural(&self) -> Option<(f32, f32)> {
        self.looking.as_ref().map(|looking| looking.natural)
    }

    /// 打开看这张图。`viewport` 是发起那一击时的窗口视口，只当第一帧的尺寸用。
    pub fn open(&mut self, image: Arc<RenderImage>, natural: (f32, f32), viewport: (f32, f32)) {
        self.area.set(viewport);
        self.looking = Some(Looking {
            image,
            natural,
            scale: MIN_SCALE,
            pan: (0.0, 0.0),
            dragged: false,
        });
        self.drag = None;
    }

    pub fn close(&mut self) {
        self.looking = None;
        self.drag = None;
    }

    /// 放大 `delta`（[`PinchEvent::delta`] 那一种比例增量：0.1 = 放大一成）。
    ///
    /// 以图的中心为锚，不做「绕手指那一点」的锚点缩放：看图要清的是局部细节，拖一下
    /// 就把中心挪过去了，绕锚点那套多出来的状态值不回这个体验。
    fn zoom_by(&mut self, delta: f32) -> bool {
        // 宽高先取出来再看图：`area` 是个 `Cell`，而下面那一步已经把 `self.looking`
        // 可变借走了，两件事不能同时借 `self`。
        let area = self.area.get();
        let Some(looking) = &mut self.looking else {
            return false;
        };
        let fit = fit_size(area, looking.natural);
        let before = looking.scale;
        let limit = max_scale(fit, looking.natural);
        looking.scale = (before * (1.0 + delta)).clamp(MIN_SCALE, limit);
        looking.pan = clamp_pan_to(looking.pan, fit, looking.scale, area);
        (looking.scale - before).abs() > f32::EPSILON
    }

    /// 平移一段增量（CSS 像素）。手指往下 ⇒ 图也往下 —— 与正文那条 `scroll_y -= dy`
    /// 是同一个方向的两次表达（正文动的是视口，这里动的是图）。
    fn pan_by(&mut self, delta: (f32, f32)) -> bool {
        let area = self.area.get();
        let Some(looking) = &mut self.looking else {
            return false;
        };
        let before = looking.pan;
        let fit = fit_size(area, looking.natural);
        looking.pan = clamp_pan_to(
            (looking.pan.0 + delta.0, looking.pan.1 + delta.1),
            fit,
            looking.scale,
            area,
        );
        (looking.pan.0 - before.0).abs() > 0.5 || (looking.pan.1 - before.1).abs() > 0.5
    }

    /// 按下：记下起点，先不当它是拖。
    fn begin_drag(&mut self, at: (f32, f32)) {
        self.drag = self.looking.as_ref().map(|looking| (at, looking.pan));
    }

    /// 拖到 `at`。过阈值才算「在拖」，那一刻起紧跟的那一击就不再意味着关闭。
    fn move_drag(&mut self, at: (f32, f32)) -> bool {
        let Some((start, start_pan)) = self.drag else {
            return false;
        };
        let delta = (at.0 - start.0, at.1 - start.1);
        if delta.0.hypot(delta.1) < DRAG_SLOP {
            return false;
        }
        let area = self.area.get();
        let Some(looking) = &mut self.looking else {
            return false;
        };
        looking.dragged = true;
        looking.pan = clamp_pan_to(
            (start_pan.0 + delta.0, start_pan.1 + delta.1),
            fit_size(area, looking.natural),
            looking.scale,
            area,
        );
        true
    }

    fn end_drag(&mut self) {
        self.drag = None;
    }

    /// 吃掉「这一下是拖」那面旗，读一次就清：下一次点击属于读者新的动作。
    fn take_dragged(&mut self) -> bool {
        self.looking
            .as_mut()
            .is_some_and(|looking| std::mem::take(&mut looking.dragged))
    }
}

/// 一对位移按适配框与倍率夹回可拖的范围。两轴各夹各的（判据在 [`clamp_pan`]）。
fn clamp_pan_to(pan: (f32, f32), fit: (f32, f32), scale: f32, area: (f32, f32)) -> (f32, f32) {
    (
        clamp_pan(pan.0, fit.0 * scale, area.0),
        clamp_pan(pan.1, fit.1 * scale, area.1),
    )
}

impl ReaderSession {
    /// 挂上阅读页持有的那一个查看器句柄。会话只认这个句柄：状态是同一份，
    /// 而重画的是那一屏（构造时给一次，见 `page::ReadingPage::new`）。
    pub(super) fn set_image_viewer(&mut self, viewer: Entity<ImageViewer>) {
        self.image_viewer = Some(viewer);
    }
}

/// 查看器那一层。没开着时给 `None`，阅读页因此少挂一个子节点。
///
/// `occlude` 是**必须**的，不是修饰：gpui 把一击发给每一个「自己的 hitbox 含有按下点」
/// 的 handler，而正文那层的 hitbox 是整屏的（`page.rs` 里顶栏那条注释记着同一件事）。
/// 不挡住身后，点一下关闭的同时底下那一屏也收到这一下 —— 就变成「关掉查看器并且翻了页」。
/// 连滚动一起收掉也是对的：放大着的时候不该还能把正文滚走。
pub fn overlay(viewer: &Entity<ImageViewer>, cx: &App) -> Option<AnyElement> {
    let read = viewer.read(cx);
    let looking = read.looking.clone()?;
    let area = read.area.get();
    let area_cell = Rc::clone(&read.area);

    let fit = fit_size(area, looking.natural);
    let width = fit.0 * looking.scale;
    let height = fit.1 * looking.scale;
    // 位移在这里按**当帧量出来的**宽高再夹一遍：窗口从横变竖之后，存着的那一份可能
    // 已经越界（越界只在拖动时才被夹回来），而露出黑边是读者一眼就能看见的错。
    let (pan_x, pan_y) = clamp_pan_to(looking.pan, fit, looking.scale, area);
    let x = (area.0 - width) / 2.0 + pan_x;
    let y = (area.1 - height) / 2.0 + pan_y;

    let on_click = {
        let viewer = viewer.clone();
        move |_: &ClickEvent, _window: &mut Window, cx: &mut App| {
            viewer.update(cx, |viewer, cx| {
                // 拖过一张图之后松手，紧跟的那一下点击不是「关掉」。
                if viewer.take_dragged() {
                    return;
                }
                viewer.close();
                cx.notify();
            });
        }
    };
    let on_pinch = {
        let viewer = viewer.clone();
        move |event: &PinchEvent, window: &mut Window, cx: &mut App| {
            if viewer.update(cx, |viewer, _| viewer.zoom_by(event.delta)) {
                window.refresh();
            }
        }
    };
    let on_scroll = {
        let viewer = viewer.clone();
        move |event: &ScrollWheelEvent, window: &mut Window, cx: &mut App| {
            let (dx, dy) = match event.delta {
                ScrollDelta::Pixels(delta) => (f32::from(delta.x), f32::from(delta.y)),
                ScrollDelta::Lines(delta) => (delta.x * WHEEL_STEP, delta.y * WHEEL_STEP),
            };
            let claimed = viewer.update(cx, |viewer, _| {
                if event.modifiers.control || event.modifiers.platform {
                    // 桌面：按住 ctrl（macOS 上是 command）滚轮缩放。触控板捏合要
                    // compositor 报手势事件，这一条总使得上。
                    viewer.zoom_by(if dy > 0.0 { -ZOOM_STEP } else { ZOOM_STEP })
                } else {
                    // 移动端的一次手指平移被翻成 ScrollWheel（模块头那条口径），
                    // 桌面的滚轮走同一条：都是把图往那个方向挪。
                    viewer.pan_by((dx, dy))
                }
            });
            if claimed {
                window.refresh();
            }
        }
    };
    let on_down = {
        let viewer = viewer.clone();
        move |event: &MouseDownEvent, _window: &mut Window, cx: &mut App| {
            viewer.update(cx, |viewer, _| {
                viewer.begin_drag((f32::from(event.position.x), f32::from(event.position.y)));
            });
        }
    };
    let on_move = {
        let viewer = viewer.clone();
        move |event: &MouseMoveEvent, window: &mut Window, cx: &mut App| {
            if event.pressed_button != Some(MouseButton::Left) {
                return;
            }
            let dragged = viewer.update(cx, |viewer, _| {
                viewer.move_drag((f32::from(event.position.x), f32::from(event.position.y)))
            });
            if dragged {
                window.refresh();
            }
        }
    };
    let on_up = {
        let viewer = viewer.clone();
        move |_: &MouseUpEvent, _window: &mut Window, cx: &mut App| {
            viewer.update(cx, |viewer, _| viewer.end_drag());
        }
    };

    Some(
        div()
            .id("image-viewer")
            // 宿主的测试要按 id 把点击投到这里。非 `test-support` 构建里这一层是透明的。
            .test_support()
            .occlude()
            .absolute()
            .inset_0()
            // 黑底而不是主题底色：书里的插图自带白边的比例千差万别，用页面那一档米白
            // 会让「图的边缘在哪」读不出来。
            .bg(hsla(0.0, 0.0, 0.0, 1.0))
            .on_click(on_click)
            .on_pinch(on_pinch)
            .on_scroll_wheel(on_scroll)
            .on_mouse_down(MouseButton::Left, on_down)
            .on_mouse_move(on_move)
            .on_mouse_up(MouseButton::Left, on_up)
            .child(
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(width))
                    .h(px(height))
                    .child(
                        img(looking.image)
                            .size_full()
                            .object_fit(ObjectFit::Contain),
                    ),
            )
            .child(
                // 这一格自己不画任何东西，只回写「这一层真落在哪儿」。窗口变高变矮时
                // 靠它逼出第二帧，让摆位按新尺寸重算。
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        let measured =
                            (f32::from(bounds.size.width), f32::from(bounds.size.height));
                        if area_cell.get() != measured {
                            area_cell.set(measured);
                            window.refresh();
                        }
                    },
                )
                .absolute()
                .inset_0(),
            )
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    //! 三条纯几何各自钉一个临界点。为什么钉这几条：它们错了都不会报错，只会让读者
    //! 看见「图跑到屏幕外」或者「放到第三下忽然黑了」—— 那种问题在真机上最难复现。

    use super::{UNKNOWN_MAX_SCALE, clamp_pan, fit_size, max_scale};

    /// 竖图在横窗口里按**高**顶住，横图反过来：等比，不裁。
    #[test]
    fn fitting_hugs_the_limiting_axis() {
        // 1080×2400 的一页摆在 800×600 的窗口里：高先顶住。
        assert_eq!(fit_size((800.0, 600.0), (1080.0, 2400.0)), (270.0, 600.0));
        // 同一页横过来放：宽先顶住。
        assert_eq!(fit_size((800.0, 600.0), (2400.0, 1080.0)), (800.0, 360.0));
    }

    /// 固有尺寸不知道就铺满那一格；视口还没量出来时不给尺寸，免得算出零框把图藏起来。
    #[test]
    fn unknown_sizes_fill_or_give_nothing() {
        assert_eq!(fit_size((800.0, 600.0), (0.0, 0.0)), (800.0, 600.0));
        assert_eq!(fit_size((0.0, 0.0), (1080.0, 2400.0)), (0.0, 0.0));
    }

    /// 放大上限就是「一个源像素对一个 CSS 像素」那一刻，而且不许低于铺满那一档。
    #[test]
    fn the_zoom_ceiling_is_one_source_pixel_per_css_pixel() {
        // 1080 宽的页铺在 270 宽 ⇒ 还能放 4 倍到原像素。
        assert_eq!(max_scale((270.0, 600.0), (1080.0, 2400.0)), 4.0);
        // 比视口还小的图：铺满已经是放大，不再往上给。
        assert_eq!(max_scale((800.0, 600.0), (600.0, 400.0)), 1.0);
        // 尺寸未知时给一个经验顶，别让读者顶在一个算不出来的数上。
        assert_eq!(max_scale((270.0, 600.0), (0.0, 0.0)), UNKNOWN_MAX_SCALE);
    }

    /// 平移夹取：铺得下就不许拖出底色，铺不满钉在中间（`room` 为 0，两头同值）。
    #[test]
    fn panning_stops_at_the_edges() {
        // 视口 800，图放到 1600：左右各能多拖 400。
        assert_eq!(clamp_pan(500.0, 1600.0, 800.0), 400.0);
        assert_eq!(clamp_pan(-500.0, 1600.0, 800.0), -400.0);
        assert_eq!(clamp_pan(120.0, 1600.0, 800.0), 120.0, "范围里的原样交出");
        // 没放到铺满：任何位移都夹回中间。
        assert_eq!(clamp_pan(120.0, 600.0, 800.0), 0.0);
    }
}

#[cfg(test)]
mod view_tests {
    //! 这一层最容易错在三件看不见的事上：该挂的时候没挂（层根本没出来）、该拦的时候
    //! 没拦（点一下关闭连着把底下那页翻了）、以及「拖一张图」被当成「点一下关闭」。
    //! 三条都在桌面就能测准：`try_find` 看得见元素在不在树里，鼠标与捏合走的是与真机
    //! 同一条 `dispatch_event` 入口。
    //!
    //! **长按那一条不在这儿钉**：`LongPressEvent` 只由触摸手势合成（`gestures.rs` 的
    //! `TouchGestureState::LongPressing`），测试窗口投不出一次「被认领的长按」——
    //! 定版式那一页的入口要上 Android 真按一次才算数。

    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::Arc;

    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt as _;
    // `test_support` 得在这儿再导入一次：内层 `mod` 不继承外层文件的 use。
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Context, Entity, InputEvent as _,
        InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
        MouseUpEvent, ParentElement as _, PinchEvent, Pixels, PlatformInput, Point, Render,
        RenderImage, ScrollDelta, ScrollWheelEvent, StatefulInteractiveElement as _, Styled as _,
        Subscription, TestAppContext, TouchPhase, Window, div, point, px, size,
    };

    use super::{ImageViewer, Looking, overlay};

    /// 视口 400×600，图 800×600：适配框因此是 `400×300`，放大上限 `2.0`（原像素那一档）。
    const VIEWPORT: (f32, f32) = (400.0, 600.0);
    const NATURAL: (f32, f32) = (800.0, 600.0);
    const CENTER: Point<Pixels> = point(px(200.0), px(300.0));

    fn image() -> Arc<RenderImage> {
        // 按 gpui 自己的解码口径造一份像素（BGRA 那一换位是硬要求，见那条函数的说明）。
        crate::ui::shared::images::bgra_render_image(2, 2)
    }

    struct ViewerHost {
        viewer: Entity<ImageViewer>,
        /// 落在**身后那一层**的一击次数：`occlude` 没拦住就会 +1。
        body_taps: Rc<Cell<usize>>,
        /// 订阅必须活着事件才来：`Subscription` 一 drop 就退订。生产上这一格在
        /// `page::ReadingPage::new` 里，测试台照抄同一个形状。
        _watch: Subscription,
    }

    impl ViewerHost {
        fn boot(cx: &mut Context<Self>) -> Self {
            let viewer = cx.new(|_| ImageViewer::default());
            let _watch = cx.observe(&viewer, |_, _, cx| cx.notify());
            Self {
                viewer,
                body_taps: Rc::default(),
                _watch,
            }
        }
    }

    impl Render for ViewerHost {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let taps = self.body_taps.clone();
            div()
                .size_full()
                .relative()
                .child(
                    div()
                        .id("body-underneath")
                        .size_full()
                        .on_click(move |_, _, _| taps.set(taps.get() + 1)),
                )
                .children(overlay(&self.viewer, cx))
        }
    }

    fn boot(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<ViewerHost>) {
        cx.update(gpui_kit::init);
        let mut host = None;
        let handle: AnyWindowHandle = cx
            .open_window(size(px(400.0), px(600.0)), |window, cx| {
                let view = cx.new(ViewerHost::boot);
                host = Some(view.clone());
                Root::new(view, window, cx)
            })
            .into();
        (handle, host.expect("测试台应已建起"))
    }

    /// 画一帧并跑空事件。摆位与 `try_find` 看的都是**已经画出来**的那一帧。
    fn tick(cx: &mut TestAppContext, handle: AnyWindowHandle) {
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("渲染应成功");
    }

    fn open(cx: &mut TestAppContext, host: &Entity<ViewerHost>) {
        cx.update(|app| {
            host.update(app, |this, app| {
                this.viewer.update(app, |viewer, cx| {
                    viewer.open(image(), NATURAL, VIEWPORT);
                    cx.notify();
                });
            });
        });
    }

    fn is_open(cx: &TestAppContext, host: &Entity<ViewerHost>) -> bool {
        host.read_with(cx, |this, cx| this.viewer.read(cx).is_open())
    }

    /// 这一层此刻的倍率与位移。直接读状态：这两条式的错都在「数」上，不在像素上。
    fn scale_and_pan(cx: &TestAppContext, host: &Entity<ViewerHost>) -> (f32, (f32, f32)) {
        host.read_with(cx, |this, cx| {
            this.viewer
                .read(cx)
                .looking
                .as_ref()
                .map(|looking: &Looking| (looking.scale, looking.pan))
                .expect("开着的时候该有一张被看的图")
        })
    }

    fn mounted(cx: &mut TestAppContext, handle: AnyWindowHandle) -> bool {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("image-viewer").is_some()
        })
        .expect("渲染应成功")
    }

    fn body_taps(cx: &TestAppContext, host: &Entity<ViewerHost>) -> usize {
        host.read_with(cx, |this, _| this.body_taps.get())
    }

    fn dispatch(cx: &mut TestAppContext, handle: AnyWindowHandle, event: PlatformInput) {
        cx.update_window(handle, |_, window, cx| window.dispatch_event(event, cx))
            .expect("事件应送达窗口");
        cx.run_until_parked();
    }

    fn click(cx: &mut TestAppContext, handle: AnyWindowHandle, at: Point<Pixels>) {
        dispatch(
            cx,
            handle,
            MouseDownEvent {
                button: MouseButton::Left,
                position: at,
                ..Default::default()
            }
            .to_platform_input(),
        );
        dispatch(
            cx,
            handle,
            MouseUpEvent {
                button: MouseButton::Left,
                position: at,
                ..Default::default()
            }
            .to_platform_input(),
        );
    }

    /// 鼠标拖一段：按下、挪出去、抬起。桌面上的「拖着看」就是这三下。
    fn drag(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        from: Point<Pixels>,
        to: Point<Pixels>,
    ) {
        dispatch(
            cx,
            handle,
            MouseDownEvent {
                button: MouseButton::Left,
                position: from,
                ..Default::default()
            }
            .to_platform_input(),
        );
        dispatch(
            cx,
            handle,
            MouseMoveEvent {
                position: to,
                pressed_button: Some(MouseButton::Left),
                ..Default::default()
            }
            .to_platform_input(),
        );
        dispatch(
            cx,
            handle,
            MouseUpEvent {
                button: MouseButton::Left,
                position: to,
                ..Default::default()
            }
            .to_platform_input(),
        );
    }

    fn pinch(cx: &mut TestAppContext, handle: AnyWindowHandle, delta: f32) {
        dispatch(
            cx,
            handle,
            PinchEvent {
                position: CENTER,
                delta,
                phase: TouchPhase::Moved,
                ..Default::default()
            }
            .to_platform_input(),
        );
    }

    fn wheel(cx: &mut TestAppContext, handle: AnyWindowHandle, delta: gpui_kit::Pixels) {
        dispatch(
            cx,
            handle,
            ScrollWheelEvent {
                position: CENTER,
                delta: ScrollDelta::Pixels(point(px(0.0), delta)),
                touch_phase: TouchPhase::Moved,
                ..Default::default()
            }
            .to_platform_input(),
        );
    }

    /// 回归门：没图就不挂层，开了才挂，关掉又收干净 —— 一层都不留在树里。
    #[gpui_kit::test]
    fn the_layer_mounts_only_while_a_picture_is_being_seen(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        tick(cx, handle);
        assert!(!mounted(cx, handle), "没在看图的时候一层都不该挂");

        open(cx, &host);
        tick(cx, handle);
        assert!(mounted(cx, handle), "开着就该在树里");

        cx.update(|app| {
            host.update(app, |this, app| {
                this.viewer.update(app, |viewer, cx| {
                    viewer.close();
                    cx.notify();
                });
            });
        });
        tick(cx, handle);
        assert!(!mounted(cx, handle), "关掉之后不该留一层黑的在树上");
    }

    /// 点一下空白关掉这一层，**而身后那一层一个字都不许收到**。
    ///
    /// 这条钉的是 `occlude`：gpui 把一击发给每一个 hitbox 含按下点的 handler，正文那层
    /// 是整屏的，少了这一格就是「关掉查看器的同时翻了页」。
    #[gpui_kit::test]
    fn a_click_on_the_layer_closes_it_and_nothing_reaches_the_body_below(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        open(cx, &host);
        tick(cx, handle);

        click(cx, handle, CENTER);
        tick(cx, handle);
        assert!(!is_open(cx, &host), "点空白该关掉这一层");
        assert_eq!(
            body_taps(cx, &host),
            0,
            "那一击属于查看器，不该同时落到正文的整屏 hitbox 上"
        );
    }

    /// 拖一张放大了的图，松手那一下不是「关掉」。
    #[gpui_kit::test]
    fn dragging_the_picture_is_not_a_click(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        open(cx, &host);
        tick(cx, handle);
        pinch(cx, handle, 1.0);

        drag(
            cx,
            handle,
            point(px(150.0), px(300.0)),
            point(px(260.0), px(300.0)),
        );
        tick(cx, handle);
        assert!(is_open(cx, &host), "拖过一张图不该把这一层关掉");
        assert_eq!(body_taps(cx, &host), 0);
        let (scale, pan) = scale_and_pan(cx, &host);
        assert_eq!(
            (scale, pan.0.round() as i32),
            (2.0, 110),
            "拖动要把图挪走那一段"
        );
    }

    /// 捏合的两个顶：放大顶在**原像素**那一档，缩小顶在铺满那一档。
    ///
    /// 越过原像素只是插值出来的糊，而低于铺满意味着图缩到比屏还小 —— 两头都不该给。
    #[gpui_kit::test]
    fn pinching_stops_at_the_source_pixels_on_both_ends(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        open(cx, &host);
        tick(cx, handle);
        assert_eq!(scale_and_pan(cx, &host).0, 1.0, "起手就是铺满那一档");

        pinch(cx, handle, 1.0);
        assert_eq!(
            scale_and_pan(cx, &host).0,
            2.0,
            "800 宽的图铺在 400 ⇒ 顶在 2 倍"
        );
        pinch(cx, handle, 1.0);
        assert_eq!(scale_and_pan(cx, &host).0, 2.0, "顶到了就不许再往上");
        pinch(cx, handle, -1.0);
        assert_eq!(scale_and_pan(cx, &host).0, 1.0, "最小就是铺满那一档");
    }

    /// 平移是按轴夹的：这一张图放大后**横向**多出 400 像素可拖，纵向刚好铺满 ⇒ 拖不动。
    /// 而没放大之前两轴都铺不满，滚轮一个字都不该挪。
    #[gpui_kit::test]
    fn panning_is_clamped_axis_by_axis(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        open(cx, &host);
        tick(cx, handle);
        wheel(cx, handle, px(-100.0));
        assert_eq!(
            scale_and_pan(cx, &host).1,
            (0.0, 0.0),
            "铺满那一档没有可拖的余地"
        );

        pinch(cx, handle, 1.0);
        // 纵向：放大后 600 高，视口也 600 高 ⇒ 钉在中间。
        wheel(cx, handle, px(-100.0));
        assert_eq!(
            scale_and_pan(cx, &host).1,
            (0.0, 0.0),
            "纵向铺得下的轴拖不动"
        );
        // 横向：800 对 400 ⇒ 每边可拖 200。
        dispatch(
            cx,
            handle,
            ScrollWheelEvent {
                position: CENTER,
                delta: ScrollDelta::Pixels(point(px(-150.0), px(0.0))),
                touch_phase: TouchPhase::Moved,
                ..Default::default()
            }
            .to_platform_input(),
        );
        assert_eq!(scale_and_pan(cx, &host).1, (-150.0, 0.0));
        dispatch(
            cx,
            handle,
            ScrollWheelEvent {
                position: CENTER,
                delta: ScrollDelta::Pixels(point(px(-150.0), px(0.0))),
                touch_phase: TouchPhase::Moved,
                ..Default::default()
            }
            .to_platform_input(),
        );
        assert_eq!(
            scale_and_pan(cx, &host).1,
            (-200.0, 0.0),
            "拖到边就停住，别把图推出去露黑边"
        );
    }
}
