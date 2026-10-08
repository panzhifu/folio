# Android 适配可行性（M0 附加 spike）

> 日期：2026-10-08 · 结论：**路线可行，但有一道必须先过的依赖统一关，且本机 Android 工具链完全没装。**

## 1. 移动端走哪条路

`panzhifu/gpui-mobile`（fork 自 `itsbalamurali/gpui-mobile`，默认分支 `main`，最后推送 2026-10-02），
描述：*Mobile platform support for GPUI — iOS (wgpu/Metal) and Android (wgpu/Vulkan)*。

Android 侧结构已成型，不是空壳：

```
src/android/{platform,window,dispatcher,display,keyboard,jni,platform_view}.rs
example/android/            ← Gradle 工程 + native-activity 宿主
crate-type = ["staticlib", "cdylib", "rlib"]
```

**许可证**：`GPL-3.0-or-later OR AGPL-3.0-or-later OR Apache-2.0` 三选一。我们取 **Apache-2.0**，
所以不会把 Folio（MIT）拖进 copyleft。注意**不要**选 AGPL 那份 —— 网络服务条款对我们没好处。
发布时要保留上游版权声明与 LICENSE 文件。

它的 `default` features 挂了 30 多个平台插件（webview / camera / sensors / location…），
Folio 一个都不要，必须 `default-features = false` 按需开。

## 2. 真正的阻塞点：两个 gpui 快照

| 层 | 依赖 | 来源 |
|---|---|---|
| 桌面 `folio-app` | `gpui-kit 0.7.1` → **`gpui-pre 0.3.8`** | crates.io，描述写明是 *snapshot of **zed@279fe07*** |
| 移动端 `gpui-mobile` | **`gpui`**（git） | `github.com/zed-industries/zed` rev **`5688167d`** |

**同一个上游项目的两个不同 commit，但 Cargo 看是两个不同的包**（一个叫 `gpui-pre`，一个叫 `gpui`），
因此**没法同时链进一个二进制**。直接结论：

- ❌ `folio-app`（含 Sidebar / TabBar / Dialog 等 gpui-kit 组件层）现在**编译不到 Android**。
- ✅ `folio-core` 零 gpui 依赖（已验证：29 个测试全绿，无 gpui）→ **Android 上原样复用，一行不改**。

好消息是这**不是分叉的 fork**，只是同一上游的两个快照。所以统一路径是明确可做的：

```
把 gpui-mobile 的 gpui 依赖从 zed@5688167 改指 gpui-pre 0.3.8，
修掉 279fe07 → 5688167 之间的 Platform trait 漂移。
```

漂移量是这件事的唯一未知数，也是 Android spike 的**第一个待跑实验**（见 §4 步骤 2）。

顺带说明为什么这比"移动端自己写一套 UI"划算：我们的排版引擎只用到 gpui 的文本与绘制原语
（`shape_line` / `LineLayout.runs[].glyphs[]` / `paint_glyph` / `canvas`），
这些在两个快照里都存在（gpui-pre 的那批 API 正是从 Zed 上游 snapshot 来的）。
所以**引擎层可以跟着快照走，只有 gpui-kit 的组件层在移动端用不了** —— 而移动端的导航本来就该是原生的。

## 3. 移动端的目标形态

```
Android Activity (Kotlin/Java 壳，Gradle 工程)
   └─ gpui-mobile 提供 surface + 触摸输入 + PlatformTextSystem
        └─ folio-core（复用：书库 / DOM / CSS / 排版算法 / 锚点 / SQLite）
             └─ folio-engine 的 Measure + Paint（对 gpui 抽象，两个快照都能实现）
   导航、书架、设置 → 平台原生 UI，不搬桌面组件
```

这跟 `docs/development-plan.md` §2.2 里 gpui-kit 官方的移动端定位一致：移动端复用**部分组件**，
不是把桌面壳搬过去。

## 4. 要跑通还缺什么

### 步骤 0：工具链（本机当前**全缺**）

```
rustup target add aarch64-linux-android   ← 未安装
cargo-ndk                                 ← 缺
Android SDK + sdkmanager                  ← 缺（ANDROID_HOME 未设置）
Android NDK                               ← 缺
gradle                                    ← 缺（java 已有）
```

体积约 3–5 GB，且要接受 Android SDK 许可。**这一步需要你确认要不要我装。**

### 步骤 1：先跑上游自己的 Android 示例

`cd /tmp/gpui-mobile/example && ./build.sh android`（fork 里脚本行为需实测）。
目的不是产出 Folio，而是确认「gpui-mobile 的 Android 路径在这台机器上真的能出 apk 并跑到手机上」。
**这一步没通过之前，任何 Folio 侧的移动端改造都是空谈。**

### 步骤 2：trait 漂移实验（决定后面怎么做）

在 fork 里把 `gpui` 依赖换成 `gpui-pre = "0.3.8"`，`cargo check --target aarch64-linux-android`，
统计报错数量与性质：

- 漂移小（几个方法签名）→ 直接做成 `panzhifu/gpui-mobile` 的一个分支，Folio 长期依赖它。
- 漂移大 → 退一步：**Folio 桌面也改用同一个 gpui 快照**（放弃 gpui-kit 组件层，代价极大，
  等于侧栏/标签/对话框/主题全部自绘），或者移动端只做「正文渲染 + 原生外壳」的最小闭环，
  不接受统一依赖，两套 UI 长期并存。

### 步骤 3：Folio 侧改造（依赖统一之后）

- workspace 加 `crates/folio-mobile`（Android cdylib，`crate-type = ["cdylib"]`）
- `folio-engine` 的 `Measure` / `Paint` 拆成对 gpui 抽象的两份实现，按 `cfg(target_os)` 选
- SQLite：`rusqlite` bundled 在 Android 上要 NDK 工具链编译 C，需验证交叉编译可用
- 字体：桌面是 `add_fonts`，移动端 `PlatformTextSystem` 的字体加载路径要单独确认

## 5. 现在不该做的事

- 不要为了移动端把 `folio-core` 里加任何 `cfg` 分支 —— 它零 gpui 依赖，本来就能跨平台，加判断只会污染它。
- 不要在步骤 1 通过前开始写 `folio-mobile`。
- 不要把 gpui-kit 的 `component` 层当成移动端目标 —— 官方明确只验证过一个 AI Chat 区域。
