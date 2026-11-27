//! 系统送进来的文件入口：文件管理器「用 Folio 打开」（`ACTION_VIEW`）与
//! 「分享到 Folio」（`ACTION_SEND`）带来的 `content://` 文档 URI。
//!
//! 与 [`cloud_link`] 同一套形状：URI 在 **JNI 线程**上收下（intent 到达时手上没有
//! `App`、更没有窗口），这里只做「收下、排队」；架面在它自己的渲染帧里取用 —— 那一帧
//! 同时握着窗口与 `&mut self`，导入卡片与进度都住在架面（`ShelfView::dispatch_import`）。
//! 架面没挂上时队列留着，等它回来再导入。
//!
//! URI 只带**临时**授权：`FolioSourceAccess` 的持久授权闸对它原本一概拒绝，所以 Java
//! 侧在 intent 进来时把 URI 登记进「本次收到的」注册表，读取对注册表成员放行（见
//! `GpuiActivity` 与 `FolioSourceAccess`）。字节落成 cache 里的临时文件交给导入管线
//! （[`folio_core::ingest::import::PathsSource::picked_temporary`]），读完即弃，与选择器
//! 副本同一条清理口径 —— URI 的授权随进程存活，攒着不读反而会失效。
//!
//! 其它平台没有 intent：队列只收不到东西，不引入任何新依赖。

use std::sync::{Mutex, OnceLock};

/// 还没被架面取走的文档 URI。一次「打开方式」来一条，连点多下最多叠几条。
static PENDING: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

fn slot() -> &'static Mutex<Vec<String>> {
    PENDING.get_or_init(|| Mutex::new(Vec::new()))
}

/// 平台层入口：把一条 intent 送来的文档 URI 收下。队列没建过也照收（`slot()` 会就地建）。
///
/// 由 `cloud_link::deliver` 的入站分流调到这里：`content://` / `file://` 归导入，
/// `folio://` 归云盘回跳（判据只写在那一处）。
pub fn deliver(uri: &str) {
    match slot().lock() {
        Ok(mut queue) => {
            // 只留前几条（与 cloud_link 同一口径）：连点十几次「用 Folio 打开」没有积着
            // 的意义，超出的丢弃，架面回来时不会一次导入一大摞。
            if queue.len() < 8 {
                queue.push(uri.to_string());
            }
        }
        Err(err) => tracing::warn!("intent_import: 入站队列的锁中毒：{err}"),
    }
}

/// 取走**全部**已收到的 URI（先进先出）。架面渲染帧里调；取到的每条落成临时文件
/// 后交导入管线。取不到架面时下一次渲染帧再取。
pub fn drain() -> Vec<String> {
    match slot().lock() {
        Ok(mut queue) => std::mem::take(&mut *queue),
        Err(err) => {
            tracing::warn!("intent_import: 入站队列的锁中毒，丢弃：{err}");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ui::services::intent_import::{deliver, drain};

    #[test]
    fn queue_keeps_order_and_caps_at_eight() {
        for index in 0..12 {
            deliver(&format!("content://provider/doc/{index}"));
        }
        let drained = drain();
        assert_eq!(drained.len(), 8, "超出的丢弃，先进先出");
        assert!(drained[0].ends_with("/0"));
        assert!(drained.last().unwrap().ends_with("/7"));
        // 取完即空：同一批不会被架面的下一次渲染帧再导入一遍。
        assert!(drain().is_empty());
    }
}
