//! 收集与摆放：行内子树展平成段文本 + piece 表；块级顺序、兄弟外边距、缩进与对齐。
//!
//! 从 `layout` 主模块切出来只为了一个文件能一次读完；跨文件的条目用
//! `pub(super)`，即**只对 `layout` 内部开放**，不属于对外公开面。

use std::ops::Range;
use std::sync::LazyLock;

use unicode_linebreak::linebreaks;

use super::cache::{BlockCache, BlockKey, CachedBlock};
use super::font::{RunStyle, StyleArena, StyleRef};
use super::geometry::{
    BlockBox, ImageRun, Justify, JustifyMode, LineBox, LineFragment, LineRun, MarkerRun,
};
use super::marker::marker_text;
use super::ports::{ImageProvider, Measure, image_size};
use super::wrap::{
    DropCap, Insets, LineAcc, LinePlace, LinePlan, collect, hard_lines, hyphen_break_banned,
    orig_of, scan_by_char, scan_by_word, trim_end_spaces,
};
use crate::content::dom::{Dom, NodeId, NodeKind};
use crate::content::format::css::computed::{
    Clear, ComputedStyle, Direction, Display, Float, StyledDom, TextAlign, TextJustify,
};

/// `style_of` 查不到时给的兵底样式。正常不会用到 —— `StyledDom` 为每个节点都算了一条。
pub(super) static DEFAULT_STYLE: LazyLock<ComputedStyle> = LazyLock::new(ComputedStyle::default);

/// 列表标记与正文之间的间距，按标记自己的字号折算。
///
/// 这是**我们定的常数**，不是规范给的数：CSS 把 `::marker` 之间的间距留给实现
/// （浏览器大致空一个空格宽）。0.25 em 在 16px 下是 4px，比空格窄一点 ——
/// 中文书面「圆点贴字」的那种紧凑感靠它，而 `ul` 的 2.5em 左内边距本来就为它留了位置。
const MARKER_GAP_EM: f32 = 0.25;

/// HTML 的 `start` / `value` 属性 → 整数。属性缺失、空串、非数字都给 `None`。
///
/// 两个属性都允许 0 与负数（HTML 的合法值），序号那一侧由 [`marker_text`] 夹回 1 起。
fn attr_count(dom: &Dom, node: NodeId, name: &str) -> Option<i64> {
    dom.attr(node, name).and_then(|v| v.trim().parse::<i64>().ok())
}

/// 摆一段行内内容的四件套：落点 `(x, y)`、行宽，以及百分比的折算基准。
///
/// 这四个数在 `children` → `block` → `wrap_paragraph` → `wrap_lines` 一路同进同出，
/// 又全是裸 `f32`：收成一条结构体不只是为了让签名短，是因为**把 `width` 与 `basis`
/// 传反了编译器不会拦**，症状是「百分比边距莫名其妙」，而无头测试里只有一屏错位。
#[derive(Clone, Copy)]
struct Slot {
    x: f32,
    y: f32,
    /// 行宽：已扣掉这一块自己的边距、边框与内边距。
    width: f32,
    /// 包含块宽：`text-indent` / `padding` 那一族百分比的折算基准。
    basis: f32,
}

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
    /// 当前块格式上下文里已经摆出去的浮动盒。`children()` 一路把它们递给后面的兄弟块，
    /// 直到某个块写了 `clear`。
    ///
    /// 为什么放在 `Ctx` 而不是当参数传：这条链上 `block` → `children` → `wrap_paragraph`
    /// → `wrap_lines` 每一层都要问它，而 `wrap_*` 的参数已经全是数了。浮动盒进出的
    /// 唯一地方是 `children`（摆一个进去）与浮动盒自己的内容（换一张空表），
    /// 后者用 `std::mem::take` 走完再换回来。
    pub(super) floats: Vec<PlacedFloat>,
}

/// 一个已经摆出去的浮动盒。坐标是章内的**绝对值**，而且记的是**外边距盒** ——
/// 绕着它排的那些行要让开的就是这一圈（`.img1` 那句 `margin-right: .8em` 正是靠它
/// 变成图与字之间的那条缝）。
#[derive(Clone, Copy)]
pub(super) struct PlacedFloat {
    /// 摆在右侧（`float: right`）。两侧的规则完全对称，所以只要一个布尔。
    pub(super) right: bool,
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) width: f32,
    pub(super) height: f32,
}

impl PlacedFloat {
    fn bottom(&self) -> f32 {
        self.y + self.height
    }

    /// 行带 `[top, top + band)` 与这个浮动盒纵向有没有相交。CSS 的口径正是「相交就让位」，
    /// 不是「顶边落在里面才让位」—— 后者会让压着图下缘的那一行字贴到图上。
    fn overlaps(&self, top: f32, band: f32) -> bool {
        self.y < top + band && self.bottom() > top
    }
}

/// `::first-letter` 涵盖的那一段字节区间：从段首起，到第一个硬行里第一个**字母单元**
/// 结束（数字也算 —— 浏览器给 `1984` 的首字符就是那个 `1`）。紧贴首字母的标点按 CSS
/// 也在这个盒里（`「開」` 的引号），而它们本来就被这一段涵盖了，不必再往回找一遍。
/// 全段一个字母单元都没有（纯标点、纯图片）时给 `None`，也就是不造这个盒。
fn first_letter_norm(collected: &Collected) -> Option<Range<usize>> {
    let head = collected.text.split('\n').next()?;
    let at = head
        .char_indices()
        .find(|(_, ch)| ch.is_alphabetic() || ch.is_ascii_digit())?
        .0;
    // 区间**从段首起**，不是从那个字母起：紧贴首字母的标点按 CSS 也算在这个盒里面
    // （`「開」` 的引号），而它们本来就被这一段涵盖了。
    Some(0..at + head[at..].chars().next()?.len_utf8())
}

/// 有没有「这块的内容就是一张图」：跳过纯空白文本与非 `img` 的行内包裹，整棵子树里
/// 只找一个 `<img>`。两张、或图旁边还站着正文，就不是这一档 → `None`
/// （那一块按不浮动处理，理由见 [`Ctx::float_outer_width`]）。
fn single_image(dom: &Dom, node: NodeId) -> Option<NodeId> {
    if dom.is_element(node, "img") {
        return Some(node);
    }
    let mut found: Option<NodeId> = None;
    for &child in dom.get(node).children.iter() {
        let n = dom.get(child);
        if n.kind == NodeKind::Text {
            if !n.text.trim().is_empty() {
                return None;
            }
            continue;
        }
        if n.kind != NodeKind::Element {
            return None;
        }
        let img = single_image(dom, child)?;
        if found.replace(img).is_some() {
            return None;
        }
    }
    found
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
    ///
    /// `width` 就是 `container` 的内容宽，因此它同时是子节点那一族百分比
    /// （`margin` / `padding` / `text-indent`）的**折算基准** —— CSS 规定这一族折自
    /// 包含块的内容宽，不是折自盒子自己的宽。
    pub(super) fn children(&mut self, container: NodeId, x: f32, y: f32, width: f32) -> f32 {
        if self.light {
            return self.children_light(container, x, y, width);
        }
        let container_style = self.style_of(container);
        let mut cursor = y;
        let mut prev_bottom = 0f32;
        let mut pending: Vec<NodeId> = Vec::new();
        // 列表序号。CSS 的列表计数器就是「同层 `display: list-item` 的兄弟按文档序」，
        // `<ol start>` 挪起点（所以先减 1，第一项 ++ 之后正好落回 `start`），
        // `<li value>` 挪这一项并把续值接在它后面。
        let mut ordinal: i64 = attr_count(self.dom, container, "start").map_or(0, |start| start - 1);

        for &child in self.children_of(container) {
            let style = self.style_of(child);
            if style.display == Display::None {
                continue;
            }
            if self.is_inline_flow(child, style) {
                pending.push(child);
                continue;
            }
            // 遇到块级子节点，先把攒着的行内内容冲成一个匿名块。匿名块的包含块就是这个
            // 容器，所以它的缩进基准同样是 `width`。
            if !pending.is_empty() {
                let pieces = std::mem::take(&mut pending);
                let line_px = (container_style.font_size * container_style.line_height).max(1.0);
                let bands = self.float_bands(x, cursor, width, line_px);
                cursor += self.wrap_paragraph(
                    container,
                    &pieces,
                    Slot { x, y: cursor, width, basis: width },
                    &Insets(bands.as_deref()),
                    container_style,
                );
                prev_bottom = 0.0;
            }
            let is_item = style.display == Display::ListItem;
            if is_item {
                ordinal = attr_count(self.dom, child, "value").unwrap_or(ordinal + 1);
            }
            let ml = style.margin_left.resolve(width).max(0.0);
            let mr = style.margin_right.resolve(width).max(0.0);
            let mt = style.margin_top.resolve(width).max(0.0);
            // 兄弟之间折叠取较大值而不是相加：这是段间距不显松的唯一原因。
            cursor += if prev_bottom > 0.0 {
                mt.max(prev_bottom)
            } else {
                mt
            };
            // `clear` 把这一块整块推到那一侧浮动盒的下缘之下。净空算的是**边框**的位置，
            // 所以推完之后前面那段折叠的右边距不再起作用。
            let clear_to = self.clear_bottom(style.clear);
            if clear_to > cursor {
                cursor = clear_to;
            }
            // 浮动盒：离开常规流。摆在当前游标那一档的侧边，**不**推进 `cursor`，
            // 于是后面每一段都从同一处起排、只是行带被它折窄。认不出宽度就落回常规流。
            if style.float != Float::None
                && self.place_float(child, style, x, cursor, width)
            {
                continue;
            }
            let lines_start = self.lines.len();
            cursor += self.block(child, x + ml, cursor, (width - ml - mr).max(1.0), width);
            // 标记贴在 `block()` **之后**：见 [`Self::attach_marker`] 那三条口径。
            if is_item {
                self.attach_marker(child, style, ordinal.max(1) as usize, lines_start);
            }
            prev_bottom = style.margin_bottom.resolve(width).max(0.0);
        }
        if !pending.is_empty() {
            let line_px = (container_style.font_size * container_style.line_height).max(1.0);
            let bands = self.float_bands(x, cursor, width, line_px);
            cursor += self.wrap_paragraph(
                container,
                &pending,
                Slot { x, y: cursor, width, basis: width },
                &Insets(bands.as_deref()),
                container_style,
            );
        }
        cursor - y
    }

    /// 旁边有哪些浮动盒、以及这一块摆在哪儿 → 缓存键里那一格。见 [`BlockKey::floats`]。
    ///
    /// 全部按**相对**这一块的偏移折：键因此与平移无关，块被搬到别的 `(x, y)` 上照样
    /// 命中（两栏对开就靠这一点），而让宽几何本身也只由相对位置决定。
    fn float_key(&self, x: f32, y: f32) -> u64 {
        if self.floats.is_empty() {
            return 0;
        }
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for f in &self.floats {
            super::key::quantize_px(f.x - x).hash(&mut h);
            super::key::quantize_px(f.y - y).hash(&mut h);
            super::key::quantize_px(f.width).hash(&mut h);
            super::key::quantize_px(f.height).hash(&mut h);
            f.right.hash(&mut h);
        }
        h.finish()
    }

    /// 行带 `[top, top + band)` 上，被已摆出的浮动盒占掉的 `(左, 右)` 两段宽 ——
    /// 相对这一块的内容盒 `[x, x + width]` 计，所以调用方拿到的是「这一行少多少可写」。
    fn float_insets(&self, x: f32, width: f32, top: f32, band: f32) -> (f32, f32) {
        let mut left = 0.0f32;
        let mut right = 0.0f32;
        for f in &self.floats {
            if !f.overlaps(top, band) {
                continue;
            }
            if f.right {
                right = right.max((x + width - f.x).max(0.0));
            } else {
                left = left.max((f.x + f.width - x).max(0.0));
            }
        }
        // 两道屏障从两头挤到相遇时不夹到负：`avail` 那一侧另有 `.max(1.0)` 兜底，
        // 这里只保证「让出去的宽不超过这一块的宽」。
        (left.min(width), right.min(width))
    }

    /// 一段落点 `(x, y)` 的正文，逐行的 `(左, 右)` 让宽表。没有浮动盒时给 `None` ——
    /// 那是正文的常态，此时折行照旧吃定宽，一次查表都不付。
    ///
    /// 表的长度由**浮动盒的下缘**决定：越过它每一行都不再让位，越界查表给 `(0, 0)`。
    ///
    /// 为什么按「第几行 × 名义行高」估而不是按真实行顶：真实行顶要等摆放那一趟才知道，
    /// 而摆放必须先有宽度 —— 两趟之间不能来回问。于是计划那一趟按名义行高定档，摆放照
    /// 计划里的数走。纯正文段落两者相等；行里夹了大字号（标题、上标）时会差不到一档，
    /// 症状是压着图下缘的那一行多让或少让一行，是有界的近似，不是漂移。
    fn float_bands(&self, x: f32, y: f32, width: f32, line_px: f32) -> Option<Vec<(f32, f32)>> {
        if self.floats.is_empty() {
            return None;
        }
        let lowest = self.floats.iter().fold(y, |acc, f| acc.max(f.bottom()));
        let rows = (((lowest - y) / line_px).ceil() as usize) + 2;
        Some(
            (0..rows)
                .map(|k| {
                    self.float_insets(x, width, y + k as f32 * line_px, line_px)
                })
                .collect(),
        )
    }

    /// `clear` 要把这一块推到多低：受影响那一侧所有浮动盒下缘的最大值。
    ///
    /// 左右分别算，不是都按 `both` 处理：`clear: right` 写在「左边还有空」的段上，
    /// 折成 `both` 会白白多挪一段。
    fn clear_bottom(&self, clear: Clear) -> f32 {
        if clear == Clear::None {
            return 0.0;
        }
        self.floats
            .iter()
            .filter(|f| match clear {
                Clear::Left => !f.right,
                Clear::Right => f.right,
                _ => true,
            })
            .map(|f| f.bottom())
            .fold(0.0f32, f32::max)
    }

    /// 浮动块的**边框盒宽**（外边距由调用方加，因为让位的那一圈是外边距盒）。
    ///
    /// 这一版只认「整块内容就是一张图」那一种，也就是书里插图框的全部写法
    /// （《哈利·波特》那 78 个 `div.img1` / `div.img2` 都是 `<div><img width="20%"></div>`）。
    /// 真正的 shrink-to-fit 要 max-content，等于再跑一趟排版；认不出来时按**不浮动**处理
    /// 并留一条 `debug!`，而不是猜一个宽度 —— 猜宽了会把整页正文挤成一条窄柱，
    /// 那比插图不绕文糟得多。
    fn float_box_width(&self, node: NodeId, style: &ComputedStyle, basis: f32) -> Option<f32> {
        let img = single_image(self.dom, node)?;
        let img_style = self.style_of(img);
        let src = self.dom.attr(img, "src")?;
        let image = self.images.resolve(src)?;
        let (w, _) = image_size(img_style, image.width, image.height, basis);
        // 图自己的左右外边距、边框与内边距都要算进来：`.img1` 里那句
        // `margin-right: .8em` 正是靠这一步变成图与字之间的那条缝。
        let [_, br, _, bl] = img_style.border_drawn();
        let inner = w
            + bl + br
            + img_style.padding_left.resolve(basis).max(0.0)
            + img_style.padding_right.resolve(basis).max(0.0)
            + img_style.margin_left.resolve(basis).max(0.0)
            + img_style.margin_right.resolve(basis).max(0.0);
        let [_, fb, _, fl] = style.border_drawn();
        Some(
            inner
                + fl + fb
                + style.padding_left.resolve(basis).max(0.0)
                + style.padding_right.resolve(basis).max(0.0),
        )
    }

    /// 摆一个浮动盒：算出它的边框盒宽、摆在所属侧、排它自己的内容，然后记进浮动表。
    ///
    /// 两件事和常规流不同：它**不推进** `cursor`（后面的块照样从这儿过），以及它里面
    /// 的排版**看不见外面的浮动盒** —— CSS 里浮动盒的内容自成一个块格式上下文，
    /// 所以那里头不躲外面的图。用 `mem::take` 换一张空表进去、走完换回来。
    /// 返回 `false` = 认不出宽度（见 [`Self::float_box_width`]），调用方按不浮动处理。
    fn place_float(
        &mut self,
        node: NodeId,
        style: &ComputedStyle,
        x: f32,
        y: f32,
        width: f32,
    ) -> bool {
        let basis = width;
        let Some(box_width) = self.float_box_width(node, style, basis) else {
            tracing::debug!(
                "float: <{}> 认不出收缩宽度，这一块按不浮动排",
                self.dom.tag_name(node).unwrap_or("?")
            );
            return false;
        };
        let ml = style.margin_left.resolve(basis).max(0.0);
        let mr = style.margin_right.resolve(basis).max(0.0);
        let mt = style.margin_top.resolve(basis).max(0.0);
        let mb = style.margin_bottom.resolve(basis).max(0.0);
        let right = style.float == Float::Right;
        // 靠左的贴左缘（含它自己的左边距），靠右的贴右缘 —— 让位的那一圈是外边距盒。
        let box_x = if right { x + width - mr - box_width } else { x + ml };
        let saved = std::mem::take(&mut self.floats);
        let height = self.block(node, box_x, y + mt, box_width, basis);
        self.floats = saved;
        self.floats.push(PlacedFloat {
            right,
            x: box_x - ml,
            y,
            width: box_width + ml + mr,
            height: height + mt + mb,
        });
        true
    }

    /// 把一个列表项的标记挂到它的**第一行**上（`list-style-position: outside` 那一档）。
    ///
    /// 三条口径决定它长这样：
    ///
    /// - **挂在行首之外，不进流动**：不占行宽、不改行盒高，正文一个字都不挪位。腾出来
    ///   给它的是 UA 那句 `ul` / `ol { padding-left: 2.5em }`。
    /// - **只在完整排版那一趟做**：轻量模式只量高，标记不改任何高度，跟着做等于每次
    ///   多付一次 `run_metrics`。
    /// - **贴在块级缓存之外**（所以调用点在 `block()` 返回之后）：缓存里那份行盒永远
    ///   没有标记，复用时照样贴得回去。反过来 —— 在块内部贴好再缓存 —— 会让序号跟着
    ///   缓存走，读者删掉列表头一项，第二项还顶着旧的号。
    fn attach_marker(
        &mut self,
        node: NodeId,
        style: &ComputedStyle,
        index: usize,
        lines_start: usize,
    ) {
        let Some(text) = marker_text(style.list_style_type, index) else {
            return;
        };
        let run_style = RunStyle::new(style);
        let metrics = self
            .measure
            .run_metrics(&text, &run_style.font, run_style.font_size);
        let gap = run_style.font_size * MARKER_GAP_EM;
        let Some(line) = self.lines.get_mut(lines_start) else {
            return; // 空列表项（`<li></li>`）没有第一行可挂
        };
        // `outside` 的那一起始边：横排在行盒之左，RTL 在行盒之右（RTL 行的右边才是起点）。
        let offset = if style.direction == Direction::Rtl {
            line.width + gap
        } else {
            -(metrics.width + gap)
        };
        line.fragments.insert(
            0,
            LineFragment::Marker(MarkerRun {
                node,
                text,
                style: run_style,
                width: metrics.width,
                ascent: metrics.ascent,
                descent: metrics.descent,
                offset,
            }),
        );
    }

    /// 轻量模式：只算块高，不断行。
    ///
    /// 跳过 `wrap_paragraph` / `run_metrics` / `LineFragment` 构建。
    /// 文本块按「字符数 / 每行字数」估算行数，乘以行高得到高度。
    ///
    /// 浮动盒在这里**只管流，不管让宽**：它自己不占垂直高度（后面的块从同一处过），
    /// `clear` 那一段净空要照算 —— 这两项是量高表与实际排版差别的主体（一张浮动的
    /// 插图今天按常规流吃掉一整段高）。每行让宽那一项不进估计，与首字下沉在轻量模式
    /// 里同样被略过是同一个口径：都是「按总宽除以版心宽」这一档近似本来就没有的项。
    fn children_light(&mut self, container: NodeId, x: f32, y: f32, width: f32) -> f32 {
        let mut cursor = y;
        let mut prev_bottom = 0f32;

        for &child in self.children_of(container) {
            let style = self.style_of(child);
            if style.display == Display::None {
                continue;
            }
            let ml = style.margin_left.resolve(width).max(0.0);
            let mr = style.margin_right.resolve(width).max(0.0);
            let mt = style.margin_top.resolve(width).max(0.0);
            let mb = style.margin_bottom.resolve(width).max(0.0);
            cursor += if prev_bottom > 0.0 {
                mt.max(prev_bottom)
            } else {
                mt
            };
            let clear_to = self.clear_bottom(style.clear);
            if clear_to > cursor {
                cursor = clear_to;
            }

            let content_width = (width - ml - mr).max(1.0);

            // 浮动盒：量它的高（里面看不见外面的浮动表），但**不推进** `cursor`。
            if style.float != Float::None {
                let right = style.float == Float::Right;
                if let Some(box_width) = self.float_box_width(child, style, width) {
                    let box_x = if right {
                        x + width - mr - box_width
                    } else {
                        x + ml
                    };
                    let saved = std::mem::take(&mut self.floats);
                    let height = self.block_light(child, box_width, width);
                    self.floats = saved;
                    self.floats.push(PlacedFloat {
                        right,
                        x: box_x - ml,
                        y: cursor,
                        width: box_width + ml + mr,
                        height: height + mt + mb,
                    });
                    continue;
                }
            }

            let height = self.block_light(child, content_width, width);

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
    ///
    /// `basis` 是**这一块的包含块宽**（父块的内容宽），用来折这一族百分比：
    /// CSS 的 `<margin>` / `<padding>` / `<text-indent>` 百分比全部折算自它，
    /// 不是折自 `width`。它必须进缓存键 —— 同一 `width` 配不同 `basis`，
    /// 同一个盒子会折出不同的内边距。
    pub(super) fn block(&mut self, node: NodeId, x: f32, y: f32, width: f32, basis: f32) -> f32 {
        let Some(key) = self.block_key(node, width, basis, x, y) else {
            return self.block_uncached(node, x, y, width, basis);
        };
        if let Some(height) = self.reuse_block(key, x, y) {
            return height;
        }
        let lines_start = self.lines.len();
        let blocks_start = self.blocks.len();
        let floats_start = self.floats.len();
        let height = self.block_uncached(node, x, y, width, basis);
        self.stash_block(
            key,
            lines_start,
            blocks_start,
            floats_start,
            x,
            y,
            height,
        );
        height
    }

    /// 这一块的缓存键；没开缓存时给 `None`（直接走实排）。
    fn block_key(&self, node: NodeId, width: f32, basis: f32, x: f32, y: f32) -> Option<BlockKey> {
        self.cache.as_ref()?;
        let style_hash = self.style_hash?.get(node as usize).copied().unwrap_or(0);
        Some(BlockKey {
            node,
            style_hash,
            width: width.to_bits(),
            basis: basis.to_bits(),
            floats: self.float_key(x, y),
        })
    }

    /// 缓存命中：把缓存的块按 `(x, y)` 平移后并入 `self.lines`/`self.blocks`，返回块高。
    ///
    /// 这一块**摆出去过的浮动盒也要搬回来** —— 命中时 `children()` 那一趟不跑，
    /// 没人往浮动表里填，于是后面的兄弟块就不再绕它（键里带了行顶，位置不会错）。
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
        self.floats.extend(entry.floats.into_iter().map(|mut f| {
            f.x += x;
            f.y += y;
            f
        }));
        Some(height)
    }

    /// 把刚排好的这一块（`self.lines`/`self.blocks` 的尾段）归一化成块局部坐标存进缓存。
    ///
    /// 三个 `_start` 是同一趟排版里三个表的起点，参数多是因为要搬的东西就是三张表 ——
    /// 与本文件 `block_uncached` 那一串落位数一样，沿用 `#[allow]` 而不是再包一层结构体。
    #[allow(clippy::too_many_arguments)]
    fn stash_block(
        &mut self,
        key: BlockKey,
        lines_start: usize,
        blocks_start: usize,
        floats_start: usize,
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
        // 这一块摆出去的浮动盒也归一化到块局部 —— 与 `lines` / `blocks` 同一条规矩，
        // 于是「命中时搬到别处」这一件事对三种产物是同一个加法。
        let floats: Vec<PlacedFloat> = self.floats[floats_start..]
            .iter()
            .map(|f| {
                let mut moved = *f;
                moved.x -= x;
                moved.y -= y;
                moved
            })
            .collect();
        cache.put(
            key,
            CachedBlock {
                lines,
                blocks,
                height,
                floats,
            },
        );
    }

    /// 摆放一个块级元素（不做块级缓存），返回边框盒高度。`basis` 见 [`Self::block`]。
    fn block_uncached(&mut self, node: NodeId, x: f32, y: f32, width: f32, basis: f32) -> f32 {
        let style = self.style_of(node);
        // 盒模型从外到内一次算清：边框与内边距一起从可用宽里扣掉，正文与折行都在内容盒里
        // 算；外层那条 `push_block` 记的仍是**边框盒**（绘制背景与边框要用它）。
        // 内边距按 `basis` 而不是 `width` 折 —— CSS 里 `padding` 的百分比折自包含块宽。
        let [bt, br, bb, bl] = style.border_drawn();
        let pl = bl + style.padding_left.resolve(basis).max(0.0);
        let pr = br + style.padding_right.resolve(basis).max(0.0);
        let pt = bt + style.padding_top.resolve(basis).max(0.0);
        let pb = bb + style.padding_bottom.resolve(basis).max(0.0);
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
            let offset = style
                .text_align
                .offset_in(style.direction, content_width, w);
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
                // 块级图片独立成行，两端对齐只管字间的余量，对整块不适用。
                justify: None,
            });
            return self.close_block(index, pt + h + pb);
        }

        let content_width = (width - pl - pr).max(1.0);
        let index = self.push_block(node, x, y, width, String::new(), true);
        let line_px = (style.font_size * style.line_height).max(1.0);
        let inner = if self.has_block_children(node) {
            self.children(node, x + pl, y + pt, content_width)
        } else {
            // 纯行内内容：先问一次「这一块旁边有没有浮动盒」。没有就是 `None`，
            // 折行照旧吃定宽，一次查表都不付。
            let bands = self.float_bands(x + pl, y + pt, content_width, line_px);
            let pieces = self.children_of(node);
            self.wrap_paragraph(
                node,
                pieces,
                Slot { x: x + pl, y: y + pt, width: content_width, basis },
                &Insets(bands.as_deref()),
                style,
            )
        };
        self.close_block(index, pt + inner + pb)
    }

    /// 轻量模式：算一个块的高，不断行。`basis` 见 [`Self::block`] —— 这一条必须与
    /// [`Self::block_uncached`] 用同一个基准折百分比，否则两阶段排版的「量高」与
    /// 「实排」会给出两个不同的高，可见块就错位。
    pub(super) fn block_light(&mut self, node: NodeId, width: f32, basis: f32) -> f32 {
        let style = self.style_of(node);
        let [bt, br, bb, bl] = style.border_drawn();
        let pl = bl + style.padding_left.resolve(basis).max(0.0);
        let pr = br + style.padding_right.resolve(basis).max(0.0);
        let pt = bt + style.padding_top.resolve(basis).max(0.0);
        let pb = bb + style.padding_bottom.resolve(basis).max(0.0);

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
    ///
    /// 行宽与百分比基准都在 [`Slot`] 里，两者只在有 `padding` / `margin` / 显式 `width`
    /// 时才分开，而 `text-indent` 的百分比只认后者。`insets` 是这一旁边浮动盒的逐行让宽
    /// （[`Insets`]），空表时折行照旧吃定宽。
    fn wrap_paragraph(
        &mut self,
        owner: NodeId,
        pieces: &[NodeId],
        slot: Slot,
        insets: &Insets<'_>,
        style: &ComputedStyle,
    ) -> f32 {
        let Slot { x, y, width, .. } = slot;
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
        let height = self.wrap_lines(owner, &collected, slot, insets, style);
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
        slot: Slot,
        insets: &Insets<'_>,
        style: &ComputedStyle,
    ) -> f32 {
        let Slot { x, y, width, basis } = slot;
        if collected.text.is_empty() {
            return 0.0;
        }
        // 缩进按包含块宽折（`basis`），再最多吃掉半个版心，否则首行会被挤成零宽。
        let indent = style.text_indent.resolve(basis).max(0.0).min(width * 0.5);
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
                    insets,
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
                    insets,
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
                    insets: plan.insets,
                    hard_end: plan.hard_end,
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
    ///
    /// 两个来源，同一个几何：
    ///
    /// 1. 作者写了 `::first-letter` —— CSS 的正路。
    /// 2. 整段**第一个** piece 自己带 `float` 的行内盒 —— 中文转换本的章首下沉几乎全是
    ///    这一种：`<p class="fp"><span class="first">这</span>一年的秋天……`，那个
    ///    span 上写着 `font-size: 2em; float: left; line-height: 1`。《哈利·波特》
    ///    301 章里有 199 章是它。
    ///
    /// 写在句子中间的浮动行内盒这一版**不**按下沉处理，照普通行内内容排：真正的行中
    /// 浮动要在一条行带里插一个盒，那是另一套几何，而书里几乎不这么写。
    fn drop_cap(
        &self,
        collected: &Collected,
        style: &ComputedStyle,
        width: f32,
    ) -> Option<DropCap> {
        let (cap_style, norm) = match style.first_letter.as_deref() {
            Some(first) => (first, first_letter_norm(collected)?),
            None => {
                let piece = collected.pieces.first()?;
                if piece.norm.start != 0 {
                    return None;
                }
                let floated = self.style_of(piece.node);
                if floated.float == Float::None {
                    return None;
                }
                (floated, piece.norm.clone())
            }
        };
        let text = collected.text.get(norm.clone())?;
        let mut run = RunStyle::new(cap_style);
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
        if let Some(dc) = cap.filter(|_| place.insets.reserved > 0.0)
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
        // 逻辑起始边那一笔让位（读者缩进、被下沉首字符压着的那几行）跟着 `image_only`
        // 免掉；两侧浮动盒的让位是**物理**的，一行纯图片挨着浮动图时同样得让开，
        // 所以不参与那个豁免。
        let start = if image_only { 0.0 } else { place.insets.start };
        let reserved = if image_only { 0.0 } else { place.insets.reserved };
        let (left, right) = (place.insets.left, place.insets.right);
        // 可用宽把这四段全扣掉：漏一段就是把字写进了不该写字的地方。
        let avail = (place.width - start - reserved - left - right).max(1.0);
        let offset = style
            .text_align
            .offset_in(style.direction, avail, acc.width);
        // 两端对齐：不是段末 / 硬行尾、不是纯图片行、且是 LTR 时，把余量摊到行内的隙上。
        // 计数用 `acc.fragments`（**逻辑序**）—— 空格数与字符数都看不出方向，用哪一份序都一样；
        // 下沉首字符那一份跳过（它不占流动宽，走 `reserved` 让位）。
        let justify = if image_only {
            None
        } else {
            let (mut chars, mut spaces) = (0u32, 0u32);
            // 数的是 `acc.fragments` 里**全部**文本 run 的字符（连下沉首字符那一段也算）——
            // 绘制/命中/选区按同一份片段顺序走，哪一份都不许在这里漏数，否则推开的字对不上。
            for fragment in &acc.fragments {
                let LineFragment::Text(run) = fragment else {
                    continue;
                };
                if let Some(text) = collected.text.get(run.norm.clone()) {
                    chars += text.chars().count() as u32;
                    spaces += text.chars().filter(|c| *c == ' ').count() as u32;
                }
            }
            justify_plan(style, place.hard_end, avail, acc.width, chars, spaces)
        };
        // 起始边让出的那一段落在哪一侧：LTR 在左，RTL 在右。行盒的 `x` 记的**一直是内容
        // 左缘**，所以 RTL 时这笔让位不加在 `x` 上，而是由上面那份 `offset` 从右缘往回算
        // —— 两条式子对「贴起始边」给出同一个结果，而作者钉死的物理 `left` 在 RTL 里
        // 就真的贴左（起始让位那一格因此不吃香，这是 CSS 的行为，不是漏的）。
        // 浮动盒的两段**不**走这条路：`left` 永远加在左缘上，RTL 也一样 ——
        // 左边那张图在阿拉伯语的书里还是站在左边。
        let rtl = style.direction.is_rtl();
        let inset = if rtl { 0.0 } else { start };
        self.lines.push(LineBox {
            owner,
            // 片段顺序 = 视觉顺序。排版与绘制、命中与选中矩形都只吃这一份顺序，所以
            // RTL 的反转在这里做一次就够（见 `ordered_for_display`）。
            fragments: ordered_for_display(acc.fragments, style.direction),
            x: place.x + left + inset + offset,
            y: place.y,
            width: acc.width,
            height,
            baseline,
            align: style.text_align,
            justify,
        });
        height
    }
}

/// 这一行要不要两端对齐、怎么摊。返回 `None` = 不对齐（左/右/中 / 段末 / `<br>` / RTL /
/// 没有余量 / 没有可摊的隙）。
///
/// 摊法只有两句（与 [`Justify`] 一致）：某字符的 x 偏移 = `extra × 它之前的隙数`；
/// `Spaces` 数空格，`Chars` 数字符。**绘制、命中、选区三处按同一句各走一遍片段**。
fn justify_plan(
    style: &ComputedStyle,
    hard_end: bool,
    avail: f32,
    content: f32,
    chars: u32,
    spaces: u32,
) -> Option<Justify> {
    // RTL 的余量摊开方向与空隙计数都反，一期不做（见 `plans/facts.md` 的已知边界）。
    if hard_end || style.direction.is_rtl() || style.text_align != TextAlign::Justify {
        return None;
    }
    if style.text_justify == TextJustify::None {
        return None;
    }
    let slack = avail - content;
    // 半像素以下不对齐：既省掉一处看不见的差异，也避免给浮点抖动造出「有的行对齐、有的不」。
    if slack <= 0.5 {
        return None;
    }
    let (mode, gaps) = match style.text_justify {
        // `inter-word` 只拉空格：没有空格就这一行不对齐。
        TextJustify::InterWord => (JustifyMode::Spaces, spaces),
        // `auto` / `inter-character` / `inter-ideograph`：有空格拉空格，否则拉字间。
        _ if spaces > 0 => (JustifyMode::Spaces, spaces),
        _ => (JustifyMode::Chars, chars.saturating_sub(1)),
    };
    if gaps == 0 {
        return None;
    }
    Some(Justify {
        extra: slack / gaps as f32,
        mode,
        gaps,
    })
}
