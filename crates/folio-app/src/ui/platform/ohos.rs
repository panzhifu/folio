//! HarmonyOS 入口。
//!
//! 应用是 NAPI 模块（cdylib），没有二进制入口。ArkTS 的 `XComponent` 用
//! `libraryname: 'folio_app'` 载入本 `.so` 时调用 [`ohos_napi_init`]。

use gpui_kit::App;

/// OHOS NAPI 运行时的模块初始化。根视图同样等到 `OnSurfaceCreated` 交出原生窗口才打开。
pub extern "C" fn ohos_napi_init(
    env: gpui_mobile::ohos::napi::NapiEnv,
    exports: gpui_mobile::ohos::napi::NapiValue,
) -> gpui_mobile::ohos::napi::NapiValue {
    // 与 Android 那条臂同一步，先装 hilog logger 与 panic hook。少了它们这一格是黑的：
    // `src/ohos` 里所有 `log::*` 都进不了 hilog（native 侧一行不出，而同一进程里 ArkTS
    // 的 `hilog.info` 一直正常 —— 这个组合会把排查方向整个带偏），而 `-Z build-std` 下
    // panic 默认只写 stderr，faultlog 里剩的就只有裸 PC。
    gpui_mobile::ohos::init_logger();
    std::panic::set_hook(Box::new(|info| {
        log::error!("GPUI PANIC: {info}");
    }));
    // SAFETY: `env` / `exports` 直接来自 NAPI 运行时。
    unsafe {
        // `with_assets` 决定 `svg().path("icons/xxx.svg")` 能否解析出字节；不调它
        // 图标会是一片空白，而且是静默的空白。走 `install_with_assets` 而不是在
        // 下面的闭包里想办法：`Application::with_assets` 消费的是 builder 本身，
        // 而闭包只拿到 `&mut App`（与 Android 臂的 `start_with_assets` 同一条理由）。
        // `AllAssets` 是编译期嵌进 `.so` 的那一份（rust-embed），所以鸿蒙这边不需要
        // 再去接 ArkTS 的 rawfile / resourceManager。
        gpui_mobile::ohos::napi::install_with_assets(
            env,
            exports,
            "folio_app",
            gpui_kit::assets::AllAssets,
            Box::new(|cx: &mut App| {
                // 与 iOS 同一条：在模块初始化回调里读配置 + 装日志，再把配置交给 `boot`。
                let (resolved, _) = folio_core::infra::logging::init_from_config(None);
                super::boot(cx, resolved);
            }),
        );
    }
    exports
}

// 模块名必须与 `XComponent` 的 `libraryname`（即 `libfolio_app.so`）一致。
gpui_mobile::gpui_ohos_napi_module!("folio_app", crate::ui::platform::ohos::ohos_napi_init);
