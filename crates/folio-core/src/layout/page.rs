//! 页：把「一栏一片」的 [`Fragment`] 按页聚合成正式输出。
//!
//! 分片器仍然逐栏吐 [`Fragment`]（连续 / 分页 / 双页是同一套切点规则的三档参数），
//! 这一层只做**聚合**：一页是 `columns` 栏的集合，页号、行区间、页高、断页原因都在
//! 这里定下来。界面不再自己乘 `page * columns + column`，也不必再猜"这一页几栏"。
//!
//! 页仍然是**切片的派生视图**，不是排版的一级阶段：`Layout` 不变，`PageList` 只是
//! 它的一个只读分组（见 `doc/阅读管线.md` 已决 #1）。所以换页高只重切、不重排；
//! `PageList` 可以随 `Layout` 一起缓存，但不需要序列化。
//!
//! 页眉页脚**不在这里**：那是阅读器的屏幕 chrome，由界面的 `PageBox` 管。这一层
//! 只回答"内容页是什么"，不回答"它在屏幕上摆哪儿"。

use std::ops::Range;

use super::fragment::Fragment;

/// 一页：`columns` 栏的集合。
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    /// 页号（0 基，章内）。
    pub index: usize,
    /// 本页覆盖的行区间（所有栏的并集）。
    pub lines: Range<usize>,
    /// 首栏在推进轴上的章内起点。
    pub origin_y: f32,
    /// 本页最高的那一栏的高度。空页为 0。
    pub height: f32,
    /// 页内各栏，顺序与 `Fragment::column` 一致（逻辑栏序）。
    pub columns: Vec<Fragment>,
    /// 本页由作者的强制断点结束。
    pub forced: bool,
    /// 本页所有栏都是补出来的空片（强制断页之后补满的那一页）。
    pub empty: bool,
}

impl Page {
    /// 本页的栏数。末页可能不满 `PageList::columns`。
    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    pub fn column(&self, index: usize) -> Option<&Fragment> {
        self.columns.get(index)
    }

    /// 整页画的那张图（有的话）。同一页的栏共享同一个旗，取第一个非空的即可。
    pub fn picture_page(&self) -> Option<&str> {
        self.columns
            .iter()
            .find_map(|column| column.picture_page.first())
            .map(String::as_str)
    }
}

/// 一章的页表。
#[derive(Clone, Debug, PartialEq)]
pub struct PageList {
    pages: Vec<Page>,
    /// 排版时一页几栏（最后一页可能不满）。
    columns: usize,
}

impl PageList {
    /// 从分片器吐出的栏序列聚合。`fragments` 必须按「页优先、页内按栏」排好
    /// （[`super::fragment::fragments`] 与 [`super::fragment::Fragmentainer`] 都是这个顺序）。
    pub fn from_fragments(fragments: Vec<Fragment>, columns: usize) -> Self {
        let columns = columns.max(1);
        if fragments.is_empty() {
            return Self {
                pages: Vec::new(),
                columns,
            };
        }

        let mut pages = Vec::new();
        let mut current: Vec<Fragment> = Vec::new();
        let mut current_page = fragments[0].page;
        for fragment in fragments {
            if fragment.page != current_page {
                pages.push(Page::from_columns(
                    current_page,
                    std::mem::take(&mut current),
                ));
                current_page = fragment.page;
            }
            current.push(fragment);
        }
        pages.push(Page::from_columns(current_page, current));

        Self { pages, columns }
    }

    pub fn len(&self) -> usize {
        self.pages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    pub fn pages(&self) -> &[Page] {
        &self.pages
    }

    pub fn get(&self, index: usize) -> Option<&Page> {
        self.pages.get(index)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Page> {
        self.pages.iter()
    }

    /// 排版时一页几栏。末页的 `Page::column_count()` 可能比它小。
    pub fn columns(&self) -> usize {
        self.columns
    }

    /// 第 `page` 页第 `column` 栏。越界给 `None`。
    pub fn slice_of(&self, page: usize, column: usize) -> Option<&Fragment> {
        self.pages.get(page)?.columns.get(column)
    }

    /// 第 `line` 行落在第几页。行号越界给最后一页。
    ///
    /// 空页（强制断页补出来的）不挡路：它们没有行，`lines.end` 等于 `start`。
    /// 这一问只在恢复进度、跳目录时发生，不在每帧路径上，所以先用线性扫；
    /// 等页数真的上千再给 `PageList` 预存一张非空页的 `ends` 表做二分。
    pub fn page_of_line(&self, line: usize) -> usize {
        let last = self.pages.len().saturating_sub(1);
        self.pages
            .iter()
            .position(|page| !page.empty && line < page.lines.end)
            .unwrap_or(last)
    }
}

impl Page {
    fn from_columns(index: usize, columns: Vec<Fragment>) -> Self {
        let first = columns.first();
        let last = columns.last();
        let origin_y = first.map_or(0.0, |fragment| fragment.origin_y);
        let height = columns
            .iter()
            .filter(|fragment| !fragment.empty)
            .map(|fragment| fragment.height)
            .fold(0.0, f32::max);
        let forced = columns.iter().any(|fragment| fragment.forced);
        let empty = columns.iter().all(|fragment| fragment.empty);
        let lines = match (first, last) {
            (Some(first), Some(last)) => first.lines.start..last.lines.end,
            _ => 0..0,
        };
        Self {
            index,
            lines,
            origin_y,
            height,
            columns,
            forced,
            empty,
        }
    }
}

impl<'a> IntoIterator for &'a PageList {
    type Item = &'a Page;
    type IntoIter = std::slice::Iter<'a, Page>;

    fn into_iter(self) -> Self::IntoIter {
        self.pages.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::{Fragment, PageList};

    fn fragment(page: usize, column: usize, start: usize, end: usize, height: f32) -> Fragment {
        Fragment {
            lines: start..end,
            origin_y: start as f32 * 10.0,
            height,
            forced: false,
            empty: false,
            page,
            column,
            picture_page: Vec::new(),
        }
    }

    fn empty(page: usize, column: usize, at: usize) -> Fragment {
        Fragment {
            lines: at..at,
            origin_y: at as f32 * 10.0,
            height: 0.0,
            forced: true,
            empty: true,
            page,
            column,
            picture_page: Vec::new(),
        }
    }

    #[test]
    fn one_column_makes_one_page_per_fragment() {
        let pages = PageList::from_fragments(
            vec![fragment(0, 0, 0, 3, 30.0), fragment(1, 0, 3, 6, 28.0)],
            1,
        );
        assert_eq!(pages.len(), 2);
        assert_eq!(pages.get(0).unwrap().lines, 0..3);
        assert_eq!(pages.get(1).unwrap().lines, 3..6);
        assert_eq!(pages.get(1).unwrap().height, 28.0);
        assert_eq!(pages.columns(), 1);
    }

    #[test]
    fn two_columns_group_by_page_and_keep_order() {
        let pages = PageList::from_fragments(
            vec![
                fragment(0, 0, 0, 2, 20.0),
                fragment(0, 1, 2, 4, 22.0),
                fragment(1, 0, 4, 6, 24.0),
                fragment(1, 1, 6, 8, 26.0),
            ],
            2,
        );
        assert_eq!(pages.len(), 2);
        assert_eq!(pages.get(0).unwrap().column_count(), 2);
        assert_eq!(pages.get(0).unwrap().lines, 0..4);
        assert_eq!(pages.get(0).unwrap().height, 22.0);
        assert_eq!(pages.get(1).unwrap().lines, 4..8);
        assert_eq!(pages.slice_of(1, 1).unwrap().height, 26.0);
    }

    #[test]
    fn the_last_page_may_have_fewer_columns() {
        let pages = PageList::from_fragments(
            vec![
                fragment(0, 0, 0, 2, 20.0),
                fragment(0, 1, 2, 4, 22.0),
                fragment(1, 0, 4, 5, 10.0),
            ],
            2,
        );
        assert_eq!(pages.len(), 2);
        assert_eq!(pages.get(1).unwrap().column_count(), 1);
        assert_eq!(pages.get(1).unwrap().lines, 4..5);
    }

    #[test]
    fn a_padded_page_is_still_a_page() {
        let pages = PageList::from_fragments(
            vec![
                fragment(0, 0, 0, 2, 20.0),
                empty(0, 1, 2),
                fragment(1, 0, 2, 4, 20.0),
            ],
            2,
        );
        assert_eq!(pages.len(), 2);
        assert_eq!(pages.get(0).unwrap().column_count(), 2);
        assert_eq!(pages.get(0).unwrap().lines, 0..2);
        assert!(!pages.get(0).unwrap().empty);
        assert!(pages.get(0).unwrap().forced);
    }

    #[test]
    fn page_of_line_agrees_with_the_linear_scan() {
        let fragments = vec![
            fragment(0, 0, 0, 2, 20.0),
            fragment(0, 1, 2, 4, 20.0),
            empty(1, 0, 4),
            empty(1, 1, 4),
            fragment(2, 0, 4, 7, 20.0),
        ];
        let pages = PageList::from_fragments(fragments, 2);
        for line in 0..10 {
            let linear = pages
                .pages()
                .iter()
                .position(|page| !page.empty && line < page.lines.end)
                .unwrap_or(pages.len() - 1);
            assert_eq!(pages.page_of_line(line), linear, "line {line}");
        }
    }

    #[test]
    fn the_picture_page_flag_survives_grouping() {
        let mut first = fragment(0, 0, 0, 1, 10.0);
        first.picture_page = vec!["cover.jpg".to_string()];
        let pages = PageList::from_fragments(vec![first, fragment(0, 1, 1, 2, 10.0)], 2);
        assert_eq!(pages.get(0).unwrap().picture_page(), Some("cover.jpg"));
    }

    #[test]
    fn an_empty_list_is_empty_not_a_panic() {
        let pages = PageList::from_fragments(Vec::new(), 2);
        assert!(pages.is_empty());
        assert_eq!(pages.len(), 0);
        assert_eq!(pages.page_of_line(0), 0);
        assert!(pages.slice_of(0, 0).is_none());
    }
}
