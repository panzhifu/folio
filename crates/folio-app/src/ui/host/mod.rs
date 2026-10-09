//! 平台服务边界：视图通过它拿平台相关的东西，自己不写 `cfg(mobile)`。
//!
//! 平台差异只允许出现在两处 —— 这里（服务）与 `super::features::reader`（两套渲染）。
//! 其余页面/部件一律走这个边界。目前边界上是两条 inset：
//!
//! - **安全区**（系统栏 / 手势条 / 挖孔），用来给自绘外壳留出上下两条。
//! - **输入法 inset**，键盘盖住多少。**刻意与安全区分开**：弹键盘不该移动外壳，
//!   需要把被聚焦的输入框挪出键盘时读它。
//!
//! 桌面端两者都是 0；Android 端读 fork 报上来的值（`gpui-pre-mobile` 的
//! `AndroidWindow::safe_area_insets_logical()` / `ime_insets_logical()`）。

use gpui_kit::{App, Global, Window};

/// 四边 inset，逻辑像素。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Insets {
    pub top: f32,
    pub bottom: f32,
    pub left: f32,
    pub right: f32,
}

impl Insets {
    pub const ZERO: Self = Self {
        top: 0.0,
        bottom: 0.0,
        left: 0.0,
        right: 0.0,
    };
}

/// 一台设备提供的平台服务。实现一个就是接上一个平台。
pub trait Host: 'static {
    /// 系统栏 / 手势 / 挖孔占掉的四条边（**不含**输入法）。逻辑像素。
    fn safe_area(&self, window: &Window) -> Insets;
    /// 软件键盘占掉的 inset（`bottom` 是键盘高度，键盘不可见时为 0）。逻辑像素。
    fn ime_insets(&self, window: &Window) -> Insets;
}

#[cfg(not(mobile))]
mod desktop;
#[cfg(mobile)]
mod mobile;

/// 装好的平台服务。由入口在开窗前 [`install`] 一次。
struct HostServices(Box<dyn Host>);

impl Global for HostServices {}

/// 装平台服务。每个入口（`super::platform`）在开窗前调一次。
pub fn install(cx: &mut App) {
    #[cfg(not(mobile))]
    cx.set_global(HostServices(Box::new(desktop::DesktopHost)));
    #[cfg(mobile)]
    cx.set_global(HostServices(Box::new(mobile::MobileHost)));
}

/// 安全区。没装（测试里）或读不到时是 0 —— 界面会贴边，但不会崩。
pub fn safe_area(window: &Window, cx: &App) -> Insets {
    cx.try_global::<HostServices>()
        .map(|services| services.0.safe_area(window))
        .unwrap_or_default()
}

/// 输入法 inset。
pub fn ime_insets(window: &Window, cx: &App) -> Insets {
    cx.try_global::<HostServices>()
        .map(|services| services.0.ime_insets(window))
        .unwrap_or_default()
}
