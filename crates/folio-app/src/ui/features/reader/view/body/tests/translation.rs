//! 选区翻译：缓存命中的那一趟不开弹层之外的任何事 —— 不出网、菜单收掉、译文就位。
//! 出网那一趟在测试执行器里没人泵（与全文搜索同一条 known 限制），它的形状由
//! `translation_event` 的 seq 守卫与 core 的解析测试兜着。

use super::harness::{drag_select, flow, meta, open_with_store, prefs, settle};
use crate::ui::features::reader::model::session::ReaderSession;
use crate::ui::features::reader::model::translation::TranslationStatus;
use crate::ui::services::config::ConfigHolder;
use folio_core::net::translate::{configured, source_hash};
use folio_core::store::Store;
use folio_core::{Format, Locator, Translation, TranslationEngine};
use gpui_kit::{Entity, TestAppContext};
use std::sync::Arc;

/// 开一本 markdown、选中一段，返回会话与选中的源文。
fn open_and_select(cx: &mut TestAppContext, store: &Arc<Store>) -> (Entity<ReaderSession>, String) {
    let flow = flow();
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
    let source = view.read_with(cx, |v: &ReaderSession, _| {
        v.selected_text().expect("已选中").trim().to_string()
    });
    assert!(!source.is_empty());
    (view, source)
}

/// 目标语言取的是「跟随界面语言」解析出的那一档（测试配置默认 zh-CN）——
/// 缓存键必须用它写，命中才测得出来。
fn target_lang(cx: &mut TestAppContext) -> String {
    cx.update(|cx| ConfigHolder::language(cx).locale.code().to_string())
}

#[gpui_kit::test]
fn a_cached_translation_lands_without_a_network_round_trip(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let store = Arc::new(Store::in_memory().expect("内存库"));
    let (view, source) = open_and_select(cx, &store);
    // 缓存键由引擎自己给：默认配置是 Google 那一档。
    let provider = configured(&Translation::default()).cache_key();
    store
        .save_translation(
            &provider,
            "auto",
            &target_lang(cx),
            &source_hash(&source),
            "预存的译文",
        )
        .expect("预存缓存");

    view.update(cx, |v, cx| v.open_translation(cx));

    let (status, menu, open, engine) = view.read_with(cx, |v, _| {
        (
            v.translation.as_ref().map(|state| state.status.clone()),
            v.selection_menu_open(),
            v.translation.is_some(),
            v.translation.as_ref().map(|state| state.engine.clone()),
        )
    });
    assert!(open, "弹层该升起来");
    assert!(!menu, "选区菜单该收掉");
    assert_eq!(engine.as_deref(), Some("Google"));
    match status {
        Some(TranslationStatus::Done { text, cached, .. }) => {
            assert_eq!(text, "预存的译文", "命中的是缓存里那份");
            assert!(cached, "这一份没出网，照实说");
        }
        other => panic!("缓存命中的那趟该立刻到手，实际是 {other:?}"),
    }
}

/// 换了引擎就该读那只引擎的缓存：同一段字、同一对语言，Google 的旧答案不能顶上来。
/// 这条同时钉住「provider 由引擎给」与「弹层头部显的是当前引擎名」。
#[gpui_kit::test]
fn switching_the_engine_reads_that_engines_cache(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let store = Arc::new(Store::in_memory().expect("内存库"));
    let (view, source) = open_and_select(cx, &store);
    let target = target_lang(cx);
    let hash = source_hash(&source);
    store
        .save_translation("google", "auto", &target, &hash, "Google 旧译")
        .expect("预存 Google 那份");
    store
        .save_translation("deepl", "auto", &target, &hash, "DeepL 新译")
        .expect("预存 DeepL 那份");

    cx.update(|cx| {
        ConfigHolder::edit_translation(cx, |translation| {
            translation.engine = TranslationEngine::DeepL;
        })
    });
    view.update(cx, |v, cx| v.open_translation(cx));

    let (status, engine) = view.read_with(cx, |v, _| {
        (
            v.translation.as_ref().map(|state| state.status.clone()),
            v.translation.as_ref().map(|state| state.engine.clone()),
        )
    });
    assert_eq!(engine.as_deref(), Some("DeepL"));
    match status {
        Some(TranslationStatus::Done { text, cached, .. }) => {
            assert_eq!(text, "DeepL 新译", "换引擎不许拿上一只的答案");
            assert!(cached, "DeepL 那份是缓存命中，不该出网");
        }
        other => panic!("DeepL 缓存该立刻到手，实际是 {other:?}"),
    }
}
