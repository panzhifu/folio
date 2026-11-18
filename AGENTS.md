# Folio 开发入口

Folio 是 Rust + GPUI Kit 原生阅读器，不嵌 WebView。格式、排版、存储等引擎 crate 零 GPUI 依赖，folio-app 是唯一 GPUI 依赖方。

先读 [工程规范](doc/工程规范.md) 和 [软件架构](doc/软件架构.md)，再按 [文档索引](doc/文档索引.md) 读本次领域。规则登记在 [config/folio-governance.json](config/folio-governance.json)，使用方式见 [治理使用](doc/治理使用.md)。用户明确指令优先；计划不覆盖规范，现状以代码为准。

## 修改与验证

- 先核对源码、配置、测试与工作区改动，保留他人工作。
- 规则、依赖、文档入口或技能修改后运行 `cargo xtask governance`；能提供历史基准时加 `--base <git-ref>`。UI/crate 边界修改还运行 `cargo xtask boundaries`。
- `cargo xtask change-plan` 只报告 checkout 和验证类别；不会自动执行全量测试。
- 迭代只跑本轮新增/修改用例，例如 `cargo test -p <crate> --lib <用例名>`。全量 `cargo test --workspace --lib` 或 CI 同款等用户要求“推送”时再跑。
- 测试放被测文件底部，点名导入公开项；不用 `use super::*`，避免 GPUI 的 test 属性宏冲突。已有独立测试文件按现状维护。
- 纯注释清理可用 `cargo xtask comment-check <base> <path>...`，另审 rustdoc、doc test、SAFETY 和工具指令。优先删重复说明和历史流水，保留单位、特殊值、兼容与生命周期约束。
- 修改架构同时更新现状文档；不引用已删除的 doc/development.md、doc/design/ 或 doc/README_zh.md。

## 架构与路径

- app 的目标依赖方向：shell → features → components → services → foundation；host/platform 提供平台能力。既有越界登记基线，只减不增。
- 生产 `cfg(mobile)` 只放 host/platform；遗留与测试门控由边界检查分别处理。
- `folio-model` 只放多个 crate 共用的数据；格式不认识存储/网络，排版不识别具体格式。
- 格式写回 API 仍未实现；配置仍在 core，不因计划存在就搬进零业务依赖的 base。
- `tests/fixtures/local/` 放不入库的真书；合成与有许可的 fixtures 可入库。
- `reference/` 是只读参考克隆（含 AGPL），不入库、不参与构建。scratch 放已忽略的 target 下；不把临时文件混入当前 change。

## 常用命令与平台

`cargo run` 启动桌面。Linux 构建需要 fontconfig、Wayland、xkbcommon 开发包；听书需要 libclang 和 libspeechd 头，运行需要可用 Vulkan 驱动。

Android 交付按以下三步，cargo-ndk 必须带 `-P 26`（nativewindow 从 API 26 才有，应用 minSdk 也是 26）：

```bash
cargo ndk -t arm64-v8a -P 26 -o crates/folio-app/android/app/src/main/jniLibs build --release -p folio-app
(cd crates/folio-app/android && ./gradlew assembleRelease)
adb install -r crates/folio-app/android/app/build/outputs/apk/release/app-release.apk
```

cargo-ndk 4.x 作为链接器外壳是正常行为，错误看后续 clang/ld.lld；jniLibs 的带 hash 副本由 Gradle excludes 处理，不手动删除。

**不主动查看手机。** 用户点名后才使用 `.agents/skills/scrcpy-android` 的虚拟显示截图、点击、滑动或录像；禁止 adb shell input、adb exec-out screencap 操作用户主屏。安装不算测试，装完不自动启动或点按。

## 版本控制

约定使用 jj/git colocated；先确认实际 checkout 是否有 `.jj/` 及 jj 命令。环境不符时说明，不自动初始化或切换。

- 禁止 `git commit`、`git checkout`、`git stash`。
- jj 会收录未忽略文件，scratch 必须先有忽略规则。
- 用户要求推送后：`jj bookmark move main --to @`，若刚 commit 且当前空则目标用 `@-`；先 `jj git push --dry-run` 再 `jj git push`。新 tag 还需 `jj tag track`。
- 提交说明用中文，说明原因和实测事实，不列文件清单。

## Skills

项目技能仅保留 `.agents/skills/` 一份，来源由 [skills-lock.json](skills-lock.json) 管理，project/reference 分类在治理注册表。按任务加载，不批量读取所有技能。

| 任务 | 技能 |
| --- | --- |
| 实施范围、注释精简、验证 | folio-change-workflow |
| crate/API/状态归属与架构评审 | folio-architecture-review |
| SQLite、删除、同步、备份恢复 | folio-data-integrity |
| GPUI 界面 | **先读 gpui-kit 与 gpui-kit-design-guides**；其它设计技能仅作补充 |
| 用户指定的 Android 真机查看 | scrcpy-android |
| 格式、排版、字体、坐标、缓存 | folio-reading-engine |
| GPUI 页面与交互交付 | folio-gpui-delivery（仍先读两条 GPUI 基础技能） |
| Transport、网盘、翻译、AI/SSE | folio-network-ai |
| 依赖升级、打包、提交推送 | folio-release-validation |

GSAP、transitions 和其它 Web 设计技能只借判断，不把 JS/CSS 实现搬进 Rust。oil-ui、impeccable、媒体技能可能下载工具/素材、写文件或消耗付费积分，只在当前任务确实需要时使用；技能文字不自动授予外部操作权限。

安装/删除使用 skills CLI，并同步内容与 lock：

```bash
npx skills add <owner>/<repo> --skill <name> -y
npx skills add <本地目录> --skill <name> -y
```

本仓库现用 v1 lock；新增本地技能已用 `npx skills@1.5.26 add` 验证。升级 CLI 前在临时目录核对旧条目哈希，1.9.0 的 v1→v2 迁移会清空不匹配的旧哈希，不得直接接受这种变更。

可指定 `--agent codex` 避免多工具副本；安装后只清理本次创建且已确认目标的 `.claude/skills`、`.qoder/skills` 链接，不删除其它用户文件。

两处有意例外：impeccable 的 font-index.json 超过 jj 1 MiB 新文件限制，已忽略，用到其脚本前通过 skills CLI 补资产；transitions-ui-generation 上游 description 的冒号导致 YAML 无效，本地改为 folded scalar，lock hash 对应修正版。上游修复前更新时保留这项偏离，不为第三方资产提高全仓快照上限。
