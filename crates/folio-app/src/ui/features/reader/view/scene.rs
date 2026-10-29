//! 排版结果 → 绘制单元：把一段排版 y 覆盖到的行，摊成一批「已定好基线」的
//! [`GlyphRun`] 与若干绝对摆位的图片。
//!
//! 从 [`crate::ui::features::reader::view::body`] 里抽出来的是**纯计算**：几何全部来自
//! 排版层，这里只加三个偏移 —— 左边这一栏的 `x`（版心在窗口里的位置）、向上的
//! `origin_y`（这一片的起点）、以及 `top`（页眉那条带子占掉的高度，正文从它下面开始
//! 画）。不带任何 gpui 元素，只有 `GlyphRun`（它本身是个数据 struct），所以这一层
//! 能在宿主上被 `cargo test` 跑、也能被别的摆法复用（连续滚动整章一条，分页/双页逐片）。

use std::ops::Range;

use folio_core::content::format::css::computed::{
    BackgroundAxis, BackgroundEdge, BackgroundPosition, BackgroundRepeat, BackgroundSize,
    BackgroundTile, BorderStyle, Color, Length,
};
use folio_core::infra::metrics::Metric;
use folio_core::layout::DecorSeed;
use gpui_kit::{Hsla, SharedString};

use crate::ui::features::reader::model::chapter_layout::{Document, RowFragment};
use crate::ui::features::reader::model::measure::FontCache;
use crate::ui::features::reader::view::paint::GlyphRun;
use crate::ui::foundation::images::ChapterImages;
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
    /// 四个角的圆角半径（px），顺序 `[左上, 右上, 右下, 左下]`，由排版层按盒尺寸折好。
    ///
    /// 只作用于**底色**那一块四边形；边框仍是四条轴对齐的直条，所以圆角盒配粗边框时
    /// 四角是直的（见 `body::decoration_child`）。
    pub radius: [f32; 4],
    /// `[上, 右, 下, 左]` 的 `(宽, 色, 画法)`，宽 0 不画。画法有四档：`solid` 一整条，
    /// `double` 拆成两条 1/3 细线，`dotted` / `dashed` 按段切开；其余（`groove` / `ridge` /
    /// `inset` / `outset`）在解析期就没接住，到不了这里（见 `css::value::parse_border_style`）。
    pub border: [(f32, Hsla, BorderStyle); 4],
    /// `background-image` 的那些层，按 CSS 书写顺序（**先画的在屏幕上更靠上**，
    /// 所以绘制侧要倒着摆）。空 = 这一块没有背景图。
    ///
    /// 位置是**相对这一块自己的边框盒左上角**，与 `x`/`top` 同一套平移已经在外层加过，
    /// 所以竖排那条路上的 `slide` 不用在这里再算一次。
    pub pictures: Vec<BlockPicture>,
}

/// 一块背景图在这一块里要画的几张像素：哪张图 + 摆在哪儿。
///
/// **像素不在这里**，与 [`VisibleImage`] 同一条理由：解没解好由图集答，摆位与字节无关。
#[derive(Clone, Debug, PartialEq)]
pub struct BlockPicture {
    /// 这一张图的活儿（按容器路径向图片索引现取）。
    pub image: crate::ui::foundation::images::DecodeRequest,
    /// 一格 `(左, 上, 宽, 高)`，相对这一块边框盒左上角。`repeat` 一族才会多于一个数。
    pub tiles: Vec<(f32, f32, f32, f32)>,
}

/// 一块背景图最多摆多少格。
///
/// 铺法那一族在数学上是无界的：一块 4×4 的纹理盖在 600×4000 的长条上是一万五千格，
/// 而连续滚动那一档一章就是一根长条。封顶之后**退化成只摆定位那一格**
/// （`no-repeat`），因为多出来的那一整片元素会把一帧的绘制预算吃光 ——
/// 画少几格是看得出的糙，卡成个位数帧是全书都读不下去。
/// 这一档在本仓库的真实语料上从没碰到过（哈利波特那 23 处最多 15×30 = 450 格），
/// 它是护栏，不是路径。
const MAX_BACKGROUND_TILES: usize = 1024;

/// 一块背景图在这个盒里要摆的那些格子（相对盒左上角）。
///
/// 三个入参全是这一帧已经拿得到的数：盒的边框盒尺寸来自排版，固有尺寸每帧现问图片索引
/// （`natural` 取不到就是 `(0, 0)`），CSS 侧的摆法来自层叠。
///
/// **拿不到固有尺寸就一条都不摆**：`cover` / `contain` 与「一条只写了百分比」的尺寸
/// 都要按比例反算另一轴，没有那个比例就只能猜一个 —— 猜错的表现是扉页那张底图被拉成
/// 一个怪形状，比那块空着更难查。`background-size: 50px` 这种两轴里至少有一条写死的，
/// 走的是另一条路（写死那轴不需要比例）。
fn background_tiles(
    box_w: f32,
    box_h: f32,
    natural: (f32, f32),
    size: &BackgroundSize,
    position: &BackgroundPosition,
    repeat: &BackgroundRepeat,
) -> Vec<(f32, f32, f32, f32)> {
    if box_w <= 0.0 || box_h <= 0.0 {
        return Vec::new();
    }
    let Some((tile_w, tile_h)) = background_tile_size(box_w, box_h, natural, size) else {
        return Vec::new();
    };
    if tile_w <= 0.0 || tile_h <= 0.0 {
        return Vec::new();
    }
    // 定位：先按「不铺」算出那一格落在哪儿，铺的时候以它为相位。
    let origin = (
        axis_offset(box_w, tile_w, &position.x),
        axis_offset(box_h, tile_h, &position.y),
    );
    let rows = axis_span(box_h, tile_h, repeat.y, origin.1);
    let cols = axis_span(box_w, tile_w, repeat.x, origin.0);
    if rows.len() * cols.len() > MAX_BACKGROUND_TILES {
        // 只摆定位那一格：丢的是「铺出去的那一整片」，保的是「这一块有底」——
        // 与整块画空相比，那是更接近作者意图的那一半。
        tracing::warn!(
            "背景图铺满要 {}×{} = {} 格，超过 {MAX_BACKGROUND_TILES} 的护栏（盒 \
             {box_w:.0}×{box_h:.0}、图 {tile_w:.0}×{tile_h:.0}），只摆定位那一格",
            cols.len(),
            rows.len(),
            cols.len() * rows.len(),
        );
        return vec![(origin.0, origin.1, tile_w, tile_h)];
    }
    let mut out = Vec::with_capacity(rows.len() * cols.len());
    for (top, height) in &rows {
        for (left, width) in &cols {
            out.push((*left, *top, *width, *height));
        }
    }
    out
}

/// 一轴上要摆的那些格子，给的已经是**这一轴上的绝对位置** `(起点, 长)`。
///
/// 只列与 `[0, side)` 真的相交的那些。从前按 ±`ceil(side/tile)+1` 对称铺开，
/// 一块 40×40 的纹理盖满 600 宽的版心会算出 33 列，两轴一乘 1089 格，
/// **在完全正当的写法上**撞上护栏然后退化成一张 —— 护栏是给 pathological 那种情况的，
/// 不该被正常书写撞响。按相交范围收，同一张图是 15 列。
///
/// `space` 与 `round` 会改步进或改图的大小，这正是这两档的关键：
/// - `space`：装得下几张就摆几张，剩下的余量**均摊成隙**（图不缩放、边缘不裁）；
///   装不下一张时 CSS 就是画一张、溢出裁掉，所以张数至少 1。
/// - `round`：按能装下的张数取整，然后**把图缩放到整除**，于是没有缝也没有裁切。
fn axis_span(side: f32, tile: f32, mode: BackgroundTile, origin: f32) -> Vec<(f32, f32)> {
    match mode {
        BackgroundTile::NoRepeat => vec![(origin, tile)],
        BackgroundTile::Repeat => {
            // 要摆的那些格：起点 < side 且 终点 > 0。图比这一轴还宽时 `first` 就是 0，
            // 也就是只画定位那一格（它自己已经溢出，溢出的一截由外壳裁掉）。
            let first = (-origin / tile).floor() as i32;
            let last = ((side - origin) / tile).ceil() as i32 - 1;
            (first..=last.max(first))
                .map(|k| (origin + k as f32 * tile, tile))
                .collect()
        }
        BackgroundTile::Space => {
            let count = ((side / tile).floor() as i32).max(1);
            let gap = if count > 1 {
                (side - count as f32 * tile) / (count - 1) as f32
            } else {
                0.0
            };
            (0..count)
                .map(|k| (origin + k as f32 * (tile + gap), tile))
                .collect()
        }
        BackgroundTile::Round => {
            let count = ((side / tile).round() as i32).max(1);
            let each = side / count as f32;
            (0..count)
                .map(|k| (origin + k as f32 * each, each))
                .collect()
        }
    }
}

/// 一张图在这个盒里画多大。`None` = 算不出来（见 [`background_tiles`] 里那条理由）。
fn background_tile_size(
    box_w: f32,
    box_h: f32,
    natural: (f32, f32),
    size: &BackgroundSize,
) -> Option<(f32, f32)> {
    let (nw, nh) = natural;
    let has_ratio = nw > 0.0 && nh > 0.0;
    match size {
        BackgroundSize::Auto => has_ratio.then_some((nw, nh)),
        BackgroundSize::Cover => has_ratio.then(|| {
            (
                nw * (box_w / nw).max(box_h / nh),
                nh * (box_w / nw).max(box_h / nh),
            )
        }),
        BackgroundSize::Contain => has_ratio.then(|| {
            (
                nw * (box_w / nw).min(box_h / nh),
                nh * (box_w / nw).min(box_h / nh),
            )
        }),
        BackgroundSize::Explicit { width, height } => {
            let w = width.and_then(|l| fold_length(l, box_w));
            let h = height.and_then(|l| fold_length(l, box_h));
            match (w, h) {
                (Some(w), Some(h)) => Some((w, h)),
                // 只写了有一条轴：另一轴按固有纵横比反算 —— 与 `layout::ports::image_size`
                // 给 `<img>` 折的那一条**同一个式子**，两处不许各有各的定义。
                (Some(w), None) => (has_ratio && w > 0.0).then(|| (w, w * (nh / nw))),
                (None, Some(h)) => (has_ratio && h > 0.0).then(|| (h * (nw / nh), h)),
                // 两轴都是 `auto`：那其实就是 `BackgroundSize::Auto` 那一档。
                (None, None) => has_ratio.then_some((nw, nh)),
            }
        }
    }
}

/// 把一条 `Length` 折成 px。`Percent` 按**这一块对应轴**的长折。
/// `em` / `rem` / `vw` / `vh` 在解析期就没接（见 `css::value::size_axis`），到不了这里。
fn fold_length(length: Length, basis: f32) -> Option<f32> {
    match length {
        Length::Px(v) => Some(v),
        Length::Percent(p) => Some(p * basis),
        Length::Auto => None,
    }
}

/// 一轴上的落点：从盒的起始边数过去多少 px。
///
/// 百分比那一档的基准是**余量（盒 − 图）**，不是盒长 —— CSS 就这么定的：
/// `background-position: 100%` 要的是「图的右边贴盒的右边」，按盒长折就会把整张图
/// 推出可视区（图比盒大时更是直接看不见）。
fn axis_offset(side: f32, tile: f32, axis: &BackgroundAxis) -> f32 {
    let free = side - tile;
    match axis {
        BackgroundAxis::Length(length) => fold_length(*length, free).unwrap_or(0.0),
        BackgroundAxis::Fraction(p) => free * p,
        BackgroundAxis::Edge(edge, offset) => {
            let push = offset.and_then(|l| fold_length(l, free)).unwrap_or(0.0);
            match edge {
                BackgroundEdge::Start => push,
                BackgroundEdge::Center => free / 2.0 + push,
                BackgroundEdge::End => free - push,
            }
        }
    }
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
        // 这一帧的可见带，说的是**推进轴**上的一段（横排 = 屏上的 y，竖排 = 从长条右缘
        // 往里数的距离）。分片与裁剔都活在那一根轴上，界面不许自己换成 y 再递进来。
        cull: Range<f32>,
        // 本片在推进轴上的原点（`Fragment::origin_y` 那一个数）。
        origin_y: f32,
        // 本片沿推进轴的长度（`Fragment::height`）。竖排要用它算这一页的左缘落在哪，
        // 横排用不到 —— 一列一列从右往左堆，物理 x 与推进轴是反着数的。
        flow_len: f32,
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
        let flow = layout.flow_mode();
        let vertical = layout.vertical();
        // 竖排：长条沿物理 x 铺，而推进轴从**右缘**往里数。本片覆盖推进轴
        // `[origin_y, origin_y + flow_len)`，它的左缘在物理上距长条右缘 `strip − origin − len`，
        // 于是「物理坐标 → 屏上」要加的平移就是这个 `slide`（横排恒 0：那一档的平移
        // 是沿 y 减 `origin_y`，写在下面每一式的老式子里）。
        let strip = layout.width;
        let slide = if vertical {
            origin_y + flow_len - strip
        } else {
            0.0
        };
        let mut runs = Vec::new();
        let mut images = Vec::new();
        // 块级装饰与行一样按可见区间裁：只在 cull 覆盖到的盒子上花钱。
        let __t = crate::ui::features::reader::model::perf::Timer::start();
        // 「哪些块带装饰」已经在排版结果换代的同一批里折好了（`Layout::decor_seeds`），
        // 每帧在这里只做三件**必须**每帧做的事：按可见带裁、加两个平移、落主题前景。
        let decorations = place_decorations(
            document.decor_seeds(),
            if vertical {
                (strip - cull.end, strip - cull.start)
            } else {
                (cull.start, cull.end)
            },
            origin_y,
            vertical,
            slide,
            x,
            top,
            foreground,
            document.images(),
        );
        let __decor = __t.ns();
        crate::ui::features::reader::model::perf::add(|frame| frame.decor_ns += __decor);
        for ix in lines {
            let Some(line) = layout.lines.get(ix) else {
                continue;
            };
            // 裁剔问的是**推进轴**上的一段：横排是行顶与行底，竖排是这一列离长条右缘多远。
            let at = layout.flow_start(line);
            if at + layout.flow_thickness(line) < cull.start || at > cull.end {
                continue;
            }
            // 章内那一份「这一行摆什么」一章只物化一次（`Document::row_units`）：文本句柄、
            // 字体描述、行内 x、基线偏移、盒高、装饰布尔、CSS 色全在那里。每帧只往上加
            // 三个平移数，以及把两个每帧才有的东西现落一次（主题色、解析好的 `Font`）。
            let row = document.row_units(ix);
            // 两个**轴中性**的屏幕纯量：`inline` 是这一行沿字方向的起点，`cross` 是基线
            // 在侧轴上的位置。横排里就是 (x, y)，竖排里同一对数落在对调的两根轴上 ——
            // 谁把这对数折回物理坐标？只有 `paint` 里 `project` / `project_delta` 那两句，
            // 规则写在 `WritingMode` 上，界面不许各折一遍。
            let inline = if vertical {
                top + layout.inline_start(line)
            } else {
                x + layout.inline_start(line)
            };
            let cross = if vertical {
                x + layout.cross_baseline(line) + slide
            } else {
                top + layout.cross_baseline(line) - origin_y
            };
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
                            // 正数（往上抬）在横排就是基线 y 变小；竖排那一档「抬」是朝列的
                            // 起点边（`vertical-rl` 的起点边在右）—— 那个符号取自同一处投影，
                            // 不在这里另立一条。行盒高与行基线已由排版层算过。
                            .at(
                                inline + unit.x,
                                cross + {
                                    let (dx, dy) = flow.project_delta(0.0, -unit.baseline_shift);
                                    if vertical { dx } else { dy }
                                },
                            )
                            .on_flow(flow)
                            .color(run_color_link(unit.color, foreground, link, unit.link))
                            // 装饰线要这个 run 的 inline box 高才能定纵向落点
                            // （见 `paint::decoration_y`），它是样式给的数，shaping 里没有。
                            .in_box(unit.box_height)
                            .decorate(unit.underline, unit.line_through)
                            // 影子与装饰线同一条来路：`RunStyle` → `RowUnit` → 这里。
                            // 绘制层在影子非空时先铺一趟影子再画字（见 `paint::paint_run`）。
                            .shadowed(unit.text_shadows.clone())
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
                        crate::ui::features::reader::model::perf::add(|frame| {
                            frame.image_ns += __image
                        });
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
                            // 竖排里的图片**不转置也不旋转**：矩形按原尺寸画，只是
                            // 「底贴基线」换成「左缘贴基线」（rl 的起点边在右，图朝起点边那侧
                            // 长出去）。列的厚度是行高，图比它宽时会压到邻列 —— 显式降级，
                            // 接竖排的图片夹取要等块级转轴那一轮。
                            images.push(VisibleImage {
                                x: if vertical {
                                    cross + placed.baseline_shift
                                } else {
                                    inline + placed.x
                                },
                                top: if vertical {
                                    inline + placed.x
                                } else {
                                    cross - placed.height - placed.baseline_shift
                                },
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
/// 剩下来的这四件事确实必须每帧做，因为它们里面都有「此刻」：可见区间、滚动偏移、
/// 主题色（主题不许烘进表 —— `plans/阅读管线.md` §九 那四条硬约束的第 2 条），以及
/// **背景图的固有尺寸**（它随图片索引变，`set_images` 会 bump `images_rev`，烘进
/// `Layout::decor_seeds` 那张表就是第二个真相 —— 与 `RowImage` 只存 `href` 同一条理由）。
///
/// 相交判据与原式逐字一致（横排那条是 `y + height < start || y > end`，竖排换成物理 x），
/// 而且**只能整表扫**：块是嵌套的，父盒的 `y + height` 远大于紧随其后的子盒，所以
/// 「与一条带相交的那些块是连续下标区间」这个前提在块上不成立 —— `Layout::stacked`
/// 那一格是为行盒量的，搬过来会悄悄漏画底色。这张表短（只有真带装饰的块），
/// 短是它的代价低，不是它被二分了 —— 漏了这条前提的活证据就是下面那条高盒测试。
#[allow(clippy::too_many_arguments)] // 九个都是「这一帧」的输入，包成结构体只是换个地方摆
fn place_decorations(
    seeds: &[DecorSeed],
    // 可见带，已经折到**盒子自己那一条被裁的轴**上：横排是 y 的一段，竖排是物理 x 的一段
    // （`Scene::build` 里从推进轴翻回来一次，这里不再各折一遍）。
    band: (f32, f32),
    origin_y: f32,
    vertical: bool,
    slide: f32,
    x: f32,
    top: f32,
    foreground: Hsla,
    // 背景图的固有尺寸与字节都从这里现问（与 `<img>` 那条同一个来路）。
    images: &ChapterImages,
) -> Vec<BlockDecor> {
    let mut out = Vec::new();
    for seed in seeds {
        let (start, thick) = if vertical {
            (seed.x, seed.width)
        } else {
            (seed.y, seed.height)
        };
        if start + thick < band.0 || start > band.1 {
            continue;
        }
        // 边框色在表里已经把 `currentColor`（本块自己的 `color`）折完了，
        // 这里只剩「没色就落主题前景」那一步。
        // 表里的宽已经是 `BorderEdge::drawn()` 的结果（「有 width 但 style 是 none」
        // 那种不存在的边在折表时就折成 0 了），这里只落色，画法原样带过去。
        let edge = |(width, color, style): (f32, Option<Color>, BorderStyle)| {
            (width, run_color(color, foreground), style)
        };
        // 背景图：一张都取不到字节就整条跳过 —— 「这一块没有底」与「底还在解」在
        // 屏幕上与从前一样，都是空着，但别拿着一个假的 `DecodeRequest` 去登记图集，
        // 那会让回收那一账记在一张根本不存在的图上。
        let mut pictures = Vec::new();
        for picture in &seed.pictures {
            let Some(request) = images.decode_request(&picture.href) else {
                continue;
            };
            let tiles = background_tiles(
                seed.width,
                seed.height,
                images.natural_size(&picture.href),
                &picture.size,
                &picture.position,
                &picture.repeat,
            );
            if tiles.is_empty() {
                continue;
            }
            pictures.push(BlockPicture {
                image: request,
                tiles,
            });
        }
        out.push(BlockDecor {
            x: if vertical {
                x + seed.x + slide
            } else {
                x + seed.x
            },
            top: if vertical {
                top + seed.y
            } else {
                seed.y - origin_y + top
            },
            width: seed.width,
            height: seed.height,
            background: seed
                .background
                .map(|color| run_color(Some(color), foreground)),
            radius: seed.radius,
            border: [
                edge(seed.border[0]),
                edge(seed.border[1]),
                edge(seed.border[2]),
                edge(seed.border[3]),
            ],
            pictures,
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
    //! 这一层剩下的几件事必须每帧做，因为它们里面都有「此刻」：可见区间、滚动偏移、主题色，
    //! 以及背景图的固有尺寸与字节。

    use super::DecorSeed;
    use super::place_decorations;
    use crate::ui::foundation::images::ChapterImages;

    /// 一本「一张图都没有」的书：`place_decorations` 现在要拿它现问固有尺寸与字节，
    /// 而这几条测试只管底色、边框与裁剔，背景图那一格一律走「取不到就跳过」那一条。
    fn images() -> ChapterImages {
        ChapterImages::empty()
    }
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
            radius: [0.0; 4],
            border: [
                (0.0, None, BorderStyle::None),
                (0.0, None, BorderStyle::None),
                (2.0, None, BorderStyle::Double),
                (2.0, Some(Color(12, 34, 56)), BorderStyle::Solid),
            ],
            pictures: Vec::new(),
        }
    }

    /// 几何按 `x` 与「origin_y / top 两个偏移」落位；色按主题前景落；
    /// 已经折好的 `currentColor`（那条有色边框）不许被主题覆盖，画法原样带过去。
    #[test]
    fn a_seed_is_placed_with_the_translations_and_the_theme() {
        let decor = place_decorations(
            &[seed(40.0, 60.0)],
            (0.0, 700.0),
            100.0,
            false,
            0.0,
            20.0,
            60.0,
            black(),
            &images(),
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
            (0.0, 700.0),
            0.0,
            false,
            0.0,
            0.0,
            0.0,
            black(),
            &images(),
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
        let decor = place_decorations(
            &[parent],
            (4000.0, 4700.0),
            0.0,
            false,
            0.0,
            0.0,
            0.0,
            black(),
            &images(),
        );
        assert_eq!(decor.len(), 1, "父盒跨着整条带，必须画：{decor:?}");
        assert_eq!(decor[0].height, 5000.0);
    }

    /// `cover` / `contain` 的格子里落下的是**按比例算出的那一个矩形**，不是拉伸过的。
    /// 数抄的是哈利波特第一册扉页的真值：底图 792×1400，版心按 600×900 排。
    #[test]
    fn cover_and_contain_keep_the_aspect_and_only_the_right_axis_overflows() {
        use super::background_tiles;
        use folio_core::content::format::css::computed::{
            BackgroundAxis, BackgroundEdge, BackgroundPosition, BackgroundRepeat, BackgroundSize,
        };
        // `no-repeat center` 就是那七张扉页原文写的那一档。**注意 CSS 的初值是 `0% 0%`
        // （左上）而不是 center** —— 这里要的是居中那一档，所以逐字写出来，
        // 不让 `BackgroundPosition::default()` 替我决定测试的语义。
        let center = BackgroundPosition {
            x: BackgroundAxis::Edge(BackgroundEdge::Center, None),
            y: BackgroundAxis::Edge(BackgroundEdge::Center, None),
        };
        let none = BackgroundRepeat {
            x: folio_core::content::format::css::computed::BackgroundTile::NoRepeat,
            y: folio_core::content::format::css::computed::BackgroundTile::NoRepeat,
        };
        let (bw, bh) = (600.0, 900.0);
        let natural = (792.0, 1400.0);

        let cover = background_tiles(bw, bh, natural, &BackgroundSize::Cover, &center, &none);
        assert_eq!(cover.len(), 1, "不铺就该只有一格");
        let (left, top, w, h) = cover[0];
        assert!(w >= bw && h >= bh, "cover 要盖满两轴：{w}×{h}");
        assert!(
            (w / h - natural.0 / natural.1).abs() < 0.01,
            "纵横比不许改：{w}×{h} vs 源 {natural:?}"
        );
        // 横轴正好贴住（600/792 是那个更大的比值），所以 x 不溢出；溢出只发生在纵轴。
        assert!((w - bw).abs() < 0.01, "横轴该刚好贴住：{w}");
        assert!(left <= 0.0 && left + w >= bw, "居中的那一格要盖住整轴");
        assert!(top < 0.0 && top + h > bh, "纵轴两头各溢出一截：{top}+{h}");

        let contain = background_tiles(bw, bh, natural, &BackgroundSize::Contain, &center, &none);
        let (left, top, w, h) = contain[0];
        assert!(w <= bw && h <= bh, "contain 不许溢出：{w}×{h}");
        assert!(
            (w / h - natural.0 / natural.1).abs() < 0.01,
            "纵横比不许改：{w}×{h}"
        );
        // contain 折出来是「贴住一轴、另一轴留白」，那一轴就是 900（源比例更瘦长）。
        assert!((h - bh).abs() < 0.01, "纵轴该贴住：{h}");
        assert!(left > 0.0, "横轴两头留白：{left}");
        assert!(
            (left - ((bw - w) / 2.0)).abs() < 0.01,
            "居中的留白要两头均分"
        );
        assert!(top.abs() < 0.01, "贴住那一轴不该有位移：{top}");
    }

    /// **拿不到固有尺寸就一条都不摆**。这不是偷懒，是不猜：`cover` 与「只写了一条百分比」
    /// 的尺寸都要按源比例反算另一轴，没有那个比例就只能编一个数 ——
    /// 编错的形状比那块空着难查得多（空着至少像「这本书没图」）。
    #[test]
    fn a_picture_without_a_known_ratio_draws_nothing() {
        use super::background_tiles;
        use folio_core::content::format::css::computed::{
            BackgroundPosition, BackgroundRepeat, BackgroundSize, BackgroundTile,
        };
        let none = BackgroundRepeat {
            x: BackgroundTile::NoRepeat,
            y: BackgroundTile::NoRepeat,
        };
        let center = BackgroundPosition::default();
        for size in [
            BackgroundSize::Auto,
            BackgroundSize::Cover,
            BackgroundSize::Contain,
        ] {
            assert!(
                background_tiles(600.0, 900.0, (0.0, 0.0), &size, &center, &none).is_empty(),
                "{size:?} 都要读源比例，不知道就不摆"
            );
        }
        // 写死了其中一轴就不需要比例：那一轴按写的来，另一轴没比例时也不摆（CSS 的 auto
        // 在源比例未知时是「按固有尺寸」，而固有尺寸正是不知道的那个）。
        let only_width = BackgroundSize::Explicit {
            width: Some(folio_core::content::format::css::computed::Length::Px(
                200.0,
            )),
            height: None,
        };
        assert!(background_tiles(600.0, 900.0, (0.0, 0.0), &only_width, &center, &none).is_empty());
        let both = BackgroundSize::Explicit {
            width: Some(folio_core::content::format::css::computed::Length::Px(
                200.0,
            )),
            height: Some(folio_core::content::format::css::computed::Length::Px(
                100.0,
            )),
        };
        assert_eq!(
            background_tiles(600.0, 900.0, (0.0, 0.0), &both, &center, &none),
            vec![(0.0, 0.0, 200.0, 100.0)],
            "两轴都写死时不需要比例"
        );
    }

    /// `repeat` 那一族的格数。这里钉的是**别把护栏算响**：一块 40×40 的纹理（哈利波特
    /// `.oval` 用的就是 `pat.png` 40×40）铺满 600 宽的版心是 15×15 = 225 格，
    /// 而不是按 ±对称铺开出来的 33×33 = 1089 格 —— 后者会在完全正当的写法上
    /// 撞上 1024 的护栏然后退化成一张，症状是纹理底只剩左上那一块。
    #[test]
    fn repeat_tiles_only_what_intersects_the_box() {
        use super::background_tiles;
        use folio_core::content::format::css::computed::{
            BackgroundPosition, BackgroundRepeat, BackgroundSize, BackgroundTile,
        };
        let tiles = background_tiles(
            600.0,
            400.0,
            (40.0, 40.0),
            &BackgroundSize::Auto,
            &BackgroundPosition::default(),
            &BackgroundRepeat {
                x: BackgroundTile::Repeat,
                y: BackgroundTile::Repeat,
            },
        );
        assert_eq!(
            tiles.len(),
            15 * 10,
            "两轴各按相交的那几格数：{}",
            tiles.len()
        );
        // 每一格都要真的与这条轴有关。
        assert!(
            tiles.iter().all(|(l, t, _, _)| *l < 600.0 && *t < 400.0),
            "铺到轴外面去的格不该进表"
        );

        // 图比这一轴还宽：只画定位那一格，溢出的一截由外壳裁。
        let one = background_tiles(
            600.0,
            900.0,
            (792.0, 1400.0),
            &BackgroundSize::Cover,
            &BackgroundPosition::default(),
            &BackgroundRepeat {
                x: BackgroundTile::Repeat,
                y: BackgroundTile::Repeat,
            },
        );
        assert_eq!(one.len(), 1, "cover 折出来的格两轴都溢出，一格就够");
    }

    /// 百分比那一档的基准是**余量（盒 − 图）**，不是盒长。写错成盒长时，图比盒大会被
    /// 整个推出可视区：屏幕上那一块是空的，而 CSS 与图片索引两边一切正常。
    #[test]
    fn a_position_percentage_scales_the_leftover_not_the_box() {
        use super::background_tiles;
        use folio_core::content::format::css::computed::{
            BackgroundAxis, BackgroundPosition, BackgroundRepeat, BackgroundSize, BackgroundTile,
        };
        let at = |p: f32| BackgroundPosition {
            x: BackgroundAxis::Fraction(p),
            y: BackgroundAxis::Fraction(0.0),
        };
        let none = BackgroundRepeat {
            x: BackgroundTile::NoRepeat,
            y: BackgroundTile::NoRepeat,
        };
        let box_w = 600.0;
        let tile = 400.0;
        for (p, want) in [(0.0, 0.0), (0.5, 100.0), (1.0, 200.0)] {
            let got = background_tiles(
                box_w,
                900.0,
                (tile, 100.0),
                &BackgroundSize::Auto,
                &at(p),
                &none,
            );
            assert!(
                (got[0].0 - want).abs() < 0.01,
                "{p} 该落在 {want}，实得 {:?}",
                got[0].0
            );
        }
        // 关键字那三档与上面那条是同一句话的三种写法：`left` = 0%、`center` = 50%、
        // `right` = 100%。漂了一档就是扉页那张底图歪着贴。
        for (edge, want) in [
            (
                folio_core::content::format::css::computed::BackgroundEdge::Start,
                0.0,
            ),
            (
                folio_core::content::format::css::computed::BackgroundEdge::Center,
                100.0,
            ),
            (
                folio_core::content::format::css::computed::BackgroundEdge::End,
                200.0,
            ),
        ] {
            let got = background_tiles(
                box_w,
                900.0,
                (tile, 100.0),
                &BackgroundSize::Auto,
                &BackgroundPosition {
                    x: BackgroundAxis::Edge(edge, None),
                    y: BackgroundAxis::Fraction(0.0),
                },
                &none,
            );
            assert!((got[0].0 - want).abs() < 0.01, "{edge:?} 该落在 {want}");
        }
    }

    /// 空表（这一章没有一处带装饰）必须给空，而不是给出一条零尺寸的东西。
    #[test]
    fn an_empty_table_paints_nothing() {
        assert!(
            place_decorations(
                &[],
                (0.0, 700.0),
                0.0,
                false,
                0.0,
                0.0,
                0.0,
                black(),
                &images(),
            )
            .is_empty()
        );
    }
}
