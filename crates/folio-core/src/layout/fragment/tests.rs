//! M4 / E4 分片：一页装几行、切点落在哪、作者的断点意图怎么兑现。
//!
//! 与 `layout::tests` 同一套办法：用「汉字 1 em、拉丁 0.5 em」的假度量，所以几何完全
//! 确定 —— 16px 字号、200px 版心 ⇒ 一行 12 个汉字；行盒高 16 × 1.75 = 28px。
//! 断言全部按这些数推出来，不是照实现的结果回填的。
//!
//! **两个容易记错的偏移**：UA 表给 `<p>` 上下各 1em 的外边距，所以第一行不在 y=0，
//! 而在 **y=16**；相邻两段之间折叠后是 **16px** 的空档，不是行盒的 28px。也就是说
//! 单段第 n 行在 `16 + 28n`，两段各一行时第二行的行顶在 `16 + 28 + 16 = 60`。
//! 页高是从这一片的**行顶**起算的绝对额度，所以跨段的那一页会比纯段内少装一行 ——
//! 按「两行 = 56px」去推多段用例的页高会推错。

use crate::content::dom::{Dom, NodeId};
use crate::content::format::css::Origin;
use crate::content::format::css::RuleSet;
use crate::content::format::css::computed::{BreakWhen, ComputedStyle, StyledDom, cascade};
use crate::layout::LayoutOptions;
use crate::layout::fragment::{
    Fragment, FragmentOptions, Fragmentainer, column_offset, column_width, fragments,
};
use crate::layout::{Layout, LineMetrics, Measure, NoImages, layout};

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

/// 一行 12 个汉字 ⇒ n 个字占 `ceil(n / 12)` 行。
fn cjk(chars: usize) -> String {
    "字".repeat(chars)
}

fn find(dom: &Dom, want: &str) -> NodeId {
    dom.elements()
        .find(|(id, _)| dom.tag_name(*id) == Some(want))
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("文档里应有 <{want}>"))
}

/// 排版 + 分片，返回分片结果与行盒总数。
fn paginated(html: &str, css: &str, width: f32, page: f32) -> (Layout, Vec<Fragment>) {
    split_columns(html, css, width, page, 1)
}

/// 同上，但指定一页几栏。
fn split_columns(
    html: &str,
    css: &str,
    width: f32,
    page: f32,
    columns: usize,
) -> (Layout, Vec<Fragment>) {
    let styled = styled(html, css, width);
    let out = fragments(
        &styled.0,
        &styled.1,
        FragmentOptions {
            height: page,
            columns,
        },
    );
    (styled.0, out)
}

/// 排完版，连同样式表一起交出去（分片要读每个块的断点意图）。
fn styled(html: &str, css: &str, width: f32) -> (Layout, StyledDom) {
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
            allow_vertical: false,
        },
        &Mono,
        &NoImages,
    );
    (laid, styles)
}

fn ranges(pages: &[Fragment]) -> Vec<(usize, usize)> {
    pages
        .iter()
        .map(|page| (page.lines.start, page.lines.end))
        .collect()
}

/// 每片的 `(页号, 栏号)`。多栏的断言全靠它 —— 「第几片」读者不关心，
/// 「这一栏落在哪一页的第几格」才是界面要画对的东西。
fn slots(pages: &[Fragment]) -> Vec<(usize, usize)> {
    pages.iter().map(|page| (page.page, page.column)).collect()
}

/// 只留有内容的片的行区间：补出来的空栏不该参与「切点有没有被挪动」这种比较。
fn content_ranges(pages: &[Fragment]) -> Vec<(usize, usize)> {
    pages
        .iter()
        .filter(|page| !page.empty)
        .map(|page| (page.lines.start, page.lines.end))
        .collect()
}

/// 分片必须把每一行不重不漏地盖住 —— 这条比任何单点断言都重要，
/// 漏一行就是「书里少了半句」，重一行就是读到重复段落。
fn assert_tiles(pages: &[Fragment], total: usize) {
    let mut next = 0usize;
    for page in pages {
        if page.empty {
            continue;
        }
        assert_eq!(page.lines.start, next, "分片之间有洞：{pages:?}");
        assert!(page.lines.end > next, "出现了不空的空分片：{pages:?}");
        next = page.lines.end;
    }
    assert_eq!(next, total, "末尾还留着没分完的行");
}

#[test]
fn continuous_mode_is_one_fragment_containing_everything() {
    let html = format!("<p>{}</p>", cjk(30));
    let (laid, pages) = paginated(&html, "", 200.0, 0.0);
    assert_eq!(laid.lines.len(), 3, "30 个字、200px 版心 → 3 行");
    assert_eq!(ranges(&pages), vec![(0, 3)], "连续滚动 = 单一片");
    assert!(
        !pages[0].forced && pages[0].height > 80.0,
        "那片该是整段的高度：{:?}",
        pages[0]
    );
}

#[test]
fn a_page_holds_whole_lines_and_the_pages_tile_them() {
    let html = format!("<p>{}</p>", cjk(60));
    let (laid, pages) = paginated(&html, "", 200.0, 60.0);
    assert_eq!(laid.lines.len(), 5);
    // 行盒 28px，页 60px ⇒ 两行占 56px 放得下，第三行要 84px 放不下。
    assert_eq!(ranges(&pages), vec![(0, 2), (2, 4), (4, 5)]);
    assert_tiles(&pages, laid.lines.len());
    for page in &pages {
        assert!(
            page.height <= 60.0,
            "第 {:?} 片高 {} 超过页高",
            page.lines,
            page.height
        );
    }
}

#[test]
fn break_after_page_ends_the_page_early_even_with_room_left() {
    let html = format!("<p class=stop>{}</p><p>{}</p>", cjk(10), cjk(30));
    let css = ".stop { break-after: page; }";
    let (laid, pages) = paginated(&html, css, 200.0, 60.0);
    assert_eq!(laid.lines.len(), 4, "1 行 + 3 行");
    // 不强制的话第一页会吃掉第二段的头一行；强制断在那里，第一页就只剩它自己。
    assert_eq!(ranges(&pages), vec![(0, 1), (1, 3), (3, 4)]);
    assert!(pages[0].forced, "第一页是被 break-after 结束的");
    assert!(!pages[1].forced, "第二页塞满两行，不是强制断的");
    assert_tiles(&pages, laid.lines.len());
}

#[test]
fn break_before_page_pushes_a_block_to_the_next_page() {
    // 两段各一行：第一段行顶 16、第二段行顶 60。页高 76 ⇒ 不加声明时两行都放得下
    // （60 + 28 = 88 > 16 + 76 = 92？不，88 ≤ 92 ✓），加了才必须另起一页。
    // 先跑一遍不加声明的作为对照，否则「分成两页」可能只是页高不够，与 break-before 无关。
    let html = format!("<p>{}</p><p class=next>{}</p>", cjk(10), cjk(10));
    let (_, without) = paginated(&html, "", 200.0, 76.0);
    assert_eq!(
        ranges(&without),
        vec![(0, 2)],
        "对照：76px 的页装得下这两行"
    );

    let (laid, pages) = paginated(&html, ".next { break-before: page; }", 200.0, 76.0);
    assert_eq!(ranges(&pages), vec![(0, 1), (1, 2)]);
    assert!(pages[0].forced, "第一页是作者要求断的，不是塞满了才断");
    assert_tiles(&pages, laid.lines.len());
}

#[test]
fn page_break_aliases_are_the_same_intent() {
    // CSS2.1 的写法在老 EPUB 里满地都是，认不得就等于把这些书的作者意图丢掉。
    let html = format!("<p class=stop>{}</p><p>{}</p>", cjk(10), cjk(10));
    let modern = paginated(html.as_str(), ".stop { break-after: page; }", 200.0, 60.0);
    let legacy = paginated(
        html.as_str(),
        ".stop { page-break-after: page; }",
        200.0,
        60.0,
    );
    assert_eq!(ranges(&modern.1), ranges(&legacy.1));
    assert!(legacy.1[0].forced, "`page-break-after` 也该标成强制断点");
}

#[test]
fn break_inside_avoid_moves_the_whole_block_down() {
    let html = format!("<p>{}</p><p class=keep>{}</p>", cjk(10), cjk(24));
    let css = ".keep { break-inside: avoid; }";
    let (laid, pages) = paginated(&html, css, 200.0, 60.0);
    assert_eq!(laid.lines.len(), 3, "1 行 + 2 行");
    // 不保护的话第一页会拿走保护块的头一行，把它劈成 1 + 1；保护之后整块挪到下一页。
    assert_eq!(ranges(&pages), vec![(0, 1), (1, 3)]);
    assert_tiles(&pages, laid.lines.len());
}

#[test]
fn avoid_gives_up_when_the_block_is_taller_than_a_page() {
    let html = format!("<p class=keep>{}</p>", cjk(48));
    let css = ".keep { break-inside: avoid; }";
    let (laid, pages) = paginated(&html, css, 200.0, 60.0);
    assert_eq!(laid.lines.len(), 4);
    // 一段四行、一页两行：想守也守不住。这里要的是「照样排出来」，
    // 而不是一片空的分片或永远走不完。
    assert_eq!(ranges(&pages), vec![(0, 2), (2, 4)]);
    assert!(
        pages.iter().all(|page| !page.empty),
        "没有奇偶补位的需求时不该出现空片：{pages:?}"
    );
}

#[test]
fn orphans_and_widows_conflict_moves_the_whole_paragraph() {
    // 一段三行、一页三行：切点会落在段内第 3 行后，留下 1 行寡行。
    // 把它挪走又会只剩 1 行孤行 —— 两个约束同时不成立时整段下沉，
    // 因为第二页装得下整段。
    let html = format!("<p>{}</p><p class=tall>{}</p>", cjk(10), cjk(30));
    let (laid, pages) = paginated(&html, "", 200.0, 84.0);
    assert_eq!(laid.lines.len(), 4, "1 行 + 3 行");
    assert_eq!(ranges(&pages), vec![(0, 1), (1, 4)]);
    assert_tiles(&pages, laid.lines.len());
}

#[test]
fn a_single_tall_paragraph_keeps_orphans_and_accepts_a_widow() {
    // 只有这一段、开头就顶着页首：无处可挪。这时保 orphans、让下一页顶一行，
    // 是排版传统里的取舍（页底留一行比页顶留一行更难看），不是随手挑的。
    let html = format!("<p>{}</p>", cjk(30));
    let (laid, pages) = paginated(&html, "", 200.0, 60.0);
    assert_eq!(laid.lines.len(), 3);
    assert_eq!(ranges(&pages), vec![(0, 2), (2, 3)]);
}

#[test]
fn widows_and_orphans_can_be_loosened_to_one() {
    // 同一份内容，`orphans: 1; widows: 1` 就是「随便切」：第一页该塞满三行。
    let html = format!("<p>{}</p>", cjk(40));
    let (laid, loose) = paginated(&html, "p { orphans: 1; widows: 1; }", 200.0, 84.0);
    assert_eq!(laid.lines.len(), 4);
    assert_eq!(ranges(&loose), vec![(0, 3), (3, 4)]);
    // 回到初值 2 / 2：第四段那行不能单独留在下一页，于是第一页少拿一行。
    let (_, strict) = paginated(&html, "", 200.0, 84.0);
    assert_eq!(ranges(&strict), vec![(0, 2), (2, 4)]);
}

#[test]
fn zero_orphans_and_widows_loosen_like_one() {
    // 转换本爱写 `orphans: 0; widows: 0`（《斗战狂潮》全书 138336 条就是这一族），
    // 意思是「别管孤行寡行」。CSS 的合法域从 1 起，所以取值层把它夹到 1；
    // 判它「不认识」会把整本推回引擎默认的 2 / 2，症状是页底比作者要的少留一行。
    let html = format!("<p>{}</p>", cjk(40));
    let (_, zero) = paginated(&html, "p { orphans: 0; widows: 0; }", 200.0, 84.0);
    let (_, loose) = paginated(&html, "p { orphans: 1; widows: 1; }", 200.0, 84.0);
    assert_eq!(ranges(&zero), ranges(&loose), "0 该夹到 1，与写 1 逐行同形");
    assert_eq!(ranges(&zero), vec![(0, 3), (3, 4)]);
}

#[test]
fn break_before_right_inserts_a_blank_page_for_parity() {
    let html = format!("<p>{}</p><p class=r>{}</p>", cjk(10), cjk(10));
    let css = ".r { break-before: right; }";
    let (laid, pages) = paginated(&html, css, 200.0, 1000.0);
    assert_eq!(laid.lines.len(), 2);
    // 第 0 页是右页（recto）；`break-after` 把内容截在第 0 页，于是第 1 页是左页，
    // 而 `right` 要它当右页 ⇒ 中间补一张空片。
    assert_eq!(pages.len(), 3, "{pages:?}");
    assert!(pages[1].empty && pages[1].lines.end == pages[1].lines.start);
    assert_eq!((pages[2].lines.start, pages[2].lines.end), (1, 2));
    assert_tiles(&pages, laid.lines.len());
}

#[test]
fn resuming_from_a_token_matches_the_full_run() {
    let html = format!("<p>{}</p>", cjk(120));
    let (laid, styles) = styled(&html, "", 200.0);
    let options = FragmentOptions {
        height: 60.0,
        columns: 1,
    };
    let all = fragments(&laid, &styles, options);
    assert_eq!(all.len(), 5, "10 行、每页 2 行");

    // 翻到第三页：前两次正常走，把游标存下来，再用它接上。
    let mut first = Fragmentainer::new(&laid, &styles, options);
    let mut seen = Vec::new();
    for _ in 0..2 {
        seen.push(first.next().expect("前两页该存在"));
    }
    let token = first.token();
    let rest: Vec<Fragment> = Fragmentainer::resumed(&laid, &styles, options, token).collect();
    assert_eq!(seen.len(), 2);
    assert_eq!(rest, all[2..], "从游标接着算必须和一次性算完逐片相等");
    assert_eq!(token.line, 4);
    assert_eq!(token.fragment, 2);
}

#[test]
fn an_empty_document_yields_no_pages() {
    let (laid, styles) = styled("<p></p>", "", 200.0);
    assert!(laid.lines.is_empty(), "空段落不该产出行盒");
    assert!(
        fragments(
            &laid,
            &styles,
            FragmentOptions {
                height: 60.0,
                columns: 1
            }
        )
        .is_empty(),
        "没有行就没有片"
    );
}

/// 初值是 CSS 定的，不是我们挑的：2 / 2 意味着「默认就不许留孤行寡行」，
/// 而 `break-*` 一律 `Auto` —— 一本书没写分片意图时，分片器只按高度切。
#[test]
fn the_initial_values_are_the_css_ones() {
    let base = ComputedStyle::default();
    assert_eq!((base.orphans, base.widows), (2, 2));
    assert_eq!(base.break_before, BreakWhen::Auto);
    assert_eq!(base.break_after, BreakWhen::Auto);
    assert!(!base.break_inside_avoid);
}

#[test]
fn break_properties_follow_css_inheritance_rules() {
    let dom = Dom::parse(b"<div class=book><p>x</p></div>");
    let styles = cascade(
        &dom,
        &RuleSet::parse(
            ".book { orphans: 4; widows: 5; break-inside: avoid; }",
            Origin::Author,
        ),
        &ComputedStyle::default(),
        "",
    );
    let div = styles.get(find(&dom, "div")).expect("div 的样式");
    let p = styles.get(find(&dom, "p")).expect("p 的样式");
    assert!(div.break_inside_avoid);
    // orphans / widows 是 CSS 里的可继承属性：书里在 `<body>` 上写一次就够。
    assert_eq!((p.orphans, p.widows), (4, 5), "这两个该继承下来");
    // break-* 不继承。若继承了，父块上一个 `avoid` 会让每个子块都挪不动，
    // 整页就再也塞不进东西 —— 这是会排不出版的错法。
    assert!(!p.break_inside_avoid, "break-inside 不该继承");
}

/// 认不出的值改状态也计 `ignored`，而不是猜一个作者没写的意思。
///
/// `orphans: 0` 已经不在这条名单里了（2026-10-15）：它不是「写错了」，是转换本表达
/// 「不设孤行约束」的写法，全书十几万条，所以取值层把它夹到合法域下沿 1。
/// 剩下的 `break-inside: avoid-column` 才是真不认识的那一类 —— 多栏还没接。
#[test]
fn a_broken_value_is_counted_rather_than_guessed() {
    let dom = Dom::parse(b"<p>x</p>");
    let styles = cascade(
        &dom,
        &RuleSet::parse(
            "p { orphans: 0; break-inside: avoid-column; }",
            Origin::Author,
        ),
        &ComputedStyle::default(),
        "",
    );
    let p = styles.get(find(&dom, "p")).expect("p 的样式");
    assert_eq!(
        p.orphans, 1,
        "`orphans: 0` 夹到 1（见 zero_orphans_and_widows_loosen_like_one），不是退回初值"
    );
    assert!(
        !p.break_inside_avoid,
        "`avoid-column` 是多栏的取值，本引擎还没接，不该被当成 `avoid`"
    );
    assert_eq!(
        styles.ignored, 1,
        "只有真不认识的那条计 ignored，夹出来的合法值不算落差"
    );
}

// ---- 多栏：一栏一片，页是 `columns` 片 ----
//
// 下面每一条都用同一个基准：**先按 `columns: 1` 跑一遍**。栏这件事只该改变
// 「第几页第几栏」的归组，不该改变任何一处切点；把它跟基准比对，就是在钉这一点。

/// 十行的书、页高 60 ⇒ 每栏两行、共五栏。分栏前后各栏的行区间必须一模一样。
fn ten_lines() -> String {
    format!("<p>{}</p>", cjk(120))
}

#[test]
fn columns_do_not_move_a_single_cut_point() {
    let html = ten_lines();
    let (laid, one) = paginated(&html, "", 200.0, 60.0);
    let (_, two) = split_columns(&html, "", 200.0, 60.0, 2);
    let (_, three) = split_columns(&html, "", 200.0, 60.0, 3);
    assert_eq!(laid.lines.len(), 10, "120 个字、一行 12 个 → 10 行");
    assert_eq!(
        content_ranges(&one),
        vec![(0, 2), (2, 4), (4, 6), (6, 8), (8, 10)],
        "基准：每栏两行"
    );
    assert_eq!(
        content_ranges(&two),
        content_ranges(&one),
        "两栏不该挪动任何切点"
    );
    assert_eq!(content_ranges(&three), content_ranges(&one), "三栏同样不该");
    assert_eq!(two.iter().filter(|page| !page.empty).count(), 5);
}

#[test]
fn a_page_carries_exactly_the_configured_number_of_columns() {
    let html = ten_lines();
    let (laid, pages) = split_columns(&html, "", 200.0, 60.0, 2);
    assert_eq!(
        ranges(&pages),
        vec![(0, 2), (2, 4), (4, 6), (6, 8), (8, 10)]
    );
    assert_eq!(
        slots(&pages),
        vec![(0, 0), (0, 1), (1, 0), (1, 1), (2, 0)],
        "第 0、1 页各两栏，末页只有一栏"
    );
    assert_tiles(&pages, laid.lines.len());
    // 末页不补满：补出来的空白栏会让读者翻到一页什么都没有。
    assert_eq!(pages.last().expect("有内容就该有末页").column, 0);
}

#[test]
fn a_forced_break_pads_the_rest_of_its_page() {
    // 第一段一行 + `break-after: page`，第二段五行。页高 60 ⇒ 每栏两行，两栏一页。
    let html = format!("<p class=stop>{}</p><p>{}</p>", cjk(10), cjk(60));
    let (laid, pages) = split_columns(&html, ".stop { break-after: page; }", 200.0, 60.0, 2);
    assert_eq!(laid.lines.len(), 6, "1 行 + 5 行");
    // 强制断点只交出第一栏，本页剩下那栏补空片，内容从下一页第一栏继续。
    assert_eq!(
        ranges(&pages),
        vec![(0, 1), (1, 1), (1, 3), (3, 5), (5, 6)],
        "第二项是补出来的空栏"
    );
    assert_eq!(
        slots(&pages),
        vec![(0, 0), (0, 1), (1, 0), (1, 1), (2, 0)],
        "补齐之后每页恰好两栏"
    );
    assert!(pages[1].empty && pages[1].lines.end == pages[1].lines.start);
    assert_tiles(&pages, laid.lines.len());
    // 对照：同样内容不分栏时不需要补位，片数就是页数。
    let (_, flat) = paginated(&html, ".stop { break-after: page; }", 200.0, 60.0);
    assert_eq!(ranges(&flat), vec![(0, 1), (1, 3), (3, 5), (5, 6)]);
    assert!(flat.iter().all(|page| !page.empty), "单栏时不该出现空片");
}

#[test]
fn parity_padding_fills_a_whole_page_when_there_are_columns() {
    // 两段各一行、页高够装两行，第二段要 `break-before: right`。
    // 单栏时补一张空片就换到右页；两栏时「补一张」只挪动一栏，那一页就同时是左页
    // 又是右页 —— 必须补满整页，内容才真的落在下一页。
    let html = format!("<p>{}</p><p class=r>{}</p>", cjk(10), cjk(10));
    let css = ".r { break-before: right; }";
    let (laid, pages) = split_columns(&html, css, 200.0, 1000.0, 2);
    assert_eq!(laid.lines.len(), 2);
    assert_eq!(
        slots(&pages),
        vec![(0, 0), (0, 1), (1, 0), (1, 1), (2, 0)],
        "第 0 页被强制断点截掉半页，第 1 页整页空白，内容落到第 2 页"
    );
    assert_eq!(pages.iter().filter(|page| page.empty).count(), 3);
    let last = pages.last().expect("末页");
    assert_eq!(last.page, 2, "偶数页号当右页");
    assert_eq!((last.lines.start, last.lines.end), (1, 2));
    assert_tiles(&pages, laid.lines.len());
    // 对照单栏：同一个声明只补一张。
    let (_, flat) = paginated(&html, css, 200.0, 1000.0);
    assert_eq!(flat.iter().filter(|page| page.empty).count(), 1);
}

#[test]
fn continuous_mode_ignores_columns() {
    let html = ten_lines();
    let (_, pages) = split_columns(&html, "", 200.0, 0.0, 3);
    assert_eq!(ranges(&pages), vec![(0, 10)], "没有页就没有栏：整篇一片");
    assert_eq!(slots(&pages), vec![(0, 0)]);
}

#[test]
fn columns_zero_is_the_same_as_one() {
    // `0` 在选项里是「没设」，不是除数。漏夹一次就是 panic。
    let html = ten_lines();
    let (_, none) = split_columns(&html, "", 200.0, 60.0, 0);
    let (_, one) = split_columns(&html, "", 200.0, 60.0, 1);
    assert_eq!(ranges(&none), ranges(&one));
    assert_eq!(slots(&none), slots(&one));
}

/// 补位状态存在游标里，而不是分片器的私有队列里 —— 这条就是那个决定的验证：
/// 只走一片就存下 Token，续算必须仍然把剩下的空栏补出来。
#[test]
fn resuming_in_the_middle_of_padding_still_pads() {
    let html = format!("<p class=stop>{}</p><p>{}</p>", cjk(10), cjk(60));
    let (laid, styles) = styled(&html, ".stop { break-after: page; }", 200.0);
    let options = FragmentOptions {
        height: 60.0,
        columns: 2,
    };
    let all = fragments(&laid, &styles, options);
    let mut first = Fragmentainer::new(&laid, &styles, options);
    let head = first.next().expect("第一栏");
    let token = first.token();
    assert_eq!(
        token.pad_until,
        token.fragment + 1,
        "断点在这一页第一栏，还剩一栏要补：{token:?}"
    );
    let rest: Vec<Fragment> = Fragmentainer::resumed(&laid, &styles, options, token).collect();
    let mut seen = vec![head];
    seen.extend(rest);
    assert_eq!(seen, all, "续算必须和一次性算完逐片相等");
    assert!(all[1].empty, "补的那一栏不能因为换了游标就丢掉");
}

#[test]
fn column_width_and_offset_are_the_numbers_the_layout_needs() {
    // 640px 四栏、栏间 20px：三条间距共 60px，剩下 580 分四份。
    assert_eq!(column_width(640.0, 4, 20.0), 145.0);
    assert_eq!(column_width(640.0, 1, 20.0), 640.0, "一栏没有间距");
    assert_eq!(column_width(640.0, 0, 20.0), 640.0, "`0` 当一栏");
    // 窄到放不下间距时给 0，不给负数 —— 负宽传进折行会直接算不出行。
    assert_eq!(column_width(100.0, 4, 40.0), 0.0);
    assert_eq!(column_offset(145.0, 20.0, 0), 0.0);
    assert_eq!(
        column_offset(145.0, 20.0, 2),
        330.0,
        "第三栏 = 2 × (栏宽 + 间距)"
    );
    // 间距为负按 0 算：读者设置里不该出现负数，但算式不能因此把栏推到左边去。
    assert_eq!(column_offset(145.0, -50.0, 1), 145.0);
}
