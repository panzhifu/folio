//! 关于页要显示的那些事实：这份二进制是用哪几版依赖编出来的，以及「实际情况」那两份文档。
//!
//! 两件事都刻意不手写：
//!
//! - **版本与 commit** 由 `build.rs` 从 `Cargo.lock` 抄成编译期常量。手写的一份和锁文件
//!   迟早分叉，而分叉的恰好是最需要准的那几行。
//! - **开发状况**不重述 —— 直接把 `plans/README.md` 与 `plans/facts.md` 嵌进二进制。
//!   在 Rust 里再写一份「做到哪儿了」的清单，等于给分叉再添一处，
//!   而这一页存在的理由就是「看实际情况」。

/// 本应用版本（`Cargo.toml` 里那一份 workspace 版本）。
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// GPUI 本体（crates.io 上的 `gpui-pre` 快照）。
pub fn gpui_pre() -> &'static str {
    option_env!("FOLIO_GPUI_PRE").unwrap_or("未知")
}

/// 组件层 gpui-kit：版本号 + 用的那个 commit。
pub fn kit() -> &'static str {
    option_env!("FOLIO_KIT").unwrap_or("未知")
}

/// 移动平台层 gpui-pre-mobile（fork）：版本号 + 钉住的那个 commit。
pub fn mobile_layer() -> &'static str {
    option_env!("FOLIO_MOBILE").unwrap_or("未知")
}

/// 开发计划全文（架构、里程碑索引、roadmap）。
pub const PLAN: &str = include_str!("../../../../../plans/README.md");

/// 已钉死的事实全文（框架/平台/构建的实测约束）。
pub const LIMITATIONS: &str = include_str!("../../../../../plans/facts.md");
