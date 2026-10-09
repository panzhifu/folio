# Folio

用 Rust + [gpui-kit](https://gpui-kit.com) 写的原生 GPU 桌面阅读器。不嵌浏览器，不用 WebView。

**状态**：M0/M1/M2 完成，M3 完成（局部样式 + 图片）。目前可用：EPUB 导入 → 侧栏书库 → 多标签阅读 → 自研引擎排版（首行缩进、居中、多 run 局部样式、行内/块级图片按 width/max-width 缩放）→ 字号/行距/字体/主题设置（在独立的设置窗口里，一页一项）→ 进度记忆（重启回到原位置）。Markdown/TXT 仍走占位渲染。
Android 侧（Meizu 20 实测）：正文与插图上屏、章节可前后翻、偏好与主题从同一份 `config.json` 读、
进度落库、界面文案跟随语言。移动 UI 还没有的是图标、书库导入与安全区适配，见 `docs/known-limitations.md`。

## 为什么自研排版

gpui-kit 的富文本组件明确不做 CSS（其 Not Goals 原文：*As a HTML viewer, we not support CSS*），而中文出版书需要的首行缩进、居中标题、扉页、局部字重、多栏、绕图、ruby 注音、竖排，全都得自己实现。所以正文由 Folio 自己的 DOM → CSS 子集 → 排版引擎产出，gpui-kit 承担应用壳（侧栏、标签、工具条、主题、对话框）与文本原语（shaping、断行、逐字形绘制）。

三条实测结论支撑这个决定，细节见 [`docs/spikes.md`](docs/spikes.md)：

- **排版可以离开窗口在后台线程完成** —— `Arc<WindowTextSystem>` 可 move 进 `background_spawn`，缩放窗口时布局不卡主线程。
- **gpui 自带 shaping**（cosmic-text + swash + harfrust），kerning / 连字 / bidi 都是现成的；`ShapedLine::paint` 还内建了文本对齐。
- **竖排不等于旋转** —— 横排 shaping 结果里 `glyph.position.x` 是推进量、`.y` 是基线偏移，两者对调即得 CJK 竖排，不需要任何 transform。

## 架构

```
crates/folio-core/   零 gpui 依赖：DOM、CSS、排版算法、锚点、SQLite、格式适配

crates/folio-app/src/ui/
  shared/            两端共用的地基：配置实体、文案、主题、字体度量、
                     「一章 → Layout」的排版状态、图片索引、公共小件
  desktop/           桌面壳：侧栏、标签、工具条、状态栏、引擎结果的虚拟列表，
                     以及 settings/（一页一个文件 + 开-or-聚焦的设置窗口）
  mobile/            移动壳：三平台入口、顶部章导航 + 底部导航栏、自绘字形元素
```

`folio-core` 不依赖 gpui 这条边界是硬约束：所以它能被 `cargo test` 全量覆盖，也是 Android 宿主里复用的前提。

`ui/shared` 的分层判据只有一条 —— **这段代码在两个平台上必须不能长得不一样**。断行口径、层叠、
版心宽度、主题解析都在这里共用一份实现；两端各自抄一份的旧写法（移动端曾有独立的
`measure.rs` 与 `document.rs`）已经合并，代价是那些代码从此只在 `cfg(mobile)` 下编译、
宿主测试碰不到，而合并之后 `tests/shared_layout.rs` 就同时是移动端的排版测试。

桌面与移动在同一个 `folio-app` 里，由 `cfg(mobile)` 互斥编译 ——
两端用的是同一个 `gpui-pre` 快照（`gpui_kit` 顶层 `pub use ::gpui::*` 把它整个转了出来），
所以能进同一张依赖图，视图代码也统一 `use gpui_kit::…`。

阅读位置与标注锚点都以**章内字符偏移**为坐标系，不是页码 —— 页码在字号或窗口宽度变化后就会失效。

## 构建

```bash
cargo run              # 起窗口（目前只有 spike 视图）
cargo test -p folio-core
cargo run --example spike   # M0 排版验证：stdout 出结论，窗口出自绘文本
```

Linux 需要 fontconfig、wayland、xkbcommon 的开发包与可用的 Vulkan 驱动。Wayland 与 X11 都已验证可跑。

Android 交叉编译（构建配方与设备事实见 [`docs/android.md`](docs/android.md)）：

```bash
export ANDROID_NDK_HOME=/opt/android-ndk
cd crates/folio-app
cargo ndk -t arm64-v8a -P 26 -o android/app/src/main/jniLibs build --release --lib
```

只想过一遍移动端的类型检查、不出 `.so`，用裸 `cargo check` 并把 NDK 的 C 工具链指全 ——
`cargo ndk` 会自动注入这几个变量，裸 `cargo` 不会，缺 `AR` 时 `libsqlite3-sys` 会在打包静态库那步失败：

```bash
TC=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin
CC_aarch64_linux_android=$TC/aarch64-linux-android26-clang \
CXX_aarch64_linux_android=$TC/aarch64-linux-android26-clang++ \
AR_aarch64_linux_android=$TC/llvm-ar \
CARGO_TARGET_DIR=~/.cache/folio-mobile-android \
  cargo check -p folio-app --target aarch64-linux-android --lib
```

## 测试样本

见 [`tests/fixtures/README.md`](tests/fixtures/README.md)：自动化测试用**程序合成**的 EPUB（可提交、可进 CI），真书只放 `tests/fixtures/local/` 且不入库。

手上的书到底还差多少样式，用这本书自己量（无界面，纯引擎）：

```sh
cargo run -p folio-core --example css-gap          # 仓库 fixtures 里的书
cargo run -p folio-core --example css-gap -- x.epub # 或任意一本
```

按**元素**排名的落差表 + 一份「把这些声明全丢掉 vs 现在」的几何对比都在 stdout。口径与限制见
[`docs/known-limitations.md`](docs/known-limitations.md)「这一张表有多准」一节。

## 文档

- [`docs/development-plan.md`](docs/development-plan.md) —— 完整开发方案：渲染路线取舍、引擎架构、CSS 子集范围、里程碑与风险
- [`docs/spikes.md`](docs/spikes.md) —— M0 实测数据与被推翻的判断
- [`docs/android.md`](docs/android.md) —— Android 移植现状：依赖路线（gpui-pre 0.3.8 + gpui-mobile）、构建配方、设备事实与触摸路径

## License

MIT
