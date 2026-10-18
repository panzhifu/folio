//! 划线（M7 第一步）：把选中的字落成一条批注，并把库里的批注解回本章坐标画出来。
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
//! 解不回来的那一条（`is_stale()`）**不画**：宁可少一条下划线，也不把读者的批注
//! 画到别的字上去。库里那条的 `stale` 标记照实回写，批注侧栏（后续里程碑）才有依据
//! 说「这条找不到了」。
//!
//! 画出来的那几条下划线同时是**可以点回去的**：点在矩形上就是把那一条重新选中
//! （[`ReaderSession::annotation_at`]），于是留在书上的痕迹不只是读数，还能改 ——
//! 就地菜单跟着给出「取消划线」与五色（[`ReaderSession::selection_annotation`]）。
//! 判据用的是画出来的那些矩形而不是「离得最近的那个字」，理由在下面那条命中方法的注释里。

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use folio_core::content::anchor::TextMap;
use folio_core::content::annotation::{ResolvedAnnotation, resolve_annotations};
use folio_core::store::{ANNOTATION_COLORS, Annotation};
use gpui_kit::component::Theme;
use gpui_kit::component::input::{InputEvent, TextareaState};
use gpui_kit::{App, AppContext as _, Context, Hsla, Pixels, Point, Window};

use super::super::note_editor::NoteEditor;
use super::super::session::{AnnotationSpans, ReaderSession, Selection};
use crate::ui::services::l10n::tr;

/// 点「划线」时用的那一种颜色（读者没先点某一枚圆点）。清单在
/// `folio_core::store::ANNOTATION_COLORS`：amber 是其中那档黄 —— 传统划线色，
/// 也是参考图里给勾选标记的那一枚。
pub(super) const DEFAULT_COLOR: &str = "amber";

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

    /// 划线：把当前选区落成一条批注，用默认那档颜色（菜单里「划线」那一格）。
    pub fn add_highlight(&mut self, cx: &mut Context<Self>) {
        self.add_highlight_with(DEFAULT_COLOR, cx);
    }

    /// 划线，但指定颜色：读者在颜色那一行直接点某一枚圆点时走这条 ——
    /// 一段新选区点色即「就用这个色划线」，不必先按「划线」再换色。
    ///
    /// 落不成只写日志，不把菜单条留在屏上装作还能划 —— 读者再点一次也不会有别的结果，
    /// 而收掉菜单至少告诉他「这一下按过了」。
    pub fn add_highlight_with(&mut self, color: &str, cx: &mut Context<Self>) {
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
        match store.add_annotation(book_id, &anchor, "", color) {
            Ok(id) => {
                tracing::info!(
                    book_id,
                    chapter = selected.chapter + 1,
                    annotation_id = id,
                    color,
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
        let (chapter, x, y) = self.to_layout(window, at)?;
        let doc = self.book()?.span_doc(chapter)?;
        // 排版还没跟上的那一帧，行盒是旧的：按旧行盒算出的矩形对不上这一帧画出来的划线。
        if !doc.is_current() || self.annotations.is_empty() {
            return None;
        }
        let hit = doc.hit_test(window, cx, x, y)?;
        let line = doc.lines().get(hit.line)?;
        if y < line.y || y >= line.y + line.height {
            return None;
        }
        let band = line.y..(line.y + line.height);
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
                    .any(|rect| x >= rect.x && x < rect.x + rect.width)
            })
            .map(|span| (chapter, span.start..span.end, span.id))
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

    /// 当前选区**正好等于**库里某一条划线时，给回那一条的 id 与颜色名。
    ///
    /// 判据是区间相等，不是「这一击是从划线上点出来的」：读者拖过把手把范围改了，手里
    /// 拿的就是一段新选区（该再划一次），而不是原来那一条 —— 菜单跟着这个判断换格子。
    pub fn selection_annotation(&self) -> Option<(i64, String)> {
        let selected = self.selection.as_ref()?;
        let doc = self.book()?.span_doc(selected.chapter)?;
        let spans = resolved_for(
            &self.annotation_spans,
            &self.annotations,
            selected.chapter,
            doc.chapter_hash(),
            doc.text_map(),
        );
        spans
            .iter()
            .find(|span| {
                !span.is_stale()
                    && span.start == selected.range.start
                    && span.end == selected.range.end
            })
            .map(|span| (span.id, span.color.clone()))
    }

    /// 取消划线：把选区对应的那一条从库里删掉，画出来的那条划线随它消失。
    ///
    /// 删完把选区收掉：屏上已经没东西可挑中了，留着一段「选中」只会让读者以为还能再删一次。
    pub fn delete_highlight(&mut self, cx: &mut Context<Self>) {
        let Some((id, _)) = self.selection_annotation() else {
            return;
        };
        self.selection_menu_open = false;
        let Some(store) = self.store.as_ref() else {
            tracing::warn!(annotation_id = id, "划线: 书库没装，那一条删不掉");
            cx.notify();
            return;
        };
        match store.delete_annotation(id) {
            Ok(()) => {
                tracing::info!(annotation_id = id, "划线: 取消一条批注");
                self.load_annotations();
                self.clear_selection();
            }
            Err(err) => tracing::error!(annotation_id = id, error = %err, "划线: 删库失败"),
        }
        cx.notify();
    }

    /// 换颜色：改库里那一条的 `color`，选区留着（读者往往还要连着比两档颜色）。
    ///
    /// `note` 原样带过去：存储层那条 `update_annotation` 是整条覆盖（note + color 一起写），
    /// 只当自己是改颜色而不带上笔记，就会把读者写过的笔记悄悄清空。
    pub fn set_highlight_color(&mut self, color: &str, cx: &mut Context<Self>) {
        let Some((id, current)) = self.selection_annotation() else {
            return;
        };
        if current == color {
            return; // 点的就是当下那一格：一趟都不跑
        }
        let Some(store) = self.store.as_ref() else {
            tracing::warn!(annotation_id = id, "划线: 书库没装，颜色改不了");
            return;
        };
        let note = self
            .annotations
            .iter()
            .find(|row| row.id == id)
            .map(|row| row.note.clone())
            .unwrap_or_default();
        match store.update_annotation(id, &note, color) {
            Ok(()) => {
                tracing::info!(annotation_id = id, color, "划线: 换一条批注的颜色");
                self.load_annotations();
            }
            Err(err) => tracing::error!(
                annotation_id = id,
                error = %err,
                "划线: 改颜色写库失败"
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
            .and_then(|(id, _)| self.annotations.iter().find(|row| row.id == *id))
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

    /// 保存笔记。已有批注走 `update_annotation`（笔记与原来的颜色一起写回），
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
        if let Some((id, color)) = editor.annotation {
            let Some(store) = self.store.clone() else {
                tracing::warn!(annotation_id = id, "笔记: 书库没装，这一条改不了");
                self.selection_menu_open = true;
                cx.notify();
                return;
            };
            match store.update_annotation(id, &text, &color) {
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
        match store.add_annotation(book_id, &anchor, &text, DEFAULT_COLOR) {
            Ok(id) => {
                tracing::info!(
                    book_id,
                    chapter = selected.chapter + 1,
                    annotation_id = id,
                    color = DEFAULT_COLOR,
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
    fn anchor_for(&self, selected: &Selection) -> Option<folio_core::content::anchor::TextAnchor> {
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

/// 五种划线实色，按 `folio_core::store::ANNOTATION_COLORS` 的顺序摆
/// （红 / 紫 / 蓝 / 绿 / 黄）。
///
/// 渲染帧开头取一次（`cx.theme()` 每帧问一遍不贵，但逐条画时查表更省事），
/// 画的时候按库里那个名字查。**不压透明度**：划线是压在字下的一条实线、圆点是一枚实心
/// 圆，都要看得清；从前底色版那 0.35 是给「整块铺在字下、不能把字吃掉」用的，现在这条
/// 理由不在了。
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
