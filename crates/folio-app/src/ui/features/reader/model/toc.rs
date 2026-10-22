//! 目录：树的上抛、折叠与跳转。章节本身住在 [`crate::ui::features::reader::model::book::Book`] 里，
//! 这里把它连同文档自带的目录树（`TocEntry`）摊成界面能画的那几行，并接住「跳到
//! 某一条」这个动作。摊平、折叠、当前行、搜索那些**判断**在 [`crate::ui::features::reader::model::toc_tree`]，
//! 这一层只负责把它们接到会话上。

use std::collections::HashSet;

use folio_core::Locator;
use gpui_kit::{App, Context};

use crate::ui::features::reader::components::image_viewer::Place;
use crate::ui::features::reader::model::session::{ReaderSession, Source};
use crate::ui::features::reader::model::toc_tree::TocRow;
use crate::ui::services::l10n::tf;

impl ReaderSession {
    /// 章节数（兜底那一行没有章，是 0）。目录面板的列表长度。
    pub fn chapter_count(&self) -> usize {
        self.book().map_or(0, |book| book.chapter_count())
    }

    /// 整本书的目录，带层级。没有书（兜底那一行）时是空表。
    ///
    /// 定版式那一支也走这一问：PDF 的 `/Outlines` 是文档自带的书签，落点是**页号**
    /// （`TocEntry::page`），由 `toc_tree::build` 认那一格优先。漫画包没有书签，
    /// 回的是空表，目录那一屏在那些书上仍然没有行可画。
    pub fn toc_rows(&self) -> Vec<TocRow> {
        if let Some(book) = self.book() {
            return book.toc_rows();
        }
        match &self.source {
            Source::Fixed(fixed) => fixed.toc_rows(),
            _ => Vec::new(),
        }
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

    /// 一处命中摆在哪一段字上的那句话：可重排给章名，**定版式给「第几页 / 共几页」**。
    ///
    /// 为什么不能一律念章名：定版式没有章表，[`Self::chapter_title_at`] 回的是空串 ——
    /// 命中那一行就成了一个空摆位，而读者点那一行之前想认的恰恰是「这处在哪」。
    /// 那句读数与底栏、干净态页脚共用同一条键（`reader.page_position`）：同一件事
    /// 在两处念出两个名字，下一个人就不知道该改哪一条。
    pub(in crate::ui::features::reader) fn hit_place_at(&self, index: usize, cx: &App) -> String {
        if self.fixed().is_some() {
            return tf(
                cx,
                "reader.page_position",
                &[("index", &(index + 1)), ("total", &self.page_count())],
            );
        }
        self.chapter_title_at(index)
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
    /// 挂进恢复请求，排版做完那一帧再落地（见 [`crate::ui::features::reader::model::session::land_at_anchor`]）。
    pub fn goto_entry(&mut self, chapter: usize, fragment: Option<String>, cx: &mut Context<Self>) {
        // 定版式那一支的目录条落点是**页号**（`TocEntry::page`），走会话级翻页那一条
        // （`set_page`：改页表、清选中、落库、notify 都在里面），不走「换章 → 等排版
        // 完再落锚点」那一条 —— 那一族在这里既没有章表也没有锚点。
        // 锚点在定版式里也不适用：`format::pdf` 造的书签只说到页，`fragment` 永远是 `None`，
        // 而这一支压根没有「章内第几个字」那一根轴。
        if self.is_fixed() {
            let before = self.page();
            self.set_page(chapter, cx);
            if self.page() != before {
                // 与可重排那一条同一立场：跳转**当场**写盘。`set_page` 里那句
                // `persist_if_moved` 只改内存里的「上次写到哪儿」与「来过」集合，
                // 真往库里写的是这一句 —— 少了它，读者点完书签就杀进程，回来还在旧页。
                self.persist_locator(Locator {
                    chapter: chapter as u32,
                    offset: 0,
                });
            }
            return;
        }
        let prefs = self.prefs.clone();
        let moved = match &mut self.source {
            Source::Book(book) => book.goto(chapter, &prefs),
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
                self.goto_offset(chapter, offset, cx);
            }
        }
    }

    /// 跳到第 `chapter` 章的第 `offset` 个字（**章内可见文本**的字符数，与
    /// `Locator.offset`、`chapters.char_len` 同一把尺）。
    ///
    /// 「定位原文」那一处与正文全文检索的每一处命中的落点都折到这一句上：两者拿到的
    /// 都是「哪一章 + 第几个字」，而那个数都不必经过排版就有（前者来自导入那一趟扫出的
    /// `images_json`，后者来自 [`crate::search`]）。同一件事两处各写一遍，就会有一处
    /// 忘了 `reset_after_chapter` 或忘了落库 —— 症状是跳过去了、位置却又滑回章首。
    ///
    /// 落点照样不在这里算：目标章多半还没排过版，「第几个字落在哪一像素」要等排版做完，
    /// 由渲染帧恢复那一步落地（`view.rs` 里 `pending_restore` 那一支）。
    pub(in crate::ui::features::reader) fn goto_offset(
        &mut self,
        chapter: usize,
        offset: usize,
        cx: &mut Context<Self>,
    ) {
        let prefs = self.prefs.clone();
        // 定版式在这一处岔开：那一格里 `chapter` 就是页号，而位置在这一族**只有页** ——
        // 页内那几个字不进落库也不进进度（`resolve_position` 那条「定版式 offset 恒为 0」
        // 的口径，`LocatorKind::Page` 同一个说法）。所以命中的偏移在这里只是「去哪一页」
        // 的说明，不是落点：把它写进库里，进度条与「上次阅读的位置」就会读到一个
        // 谁也解释不了的数。
        let offset = match &mut self.source {
            Source::Book(book) => {
                book.goto(chapter, &prefs);
                offset
            }
            Source::Fixed(_) | Source::Sample(_) => 0,
        };
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
