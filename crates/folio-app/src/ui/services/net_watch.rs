//! 网络可用性的**变化**：断→连的那一下，把云盘同步补上。
//!
//! Java 侧一条 `ConnectivityManager.NetworkCallback`（见 `GpuiSystemUi.
//! registerNetworkCallback`），`onAvailable` / `onLost` 经 JNI 导出的
//! `nativeNetworkChange` 落到 [`deliver`]。系统按**每张网**各报一次（Wi-Fi 与蜂窝并存
//! 时 onLost 一张不等于断网），所以这里先做一次状态机去重：只有真·断→连 / 连→断的
//! **转移**才进队列。
//!
//! 初始状态按「在线」记：注册后系统立刻给一轮 onAvailable，那不是转移，不该触发第二次
//! 启动同步（boot 自己那一次已经在跑）。
//!
//! 消费在外壳的渲染帧：恢复（true）→ 云盘同步开着就补一次 `sync::sync_now`；断网的
//! 当下不做全局提示 —— 同步卡片自己的错误态已经说了那件事。非安卓平台没有回调，
//! 队列恒空。

use std::sync::Mutex;

/// 还没被外壳取走的转移：`true` = 恢复在线，`false` = 掉线。
static PENDING: Mutex<Vec<bool>> = Mutex::new(Vec::new());
/// 最近一次知道的在线态；`false` = 掉线。初始按在线（见模块注释）。
static ONLINE: Mutex<bool> = Mutex::new(true);

/// JNI 入口：网络回调报一声「现在有没有网」。只有与上次不同的才进队列。
pub fn deliver(available: bool) {
    let Ok(mut online) = ONLINE.lock() else {
        return;
    };
    if *online == available {
        return;
    }
    *online = available;
    drop(online);
    if let Ok(mut queue) = PENDING.lock() {
        // 转移本身罕见，队列上界只是兜底：排满时丢旧的，最新的转移才是要响应的那个。
        if queue.len() >= 4 {
            queue.remove(0);
        }
        queue.push(available);
    }
    tracing::info!(
        "net_watch: 网络{}了",
        if available { "恢复" } else { "断开" }
    );
}

/// 取走**全部**转移（先进先出）。外壳渲染帧里调。
pub fn drain() -> Vec<bool> {
    match PENDING.lock() {
        Ok(mut queue) => std::mem::take(&mut *queue),
        Err(_) => Vec::new(),
    }
}

/// 网络恢复（断→连）的当下：云盘同步开着就补一次同步，把断网期间攒下的变更送出去。
/// 判据（开没开）跟 `sync_now` 自己那套是同一份配置，所以这一步只挡「没开同步」的
/// 那一种白跑。
pub fn recovered(cx: &mut gpui_kit::App) {
    if !crate::ui::services::config::ConfigHolder::sync(cx).enabled {
        return;
    }
    tracing::info!("net_watch: 网络恢复，补一次云盘同步");
    super::sync::sync_now(cx);
}

#[cfg(test)]
mod tests {
    use crate::ui::services::net_watch::{deliver, drain};

    #[test]
    fn only_transitions_reach_the_queue() {
        // 初始按在线：注册后系统立刻给的那轮 onAvailable 不算转移。
        deliver(true);
        assert!(drain().is_empty());
        deliver(false);
        deliver(false);
        deliver(true);
        deliver(true);
        assert_eq!(drain(), vec![false, true], "只有断→连 / 连→断的转移进队");
        assert!(drain().is_empty());
    }
}
