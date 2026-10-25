//! 静止帧替**下一张纸**先把图解开。
//!
//! 正文里的图一直是「这一帧要用才知道」：`Scene::build` 记下一条 `DecodeRequest`，图集当场
//! 起一趟后台活儿，而这一帧那块空着。翻到带图的一页时读者看到的是「先空一格，过一两帧
//! 图才浮上来」，滚得慢时更明显 —— 排版没问题，只是那笔钱付在了它最扎眼的时刻。
//!
//! 图集早就留好了预取那道门（`ImageAtlas::prepare`）：它起活、把格子按「在场」记上，
//! 只是不进 `uploaded` 那一格（那张图没被画过，放掉时不记一笔显存的账）。此前只有放大
//! 看图那一层用它，正文没用。
//!
//! ## 为什么是「紧挨着的那一张纸」，不是「下一屏所有图」
//!
//! 图集的封顶是 24 MiB（`image_atlas::MAX_RESIDENT_BYTES`），而放掉一格的判据是「离场满
//! `HELD_AFTER_LEAVE` 帧」。预取进来的那一格要**每帧被碰到**才活得下来，被碰到的次数就是
//! 它与当前屏的距离 —— 预取太远，还没翻到就被 `end_frame` 收走了，等于白解一趟。
//! 所以这里只取紧挨着的下一次翻页，而且每帧至多起一趟（[`first_unseen`] 只回第一张）。
//!
//! ## 分页与连续滚动各取哪一张
//!
//! - 分页 / 双页：换个页号就是换一整屏，所以「下一张」= `page ± 1`；到章末那一头换成
//!   邻章的首页（往后）/ 末页（往回）。邻章只在已经排进 `spans` 时才看 —— 这里不为预取
//!   触发一章的解析与排版（那是 `ensure_paged_span` 那条路的事，它有自己的门）。
//! - 连续滚动：「下一屏」是当前可见带再往下/往上一个视口高，可能跨章，所以按全局带切。
//!
//! 这一层只做**收集**：不碰图集、不碰窗口，输入是一份 `&Book` 与一条带，回一批
//! `DecodeRequest`。起活与记账由 `view::body` 在 `atlas.end_frame` 之前那一句做。

use std::ops::Range;

use crate::ui::features::reader::model::book::{Book, Placement};
use crate::ui::features::reader::model::paging::{PrefetchSide, prefetch_side};
use crate::ui::foundation::images::DecodeRequest;

/// 连续滚动：这一帧往下预解还是往上预解。
///
/// 判据是**这条链还有没有可滚的余量**：滚到底了就只剩往上那条路，否则（包括刚进书、
/// 一章读完还没触底）一律往下。为什么不做成「两头都预解」：图集封顶 24 MiB，两个方向的
/// 图同时在手会把读者眼前那几张挤出去（见文件头）。
pub(in crate::ui::features::reader) fn side_for_scroll(
    scroll: f32,
    max_scroll: f32,
) -> Option<PrefetchSide> {
    let at_bottom = scroll >= max_scroll - 0.5;
    match (max_scroll > 0.5, at_bottom) {
        // 整章一屏装得下：没有「下一屏」可去。
        (false, _) => None,
        (true, false) => Some(PrefetchSide::Ahead),
        (true, true) => Some(PrefetchSide::Behind),
    }
}

/// 分页 / 双页：这一帧预解哪一侧。
///
/// 同样只看「还有没有下一页/上一页」：往后读是绝大多数，所以只要后面还有内容就往后；
/// 到章末才回头看（回翻那一趟要的正是它）。单章单页的书两侧都没有。
pub(in crate::ui::features::reader) fn side_for_paged(
    page: usize,
    pages: usize,
    chapter: usize,
    chapters: usize,
) -> Option<PrefetchSide> {
    prefetch_side(
        page > 0 || chapter > 0,
        page + 1 < pages || chapter + 1 < chapters,
    )
}

/// 定版式：这一页之后该预解哪一页。
///
/// 定版式没有章，页序就是阅读序；与分页那一档同一条取舍 —— 后面还有内容就往后，
/// 到书尾才回头看上一页。整本书只有一页时没有可预解的下一张。
pub(in crate::ui::features::reader) fn fixed_target(page: usize, pages: usize) -> Option<usize> {
    if page >= pages {
        return None;
    }
    if page + 1 < pages {
        Some(page + 1)
    } else if page > 0 {
        Some(page - 1)
    } else {
        None
    }
}

/// 一张带着图的纸：哪一章、多高的一条带（**章内排版坐标**）。
#[derive(Clone, Debug, PartialEq)]
pub(in crate::ui::features::reader) struct PrefetchTarget {
    pub chapter: usize,
    pub band: Range<f32>,
}

impl PrefetchTarget {
    fn new(chapter: usize, band: Range<f32>) -> Self {
        Self { chapter, band }
    }
}

/// 分页 / 双页：紧挨着当前这一张纸的下一次翻页落在哪。
///
/// 往后翻到章末时换成下一章的第一张，往回翻到章首时换成上一章的末张。邻章还没排进
/// `spans`（或还没排完）就回空 —— 这一次不预取，等它排好之后的帧自然会取到。
pub(in crate::ui::features::reader) fn paged_targets(
    book: &Book,
    page: usize,
    pages: usize,
    columns: usize,
    side: PrefetchSide,
) -> Vec<PrefetchTarget> {
    let chapter = book.chapter_index();
    match side {
        PrefetchSide::Ahead => {
            if page + 1 < pages {
                page_band(book, chapter, page + 1, columns)
                    .into_iter()
                    .collect()
            } else if chapter + 1 < book.chapter_count() {
                edge(book, chapter + 1, false, columns)
                    .into_iter()
                    .collect()
            } else {
                Vec::new()
            }
        }
        PrefetchSide::Behind => {
            if page > 0 {
                page_band(book, chapter, page - 1, columns)
                    .into_iter()
                    .collect()
            } else if chapter > 0 {
                edge(book, chapter - 1, true, columns).into_iter().collect()
            } else {
                Vec::new()
            }
        }
    }
}

/// 第 `index` 章第 `page` 张纸那一片的带。
fn page_band(book: &Book, index: usize, page: usize, columns: usize) -> Option<PrefetchTarget> {
    let doc = book.span_doc(index)?;
    if !doc.is_current() {
        return None;
    }
    let slice = doc.slice_of(page, 0, columns.max(1))?;
    (slice.height > 0.0)
        .then(|| PrefetchTarget::new(index, slice.origin_y..slice.origin_y + slice.height))
}

/// 某一章的第一张（`last == false`）或最后一张（`true`）纸。
fn edge(book: &Book, index: usize, last: bool, columns: usize) -> Option<PrefetchTarget> {
    let doc = book.span_doc(index)?;
    let pages = doc.page_count().max(1);
    page_band(book, index, if last { pages - 1 } else { 0 }, columns)
}

/// 连续滚动：全局 y 上紧挨着当前可见带的那一屏。
///
/// `placed` 是这一帧铺进坐标链的那些章（`Book::placements`：章号、章基线、章高），与
/// `view::body` 搭元素树时用的是同一批数 —— 「下一屏是哪一章的哪一段」与「哪一章真摆
/// 在那儿」必须同源，否则预解的是隔壁章那一张。
pub(in crate::ui::features::reader) fn scroll_targets(
    placed: &[Placement],
    band: Range<f32>,
    viewport: f32,
    side: PrefetchSide,
) -> Vec<PrefetchTarget> {
    let step = viewport.max(1.0);
    let window = match side {
        PrefetchSide::Ahead => band.end..band.end + step,
        PrefetchSide::Behind => (band.start - step).max(0.0)..band.start,
    };
    let mut out = Vec::new();
    if window.end <= window.start {
        return out;
    }
    for placement in placed {
        let from = placement.base_y.max(window.start);
        let to = (placement.base_y + placement.height).min(window.end);
        if from < to {
            out.push(PrefetchTarget::new(
                placement.index,
                from - placement.base_y..to - placement.base_y,
            ));
        }
    }
    out
}

/// 这些带里、**这一帧还没碰过**的第一张图的活儿。
///
/// `seen` 是调用方这一帧已经交给图集碰过的那些容器路径（主场景与捕获副本 `painted` 过的），
/// 这里跳过它们 —— 那些已经在场，再问一次只是白查。剩下的第一张交给
/// `ImageAtlas::prepare` 起活；已经在图集里的那一张 `prepare` 也认（它查得到格就不重解，
/// 只把「在场」记上），所以这里不必自己查图集。
///
/// 每帧只回一张：起两趟解压在 24 MiB 的封顶下会把读者正在看的那张挤出去。
pub(in crate::ui::features::reader) fn first_unseen(
    book: &Book,
    targets: &[PrefetchTarget],
    seen: impl Fn(&str) -> bool,
) -> Option<DecodeRequest> {
    targets.iter().find_map(|target| {
        let doc = book.span_doc(target.chapter)?;
        // 没排完的章里行盒还是上一代 —— 预解出来的可能是已经不存在的图。等它落地再取。
        if !doc.is_current() {
            return None;
        }
        // 图片只住在行的片段里。**这里不物化**：`row_units` 会把那一行的文本句柄也建出来
        // （一章一次、跨帧留着），而预取只需要「这行有没有图」—— 读排版自己的片段就够
        // （`ImageRun` 就在 `LineBox::fragments` 上）。
        //
        // 带只用它**上下各放宽一行**找到表里的下标区间，然后按区间扫片段、不再按 y 比：
        // 行内图那一行比相邻文字行高得多，`lines_in_band` 的二分前提（行盒严格叠放）
        // 在这里不总成立，而我们要的只是「这一带附近有没有图」。宁可多扫两三行，
        // 也不赌那个前提 —— 它退化的表现是「预取永远取到整章、或者一行都取不到」。
        let layout = doc.layout();
        let first = layout.line_covering_or_above(target.band.start);
        let last = layout.line_covering_or_above(target.band.end.max(target.band.start));
        let end = (last + 1).min(layout.lines.len());
        (first.min(end)..end)
            .filter_map(|line| layout.lines.get(line))
            .flat_map(|line| line.images())
            .map(|image| image.href.clone())
            .find(|href| !seen(href))
            .and_then(|href| doc.images().decode_request(&href))
    })
}

#[cfg(test)]
mod tests {
    //! 这一层能在宿主上测到的只有选带那一半（`scroll_targets` 的两个方向与越界、两条
    //! 「选哪一侧」）。「那几行里有没有图、问没问到图集」要真书与窗口，走 `view/body/tests.rs`。

    use super::{PrefetchTarget, fixed_target, scroll_targets, side_for_paged, side_for_scroll};
    use crate::ui::features::reader::model::book::Placement;
    use crate::ui::features::reader::model::paging::PrefetchSide;

    fn placement(index: usize, base_y: f32, height: f32) -> Placement {
        Placement {
            index,
            base_y,
            height,
        }
    }

    /// 往后读时取的是当前带下面那一屏，往回读取上面那一屏，都不越过整条链的两端。
    #[test]
    fn a_scroll_prefetch_looks_one_viewport_out() {
        // 两章拼着：第 0 章 [0, 1000)，第 1 章基线下在 1200（中间 200 是章缝）。
        let placed = [placement(0, 0.0, 1000.0), placement(1, 1200.0, 800.0)];
        let ahead = scroll_targets(&placed, 0.0..600.0, 600.0, PrefetchSide::Ahead);
        assert_eq!(
            ahead,
            vec![PrefetchTarget::new(0, 600.0..1000.0)],
            "往后只该取本章剩下那一截"
        );
        let further = scroll_targets(&placed, 600.0..1000.0, 600.0, PrefetchSide::Ahead);
        assert_eq!(
            further,
            vec![PrefetchTarget::new(1, 0.0..400.0)],
            "跨过章缝之后该落进下一章：那一屏从 1200 起，到 1800"
        );
        let behind = scroll_targets(&placed, 1200.0..1600.0, 600.0, PrefetchSide::Behind);
        assert_eq!(
            behind,
            vec![PrefetchTarget::new(0, 600.0..1000.0)],
            "往回取的是视口顶再往上一屏（600..1200）落进上一章的那一截"
        );
        let top = scroll_targets(&placed, 0.0..600.0, 600.0, PrefetchSide::Behind);
        assert!(top.is_empty(), "书首往上没有可预解的那一屏");
    }

    /// 两个方向都选得到，而且**只选一个**：拖到最底才回头看，否则一律往前。
    #[test]
    fn a_scroll_prefetch_prefers_the_page_below_until_the_end() {
        assert_eq!(side_for_scroll(0.0, 3000.0), Some(PrefetchSide::Ahead));
        assert_eq!(side_for_scroll(1500.0, 3000.0), Some(PrefetchSide::Ahead));
        assert_eq!(side_for_scroll(3000.0, 3000.0), Some(PrefetchSide::Behind));
        assert_eq!(
            side_for_scroll(0.0, 0.0),
            None,
            "整章一屏装得下：没有下一屏可去"
        );
    }

    /// 分页同理：到章末才回头，书尾才停。
    #[test]
    fn a_paged_prefetch_prefers_the_next_page_until_the_book_ends() {
        assert_eq!(side_for_paged(0, 5, 3, 10), Some(PrefetchSide::Ahead));
        assert_eq!(
            side_for_paged(4, 5, 3, 10),
            Some(PrefetchSide::Ahead),
            "本章末页：下一章还有内容，仍该往后看"
        );
        assert_eq!(side_for_paged(4, 5, 9, 10), Some(PrefetchSide::Behind));
        assert_eq!(side_for_paged(0, 1, 0, 1), None, "一本书一页");
    }

    /// 定版式没有章：后面的页优先，书尾回看上一页，单页书没有目标。
    #[test]
    fn a_fixed_page_looks_one_page_ahead_until_the_end() {
        assert_eq!(fixed_target(0, 3), Some(1));
        assert_eq!(fixed_target(1, 3), Some(2));
        assert_eq!(fixed_target(2, 3), Some(1), "书尾回看上一页");
        assert_eq!(fixed_target(0, 1), None, "单页书没有可预解的下一张");
        assert_eq!(fixed_target(7, 3), None, "越界页不乱给目标");
    }
}
