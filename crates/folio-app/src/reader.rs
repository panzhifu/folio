use std::sync::Arc;

use folio_core::store::BookRow;
use folio_core::{Chapter, FlowDoc, Locator, Reader};
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::{
    App, AppContext as _, Context, Entity, InteractiveElement, IntoElement, ParentElement, Pixels,
    Render, SharedString, Styled, Subscription, TestSupportExt, Window, div, point, px,
};

use crate::engine::{Engine, engine_lines};
use crate::images::{BookImages, ChapterImages};

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
    engine: Option<Engine>,
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
    _observe_state: Subscription,
}

impl ReaderView {
    pub fn open(
        book: &BookRow,
        flow: FlowDoc,
        prefs: Reader,
        locator: Locator,
        _window: &mut Window,
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
            Engine::new(source.clone(), &flow.css, prefs.clone(), images)
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
            _observe_state: observe_state,
        }
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
        crate::l10n::tf(
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
            // 引擎路径的章内进度要读虚拟列表的滚动偏移；那属于 M5 的锚点接线
            // （位置从「像素偏移」改成「行盒 + 节点内偏移」）。这里不猜一个数。
            return 0.0;
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
        if self.is_engine() {
            // 章级位置仍然准确（chapter 索引照存），只是章内偏移回到章首。
            return Locator {
                chapter: self.chapter as u32,
                offset: 0,
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
}

impl Render for ReaderView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(engine) = self.engine.as_mut() {
            engine.ensure_layout(window, cx, self.pane_reserve);
            let width = px(Engine::content_width(
                window,
                &self.prefs,
                self.pane_reserve,
            ));
            let sizes = engine.item_sizes();
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
            Some(engine) => crate::engine::render_range(engine, range, cx),
            None => Vec::new(),
        }
    }
}
