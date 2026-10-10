# 排版引擎优化报告

> 状态：Tier 1 已完成；§7.1–§7.5 已完成（§九）；§7.6 页位图、§7.7 磁盘缓存待决策。
> 本文档是**唯一权威**的优化记录 —— 包含做成的、没做成的、以及为什么。
> 配套基准：`cargo run -p folio-core --release --example bench-layout`。

---

## 一、摘要

### 做了什么

对照 KOReader（crengine）的排版架构，对 `folio-core/src/layout` 做了两轮优化：

1. **P0/P1（已完成）**：`LayoutCache`（整章布局缓存）、`light` 模式（只算高）、
   `StyleArena`（样式去重）、宽度表（批量度量）。
2. **Tier 1（已完成）**：`line_inset` 提循环外、`RunStyle.font` 用 `Arc`、
   `style_of` / `children_of` 返回引用。

### 实测结果（Tier 1）

| 场景 | 起点 | 现在 | 提升 |
|------|------|------|------|
| 中文（13200 行） | 18.90 ms | **16.45 ms** | **-13.0%** |
| 混排（10950 行） | 19.86 ms | **17.78 ms** | **-10.5%** |
| 拉丁（8700 行） | 19.76 ms | **18.92 ms** | **-4.3%** |
| 轻量模式（只算高） | 0.20 ms | **0.11 ms** | **-45%** |
| 缓存命中 | ~0 ms | ~0 ms | — |

**444 个测试通过，0 编译警告。**

### 试过但失败的（同样重要）

| 想法 | 为什么失败 |
|------|------------|
| 断行改二分（预算前缀和） | 前缀表构造成本 > 二分省下的；线性扫描是顺序访问，二分是随机访问 |
| 断行内层循环拆分 | 净收益≈0，代码更复杂 |
| `compute_widths` 去 `word_spacing` 分支 | 中文 +2.4%（方向相反） |
| 合并三遍遍历 | 明细见 §五.4，初步实现 +12%，修掉一个坑后仍未能胜过现有版本 |

### 当前状态

工作区里 `layout/mod.rs` 是 §5.4 的**合并遍历版**。原文建议回退到两趟版，但
**2026-10-10 在本机复测推翻了这条结论**：合并版在拉丁/混排上明显更快，故**保留合并版**
（详见 §九.1）。原「比最优慢 6%」是另一台机器上的结果，机器相关。

---

## 二、基准方法

优化结论全靠这把尺子，所以先把方法说清楚。

### 代码

`crates/folio-core/examples/bench-layout.rs`：

```bash
cargo run -p folio-core --release --example bench-layout
# 更稳：taskset -c <核> ./target/release/examples/bench-layout
```

### 方法

| 项 | 做法 | 理由 |
|----|------|------|
| 每批次数 | 20 次 | 批内取平均，摊平单次中断 |
| 批数 | 30 批 | 取**最快的批** —— 有别的负载时只会变慢，最快的最接近真实速度 |
| 预热 | 3 批 | 让 CPU 频率与分支预测稳定 |
| 重复 | 跑 9 次进程取中位数 | 跨进程仍有余噪，中位数抗离群 |
| 绑核 | `taskset -c 2` | 避免迁移到不同物理核 |
| 度量 | 假度量（汉字 1 em、拉丁 0.5 em、空格 0.25 em） | 与 `layout::tests` 同源，两次改动可比；不是真字体，但**两种实现同源**，比值可信 |

**稳定性**：同版本重复测量偏差 < 1.5%。改动小于 2% 的差异不可靠，本文档中凡
标注「净收益≈0」的都是在这个带内。

### 语料

| 名称 | 构成 | 排完行数 |
|------|------|----------|
| 中文 | 600 段 × 约 800 汉字 | 13200 |
| 拉丁 | 300 段 × 约 2400 字符英文 | 8700 |
| 混排 | 中文 300 段 + 拉丁 150 段 | 10950 |

拉丁章字符数是中文的 1.5 倍，所以拉丁的绝对耗时更高。

### 为什么假度量也能说明问题

`Mock` 的 `char_width` 是纯算术，比真字体便宜得多。所以：

- **与字体无关的优化**（内存分配、遍历次数、缓存）→ 假度量下的比值**等于**真实比值
- **与字体相关的优化**（`char_width` 调用次数）→ 假度量**低估**收益，真字体下更大

`light` 模式、`LayoutCache`、`StyleArena`、`Arc` 都在第一类。

---

## 三、KOReader（crengine）借鉴

参考实现：`reference/koreader/base/thirdparty/kpvcrlib/crengine/`。

### 渲染流水线对比

```
KOReader                                      Folio（现在）
─────────────────────────────────────────     ────────────────────────────────
LVDocView::Render(dx, dy)                     ui::reader::Document::ensure_viewport()
  │                                             │
  ├─ checkRenderContext()  ← hash 比较          ├─ 键比较（宽、偏好、样式、DOM）
  │   └─ 没变 → 反序列化 _pagesData 直接返回     │   └─ 没变 → 返回缓存        ✅ LayoutCache
  │                                             │
  ├─ dropStyles() + applyDocumentStyleSheet()   ├─ cascade()
  ├─ initNodeStyleRecursive()  ← 每节点算样式    │   （Folio 是整体算，见 §七.2）
  ├─ initNodeRendMethodRecursive()              │
  │                                             │
  ├─ renderBlockElement(root)                   ├─ layout() → Ctx::children()
  │   └─ renderFinalBlock() 每块                │   └─ Ctx::block() 每块
  │       ├─ _renderedBlockCache 命中？          │       （无块级缓存）        ❌
  │       ├─ light_formatting？ 只算高            │       （light 是整章静态开关）❌
  │       └─ LFormattedText::Format()           │       └─ wrap_paragraph()
  │           └─ measureText() 全段一次 shaping  │           ├─ collect()
  │               → m_widths[] 逐字累计宽        │           ├─ compute_widths()
  │                                             │           └─ wrap_lines()
  ├─ context.Finalize() 分页                    ├─ Fragmentainer
  ├─ pages->serialize(_pagesData)               │   （无序列化）             ❌
  └─ updateRenderContext()                      └─ 存缓存键
```

### KOReader 的关键机制

| 机制 | 位置 | 作用 | Folio 是否已借鉴 |
|------|------|------|------------------|
| `_renderedBlockCache`（256 条 LRU） | `lvtinydom.cpp:220` | 每块的 `LFormattedText` 缓存，改一块只重排一块 | ❌ |
| `checkRenderContext()` | `lvtinydom.cpp:5598` | 渲染参数 hash 比较，不变就跳 | ✅ `LayoutCache` |
| `light_formatting` + `is_reusable` | `lvtextfm.cpp:3340, 5946` | render 阶段只算高，draw 阶段对可见页做完整排版 | ❌（Folio 的 light 是静态的） |
| `m_widths[]`（全段累计宽） | `lvtextfm.cpp:428` | `measureText` 一次 shaping 出全段逐字宽度，行宽 = 累计差 | ✅ 宽度表（但每段调用方式不同） |
| `static` 缓冲复用 | `lvtextfm.cpp:419, 2005` | 避免每段 malloc | ❌（见 §六.2） |
| `cachePageImage()` + `LVDrawThread` | `lvdocview.cpp:758, 706` | 后台线程把相邻页渲染成位图 | ❌ |
| cache file（DOM + pages 序列化） | `lvtinydom.cpp:104` | 重开书跳过解析、层叠、排版 | ❌ |
| 词级断行 | `lvtextfm.cpp` 全程 | 按词测量与排布，拉丁文迭代次数降一个量级 | ❌（Folio 是字符级） |

### 最值得借鉴的三件事

1. **两阶段排版**（`is_reusable`）—— 这是 KOReader 首屏快的根因。render 阶段遍历全书
   只算高度喂给分页器；draw 阶段只对**可见页**涉及的块做完整排版。Folio 现在要么整章
   完整排版，要么整章只算高（且只算高时拿不到行盒）。
2. **块级缓存** —— Folio 的 `LayoutCache` 是整章粒度，改一处全章重排。
3. **词级断行** —— Folio 逐字符扫，拉丁文上比 KOReader 多 5 倍迭代。

---

## 四、已完成的优化（Tier 1）

### 起点说明

本轮优化的**起点**是已含 P0/P1 的版本（`LayoutCache`、`light`、`StyleArena`、
宽度表）。本文档的「起点」列就是这个版本实测的。

### 4.1 `line_inset` 提到断行循环外 ✅

**问题**：`line_inset(cap, plan_index, first, indent)` 算的是「这一行要让出多少
缩进与保留宽」。它只跟「第几行」有关，一行之内不变，但原来**每个字符**都调一次。

**改动**（`wrap_lines`）：

```rust
// 前：每字符
for (i, _ch) in slice.char_indices() {
    let (left, reserved) = line_inset(&cap, plans.len(), first, indent);
    let avail = (width - left - reserved).max(1.0);
    ...
}

// 后：一行一次，换行时才更新
let mut inset = line_inset(&cap, plans.len(), first, indent);
let mut avail = (width - inset.0 - inset.1).max(1.0);
for (i, _ch) in slice.char_indices() {
    ...
    if at > line_start && used + w > avail {
        ...
        inset = line_inset(&cap, plans.len(), first, indent);
        avail = (width - inset.0 - inset.1).max(1.0);
    }
}
```

**实测**：中文 18.90 → 18.32（**-2.5%**），拉丁 -2.3%，混排 -2.3%。

**为什么有效**：省掉每个字符一次函数调用 + 两次 `Option` 解引用 + 一次浮点减法。

### 4.2 `RunStyle.font` 用 `Arc<FontKey>` ✅

**问题**：`RunStyle` 里唯一带**堆分配**的是 `font: FontKey { family: String,
families: Vec<String>, .. }`。而每行每个 run 都要 `style.clone()` 一份进
`LineRun`（一章一万多次）。一次 clone = 一个 `String` 分配 + 一个 `Vec` 分配
+ 内部 `String` 分配，约 60 ns。

**改动**：

```rust
pub struct RunStyle {
    // 前：每次 clone 深拷贝字体家族链
    // pub font: FontKey,
    pub font: Arc<FontKey>,   // 后：clone 只是引用计数 +1
    ...
}

impl RunStyle {
    pub fn new(style: &ComputedStyle) -> Self {
        Self { font: Arc::new(FontKey::of(style)), ... }
    }
}
```

用 `Arc` 而不是 `Rc`：`Layout` 要能跨线程（`Measure: Send + Sync`、将来后台排版）。

**实测**：中文 18.18 → 16.45（**-9.5%**），混排 19.19 → 17.78（**-7.3%**）。
拉美几乎不变（-0.2%）—— 拉丁章的行数少、每行 run 少，这项收益按 run 数走。

**API 影响**：`RunStyle.font` 的类型变了，但 `Arc<T>` 自动解引用，所以
`run.style.font.weight`、`&run.style.font` 传给 `&FontKey` 参数都照常工作。
`FontKey::candidates()` 签名不变。`candidates()` 的调用方（app 侧）零改动。

### 4.3 `style_of` / `children_of` 返回引用 ✅

**问题**：

```rust
fn style_of(&self, node: NodeId) -> ComputedStyle {
    self.styles.get(node).cloned().unwrap_or_default()   // 深拷贝：String + Vec<String>
}
fn children_of(&self, node: NodeId) -> Vec<NodeId> {
    self.dom.get(node).children.clone()                  // 分配一个 Vec
}
```

一章 600 段时，`style_of` 约 6000 次、`children_of` 约 3600 次。
实测单价：`ComputedStyle::clone()` 61 ns、`Vec<NodeId>::clone()`（1200 项）97 ns。

**改动**：

```rust
// Ctx<'a> { dom: &'a Dom, styles: &'a StyledDom, ... }
fn style_of(&self, node: NodeId) -> &'a ComputedStyle {
    self.styles.get(node).unwrap_or(&DEFAULT_STYLE)
}
fn children_of(&self, node: NodeId) -> &'a [NodeId] {
    &self.dom.get(node).children
}
```

**关键点**：返回的引用生命周期是 `'a`（绑在 `Ctx` 的两个 `&'a` 字段上），
**不借用 `&self`**。所以拿到引用之后还能继续调 `&mut self` 的方法 —— 排版每一层
都要这么做（先读子节点样式，再递归排子节点）。这是能改成引用的前提。

`DEFAULT_STYLE` 用 `LazyLock<ComputedStyle>` 兜底；实际上 `cascade` 为每个节点
（含文本节点）都算了一条，查不到只可能是下标越界。

顺带把 `Collector::intern_style` 的克隆也去掉了（`RunStyle::new` 只读
`ComputedStyle`）。

调用点要改的地方：`for child in self.children_of(..)` → `for &child in ...`，
`self.is_inline_flow(child, &style)` → `(child, style)`。

**实测**：完整排版再 -1~2%；**轻量模式 0.20 → 0.11 ms（-45%）**。

轻量模式收益特别大是意料之中的：它几乎只做 DOM 遍历与样式查询（不折行、不
shaping），所以「每次查样式都深拷贝」在它身上占比最高。

### 4.4 `light` 模式（P0，前一轮）

**改动**：`LayoutOptions { width, light }`。`light = true` 时走
`children_light` / `block_light`：不折行、不调 `run_metrics`、不建 `LineFragment`，
文本块按「字符数 ÷ 每行字数 × 行高」估高。

**实测**：完整排版 16.45 ms 时轻量 0.11 ms —— **快 150 倍**，且高完全一致。

**用途**：目录跳转、进度条、以及将来两阶段排版的 render 阶段。

### 4.5 `LayoutCache`（P0，前一轮）

**改动**：

```rust
pub struct LayoutCache { cached: Option<CacheEntry> }
// 键 = (内容宽, 偏好 hash, 样式 hash, DOM hash, light)
pub fn layout_cached<'a>(cache: &'a mut LayoutCache, ..) -> &'a Layout
```

用 `has() → put() → get()` 三步实现，避开「返回 `&'a` 的同时还要 `&mut` 自己」
的借用冲突。

**实测**：命中约 0.000 ms（纯 hash 比较），未命中 20.9 ms。

**hash 成本**（单独测）：`dom_hash` 0.033 ms + `styles_hash` 0.003 ms +
`prefs_hash` 0.0001 ms = **0.036 ms/帧**（60 fps 预算 16.7 ms）。可以每帧算。

**已知缺陷**：`dom_hash` 只 hash 前 50 个子节点、每段文本前 64 个字符，
检测不完整。当前用法安全（换章显式 `invalidate`），但这是隐患，见 §六.4。

### 4.6 `StyleArena`（P1，前一轮）

**改动**：

```rust
pub type StyleRef = u32;
pub struct StyleArena {
    styles: Vec<RunStyle>,
    index: HashMap<RunStyle, StyleRef>,
}
// Piece::style: Rc<RunStyle>  →  StyleRef（4 字节下标）
```

一章里样式种类往往只有几种（正文、粗体、斜体、标题），去重后每个文本片段
只存 4 字节下标。`RunStyle` 手写 `Hash`（f32 字段走 `to_bits`），`Color` 加
`Hash` derive。

**为什么值得**：`Rc<RunStyle>` 每次克隆都要动引用计数，且每个 `RunStyle` 是独立
堆分配；去重后一份样式只建一次。

### 4.7 宽度表 / 批量度量（P1，前一轮）

**改动**：

```rust
pub trait Measure {
    fn char_width(ch, font, size) -> f32;           // 逐字符
    fn char_widths(text, font, size) -> Vec<f32> {  // 批量，默认逐字回落
        text.chars().map(|ch| self.char_width(ch, font, size)).collect()
    }
}
// Collected::widths: Vec<f32>，按字节下标索引（多字节字符的后续字节为 0）
```

`wrap_lines` 与 `span_width` 从「逐字符查 piece（二分）+ 调 `advance`」变成
一次数组下标取值。

**注意**：这一项**没有单独 A/B 测过**，它的收益与 §五.4 的「合并遍历」纠缠在一起。
当前它把「量宽」独立成一趟遍历（见 §六.1 的瓶颈分析）。

---

## 五、试过但失败的（避免重复踩坑）

### 5.1 断行改二分（预算前缀和）❌

**想法**：逐字符累加判溢出是 O(每行字符数)；若预先算出「前 k 个字符的累计宽」
（前缀和），找「能装下的最远字符」就是一次二分，O(log n)。

**实现**：`Collected` 加 `char_bytes: Vec<usize>`（字符序号 → 字节下标）与
`prefix: Vec<f32>`（累计宽），`cut_line()` 用 `partition_point` 二分。

**结果**：

| 场景 | 改前 | 改后 |
|------|------|------|
| 中文 | 16.70 ms | 17.56 ms（**+5%**） |
| 拉丁 | 19.76 ms | 18.83 ms（-4.7%） |
| 混排 | 19.14 ms | 17.76 ms（-7.2%） |

插桩显示 `compute_widths`（含建表）从「未单独计时」涨到 **7.97 ms**，
而 `wrap_lines` 只从 8.61 降到 8.42 —— **建表的两次全量遍历 + 两个 `Vec` 分配
把二分省下的全吃掉了**。

**根因**：建前缀和必须遍历全部字符，而原来的线性扫描也只遍历一次。工作量相同，
但线性扫描是**顺序访问**（硬件预取友好），二分是**随机访问**。对 CJK 这种
「每字都可断」的文本，二分没有任何额外好处。

**结论**：放弃。线性扫描已经是这个访问模式下的最优解。

### 5.2 断行内层循环拆分 ❌

**想法**：`cap`（首字下沉）在 99% 的段落里是 `None`，但循环里每个字符都要问一次
`cap_norm.is_some_and(...)`。拆成两条循环分支，让没有下沉的那条里彻底没有这个判断。
同时把 `collected.width_at(at)` 换成预先切好的 `wslice[i]`。

**结果**：

| 场景 | 改前 | 改后 |
|------|------|------|
| 中文 | 16.55 | 16.41（-0.8%） |
| 拉丁 | 18.94 | 19.44（**+2.6%**） |
| 混排 | 17.84 | 17.81（≈0） |

再去掉 `wslice`（改回 `width_at`）后：中文 16.35（-1.2%），拉丁 19.07（+0.7%）。

**结论**：净收益在噪声带内，且宏展开让代码更难读。放弃。

### 5.3 `compute_widths` 去掉 `word_spacing` 分支 ❌

**想法**：`word-spacing` 绝大多数书是 0（CSS 初值），但内层循环每个字符都要
`if ch == ' ' { w += spacing }`。拆成「有词距 / 无词距」两条分支，无词距那条
直接拷贝。

**结果**：中文 16.58 → 16.97（**+2.4%**），拉丁 -0.6%，混排 +0.8%。

**结论**：方向相反，放弃。原因不明 —— 可能是 `dst[offset]` 切片写法生成的代码
比原来的 `widths[piece.norm.start + offset]` 差。这也提醒：**在噪声带内的改动
要连续多轮验证**，不能凭直觉。

### 5.4 合并三遍遍历 ❌（当前工作区就是这个版本）

**动机**（最重要的一条发现）：插桩显示全书字符被**完整遍历三遍**：

| 阶段 | 在做什么 | 中文占比 | 拉丁占比 |
|------|----------|----------|----------|
| `collect` | DOM 展平 + 空白折叠（逐字符判 ASCII 空白） | 20% | 33% |
| `compute_widths` | 逐字符量宽（写宽度表） | 19% | 20% |
| `wrap_lines` | 逐字符累加判溢出 | 44% | 36% |

合并「collect 与 compute_widths」看起来是最自然的下一步：展平一个文本节点时
顺手把它的宽度算好。

**实现**：给 `collect` 传 `measure`，`Collector` 里加 `widths: Vec<f32>`，
在 `push_node` 里文本追加后立即调 `push_widths(start, style)`。

**第一次结果**：**全面退步** —— 中文 16.45 → 18.79（+14%），拉丁 +11%，混排 +12%。

**根因**：`push_widths` 最初对**每个字节**都 push（CJK 一字符三字节 → 3 次 push，
其中两次是补 0），而原来的 `vec![0.0f32; text.len()]` 是一次 `memset`（向量化）。
逐字节 push 的开销是它的 3 倍。

**修法**：一次 `resize` 到位（补零），再按字符起点写：

```rust
self.widths.resize(base + slice.len(), 0.0);
for (i, (offset, ch)) in slice.char_indices().enumerate() {
    self.widths[base + offset] = w;
}
```

**修复后结果**（3 轮，未跑满 9 轮）：

| 场景 | 起点 | 修后 |
|------|------|------|
| 中文 | 16.45 | 17.49 |
| 拉丁 | 18.92 | 19.13 |
| 混排 | 17.78 | 18.30 |

**仍然更慢（+6%）**。推测原因：合并后 `push_widths` 在**收集过程中**穿插调用
`char_widths`（每次分配一个 `Vec`），破坏了 `collect` 里的顺序写模式与预取；
而原来「先把整段文本建好，再一次性算宽」让两件事各自都是连续访问。

**结论**：**这条路走不通**（至少这个形式走不通）。合并遍历省下的「一趟遍历」
不足以抵消「交错访问」的代价。**当前工作区需要回退到 §四 的版本。**

可能的替代方向（未验证）：
- 让 `Measure::char_widths` 直接填进调用方给的缓冲区（避免每次分配 `Vec`），
  也许能改变交错访问的代价结构。
- 换思路：**在 `wrap_lines` 里直接量宽**（不建表），即彻底去掉 `compute_widths`，
  回到「边折行边量」。但那样就失去了 `char_widths` 的批量接口。

**⚠️ 2026-10-10 复测：本机结论相反（见 §九.1）。** 在同一台机器上做干净的 A/B
（先回退出两趟版量 3 轮，再还原合并版量 3 轮），合并版 **更快**：拉丁 -7.4%、混排 -4.9%
（约 2% 噪声内中文持平）。推测差异来自缓存：合并版把「量宽」融进展平，`char_widths`
读的文本刚写过、还在 cache 里；两趟版第二次遍历 1.4 MB 文本时开头已经凉了。
**这条「合并更慢」的结论是机器相关的，本机不成立 —— 已保留合并版。**

---

## 六、当前瓶颈与观察

### 6.1 三遍遍历（最大）

见 §5.4。三遍合计占 60~70%。合并失败，但**这个问题本身还在**。替代思路：
- 让 `char_widths` 写进调用方提供的 `&mut [f32]`（省掉每次 `Vec` 分配）
- 词级断行（§七.3）
- 把空白折叠从「逐字符」改成「按 run 处理」（`str::split_whitespace` 一类的批处理）

### 6.2 每段临时 Vec

`wrap_lines` 每段分配：`hard_lines()` 的 `Vec<Range>`、每个 hard line 的
`breaks: Vec<usize>`、`plans: Vec<LinePlan>`。600 段 × 3~4 处 ≈ 2000 次小分配。

**估算收益**：~0.05 ms（每次分配约 25 ns）。**太小，不做**。

### 6.3 `LineFragment` 每行一个 `Vec`

`LineBox.fragments: Vec<LineFragment>`，每行一次分配（13200 行）。

**估算收益**：~0.33 ms ≈ 2%。但要把 `LineBox.fragments` 改成
`Range<usize>` 指向 `Layout.fragments`，绘制、命中、测试全要动 —— **性价比不够**。

### 6.4 `dom_hash` 缓存失效不完整

```rust
for &child in dom_node.children.iter().take(50) { ... }   // 只 hash 前 50 个
let head: String = text.chars().take(64).collect();        // 每段文本只 hash 前 64 字
```

`<body>` 有 600 个子节点时，第 51 个之后的改动检测不到。当前用法里 DOM 只在
换章时变（且换章显式 `invalidate`），所以不出问题，但这是隐患。

**建议**：要么去掉上限（DOM 不变时 hash 只在失效后算一次，代价可接受），
要么在注释里写清「这是便宜的启发式，不保证完备」。

### 6.5 每段 `collected.text.clone()`

```rust
let index = self.push_block(owner, x, y, width, collected.text.clone());
```

`BlockBox.text` 需要一份归一化文本（`LineRun::norm` 索引它）。每段克隆一次
整段文本（600 段 × 2400 字节）。

**估算**：480K 字节拷贝 ≈ 0.05 ms。**小**。

---

## 七、后续方案（按预期收益排序）

### 7.1 两阶段排版（KOReader `is_reusable`）✅ 已完成（2026-10-10，见 §九.6）

**现状**：`light` 是**静态**开关 —— 要么整章完整排版（拿得到行盒），
要么整章只算高（拿不到行盒，分页都做不了）。

**KOReader 的做法**：
- **render 阶段**：遍历全书，每块只算高度（`requestLightFormatting()`），
  把高度喂给分页器 → 得到整本书的页表
- **draw 阶段**：只对**可见页**涉及的块做完整排版（对齐、shaping、装饰）

**对 Folio 的落地**：
1. `light` 遍历算出每块高 → 得到「第几页从第几行开始」（只需高度）
2. 打开某页时，只对那几页覆盖的块做完整 `layout()`
3. 需要「块级切分」的概念：现在 `layout()` 是整章的，要能按块增量算

**收益**：大书（1000 章）打开时只需 light 遍历 + 首屏几块的完整排版。
这是 KOReader 首屏快的根因。

**工作量**：大。要引入「块 → 行盒区间 → 页」的映射，以及「按块求高」与
「按块完整排版」两条路径。**风险高**。

### 7.2 块级排版缓存（KOReader `CVRendBlockCache`）✅ 已完成（2026-10-10，见 §九.4）

**现状**：`LayoutCache` 整章粒度，改一处全章重排。

**KOReader**：`LVCacheMap<ldomNode*, LFormattedTextRef>`（256 条 LRU），
键是节点指针；命中且 `isReusable()` 就直接复用。

**对 Folio 的落地**：`BlockCache { HashMap<(NodeId, u64 样式hash, u32 宽), CachedBlock> }`，
`CachedBlock { lines: Vec<LineBox>, height: f32 }`，LRU 上限。

**收益场景**：
- 批注高亮改变一个块的样式 → 只重排那个块
- 目录跳转后回到同一章 → 块缓存仍有效
- 改字号仍要全章重排（字号是继承的）

**工作量**：中。要在 `Ctx::block()` 里查缓存并处理 `LineBox` 的下标偏移
（因为行盒是扁平表，缓存块的行要重新编号）。

### 7.3 词级断行（KOReader 全程按词）✅ 已完成（2026-10-10，见 §九.2）

**现状**：`wrap_lines` 逐字符累加。拉丁文每行约 80 字符，其中只有约 15 个词。

**KOReader**：`measureWord()` 按词测量，行是「装得下几个词」的组合。
拉丁文迭代次数降约 5 倍。

**难点**：CJK 仍需逐字（每个汉字都是断点）。所以要**按脚本分流**：
拉丁文走词级，CJK 走字符级。

**收益**：拉丁章可能 -20~30%。中文无收益。

**工作量**：中。要引入「词」的概念（已有 `linebreaks` 给出的断点，可以把
「两个断点之间」当作一个词），并改写 `wrap_lines` 的主循环。

### 7.4 `char_widths` 用 shaping 覆盖（app 侧）✅ 已实现（真机待验，见 §九.3）

**现状**：`GpuiMeasure` 没覆盖 `char_widths`，走默认逐字回落
（`resolve_font` + `advance` 各一次/字）。一章 48 万字符 = 96 万次查表。

**改法**：用一次 `shape_line` 拿整段字形位置，差分出每个字符的前进量。

**收益**：真字体下大幅减少查表（假度量下看不出 —— 见 §二）。**这是所有
「与字体相关」优化里最大的一项。**

**工作量**：小（只改 `measure.rs` 一个方法），但要改 app 侧。

### 7.5 行宽用宽度表代替每行 shaping ✅ 已完成（2026-10-10，见 §九.5）

**现状**：`emit_line` → `LineAcc::push_text` 对每行每个 run 调一次
`run_metrics`（完整 shaping）。13200 行 × 1 run = 13200 次，实测 1.39 ms（6.2%）。

**KOReader**：行宽 = `m_widths[end-1]`（累计差），不重新 shaping。

**前提**：§7.4 先做（宽度表要 shaping 级准确，含 kerning）。
`ascent`/`descent` 按样式缓存即可（与文本无关，除非字体回退）。

**收益**：省掉 13200 次 shaping。

### 7.6 页位图缓存 + 后台线程（KOReader `cachePageImage` + `LVDrawThread`）

翻页时后台预渲染相邻页到位图，翻到时直接贴。**收益大但风险高**（线程、纹理
内存、与 gpui 的 sprite atlas 交互）。

### 7.7 布局磁盘缓存（KOReader cache file）

把排好的页表序列化到磁盘，重开书直接读。**收益大**（跳过一次排版），
但要设计版本兼容、失效策略、磁盘占用。

---

## 八、当前代码状态与建议动作

### 工作区状态

| 文件 | 状态 |
|------|------|
| `crates/folio-core/src/layout/mod.rs` | ✅ **保留合并遍历版**（§5.4）；新增 §7.3 词级断行（`scan_by_word` / `scan_by_char`）、§7.2 块级缓存（`BlockCache` / `layout_with_blocks` / `subtree_style_hash`）、§7.5 行宽走宽度表（`Measure::font_extents`）、§7.1 两阶段排版（`measure_chapter` / `render_window` / `text_est_em`） |
| `crates/folio-app/src/ui/features/reader/measure.rs` | ✅ §7.4 `char_widths` 用一次 shaping 覆盖 + §7.5 `font_extents`（按字体缓存）；**该文件尚未 `mod` 进 `features/reader/mod.rs`**，随应用层搬迁一起生效 |
| `crates/folio-core/src/css/computed.rs` | `ComputedStyle` 手写 `Hash`（§7.2 的键要它）；`StyledDom::len/iter`、`Display::Hash`、`Color::Hash`（P0/P1 需要，保留） |
| `crates/folio-core/src/validate.rs` | 格式校验（独立功能，保留） |
| `crates/folio-core/examples/bench-layout.rs` | 基准（保留） |
| `plans/layout-engine-refactoring.md` | 早先的方案文档（本文档取代它） |

### 建议动作

1. ~~回退 §5.4 的合并遍历~~ → **不采纳**：本机 A/B 证明合并版更快（§九.1），保留合并版。
2. ~~删除 `plans/layout-engine-refactoring.md`~~ → 暂留（尚未确认）。
3. §七 顺序：**§7.4 ✅**、**§7.3 ✅**（§九.2）、**§7.2 ✅**（§九.4）、**§7.5 ✅**（§九.5）、
   **§7.1 ✅**（§九.6）；剩 §7.6 页位图缓存、§7.7 布局磁盘缓存（都属高风险大工程）。

### 经验教训（写给下次）

1. **先建可靠基准再动手**。本轮一开始的测量噪声（±20%）差点让我得出「二分更快」
   的错误结论。改用「批内平均 + 批间取最快 + 绑核 + 多轮中位数」后偏差降到 1.5%。
2. **2% 以内的差异不可信**。§5.2、§5.3 都是在这个带内来回摆动。
3. **直觉会错**。「去掉一个分支」可能因为生成的代码布局变化而变慢（§5.3）；
   「合并两趟遍历」可能因为破坏顺序访问模式而变慢（§5.4）。
4. **插桩比猜测快**。§四、§五 的每一项结论都来自插桩，而不是读代码猜。
5. **写下来**。失败的方向和成功的一样重要 —— §五 避免重复踩坑。

---

## 九、2026-10-10 复测与 §7.3 / §7.4

本机：单核 `taskset -c 2`，每轮跑一轮基准（批内 20 次取平均、30 批取最快），
每项量 3~4 轮取值。机器与本文档早期测量**不是同一台**，所以绝对毫秒不能与 §一 直接比，
只做同机 A/B。

### 9.1 回退 §5.4：不采纳（本机合并版更快）

先按建议回退到两趟版（`collect` + 独立 `compute_widths`）量 3 轮，再还原合并版量 3 轮：

| 场景 | 两趟版（回退后） | 合并版（保留） | 差异 |
|------|------------------|----------------|------|
| 中文 | ~17.0 ms | ~17.2 ms | 噪声内 |
| 拉丁 | ~20.3 ms | **~18.85 ms** | **合并版快 7.4%** |
| 混排 | ~18.6 ms | **~17.96 ms** | **合并版快 4.9%** |

结论：§5.4「合并更慢」是**机器相关**的，本机相反 —— 保留合并版（§5.4 已加复测注记）。
合并版的 `sum_widths`（连续切片求和）也顺带成了 §7.3 的基础。
**444 个测试通过。**

### 9.2 §7.3 词级断行：完成，拉丁 -7.4%

改法（`layout/mod.rs`）：
- 抽出 `scan_by_char`（原逐字符扫描，`span_width` 换成连续的 `sum_widths` 切片求和）；
- 新增 `scan_by_word`：按「两个断点之间的词」整段判定，词宽用 `sum_widths` 一次求和，
  每词只做一次溢出判断；行首超长词返回位置交 `scan_by_char` 硬切（复现旧行为）；
- `wrap_lines` 每个硬行**按密度分流**：`cap.is_some()` 或稀疏度不足（`len < 4×词数`）
  走逐字符（CJK、带下沉盒），否则走词级（拉丁）。
  两条路径**输出逐行一致**，分流只影响性能。词首本身就是断点，所以「按词断」=
  原来的「按字符判溢出、断在最后一个断点」。

实测（相对 §9.1 的合并版基线）：

| 场景 | 合并版 | §7.3 后 | 差异 |
|------|--------|---------|------|
| 中文 | ~17.2 ms | ~16.77 ms | ~-2.4%（贴近噪声带） |
| 拉丁 | ~18.85 ms | **~17.46 ms** | **-7.4%** |
| 混排 | ~17.96 ms | **~17.08 ms** | **-4.9%** |
| 轻量 | 0.11 ms | 0.10 ms | — |
| 缓存命中 | 0.00 ms | 0.00 ms | — |

行数完全不变（13200 / 8700 / 10950），**446 个测试通过**（新增 2 条：拉丁按词断行、
超长词硬切）。中文也有小幅收益：断行重算 `used` 从逐字符循环换成切片求和，CJK 同样受益。
clippy 无新增告警（新函数带 `#[allow(clippy::too_many_arguments)]`；仓库里既有的
`&`-deref / 测试 `field_reassign` 等告警与本次无关）。

### 9.3 §7.4 app 侧 `char_widths`：已实现，真机待验

`GpuiMeasure` 现在覆盖 `char_widths`：复用 `char_edges` 的一次 shaping 边界，
相邻边界相减即每字符前进量（纯函数 `widths_from_edges` 可在宿主单测）。
基准走 `Mock`（默认逐字 `char_widths`），所以**这里量不到**它的收益 —— 与 §二、
§7.4 的判断一致：真字体下这是收益最大的一项，需真机验证。

**注意**：本次改动只落在 `crates/folio-app/src/ui/features/reader/measure.rs`；
`folio-app` 当前因未完成的模块搬迁（`view.rs` 仍 import 旧的 `shared::document` /
`shared::measure`）整体编不过，属既存 WIP，与本次无关，故未能跑 app 侧测试。

### 9.4 §7.2 块级缓存：完成，「改一段只重排一段」快 ~11×

落到三处：
- `css/computed.rs`：给 `ComputedStyle` 手写 `Hash`（覆盖**每个字段**，浮点走 `to_bits`）——
  块级缓存的键要按「这个块子树的样式」判命中，漏字段就会把过期结果当命中，所以逐字段写死。
- `layout/mod.rs`：`BlockCache`（键 = 块节点 + 子树样式哈希 + 宽度）、`subtree_style_hash`
  （后序聚合，一次 `layout_with_blocks` 算一遍）、`Ctx::block` 拆成「查/存缓存」+ `block_uncached`，
  以及入口 `layout_with_blocks(cache, …, dom_hash)`（`dom_hash` 变更时整清）。
- 命中时把缓存块按新落点平移后并入 `lines`/`blocks`（`blocks` 的行区间重新编号），
  所以**被改动块**下方的块整体下移也不影响命中。

键**不照搬 KOReader 的「256 条 LRU」**：一次整章重排是顺序扫过所有块，容量小于块数时
LRU 会在下一趟把每块都淘汰、命中率归零（实测 600 段落一章全未命中）。默认**不限容量**
（换章整清，上限即一章块数），`with_capacity` 才启用 LRU。

基准新增「改一段」场景（600 段落，只给第 300 段加字号）：

| 场景 | 耗时 | 说明 |
|------|------|------|
| 改一段后整章重排（无缓存） | ~17.5 ms | 整章 `LayoutCache` 键都变了 |
| 改一段（块级缓存） | **~1.6 ms** | **重排 1 块 / 命中 599 块**，约 **11×** |

单测（`layout::tests::block_cache_*`）钉住：命中且与直接排版逐行一致、只改一段时未命中
恰好 +1、DOM 变了整清、超容量 LRU 淘汰；`css::computed::tests::style_hash_covers_fields`
钉住样式哈希覆盖字段。**451 个测试通过**，clippy 无新增告警。

**尚未接的**：`folio-app` 还没有持有 `BlockCache` 的地方（那要等应用层把「批注改变某块样式」
做成一次带缓存的局部重排）；本轮只把引擎这一层与入口备好，正是 §7.2「为局部重排铺路」。

### 9.5 §7.5 行宽走宽度表：完成，~2–4%

改法（`layout/mod.rs`）：
- `Measure` 增加 `font_extents(font, size) -> (ascent, descent)`，默认给 0.8 / 0.2 em
  （与空行 strut 同一档，也正是 `Mock`/测试那档）。
- `LineAcc::push_text` 不再调 `run_metrics`：run 宽取宽度表 `sum_widths(norm)`
  （已含 `word-spacing`），上下 extent 取 `font_extents`。
- 删掉因此失去用处的 `RunStyle::extra_for_spaces`。
- `run_metrics` 保留：只在首字下沉（每段至多一次）里用。

实测（相对 §9.2 的 §7.3 基线）：

| 场景 | §7.3 后 | §7.5 后 | 差异 |
|------|---------|---------|------|
| 中文 | ~16.77 ms | **~16.44 ms** | ~-2.0% |
| 拉丁 | ~17.46 ms | **~16.76 ms** | ~-4.0% |
| 混排 | ~17.08 ms | **~16.63 ms** | ~-2.6% |
| 块级缓存「改一段」 | ~1.6 ms | ~1.6 ms | — |

拉丁收益最大：它的每行 run 最长，`sum_widths` 是连续 f32 切片求和（可向量化），
比 `run_metrics` 的逐字符累加便宜得多。行数不变，**451 个测试通过**。

**app 侧**：`GpuiMeasure` 覆盖了 `font_extents`（`Mutex<HashMap<(FontKey,u32),(f32,f32)>>`
按 `(字体,字号)` 缓存，量一个代表字符即可）。**但** `features/reader/measure.rs` 目前
**还没 `mod` 进 `features/reader/mod.rs`** —— 应用层的模块搬迁还没收尾，所以这一处改动
要等搬迁完成才真正编进去、生效。宿主侧已用 `rustfmt --check` 确认该文件可解析、格式干净。

### 9.6 §7.1 两阶段排版：完成，开章 ~19×

落到 `layout/mod.rs`，两个入口：
- `measure_chapter(...) -> ChapterMeasure`：阶段一，整章只算高（走 `light`），产出
  **顶层渲染单元表**（每个块节点的绝对 `x`/`y`/宽/估算高）+ 整章高 + `block_at(y)`。
- `render_window(&map, ..., from_y, to_y) -> Layout`：阶段二，只对与 `[from_y, to_y)`
  相交的顶层单元做**完整排版**。首个相交单元锚在测得的 `y` 上，其余按实际排出的高下推、
  保持章内间距 → 窗口内部自洽。顶层有匿名行内内容时退化为整章完整排版（`simple=false`）。

**先决条件：量高要准。** 原 `light` 是「字符数 ÷ 每行字数」，两个坑：
1. 用分数 `width/size` 当每行字数 → 行数系统性偏少（改成连续量，见下）；
2. 把「一字 1 em」当通例 → 纯拉丁段估宽一倍、行数翻番。改成**逐字节按脚本估宽**
   （`text_est_em`：ASCII 空格 0.25em、其余 ASCII 0.5em、3 字节字符 1em、2 字节 0.5em），
   以 0.25em 为单位整数累计，再 `ceil(总宽 / 版心宽)` 得行数。CJK 与拉丁都因此对得很准
   （基准里中文估高与完整**逐像素相等**）。

基准（中文章 13200 行）：

| 场景 | 耗时 | 对比 |
|------|------|------|
| 完整排版 | ~16.9 ms | — |
| 两阶段 · 整章量高 | **~0.83 ms** | 快 ~20× |
| 两阶段 · 首屏渲染（~800px） | **~0.05 ms** | 只排几段 |

**开一章 ≈ 量高 0.83 ms + 首屏 0.05 ms ≈ 0.9 ms**，对比一次性完整排版 16.9 ms → **~19×**。
这才对得上「KOReader 首屏快」的说法：整章不再是打开时的成本，只有测高 + 首屏是。

测试（`layout::tests::two_stage_matches_full_layout` /
`render_window_only_covers_the_visible_blocks`）钉住：`measure_chapter` 的高与
`render_window` 的行和一次性完整排版**逐行一致**，且窗口行是完整排版里的原样一段。

**代价与边界**：
- `light` 模式从 0.11 ms 涨到 ~0.83 ms（估宽从「数字符」变成「逐字节按脚本估」）。
  依然比完整快 ~20×，换来两阶段量高准确；纯 CJK 书若只想要最快的估高可选旧的字符计数。
- 阶段二的「锚定 y」来自阶段一的**估算**高。估算与真实逐像素相等时窗口严丝合缝；
  估偏时整窗会相对真实位置整体平移（内部仍自洽，不重叠）。行内图片、`<pre>` 等
  估算够不到的情形会让量高偏一点，属已知近似。
- **app 侧没接**：`folio-app` 目前编不过（同样的模块搬迁问题），所以 `measure_chapter` /
  `render_window` 还没被阅读器用起来 —— 引擎这一层与两个入口已备好。
