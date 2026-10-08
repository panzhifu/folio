//! 二进制入口。
//!
//! - **iOS**：`fn main()` 委托给 [`folio_mobile::ios_main`]。
//! - **Android**：真正的入口是 `lib.rs` 里的 `android_main`，由 `android-activity`
//!   从 cdylib 调用；这个二进制用不到，但必须能编过（模式抄 `gpui-mobile/example/src/main.rs`）。
//! - **HarmonyOS**：应用是 NAPI 模块（cdylib），没有二进制入口。
//! - **宿主**：只打印一行提示，让 `cargo check` 不带 target triple 也过得去。

#[cfg(target_os = "ios")]
fn main() {
    folio_mobile::ios_main();
}

#[cfg(target_os = "android")]
fn main() {
    eprintln!(
        "This binary is not used on Android. The app enters via android_main() in lib.rs."
    );
}

#[cfg(target_env = "ohos")]
fn main() {
    eprintln!(
        "This binary is not used on HarmonyOS. The app enters via the NAPI module \
         registered in lib.rs (gpui_ohos_napi_module!)."
    );
}

#[cfg(not(any(target_os = "ios", target_os = "android", target_env = "ohos")))]
fn main() {
    folio_mobile::host_main();
}
