//! 划线（M7）：把选中的字落成一条批注（带颜色与画法），并把库里的批注解回本章坐标画出来。
//!
//! 两头各有一套坐标，中间只隔一次解锚：
//!
//! - **写出去**的是 [`TextAnchor`]：章下标 + 章内可见文本偏移 + 一份原文指纹
//!   （`folio_core::content::anchor`）。它进库，排版、改字号、换设备都不该动它。
//! - **画出来**的是「这一章现在第几个字摆在哪一像素」：那是**当代排版**才答得出的问题，
//!   所以每次画之前都要拿本章的 `TextMap` 把锚点重解一遍（`resolve_annotations`
//!   的四级降级：原偏移 → 同 DOM 节点 → ±400 字窗口 → 全章）。
//!
//! 解锚比整章扫一遍文本便宜不了多少，而渲染帧是每帧都来的 —— 所以按章缓存一份，
//! 键是章文本的哈希（`Document::chapter_hash`）：章文本没变，可见文本坐标就没变，
//! 上一帧解出来的位置照样作数。这里**不**用排版代数当键 —— 解出来的是第几个字到
//! 第几个字，字号、栏宽、片高都动不着它，用代数当键等于读者每动一次设置就重解整章。
//!
//! 解不回来的那一条（`is_stale()`）**不画**：宁可少一条痕迹，也不把读者的批注
//! 画到别的字上去。库里那条的 `stale` 标记照实回写，批注侧栏（后续里程碑）才有依据
//! 说「这条找不到了」。
//!
//! 画出来的那几条痕迹同时是**可以点回去的**：点在矩形上就是把那一条重新选中
//! （[`ReaderSession::annotation_at`]），于是留在书上的东西不只是读数，还能改 ——
//! 就地菜单跟着给出「取消划线」、三种画法与五色
//! （[`ReaderSession::selection_annotation`]）。
//! 判据用的是画出来的那些矩形而不是「离得最近的那个字」，理由在下面那条命中方法的注释里。

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use folio_core::content::anchor::TextMap;
use folio_core::content::annotation::{ResolvedAnnotation, resolve_annotations};
use folio_core::store::{ANNOTATION_COLORS, Annotation, AnnotationStyle};
use gpui_kit::component::Theme;
use gpui_kit::component::input::{InputEvent, TextareaState};
use gpui_kit::{App, AppContext as _, Context, Hsla, Pixels, Point, Window};

use crate::ui::features::reader::components::note_editor::NoteEditor;
use crate::ui::features::reader::components::selection;
use crate::ui::features::reader::model::fixed_book::FixedBook;
use crate::ui::features::reader::model::session::{AnnotationSpans, ReaderSession, Selection};
use crate::ui::services::config::ConfigHolder;
use crate::ui::services::l10n::tr;

/// 点「划线」时用的那一种颜色（读者没先点某一枚圆点）。清单在
/// `folio_core::store::ANNOTATION_COLORS`：amber 是其中那档黄 —— 传统划线色，
/// 也是参考图里给勾选标记的那一枚。
pub(in crate::ui::features::reader) const DEFAULT_COLOR: &str = "amber";

/// 挑中的选区**正好等于**库里某一条划线时要带回的三格。
///
/// 为什么不是 `(id, 颜色)` 那一对：存储层那条 `update_annotation` 是**整条覆盖**
/// （笔记 + 颜色 + 画法一起写），改其中任何一格都得把另两格的原值带回去。上一轮只有
/// 颜色一格可改时，`(id, color)` 勉强够；如今画法也能改，元组就开始漏带东西 ——
/// 漏带的那一格不会报错，只会悄悄退回默认档，而那正是「读者改了 A，B 没了」。
pub(in crate::ui::features::reader) struct MarkedRow {
    pub(in crate::ui::features::reader) id: i64,
    pub(in crate::ui::features::reader) color: String,
    pub(in crate::ui::features::reader) style: AnnotationStyle,
}

impl ReaderSession {
    /// 把库里的批注原样读回一份（还没解锚）。开书时一次，写入之后再来一次。
    ///
    /// 重读而不是手工往 `self.annotations` 里追一条：库里那条的 id、时间戳、
    /// 颜色规范化都是存储层算的，界面自己拼一份迟早和库里对不上。
    pub(in crate::ui::features::reader) fn load_annotations(&mut self) {
        let read = match (self.book_id, self.store.as_ref()) {
            (Some(book_id), Some(store)) => store.annotations(book_id),
            _ => Ok(Vec::new()),
        };
        match read {
            Ok(rows) => self.annotations = rows,
            Err(err) => {
                // 读不出批注不影响正文：这一屏照读，只是划线没了。
                tracing::warn!(
                    book_id = ?self.book_id,
                    error = %err,
                    "划线: 读批注失败"
                );
                self.annotations = Vec::new();
            }
        }
        self.annotation_spans.borrow_mut().clear();
    }

    /// 划线：把当前选区落成一条批注。颜色用默认那档、画法用配置里那一格
    /// （菜单里「划线」那一格按下去走的就是这条）。
    pub fn add_highlight(&mut self, cx: &mut Context<Self>) {
        let style = self.pending_style(cx);
        self.add_highlight_as(DEFAULT_COLOR, style, cx);
    }

    /// 划线，但指定颜色：读者在颜色那一行直接点某一枚圆点时走这条 ——
    /// 一段新选区点色即「就用这个色划线」，不必先按「划线」再换色。画法仍按配置里那一格。
    pub fn add_highlight_with(&mut self, color: &str, cx: &mut Context<Self>) {
        let style = self.pending_style(cx);
        self.add_highlight_as(color, style, cx);
    }

    /// 下一条该用哪种画法。挑中了库里某一条时调用方问的是**那一条**的画法
    /// （[`ReaderSession::selection_annotation`]），这一格只管「还没有条」的情形。
    ///
    /// 读的是全局配置而不是会话里再存一份：读者在设置里改过一次，这一趟阅读就该跟着变，
    /// 而会话自己那份会多出第二个「下一条用什么画法」的家。
    pub(in crate::ui::features::reader) fn pending_style(&self, cx: &App) -> AnnotationStyle {
        ConfigHolder::appearance(cx).highlight_style
    }

    /// 菜单里「划线方式」那三格按下去做什么。两件事按挑中的是不是库里已有的一条分岔：
    ///
    /// - **是**：改那一条的画法（连同它的笔记与颜色原样带回去），**不碰配置** ——
    ///   读者回头改一条老批注的方式，不该顺手把自己「下一条怎么划」那格改掉。
    /// - **不是**：把这一档记成配置里那一格（跨书持久，与设置里那一格同一个家），
    ///   并用它把当前这段划上。点色那五枚也是这条规矩（点色即划线），
    ///   所以菜单里那一行的两类点击读起来是同一件事。
    pub fn pick_style(&mut self, style: AnnotationStyle, cx: &mut Context<Self>) {
        if self.selection_annotation().is_some() {
            self.set_highlight_style(style, cx);
            return;
        }
        ConfigHolder::edit_book_appearance(cx, |appearance| appearance.highlight_style = style);
        self.add_highlight_as(DEFAULT_COLOR, style, cx);
    }

    /// 划线，颜色与画法都由调用方给。上面那三个入口最后都汇到这里，
    /// 所以「排版还没跟上就不落库」「落库后重读整表」「收掉选区」这三条只写一遍。
    ///
    /// 落不成只写日志，不把菜单条留在屏上装作还能划 —— 读者再点一次也不会有别的结果，
    /// 而收掉菜单至少告诉他「这一下按过了」。
    fn add_highlight_as(&mut self, color: &str, style: AnnotationStyle, cx: &mut Context<Self>) {
        let Some(selected) = self.selection.clone() else {
            return;
        };
        self.selection_menu_open = false;
        let Some(anchor) = self.anchor_for(&selected) else {
            tracing::warn!(
                book_id = ?self.book_id,
                chapter = selected.chapter + 1,
                "划线: 排版还没跟上，这一条没落库"
            );
            cx.notify();
            return;
        };
        let (Some(book_id), Some(store)) = (self.book_id, self.store.clone()) else {
            tracing::warn!(
                book_id = ?self.book_id,
                "划线: 没登记进书库（没有 book_id 或书库没装），批注没处落"
            );
            cx.notify();
            return;
        };
        match store.add_annotation(book_id, &anchor, "", color, style) {
            Ok(id) => {
                tracing::info!(
                    book_id,
                    chapter = selected.chapter + 1,
                    annotation_id = id,
                    color,
                    style = style.as_str(),
                    "划线: 落一条批注"
                );
                self.load_annotations();
                // 选区收掉：此刻屏上那条划线来自刚落的批注，不是来自「选中」。
                self.clear_selection();
            }
            Err(err) => tracing::error!(
                book_id,
                chapter = selected.chapter + 1,
                error = %err,
                "划线: 写库失败"
            ),
        }
        cx.notify();
    }

    /// 点在**画出来的**那条划线所在的行里：找回对应的一条批注 `(哪一章, 章内区间, 库里的 id)`。
    ///
    /// 判据是矩形，不是 `hit_test` 那个「最近的字」：后者的语义是给拖选收边的 —— 纵向夹到
    /// 最近的行、横向夹进该行的墨迹范围内（`folio_core::content::hit`），于是点在段距里、
    /// 末行之下的空白也会得到一个落在某条划线区间内的偏移。拿它当「压住了划线」，读者在
    /// 空白处的一击就会莫名把某条批注选中。这里要的是「点到哪条划线，选中哪一条」，所以
    /// 落点既要在命中那一行的**行盒**里，又要在横向落进那一段字画出来的那块矩形 ——
    /// 矩形出自同一个 `selection_rects_in_band`，与屏上那条划线同源，点得中所见。
    pub(in crate::ui::features::reader) fn annotation_at(
        &self,
        window: &Window,
        cx: &App,
        at: Point<Pixels>,
    ) -> Option<(usize, Range<usize>, i64)> {
        if let Some(book) = self.fixed() {
            return self.fixed_annotation_at(book, at);
        }
        let (chapter, x, y) = self.to_layout(window, at)?;
        let doc = self.book()?.span_doc(chapter)?;
        // 排版还没跟上的那一帧，行盒是旧的：按旧行盒算出的矩形对不上这一帧画出来的划线。
        if !doc.is_current() || self.annotations.is_empty() {
            return None;
        }
        let hit = doc.hit_test(window, cx, x, y)?;
        let line = doc.lines().get(hit.line)?;
        // 点落在不在这一行（列）里，问的是**推进轴**：横排量 y，竖排量物理 x。
        // 区间向 `Layout` 的 `flow_*` 要，别在这里各写一根轴。
        let layout = doc.layout();
        let vertical = layout.vertical();
        let start = layout.flow_start(line);
        let band = start..(start + layout.flow_thickness(line));
        let along_flow = if vertical { x } else { y };
        if !band.contains(&along_flow) {
            return None;
        }
        let spans = resolved_for(
            &self.annotation_spans,
            &self.annotations,
            chapter,
            doc.chapter_hash(),
            doc.text_map(),
        );
        spans
            .iter()
            .filter(|span| !span.is_stale())
            // 先按偏移筛掉绝大多数：一条批注的矩形只可能在它自己那几行上。
            .filter(|span| hit.offset >= span.start && hit.offset < span.end)
            .find(|span| {
                doc.selection_rects_in_band(window, cx, span.start..span.end, band.clone())
                    .iter()
                    // 矩形是物理的：横排看点在不在它那段 x 里，竖排看那段 y
                    //（那才是「沿字」的方向）。
                    .any(|rect| {
                        let (lo, hi, at) = if vertical {
                            (rect.y, rect.y + rect.height, y)
                        } else {
                            (rect.x, rect.x + rect.width, x)
                        };
                        at >= lo && at < hi
                    })
            })
            .map(|span| (chapter, span.start..span.end, span.id))
    }

    /// 定版式那一族：点在**画出来的**那块痕迹上才算挑中那一条。
    ///
    /// 判据与排版那一族同一条立场（上面那条注释写着为什么不能用「最近的字」）：
    /// 这一族的 `PageText::hit` 同样是给拖选收边用的，拿它判「压住了划线」，读者按在
    /// 页边空白上也会把某条批注选中。所以这里问的是落点在不在那一块矩形里，而矩形
    /// 与屏上那条痕迹出自同一个 `selection::page_boxes` —— 点得中所见。
    ///
    /// 矩形按**整块字盒**问，不按字下那两条细线：实线与波浪在屏上只占盒底那几像素，
    /// 但挑中的是「这一段字我划过」，拇指压在那段字的中间该算点中。排版那一族也是这么
    /// 判的（它问的是行盒那一段），两族因此是同一只手感。
    fn fixed_annotation_at(
        &self,
        book: &FixedBook,
        at: Point<Pixels>,
    ) -> Option<(usize, Range<usize>, i64)> {
        if self.annotations.is_empty() {
            return None;
        }
        let boxes = self.fixed_text_boxes.borrow();
        let local = at - self.content_bounds.get().origin;
        for (page, box_) in boxes.iter() {
            if !box_.contains(&local) {
                continue;
            }
            let Some(text) = book.page_text(*page) else {
                continue;
            };
            let Some(key) = book.page_text_key(*page) else {
                continue;
            };
            let map = text.text_map();
            let spans = resolved_for(&self.annotation_spans, &self.annotations, *page, key, &map);
            if let Some(span) = spans.iter().filter(|span| !span.is_stale()).find(|span| {
                selection::page_boxes(&text, span.start..span.end, *box_)
                    .iter()
                    .any(|rect| rect.contains(&local))
            }) {
                return Some((*page, span.start..span.end, span.id));
            }
        }
        None
    }

    /// 点在一条已有的划线上：把那一条重新选中，菜单条跟着回来。
    ///
    /// 返回 `true` = 这一击归「选中」这一套，`page.rs` 不该再拿它去翻页或开浮层。
    /// 浮层（顶栏/底栏/目录弹层）开着时让位 —— 与 `begin_mouse_selection` 同一条文：
    /// 那一击的语义是收浮层。
    pub(in crate::ui::features::reader) fn reselect_annotation(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        at: Point<Pixels>,
    ) -> bool {
        if self.overlay_open {
            return false;
        }
        let Some((chapter, range, _)) = self.annotation_at(window, cx, at) else {
            return false;
        };
        // 先走 `clear_selection` 那一趟：把上一手拖选/拖把手攒下的临时态一并作废，
        // 这一次是「挑中留在书上的那一条」，不是接着上一手拖。
        self.clear_selection();
        self.selection = Some(Selection { chapter, range });
        self.selection_menu_open = true;
        cx.notify();
        true
    }

    /// 当前选区**正好等于**库里某一条划线时，给回那一条的 id、颜色名与画法。
    ///
    /// 判据是区间相等，不是「这一击是从划线上点出来的」：读者拖过把手把范围改了，手里
    /// 拿的就是一段新选区（该再划一次），而不是原来那一条 —— 菜单跟着这个判断换格子。
    pub(in crate::ui::features::reader) fn selection_annotation(&self) -> Option<MarkedRow> {
        let selected = self.selection.as_ref()?;
        let spans = self.resolved_selection(selected)?;
        spans
            .iter()
            .find(|span| {
                !span.is_stale()
                    && span.start == selected.range.start
                    && span.end == selected.range.end
            })
            .map(|span| MarkedRow {
                id: span.id,
                color: span.color.clone(),
                style: span.style,
            })
    }

    /// 选区所在的那一章 / 那一页现在解出来的批注清单（与回显共用那一份缓存）。
    ///
    /// 两族只在**两格**上不同：坐标系从哪儿来（章的 DOM 文本节点 / 一页栅格出来的字序），
    /// 以及那份缓存的键（章文本的内容哈希 / 那一页字层的内容哈希）。「解出来的是第几个字
    /// 到第几个字」这条判据、四级降级、以及那条「解不回来就不算」的立场两侧同一条 ——
    /// 所以判据留调用方自己写，这里只把两族的来源对上。
    ///
    /// 定版式取不到字层（扫描页、漫画页）时交 `None`：那与「这一章还没解出批注」在屏上
    /// 是同一件事（挑中的就是一段没有痕迹的字）。
    fn resolved_selection(&self, selected: &Selection) -> Option<Rc<Vec<ResolvedAnnotation>>> {
        if let Some(book) = self.fixed() {
            let text = book.page_text(selected.chapter)?;
            let key = book.page_text_key(selected.chapter)?;
            let map = text.text_map();
            return Some(resolved_for(
                &self.annotation_spans,
                &self.annotations,
                selected.chapter,
                key,
                &map,
            ));
        }
        let doc = self.book()?.span_doc(selected.chapter)?;
        Some(resolved_for(
            &self.annotation_spans,
            &self.annotations,
            selected.chapter,
            doc.chapter_hash(),
            doc.text_map(),
        ))
    }

    /// 改库里那一条的**任何一格**之前，先把整条要带的另几格凑齐。
    ///
    /// 存储层那条 `update_annotation` 是整条覆盖（笔记 + 颜色 + 画法一起写），漏带的那一格
    /// 不报错，只是悄悄退回默认档 —— 于是「读者换了个颜色，他写的笔记没了」这种事在界面上
    /// 完全看不出来。三个改格子的动作（改笔记、换色、换画法）都从这里取另两格，
    /// 这条规矩就只有一处。
    fn marked_edit(&self, id: i64) -> Option<(String, String, AnnotationStyle)> {
        self.annotations
            .iter()
            .find(|row| row.id == id)
            .map(|row| (row.note.clone(), row.color.clone(), row.style))
    }

    /// 取消划线：把选区对应的那一条从库里删掉，画出来的那条划线随它消失。
    ///
    /// 删除本体在 [`ReaderSession::remove_annotation`]（列表屏的删除命令与这里是
    /// 同一条路，撤销也在那条路上）；这里只多管选区：删成了就收掉 —— 屏上已经
    /// 没东西可挑中了，留着一段「选中」只会让读者以为还能再删一次。
    pub fn delete_highlight(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.selection_annotation() else {
            return;
        };
        self.selection_menu_open = false;
        if self.remove_annotation(row.id, cx) {
            self.clear_selection();
        }
        cx.notify();
    }

    /// 换颜色：改库里那一条的 `color`，选区留着（读者往往还要连着比两档颜色）。
    ///
    /// 笔记与画法从 [`Self::marked_edit`] 原样带过去 —— 只当自己是改颜色而不带那两格，
    /// 就会把读者写过的笔记或选过的画法悄悄清空。
    pub fn set_highlight_color(&mut self, color: &str, cx: &mut Context<Self>) {
        let Some(row) = self.selection_annotation() else {
            return;
        };
        if row.color == color {
            return; // 点的就是当下那一格：一趟都不跑
        }
        let Some(store) = self.store.as_ref() else {
            tracing::warn!(annotation_id = row.id, "划线: 书库没装，颜色改不了");
            return;
        };
        let Some((note, _, style)) = self.marked_edit(row.id) else {
            return;
        };
        match store.update_annotation(row.id, &note, color, style) {
            Ok(()) => {
                tracing::info!(annotation_id = row.id, color, "划线: 换一条批注的颜色");
                self.load_annotations();
            }
            Err(err) => tracing::error!(
                annotation_id = row.id,
                error = %err,
                "划线: 改颜色写库失败"
            ),
        }
        cx.notify();
    }

    /// 换画法：改库里那一条怎么画（底色带 / 实线 / 波浪），选区留着。
    ///
    /// 与换颜色同一条语句、同一份「带齐另两格」的规矩。**不碰配置里那一格**：
    /// 回头改一条老批注的画法，不该顺手把「下一条怎么划」也改掉（那条岔路在
    /// [`ReaderSession::pick_style`]）。
    pub fn set_highlight_style(&mut self, style: AnnotationStyle, cx: &mut Context<Self>) {
        let Some(row) = self.selection_annotation() else {
            return;
        };
        if row.style == style {
            return; // 点的就是当下那一格：一趟都不跑
        }
        let Some(store) = self.store.as_ref() else {
            tracing::warn!(annotation_id = row.id, "划线: 书库没装，画法改不了");
            return;
        };
        let Some((note, color, _)) = self.marked_edit(row.id) else {
            return;
        };
        match store.update_annotation(row.id, &note, &color, style) {
            Ok(()) => {
                tracing::info!(
                    annotation_id = row.id,
                    style = style.as_str(),
                    "划线: 换一条批注的画法"
                );
                self.load_annotations();
            }
            Err(err) => tracing::error!(
                annotation_id = row.id,
                error = %err,
                "划线: 改画法写库失败"
            ),
        }
        cx.notify();
    }

    /// 这一条批注有没有文字笔记。菜单上那格图标换不换，按它决定。
    pub fn annotation_has_note(&self, id: i64) -> bool {
        self.annotations
            .iter()
            .any(|row| row.id == id && !row.note.trim().is_empty())
    }

    /// 就地为当前选区写笔记。挑中的正好是库里某条划线时，输入框里带出它原有的笔记；
    /// 否则是一张空白便签，保存那一刻才落一条新批注。
    ///
    /// 输入框在这里建而不是每帧建：`TextareaState` 握着光标、选区与撤销历史，
    /// 每帧新建会把读者刚打的字连同光标一起丢回第一帧。
    pub fn open_note_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selection.is_none() {
            return;
        }
        let existing = self.selection_annotation();
        let note = existing
            .as_ref()
            .and_then(|row| self.annotations.iter().find(|stored| stored.id == row.id))
            .map(|row| row.note.clone())
            .unwrap_or_default();
        // 引文只留开头一段：选了一整章也能一秒画完，`line_clamp(2)` 之外的部分本来也看不见。
        // 在这里取一次而不是每帧重切：便签压着时选区不会变。
        let excerpt: String = self
            .selected_text()
            .map(|text| {
                let mut chars = text.chars();
                let head: String = chars.by_ref().take(160).collect();
                if chars.next().is_some() {
                    format!("{head}…")
                } else {
                    head
                }
            })
            .unwrap_or_default();
        let placeholder = tr(cx, "reader.note_placeholder");
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(placeholder)
                .default_value(note)
        });
        // 输入一变就重画这一层：保存键在「新笔记还空着」时是灰的，得跟着框里的字翻。
        let watch = cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify());
        input.update(cx, |input, cx| input.focus(window, cx));
        self.note_editor = Some(NoteEditor {
            input,
            annotation: existing,
            excerpt: excerpt.into(),
            _watch: watch,
        });
        self.selection_menu_open = false;
        cx.notify();
    }

    /// 保存笔记。已有批注走 `update_annotation`（笔记连同原来的颜色与画法一起写回），
    /// 普通选区才新建一条 —— 两件事混成一条「先划再改」，只改笔记时会多落一条。
    pub fn save_note(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.note_editor.take() else {
            return;
        };
        let text = editor.input.read(cx).value().trim().to_string();
        let Some(selected) = self.selection.clone() else {
            cx.notify();
            return;
        };
        if let Some(row) = editor.annotation {
            let id = row.id;
            let Some(store) = self.store.clone() else {
                tracing::warn!(annotation_id = id, "笔记: 书库没装，这一条改不了");
                self.selection_menu_open = true;
                cx.notify();
                return;
            };
            // 颜色与画法从便签开那张时挑中的那一行带回去：这一句是整条覆盖，
            // 漏带哪一格哪一格就没。便签压着时菜单是收的（见 `selection_menu_open`），
            // 所以这两格在便签开着的这段路上不可能被别处改过，不必再回库里读一遍。
            match store.update_annotation(id, &text, &row.color, row.style) {
                Ok(()) => {
                    tracing::info!(annotation_id = id, "笔记: 改一条批注的笔记");
                    self.load_annotations();
                    self.clear_selection();
                }
                Err(err) => {
                    tracing::error!(annotation_id = id, error = %err, "笔记: 写库失败");
                    self.selection_menu_open = true;
                }
            }
            cx.notify();
            return;
        }
        // 新笔记空着等于什么都没发生：把菜单还给读者，让他改按「划线」或某枚颜色。
        if text.is_empty() {
            self.selection_menu_open = true;
            cx.notify();
            return;
        }
        let Some(anchor) = self.anchor_for(&selected) else {
            tracing::warn!(
                book_id = ?self.book_id,
                chapter = selected.chapter + 1,
                "笔记: 排版还没跟上，这一条没落库"
            );
            self.selection_menu_open = true;
            cx.notify();
            return;
        };
        let (Some(book_id), Some(store)) = (self.book_id, self.store.clone()) else {
            tracing::warn!(
                book_id = ?self.book_id,
                "笔记: 没登记进书库（没有 book_id 或书库没装），批注没处落"
            );
            self.selection_menu_open = true;
            cx.notify();
            return;
        };
        let style = self.pending_style(cx);
        match store.add_annotation(book_id, &anchor, &text, DEFAULT_COLOR, style) {
            Ok(id) => {
                tracing::info!(
                    book_id,
                    chapter = selected.chapter + 1,
                    annotation_id = id,
                    color = DEFAULT_COLOR,
                    style = style.as_str(),
                    "笔记: 落一条带笔记的批注"
                );
                self.load_annotations();
                self.clear_selection();
            }
            Err(err) => {
                tracing::error!(
                    book_id,
                    chapter = selected.chapter + 1,
                    error = %err,
                    "笔记: 写库失败"
                );
                self.selection_menu_open = true;
            }
        }
        cx.notify();
    }

    /// 取消笔记：收起这一层，选区与菜单原样回去。
    pub fn cancel_note_editor(&mut self, cx: &mut Context<Self>) {
        if self.note_editor.take().is_some() {
            self.selection_menu_open = self.selection.is_some();
            cx.notify();
        }
    }

    /// 选区 → 锚点。章下标与偏移都照选区自己那份，锚点里存的原文指纹由 `TextMap` 给。
    ///
    /// 定版式在这一处也岔开：那两格里「章」就是**页**、「章内偏移」就是那一页字序里的
    /// 第几个字（坐标系由 `PageText::text_map` 给，与排版那一族同形：数的是字，不数断行）。
    /// 这一族没有排版，于是也没有「排版还没跟上」的那一帧 —— 取到字层就是这一页此刻的
    /// 答案；取不到（扫描页、漫画页）就是这一条落不成，与那一族交 `None` 同一档。
    fn anchor_for(&self, selected: &Selection) -> Option<folio_core::content::anchor::TextAnchor> {
        if let Some(book) = self.fixed() {
            let text = book.page_text(selected.chapter)?;
            return Some(folio_core::content::anchor::TextAnchor::new(
                &text.text_map(),
                selected.chapter as u32,
                selected.range.start,
                selected.range.end,
            ));
        }
        let doc = self.book()?.span_doc(selected.chapter)?;
        // 排版没跟上的那一帧，`TextMap` 还是上一章的：这时落库会把批注钉错字。
        if !doc.is_current() {
            return None;
        }
        Some(folio_core::content::anchor::TextAnchor::new(
            doc.text_map(),
            selected.chapter as u32,
            selected.range.start,
            selected.range.end,
        ))
    }
}

/// 五种划线的实色，按 `folio_core::store::ANNOTATION_COLORS` 的顺序摆
/// （红 / 紫 / 蓝 / 绿 / 黄）。
///
/// 渲染帧开头取一次（`cx.theme()` 每帧问一遍不贵，但逐条画时查表更省事），
/// 画的时候按库里那个名字查。这里给出的**一律是实色**：菜单里那枚圆点是实心圆，
/// 字下那两条线（实线、波浪）也都要看得清。唯一压透明度的是**底色带**那一档，
/// 而那个数不住在这里 —— 它是绘制那一层的选择，见 `view/body.rs` 的 `HIGHLIGHT_ALPHA`
/// （整片铺在字下、不能把字吃掉，量的就是那一个 0.35）。
pub(in crate::ui::features::reader) struct Palette([Hsla; 5]);

impl Palette {
    pub(in crate::ui::features::reader) fn of(theme: &Theme) -> Self {
        Self([
            theme.red,
            theme.magenta,
            theme.cyan,
            theme.green,
            theme.yellow,
        ])
    }

    /// 按**位置**对上号：`ANNOTATION_COLORS` 与 [`Self::of`] 是同一份顺序。
    ///
    /// 认不出的名字回落到默认那一种而不是不画：存储层已经兜过一次（写进去的必然在
    /// 清单里），这里再丢一次信息，读者看到的就成了一条「划了但看不见」的批注。
    pub(in crate::ui::features::reader) fn color(&self, name: &str) -> Hsla {
        ANNOTATION_COLORS
            .iter()
            .position(|color| *color == name)
            .and_then(|ix| self.0.get(ix))
            .copied()
            .unwrap_or(self.0[0])
    }
}

/// 第 `chapter` 章现在该画在哪几段字上：命中缓存就直接给，章文本换了才重解。
///
/// 做成自由函数而不是 `ReaderSession` 的方法：渲染帧里 `self.source` 正被可变借用
/// （那一臂里要改排版状态），任何 `&self` 的方法都调不动，只能按字段分开递进来。
///
/// 键是**章文本的哈希**而不是排版代数：这里解出来的是可见文本坐标（第几个字到哪几个字），
/// 它只跟章文本有关，字号、栏宽、片高都动不着它。用代数当键的旧写法会让读者每动一次
/// 设置就把整章批注重解一遍。
pub(in crate::ui::features::reader) fn resolved_for(
    cache: &RefCell<AnnotationSpans>,
    stored: &[Annotation],
    chapter: usize,
    chapter_hash: u64,
    map: &TextMap,
) -> Rc<Vec<ResolvedAnnotation>> {
    let fresh = cache
        .borrow()
        .get(&chapter)
        .filter(|(cached, _)| *cached == chapter_hash)
        .map(|(_, spans)| spans.clone());
    if let Some(spans) = fresh {
        return spans;
    }
    let resolved = Rc::new(resolve_annotations(stored, chapter as u32, map));
    cache
        .borrow_mut()
        .insert(chapter, (chapter_hash, resolved.clone()));
    resolved
}
