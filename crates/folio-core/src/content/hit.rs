//! 引擎查询接口：把「屏幕上的一个位置」换算成「第几个字」，再把「一段字」换算回矩形。
//!
//! 这是 KOReader 的 `getWordFromPosition` / `getScreenBoxesFromPositions` 在 folio 的
//! 对应物。选中（M6）与批注回显（M7）都建立在这一层上 —— UI 只画矩形、弹菜单，
//! 字形知识全部留在引擎里，两端也就只有一套命中口径。
//!
//! 与绘制的同源性：字符前进量与排版走的是**同一个** [`Measure`] 实例
//! （`RunStyle::advance` 就是排版折行用的那一个），不会出现「按 A 算宽、按 B 命中」的错位。
//!
//! 坐标是**排版坐标**（一章的绝对坐标，行盒 `y` 那一套），不是屏幕坐标：
//! 连续滚动时调用方先减掉 `scroll_y`，分页/双页时减掉片的 `origin_y` 与栏位 `x`。
//! 返回的矩形同样是排版坐标，绘制侧照 `place_lines` 的同一条换算搬上屏。
//!
//! 进来的 `(x, y)` 与出去的矩形都是**物理**的，只有函数内部沿字走的那一段是纯量 ——
//! 竖排下这一进一出各换一次轴（`Layout::inline_start` / `vertical`），
//! 所以调用方不需要知道轴，也不需要自己翻面。

use std::ops::Range;

use crate::content::anchor::{TextMap, line_at_y, line_chars, line_offset};
use crate::content::dom::{Dom, NodeId};
use crate::infra::metrics::Metric;
use crate::layout::{Layout, LineFragment, Measure};

/// 命中结果：行盒下标 + 覆盖该位置的字符偏移。
///
/// 行下标不是装饰：拖动扩展要跟着「活动端落在哪一行」更新，进度条与跳转也读它。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub line: usize,
    pub offset: usize,
}

/// 一段选中在**一行**里画出来的矩形（排版坐标）。
///
/// 跨行的选中每行一条 —— 选中矩形天然按行断开，与浏览器一致。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelectionRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// 屏幕位置 → 字符偏移。
///
/// 纵向夹到最近的行（`line_at_y` 语义：点在两行之间、章首之前、章尾之后都停最近一行），
/// 横向按字符**前进量的中点**判归属 —— 点在字的右半边算下一个字之前，与文本编辑器一致。
///
/// 返回 `None` 的情形只有一种：命中行没有任何可见文本（纯图片行）。
/// 点在行尾之外给「最后一个字之后」的插入点，点在行首之外给行首 —— 拖动扩展靠它们收边。
pub fn hit_test(
    layout: &Layout,
    dom: &Dom,
    map: &TextMap,
    measure: &dyn Measure,
    x: f32,
    y: f32,
) -> Option<Hit> {
    if layout.lines.is_empty() {
        return None;
    }
    // 物理那两个数先折成「沿推进轴」与「沿字」：横排是 (y, x)，竖排整根轴换了方向 ——
    // 点在屏上的左边那一条，是竖排里**更晚**的一列（列从右往左堆），所以推进轴那一格
    // 要从右缘翻着数。规则只有 `Layout` 那份，这里不重算。
    let (along_flow, inline) = if layout.vertical() {
        (layout.width - x, y)
    } else {
        (y, x)
    };
    let line_index = line_at_y(layout, along_flow);
    let line = &layout.lines[line_index];
    // 这一行的文本边界。没有字符的行（纯图片 / 空行）给不出坐标。
    let line_range = line_chars(layout, dom, map, line_index)?;
    let block = layout.block_of_line(line_index)?;

    // 横向夹进这一行的墨迹范围内再逐段走。用 `ink_width`（两端对齐把行撑满，
    // 字真铺到了右边）而不是 `width`（未撑开的片段宽之和）。
    // 起点取**内联轴**上那一格的起点：横排是行带左缘 `line.x`，竖排是列顶 `line.y` ——
    // 下面一整趟走的都是「沿字走了多少」这一个纯量，只有落地时才分轴。
    let start = layout.inline_start(line);
    let inline = inline.clamp(start, start + line.ink_width());
    let mut cursor = start;
    // 行内「隙游标」：两端对齐按它算每个字的右推量（见 `Justify`）。不两端对齐时恒 0。
    let justify = line.justify;
    let mut gap_cursor = 0u32;
    // 「最后一个字之后」的落点：走到行尾也没命中时就停在这里。
    let mut end_offset = line_range.end;

    // run 的**起点**按排版的 run.width 累加 —— 绘制（`place_lines` → `paint_run`）
    // 就是这么摆的；run **内部**的字位优先问 shaping（`byte_at_x`，kerning / 字体
    // 回退都在里面），做不了 shaping 才退回 char_width 累加。三层同源，命中
    // 才贴得住画出来的字。**字距与两端对齐都会把字推开，而 shaping 的位置里没有它们** ——
    // 有任一时就不走 shaping 那条，落回逐字累加（advance 已含字距，间隙按同一句摊）。
    for fragment in &line.fragments {
        let LineFragment::Text(run) = fragment else {
            // 图片不产出字符，但**占横向位置**：游标必须推进它，后面的字才量得准。
            if let LineFragment::Image(image) = fragment {
                cursor += image.advance;
            }
            continue;
        };
        let text = block.text.get(run.norm.clone()).unwrap_or("");
        let run_x = cursor;
        // shaping 同源的 run 内定位（没有字距、也没有两端对齐时才成立）。
        if run.style.letter_spacing == 0.0
            && justify.is_none()
            && let Some(bytes) =
                measure.byte_at_x(text, &run.style.font, run.style.font_size, inline - run_x)
        {
            let offset = map
                .chars_of_bytes(dom, run.node, orig_at(run.orig.start, bytes))
                .map(|range| range.start)
                // 映射退化（空白折叠）时的兜底：行首。
                .unwrap_or(line_range.start);
            return Some(Hit {
                line: line_index,
                offset,
            });
        }
        // 退路：按字符前进量逐字走（中点判归属，中点本身归前一个字）。
        // 触点极少精确落在中点上，而测试要这个边界是确定的。
        // 像素按 advance 累加，字节单独记 —— 两个计数器用途不同，混用就全错位。
        let (mut pixel, mut bytes_before) = (0.0f32, 0usize);
        let mut gap = gap_cursor;
        for ch in text.chars() {
            let advance = run.style.advance(measure, ch);
            let shift = justify.map_or(0.0, |j| j.shift(gap));
            let char_x = run_x + pixel + shift;
            if inline < char_x + advance {
                let after = inline > char_x + advance * 0.5;
                let bytes = bytes_before + if after { ch.len_utf8() } else { 0 };
                let offset = map
                    .chars_of_bytes(dom, run.node, orig_at(run.orig.start, bytes))
                    .map(|range| range.start)
                    .unwrap_or(line_range.start);
                return Some(Hit {
                    line: line_index,
                    offset,
                });
            }
            pixel += advance;
            bytes_before += ch.len_utf8();
            gap += justify.map_or(0, |j| j.advance_cursor(ch));
        }
        gap_cursor = gap;
        // 这一整段都走完了还没命中：末落点推进到这段文本之后，游标推进整段宽。
        if let Some(range) = map.chars_of_bytes(dom, run.node, run.orig.clone()) {
            end_offset = range.end;
        }
        cursor += run.width;
    }
    Some(Hit {
        line: line_index,
        offset: end_offset,
    })
}

/// 长按落点所在**段落**的字符区间（整段选择用；阅读页的就地菜单目前不暴露它）。
///
/// 段落 = 拥有这一行的最深叶子块（`<p>`；`<em>` 这类行内盒不成块）。
/// 块的行区间是连续的，首行行首到末行行末就是整段。
pub fn paragraph_range(
    layout: &Layout,
    dom: &Dom,
    map: &TextMap,
    offset: usize,
) -> Option<Range<usize>> {
    let line = crate::content::anchor::line_at_offset(layout, dom, map, offset);
    let block = layout.block_of_line(line)?;
    let first = block.lines.start;
    let last = block.lines.end.saturating_sub(1);
    let start = line_offset(layout, dom, map, first)?;
    let end = line_chars(layout, dom, map, last)?.end;
    Some(start..end.max(start))
}

/// `from` 之后的第一段：起点严格晚于 `from.start` 的第一个段落区间。
///
/// 听书逐段推进用它（`folio_app` 的阅读会话每念完一段问一次）。段与段之间的空白
/// （pretty-print 的换行缩进也是文本节点）不进任何段区间，[`paragraph_range`] 对它
/// 回 `None`；段尾若带着空白，对 `from.end` 一问也可能回 `from` 自己 —— 两种都要
/// 跳过去，所以是向前走的循环而不是一问。
pub fn paragraph_after(
    layout: &Layout,
    dom: &Dom,
    map: &TextMap,
    from: Range<usize>,
) -> Option<Range<usize>> {
    let mut probe = from.end;
    while probe < map.chars() {
        match paragraph_range(layout, dom, map, probe) {
            Some(range) if range.start > from.start => return Some(range),
            Some(range) => probe = range.end.max(probe + 1),
            None => probe += 1,
        }
    }
    None
}

/// `orig` 区间起点 + run 内字节偏移 → node.text 里的绝对字节位置。
fn orig_at(orig_start: usize, bytes: usize) -> Range<usize> {
    let at = orig_start + bytes;
    at..at
}

/// 一段字符区间在排版里的矩形（排版坐标，每行一条），**整章的行都扫**的那一条。
///
/// 区间是**含头不含尾**（`TextAnchor` 的语义）。图片片段没有字符坐标，
/// 不产出矩形，与 `line_chars` 的口径一致。
///
/// 绘制侧走的是 [`rects_for_range_in`]（行号段由索引二分出来）；这一条留着是
/// **参考实现** —— 等价性门 [`crate::content::anchor::CharLines`] 就是拿它当基准比的，
/// 基准自己不该被优化掉。
pub fn rects_for_range(
    layout: &Layout,
    dom: &Dom,
    map: &TextMap,
    measure: &dyn Measure,
    range: Range<usize>,
) -> Vec<SelectionRect> {
    rects_for_range_in(layout, dom, map, measure, range, 0..layout.lines.len())
}

/// 只走 `lines` 那一段行号。批注回显与选区用它把「整章的行」换成「与选区**可能**相交的
/// 那几行」（`CharLines::lines_for` 二分出来，是一张索引折一次的成果）。
///
/// 为什么安全：下面那道「这一行与选区的交集为空就跳过」原样保留，所以窄化给的行号区间
/// 即使是超集也只多扫、不漏算；而它给的是**左闭右开的连续行号**，`CharLines` 只在
/// 「字符区间随行号不减」成立时才给（否则返回 `None`，调用方退回整表扫）。
/// 2026-10-15 之前这里是无条件 `0..layout.lines.len()`：一章 25 条批注实测每帧
/// +1.7–2.2 ms（`plans/阅读管线.md` §十一 第 9 条）。
pub fn rects_for_range_in(
    layout: &Layout,
    dom: &Dom,
    map: &TextMap,
    measure: &dyn Measure,
    range: Range<usize>,
    lines: Range<usize>,
) -> Vec<SelectionRect> {
    let total = map.chars();
    let start = range.start.min(total);
    let end = range.end.clamp(start, total);
    if start == end {
        return Vec::new();
    }

    let mut out = Vec::new();
    // 数的是**实际走了几行**，不是递进来的区间长度：越界或没字的行由 `line_chars` 的
    // `get` 丢掉，那一行没走过，不该记进账。这一格的代价就是「行数 × 每行的度量」，
    // 窄化生效时它是个位数，退回整章扫时它是「章行数 × 每条批注」。
    let mut walked = 0u64;
    for line_index in lines {
        walked += 1;
        let Some(line_range) = line_chars(layout, dom, map, line_index) else {
            continue;
        };
        // 这一行与选区的交集为空就跳过。
        let from = line_range.start.max(start);
        let to = line_range.end.min(end);
        if from >= to {
            continue;
        }
        let Some(block) = layout.block_of_line(line_index) else {
            continue;
        };
        let line = &layout.lines[line_index];
        // 与 `hit_test` 同一个起点：沿**内联轴**走，落地时才分轴。
        let start = layout.inline_start(line);
        let mut cursor = start;
        // 与 `hit_test` 同一份「隙游标」：两端对齐按它算每个字的右推量。
        let justify = line.justify;
        let mut gap_cursor = 0u32;
        let mut rect: Option<(f32, f32)> = None; // (x0, x1)

        for fragment in &line.fragments {
            let LineFragment::Text(run) = fragment else {
                // 图片占横向位置但不产出字符：推进游标，不产出矩形。
                if let LineFragment::Image(image) = fragment {
                    cursor += image.advance;
                }
                continue;
            };
            let text = block.text.get(run.norm.clone()).unwrap_or("");
            let run_start = map
                .chars_of_bytes(dom, run.node, run.orig.clone())
                .map(|r| r.start);
            // 与 hit_test 同一套三层：起点按 run.width，字缘优先 shaping（char_edges），
            // 退路才用 char_width 累加 —— 矩形的边才贴得住画出来的字。
            let run_x = cursor;
            let edges = measure
                .char_edges(text, &run.style.font, run.style.font_size)
                .filter(|edges| edges.len() == text.chars().count() + 1);
            if let (Some(base), Some(edges)) = (run_start, edges) {
                // `character_spacing`（字距）与两端对齐都不在 shaping 的结果里，得自己加：
                // 边界 k 的字距偏移是 `ls × k`（RTL 反向，按边表升/降判，与
                // `edges_from_glyphs` 同一判据）；两端对齐的偏移按隙游标逐字摊。
                let sign = if edges.len() >= 2 && edges[1] < edges[0] {
                    -1.0
                } else {
                    1.0
                };
                let ls = run.style.letter_spacing;
                let mut gap = gap_cursor;
                for (k, ch) in text.chars().enumerate() {
                    let at = base + k;
                    if at >= from && at < to {
                        // 一条边界表在 RTL 那一段上是**递减**的（`edges[k]` 的含义是
                        // 「这个字符在阅读方向上的起点」，见 `Measure::char_edges`），
                        // 所以取矩形要 min/max 两个都要 —— 只 `max` 那一头是以前默认
                        // 表递增留下的写法，方向一反就塌成零宽的一条。
                        let shift = justify.map_or(0.0, |j| j.shift(gap));
                        let a = run_x + edges[k] + sign * ls * k as f32 + shift;
                        let b = run_x + edges[k + 1] + sign * ls * (k + 1) as f32 + shift;
                        let (cx0, cx1) = (a.min(b), a.max(b));
                        let (x0, x1) = rect.unwrap_or((cx0, cx0));
                        rect = Some((x0.min(cx0), x1.max(cx1)));
                    }
                    gap += justify.map_or(0, |j| j.advance_cursor(ch));
                }
                gap_cursor = gap;
            } else {
                let (mut pixel, mut chars_before) = (0.0f32, 0usize);
                let mut gap = gap_cursor;
                for ch in text.chars() {
                    // `advance` 已含字距；两端对齐再按隙游标推一笔。
                    let advance = run.style.advance(measure, ch);
                    if let Some(base) = run_start {
                        let at = base + chars_before;
                        if at >= from && at < to {
                            let shift = justify.map_or(0.0, |j| j.shift(gap));
                            let cx0 = run_x + pixel + shift;
                            let cx1 = cx0 + advance;
                            let (x0, x1) = rect.unwrap_or((cx0, cx0));
                            rect = Some((x0.min(cx0), x1.max(cx1)));
                        }
                        chars_before += 1;
                    }
                    pixel += advance;
                    gap += justify.map_or(0, |j| j.advance_cursor(ch));
                }
                gap_cursor = gap;
            }
            cursor += run.width;
        }
        // 命中过至少一个字符才画：交集非空但逐字没命中（空白折叠的退化映射）
        // 时退回整行宽，宁宽勿丢。
        let (i0, i1) = match rect {
            Some(span) if span.1 > span.0 => span,
            _ => (start, start + line.ink_width()),
        };
        // 这两个数是**沿字**的两个位置，落回物理得分轴：横排铺在 x 上、吃整行高，
        // 竖排铺在 y 上、吃整列宽。「选区按行（列）断开」这一条两档相同。
        out.push(if layout.vertical() {
            SelectionRect {
                x: line.x,
                y: i0,
                width: line.width,
                height: i1 - i0,
            }
        } else {
            SelectionRect {
                x: i0,
                y: line.y,
                width: i1 - i0,
                height: line.height,
            }
        });
    }
    Metric::RectLines.add(walked);
    out
}

/// 长按落点吸到一个**词**上：返回含头不含尾的字符区间。
///
/// 词的判据与 gpui-kit 文本选择的分类一致（`text/selection.rs` 的 `CharType`）：
/// 拉丁词字符（含扩展拉丁、西里尔、越南文、连字）连成词；空白连成一段；
/// CJK 与标点**单字自为界** —— 中文没有词间空格，吸一个字就是正确的词。
/// 换行符单独一类，也单字自为界。
pub fn snap_word(map: &TextMap, offset: usize) -> Range<usize> {
    let total = map.chars();
    if total == 0 {
        return 0..0;
    }
    // 越界夹进来：拖到行尾之外再长按不该 panic。
    let offset = offset.min(total.saturating_sub(1));
    let class_of =
        |offset: usize| -> Option<CharClass> { map.text().chars().nth(offset).map(CharClass::of) };
    let Some(class) = class_of(offset) else {
        return offset..offset;
    };
    // 只有「会连成一段」的类别才向两边扩；换行、CJK、标点单字自为界。
    if !class.is_connectable() {
        return offset..offset + 1;
    }
    let mut start = offset;
    while start > 0 && class_of(start - 1) == Some(class) {
        start -= 1;
    }
    let mut end = offset + 1;
    while end < total && class_of(end) == Some(class) {
        end += 1;
    }
    start..end
}

/// 与 gpui-kit `text/selection.rs` 同一套分类（词 / 空白 / 换行 / 其他）。
///
/// `pub(crate)` 的理由是**只有一份**：定版式那一族（PDF / DjVu 的文字层，
/// [`crate::content::format::PageText::snap`]）要收的是同一个边 —— 中文与标点单字自为界、
/// 拉丁连成词。两处各写一遍分类表，就会有一处的双击在中文里选中一整句、另一处只选一个字。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CharClass {
    Word,
    Whitespace,
    Newline,
    Other,
}

impl CharClass {
    pub(crate) fn of(ch: char) -> Self {
        if is_word_char(ch) {
            Self::Word
        } else if ch == '\n' || ch == '\r' {
            Self::Newline
        } else if ch.is_whitespace() {
            Self::Whitespace
        } else {
            Self::Other
        }
    }

    /// 同类字符连成一段。换行与 CJK/标点不连。
    pub(crate) fn is_connectable(self) -> bool {
        matches!(self, Self::Word | Self::Whitespace)
    }
}

/// 词字符的集合。抄自 gpui-kit 的 `is_word_char`：ASCII 字母数字与下划线之外，
/// 把拉丁扩展（法德西等）、西里尔、越南文与组合记号也算进来 ——
/// 这几种文种的词同样靠空格分界，长按应该吸出整个词。
fn is_word_char(c: char) -> bool {
    matches!(c, '_')
        || c.is_ascii_alphanumeric()
        || matches!(c, '\u{00C0}'..='\u{00FF}')
        || matches!(c, '\u{0100}'..='\u{017F}')
        || matches!(c, '\u{0180}'..='\u{024F}')
        || matches!(c, '\u{0400}'..='\u{04FF}')
        || matches!(c, '\u{1E00}'..='\u{1EFF}')
        || matches!(c, '\u{0300}'..='\u{036F}')
}

/// 第 `line` 行的行首字符偏移的薄包装，给选择模块对齐拖动起点用。
pub fn line_start(layout: &Layout, dom: &Dom, map: &TextMap, line: usize) -> Option<usize> {
    line_offset(layout, dom, map, line)
}

/// 命中所在文本节点的薄包装，M7 批注把命中位置换 `NodeId` 时用。
pub fn node_of(map: &TextMap, offset: usize) -> Option<NodeId> {
    map.node_at(offset)
}

#[cfg(test)]
mod tests;
