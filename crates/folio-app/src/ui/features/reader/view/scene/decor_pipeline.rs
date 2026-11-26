//! 块级装饰短表按可见带折成绘制单元的那一趟。
//!
//! 从 `scene.rs` 切出来只因为那个文件贴着行数上限；路径经 scene 的 `mod` 声明可见。
//! 这里只做每帧必须做的三件事：按可见带裁、加两个屏幕平移、落主题前景 ——
//! 「哪些块带装饰」已在排版换代的同一批里折好（`Layout::decor_seeds`）。

use folio_core::content::format::css::computed::{BorderStyle, Color};
use folio_core::layout::DecorSeed;
use gpui_kit::{Bounds, Hsla, Pixels, point, px, size};

use super::tiles::background_tiles;
use super::{BlockDecor, BlockOutline, BlockPicture, BlockShadow};
use crate::ui::foundation::images::ChapterImages;
use crate::ui::foundation::style::run_color;

/// 把排版层的 `ClipRect` 折成屏幕框；平移规则与 `Scene::build` 里的 inline/cross 同源。
///
/// 只裁单轴（`overflow-x` 或 `overflow-y`）时，另一轴用 `±∞` 表示。GPUI 的
/// `ContentMask` 只收有限矩形，所以这里把无限的那两端换成远大于视口的常数；
/// 它随后会与窗口自己的 mask 取交，那一轴等价于没裁。
pub(in crate::ui::features::reader) fn screen_clip(
    rect: folio_core::layout::ClipRect,
    origin_y: f32,
    vertical: bool,
    slide: f32,
    x: f32,
    top: f32,
) -> Bounds<Pixels> {
    // 比任何一屏都大得多，又远没到 `origin + size` 溢出的量级。
    const FAR: f32 = 1.0e6;
    let x0 = if rect.x0.is_finite() { rect.x0 } else { -FAR };
    let x1 = if rect.x1.is_finite() { rect.x1 } else { FAR };
    let y0 = if rect.y0.is_finite() { rect.y0 } else { -FAR };
    let y1 = if rect.y1.is_finite() { rect.y1 } else { FAR };
    let (left, top) = if vertical {
        (x + x0 + slide, top + y0)
    } else {
        (x + x0, y0 - origin_y + top)
    };
    Bounds {
        origin: point(px(left), px(top)),
        size: size(px(x1 - x0), px(y1 - y0)),
    }
}

/// 栏间分隔线从排版坐标到绘制单元的折法：一条栏线就是一个「只有一条边的装饰块」。
///
/// 横排下那道竖带子等价于 `BlockDecor` 的左边界（`push_border(side=3)` 画的正好是
/// `(x, top, width, height)` 那条带子）；竖排下排版侧已把它投影成横带，落在上边界
/// （`side=0`）。复用边框那条路，省掉一族只画栏线的绘制原语。
fn place_column_rules(
    rules: &[folio_core::layout::ColumnRule],
    band: (f32, f32),
    origin_y: f32,
    vertical: bool,
    slide: f32,
    x: f32,
    top: f32,
    foreground: Hsla,
) -> Vec<BlockDecor> {
    let mut out = Vec::new();
    for rule in rules {
        // 裁剔问推进轴：横排是 y 的一段，竖排是物理 x 的一段（排版侧已投影，
        // 栏线转置后沿 x 占的厚度就是 rule.width）。
        let (start, thick) = if vertical {
            (rule.x, rule.width)
        } else {
            (rule.y, rule.height)
        };
        if start + thick < band.0 || start > band.1 {
            continue;
        }
        let (left, top_px, width, height) = if vertical {
            (x + rule.x + slide, top + rule.y, rule.width, rule.height)
        } else {
            (x + rule.x, rule.y - origin_y + top, rule.width, rule.height)
        };
        let edge = (width, run_color(rule.color, foreground), rule.style);
        let clear = (0.0, foreground, BorderStyle::None);
        out.push(BlockDecor {
            x: left,
            top: top_px,
            width,
            height,
            background_rect: (left, top_px, width, height),
            origin_rect: (left, top_px, width, height),
            background: None,
            radius: [0.0; 4],
            border: if vertical {
                [edge, clear, clear, clear]
            } else {
                [clear, clear, clear, edge]
            },
            pictures: Vec::new(),
            box_shadows: Vec::new(),
            outline: None,
            backdrop_blur: None,
            clip: None,
        });
    }
    out
}

/// 把 `Layout::decor_seeds` 折好的装饰短表按可见带裁一刀，落平移数与主题前景。
///
/// 不再走整章块表（那会每帧每章扫 160~600+ 块）；每帧只剩四件带「此刻」的事：
/// 可见区间、滚动、主题色、随图片索引变的背景图固有尺寸（故不烘进表）。
///
/// 判据逐字沿用原式，且**只能整表扫**：块是嵌套的，父盒 `y+height` 远大于紧随的子盒，
/// 「相交块是连续下标」这个前提在块上不成立（`Layout::stacked` 是为行盒量的）。
/// 表短（只有真带装饰的块）是代价低，不是被二分 —— 高盒测试就是这条前提的活证据。
#[allow(clippy::too_many_arguments)] // 九个都是「这一帧」的输入，包成结构体只是换个地方摆
pub(in crate::ui::features::reader) fn place_decorations(
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
    // 阅读器已经拿去当**整窗背板**画的那一条根底图，在 `seeds` 里的下标（`None` = 这一章
    // 没有整页底图）。版心里那一份必须跳过 —— 位置还差着一个页眉与安全区。
    taken_over: Option<usize>,
    // 这一章**没有可读正文**（纯图页 / 卷首那一类）。见 [`page_color_is_drawn_by_reader`]。
    picture_only_chapter: bool,
    // 多栏容器的栏间分隔线（排版侧已按块折好），与块装饰共用一张绘制表。
    column_rules: &[folio_core::layout::ColumnRule],
) -> Vec<BlockDecor> {
    let mut out = Vec::new();
    for (index, seed) in seeds.iter().enumerate() {
        // 被背板接管的那一条自己不算；其余 `full_bleed` 的也跟着跳过是既有口径 ——
        // 那种章的根底图整窗铺，块里再画一遍就是「图多出来一截」。
        if taken_over == Some(index) || (taken_over.is_some() && seed.full_bleed) {
            continue;
        }
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
        // 裁剪盒与定位盒同一套平移：裁剪盒管裁、定位盒管量。
        let fold = |rect: (f32, f32, f32, f32)| {
            if vertical {
                (x + rect.0 + slide, top + rect.1, rect.2, rect.3)
            } else {
                (x + rect.0, rect.1 - origin_y + top, rect.2, rect.3)
            }
        };
        let background_rect = fold(seed.background_rect);
        let origin_rect = fold(seed.origin_rect);
        let mut pictures = Vec::new();
        for picture in &seed.pictures {
            let Some(request) = images.decode_request(&picture.href) else {
                continue;
            };
            // 格子按**定位盒**算相位与尺寸，铺到**裁剪盒**的边为止：裁剪盒折进
            // 定位盒坐标系传下去（两盒各管各的，见 `background_tiles`）。
            let tiles = background_tiles(
                origin_rect.2,
                origin_rect.3,
                (
                    background_rect.0 - origin_rect.0,
                    background_rect.1 - origin_rect.1,
                    background_rect.2,
                    background_rect.3,
                ),
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
            background_rect,
            origin_rect,
            background: if super::decor::page_color_is_drawn_by_reader(
                index,
                seed,
                picture_only_chapter,
            ) {
                // 整页底色只由 `reading_theme::resolve` 画一次（`column.bg(reading.bg)`），
                // 这里再画一遍就是同一件事的第二个真相 —— 见判据那一条的文档。
                None
            } else {
                seed.background
                    .map(|color| run_color(Some(color), foreground))
            },
            radius: seed.radius,
            border: [
                edge(seed.border[0]),
                edge(seed.border[1]),
                edge(seed.border[2]),
                edge(seed.border[3]),
            ],
            pictures,
            box_shadows: seed
                .box_shadows
                .iter()
                .map(|shadow| BlockShadow {
                    dx: shadow.dx,
                    dy: shadow.dy,
                    blur: shadow.blur,
                    inset: shadow.inset,
                    color: run_color(shadow.color, foreground),
                })
                .collect(),
            outline: seed
                .outline
                .map(|(width, color, style, offset)| BlockOutline {
                    width,
                    color: run_color(color, foreground),
                    style,
                    offset,
                }),
            backdrop_blur: seed.backdrop_blur,
            clip: seed
                .clip
                .map(|rect| screen_clip(rect, origin_y, vertical, slide, x, top)),
        });
    }
    out.extend(place_column_rules(
        column_rules,
        band,
        origin_y,
        vertical,
        slide,
        x,
        top,
        foreground,
    ));
    out
}
