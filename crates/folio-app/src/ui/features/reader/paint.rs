//! 字形级绘制元素：一条背景矩形 + 若干条「已定好基线」的 run。
//!
//! 正文需要字形级控制（选择高亮、ruby、竖排），而这份 GPUI 快照 **没有 `paint_text`**，
//! 所以正文自己写 `Element`，逐字形 `paint_glyph`。两端同一份：以前只有移动走这条路，
//! 桌面交给 `TextView` + 虚拟列表，于是同一本书在两台上可能各排各的行。
//!
//! 落点算法照抄 gpui 自己的 `paint_line`（`text_system/line.rs:452` 起）：
//! 它把字形画在「行框原点 + (0, padding_top + ascent) + glyph.position」，
//! 前两项合起来就是这一行的**基线**，第三项是 shaping 自带的纵向偏移。
//! 基线在这里由排版层直接给出（`LineBox::y + LineBox::baseline`），
//! 于是偏移量与 `MobileMeasure::run_metrics` 量到的 ascent/descent 同源，
//! 不会出现「按 A 算的行高、按 B 画的字形」。

use gpui_kit::{
    App, Bounds, Element, ElementId, Font, GlobalElementId, Hsla, InspectorElementId, IntoElement,
    LayoutId, Length, LongPressEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, Point, SharedString, StrikethroughStyle, Style, TextRun, TouchDragEvent, TouchPhase,
    UnderlineStyle, WeakEntity, Window, black, fill, point, px, relative, size,
};

use super::session::{ReaderSession, SelectionEdge};

/// 一条已经定好位置的 run，等于 `folio_core::layout::LineRun` 的绘制侧投影。
/// `x` / `baseline` 相对本元素原点（行框左上角，未滚动时就是版心左上角）。
pub struct GlyphRun {
    pub text: SharedString,
    pub font: Font,
    pub font_size: f32,
    pub color: Hsla,
    pub x: f32,
    pub baseline: f32,
    /// `text-decoration` 的两条线。gpui 的 shaping 直接支持，所以这里只是把
    /// CSS 算出来的布尔传到 `TextRun` 上；线宽 1px、颜色跟字，与桌面端 `.underline()`
    /// 那条样式方法的默认值一致 —— 两端各定一个数的话，同一本书在两处会画得不一样。
    pub underline: bool,
    pub line_through: bool,
}

impl GlyphRun {
    /// 文本收 `SharedString` 而不是 `impl Into<SharedString>`：`From<&str>` 只覆盖
    /// `&'static str`，而行文本借自 `BlockBox::text`（活不过一帧），
    /// 在这里泛型化会把调用点逼成 `'static`。构造点自己 `SharedString::new(...)`。
    pub fn new(text: SharedString, font: Font, font_size: f32) -> Self {
        Self {
            text,
            font,
            font_size,
            color: black(),
            x: 0.0,
            baseline: font_size,
            underline: false,
            line_through: false,
        }
    }

    pub fn at(mut self, x: f32, baseline: f32) -> Self {
        self.x = x;
        self.baseline = baseline;
        self
    }

    pub fn color(mut self, color: Hsla) -> Self {
        self.color = color;
        self
    }

    /// 装饰线。两个布尔直接来自 `RunStyle`，不在这层做取舍。
    pub fn decorate(mut self, underline: bool, line_through: bool) -> Self {
        self.underline = underline;
        self.line_through = line_through;
        self
    }
}

/// 背景矩形 + 一批 run。视口内的可见行由调用方挑好再传进来
/// （没有组件层的虚拟列表，裁剪就在这儿做）。
///
/// 不带页面背景：那层底色归视图的容器 `div` —— 字形元素铺满版心，若由它背底色，
/// 它相对图片晚入树就会被 gpui 抬到图片之上，把封面那类整章的图盖成白页
/// （见 `view.rs` 绘制层级的那段注释）。
pub struct GlyphsElement {
    runs: Vec<GlyphRun>,
    /// 批注底色（M7 划线）。一块矩形一个颜色，画在选中高亮**之下**、字形之下。
    ///
    /// 与选中高亮分开两份：前者是读者留在书上的东西，一屏里同时可以有几十块；
    /// 后者是这一次挑中的那一段，还要长出把手。
    fills: Vec<(Bounds<Pixels>, Hsla)>,
    /// 选中件（M6）。空的高亮 = 这一屏没有选中。
    selection: SelectionVisual,
    /// 会话的弱引用：paint 时回写内容边界与把手命中区、注册手势监听。
    /// `None` = 兜底那一行没有书，不参与选中。
    reader: Option<WeakEntity<ReaderSession>>,
}

/// 一屏里选中的全部绘制信息，元素局部坐标（逻辑像素）。
///
/// 由 `view.rs` 按「排版矩形 → 屏幕矩形」换算好整批传入；绘制与命中都不再碰排版层。
#[derive(Default)]
pub struct SelectionVisual {
    /// 每行一条的高亮矩形，压在字形之下。
    pub highlights: Vec<Bounds<Pixels>>,
    /// 两端把手的插入符盒（0 宽、一行高，长在首个/末个选中字符的字缘上）。
    /// 那一端被裁出可视范围时不给 —— 把手跟着它标记的字消失，与 gpui-kit 一致。
    pub start_caret: Option<Bounds<Pixels>>,
    pub end_caret: Option<Bounds<Pixels>>,
    /// 高亮与把手的颜色（主题的 selection 色）。
    pub color: Hsla,
}

/// 把手的几何常量（逻辑像素），照 gpui-kit `touch_selection` 的那套：
/// 圆钮 10、杆 2、命中区 44（拇指稳定命中的下限），命中区往圆钮那一侧多伸 12。
const KNOB_SIZE: f32 = 10.0;
const BAR_WIDTH: f32 = 2.0;
const HIT_SIZE: f32 = 44.0;
const HIT_EXTENT: f32 = 12.0;

/// 把手的命中区：竖条围着插入符，往圆钮那一侧多伸一截（起点的圆钮在行上、
/// 终点的在行下，与绘制一侧同一套规矩）。
fn handle_hit(edge: SelectionEdge, caret: &Bounds<Pixels>) -> Bounds<Pixels> {
    let top = match edge {
        SelectionEdge::Start => caret.top() - px(HIT_EXTENT),
        SelectionEdge::End => caret.top(),
    };
    Bounds::new(
        point(caret.left() - px(HIT_SIZE / 2.0), top),
        size(px(HIT_SIZE), caret.size.height + px(HIT_EXTENT)),
    )
}

/// 画一个把手：杆沿插入符立起来，圆钮长在行外（起点在上、终点在下）。
fn paint_handle(edge: SelectionEdge, caret: Bounds<Pixels>, color: Hsla, window: &mut Window) {
    let bar = Bounds::new(
        point(caret.left() - px(BAR_WIDTH / 2.0), caret.top()),
        size(px(BAR_WIDTH), caret.size.height),
    );
    let knob_top = match edge {
        SelectionEdge::Start => caret.top() - px(KNOB_SIZE),
        SelectionEdge::End => caret.bottom(),
    };
    let knob = Bounds::new(
        point(caret.left() - px(KNOB_SIZE / 2.0), knob_top),
        size(px(KNOB_SIZE), px(KNOB_SIZE)),
    );
    window.paint_quad(fill(bar, color));
    window.paint_quad(fill(knob, color).corner_radii(px(KNOB_SIZE / 2.0)));
}

impl Element for GlyphsElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        // 铺满父容器：阅读栏就是整个窗口，滚动偏移在 paint 里做，不参与布局。
        let style = Style {
            size: size(
                Length::Definite(relative(1.)),
                Length::Definite(relative(1.)),
            ),
            ..Default::default()
        };
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        // 绘制层级（自下而上）：块级装饰（更早入树的兄弟）→ 选中高亮 → 字形 → 把手。
        // 高亮必须在字形之前画，否则选中的字会被自己的底色盖住。
        // 传进来的高亮是元素局部坐标，跟字形与把手一样要加上元素在窗口里的原点：
        // 阅读视图不在窗口原点（Android 的系统栏 inset、桌面的侧栏）时，漏加这一个
        // 偏移，高亮就会整块飘到文字上方 —— 把手是准的、颜色是偏的，就是它。
        for (rect, color) in &self.fills {
            window.paint_quad(fill(*rect + bounds.origin, *color));
        }
        for rect in &self.selection.highlights {
            window.paint_quad(fill(*rect + bounds.origin, self.selection.color));
        }
        if let Some(reader) = self.reader.clone() {
            register_selection(&self.selection, &reader, bounds, window, cx);
        }
        for run in &self.runs {
            paint_run(window, bounds.origin, run);
        }
        if let Some(caret) = self.selection.start_caret {
            paint_handle(
                SelectionEdge::Start,
                caret + bounds.origin,
                self.selection.color,
                window,
            );
        }
        if let Some(caret) = self.selection.end_caret {
            paint_handle(
                SelectionEdge::End,
                caret + bounds.origin,
                self.selection.color,
                window,
            );
        }
    }
}

impl IntoElement for GlyphsElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl GlyphsElement {
    pub fn new() -> Self {
        Self {
            runs: Vec::new(),
            fills: Vec::new(),
            selection: SelectionVisual::default(),
            reader: None,
        }
    }

    /// 添一块批注底色。坐标与 `runs` 同一套：元素局部，paint 里统一加元素原点。
    pub fn fill(&mut self, bounds: Bounds<Pixels>, color: Hsla) {
        self.fills.push((bounds, color));
    }

    /// 这一屏的选中件。没有选中时传默认值即可（空高亮、无把手）。
    pub fn selection(mut self, selection: SelectionVisual) -> Self {
        self.selection = selection;
        self
    }

    /// 挂上会话：paint 时回写几何、注册手势。没有书（兜底那一行）不挂。
    pub fn reader(mut self, reader: Option<WeakEntity<ReaderSession>>) -> Self {
        self.reader = reader;
        self
    }

    pub fn push(&mut self, run: GlyphRun) {
        self.runs.push(run);
    }

    pub fn extend(&mut self, runs: impl IntoIterator<Item = GlyphRun>) {
        self.runs.extend(runs);
    }
}

impl Default for GlyphsElement {
    fn default() -> Self {
        Self::new()
    }
}

/// 把一条 run 画出来：先让窗口级文本系统 shaping，再逐字形 `paint_glyph`。
fn paint_run(window: &mut Window, origin: Point<Pixels>, run: &GlyphRun) {
    if run.text.is_empty() {
        return;
    }
    let font_size = px(run.font_size);
    let text_run = TextRun {
        len: run.text.len(),
        font: run.font.clone(),
        color: run.color,
        background_color: None,
        // 1px、按字色，与桌面端 gpui 的 `.underline()` / `.line_through()` 默认值同源。
        underline: run.underline.then_some(UnderlineStyle {
            thickness: px(1.),
            color: Some(run.color),
            wavy: false,
        }),
        strikethrough: run.line_through.then_some(StrikethroughStyle {
            thickness: px(1.),
            color: Some(run.color),
        }),
    };
    let shaped = window
        .text_system()
        .shape_line(run.text.clone(), font_size, &[text_run], None);

    let baseline_x = origin.x + px(run.x);
    let baseline_y = origin.y + px(run.baseline);
    for shaped_run in &shaped.runs {
        for glyph in &shaped_run.glyphs {
            let at = point(baseline_x + glyph.position.x, baseline_y + glyph.position.y);
            // 单个字形画不出去（缺字形、图集满）不该让整帧失败：与 gpui 自己的
            // `paint_line` 一致，忽略即可。
            let _ = window.paint_glyph(at, shaped_run.font_id, glyph.id, font_size, run.color);
        }
    }
}

/// 把这一帧的选中几何交给会话（内容边界、把手命中区），并注册手势监听。
///
/// `window.on_mouse_event` 注册的监听只活到本帧事件分发结束，所以每个绘制帧
/// 都要重新挂一遍 —— 这与 gpui-kit 的 `TextSelectionLayer` 同一条规矩。监听收到
/// 的是窗口里**所有**同类事件，落到会话的方法里再按范围与状态过滤。
fn register_selection(
    visual: &SelectionVisual,
    reader: &WeakEntity<ReaderSession>,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(entity) = reader.upgrade() else {
        return;
    };
    let handle_hits = {
        let session = entity.read(cx);
        session.content_bounds.set(bounds);
        session.handle_hits.clone()
    };
    // 插入符盒换算成窗口坐标：事件侧（modules/selection.rs）只认这一份。
    *handle_hits.borrow_mut() = [
        (SelectionEdge::Start, visual.start_caret),
        (SelectionEdge::End, visual.end_caret),
    ]
    .into_iter()
    .flat_map(|(edge, caret)| caret.map(|caret| (edge, handle_hit(edge, &caret) + bounds.origin)))
    .collect();

    // 指针按下（capture）：新的一手开始。上一手攒下的 click 豁免在这里作废——
    // 正常它已被拖选松手的那一下 click 消费掉，没消费说明那一下没落在正文上。
    let down_reader = reader.clone();
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        if !phase.capture() || event.button != MouseButton::Left {
            return;
        }
        let _ = down_reader.update(cx, |reader, cx| {
            reader.suppress_tap = false;
            if reader.content_bounds.get().contains(&event.position) {
                reader.begin_mouse_selection(window, cx, event.position);
            }
        });
    });

    // 指针拖动（bubble）：把手拖动优先，否则过了阈值进入拖选。
    let move_reader = reader.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
        if !phase.bubble() || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let _ = move_reader.update(cx, |reader, cx| {
            reader.drag_mouse_selection(window, cx, event.position)
        });
    });

    // 指针抬起（bubble）：拖选成过就拦掉紧跟着的 click。不按落点过滤——
    // 拖出正文松手也要把拖动状态收掉。
    let up_reader = reader.clone();
    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
        if !phase.bubble() || event.button != MouseButton::Left {
            return;
        }
        let _ = up_reader.update(cx, |reader, cx| reader.end_mouse_selection(cx));
    });

    // 长按（触摸）：500ms 后由手势识别器送来。Started 落在字上才抢占这一手势，
    // 抢占后手指的移动/抬起都从这里走（后续选区绕词锚点扩展）。
    let long_press_reader = reader.clone();
    window.on_mouse_event(move |event: &LongPressEvent, phase, window, cx| {
        if !phase.bubble() {
            return;
        }
        let Some(entity) = long_press_reader.upgrade() else {
            return;
        };
        match event.phase {
            TouchPhase::Started => {
                let began = entity.update(cx, |reader, cx| {
                    reader.begin_long_press(window, cx, event.start_position)
                });
                if !began {
                    return;
                }
                window.capture_long_press(&entity);
                window.prevent_default();
                cx.stop_propagation();
            }
            _ if window.has_long_press_capture(&entity) => {
                entity.update(cx, |reader, cx| match event.phase {
                    TouchPhase::Moved => reader.drag_long_press(window, cx, event.position),
                    TouchPhase::Ended | TouchPhase::Cancelled => reader.end_long_press(cx),
                    TouchPhase::Started => {}
                });
                window.prevent_default();
                cx.stop_propagation();
            }
            _ => {}
        }
    });

    // 触摸拖动：落在把手上才认领（prevent_default 把它从「滚动」里抢过来），
    // 认领后的移动/抬起归把手拖动；落在别处什么都不做，滚动照常。
    let drag_reader = reader.clone();
    window.on_mouse_event(move |event: &TouchDragEvent, phase, window, cx| {
        if !phase.bubble() {
            return;
        }
        let Some(entity) = drag_reader.upgrade() else {
            return;
        };
        match event.phase {
            TouchPhase::Started => {
                let edge = entity.read_with(cx, |reader, _| {
                    reader
                        .content_bounds
                        .get()
                        .contains(&event.position)
                        .then(|| reader.handle_at(event.position))
                        .flatten()
                });
                let Some(edge) = edge else {
                    return;
                };
                entity.update(cx, |reader, _| reader.begin_edge_drag(edge, event.position));
                window.prevent_default();
                cx.stop_propagation();
            }
            _ => {
                let dragging = entity.read_with(cx, |reader, _| reader.edge_drag.is_some());
                if !dragging {
                    return;
                }
                cx.stop_propagation();
                entity.update(cx, |reader, cx| match event.phase {
                    TouchPhase::Moved => reader.update_edge_drag(window, cx, event.position),
                    TouchPhase::Ended | TouchPhase::Cancelled => reader.end_edge_drag(cx),
                    TouchPhase::Started => {}
                });
            }
        }
    });
}
