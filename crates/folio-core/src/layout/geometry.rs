//! 排版结果的数据形状：run、行片段、行盒、块盒、`Layout` 与其查询，加上 `LayoutOptions`。
//!
//! 从 `layout` 主模块切出来只为了一个文件能一次读完；跨文件的条目用
//! `pub(super)`，即**只对 `layout` 内部开放**，不属于对外公开面。

use std::ops::Range;

use super::collect::IMAGE_GLYPH;
use super::font::RunStyle;
use crate::content::dom::NodeId;
use crate::content::format::css::computed::{BorderStyle, Color, StyledDom, TextAlign};
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
#[derive(Debug, Clone, Copy, PartialEq)]
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
    /// 画法只可能是 `Solid` / `Double`（[`BorderEdge::drawn`] 已经把 `none` 折成 0
    /// 的边挡在外面）。`Double` 必须带出去：绘制层要把它拆成两条 1/3 细线，
    /// 丢了画法就退化成一条实线（真书《人类简史》的章名 `.biaoti` 上下双线）。
    ///
    /// [`BorderEdge::drawn`]: crate::content::format::css::computed::BorderEdge::drawn
    pub border: [(f32, Option<Color>, BorderStyle); 4],
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
    /// 第 `i` 行属于 `blocks` 里哪一条；与 `lines` 等长，`u32::MAX` 表示没有归属
    /// （正常排版不会出现，只有手工拼的表才有）。用 `u32` 而不是 `usize`：
    /// 一万多行一章省下的是几十 KB 的整数倍。
    owner: Vec<u32>,
    /// 行盒是否**严格叠放**：`lines[i].y + lines[i].height <= lines[i+1].y`。
    ///
    /// 真排版一定满足（每个块按游标一行行往下推），但「一定」是靠假设，而二分错了是
    /// **静默答错**：屏幕上就是「点这一行选中的是别处」。所以这一格为假时，按 y 的查询
    /// 全部退回线性扫 —— 慢一点但不会答错，也不报错（将来接负外边距 / 绝对定位那天，
    /// 症状会先在速度上出现，而不是先在读代码的人的脑子里）。
    stacked: bool,
}

impl Layout {
    /// 组装排版结果：折「行 → 块」那张表，并量一次叠放性。
    ///
    /// 排版各条路径（[`layout`](super::layout)、两阶段的
    /// [`render_window`](super::render_window)）都从这里出去，所以索引不可能漏折。
    /// 公开它是有意的：**手工构造 `Layout` 只能走这一句**，不能拼结构体字面量 ——
    /// 索引是私有的，能拼出「有行盒但没有归属表」的排版结果就等于留了一条
    /// 「测试里过、生产里 `block_of_line` 全给 `None`」的路。测试要造表就调这个函数。
    pub fn assemble(blocks: Vec<BlockBox>, lines: Vec<LineBox>, width: f32, height: f32) -> Self {
        let owner = fold_line_owner(&blocks, lines.len());
        let stacked = lines
            .windows(2)
            .all(|pair| pair[0].y + pair[0].height <= pair[1].y);
        Self {
            blocks,
            lines,
            width,
            height,
            owner,
            stacked,
        }
    }

    /// 行盒是否严格叠放（见 `stacked` 那一格）。测试与诊断读它：
    /// 真排版为假就说明排版层放出了互相压住的行，那是新的几何形态在要求索引换算法。
    pub fn stacked(&self) -> bool {
        self.stacked
    }

    /// 命中测试：纵坐标 → 行盒下标。选择与批注回显从这里起步。
    ///
    /// 口径是「**第一条**覆盖这个像素的行」：`y` 落在两块之间的留白里时没有人覆盖它，
    /// 给 `None`（要「上方最近那一行」的场合用 [`line_covering_or_above`](Self::line_covering_or_above)）。
    pub fn line_at(&self, y: f32) -> Option<usize> {
        if !self.stacked {
            return self
                .lines
                .iter()
                .position(|l| y >= l.y && y < l.y + l.height);
        }
        // 二分定位到「行顶不超过 y」的最后一行，再往回退过同一 y 上的并列行（正常排版没有）。
        let end = self.lines.partition_point(|l| l.y <= y);
        let mut found = None;
        let mut at = end;
        while at > 0 {
            at -= 1;
            let line = &self.lines[at];
            if line.y + line.height > y {
                found = Some(at);
            } else if line.y < y {
                break; // 已经严格在上方且没覆盖到，再往前只会更低
            }
        }
        found
    }

    /// 覆盖 `y` 的那一行；没有覆盖者（块间留白、或超出末尾）给**上方最近**的那一行，
    /// 一行都没有时给 0。
    ///
    /// 滚动位置 → 行号走这一句：读者停在最后一屏要回到最后一屏，而不是回到「章尾但
    /// 看不见任何字」的地方。
    pub fn line_covering_or_above(&self, y: f32) -> usize {
        if self.lines.is_empty() {
            return 0;
        }
        if !self.stacked {
            let mut line = 0usize;
            for (ix, box_) in self.lines.iter().enumerate() {
                if box_.y + box_.height > y {
                    return ix;
                }
                line = ix;
            }
            return line;
        }
        // 底边越过 `y` 的第一行。底边在叠放前提下随表序单调，所以能二分。
        let at = self.lines.partition_point(|l| l.y + l.height <= y);
        at.min(self.lines.len() - 1)
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
    /// - 没有底色、四条边又都不可见的块，进表只是让每帧多扫一条空记录；
    /// - 零宽零高的块画不出任何东西，进表同样只是白扫。
    pub fn decor_seeds(&self, styles: &StyledDom) -> Vec<DecorSeed> {
        Metric::DecorRebuilds.count();
        let mut out = Vec::new();
        for block in &self.blocks {
            if !block.border_box || block.width <= 0.0 || block.height <= 0.0 {
                continue;
            }
            let Some(style) = styles.get(block.node) else {
                continue;
            };
            // `drawn()` 已经把「有 width 但 style 是 none」那种不存在的边折成 0。
            let drawn = [
                style.border[0].drawn(),
                style.border[1].drawn(),
                style.border[2].drawn(),
                style.border[3].drawn(),
            ];
            if style.background_color.is_none() && drawn.iter().all(|w| *w <= 0.0) {
                continue;
            }
            out.push(DecorSeed {
                x: block.x,
                y: block.y,
                width: block.width,
                height: block.height,
                background: style.background_color,
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
            });
        }
        out
    }

    pub fn lines_in_band(&self, from_y: f32, to_y: f32) -> Range<usize> {
        let all = 0..self.lines.len();
        if !self.stacked || to_y <= from_y {
            return all;
        }
        let start = self.lines.partition_point(|l| l.y + l.height <= from_y);
        let end = self.lines.partition_point(|l| l.y < to_y);
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
    /// 内容区宽度（页面左右边距已扣）。
    pub width: f32,
    /// 轻量模式：只算块高，不产出行盒。
    ///
    /// 用于目录跳转、进度条等只需要「这一章多高」的场合，不需要行盒级别的细节。
    /// 跳过断行、跳过 `run_metrics`、跳过 `LineFragment` 构建，速度提升一个量级。
    pub light: bool,
}

impl LayoutOptions {
    /// 完整排版（默认）。
    pub const FULL: Self = Self {
        width: 0.0,
        light: false,
    };

    /// 轻量排版（只算高）。
    pub const LIGHT: Self = Self {
        width: 0.0,
        light: true,
    };

    /// 指定宽度的完整排版。
    pub fn with_width(width: f32) -> Self {
        Self {
            width,
            light: false,
        }
    }
}
