//! 批注：落库、回选、改色改样式不挪版。

use super::harness::{
    char_point, drag_select, drag_select_between, flow, meta, open_with_store, prefs, scroll_mode,
    settle,
};
use crate::ui::features::reader::model::session::ReaderSession;
use crate::ui::services::config::ConfigHolder;
use folio_core::store::Store;
use folio_core::{AnnotationStyle, Chapter, FlowDoc, Format, Locator};
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Entity, Focusable as _, TestAppContext, point, px,
};
use std::sync::Arc;

/// 一章只有一行字：那一行下面紧接着就是空白。
///
/// 「点在划线下方的空白里不该把那条挑中」这条判据只有在这样的形状上才测得到 ——
/// 多行章里每一行都被邻行夹着，`hit_test` 那个「夹到最近的行」永远给你一行**里**的字。
fn one_line_flow() -> FlowDoc {
    FlowDoc {
        chapters: vec![Chapter::inline("一行", "<p>于是我们奋力向前</p>")],
        ..Default::default()
    }
}

/// 把这份语料落进一个内存库并登记成一行书目，返回 `(库, book_id)`。
///
/// 划线的用例都得有**真的**库：写不进库是「划了个寂寞」，删不掉是「取消划线」那颗按钮
/// 是坏的，这两件事只有对着存储层才测得出来。
fn store_with_book(flow: &FlowDoc, format: &str) -> (Arc<Store>, i64) {
    let store = Arc::new(Store::in_memory().expect("内存库"));
    let id = store
        .add_book(std::path::Path::new("memory://fixture"), &meta(format))
        .expect("入库");
    store
        .save_flow(id, flow, Format::Markdown)
        .expect("章节入库");
    (store, id)
}

/// 拖选 → 菜单里那颗「划线」→ 点在画出来的那条划线上。把「已经重选好」这一状态共享给
/// 后面几条用例，返回那条划线的章内区间（含头不含尾）。
fn highlight_then_tap_it_back(
    cx: &mut TestAppContext,
    view: &Entity<ReaderSession>,
    handle: AnyWindowHandle,
) -> (usize, usize) {
    // 先把排版定下来：头一代行盒与最终那代对不上（第一代还没按真实版心宽重排过），
    // 在它上面选中的字，落库之后画出来的底色并不在原地。
    settle(cx, handle);
    drag_select(cx, view, handle);
    let (start, end) = view
        .read_with(cx, |v, _| {
            v.selection()
                .map(|selected| (selected.range.start, selected.range.end))
        })
        .expect("已选中");
    view.update(cx, |v, cx| v.add_highlight(cx));
    // 拖选松手那一下攒下的「吃掉下一个 click」要先放掉 —— 真实时序里它被正文那层吃掉了
    // （`tap_on_body` 第一条判据）。不吃掉，下面这一下会被它拦下来，测的就还是第一条。
    assert!(
        view.update(cx, |v, _| v.consume_tap_suppression()),
        "拖选松手该攒下一次豁免"
    );
    // 画一帧：底色上屏、`content_bounds` 就位 —— 命中问的就是画出来的那几块矩形。
    settle(cx, handle);
    let at = char_point(view, cx, 1, 2);
    let consumed = cx
        .update_window(handle, |_, window, cx| {
            view.update(cx, |v, cx| v.tap_on_body(window, cx, at))
        })
        .expect("窗口应可更新");
    assert!(
        consumed,
        "点在一条已有的划线上应被「选中」消费，不该再拿去翻页或开浮层"
    );
    (start, end)
}

/// M7 划线：选中 → 落一条批注 → 重开这本书还能解回同一区间。
///
/// 两头都要钉住：写不进库是「划了个寂寞」，读不回坐标是「重开就丢」。
/// 拿掉 `add_highlight` 里那次写库、或 `open_book` 里那句 `load_annotations`，这条就红。
#[gpui_kit::test]
fn a_highlight_lands_in_the_store_and_comes_back(cx: &mut TestAppContext) {
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
    drag_select(cx, &view, handle);
    let selected = view.read_with(cx, |v, _| v.selection()).expect("已选中");

    view.update(cx, |v, cx| v.add_highlight(cx));

    let rows = store.annotations(id).expect("读库");
    assert_eq!(rows.len(), 1, "划一下应落一条批注");
    let row = &rows[0];
    assert_eq!(row.chapter, selected.chapter as u32, "锚点属于选区那一章");
    assert_eq!(
        (row.anchor.start, row.anchor.end),
        (selected.range.start, selected.range.end),
        "锚点要就是划中的那一段"
    );
    assert!(
        !row.anchor.exact.is_empty(),
        "锚点要带原文指纹，重排后靠它找回"
    );
    assert_eq!(row.color, "amber", "还没得选颜色时落清单里第一种");
    assert!(
        view.read_with(cx, |v, _| v.selection().is_none()),
        "划完选区收掉：那条划线从此来自批注，不是来自「选中」"
    );

    // 重开这本书：库里那一条读回来，并解回本章坐标上的同一区间。
    let (again, again_handle) = open_with_store(
        cx,
        &flow,
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
        Some(store),
        Some(id),
    );
    settle(cx, again_handle);
    let spans = again.read_with(cx, |v, _| {
        v.annotation_spans.borrow().get(&0).map(|(_, spans)| {
            spans
                .iter()
                .map(|span| (span.start, span.end, span.is_stale()))
                .collect::<Vec<_>>()
        })
    });
    assert_eq!(
        spans,
        Some(vec![(selected.range.start, selected.range.end, false)]),
        "重开这一章，批注要回显在原处且不是 stale"
    );
}

/// 加一条划线**不许**动版面：页号、滚动、章高、行数、版心宽都该原地不动。
///
/// 用户看到过「点划线时整屏闪一下」。如果那条路真的触发了重排，这里至少有一个数会变；
/// 全相等就说明闪来自绘制/点击层（菜单 occlude 与那帧多画），不是排版被重跑。
#[gpui_kit::test]
fn adding_a_highlight_does_not_reflow_or_move_the_page(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (store, id) = store_with_book(&flow, "markdown");
    let (view, handle) = open_with_store(
        cx,
        &flow,
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
        Some(store),
        Some(id),
    );
    drag_select(cx, &view, handle);
    settle(cx, handle);
    let before = view.read_with(cx, |v, _| {
        let doc = v.book().expect("有书").document();
        (
            v.scroll_y,
            v.page(),
            v.chapter_index(),
            v.content_height,
            doc.height(),
            doc.lines().len(),
            v.laid_out.columns,
            v.laid_out.pages,
            v.laid_out.viewport,
        )
    });

    view.update(cx, |v, cx| v.add_highlight(cx));
    settle(cx, handle);

    let after = view.read_with(cx, |v, _| {
        let doc = v.book().expect("有书").document();
        (
            v.scroll_y,
            v.page(),
            v.chapter_index(),
            v.content_height,
            doc.height(),
            doc.lines().len(),
            v.laid_out.columns,
            v.laid_out.pages,
            v.laid_out.viewport,
        )
    });
    assert_eq!(before, after, "加划线前后版面与位置必须逐项相同");
}

/// M7 二期笔记：从既有一条划线上写笔记，存的是那一条本身；再点回去时笔记带得回来，
/// 改一次仍然只有一条 —— 不是「先划再改」的两条路。
///
/// 拿掉 `save_note` 里 `editor.annotation` 那一支，这条就会在第二次保存时多落一条；
/// 拿掉 `open_note_editor` 里回填 `note` 的那一段，输入框会是空的。
#[gpui_kit::test]
fn a_note_lands_on_the_highlight_and_edits_the_same_one(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (store, id) = store_with_book(&flow, "markdown");
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
    scroll_mode(cx, handle);
    let (start, end) = highlight_then_tap_it_back(cx, &view, handle);

    // 打开便签：菜单让位，输入框拿到焦点。
    let open = cx
        .update_window(handle, |_, window, cx| {
            view.update(cx, |v, cx| {
                v.open_note_editor(window, cx);
                v.note_editor_open()
            })
        })
        .expect("窗口应可更新");
    assert!(open, "点了笔记该把便签升起来");
    assert!(
        !view.read_with(cx, |v, _| v.selection_menu_open()),
        "便签压着时动作条不该同时上屏"
    );

    let input = view.read_with(cx, |v, _| {
        v.note_editor.as_ref().expect("便签该在").input.clone()
    });
    let focused = cx
        .update_window(handle, |_, window, cx| {
            window.focused(cx) == Some(input.read(cx).focus_handle(cx).clone())
        })
        .expect("窗口应可更新");
    assert!(focused, "便签升起后焦点该落在输入框里，软键盘才会出来");
    cx.update_window(handle, |_, window, cx| {
        input.update(cx, |input, cx| {
            input.set_value("这一段写得真好", window, cx)
        });
    })
    .expect("窗口应可更新");
    view.update(cx, |v, cx| v.save_note(cx));

    let rows = store.annotations(id).expect("读库");
    assert_eq!(rows.len(), 1, "写笔记应落在原来那条划线上");
    assert_eq!(rows[0].note, "这一段写得真好");
    assert_eq!(
        (rows[0].anchor.start, rows[0].anchor.end),
        (start, end),
        "笔记锚的就是划中的那一段"
    );
    assert!(
        view.read_with(cx, |v, _| v.selection().is_none()),
        "存完便签收掉，屏上只剩那条划线"
    );
    assert!(!view.read_with(cx, |v, _| v.note_editor_open()));

    // 再点回那条划线：便签里带出原来的笔记，改完仍然只有一条。
    let at = char_point(&view, cx, 1, 2);
    let consumed = cx
        .update_window(handle, |_, window, cx| {
            view.update(cx, |v, cx| v.tap_on_body(window, cx, at))
        })
        .expect("窗口应可更新");
    assert!(consumed, "点在划线上应重新选中那一条");
    assert!(
        view.read_with(cx, |v, _| v.selection_annotation().is_some()),
        "先要认出这一条在库里"
    );
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |v, cx| v.open_note_editor(window, cx));
    })
    .expect("窗口应可更新");
    let text = view.read_with(cx, |v, cx| {
        v.note_editor
            .as_ref()
            .expect("便签该在")
            .input
            .read(cx)
            .value()
            .to_string()
    });
    assert_eq!(text, "这一段写得真好", "已有笔记该回填进输入框");
    let input = view.read_with(cx, |v, _| {
        v.note_editor.as_ref().expect("便签该在").input.clone()
    });
    cx.update_window(handle, |_, window, cx| {
        input.update(cx, |input, cx| input.set_value("改过的笔记", window, cx));
    })
    .expect("窗口应可更新");
    view.update(cx, |v, cx| v.save_note(cx));

    let rows = store.annotations(id).expect("读库");
    assert_eq!(rows.len(), 1, "改笔记不该多落一条");
    assert_eq!(rows[0].note, "改过的笔记");
    assert_eq!(rows[0].color, "amber", "只改笔记不该动颜色");
}

/// M7 二期：留在书上的那片底色可以**再点回去**。点上去就是重新选中那一条，菜单跟着回来，
/// 并且认出「这一条已经在库里」—— 第二格是「取消划线」，不是再划一次。
///
/// 拿掉 `tap_on_body` 里「没选区时试着重选」那一条判据，这一击返回 `false`、也不留选区：
/// 读者划过的痕迹变成只读，正是这条要挡住的坏状态。
#[gpui_kit::test]
fn a_tap_on_a_highlight_selects_it_again(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (store, id) = store_with_book(&flow, "markdown");
    let (view, handle) = open_with_store(
        cx,
        &flow,
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
        Some(store),
        Some(id),
    );
    scroll_mode(cx, handle);
    let (start, end) = highlight_then_tap_it_back(cx, &view, handle);
    let (range, menu, marked) = view.read_with(cx, |v, _| {
        (
            v.selection()
                .map(|selected| (selected.range.start, selected.range.end)),
            v.selection_menu_open(),
            v.selection_annotation().map(|row| row.color),
        )
    });
    assert_eq!(range, Some((start, end)), "重选中的就是那一条原本那一段字");
    assert!(menu, "菜单条该跟着回来");
    assert_eq!(
        marked.as_deref(),
        Some("amber"),
        "选区正好等于库里那一条，菜单该换上「取消划线」那一格"
    );

    // 浮层开着时让位：那一击的语义是收顶栏与底栏，不是顺手把那条划线挑中
    // （`overlay_open` 由阅读页每帧报进来）。先清掉选区，走的才是「没有选区」那一路。
    view.update(cx, |v, _| v.clear_selection());
    view.update(cx, |v, cx| v.set_overlay_open(true, cx));
    let on_mark = char_point(&view, cx, 1, 2);
    let consumed = cx
        .update_window(handle, |_, window, cx| {
            view.update(cx, |v, cx| v.tap_on_body(window, cx, on_mark))
        })
        .expect("窗口应可更新");
    assert!(!consumed, "浮层开着的一击归收浮层，不该被重选中消费掉");
    view.update(cx, |v, cx| v.set_overlay_open(false, cx));

    // 没有选区时点在**没划过**的那一行：这一击不该被选中消费（仍归翻页与浮层）。
    let elsewhere = char_point(&view, cx, 9, 2);
    let consumed = cx
        .update_window(handle, |_, window, cx| {
            view.update(cx, |v, cx| v.tap_on_body(window, cx, elsewhere))
        })
        .expect("窗口应可更新");
    assert!(!consumed, "没划线的地方不该吃掉这一击");
    assert!(
        view.read_with(cx, |v, _| v.selection().is_none()),
        "那一击也不该留下选区"
    );
}

/// 点在划线**下面**的空白里：不算压住那一条。
///
/// `hit_test` 的纵向语义是「夹到最近的行」（那是拖选收边要的），只按「最近的字落在区间里」
/// 判的话，读者在末行之下一击就把某条批注挑中。拿掉 `annotation_at` 里那道行盒检查这条会红。
#[gpui_kit::test]
fn a_tap_below_the_marked_line_selects_nothing(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = one_line_flow();
    let (store, id) = store_with_book(&flow, "markdown");
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
    // 同一条理由：先让排版定下来，那一行的行盒才是后面要量它的那个。
    settle(cx, handle);
    let start = char_point(&view, cx, 0, 1);
    let end = char_point(&view, cx, 0, 3);
    drag_select_between(cx, handle, start, end);
    view.update(cx, |v, cx| v.add_highlight(cx));
    assert!(
        view.update(cx, |v, _| v.consume_tap_suppression()),
        "拖选松手该攒下一次豁免"
    );
    settle(cx, handle);
    assert_eq!(
        view.read_with(cx, |v, _| v.annotations.len()),
        1,
        "先真的落一条"
    );

    // 同一列、往下 6 像素：横向还在那几个字上，纵向已经出了那一行的行盒。
    let below = view.read_with(cx, |v, _| {
        let box_ = &v.lines()[0];
        point(
            px(v.prefs.margin_x + box_.x + box_.dominant_font_size() * 1.5),
            px(box_.y + box_.height + 6.0 - v.scroll_y + v.header_reserve),
        )
    });
    let consumed = cx
        .update_window(handle, |_, window, cx| {
            view.update(cx, |v, cx| v.tap_on_body(window, cx, below))
        })
        .expect("窗口应可更新");
    assert!(!consumed, "末行之下的空白不该把那条划线挑中");
    assert!(
        view.read_with(cx, |v, _| v.selection().is_none()),
        "更不该留下选区"
    );
}

/// 「取消划线」真的删得掉库里那一条，删完那条划线与选区一起收掉。
#[gpui_kit::test]
fn the_reselected_highlight_can_be_removed(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (store, id) = store_with_book(&flow, "markdown");
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
    scroll_mode(cx, handle);
    highlight_then_tap_it_back(cx, &view, handle);
    assert!(
        view.read_with(cx, |v, _| v.selection_annotation().is_some()),
        "菜单认得出这一条在库里，那颗按钮才有得按"
    );

    view.update(cx, |v, cx| v.delete_highlight(cx));

    assert!(
        store.annotations(id).expect("读库").is_empty(),
        "取消划线要真的删掉库里那一条"
    );
    assert!(
        view.read_with(cx, |v, _| v.selection().is_none()),
        "那片底色已经没了，选区也该收掉"
    );
}

/// 换颜色改的就是库里那一条自己：条目不多一条、锚点一个字没动、选区留着。
#[gpui_kit::test]
fn recoloring_a_highlight_moves_nothing(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (store, id) = store_with_book(&flow, "markdown");
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
    scroll_mode(cx, handle);
    let (start, end) = highlight_then_tap_it_back(cx, &view, handle);

    view.update(cx, |v, cx| v.set_highlight_color("sky", cx));

    let rows = store.annotations(id).expect("读库");
    assert_eq!(rows.len(), 1, "换颜色不该多落一条");
    assert_eq!(rows[0].color, "sky", "库里那条的颜色要跟着改");
    assert_eq!(
        rows[0].style,
        folio_core::AnnotationStyle::Highlight,
        "只换颜色不该把画法抹回默认档 —— `update_annotation` 是整条覆盖，漏带哪格哪格就没"
    );
    assert_eq!(
        (rows[0].anchor.start, rows[0].anchor.end),
        (start, end),
        "改的只是颜色，锚点一个字都没动"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.selection_annotation().map(|row| row.color))
            .as_deref(),
        Some("sky"),
        "菜单认得的还是那一条，读的也是新那一档"
    );
    assert!(
        view.read_with(cx, |v, _| v.selection().is_some()),
        "选区留着：读者往往还要再比一档"
    );
}

/// 颜色那一行的「直接点色划线」：落库那一条用的就是点中的那一档，不是默认色。
/// 拿掉 `add_highlight_with` 收的那个 `color` 参数（改回写死默认色）这条就红。
#[gpui_kit::test]
fn a_highlight_can_be_drawn_in_the_tapped_color(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (store, id) = store_with_book(&flow, "markdown");
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
    drag_select(cx, &view, handle);

    view.update(cx, |v, cx| v.add_highlight_with("lime", cx));

    let rows = store.annotations(id).expect("读库");
    assert_eq!(rows.len(), 1, "点色也该落一条");
    assert_eq!(rows[0].color, "lime", "落的是点中的那一档，不是默认那一档");
}

/// 左边那三格按下去，挑中的是库里**已有**那一条时：只改那一条的画法，颜色一个字都不动，
/// 而且**不碰配置里那一格** —— 回头改一条老批注的画法，不该顺手把「下一条怎么划」也改掉。
///
/// 把 `set_highlight_style` 里那句「笔记与颜色原样带过去」写死成默认色，这条红在颜色那一格；
/// 把「不碰配置」那条岔路去掉（两个岔都写偏好），这条红在最后那一格。
#[gpui_kit::test]
fn re_styling_an_existing_row_moves_only_the_style(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (store, id) = store_with_book(&flow, "markdown");
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
    scroll_mode(cx, handle);
    // 划一条、再点回那一条：手里挑中的正是库里那一行（菜单第二格已是「取消划线」）。
    let (start, end) = highlight_then_tap_it_back(cx, &view, handle);
    // 先把颜色换成**不是默认那档**的一格：留着默认黄的话，「换画法时把颜色写回默认」
    // 那种缺陷根本测不出来（改坏了也还是 amber）。
    view.update(cx, |v, cx| v.set_highlight_color("sky", cx));
    let before = store.annotations(id).expect("读库");
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].color, "sky", "这一条已经被换成非默认那一档");
    assert_eq!(
        before[0].style,
        AnnotationStyle::Highlight,
        "新划的一条用的就是配置里那一格"
    );

    view.update(cx, |v, cx| v.set_highlight_style(AnnotationStyle::Wavy, cx));

    let rows = store.annotations(id).expect("改过之后读库");
    assert_eq!(rows.len(), 1, "换画法不该多落一条");
    assert_eq!(rows[0].style, AnnotationStyle::Wavy, "改的那一格要落下去");
    assert_eq!(rows[0].color, "sky", "只换画法不该把颜色抹回默认档");
    assert_eq!(
        (rows[0].anchor.start, rows[0].anchor.end),
        (start, end),
        "改的只是画法，锚点一个字都没动"
    );
    assert_eq!(
        cx.update(|cx| ConfigHolder::appearance(cx).highlight_style),
        AnnotationStyle::Highlight,
        "改一条老账不该动「下一条怎么划」那一格"
    );
    assert!(
        view.read_with(cx, |v, _| v.selection().is_some()),
        "选区留着：与换色同一口径，读者往往还要再比一档"
    );
}

/// 左边那三格按下去，手里是**普通选区**时是两件事一起：用它把这一段划上，**并**把它记进
/// 配置 —— 于是下一段不必再点格子，按「划线」那一格就是同一档。
///
/// 这条同时钉住两个默认值的分工：库里那一列的默认（`line`，答的是 v21 之前那些条）与
/// 配置里那一格的默认（`highlight`）不是一件事，界面上划出来的每一条都**显式**带画法。
#[gpui_kit::test]
fn a_tapped_style_draws_the_selection_and_becomes_the_default(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (store, id) = store_with_book(&flow, "markdown");
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
    drag_select(cx, &view, handle);

    view.update(cx, |v, cx| v.pick_style(AnnotationStyle::Wavy, cx));

    let rows = store.annotations(id).expect("读库");
    assert_eq!(rows.len(), 1, "点画法那一格也该直接落一条，与点色同一口径");
    assert_eq!(rows[0].style, AnnotationStyle::Wavy);
    assert_eq!(rows[0].color, "amber", "没点色就用默认那档");
    assert_eq!(
        cx.update(|cx| ConfigHolder::appearance(cx).highlight_style),
        AnnotationStyle::Wavy,
        "选中的这一档要记进配置：跨书持久，重开一本书也还是它"
    );

    // 第二段不必再点格子：按「划线」那一格走的就是刚记下的那一档。
    drag_select(cx, &view, handle);
    view.update(cx, |v, cx| v.add_highlight(cx));
    let rows = store.annotations(id).expect("再读库");
    assert_eq!(rows.len(), 2, "两段各一条");
    assert_eq!(
        rows[1].style,
        AnnotationStyle::Wavy,
        "「划线」那一格读的也是配置里那一格，不是写死的默认档"
    );
}

/// 顶栏那颗书签开关：按一下钉住当前位置所在的行（带指纹，重排后找得回），再按一下拔掉，
/// 拔掉之后撤销还能把原行带回来。
///
/// 拿掉 `toggle_bookmark` 里「已钉先拔」那条岔路，这条会在第二次按下时多钉一枚；
/// 拿掉撤销槽的恢复，最后一段就空了。
#[gpui_kit::test]
fn a_bookmark_toggles_on_and_off_the_current_line(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (store, id) = store_with_book(&flow, "markdown");
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
    assert!(
        !view.read_with(cx, |v, _| v.position_bookmarked()),
        "还没钉过，开关该是灭的"
    );

    view.update(cx, |v, cx| v.toggle_bookmark(cx));

    let rows = store.bookmarks(id).expect("读库");
    assert_eq!(rows.len(), 1, "按一下应钉一枚");
    assert_eq!(rows[0].chapter, 0, "章以锚点里的为准");
    assert!(
        !rows[0].anchor.exact.is_empty(),
        "可重排书签锚的是那一行，要带指纹重排后才找得回"
    );
    assert!(
        view.read_with(cx, |v, _| v.position_bookmarked()),
        "钉上之后开关该是亮的"
    );

    // 再按一次 = 拔（走撤销那条路），拔完开关回到灭。
    view.update(cx, |v, cx| v.toggle_bookmark(cx));
    assert!(
        store.bookmarks(id).expect("读库").is_empty(),
        "再按一下要拔掉那一枚"
    );
    assert!(!view.read_with(cx, |v, _| v.position_bookmarked()));

    // 撤销：原行整份回来。
    view.update(cx, |v, cx| v.undo_delete(cx));
    let rows = store.bookmarks(id).expect("撤销后读库");
    assert_eq!(rows.len(), 1, "撤销把那一枚带回来");
    assert!(
        view.read_with(cx, |v, _| v.position_bookmarked()),
        "回到这一行，开关也该重新亮起来"
    );
}

/// 删除的撤销：列表那条 `remove_annotation`（与「取消划线」同一落点）删掉的行，
/// 撤销后要**整行原样**回来 —— id、笔记、时间戳都不变。
///
/// 拿掉 `restore_annotation` 里「按原 id 插回」那一格，撤销会变成「新划一条」，
/// 这条红在 id 上。
#[gpui_kit::test]
fn a_deleted_annotation_comes_back_with_its_row_intact(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (store, id) = store_with_book(&flow, "markdown");
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
    drag_select(cx, &view, handle);
    view.update(cx, |v, cx| v.add_highlight(cx));
    let before = store.annotations(id).expect("读库");
    assert_eq!(before.len(), 1);

    view.update(cx, |v, cx| {
        assert!(v.remove_annotation(before[0].id, cx), "删掉那条要成功")
    });
    assert!(store.annotations(id).expect("读库").is_empty());
    assert!(
        view.read_with(cx, |v, _| v.undo.is_some()),
        "删完该有一颗撤销胶囊挂着"
    );

    view.update(cx, |v, cx| v.undo_delete(cx));
    let after = store.annotations(id).expect("撤销后读库");
    assert_eq!(after.len(), 1, "撤销把那一条带回来");
    assert_eq!(
        after[0], before[0],
        "整行原样回来：id、锚点、颜色、画法、时间戳都不变"
    );
    assert!(view.read_with(cx, |v, _| v.undo.is_none()), "胶囊该收了");
}
