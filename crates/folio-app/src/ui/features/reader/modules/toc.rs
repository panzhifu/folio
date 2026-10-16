//! 目录：树的上抛、折叠与跳转。章节本身住在 [`super::super::book::Book`] 里，
//! 这里把它连同文档自带的目录树（`TocEntry`）摊成界面能画的那几行，并接住「跳到
//! 某一条」这个动作。摊平、折叠、当前行、搜索那些**判断**在 [`super::super::toc_tree`]，
//! 这一层只负责把它们接到会话上。

use std::collections::HashSet;

use folio_core::Locator;
use gpui_kit::{App, Context};

use super::super::image_viewer::Place;
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

    /// 抹掉「来过」的那份记录。
    ///
    /// 那几行灰字是读者自己翻出来的痕迹，他要一次清干净、重新看一遍读到哪儿了。
    /// 只清这一份内存里的集合，不动落库的位置 —— 回到这本书仍然落在原来那一章。
    pub fn clear_read_marks(&mut self, cx: &mut Context<Self>) {
        self.read.clear();
        cx.notify();
    }

    /// 当前章的下标。
    pub fn chapter_index(&self) -> usize {
        self.book().map_or(0, |book| book.chapter_index())
    }

    /// **(哪一章, 章内第几个字)**：目录那一屏要靠它挑出「读者正处在第几小节下面」。
    /// 与进度、批注读的是同一套坐标（`modules/position.rs` 的 `resolve_position`）。
    pub fn position_in_chapter(&self) -> Option<(usize, usize)> {
        self.resolve_position()
    }

    /// 当前那一章的**锚点偏移表**（`id` → 章内第几个字）。排版产物，所以只能在
    /// 那一章排完之后问（见 `Document::id_offsets`）。没有书时给空表。
    pub fn id_offsets(&self) -> std::collections::HashMap<String, usize> {
        self.book()
            .map(|book| book.document().id_offsets())
            .unwrap_or_default()
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
            // 定版式没有目录条（那一屏回退成页表），跳目录不适用。
            Source::Sample(_) | Source::Fixed(_) => false,
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

    /// 「定位原文」：跳到整本书那一列图里的第 `index` 张**出现的地方**。
    ///
    /// 与点目录那一条走的是同一条路（换章 → 位置归零 → 挂落位请求 → 落库），两处不同：
    ///
    /// - 请求带的是那一处图片的**字符偏移**而不是章首。那一格是导入那一趟扫出来的
    ///   （`chapters.images_json` → `content::scan::ImageUse`），与 `Locator.offset`
    ///   同一根轴，所以不必经过排版就有位置 —— 这正是这一跳能成立的前提。
    /// - 目标就在当前这一章时**也要**落位。目录那条在「既没换章、又没锚点」时会直接
    ///   说不，而这里要的恰恰是「挪到那一行」，同一章里跳是常见的那一种。
    ///
    /// 落点不在这里算：目标章往往还没排过版，「第几个字落在哪一像素」要等排版做完，
    /// 由渲染帧恢复那一步落地（`view.rs` 里 `pending_restore` 那一支）。
    ///
    /// 先把查看器关掉 —— 「定位原文」的意思是回到正文看它出现在哪儿，压着一层黑
    /// 看那张图自己不算回去。
    pub fn locate_image(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(place) = self.image_sequence.get(index).map(|slot| slot.place) else {
            // 列里没有这一条（换了书、或这一列本来就是空的）：除了关掉这一层，没地方可跳。
            self.close_image_viewer(cx);
            return;
        };
        self.close_image_viewer(cx);
        match place {
            Place::Fixed { page } => {
                // 定版式那一支：那一张图**就是**那一页，跳到第几页就是定位原文。
                self.set_page(page, cx);
            }
            Place::Reflow { chapter, offset } => {
                let prefs = self.prefs.clone();
                if let Source::Book(book) = &mut self.source {
                    book.goto(chapter, &prefs);
                }
                // 换章共用的那一段：滚动与页号归零、作废上一趟没落地的跳转、丢选中。
                self.reset_after_chapter();
                let locator = Locator {
                    chapter: chapter as u32,
                    offset,
                };
                self.pend_restore(Some(locator));
                // 拼接链此刻还没就位，反解不出「当前」位置 —— 落库直接写已知的目标。
                // 偏移随后由正常的滚动补准（与跳目录那一条同一个写法）。
                self.persist_locator(locator);
                cx.notify();
            }
        }
    }

    /// 换一章：作废滚动位置并把新位置落库。
    ///
    /// 换章后 `scroll_y` 与页号都归零，于是落库的章内偏移也就是新章的开头 —— 这是对的，
    /// 不是妥协：读者要求的就是从新的一章开始读。
    pub fn advance_chapter(&mut self, delta: isize, cx: &mut Context<Self>) {
        let prefs = self.prefs.clone();
        let moved = match &mut self.source {
            Source::Book(book) => book.advance(delta, &prefs),
            // 定版式的翻页走 `scroll_page`（按页号），这里不适用。
            Source::Sample(_) | Source::Fixed(_) => false,
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
        // 换章是整条参照系换掉：正在平移的那一张属于上一章，不该跟着跨章飞过来。
        self.cancel_turn();
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
