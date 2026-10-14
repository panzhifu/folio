# Folio 开发计划

基于 gpui-kit 的 EPUB / Markdown / TXT 阅读器，桌面（Wayland）与 Android 两端一份实现。

## 文档索引

| 文件 | 内容 |
|---|---|
| [阅读管线.md](阅读管线.md) | 七级管线与三套坐标：三级前缀和、`LayoutKey` 一处构造、章的解析产物缓存、后端形状与四个函数、线程归属、**接线顺序与验收（计数断言）** |
| [导入管线.md](导入管线.md) | 导入七步：`ingest` / `index` 那道缝、`sha2` 当身份、去重统计旧文件、封面自哈希、`Source` trait 一条管线 |
| [基础设施.md](基础设施.md) | 作业（含关闭时排空队列）、配置三层 + provenance + Arc 换页 + 热/重启清单、错误三档、日志纪律、十一件没人管的事、**§七 实现台账（2026-10-12）** |
| [Locator.md](Locator.md) | 坐标系：存的五个字段含原文快照、不依赖排版的**全局字符进度**、按格式的 source_key、四级降级复用 `anchor::relocate` |
| [M5-refactor.md](M5-refactor.md) | 架构重构（KOReader 式模块化）+ 绘制层简化 + 引擎查询接口 |
| [M6-selection.md](M6-selection.md) | 选中一期：长按选词、拖动扩展、复制 |
| [M7-annotations.md](M7-annotations.md) | 批注二期设计：TextAnchor 落库、回显、侧栏 |
| [images-lazy.md](images-lazy.md) | 图片不落字节：学 KOReader 从容器按需读（images 表只剩元数据） |
| [text-out-of-db.md](text-out-of-db.md) | 正文出库：库里只存「怎么加载」（`chapters.locator`），打开时回容器取 |
| [background-pipeline.md](background-pipeline.md) | 后台任务管线（`task`）：可取消、可报进度的后台作业 |
| [import-trait.md](import-trait.md) | 导入管线：`Source` trait 一条抽象七种来源（选文件/批量/整机扫描/监听/Wi-Fi 传书/URL/WebDAV），落库在后台 |
| [layout-optimization.md](layout-optimization.md) | 排版引擎优化记录（Tier 1 + §7.1–§7.5：两阶段排版 / 块级缓存 / 词级断行等） |
| [facts.md](facts.md) | 已钉死的事实：框架/平台/构建的实测约束（写代码前先读） |

## 架构

**分区（2026-10-12）**：`folio-core/src/` 从 22 个平铺模块收成六个区，
判据是「谁依赖谁」——`content` 可依赖 `infra`，但不依赖 `store` / `ingest`；
`infra` 谁都不依赖。这条方向是「引擎能在没有数据库的情况下跑」与
「core 能被 `cargo test` 全量覆盖」的前提，所以每个区的 `mod.rs` 里都写了一遍。

```
crates/folio-core/src/
  content/                 内容与文档：DOM / CSS / 文档接口 / 锚点 / 命中 / 批注
    document/  format/     （format 里含容器层：zip / container / cover / thumb，与 css/）
    dom/  anchor.rs  hit/  annotation.rs
  layout/                  排版引擎：行盒、分片、失效键与两级缓存
  store/                   SQLite：书目、进度、批注、集合、统计
  ingest/                  书怎么进来：导入管线、扫描、监听、身份、校验
  net/                     出网与入网：WebDAV、Wi-Fi 传书（tokio 只许在 transfer/）
  infra/                   横切：配置、日志、路径、错误、作业、服务、量具、时间
  group.rs  position.rs    两个还没找到邻居的小模块（书架分组、阅读位置）
```

文案表仍在 **crate 根**的 `locales/`，不在 `infra/` 里：`rust_i18n!` 的路径是相对
`CARGO_MANIFEST_DIR` 解，不是相对源文件 —— 搬模块时它不该跟着动。

```
crates/folio-core/          纯 Rust，无 gpui
  src/document/             统一文档接口：Document trait（可重排 / 定版式）+ BookDocument 门面
  src/dom/                  html5ever arena DOM
  src/css/                  cssparser + selectors，层叠与 UA 表
  src/layout/               排版引擎：行盒 / 分页 / 双栏分片（fragment.rs）
    key.rs                  失效键 `LayoutKey`：**一处构造，各级缓存共用**
    cache.rs                整章 `LayoutCache` 与块级 `BlockCache`（改一段只重排一段）
  src/anchor.rs             TextMap（可见文本坐标系）/ TextAnchor / 四级降级 relocate
  src/annotation.rs         批注读侧：relocate 批量解析 / 按章分组 / Markdown 导出
  src/group.rs              书架分组（按作者 / 格式；集合见 store）
  src/time.rs               日历换算（Unix 秒 ↔ 本地日 ↔ YYYY-MM-DD）
  src/hit.rs                引擎查询接口：hit_test / rects_for_range / snap_word
  src/format/               provider 注册表 + 容器解析
    content.rs              Content：可重排 FlowDoc / 定版式 FixedDoc（KOReader 的 has_pages 那一刀）
    provider.rs             DocumentProvider trait + 注册表（认领 / 权重）
    zip.rs / cbz.rs         zip 容器共用层；CBZ（定版式，自然序页表）
    epub.rs / fb2.rs / html.rs / markdown.rs / rtf.rs  可重排容器
  src/remote/               WebDAV + 传输 trait（同步签名）：dav 协议规则离线可测，ureq 是同步实现
  src/task.rs               后台任务管线：可取消、可报进度的后台作业（thread + mpsc）
  src/import.rs             导入管线：Source trait + 逐本「拷 → 解析 → 落库」（后台完成）
  src/scan.rs               整机扫描：找书不导入，候选清单勾选再走管线
  src/watch.rs              文件夹监听：inotify 事件攒批自动入库（notify）
  src/transfer/             Wi-Fi 传书：HTTP 服务器收上传（tokio/axum 只住这里）
  src/store.rs              SQLite：书目、进度、批注、集合、阅读统计、定版式页表（图片只存元数据）

crates/folio-app/src/ui/    gpui-kit 界面
  reader/                   阅读器（对应 KOReader ReaderUI）
    session.rs              ReaderSession：组装模块、命令入口、跨模块协调
    modules/                功能模块：navigation / position / toc / selection / annotations
    book.rs                 章节表、目录、Document 会话
    document.rs             一章的排版状态（层叠 + 排版 + 分片 + 坐标映射）
    view.rs                 纯渲染：读 session 状态 → scene；输入翻译
    scene.rs / paint.rs     排版结果 → 绘制单元 → canvas + ShapedLine::paint
    chrome.rs               点击分区、面板档位（纯函数）
  shell/ shelf/ settings/   导航外壳、书架、设置
  shared/                   跨屏共用：theme / l10n / config_model / book_fonts / images
```

**模块化原则**（KOReader registerModule 的 Rust 化）：每个 module 是普通 struct，由
`ReaderSession` 持有并转发命令；跨屏事件用 gpui 的 `cx.emit/subscribe`（它就是事件总线，
不另造）。`view.rs` 只读状态、翻译输入，不写业务状态。

**五个钉死的架构决策**：

1. **自研排版引擎**（folio-core/layout）而非 gpui-kit `TextView`：分页、双栏分片、
   字偏移坐标系是阅读器的命根，`TextView` 的流式布局给不了。KOReader 同样保留 crengine。
2. **进度存字偏移**（`Locator { chapter, offset }`），页号/滚动只是读数 —— 对应 KOReader
   的 xpointer 双轨制。改字号、缩窗口、换模式都不动位置。
3. **选中是引擎查询**（hit.rs）而非 UI 计算：UI 层只画矩形、弹菜单，字形知识留在引擎里。
4. **统一文档接口 + 两种内容家族**（学 KOReader）：`Document` trait 是唯一的阅读入口，
   底下只分「可重排」（行由排版引擎按视口算出）与「定版式」（页是既定的）两族，
   格式差异全部收敛在 `format::provider` 的注册表里。不做「把所有格式预转成一种文件」。
5. **失效键只有一把**（`layout::LayoutKey`）：排版结果、块级缓存、切片、批注解算缓存
   都问同一把键，构造点只有 `Document::layout_key` 一处。理由不是性能而是防漂 ——
   同一个「变了没有」在两处各答一遍，两处必然漂，症状是「改了设置版式不动」或
   「缓存看起来命中、屏上却是旧行盒」。（接线前这里有三套手搓键：`generation`、
   `applied` 的元组、`fragmented` 的五元组。）

## Roadmap（M8+ 摘要，启动时再展开成独立文件）

| 里程碑 | 内容 |
|---|---|
| M8 | 脚注弹层、`<ruby>` 注音、`writing-mode: vertical-rl` 竖排（拉丁侧转不支持，显式降级） |
| M9 | PDF（pdfium-render 栅格 + 文字层锚点、LRU 页缓存） |
| M10 | 打磨：FTS5 搜索、集合、CBZ、快捷键、打包发布 |
