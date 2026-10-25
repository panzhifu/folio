//! 阅读器自己的组件：目录、阅读设置卡片、选中、便签、看图、分享卡。
//!
//! 判据与 `ui/components/` 相反：它们出现的是阅读器的词（`ReaderSession`、`PageRef`、
//! `Locator`），所以留在纵切里。参数里出现 `Entity<ReaderSession>` 是正常的 ——
//! 它们就是这一屏的一部分。

pub(in crate::ui::features::reader) mod image_viewer;
pub(in crate::ui::features::reader) mod note_editor;
pub(in crate::ui::features::reader) mod reading_sheet;
pub(in crate::ui::features::reader) mod selection;
pub(in crate::ui::features::reader) mod share_card;
pub(in crate::ui::features::reader) mod toc_panel;
