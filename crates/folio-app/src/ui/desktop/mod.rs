//! 桌面界面层：窗口外壳、阅读视图、引擎结果的虚拟列表接法、设置页集。
//!
//! 这一层可以放任意 `gpui_kit::component` 的组件 —— 侧栏、标签栏、工具条、状态栏、
//! 对话框、`Settings` 容器 —— 因为桌面端就是这套壳的目标平台。
//! 需要在手机上同样成立的东西不放这里，放 [`super::shared`]。
//!
//! 模块划分按「谁拥有这块屏幕」：[`app`] 是窗口根与外壳，[`reader`] 是一本书的阅读视图，
//! [`engine`] 只负责把已经排好的行摆进列表，[`settings`] 是一页一个文件的设置页集。

pub mod app;
pub mod engine;
pub mod reader;
pub mod settings;
