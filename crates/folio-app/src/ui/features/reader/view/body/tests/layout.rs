//! 排版引擎经真实 EPUB provider 与字体栈的集成验收（M1–M3 的门）。

use super::harness::{epub_flow, open, prefs, settle};
use crate::ui::features::reader::model::session::ReaderSession;
use folio_core::{Chapter, FlowDoc, Locator, Reader};
use gpui_kit::AnyWindowHandle;
#[allow(unused_imports)] // `#[gpui_kit::test]` 展开要它，unused 分析看不见宏里的使用
use gpui_kit::{AppContext as _, TestAppContext};

/// M2 的验收门：中文排版基础三件事必须由排版引擎产出。上一版在桌面上这些是
/// `TextView` 画的，测到的是框架的行为；现在两端都是自绘，测的是引擎自己的行盒。
#[gpui_kit::test]
fn the_engine_indents_centers_and_spaces_lines(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = epub_flow();
    assert_eq!(flow.chapters.len(), 3, "spine 三章");

    let (view, _handle) = open(
        cx,
        &flow,
        "epub",
        Reader {
            font_size: 20.0,
            line_height: 1.8,
            ..Reader::default()
        },
        Locator::start(),
        460.0,
    );
    let lines = view.read_with(cx, |v, _| v.lines());
    let texts = view.read_with(cx, |v, _| v.line_texts());
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

    let body = lines
        .iter()
        .find(|l| l.dominant_font_size() == 20.0 && l.x > 0.0)
        .expect("应有一个首行被缩进的正文行");
    assert!(
        (body.x - 40.0).abs() < 1.0,
        "2em 缩进在 20px 字号下应是 40px，实际 {}",
        body.x
    );
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
        .source()
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
    use folio_core::content::format::css::computed::Color;

    cx.update(gpui_kit::init);
    let flow = epub_flow();
    let (view, _handle) = open(
        cx,
        &flow,
        "epub",
        Reader::default(),
        Locator::start(),
        460.0,
    );
    let lines = view.read_with(cx, |v, _| v.lines());

    // 标点挤压会把收尾标点切成单独一段，run 不再按「一行一组」聚；这里按整屏的
    // run 判三种局部样式都在，不再要求它们恰好落在同一条「≥4 run」的行上。
    let all: Vec<_> = lines.iter().flat_map(|l| l.text_runs()).collect();
    assert!(
        all.iter().any(|r| r.style.font.weight >= 700),
        "strong 应带粗体"
    );
    assert!(all.iter().any(|r| r.style.font.italic), "em 应带斜体");
    assert!(
        all.iter()
            .any(|r| r.style.color == Some(Color(0xC0, 0x39, 0x2B, 255))),
        "span.red 的局部颜色要落到 run 上"
    );
    assert!(
        all.iter().all(|r| r.width > 0.0),
        "每个 run 都要有实际绘制宽度"
    );
    let line = lines
        .iter()
        .find(|l| l.text_runs().next().is_some())
        .expect("至少有一行文本");
    let sum: f32 = line.text_runs().map(|r| r.width).sum();
    assert!(
        (sum - line.width).abs() < 0.5,
        "行宽应是各 run 宽度之和：{sum} vs {}",
        line.width
    );
}

/// M3-E3 验收：EPUB 章里的内嵌图要经排版引擎变成图片片段，`src` 按本章目录解析到
/// 容器路径，且可见文本里不留哨兵字符。字节按需现取的这一层由 `BookImages` 管，
/// 这里钉的是排版侧：解析成容器路径、带出固有尺寸。
#[gpui_kit::test]
fn images_flow_through_the_engine_as_fragments(cx: &mut TestAppContext) {
    use folio_core::content::format::ImageRes;

    cx.update(gpui_kit::init);
    let flow = FlowDoc {
        chapters: vec![
            Chapter::inline(
                "图",
                "<p>前<img src=\"../Images/pic.png\" alt=\"图\">后</p>",
            )
            .with_spine_href("OEBPS/Text/chap.xhtml"),
        ],
        images: vec![ImageRes {
            href: "OEBPS/Images/pic.png".into(),
            source: "OEBPS/Images/pic.png".into(),
            media_type: "image/png".into(),
            width: 40,
            height: 20,
        }],
        ..Default::default()
    };
    let (view, _handle) = open(
        cx,
        &flow,
        "epub",
        Reader::default(),
        Locator::start(),
        460.0,
    );
    let lines = view.read_with(cx, |v, _| v.lines());
    let texts = view.read_with(cx, |v, _| v.line_texts());

    assert_eq!(texts[0], "前后", "行内图不产出字符，哨兵要被去掉");
    let img = lines[0]
        .images()
        .next()
        .expect("../Images/pic.png 应解析成 OEBPS/Images/pic.png 并命中");
    assert_eq!(img.href, "OEBPS/Images/pic.png");
    assert_eq!((img.width, img.height), (40.0, 20.0));
}

#[gpui_kit::test]
#[ignore = "探针：读 /tmp/folio-css/h2probe.epub（本机合成的那一份），CI 机器上没那个目录"]
fn scratch_probe_h2_metrics(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let path = std::path::Path::new("/tmp/folio-css/h2probe.epub");
    let doc = crate::ui::features::reader::model::chapter_layout::open(path).expect("probe 开得出");
    let mut flow = doc.flow().expect("可重排").clone();
    for (ix, chapter) in flow.chapters.iter_mut().enumerate() {
        let text = doc.chapter_source(ix).expect("正文");
        *chapter = folio_core::Chapter::inline(chapter.title.clone(), text)
            .with_spine_href(chapter.spine_href.clone());
    }
    flow.css = doc.stylesheet();
    let (view, handle) = super::harness::open(
        cx,
        &flow,
        "epub",
        super::harness::prefs(16.0),
        folio_core::Locator::start(),
        460.0,
    );
    super::harness::settle(cx, handle);
    let lines = view.read_with(cx, |v, _| v.lines());
    let texts = view.read_with(cx, |v, _| v.line_texts());
    for (l, t) in lines.iter().zip(texts.iter()) {
        println!(
            "y={:.1} h={:.1} bl={:.1} size={:.1} asc={:.1} desc={:.1} text={}",
            l.y,
            l.height,
            l.baseline,
            l.dominant_font_size(),
            l.text_runs().map(|r| r.ascent).fold(0.0f32, f32::max),
            l.text_runs().map(|r| r.descent).fold(0.0f32, f32::max),
            t
        );
    }
}

/// 一叠三张图：一帧只画一张（角上有 `k/n`），切一张之后**不许重排**。
///
/// 一叠三张图的画廊那一本：三张 `600×300` 的图各占一段，排完落在**同一叠**里。
///
/// 两条关于这一叠的门共用它（切图不重排、浮层压着时不切图），夹具只在这里建一次。
/// `from_content` 造出来的书没有容器，`Book::new` 会把图片索引换成空表 —— 补上真清单
/// 与内存字节，走的是生产上 EPUB 那一条（同 `capture.rs` 那一族）。
fn gallery(cx: &mut TestAppContext) -> (gpui_kit::Entity<ReaderSession>, AnyWindowHandle) {
    use crate::ui::foundation::images::{BookImages, ChapterImages};

    cx.update(gpui_kit::init);
    let png: Vec<u8> = {
        use image::ImageEncoder as _;
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(
                &vec![200u8; 600 * 300 * 3],
                600,
                300,
                image::ExtendedColorType::Rgb8,
            )
            .expect("夹具 PNG 该编得出来");
        bytes
    };
    let image = |href: &str| folio_core::content::format::ImageRes {
        href: href.into(),
        source: href.into(),
        media_type: "image/png".into(),
        width: 600,
        height: 300,
    };
    let images = vec![image("a.png"), image("b.png"), image("c.png")];
    let flow = FlowDoc {
        chapters: vec![Chapter::inline(
            "画廊",
            "<p><img src=a.png></p><p><img src=b.png></p><p><img src=c.png></p><p>正文</p>",
        )],
        images: images.clone(),
        ..Default::default()
    };
    let meta = super::harness::meta("epub");
    let doc: std::sync::Arc<dyn folio_core::content::document::Document> = std::sync::Arc::from(
        folio_core::content::document::from_content(meta, folio_core::Content::Reflow(flow)),
    );
    let (view, handle) = super::harness::build_with_document(
        cx,
        doc,
        prefs(16.0),
        Locator::start(),
        460.0,
        None,
        None,
        folio_core::PageTurn::Slide,
    );
    view.update(cx, |v, _| {
        let book = match &mut v.source {
            crate::ui::features::reader::model::session::Source::Book(book) => book,
            _ => panic!("前置：这一本该是可重排那一族"),
        };
        let bytes = png.clone();
        let source: crate::ui::foundation::images::ByteSource =
            std::sync::Arc::new(move |_href: &str| Some(bytes.clone()));
        let index = std::sync::Arc::new(BookImages::new(&images, source));
        book.document_mut()
            .set_images(std::sync::Arc::new(ChapterImages::new(index, "c0.xhtml")));
    });
    settle(cx, handle);
    (view, handle)
}

/// 「现在看第几张」住界面（`Document::stack_current`），它不进 `LayoutKey`、不进样式哈希 ——
/// 那等于每划一下重排整章，分页档的页数还会跟着变 —— 那是硬约束。
/// 这条门量的就是这一格：切图前后行数与页数一个数都不许动，而画出来的确实换了一张。
#[gpui_kit::test]
fn a_stack_draws_one_member_and_switches_without_relayout(cx: &mut TestAppContext) {
    use gpui_kit::test::TestWindowExt as _;

    let (view, handle) = gallery(cx);
    let marks = view.read_with(cx, |v, _| {
        v.book()
            .expect("有书")
            .document()
            .layout()
            .lines
            .iter()
            .filter_map(|line| line.stack)
            .collect::<Vec<_>>()
    });
    assert_eq!(marks.len(), 3, "三张图都该带叠标记：{marks:?}");
    assert!(
        marks.iter().all(|m| m.count == 3 && m.id == marks[0].id),
        "一叠三格：{marks:?}"
    );
    let badge = cx
        .update_window(handle, |_, window, _| {
            window.try_find("reader-stack-badge").is_some()
        })
        .expect("窗口应能渲染");
    assert!(
        badge,
        "一叠只画一张：角标只该有一条，读者才知道这里还有两张"
    );

    let (lines, pages) = view.read_with(cx, |v, _| (v.lines().len(), v.page_count()));
    let moved = view.update(cx, |v, _| {
        let book = v.book().expect("有书");
        let mark = book
            .document()
            .layout()
            .lines
            .iter()
            .find_map(|line| line.stack)
            .expect("上面刚数过有叠");
        book.document().advance_stack(mark.id, 1, mark.count)
    });
    assert!(moved, "第 1 张 → 第 2 张该报「变了」");
    settle(cx, handle);
    let (lines_after, pages_after, current) = view.read_with(cx, |v, _| {
        let book = v.book().expect("有书");
        let mark = book
            .document()
            .layout()
            .lines
            .iter()
            .find_map(|line| line.stack)
            .expect("叠还在");
        (
            v.lines().len(),
            v.page_count(),
            book.document().stack_current(mark.id),
        )
    });
    assert_eq!(current, 1, "切到了第 2 张");
    assert_eq!(
        (lines, pages),
        (lines_after, pages_after),
        "切图不许重排：行数与页数都不该动"
    );
    // 两头停住：回到第一张之后不许回绕，也不该再报「变了」。
    let again = view.update(cx, |v, _| {
        let doc = v.book().expect("有书").document();
        (
            doc.advance_stack(0, -1, 3),
            doc.advance_stack(0, -1, 3),
            doc.stack_current(0),
        )
    });
    assert_eq!(again, (true, false, 0), "走到第一张就该停住，不回绕");

    // 分页档也要那两条「还能滑」的提示。分页这一臂从前只画当前那张（角标有、卡片边
    // 与小点没有），所以分页里叠起来的几张看起来就是一整张图，读者不知道还能横滑。
    view.update(cx, |v, cx| {
        v.set_mode(
            crate::ui::features::reader::model::chrome::PageMode::Paged,
            cx,
        )
    });
    settle(cx, handle);
    let (peek, dots) = cx
        .update_window(handle, |_, window, _| {
            (
                window.try_find("reader-stack-peek").is_some(),
                window.try_find("reader-stack-dots").is_some(),
            )
        })
        .expect("窗口应能渲染");
    assert!(peek, "分页档也该在图下露出「下一张」的卡片边");
    assert!(dots, "分页档也该有这一叠的小点");
}

/// 浮层（顶栏底栏）压着的时候，横划那一叠**一张都不切**，也不在叠的尽头翻页。
///
/// 切图那条路从前绕在认领之外：`swipe_scroll` 开头按「起手压在某一叠上」单独判，
/// 走的是 `stack_grab` 那一格，`claim_swipe` 里那条「浮层压着不认领」根本管不到它 ——
/// 于是工具栏露头之后，读者在整页大的图上横扫一下照样换了一张（与「那一击的意思是
/// 收浮层，不是操作正文」这条规矩相反）。这条门把两半都钉住：浮层开着不切，收了照切。
#[gpui_kit::test]
fn swiping_a_stack_yields_while_the_overlay_is_up(cx: &mut TestAppContext) {
    use crate::ui::features::reader::model::chrome::PageMode;
    use gpui_kit::{TouchPhase, point, px};

    let (view, handle) = gallery(cx);
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    settle(cx, handle);

    // 这一叠在屏上占的那一格：绘制那一趟登记的命中区，窗口坐标。
    let hit = view
        .read_with(cx, |v, _| {
            v.stack_hits
                .borrow()
                .iter()
                .rev()
                .find(|hit| hit.count > 1)
                .cloned()
        })
        .expect("前置：这一叠的命中区该登记过");
    let center = point(
        px(f32::from(hit.rect.origin.x) + f32::from(hit.rect.size.width) / 2.0),
        px(f32::from(hit.rect.origin.y) + f32::from(hit.rect.size.height) / 2.0),
    );
    let (mid_x, mid_y) = (f32::from(center.x), f32::from(center.y));
    let current = |cx: &mut TestAppContext| {
        view.read_with(cx, |v, _| {
            v.book().expect("有书").document().stack_current(hit.id)
        })
    };

    // 走的那一截按**那一叠自己的宽**算：`swipe_scroll` 里那条门槛是「图宽的五分之一，
    // 且不小于 24 逻辑像素」（2026-11-01 真机量出来的档），写死像素的话窗口一改就会
    // 悄悄落到线的另一边 —— 用例绿着而什么都没钉。
    let travel = (f32::from(hit.rect.size.width) / 5.0).max(24.0) * 2.0;

    // 先把「收了浮层照切」这一半钉住：往左走过门槛 = 下一张。
    //
    // 钟要一格一格推，而且要**分三格走完**：这一路吃的是识别器按帧折出来的 `ScrollEvent`
    // （原始触摸 → 锁轴 → 增量），钟不动就只有 `Started` 那一条，`stack_grab` 那一格
    // 永远收不到增量；而折出来的一条比派发晚一帧到手（实测：一次 Moved 之后读 `stack_dx`
    // 还是 0，下一格才见第一段），一格里走完就少算一段，差的正好是门槛那一截。
    let swipe = |cx: &mut TestAppContext, dir: f32| {
        let at = |x: f32| point(px(x), px(mid_y));
        let frame = std::time::Duration::from_millis(16);
        let steps = 3.0;
        super::harness::raw_touch_at(cx, handle, TouchPhase::Started, center);
        for step in 1..=3 {
            let frac = step as f32 / steps;
            cx.executor().advance_clock(frame);
            super::harness::raw_touch_at(
                cx,
                handle,
                TouchPhase::Moved,
                at(mid_x + dir * travel * frac),
            );
        }
        super::harness::raw_touch_at(cx, handle, TouchPhase::Ended, at(mid_x + dir * travel));
        settle(cx, handle);
    };
    swipe(cx, -1.0);
    assert_eq!(current(cx), 1, "前置：浮层收着时横扫该切到第二张");

    view.update(cx, |v, cx| v.set_overlay_open(true, cx));
    swipe(cx, 1.0);
    assert_eq!(
        current(cx),
        1,
        "浮层压着的时候横划不该切图 —— 那一划的意思是收浮层"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        0,
        "叠的尽头那一下也不该顺手翻页"
    );

    view.update(cx, |v, cx| v.set_overlay_open(false, cx));
    swipe(cx, 1.0);
    println!(
        "第三笔之后: dx={:?} grab={:?} frame={} current={}",
        view.read_with(cx, |v, _| v.stack_dx),
        view.read_with(cx, |v, _| v.stack_grab.map(|h| h.frame)),
        view.read_with(cx, |v, _| v.image_hits_frame),
        current(cx)
    );

    assert_eq!(current(cx), 0, "收了浮层，同样的横扫照切");
}

/// 叠图跟手动画：写会话状态，不碰排版（行数页数一个数都不许动）；
/// 松手补完到 1 之后动画自己清掉，静态地只画目标那张。
#[gpui_kit::test]
fn a_stack_drag_animates_without_relayout_and_settles(cx: &mut TestAppContext) {
    let (view, handle) = gallery(cx);
    settle(cx, handle);
    let hit = view
        .read_with(cx, |v, _| {
            v.stack_hits
                .borrow()
                .iter()
                .rev()
                .find(|hit| hit.count > 1)
                .cloned()
        })
        .expect("前置：这一叠的命中区该登记过");
    let (lines, pages) = view.read_with(cx, |v, _| (v.lines().len(), v.page_count()));

    view.update(cx, |v, cx| v.drag_stack(&hit, 0.5, 1, cx));
    let (from, to, progress) = view.read_with(cx, |v, _| {
        let m = v.stack_motion.as_ref().expect("跟手那一笔该记下");
        (m.from, m.to, m.progress)
    });
    assert_eq!((from, to), (0, 1), "从当前那张滑向下一张");
    assert!((progress - 0.5).abs() < 1e-6, "进度跟着手指走");
    assert_eq!(
        view.read_with(cx, |v, _| (v.lines().len(), v.page_count())),
        (lines, pages),
        "切图不许重排"
    );

    // 目标全清还没到手时，快照把进度封顶 0.4：旧图不会整张被推走，屏上至少有一张图。
    let clamped = view.read_with(cx, |v, cx| v.stack_motion_view(cx));
    assert!(clamped.is_some(), "动画在跑时该给绘制层一份快照");
    assert!(
        clamped.expect("上面刚判过").progress <= 0.4,
        "目标没解码好时不能把旧图推到屏外"
    );

    // 松手：补完到 1，走完时长后动画清账。
    view.update(cx, |v, cx| v.settle_stack(true, cx));
    let (started, duration) = view.read_with(cx, |v, _| {
        let m = v.stack_motion.as_ref().expect("补完该起表");
        (m.started, m.duration)
    });
    view.update(cx, |v, cx| v.advance_stack_motion(started + duration, cx));
    assert!(
        view.read_with(cx, |v, _| v.stack_motion.is_none()),
        "补完到 1 之后动画自己清掉，静态只画目标那张"
    );
}

/// 没过阈值那一笔补完到 0（弹回原图），同样会清账；动画不会挂在屏上不走。
#[gpui_kit::test]
fn a_cancelled_stack_drag_springs_back_to_the_original(cx: &mut TestAppContext) {
    let (view, handle) = gallery(cx);
    settle(cx, handle);
    let hit = view
        .read_with(cx, |v, _| {
            v.stack_hits
                .borrow()
                .iter()
                .rev()
                .find(|hit| hit.count > 1)
                .cloned()
        })
        .expect("前置：命中区该登记过");
    view.update(cx, |v, cx| {
        v.drag_stack(&hit, 0.3, 1, cx);
        v.settle_stack(false, cx);
    });
    let (started, duration) = view.read_with(cx, |v, _| {
        let m = v.stack_motion.as_ref().expect("补完该起表");
        assert_eq!(m.settle_to, 0.0, "没过阈值就弹回 0");
        (m.started, m.duration)
    });
    view.update(cx, |v, cx| v.advance_stack_motion(started + duration, cx));
    assert!(view.read_with(cx, |v, _| v.stack_motion.is_none()));
}
