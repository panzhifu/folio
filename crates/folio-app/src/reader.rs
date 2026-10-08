use folio_core::store::BookRow;
use folio_core::{Chapter, FlowDoc, Locator, Prefs};
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    Pixels, Render, ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Window, div,
    point, px,
};

pub struct ReaderView {
    book_id: i64,
    title: SharedString,
    chapters: Vec<Chapter>,
    chapter: usize,
    state: Entity<TextViewState>,
    scroll: ScrollHandle,
    prefs: Prefs,
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
        let state = cx.new(|cx| TextViewState::markdown(source, cx));
        let mut this = Self {
            book_id: book.id,
            title: book.title.clone().into(),
            chapters,
            chapter,
            state,
            scroll: ScrollHandle::new(),
            prefs,
        };
        this.restore(locator, cx);
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
    pub fn progress(&self) -> f32 {
        let max = self.scroll.max_offset().y;
        let now = self.scroll.offset().y;
        if max <= px(0.0) {
            0.0
        } else {
            (now / max).clamp(0.0, 1.0)
        }
    }

    /// 把当前滚动位置折算成「第几章第几个字符」。
    /// `RenderedText` 只有 source→rendered 单向映射，所以这里对源文本二分反解。
    pub fn locator(&self, cx: &App) -> Locator {
        let rendered = self.state.read(cx).rendered_text();
        let source = rendered.source();
        let total = rendered.len();
        let target = (self.progress() * total as f32).floor() as usize;

        let (mut lo, mut hi) = (0usize, source.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match rendered.range_for_source(mid..mid + 1) {
                Some(range) if range.start >= target => hi = mid,
                _ => lo = mid + 1,
            }
        }
        Locator::from_byte(self.chapter as u32, source, lo)
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
        self.state.update(cx, |state, cx| state.set_text(&source, cx));
        self.scroll.set_offset(point(px(0.0), px(0.0)));
        cx.notify();
    }

    /// 源偏移 → 渲染区间 → 让 TextView 自己滚过去。
    /// 渲染层需要一次布局才有区间，所以这里失败时静默跳过（首次打开时由 on_reveal 兜底）。
    fn restore(&mut self, locator: Locator, cx: &mut Context<Self>) {
        let rendered = self.state.read(cx).rendered_text();
        let byte = locator.byte_of(rendered.source());
        let Some(range) = rendered.range_for_source(byte..byte + 1) else {
            return;
        };
        let _ = self
            .state
            .update(cx, |state, cx| state.reveal_range(range, cx));
    }
}

impl Render for ReaderView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let prefs = self.prefs.clone();
        let scroll = self.scroll.clone();

        div()
            .id("reader-scroll")
            .size_full()
            .flex_1()
            .min_h_0()
            .px(px(prefs.margin_x))
            .track_scroll(&scroll)
            .overflow_y_scroll()
            .vertical_scrollbar(&scroll)
            .child(
                div()
                    .mx_auto()
                    .w_full()
                    .max_w(px(prefs.max_measure))
                    .child(
                        TextView::new(&self.state)
                            .selectable(true)
                            .text_size(px(prefs.font_size))
                            .line_height(px(prefs.font_size * prefs.line_height))
                            .font_family(prefs.font_family.clone())
                            .text_color(_cx.theme().foreground)
                            .on_reveal(move |bounds: Bounds<Pixels>, _window, _cx| {
                                let handle = scroll.clone();
                                let offset = handle.offset().y
                                    + (bounds.origin.y - handle.bounds().origin.y);
                                handle.set_offset(point(px(0.0), offset));
                            }),
                    ),
            )
    }
}
