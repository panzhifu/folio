//! 断行：硬行方案、逐字符与逐词扫描、UAX #14 断点与禁则、行尾空白，产出 `LinePlan`。
//!
//! 从 `layout` 主模块切出来只为了一个文件能一次读完；跨文件的条目用
//! `pub(super)`，即**只对 `layout` 内部开放**，不属于对外公开面。

use std::ops::Range;
use std::sync::Arc;

use super::collect::{Collected, DEFAULT_STYLE, IMAGE_GLYPH, ImageAtom, Piece};
use super::font::{RunStyle, StyleArena, StyleRef};
use super::geometry::{ImageRun, LineFragment, LineRun};
use super::ports::{ImageProvider, Measure, image_size};
use crate::content::format::css::computed::{Hyphens, StyledDom};
use crate::content::dom::{Dom, NodeId, NodeKind};

/// 一行在版心里的落位。
///
/// 四个数各自的意义：`x` 是版心左缘，`y` 是这一行行顶（逐行推进的游标），
/// `width` 是版心宽，`indent` 是这一行让出去的左边距 —— 首行是 `text-indent`，
/// 被下沉首字符压着的行还要再加上首字符的宽。左缘与对齐偏移都按 `x + indent` 起算，
/// 可用宽度另外再扣一次 `reserved`：那一格不是空白，是首字符**自己站在那儿**，
/// 所以它推不开这一行的正文（正文排在它右边），却实实在在写不了字。
#[derive(Clone, Copy)]
pub(super) struct LinePlace {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) width: f32,
    pub(super) indent: f32,
    pub(super) reserved: f32,
}

/// 一个行盒的方案：正文的字节区间（首字符那一段已摘出去），加上落位要让开的两个数。
#[derive(Clone)]
pub(super) struct LinePlan {
    pub(super) range: Range<usize>,
    pub(super) indent: f32,
    pub(super) reserved: f32,
}

/// 下沉的首字符。`norm` 从段首起，含紧贴首字母的那些标点 —— CSS 的
/// `::first-letter` 盒就是这个范围（`「引号」里的字` 的引号也算进去）。
#[derive(Clone)]
pub(super) struct DropCap {
    pub(super) norm: Range<usize>,
    pub(super) run: RunStyle,
    /// 画它占多宽，也是被压着的那几行要让开的宽度。
    pub(super) width: f32,
    pub(super) ascent: f32,
    pub(super) descent: f32,
    /// 跨几行。按字形的实际上下 extent 除以块自己的行盒高，向上取整。
    pub(super) lines: usize,
}

/// 第 `index` 行要让开的左边距，和「首字符站着、正文写不进去」的那一段宽。
/// 两个数都要从可用宽里扣，但落位不同：前者把整行推开，后者那一格站着首字符本身。
fn line_inset(cap: &Option<DropCap>, index: usize, first_of_hard: bool, indent: f32) -> (f32, f32) {
    let mut left = if first_of_hard { indent } else { 0.0 };
    let mut reserved = 0.0f32;
    if let Some(dc) = cap {
        if index == 0 {
            reserved = dc.width;
        } else if index < dc.lines {
            left += dc.width;
        }
    }
    (left, reserved)
}

fn plan_for(
    range: Range<usize>,
    cap: &Option<DropCap>,
    index: usize,
    first_of_hard: bool,
    indent: f32,
) -> LinePlan {
    let (left, reserved) = line_inset(cap, index, first_of_hard, indent);
    // 首字符盒只属于第一行；正文从它之后开始。
    let range = match cap {
        Some(dc) if index == 0 && range.start < dc.norm.end => {
            dc.norm.end.min(range.end)..range.end
        }
        _ => range,
    };
    LinePlan {
        range,
        indent: left,
        reserved,
    }
}

/// 一行的累积器：`[start, end)` 区间内按片段顺序拼接文本与图片，同时求行盒几何。
///
/// `ascent` / `descent` 存的是**各片段自己那份算完半 leading 之后**到基线的上下距离，
/// 也就是 CSS 的画法：每个行内盒占 `[基线 − ascent − half-leading, 基线 + descent +
/// half-leading]`，行盒是它们的并集。所以行盒高 = `ascent + descent`，不再单独存一份，
/// 也不用「取各盒最大」再和字形高度比大小 —— 那两种写法在混排字号或图片时都会算错。
#[derive(Default)]
pub(super) struct LineAcc {
    pub(super) fragments: Vec<LineFragment>,
    pub(super) width: f32,
    pub(super) ascent: f32,
    pub(super) descent: f32,
}

impl LineAcc {
    /// 把 `seg`（不跨图片）与各文本 piece 求交，逐 piece 产出一个文本 run。
    pub(super) fn push_text(
        &mut self,
        measure: &dyn Measure,
        collected: &Collected,
        seg: Range<usize>,
    ) {
        for piece in &collected.pieces {
            let start = seg.start.max(piece.norm.start);
            let stop = seg.end.min(piece.norm.end);
            if start >= stop {
                continue;
            }
            let norm = start..stop;
            // 样式从 arena 取，每个 piece 只存了 4 字节下标。
            let style = collected
                .styles
                .get(piece.style)
                .expect("piece 的样式一定在 arena 里");
            // 绘制宽度直接取宽度表（与断行同源，且已含 `word-spacing`），不再每行重新 shaping。
            // 这是 §7.5：一行一个 run 就省一次 `run_metrics`，一章上万行。
            let width = sum_widths(collected, norm.clone());
            // 上下 extent 只跟字体/字号有关（`font_extents` 内部可按样式缓存），也不再 shaping。
            let (ascent, descent) = measure.font_extents(&style.font, style.font_size);
            // 半 leading 按**这个 piece 自己**的字号与 line-height 算：`(盒高 − 字形上下 extent) / 2`，
            // 可正可负。它同时决定这一行里基线离行顶/行底多远，所以松行距的字在行盒里居中、
            // 紧行距（作者写 125% 而字体自然盒 144% 那种）行盒就照作者缩，字形溢出到相邻行。
            // 基线偏移（`vertical-align`）再叠一次：往上抬就多吃上方、少占下方。
            let content = ascent + descent;
            let half_leading = (style.box_height() - content) / 2.0;
            let shift = style.baseline_shift;
            self.width += width;
            self.ascent = self.ascent.max(ascent + half_leading + shift);
            self.descent = self.descent.max(descent + half_leading - shift);
            self.fragments.push(LineFragment::Text(LineRun {
                node: piece.node,
                orig: orig_of(piece, &norm),
                norm,
                style: style.clone(),
                width,
                ascent,
                descent,
            }));
        }
    }

    /// 图片底贴基线：它的整高就是本行基线以上的距离（行内替换盒没有 leading 可言）。
    pub(super) fn push_image(&mut self, img: &ImageAtom) {
        self.width += img.width;
        self.ascent = self.ascent.max(img.height);
        self.fragments.push(LineFragment::Image(ImageRun {
            node: img.node,
            href: img.href.clone(),
            width: img.width,
            height: img.height,
        }));
    }
}

/// 宽度表里 `range` 这一段的总宽。续字节是 0，所以连续切片求和就够（且可向量化）。
fn sum_widths(collected: &Collected, range: Range<usize>) -> f32 {
    collected.widths.get(range).map_or(0.0, |s| s.iter().sum())
}

/// 硬行扫描收尾：把最后一段收成一行。空硬行（`<br><br>`）也要占一行高，否则会塌掉。
/// `from` 是这次扫描的起点（整行扫描时是硬行首，逐字符兜底时是被交回的断点）。
fn finish_hard_line(
    cap: &Option<DropCap>,
    from: usize,
    hard_end: usize,
    line_start: usize,
    first: &mut bool,
    indent: f32,
    plans: &mut Vec<LinePlan>,
) {
    if line_start < hard_end {
        plans.push(plan_for(
            line_start..hard_end,
            cap,
            plans.len(),
            *first,
            indent,
        ));
        *first = false;
    } else if from == hard_end {
        plans.push(plan_for(from..hard_end, cap, plans.len(), *first, indent));
        *first = false;
    }
}

/// 逐字符扫描硬行区间 `[from, hard_end)`，把折出的行方案追加进 `plans`。
///
/// 这是断行的基准实现：密集行（CJK）、带下沉盒的段落、以及词级扫描碰到超长词时的
/// 兜底都走它。`breaks` 是绝对字节位置的合法断点表；`cap_norm` 是下沉盒覆盖的字节范围。
/// 溢出时断在「最后一个不晚于当前位置的合法断点」，没有断点可选就退回硬切（`unwrap_or(at)`）。
#[allow(clippy::too_many_arguments)]
pub(super) fn scan_by_char(
    collected: &Collected,
    cap: &Option<DropCap>,
    cap_norm: &Option<Range<usize>>,
    breaks: &[usize],
    from: usize,
    hard_end: usize,
    width: f32,
    indent: f32,
    first: &mut bool,
    plans: &mut Vec<LinePlan>,
) {
    let mut line_start = from;
    let mut used = 0.0f32;
    // `line_inset` 只跟「第几行」有关，一行之内不变。提到循环外，不然每个字符都要
    // 重算一次缩进与保留宽 —— 那是热路上最贵的一个函数调用。首行让出去的宽度由
    // `avail` 扣，不再在 `used` 里加一遍（两处都算会让首行少容两个字）。
    let mut inset = line_inset(cap, plans.len(), *first, indent);
    let mut avail = (width - inset.0 - inset.1).max(1.0);
    let slice = collected.text.get(from..hard_end).unwrap_or("");
    for (i, _ch) in slice.char_indices() {
        let at = from + i;
        // 下沉的那一段不进这一行的累加宽度：它自己成一个片段摆在行首。
        if cap_norm.as_ref().is_some_and(|norm| norm.contains(&at)) {
            continue;
        }
        // 宽度表按字节下标存，图片哨兵也已经写进去了。
        let w = collected.width_at(at);
        if at > line_start && used + w > avail {
            let cut = last_break_before(breaks, at, line_start).unwrap_or(at);
            plans.push(plan_for(line_start..cut, cap, plans.len(), *first, indent));
            line_start = cut;
            used = sum_widths(collected, cut..at) + w;
            *first = false;
            // 换了行 → 重算这一行让出去的宽。
            inset = line_inset(cap, plans.len(), *first, indent);
            avail = (width - inset.0 - inset.1).max(1.0);
        } else {
            used += w;
        }
    }
    finish_hard_line(cap, from, hard_end, line_start, first, indent, plans);
}

/// 按「词」扫描一个硬行，把行方案追加进 `plans`。
///
/// 词 = 两个断点之间的连续段（拉丁文≈单词，CJK 退化成单字）；词宽用宽度表的连续
/// 切片求和，每个词只做一次溢出判断，把「逐字符判溢出」降成「逐词判溢出」。
///
/// 溢出发生在某个词内部时，逐字符实现里 `last_break_before` 返回的正是**该词词首**
/// （词首就是一个断点），所以在词首断行与逐字符结果一致。唯一例外是「行首超长词」
/// （单个词就宽过整行）：这时返回 `Some(位置)`，交给 [`scan_by_char`] 从该位置继续
/// 硬切（复现「排不下就硬切」的兜底）；正常走完返回 `None`。
#[allow(clippy::too_many_arguments)]
pub(super) fn scan_by_word(
    collected: &Collected,
    cap: &Option<DropCap>,
    breaks: &[usize],
    hard: Range<usize>,
    width: f32,
    indent: f32,
    first: &mut bool,
    plans: &mut Vec<LinePlan>,
) -> Option<usize> {
    // 虚拟边界表：0 → 硬行首，1..=breaks.len() → 各断点，末项 → 硬行尾。
    let boundary = |j: usize| -> usize {
        if j == 0 {
            hard.start
        } else if j - 1 < breaks.len() {
            breaks[j - 1]
        } else {
            hard.end
        }
    };
    let atoms = breaks.len() + 1;
    let mut line_start = hard.start;
    let mut used = 0.0f32;
    let mut inset = line_inset(cap, plans.len(), *first, indent);
    let mut avail = (width - inset.0 - inset.1).max(1.0);
    let mut j = 0;
    while j < atoms {
        let s = boundary(j);
        let e = boundary(j + 1);
        let w = sum_widths(collected, s..e);
        if s > line_start && used + w > avail {
            // 整个词放不下且本行已有内容 → 在词首断行。
            plans.push(plan_for(line_start..s, cap, plans.len(), *first, indent));
            line_start = s;
            used = 0.0;
            *first = false;
            inset = line_inset(cap, plans.len(), *first, indent);
            avail = (width - inset.0 - inset.1).max(1.0);
            // 不推进 `j`：让同一个词在新行上重新评估。
            continue;
        }
        if s == line_start && used + w > avail {
            // 行首的一个词就宽过整行 → 交回逐字符硬切。
            return Some(line_start);
        }
        used += w;
        j += 1;
    }
    finish_hard_line(cap, hard.start, hard.end, line_start, first, indent, plans);
    None
}

/// 最近的、不晚于 `at` 且严格晚于 `line_start` 的合法断点。
fn last_break_before(breaks: &[usize], at: usize, line_start: usize) -> Option<usize> {
    let idx = breaks.partition_point(|b| *b <= at);
    for i in (0..idx).rev() {
        if breaks[i] > line_start {
            return Some(breaks[i]);
        }
    }
    None
}

/// `hyphens: none` 时，词内的连字符不再是断点 —— `je-ne-sais-quoi` 得整个留着。
///
/// 判据是「这个连字符两侧都挨着拉丁字母或数字」。只拦连字符类（`-`、U+2010、U+2011、
/// 软连字符 U+00AD），中文破折号 `—` 不拦：它是词分隔符，拦掉等于把整段中文的折行
/// 能力砍一半，那比连字符这件事大得多。行尾的 `word-`（后面是空格）也不拦，
/// 那本来就是正常的词边界。
pub(super) fn hyphen_break_banned(text: &str, at: usize, hyphens: Hyphens) -> bool {
    if hyphens != Hyphens::None {
        return false;
    }
    let (Some(before), Some(after)) = (text.get(..at), text.get(at..)) else {
        return false;
    };
    let mut rev = before.chars().rev();
    let (Some(hy), Some(letter_before)) = (rev.next(), rev.next()) else {
        return false;
    };
    let Some(letter_after) = after.chars().next() else {
        return false;
    };
    is_hyphen_char(hy)
        && letter_before.is_ascii_alphanumeric()
        && letter_after.is_ascii_alphanumeric()
}

fn is_hyphen_char(c: char) -> bool {
    matches!(c, '-' | '\u{2010}' | '\u{2011}' | '\u{00AD}')
}

/// 行尾空白不计宽也不绘制（CSS 规定）：从 `end` 往回吞掉空格。
pub(super) fn trim_end_spaces(text: &str, start: usize, end: usize) -> usize {
    let Some(slice) = text.get(start..end) else {
        return end;
    };
    let trimmed = slice.trim_end_matches([' ', '\t']);
    end - (slice.len() - trimmed.len())
}

/// piece 的 norm 与 orig 等长时（绝大多数）映射是仿射的，逐字符精确；
/// 折叠掉内部空白后两者不等长，这时退化成整段 piece 的跨度 ——
/// 精确还原留给 M5 的锚点重定位，那里本来就要按节点重跑一次同样的折叠。
pub(super) fn orig_of(piece: &Piece, norm: &Range<usize>) -> Range<usize> {
    if piece.norm.len() == piece.orig.len() {
        let base = piece.orig.start + (norm.start - piece.norm.start);
        base..base + (norm.end - norm.start)
    } else {
        piece.orig.clone()
    }
}

/// `\n` 是唯一在 `white-space: normal` 下也生效的硬换行，来源只有 `<br>` 与 `pre`。
pub(super) fn hard_lines(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, _) in text.match_indices('\n') {
        out.push(start..i);
        start = i + 1;
    }
    out.push(start..text.len());
    out
}

/// 把若干兄弟子树展平成一段文本，同时记下每个来源节点的区间与其元素样式。
/// `width` 是段落的可用宽度，用来折算图片的百分比宽度。
///
/// **宽度表在这一趟里一并算好**（见 [`Collector::push_widths`]）：原先先展平、
/// 再 `compute_widths` 走第二趟，两趟都要遍历全部字符；合起来之后只剩一趟。
pub(super) fn collect(
    pieces: &[NodeId],
    dom: &Dom,
    styles: &StyledDom,
    pre: bool,
    images: &dyn ImageProvider,
    measure: &dyn Measure,
    width: f32,
) -> Collected {
    let mut out = Collector {
        text: String::new(),
        widths: Vec::new(),
        pieces: Vec::new(),
        images: Vec::new(),
        arena: StyleArena::new(),
        pending_space: false,
        dom,
        styles,
        provider: images,
        measure,
        width,
    };
    for node in pieces {
        let style = out.intern_style(*node);
        out.push_node(*node, pre, style);
    }
    Collected {
        text: out.text,
        pieces: out.pieces,
        images: out.images,
        styles: out.arena,
        widths: out.widths,
    }
}

struct Collector<'a> {
    text: String,
    /// 与 `text` **按字节同步增长**的每字符前进量表（非字符起点的字节是 0）。
    widths: Vec<f32>,
    pieces: Vec<Piece>,
    images: Vec<ImageAtom>,
    /// 这一段用到的样式的去重表。
    arena: StyleArena,
    /// 折叠空白是**跨节点**的：上一段尾巴上的空白与下一段开头的空白是同一次折叠，
    /// 谁先落地谁占位，所以这个状态挂在收集器上。
    pending_space: bool,
    dom: &'a Dom,
    styles: &'a StyledDom,
    provider: &'a dyn ImageProvider,
    measure: &'a dyn Measure,
    /// 段落可用宽度，图片百分比宽度的基准。
    width: f32,
}

impl<'a> Collector<'a> {
    /// 为一个节点算样式并进表，返回它的下标。
    fn intern_style(&mut self, node: NodeId) -> StyleRef {
        // 不克隆 `ComputedStyle`：`RunStyle::new` 只读它。
        let computed = self.styles.get(node).unwrap_or(&DEFAULT_STYLE);
        self.arena.intern(RunStyle::new(computed))
    }

    /// 为新追加的 `text[start..]` 算每字符前进量，推进 `widths`。
    ///
    /// 与文本同源：`text` 与 `widths` 按字节同步增长，所以 `widths[at]` 就是
    /// `text` 里字节 `at` 处那个字符的前进量。
    fn push_widths(&mut self, start: usize, style: StyleRef) {
        let Some(run) = self.arena.get(style) else {
            return;
        };
        let font = Arc::clone(&run.font);
        let font_size = run.font_size;
        let spacing = run.word_spacing;
        // 借用到此结束，下面才能可变借 `self.widths`。
        let base = self.widths.len();
        let adv = self
            .measure
            .char_widths(&self.text[start..], &font, font_size);
        // 一次 `resize` 把这段扩到目标字节长（多字节字符的后续字节补 0），
        // 再按字符起点写进去。**不能逐字节 `push`**：CJK 一字符三字节，
        // push 三次（其中两次是 0）比一次 memset 填满慢得多（实测 +12%）。
        self.widths.resize(base + (self.text.len() - start), 0.0);
        for (i, (offset, ch)) in self.text[start..].char_indices().enumerate() {
            let mut w = adv.get(i).copied().unwrap_or(0.0);
            if ch == ' ' {
                w += spacing;
            }
            self.widths[base + offset] = w;
        }
    }

    /// 按下标取样式。
    #[allow(dead_code)] // 断行已改走 `Collected::widths`，这里留给以后的按节点查样式
    fn style_ref(&self, id: StyleRef) -> &RunStyle {
        self.arena.get(id).expect("intern 过的样式")
    }

    fn push_node(&mut self, node: NodeId, pre: bool, style: StyleRef) {
        let dom_node = self.dom.get(node);
        if dom_node.kind == NodeKind::Text {
            let src = dom_node.text.as_str();
            let norm_start = self.text.len();
            let orig = if pre {
                self.text.push_str(src);
                0..src.len()
            } else {
                self.push_collapsed(src)
            };
            if self.text.len() > norm_start {
                self.push_widths(norm_start, style);
                self.pieces.push(Piece {
                    node,
                    norm: norm_start..self.text.len(),
                    orig,
                    style,
                });
            }
            return;
        }
        if self.dom.is_element(node, "br") {
            self.text.push('\n');
            // `\n` 一个字节，占位 0；断行只扫 hard line 内部，这个宽不会被读到。
            self.widths.push(0.0);
            self.pending_space = false;
            return;
        }
        if self.dom.is_element(node, "img") {
            self.push_image(node);
            return;
        }
        // 文本继承的是**它所在元素**的样式，所以往下走要换成元素自己的样式。
        let own = self.intern_style(node);
        for child in dom_node.children.clone() {
            self.push_node(child, pre, own);
        }
    }

    /// 行内图片：命中资源就写一个哨兵占位并记下尺寸；命中不到就整张跳过
    /// （不产出任何字符，也不会把段落撑乱）。
    fn push_image(&mut self, node: NodeId) {
        let Some(src) = self.dom.attr(node, "src").map(str::to_string) else {
            return;
        };
        let Some(img) = self.provider.resolve(&src) else {
            return;
        };
        let style = self.styles.get(node).unwrap_or(&DEFAULT_STYLE);
        let (w, h) = image_size(style, img.width, img.height, self.width);
        self.flush_space();
        let at = self.text.len();
        self.text.push(IMAGE_GLYPH);
        self.widths.push(w);
        for _ in 1..IMAGE_GLYPH.len_utf8() {
            self.widths.push(0.0);
        }
        self.images.push(ImageAtom {
            node,
            href: img.href,
            width: w,
            height: h,
            at,
        });
        self.pending_space = false;
    }

    /// CSS 空白折叠：连续空白并成一个空格，段首与段尾的空白吞掉。
    /// 返回真正落进 `text` 的那段原文的字节跨度。
    fn push_collapsed(&mut self, src: &str) -> Range<usize> {
        let bytes = src.as_bytes();
        let Some(first) = bytes.iter().position(|b| !b.is_ascii_whitespace()) else {
            // 整段是空白：只留下「这里有过空白」的事实。
            self.pending_space = true;
            return 0..0;
        };
        let last = bytes
            .iter()
            .rposition(|b| !b.is_ascii_whitespace())
            .unwrap_or(first);
        // 原文跨度把开头的空白也算进来：那个空格确实来自本节点。
        let orig_start = if bytes[0].is_ascii_whitespace() {
            0
        } else {
            first
        };
        self.pending_space = self.pending_space || first > 0;
        for ch in src[first..last + 1].chars() {
            // 可折叠空白只有 **ASCII** 那一套（CSS 的 document white space）。
            // U+3000（全角空格）是**内容**：EasyPub 一类工具拿它做段首缩进，
            // 浏览器原样渲染 —— `char::is_whitespace()` 会把它当空白吞掉，
            // 整本书的段首缩进就没了（真机实测《斗战狂潮》）。
            if ch.is_ascii_whitespace() {
                self.pending_space = true;
                continue;
            }
            self.flush_space();
            self.text.push(ch);
        }
        // 尾巴上的空白不立刻落地，交给下一个内容去决定要不要它。
        self.pending_space =
            self.pending_space || bytes[last + 1..].iter().any(|b| b.is_ascii_whitespace());
        orig_start..last + 1
    }

    fn flush_space(&mut self) {
        if !self.pending_space {
            return;
        }
        self.pending_space = false;
        if !self.text.is_empty() && !self.text.ends_with([' ', '\n']) {
            self.text.push(' ');
        }
    }
}
