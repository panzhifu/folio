//! 平台服务边界：视图通过它拿平台相关的东西，自己不写 `cfg(mobile)`。
//!
//! 平台差异只允许出现在这里（服务）与 `super::platform`（入口与原生桥）。
//! 其余页面/部件一律走这个边界。目前边界上挂这些事：
//!
//! - **安全区**（系统栏 / 手势条 / 挖孔），用来给自绘外壳留出上下两条。
//! - **输入法 inset**，键盘盖住多少。**刻意与安全区分开**：弹键盘不该移动外壳，
//!   需要把被聚焦的输入框挪出键盘时读它。
//! - **正文字体家族**，这台设备真能解析出哪一个（见 [`device_family`]）—— 阅读视图
//!   从此不再按 target 分叉，这条也一起搬进了边界。
//! - **浮层控件点不点得住**（[`supports_overlay_controls`]），决定「我的」那一格
//!   画整页设置还是行列表。
//! - **返回键走到栈底之后退不退程序**（[`back_exits_app`]），手机要退，桌面的窗口
//!   由窗口管理器管，不该被一次返回关掉。
//! - **这台设备此刻的本地时区偏移**（[`local_offset_secs`]），统计按本地日分桶要把它
//!   当参数交给 core。这一项两端同一个实现，所以它是这个模块上的一个自由函数，
//!   不在 [`Host`] trait 上。
//! - **这一端的指针长按不长按得住**（[`supports_long_press`]），决定读书时「把图放大」
//!   是按住图片还是点一下图片。
//! - **选择器交回的路径是不是应用自己的临时副本**（[`picked_files_are_temporary`]），
//!   决定导入读完后要不要把 `cache/gpui-import-*` 那份拷贝连同目录清掉。桌面选择器
//!   给的是读者原文件，一动都不能动；Android / iOS 的桥会先落一份 sandbox / cache 拷贝，
//!   那一路必须清。
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
    /// gpui-kit 的浮层控件（下拉框、菜单、对话框、侧边面板）在这端**点得住**吗。
    fn overlay_controls(&self) -> bool;
    /// 返回键走到栈底（当前目的地那条栈已经弹不动了）时，这一端要不要**就此退出**。
    ///
    /// 手机要：那一端没有别的退出入口，宿主也不该留着前台空转。桌面不要：窗口归窗口
    /// 管理器管，一次返回键把进程关掉是错的。
    fn back_exits_app(&self) -> bool;
    /// 这一端的指针**按得住长按**吗。
    ///
    /// gpui 只在触摸手势里合成 `LongPressEvent`：那份状态机是
    /// `gpui-pre-0.3.8/src/gestures.rs` 的 `TouchGestureState::LongPressing`，吃的是
    /// `TouchEvent`。桌面的鼠标不产生 `TouchEvent`，按住不放也不会来一条长按 ——
    /// 所以「按住某物」在桌面上根本不存在，凡是把长按当入口的功能都要在这一格上分叉。
    fn long_press(&self) -> bool;
    /// 系统选择器交回的路径是不是应用自己拷出来、用完即可删的临时副本。
    ///
    /// 桌面文件选择器给的是读者原文件，不能动；Android 的 `GpuiPathPicker` 与 iOS 的
    /// sandbox copy 会先拷进应用 cache / 沙盒再把路径交出来，导入读完必须清 ——
    /// 不然 `cache/gpui-import-<uuid>/` 会一轮轮堆上去。
    fn picked_files_are_temporary(&self) -> bool;
    /// 把一张图的字节交给系统的分享面板。
    ///
    /// 移动端有系统级分享；桌面没有这一档，实现里安静 no-op。图片查看器那一层的
    /// 「分享」按钮走它（字节在 `bytes`，文件名 `name` 带扩展名，`mime` 是媒体类型）。
    fn share_image(&self, bytes: &[u8], mime: &str, name: &str);
    /// 把一张图的字节存进系统相册（移动端）/ 下载目录（桌面）。
    fn save_image(&self, bytes: &[u8], mime: &str, name: &str);
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

/// gpui-kit 的浮层控件在这端点不点得住。没装平台服务（宿主测试里）当不能：
/// 测试要钉的是界面本身，而不是某个浮层能不能真的点开。
pub fn supports_overlay_controls(cx: &App) -> bool {
    cx.try_global::<HostServices>()
        .is_some_and(|services| services.0.overlay_controls())
}

/// 返回键走到栈底要不要就此退出应用。没装平台服务（宿主测试里）当**不退**：
/// 那条路是 `AppShell::back()` 的最后一档，测试里一次返回键不该把整个测试循环关掉。
pub fn back_exits_app(cx: &App) -> bool {
    cx.try_global::<HostServices>()
        .is_some_and(|services| services.0.back_exits_app())
}

/// 这一端的指针按得住长按吗。没装平台服务（宿主测试里）当**按不住**：
/// 与 [`supports_overlay_controls`] 同一条理由 —— 测试要钉的是界面与状态，而
/// `simulate_click` 投不出长按，把桌面那一档（单击）当默认才测得到入口。
pub fn supports_long_press(cx: &App) -> bool {
    cx.try_global::<HostServices>()
        .is_some_and(|services| services.0.long_press())
}

/// 系统选择器交回的路径是不是应用自己拷出来的临时副本。没装平台服务（宿主测试里）
/// 当**不是**：测试造的是真实文件路径，导入只该读、不该删。
pub fn picked_files_are_temporary(cx: &App) -> bool {
    cx.try_global::<HostServices>()
        .is_some_and(|services| services.0.picked_files_are_temporary())
}

/// 把一张图的字节交给系统分享面板。没装平台服务（宿主测试里）时什么都不做 ——
/// 测试要钉的是「按了分享按钮有没有把字节递出去」，而递出去那一步在真机上才看得见。
pub fn share_image(bytes: &[u8], mime: &str, name: &str, cx: &App) {
    if let Some(services) = cx.try_global::<HostServices>() {
        services.0.share_image(bytes, mime, name);
    }
}

/// 把一张图的字节存进系统相册 / 下载目录。没装平台服务时什么都不做（同上）。
pub fn save_image(bytes: &[u8], mime: &str, name: &str, cx: &App) {
    if let Some(services) = cx.try_global::<HostServices>() {
        services.0.save_image(bytes, mime, name);
    }
}

/// 这台设备此刻的本地时区偏移（秒，东为正）。统计那几屏按本地日分桶要用它当参数
/// 交给 `folio_core`（core 自己不读环境，见 `folio_core::infra::time` 的模块文档）。
///
/// 它站在 `host` 这一侧，但**不是** `Host` trait 的一项：桌面与移动走的是同一次调用，
/// 没有需要分叉的实现。trait 上只该挂「两端答案不同」的服务，否则边界上长出来的
/// 是两份一模一样的代码。
///
/// 认不出时区时 `chrono` 回退 UTC ⇒ 这里给 0，日界线就按 UTC 那天。这是**降级而不是
/// 事故**：统计页照样能画，只是那一天的分界挪了几小时，所以不打错误日志。
/// （Android 与 HarmonyOS 上这条路是通的：`iana-time-zone` 读 `persist.sys.timezone`
/// 系统属性，`chrono` 对这两个目标用它内嵌的时区数据而不是去读 `/usr/share/zoneinfo`。）
pub fn local_offset_secs() -> i64 {
    i64::from(chrono::Local::now().offset().local_minus_utc())
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
    tracing::error!(
        "候选家族一个都没命中；设备注册的 {} 个名字：{names:?}",
        names.len()
    );
}
