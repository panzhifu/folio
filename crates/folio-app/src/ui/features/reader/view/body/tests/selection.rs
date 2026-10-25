//! 选中与菜单条：拖选、长按、点击归因与清理。

use super::harness::{
    char_point, dispatch, drag_select, flow, mouse_down, mouse_move, mouse_up, open, prefs,
    scroll_mode, settle, turn_frame,
};
use crate::ui::features::reader::model::chrome::PageMode;
use crate::ui::shell::NAV_TRANSITION;
use folio_core::{Chapter, FlowDoc, Locator};
use gpui_kit::{
    AppContext as _, InputEvent as _, LongPressEvent, TestAppContext, TouchPhase, point, px,
};
use std::time::Duration;

// ---- M6 选中：真实指针/手势事件驱动的状态机（见 plans/M6-selection.md） ----
/// 拉丁词的语料：长按吸词的验收要「吸出整词」可判定。
fn latin_flow() -> FlowDoc {
    FlowDoc {
        chapters: vec![Chapter::inline(
            "第一章",
            "<p>Hello world and then some more words so the paragraph wraps once or twice here</p>",
        )],
        ..Default::default()
    }
}

/// M6 验收：按下 → 拖过阈值 → 松手，选区成立；阈值内的普通点击不留高亮；
/// 拖选松手攒下的 click 豁免恰好拦住那一下；复制的内容与可视文本一致。
#[gpui_kit::test]
fn a_pointer_drag_selects_and_swallows_the_trailing_click(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );

    // 普通点击（按下即抬，无位移）不留高亮：阈值之内的一切都不算拖选。
    let tap_at = char_point(&view, cx, 0, 1);
    mouse_down(cx, handle, tap_at);
    mouse_up(cx, handle, tap_at);
    assert!(
        view.read_with(cx, |v, _| v.selection().is_none()),
        "普通点击不该留下选中"
    );

    // 真实拖选：down → 8 步 move → up（跨过阈值、跨行）。
    let start = char_point(&view, cx, 0, 1);
    let end = char_point(&view, cx, 2, 3);
    mouse_down(cx, handle, start);
    for step in 1..=8 {
        let t = step as f32 / 8.0;
        mouse_move(
            cx,
            handle,
            point(
                px(f32::from(start.x) + (f32::from(end.x) - f32::from(start.x)) * t),
                px(f32::from(start.y) + (f32::from(end.y) - f32::from(start.y)) * t),
            ),
        );
    }
    mouse_up(cx, handle, end);
    let Some(selected) = view.read_with(cx, |v, _| v.selection()) else {
        panic!("拖过阈值的松手应成立选中");
    };
    assert!(
        selected.range.start < selected.range.end,
        "选区应非空：{selected:?}"
    );

    // 拖选松手攒下的豁免：下一次读得到，再读就没有 —— 恰好拦住那一下 click。
    let first = view.update(cx, |v, _| v.consume_tap_suppression());
    let second = view.update(cx, |v, _| v.consume_tap_suppression());
    assert!(first, "拖选松手应拦住紧跟着的 click");
    assert!(!second, "豁免是一次性的");

    // 复制：与可视文本一致（TextMap::slice）。
    view.update(cx, |v, cx| v.copy_selection(cx));
    let copied = cx
        .update(|cx| cx.read_from_clipboard().expect("剪贴板应有内容").text())
        .expect("应是文本");
    let expected = view.read_with(cx, |v, _| {
        v.book()
            .expect("有书")
            .document()
            .text_map()
            .slice(selected.range)
    });
    assert_eq!(copied, expected, "复制内容应与可视文本一致");
}

/// 渲染出来的选字菜单必须真的把动作条圈进 `menu_bounds`。
///
/// 这条钉的是「点划线时闪一下」最可疑的那根线：gpui 的一击会发给**每一个**含按下点的
/// handler，菜单没有 occlude / bounds 是空的时候，点动作条会同时落到正文上，
/// 顶栏底栏被顺手翻出来。宿主测试不需要看见像素，只要 bounds 覆盖得住动作条就够了。
#[gpui_kit::test]
fn the_rendered_selection_menu_covers_its_actions(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    drag_select(cx, &view, handle);
    settle(cx, handle);
    let bounds = view
        .read_with(cx, |v, _| v.menu_bounds.get())
        .expect("菜单画上屏后 canvas 该回写 bounds");
    assert!(
        f32::from(bounds.size.width) >= 160.0,
        "菜单 bounds 太窄，动作条可能漏在命中区外：{bounds:?}"
    );
    assert!(
        f32::from(bounds.size.height) >= 88.0,
        "菜单 bounds 太矮，颜色行/动作行可能漏在命中区外：{bounds:?}"
    );
    assert!(
        view.read_with(cx, |v, _| v.taps_selection_ui(bounds.center())),
        "落在菜单上的那一击必须先被选中这一套吃掉"
    );
}

/// M6 验收：长按吸词 —— 点在词首字符上，吸出的是整个词（"Hello"，词界稳定），
/// 手势结束后菜单条开着。
#[gpui_kit::test]
fn a_long_press_snaps_to_the_word_and_opens_the_menu(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &latin_flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    scroll_mode(cx, handle);

    // 第 0 行的首字符「H」内取点：拉丁字宽约半个 em，1/4 em 处稳落第一字。
    let at = view.read_with(cx, |v, _| {
        let box_ = &v.lines()[0];
        point(
            px(v.prefs.margin_x + box_.x + box_.dominant_font_size() * 0.25),
            px(box_.y + box_.height * 0.5),
        )
    });
    for phase in [TouchPhase::Started, TouchPhase::Ended] {
        dispatch(
            cx,
            handle,
            LongPressEvent {
                phase,
                start_position: at,
                position: at,
            }
            .to_platform_input(),
        );
    }
    let (selection, menu) = view.read_with(cx, |v, _| (v.selection(), v.selection_menu_open()));
    let Some(selected) = selection else {
        panic!("长按应选中一个词");
    };
    let text = view.read_with(cx, |v, _| {
        v.book()
            .expect("有书")
            .document()
            .text_map()
            .slice(selected.range.clone())
    });
    assert_eq!(text, "Hello", "点在词首应吸出整词：{selected:?} → {text:?}");
    assert!(menu, "长按成选后菜单条应开着");
}

/// M6 验收：选中态下的一击——点选区内把菜单请回来、选区不动，点选区外清空，
/// 两下都消费（`page.rs` 不该再拿它们翻页）。
#[gpui_kit::test]
fn a_tap_with_active_selection_clears_or_keeps(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    drag_select(cx, &view, handle);
    let range = view
        .read_with(cx, |v, _| v.selection().map(|selected| selected.range))
        .expect("已选中");

    // 点在选区起点（起拖那个字必在选区内）：菜单回开，选区不动，这一击被消费。
    let inside = char_point(&view, cx, 1, 2);
    let (consumed, menu, still) = cx
        .update_window(handle, |_, window, cx| {
            view.update(cx, |v, cx| {
                let consumed = v.tap_with_selection(window, cx, inside);
                (
                    consumed,
                    v.selection_menu_open(),
                    v.selection().map(|s| s.range),
                )
            })
        })
        .expect("窗口应可更新");
    assert!(consumed, "点在选区内的一击应被选中消费");
    assert!(menu, "菜单条应回开");
    assert_eq!(still, Some(range), "选区不该动");

    // 点选区外（往下六行）：清空，这一击也被消费（不翻页）。
    let outside = char_point(&view, cx, 7, 2);
    let (consumed, cleared) = cx
        .update_window(handle, |_, window, cx| {
            view.update(cx, |v, cx| {
                let consumed = v.tap_with_selection(window, cx, outside);
                (consumed, v.selection().is_none())
            })
        })
        .expect("窗口应可更新");
    assert!(consumed, "点选区外的一击也应被消费");
    assert!(cleared, "点选区外应清空选中");
}

/// 菜单把选区清掉之后，落在菜单上的那一下仍要被「选中」这一套消费掉。
///
/// 钉的是 `tap_on_body` 里那三条判据的**顺序**：那颗「划线」「取消」在自己的 click
/// 里就清了选区，等这一下冒泡到正文时 `selection()` 已是 `None` —— 把
/// `taps_selection_ui` 那一步挪到「有没有选区」之后，这里就会返回 `false`，
/// 于是它既做了动作、又被当成「点了一下正文」，顶栏与底栏跟着一起冒出来。
///
/// 清完选区不 `settle`：真实时序里 click 处理跑在下一帧绘制之前，而绘制会把菜单
/// 那份锚位收掉（菜单已经不摆了）。`settle` 一下就把要测的那个窗口期抹平了。
#[gpui_kit::test]
fn a_tap_on_the_menu_is_consumed_even_after_it_cleared_the_selection(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    drag_select(cx, &view, handle);
    settle(cx, handle);
    // 拖选松手那一下攒下的「吃掉下一个 click」要先放掉：真实时序里它就是被正文那层
    // 吃掉的（`tap_on_body` 第一条判据）。不吃掉它，下面这一下会被它拦下来，
    // 测试就只是验了第一条判据、看着像验了菜单那条。
    assert!(
        view.update(cx, |v, _| v.consume_tap_suppression()),
        "拖选松手该攒下一次豁免"
    );
    let menu = view
        .read_with(cx, |v, _| v.menu_bounds.get())
        .expect("菜单上屏后该留下锚位");
    view.update(cx, |v, _| v.clear_selection());
    assert!(
        view.read_with(cx, |v, _| v.selection().is_none()),
        "这一条测的就是「选区已经没了」那个时刻"
    );
    let consumed = cx
        .update_window(handle, |_, window, cx| {
            view.update(cx, |v, cx| v.tap_on_body(window, cx, menu.center()))
        })
        .expect("窗口应可更新");
    assert!(
        consumed,
        "落在菜单上的一击要被消费掉，不该再往下开浮层或翻页"
    );
}

/// M6 验收：翻走即清 —— 分页翻页与换摆法都不留旧高亮（连续滚动不清，
/// `set_scroll_y` 不调 `clear_selection`，选中的字随滚动一起走）。
#[gpui_kit::test]
fn a_page_turn_and_a_mode_switch_clear_the_selection(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );

    // 分页翻页：清。
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    settle(cx, handle);
    drag_select(cx, &view, handle);
    view.update(cx, |v, cx| v.scroll_page(1, cx));
    assert!(
        view.read_with(cx, |v, _| v.selection().is_none()),
        "翻走即清"
    );
    // 让那一张平移走完，再挑下一次。命中的是**画出来的**位置（`page_shift`），而测试时钟
    // 不自己走 —— 不走完这一趟，第二次拖选按下去落在新页前方外整整一页的地方，
    // 选不出字来。这不是选中坏了，是这一帧的字确实还不在那儿。
    turn_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));

    // 换摆法：清。走「翻页方式」这条真实入口 —— 渲染帧解出来的那一档与读者亲手选的
    // 一样，都要经过 `adopt_mode` 里那次 `clear_selection`。
    drag_select(cx, &view, handle);
    scroll_mode(cx, handle);
    assert!(
        view.read_with(cx, |v, _| v.selection().is_none()),
        "换摆法即清"
    );
}
