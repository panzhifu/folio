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
//! 不透明是要害：正文让出的那一段只是按 `header_reserve` / `footer_reserve` 往下错开，
//! 跨在视口上下沿的那一行照样会被画出来（它的行顶在可见带起点之外），带子透明就等于
//! 让半行字压在章名上。滚动时字在这里被干净地切掉，才是「让出」这条带的本意。

use gpui_kit::component::{ActiveTheme, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Div, Entity, InteractiveElement as _, IntoElement, ParentElement,
    SharedString, Styled, TestSupportExt as _, div, px,
};

use crate::ui::features::reader::model::chrome;
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
    let columns = reader.mode().columns();
    let total = doc.map_or(1, |doc| doc.page_count());
    let offset = doc.map_or(0, |doc| doc.offset_of_page(page, columns));
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
fn band(height: f32, at_top: bool, bare: bool, cx: &App) -> Div {
    let shell = h_flex()
        .absolute()
        .left_0()
        .right_0()
        .h(px(height))
        .px_4()
        // 整页画那一档不铺底：读数直接浮在画上，像 Reeden 那样只留一行小字。
        // 底色在普通页上是**必需的**（正文从带子底下滚过时要被干净地切掉），所以
        // 只有调用方明确说「这一屏是整页画」时才让它透明。
        .when(!bare, |shell| shell.bg(cx.theme().background))
        .overflow_hidden();
    if at_top {
        // `h_flex` 已经把交叉轴设成居中了，这里按同一根轴改到带顶。
        shell.top_0().items_start().pt(px(HEADER_TOP_PAD))
    } else {
        shell.bottom_0().justify_between()
    }
}

/// 页眉：只有章名。空章名不占字，摆出来是一块干净的空白。
///
/// `margin_y` 是这一边上边距（像素）：带子的**不透明底色**要跟着长高，盖住正文让出的
/// 那一段。只把正文往下推而不把带子长高，滚动时跨在边上的半行字就会从那条缝里露出来。
///
/// 顶上那条系统状态栏 / 挖孔**不归这里让**：阅读层（`shell::reading_overlay`）已经把
/// 整页往下推过 `top_inset`，带子的顶边就是那一格的下沿。这里再加一次，字会再低一截、
/// 带子也会向下多盖一条正文（封面那类顶格图正是这样被切掉一截的）。层顶上那一条由
/// 层自己的底色铺着，与带子同一个色，看上去仍是一整块。
pub(in crate::ui::features::reader) fn header(
    readouts: &Readouts,
    margin_y: f32,
    bare: bool,
    cx: &App,
) -> AnyElement {
    let chapter = readouts.chapter.clone();
    band(chrome::HEADER_HEIGHT + margin_y.max(0.0), true, bare, cx)
        // 宿主测试要量带子的底边：带子高过 `header_reserve` 就等于盖住正文顶上的字/图。
        .id("reader-header-band")
        .test_support()
        .child(
            div()
                .id("reader-header-chapter")
                // 宿主测试量的就是这一格的顶边与带子的底边：字贴着带顶、带子不越进正文。
                // 非 `test-support` 构建里这一层是透明的，真机不多花一个节点。
                .test_support()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .whitespace_nowrap()
                .when(!chapter.is_empty(), |label| label.child(chapter)),
        )
        .into_any_element()
}

/// 页脚：左边章内位置，右边进度。两端各占一条边。
///
/// `bottom_inset` 是底下那条系统手势条的高度：带子长高那一段、内容留在带顶，于是地脚那行
/// 小字在手势条之上，而带子的底色（页底色）一直铺到屏幕底 —— 不让的话，底栏拾起来之后
/// 那一段就是一条空缝。
pub(in crate::ui::features::reader) fn footer(
    readouts: &Readouts,
    bottom_inset: f32,
    margin_y: f32,
    bare: bool,
    cx: &App,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let label = |text: SharedString| {
        div()
            .text_xs()
            .text_color(muted)
            .child(text)
            .into_any_element()
    };
    // 带子随上下页边距长高：它不透明，长出来的那一截负责盖住正文让出的空白，
    // 不然滚动时半行字会从那条缝里露出来。
    //
    // 读数**不许跟着长高走**：`pt(margin)` 把页边距从内容盒顶上让出去、`pb(inset)`
    // 让出手势条，内容盒于是仍是原来那 FOOTER 高的一行 —— `h_flex` 的居中只在这
    // 一截里生效，进度那行于是钉在离屏幕底固定的位置。反过来把页边距塞进 `pb`，
    // 内容盒会被顶到带子顶上，读者拖上下边距时进度条就跟着上下跑。
    let margin = margin_y.max(0.0);
    band(
        chrome::FOOTER_HEIGHT + margin + bottom_inset,
        false,
        bare,
        cx,
    )
    .when(margin > 0.0, |band| band.pt(px(margin)))
    .when(bottom_inset > 0.0, |band| band.pb(px(bottom_inset)))
    .child(label(readouts.position.clone()))
    .child(label(readouts.percent.clone()))
    .into_any_element()
}

/// 这一页的两条读数带：贴这一页的上下两条边，中间全空。
///
/// 上面那一条**不碰系统安全区**：阅读层已经把整页推过 `top_inset`（`shell::reading_overlay`），
/// 带子的顶边就是那一格的下沿，铺到屏幕顶的那一段由层自己的底色负责（与带子同色）。
/// 下面那一条仍要让过手势条：底边没有层的 padding 可用，带子自己长高、把底色铺到屏幕底。
///
/// 折页时它是纸的一部分：同一份会画进主场景（露出那一页的读数），也会画进捕获层（卷起来
/// 那张纸的读数）。所以参数是**一份快照**而不是会话 —— 同一帧里「底下的页」与「卷着的那页」
/// 是两页，各自念自己的数。
pub(in crate::ui::features::reader) fn overlay(
    readouts: &Readouts,
    header_margin: f32,
    footer_margin: f32,
    bottom_inset: f32,
    bare: bool,
    cx: &App,
) -> AnyElement {
    div()
        .absolute()
        .inset_0()
        .child(header(readouts, header_margin, bare, cx))
        .child(footer(readouts, bottom_inset, footer_margin, bare, cx))
        .into_any_element()
}
