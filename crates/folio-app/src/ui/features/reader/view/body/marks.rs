//! 批注、划线、高亮、便签与语音高亮的绘制数据。
//!
//! 这一层只算矩形与颜色，元素树仍由 `body` 拼。

use std::cell::RefCell;
use std::ops::Range;

use crate::ui::features::reader::components::selection;
use crate::ui::features::reader::model::annotations::{self, Palette};
use crate::ui::features::reader::model::chapter_layout::Document;
use crate::ui::features::reader::model::session::AnnotationSpans;
use crate::ui::features::reader::view::paint::{self, GlyphsElement, Mark};
use folio_core::content::anchor::line_at_y;
use folio_core::layout::{LineBox, LineFragment};
use folio_core::store::{Annotation, AnnotationStyle};
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    AnyElement, App, Bounds, IntoElement, Pixels, Styled, Window, canvas, point, px, size,
};

/// 一行里**字墨**的上下边（相对行顶）与高度。
///
/// 选区与批注底色的高取这一份，不取 `line.height`（行盒高）。行盒按读者的行距
/// （默认 1.75em）撑开，行盒高比字实际占的那一块高出小半行 —— 那一截是行距，不是
/// 「选中的字」。行盒还会被行内图片与注音撑大：图高 200 时整条选区跟着 200 高，
/// 屏幕上看就是「选中区域比文字高出一大截」。
///
/// 只数文本片段，`baseline_shift`（注音、上标）跟着它自己那一笔抬/落；一个文本片段
/// 都没有（纯图片行）时给 `None`，调用方退回行盒。
///
/// 最后一刀把这一条**夹回行盒里**：`run.ascent + run.descent` 是字体的度量盒（hhea 那
/// 一对），CJK 那一族普遍比行距高（真机那本 20px 的字：度量盒 31.3、行距 27.0），于是
/// 相邻两行的底色带互相吃进去一截。底色是逐行 `paint_quad` 压上去的，重叠那一档被
/// 合成两遍，读出来就是一条更深的横带（2026-11-02 真机量的：单层 `(204,226,254)`、
/// 两层 `(168,206,253)`，缝正好在 1190..1201）。gpui-kit 自家 `SelectableText` 的
/// `selection_quad_bounds` 是按 `line_height` 平铺的，两块只相接不重叠 —— 这一刀对齐它。
pub(in crate::ui::features::reader) fn text_ink_box(line: &LineBox) -> Option<(f32, f32)> {
    let mut up = 0.0f32;
    let mut down = 0.0f32;
    for fragment in &line.fragments {
        if let LineFragment::Text(run) = fragment {
            let shift = run.style.baseline_shift;
            up = up.max(run.ascent + shift);
            down = down.max(run.descent - shift);
        }
    }
    if up + down <= 0.0 {
        return None;
    }
    let up = up.min(line.baseline.max(0.0));
    let down = down.min((line.height - line.baseline).max(0.0));
    (up + down > 0.0).then_some((line.y + line.baseline - up, up + down))
}

/// 一条痕迹在屏上的落脚点：哪份排版、摆在哪儿、这一帧可见的是哪一段。
///
/// 选区、批注底色、语音高亮、字下实线四个换算入口共用这一份，省得每处再传
/// 七个位置参数；字段名替调用点记住「哪一格是哪一格」。
#[derive(Clone)]
pub(super) struct MarkSpot<'a> {
    pub(super) doc: &'a Document,
    pub(super) window: &'a Window,
    pub(super) cx: &'a App,
    /// 这一栏（片）在元素坐标里的左缘。
    pub(super) x: f32,
    /// 这一栏在排版坐标里的顶（片的 `origin_y`）。
    pub(super) origin_y: f32,
    /// 本片沿推进轴的长度（`Fragment::height`）。连续滚动那一档递整章的长，
    /// 而竖排只存在于分页，所以那一档里这一格根本不参与。
    pub(super) flow_len: f32,
    /// 元素坐标里正文顶那一截（页眉带 + 安全区）。
    pub(super) header: f32,
    /// 可见带，说的是**推进轴**上的一段（横排 = 章内 y，竖排 = 从长条右缘往里数）。
    pub(super) band: std::ops::Range<f32>,
}

/// 把「选区 ∩ 一个可见带」换成元素局部的矩形，追加进 `out`。
///
/// `spot` 是这一栏的摆位与可见带；`sub` 是选区与可见带求交后的字符区间。
///
/// 行号在进 `rects_for_range_in` **之前**就按这两条夹好（字符索引 ∩ 可见带，
/// `Document::selection_rects_in_band`），所以下面那道 y 滤是重复的一道：留着，
/// 因为「窄化算错就少一块底色」这种账只能靠两道独立的不等式互相兜。
pub(super) fn highlight_rects(
    spot: &MarkSpot<'_>,
    sub: std::ops::Range<usize>,
    out: &mut Vec<Bounds<Pixels>>,
) {
    let MarkSpot {
        doc,
        window,
        cx,
        x,
        origin_y,
        flow_len,
        header,
        band,
    } = spot.clone();
    if sub.start >= sub.end {
        return;
    }
    let layout = doc.layout();
    let vertical = layout.vertical();
    // 与 `Scene::build` 里同一个 `slide`：竖排那一档推进轴从长条右缘反着数。
    let slide = if vertical {
        origin_y + flow_len - layout.width
    } else {
        0.0
    };
    for rect in doc.selection_rects_in_band(window, cx, sub, band.clone()) {
        // 重复的那一道滤，问的还是推进轴上那一段（竖排时矩形的推进轴那一格是 x）。
        let (at, thick) = if vertical {
            (layout.width - (rect.x + rect.width), rect.width)
        } else {
            (rect.y, rect.height)
        };
        if at + thick < band.start || at > band.end {
            continue;
        }
        // 横排：底色贴字墨，不铺满整条行盒（见 [`text_ink_box`]）。`rect.y` 就是行顶，
        // 行盒按 y 单调，`+0.5` 落回它自己那一行。
        let (top, height) = if vertical {
            (rect.y, rect.height)
        } else {
            let line = &layout.lines[line_at_y(layout, rect.y + 0.5)];
            text_ink_box(line).unwrap_or((rect.y, rect.height))
        };
        // 落位分轴：横排沿 y 减片原点，竖排沿 x 加 `slide`（推进轴从长条右缘反着数）。
        let left = x + rect.x + if vertical { slide } else { 0.0 };
        let top_at = if vertical {
            top + header
        } else {
            top - origin_y + header
        };
        out.push(Bounds::new(
            point(px(left), px(top_at)),
            size(px(rect.width), px(height)),
        ));
    }
}

/// 一章的批注：库里的锚点 → 本章坐标 → 元素局部的一条条痕迹，追加进字形元素。
///
/// 换算口径与 [`highlight_rects`] 完全一致（同一套「这一栏摆在哪、可见带是哪一段」），
/// 只是来源从这一次挑中的选区换成了留在书上的批注。三档画法各要一块不同的矩形：
///
/// - **底色带**要的就是字墨那一块，所以直接复用 [`highlight_rects`] —— 与选中高亮同一条
///   换算（贴字、不铺满行盒），顺带把竖排一起对上（那条认推进轴，而字下那两条不认，见
///   [`annotation_underline_rects`] 末尾那条限制）。
/// - **字下实线**与**波浪线**要的是基线下同一条中线上的两块矩形，只差厚度。
///
/// 解不回来的那一条**不画** —— 宁可少一条痕迹，也不把读者划过的痕迹画到别的字上去
/// （见 `modules/annotations.rs`）。
///
/// 缓存与那一份库里的批注按字段分开递进来而不是拿 `&self`：调用点在 `match &mut self.source`
/// 的那一臂里，整个 `self` 已经借不出了。
pub(super) fn annotation_fills(
    element: &mut GlyphsElement,
    spot: &MarkSpot<'_>,
    cache: &RefCell<AnnotationSpans>,
    stored: &[Annotation],
    palette: &Palette,
    chapter: usize,
    cut: Option<crate::ui::features::reader::motion::turn_animation::Cut>,
) {
    let MarkSpot { doc, band, .. } = spot.clone();
    if stored.is_empty() || band.start >= band.end {
        return;
    }
    let spans =
        annotations::resolved_for(cache, stored, chapter, doc.chapter_hash(), doc.text_map());
    for span in spans.iter().filter(|span| !span.is_stale()) {
        let mut lines = Vec::new();
        match span.style {
            AnnotationStyle::Highlight => highlight_rects(spot, span.start..span.end, &mut lines),
            AnnotationStyle::Line => annotation_underline_rects(
                spot,
                span.start..span.end,
                UNDERLINE_THICKNESS,
                &mut lines,
            ),
            AnnotationStyle::Wavy => annotation_underline_rects(
                spot,
                span.start..span.end,
                paint::WAVE_HEIGHT,
                &mut lines,
            ),
        }
        // 底色带压透明度：那一块是整片铺在字下的，实色会把字吃掉。字下那两条不压 ——
        // 它们只占基线下那几像素，压了就分不清是哪一档。
        let solid = palette.color(&span.color);
        let color = if span.style == AnnotationStyle::Highlight {
            solid.opacity(HIGHLIGHT_ALPHA)
        } else {
            solid
        };
        // 带文字笔记的那一条：在**这一片最后一行的右下角**补一枚便签图标。
        //
        // 锚点仍由**末字**那一格决定：长批注跨栏/跨页时，每一带都会算出「自己这一段的最后一
        // 块」，按那块摆就会在每个接缝处都长出一枚图标 —— 末字只落在其中一带，所以整条只有
        // 一枚。横向往**右**取末字所在那一行里最右的那一条带的右端；垂直照旧落在那一行的
        // 下缘之内，不越到下一行上去。
        //
        // 取「那一行的右端」而不是「末字那一格的右端」：末字在跨栏那一条带上时它自己那一格
        // 可能正贴着栏的右边，图标会压进下一栏去；行的右端是排版算出来的列边界，永远在本栏里。
        // 垂直留在行盒**之内**而不是底下：图标要靠那几块矩形被点到（`model::annotations`
        // 的命中判据就是这些矩形），挪到行盒底下就点不着了。
        let note_anchor = if !span.note.trim().is_empty() && span.end > span.start {
            let mut tail = Vec::new();
            highlight_rects(spot, (span.end - 1)..span.end, &mut tail);
            tail.last().copied().and_then(|last| {
                let mut rows = Vec::new();
                highlight_rects(spot, span.start..span.end, &mut rows);
                let mid = f32::from(last.center().y);
                rows.into_iter()
                    .filter(|row| {
                        f32::from(row.origin.y) <= mid
                            && mid < f32::from(row.origin.y + row.size.height)
                    })
                    .map(|row| f32::from(row.origin.x + row.size.width))
                    .reduce(f32::max)
                    .map(|right| (right, last))
            })
        } else {
            None
        };
        for line in lines {
            // 读者划的那道也跟着那张纸走：中心落在被裁掉那一侧就不画。不然一条亮黄的
            // 带会浮在已经被盖走/折走的字上，那一帧读起来像划线自己没跟着翻页。
            let center = line.center();
            if cut.is_some_and(|cut| !cut.keeps(f32::from(center.x), f32::from(center.y))) {
                continue;
            }
            element.mark(if span.style == AnnotationStyle::Wavy {
                Mark::Wave(line, color)
            } else {
                Mark::Quad(line, color)
            });
        }
        if let Some((right, last)) = note_anchor {
            let icon = note_icon_bounds(right, last);
            // 让位判据量的是**图标自己**的中心：它与末字不在同一格，按末字那一格判会把
            // 「图标已被折走、字还在」那一帧画出来。
            let center = icon.center();
            if cut.is_none_or(|cut| cut.keeps(f32::from(center.x), f32::from(center.y))) {
                element.note(Mark::Note(icon, solid));
            }
        }
    }
}

/// 笔记图标的外盒：12×12，落在**这一片最后一行的右下角** —— 右端贴那一行的右边界，
/// 垂直落在那一行的下缘之内（不越到下一行上去）。
///
/// 为什么靠右：批注的「读到哪里了」这件事，落点在阅读方向的**终点**那一侧才读得出来；
/// 靠左的那一枚读者要顺着行看完才知道它指的是哪一段。行的右边界由排版给出，所以跨栏时
/// 这一枚不会跑到下一栏去（这一点与靠右末字那一格不同）。
///
/// 垂直留在行盒之内而不是底下：命中判据（`model::annotations::annotation_at`）走的就是
/// 这些矩形，图标挪到行盒外就点不着了。
///
/// 末字那一格（`last`）恒由 [`highlight_rects`] 算，所以三档画法用的是同一个行盒底边 ——
/// 画法条只有 2px 高，不能拿它自己当垂直基准。
fn note_icon_bounds(right: f32, last: Bounds<Pixels>) -> Bounds<Pixels> {
    const ICON: f32 = 12.0;
    let top = f32::from(last.origin.y) + f32::from(last.size.height) - ICON - 1.0;
    Bounds::new(point(px(right - ICON), px(top)), size(px(ICON), px(ICON)))
}

/// 听书这一段底色的透明度。比选区（[`HIGHLIGHT_ALPHA`]）淡：它是背景里的一条
/// 「机器念到这儿了」，不该与读者亲手挑出来的痕迹抢眼。
const SPEECH_ALPHA: f32 = 0.2;

/// 听书正在念的那一段：**整段**一块底色，画在字形之下。
///
/// 数据源是会话的播放状态（`speech_range`），调用点在 `match &mut self.source` 之前
/// 解好、按章号滤进来 —— 念哪段亮哪段；段内逐句高亮没有数据源（Android 没有
/// `onRangeStart`，按时钟估的那条路线第一版不做，见 `model/speech.rs` 的模块文档）。
/// 换算与批注底色带共用 [`highlight_rects`]（同一套「这一栏摆在哪、可见带是哪一段」），
/// 颜色用主题的 `primary` 而不是 `selection`：前者念出来是「在念这里」，后者是
/// 「人挑了这里」，两种意思不该同色。`cut` 与批注同一条规矩：中心落在被裁掉那一侧
/// 的矩形不画，别让底色浮在已经折走的那半张纸上。
pub(super) fn speech_fill(
    element: &mut GlyphsElement,
    spot: &MarkSpot<'_>,
    speaking: Option<Range<usize>>,
    cut: Option<crate::ui::features::reader::motion::turn_animation::Cut>,
) {
    let MarkSpot { cx, band, .. } = spot.clone();
    let Some(sub) = speaking else {
        return;
    };
    if band.start >= band.end {
        return;
    }
    let mut rects = Vec::new();
    highlight_rects(spot, sub, &mut rects);
    let color = cx.theme().primary.opacity(SPEECH_ALPHA);
    for rect in rects {
        let center = rect.center();
        if cut.is_some_and(|cut| !cut.keeps(f32::from(center.x), f32::from(center.y))) {
            continue;
        }
        element.mark(Mark::Quad(rect, color));
    }
}

/// 定版式一页上**留在书上的那几条痕迹**（元素局部矩形，交给盖在页图之上那一层画）。
///
/// 与 [`annotation_fills`] 的分工只有一处：解锚那一步完全复用 core 那一份
/// （`annotations::resolved_for`：库里的锚点 → 第几个字到第几个字，四级降级同一条），
/// 只有「一段字 → 矩形」换了一把尺 —— 排版那一族问的是行盒，这一族问的是
/// `PageText::rects` 那份逐字盒。键也跟着换：那一族用章文本的内容哈希，这一族用
/// **这一页字层**的内容哈希（`FixedBook::page_text_key`），两者担的是同一个问题
/// 「坐标系所依据的那份内容变了没有」。
///
/// 三档画法落的还是那三块矩形：读者在设置里挑的那一档不该因为书的族别换个长相。
/// 字下那两条贴的是**字盒的底**而不是基线下 `descent × 0.618`：栅格出来的那一页里
/// 没有「基线」这一格可问，引擎给的盒就是那一段字的全部占位，盒底也就是墨迹之下
/// 最近的一条线。差的那几个像素在屏上是「线离字更近了一点」，不是画到别处去。
pub(super) fn fixed_marks(
    cache: &RefCell<AnnotationSpans>,
    stored: &[Annotation],
    page: usize,
    key: u64,
    text: &folio_core::content::format::PageText,
    at: Bounds<Pixels>,
    palette: &Palette,
) -> Vec<Mark> {
    if stored.is_empty() {
        return Vec::new();
    }
    let map = text.text_map();
    let spans = annotations::resolved_for(cache, stored, page, key, &map);
    let mut out = Vec::new();
    for span in spans.iter().filter(|span| !span.is_stale()) {
        let boxes = selection::page_boxes(text, span.start..span.end, at);
        let note_anchor = (!span.note.trim().is_empty())
            .then(|| boxes.last().copied())
            .flatten();
        let solid = palette.color(&span.color);
        let (lines, color, wavy) = match span.style {
            // 底色带压透明度：那一块是整片铺在字上的，实色会把字吃掉。
            AnnotationStyle::Highlight => (
                boxes,
                palette.color(&span.color).opacity(HIGHLIGHT_ALPHA),
                false,
            ),
            AnnotationStyle::Line => (
                strip_bottom(boxes, UNDERLINE_THICKNESS),
                palette.color(&span.color),
                false,
            ),
            AnnotationStyle::Wavy => (
                strip_bottom(boxes, paint::WAVE_HEIGHT),
                palette.color(&span.color),
                true,
            ),
        };
        for line in lines {
            out.push(if wavy {
                Mark::Wave(line, color)
            } else {
                Mark::Quad(line, color)
            });
        }
        if let Some(last) = note_anchor {
            out.push(Mark::Note(page_note_icon(last, at), solid));
        }
    }
    out
}

/// 定版式那一页上的便签图标：贴在这一片**最后一行的右端**，垂直落在行盒下缘之内，
/// 并夹在**页框**里 —— PDF 页几乎铺满屏，作者写的页边距又可以是 0，不做这一道夹，
/// 图标会被切在屏幕外。
///
/// 这一族的 mark 压在**页图之上**（那些字本来就在栅格出来的图里），所以这一枚会盖住该行
/// 最后一个字的下角；流式那一族的同一枚压在字形之下，没有这个问题。
fn page_note_icon(last: Bounds<Pixels>, page: Bounds<Pixels>) -> Bounds<Pixels> {
    const ICON: f32 = 12.0;
    let page_left = f32::from(page.origin.x);
    let page_top = f32::from(page.origin.y);
    let page_right = page_left + f32::from(page.size.width);
    let page_bottom = page_top + f32::from(page.size.height);
    let right = f32::from(last.origin.x + last.size.width);
    let left = (right - ICON)
        .max(page_left + 1.0)
        .min(page_right - ICON - 1.0);
    let top = (f32::from(last.origin.y) + f32::from(last.size.height) - ICON - 1.0)
        .max(page_top + 1.0)
        .min(page_bottom - ICON - 1.0);
    Bounds::new(point(px(left), px(top)), size(px(ICON), px(ICON)))
}

/// 把每块矩形压成贴着**盒底**的那一条（字下那两档画法要的就是它）。
fn strip_bottom(mut boxes: Vec<Bounds<Pixels>>, thickness: f32) -> Vec<Bounds<Pixels>> {
    for box_ in &mut boxes {
        let bottom = f32::from(box_.origin.y) + f32::from(box_.size.height);
        box_.origin.y = px(bottom - thickness);
        box_.size.height = px(thickness);
    }
    boxes
}

/// 把一批痕迹盖在页图**之上**的那一层。
///
/// 为什么定版式要另起一层，而不是复用字形元素里那一份 `mark`：这一族的字就在那张
/// 栅格出来的图里，而页图排在 `element` 之后入树，gpui 的 z-order 是后入树者居上 ——
/// 压在字形之下等于没画。画法一行都不重写：走 [`paint::paint_mark`]，与字形元素里
/// 那一份逐像素同一条式子（波浪的起伏排布、实线的落法都在那一处）。
pub(super) fn marks_above(marks: Vec<Mark>) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            for mark in &marks {
                paint::paint_mark(mark, bounds.origin, window);
            }
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

/// 划线那条实线多厚（逻辑像素）。够看清、又不至于把字下面的笔画糊成一团。
const UNDERLINE_THICKNESS: f32 = 2.0;

/// 高亮带那一片压的透明度。`0.35` 是这一档还是唯一画法时量下来的那个数：整片铺在字下、
/// 不能把字吃掉，又要在浅深两套主题里都读得出「这一句我划过」。
///
/// 只给底色带那一档用。字下那两条线是实色 —— 它们不占字的背景，压了反而看不清是哪一档。
pub(super) const HIGHLIGHT_ALPHA: f32 = 0.35;

/// 一段批注在**可见带内**字下那两块矩形（元素局部）：每行盒一块，追加进 `out`。
///
/// 与 [`highlight_rects`] 同一套坐标换算，只把「行盒整块」换成「字下那条线」：
/// 线落在基线下 `descent × 0.618` 处（与 [`crate::ui::features::reader::view::paint`] 里 `decoration_y` 同源，
/// 所以和正文自己的 `text-decoration: underline` 画在同一条横线上）。行盒的
/// `baseline` 是「行顶到基线的距离」（`layout::collect` 的 `acc.ascent`，半 leading 已含），
/// 于是 `height − baseline` 就是基线下那段。
/// `thickness` 决定这块矩形多厚 —— 实线给 [`UNDERLINE_THICKNESS`]，波浪给
/// [`paint::WAVE_HEIGHT`]（那条线要起伏，盒子得比线本身高出一倍峰峰值）。两种厚度**压在
/// 同一条中线**上：只按顶边对齐的话，读者换一个画法会发现痕迹往下跳了一小截 ——
/// 那是这一屏里最容易被看见的错。
///
/// 这一条只算横排。竖排那一线该往哪偏，`selection_rects_in_band` 给的**物理**矩形里
/// 没有这个信息（要问的是推进轴上的 `line.baseline`），所以竖排的书只有底色带那一档
/// 是对的（它复用 [`highlight_rects`]，那条认轴）。字下那两档在竖排里照旧画进行盒的
/// `y` 那一维 —— 已知限制，与盒模型那几族边一起留给竖排的第二轮。
fn annotation_underline_rects(
    spot: &MarkSpot<'_>,
    range: std::ops::Range<usize>,
    thickness: f32,
    out: &mut Vec<Bounds<Pixels>>,
) {
    let MarkSpot {
        doc,
        window,
        cx,
        x,
        origin_y,
        header,
        band,
        ..
    } = spot.clone();
    if range.start >= range.end {
        return;
    }
    let layout = doc.layout();
    for rect in doc.selection_rects_in_band(window, cx, range, band.clone()) {
        if rect.y + rect.height <= band.start || rect.y >= band.end {
            continue;
        }
        // 这一行盒的基线。`rects_for_range_in` 给的 `y` 就是行盒的 `line.y`，
        // 行盒按 y 单调，所以 `+0.5` 落回它自己那一行（`line_at_y`）。
        let line = &layout.lines[line_at_y(layout, rect.y + 0.5)];
        let descent = (rect.height - line.baseline).max(0.0);
        let underline_y = rect.y + line.baseline + descent * 0.618;
        // 那条 2 像素实线的中心。波浪那块更厚的盒子围绕同一个中心摆。
        let center = underline_y + UNDERLINE_THICKNESS * 0.5;
        out.push(Bounds::new(
            point(
                px(x + rect.x),
                px(center - thickness * 0.5 - origin_y + header),
            ),
            size(px(rect.width), px(thickness)),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::{note_icon_bounds, page_note_icon};
    use gpui_kit::{Bounds, point, px, size};

    /// 可重排那一族：便签图标落在那一行的**右端**，垂直落在行盒下缘之内，不越到下一行。
    ///
    /// 钉的是几何规则本身（右端 + 行底），不是「画没画出来」—— 那一层要真排版与一个带
    /// 批注的章才能问，这里给的是合成的一行盒。
    #[test]
    fn note_icon_sits_at_the_bottom_right_of_its_row() {
        let row = Bounds::new(point(px(24.0), px(100.0)), size(px(300.0), px(28.0)));
        // 324 = 那一行的右边界（排版给的，不是末字那一格的右边）。
        let icon = note_icon_bounds(324.0, row);
        assert_eq!(
            (f32::from(icon.origin.x), f32::from(icon.origin.y)),
            (312.0, 115.0),
            "右端与那一行对齐，顶 = 行底 128 − 12 − 1"
        );
        assert_eq!(
            (f32::from(icon.size.width), f32::from(icon.size.height)),
            (12.0, 12.0)
        );
        assert!(
            f32::from(icon.origin.y) + f32::from(icon.size.height)
                <= f32::from(row.origin.y) + f32::from(row.size.height),
            "图标整枚留在这一行盒之内，不许压到下一行的字"
        );
    }

    /// 定版式那一族：右端与行底之外，还要夹在页框里 —— 作者把页边距写成 0 时，行盒的右端
    /// 就是页框的右沿，图标整枚会骑出页外。
    #[test]
    fn fixed_note_icon_stays_inside_the_page_box() {
        let page = Bounds::new(point(px(0.0), px(0.0)), size(px(200.0), px(300.0)));
        let row = Bounds::new(point(px(0.0), px(280.0)), size(px(200.0), px(28.0)));
        let icon = page_note_icon(row, page);
        assert_eq!(
            f32::from(icon.origin.x),
            187.0,
            "右端顶到页框右沿，夹回页内 1px"
        );
        assert_eq!(
            f32::from(icon.origin.y),
            287.0,
            "行贴在页底：按行底摆是 295，越过页底那一档要夹成 300 − 12 − 1"
        );
        assert!(
            f32::from(icon.origin.y) + f32::from(icon.size.height) <= 300.0,
            "整枚留在页框里"
        );
    }
}
