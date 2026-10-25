//! 夹具与事件助手：开书、落事件、读数。整包测试共享，改一处全体生效。
//!
//! 原件是 `view/body/tests.rs` 拆分前的头部说明：用生产类型 `ReaderSession`，
//! 不在测试里另搭近似视图；滚动走真实滚轮事件，因为直接写 `scroll_y` 会绕过
//! 要测的那段行为。

use crate::ui::features::reader::model::session::ReaderSession;
use crate::ui::services::config::ConfigHolder;
use folio_core::content::document::Document;
use folio_core::store::Store;
use folio_core::{
    Appearance, Chapter, Config, Content, DocMeta, FlowDoc, Format, Locator, PageTurn, Reader,
};
use gpui_kit::component::Root;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Entity, InputEvent as _, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, PlatformInput, Point, ScrollDelta, TestAppContext,
    TouchPhase, point, px, size,
};
use std::sync::Arc;
use std::time::Duration;

/// 一段足够长的中文，保证它会折行 —— 不折行的话「缩进改变了布局」这种断言无从成立。
pub(super) const PARA: &str = "于是我们奋力向前，逆水行舟，被不断地向后推，推入过去。";

pub(super) fn flow() -> FlowDoc {
    // `Chapter::source` 的契约是**引擎就绪的 XHTML**（纯文本的包装在 provider 里做完）。
    // 这里直接进排版层，所以自己按契约包好 —— 塞纯文本进来是测试自己违约，不是引擎该兜的。
    let source = (0..80)
        .map(|i| format!("<p>{PARA}（第 {i} 段）</p>"))
        .collect::<String>();
    FlowDoc {
        chapters: vec![Chapter::inline("第一章", source)],
        ..Default::default()
    }
}

/// 多章语料：每章 30 段（16px 下一章约三个视口高，章内可滚区间远大于
/// 测试的滚动步长，跨章之后认得出读到的是哪一章、进度也还在章首附近）。
pub(super) fn chapters_flow(count: usize) -> FlowDoc {
    FlowDoc {
        chapters: (0..count)
            .map(|i| {
                let source = (0..30)
                    .map(|k| format!("<p>{PARA}（第{i}章 第{k}段）</p>"))
                    .collect::<String>();
                Chapter::inline(format!("第{i}章"), source)
            })
            .collect(),
        ..Default::default()
    }
}

pub(super) fn meta(format: &str) -> DocMeta {
    DocMeta {
        title: "测试书".into(),
        authors: Vec::new(),
        format: Format::parse(format).expect("格式名要认得"),
        size: 0,
        ..DocMeta::default()
    }
}

/// 这一组用例的偏好。
///
/// **段间距钉在 1em**：那些断言量的是「像素进度 ↔ 字偏移」两套坐标对得上，而它们的
/// 容差是按章高标定的 —— 段距一改，章高跟着改，同样的滚动量占总高的比例就变了，
/// 容差会莫名其妙地翻红。段距本身由 `document/tests.rs` 的
/// `a_layout_preference_change_moves_the_geometry` 管，不在这里测。
pub(super) fn prefs(size: f32) -> Reader {
    Reader {
        font_size: size,
        paragraph_gap_em: 1.0,
        ..Reader::default()
    }
}

/// 开一扇窗、装好配置、把会话摆上去。`width` 决定这一帧能不能长出双页 ——
/// 退档判据按宽度而不是按 target，所以窗口尺寸就是测试里唯一要控制的变量。
pub(super) fn open(
    cx: &mut TestAppContext,
    flow: &FlowDoc,
    format: &str,
    prefs: Reader,
    locator: Locator,
    width: f32,
) -> (Entity<ReaderSession>, AnyWindowHandle) {
    open_with_store(cx, flow, format, prefs, locator, width, None, None)
}

/// [`open`] 的上下滚动版：摆法在建会话之前就位。理由同 [`open_scroll_with_store`]。
pub(super) fn open_scroll(
    cx: &mut TestAppContext,
    flow: &FlowDoc,
    format: &str,
    prefs: Reader,
    locator: Locator,
    width: f32,
) -> (Entity<ReaderSession>, AnyWindowHandle) {
    build_with_store(
        cx,
        flow,
        format,
        prefs,
        locator,
        width,
        None,
        None,
        PageTurn::Scroll,
    )
}

/// 把这一屏的「翻页方式」改成「上下滚动」，也就是摆法进连续。
///
/// 这一格存在是因为摆法**不再能直接 `set_mode(Continuous)` 拿到**：那一档现在只有
/// 「翻页方式」这一条列表进得去（`chrome::reconcile` 每帧按 `page_turn` 解一次摆法，
/// 选了纸面动画就把这本书带出连续）。测试若绕过那个口去摆 `Continuous`，测的就是一个
/// 界面上到不了的状态 —— 那种门绿了也不代表读者能走到。
pub(super) fn scroll_mode(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.update(|app| {
        ConfigHolder::edit_appearance(app, |a| a.page_turn = PageTurn::Scroll);
    });
    settle(cx, handle);
}

/// 与 [`open_with_store`] 同一条路，但「翻页方式」在**建会话之前**就摆成上下滚动。
///
/// 不能像别的档那样「装好之后再改 ConfigHolder」：进度恢复吃的是第一帧那一刻的摆法。
/// 先以平移建好、再改成上下滚动，中间那次 settle 已经用分页把位置落了一遍，之后切连续
/// 只是把视口挪回锚章章首 —— 那样测出来的是「换档丢位置」，不是它想测的那件事。
#[allow(clippy::too_many_arguments)]
pub(super) fn open_scroll_with_store(
    cx: &mut TestAppContext,
    flow: &FlowDoc,
    format: &str,
    prefs: Reader,
    locator: Locator,
    width: f32,
    store: Option<Arc<Store>>,
    book_id: Option<i64>,
) -> (Entity<ReaderSession>, AnyWindowHandle) {
    build_with_store(
        cx,
        flow,
        format,
        prefs,
        locator,
        width,
        store,
        book_id,
        PageTurn::Scroll,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn open_with_store(
    cx: &mut TestAppContext,
    flow: &FlowDoc,
    format: &str,
    prefs: Reader,
    locator: Locator,
    width: f32,
    store: Option<Arc<Store>>,
    book_id: Option<i64>,
) -> (Entity<ReaderSession>, AnyWindowHandle) {
    build_with_store(
        cx,
        flow,
        format,
        prefs,
        locator,
        width,
        store,
        book_id,
        PageTurn::Slide,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_with_store(
    cx: &mut TestAppContext,
    flow: &FlowDoc,
    format: &str,
    prefs: Reader,
    locator: Locator,
    width: f32,
    store: Option<Arc<Store>>,
    book_id: Option<i64>,
    turn: PageTurn,
) -> (Entity<ReaderSession>, AnyWindowHandle) {
    let meta = meta(format);
    // 不开文件地造一本书：`from_content` 把章节收进统一文档接口，没有容器
    // （图片与内嵌字体都不来，与「容器丢了」同一个表现）。
    let doc: Arc<dyn Document> = Arc::from(folio_core::content::document::from_content(
        meta,
        Content::Reflow(flow.clone()),
    ));
    build_with_document(cx, doc, prefs, locator, width, store, book_id, turn)
}

/// 直接拿一份已经造好的文档开一屏。给「容器在、正文不在手上」那类测试用 ——
/// 那是正文出库之后生产上真正走的那条路。
#[allow(clippy::too_many_arguments)]
pub(super) fn open_with_document(
    cx: &mut TestAppContext,
    doc: Arc<dyn Document>,
    prefs: Reader,
    locator: Locator,
    width: f32,
    store: Option<Arc<Store>>,
    book_id: Option<i64>,
) -> (Entity<ReaderSession>, AnyWindowHandle) {
    build_with_document(
        cx,
        doc,
        prefs,
        locator,
        width,
        store,
        book_id,
        PageTurn::Slide,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_with_document(
    cx: &mut TestAppContext,
    doc: Arc<dyn Document>,
    prefs: Reader,
    locator: Locator,
    width: f32,
    store: Option<Arc<Store>>,
    book_id: Option<i64>,
    turn: PageTurn,
) -> (Entity<ReaderSession>, AnyWindowHandle) {
    let mut view = None;
    let handle = cx.open_window(size(px(width), px(520.)), |window, cx| {
        // 偏好必须**装进配置**而不是只交给构造函数：会话每帧从 `ConfigHolder`
        // 同步一次偏好（读者在设置页改字号就是这条路），装默认配置就是把这里传的
        // `prefs` 当场冲掉 —— 而画面照样出得来，只有几何不对，最难查的那种红。
        let config = Config {
            reader: prefs.clone(),
            // 测试台默认给一档**有动画**的翻页。软件初始默认是「不走动画」，而这一批门
            // 测的就是那两张纸怎么走 —— 没有动画，`page_shift` 恒为零，那些断言就全成了
            // 空话。要测别的档自己在装好之后改 `ConfigHolder`（竖轴、覆盖、折页那几条就是
            // 这么做的）；只有上下滚动例外，它得在建会话之前摆好，见 [`open_scroll_with_store`]。
            appearance: Appearance {
                page_turn: turn,
                ..Default::default()
            },
            ..Default::default()
        };
        ConfigHolder::install(config, cx);
        let built = ReaderSession::with_document(doc, prefs, store, book_id, locator, cx);
        let created = cx.new(|_| built);
        view = Some(created.clone());
        Root::new(created, window, cx)
    });
    let view = view.expect("ReaderSession 应该被创建");
    settle(cx, handle.into());
    (view, handle.into())
}

/// 两轮「跑空事件 → 渲染一帧」。一轮不够：首帧只是把恢复请求挂上，
/// 而 `run_until_parked` 只跑事件不画帧。框架自己的 `settle()` 也是这个形状。
pub(super) fn settle(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("渲染应成功");
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("第二帧应成功");
    cx.run_until_parked();
}

/// 真实滚轮事件投进正文。gpui 的约定是「向下滚为负」，所以往下滚给负的 dy。
pub(super) fn scroll_down(cx: &mut TestAppContext, handle: AnyWindowHandle, pixels: f32) {
    cx.update_window(handle, |_, window, cx| {
        window.scroll(
            "folio-reader",
            ScrollDelta::Pixels(point(px(0.0), px(-pixels))),
            cx,
        );
    })
    .expect("滚动应送达阅读视图");
    cx.run_until_parked();
}

/// 鼠标滚轮那一路：增量按「行」给，与触摸拖动的像素增量不是一回事。
pub(super) fn wheel_lines(cx: &mut TestAppContext, handle: AnyWindowHandle, lines: f32) {
    cx.update_window(handle, |_, window, cx| {
        window.scroll("folio-reader", ScrollDelta::Lines(point(0.0, lines)), cx);
    })
    .expect("滚轮应送达阅读视图");
    cx.run_until_parked();
}

/// 会话现在念的是**全书**比例，算不出来（有章的字数还没算过）就是夹具的问题。
/// 这里当场说清楚而不是回 0.0 —— 回 0 会让 `progress_of(..) > 0.0` 那几条断言
/// 静默读到零、以「没进度」的样子假绿。
pub(super) fn progress_of(view: &Entity<ReaderSession>, cx: &mut TestAppContext) -> f32 {
    view.read_with(cx, |v, _| {
        v.progress()
            .expect("这份夹具每章的字数都该算得出，否则进度那一格是空的")
    })
}

pub(super) fn offset_of(view: &Entity<ReaderSession>, cx: &mut TestAppContext) -> usize {
    view.read_with(cx, |v, _| v.current_offset().unwrap_or_default())
}

/// 合成书：仓库里那本三章的 `minimal-zh.epub`，**正文与样式表已经取回来**的那种。
///
/// `document::open` 现在只给索引（库里就存这个，见 `plans/text-out-of-db.md`），
/// 所以这里按位置把每一章的正文与样式文本补上 —— 这几条测的是排版，手上得有字
/// 也得有样式。
pub(super) fn epub_flow() -> FlowDoc {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/oss/minimal-zh.epub");
    let doc = crate::ui::features::reader::model::chapter_layout::open(&path)
        .expect("合成 EPUB 应能打开");
    let mut flow = doc.flow().expect("合成 EPUB 应读成可重排").clone();
    for (ix, chapter) in flow.chapters.iter_mut().enumerate() {
        let text = doc.chapter_source(ix).expect("正文该能从容器取到");
        *chapter = Chapter::inline(chapter.title.clone(), text)
            .with_spine_href(chapter.spine_href.clone());
    }
    flow.css = doc.stylesheet();
    flow
}

/// 把一个平台事件投进窗口 —— 与真机同一入口（`Window::dispatch_event`），
/// 不直调会话方法，注册在 paint 里的那层监听必须真的收得到。
pub(super) fn dispatch(cx: &mut TestAppContext, handle: AnyWindowHandle, event: PlatformInput) {
    cx.update_window(handle, |_, window, cx| {
        window.dispatch_event(event, cx);
    })
    .expect("事件应送达窗口");
    cx.run_until_parked();
}

pub(super) fn mouse_down(cx: &mut TestAppContext, handle: AnyWindowHandle, at: Point<Pixels>) {
    dispatch(
        cx,
        handle,
        MouseDownEvent {
            button: MouseButton::Left,
            position: at,
            ..Default::default()
        }
        .to_platform_input(),
    );
}

pub(super) fn mouse_move(cx: &mut TestAppContext, handle: AnyWindowHandle, at: Point<Pixels>) {
    dispatch(
        cx,
        handle,
        MouseMoveEvent {
            position: at,
            pressed_button: Some(MouseButton::Left),
            ..Default::default()
        }
        .to_platform_input(),
    );
}

pub(super) fn mouse_up(cx: &mut TestAppContext, handle: AnyWindowHandle, at: Point<Pixels>) {
    dispatch(
        cx,
        handle,
        MouseUpEvent {
            button: MouseButton::Left,
            position: at,
            ..Default::default()
        }
        .to_platform_input(),
    );
}

/// 第 `line` 行第 `k` 个字的中心（窗口坐标）。
///
/// 换算与 `ReaderSession::to_layout` 互逆：排版坐标加回页眉让位、再减掉当前的 `scroll_y`。
/// 不这么办就等于假设正文从没动过 —— 恢复请求是在头几帧里落地的（`settle` 的那几帧正文
/// 自己挪走一档），按「滚动为 0」取点，点到的就已经是别的一行。字的横向位置按字号估：
/// 命中引擎会 snap 到落点最近的字，差小半个字不影响断言。
pub(super) fn char_point(
    view: &Entity<ReaderSession>,
    cx: &TestAppContext,
    line: usize,
    k: usize,
) -> Point<Pixels> {
    view.read_with(cx, |v, _| {
        let box_ = &v.lines()[line];
        let char_w = box_.dominant_font_size();
        point(
            px(v.prefs.margin_x + box_.x + char_w * (k as f32 + 0.5)),
            px(box_.y + box_.height * 0.5 - v.scroll_y + v.header_reserve),
        )
    })
}

/// 按下 → 拖过阈值 → 松手，成立一个选区（共享给几条用例的前置步骤）。
///
/// 起点在第 1 行：那是这份语料里第一处「上面还有字」的位置，两端都测得着。
pub(super) fn drag_select(
    cx: &mut TestAppContext,
    view: &Entity<ReaderSession>,
    handle: AnyWindowHandle,
) {
    let start = char_point(view, cx, 1, 2);
    let end = char_point(view, cx, 1, 4);
    drag_select_between(cx, handle, start, end);
    assert!(
        view.read_with(cx, |v, _| v.selection().is_some()),
        "拖过阈值的松手应成立选中"
    );
}

/// 同样的三连，起止点由调用方给 —— 只有一行字的语料里 `drag_select` 那个第 1 行不存在。
pub(super) fn drag_select_between(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    start: Point<Pixels>,
    end: Point<Pixels>,
) {
    mouse_down(cx, handle, start);
    for step in 1..=4 {
        let t = step as f32 / 4.0;
        mouse_move(
            cx,
            handle,
            point(
                px(f32::from(start.x) + (f32::from(end.x) - f32::from(start.x)) * t),
                px(f32::from(start.y) + (f32::from(end.y) - f32::from(start.y)) * t),
            ),
        );
    }
    mouse_up(cx, handle, end);
}

// ---- 翻页动画：左右平移 / 上下平移 / 连续滚动的补间（`chrome::slide_shifts`、`turn_frame`）----
/// 走一帧翻页动画：先把测试时钟推进 `wait`，再渲一帧。
///
/// 必须推时钟：`Presence` 的进度取自 `background_executor().now()`，而测试时钟不会自己走
/// （`run_until_parked` 只走到下一个到期的定时器）。不推就是永远停在起步那一格 ——
/// 那种红看着像「动画没接上」，其实只是表没走。
pub(super) fn turn_frame(cx: &mut TestAppContext, handle: AnyWindowHandle, wait: Duration) {
    cx.executor().advance_clock(wait);
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("渲染应成功");
    cx.run_until_parked();
}

/// 一根手指的原始触摸，与真机同一入口：识别器就在这条路上跑（`gestures.rs` 把一笔折成
/// `Started` 一整截 + `Moved` 若干步 + `Ended` 收尾的 `ScrollWheelEvent`，横轴锁死）。
/// 横扫那两条门都从这里发事件，所以它们验的是真路，不是绕开识别器直发事件。
pub(super) fn raw_touch(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    phase: TouchPhase,
    x: f32,
) {
    raw_touch_at(cx, handle, phase, point(px(x), px(200.0)));
}

/// 同一条，但落点由调用方给：竖着那一档要测的是 y。
pub(super) fn raw_touch_at(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    phase: TouchPhase,
    at: Point<Pixels>,
) {
    use gpui_kit::{TouchEvent, TouchId};

    dispatch(
        cx,
        handle,
        TouchEvent {
            id: TouchId(1),
            phase,
            position: at,
            predicted_position: None,
            force: None,
        }
        .to_platform_input(),
    );
}
