//! 不走动画：当场落定，也就不必为这一帧多摆一张纸。

/// 不走动画：落定就在这一帧。它 backing [`PageTurn::None`]，存在的理由见
/// [`folio_core::PageTurn`] 那条 —— 系统级的减动效位在这套 gpui 上没有任何生产者会
/// 置真，所以「关掉」必须是应用自己的一档。
use super::{Area, Axis, PageTurnStyle, Sheet};
use super::super::chrome::PageMode;

pub(in crate::ui::features::reader) struct Immediate;

impl PageTurnStyle for Immediate {
    fn axis(&self) -> Axis {
        // 没有一趟要走，这一格不被读到；给横轴只为让它有个确定的答案。
        Axis::Horizontal
    }

    fn animates(&self, _: PageMode) -> bool {
        false
    }

    fn sheets(&self, _: f32, _: bool, _: Area) -> [Sheet; 2] {
        [
            Sheet {
                offset: (0.0, 0.0),
                z: 0,
                cut: None,
            },
            Sheet {
                offset: (0.0, 0.0),
                z: 0,
                cut: None,
            },
        ]
    }
}
