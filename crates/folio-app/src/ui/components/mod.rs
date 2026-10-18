//! 跨纵切复用的组件。
//!
//! 判据：**重复出现的领域模式**。一个组件只要开始出现某一纵切的词（`ReaderSession`、
//! `ShelfItem`…），它就搬回那个纵切自己的 `components/`；反过来，两个以上纵切各写一遍的
//! 东西（顶栏、搜索框、底部弹层、行/分组/空状态）留在这里。
//!
//! 这里的一切都不许 `use crate::ui::features::…`：组件不知道有哪几个功能。

mod bottom_sheet;
mod primitives;
mod search_box;
mod top_bar;

pub use bottom_sheet::{BottomSheet, BottomSheetEvent, BottomSheetState};
pub use primitives::{empty_state, hairline, muted_label};
pub use search_box::{SearchBox, SearchBoxEvent};
pub use top_bar::TopBar;
