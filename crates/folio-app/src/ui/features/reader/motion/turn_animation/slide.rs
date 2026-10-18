//! 左右平移：两张纸同速反向走，读起来像一张长纸被抽过去。
//!
//! 两张纸的位移式子（[`slide_shifts`]）也住在这儿 —— 它是这一档的式子，从前寄在
//! `chrome.rs` 里，摆位层拆开后回到了它该在的地方。

use crate::ui::features::reader::model::chrome::PageMode;
use super::{Area, Axis, PageTurnStyle, Sheet};

/// 平移翻页里**两张纸各沿轴走多远**，按「这一趟走了多少」`progress` 算（0 = 刚起步，
/// 1 = 走完），给回 `(出场页, 进场页)`。这一层只管这一个数：分成 `x` / `y`、要不要让
/// 出场那张真的走，都是摆位层每一档自己的事。
///
/// 两张**同速**、并且始终相距 `dist`，读起来才是一条纸带整体平移；两张走同样的量而
/// 间距会变，就变成了两张卡片各自飞。进场那张起步时在阅读方向的前方外 `dist` 处
/// （`progress` = 0 ⇒ 位移 = `±dist`），走完回到原位（`progress` = 1 ⇒ 位移 = 0）；
/// 出场那张正好相反，走完走满 `dist` 出画。
///
/// 方向接反了**不报错**，画面是「下一页从左边滑进来」—— 所以这一句抽成纯函数、
/// 并且在这里钉住方向，与 `ui::shell::nav_offset` 那一条同一取舍：宿主测试只有在
/// 这一层才摸得到位移。
pub(in crate::ui::features::reader) fn slide_shifts(
    progress: f32,
    dist: f32,
    forward: bool,
) -> (f32, f32) {
    let dir = if forward { 1.0 } else { -1.0 };
    (-dir * dist * progress, dir * dist * (1.0 - progress))
}

/// 左右平移：两张纸同速反向、始终相距一屏，读起来是一条纸带整体被抽过去。
/// 式子在 [`slide_shifts`]，下面那几条门钉着「同速 + 恒距 + 方向」。
pub(in crate::ui::features::reader) struct Slide;

impl PageTurnStyle for Slide {
    fn axis(&self) -> Axis {
        Axis::Horizontal
    }

    fn animates(&self, _: PageMode) -> bool {
        true
    }

    fn sheets(&self, t: f32, forward: bool, area: Area) -> [Sheet; 2] {
        let dist = area.distance(self.axis());
        let (leaving, incoming) = slide_shifts(t, dist, forward);
        [
            Sheet {
                offset: (incoming, 0.0),
                z: 0,
                cut: None,
            },
            Sheet {
                offset: (leaving, 0.0),
                z: 0,
                cut: None,
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    //! 一挡的门跟着那一档走；共用的那几把尺在 `testing.rs`。

    use super::{Slide, slide_shifts};
    use crate::ui::features::reader::motion::turn_animation::PageTurnStyle as _;
    use crate::ui::features::reader::motion::turn_animation::testing::{W, page};

    /// 横档只沿 x 走，起步时进场那张整页停在阅读方向的前方外。
    #[test]
    fn a_horizontal_style_moves_only_along_x() {
        let [incoming, outgoing] = Slide.sheets(0.0, true, page());
        assert_eq!(incoming.offset, (W, 0.0), "起步：新页该在右外侧");
        assert_eq!(outgoing.offset, (0.0, 0.0), "起步：旧页还占着屏");
        let [incoming, _] = Slide.sheets(1.0, true, page());
        assert_eq!(incoming.offset, (0.0, 0.0), "走完：新页落回原位");
        // 往前翻是同一趟的反号。
        let [incoming, _] = Slide.sheets(0.0, false, page());
        assert_eq!(incoming.offset, (-W, 0.0));
    }

    /// 平移的两张**同速**：t = 0.5 时各走半屏，间距仍然是一屏。这条与下面那条
    /// [`slide_shifts`] 的门对上两头 —— 中间这一层没人钉的话，换成「两张各走自己的
    /// 比例」不会有任何一行代码报错。
    #[test]
    fn the_two_sheets_of_a_slide_stay_one_screen_apart() {
        for step in 0..=10 {
            let t = step as f32 / 10.0;
            let [incoming, outgoing] = Slide.sheets(t, true, page());
            let gap = incoming.offset.0 - outgoing.offset.0;
            assert_eq!(gap, W, "走了 {t} 之后两张不再相距 {W}");
        }
    }

    /// 平移翻页的两张纸各走多远。接反了不报错，画面是「下一页从左边滑进来」，
    /// 而位移只有宿主这一层摸得到 —— 所以钉在这里。
    #[test]
    fn the_two_pages_slide_apart_at_the_same_speed() {
        let dist = 400.0;

        // 起步：出场那张还在原位，进场那张停在阅读方向的前方外一整个 `dist`。
        assert_eq!(slide_shifts(0.0, dist, true), (0.0, dist));
        // 走完：进场那张落回原位，出场那张走满 `dist` 出画。
        assert_eq!(slide_shifts(1.0, dist, true), (-dist, 0.0));

        // 中途两张始终相距 `dist`（同速 = 一条纸带），间距变了就是两张卡片各自飞。
        for step in 0..=10 {
            let progress = step as f32 / 10.0;
            let (leaving, incoming) = slide_shifts(progress, dist, true);
            assert_eq!(
                incoming - leaving,
                dist,
                "走了 {progress} 之后两张不再相距 {dist}"
            );
        }

        // 进场那张**单调靠近**原位：读数越大越往右 = 往回退着走，方向接反的现场。
        let mut last = dist;
        for step in 0..=10 {
            let progress = step as f32 / 10.0;
            let (_, incoming) = slide_shifts(progress, dist, true);
            assert!(
                incoming <= last,
                "往后翻时进场那一页在 {progress} 反而更往右了"
            );
            last = incoming;
        }

        // 往前翻整对符号反过来：进场从左边来，出场往右边退。
        assert_eq!(slide_shifts(0.0, dist, false), (0.0, -dist));
        assert_eq!(slide_shifts(1.0, dist, false), (dist, 0.0));

        // 零距离（窗口还没量过）不该长出 NaN 或半张纸。
        assert_eq!(slide_shifts(0.5, 0.0, true), (0.0, 0.0));
    }
}
