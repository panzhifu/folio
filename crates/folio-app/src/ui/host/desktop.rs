//! 桌面端没有系统栏 / 输入法 inset：窗口是普通桌面窗口，chrome 由窗口管理器给。
//!
//! 字体回退与移动端不同：桌面的 GPUI 自带一份回退表（`.ZedSans` / Helvetica / Noto /
//! DejaVu…），要不到偏好家族时交给它自己退，而不是挑一个 `all_font_names()` 报出来的名字
//! —— 那个名单里第一个可能是符号字体。

use gpui_kit::{App, Window};

use super::{Host, Insets, find_family, log_no_family};

pub(super) struct DesktopHost;

impl Host for DesktopHost {
    fn safe_area(&self, _window: &Window) -> Insets {
        Insets::ZERO
    }

    fn ime_insets(&self, _window: &Window) -> Insets {
        Insets::ZERO
    }

    fn extra_windows(&self) -> bool {
        true
    }

    fn body_family(&self, wanted: &str, cx: &App) -> String {
        match find_family(wanted, cx) {
            Some(hit) => hit,
            None => {
                // 桌面落到这里意味着偏好家族和整串回退名都不在设备里。日志把名单打出来，
                // 是要看「这台机器到底有什么」，不是要猜一个 —— 猜的那一个可能是符号字体。
                log_no_family(cx);
                wanted.to_string()
            }
        }
    }
}
