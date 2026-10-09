//! 分片（Fragmentation）：把一份已经排好的 `Layout` 按可用高度切成一片一片。
//!
//! 这是「连续滚动 / 分页 / 双页对开」三种模式共用的一套代码，而不是三份 —— 区别只有
//! [`FragmentOptions::height`] 这一个数：连续滚动是「高度无限」的那一档（一片装完全部），
//! 分页是一个视口高，双页对开是一个跨页高并额外要求左右页的奇偶对得上。开发方案 §4.5
//! 把这件事列为引擎的第四个硬骨头，原因就在这儿：先有分片器，模式才是参数。
//!
//! ## 它是布局之后的纯后处理
//!
//! 分片只读 [`Layout`] 与计算样式表，不重新折行、不调度量。好处是三件事：改页高不重排
//! （动的是切片，不是排版）；同一份 `Layout` 可以同时喂给连续滚动和分页两种 UI；
//! 以及这一整块逻辑是纯函数，能在 `folio-core` 里用假度量把分片点逐行钉死。
//!
//! 代价是要在这里重读一遍每个块的断点意图，所以 [`Fragmentainer::new`] 先把四张表算出来
//! （行 → 叶子块、行 → 强制断点、不许切开的行区间）。分页是逐页进行的，这些表必须只算一次：
//! 每页重扫整棵块树会让翻到第 500 页比第 1 页慢 500 倍。
//!
//! ## 一片的高度怎么算
//!
//! `Layout` 里每行的 `y` 是**绝对坐标**，块与块之间的 margin、padding 已经体现在行的间隔里。
//! 所以一片的高取「末行的底 − 首行的顶」，行间所有留白自动算进去，不会重复计一遍。
//! 由此有一个必须说清的取舍：**断点处的上下 margin 会被丢掉** —— 第 N 页末行下面那段空白
//! 不会出现在第 N+1 页开头。CSS 的规定是两侧都保留，但那会让每页顶部多出一段随书而异的空白，
//! 在阅读器里看起来像「排版不稳」。这里选稳定，并且是有意选的。

use std::ops::Range;

use crate::css::computed::{BreakWhen, ComputedStyle, StyledDom};
use crate::dom::NodeId;

use super::Layout;

/// 分片参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FragmentOptions {
    /// 一片可用的内容高（页高减上下留白，或一栏高）。
    ///
    /// **`0` 是「不分片」的哨兵值**：连续滚动就是单一无限分片，三种模式同源的落点就在这儿。
    /// 别把它当成「每页 0 高」—— 那样每一行都要独占一片、每片又装不下，就是死循环。
    pub height: f32,
}

impl FragmentOptions {
    /// 连续滚动：一整篇就是一页。
    pub const CONTINUOUS: Self = Self { height: 0.0 };
}

/// 一片的内容。绘制方拿到的信息足够「把这段行整体上屏」：行下标区间决定画哪些行，
/// [`Fragment::origin_y`] 决定往上移多少。
#[derive(Debug, Clone, PartialEq)]
pub struct Fragment {
    /// `Layout::lines` 的下标区间。补出来的空片是 `start == end`。
    pub lines: Range<usize>,
    /// 首行的绝对 y。片内第 `i` 行画在 `lines[i].y - origin_y`。
    pub origin_y: f32,
    /// 本片占用的内容高（首行顶到末行底）。
    pub height: f32,
    /// 这一片是被作者的强制断点结束的（`break-before/after: page|always|left|right`）。
    pub forced: bool,
    /// 补出来的空片：只为把左右页的奇偶对上，本身没有内容。
    pub empty: bool,
}

impl Fragment {
    /// 本片包含的行数。
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
}

/// 分片游标：下一片从哪里开始、这是第几片。
///
/// 名字沿用开发方案 §4.5 的 `BreakToken`。它是纯数据，所以「只算下一页」不必先把整本书
/// 分完：把游标存下来，下次从它接着算。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreakToken {
    /// 下一片起始的行下标。
    pub line: usize,
    /// 已经产出了几片（含补出来的空片）。左右页的奇偶就靠它。
    pub page: usize,
}

/// 分片器。持有一份 `Layout` 的只读引用与预建表，[`Fragmentainer::next`] 一次吐一片。
pub struct Fragmentainer<'a> {
    layout: &'a Layout,
    styles: &'a StyledDom,
    options: FragmentOptions,
    /// 行 → 拥有它的**叶子块**下标。块表是先序展平的，所以「后写覆盖先写」留下的
    /// 就是最深的块，与 `Layout::block_of_line` 同一条规则。orphans / widows 是段级的，
    /// 必须用它而不是用任意祖先块。
    leaf_of: Vec<usize>,
    /// 每个块的行区间，下标与 `leaf_of` 里的编号一致。
    blocks: Vec<Range<usize>>,
    /// 与 `blocks` 同下标的 DOM 节点：查回这个块的计算样式要用。
    nodes: Vec<NodeId>,
    /// 行 → 这一行上有块声明了强制 `break-before`。同一行多个块（父块与它的首个内容块
    /// 起点相同）取第一个非 `Auto`，因为切点是同一个位置。
    force_before: Vec<Option<BreakWhen>>,
    /// 行 → 这一行上有块声明了强制 `break-after`。
    force_after: Vec<Option<BreakWhen>>,
    /// `break-inside: avoid` 的块行区间，按长度从短到长排。
    avoid: Vec<Range<usize>>,
    token: BreakToken,
    done: bool,
}

impl<'a> Fragmentainer<'a> {
    pub fn new(layout: &'a Layout, styles: &'a StyledDom, options: FragmentOptions) -> Self {
        let total = layout.lines.len();
        let mut leaf_of = vec![usize::MAX; total];
        let mut blocks: Vec<Range<usize>> = Vec::with_capacity(layout.blocks.len());
        let mut nodes: Vec<NodeId> = Vec::with_capacity(layout.blocks.len());
        let mut force_before: Vec<Option<BreakWhen>> = vec![None; total];
        let mut force_after: Vec<Option<BreakWhen>> = vec![None; total];
        let mut avoid: Vec<Range<usize>> = Vec::new();

        for block in &layout.blocks {
            let range = block.lines.clone();
            let index = blocks.len();
            blocks.push(range.clone());
            nodes.push(block.node);
            // 先序展平 ⇒ 越深的块越靠后写，最终每行留下最深的叶子块。
            for line in range.clone() {
                if line < total {
                    leaf_of[line] = index;
                }
            }
            if range.is_empty() {
                continue;
            }
            let first = range.start;
            let last = range.end - 1;
            let Some(style) = styles.get(block.node) else {
                continue;
            };
            if style.break_before.is_forced() && first < total {
                force_before[first].get_or_insert(style.break_before);
            }
            if style.break_after.is_forced() && last < total {
                force_after[last].get_or_insert(style.break_after);
            }
            // 只有一行的块没有「切开」可言，跳过以免白占一次挪动。
            if style.break_inside_avoid && range.len() > 1 {
                avoid.push(range);
            }
        }
        // 短的排前面：切点同时被多个 avoid 区间跨过时（嵌套块），先挪最小的那个 ——
        // 挪大块的代价是留出一大片空白。
        avoid.sort_by_key(|range| range.len());

        Self {
            layout,
            styles,
            options,
            leaf_of,
            blocks,
            nodes,
            force_before,
            force_after,
            avoid,
            token: BreakToken { line: 0, page: 0 },
            done: false,
        }
    }

    /// 从某个游标接着分。表仍然要重算一遍（它们是从 `Layout` 推出来的），
    /// 但排版结果不用：`Layout` 是外面的缓存，翻到第 500 页只是重跑一次预建表 + 500 次切页。
    pub fn resumed(
        layout: &'a Layout,
        styles: &'a StyledDom,
        options: FragmentOptions,
        token: BreakToken,
    ) -> Self {
        let mut this = Self::new(layout, styles, options);
        this.token = token;
        this.done = token.line >= layout.lines.len();
        this
    }

    /// 当前游标。存下来就能从这一页接着算。
    pub fn token(&self) -> BreakToken {
        self.token
    }

    /// 分完了没有。空文档直接为 `true`。
    pub fn done(&self) -> bool {
        self.done
    }

    /// 下一片；`None` 表示没有更多内容。
    ///
    /// 也叫 [`Iterator::next`]（下面有 `impl Iterator`），所以 `.count()` 就是总页数、
    /// `.nth(k)` 就是第 k 页 —— 跳到某一页不必先把整本书分完。
    pub fn next_fragment(&mut self) -> Option<Fragment> {
        let total = self.layout.lines.len();
        if self.done || self.token.line >= total {
            self.done = true;
            return None;
        }
        let start = self.token.line;

        // 连续滚动：一片装完剩下的全部。三种模式同源指的就是走这条分支，
        // 而不是另写一套「不分片」的代码。
        if self.options.height <= 0.0 {
            return Some(self.emit(start, total, false));
        }

        // 左右页的奇偶：0 基页号里偶数当右页（recto）、奇数当左页。对不上就先补一张空片。
        // 空片不推进行游标，所以同一位置上最多只会补一张。
        let page_is_right = self.token.page.is_multiple_of(2);
        if let Some(want) = self.force_before[start].and_then(BreakWhen::parity)
            && want != page_is_right
        {
            self.token.page += 1;
            return Some(Fragment {
                lines: start..start,
                origin_y: self.layout.lines[start].y,
                height: 0.0,
                forced: true,
                empty: true,
            });
        }

        let origin_y = self.layout.lines[start].y;
        let limit = origin_y + self.options.height;

        // ① 能塞几行塞几行。首行**无条件**进来：一行比一页高是常事（大图、长表格），
        //    挑它就没得前进，前进不了就是死循环。
        let mut end = start;
        while end + 1 < total && self.fits(end + 1, limit) {
            end += 1;
        }

        // ② 作者的强制断点优先于「塞满」。在 ① 选出的范围里找最早的那个，切在它那儿。
        //    扫的是快照 `scan`，切点另存 `cut`：直接改循环上界会让边界在循环里动来动去。
        let mut forced = false;
        let scan = end;
        for line in start..=scan {
            if line > start && self.force_before[line].is_some() {
                end = line - 1;
                forced = true;
                break;
            }
            if self.force_after[line].is_some() {
                end = line;
                forced = true;
                break;
            }
        }

        if !forced {
            // ③ `break-inside: avoid`：切点跨过了某个不许切开的区间，就把切点挪到它**前面**。
            //    挪不动（本片除了它就没人）时保持原样强切 —— 一本书因为一张插图排不出版，
            //    比切开了作者的表格糟得多。
            if let Some(range) = self.straddling(end + 1)
                && range.start > start
            {
                end = range.start - 1;
            }
            // ④ orphans / widows 放在 ③ 之后：③ 可能已经把切点挪到别处，而那个新位置
            //    同样可能正切在某个段中间。顺序反过来会让两处规则互相看不见。
            self.keep_orphans_and_widows(&mut end, start);
        }

        Some(self.emit(start, end + 1, forced))
    }

    /// 第 `line` 行的行盒底是否还在本片的可用高度之内。
    ///
    /// `limit` 已经是「本片原点的绝对底边」，所以这里只比 `bottom`，**不再**减一次原点 ——
    /// 减了会让第二页之后的每一页都按「原点 + 页高 + 原点」放行，页高越往后越能塞，
    /// 表现是第三页比第一页多两行，而第一页看起来完全正常。
    fn fits(&self, line: usize, limit: f32) -> bool {
        let bottom = self.layout.lines[line].y + self.layout.lines[line].height;
        bottom <= limit
    }

    /// 跨过 `cut` 这个切点的最短 avoid 区间。
    fn straddling(&self, cut: usize) -> Option<&Range<usize>> {
        self.avoid
            .iter()
            .find(|range| range.start < cut && cut < range.end)
    }

    /// 把切点回退到满足 `orphans` / `widows` 的位置。
    ///
    /// 只在「这一刀确实切在某个段内部」时才动。两条规则可能互相冲突（一个三行的段落在
    /// 两页高的容器里就是如此）：这时**整段挪到下一片**，前提是挪了以后本片仍然非空；
    /// 段正好是本片开头时无可再挪，那就保住 `orphans` 而违反 `widows` —— 页面底部留一行
    /// 比下一页顶部留一行更难看，这是排版传统里的共识，不是随手挑的。
    ///
    /// 回退后的切点必须仍满足 `start < 切点 <= 原切点`，否则一片会变空、或者切点倒退，
    /// 分片就停不下来。
    fn keep_orphans_and_widows(&self, end: &mut usize, start: usize) {
        let cut = *end + 1;
        let Some(&leaf) = self.leaf_of.get(cut) else {
            return;
        };
        let Some(range) = self.blocks.get(leaf) else {
            return;
        };
        // 切点落在段边界上不关这两个规则的事。
        if !(range.start < cut && cut < range.end) {
            return;
        }
        let Some(style) = self.block_style(leaf) else {
            return;
        };
        let orphans = style.orphans as usize;
        let widows = style.widows as usize;
        // `orphans` 数的是**本片里这一段留了几行**。段若从上一页就开始了，那它的行号
        // 起点是本片的开头而不是块的开头 —— 按块开头数会把「续排过来的一行」当成满编，
        // 结果第 N 页只剩孤零零一行还算它满足 orphans: 2。
        let portion_start = range.start.max(start);
        let mut target = cut;
        if range.end.saturating_sub(target) < widows {
            target = range.end.saturating_sub(widows);
        }
        if target.saturating_sub(portion_start) < orphans {
            if range.start > start {
                target = range.start;
            } else {
                // 段顶到本片开头了：保 orphans，接受违反 widows。
                return;
            }
        }
        if target > start && target <= cut {
            *end = target - 1;
        }
    }

    fn block_style(&self, leaf: usize) -> Option<&ComputedStyle> {
        self.nodes.get(leaf).and_then(|node| self.styles.get(*node))
    }

    /// 产出一片并推进游标。
    fn emit(&mut self, start: usize, end: usize, forced: bool) -> Fragment {
        let origin_y = self.layout.lines[start].y;
        let height = if end > start {
            let last = &self.layout.lines[end - 1];
            (last.y + last.height) - origin_y
        } else {
            0.0
        };
        self.token.line = end;
        self.token.page += 1;
        if end >= self.layout.lines.len() {
            self.done = true;
        }
        Fragment {
            lines: start..end,
            origin_y,
            height,
            forced,
            empty: false,
        }
    }
}

/// 分片器是个迭代器：`.count()` 就是总页数，`.nth(k)` 就是第 k 页，`.take(3)` 就是前三页。
/// 每一页只在需要时才算，所以千页的书翻到第 500 页不必先把前面 499 页都分一遍。
impl Iterator for Fragmentainer<'_> {
    type Item = Fragment;

    fn next(&mut self) -> Option<Fragment> {
        self.next_fragment()
    }
}

/// 一次分完。进度条、导出、测试要用它；逐页渲染应当用 [`Fragmentainer`] 的游标，
/// 不要每页都从这里重头算。
pub fn fragments(layout: &Layout, styles: &StyledDom, options: FragmentOptions) -> Vec<Fragment> {
    Fragmentainer::new(layout, styles, options).collect()
}
