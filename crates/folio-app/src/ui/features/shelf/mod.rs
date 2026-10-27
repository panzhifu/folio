//! 书架功能：封面网格 + 把书交给阅读页。
//!
//! 网格实现（触摸滚动、封面解码、搜索）在 [`view`]；一格卡片长什么样在 [`book_card`]，
//! 卡片上长按/右键弹出的管理菜单单独一份在 [`book_menu`]，菜单点开的那一整页「信息」在
//! [`book_info`]（它是压上导航栈的一页，不是架面上的一块面板），整机扫描的候选清单在
//! [`scan`]；那三块「要读者答一句话」的面板装进哪一块容器在 [`overlay`]；这里只做模块
//! 边界与再导出。压上导航栈的那两页 —— 「信息」与某一组那一页 —— 都由架面自己 push
//! （`view::ShelfView::show_info` 与 `view::ShelfView::open_page`），打开一本书则交回外壳
//! （[`ShelfEvent::Open`]）。

mod components;
mod overlay;
mod perf;
mod view;

pub use view::book_info::{BookInfoEvent, BookInfoView};
pub use view::{ShelfEvent, ShelfView};

// 阅读页那本书的书头要把多位作者并成一行、要把字节数念成人读的大小。这两条写法
// 定义仍在架上那两处（作者分隔属于卡片，大小属于扫描清单），这里只是让读页够得着，
// 免得第二份实现在别处长出来。
pub(crate) use components::AUTHOR_SEP;
pub(crate) use view::scan::fmt_size;
