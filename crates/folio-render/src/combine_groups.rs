//! 连续数字的分组：宽度折叠、禁断与命中共用一份字节区间。

use std::ops::Range;

use folio_format::css::computed::TextCombine;

/// `all` 沿用旧的成对口径；`digits n` 才按 n 位成组。
pub(crate) fn group_size(combine: TextCombine) -> Option<usize> {
    match combine {
        TextCombine::None => None,
        TextCombine::All => Some(2),
        TextCombine::Digits(n) => Some(usize::from(n).clamp(2, 4)),
    }
}

fn combine_digit(ch: char) -> bool {
    ch.is_numeric()
}

/// 连续数字按 `size` 分组；余下不足一组的数字留在原格，不半组并排。
pub(crate) fn groups(text: &str, size: usize) -> Vec<Range<usize>> {
    let size = size.clamp(2, 4);
    let mut out = Vec::new();
    let mut start = None;
    let mut count = 0usize;
    for (at, ch) in text.char_indices() {
        if !combine_digit(ch) {
            start = None;
            count = 0;
            continue;
        }
        let from = *start.get_or_insert(at);
        count += 1;
        if count == size {
            out.push(from..at + ch.len_utf8());
            start = None;
            count = 0;
        }
    }
    out
}

/// 某个字节位置是否落在分组内部（不含起止边）：那里不许断行。
pub(crate) fn inside_group(ranges: &[Range<usize>], at: usize) -> bool {
    ranges
        .iter()
        .any(|range| range.start < at && at < range.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_use_char_boundaries_and_leave_partial_runs_alone() {
        let ranges = groups("甲１２345乙", 3);
        assert_eq!(ranges, vec![3..10]);
        let ranges = groups("12345", 2);
        assert_eq!(ranges, vec![0..2, 2..4]);
    }
}
