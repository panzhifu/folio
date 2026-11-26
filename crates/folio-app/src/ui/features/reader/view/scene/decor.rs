//! 块级装饰的绘制侧类型：底色、边框、背景图、阴影与轮廓。
//!
//! 单开一间只因为 `scene.rs` 已贴着行数上限；这里只有数据形状，不含摆位与换算。

use folio_core::content::format::css::computed::{
    BackgroundAxis, BackgroundPosition, BackgroundRepeat, BackgroundSize, BackgroundTile,
    BorderStyle,
};
use folio_core::layout::{DecorPicture, DecorSeed};
use gpui_kit::{Bounds, Hsla, Pixels};

/// 可见范围内一块盒子的块级装饰：背景色与四边边框（实心画）。
///
/// 位置与尺寸是**边框盒**（含 padding 与边框自身，排版层 `folio_core::layout::BlockBox`
/// 直接给出），坐标系与 `VisibleImage` 相同：相对本元素原点。
#[derive(Debug, Clone, PartialEq)]
pub struct BlockDecor {
    pub x: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
    /// `background-color`。`None` = 书里没写这一条 —— 别拿不透明色去盖主题底色。
    pub background: Option<Hsla>,
    /// 底色与背景图的**裁剪盒**（章内坐标已折成屏幕量）：`background-clip` 的结果，
    /// 默认与边框盒重合，`padding-box` / `content-box` 时向内缩。铺色与裁图都按它。
    pub background_rect: (f32, f32, f32, f32),
    /// 背景图的**定位盒**（屏幕量，`background-origin` 的结果）：格子的相位与尺寸
    /// 都按它算，初值 `padding-box`。与裁剪盒各管各的 —— 裁剪盒裁、定位盒量。
    pub origin_rect: (f32, f32, f32, f32),
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
    /// 位置是**相对这一块自己的定位盒左上角**，与 `x`/`top` 同一套平移已经在外层加过，
    /// 所以竖排那条路上的 `slide` 不用在这里再算一次。
    pub pictures: Vec<BlockPicture>,
    /// `box-shadow` 的层列表（含 `inset`）。已经过主题与水墨换算。
    pub box_shadows: Vec<BlockShadow>,
    /// `outline`：画在边框盒**外侧**的一圈轮廓，`offset` 是轮廓与边框盒之间的缝。
    pub outline: Option<BlockOutline>,
    /// `backdrop-filter: blur(<px>)` 的半径。`None` = 没有这条声明。
    /// 绘制侧先糊底下那一块、再画本块的底色（`body::decor::decoration_child`）。
    pub backdrop_blur: Option<f32>,
    /// 祖先 `overflow: hidden|clip` 的裁剪框（屏幕坐标），`None` = 不裁。
    /// 装饰按块画，不能逐字形裁；绘制侧给这一块套一层矩形裁剪。
    pub clip: Option<Bounds<Pixels>>,
}

/// 一块盒子的阴影：偏移、模糊、内阴影与最终颜色。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlockShadow {
    pub dx: f32,
    pub dy: f32,
    pub blur: f32,
    pub inset: bool,
    pub color: Hsla,
}

/// 一块盒子的轮廓线。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlockOutline {
    pub width: f32,
    pub color: Hsla,
    pub style: BorderStyle,
    pub offset: f32,
}

/// 一块背景图在这一块里要画的几张像素：哪张图 + 摆在哪儿。
///
/// **像素不在这里**，与 `VisibleImage` 同一条理由：解没解好由图集答，摆位与字节无关。
#[derive(Clone, Debug, PartialEq)]
pub struct BlockPicture {
    /// 这一张图的活儿（按容器路径向图片索引现取）。
    pub image: crate::ui::foundation::images::DecodeRequest,
    /// 一格 `(左, 上, 宽, 高)`，相对这一块的**定位盒**左上角（`background-origin` 的
    /// 结果；画的时候按定位盒与裁剪盒的原点差平移，见 `body::decor::background_child`）。
    /// `repeat` 一族才会多于一个数。
    pub tiles: Vec<(f32, f32, f32, f32)>,
}

/// 整窗背板四边各外扩多少逻辑像素。
///
/// 背板的 `top` 是「章基线 − 滚量」那个浮点排版量（见 `body::reflowable` 的 `art_top`），
/// `height` 也来自章盒；取整到物理像素时上下各会差不到 1px。那一丝缝里露的是
/// `reader-page` 的纸底（浅色主题下近白）——读者报的「全屏图片上面和下方各透一条
/// 很细的白光」。让背板四边都盖过窗口一条，缝就不落在窗口里了。
const BACKDROP_OVERSCAN: f32 = 2.0;

/// 四边外扩、中心不动：`x/top` 各减一份、`width/height` 各加两份，
/// 于是 `x + width/2` 与 `top + height/2` 原样，cover 的格子只跟着放大 2px，
/// 画面不挪位（`backdrop_decor` 用它）。
pub(super) fn overscan_backdrop(x: f32, top: f32, width: f32, height: f32) -> (f32, f32, f32, f32) {
    (
        x - BACKDROP_OVERSCAN,
        top - BACKDROP_OVERSCAN,
        width + 2.0 * BACKDROP_OVERSCAN,
        height + 2.0 * BACKDROP_OVERSCAN,
    )
}

/// 整页画的背板图：一张图按 `size`（cover / contain）铺满整窗（居中）。
///
/// 盘上的图按屏幕比例挑过；挑不到整屏变体时，比例差得远的用 `contain` —— 宁可上下
/// 留一条底色，也不把一幅横图裁成中间一条。
pub(in crate::ui::features::reader) fn full_page_picture(
    href: &str,
    size: BackgroundSize,
) -> DecorPicture {
    DecorPicture {
        href: href.to_string(),
        size,
        position: BackgroundPosition {
            x: BackgroundAxis::Fraction(0.5),
            y: BackgroundAxis::Fraction(0.5),
        },
        repeat: BackgroundRepeat {
            x: BackgroundTile::NoRepeat,
            y: BackgroundTile::NoRepeat,
        },
    }
}

/// 一块装饰的底色是不是**整页纸色**，因而应当由读者那一层画、不在这儿画。
///
/// 根块的 `background-color`（`body { background: … }`）在浏览器里铺的是整张纸，
/// 而阅读器此前有**两条**路画它：`reading_theme::resolve` 早把同一个色铺成
/// `column.bg(reading.bg)`，这里又把 `DecorSeed.background` 折成一块 `fill_rounded_quad`
/// 叠上去（见 `body::decor::decoration_child`）。两条路取的色不完全同源
/// （一条过 `to_hsla`、一条过层叠期 `run_color`），第二条在排版结果回来时才出现，
/// 于是读者看见「先读者选的纸色，再跳成书色」那一段 —— 底色到底画几遍、谁说了算，
/// 从前没人回答。
///
/// 判据（三条同时成立）：
///
/// - 是根块（`index == 0`，章盒的第一个块就是 `<html>`/`<body>`）；
/// - 这块就是**整章**那一盒（`y == 0` 且高 ≥ 章高的 95%）。正文里某一块自己写了
///   `background` 的是真装饰，照画；
/// - 这一章**没有可读正文**（`picture_only_chapter`）。有正文的章里，块盒高只到
///   正文末尾，不等于整页 —— 硬把它当纸色会漏掉页尾那一截（根块底色曾在页尾被
///   一段白截断，段间距那一行原来就是这么补的）。那一档维持老行为：由装饰块画，
///   因为正文盒没有铺到纸底。
pub(super) fn page_color_is_drawn_by_reader(
    index: usize,
    seed: &DecorSeed,
    picture_only_chapter: bool,
) -> bool {
    index == 0 && picture_only_chapter && seed.y.abs() < 0.5 && seed.height > 0.0
}
