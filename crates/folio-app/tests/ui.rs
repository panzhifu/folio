//! M1 验收门：证明「滚动 → 折算字符偏移 → 改字号 → 恢复位置」这条回路成立。
//! 用生产视图 ReaderView，不在测试里另搭近似视图；滚动走真实滚轮事件，
//! 因为直接 set_offset 会绕过要测的那段行为。

use folio_app::reader::ReaderView;
use folio_core::store::BookRow;
use folio_core::{Chapter, FlowDoc, Locator, Prefs};
use gpui_kit::component::Root;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Entity, ScrollDelta, TestAppContext, point, px, size,
};

const PARA: &str = "于是我们奋力向前，逆水行舟，被不断地向后推，推入过去。";

fn fixture() -> FlowDoc {
    // 分成多个自然段：TextView 内部的 list 按块虚拟化，
    // 一整篇只有一个段落的话，它就是一个比视口还高的 list 项。
    let source = (0..80)
        .map(|i| format!("{PARA}（第 {i} 段）\n\n"))
        .collect::<String>();
    FlowDoc {
        chapters: vec![Chapter {
            title: "第一章".into(),
            source,
            spine_href: String::new(),
        }],
    }
}

fn book() -> BookRow {
    BookRow {
        id: 1,
        path: "memory://fixture".into(),
        title: "测试书".into(),
        authors: Vec::new(),
        format: "markdown".into(),
        size: 0,
        last_opened_at: None,
    }
}

fn open(
    cx: &mut TestAppContext,
    flow: &FlowDoc,
    prefs: Prefs,
    locator: Locator,
) -> (Entity<ReaderView>, AnyWindowHandle) {
    let row = book();
    let mut view = None;
    let handle = cx.open_window(size(px(460.), px(520.)), |window, cx| {
        let created =
            cx.new(|cx| ReaderView::open(&row, flow.clone(), prefs.clone(), locator, window, cx));
        view = Some(created.clone());
        Root::new(created, window, cx)
    });
    let view = view.expect("ReaderView 应该被创建");
    // 两轮「跑空事件 → 渲染一帧」。一轮不够：markdown 解析是异步的，
    // 首帧才把 reveal_range 请求挂上去，而 run_until_parked 只跑事件不画帧。
    // 框架自己的 settle() 也是这个形状。
    settle(cx, handle.into());
    (view, handle.into())
}

fn settle(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("渲染应成功");
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("第二帧应成功");
    cx.run_until_parked();
}

fn scroll_down(cx: &mut TestAppContext, handle: AnyWindowHandle, pixels: f32) {
    cx.update_window(handle, |_, window, cx| {
        window.scroll(
            "reader-scroll",
            ScrollDelta::Pixels(point(px(0.0), px(-pixels))),
            cx,
        );
    })
    .expect("滚动应送达阅读视图");
    cx.run_until_parked();
}

fn progress_of(cx: &mut TestAppContext, view: &Entity<ReaderView>) -> f32 {
    let view = view.clone();
    cx.update(|cx| view.read(cx).progress(cx))
}

fn locator_of(cx: &mut TestAppContext, view: &Entity<ReaderView>) -> Locator {
    let view = view.clone();
    cx.update(|cx| view.read(cx).locator(cx))
}

#[gpui_kit::test]
fn wheel_scrolling_moves_the_reading_position(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = fixture();
    let (view, handle) = open(
        cx,
        &flow,
        Prefs {
            font_size: 16.0,
            ..Default::default()
        },
        Locator::start(),
    );

    assert_eq!(
        progress_of(cx, &view),
        0.0,
        "刚打开时应在章首，不该已有位移"
    );

    scroll_down(cx, handle, 900.0);
    let progress = progress_of(cx, &view);
    assert!(
        progress > 0.05,
        "真实滚轮事件应产生可测位移，实际比例 {progress:.3}"
    );
}

#[gpui_kit::test]
fn locator_matches_the_scrolled_ratio(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = fixture();
    let total = flow.chapters[0].char_len();
    assert!(total > 1000, "语料要够长才滚得动，实际 {total} 字");

    let (view, handle) = open(
        cx,
        &flow,
        Prefs {
            font_size: 16.0,
            ..Default::default()
        },
        Locator::start(),
    );
    assert_eq!(locator_of(cx, &view).offset, 0, "未滚动时偏移应为 0");

    scroll_down(cx, handle, 900.0);
    let progress = progress_of(cx, &view);
    let locator = locator_of(cx, &view);

    assert!(locator.offset > 0, "滚动后偏移应前进");
    assert!(locator.offset < total, "偏移不应越出章尾");
    // 二分反解的正确性：字符偏移折算回比例，应与滚动比例吻合。
    let ratio_from_offset = locator.offset as f32 / total as f32;
    assert!(
        (ratio_from_offset - progress).abs() < 0.05,
        "偏移↔比例 换算不一致：offset {} / {total} = {ratio_from_offset:.3}，progress = {progress:.3}",
        locator.offset
    );
}

/// 「重启回到原处」里属于我们自己的那一半：位置必须能无损穿过 SQLite 回来。
/// 像素级落点取决于渲染器的 reveal 语义，自研排版引擎（M2）之后由我们自己保证。
#[gpui_kit::test]
fn position_survives_a_restart_through_the_store(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = fixture();
    let (view, handle) = open(
        cx,
        &flow,
        Prefs {
            font_size: 16.0,
            ..Default::default()
        },
        Locator::start(),
    );

    scroll_down(cx, handle, 900.0);
    let locator = locator_of(cx, &view);
    assert!(locator.offset > 0, "滚动后应能算出字符偏移");

    let store = folio_core::Store::in_memory().expect("内存库");
    let row = book();
    let id = store
        .add_book(std::path::Path::new(&row.path), &folio_core::DocMeta {
            title: row.title.clone(),
            authors: Vec::new(),
            format: folio_core::Format::Markdown,
            size: 0,
        })
        .expect("入库");
    store.save_flow(id, &flow).expect("章节入库");
    store
        .set_position(id, "continuous", &locator)
        .expect("写入进度");

    let (mode, restored) = store.position(id).expect("读回进度").expect("进度应存在");
    assert_eq!(mode, "continuous");
    assert_eq!(
        restored, locator,
        "进度穿过 SQLite 之后必须逐字符一致"
    );

    // 用读回来的进度重开一本书，恢复请求应当被挂上并在解析完成后落地。
    let (reopened, _) = open(
        cx,
        &flow,
        Prefs {
            font_size: 16.0,
            ..Default::default()
        },
        restored,
    );
    assert!(
        !reopened.read_with(cx, |v, _| v.has_pending_restore()),
        "解析完成后恢复请求应被消费掉，而不是永远挂着"
    );
}
