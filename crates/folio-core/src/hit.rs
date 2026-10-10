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

use crate::anchor::{TextMap, line_at_y, line_chars, line_offset};
use crate::dom::{Dom, NodeId};
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
    let line = crate::anchor::line_at_offset(layout, dom, map, offset);
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
mod tests {
    //! 几何用与 `anchor::tests` 同一套假度量（汉字 1 em、拉丁 0.5 em、空格 0.25 em），
    //! 断点与坐标都是推出来的。round-trip 是核心：命中 → 矩形 → 命中，必须回到同一个字。

    use crate::anchor::{TextMap, line_offset};
    use crate::css::computed::{ComputedStyle, cascade};
    use crate::css::{Origin, RuleSet};
    use crate::dom::{Dom, NodeId};
    use crate::hit::{Hit, hit_test, paragraph_range, rects_for_range, snap_word};
    use crate::layout::{Layout, LayoutOptions, LineMetrics, Measure, NoImages, layout};

    struct Mono;

    fn char_width(ch: char, size: f32) -> f32 {
        if ch == ' ' {
            size * 0.25
        } else if (ch as u32) >= 0x2e80 {
            size
        } else {
            size * 0.5
        }
    }

    impl Measure for Mono {
        fn char_width(&self, ch: char, _font: &crate::layout::FontKey, font_size: f32) -> f32 {
            char_width(ch, font_size)
        }

        fn run_metrics(
            &self,
            text: &str,
            _font: &crate::layout::FontKey,
            font_size: f32,
        ) -> LineMetrics {
            LineMetrics {
                width: text.chars().map(|c| char_width(c, font_size)).sum(),
                ascent: font_size * 0.8,
                descent: font_size * 0.2,
            }
        }
    }

    fn body(dom: &Dom) -> NodeId {
        dom.elements()
            .find(|(id, _)| dom.tag_name(*id) == Some("body"))
            .map(|(id, _)| id)
            .unwrap_or(dom.document)
    }

    struct Chapter {
        dom: Dom,
        layout: Layout,
        map: TextMap,
    }

    fn typeset(html: &str, css: &str, width: f32) -> Chapter {
        let dom = Dom::parse(html.as_bytes());
        let styles = cascade(
            &dom,
            &RuleSet::parse(css, Origin::Author),
            &ComputedStyle::default(),
        );
        let root = body(&dom);
        let map = TextMap::build(&dom, root);
        let laid = layout(
            &dom,
            &styles,
            root,
            LayoutOptions {
                width,
                light: false,
            },
            &Mono,
            &NoImages,
        );
        Chapter {
            dom,
            layout: laid,
            map,
        }
    }

    fn cjk(chars: usize) -> String {
        "字".repeat(chars)
    }

    /// 第 `line` 行第 `k` 个字的中心 x（排版坐标）。行内字符等宽推进，直接算。
    fn char_center(chapter: &Chapter, line: usize, k: usize) -> (f32, f32) {
        let box_ = &chapter.layout.lines[line];
        let font_size = box_.dominant_font_size();
        let x = box_.x + font_size * (k as f32 + 0.5);
        let y = box_.y + box_.height * 0.5;
        (x, y)
    }

    #[test]
    fn hitting_a_character_reports_its_offset() {
        let chapter = typeset(&format!("<p>{}</p>", cjk(120)), "", 200.0);
        assert_eq!(chapter.layout.lines.len(), 10, "一行 12 字");
        // 第 2 行第 5 个字 → 偏移 24 + 5。
        let (x, y) = char_center(&chapter, 2, 5);
        let hit = hit_test(&chapter.layout, &chapter.dom, &chapter.map, &Mono, x, y)
            .expect("正文行应该命中");
        assert_eq!(hit.line, 2);
        assert_eq!(hit.offset, 29, "第 2 行行首 24 + 第 5 个字");
    }

    /// round-trip：每个字的中心命中自己；右半边命中下一个字之前。
    #[test]
    fn hit_and_rect_round_trip_through_each_character() {
        let chapter = typeset(&format!("<p>{}</p>", cjk(120)), "", 200.0);
        let font_size = chapter.layout.lines[0].dominant_font_size();
        for line in 0..chapter.layout.lines.len() {
            for k in 0..12 {
                let (x, y) = char_center(&chapter, line, k);
                let hit = hit_test(&chapter.layout, &chapter.dom, &chapter.map, &Mono, x, y)
                    .unwrap_or_else(|| panic!("({line},{k}) 应命中"));
                assert_eq!(hit.offset, line * 12 + k, "({line},{k}) 命中自己");
                // 选中矩形包住这个字，矩形中心再命中还是它。
                let rects = rects_for_range(
                    &chapter.layout,
                    &chapter.dom,
                    &chapter.map,
                    &Mono,
                    hit.offset..hit.offset + 1,
                );
                assert_eq!(rects.len(), 1, "单字选中一行一条");
                let rect = &rects[0];
                let cx = rect.x + rect.width / 2.0;
                let cy = rect.y + rect.height / 2.0;
                let again = hit_test(&chapter.layout, &chapter.dom, &chapter.map, &Mono, cx, cy)
                    .expect("矩形中心应再命中");
                assert_eq!(again.offset, hit.offset, "矩形中心命中同一个字");
            }
            let _ = font_size;
        }
    }

    /// 行尾之外是「最后一个字之后」，行首之外是行首；纵向越界夹到最近行。
    #[test]
    fn edges_clamp_instead_of_missing() {
        let chapter = typeset(&format!("<p>{}</p>", cjk(120)), "", 200.0);
        let line = &chapter.layout.lines[3];
        // 行左之外很远。
        let hit = hit_test(
            &chapter.layout,
            &chapter.dom,
            &chapter.map,
            &Mono,
            line.x - 50.0,
            line.y + 5.0,
        )
        .expect("行左应夹到行首");
        assert_eq!(hit.offset, 36, "第 3 行行首");
        // 行右之外很远：插入点（第 4 行末 = 第 5 行首，同一坐标）。
        let hit = hit_test(
            &chapter.layout,
            &chapter.dom,
            &chapter.map,
            &Mono,
            line.x + line.width + 50.0,
            line.y + 5.0,
        )
        .expect("行右应夹到行尾");
        assert_eq!(hit.offset, 48);
        // 纵向在两行之间：夹到上一行。
        let boundary = chapter.layout.lines[2].y + chapter.layout.lines[2].height;
        let hit = hit_test(
            &chapter.layout,
            &chapter.dom,
            &chapter.map,
            &Mono,
            chapter.layout.lines[2].x + 5.0,
            boundary - 0.5,
        )
        .expect("行界附近应命中");
        assert_eq!(hit.line, 2);
    }

    /// 跨行选中每行一条，矩形纵向就是行盒，横向从首字左缘到末字右缘。
    #[test]
    fn a_range_spanning_lines_gives_one_rect_per_line() {
        let chapter = typeset(&format!("<p>{}</p>", cjk(120)), "", 200.0);
        // 从第 1 行第 10 个字选到第 3 行第 2 个字。
        let range = 22..38;
        let rects = rects_for_range(&chapter.layout, &chapter.dom, &chapter.map, &Mono, range);
        assert_eq!(rects.len(), 3, "跨三行，实际 {rects:?}");
        // 首行：从第 10 字的左缘到行尾。
        let first = &rects[0];
        let size = chapter.layout.lines[0].dominant_font_size();
        assert!(
            (first.x - (chapter.layout.lines[0].x + size * 10.0)).abs() < 0.5,
            "首矩形应从第 10 字左缘开始，实际 {}",
            first.x
        );
        // 中行：整行宽。
        let middle = &rects[1];
        assert!(
            (middle.x - chapter.layout.lines[1].x).abs() < 0.5
                && (middle.width - chapter.layout.lines[1].width).abs() < 0.5,
            "中间行应整行覆盖"
        );
        // 末行：从行首到第 2 个字的右缘。
        let last = &rects[2];
        assert!(
            (last.width - (chapter.layout.lines[2].x + size * 2.0 - last.x)).abs() < 0.5,
            "末矩形应止于第 2 字右缘"
        );
        // 矩形纵向等于行盒。
        for (rect, line) in rects.iter().zip([1, 2, 3]) {
            assert_eq!(rect.y, chapter.layout.lines[line].y);
            assert_eq!(rect.height, chapter.layout.lines[line].height);
        }
    }

    #[test]
    fn an_empty_or_inverted_range_has_no_rects() {
        let chapter = typeset(&format!("<p>{}</p>", cjk(60)), "", 200.0);
        assert!(
            rects_for_range(&chapter.layout, &chapter.dom, &chapter.map, &Mono, 5..5).is_empty()
        );
        // 反向拖：夹平。写成结构体而不是 `9..3` 字面量 —— 后者是 clippy 的
        // `reversed_empty_ranges`，而这里正是在测「传进来一个倒过来的区间会怎样」。
        let inverted = std::ops::Range { start: 9, end: 3 };
        assert!(
            rects_for_range(&chapter.layout, &chapter.dom, &chapter.map, &Mono, inverted)
                .is_empty()
        );
        // 越界夹到章尾。
        let rects = rects_for_range(&chapter.layout, &chapter.dom, &chapter.map, &Mono, 55..999);
        assert!(!rects.is_empty(), "越界应夹到章尾而不是空");
    }

    /// 多个文本节点拼成的一行：命中与矩形都要跨节点连续。
    #[test]
    fn a_line_across_nodes_hits_and_highlights_continuously() {
        let chapter = typeset(
            "<body><p>甲乙<strong>丙丁</strong>戊己</p></body>",
            "",
            600.0,
        );
        assert_eq!(chapter.layout.lines.len(), 1);
        assert_eq!(chapter.map.slice(0..6), "甲乙丙丁戊己");
        // 「丙」是第 2 个字，落在 <strong> 里。
        let (x, y) = char_center(&chapter, 0, 2);
        let hit =
            hit_test(&chapter.layout, &chapter.dom, &chapter.map, &Mono, x, y).expect("应命中");
        assert_eq!(hit.offset, 2);
        // 选中「丙丁」：一条矩形，覆盖两段节点之间的接缝。
        let rects = rects_for_range(&chapter.layout, &chapter.dom, &chapter.map, &Mono, 2..4);
        assert_eq!(rects.len(), 1);
        let size = chapter.layout.lines[0].dominant_font_size();
        assert!(
            (rects[0].width - size * 2.0).abs() < 0.5,
            "跨节点两字矩形宽 = 两字前进量，实际 {}",
            rects[0].width
        );
    }

    /// 长按吸附：拉丁吸成词，CJK 单字，空白成段。
    #[test]
    fn snapping_follows_the_script() {
        let chapter = typeset("<body><p>Hello 世界 foo bar</p></body>", "", 600.0);
        let text = &chapter.map;
        // "Hello" 占 0..5：点中间吸到整词。
        assert_eq!(snap_word(text, 2), 0..5);
        assert_eq!(snap_word(text, 0), 0..5);
        // 空格吸成那一段空格（单空格 → 单字符区间）。
        assert_eq!(snap_word(text, 5), 5..6);
        // CJK 单字自为界。
        assert_eq!(snap_word(text, 7), 7..8, "「世」一个字就是一个词");
        // "bar" 占 13..16。
        assert_eq!(snap_word(text, 14), 13..16);
        // 越界夹到末字。
        assert_eq!(snap_word(text, 999), 13..16);
    }

    /// 带 kerning 的度量：char_width 说 "AW" 各 10px（合计 20），shaping 边缘说
    /// 合计 15。命中与矩形必须跟 shaping 走 —— 这正是真机上选词偏移的那类漂移。
    struct Kerning;

    impl Measure for Kerning {
        // 用全限定路径：测试模块点名导入的清单里没有 FontKey（不用 glob 的老规矩）。
        fn char_width(&self, _ch: char, _f: &crate::layout::FontKey, _s: f32) -> f32 {
            10.0
        }
        fn run_metrics(&self, text: &str, _f: &crate::layout::FontKey, _s: f32) -> LineMetrics {
            // "AW" 有 kerning：整段 15，不是逐字和 20。
            LineMetrics {
                width: if text == "AW" {
                    15.0
                } else {
                    text.chars().count() as f32 * 10.0
                },
                ascent: 8.0,
                descent: 2.0,
            }
        }
        fn byte_at_x(
            &self,
            text: &str,
            _f: &crate::layout::FontKey,
            _s: f32,
            x: f32,
        ) -> Option<usize> {
            if text != "AW" {
                return None;
            }
            // A: 0..4，W: 4..15（kern 吃掉 6px）。
            if x < 4.0 {
                Some(if x > 2.0 { 1 } else { 0 })
            } else if x < 15.0 {
                Some(if x > 9.5 { 2 } else { 1 })
            } else {
                Some(2)
            }
        }
        fn char_edges(&self, text: &str, _f: &crate::layout::FontKey, _s: f32) -> Option<Vec<f32>> {
            if text != "AW" {
                return None;
            }
            Some(vec![0.0, 4.0, 15.0])
        }
    }

    #[test]
    fn hit_and_rects_follow_shaping_not_char_width() {
        let chapter = typeset("<body><p>AW</p></body>", "", 600.0);
        let line = &chapter.layout.lines[0];
        // char_width 会把 x=12 判给 W（10..20），shaping 说那是 A 的末尾区间（4..15）的
        // 前半 → 还是 W；x=5 才是分水岭：char_width 说在 A 里，shaping 说已在 W 里。
        let hit = hit_test(
            &chapter.layout,
            &chapter.dom,
            &chapter.map,
            &Kerning,
            line.x + 5.0,
            line.y + 5.0,
        )
        .expect("应命中");
        assert_eq!(hit.offset, 1, "x=5 按 shaping 已是 W，char_width 会误判 A");

        // 矩形：W 的矩形是 4..15（宽 11），不是 char_width 的 10..20。
        let rects = rects_for_range(&chapter.layout, &chapter.dom, &chapter.map, &Kerning, 1..2);
        assert_eq!(rects.len(), 1);
        assert!(
            (rects[0].x - (line.x + 4.0)).abs() < 0.01,
            "实际 {}",
            rects[0].x
        );
        assert!(
            (rects[0].width - 11.0).abs() < 0.01,
            "实际 {}",
            rects[0].width
        );

        // A 的矩形同理：0..4。
        let rects = rects_for_range(&chapter.layout, &chapter.dom, &chapter.map, &Kerning, 0..1);
        assert!(
            (rects[0].width - 4.0).abs() < 0.01,
            "实际 {}",
            rects[0].width
        );
    }

    #[test]
    fn an_empty_chapter_neither_hits_nor_snaps() {
        let chapter = typeset("<body><p></p></body>", "", 200.0);
        assert!(
            hit_test(
                &chapter.layout,
                &chapter.dom,
                &chapter.map,
                &Mono,
                10.0,
                10.0
            )
            .is_none()
        );
        assert_eq!(snap_word(&chapter.map, 0), 0..0);
    }

    /// 命中行的行首偏移与 anchor 的行映射对得上：选中模块两端共用一套坐标。
    #[test]
    fn hit_offsets_agree_with_the_anchor_line_map() {
        let chapter = typeset(&format!("<p>{}</p>", cjk(120)), "", 200.0);
        for line in 0..chapter.layout.lines.len() {
            let (x, y) = char_center(&chapter, line, 0);
            let hit = hit_test(&chapter.layout, &chapter.dom, &chapter.map, &Mono, x, y)
                .unwrap_or_else(|| panic!("第 {line} 行应命中"));
            let start = line_offset(&chapter.layout, &chapter.dom, &chapter.map, line)
                .unwrap_or_else(|| panic!("第 {line} 行应有行首坐标"));
            assert_eq!(hit.offset, start);
        }
    }

    /// 命中在行内图上：图不产出字符，落点是图之前文本的末尾。
    #[test]
    fn hitting_an_image_lands_after_the_previous_text() {
        struct OneImage;
        impl crate::layout::ImageProvider for OneImage {
            fn resolve(&self, _src: &str) -> Option<crate::layout::ResolvedImage> {
                Some(crate::layout::ResolvedImage {
                    href: "pic.png".into(),
                    width: 40.0,
                    height: 20.0,
                })
            }
        }
        let dom = Dom::parse("<body><p>\u{524D}<img src=\"pic.png\">\u{540E}</p>".as_bytes());
        let styles = cascade(
            &dom,
            &RuleSet::parse("", Origin::Author),
            &ComputedStyle::default(),
        );
        let root = body(&dom);
        let map = TextMap::build(&dom, root);
        let laid = layout(
            &dom,
            &styles,
            root,
            LayoutOptions {
                width: 600.0,
                light: false,
            },
            &Mono,
            &OneImage,
        );
        assert_eq!(map.chars(), 2, "「前后」两字，图不占坐标");
        // 命中在图的横向范围内（行首字之后）→ 落点是「前」之后。
        let line = &laid.lines[0];
        let size = line.dominant_font_size();
        let hit = hit_test(&laid, &dom, &map, &Mono, line.x + size + 20.0, line.y + 5.0)
            .expect("图上应命中");
        assert_eq!(hit.offset, 1, "「前」之后、「后」之前");
    }

    /// `Hit` 的行号语义：选中模块用它对齐拖动的纵向吸附。
    #[test]
    fn hit_carries_the_line_index() {
        let chapter = typeset(&format!("<p>{}</p>", cjk(120)), "", 200.0);
        let (x, y) = char_center(&chapter, 4, 3);
        let hit =
            hit_test(&chapter.layout, &chapter.dom, &chapter.map, &Mono, x, y).expect("应命中");
        assert_eq!(
            hit,
            Hit {
                line: 4,
                offset: 51
            }
        );
    }

    /// 全选本段：单段书给整段，跨行也连续；两段书只取命中那一段。
    #[test]
    fn paragraph_range_covers_the_whole_block_and_nothing_else() {
        let chapter = typeset(
            &format!("<p>{}</p><p>乙段只在第二段。</p>", cjk(120)),
            "",
            200.0,
        );
        // 命中第 2 行（第一段中间）。
        let (x, y) = char_center(&chapter, 2, 3);
        let hit =
            hit_test(&chapter.layout, &chapter.dom, &chapter.map, &Mono, x, y).expect("应命中");
        let range = paragraph_range(&chapter.layout, &chapter.dom, &chapter.map, hit.offset)
            .expect("段落应可解");
        assert_eq!(range.start, 0, "第一段从章首开始");
        assert_eq!(
            chapter.map.slice(range.clone()),
            cjk(120),
            "整段内容原样取回"
        );

        // 命中第二段：区间不跨进第一段。
        let second_start = paragraph_range(&chapter.layout, &chapter.dom, &chapter.map, 125)
            .expect("第二段应可解");
        assert_eq!(chapter.map.slice(second_start.clone()), "乙段只在第二段。");
        assert!(second_start.start >= 120, "不该吃进第一段的字");
    }
}
