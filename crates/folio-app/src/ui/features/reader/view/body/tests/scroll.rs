//! 连续滚动：章界流动、脊柱窗口、章高与视口。

use super::harness::{
    PARA, backdrop_chapter, chapters_flow, flow, meta, open, open_scroll, open_scroll_sized,
    open_scroll_with_store, plain_backdrop_chapter, prefs, progress_of, raw_touch_at, scroll_down,
    scroll_mode, settle,
};
use crate::ui::features::reader::model::book::Book;
use crate::ui::features::reader::model::chrome::PageMode;
use crate::ui::features::reader::model::session::ReaderSession;
use folio_core::store::Store;
use folio_core::{Chapter, FlowDoc, Format, Locator};
#[allow(unused_imports)] // `#[gpui_kit::test]` 展开要它，unused 分析看不见宏里的使用
use gpui_kit::{AppContext as _, Entity, TestAppContext, TouchPhase, point, px};
use std::sync::Arc;

/// 短章语料：每章只有一小段（远不到一屏），用来钉连续滚动在「章比屏短」时也不卡死。
fn short_chapters_flow(count: usize) -> FlowDoc {
    FlowDoc {
        chapters: (0..count)
            .map(|i| {
                let source = format!("<p>第{i}章 的正文只有这一小段字。</p>");
                Chapter::inline(format!("第{i}章"), source)
            })
            .collect(),
        ..Default::default()
    }
}

/// 「长章 + 两小章」语料：照《未来简史》第 3 节那一段的真实形状搭 ——
/// 第 0 章比一屏高（读者停在它中后段），第 1 章两行字，第 2 章一张整页图加一行图说，
/// 第 3 章又是正常正文。后两章连缝加起来比一屏还短，这就是会卡死的那个形状。
fn long_chapter_then_a_short_tail() -> FlowDoc {
    use folio_core::content::format::ImageRes;

    // 33 行目录：16px × 1.75 行高 = 28px 一行，整章 924px（真机那本量出来正是 924）。
    let toc = (0..33)
        .map(|i| format!("<div>目录第{i}行</div>"))
        .collect::<String>();
    FlowDoc {
        chapters: vec![
            Chapter::inline("目录", toc),
            Chapter::inline("献词", format!("<p>{PARA}</p>")),
            Chapter::inline(
                "图",
                "<div class=\"chatu\"><img src=\"fig.jpg\" alt=\"\" />\
                 <p class=\"tushuo\">图1 一行图说</p></div>",
            ),
            Chapter::inline(
                "第一章",
                (0..20)
                    .map(|k| format!("<p>{PARA}（第{k}段）</p>"))
                    .collect::<String>(),
            ),
        ],
        images: vec![ImageRes {
            href: "fig.jpg".into(),
            source: "fig.jpg".into(),
            media_type: "image/jpeg".into(),
            width: 1000,
            height: 700,
        }],
        ..Default::default()
    }
}

/// 窗口里各章排出来的高度（`0` = 还没排）。
fn chapter_heights(view: &Entity<ReaderSession>, cx: &mut TestAppContext) -> Vec<f32> {
    view.read_with(cx, |v, _| {
        let book = v.book().expect("这本书该开着");
        (0..book.chapter_count())
            .map(|i| book.span_doc(i).map(|doc| doc.height()).unwrap_or_default())
            .collect()
    })
}

// ---- 跨章连续滚动：锚章 ± 窗口拼成一条全局坐标（见 book.rs） ----
/// 向下滚过章尾：正文接上下一章，锚章迁移、进度的章号跟着走。
#[gpui_kit::test]
fn scrolling_past_the_chapter_end_flows_into_the_next_one(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &chapters_flow(3),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    scroll_mode(cx, handle);
    assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 0);

    // 100px 一步：章界是按像素判定的，步子太大可能一步迈过整章的可滚区间，
    // 「刚跨进来」的断言就失真了。
    for _ in 0..120 {
        scroll_down(cx, handle, 100.0);
        settle(cx, handle);
        if view.read_with(cx, |v, _| v.chapter_index()) == 1 {
            break;
        }
    }
    assert_eq!(
        view.read_with(cx, |v, _| v.chapter_index()),
        1,
        "滚过章尾该迁移到下一章"
    );
    let texts = view.read_with(cx, |v, _| v.line_texts().join(""));
    assert!(
        texts.contains("第1章"),
        "锚章迁移后屏上该是下一章：{texts:.80}"
    );
    let (chapter, _) = view.read_with(cx, |v, _| v.resolve_position().expect("滚得动就该有位置"));
    assert_eq!(chapter, 1, "进度的章号要跟着视口顶走");
    let progress = progress_of(&view, cx);
    assert!(
        progress < 0.5,
        "刚跨进下一章，章内比例该接近开头：{progress:.3}"
    );
}

/// 向上滚过章首：接回上一章，锚章跟着回去。
#[gpui_kit::test]
fn scrolling_back_up_reaches_the_previous_chapter(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &chapters_flow(3),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    scroll_mode(cx, handle);
    for _ in 0..40 {
        scroll_down(cx, handle, 400.0);
        settle(cx, handle);
        if view.read_with(cx, |v, _| v.chapter_index()) == 1 {
            break;
        }
    }
    assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 1);

    for _ in 0..40 {
        scroll_down(cx, handle, -400.0);
        settle(cx, handle);
        if view.read_with(cx, |v, _| v.chapter_index()) == 0 {
            break;
        }
    }
    assert_eq!(
        view.read_with(cx, |v, _| v.chapter_index()),
        0,
        "滚回章首之上该接回上一章"
    );
    let texts = view.read_with(cx, |v, _| v.line_texts().join(""));
    assert!(
        texts.contains("第0章"),
        "回去之后屏上该是上一章：{texts:.80}"
    );
}

/// 进度是**全书**的量：一路滚到底它单调不减、跨章不回落，末了明显往前走。
///
/// 从前这一格读视口顶那一章的章内比例，于是每进一章归零重来 —— 读者拖完一本三章的书
/// 看到的是三个 0%→100%。这条钉的就是「归零」这个形状：任何一次回落都当场红，
/// 而旧的章内口径在第一次跨章那一格必红（上一章末 ≈1.0 → 本章初 ≈0.0）。
#[gpui_kit::test]
fn progress_never_resets_when_the_scroll_crosses_a_chapter(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = chapters_flow(3);
    let (view, handle) = open_scroll(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
    assert_eq!(progress_of(&view, cx), 0.0, "第一章章首该是 0%");

    let mut last = 0.0;
    let mut crossed = false;
    for _ in 0..200 {
        scroll_down(cx, handle, 400.0);
        settle(cx, handle);
        let now = progress_of(&view, cx);
        assert!(
            now + 1e-3 >= last,
            "进度回落了：{last} → {now}（跨章时按章内比例念就会是这个形状）"
        );
        last = now;
        crossed |= view.read_with(cx, |v, _| v.chapter_index()) >= 1;
    }
    assert!(crossed, "这本夹具至少要有两章，否则这条测不到跨章");
    assert!(last > 0.5, "滚到底进度该明显往前走，实得 {last}");

    // 反函数那一头：拖到 50%，念回来还是 50%（± 半屏的字，因为落点按行取整）。
    view.update(cx, |v, cx| v.seek_ratio(0.5, cx));
    settle(cx, handle);
    let back = progress_of(&view, cx);
    assert!(
        (back - 0.5).abs() < 0.06,
        "拖到 50% 之后进度念回 {back}：`progress` 与 `seek_ratio` 不是同一条口径"
    );
}

/// 进度存在中间章，重开：落点是那一章的章首（全局坐标 = 章基线），
/// 向上滚能接回上一章 —— 拼接坐标对恢复进度是透明的。
#[gpui_kit::test]
fn a_locator_in_a_middle_chapter_restores_into_the_spine(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = chapters_flow(3);
    let locator = Locator {
        chapter: 1,
        offset: 0,
    };

    let store = Arc::new(Store::in_memory().expect("内存库"));
    let id = store
        .add_book(std::path::Path::new("memory://fixture"), &meta("markdown"))
        .expect("入库");
    store
        .save_flow(id, &flow, Format::Markdown)
        .expect("章节入库");
    store
        .set_position(id, PageMode::Continuous.storage_name(), &locator)
        .expect("写入进度");

    let (view, handle) = open_scroll_with_store(
        cx,
        &flow,
        "markdown",
        prefs(16.0),
        locator,
        460.0,
        Some(store),
        Some(id),
    );
    assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 1);
    let texts = view.read_with(cx, |v, _| v.line_texts().join(""));
    assert!(texts.contains("第1章"), "该落在第二章的开头：{texts:.80}");

    // 往上滚：上一章就拼在正上方，滚过章首就能接回去。
    for _ in 0..40 {
        scroll_down(cx, handle, -400.0);
        settle(cx, handle);
        if view.read_with(cx, |v, _| v.chapter_index()) == 0 {
            break;
        }
    }
    assert_eq!(
        view.read_with(cx, |v, _| v.chapter_index()),
        0,
        "中间章的章首之上该是上一章"
    );
}

/// 拼接窗口钉在锚章 ± 2：滚到哪里，预排版就跟到哪里，窗外整块让出去。
#[gpui_kit::test]
fn the_spine_window_stays_pinned_to_the_anchor(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &chapters_flow(6),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    scroll_mode(cx, handle);
    assert_eq!(
        view.read_with(cx, |v, _| v.span_window()),
        vec![0, 1, 2],
        "开书时窗口 = 锚章 + 下方两章"
    );

    // 一路滚到最后一章。
    for _ in 0..120 {
        scroll_down(cx, handle, 400.0);
        settle(cx, handle);
        if view.read_with(cx, |v, _| v.chapter_index()) == 5 {
            break;
        }
    }
    assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 5);
    assert_eq!(
        view.read_with(cx, |v, _| v.span_window()),
        vec![3, 4, 5],
        "滚到书尾，窗口滑到锚章 + 上方两章，窗外的章整块让出去"
    );
}

/// 短章也要能一章一章往下滚：连续滚动在「锚章 ± 固定几章加起来不够一屏」时，
/// 视口顶永远进不了下一章，锚章就不迁移、拼接窗口也不往前长 —— 读者卡在原地。
/// 语料给 40 个一行章：老窗口（锚章 ±2）只有 3 章 ≈ 不到一屏，必然卡死。
#[gpui_kit::test]
fn scrolling_advances_through_short_chapters(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &short_chapters_flow(40),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    scroll_mode(cx, handle);
    assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 0);
    for _ in 0..2000 {
        scroll_down(cx, handle, 40.0);
        settle(cx, handle);
        if view.read_with(cx, |v, _| v.chapter_index()) >= 30 {
            break;
        }
    }
    let last = view.read_with(cx, |v, _| v.chapter_index());
    assert!(
        last >= 30,
        "短章的连续滚动该能一直往下一章走，实际停在 {last}"
    );
}

/// 长章后面接着两小章：链尾不能停在「一屏装不满」的地方。
///
/// 读者停在长章中后段时，章首到视口那段已经滚过去了。前向预算若从**锚章章首**算起，
/// 长章自己就把「一屏半」吃光，前方只剩 `SPAN_WINDOW` 那两章的兜底；那两章连缝若比一屏
/// 还短，`content_height` 到它们末尾就没了 —— 视口顶最远只走得到链尾减一屏，永远进不了
/// 下一章：锚章不迁移、窗口不往前长，屏幕就钉在图与图说上（页脚还念 100%）。实测真机
/// 症状与这一条逐字对应。
///
/// 与 `scrolling_advances_through_short_chapters` 是一对：那一条钉「锚章自己就是短章」，
/// 这一条钉「锚章很长、前方那两章很短」。
#[gpui_kit::test]
fn a_long_chapter_with_a_short_tail_still_scrolls_on(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let prefs = prefs(16.0);
    let (view, handle) = open(
        cx,
        &long_chapter_then_a_short_tail(),
        "epub",
        prefs.clone(),
        Locator::start(),
        460.0,
    );
    scroll_mode(cx, handle);

    // 语料的前提（也就是真机上卡死的那条几何）：尾巴两章连缝比一屏还短。
    // 少了这一条，哪天语料被改高了，这条用例会绿着却什么都没测。
    let viewport = view.read_with(cx, |v, _| v.laid_out.viewport);
    let heights = chapter_heights(&view, cx);
    let gap = crate::ui::features::reader::model::book::Book::chapter_gap(&prefs);
    let tail = heights[1] + gap + heights[2];
    assert!(
        tail < viewport,
        "尾巴要短于一屏才会卡死：两章连缝 {tail:.0}px，视口 {viewport:.0}px"
    );

    assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 0);
    for _ in 0..200 {
        scroll_down(cx, handle, 100.0);
        settle(cx, handle);
        if view.read_with(cx, |v, _| v.chapter_index()) >= 3 {
            break;
        }
    }
    let last = view.read_with(cx, |v, _| v.chapter_index());
    assert!(
        last >= 3,
        "图说后面那一章该滚得到，实际停在 {last}（窗口 {:?}）",
        view.read_with(cx, |v, _| v.span_window())
    );
}

/// 上下滚动（连续模式）里竖着拖：手指走多远，正文就滚多远。
///
/// 这一档从前整笔被跟手翻页层吞掉：`swipe_geometry` 对连续模式给 `None`（它没有「一屏」
/// 可归一），而 `claim_swipe` 把 `None` 读成 `Refused` → `Swallow`，走过 16px（`SLOP`）
/// 之后每一个增量都不再落到 `scroll_y` 上。读者看到的是「选了上下滚动，往下滑没反应」——
/// 设置里拿得到的一档，手上完全用不了。
///
/// 走真机那条路（原始触摸 → 识别器 → 滚动事件），不直发 `window.scroll`：后者没有
/// `Started` 那一格，`swipe` 一直是 `None`，正好绕开要测的这一段。
#[gpui_kit::test]
fn a_vertical_drag_scrolls_in_continuous_mode(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open_scroll(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    settle(cx, handle);
    let before = view.read_with(cx, |v, _| v.scroll_y);

    // 往上拖两格，一共 200px（远过 `SLOP`）：读书顺序里就是往后滚。
    raw_touch_at(cx, handle, TouchPhase::Started, point(px(200.0), px(400.0)));
    raw_touch_at(cx, handle, TouchPhase::Moved, point(px(200.0), px(300.0)));
    raw_touch_at(cx, handle, TouchPhase::Moved, point(px(200.0), px(200.0)));
    let mid = view.read_with(cx, |v, _| v.scroll_y);
    assert!(
        mid > before + 1.0,
        "往上拖 200px 正文该跟着滚：{before:.1} → {mid:.1}"
    );

    raw_touch_at(cx, handle, TouchPhase::Ended, point(px(200.0), px(200.0)));
    let after = view.read_with(cx, |v, _| v.scroll_y);
    assert!(
        after >= mid,
        "松手只该继续往前走，不该把位置退回来：{mid:.1} → {after:.1}"
    );
}

// ---- 整页底图章：章间距与读数带的判据 ----

/// 两张满版底图上下相接时，中间那一格章间距要收成 0 —— 否则屏幕上就是一条白带。
///
/// 两条链成员都是 `Document::is_picture_chapter` 为真的章；判据与累计公式在
/// `book.rs` 的 `stack`，这条门走的是真实排版之后 `Book::placements` 给出的坐标。
#[gpui_kit::test]
fn two_full_page_chapters_are_stacked_without_a_seam(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = FlowDoc {
        chapters: vec![
            Chapter::inline("卷首一", backdrop_chapter("<p>&#160;</p>")),
            Chapter::inline("卷首二", backdrop_chapter("<p>&#160;</p>")),
        ],
        ..Default::default()
    };
    let (view, handle) = open_scroll(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
    settle(cx, handle);
    let (placements, gap) = view.read_with(cx, |v, _| {
        let book = v.book().expect("这本书该开着");
        let gap = Book::chapter_gap(&v.prefs);
        (book.placements(gap), gap)
    });
    assert_eq!(placements.len(), 2, "两章都该进链：{placements:?}");
    let (first, second) = (&placements[0], &placements[1]);
    assert!(
        first.height > 0.0 && second.height > 0.0,
        "两章都该排过：{placements:?}"
    );
    assert!(
        (second.base_y - (first.base_y + first.height)).abs() < 0.5,
        "两张满版图之间不许留章间距（gap={gap}）：{} vs {}",
        second.base_y,
        first.base_y + first.height
    );
}

/// 上一章的正文底边不许越过**下一章盒子的顶**：那是整页底图铺过来的地方。
///
/// 章盒从前只量排版坐标（`Document::extent`），而绘制把每一章的字都从盒顶又往下挪了
/// `content_top`（页眉带 + 系统安全区）。于是每章末尾都有一截字落在盒外，下一章的
/// 整页底图从盒顶铺上来正好盖住它 —— 2026-11-01 真机《诡秘之主》第 3 节末段
/// 「此书是好友小贤指定要做的…」被第 4 节的 `fm1.jpg` 压住，量到的差就是一个
/// `content_top`。这条门量的是**画出来的**底边（章基线 + content_top + 最后一行底），
/// 不是排版自报的那个数。
#[gpui_kit::test]
fn the_last_line_of_a_chapter_stays_inside_its_box(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = FlowDoc {
        chapters: vec![
            Chapter::inline("正文", "<p>第一段</p><p>第二段</p><p>第三段</p>"),
            Chapter::inline("卷首", backdrop_chapter("<p>&#160;</p>")),
        ],
        ..Default::default()
    };
    let (view, handle) = open_scroll(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
    view.update(cx, |v, cx| {
        v.set_chrome_bands(28.0, 28.0, 0.0, cx);
        v.insets.top = 24.0;
    });
    settle(cx, handle);
    let (placements, top, ink) = view.read_with(cx, |v, _| {
        let book = v.book().expect("这本书该开着");
        let gap = Book::chapter_gap(&v.prefs);
        let doc = book.span_doc(0).expect("第一章该排过");
        let ink = doc
            .layout()
            .lines
            .iter()
            .map(|line| line.y + line.height)
            .fold(0.0f32, f32::max);
        (book.placements(gap), book.chapter_top(), ink)
    });
    assert_eq!(placements.len(), 2, "两章都该进链：{placements:?}");
    assert!(
        top > 0.0 && (top - (28.0 + 24.0)).abs() < 0.5,
        "章盒里该算进页眉带 + 安全区（28 + 24），实得 {top}"
    );
    let (first, second) = (&placements[0], &placements[1]);
    let ink_bottom = first.base_y + top + ink;
    assert!(
        ink_bottom <= second.base_y + 0.5,
        "上一章最后一行画到 {ink_bottom}，越过了下一章盒顶 {} —— 下一章的整页底图会盖住它",
        second.base_y
    );
}

/// 整页画的读数带一律透明，**与这一章有没有正文无关**（2026-11-01 深夜改回）。
///
/// 2026-11-01 深夜读者报的：《诡秘之主》第 6 节 `body.fm1`（固定底图 + 人物志二十来屏）
/// 滚到纯文字那一屏也是全屏 —— 页眉那行章名直接压在正文上、正文压进页脚。判据因此从
/// 「这一屏有没有字」换成「这一章有没有**可读**的正文」：卷首那种 `<p>&#160;</p>` 撑开
/// 的仍算没有正文，带子照样透明。
#[gpui_kit::test]
fn a_full_bleed_chapter_keeps_its_bands_transparent_with_or_without_text(cx: &mut TestAppContext) {
    use gpui_kit::test::TestWindowExt as _;

    cx.update(gpui_kit::init);
    for (title, body, want_bare) in [
        ("卷首", "<p>&#160;</p>", true),
        ("人物志", "<p>Character Introduction</p>", true),
    ] {
        let flow = FlowDoc {
            chapters: vec![Chapter::inline(title, backdrop_chapter(body))],
            ..Default::default()
        };
        let (view, handle) =
            open_scroll(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
        assert!(
            view.read_with(cx, |v, _| v
                .book()
                .expect("有书")
                .document()
                .is_picture_chapter()),
            "前置：「{title}」这张底图要被认成整页画，否则透明带子不是这条判据给的"
        );
        view.update(cx, |v, cx| v.set_chrome_bands(28.0, 28.0, 0.0, cx));
        settle(cx, handle);
        let bare = cx
            .update_window(handle, |_, window, _| {
                window.try_find("reader-header-band-bare").is_some()
                    && window.try_find("reader-header-band").is_none()
            })
            .expect("窗口应能渲染");
        assert_eq!(
            bare, want_bare,
            "整页底图章「{title}」的读数带透明与否（正文 {body:?}）"
        );
    }
}

/// 有字、带整页底图的章，正文按版心坐标画 —— 命中换算不许再补一次 `content_top`。
///
/// 从前 `chapter_is_picture_page` 把「固定底图章」也算了进去，而那一档的正文其实
/// 从版心顶起画；于是手指落在一行字上，命中的是往下 `content_top` 处（真机上
/// 两百多像素、好几行）的另一行。这条门用 `top_inset = 24` 把两个坐标系拉开。
#[gpui_kit::test]
fn hitting_a_captioned_backdrop_chapter_follows_the_text(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = FlowDoc {
        chapters: vec![Chapter::inline(
            "人物志",
            backdrop_chapter("<p>Character Introduction</p>"),
        )],
        ..Default::default()
    };
    let (view, handle) = open_scroll(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
    view.update(cx, |v, cx| {
        v.set_chrome_bands(28.0, 28.0, 0.0, cx);
        v.insets.top = 24.0;
    });
    settle(cx, handle);
    let (at, want_y) = view.read_with(cx, |v, _| {
        let line = &v.lines()[0];
        let origin = v.content_bounds.get().origin;
        let mid = line.y + line.height * 0.5;
        let at = point(
            origin.x + px(v.prefs.margin_x + 4.0),
            origin.y + px(v.page_box.content.y + mid - v.scroll_painted),
        );
        (at, mid)
    });
    let got = cx
        .update_window(handle, |_, window, cx| {
            view.read_with(cx, |v, _| v.to_layout(window, at))
        })
        .expect("窗口应能渲染");
    let (chapter, _x, y) = got.expect("该落进这一章");
    assert_eq!(chapter, 0);
    assert!(
        (y - want_y).abs() < 1.0,
        "命中该落在手指那一行（{want_y}），实得 {y}"
    );
}

/// 手机比例（384×853）下《诡秘之主》头两章各占多大一格。
///
/// 桌面的 520 高复现不出来这一档：整页图的选源按**整窗**宽高比挑（`0.jpg` 4:3 旁边
/// 摆着 `0~slim.jpg` 1080×2400，只有 0.45 这一档会挑到后者），而排版给这一章的盒子
/// 高与绘制铺贴用的矩形必须对得上 —— 从前盒子按内容区高（735）而图按整屏比例挑，
/// `cover` 于是把封面上半截（LORD OF MYSTERIES / 诡秘之主 / THE CLOWN 那一整块标题）
/// 推出屏外，下一章的黑底正好接在被剪掉的那一截上（真机量到：封面只剩 229..1467）。
#[gpui_kit::test]
fn a_phone_aspect_gives_the_cover_and_its_colophon_one_screen_each(cx: &mut TestAppContext) {
    use folio_core::content::format::ImageRes;

    cx.update(gpui_kit::init);
    let cover = "<html><body><div style=\"text-align: center; padding: 0pt; margin: 0pt;\">\
        <svg xmlns=\"http://www.w3.org/2000/svg\" height=\"100%\" \
        preserveAspectRatio=\"xMidYMid meet\" version=\"1.1\" viewBox=\"0 0 1000 1333\" \
        width=\"100%\" xmlns:xlink=\"http://www.w3.org/1999/xlink\">\
        <image width=\"1000\" height=\"1333\" xlink:href=\"../Images/0.jpg\"/></svg>\
        </div></body></html>";
    let colophon = "<html><head><style>\
         body{margin:0;padding:0}\
         body.zhizuobA1{background:#fff no-repeat center;background-size:cover;\
             background-attachment:fixed;background-repeat:no-repeat;\
             background-position:bottom center;background-image:url('../Images/p0.jpg')}\
         div.zhizuodA1{margin:2em 3em 0em 3em;padding:1em 0em;background-color:#f7faf9;\
             border:1px solid #000;border-radius:28px}\
         h2,p{text-align:center}\
         </style></head><body class=\"zhizuobA1\"><div class=\"zhizuodA1\">\
         <h2>制作说明</h2><p>诡秘之主</p><p>爱潜水的乌贼◎著</p></div></body></html>";
    let image = |href: &str, w: u32, h: u32| ImageRes {
        href: href.into(),
        source: href.into(),
        media_type: "image/jpeg".into(),
        width: w,
        height: h,
    };
    let flow = FlowDoc {
        chapters: vec![
            Chapter::inline("封面", cover).with_spine_href("OEBPS/Text/cover.xhtml"),
            Chapter::inline("制作说明", colophon).with_spine_href("OEBPS/Text/top001.xhtml"),
        ],
        images: vec![
            image("OEBPS/Images/0.jpg", 1000, 1333),
            image("OEBPS/Images/0~slim.jpg", 1080, 2400),
            image("OEBPS/Images/p0.jpg", 1080, 2400),
        ],
        ..Default::default()
    };
    let (view, handle) = open_scroll_sized(
        cx,
        &flow,
        "epub",
        prefs(16.0),
        Locator::start(),
        384.0,
        853.0,
    );
    view.update(cx, |v, cx| v.set_chrome_bands(77.5, 40.6, 0.0, cx));
    settle(cx, handle);
    let chain = view.read_with(cx, |v, _| {
        let book = v.book().expect("这本书该开着");
        let gap = Book::chapter_gap(&v.prefs);
        book.placements(gap)
            .iter()
            .map(|p| {
                let doc = book.span_doc(p.index).expect("进链的章该有排版");
                (
                    p.index,
                    p.base_y,
                    p.height,
                    doc.is_picture_only_page(),
                    // 根上那张 `fixed` 底图撑到多高：绘制侧按整窗铺，这一格就得装得下
                    doc.decor_seeds()
                        .iter()
                        .find(|seed| seed.full_bleed)
                        .map_or(0.0, |seed| seed.height),
                )
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(chain.len(), 2, "两章都该进链：{chain:?}");
    let (cover_base, cover_h) = (chain[0].1, chain[0].2);
    let (colophon_base, colophon_h, backdrop_h) = (chain[1].1, chain[1].2, chain[1].4);
    assert!(chain[0].3, "封面那一章要被认成整页只有一张图：{chain:?}");
    assert!(
        (colophon_base - (cover_base + cover_h)).abs() < 0.5,
        "制作说明该接在封面之后（不留章间距）：{colophon_base} vs {cover_base}+{cover_h}"
    );
    assert!(
        colophon_h >= 853.0 - 0.5 && backdrop_h >= 853.0 - 0.5,
        "带整屏底图的那一章要占满一屏，否则图会被剪：章高 {colophon_h}，底图 {backdrop_h}"
    );
    // **盒子与铺贴同一个数**：整章只有一张图的那一章，排版按纸高（= 窗口高 − 外壳高，
    // 测试台外壳高是 0 → 853）撑，绘制侧也按这一格铺满。从前盒子只按内容区高
    // （853 − 页眉带 77.5 − 页脚带 40.6 = 735）而背板按整窗铺，多出来的 118 压在下一章
    // 头上，而下一章的正文画在背板之上 —— 真机上的读法就是「制作说明那张卡的白顶盖在
    // 封面上」。
    assert!(
        cover_h >= 853.0 - 0.5,
        "封面那一章要占满一张纸（853），否则整窗铺贴的背板会溢进下一章：实得 {cover_h}"
    );
    assert!(
        colophon_base + 77.5 >= cover_h - 0.5,
        "下一章的正文顶（基线 {colophon_base} + content_top 77.5）不许落在封面那张纸里"
    );
}

/// 可见带要一直覆盖到**内容区底边**：屏底那两行不许漏画。
///
/// 2026-11-01 真机（Meizu 20，《哈利·波特全集》第 119 节）：正文在页脚带上方空出两行多，
/// 看着像「上下边距设 0 了还是很大」。根子是那一趟把 `content_top` 在**可见带**里也减了
/// 一次 —— 绘制式子是 `base + content_top + line.y − scroll`，两边同时有它，减一次就整体
/// 上移一截。图片行高、只吃掉下半截看不出来；文字行矮，一屏少两行。
#[gpui_kit::test]
fn the_visible_band_reaches_the_bottom_of_the_content_area(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let text = "把这一章撑过一屏的正文，用来量可见带的下沿。".repeat(200);
    let flow = FlowDoc {
        chapters: vec![Chapter::inline("正文", format!("<p>{text}</p>"))],
        ..Default::default()
    };
    let (view, handle) = open_scroll_sized(
        cx,
        &flow,
        "markdown",
        prefs(16.0),
        Locator::start(),
        384.0,
        853.0,
    );
    view.update(cx, |v, cx| v.set_chrome_bands(28.0, 28.0, 24.0, cx));
    settle(cx, handle);
    let (ink_bottom, content_bottom, pitch) = view.read_with(cx, |v, _| {
        let book = v.book().expect("这本书该开着");
        let doc = book.document();
        let gap = Book::chapter_gap(&v.prefs);
        let base = book.base_of(0, gap).unwrap_or(0.0);
        let top = v.page_box.content.y;
        let viewport = v.laid_out.viewport;
        let band = super::super::local_band(v.scroll_painted..(v.scroll_painted + viewport), base);
        let visible = doc.visible_lines(band);
        let lines = doc.layout().lines.as_slice();
        let bottom = visible
            .clone()
            .map(|ix| lines[ix].y + lines[ix].height)
            .fold(0.0f32, f32::max);
        let pitch = visible
            .clone()
            .map(|ix| lines[ix].height)
            .fold(0.0f32, f32::max);
        (
            base + top + bottom - v.scroll_painted,
            top + viewport,
            pitch,
        )
    });
    assert!(
        content_bottom - ink_bottom < pitch + 0.5,
        "可见带该盖到内容区底边：末行底 {ink_bottom}，内容区底 {content_bottom}，行高 {pitch}"
    );
}

/// 底图**没写** `fixed`、但这一章一个字都没有（扉页）→ 也算整页画：读数带透明。
///
/// 《哈利·波特全集》`v1.xhtml` 就是这一种：`body` 上 `background:url(v1.jpg)`,只加了
/// `background-size: cover`,没有 `background-attachment: fixed`。从前整页画只认 `fixed`,
/// 这一章掉进正文页,顶上顶着一条不透明页眉带、图从它下面起 —— 读者报的「封面没有全屏,
/// 上面空出来一大片白」。
///
/// 判据是「这一章有没有可读正文」,不是「这一屏有没有字」:按屏判会让带子在滚动中忽隐忽现。
#[gpui_kit::test]
fn a_textless_page_with_a_plain_background_goes_full_bleed(cx: &mut TestAppContext) {
    use gpui_kit::test::TestWindowExt as _;

    cx.update(gpui_kit::init);
    for (body, want_bare) in [("<p>&#160;</p>", true), ("<p>扉页上的一行字</p>", false)] {
        let flow = FlowDoc {
            chapters: vec![Chapter::inline("扉页", plain_backdrop_chapter(body))],
            ..Default::default()
        };
        let (view, handle) =
            open_scroll(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
        view.update(cx, |v, cx| v.set_chrome_bands(28.0, 28.0, 0.0, cx));
        settle(cx, handle);
        let bare = cx
            .update_window(handle, |_, window, _| {
                window.try_find("reader-header-band-bare").is_some()
                    && window.try_find("reader-header-band").is_none()
            })
            .expect("窗口应能渲染");
        assert_eq!(bare, want_bare, "扉页（正文 {body:?}）的读数带");
    }
}

/// 上面那一条只测了扉页**正好是锚章**的那一屏。真机 2026-11-02《哈利·波特全集》第 6 节
/// `v1.xhtml` 停在的是邻章位置（视口顶还在 `toc1`，页眉写着「目录」），而那一屏量出来是：
/// 底图只铺了版心那 990×771 物理像素（左右各让 45、上下各被裁 489），整窗背板根本没画。
///
/// 所以这一条把同一个形状挪到邻章上，问绘制那一帧真正会问的两个问题：
/// [`super::super::art::page_backdrop`]（版心里那份要不要跳过）与 [`super::super::art::page_art_for_doc`]
/// （整窗背板有没有得画）。
#[gpui_kit::test]
fn a_plain_backdrop_neighbour_chapter_is_still_a_picture_chapter(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = FlowDoc {
        chapters: vec![
            Chapter::inline(
                "目录",
                (0..40)
                    .map(|i| format!("<p>目录第{i}行</p>"))
                    .collect::<String>(),
            ),
            Chapter::inline("扉页", plain_backdrop_chapter("<p>&#160;</p>")),
        ],
        ..Default::default()
    };
    let (view, handle) = open_scroll(cx, &flow, "markdown", prefs(16.0), Locator::start(), 384.0);
    view.update(cx, |v, cx| v.set_chrome_bands(28.0, 28.0, 0.0, cx));
    settle(cx, handle);
    let (anchor, backdrop, art) = view.read_with(cx, |v, _| {
        let book = v.book().expect("这本书该开着");
        let doc = book.span_doc(1).expect("邻章该已排版");
        (
            book.chapter_index(),
            super::super::art::page_backdrop(doc).is_some(),
            super::super::art::page_art_for_doc(1, doc, 384.0 / 853.0).is_some(),
        )
    });
    assert_eq!(anchor, 0, "视口顶该还停在目录，扉页是邻章");
    assert!(backdrop, "邻章的根底图也该认成整页底图（版心里那份要跳过）");
    assert!(art, "邻章也该有一张铺满整窗的背板");
}

/// manifest 里的一张图：`href` 与容器路径同一格，尺寸按真书量出来的填。
fn image(href: &str, width: u32, height: u32) -> folio_core::content::format::ImageRes {
    folio_core::content::format::ImageRes {
        href: href.into(),
        source: href.into(),
        media_type: "image/jpeg".into(),
        width,
        height,
    }
}

/// `v1.xhtml` 那一族的四种写法，逐一样本地问同一个问题：根底图认不认得出来。
///
/// 合成语料（[`plain_backdrop_chapter`]）是长手写的 `background-image:` + `margin:0`，
/// 真书用的是 `style="background: url(..) no-repeat center"` 这一条**简写**，还带
/// `body{margin:0% 1%}` 与两张透明标题图。红在哪一格，就说明整页画的判据漏在哪一格。
#[gpui_kit::test]
fn every_plain_backdrop_spelling_is_recognised(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let head = "<html><head><style>html,body{height:100%}";
    let tail = "</style></head>";
    for (name, source) in [
        (
            "长手写 + margin:0（合成语料那一格）",
            format!(
                "{head}body{{margin:0;padding:0;background-image:url(v1.jpg);\
                 background-size:cover;background-repeat:no-repeat;background-position:center}}\
                 {tail}<body><p>&#160;</p></body></html>"
            ),
        ),
        (
            "简写 background:（v1.xhtml 原文那一格）",
            format!(
                "{head}body{{margin:0;padding:0}}{tail}\
                 <body style=\"background: url(v1.jpg) no-repeat center; background-size: cover;\">\
                 <p>&#160;</p></body></html>"
            ),
        ),
        (
            "简写 + body margin 1%（main.css 那一格）",
            format!(
                "{head}body{{padding:0%;margin:0% 1% 0% 1%;line-height:100%}}{tail}\
                 <body style=\"background: url(v1.jpg) no-repeat center; background-size: cover;\">\
                 <p>&#160;</p></body></html>"
            ),
        ),
        (
            "简写 + margin 1% + 两张标题图（v1.xhtml 全套）",
            format!(
                "{head}body{{padding:0%;margin:0% 1% 0% 1%;line-height:100%}}\
                 .img{{margin:1em 10px 10px 10px;text-align:center}}\
                 .img0{{margin:10px;text-align:center}}{tail}\
                 <body style=\"background: url(v1.jpg) no-repeat center; background-size: cover;\">\
                 <div style=\"margin:-15px 0px -10px 0px;\" class=\"img0\">\
                 <img alt=\"title\" src=\"hptitle.png\" width=\"90%\"/></div>\
                 <h1 class=\"img\"><img alt=\"hpt1\" src=\"hpt1.png\" width=\"100%\"/></h1>\
                 </body></html>"
            ),
        ),
    ] {
        let flow = FlowDoc {
            chapters: vec![
                Chapter::inline(
                    "目录",
                    (0..40)
                        .map(|i| format!("<p>目录第{i}行</p>"))
                        .collect::<String>(),
                ),
                Chapter::inline("扉页", source),
            ],
            images: vec![
                image("v1.jpg", 792, 1400),
                image("hptitle.png", 736, 433),
                image("hpt1.png", 700, 81),
            ],
            ..Default::default()
        };
        let (view, handle) =
            open_scroll(cx, &flow, "markdown", prefs(16.0), Locator::start(), 384.0);
        view.update(cx, |v, cx| v.set_chrome_bands(28.0, 28.0, 0.0, cx));
        settle(cx, handle);
        let (backdrop, art, seeds, has_text) = view.read_with(cx, |v, _| {
            let book = v.book().expect("这本书该开着");
            let doc = book.span_doc(1).expect("邻章该已排版");
            (
                super::super::art::page_backdrop(doc).is_some(),
                super::super::art::page_art_for_doc(1, doc, 384.0 / 853.0).is_some(),
                doc.decor_seeds()
                    .iter()
                    .map(|s| (s.x, s.y, s.width, s.height, s.pictures.len(), s.full_bleed))
                    .collect::<Vec<_>>(),
                doc.has_text(),
            )
        });
        assert!(
            backdrop && art,
            "{name}：backdrop={backdrop} art={art} has_text={has_text} seeds={seeds:?}"
        );
    }
}
