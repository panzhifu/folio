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
use std::sync::Arc;

use folio_core::PageTurn;
use folio_core::content::anchor::line_at_y;
use folio_core::layout::FontKey;
use folio_core::layout::fragment::column_offset;
use folio_core::store::Annotation;
use gpui_kit::base::motion::{Presence, TransitionId};
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    AnyElement, App, Bounds, ClickEvent, Context, Div, ElementId, Entity, EntityId, Hsla,
    InteractiveElement as _, IntoElement, LongPressEvent, ParentElement, Pixels, Render,
    RenderImage, ScrollWheelEvent, SharedString, StatefulInteractiveElement as _, Styled,
    TestSupportExt as _, TouchPhase, WeakEntity, Window, canvas, div, img, point, px, size,
};

use super::book::Book;
use super::book_fonts::BookFonts;
use super::chrome;
use super::document::{Document, Viewport};
use super::image_atlas::ImageAtlas;
use super::image_viewer::ImageViewer;
use super::modules::annotations::{self, Palette};
use super::paint::{GlyphRun, GlyphsElement, SelectionVisual, shape};
use super::scene::{BlockDecor, Scene, VisibleImage};
use super::selection_menu;
use super::session::{AnnotationSpans, ReaderSession, Source, land_at_anchor};
use crate::ui::shared::config_model::ConfigHolder;
use crate::ui::shared::images::DecodeRequest;
use crate::ui::shell::nav_transition;
use crate::ui::shell::navigation::SizeClass;

/// 一次翻页动画在这一帧的样子。时钟已经采完、方向与进度都在里面，两臂各自折成位移。
#[derive(Clone, Copy)]
struct TurnFrame {
    /// 这一趟走了多少（已经过完缓动）：0 = 刚起步，1 = 走完。
    progress: f32,
    /// 出场那一页的页号。连续滚动没有「页」⇒ `None`，那一臂只看 [`Self::scroll`]。
    leaving: Option<usize>,
    /// 连续滚动这一趟的两端（起点 = 起那一刻画出来的 y，终点 = 目标 y）。
    ///
    /// 存两端而不是存算好的当前值：拼接坐标的补偿会在动画期间挪动 `scroll_y`（邻章排版
    /// 回来把整条链挪了），那一趟的两头要**一起**挪，否则补间落在补偿前的坐标上，
    /// 读者看见的是内容突然跳一屏。
    scroll: Option<(f32, f32)>,
    /// `true` = 往后走（下一页 / 往下滚）。
    forward: bool,
}

impl ReaderSession {
    /// 采一次翻页的时钟，走完就把动画状态收掉。没有动画在走 ⇒ `None`。
    ///
    /// 时钟用 gpui 的 `Presence`，与整屏转场同一件东西（见 `ui::shell::layer_offset`）：
    /// 它自己按 `nav_transition` 的时长插值、**自己续帧**（`MotionStatus::Running` 那一支
    /// 里调 `request_animation_frame`），所以这里不再另挂一条帧循环；系统的减弱动态效果
    /// 它直接给终值，动画当场落定而画面不多走一帧。
    ///
    /// 键按 `turn_seq` 分，一趟一把新键：复用同一把会把上一趟**没走完**的状态接到这一趟
    /// 上（读者看见第二次翻页只滑了一半就到位），而没人再采样的那把由 gpui 在帧末自己回收。
    fn turn_frame(&mut self, window: &mut Window, cx: &mut App) -> Option<TurnFrame> {
        let turn = self.turn?;
        // `present` 给 `true`：这一趟只需要「进场」那半 —— 走了多少就是 `progress`，
        // 出场那一页由同一个数反着算（`chrome::slide_shifts` 那两个返回值），
        // 所以不必再为出场页单独挂一把 `Presence`。
        let id = TransitionId::from(ElementId::from(("reader-turn", turn.seq)));
        let progress = Presence::new(id, true)
            .transition(nav_transition(cx))
            .sample(window, cx)
            .progress;
        if progress >= 1.0 {
            // 走完的那一帧起就没有第二张要画了：进场那张的位移正好归零，出场那张已经走满
            // `dist` 出画。少摆一张省下一趟场景重建，而**减弱动态效果那一档走的就是这条路**
            // （`Presence` 在 `reduce_motion` 时直接给终值）⇒ 那一档是当场落定，不多画一张纸。
            self.turn = None;
            return None;
        }
        Some(TurnFrame {
            progress,
            leaving: turn.leaving,
            scroll: turn.scroll,
            forward: turn.forward,
        })
    }

    /// 平移翻页当前沿哪根轴走（读者在全局配置里选的那一档）。
    ///
    /// 每帧读一次而不是挂订阅：这一帧的位移本来就要用这个数，而 `sync_prefs` 早就是
    /// 同一形状的每帧比较。
    fn page_turn_vertical(&self, cx: &App) -> bool {
        matches!(ConfigHolder::appearance(cx).page_turn, PageTurn::Vertical)
    }
}

impl Render for ReaderSession {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        super::perf::reset();
        // 「会话出了元素树」与「正文那一层真画了」是两件事，分开数才分得清帧间隔归谁。
        super::perf::note_render();
        let __render = super::perf::Timer::start();
        // 配置同步：比对一次 `ConfigHolder`，不一致就 `set_prefs_on_all`（逐章作废行单元、
        // shaping 表与排版输入）。改字号那一帧的钱全在这一格里，而它从前没人看。
        let __t = super::perf::Timer::start();
        self.sync_prefs(cx);
        let __sync = __t.ns();
        super::perf::add(|frame| frame.sync_ns += __sync);
        // 正文里的「注」图（脚注标记）那一击要写回会话（把注文抬成弹层），而图片元素是
        // 在本帧的元素树里挂的 —— 给绘制那条链一个句柄，图像元素自己就能调回去。
        let reader_weak = cx.entity().downgrade();
        // 图集那张图解完之后要喊谁重画：就是这个实体（`render` 正跑在它上面）。
        // 用 `EntityId` 而不是句柄，是为了让那条通知在会话已经关掉时也只是打空。
        let view = cx.entity().entity_id();
        // 图片的像素由会话那一层的图集管着（`image_atlas`）：这里每帧问一次「这张
        // 画得出来吗」，帧末再让它把离场够久的格子放掉。
        let atlas = self.atlas.clone();
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
        // 翻页动画这一帧的时钟。必须在进 `match` 之前采：`Presence::sample` 要
        // `&mut Window` + `&mut App`，而 `match &mut self.source` 进去之后 `self` 就只能
        // 逐字段借了（见下面那条同样的取舍）。采完给回的是一个 `Copy` 的快照，两臂各读各的。
        let turn = self.turn_frame(window, cx);
        // 平移沿竖轴还是横轴，是读者在全局配置里选的那一档（`PageTurn`）。
        let vertical = self.page_turn_vertical(cx);
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
        // 链接色：书面 CSS 十有八九不给链接上色（出版工具的目录页就是这么交的），所以这一格
        // 只能由阅读器给，而给的是什么色得**由主题定**（深浅两套各自对），不在这里嵌数。
        //
        // 取的是主题的 `blue` 那一格，**不是语义上的 `link`** —— 后者在框架自带的两套默认主题里
        // 与正文色是同一个值（2026-10-16 在 `TestAppContext` 上量的：浅色 `link` = Hsla{l:0.09,s:0}
        // 而 `foreground` = l:0.039；深色 `link` = l:0.98 与 `foreground` 逐字段相同），拿它上色
        // 等于没接：装上真机看目录那三条仍然是黑的。`blue` 两档都是真蓝（浅 h0.614 s0.832 l0.533 /
        // 深 h0.592 s0.939 l0.678），且仍随主题走，读者换肤不会把它钉死。
        let link = cx.theme().blue;
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
        // 「点这张图」要写出去的那个状态，由阅读页持有（见 `image_viewer`）。没挂上的
        // 会话（单独的渲染测试）图片就不可点。
        let viewer = self.image_viewer.clone();
        // 定版式那一页用什么手势放大：按得住长按的那一端用长按（单击留给翻页），
        // 按不住的那一端（桌面鼠标）用单击图内，翻页让给页外的留白。
        let long_press = crate::ui::host::supports_long_press(cx);
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
            Source::Fixed(book) => {
                // 定版式：页是既定的，没有排版链。摆当前页，等比缩进视口、居中。
                let pages = book.page_count();
                self.laid_out = super::session::LaidOut {
                    mode: chrome::PageMode::Paged,
                    columns: 1,
                    pages,
                    viewport: viewport_height,
                };
                // 恢复落点：定版式的 `locator.chapter` 就是页号。
                if let Some(locator) = self.pending_restore.take() {
                    book.set_page(locator.chapter as usize);
                }
                self.page = book.page();
                self.content_height = 0.0;
                let current = book.page();
                // 一页可能引好几张图（多图页 / 对开跨页）：按**共享高度**横成一行，整体居中。
                // 一张图时这条式子退化成「等比缩进视口」，与普通漫画的旧表现逐像素相同。
                let images = book.page_images(current);
                // 尺寸未知（0）时按视口那一格的宽高比补：一格时等于「铺满」，正是
                // 原来那条兜底；多格共享高度则必须有个比例，视口比例是最不失真的猜。
                let box_aspect = usable / viewport_height;
                let aspect = |w: u32, h: u32| {
                    if w == 0 || h == 0 {
                        box_aspect
                    } else {
                        w as f32 / h as f32
                    }
                };
                let row_aspect: f32 = images.iter().map(|(_, w, h)| aspect(*w, *h)).sum();
                if row_aspect > 0.0 {
                    let height = viewport_height.min(usable / row_aspect);
                    let y = self.header_reserve + (viewport_height - height).max(0.0) / 2.0;
                    // 整行居中：先从左边把行宽的余量让出一半，再逐张往右推进。
                    let mut x = (usable - row_aspect * height) / 2.0;
                    for &(href, w, h) in &images {
                        let width = aspect(w, h) * height;
                        // 解不出字节的那一张空着，但仍然占位：多图页的排版不该因为
                        // 少一张图就把剩下的往左挤。
                        if let Some(request) = book.image_request(href) {
                            let Some(image) = painted(&atlas, request, view, cx) else {
                                continue;
                            };
                            let index = overlays.len();
                            overlays.push(page_image(
                                x,
                                y,
                                width,
                                height,
                                image,
                                (w as f32, h as f32),
                                &viewer,
                                long_press,
                                index,
                            ));
                        }
                        x += width;
                    }
                }
            }
            Source::Book(book) => {
                if mode.is_continuous() {
                    // —— 连续滚动：锚章 ± 窗口拼成一条全局坐标 ——
                    // 1. 邻章补齐入库（0 高占位）：排版作业这就能排进后台队列，
                    //    狂飙滚动时接缝提前两章就铺好了。窗口铺多远要看**视口顶**
                    //    （读者停在长章中后段时，前方那几章也得够一屏多才滚得动）。
                    let __t = super::perf::Timer::start();
                    let __before = book.docs_mut().count();
                    book.ensure_window_spans(&self.prefs, viewport_height, self.scroll_y);
                    let __new = book.docs_mut().count().saturating_sub(__before) as u32;
                    let __spans = __t.ns();
                    super::perf::add(|frame| {
                        frame.spans_ns += __spans;
                        frame.spans_new += __new;
                    });
                    // 2. 按当前（上一帧的）坐标链记下视口顶：排版收完结果好补偿。
                    let snapshot = book.spine_snapshot(self.scroll_y, chapter_gap);
                    // 3. 排版：锚章先提交（恢复落点等它），邻居随后。连续模式只排版
                    //    不切片（一片高 0 是「不分片」的哨兵值）。
                    let __t = super::perf::Timer::start();
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
                    let __layout = __t.ns();
                    super::perf::add(|frame| frame.layout_ns += __layout);
                    // 4–7. 坐标链那一段：补偿、恢复落点、锚章迁移、总高与夹取。
                    let __spine = super::perf::Timer::start();
                    let before_compensate = self.scroll_y;
                    book.compensate_spine(snapshot, &mut self.scroll_y, chapter_gap);
                    // 补偿把整条链挪了多少，正在走的那一趟两端就跟着挪多少：两端留在
                    // 补偿前的坐标里，补间走完落的就是**跳了那一跳之后**的位置，读者看见
                    // 内容突然闪一下。挪完这一笔，动画跟着坐标链一起走。
                    let shift = self.scroll_y - before_compensate;
                    if let Some(t) = self.turn.as_mut()
                        && let Some((from, to)) = t.scroll
                    {
                        t.scroll = Some((from + shift, to + shift));
                    }
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
                        // 恢复是一次跳转（开书、跳目录、换摆法），不是翻一页：让一张从旧位置
                        // 起点的纸平移过来，读者看见的就是自己刚选的那一秒在飞。
                        //
                        // 这里写字段而不调 [`ReaderSession::cancel_turn`]：那一臂里
                        // `font_cache` 还从 `self` 上借着自己，任何 `&mut self` 的方法都过不去，
                        // 只有逐字段的写法合法（`land_at_anchor` 那条注释记的是同一件事）。
                        self.turn = None;
                    }
                    // 6. 锚章迁移：视口顶跨进了邻章（锚章自己排完才有资格答）。
                    let before_migrate = self.scroll_y;
                    book.migrate_spine(&mut self.scroll_y, chapter_gap, &self.prefs);
                    // 迁移就是「视口顶换了一章」：补间那两端是换手之前那条链上的数，让它
                    // 继续走完等于把读者送到隔壁一章的中间去。当场落定，这一帧就给他目标屏。
                    if (self.scroll_y - before_migrate).abs() >= 0.5 {
                        self.turn = None;
                    }
                    // 7. 总高与夹取：全局坐标里「滚到底」是整条链的末端。
                    self.content_height = book.content_height(chapter_gap);
                    let max_scroll = (self.content_height - viewport_height).max(0.0);
                    self.scroll_y = self.scroll_y.clamp(0.0, max_scroll);
                    // 这一帧**画在**哪一个全局 y：动画在走就落在补间上，没有动画就是
                    // `scroll_y` 自己。它是绘制与命中共用的唯一一个数 —— `to_layout` 读的
                    // 就是 `scroll_painted`，两处各读各的话，补间那两百多毫秒里按在字上的
                    // 手指会指到隔壁那一行去。终点可能因为总高在动画期间缩水而越界，夹一下。
                    let painted_y = match turn {
                        Some(frame) => frame
                            .scroll
                            .map_or(self.scroll_y, |(from, to)| {
                                chrome::tween(from, to, frame.progress)
                            })
                            .clamp(0.0, max_scroll),
                        None => self.scroll_y,
                    };
                    self.scroll_painted = painted_y;
                    let __spine = __spine.ns();
                    super::perf::add(|frame| frame.spine_ns += __spine);
                    // 8. 可见带 → 逐章切场景：每章用自己的排版与图片索引，
                    //    换算只差一个章基线。没排完的章不进链，自然一帧都不画。
                    let band = painted_y..(painted_y + viewport_height);
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
                        let __t = super::perf::Timer::start();
                        let mut scene = Scene::build(
                            doc,
                            font_cache,
                            &family,
                            doc.visible_lines(local.clone()),
                            local,
                            band.start - placed.base_y,
                            self.prefs.margin_x,
                            self.header_reserve,
                            foreground,
                            link,
                        );
                        let __scene = __t.ns();
                        let __sh = super::perf::Timer::start();
                        shape_runs(&mut scene, doc, window);
                        let __sh = __sh.ns();
                        super::perf::add(|frame| frame.shape_runs_ns += __sh);
                        let (__runs, __images) =
                            (scene.runs.len() as u32, scene.images.len() as u32);
                        super::perf::add(|frame| {
                            frame.scene_ns += __scene;
                            frame.scenes += 1;
                            frame.scene_runs += __runs;
                            frame.images += __images;
                        });
                        element.extend(scene.runs);
                        for decor in scene.decorations {
                            column.extend(decoration_child(decor));
                        }
                        for image in scene.images {
                            let Some(rendered) = painted(&atlas, image.image.clone(), view, cx)
                            else {
                                continue;
                            };
                            // 序号按「这一帧已经摆了几张」取：连续滚动时一屏可能跨两章，
                            // 各自从 0 数会撞同名。
                            let index = overlays.len();
                            overlays.push(image_child(
                                image,
                                rendered,
                                &viewer,
                                index,
                                &reader_weak,
                            ));
                        }
                        let __fills = super::perf::Timer::start();
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
                        let __fills = __fills.ns();
                        super::perf::add(|frame| frame.fills_ns += __fills);
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
                        // 与连续臂同一句：恢复是一次跳转，不是翻一页，不该让一张从旧页号
                        // 起点的纸平移过来。
                        self.turn = None;
                    }
                    self.page = self.page.min(pages.saturating_sub(1));

                    // 要摆的那几页：定住时只有当前这一页；动画在走时**多出出场那一页**，
                    // 两张沿当前轴同速平移（判据与算法都在 [`chrome::slide_shifts`]）。
                    //
                    // 为什么两张纸共用同一份 `element` / `column` / `overlays`，而不是各配一张
                    // 容器：位移在这里已经烘进 [`Scene::build`] 的 `x` / `top` 两个参数，而那
                    // 两个数是 run、块级装饰与图片**一起**加的（见 `Scene::build` 里那三处加号）。
                    // 各配一张容器就要多摊一倍元素，还要重新安排图与字形谁先入树那一坑
                    // （上面 `column` 的底色注释记的就是它）。定长数组而不是 `Vec`：这一格
                    // 每帧都跑，而定住的时候只用得上一格。
                    let mut slots: [(usize, (f32, f32)); 2] = [(self.page, (0.0, 0.0)); 2];
                    let mut count = 1usize;
                    if let Some(frame) = turn {
                        let dist = if vertical {
                            viewport_height
                        } else {
                            f32::from(viewport_size.width)
                        };
                        let (leaving, incoming) =
                            chrome::slide_shifts(frame.progress, dist, frame.forward);
                        slots[0] = (
                            self.page,
                            if vertical {
                                (0.0, incoming)
                            } else {
                                (incoming, 0.0)
                            },
                        );
                        // 出场页与进场页**不同章**的那种播不了：`set_page` 把页号夹在本章
                        // 页数里，跨章要从底栏那两颗箭头走（`advance_chapter`），而它作废动画。
                        if let Some(page) = frame.leaving.filter(|page| *page != self.page) {
                            slots[1] = (
                                page,
                                if vertical {
                                    (0.0, leaving)
                                } else {
                                    (leaving, 0.0)
                                },
                            );
                            count = 2;
                        }
                    }
                    // 当前页这一帧偏了多少。命中换算读这一格（`to_layout` 里减掉它）：
                    // 不减的话读者快速连点两下，按在新那一页上的手指算到空地那边去。
                    // 出场页那一条缝里的一击不在这格的账里 —— 它落进新页的坐标系，
                    // 而这一趟最多 `nav_transition` 那么久。
                    self.page_shift = slots[0].1;

                    // 分页 / 双页：一页取几片、每片一栏，摆在各自的栏位上。
                    let slices = book.document().slices();
                    for (page, at) in slots.into_iter().take(count) {
                        let (shift_x, shift_y) = at;
                        for index in 0..columns {
                            let Some(slice) = slices.get(page * columns + index) else {
                                continue;
                            };
                            if slice.empty {
                                continue;
                            }
                            let x = self.prefs.margin_x
                                + column_offset(column_width, gap, slice.column)
                                + shift_x;
                            let top = self.header_reserve + shift_y;
                            let mut scene = Scene::build(
                                book.document(),
                                font_cache,
                                &family,
                                slice.lines.clone(),
                                slice.origin_y..(slice.origin_y + slice.height + 1.0),
                                slice.origin_y,
                                x,
                                top,
                                foreground,
                                link,
                            );
                            let __sh = super::perf::Timer::start();
                            shape_runs(&mut scene, book.document(), window);
                            let __sh = __sh.ns();
                            super::perf::add(|f| f.shape_runs_ns += __sh);
                            element.extend(scene.runs);
                            for decor in scene.decorations {
                                column.extend(decoration_child(decor));
                            }
                            // 两张栏的图都要摆进同一份 `overlays`。从前这里是
                            // `overlays = scene.images...collect()`，后一栏把前一栏那几张
                            // **整份覆盖掉** —— 双页对开时只有右边那栏的图画得出来。
                            // 除了少画，它还让图集的账算错：被覆盖掉的那几张已经按「这一帧
                            // 在场」记过一笔，却没有元素去画它（回收看的就是这个数）。
                            // 动画期里出场页那几张同样走这一格 ⇒ 它们按「在场」记账，
                            // 帧末的回收就不会把还在画的那张图放掉。
                            for image in scene.images {
                                let Some(rendered) = painted(&atlas, image.image.clone(), view, cx)
                                else {
                                    continue;
                                };
                                let index = overlays.len();
                                overlays.push(image_child(
                                    image,
                                    rendered,
                                    &viewer,
                                    index,
                                    &reader_weak,
                                ));
                            }
                            let __fills = super::perf::Timer::start();
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
                                top,
                                slice.origin_y..(slice.origin_y + slice.height + 1.0),
                            );
                            let __fills = __fills.ns();
                            super::perf::add(|f| f.fills_ns += __fills);
                        }
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
                    let band_top = self.scroll_painted - base;
                    let band_bottom = self.scroll_painted + viewport_height - base;
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
                            // 与 `Scene::build` 那两个参数同源：正文偏了多少，高亮就跟着偏。
                            self.prefs.margin_x
                                + column_offset(width, gap, slice.column)
                                + self.page_shift.0,
                            slice.origin_y,
                            self.header_reserve + self.page_shift.1,
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
        // 帧末回收图集。**必须**在这一帧的元素树全部搭完之后：中途放掉的格子会被同一帧
        // 里的 `img` 重新传回去，白花一趟上传（`image_atlas::end_frame` 那两条不动的规矩）。
        self.atlas
            .update(cx, |atlas, cx| atlas.end_frame(window, cx));
        // 选中菜单（M6）：摆位是 `selection_menu::slot` 那条纯判断，元素本体在
        // `selection_menu.rs` 里。浮层点不住的教训见 page.rs 模块注释，所以它是压在正文
        // 之上的真按钮；拖动期间收起，松手即回。
        // 卡片高按「这一帧要不要摆颜色那一行」算 —— 挑中的正是库里某一条划线时多一行格子。
        let with_colors = self.selection_annotation().is_some();
        let slot = if self.selection_menu_open() {
            selection_menu::slot(
                menu_anchor.0,
                menu_anchor.1,
                f32::from(viewport_size.height),
                selection_menu::card_height(with_colors),
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

        // `render` 这一段结账。收在**整条元素树搭完**之后：从前它收在连续滚动那一臂的
        // 中途，于是选区矩形、菜单条、overlay 装配那一段两头都不算（`total = render + paint`
        // 是恒等式，那段时间就凭空消失了）。gpui 拿这棵树去做 taffy 布局与合成仍然在
        // 这一格**之外** —— 那部分落在 `gap_us` 里，这一层量不到。
        let __total = __render.ns();
        super::perf::add(|frame| frame.render_ns = __total);
        let listener = cx.listener(|this: &mut Self, event: &ScrollWheelEvent, _window, cx| {
            // 挪动了才重画：滚到章尾夹住、分页没攒够半屏时一整帧的重建可以整份省掉
            // （判据与 gpui 的 `div` 滚动容器、zed editor 的 `scroll` 一致）。
            if this.on_scroll(event) {
                cx.notify();
            }
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

/// 这一帧这张图画得出来吗 —— 顺手把「这一帧在场」记进图集（帧末的回收按这个数放人）。
///
/// 还在解 / 解不出来的给 `None`：那一块空着，正文照排（与从前容器丢失时同一个表现），
/// 解完那一趟会 `notify(view)` 自己逼出一帧，不需要这里去轮询。
fn painted(
    atlas: &Entity<ImageAtlas>,
    request: DecodeRequest,
    view: EntityId,
    cx: &mut App,
) -> Option<Arc<RenderImage>> {
    let __t = super::perf::Timer::start();
    let image = atlas.update(cx, |atlas, cx| atlas.painted(&request, view, cx));
    let __image = __t.ns();
    super::perf::add(|frame| frame.image_ns += __image);
    image
}

/// 一片里的一张图：绝对摆位交给 gpui 的 `img`，外加「点一下摊开看」。
///
/// 一张图若是**脚注标记**（老书把标记做成一张小「注」图），那一击归脚注 —— 抬起注文，
/// 不进图片查看器。判据是排版期就解好的 `image.footnote`（见 [`VisibleImage::footnote`]），
/// 这一层只按结果分流。
///
/// `rendered` 是图集那一层交回来的像素：`image` 里只有摆位与那条活儿（`DecodeRequest`），
/// 像素不随场景走（见 [`super::image_atlas`]）。
fn image_child(
    image: VisibleImage,
    rendered: Arc<RenderImage>,
    viewer: &Option<Entity<ImageViewer>>,
    index: usize,
    reader: &WeakEntity<ReaderSession>,
) -> AnyElement {
    let natural = image.natural;
    let drawable = rendered.clone();
    let frame = div()
        .absolute()
        .left(px(image.x))
        .top(px(image.top))
        .w(px(image.width))
        .h(px(image.height))
        .child(img(rendered).size_full());
    // 脚注标记优先：它是「注」图，不是插图，点它要的是注文而不是放大。
    if let Some(note) = image.footnote {
        return footnote_marker(frame, note, reader.clone(), index);
    }
    match viewer {
        Some(viewer) => {
            open_on_tap(frame, viewer.clone(), drawable, natural, index).into_any_element()
        }
        None => frame.into_any_element(),
    }
}

/// 给一张脚注标记图挂上「点开看注」。
///
/// 与 `open_on_tap` 同一条取舍（`block_mouse_except_scroll` 把这一击从整屏正文的
/// 分区里拿回来），只是去向换成会话的脚注状态：注文 + 点击位置一起交出去，弹层由
/// 阅读页画（见 `page::ReadingPage::footnote_popover`）。
fn footnote_marker(
    frame: Div,
    note: String,
    reader: WeakEntity<ReaderSession>,
    index: usize,
) -> AnyElement {
    frame
        .id(("reader-footnote-image", index))
        .block_mouse_except_scroll()
        .on_click(move |event: &ClickEvent, _window, cx| {
            let at = event.position();
            let note = note.clone();
            let _ = reader.update(cx, |session, cx| session.show_footnote(note, at, cx));
        })
        .test_support()
        .into_any_element()
}

/// 定版式一页的元素：绝对定位到 `(x, y)`、撑满 `w × h`，外加「看大图」那一条入口。
///
/// 这一臂的入口与正文插图不同，而且是**按端不同**：定版式的页几乎占满整屏，单击就是
/// 翻页本身。按得住长按的那一端把入口挂在长按上（单击一个字都不动，翻页照常）；
/// 按不住的那一端（桌面鼠标根本没有长按）挂单击，翻页让给页外的留白与音量键。
/// 判据在 [`crate::ui::host::supports_long_press`]，这一层不写 `cfg`。
///
/// 九个参数摆平一页：前四个是排版给好的摆位，第五六个是那张图与它的固有尺寸，
/// 后三个是「怎么开」——拆开传反而要多造一个只活一帧的结构体。
#[allow(clippy::too_many_arguments)]
fn page_image(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    image: Arc<RenderImage>,
    natural: (f32, f32),
    viewer: &Option<Entity<ImageViewer>>,
    long_press: bool,
    index: usize,
) -> AnyElement {
    let frame = div()
        .absolute()
        .left(px(x))
        .top(px(y))
        .w(px(w))
        .h(px(h))
        .child(img(image.clone()).size_full());
    let Some(viewer) = viewer.clone() else {
        return frame.into_any_element();
    };
    if long_press {
        frame
            .child(long_press_to_open(image, natural, viewer))
            .into_any_element()
    } else {
        open_on_tap(frame, viewer, image, natural, index).into_any_element()
    }
}

/// 给一张图挂上「点开放大」。
///
/// `block_mouse_except_scroll` 而不是 `occlude`：这一击要从正文那三层分区里拿回来
/// （gpui 的一击会发给每一个 hitbox 含按下点的 handler，而正文那层是整屏的 —— 见
/// `page.rs` 里顶栏那条同一取舍），但读者**从图上起手**也要能把正文滚走，所以只挡鼠标
/// 不挡滚动。
///
/// 带 id 才挂得住点击，`index` 就是这一帧里它是第几张被摆上屏的图（按 `overlays` 当时的
/// 长度取，跨章连续滚动时也不会撞名）。名字化的 id 还留了个口子：宿主的测试可以按
/// `("reader-image", 0)` 直接点在那张图上。
fn open_on_tap(
    frame: Div,
    viewer: Entity<ImageViewer>,
    image: Arc<RenderImage>,
    natural: (f32, f32),
    index: usize,
) -> impl IntoElement {
    frame
        .id(("reader-image", index))
        .block_mouse_except_scroll()
        .on_click(move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            let viewport = viewport_size(window);
            let image = image.clone();
            viewer.update(cx, |viewer, cx| {
                viewer.open(image, natural, viewport);
                cx.notify();
            });
        })
        // 挂在最外层：宿主的测试要按 `("reader-image", 序)` 点在这张图上。
        .test_support()
}

/// 「按住这一页」的监听，挂在 `canvas` 的绘制回调里：`window.on_mouse_event` 只活到本帧
/// 事件分发结束，每个绘制帧都要重挂一遍（与 [`super::paint`] 里选中那几条同一规矩）。
///
/// `prevent_default` 就是**认领**这一手势：gpui 拿 `default_prevented` 当「这一手势被
/// 接走了」的依据（`window.rs` 里 `resolve_long_press(claimed)`）。不认领的话手指抬起时
/// 会补出一个合成点击，而那一击的意思正是翻页 —— 按住看大图会变成「翻一页再看大图」。
fn long_press_to_open(
    image: Arc<RenderImage>,
    natural: (f32, f32),
    viewer: Entity<ImageViewer>,
) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window: &mut Window, _cx: &mut App| {
            let viewer = viewer.clone();
            window.on_mouse_event(move |event: &LongPressEvent, phase, window, cx| {
                if !phase.bubble() || event.phase != TouchPhase::Started {
                    return;
                }
                // 这一格只负责自己那一页：抬起之前手指挪出图外就不算「按住这张图」。
                if !bounds.contains(&event.start_position) {
                    return;
                }
                let viewport = viewport_size(window);
                let image = image.clone();
                viewer.update(cx, |viewer, cx| {
                    viewer.open(image, natural, viewport);
                    cx.notify();
                });
                window.prevent_default();
                cx.stop_propagation();
            });
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

/// 窗口视口，两个轴都是 CSS 像素。查看器第一帧拿它当摆位的格子，之后由那一层自己
/// 量出来的宽高校正（见 `image_viewer::overlay` 里那块 `canvas`）。
fn viewport_size(window: &Window) -> (f32, f32) {
    let size = window.viewport_size();
    (f32::from(size.width), f32::from(size.height))
}

/// 把「选区 ∩ 一个可见带」换成元素局部的矩形，追加进 `out`。
///
/// `x` / `origin_y` 是这一栏（片）的摆位，与 `Scene::build` 同一套换算；
/// `band` 是排版坐标里的可见 y 区间——夹进 `sub` 的字符区间在带与带的接缝处
/// 可能越进下一带（`coordinate_of_line` 给的是下一带首字符），矩形再按 y 滤一遍。
///
/// 行号在进 `rects_for_range_in` **之前**就按这两条夹好（字符索引 ∩ 可见带，
/// `Document::selection_rects_in_band`），所以下面那道 y 滤是重复的一道：留着，
/// 因为「窄化算错就少一块底色」这种账只能靠两道独立的不等式互相兜。
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
    for rect in doc.selection_rects_in_band(window, cx, sub, band.clone()) {
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

/// 把这一屏的 run 逐条过一遍章的 shaping 表：命中的抬一次引用计数，未命中的当场 shape
/// 一次并存进去（[`Document::shaped_line`]）。
///
/// 为什么在这一层而不是绘制层：查表要同时拿到 `Document`（表挂在它身上）与 `Window`
/// （shape 要走窗口级文本系统），而 `GlyphsElement::paint` 只有后者。
///
/// 没有身份的那条 run（`key` 是 `None`：兜底行、测试里手搓的）跳过，绘制时它自己会 shape。
fn shape_runs(scene: &mut Scene, doc: &Document, window: &Window) {
    for run in &mut scene.runs {
        let Some(key) = run.key else { continue };
        let shaped = doc.shaped_line(key, || shape(window, run));
        run.shaped = Some(shaped);
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
