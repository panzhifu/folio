//! 听书的**遥控**通道：通知、锁屏、耳机线控上的传输键，绕回阅读页的那一条路。
//!
//! 两个方向各一条：
//!
//! - **Rust → Java**（[`super::platform::speech_remote_show`] / `_hide`）：听书状态机每
//!   次开口/暂停/收摊时把「在播什么」递给前台服务（`FolioSpeechService`），那里管
//!   MediaSession 与 MediaStyle 通知 —— 进程因此不被回收，读者离开应用也能接着听。
//! - **Java → Rust**（`deliver`，由 JNI 导出的 `nativeMediaAction` 喂进来）：读者在通知
//!   或锁屏上按了播放/暂停/上下句/停止。这些命令发生在 **JNI 线程**上，手上没有 `App`，
//!   所以只排队；外壳在渲染帧里取走、转给阅读页（见 `shell::AppShell::render`），最终
//!   落到 `ReaderSession::apply_remote_speech`。
//!
//! 队列形状与 [`super::cloud_link`] / [`super::intent_import`] 同一条：一把锁 + 一条
//! 有界队列，没有常驻接收任务，也没有「谁在收」的问题。非安卓平台没有这一层，队列
//! 恒空。

use std::sync::Mutex;

/// 还没被外壳取走的传输命令："play" / "pause" / "stop" / "next" / "previous"。
static PENDING: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

const CAP: usize = 8;

/// JNI 入口：收一条传输命令。命令是服务那边写死的那五个词，多余的丢。
pub fn deliver(command: &str) {
    // 落队的是字面量那 &'static str，不是调用方的借用：JNI 那头的字符串活不过这一帧。
    let command: &'static str = match command {
        "play" => "play",
        "pause" => "pause",
        "stop" => "stop",
        "next" => "next",
        "previous" => "previous",
        _ => return,
    };
    if let Ok(mut queue) = PENDING.lock() {
        // 连按不积账：同一格命令只算一次；上下句连按是「挪几句」，逐条保留。
        if queue.len() >= CAP {
            queue.remove(0);
        }
        if !queue.contains(&command) || matches!(command, "next" | "previous") {
            queue.push(command);
        }
    }
}

/// 取走**全部**已收到的命令（先进先出）。外壳渲染帧里调。
pub fn drain() -> Vec<&'static str> {
    match PENDING.lock() {
        Ok(mut queue) => std::mem::take(&mut *queue),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use crate::ui::services::speech_remote::{deliver, drain};

    #[test]
    fn commands_queue_in_order_and_deduplicate_repeats() {
        for word in ["play", "pause", "pause", "next", "next", "next", "stop"] {
            deliver(word);
        }
        // 同一格命令（play/pause/stop）连按只算一次；上下句连按是「挪几句」，逐条保留。
        let commands = drain();
        assert_eq!(
            commands,
            vec!["play", "pause", "next", "next", "next", "stop"]
        );
        assert!(drain().is_empty());
        // 未知命令丢弃，不进队列。
        deliver("rewind");
        assert!(drain().is_empty());
    }
}
