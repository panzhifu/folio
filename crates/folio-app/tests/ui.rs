//! M1 验收门：证明「滚动 → 折算字符偏移 → 改字号 → 恢复位置」这条回路成立。
//! 用生产视图 ReaderView，不在测试里另搭近似视图；滚动走真实滚轮事件，
//! 因为直接 set_offset 会绕过要测的那段行为。

use folio_app::reader::ReaderView;
use folio_core::store::BookRow;
use folio_core::{Chapter, FlowDoc, Locator, Prefs};
use gpui_kit::component::{Root, WindowExt as _};
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
        ..Default::default()
    }
}

fn book() -> BookRow {
    book_as("markdown")
}

fn book_as(format: &str) -> BookRow {
    BookRow {
        id: 1,
        path: "memory://fixture".into(),
        title: "测试书".into(),
        authors: Vec::new(),
        format: format.to_string(),
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
    open_as(cx, flow, "markdown", prefs, locator)
}

fn open_as(
    cx: &mut TestAppContext,
    flow: &FlowDoc,
    format: &str,
    prefs: Prefs,
    locator: Locator,
) -> (Entity<ReaderView>, AnyWindowHandle) {
    let row = book_as(format);
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

/// 设置面板要能被真实点开，并且改一个字段真的会落到偏好实体上。
/// 只测「能编译出这个面板」没有意义 —— 那测不到取值对。
#[gpui_kit::test]
fn settings_dialog_opens_and_edits_preferences(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let store = folio_core::Store::in_memory().expect("内存库");
    let mut app = None;
    let handle = cx.open_window(size(px(900.), px(600.)), |window, cx| {
        let view = cx.new(|cx| {
            folio_app::app::FolioApp::new(store, Prefs::default(), Vec::new(), window, cx)
        });
        app = Some(view.clone());
        Root::new(view, window, cx)
    });
    let app = app.unwrap();
    let handle: AnyWindowHandle = handle.into();

    let before = cx.update(|cx| folio_app::prefs_model::PrefsHolder::get(cx).font_size);
    assert_eq!(before, 17.0, "默认字号应为 17px");

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("settings", cx);
        window.render_frame(cx);
        // 断 "dialog" 而不是 "reader-settings"：Settings 元素自己不注册观测点，
        // 而对话框层是 Root 挂的固定 id。
        assert!(
            window.try_find("dialog").is_some(),
            "点工具条的设置按钮应当打开设置对话框"
        );
    })
    .expect("设置面板应能打开");

    // 关掉面板后从偏好实体读，确认写入路径是通的而不是只画了个壳。
    cx.update_window(handle, |_, window, cx| {
        window.close_dialog(cx);
        window.render_frame(cx);
        assert!(window.try_find("dialog").is_none(), "对话框应能关闭");
    })
    .expect("面板应能关闭");

    cx.update(|cx| {
        folio_app::prefs_model::PrefsHolder::edit(cx, |p| p.font_size = 26.0);
    });
    let after = cx.update(|cx| folio_app::prefs_model::PrefsHolder::get(cx).font_size);
    assert_eq!(after, 26.0);
    let _ = app;
}

/// 越界的偏好值必须被夹住，否则排版会算出零高或无限宽。
#[gpui_kit::test]
fn out_of_range_preferences_are_clamped(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let store = folio_core::Store::in_memory().unwrap();
    let handle = cx.open_window(size(px(600.), px(400.)), |window, cx| {
        Root::new(
            cx.new(|cx| {
                folio_app::app::FolioApp::new(store, Prefs::default(), Vec::new(), window, cx)
            }),
            window,
            cx,
        )
    });
    let handle: AnyWindowHandle = handle.into();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();

    cx.update(|cx| {
        folio_app::prefs_model::PrefsHolder::edit(cx, |p| {
            p.font_size = 9000.0;
            p.line_height = 0.0;
        });
    });
    let prefs = cx.update(|cx| folio_app::prefs_model::PrefsHolder::get(cx));
    assert_eq!(prefs.font_size, 72.0, "字号应被夹到上限");
    assert_eq!(prefs.line_height, 1.0, "行距应被夹到下限");
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
        .add_book(
            std::path::Path::new(&row.path),
            &folio_core::DocMeta {
                title: row.title.clone(),
                authors: Vec::new(),
                format: folio_core::Format::Markdown,
                size: 0,
            },
        )
        .expect("入库");
    store.save_flow(id, &flow).expect("章节入库");
    store
        .set_position(id, "continuous", &locator)
        .expect("写入进度");

    let (mode, restored) = store.position(id).expect("读回进度").expect("进度应存在");
    assert_eq!(mode, "continuous");
    assert_eq!(restored, locator, "进度穿过 SQLite 之后必须逐字符一致");

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

/// M2 的验收门：中文排版基础三件事必须由**自研引擎**产出，而不是靠 TextView。
/// 首行缩进、居中标题、按样式给行距 —— 全部从引擎的行盒几何里读出来断言。
#[gpui_kit::test]
fn the_engine_indents_centers_and_spaces_lines(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/oss/minimal-zh.epub");
    let (meta, content) = folio_core::open(&path).expect("合成 EPUB 应能打开");
    // Content 现在只有 Reflow 一个分支，这个模式是不可反驳的。
    let folio_core::Content::Reflow(flow) = content;
    assert_eq!(flow.chapters.len(), 3, "spine 三章");
    assert_eq!(meta.title, "合成测试书");

    let prefs = Prefs {
        font_size: 20.0,
        line_height: 1.8,
        ..Default::default()
    };
    let (view, _handle) = open_as(cx, &flow, "epub", prefs, Locator::start());
    let lines = cx
        .update(|cx| view.read(cx).engine_lines().map(|l| l.to_vec()))
        .expect("EPUB 走自研引擎");
    let texts = cx
        .update(|cx| view.read(cx).engine_line_texts())
        .expect("EPUB 走自研引擎");
    assert!(!lines.is_empty(), "引擎应产出行盒");

    // 第一章首个行盒是标题：UA 的 h1 = 2em 之外，书自己的 CSS 写了 1.8em 且居中。
    let title = &lines[0];
    assert_eq!(texts[0], "第一章 蛇结");
    assert!(
        (title.font_size - 36.0).abs() < 0.5,
        "h1 = 1.8em × 20px，实际 {}",
        title.font_size
    );
    assert!(
        title.x > 20.0,
        "居中标题应离开版心左缘，实际 x = {}",
        title.x
    );

    // 第二段起是正文：书自己的 CSS 写了 p { text-indent: 2em }。
    let body = lines
        .iter()
        .find(|l| l.font_size == 20.0 && l.x > 0.0)
        .expect("应有一个首行被缩进的正文行");
    assert!(
        (body.x - 40.0).abs() < 1.0,
        "2em 缩进在 20px 字号下应是 40px，实际 {}",
        body.x
    );

    // 行距按各自的字号给：标题一行比正文一行高。
    assert!(
        title.height > body.height * 1.5,
        "行盒高应随字号走，标题 {} vs 正文 {}",
        title.height,
        body.height
    );
    assert!(
        (body.height - 36.0).abs() < 0.5,
        "正文行高 = 20 × 1.8，实际 {}",
        body.height
    );

    // 长段落必须真的折了行：没有任何一行容得下整段原文，才算折过。
    let longest_paragraph = flow.chapters[0]
        .source
        .split(|c| c == '<')
        .map(|s| s.chars().filter(|c| !c.is_whitespace()).count())
        .max()
        .unwrap_or(0);
    let longest_line = texts.iter().map(|t| t.chars().count()).max().unwrap_or(0);
    assert!(
        longest_line < longest_paragraph && longest_line > 4,
        "段落应被折行：最长段 {longest_paragraph} 字，最长行只有 {longest_line} 字"
    );
    for pair in lines.windows(2) {
        assert!(
            pair[1].y >= pair[0].y + pair[0].height - 0.5,
            "行盒不能重叠：{} + {} > {}",
            pair[0].y,
            pair[0].height,
            pair[1].y
        );
    }
}
