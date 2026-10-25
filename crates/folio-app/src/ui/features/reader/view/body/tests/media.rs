//! 正文里的图、链接与脚注：命中与归因。

use super::harness::{
    PARA, char_point, meta, offset_of, open, open_with_document, open_with_store, prefs,
    scroll_mode, settle,
};
use folio_core::content::document::Document;
use folio_core::store::Store;
use folio_core::{Chapter, Content, DocMeta, FlowDoc, Format, Locator};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext as _, TestAppContext, point, px};
use std::sync::Arc;

/// 一章：两段话中间夹一张 200×100 的插图，第一屏就一定摆得上。
fn image_flow() -> FlowDoc {
    FlowDoc {
        chapters: vec![Chapter::inline(
            "第一章",
            format!("<p>{PARA}</p><p><img src=\"p.png\" alt=\"\"></p><p>{PARA}</p>"),
        )],
        images: vec![folio_core::content::format::ImageRes {
            href: "p.png".into(),
            source: "p.png".into(),
            media_type: "image/png".into(),
            width: 200,
            height: 100,
        }],
        ..Default::default()
    }
}

// ---- 整本书的那一列图：顺序、出处，以及「定位原文」跳到它出现的那一行 ----
/// 两章都带图的一本合成书：第一章一张、第二章一张，另有一条 `src` 指向图片表里没有的路径。
///
/// `src` 两种形状都要有，因为那一列必须**按本章目录**解它们（与排版取字节用的是同一句
/// `ChapterImages::resolve`）：`../Images/a.png` 对 `OEBPS/text/c1.xhtml` 解到兄弟目录，
/// `b.png` 解到本章自己那一条。
///
/// 每章 20 段是**故意的**：落点要看得见，这本书就得真能滚 —— 两章各一段的书整本比
/// 一屏还短，`scroll_y` 夹在 0 上，跳章这件事在界面上根本没有对应物。段与段之间
/// **不留空白**：可见字符那一根尺连空白一起数（与 `anchor::TextMap` 同口径），
/// 夹进去一个换行，下面那两条偏移就得重算。
fn two_chapter_image_flow() -> FlowDoc {
    use folio_core::content::format::ImageRes;
    let table = |href: &str| ImageRes {
        href: href.into(),
        source: href.into(),
        media_type: "image/png".into(),
        width: 200,
        height: 100,
    };
    // 「第N行字」= 4 个可见字符一段（N 都是一位数，所以每一段一样长）。
    let para = |n: usize| format!("<p>第{n}行字</p>");
    let before = |n: usize| (0..n).map(para).collect::<Vec<_>>().join("");
    let after = |from: usize| (from..20).map(para).collect::<Vec<_>>().join("");

    FlowDoc {
        chapters: vec![
            // 图在第 3 段之后 ⇒ 它前面已经数到 3 × 4 = 12 个字。
            Chapter::inline(
                "第一章",
                format!(
                    "{}<p><img src=\"../Images/a.png\" alt=\"\"></p>{}",
                    before(3),
                    after(3)
                ),
            )
            .with_spine_href("OEBPS/text/c1.xhtml"),
            // 图在第 2 段之后 ⇒ 8 个字；后面还有一条解不出的 `gone.png`。
            Chapter::inline(
                "第二章",
                format!(
                    "{}<p><img src=\"b.png\" alt=\"\"><img src=\"gone.png\" alt=\"\"></p>{}",
                    before(2),
                    after(2)
                ),
            )
            .with_spine_href("OEBPS/text/c2.xhtml"),
        ],
        images: vec![table("OEBPS/Images/a.png"), table("OEBPS/text/b.png")],
        ..Default::default()
    }
}

// ---- 正文内的链接：点目录页正文里的 `<a href>` 跳到那一章 ----
/// 一本书内目录页：第一章的正文本身就是一份 `<ul>` 链接表，指向后面两章。
/// 这是真书里「内容里的目录」的形状（`罗生门` 的 `part0003.xhtml` 就是
/// `<a href="part0004.xhtml#a005">罗生门</a>`）。`c2` 那条带章内锚点。
fn link_flow() -> FlowDoc {
    FlowDoc {
        chapters: vec![
            Chapter::inline(
                "目录",
                "<ul>\
                 <li><a href=\"c1.xhtml\">第一章</a></li>\
                 <li><a href=\"c2.xhtml#mid2\">第二章</a></li>\
                 </ul>",
            )
            .with_spine_href("toc.xhtml"),
            Chapter::inline("第一章", "<p>第一章正文</p>").with_spine_href("c1.xhtml"),
            Chapter::inline("第二章", "<p>第二章正文</p><p id=\"mid2\">锚点段</p>")
                .with_spine_href("c2.xhtml"),
        ],
        ..Default::default()
    }
}

// ---- 脚注：点中一条引用就把注文抬出来 ----
/// 一本带脚注的书：正文里一个上标链接（`<a epub:type="noteref">`），章尾一个
/// `<aside epub:type="footnote">`。
fn footnote_flow() -> FlowDoc {
    FlowDoc {
        chapters: vec![Chapter::inline(
            "第一章",
            "<p>正文开头<a epub:type=\"noteref\" href=\"#fn1\">1</a>，正文结尾。</p>\
             <aside epub:type=\"footnote\" id=\"fn1\"><p>这是一条脚注的全文。</p></aside>",
        )],
        ..Default::default()
    }
}

// ---- 插图的可点性：点一张图，要把它交给放大查看那一层 ----
/// 一份「图片字节就在手里」的文档。
///
/// `from_content` 造出来的那本没有容器，`image_bytes` 永远给不出字节，于是插图一张
/// 都不画 —— 而「点图上那一下」在测试里就没有目标可点。这一份按 `href` 直接交出字节，
/// 排版、按需解码、绘制走的仍然是生产那几条路。
struct Illustrated {
    meta: DocMeta,
    content: Content,
}

impl Document for Illustrated {
    fn meta(&self) -> &DocMeta {
        &self.meta
    }

    fn kind(&self) -> folio_core::content::format::ContentKind {
        self.content.kind()
    }

    fn content(&self) -> folio_core::content::document::ContentRef<'_> {
        match &self.content {
            Content::Reflow(flow) => folio_core::content::document::ContentRef::Reflow(flow),
            Content::Fixed(fixed) => folio_core::content::document::ContentRef::Fixed(fixed),
        }
    }

    fn image_bytes(&self, href: &str) -> Option<Vec<u8>> {
        (href == "p.png").then(|| crate::ui::foundation::images::TINY_PNG.to_vec())
    }

    fn stylesheet(&self) -> String {
        String::new()
    }

    /// 正文按章现取那一条：字节不在容器里，就在手上这份 `FlowDoc` 里。
    fn chapter_source(&self, index: usize) -> folio_core::Result<String> {
        let Content::Reflow(flow) = &self.content else {
            return Err(folio_core::Error::NoSuchChapter(index as i64));
        };
        flow.chapters
            .get(index)
            .map(|chapter| chapter.source().to_string())
            .ok_or(folio_core::Error::NoSuchChapter(index as i64))
    }
}

/// 回归门：点正文里的一张插图，把这张图交给放大那一层。
///
/// 钉的是**按不住长按的那一端**（宿主测试没装平台服务，[`supports_long_press`] 给 false
/// ⇒ 走单击那一条）；真机（手机）上入口是长按，见 `image_child` 的两臂。
///
/// 两条断言各钉一段路：
/// - `is_open` 钉「图上真挂了入口，而且那个 handler 认得要看哪张」；
/// - 固有尺寸钉「排版有没有把尺寸带到界面」。少了后一条没有任何可见症状 —— 查看器会
///   静默退到经验上限，读者只看得见「放不大」，而线索指向的是排版。
#[gpui_kit::test]
fn tapping_an_illustration_hands_it_to_the_zoom_layer(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let doc: Arc<dyn Document> = Arc::new(Illustrated {
        meta: meta("epub"),
        content: Content::Reflow(image_flow()),
    });
    let viewer = cx.update(|cx| {
        cx.new(|_| crate::ui::features::reader::components::image_viewer::ImageViewer::default())
    });
    let (view, handle) =
        open_with_document(cx, doc, prefs(16.0), Locator::start(), 460.0, None, None);
    // 生产上这一层由阅读页挂上（它画在那一屏的最上面）；这里只要那个句柄，
    // 层本身不在本窗口的树里 —— 钉的是「点击有没有把图交出去」，不是层的摆位。
    cx.update(|app| {
        view.update(app, |session, _| {
            session.set_image_viewer(viewer.clone());
        })
    });
    settle(cx, handle);

    let found = cx
        .update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.try_find(("reader-image", 0usize)).is_some()
        })
        .expect("渲染应成功");
    assert!(found, "前置：那张插图要真被摆上屏，否则根本没有可点的东西");

    cx.update_window(handle, |_, window, cx| {
        window.click(("reader-image", 0usize), cx);
    })
    .expect("点击应送达");
    cx.run_until_parked();

    assert!(
        viewer.read_with(cx, |viewer, _| viewer.is_open()),
        "点插图该把这张图交给放大那一层"
    );
    assert_eq!(
        viewer.read_with(cx, |viewer, _| viewer.seen_natural()),
        Some((200.0, 100.0)),
        "固有尺寸要从 manifest 一路带到查看器：丢了它只会静默退到经验上限"
    );
}

/// 那一列是**阅读顺序**、带**出处**，并且只收解得出来的那几条。
///
/// 钉四件事，每一件错了都没有别的可见症状：
/// - 顺序：查看器念的「第 n 张」与左右滑走的次序全跟着这一列走，顺序错了就是
///   「第 2 张画的是第 3 张的图」，屏幕上看不出来；
/// - 出处：`chapter` + 章内可见字符偏移，「定位原文」跳的就是这两个数；
/// - 解不出来的 `src` 不进列：让读者滑到一张永远画不出来的黑纸上，比那一列少一张糟得多；
/// - 键的形状：`<本书 token>/<容器路径>`，与正文摆那张图时问图集用的是同一个键 ——
///   两边不一，点图上那一下就会认不出要看的是哪一张。
#[gpui_kit::test]
fn the_image_column_is_reading_order_with_a_place_to_jump_to(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = two_chapter_image_flow();
    let (view, handle) = open(cx, &flow, "epub", prefs(16.0), Locator::start(), 460.0);
    settle(cx, handle);

    let column = view.read_with(cx, |v, _| v.image_sequence().clone());
    let listed: Vec<(usize, usize, &str)> = column
        .iter()
        .map(|slot| match slot.place {
            crate::ui::features::reader::components::image_viewer::Place::Reflow {
                chapter,
                offset,
            } => (chapter, offset, slot.href.as_str()),
            crate::ui::features::reader::components::image_viewer::Place::Fixed { page } => {
                panic!("可重排这一支不该出现定版式那一条：page {page}")
            }
        })
        .collect();
    assert_eq!(
        listed,
        [
            // 第一章那张在第 3 段之后 ⇒ 前面已经数到 3 × 4 = 12 个字；
            // `../Images/` 解到兄弟目录那一条。
            (0, 12, "OEBPS/Images/a.png"),
            // 第二章那张在第 2 段之后 ⇒ 8 个字；同一段里后面的 `gone.png` 不在表里，不进列。
            (1, 8, "OEBPS/text/b.png"),
        ],
        "顺序、章号、章内偏移与解出来的容器路径，四样都要是这一列"
    );
    // 固有尺寸与文件名：放大上限按前者算，文件名上那个序号是防同名的两张互相盖掉。
    assert_eq!(column[0].natural, (200.0, 100.0));
    assert_eq!(column[0].name, "001_a.png");
    assert_eq!(column[1].name, "002_b.png");
    // 键带着本书的 token 与容器路径两截（`<token>/<href>`）。
    assert!(
        column[0]
            .request
            .key
            .ends_with(&format!("/{}", column[0].href)),
        "图集的键该以容器路径为后一半：{}",
        column[0].request.key
    );
}

/// 「定位原文」：关掉那一层，把正文落到那一张出现的地方 —— 目标在**别的章**也要落得对。
///
/// 换章的那一跳不能就地算坐标：目标章这时往往还没排版，「第几个字在哪一像素」要等
/// 排版做完才有答案。所以这一条真正钉的是「排版做完那一帧落对了地方」，
/// 顺带钉三件配套的事：层关了、恢复请求被取走了（不挂着就不会再跳）、位置进了库。
#[gpui_kit::test]
fn locating_a_picture_closes_the_layer_and_lands_on_its_line(cx: &mut TestAppContext) {
    use crate::ui::features::reader::components::image_viewer::ImageViewer;

    cx.update(gpui_kit::init);
    let flow = two_chapter_image_flow();
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
    scroll_mode(cx, handle);
    let viewer = cx.update(|app| app.new(|_| ImageViewer::default()));
    cx.update(|app| view.update(app, |session, _| session.set_image_viewer(viewer.clone())));
    settle(cx, handle);

    // 正文在第 0 章；把查看器开到**第二章那一张**（那一列的第 2 条）。
    let sequence = view.read_with(cx, |v, _| v.image_sequence().clone());
    cx.update(|app| {
        viewer.update(app, |viewer, cx| {
            viewer.open(1, (460.0, 520.0), sequence);
            cx.notify();
        })
    });
    settle(cx, handle);
    assert!(
        viewer.read_with(cx, |viewer, _| viewer.is_open()),
        "前置：那一层要开着，才谈得上「定位原文」把它关掉"
    );

    view.update(cx, |v, cx| v.locate_image(1, cx));
    settle(cx, handle);

    assert!(
        !viewer.read_with(cx, |viewer, _| viewer.is_open()),
        "定位原文该先把这一层关掉 —— 压着一层黑看那张图自己不算回到正文"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.chapter_index()),
        1,
        "锚章该挪到那一张所在的第 2 章"
    );
    let landed = offset_of(&view, cx);
    assert!(
        landed.abs_diff(8) <= 1,
        "落点该在那一张之前那一个字上（第二章第 2 段末 = 第 8 个字）：实际 {landed}"
    );
    assert!(
        !view.read_with(cx, |v, _| v.has_pending_restore()),
        "恢复请求该在锚章排完那一帧被取走，而不是永远挂着"
    );
    assert!(
        view.read_with(cx, |v, _| v.scroll_y) > 0.0,
        "跳到的是本章里第 2 段之后那一处，屏上不该还停在章首"
    );
    let (_, locator) = store.position(id).expect("读回进度").expect("进度应存在");
    assert_eq!(
        locator.chapter, 1,
        "跳过去就要落库：读者杀进程回来还在那一张的地方"
    );
}

/// 点正文里的一条链接，锚章真的换到它指向的那一章。走的是 `on_body_tap` 里
/// 链接优先那一支（`page.rs`）落到的那段逻辑。
#[gpui_kit::test]
fn tapping_a_link_in_the_body_text_jumps_to_its_target(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &link_flow(),
        "epub",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    scroll_mode(cx, handle);
    // 前置：目录页那一行真有字，否则根本没有可点的目标。
    let first_line: String = view.read_with(cx, |v, _| v.line_texts().join(""));
    assert!(
        first_line.contains("第一章"),
        "前置：目录页第一行该摆着链接文字：{first_line:.80}"
    );
    // 点第一个链接（第一行第一个字的中心）。
    let at = char_point(&view, cx, 0, 0);
    let consumed = cx
        .update_window(handle, |_, window, cx| {
            view.update(cx, |v, cx| v.open_link_at(window, cx, at))
        })
        .expect("渲染应成功");
    assert!(consumed, "点在链接上，这一击该被链接吃掉");
    assert_eq!(
        view.read_with(cx, |v, _| v.chapter_index()),
        1,
        "该跳到链接指向的第一章"
    );
}

/// 点中脚注引用（正文里的那个上标）就把注文抬出来，而不是去跳那一行。
#[gpui_kit::test]
fn tapping_a_footnote_ref_raises_the_note(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &footnote_flow(),
        "epub",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    scroll_mode(cx, handle);
    // 第一行是「正文开头1，正文结尾。」，第 5 个字（下标 4）就是那个上标「1」。
    let at = char_point(&view, cx, 0, 4);
    let consumed = cx
        .update_window(handle, |_, window, cx| {
            view.update(cx, |v, cx| v.open_footnote_at(window, cx, at))
        })
        .expect("渲染应成功");
    assert!(consumed, "点在脚注引用上，这一击该被脚注吃掉");
    assert_eq!(
        view.read_with(cx, |v, _| v.footnote().map(|f| f.text.clone())),
        Some("这是一条脚注的全文。".to_string()),
        "弹层该拿到章尾 <aside> 里那句原文"
    );
}

/// 锚点是**标记自己那一格**，不是手指那一下。
///
/// 同一个标记上点两处（第二处往下 40 像素，等于手指肚盖在标记下方），抬出来的小纸片该落在
/// 同一个地方 —— 从前这里存的就是落点，于是纸片跟着手指往下掉一截，读者看到的是「离标记
/// 有点远、不在正下方」。
#[gpui_kit::test]
fn the_footnote_anchor_is_the_marker_not_the_finger(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &footnote_flow(),
        "epub",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    scroll_mode(cx, handle);
    let marker = char_point(&view, cx, 0, 4);
    // 那一行的下缘与它的横向范围（窗口坐标）：小纸片该贴在这一行的下缘起笔，横向不许跑出
    // 这一行去。`char_point` 是按字号估的落点，标记本身是个窄上标，所以这里不拿它当尺量 x。
    let (bottom, line_left, line_right) = view.read_with(cx, |v, _| {
        let box_ = &v.lines()[0];
        let left = px(v.prefs.margin_x + box_.x);
        (
            px(box_.y + box_.height - v.scroll_y + v.header_reserve),
            left,
            left + px(box_.width),
        )
    });
    let mut anchors = vec![];
    for (tag, tap) in [
        ("点在标记正中", marker),
        (
            "点在标记下方 12 像素（手指肚那一挡，还在同一行里）",
            point(marker.x, marker.y + px(12.0)),
        ),
    ] {
        let got = cx
            .update_window(handle, |_, window, cx| {
                view.update(cx, |v, cx| {
                    let consumed = v.open_footnote_at(window, cx, tap);
                    (consumed, v.footnote().map(|f| f.anchor))
                })
            })
            .expect("渲染应成功");
        assert!(got.0, "{tag}：这一击该被脚注吃掉");
        let anchor = got.1.expect("{tag}：锚点该有值");
        assert!(
            (anchor.y - bottom).abs() < px(1.0),
            "{tag}：锚点该贴在标记那一行的下缘 {bottom}，实得 {anchor:?}（落点是 {tap:?}）"
        );
        assert!(
            anchor.x >= line_left && anchor.x <= line_right,
            "{tag}：锚点横向该落在标记那一行里（{line_left}…{line_right}），实得 {anchor:?}"
        );
        anchors.push(anchor);
    }
    assert_eq!(
        anchors[0], anchors[1],
        "同一个标记上的两处落点，抬出来的小纸片该在同一个地方"
    );
}
