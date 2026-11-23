//! Flex 子项的尺寸数学：主轴分行（flex-wrap）与自由空间分配（grow/shrink/justify）。
//!
//! 与 [`super::table`] 同源的分工：`collect.rs` 负责量每个子项的固有尺寸、摆放盒子、
//! 平移行盒；这一份只收几个纯数，按 CSS Flexbox 的主轴算法算出「每个子项最终主尺寸」与
//! 「分几行」。纯函数没有 DOM / 字体 / 度量依赖，便于逐条断言。
//!
//! 坐标全是**逻辑**的：主轴在横排是 x、竖排由外层的 `Layout::assemble` 统一转置。这里只
//! 谈「沿主轴走了多远」，不碰物理 x/y —— 所以横排竖排共用同一套算法。

use folio_format::css::computed::{FlexWrap, JustifyContent};

/// 一个 Flex 子项进入主轴分配前的已知量，全部 CSS px。
#[derive(Debug, Clone, Copy)]
pub(super) struct Item {
    /// flex-basis 折算后的假设主尺寸（grow/shrink 之前的起点）。
    pub basis: f32,
    /// flex-grow 因子。
    pub grow: f32,
    /// flex-shrink 因子。
    pub shrink: f32,
    /// min 主尺寸夹（CSS `min-width` / `min-height` 折算）。
    pub min: f32,
    /// max 主尺寸夹；无上限用 `f32::MAX`。
    pub max: f32,
}

/// 把子项按 `flex-wrap` 分到若干行，返回每行的子项下标（按输入顺序）。
///
/// `nowrap` 永远只给一行（哪怕超宽 —— 交给 shrink 处理）。`wrap` 贪心装箱：
/// 只要把下一项加进来（连它前面的缝）还放得下就放，否则另起一行。
/// 单项就超过主轴可用宽时，它自己独占一行（不让它和别人挤，也不为它折行）。
pub(super) fn wrap_lines(items: &[Item], main: f32, gap: f32, wrap: FlexWrap) -> Vec<Vec<usize>> {
    if items.is_empty() {
        return Vec::new();
    }
    if wrap == FlexWrap::NoWrap {
        return vec![(0..items.len()).collect()];
    }
    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut used = 0.0f32;
    for (i, item) in items.iter().enumerate() {
        let add = if current.is_empty() {
            item.basis
        } else {
            gap + item.basis
        };
        if !current.is_empty() && used + add > main + 0.01 {
            lines.push(std::mem::take(&mut current));
            used = 0.0;
            let add = item.basis;
            current.push(i);
            used += add;
        } else {
            used += add;
            current.push(i);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// 一行里所有子项的最终主尺寸（px），已按 grow/shrink 分配并夹到 [min, max]。
///
/// `main` 是这一行可用的主轴总宽，`gap` 是相邻子项之间的缝。自由空间 =
/// `main − Σbasis − gap×(n−1)`：正时按 grow 比例摊（grow 全为 0 则不分，留给 justify），
/// 负时按 `shrink × basis` 加权回收（CSS §9.7 的缩放因子），再逐项夹到上下界。
pub(super) fn resolve_line(items: &[Item], line: &[usize], main: f32, gap: f32) -> Vec<f32> {
    let n = line.len();
    if n == 0 {
        return Vec::new();
    }
    let gap_total = gap * (n.saturating_sub(1)) as f32;
    let sum_basis: f32 = line.iter().map(|&i| items[i].basis).sum();
    let free = main - gap_total - sum_basis;

    let mut sizes: Vec<f32> = line.iter().map(|&i| items[i].basis).collect();

    if free > 0.0 {
        let total_grow: f32 = line.iter().map(|&i| items[i].grow).sum();
        if total_grow > 0.0 {
            for (k, &i) in line.iter().enumerate() {
                sizes[k] = items[i].basis + free * items[i].grow / total_grow;
            }
        }
    } else if free < 0.0 {
        // shrink 的加权分母是 Σ(shrink × basis)（CSS 原文），不是 Σshrink。
        let total_weighted: f32 = line.iter().map(|&i| items[i].shrink * items[i].basis).sum();
        if total_weighted > 0.0 {
            for (k, &i) in line.iter().enumerate() {
                let shrink_amount = free * items[i].shrink * items[i].basis / total_weighted;
                sizes[k] = (items[i].basis + shrink_amount).max(0.0);
            }
        }
    }

    // 夹到 [min, max]。夹之后总长可能不再等于 main —— 剩下的余量由 justify 段落时处理。
    for (k, &i) in line.iter().enumerate() {
        sizes[k] = sizes[k].max(items[i].min).min(items[i].max);
    }
    sizes
}

/// 一行里主轴方向的起点偏移与 justify 产生的额外间距。
///
/// 返回 `(首项起点, 相邻两项之间的额外间距)`：`space-between` 把剩余空间全摊到缝里，
/// `center` 把它对半放到两端，`end` 全放起点，`start` 什么都不放。
/// `free` 是已经扣掉各项尺寸和已有缝之后的自由空间（负数按 0 处理）。
pub(super) fn justify_offset(free: f32, item_count: usize, mode: JustifyContent) -> (f32, f32) {
    let free = free.max(0.0);
    match mode {
        JustifyContent::Start => (0.0, 0.0),
        JustifyContent::End => (free, 0.0),
        JustifyContent::Center => (free / 2.0, 0.0),
        JustifyContent::SpaceBetween => {
            let extra = if item_count > 1 {
                free / (item_count - 1) as f32
            } else {
                0.0
            };
            (0.0, extra)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(basis: f32, grow: f32, shrink: f32) -> Item {
        Item {
            basis,
            grow,
            shrink,
            min: 0.0,
            max: f32::MAX,
        }
    }

    #[test]
    fn grow_splits_free_space_by_factor() {
        let items = [
            item(100.0, 1.0, 1.0),
            item(100.0, 1.0, 1.0),
            item(100.0, 2.0, 1.0),
        ];
        let line = [0, 1, 2];
        // main=500, 缝 0，basis 和 300，自由 200，按 grow 1:1:2 → 50/50/100。
        assert_eq!(
            resolve_line(&items, &line, 500.0, 0.0),
            vec![150.0, 150.0, 200.0]
        );
    }

    #[test]
    fn shrink_is_weighted_by_basis() {
        let items = [item(200.0, 0.0, 1.0), item(100.0, 0.0, 1.0)];
        let line = [0, 1];
        // main=200, basis 和 300, 自由 -100。权重 200:100 → 回收 66.67/33.33。
        let sizes = resolve_line(&items, &line, 200.0, 0.0);
        assert!((sizes[0] - 133.333).abs() < 0.01, "{:?}", sizes);
        assert!((sizes[1] - 66.667).abs() < 0.01, "{:?}", sizes);
    }

    #[test]
    fn min_max_clamp_the_result() {
        let mut a = item(10.0, 10.0, 1.0);
        a.max = 50.0;
        let items = [a, item(10.0, 1.0, 1.0)];
        let sizes = resolve_line(&items, &[0, 1], 200.0, 0.0);
        assert_eq!(sizes[0], 50.0, "grow 再多也不过 max");
    }

    #[test]
    fn wrap_breaks_into_lines_with_gaps() {
        let items = [
            item(60.0, 0.0, 1.0),
            item(60.0, 0.0, 1.0),
            item(60.0, 0.0, 1.0),
        ];
        let lines = wrap_lines(&items, 140.0, 10.0, FlexWrap::Wrap);
        assert_eq!(
            lines,
            vec![vec![0, 1], vec![2]],
            "60+10+60=130 放得下，第三个另起一行"
        );
    }

    #[test]
    fn nowrap_keeps_one_line_even_when_overflowing() {
        let items = [item(100.0, 0.0, 1.0), item(100.0, 0.0, 1.0)];
        let lines = wrap_lines(&items, 120.0, 0.0, FlexWrap::NoWrap);
        assert_eq!(lines, vec![vec![0, 1]]);
    }

    #[test]
    fn justify_space_between_spreads_the_free_space() {
        // 两项各 40，main=180，缝 0，自由 100 → 中间再加 100。
        assert_eq!(
            justify_offset(100.0, 2, JustifyContent::SpaceBetween),
            (0.0, 100.0)
        );
        assert_eq!(
            justify_offset(100.0, 2, JustifyContent::Center),
            (50.0, 0.0)
        );
        assert_eq!(justify_offset(100.0, 2, JustifyContent::End), (100.0, 0.0));
    }
}
