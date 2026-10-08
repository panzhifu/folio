//! Folio 的 gpui-kit 侧：应用壳、阅读视图与排版引擎的绘制/度量实现。
//!
//! 桌面模块与 [`mobile`] 模块按 target **互斥编译**（用 `cfg(mobile)`，由 `build.rs` 设），
//! 两者共用 `folio-core`（零 gpui 的那层）。
//!
//! 以前移动端是独立的 cargo workspace，理由是「移动用的 gpui 与桌面不是同一个包」——
//! 现在两端都是 `gpui-pre 0.3.8`，那条理由不成立，所以合成一个 crate、一张依赖图、
//! 一份 `Cargo.lock`。
//!
//! 作为库暴露模块，是为了让 `tests/` 里的 UI 集成测试能直接用生产视图，
//! 而不是在测试里另搭一套近似视图。

// ── 桌面专属 ──
#[cfg(not(mobile))]
pub mod account;
#[cfg(not(mobile))]
pub mod app;
#[cfg(not(mobile))]
pub mod config_model;
#[cfg(not(mobile))]
pub mod engine;
#[cfg(not(mobile))]
pub mod images;
#[cfg(not(mobile))]
pub mod l10n;
#[cfg(not(mobile))]
pub mod measure;
#[cfg(not(mobile))]
pub mod reader;
#[cfg(not(mobile))]
pub mod settings_view;
#[cfg(not(mobile))]
pub mod theme;

// ── 移动专属（Android / iOS / HarmonyOS）──
#[cfg(mobile)]
pub mod mobile;
