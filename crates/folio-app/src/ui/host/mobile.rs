//! 移动端：安全区与输入法 inset 挂在窗口上，由 fork 从 JNI 报上来。
//!
//! 只有 Android 有这两条（`gpui-pre-mobile` 的 Android 后端）；iOS / HarmonyOS 目前读不到，
//! 返回 0 —— 界面会贴边但不会崩。

use gpui_kit::Window;

use super::{Host, Insets};

pub(super) struct MobileHost;

impl Host for MobileHost {
    fn safe_area(&self, _window: &Window) -> Insets {
        #[cfg(target_os = "android")]
        {
            use gpui_mobile::android::jni;
            if let Some(insets) = jni::platform()
                .and_then(|platform| platform.primary_window())
                .map(|window| window.safe_area_insets_logical())
            {
                return Insets {
                    top: insets.top.max(0.0),
                    bottom: insets.bottom.max(0.0),
                    left: insets.left.max(0.0),
                    right: insets.right.max(0.0),
                };
            }
        }
        Insets::ZERO
    }

    fn ime_insets(&self, _window: &Window) -> Insets {
        #[cfg(target_os = "android")]
        {
            use gpui_mobile::android::jni;
            if let Some(insets) = jni::platform()
                .and_then(|platform| platform.primary_window())
                .map(|window| window.ime_insets_logical())
            {
                return Insets {
                    top: insets.top.max(0.0),
                    bottom: insets.bottom.max(0.0),
                    left: insets.left.max(0.0),
                    right: insets.right.max(0.0),
                };
            }
        }
        Insets::ZERO
    }
}
