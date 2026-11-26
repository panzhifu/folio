//! 网格：轨道求解与自动放置（CSS Grid 的简化子集）。
//!
//! 与 `flex` / `table` 同一层：输入是轨道定义、可用尺寸与各轨道内容尺寸，输出是
//! 每列/每行宽高与每个子项的 (行, 列)。实排与量高两条路都调这一份，两份几何从构造上同源。
//!
//! 落地范围（如实记录，别当已做）：
//! - 轨道：固定长度、百分比、`fr`、`minmax(长度, 长度)`、`minmax(长度, fr)`、`auto`
//!   （`auto` 取该轨道内容最大尺寸）。行轨道用同一套求解，内容高由排版层先量出。
//! - 放置：显式 `grid-column` / `grid-row`（线号 / `span N`）+ 行主序稀疏自动放置；
//!   跨行/跨列占住的格子会被后续项跳过，跨行格不够高时把缺口均摊进所占行。
//! - 对齐：容器 `align-items` 用于行内纵向；子项 `justify-self` 用于列内横向。
//!   `align-self` 已由逐项覆盖，`inline-grid` 仍按行内流处理。

use folio_format::css::computed::GridTrack;

/// 求解一条轴的轨道尺寸。`auto_sizes[i]` 是第 `i` 条轨道的内容尺寸
/// （只对 `auto` 轨道有意义）。
///
/// 先给固定轨道与 `auto` 轨道结清尺寸，再把剩余空间按 `fr` 比例分给弹性轨道
/// （弹性轨道有 `minmax` 下界时先扣下界再分）。可用尺寸不足以放下所有下界时按实际
/// 尺寸排、整体溢出，不缩字。
pub(super) fn resolve_tracks(
    tracks: &[GridTrack],
    available: f32,
    gap: f32,
    auto_sizes: &[f32],
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
                widths[i] = auto_sizes.get(i).copied().unwrap_or(0.0).max(0.0);
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

/// 求解列宽：一条轴的通用求解在 [`resolve_tracks`]。
pub(super) fn resolve_columns(
    tracks: &[GridTrack],
    available: f32,
    gap: f32,
    auto_widths: &[f32],
) -> Vec<f32> {
    resolve_tracks(tracks, available, gap, auto_widths)
}

/// 一个子项的定位意图：行/列起点（1-based，None = 自动）与跨格数。
pub(super) struct Item {
    pub row: Option<u16>,
    pub row_span: u16,
    pub column: Option<u16>,
    pub column_span: u16,
}

/// 放置：返回每个子项的 (行, 列)，以及总行数。
///
/// 行/列都写的按线号直接落；只写一边的扫另一边的空位；都没写的按行主序稀疏填充
/// （游标只前进、不回溯）。跨行/跨列占住的格子会被后续项跳过。
pub(super) fn place(columns: usize, items: &[Item]) -> (Vec<(usize, usize)>, usize) {
    let columns = columns.max(1);
    let mut occupied = std::collections::BTreeSet::new();
    let mut cursor_row = 0usize;
    let mut cursor_column = 0usize;
    let mut placed = Vec::with_capacity(items.len());
    let mut max_row = 0usize;
    for item in items {
        let column_span = (item.column_span.max(1) as usize).min(columns);
        let row_span = item.row_span.max(1) as usize;
        let (row, column) = match (item.row, item.column) {
            (Some(line), Some(column)) => (
                (line as usize).saturating_sub(1),
                (column as usize).saturating_sub(1).min(columns - 1),
            ),
            (Some(line), None) => {
                let row = (line as usize).saturating_sub(1);
                let column = (0..=columns.saturating_sub(column_span))
                    .find(|&c| free(&occupied, columns, row, c, row_span, column_span))
                    .unwrap_or(0);
                (row, column)
            }
            (None, Some(line)) => {
                let column = (line as usize).saturating_sub(1).min(columns - 1);
                let row = (0..)
                    .find(|&r| free(&occupied, columns, r, column, row_span, column_span))
                    .unwrap_or(0);
                (row, column)
            }
            (None, None) => {
                loop {
                    if cursor_column + column_span > columns {
                        cursor_row += 1;
                        cursor_column = 0;
                    }
                    if free(
                        &occupied,
                        columns,
                        cursor_row,
                        cursor_column,
                        row_span,
                        column_span,
                    ) {
                        break;
                    }
                    cursor_column += 1;
                }
                let cell = (cursor_row, cursor_column);
                cursor_column += column_span;
                cell
            }
        };
        for dr in 0..row_span {
            for dc in 0..column_span {
                occupied.insert((row + dr, column + dc));
            }
        }
        max_row = max_row.max(row + row_span - 1);
        placed.push((row, column));
    }
    (placed, max_row + 1)
}

/// 这些格子在 `columns` 内是否都空着。
fn free(
    occupied: &std::collections::BTreeSet<(usize, usize)>,
    columns: usize,
    row: usize,
    column: usize,
    row_span: usize,
    column_span: usize,
) -> bool {
    let end = (column + column_span).min(columns);
    (0..row_span).all(|dr| (column..end).all(|c| !occupied.contains(&(row + dr, c))))
}
