[English](../README.md) | 中文

<p align="center"><img src="../packaging/icons/folio.png" width="112" alt="Folio 图标"></p>

<h1 align="center">Folio</h1>

<p align="center">
  <strong>用 Rust 写的原生 GPU 阅读器，覆盖桌面与 Android。</strong><br>
  不嵌浏览器，不用 WebView —— 每一页都由 Folio 自己的排版引擎排出来。
</p>

---

Folio 在原生的 GPU 窗口里打开你的书，正文由自己排：`folio-core` 里一条
DOM → CSS 子集 → 排版的流水线，踩在 GPUI 的文本原语（shaping、断行、逐字形绘制）上，
应用壳来自 [gpui-kit](https://gpui-kit.com)。排版自己做，才有出版书要的那个样子 ——
首行缩进、居中章题、局部字重、按 `width` / `max-width` 缩放的插图、脚注、竖排 ——
而不必塞进一个浏览器。

## 亮点

- **懂书的引擎** —— 可重排与定版式两种文档共用同一套锚点；阅读位置记的是章内字符
  偏移而不是页码，所以改字号、拖窗口之后还能回到原处。
- **21 种格式、30 个扩展名** —— 见下面那张表。
- **三种阅读模式** —— 连续滚动、分页、双页对开（窗口不够宽时自动退回单页）；
  定版式的书走逐页模式，能放大、能分享、能保存。
- **记得住的书架** —— 书架分组、每本书的进度、阅读统计，以及书内全文搜索。
- **9 种界面语言** —— 简体中文 · 繁體中文 · English · 日本語 · 한국어 ·
  Français · Deutsch · Español · Русский.
- **四端小体积原生安装包** —— Linux、macOS、Windows、Android。

## 支持的格式

| 状态 | 格式 |
|---|---|
| 可重排，完整可读 | EPUB（含定版式）、MOBI / AZW / AZW3 / PRC（KF7 + KF8，含 hybrid）、FB2、TXT、Markdown、HTML / MHTML / HTMLZ、RTF、DOCX、老 DOC、ODT、PDB / PalmDoc、CHM |
| 定版式，逐页 | PDF、AZW4（PalmDB 内嵌 PDF）、DjVu（纯 Rust 解码，两种容器形状都读；NAVM 书签上屏当目录）、CBZ / CB7 / CBR / CBT（CBR 走纯 Rust 的 RAR 读取） |
| 部分支持 | PDF / AZW4 / DjVu **暂无文字层** —— 里面不能选中、不能批注、搜不到。Word 靠样式间接触发的编号与每节从头再数的编号、ODT 的 `text:continue-numbering` 暂不跟随 |
| 不支持 | 加 DRM 的书（DRM 标记的 PDB 会认出来并报告，不解密）、加密 RAR |

几点说明：

- 漫画：包里的 `ComicInfo.xml`（ComicRack / CDisplayEx / Calibre 写的那份）会填进
  系列与期号、制作人员、简介；包里的 `folder.jpg` / `cover.jpg` 认成**封面**，
  不会凭空多出一页。
- Word / ODT：列表编号按规范走，脚注尾注落成可点的上标，注体摆回引用它的那一章。

## 下载

安装包发布在 [Releases](https://github.com/panzhifu/folio/releases) 页，
每个 tag 一套。应用内也能检查更新（设置 → 关于），匿名访问 —— 不用账号，不用 token。

| 平台 | 安装包 | 说明 |
|---|---|---|
| Linux x86_64 | `.tar.xz` · `.deb` · `.rpm` | 需要可用的 Vulkan 驱动；Wayland 与 X11 都验证过 |
| macOS Apple Silicon | `.zip`（内含 `Folio.app`） | macOS 11+；未签名 —— 首次打开要在「隐私与安全性」里允许一次 |
| Windows x64 | NSIS 安装器或便携版 `.zip` | CI 里编出来；还没在真机上演过 |
| Android | `arm64-v8a` `.apk` | Android 8.0（API 26）以上，64 位设备 |

## 已知限制

- macOS 包未签名、未公证（没有开发者证书）。
- Windows 包还没在任何一台真 Windows 机器上装过 —— 第一次装请当成一次测试。
- Android 只出 `arm64-v8a`，且 APK 不带第三方许可文本（桌面三端的包都带）。
- PDF / AZW4 / DjVu 没有文字层（见格式表）。
- 「笔记」一屏是有意的空状态：批注后端与划线写入口已落地，界面还没接。
- HarmonyOS 与 iOS 的宿主工程在树里，但不在发布矩阵里。

## 计划中

方向，不是承诺：

1. **批注与笔记的界面** —— 把已有的后端接上屏。
2. **PDF / AZW4 / DjVu 的文字层** —— 定版式文档里的选中、批注与搜索。
3. **听书（TTS）** —— Linux 与 Android 的后端依赖已经就位，引擎本身还没写。
4. **iOS 与 HarmonyOS 宿主**，等它们进发布矩阵。

## 从源码构建

```bash
cargo run                      # 桌面窗口（Linux / macOS / Windows）
cargo test --workspace --lib   # 日常这条，快而且是全集
```

Linux 需要 fontconfig、Wayland、xkbcommon 的开发包与可用的 Vulkan 驱动。
Android 交叉编译、打包、日志配置与发版流程写在开发者手册里：
**[doc/development.md](./development.md)**。

## 底下是什么样子

```
crates/folio-core/   引擎，零 GPUI 依赖：DOM、CSS 子集、排版、锚点与批注、
                     21 个格式 provider、SQLite（书库/进度/批注/统计/页表）、日志

crates/folio-app/    应用壳：自适应导航、阅读、书架、设置、统计；
                     桌面与 Android 共用同一条渲染路径 —— 只有平台宿主层各是各的
```

两条守住这条边界的规矩：

- `folio-core` 永远不依赖 GPUI。所以它能被 `cargo test` 全量覆盖，
  也能在 Android 宿主里原样复用。
- 锚点是**章内字符偏移**，不是页码。字号或窗口一变页码就死，偏移不会。

## 许可

MIT。
