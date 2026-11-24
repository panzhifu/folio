//! 层叠序：每条绘制行沿祖先链找到最近「建立层叠上下文」的元素，取其 z-index 作为绘制序。
//!
//! 正文/命中仍按 `Layout::lines` 的源序（`line_at`、分片、选区都不变），这里只给绘制那一趟
//! 一张 z 值表：绘制层按 `(z, 源序)` 稳定排序后再画。于是 `position` 元素、z-index、opacity、
//! transform 能真正改变谁盖住谁，而字节偏移锚点、行索引、分页一律不受影响。
//!
//! 建立层叠上下文的判据（本子集）：
//! - 定位元素（`absolute` / `fixed` / `relative`）且 z-index ≠ 0；
//! - `opacity < 1`；
//! - `transform ≠ none`。
//!
//! 每行存的是**从根到 owner 的 z 链**（外层在前、最近一层在后），绘制按链做字典序比较：
//! 外层 z 先分胜负，外层相同再比内层 —— 这就是 CSS `stacking context` 树的扁平化表示。
//! 没有建立上下文时存 `None`（与全 0 同义，绘制走源序，零回归）。

use std::sync::Arc;

use folio_format::css::computed::{Positioning, StyledDom};
use folio_format::dom::{Dom, NodeId};

use crate::geometry::Layout;

/// 这个节点自己建立层叠上下文吗，建立的话返回它的 z-index。
fn establishes(style: &folio_format::css::computed::ComputedStyle) -> Option<i32> {
    let positioned = !matches!(style.position, Positioning::Static);
    if positioned && style.z_index != 0 {
        return Some(style.z_index);
    }
    if style.opacity < 1.0 || style.transform.is_some() {
        // opacity/transform 建的上下文等价于 z=0（它管的是**隔离**，不是抬层）。
        return Some(style.z_index);
    }
    None
}

/// 一条 owner 行的层叠上下文 z 链：从根到 owner，依次收集每个建立上下文那格的 z。
/// 一个上下文都没有时给 `None`。
fn effective_z(dom: &Dom, styles: &StyledDom, owner: NodeId) -> Option<Arc<[i32]>> {
    let mut path = Vec::new();
    for node in dom.ancestors_including_self(owner) {
        if let Some(style) = styles.get(node)
            && let Some(z) = establishes(style)
        {
            path.push(z);
        }
    }
    if path.is_empty() {
        return None;
    }
    // `ancestors_including_self` 是 owner→根，翻成根→owner。
    path.reverse();
    Some(Arc::from(path))
}

/// 为一份排版结果算每行 z 链。与 `lines` 等长；全 `None` = 无层叠需求。
pub(super) fn compute_line_z(
    dom: &Dom,
    styles: &StyledDom,
    layout: &Layout,
) -> Vec<Option<Arc<[i32]>>> {
    layout
        .lines
        .iter()
        .map(|line| effective_z(dom, styles, line.owner))
        .collect()
}

#[cfg(test)]
mod tests {
    // 这里只放纯逻辑的单测；端到端的「谁盖住谁」在 folio-render/tests.rs 用 line_z 断言。
    use super::*;
    use folio_format::css::computed::Transform;

    #[test]
    fn default_style_does_not_establish_a_context() {
        let s = folio_format::css::computed::ComputedStyle::default();
        assert_eq!(establishes(&s), None);
    }

    #[test]
    fn positioned_with_nonzero_z_establishes() {
        let mut s = folio_format::css::computed::ComputedStyle::default();
        s.position = Positioning::Relative;
        s.z_index = 3;
        assert_eq!(establishes(&s), Some(3));
    }

    #[test]
    fn opacity_and_transform_establish_at_their_z() {
        let mut s = folio_format::css::computed::ComputedStyle::default();
        s.opacity = 0.5;
        assert_eq!(establishes(&s), Some(0));
        s.opacity = 1.0;
        s.transform = Some(Transform::Translate(
            folio_format::css::computed::Length::ZERO,
            folio_format::css::computed::Length::ZERO,
        ));
        assert_eq!(establishes(&s), Some(0));
    }
}
