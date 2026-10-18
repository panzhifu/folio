//! 界面层。九块，依赖方向只有一条：
//!
//! `shell → features → components → services → foundation`
//!
//! - [`foundation`] —— 地基：主题、文案、错误约定、图片索引、颜色换算。无业务、无 IO、无实体。
//! - [`services`] —— 服务：配置实体、书库连接、导入队列、监听、传书。有生命周期，无元素树。
//! - [`components`] —— 跨纵切复用的组件（顶栏、搜索框、底部弹层、行/分组/空状态）。
//! - [`features`] —— 按能力纵切：每个功能自带 model / view / components / overlay。
//! - [`shell`] —— 外壳：自适应导航（Material 3）+ 每个目的地一个导航栈。
//! - [`host`] —— 平台服务边界（安全区、输入法、正文字体）。
//! - [`platform`] —— 入口与原生桥。
//!
//! 每一块的判据写在它自己的 `mod.rs` 里，搬文件之前先读那一条。
//!
//! `cfg(mobile)` **只允许出现在 [`host`] 与 [`platform`]**。阅读界面以前是第三个：桌面一套
//! 渲染、移动一套，现在两端共用逐字形那一份，平台之间只剩「导航栏占的是宽还是高」这种
//! 数据差别，它走 [`Reserve`](features::reader::Reserve) 传进去。
//! （历史遗留：还有几个文件带着 `cfg(mobile)`，见 `plans/目录架构.md` §十一 的清单，只许减不许增。）
//!
//! 两端用的是同一个 `gpui-pre` 快照（`gpui_kit` 顶层 `pub use ::gpui::*` 把它整个转出），
//! 所以视图统一 `use gpui_kit::…`，不直接引 `gpui`。

pub mod components;
pub mod foundation;
pub mod services;

pub mod features;
pub mod shell;

pub mod host;
pub mod platform;
