//! 划线（M7 第一步）：把选中的字落成一条批注，并把库里的批注解回本章坐标画出来。
//!
//! 两头各有一套坐标，中间只隔一次解锚：
//!
//! - **写出去**的是 [`TextAnchor`]：章下标 + 章内可见文本偏移 + 一份原文指纹
//!   （`folio_core::anchor`）。它进库，排版、改字号、换设备都不该动它。
//! - **画出来**的是「这一章现在第几个字摆在哪一像素」：那是**当代排版**才答得出的问题，
//!   所以每次画之前都要拿本章的 `TextMap` 把锚点重解一遍（`resolve_annotations`
//!   的四级降级：原偏移 → 同 DOM 节点 → ±400 字窗口 → 全章）。
//!
//! 解锚比整章扫一遍文本便宜不了多少，而渲染帧是每帧都来的 —— 所以按章缓存一份，
//! 键里带上排版的**代数**（`Document::generation`）：代数没变，行盒与可见文本坐标
//! 就没变，上一帧解出来的位置照样作数。
//!
//! 解不回来的那一条（`is_stale()`）**不画**：宁可少一处底色，也不把读者的批注
//! 画到别的字上去。库里那条的 `stale` 标记照实回写，批注侧栏（后续里程碑）才有依据
//! 说「这条找不到了」。

use std::cell::RefCell;
use std::rc::Rc;

use folio_core::anchor::TextMap;
use folio_core::annotation::{ResolvedAnnotation, resolve_annotations};
use folio_core::store::{ANNOTATION_COLORS, Annotation};
use gpui_kit::component::Theme;
use gpui_kit::{Context, Hsla};

use super::super::session::{AnnotationSpans, ReaderSession, Selection};

/// 没得选时划线用的那一种颜色。清单在 `folio_core::store::ANNOTATION_COLORS`，
/// 排在第一位的就是它 —— 这里不另抄一份字面量。
const DEFAULT_COLOR: &str = "amber";

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
                // 读不出批注不影响正文：这一屏照读，只是底色没了。
                tracing::warn!("划线: 读第 {:?} 本的批注失败：{err:#}", self.book_id);
                self.annotations = Vec::new();
            }
        }
        self.annotation_spans.borrow_mut().clear();
    }

    /// 划线：把当前选区落成一条批注。
    ///
    /// 落不成只写日志，不把菜单条留在屏上装作还能划 —— 读者再点一次也不会有别的结果，
    /// 而收掉菜单至少告诉他「这一下按过了」。
    pub fn add_highlight(&mut self, cx: &mut Context<Self>) {
        let Some(selected) = self.selection.clone() else {
            return;
        };
        self.selection_menu_open = false;
        let Some(anchor) = self.anchor_for(&selected) else {
            tracing::warn!(
                "划线: 第 {} 章的排版还没跟上，这一条没落库",
                selected.chapter + 1
            );
            cx.notify();
            return;
        };
        let (Some(book_id), Some(store)) = (self.book_id, self.store.clone()) else {
            tracing::warn!("划线: 这本书没登记进书库（没有 book_id 或书库没装），批注没处落");
            cx.notify();
            return;
        };
        match store.add_annotation(book_id, &anchor, "", DEFAULT_COLOR) {
            Ok(id) => {
                tracing::info!("划线: 第 {} 章落一条批注（id {id}）", selected.chapter + 1);
                self.load_annotations();
                // 选区收掉：此刻屏上那片底色来自刚落的批注，不是来自「选中」。
                self.clear_selection();
            }
            Err(err) => tracing::error!("划线: 写库失败：{err:#}"),
        }
        cx.notify();
    }

    /// 全选本章：从章首到章尾。选区仍挂在它自己那一章上。
    pub fn select_chapter(&mut self, cx: &mut Context<Self>) {
        let Some(selected) = self.selection.clone() else {
            return;
        };
        let Some(total) = self
            .book()
            .and_then(|book| book.span_doc(selected.chapter))
            .map(|doc| doc.text_map().chars())
        else {
            return;
        };
        if total == 0 {
            return;
        }
        self.selection = Some(Selection {
            chapter: selected.chapter,
            range: 0..total,
        });
        cx.notify();
    }

    /// 选区 → 锚点。章下标与偏移都照选区自己那份，锚点里存的原文指纹由 `TextMap` 给。
    fn anchor_for(&self, selected: &Selection) -> Option<folio_core::anchor::TextAnchor> {
        let doc = self.book()?.span_doc(selected.chapter)?;
        // 排版没跟上的那一帧，`TextMap` 还是上一章的：这时落库会把批注钉错字。
        if !doc.is_current() {
            return None;
        }
        Some(folio_core::anchor::TextAnchor::new(
            doc.text_map(),
            selected.chapter as u32,
            selected.range.start,
            selected.range.end,
        ))
    }
}

/// 四种批注底色，按 `folio_core::store::ANNOTATION_COLORS` 的顺序摆。
///
/// 渲染帧开头取一次（`cx.theme()` 每帧问一遍不贵，但逐块画时查表更省事），
/// 画的时候按库里那个名字查。透明度是「压得住正文又能看出是划过」那一档：
/// 划线是留在书上的痕迹，不该把字吃掉。
pub(in crate::ui::features::reader) struct Palette([Hsla; 4]);

impl Palette {
    pub(in crate::ui::features::reader) fn of(theme: &Theme) -> Self {
        Self([
            theme.yellow.opacity(0.35),
            theme.magenta.opacity(0.35),
            theme.cyan.opacity(0.35),
            theme.green.opacity(0.35),
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

/// 第 `chapter` 章现在该画在哪几段字上：命中缓存就直接给，代数变了才重解。
///
/// 做成自由函数而不是 `ReaderSession` 的方法：渲染帧里 `self.source` 正被可变借用
/// （那一臂里要改排版状态），任何 `&self` 的方法都调不动，只能按字段分开递进来。
pub(in crate::ui::features::reader) fn resolved_for(
    cache: &RefCell<AnnotationSpans>,
    stored: &[Annotation],
    chapter: usize,
    generation: u64,
    map: &TextMap,
) -> Rc<Vec<ResolvedAnnotation>> {
    let fresh = cache
        .borrow()
        .get(&chapter)
        .filter(|(cached, _)| *cached == generation)
        .map(|(_, spans)| spans.clone());
    if let Some(spans) = fresh {
        return spans;
    }
    let resolved = Rc::new(resolve_annotations(stored, chapter as u32, map));
    cache
        .borrow_mut()
        .insert(chapter, (generation, resolved.clone()));
    resolved
}
