//! 上下平移：同一趟沿竖轴走，像把一叠纸往上抽。

/// 上下平移：同一趟沿竖轴走，像把一叠纸往上抽。
use super::slide::slide_shifts;
use super::{Area, Axis, PageTurnStyle, Sheet};
use super::super::chrome::PageMode;

pub(in crate::ui::features::reader) struct SlideUp;

impl PageTurnStyle for SlideUp {
    fn axis(&self) -> Axis {
        Axis::Vertical
    }

    fn animates(&self, _: PageMode) -> bool {
        true
    }

    fn sheets(&self, t: f32, forward: bool, area: Area) -> [Sheet; 2] {
        let dist = area.distance(self.axis());
        let (leaving, incoming) = slide_shifts(t, dist, forward);
        [
            Sheet {
                offset: (0.0, incoming),
                z: 0,
                cut: None,
            },
            Sheet {
                offset: (0.0, leaving),
                z: 0,
                cut: None,
            },
        ]
    }
}


#[cfg(test)]
mod tests {
    //! 搬自摆位层那一份：一挡的门跟着那一档走，共用的尺在 `testing.rs`。

    use crate::ui::features::reader::turn_animation::PageTurnStyle as _;
    use crate::ui::features::reader::turn_animation::slide_up::{SlideUp};
    use crate::ui::features::reader::turn_animation::testing::{page, H};

    /// 竖档把同样一趟搬到 y 上：x 分量必须恒为 0，否则读起来就是斜着飞。
    #[test]
    fn a_vertical_style_moves_only_along_y() {
        for t in [0.0, 0.5, 1.0] {
            for forward in [true, false] {
                let [incoming, outgoing] = SlideUp.sheets(t, forward, page());
                assert_eq!(
                    incoming.offset.0, 0.0,
                    "竖档不该在 x 上留位移：{incoming:?}"
                );
                assert_eq!(outgoing.offset.0, 0.0, "出场那张也一样：{outgoing:?}");
            }
        }
        let [incoming, _] = SlideUp.sheets(0.0, true, page());
        assert_eq!(incoming.offset, (0.0, H));
    }
}
