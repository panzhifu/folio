//! 打开着的书库连接，挂成一个可被任何视图读取的 `Global`。
//!
//! 为什么是 Global 而不是让每个视图自己开：`Store` 里是一条 `Connection`（不可复制），
//! 而书架、阅读、将来的批注都要读同一个库。入口在开窗前 [`install`] 一次；
//! 测试装一个 `Store::in_memory()`，于是测试绝不会碰用户的真实书库。
//!
//! 句柄用 `Arc` 包一层：多个视图各自留一份，读的是同一个库。`Arc` 而不是 `Rc`，
//! 是因为导入管线（`folio_core::import`）要把 `&Store` 带进后台线程落库 ——
//! 那条线程要的是能跨线程的句柄。`Store` 自己是 `Sync` 的（连接锁在内部 `Mutex` 上），
//! 界面线程与后台线程共用同一条连接。

use std::sync::Arc;

use folio_core::Store;
use gpui_kit::{App, Global};

/// 打开着的书库连接。
struct AppStore(Arc<Store>);

impl Global for AppStore {}

/// 装书库连接。每个入口（`super::super::platform`）在开窗前调一次；测试装 `in_memory()`。
pub fn install(store: Store, cx: &mut App) {
    cx.set_global(AppStore(Arc::new(store)));
}

/// 取一份书库句柄。没装时 `None` —— 界面说「读不到书库」而不是崩。
pub fn handle(cx: &App) -> Option<Arc<Store>> {
    cx.try_global::<AppStore>().map(|store| store.0.clone())
}
