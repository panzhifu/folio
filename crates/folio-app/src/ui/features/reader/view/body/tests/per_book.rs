//! 每本书一份阅读设置：开书叠差量、弹层那一格落进那一本、换一本回到默认。
//!
//! 这一组测的是**接线**而不是差量本身 —— `folio_core::infra::book_settings` 那 12 条
//! 管「默认 + 差量算得对不对」，这里管「屏幕上那一下改动到底进了哪一层」。走错一层的
//! 症状是读者的每次微调都改到了所有书，或者改完关掉书就没了。

use super::harness::{flow, meta, open_with_store};
use crate::ui::services::config::ConfigHolder;
use folio_core::store::Store;
use folio_core::{Appearance, BookSettings, Locator, Mode, Reader};
#[allow(unused_imports)] // `#[gpui_kit::test]` 展开要它，unused 分析看不见宏里的使用
use gpui_kit::test::TestWindowExt;
#[allow(unused_imports)] // 同上：`AppContext` 给 `cx.update` 用
use gpui_kit::{AppContext as _, TestAppContext};
use std::sync::Arc;

/// 一本入库的书 + 它的句柄。这一组不读正文（差量那一列与正文无关），`add_book` 就够。
fn store_with_book(title: &str) -> (Arc<Store>, i64) {
    let store = Arc::new(Store::in_memory().expect("内存库"));
    let mut meta = meta("epub");
    meta.title = title.into();
    let id = store
        .add_book(
            &std::path::Path::new("/books").join(format!("{title}.epub")),
            &meta,
        )
        .expect("入库");
    (store, id)
}

/// 把一份排版选择记成差量（与默认逐格比）。
fn reader_patch(set: impl FnOnce(&mut Reader)) -> BookSettings {
    let base = Reader::default();
    let mut wanted = base.clone();
    set(&mut wanted);
    let mut patch = BookSettings::default();
    patch.record_reader(&base, &wanted);
    patch
}

/// 用这批夹具开一屏（默认层就是 `Reader::default()`，好处是断言能直接比默认值）。
fn open(cx: &mut TestAppContext, store: Arc<Store>, book_id: i64) -> gpui_kit::AnyWindowHandle {
    let (_session, handle) = open_with_store(
        cx,
        &flow(),
        "epub",
        Reader::default(),
        Locator::start(),
        390.0,
        Some(store),
        Some(book_id),
    );
    handle
}

/// 开书会把库里那一列的差量叠到生效值上，而**默认层一个字都不动**。
///
/// 顺序也在这条里：叠差量必须早于 `Book::new`，否则这一本先用上一本的偏好排一次、
/// 下一帧再整章重排 —— 那种写法画面是对的，只多排一遍，屏幕上看不出来。
#[gpui_kit::test]
fn opening_a_book_lands_its_own_settings(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (store, id) = store_with_book("甲本");
    store
        .set_book_settings(id, &reader_patch(|p| p.font_size = 26.0))
        .expect("落差量");

    open(cx, store, id);

    cx.update(|cx| {
        assert_eq!(
            ConfigHolder::reader(cx).font_size,
            26.0,
            "开书该把这一本的差量叠上"
        );
        assert_eq!(
            ConfigHolder::default_reader(cx).font_size,
            Reader::default().font_size,
            "这一本的字号不许出现在默认层里"
        );
    });
}

/// 弹层那一格的写入落进**这一本的库行**，而不是配置文件。
///
/// 落库走会话每帧那一次比较（`sync_book_settings`），所以要画一帧才算走完生产那条路；
/// 只断言内存里的生效值会漏掉「忘了落盘」这一整类错误。
#[gpui_kit::test]
fn the_sheet_writes_into_this_books_row(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (store, id) = store_with_book("甲本");
    let handle = open(cx, store.clone(), id);

    cx.update(|cx| ConfigHolder::edit_book_reader(cx, |p| p.margin_x = 40.0));
    // 差量要安静够那一档才落库（`SETTINGS_SETTLE_FRAMES`），所以画到第 24 帧。
    frames(cx, handle, SETTLE_FRAMES);

    let saved = store.book_settings(id).expect("读回这一本的差量");
    assert_eq!(
        saved.effective_reader(&Reader::default()).margin_x,
        40.0,
        "弹层改的那一格该落进这一本的行"
    );
    cx.update(|cx| {
        assert_eq!(ConfigHolder::reader(cx).margin_x, 40.0, "生效值当场跟着变");
        assert_eq!(
            ConfigHolder::default_reader(cx).margin_x,
            Reader::default().margin_x,
            "配置文件那一层不该被这一本碰过"
        );
    });
}

/// 差量落库要安静的那一帧数（与 `session::SETTINGS_SETTLE_FRAMES` 同一个数，不抄第二份）。
use crate::ui::features::reader::model::session::SETTINGS_SETTLE_FRAMES as SETTLE_FRAMES;

/// 画 n 帧。差量那一趟挂在渲染帧上（`sync_book_settings` 每帧一次），不画帧就走不完。
fn frames(cx: &mut TestAppContext, handle: gpui_kit::AnyWindowHandle, n: u32) {
    for _ in 0..n {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
        })
        .expect("这一屏该画得出来");
    }
}

/// 只画一帧：那条去抖的门要的节奏是「每拖一格画一帧」，而 `draw` 是画到落库那一档。
fn draw_one(cx: &mut TestAppContext, handle: gpui_kit::AnyWindowHandle) {
    frames(cx, handle, 1);
}

/// 拖一格**不该写一次库**：差量攒到安静一档才落，离席那一步兜底。
///
/// 反过来的形状是实测过的疼：一次同步 SQLite 写摊上 WAL checkpoint 就没有上界
/// （位置那一趟为此改成离席才结，`model/position.rs` 记着 34.75 ms 那一次），
/// 而一次拖动要走几十格。这条门三个数都得对上：
/// 一边拖一边画帧 ⇒ **一次都不写**；停手画满那一档 ⇒ **写一次，且是最后一格**；
/// 没停手就走 ⇒ 离席那一步当场写掉。
#[gpui_kit::test]
fn a_drag_writes_the_books_row_once_instead_of_once_per_tick(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (store, id) = store_with_book("甲本");
    let (session, handle) = open_with_store(
        cx,
        &flow(),
        "epub",
        Reader::default(),
        Locator::start(),
        390.0,
        Some(store.clone()),
        Some(id),
    );

    for tick in 0..40 {
        cx.update(|cx| {
            ConfigHolder::edit_book_reader(cx, move |p| p.margin_x = 24.0 + tick as f32)
        });
        draw_one(cx, handle);
    }
    assert!(
        store.book_settings(id).expect("读回那一列").is_empty(),
        "每拖一格写一次库：40 帧里至少已经落过一笔"
    );

    // 停手：安静满那一档，落下的是**最后一格**那个值。
    frames(cx, handle, SETTLE_FRAMES);
    let saved = store.book_settings(id).expect("读回那一列");
    assert_eq!(
        saved.effective_reader(&Reader::default()).margin_x,
        24.0 + 39.0,
        "落库的该是最后一次那格，不是中途某一格"
    );

    // 没停手就走：离席结账那一步（与位置同一条路）当场把欠的写掉。
    cx.update(|cx| {
        ConfigHolder::edit_book_reader(cx, |p| p.margin_x = 8.0);
        session.update(cx, |session, cx| {
            session.persist_position();
            session.flush_book_settings(cx);
        });
    });
    assert_eq!(
        store
            .book_settings(id)
            .expect("读回那一列")
            .effective_reader(&Reader::default())
            .margin_x,
        8.0,
        "离席还不结这一笔，进程被回收就是「改过又没了」"
    );
}

/// 两本书各一份：甲本调过、乙本跟默认、回到甲本的选择还在库里。
///
/// 这一条才是「按书存」这件事本身；写回全局那一层的话，乙本那一步就红。
#[gpui_kit::test]
fn two_books_keep_their_own_settings(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let store = Arc::new(Store::in_memory().expect("内存库"));
    let mut meta_a = meta("epub");
    meta_a.title = "甲本".into();
    let mut meta_b = meta("epub");
    meta_b.title = "乙本".into();
    let a = store
        .add_book(std::path::Path::new("/books/a.epub"), &meta_a)
        .unwrap();
    let b = store
        .add_book(std::path::Path::new("/books/b.epub"), &meta_b)
        .unwrap();

    let handle_a = open(cx, store.clone(), a);
    cx.update(|cx| ConfigHolder::edit_book_reader(cx, |p| p.line_height = 2.4));
    // 落库要安静满那一档（见 `draw`），甲本这一格改完得画够才进得了那一行。
    draw(cx, handle_a);
    assert_eq!(
        cx.update(|cx| ConfigHolder::reader(cx)).line_height,
        2.4,
        "甲本当场就该是这一档"
    );

    // 换到没单独设过的乙本：整层被替换，甲本那一格不许留下。
    open(cx, store.clone(), b);
    assert_eq!(
        cx.update(|cx| ConfigHolder::reader(cx)).line_height,
        Reader::default().line_height,
        "乙本不该带着甲本的行距 —— 开书是换整层，不是再叠一层"
    );

    // 关书（撤下叠层）之后，甲本的行距仍在它自己那一行上。
    cx.update(|cx| ConfigHolder::set_book(cx, None));
    assert_eq!(
        store
            .book_settings(a)
            .expect("甲本的差量")
            .effective_reader(&Reader::default())
            .line_height,
        2.4,
        "甲本的选择该在库里"
    );
    assert!(
        store.book_settings(b).expect("乙本的差量").is_empty(),
        "乙本没被单独设过，那一列该是空差量"
    );
}

/// 这一本的夜里**不碰软件外观的浅深**，只落在自己那一行上。
///
/// 「阅读页夜里读、关掉书书架还是白天」这一件事的写入侧；读侧的门槛在外壳那一条
/// （`reading_mounted` 才顶掉 `mode`）。
#[gpui_kit::test]
fn a_books_night_setting_stays_off_the_app_mode(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (store, id) = store_with_book("甲本");
    open(cx, store.clone(), id);

    cx.update(|cx| ConfigHolder::edit_book_appearance(cx, |a| a.reading_mode = Some(Mode::Dark)));

    cx.update(|cx| {
        assert_eq!(
            ConfigHolder::appearance(cx).reading_mode,
            Some(Mode::Dark),
            "这一本的阅读浅深该在生效值上"
        );
        assert_eq!(
            ConfigHolder::appearance(cx).mode,
            Mode::default(),
            "全局浅深归设置屏那一格管，这本书碰不到它"
        );
    });
    assert_eq!(
        store
            .book_settings(id)
            .expect("这一本的差量")
            .effective_appearance(&Appearance::default())
            .mode,
        Mode::default(),
        "叠完之后的生效值里，只归软件那一格也要还是默认"
    );
}

/// 把一份外观选择记成差量。
fn appearance_patch(set: impl FnOnce(&mut Appearance)) -> BookSettings {
    let base = Appearance::default();
    let mut wanted = base.clone();
    set(&mut wanted);
    let mut patch = BookSettings::default();
    patch.record_appearance(&base, &wanted);
    patch
}

/// 画到「差量该落库」那一档：会话每帧比较一次 `book_rev`，而停手之后要**安静满
/// `SETTINGS_SETTLE_FRAMES` 帧**才把差量抄进库（弹层每拖一格换一次号，一次拖动几十格）。
/// 所以这一组验落库的都得走满那一档，一帧不够。
fn draw(cx: &mut TestAppContext, handle: gpui_kit::AnyWindowHandle) {
    frames(cx, handle, SETTLE_FRAMES);
}

/// 「恢复本书默认」那一格抹掉整份差量，并把空差量落回库里那一列。
///
/// 走的是与改一格同一条路（生效层换一下、会话每帧抄一次），所以这里同时验「清得干净」
/// 与「清这件事也落盘了」—— 只清内存的症状是重开书又全都回来。
#[gpui_kit::test]
fn clearing_a_book_returns_it_to_the_defaults(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (store, id) = store_with_book("甲本");
    let handle = open(cx, store.clone(), id);
    cx.update(|cx| {
        ConfigHolder::edit_book_reader(cx, |p| p.font_size = 26.0);
        ConfigHolder::edit_book_appearance(cx, |a| a.reading_mode = Some(Mode::Dark));
    });
    draw(cx, handle);
    assert!(
        !store.book_settings(id).expect("这一本的差量").is_empty(),
        "前置：那两格改动该已经落库"
    );

    cx.update(ConfigHolder::clear_book);
    draw(cx, handle);

    assert!(
        store.book_settings(id).expect("这一本的差量").is_empty(),
        "那一格该被抹干净"
    );
    cx.update(|cx| {
        assert_eq!(
            ConfigHolder::reader(cx).font_size,
            Reader::default().font_size,
            "生效值该回到默认档"
        );
        assert_eq!(ConfigHolder::appearance(cx).reading_mode, None);
    });
}

/// 设置屏那一档**传到没单独设过的书**，而设过的那一本盖住它。
///
/// 这一条是「只存差量」而不是「第一次改动就快照整份」要买到的东西：快照那一版写出来，
/// 老书从此不再跟默认走，而读者看不出为什么。
#[gpui_kit::test]
fn the_default_reading_mode_reaches_books_that_never_set_their_own(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let store = Arc::new(Store::in_memory().expect("内存库"));
    let mut meta_a = meta("epub");
    meta_a.title = "甲本".into();
    let mut meta_b = meta("epub");
    meta_b.title = "乙本".into();
    let a = store
        .add_book(std::path::Path::new("/books/a.epub"), &meta_a)
        .unwrap();
    let b = store
        .add_book(std::path::Path::new("/books/b.epub"), &meta_b)
        .unwrap();
    // 甲本自己钉了浅色；乙本那一列是空的。
    store
        .set_book_settings(a, &appearance_patch(|x| x.reading_mode = Some(Mode::Light)))
        .unwrap();

    open(cx, store.clone(), b);
    cx.update(|cx| {
        ConfigHolder::edit_appearance(cx, |a| a.reading_mode = Some(Mode::Dark));
    });
    assert_eq!(
        cx.update(|cx| ConfigHolder::appearance(cx).reading_mode),
        Some(Mode::Dark),
        "乙本没单独设过，该跟着设置屏那一档"
    );

    open(cx, store, a);
    assert_eq!(
        cx.update(|cx| ConfigHolder::appearance(cx).reading_mode),
        Some(Mode::Light),
        "甲本自己钉过浅色，默认档改成夜里也不该跟着翻"
    );
}

/// 没有书挂着时这条写入路什么都不动（连默认层也不动）。
///
/// 弹层那些控件只存在于阅读层里，真走到这里说明调用点错了；把它降级成「改默认」会让
/// 「这一本的字号」悄悄变成全局选择 —— 那种错要红在这里，不该安静生效。
#[gpui_kit::test]
fn a_book_write_without_a_book_open_changes_nothing(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    cx.update(|cx| {
        ConfigHolder::install(folio_core::Config::default(), cx);
        ConfigHolder::edit_book_reader(cx, |p| p.font_size = 40.0);
    });
    cx.update(|cx| {
        assert_eq!(
            ConfigHolder::reader(cx).font_size,
            Reader::default().font_size,
            "没有书挂着时生效值不该被改动"
        );
        assert_eq!(
            ConfigHolder::default_reader(cx).font_size,
            Reader::default().font_size,
            "也不该顺手改到默认层"
        );
    });
}
