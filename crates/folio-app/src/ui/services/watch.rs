//! 文件夹监听在 app 侧的编排：读配置 → 起 / 重启 / 停 [`folio_core::ingest::watch::WatchService`]。
//!
//! 服务句柄挂在 [`super::resident`] 的进程级常驻资源里，活过任何一屏，收尾跟着它走 ——
//! 它不是界面的附件，是后台能力，所以**不再**自己拿一个 `static`（`plans/基础设施.md` §1.3）。
//! 配置里监听相关的那几项一变（[`sync_from_config`]）就**停旧的再按新配置起**：监听目录、
//! 轮询节奏都可能变，与其做差量不如整只换。书库没装、位置没确认过、监听没开，三种情形都停。
//!
//! 监听的批次在 core 内部提交（`submit_import`），成败先进日志；**界面读数走另一条**：
//! 起服务时挂一条 [`WatchEvents`](folio_core::ingest::watch::WatchEvents) 口，事件在
//! core 记完日志后交回来，转进 [`import_queue`] 那条自动导入通道 —— 与传书同一条桥、
//! 与手工导入同一套 [`import_event`](crate::ui::features::shelf::ShelfView) 处理。
//! 少了这一条会怎样：书确实进了库，架子却仍说「书架还是空的」，要重启才看得见
//! （实测过，PC 与平板都是）。

use std::sync::Arc;
use std::time::Duration;

use folio_core::ingest::watch::{WatchConfig, WatchEvents, WatchService};
use gpui_kit::App;

use super::config::ConfigHolder;
use super::library::target_dir;
use super::resident;

/// 让监听与当前配置一致：开着就起（或按新配置重启），没开就停。
///
/// 界面线程在启动与监听相关配置改动时各调一次。阻塞发生在「停旧的」上 —— 最多等
/// 监听线程一轮 tick（一秒），可接受；起的那一下只是建线程，不阻塞。
pub fn sync_from_config(cx: &App) {
    let library = ConfigHolder::library(cx);
    let store = super::app_store::handle(cx);

    let stopped = || {
        resident::services().set_watch(None);
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
    let queue = resident::pool();
    // 事件桥：core 记完日志后把这一条交回来，转进自动导入通道。架子没开时接收端
    // 已经没了 —— 送不出去就丢掉，不是错误（日志那边照记）。
    let sender = super::import_queue::auto_import_sender();
    let on_event: Arc<WatchEvents> = Arc::new(move |event| {
        let _ = sender.send_blocking(event);
    });
    resident::services().set_watch(Some(WatchService::start(
        config,
        store,
        queue,
        dest_dir,
        Some(on_event),
    )));
    tracing::info!("watch: 监听已起（{}）", watch.dir);
}
