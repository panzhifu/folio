//! 阅读功能：一套渲染、三种摆法。
//!
//! 正文由 [`paint`] 逐字形画出来（[`paint::GlyphsElement`]），排版在
//! [`crate::ui::shared::document`]，「这一屏摆什么、点在哪一区、装不下双页怎么办」
//! 这些判断在 [`chrome`]。
//!
//! 这里**没有 `cfg`**：以前桌面是「`TextView` + 虚拟列表」、移动是「自绘字形」两套实现，
//! 同一本书的行走两条不同的路；合成一套之后，平台上只剩两件事不同，而两件都不在这一层 ——
//!
//! - 外壳占的是宽还是高：[`Reserve`] 两个轴都带着，谁用哪个由模式自己决定。
//! - 正文家族在这台设备上解析不解析得出来：`crate::ui::host`。
//!
//! 分层：
//!
//! - [`chrome`] —— 纯判断，无 gpui，两端都编，宿主测试直接跑。
//! - [`paint`] —— 字形级绘制元素（`shape_line` / `paint_glyph`）。
//! - [`view`] —— 一本书的一屏：切片、摆行、滚动与翻页、位置落盘。
//! - [`page`] —— 被外壳 push 进书架栈的那一屏：干净态两条小字 + 浮出的顶栏与底栏。

mod chrome;
mod page;
mod paint;
mod view;

pub use chrome::Reserve;
pub use page::ReadingPage;
pub use view::ReaderView;
