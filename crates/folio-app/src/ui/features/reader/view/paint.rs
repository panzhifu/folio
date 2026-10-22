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

use std::collections::HashMap;
use std::sync::Arc;

use folio_core::content::format::css::computed::WritingMode;
use folio_core::infra::metrics::Metric;
use folio_core::layout::{Justify, JustifyMode};
use gpui_kit::{
    App, Bounds, Element, ElementId, Font, FontWeight, GlobalElementId, Hsla, InspectorElementId,
    IntoElement, LayoutId, Length, LineLayout, PathBuilder, Pixels, Point, SharedString,
    StrikethroughStyle, Style, TextRun, UnderlineStyle, WeakEntity, Window, black, fill, point, px,
    relative, size,
};

use crate::ui::features::reader::components::selection::{self, SelectionVisual};
use crate::ui::features::reader::model::session::ReaderSession;

/// 一条已经定好位置的 run，等于 `folio_core::layout::LineRun` 的绘制侧投影。
/// `x` / `baseline` 相对本元素原点（行框左上角，未滚动时就是版心左上角）。
pub struct GlyphRun {
    pub text: SharedString,
    pub font: Font,
    pub font_size: f32,
    pub color: Hsla,
    /// 这条 run 沿**内联轴**的起点（屏幕坐标）。横排就是 x，`vertical-rl` 是 y。
    pub x: f32,
    /// 基线在**侧轴**上的位置（屏幕坐标）。横排就是基线那条横线的 y，竖排是那条竖线的 x。
    pub baseline: f32,
    /// 这一份排版用的推进轴。整个场景同一份，逐 run 带着是因为绘制这一趟只有 run 在手。
    /// 有了它，`x` / `baseline` 这两个纯量才知道自己落在哪根轴上 —— 对调与符号都只在
    /// `WritingMode::project` / `project_delta` 那一处写，界面不许各折一遍。
    pub flow: WritingMode,
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
    /// [`Document::shaped_line`](crate::ui::features::reader::model::chapter_layout::Document::shaped_line) 填进来；
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
    /// 折页那一档给这条 run 的裁剪：这个字按它的**平面位置**判还在不在这一张纸上。
    ///
    /// 两张纸各带一个，而且两侧的判据严格互补（[`turn_animation::Cut::keeps`] 用的是带符号
    /// 距离与一个布尔），所以压在折线上的那个字只归一张画 —— 两边都留会在同一帧把它画两遍，
    /// 字色深一档；两边都丢会在折线上留出一道没有字的缝。裁剪按点不按列：折线是有倾角的
    /// 一条斜线，「第几列之后不画」那种竖棍式的判据表达不了它。
    pub cut: Option<crate::ui::features::reader::motion::turn_animation::Cut>,
}

impl GlyphRun {
    /// 这条 run 里一个字（元素局部坐标）这一帧还画不画。
    fn keeps(&self, x: f32, y: f32) -> bool {
        match self.cut {
            Some(cut) => cut.keeps(x, y),
            None => true,
        }
    }

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
            flow: WritingMode::HorizontalTb,
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
            cut: None,
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

    /// 这一条 run 用哪根推进轴。竖排那一档必须由场景层显式给，默认是横排。
    pub fn on_flow(mut self, flow: WritingMode) -> Self {
        self.flow = flow;
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

/// 留在书上的一条痕迹在这一帧的形状。三档画法（`folio_core::AnnotationStyle`）到这里归成
/// 两件事：**一块实心矩形**（高亮带、字下实线）或**一条要现铺的波浪**。
///
/// 为什么不是「一屏一条 enum」：高亮带与实线在绘制上是同一句 `paint_quad`，差的只是那块
/// 矩形多高、压多少透明度 —— 那是几何那一层（`view/body.rs`）算出来的事。只有波浪线需要
/// 这一层多做一次路径构造，所以分叉只分在这里。
pub(in crate::ui::features::reader) enum Mark {
    /// 一块实心矩形，坐标与 `runs` 同一套（元素局部）。高亮带与实线都走这一格。
    Quad(Bounds<Pixels>, Hsla),
    /// 一条波浪线。`bounds` 是它的**外盒**：左端、中线所在的 y、长，高 = 峰峰值
    /// （上下各一半）。起伏怎么排归这一层（见 [`wave_half_waves`]），落点归几何那一层。
    Wave(Bounds<Pixels>, Hsla),
}

/// 画一条痕迹。`origin` 是承载它的那块元素在窗口里的原点 —— 矩形是元素局部坐标，
/// 不加这一个偏移，阅读视图不在窗口原点（Android 的系统栏 inset、桌面的侧栏）时
/// 痕迹就整片飘到文字上方（把手准、颜色偏，就是它）。
///
/// 这一条同时服务两个画法层：可重排那一族把它压在**字形之下**（高亮带不能吃掉笔画），
/// 定版式那一族把它压在**页图之上**（那一族的字就在那张栅格出来的图里，压在图下等于
/// 没画）。两层的几何各算各的矩形，画法只有这一份 —— 三档画法将来加一档时不必两处改。
pub(in crate::ui::features::reader) fn paint_mark(
    mark: &Mark,
    origin: Point<Pixels>,
    window: &mut Window,
) {
    match mark {
        Mark::Quad(rect, color) => window.paint_quad(fill(*rect + origin, *color)),
        Mark::Wave(rect, color) => paint_wave(*rect + origin, *color, window),
    }
}

/// 波浪线在这一段长度里排几个**半波**，永远是**偶数**（一整个波 = 两个半波）。
///
/// 只按整波取数：于是每一条划线的波浪都**起于中线、终于中线**，一排划线并眼看过去收尾
/// 是齐的。按固定波长铺满再截断的话，右端会留下长短不一的残波，那是最扎眼的一种不齐。
///
/// 每一段等长（`宽度 ÷ 个数`），所以「整波」这一档是靠**轻微拉伸波长**达成的，不是靠截断：
/// 一段 8 像素的字给一个波（波长 8 而不是 7），一段 15 像素的给两个（7.5）。偏离不超过
/// 半个波，肉眼分不出与标准波长。
///
/// `0` = 这一段没有线可画（窄到一像素都不到）；短到一个波的段落也给**一整波**而不是半个，
/// 于是划一个字的那条读起来是「一个小起伏」而不是半个起伏翘在中线上。
pub(in crate::ui::features::reader) fn wave_half_waves(width: f32) -> usize {
    if width < 1.0 {
        return 0;
    }
    ((width / WAVE_LENGTH).round().max(1.0) as usize) * 2
}

/// 波浪线一个完整波占多长（逻辑像素）。
///
/// 定在 7 是照字下的空间来的：正文行高约 30、基线下那一段只有几像素，波长再短就成了
/// 一片糊，再长则一条划线凑不出两个波、读起来像一条歪线。
const WAVE_LENGTH: f32 = 7.0;
/// 波浪线离中线最远多远（单侧振幅）。
const WAVE_AMPLITUDE: f32 = 1.6;
/// 波浪线那块盒子的总高 = 峰峰值。几何那一层（`view/body.rs`）按它摆盒子，
/// 这一层从盒子的高反读振幅 —— 所以这个数是两边共用的**唯一一个**家。
pub(in crate::ui::features::reader) const WAVE_HEIGHT: f32 = WAVE_AMPLITUDE * 2.0;
/// 波浪线那条线多粗。比实线那一档（`UNDERLINE_THICKNESS = 2`）细一点：起伏的线在同样的
/// 粗细下会显得更重，因为墨都堆在同一条窄带里。
const WAVE_STROKE: f32 = 1.6;

/// 把一条波浪线画出来：从 `bounds` 的左端沿中线起，每半个波一段二次贝塞尔，
/// 控制点压在中线上下交替 `振幅` 处（振幅 = `bounds` 高的一半）。
fn paint_wave(bounds: Bounds<Pixels>, color: Hsla, window: &mut Window) {
    let width = f32::from(bounds.size.width);
    let count = wave_half_waves(width);
    if count == 0 {
        return;
    }
    let mid = f32::from(bounds.origin.y) + f32::from(bounds.size.height) * 0.5;
    let amp = f32::from(bounds.size.height) * 0.5;
    let left = f32::from(bounds.origin.x);
    let step = width / count as f32;
    let mut path = PathBuilder::stroke(px(WAVE_STROKE));
    path.move_to(point(px(left), px(mid)));
    for ix in 0..count {
        let from = left + step * ix as f32;
        let to = from + step;
        // 交替方向：第一段先**上**起，于是每条划线的起头长得一样。
        let ctrl_y = mid + if ix % 2 == 0 { -amp } else { amp };
        path.curve_to(
            point(px(to), px(mid)),
            point(px((from + to) * 0.5), px(ctrl_y)),
        );
    }
    if let Ok(built) = path.build() {
        window.paint_path(built, color);
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
    /// 批注（M7 划线）。一屏里同时可以有几十条，画在选中高亮**之下**、字形之下。
    ///
    /// 与选中高亮分开两份：前者是读者留在书上的东西，一屏里同时可以有几十条；
    /// 后者是这一次挑中的那一段，还要长出把手。
    marks: Vec<Mark>,
    /// 选中件（M6）。空的高亮 = 这一屏没有选中。画法与命中都在 [`selection`]。
    selection: SelectionVisual,
    /// 会话的弱引用：paint 时回写内容边界与把手命中区、注册手势监听。
    /// `None` = 兜底那一行没有书，不参与选中。
    reader: Option<WeakEntity<ReaderSession>>,
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
        let __paint = crate::ui::features::reader::model::perf::Timer::start();
        // 绘制层级（自下而上）：块级装饰（更早入树的兄弟）→ 批注痕迹 → 选中高亮 →
        // 字形 → 把手。痕迹与高亮都在字形之前画，字形之后再补就把字盖住了。
        // 传进来的矩形是元素局部坐标，跟字形与把手一样要加上元素在窗口里的原点：
        // 阅读视图不在窗口原点（Android 的系统栏 inset、桌面的侧栏）时，漏加这一个
        // 偏移，痕迹就会整条飘到文字上方 —— 把手是准的、颜色是偏的，就是它。
        // 波浪那一条加的是同一个偏移：`paint_wave` 认的是窗口坐标。
        for mark in &self.marks {
            paint_mark(mark, bounds.origin, window);
        }
        selection::paint_highlights(&self.selection, bounds.origin, window);
        if let Some(reader) = self.reader.clone() {
            selection::register(&self.selection, &reader, bounds, window, cx);
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
        // 同一帧里同一种 `Font` 只问一次「有没有真粗体面」：`resolve_font` 是
        // fontdb 的一次查询，不能每个字形都付。
        let mut synthetic_bold = HashMap::new();
        window.paint_layer(bounds, |window| {
            for run in &self.runs {
                let __t = crate::ui::features::reader::model::perf::Timer::start();
                __glyphs += paint_run(window, bounds.origin, run, &mut synthetic_bold);
                __shape_ns.set(__shape_ns.get() + __t.ns());
            }
        });
        crate::ui::features::reader::model::perf::log_paint(
            __paint.ns(),
            __shape_ns.get(),
            __glyphs,
        );
        selection::paint_handles(&self.selection, bounds.origin, window);
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
            marks: Vec::new(),
            selection: SelectionVisual::default(),
            reader: None,
        }
    }

    /// 添一条留在书上的痕迹。那块矩形已是它本身（实线与高亮带差的是高与透明度，
    /// 波浪差的是形状），坐标与 `runs` 同一套：元素局部，paint 里统一加元素原点。
    pub fn mark(&mut self, mark: Mark) {
        self.marks.push(mark);
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
/// [`Document::shaped_line`](crate::ui::features::reader::model::chapter_layout::Document::shaped_line) 那张表要存的值。
///
/// `Metric::ShapeLineCalls` 数的是**这一趟**，所以它的口径是「shaping 表未命中一次」，
/// 不再是「每条 run 一次」—— 表接上之后，滚回走过的地方这一格增量应当是 0。
pub(in crate::ui::features::reader) fn shape(window: &Window, run: &GlyphRun) -> Arc<LineLayout> {
    // gpui 的 `LineLayoutCache` 命中时确实不重 shaping，但它的键含整段文本 ——
    // **命中也要把文本哈希一遍**，而且只保两帧。这一格数的是走到 gpui 那层的次数。
    Metric::ShapeLineCalls.count();
    let __t_shape = crate::ui::features::reader::model::perf::Timer::start();
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
    crate::ui::features::reader::model::perf::add(|frame| frame.shape_only_ns += __t_shape.ns());
    layout
}

/// 把一条 run 画出来：几何取自 shaping 表（没有就当场 shape 一次），再逐字形 `paint_glyph`。
/// 这个 run 要不要叠一遍假粗体。
///
/// CSS 的 700 已经一路传进了 `Font`；但 fontdb 只会挑同族里最接近的真实字体面。
/// 如果这个家族只有 400，700 请求还是落回 400 —— 浏览器通常在这里做合成粗体，
/// gpui 不做。判据是「700 与 400 resolve 到同一个 FontId」：同一个面就叠印一遍，
/// 真的存在粗体面时那一格不同，绝不重复画。
fn needs_synthetic_bold(window: &Window, font: &Font, cache: &mut HashMap<Font, bool>) -> bool {
    if font.weight.0 < 600.0 {
        return false;
    }
    if let Some(known) = cache.get(font) {
        return *known;
    }
    let normal = Font {
        weight: FontWeight::NORMAL,
        ..font.clone()
    };
    let bold_id = window.text_system().resolve_font(font);
    let normal_id = window.text_system().resolve_font(&normal);
    let needed = wants_synthetic_bold_when(font, bold_id == normal_id);
    cache.insert(font.clone(), needed);
    needed
}

/// 请求了粗体，而且这个家族只有 regular 那一面。
fn wants_synthetic_bold_when(font: &Font, same_face: bool) -> bool {
    wants_synthetic_bold(font) && same_face
}

fn wants_synthetic_bold(font: &Font) -> bool {
    font.weight.0 >= 600.0
}

fn paint_run(
    window: &mut Window,
    origin: Point<Pixels>,
    run: &GlyphRun,
    synthetic_bold: &mut HashMap<Font, bool>,
) -> u32 {
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
    // 上面那一对是**横排那一档**的读法，只有画装饰线那条老路还在用。
    // 竖排走下面这一对：`run.x` / `run.baseline` 是两个轴中性的纯量（沿字的起点、
    // 基线在侧轴上的位置），经 `project` 才知道各落在物理的哪根轴上。
    let flow = run.flow;
    let (run_inline, run_cross) = flow.project(run.x, run.baseline);
    let mut glyphs = 0u32;
    let embolden = needs_synthetic_bold(window, &run.font, synthetic_bold);
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
    let __t_glyph = crate::ui::features::reader::model::perf::Timer::start();
    for shaped_run in &shaped.runs {
        for glyph in &shaped_run.glyphs {
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
            // 折页：按这个字的**平面位置**判它还在不在这一张纸上。用的是元素局部坐标
            // （`run.x` / `run.baseline` 那一套），与 `view.rs` 摆折面同一条折线、同一把尺。
            // 数过 `glyphs` 之后再裁，那个计数才真的是「这一帧画了几个字」。
            // shaping 报的两个数永远是「沿字走了多少」与「离基线多远」—— 竖排不重做
            // shaping，所以这里只是把它们经同一处投影折成物理位移。
            let (dx, dy) = flow.project_delta(
                f32::from(glyph.position.x) + shift,
                f32::from(glyph.position.y),
            );
            let x = run_inline + dx;
            let y = run_cross + dy;
            if !run.keeps(x, y) {
                continue;
            }
            glyphs += 1;
            let at = point(origin.x + px(x), origin.y + px(y));
            // 单个字形画不出去（缺字形、图集满）不该让整帧失败：与 gpui 自己的
            // `paint_line` 一致，忽略即可。
            let _ = window.paint_glyph(at, shaped_run.font_id, glyph.id, font_size, run.color);
            if embolden {
                // 没有粗体面时的假粗：同一字形再压一遍，水平错开 0.5px。
                // 只动绘制，不动 shaping / 行宽，所以不会因为「加粗」触发重排。
                let _ = window.paint_glyph(
                    point(at.x + px(0.5), at.y),
                    shaped_run.font_id,
                    glyph.id,
                    font_size,
                    run.color,
                );
            }
        }
    }
    crate::ui::features::reader::model::perf::add(|frame| frame.glyph_ns += __t_glyph.ns());
    // 装饰线。字形是我们自己一个个画的，gpui 的 `ShapedLine::paint` 里没有那一段，
    // 所以线也得自己画 —— 少这一段就是「`text-decoration` 解析了、传进 shaping 了、
    // 然后没人画」：`UnderlineStyle` 造出来只为了进 `TextRun`，画面的循环只读 glyphs。
    //
    // 一条 run 一种样式（样式来自 `RunStyle`，整段一致），所以整条 run 就是一个装饰段，
    // 不必照 gpui 那样按 `decoration_runs` 切字符区间。宽度用 `shaped.width`（**画出来的**
    // 那份宽），不用布局给的 run 宽：后者含 `word-spacing` 一类的富余，线会伸到字后面去。
    // 被折线裁到的这一张这一帧不画装饰线：字按逐点判据收了，线的起止却没有对应的收法
    // （它是一整条 run 一个矩形，而折线是斜的），伸到折线外面会比缺一根线更难看。
    // 280ms 之内的事，且只影响带下划线/删除线的那些 run。
    if (run.underline || run.line_through) && run.cut.is_none() && flow.is_vertical() {
        // 竖排：线是**竖**的，而 `paint_underline` / `paint_strikethrough` 只会画
        // `size(width, thickness)` 的横矩形（`gpui-pre/window.rs:4846-4892`），
        // 所以这里自己走 `paint_quad`。
        // 「离基线多远」仍按横排那一份式子算：把基线喂 0，拿回来的就是纯深度；
        // 深度与长度都经同一处投影换成物理的 x / y —— `vertical-rl` 的正深度往**左**。
        let (u, s) = decoration_y(px(0.0), px(run.box_height), shaped.ascent, shaped.descent);
        let width = shaped.width;
        for (on, depth) in [
            (run.underline, f32::from(u)),
            (run.line_through, f32::from(s)),
        ] {
            if !on {
                continue;
            }
            let (dx, dy) = flow.project_delta(0.0, depth);
            let (lx, lt) = flow.project(f32::from(width), 1.0);
            window.paint_quad(fill(
                Bounds::new(
                    point(
                        origin.x + px(run_inline + dx),
                        origin.y + px(run_cross + dy),
                    ),
                    size(px(lx), px(lt)),
                ),
                run.color,
            ));
        }
    } else if (run.underline || run.line_through) && run.cut.is_none() {
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

#[cfg(test)]
mod tests {
    //! 装饰线的纵向落点。gpui 的 `TestWindow` 不暴露绘制场景（`plans/M5-refactor.md` 当年就
    //! 因此把「下划线该画出来」退成真机冒烟），所以这里钉的是**那条式子**；
    //! 「`paint_run` 真的调了它」仍然只有真机能证。
    //!
    //! 点名导入而不是 `use super::*`：`gpui_kit` 转出整个 `gpui`，里面有个叫 `test` 的属性宏，
    //! glob 会让 `#[test]` 递归展开成它。
    use super::{WAVE_LENGTH, decoration_y, wants_synthetic_bold_when, wave_half_waves};
    use gpui_kit::{Font, FontWeight, Pixels, px};

    /// 波浪线的分段：永远是**偶数个半波**（整波），于是每条划线都起于中线、终于中线。
    ///
    /// 把 `* 2` 那一步去掉（改成按半波数取整）这条红在「奇数」那一格 —— 症状是屏上一排
    /// 划线的右端一半收在上、一半收在下，那是锯齿不齐最容易被看见的一种。
    #[test]
    fn the_wave_always_lands_on_a_whole_wave() {
        assert_eq!(wave_half_waves(0.0), 0, "没有宽就没有线");
        assert_eq!(wave_half_waves(0.5), 0, "窄到一像素以下不值得画");
        assert_eq!(wave_half_waves(1.0), 2, "短到一个字也给一整波，不给半个");
        assert_eq!(wave_half_waves(WAVE_LENGTH), 2, "刚好一个波长 = 两个半波");
        assert_eq!(wave_half_waves(WAVE_LENGTH * 3.0), 6);
        for width in [8.0, 23.0, 77.0, 340.0, 1200.0] {
            let halves = wave_half_waves(width);
            assert!(
                halves.is_multiple_of(2) && halves >= 2,
                "{width} 排了 {halves} 个半波，收口不在中线上"
            );
            // 每一段等长 ⇒ 实际波长与标准档的偏离不超过半波一半（`round` 的最大误差）。
            let stretched = width / (halves / 2) as f32;
            assert!(
                (stretched - WAVE_LENGTH).abs() <= WAVE_LENGTH * 0.5,
                "{width} 被拉成波长 {stretched}，离标准 {WAVE_LENGTH} 太远"
            );
        }
    }

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
    fn synthetic_bold_only_when_bold_is_requested_and_the_face_is_the_same() {
        let normal = Font::default();
        let bold = Font {
            weight: FontWeight::from(700.0),
            ..Font::default()
        };
        assert!(!wants_synthetic_bold_when(&normal, true));
        assert!(
            !wants_synthetic_bold_when(&bold, false),
            "有真粗体面时不叠印"
        );
        assert!(wants_synthetic_bold_when(&bold, true));
        let semibold = Font {
            weight: FontWeight::from(600.0),
            ..Font::default()
        };
        assert!(wants_synthetic_bold_when(&semibold, true));
    }

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
