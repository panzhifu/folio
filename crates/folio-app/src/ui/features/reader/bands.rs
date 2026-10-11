//! 干净态那两条读数带：顶上一条章名，底下一条位置与进度。
//!
//! 独立成一份文件的理由和 [`selection_menu`] 一样：改动最勤的那一块（换措辞、调字号、
//! 挪内缩）不该住在 `page.rs` 里 —— 那一屏管的是「什么时候摆干净态、什么时候摆浮层」
//! 以及浮层本体，两件事从来不同时改。
//!
//! 两条带共用 [`band`] 那一层壳：满幅、定高、左右内缩、垂直居中、**不透明**底色。
//! 不透明是要害：正文让出的那一段只是按 `header_reserve` / `footer_reserve` 往下错开，
//! 跨在视口上下沿的那一行照样会被画出来（它的行顶在可见带起点之外），带子透明就等于
//! 让半行字压在章名上。滚动时字在这里被干净地切掉，才是「让出」这条带的本意。

use gpui_kit::component::{ActiveTheme, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Div, Entity, IntoElement, ParentElement, SharedString, Styled, div, px,
};

use super::chrome;
use super::session::ReaderSession;
use crate::ui::shared::l10n::tf;

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
    reader.read_with(cx, |reader, cx| Readouts {
        chapter: reader.chapter_title().into(),
        position: position(reader, cx).into(),
        percent: chrome::percent_label(reader.progress()).into(),
    })
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

/// 一条带的壳。顶边那条贴屏幕顶、底边那条贴屏幕底，其余规格两端一致。
fn band(height: f32, at_top: bool, cx: &App) -> Div {
    let shell = h_flex()
        .absolute()
        .left_0()
        .right_0()
        .h(px(height))
        .px_4()
        .bg(cx.theme().background)
        .overflow_hidden();
    if at_top {
        shell.top_0()
    } else {
        shell.bottom_0().justify_between()
    }
}

/// 页眉：只有章名。空章名不占字，摆出来是一块干净的空白。
pub(in crate::ui::features::reader) fn header(
    reader: &Entity<ReaderSession>,
    cx: &App,
) -> AnyElement {
    let chapter = readouts(reader, cx).chapter;
    band(chrome::HEADER_HEIGHT, true, cx)
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .whitespace_nowrap()
                .when(!chapter.is_empty(), |label| label.child(chapter)),
        )
        .into_any_element()
}

/// 页脚：左边章内位置，右边进度。两端各占一条边。
pub(in crate::ui::features::reader) fn footer(
    reader: &Entity<ReaderSession>,
    cx: &App,
) -> AnyElement {
    let Readouts {
        position, percent, ..
    } = readouts(reader, cx);
    let muted = cx.theme().muted_foreground;
    let label = |text: SharedString| {
        div()
            .text_xs()
            .text_color(muted)
            .child(text)
            .into_any_element()
    };
    band(chrome::FOOTER_HEIGHT, false, cx)
        .child(label(position))
        .child(label(percent))
        .into_any_element()
}
