//! Folio 的书本模型、格式解析、标注锚点与存储层。
//!
//! 这里不放任何 UI 代码，也不引入 `gpui`。

pub mod css;
pub mod dom;
pub mod error;
pub mod format;
pub mod layout;
pub mod position;
pub mod prefs;
pub mod store;

pub use error::{Error, Result};
pub use format::{Chapter, Content, DocMeta, FlowDoc, Format, open};
pub use position::Locator;
pub use prefs::{Prefs, ThemeName};
pub use store::Store;
