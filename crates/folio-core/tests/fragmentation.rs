//! M4 / E4 分片：一页装几行、切点落在哪、作者的断点意图怎么兑现。
//!
//! 与 `tests/layout.rs` 同一套办法：用「汉字 1 em、拉丁 0.5 em」的假度量，所以几何完全
//! 确定 —— 16px 字号、200px 版心 ⇒ 一行 12 个汉字；行盒高 16 × 1.75 = 28px。
//! 断言全部按这些数推出来，不是照实现的结果回填的。
//!
//! **两个容易记错的偏移**：UA 表给 `<p>` 上下各 1em 的外边距，所以第一行不在 y=0，
//! 而在 **y=16**；相邻两段之间折叠后是 **16px** 的空档，不是行盒的 28px。也就是说
//! 单段第 n 行在 `16 + 28n`，两段各一行时第二行的行顶在 `16 + 28 + 16 = 60`。
//! 页高是从这一片的**行顶**起算的绝对额度，所以跨段的那一页会比纯段内少装一行 ——
//! 按「两行 = 56px」去推多段用例的页高会推错。

use folio_core::css::Origin;
use folio_core::css::RuleSet;
use folio_core::css::computed::{BreakWhen, ComputedStyle, StyledDom, cascade};
use folio_core::dom::{Dom, NodeId};
use folio_core::layout::LayoutOptions;
use folio_core::layout::fragment::{Fragment, FragmentOptions, Fragmentainer, fragments};
use folio_core::layout::{Layout, LineMetrics, Measure, NoImages, layout};

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
    fn char_width(&self, ch: char, _font: &folio_core::layout::FontKey, font_size: f32) -> f32 {
        char_width(ch, font_size)
    }

    fn run_metrics(
        &self,
        text: &str,
        _font: &folio_core::layout::FontKey,
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
    let styled = styled(html, css, width);
    let out = fragments(&styled.0, &styled.1, FragmentOptions { height: page });
    (styled.0, out)
}

/// 排完版，连同样式表一起交出去（分片要读每个块的断点意图）。
fn styled(html: &str, css: &str, width: f32) -> (Layout, StyledDom) {
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
    (laid, styles)
}

fn ranges(pages: &[Fragment]) -> Vec<(usize, usize)> {
    pages
        .iter()
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
    let options = FragmentOptions { height: 60.0 };
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
    assert_eq!(token.page, 2);
}

#[test]
fn an_empty_document_yields_no_pages() {
    let (laid, styles) = styled("<p></p>", "", 200.0);
    assert!(laid.lines.is_empty(), "空段落不该产出行盒");
    assert!(
        fragments(&laid, &styles, FragmentOptions { height: 60.0 }).is_empty(),
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
    );
    let p = styles.get(find(&dom, "p")).expect("p 的样式");
    assert_eq!(
        p.orphans, 2,
        "`orphans: 0` 意味着一段可以被切成剩下零行，是写错了，不该当成 1"
    );
    assert!(
        !p.break_inside_avoid,
        "`avoid-column` 是多栏的取值，本引擎还没接，不该被当成 `avoid`"
    );
    assert_eq!(
        styles.ignored, 2,
        "两条都要计入 ignored，否则没预料到的写法会被静默吞掉"
    );
}
