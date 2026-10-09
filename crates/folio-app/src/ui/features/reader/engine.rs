//! 桌面端把排版结果摆上屏：`Layout` 的每个 `LineBox` 当成虚拟列表的一行。
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
//! 排版本身在 [`crate::ui::shared::document`]，这里只有绘制。
//! 逐字形绘制（`shape_line` / `paint_glyph`）留给需要字形级控制的时候：
//! 选择高亮、ruby 注音、竖排（M5/M6）。

use std::ops::Range;
use std::rc::Rc;

use folio_core::layout::LineFragment;
use gpui_kit::base::{VirtualListScrollHandle, v_virtual_list};
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    AnyElement, App, Context, Entity, Hsla, IntoElement, ParentElement, Pixels, Render, Size,
    Styled, Window, div, px, size,
};

use crate::ui::shared::document::Document;
use crate::ui::shared::measure::gpui_font;
use crate::ui::widgets::run_color;

/// 虚拟列表要的逐项尺寸：宽取版心，高取各行自己的行盒高。
pub fn item_sizes(document: &Document) -> Rc<Vec<Size<Pixels>>> {
    let width = document.layout().width;
    Rc::new(
        document
            .lines()
            .iter()
            .map(|line| size(px(width), px(line.height)))
            .collect(),
    )
}

/// 第 `line` 项在虚拟列表里的顶部偏移：逐项高度累加。
///
/// 为什么不用 `Layout` 的绝对 `y`：列表排的是**项**，`item_sizes` 给的就是
/// `line.height`，块之间的外边距在列表空间里不存在。滚动位置要跟这套坐标对齐，
/// 用排版绝对 y 就会每段差一档 margin —— 而那正是「恢复后差半屏」那类 bug 的形状。
/// 越界的 `line` 夹到最后一项。
pub fn item_top(document: &Document, line: usize) -> f32 {
    document
        .lines()
        .iter()
        .take(line.min(document.line_count().saturating_sub(1)))
        .map(|row| row.height)
        .sum()
}

/// 列表滚过 `y` 个像素时顶边在第几行。[`item_top`] 的逆。
pub fn line_at_item_top(document: &Document, y: f32) -> usize {
    let mut used = 0.0f32;
    for (ix, row) in document.lines().iter().enumerate() {
        if used + row.height > y {
            return ix;
        }
        used += row.height;
    }
    document.line_count().saturating_sub(1)
}

/// 第 `ix` 行的元素。列表项按 [`item_sizes`] 依次下排，所以行盒的绝对 x 就是它的左偏移。
///
/// 一行里的每个 run 单独摆：字号、字重、斜体、颜色各自成立，
/// 纵向按**基线**对齐 —— gpui 把字形在自己的 line-height 盒里垂直居中，
/// 基线落在盒顶往下 `(盒高 + ascent - descent) / 2` 处，所以反推 top。
/// 度量与绘制用同一个 [`gpui_font`]，否则偏移与实宽会对不上。
fn line_element(document: &Document, ix: usize, foreground: Hsla, known: &[String]) -> AnyElement {
    let layout = document.layout();
    let Some(line) = layout.lines.get(ix) else {
        return div().into_any_element();
    };
    // 行文本住在所属块的那个归一化串里（`LineRun::norm` 是它的字节区间）。
    let Some(block) = layout.block_of_line(ix) else {
        return div().into_any_element();
    };
    let family = document.prefs().font_family.clone().into();
    let mut left = line.x;
    let mut row = div().relative().w_full().h(px(line.height));
    for fragment in &line.fragments {
        match fragment {
            LineFragment::Text(run) => {
                let text = block.text.get(run.norm.clone()).unwrap_or("");
                let box_height = run.style.box_height();
                let baseline_in_box = (box_height + run.ascent - run.descent) / 2.0;
                // 基线偏移（`vertical-align`）与装饰线都按 run 走：偏移在这里减一次，
                // 线的有无由 gpui 的 `.underline()` / `.line_through()` 负责（1px、按字色），
                // 移动端那条 `TextRun` 用的是同一组默认值。
                let mut cell = div()
                    .absolute()
                    .left(px(left))
                    .top(px(line.baseline
                        - run.style.baseline_shift
                        - baseline_in_box))
                    .whitespace_nowrap()
                    .font(gpui_font(&run.style.font, &family, known))
                    .text_size(px(run.style.font_size))
                    .line_height(px(box_height))
                    .text_color(run_color(run.style.color, foreground));
                if run.style.underline {
                    cell = cell.underline();
                }
                if run.style.line_through {
                    cell = cell.line_through();
                }
                row = row.child(cell.child(text.to_string()));
                left += run.width;
            }
            LineFragment::Image(img) => {
                // 图片底贴基线：盒顶 = 基线 - 图高。
                if let Some(source) = document.images().render_source(&img.href) {
                    row = row.child(
                        div()
                            .absolute()
                            .left(px(left))
                            .top(px(line.baseline - img.height))
                            .w(px(img.width))
                            .h(px(img.height))
                            .child(gpui_kit::img(source).size_full()),
                    );
                }
                left += img.width;
            }
        }
    }
    row.into_any_element()
}

/// 引擎正文的滚动身份。`ElementId` 由字符串派生，跨帧稳定。
pub const ENGINE_LIST_ID: &str = "engine-lines";

/// 把引擎的行列表接到视图上。`render_item` 由视图提供，因为虚拟列表的闭包签名
/// 要求它拿到 `&mut V` 与 `Context<V>`（滚动位置一类的副作用要能回写视图）。
///
/// `handle` 是**视图持有**的滚动句柄：进度（偏移 → 第几个字）与恢复（第几个字 → 滚到
/// 哪一行）都要从同一个句柄读同一个数。每帧新建一个句柄的话，滚动位置就成了
/// 「这一帧才知道」的局部量，`locator()` 永远读到 0。
pub fn engine_lines<V: Render>(
    view: Entity<V>,
    sizes: Rc<Vec<Size<Pixels>>>,
    handle: &VirtualListScrollHandle,
    render_item: impl 'static
    + Fn(&mut V, Range<usize>, &mut Window, &mut Context<V>) -> Vec<AnyElement>,
) -> impl IntoElement {
    v_virtual_list(view, ENGINE_LIST_ID, sizes, render_item).track_scroll(handle)
}

/// 一帧内把 `[start, end)` 的行画出来。正文色取当前主题，
/// 书自己声明了 `color` 的 run 由 [`run_color`] 覆盖它。
pub fn render_range(document: &Document, range: Range<usize>, cx: &App) -> Vec<AnyElement> {
    let foreground = cx.theme().foreground;
    // 这台设备注册的字族名：`gpui_font` 靠它把书里不存在的 `font-family` 落回偏好字体
    // （`resolve_font` 解析不到是 panic，不是回退）。
    let known = cx.text_system().all_font_names();
    (range.start..range.end.min(document.line_count()))
        .map(|ix| line_element(document, ix, foreground, &known))
        .collect()
}
