//! Android 入口：`android-activity` 载入 `libfolio_app.so` 后在专用原生线程调用
//! `android_main`，它不返回，直到可以退出。

use gpui_kit::{App, Application};
use gpui_mobile::android::jni;
use std::fmt::Write as _;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use tracing::Subscriber;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Layer};

use folio_core::infra::logging::BoxedLayer;

/// 由 `android-activity` 在专用原生线程调用。
#[unsafe(no_mangle)]
fn android_main(app: android_activity::AndroidApp) {
    // 先装 logcat 那一份 —— 它是 `log` facade 的后端，也是这个进程里唯一**在配置读到之前**
    // 就能看见的输出口。必须抢在 `logging::init` 之前：`tracing-subscriber` 装的时候会把
    // `log` 的全局位置让给 LogTracer，而我们的 logcat 那层正是通过这个 facade 落地的，
    // 两者反过来接就成了自己转发给自己。
    install_logcat_logger();

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
            folio_core::infra::paths::set_app_dirs(
                files.join("folio/config"),
                files.join("folio/data"),
            );
        }
        None => {
            log::error!("android_main: internal_data_path() 给了 None，偏好与进度无法落盘");
        }
    }

    // 目录报上来之后才读得到配置，而 `logging` 那一节就在配置里 —— 所以日志装在这里，
    // 不在 `super::boot` 里：那之前的这几行（JNI、平台、应用目录）正是真机上最容易出问题
    // 的一段，不能等到界面起来才有记录。`boot` 里那一次会返回 Err 并被忽略，
    // 这是预期的：一个进程只有一个 subscriber。
    // 读配置 + 装日志一步到位（§5.6 配置 → 日志），配置留着传给 `boot` ——
    // **只读一次**（§2.3）：以前这里读一次、`boot` 里又读一次。
    let (resolved, log_err) = folio_core::infra::logging::init_from_config(logcat_layer());
    // 配置到手，logcat 那层的 target 过滤也就有了它要的那一档：folio 自己的按
    // `logging.level` 放行，依赖库固定 WARN 以上（判据收在 core 的 `is_own_target` /
    // `DEPS_LEVEL`，与 tracing 那层同一份口径）。装之前是默认的 Info，够启动那几行用。
    OWN_LEVEL.store(
        own_level_raw(folio_core::infra::logging::own_level(
            &resolved.effective.logging,
        )),
        Ordering::Relaxed,
    );
    if let Err(err) = log_err {
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
        .run(move |cx: &mut App| {
            tracing::info!("android_main: run callback — booting the app shell");
            super::boot(cx, resolved);
        });

    // `run` 返回 = activity 即将结束、`App` 已 drop（它的 `AppStore` 句柄也放了）。
    // 这时按 §1.3/§5.6 的有序收尾：停监听/传书 → 关作业池 → 放掉最后一个书库句柄。
    crate::ui::shared::services::shutdown();

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

/// 把音量键这一按还给系统：调一格 `STREAM_MUSIC`，并照常出系统音量条。
///
/// 为什么需要应用自己调：音量键交给 native 之后系统那一路就不再动了，而阅读页把它们花在
/// 翻页上 —— 于是「当前不在阅读页」这一档必须有人补这一次调整（对面那份 Java 是
/// `GpuiSystemUi.adjustVolume`）。形状照 [`set_immersive`]：递给 Java 它自己切回 UI 线程，
/// 失败只写日志 —— 音量没动是小事，不该把按键派发拖退。
pub(crate) fn adjust_volume(up: bool) {
    use ::jni::objects::JValue;
    use gpui_mobile::android::jni::{activity, find_app_class, with_env};
    match with_env(|env| {
        let act = activity(env)?;
        let cls = find_app_class(env, "dev.gpui.mobile.GpuiSystemUi")?;
        env.call_static_method(
            &cls,
            ::jni::jni_str!("adjustVolume"),
            ::jni::jni_sig!("(Landroid/app/Activity;Z)V"),
            &[JValue::Object(&act), JValue::Bool(up)],
        )
        .map(|_| ())
        .map_err(|err| err.to_string())
    }) {
        Ok(()) => tracing::debug!("adjust_volume: up={up} 已递给系统"),
        Err(err) => tracing::warn!("adjust_volume: {err}"),
    }
}

/// folio 自己的日志在 logcat 里放到哪一档（整数档位，Error=1 … Trace=5，见 [`own_level_raw`]）。
/// 由 `Config::logging.level` 定，装 subscriber 之前是默认的 `Info`（3，够启动那几行用）。
/// 依赖库不看它，见 [`FilteredLogcat`]。
static OWN_LEVEL: AtomicUsize = AtomicUsize::new(3);

/// logcat 那个 `log` 后端。只建一次（`log` 全局位置只让占一次）。
static LOGCAT: OnceLock<FilteredLogcat> = OnceLock::new();

/// 包 `android_logger` 一层，**按 target 挡**依赖库的噪音。
///
/// 为什么非要这一层：依赖库（html5ever / tendril / cssparser / selectors / wgpu …）用 `log`
/// facade，记录**直接落在这一层**，不经过 `Config::logging` 那套 tracing 过滤。不挡的话
/// html5ever 每个 token 都写一次 logcat —— 实测把导入一本书的解析从 ~0.3 s 拖到 21 s。
///
/// 挡法沿用同一份口径（[`folio_core::infra::logging::is_own_target`] +
/// [`DEPS_LEVEL`](folio_core::infra::logging::DEPS_LEVEL)）：folio 自己的按配置级别放行，
/// 其余固定 WARN 以上。这样 logcat 里也能看到 folio 的 debug，而不引来看库的刷屏。
///
/// 内部那个 `AndroidLogger` 的 `max_level` 设成 `Trace`：真正的筛选在这里做完，别让底下
/// 再卡一道（`AndroidLogger::log` 会自己再过一遍 `enabled`）。
struct FilteredLogcat {
    inner: android_logger::AndroidLogger,
}

impl log::Log for FilteredLogcat {
    fn enabled(&self, meta: &log::Metadata<'_>) -> bool {
        let cap = if folio_core::infra::logging::is_own_target(meta.target()) {
            OWN_LEVEL.load(Ordering::Relaxed)
        } else {
            own_level_raw(folio_core::infra::logging::DEPS_LEVEL)
        };
        // `log::Level`（Error=1 … Trace=5）与 `OWN_LEVEL` 里记的编号同序，直接比整数。
        meta.level() as usize <= cap
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            self.inner.log(record);
        }
    }

    fn flush(&self) {
        self.inner.flush();
    }
}

/// 装 logcat 后端。位置被别的东西占了（重复初始化）就作罢，与 `android_logger::init_once`
/// 同一条纪律：日志装不上不影响程序起来。
///
/// 全局 `max_level` 压到 `Info`：依赖库的 trace/debug 在这一道就被 `log` 宏挡下，**连格式化
/// 都省了** —— 这是把导入从 21 s 拉回 0.3 s 的那一步。folio 自己走 `tracing`，由
/// [`Logcat`] 那层绕过这道直接落进 logcat，所以 debug 不受影响。
fn install_logcat_logger() {
    let logger = LOGCAT.get_or_init(|| FilteredLogcat {
        inner: android_logger::AndroidLogger::new(
            android_logger::Config::default()
                .with_max_level(log::LevelFilter::Trace)
                .with_tag("folio-mobile"),
        ),
    });
    let _ = log::set_logger(logger);
    log::set_max_level(log::LevelFilter::Info);
}

/// tracing 那套 `LevelFilter`（`OFF`…`TRACE`）转成整数档位，好与 `log::Level` 的编号比。
/// 两套编号同序（Error=1 … Trace=5），所以只做一次映射就够。
fn own_level_raw(level: tracing_subscriber::filter::LevelFilter) -> usize {
    match level {
        tracing_subscriber::filter::LevelFilter::OFF => 0,
        tracing_subscriber::filter::LevelFilter::ERROR => 1,
        tracing_subscriber::filter::LevelFilter::WARN => 2,
        tracing_subscriber::filter::LevelFilter::INFO => 3,
        tracing_subscriber::filter::LevelFilter::DEBUG => 4,
        tracing_subscriber::filter::LevelFilter::TRACE => 5,
    }
}

/// logcat 那一层：把 `tracing` 的事件转给 `log` facade（它的后端是 [`FilteredLogcat`]）。
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
        // **不走 `log::log!`**：那个宏先按全局 `max_level` 拦一道，而全局那一档被压到
        // `Info`（好让依赖库的 trace/debug 连格式化都省掉）。folio 自己的 debug 要照进
        // logcat，所以这里直接造一条 record 交给 `log::logger()` —— 绕过全局那一道，
        // 但仍经过 [`FilteredLogcat`] 的 target 过滤（folio 那支按 `OWN_LEVEL` 放行）。
        // `format_args!` 先绑到 `let` 上：它的临时值要活到那一条 record 用完为止。
        let args = format_args!("{text}");
        let record = log::Record::builder()
            .level(log_level(meta.level()))
            .target(meta.target())
            .module_path(meta.module_path())
            .file(meta.file())
            .line(meta.line())
            .args(args)
            .build();
        log::logger().log(&record);
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
