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
    // SAFETY: `env` / `exports` 直接来自 NAPI 运行时。
    unsafe {
        gpui_mobile::ohos::napi::install(
            env,
            exports,
            "folio_app",
            Box::new(|cx: &mut App| {
                // 与 iOS 同一条：在模块初始化回调里读配置 + 装日志，再把配置交给 `boot`。
                let (resolved, _) = folio_core::logging::init_from_config(None);
                super::boot(cx, resolved);
            }),
        );
    }
    exports
}

// 模块名必须与 `XComponent` 的 `libraryname`（即 `libfolio_app.so`）一致。
gpui_mobile::gpui_ohos_napi_module!("folio_app", crate::ui::platform::ohos::ohos_napi_init);
