# AGENTS.md — 给在本仓库干活的 agent

## 项目是什么

Folio：Rust + [gpui-kit](https://gpui-kit.com) 写的原生 GPU 阅读器，覆盖桌面
（Linux / macOS / Windows）与 Android。不嵌 WebView，正文由自研排版引擎
（DOM → CSS 子集 → 排版）产出，`folio-core` 零 GPUI 依赖是硬边界。

文档地图：

- [README.md](README.md)（英文，读者向）/ [doc/README_zh.md](doc/README_zh.md)（中文）
- [doc/development.md](doc/development.md) —— 构建、测试、Android 交叉编译、日志、发版配方
- [doc/design/README.md](doc/design/README.md) —— 设计稿 51 屏：手机 compact 一档（384×853）与
  平板两朝向（800×1280 / 1280×800），共用的那份尺是 `_kit.py`；
  **SVG 是真相源，改版式改生成器再重出**
- [packaging/README.md](packaging/README.md) —— 安装包里非代码资产（图标、desktop 条目）的真相源

## 仓库布局

```
crates/folio-core/   引擎：21 个格式 provider、排版、锚点与批注、SQLite、日志
crates/folio-app/    应用壳；src/ui/ 下 shared / widgets / features / shell / host / platform
                     features 按功能纵切：reader / shelf / notes / stats / settings / welcome
doc/                 面向人的文档
packaging/           四端安装包里的非代码资产与打包脚本
tests/               fixtures：程序合成的书可入库；真书只放 tests/fixtures/local/ 且不入库
reference/           借鉴用的上游只读克隆（含 AGPL），gitignore，永不入库、不参与构建
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

## 版本控制：jj（colocated）

仓库是 jj/git colocated（`.jj/` 与 `.git/` 并存），CI 与 `cargo ndk` 不受影响。三条硬规矩：

- **别在 shell 里用 `git commit` / `git checkout` / `git stash`** —— colocated 之后
  git 的 HEAD 是分离的，那些改动 jj 看不见。
- **jj 把未被 ignore 的文件全收进当前 change** —— 本机 scratch 先进 `.gitignore` 再动。
- **推送**：`jj bookmark move main --to @` 把 main 挪过来，`jj git push --dry-run`
  确认后再 `jj git push`；新 tag 还要 `jj tag track`（见 doc/development.md 的发版一节）。

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

三条设计类技能在「间距阶、颜色、层次」上**重叠**：动 GPUI 界面时仍以
`gpui-kit-design-guides` 为准（那是本仓库依赖的框架规范），另两条只作通用设计原则的补充。
`refactoring-ui` 与 `oil-ui` 都带 `references/` 按需读，不是一次性全量加载。

`oil-ui` 装的是**可执行的工作流**，不只是一份文档：它自带 `scripts/`，会要求 agent
跑 `node shoot.mjs`（驱动 Chrome 真实截图）、`python3 build_explorer.py`（生成对比页）、
`curate-media.mjs`（从 Wikimedia / NASA / Poly Haven 等联网下载素材到仓库里），
frontmatter 还预授权了 `check_update.sh`（联网自查更新），生图那条路会走
`npx github:oil-oil/oil-cli login` 并**消耗 oil-oil 账户的付费积分**。用之前先确认这一
切是当前任务需要的；它的主场是 web/App 界面，而本仓库是 GPUI 原生渲染、不嵌 WebView，
"起服务 + 截 HTML"那条主路对这里的界面基本用不上。

两条约定：

- 写或改桌面界面之前，先加载 `gpui-kit` 与 `gpui-kit-design-guides` 两条。
- 曾经给 Claude Code 用的 `.claude/skills` symlink 已删除 —— 技能的读取路径只有
  `.agents/skills/` 一份，别再建第二份副本或 symlink。
