//! 跨纵切复用的组件。
//!
//! 判据：**重复出现的领域模式**。一个组件只要开始出现某一纵切的词（`ReaderSession`、
//! `ShelfItem`…），它就搬回那个纵切自己的 `components/`；反过来，两个以上纵切各写一遍的
//! 东西（顶栏、搜索框、底部弹层、行/分组/空状态）留在这里。
//!
//! 这里的一切都不许 `use crate::ui::features::…`：组件不知道有哪几个功能。

mod access_question;
mod bottom_sheet;
mod font_picker;
mod gate_banner;
mod pill_slider;
mod primitives;
mod reading_theme_picker;
mod scroll_guard;
mod search_box;
mod top_bar;

pub use access_question::{access_question, waiting_for};
pub use bottom_sheet::{
    BottomSheet, BottomSheetEvent, BottomSheetState, any_sheet_open, dismiss_top,
};
pub use font_picker::{FontFamilySetter, FontPicker};
pub use gate_banner::gate_banner;
pub use pill_slider::{PillMark, PillSlider};
pub use primitives::{empty_state, muted_label};
pub use reading_theme_picker::ReadingThemePicker;
pub use scroll_guard::{ScrollShields, scroll_guard};
pub use search_box::{SEARCH_SETTLE_FRAMES, SearchBox, SearchBoxEvent, SearchDebounce};
pub use top_bar::TopBar;
