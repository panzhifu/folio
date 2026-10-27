//! 摆法（单页 / 双页 / 退档）：选择、宽度退档与位置迁移。

use super::harness::{
    flow, meta, offset_of, open, open_scroll, open_with_store, prefs, scroll_down, scroll_mode,
    settle,
};
use crate::ui::features::reader::model::chrome::PageMode;
use crate::ui::features::reader::model::turn::TurnIntent;
use crate::ui::features::reader::motion::turn_animation::Axis;
use crate::ui::services::config::ConfigHolder;
use folio_core::store::Store;
use folio_core::{Format, Locator, PageTurn};
#[allow(unused_imports)] // `#[gpui_kit::test]` 展开要它，unused 分析看不见宏里的使用
use gpui_kit::{AppContext as _, TestAppContext};
use std::sync::Arc;

/// 默认档只替「从没选过」的人决定：库里存过档位，宽窗口也不许改他的选择。
/// 反过来（没存过 → 1200 宽给双页）由上面那条 `a_spread_is_two_contiguous...` 钉。
#[gpui_kit::test]
fn a_stored_mode_beats_the_wide_screen_default(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let store = Arc::new(Store::in_memory().expect("内存库"));
    let id = store
        .add_book(std::path::Path::new("memory://fixture"), &meta("markdown"))
        .expect("入库");
    store
        .save_flow(id, &flow, Format::Markdown)
        .expect("章节入库");
    store
        .set_position(id, PageMode::Continuous.storage_name(), &Locator::start())
        .expect("写入进度");

    let (view, handle) = open_with_store(
        cx,
        &flow,
        "markdown",
        prefs(16.0),
        Locator::start(),
        1200.0,
        Some(store),
        Some(id),
    );
    scroll_mode(cx, handle);
    settle(cx, handle);
    assert_eq!(
        view.read_with(cx, |v, _| v.mode()),
        PageMode::Continuous,
        "读者存过的那一档优先，默认档不能覆盖它"
    );
}

/// 双页成立的那两条判据：一页确实是**两栏**，而且两栏首尾相接 —— 右栏的第一行
/// 紧跟左栏的最后一行，读者按「先左后右」读不会跳行。
#[gpui_kit::test]
fn a_spread_is_two_contiguous_columns_of_one_sheet(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 1200.0);
    assert_eq!(
        view.read_with(cx, |v, _| v.mode()),
        PageMode::Spread,
        "1200 宽的窗口没选过档，默认就该是双页（读者选过的照旧，见 set_mode）"
    );

    settle(cx, handle);
    assert_eq!(
        view.read_with(cx, |v, _| v.mode()),
        PageMode::Spread,
        "1200 宽的窗口该放得下两栏"
    );
    let pages = view.read_with(cx, |v, _| v.page_count());
    assert!(pages > 2, "长一章双页也该有多页，实际 {pages}");

    let left = view
        .read_with(cx, |v, _| v.sheet_range(0, 0))
        .expect("第 0 页左栏应有内容");
    let right = view
        .read_with(cx, |v, _| v.sheet_range(0, 1))
        .expect("第 0 页右栏应有内容");
    assert!(!left.is_empty() && !right.is_empty(), "两栏都要有行");
    assert_eq!(
        left.end, right.start,
        "右栏该从左栏断的那一行接着排，不能重叠也不能留缝"
    );

    // 一页一号：翻一页 = 翻一张纸（两栏一起走），偏移该往前走并且不超过两栏的行数。
    let before = offset_of(&view, cx);
    view.update(cx, |v, cx| v.scroll_page(1, cx));
    settle(cx, handle);
    assert_eq!(view.read_with(cx, |v, _| v.page()), 1);
    let after = offset_of(&view, cx);
    assert!(after > before, "翻一张纸该往前走，实际 {before} → {after}");
}

/// 双页里「一屏」有多长**按档分**：只有仿真折页量靠手指那一张 leaf，平移 / 上下平移 / 覆盖量
/// 整块版心。
///
/// 判据与它的理由在 [`turn_animation::paper_area`]：翻一趟要换掉的是**两栏**的字，而单面的一张纸
/// 只带得动一栏 —— 所以那三档在双页里整屏一起走。取错的两种读法都不体面：折页按整块量 → 「纸走
/// 半屏就落定，手指还在整屏里划」；平移/覆盖按半块量 → 「拖到一半纸才走一半」，而另一半原地跳变
/// （读者那句「左右覆盖只能覆盖双页其中一页」量的就是这个）。
#[gpui_kit::test]
fn only_the_curl_measures_one_leaf_in_a_spread(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 1200.0);
    view.update(cx, |v, cx| v.set_mode(PageMode::Spread, cx));
    for (turn, axis) in [
        (PageTurn::Slide, Axis::Horizontal),
        (PageTurn::SlideUp, Axis::Vertical),
        (PageTurn::Cover, Axis::Horizontal),
        (PageTurn::Curl, Axis::Horizontal),
    ] {
        cx.update(|app| {
            ConfigHolder::edit_appearance(app, |a| a.page_turn = turn);
        });
        settle(cx, handle);
        assert_eq!(
            view.read_with(cx, |v, _| v.mode()),
            PageMode::Spread,
            "{turn:?} 这一趟里摆法被改走了"
        );
        let area = view.read_with(cx, |v, _| v.page_area);
        let block = area.distance(axis);
        assert!(block > 0.0, "夹具量不出版心，这条测不到东西");
        // 这本是 LTR：往前翻自由边在右，往回翻在左。
        for (intent, right) in [(TurnIntent::Next, true), (TurnIntent::Previous, false)] {
            let leaf = area.leaf(right).distance(axis);
            // 横轴上两种读法必须量得出差别，否则这一条对那一档是空转的（上下平移沿纵轴，
            // leaf 与整块同高 —— 那一档的答案由 `paper_area` 自己的表钉）。
            if axis == Axis::Horizontal {
                assert!(
                    leaf > 0.0 && block - leaf > 1.0,
                    "夹具里 leaf（{leaf}）与整块（{block}）分不开，这一档测不到东西"
                );
            }
            let want = if turn == PageTurn::Curl { leaf } else { block };
            let (_, dist, _) = view
                .read_with(cx, |v, cx| v.swipe_geometry(cx, axis, Some(intent)))
                .unwrap_or_else(|| panic!("{turn:?} 接不住自己那根轴（{axis:?}）上的划动"));
            assert!(
                (dist - want).abs() < 1.0,
                "{turn:?} 的 {intent:?} 一屏该是 {want}（leaf {leaf} / 整块 {block}），实得 {dist}"
            );
        }
    }
}

/// 窄窗口点「双页」不该得到两条窄柱子：按宽度退回分页。
/// 这条判据按宽度而不是按 target，所以横屏平板能真双页、竖屏手机自动单页。
#[gpui_kit::test]
fn a_narrow_window_falls_back_from_spread_to_one_column(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
    view.update(cx, |v, cx| v.set_mode(PageMode::Spread, cx));
    settle(cx, handle);

    assert_eq!(
        view.read_with(cx, |v, _| v.mode()),
        PageMode::Paged,
        "460 宽放不下两栏，该退回分页"
    );
    // 读者**选**的仍然是双页：按钮的选中态读它，不然一退档那格就自己弹回去了。
    assert_eq!(view.read_with(cx, |v, _| v.wanted_mode()), PageMode::Spread);
    // 只有一栏，所以第 1 栏不存在。
    assert!(
        view.read_with(cx, |v, _| v.sheet_range(0, 1)).is_none(),
        "退档之后不该还剩一条空栏"
    );
}

/// 换摆法不换位置：连续滚到某个字，切双页（宽窗）再切回来，还在同一个字上。
/// 进度是「第几个字」，模式只是它的读数方式 —— 这条要是红了，读者的位置就会在
/// 换「翻页方式」时飘掉。
///
/// 两条换档入口都必须走它：读者亲手选的那一档（`set_mode` / 双页那颗）与渲染帧按
/// 宽度 / 翻页方式解出来的那一档（`adopt_mode`）走同一条位置换算。从前渲染帧直接
/// 赋 `self.mode`，于是「上下滚动读到一半换回仿真翻页」落在章首 —— 这条测的就是
/// 那一段。
#[gpui_kit::test]
fn switching_modes_keeps_the_same_character(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    // 起点是**上下滚动**：这一档现在只有「翻页方式」进得去，所以建会话时就得摆好。
    let (view, handle) = open_scroll(cx, &flow, "markdown", prefs(16.0), Locator::start(), 1200.0);
    settle(cx, handle);
    assert_eq!(view.read_with(cx, |v, _| v.mode()), PageMode::Continuous);
    scroll_down(cx, handle, 1200.0);
    let wanted = offset_of(&view, cx);
    assert!(wanted > 0, "先滚到一个字上，实际 {wanted}");

    // 选一张纸的走法：渲染帧的 `reconcile` 把摆法带到按宽度该长的双页。
    cx.update(|app| {
        ConfigHolder::edit_appearance(app, |a| a.page_turn = PageTurn::Slide);
    });
    settle(cx, handle);
    assert_eq!(
        view.read_with(cx, |v, _| v.mode()),
        PageMode::Spread,
        "1200 宽选纸面动画该立起双页"
    );
    let in_spread = offset_of(&view, cx);
    // 落点是「覆盖那个字的那张纸的开头」，所以它只能等于或早于原位置，
    // 且不能早过一页之内的距离。
    assert!(
        in_spread <= wanted,
        "落位不该越过原来那一行：{wanted} → {in_spread}"
    );
    assert!(in_spread > 0, "换档不该把位置丢回章首");

    // 切回上下滚动：`reconcile` 把摆法带回连续，落点仍是双页那一屏的开头那个字。
    scroll_mode(cx, handle);
    assert_eq!(view.read_with(cx, |v, _| v.mode()), PageMode::Continuous);
    assert_eq!(
        offset_of(&view, cx),
        in_spread,
        "切回连续滚动该落在双页那一屏的开头那个字上"
    );
}
