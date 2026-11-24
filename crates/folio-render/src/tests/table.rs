//! 表格（`display: table` 一族）的集成用例。
//! 从 `tests.rs` 切出来只因为那个文件贴着行数上限；共享 helper 经 `super` 取用。

use super::{Mono, find, typeset};
use crate::{Dom, Layout, LayoutOptions, NoImages, layout};
use folio_format::css::computed::{ComputedStyle, cascade};
use folio_format::css::{Origin, RuleSet};

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

/// 直接挂在 `display: table` 下的 cell（章题两列骨架的写法）要补匿名行：
/// 两格并排、内容都在，而不是整块消失。
#[test]
fn direct_table_cells_form_an_anonymous_row() {
    let html = "<div class=\"grid\"><div class=\"cell\">甲</div><div class=\"cell\">乙</div></div>";
    let css = ".grid { display: table; width: 100px } .cell { display: table-cell }";
    let (_dom, laid) = typeset(html, css, 200.0);
    assert_eq!(laid.lines.len(), 2, "两个格子各有一行文本");
    assert!(
        laid.lines.iter().all(|line| line.y == 0.0),
        "匿名行里的两格并排，行盒同在 y=0"
    );
    let xs: Vec<f32> = laid.lines.iter().map(|line| line.x).collect();
    assert_eq!(xs, vec![0.0, 50.0], "定宽 100 平分给两列");
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
            screen_width: None,
            screen_height: None,
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
    let doc = folio_core::content::document::open(path).expect("真书该打得开");
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
