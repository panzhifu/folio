# Folio

用 Rust + [gpui-kit](https://gpui-kit.com) 写的原生 GPU 桌面阅读器。不嵌浏览器，不用 WebView。

**状态**：M0–M4 引擎主线完成（排版、局部样式、图片、连续/分页/双页），M5 架构重构与 M6 选中已完成，M7 批注的后端与划线写入口已落地、笔记那一屏还是空状态。目前可用：EPUB 导入 → 侧栏书库 → 多标签阅读 → 自研引擎排版（首行缩进、居中、多 run 局部样式、行内/块级图片按 width/max-width 缩放）→ 字号/行距/字体/主题设置 → 进度记忆（重启回到原位置）。格式侧另有 MOBI / AZW3（KF7 + KF8，含 hybrid）、AZW4（PalmDB 内嵌 PDF）、PDF、DOCX / ODT、PDB / PalmDoc（认 DRM 那一格、认正文其实是标记、读早期 OEB1 的都柏林核心块）、HTMLZ、CB7 / CBR / CBT（7z / RAR / tar 漫画包；CBR 走纯 Rust 的 `rars`，不绑 unrar 的 C++）、DjVu（纯 Rust 的 `djvu-rs`，不引进 DjVuLibre；bundled 与 indirect 两种容器形状都读，NAVM 书签上屏当目录、`(metadata …)` 那块进书目）、SVG 栅格化（resvg）、定版式阅读（CBZ / CB7 / CBR / CBT / PDF / AZW4 / DjVu 与 EPUB fixed-layout，逐页含 SVG 漫画；栅格化那三族的页也点得开放大、能分享与保存）。漫画那一族的书目来自包里那份 `ComicInfo.xml`（ComicRack / CDisplayEx / Calibre 写的那份便条：书名、著者、其余角色、系列与期号、出版社、简介、主题、语种、条码），包里那张 `folder.jpg` / `cover.jpg` 认成**封面**而不是凭空多出来的一页。Word / ODT 那两族读出**列表**（记数按 `word/numbering.xml` 与 `text:list-style`，层号按 `w:ilvl` / `<text:list>` 的嵌套）与**脚注尾注**（引用落成可点的上标，注体摆回引用它的那一章，接现有脚注弹层那条链）。**未接**：PDF / AZW4 / DjVu 的文字层（选中、批注、听书在这三族上是空的）、加密 RAR（没有密码入口）、`ComicInfo.xml` 里 `<Chapters>` 的页号（基数没有可信权威，猜了会整条目录偏一页）、Word 靠样式间接触发的编号与每节从头再数的编号、ODT 的 `text:continue-numbering`。见 [`plans/`](plans/README.md)。
Android 侧（Meizu 20 实测）：正文与插图上屏、章节可前后翻、偏好与主题从同一份 `config.json` 读、
进度落库、界面文案跟随语言。实测约束清单见 [`plans/facts.md`](plans/facts.md)。

## 为什么自研排版

gpui-kit 的富文本组件明确不做 CSS（其 Not Goals 原文：*As a HTML viewer, we not support CSS*），而中文出版书需要的首行缩进、居中标题、扉页、局部字重、多栏、绕图、ruby 注音、竖排，全都得自己实现。所以正文由 Folio 自己的 DOM → CSS 子集 → 排版引擎产出，gpui-kit 承担应用壳（侧栏、标签、工具条、主题、对话框）与文本原语（shaping、断行、逐字形绘制）。

三条实测结论支撑这个决定，细节见 [`plans/facts.md`](plans/facts.md)：

- **排版可以离开窗口在后台线程完成** —— `Arc<WindowTextSystem>` 可 move 进 `background_spawn`，缩放窗口时布局不卡主线程。
- **gpui 自带 shaping**（cosmic-text + swash + harfrust），kerning / 连字 / bidi 都是现成的；`ShapedLine::paint` 还内建了文本对齐。
- **竖排不等于旋转** —— 横排 shaping 结果里 `glyph.position.x` 是推进量、`.y` 是基线偏移，两者对调即得 CJK 竖排，不需要任何 transform。

## 架构

```
crates/folio-core/   零 gpui 依赖：统一文档接口（Document trait：可重排 / 定版式）、
                     DOM、CSS、排版算法、锚点与批注（重定位/分组/导出）、书架分组、
                     SQLite（书目/进度/批注/集合/阅读统计/定版式页表）、
                     格式 provider 注册表（EPUB / FB2 / HTML / Markdown / TXT / RTF / CBZ / CHM / MOBI /
                     PDF / DOCX / ODT / PDB / AZW4 / CB7 / CBR / CBT / DJVU / HTMLZ，共 19 个）、
                     SVG 栅格化（resvg，封面 / 内联 / 逐页漫画共用）、WebDAV、配置（含 `logging` 一节）、
                     日志（`tracing` subscriber 装在这）

crates/folio-app/src/ui/
  shared/            两端共用的地基：配置实体、文案、主题、字体度量、
                     「一章 → Layout → 切片」的排版状态、图片索引、书库
  widgets/           无状态小件（发丝线、次要文字、空状态）
  features/          按功能纵切：reader（可重排一套渲染 + 连续/分页/双页，定版式另走页表）、
                     shelf、notes、stats、settings（一页一个文件 + 开-or-聚焦的设置窗口）
  shell/             应用外壳：自适应导航（侧栏 / 图标栏 / 底栏）+ 每目的地一条返回栈
  host/              平台服务边界：安全区、输入法 inset、正文字体家族
  platform/          三平台入口与原生桥
```

`folio-core` 不依赖 gpui 这条边界是硬约束：所以它能被 `cargo test` 全量覆盖，也是 Android 宿主里复用的前提。

`ui/shared` 的分层判据只有一条 —— **这段代码在两个平台上必须不能长得不一样**。断行口径、层叠、
版心宽度、主题解析都在这里共用一份实现；两端各自抄一份的旧写法（移动端曾有独立的
`measure.rs` 与 `document.rs`）已经合并。

阅读界面以前是第三个「各抄一份」的地方：桌面把行盒接进 `TextView` + 虚拟列表，移动自己逐字形
画。现在两端共用逐字形那一份，`cfg(mobile)` **只剩 `ui/host` 与 `ui/platform`** —— 平台上留存的
差别是「导航栏占的是宽还是高」这种数据（`reader::Reserve` 两个轴都带着）与字体回退，不再是两套
代码。这也意味着宿主的 `cargo check` / `clippy` / `test` 第一次真的编到了移动端读的那条渲染路径。

两端用的是同一个 `gpui-pre` 快照（`gpui_kit` 顶层 `pub use ::gpui::*` 把它整个转了出来），
所以能进同一张依赖图，视图代码统一 `use gpui_kit::…`。

阅读位置与标注锚点都以**章内字符偏移**为坐标系，不是页码 —— 页码在字号或窗口宽度变化后就会失效。

## 构建

```bash
cargo run                      # 起桌面窗口（AppShell）
cargo test --workspace --lib   # 日常这一条
```

### 测试为什么内联在 `src/`，以及这两条命令差在哪

**测试本身跑完只花 0.1 秒**，慢的全是链接。原先 19 个 `tests/*.rs` 各是一个独立 crate：
每个都要重新类型检查一遍、单独链接一次，而且拿不到 `src/` 的 incremental 缓存；
更贵的是裸 `cargo test` 会把测试根本不需要的 target 一起编出来 —— `folio-app` 的
`crate-type` 含 `cdylib`/`staticlib`，于是每次都要归档 1.96 GB 的 `libfolio_app.a`、
610 MB 的 `.so`、775 MB 的 `folio`（量这些的时候还带着 M0 那个 `spike` example，每次多链一份
717 MB，已删；`cargo test` 给 bin 生成的那份 test 跑器同理 —— `src/main.rs` 里 0 个测试，
已于 2026-10-13 用 `[[bin]] test = false` 关掉。`cargo metadata` 里现在报 `test=false`）。
实测改一个叶子文件（`folio-core/src/format/epub.rs`）：

| 命令 | 重构前 | 重构后 |
|---|---|---|
| `cargo test --workspace --lib`（覆盖 100% 用例） | 4 秒，但只跑 9 条 | **3–4 秒，跑全部 272 条** |
| `cargo test --workspace` | **395 秒** | 395 秒（不变，见下） |

所以日常循环是 `cargo test --workspace --lib`。测试全部内联之后，这一条就是**全集**，
不再需要从 `--lib` 里漏掉什么。

裸 `cargo test --workspace` 保持原样不动，CI 跑的也是它：它多花的那六分钟买的是
「全新克隆里每个 target 都编得过」—— bin、cdylib、staticlib、example 都在这条上被编一遍，
那些东西 `--lib` 根本不碰，而移动端宿主要的正是 cdylib 与 staticlib。这是有意保留的慢，
不是没优化到。

三条写测试的规矩（照 `trove` 的分法）：

- 测试内联在**被测代码所在的那个文件**底部（`#[cfg(test)] mod tests`）；只有当一个模块的
  公开面散在几个兄弟文件里，才用 `src/<模块>/tests.rs`。真需要外部环境的用例标
  `#[ignore]` 并在注释里写清运行命令（例：`format/epub.rs` 里那本本机版权书）。
- **不用 `use super::*`，点名导入**。`folio-app` 这条不是风格问题：`gpui_kit` 根做了
  `pub use ::gpui::*`，里面带一个名叫 `test` 的属性宏，glob 进来会让 `#[test]`
  递归展开成它。`ui/shared/theme.rs` 从搬进来之前就是这么写的。
- **只引 `pub` 项**。这些测试以前住在 `tests/`，编译器天然只给它们公开面；搬进 `src/`
  之后这层保证没了，就靠这条约定守 —— 想测私有实现请先想它为什么不是公开的。

测试跟着被测代码走：`features/reader/view.rs` 的 `tests` 用真实滚轮事件驱动生产视图，
`ui/shell/mod.rs` 的 `tests::booted` 那类只有桌面跑得动的用例单独一层 `#[cfg(not(mobile))]`
（`TestSupportExt` 的注册路径在移动目标上不存在）。Android 那个
job 跑的是 `clippy --target aarch64-linux-android --lib`，不开 `cfg(test)`，
所以移动目标上编不到的那部分测试不会影响它。

Linux 需要 fontconfig、wayland、xkbcommon 的开发包与可用的 Vulkan 驱动。Wayland 与 X11 都已验证可跑。

Android 交叉编译（构建配方与设备事实见 [`plans/facts.md`](plans/facts.md)）：

```bash
export ANDROID_NDK_HOME=/opt/android-ndk
cd crates/folio-app
cargo ndk -t arm64-v8a -P 26 -o android/app/src/main/jniLibs build --release --lib
```

打包安装启动走系统里的 Gradle（本机 9.8.0），**不用 `./gradlew`** —— wrapper 会重新
下载发行版，而 wrapper 缓存里那份 9.4.1 已删；`--offline` 保证依赖只来自本地缓存，
冷缓存的新机器去掉它即可：

```bash
cd crates/folio-app/android
gradle assembleDebug --offline
adb install -r app/build/outputs/apk/debug/app-debug.apk
adb shell am start -n dev.folio.reader/dev.gpui.mobile.GpuiActivity
```

重打出比上一份大得离谱的 APK（实测 372M 对 36M）不是依赖坏了：AGP 的 zipflinger
复用旧 APK 就地改写，新内容比旧的小时留下空洞。删掉 outputs 里的旧 APK 再打就好。

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

## 提交：用 jj

提交与推送走 [Jujutsu](https://docs.jj-vcs.dev)（`jj`），仓库是 **colocated** 的：`.jj/` 与
`.git/` 并存，所以 `git clone`、GitHub Actions、Android 那条 `cargo ndk` 都不受影响。

命令配方与两处实测的坑都写在 [`.agents/skills/Tool/SKILL.md`](.agents/skills/Tool/SKILL.md)
（放仓库根的 `.agents/skills/` 是为了让各种 agent 都能按同一个路径读到；`rg` / `rga` / `ast-grep` 的搜索口径、
以及 mold / just / scrcpy 在本机怎么接的，也在同一份）。要点只有三条：**别在 shell 里用 `git commit` /
`git checkout` / `git stash`**（colocated 之后 git 的 HEAD 是分离的，那些改动 jj 看不见）；
推之前要显式把 `main` 挪到要推的 change 上（`@` 前进不带 bookmark 一起走）；jj 会把**未被 ignore**
的文件当成待提交改动收进工作副本，本机 scratch 先进 ignore 名单再落提交。

## 日志

日志是**后端的一部分**，不是界面的一条附属功能：`folio-core` 里那些最容易出错的地方
（书库迁移、EPUB 容器、WebDAV 往返、排版）都直接发 `tracing` 事件，而 subscriber 装在
[`folio_core::logging`](crates/folio-core/src/logging.rs)。用 `tracing` 而不是自己排行文本，
是因为级别过滤、时间戳、字段渲染、span 耗时这些恰好就是调试要看的东西；而它**不是新依赖** ——
`gpui-pre-ztracing` 带的就是同一份 `tracing-subscriber`（`env-filter` 与 `tracing-appender`
故意不开，见根 `Cargo.toml` 那段说明）。

三层输出，共用一个过滤器：**文件** `<数据根>/logs/folio.log`（开机轮转成 `.1`/`.2`，
本轮写满就停手）、**stderr**（桌面默认开）、**宿主**（Android 的 logcat，tag 还是 `folio-mobile`）。

级别写在 `config.json` 的 `logging` 一节里，**改完重启就生效，不用重新编译** ——
真机上出问题最怕的就是「什么都不显示」，而重新编一遍 Android 是十几分钟加一次重装：

```json
{
  "logging": {
    "level": "debug",
    "targets": ["folio_core::layout=trace"],
    "file": true,
    "stderr": true,
    "dir": "",
    "max_bytes": 4194304,
    "keep": 3
  }
}
```

`level` 只管 folio 自己的代码（`folio_core` / `folio_app`）；依赖库（wgpu、cosmic-text 那一串）
固定在 `warn` 以上，否则「打开 debug」等于把整个 GPU 栈的日志一起打开。要连它们一起看，
用 `FOLIO_LOG`，它按 `RUST_LOG` 的写法解释并压过配置里的同类项：

```bash
FOLIO_LOG=debug cargo run                          # 一切 debug，包括依赖库
FOLIO_LOG=warn,folio_core::layout=trace cargo run  # 只逐行看排版
```

设备上取日志：

```bash
adb pull /data/user/0/dev.folio.reader/files/folio/data/logs/folio.log
adb logcat -s folio-mobile                          # 或看实时那一份
```

`FOLIO_DATA_DIR` 仍然压过一切（挪走整个数据根，日志跟着走），所以日志指不到应用目录时
还能手动挪到一个 `adb pull` 得到的地方。

## 错误处理

一条分界线，两个库：

- **后端 `folio-core` → `thiserror`**。`Error` 是一个能按分支处理的枚举：这一层不知道自己
  会被谁调，而「没有这本书」与「sqlite 打不开」的处置完全不同（一个提示读者，一个退出程序）。
  约定是每个变体都自带上下文（哪个路径、哪本书），并且**底层错误留在 `#[source]` 链上** ——
  压成字符串之后 `.source()` 走不下去，上层想按类型判断就只能匹配文案了。
- **界面 `folio-app` → `anyhow`**。界面对错误只有两个动作：把一条因果链说清楚，以及决定这
  一次要不要拦住那个操作。`ui::shared::error` 是那一条约定：干活的函数返回 `Result`，每个
  `?` 挂一句「当时在做哪件事」；回调没有调用方可 `?`，就在那一层 `report` 一次（`{:#}` 把整条
  链写进日志）。尽力而为的那些步（登记书目、落章节、记「最近打开」）走 `or_log`，
  而不是从前那句把原因一起丢掉的 `.ok()`。

`anyhow` 只挂在 folio-app 上：后端不许反向依赖界面的错误类型。它也不是新依赖树 ——
`gpui-base` / `gpui-component` 本来就带着。

## 测试样本

见 [`tests/fixtures/README.md`](tests/fixtures/README.md)：自动化测试用**程序合成**的 EPUB（可提交、可进 CI），真书只放 `tests/fixtures/local/` 且不入库。

手上的书到底还差多少样式，用这本书自己量（无界面，纯引擎）：

```sh
cargo run -p folio-core --example css-gap          # 仓库 fixtures 里的书
cargo run -p folio-core --example css-gap -- x.epub # 或任意一本
```

按**元素**排名的落差表 + 一份「把这些声明全丢掉 vs 现在」的几何对比都在 stdout。口径与限制见
[`plans/facts.md`](plans/facts.md)。

## 文档

开发计划在 [`plans/`](plans/README.md)（按里程碑拆文件）：

- [`plans/README.md`](plans/README.md) —— 索引、架构、钉死的架构决策、roadmap
- [`plans/facts.md`](plans/facts.md) —— 已钉死的事实：框架/平台/构建的实测约束
- [`plans/M5-refactor.md`](plans/M5-refactor.md) —— 架构重构 + 绘制简化 + 引擎查询接口
- [`plans/M6-selection.md`](plans/M6-selection.md) —— 选中一期
- [`plans/M7-annotations.md`](plans/M7-annotations.md) —— 批注二期设计

## License

MIT
