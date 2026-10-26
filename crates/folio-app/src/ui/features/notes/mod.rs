//! 「笔记」目的地：全书库的书签与批注，按书分组。
//!
//! 批注闭环的**跨书**那一面（书内清单在目录面板「批注」那一格）：读者隔了些日子回来，
//! 要的是「我在哪些书里留了哪些话」。四条布局决定，与统计那一屏同一套纪律：
//!
//! - **取数只在 [`NotesView::refresh`] 里**（切进这一屏、关掉一本书、本屏删改之后，
//!   由外壳或自己叫），`render` 一律不碰库。
//! - **按书分组，最近有动静的书在前**：跨书清单不排序就是一锅按时间的流水，而读者
//!   认的是「哪本书」。
//! - **每行可跳**：点一条 = 开那本书落到那一处（锚点里带着章与偏移，目标章多半还没
//!   排版，落点交给会话挂恢复请求）。
//! - **删除有撤销**：行尾那颗删除是即下的，后悔靠底部那颗 6 秒的胶囊（与阅读页
//!   同一套语义，各屏各一颗：删除的现场在哪屏，胶囊就在哪屏）。
//!
//! 导出在书组头上那颗「⋯」里：三档人读的格式（txt / Markdown / 网页），装配在 core
//! 的纯函数（`annotations_export`），落盘走 [`host::share_file`] —— 桌面进下载目录，
//! 手机进系统分享面板。

use std::collections::HashMap;
use std::time::Duration;

use folio_core::{AnnotationExportFormat, DocMeta, Format, annotations_export};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme, Icon, Sizable as _, ThemeStyled as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, Context, ElementId, EventEmitter, FontWeight, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Task,
    TestSupportExt as _, Window, div, px,
};

use crate::ui::components::empty_state;
use crate::ui::host;
use crate::ui::services::app_store;
use crate::ui::services::l10n::{tf, tr};
use crate::ui::shell::navigation::{self, SizeClass};

/// 撤销胶囊在屏上停留多久。与阅读页那一颗同一个数：两处的语义是同一件事。
const UNDO_TTL: Duration = Duration::from_secs(6);
/// 内容列的最大宽度，与统计那一屏同一个数：清单拉到 1600 宽是这片沙漠的近亲。
const COLUMN_MAX: f32 = 720.0;
/// 左右留白，与书架 / 统计共用同一个数：切目的地时那条对齐脊线不该跳。
const GUTTER: f32 = 12.0;

/// 这一屏向外壳报的一件事。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotesEvent {
    /// 点了一条：开那本书，落到那一处（章，章内偏移）。
    Open {
        book_id: i64,
        /// 落点。定版式那一族的偏移会被会话那头忽略（它只有页），递进去没有害处。
        jump: (usize, usize),
    },
}

/// 清单里的一行。书签与批注各占一行，字段是两族的并集 —— 与目录面板「批注」那一格
/// 同一个形状，只是出处那句在这里由库里的章标题表给（这一屏没有排版可问）。
struct MarkEntry {
    is_bookmark: bool,
    id: i64,
    chapter: usize,
    offset: usize,
    /// 章名。库里没有那一行（定版式没有章表、老库缺号）时是空串，界面就不显出处。
    place: String,
    quote: String,
    note: String,
    color: Option<String>,
    stale: bool,
    created_at: i64,
}

/// 一本书的那一组。
struct BookGroup {
    book_id: i64,
    title: String,
    /// 组内最近一次动静。组间按它倒序：最近留过话的书在最前。
    latest: i64,
    marks: Vec<MarkEntry>,
}

/// 本屏自己的撤销槽：删掉的那一行整份抄着，窗口内按原行插回。
struct PendingUndo {
    row: Deleted,
    seq: u64,
    _timer: Option<Task<()>>,
}

/// 删掉的那一行。批注与书签共用一颗胶囊，后删的顶掉先删的。
enum Deleted {
    Annotation(folio_core::store::Annotation),
    Bookmark(folio_core::store::Bookmark),
}

#[derive(Default)]
pub struct NotesView {
    /// 按书分好组的清单。`None` = 还没取到（读不到库，或那一趟查询失败）——
    /// 这一格与「库里还没有批注」是两句话。
    data: Option<Vec<BookGroup>>,
    store_missing: bool,
    undo: Option<PendingUndo>,
    undo_seq: u64,
}

impl NotesView {
    pub fn new() -> Self {
        Self::default()
    }

    /// 重取一次快照。切进这一屏（外壳叫）、关掉一本书（外壳叫）、本屏删改之后
    /// （自己叫）都走这里。
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(store) = app_store::handle(cx) else {
            self.store_missing = true;
            self.data = None;
            cx.notify();
            return;
        };
        match load_groups(&store) {
            Ok(groups) => {
                self.store_missing = false;
                self.data = Some(groups);
            }
            Err(err) => {
                tracing::warn!(error = %err, "笔记: 读批注与书签失败");
                self.store_missing = true;
                self.data = None;
            }
        }
        cx.notify();
    }

    /// 点了一条：报给外壳（开书 + 落点都在它手里）。
    fn open_entry(&mut self, book_id: i64, chapter: usize, offset: usize, cx: &mut Context<Self>) {
        cx.emit(NotesEvent::Open {
            book_id,
            jump: (chapter, offset),
        });
    }

    /// 删一行。删除是即下的，后悔靠 [`Self::undo_delete`] —— 与阅读页同一条语义。
    fn remove_entry(&mut self, is_bookmark: bool, id: i64, cx: &mut Context<Self>) {
        let Some(store) = app_store::handle(cx) else {
            return;
        };
        let deleted = if is_bookmark {
            store
                .bookmark(id)
                .ok()
                .flatten()
                .and_then(|row| store.delete_bookmark(id).ok().map(|_| row))
                .map(Deleted::Bookmark)
        } else {
            store
                .annotation(id)
                .ok()
                .flatten()
                .and_then(|row| store.delete_annotation(id).ok().map(|_| row))
                .map(Deleted::Annotation)
        };
        let Some(row) = deleted else {
            tracing::warn!(is_bookmark, id, "笔记: 那一条删不掉（读不到或写库失败）");
            return;
        };
        tracing::info!(is_bookmark, id, "笔记: 删一条记录");
        self.stash_undo(row, cx);
        self.refresh(cx);
    }

    /// 撤销最近一次本屏删除：按原行插回（id 与时间戳都不变）。
    fn undo_delete(&mut self, cx: &mut Context<Self>) {
        let Some(slot) = self.undo.take() else {
            return;
        };
        self.undo_seq += 1; // 作废在飞的到点计时：胶囊已经收了。
        let Some(store) = app_store::handle(cx) else {
            return;
        };
        let ok = match &slot.row {
            Deleted::Annotation(row) => store.restore_annotation(row).is_ok(),
            Deleted::Bookmark(row) => store.restore_bookmark(row).is_ok(),
        };
        if ok {
            tracing::info!("笔记: 撤销删除");
        } else {
            tracing::error!("笔记: 撤销写库失败，那一行没回来");
        }
        self.refresh(cx);
    }

    fn stash_undo(&mut self, row: Deleted, cx: &mut Context<Self>) {
        self.undo_seq += 1;
        let seq = self.undo_seq;
        let timer = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(UNDO_TTL).await;
            let _ = this.update(cx, |this, cx| {
                // 只收自己那一号：后来的删除换过槽，这一趟到点不该把别人的胶囊收走。
                if this.undo.as_ref().is_some_and(|slot| slot.seq == seq) {
                    this.undo = None;
                    cx.notify();
                }
            });
        });
        self.undo = Some(PendingUndo {
            row,
            seq,
            _timer: Some(timer),
        });
    }

    /// 导出一本书的批注。装配在 core 的纯函数，落盘交给平台那一层 —— 桌面进下载
    /// 目录，手机进分享面板。批注直接从库里读这一本的整表：这一屏手上那组是裁过的
    /// 视图，导出要的是权威。
    fn export_group(
        &mut self,
        book_id: i64,
        format: AnnotationExportFormat,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = app_store::handle(cx) else {
            return;
        };
        let Some(row) = store.book_meta(book_id).ok().flatten() else {
            tracing::warn!(book_id, "笔记: 导出时读不到那一本书的书目");
            return;
        };
        let meta = DocMeta {
            title: row.title,
            authors: row.authors,
            format: Format::parse(&row.format).unwrap_or_default(),
            size: row.size,
            ..DocMeta::default()
        };
        let chapters = store.chapter_titles(book_id).unwrap_or_default();
        let annotations = store.annotations(book_id).unwrap_or_default();
        let text = annotations_export(format, &meta, &chapters, &annotations);
        host::share_file(
            text.as_bytes(),
            format.mime(),
            &export_file_name(&meta.title, format),
            cx,
        );
        tracing::info!(
            book_id,
            format = format.ext(),
            count = annotations.len(),
            "笔记: 导出批注"
        );
    }

    /// 读不到书库。这一句与「还没有批注」必须分得开 —— 与统计那一屏同一条规矩。
    fn unavailable(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .w_full()
            .gap_1()
            .pt_2()
            .child(div().child(tr(cx, "notes.no_store")))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr(cx, "notes.no_store_hint")),
            )
            .into_any_element()
    }

    /// 一本书的那一组：书名 + 读数 + 导出「⋯」的组头，下面跟着这一组的行。
    fn group_section(&self, group: &BookGroup, cx: &mut Context<Self>) -> AnyElement {
        let notes_count = group.marks.iter().filter(|m| !m.is_bookmark).count();
        let bookmarks_count = group.marks.len() - notes_count;
        let counts: SharedString = tf(
            cx,
            "notes.group_counts",
            &[("notes", &notes_count), ("bookmarks", &bookmarks_count)],
        )
        .into();
        let export_panel = cx.entity().clone();
        let export_book = group.book_id;
        let export_label = tr(cx, "notes.export");
        let header = h_flex()
            .id("notes-group-head")
            .w_full()
            .items_center()
            .gap_2()
            .pt_3()
            .pb_1()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(group.title.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(counts),
            )
            .child(
                Button::new("notes-export")
                    .ghost()
                    .small()
                    .icon(IconName::CircleEllipsis)
                    .tooltip(export_label.clone().to_string())
                    .accessibility_label(export_label)
                    .dropdown_menu_with_anchor(Anchor::BottomRight, move |menu, _, cx| {
                        let (md_panel, txt_panel, html_panel) = (
                            export_panel.clone(),
                            export_panel.clone(),
                            export_panel.clone(),
                        );
                        let txt_label = tr(cx, "notes.export_text");
                        let md_label = tr(cx, "notes.export_markdown");
                        let html_label = tr(cx, "notes.export_html");
                        menu.min_w(px(200.))
                            .item(PopupMenuItem::new(txt_label).on_click(move |_, _, cx| {
                                txt_panel.update(cx, |this, cx| {
                                    this.export_group(export_book, AnnotationExportFormat::Text, cx)
                                })
                            }))
                            .item(PopupMenuItem::new(md_label).on_click(move |_, _, cx| {
                                md_panel.update(cx, |this, cx| {
                                    this.export_group(
                                        export_book,
                                        AnnotationExportFormat::Markdown,
                                        cx,
                                    )
                                })
                            }))
                            .item(PopupMenuItem::new(html_label).on_click(move |_, _, cx| {
                                html_panel.update(cx, |this, cx| {
                                    this.export_group(export_book, AnnotationExportFormat::Html, cx)
                                })
                            }))
                    }),
            );
        let rows: Vec<AnyElement> = group
            .marks
            .iter()
            .map(|entry| self.mark_row(group, entry, cx))
            .collect();
        v_flex()
            .w_full()
            .child(header)
            .children(rows)
            .into_any_element()
    }

    /// 一行：识别物（书签图标 / 颜色点）+ 引文一行 + 出处与笔记一行 + 行尾删除。
    /// 点行本身 = 跳回原文。
    fn mark_row(&self, group: &BookGroup, entry: &MarkEntry, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let badge = if entry.is_bookmark {
            Icon::new(IconName::Bookmark)
                .small()
                .text_color(theme.muted_foreground)
                .into_any_element()
        } else {
            // 颜色按 `ANNOTATION_COLORS` 里的位置对上主题色，与正文回显同一份顺序；
            // 认不出的名字（库里不该有）落默认那枚红。
            let index = folio_core::ANNOTATION_COLORS
                .iter()
                .position(|name| Some(*name) == entry.color.as_deref())
                .unwrap_or(0);
            let color = [
                theme.red,
                theme.magenta,
                theme.cyan,
                theme.green,
                theme.yellow,
            ]
            .get(index)
            .copied()
            .unwrap_or(theme.red);
            div()
                .flex_none()
                .w(px(8.0))
                .h(px(8.0))
                .rounded_full()
                .bg(color)
                .into_any_element()
        };
        let place: Option<SharedString> = if entry.place.is_empty() {
            None
        } else if entry.stale {
            Some(tf(cx, "notes.stale", &[("place", &entry.place)]).into())
        } else {
            Some(entry.place.clone().into())
        };
        let view = cx.entity().clone();
        let (book_id, chapter, offset) = (group.book_id, entry.chapter, entry.offset);
        let (delete_view, is_bookmark, id) = (view.clone(), entry.is_bookmark, entry.id);
        let note: Option<SharedString> = if entry.note.is_empty() {
            None
        } else {
            Some(entry.note.clone().into())
        };
        v_flex()
            .id(ElementId::named_usize("notes-row", entry.id as usize))
            .w_full()
            .items_center()
            .flex()
            .gap_2()
            .py_2()
            .overflow_hidden()
            .child(badge)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .overflow_hidden()
                    .child(
                        div()
                            .w_full()
                            .text_sm()
                            .text_color(theme.foreground)
                            .truncate()
                            .child(if entry.quote.is_empty() {
                                note.clone().unwrap_or_else(|| "".into())
                            } else {
                                entry.quote.clone().into()
                            }),
                    )
                    .when_some(note, |el, note| {
                        el.child(
                            div()
                                .w_full()
                                .text_sm()
                                .text_color(theme.foreground)
                                .truncate()
                                .child(note),
                        )
                    })
                    .when_some(place, |el, place| {
                        el.child(
                            div()
                                .w_full()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .truncate()
                                .child(place),
                        )
                    }),
            )
            .child(
                Button::new("notes-row-delete")
                    .ghost()
                    .small()
                    .icon(IconName::Trash)
                    .on_click(move |_, _, cx| {
                        delete_view.update(cx, |this, cx| this.remove_entry(is_bookmark, id, cx))
                    }),
            )
            .on_click(move |_, _, cx| {
                view.update(cx, |this, cx| this.open_entry(book_id, chapter, offset, cx))
            })
            .into_any_element()
    }
}

impl Render for NotesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let size = SizeClass::of(window);
        let wide = size == SizeClass::Expanded;
        // Compact 档那条底栏是浮的：内容自己把栏底下那一段空出来（与统计那一屏同一套）。
        let tail = if size.is_compact() {
            navigation::clearance(crate::ui::host::safe_area(window, cx).bottom)
        } else {
            0.0
        };
        let undo_pill = self.undo.as_ref().map(|slot| {
            let view = cx.entity().clone();
            let action = tr(cx, "notes.undo");
            let label: SharedString = match &slot.row {
                Deleted::Annotation(_) => tr(cx, "notes.deleted_annotation"),
                Deleted::Bookmark(_) => tr(cx, "notes.deleted_bookmark"),
            };
            h_flex()
                .occlude()
                .absolute()
                .left(px(24.0))
                .right(px(24.0))
                .bottom(px(tail + 12.0))
                .items_center()
                .justify_between()
                .gap_3()
                .px_4()
                .py_2()
                .popover_style(cx)
                .rounded_full()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(cx.theme().popover_foreground)
                        .child(label),
                )
                .child(
                    Button::new("notes-undo")
                        .ghost()
                        .small()
                        .label(action.to_string())
                        .accessibility_label(action)
                        .on_click(move |_, _, cx| view.update(cx, |this, cx| this.undo_delete(cx))),
                )
                .into_any_element()
        });
        let body: AnyElement = match (&self.data, self.store_missing) {
            (_, true) => self.unavailable(cx),
            (Some(groups), false) if groups.is_empty() => {
                empty_state(tr(cx, "nav.notes"), tr(cx, "nav.pending.notes"), cx).into_any_element()
            }
            (Some(groups), false) => {
                let mut sections: Vec<AnyElement> = Vec::with_capacity(groups.len());
                for group in groups {
                    sections.push(self.group_section(group, cx));
                }
                v_flex().w_full().children(sections).into_any_element()
            }
            (None, false) => {
                empty_state(tr(cx, "nav.notes"), tr(cx, "nav.pending.notes"), cx).into_any_element()
            }
        };
        let column = v_flex()
            .w_full()
            .when(wide, |el| el.max_w(px(COLUMN_MAX)))
            .px(px(GUTTER))
            .pb(px(GUTTER + tail))
            .child(body);
        v_flex()
            .id("folio-notes")
            .test_support()
            .size_full()
            .relative()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .children(undo_pill)
            .child(
                div()
                    .id("notes-scroll")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scrollbar()
                    .child(v_flex().w_full().items_center().child(column)),
            )
            .into_any_element()
    }
}

impl EventEmitter<NotesEvent> for NotesView {}

/// 导出文件名。书名是读者数据，进不了文件名的几个字符换成下划线 —— 宁可难看，
/// 不能让一次导出因为名字里的一个 `/` 落不了盘。
fn export_file_name(title: &str, format: AnnotationExportFormat) -> String {
    let cleaned: String = title
        .trim()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect();
    let cleaned = cleaned.trim();
    let cleaned = if cleaned.is_empty() {
        "批注"
    } else {
        cleaned
    };
    format!("《{cleaned}》批注.{}", format.ext())
}

/// 从库里折出按书分组的清单。组内：书签在前、批注在后，各自按章序排（同章按钉下
/// 的顺序）；组间按最近动静倒序。
fn load_groups(store: &folio_core::Store) -> folio_core::Result<Vec<BookGroup>> {
    let mut groups: HashMap<i64, BookGroup> = HashMap::new();
    for card in store.all_annotations()? {
        let annotation = &card.annotation;
        let group = groups
            .entry(annotation.book_id)
            .or_insert_with(|| BookGroup {
                book_id: annotation.book_id,
                title: card.book_title.clone(),
                latest: annotation.created_at,
                marks: Vec::new(),
            });
        group.latest = group.latest.max(annotation.created_at);
        group.marks.push(MarkEntry {
            is_bookmark: false,
            id: annotation.id,
            chapter: annotation.chapter as usize,
            offset: annotation.anchor.start,
            place: String::new(),
            quote: snippet(&annotation.anchor.exact),
            note: annotation.note.trim().to_string(),
            color: Some(annotation.color.clone()),
            stale: annotation.stale,
            created_at: annotation.created_at,
        });
    }
    for card in store.all_bookmarks()? {
        let bookmark = &card.bookmark;
        let group = groups.entry(bookmark.book_id).or_insert_with(|| BookGroup {
            book_id: bookmark.book_id,
            title: card.book_title.clone(),
            latest: bookmark.created_at,
            marks: Vec::new(),
        });
        group.latest = group.latest.max(bookmark.created_at);
        group.marks.push(MarkEntry {
            is_bookmark: true,
            id: bookmark.id,
            chapter: bookmark.chapter as usize,
            offset: bookmark.anchor.start,
            place: String::new(),
            quote: snippet(&bookmark.anchor.exact),
            note: String::new(),
            color: None,
            stale: false,
            created_at: bookmark.created_at,
        });
    }
    // 每组一张章标题表：出处那一句在这里填 —— 这一屏没有排版可问，库里那张表是权威。
    for group in groups.values_mut() {
        let titles = store.chapter_titles(group.book_id).unwrap_or_default();
        // 书签在前、批注在后（与目录面板「批注」那一格同一份读法），各自按章序排，
        // 同章按钉下的顺序。
        group
            .marks
            .sort_by_key(|m| (!m.is_bookmark, m.chapter, m.created_at));
        for mark in &mut group.marks {
            mark.place = titles
                .get(mark.chapter)
                .map(|chapter| chapter.title.trim().to_string())
                .unwrap_or_default();
        }
    }
    let mut out: Vec<BookGroup> = groups.into_values().collect();
    out.sort_by(|a, b| b.latest.cmp(&a.latest).then_with(|| a.title.cmp(&b.title)));
    Ok(out)
}

/// 一句原文在清单里显多长。`exact` 可能带着排版换行，先折空白再截 —— 与目录面板
/// 「批注」那一格同一条口径。
fn snippet(exact: &str) -> String {
    let folded: String = exact.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = folded.chars().take(48).collect();
    if folded.chars().count() > 48 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests;
