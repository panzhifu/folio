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

use std::ops::Range;

use crate::content::anchor::{TextMap, line_at_y, line_chars, line_offset};
use crate::content::dom::{Dom, NodeId};
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
    let line_index = line_at_y(layout, y);
    let line = &layout.lines[line_index];
    // 这一行的文本边界。没有字符的行（纯图片 / 空行）给不出坐标。
    let line_range = line_chars(layout, dom, map, line_index)?;
    let block = layout.block_of_line(line_index)?;

    // 横向夹进这一行的墨迹范围内再逐段走。
    let x = x.clamp(line.x, line.x + line.width);
    let mut cursor = line.x;
    // 「最后一个字之后」的落点：走到行尾也没命中时就停在这里。
    let mut end_offset = line_range.end;

    // run 的**起点**按排版的 run.width 累加 —— 绘制（`place_lines` → `paint_run`）
    // 就是这么摆的；run **内部**的字位优先问 shaping（`byte_at_x`，kerning / 字体
    // 回退都在里面），做不了 shaping 才退回 char_width 累加。三层同源，命中
    // 才贴得住画出来的字。
    for fragment in &line.fragments {
        let LineFragment::Text(run) = fragment else {
            // 图片不产出字符，但**占横向位置**：游标必须推进它，后面的字才量得准。
            if let LineFragment::Image(image) = fragment {
                cursor += image.width;
            }
            continue;
        };
        let text = block.text.get(run.norm.clone()).unwrap_or("");
        let run_x = cursor;
        // shaping 同源的 run 内定位。
        if let Some(bytes) =
            measure.byte_at_x(text, &run.style.font, run.style.font_size, x - run_x)
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
        for ch in text.chars() {
            let advance = run.style.advance(measure, ch);
            let char_x = run_x + pixel;
            if x < char_x + advance {
                let after = x > char_x + advance * 0.5;
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
        }
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

/// 长按落点所在**段落**的字符区间（全选本段用）。
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

/// `orig` 区间起点 + run 内字节偏移 → node.text 里的绝对字节位置。
fn orig_at(orig_start: usize, bytes: usize) -> Range<usize> {
    let at = orig_start + bytes;
    at..at
}

/// 一段字符区间在排版里的矩形（排版坐标，每行一条）。
///
/// 区间是**含头不含尾**（`TextAnchor` 的语义）。图片片段没有字符坐标，
/// 不产出矩形，与 `line_chars` 的口径一致。
pub fn rects_for_range(
    layout: &Layout,
    dom: &Dom,
    map: &TextMap,
    measure: &dyn Measure,
    range: Range<usize>,
) -> Vec<SelectionRect> {
    let total = map.chars();
    let start = range.start.min(total);
    let end = range.end.clamp(start, total);
    if start == end {
        return Vec::new();
    }

    let mut out = Vec::new();
    for line_index in 0..layout.lines.len() {
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
        let mut cursor = line.x;
        let mut rect: Option<(f32, f32)> = None; // (x0, x1)

        for fragment in &line.fragments {
            let LineFragment::Text(run) = fragment else {
                // 图片占横向位置但不产出字符：推进游标，不产出矩形。
                if let LineFragment::Image(image) = fragment {
                    cursor += image.width;
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
                for (k, _) in text.chars().enumerate() {
                    let at = base + k;
                    if at >= from && at < to {
                        let cx0 = run_x + edges[k];
                        let cx1 = run_x + edges[k + 1].max(edges[k]);
                        let (x0, x1) = rect.unwrap_or((cx0, cx0));
                        rect = Some((x0.min(cx0), x1.max(cx1)));
                    }
                }
            } else {
                let (mut pixel, mut chars_before) = (0.0f32, 0usize);
                for ch in text.chars() {
                    let advance = run.style.advance(measure, ch);
                    if let Some(base) = run_start {
                        let at = base + chars_before;
                        if at >= from && at < to {
                            let cx0 = run_x + pixel;
                            let cx1 = cx0 + advance;
                            let (x0, x1) = rect.unwrap_or((cx0, cx0));
                            rect = Some((x0.min(cx0), x1.max(cx1)));
                        }
                        chars_before += 1;
                    }
                    pixel += advance;
                }
            }
            cursor += run.width;
        }
        // 命中过至少一个字符才画：交集非空但逐字没命中（空白折叠的退化映射）
        // 时退回整行宽，宁宽勿丢。
        let (x0, x1) = match rect {
            Some(span) if span.1 > span.0 => span,
            _ => (line.x, line.x + line.width),
        };
        out.push(SelectionRect {
            x: x0,
            y: line.y,
            width: x1 - x0,
            height: line.height,
        });
    }
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Word,
    Whitespace,
    Newline,
    Other,
}

impl CharClass {
    fn of(ch: char) -> Self {
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
    fn is_connectable(self) -> bool {
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
