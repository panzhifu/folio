//! 网格布局的集成用例：列宽、显式行/列、自动放置与对齐。
//! 从 `tests.rs` 分出，只因为那个文件贴着行数上限；共享 helper 经 `super` 取用。

use super::{find, typeset};
use crate::{Dom, Layout};
use folio_format::dom::NodeId;

/// 容器各子元素的边框盒（x, y, w, h），按 DOM 顺序。
fn child_boxes(dom: &Dom, laid: &Layout, container: NodeId) -> Vec<(f32, f32, f32, f32)> {
    dom.element_children(container)
        .filter_map(|id| {
            laid.blocks
                .iter()
                .find(|b| b.node == id && b.border_box)
                .map(|b| (b.x, b.y, b.width, b.height))
        })
        .collect()
}

/// `1fr 1fr` 两列平分内容宽。
#[test]
fn grid_two_fr_tracks_split_the_content_width() {
    let (dom, laid) = typeset(
        "<div class='g'><div>甲</div><div>乙</div></div>",
        ".g { display: grid; grid-template-columns: 1fr 1fr; column-gap: 0 } .g > div { margin: 0 }",
        200.0,
    );
    let grid = find(&dom, "div");
    let boxes = child_boxes(&dom, &laid, grid);
    assert_eq!(boxes.len(), 2);
    assert!((boxes[0].2 - 100.0).abs() < 1.0, "第一列 100：{boxes:?}");
    assert!((boxes[1].2 - 100.0).abs() < 1.0, "第二列 100：{boxes:?}");
    assert!((boxes[0].0 - 0.0).abs() < 1.0 && (boxes[1].0 - 100.0).abs() < 1.0);
}

/// 固定轨道 + `fr`：固定先拿，剩余按 fr 分。
#[test]
fn grid_fixed_track_takes_its_width_first() {
    let (dom, laid) = typeset(
        "<div class='g'><div>甲</div><div>乙</div></div>",
        ".g { display: grid; grid-template-columns: 60px 1fr } .g > div { margin: 0 }",
        200.0,
    );
    let grid = find(&dom, "div");
    let boxes = child_boxes(&dom, &laid, grid);
    assert!((boxes[0].2 - 60.0).abs() < 1.0, "固定列 60：{boxes:?}");
    assert!((boxes[1].2 - 140.0).abs() < 1.0, "余下 140：{boxes:?}");
}

/// 显式 `grid-column` 落位；`justify-self: end` 在格子里靠右。
#[test]
fn grid_explicit_column_and_justify_self_end() {
    let (dom, laid) = typeset(
        "<div class='g'><div class='a'>式</div><div class='n'>(1)</div></div>",
        ".g { display: grid; grid-template-columns: 1fr auto; column-gap: 8px } .g > div { margin: 0 } .n { grid-column: 2; justify-self: end; width: 30px }",
        200.0,
    );
    let grid = find(&dom, "div");
    let boxes = child_boxes(&dom, &laid, grid);
    assert_eq!(boxes.len(), 2);
    let (x, _, w, _) = boxes[1];
    assert!((x + w - 200.0).abs() < 1.0, "靠右：{boxes:?}");
}

/// 自动放置按行主序换行：两个子项填满第一行后，第三个从下一行开始。
#[test]
fn grid_auto_placement_wraps_to_the_next_row() {
    let (dom, laid) = typeset(
        "<div class='g'><div>甲</div><div>乙</div><div>丙</div></div>",
        ".g { display: grid; grid-template-columns: 1fr 1fr } .g > div { margin: 0 }",
        200.0,
    );
    let grid = find(&dom, "div");
    let boxes = child_boxes(&dom, &laid, grid);
    assert_eq!(boxes.len(), 3);
    assert!(
        (boxes[0].1 - boxes[1].1).abs() < 0.5,
        "前两个同一行：{boxes:?}"
    );
    assert!(boxes[2].1 > boxes[0].1, "第三个换行：{boxes:?}");
}

/// 显式 `grid-row` 把子项放到后面的行，自动项仍从第一行开始填。
#[test]
fn grid_row_places_an_item_on_a_later_row() {
    let (dom, laid) = typeset(
        "<div class='g'><div class='r2'>甲</div><div>乙</div></div>",
        ".g { display: grid; grid-template-columns: 1fr 1fr } .g > div { margin: 0 } .r2 { grid-row: 2 }",
        200.0,
    );
    let grid = find(&dom, "div");
    let boxes = child_boxes(&dom, &laid, grid);
    assert_eq!(boxes.len(), 2);
    assert!(boxes[0].1 > boxes[1].1, "显式第二行在自动项下方：{boxes:?}");
}

/// `grid-row: span 2` 占住两行，行高不够时把缺口均摊进去。
#[test]
fn grid_row_span_covers_two_rows() {
    let (dom, laid) = typeset(
        "<div class='g'><div class='s'>高</div><div>甲</div><div>乙</div></div>",
        ".g { display: grid; grid-template-columns: 1fr 1fr } .g > div { margin: 0 } .s { grid-row: span 2; font-size: 48px }",
        200.0,
    );
    let grid = find(&dom, "div");
    let boxes = child_boxes(&dom, &laid, grid);
    let container = laid
        .blocks
        .iter()
        .find(|b| b.node == grid && b.border_box)
        .unwrap();
    assert!(
        container.height + 0.5 >= boxes[0].3,
        "两行合起来要撑住跨行项：{container:?} {boxes:?}"
    );
}
