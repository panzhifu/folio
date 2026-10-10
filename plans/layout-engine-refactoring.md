# 排版引擎重构方案

## 一、现状分析

### 当前架构

```
layout() → Ctx::children() → Ctx::block() / Ctx::wrap_paragraph()
                                    ↓
                              collect() → 展平 DOM 为文本 + pieces
                                    ↓
                              Ctx::wrap_lines() → UAX#14 断行
                                    ↓
                              Ctx::emit_line() → 逐 run 度量 → LineBox
                                    ↓
                              Layout { blocks, lines }
                                    ↓
                              Fragmentainer::fragments() → 分片
                                    ↓
                              place_lines() → 每帧绘制
```

### 性能瓶颈

| 问题 | 现状 | 影响 |
|------|------|------|
| **无缓存** | 每次 `layout()` 从头计算 | 改字号/窗口大小 → 整章重排 |
| **无增量** | 一个 CSS 属性变化 → 全章重排 | 设置页调字号卡顿 |
| **逐字符度量** | `char_width()` 每个字符调用一次 | 几千行的章每帧几万次调用 |
| **大量 clone** | `Rc::clone` / `to_string` 71 处 | 堆分配压力 |
| **无轻量模式** | 算高也要完整排版 | 目录/进度条不需要行盒 |
| **同步阻塞** | 排版在主线程 | 大章节卡顿 UI |
| **无页图缓存** | 每帧从 Layout 重新绘制 | 滚动时重复工作 |

---

## 二、KOReader 架构借鉴

### KOReader 渲染流水线

```
LVDocView::Render(dx, dy)
  │
  ├─ checkRenderContext() → 是否需要重排？
  │   ├─ 否 → 从 _pagesData 反序列化页表 → 直接画
  │   └─ 是 → 继续
  │
  ├─ dropStyles() → 清旧样式
  ├─ applyDocumentStyleSheet() → 层叠
  ├─ initNodeStyleRecursive() → 每个节点算样式
  ├─ initNodeRendMethodRecursive() → 确定渲染方式
  │
  ├─ renderBlockElement(root) → 遍历块级元素
  │   └─ renderFinalBlock() → 每个块的最终渲染
  │       ├─ light_formatting? → 只算高，不排版
  │       ├─ _renderedBlockCache[block] → 有缓存？
  │       │   └─ 是 → 直接复用 LFormattedText
  │       │   └─ 否 → LFormattedText::format() → 排版 + 缓存
  │       └─ 输出 → LVRendPageList
  │
  ├─ context.Finalize() → 分页
  ├─ pages->serialize(_pagesData) → 序列化页表
  └─ updateRenderContext() → 记录渲染参数 hash
```

### KOReader 关键优化

1. **`_renderedBlockCache`** — 每个块的 `LFormattedText` 缓存，256 条 LRU
2. **`checkRenderContext()`** — 渲染参数 hash 比较，不变则跳过
3. **`_pagesData`** — 页表序列化，重开书直接反序列化
4. **`cachePageImage()`** — 后台线程渲染页图为位图
5. **`light_formatting`** — 只算高不排版（目录/进度条用）
6. **`LVDrawThread`** — 后台绘制线程
7. **`swapToCache()`** — 大书缓存到磁盘
8. **Partial rerendering** — 只重排变化的块

---

## 三、重构方案

### 阶段 1：缓存层（最小改动，最大收益）

#### 1.1 Layout 缓存

```rust
// 新增：排版结果缓存
pub struct LayoutCache {
    // (内容宽, 偏好 hash) → Layout
    cached: Option<(f32, u64, Layout)>,
}

impl LayoutCache {
    pub fn get(&self, width: f32, prefs_hash: u64) -> Option<&Layout> {
        self.cached.as_ref()
            .filter(|(w, h, _)| *w == width && *h == prefs_hash)
            .map(|(_, _, layout)| layout)
    }
    
    pub fn put(&mut self, width: f32, prefs_hash: u64, layout: Layout) {
        self.cached = Some((width, prefs_hash, layout));
    }
    
    pub fn invalidate(&mut self) {
        self.cached = None;
    }
}
```

**收益**：改页高（分页模式切换）不重排，改字号才重排。

#### 1.2 块级缓存

```rust
// 新增：每个块（DOM 子树）的排版结果缓存
pub struct BlockCache {
    // NodeId → 该块排好的行盒
    blocks: HashMap<NodeId, CachedBlock>,
}

struct CachedBlock {
    lines: Vec<LineBox>,
    height: f32,
    // 缓存键：块内所有节点的样式 hash
    style_hash: u64,
}
```

**收益**：DOM 结构不变、样式不变时直接复用。

#### 1.3 渲染参数 Hash

```rust
// 新增：判断是否需要重排的判据
pub struct RenderContext {
    width: f32,
    prefs_hash: u64,        // 读者偏好的 hash
    styles_hash: u64,       // 全书样式的 hash
    dom_hash: u64,          // DOM 结构的 hash
}

impl RenderContext {
    pub fn needs_relayout(&self, other: &Self) -> bool {
        self.width != other.width
            || self.prefs_hash != other.prefs_hash
            || self.styles_hash != other.styles_hash
    }
}
```

### 阶段 2：轻量模式

```rust
pub struct LayoutOptions {
    pub width: f32,
    /// `true` = 只算块高，不产出 LineBox（目录/进度条用）
    pub light: bool,
}

// light 模式下：
// - 不断行（每块一行）
// - 不调用 run_metrics
// - 不建 LineFragment
// - 只累加块高
```

**收益**：目录跳转、进度条拖动快 10x。

### 阶段 3：Measure 批处理

```rust
// 现状：逐字符调用
for ch in text.chars() {
    let w = measure.char_width(ch, font, size);  // 每字符一次虚函数调用
}

// 改进：批量预度量
pub trait MeasureBatch {
    /// 预度量一段文本的所有字符宽度
    fn premeasure(&self, text: &str, font: &FontKey, font_size: f32) -> Vec<f32>;
    
    /// 预度量一段文本的 run metrics（一次 shaping）
    fn measure_run(&self, text: &str, font: &FontKey, font_size: f32) -> LineMetrics;
}

// 断行时：
let widths = measure.premeasure(text, font, size);
// 然后直接用 widths[i] 而不是 char_width(ch)
```

**收益**：减少虚函数调用次数，shaping 只做一次。

### 阶段 4：减少内存分配

```rust
// 现状：每个 Piece 都 Rc::clone
struct Piece {
    style: Rc<RunStyle>,  // 每个 piece 一次 Rc clone
}

// 改进：按样式去重
pub struct StyleArena {
    styles: Vec<RunStyle>,
    index: HashMap<RunStyleKey, usize>,
}

impl StyleArena {
    pub fn intern(&mut self, style: RunStyle) -> StyleRef {
        // 相同样式只存一份，返回索引
    }
}

// Piece 改为：
struct Piece {
    style: StyleRef,  // 索引而不是 Rc
}
```

**收益**：减少 Rc clone 和堆分配。

### 阶段 5：后台排版

```rust
// 新增：后台排版任务
pub struct LayoutWorker {
    sender: Sender<LayoutRequest>,
    receiver: Receiver<LayoutResult>,
}

struct LayoutRequest {
    chapter: String,
    width: f32,
    prefs: Reader,
}

struct LayoutResult {
    chapter_id: usize,
    layout: Layout,
}

impl LayoutWorker {
    pub fn spawn() -> Self {
        let (tx, rx) = channel();
        let (result_tx, result_rx) = channel();
        thread::spawn(move || {
            while let Ok(req) = rx.recv() {
                let layout = layout(&req.chapter, req.width, &req.prefs);
                result_tx.send(LayoutResult { ... }).ok();
            }
        });
        Self { sender: tx, receiver: result_rx }
    }
}
```

**收益**：大章节排版不阻塞 UI。

### 阶段 6：页图缓存

```rust
// 新增：渲染好的页图缓存
pub struct PageImageCache {
    // 页号 → 渲染好的位图
    cache: LruCache<usize, Arc<PageImage>>,
    max_entries: usize,
}

pub struct PageImage {
    width: u32,
    height: u32,
    // 已光栅化的页数据
    data: Vec<u8>,
}

impl PageImageCache {
    pub fn get(&mut self, page: usize) -> Option<Arc<PageImage>> {
        self.cache.get(&page).cloned()
    }
    
    pub fn put(&mut self, page: usize, image: Arc<PageImage>) {
        self.cache.put(page, image);
    }
    
    /// 后台渲染下一页
    pub fn prefetch(&self, page: usize) { ... }
}
```

**收益**：滚动时直接贴图，不需要重新从 Layout 绘制。

---

## 四、架构分层

重构后的目标架构：

```
┌─────────────────────────────────────────────────────────┐
│  UI 层 (folio-app)                                      │
│  ReaderView → ReadingPage → paint → canvas              │
└────────────────────┬────────────────────────────────────┘
                     │
┌────────────────────┴────────────────────────────────────┐
│  缓存层 (新增)                                          │
│  LayoutCache / BlockCache / PageImageCache              │
│  RenderContext (hash 比较)                              │
└────────────────────┬────────────────────────────────────┘
                     │
┌────────────────────┴────────────────────────────────────┐
│  排版层 (folio-core/layout)                              │
│  layout() → Ctx::children() → block() / wrap_paragraph() │
│  → collect() → wrap_lines() → emit_line()               │
│  + light 模式 + Measure 批处理 + StyleArena             │
└────────────────────┬────────────────────────────────────┘
                     │
┌────────────────────┴────────────────────────────────────┐
│  度量层 (folio-core/layout)                              │
│  Measure trait (char_width / run_metrics)               │
│  + MeasureBatch (premeasure / measure_run)              │
└────────────────────┬────────────────────────────────────┘
                     │
┌────────────────────┴────────────────────────────────────┐
│  平台层 (folio-app/measure)                              │
│  GpuiMeasure → gpui TextSystem                          │
└─────────────────────────────────────────────────────────┘
```

---

## 五、实施优先级

| 优先级 | 改动 | 收益 | 风险 | 状态 |
|--------|------|------|------|------|
| **P0** | LayoutCache（宽+偏好 hash → Layout） | 改页高/模式切换不重排 | 低 | ✅ 已完成 |
| **P0** | light 模式（只算高不排版） | 目录/进度条快 10x | 低 | ✅ 已完成 |
| **P1** | Measure 批处理（`char_widths` + 宽度表） | 去掉逐字符二分+虚调用 | 中 | ✅ 已完成 |
| **P1** | StyleArena（样式去重） | Rc clone 71→~10 | 低 | ✅ 已完成 |
| **P2** | BlockCache（块级缓存） | 局部重排 | 中 | 待做 |
| **P2** | 后台排版线程 | 大书不卡顿 | 中 | 待做 |
| **P3** | 页图缓存 | 滚动零重排 | 高 | 待做 |
| **P3** | 磁盘缓存（序列化 Layout） | 重开书秒开 | 高 | 待做 |

### 已完成的实现细节

**P0 — LayoutCache**（`layout/mod.rs`）

```rust
pub struct LayoutCache { cached: Option<CacheEntry> }
// 键 = (width, prefs_hash, styles_hash, dom_hash, light)
pub fn layout_cached<'a>(cache: &'a mut LayoutCache, ...) -> &'a Layout
pub fn prefs_hash(prefs: &Reader) -> u64
pub fn styles_hash(styles: &StyledDom) -> u64
pub fn dom_hash(dom: &Dom, root: NodeId) -> u64
```

- `has()` / `get()` / `put()` / `invalidate()`
- `layout_cached()` 用 `has() → put() → get()` 三步避开借用冲突

**P0 — light 模式**

```rust
pub struct LayoutOptions { pub width: f32, pub light: bool }
// children_light() / block_light()：只算块高，不断行、不 run_metrics
// 文本块按「字符数 / 每行字数 × 行高」估算
```

**P1 — Measure 批处理**

```rust
pub trait Measure {
    fn char_width(ch, font, font_size) -> f32;          // 逐字符（断点判定）
    fn char_widths(text, font, font_size) -> Vec<f32>;  // 批量（默认逐字回落）
}

// Collected::widths: Vec<f32> —— 按字节下标索引的宽度表
// wrap_lines / span_width 从「逐字符二分 + 虚调用」变成一次数组取值
```

**P1 — StyleArena**

```rust
pub type StyleRef = u32;
pub struct StyleArena { styles: Vec<RunStyle>, index: HashMap<RunStyle, StyleRef> }
// Piece::style 从 Rc<RunStyle>（堆分配 + 引用计数）变成 4 字节下标
// RunStyle 手写 Hash（f32 字段走 to_bits）
```

---

## 六、与 KOReader 的差异

| 特性 | KOReader | Folio 方案 |
|------|----------|------------|
| 块缓存 | `_renderedBlockCache` 256 条 LRU | `BlockCache` HashMap |
| 页表缓存 | `_pagesData` 序列化 | `LayoutCache` |
| 磁盘缓存 | `swapToCache()` 整书缓存 | 暂不做（SQLite 已缓存章节文本） |
| 后台绘制 | `LVDrawThread` + `cachePageImage` | `PageImageCache` + 后台线程 |
| 局部重排 | partial rerendering | BlockCache 局部失效 |
| 轻量模式 | `light_formatting` 标志 | `LayoutOptions.light` |
| 渲染参数 hash | `updateRenderContext()` | `RenderContext` |
| 字体管理 | `LVFontManager` + 回退 | gpui TextSystem |
| 图片缩放 | `img_scaling_options_t` | `shrink_image()` |

---

## 七、性能实测

用 `cargo run -p folio-core --release --example bench-layout` 量。语料是一章 1.4 MB、
1204 个顶层元素、排完 13200 行；度量是「汉字 1 em、拉丁 0.5 em」的假度量（与测试同源）。

| 场景 | 实测 | 说明 |
|------|------|------|
| 完整排版 | 20.8 ms | 13200 行 |
| 轻量模式 | 0.8 ms | **快 25x**，高完全一致 |
| 缓存命中 | ~0 ms | **快 2.7M x**（纯 hash 比较） |
| 缓存未命中 | 20.9 ms | 等于一次完整排版 |

**注意假度量的局限**：`char_width` 只是算术，所以「批处理 vs 逐字符」在它身上
看不出优势（1.0–1.1x）。真字体下 `GpuiMeasure::char_width` 要
`resolve_font` + `advance` 两次查表，比这里贵得多。

### 各项优化的真实收益

| 优化 | 假度量 | 真字体预估 | 依据 |
|------|--------|------------|------|
| light 模式 | 25x | 同量级 | 跳过断行与 shaping，与字体无关 |
| LayoutCache | 2.7M x | 同量级 | 纯 hash 比较，与字体无关 |
| StyleArena | 减少堆分配 | 同量级 | `Rc<RunStyle>` → `u32` 下标 |
| 宽度表 | 1.1x | 更大 | 真 `char_width` 两次查表；消除二分 |
| `char_widths` 批处理 | 默认实现无优势 | 大 | 待 app 侧用一次 shaping 覆盖 |

### 下一步：app 侧覆盖 `char_widths`

当前 `GpuiMeasure` 没覆盖 `char_widths`，所以走默认的逐字回落（`resolve_font` +
`advance` 各一次/字）。用一次 `shape_line` 拿整段前进量可以再提一个量级：

```rust
// folio-app/src/ui/features/reader/measure.rs
impl Measure for GpuiMeasure {
    fn char_widths(&self, text: &str, font: &FontKey, font_size: f32) -> Vec<f32> {
        // 一次 shaping，按字形位置差分出每个字符的前进量。
        // 与 `run_metrics` / `byte_at_x` 同源，不会出现「折行算得下、画出来超出」。
        ...
    }
}
```

真字体下这是 P1 里收益最大的一项，但它要改 app 侧，所以留在前端那一步做。

### 原预期（作为对照）

| 场景 | 优化前 | 优化后 |
|------|------|--------|
| 改字号 | 全章重排 ~200ms | 缓存命中 ~0ms |
| 切分页/连续 | 全章重排 ~200ms | 只重分片 ~5ms |
| 目录跳转 | 完整排版 ~200ms | light 模式 ~20ms |
| 滚动（已渲染页） | 每帧从 Layout 绘制 | 页图缓存直接贴图 |
| 大书（1000 章）打开 | 同步阻塞 | 后台线程 |
| 内存（Rc clone） | 71 次/章 | StyleArena 去重后 ~10 次 |

---

## 八、Tier 1 实施结果（已落地）

基准：`cargo run -p folio-core --release --example bench-layout`，每批 20 次取平均、
30 批取最快、`taskset` 固定核、9 轮取中位数。语料：

- **中文**：600 段 × 约 800 汉字 → 13200 行
- **拉丁**：300 段 × 约 2400 字符英文 → 8700 行
- **混排**：中文 300 段 + 拉丁 150 段 → 10950 行

### 结论：累计 -4% ~ -13%

| 场景 | 基线 | 1.2 | 1.2+1.4 | 1.2+1.4+1.3 | 累计 |
|------|------|-----|---------|-------------|------|
| 中文 | 18.90 ms | 18.32 | 18.18 | **16.45** | **-13.0%** |
| 轻量 | 0.20 ms | 0.20 | 0.10 | **0.11** | **-45%** |
| 拉丁 | 19.76 ms | 19.31 | 18.98 | **18.92** | **-4.3%** |
| 混排 | 19.86 ms | 19.40 | 19.19 | **17.78** | **-10.5%** |

### 逐项记录

| # | 改动 | 结果 |
|---|------|------|
| 1.1 | 断行改二分（预算前缀和） | ❌ **回退**。前缀表的构造成本（两次全量遍历 + 两个 `Vec`）超过二分省下的；线性扫描是顺序访问，二分是随机访问，缓存不友好。中文 +3%、表构造 +8 ms |
| 1.2 | `line_inset` 提到断行循环外 | ✅ **-2.5%**。原来每字符重算一次缩进与保留宽 |
| 1.3 | `RunStyle.font` 用 `Arc<FontKey>` | ✅ **-5~8%**。`RunStyle` 里唯一带分配的字段；每行每 run clone 一次（一章一万多次），包 `Arc` 后 clone 是引用计数 |
| 1.4 | `style_of` / `children_of` 返回引用 | ✅ **-1~2%，轻量模式 -50%**。`Ctx.styles` / `Ctx.dom` 都是 `&'a`，返回的引用不借用 `&self`，所以能跨 `&mut self` 调用持有；省掉每章几千次 `ComputedStyle`（含 `String` + `Vec<String>`）与 `Vec<NodeId>` 克隆 |
| — | 断行内层循环拆分（去掉 `cap` 判断 + `wslice`） | ❌ **回退**。中文 +1%、拉丁 -1%，净零且代码更复杂 |
| — | `compute_widths` 内层去 `word_spacing` 分支 | ❌ **回退**。中文 +2.4%，在噪声带外但方向相反，不采用 |

### 未采用但已分析清楚的

**1.5 每段临时 Vec 复用**（`plans` / `breaks` / `hard_lines`）：一章约 2000 次小分配，
估 0.05 ms，**收益太小，不做**。

**1.6 `LineFragment` 扁平化**：省掉 13200 次 `Vec<LineFragment>` 分配（每次 ~25 ns），
估 0.33 ms ≈ 2%，但要改 `LineBox.fragments` 的公开 API（绘制、命中、测试全要动），
**性价比不够，留待以后**。

### 当前阶段的瓶颈（插桩实测，相对占比）

| 阶段 | 中文 | 拉丁 |
|------|------|------|
| `collect`（DOM 展平 + 空白折叠） | 20% | 33% |
| `compute_widths`（预算宽度表） | 19% | 20% |
| `wrap_lines`（断行扫描） | 44% | 36% |
| `emit_line`（建行盒） | 15% | 11% |

**关键观察：全书字符被完整遍历三遍**（`collect` 折叠空白、`compute_widths` 量宽、
`wrap_lines` 判断点）。下一步的大头是把这三遍合并，或让 `wrap_lines` 不再遍历字符。

### 下一步的大方向（Tier 2）

1. **合并三遍遍历**：在 `collect` 里同时产出宽度表（需要把 `measure` 传进 `collect`）。
   代价是失去 `char_widths` 的批量接口 —— 但对默认实现来说本来就是逐字调用，不亏。
2. **词级断行**（KOReader 的做法）：拉丁文按词测量与排布，迭代次数降一个量级。
   CJK 仍需逐字。
3. **`char_widths` 用 shaping 覆盖**（app 侧 `GpuiMeasure`，见 §七）。
4. **块级排版缓存** / **两阶段排版**（KOReader 的 `CVRendBlockCache` / `is_reusable`）。
