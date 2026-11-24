//! 背景图摆位的纯数学：一张图画多大、落点在哪、铺几格。
//!
//! 从 scene.rs 切出来只因为那个文件贴着行数上限；路径经 scene 的 re-export 不变。

use crate::ui::foundation::images::ChapterImages;
use folio_core::content::format::css::computed::{
    BackgroundAxis, BackgroundEdge, BackgroundPosition, BackgroundRepeat, BackgroundSize,
    BackgroundTile, Length,
};
use folio_core::layout::DecorPicture;

/// 一块背景图最多摆多少格。铺法那一族数学上无界（4×4 纹理盖 600×4000 长条是一万五千格），
/// 封顶后**退化成只摆定位那一格**（`no-repeat`）：画少几格是看得出的糙，卡成个位数帧全书都读不下去。
/// 真实语料从没碰到（哈利波特 23 处最多 450 格）—— 它是护栏，不是路径。
const MAX_BACKGROUND_TILES: usize = 1024;

/// 一块背景图在这个盒里要摆的格子（相对**定位盒**左上角）。入参全是这一帧拿得到的数：
/// `box_*` 是定位盒（`background-origin` 的结果）尺寸，`cover` / 百分比定位都按它算；
/// `clip` 是裁剪盒折到定位盒坐标系里的矩形 —— `repeat` 要铺到**裁剪盒**的边为止
/// （CSS 的图铺满绘制区、相位取自定位区），`space` / `round` 按定位盒本身数张数。
/// **拿不到固有尺寸就一条都不摆**：`cover` / `contain` 与百分比尺寸都要按比例反算另一轴，
/// 猜一个的后果是底图被拉成怪形状，比空着更难查。`background-size` 至少一条写死的走另一条路。
pub(super) fn background_tiles(
    box_w: f32,
    box_h: f32,
    clip: (f32, f32, f32, f32),
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
    let phase = (
        axis_offset(box_w, tile_w, &position.x),
        axis_offset(box_h, tile_h, &position.y),
    );
    let rows = axis_span(box_h, tile_h, repeat.y, phase.1, (clip.1, clip.1 + clip.3));
    let cols = axis_span(box_w, tile_w, repeat.x, phase.0, (clip.0, clip.0 + clip.2));
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
        return vec![(phase.0, phase.1, tile_w, tile_h)];
    }
    let mut out = Vec::with_capacity(rows.len() * cols.len());
    for (top, height) in &rows {
        for (left, width) in &cols {
            out.push((*left, *top, *width, *height));
        }
    }
    out
}

/// 一轴上要摆的格子：**这一轴上的绝对位置** `(起点, 长)`，只列与 `[0, side)` 相交的。
/// `cover` 是这一轴上要盖住的范围（定位盒坐标系，`repeat` 用它铺到裁剪盒的边）。
/// 从前按 ±`ceil(side/tile)+1` 对称铺开会在正当写法上撞护栏（40×40 盖 600 版心算 1089 格），
/// 按相交范围收同一张图是 15 列。
/// `space`：装几张摆几张、余量**均摊成隙**（图不缩、边不裁），装不下就画一张溢出裁掉；
/// `round`：按能装的张数取整再**把图缩放到整除**，无缝也无裁切。这两档的张数按
/// 定位盒算（CSS 明文），不跟着裁剪盒扩。
fn axis_span(
    side: f32,
    tile: f32,
    mode: BackgroundTile,
    origin: f32,
    cover: (f32, f32),
) -> Vec<(f32, f32)> {
    match mode {
        BackgroundTile::NoRepeat => vec![(origin, tile)],
        BackgroundTile::Repeat => {
            // 要摆的那些格：与裁剪盒 `[cover.0, cover.1)` 相交。图比这一轴还宽时
            // `first` 就是含 0 的那一格（它自己已经溢出，溢出的一截由外壳裁掉）。
            let first = ((cover.0 - origin) / tile).floor() as i32;
            let last = ((cover.1 - origin) / tile).ceil() as i32 - 1;
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

/// 这一张整页画**宽度铺满了框、而且比框矮**吗。
///
/// 滚动档用它决定「贴顶还是居中」：封面那种竖图在手机屏上是宽度先到边（`contain`
/// 由宽那一轴定），把它贴着屏幕顶边画才有沉浸感；宽度不足（左右留白）或比框还高
/// （`cover` 纵向溢出）的仍按作者的定位居中 —— 后者贴顶会把溢出的那一截压进下一章。
pub(in crate::ui::features::reader) fn picture_fills_width(
    picture: &DecorPicture,
    width: f32,
    height: f32,
    images: &ChapterImages,
) -> bool {
    let (natural_w, natural_h) = images.natural_size(&picture.href);
    background_tile_size(width, height, (natural_w, natural_h), &picture.size)
        .is_some_and(|(tile_w, tile_h)| tile_w >= width - 0.5 && tile_h <= height + 0.5)
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
