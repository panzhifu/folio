//! 收集阶段的小结构与图片/首字母判据。
//!
//! 从 `collect.rs` 切出来只因为那份文件贴着行数基线；都是纯 DOM/样式查询，
//! 与摆放、断行主流程分开。

use std::ops::Range;

use folio_format::css::computed::StyledDom;
use folio_format::dom::{Dom, NodeId, NodeKind};

use crate::collect::Collected;

/// 摆一段行内内容的四件套：落点 `(x, y)`、行宽，以及百分比的折算基准。
///
/// 这四个数在 `children` → `block` → `wrap_paragraph` → `wrap_lines` 一路同进同出，
/// 又全是裸 `f32`：收成一条结构体不只是为了让签名短，是因为**把 `width` 与 `basis`
/// 传反了编译器不会拦**，症状是「百分比边距莫名其妙」，而无头测试里只有一屏错位。
#[derive(Clone, Copy)]
pub(crate) struct Slot {
    pub(crate) x: f32,
    pub(crate) y: f32,
    /// 行宽：已扣掉这一块自己的边距、边框与内边距。
    pub(crate) width: f32,
    /// 包含块宽：`text-indent` / `padding` 那一族百分比的折算基准。
    pub(crate) basis: f32,
}

/// 一个已经摆出去的浮动盒。坐标是章内的**绝对值**，而且记的是**外边距盒** ——
/// 绕着它排的那些行要让开的就是这一圈（`.img1` 那句 `margin-right: .8em` 正是靠它
/// 变成图与字之间的那条缝）。
#[derive(Clone, Copy)]
pub(crate) struct PlacedFloat {
    pub(crate) right: bool,
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
}

impl PlacedFloat {
    pub(crate) fn bottom(&self) -> f32 {
        self.y + self.height
    }

    /// 行带 `[top, top + band)` 与这个浮动盒纵向有没有相交。CSS 的口径正是「相交就让位」。
    pub(crate) fn overlaps(&self, top: f32, band: f32) -> bool {
        self.y < top + band && self.bottom() > top
    }
}

/// `::first-letter` 涵盖的那一段字节区间：从段首起，到第一个硬行里第一个**字母单元**
/// 结束（数字也算 —— 浏览器给 `1984` 的首字符就是那个 `1`）。紧贴首字母的标点按 CSS
/// 也在这个盒里（`「開」` 的引号），而它们本来就被这一段涵盖了，不必再往回找一遍。
/// 全段一个字母单元都没有（纯标点、纯图片）时给 `None`，也就是不造这个盒。
pub(crate) fn first_letter_norm(collected: &Collected) -> Option<Range<usize>> {
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
pub(crate) fn single_image(dom: &Dom, node: NodeId) -> Option<NodeId> {
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
pub(crate) fn text_len_in(dom: &Dom, node: NodeId) -> usize {
    let n = dom.get(node);
    if n.kind == NodeKind::Text {
        return n.text.chars().filter(|c| !c.is_whitespace()).count();
    }
    n.children.iter().map(|c| text_len_in(dom, *c)).sum()
}

/// 这一块的子树里有没有 `<img>`（图注那一格不许含图，含图就是「图文混排的一块」）。
pub(crate) fn has_image(dom: &Dom, node: NodeId) -> bool {
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
///   （EPUB2 的 `titlepage.xhtml` 十有八九是这个形状，基督山伯爵那本就是）。以前只认
///   `img`，这一章就折成 0 高：0 高的章在滚动链里不存在（`reader/book.rs` 那道
///   `height() > 0.0` 的筛子），症状是「从第二章回滑，滑不到第一节那张封面」。
///
/// 只认**一张** `image` 的 svg：叠多张、或带 `path`/`text` 的那种是矢量画，该走
/// `content::format::svg` 的整幅栅格化，不在排版里冒充一张位图。
pub(crate) fn picture_src(dom: &Dom, node: NodeId, styles: &StyledDom) -> Option<String> {
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
    let image = images.next();
    if image.is_some() {
        // Calibre 封面那种 `<svg><image href/></svg>`：仍是位图那一路。
        if images.next().is_some() {
            return None;
        }
        return image.and_then(|image| {
            dom.attr(image, "href")
                .or_else(|| dom.attr(image, "src"))
                .map(str::to_string)
        });
    }
    // 内联矢量花边：序列化成 data URI，由图片那条路栅格化后画出来。
    // 没有形状子节点时回 None —— 与从前「vector art 不冒充位图」一致。
    folio_format::svg::inline_svg_data_uri(dom, node, styles)
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
pub(crate) fn bleeds_wide(dom: &Dom, styles: &StyledDom, node: NodeId) -> bool {
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
pub(crate) fn fills_the_screen(
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
pub(crate) fn svg_aspect(dom: &Dom, node: NodeId) -> Option<f32> {
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
