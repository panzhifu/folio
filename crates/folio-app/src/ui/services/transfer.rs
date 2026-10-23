//! Wi-Fi 传书在 app 侧的编排：设置页的开关 → 起 / 停
//! [`folio_core::net::transfer::TransferServer`]。
//!
//! 服务句柄挂在 [`super::resident`] 的进程级常驻资源里，只活到读者关掉或进程退出 ——
//! **不写进配置**：开着的传书服务是一个「我在局域网里裸着」的状态，读者离开设置页它还在
//! 没问题（那是他刚开的），但重启还自动开着就是惊吓。收件箱里的原件在导入成功后清掉，
//! 失败保留待查（与监听同一套提交路径：勾选 = 亲手传的，重名算更新）。
//!
//! 服务本体在 core、启停编排在界面侧，是文档 `plans/基础设施.md` §1.3 的分工：
//! 服务是往作业池里塞活的**驱动者**，而「何时起停」由界面决定。

use std::path::PathBuf;
use std::sync::Arc;

use folio_core::net::transfer::{TransferConfig, TransferServer};
use folio_core::{ImportKind, ImportOutcome, Imported, PathsSource, TaskEvent};
use gpui_kit::App;

use super::app_store;
use super::config::ConfigHolder;
use super::import_queue::auto_import_sender;
use super::library::target_dir;
use super::resident;

/// 默认端口。被占就顺延（core 里最多试 10 个），真实的以 `addr` 为准。
const DEFAULT_PORT: u16 = 8410;

/// 传书服务此刻的对外信息：能打开的地址与配对码。
pub struct TransferStatus {
    pub urls: Vec<String>,
    pub pin: String,
}

/// 服务活着就给出它的对外信息；没起就是 `None`。
pub fn running() -> Option<TransferStatus> {
    resident::services().with_transfer(|server| TransferStatus {
        urls: server.urls(),
        pin: server.pin().to_string(),
    })
}

/// 按开关起停。书库没装、位置没确认过就不起 —— 收下的书总得有个去处。
pub fn set_enabled(on: bool, cx: &App) {
    if !on {
        resident::services().set_transfer(None);
        return;
    }
    let Some(store) = app_store::handle(cx) else {
        tracing::error!("transfer: 书库没装上，传书不开");
        resident::services().set_transfer(None);
        return;
    };
    let Some(book_dir) = target_dir(&ConfigHolder::library(cx)) else {
        tracing::error!("transfer: 书库位置没确认过，传书不开");
        resident::services().set_transfer(None);
        return;
    };
    let inbox_dir: PathBuf = folio_core::infra::paths::data_dir().join("inbox");
    let on_file = {
        let store = Arc::clone(&store);
        let book_dir = book_dir.clone();
        Arc::new(move |path: PathBuf| {
            // 人手传的文件只报「这是传书交的」：副本名由内容哈希定，所以没有
            // 「重名怎么办」这一问；库里已经有同内容的那一本会在判重那一步报 Duplicate。
            let source = PathsSource::new(vec![path.clone()], ImportKind::Transfer);
            let handle = folio_core::submit_import(
                &resident::pool(),
                Box::new(source),
                book_dir.clone(),
                Arc::clone(&store),
            );
            forward_and_cleanup(handle, path);
        })
    };
    match TransferServer::start(
        TransferConfig {
            port: DEFAULT_PORT,
            pin: String::new(),
            inbox_dir,
        },
        on_file,
    ) {
        // `set_transfer` 会先停旧的那只再存新的。
        Ok(server) => resident::services().set_transfer(Some(server)),
        Err(err) => {
            tracing::error!("transfer: 传书服务起不来：{err:#}");
            resident::services().set_transfer(None);
        }
    }
}

/// 转发一条导入作业的事件进自动导入通道；成功的批次顺手清掉收件箱里的原件
/// —— 收进来的书已经在书库目录里有副本，收件箱只该是中转站。
///
/// 事件的收干交给 [`folio_core::ingest::import::forward`]（它管线程创建）。
fn forward_and_cleanup(
    handle: folio_core::infra::task::TaskHandle<Vec<ImportOutcome>>,
    inbox_path: PathBuf,
) {
    let sender = auto_import_sender();
    folio_core::ingest::import::forward(handle, move |event| {
        if let TaskEvent::Finished(Ok(outcomes)) = &event
            && outcomes
                .iter()
                .any(|outcome| matches!(outcome.result, Ok(Imported::Added(_))))
            && let Err(err) = std::fs::remove_file(&inbox_path)
        {
            tracing::warn!(
                "transfer: 收件箱原件 {} 没清掉：{err}",
                inbox_path.display()
            );
        }
        // 接收端没了：没有要的人，这一条丢掉即可（后续同样丢）。
        let _ = sender.send_blocking(event);
    });
}
