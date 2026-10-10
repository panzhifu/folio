//! 目录：章节表的查询与跳转。章节本身住在 [`super::super::book::Book`] 里，
//! 这里只是把它摊平成会话对界面承诺的那几个读法（第几章 / 共几章 / 各章标题 / 能不能翻）。

use gpui_kit::{App, Context};

use super::super::session::{ReaderSession, Source};
use crate::ui::shared::l10n::tf;

impl ReaderSession {
    /// 章节数（兜底那一行没有章，是 0）。目录面板的列表长度。
    pub fn chapter_count(&self) -> usize {
        self.book().map_or(0, |book| book.chapter_count())
    }

    /// 当前章的下标。
    pub fn chapter_index(&self) -> usize {
        self.book().map_or(0, |book| book.chapter_index())
    }

    /// 第 `index` 章的标题。越界是空串，不是 panic —— 列表在换章那一帧可能还按旧长度画。
    pub fn chapter_title_at(&self, index: usize) -> String {
        self.book()
            .map(|book| book.chapter_title_at(index))
            .unwrap_or_default()
    }

    /// 当前章的标题；兜底那一行没有章，返回空串。
    pub fn chapter_title(&self) -> String {
        self.book()
            .map(|book| book.chapter_title().to_string())
            .unwrap_or_default()
    }

    /// 「第几节 / 共几节」，比整本书名短得多。顶部那条章导航用它。
    pub fn chapter_position(&self, cx: &App) -> String {
        let Some(book) = self.book() else {
            return String::new();
        };
        let total = book.chapter_count();
        if total == 0 {
            return String::new();
        }
        tf(
            cx,
            "reader.chapter_position",
            &[("index", &(book.chapter_index() + 1)), ("total", &total)],
        )
    }

    /// 能不能往这个方向换章。按钮的 disabled 态由它决定，
    /// 而不是让用户点一下什么也没发生。
    pub fn can_advance(&self, delta: isize) -> bool {
        let Some(book) = self.book() else {
            return false;
        };
        let target = book.chapter_index() as isize + delta;
        (0..book.chapter_count() as isize).contains(&target)
    }

    /// 跳到第 `index` 章的章首。
    pub fn goto_chapter(&mut self, index: usize, cx: &mut Context<Self>) {
        let prefs = self.prefs.clone();
        let moved = match &mut self.source {
            Source::Book(book) => book.goto(index, &prefs),
            Source::Sample(_) => false,
        };
        if !moved {
            return;
        }
        self.reset_after_chapter();
        self.persist_position();
        cx.notify();
    }

    /// 换一章：作废滚动位置并把新位置落库。
    ///
    /// 换章后 `scroll_y` 与页号都归零，于是落库的章内偏移也就是新章的开头 —— 这是对的，
    /// 不是妥协：读者要求的就是从新的一章开始读。
    pub fn advance_chapter(&mut self, delta: isize, cx: &mut Context<Self>) {
        let prefs = self.prefs.clone();
        let moved = match &mut self.source {
            Source::Book(book) => book.advance(delta, &prefs),
            Source::Sample(_) => false,
        };
        if !moved {
            return;
        }
        self.reset_after_chapter();
        self.persist_position();
        cx.notify();
    }

    /// 换章共用的那段：位置归零、作废恢复请求与上一帧的内容高。
    fn reset_after_chapter(&mut self) {
        self.scroll_y = 0.0;
        self.page = 0;
        self.wheel_accum = 0.0;
        self.content_height = 0.0;
        self.pending_restore = None;
    }
}
