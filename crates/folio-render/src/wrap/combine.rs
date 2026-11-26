//! `text-combine` 的宽度与片段修正：数字组并进一格。

use std::ops::Range;

use folio_format::dom::NodeId;

use super::Collector;
use crate::collect::Piece;
use crate::combine_groups;
use crate::font::StyleRef;

/// 把每组数字的宽度并进第一个字，其余位置零；起点与宽度表按字节对齐。
pub(super) fn fold_groups(
    widths: &mut [f32],
    text: &[u8],
    base: usize,
    font_size: f32,
    size: usize,
) {
    let text = String::from_utf8_lossy(text);
    for range in combine_groups::groups(&text, size) {
        let mut total = 0.0;
        for (at, _) in text[range.clone()].char_indices() {
            total += widths.get(base + range.start + at).copied().unwrap_or(0.0);
            if at > 0 {
                widths[base + range.start + at] = 0.0;
            }
        }
        widths[base + range.start] = font_size.max(total);
    }
}

/// 每组数字拆成多个零宽 piece；后续位回退 i 格、沿交叉轴对称排布。
pub(super) fn push_pieces(
    collector: &mut Collector<'_>,
    node: NodeId,
    norm: Range<usize>,
    orig: Range<usize>,
    style: StyleRef,
    size: usize,
) {
    let run = collector.arena.get(style);
    let font_size = run.map_or(0.0, |run| run.font_size);
    // 交叉轴正方向在 vertical-rl 是往左、在 vertical-lr 是往右；右侧数字要沿正方向摆。
    let sign = if run.is_some_and(|run| run.vertical_lr) {
        1.0
    } else {
        -1.0
    };
    let text = collector.text[norm.clone()].to_string();
    let groups = combine_groups::groups(&text, size);
    let mut active: Option<(Range<usize>, usize)> = None;
    for (rel, ch) in text.char_indices() {
        let len = ch.len_utf8();
        let norm_start = norm.start + rel;
        let current = active.filter(|(range, _)| norm_start < range.end);
        let (x_offset, cross_offset) = if let Some((_, index)) = current {
            let dx = -font_size * index as f32;
            let dc = sign * font_size * (0.5 + (index - 1) as f32);
            active = Some((current.unwrap().0, index + 1));
            (dx, dc)
        } else if let Some(range) = groups
            .iter()
            .find(|range| range.start == norm_start)
            .cloned()
        {
            active = Some((range, 1));
            (0.0, 0.0)
        } else {
            active = None;
            (0.0, 0.0)
        };
        let piece_orig = if orig.len() == norm.len() {
            let base = orig.start + rel;
            base..base + len
        } else {
            orig.clone()
        };
        collector.pieces.push(Piece {
            node,
            norm: norm_start..norm_start + len,
            orig: piece_orig,
            style,
            x_offset,
            cross_offset,
        });
    }
}
