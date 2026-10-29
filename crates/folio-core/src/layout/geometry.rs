//! 排版结果的数据形状：run、行片段、行盒、块盒、`Layout` 与其查询，加上 `LayoutOptions`。
//!
//! 从 `layout` 主模块切出来只为了一个文件能一次读完；跨文件的条目用
//! `pub(super)`，即**只对 `layout` 内部开放**，不属于对外公开面。

use std::ops::Range;

use super::collect::IMAGE_GLYPH;
use super::font::RunStyle;
use crate::content::dom::NodeId;
use crate::content::format::css::computed::{
    BackgroundPosition, BackgroundRepeat, BackgroundSize, BorderStyle, Color, ComputedStyle,
    Display, StyledDom, TextAlign, WritingMode,
};
use crate::infra::metrics::Metric;

/// 行盒里的一段连续同样式文本，同时带着两套坐标。
#[derive(Debug, Clone, PartialEq)]
pub struct LineRun {
    /// 文本节点。§4.2 把锚点定义在 Layout **之前**的坐标系上，就是这个 node。
    pub node: NodeId,
    /// `BlockBox::text` 的字节区间 —— 绘制用。
    pub norm: Range<usize>,
    /// `node.text` 的字节区间 —— 选择与锚点用。
    pub orig: Range<usize>,
    pub style: RunStyle,
    /// 实际绘制宽度与基线/下沉量。
    pub width: f32,
    pub ascent: f32,
    pub descent: f32,
}

/// 行盒里的一段图片。`href` 是**解析后的容器绝对路径**，绘制层按它取字节解码。
/// 宽度/高度已按 CSS 与内在尺寸算好，绘制层不需要再量。
#[derive(Debug, Clone, PartialEq)]
pub struct ImageRun {
    pub node: NodeId,
    pub href: String,
    pub width: f32,
    pub height: f32,
    /// 横向前进量：绘制宽 + 两侧留白。脚注标记图靠它在前后字之间让出缝。
    pub advance: f32,
    /// 绘制位置在前进量里的左偏移（留白）。
    pub x_offset: f32,
    /// `vertical-align` 折出来的基线偏移（正数往上）。与文本 run 的那一格同源。
    pub baseline_shift: f32,
}

/// 行盒里的一个列表标记（CSS 的 `::marker`）。
///
/// 它与 [`LineRun`] 的区别是**坐标系**：标记的文字是生成的，DOM 里没有对应的字节，
/// 所以它不进 `text_runs()` —— 选区、命中、锚点换算三条路都只认 `Text` 那一个分支，
/// 于是「点在一个标记上」不会把光标放到段首，选一段文字也不会把 `1.` 圈进去。
/// 全书的进度与批注锚点建立在可见文本偏移上，标记一旦混进那一套坐标就是整体漂移。
#[derive(Debug, Clone, PartialEq)]
pub struct MarkerRun {
    /// 挂着这个标记的块级节点（`display: list-item`，通常是 `<li>`）。
    pub node: NodeId,
    /// 标记自己的文字（`"1."` / `"•"` / `"三、"`），**不是**块文本的区间。
    pub text: String,
    /// 字体取 `<li>` 自己的计算样式：CSS 的 `::marker`  inherits from the list item。
    pub style: RunStyle,
    pub width: f32,
    pub ascent: f32,
    pub descent: f32,
    /// 相对**行首**的横向偏移。`outside`（我们唯一的摆法）把它挂在起始边之外：
    /// 横排是负数，RTL 是「行宽 + 间距」那样落到右边缘之外。
    pub offset: f32,
}

/// 行盒里的一段内容：文本、替换元素（图片），或列表标记。
#[derive(Debug, Clone, PartialEq)]
pub enum LineFragment {
    Text(LineRun),
    Image(ImageRun),
    Marker(MarkerRun),
}

/// 两端对齐时余量往**哪种隙**摊。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JustifyMode {
    /// 拉空格（词间）。中文行没有空格时这一档不产生任何隙。
    Spaces,
    /// 拉字间。
    Chars,
}

/// 一个行盒的两端对齐方案：每一处隙多摊多少 px，摊在哪种隙上，一共几处隙。
///
/// 绘制、命中、选区**三处必须按同一规则**把 `extra` 一笔笔加上去：某字符的 x 偏移 =
/// `extra × 它之前的隙数`（`Spaces` 数空格，`Chars` 数字符）。规则只有这一句
/// （[`Justify::shift`] 与 [`Justify::advance_cursor`]），三处各实现一遍只是各自走一遍
/// 片段，走的顺序与这一份规则保证一致。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Justify {
    /// 每一处隙多摊的宽度（px）。
    pub extra: f32,
    pub mode: JustifyMode,
    /// 这一行一共几处隙。撑开后的墨迹宽 = `LineBox::width + extra * gaps`，
    /// 它就是 `ink_width` 的来源，也让命中夹取知道这一行铺得到哪。
    pub gaps: u32,
}

impl Justify {
    /// 游标在 `cursor`（它之前的隙数）处的横向偏移（px，相对未对齐时的位置）。
    pub fn shift(&self, cursor: u32) -> f32 {
        self.extra * cursor as f32
    }

    /// 处理完一个字符之后游标要加几。`Spaces` 档只在空格后加，`Chars` 档每个字符后都加。
    /// 调用方**先取 [`shift`](Self::shift) 再按这一笔推进游标**，与绘制、命中逐字一致。
    pub fn advance_cursor(&self, ch: char) -> u32 {
        match self.mode {
            JustifyMode::Spaces => u32::from(ch == ' '),
            JustifyMode::Chars => 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LineBox {
    /// 所属块的 DOM 节点。
    pub owner: NodeId,
    /// 行内片段，按绘制顺序排列。空行（`<p><br></p>`）没有片段。
    pub fragments: Vec<LineFragment>,
    pub x: f32,
    pub y: f32,
    /// 各片段绘制宽度之和。
    pub width: f32,
    /// 行盒高 = 各片段 inline box 高度的最大值。
    pub height: f32,
    /// 相对 `y` 的基线下沉量：各片段最大的 ascent。
    pub baseline: f32,
    pub align: TextAlign,
    /// 两端对齐的摊法；`None` = 这一行不两端对齐（左/右/中/起边，或段末行、`<br>` 行、
    /// RTL 行）。见 [`Justify`]。
    pub justify: Option<Justify>,
}

impl LineBox {
    /// 文本片段，按绘制顺序。**标记不在这一列**，见 [`MarkerRun`]。
    pub fn text_runs(&self) -> impl Iterator<Item = &LineRun> {
        self.fragments.iter().filter_map(|f| match f {
            LineFragment::Text(r) => Some(r),
            LineFragment::Image(_) | LineFragment::Marker(_) => None,
        })
    }

    /// 图片片段，按绘制顺序。
    pub fn images(&self) -> impl Iterator<Item = &ImageRun> {
        self.fragments.iter().filter_map(|f| match f {
            LineFragment::Image(r) => Some(r),
            LineFragment::Text(_) | LineFragment::Marker(_) => None,
        })
    }

    /// 这一行的**墨迹宽度**：两端对齐把它撑满可用宽，否则就是各片段宽度之和。
    ///
    /// 命中夹取与「整行兜底矩形」用这一格，因为对齐后字真铺到了右边；而 `LineBox::width`
    /// 仍是**未撑开**的片段宽之和（列表标记的 RTL 偏移等仍按它算，不受对齐影响）。
    pub fn ink_width(&self) -> f32 {
        match self.justify {
            Some(j) => self.width + j.extra * j.gaps as f32,
            None => self.width,
        }
    }

    /// 这一行里最大的字号。状态栏与「这行是不是标题」一类判断用；
    /// 纯图片行没有文本字号，返回 0。
    pub fn dominant_font_size(&self) -> f32 {
        self.text_runs()
            .map(|r| r.style.font_size)
            .fold(0.0f32, f32::max)
    }
}

#[derive(Debug, Clone)]
pub struct BlockBox {
    pub node: NodeId,
    /// 边框盒左上角（不含 margin）。
    pub x: f32,
    pub y: f32,
    /// 边框盒尺寸（含 padding，不含 margin）。
    pub width: f32,
    pub height: f32,
    /// 本块行内内容的归一化文本；`LineRun::norm` 索引到这里。容器块为空串。
    pub text: String,
    /// `Layout::lines` 的下标区间。
    pub lines: Range<usize>,
    /// 这条注册是不是**边框盒**（几何含边框与 padding，绘制背景/边框用）。
    ///
    /// 一个块级节点可能注册两条：`block_uncached` 的边框盒（true），以及
    /// `wrap_paragraph` 为正文映射补注册的**内容盒**副本（false —— 几何内缩了
    /// 边框与 padding，只承载归一化文本）。装饰只画边框盒：内容盒副本要是也画，
    /// 它的边框条会不偏不倚落在文字起点上（真书《斗战狂潮》目录页踩过）。
    pub border_box: bool,
}

/// 一块要画的装饰的**输入**：几何 + CSS 侧的颜色，主题色一格都没落地。
///
/// 为什么要单独有这张表：「哪些块带底色或可见边框」只随层叠结果变，而层叠结果随排版
/// 一起换 —— 它**不随帧变**。从前绘制侧每帧、每个进链的章各把整章的块表走一遍来筛
/// 这几条（`plans/阅读管线.md` §十一 第 12 条，一份语料一章 160 块、长章 600+）。
/// 现在整章走一次、折成这张短表存着，每帧扫的是短表。
///
/// **这张表为什么不能像 `lines_in_band` 那样按 y 二分**：块是**嵌套**的，父盒的
/// `y + height` 远大于紧随其后的子盒（父 5000、子 120），所以「与一条带相交的那些块
/// 是连续下标区间」这个前提在块表上根本不成立 —— `Layout::stacked` 那一格是为行盒
/// 量的（行不嵌套），搬到块上就会悄悄漏画。于是这张表的正确用法就是整表扫：
/// 它短（只有真带装饰的块），而且扫得对。
#[derive(Debug, Clone, PartialEq)]
pub struct DecorSeed {
    /// 边框盒左上角（章内坐标，与 `BlockBox::x`/`y` 同一套）。
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// `background-color`。`None` = 书里没写这一条 —— 绘制侧别拿不透明色去盖主题底色。
    pub background: Option<Color>,
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
    /// [`BorderEdge::drawn`]: crate::content::format::css::computed::BorderEdge::drawn
    pub border: [(f32, Option<Color>, BorderStyle); 4],
    /// 四个角的圆角半径（px），顺序 `[左上, 右上, 右下, 左下]`，由
    /// [`ComputedStyle::corner_radii`] 按这一块的盒尺寸折好。
    ///
    /// 只作用于**底色**那一块四边形：边框仍是四条轴对齐的直条，所以圆角盒配粗边框时
    /// 四角是直的（`ui::reader::view::body::decoration_child` 那一段）。
    /// [`ComputedStyle::corner_radii`]: crate::content::format::css::computed::ComputedStyle::corner_radii
    pub radius: [f32; 4],
    /// `background-image` 的那些层，按 CSS 书写顺序（先画的在屏幕上更靠上）。空 = 没图。
    ///
    /// **字节与固有尺寸不在这里**，与 `RowImage::href` 同一个口径：那两样随图片索引变，
    /// 每帧现问 `ChapterImages` 才是对的，烘进这张表就是第二个真相。
    /// 摆位要的盒尺寸倒是已经在 `x/y/width/height` 里了。
    pub pictures: Vec<DecorPicture>,
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

/// 排版结果：两张扁平表 + 两张**排完就折好**的查询索引。
///
/// 索引放在结构里而不是让调用方每次现算是因为查询的形状：`blocks` 与 `lines` 都按**先序**
/// 入表，「第 i 行属于哪个块」按规则要扫到最后一个包含它的盒子，而一帧里这个问题会被问
/// 「可见行数」那么多次 —— 于是每帧 O(可见行 × 全部块)。同一次折表顺手量出行盒是不是
/// 自上而下叠放的，是的话按 y 的三个查询都能走二分（`plans/阅读管线.md` §十一 第 3–5 条）。
#[derive(Debug, Default)]
pub struct Layout {
    pub blocks: Vec<BlockBox>,
    pub lines: Vec<LineBox>,
    pub width: f32,
    pub height: f32,
    /// 这一份用的是哪根推进轴（CSS 的 `writing-mode`，取**根样式**那一格）。
    ///
    /// 竖排时 `blocks` / `lines` 里已经是**物理**坐标：[`assemble`](Self::assemble) 在折索引
    /// 之前把引擎排好的那份逻辑横排长条整体投影了一次（见 [`transposed`]）。所以
    /// `width` / `height` 说的也是物理那两条边，而竖排下他们对调过：`width` 是长条沿块轴的
    /// 总长（一整串列占多宽，切片就是按它切的），`height` 是一列能排多长。
    flow: WritingMode,
    /// 第 `i` 行属于 `blocks` 里哪一条；与 `lines` 等长，`u32::MAX` 表示没有归属
    /// （正常排版不会出现，只有手工拼的表才有）。用 `u32` 而不是 `usize`：
    /// 一万多行一章省下的是几十 KB 的整数倍。
    owner: Vec<u32>,
    /// 行盒是否**严格叠放**：沿推进轴上一条的底不超过下一条的顶（横排就是
    /// `lines[i].y + lines[i].height <= lines[i+1].y`，见 [`Self::flow_start`]）。
    ///
    /// 真排版一定满足（每个块按游标一行行往下推），但「一定」是靠假设，而二分错了是
    /// **静默答错**：屏幕上就是「点这一行选中的是别处」。所以这一格为假时，按推进轴的查询
    /// 全部退回线性扫 —— 慢一点但不会答错，也不报错（将来接负外边距 / 绝对定位那天，
    /// 症状会先在速度上出现，而不是先在读代码的人的脑子里）。
    stacked: bool,
}

/// 把引擎排好的那份**逻辑横排**长条整体投影成竖排的物理坐标。
///
/// **为什么是整体投影，而不是让 `collect` 直接按竖轴摆**：断行、行盒基线、shaping、
/// 命中用的 `Measure::byte_at_x` 与 `char_edges`，全都只认「沿内联轴的一个纯量」，
/// 它们在逻辑空间里本来就对。真要转轴就得把 `x` / `y` 这两个名字在引擎里改一遍，
/// 还要在 `folio-app` 那三处读 `glyph.position` 的地方各分一个叉。投影只在这里发生一次。
///
/// **代价要说清，这一条不是免费的**：盒模型那几族边是**物理**的
/// （`margin-top` / `padding-left` / `border[上右下左]`），转置会把它们摆到另一条边上。
/// CSS Writing Modes 那张 logical→physical 映射表还没接，所以竖排里
/// 「段落之间的上下留白」会变成「列与列之间的留白」，而作者写给**行**的 `margin-bottom`
/// 会横过来。段落分开的那个方向在竖排里恰好就是列间，所以多数中文竖排本看着是对的；
/// 一条式子内缩进的物理边（如表格、`hr`）会错到另一侧去。显式记进降级清单，别当它不存在。
fn transposed(
    blocks: Vec<BlockBox>,
    mut lines: Vec<LineBox>,
    width: f32,
    height: f32,
    mode: WritingMode,
) -> (Vec<BlockBox>, Vec<LineBox>, f32, f32) {
    // 长条沿块轴的总长 = 逻辑高；一列的长 = 逻辑宽。物理那两条边因此互换。
    let strip = height;
    // `vertical-rl` 的列从右往左堆：块轴反着走，所以起点要从右缘翻面数。
    let flip = mode == WritingMode::VerticalRl;
    // 逻辑 (x=沿字, y=沿列堆, w=沿字长, h=列厚) → 物理那四格。
    // `baseline` 不动：它存的始终是「沿块轴从**起点边**算进去的深度」，
    // 起点边在 rl 是列的右缘、在 lr 是左缘 —— 落地成物理 x 是 [`Layout::cross_baseline`] 的事。
    let map = |x: f32, y: f32, w: f32, h: f32| -> (f32, f32, f32, f32) {
        (if flip { strip - (y + h) } else { y }, x, h, w)
    };
    for line in &mut lines {
        let (x, y, w, h) = map(line.x, line.y, line.width, line.height);
        line.x = x;
        line.y = y;
        line.width = w;
        line.height = h;
    }
    let blocks = blocks
        .into_iter()
        .map(|mut b| {
            let (x, y, w, h) = map(b.x, b.y, b.width, b.height);
            b.x = x;
            b.y = y;
            b.width = w;
            b.height = h;
            b
        })
        .collect();
    (blocks, lines, strip, width)
}

/// 同一张塌陷表里挨着我某一侧的邻格，`side` 按 `[上, 右, 下, 左]`。
///
/// 判据是**几何**的，不是 DOM 父子：`decor_seeds` 手上只有块表，而格子的边框盒在排版期
/// 已按列宽、行高摆好 ——「右边那一格」就是 `x` 恰落在我 `x + width` 上、且与我同一行的那块。
/// 用几何而不是 DOM，块缓存命中的那条路也对：缓存里存的就是块表。
fn neighbour_cell<'a>(
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

impl Layout {
    /// 组装排版结果：竖排先把两张表整体投影成物理坐标，然后折「行 → 块」那张表，
    /// 并量一次沿推进轴的叠放性。
    ///
    /// 排版各条路径（[`layout`](super::layout)、轻量量高）都从这里出去，所以索引不可能漏折。
    /// 公开它是有意的：**手工构造 `Layout` 只能走这一句**，不能拼结构体字面量 ——
    /// 索引是私有的，能拼出「有行盒但没有归属表」的排版结果就等于留了一条
    /// 「测试里过、生产里 `block_of_line` 全给 `None`」的路。测试要造表就调这个函数。
    ///
    /// `width` / `height` 传的是**逻辑**那两条边（折行宽与整章长），竖排下返回值里对调。
    pub fn assemble(
        blocks: Vec<BlockBox>,
        lines: Vec<LineBox>,
        width: f32,
        height: f32,
        flow: WritingMode,
    ) -> Self {
        let (blocks, lines, width, height) = if flow == WritingMode::HorizontalTb {
            (blocks, lines, width, height)
        } else {
            transposed(blocks, lines, width, height, flow)
        };
        let owner = fold_line_owner(&blocks, lines.len());
        let mut out = Self {
            blocks,
            lines,
            width,
            height,
            flow,
            owner,
            stacked: false,
        };
        // 叠放性最后才折：那条判据问的是「沿推进轴的底边」，而那两个入口是 `Layout` 的
        // 方法（竖排的起点要按长条总长翻面，那份信息在 `self` 上）。
        // 横排下这一句与从前那句 `pair[0].y + pair[0].height <= pair[1].y` 逐字同一个式子。
        out.stacked = out
            .lines
            .windows(2)
            .all(|pair| out.flow_end(&pair[0]) <= out.flow_start(&pair[1]));
        out
    }

    // ---- 推进轴上的问句 ----
    //
    // `stacked`、`line_at`、`line_covering_or_above`、`lines_in_band` 和分片器的 `fits`
    // 问的其实是同一件事：「沿**推进轴**走到哪儿了」。横排里那根轴就是 `y`；
    // `writing-mode: vertical-rl` 把字改成沿 `+y` 推进、一列一列沿 `−x` 堆，于是那根轴换成
    // `x`，而且是**反着**走的 —— 阅读顺序上的第一列摆在最右边，它的物理 `x` 最大。
    // 下面那两个 match 就是那一根轴的全部；调用点一个都不用改，因为它们早就只在问
    // 「沿推进轴的起点 / 底边」了。
    //
    // 长条沿块轴的总长就是 `Layout::width`（[`transposed`] 已经把 `width` / `height` 对调过），
    // 所以 `vertical-rl` 那一条不用额外的格子。**但它必须先过 `assemble`**：一张没投影过的
    // 表配上这两条竖排分支，症状是「横排的字按错的坐标被裁掉」—— 那正是把轴拆成两刀接的
    // 坏样子，所以这两半是同一刀。

    /// 这一行在推进轴上的起点。横排 = `line.y`；竖排 = 从阅读顺序那一侧数进来的距离。
    pub fn flow_start(&self, line: &LineBox) -> f32 {
        match self.flow {
            WritingMode::HorizontalTb => line.y,
            WritingMode::VerticalRl => self.width - (line.x + line.width),
            WritingMode::VerticalLr => line.x,
        }
    }

    /// 这一行沿推进轴占多厚。横排 = 行高（一行一行往下堆的那一格）；竖排 = 一列的宽。
    pub fn flow_thickness(&self, line: &LineBox) -> f32 {
        match self.flow {
            WritingMode::HorizontalTb => line.height,
            WritingMode::VerticalRl | WritingMode::VerticalLr => line.width,
        }
    }

    /// 这一行在推进轴上的底边。命中与分片比的都是它，写成一句免得两处各折一次。
    pub fn flow_end(&self, line: &LineBox) -> f32 {
        self.flow_start(line) + self.flow_thickness(line)
    }

    // ---- 内联轴 / 侧轴上的投影 ----
    //
    // 界面那三层（绘制、命中、选区）手里的数从来只有两个纯量：「沿字走了多远」与
    // 「在侧轴上落在哪儿」。横排里那两个纯量分别是 x 与 y，竖排里对调 —— 下面这几句
    // 就是对调本身，也是**整个界面层唯一**该知道轴的地方。
    //
    // `measure.rs` 里那三处读 `glyph.position` 的一格都不用改：shaping 永远发生在
    // 逻辑横排里，所以 `position.x` 永远是推进量、`position.y` 永远是离基线的深度，
    // 只有把它们落成物理点的时候才需要问这里。

    /// 这一份排版用的是竖排那根轴吗。
    pub fn vertical(&self) -> bool {
        self.flow.is_vertical()
    }

    /// 一行在**内联轴**上的起点：横排 = 行带的左缘，竖排 = 这一列的列顶。
    pub fn inline_start(&self, line: &LineBox) -> f32 {
        match self.flow {
            WritingMode::HorizontalTb => line.x,
            WritingMode::VerticalRl | WritingMode::VerticalLr => line.y,
        }
    }

    /// 一行的基线在**侧轴**上的位置：横排 = 那条横线的 `y`，竖排 = 那条竖线的 `x`。
    ///
    /// `LineBox::baseline` 存的从头到尾是「沿块轴从**起点边**算进去的深度」——
    /// 起点边在横排是行顶，在 `vertical-rl` 是列的**右**缘，在 `vertical-lr` 是左缘。
    /// 那一个数落成物理坐标只在这一句发生一次，绘制与命中都从它起算。
    pub fn cross_baseline(&self, line: &LineBox) -> f32 {
        match self.flow {
            WritingMode::HorizontalTb => line.y + line.baseline,
            WritingMode::VerticalRl => line.x + line.width - line.baseline,
            WritingMode::VerticalLr => line.x + line.baseline,
        }
    }

    /// 从基线沿块轴走 `depth`（就是 `glyph.position.y` 与 `baseline_shift` 那一格，
    /// 正数在横排是往下）之后，落在侧轴上的物理位置。
    ///
    /// `vertical-rl` 的块轴反着走，所以同一个正数在竖排里往**左** —— 那一档的符号写在
    /// [`WritingMode::project_delta`]，这里只挑出侧轴那一个分量，两处不再各折一遍。
    /// 这一句存在的全部理由就是那个符号：画在基线右边的字在竖排里会跑到基线左边去，
    /// 而两处各自看起来都「合理」。
    pub fn cross_from_baseline(&self, line: &LineBox, depth: f32) -> f32 {
        let (dx, dy) = self.flow.project_delta(0.0, depth);
        // 侧轴是哪一根物理轴，与 `project` 那一句是同一件事：横排的侧轴是 y，竖排的是 x。
        self.cross_baseline(line) + if self.vertical() { dx } else { dy }
    }

    /// 把「内联轴上 `inline`、侧轴上 `cross`」折成物理的 `(x, y)`。
    /// 规则只有 [`WritingMode::project`] 那一处，这里只是拿着这一份排版的轴问一句。
    pub fn project(&self, inline: f32, cross: f32) -> (f32, f32) {
        self.flow.project(inline, cross)
    }

    /// 这一份排版用的轴。界面那几层把它**存一份**在自己的行元素上，
    /// 逐字形与装饰线那一趟才知道自己在问谁 —— 规则只有这一处，界面不许各自再折一遍。
    pub fn flow_mode(&self) -> WritingMode {
        self.flow
    }

    /// 把「沿字走了 `along`、沿块轴从基线走了 `depth`（正数 = 横排往下）」这两个**增量**
    /// 折成物理的 `(dx, dy)`。
    ///
    /// 这一句与 [`Self::project`](Self::project) 的分别是：`project` 折的是已经落在侧轴上的
    /// 位置，而这里折的是**还没落地的增量**，所以带符号。规则只有
    /// [`WritingMode::project_delta`] 那一处。
    pub fn project_delta(&self, along: f32, depth: f32) -> (f32, f32) {
        self.flow.project_delta(along, depth)
    }

    /// 行盒是否严格叠放（见 `stacked` 那一格）。测试与诊断读它：
    /// 真排版为假就说明排版层放出了互相压住的行，那是新的几何形态在要求索引换算法。
    pub fn stacked(&self) -> bool {
        self.stacked
    }

    /// 命中测试：推进轴上的位置 → 行盒下标（横排下它就是纵坐标）。选择与批注回显从这里起步。
    ///
    /// 口径是「**第一条**覆盖这个像素的行」：`at` 落在两块之间的留白里时没有人覆盖它，
    /// 给 `None`（要「上方最近那一行」的场合用 [`line_covering_or_above`](Self::line_covering_or_above)）。
    pub fn line_at(&self, at: f32) -> Option<usize> {
        if !self.stacked {
            return self
                .lines
                .iter()
                .position(|l| at >= self.flow_start(l) && at < self.flow_end(l));
        }
        // 二分定位到「行顶不超过 y」的最后一行，再往回退过同一 y 上的并列行（正常排版没有）。
        let end = self.lines.partition_point(|l| self.flow_start(l) <= at);
        let mut found = None;
        let mut i = end;
        while i > 0 {
            i -= 1;
            let line = &self.lines[i];
            if self.flow_end(line) > at {
                found = Some(i);
            } else if self.flow_start(line) < at {
                break; // 已经严格在上方且没覆盖到，再往前只会更低
            }
        }
        found
    }

    /// 覆盖 `at`（推进轴上的位置）的那一行；没有覆盖者（块间留白、或超出末尾）给**上方最近**
    /// 的那一行，一行都没有时给 0。
    ///
    /// 滚动位置 → 行号走这一句：读者停在最后一屏要回到最后一屏，而不是回到「章尾但
    /// 看不见任何字」的地方。
    pub fn line_covering_or_above(&self, at: f32) -> usize {
        if self.lines.is_empty() {
            return 0;
        }
        if !self.stacked {
            let mut line = 0usize;
            for (ix, box_) in self.lines.iter().enumerate() {
                if self.flow_end(box_) > at {
                    return ix;
                }
                line = ix;
            }
            return line;
        }
        // 底边越过 `at` 的第一行。底边在叠放前提下随表序单调，所以能二分。
        let i = self.lines.partition_point(|l| self.flow_end(l) <= at);
        i.min(self.lines.len() - 1)
    }

    /// 真正拥有第 `i` 行的盒子：先序入表 ⇒ 最后一个区间包含它的就是最深的叶子块。
    ///
    /// 答案是排完时折好的那张表（[`fold_line_owner`]），所以这一句是 O(1)。
    /// 折表那一步与原来那句 `blocks.iter().rev().find(|b| b.lines.contains(&i))`
    /// 是同一条规则：按表序逐个写入，最后留在格里的就是最深的块。
    pub fn block_of_line(&self, i: usize) -> Option<&BlockBox> {
        let at = *self.owner.get(i)?;
        if at == u32::MAX {
            return None;
        }
        self.blocks.get(at as usize)
    }

    /// 与可见带 `[from_y, to_y)` 有关的那些行 —— **绘制那一帧只该问这个数**。
    ///
    /// 原来的写法是把 `0..行数` 整个交给场景层，每行一次 y 比较裁剔（`plans/阅读管线.md`
    /// §十一 第 4 条），于是每帧 O(章行数)；这里两次二分。带的起点包含**顶边在带上方、
    /// 但盒子伸进来**的那一行，否则窗口最上面会缺一截。
    ///
    /// 行盒不叠放（`stacked` 为假）或带是空的时给全区间：调用方自己的 y 裁剔照旧兜住
    /// 正确性，这里只是没省下来。
    /// 把整章块表走一遍，折出「哪些块要画装饰」那张短表（见 [`DecorSeed`]）。
    ///
    /// **每次重排一次，不是每帧一次** —— 折一次是 O(全部块)，而它的答案只跟层叠结果
    /// 有关。调用点是 `reader::document::Document::apply_typeset`，门是
    /// `Metric::DecorRebuilds`（滚动过程中那一格必须不涨）。
    ///
    /// 三条筛掉的理由与绘制侧原本逐条一致，一条都没松：
    /// - **内容盒副本不画**：`wrap_paragraph` 为正文映射补注册的那条几何内缩了边框与
    ///   padding，它的边框条会不偏不倚落在文字起点上（真书《斗战狂潮》目录页踩过）；
    /// - 没有底色、四条边又都不可见、背景里又没有图的块，进表只是让每帧多扫一条空记录；
    /// - 零宽零高的块画不出任何东西，进表同样只是白扫。
    ///
    /// 第三条对新加的图那一格**同样是筛**：`width`/`height` 是边框盒的数，一块零高的盒
    /// 摆不出任何图，让扉页那种「整块是一个 `<body>`，图按盒铺」走同一道筛就够，
    /// 不需要为图另开一条例外。
    pub fn decor_seeds(&self, styles: &StyledDom) -> Vec<DecorSeed> {
        Metric::DecorRebuilds.count();
        let mut out = Vec::new();
        for (index, block) in self.blocks.iter().enumerate() {
            if !block.border_box || block.width <= 0.0 || block.height <= 0.0 {
                continue;
            }
            let Some(style) = styles.get(block.node) else {
                continue;
            };
            // `drawn()` 已经把「有 width 但 style 是 none」那种不存在的边折成 0。
            let mut drawn = [
                style.border[0].drawn(),
                style.border[1].drawn(),
                style.border[2].drawn(),
                style.border[3].drawn(),
            ];
            // 合并边框（`border-collapse: collapse`）：同一张塌陷表里相邻两格的公共边
            // 只画一条 —— 逐边与邻格**对面那一侧**比一次，赢的留、输的抹。
            // 几何不动：格子仍并排，那条线落在赢的那一格的边内侧（半个像素的偏移，
            // 肉眼分不出）。判据与优先级在 [`super::table::collapse_side_wins`]。
            if style.border_collapse && style.display == Display::TableCell {
                const OPPOSITE: [usize; 4] = [2, 3, 0, 1];
                // 打平算谁赢：右/下那一侧站着的人赢（CSS 里颜色取靠左 / 靠上的那格）。
                const I_WIN_TIES: [bool; 4] = [false, true, true, false];
                for side in 0..4 {
                    if drawn[side] <= 0.0 {
                        continue;
                    }
                    let Some(other) = neighbour_cell(&self.blocks, styles, index, side) else {
                        continue;
                    };
                    let theirs = (
                        other.border[OPPOSITE[side]].drawn(),
                        other.border[OPPOSITE[side]].style,
                    );
                    let mine = (drawn[side], style.border[side].style);
                    if !super::table::collapse_side_wins(mine, theirs, I_WIN_TIES[side]) {
                        drawn[side] = 0.0;
                    }
                }
            }
            let pictures: Vec<DecorPicture> = style
                .background_pictures()
                .map(|layer| DecorPicture {
                    href: layer.image.clone().unwrap_or_default(),
                    size: layer.size,
                    position: layer.position,
                    repeat: layer.repeat,
                })
                .collect();
            if style.background_color.is_none()
                && drawn.iter().all(|w| *w <= 0.0)
                && pictures.is_empty()
            {
                continue;
            }
            out.push(DecorSeed {
                x: block.x,
                y: block.y,
                width: block.width,
                height: block.height,
                background: style.background_color,
                radius: style.corner_radii(block.width, block.height),
                border: [
                    (
                        drawn[0],
                        style.border[0].color.or(style.color),
                        style.border[0].style,
                    ),
                    (
                        drawn[1],
                        style.border[1].color.or(style.color),
                        style.border[1].style,
                    ),
                    (
                        drawn[2],
                        style.border[2].color.or(style.color),
                        style.border[2].style,
                    ),
                    (
                        drawn[3],
                        style.border[3].color.or(style.color),
                        style.border[3].style,
                    ),
                ],
                pictures,
            });
        }
        out
    }

    /// 与可见带 `[from, to)` 有关的那些行 —— **绘制那一帧只该问这个数**。
    /// 带是推进轴上的一段（横排就是 `[上缘, 下缘)`）。
    ///
    /// 原来的写法是把 `0..行数` 整个交给场景层，每行一次 y 比较裁剔（`plans/阅读管线.md`
    /// §十一 第 4 条），于是每帧 O(章行数)；这里两次二分。带的起点包含**顶边在带上方、
    /// 但盒子伸进来**的那一行，否则窗口最上面会缺一截。
    ///
    /// 行盒不叠放（`stacked` 为假）或带是空的时给全区间：调用方自己的 y 裁剔照旧兜住
    /// 正确性，这里只是没省下来。
    pub fn lines_in_band(&self, from: f32, to: f32) -> Range<usize> {
        let all = 0..self.lines.len();
        if !self.stacked || to <= from {
            return all;
        }
        let start = self.lines.partition_point(|l| self.flow_end(l) <= from);
        let end = self.lines.partition_point(|l| self.flow_start(l) < to);
        if end <= start {
            return start..start;
        }
        start..end
    }

    /// 第 `i` 行的可见文本。图片片段不产出字符，行内图位置上的哨兵字符会被去掉。
    pub fn line_text(&self, i: usize) -> String {
        let (Some(line), Some(block)) = (self.lines.get(i), self.block_of_line(i)) else {
            return String::new();
        };
        let Some(first) = line.text_runs().next() else {
            return String::new();
        };
        let start = first.norm.start;
        let end = line.text_runs().last().map(|r| r.norm.end).unwrap_or(start);
        block
            .text
            .get(start..end)
            .unwrap_or("")
            .replace(IMAGE_GLYPH, "")
    }
}

/// 「第几行属于哪一条块盒」那张表。见 [`Layout::block_of_line`]。
fn fold_line_owner(blocks: &[BlockBox], line_count: usize) -> Vec<u32> {
    let mut owner = vec![u32::MAX; line_count];
    for (ix, block) in blocks.iter().enumerate() {
        // 块数超过 `u32` 上限在真实书里不可能；真到了就把后面那些留成「没有归属」，
        // 于是 `block_of_line` 给 `None`（少画一处底色），而不是把行号指到别处。
        let Ok(at) = u32::try_from(ix) else { break };
        for line in block.lines.clone() {
            if let Some(slot) = owner.get_mut(line) {
                *slot = at;
            }
        }
    }
    owner
}

/// 索引口径的**参照实现**：逐字照抄接线前那三句线性扫，用来在测试里证明
/// 「折表与二分给出的答案，和当年那条慢规则一个不差」。
///
/// 只给测试用（`#[cfg(test)]`），不属于对外公开面 —— 生产代码不该有一条能选慢的入口。
#[cfg(test)]
pub(super) mod scan_reference {
    use super::Layout;

    /// 接线前的 `block_of_line`：先序表倒着扫，最后一个区间包含 `i` 的就是叶子块。
    pub fn block_of_line(layout: &Layout, i: usize) -> Option<usize> {
        layout.blocks.iter().rposition(|b| b.lines.contains(&i))
    }

    /// 接线前的 `line_at`：整表扫，第一条覆盖这个像素的行。
    pub fn line_at(layout: &Layout, y: f32) -> Option<usize> {
        layout
            .lines
            .iter()
            .position(|l| y >= l.y && y < l.y + l.height)
    }

    /// 接线前的 `line_at_y`（`anchor::line_at_y`）：整表扫，没有覆盖者给最后一行。
    pub fn line_covering_or_above(layout: &Layout, y: f32) -> usize {
        let mut line = 0usize;
        for (ix, box_) in layout.lines.iter().enumerate() {
            if box_.y + box_.height > y {
                return ix;
            }
            line = ix;
        }
        line
    }

    /// 接线前的可见带：整表扫，凡与 `[from, to)` 有交集都算。
    pub fn band(layout: &Layout, from: f32, to: f32) -> Vec<usize> {
        layout
            .lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.y + l.height > from && l.y < to)
            .map(|(ix, _)| ix)
            .collect()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LayoutOptions {
    /// 内容区宽度（页面左右边距已扣）。竖排时这一格是**一列能排多长**（= 视口高），
    /// 因为折行永远发生在内联轴上 —— 名字里的 width 说的是「沿字的那一条」，不是屏幕的宽。
    pub width: f32,
    /// 轻量模式：只算块高，不产出行盒。
    ///
    /// 用于目录跳转、进度条等只需要「这一章多高」的场合，不需要行盒级别的细节。
    /// 跳过断行、跳过 `run_metrics`、跳过 `LineFragment` 构建，速度提升一个量级。
    pub light: bool,
    /// 允许按书自己声明的那根轴排吗。`false` = 不管 CSS 写了什么竖排都按横排排。
    ///
    /// 这一格由**界面**给，不是引擎的取舍：竖排现在只在分页那一档落地。连续滚动整条
    /// 滚动链（跨章拼接、全局进度、屏幕↔章内坐标那一句反算）全按「沿 y 走」建着，
    /// 转轴等于把那条 spine 重做一遍 —— 那一档接上之前，宁可让一本竖排书横着排，
    /// 也不要在滚动里把列往 y 上叠。判据与症状都记在
    /// `ui::features::reader::model::chapter_layout` 的 `vertical` 那一格上。
    pub allow_vertical: bool,
    /// 竖轴上「包含块的高」这一格 —— `height: NN%` 那一族的折算基准。
    ///
    /// `None` = 没有既定高，百分比高整族按 `auto`。这一档就是它出现之前的一切行为，
    /// 也是竖排那一档现在唯一的取值（转轴后该拿屏幕的**宽**，而那个数还没走到这一层）。
    ///
    /// 为什么要它：`html,body { height:100% }` + `.wedge { float:left; height:50%;
    /// margin-bottom:-360px }` + `.container { clear:both; height:0 }` 是把封面图**垂直
    /// 居中**的老写法（EasyPub 那族工具给封面页就写这套，《斗战狂潮》是现成样本）。
    /// 它的居中不靠表格盒，靠的是那个浮动块拿到「页高的一半」这个数；没有基准就折不出
    /// 那一半，整条链退化成 auto，图贴在顶上、下面空一整页。
    ///
    /// 谁递这个数：分页档递**一片的高**，滚动档递**视口高** —— 两个都对应浏览器里
    /// `body` 的包含块（打印时是页盒，屏幕上是视口），不是新造的一档取舍。
    pub page_height: Option<f32>,
}

impl LayoutOptions {
    /// 完整排版（默认）。
    pub const FULL: Self = Self {
        width: 0.0,
        light: false,
        allow_vertical: false,
        page_height: None,
    };

    /// 轻量排版（只算高）。
    pub const LIGHT: Self = Self {
        width: 0.0,
        light: true,
        allow_vertical: false,
        page_height: None,
    };

    /// 指定宽度的完整排版。
    pub fn with_width(width: f32) -> Self {
        Self {
            width,
            light: false,
            allow_vertical: false,
            page_height: None,
        }
    }
}
