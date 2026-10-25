# 开发者手册：构建 · 测试 · Android · 日志 · 发版

给要自己编、自己打包、自己发版的人。读者向的介绍在
[根 README](../README.md) 与 [中文版](./README_zh.md)，这里只管动手的事。

## 工具链与平台依赖

- **Rust**：CI 钉在 1.99.0（`release.yml` 里的 `dtolnay/rust-toolchain@1.99.0`），
  本地对齐这个版本，省得「我这里好好的」。
- **Linux 桌面**：fontconfig、Wayland、xkbcommon 的开发包，加一块能用的 Vulkan
  驱动。Wayland 与 X11 都验证过能跑。

## 日常命令

```bash
cargo run                      # 桌面窗口
cargo test --workspace --lib   # 日常循环：快，而且是全部用例
cargo test --workspace         # CI 同款：慢几分钟，买的是
                               # 「全新克隆里每个 target（bin/cdylib/staticlib）都编得过」
```

慢的那条是**有意保留的**，不是没优化到 —— 移动端宿主要的正是 cdylib 与 staticlib，
`--lib` 根本不碰它们。写测试的三条约定：

- 测试内联在**被测代码所在的文件**底部（`#[cfg(test)] mod tests`）。
- **点名导入，不用 `use super::*`** —— 这条在 `folio-app` 不是风格问题：
  `gpui_kit` 根有个名叫 `test` 的属性宏，glob 进来会让 `#[test]` 递归展开成它。
- **只引 `pub` 项**。想测私有实现，先想它为什么不是公开的。

## Android 交叉编译

```bash
export ANDROID_NDK_HOME=/opt/android-ndk
cd crates/folio-app
cargo ndk -t arm64-v8a -P 26 -o android/app/src/main/jniLibs build --release --lib
```

打包安装启动走系统里的 Gradle，**不用 `./gradlew`**（wrapper 会重新下载发行版）；
`--offline` 保证依赖只来自本地缓存，冷缓存的新机器去掉它：

```bash
cd crates/folio-app/android
gradle assembleDebug --offline
adb install -r app/build/outputs/apk/debug/app-debug.apk
adb shell am start -n dev.folio.reader/dev.gpui.mobile.GpuiActivity
```

两个实测过的坑：

- **重打出一份大得离谱的 APK**（实测 372M 对 36M）不是依赖坏了：AGP 的 zipflinger
  复用旧 APK 就地改写，新内容比旧的小时留下空洞。删掉 outputs 里的旧 APK 再打。
- 只想类型检查、不出 `.so`，用裸 `cargo check`，但要把 NDK 的 C 工具链指全 ——
  `cargo ndk` 会自动注入这几个变量，裸 `cargo` 不会，缺 `AR` 时 `libsqlite3-sys`
  会在打包静态库那步失败：

  ```bash
  TC=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin
  CC_aarch64_linux_android=$TC/aarch64-linux-android26-clang \
  CXX_aarch64_linux_android=$TC/aarch64-linux-android26-clang++ \
  AR_aarch64_linux_android=$TC/llvm-ar \
  CARGO_TARGET_DIR=~/.cache/folio-mobile-android \
    cargo check -p folio-app --target aarch64-linux-android --lib
  ```

## 日志

日志是后端的一部分：`folio-core` 里最容易出错的地方（书库迁移、EPUB 容器、排版）
直接发 `tracing` 事件，subscriber 装在 `folio_core::logging`。三层输出共用一个
过滤器：**文件** `<数据根>/logs/folio.log`（开机轮转成 `.1`/`.2`，本轮写满就停手）、
**stderr**（桌面默认开）、**宿主**（Android 的 logcat）。

级别写在 `config.json` 的 `logging` 一节，**改完重启就生效，不用重新编译**：

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

- `level` 只管 folio 自己的代码；依赖库（wgpu、cosmic-text 那一串）固定在
  `warn` 以上，否则「打开 debug」等于把整个 GPU 栈一起打开。
- `targets` 按 `RUST_LOG` 的写法按模块覆盖，前缀要整段对上模块边界，最深的那条赢。
- 设备上取不到文件时，把 `dir` 指到 `/storage/emulated/0/Android/data/dev.folio.reader/files`
  一类的位置，`adb pull` 直接取走；`FOLIO_DATA_DIR` 环境变量仍压过一切（挪走整个数据根，
  日志跟着走）。桌面上临时连依赖库一起看，用 `FOLIO_LOG=debug cargo run`。

## 发版（单仓形状）

打一个 `v<版本>` 的 tag 推上 origin，就触发 `release.yml`：`version` 取版本与更新
日志段，`notices` 逐字节比对第三方声明，`preflight` 只查 Android 签名的四个
secret 在不在（三十秒红完，不烧构建机时），然后 Linux / macOS / Windows / Android
四条并行出包，最后 `publish` 用本次 run 自带的 `GITHUB_TOKEN` 建 release。

### 发版之前要有的三样

每一样都红过一次，缺的时候红的样子写在后面：

1. **`CHANGELOG.md` 存在且有对应段**。`version` job 用
   `python3 packaging/changelog_section.py --version <v> --out body.md CHANGELOG.md`
   取 `## [<版本>] - <日期>` 那一段当 release 正文，应用内「设置 → 关于 → 更新日志」
   显示的就是它；找不到段非零退出，那一步直接红。平时把改动记在 `[Unreleased]` 下，
   发版那天改名成 `[<版本>] - <日期>` 并另开一个空 `[Unreleased]`。
2. **`THIRD-PARTY-NOTICES.txt` 签进仓库根**。`notices` job 用钉死的 cargo-about
   0.9.2 重新生成并与仓库里那份逐字节比对，不一致就红 —— 所以依赖变了要本地重跑
   一起提交：

   ```bash
   cargo install cargo-about --locked --version 0.9.2 --features cli
   cargo about generate --locked packaging/about.hbs > THIRD-PARTY-NOTICES.txt
   ```

   两个 flag 都不是装饰：不带 `--features cli` 是**装不上任何东西却退出码 0**，
   红的地方落在下一句 `no such command: about`；不钉版本，上游改了模板吃的结构体
   形状就会出现与依赖无关的假漂移。生成器撞到名单外的许可证会非零退出 —— 那是
   刻意的，先读条款再决定加不加进 `about.toml` 的 `accepted`。
3. **Android 签名的四个 secret**（Settings → Secrets and variables → Actions）：
   `FOLIO_KEYSTORE_B64`（`base64 -w0` 出来的一整行）、`FOLIO_KEYSTORE_PASSWORD`、
   `FOLIO_KEY_ALIAS`、`FOLIO_KEY_PASSWORD`。`preflight` 只测在不在，内容一个字不进
   日志。**换 keystore 等于所有已装用户卸载重装** —— 系统按签名判「是不是同一个
   应用」，所以正式那把要在仓库之外备份好。

### 版本号与 tag

- 三处必须一致：workspace 的 `version`（`Cargo.toml`）、
  `crates/folio-app/android/app/build.gradle.kts` 的 `versionName`、tag 名。
- **`versionCode` 每次发版 +1** —— 一致性校验查不出「忘了递增」，忘了的结果是
  系统安装器拒绝覆盖安装，症状长得像自动更新坏了。
- tag 的配方（jj 0.46 实测）：

  ```bash
  jj bookmark set main -r @        # 先把 main 挪到要发的那个 change
  git tag v0.1.2                   # 打 tag（colocated 仓里 jj 认得）
  jj tag list                      # 确认列出的就是那一个，不是顺手留下的第二个
  jj tag track v0.1.2              # 新 tag 不 track 推不动
  jj git push --dry-run            # 必须看到「tag: v0.1.2 [add to <sha>]」这一行
  jj git push                      # 真推 —— 触发发布，往公网发正式包
  ```

  `jj tag track` 那步不能省：新 tag 不 track 时 `--dry-run` 的「Changes to push」
  里**安静地只列 bookmark**，只在另一行报 `Refusing to create new remote tag` ——
  只扫那三行会以为 tag 已经跟着 main 出去了。这与 bookmark 首推要显式认领是同一类
  设计，不是 bug。

**最后那一步不可逆**：推 tag 就往公网发正式包，而发出去的 Android 包永远换不了
签名。`--dry-run` 与 `jj tag list` 不是仪式，是闸门。

### 各端要抄进 release 正文的限制

- macOS 未签名、未公证：首次打开要在「隐私与安全性」里允许一次。
- Windows 未经真机验证。
- Android 只出 `arm64-v8a`（API 26+ 的 64 位设备），且 APK 不带第三方许可文本。
- PDF / AZW4 / DjVu 里没有字的那一页选不出东西（扫描件没带 OCR）；「笔记」一屏是空状态。
- HarmonyOS 与 iOS 不在发布矩阵里。

## 提交：用 jj

仓库是 jj/git **colocated** 的（`.jj/` 与 `.git/` 并存），GitHub Actions 与
`cargo ndk` 不受影响。要点三条：

- **别在 shell 里用 `git commit` / `git checkout` / `git stash`** —— colocated
  之后 git 的 HEAD 是分离的，那些改动 jj 看不见。
- 推之前**显式把 `main` 挪到要推的 change 上**：`@` 前进不带 bookmark 一起走
  （`jj bookmark move main --to @`），dry-run 确认后再 `jj git push`。
- jj 会把**未被 ignore** 的文件都收进当前 change —— 本机 scratch 先进 `.gitignore`
  再落提交。
