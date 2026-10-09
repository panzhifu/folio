//! E1/E2/E3 块级 + 行内 + 图片布局。
//! 几何用一份「汉字 1 em、拉丁 0.5 em」的 mock `Measure` 来断言，
//! 真实字体度量在 app 侧接入，逻辑同一。只依赖公开 API。

use folio_core::css::computed::{Color, ComputedStyle, Display, cascade};
use folio_core::css::{Origin, RuleSet};
use folio_core::dom::{Dom, NodeId};
use folio_core::layout::{
    BlockBox, FontKey, ImageProvider, Layout, LayoutOptions, LineMetrics, Measure, NoImages,
    ResolvedImage, layout,
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
        LayoutOptions { width },
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

/// 行盒高取各 run inline box 的最大值，而不是第一段的。
#[test]
fn line_box_height_follows_the_tallest_run() {
    let (_, layout) = typeset(
        "<p>正文<span class=big>大字</span></p>",
        "p { font-size: 20px } .big { font-size: 40px }",
        400.0,
    );
    assert_eq!(layout.lines[0].height, 40.0 * 1.75);
    assert_eq!(layout.lines[0].baseline, 32.0, "基线取最大 ascent");
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

#[test]
fn a_tiny_line_height_still_cannot_make_lines_overlap() {
    let text = "字".repeat(40);
    let (_, layout) = typeset(
        &format!("<p>{text}</p>"),
        "p { font-size: 20px; line-height: 0.2 }",
        100.0,
    );
    assert!(layout.lines.len() >= 3);
    for pair in layout.lines.windows(2) {
        assert!(
            pair[1].y >= pair[0].y + pair[0].height,
            "行盒不能重叠：line-height 再小也要装下字形本身"
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
        LayoutOptions { width },
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
// 行盒的数，而不是 `ComputedStyle` 的数（后者在 `tests/computed.rs` 已经钉过）。

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
            folio_core::layout::LineFragment::Text(run) => Some(run.style.baseline_shift),
            folio_core::layout::LineFragment::Image(_) => None,
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
                folio_core::layout::LineFragment::Text(run) => Some(run.style.underline),
                folio_core::layout::LineFragment::Image(_) => None,
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
