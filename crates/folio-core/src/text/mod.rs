//! 文本域的**非渲染**部分：字体字节的名字表解析、家族登记表，以及（刀 2 之后）
//! 随书字体的加载管线。渲染（断行、行盒、注册进 `TextSystem`）仍在 app ——
//! 它们的边界见 [`crate::layout::Measure`] 与这里的 [`FontSink`]。

pub mod families;
pub mod load;
pub mod names;
pub mod scan;

pub use families::*;
pub use load::*;
pub use names::*;
