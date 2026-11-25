//! `text-combine` 的宽度与片段修正：每对数字并进一格。

use std::ops::Range;

use folio_format::dom::NodeId;

use super::Collector;
use crate::collect::Piece;
use crate::font::StyleRef;

/// 把每对数字的宽度并进第一个字，第二位置零；起点与宽度表按字节对齐。
pub(super) fn fold_pairs(widths: &mut [f32], text: &[u8], base: usize, font_size: f32) {
    let mut k = 0usize;
    while k + 1 < text.len() {
        if text[k].is_ascii_digit() && text[k + 1].is_ascii_digit() {
            let a = base + k;
            widths[a] = font_size.max(widths[a] + widths[a + 1]);
            widths[a + 1] = 0.0;
            k += 2;
        } else {
            k += 1;
        }
    }
}

/// 每对数字拆成两个零宽 piece；第二位回退一格与第一位同处一格，再沿交叉轴挪半格。
pub(super) fn push_pieces(
    collector: &mut Collector<'_>,
    node: NodeId,
    norm: Range<usize>,
    orig: Range<usize>,
    style: StyleRef,
) {
    let run = collector.arena.get(style);
    let font_size = run.map_or(0.0, |run| run.font_size);
    // 交叉轴正方向在 vertical-rl 是往左、在 vertical-lr 是往右；第二位要落在第一位的
    // **右边**，所以符号与推进轴那档相反。
    let sign = if run.is_some_and(|run| run.vertical_lr) {
        1.0
    } else {
        -1.0
    };
    let text = &collector.text[norm.clone()];
    let mut digits = 0usize;
    for (at, ch) in text.char_indices() {
        let digit = ch.is_ascii_digit();
        let second = digit && digits % 2 == 1;
        digits = if digit { digits + 1 } else { 0 };
        let len = ch.len_utf8();
        // 生成内容等 `orig` 与 `norm` 不等长时整段原样带上，不能按字节偏移猜。
        let piece_orig = if orig.len() == norm.len() {
            let base = orig.start + at;
            base..base + len
        } else {
            orig.clone()
        };
        collector.pieces.push(Piece {
            node,
            norm: norm.start + at..norm.start + at + len,
            orig: piece_orig,
            style,
            // 宽度表把整格记在第一字上，第二字自己零宽；回退一格才与第一字同处一格。
            x_offset: if second { -font_size } else { 0.0 },
            cross_offset: if second { sign * font_size * 0.5 } else { 0.0 },
        });
    }
}
