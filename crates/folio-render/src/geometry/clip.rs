//! overflow 裁剪框的数据形状与折叠查询。
//!
//! 从 `geometry.rs` 切出来只因为那份文件贴着行数基线；这里读的是排版折好的块/行表，
//! 仍属于 `layout` 内部机制，不对外暴露遍历规则。

use folio_format::css::computed::StyledDom;

use super::{BlockBox, Layout};

/// 一个矩形裁剪框（章内物理坐标，含头不含尾）。
///
///  unbounded 的那一边用 ±∞ 表示：只写了 `overflow-x` 或只写了 `overflow-y` 时，
/// 另一根轴不能凭空给个数去裁。`None` 才表示「这一行没有任何 overflow 祖先」。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipRect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl ClipRect {
    pub fn unbounded() -> Self {
        Self {
            x0: f32::NEG_INFINITY,
            y0: f32::NEG_INFINITY,
            x1: f32::INFINITY,
            y1: f32::INFINITY,
        }
    }

    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self {
            x0,
            y0,
            x1: x1.max(x0),
            y1: y1.max(y0),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    pub fn intersect(self, other: Self) -> Self {
        Self {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }

    pub fn intersects_rect(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> bool {
        x0 < self.x1 && x1 > self.x0 && y0 < self.y1 && y1 > self.y0
    }
}

/// 每个块的包含块：块按先序入表，父块的行区间必包住子块。
///
/// 区间相等时后入表的子块仍认先入表的父块（内容盒副本与边框盒就是这一档），
/// 夹紧的 `start < end` 保证空块不会把下一兄弟认成子块。
pub(super) fn fold_block_parents(blocks: &[BlockBox]) -> Vec<Option<u32>> {
    let mut out = vec![None; blocks.len()];
    let mut stack: Vec<usize> = Vec::new();
    for (ix, block) in blocks.iter().enumerate() {
        while let Some(&at) = stack.last() {
            let parent = &blocks[at];
            if block.lines.start >= parent.lines.start && block.lines.start < parent.lines.end {
                break;
            }
            stack.pop();
        }
        out[ix] = stack.last().and_then(|&at| u32::try_from(at).ok());
        // 零行块也压栈：它自己的子块可能排在它后面，而它的 `lines` 区间
        // 在 `close_block` 之后已经包含那些子块。空区间在下一块处自然弹出。
        stack.push(ix);
    }
    out
}

impl Layout {
    /// 这一行属于哪一条**最深的边框盒**（与 `block_of_line` 同一口径）。
    pub(super) fn line_owner(&self, i: usize) -> Option<usize> {
        let at = *self.owner.get(i)?;
        (at != u32::MAX).then_some(at as usize)
    }

    /// 这一块自己造成的裁剪框（章内物理坐标）。只认 `overflow: hidden|clip`：
    /// `scroll` / `auto` 在引擎里没有滚动容器，所以按可见处理。`None` = 不裁。
    ///
    /// 裁剪的是**边框盒**：border 与 padding 里的内容仍可见，超出边框盒才裁。
    fn own_clip_rect(&self, i: usize, styles: &StyledDom) -> Option<ClipRect> {
        let block = self.blocks.get(i)?;
        let style = styles.get(block.node)?;
        let hidden = |o: folio_format::css::computed::Overflow| {
            matches!(
                o,
                folio_format::css::computed::Overflow::Hidden
                    | folio_format::css::computed::Overflow::Clip
            )
        };
        if !hidden(style.overflow_x) && !hidden(style.overflow_y) {
            return None;
        }
        Some(ClipRect {
            x0: if hidden(style.overflow_x) {
                block.x
            } else {
                f32::NEG_INFINITY
            },
            y0: if hidden(style.overflow_y) {
                block.y
            } else {
                f32::NEG_INFINITY
            },
            x1: if hidden(style.overflow_x) {
                block.x + block.width
            } else {
                f32::INFINITY
            },
            y1: if hidden(style.overflow_y) {
                block.y + block.height
            } else {
                f32::INFINITY
            },
        })
    }

    /// 这一行所有祖先的裁剪框取交；没有任何 overflow 祖先时 `None`。
    pub fn clip_rect(&self, line: usize, styles: &StyledDom) -> Option<ClipRect> {
        let mut owner = self.line_owner(line);
        let mut clip = ClipRect::unbounded();
        let mut any = false;
        while let Some(at) = owner {
            if let Some(own) = self.own_clip_rect(at, styles) {
                any = true;
                clip = clip.intersect(own);
            }
            owner = self
                .block_parent
                .get(at)
                .copied()
                .flatten()
                .map(|parent| parent as usize);
        }
        any.then_some(clip)
    }

    /// 一个块自己的内容被**祖先**裁到什么程度（不含这一块自己的 `overflow`）。
    ///
    /// 装饰种子用这一条：块自己的底色与边框不因自己的 `overflow: hidden` 被裁，
    /// 但它在某个 overflow 祖先里面时仍要随祖先一起裁。
    pub fn ancestor_clip_rect(&self, block: usize, styles: &StyledDom) -> Option<ClipRect> {
        let mut parent = self
            .block_parent
            .get(block)
            .copied()
            .flatten()
            .map(|at| at as usize);
        let mut clip = ClipRect::unbounded();
        let mut any = false;
        while let Some(at) = parent {
            if let Some(own) = self.own_clip_rect(at, styles) {
                any = true;
                clip = clip.intersect(own);
            }
            parent = self
                .block_parent
                .get(at)
                .copied()
                .flatten()
                .map(|next| next as usize);
        }
        any.then_some(clip)
    }

    /// 排完、样式仍在手边时折一次「每行的裁剪框」。之后命中、选区、绘制都读
    /// [`line_clip`](Self::line_clip)，不再各自遍历祖先。
    pub fn fold_clips(&mut self, styles: &StyledDom) {
        self.line_clips = (0..self.lines.len())
            .map(|line| self.clip_rect(line, styles))
            .collect();
    }

    /// 这一行的裁剪框（[`fold_clips`](Self::fold_clips) 的产物）；`None` = 不裁。
    pub fn line_clip(&self, line: usize) -> Option<ClipRect> {
        self.line_clips.get(line).copied().flatten()
    }

    /// 一个章内矩形是否**完全**落在这一行的裁剪框之外。部分相交的行留给绘制层的
    /// content mask 逐字形裁，命中与选区也按同一口径保留源锚点。
    pub fn rect_clipped(&self, line: usize, rect: ClipRect) -> bool {
        let Some(clip) = self.line_clip(line) else {
            return false;
        };
        rect.x1 <= clip.x0 || rect.x0 >= clip.x1 || rect.y1 <= clip.y0 || rect.y0 >= clip.y1
    }

    /// 这一行是否**完全**落在某个祖先的裁剪框之外。
    pub fn line_clipped(&self, line: usize) -> bool {
        let Some(row) = self.lines.get(line) else {
            return false;
        };
        let box_ = ClipRect::new(row.x, row.y, row.x + row.width, row.y + row.height);
        self.rect_clipped(line, box_)
    }
}
