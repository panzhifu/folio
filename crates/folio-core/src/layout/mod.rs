//! 排版引擎 E1+E2：块级布局 + 多 run 行内布局。
//!
//! 输入 `Dom` + `StyledDom`，输出扁平的 `BlockBox` / `LineBox` 两张表。
//! 之所以是扁平表而不是树：M4 的 fragmentation 要按高度切行盒，树形反而碍事；
//! 需要父子关系时按 `BlockBox::lines` 的包含关系还原。
//!
//! 分工是刻意切开的：
//! 1. **收集** [`collect`]：把行内子树展平成一段文本 + piece 表。每个 piece 带着
//!    **它自己那个元素**的计算样式，所以 `<em>` 的斜体、`<small>` 的小字不会丢。
//! 2. **断行** [`Ctx::wrap_lines`]：由引擎自己做，字体只提供两种能力 ——
//!    单字前进量与整段绘制度量。断点取自 UAX #14（`unicode-linebreak`），
//!    这正是中文禁则（标点不该出现在行首/行尾）的依据。
//! 3. **摆放** [`Ctx::children`]：块级顺序、兄弟间外边距折叠、`text-indent`、`text-align`。
//!
//! 为什么断行要引擎自持而不是调 `TextSystem::line_wrapper`：那个 API 一次只能
//! 按一种字体折一行，多 run 段落用不了；而且禁则与行盒基线要我们自己说了算。
//! 断点按单字前进量累加判定（与 gpui 自己的 LineWrapper 同源），
//! 行宽则按实际 shaping 报告并用于对齐 —— 对齐用后者，绘制出来的行不会超出可用宽度。
//!
//! 坐标一律是布局空间内的**绝对值**，绘制阶段直接取用。
//! `blocks` 按**先序**入表：父块的 `lines` 区间包含子块，`block_of_line`
//! 取最后一个命中的盒子，就是真正拥有这些行的那个叶子块。
//!
//! 本层的已知取舍（同步记在 plans/facts.md）：
//! - 不做父子外边距折叠：嵌套容器里首个子块的 top margin 会计入父高度，
//!   比浏览器多一档空白，但不会重叠、也不会丢间距。
//! - 行内图片不产出 run（M3 的后半）；负外边距按 0；`width` / `max-width` / `float` 未接入。
//! - 嵌套元素自己的 `white-space` 不单独判断，整段按外层块的设置走。
//! - 分片（[`fragment`]）在断点处**丢掉上下 margin**：第 N 页末行下面那段空白不会出现在
//!   第 N+1 页开头。CSS 是两侧都留，这里选每页顶部稳定。

pub mod fragment;

mod cache;
mod collect;
mod font;
mod geometry;
mod key;
mod marker;
mod ports;
mod table;
mod wrap;

use self::cache::subtree_style_hash;
// `collect` 与子模块同名但分属值/类型两个命名空间：模块名不会把函数名带进本作用域，
// 而 `tests.rs` 用 `use super::*` 拿符号，所以这里显式把函数名也导入进来。
use self::collect::Ctx;
use crate::content::dom::{Dom, NodeId};
use crate::content::format::css::computed::{StyledDom, WritingMode};

pub use cache::BlockCache;
pub use font::{FontKey, RunStyle, StyleArena, StyleRef};
pub use geometry::{
    BlockBox, DecorPicture, DecorSeed, ImageRun, Justify, JustifyMode, Layout, LayoutOptions,
    LineBox, LineFragment, LineRun, MarkerRun,
};
pub use key::{LayoutInputs, LayoutKey, METRICS_REV, content_hash, quantize_px};
pub use ports::{ImageProvider, LineMetrics, Measure, NoImages, ResolvedImage};

/// 排版 `root` 的全部内容。`root` 通常是 `<body>`：它的 padding 生效，
/// 字号与字体取它自己的计算样式（读者偏好已经在 `cascade` 的根样式里注入）。
///
/// `images` 提供 `<img>` 的固有尺寸与解析后的路径；没有图片时传 [`NoImages`]。
///
/// span 开着到函数返回，所以 `logging` 把 `folio_core::layout` 开到 debug 时，日志里
/// 每个 span 一行 `close time.busy=…` —— 排版慢在哪一章，靠的就是这一句。
#[tracing::instrument(level = "debug", name = "layout", skip_all, fields(width = %options.width))]
pub fn layout(
    dom: &Dom,
    styles: &StyledDom,
    root: NodeId,
    options: LayoutOptions,
    measure: &dyn Measure,
    images: &dyn ImageProvider,
) -> Layout {
    layout_inner(dom, styles, root, options, measure, images, None)
}

/// 带**块级缓存**的排版入口。对应 KOReader 的 `_renderedBlockCache`。
///
/// 失效判断分两层，都只看传进来的这把 [`LayoutKey`]（构造点唯一，见 `layout::key`）：
/// - [`LayoutKey::context_rev`] 管**复用范围** —— 章文本、图片索引、字体表、度量口径任一
///   变了就整清缓存（`NodeId` 只在同一份 DOM 里才有意义，宽度只在同一张字体表里才成立）；
/// - 块键里的**子树样式哈希**管粒度 —— 样式没变的块照样命中，所以改一处样式只重排一块。
///
/// 调用方负责持有并复用同一个 `BlockCache`（跨多次 `layout_with_blocks`）；换章也可以
/// 显式调 [`BlockCache::clear`]。
#[allow(clippy::too_many_arguments)]
pub fn layout_with_blocks(
    cache: &mut BlockCache,
    key: &LayoutKey,
    dom: &Dom,
    styles: &StyledDom,
    root: NodeId,
    options: LayoutOptions,
    measure: &dyn Measure,
    images: &dyn ImageProvider,
) -> Layout {
    cache.sync_context(key.context_rev());
    layout_inner(dom, styles, root, options, measure, images, Some(cache))
}

/// 排版主流程。`cache` 为 `None` 时不走块级复用（[`layout`] 的路径）。
fn layout_inner(
    dom: &Dom,
    styles: &StyledDom,
    root: NodeId,
    options: LayoutOptions,
    measure: &dyn Measure,
    images: &dyn ImageProvider,
    cache: Option<&mut BlockCache>,
) -> Layout {
    // 子树样式哈希只在开了缓存时才算 —— 它是一趟额外的 DOM 遍历。
    let hashes = cache.as_ref().map(|_| subtree_style_hash(dom, styles));
    let mut ctx = Ctx {
        dom,
        styles,
        measure,
        images,
        light: options.light,
        blocks: Vec::new(),
        lines: Vec::new(),
        cache,
        style_hash: hashes.as_deref(),
        floats: Vec::new(),
        absolute: Vec::new(),
    };
    let style = ctx.style_of(root);
    // 根的包含块就是视口（`options.width`）：根上那族百分比按它折，再往里每一层
    // 按父块的内容宽折，这条基准一路由 `collect` 的 `basis` 参数带着走。
    //
    // **根自己不读 `width` / `max-width`**，与它下面的块级盒故意不同：从网页导出来的
    // 书满地是 `body { width: 600px; margin: 0 auto }` 那一类「桌面版定宽居中」，
    // 照做就等于把读者的版心缩成作者屏幕上那一栏，而版心归视口与读者设定 —— 不归作者。
    // 往下一层（`<div>`、`<p>`）就照 `collect::solve_block_width` 那条等式走。
    let pl = style.padding_left.resolve(options.width).max(0.0);
    let pt = style.padding_top.resolve(options.width).max(0.0);
    let pb = style.padding_bottom.resolve(options.width).max(0.0);
    let content_width =
        (options.width - pl - style.padding_right.resolve(options.width).max(0.0)).max(1.0);
    // **根自己的盒子也要入表**：`<body>` 上的 `background-color` / 边框要画在这一层。
    // 从前这里直接排 `children`，根自己从不登记 ⇒ 真书《人类简史》第一部分那页
    // `<body class="brownll">`（`background-color: #c4aa87`）的整幅棕底凭空消失。
    // 盒子取整个视口宽、从 (0, 0) 起，高度与 [`Layout::height`] 同一个数，两者自洽。
    // 根的包含块高 = `options.page_height`（视口 / 一页的高）。`html,body{height:100%}`
    // 那一族只有在这一格才折得出数，而它是封面页那套「浮动楔块 + 负下边距」垂直居中
    // 写法的唯一基准（判据见 `LayoutOptions::page_height`）。
    //
    // **只拿它当子节点的基准，不拿它当根盒的高**：根的高仍按内容算（下面那一行）。
    // 把 `body{height:100%}` 兑成「这一章正好一页高」会改掉每一章的分页与滚动链，
    // 而这一格要的只是让 `height:50%` 有个数可折 —— 两件事别顺手一起做。
    let child_h = Ctx::child_height_basis(style, options.page_height, pt + pb);
    let index = ctx.push_block(root, 0.0, 0.0, options.width, String::new(), true);
    let content = ctx.children(root, pl, pt, content_width, child_h);
    let mut height = ctx.close_block(index, pt + content + pb);
    // **溢出的内容也算这一章的高**。作者把整页图裹在 `height:0` 的容器里是常见写法
    // （《斗战狂潮》的封面页：`.container { height: 0em }` 里塞一张整页 `<img>`），
    // 根盒算出来是 0，而行盒实际排到了 551px 外面。浏览器的 scrollHeight 含溢出，
    // 而 `Layout::height` 是全仓唯一的「这一章有多长」：只认根盒的话，这一章在滚动链
    // 里就是 0 高（`Book::placements` 那道 `height() > 0.0` 的筛子）—— 症状正是读者
    // 说的「上下滚动滑不到封面」，分页那一侧则是切出 0 片。
    //
    // 根块自己的盒子跟着长高：背景与边框按它画，两处不一致就是「底色只铺一半」。
    let overflow = ctx
        .lines
        .iter()
        .map(|line| line.y + line.height)
        .chain(ctx.blocks.iter().map(|block| block.y + block.height))
        .fold(0.0, f32::max);
    if overflow > height + 0.5 {
        height = overflow;
        ctx.blocks[index].height = height;
    }
    // 轴取**根样式**那一格：整章一根轴。混排（这一章里既有一竖排段又有一横排表格）
    // 明确不支持 —— 那样切片、进度、命中都得按块各问一次轴，而分片器只会切一根轴。
    // `allow_vertical` 是界面那一侧的门（竖排现在只在分页那一档落地），判据与理由
    // 写在 [`LayoutOptions::allow_vertical`]。
    // 绝对定位的盒在**这一章的高定下来之后**才摆：`top` / `bottom` 的百分比基准是包含块高。
    // 它们不撑高（`height` 已经在上面收口），所以放在这一步不影响分页。
    if !options.light {
        ctx.place_absolutes(options.width, height);
    }
    let flow = if options.allow_vertical {
        style.writing_mode
    } else {
        WritingMode::HorizontalTb
    };
    let out = Layout::assemble(ctx.blocks, ctx.lines, content_width, height, flow);
    // 块数与行数是一章排版结果的体检指标：0 行意味着 DOM 或样式空了，而不是「这一章短」。
    tracing::debug!(
        "layout: {} 块 / {} 行，版心 {}，高 {}",
        out.blocks.len(),
        out.lines.len(),
        out.width,
        out.height
    );
    out
}

#[cfg(test)]
mod tests;
