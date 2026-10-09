//! 平台服务边界：视图通过它拿平台相关的东西，自己不写 `cfg(mobile)`。
//!
//! 平台差异只允许出现在这里（服务）与 `super::platform`（入口与原生桥）。
//! 其余页面/部件一律走这个边界。目前边界上是三件事：
//!
//! - **安全区**（系统栏 / 手势条 / 挖孔），用来给自绘外壳留出上下两条。
//! - **输入法 inset**，键盘盖住多少。**刻意与安全区分开**：弹键盘不该移动外壳，
//!   需要把被聚焦的输入框挪出键盘时读它。
//! - **正文字体家族**，这台设备真能解析出哪一个（见 [`device_family`]）—— 阅读视图
//!   从此不再按 target 分叉，这条也一起搬进了边界。
//!
//! 桌面端前两者都是 0；Android 端读 fork 报上来的值（`gpui-pre-mobile` 的
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
    /// 这台设备真能解析出来的正文家族名。见 [`device_family`]。
    fn body_family(&self, wanted: &str, cx: &App) -> String;
    /// 这台设备能不能再开一个窗口。桌面能，手机上「窗口」这个概念不存在。
    fn extra_windows(&self) -> bool;
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

/// 挑一个这台设备**真的能解析出来**的正文家族。
///
/// 必须挑，不能把偏好里的名字直接交给 GPUI：`TextSystem::resolve_font` 在请求家族和它
/// 整串回退名都不存在时是 **panic**（`gpui-pre-0.3.8/src/text_system.rs:370`），而读者的
/// 默认偏好 `LXGW WenKai` 只在开发机上装着。Android 的字族是从 `/system/fonts/` 预载进来的，
/// 桌面另有一份回退表 —— 所以「按名字找一个存在的」两端共用（[`find_family`]），
/// 「一个都没有时怎么办」留给各平台（[`Host::body_family`]）。
///
/// 没装平台服务（宿主测试里）时原样交出 `wanted`：测试要钉的是几何与坐标，
/// 不是某台机器的字体表。
pub fn device_family(wanted: &str, cx: &App) -> String {
    cx.try_global::<HostServices>()
        .map(|services| services.0.body_family(wanted, cx))
        .unwrap_or_else(|| wanted.to_string())
}

/// 覆盖中日文的候选家族，按「越靠前越该优先」排。
///
/// 随包字体排在系统字体之前：特意带过来的那一份不该输给系统里更泛的字面。名单加长要照
/// `all_font_names()` 实际报出来的数，不是照猜 —— 真机上候选全落空时日志会把名单整份打出来。
const CJK_PREFERRED: &[&str] = &[
    "LXGW WenKai",      // 随包字体：push 进应用目录后应当优先于系统的
    "Noto Sans CJK SC", // /system/fonts/NotoSansCJK-Regular.ttc 的简体那一面
    "Source Han Sans SC",
    "Noto Sans SC",
    "Microsoft YaHei",
    "PingFang SC",
    "Heiti SC",
    "Droid Sans Fallback", // Android 上覆盖 CJK 的传统兜底家族
    "Roboto",              // 只剩拉丁，但至少不 panic
];

/// 这台设备能不能再开一个窗口。没装平台服务（宿主测试里）当不能：
/// 测试要钉的是界面本身，不是「点了那一格有没有真开出一个窗」。
pub fn supports_extra_windows(cx: &App) -> bool {
    cx.try_global::<HostServices>()
        .is_some_and(|services| services.0.extra_windows())
}

/// 按名字比：先看偏好要的那个，再依次试 [`CJK_PREFERRED`]。
///
/// 手上只有 `all_font_names()` 这一个公开判据（真正的 `font_id` 是私有的），所以只能按名字。
pub(crate) fn find_family(wanted: &str, cx: &App) -> Option<String> {
    let names = cx.text_system().all_font_names();
    let find = |want: &str| {
        names
            .iter()
            .find(|name| name.eq_ignore_ascii_case(want))
            .cloned()
    };
    find(wanted).or_else(|| CJK_PREFERRED.iter().find_map(|want| find(want)))
}

/// 候选全落空时的那句日志。两个平台都说同一件事，就不各写一遍、也就不用猜对方写没写。
pub(crate) fn log_no_family(cx: &App) {
    let names = cx.text_system().all_font_names();
    log::error!(
        "候选家族一个都没命中；设备注册的 {} 个名字：{names:?}",
        names.len()
    );
}
