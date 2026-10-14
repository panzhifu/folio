//! 脚注：点中一条引用（正文里的「注」图或上标数字）就把注文抬成一张小纸片。
//!
//! 弹层本体在 **阅读页**（`page::ReadingPage::footnote_popover`，用 gpui-kit 的
//! [`Popover`](gpui_kit::component::popover::Popover)），这里只负责三步：
//! 「这一击落在脚注上」「注文是这句」「锚点是这儿」（窗口坐标）。会话把这个状态写进
//! [`ReaderSession::footnote`](super::super::session::ReaderSession)，阅读页订阅了会话，
//! 于是下一帧把它画出来。
//!
//! 判定与取文在 [`super::super::document::Document::footnote_note_at`]：那一层认得
//! `epub:type="noteref"` / `<aside epub:type="footnote">`，也认得老书塞在标记图
//! `alt` / `zy-footnote` 里的注文。这一层只把它接到「屏幕上的一次点」。

use gpui_kit::{Context, Pixels, Point, Window};

use super::super::session::Footnote;
use super::super::session::ReaderSession;

impl ReaderSession {
    /// 这一帧要不要画脚注弹层（画在阅读页上）。`None` = 没点着脚注。
    pub fn footnote(&self) -> Option<&Footnote> {
        self.footnote.as_ref()
    }

    /// 收掉脚注弹层。点空白、按返回、再点一次同一个标记都走这里。
    pub fn dismiss_footnote(&mut self, cx: &mut Context<Self>) {
        if self.footnote.take().is_some() {
            cx.notify();
        }
    }

    /// 把一条注文抬成弹层（`anchor` 是标记在窗口坐标里的位置，弹层贴着它长出去）。
    pub fn show_footnote(&mut self, text: String, anchor: Point<Pixels>, cx: &mut Context<Self>) {
        self.footnote = Some(Footnote { text, anchor });
        cx.notify();
    }

    /// 屏幕点若落在一条脚注引用上就把注文抬出来，返回有没有被脚注吃掉这一击。
    ///
    /// 排在普通链接之前（见 `page::ReadingPage::on_body_tap`）：脚注标记本身就是一条
    /// `<a href="#...">`，但它要的是「就地看注文」，不是「跳到注文那一行」。注文取不到
    /// （目标不在本章、也没有 `alt` 兜底）时返回 `false`，那一击落回寻常的链接跳转。
    pub fn open_footnote_at(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        at: Point<Pixels>,
    ) -> bool {
        let Some((chapter, offset)) = self.offset_at(window, cx, at) else {
            return false;
        };
        let Some(text) = self
            .book()
            .and_then(|book| book.span_doc(chapter))
            .and_then(|doc| doc.footnote_note_at(offset))
        else {
            return false;
        };
        self.show_footnote(text, at, cx);
        true
    }
}
