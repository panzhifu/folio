//! 两端共用的排版地基（`ui/shared/document.rs` 与 `measure.rs`）在宿主上跑的用例。
//!
//! 为什么这些算移动端的测试：移动 UI 本身只在 `cfg(mobile)` 下编译，宿主的
//! `cargo test` 碰不到那一侧的视图。但「同一本书在两端排出一样的行」这件事
//! 完全落在这一层 —— 源文本包装、层叠、版心宽度、断行与缓存都在这里，
//! 移动端只是把它们摆上屏的那层薄壳。所以这一层被测住，移动端的排版就不是没测过。
//!
//! 反过来讲也很清楚：这里**测不到**的是触摸滚动、章节按钮的点击、主题在 Android 上
//! 解析到浅还是深 —— 那几件只有真开窗口、真上机器才算验证。

use std::sync::Arc;

use folio_app::ui::shared::document::{Document, body_of, chapter_xhtml};
use folio_app::ui::shared::images::ChapterImages;
use folio_core::layout::LineFragment;
use folio_core::{Chapter, Format, Reader};
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Context, Render, Styled, TestAppContext, div, px, size,
};

/// 一个只占满窗口的空视图。这些用例要的是**窗口**（度量与视口都挂在窗口上），
/// 界面内容无关，所以给它最小的那一副。
struct Blank;

impl Render for Blank {
    fn render(
        &mut self,
        _window: &mut gpui_kit::Window,
        _cx: &mut Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        div().size_full()
    }
}

/// 开一个指定尺寸的窗口，返回它的句柄。版心宽度的用例靠它给视口。
fn window(cx: &mut TestAppContext, width: f32, height: f32) -> AnyWindowHandle {
    cx.open_window(size(px(width), px(height)), |_, _| Blank)
        .into()
}

/// 一段足够长的中文，保证它会折行 —— 不折行的话「缩进改变了布局」这种断言无从成立。
const LONG: &str = "于是我们奋力向前，逆水行舟，被不断地向后推，推入过去。";

fn prefs() -> Reader {
    Reader::default().clamped()
}

/// 把版心相关的两项设成「不参与这段」的值：`margin_x` 归零、上限关掉。
/// 留白与上限本身另有用例，这里只关心缩进与折行。
fn bare_prefs() -> Reader {
    let mut prefs = prefs();
    prefs.margin_x = 0.0;
    prefs.max_measure = 0.0;
    prefs
}

/// 在窗口里把一段 XHTML 排完，返回排好的 [`Document`]。
///
/// 返回整个文档而不是行坐标，是因为要断言的东西有一半在文本里（哪一行是什么、
/// 有没有留下一个画不出来的图片位）。
fn layout_of(cx: &mut TestAppContext, xhtml: &str, prefs: &Reader, width: f32) -> Document {
    let handle = window(cx, width, 900.0);
    let mut document = Document::new(
        xhtml.to_string(),
        "",
        prefs.clone(),
        Arc::new(ChapterImages::empty()),
    );
    cx.update_window(handle, |_, win, cx| {
        document.ensure_layout(win, cx, 0.0);
    })
    .expect("窗口应当可排版");
    document
}

/// Markdown 的章源文本是规范化纯文本，进排版层之前要包成 XHTML。
/// 这里钉住四件事：空行分段、段内换行成硬换行、ATX 标题成 `<hN>`、特殊字符转义。
/// 少任何一条，移动端读 `.md` 就会把整篇排成一个段落，或把尖括号当标签吃掉。
#[test]
fn markdown_blocks_become_xhtml_without_losing_text() {
    let chapter = Chapter {
        title: "第一章".into(),
        source: "# 标题\n\n第一段\n还是第一段\n\n第二段\n\n<script>x</script>".into(),
        spine_href: String::new(),
    };
    let xhtml = chapter_xhtml(&chapter, Format::Markdown);

    assert!(
        xhtml.contains("<h1>标题</h1>"),
        "ATX 标题要成 `<h1>`：{xhtml}"
    );
    assert!(
        xhtml.contains("<p>第一段<br/>还是第一段</p>"),
        "段内换行要成硬换行：{xhtml}"
    );
    assert!(xhtml.contains("<p>第二段</p>"), "空行分段：{xhtml}");
    // 转义后不能剩下裸的 `<script>`，否则 html5ever 会真当标签解析，正文就少一段。
    assert!(
        !xhtml.contains("<script>"),
        "正文里的尖括号必须转义：{xhtml}"
    );
    assert!(xhtml.contains("&lt;script&gt;"));

    // EPUB 的章节本来就是 XHTML，一个字节都不该动。
    let epub = Chapter {
        title: String::new(),
        source: "<body><p>原样</p></body>".into(),
        spine_href: String::new(),
    };
    assert_eq!(chapter_xhtml(&epub, Format::Epub), epub.source);
}

/// `max_measure == 0` 是「不设上限、随窗口铺满」的**哨兵值**，不是 0 像素的上限。
/// 当成上限去夹就会塌成 1px 版心、一行只剩一个字 —— 全屏阅读时整个正文会变成一列竖字。
#[gpui_kit::test]
fn a_zero_max_measure_means_no_cap(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let handle = window(cx, 1000.0, 600.0);
    let mut uncapped = prefs();
    uncapped.margin_x = 48.0;
    uncapped.max_measure = 0.0;
    let mut capped = uncapped.clone();
    capped.max_measure = 400.0;

    let (free, limited) = cx
        .update_window(handle, |_, win, _| {
            (
                Document::content_width(win, &uncapped, 0.0),
                Document::content_width(win, &capped, 0.0),
            )
        })
        .expect("窗口应当可读");
    assert_eq!(free, 1000.0 - 48.0 * 2.0, "不设上限时版心随视口铺满");
    assert_eq!(limited, 400.0, "设了上限就按上限");
}

/// 侧栏那种「不属于阅读栏的宽度」必须从版心里扣掉，否则引擎按整窗宽折行、
/// 屏幕按窄栏裁剪，居中的标题会被推到栏外。移动端没有侧栏，所以传 0 ——
/// 同一个函数、同一个语义，这里也替那条路径把式子钉住。
#[gpui_kit::test]
fn the_pane_reserve_is_subtracted_from_the_measure(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let handle = window(cx, 1000.0, 600.0);
    let prefs = bare_prefs();
    let width = cx
        .update_window(handle, |_, win, _| {
            Document::content_width(win, &prefs, 260.0)
        })
        .expect("窗口应当可读");
    assert_eq!(width, 740.0, "侧栏占位要从视口宽里扣掉");
}

/// **缓存的键必须覆盖所有排版输入**。
///
/// 这一条是回归门：缓存键曾经只记 `(宽度, 字号, 行距)`，于是读者在设置里改首行缩进或段距
/// 时，`ensure_layout` 认定几何参数没变而直接复用旧布局 —— 界面上看不到任何报错，
/// 只是那一项设置怎么改都不生效。现在键里带整份偏好，这条断言才会成立。
#[gpui_kit::test]
fn an_indent_change_reflows_the_document(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let handle = window(cx, 600.0, 900.0);
    let mut prefs = bare_prefs();
    prefs.indent_em = 0.0;
    let mut document = Document::new(
        format!("<p>{}</p>", LONG.repeat(6)),
        "",
        prefs.clone(),
        Arc::new(ChapterImages::empty()),
    );

    // 第一次排版：无缩进。
    cx.update_window(handle, |_, win, cx| document.ensure_layout(win, cx, 0.0))
        .expect("第一次排版应当成功");
    let flat = document.lines().first().map(|line| line.x).unwrap_or(0.0);

    // 同一个文档、同一个窗口，**只**改首行缩进：宽度、字号、行距都没变。
    // 缓存键只记那三项时这里会直接复用上面的布局，第一行的左缘一动不动。
    prefs.indent_em = 2.0;
    document.set_prefs(prefs);
    cx.update_window(handle, |_, win, cx| document.ensure_layout(win, cx, 0.0))
        .expect("第二次排版应当成功");
    let indented = document.lines().first().map(|line| line.x).unwrap_or(0.0);

    assert!(
        indented > flat,
        "首行缩进改了必须重排：不缩进时第一行左缘 {flat}，缩进两字后 {indented}"
    );
}

/// **解不出资源的 `<img>` 整张跳过**，不塌成空盒、也不留一块画不出来的空白。
///
/// 这条对移动端尤其要紧：那一侧的图片现在按引擎给的位真画
/// （`ui/mobile/reader_view.rs` 用 gpui 的 `img` 元素），所以一个解不出来的 `src`
/// 若还留着位子，读者看到的就是一块空洞，还会以为正文排错了。
/// 跳过之后行里的文本必须依然完整。
#[gpui_kit::test]
fn an_unresolvable_image_is_skipped_not_left_as_a_gap(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let document = layout_of(
        cx,
        "<p>前<img src=\"missing.png\" width=\"120\" height=\"80\">后</p>",
        &bare_prefs(),
        600.0,
    );
    let line = document.lines().first().expect("至少要有一行");
    assert!(
        !line
            .fragments
            .iter()
            .any(|fragment| matches!(fragment, LineFragment::Image(_))),
        "解不出资源的 `<img>` 该整张跳过，不该留一个画不出来的位"
    );
    assert_eq!(
        document.line_texts().concat(),
        "前后",
        "跳过之后行里的文字仍然是完整的"
    );
}

/// 排版层要找的那个根：html5ever 会替片段补出 `<body>`，所以裸片段也有一个 body 可用；
/// 取到的必须是那个元素而不是文档根，否则 `<body>` 自己的 padding 与样式不生效。
#[test]
fn body_of_picks_the_body_element() {
    for source in [
        "<html><body><p>有 body</p></body></html>",
        "<p>只有正文</p>",
        "",
    ] {
        let dom = folio_core::dom::Dom::parse(source.as_bytes());
        let body = body_of(&dom);
        assert_eq!(
            dom.tag_name(body),
            Some("body"),
            "{source:?} 该排到 `<body>` 下面，实际取到了别的节点"
        );
    }
}
