//! 块级多栏容器（`columns` / `column-count` / `column-width`）的摆放。
//!
//! 从 `collect` 切出来只因为那个文件贴着行数上限；`impl Ctx` 的方法仍是同一个
//! `Ctx` 的方法，`Slot` / `wrap_paragraph` / 浮动表这些私有项按模块内可见直接取用。
//! 纯数（栏数解算与贪心配平）在 `layout::columns`，这一份只做「量、摆、收口」。

use super::super::columns;
use super::{Ctx, Display, NodeId, PlacedFloat, Positioning, Slot, attr_count};
use crate::wrap::Insets;
use folio_format::css::computed::ComputedStyle;

/// 一个单元（一根栏里的一段）排到临时坐标后的账：盒子与行盒区间、被隔离的
/// 浮动盒、量出的高与上下外边距。配平定稿后整体平移到最终栏位。
struct Unit {
    lines: (usize, usize),
    blocks: (usize, usize),
    floats: Vec<PlacedFloat>,
    h: f32,
    mt: f32,
    mb: f32,
}

impl<'a> Ctx<'a> {
    /// 摆一个多栏容器，返回边框盒高。
    ///
    /// 与 `flex()` 同构：先把每个单元排到临时坐标（栏 0 的 x、y=0）量出高，贪心
    /// 装栏（纯数见 `columns::balance`），再把每根栏的盒子/行盒/浮动盒整体平移到
    /// 栏位。与普通块流的分界：栏内沿用外边距折叠与列标记的同一套算术，栏与栏
    /// 之间互不折、互不让位。
    ///
    /// 降级口径（都记在这里，`ComputedStyle::column_count` 指过来）：
    /// - 配平是**贪心按序**装栏（目标 = 总高 ÷ 栏数），不是 CSS 的最小化最大栏高；
    ///   单元**不跨栏拆**，比目标高的单元独占一栏并把它撑高；
    /// - 单元自身的 `float` 不再浮（与 `flex()` 对子项的口径一致），照块排；
    /// - 跨页切片沿用分片器的逐行切法：栏内内容按 y 带切断续到下一页，同一栏
    ///   的 x 不变 —— 不是 CSS「每页重开各栏」的那一档。
    // plan 是调用方按同一份盒模型解好的那一份，单拎一个参数比塞进宽度里诚实。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn columns_flow(
        &mut self,
        node: NodeId,
        x: f32,
        y: f32,
        width: f32,
        basis: f32,
        cb_h: Option<f32>,
        plan: columns::Plan,
    ) -> f32 {
        let style = self.style_of(node);
        let [bt, br, bb, bl] = style.border_drawn();
        let pl = bl + style.padding_left.resolve(basis).max(0.0);
        let pr = br + style.padding_right.resolve(basis).max(0.0);
        let pt = bt + style.padding_top.resolve(basis).max(0.0);
        let pb = bb + style.padding_bottom.resolve(basis).max(0.0);
        let content_w = (width - pl - pr).max(1.0);
        let col_w = ((content_w - plan.gap * (plan.count - 1) as f32) / plan.count as f32).max(1.0);
        let col0_x = x + pl;
        let index = self.push_block(node, x, y, width, String::new(), true);

        let mut kids: Vec<NodeId> = Vec::new();
        for &child in self.children_of(node) {
            let s = self.style_of(child);
            if s.display == Display::None {
                continue;
            }
            if matches!(s.position, Positioning::Absolute | Positioning::Fixed) {
                self.absolute.push((child, y));
                continue;
            }
            kids.push(child);
        }
        if self.light {
            let inner = self.columns_height(&kids, col_w, plan.count);
            return self.close_block(
                index,
                pt + Self::used_content_height(style, inner, pt + pb, cb_h) + pb,
            );
        }

        // ---- 完整档：逐单元排到临时坐标（栏 0 的 x、y=0）量出高 ----
        let mut units: Vec<Unit> = Vec::new();
        let mut pending: Vec<NodeId> = Vec::new();
        let mut ordinal: i64 = attr_count(self.dom, node, "start").map_or(0, |start| start - 1);
        for &child in &kids {
            let s = self.style_of(child);
            if self.is_inline_flow(child, s) {
                pending.push(child);
                continue;
            }
            if !pending.is_empty() {
                let pieces = std::mem::take(&mut pending);
                units.push(self.flush_paragraph(node, &pieces, col0_x, col_w, style));
            }
            let is_item = s.display == Display::ListItem;
            if is_item {
                ordinal = attr_count(self.dom, child, "value").unwrap_or(ordinal + 1);
            }
            let unit = self.lay_unit(child, col0_x, col_w);
            let lines_start = unit.lines.0;
            units.push(unit);
            // 列标记贴在这一单元的行上（此刻还在临时坐标，平移时跟着走）。
            if is_item {
                self.attach_marker(child, s, ordinal.max(1) as usize, lines_start);
            }
        }
        if !pending.is_empty() {
            units.push(self.flush_paragraph(node, &pending, col0_x, col_w, style));
        }

        // ---- 配平定稿，逐栏平移到最终位置 ----
        let weights: Vec<f32> = units.iter().map(|u| u.mt + u.h).collect();
        let mut content_h = 0.0f32;
        for (k, col) in columns::balance(&weights, plan.count).iter().enumerate() {
            let dx = k as f32 * (col_w + plan.gap);
            let mut cy = 0.0f32;
            let mut prev_bottom = 0.0f32;
            for &ui in col {
                let unit = &mut units[ui];
                cy += Self::collapse_margins(unit.mt, prev_bottom);
                let dy = pt + cy;
                for line in &mut self.lines[unit.lines.0..unit.lines.1] {
                    line.x += dx;
                    line.y += dy;
                }
                for block in &mut self.blocks[unit.blocks.0..unit.blocks.1] {
                    block.x += dx;
                    block.y += dy;
                }
                for f in &mut unit.floats {
                    f.x += dx;
                    f.y += dy;
                }
                self.floats.append(&mut unit.floats);
                cy += unit.h;
                prev_bottom = unit.mb;
            }
            content_h = content_h.max(cy);
        }
        self.close_block(
            index,
            pt + Self::used_content_height(style, content_h, pt + pb, cb_h) + pb,
        )
    }

    /// 轻量档：逐子块量高（口径与 `children_light` 一致 —— 行内子节点也按块量），
    /// 贪心装栏取最高栏。
    fn columns_height(&mut self, kids: &[NodeId], col_w: f32, count: u16) -> f32 {
        let mut weights: Vec<f32> = Vec::with_capacity(kids.len());
        for &child in kids {
            let s = self.style_of(child);
            weights.push(s.margin_top.resolve(col_w) + self.block_light(child, col_w, col_w, None));
        }
        let mut content_h = 0.0f32;
        for col in columns::balance(&weights, count).iter() {
            let mut h = 0.0f32;
            let mut prev_bottom = 0.0f32;
            for &ui in col {
                let s = self.style_of(kids[ui]);
                h += Self::collapse_margins(s.margin_top.resolve(col_w), prev_bottom) + weights[ui];
                prev_bottom = s.margin_bottom.resolve(col_w);
            }
            content_h = content_h.max(h);
        }
        content_h
    }

    /// 排一个块级子节点到临时坐标，收走它摆出的浮动盒（隔离）。
    fn lay_unit(&mut self, child: NodeId, tx: f32, col_w: f32) -> Unit {
        let style = self.style_of(child);
        let lines_start = self.lines.len();
        let blocks_start = self.blocks.len();
        let floats_start = self.floats.len();
        let h = if style.display == Display::Table {
            self.table(child, tx, 0.0, col_w, col_w, None)
        } else {
            self.block(child, tx, 0.0, col_w, col_w, None)
        };
        Unit {
            lines: (lines_start, self.lines.len()),
            blocks: (blocks_start, self.blocks.len()),
            floats: self.floats.drain(floats_start..).collect(),
            h,
            mt: style.margin_top.resolve(col_w),
            mb: style.margin_bottom.resolve(col_w),
        }
    }

    /// 把一段行内内容冲成匿名段（单元），同样排到临时坐标并隔离浮动盒。
    fn flush_paragraph(
        &mut self,
        owner: NodeId,
        pieces: &[NodeId],
        tx: f32,
        col_w: f32,
        style: &ComputedStyle,
    ) -> Unit {
        let lines_start = self.lines.len();
        let blocks_start = self.blocks.len();
        let floats_start = self.floats.len();
        let h = self.wrap_paragraph(
            owner,
            pieces,
            Slot {
                x: tx,
                y: 0.0,
                width: col_w,
                basis: col_w,
            },
            &Insets(None),
            style,
        );
        Unit {
            lines: (lines_start, self.lines.len()),
            blocks: (blocks_start, self.blocks.len()),
            floats: self.floats.drain(floats_start..).collect(),
            h,
            mt: 0.0,
            mb: 0.0,
        }
    }
}
