//! 阅读功能：一套渲染、三种摆法。
//!
//! 四层，判据在各自的 `mod.rs` 里：
//!
//! - [`model`] —— 状态与状态迁移：一本书的章节表、一章的排版、翻页决策、手势折算、
//!   位置换算、批注与脚注的落点。不建元素树。
//! - [`view`] —— 元素树：把会话的状态摆成元素（逐字形绘制、图片绝对定位、滚轮监听）。
//! - [`components`] —— 本纵切自己的组件：目录、选中、便签、看图、分享卡。
//! - [`motion`] —— 一次动作「怎么走」：翻页动画六档的摆位层。
//!
//! 这里**没有 `cfg`**：以前桌面是「`TextView` + 虚拟列表」、移动是「自绘字形」两套实现，
//! 同一本书的行走两条不同的路；合成一套之后，平台上只剩两件事不同，而两件都不在这一层 ——
//!
//! - 外壳占的是宽还是高：[`Reserve`] 两个轴都带着，谁用哪个由模式自己决定。
//! - 正文家族在这台设备上解析不解析得出来：`crate::ui::host`。

mod components;
mod model;
mod motion;
mod view;

pub use model::ReaderSession;
pub use model::chrome::{Arrow, Reserve};
pub use view::ReadingPage;

// 「读者自己装了一份字体」的排版失效口。字体表版本计数（`FACE_EVENTS`）住在
// `model::book_fonts`，但装字体这件事发生在 feature 外（`platform::user_fonts` 的
// boot 重注册与会话内安装）—— 在这里开一条窄口，不把整个 `model` 摆成 pub(crate)。
pub(crate) use model::book_fonts::note_external_registration;
