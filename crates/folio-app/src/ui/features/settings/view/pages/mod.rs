//! 九个设置页：每页一个文件，产出的是 [`super::super::model::spec`] 里的中立规格。
//!
//! 加一页动三处：新文件、这里一行、`super::SettingsPage` 里一个变体（顺序即侧栏顺序）。

pub(in crate::ui::features::settings) mod about;
pub(in crate::ui::features::settings) mod appearance;
pub(in crate::ui::features::settings) mod fonts;
pub(in crate::ui::features::settings) mod layout;
pub(in crate::ui::features::settings) mod library;
pub(in crate::ui::features::settings) mod speech;
pub(in crate::ui::features::settings) mod storage;
pub(in crate::ui::features::settings) mod toc;
pub(in crate::ui::features::settings) mod typography;
