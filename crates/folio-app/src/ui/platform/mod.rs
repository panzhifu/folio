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

// 三个平台的 `set_reader_fullscreen` 与 [`render_png_offscreen`] 的签名里都出现这些类型，
// 所以不按档位 gate。
use gpui_kit::{App, AsyncApp, Entity, Pixels, Render, Size, Window, WindowAppearance};

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
use gpui_kit::{AppContext as _, WindowOptions};

#[cfg(mobile)]
use crate::ui::host;
#[cfg(mobile)]
use crate::ui::services::config::{ConfigHolder, ConfigPath};

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
///   重复调用无副作用）。它把状态栏与导航栏一起藏掉；阅读层自己把返回栏留在状态栏那条线
///   以下（`shell::reading_top_inset`，沉浸态那条 inset 由挖孔给）。
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

/// 把整个任务丢回后台（等同按 HOME）—— 返回键走到栈底时走这一条，而不是退出进程。
///
/// 为什么要它而不是 `cx.quit()`：`quit` 那条路让进程在自己还挂着前台顶窗的时候消失，
/// 系统拿不到任务快照（logcat：`app died, no saved state`），于是没有过渡动画可播 ——
/// 读者看到的就是一次闪退。丢回后台则一切交给系统：动画是系统的，进程留着，再进来是
/// 瞬时恢复，真要退出还有「划掉后台卡片」那一条。
///
/// 只有 Android 有这个口（`Activity.moveTaskToBack`）。iOS / HarmonyOS 没有系统返回键，
/// 这条永远不会被走到，所以安静地什么都不做；桌面同样不接（那一端的答案是 `Stay`）。
#[cfg(target_os = "android")]
pub(crate) fn send_task_to_back() {
    android::send_task_to_back();
}

#[cfg(not(target_os = "android"))]
pub(crate) fn send_task_to_back() {}

/// 把一张图的字节交给系统分享面板。图片查看器那一层的「分享」按钮走它。
///
/// - **Android**：递给 `GpuiSystemUi.shareImage`，那边落进 `cache/share/` 再用
///   FileProvider 发 `ACTION_SEND`（见 [`android::share_image`]）。
/// - **iOS / HarmonyOS**：还没有这一档，安静 no-op。
///
/// 桌面端不经这条路：它由 `host::desktop` 直接落到下载目录（没有系统分享面板）。
#[cfg(mobile)]
pub(crate) fn share_image(bytes: &[u8], mime: &str, name: &str) {
    #[cfg(target_os = "android")]
    android::share_image(bytes, mime, name);
    #[cfg(not(target_os = "android"))]
    let _ = (bytes, mime, name);
}

/// 把一张图的字节存进系统相册。图片查看器那一层的「下载」按钮走它。
#[cfg(mobile)]
pub(crate) fn save_image(bytes: &[u8], mime: &str, name: &str) {
    #[cfg(target_os = "android")]
    android::save_image(bytes, mime, name);
    #[cfg(not(target_os = "android"))]
    let _ = (bytes, mime, name);
}

/// 打开一个目录：把它交给系统的文件管理器去显示。设置页那两格（配置文件在哪儿、
/// 自定义主题往哪儿放 JSON）走它。
///
/// 返回的是「有没有人接手」，不是「读者一定看见了」：
///
/// - **桌面**：起一个 `xdg-open`（Linux）/ `open`（macOS）/ `explorer`（Windows），
///   进程起来了就算 `true` —— 桌面环境那头开没开窗，这一层看不到。
/// - **Android / iOS**：递一条 `ACTION_VIEW` 给系统挑应用（fork 的 `url_launcher`，
///   走 JNI，所以和 [`share_image`] 一样只能在界面线程上调）。这一条是尽力而为：没有
///   应用认领这个 URI、或系统不肯收 `file://`（Android 7 起 StrictMode 对它有感）时
///   返回 `false`，界面上那一格仍留着路径本身，读者不至于什么也拿不到。
/// - **HarmonyOS**：没有这条路，`false`。
#[cfg(not(mobile))]
pub(crate) fn open_folder(path: &std::path::Path) -> bool {
    use std::process::Command;
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    match Command::new(opener).arg(path).spawn() {
        Ok(_) => true,
        Err(error) => {
            tracing::warn!("open_folder: {opener} 起不来：{error}");
            false
        }
    }
}

#[cfg(target_os = "android")]
pub(crate) fn open_folder(path: &std::path::Path) -> bool {
    use gpui_mobile::packages::url_launcher;
    // `path.display()` 以 `/` 开头，拼出来就是 `file:///…`。
    match url_launcher::launch_url(&format!("file://{}", path.display())) {
        Ok(launched) => launched,
        Err(error) => {
            tracing::warn!("open_folder: {error}");
            false
        }
    }
}

#[cfg(all(mobile, not(target_os = "android")))]
pub(crate) fn open_folder(_path: &std::path::Path) -> bool {
    false
}

/// 音量键落在**非阅读页**时，把这一按交给系统去调音量。
///
/// 阅读页把音量键花在翻页上（[`crate::ui::shell`] 里那条分流），而 Android 上
/// NativeActivity 会把音量键交给 native，系统那一路就不再动了 —— 所以「不翻页」的那一档
/// 必须有人补一次调整，否则音量键在书架、设置里就成了死键。
///
/// - **Android**：递给那份 Java（`GpuiSystemUi.adjustVolume`），调 `STREAM_MUSIC` 并带
///   `FLAG_SHOW_UI`，反馈与在应用外按一样。
/// - **桌面**：没有这条路 —— 桌面的音量键由桌面环境自己在按键派发之前处理掉，gpui 不会
///   把它当成一个 `Keystroke` 发进来，所以这里永远不会被调到。
/// - **iOS / HarmonyOS**：iOS 拿不到硬件音量键的事件（只能观察输出音量的变化，既不知道
///   「按下」也压不掉系统 HUD），所以「音量键翻页」这一档在 iOS 上做不出来；这一条静默
///   no-op 只是把口留着。
#[cfg(target_os = "android")]
pub(crate) fn adjust_system_volume(up: bool) {
    android::adjust_volume(up);
}

#[cfg(not(target_os = "android"))]
pub(crate) fn adjust_system_volume(_up: bool) {}

/// 阅读界面的亮度：把「压暗多少」这一格递给平台。`None` = 交还系统。
///
/// 与 [`set_reader_fullscreen`] 同一族——都是「书挂着的时候才接管」那一档，调用方在外壳
/// 那一处（`AppShell::reading_brightness`），值没变就不递。
///
/// - **Android**：递给 `GpuiSystemUi.setWindowBrightness`（见 `platform::android` 那一份
///   注释：它写窗口自己的 `screenBrightness`，且必须在 UI 线程上写 —— API 36 起
///   `Window.setAttributes` 有线程检查），`None` 传那个 `-1.0` 的 sentinel 把背光交还系统。
/// - **桌面**：背光不归窗口管（那是显示器的地盘，gpui 没有这个口），所以这里安静地不做
///   任何事。桌面上这一格由外壳盖的那一层压暗实现，读的同一个数
///   （`reader::chrome::dim_alpha`）—— 同一个读数、两种暗法。
/// - **iOS / HarmonyOS**：iOS 上唯一的口是**整机**亮度，动了会带走读者的系统设置，
///   这一版不接管。
#[cfg(target_os = "android")]
pub(crate) fn set_screen_brightness(value: Option<f32>) {
    android::set_screen_brightness(value);
}

#[cfg(not(target_os = "android"))]
pub(crate) fn set_screen_brightness(_value: Option<f32>) {}

/// 这一端的「界面亮度」是被**背光**压暗的，还是该由应用自己压一层黑。
///
/// 判据只有这一处：Android 有窗口级的背光口（[`set_screen_brightness`] 真能改背光），
/// 桌面上背光不归窗口管。阅读层拿这一个 bool 决定盖不盖那层黑 —— 两处各判一次，
/// 迟早有一端同一格暗两次（背光压一遍、黑层再压一遍），或者哪一边都不暗。
pub(crate) const BRIGHTNESS_IS_BACKLIGHT: bool = cfg!(target_os = "android");

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

/// 这一端能不能把一个视图在**不显示的窗口**里渲成一张图。分享卡片靠它。
///
/// 判据只写在这一处（与 [`BRIGHTNESS_IS_BACKLIGHT`] 同一个理由）：`gpui_mobile::offscreen`
/// 那个模块的门是 `all(feature = "offscreen", any(ios, android))`，桌面上它根本不存在。
/// 选区菜单据此决定摆不摆「分享卡片」那一格，[`render_png_offscreen`] 据此决定编不编
/// 那半实现 —— 两处各判一次，迟早有一端摆出一个按下去什么都不发生的按钮。
pub(crate) const RENDERS_SHARE_CARDS: bool = cfg!(any(target_os = "android", target_os = "ios"));

/// 在一个 `show: false` 的窗口里把 `make` 建出来的那个视图渲成一帧，交回 PNG 字节。
///
/// 整趟是**同步**的，但只能在界面线程「没有借用 `App`」的那一刻调：那一帧的像素是 GPUI
/// 通过窗口自己的回调交出来的，而那些回调自己借用 `App`。所以调用方要在 `cx.spawn`
/// 出来的任务里调它，不能停在按下事件的处理函数里 —— 那里 `App` 正被借用。
/// 开窗、渲帧、关窗都在这一个函数里，因此中间不会插入别的界面工作。
///
/// 渲不出来（开窗失败、那一帧没交出来、编码失败）返回 `None`，原因已经写进日志。
/// 一次分享没成不该把阅读页拖退 —— 与 [`crate::ui::host`] 那两条导出同一个立场。
///
/// `scale` 是这张图自己的设备像素比，**不是**读者屏幕的那一个：卡片是要离开应用的
/// 成品，它的像素数就该在每台设备上都一样，而不是跟着读者的屏幕密度忽大忽小。
#[cfg(any(target_os = "android", target_os = "ios"))]
pub(crate) fn render_png_offscreen<V>(
    size: Size<Pixels>,
    scale: f32,
    appearance: WindowAppearance,
    make: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
    cx: &mut AsyncApp,
) -> Option<Vec<u8>>
where
    V: Render + 'static,
{
    use gpui_mobile::offscreen::OffscreenWindow;
    use image::ImageEncoder as _;

    let window = match cx.update(|cx| OffscreenWindow::open(size, scale, appearance, cx, make)) {
        Ok(window) => window,
        Err(err) => {
            tracing::warn!("离屏渲染: 开不了那个不显示的窗口 —— {err:#}");
            return None;
        }
    };
    // 这一帧必须在两次 `update` 之间渲：`render` 自己会借 `App`。
    let rendered = window.render(size);
    cx.update(|cx| window.close(cx));
    let image = match rendered {
        Ok(image) => image,
        Err(err) => {
            tracing::warn!("离屏渲染: GPUI 没画出那一帧 —— {err:#}");
            return None;
        }
    };
    let mut buf = Vec::new();
    let encoded = image::codecs::png::PngEncoder::new(&mut buf).write_image(
        image.as_raw(),
        image.width(),
        image.height(),
        image::ExtendedColorType::Rgba8,
    );
    match encoded {
        Ok(()) => Some(buf),
        Err(err) => {
            tracing::warn!("离屏渲染: PNG 编码失败 —— {err}");
            None
        }
    }
}

/// 桌面与 HarmonyOS 那一支：没有离屏窗口可开，交不出字节。
///
/// 空臂与 [`set_screen_brightness`] 同一族写法 —— 界面那一侧因此一个 `cfg` 都不必写。
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub(crate) fn render_png_offscreen<V>(
    _size: Size<Pixels>,
    _scale: f32,
    _appearance: WindowAppearance,
    _make: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
    _cx: &mut AsyncApp,
) -> Option<Vec<u8>>
where
    V: Render + 'static,
{
    None
}

#[cfg(mobile)]
use crate::ui::foundation::error::{Context as _, report};
#[cfg(mobile)]
#[cfg(mobile)]
use crate::ui::foundation::theme;
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
            crate::ui::services::app_store::install(store, cx)
        }
        Err(err) => report("boot", &err),
    }

    // 常驻资源（导入作业池 + 监听 + 传书）收拢到一个进程级句柄里；书库句柄也交过去，
    // 好让关停时「作业池先于库连接销毁」（`plans/基础设施.md` §1.3）。
    crate::ui::services::resident::install(cx);
    // 诊断 dump（§5.8）：`FOLIO_DUMP=1` 时把生效配置 + 库版本 + 队列长度打一份。
    if folio_core::infra::diagnostics::enabled() {
        let provenance = ConfigHolder::provenance(cx);
        folio_core::infra::diagnostics::dump(
            &ConfigHolder::snapshot(cx),
            Some(&provenance),
            db_version,
            Some(crate::ui::services::resident::pool().queued()),
            // 物理内存还没接（平台层给），所以给 `None` —— 报「未知」也不报假数。
            None,
        );
    }
    // 清掉上次崩溃留在书库目录里的 `.part` 半成品（§1.3 末条）。
    crate::ui::services::library::sweep_partials(cx);
    // 老副本改名的那一趟：投进 Cleanup 档，不等它、也不在界面线程上算哈希。
    crate::ui::services::library::schedule_backfill(cx);

    // 还没有窗口，浅深只能问平台。这一趟只求「先有个能看的配色」：窗口一开，
    // `AppShell::render` 每帧按窗口自己的浅深判该不该重涂，系统翻面也有那条订阅接住。
    theme::apply(&ConfigHolder::appearance(cx), None, cx);

    // 监听是后台能力：配置开着就随启动起，活过任何一屏。
    crate::ui::services::watch::sync_from_config(cx);

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
            // 读窗口自己那一档：平台在开窗时已经把系统的夜间模式写进来（fork 的
            // `open_window` → `refresh_appearance`），gpui 建这个窗口时又取一次缓存，
            // 所以第一帧就是系统那一档，不必等第一次配置变更。
            theme::apply(&ConfigHolder::appearance(cx), Some(window), cx);
            let shell = cx.new(|cx| AppShell::boot(window, cx));
            cx.new(|cx| Root::new(shell, window, cx))
        },
    ) {
        Ok(_handle) => tracing::info!("open_main_window: 窗口已打开"),
        Err(err) => tracing::error!("open_main_window: 打开失败：{err:#}"),
    }
}
