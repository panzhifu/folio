//! 「笔记」目的地：一本书一张卡，点开一张卡看这一本留下的话。
//!
//! 批注闭环的**跨书**那一面（书内清单在目录面板「批注」那一格）：读者隔了些日子回来，
//! 要的是「我在哪些书里留了哪些话」。这一屏的主语因此是**书**，不是逐条的话 —— 顶部一颗
//! 搜索框，下面一本书一张卡（左边封面，右边从上往下：留了多少条、书名、累计读了多久），
//! 点开一张卡把这一本的逐条笔记抬成一块底部弹层。布局决定与统计那一屏同一套纪律：
//!
//! - **取数只在 [`NotesView::refresh`] 里**（切进这一屏、关掉一本书、本屏删改之后，
//!   由外壳或自己叫），`render` 一律不碰库。封面也在这里解好字节存进 [`BookGroup`]：
//!   逐帧解图是把「画一屏」变成「做一轮 IO」，而渲染帧不该干 IO。
//! - **卡片按最近动静倒序**：跨书清单不排序就是一锅按时间的流水，而读者认的是「哪本书」。
//! - **搜索框找的是书**：词对上书名，或对上这一本里任何一条引文与笔记，那一本就留在
//!   列表上。点进去看的仍是这一本的**全部**笔记 —— 搜完再进去，读者要的是那一句的上下文，
//!   不是被裁得只剩那一句。
//! - **逐条那一层是一块底部弹层，不是新的一页**：与目录、更新日志同一个容器，下拉过头、
//!   点遮罩、按返回三条路都收它。导出的「⋯」挂在弹层 header 上 —— 它的作用对象是「这一本」。
//! - **每行可跳**：点一条 = 开那本书落到那一处（锚点里带着章与偏移，目标章多半还没排版，
//!   落点交给会话挂恢复请求）。跳之前先把弹层收掉：阅读层盖在整窗之上，两层遮罩叠着，
//!   返回键与点击命中都得各判一次谁是顶。
//! - **删除有撤销**：行尾那颗删除是即下的，后悔靠底部那颗 6 秒的胶囊（与阅读页同一套
//!   语义，各屏各一颗：删除的现场在哪屏，胶囊就在哪屏）。
//!
//! 导出三档人读的格式（txt / Markdown / 网页），装配在 core 的纯函数
//! （`annotations_export`），落盘走 [`host::share_file`] —— 桌面进下载目录，手机进系统
//! 分享面板。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use folio_core::{AnnotationExportFormat, DocMeta, Format, annotations_export};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme, Icon, Sizable as _, ThemeStyled as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, Context, ElementId, Entity, EventEmitter, FontWeight,
    Image, InteractiveElement, IntoElement, ObjectFit, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, StyledImage, Subscription, Task, TestSupportExt as _,
    Window, div, img, px,
};

use crate::ui::components::{
    BottomSheet, BottomSheetEvent, BottomSheetState, SearchBox, SearchBoxEvent, empty_state,
};
use crate::ui::foundation::images::cover_image;
use crate::ui::host;
use crate::ui::services::app_store;
use crate::ui::services::l10n::{duration, tf, tr};
use crate::ui::shell::navigation::{self, SizeClass};

/// 撤销胶囊在屏上停留多久。与阅读页那一颗同一个数：两处的语义是同一件事。
const UNDO_TTL: Duration = Duration::from_secs(6);
/// 内容列的最大宽度，与统计那一屏同一个数：清单拉到 1600 宽是这片沙漠的近亲。
const COLUMN_MAX: f32 = 720.0;
/// 左右留白，与书架 / 统计共用同一个数：切目的地时那条对齐脊线不该跳。搜索框那一行、
/// 卡片列表与弹层里的行都用它，于是一列的左边界从头到尾是一条线。
const GUTTER: f32 = 12.0;
/// 弹层展开那一档占视口高的比例。与更新日志那一张卡同档：逐条笔记是要读完的正文，
/// 底下不必留着能点的卡片行 —— 这一层收掉就是收掉。
const SHEET_EXPANDED: f32 = 0.85;
/// 往下拖还能停的那一档：半屏。留着它是为了让「读几条，再回头看架子底下那一列书」
/// 这个动作有一档可停，与目录那两档同一条理由。
const SHEET_PEEK: f32 = 0.5;
/// 卡片上那一格封面：48×72，与目录面板书头那一格同一个数（同一张 2:3 的尺）。
/// 写 `px` 不写 rem：那是位图的边界，不是排版的间距。
const COVER_W: f32 = 48.0;
const COVER_H: f32 = 72.0;

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

/// 一本书的那一组，也就是列表上的那张卡。
struct BookGroup {
    book_id: i64,
    title: String,
    /// 组内最近一次动静。组间按它倒序：最近留过话的书在最前。
    latest: i64,
    marks: Vec<MarkEntry>,
    /// 这一本的**累计**阅读秒数（`reading_sessions` 合计）。取不到按 0 处理：
    /// 「这本书还没读过」是常见状态，不是异常，卡片就念「0 秒」那一档的句子。
    seconds: i64,
    /// 卡片左边那一格封面（列表档缩略图，取不到时是原图）。`None` = 画一块灰底占位，
    /// 封面是装饰，不是内容的入口。
    cover: Option<Arc<Image>>,
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

pub struct NotesView {
    /// 按书分好组的清单。`None` = 还没取到（读不到库，或那一趟查询失败）——
    /// 这一格与「库里还没有批注」是两句话。
    data: Option<Vec<BookGroup>>,
    store_missing: bool,
    undo: Option<PendingUndo>,
    undo_seq: u64,
    /// 顶部那颗搜索框。句柄必须跨帧持有：`InputState` 就是框的内容与焦点的载体，
    /// 每帧重建一个会把读者刚打的字和光标一起丢掉。
    search: Entity<SearchBox>,
    /// 框里当前的词，**已折成小写**（订阅那一刻折一次，逐帧不再折候选之外的东西）。
    /// 空串 = 没在搜，列表全摆。
    query: SharedString,
    /// 弹层里抬起的那一本：`id` 用来查它那一组的行，标题是**开层那一刻**抄下的。
    /// 抄标题是因为退场那半程还要照常给内容（判据是 [`BottomSheetState::is_idle`]），
    /// 而那一本可能正好在这半程里被删空、从 `data` 里掉出去 —— 那时卡片不该沉成一个
    /// 没有名字的空框。
    sheet_book: Option<(i64, SharedString)>,
    /// 这一屏自己那块弹层的开合与停靠档。
    sheet: Entity<BottomSheetState>,
    /// 搜索框「词变了」的订阅。
    _search_watch: Subscription,
    /// 弹层「读者自己关掉了」的订阅。
    _sheet_watch: Subscription,
}

impl NotesView {
    /// 装起这一屏。搜索框的尺寸在**建的时候**按窗口档位定下（与架面同一条读法）：
    /// 手机上一条满幅的框用默认档（拇指要按得中），桌面那一档调 `.small()` 与整屏的
    /// 控件同高。窗口档位夹在 Medium 往上，不会中途换档，所以不必跟着 resize 重建。
    pub fn boot(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let compact = SizeClass::of(window).is_compact();
        let search = cx.new(|cx| {
            let search = SearchBox::new(tr(cx, "notes.search_placeholder"), window, cx);
            if compact { search } else { search.small() }
        });
        let _search_watch = cx.subscribe(&search, |this, _, event: &SearchBoxEvent, cx| {
            // `SearchBoxEvent` 现在只有 `Change` 一条：直接解构，将来加变体时编译器会
            // 逼着补齐。词在这里就折成小写 —— 比较的那一侧只折候选，不再折两遍。
            let SearchBoxEvent::Change(query) = event;
            this.query = query.to_lowercase().into();
            cx.notify();
        });
        let sheet = cx.new(BottomSheetState::closed);
        let _sheet_watch = cx.subscribe(&sheet, |_, _, _: &BottomSheetEvent, _| {
            // 关掉只改这一层自己的开合状态（握在 `BottomSheetState` 里）。这里不清
            // `sheet_book`：退场那半程还要靠它给内容与标题，等下一张卡抬起来时自然顶掉。
        });
        Self {
            data: None,
            store_missing: false,
            undo: None,
            undo_seq: 0,
            search,
            query: SharedString::default(),
            sheet_book: None,
            sheet,
            _search_watch,
            _sheet_watch,
        }
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

    /// 点开一张卡：把这一本的逐条笔记抬成底部弹层。
    fn show_book(&mut self, book_id: i64, cx: &mut Context<Self>) {
        // 标题在抬起这一刻从清单抄一份进 [`Self::sheet_book`]，理由见那个字段。
        let title: SharedString = self
            .data
            .as_ref()
            .and_then(|groups| groups.iter().find(|g| g.book_id == book_id))
            .map(|group| group.title.clone().into())
            .unwrap_or_default();
        self.sheet_book = Some((book_id, title));
        self.sheet.update(cx, |sheet, cx| sheet.show(cx));
        cx.notify();
    }

    /// 点了一条：报给外壳（开书 + 落点都在它手里）。
    fn open_entry(&mut self, book_id: i64, chapter: usize, offset: usize, cx: &mut Context<Self>) {
        // 跳原文先把这一层收掉。阅读层是盖在整窗之上的，两层遮罩叠着的话，返回键与
        // 点击命中都要各判一次谁是顶 —— 规范那条「别把浮层叠起来」。
        self.sheet.update(cx, |sheet, cx| sheet.hide(cx));
        cx.emit(NotesEvent::Open {
            book_id,
            jump: (chapter, offset),
        });
    }

    /// 列表上现在该摆的那几本：没在搜就全摆，搜了就只摆对得上的（顺序不变，仍然是
    /// 最近动静倒序 —— 搜索不重新排序，那会把「刚留过话的书」这一列改成按命中数排，
    /// 而读者要的是原来那一列里少掉几本）。
    fn visible(&self) -> Vec<&BookGroup> {
        let Some(groups) = self.data.as_ref() else {
            return Vec::new();
        };
        if self.query.is_empty() {
            return groups.iter().collect();
        }
        groups
            .iter()
            .filter(|group| matches_query(group, self.query.as_str()))
            .collect()
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

    /// 一张卡：左边一格封面，右边从上往下三行 —— 留了多少条、书名、累计读了多久。
    ///
    /// 卡片靠**描边**立住、不靠填色也不靠投影，与设置那一屏的分组卡同一套（整屏只有一层
    /// 表面，区域由描边与留白来定；规范那条「别给每张卡都加投影」）。边框四周 16、内容
    /// 之间 12：容器在边界上花的比在内部多。
    fn book_card(&self, group: &BookGroup, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (notes_count, bookmarks_count) = counts(group);
        let counts: SharedString = tf(
            cx,
            "notes.group_counts",
            &[("notes", &notes_count), ("bookmarks", &bookmarks_count)],
        )
        .into();
        let read_time = duration(cx, group.seconds);
        let title: SharedString = group.title.clone().into();
        let cover = group.cover.clone();
        let book_id = group.book_id;
        let tapped = cx.entity().clone();
        // 闭包要的是**值**：`cx.theme()` 借来的那一份活不到下一帧。
        let hover_wash = theme.muted;
        div()
            // 元素 id 用书库主键而不是标题：重取一次快照它不变，而标题会随导入更新，
            // 拿标题当键会让一张卡每次改名都丢掉自己的按下态。挂个名也是给宿主测试留的
            // 口子（`try_find` 只认 `test_support` 的元素）；生产构建里这一层是透明的。
            .id(format!("notes-card-{book_id}"))
            .test_support()
            .w_full()
            .flex()
            .items_center()
            .gap_3()
            .p_4()
            .rounded(theme.radius_lg)
            // GPUI 不保证圆角的父容器裁掉孩子 —— 不裁的话封面那一格的方角会从卡角外头
            // 露出来（规范那条「一个圆角要管住整张可见表面」）。
            .overflow_hidden()
            .border_1()
            .border_color(theme.border)
            .cursor_pointer()
            .hover(|style| style.bg(hover_wash))
            // 按下去当场暗一截，与架上那张卡同一套回话；抬起不做动画 —— 指针跟着手指走，
            // 慢半拍的高亮比没有高亮更容易指错。
            .active(|style| style.opacity(0.75))
            .on_click(move |_, _, cx| tapped.update(cx, |this, cx| this.show_book(book_id, cx)))
            .child(cover_slot(cover, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(meta_line(IconName::Highlighter, counts, cx))
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(meta_line(IconName::Clock, read_time, cx)),
            )
            // 右端那条固定的箭头条：它说「这格点得进去，而且进去还有东西看」，同时让一列
            // 卡片的右边界落在同一条脊线上（规范那条「尾随动作走定宽的那一条道」）。
            .child(
                div()
                    .flex_none()
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(IconName::ChevronRight).small()),
            )
            .into_any_element()
    }

    /// 这一本的逐条笔记那一层：一块底部弹层，停在「展开 / 半屏」两档上，往下拖过头就关掉。
    ///
    /// 换成弹层而不是新的一页，是为了拿到**同一套退场**：下拉过头、点遮罩、按返回三条路都
    /// 收这一层，与目录、更新日志那两块同一个容器，不必再为这一屏接一条返回键的路。
    ///
    /// 收稳了那一程**照样要交元素**（弹簧的采样在里面），只是不给内容 —— 判据是
    /// [`BottomSheetState::is_idle`]，不是 `is_open()`：退场那半程 `is_open()` 已经是
    /// `false` 而卡片还浮在屏幕上，那时断掉内容就等于看着它空着沉下去。
    ///
    /// 列表自己会滚，所以这里**不开** `surface_draggable`：整块面板跟着手指走会跟内容
    /// 抢同一根手指，而「滚到顶了再接着往下拽」这一步 gpui 给不出偏移。
    fn sheet_element(&self, cx: &mut Context<Self>) -> AnyElement {
        let sheet = BottomSheet::new(&self.sheet, "notes-book")
            .expanded(SHEET_EXPANDED)
            .peek(SHEET_PEEK);
        if self.sheet.read(cx).is_idle() {
            return sheet.into_any_element();
        }
        let Some((book_id, title)) = self.sheet_book.clone() else {
            return sheet.into_any_element();
        };
        let header = h_flex()
            .w_full()
            .items_center()
            .gap_2()
            // `header` 那一槽只做「不跟着滚」，内缩由调用侧给 —— 这里给 `GUTTER`，
            // 于是弹层里的行与底下那列卡片是同一条左边界。
            .px(px(GUTTER))
            .pt_1()
            .pb_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(self.export_button(book_id, cx));
        // 这一本的行，从清单里按 id 现查。查不到只有一种来路：最后一条正是在这一层里
        // 被删掉的 —— 那就摆一句「还没有」，别摆一片看起来像坏了的空白。
        let rows: Vec<AnyElement> = self
            .data
            .as_ref()
            .and_then(|groups| groups.iter().find(|group| group.book_id == book_id))
            .map(|group| {
                group
                    .marks
                    .iter()
                    .map(|entry| self.mark_row(group, entry, cx))
                    .collect()
            })
            .unwrap_or_default();
        let body = v_flex()
            // 挂个名给宿主测试：判「抬起来时那一栏真给了内容」，而不是一个空壳沉下去。
            .id("notes-sheet-body")
            .test_support()
            .w_full()
            .px(px(GUTTER))
            // `pb_6`：滚到最末一行时别让最后一句贴住卡片下沿，与更新日志那一张卡同一档。
            .pb_6()
            .when(rows.is_empty(), |this| {
                // 库里没有批注时这颗框根本不摆（见 `render` 那一条），所以走到这里
                // 只有一种来路：最后一条正是在这一层里被删掉的。那就摆一句「还没有」，
                // 别摆一片看起来像坏了的空白。
                this.child(
                    div()
                        .w_full()
                        .pt_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr(cx, "reader.marks.none")),
                )
            })
            .children(rows)
            .into_any_element();
        sheet.header(header).child(body).into_any_element()
    }

    /// 「把这一本的批注导出成文件」那颗「⋯」。挂在弹层 header 上：它的作用对象是整本，
    /// 不是某一行。三档格式各一条菜单项，每次打开重建一遍（builder 是 `Fn`）。
    fn export_button(&self, book_id: i64, cx: &mut Context<Self>) -> AnyElement {
        let panel = cx.entity().clone();
        let label = tr(cx, "notes.export");
        Button::new("notes-export")
            .ghost()
            .small()
            .icon(IconName::CircleEllipsis)
            .tooltip(label.clone().to_string())
            .accessibility_label(label)
            .dropdown_menu_with_anchor(Anchor::BottomRight, move |menu, _, cx| {
                let (txt_panel, md_panel, html_panel) =
                    (panel.clone(), panel.clone(), panel.clone());
                let txt_label = tr(cx, "notes.export_text");
                let md_label = tr(cx, "notes.export_markdown");
                let html_label = tr(cx, "notes.export_html");
                menu.min_w(px(200.))
                    .item(PopupMenuItem::new(txt_label).on_click(move |_, _, cx| {
                        txt_panel.update(cx, |this, cx| {
                            this.export_group(book_id, AnnotationExportFormat::Text, cx)
                        })
                    }))
                    .item(PopupMenuItem::new(md_label).on_click(move |_, _, cx| {
                        md_panel.update(cx, |this, cx| {
                            this.export_group(book_id, AnnotationExportFormat::Markdown, cx)
                        })
                    }))
                    .item(PopupMenuItem::new(html_label).on_click(move |_, _, cx| {
                        html_panel.update(cx, |this, cx| {
                            this.export_group(book_id, AnnotationExportFormat::Html, cx)
                        })
                    }))
            })
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
            (Some(_), false) => {
                let visible = self.visible();
                if visible.is_empty() {
                    // 库里有话，只是这一个词没对上任何一本 —— 与「还没有批注」是两句话。
                    empty_state(tr(cx, "nav.notes"), tr(cx, "notes.no_match"), cx)
                        .into_any_element()
                } else {
                    v_flex()
                        .w_full()
                        .gap_3()
                        .children(visible.iter().map(|group| self.book_card(group, cx)))
                        .into_any_element()
                }
            }
            (None, false) => {
                empty_state(tr(cx, "nav.notes"), tr(cx, "nav.pending.notes"), cx).into_any_element()
            }
        };
        // 搜索框那一行钉在滚动区**外面**：列表滚起来时框跟着走就找不着了（与架面同一条
        // 读法）。摆不摆看的是「库里有没有东西可搜」，不是这一帧筛剩几本 —— 筛空的时候
        // 框必须在，否则读者没法把那个词清掉；一处批注都没有时摆一颗框才是纯噪音。
        // 横向与下面那列卡片同一把尺：同一个 `COLUMN_MAX` 封顶、同一个 `GUTTER` 内缩，
        // 于是框的左边界与第一张卡的左边界落在同一条脊线上。
        let search_row = self
            .data
            .as_ref()
            .is_some_and(|groups| !groups.is_empty())
            .then(|| {
                v_flex()
                    // 挂个名给宿主测试：那颗框在不在，是「有没有东西可搜」的判据。
                    .id("notes-search")
                    .test_support()
                    .w_full()
                    .items_center()
                    .flex_shrink_0()
                    .child(
                        div()
                            .w_full()
                            .when(wide, |el| el.max_w(px(COLUMN_MAX)))
                            .px(px(GUTTER))
                            .py_2()
                            .child(self.search.clone()),
                    )
            });
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
            .children(search_row)
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
            // 弹层挂在这一屏的根上：它底下那层宿主自己 anchored 到整窗，遮罩因此盖得住
            // 导航栏，而不是只压住这一屏那一格。
            .child(self.sheet_element(cx))
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
///
/// 卡片上那两格新数（累计时长与封面）也在这里一并取好 —— 整趟只在 [`NotesView::refresh`]
/// 里跑一次，`render` 拿到的已经是能直接画的东西。
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
                seconds: 0,
                cover: None,
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
            seconds: 0,
            cover: None,
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
    // 封面的那两格描述（书库根 + 封面名分）只有 `shelf()` 一条语句给，所以取一次整架再
    // 只留下这一屏用得到的那几本：界面不该自己再从 `books.path` 反推一遍书库根 ——
    // 那是第二处真相。
    let mut cover_refs: HashMap<i64, (PathBuf, folio_core::content::format::cover::CoverRef)> =
        HashMap::new();
    for item in store.shelf("").unwrap_or_default() {
        if let (Some(root), Some(cover)) = (item.cover_root, item.cover_ref)
            && groups.contains_key(&item.id)
        {
            cover_refs.insert(item.id, (root, cover));
        }
    }
    // 每组一张章标题表：出处那一句在这里填 —— 这一屏没有排版可问，库里那张表是权威。
    for group in groups.values_mut() {
        let titles = store.chapter_titles(group.book_id).unwrap_or_default();
        // 累计时长是每本一条聚合语句，行数就是「留过话的书」那几本；读不到按 0 处理
        // （「还没读过」不是异常）。
        group.seconds = store
            .book_reading(group.book_id)
            .map(|reading| reading.seconds)
            .unwrap_or_default();
        group.cover = list_cover(store, cover_refs.get(&group.book_id), group.book_id);
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

/// 卡片上那行读数的两个数：批注几条、书签几枚。就在本屏那份清单里数，不另查库。
fn counts(group: &BookGroup) -> (usize, usize) {
    let notes = group.marks.iter().filter(|mark| !mark.is_bookmark).count();
    (notes, group.marks.len() - notes)
}

/// 搜索词落在一本书上算不算对上：书名，或这一本里任何一条引文、笔记与出处。
///
/// 词在订阅那一刻已经折成小写，这里只折候选。出处也要对：读者记不住的往往是「这句
/// 出在第几章」，而章名是他手上唯一的名词。
fn matches_query(group: &BookGroup, query: &str) -> bool {
    let hits = |text: &str| text.to_lowercase().contains(query);
    hits(&group.title)
        || group
            .marks
            .iter()
            .any(|mark| hits(&mark.quote) || hits(&mark.note) || hits(&mark.place))
}

/// 一张卡那一格封面的字节 → 可画的图。
///
/// **先试磁盘上那一份列表档缩略图**（`.thumbs/96/`，导入时与卡片档一起生成，专为
/// 「一行一本」那种小图）：卡片那一格只有 48×72，用不上原图的像素，而 `Image` 持有的
/// 是编码字节，省下来的是整列的常驻内存。取不到（老库没生成、写不进目录）就**退回原图**
/// —— 缩略图是可丢弃的加速品，不是功能的入口。原图字节只在缩略图这条路走不通时才去读，
/// 免得为一格小图做两次文件 IO。
fn list_cover(
    store: &folio_core::Store,
    cover: Option<&(PathBuf, folio_core::content::format::cover::CoverRef)>,
    book_id: i64,
) -> Option<Arc<Image>> {
    let small = cover.and_then(|(root, description)| {
        let path = folio_core::content::format::thumb::thumb_path(
            root,
            folio_core::infra::paths::THUMB_LIST_PX,
            description,
        );
        std::fs::read(path).ok()
    });
    if let Some(image) = small.as_deref().and_then(cover_image) {
        return Some(image);
    }
    let full = store.book_cover(book_id).ok().flatten();
    full.as_deref().and_then(cover_image)
}

/// 封面那一格：有图铺图，没图画一块灰底。旁边那三行写着这本书叫什么、留了多少话，
/// 灰底不会读成「这本坏了」—— 与目录面板书头那一格同一个画法。
fn cover_slot(cover: Option<Arc<Image>>, cx: &App) -> AnyElement {
    div()
        .flex_none()
        .w(px(COVER_W))
        .h(px(COVER_H))
        .overflow_hidden()
        // 卡角是 `radius_lg`，这一格是它里面的一层：内角按 `max(0, 外角 - 间距)` 取更紧
        // 的那一档，转过去的那一段才看得出是一条均匀的带。
        .rounded(cx.theme().radius_tokens().sm)
        .bg(cx.theme().muted)
        .when_some(cover, |this, image| {
            this.child(img(image).size_full().object_fit(ObjectFit::Cover))
        })
        .into_any_element()
}

/// 卡上一条次要读数：一枚图标 + 一句话。
///
/// 图标占一个不跟着字长变的槽，字放不下就省略号 —— 「N 条批注 · M 枚书签」在九门语言里
/// 长短不一，而字号与颜色已经把这一定成次要，不必为某一门语言改尺。
fn meta_line(icon: IconName, text: SharedString, cx: &App) -> AnyElement {
    h_flex()
        .w_full()
        .items_center()
        .gap_1()
        .overflow_hidden()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(div().flex_none().child(Icon::new(icon).xsmall()))
        .child(div().min_w_0().truncate().child(text))
        .into_any_element()
}

#[cfg(test)]
mod tests;
