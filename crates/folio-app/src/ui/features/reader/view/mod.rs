//! 元素树：把会话的状态摆成元素。
//!
//! 这一层只组合（读 `model` 的状态、拼 `components` 的件），除了滚动条、hover、
//! 输入框内容这类瞬时状态，不持有业务状态。碰 `TextSystem` 的 `measure` 也在这里
//! ——量字是绘制的另一半。

pub(in crate::ui::features::reader) mod bands;
pub(in crate::ui::features::reader) mod body;
pub(in crate::ui::features::reader) mod paint;
pub(in crate::ui::features::reader) mod reading_page;
pub(in crate::ui::features::reader) mod regions;
pub(in crate::ui::features::reader) mod scene;

pub use reading_page::ReadingPage;
