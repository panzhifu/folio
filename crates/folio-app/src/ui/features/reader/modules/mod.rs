//! 阅读会话的功能模块：`navigation`（翻页/滚动/模式）、`position`（进度 ↔ 坐标、恢复、
//! 落库）、`toc`（章节表）。
//!
//! 每个文件都写 `impl [`ReaderSession`]`，而不是另造一套被会话持有的子 struct：
//! 这些命令要读写的正是会话自己那点状态（`scroll_y` / `page` / `pending_restore` …），
//! 拆成一个要来回转发的子 struct 只会多一层借用与样板。字段以 `pub(super)` 对 `reader`
//! 这一纵切可见，模块因此打在同一个实体上 —— 一个会话一个实体。
//!
//! 这里是**判断与状态迁移**，没有 gpui 元素；渲染在 `reader::view`。

mod navigation;
mod position;
mod toc;
