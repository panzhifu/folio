//! 阅读会话的功能模块：`navigation`（翻页/滚动/模式）、`position`（进度 ↔ 坐标、恢复、
//! 落库）、`toc`（章节表）、`annotations`（划线与笔记的读写）、`footnotes`（脚注引用）、
//! `links`（正文里的链接）、`reading_time`（把这本书读了多久切成一段一段写进库，喂
//! 「统计」那一屏）。
//!
//! 每个文件都写 `impl [`ReaderSession`]`，而不是另造一套被会话持有的子 struct：
//! 这些命令要读写的正是会话自己那点状态（`scroll_y` / `page` / `pending_restore` …），
//! 拆成一个要来回转发的子 struct 只会多一层借用与样板。字段以 `pub(super)` 对 `reader`
//! 这一纵切可见，模块因此打在同一个实体上 —— 一个会话一个实体。
//!
//! 这里是**判断与状态迁移**，没有 gpui 元素；渲染在 `reader::view`。
//! 划词那一族不在这里：它连绘制件与菜单条一起住在 [`selection`](super::super::selection)，
//! 因为高亮与把手只能在绘制期画、事件只能在绘制期注册，跟状态机分不开。

// 只有这一个模块有自由函数（`resolved_for` / `Palette`）要被 `view.rs` 直接调，
// 所以它是 `pub(super)`；其余几个只往 `ReaderSession` 上挂方法，模块本身不必对外。
pub(super) mod annotations;
mod footnotes;
mod links;
mod navigation;
mod position;
mod reading_time;
mod toc;
