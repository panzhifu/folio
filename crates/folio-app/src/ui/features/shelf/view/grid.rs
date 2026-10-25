//! 架面那一片格子**怎么排**：一行几本、格间距、封面高、整片滚动。
//!
//! 一格长什么样在 `components/book_card`；这一份只回答「这一屏能放几本、每本多大」。
//! 全是纯函数/常数 —— 列数算错不会报错，只会让架面在某个窗口宽上突然变成一格一本，
//! 所以它们能单独被断言（见 `columns_for` 的那三条）。

use super::super::components::book_card::COVER_RATIO;
use crate::ui::shell::navigation::{SizeClass, reserved_width};

pub(in crate::ui::features::shelf) const COLUMNS: u16 = 3;

/// 网格间距，与 `grid` 的 `gap_2` 同一档（默认 rem 下 8 逻辑像素）。
pub(in crate::ui::features::shelf) const GAP: f32 = 8.0;

/// 桌面上一格封面的最小宽度（逻辑像素）。
///
/// 比手机的 112 宽：鼠标桌面上书名要一眼读完，而不是靠点进去看，而一格多宽，
/// 标题那一行就能塞下几个字。再往下加一列会开始把长书名截成看不懂的半句。
pub(in crate::ui::features::shelf) const MIN_CARD: f32 = 168.0;

/// 网格与搜索行两侧的留白（逻辑像素）。和顶部那条章导航的 `px_3` 不同一档，
/// 因为这里是整片卡片的边，视觉上要比一条文字栏松。
pub(in crate::ui::features::shelf) const GUTTER: f32 = 12.0;

/// 一横排的量：几列，以及一格封面该多高。
///
/// 封面高必须由格宽**在这里算出来**，不能留给 `aspect_ratio` 在排版时按宽度推：
/// `uniform_list` 量行高时给的是 **min-content 宽度**（`measure_item(None, ..)`），
/// 而书名那一行是 `whitespace_nowrap` —— 它的 min-content 宽就是整条不折行的标题宽。
/// 于是量出来的那一格有 300 多逻辑像素宽，封面按 2:3 推出 500 多的高，行高比真实的
/// 两还多，第二排直接被推到屏幕外（真机 1080×2400 实测：量出 544，实际 236）。
/// 写死高度让行高与测量时给的宽度无关，这个坑就填上了。
#[derive(Clone, Copy)]
pub(in crate::ui::features::shelf) struct GridSpec {
    pub(in crate::ui::features::shelf) columns: u16,
    pub(in crate::ui::features::shelf) cover_height: f32,
}

/// 网格真正能用到的宽度：扣掉导航栏占的那一条与两侧留白。
///
/// `columns_for` 与 [`grid_spec`] 都从这一个式子取数：两处各算一份的话，改一侧忘另一侧
/// 会表现为「列数对了但每格宽得莫名其妙」。导航栏那一条由 `shell::navigation` 给 ——
/// 三档里只有 Medium 与 Expanded 占位，Compact 那条底栏是浮的（占 0）。
pub(in crate::ui::features::shelf) fn available_width(width: f32, size: SizeClass) -> f32 {
    (width - reserved_width(size) - GUTTER * 2.0).max(0.0)
}

/// 一横排该排成什么样：列数按 [`columns_for`]，封面高按那个列数均出来的格宽。
pub(in crate::ui::features::shelf) fn grid_spec(width: f32, size: SizeClass) -> GridSpec {
    let columns = columns_for(width, size);
    // n 格之间只有 n−1 条间距，所以只扣一条 `GAP * (n−1)`。
    let card = (available_width(width, size) - GAP * f32::from(columns - 1)) / f32::from(columns);
    GridSpec {
        columns,
        cover_height: card / COVER_RATIO,
    }
}

/// 一行放几本：手机固定 [`COLUMNS`]，桌面与平板按**内容区实际宽度**排到每格
/// 不低于 [`MIN_CARD`]。
///
/// 3 列写死在桌面上是不成立的：1600 宽的窗口会给出三格 440 像素的封面，
/// 一屏只看得见两本书 —— 而「我的书都在哪儿」正是架面要回答的问题。
///
/// 导航栏的宽度从 `shell::navigation` 取：那两个常数是阅读页预留版心用的同一份，
/// 在这里再写一遍数字的话，改一侧忘另一侧只会表现为「书架的格子宽得莫名其妙」。
///
/// `pub` 只为测试能钉住这三档的列数 —— 列数算错不会报错，只会让架面在某个窗口宽上
/// 突然变成一格一本。
pub fn columns_for(width: f32, size: SizeClass) -> u16 {
    if size.is_compact() {
        return COLUMNS;
    }
    // 第 n 格要占 n 个格宽加 n−1 条间距，所以分子补回一条 GAP。
    let fitted = ((available_width(width, size) + GAP) / (MIN_CARD + GAP)).floor() as u16;
    fitted.max(1)
}

/// 一条流里的第 `ix` 格是组卡还是书，以及它是那一份里的第几个。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::ui::features::shelf) enum Cell {
    Tile(usize),
    Book(usize),
}

/// 把已经各自排好的两段并成架面那一条流：`tiles` / `books` 给的是两段的
/// `last_opened_at`（库里两条查询同用 `SHELF_ORDER`，所以传进来就是**降序、None 在尾**），
/// 回的是每一格该从哪一段取。
///
/// 尺只有一把：**谁最近被翻开，谁的卡就在先**。点开一本没分组的书，它顶到第 1 格；
/// 点开某一分组里的书，那本书并不在整架那一片（`Store::shelf_ungrouped` 收掉了它），
/// 顶到第 1 格的就是那张组卡 —— 读者退架要找的是「我刚碰过的那一格」，至于是书还是组，
/// 不该由架面替他分档。
///
/// 两段内部不动：这里只做归并，不重排，所以库里那条序（含 `added_at` / `id` 那两个
/// 终结符）原样带过来。
pub(in crate::ui::features::shelf) fn merge_cells(
    tiles: &[Option<i64>],
    books: &[Option<i64>],
) -> Vec<Cell> {
    let mut cells = Vec::with_capacity(tiles.len() + books.len());
    let (mut t, mut b) = (0, 0);
    while t < tiles.len() || b < books.len() {
        // `Option<i64>` 自己的序就够用：`None`（没翻开过）小于任何 `Some`，于是两段的
        // 空白尾巴都自然沉到最后。并列时取书 —— 同一秒里读者要找的那本比一张组卡更好认。
        let book_first = match (tiles.get(t), books.get(b)) {
            (Some(_), None) => false,
            (None, Some(_)) => true,
            (None, None) => false,
            (Some(t), Some(b)) => b >= t,
        };
        if book_first {
            cells.push(Cell::Book(b));
            b += 1;
        } else {
            cells.push(Cell::Tile(t));
            t += 1;
        }
    }
    cells
}

#[cfg(test)]
mod tests {
    use super::{Cell, merge_cells};

    /// 谁最近被翻开谁在先：书顶到第 1 格，被翻开过的那一组也一样。
    #[test]
    fn the_most_recently_opened_card_leads_whether_book_or_group() {
        // 一张组卡（组里那本刚翻开）+ 两本书（一本更早、一本没翻开过）。
        assert_eq!(
            merge_cells(&[Some(30)], &[Some(20), None]),
            vec![Cell::Tile(0), Cell::Book(0), Cell::Book(1)],
            "点开组里那本 → 那张组卡领头，没翻开过的书垫底"
        );
        assert_eq!(
            merge_cells(&[Some(10)], &[Some(20), None]),
            vec![Cell::Book(0), Cell::Tile(0), Cell::Book(1)],
            "点开一本没分组的书 → 它顶到第 1 格，组卡跟在后面"
        );
    }

    /// 两段各自的序不许被归并打乱，并列时让书在前，两端的退化照常。
    #[test]
    fn the_merge_keeps_each_run_in_its_own_order() {
        assert_eq!(
            merge_cells(&[Some(30), Some(20), Some(10)], &[Some(25), Some(15)]),
            vec![
                Cell::Tile(0),
                Cell::Book(0),
                Cell::Tile(1),
                Cell::Book(1),
                Cell::Tile(2),
            ],
            "交错着并，两段各自还是降序"
        );
        assert_eq!(
            merge_cells(&[Some(20)], &[Some(20)]),
            vec![Cell::Book(0), Cell::Tile(0)],
            "同一秒：书在前"
        );
        assert_eq!(
            merge_cells(&[], &[Some(5), None]),
            vec![Cell::Book(0), Cell::Book(1)],
            "某一组那一页与带搜索词的整架没有组卡"
        );
        assert_eq!(
            merge_cells(&[None, None], &[None]),
            vec![Cell::Book(0), Cell::Tile(0), Cell::Tile(1)],
            "谁都没翻开过时，书那段在前、组卡按库里的序接上"
        );
        assert_eq!(merge_cells(&[], &[]), Vec::<Cell>::new(), "空架");
    }
}
