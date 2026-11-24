//! 网格容器（`display: grid`）的排版：列宽求解、自动放置、逐项摆放。
//!
//! 从 `collect` 切出来只因为 collect.rs 贴着行数上限；`impl Ctx` 的方法仍是同一个
//! `Ctx` 的方法。轨道求解与放置这两个纯数在 `layout::grid`，这一份只做「量、摆、收口」。

use super::{AlignItems, Ctx, Display, NodeId, Positioning};
use folio_format::css::computed::{GridTrack, JustifySelf};

impl<'a> Ctx<'a> {
    /// 摆一个网格容器（`display: grid`），返回边框盒高。
    ///
    /// 与 `flex()` 同构：先按 y=0 把每个子项排进自己的格子，量出各行高，再把行/块区间
    /// 整体平移到最终位置。`self.light` 只决定每项是 `block` 还是 `block_light`。
    pub(super) fn grid(
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
        let content_w = (width - pl - pr).max(1.0);
        let mut items: Vec<NodeId> = self.children_of(node).to_vec();
        items.retain(|&child| {
            let s = self.style_of(child);
            if s.display == Display::None {
                return false;
            }
            if matches!(s.position, Positioning::Absolute | Positioning::Fixed) {
                self.absolute.push((child, y));
                return false;
            }
            true
        });
        let index = self.push_block(node, x, y, width, String::new(), true);
        if items.is_empty() {
            return self.close_block(index, pt + pb);
        }
        let row_gap = style
            .row_gap
            .map(|l| l.resolve(content_w))
            .unwrap_or(0.0)
            .max(0.0);
        let col_gap = style
            .column_gap
            .map(|l| l.resolve(content_w))
            .unwrap_or(0.0)
            .max(0.0);
        let mut tracks: Vec<GridTrack> = style.grid_template_columns.to_vec();
        if tracks.is_empty() {
            tracks.push(GridTrack::Auto);
        }
        let columns = tracks.len();
        let spec: Vec<(Option<u16>, u16)> = items
            .iter()
            .map(|&child| {
                let s = self.style_of(child);
                (s.grid_column_start, s.grid_column_span)
            })
            .collect();
        let (cells, rows) = crate::grid::place(columns, &spec);
        // `auto` 轨道取该列所有子项的最大内容宽；`widths` 之后再统一求解。
        let mut auto_widths = vec![0.0f32; columns];
        for (i, &child) in items.iter().enumerate() {
            let column = cells[i].1;
            let width = self.flex_intrinsic_width(child, content_w).min(content_w);
            auto_widths[column] = auto_widths[column].max(width);
        }
        let widths = crate::grid::resolve_columns(&tracks, content_w, col_gap, &auto_widths);
        let mut col_x = vec![0.0f32; columns];
        let mut cursor = 0.0f32;
        for (column, width) in widths.iter().enumerate() {
            col_x[column] = cursor;
            cursor += width + col_gap;
        }
        // 跨列宽 = 跨过的列宽之和 + 之间的缝。
        let span_width = |column: usize, span: u16| -> f32 {
            let end = (column + span.max(1) as usize).min(columns);
            widths[column..end].iter().sum::<f32>()
                + col_gap * (end.saturating_sub(column + 1)) as f32
        };
        if self.light {
            let mut row_h = vec![0.0f32; rows];
            for (i, &child) in items.iter().enumerate() {
                let (row, column) = cells[i];
                let cell_w = span_width(column, self.style_of(child).grid_column_span).max(1.0);
                let height = self.block_light(child, cell_w, cell_w, None);
                row_h[row] = row_h[row].max(height);
            }
            let content_h = row_h.iter().sum::<f32>() + row_gap * (rows.saturating_sub(1)) as f32;
            return self.close_block(
                index,
                pt + Self::used_content_height(style, content_h, pt + pb, cb_h) + pb,
            );
        }
        /// 一格排完后的账：块/行区间、高、所在行，用来统一平移。
        struct Placed {
            block_start: usize,
            block_end: usize,
            line_start: usize,
            line_end: usize,
            height: f32,
            row: usize,
        }
        let mut placed: Vec<Placed> = Vec::with_capacity(items.len());
        for (i, &child) in items.iter().enumerate() {
            let (row, column) = cells[i];
            let child_style = self.style_of(child);
            let cell_w = span_width(column, child_style.grid_column_span).max(1.0);
            // `justify-self`：End/Center/Start 按内容固有宽摆，Auto/Stretch 铺满格子。
            let intrinsic = || {
                self.flex_intrinsic_width(child, cell_w)
                    .min(cell_w)
                    .max(1.0)
            };
            let (item_w, dx) = match child_style.justify_self {
                JustifySelf::Start => (intrinsic(), 0.0),
                JustifySelf::Center => {
                    let w = intrinsic();
                    (w, (cell_w - w) / 2.0)
                }
                JustifySelf::End => {
                    let w = intrinsic();
                    (w, cell_w - w)
                }
                JustifySelf::Auto | JustifySelf::Stretch => (cell_w, 0.0),
            };
            let item_x = x + pl + col_x[column] + dx;
            let line_start = self.lines.len();
            let block_start = self.blocks.len();
            let height = self.block(child, item_x, 0.0, item_w, item_w, None);
            placed.push(Placed {
                block_start,
                block_end: self.blocks.len(),
                line_start,
                line_end: self.lines.len(),
                height,
                row,
            });
        }
        let mut row_h = vec![0.0f32; rows];
        for slot in &placed {
            row_h[slot.row] = row_h[slot.row].max(slot.height);
        }
        let mut row_y = vec![0.0f32; rows];
        let mut cursor = 0.0f32;
        for row in 0..rows {
            row_y[row] = cursor;
            cursor += row_h[row] + row_gap;
        }
        for slot in placed {
            let align_dy = match style.align_items {
                AlignItems::Start | AlignItems::Stretch => 0.0,
                AlignItems::End => row_h[slot.row] - slot.height,
                AlignItems::Center => (row_h[slot.row] - slot.height) / 2.0,
            };
            let dy = y + pt + row_y[slot.row] + align_dy;
            for line_box in &mut self.lines[slot.line_start..slot.line_end] {
                line_box.y += dy;
            }
            for block in &mut self.blocks[slot.block_start..slot.block_end] {
                block.y += dy;
            }
            if style.align_items == AlignItems::Stretch
                && let Some(block) = self.blocks.get_mut(slot.block_start)
            {
                block.height = row_h[slot.row];
            }
        }
        let content_h = row_h.iter().sum::<f32>() + row_gap * (rows.saturating_sub(1)) as f32;
        self.close_block(
            index,
            pt + Self::used_content_height(style, content_h, pt + pb, cb_h) + pb,
        )
    }
}
