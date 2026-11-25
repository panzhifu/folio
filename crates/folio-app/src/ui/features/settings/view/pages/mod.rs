//! 十六个设置页（另有「调试」一页，debug 构建或 `debug-ui` 特性下才有）：每页一个文件，产出的是
//! [`super::super::model::spec`] 里的中立规格。
//!
//! 加一页动三处：新文件、这里一行、`super::SettingsPage` 里一个变体（顺序即侧栏顺序）。
//! 有条件存在的页还要跟着 `SettingsPage::ALL` 与 `spec_pages` 那两处 `cfg` 一起走。

pub(in crate::ui::features::settings) mod about;
pub(in crate::ui::features::settings) mod ai;
pub(in crate::ui::features::settings) mod appearance;
pub(in crate::ui::features::settings) mod backup;
#[cfg(any(debug_assertions, feature = "debug-ui"))]
pub(in crate::ui::features::settings) mod debug;
pub(in crate::ui::features::settings) mod dictionary;
pub(in crate::ui::features::settings) mod font_store;
pub(in crate::ui::features::settings) mod fonts;
pub(in crate::ui::features::settings) mod layout;
pub(in crate::ui::features::settings) mod library;
pub(in crate::ui::features::settings) mod membership;
pub(in crate::ui::features::settings) mod speech;
pub(in crate::ui::features::settings) mod stealth;
pub(in crate::ui::features::settings) mod storage;
pub(in crate::ui::features::settings) mod sync;
pub(in crate::ui::features::settings) mod toc;
pub(in crate::ui::features::settings) mod translation;
pub(in crate::ui::features::settings) mod txt_rules;
pub(in crate::ui::features::settings) mod typography;
