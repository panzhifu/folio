# folio-format：对外 API 规格

> 状态：**设计已定，代码未改**（2026-11-09）。本文描述收窄后的目标 API；当前
> `crates/folio-format/src/provider.rs` 里的 `DocumentProvider` 仍是 21 个方法，
> 迁移清单见 §六。
>
> 文中符号名与职责是契约，路径与行号会漂；认符号，不认行号。源码注释里引用的
> `plans/xxx.md` 是旧路径，对应文件现在都在 `doc/` 下。
>
> 前置：`doc/导入管线.md`（判定 → 落地 → 解析 → 落库那条管线）、
> `doc/text-out-of-db.md`（正文不入库）、`doc/Locator.md`（位置坐标系）。
> 本文只管**解析层对外长什么样**。

## 一、这一层是什么

`folio-format` 只回答两件事：这份文件是什么、怎么把它读成**位置清单**。不认识数据库、
网络、界面与读者偏好；`Cargo.toml` 里明令不许出现 `rusqlite` / `tokio` / `axum` /
`ureq` / `notify` / `dirs`（有测试逐行守着），当前 `publish = false`。

一条硬边界：**后端解析能脱离 app 单独用**。往这一层加东西之前先问这句还成不成立。

```
字节 + 路径
  → identify            判成 Readable / Known / NotABook / CorruptContainer
  → open                绑定 provider、容器句柄、规则、口令
  → Document::manifest  只产位置，不碰正文
  → Document::resolve   按位置兑现（取字节 / 解码 / 栅格化）
  → 上游 core / app     排版、锚点、绘制、入库
```

### 这次改动要解决的

不是「功能太多」，而是**公开概念太多**：现在 `lib.rs` 把十几个 `pub mod` 和几十个
内部类型全导出，外部得先理解 `ChapterText` / `ImageRes` / `FontRef` / `ParseBudget` /
`PageText` / `CssStored` 才能用。**收窄的是公开表面，不是解析能力**——一行解析代码不删，
21 种格式全留。

## 二、数据模型

### 2.1 两族，以及对外不再暴露 `Content`

格式差别到 `Content` 为止；但对外统一收进 `Manifest`，`Content`/`FlowDoc`/`FixedDoc`
退回 crate 内部。

| 族 | `ContentKind` | 位置形状 | 格式 |
|---|---|---|---|
| 可重排 | `Reflow` | `chapters: Vec<ChapterRef>` | EPUB / FB2 / HTML / MHTML / HTMLZ / CHM / TXT / MD / RTF / DOCX / DOC / ODT / MOBI / PDB |
| 定版式 | `Fixed` | `pages: Vec<PageRef>` | PDF / AZW4 / DjVu / CBZ / CB7 / CBR / CBT |

可重排的章正文是**引擎就绪的 XHTML 片段**（`contract.rs` 的唯一输出约定）；定版式没有
XHTML，页是既定的画布。往上（排版链）只有这两族，界面不问 `Format`。

### 2.2 元数据 `DocMeta`

按 Dublin Core 取舍，共 14 格：

| 格子 | 含义 |
|---|---|
| `title` | 缺了用文件名兜底，不会是空串 |
| `authors[]` / `contributors[]` | `dc:creator` / `dc:contributor`；译者、编者、绘者进后者 |
| `subjects[]` / `description` / `publisher` | 主题词、简介（剥成纯文本）、出版社 |
| `published` | **原样存字符串**，不折日期类型（作者给的精度从「2023」到完整时间戳都有） |
| `language` / `isbn` / `dctype` / `rights` | BCP 47 原样；ISBN 认三种写法 |
| `series { name, position }` | 不在 DC 十五项里；`belongs-to-collection` 与 `calibre:series` 都认 |
| `format` / `size` | 我们自己的格子，不是 DC 项 |

空串/空表 = 「这本书没说」，界面上有值才显示那行。**填充率因格式差异极大**：CBZ 靠
`ComicInfo.xml`、TXT 靠规则表从头部猜、MOBI 靠 EXTH；`.doc` 不读 SummaryInformation、
CHM 没有书目区、DjVu 真样本多为空。别假设每格都有值。

### 2.3 章节位置

`Location::Chapter(ChapterLocator)`，三态：

| 变体 | 语义 | 文件里有地址吗 |
|---|---|---|
| `Entry(href)` | 名单式容器按条目名取（EPUB spine、CHM topic、HTMLZ/MHTML 的 HTML） | 有 |
| `Section(n)` | 单文件容器第 N 章（TXT/MD/FB2/RTF/MOBI/DOCX/DOC/ODT/PDB） | **没有**，是「重新分章规则」的承诺 |
| `Inline(text)` | 降级/测试，正文随文档来 | 没有 |

`Section(n)` 存序号而非字节区间，是**刻意的取舍**：分章规则一改，字节区间静默指到
别处，而序号让「第 N 章」这个说法整体失效——两者都要重扫，后者坏得看得见。
改分章 / 页表 / 字数口径必须 bump `INDEX_VERSION`；`rules_hash` 的变化另走
`RulesChanged`（见 §五 3）。

`ChapterRef` 是 `{ index, title, char_len: Option<usize>, locator: Location }`。
`char_len` 是规范化可见字数（全局进度分母），`None` = 没算过，与 `Some(0)`（真空章）
严格区分。

### 2.4 资源位置

```rust
pub struct ResourceRef {
    pub key: String,        // 查询键，与章的 spine_href 同形
    pub source: String,     // 字节真实所在（EPUB3 fallback= 时与 key 不同）
    pub kind: ResourceKind, // Css / Font / Image / Audio / Video / Script
    pub delivery: Delivery,
}

pub enum Delivery {
    Inline,    // 没有位置，内容直接在 Manifest 里：CHM / 单 HTML 的内联 <style>
    Bytes,     // resolve → Vec<u8>：图片、字体、CSS 条目、章正文、PNG 页
    Stream,    // resolve → Box<dyn Read + Seek + Send>：视频、音频
    Declared,  // 只登记不兑现：JavaScript
}
```

「位置」不是单一概念：除了容器条目名，还有**合成 href**（DOC/RTF 的 `img-0.png`，容器里
根本没这条）、**归一化查询键**（`fallback` 时取不到字节）、**磁盘路径**（单 HTML 外链图、
漫画缓存）、以及**算出来的**（PDF `Render` 页）。所以 `key` 与 `source` 必须分开，
`Delivery` 把「怎么拿」写在类型上，调用方不用猜。

`Resolved::Bytes` 只交字节不带 mime：调用方按 `ResourceRef.kind` 或现有的
`guess_media_type`（按 magic bytes 反推）自己定。封面那一列历史上就是「只有字节、没有
媒体类型」，那份判据留在 format 层内部即可。

### 2.5 定版式

`PageRef` 的 `source` 两型：

- `Image { href }`：按容器路径取字节（CBZ/CB7/CBR/CBT 等）。
- `Render { index, edge, vector }`：交给栅格化引擎，`resolve` 交回 PNG（PDF/AZW4/DjVu）。
  `vector` 决定放大时按区域重画补不补得出细节。

**文字层 `PageText` 不在页表里**，通过 `Location::PageText { index }` 按页现取，给选中、
划词、复制、搜索命中用。画得出这一页 ≠ 读得出这一页的字；漫画页就是「有图没字」。
页表落库（SQLite 的 `fixed_json`），像素不入库。

### 2.6 三个主要载体

```rust
pub struct Manifest {
    pub meta: DocMeta,
    pub kind: ContentKind,
    pub chapters: Vec<ChapterRef>,
    pub pages: Vec<PageRef>,
    pub toc: Vec<TocEntry>,         // 可重排用 fragment，定版式用 page，互斥
    pub resources: Vec<ResourceRef>,
    pub cover: Option<CoverRef>,
    pub rules_hash: Option<String>, // 仅 Text | Pdb 有，必须随章表一起落库
}

pub enum Location {
    Chapter(ChapterLocator),
    Resource { key: String },
    Page { index: u32, edge: u32, region: Option<PageRegion> },
    PageText { index: u32 },
    AllPageTexts,   // 整本一次取齐，给搜索
    AllChapters,    // 整本 memo
}

pub enum Resolved {
    Text(String),                         // 章正文 / CSS
    Bytes(Vec<u8>),                       // 图片 / 字体 / PNG 页
    Reader(Box<dyn Read + Seek + Send>),  // 视频 / 音频
}
```

`Manifest` 是**纯数据、可序列化**（`ChapterLocator` / `FixedPage` 本来就带 serde），
可以整份落库——这正是「数据库只存位置、其他层拿位置取内容」的落点。

**复用与新增**：`DocMeta` / `ChapterLocator` / `TocEntry` / `CoverRef` / `PageRegion` /
`PageSource` / `PageText` / `Format` / `ContentKind` / `Verdict` / `Edits` /
`CompiledRules` / `Preview` / `Passphrase` / `ParseBudget` 沿用现有类型；
`Manifest` / `ChapterRef` / `PageRef` / `ResourceRef` / `ResourceKind` / `Delivery` /
`Location` / `Resolved` / `OpenOptions` 是本次新增。

## 三、对外 9 个 API

**常用的只有 `manifest()` 和 `resolve()` 两个。** 不新增门面类型，直接把 `DocumentProvider`
收窄成 `Document`，用一个顶层 `open()` 返回 trait 对象。

### 3.1 打开与判定（4 个）

```rust
pub fn open(path: &Path, opts: OpenOptions) -> Result<Box<dyn Document>>;
pub fn identify(display_name: &str, path: &Path) -> Verdict;
pub fn looks_like_a_book(path: &Path) -> bool;
pub fn supported_extensions() -> &'static [&'static str];
```

`OpenOptions` 一次性绑定四件事，避免「调用中途换页」：

```rust
pub struct OpenOptions {
    pub parse_budget: ParseBudget,               // 并掉 index_with
    pub novel_rules: Option<Arc<CompiledRules>>,  // 并掉规则绑定；None = 进程生效表
    pub passphrase: Option<Passphrase>,          // 并掉 set_passphrase
    pub warm: bool,                              // 并掉 warm
}
```

`open` 只做判定与绑定（轻），解析索引那一趟在 `manifest()`。`identify` 的 `Verdict` 四档
是导入报告的四句人话：`Readable(Format)` / `Known(还不支持)` / `NotABook` /
`CorruptContainer`（**坏包 ≠ 不是书**，读者对这两句的处置完全不同）。判定**以内容为准，
不以扩展名为准**：三层（扩展名粗筛 → 魔数 → 开容器读条目），ZIP 里还会认出「容器自己
就是书」（不许退层，把 EPUB 抽散就毁了）与「合集包逐条抽出」两种形状。
`looks_like_a_book` 是整机扫描的粗筛，在开文件之前用一张 `never_holds_a_book` 表挡掉
相册/视频/安装包（Android 共享存储上一次 `open` 是毫秒级）。

### 3.2 文档实例（2 个，核心）

```rust
pub trait Document: Send + Sync {
    fn manifest(&self) -> Result<Manifest>;
    fn resolve(&self, loc: &Location) -> Option<Resolved>;
}
```

- `manifest()` **只产位置**，不碰正文、不解图、不算像素。导入走这一条，正文不入库。
- `resolve()` 按需兑现：容器寻址、WOFF→sfnt 解码、SVG/PDF 栅格化、`Stream` 分档、缓存
  **全部留在 format 层**。读不到一律 `None`，不是 `Err`（屏幕上一块空着，正文照排）。
- 批量需求用 `Location::AllPageTexts` / `AllChapters` 覆盖，不额外加方法。

### 3.3 规则（3 个）

```rust
pub fn compile_rules(edits: &Edits) -> Arc<CompiledRules>;
pub fn split_chapters(source: &str, rules: &CompiledRules) -> Vec<ChapterRef>;
pub fn preview_chapters(source: &str, rules: &CompiledRules) -> Preview;
```

三个都是**不打开书也能用**的纯函数，给设置页与 AI 草案那两条路。规则是数据（按语言
换表 = 加文件，不发版），出厂表 + `Edits` 差量 → `CompiledRules`。

## 四、典型用法

```rust
// 导入：只落位置
let book = format::open(&staged_path, OpenOptions::default())?;
let m = book.manifest()?;
store.save_manifest(&m);   // chapters / pages / resources / toc / rules_hash 全落库

// 阅读：按位置现取
let chapter = book.resolve(&m.chapters[3].locator);  // → Resolved::Text(XHTML)
let image   = book.resolve(&Location::Resource { key: m.resources[0].key.clone() });

// 扫描粗筛
if format::looks_like_a_book(&path) {
    if let Verdict::Readable(fmt) = format::identify(&name, &path) { /* 进管线 */ }
}

// 自定义分章规则（TXT / PDB）
let rules = format::compile_rules(&edits);
let book = format::open(&path, OpenOptions { novel_rules: Some(rules), ..default() })?;
let m = book.manifest()?;  // rules_hash 已随 Manifest 返回，必须一起落库
```

## 五、关键决策与理由

1. **不建门面，直接改 trait。** 收窄的是公开表面，不是解析能力；不新增 `Book` /
   `facade.rs`，旧 `pub mod` 按 §六 收进 crate 内。

2. **兑现留在 format 层。** `Container` 干的是 ZIP 中央目录一次建索引、CHM LZX 解码状态、
   RAR/7z/tar 顺序解压缓存、大小写/分隔符宽松匹配、`..` 穿越防护、解压搬到锁外的并发
   优化。推出去等于让上层重写一整套容器知识，而按硬边界这些本该留在这一层。

3. **规则打开时绑定到实例，不是进程全局、也不是每次调用。** 现有 `rules::install` 刻意是
   `OnceLock` 不可换：分章跑在导入/重扫的工作线程里，中途换表会得到「前半本旧表、后半本
   新表」的章表，而 `rules_hash` 只有一份、判失效时对得上，于是没有任何东西会再来扫一遍。
   绑到实例既避开全局限制，又保证单本一致。**`rules_hash` 必须随 `Manifest` 一起落库**，
   否则允许传规则就是静默漂移（同一本 `Section(3)` 换份规则指到隔壁章，没人发现）。
   `Text | Pdb` 两族共用同一张表。

4. **TXT/PDB 的章节位置仍用 `Section(n)`，快做在实例内。** 不落字节偏移，三条理由：

   - 引擎的位置建立在 `normalize()` **之后**的文本上（`split_with` 的输入是规范化全文，
     `lines` 就是它按 `\n` 切出来的），前面还有 `charset::read_plain` 的码页转码；拿规范化
     后的偏移 seek 原文件会读到错地方。要修就得让 `normalize` 保偏移或产出映射表，而它的
     产物正是锚点坐标系，动它 = 进度与批注一起搅乱。
   - 规则一改，偏移**静默**失效；`Section(n)` 至少坏得看得见。
   - 收益也有限：`find_hits` / `cut` 本来就握着行号，只是产出 `Chapter::inline` 时丢了。
     把它们带出来，在实例内缓存「规范化全文 + 每章行区间」，`resolve(Section(n))` 按行
     区间 slice 即可，**不再重跑正则匹配**（取章路径上最贵的一段）。行区间只在实例内、
     不落库、不跨会话，因此不受规则变更影响，也不碰 `normalize`。

5. **`Delivery::Inline` 不能省。** CHM / 单文件 HTML 的作者样式内联在每页 `<head>` 的
   `<style>` 里，没有资源位置；只给位置会整包丢样式。

6. **`Stream` 按容器分档，不是所有容器都给得出。**

   | 容器 | 流式 | 原因 |
   |---|---|---|
   | ZIP stored/deflate | 可 | `EntryRange` + 独立句柄，天然可 seek |
   | Extracted（漫画缓存） | 可 | 已是磁盘文件，直接 `File::open` |
   | ZIP bzip2/LZMA/XZ/PPMd/Deflate64 | 否 | 走 zip-rs 锁内路径，无 seek，降级 `Bytes` |
   | CHM / ITSF | 否 | LZX 解码状态跨块，libchm 自己持有 |
   | PDF/DjVu `Render` 页 | 否 | 算出来的 PNG，本来就不是流 |

   `Reader` 要带 `Send`（`Container` 是 `Send + Sync`，播放器在别的线程）。

7. **JavaScript 只登记不执行**（`Delivery::Declared`）。GPUI 原生渲染、不嵌 WebView，
   依赖图里没有 JS 引擎；带脚本的 EPUB 常伴随 `encryption.xml`，而 DRM 明确不做。
   登记它只是为了「书里有这个资源」说得清，不提供取字节路径。

8. **保留范围：文字层 + 21 种格式全留。** `INDEX_VERSION` 与库 schema 不动；这次只动
   对外形状。

## 六、21 → 2 的合并映射与迁移清单

| 现有 `DocumentProvider` 方法 | 去向 |
|---|---|
| `format` / `supports` / `weight` | 收 `pub(crate)`（注册表内部判据；对外由 `identify` 给 `Format`） |
| `set_passphrase` | 并进 `OpenOptions` |
| `warm` | 并进 `open` |
| `index` / `index_with` / `metadata` | → `manifest()`（`ParseBudget` 进 `OpenOptions`） |
| `read` | **删**（只服务测试的合成品，改由测试内部构造） |
| `chapter_source` / `entry_text` | → `resolve(Location::Chapter)` |
| `image_bytes` | → `resolve(Location::Resource)` |
| `fonts` / `font_refs` / `font_refs_in` / `read_font` | → `resolve(Location::Resource)`，身份在 `Manifest` 里 |
| `render_page` / `render_page_region` | → `resolve(Location::Page)` |
| `page_text` / `page_texts` | → `resolve(Location::PageText{..})` / `AllPageTexts` |
| `read_texts` | → `AllChapters` / 实例内 `SectionCache` |

顶层散落项处置：

- `detect`（`lib.rs`）与 `sniff::identify` 重复 → **删 `detect`**。
- `provider_for` / `providers` / `provider_by_format` / `supports_extension` /
  `rules_hash` / `locator_for` → 收 `pub(crate)` 或并进 `Manifest`。
- `guess_media_type` / `media_extension` / `media_type_of` / `shrink_image` /
  `visible_*` → 内部工具，收 `pub(crate)`（若 core 仍需要 `visible_*`，保留公开并注明用途）。
- `pub mod` 只留 `novel::rules`（规则类型要给调用方）与 `novel::draft`（`Preview` 要给），
  其余收进 crate 内。

**这是破坏性改动**：`folio-core` / `folio-app` 所有调 `DocumentProvider` 方法的地方要同步
迁移，收 `pub mod` 尤其会红一片（`content/document/book.rs` 直接 import 了一长串）。
好消息是 `ChapterLocator::Section(n)` 语义不变，所以 `INDEX_VERSION` 与库 schema 不用动，
重扫逻辑不受影响。

## 七、落地状态

- [x] 方案与取舍定稿（本文）
- [ ] `Document` trait 与 9 个入口
- [ ] `Manifest` / `Location` / `Resolved` / `Delivery` / `OpenOptions` 等新增类型
- [ ] `Container::open_range` 与 `Stream` 分档
- [ ] `split_with` 带出行区间 + 实例内 `SectionCache`
- [ ] `pub` 收窄与 core/app 调用点迁移
- [ ] 契约测试（每格式 `open → manifest → resolve` 往返）
