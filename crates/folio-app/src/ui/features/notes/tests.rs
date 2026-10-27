//! 「笔记」那一屏的折堆：`load_groups` 的分组、组内排序、出处填法与卡片上那个时长，
//! 加上搜索那一条判据。只依赖公开 API，库用 `Store::in_memory()` —— 不碰真实书库。

use std::path::Path;

use folio_core::Config;
use folio_core::content::anchor::TextAnchor;
use folio_core::content::format::{Chapter, DocMeta, FlowDoc, Format};
use folio_core::store::{AnnotationStyle, Store};
use gpui_kit::component::Root;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext, px, size};

use super::{NotesView, load_groups, matches_query, snippet};
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

#[test]
fn groups_by_book_with_bookmarks_first_and_places_resolved() {
    let store = Store::in_memory().unwrap();
    let a = book(&store, "书甲", "/books/a.epub", &["第一章", "第二章"]);
    let b = book(&store, "书乙", "/books/b.epub", &["乙一"]);
    store
        .add_annotation(
            a,
            &anchor(1, 10),
            "甲的笔记",
            "rose",
            AnnotationStyle::default(),
        )
        .unwrap();
    store.add_bookmark(a, &anchor(1, 0)).unwrap();
    store
        .add_annotation(
            b,
            &anchor(0, 0),
            "乙的笔记",
            "amber",
            AnnotationStyle::default(),
        )
        .unwrap();

    let groups = load_groups(&store).unwrap();
    assert_eq!(groups.len(), 2, "按书分组");
    let ga = groups.iter().find(|g| g.book_id == a).unwrap();
    assert_eq!(ga.marks.len(), 2);
    assert!(ga.marks[0].is_bookmark, "书签在前");
    assert!(!ga.marks[1].is_bookmark);
    assert_eq!(ga.marks[0].place, "第二章", "出处来自库里的章标题表");
    assert_eq!(ga.marks[1].place, "第二章");
    let gb = groups.iter().find(|g| g.book_id == b).unwrap();
    assert_eq!(gb.marks[0].place, "乙一");
}

/// 组间排序：最近有动静的书在最前 —— 读者回来先看到他刚才在读的那本。
#[test]
fn the_group_with_the_latest_activity_comes_first() {
    let store = Store::in_memory().unwrap();
    let a = book(&store, "书甲", "/books/a.epub", &["一"]);
    let b = book(&store, "书乙", "/books/b.epub", &["一"]);
    store
        .add_annotation(
            a,
            &anchor(0, 0),
            "早的",
            "amber",
            AnnotationStyle::default(),
        )
        .unwrap();
    // `now()` 是秒级时钟，同一秒内靠 id 兜底；这里靠「两条一批、后写的 id 更大」
    // 不够稳，直接按创建条数把先后写死：给乙的那条是第二批，created_at >= 甲的那条。
    std::thread::sleep(std::time::Duration::from_millis(1100));
    store
        .add_annotation(b, &anchor(0, 0), "晚的", "rose", AnnotationStyle::default())
        .unwrap();

    let groups = load_groups(&store).unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].title, "书乙", "最近留过话的书排最前");
    assert_eq!(groups[1].title, "书甲");
}

#[test]
fn an_excerpt_folds_whitespace_and_truncates() {
    assert_eq!(snippet("甲 乙\n丙丁"), "甲 乙 丙丁");
    let long = "字".repeat(60);
    let got = snippet(&long);
    assert_eq!(got.chars().count(), 49, "48 字加一个省略号");
    assert!(got.ends_with('…'));
}

/// 卡片上那句「读了多久」的数从哪来：`reading_sessions` 里**这一本**的合计，按本落到
/// 它自己那一组上。两条都要钉 —— 只断言甲那个数，「每组都填全库合计」这种错查不出来
/// （甲的 5640 恰好也是全库的合计）。
#[test]
fn each_card_carries_its_own_books_total_seconds() {
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
    store.record_session(a, 1_000, 6_040, 1_200).unwrap();
    store.record_session(a, 9_000, 9_600, 100).unwrap();

    let groups = load_groups(&store).unwrap();
    let ga = groups.iter().find(|g| g.book_id == a).unwrap();
    let gb = groups.iter().find(|g| g.book_id == b).unwrap();
    assert_eq!(ga.seconds, 5_640, "两次会话的合计");
    assert_eq!(gb.seconds, 0, "另一本没读过就是 0，不许借别人的数");
}

/// 搜索词对上一本书的几条路：书名、某一条的引文、某一条自己写的笔记、某一条的出处
/// 章名；三条都对不上就不摆这一本。
///
/// 词那一侧的小写由调用方折（`NotesView::query` 那格在订阅时就折了），这里折的是候选
/// 那一边 —— 所以拉丁书名那一条要拿**已经小写**的词来问，大小写不敏感才是这样来的。
#[test]
fn a_search_word_finds_a_book_by_title_quote_note_or_chapter() {
    let store = Store::in_memory().unwrap();
    let a = book(&store, "书甲", "/books/a.epub", &["第三章"]);
    let b = book(&store, "书乙", "/books/b.epub", &["第一章"]);
    let c = book(&store, "Moby Dick", "/books/c.epub", &["一"]);
    for id in [a, b, c] {
        store
            .add_annotation(
                id,
                &anchor(0, 0),
                if id == a { "我的一句话" } else { "" },
                "rose",
                AnnotationStyle::default(),
            )
            .unwrap();
    }

    let groups = load_groups(&store).unwrap();
    let find = |id: i64| groups.iter().find(|g| g.book_id == id).unwrap();
    assert!(matches_query(find(a), "书甲"), "书名对上");
    assert!(
        matches_query(find(a), "重点句子"),
        "引文对上（锚点里 exact 那句截出来的）"
    );
    assert!(matches_query(find(a), "我的"), "自己写的笔记对上");
    assert!(matches_query(find(b), "第一章"), "出处那个章名也算对上");
    assert!(matches_query(find(c), "moby"), "候选那一边折小写");
    assert!(!matches_query(find(b), "书甲"), "书名不在另一本上");
    assert!(!matches_query(find(b), "我的"), "那句笔记只在甲那一本里");
    assert!(!matches_query(find(b), "不存在"), "三条都对不上就不摆");
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
