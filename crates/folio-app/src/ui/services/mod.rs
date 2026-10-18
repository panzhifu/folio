//! 服务：应用级单例与后台管线。
//!
//! 这一层与 [`super::foundation`] 的差别是**生命周期**：地基里的是纯函数和常量，这里每个
//! 模块都持着活的东西 —— 配置实体、书库连接、导入作业池、文件夹监听、传书服务器。
//! 它们可能在后台线程上活着，界面只是拿一个句柄去问或去改。
//!
//! 这里**不出现元素树**（没有 `AnyElement`、没有 `impl Render`）：服务不知道谁在看它。

pub mod app_store;
pub mod config;
pub mod import_queue;
pub mod l10n;
pub mod library;
pub mod services;
pub mod transfer;
pub mod watch;
