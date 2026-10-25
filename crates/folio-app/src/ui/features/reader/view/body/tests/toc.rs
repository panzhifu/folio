//! 目录跳转与锚点落点。

use super::harness::{
    PARA, meta, offset_of, open, open_scroll, open_with_store, prefs, scroll_mode, settle,
};
use crate::ui::features::reader::model::session::ReaderSession;
use folio_core::content::format::TocEntry;
use folio_core::store::Store;
use folio_core::{Chapter, FlowDoc, Format, Locator};
#[allow(unused_imports)] // `#[gpui_kit::test]` 展开要它，unused 分析看不见宏里的使用
use gpui_kit::{AppContext as _, Entity, TestAppContext};
use std::sync::Arc;

/// 目录跳转用的语料：每章带着自己的 `spine_href`（目录条目靠它对上章号），正文第 6 段
/// 写着 `id="mid{i}"`，目录里每章给两条 —— 一条指章首、一条指那个锚点。真书里「一个
/// spine 文件挂着好几条小节」（一章对十条 nav 条目）就是这个形状。
///
/// 锚点放在第 6 段而不是第一段：锚点如果就落在章首第一个字上，「跳到锚点」与
/// 「跳到章首」得到同一个偏移，锚点那条换算整个坏掉了测试也看不出来。
fn anchored_flow(count: usize) -> FlowDoc {
    FlowDoc {
        chapters: (0..count)
            .map(|i| {
                let source = (0..30)
                    .map(|k| {
                        if k == 6 {
                            format!("<p id=\"mid{i}\">锚点第{i}章{PARA}</p>")
                        } else {
                            format!("<p>{PARA}（第{i}章 第{k}段）</p>")
                        }
                    })
                    .collect::<String>();
                Chapter::inline(format!("第{i}章"), source).with_spine_href(format!("c{i}.xhtml"))
            })
            .collect(),
        toc: (0..count)
            .flat_map(|i| {
                [
                    TocEntry {
                        title: format!("第{i}章"),
                        href: format!("c{i}.xhtml"),
                        ..Default::default()
                    },
                    TocEntry {
                        title: format!("第{i}章第二节"),
                        href: format!("c{i}.xhtml"),
                        fragment: Some(format!("mid{i}")),
                        ..Default::default()
                    },
                ]
            })
            .collect(),
        ..Default::default()
    }
}

/// 目录里某一条的锚点在这本书的章内偏移表里的位置。
///
/// 偏移表（`Document::id_offsets`）是排版产物，问的是**当前锚章**那一份，所以只能在
/// 跳到那一章、并且那一章排完之后问。落点断言拿它当基准，而不是在测试里写死一个数 ——
/// 写死的数换个字体就变了，变了会让测试红在几何上而不是红在跳转上。
///
/// 落点本身读视口顶那一行反解出来的章内偏移（[`offset_of`]）：章内偏移就是进度坐标系，
/// 落点对了行自然对，而行序还夹着缩进与换行，多一层要解释的东西。
fn anchor_offset(view: &Entity<ReaderSession>, id: &str, cx: &mut TestAppContext) -> usize {
    view.read_with(cx, |v, _| {
        v.book()
            .and_then(|book| book.document().id_offsets().get(id).copied())
            .unwrap_or_else(|| panic!("锚点 {id} 该在偏移表里"))
    })
}

// ---- 目录跳转：读者点目录里的一条，屏上真的换成那一处 ----
/// 目录每一行带着「跳去哪儿」的两个数：`chapter` 由 `spine_href` 查表得到，
/// `fragment` 是那条目录自己的 `#` 后面一截。这两格错了，目录那一屏照样画得出来、
/// 标题也对，点下去才发现送到别处 —— 那是最难从截图看出来的错。
#[gpui_kit::test]
fn each_toc_row_carries_where_clicking_it_goes(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, _) = open(
        cx,
        &anchored_flow(4),
        "epub",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    let rows = view.read_with(cx, |v, _| {
        v.toc_rows()
            .iter()
            .map(|row| (row.title.clone(), row.chapter, row.fragment.clone()))
            .collect::<Vec<_>>()
    });
    assert_eq!(rows.len(), 8, "四章各两条目录");
    assert_eq!(
        rows[0],
        ("第0章".to_string(), Some(0), None),
        "指章首的那条：有章号、没有锚点"
    );
    assert_eq!(
        rows[1],
        ("第0章第二节".to_string(), Some(0), Some("mid0".to_string())),
        "指小节的那条：同一个章号，加上它自己的锚点"
    );
    // 对不上正文的条目会带 `chapter == None`，界面按「画出来但点不动」处置。
    // 这一本每一条都对得上；哪天查表坏了，这一条先红。
    assert!(
        rows.iter().all(|(_, chapter, _)| chapter.is_some()),
        "有目录条目对不上章节：{rows:?}"
    );
}

/// 点一条指向别的章的目录：锚章换过去、屏上摆那一章的开头、恢复请求在锚章排完那一帧
/// 被取走、位置当场落库（杀掉进程回来还在这一章），「来过」集合也记上这一章。
#[gpui_kit::test]
fn a_toc_jump_moves_the_screen_to_that_chapter_and_saves_it(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = anchored_flow(5);
    let store = Arc::new(Store::in_memory().expect("内存库"));
    let id = store
        .add_book(std::path::Path::new("memory://fixture"), &meta("epub"))
        .expect("入库");
    store.save_flow(id, &flow, Format::Epub).expect("章节入库");
    let (view, handle) = open_with_store(
        cx,
        &flow,
        "epub",
        prefs(16.0),
        Locator::start(),
        460.0,
        Some(store.clone()),
        Some(id),
    );
    assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 0);

    view.update(cx, |v, cx| v.goto_entry(3, None, cx));
    settle(cx, handle);

    assert_eq!(
        view.read_with(cx, |v, _| v.chapter_index()),
        3,
        "锚章该挪到第 3 章"
    );
    assert_eq!(offset_of(&view, cx), 0, "指章首的那条目录，落点就是章首");
    assert!(
        !view.read_with(cx, |v, _| v.has_pending_restore()),
        "恢复请求该在锚章排完那一帧被取走，而不是永远挂着"
    );
    let texts = view.read_with(cx, |v, _| v.line_texts());
    assert!(
        texts.iter().any(|text| text.contains("第3章 第0段")),
        "屏上该是第 3 章的开头：{texts:?}"
    );
    assert!(
        !texts.iter().any(|text| text.contains("第0章")),
        "第 0 章的行一份都不该在这份排版里：{texts:?}"
    );

    let (_, locator) = store.position(id).expect("读回进度").expect("进度应存在");
    assert_eq!(
        locator,
        Locator {
            chapter: 3,
            offset: 0
        },
        "跳转当场就把位置写了盘：读者杀进程回来还在第 3 章"
    );
    assert!(
        view.read_with(cx, |v, _| v.read_chapters().contains(&3)),
        "到过第 3 章要记进「来过」集合，目录那一屏的浅色按它画"
    );
}

/// 点一条带锚点的小节（跳到别的章里的某一处）：落点不是那一章的章首，而是那个 `id`
/// 所在那一段的第一个字。锚点换算必须等排版做完才能做，所以这一条测的是「排完那一帧
/// 落对了地方」。
#[gpui_kit::test]
fn a_toc_jump_with_an_anchor_lands_on_that_paragraph(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &anchored_flow(4),
        "epub",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    scroll_mode(cx, handle);

    view.update(cx, |v, cx| v.goto_entry(2, Some("mid2".to_string()), cx));
    settle(cx, handle);

    assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 2);
    let expected = anchor_offset(&view, "mid2", cx);
    // 没有这一条，`assert_eq!(offset_of, expected)` 在「两边都是 0」上也成立 ——
    // 那正是锚点整个没生效的样子。锚点写在第 6 段，它绝不等于章首。
    assert!(
        expected > 0,
        "锚点在第 6 段上，它的章内偏移不该等于章首的 0：{expected}"
    );
    assert_eq!(
        offset_of(&view, cx),
        expected,
        "视口顶那一行该正好从锚点那一段开始"
    );
    assert!(
        view.read_with(cx, |v, _| v
            .line_texts()
            .iter()
            .any(|text| text.contains("锚点第2章"))),
        "落点所在的那一段要在这一章的排版里"
    );
    assert!(
        !view.read_with(cx, |v, _| v.has_pending_restore()),
        "带着锚点的请求同样要在落地那一帧被取走"
    );
}

/// 一个 spine 文件挂着好几条小节（一章对十条 nav）时，跳到**同一章**的另一条不算换章，
/// `Book::goto` 会说「没动」—— 可落点照样得挪。这一段分支单独一条：
/// 少了它，读者点同章的小节就是「按下去什么都不发生」。
#[gpui_kit::test]
fn an_anchor_in_the_chapter_you_are_in_still_moves_the_landing(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    // 起点就在第 1 章的章首。
    let (view, handle) = open_scroll(
        cx,
        &anchored_flow(4),
        "epub",
        prefs(16.0),
        Locator {
            chapter: 1,
            offset: 0,
        },
        460.0,
    );
    assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 1);
    assert_eq!(offset_of(&view, cx), 0, "起点是第 1 章的章首");

    view.update(cx, |v, cx| v.goto_entry(1, Some("mid1".to_string()), cx));
    settle(cx, handle);

    assert_eq!(
        view.read_with(cx, |v, _| v.chapter_index()),
        1,
        "还在同一章，没白跳"
    );
    assert_eq!(
        offset_of(&view, cx),
        anchor_offset(&view, "mid1", cx),
        "落点从章首挪到了那一条小节"
    );
}

/// 目录指向一个书里不存在的 `id`（作者写错、或锚点挂在一个没有正文的元素上）：
/// 章还是要照跳，落点留在章首 —— 宁可回到本章开头，也不要凭猜落在正文中间。
#[gpui_kit::test]
fn an_anchor_the_layout_cannot_find_falls_back_to_the_chapter_head(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &anchored_flow(4),
        "epub",
        prefs(16.0),
        Locator::start(),
        460.0,
    );

    view.update(cx, |v, cx| {
        v.goto_entry(2, Some("这本书里没有这个id".to_string()), cx)
    });
    settle(cx, handle);

    assert_eq!(
        view.read_with(cx, |v, _| v.chapter_index()),
        2,
        "锚点解不出来，章还是要跳"
    );
    assert_eq!(
        offset_of(&view, cx),
        0,
        "落点留在章首，不是某个猜出来的中间位置"
    );
    assert!(
        view.read_with(cx, |v, _| v
            .line_texts()
            .iter()
            .any(|text| text.contains("第2章 第0段"))),
        "屏上该从第 2 章第一段开始"
    );
}
