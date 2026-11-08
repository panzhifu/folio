//! 一本打开的书里**纯**的那一半：拼接坐标的公式、窗口的边界、全书进度的口径、
//! 正文链接的去向。
//!
//! 有状态的那一半（锚章与邻章 Span 的生命周期、排版作业的接线）住在 app 的
//! `model::book::Book` —— 它的字段是 app 的排版状态机 `Document`（gpui 重）与
//! `BookImages`（gpui `Image`），搬不进 core，也不该搬：那份状态机的形态本来就是
//! 「接口在 core、实现在 app」（见 `layout::Measure` 那条模板）。这里只收§一判据
//! 过得了的三件事：
//!
//! - **拼接坐标的唯一公式**（[`stack`] / [`Placement`]）：基线就地累计；整页画
//!   那一侧不留缝。所有读数（章在哪、基线多少、链总高）与滚动补偿全都压在它上面。
//! - **窗口的边界**（[`window_range`]）：锚章 ± [`SPAN_WINDOW`] 保底，像素预算再往外铺。
//! - **不碰排版的口径**：全书进度（[`progress_at`] / [`locate_progress`]，与库里
//!   `store::books::progress_of` 同式）与正文链接的解析（[`resolve_link`]）。
//!
//! 全部只吃 `&[Chapter]` / 纯数与闭包，`cargo test` 表驱动地钉，不需要开窗口。

use std::collections::HashMap;

use crate::content::format::{Chapter, dir_of, fragment_of, resolve_href};
use crate::infra::config::Reader;

/// 拼进连续坐标的邻章半窗宽：锚章上下**至少**预排这么多章。
///
/// 读者会狂飙滚动：窗口给到 ±2，配合后台单线程排版队列（毫秒级一章），正常速度
/// 下接缝永远提前排好；狂飙时也至少还有一章的余量，滚到未就绪的边界只会停一拍，
/// 不会白屏。这是条**下限**：真正的边界由 [`window_range`] 按像素预算再往外铺，
/// 上界是单侧 [`SPAN_MAX_AHEAD`] 章 —— 常数量级，不随书的长度长。
pub const SPAN_WINDOW: usize = 2;

/// 拼接窗口**前向**的像素预算，按屏给：**视口底**再往下还要铺这么多，读者才滚得动、
/// 锚章才迁移。它必须不止一屏：迁移的判据是「视口顶进了下一章」，而视口顶最远只走得到
/// 链尾减一屏 —— 链尾不伸出下一章一屏，那个判据就永远够不着，读者钉在链尾那一屏上
/// （见 [`window_range`]）。
pub const SPAN_AHEAD_SCREENS: f32 = 1.5;
/// 拼接窗口**后向**的像素预算，按屏给。视口顶往上铺够这么多，往回滚不至于两下就到链头。
pub const SPAN_BEHIND_SCREENS: f32 = 0.5;
/// 前向窗口的**章数**上限：像素预算之外再兜一道，防一屏能放下几十个一行章时铺太多。
pub const SPAN_MAX_AHEAD: usize = 16;

/// 拼接坐标里两章之间的缝。跟着字号走（2em）并给一条下限：它是「上一章结束、
/// 下一章开始」的呼吸空隙，不是装饰条。
pub fn chapter_gap(prefs: &Reader) -> f32 {
    (prefs.font_size * 2.0).max(24.0)
}

/// 一章在全局坐标里的摆位（只读投影）：起点、高度，都是排版像素。
#[derive(Debug)]
pub struct Placement {
    pub index: usize,
    pub base_y: f32,
    pub height: f32,
}

/// 把「章高 + 是不是整页画」按章号顺序叠成全局坐标。
///
/// **整页画那一侧不留缝**。章间距是一层底色，夹在两章中间给正文透气；而整页画
/// （封面 / 卷首整幅底图）是一张铺满整窗的画，它下面再垫一条底色，读起来就是
/// 「封面底下多出一块白」（读者报的《诡秘之主》封面与下一张图之间那段大缝）。
///
/// 判据是**这一章的左侧**：缝属于前一章与后一章之间，只要前一章是整页画就不加 ——
/// 反方向（正文 → 整页画）仍然加，正文末尾那一段留白是给上一段文字收尾的，
/// 去掉会让最后一行的字贴着画像顶格。
pub fn stack(entries: Vec<(Placement, bool)>, gap: f32) -> Vec<Placement> {
    let count = entries.len();
    let picture: Vec<bool> = entries.iter().map(|(_, flag)| *flag).collect();
    let mut out: Vec<Placement> = entries
        .into_iter()
        .map(|(placement, _)| placement)
        .collect();
    let mut y = 0.0;
    for (i, placement) in out.iter_mut().enumerate() {
        placement.base_y = y;
        y += placement.height;
        if i + 1 < count && !picture[i] {
            y += gap;
        }
    }
    out
}

/// 拼接窗口的边界 `[lo, hi]`（含锚章）。两条规则一起用：**至少** `锚章 ± SPAN_WINDOW`
/// （长章书的驻留上界还是它），再多铺到「视口顶往前盖过一屏多、往回盖过半屏」为止。
///
/// 前向预算是**从视口顶算起**的，不是从锚章章首：读者停在长章中后段时，章首到视口
/// 那段已经滚过去了，若还从章首起算，「一屏半」全花在了视口**上方**，前方就只剩
/// [`SPAN_WINDOW`] 那两章的兜底。那两章若都是短页（献词页、整页图），加起来不够一屏，
/// 于是链到它们末尾就没了：视口顶最远只走得到链尾减一屏，永远进不了
/// 下一章 —— 锚章不迁移、窗口不往前长、`max_scroll` 也不再变大，读者就卡在那一屏上
/// （实测：《未来简史》第 3 节那个目录章比手机视口略高，后面接着献词页与整页图，
/// 两页连缝一共 442px，而视口有 750px —— 屏幕停在图与图说上，页脚念 100%）。
///
/// 后向同一条道理：视口顶之上那几屏多半还在锚章肚子里，锚章自己就盖住了，
/// 只有视口顶贴近锚章章首时才需要往上够一够。
///
/// `anchor_base` 是锚章此刻在链上的基线；`None`（锚章还没进链）时按章首算：
/// 两条夹取都往「少要一点」那一侧靠，多铺的那几章下一帧自己会补齐。
/// `height_of` 回一章的排版高；没排完的章由调用方兜底（按一屏估：
/// 宁可多铺一章，也不要少铺）。
pub fn window_range(
    anchor: usize,
    chapter_count: usize,
    gap: f32,
    window_px: f32,
    viewport_top: f32,
    anchor_base: Option<f32>,
    height_of: impl Fn(usize) -> f32,
) -> (usize, usize) {
    let last = chapter_count.saturating_sub(1);
    let height = &height_of;
    // 视口顶在锚章里的位置。锚章还没排完（base 问不出来）时按章首算：两条夹取
    // 都往「少要一点」那一侧靠，多铺的那几章下一帧自己会补齐。
    let top = anchor_base
        .map(|base| (viewport_top - base).clamp(0.0, height(anchor)))
        .unwrap_or(0.0);
    // 前向：一屏（视口自己）+ `SPAN_AHEAD_SCREENS` 屏的余量，都从视口顶往下量。
    let ahead = top + (window_px * (1.0 + SPAN_AHEAD_SCREENS)).max(1.0);
    let behind = (window_px * SPAN_BEHIND_SCREENS).max(1.0);

    let mut hi = anchor;
    let mut covered = height(anchor);
    while hi < last && (hi - anchor) < SPAN_MAX_AHEAD && covered < ahead {
        hi += 1;
        covered += height(hi) + gap;
    }
    hi = hi.max((anchor + SPAN_WINDOW).min(last));

    // 后向：视口顶之上已经铺了 `above` 像素（锚章露在视口上方的那一截先算进去）。
    let mut lo = anchor;
    let mut above = top;
    while lo > 0 && (anchor - lo) < SPAN_MAX_AHEAD && above < behind {
        lo -= 1;
        above += height(lo) + gap;
    }
    lo = lo.min(anchor.saturating_sub(SPAN_WINDOW));

    (lo, hi)
}

/// 「容器路径 → 章号」。目录条目与正文链接靠它对上正文：`TocEntry::href` 与
/// `Chapter::spine_href` 同形（都已百分号解码、去掉 fragment），所以是一次查表
/// 而不是模糊匹配。
///
/// 空 `spine_href` 不进表：单文件格式（TXT / MD / FB2）每一章都是空串，进了表
/// 就等于「所有目录条目都指向第 0 章」—— 那是比跳错更糟的错，因为它看着是好的。
/// 同一条 href 出现两次时留文档序在前的那一章。
pub fn toc_hrefs(chapters: &[Chapter]) -> HashMap<String, usize> {
    let mut map = HashMap::new();
    for (index, chapter) in chapters.iter().enumerate() {
        if chapter.spine_href.is_empty() {
            continue;
        }
        map.entry(chapter.spine_href.clone()).or_insert(index);
    }
    map
}

/// 正文里一条 `<a href>` 的去向（[`resolve_link`] 的结果）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// 书内位置：某一章，加上可选的章内锚点（`#id`）。
    Internal {
        chapter: usize,
        fragment: Option<String>,
    },
    /// 书外地址（`http:` / `mailto:` / `tel:` 等）。一期只解出来，收不收由调用方定。
    External { url: String },
}

/// `before` 里 `:` 前若是一个 URL scheme 就返回它，否则 `None`。
///
/// 判定照 RFC 3986：首字符字母，其余是字母数字或 `+` `-` `.`。文件名里出现 `:`
/// 极少见，而真正要拦的是 `http://…` 这种 —— 不拦就会被当相对路径去解、解不出。
fn external_scheme(before: &str) -> Option<&str> {
    let (scheme, _) = before.split_once(':')?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    (first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
    .then_some(scheme)
}

/// 正文里一条 `<a href>` 的去向。
///
/// 只分「书内」与「书外」两类。书内落成 `(章号, 可选锚点)` —— 与目录条目走的是
/// 同一套坐标（目录跳转与待落锚点同源），所以正文链接与目录链接跳得一样。
pub fn resolve_link(chapters: &[Chapter], chapter: usize, href: &str) -> Option<Link> {
    let href = href.trim();
    if href.is_empty() {
        return None;
    }
    let fragment = fragment_of(href);
    // `#` 之前那一截：空 = 纯章内锚点（`#m1`），否则是路径或外部地址。
    // 纯锚点是**先**判的：`resolve_href` 把 `#…` 整条剥掉之后会拿 base 目录
    // 当结果，对纯锚点给出一个指不到任何章的假路径。
    let before = href.split('#').next().unwrap_or("").trim();
    if before.is_empty() {
        return Some(Link::Internal { chapter, fragment });
    }
    if external_scheme(before).is_some() {
        return Some(Link::External {
            url: href.to_string(),
        });
    }
    // 相对路径按**这一章**所在目录解析（书内目录页写的是 `part0004.xhtml`，
    // 不是容器全路径），再回章节表查目标章。
    let base = chapters
        .get(chapter)
        .map(|c| dir_of(&c.spine_href))
        .unwrap_or_default();
    let path = resolve_href(&base, href);
    toc_hrefs(chapters)
        .get(&path)
        .copied()
        .map(|target| Link::Internal {
            chapter: target,
            fragment,
        })
}

/// 全书可见字数的**前缀和**：`before[i]` = 第 `i` 章之前所有章的字数，第二格 = 全书总字数。
///
/// 只要有一章的 `char_len` 是 `None` 就整本回 `None`。`None` 是「这一章的字数还没算过」，
/// 不是 0 —— 当成 0 会让分母偏小、百分比偏大，而那种偏差在屏上**看不出来**（页脚照样
/// 是一个像样的百分数）。为什么把「不知道」与「这一章真的是空的」分成两格，见
/// `Chapter::char_len`。
pub fn char_measure(chapters: &[Chapter]) -> Option<(Vec<usize>, usize)> {
    let mut before = Vec::with_capacity(chapters.len());
    let mut total = 0usize;
    for chapter in chapters {
        before.push(total);
        total += chapter.char_len?;
    }
    (total > 0).then_some((before, total))
}

/// 全书进度：`(前面各章字数之和 + 章内偏移) ÷ 全书总字数`。
///
/// 与架上卡片那一格（`store::books::progress_of`）**同一个式子、同一份 `char_len`**：
/// 那一格从库里存的定位串算，这一格从读者眼前这一屏算。两处必须同式，否则
/// 「架上写着 37%、点开页脚写 12%」就是两条各自成立的真相。
///
/// 这一格不经过排版，所以换字号、改页边距、双页退单页都不动它 —— 从前那版读的是
/// 「视口顶那一章的章内比例」，于是每进一章归零重来，读者拖完一整本看到的是十个
/// 0%→100%。
pub fn progress_at(chapters: &[Chapter], chapter: usize, offset: usize) -> Option<f32> {
    let (before, total) = char_measure(chapters)?;
    let len = *chapters.get(chapter)?.char_len.as_ref()?;
    let read = before.get(chapter)? + offset.min(len);
    Some((read as f32 / total as f32).clamp(0.0, 1.0))
}

/// 全书进度的反函数：比例 → 落点 `(哪一章, 章内第几个字)`。进度条那一拖走它。
///
/// 落在哪一章由前缀和二分定，章内偏移按比例折算 —— 与正函数同一条口径，所以拖到
/// 73% 再松手，页脚念回来的还是 73%（差一个字的舍入以内）。
pub fn locate_progress(chapters: &[Chapter], ratio: f32) -> Option<(usize, usize)> {
    let (before, total) = char_measure(chapters)?;
    let want = (ratio.clamp(0.0, 1.0) * total as f32) as usize;
    // 第一个「严格大于 want」的前缀就是目标章的下一章。
    let chapter = before
        .partition_point(|base| *base <= want)
        .saturating_sub(1);
    let len = *chapters.get(chapter)?.char_len.as_ref()?;
    Some((chapter, (want - before[chapter]).min(len)))
}
#[cfg(test)]
mod tests {
    use super::{
        Link, Placement, SPAN_AHEAD_SCREENS, SPAN_BEHIND_SCREENS, SPAN_MAX_AHEAD, SPAN_WINDOW,
        chapter_gap, char_measure, locate_progress, progress_at, resolve_link, stack, toc_hrefs,
        window_range,
    };
    use crate::content::format::Chapter;
    use crate::infra::config::Reader;

    fn placed(index: usize, height: f32) -> Placement {
        Placement {
            index,
            base_y: 0.0,
            height,
        }
    }

    /// 中间那一对整页图相接、不留缝；图与正文之间照旧留。
    #[test]
    fn two_full_page_pictures_touch_and_the_others_keep_their_gap() {
        let out = stack(
            vec![
                (placed(0, 100.0), false),
                (placed(1, 500.0), true),
                (placed(2, 500.0), true),
                (placed(3, 100.0), false),
            ],
            40.0,
        );
        assert_eq!(
            out.iter().map(|p| p.base_y).collect::<Vec<_>>(),
            vec![0.0, 140.0, 640.0, 1140.0],
            "图 1 与图 2 之间、图 2 与正文之间都不留章间距，正文之前那一格照留"
        );
        assert_eq!(
            out.last().map(|p| p.base_y + p.height),
            Some(1240.0),
            "整条链的末端跟着少掉那一格缝"
        );
    }

    /// 整页画之后**不再垫那一条底色**：封面底下那块白就是它。
    ///
    /// 从前的判据是「两张图**都**是整页画才不留缝」，于是「封面 → 正文」这一档
    /// 仍夹着一条章间距。而整页画的背板只铺到章盒高（它铺的是整窗，章盒
    /// 就是那一格），缝落在背板之外 —— 屏幕上读成封面底下空出一大块
    /// （《诡秘之主》封面与第二张图之间那段距离）。
    ///
    /// 反方向（正文 → 整页画）仍然留：正文最后一行的收尾间隙该有。
    #[test]
    fn a_picture_chapter_leaves_no_seam_after_it() {
        // 封面（整页画）→ 正文：不留缝。
        let out = stack(
            vec![(placed(0, 853.0), true), (placed(1, 200.0), false)],
            48.0,
        );
        assert_eq!(
            out.iter().map(|p| p.base_y).collect::<Vec<_>>(),
            vec![0.0, 853.0],
            "封面之后那条底色不该在：它落在背板之外，读成封面底下多出一块"
        );
        // 正文 → 封面：仍留（正文收尾的呼吸）。
        let out = stack(
            vec![(placed(0, 200.0), false), (placed(1, 853.0), true)],
            48.0,
        );
        assert_eq!(
            out.iter().map(|p| p.base_y).collect::<Vec<_>>(),
            vec![0.0, 248.0],
            "正文末尾与画之间那一格留着"
        );
    }

    /// 一侧是图、另一侧是正文：**图在前就不留缝**，图在后仍留。
    ///
    /// 这一条从「图与正文的分界也留缝」改过来：那条缝落在背板之外（背板铺到章盒高，
    /// 缝在章盒之后），屏幕上就是图底下一块白。反方向留着是为了正文末行的收尾。
    #[test]
    fn a_picture_next_to_text_keeps_its_seam() {
        let out = stack(
            vec![
                (placed(0, 200.0), false),
                (placed(1, 300.0), true),
                (placed(2, 200.0), false),
            ],
            24.0,
        );
        assert_eq!(
            out.iter().map(|p| p.base_y).collect::<Vec<_>>(),
            vec![0.0, 224.0, 524.0],
            "正文 → 图留缝（正文收尾），图 → 正文不留（背板之外那条底色会露出来）"
        );
    }

    /// 整条链都是图：每一处都不留缝，链就是一串紧挨着的画面。
    #[test]
    fn a_run_of_pictures_has_no_seam_anywhere() {
        let out = stack(
            vec![
                (placed(0, 300.0), true),
                (placed(1, 300.0), true),
                (placed(2, 300.0), true),
            ],
            40.0,
        );
        assert_eq!(
            out.iter().map(|p| p.base_y).collect::<Vec<_>>(),
            vec![0.0, 300.0, 600.0]
        );
    }

    /// [`window_range`] 的表驱动。`heights` 按章号给排版高；`gap` / `window_px` /
    /// `viewport_top` / `anchor_base` 直接给。锚章钉在第 2 章。
    fn range(
        heights: &[f32],
        gap: f32,
        window_px: f32,
        viewport_top: f32,
        anchor_base: Option<f32>,
    ) -> (usize, usize) {
        window_range(
            ANCHOR,
            heights.len(),
            gap,
            window_px,
            viewport_top,
            anchor_base,
            |index| heights[index],
        )
    }

    const ANCHOR: usize = 2;

    /// 保底窗口：预算再富余也**至少**锚章 ± [`SPAN_WINDOW`]；书比窗口还短时夹到书界。
    #[test]
    fn the_window_covers_at_least_two_chapters_each_way() {
        // 十章全是大章：预算一步就够，靠保底兜出 ±2。
        let heights = [2000.0; 10];
        let (lo, hi) = range(&heights, 24.0, 750.0, 0.0, Some(0.0));
        assert_eq!((lo, hi), (ANCHOR - SPAN_WINDOW, ANCHOR + SPAN_WINDOW));

        // 书比窗口短：hi 夹到最后一章，lo 夹到第 0 章。
        let short = [500.0; 3];
        let (lo, hi) = window_range(2, 3, 24.0, 750.0, 0.0, Some(0.0), |i| short[i]);
        assert_eq!((lo, hi), (0, 2));
    }

    /// 那条 442px 陷阱：锚章之后的章都是短页，加起来不足一屏 —— 像素预算**从视口顶
    /// 算起**，必须把链铺到书尾，视口顶才够得着「进了下一章」的判据。
    #[test]
    fn the_pixel_budget_reaches_past_a_run_of_short_chapters() {
        // 锚章比视口略高（800 > 750），后面两页连缝 442px（gap 24）。
        let heights = [800.0, 200.0, 242.0, 100.0];
        let (lo, hi) = window_range(0, 4, 24.0, 750.0, 0.0, Some(0.0), |i| heights[i]);
        assert_eq!(hi, 3, "预算不足就该一路铺到书尾，读者才不会钉在链尾那一屏");
        assert_eq!(lo, 0);
    }

    /// 前向预算**从视口顶算起**，不从章首：读者停在长章中后段时，章首到视口那段
    /// 已经滚过去了，从章首起算「一屏半」全花在视口上方，前方就只剩保底那两章。
    #[test]
    fn the_ahead_budget_measures_from_the_viewport_top() {
        // 锚章 5000 高，视口顶在 4000 处；预算 = 4000 + 1000×(1+1.5) = 6500。
        // 若从章首起算（2500），covered=5000 一步就够，hi 只到 1。
        let heights = [5000.0, 1000.0, 1000.0];
        let (lo, hi) = window_range(0, 3, 0.0, 1000.0, 4000.0, Some(0.0), |i| heights[i]);
        assert_eq!(hi, 2, "预算从视口顶算起，前方要铺到 6500 而不是 2500");
        // 视口顶之上已有 4000 像素，后向预算（0.5 屏）当场满足，lo 停在保底。
        assert_eq!(lo, 0);
    }

    /// 锚章还没进链（`anchor_base = None`）时按章首算，两条夹取都往「少要一点」靠。
    #[test]
    fn an_unready_anchor_asks_for_less_and_still_gets_the_floor() {
        let heights = [1000.0; 10];
        let (lo, hi) = window_range(5, 10, 24.0, 750.0, 4000.0, None, |i| heights[i]);
        assert_eq!((lo, hi), (3, 7));
    }

    /// 章数上限 [`SPAN_MAX_AHEAD`] 兜住「一屏放得下几十个一行章」的铺法。
    #[test]
    fn the_chapter_cap_bounds_the_forward_window() {
        // 一行章：每章 5px，预算再多也铺不过 16 章。
        let heights = [5.0; 40];
        let (lo, hi) = window_range(0, 40, 0.0, 750.0, 0.0, Some(0.0), |i| heights[i]);
        assert_eq!(hi, SPAN_MAX_AHEAD, "预算之外还有章数上限兜底");
        assert_eq!(lo, 0);
    }

    fn chapter(title: &str, href: &str, len: usize) -> Chapter {
        Chapter {
            spine_href: href.into(),
            char_len: Some(len),
            ..Chapter::inline(title, "甲")
        }
    }

    /// 全书进度与它的反函数同一条口径：拖到哪儿，页脚念回来的就是哪儿。
    #[test]
    fn progress_and_its_inverse_agree() {
        let chapters = [
            chapter("甲", "a.xhtml", 100),
            chapter("乙", "b.xhtml", 100),
            chapter("丙", "c.xhtml", 100),
        ];
        assert_eq!(char_measure(&chapters).unwrap().1, 300);
        assert_eq!(progress_at(&chapters, 1, 50), Some(0.5));
        assert_eq!(locate_progress(&chapters, 0.5), Some((1, 50)));
        // 章内偏移超出本章字数时按本章长度夹住，不借下一章的字数。
        assert_eq!(progress_at(&chapters, 1, 999), Some(200.0 / 300.0));
        // 越界的比例夹住；没有字数（还没算过）整本回 None。
        assert_eq!(locate_progress(&chapters, 1.7), Some((2, 100)));
        let unknown = [Chapter::inline("甲", "")];
        assert_eq!(char_measure(&unknown), None);
        assert_eq!(progress_at(&unknown, 0, 0), None);
    }

    /// 正文链接的三条去向：书内相对路径查表对章、纯锚点留在本章、外链原样交回。
    #[test]
    fn a_link_resolves_to_a_chapter_a_fragment_or_an_external_url() {
        let chapters = [
            chapter("甲", "text/part1.xhtml", 10),
            chapter("乙", "text/part2.xhtml", 10),
        ];
        assert_eq!(toc_hrefs(&chapters)["text/part2.xhtml"], 1);
        // 相对路径按**这一章**所在目录解析。
        assert_eq!(
            resolve_link(&chapters, 0, "part2.xhtml#m1"),
            Some(Link::Internal {
                chapter: 1,
                fragment: Some("m1".into())
            })
        );
        // 纯章内锚点：`#` 前为空，先于路径解析判掉。
        assert_eq!(
            resolve_link(&chapters, 0, "#m1"),
            Some(Link::Internal {
                chapter: 0,
                fragment: Some("m1".into())
            })
        );
        // 书外地址原样交回，收不收由调用方定。
        assert_eq!(
            resolve_link(&chapters, 0, "https://example.com/x"),
            Some(Link::External {
                url: "https://example.com/x".into()
            })
        );
        // 空串与指不到的路径都是 None。
        assert_eq!(resolve_link(&chapters, 0, ""), None);
        assert_eq!(resolve_link(&chapters, 0, "missing.xhtml"), None);
    }

    /// 章间距跟字号走（2em）并有一条下限：它留给正文呼吸，不是装饰条。
    #[test]
    fn the_chapter_gap_scales_with_the_font_and_keeps_a_floor() {
        let prefs = Reader {
            font_size: 16.0,
            ..Reader::default()
        };
        assert_eq!(chapter_gap(&prefs), 32.0);
        let small = Reader {
            font_size: 6.0,
            ..Reader::default()
        };
        assert_eq!(chapter_gap(&small), 24.0, "小字号也不许两章贴死");
        let _ = (SPAN_AHEAD_SCREENS, SPAN_BEHIND_SCREENS);
    }
}
