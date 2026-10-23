//! 移动端：安全区与输入法 inset 挂在窗口上，由 fork 从 JNI 报上来。
//!
//! 只有 Android 有这两条（`gpui-pre-mobile` 的 Android 后端）；iOS / HarmonyOS 目前读不到，
//! 返回 0 —— 界面会贴边但不会崩。
//!
//! 字体这条与桌面的差别只在「一个候选都没命中时怎么办」：移动那边**没有**可用的回退表，
//! `resolve_font` 直接 panic，所以兜底也必须是一个设备真报出来的名字。

use gpui_kit::{App, Window};

#[cfg(target_os = "android")]
use gpui_mobile::android::jni;

use super::{BackAtRoot, Host, Insets, find_family, log_no_family};

pub(super) struct MobileHost;

impl Host for MobileHost {
    fn safe_area(&self, _window: &Window) -> Insets {
        read_insets(InsetsKind::SafeArea)
    }

    fn ime_insets(&self, _window: &Window) -> Insets {
        read_insets(InsetsKind::Ime)
    }

    fn overlay_controls(&self) -> bool {
        // Android 上 gpui-kit 的浮层点不住（见 `shelf_view` 长按菜单那条路与
        // `plans/facts.md`），而设置项点不动等于设置没有。
        false
    }

    fn back_at_root(&self) -> BackAtRoot {
        // 那一侧的返回键与边缘滑动都被 fork 翻成 escape（见 `plans/facts.md`），走到栈底
        // 就是「没有可退的了」。这时候**丢回后台**而不是退出：退出那条路会让进程在自己还
        // 挂着前台顶窗时消失，系统拿不到任务快照，就没有过渡动画可播 —— 实测那就是一次
        // 闪退的样子（`Host::back_at_root` 里记着那四条量出来的日志）。要真退出，读者划掉
        // 后台卡片就是，那一条系统连进程一起收，动画也是系统的。
        BackAtRoot::SendToBack
    }

    fn long_press(&self) -> bool {
        // 手指按得住：划词那条路已经在真机上跑通（`reader::paint` 的长按监听）。
        true
    }

    fn mouse_wheel(&self) -> bool {
        // 那一端的「滚轮」就是手指平移（`gestures.rs` 把 pan 翻成 `ScrollWheelEvent`），
        // 它在图片查看器里的含义是平移与横扫切图，不是缩放。
        false
    }

    fn picked_files_are_temporary(&self) -> bool {
        // Android 的 `GpuiPathPicker`、iOS 的 sandbox copy 都先把文件拷进应用自己
        // 的 cache / 沙盒再把路径交出来，导入读完必须清。
        true
    }

    fn share_file(&self, bytes: &[u8], mime: &str, name: &str) {
        crate::ui::platform::share_file(bytes, mime, name);
    }

    fn save_image(&self, bytes: &[u8], mime: &str, name: &str) {
        crate::ui::platform::save_image(bytes, mime, name);
    }

    fn body_family(&self, wanted: &str, cx: &App) -> String {
        // 命中与否都写一行：真机上「字体不对」只能从 logcat 看出来，
        // 屏幕上长得像对的字体未必是读者要的那套。
        match find_family(wanted, cx) {
            Some(hit) => {
                tracing::info!("body_family: 用 {hit:?}");
                hit
            }
            None => fallback_body_family(wanted, cx),
        }
    }
}

// ────────────────────────────────────────────────────────────────────
// 内部辅助
// ────────────────────────────────────────────────────────────────────

/// 要读哪一类 inset。
#[derive(Clone, Copy)]
enum InsetsKind {
    SafeArea,
    Ime,
}

/// 从当前窗口读一类 inset，并做钳位；拿不到就返回零值。
#[cfg(target_os = "android")]
fn read_insets(kind: InsetsKind) -> Insets {
    let Some(win) = jni::platform().and_then(|p| p.primary_window()) else {
        return Insets::ZERO;
    };

    let raw = match kind {
        InsetsKind::SafeArea => win.safe_area_insets_logical(),
        InsetsKind::Ime => win.ime_insets_logical(),
    };

    Insets {
        top: raw.top.max(0.0),
        bottom: raw.bottom.max(0.0),
        left: raw.left.max(0.0),
        right: raw.right.max(0.0),
    }
}

#[cfg(not(target_os = "android"))]
fn read_insets(_: InsetsKind) -> Insets {
    // iOS / HarmonyOS 目前读不到，返回 0 —— 界面会贴边但不会崩。
    Insets::ZERO
}

/// 一个候选都没命中时的兜底。
///
/// 移动端 GPUI **没有**字体回退表，`resolve_font` 拿到不存在的名字会直接 panic，
/// 所以这里必须返回一个设备真报出来的名字——哪怕只是列表里的第一个。
fn fallback_body_family(wanted: &str, cx: &App) -> String {
    log_no_family(cx);
    cx.text_system()
        .all_font_names()
        .into_iter()
        .next()
        .unwrap_or_else(|| wanted.to_string())
}
