//! E1/E2/E3 块级 + 行内 + 图片布局。
//! 几何用一份「汉字 1 em、拉丁 0.5 em」的 mock `Measure` 来断言，
//! 真实字体度量在 app 侧接入，逻辑同一。只依赖公开 API。

use crate::css::computed::{Color, ComputedStyle, Display, cascade};
use crate::css::{Origin, RuleSet};
use crate::dom::{Dom, NodeId};
use crate::layout::wrap::collect;
use crate::layout::{
    BlockBox, BlockCache, FontKey, ImageProvider, Layout, LayoutCache, LayoutKey, LayoutOptions,
    LineFragment, LineMetrics, METRICS_REV, Measure, NoImages, ResolvedImage, RunStyle, StyleArena,
    layout, layout_cached, layout_with_blocks, measure_chapter, render_window,
};

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
    fn char_width(&self, ch: char, _font: &FontKey, font_size: f32) -> f32 {
        char_width(ch, font_size)
    }

    fn run_metrics(&self, text: &str, _font: &FontKey, font_size: f32) -> LineMetrics {
        LineMetrics {
            width: slice_width(text, font_size),
            ascent: font_size * 0.8,
            descent: font_size * 0.2,
        }
    }
}

fn parse(html: &str, css: &str) -> (Dom, Vec<ComputedStyle>) {
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
    let laid = layout(
        &dom,
        &styles,
        find(&dom, "body"),
        LayoutOptions {
            width,
            light: false,
        },
        &Mono,
        &NoImages,
    );
    (dom, laid)
}

fn texts(layout: &Layout) -> Vec<String> {
    (0..layout.lines.len())
        .map(|i| layout.line_text(i))
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

/// 词级断行（稀疏的拉丁文走这条路）：断点把硬行切成「词」，整词判定「装不下就
/// 挪到下一行」。拉丁 0.5em、空格 0.25em、字号 16 ⇒ 字母 8px、空格 4px，版心 100px。
/// `alpha beta` 宽 84px，再加 ` gamma` 会超，于是整个 `gamma` 挪行 —— 这与逐字符
/// 实现逐行一致（词首本身就是断点）。
#[test]
fn latin_breaks_at_word_boundaries() {
    let (_, layout) = typeset("<p>alpha beta gamma</p>", "", 100.0);
    assert_eq!(texts(&layout), ["alpha beta", "gamma"]);
}

/// 行首的「超长词」（单个词就宽过整行）退回逐字符硬切：切成装得下的段，
/// 一个字都不丢。前导空格只被行尾裁掉，不是内容丢了。
#[test]
fn a_word_wider_than_the_line_is_hard_cut() {
    let (_, layout) = typeset("<p>hi supercalifragilisticexpialidocious ok</p>", "", 100.0);
    assert_eq!(
        texts(&layout),
        ["hi", "supercalifra", "gilisticexpi", "alidocious ok"],
        "超长词该被硬切成装得下的段"
    );
    assert_eq!(
        texts(&layout).concat().replace(' ', ""),
        "hisupercalifragilisticexpialidociousok",
        "裁掉的只是行尾那个空格，字符一个都没丢"
    );
}

/// `hyphens` 是连字符唯一那条能一眼看出来的声明：法语的 `je-ne-sais-quoi`
/// 在 `none` 下不许从连字符处断行。开着时第一个连字符是断点，关掉后第一行
/// 不能以连字符结尾 —— 但内容一个字都不能丢（排不下就硬切是我们既有的兜底，
/// 不是浏览器那种溢出）。
#[test]
fn hyphens_none_removes_the_hyphen_break_opportunity() {
    let html = "<p>je-ne-sais-quoi</p>";
    let (_, open) = typeset(html, "p { font-size: 20px; hyphens: manual }", 70.0);
    assert_eq!(texts(&open), ["je-ne-", "sais-", "quoi"], "连字符处该断");

    let (_, shut) = typeset(html, "p { font-size: 20px; hyphens: none }", 70.0);
    let lines = texts(&shut);
    assert_eq!(lines.concat(), "je-ne-sais-quoi", "拦掉断点不能丢字");
    assert!(
        !lines[0].ends_with('-'),
        "`none` 之后连字符不该再是断点：{lines:?}"
    );
}

/// 中文与这条无关：`hyphens: none` 只关连字符。逐字断行照旧，
/// 破折号（词分隔符）也照断 —— 拦掉它等于把整段中文的折行能力砍一半。
#[test]
fn hyphens_none_leaves_cjk_breaking_alone() {
    for html in [
        "<p>文文文文文</p>",
        "<p>文—文文文</p>",
        "<p>「引号」里的话</p>",
        "<p>第3章：开端</p>",
    ] {
        let (_, open) = typeset(html, "", 40.0);
        let (_, shut) = typeset(html, "p { hyphens: none }", 40.0);
        let lines = texts(&shut);
        assert!(lines.len() > 1, "{html} 该被折开，否则这条断言是空的");
        assert_eq!(texts(&open), lines, "{html} 中文不该受 hyphens 影响");
    }
}

/// 首字下沉：首字符自成一个片段站在第一行的行首，正文从它右边开始；它压着的那几行
/// 让开同一格宽，之后的行回到版心左缘。基线**不许**被那个大字推下去 ——
/// 那是 `float` 在 CSS 里的语义，也是我们这一格要的效果。
#[test]
fn drop_cap_hangs_the_first_letters_box_over_the_following_lines() {
    let html = format!("<p>{}</p>", "字".repeat(40));
    let (_, plain) = typeset(&html, "p { font-size: 20px }", 200.0);
    let (dom, laid) = typeset(
        &html,
        "p { font-size: 20px } p::first-letter { font-size: 3em }",
        200.0,
    );
    // 3em 的那个字：宽 60px、上下 extent 60px，行盒高 20×1.75=35 ⇒ 跨 2 行。
    let cap_w = 60.0f32;
    let first = &laid.lines[0];
    assert_eq!(first.fragments.len(), 2, "首字符 + 正文两个片段");
    let cap = match &first.fragments[0] {
        LineFragment::Text(run) => run,
        _ => panic!("首字符该是文本片段"),
    };
    assert_eq!(cap.norm, 0..3, "那个字在段里的字节区间");
    assert_eq!(cap.style.font_size, 60.0);
    assert_eq!(cap.width, cap_w);
    // 锚点仍落在真的文本节点上：`orig` 是源节点里的区间，选择与批注都靠它。
    assert!(dom.get(cap.node).tag.is_none(), "首字符的节点该是文本节点");
    assert_eq!(cap.orig, 0..3);

    assert_eq!(first.x, 0.0, "首字符从版心左缘起");
    assert_eq!(
        first.baseline, plain.lines[0].baseline,
        "首字符不许把这一行的基线推下去"
    );
    assert_eq!(laid.lines[1].x, cap_w, "第二行让开首字符那一格");
    assert_eq!(laid.lines[2].x, 0.0, "跨过的行数之外回到左缘");
    // 让开一格之后每行少装 3 个字（60px / 20px）。
    assert_eq!(texts(&plain)[0].chars().count(), 10);
    assert_eq!(texts(&laid)[1].chars().count(), 7);
    // 内容一个字都不能丢。
    assert_eq!(texts(&laid).concat(), "字".repeat(40));
}

/// 首行缩进与下沉同时存在时，两者叠着走：首字符从缩进之后起，被压着的行
/// 让开的只是首字符那一格（缩进只作用于第一行）。
#[test]
fn drop_cap_and_text_indent_stack() {
    let html = format!("<p>{}</p>", "字".repeat(40));
    let (_, laid) = typeset(
        &html,
        "p { font-size: 20px; text-indent: 2em } p::first-letter { font-size: 3em }",
        200.0,
    );
    assert_eq!(laid.lines[0].x, 40.0, "首行让出缩进");
    assert_eq!(laid.lines[1].x, 60.0, "第二行只让出首字符那一格");
}

/// 没写 `::first-letter` 的段落一行都不该让 —— 下沉必须是显式声明换来的。
#[test]
fn no_cap_without_the_pseudo_element_rule() {
    let html = "<p>首字很大的一段话，这里没有声明。</p>";
    let (_, laid) = typeset(html, "p { font-size: 20px }", 200.0);
    assert!(
        laid.lines.iter().all(|line| line.fragments.len() == 1),
        "每行都该只有一个正文片段"
    );
    assert!(laid.lines.iter().all(|line| line.x == 0.0));
}

/// 开头是标点时，CSS 把紧贴首字母的标点一起算进首字符盒 ——
/// `「開」` 那种书名号开头，引号要跟着大字走，否则第一个引号是正常号、第二个是三号字。
#[test]
fn cap_box_includes_the_punctuation_before_the_letter() {
    let html = "<p>「開」門見山的一段话。</p>";
    let (_, laid) = typeset(
        html,
        "p { font-size: 20px } p::first-letter { font-size: 2em }",
        200.0,
    );
    let cap = match &laid.lines[0].fragments[0] {
        LineFragment::Text(run) => run,
        _ => panic!("首字符该是文本片段"),
    };
    assert!(
        texts(&laid)[0].starts_with("「開"),
        "引号该在首字符盒里：{:?}",
        texts(&laid)[0]
    );
    assert_eq!(cap.norm, 0.."「開".len(), "下沉盒含前面的标点");
    assert_eq!(cap.style.font_size, 40.0);
}

/// 数字开头也下沉，而且只下沉那一个数字单元：UAX #29 的「首字母单元」把数字算在内，
/// 浏览器给 `1984` 的 `::first-letter` 上的就是那个 `1`。
#[test]
fn cap_takes_a_leading_digit_as_its_first_letter_unit() {
    let (_, laid) = typeset(
        "<p>1984 那一年</p>",
        "p { font-size: 20px } p::first-letter { font-size: 2em }",
        200.0,
    );
    let cap = match &laid.lines[0].fragments[0] {
        LineFragment::Text(run) => run,
        _ => panic!("该有首字符盒"),
    };
    assert_eq!(cap.norm, 0..1, "只吃那一个数字，剩下的照正常流程");
    assert_eq!(cap.style.font_size, 40.0);
    assert_eq!(
        texts(&laid).concat(),
        "1984 那一年",
        "摘出去的那一格不能丢正文"
    );
}

/// 整段没有字母单元（纯标点）时不造首字符盒 —— 给一个破折号做 3em 的下沉
/// 在排版上没有任何意义。
#[test]
fn no_cap_when_there_is_no_letter_unit() {
    let (_, laid) = typeset(
        "<p>——</p>",
        "p { font-size: 20px } p::first-letter { font-size: 3em }",
        200.0,
    );
    assert_eq!(laid.lines[0].fragments.len(), 1, "纯标点不该造下沉盒");
    assert_eq!(texts(&laid).concat(), "——");
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
    let paras: Vec<&BlockBox> = layout
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
    assert_eq!(layout.lines[0].dominant_font_size(), 32.0, "UA h1 = 2em");
    assert_eq!(layout.lines[0].x, 68.0, "两字 64px 居中于 200px 版心");

    let (dom, styles) = parse("<h1>x</h1><span>y</span>", "");
    let h1 = styles.get(find(&dom, "h1") as usize).unwrap();
    assert_eq!(h1.font_weight, 700);
    assert_eq!(h1.display, Display::Block);
    let span = styles.get(find(&dom, "span") as usize).unwrap();
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
    let runs: Vec<_> = layout.lines[0].text_runs().collect();
    assert_eq!(runs.len(), 3, "三个文本节点各一段");
    let recovered: Vec<String> = runs
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

/// U+3000（全角空格）是**内容**不是空白：EasyPub 一类工具拿它做段首缩进，
/// CSS 的折叠只管 ASCII 空白，浏览器原样渲染。吞掉它 = 整本书的段首缩进消失
/// （真机实测《斗战狂潮》）。
#[test]
fn ideographic_spaces_survive_collapsing() {
    let (_, layout) = typeset("<p>\u{3000}\u{3000}辛巴的表情</p>", "", 600.0);
    assert_eq!(
        layout.line_text(0),
        "\u{3000}\u{3000}辛巴的表情",
        "段首全角空格必须原样保留"
    );
    // 它们占宽度：行宽 = 两个全角空格 + 五个字。
    assert!(
        (layout.lines[0].width - 7.0 * 16.0).abs() < 0.5,
        "实际 {}",
        layout.lines[0].width
    );
    // 段中出现的全角空格同样不折叠、不并成 ASCII 空格。
    let (_, layout) = typeset("<p>前\u{3000}\u{3000}后</p>", "", 600.0);
    assert_eq!(layout.line_text(0), "前\u{3000}\u{3000}后");
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

/// 段首段尾空白被吞掉、中间并成一个空格 —— 现在从**行盒文本**这一层断言。
#[test]
fn collection_trims_paragraph_edges() {
    let (_, layout) = typeset("<p>  甲 乙  </p>", "", 200.0);
    assert_eq!(
        layout.line_text(0),
        "甲 乙",
        "段首段尾空白吞掉，中间并成一个"
    );
}

// ---------------------------------------------------------------------------
// E2：一个段落里的多种样式
// ---------------------------------------------------------------------------

/// 局部字号与颜色必须活到行盒里：这是「多 run」存在的全部理由。
#[test]
fn a_paragraph_keeps_per_run_size_weight_and_color() {
    let html = "<p>正文<small>小字</small>正文</p>";
    let css = "p { font-size: 20px } small { font-size: 12px; color: #808080 }";
    let (_, layout) = typeset(html, css, 400.0);
    let runs: Vec<_> = layout.lines[0].text_runs().collect();
    assert_eq!(runs.len(), 3);
    assert_eq!(runs[0].style.font_size, 20.0);
    assert_eq!(runs[1].style.font_size, 12.0, "small 的小字号不该被拉平");
    assert_eq!(runs[2].style.font_size, 20.0);
    assert_eq!(runs[1].style.color, Some(Color(128, 128, 128)));
    assert_eq!(runs[0].style.color, None);
    // 行宽是各 run 实际绘制宽度之和：12px 的两个汉字只占 24px。
    assert_eq!(layout.lines[0].width, 20.0 * 4.0 + 12.0 * 2.0);
}

#[test]
fn bold_and_italic_survive_as_run_styles() {
    let (_, layout) = typeset("<p>甲<strong>乙</strong><em>丙</em></p>", "", 400.0);
    let runs: Vec<_> = layout.lines[0].text_runs().collect();
    assert_eq!(runs[1].style.font.weight, 700, "UA 的 b/strong 加粗");
    assert!(runs[2].style.font.italic, "UA 的 em 斜体");
    assert_eq!(runs[0].style.font.weight, 400);
}

/// 行盒高取各 run 的盒（含自己的半 leading）的并集，而不是第一段的。
#[test]
fn line_box_height_follows_the_tallest_run() {
    let (_, layout) = typeset(
        "<p>正文<span class=big>大字</span></p>",
        "p { font-size: 20px } .big { font-size: 40px }",
        400.0,
    );
    assert_eq!(layout.lines[0].height, 40.0 * 1.75);
    // 基线不在最大 ascent 上：每个行内盒先分到自己的半 leading，再按共同基线对齐。
    // 大字那一份是 ascent 32 + (70 − 40)/2 = 47，正文那份是 16 + 7.5 = 23.5，取大的。
    assert_eq!(
        layout.lines[0].baseline, 47.0,
        "基线 = 各 run 上方 extent 的最大值"
    );
}

/// 边框与内边距一起从可用宽里扣掉，而 `BlockBox` 记的是**边框盒** —— 画底色与边框
/// 要用外层那一圈，正文与折行要用扣完之后的内容盒，两者必须是同一个盒模型算出来的。
#[test]
fn borders_and_padding_inset_the_content_box() {
    let (dom, layout) = typeset(
        "<div class=t><p>正文</p></div>",
        ".t { border-width: 0px 0px 0px 20px; border-style: none none none solid; padding: 10px 5px 10px 5px }",
        400.0,
    );
    let div = layout
        .blocks
        .iter()
        .find(|b| b.node == find(&dom, "div"))
        .expect("div 的盒");
    assert_eq!(
        (div.x, div.width),
        (0.0, 400.0),
        "外层记的是边框盒，不是内容盒"
    );
    let p = layout
        .blocks
        .iter()
        .find(|b| b.node == find(&dom, "p"))
        .expect("p 的盒");
    // 20px 左边框 + 5px 左内边距 = 正文从 25 起，宽也照这个收窄。
    assert_eq!((p.x, p.width), (25.0, 370.0));
    assert_eq!(layout.lines[0].x, 25.0, "行盒跟着内容盒走");
    // 10 = div 的上内边距（上边框是 0），再加 `<p>` 自己的 UA 上边距 16（1em）。
    assert_eq!(p.y, 26.0, "内容起点在边框盒往里数第二层");
}

/// 中文禁则：行首不能是收尾标点，行尾不能是起句标点。
/// 断点取自 UAX #14，所以这条是「引擎自持断行」换来的能力。
#[test]
fn kinsoku_keeps_punctuation_off_the_line_edges() {
    // 「了不起」后面的右引号必须跟着引号里的字一起留在行尾，不能被挤到下一行行首。
    let text = "于是我们奋力向前逆水行舟被许多人当作「了不起的盖茨比」中译本里那句检验样本";
    let (_, layout) = typeset(&format!("<p>{text}</p>"), "", 160.0);
    let lines = texts(&layout);
    assert!(lines.len() >= 3, "这段应被折成多行");
    for line in &lines {
        let first = line.chars().next().unwrap_or(' ');
        let last = line.chars().last().unwrap_or(' ');
        assert!(
            !"。，、！？」』；：）》".contains(first),
            "行首出现了禁则标点：{line}"
        );
        assert!(!"「『（《".contains(last), "行尾出现了禁则标点：{line}");
    }
}

/// 回归门：行盒高就是作者给的 line-height，**不**被抬到字形的 content area。
///
/// 这里的假度量是 ascent 0.8em + descent 0.2em = 恰好 1em，所以 `line-height: 0.8`
/// 配 20px 字号必须给出 16px 的行盒，相邻两行的行顶也相差 16px —— 字形本体 20px 高，
/// 于是两行压在一起。浏览器画紧行距正是这个样子。真书里 `line-height: 125%` 配
/// Noto Sans CJK（自然盒约 1.44em）之前被抬成 1.44em，整本读起来比实际渲染松一成。
///
/// 顺带钉住半 leading：松行距时上下各分一半（基线不在行顶贴着），紧行距时为负、基线往上收。
#[test]
fn the_line_box_is_the_authors_line_height_even_when_glyphs_overflow() {
    let text = "字".repeat(40);
    let (_, tight) = typeset(
        &format!("<p>{text}</p>"),
        "p { font-size: 20px; line-height: 0.8 }",
        100.0,
    );
    assert!(tight.lines.len() >= 3, "这段应被折成多行");
    for line in &tight.lines {
        assert_eq!(line.height, 16.0, "行盒高 = 字号 × line-height");
        // (16 − 20) / 2 = −2：基线从字形的 16px ascent 上再收 2px。
        assert_eq!(line.baseline, 14.0);
    }
    for pair in tight.lines.windows(2) {
        assert_eq!(
            pair[1].y - pair[0].y,
            16.0,
            "行顶相差一个行盒，而不是字形的高度"
        );
    }

    let (_, loose) = typeset(
        &format!("<p>{text}</p>"),
        "p { font-size: 20px; line-height: 2 }",
        100.0,
    );
    for line in &loose.lines {
        assert_eq!(line.height, 40.0);
        // (40 − 20) / 2 = +10：leading 上下各一半，基线落在 16 + 10。
        assert_eq!(
            line.baseline, 26.0,
            "松行距的字该在行盒里居中，不是顶着上缘"
        );
    }
}

// ---------------------------------------------------------------------------
// E3：图片（替换元素）
// ---------------------------------------------------------------------------

/// 固定尺寸的图片提供者：`src` 原样当 href，尺寸查表。
struct MockImages(Vec<(&'static str, f32, f32)>);

impl ImageProvider for MockImages {
    fn resolve(&self, src: &str) -> Option<ResolvedImage> {
        self.0
            .iter()
            .find(|(k, _, _)| *k == src)
            .map(|(k, w, h)| ResolvedImage {
                href: (*k).to_string(),
                width: *w,
                height: *h,
            })
    }
}

fn typeset_images(html: &str, css: &str, width: f32, images: &dyn ImageProvider) -> (Dom, Layout) {
    let dom = Dom::parse(html.as_bytes());
    let styles = cascade(
        &dom,
        &RuleSet::parse(css, Origin::Author),
        &ComputedStyle::default(),
    );
    let laid = layout(
        &dom,
        &styles,
        find(&dom, "body"),
        LayoutOptions {
            width,
            light: false,
        },
        &Mono,
        images,
    );
    (dom, laid)
}

/// 行内图片：变成图片片段，参与行宽累加，文本照旧在两侧。
#[test]
fn an_inline_image_becomes_a_fragment_and_adds_width() {
    let imgs = MockImages(vec![("pic.png", 60.0, 40.0)]);
    let (_, layout) = typeset_images("<p>前<img src=\"pic.png\">后</p>", "", 200.0, &imgs);
    // 哨兵字符不能漏进可见文本。
    assert_eq!(layout.line_text(0), "前后");
    let img = layout.lines[0].images().next().expect("应有一个图片片段");
    assert_eq!(img.href, "pic.png");
    assert_eq!((img.width, img.height), (60.0, 40.0));
    assert_eq!(
        layout.lines[0].width,
        16.0 + 60.0 + 16.0,
        "汉字各 1em = 16px"
    );
    // 图片底贴基线，行盒还要容下文字 descender，所以 ≥ 图片高。
    assert!(layout.lines[0].height >= 40.0, "行盒要装下图片高度");
    assert_eq!(layout.lines[0].baseline, 40.0, "图片底贴基线");
}

/// 纯图片行不吃首行缩进：一行里只有一张图时，`text-indent` 若照样套上去，
/// 整幅宽的封面图会被推出版心右缘（真书《斗战狂潮》封面页踩过）。
/// 图文混排的首行不受影响：缩进是 CSS 语义，照套。
#[test]
fn an_image_only_line_skips_the_first_line_indent() {
    // 图片独占一行：x 不含 2em 缩进。
    let imgs = MockImages(vec![("cover.jpg", 80.0, 150.0)]);
    let (_, layout) = typeset_images(
        "<p><img src=\"cover.jpg\"></p>",
        "p { text-indent: 2em; font-size: 16px }",
        120.0,
        &imgs,
    );
    assert_eq!(layout.lines.len(), 1);
    assert_eq!(layout.lines[0].x, 0.0, "纯图片行不缩进");
    assert_eq!(layout.lines[0].width, 80.0);

    // 图文混排：首行缩进照套（CSS 语义）。
    let imgs = MockImages(vec![("pic.png", 20.0, 20.0)]);
    let (_, layout) = typeset_images(
        "<p>甲<img src=\"pic.png\"></p>",
        "p { text-indent: 2em; font-size: 16px }",
        200.0,
        &imgs,
    );
    assert_eq!(layout.lines[0].x, 32.0, "图文混排的首行缩进不变");
}

/// 放不下的行内图要整体折到下一行，不能被拆开。
#[test]
fn an_image_wraps_to_the_next_line_intact() {
    let imgs = MockImages(vec![("pic.png", 100.0, 20.0)]);
    let (_, layout) = typeset_images("<p>甲甲甲<img src=\"pic.png\"></p>", "", 100.0, &imgs);
    assert!(layout.lines.len() >= 2);
    assert_eq!(layout.line_text(0), "甲甲甲");
    assert_eq!(layout.lines[0].images().count(), 0, "第一行不该有图");
    assert_eq!(layout.lines[1].images().count(), 1, "图整体落到第二行");
}

/// 块级图片：独立成行，按容器的 text-align 居中。
#[test]
fn a_block_image_is_centered_by_the_container() {
    let imgs = MockImages(vec![("pic.png", 100.0, 50.0)]);
    let (_, layout) = typeset_images(
        "<div><img src=\"pic.png\"></div>",
        "div { text-align: center } img { display: block }",
        200.0,
        &imgs,
    );
    assert_eq!(layout.lines.len(), 1);
    assert_eq!(layout.lines[0].x, 50.0, "(200 - 100) / 2");
    assert_eq!(layout.lines[0].images().next().unwrap().width, 100.0);
}

/// `width` 用 px：按指定宽，高度按纵横比。
#[test]
fn width_in_px_scales_the_height_by_aspect_ratio() {
    let imgs = MockImages(vec![("pic.png", 60.0, 40.0)]);
    let (_, layout) = typeset_images(
        "<p><img src=\"pic.png\"></p>",
        "img { width: 120px }",
        300.0,
        &imgs,
    );
    let img = layout.lines[0].images().next().unwrap();
    assert_eq!((img.width, img.height), (120.0, 80.0));
}

/// `width: 100%`：按包含块内容宽度折算。
#[test]
fn width_percent_resolves_against_the_container() {
    let imgs = MockImages(vec![("pic.png", 60.0, 40.0)]);
    let (_, layout) = typeset_images(
        "<p><img src=\"pic.png\"></p>",
        "img { width: 100% }",
        300.0,
        &imgs,
    );
    let img = layout.lines[0].images().next().unwrap();
    assert_eq!((img.width, img.height), (300.0, 200.0));
}

/// `max-width` 夹小图片，高度按纵横比跟着缩。
#[test]
fn max_width_clamps_and_scales_height() {
    let imgs = MockImages(vec![("pic.png", 60.0, 40.0)]);
    let (_, layout) = typeset_images(
        "<p><img src=\"pic.png\"></p>",
        "img { max-width: 30px }",
        300.0,
        &imgs,
    );
    let img = layout.lines[0].images().next().unwrap();
    assert_eq!(img.width, 30.0);
    assert!((img.height - 20.0).abs() < 0.01);
}

/// 没写 `max-width` 的超宽图也不能溢出容器。
#[test]
fn an_oversized_image_never_overflows() {
    let imgs = MockImages(vec![("big.png", 500.0, 100.0)]);
    let (_, layout) = typeset_images("<p><img src=\"big.png\"></p>", "", 300.0, &imgs);
    let img = layout.lines[0].images().next().unwrap();
    assert_eq!(img.width, 300.0);
    assert!(img.width <= 300.0, "插图宽度不得超过版心");
    assert!((img.height - 60.0).abs() < 0.01);
}

/// 解析不到的图整张跳过，不塌出空盒子、也不留哨兵。
#[test]
fn an_unresolvable_image_is_skipped_cleanly() {
    let imgs = MockImages(vec![]);
    let (_, layout) = typeset_images("<p><img src=\"missing.png\"></p>", "", 300.0, &imgs);
    assert!(layout.lines.is_empty(), "没有文本也没有图，就不该有行盒");
}

// ---- `word-spacing` / `vertical-align` / `text-decoration` 在几何里长什么样 ----
//
// 这三个属性的落差都发生在「样式表已经算出数、排版层没用」这一步，所以断言的必须是
// 行盒的数，而不是 `ComputedStyle` 的数（后者在 `css::computed::tests` 已经钉过）。

#[test]
fn word_spacing_widens_the_line_and_pulls_the_break_earlier() {
    // mock 度量：拉丁 0.5em、空格 0.25em，字号 16 ⇒ 字母 8px、空格 4px。
    let html = "<p>a b c d e f</p>";
    let (_, plain) = typeset(html, "", 100.0);
    assert_eq!(plain.lines.len(), 1, "68px 的内容在 100px 版心里是一行");
    assert_eq!(plain.lines[0].width, 68.0);

    // 每个空格后再加 8px：内容变成 68 + 5 × 8 = 108px，100px 放不下，必须提前折。
    let (_, spread) = typeset(html, "p { word-spacing: 8px }", 100.0);
    assert_eq!(spread.lines.len(), 2, "词距该参与折行，不只是画出来宽一点");
    // 第一行 5 个字母 + 4 个空格（行尾空格被裁掉，所以它的词距也不算）。
    assert_eq!(spread.lines[0].width, 88.0, "5 × 8 + 4 × (4 + 8)");
    assert_eq!(spread.lines[1].width, 8.0);
    assert_eq!(spread.line_text(1), "f");
}

/// 上标要的是两件事：这一行的基线被抬高，行盒高把抬出去的那截也装下。
/// `line-height: 1` 是为了让行盒高真的由字形决定 —— 否则 line-height 的 28px 早就盖住了。
#[test]
fn vertical_align_lifts_the_baseline_and_grows_the_line_box() {
    let html = "<p>文<em class=s>上</em>文</p>";
    let css = "p { line-height: 1 } .s { vertical-align: super }";
    let (_, laid) = typeset(html, css, 200.0);
    assert_eq!(laid.lines.len(), 1);
    let line = &laid.lines[0];
    let shift = 16.0 / 3.0;
    assert!(
        (line.baseline - (12.8 + shift)).abs() < 0.01,
        "基线该是最高那段（被抬过的）ascent，实际 {}",
        line.baseline
    );
    assert!(
        (line.height - (12.8 + shift + 3.2)).abs() < 0.01,
        "行盒高要把上标抬出去的一截装下，否则它会压到下一行，实际 {}",
        line.height
    );
    // 只有那个 run 带偏移，邻居不受影响。
    let shifts: Vec<f32> = line
        .fragments
        .iter()
        .filter_map(|f| match f {
            crate::layout::LineFragment::Text(run) => Some(run.style.baseline_shift),
            crate::layout::LineFragment::Image(_) => None,
        })
        .collect();
    assert_eq!(shifts.len(), 3);
    assert!(shifts[1].abs() - shift < 0.01, "中间那段是 <em>");
    assert_eq!(shifts[0], 0.0);
    assert_eq!(shifts[2], 0.0);
}

/// `sub` 反过来：多吃 descent，基线不动。
#[test]
fn vertical_align_sub_extends_the_descent_without_moving_the_baseline() {
    let (_, laid) = typeset(
        "<p>文<em class=s>下</em>文</p>",
        "p { line-height: 1 } .s { vertical-align: sub }",
        200.0,
    );
    let line = &laid.lines[0];
    assert!((line.baseline - 12.8).abs() < 0.01, "基线仍由正文那段决定");
    assert!(
        (line.height - (12.8 + 3.2 + 4.0)).abs() < 0.01,
        "descent 多出 0.25em（4px），行盒高跟着长，实际 {}",
        line.height
    );
}

/// 装饰线不改变几何（CSS 也是如此），它必须**原样到达 run**，绘制层才有东西可画。
#[test]
fn decorations_reach_the_run_without_changing_geometry() {
    let (_, plain) = typeset("<p>正文<em class=u>下划</em></p>", "", 200.0);
    let (_, marked) = typeset(
        "<p>正文<em class=u>下划</em></p>",
        ".u { text-decoration: underline }",
        200.0,
    );
    let flags = |layout: &Layout| -> Vec<bool> {
        layout.lines[0]
            .fragments
            .iter()
            .filter_map(|f| match f {
                crate::layout::LineFragment::Text(run) => Some(run.style.underline),
                crate::layout::LineFragment::Image(_) => None,
            })
            .collect()
    };
    assert_eq!(flags(&plain), vec![false, false], "没声明就不该有下划线");
    assert_eq!(flags(&marked), vec![false, true]);
    assert_eq!(
        marked.lines[0].height, plain.lines[0].height,
        "装饰线不计入行盒高，两次的几何必须一模一样"
    );
    assert_eq!(marked.lines[0].width, plain.lines[0].width);
}

// ---- light 模式 ----

/// light 模式：只算高，不产出行盒。用于目录/进度条。
#[test]
fn light_mode_produces_no_lines() {
    let dom = Dom::parse("<p>一段足够长的中文正文，用来验证轻量模式只算高不断行。</p>".as_bytes());
    let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default());
    let body = dom
        .elements()
        .find(|(id, _)| dom.tag_name(*id) == Some("body"))
        .map(|(id, _)| id)
        .unwrap_or(dom.document);

    let light = layout(
        &dom,
        &styles,
        body,
        LayoutOptions {
            width: 600.0,
            light: true,
        },
        &Mono,
        &NoImages,
    );
    assert!(light.lines.is_empty(), "light 模式不该产出行盒");
    assert!(!light.blocks.is_empty(), "light 模式仍应产出块");
    assert!(light.height > 0.0, "light 模式应算出高");
}

/// light 模式与完整模式的块高应接近（误差在一个行高以内）。
#[test]
fn light_mode_height_matches_full_within_tolerance() {
    let dom = Dom::parse("<p>第一段正文。</p><p>第二段正文。</p><p>第三段正文。</p>".as_bytes());
    let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default());
    let body = dom
        .elements()
        .find(|(id, _)| dom.tag_name(*id) == Some("body"))
        .map(|(id, _)| id)
        .unwrap_or(dom.document);

    let full = layout(
        &dom,
        &styles,
        body,
        LayoutOptions {
            width: 600.0,
            light: false,
        },
        &Mono,
        &NoImages,
    );
    let light = layout(
        &dom,
        &styles,
        body,
        LayoutOptions {
            width: 600.0,
            light: true,
        },
        &Mono,
        &NoImages,
    );
    let diff = (full.height - light.height).abs();
    let line_px = 16.0 * 1.5; // 默认字号 * 行距
    assert!(
        diff < line_px,
        "light 与完整模式的高差应在一个行高以内：full={} light={} diff={}",
        full.height,
        light.height,
        diff
    );
}

// ---- 两阶段排版 ----

/// 语料：30 段纯汉字，段长递增（40..=69），字号整除版心 → 轻量估高与真实折行逐字对上。
fn two_stage_corpus() -> (Dom, NodeId, LayoutOptions) {
    let html: String = (0..30)
        .map(|i| format!("<p>{}</p>", "文".repeat(40 + i)))
        .collect();
    let dom = Dom::parse(html.as_bytes());
    let body = dom
        .elements()
        .find(|(id, _)| dom.tag_name(*id) == Some("body"))
        .map(|(id, _)| id)
        .unwrap_or(dom.document);
    (
        dom,
        body,
        LayoutOptions {
            width: 320.0,
            light: false,
        },
    )
}

/// `measure_chapter` 的高、以及整窗口 `render_window` 的行，都应与一次性完整排版一致。
#[test]
fn two_stage_matches_full_layout() {
    let (dom, body, opts) = two_stage_corpus();
    let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default());

    let full = layout(&dom, &styles, body, opts, &Mono, &NoImages);
    let map = measure_chapter(&dom, &styles, body, opts, &Mono, &NoImages);
    assert!(
        (map.height - full.height).abs() < 0.5,
        "轻量估高应与完整排版一致：light={} full={}",
        map.height,
        full.height
    );

    let all = render_window(&map, &dom, &styles, &Mono, &NoImages, 0.0, map.height + 1.0);
    assert_eq!(
        all.lines.len(),
        full.lines.len(),
        "整窗口的行数应与完整排版一致"
    );
    for (a, b) in all.lines.iter().zip(&full.lines) {
        assert_eq!((a.x, a.y, a.width, a.height), (b.x, b.y, b.width, b.height));
    }
}

/// `render_window` 只排可见窗口：行是完整排版里对应那一段的**原样**，且远少于整章。
#[test]
fn render_window_only_covers_the_visible_blocks() {
    let (dom, body, opts) = two_stage_corpus();
    let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default());
    let full = layout(&dom, &styles, body, opts, &Mono, &NoImages);
    let map = measure_chapter(&dom, &styles, body, opts, &Mono, &NoImages);

    let from = map.height * 0.5;
    let win = render_window(&map, &dom, &styles, &Mono, &NoImages, from, from + 200.0);
    assert!(!win.lines.is_empty(), "窗口里该有行");
    assert!(win.lines.len() < full.lines.len(), "窗口的行应远少于整章");

    // 窗口的首行能在完整排版里找到；其后应与完整排版逐行一致（锚定后 y 不变）。
    let base = full
        .lines
        .iter()
        .position(|l| (l.y - win.lines[0].y).abs() < 0.01)
        .expect("窗口首行应出现在完整排版里");
    assert!(base + win.lines.len() <= full.lines.len());
    for (k, wl) in win.lines.iter().enumerate() {
        let fl = &full.lines[base + k];
        assert_eq!(
            (wl.x, wl.y, wl.width, wl.height),
            (fl.x, fl.y, fl.width, fl.height)
        );
    }
}

// ---- StyleArena ----

/// 相同的样式只占一格 —— 这是去重存在的理由。
#[test]
fn style_arena_deduplicates_identical_styles() {
    let mut arena = StyleArena::new();
    let style = RunStyle::default();
    let a = arena.intern(style.clone());
    let b = arena.intern(style);
    assert_eq!(a, b, "同一份样式两次 intern 该拿到同一个下标");
    assert_eq!(arena.len(), 1, "只该占一格");
}

/// 不同样式各占一格。
#[test]
fn style_arena_keeps_distinct_styles_apart() {
    let mut arena = StyleArena::new();
    // `font` 是 `Arc<FontKey>`，要换字段就整个换一份。
    let bold = RunStyle {
        font: std::sync::Arc::new(FontKey {
            weight: 700,
            ..FontKey::default()
        }),
        ..RunStyle::default()
    };
    let a = arena.intern(RunStyle::default());
    let b = arena.intern(bold);
    assert_ne!(a, b);
    assert_eq!(arena.len(), 2);
    assert_eq!(arena.get(a).unwrap().font.weight, 0);
    assert_eq!(arena.get(b).unwrap().font.weight, 700);
}

/// 字号不同就是不同样式（f32 按位比）。
#[test]
fn style_arena_distinguishes_float_fields() {
    let mut arena = StyleArena::new();
    let big = RunStyle {
        font_size: 20.0,
        ..RunStyle::default()
    };
    let a = arena.intern(RunStyle::default());
    let b = arena.intern(big);
    assert_ne!(a, b, "字号不同不该被并成一份");
}

/// 一章里的样式种类数远小于片段数 —— 这是去重的收益。
#[test]
fn one_paragraph_shares_one_style() {
    let html = "<p>一段<b>粗体</b>与<i>斜体</i>混排的正文。</p>";
    let dom = Dom::parse(html.as_bytes());
    let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default());
    let body = dom
        .elements()
        .find(|(id, _)| dom.tag_name(*id) == Some("body"))
        .map(|(id, _)| id)
        .unwrap_or(dom.document);
    let collected = collect(&[body], &dom, &styles, false, &NoImages, &Mono, 600.0);
    // 三种样式：正文、粗体、斜体。片段数（pieces）至少 3，但样式表明显更小。
    assert!(
        collected.styles.len() <= 4,
        "一段混排只该有几份样式，实际 {}",
        collected.styles.len()
    );
    assert!(
        collected.pieces.len() >= 3,
        "三个文本节点该是至少三个 piece，实际 {}",
        collected.pieces.len()
    );
}

// ---- 宽度表（批量度量）----

/// 宽度表按字节下标存，长度与文本一致；每个字符起点都有一个值。
#[test]
fn the_width_table_covers_every_character() {
    let dom = Dom::parse("<p>abc汉字</p>".as_bytes());
    let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default());
    let body = dom
        .elements()
        .find(|(id, _)| dom.tag_name(*id) == Some("body"))
        .map(|(id, _)| id)
        .unwrap_or(dom.document);
    let collected = collect(&[body], &dom, &styles, false, &NoImages, &Mono, 600.0);
    assert_eq!(collected.widths.len(), collected.text.len());
    // 每个字符起点都该有非零宽度（Mono 对空格也给 0.25em）。
    for (offset, _) in collected.text.char_indices() {
        assert!(
            collected.width_at(offset) > 0.0,
            "字节 {offset} 处的字符宽度不该为 0"
        );
    }
}

/// 宽度表与逐字符度量得出同一个总宽 —— 这是「批处理不改变结果」的判据。
#[test]
fn the_batch_width_matches_per_char_measurement() {
    let dom = Dom::parse("<p>混排 Latin 与汉字。</p>".as_bytes());
    let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default());
    let body = dom
        .elements()
        .find(|(id, _)| dom.tag_name(*id) == Some("body"))
        .map(|(id, _)| id)
        .unwrap_or(dom.document);
    let collected = collect(&[body], &dom, &styles, false, &NoImages, &Mono, 600.0);

    let mut batch = 0.0;
    let mut per_char = 0.0;
    for (offset, ch) in collected.text.char_indices() {
        batch += collected.width_at(offset);
        let style = collected.style_at(offset).expect("每个字符都该有样式");
        per_char += style.advance(&Mono, ch);
    }
    assert!(
        (batch - per_char).abs() < 0.01,
        "批处理与逐字符必须同源：batch={batch} per_char={per_char}"
    );
}

// ---- 缓存与失效键 ----
//
// 那一大片（`LayoutKey` / `LayoutCache` / `BlockCache`）挪到了同目录的 `tests/cache.rs`：
// 三块连着写一份文件读不完，而它们钉的是同一件事——「输入没变就别重算」。
mod cache;
// 排版结果上两张索引（行 → 块、按 y 二分）与接线前线性扫的逐行对照。
mod index;
