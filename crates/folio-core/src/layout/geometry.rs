//! 排版结果的数据形状：run、行片段、行盒、块盒、`Layout` 与其查询，加上 `LayoutOptions`。
//!
//! 从 `layout` 主模块切出来只为了一个文件能一次读完；跨文件的条目用
//! `pub(super)`，即**只对 `layout` 内部开放**，不属于对外公开面。

use std::ops::Range;

use super::collect::IMAGE_GLYPH;
use super::font::RunStyle;
use crate::content::dom::NodeId;
use crate::content::format::css::computed::TextAlign;

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
}

/// 行盒里的一段内容：文本或替换元素（图片）。M6 的 ruby 会加第三个分支。
#[derive(Debug, Clone, PartialEq)]
pub enum LineFragment {
    Text(LineRun),
    Image(ImageRun),
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
}

impl LineBox {
    /// 文本片段，按绘制顺序。
    pub fn text_runs(&self) -> impl Iterator<Item = &LineRun> {
        self.fragments.iter().filter_map(|f| match f {
            LineFragment::Text(r) => Some(r),
            LineFragment::Image(_) => None,
        })
    }

    /// 图片片段，按绘制顺序。
    pub fn images(&self) -> impl Iterator<Item = &ImageRun> {
        self.fragments.iter().filter_map(|f| match f {
            LineFragment::Image(r) => Some(r),
            LineFragment::Text(_) => None,
        })
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
