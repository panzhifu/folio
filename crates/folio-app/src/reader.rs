use folio_core::store::BookRow;
use folio_core::{Chapter, FlowDoc, Locator, Prefs};
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::{
    App, AppContext as _, Context, Entity, InteractiveElement, IntoElement, ParentElement, Pixels,
    Render, SharedString, Styled, Subscription, TestSupportExt, Window, div, point, px,
};

pub struct ReaderView {
    book_id: i64,
    title: SharedString,
    chapters: Vec<Chapter>,
    chapter: usize,
    state: Entity<TextViewState>,
    prefs: Prefs,
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
        prefs: Prefs,
        locator: Locator,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let chapters = flow.chapters;
        let chapter = (locator.chapter as usize).min(chapters.len().saturating_sub(1));
        let source = chapters
            .get(chapter)
            .map(|c| c.source.as_str())
            .unwrap_or_default();
        let state = cx.new(|cx| TextViewState::markdown(source, cx).scrollable(true));
        let observe_state = cx.observe(&state, |this, _, cx| {
            if this.apply_pending_restore(cx) {
                cx.notify();
            }
        });
        let this = Self {
            book_id: book.id,
            title: book.title.clone().into(),
            chapters,
            chapter,
            state,
            prefs,
            pending_restore: (locator.offset > 0).then_some(locator),
            _observe_state: observe_state,
        };
        this
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
    pub fn chapter_position(&self) -> String {
        if self.chapters.is_empty() {
            return String::new();
        }
        format!("第 {} / {} 节", self.chapter + 1, self.chapters.len())
    }

    /// 已滚过的比例。用它而不是页码来换算位置：字号与窗口宽度变化后
    /// 页码失效，但同一章内的相对位置基本守恒。
    ///
    /// 滚动条由 TextView 内部的 list 持有（`scrollable(true)`），所以进度从它的
    /// `list_state()` 读。外面再套一层滚动容器是错的：内层 list 自己吃掉视口高度，
    /// 外层量不到内容高度，`max_offset` 恒为 0。
    pub fn progress(&self, cx: &App) -> f32 {
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

    pub fn apply_prefs(&mut self, prefs: Prefs, cx: &mut Context<Self>) {
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
        // 换章即作废未落地的恢复请求，否则会把上一章的位置套到本章上。
        self.pending_restore = None;
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 解析未完时这里返回 false，留到下一次渲染再落地；落地后 reveal_range
        // 自己 notify 一帧，因为 pending_restore 已被取走，不会循环。
        self.apply_pending_restore(cx);
        let prefs = self.prefs.clone();
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
                div()
                    .mx_auto()
                    .w_full()
                    .h_full()
                    .max_w(px(prefs.max_measure))
                    .child(
                        TextView::new(&self.state)
                            .scrollable(true)
                            .selectable(true)
                            .text_size(px(prefs.font_size))
                            .line_height(px(prefs.font_size * prefs.line_height))
                            .font_family(prefs.font_family.clone())
                            .text_color(cx.theme().foreground),
                    ),
            )
    }
}
