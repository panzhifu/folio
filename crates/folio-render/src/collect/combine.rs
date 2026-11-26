//! `text-combine` 的断点修正：数字组不拆行。

use std::ops::Range;

use super::Collected;
use crate::combine_groups;

/// 连续数字不是 UAX #14 的断点；补完整组后的边界，摘组内的，硬切兜底也只切在对外。
pub(super) fn add_breaks(
    collected: &Collected,
    hard: &Range<usize>,
    breaks: &mut Vec<usize>,
    size: usize,
) {
    let ranges = combine_groups::groups(&collected.text, size);
    breaks.retain(|at| !combine_groups::inside_group(&ranges, *at));
    for range in ranges {
        if range.end < hard.end {
            breaks.push(range.end);
        }
    }
    breaks.sort_unstable();
    breaks.dedup();
}
