//! 文件夹监听在 app 侧的生命周期：读配置 → 起 / 重启 / 停 [`folio_core::watch::WatchService`]。
//!
//! 服务挂在 `static` 里，活过任何一屏的切换 —— 它不是界面的附件，是后台能力。
//! 配置每改一次（[`sync_from_config`]）就**停旧的再按新配置起**：监听目录、
//! 轮询节奏都可能在配置里变，与其做差量不如整只换。书库没装、位置没确认过、
//! 监听没开，三种情形都停。
//!
//! 监听的批次在 core 内部提交（`submit_import`），成败只进日志 —— 自动导入
//! 不打扰读者，架上多出一本他自己看得见。要给自动导入接界面读数时，把
//! `submit_import` 的句柄交 [`forward_to_auto_imports`](super::import_queue)
//! 转发即可（传书那一步就是这么接的）。

use std::sync::{Mutex, OnceLock};

use folio_core::watch::{WatchConfig, WatchService};
use gpui_kit::App;

use super::config_model::ConfigHolder;
use super::import_queue::import_queue;
use super::library::target_dir;

static WATCH: OnceLock<Mutex<Option<WatchService>>> = OnceLock::new();

fn slot() -> &'static Mutex<Option<WatchService>> {
    WATCH.get_or_init(|| Mutex::new(None))
}

/// 让监听与当前配置一致：开着就起（或按新配置重启），没开就停。
///
/// 界面线程在启动与配置改动时各调一次。阻塞发生在「停旧的」上 —— 最多等
/// 监听线程一轮 tick（一秒），可接受；起的那一下只是建线程，不阻塞。
pub fn sync_from_config(cx: &App) {
    let want = ConfigHolder::library(cx).watch.clone();
    let dest_dir = target_dir(&ConfigHolder::library(cx));
    let store = super::app_store::handle(cx);

    let mut guard = slot().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(service) = guard.take() {
        service.stop();
        tracing::info!("watch: 监听已停");
    }

    let (Some(watch), Some(dest_dir), Some(store)) = (want, dest_dir, store) else {
        return;
    };
    let config = WatchConfig {
        dir: watch.dir.clone().into(),
        poll_interval: std::time::Duration::from_secs(watch.interval_secs as u64),
        max_depth: 3,
    };
    let queue = import_queue().clone();
    *guard = Some(WatchService::start(config, store, queue, dest_dir));
    tracing::info!("watch: 监听已起（{}）", watch.dir);
}
