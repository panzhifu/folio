//! 跟手翻页手势：认领、跟手、弹回、惯性与轴拒绝。

use super::harness::{
    backdrop_chapter, flow, open, prefs, raw_touch, raw_touch_at, settle, turn_frame, wheel_lines,
};
use crate::ui::features::reader::model::chrome::PageMode;
use crate::ui::features::reader::model::swipe::{FLING_SPEED, SNAP_FRACTION, fling_floor};
use crate::ui::features::reader::motion::turn_animation::Axis;
use crate::ui::services::config::ConfigHolder;
use crate::ui::shell::NAV_TRANSITION;
use folio_core::{Chapter, FlowDoc, Locator, PageTurn};
use gpui_kit::{TestAppContext, TouchPhase, point, px};
use std::time::Duration;

/// 横扫过半：跟手起来 → 页号在松手前一个字不动 → 松手那一刻翻过去、剩下那截走完。
#[gpui_kit::test]
fn a_horizontal_swipe_follows_the_finger_and_lands(cx: &mut TestAppContext) {
    use crate::ui::features::reader::model::session::TurnRun;

    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    settle(cx, handle);
    let first = view.read_with(cx, |v, _| v.page());

    raw_touch(cx, handle, TouchPhase::Started, 360.0);
    raw_touch(cx, handle, TouchPhase::Moved, 240.0);
    let following = view.read_with(cx, |v, _| v.turn);
    assert!(
        matches!(following, Some(turn) if matches!(turn.run, TurnRun::Following { .. })),
        "横扫过阈值该进入跟手，实得 {following:?}"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        first,
        "跟手期间页号一个字都不动 —— 弹回时才不必撤销任何东西"
    );
    assert_ne!(
        view.read_with(cx, |v, _| v.page_shift),
        (0.0, 0.0),
        "跟手就是纸真的跟着手指走了，不是只挂了个状态"
    );

    raw_touch(cx, handle, TouchPhase::Moved, 60.0);
    raw_touch(cx, handle, TouchPhase::Ended, 60.0);
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        first + 1,
        "走过大半屏，松手那一下就该翻页"
    );
    turn_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));
    assert!(view.read_with(cx, |v, _| v.turn.is_none()));
    assert_eq!(view.read_with(cx, |v, _| v.page_shift), (0.0, 0.0));
}

/// 浮层（顶栏底栏）开着时，正文上的翻页输入整体让位：跟手横扫不翻页，滚轮一格也不再是
/// 一页 —— 那些输入的意思是「收浮层」。分区点击那一半钉在 `chrome::Chrome` 的
/// `handle_body_tap` 里；这里钉的是手势与滚轮这两半，以及「收了浮层，同样的输入回到
/// 正轨」—— 证明挡住的是这一面旗，不是手势路坏了。连续滚动不在这一条里：那一路
/// 没有「翻页」这个动作，手指滚的就是正文本身。
///
/// 从前这些输入在工具栏底下照翻：读者以为这一下在收浮层，纸却已经换了页。
#[gpui_kit::test]
fn body_turn_inputs_yield_while_the_overlay_is_up(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    settle(cx, handle);
    let first = view.read_with(cx, |v, _| v.page());

    view.update(cx, |v, cx| v.set_overlay_open(true, cx));
    // 与真机同一条事件路：识别器照跑，翻页层不认领 —— 跟手腿一帧都不该有。
    raw_touch(cx, handle, TouchPhase::Started, 360.0);
    raw_touch(cx, handle, TouchPhase::Moved, 240.0);
    assert!(
        view.read_with(cx, |v, _| v.turn.is_none()),
        "浮层开着时不该挂起一趟跟手翻页"
    );
    raw_touch(cx, handle, TouchPhase::Moved, 60.0);
    raw_touch(cx, handle, TouchPhase::Ended, 60.0);
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        first,
        "浮层开着时横扫到松手都不该翻页"
    );
    wheel_lines(cx, handle, -1.0);
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        first,
        "浮层开着时滚轮一格也不该翻页"
    );

    view.update(cx, |v, cx| v.set_overlay_open(false, cx));
    wheel_lines(cx, handle, -1.0);
    assert_eq!(view.read_with(cx, |v, _| v.page()), first + 1);
    raw_touch(cx, handle, TouchPhase::Started, 360.0);
    raw_touch(cx, handle, TouchPhase::Moved, 240.0);
    raw_touch(cx, handle, TouchPhase::Moved, 60.0);
    raw_touch(cx, handle, TouchPhase::Ended, 60.0);
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        first + 2,
        "收了浮层，同样的横扫照翻"
    );
}

/// 整章只有一页的那种插页（一本书里每张整页照片就是这样一个章）：横扫该**跟手**。
///
/// 从前 `claim_swipe` 在跨章那一格直接放弃跟手（理由写成「目标章还没排版」），于是这种页
/// 每一划都要等抬手由 `turn()` 补一趟自动动画 —— 真机 2026-10-22 量到读者那九划全是
/// `released=false` 的 380 ms 动画，跟手腿一帧都没有。把跨章那一格改回「不跟手直接返回」，
/// 这里第一条断言就红。
#[gpui_kit::test]
fn a_swipe_off_a_one_page_chapter_follows_the_finger(cx: &mut TestAppContext) {
    use crate::ui::features::reader::model::session::TurnRun;

    cx.update(gpui_kit::init);
    // 四章、每章一行 ⇒ 整章只有一页 ⇒ 往左那一划必是跨章那一笔。
    let short_chapters = FlowDoc {
        chapters: (0..4)
            .map(|i| Chapter::inline(format!("第{i}章"), format!("<p>短章第{i}行。</p>")))
            .collect(),
        ..Default::default()
    };
    let (view, handle) = open(
        cx,
        &short_chapters,
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    cx.update(|app| {
        ConfigHolder::edit_appearance(app, |a| a.page_turn = PageTurn::Curl);
    });
    settle(cx, handle);
    // 前置两格：夹具走样（不止一页）或邻章没排好，这条就会空跑成「章内翻页」那一档。
    assert_eq!(
        view.read_with(cx, |v, _| v.page_count()),
        1,
        "夹具要走样了：这一章不止一页，测的就不是跨章"
    );
    assert!(
        view.read_with(cx, |v, _| v.chapter_landing(1).is_some()),
        "前置：邻章该已经排好（生产上每帧预排版都会做到），否则这条测的是「没排好」那一格"
    );

    let before = view.read_with(cx, |v, _| (v.chapter_index(), v.page()));
    raw_touch(cx, handle, TouchPhase::Started, 360.0);
    raw_touch(cx, handle, TouchPhase::Moved, 240.0);
    let following = view.read_with(cx, |v, _| v.turn);
    assert!(
        matches!(following, Some(turn) if matches!(turn.run, TurnRun::Following { .. })),
        "从整页插页往左划该起跟手，实得 {following:?}"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.turn.and_then(|turn| turn.arriving)),
        Some(crate::ui::features::reader::model::session::PageRef {
            chapter: 1,
            page: 0
        }),
        "跟手这一趟要摆的第二张纸该是下一章第一页"
    );
    assert_eq!(
        view.read_with(cx, |v, _| (v.chapter_index(), v.page())),
        before,
        "跟手期间章号与页号一个字都不动"
    );
    // 折页这一档旧页由 GPU 网格画，`page_shift` 不归它管（那是 Cover / Slide 的位移），
    // 所以「跟手走了没有」读的是这一趟的推进量：手指走了 120 / 460 ≈ 0.26。
    let t = match view.read_with(cx, |v, _| v.turn.map(|turn| turn.run)) {
        Some(TurnRun::Following { t }) => t,
        other => panic!("跟手那一趟该在飞，实得 {other:?}"),
    };
    assert!(
        (0.2..0.35).contains(&t),
        "推进量该跟着手指走（约 0.26），实得 {t}"
    );

    // 走过大半屏：松手落定到下一章去（换锚章由动画走完那一刻接手，不在松手那一格）。
    raw_touch(cx, handle, TouchPhase::Moved, 60.0);
    raw_touch(cx, handle, TouchPhase::Ended, 60.0);
    turn_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));
    assert_eq!(
        view.read_with(cx, |v, _| (v.chapter_index(), v.page())),
        (1, 0),
        "走完那一刻该落在下一章第一页"
    );

    // 没过线那一划退回原地：章号一个字不动。
    raw_touch(cx, handle, TouchPhase::Started, 360.0);
    raw_touch(cx, handle, TouchPhase::Moved, 330.0);
    raw_touch(cx, handle, TouchPhase::Ended, 330.0);
    turn_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));
    assert_eq!(
        view.read_with(cx, |v, _| (v.chapter_index(), v.page())),
        (1, 0),
        "短划该弹回原地，不该把章换掉"
    );
}

/// 横扫没过那一屏的五分之一：跟手照样起来，松手弹回，页号一个字不动。
/// 这条与上面那条是一对 —— 只钉「该翻」会漏掉「不该翻」，而那半才是弹回那段动画的去处。
#[gpui_kit::test]
fn a_short_horizontal_swipe_bounces_back(cx: &mut TestAppContext) {
    use crate::ui::features::reader::model::session::TurnRun;

    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    settle(cx, handle);
    let first = view.read_with(cx, |v, _| v.page());

    raw_touch(cx, handle, TouchPhase::Started, 360.0);
    raw_touch(cx, handle, TouchPhase::Moved, 330.0);
    assert!(
        matches!(view.read_with(cx, |v, _| v.turn), Some(turn) if matches!(turn.run, TurnRun::Following { .. })),
        "三十像素过了本层的阈值，该起跟手"
    );
    raw_touch(cx, handle, TouchPhase::Ended, 330.0);
    assert_eq!(view.read_with(cx, |v, _| v.page()), first, "没过线不该翻");
    turn_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));
    assert!(
        view.read_with(cx, |v, _| v.turn.is_none()),
        "弹回走完该收掉"
    );
    assert_eq!(view.read_with(cx, |v, _| v.page_shift), (0.0, 0.0));
}

/// 短促一甩：走过的距离没到落定线，可松手那一下够快 —— 照样翻页。
///
/// `swipe::tests` 里那几条只钉判据本身；这一条钉的是**速度真从事件里量出来了**。量不出来
/// 只有「一甩不翻」这一种表现，而它与「这一律压根没生效」在屏幕上看不出差别：钟没走、
/// 取样窗口写反、抬手那一条零增量把最近一次有位移的速度抹掉，三种都红在这里。
///
/// 走的是真机那条路（原始触摸 → 识别器 → 锁了轴的 `ScrollWheelEvent`），钟按 16ms 一格推：
/// 一笔的第一格只立分母起点，所以速度来自第二格那一段。
#[gpui_kit::test]
fn a_short_but_fast_flick_lands(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    settle(cx, handle);
    let first = view.read_with(cx, |v, _| v.page());
    // 两条线都按这一屏那一档的轴长算，不写死像素：窗口或语料一改，写死的数就会
    // 悄悄落到线的那一边去，用例绿着却什么都没测。
    let (_, dist, _) = view
        .read_with(cx, |v, cx| v.swipe_geometry(cx, Axis::Horizontal, None))
        .expect("分页 + 横扫该有几何");
    let total = (fling_floor(dist) + SNAP_FRACTION * dist) * 0.5;
    assert!(
        total > fling_floor(dist) + 1.0 && total < SNAP_FRACTION * dist - 1.0,
        "前置：走的那一截要落在下限与落定线之间（轴长 {dist:.0}，取 {total:.0}）"
    );
    let step = total * 0.5;
    assert!(
        step / 0.016 >= FLING_SPEED,
        "前置：第二格那一段在 16ms 里要走够快（{:.0} dp/s）",
        step / 0.016
    );

    raw_touch(cx, handle, TouchPhase::Started, 360.0);
    // 这一格是识别器发来的第一条（`Started`，带走过 slop 的整截）：只立起点，还没有速度。
    cx.executor().advance_clock(Duration::from_millis(16));
    raw_touch(cx, handle, TouchPhase::Moved, 360.0 - step);
    // 这一格才是速度：同一段位移除以 16ms。
    cx.executor().advance_clock(Duration::from_millis(16));
    raw_touch(cx, handle, TouchPhase::Moved, 360.0 - total);
    raw_touch(cx, handle, TouchPhase::Ended, 360.0 - total);
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        first + 1,
        "走过 {total:.0}（落定线 {line:.0}）但松手 {:.0} dp/s，该由速度那一律翻页",
        step / 0.016,
        line = SNAP_FRACTION * dist,
    );
}

/// 慢拖过五分之一：不甩也翻页 —— 这一条钉的是落定线本身降到了 1/5。
///
/// 抬手前故意停 150ms，速度那一律在这一条里必然读成 0，放行它的只有距离那一律。少了这一句，
/// 「过线」可能来自速度那一支，把 `SNAP_FRACTION` 改回 1/3 也照样绿 —— 那条改动的门就空转了。
#[gpui_kit::test]
fn a_slow_drag_past_a_fifth_of_the_screen_lands(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    settle(cx, handle);
    let first = view.read_with(cx, |v, _| v.page());
    let (_, dist, _) = view
        .read_with(cx, |v, cx| v.swipe_geometry(cx, Axis::Horizontal, None))
        .expect("分页 + 横扫该有几何");
    let total = SNAP_FRACTION * dist + 4.0;
    assert!(
        total < dist / 3.0,
        "前置：走的那一截要短于旧的三分之一线，否则这一条挡不住改回去"
    );

    raw_touch(cx, handle, TouchPhase::Started, 360.0);
    cx.executor().advance_clock(Duration::from_millis(16));
    raw_touch(cx, handle, TouchPhase::Moved, 360.0 - total);
    cx.executor().advance_clock(Duration::from_millis(150));
    raw_touch(cx, handle, TouchPhase::Ended, 360.0 - total);
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        first + 1,
        "慢拖走过 {total:.0}（线 {:.0}）就该落定，不必甩",
        SNAP_FRACTION * dist,
    );
}

/// 拖到一半停住再松手：那一截位移再快也不该翻 —— 速度按停手作废，退回距离那一律。
///
/// 与上面那条是一对：只钉「快就翻」会漏掉「停过手不翻」，而后者才是读者慢慢挪到半屏、
/// 想了想、抬手这一路的去处。
#[gpui_kit::test]
fn a_flick_that_stops_before_the_release_bounces_back(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    settle(cx, handle);
    let first = view.read_with(cx, |v, _| v.page());
    let (_, dist, _) = view
        .read_with(cx, |v, cx| v.swipe_geometry(cx, Axis::Horizontal, None))
        .expect("分页 + 横扫该有几何");
    let total = (fling_floor(dist) + SNAP_FRACTION * dist) * 0.5;

    raw_touch(cx, handle, TouchPhase::Started, 360.0);
    cx.executor().advance_clock(Duration::from_millis(16));
    raw_touch(cx, handle, TouchPhase::Moved, 360.0 - total * 0.5);
    cx.executor().advance_clock(Duration::from_millis(16));
    raw_touch(cx, handle, TouchPhase::Moved, 360.0 - total);
    // 停手：比 FLING_STOPPED 宽，且别宽到把识别器那一笔判成上一笔的收尾（500ms 长按档）。
    cx.executor().advance_clock(Duration::from_millis(150));
    raw_touch(cx, handle, TouchPhase::Ended, 360.0 - total);
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        first,
        "停过手就没有速度，走过 {total:.0} 没过线 {line:.0} 该弹回",
        line = SNAP_FRACTION * dist,
    );
    turn_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));
    assert!(
        view.read_with(cx, |v, _| v.turn.is_none()),
        "弹回走完该收掉"
    );
}

/// 宽屏上「冲一格、停住、再抬手」：那一段摊出来的平均，不该当成松手那一下的速度。
///
/// 上面那条测的是**抬手时的新鲜度**（最近一次有位移的取样过旧就作废）。这一条测的是
/// **取样那一段**：一笔里有一条长段 —— 分母盖过停手那一档，手指在头几毫秒冲了八十来像素、
/// 随后停住不动、抬手 —— 平均除出来过了速度门，可松手那一下其实没有速度。
///
/// 这一支在手机那一档的屏宽上量不出来：摊过 110ms 才够 700 的那一截（≈77）加上一笔起步
/// 刚过 [`SLOP`](crate::ui::features::reader::model::swipe::SLOP) 的那 20，在 384 宽那一屏上
/// 已经越过落定线，归距离那一律管了。所以这里开宽窗。
#[gpui_kit::test]
fn a_burst_then_a_stall_on_a_wide_page_does_not_fling(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        1000.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    settle(cx, handle);
    let first = view.read_with(cx, |v, _| v.page());
    let (_, dist, _) = view
        .read_with(cx, |v, cx| v.swipe_geometry(cx, Axis::Horizontal, None))
        .expect("分页 + 横扫该有几何");

    // 攒下来那一段摊到的时长：比停手那一档（100ms）长，又不长到把识别器那一笔判成上一笔。
    const STALL: f32 = 0.11;
    let start = 20.0;
    let burst = FLING_SPEED * STALL + 8.0;
    let total = start + burst;
    let line = SNAP_FRACTION * dist;
    assert!(
        burst / STALL >= FLING_SPEED,
        "前置：那一段摊到 {STALL}s 要过得了速度门，实得 {:.0}",
        burst / STALL
    );
    assert!(
        fling_floor(dist) < total && total < line - 1.0,
        "前置：这一屏得够宽 —— 走过 {total:.0} 要过了下限、又没到落定线 {line:.0}（轴长 {dist:.0}）"
    );

    raw_touch(cx, handle, TouchPhase::Started, 900.0);
    raw_touch(cx, handle, TouchPhase::Moved, 900.0 - start);
    // 同一格再冲一截：钟没走，这一段攒着不取样。
    raw_touch(cx, handle, TouchPhase::Moved, 900.0 - total);
    cx.executor().advance_clock(Duration::from_secs_f32(STALL));
    raw_touch(cx, handle, TouchPhase::Ended, 900.0 - total);
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        first,
        "停过手之后抬手，那一段 {burst:.0} 摊到 {STALL}s 的平均不该被当成松手速度"
    );
}

/// 回上一页那一趟：往右横扫同样要跟手、同样过线落定。
///
/// 这条与「往后翻」那条是一对，钉的是一个只漏一半的缺陷：位移按读书顺序折出来时，
/// 回上一页天生是**负**的，而进度那一步把负的夹成 0 —— 于是往右划纸不动、松手也永远
/// 过不了线，横扫只有半个方向能用。认领之后按那一步的方向取距离才两边都成立。
#[gpui_kit::test]
fn a_horizontal_swipe_back_lands_on_the_previous_page(cx: &mut TestAppContext) {
    use crate::ui::features::reader::model::session::TurnRun;

    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    settle(cx, handle);
    // 先往后走两页再往回划：从第 0 页起划回去，那一趟本来就没有上一页可去。
    for _ in 0..2 {
        view.update(cx, |v, cx| v.scroll_page(1, cx));
    }
    settle(cx, handle);
    let from = view.read_with(cx, |v, _| v.page());
    assert_eq!(from, 2, "语料该够切出第三页，否则这条测不到东西");

    // 往右划：跟手起来的那一张纸该是**动过的**，不只是挂了个状态。
    raw_touch(cx, handle, TouchPhase::Started, 60.0);
    raw_touch(cx, handle, TouchPhase::Moved, 210.0);
    let following = view.read_with(cx, |v, _| v.turn);
    assert!(
        matches!(following, Some(turn) if matches!(turn.run, TurnRun::Following { .. })),
        "往右横扫过阈值该进入跟手，实得 {following:?}"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        from,
        "跟手期间页号不动"
    );
    assert_ne!(
        view.read_with(cx, |v, _| v.page_shift),
        (0.0, 0.0),
        "回上一页那一趟纸也得跟着手指走"
    );
    raw_touch(cx, handle, TouchPhase::Moved, 360.0);
    raw_touch(cx, handle, TouchPhase::Ended, 360.0);
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        from - 1,
        "走过大半屏往回划，松手该落回上一页"
    );
    turn_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));
    assert!(view.read_with(cx, |v, _| v.turn.is_none()));
    assert_eq!(view.read_with(cx, |v, _| v.page_shift), (0.0, 0.0));

    // 连着再往右划一次：读者来回划是常态，前一划的惯性不该把下一划的方向带偏。
    raw_touch(cx, handle, TouchPhase::Started, 60.0);
    raw_touch(cx, handle, TouchPhase::Moved, 360.0);
    raw_touch(cx, handle, TouchPhase::Ended, 360.0);
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        from - 2,
        "第二划该再往回一页"
    );
    turn_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));
    assert_eq!(view.read_with(cx, |v, _| v.page_shift), (0.0, 0.0));
}

/// 折页那一档往**右**横扫：纸得真的跟着卷起来，而且起的是**左**边那一条。
///
/// 那条律只认「往左划」那一套坐标，往回翻的手势要在进出口各映一次横向。只映出口那一半的时候，
/// 进度那一步照样在走（它读的是折过号的位移），而上面那条 `a_horizontal_swipe_back…` 走的是
/// 平移那一档（判的是 `page_shift`，折页恒为 0），两扇门都绿，屏上却是「往右拖整趟不动、松手
/// 才突然卷起来」—— 读者报的「只能往左滑不能往右滑」。这条读的是 GPU 那份 uniform。
#[gpui_kit::test]
fn a_curl_swipe_back_rolls_the_page(cx: &mut TestAppContext) {
    use crate::ui::features::reader::model::session::TurnRun;
    use crate::ui::features::reader::motion::turn_animation::cylinder_at;

    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    cx.update(|app| {
        ConfigHolder::edit_appearance(app, |a| a.page_turn = PageTurn::Curl);
    });
    settle(cx, handle);
    // 先往后走两页：从第 0 页往回划，那一趟没有上一页可去，测不到东西。
    //
    // 折页要等当前页的捕获纹理完整落地才放行下一页，而静态页不会自己再续一帧，所以每
    // 翻一页都把动画走完、再 settle 两帧让 `capture_committed` 落到 `captured_page`。
    for _ in 0..2 {
        view.update(cx, |v, cx| v.scroll_page(1, cx));
        turn_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));
        settle(cx, handle);
    }
    let from = view.read_with(cx, |v, _| v.page());
    assert_eq!(from, 2, "语料该够切出第三页");

    raw_touch(cx, handle, TouchPhase::Started, 60.0);
    raw_touch(cx, handle, TouchPhase::Moved, 210.0);
    let turn = view
        .read_with(cx, |v, _| v.turn)
        .expect("往右横扫过阈值该挂上跟手那一趟");
    let TurnRun::Following { t } = turn.run else {
        panic!("该在跟手那一态，实得 {:?}", turn.run);
    };
    assert!(!turn.forward, "往右横扫该被判成回上一页");
    assert!(t > 0.0, "进度那一步该跟着走，实得 {t}");
    let grip = turn.grip.expect("折页这一档，手是每一步都跟着写的");
    let area = view.read_with(cx, |v, _| v.page_area());

    // 网格那头判「哪一段已经卷走」读的就是这一份 uniform：`n·x + n·y − d > 0` 那一侧离场。
    let cylinder = cylinder_at(t, turn.forward, area, Some(grip)).expect("量得出圆柱");
    let rolled = |x: f32| cylinder.nx * x + cylinder.ny * 200.0 - cylinder.d > 0.0;
    assert!(
        rolled(area.left + 1.0),
        "往右拖这一笔该把**左**边那一窄条卷起来，实得 n = ({}, {}) d = {}",
        cylinder.nx,
        cylinder.ny,
        cylinder.d
    );
    assert!(
        !rolled(area.right - 1.0),
        "右缘那一条还该整片平摊着 —— 起的是左边，不是整页一起走"
    );
    // 卷筒也得真的长出来：半径为 0 就是一根硬折的棍，与「仿真」那一格无关。
    assert!(
        cylinder.radius > 0.0,
        "跟手途中该有一段落在柱面上，实得 radius = {}",
        cylinder.radius
    );

    raw_touch(cx, handle, TouchPhase::Moved, 360.0);
    raw_touch(cx, handle, TouchPhase::Ended, 360.0);
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        from - 1,
        "走过大半屏往回划，松手该落回上一页"
    );
}

/// 单页仿真折页揭的是**整张纸**：页眉那行章名、页脚那行页码与正文一起卷。
///
/// `page_area` 能分开这两件事：正文版心从 `header_reserve` 开始、到页脚带上沿结束；
/// 折页的纸面从阅读页顶（y=0）铺到窗口底。顺带钉住纸底下那一页的读数是**目标页**的。
#[gpui_kit::test]
fn a_curl_sheet_includes_the_header_and_footer_bands(cx: &mut TestAppContext) {
    use crate::ui::features::reader::view::bands;
    use gpui_kit::AppContext as _;

    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| {
        v.set_mode(PageMode::Paged, cx);
        // 页眉/页脚各 28：正文版心因此从 28 开始、到窗口底减 28 结束；纸面两头都要越过它们。
        v.set_chrome_bands(28.0, 28.0, 0.0, cx);
    });
    cx.update(|app| {
        ConfigHolder::edit_appearance(app, |a| a.page_turn = PageTurn::Curl);
    });
    settle(cx, handle);
    let window_h = cx
        .update_window(handle, |_, window, _| {
            f32::from(window.viewport_size().height)
        })
        .expect("量窗口");
    let area = view.read_with(cx, |v, _| v.page_area());
    assert_eq!(area.top, 0.0, "纸面该从阅读页顶开始，实得 {area:?}");
    assert_eq!(
        area.bottom, window_h,
        "纸面该铺到窗口底（页脚带与手势条那一段都在纸上），实得 {area:?}"
    );

    // 折到一半时，纸底下露出来的是目标页：它的读数带要念目标页的页码。
    let readouts = view.read_with(cx, |v, cx| bands::readouts_at(v, v.chapter_index(), 1, cx));
    assert_eq!(readouts.chapter, "第一章");
    assert!(
        readouts.position.contains('2'),
        "第 2 页的读数该是「2」，实得 {:?}",
        readouts.position
    );
}

/// 页眉那条带子只让**一次**安全区：可重排的页从窗口 0 起，带子自己落在 `top_inset`
/// 那条线下面，不再让第二次。
///
/// 带子顶边 = `top_inset`，章名贴着带顶 2 像素，带子底边 = `top_inset + header_reserve`；
/// 正文从这条线往下。带子再让一次时，章名会再低一截、带子也会向下多盖 `top_inset`
/// 那么高的正文 —— 真机上正是「页眉很低、顶上那截图片被切掉」。
#[gpui_kit::test]
fn the_header_band_does_not_take_the_safe_area_twice(cx: &mut TestAppContext) {
    use gpui_kit::{AppContext as _, test::TestWindowExt as _};

    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| {
        v.set_mode(PageMode::Paged, cx);
        // 上下边距都取 0，只留页眉那一格：这一条门量的是安全区与带子的关系。
        v.set_chrome_bands(28.0, 0.0, 0.0, cx);
        // 可重排的页从窗口 0 起：带子自己让 24，正文接着让。
        v.insets.top = 24.0;
    });
    settle(cx, handle);
    let (label_top, band_bottom) = cx
        .update_window(handle, |_, window, _| {
            let label = window
                .try_find("reader-header-chapter")
                .expect("页眉那条章名该画出来");
            let band = window
                .try_find("reader-header-band")
                .expect("页眉那条带子该画出来");
            (
                f32::from(label.bounds().origin.y),
                f32::from(band.bounds().bottom()),
            )
        })
        .expect("窗口应能渲染");
    assert!(
        (label_top - (24.0 + 2.0)).abs() < 0.5,
        "章名该贴着带顶（24 + 2），安全区只让一次；实得顶边 {label_top}"
    );
    assert!(
        (band_bottom - (24.0 + 28.0)).abs() < 0.5,
        "带子底边该正好停在正文让出的那一格（24 + 28）；实得 {band_bottom}"
    );
}

/// 平移翻页里**每一页的整页底图跟着自己那张纸走**。
///
/// 2026-11-01 深夜的现场：整页底图只在 `turn.is_none()` 时画，一开翻就整块消失 ——
/// 读者读到的是「字在滑，纸没动」，于是只有仿真折页（底图烘进了捕获纹理）像一整张纸。
/// 这条门量三个数：静止时底图在原点；跟手途中**两张纸各有一张底图**；两者相距一屏，
/// 且旧页在左、新页在右（这一笔是往后翻）。
#[gpui_kit::test]
fn a_page_backdrop_rides_its_own_sheet(cx: &mut TestAppContext) {
    use crate::ui::features::reader::view::body::page_art_element_id;
    use folio_core::content::format::ImageRes;
    use gpui_kit::{AppContext as _, test::TestWindowExt as _};

    cx.update(gpui_kit::init);
    let body = (0..60)
        .map(|i| {
            format!("<p>Page art paragraph number {i}, long enough to wrap a line or two.</p>")
        })
        .collect::<String>();
    let flow = FlowDoc {
        chapters: vec![Chapter::inline("第一章", backdrop_chapter(&body))],
        // 有一条元数据就够：`book.get(href)` 只按表认这张图（字节在后台那一趟才去读），
        // 而这里量的是元素树上那一层的位移，不是像素。
        images: vec![ImageRes {
            href: "fm1.jpg".into(),
            source: "fm1.jpg".into(),
            media_type: "image/jpeg".into(),
            width: 1080,
            height: 2400,
        }],
        ..Default::default()
    };
    let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    settle(cx, handle);
    assert!(
        view.read_with(cx, |v, _| v.page_count()) >= 2,
        "语料得切出两页，否则这一趟没有「另一张纸」"
    );

    let at = |cx: &mut TestAppContext, id| {
        cx.update_window(handle, |_, window, _| window.try_find(id))
            .expect("窗口应能渲染")
    };
    let still = at(cx, page_art_element_id(0, 0)).expect("静止帧该铺这一页的底图");
    assert_eq!(
        f32::from(still.bounds().origin.x),
        0.0,
        "不翻页时底图就在原点"
    );

    // 往后翻：从右往左扫。跟手途中页号不动，屏上该同时有两张纸、两张底图。
    raw_touch(cx, handle, TouchPhase::Started, 360.0);
    raw_touch(cx, handle, TouchPhase::Moved, 240.0);
    settle(cx, handle);
    let (leaving, arriving, width) = cx
        .update_window(handle, |_, window, _| {
            let leaving = window
                .try_find(page_art_element_id(0, 0))
                .expect("旧页的底图该跟着旧纸走，不该整块消失");
            let arriving = window
                .try_find(page_art_element_id(0, 1))
                .expect("新页的底图该铺在自己那张纸上");
            (
                f32::from(leaving.bounds().origin.x),
                f32::from(arriving.bounds().origin.x),
                f32::from(window.viewport_size().width),
            )
        })
        .expect("窗口应能渲染");
    assert!(leaving < 0.0, "往后翻时旧页该往左走，实得 x = {leaving}");
    assert!(
        arriving > 0.0,
        "新页该还在右边等着进来，实得 x = {arriving}"
    );
    assert!(
        ((arriving - leaving) - width).abs() < 1.0,
        "两张纸始终相距一屏（{width}），实得 {} —— 底图没跟纸走才会对不上",
        arriving - leaving
    );
}

/// 分页档：**有正文**的整页底图章不让带子透明，纯图页才让。
///
/// 2026-11-01 白天那版是「不管有没有字都铺满」，当天深夜按读者的口径改回来：有字的
/// 底图章（人物志那一族）铺满等于章名压在正文上、正文压进页脚。这一条与连续滚动
/// 那一档共用一个判据（`view::body` 的 `page_art_bare` / `bare_bands`），分页只是
/// 走每页一份带子。
#[gpui_kit::test]
fn a_paged_captioned_backdrop_keeps_its_bands(cx: &mut TestAppContext) {
    use gpui_kit::AppContext as _;
    use gpui_kit::test::TestWindowExt as _;

    cx.update(gpui_kit::init);
    let flow = FlowDoc {
        chapters: vec![Chapter::inline(
            "人物志",
            backdrop_chapter("<p>Character Introduction</p>"),
        )],
        ..Default::default()
    };
    let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
    assert!(
        view.read_with(cx, |v, _| v
            .book()
            .expect("有书")
            .document()
            .is_picture_chapter()),
        "前置：这张底图要被认成整页画"
    );
    view.update(cx, |v, cx| {
        v.set_mode(PageMode::Paged, cx);
        v.set_chrome_bands(28.0, 28.0, 0.0, cx);
    });
    settle(cx, handle);
    let bare = cx
        .update_window(handle, |_, window, _| {
            window.try_find("reader-header-band-bare").is_some()
                && window.try_find("reader-header-band").is_none()
        })
        .expect("窗口应能渲染");
    assert!(
        bare,
        "整页底图章（有字也算）在分页档也让带子透明：底图铺满整窗"
    );
}

/// 从右往左读的那一本里走折页：往**右**划是「往后翻」，而纸揭的是**左**边那一窄条。
///
/// 这条钉的是「阅读方向真的一路走到了几何」：会话读那本书的 `progression_rtl` → 动画帧带着它
/// → 折页按「自由边在哪一侧」判（`forward != rtl`）。从前那条链断在最后一格，折页把 `forward`
/// 直接当判据用，于是 RTL 里往后翻那一笔（往右拖）被「往翻页反方向走的那一截不算」夹成 0 ——
/// 与 LTR 往回翻那个缺陷同一个读数，而这一半从来没有门过。
#[gpui_kit::test]
fn a_curl_swipe_in_an_rtl_book_rolls_from_the_left_edge(cx: &mut TestAppContext) {
    use crate::ui::features::reader::model::session::TurnRun;
    use crate::ui::features::reader::motion::turn_animation::Pose;
    use crate::ui::features::reader::motion::turn_animation::cylinder_at;

    cx.update(gpui_kit::init);
    let mut doc = flow();
    doc.progression_rtl = true;
    let (view, handle) = open(cx, &doc, "markdown", prefs(16.0), Locator::start(), 460.0);
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    cx.update(|app| {
        ConfigHolder::edit_appearance(app, |a| a.page_turn = PageTurn::Curl);
    });
    settle(cx, handle);
    let rtl = view.read_with(cx, |v, _| v.progression_rtl());
    assert!(rtl, "语料得真的是从右往左读的那一本，否则这条什么都测不到");
    let from = view.read_with(cx, |v, _| v.page());

    // 往**右**划：从右往左读的那一本里，那才是「往后翻」那一笔。
    raw_touch(cx, handle, TouchPhase::Started, 60.0);
    raw_touch(cx, handle, TouchPhase::Moved, 210.0);
    let turn = view
        .read_with(cx, |v, _| v.turn)
        .expect("RTL 里往右横扫过阈值该挂上跟手那一趟");
    let TurnRun::Following { t } = turn.run else {
        panic!("该在跟手那一态，实得 {:?}", turn.run);
    };
    assert!(turn.forward, "RTL 里往右划该是**往后**翻");
    assert!(t > 0.0, "进度那一步该跟着走，实得 {t}");
    let grip = turn.grip.expect("折页这一档，手是每一步都跟着写的");
    let area = view.read_with(cx, |v, _| v.page_area());

    let free_right = Pose::free_edge_right(turn.forward, rtl);
    let cylinder = cylinder_at(t, free_right, area, Some(grip)).expect("量得出圆柱");
    let rolled = |x: f32| cylinder.nx * x + cylinder.ny * 200.0 - cylinder.d > 0.0;
    assert!(
        rolled(area.left + 1.0),
        "自由边在该本的左边，左缘那一窄条得已经离场；n = ({}, {}) d = {}",
        cylinder.nx,
        cylinder.ny,
        cylinder.d
    );
    assert!(
        !rolled(area.right - 1.0),
        "右缘那一条还该平摊着 —— 判据要是读成了 `forward`，这里整页都不会动"
    );
    assert!(
        cylinder.radius > 0.0,
        "跟手途中该有一段落在柱面上，实得 radius = {}",
        cylinder.radius
    );

    raw_touch(cx, handle, TouchPhase::Moved, 360.0);
    raw_touch(cx, handle, TouchPhase::Ended, 360.0);
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        from + 1,
        "往右划过大半屏，松手该往后翻一页"
    );
}

/// 选了「上下平移」之后，竖着拖的那一趟也得跟着手指走。
///
/// 从前这一档在手指上永远不成立：跟手那一层只认横轴，竖的那一趟整个被 `on_scroll` 的
/// 攒半屏接走，读者拖过半屏看到的是一次没有动画的跳页 —— 设置里选得到的东西，手上拿不到。
#[gpui_kit::test]
fn a_vertical_drag_follows_the_vertical_style(cx: &mut TestAppContext) {
    use crate::ui::features::reader::model::session::TurnRun;

    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    cx.update(|app| {
        ConfigHolder::edit_appearance(app, |a| a.page_turn = PageTurn::SlideUp);
    });
    settle(cx, handle);
    let first = view.read_with(cx, |v, _| v.page());

    // 往上拖：读书顺序里是往后翻。
    let from = point(px(200.0), px(400.0));
    raw_touch_at(cx, handle, TouchPhase::Started, from);
    raw_touch_at(cx, handle, TouchPhase::Moved, point(px(200.0), px(260.0)));
    let following = view.read_with(cx, |v, _| v.turn);
    assert!(
        matches!(following, Some(turn) if matches!(turn.run, TurnRun::Following { .. })),
        "竖档里往上拖过阈值该进入跟手，实得 {following:?}"
    );
    let shift = view.read_with(cx, |v, _| v.page_shift);
    assert_eq!(shift.0, 0.0, "竖档不该在 x 上留下位移：{shift:?}");
    assert_ne!(shift.1, 0.0, "跟手就是纸真的跟着手指走了：{shift:?}");

    raw_touch_at(cx, handle, TouchPhase::Moved, point(px(200.0), px(120.0)));
    raw_touch_at(cx, handle, TouchPhase::Ended, point(px(200.0), px(120.0)));
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        first + 1,
        "走过大半屏，松手该翻过去"
    );
    turn_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));
    assert_eq!(view.read_with(cx, |v, _| v.page_shift), (0.0, 0.0));
}

/// 轴归**动画档**：选的是「左右平移」（横轴），竖着拖这一笔什么都不该发生。
///
/// 从前这条路径是「轴不合也翻页，只是不跟手」—— 松手那一刻交给点屏那条路，照样翻一页。
/// 读者的观感是「我竖着划了一下，页面莫名其妙往前跳」，而那一跳走的还是横着的动画。
/// 现在另一根轴上的手势归滚动层（见 `swipe::claim_swipe` 的 `Claim::Refused`）。
#[gpui_kit::test]
fn a_vertical_drag_on_a_horizontal_style_does_nothing(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    settle(cx, handle);
    let first = view.read_with(cx, |v, _| v.page());
    let scroll = view.read_with(cx, |v, _| v.scroll_y);

    let from = point(px(200.0), px(400.0));
    raw_touch_at(cx, handle, TouchPhase::Started, from);
    raw_touch_at(cx, handle, TouchPhase::Moved, point(px(200.0), px(260.0)));
    assert!(
        view.read_with(cx, |v, _| v.turn).is_none(),
        "横档遇上竖划，一格纸都不该动"
    );
    raw_touch_at(cx, handle, TouchPhase::Moved, point(px(200.0), px(120.0)));
    raw_touch_at(cx, handle, TouchPhase::Ended, point(px(200.0), px(120.0)));
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        first,
        "竖着拖过大半屏也不该翻页：轴是读者选的那一档说了算"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.scroll_y),
        scroll,
        "分页里竖着拖也不该滚 —— 这一笔既不翻页也不滚动"
    );
}

/// 反过来那一半：选「上下平移」（竖轴），横扫同样什么都不该发生。
#[gpui_kit::test]
fn a_horizontal_drag_on_a_vertical_style_does_nothing(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    cx.update(|app| {
        ConfigHolder::edit_appearance(app, |a| a.page_turn = PageTurn::SlideUp);
    });
    settle(cx, handle);
    let first = view.read_with(cx, |v, _| v.page());

    raw_touch(cx, handle, TouchPhase::Started, 360.0);
    raw_touch(cx, handle, TouchPhase::Moved, 210.0);
    assert!(
        view.read_with(cx, |v, _| v.turn).is_none(),
        "竖档遇上横划不该摆跟手那两张纸"
    );
    raw_touch(cx, handle, TouchPhase::Moved, 60.0);
    raw_touch(cx, handle, TouchPhase::Ended, 60.0);
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        first,
        "竖档里横扫也不该翻页"
    );
}

/// 轴对上的那一半照旧：选「上下平移」，竖着拖就跟手、松手翻一页，动画在 y 上走。
#[gpui_kit::test]
fn a_vertical_drag_on_a_vertical_style_follows_the_finger(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    cx.update(|app| {
        ConfigHolder::edit_appearance(app, |a| a.page_turn = PageTurn::SlideUp);
    });
    settle(cx, handle);
    let first = view.read_with(cx, |v, _| v.page());

    let from = point(px(200.0), px(400.0));
    raw_touch_at(cx, handle, TouchPhase::Started, from);
    raw_touch_at(cx, handle, TouchPhase::Moved, point(px(200.0), px(300.0)));
    assert!(
        view.read_with(cx, |v, _| v.turn.is_some()),
        "竖档 + 竖划：纸该跟着手指走"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        first,
        "跟手期间页号不动"
    );
    raw_touch_at(cx, handle, TouchPhase::Moved, point(px(200.0), px(120.0)));
    raw_touch_at(cx, handle, TouchPhase::Ended, point(px(200.0), px(120.0)));
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        first + 1,
        "走过大半屏，松手翻过去"
    );
    let shift = view.read_with(cx, |v, _| v.page_shift);
    assert_eq!(shift.0, 0.0, "竖档的动画不该在 x 上位移：{shift:?}");
    assert_ne!(shift.1, 0.0, "松手后该看到竖档那趟平移：{shift:?}");
}

/// 松手后的惯性尾巴归**发起那一笔**，不该再翻一页，也不该把收尾动画按停。
///
/// 从前这条是「竖向滑动影响竖向翻页」的现场：惯性 `Moved` 没有跟踪态，落到滚动臂里，
/// 一句 `cancel_turn()` 把松手那一腿当场按停（读者看到「啪」一下到位），
/// 而 `wheel_accum` 又攒过半屏，于是再翻一页。
#[gpui_kit::test]
fn momentum_after_a_swipe_neither_turns_again_nor_cuts_the_animation(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    cx.update(|app| {
        ConfigHolder::edit_appearance(app, |a| a.page_turn = PageTurn::SlideUp);
    });
    settle(cx, handle);
    let first = view.read_with(cx, |v, _| v.page());

    let from = point(px(200.0), px(400.0));
    raw_touch_at(cx, handle, TouchPhase::Started, from);
    raw_touch_at(cx, handle, TouchPhase::Moved, point(px(200.0), px(300.0)));
    raw_touch_at(cx, handle, TouchPhase::Moved, point(px(200.0), px(120.0)));
    raw_touch_at(cx, handle, TouchPhase::Ended, point(px(200.0), px(120.0)));
    assert_eq!(view.read_with(cx, |v, _| v.page()), first + 1);

    // 惯性：同一根轴上继续往前的四段合成增量，然后收尾。
    for (y, dy) in [(110.0, 110.0), (80.0, 80.0), (40.0, 40.0), (20.0, 20.0)] {
        let _ = y;
        raw_touch_at(
            cx,
            handle,
            TouchPhase::Moved,
            point(px(200.0), px(400.0 - dy)),
        );
    }
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        first + 1,
        "惯性不该再翻一页"
    );
    assert!(
        view.read_with(cx, |v, _| v.turn.is_some()),
        "惯性也不该把松手那一腿按停 —— 动画还在走"
    );
    raw_touch_at(cx, handle, TouchPhase::Ended, point(px(200.0), px(20.0)));
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        first + 1,
        "收尾那一条同样不该翻页"
    );
    turn_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));
    assert_eq!(view.read_with(cx, |v, _| v.page_shift), (0.0, 0.0));
}

/// 斜着那一笔的**第二个自由度**只能来自原始触摸流。
///
/// 识别器把一笔定性成某一轴之后，就把另一根的增量抹成 0（`lock_delta_to_axis`）：只靠它折
/// 出来的 `ScrollWheelEvent`，「起手点 + 累计位移」反推出的手指此刻永远停在按下那一格的纵
/// 坐标上，折线转不起来。这一条同时是给元素订阅 `TouchEvent` 那个口子留的门 —— 口子没开，
/// `finger` 就是 `None`，`at.1` 与 `from.1` 逐字相同，这里当场红。
#[gpui_kit::test]
fn a_diagonal_swipe_moves_the_fold_on_both_axes(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    settle(cx, handle);
    cx.update(|app| {
        ConfigHolder::edit_appearance(app, |a| a.page_turn = PageTurn::Curl);
    });

    // 横着走得比竖着多一点：定性该归横轴（翻页），而纵向那一截只有原始流里还留着。
    raw_touch_at(cx, handle, TouchPhase::Started, point(px(400.), px(120.)));
    raw_touch_at(cx, handle, TouchPhase::Moved, point(px(250.), px(260.)));

    assert!(
        view.read_with(cx, |v, _| v.finger.is_some()),
        "原始触摸没到元素这一层：折线只剩一个自由度"
    );
    let grip = view
        .read_with(cx, |v, _| v.turn.and_then(|turn| turn.grip))
        .expect("跟手那一趟该带着读者这只手");
    assert!(
        (grip.at.1 - grip.from.1).abs() > 0.1,
        "斜着那一笔的折线只走了一根轴：按下 y = {}，此刻 y = {}",
        grip.from.1,
        grip.at.1
    );
}

/// 仿真折页翻过一页之后，**下一趟要立刻接得上**：点右侧直接翻，不许再点一下才动。
///
/// 这条钉的是两件事连在一起的那个空档：捕获只在「动画不在飞」时烘（`capture_wanted`），
/// 而落定那一帧场上既没有动效也没有别的通知 —— 下一帧不会来，这一页的纹理就要等读者
/// 下一次碰屏幕；而折页的门读的恰恰是「这一页烘好没有」。`turn_frame` 在落定时补的那一发
/// 重画就是填这个空档的。
///
/// 判据取「同一帧里页号真的动了」，不用 `pending_turn`：排进队也算「点了一下没翻」，
/// 那正是读者报的症状。
#[gpui_kit::test]
fn a_curl_turn_recaptures_the_page_without_a_second_tap(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    cx.update(|app| {
        ConfigHolder::edit_appearance(app, |a| a.page_turn = PageTurn::Curl);
    });
    settle(cx, handle);
    let first = view.read_with(cx, |v, _| v.page());
    // 起点必须已经烘好，否则这里测的是「进书还没烘」那一格。
    assert!(
        view.read_with(cx, |v, _| v.curl_capture_ready()),
        "前置：第一页的纹理该已经烘好"
    );

    view.update(cx, |v, cx| v.scroll_page(1, cx));
    turn_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));
    let landed = view.read_with(cx, |v, _| v.page());
    assert_eq!(landed, first + 1, "第一趟点屏该翻过去");

    // 不补任何 settle：落定那一帧自己补的重画就该把这一页烘上。
    turn_frame(cx, handle, Duration::from_millis(16));
    assert!(
        view.read_with(cx, |v, _| v.curl_capture_ready()),
        "翻完一页之后这一页的纹理该已经烘好 —— 否则下一次点击又要排队"
    );

    let before = view.read_with(cx, |v, _| v.page());
    view.update(cx, |v, cx| v.scroll_page(1, cx));
    let after = view.read_with(cx, |v, _| v.page());
    assert_eq!(after, before + 1, "连着点右侧该连着翻，不许卡在等待里");
    assert_eq!(
        view.read_with(cx, |v, _| v.pending_turn),
        None,
        "门开着时不该有排队的翻页"
    );
}

/// 翻完一页、动画落定之后，这一页的纹理**紧接着那一帧就烘上**，不必等读者再碰屏幕。
///
/// 捕获只在「动画不在飞」时烘，而落定那一帧没有任何别的东西会请求重画 —— `Presence` 的
/// 续帧只归时钟。全靠 `turn_frame` 在落定那一刻自己补的那一发 `notify` 才把下一帧要来；
/// 少了它，纹理要等读者下一次触碰才有，而折页的门读的正是「这一页烘好没有」：连点两下
/// 的第二下就卡在排队里不动。
///
/// 「紧接着那一帧」而不是「同一帧」：gpui 一帧出元素树、**下一帧**才 paint，捕获请求
/// 排在 paint 那一趟里（`capture_region` 那块 canvas 的绘制回调）。落定帧出树、下一帧
/// 请求捕获、提交在再下一帧的 `render` 开头落进 `captured_page` —— 所以这里的时间线是
/// 「落定帧之后再过两帧」。真机与这里同一条链，没为测试放宽任何判据。
#[gpui_kit::test]
fn the_landed_page_is_baked_without_another_touch(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    cx.update(|app| {
        ConfigHolder::edit_appearance(app, |a| a.page_turn = PageTurn::Curl);
    });
    settle(cx, handle);
    assert!(
        view.read_with(cx, |v, _| v.curl_capture_ready()),
        "前置：第一页该已经烘好"
    );

    view.update(cx, |v, cx| v.scroll_page(1, cx));
    // 落定那一刻：先把时钟推到动画结束，再按帧走 —— 一帧出树、一帧 paint 请求捕获、
    // 提交在再下一帧的 `render` 开头落定。三帧恰好是这一条链的长度。
    turn_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));
    let key = view.read_with(cx, |v, _| (v.chapter_index(), v.page()));
    assert_eq!(key.1, 1, "前置：这一趟该已经落在第二页");
    turn_frame(cx, handle, Duration::ZERO);
    turn_frame(cx, handle, Duration::ZERO);
    assert_eq!(
        view.read_with(cx, |v, _| v.captured_page),
        Some(key),
        "翻稳之后这一页该已经烘上，不必等读者再碰一次屏幕；实得 {:?}",
        view.read_with(cx, |v, _| v.captured_page)
    );
    assert!(
        view.read_with(cx, |v, _| v.curl_capture_ready()),
        "门该在没人碰屏幕的情况下自己开"
    );
}

/// 手指落在**可拖动的插图**上横扫：图跟着手指走，页号一个字不动。
///
/// 钉的是手势优先级：插图（`object-fit: cover` 被裁掉那一圈）接走这一笔，翻页层就得
/// 让开 —— 不然读者想看图的下半截，结果翻了一页。命中区由画图那一趟登记（真机上就是
/// 上一帧的绘制回调），这里直接放一条，走的是 `on_scroll` 那条真路。
#[gpui_kit::test]
fn dragging_an_image_pans_it_instead_of_turning_the_page(cx: &mut TestAppContext) {
    use crate::ui::features::reader::model::session::ImageHit;
    use gpui_kit::{Bounds, point, px, size};

    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    settle(cx, handle);
    let before = view.read_with(cx, |v, _| v.page());

    // 一块盖在正文上的图框：纵向还能拖 120px（图比框高）。
    view.update(cx, |v, _| {
        v.image_hits.borrow_mut().push(ImageHit {
            key: "img-1".into(),
            rect: Bounds::new(point(px(0.0), px(100.0)), size(px(460.0), px(300.0))),
            slack: (0.0, 120.0),
            frame: u64::MAX,
        });
    });

    // 竖着拖：识别器会在定轴那一刻把另一轴的增量抹成 0（`lock_delta_to_axis`），
    // 所以想量纵向偏移就得竖着划 —— 斜着划在真机上同样是「只有一根轴有数」。
    raw_touch_at(cx, handle, TouchPhase::Started, point(px(60.0), px(260.0)));
    raw_touch_at(cx, handle, TouchPhase::Moved, point(px(60.0), px(140.0)));
    raw_touch_at(cx, handle, TouchPhase::Ended, point(px(60.0), px(140.0)));

    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        before,
        "在图上拖不该翻页"
    );
    let pan = view.read_with(cx, |v, _| v.image_pans.get("img-1").copied());
    assert!(
        pan.is_some_and(|(_, y)| y < 0.0),
        "图该跟着手指动（手指往上拖 ⇒ 偏移为负 = 露出下面那截），实得 {pan:?}"
    );
    // 拖出 slack 也要停住：图不能被拖出自己的内容。
    view.update(cx, |v, _| {
        if let Some(offset) = v.image_pans.get_mut("img-1") {
            offset.1 = -9999.0;
        }
    });
    raw_touch_at(cx, handle, TouchPhase::Started, point(px(60.0), px(260.0)));
    raw_touch_at(cx, handle, TouchPhase::Moved, point(px(60.0), px(150.0)));
    raw_touch_at(cx, handle, TouchPhase::Ended, point(px(60.0), px(150.0)));
    let clamped = view.read_with(cx, |v, _| v.image_pans.get("img-1").copied().unwrap().1);
    assert!(
        (-120.0..=0.0).contains(&clamped),
        "拖动偏移该夹在 slack 以内，实得 {clamped}"
    );
}
