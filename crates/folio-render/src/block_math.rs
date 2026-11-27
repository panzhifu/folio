//! 流式块级排版的两条纯数式子：作者尺寸折算与竖外边距折叠。
//!
//! 从 `geometry.rs` 切出来只因为那个文件贴着行数上限；口径的现场注释在各自的
//! 文档上，实排与只量高两条路都问这一份，抄两遍迟早漂。

use folio_format::css::computed::Length;

/// 作者写了尺寸就用它，没写给 `None`。`height` 那一族认 px 与**有基准的**百分比：
/// 百分比折自包含块的**高**，而流式排版里父块的高常由内容算出来 —— CSS 在同一条
/// 规则下也判 `auto`，所以「没写过」与「写了 `50%` 而上游没有既定高」是同一个数。
/// 基准从 [`LayoutOptions::page_height`] 一路传下来，判据写在那一格的注释上。
///
/// 只给流内块级盒用。替换元素（`<img>` / `<svg>`）走
/// [`crate::ports::image_size`]，那条路里百分比折得动（基准是图的可用宽）。
pub(crate) fn definite(length: Option<Length>, cb_h: Option<f32>) -> Option<f32> {
    match length {
        Some(Length::Px(v)) => Some(v.max(0.0)),
        Some(Length::Percent(p)) => cb_h.map(|basis| (p * basis).max(0.0)),
        _ => None,
    }
}

/// 相邻两个竖外边距折叠成一段净空（CSS 2.1 §10.3.4）。
///
/// 规范那三档合成一个式子就够了：**最大正数 + 最小负数**。两个正数得较大值
/// （段间距不显松靠的就是这一档），一正一负得两者之和，两个负数得较负的那个。
///
/// `0` 是这个式子的恒等元，所以调用点用 `prev_bottom = 0.0` 起手就等价于
/// 「前面没有兄弟」；从前这里写的是 `if prev_bottom > 0.0`，负下边距会同时踩中
/// 「没有前驱」和「夹回 0」两个坑 —— 换成一趟加法就没这两件事了。
pub(crate) fn collapse_margins(top: f32, bottom: f32) -> f32 {
    top.max(bottom).max(0.0) + top.min(bottom).min(0.0)
}

/// 投影只在这里发生一次。
///
/// **代价要说清，这一条不是免费的**：盒模型那几族边是**物理**的
/// （`margin-top` / `padding-left` / `border[上右下左]`），转置会把它们摆到另一条边上。
/// CSS Writing Modes 那张 logical→physical 映射表还没接，所以竖排里
/// 「段落之间的上下留白」会变成「列与列之间的留白」，而作者写给**行**的 `margin-bottom`
/// 会横过来。段落分开的那个方向在竖排里恰好就是列间，所以多数中文竖排本看着是对的；
/// 一条式子内缩进的物理边（如表格、`hr`）会错到另一侧去。显式记进降级清单，别当它不存在。
use folio_format::css::computed::WritingMode;

use crate::{BlockBox, LineBox};

/// 把引擎排好的逻辑横排长条整体投影成**物理**坐标；竖排两轴对调也在这里。
pub(crate) fn transposed(
    blocks: Vec<BlockBox>,
    mut lines: Vec<LineBox>,
    width: f32,
    height: f32,
    mode: WritingMode,
) -> (Vec<BlockBox>, Vec<LineBox>, f32, f32) {
    // 长条沿块轴的总长 = 逻辑高；一列的长 = 逻辑宽。物理那两条边因此互换。
    let strip = height;
    // `vertical-rl` 的列从右往左堆：块轴反着走，所以起点要从右缘翻面数。
    let flip = mode == WritingMode::VerticalRl;
    // 逻辑 (x=沿字, y=沿列堆, w=沿字长, h=列厚) → 物理那四格。
    // `baseline` 不动：它存的始终是「沿块轴从**起点边**算进去的深度」，
    // 起点边在 rl 是列的右缘、在 lr 是左缘 —— 落地成物理 x 是 [`Layout::cross_baseline`] 的事。
    let map = |x: f32, y: f32, w: f32, h: f32| -> (f32, f32, f32, f32) {
        (if flip { strip - (y + h) } else { y }, x, h, w)
    };
    for line in &mut lines {
        let (x, y, w, h) = map(line.x, line.y, line.width, line.height);
        line.x = x;
        line.y = y;
        line.width = w;
        line.height = h;
    }
    let blocks = blocks
        .into_iter()
        .map(|mut b| {
            let (x, y, w, h) = map(b.x, b.y, b.width, b.height);
            b.x = x;
            b.y = y;
            b.width = w;
            b.height = h;
            b
        })
        .collect();
    (blocks, lines, strip, width)
}
