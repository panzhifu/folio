//! 排版结果 → 绘制单元：把一段排版 y 覆盖到的行，摊成一批「已定好基线」的
//! [`GlyphRun`] 与若干绝对摆位的图片。
//!
//! 从 [`crate::ui::features::reader::view`] 里抽出来的是**纯计算**：几何全部来自
//! 排版层，这里只加三个偏移 —— 左边这一栏的 `x`（版心在窗口里的位置）、向上的
//! `origin_y`（这一片的起点）、以及 `top`（页眉那条带子占掉的高度，正文从它下面开始
//! 画）。不带任何 gpui 元素，只有 `GlyphRun`（它本身是个数据 struct），所以这一层
//! 能在宿主上被 `cargo test` 跑、也能被别的摆法复用（连续滚动整章一条，分页/双页逐片）。

use std::ops::Range;
use std::sync::Arc;

use folio_core::css::computed::{BorderEdge, ComputedStyle};
use folio_core::dom::NodeId;
use folio_core::layout::{Layout, LineFragment};
use gpui_kit::{Hsla, SharedString};

use super::document::Document;
use super::measure::FontCache;
use super::paint::GlyphRun;
use crate::ui::widgets::run_color;

/// 可见范围内一块盒子的块级装饰：背景色与四边边框（实心画）。
///
/// 位置与尺寸是**边框盒**（含 padding 与边框自身，排版层 [`folio_core::layout::BlockBox`]
/// 直接给出），坐标系与 [`VisibleImage`] 相同：相对本元素原点。
#[derive(Debug, Clone, PartialEq)]
pub struct BlockDecor {
    pub x: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
    /// `background-color`。`None` = 书里没写这一条 —— 别拿不透明色去盖主题底色。
    pub background: Option<Hsla>,
    /// `[上, 右, 下, 左]` 的 `(宽, 色)`，宽 0 不画。`double` / `dotted` 等花式
    /// 一律按实心画：引擎对 `border-style` 的支持目前就到「有没有」。
    pub border: [(f32, Hsla); 4],
}

/// 可见范围内的一张图：位置与尺寸已由排版算好，这里只负责把「画在哪儿」记下来。
pub struct VisibleImage {
    /// 相对本元素原点的左上角与尺寸（逻辑像素）。
    pub x: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
    pub image: Arc<gpui_kit::Image>,
}

/// 一屏（或一栏）要画的全部单元。
#[derive(Default)]
pub struct Scene {
    pub runs: Vec<GlyphRun>,
    pub images: Vec<VisibleImage>,
    pub decorations: Vec<BlockDecor>,
}

impl Scene {
    /// 把 `[lines]` 这段排版行里、被 `cull`（排版 y 区间）覆盖到的行摆进场景。
    ///
    /// 行内每个 run 用自己的宽度累加定 x；基线一律
    /// `line.y + line.baseline − origin_y + top`：gpui 自己的 `paint_line` 先把 run 的
    /// line-height 盒顶摆到 `baseline − (盒高 + ascent − descent)/2`，再由它把基线落回盒内
    /// 同一个偏移，两头抵消，结果就是这一条线。我们在 `paint` 那一层自己画字形，所以直接用它。
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        document: &Document,
        cache: &mut FontCache,
        family: &SharedString,
        lines: Range<usize>,
        cull: Range<f32>,
        origin_y: f32,
        x: f32,
        top: f32,
        foreground: Hsla,
    ) -> Self {
        // 等宽偏好与书的字体整份取一次：一个场景里所有 run 共用，别在逐 run 里克隆。
        let mono = SharedString::from(document.prefs().mono_font_family.clone());
        let faces = document.fonts();
        let layout = document.layout();
        let mut runs = Vec::new();
        let mut images = Vec::new();
        // 块级装饰与行一样按可见区间裁：只在 cull 覆盖到的盒子上花钱。
        let decorations = collect_decorations(
            layout,
            cull.clone(),
            origin_y,
            x,
            top,
            foreground,
            &|node| document.style_of(node).cloned(),
        );
        for ix in lines {
            let Some(line) = layout.lines.get(ix) else {
                continue;
            };
            if line.y + line.height < cull.start || line.y > cull.end {
                continue;
            }
            // 行文本住在所属块的那个归一化串里（`LineRun::norm` 是它的字节区间）。
            let Some(block) = layout.block_of_line(ix) else {
                continue;
            };
            let mut x = x + line.x;
            let baseline = line.y + line.baseline - origin_y + top;
            for fragment in &line.fragments {
                match fragment {
                    LineFragment::Text(run) => {
                        let text = block.text.get(run.norm.clone()).unwrap_or("");
                        if text.is_empty() {
                            x += run.width;
                            continue;
                        }
                        runs.push(
                            GlyphRun::new(
                                SharedString::new(text),
                                cache.resolve(&run.style.font, family, &mono, faces),
                                run.style.font_size,
                            )
                            // 每条 run 自己的基线：`vertical-align` 折出来的偏移在这里减掉，
                            // 正数（往上抬）就是基线 y 变小。行盒高与行基线已由排版层算过，
                            // 这里只搬运那一个数。
                            .at(x, baseline - run.style.baseline_shift)
                            .color(run_color(run.style.color, foreground))
                            .decorate(run.style.underline, run.style.line_through),
                        );
                        x += run.width;
                    }
                    LineFragment::Image(placed) => {
                        // 图片交给 gpui 自己的 `img` 元素画（纹理上传由它负责），
                        // 底贴基线：盒顶 = 基线 − 图高。解不出资源的 `<img>` 留位不画。
                        let rendered = document.images().render_source(&placed.href);
                        // 每张图片每帧一条 `info!` 会把日志刷爆，而格式化本身就是要省的开销
                        // （`plans/阅读管线.md` §十一 第 11 条）：这一句降到 `trace!`。
                        tracing::trace!(
                            "Scene: 图片 {} {}×{}，可绘制 = {}",
                            placed.href,
                            placed.width,
                            placed.height,
                            rendered.is_some()
                        );
                        if let Some(image) = rendered {
                            images.push(VisibleImage {
                                x,
                                top: baseline - placed.height,
                                width: placed.width,
                                height: placed.height,
                                image,
                            });
                        }
                        x += placed.width;
                    }
                }
            }
        }
        Self {
            runs,
            images,
            decorations,
        }
    }
}

/// 从排版结果里挑出**可见范围**内带块级装饰（`background-color` / 边框）的盒子。
///
/// 几何直接用 [`folio_core::layout::BlockBox`] 的边框盒；颜色取层叠结果，边框色
/// 缺省跟块的文字色（CSS 的 `currentColor`），再落到主题前景。样式查询走闭包，
/// 是为了测试不用真排一章版。
fn collect_decorations(
    layout: &Layout,
    cull: Range<f32>,
    origin_y: f32,
    x: f32,
    top: f32,
    foreground: Hsla,
    style_of: &dyn Fn(NodeId) -> Option<ComputedStyle>,
) -> Vec<BlockDecor> {
    let mut out = Vec::new();
    for block in &layout.blocks {
        // 内容盒副本（`wrap_paragraph` 为正文映射补注册的那条）不画：几何内缩了
        // 边框与 padding，它的边框条会落在文字上。只画边框盒。
        if !block.border_box {
            continue;
        }
        if block.y + block.height < cull.start || block.y > cull.end {
            continue;
        }
        let Some(style) = style_of(block.node) else {
            continue;
        };
        if style.background_color.is_none() && style.border.iter().all(|edge| edge.drawn() <= 0.0) {
            continue;
        }
        if block.width <= 0.0 || block.height <= 0.0 {
            continue;
        }
        // 边框色没写就是 currentColor：先落块自己的文字色，再落主题前景。
        let edge = |edge: &BorderEdge| run_color(edge.color.or(style.color), foreground);
        out.push(BlockDecor {
            x: x + block.x,
            top: block.y - origin_y + top,
            width: block.width,
            height: block.height,
            background: style
                .background_color
                .map(|color| run_color(Some(color), foreground)),
            border: [
                (style.border[0].drawn(), edge(&style.border[0])),
                (style.border[1].drawn(), edge(&style.border[1])),
                (style.border[2].drawn(), edge(&style.border[2])),
                (style.border[3].drawn(), edge(&style.border[3])),
            ],
        });
    }
    out
}

#[cfg(test)]
mod tests {
    //! 块级装饰的挑拣：只挑带 `background-color` / 边框的盒子、按可见区间裁、
    //! 边框色缺省跟块自己的文字色（CSS 的 `currentColor`）。

    use super::collect_decorations;
    use crate::ui::widgets::run_color;
    use folio_core::css::computed::{BorderStyle, Color, ComputedStyle};
    use folio_core::layout::{BlockBox, Layout};
    use gpui_kit::black;

    /// 挑拣只关心块的几何与样式，行盒给空表就够。
    fn layout_of(blocks: Vec<BlockBox>) -> Layout {
        // 只能走 `assemble`：索引是私有的，拼结构体字面量会造出「有块盒但没有归属表」的
        // 排版结果，那种东西在生产里就是「底色一片空白」那种查不出根因的症状。
        Layout::assemble(blocks, Vec::new(), 336.0, 500.0)
    }

    fn block(node: u32, y: f32) -> BlockBox {
        BlockBox {
            node,
            x: 12.0,
            y,
            width: 300.0,
            height: 60.0,
            text: String::new(),
            lines: 0..0,
            border_box: true,
        }
    }

    fn styles_of(by_node: &[(u32, ComputedStyle)]) -> impl Fn(u32) -> Option<ComputedStyle> + '_ {
        move |node| {
            by_node
                .iter()
                .find(|(id, _)| *id == node)
                .map(|(_, style)| style.clone())
        }
    }

    /// 底色 + 边框的盒子被挑出来，几何按 `x` 与「origin_y / top 两个偏移」落位。
    #[test]
    fn a_styled_block_in_view_becomes_a_decoration() {
        let mut style = ComputedStyle::default();
        style.background_color = Some(Color(193, 204, 192));
        style.border[2] = folio_core::css::computed::BorderEdge {
            width: 2.0,
            style: BorderStyle::Solid,
            color: None,
        };
        let layout = layout_of(vec![block(7, 40.0)]);
        let decor = collect_decorations(
            &layout,
            0.0..700.0,
            100.0,
            20.0,
            60.0,
            black(),
            &styles_of(&[(7, style)]),
        );
        assert_eq!(decor.len(), 1);
        let decor = &decor[0];
        assert_eq!(
            (decor.x, decor.top),
            (32.0, 0.0),
            "x += 版心左缘；top = y − origin_y + top"
        );
        assert_eq!((decor.width, decor.height), (300.0, 60.0));
        assert_eq!(
            decor.background,
            Some(run_color(Some(Color(193, 204, 192)), black())),
            "背景色按字节转主题色"
        );
        assert_eq!(decor.border[2].0, 2.0, "下边框要画");
        assert_eq!(
            decor.border[2].1,
            black(),
            "边框色缺省 currentColor，块没写文字色就落主题前景"
        );
    }

    /// 没写装饰的盒子与可见范围外的盒子都不产生绘制单元；完全没样式的节点
    /// （层叠表里查不到）也一样。
    #[test]
    fn unstyled_blocks_and_blocks_out_of_view_are_skipped() {
        let layout = layout_of(vec![block(1, 40.0), block(2, 900.0), block(3, 100.0)]);
        let decor = collect_decorations(
            &layout,
            0.0..700.0,
            0.0,
            0.0,
            0.0,
            black(),
            // 节点 1 有样式但没写背景与边框；节点 2 有样式但在窗外；节点 3 无样式。
            &styles_of(&[(1, ComputedStyle::default()), (2, ComputedStyle::default())]),
        );
        assert!(decor.is_empty(), "实际 {decor:?}");
    }

    /// 内容盒副本（`border_box = false`）即使带着样式与装饰也不画 ——
    /// 真书目录页上，它的边框条会不偏不倚压在文字起点上。
    #[test]
    fn a_content_box_copy_is_never_decorated() {
        let mut style = ComputedStyle::default();
        style.background_color = Some(Color(193, 204, 192));
        let mut phantom = block(7, 40.0);
        phantom.border_box = false;
        let layout = layout_of(vec![block(7, 40.0), phantom]);
        let decor = collect_decorations(
            &layout,
            0.0..700.0,
            0.0,
            0.0,
            0.0,
            black(),
            &styles_of(&[(7, style)]),
        );
        assert_eq!(
            decor.len(),
            1,
            "同节点两条注册只该画出边框盒那条：{decor:?}"
        );
    }
}
