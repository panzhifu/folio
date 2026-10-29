//! E1/E2/E3 块级 + 行内 + 图片布局。
//! 几何用一份「汉字 1 em、拉丁 0.5 em」的 mock `Measure` 来断言，
//! 真实字体度量在 app 侧接入，逻辑同一。只依赖公开 API。

use crate::content::dom::{Dom, NodeId};
use crate::content::format::css::computed::{
    BorderStyle, Clear, Color, ComputedStyle, Display, Float, cascade,
};
use crate::content::format::css::{Origin, RuleSet};
use crate::layout::wrap::collect;
use crate::layout::{
    BlockBox, BlockCache, FontKey, ImageProvider, JustifyMode, Layout, LayoutKey, LayoutOptions,
    LineFragment, LineMetrics, Measure, NoImages, ResolvedImage, RunStyle, StyleArena, layout,
    layout_with_blocks,
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
    let styles = cascade(&dom, &rules, &ComputedStyle::default(), "");
    (dom, styles.styles.clone())
}

fn find(dom: &Dom, want: &str) -> NodeId {
    dom.elements()
        .find(|(id, _)| dom.tag_name(*id) == Some(want))
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("文档里应有 <{want}>"))
}

fn typeset(html: &str, css: &str, width: f32) -> (Dom, Layout) {
    typeset_with(&Mono, html, css, width)
}

fn typeset_with(measure: &dyn Measure, html: &str, css: &str, width: f32) -> (Dom, Layout) {
    typeset_in(measure, html, css, width, None)
}

/// 两端对齐那一族用例的样式表，中间可以再加一条声明。
///
/// 为什么要一个函数而不是拼字符串：`"…margin: 0 } ; text-justify: none"` 那种拼法
/// 把后一条掉在规则块**外面**，层叠根本收不到它，于是 `text-justify` 的三档测试
/// 全在比默认值 —— 红得毫无信息量。
fn justify_css(extra: &str) -> String {
    let mut css = String::from("p { text-align: justify; text-indent: 0; margin: 0");
    if !extra.is_empty() {
        css.push_str("; ");
        css.push_str(extra);
    }
    css.push(' ');
    css.push('}');
    css
}

/// 同上，多给一格竖轴基准（`LayoutOptions::page_height`）—— `height: NN%` 那一族
/// 只有在这一格是 `Some` 时才折得出数。谁递它、以及竖排那一档为什么还递 `None`，
/// 判据都写在 `geometry.rs` 那一格的注释上。
fn typeset_in(
    measure: &dyn Measure,
    html: &str,
    css: &str,
    width: f32,
    page_height: Option<f32>,
) -> (Dom, Layout) {
    let dom = Dom::parse(html.as_bytes());
    let styles = cascade(
        &dom,
        &RuleSet::parse(css, Origin::Author),
        &ComputedStyle::default(),
        "",
    );
    let laid = layout(
        &dom,
        &styles,
        find(&dom, "body"),
        LayoutOptions {
            width,
            light: false,
            // 夹具里会写 `writing-mode`，所以这两条排版入口都让书的 CSS 说了算。
            allow_vertical: true,
            page_height,
        },
        measure,
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

/// 百分比边距折自**包含块宽**。
///
/// 这一条是《哈利·波特全集》那 86441 条 `margin: 0% 1% 0% 1%` 的落点：以前取值层
/// 判它「不认识」，整条边距静默消失（`css::value::length_px` 拒 `%` 的那一版口径）。
#[test]
fn a_percent_margin_folds_against_the_containing_block() {
    let html = format!("<p>{}</p>", "字".repeat(40));
    let (_, layout) = typeset(&html, "p { margin-left: 10% }", 200.0);
    assert_eq!(layout.lines[0].x, 20.0, "200px 版心的 10% = 20px");
    assert_eq!(
        texts(&layout)[0].chars().count(),
        11,
        "扣掉 20px 只剩 180px，段落要跟着少一字"
    );
}

/// 往上多套一层时，基准换成**父块的内容宽**，不是章的版心宽。
///
/// 这是 `basis` 逐层递下去的唯一证据：折错一层就会得 20px（10% × 200）而不是 18px
/// （10% × 180）。`collect::children` 的 `width` 与 `block` 的 `basis` 只差这一个参数，
/// 混用编译器不会拦，所以拦它的只能是这条。
#[test]
fn a_percent_margin_folds_against_the_parent_content_width() {
    let html = format!("<div><p>{}</p></div>", "字".repeat(40));
    let (_, layout) = typeset(
        &html,
        "div { padding-left: 20px } p { margin-left: 10% }",
        200.0,
    );
    assert_eq!(
        layout.lines[0].x, 38.0,
        "20 + 10% × 180：段落折自 div 的内容宽"
    );
}

/// 负的左外边距进 §10.3.3 那条求解式：`width: auto` 的块因此**变宽**并整体左移，
/// 而不是被夹回 0。
///
/// 这一条是《斗战狂潮》全书目录那 1223 个 `<dt class="tocl2">` 的落点 —— 作者写
/// `margin-left: -20px` 要的就是那 20px 悬挂。取值层认得它、也存得下 `Px(-20)`，
/// 从前在排版期每个解析点被 `.max(0.0)` 归零，于是这条声明在 `css-gap` 那张表上
/// 报「接住了」，屏上却一个字都没挪。
#[test]
fn a_negative_margin_shifts_the_block_out_and_widens_it() {
    let html = format!("<p>{}</p>", "字".repeat(40));
    let (_, layout) = typeset(&html, "p { margin: 0; margin-left: -20px }", 200.0);
    assert_eq!(layout.lines[0].x, -20.0, "整块左移 20px");
    assert_eq!(
        texts(&layout)[0].chars().count(),
        13,
        "200 + 20 = 220px 宽、容 13 字（夹回 0 那一档是 200px、12 字）"
    );
}

/// 相邻竖外边距的折叠规则带负数那两档（CSS 2.1 §10.3.4）：一正一负取「最大正数 +
/// 最小负数」，两个都负取最负的那一个。
///
/// 第三条是**守住旧行为**的那一条：两个正数仍取较大值。段间距不显松靠的就是它，
/// 把折叠式换成相加会让全书每一段都多出一截空。
#[test]
fn negative_vertical_margins_collapse_by_the_css_rule() {
    let html = "<p class=a>甲</p><p class=b>乙</p>";
    let gap = |css: &str| {
        let (_, layout) = typeset(html, css, 200.0);
        let paras: Vec<&BlockBox> = layout
            .blocks
            .iter()
            .filter(|b| !b.text.is_empty())
            .collect();
        assert_eq!(paras.len(), 2, "夹具该排出一前一后两段");
        paras[1].y - (paras[0].y + paras[0].height)
    };
    assert_eq!(
        gap("p { margin: 0 } .a { margin-bottom: -10px } .b { margin-top: 20px }"),
        10.0,
        "正 20 与负 10 折叠成 10"
    );
    assert_eq!(
        gap("p { margin: 0 } .a { margin-bottom: -30px } .b { margin-top: -10px }"),
        -30.0,
        "两个都负取最负那一个：后一段被提到前一段的底下 30px"
    );
    assert_eq!(
        gap("p { margin: 0 } .a { margin-bottom: 12px } .b { margin-top: 20px }"),
        20.0,
        "两个正数仍取较大，不相加"
    );
}

/// 竖百分比高：`page_height` 给得出，`html,body { height:100% }` 这一族才落得出数。
///
/// 这一条钉的是《斗战狂潮》封面页那一整套「浮动楔块 + 负下边距」的垂直居中写法：楔块
/// 拿到页高的一半，减掉自己那 360px 的负下边距，`clear` 把后面的容器推到那一格**之下**。
/// 页不够高（一半不足 360）时 `clear` 不往回推，图就贴在顶上 —— 浏览器同一条规则，
/// 不是我们这侧的降级，所以两档都钉住。
///
/// 它同时是这一整条链唯一的证据：`50%` 折自 `body` 的 100%，`body` 的 100% 折自
/// `page_height`。少传一层，楔块就退回 `auto`（第三档）。
#[test]
fn a_float_wedge_gets_half_the_page_and_pushes_the_cleared_block_down() {
    let css = "html,body { height:100%; margin:0; padding:0 }\n\
               .wedge { float:left; height:50%; margin-bottom:-360px }\n\
               .container { clear:both; height:0em }";
    let html = "<div class=wedge></div><div class=container>封面</div>";
    let top = |page: Option<f32>| {
        let (_, laid) = typeset_in(&Mono, html, css, 600.0, page);
        laid.blocks
            .iter()
            .find(|b| b.text == "封面")
            .map(|b| b.y)
            .expect("容器里那一行该有块")
    };
    assert_eq!(top(Some(1200.0)), 240.0, "1200 的一半 600 减掉 360");
    assert_eq!(
        top(Some(640.0)),
        0.0,
        "页比内容矮时 `clear` 不往回推，容器留在流的原位"
    );
    assert_eq!(
        top(None),
        0.0,
        "没有竖轴基准时楔块按 auto —— 与这一格出现之前逐字相同"
    );
}

/// 百分比高的链**只在父块明确写过 `height` 时**才往下一层传（CSS §10.5）。
///
/// 外层写 `height: 200px`，里层 `height: 50%` 得 100；外层不写而由内容撑高时，里层那个
/// 百分比没有可折的数，按 `auto` 走内容高（假度量下一行 28）。两档都要钉：只钉前一档
/// 的话，「父块由内容撑高也把自己的高传下去」这种看着更聪明的写法能悄悄过关，
/// 而那是改整本书的分页形状 —— 一章里每个段落的百分比高都会跟着变。
#[test]
fn a_percent_height_needs_an_explicit_parent_height() {
    let html = "<div class=a><div class=b>字</div><div class=c>后</div></div>";
    let top = |css: &str| {
        let (_, laid) = typeset_in(&Mono, html, css, 600.0, None);
        laid.blocks
            .iter()
            .find(|b| b.text == "后")
            .map(|b| b.y)
            .expect("后一块该有行")
    };
    assert_eq!(
        top(".a { height: 200px } .b { height: 50% } .c { height: 0 }"),
        100.0,
        "父块写了 200，里层 50% 折成 100"
    );
    assert_eq!(
        top(".b { height: 50% } .c { height: 0 }"),
        28.0,
        "父块没写：里层的百分比按 auto，回到一行的高"
    );
}

/// 《斗战狂潮》封面页那一整套写法，按这本书真实的那一份排一遍。
///
/// CSS 逐字取自它的 `cover.html` 章内那块 `<style>`，`table` 那一族也照抄（包括作者
/// 漏掉的 `td`）。这条要钉的**不是**「表格布局做好了」—— 这里没有表格盒，`table` 与
/// `tr` 就是两个套着的普通块 —— 而是「封面居中本来就不靠表格盒」：楔块拿到页高的一半，
/// 减掉自己那 360px 的负下边距，`clear` 把容器推到那一格之下，720 高的图正好落在页中间。
///
/// 三档一起钉，是因为只有中间那一档会看错：页比图矮时浏览器也不往回拉（`clear` 只往下推），
/// 而滚动那一档没有竖轴基准 —— 那一档要是也跟着变，症状就是同一本书在两种读法下版式不同。
#[test]
fn the_cover_page_wedge_centers_the_image_only_when_the_page_is_tall_enough() {
    let css = "html,body {height:100%; margin:0; padding:0;}\n\
               .wedge {float:left; height:50%; margin-bottom: -360px;}\n\
               .container {clear:both; height:0em; position: relative;}\n\
               table, tr, th {height: 720px; width:100%; text-align:center;}";
    let html = "<div class=\"wedge\"></div><div class=\"container\"><table><tr><td>\
                <img src=\"cover.jpg\" alt=\"封面\" /></td></tr></table></div>";
    let images = MockImages(vec![("cover.jpg", 538.0, 720.0)]);
    let top_of_image = |page: Option<f32>| {
        let (_, laid) = typeset_images_page(html, css, 600.0, page, &images);
        laid.lines
            .iter()
            .find(|line| matches!(line.fragments.first(), Some(LineFragment::Image(_))))
            .map(|line| line.y)
            .expect("封面那张图该排出一行")
    };
    assert_eq!(
        top_of_image(Some(1200.0)),
        240.0,
        "1200 的一半 600 减 360；图本身 720 高，正好铺满 240–960 那一格"
    );
    assert_eq!(
        top_of_image(Some(640.0)),
        0.0,
        "页只有 640 高：往回拉会把图切到页外，浏览器也不这么排"
    );
    assert_eq!(top_of_image(None), 0.0, "滚动档没有竖轴基准，按 auto");
}

/// 内边距的百分比折自包含块宽，不是盒子自己的宽（CSS 规定 `padding` 的百分比分母
/// 是**行内轴**的包含块尺寸）。
#[test]
fn a_percent_padding_folds_against_the_containing_block() {
    let html = format!("<div><p>{}</p></div>", "字".repeat(40));
    let (_, layout) = typeset(&html, "div { padding-left: 10% }", 200.0);
    assert_eq!(layout.lines[0].x, 20.0, "div 的 10% 内边距折自 200px");
    assert_eq!(
        texts(&layout)[0].chars().count(),
        11,
        "内边距真的把段落收窄了"
    );
}

/// 首行缩进的百分比也按包含块宽：外层让掉 50px 之后，`10%` 是 15px 而不是 20px。
#[test]
fn a_percent_text_indent_folds_against_the_containing_block() {
    let html = format!("<div><p>{}</p></div>", "字".repeat(40));
    let (_, layout) = typeset(
        &html,
        "div { padding-left: 50px } p { text-indent: 10% }",
        200.0,
    );
    assert_eq!(layout.lines[0].x, 65.0, "50 + 10% × 150");
    assert_eq!(layout.lines[1].x, 50.0, "缩进只吃首行");
}

/// `<ul>` 的每一项在第一行之外挂一个圆点，而正文一个字都不挪。
///
/// 这一段是列表标记的整条口径：标记挂在**流动之外**（`offset` 是负的，不进 `x` 的累加），
/// 占的是 UA 那句 `ul { padding-left: 2.5em }` 让出来的位置；而行文本与 `text_runs()`
/// 都看不见它 —— 选区、命中、批注锚点那一套坐标是「第几个可见字」，标记的字是生成的，
/// DOM 里没有对应的字节，进过一次就等于整本书的锚点漂移。
#[test]
fn a_list_item_gets_its_bullet_outside_the_text() {
    let (_, layout) = typeset("<ul><li>甲</li><li>乙</li></ul>", "", 200.0);
    assert_eq!(layout.lines.len(), 2);
    for (ix, want) in [(0, "甲"), (1, "乙")] {
        let line = &layout.lines[ix];
        assert_eq!(line.x, 40.0, "2.5em 的左内边距把正文让到 40px");
        let LineFragment::Marker(marker) = &line.fragments[0] else {
            panic!("第 {ix} 行首应是标记片段，实际是 {:?}", line.fragments[0]);
        };
        assert_eq!(marker.text, "•");
        // 度量桩给 `•` 半个字号（8px），间距 0.25 em（4px）→ 挂在行首左 12px。
        assert_eq!(marker.offset, -12.0);
        assert_eq!(layout.line_text(ix), want, "标记不进行文本");
        assert_eq!(line.text_runs().count(), 1, "命中只看得见那一条正文 run");
    }
}

/// 有序列表的计数：`<ol start>` 挪起点，`<li value>` 挪这一项并把续值接在它后面。
#[test]
fn an_ordered_list_counts_start_and_value() {
    let (_, layout) = typeset(
        "<ol start=\"3\"><li>甲</li><li value=\"10\">乙</li><li>丙</li></ol>",
        "",
        200.0,
    );
    let marks: Vec<String> = layout
        .lines
        .iter()
        .filter_map(|line| match line.fragments.first() {
            Some(LineFragment::Marker(marker)) => Some(marker.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(marks, vec!["3.", "10.", "11."]);
}

/// 嵌套列表把类型**改回去**：`ol` 里套 `ul`，子层必须回到圆点。
///
/// 这一条同时钉住两处：`list-style-type` 的可继承（CSS 2.1，`<ol>` 的十进制靠它才落到
/// `<li>`），以及 UA 表给 `ul` 写的那一条 —— 少写后者，整个子树会跟着父层的十进制走。
#[test]
fn a_nested_list_resets_the_marker_type() {
    let (_, layout) = typeset("<ol><li>甲<ul><li>乙</li></ul></li></ol>", "", 400.0);
    let marks: Vec<String> = layout
        .lines
        .iter()
        .filter_map(|line| match line.fragments.first() {
            Some(LineFragment::Marker(marker)) => Some(marker.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(marks, vec!["1.", "•"], "实际 {marks:?}");
    assert_eq!(layout.line_text(1), "乙", "内层那一项的文字没被标记污染");
}

/// `list-style-type: none` 是「一条都不画」，不是「画不出来」。
///
/// 《蛇结》那 51 个 `li` 写的就是这一条：以前我们根本不存在标记，所以输出恰好一致，
/// 那是**假阴性**；标记做了之后，`none` 必须仍然什么都不画，落差表才不是说谎的。
#[test]
fn list_style_type_none_draws_no_marker_while_other_types_do() {
    let (_, circle) = typeset(
        "<ul><li>甲</li></ul>",
        "li { list-style-type: circle }",
        200.0,
    );
    assert!(
        matches!(
            circle.lines[0].fragments.first(),
            Some(LineFragment::Marker(_))
        ),
        "`circle` 该画出标记"
    );
    let (_, none) = typeset(
        "<ul><li>甲</li></ul>",
        "li { list-style-type: none }",
        200.0,
    );
    assert_eq!(none.lines[0].fragments.len(), 1, "只剩正文那一个片段");
    assert_eq!(none.line_text(0), "甲");
}

/// 汉字记数 —— EPUB 3 CSS Profile 点名收录的那一族。
#[test]
fn cjk_ideographic_markers_use_chinese_numerals() {
    let (_, layout) = typeset(
        "<ol><li>甲</li><li>乙</li></ol>",
        "li { list-style-type: cjk-ideographic }",
        200.0,
    );
    let marks: Vec<&str> = layout
        .lines
        .iter()
        .filter_map(|line| match line.fragments.first() {
            Some(LineFragment::Marker(marker)) => Some(marker.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(marks, vec!["一、", "二、"]);
    // 「一、」两个全角字 = 32px，加 4px 间距 → 挂在行首左 36px。
    let LineFragment::Marker(marker) = &layout.lines[0].fragments[0] else {
        panic!("应有标记");
    };
    assert_eq!(marker.offset, -36.0);
}

/// 跨段浮动插图：图**不吃垂直空间**（后面的段从同一处起排），但它旁边那几行要让开图宽。
///
/// 这是《哈利·波特》附录里 `<div class="img1"><img width="20%"></div>` 那一族的形状。
#[test]
fn a_floated_image_shortens_only_the_lines_beside_it() {
    let html = format!(
        "<div class=i><img src=a></div><p>{}</p><p>{}</p>",
        "字".repeat(40),
        "字".repeat(40)
    );
    let images = MockImages(vec![("a", 200.0, 100.0)]);
    let (_, laid) = typeset_images(
        &html,
        ".i { float: left; margin-right: 20px } img { width: 100px }",
        200.0,
        &images,
    );
    let xs: Vec<f32> = laid.lines.iter().map(|l| l.x).collect();
    let lens: Vec<usize> = texts(&laid).iter().map(|t| t.chars().count()).collect();
    assert_eq!(
        lens,
        vec![0, 5, 5, 12, 12, 6, 12, 12, 12, 4],
        "压在图旁边的两行只剩 80px（每行五字），出了那段就回到十二字：{lens:?}"
    );
    assert_eq!(
        xs,
        vec![0.0, 120.0, 120.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        "让开的是图 + 它那 20px 右外边距；第一段没被图推下去，第二段也没有"
    );
}

/// `clear: both` 是唯一一句「别绕了」：这一段整块挪到浮动盒下缘之下。
#[test]
fn clear_both_drops_the_block_below_the_float() {
    let html = format!(
        "<div class=i><img src=a></div><p>短句</p><p class=c>{}</p>",
        "字".repeat(12)
    );
    let css = ".i { float: left; margin-right: 20px } img { width: 100px } .c { clear: both }";
    let wrapped = {
        let images = MockImages(vec![("a", 100.0, 300.0)]);
        let (_, laid) = typeset_images(
            &html.replace(" class=c", ""),
            &css.replace(".c { clear: both }", ""),
            200.0,
            &images,
        );
        laid
    };
    let images = MockImages(vec![("a", 100.0, 300.0)]);
    let (_, cleared) = typeset_images(&html, css, 200.0, &images);
    // 没 clear：第二段紧接着第一段排，仍然压在图旁边（图高 300，还没用完）。
    assert_eq!(wrapped.lines[2].y, 60.0, "第一段 16 + 28 + 折叠的 16");
    assert_eq!(wrapped.lines[2].x, 120.0, "没净空就照旧绕着排");
    // 清了：整段被推到图的下缘（300）那一行才算起始边距。
    assert_eq!(cleared.lines[2].y, 300.0, "`clear` 之后这一段从图底下开始");
    assert_eq!(cleared.lines[2].x, 0.0, "图底下不再让宽");
    assert_eq!(texts(&cleared)[2].chars().count(), 12, "整幅宽十二字");
}

/// 浮动盒的负外边距同样进让位那一圈：`PlacedFloat` 的边距盒宽是 `box_width + ml + mr`。
///
/// 上面那条钉的是 `margin-right: 20px` → 让到 120；这一条钉反号 → 让到 80。
/// 只有负号那一档会走到「夹回 0」与「照算」的分岔上。
#[test]
fn a_negative_float_margin_shrinks_the_clearance() {
    let html = format!("<div class=i><img src=a></div><p>{}</p>", "字".repeat(12));
    let images = MockImages(vec![("a", 200.0, 100.0)]);
    let (_, laid) = typeset_images(
        &html,
        ".i { float: left; margin-right: -20px } img { width: 100px }",
        200.0,
        &images,
    );
    assert_eq!(laid.lines[1].x, 80.0, "图宽 100 减去那 20px 负边距");
    assert_eq!(
        texts(&laid)[1].chars().count(),
        7,
        "让位圈窄了一档，压在图旁边的这行反而多容一字（夹回 0 那一档是六字）"
    );
}

/// 中文转换本的章首下沉写法：`<span class="first">这</span>` 上写的是 `float: left`，
/// 不是 `::first-letter`。《哈利·波特》301 章里有 199 章是它，所以这一种必须认。
///
/// 下沉的字与 `::first-letter` 那一种有一个要紧的区别：它是 DOM 里**真有的字**，
/// 所以它照常进文本流 —— 选中、命中、批注锚点都看得见它（列表标记正相反，
/// 见 [`a_list_item_gets_its_bullet_outside_the_text`]）。
#[test]
fn a_floated_leading_span_becomes_a_drop_cap() {
    let html = format!("<p><span class=f>这</span>{}</p>", "一年的秋天".repeat(6));
    let (_, laid) = typeset(
        &html,
        ".f { font-size: 2em; float: left; line-height: 1 }",
        200.0,
    );
    assert_eq!(laid.lines.len(), 3, "30 字 + 一个下沉字，三行");
    assert_eq!(
        laid.lines[0].fragments.len(),
        2,
        "首行是「下沉的字 + 正文」两个片段"
    );
    assert!(laid.line_text(0).starts_with('这'), "下沉的字仍在文本里");
    let lens: Vec<usize> = texts(&laid).iter().map(|t| t.chars().count()).collect();
    assert_eq!(lens, vec![11, 10, 10], "被压着的两行只剩 168px：{lens:?}");
    assert_eq!(laid.lines[1].x, 32.0, "第二行让开 32px 的下沉字");
    assert_eq!(laid.lines[2].x, 0.0, "第三行回到整幅宽（大字只跨两行）");
}

/// RTL 的列表把标记挂在**右**边：`outside` 说的是起始边之外，不是物理左边。
#[test]
fn a_rtl_list_hangs_its_marker_on_the_start_side() {
    let (_, layout) = typeset("<ul><li>كلمة</li></ul>", "ul { direction: rtl }", 200.0);
    let line = &layout.lines[0];
    let LineFragment::Marker(marker) = &line.fragments[0] else {
        panic!("RTL 的列表项也该有标记");
    };
    assert!(
        marker.offset > 0.0,
        "RTL 的标记要落在行盒之右，实测 {}",
        marker.offset
    );
    assert_eq!(line.text_runs().count(), 1, "文本坐标那一侧不受方向影响");
}

/// `float` / `clear` 的取值表：CSS 3 的 `inline-start` 之类不接，也不降级成 `left`。
#[test]
fn float_and_clear_take_their_physical_keywords() {
    let (dom, styles) = parse("<p></p>", "p { float: right; clear: both }");
    let p = &styles[find(&dom, "p") as usize];
    assert_eq!((p.float, p.clear), (Float::Right, Clear::Both));

    let (dom, styles) = parse("<p></p>", "p { float: inline-start; clear: keep }");
    let p = &styles[find(&dom, "p") as usize];
    assert_eq!(
        (p.float, p.clear),
        (Float::None, Clear::None),
        "认不出的值回到初值，不猜一个作者没写的方向"
    );
}

/// 按 class 找那一块的**边框盒**注册（画背景与边框用的就是这一条）。
fn box_of_class<'a>(dom: &Dom, layout: &'a Layout, want: &str) -> &'a BlockBox {
    let node = dom
        .elements()
        .find(|(id, _)| dom.classes(*id).any(|c| c == want))
        .map(|(id, _)| id)
        .expect("文档里应有带这一 class 的元素");
    layout
        .blocks
        .iter()
        .find(|b| b.node == node && b.border_box)
        .expect("这一块注册了边框盒")
}

/// 作者写了 `width` 的块按那一档收窄，横向余量归 `margin: auto` 均分。
///
/// 块级居中在这一格之前不是「没做」，是那条等式无解：块从不比可用宽窄，`auto` 边距
/// 能分到的余量恒为 0（`css::value` 当年正是按这个把 `auto` 折成 0 的）。三条都照
/// CSS 2.1 §10.3.3：两边 `auto` 才居中，只写一边就贴另一边，两边都不写则留在右边。
#[test]
fn a_written_width_narrows_the_block_and_auto_margins_center_it() {
    let (dom, laid) = typeset(
        "<div class=a>甲</div><div class=b>乙</div><div class=c>丙</div>",
        ".a { width: 100px; margin: 0 auto }\
         .b { width: 100px }\
         .c { width: 100px; margin-left: auto }",
        200.0,
    );
    let a = box_of_class(&dom, &laid, "a");
    assert_eq!((a.x, a.width), (50.0, 100.0), "两边 auto → 各得 50px 余量");
    let b = box_of_class(&dom, &laid, "b");
    assert_eq!(
        (b.x, b.width),
        (0.0, 100.0),
        "两边都不是 auto → 定宽块贴左缘，余量留在右边"
    );
    let c = box_of_class(&dom, &laid, "c");
    assert_eq!(
        (c.x, c.width),
        (100.0, 100.0),
        "只有左边 auto → 整个余量归它，块贴右缘"
    );
}

/// `max-width` 夹的是**折行宽度**，不只是那块矩形 —— 写着它的段落每行只剩六字。
///
/// 这一条正是「按元素排名的落差表」量不到的那一格：声明早就 `Applied`，表上不留痕迹，
/// 而排版层从前不读它。`plans/facts.md` 给那张表写的口径本来就点明了「不含几何影响」，
/// 这几条排版断言就是那一侧的替身。
#[test]
fn a_max_width_shortens_every_line_of_the_paragraph() {
    let html = format!("<p class=mw>{}</p>", "字".repeat(18));
    let (dom, laid) = typeset(&html, ".mw { max-width: 100px; margin: 0 auto }", 200.0);
    let box_ = box_of_class(&dom, &laid, "mw");
    assert_eq!(
        (box_.x, box_.width),
        (50.0, 100.0),
        "100px 定宽在 200px 版心里居中"
    );
    assert_eq!(
        texts(&laid),
        vec!["字".repeat(6), "字".repeat(6), "字".repeat(6)],
        "每行按收窄后的 100px 折，不是按版心 200px"
    );
    assert!(
        laid.lines.iter().all(|l| l.x == 50.0),
        "行盒跟着内容盒走：{:?}",
        laid.lines.iter().map(|l| l.x).collect::<Vec<_>>()
    );
}

/// 作者写的 `height` 定住盒高与游标推进量，**内容溢出可见**。
///
/// 《哈利·波特》每章末尾那道分隔条就是这一档：`<ol class="duokan-footnote-content hr">`
/// 是个**空**列表，上面写着 `width:50%; height:2px; background-color:#F3221D`，全书 120 处。
/// 从前高由内容算就是 0，而 `decor_seeds` 把零高的块整个筛掉 —— 那道红线在屏上并不存在。
/// 这里连装饰一起断言，因为「盒子有了高」与「看得见」是两件事。
#[test]
fn a_written_height_holds_the_box_and_the_next_sibling_starts_below_it() {
    let html = "<ol class=hr></ol><p>注</p>";
    let css = ".hr { margin: 0; padding: 0; width: 50%; height: 2px; background-color: #ff0000 }";
    let dom = Dom::parse(html.as_bytes());
    let styles = cascade(
        &dom,
        &RuleSet::parse(css, Origin::Author),
        &ComputedStyle::default(),
        "",
    );
    let laid = layout(
        &dom,
        &styles,
        find(&dom, "body"),
        LayoutOptions {
            width: 200.0,
            light: false,
            allow_vertical: false,
            page_height: None,
        },
        &Mono,
        &NoImages,
    );
    let bar = box_of_class(&dom, &laid, "hr");
    assert_eq!(
        (bar.x, bar.y, bar.width, bar.height),
        (0.0, 0.0, 100.0, 2.0)
    );
    assert_eq!(
        laid.lines[0].y, 18.0,
        "后面的段从 2px 底下 + 它自己那 1em 上边距起排，而不是从一块红底下面"
    );
    let seeds = laid.decor_seeds(&styles);
    assert_eq!(
        seeds.len(),
        1,
        "这道分隔条该进「要画装饰」那张表：{seeds:?}"
    );
    assert_eq!(
        (seeds[0].x, seeds[0].y, seeds[0].width, seeds[0].height),
        (0.0, 0.0, 100.0, 2.0),
        "画出来的是半宽两像素的一道，不是整幅一堵墙"
    );
}

/// 流式排版里 `height` 与 `max-height` 的**百分比**落空：它们的基准是包含块的高，
/// 而父块的高本来就是由内容算出来的 —— CSS 在同一条规则下也判 `auto`，不是我们偷懒。
#[test]
fn a_percent_height_is_not_a_number_in_flow() {
    let (dom, laid) = typeset(
        "<div class=t>字</div><div class=u>字</div>",
        ".t { height: 100% } .u { max-height: 50% }",
        200.0,
    );
    assert_eq!(
        box_of_class(&dom, &laid, "t").height,
        28.0,
        "一行字由内容撑，不会被百分比撑成整页"
    );
    assert_eq!(
        box_of_class(&dom, &laid, "u").height,
        28.0,
        "`max-height` 同一条"
    );
}

/// `min-width` 夹在 `max-width` **之后**，而且管 `width: auto` 那一档（CSS 2.1 §10.4）。
///
/// 三条各钉一个失败模式：前两条钉**夹序**（写成先 min 后 max，矛盾声明会给 40 而不是
/// 60 —— 与浏览器不一致，且屏上只是「那块窄了一点」，最难查）；第三条钉 `auto` 也被抬，
/// 那块溢出到 200px 版心外面去，这是 CSS 的行为，不是排版 bug。
#[test]
fn a_min_width_wins_over_a_smaller_max_width() {
    let (dom, laid) = typeset(
        "<div class=a>字</div><div class=b>字</div><div class=c>字</div>",
        ".a { width: 100px; max-width: 40px; min-width: 60px } \
         .b { max-width: 40px; min-width: 60px } \
         .c { min-width: 260px }",
        200.0,
    );
    assert_eq!(
        box_of_class(&dom, &laid, "a").width,
        60.0,
        "矛盾声明里 min-width 赢"
    );
    assert_eq!(
        box_of_class(&dom, &laid, "b").width,
        60.0,
        "没写 width 时同一条夹序"
    );
    let c = box_of_class(&dom, &laid, "c");
    assert_eq!(
        (c.x, c.width),
        (0.0, 260.0),
        "`width: auto` 填满 200px 之后仍被抬到 260：溢出到版心外"
    );
}

/// `min-height` 抬「由内容撑高」那一档，夹序同横向（§10.7：与 `max-height` 矛盾时它赢）。
///
/// 三条断言里 `u` 是闸门：把 min/max 两条夹反，`s` 与 `t` 仍然各自对得上，只有 `u` 会
/// 从 40 掉回 20 —— 那一格才是「夹序」本身的证据。
/// 高定住之后**后面的块跟着下移**，这是屏上看得见的那一半（内容照排、溢出可见）。
#[test]
fn a_min_height_holds_a_short_box_and_the_cursor_with_it() {
    let (dom, laid) = typeset(
        "<p class=s>短</p><p class=t>字字字字字字字字</p><p class=u>字</p>",
        ".s { margin: 0; min-height: 90px } \
         .t { margin: 0; min-height: 10px; max-height: 20px } \
         .u { margin: 0; min-height: 40px; max-height: 20px }",
        200.0,
    );
    let s = box_of_class(&dom, &laid, "s");
    let t = box_of_class(&dom, &laid, "t");
    let u = box_of_class(&dom, &laid, "u");
    assert_eq!((s.y, s.height), (0.0, 90.0), "一行字的内容高被抬到 90");
    assert_eq!(
        (t.y, t.height),
        (90.0, 20.0),
        "游标从 90 底下起排；内容 28 被 max-height 夹到 20，比它小的 min-height 不改"
    );
    assert_eq!(
        u.height, 40.0,
        "`max-height: 20` 与 `min-height: 40` 矛盾时 min 赢"
    );
}

/// 轻量模式（只量高，给目录与进度条）与实排必须给同一个数。
///
/// 这条不是重复断言：`children_light` 与 `block_light` 走的是另一条分流，从前它把
/// `width` 当块宽记过（见 `solve_block_width` 的文档），症状是滚动位置对不上屏上的块。
/// 两个 `min-*` 各有一格留在实排那一路而轻路没走，两条高就会岔开。
#[test]
fn the_min_sizing_clamps_agree_between_full_and_light() {
    let html = "<p class=s>短</p><p class=w>字</p>";
    let css = ".s { margin: 0; min-height: 90px } .w { margin: 0; min-width: 260px }";
    let dom = Dom::parse(html.as_bytes());
    let styles = cascade(
        &dom,
        &RuleSet::parse(css, Origin::Author),
        &ComputedStyle::default(),
        "",
    );
    let body = find(&dom, "body");
    let run = |light: bool| {
        layout(
            &dom,
            &styles,
            body,
            LayoutOptions {
                width: 200.0,
                light,
                allow_vertical: false,
                page_height: None,
            },
            &Mono,
            &NoImages,
        )
    };
    let full = run(false);
    let light = run(true);
    assert_eq!(
        box_of_class(&dom, &light, "s").height,
        box_of_class(&dom, &full, "s").height,
        "轻路的这一块高要与实排同一条等式"
    );
    assert_eq!(
        box_of_class(&dom, &light, "w").width,
        box_of_class(&dom, &full, "w").width,
        "轻路的这一块宽要与实排同一条等式"
    );
    // 两格都必须是「走新路径才可能出」的数：夹子被删掉时这两条一起掉回 28 与 200。
    assert_eq!(box_of_class(&dom, &light, "s").height, 90.0);
    assert_eq!(box_of_class(&dom, &light, "w").width, 260.0);
}

/// 作者写了定宽的浮动盒：不必收缩就知道它多宽，于是这一块真的离开常规流。
///
/// `.muen .left { width: 20%; float: left }`（《哈利·波特》13 处）从前落在
/// 「只认整块内容是一张图」那一条之外，整行退回常规流：插图框旁边不再有绕文。
/// 与 [`a_floated_image_shortens_only_the_lines_beside_it`] 同一形状，只是宽的来源换了。
#[test]
fn a_definite_width_float_shortens_the_lines_beside_it() {
    let html = format!("<div class=l>题</div><p>{}</p>", "字".repeat(24));
    let (_, laid) = typeset(
        &html,
        ".l { width: 40px; float: left; margin-right: 20px }",
        200.0,
    );
    assert_eq!(
        laid.lines[1].y, 16.0,
        "浮动盒不吃垂直空间：正文第一段只按自己那 1em 上边距起排，\
         而不是等浮动盒那一档（28px）排完 —— 落回常规流会是 44"
    );
    assert_eq!(laid.lines[1].x, 60.0, "让开的是图那 40px 加它 20px 右边距");
    assert_eq!(
        texts(&laid),
        vec![
            "题".to_string(),
            "字".repeat(8),
            "字".repeat(12),
            "字".repeat(4)
        ],
        "压在它旁边那一行只剩 140px，出了那一档回到十二字"
    );
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

/// 根元素（通常是 `<body>`）**自己的**底色要进装饰表。
///
/// 真书《人类简史》第一部分那页是 `<body class="brownll">` + `background-color: #c4aa87`：
/// 整幅棕底挂在根上，而从前 `layout_inner` 只排 `children`、从不登记根自己，
/// 于是那一层底色整屏丢掉（读者看到的就是「这一页的图片没有背景颜色」）。
#[test]
fn the_root_elements_own_background_reaches_the_decor_table() {
    let dom = Dom::parse("<body class=\"page\"><p>正文</p></body>".as_bytes());
    let styles = cascade(
        &dom,
        &RuleSet::parse(".page { background-color: #c4aa87 }", Origin::Author),
        &ComputedStyle::default(),
        "",
    );
    let laid = layout(
        &dom,
        &styles,
        find(&dom, "body"),
        LayoutOptions::with_width(300.0),
        &Mono,
        &NoImages,
    );
    let root = laid
        .decor_seeds(&styles)
        .into_iter()
        .find(|s| s.x == 0.0 && s.width == 300.0)
        .expect("根那一层要有一条装饰，而不是整屏丢掉");
    assert_eq!(root.background, Some(Color(0xc4, 0xaa, 0x87)));
    assert_eq!((root.x, root.y), (0.0, 0.0), "根铺满整个视口、从页首起");
    assert_eq!(root.height, laid.height, "根底色盖满整章");
}

/// `border-style: double` 的**画法**要一路带到装饰表：绘制层据此拆成两条细线，
/// 丢了它真书 `.biaoti` 的上下双线就退化成一条实线。
#[test]
fn a_double_border_reaches_the_decor_table_with_its_style() {
    let dom = Dom::parse("<body><h2 class=\"t\">章名</h2></body>".as_bytes());
    let styles = cascade(
        &dom,
        &RuleSet::parse(
            ".t { border-top: double 3px #000; border-bottom: double 3px #000 }",
            Origin::Author,
        ),
        &ComputedStyle::default(),
        "",
    );
    let laid = layout(
        &dom,
        &styles,
        find(&dom, "body"),
        LayoutOptions::with_width(300.0),
        &Mono,
        &NoImages,
    );
    let t = laid
        .decor_seeds(&styles)
        .into_iter()
        .find(|s| s.border[0].0 > 0.0 && s.border[2].0 > 0.0)
        .expect("章名那块要有上下边框");
    assert_eq!(
        t.border[0],
        (3.0, Some(Color(0, 0, 0)), BorderStyle::Double)
    );
    assert_eq!(
        t.border[2],
        (3.0, Some(Color(0, 0, 0)), BorderStyle::Double)
    );
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

/// 按**这段字会落到哪张脸**报上下 extent 的假度量：拉丁档 0.8/0.2 em（约 1em 自然盒），
/// 汉字档 1.1/0.3 em（Noto Sans CJK 那种约 1.4em）。这正是真机上「样式声明的是拉丁字族、
/// 汉字按字符回退到另一张脸」的形状。
struct Fallback;

/// 这一档的上下 extent。**判据是码位区间**，与 `foliage` 那边 `char_width` 同一道分界
/// （`0x2e80` 起是 CJK 与假名那一坨），两处认的是同一件事。
fn fallback_extents(text: &str, size: f32) -> (f32, f32) {
    if text.chars().any(|c| (c as u32) >= 0x2e80) {
        (size * 1.1, size * 0.3)
    } else {
        (size * 0.8, size * 0.2)
    }
}

impl Measure for Fallback {
    fn char_width(&self, ch: char, _font: &FontKey, font_size: f32) -> f32 {
        char_width(ch, font_size)
    }

    fn run_metrics(&self, text: &str, _font: &FontKey, font_size: f32) -> LineMetrics {
        let (ascent, descent) = fallback_extents(text, font_size);
        LineMetrics {
            width: slice_width(text, font_size),
            ascent,
            descent,
        }
    }

    fn font_extents(&self, text: &str, _font: &FontKey, font_size: f32) -> (f32, f32) {
        fallback_extents(text, font_size)
    }
}

/// 回归门：**样式声明的字体不是这段字实际用的字体**。
///
/// 中英混排里汉字走的是回退链上另一张脸，那张脸比拉丁脸高一大截；行盒的 extent 只有按
/// **这段文字**去量才拿得到。`Measure::font_extents` 以前不收文本、实现方拿一个 `'x'`
/// 量一次按 `(字体, 字号)` 缓存，于是中文行永远按拉丁高度折 —— 行盒矮过字形、上下沿切字，
/// 而 `LineRun.ascent` 同时是绘制基线与选中矩形的高，错就三处一起错。
///
/// 关掉修复的对照（都验过是红的）：把 `wrap.rs` 那句改成递一个固定串、或干脆不递 `text`，
/// 汉字行就会拿到拉丁档，`baseline` 从 20.0 掉回 18.0、`ascent` 从 22.0 掉回 16.0。
#[test]
fn the_line_box_measures_the_font_the_text_actually_uses() {
    let css = "p { font-size: 20px; line-height: 1.2 }";
    let (_, latin) = typeset_with(&Fallback, "<p>abcdefghij</p>", css, 300.0);
    let (_, han) = typeset_with(&Fallback, "<p>于是我们奋力向前</p>", css, 300.0);

    // 拉丁档：ascent 16 descent 4 → 内容高 20，行盒 24 → 半 leading +2 → 基线 18。
    assert_eq!(latin.lines[0].baseline, 18.0, "拉丁行按拉丁脸的量");
    // 汉字档：ascent 22 descent 6 → 内容高 28 比行盒 24 还高 → 半 leading −2 → 基线 20。
    assert_eq!(
        han.lines[0].baseline, 20.0,
        "汉字行必须按回退脸的量，不是按声明的那张拉丁脸"
    );
    assert_ne!(
        han.lines[0].baseline, latin.lines[0].baseline,
        "两档量出同一个数就是没按文本量"
    );
    // 行盒高仍然是作者给的 24：改的只是基线在盒里的位置（`the_line_box_is_the_authors_line_height…`
    // 那条门不该被这次改动动到，它现在也照样绿）。
    assert_eq!(han.lines[0].height, 24.0);

    // run 自己带的那份 extent 是绘制与选区用的，必须同样是汉字档的那一份。
    let LineFragment::Text(run) = &han.lines[0].fragments[0] else {
        panic!("这一行该是文本 run");
    };
    assert_eq!(run.ascent, 22.0, "run 的 ascent 要拿回退脸的那一份");
    assert_eq!(run.descent, 6.0);
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
    typeset_images_page(html, css, width, None, images)
}

/// 同上，多给一格竖轴基准（`LayoutOptions::page_height`），理由见 [`typeset_in`]。
fn typeset_images_page(
    html: &str,
    css: &str,
    width: f32,
    page_height: Option<f32>,
    images: &dyn ImageProvider,
) -> (Dom, Layout) {
    let dom = Dom::parse(html.as_bytes());
    let styles = cascade(
        &dom,
        &RuleSet::parse(css, Origin::Author),
        &ComputedStyle::default(),
        "",
    );
    let laid = layout(
        &dom,
        &styles,
        find(&dom, "body"),
        LayoutOptions {
            width,
            light: false,
            // 夹具里会写 `writing-mode`，所以这两条排版入口都让书的 CSS 说了算。
            allow_vertical: true,
            page_height,
        },
        &Mono,
        images,
    );
    (dom, laid)
}

/// Calibre 封面页那一族：`<svg viewBox="0 0 563 751"><image xlink:href="cover.jpeg"/></svg>`
/// 要排成**一张占满版心的图**。
///
/// 这一条钉的是「回滑滑不到封面」那个 bug 的地基：只认 `<img>` 的排版在这格上给 0 高，
/// 而 0 高的章在滚动链里根本不存在（`reader/book.rs` 那道 `height() > 0.0` 的筛子），
/// 于是从第二章往上滑，滑到顶就是第二章自己的头。基督山伯爵那本的 `titlepage.xhtml`
/// 就是这个形状，spine 第 0 项。
///
/// 宽按 `width="100%"` 铺满版心、高按 `viewBox` 的比例 —— 这两个数是 SVG 的属性而不是
/// CSS，样式表里查不到，只能从元素上直接读。
/// 整页 `<img>` 裹在一个 `height:0` 的容器里：根盒是 0，图排到盒子外面。
///
/// 《斗战狂潮》（骷髅精灵）的封面页就是这个形状（EasyPub 导出的 `cover.html`：
/// `.container { height: 0em }` 里塞一张 538×720 的整页图）。它钉的是比「认不认
/// `<img>`」更深的一层：行盒明明排出来了，`Layout::height` 却跟着根盒停在 0 ——
/// 而全仓只有这一个数回答「这一章有多长」，于是这一章在滚动链里不存在
/// （`Book::placements` 的 `height() > 0.0`），读者从目录章往回滑，滑到顶是目录章
/// 自己的头，「滑不到封面」。浏览器把溢出算进 `scrollHeight`，这里跟同一条。
#[test]
fn an_image_overflowing_a_zero_height_container_still_counts_as_height() {
    let html = "<div class=\"container\"><img src=\"cover.jpg\" alt=\"封面\"/></div>";
    let imgs = MockImages(vec![("cover.jpg", 538.0, 720.0)]);
    let (_, layout) = typeset_images(html, ".container { height: 0 }", 370.0, &imgs);

    assert_eq!(layout.lines.len(), 1, "整页就这一张图，一行");
    let img = layout.lines[0].images().next().expect("要有图片片段");
    assert!((img.width - 370.0).abs() < 1.0, "宽度铺满版心");
    assert!(
        layout.height > 400.0,
        "溢出的那页要算进章高，实得 {}（根盒 0 不是答案）",
        layout.height
    );
}

#[test]
fn a_calibre_svg_cover_page_lays_out_as_one_picture() {
    let html = "<div><svg xmlns=\"http://www.w3.org/2000/svg\" width=\"100%\" height=\"100%\" \
                viewBox=\"0 0 563 751\" preserveAspectRatio=\"none\">\
                <image width=\"563\" height=\"751\" xlink:href=\"cover.jpeg\"/></svg></div>";
    let imgs = MockImages(vec![("cover.jpeg", 563.0, 751.0)]);
    let (_, layout) = typeset_images(html, "", 563.0, &imgs);

    assert_eq!(layout.lines.len(), 1, "整页就这一张图，一行");
    let img = layout.lines[0].images().next().expect("要有图片片段");
    // `xlink:href` 在 DOM 里存的是去掉前缀的 `href`（`content/dom` 用 `name.local`）。
    assert_eq!(img.href, "cover.jpeg");
    assert_eq!(
        (img.width, img.height),
        (563.0, 751.0),
        "铺满版心 + viewBox 比例"
    );
    assert!(
        (layout.height - 751.0).abs() < 0.5,
        "这一章的高就是那张图的高：{} 而不是 0",
        layout.height
    );
}

/// 图暂时解不出来时，那一格**也要留着**：留了高，那一页才滑得进去。
///
/// 与 `<img>` 那一档的分别是有意的：`<img>` 的尺寸只能从图本身来，取不到就整张跳过；
/// `<svg>` 的比例是它自己的 `viewBox` 给的，不依赖那张图。占位照常产出（哨兵字符 + 宽度表
/// 里有这一格），而 `scene.rs` 查不到那条 `href` 就是**留位不画** —— 那一句本来就是它
/// 对解不出资源的图的规定动作。
#[test]
fn an_unresolvable_svg_cover_still_holds_its_box() {
    let html = "<div><svg width=\"100%\" viewBox=\"0 0 563 751\">\
                <image href=\"cover.jpeg\"/></svg></div>";
    let (dom, layout) = typeset_images(html, "", 563.0, &MockImages(vec![]));

    assert_eq!(layout.lines.len(), 1, "占位照常成行");
    let img = layout.lines[0].images().next().expect("要有那一格");
    assert_eq!(
        (img.width, img.height),
        (563.0, 751.0),
        "没有固有尺寸也要按 viewBox 占位"
    );
    // `href` 交的是原始引用：查不到就是不画，不是画错东西。
    assert_eq!(img.href, "cover.jpeg");
    assert_eq!(dom.tag_name(img.node), Some("svg"), "那一格记在 svg 上");
    assert!(
        (layout.height - 751.0).abs() < 0.5,
        "图解不出来也要占住这一页的高，否则那一章在滚动链里不存在：{}",
        layout.height
    );
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

/// 只写 `height`：宽按纵横比反算。脚注的「注」图就是这条 ——
/// `sup img { height:1em; width:auto }` 以前会得到「固有宽 × 1em」的扁图。
#[test]
fn height_in_px_scales_the_width_by_aspect_ratio() {
    let imgs = MockImages(vec![("note.png", 36.0, 48.0)]);
    let (_, layout) = typeset_images(
        "<p><img src=\"note.png\"></p>",
        "img { height: 24px }",
        300.0,
        &imgs,
    );
    let img = layout.lines[0].images().next().unwrap();
    assert_eq!((img.width, img.height), (18.0, 24.0));
}

/// 脚注编号图在 `<sup>` 里：高度受 1em 限制、宽按比例，并且图片的基线偏移要
/// 进到行盒的 ascent 里 —— 否则它会压到上一行。
#[test]
fn a_superscript_marker_image_is_capped_and_lifted_with_its_line() {
    let imgs = MockImages(vec![("note.png", 36.0, 48.0)]);
    let html = "<p>字<sup><a href=\"#n\"><img src=\"note.png\"></a></sup>字</p>";
    let css = "sup { font-size: 10px; vertical-align: 6px } sup img { height: 10px; width: auto }";
    let (_, layout) = typeset_images(html, css, 300.0, &imgs);
    let img = layout.lines[0].images().next().unwrap();
    assert_eq!(
        (img.width, img.height),
        (7.5, 10.0),
        "宽按 36:48 反算，高受 1em 限制"
    );
    assert_eq!(img.baseline_shift, 6.0, "上标偏移要落在图片本身上");
    assert!(
        img.x_offset > 0.0 && img.advance > img.width,
        "标记图要在前后字之间让出留白：width={} advance={} offset={}",
        img.width,
        img.advance,
        img.x_offset
    );
    let line = &layout.lines[0];
    assert!(
        line.baseline >= img.height + img.baseline_shift,
        "行盒基线之上必须容得下图片高度 + 上标偏移：{} < {}",
        line.baseline,
        img.height + img.baseline_shift
    );
    assert!(
        line.baseline - img.height - img.baseline_shift >= 0.0,
        "图片顶边掉出了行盒顶"
    );
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
            crate::layout::LineFragment::Image(_) | crate::layout::LineFragment::Marker(_) => None,
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
                crate::layout::LineFragment::Image(_) | crate::layout::LineFragment::Marker(_) => {
                    None
                }
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
    let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default(), "");
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
            allow_vertical: false,
            page_height: None,
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
    let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default(), "");
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
            allow_vertical: false,
            page_height: None,
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
            allow_vertical: false,
            page_height: None,
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
    let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default(), "");
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
    let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default(), "");
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
    let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default(), "");
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
// 那一大片（`LayoutKey` / `BlockCache`）挪到了同目录的 `tests/cache.rs`：
// 三块连着写一份文件读不完，而它们钉的是同一件事——「输入没变就别重算」。
mod cache;
// 排版结果上两张索引（行 → 块、按推进轴二分）与接线前线性扫的逐行对照。
mod index;
// 竖排：同一份几何在两根轴上的逐条对照（转置漏一面、接反轴、基线符号错都在这片露）。
mod vertical;

// ---- 方向：RTL 的行盒几何 ----

/// RTL 段落的行盒从**起始边**（右）起笔，LTR 同一份内容从左起笔。
///
/// mock 度量是「拉丁 0.5 em、字号 16」，所以一行占多宽是能算死的数：`abcd` 32 + `ef` 16
/// = 48，版心 200 → LTR 起笔 0，RTL 起笔 152。方向只许改「落在哪儿」，不许改折行与行宽 ——
/// 那两样改了就等于进度与分片跟着变，而读者没换书。
#[test]
fn an_rtl_line_hangs_from_the_start_edge() {
    let html = "<p>abcd<i>ef</i></p>";
    let (_, ltr) = typeset(html, "", 200.0);
    let (_, rtl) = typeset(html, "p { direction: rtl }", 200.0);
    assert_eq!(ltr.lines.len(), rtl.lines.len(), "方向不许改变折行结果");
    assert_eq!(
        ltr.lines[0].width, rtl.lines[0].width,
        "同一行的宽不该随方向变"
    );
    assert_eq!(ltr.lines[0].x, 0.0, "LTR 的起始边就是左");
    assert_eq!(
        rtl.lines[0].x + rtl.lines[0].width,
        200.0,
        "RTL 该贴住版心右缘，实际行盒 {:?}",
        (rtl.lines[0].x, rtl.lines[0].width)
    );
}

/// 一行里的片段按**视觉顺序**排，而每段自己的字节区间仍是逻辑的那一段。
///
/// 反转只做一次、做在这一层：绘制（`ui::reader::scene`）、命中与选中矩形
/// （`content::hit`）都只吃 `LineBox::fragments` 这一份顺序，两处各反一次就又分叉了。
/// 区间不许动 —— 锚点、进度、批注都按它算（`plans/Locator.md`）。
#[test]
fn rtl_reverses_fragment_order_but_not_the_byte_ranges() {
    let html = "<p>abcd<i>ef</i></p>";
    let norms = |laid: &Layout| -> Vec<std::ops::Range<usize>> {
        laid.lines[0]
            .fragments
            .iter()
            .filter_map(|f| match f {
                LineFragment::Text(run) => Some(run.norm.clone()),
                LineFragment::Image(_) | LineFragment::Marker(_) => None,
            })
            .collect()
    };
    let (_, ltr) = typeset(html, "", 200.0);
    let (_, rtl) = typeset(html, "p { direction: rtl }", 200.0);
    let (a, b) = (norms(&ltr), norms(&rtl));
    assert!(
        a.len() >= 2 && b.len() >= 2,
        "这份夹具该给出至少两个片段，实际 {a:?} / {b:?}"
    );
    assert!(a[0].start < a[1].start, "LTR 的视觉序就是逻辑序：{a:?}");
    assert!(b[0].start > b[1].start, "RTL 该反过来：{b:?}");
    let mut from_ltr = a.clone();
    from_ltr.sort_unstable_by_key(|r| r.start);
    let mut from_rtl = b.clone();
    from_rtl.sort_unstable_by_key(|r| r.start);
    assert_eq!(
        from_ltr, from_rtl,
        "两个方向排出来的字节区间必须一模一样（顺序除外）"
    );
}

/// 作者钉死的物理 `left` 在 RTL 里**不跟着翻**，默认那一档（`start`）翻。
///
/// 这一条是 `Start` / `End` 两个变体存在的全部理由：合成一档就分不开这两种作者意图，
/// 而写法上撞对同一个位置的只有 `right`。
#[test]
fn a_physical_alignment_stays_physical_under_rtl() {
    let html = "<p>abcd</p>";
    let (_, left) = typeset(html, "p { direction: rtl; text-align: left }", 200.0);
    assert_eq!(left.lines[0].x, 0.0, "写了 left 就该贴左");
    let (_, start) = typeset(html, "p { direction: rtl }", 200.0);
    assert!(start.lines[0].x > 0.0, "不写就该贴起始边（右）");
    let (_, right) = typeset(html, "p { direction: rtl; text-align: right }", 200.0);
    assert_eq!(
        right.lines[0].x, start.lines[0].x,
        "物理 right 与 RTL 的 start 撞在同一处，这是巧合而不是合并成一档的理由"
    );
    // LTR 那边一克都不许变：这一批改动若动了 LTR 的落位，红的是全部现有排版测试。
    let (_, ltr_start) = typeset(html, "", 200.0);
    assert_eq!(ltr_start.lines[0].x, 0.0);
    let (_, ltr_end) = typeset(html, "p { text-align: end }", 200.0);
    assert_eq!(ltr_end.lines[0].x + ltr_end.lines[0].width, 200.0);
}

/// 首行缩进让的是**起始边**：LTR 让左，RTL 让右，而让完的行宽一模一样。
#[test]
fn text_indent_insets_the_start_side_in_both_directions() {
    let html = "<p>abcd</p>";
    let (_, ltr) = typeset(html, "p { text-indent: 20px }", 200.0);
    assert_eq!(ltr.lines[0].x, 20.0, "LTR 缩左");
    let (_, rtl) = typeset(html, "p { direction: rtl; text-indent: 20px }", 200.0);
    assert_eq!(
        rtl.lines[0].x + rtl.lines[0].width,
        180.0,
        "RTL 缩右：右缘该让出 20，实际 {:?}",
        (rtl.lines[0].x, rtl.lines[0].width)
    );
    assert_eq!(rtl.lines[0].width, ltr.lines[0].width, "让位不该改字宽");
}

// ---- 两端对齐（`text-align: justify`）与字距（`letter-spacing`）----
//
// 这两项都改**行内**的字位：对齐把余量摊到行内的隙上，字距把每个字推宽。它们与
// `x` / `width` 无关（`x` 是行盒左缘、`width` 是未撑开的片段宽之和），所以断言要看
// `ink_width()`（撑开后的墨迹宽）与逐字位置，不能只看 `x` / `width` —— 那正是接通前
// 它们「落了 `_ => 0.0` 却测不出来」的原因。

/// 两端对齐：除末行外每行都摊满版心；末行不对齐（CSS 规定段末行不 justify）。
#[test]
fn a_justified_paragraph_fills_the_measure_except_the_last_line() {
    // 25 个汉字，16px/字、版心 170px → 每行 10 字（160px，余 10px 可摊），共 3 行。
    // 版心特意不是 16 的整数倍：整数倍时每行正好顶满、余量为 0，就看不出「有没有摊开」。
    let html = format!("<p>{}</p>", "汉".repeat(25));
    let width = 170.0;
    let (_, laid) = typeset(
        &html,
        "p { text-align: justify; text-indent: 0; margin: 0 }",
        width,
    );
    assert!(
        laid.lines.len() >= 3,
        "该折成至少三行，实际 {} 行",
        laid.lines.len()
    );
    let last = laid.lines.len() - 1;
    for (i, line) in laid.lines.iter().enumerate() {
        if i == last {
            assert!(line.justify.is_none(), "末行不该对齐：余量留在右边是惯例");
        } else {
            assert!(line.justify.is_some(), "第 {i} 行该两端对齐");
            assert!(
                (line.ink_width() - width).abs() <= 0.5,
                "第 {i} 行该摊满 {width}，实际墨迹宽 {}（未撑开的片段宽 {}）",
                line.ink_width(),
                line.width
            );
        }
    }
}

/// `<br>` 收尾的行不对齐：强制断行的行与段末行同待遇。
#[test]
fn a_line_ended_by_a_br_is_not_justified() {
    // 11 个汉字，版心 100px（16px/字）→ 第一硬行折成 6 + 5 两行，第 2 行是硬行尾；
    // `<br>` 之后 "尾" 自成第三行。
    let html = format!("<p>{}<br>尾</p>", "汉".repeat(11));
    let (_, laid) = typeset(
        &html,
        "p { text-align: justify; text-indent: 0; margin: 0 }",
        100.0,
    );
    assert_eq!(laid.lines.len(), 3, "该是 6 / 5 / 1 三行");
    assert!(laid.lines[0].justify.is_some(), "第一行不是硬行尾，该对齐");
    assert!(laid.lines[1].justify.is_none(), "`<br>` 前那一行不对齐");
    assert!(laid.lines[2].justify.is_none(), "`<br>` 之后那一行不对齐");
}

/// `text-justify: inter-word` 只拉空格，所以**没有空格的中文行一律不对齐**
/// （`layout::collect::justify_plan` 里那一臂给 `(Spaces, 0)`，`gaps == 0` 就返回 `None`）。
///
/// 这一档从前一次没测过：中文书在 `<body>` 上写 `text-justify: inter-word` 的意图是
/// 「别把我的汉字摊开」，而它折错的表现为每一行字间多一道缝 —— 与默认 `auto`
/// 在屏上是两种不同的版式。这里拿 `auto` 当对照：同一份 HTML 与版心，
/// `auto` 摊字间、`inter-word` 一行都不摊。
#[test]
fn inter_word_justification_leaves_a_spaceless_cjk_line_unstretched() {
    let html = format!("<p>{}</p>", "汉".repeat(25));
    // 16px/字、版心 170px → 每行 10 字（160px），余 10px 可摊；末行 5 字是段末，不对齐。
    let (_, auto) = typeset(&html, &justify_css(""), 170.0);
    let last = auto.lines.len() - 1;
    let stretched = auto
        .lines
        .iter()
        .take(last)
        .filter(|line| matches!(line.justify, Some(j) if j.mode == JustifyMode::Chars))
        .count();
    assert!(
        stretched >= 2,
        "默认 `auto` 该把没有空格的行摊在字间，实际摊了 {stretched} 行"
    );

    let (_, word) = typeset(&html, &justify_css("text-justify: inter-word"), 170.0);
    assert_eq!(word.lines.len(), auto.lines.len(), "摊不摊不该改行数");
    for (i, line) in word.lines.iter().enumerate() {
        assert!(
            line.justify.is_none(),
            "第 {i} 行没有空格，`inter-word` 不该产生任何隙"
        );
        assert!(
            (line.ink_width() - line.width).abs() <= 0.01,
            "不摊开的行，墨迹宽应等于未撑开的片段宽"
        );
    }
}

/// `inter-word` 的另一半：有空格时它照样摊空格，且只摊在空格上。
/// 少了这一条对照，上面那个「一行都不摊」也可能只是「两端对齐整条断了」。
#[test]
fn inter_word_stretches_the_spaces_it_finds_and_only_those() {
    // 「汉 」= 16 + 4 = 20px。20 个汉字 19 个空格 → 版心 170 时每行 8 字 7 隙（156px）。
    let html = format!("<p>{}</p>", "汉 ".repeat(20).trim());
    let (_, word) = typeset(&html, &justify_css("text-justify: inter-word"), 170.0);
    assert!(
        word.lines.len() >= 2,
        "该折成至少两行，实际 {} 行",
        word.lines.len()
    );
    let first = &word.lines[0];
    let Some(justify) = first.justify else {
        panic!(
            "有空格的行该由 `inter-word` 摊开，实际没摊（行宽 {}，版心 170）",
            first.width
        );
    };
    assert_eq!(justify.mode, JustifyMode::Spaces, "只拉空格那一档");
    assert_eq!(justify.gaps, 7, "一行 8 个字之间 7 处空格隙");
    assert!((first.ink_width() - 170.0).abs() <= 0.5, "摊满版心");
}

/// `text-justify: none` 关掉摊余量，即使行里有空格。它是「这一处不要两端对齐」，
/// 与 `text-align` 没有 `none` 这个值是两件事。
#[test]
fn text_justify_none_switches_off_the_stretch() {
    let html = format!("<p>{}</p>", "汉 ".repeat(20).trim());
    let (_, on) = typeset(&html, &justify_css(""), 170.0);
    assert!(
        on.lines[0].justify.is_some(),
        "对照：默认该摊开，否则下面那条 `none` 是空过的"
    );
    let (_, off) = typeset(&html, &justify_css("text-justify: none"), 170.0);
    assert!(off.lines[0].justify.is_none(), "`none` 要关掉摊余量");
}

/// 在不是 `<li>` 的元素上写 `display: list-item` 也要真画出标记 ——
/// 这一档是标记的唯一入口，UA 表只给 `<li>`，所以除了这条声明没有别的路能到这里。
#[test]
fn display_list_item_on_a_paragraph_draws_a_marker() {
    let (_, laid) = typeset("<p>项</p>", "p { display: list-item }", 200.0);
    let LineFragment::Marker(marker) = &laid.lines[0].fragments[0] else {
        panic!(
            "<p> 写了 `display: list-item`，行首应是标记片段，实际是 {:?}",
            laid.lines[0].fragments[0]
        );
    };
    assert_eq!(marker.text, "•", "标记类型回 CSS 初值 disc");
    assert_eq!(laid.line_text(0), "项", "标记不进文本");
}

/// 字距：每个字符都推宽，于是同样版心折得更早，run 宽也按字距加。
#[test]
fn letter_spacing_widens_each_character_and_breaks_earlier() {
    let html = format!("<p>{}</p>", "汉".repeat(20));
    let width = 240.0;
    let (_, plain) = typeset(&html, "p { text-indent: 0; margin: 0 }", width);
    let (_, spaced) = typeset(
        &html,
        "p { letter-spacing: 8px; text-indent: 0; margin: 0 }",
        width,
    );
    // 无字距：16px/字 → 每行 15 字；有 8px 字距：24px/字 → 每行 10 字。
    assert_eq!(plain.line_text(0).chars().count(), 15, "无字距每行 15 字");
    assert_eq!(
        spaced.line_text(0).chars().count(),
        10,
        "加 8px 字距每行 10 字（折行更早）"
    );
    // 行宽 = 10 × (16 + 8) = 240 —— 与宽度表同源，折行按它判的。
    assert!(
        (spaced.lines[0].width - 240.0).abs() < 0.01,
        "首行宽该是 10 × 24 = 240，实际 {}",
        spaced.lines[0].width
    );
}

/// `box-sizing: border-box` 时作者写的数**已经含**边框与内边距：横向那一格。
///
/// 两条断言钉两件不同的事，缺一条就是「看着对了」：盒宽只证明矩形折了，而**折行宽**
/// 才证明排版层真按内容盒走 —— 从前有一条改动把矩形改窄而行仍按 100px 折，屏上正文
/// 溢出插图框，报表上什么也没说。
#[test]
fn border_box_folds_padding_and_border_out_of_the_declared_width() {
    let html = format!("<p class=a>{}</p>", "字".repeat(12));

    let (dom, laid) = typeset(
        &html,
        ".a { width: 100px; padding: 0 10px; border: 5px solid black }",
        200.0,
    );
    assert_eq!(
        box_of_class(&dom, &laid, "a").width,
        130.0,
        "初值 content-box：100 是内容宽，边框盒再加左右各 15"
    );
    assert_eq!(
        texts(&laid),
        vec!["字".repeat(6), "字".repeat(6)],
        "按内容宽 100 折：一行 6 字"
    );

    let (dom, laid) = typeset(
        &html,
        ".a { box-sizing: border-box; width: 100px; padding: 0 10px; border: 5px solid black }",
        200.0,
    );
    assert_eq!(
        box_of_class(&dom, &laid, "a").width,
        100.0,
        "border-box：作者写的 100 就是边框盒，内容只剩 70"
    );
    assert_eq!(
        texts(&laid),
        vec!["字".repeat(4), "字".repeat(4), "字".repeat(4)],
        "折行跟着内容盒走，不是跟着矩形走"
    );
}

/// 纵向那一格：`height` 与**游标推进量**一起折，后面的兄弟不许留在原地。
#[test]
fn border_box_folds_the_vertical_pair_out_of_the_declared_height() {
    let html = "<p class=h>字</p><p class=n>后</p>";
    let box_edges = "height: 60px; padding: 10px 0; border: 5px solid black; margin: 0";

    let (dom, laid) = typeset(
        html,
        &format!(".h {{ {box_edges} }} .n {{ margin: 0 }}"),
        200.0,
    );
    let h = box_of_class(&dom, &laid, "h");
    assert_eq!(
        h.height, 90.0,
        "content-box：60 的内容 + 上下各 15 的边距与边框"
    );
    assert_eq!(
        box_of_class(&dom, &laid, "n").y,
        90.0,
        "后面的块从 90 底下起排"
    );

    let (dom, laid) = typeset(
        html,
        &format!(".h {{ box-sizing: border-box; {box_edges} }} .n {{ margin: 0 }}"),
        200.0,
    );
    assert_eq!(
        box_of_class(&dom, &laid, "h").height,
        60.0,
        "border-box：60 含上下各 15，内容只剩 30"
    );
    assert_eq!(
        box_of_class(&dom, &laid, "n").y,
        60.0,
        "推进量跟着折 —— 这一格错了就是滚动位置与屏上的块对不上"
    );
}

/// 浮动盒那一格与流内块共用同一份式子：`border-box` 的 40 就是 40，旁边的行让开 40。
///
/// 这一条之所以要单独钉：浮动盒的宽从前是**另一条**算式（`float_box_width`），
/// 它连 `min-width` 都不看，而 `solve_block_width` 看 —— 两个落点迟早分叉，
/// 症状是同一张插图框在流内与浮在旁边宽窄不同。
#[test]
fn a_border_box_float_lets_the_lines_beside_it_pass_the_authors_number() {
    let html = format!("<div class=l>题</div><p>{}</p>", "字".repeat(24));
    let edges = "width: 40px; padding: 0 8px; border: 2px solid black; float: left";

    let (_, laid) = typeset(&html, &format!(".l {{ {edges} }}"), 200.0);
    assert_eq!(
        laid.lines[1].x, 60.0,
        "content-box：让开的是 40 + 左右各 10"
    );

    let (_, laid) = typeset(
        &html,
        &format!(".l {{ box-sizing: border-box; {edges} }}"),
        200.0,
    );
    assert_eq!(
        laid.lines[1].x, 40.0,
        "border-box：让开的就是作者写的那 40，内容只剩 20"
    );
}

/// `min-width` 在浮动盒那一条也算数（从前只有流内块那一条看它）。
#[test]
fn a_min_width_raises_a_float_the_author_left_undimensioned_enough() {
    let html = format!("<div class=l>题目</div><p>{}</p>", "字".repeat(24));
    let (_, laid) = typeset(
        &html,
        ".l { width: 20px; min-width: 40px; float: left }",
        200.0,
    );
    assert_eq!(
        laid.lines[1].x, 40.0,
        "`min-width` 把浮动盒抬到 40，与流内块同一条夹序"
    );
}

/// 替换元素那一条路上的 `min-*` / `max-*` / `box-sizing`。
///
/// 这一组是直接问 [`image_size`] 本身，不绕排版：它是一张图的**唯一**算式，
/// 而从前 `min-*` 在这里根本不参与 —— 声明在层叠期报了 `Applied`，屏上一点不变，
/// 正是 `plans/facts.md` 记过的那条「Applied ≠ 排版层读了」的缝。
#[test]
fn image_size_folds_the_min_pair_and_the_box_sizing_pair() {
    use crate::content::format::css::computed::{BorderEdge, BorderStyle, Length};
    use crate::layout::ports::image_size;
    let base = ComputedStyle::default();

    // 1) `min-width` 抬一张窄图，另一轴按纵横比跟着走。
    let style = ComputedStyle {
        min_width: Some(Length::Px(300.0)),
        ..base.clone()
    };
    assert_eq!(image_size(&style, 100.0, 50.0, 600.0), (300.0, 150.0));

    // 2) 夹序：与流内块同一条，`max-width` 先夹、`min-width` 后夹，矛盾时 min 赢。
    let style = ComputedStyle {
        max_width: Some(Length::Px(40.0)),
        min_width: Some(Length::Px(60.0)),
        ..base.clone()
    };
    assert_eq!(image_size(&style, 100.0, 50.0, 600.0), (60.0, 30.0));

    // 3) `min-height` 反算宽（正方形，两条轴一起走）。
    let style = ComputedStyle {
        min_height: Some(Length::Px(200.0)),
        ..base.clone()
    };
    assert_eq!(image_size(&style, 100.0, 100.0, 900.0), (200.0, 200.0));

    // 4) 「绝不溢出容器」那一刀仍压在最后，是产品口径：`min-width` 赢不过版心。
    let style = ComputedStyle {
        min_width: Some(Length::Px(900.0)),
        ..base.clone()
    };
    assert_eq!(image_size(&style, 100.0, 50.0, 600.0), (600.0, 300.0));

    // 5) `box-sizing: border-box` 在替换元素上同样把边框与内边距从作者写的数里扣掉。
    let edges = [BorderEdge {
        width: 5.0,
        style: BorderStyle::Solid,
        color: None,
    }; 4];
    let boxed = ComputedStyle {
        width: Some(Length::Px(100.0)),
        padding_left: Length::Px(10.0),
        padding_right: Length::Px(10.0),
        border: edges,
        ..base.clone()
    };
    assert_eq!(
        image_size(&boxed, 400.0, 200.0, 600.0),
        (100.0, 50.0),
        "初值 content-box：100 是内容宽"
    );
    let border_box = ComputedStyle {
        box_sizing: crate::content::format::css::computed::BoxSizing::BorderBox,
        ..boxed
    };
    assert_eq!(
        image_size(&border_box, 400.0, 200.0, 600.0),
        (70.0, 35.0),
        "border-box：100 含左右各 15，内容只剩 70，高按比例算"
    );
}

/// `border-radius` 一路走到装饰表：四角半径在排版期按**这一块的盒尺寸**折成 px。
/// 只有写进装饰表，绘制层才有圆角可画 —— 停在 `ComputedStyle` 里就是又一条「接住了但没画」。
#[test]
fn border_radius_reaches_the_decor_table_in_pixels() {
    let dom = Dom::parse("<body class=\"page\"><p>正文</p></body>".as_bytes());
    let styles = cascade(
        &dom,
        &RuleSet::parse(
            ".page { background-color: #c4aa87; border-radius: 6px }",
            Origin::Author,
        ),
        &ComputedStyle::default(),
        "",
    );
    let laid = layout(
        &dom,
        &styles,
        find(&dom, "body"),
        LayoutOptions::with_width(300.0),
        &Mono,
        &NoImages,
    );
    let root = laid
        .decor_seeds(&styles)
        .into_iter()
        .find(|s| s.x == 0.0 && s.width == 300.0)
        .expect("根那一层要有一条装饰");
    assert_eq!(
        root.radius, [6.0; 4],
        "六像素的圆角要原样带到装饰表（盒高远大于 12px，不会被封顶）"
    );
}

/// 只写圆角、不写底色与边框的块不该因此多出装饰种子：圆角本身不画任何东西，
/// 凭空多一条会让装饰表在每一屏都多走一遍。
#[test]
fn a_bare_border_radius_does_not_create_a_decor_seed() {
    let dom = Dom::parse("<body><p>正文</p></body>".as_bytes());
    let styles = cascade(
        &dom,
        &RuleSet::parse("p { border-radius: 8px }", Origin::Author),
        &ComputedStyle::default(),
        "",
    );
    let laid = layout(
        &dom,
        &styles,
        find(&dom, "body"),
        LayoutOptions::with_width(300.0),
        &Mono,
        &NoImages,
    );
    assert!(
        laid.decor_seeds(&styles).is_empty(),
        "没有底色与边框的块不该进装饰表：{:?}",
        laid.decor_seeds(&styles)
    );
}

// ---- 表格（`display: table` 一族）----
//
// 度量桩：汉字 16px、空格 4px。列宽分配与 `layout::table::solve_columns` 同源，
// 所以这里的数都是能笔算出来的，不是「跑一下看看」。

/// 一格里没有别的东西时，格子的边框盒就是它自己的落点与尺寸。
fn cells(dom: &Dom, laid: &Layout) -> Vec<(f32, f32, f32, f32)> {
    laid.blocks
        .iter()
        .filter(|b| b.border_box && matches!(dom.tag_name(b.node), Some("td") | Some("th")))
        .map(|b| (b.x, b.y, b.width, b.height))
        .collect()
}

/// 两行两列：格子**并排**（不是一列串下来），第二行落在第一行的下缘。
///
/// 版心 200、四格的固有宽都是 32（两字），所以两列各得 100 —— 摊满可用宽。
/// 一行一个字高 28（16 × 1.75），两行 56。
#[test]
fn a_two_by_two_table_puts_cells_side_by_side() {
    let html = "<table><tr><td>甲乙</td><td>丙丁</td></tr>\
                <tr><td>戊</td><td>己</td></tr></table>";
    let (dom, laid) = typeset(html, "", 200.0);
    assert_eq!(
        cells(&dom, &laid),
        vec![
            (0.0, 0.0, 32.0, 28.0),
            (32.0, 0.0, 32.0, 28.0),
            (0.0, 28.0, 32.0, 28.0),
            (32.0, 28.0, 32.0, 28.0),
        ],
        "每列 32（两字）宽、两行各 28 高"
    );
    // `width: auto` 的表**收缩到内容宽**（两列各 32 = 64），不铺满 200 的版心。
    let table = laid
        .blocks
        .iter()
        .find(|b| b.border_box && dom.tag_name(b.node) == Some("table"))
        .expect("表自己也该有一条盒子（底色与边框画在它上面）");
    assert_eq!((table.width, table.height), (64.0, 56.0));
}

/// 行高取**最高**那一格：矮格子把自己的盒子拉到行高，于是同一行的边框与底色齐平。
///
/// 表固定 100 宽：两列按 `min + (max-min)×t` 插值 —— 左边 84（10 个汉字折成两行）、
/// 右边 16（一个字一行）。行高 = 56，右边那一格的盒子也要是 56，不是它自己的 28。
#[test]
fn a_short_cell_stretches_to_the_row_height() {
    let html =
        "<table style=\"width:100px\"><tr><td>甲乙丙丁戊己庚辛壬癸</td><td>短</td></tr></table>";
    let (dom, laid) = typeset(html, "", 600.0);
    assert_eq!(
        cells(&dom, &laid),
        vec![(0.0, 0.0, 84.0, 56.0), (84.0, 0.0, 16.0, 56.0)],
        "矮的那一格被拉到行高 56"
    );
}

/// 作者写在格子上的百分比宽直接决定列宽（HP 的 `th{width:25%}` / `td{width:75%}` 就是这一档）。
#[test]
fn cell_percentage_widths_become_the_column_widths() {
    let html = "<table style=\"width:200px\"><tr>\
                <th style=\"width:25%\">甲</th><td style=\"width:75%\">乙</td></tr></table>";
    let (dom, laid) = typeset(html, "", 600.0);
    let got: Vec<(f32, f32)> = cells(&dom, &laid)
        .into_iter()
        .map(|(x, _, w, _)| (x, w))
        .collect();
    assert_eq!(got, vec![(0.0, 50.0), (50.0, 150.0)], "25% / 75% 各就各位");
}

/// 表是一个普通的块：它后面的段落落在表的下缘之下（游标照常推进）。
#[test]
fn a_table_advances_the_cursor_like_any_block() {
    let html = "<table><tr><td>甲</td></tr></table><p>后文</p>";
    let (dom, laid) = typeset(html, "", 200.0);
    let table_h = laid
        .blocks
        .iter()
        .find(|b| b.border_box && dom.tag_name(b.node) == Some("table"))
        .map(|b| b.height)
        .expect("表盒子");
    let p_y = laid
        .blocks
        .iter()
        .find(|b| b.border_box && dom.tag_name(b.node) == Some("p"))
        .map(|b| b.y)
        .expect("段落盒子");
    // 表的下边距 0、段落自己的 UA 上边距 1em（16px）：折叠后取较大者，于是从
    // 「表的下缘 + 16」起排。这一条同时钉住「表格回到常规流」—— 它不是浮动盒。
    assert_eq!(p_y, table_h + 16.0, "段落从表的下缘 + 自己的上边距起排");
}

/// 量高（light）与实排给出**同一个**表高：两路共用网格、列宽与行高，
/// light 只把「每格怎么量」换成估算 —— 与普通块同一条容差（一个行高以内）。
#[test]
fn light_mode_measures_a_table_to_the_same_height() {
    let html = "<table><tr><td>甲乙</td><td>丙丁</td></tr>\
                <tr><td>戊</td><td>己</td></tr></table><p>后文</p>";
    let dom = Dom::parse(html.as_bytes());
    let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default(), "");
    let body = find(&dom, "body");
    let full = layout(
        &dom,
        &styles,
        body,
        LayoutOptions::with_width(200.0),
        &Mono,
        &NoImages,
    );
    let light = layout(
        &dom,
        &styles,
        body,
        LayoutOptions {
            width: 200.0,
            light: true,
            allow_vertical: false,
            page_height: None,
        },
        &Mono,
        &NoImages,
    );
    assert!(light.lines.is_empty(), "light 模式不该产出行盒");
    let diff = (full.height - light.height).abs();
    assert!(
        diff <= 28.0,
        "表高实排 {} vs 量高 {}，差 {diff}",
        full.height,
        light.height
    );
}

/// 格子的盒子进装饰表：底色与边框才画得出来，而且铺的是**拉高之后**的盒子。
#[test]
fn a_cell_box_reaches_the_decor_table_at_the_row_height() {
    // 表固定 100 宽：左边那格拿 84（10 个汉字折成两行），右边 16。
    let html = "<table style=\"width:100px\"><tr>\
                <td style=\"background-color:#123456\">甲乙丙丁戊己庚辛壬癸</td>\
                <td style=\"background-color:#654321\">短</td></tr></table>";
    let dom = Dom::parse(html.as_bytes());
    let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default(), "");
    let laid = layout(
        &dom,
        &styles,
        find(&dom, "body"),
        LayoutOptions::with_width(200.0),
        &Mono,
        &NoImages,
    );
    let seeds = laid.decor_seeds(&styles);
    let colored: Vec<(f32, f32, f32, f32)> = seeds
        .iter()
        .filter(|s| s.background.is_some())
        .map(|s| (s.x, s.y, s.width, s.height))
        .collect();
    assert_eq!(colored.len(), 2, "两个格子各一条底色");
    assert_eq!(
        colored[0].3, colored[1].3,
        "同一行的两条底色要一样高（都被拉到行高）"
    );
    assert!(
        colored[0].3 > 28.0,
        "左边那格折成两行，行高该大于一行：{:?}",
        colored[0]
    );
}

/// 真书门：哈利·波特那 5 张表要**并排**排出来（不是一列串下来）。
///
/// 合成夹具能钉住几何算式，钉不住「这本书的表格到底进没进这条路」—— 比如 UA 表没给
/// `<table>` 四档 display，合成用例照样过，而真书上整本书的表格会退化成竖着堆的块。
#[test]
#[ignore = "开真书找表格章节"]
fn the_harry_potter_tables_layout_side_by_side() {
    let path = std::path::Path::new("../../tests/fixtures/local/hali-bote-quanji.epub");
    if !path.is_file() {
        println!("跳过：{} 不在", path.display());
        return;
    }
    let doc = crate::content::document::open(path).expect("真书该打得开");
    let flow = doc.flow().expect("这本是可重排");
    let rules = RuleSet::parse(&doc.stylesheet(), Origin::Author);
    let mut tables = 0usize;
    let mut paired_rows = 0usize;
    for index in 0..flow.chapters.len() {
        let Ok(source) = doc.chapter_source(index) else {
            continue;
        };
        if !source.contains("<table") {
            continue;
        }
        let dom = Dom::parse(source.as_bytes());
        let styles = cascade(&dom, &rules, &ComputedStyle::default(), "");
        let laid = layout(
            &dom,
            &styles,
            dom.document,
            LayoutOptions::with_width(600.0),
            &Mono,
            &NoImages,
        );
        let table_boxes = laid
            .blocks
            .iter()
            .filter(|b| b.border_box && dom.tag_name(b.node) == Some("table"))
            .count();
        tables += table_boxes;
        // 同一行里出现两格（同 y、不同 x）才算「并排」。
        let cells: Vec<(f32, f32)> = laid
            .blocks
            .iter()
            .filter(|b| b.border_box && matches!(dom.tag_name(b.node), Some("td") | Some("th")))
            .map(|b| (b.x, b.y))
            .collect();
        paired_rows += cells
            .iter()
            .filter(|(x, y)| {
                cells
                    .iter()
                    .any(|(ox, oy)| (oy - y).abs() < 0.01 && (ox - x).abs() > 1.0)
            })
            .count();
    }
    // 门自己把量到的两个数打出来：绿了也要能看出「量到了几个」，
    // 否则「表格没进这条路」与「这本书没表格」在输出上长得一样。
    println!("表 {tables} 张，同排有邻格的格子 {paired_rows} 个");
    assert!(tables >= 3, "这本书实测有 5 张表，只排出来 {tables} 张");
    assert!(
        paired_rows >= 4,
        "表格的格子要并排：只找到 {paired_rows} 个「同排有邻格」的格子"
    );
}

/// `border-collapse: collapse` 的合并：相邻两格的公共边只画**一条**。
///
/// 两格各写 2px solid，共用边两侧各画一条就是 4px（那是 `separate` 的默认行为）；
/// 塌陷之后留赢的那条 2px。判据是「左格右边 + 右格左边」这两个数之和。
#[test]
fn collapsed_borders_draw_the_shared_edge_once() {
    let html = "<table style=\"width:100px; border-collapse: collapse\"><tr>\
                <td style=\"border:2px solid #000\">甲</td>\
                <td style=\"border:2px solid #000\">乙</td></tr></table>";
    let dom = Dom::parse(html.as_bytes());
    let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default(), "");
    let laid = layout(
        &dom,
        &styles,
        find(&dom, "body"),
        LayoutOptions::with_width(200.0),
        &Mono,
        &NoImages,
    );
    let edges: Vec<[f32; 4]> = laid
        .decor_seeds(&styles)
        .iter()
        .filter(|s| s.width > 0.0 && s.height > 0.0 && s.border.iter().any(|b| b.0 > 0.0))
        .map(|s| [s.border[0].0, s.border[1].0, s.border[2].0, s.border[3].0])
        .collect();
    assert_eq!(edges.len(), 2, "两格各一条装饰：{edges:?}");
    let shared = edges[0][1] + edges[1][3];
    assert_eq!(
        shared, 2.0,
        "公共边只画一条 2px（两张加起来是 4px 就是没合并）"
    );
    assert_eq!(edges[0][3], 2.0, "外圈那条边不受影响");
    assert_eq!(edges[1][1], 2.0, "外圈那条边不受影响");
}

/// 合并时**宽的那条赢**：左 1px、右 3px，共用边画 3px，左边那条被抹掉。
#[test]
fn the_wider_border_wins_the_shared_edge() {
    let html = "<table style=\"width:100px; border-collapse: collapse\"><tr>\
                <td style=\"border:1px solid #000\">甲</td>\
                <td style=\"border:3px solid #000\">乙</td></tr></table>";
    let dom = Dom::parse(html.as_bytes());
    let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default(), "");
    let laid = layout(
        &dom,
        &styles,
        find(&dom, "body"),
        LayoutOptions::with_width(200.0),
        &Mono,
        &NoImages,
    );
    let edges: Vec<[f32; 4]> = laid
        .decor_seeds(&styles)
        .iter()
        .filter(|s| s.width > 0.0 && s.height > 0.0 && s.border.iter().any(|b| b.0 > 0.0))
        .map(|s| [s.border[0].0, s.border[1].0, s.border[2].0, s.border[3].0])
        .collect();
    assert_eq!(edges[0][1], 0.0, "左边那格窄，共用边让给右格");
    assert_eq!(edges[1][3], 3.0, "宽的赢");
}

/// 没有 `border-collapse` 时保持 `separate`：两条边各画各的（合计 4px）。
#[test]
fn separate_borders_still_draw_both_sides() {
    let html = "<table style=\"width:100px\"><tr>\
                <td style=\"border:2px solid #000\">甲</td>\
                <td style=\"border:2px solid #000\">乙</td></tr></table>";
    let dom = Dom::parse(html.as_bytes());
    let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default(), "");
    let laid = layout(
        &dom,
        &styles,
        find(&dom, "body"),
        LayoutOptions::with_width(200.0),
        &Mono,
        &NoImages,
    );
    let edges: Vec<[f32; 4]> = laid
        .decor_seeds(&styles)
        .iter()
        .filter(|s| s.width > 0.0 && s.height > 0.0 && s.border.iter().any(|b| b.0 > 0.0))
        .map(|s| [s.border[0].0, s.border[1].0, s.border[2].0, s.border[3].0])
        .collect();
    assert_eq!(edges.len(), 2);
    assert_eq!(edges[0][1] + edges[1][3], 4.0, "默认 separate：两条边都画");
}

/// `word-break: break-all`：**词内部**也能断，于是同样的版心能少一行。
///
/// 度量桩：拉丁 8px/字、空格 4px。版心 100：
/// - 默认（`normal`）：`aaa` 之后那个 20 字的词放不下剩余 72px，整词挪到下一行再硬切
///   → 3 行（`aaa` / 前 12 字 / 后 8 字）。
/// - `break-all`：词内到处能断，第一行 `aaa ` + 9 个字母正好 100px → 2 行。
#[test]
fn break_all_lets_a_long_word_split_to_fill_the_line() {
    let html = "<p>aaa supercalifragilistic</p>";
    let (_, normal) = typeset(html, "p { text-indent: 0; margin: 0 }", 100.0);
    let (_, all) = typeset(
        html,
        "p { text-indent: 0; margin: 0; word-break: break-all }",
        100.0,
    );
    let texts = |laid: &Layout| -> Vec<String> {
        (0..laid.lines.len())
            .map(|i| laid.line_text(i).to_string())
            .collect()
    };
    assert_eq!(
        texts(&normal),
        vec!["aaa", "supercalifra", "gilistic"],
        "默认：词只能整段挪到下一行再硬切"
    );
    assert_eq!(
        texts(&all),
        vec!["aaa supercali", "fragilistic"],
        "break-all：词内断，第一行填满"
    );
    assert!(
        (all.lines[0].width - 100.0).abs() < 0.01,
        "第一行该正好填满版心，实际 {}",
        all.lines[0].width
    );
}

/// 表格里 `break-all` 的格子：最小宽从「整词」变成一个「字」，于是同样窄的表能排下内容。
///
/// 表固定 30px、格里是 10 个拉丁字的整词（80px）：
/// - 默认：列的最小宽是整词 80（表溢出到 80），一行排完；
/// - `break-all`：最小宽是一个字 8，列按可用宽 30 收住 → 每行 3 个字，共 4 行。
#[test]
fn break_all_shrinks_a_cells_minimum_width_to_one_character() {
    let html = "<table style=\"width:30px\"><tr><td>abcdefghij</td></tr></table>";
    let (dom, normal) = typeset(html, "", 200.0);
    let (dom2, all) = typeset(html, "td { word-break: break-all }", 200.0);
    let cell_w = |dom: &Dom, laid: &Layout| {
        laid.blocks
            .iter()
            .find(|b| b.border_box && dom.tag_name(b.node) == Some("td"))
            .map(|b| b.width)
            .expect("格子盒子")
    };
    assert_eq!(cell_w(&dom, &normal), 80.0, "默认：列宽被整词顶到 80");
    assert_eq!(cell_w(&dom2, &all), 30.0, "break-all：列收在可用宽 30");
    let lines = |laid: &Layout| laid.lines.len();
    assert_eq!(lines(&normal), 1, "80 宽正好一行排完 10 个字");
    assert_eq!(lines(&all), 4, "30 宽每行 3 个字 → 4 行");
}

/// 格内 `vertical-align`：内容在行高里贴哪一边。行高取最高那一格（这里是 56，右边只有 28）。
///
/// `middle` 挪一半（14）、`bottom` 挪到底（28）、`top`（初值）不动。挪的是**内容**，
/// 盒子本身始终拉满行高 —— 边框与底色因此仍铺满一行。
#[test]
fn a_table_cell_can_align_its_content_middle_or_bottom() {
    let line_y = |valign: &str| -> (f32, f32) {
        let html = format!(
            "<table style=\"width:100px\"><tr><td>甲乙丙丁戊己庚辛壬癸</td>\
             <td style=\"vertical-align: {valign}\">短</td></tr></table>"
        );
        let (dom, laid) = typeset(&html, "", 600.0);
        let at = (0..laid.lines.len())
            .find(|i| laid.line_text(*i) == "短")
            .unwrap_or_else(|| panic!("右边那一格该有一行「短」"));
        let cell_h = laid
            .blocks
            .iter()
            .filter(|b| b.border_box && dom.tag_name(b.node) == Some("td"))
            .map(|b| b.height)
            .fold(0.0f32, f32::max);
        (laid.lines[at].y, cell_h)
    };
    assert_eq!(line_y("top").0, 0.0, "默认贴行顶");
    assert_eq!(line_y("middle").0, 14.0, "行高 56、内容 28 → 挪一半");
    assert_eq!(line_y("bottom").0, 28.0, "贴行底");
    for valign in ["top", "middle", "bottom"] {
        let (_, cell_h) = line_y(valign);
        assert_eq!(cell_h, 56.0, "盒子始终拉满行高（{valign}）");
    }
}

/// `border-spacing`：列与列、行与行之间留缝，缝算在表盒里。
///
/// 两列各 32 宽、两行各 28 高、缝 10：表 74×66，第二列起点 42、第二行起点 38。
/// 塌陷模式下这条被忽略（CSS 2.1 §17.6.1），于是又回到 64×56。
#[test]
fn border_spacing_opens_gaps_between_cells() {
    let html = "<table style=\"border-spacing: 10px\"><tr><td>甲乙</td><td>丙丁</td></tr>\
                <tr><td>戊</td><td>己</td></tr></table>";
    let (dom, laid) = typeset(html, "", 200.0);
    assert_eq!(
        cells(&dom, &laid),
        vec![
            (0.0, 0.0, 32.0, 28.0),
            (42.0, 0.0, 32.0, 28.0),
            (0.0, 38.0, 32.0, 28.0),
            (42.0, 38.0, 32.0, 28.0),
        ],
        "第二列 32+10、第二行 28+10"
    );
    let table = laid
        .blocks
        .iter()
        .find(|b| b.border_box && dom.tag_name(b.node) == Some("table"))
        .expect("表盒子");
    assert_eq!((table.width, table.height), (74.0, 66.0), "缝算进表盒");

    let collapsed = "<table style=\"border-spacing: 10px; border-collapse: collapse\"><tr>\
                     <td>甲乙</td><td>丙丁</td></tr><tr><td>戊</td><td>己</td></tr></table>";
    let (dom2, laid2) = typeset(collapsed, "", 200.0);
    assert_eq!(
        cells(&dom2, &laid2),
        vec![
            (0.0, 0.0, 32.0, 28.0),
            (32.0, 0.0, 32.0, 28.0),
            (0.0, 28.0, 32.0, 28.0),
            (32.0, 28.0, 32.0, 28.0),
        ],
        "塌陷模式下 border-spacing 被忽略"
    );
}

// ---- 绝对定位（封面 / 标题页那几块装饰）----

/// 绝对定位的盒脱流：按 `left` / `bottom` 摆，**不推进游标、不撑高**这一章。
///
/// 版心 200。正文一段（上外边距 16 + 一行 28 + 下外边距 16）→ 这一章高 44。
/// 装饰块 `bottom: 0; height: 20px` → 落在 y = 44 − 20 = 24，`width: 50%` → 100 宽。
#[test]
fn an_absolute_box_is_placed_against_the_chapter_and_does_not_push_the_flow() {
    let html = "<p>正文</p>\
                <div style=\"position:absolute; left:0; bottom:0; width:50%; height:20px; \
                background-color:#123456\"></div>";
    let (dom, laid) = typeset(html, "", 200.0);
    let p_y = laid
        .blocks
        .iter()
        .find(|b| b.border_box && dom.tag_name(b.node) == Some("p"))
        .map(|b| b.y)
        .expect("段落");
    assert_eq!(p_y, 16.0, "绝对定位的兄弟不该把段落推走");
    let div = laid
        .blocks
        .iter()
        .find(|b| b.border_box && b.width > 0.0 && dom.tag_name(b.node) == Some("div"))
        .expect("装饰块");
    assert_eq!(
        (div.x, div.y, div.width, div.height),
        (0.0, 24.0, 100.0, 20.0),
        "bottom:0 + height:20 → 贴本章底；width:50% → 100"
    );
    assert_eq!(laid.height, 44.0, "绝对定位不撑高这一章");
}

/// 封面那套居中写法：`left:50%; margin-left:-9.5em; top:45%; margin-top:-4em`。
/// `left` / `top` 定位的是**外边距盒**外缘，所以负外边距把盒子往回拉半个自身尺寸。
#[test]
fn absolute_offsets_are_margin_edges_so_negative_margins_center_the_box() {
    let html = "<p>正文</p>\
                <div style=\"position:absolute; width:19em; height:8em; top:45%; left:50%; \
                margin-left:-9.5em; margin-top:-4em\"></div>";
    let (dom, laid) = typeset(html, "", 200.0);
    // 本章高 60：top 45% = 19.8、margin-top -4em（-64）→ -44.2；left 50% = 100、margin-left -9.5em（-152）→ -52。
    let div = laid
        .blocks
        .iter()
        .find(|b| b.border_box && dom.tag_name(b.node) == Some("div") && b.width > 200.0)
        .expect("装饰块");
    assert_eq!((div.x, div.y), (-52.0, -44.2), "负外边距把盒子拉回中心");
    assert_eq!((div.width, div.height), (304.0, 128.0), "19em × 8em");
}

/// `position: relative` 判不认识（它要不脱流地挪已排好的子树，命中与锚点都得跟着）；
/// 认不出时整条作废、块照常留在流里。
#[test]
fn relative_positioning_is_still_rejected() {
    let dom = Dom::parse("<p style=\"position: relative\">正文</p>".as_bytes());
    let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default(), "");
    assert_eq!(styles.ignored, 1, "`relative` 还没做，判不认识");
    let laid = layout(
        &dom,
        &styles,
        find(&dom, "body"),
        LayoutOptions::with_width(200.0),
        &Mono,
        &NoImages,
    );
    assert_eq!(
        laid.blocks
            .iter()
            .find(|b| b.border_box && dom.tag_name(b.node) == Some("p"))
            .map(|b| b.y),
        Some(16.0),
        "作废的定位不改这一块的常规流位置"
    );
}

/// `colspan`：跨列的格拿走那几列的总宽，下一行的格子仍按列对齐。
#[test]
fn a_colspan_cell_spans_its_columns_and_the_next_row_still_aligns() {
    let html = "<table style=\"width:100px\"><tr><td colspan=\"2\">甲</td></tr>\
                <tr><td>乙</td><td>丙</td></tr></table>";
    let (dom, laid) = typeset(html, "", 600.0);
    assert_eq!(
        cells(&dom, &laid),
        vec![
            (0.0, 0.0, 100.0, 28.0),
            (0.0, 28.0, 50.0, 28.0),
            (50.0, 28.0, 50.0, 28.0),
        ],
        "表头那格跨满 100；下一行两格各 50"
    );
}

/// `rowspan`：跨行的格高度取它跨过的那几行之和，下一行被挤到它右边那一列。
#[test]
fn a_rowspan_cell_takes_the_sum_of_its_rows() {
    let html = "<table style=\"width:100px\"><tr><td rowspan=\"2\">甲</td><td>乙</td></tr>\
                <tr><td>丙</td></tr></table>";
    let (dom, laid) = typeset(html, "", 600.0);
    assert_eq!(
        cells(&dom, &laid),
        vec![
            (0.0, 0.0, 50.0, 56.0),
            (50.0, 0.0, 50.0, 28.0),
            (50.0, 28.0, 50.0, 28.0),
        ],
        "跨行格高 56（两行之和）；第二行那格落在第 1 列"
    );
    // 跨行格自己也只排一次：三格（不是四格）。
    assert_eq!(cells(&dom, &laid).len(), 3);
}
