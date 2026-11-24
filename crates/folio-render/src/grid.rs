//! 网格：列轨道求解与自动放置（CSS Grid 的简化子集）。
//!
//! 与 `flex` / `table` 同一层：输入是轨道定义、可用宽与各列内容宽，输出是每列宽与
//! 每个子项的 (行, 列)。实排与量高两条路都调这一份，两份几何从构造上同源。
//!
//! 落地范围（如实记录，别当已做）：
//! - 列轨道：固定长度、百分比、`fr`、`minmax(长度, 长度)`、`minmax(长度, fr)`、`auto`
//!   （`auto` 取该列内容最大宽）。行轨道不读 `grid-template-rows`，一律按内容高。
//! - 放置：显式 `grid-column`（线号 / `span N`）+ 行主序稀疏自动放置；`grid-row` 的
//!   显式值暂不参与放置（只存下来），`rowspan` 不撑行。
//! - 对齐：容器 `align-items` 用于行内纵向；子项 `justify-self` 用于列内横向。
//!   `align-self` 与嵌套 grid 不在这一档。

use folio_format::css::computed::GridTrack;

/// 求解列宽。`auto_widths[i]` 是第 `i` 列的内容最大宽（只对 `auto` 轨道有意义）。
///
/// 先给固定轨道与 `auto` 轨道结清尺寸，再把剩余空间按 `fr` 比例分给弹性轨道
/// （弹性轨道有 `minmax` 下界时先扣下界再分）。可用宽不足以放下所有下界时按实际
/// 尺寸排、表格整体溢出，不缩字。
pub(super) fn resolve_columns(
    tracks: &[GridTrack],
    available: f32,
    gap: f32,
    auto_widths: &[f32],
) -> Vec<f32> {
    let n = tracks.len();
    let mut widths = vec![0.0f32; n];
    let mut fr = vec![0.0f32; n];
    let mut min = vec![0.0f32; n];
    for (i, track) in tracks.iter().enumerate() {
        match track {
            GridTrack::Length(length) => widths[i] = length.resolve(available).max(0.0),
            GridTrack::MinMax(low, high) => {
                let lo = low.resolve(available).max(0.0);
                widths[i] = high.resolve(available).max(lo);
            }
            GridTrack::Fr(f) => fr[i] = f.max(0.0),
            GridTrack::MinMaxFr(low, f) => {
                min[i] = low.resolve(available).max(0.0);
                fr[i] = f.max(0.0);
            }
            GridTrack::Auto => {
                widths[i] = auto_widths.get(i).copied().unwrap_or(0.0).max(0.0);
            }
        }
    }
    let gaps = gap * (n.saturating_sub(1)) as f32;
    let fixed_sum: f32 = widths.iter().sum();
    let min_sum: f32 = min.iter().sum();
    let free = (available - gaps - fixed_sum - min_sum).max(0.0);
    let total_fr: f32 = fr.iter().sum();
    if total_fr > 0.0 {
        for i in 0..n {
            if fr[i] > 0.0 {
                widths[i] = min[i] + free * fr[i] / total_fr;
            }
        }
    }
    widths
}

/// 自动放置：返回每个子项的 (行, 列)，以及总行数。
///
/// 显式列优先：列号比当前游标靠前时换行；自动项在当前行放不下时换行。这是 CSS
/// 「稀疏」放置的简化版（不回溯填空），对出版书里的两列骨架与编号方程足够。
pub(super) fn place(columns: usize, items: &[(Option<u16>, u16)]) -> (Vec<(usize, usize)>, usize) {
    let columns = columns.max(1);
    let mut row = 0usize;
    let mut cursor = 0usize;
    let mut placed = Vec::with_capacity(items.len());
    let mut max_row = 0usize;
    for &(start, span) in items {
        let span = (span.max(1) as usize).min(columns);
        let column = match start {
            Some(line) => {
                let column = (line as usize).saturating_sub(1).min(columns - 1);
                if column < cursor {
                    row += 1;
                }
                cursor = (column + span).min(columns);
                column
            }
            None => {
                if cursor + span > columns {
                    row += 1;
                    cursor = 0;
                }
                let column = cursor;
                cursor = (column + span).min(columns);
                column
            }
        };
        max_row = max_row.max(row);
        placed.push((row, column));
    }
    (placed, max_row + 1)
}
