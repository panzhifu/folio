//! 块级装饰种子的数据形状：几何、底色、边框、背景图层。
//!
//! 从 geometry.rs 切出来只因为那个文件贴着行数上限；路径经 re-export 不变。

use std::sync::Arc;

use crate::BlockBox;
use folio_format::css::computed::StyledDom;
use folio_format::css::computed::{
    BackgroundPosition, BackgroundRepeat, BackgroundSize, BorderStyle, Color, ComputedStyle,
    Display, ShadowLayer,
};
/// 一块要画的装饰的**输入**：几何 + CSS 侧的颜色，主题色一格都没落地。
///
/// 「哪些块带底色或可见边框」只随层叠结果变、不随帧变：从前每帧每章走整章块表筛，
/// 现在整章走一次折成短表，每帧扫短表。
///
/// **不能像 `lines_in_band` 那样按 y 二分**：块是嵌套的，父盒 `y+height` 远大于紧随的子盒，
/// 「相交块是连续下标」在块表上不成立（`Layout::stacked` 是为行盒量的）。这张表短，整表扫。
#[derive(Debug, Clone, PartialEq)]
pub struct DecorSeed {
    /// 边框盒左上角（章内坐标，与 `BlockBox::x`/`y` 同一套）。
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// `background-color`。`None` = 书里没写这一条 —— 绘制侧别拿不透明色去盖主题底色。
    pub background: Option<Color>,
    /// 底色与背景图的**裁剪盒**（章内坐标，`background-clip` 的结果）：默认就是上面的边框盒，
    /// `padding-box` / `content-box` 时向内缩掉边框与内边距。绘制侧按它铺色与裁图。
    pub background_rect: (f32, f32, f32, f32),
    /// 背景图的**定位盒**（章内坐标，`background-origin` 的结果）：`cover` / `contain`
    /// 与百分比定位的相位、尺寸都按它算，初值 `padding-box`（CSS 同）。与上面那个
    /// 裁剪盒各管各的 —— 裁剪盒说「画到哪儿为止」，定位盒说「从哪儿量起」。
    pub origin_rect: (f32, f32, f32, f32),
    /// `[上, 右, 下, 左]` 的（要画的宽, 色, 画法）。宽 0 不画。
    ///
    /// 色在这里**已经把 `currentColor` 折完了**：没写 border-color 就取本块自己的
    /// `color`（`color` 可继承，所以那一色在层叠期不一定写在本元素上）。
    /// 剩下的只有「`None` 落主题前景」那一步 —— 那是主题，必须每帧现落，不许烘进这张表。
    ///
    /// 画法只可能是 `Solid` / `Double` / `Dotted` / `Dashed`（[`BorderEdge::drawn`] 已经把
    /// `none` 折成 0 的边挡在外面）。画法必须带出去：`Double` 要拆成两条 1/3 细线、
    /// `Dotted` / `Dashed` 要按段切开，丢了画法就退化成一条实线
    /// （真书《人类简史》的章名 `.biaoti` 上下双线、哈利·波特 `.line` 那条点线）。
    ///
    /// [`BorderEdge::drawn`]: folio_format::css::computed::BorderEdge::drawn
    pub border: [(f32, Option<Color>, BorderStyle); 4],
    /// 四个角的圆角半径（px），顺序 `[左上, 右上, 右下, 左下]`，由
    /// [`ComputedStyle::corner_radii`] 按这一块的盒尺寸折好。
    ///
    /// 只作用于**底色**那一块四边形：边框仍是四条轴对齐的直条，所以圆角盒配粗边框时
    /// 四角是直的（`ui::reader::view::body::decoration_child` 那一段）。
    /// [`ComputedStyle::corner_radii`]: folio_format::css::computed::ComputedStyle::corner_radii
    pub radius: [f32; 4],
    /// `background-image` 的那些层，按 CSS 书写顺序（先画的在屏幕上更靠上）。空 = 没图。
    ///
    /// **字节与固有尺寸不在这里**，与 `RowImage::href` 同一个口径：那两样随图片索引变，
    /// 每帧现问 `ChapterImages` 才是对的，烘进这张表就是第二个真相。
    /// 摆位要的盒尺寸倒是已经在 `x/y/width/height` 里了。
    pub pictures: Vec<DecorPicture>,
    /// `box-shadow` 的层列表（含 `inset`）。色在层叠期已是 `Color`；`None` 落本块字色。
    /// 空 = 没有阴影。
    pub box_shadows: Arc<[ShadowLayer]>,
    /// `outline`：`(宽, 色, 画法, 偏移)`，画在边框盒**外侧**。`None` = 没有或宽 0。
    pub outline: Option<(f32, Option<Color>, BorderStyle, f32)>,
    /// `backdrop-filter: blur(<px>)` 的半径。`None` = 没有这条声明。
    /// 绘制侧只把它转成 gpui 的毛玻璃 primitive；底下压着什么由绘制顺序决定。
    pub backdrop_blur: Option<f32>,
    /// 这是**章节根**的一块 `background-attachment: fixed` 底图：书要的是一整页的画。
    ///
    /// 阅读器把它当整窗背板画（贴在窗口上、不进版心），并把这条种子从常规块装饰里
    /// 滤掉 —— 否则同一张画会在版心里再画一遍，位置还差着一个页眉与安全区。
    /// 判据与「这一章至少一屏高」那条同源（见 `layout_inner`）。
    pub full_bleed: bool,
    /// 祖先 `overflow: hidden|clip` 给这一块的裁剪框（章内物理坐标）。
    /// `None` = 不在任何 overflow 祖先里；块自己的 `overflow` 不裁自己的边框与底色。
    pub clip: Option<crate::ClipRect>,
}

/// 一块背景图的一层：哪张图 + 怎么摆。全是 CSS 侧的原始数，一个像素结果都不带。
#[derive(Debug, Clone, PartialEq)]
pub struct DecorPicture {
    /// 容器内的绝对路径。样式表拼进层叠前已由 `absolutize_css_urls` 折算过，
    /// 所以这里能直接拿去问图片索引，不需要再按本章目录解一次相对路径。
    pub href: String,
    pub size: BackgroundSize,
    pub position: BackgroundPosition,
    pub repeat: BackgroundRepeat,
}

/// 一条栏间分隔线：多栏容器在栏缝正中画的一条竖带子。
///
/// 与 `DecorSeed` 分开成表：它不是某个块自己的边框，而是块级多栏容器的**附加产物**
/// （一条多栏可能画 n−1 条），挂在 `Layout::column_rules` 上随整章排版结果走。
/// 三要素复用 `BorderStyle` 的画法，色在层叠期已折好；`None` 落本块字色那一刀
/// 由 `decor_seeds` emit 时当场做（`color.or(style.color)`），与边框同一条规矩。
///
/// 坐标是**章内坐标**，横排下 x 是竖带子左缘、y 是顶缘、w 是线宽、h 是内容高；
/// 竖排下由 `transposed` 一并投影成横线（块轴转置）。
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnRule {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub color: Option<Color>,
    pub style: BorderStyle,
}

/// 同一张塌陷表里挨着我某一侧的邻格，`side` 按 `[上, 右, 下, 左]`。
///
/// 判据是**几何**的，不是 DOM 父子：`decor_seeds` 手上只有块表，而格子的边框盒在排版期
/// 已按列宽、行高摆好 ——「右边那一格」就是 `x` 恰落在我 `x + width` 上、且与我同一行的那块。
/// 用几何而不是 DOM，块缓存命中的那条路也对：缓存里存的就是块表。
pub(crate) fn neighbour_cell<'a>(
    blocks: &[BlockBox],
    styles: &'a StyledDom,
    index: usize,
    side: usize,
) -> Option<&'a ComputedStyle> {
    const TOL: f32 = 0.5;
    let me = &blocks[index];
    blocks.iter().enumerate().find_map(|(other_index, other)| {
        if other_index == index || !other.border_box {
            return None;
        }
        let style = styles.get(other.node)?;
        if style.display != Display::TableCell || !style.border_collapse {
            return None;
        }
        let touches = match side {
            0 => ((other.y + other.height) - me.y).abs() < TOL && (other.x - me.x).abs() < TOL,
            1 => (other.x - (me.x + me.width)).abs() < TOL && (other.y - me.y).abs() < TOL,
            2 => (other.y - (me.y + me.height)).abs() < TOL && (other.x - me.x).abs() < TOL,
            _ => ((other.x + other.width) - me.x).abs() < TOL && (other.y - me.y).abs() < TOL,
        };
        touches.then_some(style)
    })
}
