# Folio 开发计划

基于 gpui-kit 的 EPUB / Markdown / TXT 阅读器，桌面（Wayland）与 Android 两端一份实现。

## 文档索引

| 文件 | 内容 |
|---|---|
| [M5-refactor.md](M5-refactor.md) | 架构重构（KOReader 式模块化）+ 绘制层简化 + 引擎查询接口 |
| [M6-selection.md](M6-selection.md) | 选中一期：长按选词、拖动扩展、复制 |
| [M7-annotations.md](M7-annotations.md) | 批注二期设计：TextAnchor 落库、回显、侧栏 |
| [images-lazy.md](images-lazy.md) | 图片不落字节：学 KOReader 从容器按需读（images 表只剩元数据） |
| [background-pipeline.md](background-pipeline.md) | 后台任务管线（`task`）+ 导入管线（`import`）：可取消、可报进度的后台作业 |
| [layout-optimization.md](layout-optimization.md) | 排版引擎优化记录（Tier 1 + §7.1–§7.5：两阶段排版 / 块级缓存 / 词级断行等） |
| [facts.md](facts.md) | 已钉死的事实：框架/平台/构建的实测约束（写代码前先读） |

## 架构

```
crates/folio-core/          纯 Rust，无 gpui
  src/document/             统一文档接口：Document trait（可重排 / 定版式）+ BookDocument 门面
  src/dom/                  html5ever arena DOM
  src/css/                  cssparser + selectors，层叠与 UA 表
  src/layout/               排版引擎：行盒 / 分页 / 双栏分片（fragment.rs）
  src/anchor.rs             TextMap（可见文本坐标系）/ TextAnchor / 四级降级 relocate
  src/annotation.rs         批注读侧：relocate 批量解析 / 按章分组 / Markdown 导出
  src/group.rs              书架分组（按作者 / 格式；集合见 store）
  src/time.rs               日历换算（Unix 秒 ↔ 本地日 ↔ YYYY-MM-DD）
  src/hit.rs                引擎查询接口：hit_test / rects_for_range / snap_word
  src/format/               provider 注册表 + 容器解析
    content.rs              Content：可重排 FlowDoc / 定版式 FixedDoc（KOReader 的 has_pages 那一刀）
    provider.rs             DocumentProvider trait + 注册表（认领 / 权重）
    zip.rs / cbz.rs         zip 容器共用层；CBZ（定版式，自然序页表）
    epub.rs / fb2.rs / html.rs / markdown.rs  可重排容器
  src/remote/               WebDAV：传输 trait + 协议规则（离线可测）
  src/task.rs               后台任务管线：可取消、可报进度的后台作业（thread + mpsc）
  src/import.rs             导入管线：拷进书库 + 解析，建在 task 上（落库留在调用方）
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

**三个钉死的架构决策**：

1. **自研排版引擎**（folio-core/layout）而非 gpui-kit `TextView`：分页、双栏分片、
   字偏移坐标系是阅读器的命根，`TextView` 的流式布局给不了。KOReader 同样保留 crengine。
2. **进度存字偏移**（`Locator { chapter, offset }`），页号/滚动只是读数 —— 对应 KOReader
   的 xpointer 双轨制。改字号、缩窗口、换模式都不动位置。
3. **选中是引擎查询**（hit.rs）而非 UI 计算：UI 层只画矩形、弹菜单，字形知识留在引擎里。
4. **统一文档接口 + 两种内容家族**（学 KOReader）：`Document` trait 是唯一的阅读入口，
   底下只分「可重排」（行由排版引擎按视口算出）与「定版式」（页是既定的）两族，
   格式差异全部收敛在 `format::provider` 的注册表里。不做「把所有格式预转成一种文件」。

## Roadmap（M8+ 摘要，启动时再展开成独立文件）

| 里程碑 | 内容 |
|---|---|
| M8 | 脚注弹层、`<ruby>` 注音、`writing-mode: vertical-rl` 竖排（拉丁侧转不支持，显式降级） |
| M9 | PDF（pdfium-render 栅格 + 文字层锚点、LRU 页缓存） |
| M10 | 打磨：FTS5 搜索、集合、CBZ、快捷键、打包发布 |
