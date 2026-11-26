//! `text-combine` 的断点修正：连续数字两两成组，成对不拆行。

use std::ops::Range;

use super::Collected;

/// 断点夹在数字对中间吗：`text-combine` 不许拆对。连续数字两两成组，
/// 宽度表把第二位折进第一位，顺序须与 `wrap` 一致。两侧都得是数字。
fn breaks_combine(collected: &Collected, at: usize) -> bool {
    let bytes = collected.text.as_bytes();
    if at == 0
        || at >= bytes.len()
        || !bytes[at - 1].is_ascii_digit()
        || !bytes[at].is_ascii_digit()
    {
        return false;
    }
    let mut start = at;
    while start > 0 && bytes[start - 1].is_ascii_digit() {
        start -= 1;
    }
    (at - start) % 2 == 1
}

/// 连续数字不是 UAX #14 的断点；补偶数位后的边界，摘奇数位后的，
/// 硬切兜底也只切在对外。
pub(super) fn add_breaks(collected: &Collected, hard: &Range<usize>, breaks: &mut Vec<usize>) {
    let bytes = collected.text.as_bytes();
    let mut run = 0usize;
    let mut i = hard.start;
    while i < hard.end {
        if bytes[i].is_ascii_digit() {
            run += 1;
            if run.is_multiple_of(2) && i + 1 < hard.end {
                breaks.push(i + 1);
            }
            i += 1;
        } else {
            run = 0;
            i += collected.text[i..].chars().next().map_or(1, char::len_utf8);
        }
    }
    breaks.sort_unstable();
    breaks.dedup();
    breaks.retain(|at| !breaks_combine(collected, *at));
}
