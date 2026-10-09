//! Folio 的书本模型、格式解析、标注锚点与存储层。
//!
//! 这里不放任何 UI 代码，也不引入 `gpui`。

// 界面文案表。`locales/` 下一门语言一份 TOML（文件名去掉扩展名就是语言标签），编译期嵌进来，
// 取文案走 `i18n::t` / `i18n::t_with`；
// `fallback = "en"` 与 gpui-kit 组件层一致，这样缺一条日文文案时框架和我们回落到同一种语言。
rust_i18n::i18n!("locales", fallback = "en");

pub mod anchor;
pub mod config;
pub mod css;
pub mod dom;
pub mod error;
pub mod format;
pub mod i18n;
pub mod layout;
pub mod paths;
pub mod position;
pub mod store;

pub use anchor::{
    Confidence, Relocated, TextAnchor, TextMap, line_at_offset, line_at_y, line_chars, line_offset,
    relocate,
};
pub use config::{Account, Appearance, Config, Language, Library, Mode, Reader};
pub use error::{Error, Result};
pub use format::{Chapter, Content, DocMeta, FlowDoc, Format, open};
pub use i18n::Locale;
pub use paths::{config_dir, config_path, data_dir, db_path, themes_dir};
pub use position::Locator;
pub use store::{ANNOTATION_COLORS, Annotation, SearchHit, Store};
