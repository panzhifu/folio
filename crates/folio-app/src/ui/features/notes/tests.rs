//! 「笔记」那一屏的折堆：纯聚合那半（分组、组内排序、出处填法、搜索判据）随实现
//! 搬进了 `folio_core::store::notes` 的测试；这里留的是要真窗口的那半 —— 弹层的
//! 升降、命中区的兄弟关系。库用 `Store::in_memory()` —— 不碰真实书库。

use std::path::Path;

use folio_core::ChapterExt;
use folio_core::Config;
use folio_core::content::anchor::TextAnchor;
use folio_core::content::format::{Chapter, DocMeta, FlowDoc, Format};
use folio_core::store::notes::MarkEntry;
use folio_core::store::{AnnotationStyle, Store};
use gpui_kit::component::Root;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext, px, size};

use super::{NotesEvent, NotesView, row_text};
use crate::ui::services::app_store;
use crate::ui::services::config::ConfigHolder;

fn meta(title: &str) -> DocMeta {
    DocMeta {
        title: title.into(),
        authors: vec!["作者甲".into()],
        format: Format::Epub,
        size: 4096,
        ..DocMeta::default()
    }
}

/// 一条有指纹的锚点。坐标本身随便取，测试要的是它能不能原样回来。
fn anchor(chapter: u32, start: usize) -> TextAnchor {
    TextAnchor {
        chapter,
        start,
        end: start + 4,
        exact: "重点句子".into(),
        prefix: "前面的字".into(),
        suffix: "后面的字".into(),
        path: String::new(),
    }
}

/// 一本带章标题表的书 —— 出处那一句要从库里折出来。
fn book(store: &Store, title: &str, file: &str, chapters: &[&str]) -> i64 {
    let id = store.add_book(Path::new(file), &meta(title)).unwrap();
    store
        .save_flow(
            id,
            &FlowDoc {
                chapters: chapters.iter().map(|t| Chapter::inline(*t, "")).collect(),
                ..Default::default()
            },
            Format::Epub,
        )
        .unwrap();
    id
}

/// 一行里主行与第二行怎么分：笔记只能出现在其中一行。
///
/// 引文那格空着时笔记顶上去当主行 —— 这条判错一个 `&&`，同一句话就在屏幕上念两遍，
/// 而代码里只差一个条件。四种来路都钉住。
#[test]
fn the_note_is_never_both_the_headline_and_the_second_line() {
    let entry = |is_bookmark: bool, quote: &str, note: &str| MarkEntry {
        is_bookmark,
        id: 1,
        chapter: 0,
        offset: 0,
        place: "第一章".into(),
        quote: quote.into(),
        note: note.into(),
        color: None,
        stale: false,
        created_at: 0,
    };
    let place: gpui_kit::SharedString = "第一章".into();
    let bookmark: gpui_kit::SharedString = "书签".into();

    let (headline, second) = row_text(
        &entry(false, "原文", "我的话"),
        Some(&place),
        bookmark.clone(),
    );
    assert_eq!(headline.to_string(), "原文");
    assert_eq!(
        second.map(|second| second.to_string()).as_deref(),
        Some("我的话"),
        "引文与笔记各占一行"
    );

    let (headline, second) = row_text(&entry(false, "", "我的话"), Some(&place), bookmark.clone());
    assert_eq!(headline.to_string(), "我的话", "引文空着时笔记顶上去");
    assert!(second.is_none(), "同一句笔记不许摆两遍");

    let (headline, second) = row_text(
        &entry(false, "同一句", "同一句"),
        Some(&place),
        bookmark.clone(),
    );
    assert_eq!(headline.to_string(), "同一句");
    assert!(second.is_none(), "引文与笔记同一句时只留主行");

    let (headline, second) = row_text(&entry(true, "", ""), Some(&place), bookmark);
    assert_eq!(headline.to_string(), "书签", "书签没有原文时主行念「书签」");
    assert!(second.is_none());
}

/// 起一扇把「笔记」那一屏摆在根上的窗。底部弹层要真窗口才量得到视口高，而这一屏
/// 的渲染路径（取数 → 一本一张卡 → 点开抬弹层）正是这里要测的那条。
///
/// 窗口取 420×900：Compact 档，与手机上那一档同一形态（宽档的封顶与居中被跳过去）。
fn boot_window(cx: &mut TestAppContext, store: Store) -> (Entity<NotesView>, AnyWindowHandle) {
    cx.update(gpui_kit::init);
    let mut view = None;
    let handle: AnyWindowHandle = cx
        .open_window(size(px(420.), px(900.)), |window, cx| {
            app_store::install(store, cx);
            ConfigHolder::install(Config::default(), cx);
            let notes = cx.new(|cx| NotesView::boot(window, cx));
            view = Some(notes.clone());
            Root::new(notes, window, cx)
        })
        .into();
    let notes = view.expect("这一屏该建起来");
    // 第一份快照在装起来这一刻就取（与外壳同一条），再渲染一帧才有卡片可找。
    cx.update_window(handle, |_, _window, cx| {
        notes.update(cx, |notes, cx| notes.refresh(cx));
    })
    .expect("窗口应能渲染");
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("首帧应画出来");
    (notes, handle)
}

/// 一本一张卡、顶上那颗搜索框在。
///
/// 钉的是接线，不是样子：卡片的张数由「有批注的书」决定，框挂在这一屏的根上而不是
/// 滚动的列表里。排出来的三行字多大、封面那格歪不歪，只有真机看得出来。
#[gpui_kit::test]
fn one_card_per_book_under_a_search_box(cx: &mut TestAppContext) {
    let store = Store::in_memory().unwrap();
    let a = book(&store, "书甲", "/books/a.epub", &["一"]);
    let b = book(&store, "书乙", "/books/b.epub", &["一"]);
    store
        .add_annotation(
            a,
            &anchor(0, 0),
            "甲的话",
            "rose",
            AnnotationStyle::default(),
        )
        .unwrap();
    store
        .add_annotation(
            b,
            &anchor(0, 0),
            "乙的话",
            "amber",
            AnnotationStyle::default(),
        )
        .unwrap();
    let (_view, handle) = boot_window(cx, store);

    let found = cx
        .update_window(handle, |_, window, _| {
            (
                window.try_find("folio-notes").is_some(),
                window.try_find("notes-search").is_some(),
                window.try_find(format!("notes-card-{a}")).is_some(),
                window.try_find(format!("notes-card-{b}")).is_some(),
            )
        })
        .expect("窗口应能渲染");
    assert!(found.0, "这一屏应画出自己的根");
    assert!(found.1, "库里有的可搜，顶上那颗框就该在");
    assert!(found.2 && found.3, "两本书各留过话 → 两张卡");
}

/// 库里一处批注都没有：根照画，那颗框**不摆**。
///
/// 这一条与上一条是一对 —— 只测「有」，把框无条件摆上也过得了；而没东西可搜时
/// 摆一颗框是噪音（规范那条「不适用的动作要么藏起来，要么禁用并说清为什么」）。
#[gpui_kit::test]
fn an_empty_library_paints_no_search_box(cx: &mut TestAppContext) {
    let store = Store::in_memory().unwrap();
    let (_view, handle) = boot_window(cx, store);
    cx.update_window(handle, |_, window, _| {
        assert!(
            window.try_find("folio-notes").is_some(),
            "空库也该画出这一屏的根（画的是空状态，不是黑屏）"
        );
        assert!(
            window.try_find("notes-search").is_none(),
            "一处批注都没有时不该摆一颗搜索框"
        );
    })
    .expect("窗口应能渲染");
}

/// 点一张卡 → 这一本的逐条笔记那一层升起来，并且内容真交进去了。
///
/// 抬起是弹簧按**测试钟**走的多帧过程：不推开钟，露出高永远停在 0，卡片看着就像
/// 「按了没反应」（那条到点才挂载的判据见 `BottomSheetState::is_idle`）。
#[gpui_kit::test]
fn tapping_a_card_raises_that_books_notes(cx: &mut TestAppContext) {
    let store = Store::in_memory().unwrap();
    let a = book(&store, "书甲", "/books/a.epub", &["第一章"]);
    store
        .add_annotation(
            a,
            &anchor(0, 0),
            "甲的话",
            "rose",
            AnnotationStyle::default(),
        )
        .unwrap();
    let (_view, handle) = boot_window(cx, store);

    cx.update_window(handle, |_, window, cx| {
        window.click(format!("notes-card-{a}"), cx);
    })
    .expect("窗口应能渲染");
    for _ in 0..40 {
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(16));
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("渲染应成功");
    }
    cx.update_window(handle, |_, window, _| {
        let surface = window
            .try_find("bottom-sheet-surface")
            .expect("点开一张卡该把那一本的笔记抬成底部弹层");
        let height = f32::from(surface.bounds().size.height);
        let viewport = f32::from(window.viewport_size().height);
        assert!(
            height > viewport * 0.7,
            "展开那一档该是视口高的 85%，实测 {height} / {viewport}"
        );
        assert!(
            window.try_find("notes-sheet-body").is_some(),
            "抬起来时要把逐条那一栏交给弹层，不能只升一块空壳"
        );
    })
    .expect("窗口应能渲染");
}

/// 行尾那颗删除与「点文字跳原文」是两块**兄弟**命中区：删一条不该顺手把那本书打开。
///
/// gpui 把一次点击发给每个含按下点的 hitbox（不沿祖先链冒泡）。删除钮从前挂在可跳的
/// 那一层里，同一指下去删完还跳一次 —— 屏幕上就是「删掉一条笔记，书也开了」。这里
/// 钉住删除只删：事件不 `Open`、行从清单里消失、弹层还留在原地。
#[gpui_kit::test]
fn deleting_a_row_does_not_open_the_book(cx: &mut TestAppContext) {
    use std::cell::Cell;
    use std::rc::Rc;

    let store = Store::in_memory().unwrap();
    let a = book(&store, "书甲", "/books/a.epub", &["第一章"]);
    let id = store
        .add_annotation(
            a,
            &anchor(0, 0),
            "甲的话",
            "rose",
            AnnotationStyle::default(),
        )
        .unwrap();
    let (view, handle) = boot_window(cx, store);
    let opened = Rc::new(Cell::new(0usize));
    let _watch = cx.update({
        let opened = opened.clone();
        |cx| {
            cx.subscribe(&view, move |_, _: &NotesEvent, _| {
                opened.set(opened.get() + 1)
            })
        }
    });

    cx.update_window(handle, |_, window, cx| {
        window.click(format!("notes-card-{a}"), cx);
    })
    .expect("窗口应能渲染");
    for _ in 0..40 {
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(16));
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("渲染应成功");
    }

    cx.update_window(handle, |_, window, cx| {
        assert!(
            window.try_find(("notes-jump", id as usize)).is_some(),
            "弹层里该有这一条"
        );
        window.click(("notes-row-delete", id as usize), cx);
    })
    .expect("窗口应能渲染");
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(opened.get(), 0, "删一条不该同时打开那本书");
        assert!(
            window.try_find(("notes-jump", id as usize)).is_none(),
            "删掉的那一条该从清单里消失"
        );
        assert!(
            window.try_find("notes-sheet-body").is_some(),
            "删一条不该顺手把弹层收掉"
        );
    })
    .expect("窗口应能渲染");
}
