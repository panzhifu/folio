# 打包

四端安装包里**不是代码**的那几样东西住在这儿：图标、`.desktop` 条目、deb/rpm 的元数据、
Windows 安装器的脚本。构建它们的那条 workflow 在 `.github/workflows/release.yml`。

## 图标：主图只有一张

主图是 `crates/folio-app/ohos/AppScope/resources/base/media/app_icon.png`（192×192 RGBA，
绿色圆角底 + 一本打开的书 + 一枚金色书签）。它原本是 HarmonyOS 宿主用的那份，**但它是仓库里
唯一一张完整的应用图标位图**，所以这里当主图用。

`icons/folio.png` 是它的一份**拷贝**（不是软链：CI 里 checkout 出来的软链要 `git config core.symlinks`
配合才可靠）。**改图标要改主图那一份，然后重跑下面两条命令**，别只改 `icons/` 里的拷贝 ——
那会出现「Android 与 ohos 上是新图、桌面安装包里还是旧图」这种没人会立刻发现的漂移。

```bash
SRC=crates/folio-app/ohos/AppScope/resources/base/media/app_icon.png
cp "$SRC" packaging/icons/folio.png
magick "$SRC" -define icon:auto-resize=16,24,32,48,64,128,256 packaging/icons/folio.ico
```

`folio.ico` 是签进来的**派生物**，不是第二真相源：Windows 的 NSIS 在构建期不保证有 ImageMagick，
而安装器与卸载表的图标必须在打包那一刻就在盘上。再生成命令写在上面，改主图时一起跑。

## 各端拿走了什么

| 端 | 用哪几样 | 落在包里哪儿 |
|---|---|---|
| Linux `.tar.xz` | `folio.desktop` + `icons/folio.png` | `share/applications/`、`share/icons/hicolor/192x192/apps/folio.png` |
| Linux `.deb` / `.rpm` | 同上，加 `control.template` / `folio.spec` | 同上；`/usr/bin/folio` 是二进制 |
| Windows | `icons/folio.ico` + `windows/folio.nsi` | exe 的图标资源、安装器与卸载表 |
| macOS | `icons/folio.png` | `Folio.app/Contents/Resources/folio.png` |
| Android | **不用这里任何一样** —— 自适应图标是 `crates/folio-app/android/app/src/main/res/` 那几份 vector XML | — |

## 版本号从哪儿来

这些文件里出现的 `@VERSION@` 都由 workflow 在打包那一刻替换（`sed` 一遍），**不手写第二份版本号**。
唯一的真相是根 `Cargo.toml` 的 workspace `version`；`android/app/build.gradle.kts` 的
`versionName` 与 tag 由 workflow 校验与它一致，而 `versionCode` 没人管 —— 那条单独记在
README「发版」那一节。
