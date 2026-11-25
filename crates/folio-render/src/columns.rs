//! 块级多栏容器（`columns` / `column-count` / `column-width`）的纯尺寸数学。
//!
//! 栏数解算与配平这两个数不碰排版表，独立可测；盒子摆放与平移在
//! `collect::columns_layout`。降级口径写在 `ComputedStyle::column_count` 上。

use folio_format::css::computed::{ColumnFill, ComputedStyle};

/// 多栏容器的解算结果：栏数与栏缝。
///
/// `gap` 是折好的 px（`column-gap` 没写时取 `normal` = 1em）；`count ≥ 2` 才是
/// 多栏，折出 1 栏的容器退回普通块流，与没写一样。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plan {
    pub count: u16,
    pub gap: f32,
    /// `column-fill`：`Balance` 走最小化最大栏高，`Auto` 按序装满。
    pub fill: ColumnFill,
}

/// 解一份多栏方案。`column-count` 优先；只写 `column-width` 时按 CSS 的
/// 「先塞满再折栏」公式折出栏数（`(盒 + 缝) / (栏 + 缝)` 向下取整，至少 1）。
/// 两条都没写（或宽度非正）不是多栏。
pub fn plan(
    count: Option<u16>,
    width: Option<f32>,
    gap: f32,
    content_w: f32,
    fill: ColumnFill,
) -> Option<Plan> {
    let gap = gap.max(0.0);
    let n = match (count, width) {
        (Some(n), _) => n,
        (None, Some(w)) if w > 0.0 && content_w.is_finite() => {
            (((content_w + gap) / (w + gap)).floor().max(1.0)) as u16
        }
        _ => return None,
    };
    (n >= 2).then_some(Plan {
        count: n,
        gap,
        fill,
    })
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
        style.column_fill,
    )
}

/// 把一列已量好的单元高**贪心装进 `count` 栏**：目标栏高 = 总高 ÷ 栏数，
/// 当前栏装不下（且非空）就换下一栏；最后一栏收走全部剩余。
///
/// CSS 的配平是最小化最大栏高，那是 NP 难的一档；贪心按序装栏是它的
/// 一阶近似——单元按文档序进栏、栏内次序不乱，栏高差一截是允许的降级
/// （记在 `ComputedStyle::column_count` 的文档上）。权重带上各单元的
/// 上外边距，首栏不空时才不至于把第一个单元顶格。
pub fn balance(weights: &[f32], count: u16, fill: ColumnFill) -> Vec<Vec<usize>> {
    if weights.is_empty() || count == 0 {
        return (0..count).map(|_| Vec::new()).collect();
    }
    match fill {
        ColumnFill::Auto => pack(weights, weights.iter().sum::<f32>() / count as f32, count),
        ColumnFill::Balance => balanced(weights, count),
    }
}

/// 按 `target` 一栏高贪心装满：当前栏放不下（且非空）就换下一栏，最后一栏收走剩余。
fn pack(weights: &[f32], target: f32, count: u16) -> Vec<Vec<usize>> {
    let mut columns: Vec<Vec<usize>> = (0..count).map(|_| Vec::new()).collect();
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

/// 按 `target` 一栏高装，需要几栏。与 [`pack`] 同一套判据，只是不记账。
fn columns_for(weights: &[f32], target: f32) -> usize {
    let mut columns = 1usize;
    let mut filled = 0.0f32;
    for &w in weights {
        if filled > 0.0 && filled + w > target {
            columns += 1;
            filled = 0.0;
        }
        filled += w;
    }
    columns
}

/// `column-fill: balance`：二分「能装进 `count` 栏」的最小目标高，再按它装栏。
///
/// 单元不跨栏拆，所以下界是单个最高单元；32 轮足够把区间收到亚像素。
/// 与贪心按「总高 ÷ 栏数」装相比，这一档在单元高矮不一时真会挪那几个矮单元。
fn balanced(weights: &[f32], count: u16) -> Vec<Vec<usize>> {
    let total: f32 = weights.iter().sum();
    let tallest = weights.iter().copied().fold(0.0f32, f32::max);
    let (mut lo, mut hi) = (tallest, total.max(tallest));
    for _ in 0..32 {
        let mid = (lo + hi) * 0.5;
        if columns_for(weights, mid) <= count as usize {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    pack(weights, hi, count)
}

#[cfg(test)]
mod tests {
    use super::{Plan, balance, plan};
    use folio_format::css::computed::ColumnFill;

    #[test]
    fn count_wins_and_width_derives_the_column_number() {
        assert_eq!(
            plan(Some(2), None, 16.0, 600.0, ColumnFill::Balance),
            Some(Plan {
                count: 2,
                gap: 16.0,
                fill: ColumnFill::Balance,
            })
        );
        // 只写栏宽：616 / (200+16) = 2.85 → 2 栏
        assert_eq!(
            plan(None, Some(200.0), 16.0, 600.0, ColumnFill::Auto).map(|p| p.count),
            Some(2)
        );
        // 栏宽太宽折出 1 栏：不是多栏，退普通流
        assert_eq!(plan(None, Some(600.0), 16.0, 600.0, ColumnFill::Auto), None);
        // 两条都没写：不是多栏
        assert_eq!(plan(None, None, 16.0, 600.0, ColumnFill::Auto), None);
        // 非正宽不折栏
        assert_eq!(plan(None, Some(0.0), 16.0, 600.0, ColumnFill::Auto), None);
    }

    #[test]
    fn auto_fills_a_column_until_the_target_then_moves_on() {
        // 4 个 100 高的单元装 2 栏：目标 200，每栏恰好 2 个。
        assert_eq!(
            balance(&[100.0; 4], 2, ColumnFill::Auto),
            vec![vec![0, 1], vec![2, 3]]
        );
        // 3 个装 2 栏：目标 150 装不下第二个 100，后两格进下一栏 —— 高度差一截
        // 是允许的贪心结果（最优解的最大栏高同为 200）。
        assert_eq!(
            balance(&[100.0, 100.0, 100.0], 2, ColumnFill::Auto),
            vec![vec![0], vec![1, 2]]
        );
        // 单元比目标还高：空栏也直接收它（不跨栏拆单元）。
        assert_eq!(
            balance(&[500.0, 50.0], 2, ColumnFill::Auto),
            vec![vec![0], vec![1]]
        );
        // 最后一栏收走全部剩余：溢出不再换栏（60×5、目标 150 → 2+3 格）。
        assert_eq!(
            balance(&[60.0; 5], 2, ColumnFill::Auto),
            vec![vec![0, 1], vec![2, 3, 4]]
        );
        // 空表给空栏，不 panic。
        let empty: Vec<Vec<usize>> = balance(&[], 3, ColumnFill::Auto);
        let want: Vec<Vec<usize>> = vec![vec![], vec![], vec![]];
        assert_eq!(empty, want);
    }

    /// `balance` 找「能装进 N 栏的最小目标高」，单元高矮不一时会挪矮的那几个。
    #[test]
    fn balance_evens_out_the_tallest_column() {
        // 5 个 60：最优最大栏高 180（3+2 格），不是按 150 的 2+3。
        assert_eq!(
            balance(&[60.0; 5], 2, ColumnFill::Balance),
            vec![vec![0, 1, 2], vec![3, 4]]
        );
        // 100×3：最优最大栏高 200。
        assert_eq!(
            balance(&[100.0; 3], 2, ColumnFill::Balance),
            vec![vec![0, 1], vec![2]]
        );
        // 高矮不一时矮的那个跟着高的走，不按总高均分切开。
        assert_eq!(
            balance(&[200.0, 100.0], 2, ColumnFill::Balance),
            vec![vec![0], vec![1]]
        );
        // 空表同样给空栏。
        let want: Vec<Vec<usize>> = vec![vec![], vec![], vec![]];
        assert_eq!(balance(&[], 3, ColumnFill::Balance), want);
    }
}
