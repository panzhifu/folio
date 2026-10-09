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
//! 分片器只接收「一栏这么高」这个数。两者由调用方对齐，见 `docs/known-limitations.md`。

use std::ops::Range;

use crate::css::computed::{BreakWhen, ComputedStyle, StyledDom};
use crate::dom::NodeId;

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
    /// 引擎还没有。见 `docs/known-limitations.md`「分片」一节。
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
mod tests {
    //! M4 / E4 分片：一页装几行、切点落在哪、作者的断点意图怎么兑现。
    //!
    //! 与 `layout::tests` 同一套办法：用「汉字 1 em、拉丁 0.5 em」的假度量，所以几何完全
    //! 确定 —— 16px 字号、200px 版心 ⇒ 一行 12 个汉字；行盒高 16 × 1.75 = 28px。
    //! 断言全部按这些数推出来，不是照实现的结果回填的。
    //!
    //! **两个容易记错的偏移**：UA 表给 `<p>` 上下各 1em 的外边距，所以第一行不在 y=0，
    //! 而在 **y=16**；相邻两段之间折叠后是 **16px** 的空档，不是行盒的 28px。也就是说
    //! 单段第 n 行在 `16 + 28n`，两段各一行时第二行的行顶在 `16 + 28 + 16 = 60`。
    //! 页高是从这一片的**行顶**起算的绝对额度，所以跨段的那一页会比纯段内少装一行 ——
    //! 按「两行 = 56px」去推多段用例的页高会推错。

    use crate::css::Origin;
    use crate::css::RuleSet;
    use crate::css::computed::{BreakWhen, ComputedStyle, StyledDom, cascade};
    use crate::dom::{Dom, NodeId};
    use crate::layout::LayoutOptions;
    use crate::layout::fragment::{
        Fragment, FragmentOptions, Fragmentainer, column_offset, column_width, fragments,
    };
    use crate::layout::{Layout, LineMetrics, Measure, NoImages, layout};

    struct Mono;

    fn char_width(ch: char, size: f32) -> f32 {
        if ch == ' ' {
            size * 0.25
        } else if (ch as u32) >= 0x2e80 {
            size
        } else {
            size * 0.5
        }
    }

    impl Measure for Mono {
        fn char_width(&self, ch: char, _font: &crate::layout::FontKey, font_size: f32) -> f32 {
            char_width(ch, font_size)
        }

        fn run_metrics(
            &self,
            text: &str,
            _font: &crate::layout::FontKey,
            font_size: f32,
        ) -> LineMetrics {
            LineMetrics {
                width: text.chars().map(|c| char_width(c, font_size)).sum(),
                ascent: font_size * 0.8,
                descent: font_size * 0.2,
            }
        }
    }

    /// 一行 12 个汉字 ⇒ n 个字占 `ceil(n / 12)` 行。
    fn cjk(chars: usize) -> String {
        "字".repeat(chars)
    }

    fn find(dom: &Dom, want: &str) -> NodeId {
        dom.elements()
            .find(|(id, _)| dom.tag_name(*id) == Some(want))
            .map(|(id, _)| id)
            .unwrap_or_else(|| panic!("文档里应有 <{want}>"))
    }

    /// 排版 + 分片，返回分片结果与行盒总数。
    fn paginated(html: &str, css: &str, width: f32, page: f32) -> (Layout, Vec<Fragment>) {
        split_columns(html, css, width, page, 1)
    }

    /// 同上，但指定一页几栏。
    fn split_columns(
        html: &str,
        css: &str,
        width: f32,
        page: f32,
        columns: usize,
    ) -> (Layout, Vec<Fragment>) {
        let styled = styled(html, css, width);
        let out = fragments(
            &styled.0,
            &styled.1,
            FragmentOptions {
                height: page,
                columns,
            },
        );
        (styled.0, out)
    }

    /// 排完版，连同样式表一起交出去（分片要读每个块的断点意图）。
    fn styled(html: &str, css: &str, width: f32) -> (Layout, StyledDom) {
        let dom = Dom::parse(html.as_bytes());
        let styles = cascade(
            &dom,
            &RuleSet::parse(css, Origin::Author),
            &ComputedStyle::default(),
        );
        let laid = layout(
            &dom,
            &styles,
            find(&dom, "body"),
            LayoutOptions { width },
            &Mono,
            &NoImages,
        );
        (laid, styles)
    }

    fn ranges(pages: &[Fragment]) -> Vec<(usize, usize)> {
        pages
            .iter()
            .map(|page| (page.lines.start, page.lines.end))
            .collect()
    }

    /// 每片的 `(页号, 栏号)`。多栏的断言全靠它 —— 「第几片」读者不关心，
    /// 「这一栏落在哪一页的第几格」才是界面要画对的东西。
    fn slots(pages: &[Fragment]) -> Vec<(usize, usize)> {
        pages.iter().map(|page| (page.page, page.column)).collect()
    }

    /// 只留有内容的片的行区间：补出来的空栏不该参与「切点有没有被挪动」这种比较。
    fn content_ranges(pages: &[Fragment]) -> Vec<(usize, usize)> {
        pages
            .iter()
            .filter(|page| !page.empty)
            .map(|page| (page.lines.start, page.lines.end))
            .collect()
    }

    /// 分片必须把每一行不重不漏地盖住 —— 这条比任何单点断言都重要，
    /// 漏一行就是「书里少了半句」，重一行就是读到重复段落。
    fn assert_tiles(pages: &[Fragment], total: usize) {
        let mut next = 0usize;
        for page in pages {
            if page.empty {
                continue;
            }
            assert_eq!(page.lines.start, next, "分片之间有洞：{pages:?}");
            assert!(page.lines.end > next, "出现了不空的空分片：{pages:?}");
            next = page.lines.end;
        }
        assert_eq!(next, total, "末尾还留着没分完的行");
    }

    #[test]
    fn continuous_mode_is_one_fragment_containing_everything() {
        let html = format!("<p>{}</p>", cjk(30));
        let (laid, pages) = paginated(&html, "", 200.0, 0.0);
        assert_eq!(laid.lines.len(), 3, "30 个字、200px 版心 → 3 行");
        assert_eq!(ranges(&pages), vec![(0, 3)], "连续滚动 = 单一片");
        assert!(
            !pages[0].forced && pages[0].height > 80.0,
            "那片该是整段的高度：{:?}",
            pages[0]
        );
    }

    #[test]
    fn a_page_holds_whole_lines_and_the_pages_tile_them() {
        let html = format!("<p>{}</p>", cjk(60));
        let (laid, pages) = paginated(&html, "", 200.0, 60.0);
        assert_eq!(laid.lines.len(), 5);
        // 行盒 28px，页 60px ⇒ 两行占 56px 放得下，第三行要 84px 放不下。
        assert_eq!(ranges(&pages), vec![(0, 2), (2, 4), (4, 5)]);
        assert_tiles(&pages, laid.lines.len());
        for page in &pages {
            assert!(
                page.height <= 60.0,
                "第 {:?} 片高 {} 超过页高",
                page.lines,
                page.height
            );
        }
    }

    #[test]
    fn break_after_page_ends_the_page_early_even_with_room_left() {
        let html = format!("<p class=stop>{}</p><p>{}</p>", cjk(10), cjk(30));
        let css = ".stop { break-after: page; }";
        let (laid, pages) = paginated(&html, css, 200.0, 60.0);
        assert_eq!(laid.lines.len(), 4, "1 行 + 3 行");
        // 不强制的话第一页会吃掉第二段的头一行；强制断在那里，第一页就只剩它自己。
        assert_eq!(ranges(&pages), vec![(0, 1), (1, 3), (3, 4)]);
        assert!(pages[0].forced, "第一页是被 break-after 结束的");
        assert!(!pages[1].forced, "第二页塞满两行，不是强制断的");
        assert_tiles(&pages, laid.lines.len());
    }

    #[test]
    fn break_before_page_pushes_a_block_to_the_next_page() {
        // 两段各一行：第一段行顶 16、第二段行顶 60。页高 76 ⇒ 不加声明时两行都放得下
        // （60 + 28 = 88 > 16 + 76 = 92？不，88 ≤ 92 ✓），加了才必须另起一页。
        // 先跑一遍不加声明的作为对照，否则「分成两页」可能只是页高不够，与 break-before 无关。
        let html = format!("<p>{}</p><p class=next>{}</p>", cjk(10), cjk(10));
        let (_, without) = paginated(&html, "", 200.0, 76.0);
        assert_eq!(
            ranges(&without),
            vec![(0, 2)],
            "对照：76px 的页装得下这两行"
        );

        let (laid, pages) = paginated(&html, ".next { break-before: page; }", 200.0, 76.0);
        assert_eq!(ranges(&pages), vec![(0, 1), (1, 2)]);
        assert!(pages[0].forced, "第一页是作者要求断的，不是塞满了才断");
        assert_tiles(&pages, laid.lines.len());
    }

    #[test]
    fn page_break_aliases_are_the_same_intent() {
        // CSS2.1 的写法在老 EPUB 里满地都是，认不得就等于把这些书的作者意图丢掉。
        let html = format!("<p class=stop>{}</p><p>{}</p>", cjk(10), cjk(10));
        let modern = paginated(html.as_str(), ".stop { break-after: page; }", 200.0, 60.0);
        let legacy = paginated(
            html.as_str(),
            ".stop { page-break-after: page; }",
            200.0,
            60.0,
        );
        assert_eq!(ranges(&modern.1), ranges(&legacy.1));
        assert!(legacy.1[0].forced, "`page-break-after` 也该标成强制断点");
    }

    #[test]
    fn break_inside_avoid_moves_the_whole_block_down() {
        let html = format!("<p>{}</p><p class=keep>{}</p>", cjk(10), cjk(24));
        let css = ".keep { break-inside: avoid; }";
        let (laid, pages) = paginated(&html, css, 200.0, 60.0);
        assert_eq!(laid.lines.len(), 3, "1 行 + 2 行");
        // 不保护的话第一页会拿走保护块的头一行，把它劈成 1 + 1；保护之后整块挪到下一页。
        assert_eq!(ranges(&pages), vec![(0, 1), (1, 3)]);
        assert_tiles(&pages, laid.lines.len());
    }

    #[test]
    fn avoid_gives_up_when_the_block_is_taller_than_a_page() {
        let html = format!("<p class=keep>{}</p>", cjk(48));
        let css = ".keep { break-inside: avoid; }";
        let (laid, pages) = paginated(&html, css, 200.0, 60.0);
        assert_eq!(laid.lines.len(), 4);
        // 一段四行、一页两行：想守也守不住。这里要的是「照样排出来」，
        // 而不是一片空的分片或永远走不完。
        assert_eq!(ranges(&pages), vec![(0, 2), (2, 4)]);
        assert!(
            pages.iter().all(|page| !page.empty),
            "没有奇偶补位的需求时不该出现空片：{pages:?}"
        );
    }

    #[test]
    fn orphans_and_widows_conflict_moves_the_whole_paragraph() {
        // 一段三行、一页三行：切点会落在段内第 3 行后，留下 1 行寡行。
        // 把它挪走又会只剩 1 行孤行 —— 两个约束同时不成立时整段下沉，
        // 因为第二页装得下整段。
        let html = format!("<p>{}</p><p class=tall>{}</p>", cjk(10), cjk(30));
        let (laid, pages) = paginated(&html, "", 200.0, 84.0);
        assert_eq!(laid.lines.len(), 4, "1 行 + 3 行");
        assert_eq!(ranges(&pages), vec![(0, 1), (1, 4)]);
        assert_tiles(&pages, laid.lines.len());
    }

    #[test]
    fn a_single_tall_paragraph_keeps_orphans_and_accepts_a_widow() {
        // 只有这一段、开头就顶着页首：无处可挪。这时保 orphans、让下一页顶一行，
        // 是排版传统里的取舍（页底留一行比页顶留一行更难看），不是随手挑的。
        let html = format!("<p>{}</p>", cjk(30));
        let (laid, pages) = paginated(&html, "", 200.0, 60.0);
        assert_eq!(laid.lines.len(), 3);
        assert_eq!(ranges(&pages), vec![(0, 2), (2, 3)]);
    }

    #[test]
    fn widows_and_orphans_can_be_loosened_to_one() {
        // 同一份内容，`orphans: 1; widows: 1` 就是「随便切」：第一页该塞满三行。
        let html = format!("<p>{}</p>", cjk(40));
        let (laid, loose) = paginated(&html, "p { orphans: 1; widows: 1; }", 200.0, 84.0);
        assert_eq!(laid.lines.len(), 4);
        assert_eq!(ranges(&loose), vec![(0, 3), (3, 4)]);
        // 回到初值 2 / 2：第四段那行不能单独留在下一页，于是第一页少拿一行。
        let (_, strict) = paginated(&html, "", 200.0, 84.0);
        assert_eq!(ranges(&strict), vec![(0, 2), (2, 4)]);
    }

    #[test]
    fn break_before_right_inserts_a_blank_page_for_parity() {
        let html = format!("<p>{}</p><p class=r>{}</p>", cjk(10), cjk(10));
        let css = ".r { break-before: right; }";
        let (laid, pages) = paginated(&html, css, 200.0, 1000.0);
        assert_eq!(laid.lines.len(), 2);
        // 第 0 页是右页（recto）；`break-after` 把内容截在第 0 页，于是第 1 页是左页，
        // 而 `right` 要它当右页 ⇒ 中间补一张空片。
        assert_eq!(pages.len(), 3, "{pages:?}");
        assert!(pages[1].empty && pages[1].lines.end == pages[1].lines.start);
        assert_eq!((pages[2].lines.start, pages[2].lines.end), (1, 2));
        assert_tiles(&pages, laid.lines.len());
    }

    #[test]
    fn resuming_from_a_token_matches_the_full_run() {
        let html = format!("<p>{}</p>", cjk(120));
        let (laid, styles) = styled(&html, "", 200.0);
        let options = FragmentOptions {
            height: 60.0,
            columns: 1,
        };
        let all = fragments(&laid, &styles, options);
        assert_eq!(all.len(), 5, "10 行、每页 2 行");

        // 翻到第三页：前两次正常走，把游标存下来，再用它接上。
        let mut first = Fragmentainer::new(&laid, &styles, options);
        let mut seen = Vec::new();
        for _ in 0..2 {
            seen.push(first.next().expect("前两页该存在"));
        }
        let token = first.token();
        let rest: Vec<Fragment> = Fragmentainer::resumed(&laid, &styles, options, token).collect();
        assert_eq!(seen.len(), 2);
        assert_eq!(rest, all[2..], "从游标接着算必须和一次性算完逐片相等");
        assert_eq!(token.line, 4);
        assert_eq!(token.fragment, 2);
    }

    #[test]
    fn an_empty_document_yields_no_pages() {
        let (laid, styles) = styled("<p></p>", "", 200.0);
        assert!(laid.lines.is_empty(), "空段落不该产出行盒");
        assert!(
            fragments(
                &laid,
                &styles,
                FragmentOptions {
                    height: 60.0,
                    columns: 1
                }
            )
            .is_empty(),
            "没有行就没有片"
        );
    }

    /// 初值是 CSS 定的，不是我们挑的：2 / 2 意味着「默认就不许留孤行寡行」，
    /// 而 `break-*` 一律 `Auto` —— 一本书没写分片意图时，分片器只按高度切。
    #[test]
    fn the_initial_values_are_the_css_ones() {
        let base = ComputedStyle::default();
        assert_eq!((base.orphans, base.widows), (2, 2));
        assert_eq!(base.break_before, BreakWhen::Auto);
        assert_eq!(base.break_after, BreakWhen::Auto);
        assert!(!base.break_inside_avoid);
    }

    #[test]
    fn break_properties_follow_css_inheritance_rules() {
        let dom = Dom::parse(b"<div class=book><p>x</p></div>");
        let styles = cascade(
            &dom,
            &RuleSet::parse(
                ".book { orphans: 4; widows: 5; break-inside: avoid; }",
                Origin::Author,
            ),
            &ComputedStyle::default(),
        );
        let div = styles.get(find(&dom, "div")).expect("div 的样式");
        let p = styles.get(find(&dom, "p")).expect("p 的样式");
        assert!(div.break_inside_avoid);
        // orphans / widows 是 CSS 里的可继承属性：书里在 `<body>` 上写一次就够。
        assert_eq!((p.orphans, p.widows), (4, 5), "这两个该继承下来");
        // break-* 不继承。若继承了，父块上一个 `avoid` 会让每个子块都挪不动，
        // 整页就再也塞不进东西 —— 这是会排不出版的错法。
        assert!(!p.break_inside_avoid, "break-inside 不该继承");
    }

    /// 认不出的值改状态也计 `ignored`，而不是猜一个作者没写的意思。
    #[test]
    fn a_broken_value_is_counted_rather_than_guessed() {
        let dom = Dom::parse(b"<p>x</p>");
        let styles = cascade(
            &dom,
            &RuleSet::parse(
                "p { orphans: 0; break-inside: avoid-column; }",
                Origin::Author,
            ),
            &ComputedStyle::default(),
        );
        let p = styles.get(find(&dom, "p")).expect("p 的样式");
        assert_eq!(
            p.orphans, 2,
            "`orphans: 0` 意味着一段可以被切成剩下零行，是写错了，不该当成 1"
        );
        assert!(
            !p.break_inside_avoid,
            "`avoid-column` 是多栏的取值，本引擎还没接，不该被当成 `avoid`"
        );
        assert_eq!(
            styles.ignored, 2,
            "两条都要计入 ignored，否则没预料到的写法会被静默吞掉"
        );
    }

    // ---- 多栏：一栏一片，页是 `columns` 片 ----
    //
    // 下面每一条都用同一个基准：**先按 `columns: 1` 跑一遍**。栏这件事只该改变
    // 「第几页第几栏」的归组，不该改变任何一处切点；把它跟基准比对，就是在钉这一点。

    /// 十行的书、页高 60 ⇒ 每栏两行、共五栏。分栏前后各栏的行区间必须一模一样。
    fn ten_lines() -> String {
        format!("<p>{}</p>", cjk(120))
    }

    #[test]
    fn columns_do_not_move_a_single_cut_point() {
        let html = ten_lines();
        let (laid, one) = paginated(&html, "", 200.0, 60.0);
        let (_, two) = split_columns(&html, "", 200.0, 60.0, 2);
        let (_, three) = split_columns(&html, "", 200.0, 60.0, 3);
        assert_eq!(laid.lines.len(), 10, "120 个字、一行 12 个 → 10 行");
        assert_eq!(
            content_ranges(&one),
            vec![(0, 2), (2, 4), (4, 6), (6, 8), (8, 10)],
            "基准：每栏两行"
        );
        assert_eq!(
            content_ranges(&two),
            content_ranges(&one),
            "两栏不该挪动任何切点"
        );
        assert_eq!(content_ranges(&three), content_ranges(&one), "三栏同样不该");
        assert_eq!(two.iter().filter(|page| !page.empty).count(), 5);
    }

    #[test]
    fn a_page_carries_exactly_the_configured_number_of_columns() {
        let html = ten_lines();
        let (laid, pages) = split_columns(&html, "", 200.0, 60.0, 2);
        assert_eq!(
            ranges(&pages),
            vec![(0, 2), (2, 4), (4, 6), (6, 8), (8, 10)]
        );
        assert_eq!(
            slots(&pages),
            vec![(0, 0), (0, 1), (1, 0), (1, 1), (2, 0)],
            "第 0、1 页各两栏，末页只有一栏"
        );
        assert_tiles(&pages, laid.lines.len());
        // 末页不补满：补出来的空白栏会让读者翻到一页什么都没有。
        assert_eq!(pages.last().expect("有内容就该有末页").column, 0);
    }

    #[test]
    fn a_forced_break_pads_the_rest_of_its_page() {
        // 第一段一行 + `break-after: page`，第二段五行。页高 60 ⇒ 每栏两行，两栏一页。
        let html = format!("<p class=stop>{}</p><p>{}</p>", cjk(10), cjk(60));
        let (laid, pages) = split_columns(&html, ".stop { break-after: page; }", 200.0, 60.0, 2);
        assert_eq!(laid.lines.len(), 6, "1 行 + 5 行");
        // 强制断点只交出第一栏，本页剩下那栏补空片，内容从下一页第一栏继续。
        assert_eq!(
            ranges(&pages),
            vec![(0, 1), (1, 1), (1, 3), (3, 5), (5, 6)],
            "第二项是补出来的空栏"
        );
        assert_eq!(
            slots(&pages),
            vec![(0, 0), (0, 1), (1, 0), (1, 1), (2, 0)],
            "补齐之后每页恰好两栏"
        );
        assert!(pages[1].empty && pages[1].lines.end == pages[1].lines.start);
        assert_tiles(&pages, laid.lines.len());
        // 对照：同样内容不分栏时不需要补位，片数就是页数。
        let (_, flat) = paginated(&html, ".stop { break-after: page; }", 200.0, 60.0);
        assert_eq!(ranges(&flat), vec![(0, 1), (1, 3), (3, 5), (5, 6)]);
        assert!(flat.iter().all(|page| !page.empty), "单栏时不该出现空片");
    }

    #[test]
    fn parity_padding_fills_a_whole_page_when_there_are_columns() {
        // 两段各一行、页高够装两行，第二段要 `break-before: right`。
        // 单栏时补一张空片就换到右页；两栏时「补一张」只挪动一栏，那一页就同时是左页
        // 又是右页 —— 必须补满整页，内容才真的落在下一页。
        let html = format!("<p>{}</p><p class=r>{}</p>", cjk(10), cjk(10));
        let css = ".r { break-before: right; }";
        let (laid, pages) = split_columns(&html, css, 200.0, 1000.0, 2);
        assert_eq!(laid.lines.len(), 2);
        assert_eq!(
            slots(&pages),
            vec![(0, 0), (0, 1), (1, 0), (1, 1), (2, 0)],
            "第 0 页被强制断点截掉半页，第 1 页整页空白，内容落到第 2 页"
        );
        assert_eq!(pages.iter().filter(|page| page.empty).count(), 3);
        let last = pages.last().expect("末页");
        assert_eq!(last.page, 2, "偶数页号当右页");
        assert_eq!((last.lines.start, last.lines.end), (1, 2));
        assert_tiles(&pages, laid.lines.len());
        // 对照单栏：同一个声明只补一张。
        let (_, flat) = paginated(&html, css, 200.0, 1000.0);
        assert_eq!(flat.iter().filter(|page| page.empty).count(), 1);
    }

    #[test]
    fn continuous_mode_ignores_columns() {
        let html = ten_lines();
        let (_, pages) = split_columns(&html, "", 200.0, 0.0, 3);
        assert_eq!(ranges(&pages), vec![(0, 10)], "没有页就没有栏：整篇一片");
        assert_eq!(slots(&pages), vec![(0, 0)]);
    }

    #[test]
    fn columns_zero_is_the_same_as_one() {
        // `0` 在选项里是「没设」，不是除数。漏夹一次就是 panic。
        let html = ten_lines();
        let (_, none) = split_columns(&html, "", 200.0, 60.0, 0);
        let (_, one) = split_columns(&html, "", 200.0, 60.0, 1);
        assert_eq!(ranges(&none), ranges(&one));
        assert_eq!(slots(&none), slots(&one));
    }

    /// 补位状态存在游标里，而不是分片器的私有队列里 —— 这条就是那个决定的验证：
    /// 只走一片就存下 Token，续算必须仍然把剩下的空栏补出来。
    #[test]
    fn resuming_in_the_middle_of_padding_still_pads() {
        let html = format!("<p class=stop>{}</p><p>{}</p>", cjk(10), cjk(60));
        let (laid, styles) = styled(&html, ".stop { break-after: page; }", 200.0);
        let options = FragmentOptions {
            height: 60.0,
            columns: 2,
        };
        let all = fragments(&laid, &styles, options);
        let mut first = Fragmentainer::new(&laid, &styles, options);
        let head = first.next().expect("第一栏");
        let token = first.token();
        assert_eq!(
            token.pad_until,
            token.fragment + 1,
            "断点在这一页第一栏，还剩一栏要补：{token:?}"
        );
        let rest: Vec<Fragment> = Fragmentainer::resumed(&laid, &styles, options, token).collect();
        let mut seen = vec![head];
        seen.extend(rest);
        assert_eq!(seen, all, "续算必须和一次性算完逐片相等");
        assert!(all[1].empty, "补的那一栏不能因为换了游标就丢掉");
    }

    #[test]
    fn column_width_and_offset_are_the_numbers_the_layout_needs() {
        // 640px 四栏、栏间 20px：三条间距共 60px，剩下 580 分四份。
        assert_eq!(column_width(640.0, 4, 20.0), 145.0);
        assert_eq!(column_width(640.0, 1, 20.0), 640.0, "一栏没有间距");
        assert_eq!(column_width(640.0, 0, 20.0), 640.0, "`0` 当一栏");
        // 窄到放不下间距时给 0，不给负数 —— 负宽传进折行会直接算不出行。
        assert_eq!(column_width(100.0, 4, 40.0), 0.0);
        assert_eq!(column_offset(145.0, 20.0, 0), 0.0);
        assert_eq!(
            column_offset(145.0, 20.0, 2),
            330.0,
            "第三栏 = 2 × (栏宽 + 间距)"
        );
        // 间距为负按 0 算：读者设置里不该出现负数，但算式不能因此把栏推到左边去。
        assert_eq!(column_offset(145.0, -50.0, 1), 145.0);
    }
}
