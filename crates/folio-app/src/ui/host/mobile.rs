//! 移动端：安全区与输入法 inset 挂在窗口上，由 fork 从 JNI 报上来。
//!
//! 只有 Android 有这两条（`gpui-pre-mobile` 的 Android 后端）；iOS / HarmonyOS 目前读不到，
//! 返回 0 —— 界面会贴边但不会崩。
//!
//! 字体这条与桌面的差别只在「一个候选都没命中时怎么办」：移动那边**没有**可用的回退表，
//! `resolve_font` 直接 panic，所以兜底也必须是一个设备真报出来的名字。

use gpui_kit::{App, Window};

use super::{Host, Insets, find_family, log_no_family};

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

    fn body_family(&self, wanted: &str, cx: &App) -> String {
        // 候选名单两端共用（`super::CJK_PREFERRED`）。命中与否都写一行：真机上
        // 「字体不对」只能从 logcat 看出来，屏幕上长得像对的字体未必是读者要的那套。
        if let Some(hit) = find_family(wanted, cx) {
            log::info!("body_family: 用 {hit:?}");
            return hit;
        }
        log_no_family(cx);
        // 兜底也必须是一个真实存在的名字，否则下一次 resolve 还是 panic。
        cx.text_system()
            .all_font_names()
            .into_iter()
            .next()
            .unwrap_or_else(|| wanted.to_string())
    }
}
