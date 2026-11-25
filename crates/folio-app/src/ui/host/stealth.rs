//! 低干扰阅读的平台能力边界。
//!
//! 视图只问「这一端支持什么」，不写 `cfg(mobile)`；真正的窗口调用放在实现里。
//! 能力查询与操作分离：能力缺失时状态机照常运转，只是不改变窗口。

use gpui_kit::{App, Window};

/// 当前窗口服务能可靠提供的能力。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StealthCapabilities {
    pub transparent_background: bool,
    pub minimize: bool,
    pub activate: bool,
    pub always_on_top: bool,
    pub global_hotkey: bool,
}

/// 操作失败的原因。只用于日志与设置页提示，不回显用户热键以外的内容。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StealthError {
    Unsupported,
    NoWindow,
    HotkeyBusy,
}

impl StealthError {
    pub fn text(self) -> &'static str {
        match self {
            Self::Unsupported => "当前平台不支持这项窗口能力",
            Self::NoWindow => "窗口已经关闭",
            Self::HotkeyBusy => "快捷键已被其它程序占用",
        }
    }
}

/// 平台提供的窗口能力。移动平台实现为全不支持，桌面按当前窗口系统降级。
pub trait StealthHost: 'static {
    fn capabilities(&self) -> StealthCapabilities;

    fn set_transparent(
        &self,
        transparent: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<(), StealthError>;

    fn hide(&self, window: &mut Window, cx: &mut App) -> Result<(), StealthError>;

    fn restore(&self, window: &mut Window, cx: &mut App) -> Result<(), StealthError>;

    fn set_always_on_top(
        &self,
        _on: bool,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Result<(), StealthError> {
        Err(StealthError::Unsupported)
    }
}
