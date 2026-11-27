//! 每帧入站钩子：系统/远程送进 shell 的那些「到达即消费」的通道。
//!
//! 单开一间是因为 `shell/mod.rs` 贴着行数基线；这一间只收「渲染帧里排空队列 /
//! 递平台」的那几段 —— 判据与消费方都写在各自的服务与平台层，这里只接线。

use gpui_kit::Window;

use super::super::AppShell;
use crate::ui::services::config::ConfigHolder;

impl AppShell {
    /// 最近在读的那本书（长按图标「继续阅读」）：查会话表的最后一条，开着就换书 /
    /// 盖层；从没读过就落回书架 —— 快捷方式不该第一次用就把读者领进空白屏。
    fn open_recent_reading(&mut self, window: &mut Window, cx: &mut gpui_kit::Context<AppShell>) {
        let book = crate::ui::services::app_store::handle(cx)
            .and_then(|store| store.latest_read_book().ok().flatten());
        match book {
            Some(book_id) => self.open_reading(book_id, cx),
            None => self.go(crate::ui::shell::navigation::Destination::Shelf, window, cx),
        }
    }
}

/// 每帧收一次：听书遥控、快捷方式路由、网络恢复、屏幕常亮。调用点在
/// `AppShell::render` 末尾（原 `was_active` 结账那一行，顺手由这里带过）。
pub(in crate::ui::shell) fn inbound(
    this: &mut AppShell,
    window: &mut Window,
    cx: &mut gpui_kit::Context<AppShell>,
) {
    let active = window.is_window_active();
    // 听书遥控（通知 / 锁屏 / 耳机线控）：JNI 那头排进来的传输命令，这一帧转给阅读层。
    // 书没收摊时读者点的那本就是挂着这本；队列空时零开销。
    if let Some(layer) = this.reading.as_ref() {
        let commands = crate::ui::services::speech_remote::drain();
        if !commands.is_empty() {
            layer
                .page
                .update(cx, |page, cx| page.apply_speech_commands(&commands, cx));
        }
    }
    // 长按图标快捷方式（folio://action/…）：JNI 那头排进来的动作，这一帧导航。
    // 「继续阅读」开最近在读的那本；「笔记」切到笔记那一屏。
    for url in crate::ui::services::routing::drain() {
        match url.strip_prefix("folio://action/") {
            Some("read") => this.open_recent_reading(window, cx),
            Some("notes") => this.go(crate::ui::shell::navigation::Destination::Notes, window, cx),
            _ => {}
        }
    }
    // 网络恢复（断→连）：云盘同步开着就补一次同步。断网的当下不做全局提示 ——
    // 同步卡片自己的错误态已经说了那件事。
    for available in crate::ui::services::net_watch::drain() {
        if available {
            crate::ui::services::net_watch::recovered(cx);
        }
    }
    // 「屏幕常亮」跟着书走：书挂着就亮着（阅读页自己息屏最恼人，听书与内嵌音频播放
    // 时阅读层本来就挂着，一并覆盖），书收了把 flag 交还系统。每帧递一次：平台侧
    // 对重复值去重（真值变化才 JNI），Android 前后台重建窗口丢 flag 的那一帧也自然
    // 补上。偏好关闭时同样递一次「关」。
    let keep_on = this.reading_mounted() && ConfigHolder::appearance(cx).keep_screen_on;
    window.defer(cx, move |_, _| {
        crate::ui::platform::set_keep_screen_on(keep_on);
    });
    this.was_active = active;
}
