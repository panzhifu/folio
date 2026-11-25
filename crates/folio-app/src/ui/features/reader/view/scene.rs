//! 排版结果 → 绘制单元：把一段排版 y 覆盖到的行，摊成一批「已定好基线」的
//! [`GlyphRun`] 与若干绝对摆位的图片。
//!
//! 这是**纯计算**：几何全部来自排版层，这里只加三个屏幕偏移（栏 left、起点 origin_y、
//! 页眉 top）。不带 gpui 元素，只有数据 [`GlyphRun`]，故能在宿主上跑 `cargo test`。

use std::ops::Range;

use folio_core::content::format::css::computed::BorderStyle;
use folio_core::infra::metrics::Metric;
use folio_core::layout::DecorPicture;
use gpui_kit::{Hsla, SharedString, hsla};

use crate::ui::features::reader::model::chapter_layout::{Document, RowFragment};
use crate::ui::features::reader::model::measure::FontCache;
use crate::ui::features::reader::model::session::StackMotionView;
use crate::ui::features::reader::view::paint::GlyphRun;

mod decor;
mod decor_pipeline;
mod tiles;
use crate::ui::foundation::images::ChapterImages;
use crate::ui::foundation::style::{desaturate, desaturate_author, run_color, run_color_link};
pub(in crate::ui::features::reader) use decor::full_page_picture;
use decor::overscan_backdrop;
pub use decor::{BlockDecor, BlockOutline, BlockPicture, BlockShadow};
use decor_pipeline::place_decorations;
use tiles::background_tiles;
pub(in crate::ui::features::reader) use tiles::picture_fills_width;

/// 这一行是不是当前那张的左右邻居。两头之外的不预解：一叠可能有八张，
/// 全排全清会把图集 24 MB 的封顶挤爆，反而把眼前这一张挤走。
fn stack_neighbor(index: u16, current: u16) -> bool {
    index == current.saturating_add(1) || index + 1 == current
}

/// 可见范围内的一张图：位置与尺寸已由排版算好，这里只负责把「画在哪儿」记下来。
pub struct VisibleImage {
    /// 相对本元素原点的左上角与尺寸（逻辑像素）。
    pub x: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
    /// 这一张属于哪一叠图（不在叠里 = `None`）。
    ///
    /// 三个用途：点开大图时从**这一张**开始、横滑切图认领这一笔、图上那格 `k/n` 角标。
    pub stack: Option<folio_core::layout::StackMark>,
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
    /// 这张图怎么在 `width × height` 这个盒里摆（CSS 的 `object-fit`）。
    ///
    /// 排版期只定盒；`cover` / `contain` 要的「按哪一块区域采样」在这一格。以前一条不认，
    /// 修图书里 `width:100%; object-fit:cover` 的图会被拉成扁的（读者 2026-10-30 报的
    /// 「图片显示效果」）。
    pub fit: crate::ui::foundation::images::ObjectFit,
    /// 叠图切图那一笔里，这一张要平移多少（逻辑像素）；0 = 不动。
    ///
    /// 只有动画在跑时才非 0：旧图往手指那一侧滑走、新图从另一侧进来，两图都画，
    /// 由 [`super::body::images::image_child`] 的裁切层把超出那一格的部分切掉。
    pub stack_slide: f32,
    /// 这一张是不是动画里「正在进来」的那一张。角标 `k/n` 只画在其中一张上，
    /// 免得动画那 180 ms 里屏上出现两个同样的角标。
    pub stack_incoming: bool,
}

/// 一屏（或一栏）要画的全部单元。
#[derive(Default)]
pub struct Scene {
    pub runs: Vec<GlyphRun>,
    pub images: Vec<VisibleImage>,
    pub decorations: Vec<BlockDecor>,
    /// 这一叠里**下一张**的图（不在屏上，只为让它提前解码）。
    ///
    /// 叠图横滑切一张时，新那一张的字节第一次才去解：大图（1000×1778 那种封面）
    /// 一下就是几十毫秒，读者读成「卡一下」。这一格在搭场景时顺手收「第 k+1 格」的
    /// 请求，绘制侧交给图集预热 —— 与翻页预取同一条思路，命中时 `prepare` 什么都不做。
    pub upcoming: Vec<crate::ui::foundation::images::DecodeRequest>,
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
        // 章节根的整页底图由阅读器当整窗背板画（见 `body::page_art`），这里跳过它，
        // 别在版心里再画一遍 —— 位置还差着一个页眉与安全区。
        taken_over: Option<usize>,
        // 水墨那一档：这一帧要画的**作者颜色**（文字色、底色、边框、影子）全部按亮度
        // 去饱和。主题那边已经在换肤时变灰了，两条路各管各的一半。
        ink: bool,
        // 哪一章（`StackMark.id` 只在一章的一份排版里作数）。
        chapter: usize,
        // 叠图切图那一笔的位移快照。`None` = 静态，只画当前那一张。
        stack_motion: Option<StackMotionView>,
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
        let mut upcoming = Vec::new();
        // 块级装饰与行一样按可见区间裁：只在 cull 覆盖到的盒子上花钱。
        let __t = crate::ui::features::reader::model::perf::Timer::start();
        // 装饰表随排版换代折好；每帧只裁可见带、加平移、落主题前景。
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
            taken_over,
            // 只在根块收整页底图时问一次，不逐块扫全文行片段。
            !document.has_text(),
            &layout.column_rules,
        );
        let __decor = __t.ns();
        crate::ui::features::reader::model::perf::add(|frame| frame.decor_ns += __decor);
        for ix in layout.paint_order(lines.start, lines.end) {
            let Some(line) = layout.lines.get(ix) else {
                continue;
            };
            // 裁剔问的是**推进轴**上的一段：横排是行顶与行底，竖排是这一列离长条右缘多远。
            let at = layout.flow_start(line);
            if at + layout.flow_thickness(line) < cull.start || at > cull.end {
                continue;
            }
            // 一叠图只画「现在这一张」那一格，其余各张整行跳过 —— 图与它的注都是
            // 那一叠第 k 格的行，所以切图时图注跟着一起换，不必另立一条规则。
            // 「看第几张」住界面（`Document::stack_current`）：它变了不许重排、不许改页数。
            let current = document.stack_current(line.stack.map(|mark| mark.id).unwrap_or(0));
            let motion = stack_motion.filter(|motion| {
                motion.chapter == chapter
                    && line.stack.is_some_and(|mark| mark.id == motion.id)
                    && motion.progress > 0.0
            });
            if let Some(mark) = line.stack {
                // 动画在跑时，除了当前那一张，还要把「滑走的那张」与「进来的那张」
                // 一并画出来；静态时仍然只画当前这一格。
                let drawn = match motion {
                    Some(m) => mark.index == current || mark.index == m.from || mark.index == m.to,
                    None => mark.index == current,
                };
                if !drawn {
                    // 叠里**左右两张邻居**先预热：这一行走不到绘制，但它们的图是读者
                    // 下一次横滑（往前往后都一样）要的那张，提前按全清解掉，切的那一下
                    // 直接是清晰的，不用先拿一张 360px 的糊图顶着再换。
                    if stack_neighbor(mark.index, current) {
                        for image in line.images() {
                            if let Some(request) = document.images().decode_request(&image.href) {
                                upcoming.push(request);
                            }
                        }
                    }
                    continue;
                }
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
                        // `visibility: hidden|collapse`：照常占位但绘制跳过；子级 visible 已在样式层折好。
                        if unit.hidden {
                            continue;
                        }
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
                            // 每条 run 自己的基线：`vertical-align` 的偏移在这里减掉，
                            // 符号取自同一处投影，不在这里另立一条。行盒高与基线已由排版层算过。
                            .at(
                                inline + unit.x,
                                cross + {
                                    let (dx, dy) =
                                        flow.project_delta(unit.cross_offset, -unit.baseline_shift);
                                    if vertical { dx } else { dy }
                                },
                            )
                            .on_flow(flow)
                            .color(run_color_link(unit.color, foreground, link, unit.link))
                            // 装饰线要这个 inline box 高定纵向落点（见 `paint::decoration_y`）。
                            .in_box(unit.box_height)
                            .decorate(unit.underline, unit.line_through)
                            .decoration(
                                unit.decoration_style,
                                unit.decoration_color
                                    .map(|color| run_color(Some(color), foreground)),
                            )
                            // 影子与装饰线同一条来路：`RunStyle` → `RowUnit` → 绘制层。
                            .shadowed(unit.text_shadows.clone())
                            // 着重号：颜色缺省跟字色，绘制层在每个非空白字形旁画点。
                            .emphasis(
                                unit.emphasis,
                                unit.emphasis_color.map(|c| run_color(Some(c), foreground)),
                                unit.emphasis_position,
                            )
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
                        if placed.hidden {
                            continue;
                        }
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
                        // ：这一句降到 `trace!`。
                        tracing::trace!(
                            "Scene: 图片 {} {}×{}，索引里有 = {}",
                            placed.href,
                            placed.width,
                            placed.height,
                            request.is_some()
                        );
                        if let Some(image) = request {
                            // 切图那一笔：旧图往手指那一侧滑走，新图从另一侧进来。
                            // `dir` 是「往下一张」还是「往上一张」，由 from/to 定，不用
                            // 再读一次手势方向 —— 两处各读一遍就会在 RTL 书上打架。
                            let (stack_slide, stack_incoming) = match (line.stack, motion) {
                                (Some(mark), Some(m)) if m.progress > 0.0 => {
                                    let dir = if m.to > m.from { 1.0 } else { -1.0 };
                                    if mark.index == m.from {
                                        (-m.progress * placed.width * dir, false)
                                    } else if mark.index == m.to {
                                        ((1.0 - m.progress) * placed.width * dir, true)
                                    } else {
                                        (0.0, false)
                                    }
                                }
                                _ => (0.0, false),
                            };
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
                                // 这一格两侧同为 app 侧的 `ObjectFit`（core 那一层在
                                // `chapter_layout` 摆位时已经折过来），不再 `into` 一次。
                                fit: placed.fit,
                                image,
                                stack: line.stack,
                                stack_slide,
                                stack_incoming,
                            });
                        }
                    }
                }
            }
        }
        // 量具：一次构造记一笔（**逐 run 计数会让量具变成被测量的开销**）。
        // `Metric::SceneRuns` 的口径 2026-10-15 起是「这一帧新建了几条行单元」，在
        // `Document::row_units` 里付；滚回应当增量 0，往前滚该付（单元表按行懒物化）。
        Metric::SceneRebuilds.count();
        let mut scene = Self {
            upcoming,
            runs,
            images,
            decorations,
        };
        if ink {
            scene.desaturate();
        }
        scene
    }

    /// 水墨：把这一帧的作者颜色全部压成同亮度的灰。
    ///
    /// 只管**书里写死的**那几格；主题色（正文色、链接色、选区）在换肤那一步已经去过
    /// 饱和，这里再过一遍是幂等的。图片不走这里 —— 它们在图集里按灰度解码。
    fn desaturate(&mut self) {
        for run in &mut self.runs {
            run.color = desaturate(run.color);
            if run.text_shadows.iter().any(|shadow| shadow.color.is_some()) {
                run.text_shadows = run
                    .text_shadows
                    .iter()
                    .map(|shadow| {
                        let mut shadow = *shadow;
                        shadow.color = shadow.color.map(desaturate_author);
                        shadow
                    })
                    .collect::<Vec<_>>()
                    .into();
            }
        }
        for decor in &mut self.decorations {
            decor.background = decor.background.map(desaturate);
            for edge in &mut decor.border {
                edge.1 = desaturate(edge.1);
            }
            for shadow in &mut decor.box_shadows {
                shadow.color = desaturate(shadow.color);
            }
            if let Some(outline) = &mut decor.outline {
                outline.color = desaturate(outline.color);
            }
        }
    }
}

/// 一整窗的背板（整页画章 / 单图页）：按**窗口尺寸**摆背景图，坐标就是阅读会话
/// 坐标；摆法仍走 CSS 的 `background-size` / `-position` / `-repeat`。
pub(in crate::ui::features::reader) fn backdrop_decor(
    pictures: &[DecorPicture],
    x: f32,
    top: f32,
    width: f32,
    height: f32,
    images: &ChapterImages,
) -> Option<BlockDecor> {
    // 先外扩再折 cover 的格子：像素按外扩后的框算，缝被图自己盖掉，不留底色。
    let (x, top, width, height) = overscan_backdrop(x, top, width, height);
    let mut out = Vec::new();
    for picture in pictures {
        let Some(request) = images.decode_request(&picture.href) else {
            continue;
        };
        let tiles = background_tiles(
            width,
            height,
            // 背板没有边框与内边距，定位盒与裁剪盒重合。
            (0.0, 0.0, width, height),
            images.natural_size(&picture.href),
            &picture.size,
            &picture.position,
            &picture.repeat,
        );
        if tiles.is_empty() {
            continue;
        }
        out.push(BlockPicture {
            image: request,
            tiles,
        });
    }
    (!out.is_empty()).then(|| BlockDecor {
        x,
        top,
        width,
        height,
        background_rect: (x, top, width, height),
        origin_rect: (x, top, width, height),
        background: None,
        radius: [0.0; 4],
        border: [(0.0, hsla(0.0, 0.0, 0.0, 0.0), BorderStyle::None); 4],
        pictures: out,
        box_shadows: Vec::new(),
        outline: None,
        backdrop_blur: None,
    })
}

#[cfg(test)]
mod tests {
    //! 装饰短表的**每帧那一半**：按可见带裁、加三个平移数、落主题前景。
    //!
    //! 「哪些块带装饰、色走 currentColor 折成什么」在 `Layout::decor_seeds`（引擎层）测，
    //! 对应用例在 `layout::tests::index`（拿接线前那趟每帧全表扫描当参照实现比的）。
    //! 这一层剩下的几件事必须每帧做，因为它们里面都有「此刻」：可见区间、滚动偏移、主题色，
    //! 以及背景图的固有尺寸与字节。

    use super::place_decorations;
    use super::{
        BlockDecor, BlockOutline, BlockShadow, Scene, decor, overscan_backdrop, stack_neighbor,
    };
    use crate::ui::features::reader::view::paint::GlyphRun;
    use crate::ui::foundation::images::ChapterImages;
    use folio_core::content::format::css::computed::TextShadow;
    use folio_core::layout::DecorSeed;
    use gpui_kit::{Font, Rgba, SharedString, hsla};

    use folio_core::content::format::css::computed::ShadowLayer;

    /// 一本「一张图都没有」的书：`place_decorations` 现在要拿它现问固有尺寸与字节，
    /// 而这几条测试只管底色、边框与裁剔，背景图那一格一律走「取不到就跳过」那一条。
    fn images() -> ChapterImages {
        ChapterImages::empty()
    }
    use crate::ui::foundation::style::run_color;
    use folio_core::content::format::css::computed::{BorderStyle, Color};
    use gpui_kit::black;

    /// 整页画背板要盖过窗口四边：真机上「图片上下各透一条很细的白光」就是浮点
    /// 取整差的那一丝缝，缝里露的是 `reader-page` 的纸底。外扩还要**不挪画心** ——
    /// 只放大不平移，作者写的 `bottom center` 才还落在那个位置。
    #[test]
    fn a_full_page_backdrop_overscans_all_four_edges() {
        let (x, top, w, h) = overscan_backdrop(0.0, 0.0, 384.0, 853.0);
        assert!(x < 0.0 && top < 0.0, "左 / 上要盖过窗口起点：{x} / {top}");
        assert!(
            x + w > 384.0 && top + h > 853.0,
            "右 / 下要盖过窗口终点：{} / {}",
            x + w,
            top + h
        );
        assert_eq!(x + w / 2.0, 192.0, "画心横向不动");
        assert_eq!(top + h / 2.0, 426.5, "画心纵向不动");
    }

    /// 整页底色只画一次：纯图页那一章的根块底色由 `reading_theme::resolve` 铺
    /// （`column.bg(reading.bg)`），这里不许再折一块装饰盖上去 —— 两条路取的色不
    /// 完全同源，第二条晚一帧出现，读者看见的正是「先读者纸色、再跳书色」。
    ///
    /// 反方向三条也钉住：有正文的章（块高不等于整页，页尾那一截要靠它画）、
    /// 正文中间自己写了底的块（真装饰）、以及根块以外的高盒，都照旧画。
    #[test]
    fn the_page_itself_is_painted_once() {
        use super::decor::page_color_is_drawn_by_reader;
        // 纯图页的根块：整页底色交给读者那一层。
        assert!(page_color_is_drawn_by_reader(0, &seed(0.0, 853.0), true));
        // 有正文的章：根块照样画（正文盒没铺到纸底）。
        assert!(!page_color_is_drawn_by_reader(0, &seed(0.0, 853.0), false));
        // 正文中间的一块：真装饰，照画。
        assert!(!page_color_is_drawn_by_reader(3, &seed(120.0, 60.0), true));
        // 根块但从页面中间起：不是整页。
        assert!(!page_color_is_drawn_by_reader(0, &seed(120.0, 60.0), true));
    }

    /// 一条 300×60 的底色种子：下边一条 `double`、左边一条 `solid`，`None` 的色走主题前景。
    fn seed(y: f32, height: f32) -> DecorSeed {
        use std::sync::Arc;

        DecorSeed {
            x: 12.0,
            y,
            width: 300.0,
            height,
            background_rect: (12.0, y, 300.0, height),
            origin_rect: (12.0, y, 300.0, height),
            background: Some(Color(193, 204, 192, 255)),
            full_bleed: false,
            radius: [0.0; 4],
            border: [
                (0.0, None, BorderStyle::None),
                (0.0, None, BorderStyle::None),
                (2.0, None, BorderStyle::Double),
                (2.0, Some(Color(12, 34, 56, 255)), BorderStyle::Solid),
            ],
            pictures: Vec::new(),
            backdrop_blur: None,
            box_shadows: Arc::from([ShadowLayer {
                dx: 1.0,
                dy: 2.0,
                blur: 3.0,
                inset: false,
                color: Some(Color(9, 8, 7, 255)),
            }]),
            outline: Some((1.0, None, BorderStyle::Solid, 2.0)),
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
            None,
            false,
            &[],
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
            Some(run_color(Some(Color(193, 204, 192, 255)), black())),
            "背景色按字节转主题色"
        );
        assert_eq!(
            decor.border[2],
            (2.0, black(), BorderStyle::Double),
            "没写色的边框落主题前景，画法要跟着走"
        );
        assert_eq!(
            decor.border[3].1,
            run_color(Some(Color(12, 34, 56, 255)), black()),
            "表里已经折好 currentColor 的那一条，色必须是块自己的，不是主题的"
        );
        assert_eq!(decor.border[0].0, 0.0, "宽 0 的边不画");
        assert_eq!(
            decor.box_shadows,
            vec![BlockShadow {
                dx: 1.0,
                dy: 2.0,
                blur: 3.0,
                inset: false,
                color: run_color(Some(Color(9, 8, 7, 255)), black()),
            }],
            "阴影的几何原样带过去，颜色走作者色"
        );
        assert_eq!(
            decor.outline,
            Some(BlockOutline {
                width: 1.0,
                color: black(),
                style: BorderStyle::Solid,
                offset: 2.0,
            }),
            "轮廓没写色时走主题前景，偏移保留"
        );
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
            None,
            false,
            &[],
        );
        assert_eq!(decor.len(), 1, "900 那条在窗外：{decor:?}");
    }

    /// 被整窗背板接管的那一条按**下标**跳，不按 `full_bleed` 跳。
    ///
    /// 从前这一格只认 `full_bleed`，而《哈利·波特全集》`v1.xhtml` 的根底图没写
    /// `background-attachment: fixed` —— 于是版心里那一份照旧画了一遍，压在整窗背板与
    /// 纸底色之上，读者看到的是裁成章高一条的复制品（2026-11-02 真机）。
    #[test]
    fn the_taken_over_seed_is_dropped_by_index_not_by_flag() {
        let decor = place_decorations(
            &[seed(40.0, 60.0), seed(120.0, 60.0)],
            (0.0, 700.0),
            0.0,
            false,
            0.0,
            0.0,
            0.0,
            black(),
            &images(),
            Some(0),
            false,
            &[],
        );
        assert_eq!(decor.len(), 1, "背板接管的那一条不再进版心：{decor:?}");
        assert_eq!(decor[0].top, 120.0, "跳的是第 0 条，其余照画：{decor:?}");
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
            None,
            false,
            &[],
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

        let cover = background_tiles(
            bw,
            bh,
            (0.0, 0.0, bw, bh),
            natural,
            &BackgroundSize::Cover,
            &center,
            &none,
        );
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

        let contain = background_tiles(
            bw,
            bh,
            (0.0, 0.0, bw, bh),
            natural,
            &BackgroundSize::Contain,
            &center,
            &none,
        );
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
                background_tiles(
                    600.0,
                    900.0,
                    (0.0, 0.0, 600.0, 900.0),
                    (0.0, 0.0),
                    &size,
                    &center,
                    &none
                )
                .is_empty(),
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
        assert!(
            background_tiles(
                600.0,
                900.0,
                (0.0, 0.0, 600.0, 900.0),
                (0.0, 0.0),
                &only_width,
                &center,
                &none
            )
            .is_empty()
        );
        let both = BackgroundSize::Explicit {
            width: Some(folio_core::content::format::css::computed::Length::Px(
                200.0,
            )),
            height: Some(folio_core::content::format::css::computed::Length::Px(
                100.0,
            )),
        };
        assert_eq!(
            background_tiles(
                600.0,
                900.0,
                (0.0, 0.0, 600.0, 900.0),
                (0.0, 0.0),
                &both,
                &center,
                &none
            ),
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
            (0.0, 0.0, 600.0, 400.0),
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
            (0.0, 0.0, 600.0, 900.0),
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

    /// `repeat` 铺到**裁剪盒**的边为止：定位盒缩进去之后，定位盒外面的边框带还要
    /// 接着铺（相位从定位盒量起，绘制区是整个裁剪盒）。这里钉的是覆盖范围随裁剪盒
    /// 扩、不随定位盒缩 —— 铺少了，边框带里露出一条底色。
    #[test]
    fn repeat_covers_the_clip_box_beyond_the_origin_box() {
        use super::background_tiles;
        use folio_core::content::format::css::computed::{
            BackgroundPosition, BackgroundRepeat, BackgroundSize, BackgroundTile,
        };
        let rep = BackgroundRepeat {
            x: BackgroundTile::Repeat,
            y: BackgroundTile::NoRepeat,
        };
        // 定位盒 500 宽、裁剪盒左边多出 60 的边框带：40px 纹理从 −60 铺到 500 是 15 格，
        // 只按定位盒算是 13 格。
        let tiles = background_tiles(
            500.0,
            400.0,
            (-60.0, 0.0, 560.0, 400.0),
            (40.0, 40.0),
            &BackgroundSize::Auto,
            &BackgroundPosition::default(),
            &rep,
        );
        assert_eq!(tiles.len(), 15, "覆盖要伸到裁剪盒的左缘外：{tiles:?}");
        assert!(
            tiles.iter().all(|(l, ..)| *l < 500.0 && *l + 40.0 > -60.0),
            "每格都要与裁剪盒相交"
        );
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
                (0.0, 0.0, box_w, 900.0),
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
                (0.0, 0.0, box_w, 900.0),
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
                None,
                false,
                &[],
            )
            .is_empty()
        );
    }

    /// 滚动档那条「宽度铺满就贴顶」的判据：封面那种竖图在手机屏的章框里是**宽度先
    /// 到边**，该贴顶；左右留白的、以及纵向溢出的（`cover` 比框还高）都不许贴 ——
    /// 后者贴顶会把多出来的那一截压进下一章。
    #[test]
    fn a_page_art_picture_hugs_the_top_only_when_it_fills_the_width() {
        use super::{DecorPicture, picture_fills_width};
        use crate::ui::foundation::images::{BookImages, ChapterImages, no_images};
        use folio_core::content::format::ImageRes;
        use folio_core::content::format::css::computed::{
            BackgroundAxis, BackgroundPosition, BackgroundRepeat, BackgroundSize, BackgroundTile,
        };
        use std::sync::Arc;

        let res = |href: &str, width: u32, height: u32| ImageRes {
            href: href.into(),
            source: href.into(),
            media_type: "image/jpeg".into(),
            width,
            height,
        };
        let table = vec![
            // 封面：1000×1778 在 1080×2530 的框里按宽定 → 1080×1920。
            res("cover.jpg", 1000, 1778),
            // 竖长条：按高定 → 632×2530，左右留白。
            res("narrow.jpg", 600, 2400),
            // 比框还高：cover 按宽定 → 1080×3240，纵向溢出。
            res("tall.jpg", 1000, 3000),
        ];
        let images = ChapterImages::new(
            Arc::new(BookImages::new(&table, no_images())),
            "Text/chap.xhtml",
        );
        let picture = |href: &str, size: BackgroundSize| DecorPicture {
            href: href.into(),
            size,
            position: BackgroundPosition {
                x: BackgroundAxis::Fraction(0.5),
                y: BackgroundAxis::Fraction(0.5),
            },
            repeat: BackgroundRepeat {
                x: BackgroundTile::NoRepeat,
                y: BackgroundTile::NoRepeat,
            },
        };
        assert!(
            picture_fills_width(
                &picture("cover.jpg", BackgroundSize::Contain),
                1080.0,
                2530.0,
                &images
            ),
            "宽度到边、比框矮的封面该贴顶"
        );
        assert!(
            !picture_fills_width(
                &picture("narrow.jpg", BackgroundSize::Contain),
                1080.0,
                2530.0,
                &images
            ),
            "左右留白的那一档仍居中"
        );
        assert!(
            !picture_fills_width(
                &picture("tall.jpg", BackgroundSize::Cover),
                1080.0,
                2530.0,
                &images
            ),
            "纵向溢出的那一档不许贴顶（会压进下一章）"
        );
    }

    /// 水墨：作者写的文字色、底色、边框与 text-shadow 全部按观感亮度变灰，alpha 不动。
    ///
    /// 钉的是「水墨等于正常版式，只是黑白」：底色**不能**被撤掉（那是上一版的做法），
    /// 也不能留着色相。主题色不在这里测 —— 它在换肤那一步处理。
    #[test]
    fn ink_scrubs_author_colours_without_dropping_the_backdrop() {
        let colored = hsla(0.33, 0.62, 0.48, 0.8);
        let shadow = TextShadow {
            dx: 1.0,
            dy: 1.0,
            blur: 0.0,
            color: Some(Color(255, 0, 0, 200)),
        };
        let run = GlyphRun::new(SharedString::from("字"), Font::default(), 16.0)
            .color(colored)
            .shadowed(vec![shadow].into());
        let mut scene = Scene {
            runs: vec![run],
            decorations: vec![BlockDecor {
                x: 0.0,
                top: 0.0,
                width: 300.0,
                height: 40.0,
                background_rect: (0.0, 0.0, 300.0, 40.0),
                origin_rect: (0.0, 0.0, 300.0, 40.0),
                background: Some(colored),
                radius: [0.0; 4],
                border: [(2.0, colored, BorderStyle::Solid); 4],
                pictures: Vec::new(),
                box_shadows: vec![BlockShadow {
                    dx: 1.0,
                    dy: 2.0,
                    blur: 0.0,
                    inset: false,
                    color: colored,
                }],
                outline: Some(BlockOutline {
                    width: 1.0,
                    color: colored,
                    style: BorderStyle::Solid,
                    offset: 1.0,
                }),
                backdrop_blur: None,
            }],
            ..Default::default()
        };
        scene.desaturate();

        let run = &scene.runs[0];
        assert_eq!(run.color.s, 0.0, "正文 run 还带着色相：{:?}", run.color);
        assert!(
            (run.color.a - colored.a).abs() < 1e-6,
            "去饱和改掉了 alpha：{:?}",
            run.color
        );
        // 观感亮度不能变：底色是浅绿，去饱和之后仍该是一块浅灰，而不是黑。
        let gray = Rgba::from(run.color);
        let before = Rgba::from(colored);
        let luma = 0.2126 * before.r + 0.7152 * before.g + 0.0722 * before.b;
        assert!(
            (0.2126 * gray.r + 0.7152 * gray.g + 0.0722 * gray.b - luma).abs() < 0.01,
            "亮度被改了：{gray:?} 原 {before:?}"
        );
        assert_eq!(
            run.text_shadows[0].color,
            Some(Color(54, 54, 54, 200)),
            "影子颜色没按亮度变灰（0.2126×255 ≈ 54），或 alpha 被改了"
        );

        let decor = &scene.decorations[0];
        let background = decor.background.expect("底色被撤掉了，水墨该留着它");
        assert_eq!(background.s, 0.0, "底色还带着色相：{background:?}");
        for edge in &decor.border {
            assert_eq!(edge.1.s, 0.0, "边框还带着色相：{edge:?}");
            assert!((edge.1.a - colored.a).abs() < 1e-6, "边框 alpha 被改了");
        }
        assert_eq!(decor.box_shadows[0].color.s, 0.0, "阴影还带着色相");
        let outline = decor.outline.expect("轮廓被撤掉了");
        assert_eq!(outline.color.s, 0.0, "轮廓还带着色相");
    }

    /// 叠图只预解左右两张邻居：往前滑、往回滑都一样直接清晰；隔了一张的不预解
    /// （一叠可能有八张，全排全清会把图集的封顶挤爆）。
    #[test]
    fn only_the_two_neighbours_of_a_stack_are_prefetched() {
        assert!(stack_neighbor(1, 0), "下一张");
        assert!(stack_neighbor(0, 1), "上一张");
        assert!(stack_neighbor(2, 1), "下一张");
        assert!(!stack_neighbor(3, 1), "隔了一张不预解");
        assert!(!stack_neighbor(0, 0), "当前这张不在这里预解");
        assert!(!stack_neighbor(0, u16::MAX), "末尾不回绕");
    }
}
