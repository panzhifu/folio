//! 目录：树的上抛、折叠与跳转。章节本身住在 [`super::super::book::Book`] 里，
//! 这里把它连同文档自带的目录树（`TocEntry`）摊成界面能画的那几行，并接住「跳到
//! 某一条」这个动作。摊平、折叠、当前行、搜索那些**判断**在 [`super::super::toc_tree`]，
//! 这一层只负责把它们接到会话上。

use std::collections::HashSet;

use folio_core::Locator;
use gpui_kit::{App, Context};

use super::super::session::{ReaderSession, Source};
use super::super::toc_tree::TocRow;
use crate::ui::shared::l10n::tf;

impl ReaderSession {
    /// 章节数（兜底那一行没有章，是 0）。目录面板的列表长度。
    pub fn chapter_count(&self) -> usize {
        self.book().map_or(0, |book| book.chapter_count())
    }

    /// 整本书的目录，带层级。没有书（兜底那一行）时是空表。
    pub fn toc_rows(&self) -> Vec<TocRow> {
        self.book().map(|book| book.toc_rows()).unwrap_or_default()
    }

    /// 到过的章。目录那一屏把来过的画成浅色，没来过的正常前景。
    pub fn read_chapters(&self) -> &HashSet<usize> {
        &self.read
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

    /// 跳到目录里的某一条：那一章，加上（可选）章内锚点 `id`。
    ///
    /// 锚点不在这里换算成章内偏移 —— 目标章往往还没排版，偏移表还不存在。带着 `id`
    /// 挂进恢复请求，排版做完那一帧再落地（见 [`super::super::session::land_at_anchor`]）。
    pub fn goto_entry(&mut self, chapter: usize, fragment: Option<String>, cx: &mut Context<Self>) {
        let prefs = self.prefs.clone();
        let moved = match &mut self.source {
            Source::Book(book) => book.goto(chapter, &prefs),
            Source::Sample(_) => false,
        };
        // 一个 spine 文件挂着好几条目录（一章对十个小节）时，跳到同一章的另一条不算
        // 「换章」，`goto` 会说不 —— 可落点照样要动。只有既没换章、又没锚点，才是
        // 真的什么都没要发生。
        if !moved && fragment.is_none() {
            return;
        }
        self.reset_after_chapter();
        self.restore_to_entry(chapter, fragment);
        // 拼接链此刻还没就位，反解不出「当前」位置 —— 落库直接写已知的目标章首。
        // 锚点那一截是第几个字要等排版做完才知道，由随后正常的滚动落库补上。
        self.persist_locator(Locator {
            chapter: chapter as u32,
            offset: 0,
        });
        cx.notify();
    }

    /// 挂一趟「跳到某章的某个锚点」的恢复请求。
    ///
    /// 这是唯一允许在 [`Self::pend_restore`] 之外写 `pending_restore` 的地方，
    /// 因为它俩必须一起写：请求与它要落的锚点不能来自两趟不同的跳转。
    fn restore_to_entry(&mut self, chapter: usize, fragment: Option<String>) {
        self.pending_anchor = fragment;
        self.pending_restore = Some(Locator {
            chapter: chapter as u32,
            offset: 0,
        });
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
        let target = self.chapter_index();
        self.reset_after_chapter();
        self.persist_locator(Locator {
            chapter: target as u32,
            offset: 0,
        });
        cx.notify();
    }

    /// 换章共用的那段：位置归零、作废恢复请求与上一帧的内容高。
    /// 选中一并丢：章内字符偏移是按章算的坐标系，换章后旧选区指向的是别人的字。
    ///
    /// 连续模式的落点是全局 y：章首未必拼在 0 的位置（上一章可能正占着上方），
    /// 所以挂一个**章首恢复请求**，锚章排完的那一帧把它落到正确的那一像素。
    fn reset_after_chapter(&mut self) {
        self.scroll_y = 0.0;
        self.page = 0;
        self.wheel_accum = 0.0;
        self.content_height = 0.0;
        // 换章这一跳没有锚点：上一趟跳转留下的那个由 `pend_restore` 一并作废。
        self.pend_restore(self.mode.is_continuous().then(|| Locator {
            chapter: self.chapter_index() as u32,
            offset: 0,
        }));
        self.clear_selection();
    }

    /// 拼进坐标窗口的章（含锚章）。测试读：窗口必须钉在锚章 ± 邻章半窗宽。
    #[cfg(test)]
    pub(in crate::ui::features::reader) fn span_window(&self) -> Vec<usize> {
        self.book()
            .map(|book| book.span_indices())
            .unwrap_or_default()
    }
}
