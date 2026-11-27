//! 长按图标快捷方式的**动作路由**：`folio://action/read`（继续阅读）、
//! `folio://action/notes`（笔记）。
//!
//! 快捷方式 intent 带 `folio://` scheme 的 data，走的是与云盘回跳同一条深链通道
//! （`GpuiDeepLink` / `nativeOnDeepLink`）；`cloud_link::deliver` 的入站分流把
//! `folio://action/…` 送到这一条队列。外壳在渲染帧里取走、按动作词导航
//! （[`crate::ui::shell`] 的 `go` / `open_recent_reading`）。
//!
//! 与回跳那条队列同样的理由：intent 到达在 **JNI 线程**上，手上没有 `App` 也没有窗口，
//! 只能收下排队。其它平台没有快捷方式，队列恒空（桌面要复用这套动作时再接）。

use std::sync::Mutex;

/// 还没被外壳取走的动作 URL。
static PENDING: Mutex<Vec<String>> = Mutex::new(Vec::new());

const CAP: usize = 4;

/// 入站分流点（由 `cloud_link::deliver` 调到这里）。
pub fn deliver(url: &str) {
    if let Ok(mut queue) = PENDING.lock() {
        if queue.len() < CAP {
            queue.push(url.to_string());
        }
    }
}

/// 取走**全部**已收到的动作（先进先出）。外壳渲染帧里调。
pub fn drain() -> Vec<String> {
    match PENDING.lock() {
        Ok(mut queue) => std::mem::take(&mut *queue),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use crate::ui::services::routing::{deliver, drain};

    #[test]
    fn actions_queue_in_order_and_cap() {
        for index in 0..6 {
            deliver(&format!("folio://action/x{index}"));
        }
        let drained = drain();
        assert_eq!(drained.len(), 4);
        assert!(drained[0].ends_with("x0"));
        assert!(drain().is_empty());
    }
}
