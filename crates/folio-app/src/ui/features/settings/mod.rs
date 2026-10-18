//! 阅读设置：一页一个文件，桌面与手机共用同一份「页面规格」。
//!
//! - [`model`] —— 中立规格：两端共用的唯一数据源（见 `spec.rs` 头部）；
//! - [`view`] —— 两份界面：桌面整页（`desktop`）、手机的钻入式紧凑列表（`compact`），
//!   以及七个页面文件（`pages/`）。它们是同一个配置的两个面子，不是两套数据。
//!
//! 挂到哪儿由 `shell` 决定：这一组页面是主窗口里的一个目的地，不是独立窗口。

mod model;
mod view;

pub use view::{SettingsPage, SettingsView};
pub(crate) use view::CompactSettings;
