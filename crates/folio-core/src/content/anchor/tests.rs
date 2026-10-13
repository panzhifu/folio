//! 锚点坐标系与降级重定位。
//!
//! 这一份测试盯的是 M5 的验收句：「改 3 档字号 + 缩窗口 → 全部锚点命中或显式 stale」。
//! 几何用与 `layout::tests` 同一套假度量（汉字 1 em、拉丁 0.5 em、空格 0.25 em），
//! 所以行盒数量与断点都是推出来的，不是照实现回填的。

use crate::content::anchor::{
    CONTEXT_CHARS, Confidence, Relocated, TextAnchor, TextMap, line_at_offset, line_at_y,
    line_chars, line_offset, relocate,
};
use crate::content::dom::{Dom, NodeId};
use crate::content::format::css::computed::{ComputedStyle, cascade};
use crate::content::format::css::{Origin, RuleSet};
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

/// 一章的三件套：DOM、排版结果、可见文本坐标。
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

/// 第 `line` 行的可见文本（按坐标切出来，不走 `Layout::line_text`）。
fn line_slice(chapter: &Chapter, line: usize) -> String {
    chapter
        .map
        .slice(line_chars(&chapter.layout, &chapter.dom, &chapter.map, line).unwrap())
}

#[test]
fn the_visible_text_excludes_style_and_keeps_document_order() {
    let html = "<html><head><style>p { color: red }</style><title>书名</title></head>\
                <body><p>甲<em>乙</em>丙</p><p>丁</p></body></html>";
    let chapter = typeset(html, "", 200.0);
    assert_eq!(
        chapter.map.text(),
        "甲乙丙丁",
        "内联 CSS 与 <title> 都不是正文；文本节点按文档序拼"
    );
    assert_eq!(chapter.map.chars(), 4);
    // 字节数是 12（一字三字节），坐标系是字符数 4 —— 数错的那一处一定是中文。
    assert_eq!(chapter.map.text().len(), 12);
}

#[test]
fn an_empty_map_has_no_coordinates() {
    let chapter = typeset("<body><p></p></body>", "", 200.0);
    assert!(chapter.map.is_empty());
    assert_eq!(chapter.map.chars(), 0);
    assert!(line_chars(&chapter.layout, &chapter.dom, &chapter.map, 0).is_none());
}

/// 核心往返：**行的偏移**存下来再找回来，必须是同一行。
#[test]
fn every_lines_offset_points_back_to_that_line() {
    let chapter = typeset(&format!("<p>{}</p>", cjk(120)), "", 200.0);
    let total = chapter.layout.lines.len();
    assert_eq!(total, 10, "120 个字、一行 12 个");
    for line in 0..total {
        let offset = line_offset(&chapter.layout, &chapter.dom, &chapter.map, line)
            .unwrap_or_else(|| panic!("第 {line} 行该有文本坐标"));
        assert_eq!(offset, line * 12, "每行 12 个字");
        assert_eq!(
            line_at_offset(&chapter.layout, &chapter.dom, &chapter.map, offset),
            line,
            "第 {line} 行的行首偏移找回来必须是同一行"
        );
    }
}

#[test]
fn a_run_spanning_two_nodes_gives_one_wrapping_range() {
    // `<p>` 里有三个文本节点（正文 + <em> + 正文），它们排在同一行上。
    let chapter = typeset("<body><p>前段<em>中段</em>后段</p></body>", "", 600.0);
    assert_eq!(chapter.layout.lines.len(), 1);
    assert_eq!(
        line_slice(&chapter, 0),
        "前段中段后段",
        "一行横跨多个文本节点时要给出包住它们的区间"
    );
}

/// M5 验收的那一句：字号与宽度变了，坐标不变，落回的行也还是同一句。
#[test]
fn the_offset_survives_three_font_sizes_and_a_narrower_page() {
    let html = format!("<body><p>{}</p><p>{}</p></body>", cjk(60), cjk(60));
    let base = typeset(&html, "", 200.0);
    // 取第二段开头那一行（跨段边界的那一行最容易被排版参数影响）。
    let target = 5usize;
    let offset = line_offset(&base.layout, &base.dom, &base.map, target).unwrap();
    let words = line_slice(&base, target);

    for (css, width) in [
        ("", 140.0),
        ("p { font-size: 24px }", 200.0),
        ("p { font-size: 13px; line-height: 2 }", 320.0),
    ] {
        let changed = typeset(&html, css, width);
        let landed = line_at_offset(&changed.layout, &changed.dom, &changed.map, offset);
        let landed_words = line_slice(&changed, landed);
        assert!(
            words.starts_with(&landed_words) || landed_words.starts_with(&words),
            "宽度 {width}、样式 {css:?}：原行「{words}」与落回行「{landed_words}」不是同一句"
        );
    }
}

/// 行盒与偏移之外的另一条通道：像素 → 行号。桌面与移动共用它。
#[test]
fn line_at_y_chooses_the_line_that_covers_the_pixel() {
    let chapter = typeset(&format!("<p>{}</p>", cjk(60)), "", 200.0);
    assert_eq!(chapter.layout.lines.len(), 5);
    // 首行行顶 16（UA 给 <p> 的 1em 上边距），行盒 28px。
    assert_eq!(line_at_y(&chapter.layout, 0.0), 0, "还没滚出去也停在首行");
    assert_eq!(line_at_y(&chapter.layout, 16.0), 0);
    assert_eq!(
        line_at_y(&chapter.layout, 43.0),
        0,
        "43 还在第一行里（它的底是 44）"
    );
    assert_eq!(line_at_y(&chapter.layout, 44.0), 1, "到行的底才算下一行");
    assert_eq!(line_at_y(&chapter.layout, 999.0), 4, "滚过头也停在最后一行");
}

#[test]
fn anchors_carry_their_fingerprints() {
    let chapter = typeset(&format!("<body><p>{}</p></body>", cjk(60)), "", 200.0);
    // start 取 30：前面有满 24 字可取，`prefix` 才是全长而不是被夹短。
    let anchor = TextAnchor::new(&chapter.map, 0, 30, 36);
    assert_eq!(anchor.exact.chars().count(), 6);
    assert_eq!(anchor.exact, cjk(6));
    assert_eq!(anchor.prefix.chars().count(), CONTEXT_CHARS, "前 24 字");
    assert_eq!(anchor.suffix.chars().count(), CONTEXT_CHARS, "后 24 字");
    assert!(!anchor.path.is_empty(), "起点所在文本节点的路径要带上");
    // `end < start` 的调用是界面上的拖反向选择，夹平成插入点而不是 panic。
    let flipped = TextAnchor::new(&chapter.map, 0, 20, 10);
    assert_eq!((flipped.start, flipped.end), (20, 20));
}

#[test]
fn an_exact_anchor_relocates_as_exact() {
    let chapter = typeset(&format!("<body><p>{}</p></body>", cjk(60)), "", 200.0);
    let anchor = TextAnchor::new(&chapter.map, 0, 24, 30);
    assert_eq!(
        relocate(&anchor, &chapter.map),
        Relocated {
            start: 24,
            end: 30,
            confidence: Confidence::Exact
        }
    );
}

/// 第二级：同一个文本节点里插了字，偏移错了但路径还在。
#[test]
fn relocate_falls_back_to_the_same_dom_node() {
    let before = typeset("<body><p>开头重点句子结尾</p></body>", "", 200.0);
    // 「重点句子」在 2..6。
    let anchor = TextAnchor::new(&before.map, 0, 2, 6);
    assert_eq!(anchor.exact, "重点句子");
    assert_eq!(anchor.prefix, "开头");
    assert_eq!(anchor.suffix, "结尾");

    // 同一段前面多了五个字：文本节点没变（路径相同），坐标全体右移。
    let after = typeset("<body><p>新加的字串开头重点句子结尾</p></body>", "", 200.0);
    // 先确认第①级确实失败了 —— 原偏移上已经不是那句话，否则这条测试钉不住降级。
    assert_ne!(after.map.slice(2..6), anchor.exact);
    let hit = relocate(&anchor, &after.map);
    assert_eq!(hit.confidence, Confidence::SameNode, "实际：{hit:?}");
    assert_eq!(hit.start, 7, "原 2 + 前面插的 5");
    assert_eq!(after.map.slice(hit.start..hit.end), anchor.exact);
}

/// 第三级：路径没了、偏移也错了，但内容就在附近。
///
/// 注意「同一句话被包进 `<em>`」本身**不会**动可见文本坐标（拼接顺序没变），
/// 那条只会让第①级继续命中，测不到降级 —— 所以这里同时做两件事：
/// 段首插字（坐标全体右移，第①级失效）+ 把句子拆进 `<em>`（原来那个文本节点
/// 现在只含前 44 字，第②级也够不着），剩下的就只有附近窗口这一条路。
#[test]
fn relocate_falls_back_to_the_neighbourhood() {
    let before = typeset(
        &format!("<body><p>{}重点句{}</p></body>", cjk(40), cjk(6)),
        "",
        200.0,
    );
    let at = before
        .map
        .text()
        .find("重点句")
        .expect("fixture 里有这句话");
    let start = before.map.text()[..at].chars().count();
    let anchor = TextAnchor::new(&before.map, 0, start, start + 3);
    assert_eq!(anchor.exact, "重点句");
    assert_eq!(start, 40);

    let inserted = "补了四个字";
    let after = typeset(
        &format!(
            "<body><p>{}{}<em>重点句</em>{}</p></body>",
            inserted,
            cjk(40),
            cjk(6)
        ),
        "",
        200.0,
    );
    // 第①级失效：原偏移上是填充文字，不是那句话。
    assert_ne!(after.map.slice(start..start + 3), anchor.exact);
    // 第②级失效：锚点记的那条路径现在指向「段首插字 + 40 个「字」」这一个节点。
    let hit = relocate(&anchor, &after.map);
    assert_eq!(hit.confidence, Confidence::Nearby, "实际：{hit:?}");
    assert_eq!(after.map.slice(hit.start..hit.end), "重点句");
    assert_eq!(
        hit.start,
        start + inserted.chars().count(),
        "内容只跟着段首插的字右移，没有跳到别处去"
    );
}

/// 第四级：附近几百字内没有，整章里还有 —— 命中，但置信度要留给界面。
#[test]
fn relocate_falls_back_to_the_whole_chapter() {
    let chapter = typeset(
        &format!("<body><p>{}唯一句{}</p></body>", cjk(600), cjk(600)),
        "",
        200.0,
    );
    // 手工造一条「偏移与路径都不再有效、但整章还搜得到」的锚点。
    let anchor = TextAnchor {
        chapter: 0,
        start: 5,
        end: 8,
        exact: "唯一句".into(),
        prefix: cjk(24),
        suffix: cjk(24),
        path: "/html[1]/body[1]/p[1]#999".into(),
    };
    let hit = relocate(&anchor, &chapter.map);
    assert_eq!(hit.confidence, Confidence::Anywhere, "实际：{hit:?}");
    assert_eq!(chapter.map.slice(hit.start..hit.end), "唯一句");
}

#[test]
fn relocate_says_stale_instead_of_guessing_a_neighbour() {
    let chapter = typeset("<body><p>正文在这里</p></body>", "", 200.0);
    let anchor = TextAnchor {
        chapter: 0,
        start: 0,
        end: 4,
        exact: "这一句不在这本书里".into(),
        prefix: String::new(),
        suffix: String::new(),
        path: String::new(),
    };
    let hit = relocate(&anchor, &chapter.map);
    assert_eq!(hit.confidence, Confidence::Stale);
    // stale 不编造坐标：给的是夹到范围内的原值，让调用方自己标「找不到」。
    assert_eq!((hit.start, hit.end), (0, 4));
}

#[test]
fn an_empty_chapter_is_stale_not_a_panic() {
    let chapter = typeset("<body><p></p></body>", "", 200.0);
    let anchor = TextAnchor::new(&chapter.map, 0, 0, 5);
    assert_eq!(
        relocate(&anchor, &chapter.map).confidence,
        Confidence::Stale
    );
}

#[test]
fn the_anchor_json_round_trips_and_garbage_is_not_an_anchor() {
    let chapter = typeset("<body><p>一句中文</p></body>", "", 200.0);
    let anchor = TextAnchor::new(&chapter.map, 3, 1, 3);
    let json = anchor.to_json();
    assert_eq!(TextAnchor::from_json(&json).as_ref(), Some(&anchor));
    // 存的列可能来自旧版本或被别的程序写坏 —— 读不回来是「没有锚点」，不是 panic。
    assert_eq!(TextAnchor::from_json("{ not json"), None);
    assert_eq!(TextAnchor::from_json(r#"{"chapter":"x"}"#), None);
    // 只带坐标的 JSON（进度那种形状）不该被当锚点解出来：缺字段就整体失败。
    assert_eq!(TextAnchor::from_json(r#"{"chapter":0,"start":1}"#), None);
}

#[test]
fn a_point_anchor_carries_only_a_coordinate() {
    let point = TextAnchor::point(2, 1234);
    assert_eq!((point.chapter, point.start, point.end), (2, 1234, 1234));
    assert!(point.exact.is_empty() && point.path.is_empty());
    // 进度这种形状没有指纹，所以 relocate 直接判 stale 而不是猜。
    let chapter = typeset("<body><p>正文</p></body>", "", 200.0);
    assert_eq!(relocate(&point, &chapter.map).confidence, Confidence::Stale);
}

/// 标签之间的换行与缩进也是文本节点，所以「第一段正文的第一个字」坐标通常是 1，
/// 不是 0。这不是 bug，是坐标系的定义（见模块头），但它必须被钉住 ——
/// 否则将来谁把空白节点滤掉，整本书已存的锚点就会集体平移而没人知道。
#[test]
fn whitespace_between_tags_is_part_of_the_coordinate_space() {
    let pretty = typeset("<body>\n  <p>正文</p>\n  <p>第二段</p>\n</body>", "", 600.0);
    // 实测的解析形状：`</p>\n  <p>` 之间是一段空白 ⇒ 一个文本节点，不是两个。
    assert_eq!(
        pretty.map.text(),
        "\n  正文\n  第二段\n",
        "文本节点按文档序原样拼，包括标签之间的空白"
    );
    let offset = line_offset(&pretty.layout, &pretty.dom, &pretty.map, 0).unwrap_or(999);
    assert!(offset > 0, "第一行的坐标不该是 0：{offset}");
    assert_eq!(pretty.map.slice(offset..offset + 2), "正文");
    // 同一份内容压掉空白之后坐标就变了 —— 所以坐标系只能有一套定义。
    let tight = typeset("<body><p>正文</p><p>第二段</p></body>", "", 600.0);
    assert_eq!(
        line_offset(&tight.layout, &tight.dom, &tight.map, 0).unwrap_or(999),
        0
    );
}
