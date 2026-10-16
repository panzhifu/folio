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

use std::sync::Arc;

use folio_core::infra::metrics::Metric;
use folio_core::layout::{Justify, JustifyMode};
use gpui_kit::{
    App, Bounds, Element, ElementId, Font, GlobalElementId, Hsla, InspectorElementId, IntoElement,
    LayoutId, Length, LineLayout, LongPressEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, SharedString, StrikethroughStyle, Style, TextRun, TouchDragEvent,
    TouchPhase, UnderlineStyle, WeakEntity, Window, black, fill, point, px, relative, size,
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
    /// 这个 run 的 inline box 高（`font_size × line-height`），只用来定装饰线的纵向落点。
    ///
    /// 为什么要单独带一份：gpui 的 `paint_line` 算基线在盒里的位置、算删除线的高度都要
    /// 这个数（见 [`decoration_y`]），而 `ShapedLine` 里没有它 —— 它是**读者与样式**给的，
    /// 不是 shaping 的结果。缺了它就只能自己猜一个比例，那正是「按 A 量、按 B 画」的起手式。
    pub box_height: f32,
    /// 这一条 run 在**章内**的身份：`(行号, 行内片段槽位)`，由 `scene::Scene::build` 摊
    /// run 时按 enumerate 盖上。`None` = 兜底那一行或测试里手搓的 run，不参与 shaping 表。
    pub key: Option<(u32, u32)>,
    /// 已经 shape 好的那一份几何。视图层在 `Scene::build` 之后按 [`Self::key`] 查
    /// [`Document::shaped_line`](super::document::Document::shaped_line) 填进来；
    /// `None` 时 [`paint_run`] 当场 shape（走 gpui 那两帧缓存）。
    ///
    /// 存 `Arc<LineLayout>` 而不是 `ShapedLine`：后者的 `decoration_runs` 里带主题色，
    /// 而我们从不读它（装饰线是这一层自己画的）。`LineLayout` 里只有 `font_id`、字形与
    /// 落点，**结构上就不可能把颜色烘进表**。
    pub shaped: Option<Arc<LineLayout>>,
    /// 字距（px）。绘制按字形所在字符序号右推 `letter_spacing × 序号`。
    pub letter_spacing: f32,
    /// 两端对齐的摊法 + 这一段起点已累计的右推量（px）。绘制在 run 内按局部隙序号再推一笔。
    /// 与 `hit`/选区同一句规则（见 `folio_core::layout::Justify`）。
    pub justify: Option<Justify>,
    pub justify_shift: f32,
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
            // 默认按字号一档（UA 的 `line-height: normal` 近似）。真排版会用
            // `RunStyle::box_height()` 覆盖它，见 [`GlyphRun::in_box`]。
            box_height: font_size,
            key: None,
            shaped: None,
            letter_spacing: 0.0,
            justify: None,
            justify_shift: 0.0,
        }
    }

    /// 字距 + 两端对齐：`paint_run` 摊字形时要的那两笔偏移。场景层从 `RowUnit` 递过来。
    pub fn spacing(
        mut self,
        letter_spacing: f32,
        justify: Option<Justify>,
        justify_shift: f32,
    ) -> Self {
        self.letter_spacing = letter_spacing;
        self.justify = justify;
        self.justify_shift = justify_shift;
        self
    }

    /// 这个 run 的 inline box 高。场景层从 `RunStyle::box_height()` 递过来。
    pub fn in_box(mut self, box_height: f32) -> Self {
        self.box_height = box_height;
        self
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

    /// 盖上章内身份，让 shaping 表认得它（见 [`GlyphRun::key`]）。
    pub fn keyed(mut self, key: (u32, u32)) -> Self {
        self.key = Some(key);
        self
    }

    /// 一个字形该右推多少（px）。`char_index` 是它在**阅读序**里之前有几个字符，
    /// `spaces_before` 是之前的空格数 —— 字距按前者，两端对齐按摊法取前者或后者。
    /// 这一句与 `folio_core` 的 `Justify`、以及 `hit`/选区逐字一致。
    fn glyph_shift(&self, char_index: u32, spaces_before: u32) -> f32 {
        let gap = match self.justify {
            Some(j) => match j.mode {
                JustifyMode::Chars => char_index,
                JustifyMode::Spaces => spaces_before,
            },
            None => 0,
        };
        self.justify_shift
            + self.justify.map_or(0.0, |j| j.extra * gap as f32)
            + self.letter_spacing * char_index as f32
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
    /// 批注下划线（M7 划线）。一条细矩形一个颜色，画在选中高亮**之下**、字形之下。
    ///
    /// 与选中高亮分开两份：前者是读者留在书上的东西，一屏里同时可以有几十条；
    /// 后者是这一次挑中的那一段，还要长出把手。
    underlines: Vec<(Bounds<Pixels>, Hsla)>,
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
        let __paint = super::perf::Timer::start();
        // 绘制层级（自下而上）：块级装饰（更早入树的兄弟）→ 批注下划线 → 选中高亮 →
        // 字形 → 把手。下划线与高亮都在字形之前画，字形之后再补就把字盖住了。
        // 传进来的矩形是元素局部坐标，跟字形与把手一样要加上元素在窗口里的原点：
        // 阅读视图不在窗口原点（Android 的系统栏 inset、桌面的侧栏）时，漏加这一个
        // 偏移，下划线就会整条飘到文字上方 —— 把手是准的、颜色是偏的，就是它。
        for (rect, color) in &self.underlines {
            window.paint_quad(fill(*rect + bounds.origin, *color));
        }
        for rect in &self.selection.highlights {
            window.paint_quad(fill(*rect + bounds.origin, self.selection.color));
        }
        if let Some(reader) = self.reader.clone() {
            register_selection(&self.selection, &reader, bounds, window, cx);
        }
        // 字形整屏共用**一个 layer**：gpui 的 `Scene::insert_primitive` 在没有活动 layer 时，
        // 每个 primitive 都要往 BoundsTree 里插一次（`scene.rs` 的 `push_layer` 分支），
        // 一屏上千字形就是上千次 R 树插入 —— 这一笔与「画了多少字」同量级，是滚动帧里
        // 最容易被忽略的固定开销。zed 自己的 `paint_line` 同样把整行字形包进一个 layer
        // （`text_system/line.rs:547` 的 `window.paint_layer(line_bounds, …)`）。
        //
        // 层级不变：layer 只吃 order（入 scene 的顺序仍在），所以高亮仍压在字下、把手仍
        // 压在字上 —— 它们是在这个 layer 之外画的。
        let __shape_ns = std::cell::Cell::new(0u64);
        let mut __glyphs = 0u32;
        window.paint_layer(bounds, |window| {
            for run in &self.runs {
                let __t = super::perf::Timer::start();
                __glyphs += paint_run(window, bounds.origin, run);
                __shape_ns.set(__shape_ns.get() + __t.ns());
            }
        });
        super::perf::log_paint(__paint.ns(), __shape_ns.get(), __glyphs);
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
            underlines: Vec::new(),
            selection: SelectionVisual::default(),
            reader: None,
        }
    }

    /// 添一条批注下划线。`bounds` 已是那条线本身（细矩形，高由调用方给），坐标与 `runs`
    /// 同一套：元素局部，paint 里统一加元素原点。
    pub fn underline(&mut self, bounds: Bounds<Pixels>, color: Hsla) {
        self.underlines.push((bounds, color));
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

/// 两条装饰线的 y，全按 gpui `paint_line` 的式子来，不自定比例。
///
/// 输入是「这一 run 的基线 y、inline box 高、shaping 报的 ascent/descent」。gpui 里
/// 盒顶在 `origin.y`、基线落在 `盒顶 + padding_top + ascent`，其中
/// `padding_top = (line_height − ascent − descent) / 2`；下划线再往下一条
/// `descent × 0.618`（gpui 的 `UNDERLINE_DESCENT_OFFSET_FACTOR`，它把那个数包在公开函数
/// [`gpui::underline_y_offset`] 里），删除线落在 `盒顶 + (ascent/2 + padding_top + ascent) / 2`。
/// 自己猜一个「基线下 2px」那种数，换字号就会露出：线与字之间的缝会跟着字号涨缩，
/// 而 gpui 自己画的那条不会 —— 两条线就会在不同字号的段落里对不齐。
fn decoration_y(
    baseline: Pixels,
    box_height: Pixels,
    ascent: Pixels,
    descent: Pixels,
) -> (Pixels, Pixels) {
    let padding_top = (box_height - ascent - descent) / 2.;
    let baseline_offset = padding_top + ascent;
    let top = baseline - baseline_offset;
    let underline = top + baseline_offset + descent * 0.618;
    let strikethrough = top + (ascent * 0.5 + baseline_offset) * 0.5;
    (underline, strikethrough)
}

/// shaping 的**唯一入口**：一条 run → 一份几何。
///
/// 走 `layout_line` 而不是 `shape_line`：后者要多造一份 `decoration_runs`（颜色、下划线、
/// 删除线各一格），而我们从不读它 —— 装饰线是 [`paint_run`] 自己画的（`plans/阅读管线.md`
/// §十四 记的就是这件事）。`layout_line` 直接给回 `Arc<LineLayout>`，也正是
/// [`Document::shaped_line`](super::document::Document::shaped_line) 那张表要存的值。
///
/// `Metric::ShapeLineCalls` 数的是**这一趟**，所以它的口径是「shaping 表未命中一次」，
/// 不再是「每条 run 一次」—— 表接上之后，滚回走过的地方这一格增量应当是 0。
pub(super) fn shape(window: &Window, run: &GlyphRun) -> Arc<LineLayout> {
    // gpui 的 `LineLayoutCache` 命中时确实不重 shaping，但它的键含整段文本 ——
    // **命中也要把文本哈希一遍**，而且只保两帧。这一格数的是走到 gpui 那层的次数。
    Metric::ShapeLineCalls.count();
    let __t_shape = super::perf::Timer::start();
    let text_run = TextRun {
        len: run.text.len(),
        font: run.font.clone(),
        color: run.color,
        background_color: None,
        // 1px、按字色，与桌面端 gpui 的 `.underline()` / `.line_through()` 默认值同源。
        // 单条 run 进 `layout_line` 时这两格只参与「要不要切 font run」的判断，而只有一条
        // 就无从切起 —— 它们不影响回来的几何，留着是为了哪天传多条 run 时口径不漂。
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
    let layout =
        window
            .text_system()
            .layout_line(run.text.as_ref(), px(run.font_size), &[text_run], None);
    super::perf::add(|frame| frame.shape_only_ns += __t_shape.ns());
    layout
}

/// 把一条 run 画出来：几何取自 shaping 表（没有就当场 shape 一次），再逐字形 `paint_glyph`。
fn paint_run(window: &mut Window, origin: Point<Pixels>, run: &GlyphRun) -> u32 {
    if run.text.is_empty() {
        return 0;
    }
    let font_size = px(run.font_size);
    let shaped = match &run.shaped {
        Some(layout) => layout.clone(),
        None => shape(window, run),
    };

    let baseline_x = origin.x + px(run.x);
    let baseline_y = origin.y + px(run.baseline);
    let mut glyphs = 0u32;
    // 字距 / 两端对齐的右推：按字形在**阅读序**里的字符序号逐字累加。LTR 下字形的
    // `index` 在视觉序里递增，走一遍即可；RTL 下它在视觉序里递减（一期不做字距/对齐的
    // RTL 精确分布），跳过右推即可 —— 与 `hit`/选区的口径一致。
    let first_two: Vec<usize> = shaped
        .runs
        .iter()
        .flat_map(|r| r.glyphs.iter())
        .take(2)
        .map(|g| g.index)
        .collect();
    let rtl = first_two.len() == 2 && first_two[1] < first_two[0];
    let mut char_index = 0u32;
    let mut spaces_before = 0u32;
    let mut prev_index = 0usize;
    let __t_glyph = super::perf::Timer::start();
    for shaped_run in &shaped.runs {
        for glyph in &shaped_run.glyphs {
            glyphs += 1;
            let shift = if rtl {
                0.0
            } else {
                if glyph.index >= prev_index
                    && let Some(between) = run.text.get(prev_index..glyph.index)
                {
                    for ch in between.chars() {
                        char_index += 1;
                        if ch == ' ' {
                            spaces_before += 1;
                        }
                    }
                    prev_index = glyph.index;
                }
                run.glyph_shift(char_index, spaces_before)
            };
            let at = point(
                baseline_x + glyph.position.x + px(shift),
                baseline_y + glyph.position.y,
            );
            // 单个字形画不出去（缺字形、图集满）不该让整帧失败：与 gpui 自己的
            // `paint_line` 一致，忽略即可。
            let _ = window.paint_glyph(at, shaped_run.font_id, glyph.id, font_size, run.color);
        }
    }
    super::perf::add(|frame| frame.glyph_ns += __t_glyph.ns());
    // 装饰线。字形是我们自己一个个画的，gpui 的 `ShapedLine::paint` 里没有那一段，
    // 所以线也得自己画 —— 少这一段就是「`text-decoration` 解析了、传进 shaping 了、
    // 然后没人画」：`UnderlineStyle` 造出来只为了进 `TextRun`，画面的循环只读 glyphs。
    //
    // 一条 run 一种样式（样式来自 `RunStyle`，整段一致），所以整条 run 就是一个装饰段，
    // 不必照 gpui 那样按 `decoration_runs` 切字符区间。宽度用 `shaped.width`（**画出来的**
    // 那份宽），不用布局给的 run 宽：后者含 `word-spacing` 一类的富余，线会伸到字后面去。
    if run.underline || run.line_through {
        let (underline_y, strikethrough_y) = decoration_y(
            baseline_y,
            px(run.box_height),
            shaped.ascent,
            shaped.descent,
        );
        let width = shaped.width;
        if run.underline {
            window.paint_underline(
                point(baseline_x, underline_y),
                width,
                &UnderlineStyle {
                    thickness: px(1.),
                    color: Some(run.color),
                    wavy: false,
                },
            );
        }
        if run.line_through {
            window.paint_strikethrough(
                point(baseline_x, strikethrough_y),
                width,
                &StrikethroughStyle {
                    thickness: px(1.),
                    color: Some(run.color),
                },
            );
        }
    }
    glyphs
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

#[cfg(test)]
mod tests {
    //! 装饰线的纵向落点。gpui 的 `TestWindow` 不暴露绘制场景（`plans/M5-refactor.md` 当年就
    //! 因此把「下划线该画出来」退成真机冒烟），所以这里钉的是**那条式子**；
    //! 「`paint_run` 真的调了它」仍然只有真机能证。
    //!
    //! 点名导入而不是 `use super::*`：`gpui_kit` 转出整个 `gpui`，里面有个叫 `test` 的属性宏，
    //! glob 会让 `#[test]` 递归展开成它。
    use super::decoration_y;
    use gpui_kit::{Pixels, px};

    fn y(box_height: f32, ascent: f32, descent: f32) -> (Pixels, Pixels) {
        // 基线摆在 100 处即可：两条线都是相对它算的，绝对值不影响比对。
        decoration_y(px(100.), px(box_height), px(ascent), px(descent))
    }

    /// 下划线用 gpui **自己那个公开函数**比一遍，而不是抄一个「基线下 2px」。
    /// 系数（`UNDERLINE_DESCENT_OFFSET_FACTOR`）哪天变了，这条会红——真机上它的症状是
    /// 「线与字之间的缝跟着字号涨缩」，而那要跑很久才看得出来。
    ///
    /// 比的是**差**而不是 `==`：两边都是 f32 的加减乘，只是次序不同（实测同一组数差
    /// `0.000004px`），而 1/100 px 远在任何设备的像素网格之下——拿精确相等当门，
    /// 钉住的会是编译器的折叠顺序，不是几何。
    #[test]
    fn the_underline_follows_gpui_own_offset() {
        for (height, ascent, descent) in [
            (24.0f32, 16.0f32, 4.0f32),
            (40.0, 16.0, 4.0),
            (17.0, 14.0, 3.0),
        ] {
            let padding_top = (height - ascent - descent) / 2.;
            let top = px(100.) - (px(padding_top) + px(ascent));
            let expected = top + gpui_kit::underline_y_offset(px(height), px(ascent), px(descent));
            let got = y(height, ascent, descent).0;
            assert!(
                (f32::from(got) - f32::from(expected)).abs() < 0.01,
                "盒高 {height} / ascent {ascent} / descent {descent}：{got:?} vs {expected:?}"
            );
        }
    }

    /// 删除线落在**字形身体里**：基线之上、盒顶之下。两条都从 gpui 的式子抄，
    /// 所以这个区间是无条件的 —— 一旦哪天自己改了数，它就先出界。
    #[test]
    fn the_strikethrough_sits_inside_the_glyph_body() {
        let (underline, strikethrough) = y(24.0, 16.0, 4.0);
        assert!(
            strikethrough < px(100.) && strikethrough > px(100.) - px(16.0),
            "删除线该压在基线之上、字形上半身里，实际 {strikethrough:?}"
        );
        assert!(
            underline > px(100.),
            "下划线该在基线之下，实际 {underline:?}"
        );
    }
}
