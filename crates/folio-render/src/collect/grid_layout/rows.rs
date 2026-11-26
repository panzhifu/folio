//! Grid 行轨道的尺寸求解与跨行配平。
//!
//! 输入是模板轨道、自动行的实测内容高、跨行项的高度；输出每行高。纯数在这里，
//! `grid_layout` 只负责把子项摆进算出的行。

use folio_format::css::computed::GridTrack;

/// 一条跨行项占据的行范围与高度。
pub(super) struct Span {
    pub row: usize,
    pub span: usize,
    pub height: f32,
}

impl Span {
    pub fn new(row: usize, span: usize, height: f32) -> Self {
        Self { row, span, height }
    }
}

/// 先量出单行项的内容高，再让跨行项的缺口只加在 `auto` 行上。
///
/// `fr` 行要在内容高量完之后才能按剩余空间分配，所以这里只收单行项的高度；
/// 固定长度行（含 `minmax` 上界是长度的那一档）先给终值。
pub(super) fn auto_row_sizes(
    tracks: &[GridTrack],
    rows: usize,
    slot_heights: impl Iterator<Item = (usize, usize, f32)>,
) -> Vec<f32> {
    let mut auto = vec![0.0f32; rows];
    for (row, span, height) in slot_heights {
        if span == 1 {
            auto[row] = auto[row].max(height);
        }
    }
    for (row, height) in auto.iter_mut().enumerate().take(rows) {
        if matches!(tracks.get(row), None | Some(GridTrack::Auto)) {
            continue;
        }
        *height = 0.0;
    }
    auto
}

/// 按行模板解出每行高。
///
/// 百分比行轨道折自容器内容宽（与 `row-gap` 同口径），只有 `auto` / `MinMaxFr`
/// 的自动部分吃实测内容。`fr` 分剩下的空间；跨行项不够放时只把缺口摊给 `auto` 行。
pub(super) fn solve_rows(
    tracks: &[GridTrack],
    row_count: usize,
    available: f32,
    gap: f32,
    auto_heights: &[f32],
    spans: &[Span],
) -> Vec<f32> {
    let mut heights = vec![0.0f32; row_count];
    let mut fr = vec![0.0f32; row_count];
    let mut min = vec![0.0f32; row_count];
    for row in 0..row_count {
        match tracks.get(row) {
            Some(GridTrack::Length(length)) => {
                heights[row] = length.resolve(available).max(0.0);
            }
            // 行模板里的 `max-content` 在解析期折成 1e6 px；当 auto 处理，否则一行的
            // 内容撑出的高度会先被解析期这个大数顶到百万像素。
            Some(GridTrack::MinMax(low, high)) if high.resolve(available) >= 1e6 => {
                min[row] = low.resolve(available).max(0.0);
                heights[row] = auto_heights[row].max(min[row]);
            }
            Some(GridTrack::MinMax(low, high)) => {
                let lo = low.resolve(available).max(0.0);
                heights[row] = high.resolve(available).max(lo);
            }
            Some(GridTrack::Fr(value)) => fr[row] = value.max(0.0),
            Some(GridTrack::MinMaxFr(low, value)) => {
                min[row] = low.resolve(available).max(0.0);
                fr[row] = value.max(0.0);
            }
            Some(GridTrack::Auto) | None => heights[row] = auto_heights[row],
        }
    }
    let gaps = gap * row_count.saturating_sub(1) as f32;
    let fixed: f32 = heights.iter().sum();
    let mins: f32 = min.iter().sum();
    let free = (available - gaps - fixed - mins).max(0.0);
    let total_fr: f32 = fr.iter().sum();
    if total_fr > 0.0 {
        for row in 0..row_count {
            if fr[row] > 0.0 {
                heights[row] = min[row] + free * fr[row] / total_fr;
            }
        }
    }
    for span in spans {
        if span.span <= 1 {
            continue;
        }
        let end = (span.row + span.span).min(row_count);
        let covered: f32 = heights[span.row..end].iter().sum::<f32>()
            + gap * end.saturating_sub(span.row + 1) as f32;
        let deficit = span.height - covered;
        if deficit <= 0.0 || end <= span.row {
            continue;
        }
        let flexible: Vec<usize> = (span.row..end)
            .filter(|&r| matches!(tracks.get(r), None | Some(GridTrack::Auto)))
            .collect();
        if flexible.is_empty() {
            continue;
        }
        let share = deficit / flexible.len() as f32;
        for row in flexible {
            heights[row] += share;
        }
    }
    heights
}

/// 行起点前缀和。
pub(super) fn row_offsets(heights: &[f32], gap: f32) -> Vec<f32> {
    let mut offsets = vec![0.0f32; heights.len()];
    let mut cursor = 0.0f32;
    for (row, height) in heights.iter().enumerate() {
        offsets[row] = cursor;
        cursor += height + gap;
    }
    offsets
}

/// 跨行格总高（含中间的缝）。
pub(super) fn span_height(row: usize, span: usize, heights: &[f32], gap: f32) -> f32 {
    let end = (row + span).min(heights.len());
    heights[row..end].iter().sum::<f32>() + gap * end.saturating_sub(row + 1) as f32
}
