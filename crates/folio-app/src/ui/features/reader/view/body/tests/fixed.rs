//! 定版式（PDF）：页栈、滚动、选中、搜索与缩放入口。

use super::harness::{
    dispatch, drag_select_between, open_with_document, prefs, raw_touch_at, scroll_down, settle,
    turn_frame,
};
use crate::ui::features::reader::model::chrome::PageMode;
use crate::ui::features::reader::model::session::ReaderSession;
use crate::ui::services::config::ConfigHolder;
use crate::ui::shell::NAV_TRANSITION;
use folio_core::content::document::Document;
use folio_core::store::Store;
use folio_core::{Content, Format, Locator, PageTurn};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, ElementId, Entity, InputEvent as _, LongPressEvent,
    PinchEvent, Pixels, Point, TestAppContext, TouchPhase, point, px,
};
use std::sync::Arc;
use std::time::Duration;

// ---- 定版式那一页上的选中：字选得中、复制得到、划得进库、点得回来 ----
/// 第 `index` 个字在屏上的落点（窗口坐标），`along` 是压在这个字的哪一分之几处。
///
/// 一个像素都不硬编码：落点由**这一帧记下的那张表**（`fixed_text_boxes`）里当前页的
/// 那一格与那一页自己的字层折回来，与 `selection::page_units` 互为逆运算。换窗口宽、
/// 换 `RENDER_EDGE` 那一档、换一页的尺寸，这条式子都不用改 —— 而它同时也是「绘制摆的
/// 那一格」与「命中折回去的那一格」是同一格的证据：两处各算一把尺的话，这一条会红在
/// 色块与手指错开上。
fn pdf_char(
    view: &Entity<ReaderSession>,
    cx: &TestAppContext,
    index: usize,
    along: f32,
) -> Point<Pixels> {
    view.read_with(cx, |v, _| {
        let current = v.page();
        let boxes = v.fixed_text_boxes.borrow();
        let (page, box_) = boxes
            .iter()
            .copied()
            .find(|(page, _)| *page == current)
            .or_else(|| boxes.first().copied())
            .expect("那一格该被量出来了");
        let text = v
            .fixed()
            .and_then(|book| book.page_text(page))
            .expect("这一页该有文字层");
        let item = text.chars[index];
        let origin = v.content_bounds.get().origin;
        // 一单位折几像素：与 `page_boxes` 同一把倒数。
        let sx = f32::from(box_.size.width) / text.width;
        let sy = f32::from(box_.size.height) / text.height;
        point(
            px(f32::from(origin.x)
                + f32::from(box_.origin.x)
                + (item.rect.x + item.rect.width * along) * sx),
            px(f32::from(origin.y)
                + f32::from(box_.origin.y)
                + (item.rect.y + item.rect.height * 0.5) * sy),
        )
    })
}

/// 开一本真 PDF：入库 → 带容器打开 → 走 `open_book` 那条生产路（只有它按 `is_fixed()`
/// 分流到定版式那一支 —— `with_document` 那条构造路不分族，用它测定版式就是测可重排）。
fn open_fixed_pdf(
    cx: &mut TestAppContext,
    name: &str,
) -> (Entity<ReaderSession>, AnyWindowHandle, Arc<Store>, i64) {
    open_fixed_pdf_with_turn(cx, name, PageTurn::Slide)
}

/// 同上，但「翻页方式」在 `open_book` 之前就摆好。
///
/// 连续竖滚必须这么开：摆法在渲染帧里由 `page_turn` 解出来，先开成整页翻再改配置
/// 也能到，但这里要测的正是「一打开就是连续」那条生产路。
fn open_fixed_pdf_with_turn(
    cx: &mut TestAppContext,
    name: &str,
    turn: PageTurn,
) -> (Entity<ReaderSession>, AnyWindowHandle, Arc<Store>, i64) {
    open_fixed_pdf_at(cx, name, 460.0, turn)
}

/// 同上，但窗口宽由调用方给：定版式双页要 1200 宽那一档才装得下两页。
fn open_fixed_pdf_at(
    cx: &mut TestAppContext,
    name: &str,
    width: f32,
    turn: PageTurn,
) -> (Entity<ReaderSession>, AnyWindowHandle, Arc<Store>, i64) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
    let (meta, content) = folio_core::content::format::provider_for(&path)
        .expect("PDF 该有 provider")
        .index(&path)
        .expect("入库的那份真样本该读得开");
    let store = Arc::new(Store::in_memory().expect("内存库"));
    let id = store.add_book(&path, &meta).expect("入库");
    store
        .save_content(id, &content, Format::Pdf)
        .expect("页表入库");
    let document = folio_core::content::document::with_container(meta, content, &path, None);
    let (view, handle) = open_with_document(
        cx,
        Arc::from(document),
        prefs(16.0),
        Locator::start(),
        width,
        Some(store.clone()),
        Some(id),
    );
    cx.update(|app| {
        ConfigHolder::edit_appearance(app, |appearance| appearance.page_turn = turn);
    });
    view.update(cx, |v, cx| v.open_book(id, cx));
    settle(cx, handle);
    (view, handle, store, id)
}

/// 从同一份库、同一本书再开一屏：`open_book` 读回的是上一次落的位置。
fn reopen_fixed_pdf(
    cx: &mut TestAppContext,
    name: &str,
    store: Arc<Store>,
    id: i64,
    turn: PageTurn,
) -> (Entity<ReaderSession>, AnyWindowHandle) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
    let (meta, content) = folio_core::content::format::provider_for(&path)
        .expect("PDF 该有 provider")
        .index(&path)
        .expect("入库的那份真样本该读得开");
    let document = folio_core::content::document::with_container(meta, content, &path, None);
    let locator = store
        .position(id)
        .ok()
        .flatten()
        .map(|(_, locator)| locator)
        .unwrap_or_default();
    let (view, handle) = open_with_document(
        cx,
        Arc::from(document),
        prefs(16.0),
        locator,
        460.0,
        Some(store),
        Some(id),
    );
    cx.update(|app| {
        ConfigHolder::edit_appearance(app, |appearance| appearance.page_turn = turn);
    });
    view.update(cx, |v, cx| v.open_book(id, cx));
    settle(cx, handle);
    (view, handle)
}

/// 定版式的书签：点目录里的一条，屏上真的换成那一页，位置也当场落库。
///
/// 用入库的那本 PDF（`tests/fixtures/oss/pdf/three-pages.pdf`）而不是 `from_content` 造的
/// 合成文档：这一条要测的是「书签的落点是**页号**」这一整路 —— 合成那一份没有容器，
/// 取像素永远回空，界面就算跳对了也证明不了读者看得见。这里走的是生产上那条路：
/// provider 读索引 → 落库 → `with_container` 打开（正文与像素都回原文件现取）。
#[gpui_kit::test]
fn a_bookmark_in_a_pdf_moves_the_screen_to_that_page(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/oss/pdf/three-pages.pdf");
    let (meta, content) = folio_core::content::format::provider_for(&path)
        .expect("PDF 该有 provider")
        .index(&path)
        .expect("入库的那本夹具该读得开");
    let store = Arc::new(Store::in_memory().expect("内存库"));
    let id = store.add_book(&path, &meta).expect("入库");
    store
        .save_content(id, &content, folio_core::content::format::Format::Pdf)
        .expect("页表入库");

    let document = folio_core::content::document::with_container(meta, content, &path, None);
    let (view, handle) = open_with_document(
        cx,
        Arc::from(document),
        prefs(16.0),
        Locator::start(),
        460.0,
        Some(store.clone()),
        Some(id),
    );
    // `with_document` 那一条构造路不分族（它只吃一份现成的文档），而读者真的翻开一本
    // 书走的是 `open_book`：从库读回内容 → 按 `doc.is_fixed()` 分流。这里按生产那条路
    // 再走一遍，测的才是读者撞到的那一段。
    view.update(cx, |v, cx| v.open_book(id, cx));
    settle(cx, handle);
    assert_eq!(view.read_with(cx, |v, _| v.page_count()), 3);
    assert!(
        view.read_with(cx, |v, _| v.is_fixed()),
        "这一本该走定版式那一条"
    );

    // 目录那一屏的行：标题 + 点下去去哪儿。`chapter` 在定版式这一支装的是**页号**
    // （`Locator.chapter` 在这一族本来就是页码，`toc_tree::build` 认 `TocEntry::page` 优先）。
    let rows = view.read_with(cx, |v, _| {
        v.toc_rows()
            .iter()
            .map(|row| (row.title.clone(), row.chapter))
            .collect::<Vec<_>>()
    });
    assert_eq!(
        rows,
        vec![
            ("第一章".to_string(), Some(0)),
            ("第二章".to_string(), Some(1))
        ],
        "书签该上屏，落点是页号"
    );

    view.update(cx, |v, cx| v.goto_entry(1, None, cx));
    settle(cx, handle);
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        1,
        "点「第二章」该落在第 2 页"
    );
    let (_, locator) = store.position(id).expect("读回进度").expect("进度应存在");
    assert_eq!(
        locator,
        Locator {
            chapter: 1,
            offset: 0
        },
        "页号就是定版式的位置：跳转当场落库，杀进程回来还在这一页"
    );
}

/// 定版式的下一页在读者翻过去之前就解好：PDF 的一页要回引擎栅格化，从前那一笔
/// 要等翻页那一下才付。这里用三页 PDF，停在第一页，断言第二页的键已经在图集里。
///
/// 这一条不直调 `ImageAtlas::prepare` —— 它测的正是「定版式那一支有没有把下一页
/// 递过去」。载荷是后台线程，所以等一小会儿；超过两秒还没好就是没接上。
#[gpui_kit::test]
fn the_next_pdf_page_is_decoded_before_the_reader_turns(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle, _, _) = open_fixed_pdf(cx, "../../tests/fixtures/oss/pdf/three-pages.pdf");
    let next_key = view.read_with(cx, |v, _| {
        let book = v.fixed().expect("这一本该走定版式");
        assert_eq!(book.page(), 0, "前置：读者停在第 1 页");
        // 预取买的是**预览**那一档（矢量页 720 长边），不是高清：高清翻过去再买，
        // 否则下一趟翻页的首帧又要等一整趟 2048 栅格化（`prepare_tile` 那段注释）。
        book.page_tiles(1)
            .into_iter()
            .find_map(|tile| tile.preview)
            .map(|request| request.key)
            .expect("第 2 页该有一条预览活儿")
    });
    let mut ready = false;
    for _ in 0..100 {
        settle(cx, handle);
        ready = view.read_with(cx, |v, cx| v.atlas.read(cx).peek(&next_key).is_some());
        if ready {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        ready,
        "第 2 页的预览该在翻过去之前就解好在图集里（键 {next_key}）"
    );
}

/// 定版式的「上下滚动」：页按版心宽纵向排开，滚轮挪的是栈上的像素，视口顶那一页
/// 跟着改写页号 —— 页脚、进度、落库都读这一个数。
#[gpui_kit::test]
fn a_fixed_book_scrolls_continuously_and_tracks_the_page_under_the_viewport(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_kit::init);
    let (view, handle, _, _) = open_fixed_pdf_with_turn(
        cx,
        "../../tests/fixtures/oss/pdf/three-pages.pdf",
        PageTurn::Scroll,
    );
    let top1 = view.read_with(cx, |v, _| {
        assert!(v.is_fixed(), "前置：这一本该走定版式");
        assert_eq!(
            v.mode(),
            PageMode::Continuous,
            "「上下滚动」要把定版式带进连续竖滚"
        );
        assert_eq!(v.page_count(), 3);
        assert_eq!(v.page(), 0);
        assert!(v.scroll_y.abs() < 0.5, "一打开在页栈顶上");
        assert!(
            v.content_height > v.laid_out.viewport,
            "三页排成的栈要比一屏高：{} vs {}",
            v.content_height,
            v.laid_out.viewport
        );
        let stack = v.fixed().expect("定版式").stack().expect("连续档该建页栈");
        assert_eq!(stack.len(), 3);
        let top = stack.top_of(1).expect("第二页该在栈上");
        assert!(
            top > v.laid_out.viewport,
            "第二页的页首该在第一屏之外：{top}"
        );
        top
    });

    // 真滚轮事件走到第二页页首（不走内部赋值，测的就是事件那条路）。
    scroll_down(cx, handle, top1 + 1.0);
    settle(cx, handle);
    view.read_with(cx, |v, _| {
        assert!(
            v.scroll_y >= top1,
            "滚过第二页页首之后坐标该在它附近：{} vs {top1}",
            v.scroll_y
        );
        assert_eq!(v.page(), 1, "视口顶那一页要跟着滚动改写");
        assert!(
            v.progress().is_some_and(|progress| progress > 0.0),
            "页号进了第二页，全书进度不该还停在 0"
        );
    });
}

/// 定版式的一页在**版心**里居中，而且页与页之间不留缝。
///
/// 从前 `fixed_page_tiles_at` 从窗口 x=0 起算，版心左右各 24 的页边距只有左边那一份
/// 真的用上：整条行贴左，右边攒着两倍页边距。PDF 因此右边永远黑一条，读者报的就是这个。
#[test]
fn a_fixed_page_row_is_centred_inside_the_reader_margin() {
    use crate::ui::features::reader::model::fixed_book::PageTile;
    use crate::ui::features::reader::view::body::fixed::fixed_page_tiles_at;
    use crate::ui::foundation::images::no_images;

    let tile = |width: u32, height: u32| PageTile {
        request: None,
        preview: None,
        width,
        height,
        href: String::new(),
        mime: String::new(),
        export: no_images(),
        renders: false,
        vector: false,
    };

    // 版心 300、左边距 24 ⇒ 窗口宽 348。400×600 的一页按高 300 摆成 200 宽：
    // 行首该在 24 + (300-200)/2 = 74，行尾 274，右边同样空 74。
    let (tiles, _) = fixed_page_tiles_at(vec![tile(400, 600)], 300.0, 300.0, 0.0, 24.0, 0.5, false);
    assert_eq!(tiles.len(), 1);
    let left = tiles[0].x;
    let right = 348.0 - (left + tiles[0].width);
    assert!(
        (left - right).abs() < 0.5,
        "左右页边距该相等：left={left} right={right} width={}",
        tiles[0].width
    );
    assert!(left >= 24.0, "不许钻到左边距里面：{left}");

    // 铺满版心的那一档从头就贴着左边距，不是贴着窗口。
    let (tiles, _) = fixed_page_tiles_at(vec![tile(300, 300)], 300.0, 300.0, 0.0, 24.0, 1.0, false);
    assert!(
        (tiles[0].x - 24.0).abs() < 0.5,
        "铺满时该从左边距起：{}",
        tiles[0].x
    );
    assert!((tiles[0].width - 300.0).abs() < 0.5);
}

/// 连续档里两页之间不留缝：第二页的页首就是第一页的页底。
#[gpui_kit::test]
fn fixed_pages_touch_in_continuous_mode(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, _handle, _, _) = open_fixed_pdf_with_turn(
        cx,
        "../../tests/fixtures/oss/pdf/three-pages.pdf",
        PageTurn::Scroll,
    );
    let gap = view.read_with(cx, |v, _| {
        let stack = v.fixed().expect("定版式").stack().expect("连续档该建页栈");
        stack.top_of(1).expect("第二页") - stack.height_of(0).expect("第一页高")
    });
    assert!(gap.abs() < 0.5, "页与页之间不留缝，实得 {gap}");
}

/// 连续竖滚里点一下 / 按方向键仍然是「滚一屏 90%」，而且走的是与流式连续同一套补间：
/// 权威位置当场到终点，画出来的位置从起点追过去。
#[gpui_kit::test]
fn a_fixed_tap_turn_tweens_the_stack(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle, _, _) = open_fixed_pdf_with_turn(
        cx,
        "../../tests/fixtures/oss/pdf/three-pages.pdf",
        PageTurn::Scroll,
    );
    let start = view.read_with(cx, |v, _| v.scroll_y);
    view.update(cx, |v, cx| v.scroll_page(1, cx));
    let (target, turn) = view.read_with(cx, |v, _| (v.scroll_y, v.turn));
    assert!(target > start, "点一下要往前走：{start} → {target}");
    let turn = turn.expect("离散翻页该起一趟补间");
    assert_eq!(turn.style, PageTurn::Scroll, "这一档就是上下滚动");
    assert_eq!(
        turn.scroll,
        Some((start, target)),
        "补间的两端是画出来的起点与权威终点"
    );

    turn_frame(cx, handle, Duration::from_millis(1));
    let painted = view.read_with(cx, |v, _| v.scroll_painted);
    assert!(
        (painted - start).abs() < 1.0,
        "起步该还画在起点上：{painted} vs {start}"
    );
    turn_frame(cx, handle, Duration::from_millis(140));
    let half = view.read_with(cx, |v, _| v.scroll_painted);
    assert!(
        half > start && half < target,
        "走到一半该落在两点之间：{start} < {half} < {target}"
    );
    turn_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));
    let (painted, settled) = view.read_with(cx, |v, _| (v.scroll_painted, v.turn));
    assert!(settled.is_none(), "走完要把补间收掉");
    assert!(
        (painted - target).abs() < 1.0,
        "定住后画的位置就是权威位置：{painted} vs {target}"
    );
}

/// 从连续竖滚换回整页翻：页号与读到的那一页不丢，滚动坐标归零。
#[gpui_kit::test]
fn a_fixed_book_keeps_its_page_when_scroll_mode_is_turned_off(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle, _, _) = open_fixed_pdf_with_turn(
        cx,
        "../../tests/fixtures/oss/pdf/three-pages.pdf",
        PageTurn::Scroll,
    );
    let top1 = view.read_with(cx, |v, _| {
        v.fixed().expect("定版式").stack().expect("页栈").top_of(1)
    });
    let top1 = top1.expect("第二页该在栈上");
    scroll_down(cx, handle, top1 + 1.0);
    settle(cx, handle);
    assert_eq!(view.read_with(cx, |v, _| v.page()), 1);

    cx.update(|app| {
        ConfigHolder::edit_appearance(app, |appearance| appearance.page_turn = PageTurn::Slide);
    });
    settle(cx, handle);
    view.read_with(cx, |v, _| {
        assert_eq!(v.mode(), PageMode::Paged, "选回纸面动画就回整页翻");
        assert_eq!(v.page(), 1, "换档不丢读到的那一页");
        assert!(v.scroll_y.abs() < 0.5, "整页翻没有滚动坐标：{}", v.scroll_y);
    });
}

/// 连续竖滚里页被滚上去一截之后，字还选得中：命中表记的是**这一帧摆出去的位置**
/// （已经减掉滚动量），不是页在栈上的静止坐标。
#[gpui_kit::test]
fn a_fixed_text_page_still_selects_after_the_stack_scrolls(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle, _, _) = open_fixed_pdf_with_turn(
        cx,
        "../../tests/fixtures/oss/pdf/wps-paper.pdf",
        PageTurn::Scroll,
    );
    scroll_down(cx, handle, 40.0);
    settle(cx, handle);
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        0,
        "滚一点点还停在第 1 页"
    );

    let from = pdf_char(&view, cx, 0, 0.25);
    let to = pdf_char(&view, cx, 4, 0.75);
    drag_select_between(cx, handle, from, to);
    assert_eq!(
        view.read_with(cx, |v, _| v.selected_text()),
        // 拖的是第 0 到第 4 个字，正好是 `TITLE` 五个字；落笔位置修好之后，
        // 字序里不再有那个补出来的空格（从前是 `TI TL`）。
        Some("TITLE".to_string()),
        "滚动之后的落点仍该按这一帧的页框折回字序"
    );
}

/// 连续竖滚里读到第三页再离开：重启之后还在第三页的页首。
///
/// 位置按页落库（定版式的 `locator.chapter` 就是页号）。库里存的那一档可能是空串
/// （摆法是渲染帧按「翻页方式」解出来的，不是读者在这本书上选过档），所以重开时
/// 还要靠 `page_turn = scroll` 把它重新带进连续 —— 这条路径正是最容易把位置丢在
/// 第一帧的那一种。
#[gpui_kit::test]
fn a_fixed_scroll_position_reopens_on_the_same_page(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle, store, id) = open_fixed_pdf_with_turn(
        cx,
        "../../tests/fixtures/oss/pdf/three-pages.pdf",
        PageTurn::Scroll,
    );
    view.update(cx, |v, cx| v.set_page(2, cx));
    settle(cx, handle);
    let bottom = view.read_with(cx, |v, _| {
        assert_eq!(v.page(), 2, "先站到第三页");
        let bottom = v.max_scroll();
        assert!(
            (v.scroll_y - bottom).abs() < 0.5,
            "最后一页比一屏矮，落页夹在全书末尾：{} vs {bottom}",
            v.scroll_y
        );
        assert_eq!(
            v.fixed()
                .expect("定版式")
                .page_at_scroll(v.scroll_y, v.laid_out.viewport),
            Some(2),
            "滚到书尾时页号归最后一页，哪怕它的页首到不了视口顶"
        );
        bottom
    });
    view.update(cx, |v, _| v.persist_position());
    assert_eq!(
        store.position(id).expect("读库").expect("该存过").1.chapter,
        2,
        "页号就是落库的位置"
    );

    let (again, _) = reopen_fixed_pdf(
        cx,
        "../../tests/fixtures/oss/pdf/three-pages.pdf",
        store,
        id,
        PageTurn::Scroll,
    );
    again.read_with(cx, |v, _| {
        assert_eq!(v.mode(), PageMode::Continuous, "重开仍是连续竖滚");
        assert_eq!(v.page(), 2, "位置还在第三页");
        assert!(
            (v.scroll_y - bottom).abs() < 0.5,
            "重开落回全书末尾：{} vs {bottom}",
            v.scroll_y
        );
    });
}

/// 定版式翻页与流式同一套纸面动画：全局选的是折页时，定版式的**位图页**也烘一张
/// 捕获纹理来卷 —— 从前那一档被筛掉、退到平移，理由是「没有可卷的排版纹理」。
/// 现在捕获链补上了，两个可选集也同源。
#[gpui_kit::test]
fn a_fixed_page_turn_takes_the_curl_style(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle, _, _) = open_fixed_pdf(cx, "../../tests/fixtures/oss/pdf/three-pages.pdf");
    cx.update(|app| {
        ConfigHolder::edit_appearance(app, |a| a.page_turn = PageTurn::Curl);
    });
    view.update(cx, |v, cx| v.scroll_page(1, cx));
    assert_eq!(view.read_with(cx, |v, _| v.page()), 1, "先翻到第 2 页");
    let style = view.read_with(cx, |v, _| v.turn.map(|turn| turn.style));
    assert_eq!(
        style,
        Some(PageTurn::Curl),
        "定版式与可重排一样走折页，而不是退到平移"
    );
    assert!(
        view.read_with(cx, |v, _| v.turn.is_some()),
        "这一趟该真的在走"
    );
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("定版式卷曲那一帧该画得出来");
}

/// 定版式起卷要两样东西：正面那张纹理里是**这一页的位图**，背面烘的是**目标页**。
/// 起卷那一帧只摆进场那一张纸 —— 旧页在纹理里，由 GPU 网格卷走；落定那一帧只剩
/// 当前那一页，不该出现两张同一页。
#[gpui_kit::test]
fn a_fixed_curl_bakes_the_page_bitmap_and_the_target_page_on_its_back(cx: &mut TestAppContext) {
    use crate::ui::features::reader::model::turn::TurnIntent;

    cx.update(gpui_kit::init);
    let (view, handle, _, _) = open_fixed_pdf_with_turn(
        cx,
        "../../tests/fixtures/oss/pdf/three-pages.pdf",
        PageTurn::Curl,
    );
    // 高清像素要真解进图集之后才允许烘（`fixed_capture_ready` 那一格）。
    let mut ready = false;
    for _ in 0..20 {
        settle(cx, handle);
        if view.read_with(cx, |v, _| v.curl_capture_ready()) {
            ready = true;
            break;
        }
    }
    assert!(ready, "前置：这一页每一格的像素都该落地，捕获门才开得了");
    assert_eq!(
        view.read_with(cx, |v, _| v.captured_page),
        Some((0, 0)),
        "前置：正面那张纹理记的是这一页（定版式的章号恒为 0）"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.captured_back),
        None,
        "前置：没起趟之前不该烘背面"
    );
    assert!(
        view.read_with(cx, |v, _| v.fixed_capture_images) >= 1,
        "正面捕获副本里该真有一张页图（不是一张纸色）"
    );

    view.update(cx, |v, cx| v.turn(TurnIntent::Next, cx));
    assert_eq!(
        view.read_with(cx, |v, _| v.pending_turn),
        None,
        "纹理在手，这一趟不该排队"
    );
    settle(cx, handle);
    assert_eq!(view.read_with(cx, |v, _| v.page()), 1, "这一趟真翻了页");
    assert_eq!(
        view.read_with(cx, |v, _| v.turn.map(|turn| turn.style)),
        Some(PageTurn::Curl)
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.captured_back.map(|(back, _)| back)),
        Some((0, 1, 0)),
        "单页折页的背面该是目标页整张"
    );
    assert!(
        view.read_with(cx, |v, _| v.fixed_capture_images) >= 1,
        "背面捕获副本里该真有目标页那张图"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.fixed_sheets_drawn),
        1,
        "起卷那一帧只摆进场那一张：旧页在捕获纹理里"
    );

    turn_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));
    assert!(
        view.read_with(cx, |v, _| v.turn.is_none()),
        "走完该收掉这一趟"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.fixed_sheets_drawn),
        1,
        "落定那一帧只剩当前这一页，不该出现两张同一页"
    );
}

/// 定版式也跟着手指走：页号在松手那一刻才落，跟手期间只摆两张纸。从前这一族在
/// `swipe_geometry` 里被整条挡掉，读者读到的是「划不动，松手才翻」。
#[gpui_kit::test]
fn a_fixed_swipe_follows_the_finger_and_lands_on_release(cx: &mut TestAppContext) {
    use crate::ui::features::reader::model::session::TurnRun;

    cx.update(gpui_kit::init);
    let (view, handle, _, _) = open_fixed_pdf(cx, "../../tests/fixtures/oss/pdf/three-pages.pdf");
    let first = view.read_with(cx, |v, _| v.page());
    raw_touch_at(cx, handle, TouchPhase::Started, point(px(400.0), px(200.0)));
    raw_touch_at(cx, handle, TouchPhase::Moved, point(px(250.0), px(200.0)));
    let following = view.read_with(cx, |v, _| {
        v.turn.map(|turn| (turn.run, turn.grip.is_some()))
    });
    assert!(
        matches!(following, Some((TurnRun::Following { t }, true)) if t > 0.0),
        "这一笔该跟手走起来（拿到的 {following:?}）"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        first,
        "跟手期间页号一个字都不动"
    );

    raw_touch_at(cx, handle, TouchPhase::Ended, point(px(250.0), px(200.0)));
    turn_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        first + 1,
        "拖过落定线，松手那一腿该落到下一页"
    );
}

/// 折页那一档也一样跟手：捕获门开着时，这一笔该认领成 `Following`，而不是被
/// 「旧页纹理还没烘好」挡到松手才补一趟。
#[gpui_kit::test]
fn a_fixed_curl_swipe_follows_once_the_texture_is_baked(cx: &mut TestAppContext) {
    use crate::ui::features::reader::model::session::TurnRun;

    cx.update(gpui_kit::init);
    let (view, handle, _, _) = open_fixed_pdf_with_turn(
        cx,
        "../../tests/fixtures/oss/pdf/three-pages.pdf",
        PageTurn::Curl,
    );
    let mut ready = false;
    for _ in 0..20 {
        settle(cx, handle);
        if view.read_with(cx, |v, _| v.curl_capture_ready()) {
            ready = true;
            break;
        }
    }
    assert!(ready, "前置：这一页的捕获纹理该烘好");

    let first = view.read_with(cx, |v, _| v.page());
    raw_touch_at(cx, handle, TouchPhase::Started, point(px(400.0), px(200.0)));
    raw_touch_at(cx, handle, TouchPhase::Moved, point(px(200.0), px(200.0)));
    let following = view.read_with(cx, |v, _| v.turn.map(|turn| (turn.run, turn.style)));
    assert!(
        matches!(following, Some((TurnRun::Following { t }, PageTurn::Curl)) if t > 0.0),
        "折页这一笔该跟手（拿到的 {following:?}）"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        first,
        "跟手期间页号不动"
    );
}

/// 平摊三档也采那张捕获纹理：起滑那一帧旧页由网格画，不再实时摆一遍位图。
#[gpui_kit::test]
fn a_fixed_slide_turn_draws_the_baked_page_instead_of_the_bitmap(cx: &mut TestAppContext) {
    use gpui_kit::ElementId;
    use gpui_kit::test::TestWindowExt;

    cx.update(gpui_kit::init);
    let (view, handle, _, _) = open_fixed_pdf(cx, "../../tests/fixtures/oss/pdf/three-pages.pdf");
    let mut ready = false;
    for _ in 0..20 {
        settle(cx, handle);
        if view.read_with(cx, |v, _| v.curl_capture_ready()) {
            ready = true;
            break;
        }
    }
    assert!(ready, "前置：这一页的捕获纹理该烘好");

    view.update(cx, |v, cx| v.scroll_page(1, cx));
    turn_frame(cx, handle, Duration::from_millis(1));
    let flat = cx
        .update_window(handle, |_, window, _| {
            window
                .try_find(ElementId::from("reader-flat-capture"))
                .is_some()
        })
        .expect("窗口应能渲染");
    assert!(flat, "出场页该走烘好的整页纹理");
    assert_eq!(
        view.read_with(cx, |v, _| v.fixed_sheets_drawn),
        1,
        "旧页在纹理里，主场景只摆进场那一张"
    );
}

/// 定版式双页 = **相邻两页各占半张**（合订本摊开），不是把一页排成两栏：
/// 一屏摆两张纸，翻页步进 2。
#[gpui_kit::test]
fn a_fixed_spread_shows_two_adjacent_pages_and_steps_two(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle, _, _) = open_fixed_pdf_at(
        cx,
        "../../tests/fixtures/oss/pdf/three-pages.pdf",
        1200.0,
        PageTurn::Slide,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Spread, cx));
    settle(cx, handle);
    assert_eq!(
        view.read_with(cx, |v, _| v.mode()),
        PageMode::Spread,
        "1200 宽的窗装得下两页"
    );
    assert_eq!(
        view.read_with(cx, |v, _| (v.page(), v.fixed_sheets_drawn)),
        (0, 2),
        "一屏相邻两页（0/1），两张纸都得真摆上"
    );

    view.update(cx, |v, cx| v.scroll_page(1, cx));
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        2,
        "双页一步跨两张相邻页（0/1 → 2/3）"
    );
    settle(cx, handle);
    assert_eq!(
        view.read_with(cx, |v, _| v.fixed_sheets_drawn),
        1,
        "只剩尾页时那一屏只摆一张（第 3 页），另一半留空"
    );
}

/// 拖过 `TITLE` 那五个字：选区成立、复制到手的是那一串、划下去真进了库，
/// 而翻回这一屏时那条痕迹**点得回去**（留在书上的东西不只是读数）。
///
/// 样本是 `tests/fixtures/oss/pdf/wps-paper.pdf` —— WPS（Kingsoft Office）生产的两页
/// 论文模板，字体是 `Identity-H` 子集 + `ToUnicode`。**不用仓库里那本 `three-pages.pdf`**：
/// 它的内容流只有一个灰矩形，一个字都没有，拿它测「选中」就是让一排断言空跑而全绿
/// （那本正好留给下面那条「没有字的一页」的用例）。
#[gpui_kit::test]
fn a_pdf_page_selects_copies_marks_and_points_back(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle, store, id) =
        open_fixed_pdf(cx, "../../tests/fixtures/oss/pdf/wps-paper.pdf");
    assert!(
        view.read_with(cx, |v, _| v.is_fixed()),
        "前置：这一本该走定版式那一条"
    );

    // 起手压在第一个字的左半边（给插入点 0），收尾压在第五个字的右半边（给 5）。
    let from = pdf_char(&view, cx, 0, 0.25);
    let to = pdf_char(&view, cx, 4, 0.75);
    drag_select_between(cx, handle, from, to);
    let selected = view
        .read_with(cx, |v, _| v.selection())
        .expect("拖过阈值该成立选区");
    assert_eq!(
        (selected.chapter, selected.range),
        (0, 0..5),
        "定版式里 `chapter` 就是页号，偏移是那一页字序里的第几个字"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.selected_text()),
        // 拖过的那五个字就是 `TITLE`；落笔位置修好后字序里不再有假空格。
        Some("TITLE".to_string()),
        "复制那一格取的该是选中的那几个字"
    );

    // 划线：落库的是那一条锚点，而选区收掉（屏上那条从此来自库里）。
    view.update(cx, |v, cx| v.add_highlight(cx));
    let rows = store.annotations(id).expect("读回批注");
    assert_eq!(rows.len(), 1, "划下去该有一条批注");
    let anchor = &rows[0].anchor;
    assert_eq!((anchor.chapter, anchor.start, anchor.end), (0, 0, 5));
    // 快照存的就是字序那一串（与 `selected_text` 同一份，「翻回原文」那一步比的也是
    // 这一串）。
    assert_eq!(anchor.exact, "TITLE", "快照存的是字序那一串");
    assert!(
        anchor.path.is_empty(),
        "定版式没有 DOM 路径可指：{}",
        anchor.path
    );
    assert!(
        view.read_with(cx, |v, _| v.selection().is_none()),
        "落库之后选区该收掉"
    );

    // 点回那一条：压在中间那个字上（痕迹画出来的那一条之内）。
    //
    // 先把拖选松手攒下的那一下豁免作废 —— 它是上一手的尾巴，而 `tap_on_body` 的
    // **第一条**判据就是它：不先吃掉，下面这一击会被它拦下来，测到的还是第一条。
    // 可重排那几条用例有同一道前置（见上面 `tap_on_body` 那组注释）。
    view.update(cx, |v, _| v.consume_tap_suppression());
    let on_mark = pdf_char(&view, cx, 1, 0.5);
    let picked = cx
        .update_window(handle, |_, window, cx| {
            view.update(cx, |v, cx| v.tap_on_body(window, cx, on_mark))
        })
        .expect("点击应送达");
    assert!(
        picked,
        "点在画出来的那条痕迹上该把那一条挑回来，而不是去翻页"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.selection().map(|s| s.range)),
        Some(0..5)
    );
    assert!(
        view.read_with(cx, |v, _| v.selection_annotation().is_some()),
        "挑回来的正是库里那一条 → 菜单那一格该回到「取消划线」"
    );

    // 桌面上「单击这一页」本来是看大图（宿主测试没装平台服务，`supports_long_press`
    // 给 false ⇒ 那一族的入口挂在单击上）。接上选中之后这一族的入口不许悄悄没掉：
    // 没有选区时那一击仍然摊得开。
    //
    // 只能钉这半边：`window.click(那个 id)` 点的是元素**正中**，而页心那一点落在留白上
    // （`TITLE` 在页顶），所以「已划的线上的一击归选中」那一条先后没法用 id 投出来 ——
    // 它由上面那次直接问 `tap_on_body` 钉住（同一个入口函数，只是落点得由测试自己定），
    // 屏幕上的分别要真机点一下才算看完。
    let viewer = cx.update(|cx| {
        cx.new(|_| crate::ui::features::reader::components::image_viewer::ImageViewer::default())
    });
    cx.update(|cx| view.update(cx, |v, _| v.set_image_viewer(viewer.clone())));
    settle(cx, handle);
    view.update(cx, |v, _| v.clear_selection());
    cx.update_window(handle, |_, window, cx| {
        window.click(("reader-image", 0usize), cx);
    })
    .expect("那一击该送达");
    assert!(
        viewer.read_with(cx, |v, _| v.is_open()),
        "没有选区时单击仍该把这一页摊开看"
    );
    assert!(
        view.read_with(cx, |v, _| v.selection().is_none()),
        "开大图这一趟不该顺手成立一个选区"
    );
    viewer.update(cx, |v, _| v.close());

    // 翻到第二页：一屏就是一页，挂在旧页的选区没有字可锚，跟着清。
    view.update(cx, |v, cx| v.set_page(1, cx));
    settle(cx, handle);
    assert!(
        view.read_with(cx, |v, _| v.selection().is_none()),
        "翻走即清：留着就是一份屏上看不见、菜单还开着的选中"
    );
    // 第二页也有文字层（一本真书不会只有首页读得出字），那一格该重新量出来。
    assert!(
        view.read_with(cx, |v, _| !v.fixed_text_boxes.borrow().is_empty()),
        "换页之后那一格要重新记上，否则新一页选不中"
    );
    let second = pdf_char(&view, cx, 0, 0.25);
    let second_to = pdf_char(&view, cx, 3, 0.75);
    drag_select_between(cx, handle, second, second_to);
    assert!(
        view.read_with(cx, |v, _| v.selection().is_some()),
        "第二页照样选得中"
    );
}

/// 没有文字层的那一页：什么都选不中，也不 panic，页的摆位与从前逐字节相同。
///
/// 用 `three-pages.pdf` —— 它的内容流只有一个灰矩形，**一个字都没有**，正是扫描件与
/// 漫画那一档的形状。这一条与上一条配成一对：缺了它，「这一页没字」与「选中坏了」
/// 在屏上是同一个表现，而测试分不出两者。
#[gpui_kit::test]
fn a_pdf_page_without_a_text_layer_selects_nothing(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle, _, _) = open_fixed_pdf(cx, "../../tests/fixtures/oss/pdf/three-pages.pdf");
    assert!(
        view.read_with(cx, |v, _| v.is_fixed()),
        "前置：这一本该走定版式那一条"
    );
    assert_eq!(view.read_with(cx, |v, _| v.page_count()), 3, "三页都该在");

    // 整页中央拖一把：从前的表现就是「什么都不发生」，这一条不许变。
    drag_select_between(
        cx,
        handle,
        point(px(180.0), px(240.0)),
        point(px(300.0), px(240.0)),
    );
    assert!(
        view.read_with(cx, |v, _| v.selection().is_none()),
        "没有字的一页不该成立选区"
    );
    assert!(
        view.read_with(cx, |v, _| v.fixed_text_boxes.borrow().is_empty()),
        "没有文字层的那一页不该记下那一格"
    );
}

/// 底栏那颗「放大整页」用的身份：**当前在读的那一页**在整本书那一列里的第几条。
///
/// 这一格值得单独钉：按钮不在正文的绘制帧里，手上没有那张图的键，只能回会话问
/// （`FixedBook::current_page_key`）。两处各数一遍「这一页是第几张」的话，翻过页之后
/// 按下去就会开成上一页 —— 而那一层看起来一切正常，只是摆的不是这一页。
///
/// 顺带钉住那条「按图集的键开得动那一层」的共用路：正文里点在图上那一次与这一格
/// 走的是同一个 `open_picture`，所以它红的时候两处一起红。
#[gpui_kit::test]
fn the_zoom_entry_follows_the_page_being_read(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle, _, _) = open_fixed_pdf(cx, "../../tests/fixtures/oss/pdf/wps-paper.pdf");
    let viewer = cx.update(|cx| {
        cx.new(|_| crate::ui::features::reader::components::image_viewer::ImageViewer::default())
    });
    cx.update(|cx| view.update(cx, |v, _| v.set_image_viewer(viewer.clone())));

    let key = view
        .read_with(cx, |v, _| {
            v.fixed().and_then(|book| book.current_page_key())
        })
        .expect("定版式的一页该在整本书那一列里有身份");
    assert_eq!(
        view.read_with(cx, |v, _| v.image_index_of_key(&key)),
        Some(0),
        "第一页该是那一列里的第一条"
    );

    cx.update_window(handle, |_, window, cx| {
        crate::ui::features::reader::view::body::images::open_picture(
            &viewer,
            &view.downgrade(),
            &key,
            window,
            cx,
        );
    })
    .expect("那一格该送达");
    assert!(
        viewer.read_with(cx, |v, _| v.is_open()),
        "底栏那一格该把这一页交给放大那一层"
    );

    // 翻到第二页：身份跟着换到第二条，而按钮按这一刻问的就是这一个。
    view.update(cx, |v, cx| v.set_page(1, cx));
    settle(cx, handle);
    let second = view
        .read_with(cx, |v, _| {
            v.fixed().and_then(|book| book.current_page_key())
        })
        .expect("第二页也该有身份");
    assert_ne!(second, key, "身份跟着在读的那一页走");
    assert_eq!(
        view.read_with(cx, |v, _| v.image_index_of_key(&second)),
        Some(1)
    );

    // 一页空的定版式书没有身份可取（空页表）：那一格按下去什么都不做，而不是开一层
    // 「0 / 0」。这一条与上面那条成对 —— 缺了它，「取不到」与「取错了」在屏上分不出。
    let empty = {
        let meta = folio_core::DocMeta {
            title: "空".into(),
            authors: Vec::new(),
            format: Format::Cbz,
            size: 0,
            ..folio_core::DocMeta::default()
        };
        let doc: Arc<dyn Document> = Arc::from(folio_core::content::document::from_content(
            meta,
            Content::Fixed(folio_core::content::format::FixedDoc::default()),
        ));
        let (book, _) =
            open_with_document(cx, doc, prefs(16.0), Locator::start(), 460.0, None, None);
        book.read_with(cx, |v, _| v.fixed().and_then(|b| b.current_page_key()))
    };
    assert_eq!(empty, None, "空页表取不出身份，那一格就不该开那一层");
}

/// 定版式那一本搜得动，而点一处命中跳的是**那一页**，落库的那条位置仍然只有页号。
///
/// 走的是真那一条链：会话交出打开着的文档（`searchable()`）→ core 那一趟按页扫 →
/// 命中跳过去走 `goto_offset`。三段各钉一件事：
///
/// - 交不出文档时那一屏只会摆一句「还没写词」—— 这一族接通之前 `searchable()` 对
///   定版式回的就是 `None`，于是那条空态永远挂着，没人发现搜索是断的。
/// - 命中归它自己那一页（不是首页，也不是「第几章」）。
/// - 跳过去之后屏上换了页，而库里那条 `Locator` 的偏移是 0：页内那几个字进了库，
///   进度条与「上次阅读的位置」就各读到一个解释不了的数（`resolve_position` 那条口径）。
#[gpui_kit::test]
fn a_pdf_book_searches_and_a_hit_jumps_to_its_page(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle, store, id) =
        open_fixed_pdf(cx, "../../tests/fixtures/oss/pdf/wps-paper.pdf");
    // 那一趟扫描本身的门在 core（`search::tests`，那里造得出 `StageCtx`）；这一侧只钉
    // 界面这一环：**交得出整本的字**，而且命中那一页上确实有那几个字。
    let (texts, pages) = view.read_with(cx, |v, _| {
        let doc = v.searchable().expect("定版式也该交得出那份文档");
        let pages = doc.fixed().expect("这一本该是定版式").pages.len();
        (doc.page_texts().expect("带字的 PDF 该给得出整本"), pages)
    });
    assert_eq!(texts.len(), pages, "整本那一列的长度就是页数");
    // **别拿 `contains` 问这一列**：定版式的字序里有按几何补出来的空格（`T`→`I` 那
    // 3.29 pt 被判成词缝那一类，整页对 poppler 的账在 `doc/优化计划.md` CT-11），
    // 所以「那一页写着 COPYRIGHT」不等于「字符串里连着写」。这里问的是**真那一路匹配器**：
    // 它对空白不敏感（`search::each_hit`），而跳过去要的正是它报出来的那个偏移。
    let query = folio_core::search::Query::parse("COPYRIGHT").expect("有词");
    let mut found: Vec<folio_core::search::Hit> = Vec::new();
    folio_core::search::scan_chapter(1, &texts[1], &query, &Default::default(), &mut found);
    let hit = found
        .first()
        .expect("第二页该扫出 COPYRIGHT（扫描扫的就是这一列）")
        .clone();
    assert_eq!(hit.chapter, 1, "命中该归第二页");

    // 那一行上面那句「这是哪儿的字」：定版式没有章表，念章名会念出一个空摆位。
    let place = view.read_with(cx, |v, cx| v.hit_place_at(hit.chapter, cx));
    assert!(
        !place.is_empty() && place.contains('2'),
        "那一行要说得出是第几页，而不是一个空摆位：{place:?}"
    );
    let _ = &hit.end;

    view.update(cx, |v, cx| v.goto_offset(hit.chapter, hit.start, cx));
    settle(cx, handle);
    assert_eq!(view.read_with(cx, |v, _| v.page()), 1, "跳过去该落在第二页");
    let (_, locator) = store.position(id).expect("读回进度").expect("进度该在");
    assert_eq!(
        locator,
        folio_core::Locator {
            chapter: 1,
            offset: 0
        },
        "页号就是定版式的位置：命中的页内偏移不许进库"
    );
}

// ---- 双指缩放：整页翻与上下滚动两档，PDF 与漫画同一套 ----
//
// 漫画（CBZ / 图片页）与 PDF 走的是**同一条**定版式绘制臂，只是前者没有字层；
// 缩放映射在字层之外，所以这几条用 PDF 夹具钉住几何，漫画那一族由「共用同一条臂」
// 保证 —— 需要单独看漫画的时候，把 `open_fixed_pdf` 换成 CBZ 的打开方式即可。

/// 屏上那一页的 `(x, 宽, y, 高)`，取自**命中那一把尺**（`fixed_text_boxes`）：
/// 它与摆出去的那张图是同一份矩形（同一帧、同一个映射），而且是带字的 PDF 页
/// 唯一那份可以直接问出来的账。找不到给一串 `-1`。
fn page_rect(cx: &TestAppContext, view: &Entity<ReaderSession>) -> (f32, f32, f32, f32) {
    view.read_with(cx, |v, _| {
        v.fixed_text_boxes
            .borrow()
            .first()
            .map(|(_, at)| {
                (
                    f32::from(at.origin.x),
                    f32::from(at.size.width),
                    f32::from(at.origin.y),
                    f32::from(at.size.height),
                )
            })
            .unwrap_or((-1.0, -1.0, -1.0, -1.0))
    })
}

/// 一记捏合（`PinchEvent` 的相位固定 `Moved`，与真机逐帧派发的那一截一致）。
fn pinch(cx: &mut TestAppContext, handle: AnyWindowHandle, delta: f32, at: Point<Pixels>) {
    dispatch(
        cx,
        handle,
        PinchEvent {
            position: at,
            delta,
            phase: TouchPhase::Moved,
            ..Default::default()
        }
        .to_platform_input(),
    );
}

/// 双指捏合把一整页放大：图真的变大、倍率写回会话，且夹取把纸留在视口里。
#[gpui_kit::test]
fn pinching_scales_the_fixed_page(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle, _, _) = open_fixed_pdf(cx, "../../tests/fixtures/oss/pdf/wps-paper.pdf");
    let (x0, w0, _, h0) = page_rect(cx, &view);
    assert!(w0 > 50.0 && h0 > 50.0, "前置：这一页该画出来了 {w0}×{h0}");

    // 手指落在页心。
    pinch(cx, handle, 1.0, point(px(x0 + w0 / 2.0), px(260.0)));
    settle(cx, handle);

    let (x1, w1, _, h1) = page_rect(cx, &view);
    let scale = view.read_with(cx, |v, _| v.fixed_zoom.scale());
    assert!((scale - 2.0).abs() < 1e-2, "放大一倍该落到 2：{scale}");
    assert!(
        w1 > w0 * 1.5 && h1 > h0 * 1.5,
        "图该跟着放大：{w0}×{h0} → {w1}×{h1}"
    );
    assert!(
        (x1 + w1 / 2.0 - (x0 + w0 / 2.0)).abs() < 2.0,
        "手指在页心：放大之后这一页仍该居中"
    );
}

/// 放大着的时候一根手指拖的是纸，不是页：图跟着手走，页号一动不动。
#[gpui_kit::test]
fn dragging_pans_the_zoomed_page_without_turning_it(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle, _, _) = open_fixed_pdf(cx, "../../tests/fixtures/oss/pdf/wps-paper.pdf");
    let (x0, w0, _, _) = page_rect(cx, &view);
    pinch(cx, handle, 1.5, point(px(x0 + w0 / 2.0), px(260.0)));
    settle(cx, handle);
    let (before_x, bwx, before_y, _) = page_rect(cx, &view);
    let page_before = view.read_with(cx, |v, _| v.page());
    assert!(bwx > w0, "前置：该已经放大了");

    let start = point(px(before_x + bwx / 2.0), px(260.0));
    let moved = point(px(before_x + bwx / 2.0 - 60.0), px(200.0));
    raw_touch_at(cx, handle, TouchPhase::Started, start);
    raw_touch_at(cx, handle, TouchPhase::Moved, moved);
    raw_touch_at(cx, handle, TouchPhase::Ended, moved);
    settle(cx, handle);

    let (after_x, _, after_y, _) = page_rect(cx, &view);
    assert!(
        (after_x - before_x).abs() > 20.0 || (after_y - before_y).abs() > 20.0,
        "纸该跟着手走：({before_x}, {before_y}) → ({after_x}, {after_y})"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        page_before,
        "放大着拖动的是纸，不该顺手翻页"
    );
}

/// 翻页回到铺满：读者按的是「看下一页」，不是「在新的一页上接着看这个角」。
#[gpui_kit::test]
fn turning_a_page_drops_the_zoom_back_to_fit(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle, _, _) = open_fixed_pdf(cx, "../../tests/fixtures/oss/pdf/wps-paper.pdf");
    let (_, fit0, _, _) = page_rect(cx, &view);
    assert!(fit0 > 50.0, "前置：这一页该画出来了 {fit0}");
    pinch(cx, handle, 1.5, point(px(200.0), px(260.0)));
    settle(cx, handle);
    assert!(view.read_with(cx, |v, _| v.fixed_zoom.scale()) > 1.2);

    view.update(cx, |v, cx| v.scroll_page(1, cx));
    settle(cx, handle);
    turn_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));

    assert_eq!(
        view.read_with(cx, |v, _| v.fixed_zoom.scale()),
        1.0,
        "翻页之后该回到铺满"
    );
    let (_, w1, _, _) = page_rect(cx, &view);
    let fit = view.read_with(cx, |v, _| v.fixed_frame.page.0);
    assert!(
        (w1 - fit).abs() < 1.5,
        "新一页该按铺满摆（这一页自己的铺满宽）：{w1} vs {fit}"
    );
}

/// 上下滚动那一档：捏合前后，手指下那一段内容停在原处（页栈还是 1x 坐标，
/// 变的只是画出来的倍率与滚动量）。
#[gpui_kit::test]
fn pinching_in_continuous_mode_holds_the_point_under_the_fingers(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle, _, _) = open_fixed_pdf_with_turn(
        cx,
        "../../tests/fixtures/oss/pdf/three-pages.pdf",
        PageTurn::Scroll,
    );
    scroll_down(cx, handle, 120.0);
    settle(cx, handle);

    let focal_y = 300.0_f32;
    let (scroll_before, pivot) = view.read_with(cx, |v, _| (v.scroll_y, v.fixed_frame.origin.1));
    assert!(scroll_before > 0.0, "前置：该已经滚下去一点");

    pinch(cx, handle, 0.5, point(px(230.0), px(focal_y)));
    settle(cx, handle);

    let (scroll_after, scale, pivot_after) = view.read_with(cx, |v, _| {
        (v.scroll_y, v.fixed_zoom.scale(), v.fixed_frame.origin.1)
    });
    assert!((scale - 1.5).abs() < 1e-2, "该放大到 1.5：{scale}");
    let content_before = scroll_before + (focal_y - pivot) / 1.0;
    let content_after = scroll_after + (focal_y - pivot_after) / scale;
    assert!(
        (content_before - content_after).abs() < 2.0,
        "手指下那一段该停在原处：{content_before} vs {content_after}"
    );
}

/// 定版式正文上的**长按**：与可重排同一套手势 —— 吸词、松手后菜单出来。
///
/// 手机上 PDF 没有拖选，长按是唯一的入口。这条与上面拖选那条互补：拖选走鼠标事件，
/// 长按走手势识别器送来的 `LongPressEvent`，两条少一条，读者就少一半。
#[gpui_kit::test]
fn a_pdf_page_long_press_selects_and_opens_the_menu(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle, _, _) = open_fixed_pdf(cx, "../../tests/fixtures/oss/pdf/wps-paper.pdf");
    settle(cx, handle);
    let at = pdf_char(&view, cx, 0, 0.25);
    for phase in [TouchPhase::Started, TouchPhase::Ended] {
        dispatch(
            cx,
            handle,
            LongPressEvent {
                phase,
                start_position: at,
                position: at,
            }
            .to_platform_input(),
        );
    }
    let (selection, menu) = view.read_with(cx, |v, _| (v.selection(), v.selection_menu_open()));
    assert!(selection.is_some(), "定版式长按应选中一个词");
    assert!(menu, "定版式长按成选后菜单条应开着");
    // 开着不等于摆得出来：定版式的高亮在另一层里算，菜单锚点必须也接到那一份，
    // 否则 `slot` 判 Hidden，屏上就是「选区在、菜单不出现」。
    settle(cx, handle);
    let bounds = view
        .read_with(cx, |v, _| v.menu_bounds.get())
        .expect("定版式长按后菜单应真的上屏");
    assert!(
        f32::from(bounds.size.height) >= 88.0,
        "菜单 bounds 太矮：{bounds:?}"
    );
    assert!(
        view.read_with(cx, |v, _| v.taps_selection_ui(bounds.center())),
        "落在菜单上的那一击必须先被选中这一套吃掉"
    );
}

/// 双页的折页只揭**靠手指那半张 leaf**：往前翻折左半张，背面是下一跨页的右页；
/// 往回翻折右半张，背面是上一跨页的左页。两张相邻页同屏时也照样成立。
#[gpui_kit::test]
fn a_fixed_spread_curl_peels_one_leaf_and_bakes_the_neighbouring_page(cx: &mut TestAppContext) {
    use crate::ui::features::reader::model::turn::TurnIntent;

    cx.update(gpui_kit::init);
    let (view, handle, _, _) = open_fixed_pdf_at(
        cx,
        "../../tests/fixtures/oss/pdf/three-pages.pdf",
        1200.0,
        PageTurn::Curl,
    );
    view.update(cx, |v, cx| v.set_mode(PageMode::Spread, cx));
    settle(cx, handle);
    // 走到最后一跨页（第 3 页 + 空白右半），再从那里往回翻：到达那一跨页是 0/1。
    view.update(cx, |v, cx| v.set_page(2, cx));
    let mut ready = false;
    for _ in 0..20 {
        settle(cx, handle);
        if view.read_with(cx, |v, _| v.curl_capture_ready()) {
            ready = true;
            break;
        }
    }
    assert!(ready, "前置：这一跨页的捕获纹理该烘好");
    assert_eq!(
        view.read_with(cx, |v, _| v.captured_page),
        Some((0, 2)),
        "前置：正面那张纹理记的是左页（定版式的章号恒为 0）"
    );

    view.update(cx, |v, cx| v.turn(TurnIntent::Previous, cx));
    assert_eq!(
        view.read_with(cx, |v, _| v.pending_turn),
        None,
        "纹理在手，这一趟不该排队"
    );
    settle(cx, handle);
    assert_eq!(view.read_with(cx, |v, _| v.page()), 0);
    assert_eq!(
        view.read_with(cx, |v, _| v.turn.map(|turn| turn.style)),
        Some(PageTurn::Curl)
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.captured_back.map(|(back, _)| back)),
        Some((0, 1, 0)),
        "往回翻揭右半张，背面该是到达那一跨页的右页"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.fixed_sheets_drawn),
        2,
        "到达那一跨页是相邻两页，两张纸都得摆上"
    );
}

/// 放大着的那一帧不烘纹理；读者这时点一下，必须**先收起缩放再排队等门** ——
/// 顺序反了，这一趟就永远停在「门没开」而页还放大着，捕获永远烘不出来。
#[gpui_kit::test]
fn a_zoomed_page_resets_before_the_curl_gate_queues_the_turn(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle, _, _) = open_fixed_pdf_with_turn(
        cx,
        "../../tests/fixtures/oss/pdf/three-pages.pdf",
        PageTurn::Curl,
    );
    // 模拟「纹理还没烘上」。
    view.update(cx, |v, _| {
        v.capture_dirty = true;
        v.captured_page = None;
    });
    let (x, w, _, _) = page_rect(cx, &view);
    pinch(cx, handle, 1.0, point(px(x + w / 2.0), px(260.0)));
    settle(cx, handle);
    assert!(
        view.read_with(cx, |v, _| v.fixed_zoom.scale()) > 1.5,
        "前置：这一页该是放大着的"
    );

    view.update(cx, |v, cx| v.scroll_page(1, cx));
    assert_eq!(
        view.read_with(cx, |v, _| v.pending_turn),
        Some(crate::ui::features::reader::model::turn::TurnIntent::Next),
        "门没开：这一趟该排队"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.fixed_zoom.scale()),
        1.0,
        "排队之前先把缩放收回 1x，否则捕获永远烘不出来"
    );
    assert_eq!(view.read_with(cx, |v, _| v.page()), 0);

    // 门开之后那一帧末会把排队这一趟补上。
    for _ in 0..20 {
        settle(cx, handle);
        if view.read_with(cx, |v, _| v.page()) == 1 {
            break;
        }
    }
    assert_eq!(
        view.read_with(cx, |v, _| v.page()),
        1,
        "纹理烘好之后排队那一趟该补上"
    );
    assert_eq!(view.read_with(cx, |v, _| v.pending_turn), None);
}

/// 定版式的每页读数带也跟着纸走：翻页那一帧它的 x 等于纸的位移，不是钉在窗口左边。
/// 从前定版式只有一层全局带，纸在动、带子不动。
#[gpui_kit::test]
fn a_fixed_band_rides_its_sheet(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle, _, _) = open_fixed_pdf(cx, "../../tests/fixtures/oss/pdf/three-pages.pdf");
    let rest = fixed_band_x(cx, handle, 0).expect("静止帧该有第 0 页的带子");
    assert!(
        rest.abs() < 1.0,
        "静止时带子该贴在纸的左缘，实得 x = {rest}"
    );

    view.update(cx, |v, cx| v.scroll_page(1, cx));
    turn_frame(cx, handle, Duration::from_millis(1));
    let moving = fixed_band_x(cx, handle, 1).expect("进场页的带子该在树里");
    assert!(
        moving > 150.0,
        "进场页的带子该跟着纸从右侧进来，实得 x = {moving}"
    );

    turn_frame(cx, handle, NAV_TRANSITION + Duration::from_millis(20));
    let landed = fixed_band_x(cx, handle, 1).expect("落定后带子该还在");
    assert!(
        landed.abs() < 1.0,
        "落定后带子该回到纸的静止位置，实得 x = {landed}"
    );
}

/// 第 `page` 页那条定版式带子的 x（窗口坐标）。
fn fixed_band_x(cx: &mut TestAppContext, handle: AnyWindowHandle, page: usize) -> Option<f32> {
    cx.update_window(handle, |_, window, _| {
        window
            .try_find(ElementId::from(("fixed-page-band", page as u64)))
            .map(|element| f32::from(element.bounds().origin.x))
    })
    .expect("窗口应能渲染")
}
