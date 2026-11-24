//! 块级多栏容器（`columns` / `column-count` / `column-width`）的纯尺寸数学。
//!
//! 栏数解算与配平这两个数不碰排版表，独立可测；盒子摆放与平移在
//! `collect::columns_layout`。降级口径写在 `ComputedStyle::column_count` 上。

use folio_format::css::computed::ComputedStyle;

/// 多栏容器的解算结果：栏数与栏缝。
///
/// `gap` 是折好的 px（`column-gap` 没写时取 `normal` = 1em）；`count ≥ 2` 才是
/// 多栏，折出 1 栏的容器退回普通块流，与没写一样。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plan {
    pub count: u16,
    pub gap: f32,
}

/// 解一份多栏方案。`column-count` 优先；只写 `column-width` 时按 CSS 的
/// 「先塞满再折栏」公式折出栏数（`(盒 + 缝) / (栏 + 缝)` 向下取整，至少 1）。
/// 两条都没写（或宽度非正）不是多栏。
pub fn plan(count: Option<u16>, width: Option<f32>, gap: f32, content_w: f32) -> Option<Plan> {
    let gap = gap.max(0.0);
    let n = match (count, width) {
        (Some(n), _) => n,
        (None, Some(w)) if w > 0.0 && content_w.is_finite() => {
            (((content_w + gap) / (w + gap)).floor().max(1.0)) as u16
        }
        _ => return None,
    };
    (n >= 2).then_some(Plan { count: n, gap })
}

/// 从计算样式解一份多栏方案；缝没写取 `normal` = 1em（多栏语境的初值，与 Flex
/// 那一档「`normal` = 0」不同源，别共用一把尺）。
pub fn plan_for(style: &ComputedStyle, content_w: f32) -> Option<Plan> {
    let gap = style
        .column_gap
        .map(|l| l.resolve(content_w).max(0.0))
        .unwrap_or(style.font_size);
    plan(
        style.column_count,
        style.column_width.map(|l| l.resolve(content_w)),
        gap,
        content_w,
    )
}

/// 把一列已量好的单元高**贪心装进 `count` 栏**：目标栏高 = 总高 ÷ 栏数，
/// 当前栏装不下（且非空）就换下一栏；最后一栏收走全部剩余。
///
/// CSS 的配平是最小化最大栏高，那是 NP 难的一档；贪心按序装栏是它的
/// 一阶近似——单元按文档序进栏、栏内次序不乱，栏高差一截是允许的降级
/// （记在 `ComputedStyle::column_count` 的文档上）。权重带上各单元的
/// 上外边距，首栏不空时才不至于把第一个单元顶格。
pub fn balance(weights: &[f32], count: u16) -> Vec<Vec<usize>> {
    let mut columns: Vec<Vec<usize>> = (0..count).map(|_| Vec::new()).collect();
    if weights.is_empty() || count == 0 {
        return columns;
    }
    let total: f32 = weights.iter().sum();
    let target = total / count as f32;
    let mut col = 0usize;
    let mut filled = 0.0f32;
    for (i, &w) in weights.iter().enumerate() {
        let overflow = filled > 0.0 && filled + w > target;
        let last = col == columns.len() - 1;
        if overflow && !last {
            col += 1;
            filled = 0.0;
        }
        columns[col].push(i);
        filled += w;
    }
    columns
}

#[cfg(test)]
mod tests {
    use super::{Plan, balance, plan};

    #[test]
    fn count_wins_and_width_derives_the_column_number() {
        assert_eq!(
            plan(Some(2), None, 16.0, 600.0),
            Some(Plan {
                count: 2,
                gap: 16.0
            })
        );
        // 只写栏宽：616 / (200+16) = 2.85 → 2 栏
        assert_eq!(
            plan(None, Some(200.0), 16.0, 600.0).map(|p| p.count),
            Some(2)
        );
        // 栏宽太宽折出 1 栏：不是多栏，退普通流
        assert_eq!(plan(None, Some(600.0), 16.0, 600.0), None);
        // 两条都没写：不是多栏
        assert_eq!(plan(None, None, 16.0, 600.0), None);
        // 非正宽不折栏
        assert_eq!(plan(None, Some(0.0), 16.0, 600.0), None);
    }

    #[test]
    fn balance_fills_a_column_until_the_target_then_moves_on() {
        // 4 个 100 高的单元装 2 栏：目标 200，每栏恰好 2 个。
        assert_eq!(balance(&[100.0; 4], 2), vec![vec![0, 1], vec![2, 3]]);
        // 3 个装 2 栏：目标 150 装不下第二个 100，后两格进下一栏 —— 高度差一截
        // 是允许的贪心结果（最优解的最大栏高同为 200）。
        assert_eq!(
            balance(&[100.0, 100.0, 100.0], 2),
            vec![vec![0], vec![1, 2]]
        );
        // 单元比目标还高：空栏也直接收它（不跨栏拆单元）。
        assert_eq!(balance(&[500.0, 50.0], 2), vec![vec![0], vec![1]]);
        // 最后一栏收走全部剩余：溢出不再换栏（60×5、目标 150 → 2+3 格）。
        assert_eq!(balance(&[60.0; 5], 2), vec![vec![0, 1], vec![2, 3, 4]]);
        // 空表给空栏，不 panic。
        let empty: Vec<Vec<usize>> = balance(&[], 3);
        let want: Vec<Vec<usize>> = vec![vec![], vec![], vec![]];
        assert_eq!(empty, want);
    }
}
