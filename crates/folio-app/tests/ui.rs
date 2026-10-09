//! M1 验收门：证明「滚动 → 折算字符偏移 → 改字号 → 恢复位置」这条回路成立。
//! 用生产视图 ReaderView，不在测试里另搭近似视图；滚动走真实滚轮事件，
//! 因为直接 set_offset 会绕过要测的那段行为。

use folio_app::ui::desktop::reader::ReaderView;
use folio_core::store::BookRow;
use folio_core::{Chapter, Config, FlowDoc, Language, Locator, Reader};
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
    prefs: Reader,
    locator: Locator,
) -> (Entity<ReaderView>, AnyWindowHandle) {
    open_as(cx, flow, "markdown", prefs, locator)
}

fn open_as(
    cx: &mut TestAppContext,
    flow: &FlowDoc,
    format: &str,
    prefs: Reader,
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

/// 设置要开成**一个独立窗口**，而且重复点只聚焦、不再叠第二个。
///
/// 这条验的是两次回归之后的形状：设置一度挂在 448px 的对话框里，扣掉它自己的页面导航
/// 只剩一百多 px，每个字段竖着堆成一列没法看的窄条；后来它是主窗口的一个页面，
/// 占掉了书名、进度、标签那条线，而且「返回阅读」成了必须多点一下的步骤。
#[gpui_kit::test]
fn the_settings_button_opens_one_settings_window(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let store = folio_core::Store::in_memory().expect("内存库");
    let handle: AnyWindowHandle = cx
        .open_window(size(px(900.), px(600.)), |window, cx| {
            let view = cx.new(|cx| {
                folio_app::ui::desktop::app::FolioApp::new(
                    store,
                    Config::default(),
                    Vec::new(),
                    window,
                    cx,
                )
            });
            Root::new(view, window, cx)
        })
        .into();

    let before =
        cx.update(|cx| folio_app::ui::shared::config_model::ConfigHolder::reader(cx).font_size);
    assert_eq!(before, 17.0, "默认字号应为 17px");

    // 第一下：多出一个窗口。
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("settings", cx);
        window.render_frame(cx);
    })
    .expect("主窗口应当还在");
    let windows = cx.update(|cx| cx.windows());
    assert_eq!(windows.len(), 2, "点工具条的设置按钮应当新开一个设置窗口");

    // 第二下：聚焦已有那个，不叠第二个。两个窗口各自记着读者刚改的字段是无从合并的。
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("settings", cx);
        window.render_frame(cx);
    })
    .expect("主窗口应当还在");
    assert_eq!(
        cx.update(|cx| cx.windows()).len(),
        2,
        "设置窗口已经开着时再点，应该聚焦它而不是再开一个"
    );

    // 与主窗口共用同一份偏好实体：改字号立刻读得回来。
    let settings = *windows
        .iter()
        .find(|window| **window != handle)
        .expect("第二个窗口就是设置窗口");
    cx.update(|cx| {
        folio_app::ui::shared::config_model::ConfigHolder::edit_reader(cx, |p| p.font_size = 26.0);
    });
    let after =
        cx.update(|cx| folio_app::ui::shared::config_model::ConfigHolder::reader(cx).font_size);
    assert_eq!(after, 26.0);
    // 设置窗口真能画出一帧：拿到空壳或组件建不起来都会在这里失败。
    cx.update_window(settings, |_, window, cx| window.render_frame(cx))
        .expect("设置窗口应当可渲染");
}

/// 越界的偏好值必须被夹住，否则排版会算出零高或无限宽。
#[gpui_kit::test]
fn out_of_range_preferences_are_clamped(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let store = folio_core::Store::in_memory().unwrap();
    let handle = cx.open_window(size(px(600.), px(400.)), |window, cx| {
        Root::new(
            cx.new(|cx| {
                folio_app::ui::desktop::app::FolioApp::new(
                    store,
                    Config::default(),
                    Vec::new(),
                    window,
                    cx,
                )
            }),
            window,
            cx,
        )
    });
    let handle: AnyWindowHandle = handle.into();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();

    cx.update(|cx| {
        folio_app::ui::shared::config_model::ConfigHolder::edit_reader(cx, |p| {
            p.font_size = 9000.0;
            p.line_height = 0.0;
        });
    });
    let prefs = cx.update(|cx| folio_app::ui::shared::config_model::ConfigHolder::reader(cx));
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
        Reader {
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
        Reader {
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
        Reader {
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
        Reader {
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

    let prefs = Reader {
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
        (title.dominant_font_size() - 36.0).abs() < 0.5,
        "h1 = 1.8em × 20px，实际 {}",
        title.dominant_font_size()
    );
    assert!(
        title.x > 20.0,
        "居中标题应离开版心左缘，实际 x = {}",
        title.x
    );

    // 第二段起是正文：书自己的 CSS 写了 p { text-indent: 2em }。
    let body = lines
        .iter()
        .find(|l| l.dominant_font_size() == 20.0 && l.x > 0.0)
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
        .split('<')
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

/// M3 的验收：一个段落里的局部字重、斜体与颜色必须各自成立。
/// 这一条测的是**真实字体栈**上的多 run 度量与摆放，mock 测不到。
#[gpui_kit::test]
fn inline_styles_survive_into_runs(cx: &mut TestAppContext) {
    use folio_core::css::computed::Color;

    cx.update(gpui_kit::init);
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/oss/minimal-zh.epub");
    let (_, content) = folio_core::open(&path).expect("合成 EPUB 应能打开");
    let folio_core::Content::Reflow(flow) = content;
    let (view, _handle) = open_as(cx, &flow, "epub", Reader::default(), Locator::start());
    let lines = cx
        .update(|cx| view.read(cx).engine_lines().map(|l| l.to_vec()))
        .expect("EPUB 走自研引擎");

    let rich = lines
        .iter()
        .find(|l| l.text_runs().count() >= 4)
        .expect("带 em/strong/span 的那一行应被切成多个 run");
    assert!(
        rich.text_runs().any(|r| r.style.font.weight >= 700),
        "strong 应带粗体"
    );
    assert!(rich.text_runs().any(|r| r.style.font.italic), "em 应带斜体");
    assert!(
        rich.text_runs()
            .any(|r| r.style.color == Some(Color(0xC0, 0x39, 0x2B))),
        "span.red 的局部颜色要落到 run 上"
    );
    assert!(
        rich.text_runs().all(|r| r.width > 0.0),
        "每个 run 都要有实际绘制宽度"
    );
    let sum: f32 = rich.text_runs().map(|r| r.width).sum();
    assert!(
        (sum - rich.width).abs() < 0.5,
        "行宽应是各 run 宽度之和：{sum} vs {}",
        rich.width
    );
}

/// 多语言的验收：改偏好之后，**真实渲染出来的**按钮文案要立刻换成另一种语言。
/// 只测 `tr()` 的返回值是不够的 —— 那测不到「视图有没有跟着重画」。
#[gpui_kit::test]
fn switching_the_interface_language_changes_the_rendered_labels(cx: &mut TestAppContext) {
    use folio_core::i18n::Locale;

    cx.update(gpui_kit::init);
    let store = folio_core::Store::in_memory().expect("内存库");
    let config = Config {
        language: Language {
            locale: Locale::ZhHans,
        },
        ..Default::default()
    };
    let handle: AnyWindowHandle = cx
        .open_window(size(px(900.), px(600.)), |window, cx| {
            let view = cx.new(|cx| {
                folio_app::ui::desktop::app::FolioApp::new(
                    store,
                    config.clone(),
                    Vec::new(),
                    window,
                    cx,
                )
            });
            Root::new(view, window, cx)
        })
        .into();

    // 断言走空库引导那个**带文字**的按钮：侧栏的导入入口是纯图标，没有 label 可查。
    let import_label = |cx: &mut TestAppContext| {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.find("empty-import").label().map(|s| s.to_string())
        })
        .expect("应能读到导入按钮的文案")
    };
    assert_eq!(
        import_label(cx).as_deref(),
        Some("导入书籍…"),
        "偏好是中文时界面应为中文"
    );

    cx.update(|cx| {
        folio_app::ui::shared::config_model::ConfigHolder::edit_language(cx, |l| {
            l.locale = Locale::En
        });
    });
    assert_eq!(
        import_label(cx).as_deref(),
        Some("Import book…"),
        "切换界面语言后，重画出来的文案要跟着变"
    );

    // 章节位置这类带参文案：走的是同一套占位符，语言变了顺序也要对。
    cx.update(|cx| {
        folio_app::ui::shared::config_model::ConfigHolder::edit_language(cx, |l| {
            l.locale = Locale::Ja
        });
    });
    let label = cx
        .update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.find("empty-import").label().map(|s| s.to_string())
        })
        .expect("应能读到导入按钮的文案");
    assert_eq!(label.as_deref(), Some("本を追加…"), "日文文案也要生效");
}

/// 一张 1×1 的合法 PNG，用来喂给引擎（解码由 gpui 负责，这里只要能过 header）。
const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

/// M3-E3 验收（app 侧）：EPUB 章里的内嵌图要经真实引擎变成图片片段，
/// `src` 按本章目录解析到容器路径，且可见文本里不留哨兵字符。
#[gpui_kit::test]
fn images_flow_through_the_engine_as_fragments(cx: &mut TestAppContext) {
    use folio_core::format::ImageRes;

    cx.update(gpui_kit::init);
    let flow = FlowDoc {
        chapters: vec![Chapter {
            title: "图".into(),
            source: "<p>前<img src=\"../Images/pic.png\" alt=\"图\">后</p>".into(),
            spine_href: "OEBPS/Text/chap.xhtml".into(),
        }],
        images: vec![ImageRes {
            href: "OEBPS/Images/pic.png".into(),
            media_type: "image/png".into(),
            width: 40,
            height: 20,
            bytes: PNG_1X1.to_vec(),
        }],
        ..Default::default()
    };
    let (view, _handle) = open_as(cx, &flow, "epub", Reader::default(), Locator::start());
    let lines = cx
        .update(|cx| view.read(cx).engine_lines().map(|l| l.to_vec()))
        .expect("EPUB 走自研引擎");
    let texts = cx
        .update(|cx| view.read(cx).engine_line_texts())
        .expect("EPUB 走自研引擎");

    assert_eq!(texts[0], "前后", "行内图不产出字符，哨兵要被去掉");
    let img = lines[0]
        .images()
        .next()
        .expect("../Images/pic.png 应解析成 OEBPS/Images/pic.png 并命中");
    assert_eq!(img.href, "OEBPS/Images/pic.png");
    assert_eq!((img.width, img.height), (40.0, 20.0));
}

fn open_app(cx: &mut TestAppContext) -> AnyWindowHandle {
    cx.update(gpui_kit::init);
    cx.open_window(size(px(900.), px(600.)), |window, cx| {
        let view = cx.new(|cx| {
            folio_app::ui::desktop::app::FolioApp::new(
                folio_core::Store::in_memory().expect("内存库"),
                Config::default(),
                Vec::new(),
                window,
                cx,
            )
        });
        Root::new(view, window, cx)
    })
    .into()
}

/// 回归门：自定义标题栏必须挂在视图树里（曾经的 bug 是窗口选项声明了透明标题栏、
/// 却没有任何人绘制它，于是顶上什么都没有）。
#[gpui_kit::test]
fn the_app_renders_a_custom_title_bar(cx: &mut TestAppContext) {
    let handle = open_app(cx);
    let present = cx
        .update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("app-title").is_some()
        })
        .expect("窗口应能渲染");
    assert!(present, "标题栏里的应用标题应出现在视图树中");
}

/// 账号入口必须始终完整可见：折叠后侧栏只有 48px，任何一层多余的
/// 内边距都会把 26px 的头像挤到被侧栏裁掉（组件的 `SidebarHeader` /
/// `SidebarFooter` 各自还带一层 `p_2`，套在 `#header` / `#footer` 的
/// `px_2` 里面就只剩 16px 可用宽度）。
#[gpui_kit::test]
fn the_account_row_stays_fully_visible_when_collapsed(cx: &mut TestAppContext) {
    let handle = open_app(cx);
    let measure = |cx: &mut TestAppContext| {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let account = window.find("account");
            let toggle = window.find("collapse");
            (
                account.bounds().size.width.as_f32(),
                account.visible(),
                toggle.bounds().size.width.as_f32(),
            )
        })
        .expect("应能读到折叠前后的几何")
    };

    let _ = measure(cx);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("collapse", cx);
        window.render_frame(cx);
    })
    .expect("点击折叠按钮");

    let (account_w, account_visible, toggle_w) = measure(cx);
    assert!(
        account_w >= 26.0 && toggle_w >= 24.0,
        "折叠态账号行应容得下 26px 头像、标题栏应容得下折叠按钮，实际 账号行 {account_w} / 折叠按钮 {toggle_w}（可见 {account_visible}）"
    );
}

/// 侧栏底部的账号入口：点它应打开设置窗口。
#[gpui_kit::test]
fn the_account_entry_opens_settings(cx: &mut TestAppContext) {
    let handle = open_app(cx);
    let before = cx.update(|cx| cx.windows()).len();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("account", cx);
        window.render_frame(cx);
    })
    .expect("账号入口应能打开设置");
    assert_eq!(
        cx.update(|cx| cx.windows()).len(),
        before + 1,
        "点侧栏账号入口应新开一个设置窗口"
    );
}

/// 账号名走的是配置实体：改完立刻能读回来（落盘由 `ConfigPath` 决定，测试里不落盘）。
#[gpui_kit::test]
fn editing_the_account_name_updates_the_holder(cx: &mut TestAppContext) {
    let handle = open_app(cx);
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    cx.update(|cx| {
        folio_app::ui::shared::config_model::ConfigHolder::edit_account(cx, |a| {
            a.name = "阿遥".into()
        });
    });
    let name = cx.update(|cx| folio_app::ui::shared::config_model::ConfigHolder::account(cx).name);
    assert_eq!(name, "阿遥");
}

fn epub_flow() -> FlowDoc {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/oss/minimal-zh.epub");
    let (_, content) = folio_core::open(&path).expect("合成 EPUB 应能打开");
    let folio_core::Content::Reflow(flow) = content;
    flow
}

#[gpui_kit::test]
fn the_engine_progress_is_a_character_offset_that_comes_back(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = epub_flow();
    // 字号故意给到 60：这本合成书的第一章很短，默认字号下整章只有几行、比窗口还短，
    // 列表没有可滚的范围（`max_offset` 实测为 0），那样的话这条测试测的是空集。
    let prefs = Reader {
        font_size: 60.0,
        ..Default::default()
    };
    let (view, handle) = open_as(cx, &flow, "epub", prefs.clone(), Locator::start());
    let lines = view.read_with(cx, |v, _| v.engine_lines().map(|l| l.len()).unwrap_or(0));
    assert!(lines > 4, "大字号下第一章该排出多行，实际 {lines}");
    // 跳转目标取「本章中点」。写死一个坐标的话，CI 上字体不同 → 行高不同 →
    // 可能落在第一行里，那条测试就变成测空集了。
    let total = view.read_with(cx, |v, _| v.text_length());
    assert!(total > 40, "第一章该有几十个字可跳，实际 {total}");
    let target = total / 2;
    // 章首那一行的坐标不是 0：pretty-print 过的 XHTML 里 `<body>` 与 `<p>` 之间的
    // 换行本身是文本节点，也进坐标系。这里钉的是「刚打开时还在最前面」，
    // 而不是「等于 0」—— 见 `anchor` 模块头对坐标系的说明。
    let start = locator_of(cx, &view).offset;
    assert!(start < 8, "刚打开该在章首附近，实际坐标 {start}");

    // 引擎那张列表的 `ElementId` 不在测试支持的注册路径里（它注册在
    // `engine-scroll` 之下），所以滚动这件事走产品本来就要有的那个入口：
    // 「跳到章内第 N 个字」。目录跳章、书签、恢复进度后面用的都是它。
    view.update(cx, |v, _| v.jump_to_offset(target));
    settle(cx, handle);

    let saved = locator_of(cx, &view);
    assert!(
        saved.offset > start,
        "跳到中点坐标 {target} 之后章内偏移该往前走，实际 {start} → {}",
        saved.offset
    );
    assert!(
        saved.offset <= target,
        "落点该是覆盖 {target} 那一行的行首，不该越过它：{}",
        saved.offset
    );
    // 也不能越过一整行太多：下一行的行首都比目标早，说明跳过头了。
    assert!(
        target - saved.offset < target / 4,
        "落点离目标 {target} 太远：{}",
        saved.offset
    );
    let progress = progress_of(cx, &view);
    assert!(
        progress > 0.0 && progress < 1.0,
        "滚动比例该在 (0,1) 之间，实际 {progress}"
    );

    // 用同一个坐标重开：落回的行反解出来的偏移必须还是它。
    let (reopened, _handle) = open_as(cx, &flow, "epub", prefs, saved);
    assert!(
        !reopened.read_with(cx, |v, _| v.has_pending_restore()),
        "恢复请求该在排版做完的那一帧被消费掉"
    );
    assert_eq!(
        locator_of(cx, &reopened).offset,
        saved.offset,
        "坐标 → 行顶 → 再反解回坐标，必须回到同一个字"
    );
    assert!(
        progress_of(cx, &reopened) > 0.0,
        "重开之后列表真的滚出去了，不是只记了个数"
    );
}
