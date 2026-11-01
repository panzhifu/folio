//! 干净态那两条读数带：顶上一条章名，底下一条位置与进度。
//!
//! 它们由阅读会话自己画（`view::body`），不再是阅读页的兄弟层：仿真折页揭的是整张纸，
//! 这两条带也在纸上 —— 同一份 `Readouts` 会分别画进主场景（露出的目标页）与捕获层
//! （卷起来的旧页正面 / 背面）。所以这里的函数只吃一份快照，不自己回读会话。
//!
//! 独立成一份文件的理由和 [`crate::ui::features::reader::components::selection`] 一样：改动最勤的那一块（换措辞、调字号、
//! 挪内缩）不该住在 `page.rs` 里 —— 那一屏管的是「什么时候摆干净态、什么时候摆浮层」
//! 以及浮层本体，两件事从来不同时改。
//!
//! 两条带共用 [`band`] 那一层壳：满幅、定高、左右内缩、**不透明**底色。带里的字垂直摆位
//! 两条不一样：页眉贴带顶（见 [`HEADER_TOP_PAD`]），页脚居中。
//! 不透明是要害：正文让出的那一段只是按两条带的高往下错开，
//! 跨在视口上下沿的那一行照样会被画出来（它的行顶在可见带起点之外），带子透明就等于
//! 让半行字压在章名上。滚动时字在这里被干净地切掉，才是「让出」这条带的本意。

use gpui_kit::component::{ActiveTheme, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Div, Entity, InteractiveElement as _, IntoElement, ParentElement,
    SharedString, Styled, TestSupportExt as _, div, px,
};

use crate::ui::features::reader::model::chrome;
use crate::ui::features::reader::model::page_geometry::{BandPolicy, PageBox, PageRect};
use crate::ui::features::reader::model::session::ReaderSession;
use crate::ui::services::l10n::tf;

/// 页眉那条带里，字的上沿离带顶留多少（逻辑像素）。
///
/// [`band`] 的壳是 `h_flex`，交叉轴居中：带高 28 而 `text_xs` 的行盒约 17，居中就把这一行
/// 小字往下推了 5–6 像素。章名是贴着这一页顶边的一行读数（再往上就是阅读层让给状态栏 /
/// 挖孔的那一条），靠上排才不像正文的第一行。带高 [`chrome::HEADER_HEIGHT`] 不动 ——
/// 那个数同时是正文让出的高度（`page.rs` 把它递进 `set_chrome_bands`），改它等于改版心。
const HEADER_TOP_PAD: f32 = 2.0;

/// 这一屏该念出去的三个数。
#[derive(Clone)]
pub(in crate::ui::features::reader) struct Readouts {
    /// 当前章的章名。没有章名（书没带目录）时是空串，摆出来就是一块空白。
    pub chapter: SharedString,
    /// 章内位置：连续滚动念「第几节 / 共几节」，分页与双页念「第几页 / 共几页」。
    pub position: SharedString,
    /// 进度百分比，念法见 [`chrome::percent_label`]。
    pub percent: SharedString,
}

/// 把三个数一次算好。干净态那条页脚与浮层里那条状态行**共用这一份** ——
/// 同一本书的同一时刻不该在两处念出两个数。
pub(in crate::ui::features::reader) fn readouts(
    reader: &Entity<ReaderSession>,
    cx: &App,
) -> Readouts {
    reader.read_with(cx, readouts_of)
}

/// 同 [`readouts`]，但手上已经有一份 `&ReaderSession`（折页在它自己的 `render` 里画带子，
/// 不能再回读自己这个实体）。
pub(in crate::ui::features::reader) fn readouts_of(reader: &ReaderSession, cx: &App) -> Readouts {
    Readouts {
        chapter: reader.chapter_title().into(),
        position: position(reader, cx).into(),
        percent: chrome::percent_label(reader.progress()).into(),
    }
}

/// 指定页的读数：**只**按这一页自己的章名/页号算，百分比由调用方给（整本书的进度
/// 在翻页那一刻是同一个数，不值得为一个跨章目标再算一遍）。
///
/// 分页每一页一份带子时用它：跨章翻页里左右两页的章名/页号不同，不能都念当前页。
pub(in crate::ui::features::reader) fn readouts_for(
    doc: &crate::ui::features::reader::model::chapter_layout::Document,
    chapter: SharedString,
    page: usize,
    percent: SharedString,
    cx: &App,
) -> Readouts {
    let total = doc.page_count().max(1);
    Readouts {
        chapter,
        position: tf(
            cx,
            "reader.page_position",
            &[("index", &(page + 1)), ("total", &total)],
        )
        .into(),
        percent,
    }
}

/// 指定章、页的读数：折页翻到一半时，**底下的那一页**不是当前页。
///
/// 双页时 `page` 是一张纸的号（两栏算同一号），与 [`ReaderSession::page`] 同一个口径。
/// 定版式没有章表，退回它自己那份「第几页 / 共几页」。
pub(in crate::ui::features::reader) fn readouts_at(
    reader: &ReaderSession,
    chapter: usize,
    page: usize,
    cx: &App,
) -> Readouts {
    let Some(book) = reader.book() else {
        return Readouts {
            chapter: reader.chapter_title().into(),
            position: tf(
                cx,
                "reader.page_position",
                &[("index", &(page + 1)), ("total", &reader.page_count())],
            )
            .into(),
            percent: chrome::percent_label(reader.progress()).into(),
        };
    };
    let doc = book.span_doc(chapter);
    let total = doc.map_or(1, |doc| doc.page_count());
    let offset = doc.map_or(0, |doc| doc.offset_of_page(page));
    Readouts {
        chapter: book.chapter_title_at(chapter).into(),
        position: tf(
            cx,
            "reader.page_position",
            &[("index", &(page + 1)), ("total", &total)],
        )
        .into(),
        percent: chrome::percent_label(book.progress_at(chapter, offset)).into(),
    }
}

/// 「读到哪儿了」那句话，按当前生效的那一档念。
fn position(reader: &ReaderSession, cx: &App) -> String {
    if reader.mode().is_continuous() {
        return reader.chapter_position(cx);
    }
    // 双页时一号就是一张纸（两栏算同一号），所以分页与双页共用这一句。
    tf(
        cx,
        "reader.page_position",
        &[
            ("index", &(reader.page() + 1)),
            ("total", &reader.page_count()),
        ],
    )
}

/// 一条带的壳。顶边那条贴**这一页的顶边**、底边那条贴屏幕底，宽度与底色两端一致；
/// 带里的字只有垂直摆位不同：顶边那条靠带顶（[`HEADER_TOP_PAD`]），底边那条居中。
/// 顶上那条再往上就是阅读层让给系统的那一段，不在纸面里，也不用这条带管。
/// 用 [`PageBox`] 的矩形画全局那两条带（连续滚动 / 定版式）。
///
/// 位置、带高、页边距、手势条内缩全从 `PageBox` 来，不再由调用方一个个传。
/// 定版式（[`BandPolicy::Overlay`]）带子是半透明浮层，页图仍是整窗。
pub(in crate::ui::features::reader) fn overlay_in(
    page: &PageBox,
    readouts: &Readouts,
    bare: bool,
    cx: &App,
) -> AnyElement {
    overlay_bands(page, readouts, bare, true, cx)
}

/// 页内局部版：宽度跟着父容器走（双页时是半页），只读 `PageBox` 的纵向矩形。
pub(in crate::ui::features::reader) fn overlay_page(
    page: &PageBox,
    readouts: &Readouts,
    bare: bool,
    cx: &App,
) -> AnyElement {
    overlay_bands(page, readouts, bare, false, cx)
}

fn overlay_bands(
    page: &PageBox,
    readouts: &Readouts,
    bare: bool,
    full: bool,
    cx: &App,
) -> AnyElement {
    div()
        .absolute()
        .inset_0()
        .child(header_in(page, readouts, bare, full, cx))
        .child(footer_in(page, readouts, bare, full, cx))
        .into_any_element()
}

fn header_in(page: &PageBox, readouts: &Readouts, bare: bool, full: bool, cx: &App) -> AnyElement {
    let top_pad = (page.header.y - page.header_paint.y).max(0.0) + HEADER_TOP_PAD;
    let chapter = readouts.chapter.clone();
    band_shell(page.header_paint, page.bands, bare, true, full, cx)
        .pt(px(top_pad))
        .id(if bare {
            "reader-header-band-bare"
        } else {
            "reader-header-band"
        })
        .test_support()
        .child(
            div()
                .id("reader-header-chapter")
                .test_support()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .whitespace_nowrap()
                .when(!chapter.is_empty(), |label| label.child(chapter)),
        )
        .into_any_element()
}

fn footer_in(page: &PageBox, readouts: &Readouts, bare: bool, full: bool, cx: &App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let label = |text: SharedString| {
        div()
            .text_xs()
            .text_color(muted)
            .child(text)
            .into_any_element()
    };
    let margin = (page.footer.h - chrome::FOOTER_HEIGHT).max(0.0);
    let bottom_inset = (page.paper.bottom() - page.footer.bottom()).max(0.0);
    band_shell(page.footer_paint, page.bands, bare, false, full, cx)
        .pt(px(margin))
        .pb(px(bottom_inset))
        .child(label(readouts.position.clone()))
        .child(label(readouts.percent.clone()))
        .into_any_element()
}

/// 一条带的壳：`paint` 是底色铺到纸边的那一块，文字的内缩由调用方用
/// `pt` / `pb` 让出来。`Reserved` 不透明；`Overlay` 半透明，读成浮在页图上。
///
/// `full` = 按 `paint` 的 x/宽铺（整窗那一份）；`false` = 宽度交给父容器
/// （双页的一页那一份，父容器已经是半页宽）。
fn band_shell(
    paint: PageRect,
    policy: BandPolicy,
    bare: bool,
    at_top: bool,
    full: bool,
    cx: &App,
) -> Div {
    let shell = h_flex().absolute().px_4().overflow_hidden();
    let shell = if full {
        shell.left(px(paint.x)).w(px(paint.w))
    } else {
        shell.left_0().right_0()
    };
    let shell = shell.top(px(paint.y)).h(px(paint.h));
    let shell = if bare {
        shell
    } else {
        match policy {
            BandPolicy::Reserved => shell.bg(cx.theme().background),
            // 定版式：页图铺满整窗，带子不铺底，只让文字浮在照片上。
            BandPolicy::Overlay => shell,
        }
    };
    if at_top {
        shell.items_start()
    } else {
        shell.justify_between()
    }
}
