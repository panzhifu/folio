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
            // 灰度 AA 而不是平台建议的次像素 AA（wgpu 后端 `recommended_rendering_mode`
            // 恒回 `Subpixel`，见 `gpui-pre-wgpu` 的 `cosmic_text_system.rs`）。
            //
            // 这不是随手设的：这一笔落在**整屏一次进来**的那几个帧上（开书第一屏、跳章、
            // 被合帧的快 fling），次像素那条路要 44–52 ms —— 差在 swash 对 CJK 轮廓做 LCD
            // 渲染的代价与图集里每格三通道的字节数上。3 倍屏上肉眼差别很小（灰度也是
            // Android 上多数应用的正文口径）。
            //
            // 两个旧说法 2026-10-15 用同一个探针字段 `glyph_only_us` 重量过，都不准：
            // 灰度**没**降到当年记的「6–10 ms」（冷一屏读到 26.9–44.2 ms / 314–384 字，
            // 暖一屏 15.2 ms）；而它也**不是**「每翻一屏都要付一次」—— 慢滚时新行逐行进屏，
            // 每帧只 0.4–0.5 ms，4080 帧连续滚动里 ≥8 ms 的一共两条。设这一句的理由仍然
            // 成立（省的就是那两条），但别拿它当「滚动卡顿的主因」。数、三个新计数与
            // 帧间隔的归因都在 `ui/features/reader/perf.rs` 的模块头。
            //
            // 想换回去就删掉这一句：观感优先于这点帧预算时，它不该拦着。
            cx.set_text_rendering_mode(gpui_kit::TextRenderingMode::Grayscale);
            super::boot(cx, resolved);
        });

    // `run` 返回 = activity 即将结束、`App` 已 drop（它的 `AppStore` 句柄也放了）。
    // 这时按 §1.3/§5.6 的有序收尾：停监听/传书 → 关作业池 → 放掉最后一个书库句柄。
    crate::ui::shared::services::shutdown();

    tracing::info!("android_main: Application::run returned — activity will finish");

    // 收尾之后**必须把这个进程带走**，不能只是让 `android_main` 返回。
    //
    // 理由是两条撞在一起的事实：
    // - `android-activity` 每条 Activity 实例都换一根**新的** native 线程跑 `android_main`；
    // - `gpui_mobile` 的 `PLATFORM` 是进程内的 `OnceLock`，里面那个 `AndroidDispatcher`
    //   记的是**第一次**那根线程的 `ALooper`（`AndroidDispatcher::new()` 当场
    //   `ALooper_forThread()` 存的）。
    //
    // 所以只要进程活到下一次进入，新的 `android_main` 就会拿旧 looper 去
    // `App::new_app` → `assert!(is_main_thread(), "must construct App on main thread")`
    // → SIGABRT。2026-10-18 在 Meizu 20 上实测到整条链：按返回键结束 Activity 后进程留在
    // 后台（logcat：`The Process dev.folio.reader Already Exists in BG. So sending its
    // PID: 28388`），再点开是同一 pid、新的 tid 28688，日志停在
    // `android_main: creating GPUI Application` 紧跟一句
    // `PANIC ... app.rs:927:9: must construct App on main thread`。第三次能进是因为那次
    // abort 已经把进程带走了 —— 也就是说「闪退一次」本身就是这个 bug 的自愈。
    //
    // 同一根因还有第二处症状：第二次进入时 `日志装不上：这个进程里只有一个 subscriber 的
    // 位置`（tracing 的全局 dispatcher 也只有一个坑位），所以那一次连日志都是残的。
    //
    // 按 HOME 退到后台**不走这条路**：Activity 没结束、`run` 不返回，进程留着是好事
    // （再进来是瞬时的，不用重开书、不用重排）。这里退的只有「Activity 已经决定结束」
    // 那一种情况，而它下一次进来本来就要重建整个应用。
    //
    // 真正的长期解不在这一句：要么把 `PLATFORM` 做成可重绑（要动 gpui-mobile 那份 fork），
    // 要么迁到 fork 里 `android::host` 那条进程常驻渲染线程的路（它存在的理由就是这一条）。
    //
    // 用 `_exit` 而不是 `std::process::exit`：后者会跑 atexit 与全局析构，而 Android 的
    // HWUI 工作线程正好去拿那把已经被销毁的锁 —— 2026-10-18 实测
    // `FORTIFY: pthread_mutex_lock called on a destroyed mutex` → `Fatal signal 6` in
    // `hwuiTask0`，「不闪退」的退出自己变成了闪退。`_exit` 直接陷进 `exit_group`，一个
    // 处理器都不跑；上面那句 `run returned` 与有序收尾都已经写完了，日志的最后一两行可能
    // 留在缓冲区里没落盘，换一次不崩的启动是划算的。
    unsafe { libc::_exit(0) };
}

/// 藏起 / 还原系统栏（阅读沉浸）。状态栏与导航栏一起藏；读者按完返回、书还在往外走的那一段，
/// 阅读层自己让开顶上那条线（挖孔 / 状态栏）与底下那条线，所以藏栏不会让返回栏贴到屏幕顶。
/// 递给 Java，它自己切回 UI 线程，所以这里可以在 GPUI 的原生线程上调用而不阻塞。失败只写日志：
/// 藏不成只是顶上多一条状态栏，不该把阅读页拖退。
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

/// 把整个任务丢回后台（返回键走到栈底那一档，见 `platform::send_task_to_back` 那条）。
///
/// 形状照 [`set_immersive`]：递给 Java、它自己切回 UI 线程（`moveTaskToBack` 是
/// `@MainThread`，在 GPUI 的原生线程直调会撞 `CalledFromWrongThreadException`），失败只写
/// 日志 —— 退不回后台不该把返回键这一按变成崩，最坏情况就是再按一次 HOME。
pub(crate) fn send_task_to_back() {
    use gpui_mobile::android::jni::{activity, find_app_class, with_env};
    // 同上：本文件顶部把 `gpui_mobile::android::jni` 引进成了 `jni`，这里要走 crate 根。
    use ::jni::objects::JValue;
    match with_env(|env| {
        let act = activity(env)?;
        let cls = find_app_class(env, "dev.gpui.mobile.GpuiSystemUi")?;
        env.call_static_method(
            &cls,
            ::jni::jni_str!("moveTaskToBack"),
            ::jni::jni_sig!("(Landroid/app/Activity;Z)V"),
            &[JValue::Object(&act), JValue::Bool(true)],
        )
        .map(|_| ())
        .map_err(|err| err.to_string())
    }) {
        Ok(()) => tracing::info!("send_task_to_back: 已递给系统"),
        Err(err) => tracing::warn!("send_task_to_back: {err}"),
    }
}

/// 阅读界面的亮度：这一屏要压多暗（`None` = 交还系统）。
///
/// 这一格是**窗口级**的背光覆盖（对面 `GpuiSystemUi.setWindowBrightness` 写
/// `WindowManager.LayoutParams.screenBrightness`）：不需要权限，不动别的 app、也不改
/// 系统设置；读者离开这一屏时把 -1.0（`BRIGHTNESS_OVERRIDE_NONE`）写回去就是交还。
///
/// **必须切回 UI 线程**（对面自己在 Java 里 `runOnUiThread`）：`Window.setAttributes`
/// 会走到 `ViewRootImpl.requestLayout`，那一条自 API 36 起有线程检查 —— 实测 MEIZU 20 /
/// Android 16 上，在 GPUI 的原生线程直写抛
/// `CalledFromWrongThreadException: ... Expected: main Calling: android_main`，
/// 而 fork 那版 `gpui_mobile::set_screen_brightness` 正是在那个线程直写（见下）。
/// 形状照 [`set_immersive`]：递给 Java、它自己切线程，失败只写日志 —— 背光没跟上
/// 是小事，不该把翻页拖退。
///
/// 为什么不再调 fork 的 `gpui_mobile::set_screen_brightness`：那个实现在原生线程上
/// 直接 `getAttributes` / `setAttributes`，在 Android 16 上每一次调用都落进那条线程
/// 检查（只能吞掉那个 Java 异常），所以那一格在真机上从来没真正生效过。
/// 阈值与 sentinel 与它保持一致：`Some` 夹到 0.0–1.0，`None` / 非有限值归 -1.0。
pub(crate) fn set_screen_brightness(value: Option<f32>) {
    let wanted = match value {
        Some(v) if v.is_finite() => v.clamp(0.0, 1.0),
        _ => -1.0,
    };
    use ::jni::objects::JValue;
    use gpui_mobile::android::jni::{activity, find_app_class, with_env};
    match with_env(|env| {
        let act = activity(env)?;
        let cls = find_app_class(env, "dev.gpui.mobile.GpuiSystemUi")?;
        env.call_static_method(
            &cls,
            ::jni::jni_str!("setWindowBrightness"),
            ::jni::jni_sig!("(Landroid/app/Activity;F)V"),
            &[JValue::Object(&act), JValue::Float(wanted)],
        )
        .map(|_| ())
        .map_err(|err| err.to_string())
    }) {
        Ok(()) => tracing::debug!("set_screen_brightness: {wanted} 已递给系统"),
        Err(err) => tracing::warn!("set_screen_brightness: {err}"),
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

/// 把一张图的字节交给系统分享面板（对面是 `GpuiSystemUi.shareImage`）。
///
/// 字节从这边过去而不是递一个文件路径：图存在容器里（EPUB 是 zip），Reader 手上本来
/// 就没有盘上的文件；那边收下字节自己落进 `cache/share/` 再用 FileProvider 发出去。
/// 形状照 [`set_immersive`]：递给 Java、它自己切回 UI 线程，失败只写日志。
pub(crate) fn share_image(bytes: &[u8], mime: &str, name: &str) {
    use ::jni::objects::JValue;
    use gpui_mobile::android::jni::{activity, find_app_class, with_env};
    match with_env(|env| {
        let act = activity(env)?;
        let cls = find_app_class(env, "dev.gpui.mobile.GpuiSystemUi")?;
        let data = env
            .byte_array_from_slice(bytes)
            .map_err(|e| e.to_string())?;
        let mime = env.new_string(mime).map_err(|e| e.to_string())?;
        let name = env.new_string(name).map_err(|e| e.to_string())?;
        env.call_static_method(
            &cls,
            ::jni::jni_str!("shareImage"),
            ::jni::jni_sig!("(Landroid/app/Activity;[BLjava/lang/String;Ljava/lang/String;)V"),
            &[
                JValue::Object(&act),
                JValue::Object(&data),
                JValue::Object(&mime),
                JValue::Object(&name),
            ],
        )
        .map(|_| ())
        .map_err(|err| err.to_string())
    }) {
        Ok(()) => tracing::debug!("share_image: 已递给系统"),
        Err(err) => tracing::warn!("share_image: {err}"),
    }
}

/// 把一张图存进系统相册（对面是 `GpuiSystemUi.saveImageToGallery`）。
pub(crate) fn save_image(bytes: &[u8], mime: &str, name: &str) {
    use ::jni::objects::JValue;
    use gpui_mobile::android::jni::{activity, find_app_class, with_env};
    match with_env(|env| {
        let act = activity(env)?;
        let cls = find_app_class(env, "dev.gpui.mobile.GpuiSystemUi")?;
        let data = env
            .byte_array_from_slice(bytes)
            .map_err(|e| e.to_string())?;
        let mime = env.new_string(mime).map_err(|e| e.to_string())?;
        let name = env.new_string(name).map_err(|e| e.to_string())?;
        env.call_static_method(
            &cls,
            ::jni::jni_str!("saveImageToGallery"),
            ::jni::jni_sig!("(Landroid/app/Activity;[BLjava/lang/String;Ljava/lang/String;)V"),
            &[
                JValue::Object(&act),
                JValue::Object(&data),
                JValue::Object(&mime),
                JValue::Object(&name),
            ],
        )
        .map(|_| ())
        .map_err(|err| err.to_string())
    }) {
        Ok(()) => tracing::debug!("save_image: 已递给系统"),
        Err(err) => tracing::warn!("save_image: {err}"),
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
