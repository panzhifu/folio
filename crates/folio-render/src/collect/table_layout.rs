//! 表格排版：`display: table` 的网格、列宽与逐格摆放。
//!
//! 从 `collect` 切出来只因为 collect.rs 贴着行数上限；`impl Ctx` 的方法仍是同一个 `Ctx` 的方法。

use super::{BlockBox, CellVAlign, Ctx, Length, NodeId, Sizes, WordBreak, collect, table};
use unicode_linebreak::linebreaks;

impl<'a> Ctx<'a> {
    /// 摆一张表（`display: table`），返回边框盒高。
    ///
    /// 实排与量高两条路共用这一份：网格、每列的固有宽、列宽分配、每行的行高与格子落点
    /// 全在这里算**一次**，`self.light` 只决定「每格是 `block` 还是 `block_light`」。
    /// 分开写两份的症状是滚动位置与屏上的块对不上，而那种错在无头测试里只表现为
    /// 「总高差几个像素」。
    ///
    /// 单元格**顶对齐**（CSS 表格格的默认 `vertical-align: top`），所以格子内容按行顶排出
    /// 之后不必平移；行高只用来把格子的**盒子**拉满 —— 边框与底色因此铺满一整行，
    /// 与浏览器一致。落地范围的边界写在 `layout::table` 的模块文档上。
    pub(super) fn table(
        &mut self,
        node: NodeId,
        x: f32,
        y: f32,
        width: f32,
        basis: f32,
        cb_h: Option<f32>,
    ) -> f32 {
        let style = self.style_of(node);
        let [bt, br, bb, bl] = style.border_drawn();
        let pl = bl + style.padding_left.resolve(basis).max(0.0);
        let pr = br + style.padding_right.resolve(basis).max(0.0);
        let pt = bt + style.padding_top.resolve(basis).max(0.0);
        let pb = bb + style.padding_bottom.resolve(basis).max(0.0);
        let grid = table::collect_grid(self.dom, self.styles, node);
        if grid.cells.is_empty() || grid.columns == 0 {
            // 空表（只有 `<caption>` 或格式不对）：留一个 0 内容高的盒子，
            // 它的 padding / 边框照画 —— 作者写的盒模型不该因为没格子就消失。
            let index = self.push_block(node, x, y, width, String::new(), true);
            return self.close_block(index, pt + pb);
        }
        // `border-spacing` 只在 `separate`（初值）下生效；塌陷模式里它被忽略（CSS 2.1 §17.6.1）。
        let spacing = if style.border_collapse {
            [0.0f32, 0.0]
        } else {
            style.border_spacing
        };
        let gaps_x = spacing[0] * (grid.columns.saturating_sub(1)) as f32;
        let available = (width - pl - pr - gaps_x).max(1.0);
        // 每列的固有宽。跨格的固有宽按**列数均摊**（CSS 按比例分，这里取均分）：
        // 一格跨两列要 100px，那两列各记 50。
        //
        // `table-layout: fixed`（CSS 2.1 §17.5.2.1）另走一条：列宽**只**由第一行的
        // 作者宽度定，后面各行的长内容不再把列撑宽。第一行没写宽度的列平分余量 ——
        // 把它们的 min/max 都留 0，`solve_columns` 的宽裕臂就会按等份摊（`max` 全相等）。
        let mut columns = vec![table::Column::default(); grid.columns];
        if style.table_layout_fixed {
            for cell in grid.cells.iter().filter(|cell| cell.row == 0) {
                let cell_style = self.style_of(cell.node);
                let [_, br, _, bl] = cell_style.border_drawn();
                // 与 `cell_intrinsic` 同一口径：百分比内边距按 0 折（此时还没有列宽基准）。
                let pad_x = bl
                    + br
                    + cell_style.padding_left.resolve(0.0).max(0.0)
                    + cell_style.padding_right.resolve(0.0).max(0.0);
                let fixed = cell_style.width.and_then(|length| match length {
                    Length::Px(v) => Some(v + pad_x),
                    Length::Percent(p) => Some(p * available + pad_x),
                    Length::Auto => None,
                });
                let span = cell.colspan as f32;
                let share = table::Column {
                    min: 0.0,
                    max: 0.0,
                    fixed: fixed.map(|width| width / span),
                };
                let end = (cell.column + cell.colspan).min(grid.columns);
                for column in &mut columns[cell.column..end] {
                    column.absorb(share);
                }
            }
        } else {
            for cell in &grid.cells {
                let intrinsic = self.cell_intrinsic(cell.node, available);
                let span = cell.colspan as f32;
                let share = table::Column {
                    min: intrinsic.min / span,
                    max: intrinsic.max / span,
                    fixed: intrinsic.fixed.map(|fixed| fixed / span),
                };
                let end = (cell.column + cell.colspan).min(grid.columns);
                for column in &mut columns[cell.column..end] {
                    column.absorb(share);
                }
            }
        }
        // CSS 里 `width: auto` 的表格**收缩到内容宽**（上限是可用宽），不铺满版心 ——
        // 一张两列的小表被拉成整页宽，正是「表格看起来坏了」最典型的那个症状。
        // 收缩之后 `margin: auto` 的中缝要重算：`solve_block_width` 是按可用宽均分的。
        // fixed 表格即使 `width: auto` 也铺满可用宽（CSS §17.5.2.1），不跟着内容缩。
        let mut x = x;
        let mut box_width = width;
        if style.width.is_none() && !style.table_layout_fixed {
            let natural: f32 =
                columns.iter().map(|c| c.max.max(1.0)).sum::<f32>() + gaps_x + pl + pr;
            box_width = natural.min(width);
            if box_width < width {
                let (had_ml, _) = self.solve_block_width(style, width, x);
                let ml = match (
                    style.margin_left == Length::Auto,
                    style.margin_right == Length::Auto,
                ) {
                    (true, true) => (width - box_width) / 2.0,
                    (true, false) => width - box_width,
                    _ => had_ml,
                };
                x = x - had_ml + ml;
            }
        }
        let content_width = (box_width - pl - pr).max(1.0);
        // 列宽是在**扣掉缝之后**的那一段里分的 —— 忘了这一步，缝会被当余量摊进列里
        // （两列各 32、缝 10 的表会排成两列各 37 而没有缝）。
        let widths = table::solve_columns(&columns, (content_width - gaps_x).max(1.0));
        // 每列沿横轴的起点（含缝）。
        let mut col_left = Vec::with_capacity(grid.columns);
        let mut cursor = 0.0f32;
        for (column, width) in widths.iter().enumerate() {
            col_left.push(cursor);
            cursor += width;
            if column + 1 < grid.columns {
                cursor += spacing[0];
            }
        }
        let span_width = |cell: &table::Cell| -> f32 {
            let end = (cell.column + cell.colspan).min(grid.columns);
            let span = &widths[cell.column..end];
            let mut width: f32 = span.iter().sum();
            // 跨过的列之间那几道缝也算进这一格。
            width += spacing[0] * span.len().saturating_sub(1) as f32;
            width
        };
        let index = self.push_block(node, x, y, box_width, String::new(), true);
        /// 一格排完之后记下的账：它的块/行区间、内容高、跨了哪几列几行。
        struct Placed {
            block_start: usize,
            block_end: usize,
            line_start: usize,
            line_end: usize,
            height: f32,
            cell: table::Cell,
            width: f32,
        }
        // 逐格先按 **y = 0** 排：行高要等所有格子都量完才知道（跨行格还会把行撑高），
        // 位置最后统一挪 —— 与块缓存命中时的平移是同一件事。
        let mut placed: Vec<Placed> = Vec::with_capacity(grid.cells.len());
        for cell in &grid.cells {
            let cell_w = span_width(cell);
            let cell_x = x + pl + col_left[cell.column];
            let block_start = self.blocks.len();
            let line_start = self.lines.len();
            let height = if self.light {
                let height = self.block_light(cell.node, cell_w, cell_w, None);
                // 量高这条路不产出行盒，格子自己的盒子要在这里补记 ——
                // 与 `children_light` 给普通块补记那一条同一个理由。
                self.blocks.push(BlockBox {
                    node: cell.node,
                    x: cell_x,
                    y: 0.0,
                    width: cell_w,
                    height,
                    text: String::new(),
                    lines: 0..0,
                    border_box: true,
                });
                height
            } else {
                self.block(cell.node, cell_x, 0.0, cell_w, cell_w, None)
            };
            placed.push(Placed {
                block_start,
                block_end: self.blocks.len(),
                line_start,
                line_end: self.lines.len(),
                height,
                cell: *cell,
                width: cell_w,
            });
        }
        // 行高：单行格先定自然高，再让跨行格把缺口**均摊**进去（CSS 按比例摊，这里均分）。
        // 跨行格按 rowspan 从小到大处理，这样小的先撑、大的拿到的是已经撑过的行高。
        let row_count = grid.slots.len().max(1);
        let mut row_h = vec![0.0f32; row_count];
        for slot in &placed {
            if slot.cell.rowspan == 1 && slot.cell.row < row_count {
                row_h[slot.cell.row] = row_h[slot.cell.row].max(slot.height);
            }
        }
        let mut spanning: Vec<usize> = (0..placed.len())
            .filter(|i| placed[*i].cell.rowspan > 1)
            .collect();
        spanning.sort_by_key(|i| placed[*i].cell.rowspan);
        for index_ in spanning {
            let start = placed[index_].cell.row;
            let end = (start + placed[index_].cell.rowspan).min(row_count);
            if end <= start {
                continue;
            }
            let have: f32 = row_h[start..end].iter().sum();
            let deficit = placed[index_].height - have;
            if deficit > 0.0 {
                let share = deficit / (end - start) as f32;
                for height in &mut row_h[start..end] {
                    *height += share;
                }
            }
        }
        // 行顶（含行间的缝）。
        let mut row_top = vec![0.0f32; row_count];
        let mut cursor = 0.0f32;
        for row in 0..row_count {
            row_top[row] = cursor;
            cursor += row_h[row];
            if row + 1 < row_count {
                cursor += spacing[1];
            }
        }
        // 摆到位：内容整体挪到行顶，盒子拉到跨行总高，再按 `vertical-align` 挪内容。
        for slot in placed {
            let start = slot.cell.row.min(row_count - 1);
            let end = (slot.cell.row + slot.cell.rowspan)
                .min(row_count)
                .max(start + 1);
            let spanned =
                row_h[start..end].iter().sum::<f32>() + spacing[1] * (end - start - 1) as f32;
            let top = y + pt + row_top[start];
            if top != 0.0 && !self.light {
                for line in &mut self.lines[slot.line_start..slot.line_end] {
                    line.y += top;
                }
                for block in &mut self.blocks[slot.block_start..slot.block_end] {
                    block.y += top;
                }
            }
            self.blocks[slot.block_start].x = x + pl + col_left[slot.cell.column];
            self.blocks[slot.block_start].y = top;
            self.blocks[slot.block_start].width = slot.width;
            self.blocks[slot.block_start].height = spanned.max(slot.height);
            let valign = self
                .style_of(self.blocks[slot.block_start].node)
                .cell_valign;
            let dy = match valign {
                CellVAlign::Top => 0.0,
                CellVAlign::Middle => (spanned - slot.height) / 2.0,
                CellVAlign::Bottom => spanned - slot.height,
            };
            if dy > 0.0 && !self.light {
                for line in &mut self.lines[slot.line_start..slot.line_end] {
                    line.y += dy;
                }
                // 第 0 块是格子自己的边框盒（它已经拉到跨行总高，不动）；里面那些跟着挪。
                for block in &mut self.blocks[slot.block_start + 1..slot.block_end] {
                    block.y += dy;
                }
            }
        }
        self.close_block(
            index,
            pt + Self::used_content_height(style, cursor, pt + pb, cb_h) + pb,
        )
    }

    /// `min` 是最长不可断段的宽。断点用 `unicode_linebreak`，与折行那一趟同一把尺 ——
    /// 自己另写一套「哪里能断」的判据，列宽就会与真正的折行位置对不上。
    ///
    /// 量的是这一格**整棵子树**的文本，按一整段算：格子里有多个段落时，真实的最大宽
    /// 是「最宽那一段」而不是「全加起来」。这个偏大会被列宽分配的「按比例摊」吃掉一部分，
    /// 而表格里一格多段本就少见（本机四本语料 0 处），所以先按一整段算。
    fn cell_intrinsic(&self, cell: NodeId, table_width: f32) -> table::Column {
        let style = self.style_of(cell);
        let [_, br, _, bl] = style.border_drawn();
        // 百分比内边距在这里按 0 折：量固有宽时还没有一列宽可当基准（见 `layout::table`）。
        let pad_x = bl
            + br
            + style.padding_left.resolve(0.0).max(0.0)
            + style.padding_right.resolve(0.0).max(0.0);
        let collected = collect(
            &[cell],
            self.dom,
            self.styles,
            style.white_space_pre,
            self.images,
            self.measure,
            Sizes {
                width: table_width,
                screen_width: self.screen.map(|(w, _)| w),
                page_height: self.page_height,
                screen_height: self.screen.map(|(_, h)| h),
            },
        );
        let widths = &collected.widths;
        let max: f32 = widths.iter().sum();
        let mut min = 0.0f32;
        if style.word_break == WordBreak::BreakAll || style.overflow_wrap_break {
            // 任意两字之间都能断 ⇒ 最小宽就是**最宽的那个字**。
            min = widths.iter().copied().fold(0.0, f32::max);
        } else {
            let mut start = 0usize;
            for (at, _) in linebreaks(&collected.text) {
                if at > start && at <= widths.len() {
                    min = min.max(widths[start..at].iter().sum::<f32>());
                }
                start = at;
            }
            if start < widths.len() {
                min = min.max(widths[start..].iter().sum::<f32>());
            }
        }
        let fixed = style.width.and_then(|length| match length {
            Length::Px(v) => Some(v),
            Length::Percent(p) => Some(p * table_width),
            Length::Auto => None,
        });
        table::Column {
            min: min + pad_x,
            max: max.max(min) + pad_x,
            fixed,
        }
    }
} // impl Ctx
