//! 位置：滚动/页号 ↔ 可见文本偏移的换算、进度恢复、以及把位置写回书库。
//!
//! 整套设计的前提是**进度落在「第几个字」上**（见 `plans/README.md` 架构决策 ②）：
//! 换字号、缩窗口、换摆法都不动位置，页号与像素只是那一个字的读数方式。所以这里
//! 只有一个真源（[`ReaderSession::resolve_position`]），其余都是它的正/反解。
//!
//! 连续滚动的坐标是**拼接窗口的全局 y**：视口顶落在哪一章（缝归后一章）就反解
//! 哪一章的章内偏移 —— 落库存出去的 `Locator { chapter, offset }` 与单章时代
//! 逐字节同形，恢复、书签、批注锚点都不用知道「拼接」的存在。

use folio_core::Locator;
use gpui_kit::Context;

use crate::ui::features::reader::model::book::Book;
use crate::ui::features::reader::model::session::{ReaderSession, Source};

impl ReaderSession {
    /// 当前视口顶的位置：**(哪一章, 章内第几个字)**。
    ///
    /// 连续滚动读视口顶所在章（全局坐标，[`Book::chapter_at_y`]）；分页 / 双页读
    /// 这一页第一栏的首行 —— 空片（作者强制断点补出来的）跳过，全空才给章首。
    /// 三种模式收敛到同一套坐标，进度与批注才对得上。
    pub(in crate::ui::features::reader) fn resolve_position(&self) -> Option<(usize, usize)> {
        // 定版式：`chapter` 就是页号、`offset` 恒为 0（`LocatorKind::Page` 那条口径）。
        if let Some(fixed) = self.fixed() {
            return Some((fixed.page(), 0));
        }
        let Source::Book(book) = &self.source else {
            return None;
        };
        if self.laid_out.mode.is_continuous() {
            let gap = Book::chapter_gap(&self.prefs);
            let chapter = book.chapter_at_y(self.scroll_y, gap)?;
            let base = book.base_of(chapter, gap).unwrap_or(0.0);
            let offset = book.span_doc(chapter)?.offset_at_y(self.scroll_y - base);
            return Some((chapter, offset));
        }
        let columns = self.laid_out.columns.max(1);
        for column in 0..columns {
            let Some(slice) = book.document().slice_of(self.page, column, columns) else {
                break;
            };
            if slice.empty {
                continue;
            }
            return Some((
                book.chapter_index(),
                book.document().coordinate_of_line(slice.lines.start),
            ));
        }
        Some((book.chapter_index(), 0))
    }

    /// 当前屏幕上顶那一条的可见文本偏移。宿主测试读。
    #[cfg(test)]
    pub(in crate::ui::features::reader) fn current_offset(&self) -> Option<usize> {
        self.resolve_position().map(|(_, offset)| offset)
    }

    /// 字偏移跨过了一行才记这一笔 —— 而这一笔**不落位置**，只记「人还在」与「到过这一章」。
    ///
    /// 位置那一趟是同步 SQLite 写。挂在输入路径上它平时 265 µs（中位），可一旦摊上 WAL
    /// checkpoint 或那条全库连接上的锁竞争就没有上界：2026-10-16 真机量到一趟
    /// **34.75 ms**，紧跟着一格走出邻格三倍的位移（80 px → 237 px），那就是读者说的
    /// 「停止滑动那一下发抖」。所以位置改到离席才结 —— [`ReaderSession::persist_position`]
    /// （退出阅读，`page::close` 调它）与外壳那一处「窗口失去焦点」（Android 一切后台，
    /// 进程随时可能被回收，那是最后一个可靠的机会）。
    ///
    /// 留在这里的两笔都不该等：「人还在」是阅读时长那条宽限线的证据点，等一下少一段；
    /// 而「到过哪一章」每章最多打一次库（判据是集合的 `insert`），目录那一屏的浅色按它画，
    /// 等到退出才亮就晚了。
    pub(in crate::ui::features::reader) fn persist_if_moved(&mut self) {
        let Some((chapter, offset)) = self.resolve_position() else {
            return;
        };
        if chapter == self.saved_chapter && offset == self.saved_offset {
            return;
        }
        self.note_reading_activity();
        self.saved_chapter = chapter;
        self.saved_offset = offset;
        self.mark_visited(chapter as u32);
    }

    /// 当前这一章的位置写进书库。没有 `book_id`（库打不开）时静默跳过 ——
    /// 进度落盘失败不该让阅读停下来。
    pub fn persist_position(&mut self) {
        let Some((chapter, offset)) = self.resolve_position() else {
            return;
        };
        self.persist_locator(Locator {
            chapter: chapter as u32,
            offset,
        });
    }

    /// 把一个已知的位置写进书库（换章后的章首、恢复落点、目录跳转、离席结账都用它）：
    /// 那些时刻拼接链还没就位，反解不出「当前」位置，只能直接给。
    ///
    /// 这是**唯一**落位置那一行的地方，而它只在离散动作上被调到。滚动那条路走
    /// [`ReaderSession::persist_if_moved`]，那一趟不写这里。
    pub(in crate::ui::features::reader) fn persist_locator(&mut self, locator: Locator) {
        // 「位置动过一下」就是「人还在」的证据，阅读时长的宽限线按它算
        // （见 `modules/reading_time`）。放在这里而不是各个输入口：滚、翻、跳、
        // 拖进度条最终都要落这一条。
        self.note_reading_activity();
        self.saved_chapter = locator.chapter as usize;
        self.saved_offset = locator.offset;
        self.mark_visited(locator.chapter);
        let (Some(store), Some(id)) = (&self.store, self.book_id) else {
            return;
        };
        // 默认档不落库：那一档是渲染帧按窗口宽算出来的，不是读者的选择。写进去
        // 就等于替他把这本书钉死，换到窄设备只能被 `effective` 降级，回不到连续滚动。
        let mode = if self.mode_undecided {
            String::new()
        } else {
            self.laid_out.mode.storage_name().to_string()
        };
        // ── 临时探针：写库这一趟的耗时，含抢那条全库连接互斥锁的等待（量完摘）──
        let __write = std::time::Instant::now();
        let written = store.set_position(id, &mode, &locator);
        crate::ui::features::reader::model::perf::scroll_note_write(
            __write.elapsed().as_micros() as u64
        );
        if let Err(err) = written {
            tracing::warn!(
                book_id = id,
                chapter = locator.chapter,
                error = %err,
                "persist_position: 写入失败"
            );
        }
    }

    /// 「到过这一章」。集合的 `insert` 返回是不是新入，所以「每跨过一行记一次」不会
    /// 变成「每跨过一行打一次库」。目录那一屏的浅色按它画。
    fn mark_visited(&mut self, chapter: u32) {
        let (Some(store), Some(id)) = (&self.store, self.book_id) else {
            return;
        };
        if self.read.insert(chapter as usize)
            && let Err(err) = store.mark_chapter_read(id, chapter as usize)
        {
            tracing::warn!(
                book_id = id,
                chapter,
                error = %err,
                "mark_chapter_read: 写入失败"
            );
        }
    }

    /// 跳到**锚章**内第 `offset` 个字所在的那一屏。目录跳转、书签、恢复进度共用这一个入口。
    pub fn jump_to_offset(&mut self, offset: usize, cx: &mut Context<Self>) {
        // 定版式没有「章内第几个字」这个坐标：位置按页号走，恢复在渲染帧里直接落页。
        if self.is_fixed() {
            return;
        }
        let gap = Book::chapter_gap(&self.prefs);
        let Source::Book(book) = &mut self.source else {
            return;
        };
        if self.laid_out.mode.is_continuous() {
            // 全局 y = 锚章基线 + 章内行顶。锚章还没进链（没排完）时基线是 0，
            // 与「排版没做过就回章首」的既有兜底一致。
            let base = book.base_of(book.chapter_index(), gap).unwrap_or(0.0);
            self.scroll_y = base + book.document().y_at_offset(offset);
        } else {
            let line = book.document().line_at_offset(offset);
            self.page = book
                .document()
                .page_of_line(line)
                .min(self.laid_out.pages.saturating_sub(1));
        }
        self.persist_position();
        cx.notify();
    }
}
