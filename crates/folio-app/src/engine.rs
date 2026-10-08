//! 自研排版引擎的呈现层：把 `Layout` 的每个 `LineBox` 当成虚拟列表的一行。
//!
//! 为什么用 `v_virtual_list` 而不是自己滚：
//! - 行盒高度是**已知**的（字号 × 行距），正是它要的 `item_sizes`；
//! - 滚轮、滚动条、`scroll_to_item` 由框架负责，不重造；
//! - 一章几千行也只渲染视口内的；M4 的分页 = 同一个列表换 item 尺寸。
//!
//! 一行之内**不再让 gpui 参与折行**：文本以 `whitespace_nowrap` 落进去，
//! 左缘偏移、居中偏移、行高全部来自引擎算好的几何 —— 这一屏的排版由引擎说了算，
//! gpui 只把一行文字画在指定位置。
//!
//! 逐字形绘制（`shape_line` / `paint_glyph`）留给需要字形级控制的时候：
//! 选择高亮、ruby 注音、竖排（M5/M6）。

use std::ops::Range;
use std::rc::Rc;

use folio_core::css::computed::{ComputedStyle, TextAlign, cascade};
use folio_core::css::{Origin, RuleSet};
use folio_core::dom::{Dom, NodeId};
use folio_core::layout::{Layout, LayoutOptions, LineBox, layout as typeset};
use folio_core::prefs::Prefs;
use gpui_kit::base::v_virtual_list;
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    AnyElement, App, Context, Entity, IntoElement, ParentElement, Pixels, Render, Styled, Window,
    div, px, size,
};

use crate::measure::{GpuiMeasure, gpui_font};

pub struct Engine {
    /// 章源文本（XHTML）。排版每次从它重算，只在几何参数没变时复用结果。
    source: String,
    rules: RuleSet,
    prefs: Prefs,
    /// 上次排版时的 (内容宽度, 字号, 行距)。
    laid_out: Option<(f32, f32, f32)>,
    layout: Layout,
}

impl Engine {
    pub fn new(source: String, css: &str, prefs: Prefs) -> Self {
        let mut rules = RuleSet::default();
        // 书自己的 CSS 是作者层；读者偏好通过根样式进来，
        // 带 `!important` 的读者样式表留给 M5 的 reader.css。
        rules.extend(RuleSet::parse(css, Origin::Author));
        Self {
            source,
            rules,
            prefs,
            laid_out: None,
            layout: Layout::default(),
        }
    }

    pub fn set_prefs(&mut self, prefs: Prefs) {
        self.prefs = prefs;
    }

    pub fn set_chapter(&mut self, source: String) {
        if source != self.source {
            self.source = source;
            self.laid_out = None;
        }
    }

    pub fn lines(&self) -> &[LineBox] {
        &self.layout.lines
    }

    pub fn line_count(&self) -> usize {
        self.layout.lines.len()
    }

    /// 每行的可见文本。测试用它断言「哪一行是什么」，
    /// 状态栏以后也用它取当前行。
    pub fn line_texts(&self) -> Vec<String> {
        (0..self.layout.lines.len())
            .map(|ix| self.layout.line_text(ix).to_string())
            .collect()
    }

    pub fn height(&self) -> f32 {
        self.layout.height
    }

    /// 版心宽度。**必须 ≤ 阅读栏实际宽度**，否则引擎按宽列折行、屏幕按窄栏裁剪，
    /// 居中的标题就会被推到栏外（这一步量不到 flex 之后的真实宽度，
    /// 所以由外壳把侧栏占位报进来）。
    pub fn content_width(window: &Window, prefs: &Prefs, pane_reserve: f32) -> f32 {
        let viewport = window.viewport_size();
        let usable = viewport.width - px(pane_reserve + prefs.margin_x * 2.0);
        f32::from(usable.min(px(prefs.max_measure))).max(1.0)
    }

    /// 根样式 = 读者偏好。书自己的 CSS 能覆盖它（作者层高于读者普通层，
    /// 这与浏览器一致），但读者的 `!important` 终究能压回来（M5）。
    fn root_base(prefs: &Prefs) -> ComputedStyle {
        ComputedStyle {
            font_size: prefs.font_size,
            line_height: prefs.line_height,
            font_family: prefs.font_family.clone(),
            text_align: if prefs.justify {
                TextAlign::Justify
            } else {
                TextAlign::Left
            },
            text_indent: prefs.font_size * prefs.indent_em.max(0.0),
            ..ComputedStyle::default()
        }
    }

    /// 几何参数没变就复用上一次的排版。度量要用窗口级文本系统，所以这一步在
    /// `render` 里做；M4 把整条链挪进 `background_spawn`，主线程不再参与。
    pub fn ensure_layout(&mut self, window: &mut Window, cx: &mut App, pane_reserve: f32) {
        let width = Self::content_width(window, &self.prefs, pane_reserve);
        let key = (width, self.prefs.font_size, self.prefs.line_height);
        if self.laid_out == Some(key) {
            return;
        }
        let measure = GpuiMeasure::new(
            cx.text_system().clone(),
            window.text_system().clone(),
            self.prefs.font_family.clone(),
        );
        let dom = Dom::parse(self.source.as_bytes());
        let styles = cascade(&dom, &self.rules, &Self::root_base(&self.prefs));
        self.layout = typeset(
            &dom,
            &styles,
            body_of(&dom),
            LayoutOptions { width },
            &measure,
        );
        self.laid_out = Some(key);
    }

    /// 虚拟列表要的逐项尺寸。
    pub fn item_sizes(&self) -> Rc<Vec<gpui_kit::Size<Pixels>>> {
        let width = self.layout.width;
        Rc::new(
            self.layout
                .lines
                .iter()
                .map(|l| size(px(width), px(l.height)))
                .collect(),
        )
    }

    /// 第 `ix` 行的元素。列表项按 `item_sizes` 依次下排，所以行盒的绝对 x 就是它的左偏移。
    ///
    /// 一行里的每个 run 单独摆：字号、字重、斜体、颜色各自成立，
    /// 纵向按**基线**对齐 —— gpui 把字形在自己的 line-height 盒里垂直居中，
    /// 基线落在盒顶往下 `(盒高 + ascent - descent) / 2` 处，所以反推 top。
    /// 度量与绘制用同一个 `gpui_font`，否则偏移与实宽会对不上（见 measure.rs）。
    fn line_element(&self, ix: usize, foreground: gpui_kit::Hsla) -> AnyElement {
        let Some(line) = self.layout.lines.get(ix) else {
            return div().into_any_element();
        };
        let Some(block) = self.layout.block_of_line(ix) else {
            return div().into_any_element();
        };
        let family: gpui_kit::SharedString = self.prefs.font_family.clone().into();
        let mut left = line.x;
        let mut row = div().relative().w_full().h(px(line.height));
        for run in &line.runs {
            let text = block.text.get(run.norm.clone()).unwrap_or("");
            let box_height = run.style.box_height();
            let baseline_in_box = (box_height + run.ascent - run.descent) / 2.0;
            row = row.child(
                div()
                    .absolute()
                    .left(px(left))
                    .top(px(line.baseline - baseline_in_box))
                    .whitespace_nowrap()
                    .font(gpui_font(&run.style.font, &family))
                    .text_size(px(run.style.font_size))
                    .line_height(px(box_height))
                    .text_color(run.style.color.map_or(foreground, |c| {
                        gpui_kit::Rgba {
                            r: c.0 as f32 / 255.0,
                            g: c.1 as f32 / 255.0,
                            b: c.2 as f32 / 255.0,
                            a: 1.0,
                        }
                        .into()
                    }))
                    .child(text.to_string()),
            );
            left += run.width;
        }
        row.into_any_element()
    }
}

/// `<body>` 不在（纯片段或畸形 XHTML）时退回文档根：排版层只认「从这里往下找内容」。
fn body_of(dom: &Dom) -> NodeId {
    dom.elements()
        .find(|(id, _)| dom.tag_name(*id) == Some("body"))
        .map(|(id, _)| id)
        .unwrap_or(dom.document)
}

/// 引擎正文的滚动身份。`ElementId` 由字符串派生，跨帧稳定。
pub const ENGINE_LIST_ID: &str = "engine-lines";

/// 把引擎的行列表接到视图上。`render_item` 由视图提供，因为虚拟列表的闭包签名
/// 要求它拿到 `&mut V` 与 `Context<V>`（滚动位置一类的副作用要能回写视图）。
pub fn engine_lines<V: Render>(
    view: Entity<V>,
    sizes: Rc<Vec<gpui_kit::Size<Pixels>>>,
    render_item: impl 'static
    + Fn(&mut V, Range<usize>, &mut Window, &mut Context<V>) -> Vec<AnyElement>,
) -> impl IntoElement {
    v_virtual_list(view, ENGINE_LIST_ID, sizes, render_item)
}

/// 一帧内把 `[start, end)` 的行画出来。`engine` 与 `cx` 分别只读，
/// 所以这个函数不需要 `&mut`。
pub fn render_range(engine: &Engine, range: Range<usize>, cx: &App) -> Vec<AnyElement> {
    let foreground = cx.theme().foreground;
    (range.start..range.end.min(engine.line_count()))
        .map(|ix| engine.line_element(ix, foreground))
        .collect()
}
