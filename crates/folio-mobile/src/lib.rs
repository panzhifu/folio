//! Folio 的移动端口：**一个独立的 cargo workspace**。
//!
//! 为什么要独立（根 Cargo.toml 里把本目录 `exclude` 了）：
//! - 桌面端 `folio-app` 的 GPUI 是 crates.io 的 `gpui-pre`，Android 平台层来自
//!   `gpui-mobile` fork，它带的是 Zed 的 git `gpui`。两个都叫 `gpui` 的 lib
//!   不能进同一个二进制。
//! - `[patch.crates-io]` 只在 workspace 根生效，而 `async-task` 那个补丁
//!   桌面端不该继承。
//!
//! 所以这里只依赖 `folio-core`（不含 gpui，已验证能交叉编译到
//! `aarch64-linux-android`），UI 层对着移动端的 GPUI 重写。
//!
//! ## 入口
//!
//! - **Android**：`android-activity` 载入 `libfolio_mobile.so` 后在专用原生线程调
//!   [`android_main`]，它**不返回**直到 activity 结束。
//! - **iOS**：`main.m` 调 [`gpui_ios_register_app`] 注册根视图，再交给 UIKit 生命周期。
//! - **HarmonyOS**：`XComponent` 按 `libraryname` 载入 `.so`，NAPI 模块构造器接上
//!   [`ohos_napi_init`]。
//! - **宿主（Linux/macOS）**：没有移动平台层，只保留 [`host_main`] 让 `cargo check` 过得去。
//!
//! ## 窗口生命周期（Android）
//!
//! 不在启动时 `cx.open_window(...)`：原生 surface 就绪后系统才发
//! `MainEvent::InitWindow`。`Application::with_platform(..).run(..)` 在 Android 上
//! **按设计阻塞**（它驱动原生事件循环），`|cx| { … }` 那个闭包被推迟到
//! `InitWindow` 处理完、真的存在 `AndroidWindow` 之后才执行 —— 于是 `Application`
//! 连同它的 `Rc<RefCell<AppContext>>` 一直在这一帧栈上活着，GPUI 回调持有的
//! `Weak` 引用不会失效。照抄 `gpui-mobile/example/src/lib.rs:85` 与 `:257`。
//!
//! ## 构建
//!
//! ```text
//! rustup target add aarch64-linux-android
//! export ANDROID_NDK_HOME=/opt/android-ndk
//! # -P 26 不能省：cargo-ndk 默认 min-API 是 21，而 NDK r30 的 sysroot 里
//! # libnativewindow.so（ndk-sys / android-activity 要链）只在 API 26+ 目录里。
//! cargo ndk -t arm64-v8a -P 26 build      # → libfolio_mobile.so
//! ```

// 链接 gpui-mobile，让它的符号（jni 辅助、platform 等）在 cdylib 里可用。
extern crate gpui_mobile;

pub mod document;
pub mod measure;
pub mod paint;
pub mod reader_view;
pub mod shell;

#[cfg(any(target_os = "ios", target_os = "android", target_env = "ohos"))]
use gpui::{App, AppContext as _, WindowOptions};

#[cfg(target_os = "android")]
use gpui::Application;

#[cfg(target_os = "android")]
use gpui_mobile::android::jni;

#[cfg(any(target_os = "ios", target_os = "android", target_env = "ohos"))]
use shell::ShellView;

// ═══════════════════════════════════════════════════════════════════════════
// Android 入口
// ═══════════════════════════════════════════════════════════════════════════

/// 由 `android-activity` 在专用原生线程调用。不返回，直到可以退出。
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: android_activity::AndroidApp) {
    // 先装 logger —— 之后所有日志都要能在 logcat 里看见。
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("folio-mobile"),
    );

    // panic 也进 logcat，而不是默默 abort。
    jni::install_panic_hook();

    log::info!("android_main: entered");

    let _platform = jni::init_platform(&app);
    log::info!("android_main: platform initialised");

    // 拿到能交给 GPUI 的 SharedPlatform（全局 Arc<AndroidPlatform> 的 Rc 包装）。
    let shared = match jni::shared_platform() {
        Some(s) => s,
        None => {
            log::error!("android_main: shared_platform() returned None — aborting");
            return;
        }
    };

    log::info!("android_main: creating GPUI Application");

    Application::with_platform(shared.into_rc()).run(|cx: &mut App| {
        log::info!("android_main: run callback — opening reader window");
        open_main_window(cx);
    });

    log::info!("android_main: Application::run returned — activity will finish");
}

// ═══════════════════════════════════════════════════════════════════════════
// iOS 入口
// ═══════════════════════════════════════════════════════════════════════════

/// 把根视图注册给 GPUI 的 iOS 平台。由 `main.m` 在 `run_app()` **之前**调用，
/// 符号在 staticlib 里被 Xcode 强链接。
#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
pub extern "C" fn gpui_ios_register_app() {
    gpui_mobile::ios::ffi::set_app_callback(Box::new(|cx: &mut App| {
        open_main_window(cx);
    }));
}

/// `main.rs` 二进制用的便捷入口。
#[cfg(target_os = "ios")]
pub fn ios_main() {
    gpui_ios_register_app();
    gpui_mobile::ios::ffi::run_app();
}

// ═══════════════════════════════════════════════════════════════════════════
// HarmonyOS 入口
// ═══════════════════════════════════════════════════════════════════════════

/// OHOS NAPI 运行时的模块初始化。ArkTS 的 `XComponent` 用
/// `libraryname: 'folio_mobile'` 载入本 `.so` 时调用。
/// 根视图同样等到 `OnSurfaceCreated` 交出原生窗口才打开。
#[cfg(target_env = "ohos")]
pub extern "C" fn ohos_napi_init(
    env: gpui_mobile::ohos::napi::NapiEnv,
    exports: gpui_mobile::ohos::napi::NapiValue,
) -> gpui_mobile::ohos::napi::NapiValue {
    // SAFETY: `env` / `exports` 直接来自 NAPI 运行时。
    unsafe {
        gpui_mobile::ohos::napi::install(
            env,
            exports,
            "folio_mobile",
            Box::new(|cx: &mut App| open_main_window(cx)),
        );
    }
    exports
}

// 模块名必须与 `XComponent` 的 `libraryname`（即 `libfolio_mobile.so`）一致。
#[cfg(target_env = "ohos")]
gpui_mobile::gpui_ohos_napi_module!("folio_mobile", crate::ohos_napi_init);

// ═══════════════════════════════════════════════════════════════════════════
// 共享的窗口创建
// ═══════════════════════════════════════════════════════════════════════════

/// 打开应用窗口：根视图是 `shell::ShellView`（正文 + 底部导航栏）。
/// 三个平台上窗口都是全屏，所以 `window_bounds` 给 `None`。
#[cfg(any(target_os = "ios", target_os = "android", target_env = "ohos"))]
fn open_main_window(cx: &mut App) {
    match cx.open_window(
        WindowOptions {
            window_bounds: None,
            ..Default::default()
        },
        |_, app| app.new(|cx| ShellView::boot(cx)),
    ) {
        Ok(_handle) => log::info!("open_main_window: 窗口已打开"),
        Err(err) => log::error!("open_main_window: 打开失败：{err:#}"),
    }
    cx.activate(true);
}

// ═══════════════════════════════════════════════════════════════════════════
// 宿主（非移动平台）
// ═══════════════════════════════════════════════════════════════════════════

/// 宿主上没有移动平台层，窗口起不来。留着这个函数只为让 `cargo check` /
/// `cargo clippy` 在 x86 主机上过得去（`main.rs` 里对应的 `fn main` 也只打一行提示）。
#[cfg(not(any(target_os = "ios", target_os = "android", target_env = "ohos")))]
pub fn host_main() {
    eprintln!(
        "folio-mobile 只为 iOS / Android / HarmonyOS 而写。\
         请用 `cargo ndk -t arm64-v8a build`（Android）或 `--target aarch64-apple-ios`（iOS）。"
    );
}
