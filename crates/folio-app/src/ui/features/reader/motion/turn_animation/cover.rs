//! 左右覆盖：旧页停在原地，新页整张盖上来。
//!
//! 与平移之间**唯一**看得见的差别是那两条影子：位移只差在停不停那一张，而停着的那一张
//! 没有影子 —— 没有它们，这一档在画面上与平移分不开。

use crate::ui::features::reader::model::chrome::PageMode;
use super::{Area, Axis, Cut, PageTurnStyle, Poly, Shades, Sheet};
use super::geometry::{edge_pair, travel_edge};

/// 覆盖那一档：移动那张纸前缘投在下面那张上的带有多宽（按页宽）。
pub(in crate::ui::features::reader) const SLIDE_WIDE: f32 = 0.08;

/// 那条投影有多黑。定值、不随进度：前缘走到页的左缘时投影自己被裁没了，
/// 不需要再乘一个 `(1 − t)`。
pub(in crate::ui::features::reader) const SLIDE_INK: f32 = 0.18;



/// 覆盖那一档两张纸各占哪一块：`[压在上面那张, 被压住那张]`。
///
/// 版心还没量出来时给 `[None, None]` —— 那一刻 `travel_edge` 会算出一条把整页一劈两半的
/// 假边界，两张纸各丢一半字。
fn cover_cuts(progress: f32, forward: bool, area: Area) -> [Option<Cut>; 2] {
    if area.width() <= 0.0 || area.height() <= 0.0 {
        return [None, None];
    }
    let edge = travel_edge(progress, forward, area);
    [Some(Cut::new(edge, true)), Some(Cut::new(edge, false))]
}

/// 覆盖那一档要盖上去的那几块：前缘投影 + 前缘纸边光。
///
/// 这两块就是「覆盖」与「平移」之间**唯一**看得见的差别：位移只差在停不停那一张，
/// 而停着的那一张没有影子 —— 没有这两条，`Cover` 在画面上与 `Slide` 分不开。
fn cover_shades(progress: f32, forward: bool, area: Area) -> Shades {
    let mut out = Shades::new();
    if area.width() <= 0.0 || area.height() <= 0.0 {
        return out;
    }
    let page = Poly::rect(area);
    let edge = travel_edge(progress, forward, area);
    edge_pair(page, edge, SLIDE_WIDE * area.width(), SLIDE_INK, &mut out);
    out
}

/// 左右覆盖：旧页停在原地不动，新页整张盖上来 —— 与 `Slide` 的差别就在出场那张不走，
/// 因此新页**必须后画**（`z` 高一档），否则旧页压着新页，读起来还是平移。
///
/// 两张纸都带裁剪，而且两侧严格互补：新页只画它自己已经占到的那一条，旧页只画还没被
/// 盖住的那一条。少了这一对，「盖上来」在画面上就是**两页的字叠在一起** —— 字形共用一个
/// 元素、谁也没有自己的底色（见 [`Cut`]），所以盖住必须靠裁，不能靠画在上面那张去挡。
pub(in crate::ui::features::reader) struct Cover;

impl PageTurnStyle for Cover {
    fn axis(&self) -> Axis {
        Axis::Horizontal
    }

    fn animates(&self, _: PageMode) -> bool {
        true
    }

    fn sheets(&self, t: f32, forward: bool, area: Area) -> [Sheet; 2] {
        let dist = area.distance(self.axis());
        // 进场那张从阅读方向的前方走到原位：就是 `slide_shifts` 的第二个返回值，
        // 只是不再让出场那张跟着走。
        let dir = if forward { 1.0 } else { -1.0 };
        let [top, bottom] = cover_cuts(t, forward, area);
        [
            Sheet {
                offset: (dir * dist * (1.0 - t), 0.0),
                z: 1,
                cut: top,
            },
            Sheet {
                offset: (0.0, 0.0),
                z: 0,
                cut: bottom,
            },
        ]
    }

    fn shades(&self, t: f32, forward: bool, area: Area) -> Shades {
        cover_shades(t, forward, area)
    }
}


#[cfg(test)]
mod tests {
    //! 搬自摆位层那一份：一挡的门跟着那一档走，共用的尺在 `testing.rs`。

    use crate::ui::features::reader::motion::turn_animation::cover::{Cover};
    use crate::ui::features::reader::motion::turn_animation::geometry::{Area, Tint, EDGE_WIDE};
    use crate::ui::features::reader::motion::turn_animation::testing::{page, complementary, x_span, W};
    use crate::ui::features::reader::motion::turn_animation::PageTurnStyle as _;

    /// 覆盖档的差别就两格：旧页停着不动，新页压在它上面。少任何一条，`Cover` 与
    /// `Slide` 就是一模一样的画面。
    #[test]
    fn a_cover_leaves_the_old_page_put_and_on_top() {
        for t in [0.0, 0.3, 1.0] {
            let [incoming, outgoing] = Cover.sheets(t, true, page());
            assert_eq!(outgoing.offset, (0.0, 0.0), "旧页该停在原地：t = {t}");
            assert!(
                incoming.z > outgoing.z,
                "新页必须后画，否则旧页盖着它：t = {t}"
            );
        }
        let [incoming, _] = Cover.sheets(0.0, true, page());
        assert_eq!(incoming.offset, (W, 0.0));
        let [incoming, _] = Cover.sheets(0.0, false, page());
        assert_eq!(incoming.offset, (-W, 0.0), "往前翻时新页从左边盖进来");
    }
    /// 覆盖那一档的两块影子必须**贴着这一帧那张纸真实的前缘** —— 而那个前缘是
    /// [`PageTurnStyle::sheets`] 给出去的位移算出来的，不是这里另算一遍。
    ///
    /// 这条就是旧版那个差一个页边距的毛病的反面：上一版阴影按 `margin + seam.at` 摆、
    /// 裁剪按 `seam.at` 判，两处各拿一把尺，看着就是「影子跑在纸前面一截」。现在两块
    /// 都从同一个 `offset` 长出来，谁改了另一条就红。
    #[test]
    fn the_cover_shadow_sticks_to_the_moving_edge() {
        for t in [0.2, 0.5, 0.8] {
            for forward in [true, false] {
                let area = page();
                let items = Cover.shades(t, forward, area).items().to_vec();
                let [incoming, _] = Cover.sheets(t, forward, area);
                let cast = &items[0];
                let edge = &items[1];
                assert_eq!(cast.tint, Tint::Dark, "第一块是投在下面那张上的");
                assert_eq!(edge.tint, Tint::Light, "第二块是那张纸自己的边光");
                let (low, high) = x_span(&cast.shape);
                // 前缘在屏幕上的横坐标：往后翻是那张纸的左缘，往前翻是它的右缘。
                let lead = if forward {
                    incoming.offset.0
                } else {
                    incoming.offset.0 + area.width()
                };
                if forward {
                    assert!(
                        (high - lead).abs() < 1e-3,
                        "t = {t} 往后翻：投影的外缘该正好压在纸的前缘 {lead} 上，实得 {high}"
                    );
                    assert!(low < high, "投影该往纸前来时的方向展开");
                } else {
                    assert!(
                        (low - lead).abs() < 1e-3,
                        "t = {t} 往前翻：投影该在纸前进的那一侧，从 {lead} 起，实得 {low}"
                    );
                    assert!(high > low);
                }
                // 纸边光必须落在**纸里面**，紧挨着前缘，不与投影重叠。
                let (inner_low, inner_high) = x_span(&edge.shape);
                let inside = if forward {
                    inner_low >= lead - 1e-3 && inner_high <= lead + EDGE_WIDE + 1e-3
                } else {
                    inner_high <= lead + 1e-3 && inner_low >= lead - EDGE_WIDE - 1e-3
                };
                assert!(
                    inside,
                    "t = {t} forward = {forward}：纸边光该贴在纸内侧那 {EDGE_WIDE} 像素里，实得 [{inner_low}, {inner_high}]，前缘 {lead}"
                );
            }
        }
    }
    #[test]
    fn a_cover_never_stacks_two_pages_of_text() {
        for forward in [true, false] {
            for step in 1..10 {
                let t = step as f32 / 10.0;
                let area = page();
                let [top, bottom] = Cover.sheets(t, forward, area);
                let top_cut = top.cut.expect("盖在上面那张只画自己占的那一条");
                let bottom_cut = bottom.cut.expect("被盖住那张只画还没被盖住的那一条");
                complementary(top_cut, bottom_cut, area);
                // 前缘在屏幕上的横坐标，与那张纸摆出去的位置同源。
                let lead = if forward {
                    top.offset.0
                } else {
                    top.offset.0 + area.width()
                };
                // 前缘之内（那张纸占的地方）归它，之外归旧页。
                let within = if forward { lead + 1.0 } else { lead - 1.0 };
                let without = if forward { lead - 1.0 } else { lead + 1.0 };
                assert!(
                    top_cut.keeps(within, 300.0) && !bottom_cut.keeps(within, 300.0),
                    "t = {t} forward = {forward}：前缘 {lead} 之内该归新页，实得 x = {within}"
                );
                assert!(
                    bottom_cut.keeps(without, 300.0) && !top_cut.keeps(without, 300.0),
                    "t = {t} forward = {forward}：前缘 {lead} 之外该留着旧页的字"
                );
            }
        }
        // 版心量不出来时两张纸都不能裁 —— 否则那条假边界会把整页字一劈两半。
        let flat = Area::new(0.0, 0.0, 0.0, 0.0);
        for sheet in Cover.sheets(0.5, true, flat) {
            assert_eq!(sheet.cut, None, "量不出面积时两张纸都整张画");
        }
    }
}
