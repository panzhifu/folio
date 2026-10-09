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
    LayoutId, Length, Pixels, Point, SharedString, StrikethroughStyle, Style, TextRun,
    UnderlineStyle, Window, black, fill, point, px, relative, size,
};

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
pub struct GlyphsElement {
    runs: Vec<GlyphRun>,
    background: Option<Hsla>,
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
        _cx: &mut App,
    ) {
        if let Some(background) = self.background {
            window.paint_quad(fill(bounds, background));
        }
        for run in &self.runs {
            paint_run(window, bounds.origin, run);
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
            background: None,
        }
    }

    pub fn background(mut self, color: Hsla) -> Self {
        self.background = Some(color);
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
