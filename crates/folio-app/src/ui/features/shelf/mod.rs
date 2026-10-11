//! 书架功能：封面网格 + 把书交给阅读页。
//!
//! 网格实现（触摸滚动、封面解码、搜索）在 [`view`]；这里只做模块边界与再导出。
//! 打开一本书在当前阶段仍由外壳处理（见 `shell`），将来会改成往书架自己的导航栈里 push。

mod scan;
pub mod view;

pub use view::{ShelfEvent, ShelfView};
