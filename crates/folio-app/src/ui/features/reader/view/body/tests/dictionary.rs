//! 选区查词典：动作条那一格落成状态、菜单收起、空词典当场给空结果。
//!
//! 有词典的那一趟查盘在 `services::dictionary` 的测试里（纯函数，不碰 gpui）；
//! 这里只钉阅读器这一侧的接线：状态起来、选区菜单收起、关掉之后状态清掉。

use super::harness::{drag_select, flow, open, prefs, settle};
use crate::ui::features::reader::model::dictionary::DictionaryStatus;
use crate::ui::features::reader::model::session::ReaderSession;
use folio_core::Locator;
use gpui_kit::TestAppContext;

#[gpui_kit::test]
fn selecting_a_word_opens_an_empty_dictionary_card_and_closing_clears_it(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    settle(cx, handle);
    drag_select(cx, &view, handle);

    // 这一台没有导入过词典：开仍然是开得起来的（动作条那一格此时其实不摆），
    // 弹层应当当场给空结果，而不是永远停在 Pending。
    view.update(cx, |v, cx| v.open_dictionary(cx));
    let (open, menu, query, status) = view.read_with(cx, |v: &ReaderSession, _| {
        (
            v.dictionary_open(),
            v.selection_menu_open(),
            v.dictionary.as_ref().map(|state| state.query.clone()),
            v.dictionary.as_ref().map(|state| state.status.clone()),
        )
    });
    assert!(open, "查词弹层该升起来");
    assert!(!menu, "选区菜单该收掉");
    let query = query.expect("弹层该带着查询词");
    assert!(!query.is_empty(), "空选区不该开这一层");
    match status {
        Some(DictionaryStatus::Done { hits }) => assert!(hits.is_empty(), "没导入词典时该是空结果"),
        other => panic!("没导入词典时该当场得到空结果，实际是 {other:?}"),
    }

    view.update(cx, |v, cx| v.close_dictionary(cx));
    assert!(
        !view.read_with(cx, |v: &ReaderSession, _| v.dictionary_open()),
        "关掉之后状态该清掉"
    );
}
