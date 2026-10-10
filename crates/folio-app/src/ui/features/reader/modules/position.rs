//! 位置：滚动/页号 ↔ 可见文本偏移的换算、进度恢复、以及把位置写回书库。
//!
//! 整套设计的前提是**进度落在「第几个字」上**（见 `plans/README.md` 架构决策 ②）：
//! 换字号、缩窗口、换摆法都不动位置，页号与像素只是那一个字的读数方式。所以这里
//! 只有一个真源（`current_offset`），其余都是它的正/反解。

use folio_core::Locator;
use gpui_kit::Context;

use super::super::session::{ReaderSession, Source};

impl ReaderSession {
    /// 当前屏幕上顶那一条的可见文本偏移。
    ///
    /// 连续滚动读视口顶；分页 / 双页读这一页第一栏的首行 —— 空片（作者强制断点补出来的）
    /// 跳过，全空才给章首。三种模式收敛到同一个数，进度与批注才在同一套坐标上。
    pub(in crate::ui::features::reader) fn current_offset(&self) -> Option<usize> {
        let Source::Book(book) = &self.source else {
            return None;
        };
        if self.laid_out.mode.is_continuous() {
            return Some(book.document().offset_at_y(self.scroll_y));
        }
        let columns = self.laid_out.columns.max(1);
        for column in 0..columns {
            let Some(slice) = book.document().slice_of(self.page, column, columns) else {
                break;
            };
            if slice.empty {
                continue;
            }
            return Some(book.document().coordinate_of_line(slice.lines.start));
        }
        Some(0)
    }

    /// 字偏移跨过了一行才写盘：进度不是「滚到第几个像素」而是「读到第几个字」，
    /// 同一个字里再怎么滑都不必写盘。杀进程回来还在同一行，靠的就是这一步。
    pub(super) fn persist_if_moved(&mut self) {
        if self
            .current_offset()
            .is_some_and(|offset| offset != self.saved_offset)
        {
            self.persist_position();
        }
    }

    /// 当前这一章的位置写进书库。没有 `book_id`（库打不开）时静默跳过 ——
    /// 进度落盘失败不该让阅读停下来。
    pub fn persist_position(&mut self) {
        let (Some(store), Some(id)) = (&self.store, self.book_id) else {
            return;
        };
        let locator = Locator {
            chapter: self.chapter_index() as u32,
            offset: self.current_offset().unwrap_or_default(),
        };
        self.saved_offset = locator.offset;
        let mode = self.laid_out.mode.storage_name();
        if let Err(err) = store.set_position(id, mode, &locator) {
            log::warn!("persist_position: 写入失败 {err}");
        }
    }

    /// 跳到章内第 `offset` 个字所在的那一屏。目录跳转、书签、恢复进度共用这一个入口。
    pub fn jump_to_offset(&mut self, offset: usize, cx: &mut Context<Self>) {
        let Source::Book(book) = &self.source else {
            return;
        };
        let line = book.document().line_at_offset(offset);
        if self.laid_out.mode.is_continuous() {
            self.scroll_y = book.document().y_at_offset(offset);
        } else {
            self.page = book
                .document()
                .page_of_line(line)
                .min(self.laid_out.pages.saturating_sub(1));
        }
        self.persist_position();
        cx.notify();
    }
}
