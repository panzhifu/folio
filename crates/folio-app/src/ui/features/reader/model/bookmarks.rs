//! 书签：把「读到这儿」钉成一个可跳回的位置，以及两处列表共用的删除与撤销。
//!
//! 与批注分家（存储层见 `folio_core::store::bookmarks` 的模块文档），这一层只管三件事：
//!
//! - **钉 / 拔**：顶栏那颗开关。可重排书锚住当前位置所在的**那一行** —— 锚点有了
//!   指纹才找得回（重排、改字号都不动它），列表里也有一句原文可显；定版式锚成点，
//!   「哪一页」就是位置的完整地址，没有字层的页（扫描页、漫画页）照样钉得上。
//! - **列表删除**：目录面板「批注」那一格与阅读侧的删除命令都汇到
//!   [`ReaderSession::remove_annotation`] / [`ReaderSession::remove_bookmark`]，
//!   两处共享同一条「删 → 暂存 → 给撤销」的路。
//! - **撤销**：[`ReaderSession::undo_delete`]。胶囊本体由阅读页渲染（`regions.rs`
//!   之外的一小块，挂在 `reading_page.rs`），状态与计时都在会话里 —— 会话是
//!   「删」这个动作的唯一现场。
//!
//! 正文上不为书签画狗耳：位置痕迹的可见性由列表屏与顶栏开关承担，往字形边上叠
//! 装饰是绘制那一层的另一个决定。

use folio_core::content::anchor::{TextAnchor, line_at_offset, line_chars, relocate};
use gpui_kit::{Context, SharedString};

use crate::ui::features::reader::model::session::ReaderSession;
use crate::ui::features::reader::model::undo::{DeletedRow, UndoSlot, UNDO_TTL};
use crate::ui::services::l10n::tr;

impl ReaderSession {
    /// 把库里的书签原样读回一份。与 [`ReaderSession::load_annotations`] 同一条立场：
    /// 重读而不是手工往内存里追一条，id、时间戳都以库里为准。
    pub(in crate::ui::features::reader) fn load_bookmarks(&mut self) {
        let read = match (self.book_id, self.store.as_ref()) {
            (Some(book_id), Some(store)) => store.bookmarks(book_id),
            _ => Ok(Vec::new()),
        };
        match read {
            Ok(rows) => self.bookmarks = rows,
            Err(err) => {
                tracing::warn!(book_id = ?self.book_id, error = %err, "书签: 读列表失败");
                self.bookmarks = Vec::new();
            }
        }
    }

    /// 当前位置钉在哪一枚书签上。判据是**落回当前坐标**的区间盖住当前位置：
    /// 书签锚的是一行，读者停在那一行的哪个字上都算钉着；重排之后按指纹找回了
    /// 新位置，这条判据照常成立。
    pub(in crate::ui::features::reader) fn bookmark_at_position(&self) -> Option<i64> {
        if self.is_fixed() {
            // 定版式：`chapter` 就是页号，位置只有页 —— 同页即同签。
            let page = self.page();
            return self
                .bookmarks
                .iter()
                .find(|b| b.chapter as usize == page)
                .map(|b| b.id);
        }
        let (chapter, offset) = self.position_in_chapter()?;
        let doc = self.book()?.span_doc(chapter)?;
        if !doc.is_current() {
            return None;
        }
        let map = doc.text_map();
        self.bookmarks
            .iter()
            .filter(|b| b.chapter as usize == chapter)
            .find(|b| {
                let at = relocate(&b.anchor, map);
                at.start <= offset && offset < at.end
            })
            .map(|b| b.id)
    }

    /// 顶栏那颗书签开关的选中态。
    pub fn position_bookmarked(&self) -> bool {
        self.bookmark_at_position().is_some()
    }

    /// 当前位置 → 书签锚点。定版式点锚；可重排锚整行（见模块文档）。
    fn position_anchor(&self) -> Option<TextAnchor> {
        if self.is_fixed() {
            return Some(TextAnchor::point(self.page() as u32, 0));
        }
        let (chapter, offset) = self.position_in_chapter()?;
        let doc = self.book()?.span_doc(chapter)?;
        // 排版没跟上的那一帧，行盒与坐标表都是旧的：这时钉会把书签钉错行。
        if !doc.is_current() {
            return None;
        }
        let line = line_at_offset(doc.layout(), doc.dom(), doc.text_map(), offset);
        let range = line_chars(doc.layout(), doc.dom(), doc.text_map(), line)?;
        Some(TextAnchor::new(
            doc.text_map(),
            chapter as u32,
            range.start,
            range.end,
        ))
    }

    /// 顶栏那颗开关：没钉就钉，钉了就拔（拔走撤销那条路）。
    pub fn toggle_bookmark(&mut self, cx: &mut Context<Self>) {
        let (Some(book_id), Some(store)) = (self.book_id, self.store.clone()) else {
            tracing::warn!(book_id = ?self.book_id, "书签: 没登记进书库（没有 book_id 或书库没装），钉不了");
            cx.notify();
            return;
        };
        if let Some(id) = self.bookmark_at_position() {
            self.remove_bookmark(id, cx);
            return;
        }
        let Some(anchor) = self.position_anchor() else {
            tracing::warn!(
                book_id = ?self.book_id,
                "书签: 排版还没跟上，这一下没钉上"
            );
            cx.notify();
            return;
        };
        match store.add_bookmark(book_id, &anchor) {
            Ok(id) => {
                tracing::info!(
                    book_id,
                    chapter = anchor.chapter,
                    bookmark_id = id,
                    "书签: 钉一枚"
                );
                self.load_bookmarks();
            }
            Err(err) => tracing::error!(book_id, error = %err, "书签: 写库失败"),
        }
        cx.notify();
    }

    /// 删一条批注（列表屏与「取消划线」共用）。返回 `true` = 真的删了，
    /// 调用方该收掉屏上还指着它的一切。
    pub fn remove_annotation(&mut self, id: i64, cx: &mut Context<Self>) -> bool {
        let Some(store) = self.store.as_ref() else {
            tracing::warn!(annotation_id = id, "划线: 书库没装，那一条删不掉");
            cx.notify();
            return false;
        };
        let Some(row) = store.annotation(id).ok().flatten() else {
            tracing::warn!(annotation_id = id, "划线: 那一条已经不在库里");
            return false;
        };
        match store.delete_annotation(id) {
            Ok(()) => {
                tracing::info!(annotation_id = id, "划线: 删一条批注");
                let label = tr(cx, "reader.undo.annotation");
                self.stash_undo(label, DeletedRow::Annotation(row), cx);
                self.load_annotations();
                cx.notify();
                true
            }
            Err(err) => {
                tracing::error!(annotation_id = id, error = %err, "划线: 删库失败");
                false
            }
        }
    }

    /// 拔一枚书签（顶栏开关与列表屏共用），走撤销同一条路。
    pub fn remove_bookmark(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(store) = self.store.as_ref() else {
            tracing::warn!(bookmark_id = id, "书签: 书库没装，那一枚拔不掉");
            cx.notify();
            return;
        };
        let Some(row) = store.bookmark(id).ok().flatten() else {
            tracing::warn!(bookmark_id = id, "书签: 那一枚已经不在库里");
            return;
        };
        match store.delete_bookmark(id) {
            Ok(()) => {
                tracing::info!(bookmark_id = id, "书签: 拔一枚");
                let label = tr(cx, "reader.undo.bookmark");
                self.stash_undo(label, DeletedRow::Bookmark(row), cx);
                self.load_bookmarks();
            }
            Err(err) => tracing::error!(bookmark_id = id, error = %err, "书签: 删库失败"),
        }
        cx.notify();
    }

    /// 撤销最近一次删除：按原行插回（id 与时间戳都不变），列表随之重读。
    pub fn undo_delete(&mut self, cx: &mut Context<Self>) {
        let Some(slot) = self.undo.take() else {
            return;
        };
        self.undo_seq += 1; // 作废在飞的到点计时：胶囊已经收了。
        let Some(store) = self.store.clone() else {
            return;
        };
        let outcome = match &slot.row {
            DeletedRow::Annotation(row) => {
                let ok = store.restore_annotation(row).is_ok();
                if ok {
                    self.load_annotations();
                }
                ok
            }
            DeletedRow::Bookmark(row) => {
                let ok = store.restore_bookmark(row).is_ok();
                if ok {
                    self.load_bookmarks();
                }
                ok
            }
        };
        if outcome {
            tracing::info!("撤销: 恢复一条被删的记录");
        } else {
            tracing::error!("撤销: 写库失败，那一行没回来");
        }
        cx.notify();
    }

    /// 把刚删的那一行暂存进胶囊，并起一趟到点自动收起的计时。
    fn stash_undo(&mut self, label: SharedString, row: DeletedRow, cx: &mut Context<Self>) {
        self.undo_seq += 1;
        let seq = self.undo_seq;
        let timer = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(UNDO_TTL).await;
            let _ = this.update(cx, |this, cx| {
                if this.undo.as_ref().is_some_and(|slot| slot.seq == seq) {
                    this.undo = None;
                    cx.notify();
                }
            });
        });
        self.undo = Some(UndoSlot {
            label,
            row,
            seq,
            _timer: Some(timer),
        });
    }
}
