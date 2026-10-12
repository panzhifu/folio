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
use super::session::{AnnotationSpans, ReaderSession, Source, land_at_anchor};
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
                        let locator = land_at_anchor(self.pending_anchor.take(), locator, book);
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
                        // 这一章在排版坐标里的可见带（减掉章基线）。行区间向引擎要：
                        // 「哪些行与这条带有关」是行盒的知识，不是界面的（`plans/阅读管线.md`
                        // §十一 第 4、10 条）——以前这里递的是 `0..行数`，每帧扫一遍整章。
                        let local = (band.start - placed.base_y)..(band.end - placed.base_y);
                        let scene = Scene::build(
                            doc,
                            font_cache,
                            &family,
                            doc.visible_lines(local.clone()),
                            local,
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
                        let locator = land_at_anchor(self.pending_anchor.take(), locator, book);
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
    let spans =
        annotations::resolved_for(cache, stored, chapter, doc.chapter_hash(), doc.text_map());
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
mod tests;
