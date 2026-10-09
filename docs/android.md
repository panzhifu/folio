# Android 移植：现状、依赖路线与构建配方

> 更新 2026-10-09。首次真机验证 2026-10-08（Meizu 20）：**正文渲染、平台滚动、底部导航栏四格切换成立**。
> 这份文档原先记的是 M0 阶段的可行性判断，那些判断已经被实测推翻或落实，逐条更正如下。
> 缺口清单在 `docs/known-limitations.md` 的「移动端」一节。

## 1. 依赖路线

| 层 | 来源 | 版本 |
|---|---|---|
| 平台层（Android 后端） | `panzhifu/gpui-mobile` 的 `line/gpui-pre` 线（跟踪 `longbridge/gpui-mobile`），**以钉 rev 的 git 依赖接入** | crates.io 包名 **`gpui-pre-mobile`**，lib 名仍是 `gpui_mobile` |
| GPUI 本体 | crates.io | **`gpui-pre =0.3.8`**，与桌面端 gpui-kit 钉的是同一版 |
| 排版与书库 | `crates/folio-core` | 路径依赖，零 gpui，原样复用 |

**依赖形式：git + 钉 rev，不再是仓库外的 path 依赖。** 以前是
`gpui-pre-mobile = { path = "../../../gpui-mobile" }`，好处是能直接改 fork 不用推，
代价是**仓库不自洽**：全新克隆（也就是 CI）里连 `cargo metadata` 都跑不起来，实测退出 101
`failed to load manifest for dependency gpui-pre-mobile`；而 GitHub 的 `actions/checkout`
拒绝把依赖检出到 workspace 外面（`Repository path ... is not under ...`），两条路都堵死。
现在钉的是 `rev = "96afdf2b…"`，`Cargo.lock` 记下具体 commit —— 比跟分支头走更可复现，
而且 fork 那份代码从此不会在没人改 Cargo.toml 的情况下换掉。

要改 fork 本身而不先推上去：`~/.cargo/config.toml` 里加
`paths = ["/home/noke/Code/gpui-mobile"]`，cargo 会用那个目录顶替同名的 git 依赖，
不用动这里的 rev；这个覆盖只对本机生效，CI 永远按锁里那份取。

**原先判断的阻塞点已经解除。** 旧文档写的是：桌面 `gpui-kit` 钉 crates.io 的 `gpui-pre`，而
`gpui-mobile` 依赖 Zed git 的 `gpui`，同一上游的两个快照被 Cargo 当作两个包，无法同时链进一个二进制，
而「修掉两个快照之间的 `Platform` trait 漂移」是唯一未知数。实测：`gpui-mobile` 的活跃上游
（`longbridge/gpui-mobile`）本身就跑在 `gpui-pre` 这条线上，从它的 0.3.7 跟到桌面端的 0.3.8，
漂移只有**一处** —— `RequestFrameOptions` 新增 `signal_at: Option<Instant>` 与
`signal_source: FrameRequestSource`，在 `src/android/window.rs` 的 AChoreographer 回调处填
`None` + `NativeCallback` 即可。所以既不需要把桌面降到旧快照，也不需要接受「两套 UI 并存」。

fork 上这两处改动（`Cargo.toml` 四处 pin + `src/android/window.rs` 补两个字段）已随
`line/gpui-pre` 推到 `panzhifu/gpui-mobile`；本仓库钉的 `96afdf2b` 就是含这两处的那个 commit。

**输入法与返回手势的宿主契约（2026-10-09，`96afdf2b`）。** 两处原先由应用侧绕过的平台问题
现在收在 fork 的 `GpuiInputActivity` 里，做法照抄成熟实现（SDL 的 `SDLSurface.onApplyWindowInsets`、
Godot 的 `Godot.kt`）：安全区只取系统栏 / 手势 / 挖孔那几类 inset，**不含** `WindowInsets.Type.ime()`；
键盘的可见性与高度单独上报。于是：

- `safe_area_insets_logical()` 在弹键盘时不再变（原先它从内容区推，而内容区把输入法算进去，
  实测 `bottom` 会从 70 跳到 859 物理像素），底栏不再被顶到键盘上面。
- 键盘另走 `ime_insets_logical()` / `keyboard_visible()`，需要把被聚焦的输入框挪出键盘时读它，
  而不是改整个布局。宿主 Activity 用 `adjustNothing`（键盘覆盖窗口，不缩窗口）。
- `ContentRectChanged` / `InsetsChanged` 不再被当成窗口 resize，只是刷新一个兜底值。
- 返回手势：Android 13 起系统走 `OnBackInvokedDispatcher`，不再发 `AKEYCODE_BACK`。fork 注册
  `OnBackInvokedCallback` 把它转成与输入队列同一条 BACK（→ `escape`）路径，manifest 需
  `android:enableOnBackInvokedCallback="true"`。**返回由应用负责**：`ShellView` 把 escape 绑成
  `Back`，到根页面 `cx.quit()`。

**许可证**：`GPL-3.0-or-later OR AGPL-3.0-or-later OR Apache-2.0` 三选一，取 **Apache-2.0**，
不把 MIT 的 Folio 拖进 copyleft。发布时保留上游版权声明与 LICENSE。
**待办**：开发方案 §2.2b 要求这条依赖 `default-features = false`（它默认挂 30 多个平台插件 feature），
目前还没关。

**为什么移动端以前是独立的 cargo workspace，现在又不独立了**：早先根 `Cargo.toml` 用
`exclude = ["crates/folio-mobile"]`，理由是「两端链的不是同一个 `gpui` 包」。fork 换到
`gpui-pre =0.3.8` 之后这条不成立了，于是**合并进 `crates/folio-app`，移动代码在 `src/ui/mobile/`**
（界面层现在分 `src/ui/{shared,desktop,mobile}/`，两端必须一致的东西放在 `shared`），
整体由 `cfg(mobile)`（`crates/folio-app/build.rs` 设）与桌面模块互斥编译。代价是两边共用一张依赖图、
一份 `Cargo.lock`：`[patch.crates-io]`（`async-task` 那条，fork 需要）和 `[profile.release]`
因此都挪到了仓库根 `Cargo.toml`。`libc` 那次钉 0.2.189 的理由（`xattr` 按旧名引 `ENOATTR`）
在合并时已不存在——两个 lock 里都没有 `xattr`——所以取桌面端的 0.2.190，实测 Android 链接通过。

## 2. 构建与安装（本机实测可用）

```bash
export ANDROID_NDK_HOME=/opt/android-ndk
cd crates/folio-app

# 1) 交叉编译 cdylib → libfolio_app.so（[lib] name = "folio_app"）
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
- **`RUST_FONTCONFIG_DLOPEN=on`**（已在 `crates/folio-app/.cargo/config.toml`）：
  `yeslogic-fontconfig-sys` 默认要求链接期找到 libfontconfig，Android sysroot 里没有；
  真机上字体由 cosmic-text 扫 `/system/fonts/`。
- **target 目录放 `~/.cache` 而不是 `/tmp`**：`/tmp` 是 tmpfs，11 GB 的构建缓存会吃掉内存。
  产物 `libgpui_mobile.so` 由 fork 自己那次 `cargo ndk` 生成，改 fork 才需要重编它。
- **只想过类型检查、不出 `.so`，就别用 `cargo ndk`**，直接 `cargo check --target aarch64-linux-android`，
  但要把 NDK 的 C 工具链变量给全 —— `CC` / `CXX` / `AR` 是 `cargo ndk` 平时替我们注入的，
  裸 `cargo` 不会。缺 `AR` 时 `libsqlite3-sys` 会在打包 `libsqlite3.a` 那步报
  `failed to find tool "aarch64-linux-android-ar"`，看起来像代码问题，其实是环境：

  ```bash
  TC=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin
  CC_aarch64_linux_android=$TC/aarch64-linux-android26-clang \
  CXX_aarch64_linux_android=$TC/aarch64-linux-android26-clang++ \
  AR_aarch64_linux_android=$TC/llvm-ar \
  CARGO_TARGET_DIR=~/.cache/folio-mobile-android \
    cargo check -p folio-app --target aarch64-linux-android --lib
  ```

  `aarch64-linux-android-clang` 这种**不带 API 号**的名字在 NDK r30 里不存在，所以 `CC` 要指到
  `aarch64-linux-android26-clang`（26 与上面 `-P 26` 同档）。

移动模块整体由 `cfg(mobile)` 门控，所以宿主（不交叉编译）上 `cargo check` / `cargo clippy`
编的是桌面视图那一侧，`gpui-pre-mobile` 根本不会进依赖图。**但 `src/ui/shared/` 两端都编** ——
断行、层叠、版心宽度、主题解析都在那里，所以宿主的 `cargo test`（`ui/shared/document.rs` 的 tests）
确实覆盖了移动端排版的一部分，而不是「移动端一条测试都没有」。
覆盖不到的仍然是移动 UI 自己：触摸滚动、换章按钮、真机上的浅深解析。

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

## 6. 桌面组件层能不能上 Android：已实测，能（2026-10-09）

**`gpui-kit` 与 fork 的 AndroidPlatform 可以链进同一个二进制并在真机跑起来。**
旧文档里「`folio-app` 的组件层编译不到 Android」这句话已经不成立 —— 它是两个 gpui 快照冲突时期的
推论，而 fork 换到 `gpui-pre =0.3.8` 之后，桌面与移动端用的是**同一个** `gpui` crate。

结构上的理由：`gpui-pre` 核心里**没有平台代码**（`x11` / `wayland` 只是 feature 标记，真正的平台实现
在 `gpui-pre-platform` 那个 crate 里），`Platform` 由外部注入 —— 桌面注 `gpui-pre-platform`，
移动端注 `gpui_mobile::android`。所以组件层是平台无关的源码，实测也证实了这一点：
`gpui-base` / `gpui-component` / `gpui-kit` 交叉 check 时，依赖图里根本没有
platform / wgpu / x11 / wayland / cosmic-text。

四道关的实测数（探针 = 给当时的 `crates/folio-mobile` 加一条 `gpui-kit` git 依赖；两端现在已合并进 `crates/folio-app`）：

| 关 | 结果 |
|---|---|
| 依赖解析 | 重解析 323 个包；`libc` 的 0.2.189 钉住未被动 |
| 交叉 check | 392 个 crate 通过，`aarch64-linux-android` 零错误 |
| release 链接 | 通过（`lto = "fat"`，1m59s） |
| 真机 | `gpui_kit::init(cx)` 在注入的平台下不崩；`ThemeRegistry` 报 2 套主题；窗口正常打开 |

主题值确实到了绘制路径：栏底从写死的 `(250,249,245)` 变成 `(255,255,255)`，正是
`crates/base/src/theme_tokens.rs:57` 里 Default Light 的 `background`。一个真 `Button` 画了出来
（含中文），点击真的改变了应用状态并重绘；一次 tap 对应一条回调日志。

**代价**：`libfolio_app.so` 11,601,808 → 15,398,560 字节（+3.80 MB，+32.7%）。APK 的增量与它逐项相等
（+3,852,864 字节），但别拿当时的 APK 绝对值当基线 —— 那次量到的 36.7 MB 里有约 11 MB 是干净重编复现
不出来的残留，同一份源码重新打过是 25.8 MB。

**还没验的四件事**：`application().with_assets()` 那条图标通路在 Android 上怎么接（移动端走的是
`with_platform`）；浮层类组件（`Dialog` / `Sheet` / `Popover`）；按下态视觉（实测常态与按下态像素零差异，
不确定是本来就没有变化还是没抓到）；`Mode::System`（fork 有 `appearance()`，但宿主 Activity 没声明
night 资源）。

**由此定下的主题方案已经实施（2026-10-09）**：两端都走 `ThemeRegistry::load_themes_from_str` +
读 `cx.theme()`，移动端不写任何 JSON 解析，界面上也没有一行裸色值。落地的三件事：

- `crates/folio-app/src/ui/shared/theme.rs` 变成两端共用（它原先在 `cfg(not(mobile))` 那一侧）。
  主题嵌入仍由 `crates/folio-app/build.rs` 做，合并后两端都会跑它，`FOLIO_THEMES_DIR` 照样可覆盖。
- 移动入口的启动顺序是 `gpui_kit::init` → `register_builtin_themes` → `register_user_themes` →
  读 `config.json` → `ConfigHolder::install` → `theme::apply`，窗口开起来后再用**窗口自己的**外观
  涂一次。这一步不能省：`AndroidPlatform::window_appearance()` 是写死返回 `Dark` 的
  （`gpui-mobile/src/android/platform.rs:1095`），真正的系统夜间模式只在窗口级
  （`AndroidPlatformWindow::appearance()`，由 JNI 的 `query_night_mode_via_jni` 喂）。
  所以「跟随系统」必须读 `window.appearance()`，`theme::repaint` 就是为这个留的入口 ——
  它换肤但不刷所有窗口，可以在渲染那一帧里安全调用。
- **`paths::config_dir()` 在 Android 上落到 `/tmp/.config/folio` 这条已经解决**：`folio-core::paths`
  多了 `set_app_dirs(config, data)`，Android 入口在建 `Application` 之前用
  `app.internal_data_path()/files/folio/{config,data}` 报上来。优先级是
  环境变量 → 宿主报的目录 → 操作系统标准位置，只有第一次 `set_app_dirs` 生效。
  这条原先挡住的不只是 `config.json`，还有 `themes/` 目录与 `folio.db`。

一个留给产品的决定：Default Light 的 `background` 是纯白，而现在移动端的正文底色是为阅读写的暖纸色
`(250,249,245)`。阅读区到底跟 `background`、跟 `surface`/`muted`，还是单独走暖色/sepia，
不该由渲染层顺手定。**当前实现选了 `background` + `foreground`**（也就是纯白底），
所以真机上看着会比改动前刺眼 —— 这一条要么由主题层给阅读区单独一个语义 token，
要么由读者默认挑一套暖色主题，不该继续在视图里写死颜色。

探针代码已回退，改动留成补丁：`~/.cache/folio-probe-backup/probe.patch`（`git apply` 可复原）。

## 7. 一处遗留的误导

`src/ui/mobile/mod.rs` 里还有 `#[cfg(target_env = "ohos")]` 的 HarmonyOS 入口，引用
`gpui_mobile::ohos::napi`
与 `gpui_ohos_napi_module!` —— 而 `longbridge/gpui-mobile` 这条线**没有 `ohos` 模块**（鸿蒙那份改动留在
旧线的本地分支 `feat/harmonyos-support` 上）。那段代码在当前依赖下永远不会被编译，但模块文档注释还把
HarmonyOS 列为入口之一。要么删掉，要么等鸿蒙那条线重接回来时一起处理，不要当成可用能力读。
