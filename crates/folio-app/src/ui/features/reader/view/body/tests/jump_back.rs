//! 正文点链接/脚注跳出去之后的「回原处」：只留一层，回来就用掉。
//!
//! 这一族走的是真链路：记来处与落回去都调生产里那两个口（`remember_jump_origin` /
//! `back_to_jump_origin`），而落点复用检索命中与「定位原文」那一条 `goto_offset`，
//! 所以「跳转怎么落地」这一段是被真跑的。没测的只有 `open_link_at` 里那句
//! 「跳之前先记一下来处」—— 那一句要真 `<a href>` 与它的命中点，归到真机那一次看。

use super::harness::{chapters_flow, offset_of, open, prefs, scroll_down, scroll_mode, settle};
use folio_core::Locator;
#[allow(unused_imports)] // `#[gpui_kit::test]` 展开要它，unused 分析看不见宏里的使用
use gpui_kit::{AppContext as _, TestAppContext};

/// 跳出去之前那一处被记下来；回来落的正是那一处；那一格同时收掉。
#[gpui_kit::test]
fn a_link_jump_keeps_one_origin_and_returning_lands_back_on_it(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &chapters_flow(4),
        "epub",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    scroll_mode(cx, handle);
    // 先往下滚一段：偏移还停在章首 0 的话，「回到原处」与「跳到章首」得到同一个数，
    // 落错了测试也看不出来。
    scroll_down(cx, handle, 400.0);
    settle(cx, handle);
    let (origin_chapter, origin_offset) = view
        .read_with(cx, |v, _| v.resolve_position())
        .expect("开书之后该问得出当前位置");
    assert!(
        origin_offset > 0,
        "滚过一段，章内偏移该离开章首：{origin_offset}"
    );

    // 与点一条链接同一对动作：记来处 → 沿目录那一条路跳过去。
    view.update(cx, |v, cx| {
        v.remember_jump_origin(3);
        v.goto_entry(3, None, cx);
    });
    settle(cx, handle);

    assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 3);
    assert_eq!(
        view.read_with(cx, |v, _| v.jump_back),
        Some(Locator {
            chapter: origin_chapter as u32,
            offset: origin_offset,
        }),
        "记的该是跳出去之前那一处"
    );

    view.update(cx, |v, cx| v.back_to_jump_origin(cx));
    settle(cx, handle);

    assert_eq!(
        view.read_with(cx, |v, _| v.chapter_index()),
        origin_chapter,
        "落回原来那一章"
    );
    assert_eq!(
        offset_of(&view, cx),
        origin_offset,
        "落回原来那一处：跳出去时在哪一行，回来还在那一行"
    );
    assert!(
        view.read_with(cx, |v, _| v.jump_back).is_none(),
        "只有一层，用完就收掉，屏上不该还留着那一格"
    );
}

/// 原地跳不留那一格：同一章的章首就是现在这一处，留了等于摆一个点了没反应的按钮。
#[gpui_kit::test]
fn a_jump_that_does_not_move_the_page_records_nothing(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &chapters_flow(4),
        "epub",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    scroll_mode(cx, handle);
    settle(cx, handle);

    view.update(cx, |v, _| v.remember_jump_origin(0));

    assert_eq!(
        view.read_with(cx, |v, _| v.jump_back),
        None,
        "章首 → 同一章的章首，不该留下一格「回原处」"
    );
}

/// 第二跳把来处换成**最新那一处**，不往下堆：读者按那一格要回的是刚才那一步，
/// 不是三步之前 —— 这也是这一族只做一层的原因。
#[gpui_kit::test]
fn a_second_jump_replaces_the_single_origin(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &chapters_flow(4),
        "epub",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    scroll_mode(cx, handle);
    scroll_down(cx, handle, 400.0);
    settle(cx, handle);

    view.update(cx, |v, cx| {
        v.remember_jump_origin(2);
        v.goto_entry(2, None, cx);
    });
    settle(cx, handle);
    // 落在第 2 章的章首（偏移 0，章不是目标章 ⇒ 这一跳有得记）。
    view.update(cx, |v, _| v.remember_jump_origin(3));

    assert_eq!(
        view.read_with(cx, |v, _| v.jump_back),
        Some(Locator {
            chapter: 2,
            offset: 0,
        }),
        "来处换成刚离开的那一处，而不是第一跳那一处"
    );
}
