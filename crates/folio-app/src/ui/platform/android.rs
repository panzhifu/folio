//! Android 入口：`android-activity` 载入 `libfolio_app.so` 后在专用原生线程调用
//! `android_main`，它不返回，直到可以退出。

use gpui_kit::{App, Application};
use gpui_mobile::android::jni;
use std::fmt::Write as _;
use tracing::Subscriber;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Layer};

use folio_core::logging::BoxedLayer;

/// 由 `android-activity` 在专用原生线程调用。
#[unsafe(no_mangle)]
fn android_main(app: android_activity::AndroidApp) {
    // 先装 logcat 那一份 —— 它是 `log` facade 的后端，也是这个进程里唯一**在配置读到之前**
    // 就能看见的输出口。必须抢在 `logging::init` 之前：`tracing-subscriber` 装的时候会把
    // `log` 的全局位置让给 LogTracer，而我们的 logcat 那层正是通过这个 facade 落地的，
    // 两者反过来接就成了自己转发给自己。
    android_logger::init_once(
        android_logger::Config::default()
            // 下限开到 Trace：真正的筛选由 `Config::logging` 那一节做，别让这里再卡一道。
            .with_max_level(log::LevelFilter::Trace)
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

    // 目录报上来之后才读得到配置，而 `logging` 那一节就在配置里 —— 所以日志装在这里，
    // 不在 `super::boot` 里：那之前的这几行（JNI、平台、应用目录）正是真机上最容易出问题
    // 的一段，不能等到界面起来才有记录。`boot` 里那一次会返回 Err 并被忽略，
    // 这是预期的：一个进程只有一个 subscriber。
    let config = folio_core::Config::load();
    if let Err(err) = folio_core::logging::init(&config.logging, logcat_layer()) {
        log::error!("android_main: 日志装不上，只有 logcat：{err}");
    }

    // 拿到能交给 GPUI 的 SharedPlatform（全局 Arc<AndroidPlatform> 的 Rc 包装）。
    let shared = match jni::shared_platform() {
        Some(shared) => shared,
        None => {
            log::error!("android_main: shared_platform() returned None — aborting");
            return;
        }
    };

    tracing::info!("android_main: creating GPUI Application");

    // `with_assets` 决定 `svg().path("icons/xxx.svg")` 能否解析出字节；不调它图标会是一片空白。
    Application::with_platform(shared.into_rc())
        .with_assets(gpui_kit::assets::AllAssets)
        .run(|cx: &mut App| {
            tracing::info!("android_main: run callback — booting the app shell");
            super::boot(cx);
        });

    tracing::info!("android_main: Application::run returned — activity will finish");
}

/// 藏起 / 还原系统栏（阅读沉浸）。递给 Java，它自己切回 UI 线程，所以这里可以在
/// GPUI 的原生线程上调用而不阻塞。失败只写日志：藏不成只是顶上一条状态栏，
/// 不该把阅读页拖退。
pub(crate) fn set_immersive(on: bool) {
    use gpui_mobile::android::jni::{activity, find_app_class, with_env};
    // 本文件顶部把 `gpui_mobile::android::jni` 引进成了 `jni`，所以这里必须写 `::jni`
    // 走 crate 根，否则 `jni::objects` 指的是 fork 那个模块而不是 jni crate。
    use ::jni::objects::JValue;
    match with_env(|env| {
        let act = activity(env)?;
        let cls = find_app_class(env, "dev.gpui.mobile.GpuiSystemUi")?;
        env.call_static_method(
            &cls,
            ::jni::jni_str!("setImmersive"),
            ::jni::jni_sig!("(Landroid/app/Activity;Z)V"),
            &[JValue::Object(&act), JValue::Bool(on)],
        )
        .map(|_| ())
        .map_err(|err| err.to_string())
    }) {
        Ok(()) => tracing::debug!("set_immersive: {on} 已递给系统"),
        Err(err) => tracing::warn!("set_immersive: {err}"),
    }
}

/// logcat 那一层：把 `tracing` 的事件转给 `log` facade（它的后端是 `android_logger`）。
///
/// 只带正文与字段，不带时间戳与级别前缀 —— logcat 每行自己会打时间、优先级与 tag，
/// 文件里那份才是完整的。
fn logcat_layer() -> Option<BoxedLayer> {
    Some(Box::new(Logcat) as BoxedLayer)
}

struct Logcat;

impl<S: Subscriber> Layer<S> for Logcat {
    fn on_event(&self, event: &tracing::Event<'_>, _cx: Context<'_, S>) {
        let meta = event.metadata();
        let mut text = String::new();
        event.record(&mut IntoText(&mut text));
        // 级别是运行时才知道的，所以用 `log::log!`（它专门为这一种情况留了级别参数）。
        log::log!(
            target: meta.target(),
            log_level(meta.level()),
            "{text}"
        );
    }
}

/// 把一条事件拼成一句：正文 + 那些 `k = v` 的字段。
struct IntoText<'a>(&'a mut String);

impl Visit for IntoText<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        // `tracing` 把格式化出来的正文存在一个叫 `message` 的字段里。
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        } else {
            let _ = write!(self.0, "{}={value:?}", field.name());
        }
    }
}

fn log_level(level: &tracing::Level) -> log::Level {
    match *level {
        tracing::Level::ERROR => log::Level::Error,
        tracing::Level::WARN => log::Level::Warn,
        tracing::Level::INFO => log::Level::Info,
        tracing::Level::DEBUG => log::Level::Debug,
        tracing::Level::TRACE => log::Level::Trace,
    }
}
