//! 几何用与 `anchor::tests` 同一套假度量（汉字 1 em、拉丁 0.5 em、空格 0.25 em），
//! 断点与坐标都是推出来的。round-trip 是核心：命中 → 矩形 → 命中，必须回到同一个字。

use std::ops::Range;

use crate::content::anchor::{CharLines, TextMap, line_offset};
use crate::content::dom::{Dom, NodeId};
use crate::content::format::css::computed::{ComputedStyle, cascade};
use crate::content::format::css::{Origin, RuleSet};
use crate::content::hit::{
    Hit, hit_test, paragraph_range, rects_for_range, rects_for_range_in, snap_word,
};
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
    let hit =
        hit_test(&chapter.layout, &chapter.dom, &chapter.map, &Mono, x, y).expect("正文行应该命中");
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
    assert!(rects_for_range(&chapter.layout, &chapter.dom, &chapter.map, &Mono, 5..5).is_empty());
    // 反向拖：夹平。写成结构体而不是 `9..3` 字面量 —— 后者是 clippy 的
    // `reversed_empty_ranges`，而这里正是在测「传进来一个倒过来的区间会怎样」。
    let inverted = std::ops::Range { start: 9, end: 3 };
    assert!(
        rects_for_range(&chapter.layout, &chapter.dom, &chapter.map, &Mono, inverted).is_empty()
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
    let hit = hit_test(&chapter.layout, &chapter.dom, &chapter.map, &Mono, x, y).expect("应命中");
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
    fn byte_at_x(&self, text: &str, _f: &crate::layout::FontKey, _s: f32, x: f32) -> Option<usize> {
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
    let hit =
        hit_test(&laid, &dom, &map, &Mono, line.x + size + 20.0, line.y + 5.0).expect("图上应命中");
    assert_eq!(hit.offset, 1, "「前」之后、「后」之前");
}

/// `Hit` 的行号语义：选中模块用它对齐拖动的纵向吸附。
#[test]
fn hit_carries_the_line_index() {
    let chapter = typeset(&format!("<p>{}</p>", cjk(120)), "", 200.0);
    let (x, y) = char_center(&chapter, 4, 3);
    let hit = hit_test(&chapter.layout, &chapter.dom, &chapter.map, &Mono, x, y).expect("应命中");
    assert_eq!(
        hit,
        Hit {
            line: 4,
            offset: 51
        }
    );
}

/// 整段选择：单段书给整段，跨行也连续；两段书只取命中那一段。
#[test]
fn paragraph_range_covers_the_whole_block_and_nothing_else() {
    let chapter = typeset(
        &format!("<p>{}</p><p>乙段只在第二段。</p>", cjk(120)),
        "",
        200.0,
    );
    // 命中第 2 行（第一段中间）。
    let (x, y) = char_center(&chapter, 2, 3);
    let hit = hit_test(&chapter.layout, &chapter.dom, &chapter.map, &Mono, x, y).expect("应命中");
    let range = paragraph_range(&chapter.layout, &chapter.dom, &chapter.map, hit.offset)
        .expect("段落应可解");
    assert_eq!(range.start, 0, "第一段从章首开始");
    assert_eq!(
        chapter.map.slice(range.clone()),
        cjk(120),
        "整段内容原样取回"
    );

    // 命中第二段：区间不跨进第一段。
    let second_start =
        paragraph_range(&chapter.layout, &chapter.dom, &chapter.map, 125).expect("第二段应可解");
    assert_eq!(chapter.map.slice(second_start.clone()), "乙段只在第二段。");
    assert!(second_start.start >= 120, "不该吃进第一段的字");
}

/// 批注回显与选区的那条快路：**结果与整章扫逐条相同**。
///
/// 基准是 `rects_for_range`（参考实现，永远走全部行），被测的是
/// `rects_for_range_in` + 索引二分出来的行号段。区间把边界都覆盖一遍：单字、跨行、
/// 整章、空、反向、越界、每一个偏移上的短选区，两种文字方向各跑一遍。
/// 少一行就是屏上少一块底色，而那种错只在某本书的某一条批注上出现。
#[test]
fn narrowed_rects_are_line_for_line_the_full_scan() {
    for css in ["", "p { direction: rtl }"] {
        let chapter = typeset(
            &format!(
                "<body><p>{}</p><p>短段落</p><p>{}</p></body>",
                cjk(90),
                cjk(80)
            ),
            css,
            200.0,
        );
        let index = CharLines::build(&chapter.layout, &chapter.dom, &chapter.map);
        assert!(
            index.is_ordered(),
            "样式 {css:?}：索引不可用，这条门就退化成自己跟自己比"
        );
        let total = chapter.map.chars();
        let mut ranges: Vec<Range<usize>> = vec![
            0..1,
            5..5,
            22..38,
            0..total,
            total..total + 50,
            Range { start: 30, end: 12 },
        ];
        for at in 0..total {
            ranges.push(at..at + 1);
            ranges.push(at..at + 5);
        }
        for range in ranges {
            let wide = rects_for_range(
                &chapter.layout,
                &chapter.dom,
                &chapter.map,
                &Mono,
                range.clone(),
            );
            let lines = index
                .lines_for(range.clone())
                .unwrap_or(0..chapter.layout.lines.len());
            let tight = rects_for_range_in(
                &chapter.layout,
                &chapter.dom,
                &chapter.map,
                &Mono,
                range.clone(),
                lines.clone(),
            );
            assert_eq!(
                tight, wide,
                "样式 {css:?}、区间 {range:?}、行号段 {lines:?}：窄化改了答案"
            );
            assert!(
                lines.end <= chapter.layout.lines.len(),
                "行号段 {lines:?} 越过了行数 {}",
                chapter.layout.lines.len()
            );
        }
    }
}

/// 跨多行的长选区：窄化给的行号段要**严格短于**整章，否则这条优化在最常见的批注
/// 形状上根本没生效（把 `CharLines::lines_for` 的两头夹取去掉就是这条红）。
#[test]
fn a_selection_spanning_lines_scans_fewer_lines_than_the_chapter() {
    let chapter = typeset(
        &format!("<body><p>{}</p><p>{}</p></body>", cjk(240), cjk(240)),
        "",
        200.0,
    );
    let index = CharLines::build(&chapter.layout, &chapter.dom, &chapter.map);
    let total_lines = chapter.layout.lines.len();
    // 中间那 40 个字（跨三四行），两侧各留一百多字。
    let range = 250..290;
    let lines = index.lines_for(range.clone()).expect("单调的章");
    assert!(
        lines.len() * 2 < total_lines,
        "半本都不到的选区给了 {lines:?}／整章 {total_lines} 行"
    );
    let rects = rects_for_range_in(
        &chapter.layout,
        &chapter.dom,
        &chapter.map,
        &Mono,
        range.clone(),
        lines.clone(),
    );
    assert_eq!(
        rects,
        rects_for_range(&chapter.layout, &chapter.dom, &chapter.map, &Mono, range),
        "省了行却换了答案"
    );
    assert!(!rects.is_empty(), "这段选区本该有底色");
}

/// 两端对齐后字被摊开：**命中与选区都要跟着摊开走**。最后一个字的选中矩形右缘要贴住
/// 撑开后的行右缘（`line.x + ink_width`），而不是停在未撑开的 `width` 上 —— 这正是
/// 「接住一半」会露馅的地方：排版摊开了、命中/选区不摊，划词就短一截。
#[test]
fn justification_moves_the_selection_rect_to_the_stretched_edge() {
    // 一行 10 个汉字（16px/字），版心 170px → 余 10px 摊到 9 处字间。
    let chapter = typeset(
        &format!("<p>{}</p>", cjk(20)),
        "p { text-align: justify; text-indent: 0; margin: 0 }",
        170.0,
    );
    let line = &chapter.layout.lines[0];
    assert!(line.justify.is_some(), "首行该两端对齐");
    // 首行第 10 个字（偏移 9）的矩形。
    let rects = rects_for_range(&chapter.layout, &chapter.dom, &chapter.map, &Mono, 9..10);
    assert_eq!(rects.len(), 1, "单字一行一条");
    let rect = &rects[0];
    let stretched_right = line.x + line.ink_width();
    assert!(
        (rect.x + rect.width - stretched_right).abs() < 1.0,
        "最后一个字该贴住撑开的右缘 {stretched_right}，实际 {}",
        rect.x + rect.width
    );
    assert!(
        rect.x + rect.width > line.x + line.width,
        "没有两端对齐时右缘会停在 {}，对齐后才到 {}",
        line.x + line.width,
        rect.x + rect.width
    );
    // 命中也要跟着摊开走：最后一个字（第 10 个字）被推到 [154, 170]，它的中点在 162。
    // 点 158（左半边）落在它上面；没有两端对齐时它的框是 [144, 160]，158 会落到右半边、
    // 命中「它之后」的插入点（偏移 10）—— 这一对坐标正是「摊开与不摊开」的分界。
    let y = line.y + line.height * 0.5;
    let hit = hit_test(&chapter.layout, &chapter.dom, &chapter.map, &Mono, 158.0, y)
        .expect("正文行应命中");
    assert_eq!(hit.offset, 9, "158 在最后一个字的左半边，命中它自己");
    let past_edge = hit_test(
        &chapter.layout,
        &chapter.dom,
        &chapter.map,
        &Mono,
        stretched_right - 1.0,
        y,
    )
    .expect("正文行应命中");
    assert_eq!(past_edge.offset, 10, "贴右缘是「最后一个字之后」的插入点");
}
