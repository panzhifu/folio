//! 批注痕迹的绘制原语：高亮/实线是矩形，字下波浪与便签图标走矢量路径。
//!
//! 从 `paint.rs` 抽出来只因为那份文件贴着行数基线；几何与颜色仍由 `view/body/marks.rs`
//! 算，这里只负责把一条 `Mark` 画到窗口上。

use gpui_kit::{Bounds, Hsla, PathBuilder, Pixels, Point, Window, fill, point, px};

/// 留在书上的一条痕迹在这一帧的形状。三档画法（`folio_core::AnnotationStyle`）到这里归成
/// 两件事：**一块实心矩形**（高亮带、字下实线）或**一条要现铺的波浪**。
///
/// 为什么不是「一屏一条 enum」：高亮带与实线在绘制上是同一句 `paint_quad`，差的只是那块
/// 矩形多高、压多少透明度 —— 那是几何那一层（`view/body.rs`）算出来的事。只有波浪线需要
/// 这一层多做一次路径构造，所以分叉只分在这里。
#[derive(Clone)]
pub(in crate::ui::features::reader) enum Mark {
    /// 一块实心矩形，坐标与 `runs` 同一套（元素局部）。高亮带与实线都走这一格。
    Quad(Bounds<Pixels>, Hsla),
    /// 一条波浪线。`bounds` 是它的**外盒**：左端、中线所在的 y、长，高 = 峰峰值
    /// （上下各一半）。起伏怎么排归这一层（见 [`wave_half_waves`]），落点归几何那一层。
    Wave(Bounds<Pixels>, Hsla),
    /// 带文字笔记那一条的**便签图标**：落在这一片标记的左下角（末行的左端、行盒下缘之内），
    /// 表明这一段下面还有读者的笔记。`bounds` 是图标的外盒，颜色取那条批注的实色（不压透明度）。
    Note(Bounds<Pixels>, Hsla),
}

/// 画一条痕迹。`origin` 是承载它的元素在窗口里的原点 —— 矩形是元素局部坐标，
/// 漏加这一偏移，痕迹就整片飘到文字上方（把手准、颜色偏，就是它）。
///
/// 可重排一族把它压在字形下（高亮带不能吃掉笔画），定版式一族把它压在页图上
/// （那一族的字就在栅格图里，压在图下等于没画）。两层的几何各算各的矩形，画法共用这一份。
pub(in crate::ui::features::reader) fn paint_mark(
    mark: &Mark,
    origin: Point<Pixels>,
    window: &mut Window,
) {
    match mark {
        Mark::Quad(rect, color) => window.paint_quad(fill(*rect + origin, *color)),
        Mark::Wave(rect, color) => paint_wave(*rect + origin, *color, window),
        Mark::Note(rect, color) => paint_note(*rect + origin, *color, window),
    }
}

/// 一枚小小的「便签纸」：方框、右上折角、两条字线。**纯矢量**，不依赖字体里有没有
/// 那个符号；底色压得很淡，压在字或页图上时不抢眼，但一眼能认出「这里带笔记」。
fn paint_note(bounds: Bounds<Pixels>, color: Hsla, window: &mut Window) {
    let x = f32::from(bounds.origin.x);
    let y = f32::from(bounds.origin.y);
    let w = f32::from(bounds.size.width);
    let h = f32::from(bounds.size.height);
    window.paint_quad(fill(bounds, color.opacity(0.14)));
    let pad = 1.6;
    let fold = (w * 0.32).max(2.0);
    let mut path = PathBuilder::stroke(px(1.2));
    path.move_to(point(px(x + pad), px(y + pad)));
    path.line_to(point(px(x + w - fold), px(y + pad)));
    path.line_to(point(px(x + w - pad), px(y + fold)));
    path.line_to(point(px(x + w - pad), px(y + h - pad)));
    path.line_to(point(px(x + pad), px(y + h - pad)));
    path.close();
    // 折角的两笔：从折点往下、再折回纸内。
    path.move_to(point(px(x + w - fold), px(y + pad)));
    path.line_to(point(px(x + w - fold), px(y + fold)));
    path.line_to(point(px(x + w - pad), px(y + fold)));
    for at in [0.52, 0.72] {
        let line_y = y + h * at;
        path.move_to(point(px(x + pad + 1.8), px(line_y)));
        path.line_to(point(px(x + w - pad - 1.8), px(line_y)));
    }
    if let Ok(built) = path.build() {
        window.paint_path(built, color);
    }
}

/// 波浪线的半波数，永远是偶数：每条线起于中线、终于中线，一排并看收尾才齐。
/// 段数按整波算，等宽分段即轻微拉伸波长，不截断；`0` 表示窄到无线可画。
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
pub(in crate::ui::features::reader) const WAVE_LENGTH: f32 = 7.0;
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
