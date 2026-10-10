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
        super::boot(cx);
    }));
}

/// `main.rs` 二进制用的便捷入口。
pub fn ios_main() {
    gpui_ios_register_app();
    gpui_mobile::ios::ffi::run_app();
}
