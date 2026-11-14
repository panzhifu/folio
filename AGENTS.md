# AGENTS.md — 给在本仓库干活的 agent

## 项目是什么

Folio：Rust + [gpui-kit](https://gpui-kit.com) 写的原生 GPU 阅读器，覆盖桌面
（Linux / macOS / Windows）与 Android（另有 iOS / HarmonyOS 两条入口臂）。
不嵌 WebView，正文由自研排版引擎（DOM → CSS 子集 → 排版）产出；引擎与界面分离，
引擎各 crate 零 GPUI 依赖是硬边界，`folio-app` 是唯一的 GPUI 依赖方。

## 文档地图

`doc/` 下这十二篇是现状的真相源；`软件架构.md` 开头写明「以代码为准，改了架构顺手改
这一篇」。先看总纲，再按要改的层挑对应的 API 篇。

| 篇 | 讲什么 |
|---|---|
| [doc/软件架构.md](doc/软件架构.md) | **总纲，先读**：workspace 与 crate 依赖图、各层职责与边界、关键数据流、横切机制、平台矩阵；§7 是 crate 拆分路线图 |
| [doc/地基.md](doc/地基.md) | `folio-base`：目录、配置、日志、作业池、取消、身份、内存预算、时间、占用 |
| [doc/数据结构.md](doc/数据结构.md) | `folio-model`：共用类型与三条规矩（只有数据 / 两个以上消费方才进 / 换 crate 不换形状） |
| [doc/format.md](doc/format.md) | 格式解析六个 API：识别 / 打开 / 章表 / 资源 / 取内容 / 写回 |
| [doc/渲染.md](doc/渲染.md) | 排版（`folio-render`）与绘制（`folio-app`）两段管线、仿真翻页 |
| [doc/网络.md](doc/网络.md) | `Transport` / `RemoteStore`、OPDS、Wi-Fi 传书、局域网同步、更新检查、会员兑换 |
| [doc/AI.md](doc/AI.md) | `folio-ai` 的问答；配置到参数的挑选留在 core 入口层 |
| [doc/翻译.md](doc/翻译.md) | `folio-translate` 的选区翻译（三只引擎）与本地词典（StarDict） |
| [doc/听书.md](doc/听书.md) | `folio-speech` 的听书引擎收口（两条管道）与会话侧分工、平台矩阵 |
| [doc/字体.md](doc/字体.md) | `folio-fonts` 的随书字体装载（text）与用户字体库（fonts）、FontsError |
| [doc/储存.md](doc/储存.md) | `Store` 的 SQLite API：书目 / 进度 / 批注 / 统计 / 笔记聚合 |
| [doc/服务.md](doc/服务.md) | 服务层与导入管线（写得最全的一条业务线）、`Services` 组合根 |

**别把设计稿当现状**：各篇明确标了「现状 ≠ 目标」的地方 —— `format.md` 末行只
剩 API 6（写回）尚未实现（1–5 已有对应物，4 资源清单于 4015abf 落地）、`地基.md`
的 settings 层仍在 core。拆分路线图（`软件架构.md` §7）五个阶段已于 2026-11-12
全部收尾，路线图本身成了记录。动这些之前先读对应段落，别照着文档以为已经有代码。

[README.md](README.md)（英文，读者向）/ [CHANGELOG.md](CHANGELOG.md) /
[packaging/README.md](packaging/README.md)（安装包里非代码资产：图标、desktop 条目）。
旧的 `doc/development.md` / `doc/design/` / `doc/README_zh.md` 已在 2026-11-10
随旧 doc 一并删除，别再引用；构建、测试、发版的约定以本文下面几节为准。

## 仓库布局

```
crates/
  folio-base/    地基：时间、路径、作业池、取消、日志、身份、内存预算；零业务依赖
  folio-model/   公共数据模型：诸 crate 共用的类型，只有数据（三条规矩见其 lib.rs）
  folio-format/  文件格式解析：字节 + 路径 → 统一文档模型（21 个 provider、DOM、CSS、scan、
                 Document trait 与 BookDocument 门面）
  folio-net/     网络面：Transport / RemoteStore / WebDAV + OPDS / 更新 / 兑换 /
                 网盘引擎与局域网两族（原 core::net 的机制半边，阶段三扩容）
  folio-account/ 账号：一台安装自持的 128-bit 密钥与局域网双向证明（ring 唯一用户）
  folio-store/   SQLite 书库：书目/进度/批注/书签/集合/统计/导入日志 + 封面缩略图的
                 落盘约定（原 core::store 与 content::assets，rusqlite 随行）
  folio-translate/ 选区翻译三只引擎与本地 StarDict 词典（原 folio-ai::translate 与 core::dict）
  folio-ai/      AI 问答：一层 provider 与三档预设问法（原 core::net::ai）
  folio-render/  排版引擎：DOM + 样式 → 扁平的块盒 / 行盒两张表（原 core::layout）
  folio-speech/  听书引擎收口：专职线程持有 tts 后端、两条管道（原 core::speech）
  folio-fonts/   字体域：随书字体装载与子集化（text）+ 用户字体库下载落盘（fonts）；
                 skera / read-fonts / write-fonts / ttf-parser 只在这里
  folio-search/  书内搜索：现读现扫不建索引，只认一本书与一个查询词（原 core::search）
  folio-core/    引擎：锚点与命中、导入、网络的入口层、同步编排；
                 内部按 content / ingest / net / infra 单向分区（store 已独立）
  folio-app/     应用壳：GPUI 界面、平台入口、原生桥；src/ui/ 依赖方向只有一条
                 shell ▶ features ▶ components ▶ services ▶ foundation（host / platform 在两侧），
                 `cfg(mobile)` 只许出现在 host 与 platform
doc/             十一篇架构与分层 API 文档（见上面的文档地图）
packaging/       四端安装包里的非代码资产与打包脚本
tests/           fixtures：程序合成的书可入库；真书只放 tests/fixtures/local/ 且不入库
reference/       借鉴用的上游只读克隆（含 AGPL），gitignore，永不入库、不参与构建
```

## 常用命令

```bash
cargo run                      # 桌面窗口
cargo test --workspace --lib   # 日常全集，快
cargo test --workspace         # CI 同款；慢是买「每个 target 都编得过」
```

Linux 需要 fontconfig / Wayland / xkbcommon 开发包与可用 Vulkan 驱动。
测试内联在被测文件底部、点名导入（不用 `use super::*`，`gpui_kit` 根有个名叫
`test` 的属性宏会把 `#[test]` 递归展开成它）、只引 `pub` 项。

## Android 打包三步：cargo-ndk 必须带 `-P 26`

```bash
cargo ndk -t arm64-v8a -P 26 -o crates/folio-app/android/app/src/main/jniLibs \
    build --release -p folio-app                    # 编 libfolio_app.so 落进 jniLibs
(cd crates/folio-app/android && ./gradlew assembleRelease)
adb install -r crates/folio-app/android/app/build/outputs/apk/release/app-release.apk
```

`-P 26` 不能省（2026-11-14 实测）：本机 cargo-ndk 4.1.2 的默认 API 是 **21**，而
gpui 链接要的 `libnativewindow.so` 在 NDK（r30，`/opt/android-ndk`）sysroot 里
**26 起才有**，不带就是 `ld.lld: error: unable to find library -lnativewindow`；
app 的 minSdk 也是 26。等价写法是环境变量 `CARGO_NDK_PLATFORM=26`。

两条排错口径，别自己吓自己：

- rustc 报「linking with /usr/bin/cargo-ndk」**不是配置错** —— 4.x 把自己垫在
  NDK clang 前面当壳（靠 `_CARGO_NDK_LINK_*` 环境变量转手，strings 里可验），
  真错看它下面那行 ld.lld / clang 的输出。
- jniLibs 里 cargo-ndk 捎带拷进来的带哈希 cdylib 副本（`libdjvu_rs-<hash>.so`
  之类）**不用管也别删**：链进主库的是 rlib，gradle 的
  `packaging.jniLibs.excludes`（app/build.gradle.kts）已把它们排除出 APK，
  手动删了下次构建又会生成。

## 测试：只跑新加的，全量等推送，真机走 scrcpy

- **迭代时只跑这一轮新加 / 改到的那几条测试**（按名字过滤，通常一两秒）；
  **全量**（`cargo test --workspace --lib` 或 CI 同款）等用户说「推送」时再统一跑。
- **不主动看手机。** 真机查看（截图、点按）用户点名才做，且一律走 `scrcpy-android`
  技能的 scrcpy 虚拟显示（`.agents/skills/scrcpy-android`）截图、点击、滑动、录像；
  不许用 `adb shell input`、`adb exec-out screencap` 之类直接驱动或截取用户正在用的
  那块屏 —— 那会把手机抢走。
- 打包装机（`adb install -r`）不算测试，交付新包时可以做；但装完**不许顺手启动、点按**。

## 版本控制：jj（colocated）

仓库是 jj/git colocated（`.jj/` 与 `.git/` 并存），CI 与 `cargo ndk` 不受影响。三条硬规矩：

- **别在 shell 里用 `git commit` / `git checkout` / `git stash`** —— colocated 之后
  git 的 HEAD 是分离的，那些改动 jj 看不见。
- **jj 把未被 ignore 的文件全收进当前 change** —— 本机 scratch 先进 `.gitignore` 再动。
- **推送**：`jj bookmark move main --to @` 把 main 挪过来（若刚 `jj commit` 过、当前
  change 是空的，要挪的是 `@-`），`jj git push --dry-run` 确认后再 `jj git push`；
  新 tag 还要 `jj tag track`。发版步骤以本节为准，旧 `doc/development.md` 已删除。

提交说明用中文，写**为什么**与实测到的事实（红在哪、量出来是多少），不写改动文件的清单。

## Agent skills（`.agents/skills/`）

装在仓库里的技能跟着仓库走，所有 agent 按这个路径读。每条技能一个目录：
`SKILL.md` 是正文，`agents/openai.yaml` 是给 OpenAI 系 agent 的元数据。
[skills-lock.json](skills-lock.json) 记下每条的来源（GitHub 仓库 + 仓内路径）与
`computedHash`，是「装没装对版本」的真相源 —— 增删技能时 lock 与盘上内容要同一笔提交。

| 技能 | 来源 | 干什么 |
|---|---|---|
| `gpui-kit` | `longbridge/gpui-kit` | GPUI Kit 组件用法（Button / Input 等，`gpui_kit::component`） |
| `gpui-kit-design-guides` | `longbridge/gpui-kit` | 设计规范：动任何界面（布局、间距、颜色、组件选择、交互状态）之前先读 |
| `grilling` | `mattpocock/skills` | 把一个方案一轮轮拷问成决策树：每轮只问前提已定的问题并给推荐答案 |
| `grill-me` | `mattpocock/skills` | grilling 的入口，只负责把它调起来 |
| `refactoring-ui` | `s0xDk/refactoring-ui-skill` | Refactoring UI 那套具体规则：受限的间距/字号/颜色/阴影阶、靠 weight 与 color 做层次、靠模拟光照做深度 |
| `oil-ui` | `oil-oil/oil-ui` | 设计方向探索与评审：出多风格同屏对比页、按真实截图打磨层级。**会驱动 agent 跑脚本**，见下 |
| `impeccable` | `pbakaus/impeccable` | 前端界面的总入口：层级、排版、颜色、动效、UX 文案、可访问性、边界态，另有成体系的「更胆/更静/说清楚」各档。**自带脚本与可下载引擎**，见下 |
| `emil-design-eng` | `emilkowalski/skills` | Emil Kowalski 的 UI 打磨总则：组件的 hover/press 档、动效该不该做、隐形细节；纯一份 `SKILL.md` |
| `animate` … `review-animations` 共 7 条 | `emilkowalski/skills` | 同一包里的动效分册：`animate`（从零定一段动画：要不要动→目的→工具→属性→曲线与时长→打断与退场）、`apple-design`（WWDC 那套流体物理交互搬到 web：弹簧、手势、可打断、材质与景深）、`review-animations`（按高标准审一份动效代码）、`improve-animations`（审整个仓库的动效、出优先级清单与实施计划，**只读不改**）、`find-animation-opportunities`（找哪里该动、并否掉不该动的，只提不改）、`animation-vocabulary`（模糊描述→精确术语的反查词典）、`animation-performance`（掉帧成因：合成层、layout/paint/composite、CSS vs WAAPI vs rAF） |
| `gsap-core` … `gsap-performance` 共 8 条 | `greensock/gsap-skills` | GSAP（JS 动画库）官方技能：core / timeline / scrolltrigger / plugins / utils / react / frameworks / performance，每条一份 `SKILL.md` |
| `transitions-dev` | `jakubantalik/transitions.dev` | 现成 web 过渡的分册（盘上 35 条，一条一份 md）+ `_root.css` 里的动效 token 阶（时长 / 位移 / 缩放 / 模糊 / 缓动）。有 `transitions apply` 这类**会往工程里写文件**的命令 |
| `transitions-polish` | `jakubantalik/transitions.dev` | 上一条的附加档：按 token 阶调已有的动效，含开与关不对称、hover in 与 hover out 分别给值 |
| `transitions-ui-generation` | `jakubantalik/transitions.dev` | 「AI 正在生成这个界面」那一档：点阵线框 → 扫描线揭示真 UI、标题按字形轮廓逐字画、图像从自己的像素铺半调；带 `ui-generation.js` 等无依赖运行时与 demo html。**这条的上游 frontmatter 有语法错，盘上是本地改过的**，见下 |
| `scrcpy-android` | 本仓库（`sourceType: local`） | 用 scrcpy 虚拟显示在 Android 上截图 / 点击 / 滑动 / 录像，不占用手机主屏 |

设计类技能在「间距阶、颜色、层次、动效」上**重叠**：动 GPUI 界面时仍以
`gpui-kit-design-guides` 为准（那是本仓库依赖的框架规范），其余只作通用设计原则的补充。
`refactoring-ui`、`oil-ui`、`impeccable`、`transitions-dev` 都带按需读的分册
（`references/` 或一条一份的 md），不是一次性全量加载。

这二十条（`impeccable`、emil 那 8 条、`gsap-*` 8 条、`transitions-*` 3 条）的**主场是
web**：GSAP 是个 JS 库，transitions.dev 交付的是 CSS，而 folio 的界面由 GPUI 原生渲染、
不嵌 WebView，正文那套 CSS 子集里根本没有 `transition`。装它们是为了借**判断**——什么时候
该动、动多大、时长与缓动怎么成阶、开与关为什么不对称、`prefers-reduced-motion`
怎么让路——不是把代码搬进来。它们的示例代码不是本仓库的改法。

`oil-ui` 装的是**可执行的工作流**，不只是一份文档：它自带 `scripts/`，会要求 agent
跑 `node shoot.mjs`（驱动 Chrome 真实截图）、`python3 build_explorer.py`（生成对比页）、
`curate-media.mjs`（从 Wikimedia / NASA / Poly Haven 等联网下载素材到仓库里），
frontmatter 还预授权了 `check_update.sh`（联网自查更新），生图那条路会走
`npx github:oil-oil/oil-cli login` 并**消耗 oil-oil 账户的付费积分**。用之前先确认这一
切是当前任务需要的；它的主场是 web/App 界面，而本仓库是 GPUI 原生渲染、不嵌 WebView，
"起服务 + 截 HTML"那条主路对这里的界面基本用不上。

`impeccable` 同理，也不是一份纯文档：盘上 2.3 MB / 63 个文件里，
`scripts/impeccable` 是一个 POSIX launcher，包里**没有**引擎二进制（`bin/<os>-<arch>/`
不存在），第一次跑会从 `github.com/pbakaus/impeccable` 的 release
（tag `engine-v<版本>`）下载到 `~/.impeccable/bin/<版本>/`，并按
`scripts/engine-checksums.txt` 的 sha256 校验 —— 清单里没有这一版的校验和就直接
拒绝下载（fail closed，连网络都不发）；`scripts/live-browser*.js`（单 `live-browser.js`
就 553 KB）驱动 Chrome 做实时截图与就地改 DOM，`scripts/data/font-index.json` 1.1 MB
是字体索引，`agents/*.toml` 还定义了四个子 agent（documenter / asset-producer /
manual-edit-applier / finish-reviewer，另有 `reference/degraded/` 那份无子 agent 时的
降级路径）。`reference/` 是 45 份按需读的 md，别整包加载。用之前先确认这条路
是当前任务需要的。

那 1.1 MB 的 `scripts/data/font-index.json` **不在仓库里**：它正好超出 jj 快照新文件的
1 MiB 上限（`/target` 之外唯一被这条上限挡住的东西），而它是第三方技能自带的资产 —— 为了
它去改仓库的快照上限不值。技能重装一次就自己回到盘上，所以新克隆里缺这一格时先
`npx skills add pbakaus/impeccable` 补回来，再跑 impeccable 自己的脚本。规则写在
`.gitignore` 里，那条拒绝的警告也随之消失。

`transitions-ui-generation` 盘上那份与上游**不一样**，是一处有意的本地偏离：上游
`skills/transitions-ui-generation/SKILL.md` 的 frontmatter 里 `description:` 是一条没加
引号的 plain scalar，值内有两处「冒号+空格」（`Five variants: ui` 与
`the default: a dotted wireframe`），YAML 于是把整段当嵌套映射拒掉 ——
`Nested mappings are not allowed in compact mappings`，skills CLI 直接**跳过这条不装**。
同包的 `transitions-dev` / `transitions-polish` 实测各 0 处「冒号+空格」，所以那两条
装得上（这个差别是判断依据，不是猜的）。改法是把那一行换成 folded block scalar
（`description: >-` 再加一行缩进），文本一字未动（1409 字符，`yaml.safe_load` 后
`name` / `license` / `description` 三键齐全）。代价写清楚：`npx skills update` 从上游
拉不回这条，要重来只能照上面自己改；上游哪天自己加了引号，重装原版覆盖这份即可。
lock 里记的是上游坐标（`jakubantalik/transitions.dev` + `skillPath`），
`computedHash` 记的是改过的那份，所以对不上上游原文，这是刻意的。

装与删走 skills CLI，项目级安装落进 `.agents/skills/` 并同步改写
[skills-lock.json](skills-lock.json)（2026-10-27 这一批 20 条就是这么进的）：

```bash
npx skills add <owner>/<repo> --skill <name> -y     # 一次一条，或 --skill '*' 整包
npx skills add <本地目录> --skill <name> -y          # 上游那份读不进 YAML 时（见上）走这条
```

三条约定：

- 写或改桌面界面之前，先加载 `gpui-kit` 与 `gpui-kit-design-guides` 两条。
- 技能的读取路径只有 `.agents/skills/` 一份，别再建第二份副本或 symlink。
  skills CLI 每装一次都会顺手给 `.claude/skills/`、`.qoder/skills/` 各做一份指向
  `.agents/skills/` 的 symlink 副本（它认 79 个 agent），这两处已进 `.gitignore`，
  装完删掉即可 —— 之前给 Claude Code 用的那份 `.claude/skills` 就是这么删的。
- `transitions-dev` 的 `transitions apply` / `transitions refine` 与 `impeccable` 的
  live 那条路都会**往工程里写文件**（CSS、DOM、素材）；在本仓库动手前先确认写的
  是当前任务要的东西，别把 web 的 CSS 落进 Rust 源码树。
