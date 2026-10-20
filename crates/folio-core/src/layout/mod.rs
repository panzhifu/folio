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
mod wrap;

use self::cache::subtree_style_hash;
// `collect` 与子模块同名但分属值/类型两个命名空间：模块名不会把函数名带进本作用域，
// 而 `tests.rs` 用 `use super::*` 拿符号，所以这里显式把函数名也导入进来。
use self::collect::Ctx;
use crate::content::dom::{Dom, NodeId};
use crate::content::format::css::computed::{Display, StyledDom, WritingMode};

pub use cache::{BlockCache, LayoutCache, layout_cached};
pub use font::{FontKey, RunStyle, StyleArena, StyleRef};
pub use geometry::{
    BlockBox, DecorSeed, ImageRun, Justify, JustifyMode, Layout, LayoutOptions, LineBox,
    LineFragment, LineRun, MarkerRun,
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
    let index = ctx.push_block(root, 0.0, 0.0, options.width, String::new(), true);
    let content = ctx.children(root, pl, pt, content_width);
    let height = ctx.close_block(index, pt + content + pb);
    // 轴取**根样式**那一格：整章一根轴。混排（这一章里既有一竖排段又有一横排表格）
    // 明确不支持 —— 那样切片、进度、命中都得按块各问一次轴，而分片器只会切一根轴。
    // `allow_vertical` 是界面那一侧的门（竖排现在只在分页那一档落地），判据与理由
    // 写在 [`LayoutOptions::allow_vertical`]。
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

/// 两阶段排版的第一阶段产物：整章「只算高」得到的**顶层渲染单元表** + 整章高。
///
/// 对应 KOReader 的 render 阶段（`requestLightFormatting`）：只算几何、不建行盒，
/// 供分页 / 滚动 / 进度用。走 `light` 走法，一章 0.1 ms 量级。
pub struct ChapterMeasure {
    root: NodeId,
    /// 触发测量时的原始页宽（窗口宽）；退化整章重排要用。
    page_width: f32,
    /// 版心宽（内容宽）。
    pub width: f32,
    /// 整章高。
    pub height: f32,
    /// 顶层流内单元，文档序；`y` 是绝对值。
    units: Vec<RenderUnit>,
    /// 顶层是不是「纯块级」。有匿名行内内容时为 `false`（见 [`render_window`]）。
    simple: bool,
}

/// 一个顶层渲染单元：一个块级子节点及其在章内的位置与（估算）高。
struct RenderUnit {
    node: NodeId,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

impl ChapterMeasure {
    /// 落在 `y` 处的顶层单元的节点 —— 「跳到某处」把 y 解回 DOM 节点用。
    pub fn block_at(&self, y: f32) -> Option<NodeId> {
        self.units
            .iter()
            .find(|u| y >= u.y && y < u.y + u.height)
            .map(|u| u.node)
    }
}

/// 阶段一：整章只算高。等价于 [`layout`] 的 `light` 走法，但把**顶层单元**的位置记下来，
/// 供 [`render_window`] 只重排可见窗口。
///
/// 只处理「顶层纯块级」的常见章节；顶层混有匿名行内内容时标为退化，`render_window`
/// 会退回整章完整排版（正确但没了增量收益）。
pub fn measure_chapter(
    dom: &Dom,
    styles: &StyledDom,
    root: NodeId,
    options: LayoutOptions,
    measure: &dyn Measure,
    images: &dyn ImageProvider,
) -> ChapterMeasure {
    let mut ctx = Ctx {
        dom,
        styles,
        measure,
        images,
        light: true,
        blocks: Vec::new(),
        lines: Vec::new(),
        cache: None,
        style_hash: None,
        floats: Vec::new(),
    };
    let style = ctx.style_of(root);
    let pl = style.padding_left.resolve(options.width).max(0.0);
    let pt = style.padding_top.resolve(options.width).max(0.0);
    let pb = style.padding_bottom.resolve(options.width).max(0.0);
    let content_width =
        (options.width - pl - style.padding_right.resolve(options.width).max(0.0)).max(1.0);

    let mut units = Vec::new();
    let mut cursor = pt;
    let mut prev_bottom = 0.0f32;
    let mut simple = true;
    for &child in ctx.children_of(root) {
        let s = ctx.style_of(child);
        if s.display == Display::None {
            continue;
        }
        // 顶层出现匿名行内内容就切不动块了，标成退化。
        if ctx.is_inline_flow(child, s) {
            simple = false;
            break;
        }
        let mt = s.margin_top.resolve(content_width).max(0.0);
        // 与 `children_light` 同一条外边距折叠：兄弟间取较大值，不叠加。
        cursor += if prev_bottom > 0.0 {
            mt.max(prev_bottom)
        } else {
            mt
        };
        // 横向也走同一条求解式（`layout::collect::solve_block_width`）：这一张表要是
        // 少算一处，作者写了定宽的块就会「量出来满宽、排出来定宽」，窗口对不上。
        let (ml, cw) = ctx.solve_block_width(s, content_width);
        let height = ctx.block_light(child, cw, content_width);
        units.push(RenderUnit {
            node: child,
            x: pl + ml,
            y: cursor,
            width: cw,
            height,
        });
        cursor += height;
        prev_bottom = s.margin_bottom.resolve(content_width).max(0.0);
    }
    if !simple {
        units.clear();
    }
    ChapterMeasure {
        root,
        page_width: options.width,
        width: content_width,
        height: if simple { cursor + pb } else { 0.0 },
        units,
        simple,
    }
}

/// 阶段二：只对与 `[from_y, to_y)` 相交的顶层单元做**完整排版**
/// （对应 KOReader 的 draw 阶段：只对可见页涉及的块做对齐 / shaping / 装饰）。
///
/// 首个相交单元锚在测得的 `y` 上，其余按**实际**排出的高依次下推、并保持章内间距，
/// 所以窗口内部是自洽的（不会重叠）。返回的 [`Layout`] 只含这个窗口的行盒，
/// `height` 仍是整章高（来自 [`measure_chapter`]）。
///
/// 顶层有匿名行内内容（`simple == false`）时退化为整章完整排版。
pub fn render_window(
    chapter: &ChapterMeasure,
    dom: &Dom,
    styles: &StyledDom,
    measure: &dyn Measure,
    images: &dyn ImageProvider,
    from_y: f32,
    to_y: f32,
) -> Layout {
    if !chapter.simple {
        return layout(
            dom,
            styles,
            chapter.root,
            LayoutOptions {
                width: chapter.page_width,
                light: false,
                // 两阶段那条路整个按「单元沿 y 排」建着，竖排接不上（见 `render_window` 尾）。
                allow_vertical: false,
            },
            measure,
            images,
        );
    }
    let mut ctx = Ctx {
        dom,
        styles,
        measure,
        images,
        light: false,
        blocks: Vec::new(),
        lines: Vec::new(),
        cache: None,
        style_hash: None,
        floats: Vec::new(),
    };
    let mut bottom = 0.0f32;
    let mut prev: Option<&RenderUnit> = None;
    for unit in &chapter.units {
        if unit.y + unit.height < from_y {
            continue; // 整块在窗口上方
        }
        if unit.y > to_y {
            break; // 整块在窗口下方，后面更靠下
        }
        let top = match prev {
            None => unit.y, // 首个相交单元锚在测得的 y 上
            // 上一个实际底边 + 章内间距（map 里这个间距就是外边距折叠的结果）
            Some(p) => bottom + (unit.y - (p.y + p.height)),
        };
        // `basis` 用章的版心宽：这些单元是顶层块，它们的包含块就是根的内容盒，
        // 与 `measure_chapter` 里算 `unit.width` 时折边距用的同一个数。
        let h = ctx.block(unit.node, unit.x, top, unit.width, chapter.width);
        bottom = top + h;
        prev = Some(unit);
    }
    // 两阶段那条路是给「必须只排一部分」准备的（见 `plans/阅读管线.md` §十二），
    // 界面现在没接。它整条都按「单元沿 y 排、切片沿 y 切」建着（`ChapterMeasure::units`
    // 的 `y`、`block_at(y)`、`RenderUnit`），竖排要接上去得先把那套改成沿推进轴 ——
    // 所以这里钉死横排，别让一份「轴说竖排、单元表还按 y」的结果从这里出去。
    Layout::assemble(
        ctx.blocks,
        ctx.lines,
        chapter.width,
        chapter.height,
        WritingMode::HorizontalTb,
    )
}

#[cfg(test)]
mod tests;
