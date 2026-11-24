//! `columns` 块级多栏的端到端用例：两栏并排、栏缝、贪心配平进栏。
//! 纯数（栏数解算与配平）的单测在 `columns.rs`，这一份钉的是「真排出两栏」。

use super::{Mono, find, typeset};
use folio_format::dom::NodeId;

/// 四个 100 高的块装两栏：目标 200，前两个进第一栏、后两个进第二栏；
/// 第二栏的 x = 第一栏 + 栏宽 + 栏缝，两栏各自从容器顶往下排，栏内照常堆叠。
#[test]
fn columns_flow_side_by_side_and_balance_by_children() {
    let (dom, laid) = typeset(
        "<div class='cols'><p>一</p><p>二</p><p>三</p><p>四</p></div>",
        ".cols { columns: 2; column-gap: 20px; margin: 0 } \
         .cols p { display: block; height: 100px; margin: 0 }",
        400.0,
    );
    let box_of = |id: NodeId| {
        laid.blocks
            .iter()
            .find(|b| b.node == id)
            .unwrap_or_else(|| panic!("节点 {:?} 该有一条块记录", id))
    };
    let container = box_of(find(&dom, "div"));
    let kids: Vec<NodeId> = dom
        .elements()
        .filter(|(id, _)| dom.tag_name(*id) == Some("p"))
        .map(|(id, _)| id)
        .collect();
    assert_eq!(kids.len(), 4);
    let [a, b, c, d] = [
        box_of(kids[0]),
        box_of(kids[1]),
        box_of(kids[2]),
        box_of(kids[3]),
    ];

    // 栏宽 = (400 − 20) / 2 = 190：第一栏在容器内容原点，第二栏在 +210。
    let rel_x = |k: &crate::BlockBox| k.x - container.x;
    let rel_y = |k: &crate::BlockBox| k.y - container.y;
    assert_eq!(rel_x(&a), 0.0, "第一栏贴容器左缘");
    assert_eq!(rel_x(&b), 0.0, "配平把前两个单元都装进第一栏");
    assert_eq!(rel_x(&c), 210.0, "第二栏 = 栏宽 190 + 缝 20");
    assert_eq!(rel_x(&d), 210.0);
    assert_eq!(rel_y(&a), 0.0, "两栏都从容器顶往下排");
    assert_eq!(rel_y(&c), 0.0);
    assert_eq!(rel_y(&b) - rel_y(&a), 100.0, "栏内按块堆叠");
    assert_eq!(rel_y(&d) - rel_y(&c), 100.0);
    // 容器高 = 最高那根栏（两根都是 200）。
    assert_eq!(container.height, 200.0, "容器高由配平后的栏高决定");
}

/// 只写 `column-width` 时按 CSS 公式折栏数：400 的盒、200 的栏宽、40 的缝
/// 折出 1 栏 —— 退回普通块流，与没写一样。
#[test]
fn a_single_derived_column_falls_back_to_normal_flow() {
    let (dom, laid) = typeset(
        "<div class='cols'><p>一</p><p>二</p></div>",
        ".cols { column-width: 200px; column-gap: 40px; margin: 0 } \
         .cols p { height: 50px; margin: 0 }",
        400.0,
    );
    let container = laid
        .blocks
        .iter()
        .find(|b| b.node == find(&dom, "div"))
        .unwrap();
    let kids: Vec<NodeId> = dom
        .elements()
        .filter(|(id, _)| dom.tag_name(*id) == Some("p"))
        .map(|(id, _)| id)
        .collect();
    let box_of = |id: NodeId| laid.blocks.iter().find(|b| b.node == id).unwrap();
    let a = box_of(kids[0]);
    let b = box_of(kids[1]);
    assert_eq!(a.x, b.x, "一栏 = 不多栏：两个块上下堆");
    assert_eq!(b.y - a.y, 50.0);
    assert_eq!(container.height, 100.0);
}
