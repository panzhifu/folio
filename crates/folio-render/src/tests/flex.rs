//! Flex 容器（`display: flex` 一族）的集成用例。
//! 从 `tests.rs` 分出，只因为那个文件贴着行数上限；共享 helper 经 `super` 取用。

use super::{Mono, find, typeset};
use crate::{Dom, Layout, LayoutOptions, NoImages, layout};

// ---- Flex 容器（`display: flex` 一族，2026-11-23）----
//
// 度量桩：汉字 16px、拉丁 8px、行高 28。Flex 子项按主轴排，坐标全在逻辑域，
// 竖排由 Layout::assemble 转置，所以同一套断言横竖共用。

/// 按 tag 取子项的边框盒 `(x, y, w, h)`，按 CSS `order`-modified 视觉序。
/// 函数名保留 `flex_boxes` 是为旧用例少改；返回顺序用页面上的左右/上下位置来定。
fn flex_boxes(dom: &Dom, laid: &Layout, tag_name: &str) -> Vec<(f32, f32, f32, f32)> {
    laid.blocks
        .iter()
        .filter(|b| b.border_box && dom.tag_name(b.node) == Some(tag_name))
        .map(|b| (b.x, b.y, b.width, b.height))
        .collect()
}

#[test]
fn flex_row_places_items_side_by_side() {
    let html = "<div><i>甲</i><i>乙</i><i>丙</i></div>";
    let css = "div { display: flex } i { font-size: 16px }";
    let (dom, laid) = typeset(html, css, 200.0);
    let boxes = flex_boxes(&dom, &laid, "i");
    assert_eq!(boxes.len(), 3, "三个 Flex 子项各有边框盒");
    // 每项固有宽 16，不 grow、不 justify → 贴起点依次排：0/16/32，同一行 y。
    assert_eq!(boxes[0].0, 0.0);
    assert_eq!(boxes[1].0, 16.0);
    assert_eq!(boxes[2].0, 32.0);
    assert_eq!(boxes[0].1, boxes[1].1, "同一行 y 相同");
}

#[test]
fn flex_row_gap_inserts_space_between_items() {
    let html = "<div><i>甲</i><i>乙</i></div>";
    let css = "div { display: flex; gap: 0 10px }";
    let (dom, laid) = typeset(html, css, 200.0);
    let boxes = flex_boxes(&dom, &laid, "i");
    // 两项各 16，column-gap=10 → 第二项起点 16+10=26。
    assert_eq!(boxes[0].0, 0.0);
    assert_eq!(boxes[1].0, 26.0, "gap 是主轴上的缝");
}

#[test]
fn flex_row_grow_splits_free_space() {
    let html = "<div><i>甲</i><i>乙</i></div>";
    let css = "div { display: flex } i { flex: 1 }";
    let (dom, laid) = typeset(html, css, 200.0);
    let boxes = flex_boxes(&dom, &laid, "i");
    // 两项 grow=1、basis=auto(16)，自由 200-32=168 平分 → 各 16+84=100。
    assert_eq!(boxes[0].2, 100.0, "第一项宽 100");
    assert_eq!(boxes[1].2, 100.0, "第二项宽 100");
    assert_eq!(boxes[1].0, 100.0, "第二项紧接第一项");
}

#[test]
fn flex_row_space_between_pushes_items_to_edges() {
    let html = "<div><i>甲</i><i>乙</i></div>";
    let css = "div { display: flex; justify-content: space-between }";
    let (dom, laid) = typeset(html, css, 200.0);
    let boxes = flex_boxes(&dom, &laid, "i");
    // 两项各 16，自由 168 全摊到中间那一道缝 → 第二项起点 200-16=184。
    assert_eq!(boxes[0].0, 0.0);
    assert_eq!(boxes[1].0, 184.0);
}

#[test]
fn flex_basis_percent_resolves_against_container() {
    let html = "<div><i>甲</i><i>乙</i></div>";
    let css = "div { display: flex } i { flex: 0 1 34% }";
    let (dom, laid) = typeset(html, css, 200.0);
    let boxes = flex_boxes(&dom, &laid, "i");
    // 每项 basis 34% × 200 = 68；grow=0 → 不撑开，各 68。
    assert_eq!(boxes[0].2, 68.0);
    assert_eq!(boxes[1].2, 68.0);
    assert_eq!(boxes[1].0, 68.0);
}

#[test]
fn flex_column_stacks_items_vertically() {
    let html = "<div><i>甲</i><i>乙</i></div>";
    let css = "div { display: flex; flex-direction: column }";
    let (dom, laid) = typeset(html, css, 200.0);
    let boxes = flex_boxes(&dom, &laid, "i");
    assert_eq!(boxes.len(), 2);
    assert_eq!(boxes[0].0, boxes[1].0, "column 下交叉轴对齐起点相同");
    assert!(boxes[1].1 > boxes[0].1, "column 下第二项在第一项下面");
}

#[test]
fn flex_row_wrap_breaks_into_lines() {
    let html = "<div><i>甲</i><i>乙</i><i>丙</i></div>";
    let css = "div { display: flex; flex-wrap: wrap } i { flex: 0 1 50px }";
    let (dom, laid) = typeset(html, css, 90.0);
    let boxes = flex_boxes(&dom, &laid, "i");
    // 每项 basis 50，版心 90 → 一行放不下一对(100)，第二个子项换到下一行。
    assert!(boxes[1].1 > boxes[0].1, "第二个子项换到下一行");
}

#[test]
fn flex_row_reverse_mirrors_the_main_axis() {
    let html = "<div><i class='a'>甲</i><i class='b'>乙</i></div>";
    let css = "div { display: flex; flex-direction: row-reverse }";
    let (dom, laid) = typeset(html, css, 200.0);
    let container = find(&dom, "div");
    let mut children = dom.element_children(container);
    let a = children.next().unwrap();
    let b = children.next().unwrap();
    let box_of = |node| {
        laid.blocks
            .iter()
            .find(|block| block.node == node && block.border_box)
            .unwrap()
    };
    // 源序 a,b；反向后 b 贴起点，a 贴在 b 右侧。DOM 表不变，命中与选区仍按源序。
    assert!(
        box_of(a).x > box_of(b).x,
        "反向后第一个源序项在右边：{:?} {:?}",
        box_of(a),
        box_of(b)
    );
}

#[test]
fn flex_order_changes_visual_but_not_source_order() {
    let html = "<div><i class='a'>甲</i><i class='b'>乙</i></div>";
    let css = "div { display: flex } .a { order: 2 } .b { order: -1 }";
    let (dom, laid) = typeset(html, css, 200.0);
    let container = find(&dom, "div");
    let mut children = dom.element_children(container);
    let a = children.next().unwrap();
    let b = children.next().unwrap();
    let box_of = |node| {
        laid.blocks
            .iter()
            .find(|block| block.node == node && block.border_box)
            .unwrap()
    };
    assert!(
        box_of(b).x < box_of(a).x,
        "order 小的 b 先摆：{:?} {:?}",
        box_of(a),
        box_of(b)
    );
}

#[test]
fn flex_column_reverse_stacks_from_the_bottom() {
    let html = "<div><i class='a'>甲</i><i class='b'>乙</i></div>";
    let css = "div { display: flex; flex-direction: column-reverse; height: 100px }";
    let (dom, laid) = typeset(html, css, 200.0);
    let container = find(&dom, "div");
    let mut children = dom.element_children(container);
    let a = children.next().unwrap();
    let b = children.next().unwrap();
    let box_of = |node| {
        laid.blocks
            .iter()
            .find(|block| block.node == node && block.border_box)
            .unwrap()
    };
    // 反向 column：源序第一项从底部起，所以 a 在下、b 在上。
    assert!(
        box_of(a).y > box_of(b).y,
        "反向 column 把源序第一项放在下面：{:?} {:?}",
        box_of(a),
        box_of(b)
    );
}

#[test]
fn flex_justify_space_evenly_spreads_all_gaps() {
    let html = "<div><i>甲</i><i>乙</i></div>";
    let css = "div { display: flex; justify-content: space-evenly }";
    let (dom, laid) = typeset(html, css, 200.0);
    let boxes = flex_boxes(&dom, &laid, "i");
    // 每项 16，自由 168；三道等缝各 56 → 起点 56 与 56+16+56=128。
    assert!((boxes[0].0 - 56.0).abs() < 0.01, "{boxes:?}");
    assert!((boxes[1].0 - 128.0).abs() < 0.01, "{boxes:?}");
}

#[test]
fn flex_align_content_space_between_spreads_multiple_lines() {
    let html = "<div><i>甲</i><i>乙</i><i>丙</i></div>";
    let css = "div { display: flex; flex-wrap: wrap; align-content: space-between; height: 100px } i { flex: 0 1 50px }";
    let (dom, laid) = typeset(html, css, 90.0);
    let boxes = flex_boxes(&dom, &laid, "i");
    assert!(boxes[1].1 > boxes[0].1, "前两项在第一行");
    assert!(boxes[2].1 > boxes[1].1, "第三项在第二行");
    assert!(
        boxes[2].1 + boxes[2].3 > boxes[0].1 + 84.0,
        "space-between 把两行拉开到容器上下：{boxes:?}"
    );
}

#[test]
fn flex_align_items_baseline_aligns_first_lines() {
    let html = "<div><i class='a'>甲</i><i class='b'>乙</i></div>";
    let css = "div { display: flex; align-items: baseline } .a { font-size: 40px } .b { font-size: 16px }";
    let (dom, laid) = typeset(html, css, 200.0);
    let boxes = flex_boxes(&dom, &laid, "i");
    let baseline = |node| {
        laid.lines
            .iter()
            .find(|l| l.owner == node)
            .map(|l| l.y + l.baseline)
    };
    let container = find(&dom, "div");
    let mut children = dom.element_children(container);
    let a = children.next().unwrap();
    let b = children.next().unwrap();
    assert_eq!(baseline(a), baseline(b), "两项首行基线要对齐：{boxes:?}");
}

#[test]
fn flex_in_vertical_writing_mode_transposes_to_columns() {
    let html = "<div><i>甲</i><i>乙</i></div>";
    let css = "body { writing-mode: vertical-rl } div { display: flex } i { flex: 1 }";
    let (dom, laid) = typeset(html, css, 200.0);
    let boxes = flex_boxes(&dom, &laid, "i");
    assert_eq!(boxes.len(), 2);
    // 竖排整份结果被转置：逻辑主轴 x 变成物理竖直方向 → 两个子项在同一物理 x、
    // 不同的物理 y（沿列方向）；逻辑交叉 y 变成物理 x。
    assert!(
        (boxes[0].0 - boxes[1].0).abs() < 0.01,
        "竖排下同一 Flex 行的两项物理 x 相同（转置后）：
        {:?}",
        boxes
    );
    assert!(boxes[1].1 > boxes[0].1, "竖排下两项沿竖直主轴排开");
}

#[test]
fn flex_light_layout_agrees_with_full_on_height() {
    use folio_format::css::computed::{ComputedStyle, cascade};
    use folio_format::css::{Origin, RuleSet};
    let html = "<div><i>甲</i><i>乙</i><i>丙</i></div>";
    let css = "div { display: flex } i { flex: 1 }";
    let (_, full) = typeset(html, css, 200.0);
    let dom = Dom::parse(html.as_bytes());
    let styles = cascade(
        &dom,
        &RuleSet::parse(css, Origin::Author),
        &ComputedStyle::default(),
        "",
    );
    let light = layout(
        &dom,
        &styles,
        find(&dom, "body"),
        LayoutOptions {
            width: 200.0,
            light: true,
            allow_vertical: true,
            page_height: None,
            screen_width: None,
            screen_height: None,
        },
        &Mono,
        &NoImages,
    );
    assert_eq!(
        light.height, full.height,
        "轻量档与完整档对 Flex 容器的章高必须一致"
    );
}
