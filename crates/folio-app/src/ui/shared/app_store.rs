//! 打开着的书库连接，挂成一个可被任何视图读取的 `Global`。
//!
//! 为什么是 Global 而不是让每个视图自己开：`Store` 里是一条 `Connection`（不可复制），
//! 而书架、阅读、将来的批注都要读同一个库。入口在开窗前 [`install`] 一次；
//! 测试装一个 `Store::in_memory()`，于是测试绝不会碰用户的真实书库。
//!
//! 句柄用 `Rc` 包一层：多个视图各自留一份，读的是同一个库（WAL 下多连接也只读同一份，
//! 但这里连多连接都不需要了）。`Rc` 不进跨线程结构，App 是单线程的。

use std::rc::Rc;

use folio_core::Store;
use gpui_kit::{App, Global};

/// 打开着的书库连接。
struct AppStore(Rc<Store>);

impl Global for AppStore {}

/// 装书库连接。每个入口（`super::super::platform`）在开窗前调一次；测试装 `in_memory()`。
pub fn install(store: Store, cx: &mut App) {
    cx.set_global(AppStore(Rc::new(store)));
}

/// 取一份书库句柄。没装时 `None` —— 界面说「读不到书库」而不是崩。
pub fn handle(cx: &App) -> Option<Rc<Store>> {
    cx.try_global::<AppStore>().map(|store| store.0.clone())
}
