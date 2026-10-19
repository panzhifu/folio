//! 各档的门共用的那几把尺：一块竖版心、数顶点、取纸面、判两条裁剪互不重叠。

use super::geometry::{Area, Cut, Poly};

/// 一块竖版心：400 × 600，左上角在原点。
pub(in crate::ui::features::reader) fn page() -> Area {
    Area::new(0.0, 0.0, 400.0, 600.0)
}

/// 起步与走完这两端各是什么，全靠这几个数说话。
pub(in crate::ui::features::reader) const W: f32 = 400.0;
pub(in crate::ui::features::reader) const H: f32 = 600.0;

/// 两张纸的裁剪必须**恰好互补**：任何一点只归一张画。两张都画 = 字叠字、那一帧字色深
/// 一档；两张都不画 = 那一块露出一片没有字的页底。两种都是这一层能犯的最坏的错，
/// 而且都不会让别处变红。
pub(in crate::ui::features::reader) fn complementary(top: Cut, bottom: Cut, area: Area) {
    for xi in 0..=10 {
        for yi in 0..=10 {
            let x = area.left + area.width() * xi as f32 / 10.0;
            let y = area.top + area.height() * yi as f32 / 10.0;
            assert_ne!(
                top.keeps(x, y),
                bottom.keeps(x, y),
                "({x:.0}, {y:.0}) 处两张纸{}：必须恰好一张画",
                if top.keeps(x, y) {
                    "都画"
                } else {
                    "都不画"
                }
            );
        }
    }
}

/// 一个多边形在横向上的跨度。
pub(in crate::ui::features::reader) fn x_span(poly: &Poly) -> (f32, f32) {
    let mut span = (f32::MAX, f32::MIN);
    poly.each(|x, _| span.0 = span.0.min(x));
    poly.each(|x, _| span.1 = span.1.max(x));
    span
}
