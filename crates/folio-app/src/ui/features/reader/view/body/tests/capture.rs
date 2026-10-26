//! 折页捕获与烘焙：哪一帧该烘、烘哪一页、纹理何时作废。

use super::harness::{PARA, build_with_document, flow, meta, open, prefs, settle, turn_frame};
use crate::ui::features::reader::model::chrome::PageMode;
use crate::ui::features::reader::model::turn::TurnIntent;
use crate::ui::services::config::ConfigHolder;
use crate::ui::shell::NAV_TRANSITION;
use folio_core::content::document::Document;
use folio_core::{Chapter, Content, FlowDoc, Locator, PageTurn};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Context, IntoElement, ParentElement, Render, Styled,
    TestAppContext, Window, canvas, div, px, size,
};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// 进书那 280ms 里旧页纹理没烘好：这一下**不能吞掉**，要排队等门开。
///
/// 从前的行为是 `turn()` 直接 return、`claim_swipe` 直接 `Refused`，读者读到的是
/// 「第一划没反应，过一会儿才行」。这条钉的是排队与补执行。
#[gpui_kit::test]
fn a_curl_turn_waits_for_the_capture_instead_of_being_dropped(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    cx.update(|app| {
        ConfigHolder::edit_appearance(app, |a| a.page_turn = PageTurn::Curl);
    });
    settle(cx, handle);
    assert!(
        view.read_with(cx, |v, _| v.page_count()) > 1,
        "这本书至少要有两页才测得到下一趟"
    );

    let before = view.read_with(cx, |v, _| v.page());
    view.update(cx, |v, _| {
        // 模拟「进场横移还没结束、纹理没烘好」：门关着。
        v.capture_dirty = true;
        v.pending_turn = None;
    });
    view.update(cx, |v, cx| v.turn(TurnIntent::Next, cx));
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        before,
        "门没开时不该翻页"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.pending_turn),
        Some(TurnIntent::Next),
        "被拒的那一趟该排队，而不是丢掉"
    );

    // 纹理落地：门开，渲染帧末补执行。
    let key = view.read_with(cx, |v, _| (v.chapter_index(), v.page()));
    view.update(cx, |v, _| {
        v.capture_dirty = false;
        v.captured_page = Some(key);
    });
    view.update(cx, |v, cx| v.drain_pending_turn(cx));
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        before + 1,
        "门一开就该把排队那一趟补上"
    );
}

/// 进书之后**最终**一定要烘到那一张：上面那条门不许把捕获彻底掐死。
///
/// 若把 `content_ready` 判成恒假（比如拿错了「排完了没有」的那个问句），第一条门绿了、
/// 这一条红 —— 读者见到的就是「仿真翻页一直不出场，退成一张硬片」。
#[gpui_kit::test]
fn the_first_page_is_baked_once_the_typesetting_lands(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    cx.update(|app| {
        ConfigHolder::edit_appearance(app, |a| a.page_turn = PageTurn::Curl);
    });
    settle(cx, handle);
    settle(cx, handle);
    let current = view.read_with(cx, |v, _| (v.chapter_index(), v.page()));
    assert!(
        view.read_with(cx, |v, _| v
            .book()
            .is_some_and(|b| b.document().is_current())),
        "前置：排版该已经落地，否则这一条测的是「没排完」那一格"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.captured_page),
        Some(current),
        "排版落地之后这一页该烘好 —— 门不许把捕获掐死"
    );
    assert!(
        !view.read_with(cx, |v, _| v.capture_dirty),
        "烘好之后不该还挂着脏标记"
    );
    assert!(
        view.read_with(cx, |v, _| v.curl_capture_ready()),
        "门开着，第一划才能起跟手"
    );
}

/// 静止帧把**下一张纸**上的图先解掉：翻过去时那张图已经在上屏的那一帧里。
///
/// 从前正文只走 `painted`（这一帧要用才问），翻到带图的一页时先空一格、等后台解完再补。
/// 这条钉的是接上之后「翻过去那一帧，图集里已经有那一格」。
///
/// 夹具：两章，第一章两屏且第二屏整屏是那张 300×300 的 PNG；读者停在第一章第一屏。
/// 这一屏上没有它（`painted` 没碰过），所以只能由预取把它解出来。
#[gpui_kit::test]
fn the_next_page_image_is_decoded_before_the_reader_turns_to_it(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    // 一张 300×300 的 PNG：尺寸写进 `ImageRes`，字节由容器那条 `ByteSource` 现给。
    // 摆出来的框是「版心宽 × 同高」，所以它独占一整屏 —— 它是第二屏的全部内容。
    let png = {
        use image::ImageEncoder as _;
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(
                &vec![200u8; 300 * 300 * 3],
                300,
                300,
                image::ExtendedColorType::Rgb8,
            )
            .expect("夹具 PNG 该编得出来");
        bytes
    };
    let href = "Images/plate.png".to_string();
    let mut first = String::new();
    for i in 0..3 {
        first.push_str(&format!("<p>{PARA}（第 {i} 段）</p>"));
    }
    // 那张图靠作者的强制断点单独起一屏。为什么不用「数够几段文字正好填满第一屏」来摆：
    // 那种摆法把落点绑在行高上，行高一变图就漂去第三屏，而红出来的样子是「预取没解出图」
    // —— 看着像预取坏了（就这么红过一次，下面那条前置断言钉的是它）。
    first.push_str(&format!("<p class=\"plate\"><img src=\"{href}\" /></p>"));
    // 「最后一张图」必须有：`Book` 发现容器里一张图都没有时会把图片索引换成空的
    // （容器丢了那个降级），而这一条测的正是索引里有图。
    let tail = "<p><img src=\"Images/tail.png\" /></p>".to_string();
    let flow = FlowDoc {
        chapters: vec![
            Chapter::inline("第一章", first).with_spine_href("c0.xhtml"),
            Chapter::inline("第二章", format!("<p>{PARA}</p>{tail}")).with_spine_href("c1.xhtml"),
        ],
        css: ".plate { break-before: page; }".to_string(),
        ..Default::default()
    };
    // 清单单独留着：`from_content` 造出来的书没有容器，`Book::new` 会把图片索引换成空表
    // （「容器丢了」那条降级），所以下面在图建好之后再把这一份补进去。
    let images = vec![
        folio_core::content::format::ImageRes {
            href: "Images/plate.png".to_string(),
            source: "Images/plate.png".to_string(),
            media_type: "image/png".to_string(),
            width: 300,
            height: 300,
        },
        folio_core::content::format::ImageRes {
            href: "Images/tail.png".to_string(),
            source: "Images/tail.png".to_string(),
            media_type: "image/png".to_string(),
            width: 300,
            height: 300,
        },
    ];
    let meta = meta("markdown");
    let doc: Arc<dyn Document> = Arc::from(folio_core::content::document::from_content(
        meta,
        Content::Reflow(flow),
    ));
    let bytes: crate::ui::foundation::images::ByteSource = {
        let png = png.clone();
        std::sync::Arc::new(move |_href: &str| Some(png.clone()))
    };
    let (view, handle) = build_with_document(
        cx,
        doc,
        prefs(16.0),
        Locator::start(),
        460.0,
        None,
        None,
        PageTurn::Slide,
    );
    // `from_content` 造出来的书没有容器，`Book::new` 会把图片索引换成空表（「容器丢了」
    // 那条降级）。这里把真清单与内存字节补进去 —— 补的正是生产上 EPUB 那一份。
    view.update(cx, |v, _| {
        let book = match &mut v.source {
            crate::ui::features::reader::model::session::Source::Book(book) => book,
            _ => panic!("前置：这一本该是可重排那一族"),
        };
        let index = std::sync::Arc::new(crate::ui::foundation::images::BookImages::new(
            &images, bytes,
        ));
        book.document_mut().set_images(std::sync::Arc::new(
            crate::ui::foundation::images::ChapterImages::new(index, "c0.xhtml"),
        ));
    });
    // 那一屏上没有任何图可画 —— 所以先确认图集里是空的：下面那个非零只可能来自预取。
    let before = cx.update(|app| view.read_with(app, |v, cx| v.atlas.read(cx).resident_bytes()));
    settle(cx, handle);
    settle(cx, handle);
    assert!(
        view.read_with(cx, |v, _| v.page_count()) > 1,
        "前置：这一章要至少两屏，否则「下一张纸」就是它自己"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        0,
        "前置：读者停在第一屏"
    );
    // 前置：那张图必须在**下一张纸**上、且不在当前这一张上。这一条不成立时下面红的是
    // 「预取没解出图」，看起来像预取坏了，其实是夹具的图漂去了别的屏 —— 所以在这里说清楚。
    let image_pages = view.read_with(cx, |v, _| {
        let doc = v.book().expect("有书").document();
        doc.layout()
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.images().next().is_some())
            .map(|(line, _)| doc.page_of_line(line))
            .collect::<Vec<_>>()
    });
    assert_eq!(
        image_pages.first(),
        Some(&1),
        "前置：第一章那张图要在第 2 屏（读者在第 1 屏），实际这几屏上有它：{image_pages:?}"
    );
    // 图集里的键是「这本书的 token + 容器路径」。测试拿不到 token，所以问占用字节：
    // 只有真解过一张图才非零 —— 而这一屏上没有任何图可画，那是预取解出来的。
    let decoded = cx.update(|app| view.read_with(app, |v, cx| v.atlas.read(cx).resident_bytes()));
    assert!(
        decoded > before,
        "下一屏那张图该已经解好在图集里（预取接上了），实际占用 {decoded} 字节（之前 {before}）"
    );
}

/// 折页那一趟把**纸的背面**也烘了：双页是目标跨页的另一半，单页是目标页整张。
///
/// 卷起来那一片的背面采的是捕获层 2（`page_curl.wgsl` 里那句镜像 U），从前采的是正面的
/// 镜像 —— 读起来像墨透过来，而不是「这张纸背面印着下一页」。这一条钉的是接上之后「烘的
/// 是哪一格」：往前翻要目标那一页的左半，往后翻要它的右半（同一页在两趟里要的栏不一样）。
#[gpui_kit::test]
fn a_curl_turn_bakes_the_back_of_the_sheet_being_peeled(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 1200.0);
    view.update(cx, |v, cx| v.set_mode(PageMode::Spread, cx));
    cx.update(|app| {
        ConfigHolder::edit_appearance(app, |a| a.page_turn = PageTurn::Curl);
    });
    settle(cx, handle);
    let resting = view.read_with(cx, |v, _| (v.chapter_index(), v.page()));
    assert_eq!(
        view.read_with(cx, |v, _| v.captured_page),
        Some(resting),
        "前置：定住那一帧正面该烘好（门是开的）"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.captured_back),
        None,
        "前置：没起趟之前不该烘背面"
    );

    // 往前翻：自由边在右 ⇒ 揭起来的是右半，背面 = 目标那一页的左半。
    view.update(cx, |v, cx| v.turn(TurnIntent::Next, cx));
    settle(cx, handle);
    let ahead = view.read_with(cx, |v, _| (v.chapter_index(), v.page()));
    assert_ne!(ahead.1, resting.1, "前置：这一趟真翻了页");
    assert_eq!(
        view.read_with(cx, |v, _| v.captured_back.map(|(back, _)| back)),
        Some((ahead.0, ahead.1, 0)),
        "往前翻该烘目标跨页的左半"
    );

    // 往后翻：自由边在左 ⇒ 背面换成退回来那一跨页的右半。先把上一趟走完、再让正面为新的
    // 一页烘一次（门没开的话这一趟会排队，那就是另一条门在测的事了）。
    turn_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));
    settle(cx, handle);
    settle(cx, handle);
    assert_eq!(
        view.read_with(cx, |v, _| v.captured_page),
        Some(ahead),
        "前置：正面该已经为新一页重烘好"
    );
    view.update(cx, |v, cx| v.turn(TurnIntent::Previous, cx));
    assert_eq!(
        view.read_with(cx, |v, _| v.pending_turn),
        None,
        "门开着，这一趟不该排队"
    );
    settle(cx, handle);
    let back_to = view.read_with(cx, |v, _| (v.chapter_index(), v.page()));
    assert_eq!(
        view.read_with(cx, |v, _| v.captured_back.map(|(back, _)| back)),
        Some((back_to.0, back_to.1, 1)),
        "往后翻该烘退回来那一跨页的右半"
    );
}

/// 单页那一档没有「另一半」可取：背面就是目标页整张（与静读天下那条实测一致 —— 卷起来的
/// 那一片背面是下一页的真字，不是透墨）。
#[gpui_kit::test]
fn a_single_page_curl_bakes_the_arriving_page_on_its_back(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    cx.update(|app| {
        ConfigHolder::edit_appearance(app, |a| a.page_turn = PageTurn::Curl);
    });
    settle(cx, handle);
    let resting = view.read_with(cx, |v, _| (v.chapter_index(), v.page()));
    assert_eq!(
        view.read_with(cx, |v, _| v.captured_page),
        Some(resting),
        "前置：正面已经烘好"
    );
    assert!(
        view.read_with(cx, |v, _| v.page_count()) > 1,
        "这本书至少要有两页才测得到下一趟"
    );

    view.update(cx, |v, cx| v.turn(TurnIntent::Next, cx));
    settle(cx, handle);
    let ahead = view.read_with(cx, |v, _| (v.chapter_index(), v.page()));
    assert_eq!(
        view.read_with(cx, |v, _| v.captured_back.map(|(back, _)| back)),
        Some((ahead.0, ahead.1, 0)),
        "单页的背面该是目标页整张（只有一栏）"
    );
}

/// 捕获矩形按**最终位置**取：横向抵消外壳进场那一段的横移（[`super::super::capture_region`]）。
///
/// 测试台没有外壳，横移不会真的把内容推走，所以这里把两半分开钉：
/// - 纯函数那一半钉**符号**（往回减，不是往前加）；
/// - 会话那一半钉「窗口外就拒烘」。这一条**不是**「进场期间不烘」：副本只进捕获层、主 pass
///   取不到它（层隔离由 `a_capture_layer_wraps_its_whole_subtree_and_pops_back` 钉），
///   所以横移在真机上不影响能不能烘 —— 那里内容跟着一起偏，矩形仍在窗口内。
#[gpui_kit::test]
fn the_capture_rect_is_taken_at_the_resting_position(cx: &mut TestAppContext) {
    use crate::ui::features::reader::motion::turn_animation::Area;
    let area = Area::new(0.0, 40.0, 900.0, 700.0);
    let rect = super::super::capture_region(area, 300.0);
    assert_eq!(f32::from(rect.origin.x), -300.0, "该往回减，不是往前加");
    assert_eq!(f32::from(rect.origin.y), 40.0, "纵向没有横移");
    assert_eq!(f32::from(rect.size.width), 900.0);
    let viewport = (900.0, 700.0);
    assert!(
        !super::super::in_viewport(rect, viewport),
        "偏出窗口的那一趟不该请求捕获"
    );
    assert!(super::super::in_viewport(
        super::super::capture_region(area, 0.0),
        viewport
    ));

    cx.update(gpui_kit::init);
    let flow = flow();
    let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    settle(cx, handle);
    let key = view.read_with(cx, |v, _| (v.chapter_index(), v.page()));
    assert_eq!(
        view.read_with(cx, |v, _| v.captured_page),
        Some(key),
        "前置：定住那一帧烘好了"
    );
    view.update(cx, |v, _| {
        v.enter_shift = 300.0;
        v.capture_dirty = true;
        v.captured_page = None;
    });
    settle(cx, handle);
    assert_eq!(
        view.read_with(cx, |v, _| v.captured_page),
        None,
        "偏移没对上时那一格在窗口外，该拒烘"
    );
    view.update(cx, |v, _| v.enter_shift = 0.0);
    settle(cx, handle);
    assert_eq!(
        view.read_with(cx, |v, _| v.captured_page),
        Some(key),
        "落定后该把没收成的那一张补烘上"
    );
}

/// 窗口尺寸与那张捕获纹理不同源 ⇒ 必须重烘。
///
/// 「烘过哪一页」那个键拦不住这一格：分屏、折叠展开、surface 重建之后章号页号一个字都不动，而
/// 纹理里被写过的只有**当时**那块矩形，网格取 uv 却按**今天**的窗口归一 —— 差出来的那一截采到没
/// 被写过的地方，读出来就是黑。这里不去动测试平台的 resize（那一段不归这一档管），直接把
/// 「记下来的尺寸与今天不同」那一格状态摆出来，看下一帧认不认。
#[gpui_kit::test]
fn a_stale_captured_size_forces_a_recapture(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
    cx.update(|app| {
        ConfigHolder::edit_appearance(app, |a| a.page_turn = PageTurn::Curl);
    });
    settle(cx, handle);
    let fresh = view.read_with(cx, |v, _| {
        (v.captured_size, v.capture_dirty, v.captured_page)
    });
    assert!(
        fresh.0.is_some() && !fresh.1,
        "定住的这一帧该已经把当前页烘了，实得 {fresh:?}"
    );

    // 摆成「窗口变了」那一样子：尺寸那一格与今天对不上，页号章号一个字不动。
    view.update(cx, |v, _| v.captured_size = Some((0.0, 0.0)));
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("渲染应成功");
    // 捕获是两帧的事：这一帧在绘制回调里发请求并记下 `capture_committed`，下一次
    // `render` 开头才把它落到 `captured_page` / `captured_size`。
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("第二帧应成功");
    cx.run_until_parked();
    let now = view.read_with(cx, |v, _| {
        (v.captured_size, v.capture_dirty, v.captured_page)
    });
    assert_ne!(
        now.0,
        Some((0.0, 0.0)),
        "尺寸不同源却没重烘：那条黑就是从这里来的"
    );
    assert!(!now.1, "重烘之后脏标记该落下，实得 {now:?}");
    assert_eq!(now.2, fresh.2, "重烘不改页号");
}

/// 捕获副本的壳：切层、画子树、切回主场景，三步必须在**同一趟 `paint`** 里。
///
/// 这一条钉的是「整页字印了两遍」。副本与上屏那一份共用坐标、共用字色，只要有一帧落进
/// 主 pass，屏上就是同一页逐字重合地再画一遍 —— 而它只在这一页「还没烘好」的那几帧才画
/// （[`super::super::capture_wanted`]），所以读起来是「有的页面有、有的页面没有」。
///
/// 从前这里是「`canvas` 闸 → 内容 → `canvas` 闸」三个兄弟元素，靠绘制顺序保证内容在层里；
/// 闸那一步一旦被跳过（`canvas` 没上屏、重放把 `SetCaptureLayer` 丢了），内容就漏到主场景。
/// 现在壳把三步收在一次 `paint` 里，这条门读的就是那三个位置：**壳前 0、壳里 1、壳后 0**。
/// 三个读数缺哪个都会红 —— 尤其是最后那个：漏了切回去，后面整页内容都会排进捕获段。
struct LayerProbe {
    seen: Rc<RefCell<[u8; 3]>>,
}

impl Render for LayerProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let before = self.seen.clone();
        let inside = self.seen.clone();
        let after = self.seen.clone();
        div()
            .size_full()
            .child(canvas(
                |_, _, _| {},
                move |_, _, window, _| before.borrow_mut()[0] = window.capture_layer(),
            ))
            .child(super::super::capture_layer(
                1,
                div().size_full().child(canvas(
                    |_, _, _| {},
                    move |_, _, window, _| inside.borrow_mut()[1] = window.capture_layer(),
                )),
            ))
            .child(canvas(
                |_, _, _| {},
                move |_, _, window, _| after.borrow_mut()[2] = window.capture_layer(),
            ))
    }
}

#[gpui_kit::test]
fn a_capture_layer_wraps_its_whole_subtree_and_pops_back(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let seen = Rc::new(RefCell::new([9u8, 9, 9]));
    let probe = seen.clone();
    let window = cx.open_window(size(px(200.0), px(120.0)), move |_, _| LayerProbe {
        seen: probe,
    });
    let handle: AnyWindowHandle = window.into();
    // `open_window` 自己画了一帧；再显式走一趟，读数取的是真正调过绘制回调的那一帧。
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("渲染应成功");
    let seen = *seen.borrow();
    assert_eq!(
        seen,
        [0, 1, 0],
        "壳的三个读数该是「主场景 → 捕获层 1 → 主场景」，实得 {seen:?}"
    );
}
