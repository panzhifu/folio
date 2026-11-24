//! `align-self` 的逐项覆盖：flex 行内交叉轴与 grid 行内纵向。
//! 从 `tests.rs` 分出，只因为那个文件贴着行数上限；共享 helper 经 `super` 取用。

use super::{find, typeset};
use crate::Layout;
use folio_format::dom::NodeId;

fn boxes(
    dom: &folio_format::dom::Dom,
    laid: &Layout,
    container: NodeId,
) -> Vec<(f32, f32, f32, f32)> {
    dom.element_children(container)
        .filter_map(|id| {
            laid.blocks
                .iter()
                .find(|b| b.node == id && b.border_box)
                .map(|b| (b.x, b.y, b.width, b.height))
        })
        .collect()
}

/// flex：容器 `align-items: start`，子项 `align-self: end` 贴整行底。
#[test]
fn flex_align_self_overrides_container_align_items() {
    let (dom, laid) = typeset(
        "<div><i class='t'>甲</i><i class='e'>乙</i></div>",
        "div { display: flex; align-items: start } i { font-size: 16px } .t { font-size: 40px } .e { align-self: end }",
        300.0,
    );
    let grid = find(&dom, "div");
    let boxes = boxes(&dom, &laid, grid);
    assert_eq!(boxes.len(), 2);
    assert!(boxes[1].1 > boxes[0].1, "小字那一项贴行底：{boxes:?}");
}

/// grid：同一行里 `align-self: end` 把子项压到行底。
#[test]
fn grid_align_self_overrides_container_align_items() {
    let (dom, laid) = typeset(
        "<div class='g'><div class='t'>甲</div><div class='e'>乙</div></div>",
        ".g { display: grid; grid-template-columns: 1fr 1fr; align-items: start } \
         .g > div { margin: 0 } .t { font-size: 40px } .e { font-size: 16px; align-self: end }",
        300.0,
    );
    let grid = find(&dom, "div");
    let boxes = boxes(&dom, &laid, grid);
    assert_eq!(boxes.len(), 2);
    assert!(boxes[1].1 > boxes[0].1, "第二项贴行底：{boxes:?}");
}
