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
use crate::content::dom::{Dom, NodeId, NodeKind};
use crate::content::format::css::computed::{Hyphens, RubyPosition, StyledDom};

/// 注音与基字之间的缝，按容器自己的字号折算。
///
/// 这是**我们定的常数**，不是规范给的数：CSS 把 ruby 的注音间距留给实现，浏览器给的是
/// 一两个 px 级别的空隙。0.08 em 在 16 px 正文 + 半号注音那一档是 1.3 px —— 再小就会让
/// 注音的下沿咬到基字的上沿，再大就变成两行之间凭空多一条缝。列表标记与正文之间那道缝
/// （`collect.rs` 的 `MARKER_GAP_EM`）是同一个立场：留白由实现定，取「贴字」那一档。
const RUBY_GAP_EM: f32 = 0.08;

/// 一行在版心里的落位：行带左缘 `x`、行顶 `y`、整幅宽 `width`，加上这一行让出去的
/// 四段宽（[`RowInsets`]，四段各是什么、为什么要分成四段写在那一条上）。
///
/// 折行那一趟只决定「这一行从哪起、占多高的一档」与这四段，剩下的宽度与对齐都在
/// [`crate::layout::collect::Ctx::emit_line`] 里按它算 —— 两趟吃的是同一份 `RowInsets`，
/// 所以「按多少宽断的行」与「按多少宽摆的字」不可能对不上。
#[derive(Clone, Copy)]
pub(super) struct LinePlace {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) width: f32,
    pub(super) insets: RowInsets,
    /// 就是 [`LinePlan::hard_end`]：这一行以硬换行收尾（`<br>` 或段末），**不做**两端对齐。
    pub(super) hard_end: bool,
}

/// 一个行盒的方案：正文的字节区间（首字符那一段已摘出去），加上落位要让开的那几段。
#[derive(Clone)]
pub(super) struct LinePlan {
    pub(super) range: Range<usize>,
    /// 让宽的四段，摆位与折行共用同一份。
    pub(super) insets: RowInsets,
    /// 这一行以**硬换行**收尾（`<br>`，或它就是本段的最后一行）。CSS 规定这类行
    /// **不做**两端对齐 —— 段末行与强制断行处让余量留在右边，是排版惯例也是规范。
    pub(super) hard_end: bool,
}

/// 这一行让出去的四段宽。分四段是因为它们的**几何作用不同**，混成一个数就会在 RTL 里错：
///
/// - `start` 是逻辑起始边的位移（首行 `text-indent`、被下沉首字符压着的那几行让开的宽）。
///   RTL 时它落在右边，由对齐那一路从右缘往回算。
/// - `left` / `right` 是两侧浮动盒吃掉的宽，是**物理边**：左边的图在 RTL 里也站在左边，
///   所以它只能从行带的左缘扣，不能跟 `start` 加在一起。
/// - `reserved` 是首字符自己站的那一格：推不开正文（正文排在它右边），但写不了字。
#[derive(Clone, Copy, Default)]
pub(super) struct RowInsets {
    pub(super) start: f32,
    pub(super) left: f32,
    pub(super) right: f32,
    pub(super) reserved: f32,
}

impl RowInsets {
    /// 整幅宽 `width` 上这一行还剩多少可写。四个数都要扣，一个都不能忘：
    /// 忘了 `left` / `right` 就会把字写进图里，忘了 `reserved` 会把首字符盖住。
    pub(super) fn avail(&self, width: f32) -> f32 {
        (width - self.start - self.left - self.right - self.reserved).max(1.0)
    }
}

/// 第 `index` 行让出去的四段。浮动那两段查 [`Insets`]，另外两段来自下沉盒与缩进。
fn line_inset(
    cap: &Option<DropCap>,
    insets: &Insets<'_>,
    index: usize,
    first_of_hard: bool,
    indent: f32,
) -> RowInsets {
    let mut out = RowInsets {
        start: if first_of_hard { indent } else { 0.0 },
        ..RowInsets::default()
    };
    if let Some(dc) = cap {
        if index == 0 {
            out.reserved = dc.width;
        } else if index < dc.lines {
            out.start += dc.width;
        }
    }
    let (left, right) = insets.at(index);
    out.left = left;
    out.right = right;
    out
}

fn plan_for(
    range: Range<usize>,
    cap: &Option<DropCap>,
    insets: &Insets<'_>,
    index: usize,
    first_of_hard: bool,
    indent: f32,
) -> LinePlan {
    let row = line_inset(cap, insets, index, first_of_hard, indent);
    // 首字符盒只属于第一行；正文从它之后开始。
    let range = match cap {
        Some(dc) if index == 0 && range.start < dc.norm.end => {
            dc.norm.end.min(range.end)..range.end
        }
        _ => range,
    };
    LinePlan {
        range,
        insets: row,
        hard_end: false,
    }
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

/// 每一行要让开的 (左, 右) 两段宽，按**行号**索引。
///
/// `None` = 这一段旁边没有浮动盒，那是正文的常态：每一行都吃整幅宽，一次查表都不付。
/// 表的长度由浮动盒的下缘定，表外的行取 `(0, 0)` —— 浮动盒都在它上面结束了。
#[derive(Clone, Copy, Default)]
pub(super) struct Insets<'a>(pub(super) Option<&'a [(f32, f32)]>);

impl Insets<'_> {
    fn at(&self, index: usize) -> (f32, f32) {
        self.0
            .and_then(|bands| bands.get(index).copied())
            .unwrap_or((0.0, 0.0))
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
            // 上下 extent 按**这段字**量：样式声明的字体不等于这段字实际用的字体（汉字与
            // 假名走回退链里另一张脸），所以这里必须把 `text` 递下去 —— 只按 `(字体, 字号)`
            // 缓存会让混排行盒按拉丁字体的度量折，见 [`Measure::font_extents`]。
            // 一次 shaping 都不额外付：实现方按（字体, 字号, 文字类别）缓存，正文仍不走逐行 shaping。
            let (ascent, descent) =
                measure.font_extents(&collected.text[norm.clone()], &style.font, style.font_size);
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
                // ruby 注音那一族 piece 带的横向落点；正文恒为 0。
                x_offset: piece.x_offset,
            }));
        }
    }

    /// 行内替换盒：整高在基线以上，再按 `vertical-align` 上抬/下沉。
    ///
    /// 从前这里只看 `height`，图片的 `baseline_shift` 被丢在地上；`sup img` 因此
    /// 既没有上标位移，行盒也没把它的顶算进去，视觉上就是压到上一行字上。
    pub(super) fn push_image(&mut self, img: &ImageAtom) {
        self.width += img.advance;
        self.ascent = self.ascent.max(img.height + img.baseline_shift.max(0.0));
        self.descent = self.descent.max((-img.baseline_shift).max(0.0));
        self.fragments.push(LineFragment::Image(ImageRun {
            node: img.node,
            href: img.href.clone(),
            width: img.width,
            height: img.height,
            advance: img.advance,
            x_offset: img.x_offset,
            baseline_shift: img.baseline_shift,
            fit: img.fit,
        }));
    }
}

/// 宽度表里 `range` 这一段的总宽。续字节是 0，所以连续切片求和就够（且可向量化）。
fn sum_widths(collected: &Collected, range: Range<usize>) -> f32 {
    collected.widths.get(range).map_or(0.0, |s| s.iter().sum())
}

/// 硬行扫描收尾：把最后一段收成一行。空硬行（`<br><br>`）也要占一行高，否则会塌掉。
/// `from` 是这次扫描的起点（整行扫描时是硬行首，逐字符兜底时是被交回的断点）。
#[allow(clippy::too_many_arguments)] // 与上面两个扫描函数同一批输入，拆开反而要多传一层
fn finish_hard_line(
    cap: &Option<DropCap>,
    insets: &Insets<'_>,
    from: usize,
    hard_end: usize,
    line_start: usize,
    first: &mut bool,
    indent: f32,
    plans: &mut Vec<LinePlan>,
) {
    let before = plans.len();
    if line_start < hard_end {
        plans.push(plan_for(
            line_start..hard_end,
            cap,
            insets,
            plans.len(),
            *first,
            indent,
        ));
        *first = false;
    } else if from == hard_end {
        plans.push(plan_for(
            from..hard_end,
            cap,
            insets,
            plans.len(),
            *first,
            indent,
        ));
        *first = false;
    }
    // 这一趟收尾的这一行就是本硬行的最后一行（段末 / `<br>` 前）→ 记成不对齐。
    if plans.len() > before
        && let Some(last) = plans.last_mut()
    {
        last.hard_end = true;
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
    insets: &Insets<'_>,
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
    let mut inset = line_inset(cap, insets, plans.len(), *first, indent);
    let mut avail = inset.avail(width);
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
            plans.push(plan_for(
                line_start..cut,
                cap,
                insets,
                plans.len(),
                *first,
                indent,
            ));
            line_start = cut;
            used = sum_widths(collected, cut..at) + w;
            *first = false;
            // 换了行 → 重算这一行让出去的宽。
            inset = line_inset(cap, insets, plans.len(), *first, indent);
            avail = inset.avail(width);
        } else {
            used += w;
        }
    }
    finish_hard_line(
        cap, insets, from, hard_end, line_start, first, indent, plans,
    );
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
    insets: &Insets<'_>,
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
    let mut inset = line_inset(cap, insets, plans.len(), *first, indent);
    let mut avail = inset.avail(width);
    let mut j = 0;
    while j < atoms {
        let s = boundary(j);
        let e = boundary(j + 1);
        let w = sum_widths(collected, s..e);
        if s > line_start && used + w > avail {
            // 整个词放不下且本行已有内容 → 在词首断行。
            plans.push(plan_for(
                line_start..s,
                cap,
                insets,
                plans.len(),
                *first,
                indent,
            ));
            line_start = s;
            used = 0.0;
            *first = false;
            inset = line_inset(cap, insets, plans.len(), *first, indent);
            avail = inset.avail(width);
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
    finish_hard_line(
        cap, insets, hard.start, hard.end, line_start, first, indent, plans,
    );
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

/// 折一趟行内内容要用的三个**尺寸基准**。
///
/// 并成一个结构体的理由不是「参数太多想收拢一下」，而是这三个数**同源同批**：
/// 都由调用方从 `LayoutOptions` 与这一块的可用宽一起给出，少给一个就换一档口径 ——
/// `screen_width` 是 `None` 就是「界面没报屏」那一档（图片退回按栏宽算，见
/// [`super::collect::Ctx::screen` 的文档]），`page_height` 是 `None` 时「够不够大」
/// 只按宽判。拆成三个散参数时 `collect` 的实参涨到了 clippy 的七格上限之外，
/// 而那就是「这三位本来是一个东西」的信号。
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Sizes {
    /// 段落可用宽：图片百分比宽度与折行的基准。
    pub width: f32,
    /// 屏幕宽（`None` = 界面没给）。判「大图铺满」用。
    pub screen_width: Option<f32>,
    /// 一页（一屏）的高，判「这张图是不是大到该铺满」用。
    pub page_height: Option<f32>,
    /// 屏幕高（`None` = 界面没给）。连续滚动那一档没有页高，判「够不够大」时
    /// 由它顶上 —— 两档同一条判据，见 [`super::collect::fills_the_screen`]。
    pub screen_height: Option<f32>,
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
    sizes: Sizes,
) -> Collected {
    let Sizes {
        width,
        screen_width,
        page_height,
        screen_height,
    } = sizes;
    let mut out = Collector {
        text: String::new(),
        widths: Vec::new(),
        pieces: Vec::new(),
        images: Vec::new(),
        rubies: Vec::new(),
        arena: StyleArena::new(),
        pending_space: false,
        in_note: false,
        ruby_depth: 0,
        note_width: 0.0,
        dom,
        styles,
        provider: images,
        measure,
        width,
        screen_width,
        page_height,
        screen_height,
        alone_in_paragraph: pieces.len() == 1
            && super::collect::picture_src(dom, pieces[0]).is_some(),
    };
    for node in pieces {
        let style = out.intern_style(*node);
        out.push_node(*node, pre, style);
    }
    Collected {
        text: out.text,
        pieces: out.pieces,
        images: out.images,
        rubies: out.rubies,
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
    /// ruby 容器在 `text` 里占的段（基字 + 注音），按起点升序。见 [`Collector::push_ruby`]。
    rubies: Vec<Range<usize>>,
    /// 这一段用到的样式的去重表。
    arena: StyleArena,
    /// 折叠空白是**跨节点**的：上一段尾巴上的空白与下一段开头的空白是同一次折叠，
    /// 谁先落地谁占位，所以这个状态挂在收集器上。
    pending_space: bool,
    /// 现在正在收注音（`<rt>`）子树。为真时 [`Self::push_widths`] 把前进量**记走、不落表** ——
    /// 容器要让的那一段宽已在基字那头一次结清（见 [`Collector::push_ruby`]）。
    ///
    /// 它只在一个容器的注音那一趟里为真：更里面的 `<ruby>` 被下面的 [`ruby_depth`] 挡回普通
    /// 行内递归，所以这里不需要栈，`note_width` 也就只需要一个累加器。
    in_note: bool,
    /// 现在在某个 ruby 容器的子树里吗。>0 时更里面的 `<ruby>` **不再当容器**，照普通行内收：
    /// 嵌套 ruby（注音上面还有注音）本机语料 0 处，为它维护一层栈不如退回行内。
    ruby_depth: u32,
    /// 当前这一趟注音子树的真实宽度总和（`in_note` 期间累加，容器用它定前进量）。
    note_width: f32,
    dom: &'a Dom,
    styles: &'a StyledDom,
    provider: &'a dyn ImageProvider,
    measure: &'a dyn Measure,
    /// 段落可用宽度，图片百分比宽度的基准。
    width: f32,
    /// 屏幕宽（`None` = 界面没给）。见 `Ctx::screen`。
    screen_width: Option<f32>,
    /// 一页（一屏）的高，用来判「这张图是不是大到该铺满」。`None` 时退回
    /// [`Self::screen_height`]，两者都没有才只按宽判。
    page_height: Option<f32>,
    /// 屏幕高（`None` = 界面没给）。见 [`Sizes::screen_height`]。
    screen_height: Option<f32>,
    /// 这一趟的行内内容**只有一张图**（`<p><img></p>` 那种独占一段的插图）。
    ///
    /// 书里的插图绝大多数是这么写的：`<img>` 在 UA 表里天生 inline，于是它不会走
    /// 块级那条路，而被收成一个「只有一张图」的匿名段。读者那一档「铺满屏幕」说的
    /// 是「整块的图铺到屏幕边」—— 独占一段就是那个「整块」，所以这一格让行内臂
    /// 也有资格按屏幕宽排；夹在字中间的那种（同段还有别的 piece）仍然不放大，
    /// 否则一张图标会把整段顶走。
    alone_in_paragraph: bool,
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
        let word_spacing = run.word_spacing;
        let letter_spacing = run.letter_spacing;
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
            // 字距叠在**每个**字符上，词距再叠在空格上 —— 与 `RunStyle::advance` 逐字一致，
            // 否则会出现「按宽度表断行、按 advance 命中」两处漂。
            let mut w = adv.get(i).copied().unwrap_or(0.0) + letter_spacing;
            if ch == ' ' {
                w += word_spacing;
            }
            if self.in_note {
                // 注音那一趟：宽度**记走**（容器要让的那一段靠它），表里留 0.0 ——
                // `resize` 已经填过 0，所以这里只是不落数。断行与行宽因此看不见注音，
                // 而绘制与命中按 `RunStyle::advance` 逐字走，注音自己的字还是正常间距。
                self.note_width += w;
                continue;
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
                    x_offset: 0.0,
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
        // 行内替换元素：`<img>` 与 Calibre 封面那一族 `<svg><image xlink:href/></svg>`
        // —— 判据与「这一格该多大」的来源见 [`super::collect::picture_src`]。
        // 命中就当一张图占住，不再递归它的子节点（svg 里那张 image 不能再被收一次）。
        if super::collect::picture_src(self.dom, node).is_some() {
            self.push_image(node);
            return;
        }
        // ruby 容器：注音抬到基字上方（或下方）、不吃前进量。认不出那一种形状就返回
        // `false`，落到下面那条普通行内递归 —— 与这一格改之前逐字相同。
        // `ruby_depth > 0` 时不当容器：嵌套 ruby 的几何不猜，见 `Collector::ruby_depth`。
        if self.ruby_depth == 0 && self.dom.is_element(node, "ruby") && self.push_ruby(node, pre) {
            return;
        }
        // 文本继承的是**它所在元素**的样式，所以往下走要换成元素自己的样式。
        let own = self.intern_style(node);
        for child in dom_node.children.clone() {
            self.push_node(child, pre, own);
        }
    }

    /// `<ruby>` 容器：基字照常规流行内走，注音（`<rt>`）排在它上方或下方。
    ///
    /// 只认「基字在前、注音在后」那一种形状 —— 本机唯一带真 ruby 的书（《诡秘之主》，
    /// 40 章 552 组）写的就是 `<ruby>愚者<rt>The Fool</rt></ruby>`。三种情况返回 `false`
    /// 交回普通行内递归：没有注音、没有可见的基字、注音后面还站着别的内容（HTML 规范
    /// 那种 `<rp>(</rp>` 括号形态正好落在这一档，见 `ua::INLINE_TAGS` 那条注释）。
    ///
    /// **三个数在这一趟里算清，全部折进既有的那几张表，不新开一条几何通道**：
    ///
    /// 1. **容器前进量** = max(基字宽, 注音宽)。差值补给基字最后一个字符在宽度表里的
    ///    那一格，于是断行、行宽、对齐、命中吃的还是同一份表 —— 这个引擎的纪律是「换一处
    ///    就要改三处」，另开一条前进量来源就是第二个真相。
    /// 2. **抬升量** = 基字上沿 + 一道缝 + 注音下沿（`ruby-position: under` 时符号相反、
    ///    换成两条下沿）。它写进注音子树每一条样式的 `baseline_shift`：`LineAcc::push_text`
    ///    本来就把这一格算进 `ascent`，所以行盒自己长高、分片跟着走，绘制那一路早就认它。
    ///    注音的上下 extent 要等它落进表里才量得出，而 `baseline_shift` 得在推之前就知道 ——
    ///    所以先按零位移铺进去，再逐 piece 换成带位移的那一份样式（一章的注音是几十到
    ///    几百个 piece，不是正文那种一万量级，这一趟换样式不贵）。
    /// 3. **横向落点** = `ruby-align` 折出来的偏移。注音在流动里排在**容器末尾**（它前面的
    ///    基字已经让过 container 宽），要挪回的那一段就是 `container − flow_offset`。
    ///
    /// 注音的字符在宽度表里是 0，但 `RunStyle::advance` 照自己的字号走 —— 所以注音内部的字
    /// 距与选中矩形都正常，只有它对**容器之外**的流动不贡献前进量。
    fn push_ruby(&mut self, node: NodeId, pre: bool) -> bool {
        let children = self.dom.get(node).children.clone();
        let mut bases: Vec<NodeId> = Vec::new();
        let mut notes: Vec<NodeId> = Vec::new();
        for child in children {
            if self.dom.is_element(child, "rt") {
                notes.push(child);
            } else if notes.is_empty() {
                bases.push(child);
            } else {
                // 注音之后还有内容：不是这一档形状，整棵子树退回普通行内。
                return false;
            }
        }
        if notes.is_empty() {
            return false;
        }
        // 基字得有可见内容：注音要有东西可依附，全空白就当普通行内。
        // 这一条必须在**任何改动之前**判掉，否则递归过一次基字就退不回去了。
        if !bases
            .iter()
            .any(|child| !self.dom.text_of_subtree(*child).trim().is_empty())
        {
            return false;
        }
        // 容器的这几格决定这一趟怎么折；`intern_style` 走的是同一份样式表。
        let (align, under, rtl, gap) = {
            let container = self.styles.get(node).unwrap_or(&DEFAULT_STYLE);
            (
                container.ruby_align,
                container.ruby_position == RubyPosition::Under,
                container.direction.is_rtl(),
                container.font_size * RUBY_GAP_EM,
            )
        };
        let own = self.intern_style(node);
        self.ruby_depth += 1;

        let base_start = self.text.len();
        let base_pieces = self.pieces.len();
        for child in &bases {
            self.push_node(*child, pre, own);
        }
        let base_end = self.text.len();

        let note_pieces = self.pieces.len();
        self.in_note = true;
        for child in &notes {
            self.push_node(*child, pre, own);
        }
        self.in_note = false;
        let note_width = std::mem::take(&mut self.note_width);
        let note_end = self.text.len();
        self.ruby_depth -= 1;

        // 两条 extent：基字那一头要的是**上沿**（它决定注音排多高），注音那一头要的是
        // 自己的下沿（它决定抬升量再加多少）。位移也叠进去 —— 基字在 `<sup>` 里时本来就抬着。
        let (mut base_top, mut base_bottom) = (0.0f32, 0.0f32);
        for piece in &self.pieces[base_pieces..note_pieces] {
            let Some(run) = self.arena.get(piece.style) else {
                continue;
            };
            let (ascent, descent) = self.measure.font_extents(
                &self.text[piece.norm.start..piece.norm.end],
                &run.font,
                run.font_size,
            );
            base_top = base_top.max(ascent + run.baseline_shift);
            base_bottom = base_bottom.max(descent - run.baseline_shift);
        }
        let (mut note_top, mut note_bottom) = (0.0f32, 0.0f32);
        for piece in &self.pieces[note_pieces..] {
            let Some(run) = self.arena.get(piece.style) else {
                continue;
            };
            let (ascent, descent) = self.measure.font_extents(
                &self.text[piece.norm.start..piece.norm.end],
                &run.font,
                run.font_size,
            );
            note_top = note_top.max(ascent + run.baseline_shift);
            note_bottom = note_bottom.max(descent - run.baseline_shift);
        }
        // 抬升量按「注音的下沿落在基字上沿之上、中间留一道缝」定：这样两头的字形都不咬。
        let rise = if under {
            -(base_bottom + gap + note_top)
        } else {
            base_top + gap + note_bottom
        };

        // 前进量：容器取较宽那一条，差值挂在基字末字上（表里的数只有这一处动）。
        let base_width: f32 = self
            .widths
            .get(base_start..base_end)
            .map_or(0.0, |slice| slice.iter().sum());
        let container = base_width.max(note_width);
        let extra = container - base_width;
        if extra > 0.0
            && let Some(last) = self.text[base_start..base_end].char_indices().next_back()
        {
            self.widths[base_start + last.0] += extra;
        }

        // 注音 piece：换成带位移的那一份样式，并把横向落点记上。
        //
        // `back` 是「沿字轴往回流走多少」，而 `Piece::x_offset` 与列表标记那一格同一口径 ——
        // **物理**左偏移：横排往左是负，RTL 的流动本身从右往左，往回流就是往右（正）。
        // 居中那一档两头同号同一个数，所以这一支只影响 `start` / `end` 两档。
        let back = container - align.flow_offset(container, note_width);
        let offset = if rtl { back } else { -back };
        let end = self.pieces.len();
        for index in note_pieces..end {
            let Some(mut shifted) = self.arena.get(self.pieces[index].style).cloned() else {
                continue;
            };
            shifted.baseline_shift += rise;
            let style = self.arena.intern(shifted);
            let piece = &mut self.pieces[index];
            piece.style = style;
            piece.x_offset = offset;
        }
        self.rubies.push(base_start..note_end);
        true
    }

    /// 行内图片：写一个哨兵占位并记下尺寸。
    ///
    /// 资源解不出来时分两档，与 `scene.rs` 那句「解不出资源的 `<img>` 留位不画」同一个口径：
    /// - `<svg>` 带 `viewBox`：比例是**版面**给的，不依赖那张图，所以照样占位（否则这一页
    ///   高 0，在滚动链里根本不存在 —— 症状就是「回滑滑不到封面」）。`href` 交原始引用，
    ///   画的那一层查不到就是不画。
    /// - `<img>`：尺寸只能从图本身来，取不到就整张跳过（不产出字符，也不会把段落撑乱）。
    fn push_image(&mut self, node: NodeId) {
        let Some(src) = super::collect::picture_src(self.dom, node) else {
            return;
        };
        let aspect = super::collect::svg_aspect(self.dom, node);
        let resolved = self.provider.resolve(&src);
        let style = self.styles.get(node).unwrap_or(&DEFAULT_STYLE);
        // 作者写了 `duokan-bleed`（本节点或包它的任意一层）就是把这块的可用宽**改成屏幕宽**：
        // `width:100%` 因此折成满屏而不是满栏。读者那一档「原始尺寸」压掉它 ——
        // 那一档说的就是「别替我改这张图」，作者的话也不例外。
        //
        // 读者选了「铺满屏幕」时同样换成屏幕宽：那一档说的就是「图铺到屏幕边上」，
        // 与作者有没有出血无关。两档共用下面这一个 `wide` —— 基准宽与「够不够大」
        // 的判据必须同源，否则「基准换了、判据没换」会让同一张图在两处得到不同的宽。
        let bleed = super::collect::bleeds_wide(self.dom, self.styles, node)
            && !matches!(
                style.image_scale,
                crate::infra::config::ImageScale::Original
            );
        // 「铺满屏幕」这一档对**独占一段**的图生效：书里的插图几乎都是 `<p><img></p>`
        // 那个形状（`<img>` 天生 inline），按它才算「整块的图」。
        let wide = bleed
            || (self.alone_in_paragraph
                && matches!(style.image_scale, crate::infra::config::ImageScale::Screen));
        // 基准：要铺到屏幕上的那一张换成屏幕宽（比栏宽窄时仍取栏宽，不许把图缩进栏里）。
        let basis = if wide {
            self.screen_width
                .map_or(self.width, |screen| screen.max(self.width))
        } else {
            self.width
        };
        let (mut w, mut h) = match (&resolved, aspect) {
            (_, Some(ratio)) => (basis, basis * ratio),
            (Some(img), None) => image_size(style, img.width, img.height, basis, self.screen_width),
            (None, None) => return,
        };
        // 大图铺满屏幕（读者 2026-10-30 定的口径）：**够大**的图按屏幕宽等比放大，
        // 而不是缩在页边距里。判据是 CSS 里的 `object-fit` —— 作者写了它就是在说
        // 「这张图要盖住一块区域」，而 `width:100%` 已经把盒子定成栏宽；`cover` 与
        // 大尺寸的 `contain` 都按屏幕宽重算一遍宽高。只放大不改小：一张小图标不该被
        // 拉成满屏（`scale-down` 与 `fill` 保持原样）。
        if let Some(screen) = self.screen_width
            && matches!(
                style.image_scale,
                crate::infra::config::ImageScale::Follow | crate::infra::config::ImageScale::Screen
            )
            && (bleed
                || matches!(
                    style.object_fit,
                    crate::content::format::css::computed::ObjectFit::Cover
                        | crate::content::format::css::computed::ObjectFit::Contain
                ))
            && w < screen
            && resolved.is_some()
        {
            // `contain` 还要求「本来就不小」：宽至少是栏宽的八成，或高至少是**一屏**的
            // 八成（一张页内的插图不该被撑满整屏）。`cover` 是作者显式要盖满，不再加这一条。
            //
            // 页高那一格分页/仿真有、连续滚动没有（滚动档 `page_height` 传 `None`），
            // 所以它必须退回**屏高**：只按页高判会让同一张图在滚动里被放大、切到仿真又
            // 缩回去 —— 读者报的「切到仿真图变小」正是这一条。判据只留一处，见
            // [`super::collect::fills_the_screen`]。
            // 行内那一臂传 `bleed` 而不是 `wide`：读者那一档「铺满屏幕」只管整块独占的图，
            // 夹在字里的一张图标被拉成满屏等于把整段顶走（这一条由 `the_fills_the_screen_rule`
            // 那条门钉住）。作者的 `duokan-bleed` 照旧算数。
            //
            // 但「够不够大」这一条**两档共用同一份输入**：页高缺省时退回屏高。从前这里
            // 只认页高，连续滚动（`page_height: None`）里 `contain` 恒不放大，切到分页
            // 又放大 —— 读者报的「切到仿真图变小」正是这一条。
            let big_enough = super::collect::fills_the_screen(
                wide || matches!(
                    style.object_fit,
                    crate::content::format::css::computed::ObjectFit::Contain
                ),
                style,
                w,
                h,
                self.width,
                self.page_height,
                self.screen_height,
            );
            if big_enough {
                h *= screen / w.max(1.0);
                w = screen;
            }
        }
        // 页高是硬上限：这一行图会被分片器整个放进一页，超过的那一截没有下一页接。
        if let Some(page_h) = self.page_height {
            let page_h = page_h.max(1.0);
            if h > page_h && h > 0.0 {
                w *= page_h / h;
                h = page_h;
            }
        }
        // 上标里的标记图：书里的 `sup img { height: 1em; width: auto }` 常常被
        // 作者样式表漏掉；没有这一道上限时，36×48 的「注」图会按固有尺寸顶出整行。
        // 上限取最近那个 `<sup>` 的字号 —— 那是上标本身那一档，不是父块的正文字号。
        let sup = self
            .dom
            .ancestors(node)
            .find(|id| self.dom.tag_name(*id) == Some("sup"));
        let sup_font = sup
            .and_then(|id| self.styles.get(id))
            .map(|style| style.font_size)
            .filter(|cap| *cap > 0.0);
        if let Some(cap) = sup_font
            && h > cap
        {
            if h > 0.0 {
                w *= cap / h;
            }
            h = cap;
        }
        // 标记图与前后字之间留一道缝。浏览器里 `sup` 边缘贴着前者是常态，可
        // 读者看到的是「圆圈盖在字上」；这道留白只加前进量，不拉伸图片。
        // 出血的那一张比这一栏宽，左右各摊一半溢出（负的 `x_offset` 把画的那一格往左推）：
        // 它因此盖在页边距上，而**居中在这块的内容盒**（左右页边距对称时就是屏幕居中）。
        // 行宽按同一个式子算 —— `advance = w + 2·x_offset`，摊掉的那一截正好把行折回栏宽，
        // 所以一张满屏图不会把这一行顶宽，断行与命中吃的仍是这个数。
        // 判据与上面那一格同源：用 `wide` 而不是 `bleed`，否则「铺满屏幕」那一档的图
        // 会靠右溢出（只往右长），而不是居中盖在左右页边距上。
        let overflow = if wide {
            ((w - self.width) / 2.0).max(0.0)
        } else {
            0.0
        };
        let x_offset = sup_font.map_or(0.0, |cap| (cap * 0.18).max(1.5)) - overflow;
        let advance = w + x_offset * 2.0;
        // `vertical-align` 不继承，但图片在 `sup` 里面时，那个上标位移属于它所在的行内盒。
        // 逐级把祖先的基线偏移加回来，`<sup><a><img></a></sup>` 这条链才不会丢。
        let mut baseline_shift = style.baseline_shift;
        for anc in self.dom.ancestors(node) {
            baseline_shift += self
                .styles
                .get(anc)
                .map_or(0.0, |style| style.baseline_shift);
        }
        let href = match resolved {
            Some(img) => img.href,
            None => src,
        };
        self.flush_space();
        let at = self.text.len();
        self.text.push(IMAGE_GLYPH);
        self.widths.push(advance);
        for _ in 1..IMAGE_GLYPH.len_utf8() {
            self.widths.push(0.0);
        }
        self.images.push(ImageAtom {
            node,
            href,
            width: w,
            height: h,
            advance,
            x_offset,
            baseline_shift,
            at,
            fit: style.object_fit,
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
