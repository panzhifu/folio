use std::sync::Arc;

use folio_core::store::BookRow;
use folio_core::{Chapter, FlowDoc, Locator, Reader};
use gpui_kit::base::VirtualListScrollHandle;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::{
    App, AppContext as _, Context, Entity, InteractiveElement, IntoElement, ParentElement, Pixels,
    Render, SharedString, Styled, Subscription, TestSupportExt, Window, div, point, px,
};

use super::engine::{engine_lines, item_sizes, item_top, line_at_item_top, render_range};
use crate::ui::shared::document::Document;
use crate::ui::shared::images::{BookImages, ChapterImages};
use crate::ui::shared::l10n::tf;

/// 用自研引擎渲染的格式。EPUB 的 XHTML 直接进 DOM → CSS → Layout；
/// Markdown / TXT 还没接进引擎（要先把源文本转成 XHTML，见 M5），
/// 仍走 M1 的 `TextView` 占位实现。
fn uses_engine(format: &str) -> bool {
    matches!(format, "epub")
}

pub struct ReaderView {
    book_id: i64,
    title: SharedString,
    chapters: Vec<Chapter>,
    chapter: usize,
    /// `Some` 时正文由自研引擎排版，`None` 时是 M1 的 TextView。
    engine: Option<Document>,
    /// 整本书的图片索引，换章时按新章的目录重建视图（见 `reload`）。
    book_images: Arc<BookImages>,
    state: Entity<TextViewState>,
    prefs: Reader,
    /// 外壳里不属于阅读栏的宽度（展开的侧栏）。引擎的版心必须扣掉它，
    /// 否则会按整窗宽度折行、被窄栏裁掉。
    pane_reserve: f32,
    /// 等正文解析完（`rendered_text` 非空）再落地。
    /// markdown 解析是异步的，构造时直接 reveal_range 会因文本为空而失败；
    /// 而 TextViewState 不实现 EventEmitter，所以用 `observe` 在它每次变化时重试。
    pending_restore: Option<Locator>,
    /// 引擎那张虚拟列表的滚动句柄。**视图持有**：进度要从这里读偏移、
    /// 恢复要往这里写行号，每帧新建就等于两条路都读不到东西。
    engine_scroll: VirtualListScrollHandle,
    _observe_state: Subscription,
}

impl ReaderView {
    pub fn open(
        book: &BookRow,
        flow: FlowDoc,
        prefs: Reader,
        locator: Locator,
        cx: &mut Context<Self>,
    ) -> Self {
        let chapter = (locator.chapter as usize).min(flow.chapters.len().saturating_sub(1));
        let source = flow
            .chapters
            .get(chapter)
            .map(|c| c.source.as_str())
            .unwrap_or_default()
            .to_string();
        let spine_href = flow
            .chapters
            .get(chapter)
            .map(|c| c.spine_href.as_str())
            .unwrap_or_default()
            .to_string();
        let book_images = Arc::new(BookImages::new(&flow.images));
        let engine = uses_engine(&book.format).then(|| {
            let images = Arc::new(ChapterImages::new(book_images.clone(), &spine_href));
            Document::new(source.clone(), &flow.css, prefs.clone(), images)
        });
        // 引擎路径不用 TextViewState，但仍要有一个句柄占位：`observe` 与
        // `reveal_range` 那套只对 TextView 有意义，引擎的位置恢复在 M5。
        let state = if engine.is_some() {
            cx.new(|cx| TextViewState::markdown("", cx))
        } else {
            cx.new(|cx| TextViewState::markdown(&source, cx).scrollable(true))
        };
        let observe_state = cx.observe(&state, |this, _, cx| {
            if this.apply_pending_restore(cx) {
                cx.notify();
            }
        });
        Self {
            book_id: book.id,
            title: book.title.clone().into(),
            chapters: flow.chapters,
            chapter,
            engine,
            book_images,
            state,
            prefs,
            pane_reserve: 0.0,
            pending_restore: (locator.offset > 0).then_some(locator),
            engine_scroll: VirtualListScrollHandle::new(),
            _observe_state: observe_state,
        }
    }

    /// 本章可见文本的字数（锚点坐标系的长度）。状态栏的「共 N 字」与恢复/跳转
    /// 的目标位置都从它推。TextView 那条路径还没接坐标表，所以给 0。
    pub fn text_length(&self) -> usize {
        self.engine
            .as_ref()
            .map_or(0, |engine| engine.text_map().chars())
    }

    /// 引擎算好的行几何。测试与状态栏都从这里读，不在测试里另搭近似视图。
    pub fn engine_lines(&self) -> Option<&[folio_core::layout::LineBox]> {
        self.engine.as_ref().map(|e| e.lines())
    }

    /// 每行的可见文本，与 `engine_lines()` 一一对应。
    pub fn engine_line_texts(&self) -> Option<Vec<String>> {
        self.engine.as_ref().map(|e| e.line_texts())
    }

    /// 外壳在每次渲染时报告阅读栏之外的宽度。没变就不 notify，避免父子互相触发。
    pub fn set_pane_reserve(&mut self, reserve: f32, cx: &mut Context<Self>) {
        if (self.pane_reserve - reserve).abs() > 0.5 {
            self.pane_reserve = reserve;
            cx.notify();
        }
    }

    pub fn is_engine(&self) -> bool {
        self.engine.is_some()
    }

    pub fn book_id(&self) -> i64 {
        self.book_id
    }

    pub fn title(&self) -> &SharedString {
        &self.title
    }

    pub fn mode_name(&self) -> &'static str {
        "continuous"
    }

    pub fn chapter_label(&self) -> String {
        self.chapters
            .get(self.chapter)
            .map(|chapter| chapter.title.clone())
            .unwrap_or_default()
    }

    /// 状态栏用的「第几节 / 共几节」，比整本书名短得多。
    pub fn chapter_position(&self, cx: &App) -> String {
        if self.chapters.is_empty() {
            return String::new();
        }
        tf(
            cx,
            "reader.chapter_position",
            &[
                ("index", &(self.chapter + 1)),
                ("total", &self.chapters.len()),
            ],
        )
    }

    /// 已滚过的比例。用它而不是页码来换算位置：字号与窗口宽度变化后
    /// 页码失效，但同一章内的相对位置基本守恒。
    ///
    /// 滚动条由 TextView 内部的 list 持有（`scrollable(true)`），所以进度从它的
    /// `list_state()` 读。外面再套一层滚动容器是错的：内层 list 自己吃掉视口高度，
    /// 外层量不到内容高度，`max_offset` 恒为 0。
    pub fn progress(&self, cx: &App) -> f32 {
        if self.is_engine() {
            // 引擎路径读虚拟列表自己的滚动偏移。gpui 的约定是「往下滚为负」，
            // 与下面 TextView 那条同一套符号处理：取绝对值相除，不猜正负号。
            let scrolled = f32::from(self.engine_scroll.offset().y).abs();
            let reachable = f32::from(self.engine_scroll.max_offset().y).abs();
            if reachable <= 0.0 {
                return 0.0;
            }
            return (scrolled / reachable).clamp(0.0, 1.0);
        }
        let list = self.state.read(cx).list_state();
        let max = list.max_offset_for_scrollbar().y;
        // gpui 的滚动偏移向下为负（与 `scroll_px_offset_for_scrollbar` 一致），
        // 直接相除会得到负比例。
        let now = list.scroll_px_offset_for_scrollbar().y;
        if max <= px(0.0) {
            0.0
        } else {
            (-now / max).clamp(0.0, 1.0)
        }
    }

    /// 把当前滚动位置折算成「第几章第几个字符」。
    /// `RenderedText` 只有 source→rendered 单向映射，所以这里二分反解。
    ///
    /// 探针必须落在**字符边界**上：中文一字三字节，用 `mid..mid+1` 这种单字节
    /// 探针会大量落到边界外而返回 None，反解就会被一路推向章首。
    pub fn locator(&self, cx: &App) -> Locator {
        if let Some(engine) = self.engine.as_ref() {
            // 可见文本的字符偏移：字号、行距、版心宽、分页模式怎么改，这个数都不动。
            // 像素先换成**行号**（在列表自己的坐标空间里，见 `line_at_item_top`），
            // 再由共用层把行号换成坐标 —— 中间那一跳不经过布局绝对 y，
            // 所以块间 margin 不会把滚动位置和文字位置错开。
            let scrolled = f32::from(self.engine_scroll.offset().y).abs();
            let line = line_at_item_top(engine, scrolled);
            return Locator {
                chapter: self.chapter as u32,
                offset: engine.coordinate_of_line(line),
            };
        }
        fn next_boundary(s: &str, from: usize) -> usize {
            let mut p = from.min(s.len());
            while p < s.len() && !s.is_char_boundary(p) {
                p += 1;
            }
            p
        }

        let rendered = self.state.read(cx).rendered_text();
        let source = rendered.source();
        let total = rendered.len();
        let target = (self.progress(cx) * total as f32).floor() as usize;

        let (mut lo, mut hi) = (0usize, source.len());
        while lo < hi {
            // mid 必须严格在 (lo, hi) 内：吸附到字符边界后很容易撞到 hi，
            // 那样 hi = mid 不缩小、lo = end 也不前进，整个循环就转不出来。
            let mid = next_boundary(source, lo + (hi - lo) / 2).min(hi - 1);
            if mid <= lo {
                break;
            }
            let end = next_boundary(source, mid + 1).max(mid + 1).min(hi);
            match rendered.range_for_source(mid..end) {
                // 空白与 markdown 语法字符不产生渲染输出，往左收。
                None => hi = mid,
                Some(range) if range.start >= target => hi = mid,
                Some(_) => lo = end,
            }
        }
        Locator::from_byte(self.chapter as u32, source, lo)
    }

    /// 滚动几何：(已滚像素, 可滚上限, list 项数)。状态栏与测试都用它定位问题，
    /// 因为「滚不动」有三种完全不同的成因。
    pub fn scroll_metrics(&self, cx: &App) -> (Pixels, Pixels, usize) {
        let list = self.state.read(cx).list_state();
        (
            list.scroll_px_offset_for_scrollbar().y,
            list.max_offset_for_scrollbar().y,
            list.item_count(),
        )
    }

    pub fn apply_prefs(&mut self, prefs: Reader, cx: &mut Context<Self>) {
        if let Some(engine) = self.engine.as_mut() {
            engine.set_prefs(prefs.clone());
        }
        self.prefs = prefs;
        cx.notify();
    }

    pub fn next_chapter(&mut self, cx: &mut Context<Self>) {
        if self.chapter + 1 < self.chapters.len() {
            self.chapter += 1;
            self.reload(cx);
        }
    }

    pub fn prev_chapter(&mut self, cx: &mut Context<Self>) {
        if self.chapter > 0 {
            self.chapter -= 1;
            self.reload(cx);
        }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let source = self.chapters[self.chapter].source.clone();
        let spine_href = self.chapters[self.chapter].spine_href.clone();
        // 换章即作废未落地的恢复请求，否则会把上一章的位置套到本章上。
        self.pending_restore = None;
        if let Some(engine) = self.engine.as_mut() {
            // 引擎自己按 (宽度, 字号, 行距) 缓存排版结果；换章要显式作废。
            // 图片的相对路径基准目录随章变化，所以索引也要重建。
            engine.set_chapter(source);
            engine.set_images(Arc::new(ChapterImages::new(
                self.book_images.clone(),
                &spine_href,
            )));
            cx.notify();
            return;
        }
        self.state.update(cx, |state, cx| {
            state.set_text(&source, cx);
            state
                .list_state()
                .set_offset_from_scrollbar(point(px(0.0), px(0.0)));
        });
        cx.notify();
    }

    /// 恢复请求是否还挂着（false 表示已落地或被放弃）。
    pub fn has_pending_restore(&self) -> bool {
        self.pending_restore.is_some()
    }

    /// 正文解析完成后才落地恢复：源偏移 → 渲染区间 → 交给 TextView 的 reveal_range。
    /// 返回 false 表示文本还是空的（异步解析未完），下次变化再试。
    fn apply_pending_restore(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(locator) = self.pending_restore else {
            return true;
        };
        let rendered = self.state.read(cx).rendered_text();
        if rendered.is_empty() {
            return false;
        }
        let byte = locator.byte_of(rendered.source());
        self.pending_restore = None;
        if let Some(range) = rendered.range_for_source(byte..byte + 1) {
            let _ = self
                .state
                .update(cx, |state, cx| state.reveal_range(range, cx));
        }
        true
    }

    /// 跳到章内第 `offset` 个字那一行的行顶。
    ///
    /// 目录跳转、书签、恢复进度共用这一个入口。落点是**列表自己的坐标空间**
    /// （逐项高度累加，见 `engine::item_top`），不是排版的绝对 `y` —— 后者含块间
    /// margin，列表项不含，拿错一套就会「恢复后差半屏」。
    pub fn jump_to_offset(&mut self, offset: usize) {
        let Some(engine) = self.engine.as_ref() else {
            // TextView 那条路径的位置恢复走 `reveal_range`，与坐标不同源，别混在这里。
            return;
        };
        let top = item_top(engine, engine.line_at_offset(offset));
        self.engine_scroll.set_offset(point(px(0.0), -px(top)));
    }

    /// 引擎路径的位置恢复：坐标 → 行号 → 列表滚到那一行的行顶。
    ///
    /// 必须在 `ensure_layout` 之后调用（要的是这一帧的行盒）—— 排版没做过时
    /// `line_at_offset` 只会给第 0 行，那是「章首」不是「上次的位置」。
    fn apply_engine_restore(
        engine: &Document,
        pending: &mut Option<Locator>,
        scroll: &VirtualListScrollHandle,
    ) {
        // 三个参数分开传，是为了躲开一个纯 borrow-checker 的形状问题：渲染帧里
        // `self.engine` 已经借成 `&mut`，再调 `&mut self` 的方法就是二次可变借用。
        // 字段之间互不相干，逐个借出来就行。
        let Some(locator) = pending.take() else {
            return;
        };
        let top = item_top(engine, engine.line_at_offset(locator.offset));
        scroll.set_offset(point(px(0.0), -px(top)));
    }
}

impl Render for ReaderView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(engine) = self.engine.as_mut() {
            engine.ensure_layout(window, cx, self.pane_reserve);
            Self::apply_engine_restore(engine, &mut self.pending_restore, &self.engine_scroll);
            let width = px(Document::content_width(
                window,
                &self.prefs,
                self.pane_reserve,
            ));
            let sizes = item_sizes(engine);
            let view = cx.entity();
            return div()
                .id("engine-scroll")
                .test_support()
                .size_full()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .px(px(self.prefs.margin_x))
                .child(div().mx_auto().w(width).h_full().child(engine_lines(
                    view,
                    sizes,
                    &self.engine_scroll,
                    |view: &mut Self, range, _window, cx| view.render_engine_range(range, cx),
                )))
                .into_any_element();
        }
        // 解析未完时这里返回 false，留到下一次渲染再落地；落地后 reveal_range
        // 自己 notify 一帧，因为 pending_restore 已被取走，不会循环。
        self.apply_pending_restore(cx);
        let prefs = self.prefs.clone();
        // `max_measure == 0` 是「不设上限、随窗口铺满」的哨兵值，所以只有真设了上限
        // 才加 `max_w`：把它当 0 宽的上限用，这一栏就塌成零宽。
        let column = div().mx_auto().w_full().h_full();
        let column = if prefs.max_measure > 0.0 {
            column.max_w(px(prefs.max_measure))
        } else {
            column
        };
        div()
            .id("reader-scroll")
            .test_support()
            .size_full()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .px(px(prefs.margin_x))
            .child(gpui_kit::base::TextSelectionLayer)
            .child(
                column.child(
                    TextView::new(&self.state)
                        .scrollable(true)
                        .selectable(true)
                        .text_size(px(prefs.font_size))
                        .line_height(px(prefs.font_size * prefs.line_height))
                        .font_family(prefs.font_family.clone())
                        .text_color(cx.theme().foreground),
                ),
            )
            .into_any_element()
    }
}

impl ReaderView {
    /// 虚拟列表回调：把 `[start, end)` 的行盒画出来。
    fn render_engine_range(
        &mut self,
        range: std::ops::Range<usize>,
        cx: &mut Context<Self>,
    ) -> Vec<gpui_kit::AnyElement> {
        match self.engine.as_ref() {
            Some(engine) => render_range(engine, range, cx),
            None => Vec::new(),
        }
    }
}

impl crate::ui::features::reader::ReaderHost for ReaderView {
    fn book_id(&self) -> Option<i64> {
        Some(self.book_id)
    }

    fn title(&self) -> gpui_kit::SharedString {
        self.title.clone()
    }

    fn position_label(&self, cx: &gpui_kit::App) -> String {
        let title = self.chapter_label();
        let position = self.chapter_position(cx);
        match (title.is_empty(), position.is_empty()) {
            (false, false) => format!("{title} · {position}"),
            (false, true) => title,
            (true, false) => position,
            (true, true) => String::new(),
        }
    }

    fn mode_name(&self) -> &'static str {
        ReaderView::mode_name(self)
    }

    fn can_advance(&self, delta: isize) -> bool {
        let target = self.chapter as isize + delta;
        (0..self.chapters.len() as isize).contains(&target)
    }

    fn advance_chapter(&mut self, delta: isize, cx: &mut gpui_kit::Context<Self>) {
        if delta < 0 {
            self.prev_chapter(cx);
        } else if delta > 0 {
            self.next_chapter(cx);
        }
    }

    fn set_reserve(&mut self, reserve: f32, cx: &mut gpui_kit::Context<Self>) {
        self.set_pane_reserve(reserve, cx);
    }

    fn persist(&mut self, cx: &mut gpui_kit::Context<Self>) {
        let Some(store) = crate::ui::shared::app_store::handle(cx) else {
            return;
        };
        let locator = self.locator(cx);
        if let Err(err) = store.set_position(self.book_id, ReaderView::mode_name(self), &locator) {
            log::warn!("ReaderView::persist: 写入失败 {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    //! M1 验收门：证明「滚动 → 折算字符偏移 → 改字号 → 恢复位置」这条回路成立。
    //! 用生产视图 `ReaderView`，不在测试里另搭近似视图；滚动走真实滚轮事件，
    //! 因为直接 set_offset 会绕过要测的那段行为。
    //!
    //! 这块跟着 `desktop` 一起编译 —— `reader/mod.rs` 只在 `cfg(not(mobile))` 下挂它，
    //! 所以不用再叠一层 `not(mobile)`；移动端那一侧的渲染由 `page_mobile` 负责。
    //! 导入点名而不 `use super::*`：`gpui_kit` 根把 `gpui` 整个转了出来，里面有一个
    //! 叫 `test` 的属性宏，glob 会让下面的 `#[test]` 递归展开成它。

    use super::ReaderView;
    use folio_core::store::BookRow;
    use folio_core::{Chapter, FlowDoc, Locator, Reader};
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Entity, ScrollDelta, TestAppContext, point, px, size,
    };

    /// 一段足够长的中文，保证它会折行 —— 不折行的话「缩进改变了布局」这种断言无从成立。
    const PARA: &str = "于是我们奋力向前，逆水行舟，被不断地向后推，推入过去。";

    fn fixture() -> FlowDoc {
        // 分成多个自然段：TextView 内部的 list 按块虚拟化，
        // 一整篇只有一个段落的话，它就是一个比视口还高的 list 项。
        let source = (0..80)
            .map(|i| format!("{PARA}（第 {i} 段）\n\n"))
            .collect::<String>();
        FlowDoc {
            chapters: vec![Chapter {
                title: "第一章".into(),
                source,
                spine_href: String::new(),
            }],
            ..Default::default()
        }
    }

    fn book() -> BookRow {
        book_as("markdown")
    }

    fn book_as(format: &str) -> BookRow {
        BookRow {
            id: 1,
            path: "memory://fixture".into(),
            title: "测试书".into(),
            authors: Vec::new(),
            format: format.to_string(),
            size: 0,
            last_opened_at: None,
        }
    }

    fn open(
        cx: &mut TestAppContext,
        flow: &FlowDoc,
        prefs: Reader,
        locator: Locator,
    ) -> (Entity<ReaderView>, AnyWindowHandle) {
        open_as(cx, flow, "markdown", prefs, locator)
    }

    fn open_as(
        cx: &mut TestAppContext,
        flow: &FlowDoc,
        format: &str,
        prefs: Reader,
        locator: Locator,
    ) -> (Entity<ReaderView>, AnyWindowHandle) {
        let row = book_as(format);
        let mut view = None;
        let handle = cx.open_window(size(px(460.), px(520.)), |window, cx| {
            let created =
                cx.new(|cx| ReaderView::open(&row, flow.clone(), prefs.clone(), locator, cx));
            view = Some(created.clone());
            Root::new(created, window, cx)
        });
        let view = view.expect("ReaderView 应该被创建");
        // 两轮「跑空事件 → 渲染一帧」。一轮不够：markdown 解析是异步的，
        // 首帧才把 reveal_range 请求挂上去，而 run_until_parked 只跑事件不画帧。
        // 框架自己的 settle() 也是这个形状。
        settle(cx, handle.into());
        (view, handle.into())
    }

    fn settle(cx: &mut TestAppContext, handle: AnyWindowHandle) {
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("渲染应成功");
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("第二帧应成功");
        cx.run_until_parked();
    }

    fn scroll_down(cx: &mut TestAppContext, handle: AnyWindowHandle, pixels: f32) {
        cx.update_window(handle, |_, window, cx| {
            window.scroll(
                "reader-scroll",
                ScrollDelta::Pixels(point(px(0.0), px(-pixels))),
                cx,
            );
        })
        .expect("滚动应送达阅读视图");
        cx.run_until_parked();
    }

    fn progress_of(cx: &mut TestAppContext, view: &Entity<ReaderView>) -> f32 {
        let view = view.clone();
        cx.update(|cx| view.read(cx).progress(cx))
    }

    fn locator_of(cx: &mut TestAppContext, view: &Entity<ReaderView>) -> Locator {
        let view = view.clone();
        cx.update(|cx| view.read(cx).locator(cx))
    }

    /// 一张 1×1 的合法 PNG，用来喂给引擎（解码由 gpui 负责，这里只要能过 header）。
    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    fn epub_flow() -> FlowDoc {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/oss/minimal-zh.epub");
        let (_, content) = folio_core::open(&path).expect("合成 EPUB 应能打开");
        let folio_core::Content::Reflow(flow) = content;
        flow
    }

    #[gpui_kit::test]
    fn wheel_scrolling_moves_the_reading_position(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = fixture();
        let (view, handle) = open(
            cx,
            &flow,
            Reader {
                font_size: 16.0,
                ..Default::default()
            },
            Locator::start(),
        );

        assert_eq!(
            progress_of(cx, &view),
            0.0,
            "刚打开时应在章首，不该已有位移"
        );

        scroll_down(cx, handle, 900.0);
        let progress = progress_of(cx, &view);
        assert!(
            progress > 0.05,
            "真实滚轮事件应产生可测位移，实际比例 {progress:.3}"
        );
    }

    #[gpui_kit::test]
    fn locator_matches_the_scrolled_ratio(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = fixture();
        let total = flow.chapters[0].char_len();
        assert!(total > 1000, "语料要够长才滚得动，实际 {total} 字");

        let (view, handle) = open(
            cx,
            &flow,
            Reader {
                font_size: 16.0,
                ..Default::default()
            },
            Locator::start(),
        );
        assert_eq!(locator_of(cx, &view).offset, 0, "未滚动时偏移应为 0");

        scroll_down(cx, handle, 900.0);
        let progress = progress_of(cx, &view);
        let locator = locator_of(cx, &view);

        assert!(locator.offset > 0, "滚动后偏移应前进");
        assert!(locator.offset < total, "偏移不应越出章尾");
        // 二分反解的正确性：字符偏移折算回比例，应与滚动比例吻合。
        let ratio_from_offset = locator.offset as f32 / total as f32;
        assert!(
            (ratio_from_offset - progress).abs() < 0.05,
            "偏移↔比例 换算不一致：offset {} / {total} = {ratio_from_offset:.3}，progress = {progress:.3}",
            locator.offset
        );
    }

    /// 「重启回到原处」里属于我们自己的那一半：位置必须能无损穿过 SQLite 回来。
    /// 像素级落点取决于渲染器的 reveal 语义，自研排版引擎（M2）之后由我们自己保证。
    #[gpui_kit::test]
    fn position_survives_a_restart_through_the_store(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = fixture();
        let (view, handle) = open(
            cx,
            &flow,
            Reader {
                font_size: 16.0,
                ..Default::default()
            },
            Locator::start(),
        );

        scroll_down(cx, handle, 900.0);
        let locator = locator_of(cx, &view);
        assert!(locator.offset > 0, "滚动后应能算出字符偏移");

        let store = folio_core::Store::in_memory().expect("内存库");
        let row = book();
        let id = store
            .add_book(
                std::path::Path::new(&row.path),
                &folio_core::DocMeta {
                    title: row.title.clone(),
                    authors: Vec::new(),
                    format: folio_core::Format::Markdown,
                    size: 0,
                },
            )
            .expect("入库");
        store.save_flow(id, &flow).expect("章节入库");
        store
            .set_position(id, "continuous", &locator)
            .expect("写入进度");

        let (mode, restored) = store.position(id).expect("读回进度").expect("进度应存在");
        assert_eq!(mode, "continuous");
        assert_eq!(restored, locator, "进度穿过 SQLite 之后必须逐字符一致");

        // 用读回来的进度重开一本书，恢复请求应当被挂上并在解析完成后落地。
        let (reopened, _) = open(
            cx,
            &flow,
            Reader {
                font_size: 16.0,
                ..Default::default()
            },
            restored,
        );
        assert!(
            !reopened.read_with(cx, |v, _| v.has_pending_restore()),
            "解析完成后恢复请求应被消费掉，而不是永远挂着"
        );
    }

    /// M2 的验收门：中文排版基础三件事必须由**自研引擎**产出，而不是靠 TextView。
    /// 首行缩进、居中标题、按样式给行距 —— 全部从引擎的行盒几何里读出来断言。
    #[gpui_kit::test]
    fn the_engine_indents_centers_and_spaces_lines(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/oss/minimal-zh.epub");
        let (meta, content) = folio_core::open(&path).expect("合成 EPUB 应能打开");
        // Content 现在只有 Reflow 一个分支，这个模式是不可反驳的。
        let folio_core::Content::Reflow(flow) = content;
        assert_eq!(flow.chapters.len(), 3, "spine 三章");
        assert_eq!(meta.title, "合成测试书");

        let prefs = Reader {
            font_size: 20.0,
            line_height: 1.8,
            ..Default::default()
        };
        let (view, _handle) = open_as(cx, &flow, "epub", prefs, Locator::start());
        let lines = cx
            .update(|cx| view.read(cx).engine_lines().map(|l| l.to_vec()))
            .expect("EPUB 走自研引擎");
        let texts = cx
            .update(|cx| view.read(cx).engine_line_texts())
            .expect("EPUB 走自研引擎");
        assert!(!lines.is_empty(), "引擎应产出行盒");

        // 第一章首个行盒是标题：UA 的 h1 = 2em 之外，书自己的 CSS 写了 1.8em 且居中。
        let title = &lines[0];
        assert_eq!(texts[0], "第一章 蛇结");
        assert!(
            (title.dominant_font_size() - 36.0).abs() < 0.5,
            "h1 = 1.8em × 20px，实际 {}",
            title.dominant_font_size()
        );
        assert!(
            title.x > 20.0,
            "居中标题应离开版心左缘，实际 x = {}",
            title.x
        );

        // 第二段起是正文：书自己的 CSS 写了 p { text-indent: 2em }。
        let body = lines
            .iter()
            .find(|l| l.dominant_font_size() == 20.0 && l.x > 0.0)
            .expect("应有一个首行被缩进的正文行");
        assert!(
            (body.x - 40.0).abs() < 1.0,
            "2em 缩进在 20px 字号下应是 40px，实际 {}",
            body.x
        );

        // 行距按各自的字号给：标题一行比正文一行高。
        assert!(
            title.height > body.height * 1.5,
            "行盒高应随字号走，标题 {} vs 正文 {}",
            title.height,
            body.height
        );
        assert!(
            (body.height - 36.0).abs() < 0.5,
            "正文行高 = 20 × 1.8，实际 {}",
            body.height
        );

        // 长段落必须真的折了行：没有任何一行容得下整段原文，才算折过。
        let longest_paragraph = flow.chapters[0]
            .source
            .split('<')
            .map(|s| s.chars().filter(|c| !c.is_whitespace()).count())
            .max()
            .unwrap_or(0);
        let longest_line = texts.iter().map(|t| t.chars().count()).max().unwrap_or(0);
        assert!(
            longest_line < longest_paragraph && longest_line > 4,
            "段落应被折行：最长段 {longest_paragraph} 字，最长行只有 {longest_line} 字"
        );
        for pair in lines.windows(2) {
            assert!(
                pair[1].y >= pair[0].y + pair[0].height - 0.5,
                "行盒不能重叠：{} + {} > {}",
                pair[0].y,
                pair[0].height,
                pair[1].y
            );
        }
    }

    /// M3 的验收：一个段落里的局部字重、斜体与颜色必须各自成立。
    /// 这一条测的是**真实字体栈**上的多 run 度量与摆放，mock 测不到。
    #[gpui_kit::test]
    fn inline_styles_survive_into_runs(cx: &mut TestAppContext) {
        use folio_core::css::computed::Color;

        cx.update(gpui_kit::init);
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/oss/minimal-zh.epub");
        let (_, content) = folio_core::open(&path).expect("合成 EPUB 应能打开");
        let folio_core::Content::Reflow(flow) = content;
        let (view, _handle) = open_as(cx, &flow, "epub", Reader::default(), Locator::start());
        let lines = cx
            .update(|cx| view.read(cx).engine_lines().map(|l| l.to_vec()))
            .expect("EPUB 走自研引擎");

        let rich = lines
            .iter()
            .find(|l| l.text_runs().count() >= 4)
            .expect("带 em/strong/span 的那一行应被切成多个 run");
        assert!(
            rich.text_runs().any(|r| r.style.font.weight >= 700),
            "strong 应带粗体"
        );
        assert!(rich.text_runs().any(|r| r.style.font.italic), "em 应带斜体");
        assert!(
            rich.text_runs()
                .any(|r| r.style.color == Some(Color(0xC0, 0x39, 0x2B))),
            "span.red 的局部颜色要落到 run 上"
        );
        assert!(
            rich.text_runs().all(|r| r.width > 0.0),
            "每个 run 都要有实际绘制宽度"
        );
        let sum: f32 = rich.text_runs().map(|r| r.width).sum();
        assert!(
            (sum - rich.width).abs() < 0.5,
            "行宽应是各 run 宽度之和：{sum} vs {}",
            rich.width
        );
    }

    /// M3-E3 验收（app 侧）：EPUB 章里的内嵌图要经真实引擎变成图片片段，
    /// `src` 按本章目录解析到容器路径，且可见文本里不留哨兵字符。
    #[gpui_kit::test]
    fn images_flow_through_the_engine_as_fragments(cx: &mut TestAppContext) {
        use folio_core::format::ImageRes;

        cx.update(gpui_kit::init);
        let flow = FlowDoc {
            chapters: vec![Chapter {
                title: "图".into(),
                source: "<p>前<img src=\"../Images/pic.png\" alt=\"图\">后</p>".into(),
                spine_href: "OEBPS/Text/chap.xhtml".into(),
            }],
            images: vec![ImageRes {
                href: "OEBPS/Images/pic.png".into(),
                media_type: "image/png".into(),
                width: 40,
                height: 20,
                bytes: PNG_1X1.to_vec(),
            }],
            ..Default::default()
        };
        let (view, _handle) = open_as(cx, &flow, "epub", Reader::default(), Locator::start());
        let lines = cx
            .update(|cx| view.read(cx).engine_lines().map(|l| l.to_vec()))
            .expect("EPUB 走自研引擎");
        let texts = cx
            .update(|cx| view.read(cx).engine_line_texts())
            .expect("EPUB 走自研引擎");

        assert_eq!(texts[0], "前后", "行内图不产出字符，哨兵要被去掉");
        let img = lines[0]
            .images()
            .next()
            .expect("../Images/pic.png 应解析成 OEBPS/Images/pic.png 并命中");
        assert_eq!(img.href, "OEBPS/Images/pic.png");
        assert_eq!((img.width, img.height), (40.0, 20.0));
    }

    #[gpui_kit::test]
    fn the_engine_progress_is_a_character_offset_that_comes_back(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = epub_flow();
        // 字号故意给到 60：这本合成书的第一章很短，默认字号下整章只有几行、比窗口还短，
        // 列表没有可滚的范围（`max_offset` 实测为 0），那样的话这条测试测的是空集。
        let prefs = Reader {
            font_size: 60.0,
            ..Default::default()
        };
        let (view, handle) = open_as(cx, &flow, "epub", prefs.clone(), Locator::start());
        let lines = view.read_with(cx, |v, _| v.engine_lines().map(|l| l.len()).unwrap_or(0));
        assert!(lines > 4, "大字号下第一章该排出多行，实际 {lines}");
        // 跳转目标取「本章中点」。写死一个坐标的话，CI 上字体不同 → 行高不同 →
        // 可能落在第一行里，那条测试就变成测空集了。
        let total = view.read_with(cx, |v, _| v.text_length());
        assert!(total > 40, "第一章该有几十个字可跳，实际 {total}");
        let target = total / 2;
        // 章首那一行的坐标不是 0：pretty-print 过的 XHTML 里 `<body>` 与 `<p>` 之间的
        // 换行本身是文本节点，也进坐标系。这里钉的是「刚打开时还在最前面」，
        // 而不是「等于 0」—— 见 `anchor` 模块头对坐标系的说明。
        let start = locator_of(cx, &view).offset;
        assert!(start < 8, "刚打开该在章首附近，实际坐标 {start}");

        // 引擎那张列表的 `ElementId` 不在测试支持的注册路径里（它注册在
        // `engine-scroll` 之下），所以滚动这件事走产品本来就要有的那个入口：
        // 「跳到章内第 N 个字」。目录跳章、书签、恢复进度后面用的都是它。
        view.update(cx, |v, _| v.jump_to_offset(target));
        settle(cx, handle);

        let saved = locator_of(cx, &view);
        assert!(
            saved.offset > start,
            "跳到中点坐标 {target} 之后章内偏移该往前走，实际 {start} → {}",
            saved.offset
        );
        assert!(
            saved.offset <= target,
            "落点该是覆盖 {target} 那一行的行首，不该越过它：{}",
            saved.offset
        );
        // 也不能越过一整行太多：下一行的行首都比目标早，说明跳过头了。
        assert!(
            target - saved.offset < target / 4,
            "落点离目标 {target} 太远：{}",
            saved.offset
        );
        let progress = progress_of(cx, &view);
        assert!(
            progress > 0.0 && progress < 1.0,
            "滚动比例该在 (0,1) 之间，实际 {progress}"
        );

        // 用同一个坐标重开：落回的行反解出来的偏移必须还是它。
        let (reopened, _handle) = open_as(cx, &flow, "epub", prefs, saved);
        assert!(
            !reopened.read_with(cx, |v, _| v.has_pending_restore()),
            "恢复请求该在排版做完的那一帧被消费掉"
        );
        assert_eq!(
            locator_of(cx, &reopened).offset,
            saved.offset,
            "坐标 → 行顶 → 再反解回坐标，必须回到同一个字"
        );
        assert!(
            progress_of(cx, &reopened) > 0.0,
            "重开之后列表真的滚出去了，不是只记了个数"
        );
    }
}
