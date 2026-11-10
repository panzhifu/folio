//! 文本域的**非渲染**部分：字体字节的名字表解析、家族登记表，以及随书字体的
//! 加载管线。渲染（断行、行盒、注册进 `TextSystem`）仍在 app —— 它们的边界见
//! folio-render 的 `Measure` 与这里的 [`FontSink`]。原住 folio-core::text
//! （2026-11-12 阶段五拆分原样搬来，「换 crate 不换形状」：core 经模块别名沿用旧路径）。

pub mod families;
pub mod load;
pub mod names;
pub mod scan;

pub use families::*;
pub use load::*;
pub use names::*;
