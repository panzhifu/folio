# Folio 开发计划

基于 gpui-kit 的中文电子书阅读器，桌面（Wayland）与 Android 两端一份实现。支持哪些格式**以 provider 注册表为准**（`content/format/provider.rs` 的 `providers()`，现在 19 个），这份文档不抄第二张表。

## 文档索引

| 文件 | 内容 |
|---|---|
| [阅读管线.md](阅读管线.md) | 七级管线与三套坐标：三级前缀和、`LayoutKey` 一处构造、章的解析产物缓存、后端形状与四个函数、线程归属、**接线顺序与验收（计数断言）** |
| [导入管线.md](导入管线.md) | 导入七步：`ingest` / `index` 那道缝、`sha2` 当身份、去重统计旧文件、封面自哈希、`Source` trait 一条管线 |
| [基础设施.md](基础设施.md) | 作业（含关闭时排空队列）、配置三层 + provenance + Arc 换页 + 热/重启清单、错误三档、日志纪律、十一件没人管的事、**§七 实现台账（2026-10-12）** |
| [Locator.md](Locator.md) | 坐标系：存的五个字段含原文快照、不依赖排版的**全局字符进度**、按格式的 source_key、四级降级复用 `anchor::relocate` |
| [听书.md](听书.md) | 合成朗读后端（`tts` 0.26.3）：两端能力表、Android 三条硬约束（`JNI_OnLoad` / 死的 `onStop` / 500 ms 阻塞）、Linux 要 `speechd`、段而非句、进度落 `positions`、必须与可省 |
| [纯文本小说.md](纯文本小说.md) | `.txt` 那一族的三段边界（文本说了算 / 规则说了算 / 代码说了算）、四条否决门的固定顺序、真书教出来的三则修正、下一步四件事各带验收门、以及**明确不做**的清单 |

| [M5-refactor.md](M5-refactor.md) | 架构重构（KOReader 式模块化）+ 绘制层简化 + 引擎查询接口 |
| [M6-selection.md](M6-selection.md) | 选中一期：长按选词、拖动扩展、复制 |
| [M7-annotations.md](M7-annotations.md) | 批注二期设计：TextAnchor 落库、回显、侧栏 |
| [images-lazy.md](images-lazy.md) | 图片不落字节：学 KOReader 从容器按需读（images 表只剩元数据） |
| [text-out-of-db.md](text-out-of-db.md) | 正文出库：库里只存「怎么加载」（`chapters.locator`），打开时回容器取 |
| [background-pipeline.md](background-pipeline.md) | 后台任务管线（`task`）：可取消、可报进度的后台作业 |
| [import-trait.md](import-trait.md) | 导入管线：`Source` trait 一条抽象七种来源（选文件/批量/整机扫描/监听/Wi-Fi 传书/URL/WebDAV），落库在后台 |
| [layout-optimization.md](layout-optimization.md) | 排版引擎优化记录（Tier 1 + §7.1–§7.5：两阶段排版 / 块级缓存 / 词级断行等） |
| [layout-engine-refactoring.md](layout-engine-refactoring.md) | 排版引擎的重构方案（与上一份的优化记录分开放：那一份记量过的数，这一份记结构的去向） |
| [翻页轴向.md](翻页轴向.md) | 手势的轴由动画档决定（不再由手指定）+ 一笔手势只落一个动作 + 惯性尾巴归发起那一笔；含四条缺陷与判定表 |
| [目录与组件.md](目录与组件.md) | 目录重构：四条依赖方向 + 纵切统一四层（model/view/components/overlay）+ reader 26 文件逐行搬迁表 + 页面拆组件的三条自查 + 六次提交 |
| [目录架构.md](目录架构.md) | 上一份的**可执行版**：目标态完整目录树、依赖矩阵、`mod.rs` 骨架、按批次的 `mv`/`sed` 脚本与门禁命令 |
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
crates/folio-core/src/              纯 Rust，无 gpui
  content/                          内容与文档
    dom/  css/  layout 那一侧的输入
    document/                       统一文档接口：`Document` trait + `BookDocument` 门面
                                    （可重排 / 定版式两族都从这一个入口出去）
    format/                         provider 注册表 + 容器解析，**格式差异到这一层为止**
      provider.rs                   `DocumentProvider`（index / chapter_source / read_texts /
                                    image_bytes / render_page / fonts / entry_text）与注册表
      content.rs                    `Content`：可重排 `FlowDoc` / 定版式 `FixedDoc`
                                    （KOReader 的 has_pages 那一刀）
      zip.rs container.rs palmdb.rs 三种容器共用层（ZIP、按偏移直读的条目、PalmDB）
      comic_cache.rs                7z / RAR / tar 的磁盘解压缓存（CB7 / CBR / CBT）
      epub.rs fb2.rs html.rs htmlz.rs markdown.rs text.rs rtf.rs chm.rs mobi.rs
      pdf.rs azw4.rs docx.rs odt.rs pdb.rs cbz.rs cb7.rs cbr.rs cbt.rs djvu.rs
      novel/                        `.txt` 那一族：规则表（`assets/novel-rules.*.json`）+ 引擎
      charset.rs                    字节 → 文本那四档（BOM / 声明 / 探测 / UTF-8）
      css/ cover.rs thumb.rs svg.rs woff.rs xhtml.rs sniff.rs
    anchor/ hit/ annotation.rs scan.rs
  layout/                           排版引擎：行盒、分片、失效键与两级缓存
    key.rs                          失效键 `LayoutKey`：**一处构造，各级缓存共用**
    cache.rs                        整章 `LayoutCache` 与块级 `BlockCache`（改一段只重排一段）
  store/                            SQLite：书目、进度、批注、集合、阅读统计、定版式页表
  ingest/                           书怎么进来：`import`（Source + 七步管线）、`scan`、
                                    `watch`、`identity`（sha2 当身份）、`validate`
  net/                              出网与入网：WebDAV、Wi-Fi 传书（tokio 只许在 transfer/）
  infra/                            横切：配置、日志、路径、错误、作业、服务、量具、时间、预算
  search/                           书内全文搜索（现读现扫，零索引）
  group.rs position.rs time.rs      还没找到邻居的三个小模块（书架分组、阅读位置、日历换算）

crates/folio-app/src/ui/            gpui-kit 界面
  features/reader/                  阅读器（对应 KOReader 的 ReaderUI），按纵切分四层
    model/                          状态与判断：`session`（组装与命令入口）、`book`（章表 +
                                    排版会话）、`fixed_book`（定版式页表 + 当前页）、
                                    `chapter_layout`（一章的层叠 + 排版 + 分片）、
                                    `toc` / `toc_tree`、`position`、`selection` 那几条、
                                    `image_atlas`（图集回收）、`turn` / `paging` / `swipe`
    view/                           纯渲染：`body`（读状态 → 场景 + 输入翻译）、`scene`、
                                    `paint`、`regions` / `bands` / `reading_page`
    components/                     有交互的部件：`toc_panel`、`image_viewer`、`selection`、
                                    `share_card`、`note_editor`
    motion/                         翻页动画（仿真卷曲那一条）
  features/shelf|notes|stats|settings|welcome/   其余目的地
  components/ widgets/              跨屏共用的小件（发丝线、空状态、顶栏）
  foundation/                       图片、字体、动效这类没有界面的地基
  services/                         l10n、library（删书那几条路）、config
  shell/                            应用外壳：自适应导航 + 每目的地一条返回栈
  host/ platform/                   平台边界（`cfg(mobile)` 只许留在这两层）
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
| M8 | **已落地两条**：`writing-mode: vertical-rl` 竖排（逻辑横排 + 一次转置；拉丁侧转不支持，显式降级，见 `layout/geometry.rs` 与 `layout/tests/vertical.rs`）、脚注弹层（`reader/model/footnotes.rs` + 阅读页那张小纸片）。剩一条：**`<ruby>` 注音** —— 层叠与排版两侧都还没有这一族标签的位置 |
| M9 | PDF。**已落地**：纯 Rust 的 `pdf_oxide`（不是 `pdfium-render` —— 那条要 C，且只在 `table-ml` 那一档后面）栅格化一页、页缓存复用图集那一层（`reader::image_atlas`，24 MB 封顶 + 离场两帧就放）、书签上屏并能跳页、书目与首页封面、**那一页也点得开放大 / 分享 / 保存**（`PageSource::Render` 的页现在进整本书那一列，见 `fixed_book::build_image_sequence`）。剩下的是一刀：**文字层锚点**（`extract_words` 实测存在，交回 pt 的盒子、原点左下 y 朝上，而栅格原点在左上 —— 接进 `hit` / `anchor` 要翻 y），在那之前 PDF / AZW4 / DjVu 里选不出字，与漫画同档 |
| M10 | 打磨：全库搜索（书内那一条已落地：现读现扫零索引；FTS5 被数否决）、集合、快捷键、打包发布。**CBZ 早已落地，从这一行拿掉了** |
| 格式扩展 | **已落地**：DOCX / ODT、PDB / PalmDoc、AZW4（PalmDB 内嵌 PDF）、HTMLZ、CB7 / CBR / CBT（7z / RAR / tar 漫画包；CBR 走纯 Rust 的 `rars`，不绑 unrar 的 C++）、DjVu（纯 Rust 的 `djvu-rs`，不引进 DjVuLibre；bundled 与 indirect 两种容器形状都读，NAVM 书签 → 目录、`(metadata …)` → 书目）。三种只能顺序解压的容器共用一份磁盘缓存（`comic_cache`：导入改名时那一格跟着搬、删书时跟着删、每次解压后压回上限），也共用同一份书目来源 —— 包里那份 `ComicInfo.xml`（口径按 Komga 的生产级 DTO），以及那张 `folder.jpg` / `cover.jpg` 封面缓存（认成封面，不排进页表）。Word / ODT 的**列表与脚注**也落了：DOCX 走 `w:numPr` + `word/numbering.xml`（含 `w:lvlOverride`）与 `w:footnoteReference` + `word/footnotes.xml` / `endnotes.xml`，ODT 走 `text:list` + `text:list-style`（`style:num-format` 与 `text:bullet-char`）与 `text:note`；两族都归约成引擎已有的 `<ul>/<ol>/<li>` 与 `epub:type="noteref"` / `epub:type="footnote"` 那一对形状，脚注弹层不用改。<br>**后置**：PDF / AZW4 / DjVu 的文字层（`FixedDoc` 页内文本框与批注坐标）、加密 RAR（无密码入口）、`ComicInfo.xml` 的 `<Chapters>`（页号基数没有权威，故意不读）、indirect DjVu 的**真样本验证**（代码走 `open_dir`，本机没有一份真 indirect 文件可撞）、Word 的**样式间接编号**（两个生产者都把 `numPr` 写在段落上，所以没去 `word/styles.xml` 找那一层）、`w:numRestart` 与 ODT 的 `text:continue-numbering`（号一律按出现序数，「接上一串」的那种写法屏上会从 1 重来） |
| 真样本覆盖 | 拿**别人生产的文件**量过的：EPUB（calibre 导出）、CHM（7-Zip 打包）、MOBI / AZW3（libmobi 样本 11 本，含两本 `TEXt`/`REAd` 的真 PalmDOC）、TXT（Gutenberg 公版 8 本）、DjVu（djvu-rs 测试集 + 一份 71 页带 100 条书签的公版 spec）、RAR（rarlab 测试集）、PDF（合成，形状经 poppler 验）、**DOCX（python-docx 那四份 + docx-rs 测试集里带 `w:numPr` / `w:footnote` 的五份，共 9 份真 Word 文件**，量出表格里每个格子是一整段、编号定义在 `numbering.xml` 而引用在段落上、`separator` / `continuationSeparator` / `continuationNotice` 那三种格子占着与真注文同一个 id）、**ODT（odfjs 测试集 + webodf 的 welcome.odt，共 5 份**，量出 `text:bullet-char` 的 `•` `◦` `▪` `➢` 四层与 `style:num-format="A"` / `"1"`，以及 LibreOffice 真写出的空 `text:style-name`）、**PalmDOC（那两本**，量出三条：书名在数据库名那一格、正文可以是标记、加密方式那一格没人读）。真样本顺带量出一条**跨格式**的：`&amp;` 在 quick-xml 0.42 里是独立事件，四处只吃 `Event::Text` 的解析会静默丢掉那一个字符（EPUB 的 OPF / NCX / nav、DOCX、ODT、ComicInfo），收口在 `format::xml_piece`。还**只有合成夹具**的：ODT 的 `text:note`（手边五份 .odt 里没有任何一份带注）、HTMLZ、AZW4、CB7 / CBT、ComicInfo.xml、漫画那张 `folder.jpg` —— 这几条的门目前是「自己写的夹具刚好符合自己的解析」，撞到真文件形状之前别当成已验过 |
