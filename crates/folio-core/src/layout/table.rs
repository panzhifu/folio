//! 表格：网格抽取与列宽分配。
//!
//! 这一份**不认识 `Layout`**：输入是 DOM + 样式 + 可用宽，输出是「几行几列、每格是谁、
//! 每列多宽」。实排（`collect::Ctx::children`）与量高（`children_light`）两条路都调它，
//! 所以两份几何从构造上同源 —— 抄两遍的症状是滚动位置与屏上的块对不上。
//!
//! 落地范围（**都写在这里，别当已做**）：
//!
//! - CSS 2.1 的 **auto** 表格布局（§17.5.2.2 那个算法的简化版）。`table-layout: fixed`
//!   不认，判不认识。
//! - `colspan` / `rowspan` 认（HTML 属性，上限 64 格）：跨格只排一次，宽度取它跨过的
//!   那几列之和，固有宽按列数均摊；行高先按单行格算，再让跨行格把缺口**均摊**进去
//!   （CSS 按比例摊，这里取均分）。
//! - `border-collapse: collapse` 的**画**那一半做了（相邻两格公共边只画一条，见
//!   `Layout::decor_seeds`），几何没做（不互相让出半个边宽）。
//! - 格内竖直对齐做 `top` / `middle` / `bottom`（`CellVAlign`）。
//! - `display: inline-table` 不认（要的是行内块那一套 shrink-to-fit）。
//! - 量固有宽时，格子的**百分比**左右内边距按 0 折 —— 那时还没有列宽可当基准。
//!   绝对长度与 `em` 照常算。

use crate::content::dom::{Dom, NodeId};
use crate::content::format::css::computed::{BorderStyle, Display, StyledDom};

/// 一格：它从哪一列起、跨几列几行。
///
/// 跨格在**占位表**里覆盖的每一个格子都出现；`table()` 按 `cells` 逐格排，所以跨格
/// 只排一次，宽度取它跨过的那几列之和。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Cell {
    pub node: NodeId,
    pub column: usize,
    pub colspan: usize,
    pub rowspan: usize,
    pub row: usize,
}

/// 一张表的网格：`slots[r][c]` 是占这一格的那一格；`None` = 空位（短行、或被上一个
/// 跨行格占着之外的洞）。**行数取占位表的长度** —— 跨行格可以在最后一行之外再撑出几行。
pub(super) struct Grid {
    pub slots: Vec<Vec<Option<NodeId>>>,
    pub cells: Vec<Cell>,
    pub columns: usize,
}

/// 从一张表抽网格。行组（`thead` / `tbody` / `tfoot`）展开一层；不合规的孩子
/// （直接写在表里的文字、`<caption>`、`<colgroup>`）跳过。
///
/// CSS 会给这些孩子造匿名盒（匿名行、匿名格），我们不做：出版书里的表格是干净的，
/// 而匿名盒要额外牵出「匿名块继承哪一份样式、要不要参与列宽」那一整串决定。
///
/// `colspan` / `rowspan` 按 HTML 的属性解析（`0` / 非数字 / 缺省都当 1，上限 64 格）。
/// 铺格子时**跳过已被占的位置**（上一行跨下来的、或本行左边跨过来的），这是表格布局的
/// 标准做法，否则跨行格会把后面的格挤到错列。
pub(super) fn collect_grid(dom: &Dom, styles: &StyledDom, table: NodeId) -> Grid {
    let display_of = |node: NodeId| styles.get(node).map(|s| s.display);
    let mut raw: Vec<Vec<NodeId>> = Vec::new();
    for child in dom.element_children(table) {
        match display_of(child) {
            Some(Display::TableRow) => raw.push(cells_of(dom, &display_of, child)),
            Some(Display::TableRowGroup) => {
                for row in dom.element_children(child) {
                    if display_of(row) == Some(Display::TableRow) {
                        raw.push(cells_of(dom, &display_of, row));
                    }
                }
            }
            _ => {}
        }
    }
    let mut slots: Vec<Vec<Option<NodeId>>> = vec![Vec::new(); raw.len()];
    let mut cells = Vec::new();
    for (row, nodes) in raw.iter().enumerate() {
        let mut column = 0usize;
        for &node in nodes {
            // 找下一个空位：跳过被跨行 / 跨列格占掉的格子。
            while slots
                .get(row)
                .and_then(|r| r.get(column))
                .is_some_and(|slot| slot.is_some())
            {
                column += 1;
            }
            let colspan = span_attr(dom, node, "colspan");
            let rowspan = span_attr(dom, node, "rowspan");
            for dr in 0..rowspan {
                let rr = row + dr;
                while slots.len() <= rr {
                    slots.push(Vec::new());
                }
                while slots[rr].len() < column + colspan {
                    slots[rr].push(None);
                }
                for dc in 0..colspan {
                    slots[rr][column + dc] = Some(node);
                }
            }
            cells.push(Cell {
                node,
                column,
                colspan,
                rowspan,
                row,
            });
            column += colspan;
        }
    }
    let columns = slots.iter().map(|r| r.len()).max().unwrap_or(0);
    Grid {
        slots,
        cells,
        columns,
    }
}

/// HTML 的 `colspan` / `rowspan`：缺省、非数字、`0` 都当 1；上限 64 格。
fn span_attr(dom: &Dom, node: NodeId, name: &str) -> usize {
    dom.attr(node, name)
        .and_then(|v| v.trim().parse::<i64>().ok())
        .map(|n| n.clamp(1, 64) as usize)
        .unwrap_or(1)
}

fn cells_of(
    dom: &Dom,
    display_of: &impl Fn(NodeId) -> Option<Display>,
    row: NodeId,
) -> Vec<NodeId> {
    dom.element_children(row)
        .filter(|c| display_of(*c) == Some(Display::TableCell))
        .collect()
}

/// 一列的固有宽。
///
/// `min` 是这一列最长**不可断段**的宽，`max` 是一整行不断时的宽（两者都已含这一格的
/// 左右边框与内边距）。`fixed` 是作者写在这一列某一格上的 `width`（百分比已按表宽折成
/// px）—— 有它时这一列按定宽排，余下的宽再分给别的列。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) struct Column {
    pub min: f32,
    pub max: f32,
    pub fixed: Option<f32>,
}

impl Column {
    /// 把一格并进这一列：最小/最大各取较宽的那个，定宽取较宽的、没有就沿用已有的。
    pub fn absorb(&mut self, cell: Column) {
        self.min = self.min.max(cell.min);
        self.max = self.max.max(cell.max.max(cell.min));
        self.fixed = match (self.fixed, cell.fixed) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
    }
}

/// 列宽分配：CSS 2.1 §17.5.2.2 那个 auto 算法的简化版。
///
/// 三条，按顺序：
/// 1. **定宽的列**先拿走作者写的数（百分比已在调用方折成 px）。
/// 2. 余下的宽按「最小宽」保底：装不下就每列只给最小宽，表格溢出包含块
///    （CSS 允许表格溢出，而缩字是另一回事，不在这一层做）。
/// 3. 再往上的余量按各列 `max - min` 的比例摊；全都摊满之后还多的，
///    按 `max` 的比例继续摊（空表或全 0 宽时平分），于是**表格填满可用宽**。
///
/// 每一列至少给 1px：0 宽的列会让里面的字整列画在框外，比溢出错得隐蔽。
pub(super) fn solve_columns(columns: &[Column], available: f32) -> Vec<f32> {
    let mut out: Vec<f32> = columns.iter().map(|c| c.fixed.unwrap_or(0.0)).collect();
    let free: Vec<usize> = (0..columns.len())
        .filter(|&i| columns[i].fixed.is_none())
        .collect();
    if free.is_empty() {
        return out;
    }
    let fixed_sum: f32 = columns.iter().filter_map(|c| c.fixed).sum();
    let rest = (available - fixed_sum).max(0.0);
    let min_sum: f32 = free.iter().map(|&i| columns[i].min.max(1.0)).sum();
    let max_sum: f32 = free.iter().map(|&i| columns[i].max.max(1.0)).sum();

    // 只看「装不装得下」。**不能**把 `max_sum <= min_sum` 并进来当「装不下」：
    // 那表示所有列都没有伸缩空间（`min == max`），此时余量照样要摊出去 ——
    // 从前并进来过，症状是一张 `width:100px`、两列各 16px 固有宽的表只排 32px 宽。
    if rest <= min_sum {
        for &i in &free {
            out[i] = columns[i].min.max(1.0);
        }
        return out;
    }
    if rest >= max_sum {
        let extra = rest - max_sum;
        for &i in &free {
            let share = if max_sum > 0.0 {
                columns[i].max.max(1.0) / max_sum
            } else {
                1.0 / free.len() as f32
            };
            out[i] = columns[i].max.max(1.0) + extra * share;
        }
        return out;
    }
    let t = (rest - min_sum) / (max_sum - min_sum);
    for &i in &free {
        let min = columns[i].min.max(1.0);
        let max = columns[i].max.max(min);
        out[i] = min + (max - min) * t;
    }
    out
}

/// 相邻两格共用一条边时，**我这一侧**画不画。
///
/// CSS 2.1 §17.6.2 的冲突解法取前三条：
/// 1. 宽的赢；
/// 2. 一样宽看画法：`double` > `solid` > `dashed` > `dotted` > `none`；
/// 3. 再一样看位置：左右相邻时**左边**那格的颜色赢，上下相邻时**上边**那格的颜色赢 ——
///    所以站在右/下那一侧的人打平算赢，站在左/上那一侧的人打平算输。
///
/// `border-style: hidden` 折进 `none`（宽 0，本机语料 0 处），于是 CSS 里
/// 「`hidden` 压掉整条线」那一档给不出来：邻居的边框会赢。**记在这里，别当已做。**
pub(super) fn collapse_side_wins(
    mine: (f32, BorderStyle),
    theirs: (f32, BorderStyle),
    i_win_ties: bool,
) -> bool {
    match mine
        .0
        .partial_cmp(&theirs.0)
        .unwrap_or(std::cmp::Ordering::Equal)
    {
        std::cmp::Ordering::Greater => return true,
        std::cmp::Ordering::Less => return false,
        std::cmp::Ordering::Equal => {}
    }
    fn rank(style: BorderStyle) -> u8 {
        // CSS 2.1 §17.6.2 的次序（`hidden` 已折进 `none`）：
        // none < inset < groove < outset < ridge < dotted < dashed < solid < double。
        match style {
            BorderStyle::None => 0,
            BorderStyle::Inset => 1,
            BorderStyle::Groove => 2,
            BorderStyle::Outset => 3,
            BorderStyle::Ridge => 4,
            BorderStyle::Dotted => 5,
            BorderStyle::Dashed => 6,
            BorderStyle::Solid => 7,
            BorderStyle::Double => 8,
        }
    }
    match rank(mine.1).cmp(&rank(theirs.1)) {
        std::cmp::Ordering::Equal => i_win_ties,
        other => other == std::cmp::Ordering::Greater,
    }
}

#[cfg(test)]
mod tests {
    //! 只测纯的那两半：抽网格与分列宽。几何（行高、格子落点）在 `layout::tests` 里。
    //!
    //! 点名导入而不是 `use super::*`：`gpui_kit` 根有个名叫 `test` 的属性宏。

    use super::{Column, collapse_side_wins, collect_grid, solve_columns};
    use crate::content::dom::Dom;
    use crate::content::format::css::computed::BorderStyle;
    use crate::content::format::css::computed::{ComputedStyle, Display, cascade};

    fn grid(html: &str, css: &str) -> (usize, usize) {
        let dom = Dom::parse(html.as_bytes());
        let styles = cascade(&dom, &Default::default(), &ComputedStyle::default(), "");
        let _ = css;
        let table = dom
            .elements()
            .find(|(_, n)| n.tag.as_ref().map(|t| t.local.as_ref()) == Some("table"))
            .map(|(id, _)| id)
            .expect("文档里应有 <table>");
        let grid = collect_grid(&dom, &styles, table);
        (grid.slots.len(), grid.columns)
    }

    /// 三行两列；行组（`<tbody>`）要展开一层，`<caption>` 不进网格。
    #[test]
    fn the_grid_expands_row_groups_and_skips_captions() {
        let html = "<table><caption>表一</caption><thead><tr><th>甲</th><th>乙</th></tr></thead>\
                    <tbody><tr><td>一</td><td>二</td></tr><tr><td>三</td></tr></tbody></table>";
        let (rows, columns) = grid(html, "");
        assert_eq!((rows, columns), (3, 2), "thead 1 行 + tbody 2 行，两列");
    }

    /// 短行不补齐成 `None` 之外的形状：网格里短行就是短，列数取最长那一行。
    #[test]
    fn a_short_row_leaves_holes_not_phantom_cells() {
        let html = "<table><tr><td>一</td><td>二</td><td>三</td></tr><tr><td>四</td></tr></table>";
        let (rows, columns) = grid(html, "");
        assert_eq!((rows, columns), (2, 3));
    }

    /// `colspan`：一格占多列，后面的格顺延到它右边。
    #[test]
    fn a_colspan_cell_takes_several_columns() {
        let html = "<table><tr><td colspan=\"2\">甲</td><td>乙</td></tr></table>";
        let (_, columns) = grid(html, "");
        assert_eq!(columns, 3, "跨两列 + 一格 = 三列");
        let dom = Dom::parse(html.as_bytes());
        let styles = cascade(&dom, &Default::default(), &ComputedStyle::default(), "");
        let table = dom
            .elements()
            .find(|(_, n)| n.tag.as_ref().map(|t| t.local.as_ref()) == Some("table"))
            .map(|(id, _)| id)
            .unwrap();
        let grid = collect_grid(&dom, &styles, table);
        assert_eq!(grid.cells[0].colspan, 2);
        assert_eq!(grid.cells[0].column, 0);
        assert_eq!(grid.cells[1].column, 2, "第二格从第三列起");
    }

    /// `rowspan`：一格占多行，下一行**跳过**它占着的那一列。
    #[test]
    fn a_rowspan_cell_pushes_the_next_row_past_its_column() {
        let html = "<table><tr><td rowspan=\"2\">甲</td><td>乙</td></tr>\
                    <tr><td>丙</td></tr></table>";
        let (rows, columns) = grid(html, "");
        assert_eq!((rows, columns), (2, 2));
        let dom = Dom::parse(html.as_bytes());
        let styles = cascade(&dom, &Default::default(), &ComputedStyle::default(), "");
        let table = dom
            .elements()
            .find(|(_, n)| n.tag.as_ref().map(|t| t.local.as_ref()) == Some("table"))
            .map(|(id, _)| id)
            .unwrap();
        let grid = collect_grid(&dom, &styles, table);
        let second = grid
            .cells
            .iter()
            .find(|c| c.row == 1)
            .expect("第二行那一格");
        assert_eq!(
            second.column, 1,
            "第二行只有一个格，它要落在第 1 列（第 0 列被上面的跨行格占着）"
        );
    }

    /// 定宽的列先拿走作者写的数，其余列分剩下的；装不下时每列只给最小宽（表溢出）。
    #[test]
    fn fixed_columns_take_their_width_first() {
        let cols = [
            Column {
                min: 10.0,
                max: 20.0,
                fixed: Some(100.0),
            },
            Column {
                min: 10.0,
                max: 30.0,
                fixed: None,
            },
            Column {
                min: 10.0,
                max: 30.0,
                fixed: None,
            },
        ];
        let w = solve_columns(&cols, 300.0);
        assert_eq!(w[0], 100.0, "定宽那一列原样");
        assert!((w[1] + w[2] - 200.0).abs() < 0.01, "其余两列分剩下的 200");
        assert!((w[1] - 100.0).abs() < 0.01 && (w[2] - 100.0).abs() < 0.01);
    }

    /// 宽裕时填满可用宽：摊完 `max - min` 的差额后还有余，就按 `max` 继续摊。
    #[test]
    fn spare_width_fills_the_available_measure() {
        let cols = [
            Column {
                min: 10.0,
                max: 20.0,
                fixed: None,
            },
            Column {
                min: 10.0,
                max: 60.0,
                fixed: None,
            },
        ];
        let w = solve_columns(&cols, 200.0);
        assert!(
            (w.iter().sum::<f32>() - 200.0).abs() < 0.01,
            "总和该等于可用宽"
        );
        assert!(w[1] > w[0], "固有宽大的那列拿得多：{w:?}");
    }

    /// 装不下：每列给最小宽，**不缩字**、也不把列压成 0。
    #[test]
    fn a_narrow_measure_keeps_every_column_at_its_minimum() {
        let cols = [
            Column {
                min: 50.0,
                max: 100.0,
                fixed: None,
            },
            Column {
                min: 80.0,
                max: 100.0,
                fixed: None,
            },
        ];
        let w = solve_columns(&cols, 60.0);
        assert_eq!(w, vec![50.0, 80.0], "各自的最小宽，表整体溢出");
    }

    /// 所有列都**没有伸缩空间**（`min == max`）时，余量照样要摊出去。
    ///
    /// 从前这一档被并进「装不下」的分支，症状是一张 `width:100px`、两列固有宽各 16px
    /// 的表只排 32px 宽（跨列的表头最先把这条撞出来）。
    #[test]
    fn columns_without_slack_still_share_the_spare_width() {
        let cols = [
            Column {
                min: 16.0,
                max: 16.0,
                fixed: None,
            },
            Column {
                min: 16.0,
                max: 16.0,
                fixed: None,
            },
        ];
        assert_eq!(solve_columns(&cols, 100.0), vec![50.0, 50.0]);
        assert_eq!(
            solve_columns(&cols, 20.0),
            vec![16.0, 16.0],
            "真装不下时才落在最小宽"
        );
    }

    /// 空网格与零宽列不许给出 0：0 宽的列会把整列字画到框外。
    ///
    /// 一列什么都没有、可用宽也是 0 时给 1px 保底；可用宽有富余时它就铺满 ——
    /// `width: auto` 的表在调用方已经按内容收缩过一次，所以「铺满」不会真发生。
    #[test]
    fn degenerate_columns_still_get_a_pixel() {
        assert!(solve_columns(&[], 100.0).is_empty());
        assert_eq!(
            solve_columns(
                &[Column {
                    min: 0.0,
                    max: 0.0,
                    fixed: None,
                }],
                0.0,
            ),
            vec![1.0],
            "可用宽 0 时保底 1px，绝不出现 0 宽"
        );
        assert_eq!(
            solve_columns(
                &[Column {
                    min: 0.0,
                    max: 0.0,
                    fixed: None,
                }],
                100.0,
            ),
            vec![100.0],
            "有富余时这一列铺满"
        );
    }

    /// 合并时公共边归谁：宽 > 画法 > 位置。三条各钉一格。
    #[test]
    fn collapse_side_wins_by_width_then_style_then_position() {
        use BorderStyle::{Dashed, Double, Solid};
        assert!(
            collapse_side_wins((3.0, Solid), (1.0, Double), false),
            "宽的赢，哪怕对面画法更高级"
        );
        assert!(
            collapse_side_wins((2.0, Double), (2.0, Solid), false),
            "一样宽时 double 压 solid"
        );
        assert!(
            !collapse_side_wins((2.0, Dashed), (2.0, Solid), true),
            "solid 压 dashed，哪怕 dashed 站在能赢打平的那一侧"
        );
        assert!(
            collapse_side_wins((2.0, Solid), (2.0, Solid), true),
            "宽与画法都打平时由位置决定"
        );
        assert!(
            !collapse_side_wins((2.0, Solid), (2.0, Solid), false),
            "站在左/上那一侧的人打平算输（CSS 取靠左/靠上那格的颜色）"
        );
    }

    /// `display` 不认识表格四档时这张网格就是空的 —— 这里钉住 UA 表给的默认值。
    #[test]
    fn the_ua_sheet_turns_table_tags_into_table_displays() {
        let dom = Dom::parse("<table><tr><td>x</td></tr></table>".as_bytes());
        let styles = cascade(&dom, &Default::default(), &ComputedStyle::default(), "");
        let display = |tag: &str| {
            let id = dom
                .elements()
                .find(|(_, n)| n.tag.as_ref().map(|t| t.local.as_ref()) == Some(tag))
                .map(|(id, _)| id)
                .unwrap();
            styles.get(id).map(|s| s.display)
        };
        assert_eq!(display("table"), Some(Display::Table));
        assert_eq!(display("tr"), Some(Display::TableRow));
        assert_eq!(display("td"), Some(Display::TableCell));
    }
}
