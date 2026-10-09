//! 二进制入口。
//!
//! - **桌面**：走 gpui-kit 的 `application()`，开 [`AppShell`](folio_app::ui::shell::AppShell)
//!   窗口（[`folio_app::ui::platform::desktop::run_desktop`]）。
//! - **iOS**：委托给 [`folio_app::ui::platform::ios::ios_main`]；`main.m` 在 `run_app()` 之前先调
//!   `platform::ios::gpui_ios_register_app`。
//! - **Android**：真正的入口是 cdylib 里的 `platform::android::android_main`，由 `android-activity`
//!   从 `libfolio_app.so` 调起；这个 bin 用不到，但必须能编过（`cargo build` 不带
//!   `--lib` 时会建它）。
//! - **HarmonyOS**：应用是 NAPI 模块（cdylib），没有二进制入口。
//!
//! 桌面那个 `main` 不能用 `#[cfg(not(mobile))]` 之外的写法，是因为 bin target 在移动
//! target 上也会被构建，缺一个 `fn main` 会直接报错。

#[cfg(not(mobile))]
fn main() {
    folio_app::ui::platform::desktop::run_desktop();
}

#[cfg(target_os = "ios")]
fn main() {
    folio_app::ui::platform::ios::ios_main();
}

// Android / HarmonyOS：入口在 cdylib 里，这个 bin 只为「`cargo build` 不带 `--lib`
// 也要有个 `fn main`」而存在，什么都不做。
#[cfg(any(target_env = "ohos", target_os = "android"))]
fn main() {}
