//! 上下滚动：不摆第二张纸，翻页就是画出来的位置补间过去。
//!
//! 只有连续滚动摆法成立（判据在 [`applies`](super::applies)）。

/// 上下滚动：不摆第二张纸，翻页就是**画出来的那个位置**补间过去 —— 与连续滚动那一臂
/// 同一条路，只是把它搬到分页上。
///
/// 只有连续摆法有意义（那里本来就在补间一个 y）。分页/双页里它退化成「当场落定」：
/// 没有可滚的东西，也没有第二张纸可摆 —— 所以 [`PageTurnStyle::animates`] 要看摆法。
/// 底栏那一格按当前摆法把它筛掉，读者不会手动撞进这个退化态。
use super::{Area, Axis, PageTurnStyle, Sheet};
use crate::ui::features::reader::model::chrome::PageMode;

pub(in crate::ui::features::reader) struct Scroll;

impl PageTurnStyle for Scroll {
    fn axis(&self) -> Axis {
        Axis::Vertical
    }

    fn animates(&self, mode: PageMode) -> bool {
        mode.is_continuous()
    }

    fn sheets(&self, _: f32, _: bool, _: Area) -> [Sheet; 2] {
        // 这一档不摆第二张纸：分页那一臂拿不到这两格，连续那一臂只看补间出来的 y。
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
