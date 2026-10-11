//! 阅读视图：把会话的状态摆成元素——逐行画字形、把图片绝对定位、接住滚轮。
//!
//! 这里是**纯渲染**：排版不在这里（在 [`crate::ui::features::reader::document`]），
//! 状态与命令不在这里（在 [`crate::ui::features::reader::session`]）。`render` 只按当前
//! 模式挑出要摆的那几「片」，把排版行摊成 [`Scene`]，再挂上滚轮监听。
//!
//! 三种模式只差 [`Viewport`] 的那三个数，切片由核心分片器给出：
//!
//! - **连续滚动**：锚章 ± 窗口拼成一条全局坐标（[`super::book::Book`]），位置是
//!   `scroll_y` 这个**全局**像素数；滚过章尾自然接下一章，可见带逐章切场景。
//!   手机上默认这一档。
//! - **分页**：一片 = 一个视口高、一页一栏，位置是页号。
//! - **双页对开**：一片 = 一栏、一页两栏，位置还是页号（一号就是「一张纸」）。
//!
//! 宽度装不下两栏时双页自动退回分页（判据在 [`chrome::PageMode::effective`]，按宽度而不是
//! 按 target），所以横屏的平板真能双页，竖屏手机不会长出两条窄柱子。
//!
//! 滚动**不自己做惯量**：`gpui-mobile` 的平台层已经把触摸翻译成了 `ScrollWheel` 事件
//! （`gpui-mobile/src/android/window.rs:1316` 起，用 `momentum::VelocityTracker` 估速度、
//! 用 `MomentumScroller` 在松手后每帧派发一次衰减增量），视图只要把事件里的 dy 累加到
//! `scroll_y`。在这里再套一个 `MomentumScroller` 会把同一份惯量算两遍，滚起来直接翻倍。

use std::cell::RefCell;
use std::ops::Range;

use folio_core::anchor::line_at_y;
use folio_core::layout::FontKey;
use folio_core::layout::fragment::column_offset;
use folio_core::store::Annotation;
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    AnyElement, App, Bounds, Context, Hsla, InteractiveElement as _, IntoElement, ParentElement,
    Pixels, Render, ScrollWheelEvent, SharedString, Styled, TestSupportExt as _, Window, div, img,
    point, px, size,
};

use super::book::Book;
use super::book_fonts::BookFonts;
use super::chrome;
use super::document::{Document, Viewport};
use super::modules::annotations::{self, Palette};
use super::paint::{GlyphRun, GlyphsElement, SelectionVisual};
use super::scene::{BlockDecor, Scene, VisibleImage};
use super::selection_menu;
use super::session::{AnnotationSpans, ReaderSession, Source};
use crate::ui::shell::navigation::SizeClass;

impl Render for ReaderSession {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_prefs(cx);
        let viewport_size = window.viewport_size();
        // 有效视口 = 窗口高 − 外壳占的高 − 页眉 − 页脚。可见行筛选、滚动上限、
        // `on_scroll` 的夹范围全都从这里取，所以底部那一截内容仍然滚得进来，
        // 也不会滚出多余空白。浮层（顶栏/底栏/面板）压在正文**之上**，不参与这个数：
        // 点一下屏幕字不该跳走。
        let viewport_height = (f32::from(viewport_size.height)
            - self.reserve.height
            - self.header_reserve
            - self.footer_reserve)
            .max(1.0);
        let gap = chrome::spread_gap(self.prefs.margin_x);
        // 退档判据与排版必须量同一个版心宽，所以用 `Document` 的那个式子算，不在这里另定一套。
        let usable = Document::content_width(window, &self.prefs, self.reserve.width);
        if self.mode_undecided && !SizeClass::of(window).is_compact() {
            // 读者没选过档，且这是平板 / 桌面的窗口宽度档：每一帧按当前宽重算默认档。
            // 手机档（Compact）不参与 —— 那里连续滚动是习惯本身，两条窄柱子不是体验。
            // 每帧重算而不是定一次：窗口从 1200 收到 700，双页就该退回连续滚动，
            // 不该把「第一次打开时是横屏平板」那一刻钉死。
            self.mode = chrome::default_mode(usable, gap);
        }
        let mode = self.mode.effective(usable, gap);
        let columns = mode.columns();

        let foreground = cx.theme().foreground;
        // 批注底色只有那四种（`ANNOTATION_COLORS`），一次取好，逐块画时查表。
        let palette = Palette::of(cx.theme());
        let family = self.family.clone();
        // 两章之间的缝：拼接坐标用它隔开相邻的章（口径在 `Book::chapter_gap`）。
        let chapter_gap = Book::chapter_gap(&self.prefs);
        // 等宽偏好整帧取一次：兜底那一行与正文的 run 都要它。
        let mono = SharedString::from(self.prefs.mono_font_family.clone());
        // `FontKey → Font` 的解析每帧对每条可见 run 都要一次，交给会话持有的缓存
        // （避免逐 run 扫设备字体表）。body / mono / faces / 设备表变了由会话清或重建。
        let font_cache = &mut self.font_cache;
        // 页面底色放在 `column` 自己身上（父背景先于所有子节点画）：字形元素铺满
        // 整个版心，若把不透明背景挂在它身上、它又在图片之后才入树，白底会把先画的
        // 图片整个盖住 —— 封面章整页空白的根因（gpui 的 z-order 是「相交即 +1，
        // 后画者居上」，见 plans/facts.md）。层级从此固定为：页底 → 块级装饰 →
        // 字形 → 图片。
        let mut element = GlyphsElement::new();
        let mut column = div().size_full().relative().bg(cx.theme().background);
        // 图片永远压在字形之上：收集起来，等 `element` 入树之后再挂。
        let mut overlays: Vec<AnyElement> = Vec::new();
        // `&mut self.source` 只借用这一个字段，所以臂里写 `self.content_height`、
        // 读 `self.scroll_y` 都不冲突（前提是别把 `self` 整个传进函数）。
        match &mut self.source {
            Source::Sample(text) => {
                let font = font_cache.resolve(
                    &FontKey {
                        family: String::new(),
                        families: Vec::new(),
                        weight: 400,
                        italic: false,
                    },
                    &family,
                    &mono,
                    // 兜底那一行没有书，也就没有内嵌字体。空表不分配，等于「没有这个功能」。
                    &BookFonts::empty(),
                );
                // 单行：左边距 `margin_x`，基线给到页眉下面 `margin_x + font_size` 处。
                element.push(
                    GlyphRun::new(text.clone(), font, self.prefs.font_size)
                        .at(
                            self.prefs.margin_x,
                            self.header_reserve + self.prefs.margin_x + self.prefs.font_size,
                        )
                        .color(foreground),
                );
                self.content_height = 0.0;
                self.laid_out.viewport = viewport_height;
            }
            Source::Book(book) => {
                if mode.is_continuous() {
                    // —— 连续滚动：锚章 ± 窗口拼成一条全局坐标 ——
                    // 1. 邻章补齐入库（0 高占位）：排版作业这就能排进后台队列，
                    //    狂飙滚动时接缝提前两章就铺好了。
                    book.ensure_window_spans(&self.prefs);
                    // 2. 按当前（上一帧的）坐标链记下视口顶：排版收完结果好补偿。
                    let snapshot = book.spine_snapshot(self.scroll_y, chapter_gap);
                    // 3. 排版：锚章先提交（恢复落点等它），邻居随后。连续模式只排版
                    //    不切片（一片高 0 是「不分片」的哨兵值）。
                    for doc in book.docs_mut() {
                        doc.ensure_viewport(
                            window,
                            cx,
                            Viewport {
                                reserve: self.reserve.width,
                                columns: 1,
                                height: 0.0,
                                gap,
                            },
                        );
                    }
                    // 4. 高度变了就补偿：视口顶那一行字在屏幕上纹丝不动。
                    book.compensate_spine(snapshot, &mut self.scroll_y, chapter_gap);
                    self.laid_out = super::session::LaidOut {
                        mode,
                        columns,
                        pages: 1,
                        viewport: viewport_height,
                    };
                    // 5. 恢复落点：全局 y = 章基线 + 章内行顶。请求可能指向邻章
                    //    （跨章进度、换模式、跳章），先把锚章挪过去。
                    if let Some(target) =
                        self.pending_restore.map(|locator| locator.chapter as usize)
                        && target != book.chapter_index()
                        && target < book.chapter_count()
                    {
                        book.set_anchor(target, &self.prefs);
                    }
                    if self.pending_restore.is_some() && book.document().is_current() {
                        let locator = self.pending_restore.take().expect("上面刚判过 Some");
                        self.scroll_y = book
                            .global_y_of_offset(
                                locator.chapter as usize,
                                locator.offset,
                                chapter_gap,
                            )
                            .unwrap_or_else(|| book.document().y_at_offset(locator.offset));
                    }
                    // 6. 锚章迁移：视口顶跨进了邻章（锚章自己排完才有资格答）。
                    book.migrate_spine(&mut self.scroll_y, chapter_gap, &self.prefs);
                    // 7. 总高与夹取：全局坐标里「滚到底」是整条链的末端。
                    self.content_height = book.content_height(chapter_gap);
                    let max_scroll = (self.content_height - viewport_height).max(0.0);
                    self.scroll_y = self.scroll_y.clamp(0.0, max_scroll);
                    // 8. 可见带 → 逐章切场景：每章用自己的排版与图片索引，
                    //    换算只差一个章基线。没排完的章不进链，自然一帧都不画。
                    let band = self.scroll_y..(self.scroll_y + viewport_height);
                    for placed in book.placements(chapter_gap) {
                        if placed.base_y + placed.height < band.start || placed.base_y > band.end {
                            continue;
                        }
                        let Some(doc) = book.span_doc(placed.index) else {
                            continue;
                        };
                        let scene = Scene::build(
                            doc,
                            font_cache,
                            &family,
                            0..doc.line_count(),
                            (band.start - placed.base_y)..(band.end - placed.base_y),
                            band.start - placed.base_y,
                            self.prefs.margin_x,
                            self.header_reserve,
                            foreground,
                        );
                        element.extend(scene.runs);
                        for decor in scene.decorations {
                            column.extend(decoration_child(decor));
                        }
                        overlays.extend(scene.images.into_iter().map(image_child));
                        annotation_fills(
                            &mut element,
                            doc,
                            window,
                            cx,
                            &self.annotation_spans,
                            &self.annotations,
                            &palette,
                            placed.index,
                            self.prefs.margin_x,
                            band.start - placed.base_y,
                            self.header_reserve,
                            (band.start - placed.base_y)..(band.end - placed.base_y),
                        );
                    }
                } else {
                    // —— 分页 / 双页：只用锚章那份排版，一页取几片、每片一栏 ——
                    let column_width = book.document_mut().ensure_viewport(
                        window,
                        cx,
                        Viewport {
                            reserve: self.reserve.width,
                            columns,
                            height: viewport_height,
                            gap,
                        },
                    );
                    let pages = book.document().page_count();
                    self.laid_out = super::session::LaidOut {
                        mode,
                        columns,
                        pages,
                        viewport: viewport_height,
                    };

                    // 上次读到第几个字 → 该摆在哪一页。恢复请求可能指向邻章
                    // （跨章进度）：先把锚章挪过去，等它的排版做完那一帧再落地。
                    if let Some(target) =
                        self.pending_restore.map(|locator| locator.chapter as usize)
                        && target != book.chapter_index()
                        && target < book.chapter_count()
                    {
                        book.set_anchor(target, &self.prefs);
                    }
                    if self.pending_restore.is_some() && book.document().is_current() {
                        let locator = self.pending_restore.take().expect("上面刚判过 Some");
                        let line = book.document().line_at_offset(locator.offset);
                        self.page = book
                            .document()
                            .page_of_line(line)
                            .min(pages.saturating_sub(1));
                    }
                    self.page = self.page.min(pages.saturating_sub(1));

                    // 分页 / 双页：一页取几片、每片一栏，摆在各自的栏位上。
                    let slices = book.document().slices();
                    for index in 0..columns {
                        let Some(slice) = slices.get(self.page * columns + index) else {
                            continue;
                        };
                        if slice.empty {
                            continue;
                        }
                        let x =
                            self.prefs.margin_x + column_offset(column_width, gap, slice.column);
                        let scene = Scene::build(
                            book.document(),
                            font_cache,
                            &family,
                            slice.lines.clone(),
                            slice.origin_y..(slice.origin_y + slice.height + 1.0),
                            slice.origin_y,
                            x,
                            self.header_reserve,
                            foreground,
                        );
                        element.extend(scene.runs);
                        for decor in scene.decorations {
                            column.extend(decoration_child(decor));
                        }
                        overlays = scene.images.into_iter().map(image_child).collect();
                        annotation_fills(
                            &mut element,
                            book.document(),
                            window,
                            cx,
                            &self.annotation_spans,
                            &self.annotations,
                            &palette,
                            book.chapter_index(),
                            x,
                            slice.origin_y,
                            self.header_reserve,
                            slice.origin_y..(slice.origin_y + slice.height + 1.0),
                        );
                    }
                }
            }
        }
        // 选区挂在它自己的章上：那一章被裁出拼接窗口（滚远了）就一并丢 ——
        // 高亮、把手、复制都再没有数据源了。
        if let Some(selected) = &self.selection
            && self
                .book()
                .is_none_or(|book| book.span_doc(selected.chapter).is_none())
        {
            self.selection = None;
        }
        // 选中件（M6）：把「选区 ∩ 可见范围」换成元素局部矩形喂给字形元素
        // （高亮压在字下、把手压在字上、手势监听随 paint 注册），两端坐标留给菜单条。
        // 排版还没跟上（换章/改字号在飞）的那一帧不画：行盒是旧的，矩形会指错字。
        let mut visual = SelectionVisual {
            color: cx.theme().selection,
            ..Default::default()
        };
        if let (Source::Book(book), Some(selected)) = (&self.source, self.selection.clone()) {
            if let Some(doc) = book.span_doc(selected.chapter)
                && doc.is_current()
            {
                if mode.is_continuous() {
                    // 章基线把视口顶换算成章内坐标：可见带 → 覆盖它的字符区间；
                    // 选区与它求交，矩形只扫可见的那几行。
                    let base = book.base_of(selected.chapter, chapter_gap).unwrap_or(0.0);
                    let layout = doc.layout();
                    let total = doc.text_map().chars();
                    let band_top = self.scroll_y - base;
                    let band_bottom = self.scroll_y + viewport_height - base;
                    let first = line_at_y(layout, band_top);
                    let last = line_at_y(layout, band_bottom);
                    let sub = selected.range.start.max(doc.coordinate_of_line(first))
                        ..selected.range.end.min(if last + 1 < layout.lines.len() {
                            doc.coordinate_of_line(last + 1)
                        } else {
                            total
                        });
                    highlight_rects(
                        doc,
                        window,
                        cx,
                        sub,
                        self.prefs.margin_x,
                        band_top,
                        self.header_reserve,
                        band_top..band_bottom,
                        &mut visual.highlights,
                    );
                } else {
                    // 分页 / 双页：翻走即清，选区必在锚章 —— 逐栏求交照搬。
                    let layout = doc.layout();
                    let total = doc.text_map().chars();
                    let width =
                        folio_core::layout::fragment::column_width(usable, columns, gap).max(1.0);
                    for index in 0..columns {
                        let Some(slice) = doc.slice_of(self.page, index, columns) else {
                            continue;
                        };
                        if slice.empty || slice.lines.is_empty() {
                            continue;
                        }
                        let first_box = &layout.lines[slice.lines.start];
                        let last_box = &layout.lines[slice.lines.end - 1];
                        let sub = selected
                            .range
                            .start
                            .max(doc.coordinate_of_line(slice.lines.start))
                            ..selected
                                .range
                                .end
                                .min(if slice.lines.end < layout.lines.len() {
                                    doc.coordinate_of_line(slice.lines.end)
                                } else {
                                    total
                                });
                        highlight_rects(
                            doc,
                            window,
                            cx,
                            sub,
                            self.prefs.margin_x + column_offset(width, gap, slice.column),
                            slice.origin_y,
                            self.header_reserve,
                            first_box.y..(last_box.y + last_box.height),
                            &mut visual.highlights,
                        );
                    }
                }
            }
            // 把手长在首末高亮的字缘上（0 宽、一行高）；两端都滚出可视范围就没有把手，
            // 与 gpui-kit「端点跟着它标记的字消失」同一套规矩。
            if let Some(first) = visual.highlights.first().copied() {
                visual.start_caret =
                    Some(Bounds::new(first.origin, size(px(0.0), first.size.height)));
            }
            if let Some(last) = visual.highlights.last().copied() {
                visual.end_caret = Some(Bounds::new(
                    point(last.right(), last.top()),
                    size(px(0.0), last.size.height),
                ));
            }
        }
        // 菜单条锚在首行高亮之上，放不下就掉到末行高亮之下。
        let menu_anchor = (
            visual.highlights.first().map(|rect| f32::from(rect.top())),
            visual
                .highlights
                .last()
                .map(|rect| f32::from(rect.bottom())),
        );
        element = element.selection(visual);
        if matches!(self.source, Source::Book(_)) {
            element = element.reader(Some(cx.entity().downgrade()));
        }
        column = column.child(element);
        // 图片最后入树：绝对定位不参与布局，却要压住上面所有层级。
        for image in overlays {
            column = column.child(image);
        }
        // 选中菜单（M6）：摆位是 `selection_menu::slot` 那条纯判断，元素本体在
        // `selection_menu.rs` 里。浮层点不住的教训见 page.rs 模块注释，所以它是压在正文
        // 之上的真按钮；拖动期间收起，松手即回。
        let slot = if self.selection_menu_open() {
            selection_menu::slot(
                menu_anchor.0,
                menu_anchor.1,
                f32::from(viewport_size.height),
            )
        } else {
            selection_menu::Slot::Hidden
        };
        // 没有菜单上屏时必须把上一帧的锚位清掉：`tap_with_selection` 读的就是那份
        // bounds，留着的话「点一下菜单原来那块空地」还算点在菜单上。
        match self.selection_menu(cx, &slot) {
            Some(menu) => column = column.child(menu),
            None => self.menu_bounds.set(None),
        }

        let listener = cx.listener(|this: &mut Self, event: &ScrollWheelEvent, _window, cx| {
            this.on_scroll(event);
            cx.notify();
        });
        div()
            .id("folio-reader")
            // 宿主的测试要按 id 把滚轮事件投到这里。非 `test-support` 构建里这一层是透明的
            // （`Observed` 原样转交内部元素），所以真机上不多花一个节点。
            .test_support()
            .size_full()
            .overflow_hidden()
            .on_scroll_wheel(listener)
            .child(column)
    }
}

/// 一片里的一张图：绝对摆位交给 gpui 的 `img`。
fn image_child(image: VisibleImage) -> AnyElement {
    div()
        .absolute()
        .left(px(image.x))
        .top(px(image.top))
        .w(px(image.width))
        .h(px(image.height))
        .child(img(image.image).size_full())
        .into_any_element()
}

/// 把「选区 ∩ 一个可见带」换成元素局部的矩形，追加进 `out`。
///
/// `x` / `origin_y` 是这一栏（片）的摆位，与 `Scene::build` 同一套换算；
/// `band` 是排版坐标里的可见 y 区间——夹进 `sub` 的字符区间在带与带的接缝处
/// 可能越进下一带（`coordinate_of_line` 给的是下一带首字符），矩形再按 y 滤一遍。
#[allow(clippy::too_many_arguments)]
fn highlight_rects(
    doc: &Document,
    window: &Window,
    cx: &App,
    sub: std::ops::Range<usize>,
    x: f32,
    origin_y: f32,
    header: f32,
    band: std::ops::Range<f32>,
    out: &mut Vec<Bounds<Pixels>>,
) {
    if sub.start >= sub.end {
        return;
    }
    for rect in doc.selection_rects(window, cx, sub) {
        if rect.y + rect.height <= band.start || rect.y >= band.end {
            continue;
        }
        out.push(Bounds::new(
            point(px(x + rect.x), px(rect.y - origin_y + header)),
            size(px(rect.width), px(rect.height)),
        ));
    }
}

/// 一章的批注底色：库里的锚点 → 本章坐标 → 元素局部矩形，追加进字形元素。
///
/// 换算口径与 [`highlight_rects`] 完全一致（同一套「这一栏摆在哪、可见带是哪一段」），
/// 只是来源从这一次挑中的选区换成了留在书上的批注。解不回来的那一条**不画** ——
/// 宁可少一块底色，也不把读者划过的痕迹画到别的字上去（见 `modules/annotations.rs`）。
///
/// 缓存与那一份库里的批注按字段分开递进来而不是拿 `&self`：调用点在 `match &mut self.source`
/// 的那一臂里，整个 `self` 已经借不出了。
#[allow(clippy::too_many_arguments)]
fn annotation_fills(
    element: &mut GlyphsElement,
    doc: &Document,
    window: &Window,
    cx: &App,
    cache: &RefCell<AnnotationSpans>,
    stored: &[Annotation],
    palette: &Palette,
    chapter: usize,
    x: f32,
    origin_y: f32,
    header: f32,
    band: Range<f32>,
) {
    if stored.is_empty() || band.start >= band.end {
        return;
    }
    let spans = annotations::resolved_for(cache, stored, chapter, doc.generation(), doc.text_map());
    for span in spans.iter() {
        if span.is_stale() {
            continue;
        }
        let mut rects = Vec::new();
        highlight_rects(
            doc,
            window,
            cx,
            span.start..span.end,
            x,
            origin_y,
            header,
            band.clone(),
            &mut rects,
        );
        let color = palette.color(&span.color);
        for rect in rects {
            element.fill(rect, color);
        }
    }
}

/// 一块实心矩形（背景或一条边框）。
fn fill_quad(x: f32, top: f32, width: f32, height: f32, color: Hsla) -> AnyElement {
    div()
        .absolute()
        .left(px(x))
        .top(px(top))
        .w(px(width))
        .h(px(height))
        .bg(color)
        .into_any_element()
}

/// 一块盒子的块级装饰：背景一整块 + 四边边框各一条实心条。
/// 几何与 [`image_child`] 同一坐标系；边条在角上互相压边，与 CSS 的 45° 拼角
/// 差一个斜切，肉眼分不出来。
fn decoration_child(decor: BlockDecor) -> Vec<AnyElement> {
    let mut parts = Vec::new();
    if let Some(background) = decor.background {
        parts.push(fill_quad(
            decor.x,
            decor.top,
            decor.width,
            decor.height,
            background,
        ));
    }
    let [top, right, bottom, left] = decor.border;
    if top.0 > 0.0 {
        parts.push(fill_quad(decor.x, decor.top, decor.width, top.0, top.1));
    }
    if bottom.0 > 0.0 {
        parts.push(fill_quad(
            decor.x,
            decor.top + decor.height - bottom.0,
            decor.width,
            bottom.0,
            bottom.1,
        ));
    }
    if left.0 > 0.0 {
        parts.push(fill_quad(decor.x, decor.top, left.0, decor.height, left.1));
    }
    if right.0 > 0.0 {
        parts.push(fill_quad(
            decor.x + decor.width - right.0,
            decor.top,
            right.0,
            decor.height,
            right.1,
        ));
    }
    parts
}

#[cfg(test)]
mod tests {
    //! M1–M4 的验收门，现在挂在**统一的那一份**视图/会话上。
    //!
    //! 这批用例原来在 `desktop.rs`（虚拟列表那一套）里，`mobile.rs` 那边一条都没有 —— 于是
    //! 「手机上正文渲染成立」这件事靠的是真机截图，而桌面上那条回路红过也没人知道。
    //! 两端一份实现之后，这些断言测的就是真机上跑的那条代码路径。
    //!
    //! 用生产类型 `ReaderSession`，不在测试里另搭近似视图；滚动走真实滚轮事件，
    //! 因为直接写 `scroll_y` 会绕过要测的那段行为。
    //! 导入点名而不 `use super::*`：`gpui_kit` 根把 `gpui` 整个转了出来，里面有一个
    //! 叫 `test` 的属性宏，glob 会让下面的 `#[test]` 递归展开成它。

    use std::sync::Arc;

    use folio_core::document::Document;
    use folio_core::store::Store;
    use folio_core::{Chapter, Config, Content, DocMeta, FlowDoc, Format, Locator, Reader};
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Entity, InputEvent as _, LongPressEvent, MouseButton,
        MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, PlatformInput, Point, ScrollDelta,
        TestAppContext, TouchPhase, point, px, size,
    };

    use crate::ui::features::reader::chrome::PageMode;
    use crate::ui::features::reader::session::ReaderSession;
    use crate::ui::shared::config_model::ConfigHolder;

    /// 一段足够长的中文，保证它会折行 —— 不折行的话「缩进改变了布局」这种断言无从成立。
    const PARA: &str = "于是我们奋力向前，逆水行舟，被不断地向后推，推入过去。";

    fn flow() -> FlowDoc {
        // `Chapter::source` 的契约是**引擎就绪的 XHTML**（纯文本的包装在 provider 里做完）。
        // 这里直接进排版层，所以自己按契约包好 —— 塞纯文本进来是测试自己违约，不是引擎该兜的。
        let source = (0..80)
            .map(|i| format!("<p>{PARA}（第 {i} 段）</p>"))
            .collect::<String>();
        FlowDoc {
            chapters: vec![Chapter::inline("第一章", source)],
            ..Default::default()
        }
    }

    /// 多章语料：每章 30 段（16px 下一章约三个视口高，章内可滚区间远大于
    /// 测试的滚动步长，跨章之后认得出读到的是哪一章、进度也还在章首附近）。
    fn chapters_flow(count: usize) -> FlowDoc {
        FlowDoc {
            chapters: (0..count)
                .map(|i| {
                    let source = (0..30)
                        .map(|k| format!("<p>{PARA}（第{i}章 第{k}段）</p>"))
                        .collect::<String>();
                    Chapter::inline(&format!("第{i}章"), source)
                })
                .collect(),
            ..Default::default()
        }
    }

    fn meta(format: &str) -> DocMeta {
        DocMeta {
            title: "测试书".into(),
            authors: Vec::new(),
            format: Format::parse(format).expect("格式名要认得"),
            size: 0,
        }
    }

    fn prefs(size: f32) -> Reader {
        Reader {
            font_size: size,
            ..Reader::default()
        }
    }

    /// 开一扇窗、装好配置、把会话摆上去。`width` 决定这一帧能不能长出双页 ——
    /// 退档判据按宽度而不是按 target，所以窗口尺寸就是测试里唯一要控制的变量。
    fn open(
        cx: &mut TestAppContext,
        flow: &FlowDoc,
        format: &str,
        prefs: Reader,
        locator: Locator,
        width: f32,
    ) -> (Entity<ReaderSession>, AnyWindowHandle) {
        open_with_store(cx, flow, format, prefs, locator, width, None, None)
    }

    #[allow(clippy::too_many_arguments)]
    fn open_with_store(
        cx: &mut TestAppContext,
        flow: &FlowDoc,
        format: &str,
        prefs: Reader,
        locator: Locator,
        width: f32,
        store: Option<Arc<Store>>,
        book_id: Option<i64>,
    ) -> (Entity<ReaderSession>, AnyWindowHandle) {
        let meta = meta(format);
        // 不开文件地造一本书：`from_content` 把章节收进统一文档接口，没有容器
        // （图片与内嵌字体都不来，与「容器丢了」同一个表现）。
        let doc: Arc<dyn Document> = Arc::from(folio_core::document::from_content(
            meta,
            Content::Reflow(flow.clone()),
        ));
        open_with_document(cx, doc, prefs, locator, width, store, book_id)
    }

    /// 直接拿一份已经造好的文档开一屏。给「容器在、正文不在手上」那类测试用 ——
    /// 那是正文出库之后生产上真正走的那条路。
    #[allow(clippy::too_many_arguments)]
    fn open_with_document(
        cx: &mut TestAppContext,
        doc: Arc<dyn Document>,
        prefs: Reader,
        locator: Locator,
        width: f32,
        store: Option<Arc<Store>>,
        book_id: Option<i64>,
    ) -> (Entity<ReaderSession>, AnyWindowHandle) {
        let mut view = None;
        let handle = cx.open_window(size(px(width), px(520.)), |window, cx| {
            // 偏好必须**装进配置**而不是只交给构造函数：会话每帧从 `ConfigHolder`
            // 同步一次偏好（读者在设置页改字号就是这条路），装默认配置就是把这里传的
            // `prefs` 当场冲掉 —— 而画面照样出得来，只有几何不对，最难查的那种红。
            let config = Config {
                reader: prefs.clone(),
                ..Default::default()
            };
            ConfigHolder::install(config, cx);
            let built = ReaderSession::with_document(doc, prefs, store, book_id, locator, cx);
            let created = cx.new(|_| built);
            view = Some(created.clone());
            Root::new(created, window, cx)
        });
        let view = view.expect("ReaderSession 应该被创建");
        settle(cx, handle.into());
        (view, handle.into())
    }

    /// 两轮「跑空事件 → 渲染一帧」。一轮不够：首帧只是把恢复请求挂上，
    /// 而 `run_until_parked` 只跑事件不画帧。框架自己的 `settle()` 也是这个形状。
    fn settle(cx: &mut TestAppContext, handle: AnyWindowHandle) {
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("渲染应成功");
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("第二帧应成功");
        cx.run_until_parked();
    }

    /// 真实滚轮事件投进正文。gpui 的约定是「向下滚为负」，所以往下滚给负的 dy。
    fn scroll_down(cx: &mut TestAppContext, handle: AnyWindowHandle, pixels: f32) {
        cx.update_window(handle, |_, window, cx| {
            window.scroll(
                "folio-reader",
                ScrollDelta::Pixels(point(px(0.0), px(-pixels))),
                cx,
            );
        })
        .expect("滚动应送达阅读视图");
        cx.run_until_parked();
    }

    fn progress_of(view: &Entity<ReaderSession>, cx: &mut TestAppContext) -> f32 {
        view.read_with(cx, |v, _| v.progress())
    }

    fn offset_of(view: &Entity<ReaderSession>, cx: &mut TestAppContext) -> usize {
        view.read_with(cx, |v, _| v.current_offset().unwrap_or_default())
    }

    /// 合成书：仓库里那本三章的 `minimal-zh.epub`，**正文与样式表已经取回来**的那种。
    ///
    /// `document::open` 现在只给索引（库里就存这个，见 `plans/text-out-of-db.md`），
    /// 所以这里按位置把每一章的正文与样式文本补上 —— 这几条测的是排版，手上得有字
    /// 也得有样式。
    fn epub_flow() -> FlowDoc {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/oss/minimal-zh.epub");
        let doc = crate::ui::features::reader::document::open(&path).expect("合成 EPUB 应能打开");
        let mut flow = doc.flow().expect("合成 EPUB 应读成可重排").clone();
        for (ix, chapter) in flow.chapters.iter_mut().enumerate() {
            let text = doc.chapter_source(ix).expect("正文该能从容器取到");
            *chapter = Chapter::inline(chapter.title.clone(), text)
                .with_spine_href(chapter.spine_href.clone());
        }
        flow.css = doc.stylesheet();
        flow
    }

    /// **正文出库之后真正的开书路**：库里只有索引，正文按位置回容器取。
    ///
    /// 这条钉的是三段接得上：
    ///
    /// 1. `save_document` 把正文换成位置（库里读回来 `source` 是空的）；
    /// 2. `with_container` 把索引与容器接上；
    /// 3. 阅读会话能把正文取出并摊成行盒 —— 包括**换章**后再取一次。
    ///
    /// 前两条在 core 有单测，第三条只有到这里才成立：`Book` 的按章缓存、
    /// `Document::set_chapter` 的重排、以及“换章取不到时不该把上一章留在屏上”。
    #[gpui_kit::test]
    fn a_book_whose_text_is_not_in_the_db_loads_it_from_the_container(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/oss/minimal-zh.epub");
        // 库里的 `path` 要指向真文件，所以先拷一份（拷不到的那天也能一眼看出是这条挂了）。
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.epub");
        std::fs::copy(&fixture, &path).unwrap();

        // 导入那一步：读一遍 → 落库（只落索引）→ 丢掉手上那份。
        let store = Arc::new(Store::open_at(&dir.path().join("folio.db")).unwrap());
        let opened = crate::ui::features::reader::document::open(&path).unwrap();
        let id = store.add_book(&path, opened.meta()).unwrap();
        store.save_document(id, opened.as_ref()).unwrap();
        drop(opened);

        // 开书那一步：从库里读回的 `Content` 里**没有正文**，就只有位置。
        let row = store.book(id).unwrap().unwrap();
        let content = store.load_content(id).unwrap().unwrap();
        let Content::Reflow(flow) = &content else {
            panic!("合成 EPUB 该读成可重排");
        };
        assert!(
            flow.chapters.iter().all(|c| c.source().is_empty()),
            "库里不该存正文"
        );
        let doc: Arc<dyn Document> = Arc::from(folio_core::document::with_container(
            DocMeta {
                title: row.title,
                authors: row.authors,
                format: Format::parse(&row.format).unwrap(),
                size: row.size,
            },
            content,
            &path,
        ));

        // 字号故意给到 60：这本合成书的三章都只有几行，默认字号下整本书比一个
        // 视口还短，「把下一章顶到视口顶」物理上不可达，恢复落点会被滚动上限
        // 拉回去、锚章也按「视口顶在哪章」的规矩落回上一章 —— 断言就成了测空集。
        // 大字号下每章都高过视口，换章的落点才可断言（与
        // `the_engine_progress_is_a_character_offset_that_comes_back` 同一个手法）。
        let (view, _handle) =
            open_with_document(cx, doc, prefs(60.0), Locator::start(), 460.0, None, None);
        let first: String = view.read_with(cx, |v, _| v.line_texts().join(""));
        assert!(
            first.contains("夜色从花园那头压过来"),
            "第一章的正文该从容器里取出来摊成行盒：{first:.120}"
        );

        // 换到第二章：这是另一条 zip 条目，也要真的取到（不是上一章的残留）。
        view.update(cx, |v, cx| v.advance_chapter(1, cx));
        settle(cx, _handle);
        let second: String = view.read_with(cx, |v, _| v.line_texts().join(""));
        assert!(
            second.contains("喷泉的水声"),
            "第二章该取到自己的正文：{second:.120}"
        );
        assert!(!second.contains("夜色从花园那头"), "不该把上一章留在屏上");
        assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 1);
    }

    #[gpui_kit::test]
    fn wheel_scrolling_moves_the_reading_position(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = flow();
        let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);

        // 「章首」的真实不变式是字偏移为 0：恢复落点摆的是首行盒顶，章内比例
        // 可能带着首段的段前距（不是严格的 0.0），落库的坐标必须还是 0。
        assert_eq!(offset_of(&view, cx), 0, "刚打开时应在章首，不该已有位移");

        scroll_down(cx, handle, 900.0);
        let progress = progress_of(&view, cx);
        assert!(
            progress > 0.05,
            "真实滚轮事件应产生可测位移，实际比例 {progress:.3}"
        );
    }

    #[gpui_kit::test]
    fn locator_matches_the_scrolled_ratio(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = flow();
        let total = flow.chapters[0].char_len();
        assert!(total > 1000, "语料要够长才滚得动，实际 {total} 字");

        let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
        assert_eq!(offset_of(&view, cx), 0, "未滚动时偏移应为 0");

        scroll_down(cx, handle, 900.0);
        let progress = progress_of(&view, cx);
        let offset = offset_of(&view, cx);

        assert!(offset > 0, "滚动后偏移应前进");
        assert!(offset < total, "偏移不应越出章尾");
        // 正文全是等高段落，所以「滚过的比例」与「读到的字数比例」该吻合。
        // 留一档余量：首行缩进与段距让行顶与字数不是严格线性。
        let ratio = offset as f32 / total as f32;
        assert!(
            (ratio - progress).abs() < 0.1,
            "偏移↔比例 换算不一致：offset {offset} / {total} = {ratio:.3}，progress = {progress:.3}"
        );
    }

    /// 「重启回到原处」里属于我们自己的那一半：位置必须能无损穿过 SQLite 回来，
    /// 连**当时那一档摆法**一起回来（进度存的是字偏移，模式只是它的读数方式）。
    #[gpui_kit::test]
    fn position_survives_a_restart_through_the_store(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = flow();
        let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);

        scroll_down(cx, handle, 900.0);
        let locator = Locator {
            chapter: 0,
            offset: offset_of(&view, cx),
        };
        assert!(locator.offset > 0, "滚动后应能算出字符偏移");

        let store = Arc::new(Store::in_memory().expect("内存库"));
        let id = store
            .add_book(std::path::Path::new("memory://fixture"), &meta("markdown"))
            .expect("入库");
        store
            .save_flow(id, &flow, Format::Markdown)
            .expect("章节入库");
        store
            .set_position(id, PageMode::Continuous.storage_name(), &locator)
            .expect("写入进度");

        let (mode, restored) = store.position(id).expect("读回进度").expect("进度应存在");
        assert_eq!(mode, "continuous");
        assert_eq!(restored, locator, "进度穿过 SQLite 之后必须逐字符一致");

        // 用读回来的进度重开：恢复请求应在排版做完那一帧被消费掉，偏移落回同一个字。
        let (reopened, _) = open_with_store(
            cx,
            &flow,
            "markdown",
            prefs(16.0),
            restored,
            460.0,
            Some(store),
            Some(id),
        );
        assert!(
            !reopened.read_with(cx, |v, _| v.has_pending_restore()),
            "解析完成后恢复请求应被消费掉，而不是永远挂着"
        );
        assert_eq!(offset_of(&reopened, cx), restored.offset);
        assert!(progress_of(&reopened, cx) > 0.0, "重开之后正文真的滚出去了");
    }

    /// M2 的验收门：中文排版基础三件事必须由排版引擎产出。上一版在桌面上这些是
    /// `TextView` 画的，测到的是框架的行为；现在两端都是自绘，测的是引擎自己的行盒。
    #[gpui_kit::test]
    fn the_engine_indents_centers_and_spaces_lines(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = epub_flow();
        assert_eq!(flow.chapters.len(), 3, "spine 三章");

        let (view, _handle) = open(
            cx,
            &flow,
            "epub",
            Reader {
                font_size: 20.0,
                line_height: 1.8,
                ..Reader::default()
            },
            Locator::start(),
            460.0,
        );
        let lines = view.read_with(cx, |v, _| v.lines());
        let texts = view.read_with(cx, |v, _| v.line_texts());
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

        let body = lines
            .iter()
            .find(|l| l.dominant_font_size() == 20.0 && l.x > 0.0)
            .expect("应有一个首行被缩进的正文行");
        assert!(
            (body.x - 40.0).abs() < 1.0,
            "2em 缩进在 20px 字号下应是 40px，实际 {}",
            body.x
        );
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
            .source()
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
        let flow = epub_flow();
        let (view, _handle) = open(
            cx,
            &flow,
            "epub",
            Reader::default(),
            Locator::start(),
            460.0,
        );
        let lines = view.read_with(cx, |v, _| v.lines());

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

    /// M3-E3 验收：EPUB 章里的内嵌图要经排版引擎变成图片片段，`src` 按本章目录解析到
    /// 容器路径，且可见文本里不留哨兵字符。字节按需现取的这一层由 `BookImages` 管，
    /// 这里钉的是排版侧：解析成容器路径、带出固有尺寸。
    #[gpui_kit::test]
    fn images_flow_through_the_engine_as_fragments(cx: &mut TestAppContext) {
        use folio_core::format::ImageRes;

        cx.update(gpui_kit::init);
        let flow = FlowDoc {
            chapters: vec![
                Chapter::inline(
                    "图",
                    "<p>前<img src=\"../Images/pic.png\" alt=\"图\">后</p>",
                )
                .with_spine_href("OEBPS/Text/chap.xhtml"),
            ],
            images: vec![ImageRes {
                href: "OEBPS/Images/pic.png".into(),
                media_type: "image/png".into(),
                width: 40,
                height: 20,
            }],
            ..Default::default()
        };
        let (view, _handle) = open(
            cx,
            &flow,
            "epub",
            Reader::default(),
            Locator::start(),
            460.0,
        );
        let lines = view.read_with(cx, |v, _| v.lines());
        let texts = view.read_with(cx, |v, _| v.line_texts());

        assert_eq!(texts[0], "前后", "行内图不产出字符，哨兵要被去掉");
        let img = lines[0]
            .images()
            .next()
            .expect("../Images/pic.png 应解析成 OEBPS/Images/pic.png 并命中");
        assert_eq!(img.href, "OEBPS/Images/pic.png");
        assert_eq!((img.width, img.height), (40.0, 20.0));
    }

    /// M4 的门：坐标 → 行 → 落点 → 再反解回坐标，必须回到同一个字。
    /// 这一条与「进度落在第几个字上」整套设计同源，所以它红了就是锚点坐标系红了。
    #[gpui_kit::test]
    fn the_engine_progress_is_a_character_offset_that_comes_back(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = epub_flow();
        // 字号故意给到 60：这本合成书的第一章很短，默认字号下整章只有几行、比窗口还短，
        // 那一档下没有可滚的范围，测试就成了测空集。
        let prefs = prefs(60.0);
        let (view, handle) = open(cx, &flow, "epub", prefs.clone(), Locator::start(), 460.0);
        let lines = view.read_with(cx, |v, _| v.lines().len());
        assert!(lines > 4, "大字号下第一章该排出多行，实际 {lines}");
        // 跳转目标取「本章中点」。写死一个坐标的话，CI 上字体不同 → 行高不同 →
        // 可能落在第一行里，那条测试就变成测空集了。
        let total = view.read_with(cx, |v, _| v.text_length());
        assert!(total > 40, "第一章该有几十个字可跳，实际 {total}");
        let target = total / 2;
        // 章首那一行的坐标不是 0：pretty-print 过的 XHTML 里 `<body>` 与 `<p>` 之间的
        // 换行本身是文本节点，也进坐标系。
        let start = offset_of(&view, cx);
        assert!(start < 8, "刚打开该在章首附近，实际坐标 {start}");

        view.update(cx, |v, cx| v.jump_to_offset(target, cx));
        settle(cx, handle);

        let saved = offset_of(&view, cx);
        assert!(
            saved > start,
            "跳到中点坐标 {target} 之后章内偏移该往前走，实际 {start} → {saved}"
        );
        assert!(
            saved <= target,
            "落点该是覆盖 {target} 那一行的行首，不该越过它：{saved}"
        );
        assert!(
            target - saved < target / 4,
            "落点离目标 {target} 太远：{saved}"
        );
        let progress = progress_of(&view, cx);
        assert!(
            progress > 0.0 && progress < 1.0,
            "滚动比例该在 (0,1) 之间，实际 {progress}"
        );

        // 用同一个坐标重开：落回的行反解出来的偏移必须还是它。
        let (reopened, _) = open(
            cx,
            &flow,
            "epub",
            prefs,
            Locator {
                chapter: 0,
                offset: saved,
            },
            460.0,
        );
        assert!(
            !reopened.read_with(cx, |v, _| v.has_pending_restore()),
            "恢复请求该在排版做完的那一帧被消费掉"
        );
        assert_eq!(
            offset_of(&reopened, cx),
            saved,
            "坐标 → 行顶 → 再反解回坐标，必须回到同一个字"
        );
        assert!(
            progress_of(&reopened, cx) > 0.0,
            "重开之后正文真的滚出去了，不是只记了个数"
        );
    }

    /// 默认档只替「从没选过」的人决定：库里存过档位，宽窗口也不许改他的选择。
    /// 反过来（没存过 → 1200 宽给双页）由上面那条 `a_spread_is_two_contiguous...` 钉。
    #[gpui_kit::test]
    fn a_stored_mode_beats_the_wide_screen_default(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = flow();
        let store = Arc::new(Store::in_memory().expect("内存库"));
        let id = store
            .add_book(std::path::Path::new("memory://fixture"), &meta("markdown"))
            .expect("入库");
        store
            .save_flow(id, &flow, Format::Markdown)
            .expect("章节入库");
        store
            .set_position(id, PageMode::Continuous.storage_name(), &Locator::start())
            .expect("写入进度");

        let (view, handle) = open_with_store(
            cx,
            &flow,
            "markdown",
            prefs(16.0),
            Locator::start(),
            1200.0,
            Some(store),
            Some(id),
        );
        settle(cx, handle);
        assert_eq!(
            view.read_with(cx, |v, _| v.mode()),
            PageMode::Continuous,
            "读者存过的那一档优先，默认档不能覆盖它"
        );
    }

    /// 双页成立的那两条判据：一页确实是**两栏**，而且两栏首尾相接 —— 右栏的第一行
    /// 紧跟左栏的最后一行，读者按「先左后右」读不会跳行。
    #[gpui_kit::test]
    fn a_spread_is_two_contiguous_columns_of_one_sheet(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = flow();
        let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 1200.0);
        assert_eq!(
            view.read_with(cx, |v, _| v.mode()),
            PageMode::Spread,
            "1200 宽的窗口没选过档，默认就该是双页（读者选过的照旧，见 set_mode）"
        );

        settle(cx, handle);
        assert_eq!(
            view.read_with(cx, |v, _| v.mode()),
            PageMode::Spread,
            "1200 宽的窗口该放得下两栏"
        );
        let pages = view.read_with(cx, |v, _| v.page_count());
        assert!(pages > 2, "长一章双页也该有多页，实际 {pages}");

        let left = view
            .read_with(cx, |v, _| v.sheet_range(0, 0))
            .expect("第 0 页左栏应有内容");
        let right = view
            .read_with(cx, |v, _| v.sheet_range(0, 1))
            .expect("第 0 页右栏应有内容");
        assert!(!left.is_empty() && !right.is_empty(), "两栏都要有行");
        assert_eq!(
            left.end, right.start,
            "右栏该从左栏断的那一行接着排，不能重叠也不能留缝"
        );

        // 一页一号：翻一页 = 翻一张纸（两栏一起走），偏移该往前走并且不超过两栏的行数。
        let before = offset_of(&view, cx);
        view.update(cx, |v, cx| v.scroll_page(1, cx));
        settle(cx, handle);
        assert_eq!(view.read_with(cx, |v, _| v.page()), 1);
        let after = offset_of(&view, cx);
        assert!(after > before, "翻一张纸该往前走，实际 {before} → {after}");
    }

    /// 窄窗口点「双页」不该得到两条窄柱子：按宽度退回分页。
    /// 这条判据按宽度而不是按 target，所以横屏平板能真双页、竖屏手机自动单页。
    #[gpui_kit::test]
    fn a_narrow_window_falls_back_from_spread_to_one_column(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = flow();
        let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
        view.update(cx, |v, cx| v.set_mode(PageMode::Spread, cx));
        settle(cx, handle);

        assert_eq!(
            view.read_with(cx, |v, _| v.mode()),
            PageMode::Paged,
            "460 宽放不下两栏，该退回分页"
        );
        // 读者**选**的仍然是双页：按钮的选中态读它，不然一退档那格就自己弹回去了。
        assert_eq!(view.read_with(cx, |v, _| v.wanted_mode()), PageMode::Spread);
        // 只有一栏，所以第 1 栏不存在。
        assert!(
            view.read_with(cx, |v, _| v.sheet_range(0, 1)).is_none(),
            "退档之后不该还剩一条空栏"
        );
    }

    /// 换摆法不换位置：连续滚到某个字，切双页（宽窗）再切回来，还在同一个字上。
    /// 进度是「第几个字」，模式只是它的读数方式 —— 这条要是红了，读者的位置就会在
    /// 点一下「双页」时飘掉。
    #[gpui_kit::test]
    fn switching_modes_keeps_the_same_character(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = flow();
        let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 1200.0);
        // 这条测的是「换档不丢字」，起点得自己钉住：1200 宽的默认档已经是双页了。
        view.update(cx, |v, cx| v.set_mode(PageMode::Continuous, cx));
        settle(cx, handle);
        scroll_down(cx, handle, 1200.0);
        let wanted = offset_of(&view, cx);
        assert!(wanted > 0, "先滚到一个字上，实际 {wanted}");

        view.update(cx, |v, cx| v.set_mode(PageMode::Spread, cx));
        settle(cx, handle);
        assert_eq!(view.read_with(cx, |v, _| v.mode()), PageMode::Spread);
        let in_spread = offset_of(&view, cx);
        // 落点是「覆盖那个字的那张纸的开头」，所以它只能等于或早于原位置，
        // 且不能早过一页之内的距离。
        assert!(
            in_spread <= wanted,
            "落位不该越过原来那一行：{wanted} → {in_spread}"
        );

        view.update(cx, |v, cx| v.set_mode(PageMode::Continuous, cx));
        settle(cx, handle);
        assert_eq!(
            offset_of(&view, cx),
            in_spread,
            "切回连续滚动该落在双页那一屏的开头那个字上"
        );
    }

    // ---- M6 选中：真实指针/手势事件驱动的状态机（见 plans/M6-selection.md） ----

    /// 拉丁词的语料：长按吸词的验收要「吸出整词」可判定。
    fn latin_flow() -> FlowDoc {
        FlowDoc {
            chapters: vec![Chapter::inline(
                "第一章",
                "<p>Hello world and then some more words so the paragraph wraps once or twice here</p>",
            )],
            ..Default::default()
        }
    }

    /// 把一个平台事件投进窗口 —— 与真机同一入口（`Window::dispatch_event`），
    /// 不直调会话方法，注册在 paint 里的那层监听必须真的收得到。
    fn dispatch(cx: &mut TestAppContext, handle: AnyWindowHandle, event: PlatformInput) {
        cx.update_window(handle, |_, window, cx| {
            window.dispatch_event(event, cx);
        })
        .expect("事件应送达窗口");
        cx.run_until_parked();
    }

    fn mouse_down(cx: &mut TestAppContext, handle: AnyWindowHandle, at: Point<Pixels>) {
        dispatch(
            cx,
            handle,
            MouseDownEvent {
                button: MouseButton::Left,
                position: at,
                ..Default::default()
            }
            .to_platform_input(),
        );
    }

    fn mouse_move(cx: &mut TestAppContext, handle: AnyWindowHandle, at: Point<Pixels>) {
        dispatch(
            cx,
            handle,
            MouseMoveEvent {
                position: at,
                pressed_button: Some(MouseButton::Left),
                ..Default::default()
            }
            .to_platform_input(),
        );
    }

    fn mouse_up(cx: &mut TestAppContext, handle: AnyWindowHandle, at: Point<Pixels>) {
        dispatch(
            cx,
            handle,
            MouseUpEvent {
                button: MouseButton::Left,
                position: at,
                ..Default::default()
            }
            .to_platform_input(),
        );
    }

    /// 第 `line` 行第 `k` 个字的中心（窗口坐标）。阅读会话直开时页眉让位为 0、
    /// 滚动在章首，所以窗口坐标 = 排版坐标 + 版心左缘。字的横向位置按字号估：
    /// 命中引擎会 snap 到落点最近的字，差小半个字不影响断言。
    fn char_point(
        view: &Entity<ReaderSession>,
        cx: &TestAppContext,
        line: usize,
        k: usize,
    ) -> Point<Pixels> {
        view.read_with(cx, |v, _| {
            let box_ = &v.lines()[line];
            let char_w = box_.dominant_font_size();
            point(
                px(v.prefs.margin_x + box_.x + char_w * (k as f32 + 0.5)),
                px(box_.y + box_.height * 0.5),
            )
        })
    }

    /// 按下 → 拖过阈值 → 松手，成立一个选区（共享给几条用例的前置步骤）。
    fn drag_select(cx: &mut TestAppContext, view: &Entity<ReaderSession>, handle: AnyWindowHandle) {
        let start = char_point(view, cx, 1, 2);
        let end = char_point(view, cx, 1, 4);
        mouse_down(cx, handle, start);
        for step in 1..=4 {
            let t = step as f32 / 4.0;
            mouse_move(
                cx,
                handle,
                point(
                    px(f32::from(start.x) + (f32::from(end.x) - f32::from(start.x)) * t),
                    px(f32::from(start.y) + (f32::from(end.y) - f32::from(start.y)) * t),
                ),
            );
        }
        mouse_up(cx, handle, end);
        assert!(
            view.read_with(cx, |v, _| v.selection().is_some()),
            "拖过阈值的松手应成立选中"
        );
    }

    /// M6 验收：按下 → 拖过阈值 → 松手，选区成立；阈值内的普通点击不留高亮；
    /// 拖选松手攒下的 click 豁免恰好拦住那一下；复制的内容与可视文本一致。
    #[gpui_kit::test]
    fn a_pointer_drag_selects_and_swallows_the_trailing_click(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (view, handle) = open(
            cx,
            &flow(),
            "markdown",
            prefs(16.0),
            Locator::start(),
            460.0,
        );

        // 普通点击（按下即抬，无位移）不留高亮：阈值之内的一切都不算拖选。
        let tap_at = char_point(&view, cx, 0, 1);
        mouse_down(cx, handle, tap_at);
        mouse_up(cx, handle, tap_at);
        assert!(
            view.read_with(cx, |v, _| v.selection().is_none()),
            "普通点击不该留下选中"
        );

        // 真实拖选：down → 8 步 move → up（跨过阈值、跨行）。
        let start = char_point(&view, cx, 0, 1);
        let end = char_point(&view, cx, 2, 3);
        mouse_down(cx, handle, start);
        for step in 1..=8 {
            let t = step as f32 / 8.0;
            mouse_move(
                cx,
                handle,
                point(
                    px(f32::from(start.x) + (f32::from(end.x) - f32::from(start.x)) * t),
                    px(f32::from(start.y) + (f32::from(end.y) - f32::from(start.y)) * t),
                ),
            );
        }
        mouse_up(cx, handle, end);
        let Some(selected) = view.read_with(cx, |v, _| v.selection()) else {
            panic!("拖过阈值的松手应成立选中");
        };
        assert!(
            selected.range.start < selected.range.end,
            "选区应非空：{selected:?}"
        );

        // 拖选松手攒下的豁免：下一次读得到，再读就没有 —— 恰好拦住那一下 click。
        let first = view.update(cx, |v, _| v.consume_tap_suppression());
        let second = view.update(cx, |v, _| v.consume_tap_suppression());
        assert!(first, "拖选松手应拦住紧跟着的 click");
        assert!(!second, "豁免是一次性的");

        // 复制：与可视文本一致（TextMap::slice）。
        view.update(cx, |v, cx| v.copy_selection(cx));
        let copied = cx
            .update(|cx| cx.read_from_clipboard().expect("剪贴板应有内容").text())
            .expect("应是文本");
        let expected = view.read_with(cx, |v, _| {
            v.book()
                .expect("有书")
                .document()
                .text_map()
                .slice(selected.range)
        });
        assert_eq!(copied, expected, "复制内容应与可视文本一致");
    }

    /// M6 验收：长按吸词 —— 点在词首字符上，吸出的是整个词（"Hello"，词界稳定），
    /// 手势结束后菜单条开着。
    #[gpui_kit::test]
    fn a_long_press_snaps_to_the_word_and_opens_the_menu(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (view, handle) = open(
            cx,
            &latin_flow(),
            "markdown",
            prefs(16.0),
            Locator::start(),
            460.0,
        );

        // 第 0 行的首字符「H」内取点：拉丁字宽约半个 em，1/4 em 处稳落第一字。
        let at = view.read_with(cx, |v, _| {
            let box_ = &v.lines()[0];
            point(
                px(v.prefs.margin_x + box_.x + box_.dominant_font_size() * 0.25),
                px(box_.y + box_.height * 0.5),
            )
        });
        for phase in [TouchPhase::Started, TouchPhase::Ended] {
            dispatch(
                cx,
                handle,
                LongPressEvent {
                    phase,
                    start_position: at,
                    position: at,
                }
                .to_platform_input(),
            );
        }
        let (selection, menu) = view.read_with(cx, |v, _| (v.selection(), v.selection_menu_open()));
        let Some(selected) = selection else {
            panic!("长按应选中一个词");
        };
        let text = view.read_with(cx, |v, _| {
            v.book()
                .expect("有书")
                .document()
                .text_map()
                .slice(selected.range.clone())
        });
        assert_eq!(text, "Hello", "点在词首应吸出整词：{selected:?} → {text:?}");
        assert!(menu, "长按成选后菜单条应开着");
    }

    /// M7 划线：选中 → 落一条批注 → 重开这本书还能解回同一区间。
    ///
    /// 两头都要钉住：写不进库是「划了个寂寞」，读不回坐标是「重开就丢」。
    /// 拿掉 `add_highlight` 里那次写库、或 `open_book` 里那句 `load_annotations`，这条就红。
    #[gpui_kit::test]
    fn a_highlight_lands_in_the_store_and_comes_back(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = flow();
        let store = Arc::new(Store::in_memory().expect("内存库"));
        let id = store
            .add_book(std::path::Path::new("memory://fixture"), &meta("markdown"))
            .expect("入库");
        store
            .save_flow(id, &flow, Format::Markdown)
            .expect("章节入库");
        let (view, handle) = open_with_store(
            cx,
            &flow,
            "markdown",
            prefs(16.0),
            Locator::start(),
            460.0,
            Some(store.clone()),
            Some(id),
        );
        drag_select(cx, &view, handle);
        let selected = view.read_with(cx, |v, _| v.selection()).expect("已选中");

        view.update(cx, |v, cx| v.add_highlight(cx));

        let rows = store.annotations(id).expect("读库");
        assert_eq!(rows.len(), 1, "划一下应落一条批注");
        let row = &rows[0];
        assert_eq!(row.chapter, selected.chapter as u32, "锚点属于选区那一章");
        assert_eq!(
            (row.anchor.start, row.anchor.end),
            (selected.range.start, selected.range.end),
            "锚点要就是划中的那一段"
        );
        assert!(
            !row.anchor.exact.is_empty(),
            "锚点要带原文指纹，重排后靠它找回"
        );
        assert_eq!(row.color, "amber", "还没得选颜色时落清单里第一种");
        assert!(
            view.read_with(cx, |v, _| v.selection().is_none()),
            "划完选区收掉：那片底色从此来自批注，不是来自「选中」"
        );

        // 重开这本书：库里那一条读回来，并解回本章坐标上的同一区间。
        let (again, again_handle) = open_with_store(
            cx,
            &flow,
            "markdown",
            prefs(16.0),
            Locator::start(),
            460.0,
            Some(store),
            Some(id),
        );
        settle(cx, again_handle);
        let spans = again.read_with(cx, |v, _| {
            v.annotation_spans.borrow().get(&0).map(|(_, spans)| {
                spans
                    .iter()
                    .map(|span| (span.start, span.end, span.is_stale()))
                    .collect::<Vec<_>>()
            })
        });
        assert_eq!(
            spans,
            Some(vec![(selected.range.start, selected.range.end, false)]),
            "重开这一章，批注要回显在原处且不是 stale"
        );
    }

    /// M6 验收：选中态下的一击——点选区内把菜单请回来、选区不动，点选区外清空，
    /// 两下都消费（`page.rs` 不该再拿它们翻页）。
    #[gpui_kit::test]
    fn a_tap_with_active_selection_clears_or_keeps(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (view, handle) = open(
            cx,
            &flow(),
            "markdown",
            prefs(16.0),
            Locator::start(),
            460.0,
        );
        drag_select(cx, &view, handle);
        let range = view
            .read_with(cx, |v, _| v.selection().map(|selected| selected.range))
            .expect("已选中");

        // 点在选区起点（起拖那个字必在选区内）：菜单回开，选区不动，这一击被消费。
        let inside = char_point(&view, cx, 1, 2);
        let (consumed, menu, still) = cx
            .update_window(handle, |_, window, cx| {
                view.update(cx, |v, cx| {
                    let consumed = v.tap_with_selection(window, cx, inside);
                    (
                        consumed,
                        v.selection_menu_open(),
                        v.selection().map(|s| s.range),
                    )
                })
            })
            .expect("窗口应可更新");
        assert!(consumed, "点在选区内的一击应被选中消费");
        assert!(menu, "菜单条应回开");
        assert_eq!(still, Some(range), "选区不该动");

        // 点选区外（往下六行）：清空，这一击也被消费（不翻页）。
        let outside = char_point(&view, cx, 7, 2);
        let (consumed, cleared) = cx
            .update_window(handle, |_, window, cx| {
                view.update(cx, |v, cx| {
                    let consumed = v.tap_with_selection(window, cx, outside);
                    (consumed, v.selection().is_none())
                })
            })
            .expect("窗口应可更新");
        assert!(consumed, "点选区外的一击也应被消费");
        assert!(cleared, "点选区外应清空选中");
    }

    /// 菜单把选区清掉之后，落在菜单上的那一下仍要被「选中」这一套消费掉。
    ///
    /// 钉的是 `tap_on_body` 里那三条判据的**顺序**：那颗「划线」「取消」在自己的 click
    /// 里就清了选区，等这一下冒泡到正文时 `selection()` 已是 `None` —— 把
    /// `taps_selection_ui` 那一步挪到「有没有选区」之后，这里就会返回 `false`，
    /// 于是它既做了动作、又被当成「点了一下正文」，顶栏与底栏跟着一起冒出来。
    ///
    /// 清完选区不 `settle`：真实时序里 click 处理跑在下一帧绘制之前，而绘制会把菜单
    /// 那份锚位收掉（菜单已经不摆了）。`settle` 一下就把要测的那个窗口期抹平了。
    #[gpui_kit::test]
    fn a_tap_on_the_menu_is_consumed_even_after_it_cleared_the_selection(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (view, handle) = open(
            cx,
            &flow(),
            "markdown",
            prefs(16.0),
            Locator::start(),
            460.0,
        );
        drag_select(cx, &view, handle);
        settle(cx, handle);
        // 拖选松手那一下攒下的「吃掉下一个 click」要先放掉：真实时序里它就是被正文那层
        // 吃掉的（`tap_on_body` 第一条判据）。不吃掉它，下面这一下会被它拦下来，
        // 测试就只是验了第一条判据、看着像验了菜单那条。
        assert!(
            view.update(cx, |v, _| v.consume_tap_suppression()),
            "拖选松手该攒下一次豁免"
        );
        let menu = view
            .read_with(cx, |v, _| v.menu_bounds.get())
            .expect("菜单上屏后该留下锚位");
        view.update(cx, |v, _| v.clear_selection());
        assert!(
            view.read_with(cx, |v, _| v.selection().is_none()),
            "这一条测的就是「选区已经没了」那个时刻"
        );
        let consumed = cx
            .update_window(handle, |_, window, cx| {
                view.update(cx, |v, cx| v.tap_on_body(window, cx, menu.center()))
            })
            .expect("窗口应可更新");
        assert!(
            consumed,
            "落在菜单上的一击要被消费掉，不该再往下开浮层或翻页"
        );
    }

    /// M6 验收：翻走即清 —— 分页翻页与换摆法都不留旧高亮（连续滚动不清，
    /// `set_scroll_y` 不调 `clear_selection`，选中的字随滚动一起走）。
    #[gpui_kit::test]
    fn a_page_turn_and_a_mode_switch_clear_the_selection(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (view, handle) = open(
            cx,
            &flow(),
            "markdown",
            prefs(16.0),
            Locator::start(),
            460.0,
        );

        // 分页翻页：清。
        view.update(cx, |v, cx| v.set_mode(PageMode::Paged, cx));
        settle(cx, handle);
        drag_select(cx, &view, handle);
        view.update(cx, |v, cx| v.scroll_page(1, cx));
        assert!(
            view.read_with(cx, |v, _| v.selection().is_none()),
            "翻走即清"
        );

        // 换摆法：清。
        drag_select(cx, &view, handle);
        view.update(cx, |v, cx| v.set_mode(PageMode::Continuous, cx));
        assert!(
            view.read_with(cx, |v, _| v.selection().is_none()),
            "换摆法即清"
        );
    }

    // ---- 跨章连续滚动：锚章 ± 窗口拼成一条全局坐标（见 book.rs） ----

    /// 向下滚过章尾：正文接上下一章，锚章迁移、进度的章号跟着走。
    #[gpui_kit::test]
    fn scrolling_past_the_chapter_end_flows_into_the_next_one(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (view, handle) = open(
            cx,
            &chapters_flow(3),
            "markdown",
            prefs(16.0),
            Locator::start(),
            460.0,
        );
        assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 0);

        // 100px 一步：章界是按像素判定的，步子太大可能一步迈过整章的可滚区间，
        // 「刚跨进来」的断言就失真了。
        for _ in 0..120 {
            scroll_down(cx, handle, 100.0);
            settle(cx, handle);
            if view.read_with(cx, |v, _| v.chapter_index()) == 1 {
                break;
            }
        }
        assert_eq!(
            view.read_with(cx, |v, _| v.chapter_index()),
            1,
            "滚过章尾该迁移到下一章"
        );
        let texts = view.read_with(cx, |v, _| v.line_texts().join(""));
        assert!(
            texts.contains("第1章"),
            "锚章迁移后屏上该是下一章：{texts:.80}"
        );
        let (chapter, _) =
            view.read_with(cx, |v, _| v.resolve_position().expect("滚得动就该有位置"));
        assert_eq!(chapter, 1, "进度的章号要跟着视口顶走");
        let progress = progress_of(&view, cx);
        assert!(
            progress < 0.5,
            "刚跨进下一章，章内比例该接近开头：{progress:.3}"
        );
    }

    /// 向上滚过章首：接回上一章，锚章跟着回去。
    #[gpui_kit::test]
    fn scrolling_back_up_reaches_the_previous_chapter(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (view, handle) = open(
            cx,
            &chapters_flow(3),
            "markdown",
            prefs(16.0),
            Locator::start(),
            460.0,
        );
        for _ in 0..40 {
            scroll_down(cx, handle, 400.0);
            settle(cx, handle);
            if view.read_with(cx, |v, _| v.chapter_index()) == 1 {
                break;
            }
        }
        assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 1);

        for _ in 0..40 {
            scroll_down(cx, handle, -400.0);
            settle(cx, handle);
            if view.read_with(cx, |v, _| v.chapter_index()) == 0 {
                break;
            }
        }
        assert_eq!(
            view.read_with(cx, |v, _| v.chapter_index()),
            0,
            "滚回章首之上该接回上一章"
        );
        let texts = view.read_with(cx, |v, _| v.line_texts().join(""));
        assert!(
            texts.contains("第0章"),
            "回去之后屏上该是上一章：{texts:.80}"
        );
    }

    /// 进度存在中间章，重开：落点是那一章的章首（全局坐标 = 章基线），
    /// 向上滚能接回上一章 —— 拼接坐标对恢复进度是透明的。
    #[gpui_kit::test]
    fn a_locator_in_a_middle_chapter_restores_into_the_spine(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = chapters_flow(3);
        let locator = Locator {
            chapter: 1,
            offset: 0,
        };

        let store = Arc::new(Store::in_memory().expect("内存库"));
        let id = store
            .add_book(std::path::Path::new("memory://fixture"), &meta("markdown"))
            .expect("入库");
        store
            .save_flow(id, &flow, Format::Markdown)
            .expect("章节入库");
        store
            .set_position(id, PageMode::Continuous.storage_name(), &locator)
            .expect("写入进度");

        let (view, handle) = open_with_store(
            cx,
            &flow,
            "markdown",
            prefs(16.0),
            locator,
            460.0,
            Some(store),
            Some(id),
        );
        assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 1);
        let texts = view.read_with(cx, |v, _| v.line_texts().join(""));
        assert!(texts.contains("第1章"), "该落在第二章的开头：{texts:.80}");

        // 往上滚：上一章就拼在正上方，滚过章首就能接回去。
        for _ in 0..40 {
            scroll_down(cx, handle, -400.0);
            settle(cx, handle);
            if view.read_with(cx, |v, _| v.chapter_index()) == 0 {
                break;
            }
        }
        assert_eq!(
            view.read_with(cx, |v, _| v.chapter_index()),
            0,
            "中间章的章首之上该是上一章"
        );
    }

    /// 拼接窗口钉在锚章 ± 2：滚到哪里，预排版就跟到哪里，窗外整块让出去。
    #[gpui_kit::test]
    fn the_spine_window_stays_pinned_to_the_anchor(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (view, handle) = open(
            cx,
            &chapters_flow(6),
            "markdown",
            prefs(16.0),
            Locator::start(),
            460.0,
        );
        assert_eq!(
            view.read_with(cx, |v, _| v.span_window()),
            vec![0, 1, 2],
            "开书时窗口 = 锚章 + 下方两章"
        );

        // 一路滚到最后一章。
        for _ in 0..120 {
            scroll_down(cx, handle, 400.0);
            settle(cx, handle);
            if view.read_with(cx, |v, _| v.chapter_index()) == 5 {
                break;
            }
        }
        assert_eq!(view.read_with(cx, |v, _| v.chapter_index()), 5);
        assert_eq!(
            view.read_with(cx, |v, _| v.span_window()),
            vec![3, 4, 5],
            "滚到书尾，窗口滑到锚章 + 上方两章，窗外的章整块让出去"
        );
    }

    /// 改字号全链重排：进度的章号不换 —— 坐标是「第几个字」，跨重排存活。
    #[gpui_kit::test]
    fn a_pref_change_reflows_the_whole_spine_without_losing_the_chapter(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (view, handle) = open(
            cx,
            &chapters_flow(3),
            "markdown",
            prefs(16.0),
            Locator::start(),
            460.0,
        );
        for _ in 0..40 {
            scroll_down(cx, handle, 400.0);
            settle(cx, handle);
            if view.read_with(cx, |v, _| v.chapter_index()) == 1 {
                break;
            }
        }
        let (chapter, offset) =
            view.read_with(cx, |v, _| v.resolve_position().expect("滚得动就该有位置"));
        assert_eq!(chapter, 1);

        cx.update(|cx| ConfigHolder::edit_reader(cx, |prefs| prefs.font_size = 24.0));
        settle(cx, handle);
        let (after_chapter, after_offset) =
            view.read_with(cx, |v, _| v.resolve_position().expect("重排后该有位置"));
        assert_eq!(after_chapter, 1, "重排不换章");
        assert!(
            after_offset > 0 && after_offset != offset,
            "字号变了行数变了，章内偏移落在别处才是重排过的证据：{offset} → {after_offset}"
        );
    }
}
