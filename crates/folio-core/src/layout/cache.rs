//! 增量排版的缓存与键：整章的 `LayoutCache`、块级的 `BlockCache`，以及 DOM 内容哈希。
//!
//! 从 `layout` 主模块切出来只为了一个文件能一次读完；跨文件的条目用
//! `pub(super)`，即**只对 `layout` 内部开放**，不属于对外公开面。
//!
//! 缓存的**失效键**不在这里定义，在 [`LayoutKey`](super::LayoutKey)：多级键必然漂，
//! 所以只有一把。

use std::collections::{HashMap, VecDeque};

use super::collect::DEFAULT_STYLE;
use super::geometry::{BlockBox, Layout, LayoutOptions, LineBox};
use super::key::LayoutKey;
use super::layout;
use super::ports::{ImageProvider, Measure};
use crate::css::computed::StyledDom;
use crate::dom::{Dom, NodeId};

/// 排版结果缓存 —— 「输入一模一样就什么都不用算」那一层，对应 KOReader 的
/// `checkRenderContext()`。
///
/// 键就是 [`LayoutKey`]（外加 `light` 那一档：轻量排版只算高、不产行盒，与完整排版
/// 是两个产物）。它与 [`BlockCache`] 互补：这一层问「整章要不要重排」，那一级问
/// 「这一块要不要重排」。
pub struct LayoutCache {
    cached: Option<CacheEntry>,
}

struct CacheEntry {
    key: LayoutKey,
    /// 轻量 / 完整：同一把键下两份产物不能互用。
    light: bool,
    layout: Layout,
}

impl LayoutCache {
    pub fn new() -> Self {
        Self { cached: None }
    }

    /// 检查缓存是否存在（不返回引用，避免借用冲突）。
    pub fn has(&self, key: &LayoutKey, light: bool) -> bool {
        self.cached
            .as_ref()
            .is_some_and(|entry| entry.key == *key && entry.light == light)
    }

    /// 取缓存。键不变时直接复用。
    pub fn get(&self, key: &LayoutKey, light: bool) -> Option<&Layout> {
        self.cached.as_ref().and_then(|entry| {
            if entry.key == *key && entry.light == light {
                Some(&entry.layout)
            } else {
                None
            }
        })
    }

    /// 存缓存。
    pub fn put(&mut self, key: LayoutKey, light: bool, layout: Layout) {
        self.cached = Some(CacheEntry { key, light, layout });
    }

    /// 失效（换章、改样式时调用）。
    pub fn invalidate(&mut self) {
        self.cached = None;
    }
}

impl Default for LayoutCache {
    fn default() -> Self {
        Self::new()
    }
}

/// 块级排版缓存的键：**块节点 + 该块子树的样式哈希 + 宽度**。
///
/// 子树样式哈希（[`subtree_style_hash`]）让「改一个块的样式」只改它自己的哈希，
/// 从而只失效那一个块（它的祖先虽然哈希也变了，但祖先重排时会逐个问子节点，
/// 没变的子节点照样命中）。宽度按位存，因为 `f32` 不能当 `HashMap` 键。
///
/// 键里**不含 DOM 内容、字体表与度量口径** —— 那三样是**整张缓存的复用范围**，
/// 由 [`BlockCache::context_rev`] 一次守住：它们任一变了，样式哈希没变也不能留下
/// （旧 `NodeId` 命中新 DOM 会排错章，旧字体表算出的宽度会与新字体对不上）。
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct BlockKey {
    pub(super) node: NodeId,
    pub(super) style_hash: u64,
    pub(super) width: u32,
}

/// 一块排好后的产物，坐标**相对该块边框盒左上角**（复用时要平移到新位置）。
///
/// `lines` 的区间在 `blocks` 里是相对上面这张 `lines` 表的（0 起）—— 复用时要整体
/// 加上落点偏移，`blocks` 的 `lines` 区间再平移到 `Layout::lines` 里。
pub(super) struct CachedBlock {
    pub(super) lines: Vec<LineBox>,
    pub(super) blocks: Vec<BlockBox>,
    pub(super) height: f32,
}

/// 块级排版缓存。对应 KOReader 的 `_renderedBlockCache`。
///
/// 与整章粒度的 [`LayoutCache`] 互补：整章缓存「什么都变了就全重排」，这一层
/// 「只有某些块的样式变了就只重排那些块」。批注高亮（改一个块的样式）就是它的主场。
///
/// **默认不限容量**（换章时 [`clear`](Self::clear) 整清，所以上限就是一章的块数）。
/// 这里刻意不照搬 KOReader 的「256 条 LRU」：一次整章重排是**顺序扫过所有块**，只要
/// 容量小于块数，LRU 就会在下一趟把每块都淘汰掉 —— 命中率归零（实测 600 段落一章
/// 全未命中）。想限容量用 [`with_capacity`](Self::with_capacity)，那时才启用 LRU。
pub struct BlockCache {
    entries: HashMap<BlockKey, CachedBlock>,
    /// LRU 顺序，最近用过的在末尾。只在**限容量**时维护（不限容量时永不淘汰）。
    order: VecDeque<BlockKey>,
    /// `None` = 不限容量。
    cap: Option<usize>,
    /// 这份缓存是为哪个**复用范围**建的（[`LayoutKey::context_rev`]）；对不上就整清。
    /// 复用范围 = 章文本 + 图片索引 + 字体集 + 度量版本，见 [`BlockKey`] 的说明。
    context: Option<u64>,
    /// 命中 / 未命中次数，测试与基准用来核对「只重排了一块」。
    pub hits: u64,
    pub misses: u64,
}

impl BlockCache {
    /// 不限容量（推荐）：上限是一章的块数，换章时整清。
    pub fn new() -> Self {
        Self::with_capacity_opt(None)
    }

    /// 限容量（LRU 淘汰）。**注意**：容量小于一章块数时，整章顺序重排会来回淘汰、
    /// 命中率归零，所以只在「工作集确实小」时用。
    pub fn with_capacity(cap: usize) -> Self {
        Self::with_capacity_opt(Some(cap.max(1)))
    }

    fn with_capacity_opt(cap: Option<usize>) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            cap,
            context: None,
            hits: 0,
            misses: 0,
        }
    }

    /// 整清（换章 / 改 DOM 时调用）。命中计数保留，方便跨轮观察。
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.context = None;
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 复用范围变了就整清。返回是否有过清理。
    ///
    /// 传 [`LayoutKey::context_rev`]，不要传 `dom_hash`：后者只盖住章文本那一格，
    /// 字体表或度量口径变了它看不出来，而那些变化下留下的块是按旧口径算的宽度。
    pub(super) fn sync_context(&mut self, context_rev: u64) -> bool {
        if self.context != Some(context_rev) {
            self.entries.clear();
            self.order.clear();
            self.context = Some(context_rev);
            true
        } else {
            false
        }
    }

    fn touch(&mut self, key: &BlockKey) {
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
        }
        self.order.push_back(*key);
    }

    /// 取一块（克隆一份给调用方）；限容量时顺带把它挪到 LRU 末尾。
    pub(super) fn get(&mut self, key: &BlockKey) -> Option<CachedBlock> {
        let out = self.entries.get(key).map(|entry| CachedBlock {
            lines: entry.lines.clone(),
            blocks: entry.blocks.clone(),
            height: entry.height,
        })?;
        if self.cap.is_some() {
            self.touch(key);
        }
        Some(out)
    }

    /// 存一块；限容量且超上限时从 LRU 头部淘汰。
    pub(super) fn put(&mut self, key: BlockKey, block: CachedBlock) {
        let fresh = self.entries.insert(key, block).is_none();
        let Some(cap) = self.cap else {
            return; // 不限容量：不维护 LRU，也不淘汰
        };
        if fresh {
            self.touch(&key);
        }
        while self.entries.len() > cap {
            match self.order.pop_front() {
                Some(old) => {
                    self.entries.remove(&old);
                }
                None => break,
            }
        }
    }
}

impl Default for BlockCache {
    fn default() -> Self {
        Self::new()
    }
}

/// 每个节点「自己 + 整棵子树」的样式哈希，供 [`BlockCache`] 做键。
///
/// 后序聚合：`h(node) = hash(own_style(node), h(child₁), h(child₂), …)`。用显式栈做后序，
/// 不依赖「子节点下标一定大于父节点」这种隐含假设（折叠/注入会不会破坏它不好保证）。
/// 一次 `layout_with_blocks` 算一遍，约等于多遍历一次 DOM —— 相对整章排版可忽略。
pub(super) fn subtree_style_hash(dom: &Dom, styles: &StyledDom) -> Vec<u64> {
    use std::hash::{Hash, Hasher};
    let mut preorder: Vec<NodeId> = Vec::with_capacity(dom.nodes.len());
    let mut stack = vec![dom.document];
    while let Some(node) = stack.pop() {
        preorder.push(node);
        for &child in &dom.get(node).children {
            stack.push(child);
        }
    }
    let mut out = vec![0u64; dom.nodes.len()];
    // 前序的**逆序**是一个合法后序：任一节点的子树在逆序里都排在它自己前面。
    for &node in preorder.iter().rev() {
        let mut hasher = std::hash::DefaultHasher::new();
        styles.get(node).unwrap_or(&DEFAULT_STYLE).hash(&mut hasher);
        for &child in &dom.get(node).children {
            out[child as usize].hash(&mut hasher);
        }
        out[node as usize] = hasher.finish();
    }
    out
}

/// 带缓存的排版入口：整章粒度的「输入没变就什么都不算」。
///
/// 键是传进来的 [`LayoutKey`]，不是这里现算的几个哈希 —— 各级缓存共用同一把键，
/// 才不会出现「块缓存认为没变、整章缓存认为变了」那种两处各答一遍的漂。
#[allow(clippy::too_many_arguments)]
pub fn layout_cached<'a>(
    cache: &'a mut LayoutCache,
    key: &LayoutKey,
    dom: &Dom,
    styles: &StyledDom,
    root: NodeId,
    options: LayoutOptions,
    measure: &dyn Measure,
    images: &dyn ImageProvider,
) -> &'a Layout {
    if !cache.has(key, options.light) {
        let layout = layout(dom, styles, root, options, measure, images);
        cache.put(*key, options.light, layout);
    }
    cache.get(key, options.light).expect("just put")
}
