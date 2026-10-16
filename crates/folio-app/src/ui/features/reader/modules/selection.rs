//! 选中（M6 一期，session-only）：长按吸词、指针拖选、两端把手、就地菜单条。
//!
//! 状态字段住在 [`ReaderSession`](super::super::session::ReaderSession) 上（与
//! `LaidOut` / `Source` 同一批），这一文件只有方法。选区存的是**章内可见文本坐标**
//! （`folio_core::content::anchor::TextMap` 那套「第几个字」），与进度、批注锚点同源：
//! 滚动不动它，重排不丢它，换章必须丢（坐标系换了）。
//!
//! 几何知识全部在引擎（`folio_core::content::hit`：命中、吸词、选区矩形），这里只做三件事：
//! 把屏幕点换算成排版点、把交互事件翻译成状态迁移、把「复制」落进剪贴板。
//! 交互规矩照搬 gpui-kit 的 `TextSelectionLayer` / `touch_selection`：
//!
//! - **长按**（触摸）：`LongPress` Started 吸一个词，手指继续拖就绕词锚点扩展
//!   （`capture_long_press` 后续 Moved/Ended 都归我们），松手成选、把手与菜单条出现。
//! - **指针**（桌面与触控板）：按下只记锚点，**拖过 4px 阈值才成立选中**——普通点击
//!   不留高亮；拖选松手的那一下 `ClickEvent` 由 `suppress_tap` 吃掉，否则
//!   `page::on_body_tap` 会把刚选的清掉。
//! - **把手**：松手后两端各留一个（杆 + 圆钮，44px 命中区），是长按之后**唯一**能改
//!   选中的路。拖动期间记「手指→插入符」偏移（圆钮长在行外，手指不压在文字上）；
//!   拖到另一端正上停住不塌缩；拖过另一端两端互换、手指改握新的那头；拖动期间
//!   菜单条让位、松手即回。
//! - **点按**：选中态下点选区外清空、点选区内把菜单条请回来，两下都消费这一击
//!   （不翻页）；没有选区时点在一条**已有的划线**上就是把那一条重新选中（判断与命中
//!   在 `modules/annotations.rs`）；浮层（顶栏/底栏）开着时指针让位——那一击的语义是收浮层。
//!
//! 绘制在 [`crate::ui::features::reader::paint`]（高亮矩形压在字下、把手压在字上，
//! 事件监听也注册在那份元素的 paint 里——gpui 的 `window.on_mouse_event` 只在
//! 绘制期内有效）与 `view`（菜单条是浮层里的真按钮）。

use std::ops::Range;

use gpui_kit::{App, ClipboardItem, Context, Pixels, Point, Window, point};

use super::super::book::Book;
use super::super::chrome;
use super::super::document::Document;
use super::super::session::{
    DRAG_THRESHOLD_PX, EdgeDrag, MouseDrag, ReaderSession, Selection, SelectionEdge,
};

impl ReaderSession {
    // ---- 查询：视图与 page.rs 用 ----

    /// 当前选中（挂在某一章上的章内字符坐标，含头不含尾）。
    pub(in crate::ui::features::reader) fn selection(&self) -> Option<Selection> {
        self.selection.clone()
    }

    /// 菜单条现在该不该摆出来。拖动期间收起（让位），松手即回；笔记那张便签压着时
    /// 也不摆 —— 两层同时上屏就是两套输入焦点在抢同一只手。
    pub fn selection_menu_open(&self) -> bool {
        self.selection_menu_open && self.edge_drag.is_none() && self.note_editor.is_none()
    }

    /// 浮层（顶栏/底栏）开合，由 `page.rs` 每帧报告。开着时指针让位：
    /// 那一击的语义是收浮层，不是选字。
    pub fn set_overlay_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.overlay_open != open {
            self.overlay_open = open;
            cx.notify();
        }
    }

    // ---- 长按（触摸）：gpui 的手势识别器 500ms 后送 LongPress ----

    /// 长按落词。返回 `false` = 这一下没落在字上，调用方别抢占手势。
    pub(in crate::ui::features::reader) fn begin_long_press(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        at: Point<Pixels>,
    ) -> bool {
        if self.overlay_open || self.note_editor.is_some() || self.chapter_turn_in_flight() {
            return false;
        }
        let Some((chapter, word)) = self.word_at(window, cx, at) else {
            return false;
        };
        self.long_press_anchor = Some((chapter, word.start));
        self.selection = Some(Selection {
            chapter,
            range: word,
        });
        self.selection_menu_open = false;
        self.mouse_drag = None;
        cx.notify();
        true
    }

    /// 长按之后手指继续拖：活动端绕词锚点扩展，反向拖两端互换。
    /// 拖出锚点所在的那一章就停住：章内坐标不能跨章比较。
    pub(in crate::ui::features::reader) fn drag_long_press(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        at: Point<Pixels>,
    ) {
        let Some((anchor_chapter, anchor)) = self.long_press_anchor else {
            return;
        };
        let Some((chapter, offset)) = self.offset_at(window, cx, at) else {
            return;
        };
        if chapter != anchor_chapter || offset == anchor {
            return; // 停在锚点上不塌缩
        }
        self.selection = Some(Selection {
            chapter: anchor_chapter,
            range: anchor.min(offset)..anchor.max(offset),
        });
        cx.notify();
    }

    /// 长按手势结束：成选就把菜单条请出来，拖塌了就当没选中过。
    pub(in crate::ui::features::reader) fn end_long_press(&mut self, cx: &mut Context<Self>) {
        self.long_press_anchor = None;
        let non_empty = self
            .selection
            .as_ref()
            .is_some_and(|selected| selected.range.start < selected.range.end);
        if non_empty {
            self.selection_menu_open = true;
        } else {
            self.clear_selection();
        }
        cx.notify();
    }

    // ---- 指针（桌面）：按下记锚点，拖过阈值才成选 ----

    /// 指针按下。把手命中就去拖把手，否则只记锚点——普通点击到此为止，
    /// 交给 click 的分区逻辑；拖过阈值才算「在选中」。
    pub(in crate::ui::features::reader) fn begin_mouse_selection(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        at: Point<Pixels>,
    ) {
        // 新的一手开始，上一手拖选攒下的豁免到此为止（正常情况下它已被
        // 拖选松手的那一下 click 消费掉了；没消费说明那一下没落在正文上，丢弃）。
        self.suppress_tap = false;
        if let Some(edge) = self.handle_at(at) {
            self.begin_edge_drag(edge, at);
            return;
        }
        if self.overlay_open || self.note_editor.is_some() || self.chapter_turn_in_flight() {
            return;
        }
        let Some((chapter, offset)) = self.offset_at(window, cx, at) else {
            return;
        };
        self.mouse_drag = Some(MouseDrag {
            chapter,
            anchor: offset,
            origin: at,
            dragged: false,
        });
    }

    /// 指针拖动：把手拖动优先；否则过了阈值就进入拖选，选区从锚点跟着手指走。
    /// 拖出起手的那一章就停：章内坐标不能跨章比较。
    pub(in crate::ui::features::reader) fn drag_mouse_selection(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        at: Point<Pixels>,
    ) {
        if self.edge_drag.is_some() {
            self.update_edge_drag(window, cx, at);
            return;
        }
        let Some(drag) = self.mouse_drag.as_ref() else {
            return;
        };
        let moved = (at - drag.origin).magnitude() > f64::from(DRAG_THRESHOLD_PX);
        if !moved {
            return;
        }
        let (chapter, anchor) = (drag.chapter, drag.anchor);
        let Some((hit_chapter, offset)) = self.offset_at(window, cx, at) else {
            return;
        };
        if hit_chapter != chapter || offset == anchor {
            return;
        }
        self.mouse_drag
            .as_mut()
            .expect("drag 在上面刚检查过")
            .dragged = true;
        self.selection = Some(Selection {
            chapter,
            range: anchor.min(offset)..anchor.max(offset),
        });
        self.selection_menu_open = false;
        cx.notify();
    }

    /// 指针松手。拖选成过才拦 click；普通点击什么都不做（留给分区逻辑）。
    pub(in crate::ui::features::reader) fn end_mouse_selection(&mut self, cx: &mut Context<Self>) {
        if self.edge_drag.is_some() {
            self.end_edge_drag(cx);
            return;
        }
        if let Some(drag) = self.mouse_drag.take()
            && drag.dragged
        {
            self.suppress_tap = true;
            self.selection_menu_open = true;
            cx.notify();
        }
    }

    // ---- 把手：长按之后唯一能改选中的路 ----

    /// 从把手处起拖。`finger` 是按下/起拖的位置，记下它与插入符的偏移——
    /// 圆钮长在行的上/下侧，手指不压在文字上，后续每帧用这个偏移还原「手指指的是哪个字」。
    pub(in crate::ui::features::reader) fn begin_edge_drag(
        &mut self,
        edge: SelectionEdge,
        finger: Point<Pixels>,
    ) {
        let Some(caret) = self.handle_center(edge) else {
            return;
        };
        self.selection_menu_open = false;
        self.mouse_drag = None;
        self.edge_drag = Some(EdgeDrag {
            edge,
            offset: caret - finger,
        });
    }

    /// 把手拖动：手指映射回文字位置后改被拖的那一端。
    /// 拖到另一端正上停住不塌缩；拖过另一端两端互换、手指改握新的那头；
    /// 拖出选区所在的那一章也停。
    pub(in crate::ui::features::reader) fn update_edge_drag(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        finger: Point<Pixels>,
    ) {
        let Some(drag) = self.edge_drag.as_ref() else {
            return;
        };
        let (edge, offset) = (drag.edge, drag.offset);
        let Some(selected) = self.selection.clone() else {
            return;
        };
        let at = point(offset.x + finger.x, offset.y + finger.y);
        let Some((hit_chapter, moving)) = self.offset_at(window, cx, at) else {
            return;
        };
        if hit_chapter != selected.chapter {
            return;
        }
        let fixed = match edge {
            SelectionEdge::Start => selected.range.end,
            SelectionEdge::End => selected.range.start,
        };
        if moving == fixed {
            return; // 正停在另一端上：不塌缩
        }
        let crossed = match edge {
            SelectionEdge::Start => moving > fixed,
            SelectionEdge::End => moving < fixed,
        };
        if crossed {
            // 拖过了另一端：互换，手指从此握的是新的那一头。
            self.edge_drag.as_mut().expect("drag 在上面刚检查过").edge = edge.opposite();
        }
        self.selection = Some(Selection {
            chapter: selected.chapter,
            range: moving.min(fixed)..moving.max(fixed),
        });
        cx.notify();
    }

    /// 把手松手：菜单条回到选区上方。这一下不产生任何 click 语义，
    /// 但保险起见照拖选一样拦掉紧跟着的那一下。
    pub(in crate::ui::features::reader) fn end_edge_drag(&mut self, cx: &mut Context<Self>) {
        if self.edge_drag.take().is_some() {
            self.suppress_tap = true;
            self.selection_menu_open = true;
            cx.notify();
        }
    }

    // ---- 点按（page.rs 的 on_body_tap 进来）----

    /// 拖选/把手拖动松手后，吃掉紧跟着的那一下 click。
    pub fn consume_tap_suppression(&mut self) -> bool {
        std::mem::take(&mut self.suppress_tap)
    }

    /// 选中态下正文上的一击。返回 `true` = 这一击被选中消费（点选区外清空、
    /// 点选区内把菜单条请回来），`page.rs` 不该再拿它去翻页。
    pub fn tap_with_selection(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        at: Point<Pixels>,
    ) -> bool {
        if self.taps_selection_ui(at) {
            return true; // 把手与菜单条自己处理，别当成「点别处」
        }
        let on_selection = self
            .offset_at(window, cx, at)
            .is_some_and(|(chapter, offset)| {
                self.selection.as_ref().is_some_and(|selected| {
                    selected.chapter == chapter && selected.range.contains(&offset)
                })
            });
        if on_selection {
            self.selection_menu_open = true;
        } else {
            self.clear_selection();
        }
        cx.notify();
        true
    }

    /// 正文上的一击要不要被「选中」这一套消费掉（消费了就不算点正文：不开浮层、不翻页）。
    ///
    /// 四条判据的**顺序**就是修的那个 bug 本身，所以它待在会话里而不是 `page.rs`：
    /// 只有在这里，「菜单刚被点掉、选区已经清空」那个窗口期才测得到。
    ///
    /// 1. 拖选 / 拖把手松手紧跟着的那一下 —— 纯粹是合成的 click，谁都不该理它。
    /// 2. 落在菜单或把手上 —— 菜单里那颗「划线」「取消」会在自己的 click 里就把选区
    ///    清掉，所以这条必须在「有没有选区」之前问，否则等这一下冒泡到正文时
    ///    `selection()` 已是 `None`，顶栏与底栏会跟着一起冒出来。
    /// 3. 有选区时交给 [`Self::tap_with_selection`]：点选区内请回菜单、点选区外清空。
    /// 4. 没选区时点在一条**已有的划线**上，就是把那一条重新选中
    ///    （[`Self::reselect_annotation`]）—— 留在书上的痕迹不能只读不能改。
    ///    这一条不分左中右三区：落在底色上的那一击归选中，两侧照常翻页。
    pub fn tap_on_body(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        at: Point<Pixels>,
    ) -> bool {
        // 点一下正文就是「人还在」的证据 —— 哪怕这一击只把浮层收起来、位置一点没动。
        self.note_reading_activity();
        // 笔记那张便签压着时，正文这一击归它自己的遮罩（正常情况下也到不了这里，
        // 遮罩是 `occlude` 的；这一句是防手势层与元素层差一帧的那一下）。
        if self.note_editor.is_some() || self.chapter_turn_in_flight() {
            return true;
        }
        if self.consume_tap_suppression() {
            return true;
        }
        if self.taps_selection_ui(at) {
            return true;
        }
        if self.selection().is_some() {
            return self.tap_with_selection(window, cx, at);
        }
        self.reselect_annotation(window, cx, at)
    }

    /// 这一击是否落在选中的交互件上（把手命中区或菜单条）。这些面上的一击
    /// 属于它们自己，不算「点别处清除」。
    pub(in crate::ui::features::reader) fn taps_selection_ui(&self, at: Point<Pixels>) -> bool {
        self.menu_bounds.get().is_some_and(|b| b.contains(&at))
            || self
                .handle_hits
                .borrow()
                .iter()
                .any(|(_, hit)| hit.contains(&at))
    }

    // ---- 命令（菜单条）----

    /// 复制所选文本进剪贴板。菜单条收起，高亮留着——读者复制完往往还要看清选了什么。
    pub fn copy_selection(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.selected_text() else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.selection_menu_open = false;
        cx.notify();
    }

    /// 当前选区里的字。复制与笔记的引文用同一个来源：两处念出来的必须是同一段。
    pub(in crate::ui::features::reader) fn selected_text(&self) -> Option<String> {
        let selected = self.selection.as_ref()?;
        let text = self
            .book()
            .and_then(|book| book.span_doc(selected.chapter))
            .map(|doc| doc.text_map().slice(selected.range.clone()))
            .unwrap_or_default();
        (!text.is_empty()).then_some(text)
    }

    /// 清空选中。菜单条、笔记编辑器与拖动临时态一并收掉。
    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.selection_menu_open = false;
        self.note_editor = None;
        self.long_press_anchor = None;
        self.mouse_drag = None;
        self.edge_drag = None;
    }

    // ---- 几何：屏幕点 ↔ 某一章的章内字符偏移 ----

    /// 屏幕点 → `(哪一章, 章内字符偏移)`。没有书 / 版心还没量过 / 那一点落进的章
    /// 还没排出版来，都是 `None`。
    pub(in crate::ui::features::reader) fn offset_at(
        &self,
        window: &Window,
        cx: &App,
        at: Point<Pixels>,
    ) -> Option<(usize, usize)> {
        let (chapter, x, y) = self.to_layout(window, at)?;
        let doc = self.book()?.span_doc(chapter)?;
        doc.hit_test(window, cx, x, y)
            .map(|hit| (chapter, hit.offset))
    }

    /// 屏幕点吸一个词（长按用）。命中不到字（纯图片行、空白）给 `None`。
    fn word_at(
        &self,
        window: &Window,
        cx: &App,
        at: Point<Pixels>,
    ) -> Option<(usize, Range<usize>)> {
        let (chapter, offset) = self.offset_at(window, cx, at)?;
        let word = self.book()?.span_doc(chapter)?.snap_word(offset);
        (word.start < word.end).then_some((chapter, word))
    }

    /// 窗口坐标 → `(哪一章, 排版坐标)`。绘制（`view.rs` 摆行）与命中
    /// （`folio_core::content::hit`）的换算互为逆运算，两头的偏移必须同源：连续滚动是
    /// 「全局 y − 章基线」，分页/双页是片的 `origin_y` 与栏位 `x`，两条带子的高度
    /// 都扣在同一个地方。
    pub(in crate::ui::features::reader) fn to_layout(
        &self,
        window: &Window,
        at: Point<Pixels>,
    ) -> Option<(usize, f32, f32)> {
        let origin = self.content_bounds.get().origin;
        let local = at - origin;
        let prefs = &self.prefs;
        if self.laid_out.mode.is_continuous() {
            let book = self.book()?;
            let gap = Book::chapter_gap(prefs);
            // 读**画出来的**那一个 y（`scroll_painted`）而不是 `scroll_y`：平移补间在走的
            // 那两百多毫秒里，屏上的字按画出来的位置摆，命中也按它算才对得上一个字段。
            let global_y = f32::from(local.y) - self.header_reserve + self.scroll_painted;
            let chapter = book.chapter_at_y(global_y, gap)?;
            let base = book.base_of(chapter, gap).unwrap_or(0.0);
            return Some((
                chapter,
                f32::from(local.x) - prefs.margin_x,
                global_y - base,
            ));
        }
        // 分页 / 双页：先按横坐标定栏位，再按那一栏的片定纵向原点。
        let gap = chrome::spread_gap(prefs.margin_x);
        let usable = Document::content_width(window, prefs, self.reserve.width);
        let width =
            folio_core::layout::fragment::column_width(usable, self.laid_out.columns, gap).max(1.0);
        // 平移翻页在走的时候整页偏了一段（`page_shift`，由渲染帧按当前页写入）。减掉它，
        // 两条轴才与 `view.rs` 烘进 `Scene::build` 的那两个加数互为逆运算。
        let (shift_x, shift_y) = self.page_shift;
        let column = if (f32::from(local.x) - prefs.margin_x - shift_x) >= width + gap / 2.0 {
            1
        } else {
            0
        };
        let book = self.book()?;
        let slice = book
            .document()
            .slice_of(self.page, column, self.laid_out.columns)?;
        let x = prefs.margin_x
            + folio_core::layout::fragment::column_offset(width, gap, slice.column)
            + shift_x;
        Some((
            book.chapter_index(),
            f32::from(local.x) - x,
            f32::from(local.y) - self.header_reserve - shift_y + slice.origin_y,
        ))
    }

    // ---- 把手命中 ----

    /// 这一帧里 `edge` 那端把手插入符的中心（窗口坐标）。paint 每帧写入
    /// [`ReaderSession::handle_hits`]，这里只读。
    fn handle_center(&self, edge: SelectionEdge) -> Option<Point<Pixels>> {
        self.handle_hits
            .borrow()
            .iter()
            .find(|(candidate, _)| *candidate == edge)
            .map(|(_, hit)| hit.center())
    }

    /// 落点在哪个把手的命中区里（44px，宁大勿小——拇指的命中下限）。
    /// 手势注册在 `paint.rs`，触摸拖动的 Started 也从这里问。
    pub(in crate::ui::features::reader) fn handle_at(
        &self,
        at: Point<Pixels>,
    ) -> Option<SelectionEdge> {
        self.handle_hits
            .borrow()
            .iter()
            .find(|(_, hit)| hit.contains(&at))
            .map(|(edge, _)| *edge)
    }
}
