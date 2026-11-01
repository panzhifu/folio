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
    BlockBox, ImageRun, Justify, JustifyMode, LineBox, LineFragment, LineRun, MarkerRun, StackMark,
};
use super::marker::marker_text;
use super::ports::{ImageProvider, Measure, image_size};
use super::table;
use super::wrap::{
    DropCap, Insets, LineAcc, LinePlace, LinePlan, Sizes, collect, hanging_width, hangs_at_end,
    hard_lines, hyphen_break_banned, orig_of, scan_by_char, scan_by_word, trim_end_spaces,
};
use crate::content::dom::{Dom, NodeId, NodeKind};
use crate::content::format::css::computed::{
    CellVAlign, Clear, ComputedStyle, Direction, Display, Float, Length, Positioning, StyledDom,
    TextAlign, TextJustify, WordBreak,
};

/// `style_of` 查不到时给的兵底样式。正常不会用到 —— `StyledDom` 为每个节点都算了一条。
pub(super) static DEFAULT_STYLE: LazyLock<ComputedStyle> = LazyLock::new(ComputedStyle::default);

/// 列表标记与正文之间的间距，按标记自己的字号折算。
///
/// 这是**我们定的常数**，不是规范给的数：CSS 把 `::marker` 之间的间距留给实现
/// （浏览器大致空一个空格宽）。0.25 em 在 16px 下是 4px，比空格窄一点 ——
/// 中文书面「圆点贴字」的那种紧凑感靠它，而 `ul` 的 2.5em 左内边距本来就为它留了位置。
const MARKER_GAP_EM: f32 = 0.25;

/// 同一小节里**连续的图单元**叠成一格时，占满一屏就不叠：这一成是那条界线。
///
/// 读者要的是画册/相册那种观感：同一个小节里连着几张插图，不必一张占一屏、串成
/// 图注的最长字数。超过它就不认为是图注，而是「图后面紧跟的一段正文」—— 那种块
/// 不进叠（宁严不宽：吞一段正文进叠，读者就少看见一段字）。
pub(super) const STACK_CAPTION_MAX: usize = 64;

/// 叠图「同尺寸」那一格的容差：高差在**高的这个比例**以内就算一样大。
///
/// 同一个画廊里的一组封面常常差几像素 —— 《哈利·波特全集》美版那七张是
/// 1000×441 / 1000×460 / 1000×447，铺到屏宽之后高 476 / 497 / 483。按 0.5 像素比，
/// 这七张一张都叠不起来，而它们本来就是「同款同尺寸」的一叠。
///
/// 放宽是有代价的：叠起来同一时刻只显示一张，差这一档在画面上看不出来，**但注释会
/// 跟着矮的那张往上跑**，所以只放这一档（约半行到一行字的高度），再大就不划算 ——
/// 横图与竖图混排时仍然挡在门外。
pub(super) const STACK_SIZE_TOLERANCE: f32 = 0.08;

/// HTML 的 `start` / `value` 属性 → 整数。属性缺失、空串、非数字都给 `None`。
///
/// 两个属性都允许 0 与负数（HTML 的合法值），序号那一侧由 [`marker_text`] 夹回 1 起。
fn attr_count(dom: &Dom, node: NodeId, name: &str) -> Option<i64> {
    dom.attr(node, name)
        .and_then(|v| v.trim().parse::<i64>().ok())
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
    /// 屏幕（窗口）的宽与高，逻辑像素。只给图片用：超过屏幕的图按屏幕宽缩（「大图铺满
    /// 屏幕」）。`None` = 界面没给，行为与从前一样（按栏宽夹）。
    pub(super) screen: Option<(f32, f32)>,
    /// 一页内容区的高（分页是页高，滚动是视口内容高）。图片的盒高不许超过它：
    /// 分片器把一行图整个放进一页，超出的那一截没有下一页接，页底就是裁切。
    pub(super) page_height: Option<f32>,
    /// 当前块格式上下文里已经摆出去的浮动盒。`children()` 一路把它们递给后面的兄弟块，
    /// 直到某个块写了 `clear`。
    ///
    /// 为什么放在 `Ctx` 而不是当参数传：这条链上 `block` → `children` → `wrap_paragraph`
    /// → `wrap_lines` 每一层都要问它，而 `wrap_*` 的参数已经全是数了。浮动盒进出的
    /// 唯一地方是 `children`（摆一个进去）与浮动盒自己的内容（换一张空表），
    /// 后者用 `std::mem::take` 走完再换回来。
    pub(super) floats: Vec<PlacedFloat>,
    /// 脱流的绝对定位盒（`position: absolute` / `fixed`）与它**不脱流时的起点** `y`。
    /// 它们在 `children` 里被跳过、
    /// 只收在这里，等整章的 `height` 定下来再摆 —— `top` / `bottom` 的百分比基准是包含块高。
    pub(super) absolute: Vec<(NodeId, f32)>,
    /// 下一叠图的序号。按**起叠的先后**现编，所以它属于这一份排版结果：换一次字号重排，
    /// 序号从头再来 —— 界面那份「现在看第几张」必须在换排版时清掉（见 `StackMark`）。
    pub(super) stack_seq: u32,
    /// 排这一块时要把哪个孩子**挪到队尾**（只在叠图那一趟设置，见 [`Self::children`]）。
    ///
    /// 图注统一摆在这一叠的**下面**：作者把注写在图前时（《哈利·波特全集》画廊那种
    /// `<p>注</p><img>`），排这一块就按「图…注」的顺序来一次。只对当前这一块生效，
    /// 排完立刻清掉；换过顺序的那一块也**不走块缓存**（缓存键里没有这一格）。
    pub(super) caption_last: Option<NodeId>,
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
/// （那一块按不浮动处理，两档判据见 [`Ctx::float_box_width`]）。
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

/// 一棵子树里的非空白字数（图注那一格的长短判据）。
fn text_len_in(dom: &Dom, node: NodeId) -> usize {
    let n = dom.get(node);
    if n.kind == NodeKind::Text {
        return n.text.chars().filter(|c| !c.is_whitespace()).count();
    }
    n.children.iter().map(|c| text_len_in(dom, *c)).sum()
}

/// 这一块的子树里有没有 `<img>`（图注那一格不许含图，含图就是「图文混排的一块」）。
fn has_image(dom: &Dom, node: NodeId) -> bool {
    if dom.is_element(node, "img") {
        return true;
    }
    dom.get(node).children.iter().any(|c| has_image(dom, *c))
}

/// 「这一格的内容就是一张图」的那一族，交回它的**原始**引用值（给 `images.resolve` 那张表）。
///
/// 认三种形状：
/// - `<img src>` —— 一直认的那一种。
/// - `<image href>` / `<image xlink:href>` —— SVG 里的图。DOM 存属性名时丢掉前缀并转小写
///   （`content/dom` 的 `add_attrs_if_missing` 用 `name.local`），所以 `xlink:href` 落表就是
///   `href`，不需要按前缀找。
/// - `<svg …><image href="cover.jpeg"/></svg>` —— **Calibre 导出封面页的标准写法**
///   （EPUB2 的 `titlepage.xhtml` 十有八九是这个形状，基督山伯爵那本就是）。以前只认 `img`，
///   这一章就折成 0 高：0 高的章在滚动链里不存在（`reader/book.rs` 那道 `height() > 0.0`
///   的筛子），症状是「从第二章回滑，滑不到第一节那张封面」。
///
/// 只认**一张** `image` 的 svg：叠多张、或带 `path`/`text` 的那种是矢量画，该走
/// `content::format::svg` 的整幅栅格化，不在排版里冒充一张位图。
pub(super) fn picture_src(dom: &Dom, node: NodeId) -> Option<String> {
    if dom.is_element(node, "img") {
        return dom.attr(node, "src").map(str::to_string);
    }
    if dom.is_element(node, "image") {
        return dom
            .attr(node, "href")
            .or_else(|| dom.attr(node, "src"))
            .map(str::to_string);
    }
    if !dom.is_element(node, "svg") {
        return None;
    }
    let mut images = dom
        .element_children(node)
        .filter(|child| dom.is_element(*child, "image"));
    let image = images.next()?;
    if images.next().is_some() {
        return None;
    }
    dom.attr(image, "href")
        .or_else(|| dom.attr(image, "src"))
        .map(str::to_string)
}

/// 这一张图该不该**横向出血**到屏幕宽（多看 / 掌阅的 `duokan-bleed`）。
///
/// 查的是**本节点与它的每一层祖先**：真书里这一条写在包图的那个块上，不写在 `<img>`
/// 自己身上 ——《诡秘之主》的三个类全是 `div`（`fmdG1` / `textdA1` / `textdE1`），
/// 《基督山伯爵》同理。只查本节点样式的写法等于整条没接（而落差表会说「接住了」，
/// 正是本仓库那条「Applied 不等于排版层读了」要挡的形状）。
///
/// 只认横向那两条边（`left` / `right`）。纵向出血要的是「这张图盖住页顶/页底」，
/// 那得改分片边界，不是改一张图的盒尺寸 —— 现在没人消费，所以 `top` / `bottom`
/// 单独置位时这里回 `false`（它确实什么都不改，与没写同一表现）。
pub(super) fn bleeds_wide(dom: &Dom, styles: &StyledDom, node: NodeId) -> bool {
    dom.ancestors_including_self(node)
        .any(|id| styles.get(id).is_some_and(|style| style.bleed.horizontal()))
}

/// 这张图该不该按**屏幕宽**铺开：块级与行内两处共用这一条。
///
/// 「该铺」由两件事决定，缺一条都不铺：
///
/// - **要不要铺**（`wide`）：作者写了 `duokan-bleed`，或读者那一档是「铺满屏幕」。
///   `wide` 由调用方先算好（`bleed` 要先被「原始尺寸」那一档压掉），这里不再重判。
/// - **够不够大**（`big_enough`）：一块 24px 的徽标独占一段也不该被拉成满屏。判据是
///   宽 ≥ 版心八成、或高 ≥ **一屏**八成；`object-fit: cover` 是作者显式要盖满，免这一条。
///
/// 「一屏」= 页高，没有页高（连续滚动那一档 `page_height` 传 `None`）就用屏高。
/// 从前这里只认页高，于是同一张图在滚动里被放大、切到分页/仿真又缩回去 ——
/// 读者报的「切到仿真图变小」就是这一条在两端各判了一次的结果。
pub(super) fn fills_the_screen(
    wide: bool,
    w: f32,
    h: f32,
    column_width: f32,
    page_height: Option<f32>,
    screen_height: Option<f32>,
) -> bool {
    // `object-fit` 只决定「图在自己的盒里怎么裁/留底」，**不改盒子的宽**。
    // 盒子的宽听 CSS；要铺到屏幕宽只有作者显式出血（`wide`）这一条。
    if !wide {
        return false;
    }
    // 「够不够大」：一张页内的插图不该被撑满整屏。一屏 = 页高，没有页高（连续滚动）
    // 就用屏高 —— 两档同一条判据。
    let one_screen = page_height.or(screen_height).unwrap_or(f32::INFINITY);
    w >= column_width * 0.8 || h >= one_screen * 0.8
}

/// `<svg>` 这一格的高宽比，来自它的 `viewBox`（属性名同样已转小写）。不是 svg、或 viewBox
/// 读不出正数时给 `None`。
///
/// 这一格的比例是**版面**信息而不是图片信息：Calibre 那份写的是
/// `width="100%" height="100%" viewBox="0 0 563 751"`，意思是「铺满版心，按 563:751 摊开」。
/// 所以有 viewBox 就按它给高 —— 图暂时解不出来也照样留这一格，否则那一章高 0、
/// 在滚动链里根本不存在。
pub(super) fn svg_aspect(dom: &Dom, node: NodeId) -> Option<f32> {
    if !dom.is_element(node, "svg") {
        return None;
    }
    let view = dom.attr(node, "viewbox")?;
    let nums: Vec<f32> = view
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter_map(|part| part.trim().parse().ok())
        .collect();
    // `viewBox="min-x min-y width height"`：四位，且宽高都是正数。
    if nums.len() != 4 || nums[2] <= 0.0 {
        return None;
    }
    (nums[3] > 0.0).then_some(nums[3] / nums[2])
}

/// 一段行内内容展平后的产物。
pub(super) struct Collected {
    pub(super) text: String,
    pub(super) pieces: Vec<Piece>,
    /// 文本流里的图片占位。`at` 指向 `text` 里那个哨兵字符的字节起点。
    pub(super) images: Vec<ImageAtom>,
    /// ruby 容器占的那一段（基字 + 注音的并集），按起点升序 —— 收集器只会往后追加。
    ///
    /// 它唯一的用处是给断行禁则查「这个断点是不是落在容器内部」（[`Self::breaks_ruby`]）：
    /// 一个容器不许拆开排到两行上，否则注音会跟着自己的基字分家。
    pub(super) rubies: Vec<Range<usize>>,
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
    /// 沿字轴额外挪的 px，口径与 [`MarkerRun::offset`] 同一份：**物理**左偏移，
    /// 普通正文恒为 0（只有 ruby 注音那一族 piece 带非零值）。
    ///
    /// 注音不吃前进量（它的字符在宽度表里是 0），流动把它排在容器末尾，而 `ruby-align`
    /// 要的那一段在容器里头，所以折 [`LineRun`] 时原样带过去，绘制与命中各加一次同一个数。
    ///
    /// [`MarkerRun::offset`]: super::geometry::MarkerRun::offset
    /// [`LineRun`]: super::geometry::LineRun
    pub(super) x_offset: f32,
}

/// 段落里一张图片：锚定在文本流某个位置，宽度已按 CSS 与内在尺寸算好。
pub(super) struct ImageAtom {
    pub(super) node: NodeId,
    pub(super) href: String,
    pub(super) width: f32,
    pub(super) height: f32,
    /// 横向前进量 = 画出来的宽 + 两侧留白。脚注标记图在 `sup` 里紧贴前后字，
    /// 不加这一格就会压住「地」「等」的边；留白只进前进量，不进图片本身。
    pub(super) advance: f32,
    /// 图片在前进量里的左偏移（留白）。绘制 x = 行内游标 + `x_offset`。
    pub(super) x_offset: f32,
    /// 基线偏移（正数往上）。图片自己写在 `<img>` 上的 `vertical-align` 加上它所在
    /// 行内父级（`<sup>` 那类）的偏移 —— 图片不是文字，不能指望样式继承替它带上。
    pub(super) baseline_shift: f32,
    /// 哨兵在 `Collected::text` 里的字节起点。
    pub(super) at: usize,
    /// 这张图怎么在盒里摆（CSS 的 `object-fit`）。
    pub(super) fit: crate::content::format::css::computed::ObjectFit,
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

    /// 断点 `at` 落在某个 ruby 容器**内部**吗。
    ///
    /// 容器不许拆开排到两行上：注音得跟着自己的基字，拆开了就是上一行留基字、下一行留注音。
    /// 落在容器起止两边都算合法断点（`at == start` 是「容器整体挪到下一行」，
    /// `at == end` 是容器之后的正常断行），只有夹在中间才否掉。
    ///
    /// 表按起点升序（收集器只会往后追加），所以二分找最后一个起点早于 `at` 的容器。
    pub(super) fn breaks_ruby(&self, at: usize) -> bool {
        let idx = self.rubies.partition_point(|span| span.start < at);
        idx > 0 && at < self.rubies[idx - 1].end
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

    /// 作者写了尺寸就用它，没写给 `None`。`height` 那一族认 px 与**有基准的**百分比：
    /// 百分比折自包含块的**高**，而流式排版里父块的高常由内容算出来 —— CSS 在同一条
    /// 规则下也判 `auto`，所以「没写过」与「写了 `50%` 而上游没有既定高」是同一个数。
    /// 基准从 [`LayoutOptions::page_height`] 一路传下来，判据写在那一格的注释上。
    ///
    /// 只给流内块级盒用。替换元素（`<img>` / `<svg>`）走
    /// [`super::ports::image_size`]，那条路里百分比折得动（基准是图的可用宽）。
    pub(super) fn definite(length: Option<Length>, cb_h: Option<f32>) -> Option<f32> {
        match length {
            Some(Length::Px(v)) => Some(v.max(0.0)),
            Some(Length::Percent(p)) => cb_h.map(|basis| (p * basis).max(0.0)),
            _ => None,
        }
    }

    /// 相邻两个竖外边距折叠成一段净空（CSS 2.1 §10.3.4）。
    ///
    /// 规范那三档合成一个式子就够了：**最大正数 + 最小负数**。两个正数得较大值
    /// （段间距不显松靠的就是这一档），一正一负得两者之和，两个负数得较负的那个。
    ///
    /// `0` 是这个式子的恒等元，所以调用点用 `prev_bottom = 0.0` 起手就等价于
    /// 「前面没有兄弟」；从前这里写的是 `if prev_bottom > 0.0`，负下边距会同时踩中
    /// 「没有前驱」和「夹回 0」两个坑 —— 换成一趟加法就没这两件事了。
    pub(super) fn collapse_margins(top: f32, bottom: f32) -> f32 {
        top.max(bottom).max(0.0) + top.min(bottom).min(0.0)
    }

    /// 流内块级盒的横向求解（CSS 2.1 §10.3.3），返回 `(左边距, 边框盒宽)`。
    ///
    /// `avail` 是包含块的内容宽，同时是 `width` / `margin` / `padding` 这一族百分比的
    /// 折算基准 —— CSS 规定它们全部折自包含块宽，不折自这一块自己。三条规则照规范走：
    ///
    /// - `width: auto` 吃掉「减去两边与边框内边距之后的余量」，与这一格改之前逐字相同；
    ///   写了 `width` 或 `max-width` 就按折出的边框盒宽定下来（`max-width` 夹在**内容宽**
    ///   上再加边框与内边距，因为作者写的那三个数一律先过 [`ComputedStyle::content_size`]
    ///   —— `box-sizing: border-box` 时那一格里已经含着边框与内边距）。
    /// - `min-width` 夹在 `max-width` **之后**（CSS 2.1 §10.4：两者写出矛盾时赢的是
    ///   `min-width`），并且它同样管 `width: auto` 那一档 —— 填满之后还可以被抬到溢出，
    ///   浏览器就是这么溢出（`spare < 0` 那一支跟着把 `auto` 左边距收 0）。
    /// - 装不满时的余量归 `margin: auto`：两边都 `auto` 就均分，只有一边 `auto` 就全给它。
    ///   这一条就是「定宽块默认贴左缘、写着 `margin: 0 auto` 才居中」那一条的分界。
    ///   两边都不是 `auto` 时余量天然留在右边，右边距不必动 —— 与浏览器同解。
    /// - 装不下（`width` + 两边 > `avail`）时按溢出排，不缩作者写的数：over-constrained
    ///   那一档规范改的也是右边距。
    ///
    /// 为什么这一格只有一份：流内摆放有两条路都要问它 —— [`Self::children`]（实排）与
    /// [`Self::children_light`]（只量高）。抄两遍迟早漂，症状是滚动位置与屏幕上的块对不上。
    pub(super) fn solve_block_width(
        &self,
        style: &ComputedStyle,
        avail: f32,
        parent_x: f32,
    ) -> (f32, f32) {
        let [_, border_right, _, border_left] = style.border_drawn();
        let bl = border_left + style.padding_left.resolve(avail).max(0.0);
        let br = border_right + style.padding_right.resolve(avail).max(0.0);
        let auto_left = style.margin_left.is_auto();
        let auto_right = style.margin_right.is_auto();
        // `resolve` 给 `auto` 回 0，所以这两格先按「不占位」收着，余量最后再分回左边距。
        // **负数照算不夹**：§10.3.3 那条等式里外边距就是带符号的一项，`margin-left:
        // -20px` 配 `width: auto` 得的是「块左移 20px 且宽出 20px」这一解，浏览器同解。
        // 从前这里夹过 0，取值层已经把负数存进 `Length::Px` 了、排版期又归零，于是
        // 这条声明在 `examples/css-gap` 那张表上报「接住了」而屏上纹丝不动 ——
        // 《斗战狂潮》目录那 1223 个 `dt.tocl2` 的悬挂缩进就是这么丢的。
        // 内边距仍夹 0：CSS 里负 `padding` 本就是非法值，不属这一族。
        let ml_set = style.margin_left.resolve(avail);
        let mr_set = style.margin_right.resolve(avail);
        let room = (avail - ml_set - mr_set - bl - br).max(0.0);
        // 作者写的三个数先按 `box-sizing` 折成**内容宽**（式子只有一份，见
        // [`ComputedStyle::content_size`]）；`width: auto` 那一档不折 —— `room` 本来就是
        // 从可用宽扣掉边框内边距之后剩下的内容宽。
        let fold = |declared: Length| style.content_size(declared.resolve(avail).max(0.0), bl + br);
        let mut content = style.width.map_or(room, fold);
        if let Some(max_w) = style.max_width {
            content = content.min(fold(max_w));
        }
        // 夹序在 `max-width` 之后：见本函数文档里 §10.4 那一条。反过来写的话，
        // 「`max-width: 40px` + `min-width: 60px`」这种矛盾声明会得 40，浏览器得 60。
        if let Some(min_w) = style.min_width {
            content = content.max(fold(min_w));
        }
        let mut box_width = (content + bl + br).max(1.0);
        let mut ml = ml_set;
        let spare = avail - ml_set - mr_set - box_width;
        if spare > 0.0 && auto_left {
            // 两边都 `auto` 时各得一半：左边距拿一半，另一半就是留在右边的那段余量，
            // 不必再动右边距；只有一边 `auto` 时整个余量归它。
            ml += if auto_right { spare / 2.0 } else { spare };
        } else if spare < 0.0 && auto_left {
            ml = 0.0;
        }
        // **负边距把块推出页面左缘时收回到缘上。** 浏览器里这种溢出就留在屏幕外
        // （LTR 往左没有滚动），而阅读器的正文没有横向滚动条：《诡秘之主》角色页
        // `p.tt1 { margin: 1.5em 0 1em -1em }` 的「简介」「尊名」两格因此被屏缘切掉半字。
        // `width: auto` 的块负边距是**加在宽上**的（room 那一项），收回左缘就把那一截
        // 宽原样扣回，退回满幅；写了 `width` 的只挪不缩 —— 作者要的宽留着，
        // 右缘照浏览器溢出裁切。
        let off = (parent_x + ml).min(0.0);
        if off < 0.0 {
            ml -= off;
            if style.width.is_none() {
                box_width = (box_width + off).max(1.0);
            }
        }
        (ml, box_width)
    }

    /// 把作者写的 `height` / `max-height` / `min-height` 盖到内容算出来的高上。
    ///
    /// 写了 `height` 就按那一档定盒，没写就由内容撑；`max-height` 两种情况都夹一刀，
    /// `min-height` 夹在**之后**（CSS 2.1 §10.7：与 `max-height` 矛盾时 `min-height` 赢，
    /// 与横向那一族同一条）。`min-height` 对「由内容撑高」这一档是真会改屏的：
    /// 内容只有一行、作者要的是至少三行的空白（诗排版与填空册常见写法）。
    /// **内容照排、溢出可见**：CSS 里 `height` 不裁剪（`overflow` 这一版也不认），
    /// 行盒仍然画到盒子外面去，只是后面的兄弟从这一格的高度下移 —— 浏览器同一条规则，
    /// 跟着它比自创一套「看起来更合理」的取巧稳。
    ///
    /// 只给非替换的流内块用；替换元素的高由
    /// [`super::ports::image_size`] 从图的固有尺寸折。
    fn used_content_height(
        style: &ComputedStyle,
        inner: f32,
        pad_border: f32,
        cb_h: Option<f32>,
    ) -> f32 {
        // 作者写的三个数都先过 [`ComputedStyle::content_size`]：`box-sizing: border-box`
        // 时那一格里已经含上下的边框与内边距。`pad_border` 就是调用点那一对 `pt + pb`，
        // 由调用方传进来 —— 这一格自己不算边距，是为了不与「哪几条边算进去」再有一份定义。
        let fold = |px: f32| style.content_size(px, pad_border);
        let mut h = Self::definite(style.height, cb_h).map_or(inner, fold);
        if let Some(max_h) = Self::definite(style.max_height, cb_h) {
            h = h.min(fold(max_h));
        }
        if let Some(min_h) = Self::definite(style.min_height, cb_h) {
            h = h.max(fold(min_h));
        }
        h
    }

    /// 这一块的**子节点**折百分比高时用的那个数。
    ///
    /// 只有作者明确写了 `height` 才算得出：CSS 那条规则是「包含块的高没明确给定（由内容
    /// 撑出来）时，百分比高按 `auto`」，所以内容撑出来的高不进这一档，哪怕它是个正数。
    ///
    /// `max-height` / `min-height` 夹出来的那一档刻意也不进：夹子在子节点排完才落地，
    /// 反过来要子节点的百分比等这个数就是循环定义。浏览器同样按作者写的那个 `height` 折。
    pub(super) fn child_height_basis(
        style: &ComputedStyle,
        cb_h: Option<f32>,
        pad_border: f32,
    ) -> Option<f32> {
        Self::definite(style.height, cb_h).map(|px| style.content_size(px, pad_border))
    }

    /// 摆放 `container` 的流内子内容。`x`/`y` 是其内容盒左上角（绝对），返回内容高度。
    ///
    /// `width` 就是 `container` 的内容宽，因此它同时是子节点那一族百分比
    /// （`margin` / `padding` / `text-indent`）的**折算基准** —— CSS 规定这一族折自
    /// 包含块的内容宽，不是折自盒子自己的宽。
    ///
    /// `container_h` 是同一件事在竖轴上的那一份：`container` 的内容高，**仅在作者明确
    /// 给定过时**是 `Some`（判据见 [`Self::child_height_basis`]）。子节点的 `height: NN%`
    /// 折它，`None` 时那一族按 `auto`。
    /// 这一块是不是标题（`<h1>`–`<h6>`）。叠图按「同一个小节」生效，而小节的边界就是
    /// 标题 —— 跨过一条标题就不再叠，读者的「上一节三张图 / 新一节另一组图」不会被
    /// 连成一堆。
    fn is_heading(&self, node: NodeId) -> bool {
        matches!(
            self.dom.tag_name(node),
            Some("h1" | "h2" | "h3" | "h4" | "h5" | "h6")
        )
    }

    /// 这一块是不是一个「图单元」：恰好一张图，外加最多一段**短图注**。
    ///
    /// 与 [`single_image`] 的分别是后者只认「一个字的字都没有」那一档，而多看/掌阅那族
    /// 导出的画廊 cell 长这样（《诡秘之主》ffwp000.xhtml「诡秘地理日志」连着九个）：
    ///
    /// ```html
    /// <div class="duokan-image-gallery-cell">
    ///   <img src="../Images/xm001.jpg"/><p class="tpcrb"><b>灾祸之城</b></p>
    /// </div>
    /// ```
    ///
    /// 图旁边站着一段注，`single_image` 当场回 `None` —— 这本书的画廊因此从来没叠过，
    /// 一张注一块地排成一条长带。
    ///
    /// **注写在图前也算**（《哈利·波特全集》art.xhtml 的画廊就是
    /// `<p>美版系列封面</p><img>`）：位置不参与判定，只认「一张图 + 最多一段不超字的注」。
    /// 摆的时候由 [`Ctx::children`] 把注挪到图后（这一叠的下面），见 [`Ctx::caption_last`]。
    fn image_unit(&self, node: NodeId) -> Option<NodeId> {
        self.image_parts(node).map(|(img, _)| img)
    }

    /// 同 [`Self::image_unit`]，把那段图注也交出来（没有注就是 `None`）。
    fn image_parts(&self, node: NodeId) -> Option<(NodeId, Option<NodeId>)> {
        if self.dom.is_element(node, "img") {
            return Some((node, None));
        }
        let mut img: Option<NodeId> = None;
        let mut caption: Option<NodeId> = None;
        for &child in self.dom.get(node).children.iter() {
            let n = self.dom.get(child);
            if n.kind == NodeKind::Text && n.text.trim().is_empty() {
                continue; // 标签之间的换行
            }
            if let Some(found) = single_image(self.dom, child) {
                if img.replace(found).is_some() {
                    return None; // 两张图 → 这是图文混排的一块，不是「图 + 注」
                }
                continue;
            }
            let style = self.style_of(child);
            // 图注那一格：块级、不含图、不含标题、字数在 `STACK_CAPTION_MAX` 以内。
            // 超字数就当成「图后面紧跟的一段正文」，整块不进叠（宁严不宽）。
            //
            // **不要求注已经见过图**：注写在图前是同一件事（哈利波特那本就是这样）。
            let words = text_len_in(self.dom, child);
            if caption.is_some()
                || has_image(self.dom, child)
                || self.is_inline_flow(child, style)
                || self.is_heading(child)
                || words == 0
                || words > STACK_CAPTION_MAX
            {
                return None;
            }
            caption = Some(child);
        }
        img.map(|img| (img, caption))
    }

    /// 这一块的图注写在图**前**时交回那个孩子（叠图那一趟把它挪到队尾用）。
    ///
    /// 「前」按孩子顺序算，不按图的节点位置：图可能裹在一层 `<div>` 里，那一层才是它的
    /// 位置。注本来就在图后的（诡秘那族）回 `None`，那一趟连缓存都照走。
    fn caption_before_image(&self, node: NodeId) -> Option<NodeId> {
        let (img, caption) = self.image_parts(node)?;
        let caption = caption?;
        let kids = self.children_of(node);
        let cap_at = kids.iter().position(|kid| *kid == caption)?;
        let img_at = kids
            .iter()
            .position(|kid| single_image(self.dom, *kid) == Some(img))?;
        (cap_at < img_at).then_some(caption)
    }

    /// 一个图单元里那张图的**显示尺寸**，与画出来那一格同源（判据见 [`Self::picture_box`]）。
    ///
    /// 量显示尺寸而不是原图像素：读者的「铺满」是默认档，会把一张 80% 宽的图放大到整屏，
    /// 按像素比就会把「其实一样大」的两张判成不能叠。
    fn unit_box(&self, img: NodeId, width: f32) -> Option<(f32, f32)> {
        let src = picture_src(self.dom, img)?;
        let style = self.style_of(img);
        let [bt, br, bb, bl] = style.border_drawn();
        let pl = bl + style.padding_left.resolve(width).max(0.0);
        let pr = br + style.padding_right.resolve(width).max(0.0);
        let pt = bt + style.padding_top.resolve(width).max(0.0);
        let pb = bb + style.padding_bottom.resolve(width).max(0.0);
        let (w, h, _) = self.picture_box(img, &src, &style, (width - pl - pr).max(1.0), pt + pb)?;
        Some((w, h))
    }

    /// 从 `from` 起**连续**可叠的那一串兄弟（返回下标，第一个就是 `from`）。凑不满两张回空。
    ///
    /// 两条口径：显示尺寸相同（宽 ±0.5，高差 ≤ [`STACK_SIZE_TOLERANCE`]）、不跨标题。
    /// 整页大的图也允许叠：一页画廊的每张人物图都可能接近一页高。「同尺寸」是「一张盖得住另一张」的意思 —— 尺寸
    /// 差得多的两张叠起来，矮的那张会在高的那张身上露出一道边，读者看到的是排版坏了。
    fn stack_run(&self, kids: &[NodeId], from: usize, width: f32) -> Vec<usize> {
        let mut run: Vec<usize> = Vec::new();
        let mut base: Option<(f32, f32)> = None;
        for at in from..kids.len() {
            let n = self.dom.get(kids[at]);
            // 标签之间的换行与缩进不是内容：真书（尤其 Calibre / 多看导出）在每一个
            // gallery cell 之间都夹着一个纯空白文本节点，把它当成「中断」等于这一叠
            // 永远只有一格 —— 《诡秘之主》与《哈利·波特全集》的画廊都没叠过，就是它。
            if n.kind == NodeKind::Text && n.text.trim().is_empty() {
                continue;
            }
            let style = self.style_of(kids[at]);
            if style.display == Display::None {
                continue;
            }
            if self.is_heading(kids[at]) {
                break;
            }
            let (w, h) = match self
                .image_unit(kids[at])
                .and_then(|img| self.unit_box(img, width))
            {
                Some(size) => size,
                None => break,
            };
            if h <= 0.0 {
                break;
            }
            match base {
                None => base = Some((w, h)),
                Some((w0, h0))
                    if (w - w0).abs() <= 0.5
                        && (h - h0).abs() <= (h.max(h0) * STACK_SIZE_TOLERANCE).max(0.5) => {}
                Some(_) => break,
            }
            run.push(at);
        }
        if run.len() < 2 {
            run.clear();
        }
        run
    }

    pub(super) fn children(
        &mut self,
        container: NodeId,
        x: f32,
        y: f32,
        width: f32,
        container_h: Option<f32>,
    ) -> f32 {
        if self.light {
            return self.children_light(container, x, y, width, container_h);
        }
        let container_style = self.style_of(container);
        let mut cursor = y;
        let mut prev_bottom = 0f32;
        let mut pending: Vec<NodeId> = Vec::new();
        let mut kids: Vec<NodeId> = self.children_of(container).to_vec();
        // 这一趟要把哪个孩子挪到队尾（图注摆到这一叠下面，见 [`Self::caption_last`]）。
        if let Some(caption) = self.caption_last
            && let Some(at) = kids.iter().position(|kid| *kid == caption)
        {
            let caption = kids.remove(at);
            kids.push(caption);
        }
        // 已经并进上一叠的最后一个兄弟的下标：叠起来的一串只占一格，后面的成员不再
        // 各自摆一遍。`None` = 这一层没有叠（写 `Some(0)` 或 `0` 会把第一个孩子吃掉）。
        let mut skip_until: Option<usize> = None;
        // 列表序号。CSS 的列表计数器就是「同层 `display: list-item` 的兄弟按文档序」，
        // `<ol start>` 挪起点（所以先减 1，第一项 ++ 之后正好落回 `start`），
        // `<li value>` 挪这一项并把续值接在它后面。
        let mut ordinal: i64 =
            attr_count(self.dom, container, "start").map_or(0, |start| start - 1);

        for (at, &child) in kids.iter().enumerate() {
            if skip_until.is_some_and(|last| at <= last) {
                continue;
            }
            let style = self.style_of(child);
            if style.display == Display::None {
                continue;
            }
            // 绝对定位：脱流，收起来等整章排完再摆。**排在行内判断之前** ——
            // `<span style="position:absolute">` 也是脱流的，不能当行内内容攒进段落。
            if matches!(style.position, Positioning::Absolute | Positioning::Fixed) {
                self.absolute.push((child, cursor));
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
                    Slot {
                        x,
                        y: cursor,
                        width,
                        basis: width,
                    },
                    &Insets(bands.as_deref()),
                    container_style,
                );
                prev_bottom = 0.0;
            }
            let is_item = style.display == Display::ListItem;
            if is_item {
                ordinal = attr_count(self.dom, child, "value").unwrap_or(ordinal + 1);
            }
            // 「这一块里只有一张图」时**上外边距不许是负的**：作者常用 `margin-top: -15px`
            // 把标题图往上贴，而阅读器顶上那条页眉带是不透明的，图会被它切掉一条
            // （《哈利·波特全集》`v1.xhtml` 的 `hptitle.png` 顶边正好少 15px）。
            // 只管「只有图」的块：正文块的负上边距是作者让它压在上一段上，照浏览器办。
            let mt = style.margin_top.resolve(width);
            let mt = if mt < 0.0 && single_image(self.dom, child).is_some() {
                0.0
            } else {
                mt
            };
            // 兄弟之间折叠取较大值而不是相加：这是段间距不显松的唯一原因。负数那
            // 三档（一正一负求和、两负取较负）见 [`Self::collapse_margins`]，`0` 是
            // 它的恒等元，所以「前面没有兄弟」这一档不必再单独判。
            cursor += Self::collapse_margins(mt, prev_bottom);
            // `clear` 把这一块整块推到那一侧浮动盒的下缘之下。净空算的是**边框**的位置，
            // 所以推完之后前面那段折叠的右边距不再起作用。
            if let Some(clear_to) = self.clear_bottom(style.clear) {
                cursor = cursor.max(clear_to);
            }
            // 浮动盒：离开常规流。摆在当前游标那一档的侧边，**不**推进 `cursor`，
            // 于是后面每一段都从同一处起排、只是行带被它折窄。认不出宽度就落回常规流。
            if style.float != Float::None
                && self.place_float(child, style, x, cursor, width, container_h)
            {
                continue;
            }
            // 横向那两个数一起解：`width` 写了就按作者的数收窄盒子，`margin: auto`
            // 因此第一次有东西可均分（判据见 [`Self::solve_block_width`]）。
            let (ml, box_width) = self.solve_block_width(style, width, x);
            // 一整串图叠成**同一格**：所有成员摆在当前 cursor 上，一叠只留一张图的高。
            // 「现在看第几张」不住这里（那是界面的状态，改了不许重排），排版只给每一行
            // 标上「这一叠的第几张、一共几张」。图注跟着它那张图走，所以换图就是换注。
            let run = self.stack_run(&kids, at, width);
            if run.len() >= 2 {
                let id = self.stack_seq;
                self.stack_seq += 1;
                let count = run.len() as u16;
                let mut stack_h = 0.0f32;
                for (k, &member) in run.iter().enumerate() {
                    let mstyle = self.style_of(kids[member]);
                    let (mml, mw) = self.solve_block_width(mstyle, width, x);
                    let start = self.lines.len();
                    // 图注一律摆在这一叠的**下面**：作者的注写在图前时（哈利波特画廊那种
                    // `<p>注</p><img>`），排这一块时把它挪到队尾 —— 叠起来只剩一张图，
                    // 注是这一叠的说明，贴着图的下面读才顺。注本来就在图后的不动。
                    let caption = self.caption_before_image(kids[member]);
                    self.caption_last = caption;
                    let h = if mstyle.display == Display::Table {
                        self.table(kids[member], x + mml, cursor, mw, width, container_h)
                    } else if caption.is_some() {
                        // 换过顺序的这一块**不进缓存**：`BlockKey` 里没有「这一趟换了孩子
                        // 顺序」这一格，命中一份按原顺序排好的结果就等于没换。
                        self.block_uncached(kids[member], x + mml, cursor, mw, width, container_h)
                    } else {
                        self.block(kids[member], x + mml, cursor, mw, width, container_h)
                    };
                    self.caption_last = None;
                    for line in &mut self.lines[start..] {
                        line.stack = Some(StackMark {
                            id,
                            index: k as u16,
                            count,
                        });
                    }
                    stack_h = stack_h.max(h);
                }
                cursor += stack_h;
                // 叠内各张的外边距不参与兄弟折叠：它们本来就不是一块接一块排下来的。
                prev_bottom = 0.0;
                skip_until = run.last().copied();
                continue;
            }
            let lines_start = self.lines.len();
            let placed_y = cursor;
            let block_h = if style.display == Display::Table {
                self.table(child, x + ml, cursor, box_width, width, container_h)
            } else {
                self.block(child, x + ml, cursor, box_width, width, container_h)
            };
            cursor = placed_y + block_h;
            // 标记贴在 `block()` **之后**：见 [`Self::attach_marker`] 那三条口径。
            if is_item {
                self.attach_marker(child, style, ordinal.max(1) as usize, lines_start);
            }
            prev_bottom = style.margin_bottom.resolve(width);
        }
        if !pending.is_empty() {
            let line_px = (container_style.font_size * container_style.line_height).max(1.0);
            let bands = self.float_bands(x, cursor, width, line_px);
            cursor += self.wrap_paragraph(
                container,
                &pending,
                Slot {
                    x,
                    y: cursor,
                    width,
                    basis: width,
                },
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
                .map(|k| self.float_insets(x, width, y + k as f32 * line_px, line_px))
                .collect(),
        )
    }

    /// `clear` 要把这一块推到多低：受影响那一侧所有浮动盒下缘的最大值。
    ///
    /// 左右分别算，不是都按 `both` 处理：`clear: right` 写在「左边还有空」的段上，
    /// 折成 `both` 会白白多挪一段。
    /// 这一侧浮动盒的下缘 —— **`None` = 这一句 `clear` 没有东西可让**。
    ///
    /// 返回 `Option` 而不是「没有就给 0」：调用点要的是「把游标推到那下面」，
    /// 而负的相邻外边距现在能把游标推到容器顶之上（<0），那时 0 不是「没有净空」
    /// 而是「净空到 0」，会把刚折叠出来的负数整个抹平。
    fn clear_bottom(&self, clear: Clear) -> Option<f32> {
        if clear == Clear::None {
            return None;
        }
        self.floats
            .iter()
            .filter(|f| match clear {
                Clear::Left => !f.right,
                Clear::Right => f.right,
                _ => true,
            })
            .map(|f| f.bottom())
            .reduce(f32::max)
    }

    /// 浮动块的**边框盒宽**（外边距由调用方加，因为让位的那一圈是外边距盒）。
    ///
    /// 认三档，都不猜：
    ///
    /// - 作者自己写了 `width`（《哈利·波特》的 `.muen .left { width: 20%; float: left }`）：
    ///   那一档本来就是既定值，不必收缩，按内容盒折出宽再加边框与内边距。
    ///   只有 `max-width` 没写 `width` 时**不走这一档** —— 那等于把「不超过」当成
    ///   「就是」，满宽的浮动盒会把后面每一行的可用宽压成 0，正文直接消失。
    /// - 块里**一个流内子节点都没有**（封面页那个空楔块）：空内容的 shrink-to-fit 恒等于
    ///   0，宽就只剩它自己的内边距与边框那一圈。落点与症状写在下面那一臂的注释上。
    /// - 「整块内容就是一张图」（《哈利·波特》那 78 个 `div.img1` / `div.img2` 都是
    ///   `<div><img width="20%"></div>`）：按图的尺寸折。
    ///
    /// 三档都对不上时返回 `None`，按**不浮动**处理并留一条 `debug!`。真正的
    /// shrink-to-fit 要 max-content，等于再跑一趟排版；猜一个宽度会把整页正文挤成
    /// 一条窄柱，那比插图不绕文糟得多。
    fn float_box_width(&self, node: NodeId, style: &ComputedStyle, basis: f32) -> Option<f32> {
        let [_, d_border_right, _, d_border_left] = style.border_drawn();
        let d_bl = d_border_left + style.padding_left.resolve(basis).max(0.0);
        let d_br = d_border_right + style.padding_right.resolve(basis).max(0.0);
        if let Some(width) = style.width {
            // 三个数先过 [`ComputedStyle::content_size`]（与流内块那一条同源），夹序也同
            // 一条：`max-width` 先夹、`min-width` 后夹。浮动盒从前不看 `min-width`，
            // 于是 `.x { min-width: 120px; float: left }` 排出来的盒比作者要窄 ——
            // 这一格与流内块共用式子之后不会再分叉。
            let fold = |declared: Length| {
                style.content_size(declared.resolve(basis).max(0.0), d_bl + d_br)
            };
            let mut content = fold(width);
            if let Some(max_w) = style.max_width {
                content = content.min(fold(max_w));
            }
            if let Some(min_w) = style.min_width {
                content = content.max(fold(min_w));
            }
            return Some((content + d_bl + d_br).max(1.0));
        }
        // 空内容的浮动盒：宽就是它的内边距与边框那一圈。这不是猜 —— 一个字都没有的块
        // 的 shrink-to-fit 恒等于 0，而这一档在书里有真用途：`.wedge { float:left;
        // height:50%; margin-bottom:-360px }` 那种**空楔块**是靠高与负边距干活的。
        // 从前这里认不出宽，楔块就落回常规流：它自己吃掉 50% 那一截高，负下边距再把
        // 后面的容器拉到**页面顶之上**（页比内容矮时是个负数，切片窗口从 0 起，那截
        // 内容就再也滚不到）。浏览器那边浮动盒不吃垂直高，而 `clear` 只往下推不往回拉，
        // 容器贴在 0 —— 认下这一档才是同一条形状。
        if !self
            .children_of(node)
            .iter()
            .any(|&c| self.style_of(c).display != Display::None)
        {
            return Some(d_bl + d_br);
        }
        let img = single_image(self.dom, node)?;
        let img_style = self.style_of(img);
        let src = self.dom.attr(img, "src")?;
        let image = self.images.resolve(src)?;
        // 图的百分比宽折自**这个浮动盒的内容宽**，不是折自版心：`.img1` 那类
        // 带内边距的插图框里 `<img width="100%">` 要铺满的是框里面那一圈。
        //
        // 但「浮动盒自己多宽」这一趟不能解百分比 —— 盒宽还没定，拿它当基准是循环
        // 定义。CSS 的 shrink-to-fit 在算内禀宽时把百分比当 auto：先按图的固有宽
        // 收缩，之后图在盒内再按百分比落位。浏览器实测（434×500 的图 +
        // `width="40%"`）：可用宽 1000 时盒宽 434、图 173.6；可用宽 288 时盒宽被
        // 夹到 288、图 114.9。`image_size` 末尾那刀「绝不溢出容器」正好给出同一种夹法。
        let percentage = matches!(img_style.width, Some(Length::Percent(_)))
            || matches!(img_style.height, Some(Length::Percent(_)));
        let intrinsic;
        let sizing_style = if percentage {
            intrinsic = {
                let mut style = img_style.clone();
                if matches!(style.width, Some(Length::Percent(_))) {
                    style.width = None;
                }
                if matches!(style.height, Some(Length::Percent(_))) {
                    style.height = None;
                }
                style
            };
            &intrinsic
        } else {
            img_style
        };
        let (w, _) = image_size(
            sizing_style,
            image.width,
            image.height,
            (basis - d_bl - d_br).max(1.0),
            None,
        );
        // 图自己的左右外边距、边框与内边距都要算进来：`.img1` 里那句
        // `margin-right: .8em` 正是靠这一步变成图与字之间的那条缝。
        let [_, i_border_right, _, i_border_left] = img_style.border_drawn();
        let inner = w
            + i_border_left
            + i_border_right
            + img_style.padding_left.resolve(basis).max(0.0)
            + img_style.padding_right.resolve(basis).max(0.0)
            + img_style.margin_left.resolve(basis)
            + img_style.margin_right.resolve(basis);
        // 地板与上面 `width` 那一档同一个数：这一族现在把带符号的外边距算进来了，
        // `margin: -200px` 那种写法能把边距盒折到零以下，而负宽的盒会让折行那一趟
        // 拿不到可写的宽。
        Some((inner + d_bl + d_br).max(1.0))
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
        cb_h: Option<f32>,
    ) -> bool {
        let basis = width;
        let Some(box_width) = self.float_box_width(node, style, basis) else {
            tracing::debug!(
                "float: <{}> 认不出收缩宽度，这一块按不浮动排",
                self.dom.tag_name(node).unwrap_or("?")
            );
            return false;
        };
        let ml = style.margin_left.resolve(basis);
        let mr = style.margin_right.resolve(basis);
        let mt = style.margin_top.resolve(basis);
        let mb = style.margin_bottom.resolve(basis);
        // 外边距盒不比版心窄的浮动盒旁边一个字都写不下。浏览器会把这一段的第一行
        // 整体下移到图之下（CSS 2.1 §9.5），结局与「不浮动、按常规流走」一样；
        // 这里直接落回常规流 —— 否则后面的正文会被挤成一条宽 1px 的单字柱，字还
        // 溢到版心之外。《哈利·波特》作者简介页在手机宽度上就是这一档：图的固有宽
        // 434 比版心还宽，`width="40%"` 的浮动盒因此铺满整幅。
        if box_width + ml + mr >= width {
            return false;
        }
        let right = style.float == Float::Right;
        // 靠左的贴左缘（含它自己的左边距），靠右的贴右缘 —— 让位的那一圈是外边距盒。
        let box_x = if right {
            x + width - mr - box_width
        } else {
            x + ml
        };
        let saved = std::mem::take(&mut self.floats);
        let height = self.block(node, box_x, y + mt, box_width, basis, cb_h);
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
    ///
    /// `container_h` 与 `children` 那一条同义（子节点折百分比高的基准，判据见
    /// [`Self::child_height_basis`]）：**两趟必须吃同一个基准**，否则量出来的高与
    /// 实排的高不是一个数，症状是滚动位置与块对不上。
    fn children_light(
        &mut self,
        container: NodeId,
        x: f32,
        y: f32,
        width: f32,
        container_h: Option<f32>,
    ) -> f32 {
        let mut cursor = y;
        let mut prev_bottom = 0f32;

        for &child in self.children_of(container) {
            let style = self.style_of(child);
            if style.display == Display::None {
                continue;
            }
            // 绝对定位不撑高、也不在量高这一趟摆（要把整章的高算出来才有基准）。
            if matches!(style.position, Positioning::Absolute | Positioning::Fixed) {
                continue;
            }
            // 外边距一律带符号照算，与 `children` 同一条口径（判据见
            // [`Self::solve_block_width`] 那一段）。
            let ml = style.margin_left.resolve(width);
            let mr = style.margin_right.resolve(width);
            let mt = style.margin_top.resolve(width);
            let mb = style.margin_bottom.resolve(width);
            cursor += Self::collapse_margins(mt, prev_bottom);
            if let Some(clear_to) = self.clear_bottom(style.clear) {
                cursor = cursor.max(clear_to);
            }

            // 浮动盒：量它的高（里面看不见外面的浮动表），但**不推进** `cursor`。
            // 这一档不走 [`Self::solve_block_width`]：CSS 里浮动盒的 `auto` 边距一律按 0，
            // 且它的宽是收缩的而不是填满余量，与流内块是两条式子。
            if style.float != Float::None {
                let right = style.float == Float::Right;
                if let Some(box_width) = self.float_box_width(child, style, width)
                    && box_width + ml + mr < width
                {
                    let box_x = if right {
                        x + width - mr - box_width
                    } else {
                        x + ml
                    };
                    let saved = std::mem::take(&mut self.floats);
                    let height = self.block_light(child, box_width, width, container_h);
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
                // 与 [`Self::place_float`] 同一条「满幅浮动盒落回常规流」的判据：
                // 两趟必须同进同退，否则量出来的高与实排的高差一整张图。
            }

            // 流内块：横向与 `children` 同一条求解式（定宽收窄 + `auto` 均分）。
            let (used_ml, box_width) = self.solve_block_width(style, width, x);
            // 表格自己记块（它要逐格记），所以这条分支不能落到下面统一 `push_block` 那一句。
            if style.display == Display::Table {
                let height = self.table(child, x + used_ml, cursor, box_width, width, container_h);
                cursor += height;
                prev_bottom = mb;
                continue;
            }
            let height = self.block_light(child, box_width, width, container_h);

            // 记录块（无行盒）。`x` 与宽都取求解后的那两个数 —— 与 `children` 那条路
            // 逐字同源，轻量档的「量高」与「实排」才给出同一个盒子。
            self.blocks.push(BlockBox {
                node: child,
                x: x + used_ml,
                y: cursor,
                width: box_width,
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

    /// 摆一张表（`display: table`），返回边框盒高。
    ///
    /// 实排与量高两条路共用这一份：网格、每列的固有宽、列宽分配、每行的行高与格子落点
    /// 全在这里算**一次**，`self.light` 只决定「每格是 `block` 还是 `block_light`」。
    /// 分开写两份的症状是滚动位置与屏上的块对不上，而那种错在无头测试里只表现为
    /// 「总高差几个像素」。
    ///
    /// 单元格**顶对齐**（CSS 表格格的默认 `vertical-align: top`），所以格子内容按行顶排出
    /// 之后不必平移；行高只用来把格子的**盒子**拉满 —— 边框与底色因此铺满一整行，
    /// 与浏览器一致。落地范围的边界写在 `layout::table` 的模块文档上。
    fn table(
        &mut self,
        node: NodeId,
        x: f32,
        y: f32,
        width: f32,
        basis: f32,
        cb_h: Option<f32>,
    ) -> f32 {
        let style = self.style_of(node);
        let [bt, br, bb, bl] = style.border_drawn();
        let pl = bl + style.padding_left.resolve(basis).max(0.0);
        let pr = br + style.padding_right.resolve(basis).max(0.0);
        let pt = bt + style.padding_top.resolve(basis).max(0.0);
        let pb = bb + style.padding_bottom.resolve(basis).max(0.0);
        let grid = table::collect_grid(self.dom, self.styles, node);
        if grid.cells.is_empty() || grid.columns == 0 {
            // 空表（只有 `<caption>` 或格式不对）：留一个 0 内容高的盒子，
            // 它的 padding / 边框照画 —— 作者写的盒模型不该因为没格子就消失。
            let index = self.push_block(node, x, y, width, String::new(), true);
            return self.close_block(index, pt + pb);
        }
        // `border-spacing` 只在 `separate`（初值）下生效；塌陷模式里它被忽略（CSS 2.1 §17.6.1）。
        let spacing = if style.border_collapse {
            [0.0f32, 0.0]
        } else {
            style.border_spacing
        };
        let gaps_x = spacing[0] * (grid.columns.saturating_sub(1)) as f32;
        let available = (width - pl - pr - gaps_x).max(1.0);
        // 每列的固有宽。跨格的固有宽按**列数均摊**（CSS 按比例分，这里取均分）：
        // 一格跨两列要 100px，那两列各记 50。
        let mut columns = vec![table::Column::default(); grid.columns];
        for cell in &grid.cells {
            let intrinsic = self.cell_intrinsic(cell.node, available);
            let span = cell.colspan as f32;
            let share = table::Column {
                min: intrinsic.min / span,
                max: intrinsic.max / span,
                fixed: intrinsic.fixed.map(|fixed| fixed / span),
            };
            let end = (cell.column + cell.colspan).min(grid.columns);
            for column in &mut columns[cell.column..end] {
                column.absorb(share);
            }
        }
        // CSS 里 `width: auto` 的表格**收缩到内容宽**（上限是可用宽），不铺满版心 ——
        // 一张两列的小表被拉成整页宽，正是「表格看起来坏了」最典型的那个症状。
        // 收缩之后 `margin: auto` 的中缝要重算：`solve_block_width` 是按可用宽均分的。
        let mut x = x;
        let mut box_width = width;
        if style.width.is_none() {
            let natural: f32 =
                columns.iter().map(|c| c.max.max(1.0)).sum::<f32>() + gaps_x + pl + pr;
            box_width = natural.min(width);
            if box_width < width {
                let (had_ml, _) = self.solve_block_width(style, width, x);
                let ml = match (
                    style.margin_left == Length::Auto,
                    style.margin_right == Length::Auto,
                ) {
                    (true, true) => (width - box_width) / 2.0,
                    (true, false) => width - box_width,
                    _ => had_ml,
                };
                x = x - had_ml + ml;
            }
        }
        let content_width = (box_width - pl - pr).max(1.0);
        // 列宽是在**扣掉缝之后**的那一段里分的 —— 忘了这一步，缝会被当余量摊进列里
        // （两列各 32、缝 10 的表会排成两列各 37 而没有缝）。
        let widths = table::solve_columns(&columns, (content_width - gaps_x).max(1.0));
        // 每列沿横轴的起点（含缝）。
        let mut col_left = Vec::with_capacity(grid.columns);
        let mut cursor = 0.0f32;
        for (column, width) in widths.iter().enumerate() {
            col_left.push(cursor);
            cursor += width;
            if column + 1 < grid.columns {
                cursor += spacing[0];
            }
        }
        let span_width = |cell: &table::Cell| -> f32 {
            let end = (cell.column + cell.colspan).min(grid.columns);
            let span = &widths[cell.column..end];
            let mut width: f32 = span.iter().sum();
            // 跨过的列之间那几道缝也算进这一格。
            width += spacing[0] * span.len().saturating_sub(1) as f32;
            width
        };
        let index = self.push_block(node, x, y, box_width, String::new(), true);
        /// 一格排完之后记下的账：它的块/行区间、内容高、跨了哪几列几行。
        struct Placed {
            block_start: usize,
            block_end: usize,
            line_start: usize,
            line_end: usize,
            height: f32,
            cell: table::Cell,
            width: f32,
        }
        // 逐格先按 **y = 0** 排：行高要等所有格子都量完才知道（跨行格还会把行撑高），
        // 位置最后统一挪 —— 与块缓存命中时的平移是同一件事。
        let mut placed: Vec<Placed> = Vec::with_capacity(grid.cells.len());
        for cell in &grid.cells {
            let cell_w = span_width(cell);
            let cell_x = x + pl + col_left[cell.column];
            let block_start = self.blocks.len();
            let line_start = self.lines.len();
            let height = if self.light {
                let height = self.block_light(cell.node, cell_w, cell_w, None);
                // 量高这条路不产出行盒，格子自己的盒子要在这里补记 ——
                // 与 `children_light` 给普通块补记那一条同一个理由。
                self.blocks.push(BlockBox {
                    node: cell.node,
                    x: cell_x,
                    y: 0.0,
                    width: cell_w,
                    height,
                    text: String::new(),
                    lines: 0..0,
                    border_box: true,
                });
                height
            } else {
                self.block(cell.node, cell_x, 0.0, cell_w, cell_w, None)
            };
            placed.push(Placed {
                block_start,
                block_end: self.blocks.len(),
                line_start,
                line_end: self.lines.len(),
                height,
                cell: *cell,
                width: cell_w,
            });
        }
        // 行高：单行格先定自然高，再让跨行格把缺口**均摊**进去（CSS 按比例摊，这里均分）。
        // 跨行格按 rowspan 从小到大处理，这样小的先撑、大的拿到的是已经撑过的行高。
        let row_count = grid.slots.len().max(1);
        let mut row_h = vec![0.0f32; row_count];
        for slot in &placed {
            if slot.cell.rowspan == 1 && slot.cell.row < row_count {
                row_h[slot.cell.row] = row_h[slot.cell.row].max(slot.height);
            }
        }
        let mut spanning: Vec<usize> = (0..placed.len())
            .filter(|i| placed[*i].cell.rowspan > 1)
            .collect();
        spanning.sort_by_key(|i| placed[*i].cell.rowspan);
        for index_ in spanning {
            let start = placed[index_].cell.row;
            let end = (start + placed[index_].cell.rowspan).min(row_count);
            if end <= start {
                continue;
            }
            let have: f32 = row_h[start..end].iter().sum();
            let deficit = placed[index_].height - have;
            if deficit > 0.0 {
                let share = deficit / (end - start) as f32;
                for height in &mut row_h[start..end] {
                    *height += share;
                }
            }
        }
        // 行顶（含行间的缝）。
        let mut row_top = vec![0.0f32; row_count];
        let mut cursor = 0.0f32;
        for row in 0..row_count {
            row_top[row] = cursor;
            cursor += row_h[row];
            if row + 1 < row_count {
                cursor += spacing[1];
            }
        }
        // 摆到位：内容整体挪到行顶，盒子拉到跨行总高，再按 `vertical-align` 挪内容。
        for slot in placed {
            let start = slot.cell.row.min(row_count - 1);
            let end = (slot.cell.row + slot.cell.rowspan)
                .min(row_count)
                .max(start + 1);
            let spanned =
                row_h[start..end].iter().sum::<f32>() + spacing[1] * (end - start - 1) as f32;
            let top = y + pt + row_top[start];
            if top != 0.0 && !self.light {
                for line in &mut self.lines[slot.line_start..slot.line_end] {
                    line.y += top;
                }
                for block in &mut self.blocks[slot.block_start..slot.block_end] {
                    block.y += top;
                }
            }
            self.blocks[slot.block_start].x = x + pl + col_left[slot.cell.column];
            self.blocks[slot.block_start].y = top;
            self.blocks[slot.block_start].width = slot.width;
            self.blocks[slot.block_start].height = spanned.max(slot.height);
            let valign = self
                .style_of(self.blocks[slot.block_start].node)
                .cell_valign;
            let dy = match valign {
                CellVAlign::Top => 0.0,
                CellVAlign::Middle => (spanned - slot.height) / 2.0,
                CellVAlign::Bottom => spanned - slot.height,
            };
            if dy > 0.0 && !self.light {
                for line in &mut self.lines[slot.line_start..slot.line_end] {
                    line.y += dy;
                }
                // 第 0 块是格子自己的边框盒（它已经拉到跨行总高，不动）；里面那些跟着挪。
                for block in &mut self.blocks[slot.block_start + 1..slot.block_end] {
                    block.y += dy;
                }
            }
        }
        self.close_block(
            index,
            pt + Self::used_content_height(style, cursor, pt + pb, cb_h) + pb,
        )
    }

    /// 摆绝对定位的盒：脱流，按 `left` / `right` / `top` / `bottom` 相对**这一章的盒**摆。
    ///
    /// 必须在整章的高定下来之后调用（`top` / `bottom` 的百分比基准是包含块高）。
    /// 这些盒**不撑开**这一章的高 —— 与 CSS 一致（绝对定位不参与祖先的高度计算），
    /// 也让分页不会被封面那一块装饰改写。
    ///
    /// 落点：`left` / `top` 定位的是**外边距盒**的外缘，所以 `margin` 加在偏移上 ——
    /// 封面那套 `left:50%; margin-left:-9.5em; width:19em` 的居中写法靠的正是它。
    /// 没写 `width` 时铺满包含块（没有 shrink-to-fit，见模块文档）。
    ///
    /// 返回摆出来的所有盒的下缘（章内坐标）：它们不进祖先的高度，但进**滚动溢出**，
    /// 调用方据此把章高补到能装下它们（见 `layout_inner` 的调用点）。
    pub(super) fn place_absolutes(&mut self, cb_w: f32, page: Option<f32>, content_h: f32) -> f32 {
        let cb_h = page.unwrap_or(content_h);
        let mut placed_bottom = 0.0f32;
        // 嵌套的绝对定位盒会在这趟里被收进来，所以循环到收干净；上限防病态文档。
        let mut rounds = 0;
        while !self.absolute.is_empty() && rounds < 64 {
            rounds += 1;
            let nodes = std::mem::take(&mut self.absolute);
            for (node, static_y) in nodes {
                let style = self.style_of(node);
                let ml = style.margin_left.resolve(cb_w);
                let mr = style.margin_right.resolve(cb_w);
                let mt = style.margin_top.resolve(cb_w);
                let mb = style.margin_bottom.resolve(cb_w);
                let left = style.left.map(|l| l.resolve(cb_w));
                let right = style.right.map(|l| l.resolve(cb_w));
                let top = style.top.map(|l| l.resolve(cb_h));
                let bottom = style.bottom.map(|l| l.resolve(cb_h));
                let width = style
                    .width
                    .map(|l| l.resolve(cb_w))
                    .or_else(|| match (left, right) {
                        (Some(l), Some(r)) => Some((cb_w - l - r).max(1.0)),
                        _ => None,
                    })
                    .unwrap_or(cb_w);
                let x = match (left, right) {
                    (Some(l), _) => l + ml,
                    (None, Some(r)) => cb_w - r - mr - width,
                    _ => ml,
                };
                // 只写了 `bottom` 时还不知道盒有多高，先按 0 排、量出来再挪。
                let y0 = top.map_or(0.0, |t| t + mt);
                let blocks_start = self.blocks.len();
                let lines_start = self.lines.len();
                let height = self.block(node, x, y0, width, cb_w, None);
                let y = match (top, bottom) {
                    (Some(t), _) => t + mt,
                    (None, Some(b)) => {
                        let target = (cb_h - b - mb - height).max(0.0);
                        // 有页盒时，**底锚定**的盒不许上移进正文：页脚（出版社 / `.dibu`）
                        // 在正文比一页还高的时候按页底摆会压住末行（《未来简史》真机），
                        // 夹到它不脱流时的起点 + 上外边距，最多落在正文之后。
                        // 没有页盒（滚动档旧口径 / 竖排）保持原样。
                        if page.is_some() {
                            target.max(static_y + mt)
                        } else {
                            target
                        }
                    }
                    _ => mt,
                };
                let dy = y - y0;
                if dy != 0.0 {
                    for line in &mut self.lines[lines_start..] {
                        line.y += dy;
                    }
                    // 盒子自己也挪（这里与表格格子不同：格子的盒子拉满行高，绝对定位的盒子
                    // 高度就是内容高）。
                    for block in &mut self.blocks[blocks_start..] {
                        block.y += dy;
                    }
                }
                for block in &self.blocks[blocks_start..] {
                    placed_bottom = placed_bottom.max(block.y + block.height);
                }
                for line in &self.lines[lines_start..] {
                    placed_bottom = placed_bottom.max(line.y + line.height);
                }
            }
        }
        placed_bottom
    }

    /// 一格的两条固有宽（都含这一格左右边框与内边距）：`max` 是一整行不折的宽，
    /// `min` 是最长不可断段的宽。断点用 `unicode_linebreak`，与折行那一趟同一把尺 ——
    /// 自己另写一套「哪里能断」的判据，列宽就会与真正的折行位置对不上。
    ///
    /// 量的是这一格**整棵子树**的文本，按一整段算：格子里有多个段落时，真实的最大宽
    /// 是「最宽那一段」而不是「全加起来」。这个偏大会被列宽分配的「按比例摊」吃掉一部分，
    /// 而表格里一格多段本就少见（本机四本语料 0 处），所以先按一整段算。
    fn cell_intrinsic(&self, cell: NodeId, table_width: f32) -> table::Column {
        let style = self.style_of(cell);
        let [_, br, _, bl] = style.border_drawn();
        // 百分比内边距在这里按 0 折：量固有宽时还没有一列宽可当基准（见 `layout::table`）。
        let pad_x = bl
            + br
            + style.padding_left.resolve(0.0).max(0.0)
            + style.padding_right.resolve(0.0).max(0.0);
        let collected = collect(
            &[cell],
            self.dom,
            self.styles,
            style.white_space_pre,
            self.images,
            self.measure,
            Sizes {
                width: table_width,
                screen_width: self.screen.map(|(w, _)| w),
                page_height: self.page_height,
                screen_height: self.screen.map(|(_, h)| h),
            },
        );
        let widths = &collected.widths;
        let max: f32 = widths.iter().sum();
        let mut min = 0.0f32;
        if style.word_break == WordBreak::BreakAll {
            // 任意两字之间都能断 ⇒ 最小宽就是**最宽的那个字**。
            min = widths.iter().copied().fold(0.0, f32::max);
        } else {
            let mut start = 0usize;
            for (at, _) in linebreaks(&collected.text) {
                if at > start && at <= widths.len() {
                    min = min.max(widths[start..at].iter().sum::<f32>());
                }
                start = at;
            }
            if start < widths.len() {
                min = min.max(widths[start..].iter().sum::<f32>());
            }
        }
        let fixed = style.width.and_then(|length| match length {
            Length::Px(v) => Some(v),
            Length::Percent(p) => Some(p * table_width),
            Length::Auto => None,
        });
        table::Column {
            min: min + pad_x,
            max: max.max(min) + pad_x,
            fixed,
        }
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
    ///
    /// `cb_h` 是竖轴上同一件事的那一份：包含块的高，仅在父块**明确写过** `height` 时
    /// 是 `Some`（判据见 [`Self::child_height_basis`]）。它同样进缓存键 —— 同一块在
    /// 「父块写了高」与「父块由内容撑高」两种容器里折出的高不是一个数。
    pub(super) fn block(
        &mut self,
        node: NodeId,
        x: f32,
        y: f32,
        width: f32,
        basis: f32,
        cb_h: Option<f32>,
    ) -> f32 {
        let Some(key) = self.block_key(node, width, basis, cb_h, x, y) else {
            return self.block_uncached(node, x, y, width, basis, cb_h);
        };
        if let Some(height) = self.reuse_block(key, x, y) {
            return height;
        }
        let lines_start = self.lines.len();
        let blocks_start = self.blocks.len();
        let floats_start = self.floats.len();
        let height = self.block_uncached(node, x, y, width, basis, cb_h);
        self.stash_block(key, lines_start, blocks_start, floats_start, x, y, height);
        height
    }

    /// 这一块的缓存键；没开缓存时给 `None`（直接走实排）。
    fn block_key(
        &self,
        node: NodeId,
        width: f32,
        basis: f32,
        cb_h: Option<f32>,
        x: f32,
        y: f32,
    ) -> Option<BlockKey> {
        self.cache.as_ref()?;
        let style_hash = self.style_hash?.get(node as usize).copied().unwrap_or(0);
        Some(BlockKey {
            node,
            style_hash,
            width: width.to_bits(),
            basis: basis.to_bits(),
            // `None` 与 `Some(0.0)` 不是同一个数（`.container{height:0}` 真会写出 0），
            // 所以这一格存的是 `Option` 的位，不是直接 `to_bits`。
            cb_h: cb_h.map(|h| h.to_bits()),
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

    /// 摆放一个块级元素（不做块级缓存），返回边框盒高度。`basis` 与 `cb_h` 见
    /// [`Self::block`]：两个都是**这一块的包含块**的尺寸，一个折横向那族百分比，
    /// 一个折 `height` 那一族。
    fn block_uncached(
        &mut self,
        node: NodeId,
        x: f32,
        y: f32,
        width: f32,
        basis: f32,
        cb_h: Option<f32>,
    ) -> f32 {
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
        // 认的那一族见 [`picture_src`]（`<img>` 与 Calibre 封面那种 `<svg><image>`）。
        if let Some(src) = picture_src(self.dom, node) {
            let content_width = (width - pl - pr).max(1.0);
            let Some((w, h, wide)) = self.picture_box(node, &src, &style, content_width, pt + pb)
            else {
                return 0.0; // 图取不到就整块不留痕，不塌成空盒子
            };
            let img = self.images.resolve(&src);
            let fit = style.object_fit;
            let index = self.push_block(node, x, y, width, String::new(), true);
            let Some(img) = img else {
                // 格子留着、这一格不画：图还没解出来时，这一页也得滑得进来。
                return self.close_block(index, pt + h + pb);
            };
            // 块级图片的对齐：`start`/`end` 先按这一块的 `direction` 折成物理左右，
            // 再算让出多少（`TextAlign::offset_in` 是全仓唯一一处这份算术）。
            // 出血的那一张比这一块宽：左右各摊一半溢出，画出去的那一截盖在页边距上，
            // 而整张**居中在这块的内容盒**（左右页边距对称时就是屏幕居中）。作者另写了
            // `text-align: left/right` 时不抢它的话 —— 出血说的是「铺到边上」，
            // 贴边摆会让一半图掉到屏外。
            let overflow = ((w - content_width) / 2.0).max(0.0);
            let offset = if wide && overflow > 0.0 {
                -overflow
            } else {
                style
                    .text_align
                    .offset_in(style.direction, content_width, w)
            };
            self.lines.push(LineBox {
                owner: node,
                fragments: vec![LineFragment::Image(ImageRun {
                    node,
                    href: img.href,
                    width: w,
                    height: h,
                    advance: w,
                    x_offset: 0.0,
                    baseline_shift: 0.0,
                    fit,
                })],
                x: x + pl + offset,
                y: y + pt,
                width: w,
                height: h,
                baseline: h,
                align: style.text_align,
                // 块级图片独立成行，两端对齐只管字间的余量，对整块不适用。
                justify: None,
                // 叠图那一格由 `children` 起叠之后统一回填（这里先给 `None`：
                // 缓存命中的块直接拿回整段行盒，漏了回填也不会读到脏值）。
                stack: None,
            });
            return self.close_block(index, pt + h + pb);
        }

        let content_width = (width - pl - pr).max(1.0);
        // 子节点折 `height: NN%` 用的基准。这一块**明确写过** `height` 才给得出，
        // 由内容撑出来的高不算（两条理由都在 [`Self::child_height_basis`] 上）。
        let child_h = Self::child_height_basis(style, cb_h, pt + pb);
        let index = self.push_block(node, x, y, width, String::new(), true);
        let line_px = (style.font_size * style.line_height).max(1.0);
        let inner = if self.has_block_children(node) {
            self.children(node, x + pl, y + pt, content_width, child_h)
        } else {
            // 纯行内内容：先问一次「这一块旁边有没有浮动盒」。没有就是 `None`，
            // 折行照旧吃定宽，一次查表都不付。
            let bands = self.float_bands(x + pl, y + pt, content_width, line_px);
            let pieces = self.children_of(node);
            self.wrap_paragraph(
                node,
                pieces,
                Slot {
                    x: x + pl,
                    y: y + pt,
                    width: content_width,
                    basis,
                },
                &Insets(bands.as_deref()),
                style,
            )
        };
        self.close_block(
            index,
            pt + Self::used_content_height(style, inner, pt + pb, cb_h) + pb,
        )
    }

    /// 一张块级图（[`picture_src`] 认的那一族）的**显示尺寸**：作者 CSS 的宽高、出血与
    /// 「铺满屏幕」那一档、页高硬夹，一路算到底。回 `(宽, 高, 按屏幕宽铺的那一档成立)`。
    ///
    /// 块级图那一支（[`Self::block_uncached`]）与叠图的判据（「同尺寸」「不满一屏」）
    /// 共用这一份：判据量的必须是**画出来那一格**，而不是原图像素 —— `ImageScale::Screen`
    /// 默认档会把一张 80% 宽的图放大到整屏，按像素比就会把「其实一样大」的两张判成两张。
    ///
    /// `content_width` 是这一块的内容宽（已扣边框与内边距），`extra` 是上下那一圈之和：
    /// 页高硬夹要吃掉它，块本身还得装下边框与内边距。
    fn picture_box(
        &self,
        node: NodeId,
        src: &str,
        style: &ComputedStyle,
        content_width: f32,
        extra: f32,
    ) -> Option<(f32, f32, bool)> {
        let img = self.images.resolve(src);
        let aspect = svg_aspect(self.dom, node);
        // 出血（`duokan-bleed`，写在本节点或包它的任意一层）把这一块的可用宽换成屏幕宽。
        //
        // 从前这里还叠着读者那一档「图片显示」（原始尺寸压掉出血、铺满屏幕放开基准）。
        // 2026-11-01 深夜读者明确不要那一档：块级大图一律按屏幕宽那一档摆，判据只剩
        // 「本来是够大」那一条（见下面 `fills_the_screen`）。
        let bleed = bleeds_wide(self.dom, self.styles, node);
        let wide = bleed;
        let basis_w = if wide {
            self.screen
                .map_or(content_width, |(screen, _)| screen.max(content_width))
        } else {
            content_width
        };
        let (mut w, mut h) = match (&img, aspect) {
            // 有 viewBox 的那一档：宽铺满版心（出血时铺满屏）、高按比例，与图解没解出来无关。
            (_, Some(ratio)) => (basis_w, basis_w * ratio),
            (Some(img), None) => image_size(
                style,
                img.width,
                img.height,
                basis_w,
                // 只有出血那一档才把上限换成屏幕宽；普通图的上限是**所在容器**
                // （画廊的 15% 边距就靠这一格生效）。
                if wide {
                    self.screen.map(|(w, _)| w)
                } else {
                    None
                },
            ),
            // 图取不到就整块不留痕，不塌成空盒子。
            (None, None) => return None,
        };
        // 大图铺满屏幕（与 `wrap::push_image` 那一段同一条口径）：`object-fit` 说
        // 「这张图要盖住一块区域」、而且它本来就不小，就按**屏幕宽**重算宽高，
        // 越过左右页边距铺到边上。块级图片是满版图的绝大多数写法（`p > img` 单独一段）。
        if let Some(screen) = self.screen.map(|(w, _)| w)
            && w < screen
            && h > 0.0
        {
            // 「铺满」那一档不看 `object-fit`（读者已经说了要大图），但仍要求**本来就不小**：
            // 一块只有 24px 的徽标独占一段时也不该被拉成满屏，那不是大图而是排版坏了。
            // 判据与行内那一支共用 [`fills_the_screen`]：页高缺省时退回屏高，
            // 两档才量得一样。
            // 「该铺」恒为真：读者那一档已经撤掉，块级大图一律按屏幕宽铺；
            // 剩下那一条「本来就不小」由 `fills_the_screen` 里的宽/高八成挡着。
            let big_enough = fills_the_screen(
                wide,
                w,
                h,
                content_width,
                self.page_height,
                self.screen.map(|(_, h)| h),
            );
            if big_enough {
                h *= screen / w;
                w = screen;
            }
        }
        // 页高是硬上限：分片器不会把一张图切成两页。
        let (w, h) = self.fit_image_height(w, h, extra);
        // 铺到屏宽的那一张按「出血」摆（块级那一支会左右各摊一半溢出，图才居中）。
        // 从前这一格只看作者的 `duokan-bleed`；读者档撤掉之后，凡是比版心宽的都算。
        Some((w, h, wide || w > content_width))
    }

    /// 把一张图的盒高夹进一页：超过页高按比例缩宽，绝不把页底切掉。
    ///
    /// 分片器把一行图**整个**放进一页（`fragment.rs` 的「一行比一页高是常事」），
    /// 超出的那一截没有下一页可接。`extra` 是图片所在块的上下边框/内边距：
    /// 图片行画在内容盒里，块本身还要装下这一圈。
    fn fit_image_height(&self, w: f32, h: f32, extra: f32) -> (f32, f32) {
        let Some(max_h) = self.page_height else {
            return (w, h);
        };
        let max_h = (max_h - extra).max(1.0);
        if h > max_h && h > 0.0 {
            (w * max_h / h, max_h)
        } else {
            (w, h)
        }
    }

    /// 轻量模式：算一个块的高，不断行。`basis` 见 [`Self::block`] —— 这一条必须与
    /// [`Self::block_uncached`] 用同一个基准折百分比，否则轻量档的「量高」与「实排」
    /// 会给出两个不同的高。
    pub(super) fn block_light(
        &mut self,
        node: NodeId,
        width: f32,
        basis: f32,
        cb_h: Option<f32>,
    ) -> f32 {
        let style = self.style_of(node);
        let [bt, br, bb, bl] = style.border_drawn();
        let pl = bl + style.padding_left.resolve(basis).max(0.0);
        let pr = br + style.padding_right.resolve(basis).max(0.0);
        let pt = bt + style.padding_top.resolve(basis).max(0.0);
        let pb = bb + style.padding_bottom.resolve(basis).max(0.0);

        // 替换元素（图片）：按固有尺寸算高。认的那一族与高的来源见 [`picture_src`]、
        // [`svg_aspect`] —— 这一格必须与 `block_uncached` 同口径，否则轻量档的
        // 高度与实际排版不一致。
        if let Some(src) = picture_src(self.dom, node) {
            let content_width = (width - pl - pr).max(1.0);
            let img = self.images.resolve(&src);
            let wide = bleeds_wide(self.dom, self.styles, node);
            let h = match (img, svg_aspect(self.dom, node)) {
                (_, Some(ratio)) => content_width * ratio,
                (Some(img), None) => {
                    image_size(
                        style,
                        img.width,
                        img.height,
                        content_width,
                        if wide {
                            self.screen.map(|(w, _)| w)
                        } else {
                            None
                        },
                    )
                    .1
                }
                (None, None) => return 0.0,
            };
            let (_, h) = self.fit_image_height(content_width, h, pt + pb);
            return pt + h + pb;
        }

        let content_width = (width - pl - pr).max(1.0);
        // 与 `block_uncached` 同一个基准：这一趟量高，那一趟实排，两边不许各拿一个数。
        let child_h = Self::child_height_basis(style, cb_h, pt + pb);
        let inner = if self.has_block_children(node) {
            // 有块级子节点：递归。
            self.children_light(node, 0.0, 0.0, content_width, child_h)
        } else {
            // 纯文本块：按**估算总宽**除以版心宽估行数。用总宽（连续量）而不是「每行字数」
            // 的离散近似 —— 轻量档据此估高，行数偏一行就差一行高。
            let est = self.text_est_em(node) * style.font_size;
            let line_px = (style.font_size * style.line_height).max(1.0);
            let lines = (est / content_width).ceil().max(1.0);
            lines * line_px
        };
        pt + Self::used_content_height(style, inner, pt + pb, cb_h) + pb
    }

    /// 估算一个节点下文本的总宽，单位是 em（用于轻量模式量高）。
    ///
    /// 逐**字节**给一个近似宽：ASCII 空格 0.25 em、其余 ASCII 0.5 em、3 字节字符（CJK 等）
    /// 1 em、2 字节字符 0.5 em、续字节不计。以 0.25 em 为单位**整数**累计，比逐字符解码 +
    /// 浮点求和便宜（一章 0.3 ms 量级）。不是真字体度量，但比「一字 1 em」准得多 ——
    /// 后者会把纯拉丁段估宽一倍、行数翻番，轻量档据此估高就会差一整行。
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
            Sizes {
                width,
                screen_width: self.screen.map(|(w, _)| w),
                page_height: self.page_height,
                screen_height: self.screen.map(|(_, h)| h),
            },
        );
        let index = self.push_block(owner, x, y, width, collected.text.clone(), false);
        let height = self.wrap_lines(owner, &collected, slot, insets, style);
        self.close_block(index, height)
    }

    /// 先序入表：盒子必须在断行之前拿到自己的下标，否则 `lines` 区间无法回填。
    ///
    /// `border_box` 见 [`BlockBox::border_box`]：`block_uncached` 走 true，
    /// `wrap_paragraph` 的内容盒副本走 false。
    ///
    /// `pub(super)`：`layout` 主模块的入口要用它给**根元素**（通常是 `<body>`）也登记
    /// 一条边框盒，否则根自己的 `background-color` / 边框会整屏丢掉（见 `layout_inner`）。
    pub(super) fn push_block(
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

    pub(super) fn close_block(&mut self, index: usize, height: f32) -> f32 {
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
            let mut breaks: Vec<usize> = if style.word_break == WordBreak::BreakAll {
                // `word-break: break-all`：**任意两个字符之间**都能断。不走 UAX #14 的
                // 词边界（那正是这一档要放开的东西），也不再摘连字符断点。
                slice
                    .char_indices()
                    .skip(1)
                    .map(|(ix, _)| hard.start + ix)
                    .collect()
            } else {
                linebreaks(slice)
                    .filter(|(ix, _)| *ix > 0 && *ix < slice.len())
                    .map(|(ix, _)| hard.start + ix)
                    // `hyphens: none` 的书不许在连字符处断，所以把那一批断点摘掉。
                    .filter(|at| !hyphen_break_banned(&collected.text, *at, style.hyphens))
                    .collect()
            };
            // ruby 容器不许拆开排到两行上：落在容器**内部**的断点全部摘掉，起止两边留着
            //（起点那一格是「整个容器挪到下一行」，终点那一格是容器之后的正常断行）。
            // `word-break: break-all` 那一档同样要摘：它放开的是正文的断行机会，
            // 不是把注音与它的基字分家 —— 那样上一行只剩基字、下一行只剩注音。
            breaks.retain(|at| !collected.breaks_ruby(*at));
            // `cap` 在整段里都不变，先把「有没有」问一次，别每字符问。
            let cap_norm = cap.as_ref().map(|dc| dc.norm.clone());
            // 断点把硬行切成若干「词」（两个断点之间的连续段）：拉丁文里≈单词，
            // CJK 里退化成单字。稀疏（平均每词 ≥ 4 字节 —— 拉丁那种词边界断行）走词级
            // 扫描，每个词只做一次溢出判断；密集（CJK 几乎每字一个断点）或带下沉盒时走
            // 逐字符。两条路径输出逐行一致，这个分类只影响性能。
            //
            // 含可悬挂标点的硬行一律走逐字符：悬挂要在一行末尾「越界一格」再接着判下一个
            // 字，词级扫描是按整词判的，拆不出标点前后这两步。纯拉丁硬行没有这类标点，
            // 所以稀疏这条快路不受影响。
            let sparse = !breaks.is_empty()
                && slice.len() >= 4 * (breaks.len() + 1)
                && !slice.chars().any(hangs_at_end);
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
                    // 下沉的首字符就是它自己那一格，不额外挪。
                    x_offset: 0.0,
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
        let reserved = if image_only {
            0.0
        } else {
            place.insets.reserved
        };
        let (left, right) = (place.insets.left, place.insets.right);
        // 可用宽把这四段全扣掉：漏一段就是把字写进了不该写字的地方。
        let avail = (place.width - start - reserved - left - right).max(1.0);
        // 行尾悬挂（LaTeX xeCJK 的 `kaiming` 那一档）：这一段是**恰好因为末尾收尾标点
        // 才溢出**的，就把标点的前进量从「该对齐到的宽」里摘出去 —— 内容照样贴到版心
        // 边缘，标点整格落在外面的留白里。内容本身就溢出时（长词硬切）不挂，否则只是
        // 把标点推得更远。
        //
        // `LineBox::width` 仍记含标点的片段宽之和：命中、选区与墨迹宽要的是字真正画到
        // 哪儿。`acc.width` 与 `content` 的差就是悬挂量。
        let hang = if image_only || style.direction.is_rtl() {
            0.0
        } else {
            let width = hanging_width(collected, end);
            if width > 0.0 && acc.width > avail && acc.width - width <= avail {
                width
            } else {
                0.0
            }
        };
        let content = acc.width - hang;
        // 纯图片行比可用宽还宽（「铺满」/出血档把图放成屏幕宽，而这一块在画廊那种
        // `margin: 0 15%` 的容器里）：左右各摊一半溢出，图**居中在这块的内容盒**上。
        // `offset_in` 把余量夹成 0，照它摆就是整张往右溢出、右半截被屏缘裁掉 ——
        // 哈利波特画廊那七张「偏右 + 被裁」就是这么来的。块级图片那条路早就是这么摆的
        // （见 `block_uncached` 的 `-overflow`），行内这一支补上同一口径。
        let overflow = if image_only {
            ((acc.width - avail) / 2.0).max(0.0)
        } else {
            0.0
        };
        let offset = if overflow > 0.0 {
            -overflow
        } else {
            style.text_align.offset_in(style.direction, avail, content)
        };
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
            // `content` 是不含悬挂量的那一份：标点整格在版心外，余量只摊到正文的隙上；
            // 悬挂段自己的前进量不进 `content`，所以不会把版心右缘一起推走。
            justify_plan(style, place.hard_end, avail, content, chars, spaces)
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
            stack: None,
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
