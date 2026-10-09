//! 桌面端没有系统栏 / 输入法 inset：窗口是普通桌面窗口，chrome 由窗口管理器给。

use gpui_kit::Window;

use super::{Host, Insets};

pub(super) struct DesktopHost;

impl Host for DesktopHost {
    fn safe_area(&self, _window: &Window) -> Insets {
        Insets::ZERO
    }

    fn ime_insets(&self, _window: &Window) -> Insets {
        Insets::ZERO
    }
}
