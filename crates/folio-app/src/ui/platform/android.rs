//! Android 入口：`android-activity` 载入 `libfolio_app.so` 后在专用原生线程调用
//! `android_main`，它不返回，直到可以退出。

use gpui_kit::{App, Application};
use gpui_mobile::android::jni;

/// 由 `android-activity` 在专用原生线程调用。
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
        Some(shared) => shared,
        None => {
            log::error!("android_main: shared_platform() returned None — aborting");
            return;
        }
    };

    log::info!("android_main: creating GPUI Application");

    // `with_assets` 决定 `svg().path("icons/xxx.svg")` 能否解析出字节；不调它图标会是一片空白。
    Application::with_platform(shared.into_rc())
        .with_assets(gpui_kit::assets::AllAssets)
        .run(|cx: &mut App| {
            log::info!("android_main: run callback — booting the app shell");
            super::boot(cx);
        });

    log::info!("android_main: Application::run returned — activity will finish");
}
