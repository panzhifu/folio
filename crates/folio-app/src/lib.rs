//! Folio 的 gpui-kit 侧：应用壳、阅读视图与排版引擎的绘制/度量实现。
//!
//! 全部界面代码在 [`ui`] 下面，按「两端共用 / 桌面 / 移动」分三层；
//! 桌面模块与 [`ui::mobile`] 按 target **互斥编译**（用 `cfg(mobile)`，由 `build.rs` 设），
//! 两端共用的那层依赖 `gpui_kit` 与 `folio-core`（零 gpui 的那层）。
//!
//! 以前移动端是独立的 cargo workspace，理由是「移动用的 gpui 与桌面不是同一个包」——
//! 现在两端都是同一个 `gpui-pre` 快照（`gpui_kit` 顶层把它整个转出），那条理由不成立，
//! 所以合成一个 crate、一张依赖图、一份 `Cargo.lock`，度量与排版状态也合并成一份实现。
//!
//! 作为库（而不是只有 `main.rs`）存在，是因为移动宿主要 dlopen 这份 cdylib、
//! 让 Xcode 链那份 staticlib（见 `Cargo.toml` 的 `crate-type`），而桌面的 `main.rs`
//! 只是同一层代码的另一个入口。测试不再需要从外面引用生产视图：它们内联在各模块的
//! `#[cfg(test)] mod tests` 里，跑的就是生产视图本身。

pub mod ui;
