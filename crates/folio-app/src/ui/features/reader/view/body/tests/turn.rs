//! 翻页与翻页动画：章界滑动、各档纸面动画、时长与方向键。

use super::harness::{
    PARA, flow, open, prefs, raw_touch, scroll_down, scroll_mode, settle, turn_frame, wheel_lines,
};
use crate::ui::features::reader::model::chrome::{Arrow, PageMode};
use crate::ui::services::config::ConfigHolder;
use crate::ui::shell::{NAV_TRANSITION, turn_duration};
use folio_core::infra::metrics::{Metric, take_local};
use folio_core::{Chapter, FlowDoc, Locator, PageTurn};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, ScrollDelta, TestAppContext, TouchPhase, point, px,
};
use std::time::Duration;

/// 这一帧重建了几次场景。分页且定住时应当是 1（一栏一份），动画在走时是 2
/// （出场页 + 进场页各一份）—— 这是「两张纸真的都在树上」的唯一可测证据，
/// 因为位移本身看不见，而少摆一张的画法在屏幕上只是「下一页没来」。
fn rebuilds_this_frame(cx: &mut TestAppContext, handle: AnyWindowHandle, wait: Duration) -> u64 {
    take_local(Metric::SceneRebuilds);
    turn_frame(cx, handle, wait);
    take_local(Metric::SceneRebuilds)
}

/// 两章、每章都够长：跨章滑动要目标章真的切出页来，短到一页的语料测不出「滑到下一页」。
fn two_chapter_flow() -> FlowDoc {
    let source = (0..40)
        .map(|i| format!("<p>{PARA}（第 {i} 段）</p>"))
        .collect::<String>();
    FlowDoc {
        chapters: vec![
            Chapter::inline("第一章", source.clone()),
            Chapter::inline("第二章", source),
        ],
        ..Default::default()
    }
}

/// 方向键翻页（PC）：右 / 下往前，左 / 上往回，四颗都归正文。
///
/// 这一条钉的是**接线**：键绑定 → 动作 → `ReadingPage::arrow_turn` → `ReaderSession`
/// 那段路由只有真窗口跑得完，而它的错法很安静（按键没反应，或者反过来）。RTL 那一轴
/// 在 `chrome::Arrow::turn` 的单元测试里。
#[gpui_kit::test]
fn the_arrow_keys_turn_the_pages(cx: &mut TestAppContext) {
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
    assert_eq!(view.read_with(cx, |v, _| v.page()), 0);

    // 每一步都把动画走完再按下一颗：翻页那 280 毫秒里再按一下，落点会叠在两趟之间。
    for (arrow, expected) in [
        (Arrow::Right, 1),
        (Arrow::Down, 2),
        (Arrow::Left, 1),
        (Arrow::Up, 0),
    ] {
        let handled = view.update(cx, |v, cx| v.arrow_turn(arrow, cx));
        settle(cx, handle);
        assert!(handled, "{arrow:?} 该归正文");
        assert_eq!(
            view.read_with(cx, |v, _| v.page()),
            expected,
            "{arrow:?} 之后该落到第 {expected} 页"
        );
    }
}

/// 「翻页时长」那一格真管到了点屏那一趟：设成 700 ms 之后，300 ms 时那张纸还在半路，
/// 走过整趟才落定。
///
/// 这一条测的是**接线**而不是算术 —— `release_duration` 那类纯函数的门测不到这里。
/// 假如渲染帧仍然现读全仓共用那一个数（或 `Turn::over` 那一份快照没接上），300 ms 那一趟
/// 早就落定，第一个断言直接红。
#[gpui_kit::test]
fn a_longer_turn_duration_keeps_the_paper_in_flight(cx: &mut TestAppContext) {
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
    // 界面上那条滑条推到最慢那一头，就是这一句。
    view.update(cx, |_, cx| {
        ConfigHolder::edit_appearance(cx, |appearance| appearance.turn_duration = 700.0)
    });

    view.update(cx, |v, cx| v.scroll_page(1, cx));
    turn_frame(cx, handle, Duration::from_millis(300));
    assert!(
        view.read_with(cx, |v, _| v.turn).is_some(),
        "700 ms 的一趟在 300 ms 时不该已经落定"
    );
    assert_ne!(
        view.read_with(cx, |v, _| v.page_shift),
        (0.0, 0.0),
        "还在走的那一趟纸该还带着位移"
    );

    turn_frame(cx, handle, Duration::from_millis(420));
    assert!(
        view.read_with(cx, |v, _| v.turn).is_none(),
        "走过 720 ms（> 700）该落定了"
    );
    assert_eq!(view.read_with(cx, |v, _| v.page_shift), (0.0, 0.0));
}

/// 同一格也管得住松手那一腿的**上限**：停过手再抬（最近那次取样到松手之间过了
/// `FLING_STOPPED` 那一格的新鲜度，速度读成 0），`release_duration` 整条退回 `fallback`，
/// 于是这一腿的时长就是读者那一格的**准数**，可以逐毫秒断言。
///
/// 这条钉的是 `swipe_release` 里那一个 `fallback` 实参。它若还写死全仓共用那一个数，这里
/// 量到的就是 280 ms 而不是 700 —— 上限不跟着走，慢速档下「狠一点的一甩」仍比点屏那一趟快，
/// 读者感到的正是当初那句「太快」。
#[gpui_kit::test]
fn the_release_leg_is_capped_by_the_turn_duration(cx: &mut TestAppContext) {
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
    view.update(cx, |_, cx| {
        ConfigHolder::edit_appearance(cx, |appearance| appearance.turn_duration = 700.0)
    });

    let first = view.read_with(cx, |v, _| v.page());
    // 往**左**划 180 dp（> 认领那一格要的 0.3 屏 ≈ 138 dp，也过 0.2 的落线）：从第 0 页起
    // 只有「下一页」可去，往右划那一趟没有上一页可落，`swipe_release` 根本不会起动画。
    raw_touch(cx, handle, TouchPhase::Started, 300.0);
    raw_touch(cx, handle, TouchPhase::Moved, 120.0);
    // 停一下手：让最近那次取样到松手之间过了新鲜度那一格（`FLING_STOPPED` = 100 ms），
    // 速度读成 0。
    cx.executor().advance_clock(Duration::from_millis(140));
    raw_touch(cx, handle, TouchPhase::Ended, 120.0);
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        first + 1,
        "这一笔该把页翻过去，否则下面测的是不存在的动画"
    );

    let run = view.read_with(cx, |v, _| v.turn);
    let over = match run {
        Some(turn) => match turn.run {
            TurnRun::Released { over, .. } => over,
            other => panic!("松手后该在 Released 那一态，实得 {other:?}"),
        },
        None => panic!("松手该留下一趟在走的动画"),
    };
    assert_eq!(
        over,
        Duration::from_millis(700),
        "速度读成 0 时这一腿就该等于读者那一格"
    );
    turn_frame(cx, handle, Duration::from_millis(720));
    assert!(view.read_with(cx, |v, _| v.turn).is_none());
}

/// 默认档必须与整屏进出那一个数相等：那条「时长只有一个家」的规则就靠这一句钉住 ——
/// core 那侧的常数（`Appearance::TURN_DURATION_DEFAULT`）引用不到这一个数，等式只能在
/// app 这一侧断言。不等的那天，没碰过滑条的读者在同一屏里先感到两种快慢。
#[gpui_kit::test]
fn the_default_turn_duration_is_the_nav_transition(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    assert_eq!(cx.update(|cx| turn_duration(cx)), NAV_TRANSITION);
}

/// 章界不再夹在 `set_page` 里：目标章排好后，源页与目标页各摆一份场景，
/// 动画落地那一帧才换锚章。这条是「跨章滑动」的宿主门。
#[gpui_kit::test]
fn a_paged_chapter_boundary_slides_between_chapters(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &two_chapter_flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    settle(cx, handle);
    let last = view.read_with(cx, |v, _| v.page_count().saturating_sub(1));
    assert!(last > 0, "前置：第一章该切成多页");
    view.update(cx, |v, cx| v.set_page(last, cx));
    settle(cx, handle);
    assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 0);

    // 章界按下一页：先等目标章排版，再起跨章 slide。
    view.update(cx, |v, cx| v.scroll_page(1, cx));
    let mut turn = None;
    for _ in 0..12 {
        settle(cx, handle);
        turn = view.read_with(cx, |v, _| v.turn);
        if turn.is_some() {
            break;
        }
    }
    let turn = turn.expect("目标章排好后该起跨章滑动");
    let (leaving, arriving) = (
        turn.leaving.expect("有源页"),
        turn.arriving.expect("有目标页"),
    );
    assert_eq!((leaving.chapter, leaving.page), (0, last));
    assert_eq!((arriving.chapter, arriving.page), (1, 0));
    assert_eq!(
        view.read_with(cx, |v, _| v.chapter_index()),
        0,
        "动画没走完之前不能先换章"
    );

    // 半程：两张纸都在树上（源章页 + 目标章页各一份场景）。
    let rebuilds = rebuilds_this_frame(cx, handle, Duration::from_millis(140));
    assert_eq!(rebuilds, 2, "跨章滑动要两张纸同时上屏，实得 {rebuilds}");
    assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 0);

    // 走完：锚章换成第二章，页号落到它的第一页。
    turn_frame(cx, handle, Duration::from_millis(200));
    assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 1);
    assert_eq!(view.read_with(cx, |v, _| v.page()), 0);
    assert!(view.read_with(cx, |v, _| v.turn.is_none()));

    // 反向：第二章第一页往回，目标页应是第一章最后一页。
    view.update(cx, |v, cx| v.scroll_page(-1, cx));
    let mut back = None;
    for _ in 0..12 {
        settle(cx, handle);
        back = view.read_with(cx, |v, _| v.turn);
        if back.is_some() {
            break;
        }
    }
    let back = back.expect("反向跨章也该起滑动");
    let arriving = back.arriving.expect("有目标页");
    assert_eq!((arriving.chapter, arriving.page), (0, last));
    // 第一趟采样只把 Presence 的起点钉住；再走 300ms 才越过 280ms 的转场。
    turn_frame(cx, handle, Duration::from_millis(1));
    turn_frame(cx, handle, Duration::from_millis(300));
    assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 0);
    assert_eq!(view.read_with(cx, |v, _| v.page()), last);
}

/// 滚轮翻到章末时也走同一条跨章滑动：先不夹在 `last` 上，等目标章排好再走。
#[gpui_kit::test]
fn a_paged_wheel_at_the_chapter_edge_starts_the_chapter_slide(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &two_chapter_flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    settle(cx, handle);
    let (last, viewport) = view.read_with(cx, |v, _| {
        (v.page_count().saturating_sub(1), v.laid_out.viewport)
    });
    view.update(cx, |v, cx| v.set_page(last, cx));
    settle(cx, handle);

    scroll_down(cx, handle, viewport * 0.6);
    let mut turn = None;
    for _ in 0..12 {
        settle(cx, handle);
        turn = view.read_with(cx, |v, _| v.turn);
        if turn.is_some() {
            break;
        }
    }
    let turn = turn.expect("章末滚轮也该起跨章滑动");
    let arriving = turn.arriving.expect("有目标页");
    assert_eq!((arriving.chapter, arriving.page), (1, 0));
}

/// 把翻页动画的两张纸摆上屏：分页翻页在走的那一段，出场页与进场页**同时**在场，
/// 方向是「下一页从右外进来、上一页往左走」，走完就只画一张。
#[gpui_kit::test]
fn a_paged_turn_draws_both_pages_until_the_slide_has_run(cx: &mut TestAppContext) {
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
    assert!(
        view.read_with(cx, |v, _| v.turn.is_none()),
        "定住的时候不该有动画挂着"
    );
    assert_eq!(
        rebuilds_this_frame(cx, handle, Duration::from_millis(1)),
        1,
        "定住的一帧只该摆当前这一页"
    );

    view.update(cx, |v, cx| v.scroll_page(1, cx));
    // 起步那一帧：时钟还没走，进场那张整页停在阅读方向的前方外，出场那张还占着屏。
    let rebuilds = rebuilds_this_frame(cx, handle, Duration::from_millis(1));
    assert_eq!(
        rebuilds, 2,
        "翻页在走，出场页与进场页要各摆一份场景（拿到 {rebuilds} 份）"
    );
    let (turn, shift, chapter) =
        view.read_with(cx, |v, _| (v.turn, v.page_shift, v.chapter_index()));
    let turn = turn.expect("起步那一帧动画还挂着");
    assert_eq!(
        turn.leaving.map(|page| (page.chapter, page.page)),
        Some((chapter, first)),
        "出场页该是刚离开的那一页"
    );
    assert!(turn.forward, "往下一页翻 ⇒ 方向是往后");
    assert!(
        shift.0 > 100.0 && shift.1 == 0.0,
        "左右平移应当只沿 x 走，实得 {shift:?}"
    );

    // 走到一半：进场那张在靠近，还没落位。
    turn_frame(cx, handle, Duration::from_millis(140));
    let half = view.read_with(cx, |v, _| v.page_shift);
    assert!(
        half.0 > 0.0 && half.0 < shift.0,
        "位移该单调收小：{shift:?} → {half:?}"
    );

    // 走完：状态收干净，第二张不再入树。
    let rebuilds = rebuilds_this_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));
    assert_eq!(
        rebuilds, 1,
        "走完的那一帧不该再摆出场页（拿到 {rebuilds} 份场景）"
    );
    view.read_with(cx, |v, _| {
        assert!(v.turn.is_none(), "走完该把动画状态收掉");
        assert_eq!(v.page_shift, (0.0, 0.0), "定住后当前页不该带着位移");
    });
    assert_eq!(view.read_with(cx, |v, _| v.page()), first + 1);
}

/// 竖档只沿 y 走：接到 x 上不报错，画面是「一页从左边滑进来」，而这是这一档唯一的
/// 可见差别，所以钉在位移的两个分量上。
#[gpui_kit::test]
fn the_vertical_axis_moves_the_page_up_not_across(cx: &mut TestAppContext) {
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
    // 轴是**全局**偏好（`appearance.page_turn`），不挂在书上：走的是配置那条路，
    // 与设置页里那一档同一个入口。
    cx.update(|app| {
        ConfigHolder::edit_appearance(app, |a| a.page_turn = PageTurn::SlideUp);
    });
    turn_frame(cx, handle, Duration::from_millis(1));

    view.update(cx, |v, cx| v.scroll_page(1, cx));
    turn_frame(cx, handle, Duration::from_millis(1));
    let shift = view.read_with(cx, |v, _| v.page_shift);
    assert_eq!(shift.0, 0.0, "竖档不该在 x 上留下位移：{shift:?}");
    assert!(shift.1 > 100.0, "竖档起步该整页偏在 y 上：{shift:?}");

    // 翻回上一页：方向反号，仍然是竖着走。
    turn_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));
    view.update(cx, |v, cx| v.scroll_page(-1, cx));
    turn_frame(cx, handle, Duration::from_millis(1));
    let back = view.read_with(cx, |v, _| v.page_shift);
    assert_eq!(back.0, 0.0, "往回翻也不该在 x 上位移：{back:?}");
    assert!(back.1 < 0.0, "往回翻时进场那张该从上方来：{back:?}");
}

/// 横扫不该把正在走的平移掐掉。`on_scroll` 读的是 `delta.y`，一次只有横分量的滑动
/// 就是 `dy == 0`；从前落定动画写在 `dy` 判据**之前**，于是「点屏翻页 + 随手一划」
/// 看到的直接是跳页。这一条钉的就是那两百多毫秒还该在屏幕上走完。
#[gpui_kit::test]
fn a_horizontal_swipe_does_not_cut_the_slide_short(cx: &mut TestAppContext) {
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

    view.update(cx, |v, cx| v.scroll_page(1, cx));
    turn_frame(cx, handle, Duration::from_millis(1));
    let before = view.read_with(cx, |v, _| v.page_shift);
    assert!(before.0 > 100.0, "起步那一帧该有横向位移：{before:?}");

    // 纯横扫：有 dx、dy 为 0。
    cx.update_window(handle, |_, window, cx| {
        window.scroll(
            "folio-reader",
            ScrollDelta::Pixels(point(px(-160.0), px(0.0))),
            cx,
        );
    })
    .expect("滚动应送达阅读视图");
    cx.run_until_parked();

    turn_frame(cx, handle, Duration::from_millis(40));
    view.read_with(cx, |v, _| {
        assert!(v.turn.is_some(), "横扫不该收掉这趟动画");
        let shift = v.page_shift;
        assert!(
            shift.0 > 0.0,
            "横扫之后平移还在走（x 归零就是被掐掉了）：{shift:?}"
        );
    });
}

/// 连续滚动的「上下滚动」：一次离散翻页跳的是**画出来的**那个 y，
/// 从起点补间到终点，而 `scroll_y` 一开始就是终点（那才是权威位置、落库的口径）。
#[gpui_kit::test]
fn a_continuous_turn_tweens_the_painted_position(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    scroll_mode(cx, handle);
    settle(cx, handle);
    let start = view.read_with(cx, |v, _| v.scroll_y);

    view.update(cx, |v, cx| v.scroll_page(1, cx));
    let (target, turn) = view.read_with(cx, |v, _| (v.scroll_y, v.turn));
    assert!(
        target > start,
        "连续模式点一下要往前一屏：{start} → {target}"
    );
    let turn = turn.expect("离散翻页该起一趟补间");
    assert!(turn.scroll.is_some(), "连续那一臂的两端在这里");
    assert_eq!(turn.scroll.expect("上面刚判过"), (start, target));

    turn_frame(cx, handle, Duration::from_millis(1));
    let painted = view.read_with(cx, |v, _| v.scroll_painted);
    assert!(
        (painted - start).abs() < 1.0,
        "起步该还画在起点上，实得 {painted}（起点 {start}）"
    );

    turn_frame(cx, handle, Duration::from_millis(140));
    let half = view.read_with(cx, |v, _| v.scroll_painted);
    assert!(
        half > start && half < target,
        "走到一半该落在两点之间：{start} < {half} < {target}"
    );

    turn_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));
    let (painted, settled) = view.read_with(cx, |v, _| (v.scroll_painted, v.turn));
    assert!(settled.is_none(), "走完要把补间收掉");
    assert!(
        (painted - target).abs() < 1.0,
        "定住后画的位置就是权威位置：{painted} vs {target}"
    );
}

/// 手指还在屏上，补间就该让位：滚轮事件一到当场落定，别让一张纸追在手指后面滑。
#[gpui_kit::test]
fn a_wheel_event_cancels_a_scroll_tween(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    scroll_mode(cx, handle);
    settle(cx, handle);
    view.update(cx, |v, cx| v.scroll_page(1, cx));
    turn_frame(cx, handle, Duration::from_millis(60));
    assert!(view.read_with(cx, |v, _| v.turn.is_some()), "补间还在走");

    scroll_down(cx, handle, 40.0);
    assert!(
        view.read_with(cx, |v, _| v.turn.is_none()),
        "滚轮一到位，补间必须当场让路"
    );
    turn_frame(cx, handle, Duration::from_millis(1));
    let (painted, target) = view.read_with(cx, |v, _| (v.scroll_painted, v.scroll_y));
    assert_eq!(painted, target, "没有动画在走时，画的位置就是权威位置");
}

/// 系统不让动的时候（减弱动态效果）：翻页当场落位，一张多余的纸都不画。
/// 判据不能只写「时长归零」—— 那要落到 `Presence` 给终值、而给终值那一帧仍可能多摆一张。
#[gpui_kit::test]
fn reduced_motion_lands_the_page_without_drawing_a_second_one(cx: &mut TestAppContext) {
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
    cx.update(|app| app.set_reduce_motion(true));
    let first = view.read_with(cx, |v, _| v.page());

    view.update(cx, |v, cx| v.scroll_page(1, cx));
    let rebuilds = rebuilds_this_frame(cx, handle, Duration::from_millis(1));
    assert_eq!(
        rebuilds, 1,
        "减动效那一档只画当前这一页（拿到 {rebuilds} 份场景）"
    );
    let (turn, shift, page) = view.read_with(cx, |v, _| (v.turn, v.page_shift, v.page()));
    assert!(turn.is_none(), "减动效不该留下还在走的动画");
    assert_eq!(shift, (0.0, 0.0), "位移当场归零");
    assert_eq!(page, first + 1, "页号照旧往前，动画让路而不是拦下这一翻");
}

/// 「不走动画」那一档：当场落位，多余的纸一张都不画。
///
/// 上面那条减动效测的不是这一件事：那条靠 `Presence` 给回终值（时钟仍然挂了），这一条靠
/// **根本不挂时钟**（`start_turn` 直接不挂），少的是那一趟多摆的切片与重排。
#[gpui_kit::test]
fn the_disabled_style_lands_the_page_without_a_second_sheet(cx: &mut TestAppContext) {
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
        ConfigHolder::edit_appearance(app, |a| a.page_turn = PageTurn::None);
    });
    let first = view.read_with(cx, |v, _| v.page());

    view.update(cx, |v, cx| v.scroll_page(1, cx));
    let rebuilds = rebuilds_this_frame(cx, handle, Duration::from_millis(1));
    assert_eq!(
        rebuilds, 1,
        "不走动画那一档只画当前这一页（拿到 {rebuilds} 份场景）"
    );
    let (turn, shift, page) = view.read_with(cx, |v, _| (v.turn, v.page_shift, v.page()));
    assert!(turn.is_none(), "这一档不该留下还在走的动画");
    assert_eq!(shift, (0.0, 0.0), "没有位移可减，命中不必知道这一页在动");
    assert_eq!(page, first + 1, "页号照旧往前");
}

/// 覆盖档仍然摆两张纸（新页盖在**停着不动**的旧页之上），而 `page_shift` 归进场那一张：
/// 命中要减的是当前页的位移，与谁后画无关。这两条各钉一半 —— 只验「两张」的话，把
/// `slots[0]` 换成出场页也不会报错，但横扫之后点字就点偏了。
#[gpui_kit::test]
fn a_cover_draws_two_sheets_and_keeps_the_offset_on_the_arriving_one(cx: &mut TestAppContext) {
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
        ConfigHolder::edit_appearance(app, |a| a.page_turn = PageTurn::Cover);
    });

    view.update(cx, |v, cx| v.scroll_page(1, cx));
    let rebuilds = rebuilds_this_frame(cx, handle, Duration::from_millis(1));
    assert_eq!(
        rebuilds, 2,
        "覆盖档起步要两张纸同时上屏（拿到 {rebuilds} 份场景）"
    );
    let shift = view.read_with(cx, |v, _| v.page_shift);
    assert!(
        shift.0 > 100.0 && shift.1 == 0.0,
        "覆盖也是横轴，且位移该记在进场那一张上：{shift:?}"
    );

    let rebuilds = rebuilds_this_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));
    assert_eq!(
        rebuilds, 1,
        "走完不该再摆那张停着的旧页（拿到 {rebuilds} 份）"
    );
    assert_eq!(view.read_with(cx, |v, _| v.page_shift), (0.0, 0.0));
}

/// 折页那一档现在只摆**进场**那一张：旧页在 `capture_page_region` 烘的纹理里，
/// 由 GPU 网格卷起来。于是 `page_shift` 全程归零，命中换算什么都不必减 ——
/// 谁把 `Curl::sheets_at` 改回带位移的那一支，画面仍然能动，只有点在补间途中会偏。
#[gpui_kit::test]
fn a_gpu_fold_draws_one_sheet_and_the_captured_old_page(cx: &mut TestAppContext) {
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

    view.update(cx, |v, cx| v.scroll_page(1, cx));
    let rebuilds = rebuilds_this_frame(cx, handle, Duration::from_millis(1));
    assert_eq!(
        rebuilds, 1,
        "GPU 折页只摆进场那一张：旧页在捕获纹理里，不该再走一遍字形（拿到 {rebuilds} 份场景）"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.page_shift),
        (0.0, 0.0),
        "折页里两张纸都停在原位，命中不该减任何东西"
    );

    let rebuilds = rebuilds_this_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));
    assert_eq!(
        rebuilds, 1,
        "走完不该再摆那张被折走的旧页（拿到 {rebuilds} 份）"
    );
    assert_eq!(view.read_with(cx, |v, _| v.page_shift), (0.0, 0.0));
}

/// 每一趟翻页都要一把**新**时钟键。章界那一趟从前在渲染帧里手摆 `seq: 0`、再把计数器加一
/// —— 于是它和任何一趟都可能撞同一把键；撞了不报错，那把键上还挂着上一趟「已走完」的状态，
/// `Presence` 第一帧就给回 1.0，章当场落定、看着像没有跨章动画。
#[gpui_kit::test]
fn consecutive_chapter_turns_get_their_own_clock_key(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &two_chapter_flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    settle(cx, handle);
    let last = view.read_with(cx, |v, _| v.page_count().saturating_sub(1));
    view.update(cx, |v, cx| v.set_page(last, cx));
    settle(cx, handle);

    // 正向跨章：等目标章排版，然后拿到那一趟。
    view.update(cx, |v, cx| v.scroll_page(1, cx));
    let mut forward = None;
    for _ in 0..12 {
        settle(cx, handle);
        forward = view.read_with(cx, |v, _| v.turn);
        if forward.is_some() {
            break;
        }
    }
    let forward = forward.expect("正向跨章该起滑动");

    // 走完落地，再往回跨一次。
    turn_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));
    settle(cx, handle);
    view.update(cx, |v, cx| v.scroll_page(-1, cx));
    let mut back = None;
    for _ in 0..12 {
        settle(cx, handle);
        back = view.read_with(cx, |v, _| v.turn);
        if back.is_some() {
            break;
        }
    }
    let back = back.expect("反向跨章也该起滑动");
    assert_ne!(
        back.seq, forward.seq,
        "两趟跨章共用一把时钟键：{:?} vs {:?}",
        back.seq, forward.seq
    );
}

/// 鼠标滚轮一格一页，不攒半屏。
///
/// 从前滚轮与触摸拖动共用「攒够半屏翻一页」那条规矩：一格 1 行折成 16px，而阈值是
/// 视口一半（几百像素），于是要转二十五格才翻一页。
#[gpui_kit::test]
fn a_wheel_notch_turns_exactly_one_page(cx: &mut TestAppContext) {
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

    wheel_lines(cx, handle, -1.0);
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        first + 1,
        "一格就是一页"
    );
    wheel_lines(cx, handle, -1.0);
    wheel_lines(cx, handle, -1.0);
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        first + 3,
        "三格三页：不该攒着，也不该一次跳两页"
    );
}

/// 「不走动画」那一档在章界：页号与锚章都得当场落过去，不能静默丢掉。
///
/// 这一档不挂时钟（`start_turn` 返回 false），而换章只在动画走完那一帧由
/// `finish_chapter_turn` 做 —— 从前这条路上没有任何人补这一下，读者按到的是
/// 「章末点两侧没反应」。两向各钉一次：往后落在下一章首页，往回落在上一章末页。
#[gpui_kit::test]
fn the_disabled_style_still_crosses_a_chapter(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &two_chapter_flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    settle(cx, handle);
    cx.update(|app| {
        ConfigHolder::edit_appearance(app, |a| a.page_turn = PageTurn::None);
    });
    let last = view.read_with(cx, |v, _| v.page_count().saturating_sub(1));
    assert!(last > 0, "前置：第一章该切成多页");
    view.update(cx, |v, cx| v.set_page(last, cx));
    settle(cx, handle);

    view.update(cx, |v, cx| v.scroll_page(1, cx));
    for _ in 0..4 {
        settle(cx, handle);
    }
    assert_eq!(
        view.read_with(cx, |v, _| (v.chapter_index(), v.page())),
        (1, 0),
        "章末按下一页该落在第二章首页"
    );
    assert!(
        view.read_with(cx, |v, _| v.turn.is_none()
            && v.pending_chapter_turn.is_none()),
        "不走动画那一档不该留下动画或 pending"
    );

    view.update(cx, |v, cx| v.scroll_page(-1, cx));
    for _ in 0..4 {
        settle(cx, handle);
    }
    assert_eq!(
        view.read_with(cx, |v, _| (v.chapter_index(), v.page())),
        (0, last),
        "章首按上一页该落在第一章末页"
    );
}

/// 松手那一腿的时钟从**抬手那一刻**起算，不是从第一帧起算。
///
/// 夹具在事件当刻就会渲一帧（`TestWindow` 在 `update_window` 里就画），真机上那一帧
/// 要到下一个 vsync 才来 —— 这一截正是旧行为吞掉的那一帧。所以这条不发射手事件，
/// 直接把「抬手发生在 16 ms 前」写进状态：第一帧就该按 16 ms 走，而不是停在原地。
#[gpui_kit::test]
fn the_release_leg_clock_starts_at_the_release_not_the_first_frame(cx: &mut TestAppContext) {
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
    cx.update(|app| {
        ConfigHolder::edit_appearance(app, |a| a.page_turn = PageTurn::Slide);
    });

    // 一笔往左拖到过半屏之前：跟手起来，页号还没动。
    raw_touch(cx, handle, TouchPhase::Started, 300.0);
    raw_touch(cx, handle, TouchPhase::Moved, 120.0);
    let (held, turn) = view.read_with(cx, |v, _| (v.page_shift, v.turn));
    let TurnRun::Following { t } = turn.expect("过阈值该起跟手").run else {
        panic!("跟手那一趟该在 Following");
    };
    assert_ne!(held, (0.0, 0.0), "前置：跟手那一刻纸该带着位移");

    // 抬手发生在这一刻；第一帧要到 16 ms 之后才来。
    let released_at = cx.update(|cx| cx.background_executor().now());
    cx.executor().advance_clock(Duration::from_millis(16));
    view.update(cx, |v, _| {
        let mut turn = v.turn.expect("该还在跟手");
        v.turn_seq += 1;
        turn.seq = v.turn_seq;
        turn.run = TurnRun::Released {
            from: t,
            revert: false,
            over: Duration::from_millis(280),
            since: released_at,
        };
        v.turn = Some(turn);
    });

    turn_frame(cx, handle, Duration::ZERO);
    let moved = view.read_with(cx, |v, _| v.page_shift);
    assert_ne!(
        moved, held,
        "抬手后的第一帧就该按 16 ms 走，实得仍停在 {held:?}"
    );
    assert!(
        view.read_with(cx, |v, _| v.turn).is_some(),
        "16 ms 远没到落定"
    );
}
