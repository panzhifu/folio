# Android 移植：现状、依赖路线与构建配方

> 更新 2026-10-09。首次真机验证 2026-10-08（Meizu 20）：**正文渲染、平台滚动、底部导航栏四格切换成立**。
> 这份文档原先记的是 M0 阶段的可行性判断，那些判断已经被实测推翻或落实，逐条更正如下。
> 缺口清单在 `docs/known-limitations.md` 的「移动端」一节。

## 1. 依赖路线

| 层 | 来源 | 版本 |
|---|---|---|
| 平台层（Android 后端） | `longbridge/gpui-mobile`，本机 clone 在 `~/Code/gpui-mobile`，分支 `line/gpui-pre` 跟踪 `longbridge/main` | crates.io 包名 **`gpui-pre-mobile`**，lib 名仍是 `gpui_mobile` |
| GPUI 本体 | crates.io | **`gpui-pre =0.3.8`**，与桌面端 gpui-kit 钉的是同一版 |
| 排版与书库 | `crates/folio-core` | 路径依赖，零 gpui，原样复用 |

**原先判断的阻塞点已经解除。** 旧文档写的是：桌面 `gpui-kit` 钉 crates.io 的 `gpui-pre`，而
`gpui-mobile` 依赖 Zed git 的 `gpui`，同一上游的两个快照被 Cargo 当作两个包，无法同时链进一个二进制，
而「修掉两个快照之间的 `Platform` trait 漂移」是唯一未知数。实测：`gpui-mobile` 的活跃上游
（`longbridge/gpui-mobile`）本身就跑在 `gpui-pre` 这条线上，从它的 0.3.7 跟到桌面端的 0.3.8，
漂移只有**一处** —— `RequestFrameOptions` 新增 `signal_at: Option<Instant>` 与
`signal_source: FrameRequestSource`，在 `src/android/window.rs` 的 AChoreographer 回调处填
`None` + `NativeCallback` 即可。所以既不需要把桌面降到旧快照，也不需要接受「两套 UI 并存」。

fork 上这两处改动（`Cargo.toml` 四处 pin + `src/android/window.rs` 补两个字段）目前**只在本机分支上，
没有 push**。

**许可证**：`GPL-3.0-or-later OR AGPL-3.0-or-later OR Apache-2.0` 三选一，取 **Apache-2.0**，
不把 MIT 的 Folio 拖进 copyleft。发布时保留上游版权声明与 LICENSE。
**待办**：开发方案 §2.2b 要求这条依赖 `default-features = false`（它默认挂 30 多个平台插件 feature），
目前还没关。

**为什么 `crates/folio-mobile` 是独立的 cargo workspace**（根 `Cargo.toml` 里 `exclude`）：
`[patch.crates-io]`（`async-task` 那条，fork 需要）只在 workspace 根生效，桌面端不该继承；
分家之后移动端自己有一份 `Cargo.lock`，`libc` 被钉在 0.2.189（0.2.190 起 `ENOATTR` 从 android
模块里删掉，而 `gpui → http_client → async-tar → xattr` 还按旧名字引它）。

## 2. 构建与安装（本机实测可用）

```bash
export ANDROID_NDK_HOME=/opt/android-ndk
cd crates/folio-mobile

# 1) 交叉编译 cdylib → libfolio_mobile.so
CARGO_TARGET_DIR=~/.cache/folio-mobile-android \
  cargo ndk -t arm64-v8a -P 26 -o android/app/src/main/jniLibs build --release --lib

# 2) 打 APK（用系统 gradle，不用 wrapper）
cd android && gradle --no-daemon :app:assembleDebug

# 3) 装机并起 Activity
adb install -r app/build/outputs/apk/debug/app-debug.apk
adb shell am force-stop dev.folio.reader
adb shell am start -n dev.folio.reader/dev.gpui.mobile.GpuiActivity
```

三个不能省的细节：

- **`-P 26` 必须显式给**。`cargo ndk` 默认 min-API 是 21，而 NDK r30 的 sysroot 里
  `libnativewindow.so`（`ndk-sys` / `android-activity` 要链）只在 API 26+ 目录里。
  写进 `.cargo/config.toml` 的 `[env]` 也没用 —— 那是给 cargo 的子进程的，`cargo ndk` 自己是外层。
- **`RUST_FONTCONFIG_DLOPEN=on`**（已在 `crates/folio-mobile/.cargo/config.toml`）：
  `yeslogic-fontconfig-sys` 默认要求链接期找到 libfontconfig，Android sysroot 里没有；
  真机上字体由 cosmic-text 扫 `/system/fonts/`。
- **target 目录放 `~/.cache` 而不是 `/tmp`**：`/tmp` 是 tmpfs，11 GB 的构建缓存会吃掉内存。
  产物 `libgpui_mobile.so` 由 fork 自己那次 `cargo ndk` 生成，改 fork 才需要重编它。

宿主侧（不交叉编译）只保留 `host_main()`，为的是 `cargo check` / `cargo clippy` 在 x86 上过得去。

## 3. 设备事实（Meizu 20）

- Vulkan：Adreno 740，1.0.16 level 1 + compute —— 与宿主声明的 `android.hardware.vulkan.version=0x00401000` 一致。
- 屏幕 1080×2400，scale 2.8（⇒ 逻辑尺寸约 386×857）。
- 窗口打开时 fork 报 `safe_area_insets updated: top=116 bottom=70`，单位是**物理像素**（底部 ≈25 逻辑像素）。
- 系统注册 **13 个字族**，能用的中文正文家族是 **`Noto Sans CJK SC`**。

## 4. 三条最贵的知识

1. **`TextSystem::resolve_font` 是 panic 而不是回退**（`gpui-pre-0.3.8/src/text_system.rs:370`）：
   请求家族连同整串桌面回退名（`.ZedSans` / Helvetica / Segoe UI / Noto Sans / DejaVu Sans / Arial…）
   都不存在时直接 SIGABRT，而这在 Android 上是常态。唯一的公开判据是 `all_font_names()`
   （`font_id` 是私有的），所以按名字比 —— `reader_view::device_family()` 干这件事。
   任何新加的文字（包括导航栏标签）都要显式 `.font(...)` 指定这台设备真能解析的家族。
2. **文档必须落在应用专属外部目录** `/sdcard/Android/data/dev.folio.reader/files/`。
   `/sdcard/Download/` 里的文件属主是媒体提供者（另一个 uid），scoped storage 下本进程读到 EACCES，
   文件名放对了也不会出现画面。见 `reader_view::candidate_paths()`。
3. **Markdown 适配器按 `#` 和 `##` 一起分章**，所以以 `# 标题` 开头的样例会让 chapter 0 只剩那个标题 ——
   看起来完全像「正文没渲染出来」，但不是。测试语料要把正文段落放在第一个标题之前。

## 5. 触摸与滚动的实际路径

- 平台层只上报 `PlatformInput::Touch`（`gpui-mobile/src/android/window.rs:1490`）。一次轻点由 gpui 的
  手势识别器合成 `RecognizedTouchGesture::Tap { down, up }`，再派发 MouseDown/MouseUp
  （`gpui-pre-0.3.8/src/window.rs:5719`），所以 `div().on_click(...)` 在真机上成立。
- **惯量不自己做**。平台层的 `momentum::VelocityTracker` + `MomentumScroller` 已经把触摸翻译成
  `ScrollWheel` 事件，视图只累加 dy；在这里再套一个 `MomentumScroller` 会把同一份惯量算两遍，滚起来翻倍。
- `adb shell input tap` / `input swipe` 注入的是同一批事件，可以用来验证。

两条验证卫生（都在这台设备上真踩过）：

- **会出现幽灵触摸**：一次冷启动后没人碰屏幕，截图里却是第三格被选中。判断输入行为前先
  `am force-stop` + `am start` 取基线。
- **`screencap` 的 md5 不能当比较依据**：状态栏时钟每秒变，整帧哈希必变。要比就裁掉状态栏与栏区，
  数内容区的差异像素。

## 6. 一处遗留的误导

`src/lib.rs` 里还有 `#[cfg(target_env = "ohos")]` 的 HarmonyOS 入口，引用 `gpui_mobile::ohos::napi`
与 `gpui_ohos_napi_module!` —— 而 `longbridge/gpui-mobile` 这条线**没有 `ohos` 模块**（鸿蒙那份改动留在
旧线的本地分支 `feat/harmonyos-support` 上）。那段代码在当前依赖下永远不会被编译，但模块文档注释还把
HarmonyOS 列为入口之一。要么删掉，要么等鸿蒙那条线重接回来时一起处理，不要当成可用能力读。
