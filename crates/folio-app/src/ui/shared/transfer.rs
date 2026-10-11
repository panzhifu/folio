//! Wi-Fi 传书在 app 侧的生命周期：设置页的开关 → 起 / 停
//! [`folio_core::transfer::TransferServer`]。
//!
//! 服务挂在 `static` 里，只活到读者关掉或进程退出 —— **不写进配置**：开着的
//! 传书服务是一个「我在局域网里裸着」的状态，读者离开设置页它还在没问题
//! （那是他刚开的），但重启还自动开着就是惊吓。收件箱里的原件在导入成功后
//! 清掉，失败保留待查（与监听同一套提交路径：勾选 = 亲手传的，重名算更新）。

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use folio_core::transfer::{TransferConfig, TransferServer};
use folio_core::{ImportKind, ImportOutcome, PathsSource, Policy, TaskEvent};
use gpui_kit::App;

use super::config_model::ConfigHolder;
use super::import_queue::{auto_import_sender, import_queue};
use super::library::target_dir;
use super::app_store;

/// 默认端口。被占就顺延（core 里最多试 10 个），真实的以 `addr` 为准。
const DEFAULT_PORT: u16 = 8410;

static SERVER: OnceLock<Mutex<Option<TransferServer>>> = OnceLock::new();

fn slot() -> &'static Mutex<Option<TransferServer>> {
    SERVER.get_or_init(|| Mutex::new(None))
}

/// 传书服务此刻的对外信息：能打开的地址与配对码。
pub struct TransferStatus {
    pub urls: Vec<String>,
    pub pin: String,
}

/// 服务活着就给出它的对外信息；没起就是 `None`。
pub fn running() -> Option<TransferStatus> {
    let guard = slot().lock().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().map(|server| TransferStatus {
        urls: server.urls(),
        pin: server.pin().to_string(),
    })
}

/// 按开关起停。书库没装、位置没确认过就不起 —— 收下的书总得有个去处。
pub fn set_enabled(on: bool, cx: &App) {
    let mut guard = slot().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(server) = guard.take() {
        server.stop();
    }
    if !on {
        return;
    }
    let Some(store) = app_store::handle(cx) else {
        tracing::error!("transfer: 书库没装上，传书不开");
        return;
    };
    let Some(dest_dir) = target_dir(&ConfigHolder::library(cx)) else {
        tracing::error!("transfer: 书库位置没确认过，传书不开");
        return;
    };
    let inbox_dir: PathBuf = folio_core::paths::data_dir().join("inbox");
    let on_file = {
        let store = Arc::clone(&store);
        let dest_dir = dest_dir.clone();
        Arc::new(move |path: PathBuf| {
            // 人手传的文件：Picked 语义（重名算更新），事件流回架面读数。
            let source = PathsSource::with_policy(
                vec![path.clone()],
                ImportKind::Transfer,
                Policy {
                    skip_existing: false,
                    copy_in: true,
                    on_name_clash: folio_core::NameClash::Overwrite,
                },
            );
            let handle = folio_core::submit_import(
                import_queue(),
                Box::new(source),
                dest_dir.clone(),
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
        Ok(server) => {
            *guard = Some(server);
        }
        Err(err) => tracing::error!("transfer: 传书服务起不来：{err:#}"),
    }
}

/// 转发一条导入作业的事件进自动导入通道；成功的批次顺手清掉收件箱里的原件
/// —— 收进来的书已经在书库目录里有副本，收件箱只该是中转站。
fn forward_and_cleanup(
    handle: folio_core::task::TaskHandle<Vec<ImportOutcome>>,
    inbox_path: PathBuf,
) {
    let sender = auto_import_sender();
    std::thread::Builder::new()
        .name("folio-transfer-import".to_string())
        .spawn(move || {
            for event in handle.events.iter() {
                if let TaskEvent::Finished(Ok(outcomes)) = &event
                    && outcomes
                        .iter()
                        .any(|outcome| matches!(outcome.result, Ok(Some(_))))
                    && let Err(err) = std::fs::remove_file(&inbox_path)
                {
                    tracing::warn!("transfer: 收件箱原件 {} 没清掉：{err}", inbox_path.display());
                }
                if sender.send_blocking(event).is_err() {
                    break; // 接收端没了：没有要的人，收手。
                }
            }
        })
        .expect("起传书导入转发线程");
}
