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
//!
//! ## 多栏：一栏就是一**片**
//!
//! [`FragmentOptions::columns`] 大于 1 时，一片代表**一栏**，一页是连续的 `columns` 片。
//! 于是分栏没有第二套代码：栏只是「片的编号到页号的除法」，①②③④ 四条切点规则一条都不用改
//! —— 它们本来就只关心「这一栏装到哪儿」。
//!
//! 两条由此而来的不变量，界面可以依赖：
//! - **文档中间的每一页恰好 `columns` 片**。作者的强制断点意味着「下一页重新开始」，
//!   所以本页剩下的栏由 [`BreakToken::pad_until`] 补成空片；`Fragment::page` /
//!   `Fragment::column` 直接给出位置，不必自己数。
//! - **最后一片可能不满一页**：内容尽了就直接结束。补满只会让读者翻到一页空白。
//!
//! 栏**宽**不在这里算 —— 折行发生在排版期，所以 [`column_width`] 给排版方用，
//! 分片器只接收「一栏这么高」这个数。两者由调用方对齐，见 `plans/facts.md`。

use std::ops::Range;

use crate::content::format::css::computed::{BreakWhen, ComputedStyle, StyledDom};
use crate::content::dom::NodeId;

use super::Layout;

/// 分片参数。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FragmentOptions {
    /// 一片可用的内容高（页高减上下留白，或**一栏**的高）。
    ///
    /// **`0` 是「不分片」的哨兵值**：连续滚动就是单一无限分片，三种模式同源的落点就在这儿。
    /// 别把它当成「每页 0 高」—— 那样每一行都要独占一片、每片又装不下，就是死循环。
    pub height: f32,
    /// 一页几栏。`0` 与 `1` 同义（不分栏），所以 [`Fragmentainer`] 里一律读
    /// [`Fragmentainer::columns`] 拿夹过的值。
    ///
    /// 这是**视口参数**，不是作者的 `column-count`：一栏的宽度决定折行，得在排版期就用，
    /// 而作者的 per-block 分栏（同一本书里某几段分栏、其余不分）需要块级的多栏布局，
    /// 引擎还没有。见 `plans/facts.md` 与 `css/computed.rs` 的忽略清单。
    pub columns: usize,
}

impl FragmentOptions {
    /// 连续滚动：一整篇就是一页，分几栏不起作用。
    pub const CONTINUOUS: Self = Self {
        height: 0.0,
        columns: 1,
    };
}

/// 一栏的内容宽度：`总宽 − 栏间距之和`，再除以栏数。下限 0，不返回负数。
///
/// 归排版期用：折行发生在**栏宽**上，不是页宽上。分片器只管「一栏装几行」，
/// 拿到 `columns` 时它假设送进来的 `Layout` 已经是按这个宽度排好的 ——
/// 两者由调用方对齐，这就是为什么栏宽算式放在核心而不是界面里。
pub fn column_width(total: f32, columns: usize, gap: f32) -> f32 {
    let n = columns.max(1) as f32;
    ((total - gap.max(0.0) * (n - 1.0)) / n).max(0.0)
}

/// 第 `column` 栏的左边界偏移。`gap` 由调用方给（界面才知道栏间距画多少）。
pub fn column_offset(width: f32, gap: f32, column: usize) -> f32 {
    column as f32 * (width + gap.max(0.0))
}

/// 一片的内容。绘制方拿到的信息足够「把这段行整体上屏」：行下标区间决定画哪些行，
/// [`Fragment::origin_y`] 决定往上移多少，[`Fragment::column`] 决定往右移多少。
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
    /// 补出来的空片：只为填满一页或对上左右页的奇偶，本身没有内容。
    pub empty: bool,
    /// 本页是第几页（0 基）。左右页奇偶看它，`columns == 1` 时它等于已产出的片数。
    pub page: usize,
    /// 本片在所属页内的栏号（0 基）。`columns == 1` 时恒为 0。
    pub column: usize,
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
    /// 已经产出了几**片**（= 几栏，含补出来的空栏）。页号与栏号都是它的除法结果，
    /// 所以这里不再单独存页号 —— 两个数会互相说谎。
    pub fragment: usize,
    /// 内容必须等到第几个片槽才开始。`fragment < pad_until` 时补空栏。
    ///
    /// 为什么进游标而不是进分片器的私有队列：强制断点会**跳过**本页剩下的栏，
    /// 而如果这份状态不在 `BreakToken` 里，从中间某一页续算时就会丢掉那些空栏，
    /// 于是续算结果和一次性算完不再逐片相等（这正是 `resumed` 唯一要保证的事）。
    pub pad_until: usize,
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
            token: BreakToken {
                line: 0,
                fragment: 0,
                pad_until: 0,
            },
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
    /// 也叫 [`Iterator::next`]（下面有 `impl Iterator`），所以 `.count()` 就是**总片数**
    /// （= 总栏数），`.nth(k)` 就是第 k 片 —— 跳到某一页不必先把整本书分完。
    /// 一页几栏时页号是 `k / columns`，见 [`Fragment::page`]。
    pub fn next_fragment(&mut self) -> Option<Fragment> {
        let total = self.layout.lines.len();
        if self.done || self.token.line >= total {
            self.done = true;
            return None;
        }
        let start = self.token.line;
        let columns = self.columns();

        // 补空栏。空栏**不推进行游标**，所以补完之后下一张内容片仍从 `start` 开始；
        // 一次调用只发一张，是为了让 `Fragmentainer` 保持迭代器形状（`nth(k)` 才仍然对得上）。
        if self.token.fragment < self.token.pad_until {
            return Some(self.blank());
        }

        // 连续滚动：一片装完剩下的全部。三种模式同源指的就是走这条分支，
        // 而不是另写一套「不分片」的代码。`columns` 在这里不起作用：没有页，
        // 就没有「一页几栏」。
        if self.options.height <= 0.0 {
            return Some(self.emit(start, total, false));
        }

        // 左右页的奇偶：0 基页号里偶数当右页（recto）、奇数当左页。对不上就把**当前页补满**，
        // 下一页的奇偶一定翻转 —— 栏数大于 1 时「补一张」不够，必须补到页边界，
        // 否则内容会落在同一页的空栏里，那一页就同时是左页又是右页。
        if let Some(want) = self.force_before[start].and_then(BreakWhen::parity) {
            let page = self.token.fragment / columns;
            if want != page.is_multiple_of(2) {
                self.token.pad_until = (page + 1) * columns;
                return Some(self.blank());
            }
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

        let fragment = self.emit(start, end + 1, forced);
        // 强制断点的意思是「下一页重新开始」，所以本页剩下的栏要补空片填满。
        // 这样「文档中间每页恰好 `columns` 片」始终成立，界面按 `片号 / columns` 分组
        // 就不会把两页的内容串成一页。末尾那一页不补：内容尽了就走上面 `done` 那条早退，
        // 补出来的空白栏只会让读者翻到一页什么都没有。
        if forced {
            let next_page_start = (fragment.page + 1) * columns;
            self.token.pad_until = self.token.pad_until.max(next_page_start);
        }
        Some(fragment)
    }

    /// 一页几栏，`0` 当 `1` 用。**所有**除法与取模都走这里 —— 忘了夹一次就是除零 panic。
    fn columns(&self) -> usize {
        self.options.columns.max(1)
    }

    /// 补一张空片：占一个片槽，不推进行游标。
    fn blank(&mut self) -> Fragment {
        let columns = self.columns();
        let slot = self.token.fragment;
        self.token.fragment = slot + 1;
        Fragment {
            lines: self.token.line..self.token.line,
            origin_y: self.layout.lines[self.token.line].y,
            height: 0.0,
            forced: true,
            empty: true,
            page: slot / columns,
            column: slot % columns,
        }
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
        let columns = self.columns();
        let slot = self.token.fragment;
        let origin_y = self.layout.lines[start].y;
        let height = if end > start {
            let last = &self.layout.lines[end - 1];
            (last.y + last.height) - origin_y
        } else {
            0.0
        };
        self.token.fragment = slot + 1;
        self.token.line = end;
        if end >= self.layout.lines.len() {
            self.done = true;
        }
        Fragment {
            lines: start..end,
            origin_y,
            height,
            forced,
            empty: false,
            page: slot / columns,
            column: slot % columns,
        }
    }
}

/// 分片器是个迭代器：`.count()` 就是总片数（一栏一片），`.nth(k)` 就是第 k 片，
/// `.take(3)` 就是前三片。每一片只在需要时才算，所以千页的书翻到第 500 页
/// 不必先把前面 499 页都分一遍。
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

#[cfg(test)]
mod tests;
