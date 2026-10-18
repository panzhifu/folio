//! 排版结果 → 绘制单元：把一段排版 y 覆盖到的行，摊成一批「已定好基线」的
//! [`GlyphRun`] 与若干绝对摆位的图片。
//!
//! 从 [`crate::ui::features::reader::view::body`] 里抽出来的是**纯计算**：几何全部来自
//! 排版层，这里只加三个偏移 —— 左边这一栏的 `x`（版心在窗口里的位置）、向上的
//! `origin_y`（这一片的起点）、以及 `top`（页眉那条带子占掉的高度，正文从它下面开始
//! 画）。不带任何 gpui 元素，只有 `GlyphRun`（它本身是个数据 struct），所以这一层
//! 能在宿主上被 `cargo test` 跑、也能被别的摆法复用（连续滚动整章一条，分页/双页逐片）。

use std::ops::Range;

use folio_core::content::format::css::computed::{BorderStyle, Color};
use folio_core::infra::metrics::Metric;
use folio_core::layout::DecorSeed;
use gpui_kit::{Hsla, SharedString};

use crate::ui::features::reader::model::chapter_layout::{Document, RowFragment};
use crate::ui::features::reader::model::measure::FontCache;
use crate::ui::features::reader::view::paint::GlyphRun;
use crate::ui::foundation::style::{run_color, run_color_link};

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
    /// `[上, 右, 下, 左]` 的 `(宽, 色, 画法)`，宽 0 不画。画法今天就 `solid` 与
    /// `double` 两档：`double` 由绘制层拆成两条 1/3 细线，其余（`dotted` / `groove` …）
    /// 在解析期就没接住，到不了这里（见 `css::value::parse_border_style`）。
    pub border: [(f32, Hsla, BorderStyle); 4],
}

/// 可见范围内的一张图：位置与尺寸已由排版算好，这里只负责把「画在哪儿」记下来。
pub struct VisibleImage {
    /// 相对本元素原点的左上角与尺寸（逻辑像素）。
    pub x: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
    /// 这张图的活儿：怎么取字节、按什么格式解、进图集占多少。
    ///
    /// **像素不在这里** —— 场景只负责「这一帧该摆哪张图」，解没解好由
    /// [`crate::ui::features::reader::model::image_atlas::ImageAtlas`] 答（`view.rs` 在搭元素那一趟问它）。
    /// 摆位用的是排版给的框，与字节无关，所以这一格给得起。
    pub image: crate::ui::foundation::images::DecodeRequest,
    /// 这张图是不是一条**脚注引用**的标记；是的话带上注文。
    ///
    /// 老书把脚注标记做成一张 10px 的「注」图（`蛇结` 就是），图外面套一条
    /// `<a epub:type="noteref">`。文字命中碰不到这种标记（`<a>` 里没有一个可命中的
    /// 字符），所以注文在这里解好带出来，绘制层点它时直接抬弹层，不再去开图片查看器。
    pub footnote: Option<String>,
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
        // 主题的链接色。作者没写 `color` 而这一段在一条 `<a href>` 里时落到它，
        // 由调用方从当前主题取（引擎里不嵌任何色值）。
        link: Hsla,
    ) -> Self {
        // 等宽偏好与书的字体整份取一次：一个场景里所有 run 共用，别在逐 run 里克隆。
        let mono = SharedString::from(document.prefs().mono_font_family.clone());
        let faces = document.fonts();
        let layout = document.layout();
        let mut runs = Vec::new();
        let mut images = Vec::new();
        // 块级装饰与行一样按可见区间裁：只在 cull 覆盖到的盒子上花钱。
        let __t = crate::ui::features::reader::model::perf::Timer::start();
        // 「哪些块带装饰」已经在排版结果换代的同一批里折好了（`Layout::decor_seeds`），
        // 每帧在这里只做三件**必须**每帧做的事：按可见带裁、加两个平移、落主题前景。
        let decorations = place_decorations(
            document.decor_seeds(),
            cull.clone(),
            origin_y,
            x,
            top,
            foreground,
        );
        let __decor = __t.ns();
        crate::ui::features::reader::model::perf::add(|frame| frame.decor_ns += __decor);
        for ix in lines {
            let Some(line) = layout.lines.get(ix) else {
                continue;
            };
            if line.y + line.height < cull.start || line.y > cull.end {
                continue;
            }
            // 章内那一份「这一行摆什么」一章只物化一次（`Document::row_units`）：文本句柄、
            // 字体描述、行内 x、基线偏移、盒高、装饰布尔、CSS 色全在那里。每帧只往上加
            // 三个平移数，以及把两个每帧才有的东西现落一次（主题色、解析好的 `Font`）。
            let row = document.row_units(ix);
            let column_x = x + line.x;
            let baseline = line.y + line.baseline - origin_y + top;
            for fragment in row.iter() {
                match fragment {
                    RowFragment::Text(unit) => {
                        runs.push(
                            GlyphRun::new(
                                unit.text.clone(),
                                cache.resolve(&unit.font, family, &mono, faces),
                                unit.font_size,
                            )
                            // 章内身份：shaping 表按它查（`Document::shaped_line`）。
                            // 行号与槽位都只在**这一份排版**里作数，而排版一换
                            // `Document::drop_fragments` 就把两张表一起清掉。
                            .keyed((ix as u32, unit.slot))
                            // 每条 run 自己的基线：`vertical-align` 折出来的偏移在这里减掉，
                            // 正数（往上抬）就是基线 y 变小。行盒高与行基线已由排版层算过。
                            .at(column_x + unit.x, baseline - unit.baseline_shift)
                            .color(run_color_link(unit.color, foreground, link, unit.link))
                            // 装饰线要这个 run 的 inline box 高才能定纵向落点
                            // （见 `paint::decoration_y`），它是样式给的数，shaping 里没有。
                            .in_box(unit.box_height)
                            .decorate(unit.underline, unit.line_through)
                            // 字距与两端对齐：绘制在 run 内按字符序号 / 隙序号逐字右推，
                            // 与 `hit`/选区同一句规则。
                            .spacing(
                                unit.letter_spacing,
                                unit.justify,
                                unit.justify_shift,
                            ),
                        );
                    }
                    RowFragment::Image(placed) => {
                        // 图片交给 gpui 自己的 `img` 元素画（纹理上传由它负责），
                        // 底贴基线：盒顶 = 基线 − 图高。解不出资源的 `<img>` 留位不画。
                        //
                        // 这一格现在只**登记**这张图（一条 `DecodeRequest`），不去容器取字节：
                        // 那一趟连同解码整份搬到了后台（`crate::ui::features::reader::model::image_atlas`），绘制帧里
                        // 一次解压都不该有。
                        let __t = crate::ui::features::reader::model::perf::Timer::start();
                        let request = document.images().decode_request(&placed.href);
                        let __image = __t.ns();
                        crate::ui::features::reader::model::perf::add(|frame| frame.image_ns += __image);
                        // 每张图片每帧一条 `info!` 会把日志刷爆，而格式化本身就是要省的开销
                        // （`plans/阅读管线.md` §十一 第 11 条）：这一句降到 `trace!`。
                        tracing::trace!(
                            "Scene: 图片 {} {}×{}，索引里有 = {}",
                            placed.href,
                            placed.width,
                            placed.height,
                            request.is_some()
                        );
                        if let Some(image) = request {
                            images.push(VisibleImage {
                                x: column_x + placed.x,
                                top: baseline - placed.height - placed.baseline_shift,
                                width: placed.width,
                                height: placed.height,
                                footnote: document.footnote_note_for_node(placed.node),
                                image,
                            });
                        }
                    }
                }
            }
        }
        // 量具：一次构造记一笔。**逐 run 计数会把量具自己变成被测量的开销**
        // （`folio_core::infra::metrics` 模块头那条纪律），而这一趟要回答的问题是一次的：
        // 这一帧重建了几次场景。
        //
        // `Metric::SceneRuns` 不在这里数了——它的口径 2026-10-15 起是「这一帧新建了几条
        // 行单元」，那笔钱在 `Document::row_units` 里付，计数也就跟着挪到那一处。
        // 滚回走过的地方应当增量 0，往前滚那一趟该付（单元表按行懒物化）。
        Metric::SceneRebuilds.count();
        Self {
            runs,
            images,
            decorations,
        }
    }
}

/// 把装饰短表按可见带裁一刀，再落三个平移数与主题前景。
///
/// 表是 `Layout::decor_seeds` 在每次拿到新一代排版结果时折一次的（`Document::apply_typeset`），
/// 所以这里**不再走整章的块表** —— 那一趟从前是每帧、链上每个进链的章各一次
/// （`plans/阅读管线.md` §十一 第 12 条，一份语料一章 160 块、长章 600+）。
/// 剩下来的这三件事确实必须每帧做，因为它们里面都有「此刻」：可见区间、滚动偏移、
/// 主题色（主题不许烘进表 —— `plans/阅读管线.md` §九 那四条硬约束的第 2 条）。
///
/// 相交判据与原式逐字一致（`y + height < start || y > end`），而且**只能整表扫**：
/// 块是嵌套的，父盒的 `y + height` 远大于紧随其后的子盒，所以「与一条带相交的那些块
/// 是连续下标区间」这个前提在块上不成立 —— `Layout::stacked` 那一格是为行盒量的，
/// 搬过来会悄悄漏画底色。这张表短（只有真带装饰的块），短是它的代价低，不是它被二分了。
fn place_decorations(
    seeds: &[DecorSeed],
    cull: Range<f32>,
    origin_y: f32,
    x: f32,
    top: f32,
    foreground: Hsla,
) -> Vec<BlockDecor> {
    let mut out = Vec::new();
    for seed in seeds {
        if seed.y + seed.height < cull.start || seed.y > cull.end {
            continue;
        }
        // 边框色在表里已经把 `currentColor`（本块自己的 `color`）折完了，
        // 这里只剩「没色就落主题前景」那一步。
        // 表里的宽已经是 `BorderEdge::drawn()` 的结果（「有 width 但 style 是 none」
        // 那种不存在的边在折表时就折成 0 了），这里只落色，画法原样带过去。
        let edge = |(width, color, style): (f32, Option<Color>, BorderStyle)| {
            (width, run_color(color, foreground), style)
        };
        out.push(BlockDecor {
            x: x + seed.x,
            top: seed.y - origin_y + top,
            width: seed.width,
            height: seed.height,
            background: seed
                .background
                .map(|color| run_color(Some(color), foreground)),
            border: [
                edge(seed.border[0]),
                edge(seed.border[1]),
                edge(seed.border[2]),
                edge(seed.border[3]),
            ],
        });
    }
    out
}

#[cfg(test)]
mod tests {
    //! 装饰短表的**每帧那一半**：按可见带裁、加三个平移数、落主题前景。
    //!
    //! 「哪些块带装饰、色走 currentColor 折成什么」不在这里测了 —— 那是
    //! `Layout::decor_seeds` 的职责，2026-10-15 从这一层挪进引擎层，
    //! 对应用例在 `layout::tests::index`（拿接线前那趟每帧全表扫描当参照实现比的）。
    //! 这一层剩下的三件事必须每帧做，因为它们里面都有「此刻」：可见区间、滚动偏移、主题色。

    use super::DecorSeed;
    use super::place_decorations;
    use crate::ui::foundation::style::run_color;
    use folio_core::content::format::css::computed::{BorderStyle, Color};
    use gpui_kit::black;

    /// 一条 300×60 的底色种子：下边一条 `double`、左边一条 `solid`，`None` 的色走主题前景。
    fn seed(y: f32, height: f32) -> DecorSeed {
        DecorSeed {
            x: 12.0,
            y,
            width: 300.0,
            height,
            background: Some(Color(193, 204, 192)),
            border: [
                (0.0, None, BorderStyle::None),
                (0.0, None, BorderStyle::None),
                (2.0, None, BorderStyle::Double),
                (2.0, Some(Color(12, 34, 56)), BorderStyle::Solid),
            ],
        }
    }

    /// 几何按 `x` 与「origin_y / top 两个偏移」落位；色按主题前景落；
    /// 已经折好的 `currentColor`（那条有色边框）不许被主题覆盖，画法原样带过去。
    #[test]
    fn a_seed_is_placed_with_the_translations_and_the_theme() {
        let decor = place_decorations(&[seed(40.0, 60.0)], 0.0..700.0, 100.0, 20.0, 60.0, black());
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
        assert_eq!(
            decor.border[2],
            (2.0, black(), BorderStyle::Double),
            "没写色的边框落主题前景，画法要跟着走"
        );
        assert_eq!(
            decor.border[3].1,
            run_color(Some(Color(12, 34, 56)), black()),
            "表里已经折好 currentColor 的那一条，色必须是块自己的，不是主题的"
        );
        assert_eq!(decor.border[0].0, 0.0, "宽 0 的边不画");
    }

    /// 窗外的一条不产生绘制单元。
    #[test]
    fn a_seed_outside_the_visible_band_is_dropped() {
        let decor = place_decorations(
            &[seed(40.0, 60.0), seed(900.0, 60.0)],
            0.0..700.0,
            0.0,
            0.0,
            0.0,
            black(),
        );
        assert_eq!(decor.len(), 1, "900 那条在窗外：{decor:?}");
    }

    /// **这条就是「块表不能按 y 二分」的活证据**：一条从很上面就开始的高盒（父盒
    /// 的 `y + height` 远大于其后任何一条）必须照样画出来 —— 它与可见带相交，
    /// 但它的 `y` 在带下方很远。整表扫给的是这个正确答案；
    /// 谁将来想给这张表加二分，这条会拦住他。
    #[test]
    fn a_tall_box_starting_far_above_the_band_is_still_painted() {
        let parent = seed(0.0, 5000.0);
        let decor = place_decorations(&[parent], 4000.0..4700.0, 0.0, 0.0, 0.0, black());
        assert_eq!(decor.len(), 1, "父盒跨着整条带，必须画：{decor:?}");
        assert_eq!(decor[0].height, 5000.0);
    }

    /// 空表（这一章没有一处带装饰）必须给空，而不是给出一条零尺寸的东西。
    #[test]
    fn an_empty_table_paints_nothing() {
        assert!(place_decorations(&[], 0.0..700.0, 0.0, 0.0, 0.0, black()).is_empty());
    }
}
