//! 平台入口与原生桥。常规 UI **不放这里**。
//!
//! 每个平台的入口（`android_main` / `gpui_ios_register_app` / `ohos_napi_init` / 桌面 `main`）
//! 装好主题、配置、书库与 [`super::host`]，然后打开 [`super::shell::AppShell`]。
//!
//! 这些入口函数的**符号名是对外契约**（Android 的 `android_main`、iOS 的
//! `gpui_ios_register_app`、OHOS 的 `gpui_ohos_napi_module!`），不许改。

// 链接 gpui-mobile，让它的符号在 cdylib 里可用（Android / iOS / HarmonyOS）。
#[cfg(mobile)]
extern crate gpui_mobile;

#[cfg(not(mobile))]
pub mod desktop;

#[cfg(target_os = "android")]
pub mod android;
#[cfg(target_os = "ios")]
pub mod ios;
#[cfg(target_env = "ohos")]
pub mod ohos;

// ── 移动三端共用的启动 ────────────────────────────────────────────────────────

#[cfg(mobile)]
use folio_core::Config;
#[cfg(mobile)]
use gpui_kit::component::Root;
#[cfg(mobile)]
use gpui_kit::{App, AppContext as _, WindowOptions};

#[cfg(mobile)]
use crate::ui::host;
#[cfg(mobile)]
use crate::ui::shared::config_model::{ConfigHolder, ConfigPath};
#[cfg(mobile)]
use crate::ui::shared::theme;
#[cfg(mobile)]
use crate::ui::shell::AppShell;

/// 装框架、主题集、读偏好、装书库与平台服务，然后开窗。顺序与桌面的 `run_desktop` 一致。
///
/// `gpui_kit::init` 必须最先：它把 `Theme` 与 `ThemeRegistry` 这两个 Global 建出来，
/// 之后任何一次 `cx.theme()` 才有东西可读。主题集要在它之后装，理由同理。
#[cfg(mobile)]
pub(crate) fn boot(cx: &mut App) {
    gpui_kit::init(cx);
    theme::register_builtin_themes(cx);
    theme::register_user_themes(cx);
    host::install(cx);

    let config = Config::load();
    config.language.locale.apply();
    cx.set_global(ConfigPath(Some(folio_core::config_path())));
    ConfigHolder::install(config, cx);

    // 书库挂成全局（与桌面同一份实现）。打不开时只记日志：界面照起，
    // 只是导入与进度落不了盘。
    match folio_core::Store::open_default() {
        Ok(store) => crate::ui::shared::app_store::install(store, cx),
        Err(err) => log::error!("boot: 书库打不开：{err}"),
    }

    // 还没有窗口，浅深只能问平台 —— 而 Android 上平台级那一问恒为深色。
    // 这只是「先有个能看的配色」，真正的跟随系统由 `AppShell::render` 拿窗口自己的外观再涂。
    theme::apply(&ConfigHolder::appearance(cx), None, cx);

    open_main_window(cx);
    cx.activate(true);
}

/// 打开应用窗口：根视图是 `Root` 包着的 [`AppShell`]。三个移动平台上窗口都是全屏。
///
/// 外面必须套一层 `Root`：`gpui_kit::component` 的每个组件都从 `Root` 建立的窗口级
/// 主题与 `rem` 上下文里取尺寸与配色，没有它界面就和桌面差一档。
#[cfg(mobile)]
fn open_main_window(cx: &mut App) {
    match cx.open_window(
        WindowOptions {
            window_bounds: None,
            ..Default::default()
        },
        |window, cx| {
            // 「跟随系统」在 Android 上只能问窗口：平台级那一问固定返回深色。
            theme::apply(&ConfigHolder::appearance(cx), Some(window), cx);
            let shell = cx.new(|cx| AppShell::boot(window, cx));
            cx.new(|cx| Root::new(shell, window, cx))
        },
    ) {
        Ok(_handle) => log::info!("open_main_window: 窗口已打开"),
        Err(err) => log::error!("open_main_window: 打开失败：{err:#}"),
    }
}
