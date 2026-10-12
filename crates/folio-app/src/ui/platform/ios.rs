//! iOS 入口。
//!
//! `Application` 在 gpui-mobile 内部构造（`ios::ffi::run_app`），没有注册 `AssetSource`
//! 的口子，所以 iOS 上的图标目前是空白（见 `plans/facts.md`）。

use gpui_kit::App;

/// 把根视图注册给 GPUI 的 iOS 平台。由 `main.m` 在 `run_app()` **之前**调用，
/// 符号在 staticlib 里被 Xcode 强链接。
#[unsafe(no_mangle)]
pub extern "C" fn gpui_ios_register_app() {
    gpui_mobile::ios::ffi::set_app_callback(Box::new(|cx: &mut App| {
        // iOS 没有单独的原生入口能提前装日志，就在这次注册回调里读配置 + 装日志，
        // 再把配置交给 `boot`（§5.6 配置→日志、§2.3 只读一次）。
        let (resolved, _) = folio_core::logging::init_from_config(None);
        super::boot(cx, resolved);
    }));
}

/// `main.rs` 二进制用的便捷入口。
pub fn ios_main() {
    gpui_ios_register_app();
    gpui_mobile::ios::ffi::run_app();
}
