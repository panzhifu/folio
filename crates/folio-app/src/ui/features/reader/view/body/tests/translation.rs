//! 选区翻译：缓存命中的那一趟不开弹层之外的任何事 —— 不出网、菜单收掉、译文就位。
//! 出网那一趟在测试执行器里没人泵（与全文搜索同一条known限制），它的形状由
//! `translation_event` 的 seq 守卫与 core 的解析测试兜着。

use super::harness::{drag_select, flow, meta, open_with_store, prefs, settle};
use crate::ui::features::reader::model::translation::TranslationStatus;
use crate::ui::features::reader::model::session::ReaderSession;
use folio_core::net::translate::{GoogleTranslator, source_hash};
use folio_core::store::Store;
use folio_core::{Format, Locator};
use gpui_kit::TestAppContext;
use std::sync::Arc;

#[gpui_kit::test]
fn a_cached_translation_lands_without_a_network_round_trip(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let store = Arc::new(Store::in_memory().expect("内存库"));
    let id = store
        .add_book(std::path::Path::new("memory://fixture"), &meta("markdown"))
        .expect("入库");
    store
        .save_flow(id, &flow, Format::Markdown)
        .expect("章节入库");
    let (view, handle) = open_with_store(
        cx,
        &flow,
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
        Some(store.clone()),
        Some(id),
    );
    settle(cx, handle);
    drag_select(cx, &view, handle);
    let source = view
        .read_with(cx, |v: &ReaderSession, _| {
            v.selected_text().expect("已选中").trim().to_string()
        });
    assert!(!source.is_empty());

    // 目标语言取的是「跟随界面语言」解析出的那一档（测试配置默认 zh-CN）——
    // 缓存键必须用它写，命中才测得出来。
    let target = cx.update(|cx| {
        crate::ui::services::config::ConfigHolder::language(cx)
            .locale
            .code()
            .to_string()
    });
    store
        .save_translation(
            GoogleTranslator::NAME,
            "auto",
            &target,
            &source_hash(&source),
            "预存的译文",
        )
        .expect("预存缓存");

    view.update(cx, |v, cx| v.open_translation(cx));

    let (status, menu, open) = view.read_with(cx, |v, _| {
        (
            v.translation.as_ref().map(|state| state.status.clone()),
            v.selection_menu_open(),
            v.translation.is_some(),
        )
    });
    assert!(open, "弹层该升起来");
    assert!(!menu, "选区菜单该收掉");
    match status {
        Some(TranslationStatus::Done {
            text, cached, ..
        }) => {
            assert_eq!(text, "预存的译文", "命中的是缓存里那份");
            assert!(cached, "这一份没出网，照实说");
        }
        other => panic!("缓存命中的那趟该立刻到手，实际是 {other:?}"),
    }
}
