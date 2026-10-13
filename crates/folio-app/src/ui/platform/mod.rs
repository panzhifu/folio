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

// 三个平台的 `set_reader_fullscreen` 签名里都出现它，所以不按档位 gate。
use gpui_kit::Window;

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
use folio_core::Resolved;
#[cfg(mobile)]
use gpui_kit::component::Root;
#[cfg(mobile)]
use gpui_kit::{App, AppContext as _, WindowOptions};

#[cfg(mobile)]
use crate::ui::host;
#[cfg(mobile)]
use crate::ui::shared::config_model::{ConfigHolder, ConfigPath};

/// 整机扫描前的存储授权：拿到了「所有文件访问」（或旧设备的读存储）没有。
///
/// **阻塞调用** —— Android 上会把调用线程停在闩上，直到读者从系统设置页回来
/// （fork `gpui-pre-mobile` 29f19d3 的 permission_handler 是同步契约），所以只能在
/// 后台线程上调，绝不能在界面线程上。API 30+ 看 MANAGE_EXTERNAL_STORAGE 的结果，
/// 更旧的设备看 READ_EXTERNAL_STORAGE（那一档只有它）；请求一并发出，拿到的
/// 任何一个都算数 —— 拿不到时扫描照跑，只是 roots 缩到应用自己的目录。
#[cfg(target_os = "android")]
pub(crate) fn request_storage_access() -> bool {
    use gpui_mobile::packages::permission_handler::{self, Permission};
    match permission_handler::request_permissions(&[
        Permission::ManageExternalStorage,
        Permission::Storage,
    ]) {
        Ok(statuses) => statuses.iter().any(|(permission, status)| {
            matches!(
                permission,
                Permission::ManageExternalStorage | Permission::Storage
            ) && status.is_granted()
        }),
        Err(err) => {
            tracing::warn!("request_storage_access: {err}");
            false
        }
    }
}

/// 非安卓平台没有这一道门：扫描的 roots 本来就在应用沙盒或家目录里。
#[cfg(not(target_os = "android"))]
pub(crate) fn request_storage_access() -> bool {
    true
}

/// 把窗口切到（或切回）系统级全屏：阅读页在最上面时，读者要的是整屏都是正文。
///
/// 三端各一份实现，因为「全屏」在每一端根本不是一回事：
///
/// - **桌面**：`Window::toggle_fullscreen` 是真 fullscreen（wayland 走
///   `set_fullscreen`，X11 翻 `_NET_WM_STATE_FULLSCREEN`）。它是个**翻转**而不是设定，
///   所以这里先读一次实际状态再决定要不要翻 —— 逐帧盲调会让窗口来回跳。
/// - **Android**：fork 只能改系统栏颜色，藏栏走我们自己那份 `GpuiSystemUi`（设定语义，
///   重复调用无副作用）。
/// - **iOS / HarmonyOS**：没有这个口，安静地什么都不做。
#[cfg(not(mobile))]
pub(crate) fn set_reader_fullscreen(on: bool, window: &mut Window) {
    if window.is_fullscreen() != on {
        window.toggle_fullscreen();
    }
}

#[cfg(target_os = "android")]
pub(crate) fn set_reader_fullscreen(on: bool, _window: &mut Window) {
    android::set_immersive(on);
}

#[cfg(all(mobile, not(target_os = "android")))]
pub(crate) fn set_reader_fullscreen(_on: bool, _window: &mut Window) {}

/// 状态栏图标那一档，跟着界面的浅深走：浅色界面要**深色**图标。
///
/// 不跟着翻会白图标画在白底上，整条状态栏看着像消失了 —— Android 15 起系统强制
/// edge-to-edge，状态栏底色是透明的，读者的界面直接画在它下面，图标色就成了唯一
/// 让它可见的东西。真机（Meizu 20 / Android 16）实测：顶上那条 116 物理像素纯白，
/// 而 `dumpsys window` 里 `StatusBar` 窗口 `isVisible=true`、`HAS_DRAWN`，应用也
/// 收到了 `content_rect_insets top=116`（位置留了，只是没东西看得见）。
///
/// 口子是 fork 现成的 `set_system_chrome`：`status_bar_color` / `navigation_bar_color`
/// 留 `None`（那两个 setter 在新系统上已被忽略，且这里要改的只有图标色），只把
/// `status_bar_style` 递过去。它自己带一份「上次应用过什么」的缓存，重复调不会
/// 再发 JNI，所以挂在换肤那条路上是安全的。
#[cfg(any(target_os = "android", target_os = "ios"))]
pub(crate) fn set_status_bar_icons(dark_icons: bool) {
    use gpui_mobile::{StatusBarContentStyle, SystemChromeStyle, set_system_chrome};
    set_system_chrome(&SystemChromeStyle {
        status_bar_style: if dark_icons {
            StatusBarContentStyle::Dark
        } else {
            StatusBarContentStyle::Light
        },
        ..SystemChromeStyle::default()
    });
}

/// 桌面与 HarmonyOS 没有这一档：桌面上窗口 chrome 由应用自绘（`TitleBar`），
/// 状态栏是操作系统的地盘，不归应用管。
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub(crate) fn set_status_bar_icons(_dark_icons: bool) {}

#[cfg(mobile)]
use crate::ui::shared::error::{Context as _, report};
#[cfg(mobile)]
#[cfg(mobile)]
use crate::ui::shared::theme;
#[cfg(mobile)]
use crate::ui::shell::AppShell;

/// 装框架、主题集、书库与平台服务，然后开窗。顺序与桌面的 `run_desktop` 一致。
///
/// `config` 由**入口**读好、日志也由入口装好之后再传进来（Android 在 `android_main` 里、
/// iOS / HarmonyOS 在各自的注册回调里，都走 `logging::init_from_config`）——`boot` 不再
/// 自己读配置，配置因此**只读一次**（§2.3）。
///
/// `gpui_kit::init` 必须最先：它把 `Theme` 与 `ThemeRegistry` 这两个 Global 建出来，
/// 之后任何一次 `cx.theme()` 才有东西可读。主题集要在它之后装，理由同理。
#[cfg(mobile)]
pub(crate) fn boot(cx: &mut App, resolved: Resolved) {
    gpui_kit::init(cx);
    // 移动三端的 `Application` 都是框架内部建的，构造链够不着，所以在这里换掉
    // gpui 默认那份「不发请求」的 client。
    theme::register_builtin_themes(cx);
    theme::register_user_themes(cx);
    host::install(cx);

    // 配置由入口读好、日志由入口装好再传进来（§5.6 配置→日志、§2.3 启动读一次）：
    // 以前这里自己再 `Config::load()` 一次，于是 Android 上读了两遍。`boot` 只负责
    // 界面与书库的接线，不再碰配置的来源与日志的装配。
    resolved.effective.language.locale.apply();
    cx.set_global(ConfigPath(Some(folio_core::config_path())));
    ConfigHolder::install_resolved(resolved, cx);

    // 书库挂成全局（与桌面同一份实现）。打不开时只记日志：界面照起，
    // 只是导入与进度落不了盘。`report` 落的是整条链 —— 「打不开」在设备上可能是
    // 目录不存在、权限、也可能是上一轮留下的坏库文件，只有链说得出是哪种。
    let mut db_version = None;
    match folio_core::Store::open_default()
        .map_err(anyhow::Error::from)
        .context("装书库")
    {
        Ok(store) => {
            // 启动现场（§4.2）：库是哪个 schema 版本。留一份给下面的诊断 dump（§5.8）。
            db_version = match store.user_version() {
                Ok(version) => {
                    tracing::info!("库 schema v{version}");
                    Some(version)
                }
                Err(err) => {
                    tracing::warn!("库 schema 版本读不出：{err:#}");
                    None
                }
            };
            // 源文件不在的那些书：连数据一起删掉（理由同桌面那一处）。
            match store.prune_missing() {
                Ok(gone) if !gone.is_empty() => {
                    tracing::warn!("boot: 清理了 {} 本源文件已不在的书：{gone:?}", gone.len());
                }
                Ok(_) => {}
                Err(err) => tracing::warn!("boot: 清理失效的书失败：{err:#}"),
            }
            crate::ui::shared::app_store::install(store, cx)
        }
        Err(err) => report("boot", &err),
    }

    // 常驻资源（导入作业池 + 监听 + 传书）收拢到一个进程级句柄里；书库句柄也交过去，
    // 好让关停时「作业池先于库连接销毁」（`plans/基础设施.md` §1.3）。
    crate::ui::shared::services::install(cx);
    // 诊断 dump（§5.8）：`FOLIO_DUMP=1` 时把生效配置 + 库版本 + 队列长度打一份。
    if folio_core::infra::diagnostics::enabled() {
        let provenance = ConfigHolder::provenance(cx);
        folio_core::infra::diagnostics::dump(
            &ConfigHolder::snapshot(cx),
            Some(&provenance),
            db_version,
            Some(crate::ui::shared::services::pool().queued()),
            // 物理内存还没接（平台层给），所以给 `None` —— 报「未知」也不报假数。
            None,
        );
    }
    // 清掉上次崩溃留在书库目录里的 `.part` 半成品（§1.3 末条）。
    crate::ui::shared::library::sweep_partials(cx);
    // 老副本改名的那一趟：投进 Cleanup 档，不等它、也不在界面线程上算哈希。
    crate::ui::shared::library::schedule_backfill(cx);

    // 还没有窗口，浅深只能问平台 —— 而 Android 上平台级那一问恒为深色。
    // 这只是「先有个能看的配色」，真正的跟随系统由 `AppShell::render` 拿窗口自己的外观再涂。
    theme::apply(&ConfigHolder::appearance(cx), None, cx);

    // 监听是后台能力：配置开着就随启动起，活过任何一屏。
    crate::ui::shared::watch::sync_from_config(cx);

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
        Ok(_handle) => tracing::info!("open_main_window: 窗口已打开"),
        Err(err) => tracing::error!("open_main_window: 打开失败：{err:#}"),
    }
}
