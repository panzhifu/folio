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
扫描本仓得出的（含「笔记屏是空状态」这类自我揭短的结论）。写作日期 2026-10-26。

图例：✅ 有 · ⚠️ 部分 / 有差距 · ❌ 没有 · — 文中未盘点

## 结论

| 判定 | 内容 |
|---|---|
| 总体 | **阅读内核已站进第一梯队，差距集中在外向生态。** |
| 领先 / 独有 | 格式矩阵最宽（21 provider 全纯 Rust）；自研排版引擎、零 WebView；双页自动退档、仿真卷页、Wi-Fi 传书（局域网 + PIN）、字符偏移 Locator（换字号换窗口不漂移）、**听读一体**（五家里只有 Folio 与 Readest 有朗读，且只有 Folio 把听书计入统计） |
| 差距（按补齐划算程度） | ① **批注闭环** —— 列表屏（书内「批注」格 + 跨书「笔记」屏）、导出（txt/MD/HTML）、书签、删除撤销**已补齐**；剩批注导入互通；② **同步** —— WebDAV 协议层已写好但零接线；③ **OPDS 书源**；④ **TTS** 补平台与句级；⑤ **AI 与词典** —— 选区翻译弹窗（Google 免 key / OpenAI 兼容 / DeepL 三档）**第 2 期已补**，本地词典与 AI 是战略级选择，暂缓不动摇内核 |
| 不追 | Okular 的表单 / 签名 / 打印 / 演示模式是文档查看器的物种能力；KOReader 的 Kindle/Kobo/ReMarkable 那一串墨水屏宿主是它物种的一部分 —— 都与 Folio 的定位无关，不进 roadmap |

## 功能矩阵

### 格式与内核

| 功能 | **Folio** | Okular | Foliate | Readest | Anx Reader | KOReader |
|---|---|---|---|---|---|---|
| 格式数量 | **21 provider**（测试钉住 30 个扩展名） | 14 后端（PDF 系为主） | 6 种 | 14 种 | 白名单 6 种 | 最宽一族：crengine 30+ 扩展名 + PDF/DjVu/图片 |
| DOCX / ODT / RTF / CHM | ✅ | ❌ | ❌ | ❌ | ❌ | ✅（crengine） |
| HTMLZ / MHTML | ✅ | ❌ | ❌ | ❌ | ❌ | ⚠️ HTMLZ 有，MHTML 无 |
| DjVu / AZW4 | ✅ | ❌ 或靠转换 | ❌ 或靠转换 | ❌ 或靠转换 | ❌ 或靠转换 | ⚠️ DjVu 有，AZW4 无 |
| MD / FB2 / FB3 / PDB | ❌（部分在路上） | ❌ | ❌ | ❌ | ❌ | ✅ |
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
| 版式可调 | ✅ 字号 / 行距 / 段距 / 缩进 / 两端对齐 / 版心 / 边距 / 字体 | ⚠️ 仅全局字体 | ⚠️ 字体 / 行高 / 连字 / 边距 / CSS | ✅（另加 12 个 transformer） | ✅（另加 Bionic Reading） | ✅ **最细**：字重 / 字距 / CJK 缩放 / 断词词典 / DPI / Gamma / 提示（`creoptions.lua` 全表） |
| 简繁转换等 transformer | ❌ | ❌ | ❌ | ✅ 12 个 | ❌ | — |
| Bionic Reading | ❌ | ❌ | ❌ | ❌ | ✅ | — |
| 主题 | 明暗三态 + 用户 JSON 主题目录 | 9 种色彩滤镜（无纸色主题） | 9 套 + 自定义 | 7 套 × 明暗 + 自定义 | 自定义色 + 背景图 | ⚠️ 夜间反色一档 + 定时切换（无多档主题系统） |
| 连续滚动 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 分页 | ✅ | ✅ 单页 / 概览 | ✅ | ✅ | ✅（自适应） | ✅（含页重叠） |
| 双页 / 对开 | ✅ 宽度不够自动退档 | ✅ 对开 | ⚠️ 多列流式 | ✅ 双栏 | ❌ | ✅ crengine 双栏（仅 page 模式）；PDF 走 kopt 切割 |
| 翻页动画 | **6 档**：Slide / SlideUp / Cover / Curl 仿真 / Scroll / None | ❌ 无仿真 | ❌ 无仿真 | push / slide / curl | ❌ 无仿真 | ⚠️ 仅 swipe 过渡（设备相关），无仿真 |

### 批注 —— 闭环已补齐

| 功能 | **Folio** | Okular | Foliate | Readest | Anx Reader | KOReader |
|---|---|---|---|---|---|---|
| 高亮 / 图形样式 | ✅ 5 色 × 3 画法（色带 / 实线 / 波浪） | ✅ 13 类图形工具（手绘 / 图章 / 形状，定位外） | ✅（CFI） | ✅ 3 样式 + 手写 | ✅ 2 样式 | ✅ 9 色 + 自定义 × 4 画法（色带 / 下划线 / 删除线 / 反色） |
| 存储与锚点 | ✅ SQLite，锚点是格式版本化 JSON（v13 迁移防 reindex 级联删） | ✅ 写回 PDF / sidecar XML | ✅ CFI 明文 JSON | ✅ | ✅ CFI | ✅ 书旁 `.sdr/metadata.lua` sidecar；批注 / 笔记 / 书签统一一份 annotations 模型 |
| 选中交互 | ✅ 拖选 + 长按吸词 + 把手 | — | — | — | — | ✅ 长按选词 |
| 就地笔记卡 | ✅ | — | — | — | — | ✅ Add note（可存 html / md） |
| 批注列表屏 | ✅ 目录面板「批注」格（书内）+「笔记」目的地（跨书，按书分组） | ✅ 按类型 / 按页过滤 | ✅ | ✅ | ✅ | ✅ booklist（⚠️ 无按类型 / 页过滤） |
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
| OPDS 书源 | ❌ | ❌ | ✅ 1.x + 2.0 | ✅ 全家桶 + OPDS-PSE（流式阅读） | ❌ | ✅ 1.x + 2.0 + OPDS-PSE |
| 网页剪藏 | ❌ | ❌ | ❌ | ✅ | ❌ | ✅ Wallabag |
| RSS / 新闻下载 | ❌ | ❌ | ❌ | — | ❌ | ✅ feed → EPUB |
| Calibre 无线 | ❌ | ❌ | ❌ | — | ❌ | ✅ 元数据搜索 + 无线传书 |
| 局域网传书 | ✅ Wi-Fi 浏览器 + 四位 PIN | ❌ | ❌ | ⚠️ LocalSend 勉强同类 | ❌ | ⚠️ 走 Calibre 服务端 |
| 收件箱目录监听 | ✅ inotify | — | — | — | — | — |
| 扫描导入 + 判重 | ✅ 整机扫描勾选导入 + content_hash 判重 | ❌ 无书库 | ✅ 书架 | ✅ 智能书架 | ✅ | ⚠️ 文件管理器制（无库级判重） |
| 分组 / 标签 / 评分 | ✅ 分组 | ❌ | ✅ | ✅ | ✅ 标签 / 分组 / 评分 | ✅ collections / favorites |
| 元数据编辑 | ✅ | — | — | — | — | ✅ 书详情可改 + 评论 |
| 跨书搜索 | — | — | — | ✅ | — | ⚠️ 文件名搜索 |
| 格式转换 | ❌ | — | ❌ | ❌ | ❌ | ✅ 内置 converter |

### 阅读统计

| 功能 | **Folio** | Okular | Foliate | Readest | Anx Reader | KOReader |
|---|---|---|---|---|---|---|
| 年热力图 | ✅ 整屏 | ❌ | ✅ | ❌（逐页数据无 UI） | ✅ | ❌（月历视图 + 每日直方图） |
| 连续天数 / 连击 | ✅ 最长连续天数 | ❌ | ✅ 连击 | ❌ | ✅ 连击 | ❌ |
| 其他图表 | ⚠️ 仅热力图 | ❌ | ✅ 瓦片 | ❌ | ✅ 图表 | ✅ 月历 + 直方图 + 单书统计 |
| 结账粒度 | ✅ 5 分钟段 + 空闲宽限防挂机 | ❌ | — | — | — | — |
| 听书计入 | ✅ | ❌ | ❌ | ❌ | ❌ | ❌（无朗读） |

### 同步

| 功能 | **Folio** | Okular | Foliate | Readest | Anx Reader | KOReader |
|---|---|---|---|---|---|---|
| 进度 / 批注 / 书库同步 | ❌ | ❌ | ❌（官方口径：把 JSON 复制走） | ✅ 官方云 | ✅ WebDAV 整库（书 + 进度 + 批注 + 云端书按需下载） | ⚠️ 进度走 KOSync（可自建服务器）；批注仅文件级导出 / 导入合并 |
| 同步协议 | WebDAV（协议层已写好未接线） | — | — | KOSync / WebDAV / S3 / OneDrive / iCloud（5 类） | WebDAV | KOSync |

> Folio 的独特状态是「备好未接」：`net/remote/dav.rs` 的 WebDAV 协议层
> （PROPFIND / GET / PUT / MKCOL）已写好、app 侧零调用 —— 六家里没有第二种这种形态。

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

| # | 事项 | Folio 现状 | 照谁做 | 上游参考 |
|---|---|---|---|---|
| 1 | ~~**批注闭环**：列表屏、导出、书签、删除撤销~~ **已补（2026-10-26）**：书内「批注」格 + 跨书「笔记」屏、txt/MD/HTML 三档导出、顶栏书签开关、6 秒撤销胶囊 | 后端原样（5 色 3 画法 + SQLite + 版本化锚点），UI 四件套已接 | 列表照 Okular、导出照 Foliate、撤销照 Okular 命令层 | Okular `part/side_reviews.cpp`、`core/documentcommands_p.h`；Foliate `annotations.js` |
| 2 | **批注导入互通**（Moon+ Reader / ReadEra / JSON） | 无 | Readest 已趟出格式兼容的面；KOReader 的 `.annotations.lua` 是「文件级搬家 + 合并」的另一条路 | 随批注闭环一起做 |
| 3 | **同步**：接线 WebDAV，做进度 + 批注整库同步 | `net/remote/dav.rs` 协议层零调用 | 形态照 Anx 的整库同步 | 自家 `folio-core/src/net/remote/dav.rs`；第二步 KOSync（KOReader 生态事实标准） |
| 4 | **OPDS 书源** | 无 | Foliate（1.x + 2.0 一并支持）；KOReader 连 PSE 都有 | Foliate `src/opds/`；KOReader `plugins/opds.koplugin/`；Readest 的 OPDS-PSE |
| 5 | **TTS**：段级 → 句级高亮，补 macOS / Windows 后端 | 段级 utterance，仅 Linux / Android | Foliate 的句级高亮 | folio-core 的行盒已在手；之后补 EPUB 3 Media Overlays |
| 6 | **AI / 词典 / 翻译** —— 选区翻译弹窗（Google 免 key / OpenAI 兼容 / DeepL 三档 + v24 缓存 + 弹层内换语 + 设置页配引擎）**第 2 期已补**；剩：本地词典、AI 陪读 | 弹窗 + 缓存 + 短语言清单 + 三引擎设置页已接 | 弹窗照 Foliate/Readest；引擎抽象照 Readest `TranslationProvider`；端点照 KOReader `translator.lua`（`client=gtx`） | `net/translate/`、`store/translations.rs`；上游参照 Readest `services/translators/`、KOReader `frontend/ui/translator.lua`、Foliate `selection-tools/translate.html` |

## Folio 已经领先的地方（保持住）

| 领域 | Folio 现状 | 对比 |
|---|---|---|
| 格式矩阵 | 21 provider 纯 Rust；DOCX / ODT / RTF / CHM 一格，六家里只有 Folio 与 KOReader 有 | 其余四款全空；纯 Rust 的容器解码（ZIP / RAR / 7z / tar）是独一份的工程形态 |
| 内核路线 | 与 KOReader 并列的「无 WebView 自研排版」，GPU 渲染 | 仿真卷页、逐帧捕获、跟手手势都长在这条路上；KOReader 那条路上长的是墨水屏宿主 |
| 定位系统 | 字符偏移 Locator，换字号 / 换窗口不漂移 | foliate 系用 CFI（同性质）；KOReader 用 xpointer（HEAD 起并存 EPubCFI）；Okular 用页号（换字号就漂） |
| 本地书库体验 | 判重、监听、Wi-Fi 传书 + PIN，全离线零账号 | 六家里只有 Readest 的 LocalSend 勉强同类；KOReader 的无线传书要 Calibre 服务端 |
| 听读一体 | 听书时间计入统计、自动跟读；六家里只有 Folio 与 Readest 有朗读 | KOReader 连内置 TTS 都没有；「听就是读」的设计只有 Folio 在做 |
