//! 界面层的目录约定：`shared` 是两端共用的地基，`desktop` 与 `mobile` 按 target 互斥编译。
//!
//! 分层只有一条判据：**这段代码在两个平台上需不需要长得不同**。
//!
//! - [`shared`] —— 配置实体、文案、主题、字体度量、「一章 → `Layout`」这类两端必须
//!   逐字一致的东西。这里出现 `cfg(mobile)` 就说明分层没做完。
//! - [`desktop`] —— 桌面壳：侧栏、标签、工具条、状态栏、设置窗口，以及 `gpui_kit::component`
//!   的虚拟列表接法。
//! - [`mobile`] —— 移动端壳：平台入口、底部导航栏、自绘字形元素。
//!
//! 两端用的是**同一个 `gpui-pre` 快照**（`gpui_kit` 顶层 `pub use ::gpui::*` 把它整个转出），
//! 所以视图代码统一 `use gpui_kit::…`，不直接引 `gpui` —— 那正是这些模块能共用的原因。
//! 以前移动端是独立 workspace、只能链 `gpui-pre` 原语，才不得不自己拼导航栏、
//! 另写一份度量；那条理由现在不成立了。

pub mod shared;

// ── 桌面专属 ──
#[cfg(not(mobile))]
pub mod desktop;

// ── 移动专属（Android / iOS / HarmonyOS）──
#[cfg(mobile)]
pub mod mobile;
