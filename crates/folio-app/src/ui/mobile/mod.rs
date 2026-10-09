//! 移动端（Android / iOS / HarmonyOS）：平台入口 + 外壳视图。
//!
//! 这是 [`super`] 的一个子模块，只在移动 target 上编译（`cfg(mobile)`，见 `build.rs`）。
//! 平台层来自 `gpui-pre-mobile`（lib 名 `gpui_mobile`，长桥/社区的兼容 fork），
//! 排版与书库原样复用 `folio-core`，视图地基与桌面共用 [`super::shared`]
//! —— 两端的 `gpui` 是同一个 `gpui-pre` 快照，`gpui_kit` 顶层把它整个转了出来。
//!
//! ## 入口
//!
//! - **Android**：`android-activity` 载入 `libfolio_app.so` 后在专用原生线程调
//!   [`android_main`]，它**不返回**直到 activity 结束。
//! - **iOS**：`main.m` 调 [`gpui_ios_register_app`] 注册根视图，再交给 UIKit 生命周期。
//! - **HarmonyOS**：`XComponent` 按 `libraryname` 载入 `.so`，NAPI 模块构造器接上
//!   [`ohos_napi_init`]。
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
//! ## 落盘位置
//!
//! 移动平台没有 `$HOME`，也没有「用户配置目录」这个概念，可写路径只有宿主在启动时
//! 报的那一个应用专属目录，所以在这里调 [`folio_core::paths::set_app_dirs`]。
//! 报早于读：配置与数据库的路径都在那之后才解析。
//!
//! ## 构建
//!
//! ```text
//! rustup target add aarch64-linux-android
//! export ANDROID_NDK_HOME=/opt/android-ndk
//! # -P 26 不能省：cargo-ndk 默认 min-API 是 21，而 NDK r30 的 sysroot 里
//! # libnativewindow.so（ndk-sys / android-activity 要链）只在 API 26+ 目录里。
//! cd crates/folio-app
//! cargo ndk -t arm64-v8a -P 26 -o android/app/src/main/jniLibs build --release --lib
//! # → libfolio_app.so（[lib] name = "folio_app"）
//! ```

// 链接 gpui-mobile，让它的符号（jni 辅助、platform 等）在 cdylib 里可用。
extern crate gpui_mobile;

pub mod paint;
pub mod reader_view;
pub mod shell;

#[cfg(any(target_os = "ios", target_os = "android", target_env = "ohos"))]
use gpui_kit::{App, AppContext as _, WindowOptions, component::Root};

#[cfg(target_os = "android")]
use gpui_kit::Application;

#[cfg(target_os = "android")]
use gpui_mobile::android::jni;

#[cfg(any(target_os = "ios", target_os = "android", target_env = "ohos"))]
use crate::ui::shared::{
    config_model::{ConfigHolder, ConfigPath},
    theme,
};
#[cfg(any(target_os = "ios", target_os = "android", target_env = "ohos"))]
use folio_core::Config;
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

    // 应用专属内部目录（`/data/user/0/<包名>/files`）：属主是本应用，
    // 不需要存储权限，进程被杀也还在。拿不到就维持原样 —— 读写会失败，
    // 但界面仍然能起来，这条日志说明为什么偏好没保住。
    match app.internal_data_path() {
        Some(root) => {
            let files = root.join("files");
            log::info!("android_main: 应用目录 {files:?}");
            folio_core::paths::set_app_dirs(files.join("folio/config"), files.join("folio/data"));
        }
        None => {
            log::error!("android_main: internal_data_path() 给了 None，偏好与进度无法落盘");
        }
    }

    // 拿到能交给 GPUI 的 SharedPlatform（全局 Arc<AndroidPlatform> 的 Rc 包装）。
    let shared = match jni::shared_platform() {
        Some(s) => s,
        None => {
            log::error!("android_main: shared_platform() returned None — aborting");
            return;
        }
    };

    log::info!("android_main: creating GPUI Application");

    // `with_assets` 决定 `svg().path("icons/xxx.svg")` 能否解析出字节。不调它时
    // gpui 用的是默认源 `()`，它对任何路径都返回 `Ok(None)` —— 于是 `Icon` /
    // `IconName` 不报错，只是画出一片空白。这里注册的 `AllAssets` 是整套内嵌
    // Lucide 目录（1830 个 SVG，源码 0.71 MB，随二进制一起打包，不走文件系统），
    // 与桌面端入口注册的是同一份，所以两端的图标名可以通用。
    //
    // iOS / HarmonyOS 上做不到这一步：那两端的 `Application` 在 gpui-mobile 内部
    // 构造（`ios::ffi::run_app`、napi 入口），只有 Android 有 `host::start_with_assets`
    // 这类注册口，所以移动端的图标目前只保证 Android。
    Application::with_platform(shared.into_rc())
        .with_assets(gpui_kit::assets::AllAssets)
        .run(|cx: &mut App| {
            log::info!("android_main: run callback — booting the app shell");
            boot(cx);
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
        boot(cx);
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
/// `libraryname: 'folio_app'` 载入本 `.so` 时调用。
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
            "folio_app",
            Box::new(|cx: &mut App| boot(cx)),
        );
    }
    exports
}

// 模块名必须与 `XComponent` 的 `libraryname`（即 `libfolio_app.so`）一致。
#[cfg(target_env = "ohos")]
gpui_mobile::gpui_ohos_napi_module!("folio_app", crate::ui::mobile::ohos_napi_init);

// ═══════════════════════════════════════════════════════════════════════════
// 三个平台共用的启动
// ═══════════════════════════════════════════════════════════════════════════

/// 装框架、装主题集、读偏好，然后开窗口。顺序与桌面的 `run_desktop` 一致。
///
/// `gpui_kit::init` 必须最先：它把 `Theme` 与 `ThemeRegistry` 这两个 Global 建出来，
/// 之后任何一次 `cx.theme()` 才有东西可读。主题集要在它之后装，理由同理。
#[cfg(any(target_os = "ios", target_os = "android", target_env = "ohos"))]
fn boot(cx: &mut App) {
    gpui_kit::init(cx);
    theme::register_builtin_themes(cx);
    theme::register_user_themes(cx);

    let config = Config::load();
    config.language.locale.apply();
    cx.set_global(ConfigPath(Some(folio_core::config_path())));
    ConfigHolder::install(config, cx);

    // 还没有窗口，浅深只能问平台 —— 而 Android 上平台级那一问恒为深色。
    // 所以这只是「先有个能看的配色」，真正的跟随系统由 `ShellView::render`
    // 拿着窗口自己的外观再涂一次。改偏好后的落盘与重涂同样由那个视图负责，
    // 订阅留在视图手上才不会因为 `Subscription` 被丢弃而静默失效。
    theme::apply(&ConfigHolder::appearance(cx), None, cx);

    open_main_window(cx);
    cx.activate(true);
}

/// 打开应用窗口：根视图是 `Root` 包着的 [`shell::ShellView`]（正文 + 顶部章导航 + 底部栏）。
/// 三个平台上窗口都是全屏，所以 `window_bounds` 给 `None`。
///
/// 外面必须套一层 `Root`：`gpui_kit::component` 的每个组件都从 `Root` 建立的窗口级
/// 主题与 `rem` 上下文里取尺寸与配色，没有它组件会退回自己的默认值，
/// 界面就和桌面差一档。
#[cfg(any(target_os = "ios", target_os = "android", target_env = "ohos"))]
fn open_main_window(cx: &mut App) {
    match cx.open_window(
        WindowOptions {
            window_bounds: None,
            ..Default::default()
        },
        |window, cx| {
            // 「跟随系统」在 Android 上只能问窗口：`cx.window_appearance()` 走的是
            // 平台级实现，那个实现固定返回深色；窗口级 appearance 由 JNI 真的查了
            // 系统的夜间模式（`gpui-mobile/src/android/window.rs:1285`）。
            theme::apply(&ConfigHolder::appearance(cx), Some(window), cx);
            let shell = cx.new(ShellView::boot);
            cx.new(|cx| Root::new(shell, window, cx))
        },
    ) {
        Ok(_handle) => log::info!("open_main_window: 窗口已打开"),
        Err(err) => log::error!("open_main_window: 打开失败：{err:#}"),
    }
}
