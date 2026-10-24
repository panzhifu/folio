---
name: codeing tool
description: This repo commits and pushes through jj (Jujutsu, colocated with .git), tags releases with git tag in the colocated repo, and searches code with rg / rga / ast-grep. Use when describing, splitting, squashing, undoing or pushing changes here, when reading jj status/log or moving the main bookmark, when cutting a version tag or a release build, when tempted to use git add/commit/checkout/stash in this repo, when looking for a symbol, signature, or text across the Rust sources, plans docs, or EPUB fixtures, and when using this machine's CLI setup (mold wired for the host triple, just without a justfile, scrcpy for on-device measurement).
---

## 概述

仓库自 2026-10-13 起用 `jj`（本机验证 0.46.0）提交，`jj git init` 建的 **colocated** 工作区：
`.jj/` 与 `.git/` 并存，`git clone` / GitHub Actions / `cargo ndk` 照常，但**写入一律走 jj**。

## 提交与推送

一个改动落地的顺序（`@` 就是当前 change，本身就是个提交，没有 index 这一步）：

```bash
jj status                       # 先看 @ 里有什么
jj describe -m "说明"            # 写完说明它就是一个提交
jj bookmark set main -r @       # main 不会跟着 @ 走，必须显式挪
jj git push --dry-run           # 报「哪个 bookmark 要前进 / 有没有分叉」
jj git push
jj new                          # 起下一个 change；不写这句，下一次编辑改进的就是刚推上去那笔
```

规则，都是踩过的：

- **不用 `git add` / `git commit` / `git checkout` / `git stash`**。colocated 之后 `.git/HEAD`
  是分离的（里面直接写 sha），这些命令改的是 jj 看不见的地方，两边就此分叉。撤销用 `jj undo`，
  回到旧提交用 `jj new <change_id>`，挪开改动就另起一个 change。
- `git status` / 裸 `git log` 读的是那个分离的 HEAD，**能报出根本不存在的改动**：实测刚用
  `jj describe` 提交完 README，`refs/heads/main` 已是新提交，`git status` 还在报
  `M README.md`。状态判断只信 `jj status`；要看 git 侧历史就显式 `git log main`。
- **jj 会把未被 ignore 的文件当成待提交改动收进 `@`**（git 的「untracked 就看不见」在这里不成立；
  `jj git init` 的第一次快照尤其如此）。落 `describe` 之前先读 `jj status` 里有没有来路不明的 `A`
  行 —— 本机那个量屏 scratch `.debug/` 就是这样以 20 个文件混进来的。处置两步、缺一不可：

  ```bash
  printf '/.debug/\n' >> .git/info/exclude   # 1. 先让 ignore 规则存在
  jj file untrack '.debug/**'                # 2. 再把已进 @ 的那批请出去（文件留在盘上）
  ```

  只加规则不够（已进 `@` 的不会自己退），只 `untrack` 也不够（下次快照又收回来）。
- 拆与合：`jj split`（一个 change 里的两件事拆成两个提交）、`jj squash`（合进父提交）、
  `jj abandon`（丢掉一个 change）。
- **`jj commit <路径> -m "说明"` 的落点跟直觉相反**（本机 2026-10-24 实测）：选中的那些路径留在
  **原来那个 change**（连 change_id 一起），**没被选中的改动被推到一个新的子 change、不带任何说明**，
  而 `-m` 写进的是原来那一份。于是当 `@` 里同时有「用户改到一半的东西」和「我的几个文件」时，这一句
  会**把用户那笔提交说明覆盖掉**，而他的改动落进一个没说明的新 change。要按路径分笔又不吞掉别人的说明，
  两条路：不带 `-m` 提交，然后 `jj describe -r <我那笔> -m ...`、`jj describe -m <他的原话>` 还回去
  （动手前先 `jj status` 把原话抄下来）；或者反过来用 `jj squash -f <来源> --into <目标> -- <路径>`
  只挑走自己要的那几个文件。自检：落笔后对每一笔跑 `jj diff --name-only -r <change>` 核对**文件归谁**、
  `jj log` 核对**说明还在不在**；被覆盖的那一份没丢，在 oplog 里（`jj undo` 退一步，或从刚才抄下的
  原文贴回）。
- `main*` 结尾的星号 = 这个 bookmark 还没推到远端；`main@origin` 是远端那份的位置。
- **推的时机由用户点头**，不要在同一轮修复里顺手推；`git rm`、删产物这类也单独确认。

### 打 tag 与发版

本机 jj 0.46.0 实测：**colocated 仓里 `git tag` 建出来的 tag jj 认** —— `jj tag list` 列得出，
`jj git push` 接得住（上一条「别用 git 写入」讲的是 commit/checkout/stash 那些会动 HEAD 与
index 的命令，tag 不在这之列：它只是一个 ref，jj 下一次快照会 import 进来）。

```bash
jj bookmark set main -r @        # 先把 main 挪到要发的那个 change
git tag v0.1.1                   # 打 tag
jj tag list                      # 确认列出的就是那一个（顺手留下的第二个会一起被推上去）
jj git push --dry-run            # 应报出「tag: v0.1.1 [add to <sha>]」这一行
jj git push                      # 真推 —— 这一步会触发发布 workflow，要用户明确点头
```

`--dry-run` 与 `jj tag list` 那两行不是仪式：**推 tag = 往公网发不可撤销的正式包**，而发出去的
Android 包永远换不了签名（换 keystore 等于所有已装用户卸载重装）。发版流程与四端的限制清单在
`README.md` 的「发版」那一节，删除测试 tag 用 `git tag -d <名>`。

### 提交信息

中文，散文式一句话讲清「改了什么 + 为什么」，分号分句，不用 `feat:`/`fix:` 前缀。正文写**为什么**
与实测数字：性能类改动要给改动前后的毫秒/字节对照，作废某条旧计划要给理由与文件行号。事实若已在
`Cargo.lock` 或 `plans/` 里，就引用它，别再抄第二份。

## 推送前的门

按顺序跑，全绿再落提交（`--lib` 那条约 3–4 秒，其余是编译bound）：

```bash
cargo fmt --all -- --check                 # CI 的这一道；报文件数，不要只说「跑过 fmt」
cargo check --workspace --all-targets      # 含 tests/ 与 examples/
cargo test --workspace --lib               # 全集，日常这条就够
```

拆成一串提交后，要核对「每个中间提交都编得过」：另开 worktree、共用主仓库的 target 目录，
依赖图保持热的，只重编两个 workspace crate，不链接大二进制：

```bash
git worktree add --detach /home/noke/Code/folio-commitcheck HEAD
cd /home/noke/Code/folio-commitcheck
for c in <sha…>; do git checkout -q --detach $c
  CARGO_TARGET_DIR=/home/noke/Code/folio/target cargo check --workspace --all-targets -q; done
git worktree remove --force /home/noke/Code/folio-commitcheck
```

别在 `/tmp` 下建 worktree（tmpfs 撑不住构建产物）。主仓库里有个 `/tmp/folio-wt` 是用户自己的，
不要动。

## 搜索代码

内置 **Grep 工具就是 ripgrep**，优先用它（`-n`/`-C`/`-A`/`-B`/`output_mode`/`glob`/`multiline`
都在参数里）。只有要管道、统计、`--json`、或搜非文本文件时才走 shell 的 `rg` / `rga`。

- **`rg` 的 `-r` 是 `--replace`，不是 grep 的「recursive」也不是「regexp」**：它吃掉紧跟的那个参数当
  替换文本。于是 `rg -rn 'a|b' crates`（把 grep 的习惯带过来）不会报错，而是**把所有命中替换成字母
  `n`**、连 `-n` 也没生效。真实危害不是少个行号，是它凭空造出一份「符号都改名了」的假证据 —— 本机
  已据此误判过一次不存在的重命名。要行号只写 `-n`；`rg` 本来就递归，不需要 `-r`。
  任何短 flag 与 `-r` 合并都危险（`-rn`、`-rw`、`-rl`）。
  **读输出时的自检**：命中处变成一个与 pattern 完全无关的短 token、或整片行数里夹着 `n`，就是踩了；
  这一条不能拿来当证据，重跑一遍只用 `-n` 并核对**匹配文本本身**还在。
  这也是上面那句「优先用内置 Grep 工具」的又一个理由 —— 那边 `-n` 是布尔参数，结构上不可能踩到。

- **ignore 只在「遍历」时生效，显式给出路径就照搜**（实测）：从仓库根跑 `rg` 会跳过 `target/`、
  `.debug/`（走 `.git/info/exclude`）、`reference/`、`/screenshots/`、`tests/fixtures/local/`；
  而 `rg 关键词 reference/` 这种把被 ignore 的目录**写全**的形式，`rg` 与 `rga` 都正常搜，
  不需要 `--no-ignore`。要连隐藏文件一起遍历才加 `-u` / `--hidden`。
- 常用形态：`rg -n 'fn font_extents|\.font_extents\(' crates`（签名 + 调用点一趟看全）、
  `rg -l <符号>`（哪些文件）、`rg -t rust`、`rg -g '!**/tests.rs'`、`rg -F`（字面量，中文文案里
  带括号时必加）。
- 跨行模式要显式开：`rg -U --multiline-dotall -c 'struct X \{[\s\S]*?\n\}' crates`。
- 按命中密度找改动落点：`rg -c <符号> crates | sort -t: -k2 -rn | head`（那句里的 `-rn` 属于
  `sort`，倒序数字；`rg` 那一头永远不该带 `-r`）。
- **rg 是文本级**：语法级的搜索与批量改写（比如「给某个方法加一个参数后找出所有少传一位的调用」）
  它做不到，走下面「结构级搜索」那条 `ast-grep`（本机装了 0.44.1，只有 `ast-grep` 这个命令名，没有 `sg`）。
- **搜 EPUB 里的正文不要用 `rga`**：它的抽取按扩展名挑外部命令，EPUB 那一支要 `pandoc`，本机没装，
  实测直接失败（`run_adapter … adapter: pandoc`）。解包管道就够：

  ```bash
  unzip -p 那本书.epub '*.xhtml' '*.html' | rg -n '关键词'
  ```

  `rga`（0.10.10）在明文目录与它认得且命令齐备的格式上仍然可用（`reference/` 那种克隆直接写全路径）。

- 本机 PATH 里的 `rg` 是 Qoder 插件自带的 **15.0.0**；系统那份是 `/usr/bin/rg`（**15.2.0**）。
  需要系统那份的行为时写全路径。

## 工具

### 链接器：mold 3.0.0，只接在主机 triple 上

`~/.cargo/config.toml` 里加的是

```toml
[target.x86_64-unknown-linux-gnu]
rustflags = ["-C", "link-arg=-fuse-ld=mold"]
```

**只给这一个 triple**：`[build] rustflags` 会连 build script 与 proc-macro 一起污染，而 Android 那条
交叉编译用的是 NDK 自己的 lld，不该看见 `-fuse-ld=mold`。实测 `cargo build -v` 确实把这条 flag 传给了
rustc，链出来的二进制能跑；`/usr/bin/ld.mold` 在，`gcc -fuse-ld=mold` 直接可用。要治的是那笔老账：
`touch crates/folio-app/src/lib.rs` 之后 `cargo test -p folio-app --no-run` 曾 >12 分钟，慢的全是
6 个 598–755 MB 的二进制各链接一次。**改 flag 会让整个 `target/` 失效，第一次要全量重编。**

### 结构级搜索：ast-grep 0.44.1（本机只有 `ast-grep` 这个命令名，没有 `sg`）

`rg` 只认文本；「给一个方法加了参数，找出所有少传一位的调用」这类活得按语法找。在 folio 上实测：

```bash
ast-grep run -p '$M.font_extents($$$)' -l rust crates           # 调用点 → layout/wrap.rs:133
ast-grep run -p 'fn font_extents($$$) -> $T $B' -l rust crates  # 实现体 → reader/measure.rs:404
```

批量带修补的改写走 `ast-grep scan -r <规则.yml>`，那要先写规则文件 —— 一次性的改动别建。
这里的 `-r` 是 `--rules`，与上面 `rg` 那个 `-r`（`--replace`）不是同一回事，别把一条的规矩带到另一条。

### 任务配方：just 1.58.0（仓库里**还没有** `justfile`）

README 里那几条环境变量配方（NDK 的 `CC/CXX/AR` + `CARGO_TARGET_DIR`、`gradle assembleDebug --offline`、
adb 三连）正适合收成 `just mobile-check` 这样的命令。**建 `justfile` 之前先问用户。**

### 真机：scrcpy 5.0.1，adb 上连着设备（`adb devices` 查当前序列号）

`.debug/` 那套 `adb screencap → magick/identify 量像素` 适合静态尺寸；翻页卷曲这种逐帧的东西该录：

```bash
scrcpy -n -r 卷曲.mp4 --time-limit=10   # -n 只镜像不接管输入，10 秒自动停
ffmpeg -i 卷曲.mp4 帧-%03d.png          # 抽帧，再交给 magick 量
```

`-m/--max-size` 降分辨率，`--window-title` 给窗口起名（配合 `slurp` 取几何）。别在用户不知情的情况下
启动镜像窗口或接管设备输入。

### 已装 / 没装

已装：`jj 0.46.0` `ast-grep` `mold` `just` `scrcpy` `ffmpeg` `rg 15.2`（PATH 里那份是 Qoder 插件自带
的 15.0.0，系统那份写全路径 `/usr/bin/rg`）`rga` `fd` `bat` `eza` `jq` `grim` + `slurp`（Wayland
截图 + 取几何）`imagemagick`（`magick`/`convert`/`identify`）`sqlite3` `zoxide` `lld`。

没装、要用先问用户（都在 Arch `extra` 源里）：`pandoc-cli`（补上它 `rga` 才吃得动 EPUB）`hyperfine`
`cargo-machete` `bacon` `cargo-watch` `sccache` `tokei` `fzf` `git-delta` `taplo`。
