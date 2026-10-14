//! 排版结果上那两张索引的**口径**用例：折出来的表、二分出来的区间，必须与接线前那条
//! 「每次现扫一遍全表」的规则一个不差。
//!
//! 为什么整片都拿参照实现来比，而不是挑几个手算的期望值：这三条查询里最要命的不是
//! 「算错一个像素」，而是**边界口径漂了**——「第一条覆盖这个像素的行」与
//! 「没有覆盖者给最后一行」是两条不同的规则，它们各自决定点选落在哪、滚动位置换成
//! 第几个字。挑期望值的人只会挑自己记得住的边界；参照实现把每一条 y 都跑一遍。
//!
//! 参照实现在 `layout::geometry::scan_reference`：那是接线前那几行代码照抄的，只在
//! `cfg(test)` 里编译。

use super::typeset;
use crate::content::format::css::computed::TextAlign;
use crate::layout::geometry::scan_reference;
use crate::layout::{BlockBox, DecorSeed, Layout, LineBox};

/// 一条行盒（不带片段：这三条查询只看几何）。
fn line(owner: u32, y: f32, height: f32) -> LineBox {
    LineBox {
        owner,
        fragments: Vec::new(),
        x: 0.0,
        y,
        width: 100.0,
        height,
        baseline: height,
        align: TextAlign::Left,
        justify: None,
    }
}

/// 一条块盒。几何与文本都不重要，重要的是它占哪几行、是不是边框盒那一条注册。
fn block(node: u32, lines: std::ops::Range<usize>, border_box: bool) -> BlockBox {
    BlockBox {
        node,
        x: 0.0,
        y: 0.0,
        width: 100.0,
        height: 50.0,
        text: String::new(),
        lines,
        border_box,
    }
}

/// 真排一章版：标题 + 嵌套容器 + 强调 + 引用 + 尾段，行盒与块盒都够多，边界也自然。
fn a_real_layout() -> Layout {
    let html = "<h1>标题</h1><div class=\"chapter\"><p>第一段，字多到该折几行才排得下，\
                于是这里就有了好几个行盒可以查。再多写几句，好让这一章的行数配得上「边界」\
                这两个字：折行处、留白处、块与块的接缝处，都该有东西可探。</p>\
                <p>第二段。<em>强调</em>后面还有字。</p>\
                <blockquote><p>引用里的段落，块嵌套着块。</p></blockquote></div><p>尾段</p>";
    let (_, layout) = typeset(
        html,
        ".chapter { margin: 8px } blockquote { padding: 4px }",
        260.0,
    );
    assert!(
        layout.lines.len() > 10,
        "这章得排出足够多的行才有边界可查：{}",
        layout.lines.len()
    );
    assert!(
        layout.stacked(),
        "真排出来的行盒必须叠放，否则下面全在测回退那条路"
    );
    layout
}

/// 折好的「行 → 块」表必须与先序规则逐行同答案。
///
/// 索引化本身不会让它变红（两条规则定义相同）；它红的是**折表写错**那一类：
/// 写成正向 `find`（留下的是最浅的祖先块）、或者循环顺序颠倒（留下的是最后一个兄弟
/// 而不是叶子）。症状分别是「底色画满整段」与「行文本取到别段的串」。
#[test]
fn the_folded_owner_is_the_preorder_rule() {
    let layout = a_real_layout();
    for i in 0..layout.lines.len() {
        let indexed = layout.block_of_line(i);
        let scanned =
            scan_reference::block_of_line(&layout, i).and_then(|ix| layout.blocks.get(ix));
        assert_eq!(
            indexed.map(|b| (b.node, b.border_box)),
            scanned.map(|b| (b.node, b.border_box)),
            "第 {i} 行的归属块对不上"
        );
        // 归属块的行区间一定包着这一行：折表指错条目时这条先炸。
        assert!(
            indexed.is_some_and(|b| b.lines.contains(&i)),
            "第 {i} 行没有归属块或归属块的区间不含它"
        );
    }
}

/// 越界的行号给 `None`，不许回绕到别的块；没排过版（空表）也一样。
#[test]
fn owner_of_an_out_of_range_line_is_none() {
    let layout = a_real_layout();
    assert!(layout.block_of_line(layout.lines.len()).is_none());
    assert!(layout.block_of_line(usize::MAX).is_none());
    assert!(Layout::default().block_of_line(0).is_none());
    assert_eq!(Layout::default().line_text(0), "");
}

/// 二分的 `line_at` 必须与整表扫逐像素同答案，包括块间留白与章尾之外。
#[test]
fn binary_line_at_matches_the_scan_on_a_real_chapter() {
    let layout = a_real_layout();
    let mut probes = vec![-10.0, 0.0, layout.height + 40.0];
    for l in &layout.lines {
        probes.extend([l.y - 0.5, l.y, l.y + l.height / 2.0, l.y + l.height - 0.5]);
    }
    // 留白也要探到：行与行之间那些没人覆盖的 y，答案必须是 `None`，而不是夹一行出来。
    for pair in layout.lines.windows(2) {
        probes.push(pair[0].y + pair[0].height + 0.1);
    }
    for y in probes {
        assert_eq!(
            layout.line_at(y),
            scan_reference::line_at(&layout, y),
            "y = {y} 处二分与整表扫给的不是同一行"
        );
    }
}

/// 「没有覆盖者给上方最近那一行」是 `anchor::line_at_y` 的口径，索引化之后必须一模一样。
#[test]
fn line_covering_or_above_matches_the_anchor_scan() {
    let layout = a_real_layout();
    for &y in &[
        -5.0,
        0.0,
        1.0,
        layout.height / 2.0,
        layout.height - 1.0,
        layout.height,
        layout.height + 500.0,
    ] {
        assert_eq!(
            layout.line_covering_or_above(y),
            scan_reference::line_covering_or_above(&layout, y),
            "y = {y}"
        );
    }
    // 超出末尾：给最后一行，而不是行数（行数没有对应的行盒，调用方拿它去索引就是越界）。
    assert_eq!(
        layout.line_covering_or_above(layout.height + 500.0),
        layout.lines.len() - 1
    );
    // 空排版（窗口第一帧之前）：0，不是 panic。
    assert_eq!(Layout::default().line_covering_or_above(12.0), 0);
}

/// 可见带必须**恰好**是与这条带有交集的那些行，不多不少，而且是一段连续下标。
///
/// 「不少」= 屏上不缺行；「不多」= 这一帧不再为章里其余几千行花钱
/// （`plans/阅读管线.md` §十一 第 4 条要的就是这个）。窗口顶上只露出下半截的那一行
/// 也得在带里，否则最上面一行的字会缺半截。
#[test]
fn the_band_is_exactly_the_lines_touching_it() {
    let layout = a_real_layout();
    let middle = layout.height / 2.0;
    let window = 60.0;
    for (from, to) in [
        (0.0, 1.0),
        (0.0, layout.height),
        (middle, middle + window),
        (middle - 0.5, middle + 0.5),
        (layout.height - 1.0, layout.height),
        // 整条带在章下方：谁都不该进来。
        (layout.height + 10.0, layout.height + 80.0),
    ] {
        let got = layout.lines_in_band(from, to);
        let want = scan_reference::band(&layout, from, to);
        let listed: Vec<usize> = got.clone().collect();
        assert_eq!(listed, want, "带 [{from}, {to}) 与行的交集不是这一段");
    }
    // 一帧的开销：可见一带的行数远小于整章行数，这才是「不再逐行比 y」的意义。
    let visible = layout.lines_in_band(middle, middle + window);
    assert!(
        !visible.is_empty() && visible.len() * 2 < layout.lines.len(),
        "带里装了半章以上的行，裁剔等于没生效：{} / {}",
        visible.len(),
        layout.lines.len()
    );
}

/// 空带与反带（调用方算错时）给全区间：它自己的逐行 y 比较照旧把结果裁对，
/// 这里只是没省下来 —— 而不是给一个空区间让屏幕上少一片字。
#[test]
fn a_degenerate_band_falls_back_to_every_line() {
    let layout = a_real_layout();
    let middle = layout.height / 2.0;
    assert_eq!(layout.lines_in_band(middle, middle), 0..layout.lines.len());
    assert_eq!(
        layout.lines_in_band(middle + 10.0, middle),
        0..layout.lines.len()
    );
}

/// 行盒不叠放（手工拼的表；将来接了负外边距 / 绝对定位也会是这种）时，按 y 的查询退回
/// 整表扫，答案必须仍然与参照实现一致 —— 这一条钉的是「宁可慢，不可答错」。
#[test]
fn unstacked_lines_fall_back_to_the_scan_and_stay_right() {
    // 故意把第二条摆在第一条上面：`stacked` 一量就是假。
    let layout = Layout::assemble(
        vec![block(7, 0..2, true), block(9, 1..2, true)],
        vec![line(7, 100.0, 20.0), line(9, 10.0, 20.0)],
        100.0,
        200.0,
    );
    assert!(!layout.stacked());
    for &y in &[5.0, 15.0, 105.0, 300.0] {
        assert_eq!(
            layout.line_at(y),
            scan_reference::line_at(&layout, y),
            "不叠放时二分给出的是别的一行：y = {y}"
        );
        assert_eq!(
            layout.line_covering_or_above(y),
            scan_reference::line_covering_or_above(&layout, y),
            "y = {y}"
        );
    }
    assert_eq!(layout.line_at(105.0), Some(0));
    // 带退回全区间。
    assert_eq!(layout.lines_in_band(0.0, 200.0), 0..2);
    // 归属表与叠放性无关：先序规则照旧。
    assert_eq!(layout.block_of_line(1).map(|b| b.node), Some(9));
    assert_eq!(layout.block_of_line(0).map(|b| b.node), Some(7));
}

/// 同一区间被两条块盒注册（边框盒 + `wrap_paragraph` 为正文映射补的内容盒副本）时，
/// 表里留的是**后入表**那一条 —— 与接线前那句 `blocks.iter().rev().find(...)` 同一条规则。
#[test]
fn the_later_registration_of_a_range_keeps_the_slot() {
    let layout = Layout::assemble(
        vec![block(3, 0..1, true), block(3, 0..1, false)],
        vec![line(3, 0.0, 18.0)],
        100.0,
        20.0,
    );
    let owner = layout.block_of_line(0).expect("这一行该有归属");
    assert!(
        !owner.border_box,
        "留下的该是后入表的那条注册，绘制层据此把装饰与文本分开问"
    );
}

/// 祖先包含子孙的区间时，叶子块赢：这正是「底色画在段上而不是整篇上」那条规则。
#[test]
fn the_deepest_block_wins_a_shared_line() {
    let layout = Layout::assemble(
        vec![
            block(1, 0..3, true), // <div>：三行都算它的
            block(2, 0..2, true), // <p>：前两行
            block(3, 1..2, true), // <em> 折出来的第二行只归它
        ],
        vec![line(2, 0.0, 18.0), line(3, 18.0, 18.0), line(1, 36.0, 18.0)],
        100.0,
        60.0,
    );
    assert_eq!(layout.block_of_line(0).map(|b| b.node), Some(2));
    assert_eq!(layout.block_of_line(1).map(|b| b.node), Some(3));
    assert_eq!(layout.block_of_line(2).map(|b| b.node), Some(1));
    for i in 0..3 {
        assert_eq!(
            layout.block_of_line(i).map(|b| b.node),
            scan_reference::block_of_line(&layout, i)
                .and_then(|ix| layout.blocks.get(ix))
                .map(|b| b.node),
            "第 {i} 行与倒着扫不同"
        );
    }
}

/// 零行盒的表（轻量排版、空章）：折表是空的，三条查询都不许 panic。
#[test]
fn a_layout_without_lines_still_answers_every_query() {
    let empty = Layout::assemble(vec![block(1, 0..0, true)], Vec::new(), 260.0, 40.0);
    assert!(empty.lines.is_empty());
    assert!(empty.stacked(), "零行的表当然算叠放");
    assert!(empty.line_at(0.0).is_none());
    assert!(empty.block_of_line(0).is_none());
    assert_eq!(empty.line_covering_or_above(0.0), 0);
    assert_eq!(empty.lines_in_band(0.0, 10.0).len(), 0);
}

// ---- 装饰短表：折一次，而不是每帧走整章（§十一 第 12 条）----

use crate::content::format::css::computed::{BorderEdge, Color, ComputedStyle, StyledDom};

/// 一条带几何的块盒（`block()` 那个助手全给 y=0，装饰要按 y/height 判，这里要能指定）。
fn box_at(node: u32, y: f32, height: f32, border_box: bool) -> BlockBox {
    BlockBox {
        node,
        x: 4.0,
        y,
        width: 300.0,
        height,
        text: String::new(),
        lines: 0..0,
        border_box,
    }
}

/// 接线前那趟每帧扫描照抄的参照实现：走**全表**、逐条用同样三条筛子、同样的色折法。
/// 拿参照实现比而不是挑几个手算的期望值，理由与本文件开头那条一样 —— 会漂的是边界口径。
fn reference_seeds(layout: &Layout, styles: &StyledDom) -> Vec<DecorSeed> {
    let mut out = Vec::new();
    for block in &layout.blocks {
        if !block.border_box {
            continue;
        }
        let Some(style) = styles.get(block.node) else {
            continue;
        };
        let drawn = [
            style.border[0].drawn(),
            style.border[1].drawn(),
            style.border[2].drawn(),
            style.border[3].drawn(),
        ];
        if style.background_color.is_none() && drawn.iter().all(|w| *w <= 0.0) {
            continue;
        }
        if block.width <= 0.0 || block.height <= 0.0 {
            continue;
        }
        out.push(DecorSeed {
            x: block.x,
            y: block.y,
            width: block.width,
            height: block.height,
            background: style.background_color,
            border: [
                (drawn[0], style.border[0].color.or(style.color)),
                (drawn[1], style.border[1].color.or(style.color)),
                (drawn[2], style.border[2].color.or(style.color)),
                (drawn[3], style.border[3].color.or(style.color)),
            ],
        });
    }
    out
}

/// 一份按节点下标摆好的样式表（`StyledDom::get` 就是按 `NodeId` 当 `styles` 下标）。
fn decorated(by_node: &[(u32, ComputedStyle)]) -> StyledDom {
    let top = by_node.iter().map(|(id, _)| *id).max().unwrap_or(0) as usize + 1;
    let mut styles = vec![ComputedStyle::default(); top];
    for (id, style) in by_node {
        styles[*id as usize] = style.clone();
    }
    StyledDom { styles, ignored: 0 }
}

/// 折出来的短表必须与那趟每帧全表扫描**逐条相同**，包括顺序。
#[test]
fn the_decor_table_matches_the_per_frame_full_scan() {
    let bg = ComputedStyle {
        background_color: Some(Color(193, 204, 192)),
        ..ComputedStyle::default()
    };
    let mut bordered = ComputedStyle::default();
    bordered.border[2] = BorderEdge {
        width: 2.0,
        style: crate::content::format::css::computed::BorderStyle::Solid,
        color: None,
    };
    bordered.color = Some(Color(12, 34, 56));
    // 节点 5 有样式但什么装饰都没写 —— 不该进表。
    let plain = ComputedStyle::default();

    let layout = Layout::assemble(
        vec![
            box_at(1, 0.0, 120.0, true),   // 底色
            box_at(2, 120.0, 300.0, true), // 下边框，色走 currentColor
            box_at(3, 420.0, 60.0, true),  // 无装饰
            box_at(3, 420.0, 60.0, false), // 同一节点的内容盒副本
            box_at(4, 480.0, 0.0, true),   // 零高
            box_at(6, 480.0, 90.0, true),  // 层叠表里查不到的节点
        ],
        Vec::new(),
        336.0,
        600.0,
    );
    let sheet = decorated(&[(1, bg), (2, bordered), (3, plain.clone()), (5, plain)]);

    let got = layout.decor_seeds(&sheet);
    assert_eq!(
        got,
        reference_seeds(&layout, &sheet),
        "短表必须与全表扫描逐条相同"
    );
    assert_eq!(got.len(), 2, "只该有底色与下边框那两条：{got:?}");
    // currentColor 在折表时就折完了：那条下边框的色是块自己的文字色，不是 None。
    assert_eq!(got[1].border[2], (2.0, Some(Color(12, 34, 56))));
    // 主题色一格都没进表 —— 剩下的那步「None 落主题前景」是绘制侧每帧做的。
    assert_eq!(got[0].border[2], (0.0, None));
}

/// 零宽/零高、内容盒副本、无装饰、层叠查不到这四条筛子单独钉一遍。
/// 上面的参照对比已经涵盖，这几条是「谁将来手改筛子时最容易被顺手放宽」的那四条。
#[test]
fn each_decor_filter_still_fires_on_its_own() {
    let bg = ComputedStyle {
        background_color: Some(Color(1, 2, 3)),
        ..ComputedStyle::default()
    };
    let one = |b: BlockBox| Layout::assemble(vec![b], Vec::new(), 336.0, 600.0);
    let sheet = decorated(&[(1, bg.clone()), (2, bg)]);

    assert_eq!(one(box_at(1, 0.0, 50.0, true)).decor_seeds(&sheet).len(), 1);
    // 内容盒副本
    assert!(
        one(box_at(1, 0.0, 50.0, false))
            .decor_seeds(&sheet)
            .is_empty()
    );
    // 零高
    let mut flat = box_at(1, 0.0, 0.0, true);
    assert!(one(flat.clone()).decor_seeds(&sheet).is_empty());
    // 零宽
    flat.height = 50.0;
    flat.width = 0.0;
    assert!(one(flat).decor_seeds(&sheet).is_empty());
    // 层叠表里查不到的节点
    assert!(
        one(box_at(9, 0.0, 50.0, true))
            .decor_seeds(&sheet)
            .is_empty()
    );
    // 有样式但没写任何装饰
    let bare = decorated(&[]);
    assert!(
        one(box_at(1, 0.0, 50.0, true))
            .decor_seeds(&bare)
            .is_empty()
    );
}
