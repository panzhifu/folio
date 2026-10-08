//! E1 块级 + 行内布局。几何用一份「汉字 1 em、拉丁 0.5 em」的 mock `Measure` 来断言，
//! 真实字体度量在 app 侧接入，逻辑同一。

use crate::css::computed::{ComputedStyle, Display};
use crate::css::{Origin, RuleSet};
use crate::dom::{Dom, NodeId};

use super::{FontKey, Layout, LayoutOptions, LineMetrics, Measure, collect};
use crate::css::computed::cascade;

/// 断行规则：空格后优先，CJK 逐字可断（硬断）。
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

fn slice_width(text: &str, size: f32) -> f32 {
    text.chars().map(|c| char_width(c, size)).sum()
}

impl Measure for Mono {
    fn split(
        &self,
        text: &str,
        _font: &FontKey,
        font_size: f32,
        avail: f32,
        first_indent: f32,
    ) -> Vec<std::ops::Range<usize>> {
        let mut out = Vec::new();
        let mut line_start = 0usize;
        let mut used = first_indent;
        let mut break_at: Option<usize> = None;
        for (i, ch) in text.char_indices() {
            let w = char_width(ch, font_size);
            if i == line_start {
                used += w;
                continue;
            }
            if ch == ' ' {
                used += w;
                break_at = Some(i + 1);
                continue;
            }
            if used + w > avail {
                let cut = break_at.unwrap_or(i);
                let raw = &text[line_start..cut];
                let end = cut - (raw.len() - raw.trim_end().len());
                if line_start < end {
                    out.push(line_start..end);
                }
                line_start = end;
                used = slice_width(&text[end..i], font_size) + w;
                break_at = None;
            } else {
                used += w;
            }
        }
        if line_start < text.len() {
            let raw = &text[line_start..];
            let end = text.len() - (raw.len() - raw.trim_end().len());
            if line_start < end {
                out.push(line_start..end);
            }
        }
        if out.is_empty() {
            out.push(0..0);
        }
        out
    }

    fn metrics(&self, text: &str, _font: &FontKey, font_size: f32) -> LineMetrics {
        LineMetrics {
            width: slice_width(text, font_size),
            ascent: font_size * 0.8,
            descent: font_size * 0.2,
        }
    }
}

fn parse(html: &str, css: &str) -> (Dom, Vec<crate::css::computed::ComputedStyle>) {
    let dom = Dom::parse(html.as_bytes());
    let rules = RuleSet::parse(css, Origin::Author);
    let styles = cascade(&dom, &rules, &ComputedStyle::default());
    (dom, styles.styles.clone())
}

fn find(dom: &Dom, want: &str) -> NodeId {
    dom.elements()
        .find(|(id, _)| dom.tag_name(*id) == Some(want))
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("文档里应有 <{want}>"))
}

fn typeset(html: &str, css: &str, width: f32) -> (Dom, Layout) {
    let dom = Dom::parse(html.as_bytes());
    let styles = cascade(
        &dom,
        &RuleSet::parse(css, Origin::Author),
        &ComputedStyle::default(),
    );
    let layout = super::layout(
        &dom,
        &styles,
        find(&dom, "body"),
        LayoutOptions { width },
        &Mono,
    );
    (dom, layout)
}

fn texts(layout: &Layout) -> Vec<String> {
    (0..layout.lines.len())
        .map(|i| layout.line_text(i).to_string())
        .collect()
}

#[test]
fn a_long_paragraph_wraps_and_stacks() {
    let html = format!("<p>{}</p>", "字".repeat(30));
    let (_, layout) = typeset(&html, "", 200.0);
    assert_eq!(
        texts(&layout),
        vec!["字".repeat(12), "字".repeat(12), "字".repeat(6)],
        "16px 字号、200px 版心 → 每行 12 字"
    );
    assert!(
        layout.lines.iter().all(|l| l.height == 28.0),
        "行盒高 = 字号 × 行距"
    );
    for pair in layout.lines.windows(2) {
        assert!(pair[1].y > pair[0].y, "行盒必须单调下移");
    }
}

/// 中文排版基础之一：首行缩进只作用于块里的第一行。
#[test]
fn text_indent_shifts_only_the_first_line() {
    let html = format!("<p>{}</p>", "字".repeat(30));
    let (_, layout) = typeset(&html, "p { text-indent: 2em }", 200.0);
    assert_eq!(layout.lines[0].x, 32.0, "首行缩进 2em = 32px");
    assert_eq!(layout.lines[1].x, 0.0, "后续行回到版心左缘");
    assert_eq!(
        texts(&layout)[0].chars().count(),
        10,
        "首行少容 2 字，而不是整段都少"
    );
}

#[test]
fn sibling_paragraph_margins_collapse() {
    let (_, layout) = typeset("<p>甲</p><p>乙</p>", "", 200.0);
    let paras: Vec<&super::BlockBox> = layout
        .blocks
        .iter()
        .filter(|b| !b.text.is_empty())
        .collect();
    assert_eq!(paras.len(), 2);
    assert_eq!(paras[0].y, 16.0, "UA 给 p 的 1em 上边距");
    assert_eq!(
        paras[1].y - (paras[0].y + paras[0].height),
        16.0,
        "相邻段边距取较大值而不是相加"
    );
}

#[test]
fn headings_get_ua_size_and_weight_and_can_be_centered() {
    let (_, layout) = typeset("<h1>章名</h1>", "h1 { text-align: center }", 200.0);
    assert_eq!(layout.lines.len(), 1);
    assert_eq!(layout.lines[0].font_size, 32.0, "UA h1 = 2em");
    assert_eq!(layout.lines[0].x, 68.0, "两字 64px 居中于 200px 版心");

    let (dom, styles) = parse("<h1>x</h1><span>y</span>", "");
    let h1 = styles.iter().nth(find(&dom, "h1") as usize).unwrap();
    assert_eq!(h1.font_weight, 700);
    assert_eq!(h1.display, Display::Block);
    let span = styles.iter().nth(find(&dom, "span") as usize).unwrap();
    assert_eq!(span.display, Display::Inline, "UA 必须知道 span 是行内");
}

#[test]
fn br_forces_a_break_and_is_not_rendered() {
    let (_, layout) = typeset("<p>甲<br>乙</p>", "", 200.0);
    assert_eq!(texts(&layout), vec!["甲", "乙"]);
}

#[test]
fn nested_padding_offsets_and_narrows_the_child() {
    let html = format!("<div><p>{}</p></div>", "字".repeat(40));
    let (_, layout) = typeset(&html, "div { padding-left: 20px }", 200.0);
    let p = layout
        .blocks
        .iter()
        .find(|b| !b.text.is_empty())
        .expect("段落块");
    assert_eq!(p.x, 20.0);
    assert_eq!(p.width, 180.0);
    assert_eq!(layout.lines[0].x, 20.0, "行盒跟随内容盒");
    assert_eq!(texts(&layout)[0].chars().count(), 11, "180px 只容 11 字");
}

#[test]
fn display_none_removes_the_whole_subtree() {
    let (_, layout) = typeset("<p>留</p><p class=x>去</p>", "p.x { display: none }", 200.0);
    assert_eq!(texts(&layout), vec!["留"]);
}

#[test]
fn inline_pieces_keep_their_own_node_offsets() {
    let (dom, layout) = typeset("<p>甲<strong>乙</strong>丙</p>", "", 200.0);
    assert_eq!(layout.line_text(0), "甲乙丙");
    let fragments = &layout.lines[0].fragments;
    assert_eq!(fragments.len(), 3, "三个文本节点各一段");
    let recovered: Vec<String> = fragments
        .iter()
        .map(|f| {
            let node = dom.get(f.node);
            node.text.get(f.orig.clone()).unwrap_or("").to_string()
        })
        .collect();
    assert_eq!(
        recovered,
        vec!["甲", "乙", "丙"],
        "锚点坐标要能原路取回文本"
    );
}

#[test]
fn whitespace_between_pieces_collapses_to_one_space() {
    let (_, layout) = typeset("<p>前   <em>中</em>   后</p>", "", 200.0);
    assert_eq!(layout.line_text(0), "前 中 后", "跨节点的空白只折叠一次");
}

#[test]
fn pre_keeps_inner_whitespace_and_hard_breaks() {
    let (_, layout) = typeset("<pre>一\n  二</pre>", "", 200.0);
    assert_eq!(texts(&layout), vec!["一", "  二"]);
}

#[test]
fn empty_paragraph_produces_no_line_box() {
    let (_, layout) = typeset("<p></p><p>有</p>", "", 200.0);
    assert_eq!(layout.lines.len(), 1);
    assert_eq!(layout.lines[0].y, 32.0, "空段不占行高，两段边距折叠成一档");
}

#[test]
fn line_hit_testing_walks_the_stacks() {
    let html = format!("<p>{}</p>", "字".repeat(30));
    let (_, layout) = typeset(&html, "", 200.0);
    assert_eq!(layout.line_at(layout.lines[0].y), Some(0));
    assert_eq!(layout.line_at(layout.lines[1].y + 1.0), Some(1));
    assert_eq!(layout.line_at(layout.height + 10.0), None);
}

#[test]
fn layout_output_is_send_so_it_can_be_built_off_thread() {
    fn assert_send<T: Send>() {}
    assert_send::<Layout>();
    let (_, layout) = typeset("<p>文</p>", "", 200.0);
    assert_eq!(layout.lines.len(), 1);
}

#[test]
fn a_wider_viewport_yields_fewer_lines() {
    let html = format!("<p>{}</p>", "字".repeat(60));
    let (_, narrow) = typeset(&html, "", 160.0);
    let (_, wide) = typeset(&html, "", 320.0);
    assert_eq!(narrow.lines.len(), 6, "10 字/行");
    assert_eq!(wide.lines.len(), 3, "20 字/行");
    assert!(wide.height < narrow.height, "变宽必须变矮");
}

#[test]
fn collection_trims_paragraph_edges() {
    let (dom, _) = parse("<p>  甲 乙  </p>", "");
    let p = find(&dom, "p");
    let collected = collect(&dom.get(p).children.clone(), &dom, false);
    assert_eq!(collected.text, "甲 乙", "段首段尾空白吞掉，中间并成一个");
}
