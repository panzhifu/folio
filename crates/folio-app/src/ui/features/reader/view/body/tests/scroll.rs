//! 连续滚动：章界流动、脊柱窗口、章高与视口。

use super::harness::{
    PARA, chapters_flow, flow, meta, open, open_scroll, open_scroll_with_store, prefs, progress_of,
    raw_touch_at, scroll_down, scroll_mode, settle,
};
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
