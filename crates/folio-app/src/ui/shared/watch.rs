//! 文件夹监听在 app 侧的编排：读配置 → 起 / 重启 / 停 [`folio_core::watch::WatchService`]。
//!
//! 服务句柄挂在 [`super::services`] 的进程级常驻资源里，活过任何一屏，收尾跟着它走 ——
//! 它不是界面的附件，是后台能力，所以**不再**自己拿一个 `static`（`plans/基础设施.md` §1.3）。
//! 配置里监听相关的那几项一变（[`sync_from_config`]）就**停旧的再按新配置起**：监听目录、
//! 轮询节奏都可能变，与其做差量不如整只换。书库没装、位置没确认过、监听没开，三种情形都停。
//!
//! 监听的批次在 core 内部提交（`submit_import`），成败只进日志 —— 自动导入
//! 不打扰读者，架上多出一本他自己看得见。要给自动导入接界面读数时，把
//! `submit_import` 的句柄交 [`forward_to_auto_imports`](super::import_queue)
//! 转发即可（传书那一步就是这么接的）。

use std::time::Duration;

use folio_core::watch::{WatchConfig, WatchService};
use gpui_kit::App;

use super::config_model::ConfigHolder;
use super::library::target_dir;
use super::services;

/// 让监听与当前配置一致：开着就起（或按新配置重启），没开就停。
///
/// 界面线程在启动与监听相关配置改动时各调一次。阻塞发生在「停旧的」上 —— 最多等
/// 监听线程一轮 tick（一秒），可接受；起的那一下只是建线程，不阻塞。
pub fn sync_from_config(cx: &App) {
    let library = ConfigHolder::library(cx);
    let store = super::app_store::handle(cx);

    let stopped = || {
        services::services().set_watch(None);
    };

    let Some(watch) = library.watch.clone() else {
        stopped();
        return;
    };
    let Some(dest_dir) = target_dir(&library) else {
        tracing::error!("watch: 书库位置没确认过，监听不开");
        stopped();
        return;
    };
    let Some(store) = store else {
        tracing::error!("watch: 书库没装上，监听不开");
        stopped();
        return;
    };

    let config = WatchConfig {
        dir: watch.dir.clone().into(),
        poll_interval: Duration::from_secs(watch.interval_secs as u64),
        max_depth: 3,
    };
    // `set_watch` 会先停旧的那只再存新的，所以这里不必单独停。
    let queue = services::pool();
    services::services().set_watch(Some(WatchService::start(config, store, queue, dest_dir)));
    tracing::info!("watch: 监听已起（{}）", watch.dir);
}
