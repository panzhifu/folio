# Format API 设计

这一层把一个书文件变成可读、可取、可写回的东西。按你的功能定六个 API，
按使用顺序排列；每个只写函数名、参数、返回值和用法。

## 使用顺序（一屏看完）

```rust
// 1. 识别：这是什么格式
let Some(format) = detect(path) else { /* 拒绝导入 */ };

// 2. 打开：读元数据（可重排、定版式同一份）
let mut book = open(path)?;
let meta = book.meta();

// 3. 章节位置
let chapters = book.chapters();

// 4. 资源清单
let list = book.resources();
let bytes = book.resource_bytes(&list[0].href);

// 5. 取内容：用第 3 步拿到的章
let text = book.chapter_text(&chapters[3])?;

// 6. 写回：能写的格式才用
if book.can_edit() {
    book.write_meta(&new_meta)?;
    book.write_chapter(&chapters[3], "新正文")?;
}
```

## API 一览

### 1. detect — 识别文件格式

```rust
pub fn detect(path: &Path) -> Option<Format>
```

- 参数：文件路径。
- 返回：格式（EPUB / TXT / PDF…21 种）；认不出回 `None`——拒绝导入，不猜。
- 用法：导入前第一问，`None` 就不要这本书。

### 2. open + meta — 加载与元数据

```rust
pub fn open(path: &Path) -> Result<Book>
pub fn meta(&self) -> &DocMeta
```

- `open`：只读索引，不解析正文，所以便宜；带密码的 PDF 另有 `set_passphrase`。
- `meta`：书名、作者、语言、封面等，一份 `DocMeta`。
  **可重排（文档式）与定版式拿的是同一份**，这一步不区分两族。

### 3. chapters — 获取章节位置

```rust
pub fn chapters(&self) -> &[Chapter]
```

- 返回：章表，每章 = 标题 + 位置。
- txt 已经按分章规则切好，调用方不感知规则。
- 拿到的章直接喂给第 5、6 步。

### 4. resources — 资源清单

```rust
pub fn resources(&self) -> &[Resource]
pub fn resource_bytes(&self, href: &str) -> Option<Vec<u8>>
```

```rust
pub enum ResourceKind { Css, Image, Text, Font, Video, Audio, JavaScript, Other }
pub struct Resource { pub href: String, pub kind: ResourceKind, pub size: Option<u64> }
```

- 类型一步写全；**消费先只认 Css、Image、Text 三类**——Text 是书内夹带的
  纯文本（txt 附件之类），当正文一样取字节（Font 已在用）；
  Video / Audio 先登记，JavaScript 只登记、永不执行。
- 章内图片引用指向这份清单，不再单独存图。
- 取不到字节回 `None`：资源缺失，那块地方空着。

### 5. chapter_text / page_image — 获取书籍内容

```rust
// 可重排（文档式）：按第 3 步的章取正文
pub fn chapter_text(&self, chapter: &Chapter) -> Result<String>

// 定版式：按页取一张图
pub fn page_image(&self, page: usize, edge: u32) -> Option<Vec<u8>>
```

- 打开书时才付正文的钱（与 `open` 分工：导入只付索引）。
- `edge` 是长边像素上限，要多大画多大。

### 6. can_edit + write_meta / write_chapter — 编辑书籍文件

```rust
pub fn can_edit(&self) -> bool
pub fn write_meta(&mut self, meta: &DocMeta) -> Result<()>
pub fn write_chapter(&mut self, chapter: &Chapter, body: &str) -> Result<()>
```

- **先问 `can_edit`**：目前只有 EPUB 与 txt / markdown 回 `true`（内部两档：
  ZIP 重打包、纯文本就地改），其余格式这两个 write 不要出现。
- 写回只动书库里的副本，用户的原文件不碰。
- 写回是原子替换，失败不毁书；写完章表重扫、缓存失效由引擎侧做。

## 优化（现有实现里已经做了的）

只报名字，每条一句话：

- **容器共用**——ZIP / 7z / RAR / tar / PalmDB / OLE2 / ITSF 一族一份读取层，
  21 个格式复用：EPUB、DOCX、ODT、CBZ 开的是同一个 zip。
- **索引 / 正文分离**——打开只读索引，正文按章现取；导入不白解一千多个 zip 条目。
- **一趟扫描多份产出**——章表、字数、图片引用、字符集，一次 tokenizer 全出，
  不为每样各扫一遍。
- **并行预算**——同时导 M 本、每本内部 I 路，一份总预算两层分，不各猜各的。
- **判定便宜在前**——扩展名先判（不花钱），字节嗅探兜底。
- **共用文本链路**——charset 识别 + 规范化一条线，txt / markdown / HTML 族走同一条。
- **共用件一份**——href 解析、字符分类、woff / woff2 字体解码、SVG 栅格化，
  全仓库各一份，格式不自带。
- **纯函数形状**——路径进、类型出，测试不需要容器以外的任何东西。

---

现状一行：1 / 2 / 3 / 5 在现有代码里已有对应物（`provider_for` / `index` /
`chapters` 章表 / `chapter_source`），4 与 6 是新增。
