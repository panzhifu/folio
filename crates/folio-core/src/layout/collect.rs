//! 收集与摆放：行内子树展平成段文本 + piece 表；块级顺序、兄弟外边距、缩进与对齐。
//!
//! 从 `layout` 主模块切出来只为了一个文件能一次读完；跨文件的条目用
//! `pub(super)`，即**只对 `layout` 内部开放**，不属于对外公开面。

use std::ops::Range;
use std::sync::LazyLock;

use unicode_linebreak::linebreaks;

use super::cache::{BlockCache, BlockKey, CachedBlock};
use super::font::{RunStyle, StyleArena, StyleRef};
use super::geometry::{BlockBox, ImageRun, LineBox, LineFragment, LineRun};
use super::ports::{ImageProvider, Measure, image_size};
use super::wrap::{
    DropCap, LineAcc, LinePlace, LinePlan, collect, hard_lines, hyphen_break_banned, orig_of,
    scan_by_char, scan_by_word, trim_end_spaces,
};
use crate::content::dom::{Dom, NodeId, NodeKind};
use crate::content::format::css::computed::{ComputedStyle, Display, StyledDom};

/// `style_of` 查不到时给的兵底样式。正常不会用到 —— `StyledDom` 为每个节点都算了一条。
pub(super) static DEFAULT_STYLE: LazyLock<ComputedStyle> = LazyLock::new(ComputedStyle::default);

pub(super) struct Ctx<'a> {
    pub(super) dom: &'a Dom,
    pub(super) styles: &'a StyledDom,
    pub(super) measure: &'a dyn Measure,
    pub(super) images: &'a dyn ImageProvider,
    /// 轻量模式：只算块高，不产出行盒。
    pub(super) light: bool,
    pub(super) blocks: Vec<BlockBox>,
    pub(super) lines: Vec<LineBox>,
    /// 块级缓存；`None` = 不做块级复用（[`layout`] 的默认路径）。
    pub(super) cache: Option<&'a mut BlockCache>,
    /// 每节点的子树样式哈希，与 `cache` 同生共死（见 [`subtree_style_hash`]）。
    pub(super) style_hash: Option<&'a [u64]>,
}

/// 一段行内内容展平后的产物。
pub(super) struct Collected {
    pub(super) text: String,
    pub(super) pieces: Vec<Piece>,
    /// 文本流里的图片占位。`at` 指向 `text` 里那个哨兵字符的字节起点。
    pub(super) images: Vec<ImageAtom>,
    /// 这一段用到的所有 run 样式，按值去重。`Piece::style` 是它的下标。
    pub(super) styles: StyleArena,
    /// 每个字符的前进量，按 `text` 的**字节下标**索引（非字符起点的字节是 0）。
    ///
    /// 断行与已用宽重算本来是逐字符做 `piece_at` 二分 + 一次 `char_width` 虚调用；
    /// 预算成这张表之后两处都只是一次数组取值。长度 = `text.len()`，与字节下标一一对应，
    /// 所以取的时候不必再判字符边界。
    pub(super) widths: Vec<f32>,
}

/// 一个文本节点对段落的贡献：段内区间、原节点区间、以及**该文本所属元素**的样式。
pub(super) struct Piece {
    pub(super) node: NodeId,
    pub(super) norm: Range<usize>,
    pub(super) orig: Range<usize>,
    /// 样式下标，指向 `Collected::styles`。
    ///
    /// 以前这里是 `Rc<RunStyle>`（一份样式一个堆分配 + 每次克隆一次引用计数）；
    /// 去重后同一样式只存一次，片段只占 4 字节。
    pub(super) style: StyleRef,
}

/// 段落里一张图片：锚定在文本流某个位置，宽度已按 CSS 与内在尺寸算好。
pub(super) struct ImageAtom {
    pub(super) node: NodeId,
    pub(super) href: String,
    pub(super) width: f32,
    pub(super) height: f32,
    /// 哨兵在 `Collected::text` 里的字节起点。
    pub(super) at: usize,
}

/// 行内图片在文本流里的占位字符。用它而不是另建 atom 流，是为了复用既有的
/// 字节区间机制（`linebreaks` / 切片 / 断行宽度累加全都按字节走）。
pub(super) const IMAGE_GLYPH: char = '\u{FFFC}';

impl Collected {
    /// 覆盖 `at` 的 piece 下标。落在 piece 之间的空白上时返回前一个 ——
    /// 那个空格本来就是从上一个元素带过来的。
    fn piece_at(&self, at: usize) -> Option<usize> {
        let idx = self.pieces.partition_point(|p| p.norm.start <= at);
        if idx == 0 {
            return None;
        }
        Some(idx - 1)
    }

    /// 按字节下标取它所属的样式。宽度表已经预先算好了前进量，所以断行不再走这里；
    /// 保留它给绘制、命中、`::first-letter` 那几条按位置查样式的路径。
    #[allow(dead_code)]
    pub(super) fn style_at(&self, at: usize) -> Option<&RunStyle> {
        let piece = self.piece_at(at)?;
        self.styles.get(self.pieces[piece].style)
    }

    /// 字节下标 `at` 处那个字符的前进量。超出表长给 0。
    ///
    /// 表在 [`collect`] 那一趟里已经填好了（见 [`Collector::push_widths`]），这里只读。
    pub(super) fn width_at(&self, at: usize) -> f32 {
        self.widths.get(at).copied().unwrap_or(0.0)
    }

    /// 正好落在 `at` 上的图片。
    #[allow(dead_code)] // 图片宽度已进 `widths`，这里留给命中/绘制路径
    fn image_at(&self, at: usize) -> Option<&ImageAtom> {
        let idx = self.images.partition_point(|im| im.at < at);
        self.images.get(idx).filter(|im| im.at == at)
    }
}

/// 把一行的片段排成**视觉顺序**。
///
/// RTL 时整列反转：逻辑上第一个片段（阅读起点）落到最右边。每个片段内部不再动 ——
/// 字形级的重排与镜像由绘制那一路的 shaping 负责（`cosmic-text` 每次 `shape_line`
/// 自己按整段跑 UAX #9），这里再动一次就是两处各排各的。
///
/// 这等价于「每个带样式的片段是一个嵌入盒、同一层的片段整组反序」，也就是 UAX #9 在
/// 一本书里最常见的那种情形：阿拉伯段落里插一个 `<span dir="ltr">2024</span>`，
/// 数字岛保持自己顺序、岛与岛之间随段落反序。已知不覆盖的是作者显式写
/// `unicode-bidi: isolate` / `bidi-override` 的那两档（我们没接那两个值）。
fn ordered_for_display(
    mut fragments: Vec<LineFragment>,
    direction: crate::content::format::css::computed::Direction,
) -> Vec<LineFragment> {
    if direction.is_rtl() {
        fragments.reverse();
    }
    fragments
}

impl<'a> Ctx<'a> {
    /// 节点的计算样式。
    ///
    /// 返回**引用**而不是克隆：`Ctx.styles` 是 `&'a StyledDom`，所以这个引用的生命周期
    /// 独立于 `&self` —— 拿到它之后还能继续调 `&mut self` 的方法（排版里每层都要这么干），
    /// 而克隆一份 `ComputedStyle`（内含 `String` + `Vec<String>`）在每章是几千次分配。
    ///
    /// `StyledDom` 为每个节点（含文本节点）都算了一条，所以查不到只可能是节点下标越界；
    /// 那种时候给一份静态默认样式，而不是 panic。
    pub(super) fn style_of(&self, node: NodeId) -> &'a ComputedStyle {
        self.styles.get(node).unwrap_or(&DEFAULT_STYLE)
    }

    /// 节点的子节点列表。同上：借出 `Dom` 里的那一份，不克隆 `Vec`。
    pub(super) fn children_of(&self, node: NodeId) -> &'a [NodeId] {
        &self.dom.get(node).children
    }

    /// 摆放 `container` 的流内子内容。`x`/`y` 是其内容盒左上角（绝对），返回内容高度。
    pub(super) fn children(&mut self, container: NodeId, x: f32, y: f32, width: f32) -> f32 {
        if self.light {
            return self.children_light(container, x, y, width);
        }
        let container_style = self.style_of(container);
        let mut cursor = y;
        let mut prev_bottom = 0f32;
        let mut pending: Vec<NodeId> = Vec::new();

        for &child in self.children_of(container) {
            let style = self.style_of(child);
            if style.display == Display::None {
                continue;
            }
            if self.is_inline_flow(child, style) {
                pending.push(child);
                continue;
            }
            // 遇到块级子节点，先把攒着的行内内容冲成一个匿名块。
            if !pending.is_empty() {
                let pieces = std::mem::take(&mut pending);
                cursor +=
                    self.wrap_paragraph(container, &pieces, x, cursor, width, container_style);
                prev_bottom = 0.0;
            }
            let ml = style.margin_left.max(0.0);
            let mr = style.margin_right.max(0.0);
            let mt = style.margin_top.max(0.0);
            // 兄弟之间折叠取较大值而不是相加：这是段间距不显松的唯一原因。
            cursor += if prev_bottom > 0.0 {
                mt.max(prev_bottom)
            } else {
                mt
            };
            cursor += self.block(child, x + ml, cursor, (width - ml - mr).max(1.0));
            prev_bottom = style.margin_bottom.max(0.0);
        }
        if !pending.is_empty() {
            cursor += self.wrap_paragraph(container, &pending, x, cursor, width, container_style);
        }
        cursor - y
    }

    /// 轻量模式：只算块高，不断行。
    ///
    /// 跳过 `wrap_paragraph` / `run_metrics` / `LineFragment` 构建。
    /// 文本块按「字符数 / 每行字数」估算行数，乘以行高得到高度。
    fn children_light(&mut self, container: NodeId, x: f32, y: f32, width: f32) -> f32 {
        let mut cursor = y;
        let mut prev_bottom = 0f32;

        for &child in self.children_of(container) {
            let style = self.style_of(child);
            if style.display == Display::None {
                continue;
            }
            let ml = style.margin_left.max(0.0);
            let mr = style.margin_right.max(0.0);
            let mt = style.margin_top.max(0.0);
            let mb = style.margin_bottom.max(0.0);
            cursor += if prev_bottom > 0.0 {
                mt.max(prev_bottom)
            } else {
                mt
            };

            let content_width = (width - ml - mr).max(1.0);
            let height = self.block_light(child, content_width);

            // 记录块（无行盒）。
            self.blocks.push(BlockBox {
                node: child,
                x: x + ml,
                y: cursor,
                width,
                height,
                text: String::new(),
                lines: 0..0,
                border_box: true,
            });
            cursor += height;
            prev_bottom = mb;
        }
        cursor - y
    }

    /// 走行内路径吗：文本节点、`<br>`、`display: inline` 都是。
    pub(super) fn is_inline_flow(&self, node: NodeId, style: &ComputedStyle) -> bool {
        self.dom.get(node).kind != NodeKind::Element
            || self.dom.is_element(node, "br")
            || style.display == Display::Inline
    }

    /// 直接子节点里有没有块级元素。只看一层：更深的嵌套由递归时重新判断。
    fn has_block_children(&self, node: NodeId) -> bool {
        self.children_of(node).iter().any(|&child| {
            let style = self.style_of(child);
            style.display != Display::None && !self.is_inline_flow(child, style)
        })
    }

    /// 摆放一个块级元素，返回边框盒高度。开了块级缓存时先查缓存、未命中才实排。
    pub(super) fn block(&mut self, node: NodeId, x: f32, y: f32, width: f32) -> f32 {
        let Some(key) = self.block_key(node, width) else {
            return self.block_uncached(node, x, y, width);
        };
        if let Some(height) = self.reuse_block(key, x, y) {
            return height;
        }
        let lines_start = self.lines.len();
        let blocks_start = self.blocks.len();
        let height = self.block_uncached(node, x, y, width);
        self.stash_block(key, lines_start, blocks_start, x, y, height);
        height
    }

    /// 这一块的缓存键；没开缓存时给 `None`（直接走实排）。
    fn block_key(&self, node: NodeId, width: f32) -> Option<BlockKey> {
        self.cache.as_ref()?;
        let style_hash = self.style_hash?.get(node as usize).copied().unwrap_or(0);
        Some(BlockKey {
            node,
            style_hash,
            width: width.to_bits(),
        })
    }

    /// 缓存命中：把缓存的块按 `(x, y)` 平移后并入 `self.lines`/`self.blocks`，返回块高。
    fn reuse_block(&mut self, key: BlockKey, x: f32, y: f32) -> Option<f32> {
        let cache = self.cache.as_deref_mut()?;
        let Some(entry) = cache.get(&key) else {
            cache.misses += 1;
            return None;
        };
        cache.hits += 1;
        let height = entry.height;
        let base = self.lines.len();
        for mut line in entry.lines {
            line.x += x;
            line.y += y;
            self.lines.push(line);
        }
        for mut block in entry.blocks {
            block.x += x;
            block.y += y;
            block.lines = (block.lines.start + base)..(block.lines.end + base);
            self.blocks.push(block);
        }
        Some(height)
    }

    /// 把刚排好的这一块（`self.lines`/`self.blocks` 的尾段）归一化成块局部坐标存进缓存。
    fn stash_block(
        &mut self,
        key: BlockKey,
        lines_start: usize,
        blocks_start: usize,
        x: f32,
        y: f32,
        height: f32,
    ) {
        let Some(cache) = self.cache.as_deref_mut() else {
            return;
        };
        let lines: Vec<LineBox> = self.lines[lines_start..]
            .iter()
            .map(|line| {
                let mut line = line.clone();
                line.x -= x;
                line.y -= y;
                line
            })
            .collect();
        let blocks: Vec<BlockBox> = self.blocks[blocks_start..]
            .iter()
            .map(|block| {
                let mut block = block.clone();
                block.x -= x;
                block.y -= y;
                block.lines = (block.lines.start - lines_start)..(block.lines.end - lines_start);
                block
            })
            .collect();
        cache.put(
            key,
            CachedBlock {
                lines,
                blocks,
                height,
            },
        );
    }

    
/// 摆放一个块级元素（不做块级缓存），返回边框盒高度。
    fn block_uncached(&mut self, node: NodeId, x: f32, y: f32, width: f32) -> f32 {
        let style = self.style_of(node);
        // 盒模型从外到内一次算清：边框与内边距一起从可用宽里扣掉，正文与折行都在内容盒里
        // 算；外层那条 `push_block` 记的仍是**边框盒**（绘制背景与边框要用它）。
        let [bt, br, bb, bl] = style.border_drawn();
        let pl = bl + style.padding_left.max(0.0);
        let pr = br + style.padding_right.max(0.0);
        let pt = bt + style.padding_top.max(0.0);
        let pb = bb + style.padding_bottom.max(0.0);
        // 替换元素（`display: block` 的图片）：整块就是这张图，独立成行。
        if self.dom.is_element(node, "img") {
            let src = self.dom.attr(node, "src").map(str::to_string);
            let Some(img) = src.as_deref().and_then(|s| self.images.resolve(s)) else {
                return 0.0; // 图取不到就整块不留痕，不塌成空盒子
            };
            let content_width = (width - pl - pr).max(1.0);
            let (w, h) = image_size(style, img.width, img.height, content_width);
            let index = self.push_block(node, x, y, width, String::new(), true);
            // 块级图片的对齐：`start`/`end` 先按这一块的 `direction` 折成物理左右，
            // 再算让出多少（`TextAlign::offset_in` 是全仓唯一一处这份算术）。
            let offset = style.text_align.offset_in(style.direction, content_width, w);
            self.lines.push(LineBox {
                owner: node,
                fragments: vec![LineFragment::Image(ImageRun {
                    node,
                    href: img.href,
                    width: w,
                    height: h,
                })],
                x: x + pl + offset,
                y: y + pt,
                width: w,
                height: h,
                baseline: h,
                align: style.text_align,
            });
            return self.close_block(index, pt + h + pb);
        }

        let content_width = (width - pl - pr).max(1.0);
        let index = self.push_block(node, x, y, width, String::new(), true);
        let inner = if self.has_block_children(node) {
            self.children(node, x + pl, y + pt, content_width)
        } else {
            let pieces = self.children_of(node);
            self.wrap_paragraph(node, pieces, x + pl, y + pt, content_width, style)
        };
        self.close_block(index, pt + inner + pb)
    }

    /// 轻量模式：算一个块的高，不断行。
    pub(super) fn block_light(&mut self, node: NodeId, width: f32) -> f32 {
        let style = self.style_of(node);
        let [bt, br, bb, bl] = style.border_drawn();
        let pl = bl + style.padding_left.max(0.0);
        let pr = br + style.padding_right.max(0.0);
        let pt = bt + style.padding_top.max(0.0);
        let pb = bb + style.padding_bottom.max(0.0);

        // 替换元素（图片）：按固有尺寸算高。
        if self.dom.is_element(node, "img") {
            let src = self.dom.attr(node, "src").map(str::to_string);
            if let Some(img) = src.as_deref().and_then(|s| self.images.resolve(s)) {
                let content_width = (width - pl - pr).max(1.0);
                let (_, h) = image_size(style, img.width, img.height, content_width);
                return pt + h + pb;
            }
            return 0.0;
        }

        let content_width = (width - pl - pr).max(1.0);
        let inner = if self.has_block_children(node) {
            // 有块级子节点：递归。
            self.children_light(node, 0.0, 0.0, content_width)
        } else {
            // 纯文本块：按**估算总宽**除以版心宽估行数。用总宽（连续量）而不是「每行字数」
            // 的离散近似 —— 两阶段排版据此定位可见块，行数偏一行就会错位。
            let est = self.text_est_em(node) * style.font_size;
            let line_px = (style.font_size * style.line_height).max(1.0);
            let lines = (est / content_width).ceil().max(1.0);
            lines * line_px
        };
        pt + inner + pb
    }

    /// 估算一个节点下文本的总宽，单位是 em（用于轻量模式 / 两阶段量高）。
    ///
    /// 逐**字节**给一个近似宽：ASCII 空格 0.25 em、其余 ASCII 0.5 em、3 字节字符（CJK 等）
    /// 1 em、2 字节字符 0.5 em、续字节不计。以 0.25 em 为单位**整数**累计，比逐字符解码 +
    /// 浮点求和便宜（一章 0.3 ms 量级）。不是真字体度量，但比「一字 1 em」准得多 ——
    /// 后者会把纯拉丁段估宽一倍、行数翻番，两阶段据此定位可见块就会错位。
    fn text_est_em(&self, node: NodeId) -> f32 {
        let dom_node = self.dom.get(node);
        if dom_node.kind == NodeKind::Text {
            let mut quarters = 0u32; // 0.25 em 为单位
            for &b in dom_node.text.as_bytes() {
                quarters += if b < 0x80 {
                    if b == b' ' { 1 } else { 2 }
                } else if b >= 0xE0 {
                    4
                } else if b >= 0xC0 {
                    2
                } else {
                    0 // 0x80..0xC0：UTF-8 续字节
                };
            }
            return quarters as f32 / 4.0;
        }
        // 有块级子节点时递归会在 block_light 里处理，这里只算直接文本。
        if self.has_block_children(node) {
            return 0.0;
        }
        dom_node.children.iter().map(|&c| self.text_est_em(c)).sum()
    }

    /// 把一段行内内容（块自己的子节点，或容器里混排的匿名块）摆成若干行盒。
    fn wrap_paragraph(
        &mut self,
        owner: NodeId,
        pieces: &[NodeId],
        x: f32,
        y: f32,
        width: f32,
        style: &ComputedStyle,
    ) -> f32 {
        let collected = collect(
            pieces,
            self.dom,
            self.styles,
            style.white_space_pre,
            self.images,
            self.measure,
            width,
        );
        let index = self.push_block(owner, x, y, width, collected.text.clone(), false);
        let height = self.wrap_lines(owner, &collected, x, y, width, style);
        self.close_block(index, height)
    }

    /// 先序入表：盒子必须在断行之前拿到自己的下标，否则 `lines` 区间无法回填。
    ///
    /// `border_box` 见 [`BlockBox::border_box`]：`block_uncached` 走 true，
    /// `wrap_paragraph` 的内容盒副本走 false。
    fn push_block(
        &mut self,
        node: NodeId,
        x: f32,
        y: f32,
        width: f32,
        text: String,
        border_box: bool,
    ) -> usize {
        let start = self.lines.len();
        self.blocks.push(BlockBox {
            node,
            x,
            y,
            width,
            height: 0.0,
            text,
            lines: start..start,
            border_box,
        });
        self.blocks.len() - 1
    }

    fn close_block(&mut self, index: usize, height: f32) -> f32 {
        let block = &mut self.blocks[index];
        block.height = height;
        block.lines = block.lines.start..self.lines.len();
        height
    }

    /// 断行。先只算出「哪些字节区间构成一行、该行让出多少缩进」，
    /// 再统一摆放 —— 分两趟是为了让 `&mut self.lines` 的写入不跟度量借用打架。
    fn wrap_lines(
        &mut self,
        owner: NodeId,
        collected: &Collected,
        x: f32,
        y: f32,
        width: f32,
        style: &ComputedStyle,
    ) -> f32 {
        if collected.text.is_empty() {
            return 0.0;
        }
        // 缩进最多吃掉半个版心，否则首行会被挤成零宽。
        let indent = style.text_indent.max(0.0).min(width * 0.5);
        let cap = self.drop_cap(collected, style, width);
        let mut plans: Vec<LinePlan> = Vec::new();
        let mut first = true;

        for hard in hard_lines(&collected.text) {
            let slice = collected.text.get(hard.clone()).unwrap_or("");
            // UAX #14：给出的下标是「下一行可以从这里开始」。
            let breaks: Vec<usize> = linebreaks(slice)
                .filter(|(ix, _)| *ix > 0 && *ix < slice.len())
                .map(|(ix, _)| hard.start + ix)
                // `hyphens: none` 的书不许在连字符处断，所以把那一批断点摘掉。
                .filter(|at| !hyphen_break_banned(&collected.text, *at, style.hyphens))
                .collect();
            // `cap` 在整段里都不变，先把「有没有」问一次，别每字符问。
            let cap_norm = cap.as_ref().map(|dc| dc.norm.clone());
            // 断点把硬行切成若干「词」（两个断点之间的连续段）：拉丁文里≈单词，
            // CJK 里退化成单字。稀疏（平均每词 ≥ 4 字节 —— 拉丁那种词边界断行）走词级
            // 扫描，每个词只做一次溢出判断；密集（CJK 几乎每字一个断点）或带下沉盒时走
            // 逐字符。两条路径输出逐行一致，这个分类只影响性能。
            let sparse = !breaks.is_empty() && slice.len() >= 4 * (breaks.len() + 1);
            // 词级扫描碰到「行首超长词」时把位置交回来，由逐字符那段从那儿接着硬切。
            let fallback = if cap.is_none() && sparse {
                scan_by_word(
                    collected,
                    &cap,
                    &breaks,
                    hard.clone(),
                    width,
                    indent,
                    &mut first,
                    &mut plans,
                )
            } else {
                None
            };
            if cap.is_some() || !sparse || fallback.is_some() {
                scan_by_char(
                    collected,
                    &cap,
                    &cap_norm,
                    &breaks,
                    fallback.unwrap_or(hard.start),
                    hard.end,
                    width,
                    indent,
                    &mut first,
                    &mut plans,
                );
            }
        }

        let mut cursor = y;
        // 这个块的第一行行盒的下标 —— 首字符的基线要贴着它。
        let first_line = self.lines.len();
        for plan in plans {
            cursor += self.emit_line(
                owner,
                collected,
                plan.range,
                LinePlace {
                    x,
                    y: cursor,
                    width,
                    indent: plan.indent,
                    reserved: plan.reserved,
                },
                cap.as_ref(),
                style,
            );
        }
        // 下沉的字比它占的那几行还长时（`line-height` 给得很小），尾巴会伸到段落底下，
        // 下一个块就会压在那半截字上。把这一段过 hang 补进段落高 —— 行盒本身不动，
        // 正文的基线位置因此不受影响。
        if let Some(dc) = &cap {
            let baseline = self
                .lines
                .get(first_line)
                .map(|line| line.y + line.baseline)
                .unwrap_or(y);
            cursor = cursor.max(baseline + dc.descent);
        }
        cursor - y
    }

    /// 首字下沉那一段的几何。三个条件缺一不可：这个块写过 `::first-letter`、
    /// 第一个硬行里找得到一个**字母单元**、并且量得出宽度。字母单元按 UAX #29 的说法
    /// 算（数字也算 —— 浏览器给 `1984` 的 `::first-letter` 上的就是那个 `1`），
    /// 全段一个字母单元都没有（纯标点、纯图片）就不造这个盒。
    ///
    /// 跨几行按字形的实际上下 extent 除以块自己的行盒高向上取整：用 extent 而不是
    /// inline box 高，是因为 `line-height: 0.9`（首字下沉的经典写法）会把盒高压到字形
    /// 以下，少算一行就让下一行的正文压在字的尾巴上。
    fn drop_cap(
        &self,
        collected: &Collected,
        style: &ComputedStyle,
        width: f32,
    ) -> Option<DropCap> {
        let cap = style.first_letter.as_deref()?;
        let head = collected.text.split('\n').next()?;
        let at = head
            .char_indices()
            .find(|(_, ch)| ch.is_alphabetic() || ch.is_ascii_digit())?
            .0;
        let end = at + head[at..].chars().next()?.len_utf8();
        // 下沉盒从段首起：紧贴首字母的标点按 CSS 也算在这个盒里面（`「開」` 的引号），
        // 而它们本来就被这一段涵盖了，不必再往回找一遍。
        let norm = 0..end;
        let text = collected.text.get(norm.clone())?;
        let mut run = RunStyle::new(cap);
        // 首字符就贴在第一行的基线上，`vertical-align` 对它没有意义；那个数会把
        // 绘制基线推走，所以清掉。
        run.baseline_shift = 0.0;
        let m = self.measure.run_metrics(text, &run.font, run.font_size);
        let line_px = (style.font_size * style.line_height).max(1.0);
        Some(DropCap {
            width: m.width.min(width * 0.6).max(1.0),
            ascent: m.ascent,
            descent: m.descent,
            lines: (((m.ascent + m.descent) / line_px).ceil() as usize).max(1),
            norm,
            run,
        })
    }

    /// 把 `[start, end)` 收成一个行盒，返回它的高度。
    ///
    /// 落位的四个数打包成 [`LinePlace`]：折行只决定「这一行从哪起、占多高的一档」，
    /// 剩下的宽度与对齐都在这里按它算。
    fn emit_line(
        &mut self,
        owner: NodeId,
        collected: &Collected,
        range: Range<usize>,
        place: LinePlace,
        cap: Option<&DropCap>,
        style: &ComputedStyle,
    ) -> f32 {
        let end = trim_end_spaces(&collected.text, range.start, range.end);
        let mut acc = LineAcc::default();
        // 按位置把文本与图片交替收进一行：图片锚在文本流的字节偏移上。
        let mut cursor = range.start;
        for img in collected
            .images
            .iter()
            .filter(|im| im.at >= range.start && im.at < end)
        {
            if img.at > cursor {
                acc.push_text(self.measure, collected, cursor..img.at);
            }
            acc.push_image(img);
            cursor = img.at + IMAGE_GLYPH.len_utf8();
        }
        if cursor < end {
            acc.push_text(self.measure, collected, cursor..end);
        }
        if acc.fragments.is_empty() {
            // 空行走块的 strut：没有字形可量，就按块自己的字号与 line-height 撑出一行，
            // 否则 `<p><br></p>` 会塌成零高。上下 extent 用 0.8 / 0.2 这一档近似
            // （中文与多数拉丁字体的常见比例），半 leading 照同一个式子分。
            let block_style = RunStyle::new(style);
            let em = block_style.font_size;
            let half_leading = (block_style.box_height() - em) / 2.0;
            acc.ascent = em * 0.8 + half_leading;
            acc.descent = em * 0.2 + half_leading;
        }
        // 下沉的首字符排在片段最前面：绘制层从 `line.x` 起按片段宽度逐个右推，
        // 正文自然就从它右边开始，不必再给这一行加一次左边距。
        // 它的上下 extent 故意**不进** `acc.ascent` —— 基线必须留在正文那一份上，
        // 否则一行正文会被一个大字整行推下去（`float` 在 CSS 里正是这个意思）。
        if let Some(dc) = cap.filter(|_| place.reserved > 0.0)
            && let Some(index) = collected.piece_at(dc.norm.start)
        {
            let piece = &collected.pieces[index];
            acc.fragments.insert(
                0,
                LineFragment::Text(LineRun {
                    node: piece.node,
                    norm: dc.norm.clone(),
                    orig: orig_of(piece, &dc.norm),
                    style: dc.run.clone(),
                    width: dc.width,
                    ascent: dc.ascent,
                    descent: dc.descent,
                }),
            );
        }
        // 行盒 = 各片段（含自己的半 leading）到基线上下距离的并集。作者把 line-height
        // 收到字形本体之下时它照它缩，两行的字形因此叠在一起 —— 浏览器就是这么画的。
        // 夹一个 0 下限：行顶是按行盒高逐行推进的，负数会让后面的几何与分片失去单调性。
        let height = (acc.ascent + acc.descent).max(0.0);
        // 基线离行顶就是累积出来的上方 extent（每个片段自己那份 leading 已经算在里面）。
        let baseline = acc.ascent;
        // 纯图片行不吃首行缩进：一行里只有一张图（封面页的常态）时，把读者偏好的
        // 2em 缩进套上去只会把整幅宽的图推出版心右缘（《斗战狂潮》封面踩过）。
        // 对齐仍有效：居中样式的图片按整幅版心居中，与块级图片同一待遇。
        let image_only =
            acc.fragments.len() == 1 && matches!(acc.fragments[0], LineFragment::Image(_));
        let indent = if image_only { 0.0 } else { place.indent };
        let reserved = if image_only { 0.0 } else { place.reserved };
        // 可用宽要扣掉这一行让出去的左边距（首行的 `text-indent`、被首字符压着的
        // 那几行让开的一格），否则居中的标题会按整幅宽算偏移，把字推出版心。
        let avail = (place.width - indent - reserved).max(1.0);
        let offset = style.text_align.offset_in(style.direction, avail, acc.width);
        // 起始边让出的那一段落在哪一侧：LTR 在左，RTL 在右。行盒的 `x` 记的**一直是内容
        // 左缘**，所以 RTL 时这笔让位不加在 `x` 上，而是由上面那份 `offset` 从右缘往回算
        // —— 两条式子对「贴起始边」给出同一个结果，而作者钉死的物理 `left` 在 RTL 里
        // 就真的贴左（起始让位那一格因此不吃香，这是 CSS 的行为，不是漏的）。
        let rtl = style.direction.is_rtl();
        let inset = if rtl { 0.0 } else { indent };
        self.lines.push(LineBox {
            owner,
            // 片段顺序 = 视觉顺序。排版与绘制、命中与选中矩形都只吃这一份顺序，所以
            // RTL 的反转在这里做一次就够（见 `ordered_for_display`）。
            fragments: ordered_for_display(acc.fragments, style.direction),
            x: place.x + inset + offset,
            y: place.y,
            width: acc.width,
            height,
            baseline,
            align: style.text_align,
        });
        height
    }
}
