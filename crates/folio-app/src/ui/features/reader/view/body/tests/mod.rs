//! 阅读页正文（`super` 是 `view::body`）的宿主测试套件，按主题拆分：
//! 每个文件一个功能面，夹具与事件助手都在 [`harness`]。
//!
//! 这批用例原来在 `desktop.rs`（虚拟列表那一套）里，两端一份实现之后，
//! 这些断言测的就是真机上跑的那条代码路径。
//!
//! 跑法：`cargo test -p folio-app --lib reader`。导入点名而不 glob：
//! `gpui_kit` 根有个名叫 `test` 的属性宏，glob 会让下面的 `#[test]` 递归展开成它。

mod annotation;
mod capture;
mod fixed;
mod harness;
mod layout;
mod library;
mod media;
mod mode;
mod reflow;
mod scroll;
mod selection;
mod speech;
mod swipe;
mod toc;
mod translation;
mod turn;
