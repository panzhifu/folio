# 五款上游阅读器对比 Folio：功能差距

给「借阅上游」用的对比盘点（早期两篇 readest vs anx-reader、okular vs foliate 已并入
本文）。对比轴是**各款成熟阅读器与 Folio 的差距**：它们有什么我们没有的、我们有什么
它们没有的、值得借谁。全文以表格为主 —— 功能矩阵看现状，路线图表看「照谁做」；
补充 Folio 新功能时在矩阵里加一行、六列照实填即可。

对象与快照（都是只读克隆，登记见 [`../reference/README.md`](../reference/README.md)）：

| 软件 | 版本 / 快照 | 技术栈 |
|---|---|---|
| Okular | 26.11.70 @ `896b2fe` | C++/Qt，KDE |
| Foliate | 3.3.0 @ `67b6676` | GJS/GTK4 + WebKitGTK（foliate-js 母项目） |
| Readest | 0.12.16 @ `2d37bbfc2` | Tauri v2 + React（foliate-js） |
| Anx Reader | 1.15.0 @ `0c7fc1ad` | Flutter + WebView（foliate-js） |
| KOReader | HEAD `0deae50` @ 2026-10-09 | Lua 前端 + C++ crengine，无 WebView |
| **Folio（本仓）** | 工作副本 | Rust + gpui-kit 原生 GPU，自研排版引擎 |

方法：六边都以**代码为准**、README 只当辅证，关键论断落到文件（集中在文末
[值得借的清单](#值得借的清单按优先级)的「上游参考」列）。Folio 自身的盘点同样是只读
扫描本仓得出的（也照实记下自己的短处）。写作日期 2026-10-26；**2026-10-28 对着代码把
Folio 这一列复扫了一遍**，动了的格子：版式可调（补定版式的显示缩放）、双页（补绕书脊揭
半张 leaf 与中缝只留白）、批注列表屏（笔记屏带搜索）、扫描导入（逐本开容器的范围）、
阅读统计（早就不止热力图了）、同步（每本书的阅读设置已随快照走），文末清单跟着改。

图例：✅ 有 · ⚠️ 部分 / 有差距 · ❌ 没有 · — 文中未盘点

## 结论

| 判定 | 内容 |
|---|---|
| 总体 | **阅读内核已站进第一梯队，差距集中在外向生态。** |
| 领先 / 独有 | 格式矩阵最宽（21 provider 全纯 Rust）；自研排版引擎、零 WebView；双页自动退档、仿真卷页、Wi-Fi 传书（局域网 + PIN）、字符偏移 Locator（换字号换窗口不漂移）、**听读一体**（五家里只有 Folio 与 Readest 有朗读，且只有 Folio 把听书计入统计） |
| 差距（按补齐划算程度） | ① **批注闭环** —— 列表屏（书内「批注」格 + 跨书「笔记」屏，后者一屏一本一张卡、可搜）、导出（txt/MD/HTML）、书签、删除撤销**已补齐**；剩批注导入互通；② **同步** —— WebDAV 手动版已接（进度 / 批注 / 书签 + 每本书的阅读设置）；书文件与自动同步未做；③ **OPDS 书源** —— 1.x Atom + 2.0 JSON 的浏览与下载已接，搜索 / OPDS-PSE 未做；④ **TTS** 补平台与句级；⑤ **AI 与词典** —— 选区翻译弹窗（Google 免 key / OpenAI 兼容 / DeepL 三档）**第 2 期已补**，本地词典与 AI 是战略级选择，暂缓不动摇内核；⑥ **跨书全文搜索** —— 书内那一趟已落地（现读现扫、零索引），全库那一趟还没接 |
| 不追 | Okular 的表单 / 签名 / 打印 / 演示模式是文档查看器的物种能力；KOReader 的 Kindle/Kobo/ReMarkable 那一串墨水屏宿主是它物种的一部分 —— 都与 Folio 的定位无关，不进 roadmap |

## 功能矩阵

### 格式与内核

| 功能 | **Folio** | Okular | Foliate | Readest | Anx Reader | KOReader |
|---|---|---|---|---|---|---|
| 格式数量 | **21 provider**（测试钉住 30 个扩展名） | 14 后端（PDF 系为主） | 6 种 | 14 种 | 白名单 6 种 | 最宽一族：crengine 30+ 扩展名 + PDF/DjVu/图片 |
| DOCX / ODT / RTF / CHM | ✅ | ❌ | ❌ | ❌ | ❌ | ✅（crengine） |
| HTMLZ / MHTML | ✅ | ❌ | ❌ | ❌ | ❌ | ⚠️ HTMLZ 有，MHTML 无 |
| DjVu / AZW4 | ✅ | ❌ 或靠转换 | ❌ 或靠转换 | ❌ 或靠转换 | ❌ 或靠转换 | ⚠️ DjVu 有，AZW4 无 |
| MD / FB2 / FB3 / PDB | ✅ MD / FB2 / PDB（各有 provider 与测试）；FB3 ❌ | ❌ | ❌ | ❌ | ❌ | ✅ |
| 压缩容器（ZIP / RAR / 7z / tar） | ✅ 纯 Rust 解码，加密 RAR 明确不支持 | — | — | — | — | ✅ zip 兜底 + cbz/cbr/cbt |
| 定版式带文字层（选中 / 批注 / 搜索） | ✅ PDF / AZW4 / DjVu / 固定 EPUB / 漫画；无字页诚实退化 | — | — | — | — | ✅ PDF 高亮可写回注释 |
| 排版内核 | **自研 DOM→CSS 子集→排版**，零 WebView | Qt 原生（QTextDocument 重排） | WebKitGTK + foliate-js | WebView + foliate-js | WebView + foliate-js | **C++ crengine 自研**，无 WebView；PDF 走 mupdf + kopt 重排 |
| 定位系统 | **字符偏移 Locator**（换字号 / 换窗口不漂移） | 页号（换字号就漂） | CFI | CFI | CFI | xpointer（HEAD 起并存 EPubCFI） |

> 内核路线的分野：无 WebView 阵营只有 Folio 与 KOReader 两家（自研排版 vs crengine），
> Okular 是 Qt 原生但重排走 QTextDocument。代价是每个格式都要自己写适配（已写 21 个），
> 收益是渲染节奏完全在自己手里 —— 仿真翻页、逐帧捕获、手势跟手都长在这条路上。

### 版式、主题与摆法

| 功能 | **Folio** | Okular | Foliate | Readest | Anx Reader | KOReader |
|---|---|---|---|---|---|---|
| 版式可调 | ✅ 字号 / 行距 / 段距 / 缩进 / 字距 / 字重 / 文字方向 / 两端对齐 / 版心 / 左右边距 / 上下边距（默认跟书 `@page`）/ 字体（设备已注册家族下拉）；定版式那一族另有一档**显示缩放**（双指 1.0–4.0，源图没那么多像素就按源图宽封顶，翻页与换档自动回铺满） | ⚠️ 仅全局字体 | ⚠️ 字体 / 行高 / 连字 / 边距 / CSS | ✅（另加 12 个 transformer） | ✅（另加 Bionic Reading） | ✅ **最细**：字重 / 字距 / CJK 缩放 / 断词词典 / DPI / Gamma / 提示（`creoptions.lua` 全表） |
| 简繁转换等 transformer | ❌ | ❌ | ❌ | ✅ 12 个 | ❌ | — |
| Bionic Reading | ❌ | ❌ | ❌ | ❌ | ✅ | — |
| 主题 | 明暗三态 + 用户 JSON 主题目录 | 9 种色彩滤镜（无纸色主题） | 9 套 + 自定义 | 7 套 × 明暗 + 自定义 | 自定义色 + 背景图 | ⚠️ 夜间反色一档 + 定时切换（无多档主题系统） |
| 连续滚动 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 分页 | ✅ | ✅ 单页 / 概览 | ✅ | ✅ | ✅（自适应） | ✅（含页重叠） |
| 双页 / 对开 | ✅ 宽度不够自动退档；翻一趟绕书脊揭**靠手指那半张 leaf**（仿真卷页的纸越过中缝落在另一半上，背面就是新跨页那一栏），中缝只留白、不画阴影 | ✅ 对开 | ⚠️ 多列流式 | ✅ 双栏 | ❌ | ✅ crengine 双栏（仅 page 模式）；PDF 走 kopt 切割 |
| 翻页动画 | **6 档**：Slide / SlideUp / Cover / Curl 仿真 / Scroll / None | ❌ 无仿真 | ❌ 无仿真 | push / slide / curl | ❌ 无仿真 | ⚠️ 仅 swipe 过渡（设备相关），无仿真 |

### 批注 —— 闭环已补齐

| 功能 | **Folio** | Okular | Foliate | Readest | Anx Reader | KOReader |
|---|---|---|---|---|---|---|
| 高亮 / 图形样式 | ✅ 5 色 × 3 画法（色带 / 实线 / 波浪） | ✅ 13 类图形工具（手绘 / 图章 / 形状，定位外） | ✅（CFI） | ✅ 3 样式 + 手写 | ✅ 2 样式 | ✅ 9 色 + 自定义 × 4 画法（色带 / 下划线 / 删除线 / 反色） |
| 存储与锚点 | ✅ SQLite，锚点是格式版本化 JSON（v13 迁移防 reindex 级联删） | ✅ 写回 PDF / sidecar XML | ✅ CFI 明文 JSON | ✅ | ✅ CFI | ✅ 书旁 `.sdr/metadata.lua` sidecar；批注 / 笔记 / 书签统一一份 annotations 模型 |
| 选中交互 | ✅ 拖选 + 长按吸词 + 把手 | — | — | — | — | ✅ 长按选词 |
| 就地笔记卡 | ✅ | — | — | — | — | ✅ Add note（可存 html / md） |
| 批注列表屏 | ✅ 目录面板「批注」格（书内）+「笔记」目的地（跨书，一屏一本一张卡，卡面走 `.thumbs/96/` 缩略图；可搜：书名 / 引文 / 笔记 / 出处章名，候选侧折小写） | ✅ 按类型 / 按页过滤 | ✅ | ✅ | ✅ | ✅ booklist（⚠️ 无按类型 / 页过滤） |
| 批注导出 | ✅ txt / Markdown / HTML 三档 | ✅ 写回 PDF | ✅ 4 格式 | ✅ Readwise / Notion | ✅ 4 格式 | ✅ **9 目标**（HTML / MD / txt / JSON / Joplin / Readwise / Nextcloud / myClippings / xmnote） |
| 批注导入（带着笔记搬家） | ❌ | — | — | ✅ Moon+ Reader / ReadEra | — | ✅ `.annotations.lua` 跨设备合并 |
| 用户书签 | ✅ 顶栏开关 + 列表（可重排锚整行、定版式锚页号） | ✅ | ✅ | ✅ | ✅ | ✅ 与高亮统一模型 |
| 删除撤销 | ✅ 6 秒撤销胶囊（会话级，按原行插回） | ✅ 命令式撤销层 | ✅ | ✅ | ✅ | ❌ 删了就是删了 |
| 摘录 / 引文分享卡 | ✅ 移动端引文分享卡 | ❌ | — | — | ✅ 摘录分享卡 | — |

### 朗读（TTS）

| 功能 | **Folio** | Okular | Foliate | Readest | Anx Reader | KOReader |
|---|---|---|---|---|---|---|
| 引擎 / 后端 | ⚠️ Linux（Speech Dispatcher）+ Android（JNI），macOS / Windows 入口隐藏 | ✅ Qt TTS | ✅ | ✅ 5 引擎（含云端音色） | ✅ 4 引擎 | ❌ **无内置 TTS**（靠第三方插件，上游 issue 确认） |
| 朗读高亮 | ⚠️ 段级 utterance | ❌ 无高亮 | ✅ 句级（SSIP mark 事件） | — | — | ❌ |
| EPUB 3 Media Overlays（旁白） | ❌ | ❌ | ✅ | — | — | ❌ |
| 有声书配对 | ❌ | ❌ | ❌ | ✅ | ❌ | ❌ |
| 后台播放 + 媒体通知 | — | — | — | — | ✅ | ❌ |
| 听读一体：听书计入统计 + 自动跟读 | ✅ 独有 | ❌ | ❌ | ❌ | ❌ | ❌ |

### 词典、翻译与 AI

| 功能 | **Folio** | Okular | Foliate | Readest | Anx Reader | KOReader |
|---|---|---|---|---|---|---|
| 本地词典 | ❌ | ❌ | ❌ | ✅（词典生态） | ❌ | ✅ Stardict + sdcv |
| 选区翻译弹窗 | ✅ 三引擎可配 + SQLite 缓存 + 弹层内换语言 | ❌ | ✅ | ✅ 多引擎可配 | ❌ | ✅ 翻译仅 Google 一个引擎 |
| 翻译引擎可配 / 自定义 | ✅ Google 免 key、OpenAI 兼容（自填地址 / 模型 / Key）、DeepL；缓存键带引擎与模型指纹 | ❌ | ❌ | ✅ 4 内置 + 自定义 OpenAI 兼容 | ✅ 6 种 provider | ⚠️ 服务器可换，仅 Google 系 |
| AI 陪读（防剧透 RAG，只许讨论已读页码） | ❌ | ❌ | ❌ | ✅ | ❌ | ❌ |
| AI 多 provider + agent 工具 | ❌ | ❌ | ❌ | ❌ | ✅ | ❌ |
| 全书双语翻译 | ❌ | ❌ | ❌ | ❌ | ✅ | ❌ |
| 维基百科 | ❌ | ❌ | ✅ | — | — | ✅ 词条可存成 EPUB |

### 书源与书库

| 功能 | **Folio** | Okular | Foliate | Readest | Anx Reader | KOReader |
|---|---|---|---|---|---|---|
| OPDS 书源 | ✅ 1.x Atom + 2.0 JSON；浏览 / 封面 / 下载入书库 / 翻页；Basic 认证；无搜索与 PSE | ❌ | ✅ 1.x + 2.0 | ✅ 全家桶 + OPDS-PSE（流式阅读） | ❌ | ✅ 1.x + 2.0 + OPDS-PSE |
| 网页剪藏 | ❌ | ❌ | ❌ | ✅ | ❌ | ✅ Wallabag |
| RSS / 新闻下载 | ❌ | ❌ | ❌ | — | ❌ | ✅ feed → EPUB |
| Calibre 无线 | ❌ | ❌ | ❌ | — | ❌ | ✅ 元数据搜索 + 无线传书 |
| 局域网传书 | ✅ Wi-Fi 浏览器 + 四位 PIN | ❌ | ❌ | ⚠️ LocalSend 勉强同类 | ❌ | ⚠️ 走 Calibre 服务端 |
| 收件箱目录监听 | ✅ inotify | — | — | — | — | — |
| 扫描导入 + 判重 | ✅ 整机扫描勾选导入 + content_hash 判重；架面那一趟**逐本**开容器只为了还没入库的（`shelf: Option<i64>` 分了在架与否，在架的读库不重开文件；没入库的顺序开、一次一本 `ParseBudget::AtMost(1)`、`Priority::Prewarm`），卡片封面走 `<书库>/.thumbs/96/`（按封面字节自己哈希命名，取不到回原图） | ❌ 无书库 | ✅ 书架 | ✅ 智能书架 | ✅ | ⚠️ 文件管理器制（无库级判重） |
| 分组 / 标签 / 评分 | ✅ 分组 | ❌ | ✅ | ✅ | ✅ 标签 / 分组 / 评分 | ✅ collections / favorites |
| 元数据编辑 | ✅ | — | — | — | — | ✅ 书详情可改 + 评论 |
| 跨书搜索 | ⚠️ 书内全文搜索已有（`folio-core/src/search.rs`，现读现扫零索引）；跨书未接 —— 清单第 7 条 | — | — | ✅ | — | ⚠️ 文件名搜索 |
| 格式转换 | ❌ | — | ❌ | ❌ | ❌ | ✅ 内置 converter |

### 阅读统计

| 功能 | **Folio** | Okular | Foliate | Readest | Anx Reader | KOReader |
|---|---|---|---|---|---|---|
| 年热力图 | ✅ 年档一整屏 | ❌ | ✅ | ❌（逐页数据无 UI） | ✅ | ❌（月历视图 + 每日直方图） |
| 连续天数 / 连击 | ✅ 最长连续天数 | ❌ | ✅ 连击 | ❌ | ✅ 连击 | ❌ |
| 其他图表 | ✅ **区间报告**：总 / 日 / 周 / 月 / 年五档可切、可前后翻页；周历与月历、小时分布、单次时长直方（五档：`<5m / 5–15m / 15–30m / 30–60m / >60m`）、时段四桶、最值与连续、书目排行。一次 `reading_history()` 读库出全部聚合，界面按区间切片（2026-10-27 把原先只有年热力图那一屏换掉了；`chars` 口径未定，所以字数那一格界面仍然不显示） | ❌ | ✅ 瓦片 | ❌ | ✅ 图表 | ✅ 月历 + 直方图 + 单书统计 |
| 结账粒度 | ✅ 5 分钟段 + 空闲宽限防挂机 | ❌ | — | — | — | — |
| 听书计入 | ✅ 自动跟读走 `jump_to_offset`（自带落库），停听那一刻按段起点补一账 | ❌ | ❌ | ❌ | ❌ | ❌（无朗读） |

### 同步

| 功能 | **Folio** | Okular | Foliate | Readest | Anx Reader | KOReader |
|---|---|---|---|---|---|---|
| 进度 / 批注 / 书库同步 | ⚠️ 进度 / 批注 / 书签 + **每本书的阅读设置**（手动 WebDAV；书文件与自动同步未接） | ❌ | ❌（官方口径：把 JSON 复制走） | ✅ 官方云 | ✅ WebDAV 整库（书 + 进度 + 批注 + 云端书按需下载） | ⚠️ 进度走 KOSync（可自建服务器）；批注仅文件级导出 / 导入合并 |
| 同步协议 | WebDAV（手动；每设备一份 JSON 快照，按记录更新时间合并） | — | — | KOSync / WebDAV / S3 / OneDrive / iCloud（5 类） | WebDAV | KOSync |

> Folio 的同步已经接线，但**只做手动**：远端每台设备写自己的一份 JSON，合并按每条记录的
> 更新时间取新，删除用墓碑传播。搬的四样是进度、批注、书签，以及 2026-10-27 加上的
> **每本书的阅读设置** —— 那一格走的是差量：库里存的本来就是「这一本与全局默认不同的
> 那几格」（逐格比对接 serde 走，不手写 `Reader` 字段清单；`mode` / `light` / `dark`
> 三格语义上不给单本书覆盖，用排除表挡住），所以同步出去的是整本差量 + 一个
> `updated_at`，没单独设过的书不会被一份快照绑死在全局默认之外。**书文件与自动 / 后台
> 同步仍未做** —— 它与 Anx 的整库 WebDAV 不是一个量级。

### 平台

| 功能 | **Folio** | Okular | Foliate | Readest | Anx Reader | KOReader |
|---|---|---|---|---|---|---|
| 平台矩阵 | Linux / macOS / Windows / Android；iOS 与 HarmonyOS 宿主在树、未进发布矩阵 | 全平台 + Android | 仅 Linux | 6 端 + Web | 5 端 + HarmonyOS | **最野**：Android + Kindle / Kobo / PocketBook / Bookeen / Cervantes / ReMarkable + 桌面三端（SDL） |
| 移动端功能完整性 | ✅ 全功能 | ⚠️ Android 无批注 UI | ❌ 无移动端 | — | — | ✅ Android 是一等公民 |
| 生态插件 | ❌ | ❌ | — | ✅ KOReader / Calibre / 浏览器扩展 | ❌ | ✅ 37 个插件（统计 / OPDS / 新闻 / 剪藏…） |

> 多端对 Folio 只是 host 层分叉，不是重写。

### 文档工作流（不追）

| 功能 | **Folio** | Okular | Foliate | Readest | Anx Reader | KOReader |
|---|---|---|---|---|---|---|
| 表单 / 数字签名 / CUPS 打印 / 演示模式 | ❌ 不追 | ✅ | ❌ | ❌ | ❌ | ❌ |

## 值得借的清单（按优先级）

| # | 事项 | Folio 现状 | 状态 | 照谁做 | 上游参考 |
|---|---|---|---|---|---|
| 1 | ~~**批注闭环**：列表屏、导出、书签、删除撤销~~ **已补（2026-10-26）**：书内「批注」格 + 跨书「笔记」屏、txt/MD/HTML 三档导出、顶栏书签开关、6 秒撤销胶囊；**2026-10-27 那一屏又补了搜索**（一屏一本一张卡，命中面是书名 / 引文 / 笔记 / 出处章名） | 后端原样（5 色 3 画法 + SQLite + 版本化锚点），UI 四件套已接 | ✅ 已完成 | 列表照 Okular、导出照 Foliate、撤销照 Okular 命令层 | Okular `part/side_reviews.cpp`、`core/documentcommands_p.h`；Foliate `annotations.js` |
| 2 | **批注导入互通**（Moon+ Reader / ReadEra / JSON） | 无 | ❌ 未做 | Readest 已趟出格式兼容的面；KOReader 的 `.annotations.lua` 是「文件级搬家 + 合并」的另一条路 | Readest 的 Moon+ / ReadEra 导入；KOReader `.annotations.lua`（路径待盘点） |
| 3 | ~~**同步**：接线 WebDAV，做进度 + 批注整库同步~~ **手动版已补（2026-10-26）**：进度 / 批注 / 书签，每设备一份快照，按记录更新时间合并；**2026-10-27 再带上每本书的阅读设置**（整本差量 + 一个 `updated_at`）；书文件与自动同步未接 | `crates/folio-core/src/sync.rs`（快照与合并）+ `net/remote/dav.rs`（传输）；设置页可测试连接 / 立即同步 | ⚠️ 部分：手动已接四样，书文件 / 自动未做 | 形态照 Anx 的整库同步 | 自家 `folio-core/src/sync.rs`、`net/remote/dav.rs`、`infra/book_settings.rs`；第二步 KOSync（KOReader 生态事实标准） |
| 4 | ~~**OPDS 书源**~~ **已补（2026-10-26）**：1.x Atom + 2.0 JSON，书源管理 / 封面 / 下载入书库 / 翻页，Basic 认证；搜索与 OPDS-PSE 未做 | 架面「⋮」→ OPDS 书源；core `net/opds.rs` | ⚠️ 部分：浏览与下载已接，搜索 / PSE 未做 | Foliate（1.x + 2.0 一并支持）；KOReader 连 PSE 都有 | Foliate `src/opds/`；KOReader `plugins/opds.koplugin/`；Readest 的 OPDS-PSE |
| 5 | **TTS**：段级 → 句级高亮，补 macOS / Windows 后端 | 段级 utterance，仅 Linux / Android | ⚠️ 部分：段级已做，句级 / macOS / Windows 未做 | Foliate 的句级高亮 | folio-core 的行盒已在手；之后补 EPUB 3 Media Overlays |
| 6 | ~~**选区翻译**：Google 免 key / OpenAI 兼容 / DeepL 三档 + v24 缓存 + 弹层内换语 + 设置页配引擎~~ **已补（2026-10-26）**；剩 **本地词典**、**AI 陪读** | 翻译三引擎已接；词典 / AI 仍无 | ⚠️ 部分：翻译完成，词典 / AI 未做 | 翻译照 Foliate / Readest，端点照 KOReader；词典照 KOReader 的 Stardict + sdcv；AI 陪读照 Readest 的防剧透 RAG | `net/translate/`、`store/translations.rs`；KOReader `frontend/ui/translator.lua`；Readest `services/translators/`（词典 / AI 上游路径待盘点） |
| 7 | **跨书全文搜索**：在整架书里找一段字，而不是一本书里找 | 书内那一趟已落地 —— `folio-core/src/search.rs` 现读现扫、零索引，文件头带着三本真书的量（典型一本 10 ms 内、最坏那本全本 274 ms；FTS5 trigram 因「两个字的中文查询静默返回 0 行」与 3.2–4.4× 体积被否）。`scan` 的形状是「交出单本文档」，界面那侧也只有 `toc_panel` 一处用它 | ❌ 未做（只到单本） | Readest 的跨库搜索；KOReader 只有文件名搜索，不是标杆 | 自家 `crates/folio-core/src/search.rs`；缺的「逐本开容器」那队列 2026-10-27 已经在架面落了一半（`shelf/view/scan.rs`：在架的读库不重开文件、没入库的按 `ParseBudget::AtMost(1)` 顺序开 + `Priority::Prewarm`），全库扫描可以直接长在同一套地基与作业池上 |
| 8 | **正文预处理那一层**：简繁转换、Bionic Reading 这类「进排版之前改一遍文本」 | 没有插槽。CSS 侧 `text-transform` 在**忽略清单**里，理由已经写在代码上：它会改变字符本身，而锚点与进度都建立在源文本字节偏移上（`Locator` 的坐标系）—— 见 `content/format/css/computed.rs` 那条注释。`.txt` 小说那一族有规则化的路（`content/format/novel/` 的 detect / rules），但那是**分章与书目提取**，不是文本变换 | ❌ 未做，且不是加一条 `to_uppercase()` 的事：要接就得连锚点那套字节偏移映射一起定 | Readest 的 12 个 transformer（含简繁）、Anx 的 Bionic | Readest 的 transformer 那一层（上游路径待盘点）；自家 `content/format/css/computed.rs` 的忽略清单注释、[`Locator.md`](Locator.md) 那套字节偏移坐标系 |

## Folio 已经领先的地方（保持住）

| 领域 | Folio 现状 | 对比 |
|---|---|---|
| 格式矩阵 | 21 provider 纯 Rust；DOCX / ODT / RTF / CHM 一格，六家里只有 Folio 与 KOReader 有 | 其余四款全空；纯 Rust 的容器解码（ZIP / RAR / 7z / tar）是独一份的工程形态 |
| 内核路线 | 与 KOReader 并列的「无 WebView 自研排版」，GPU 渲染 | 仿真卷页、逐帧捕获、跟手手势都长在这条路上；KOReader 那条路上长的是墨水屏宿主 |
| 定位系统 | 字符偏移 Locator，换字号 / 换窗口不漂移 | foliate 系用 CFI（同性质）；KOReader 用 xpointer（HEAD 起并存 EPubCFI）；Okular 用页号（换字号就漂） |
| 本地书库体验 | 判重、监听、Wi-Fi 传书 + PIN，全离线零账号 | 六家里只有 Readest 的 LocalSend 勉强同类；KOReader 的无线传书要 Calibre 服务端 |
| 听读一体 | 听书时间计入统计、自动跟读；六家里只有 Folio 与 Readest 有朗读 | KOReader 连内置 TTS 都没有；「听就是读」的设计只有 Folio 在做 |
