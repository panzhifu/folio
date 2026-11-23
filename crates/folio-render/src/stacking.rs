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
//! 取**最近**那一个的 z。都不满足时 z=0 —— 与「没有层叠需求」同值，绘制走原序，零回归。
//!
//! 边界（如实记录）：这是**扁平**的层叠序 —— 嵌套层叠上下文取最近那一层的 z，不构造 CSS
//! 规范里那棵 stacking context 树（外层 z 带着整棵子树一起排的语义未建模）。负 z 排在
//! 正文前、正 z 排在正文后、`opacity` / `transform` 建上下文隔离都已覆盖；书的样本只有
//! 单层（海报 z=0、标题 z=1），所以这一档足够。真正到了多层嵌套且需要严格 CSS 顺序的书，
//! 这一处要升级成「从根到 owner 的上下文 z 链」做字典序比较，而不是单个 i32。

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

/// 一条 owner 行的有效绘制 z：从 owner 往根走，取最近建立上下文那格的 z。
fn effective_z(dom: &Dom, styles: &StyledDom, owner: NodeId) -> i32 {
    for node in dom.ancestors_including_self(owner) {
        if let Some(style) = styles.get(node)
            && let Some(z) = establishes(style)
        {
            return z;
        }
    }
    0
}

/// 为一份排版结果算每行 z 值表。与 `lines` 等长。
///
/// 全部为 0 时返回的 `Vec` 仍全 0 —— 绘制层据此判定「无层叠需求」，走原序。
pub(super) fn compute_line_z(dom: &Dom, styles: &StyledDom, layout: &Layout) -> Vec<i32> {
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
