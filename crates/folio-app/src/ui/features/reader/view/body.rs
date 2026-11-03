//! 阅读视图：把会话的状态摆成元素——逐行画字形、把图片绝对定位、接住滚轮。
//!
//! 这一层是**每帧控制器 + 绘制器**：排版本身不在这里（在
//! [`crate::ui::features::reader::model::chapter_layout`]），状态与命令也不在这里
//! （在 [`crate::ui::features::reader::model::session`]），但这一帧要排的作业、
//! 要补的坐标链、要落的章界，以及它们的提交都在这里结算。`render` 按当前模式
//! 挑出要摆的那几「片」，把排版行摊成 [`Scene`]，再挂上滚轮监听。
//!
//! 三种模式只差 [`Viewport`] 的那三个数，切片由核心分片器给出：
//!
//! - **连续滚动**：锚章 ± 窗口拼成一条全局坐标（[`crate::ui::features::reader::model::book::Book`]），位置是
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
//!
//! 读屏（TalkBack 那一类）也接在这一层的根节点上：正文是逐字形自绘的，accesskit 那棵树
//! 里本来一个字都没有，所以要按「这一屏的行」现造一份合成子节点（[`spoken_lines`] 收集，
//! `folio-reader` 那一句 `.role()` + `.a11y_synthetic_children()` 入树）。这份收集只在
//! `Window::is_a11y_active()` 为真时跑 —— 没有读屏连着就一个节点都不建，也不为它分配。

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use folio_core::PageTurn;
use folio_core::content::anchor::line_at_y;
use folio_core::layout::fragment::column_offset;
use gpui_kit::base::motion::{Presence, TransitionId};
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    A11ySubtreeBuilder, AnyElement, App, Bounds, Context, ElementId, Entity, EntityId, Hsla,
    InteractiveElement as _, IntoElement, PageCurlMesh, ParentElement, Render, Role, ScrollEvent,
    SharedString, StatefulInteractiveElement as _, Styled, TestSupportExt as _, WeakEntity, Window,
    accesskit, div, point, px, size,
};

use crate::ui::features::reader::components::image_viewer::ImageViewer;
use crate::ui::features::reader::components::selection::{self, SelectionVisual};
use crate::ui::features::reader::model::annotations::Palette;
use crate::ui::features::reader::model::book::Book;
use crate::ui::features::reader::model::chapter_layout::Document;
use crate::ui::features::reader::model::chrome;
use crate::ui::features::reader::model::image_atlas::ImageAtlas;
use crate::ui::features::reader::model::page_geometry::{
    BandPolicy, PageBox, PageInsets, PageRect,
};
use crate::ui::features::reader::model::paging::PrefetchSide;
use crate::ui::features::reader::model::prefetch;
use crate::ui::features::reader::model::session::{
    ImageHit, PageRef, ReaderSession, Source, StackHit, TurnRun,
};
use crate::ui::features::reader::model::swipe::{release_easing, release_transition};
use crate::ui::features::reader::motion::turn_animation;
use crate::ui::features::reader::view::bands;
use crate::ui::features::reader::view::paint::GlyphsElement;
use crate::ui::services::config::ConfigHolder;
use crate::ui::shell::{turn_duration, turn_transition};

/// 定版式预取几页。
///
/// 两张是「读者手上这一页 + 下一张」的量：720 档预览一页约 2 MB，两页叠在 24 MB 的
/// 手机预算里还够；一张的话快速滑动时读者到那一页它往往还在画（手机上栅格化一页是
/// 几十到几百 ms）。再多就不再给了 —— 内存先炸，而滑得更快的读者本来也看不清。
mod a11y;
pub(in crate::ui::features::reader) mod art;
mod capture;
mod decor;
mod fixed;
mod gestures;
pub(in crate::ui::features::reader) mod images;
pub(in crate::ui::features::reader) mod marks;
mod paper;
mod reflowable;
mod shape;
mod turn_view;

use art::PageArt;
use capture::{capture_wanted, sheet_area_in_page};
use gestures::{fixed_gestures, touch_follow};
use marks::{MarkSpot, highlight_rects};

const PREFETCH_RADIUS: usize = 2;

/// 一次翻页动画在这一帧的样子。时钟已经采完、方向与档位都在里面，两臂各自折成位移。
#[derive(Clone, Copy)]
struct TurnFrame {
    /// 这一趟走了多少（已经过完缓动）：0 = 刚起步，1 = 走完。
    progress: f32,
    /// 起这一趟时抄下来的那一档（见 [`Turn::style`]）：摆位层按它取轴、取两张纸的位移与层级。
    style: PageTurn,
    /// 出场与进场那两张纸。连续滚动没有「页」⇒ 两格都是 `None`，那一臂只看 [`Self::scroll`]。
    leaving: Option<crate::ui::features::reader::model::session::PageRef>,
    arriving: Option<crate::ui::features::reader::model::session::PageRef>,
    /// 连续滚动这一趟的两端（起点 = 起那一刻画出来的 y，终点 = 目标 y）。
    ///
    /// 存两端而不是存算好的当前值：拼接坐标的补偿会在动画期间挪动 `scroll_y`（邻章排版
    /// 回来把整条链挪了），那一趟的两头要**一起**挪，否则补间落在补偿前的坐标上，
    /// 读者看见的是内容突然跳一屏。
    scroll: Option<(f32, f32)>,
    /// 读者这一笔的手（起手点 + 此刻位置，版心内归一化）。折页那一档按它算折线。
    grip: Option<crate::ui::features::reader::motion::turn_animation::Grip>,
    /// 这一趟的进度由谁给（松手那一腿要把「手指此刻」补间到落点，所以要看见 `Released`）。
    run: crate::ui::features::reader::model::session::TurnRun,
    /// 松手那一腿自己走了几成（缓动之后，0 = 松手那一刻、1 = 走完）。只有 `Released`
    /// 那一腿有值；折页拿它当手指的时钟 —— 这一格**不能**从 `progress` 反解：折页的
    /// 视觉走量比 `progress` 的归一化长（见 `turn_view::plan_sheets`），拖过头时
    /// `from = 1`，反解的分母被夹成 `1e-3` 会恒得 0，纸就定在松手那一格不动。
    release: Option<f32>,
    /// `true` = 往后走（下一页 / 往下滚）。
    forward: bool,
    /// `true` = 这一本从右往左读。只有折页那一档读它（它决定纸的**自由边**在哪一侧，
    /// 见 `turn_animation::Pose::free_edge_right`）；平移/覆盖那几档今天还没接这一格。
    rtl: bool,
}

/// 一次翻页在这一帧产出的全部东西：纸面阴影、卷曲网格、三份捕获副本与它们的目标页。
///
/// 两臂（可重排 / 定版式）写同一份，`body::render` 后半段的装配只读它。从前这是
/// 十五个并列的 `&mut` 出参，每加一层捕获就要在七个结构体里各加一格。
struct TurnPaint {
    /// 盖在两张纸之上的折面投影与纸边光；其余档位是空清单。
    shades: turn_animation::Shades,
    /// 这一帧要画的卷曲网格。网格取代旧页的字形与图片 —— 旧页已经烘进纹理。
    mesh: Option<PageCurlMesh>,
    /// 被揭起来的那一张 leaf（双页 = 半张跨页，单页 = 整张纸）。捕获纹理、网格与
    /// 手指归一化三处共用这一块；`None` = 这一帧没有折页在飞。
    leaf: Option<turn_animation::Area>,
    /// 捕获层 1：正面（旧页）。
    front: Option<paper::PaperCapture>,
    /// 捕获层 2：背面（卷起来那张纸的另一面）。
    back: Option<paper::PaperCapture>,
    /// 捕获层 3：进场页。
    incoming: Option<paper::PaperCapture>,
    /// 背面那一栏要横向挪进 leaf 那一格：纹理按 leaf 那块矩形烘，网格也按 leaf 的尺寸采。
    back_dx: f32,
    /// 这一帧要烘进捕获层 2 的是哪一格（`(章, 页, 物理栏)`）。`None` = 这一帧不烘背面。
    back_key: Option<(usize, usize, usize)>,
    /// 这一帧要烘进场页的是哪一格（`(章, 页)`）+ 烘那一帧进场页自己的位移。
    incoming_bake: Option<((usize, usize), (f32, f32))>,
    /// 这一帧有没有页面走平摊网格：网格纹理里已经含读数带，末端就不补全局兜底带。
    mesh_page_drawn: bool,
    /// 这一趟翻到哪一页。翻页途中**底下那一页**的页眉/页脚要按它念，而捕获层 2
    /// 烘过一次之后 `back_key` 就不再是 `Some` 了。
    arriving: Option<PageRef>,
}

impl TurnPaint {
    fn new() -> Self {
        Self {
            shades: turn_animation::Shades::new(),
            mesh: None,
            leaf: None,
            front: None,
            back: None,
            incoming: None,
            back_dx: 0.0,
            back_key: None,
            incoming_bake: None,
            mesh_page_drawn: false,
            arriving: None,
        }
    }
}

impl ReaderSession {
    /// 采一次翻页的时钟，走完就把动画状态收掉。没有动画在走 ⇒ `None`。
    ///
    /// 时钟用 gpui 的 `Presence`，与整屏转场同一件东西（见 `ui::shell::layer_offset`）：
    /// 它自己按这一趟那一个时长插值（`Turn::over`，见 [`turn_transition`]）、**自己续帧**
    /// （`MotionStatus::Running` 那一支
    /// 里调 `request_animation_frame`），所以这里不再另挂一条帧循环；系统的减弱动态效果
    /// 它直接给终值，动画当场落定而画面不多走一帧。
    ///
    /// 键按 `turn_seq` 分，一趟一把新键：复用同一把会把上一趟**没走完**的状态接到这一趟
    /// 上（读者看见第二次翻页只滑了一半就到位），而没人再采样的那把由 gpui 在帧末自己回收。
    fn turn_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<TurnFrame> {
        let turn = self.turn?;
        // 进度由谁给，看这一趟是谁在驱动它。
        //
        // 手指还握着时**不采时钟**：`Presence` 的意义是「自己续帧追一个终点」，而此刻终点
        // 在手指上，两头一起动就是画面在拉扯。
        let (progress, walked, release) = match turn.run {
            TurnRun::Following { t } => (t, false, None),
            run => {
                // `present` 给 `true`：这一趟只需要「进场」那半 —— 走了多少就是 `progress`，
                // 出场那一页由同一个数反着算（`turn_animation::slide::slide_shifts` 那两个返回值），
                // 所以不必再为出场页单独挂一把 `Presence`。
                // 松手那一腿有自己的时长与曲线：它要以松手那一刻的速度起步（
                // `swipe::release_duration` / `release_shape`），而共用那一条 `EaseInOut`
                // 两端速度都是 0，接手就是「手一抬纸先停死」。点屏 / 按键那一趟用的是读者
                // 那一格的快照（`Turn::over`）—— 它没有前一段速度要接，但也**不能**每帧现读：
                // 拖那条滑条每一帧都在写配置，现读等于给正在走的时钟每帧重新瞄准。
                let transition = match run {
                    TurnRun::Released { over, .. } => release_transition(over),
                    _ => turn_transition(turn.over, cx),
                };
                let id = TransitionId::from(ElementId::from(("reader-turn", turn.seq)));
                // 这一采管两件事：按这一趟的时长自己续帧，以及系统的减弱动态效果直接给终值。
                let sampled = Presence::new(id, true)
                    .transition(transition)
                    .sample(window, cx);
                match run {
                    TurnRun::Released {
                        from,
                        revert,
                        over,
                        since,
                    } => {
                        // 从**松手那一刻**起算，不是从这一帧起算：抬手到第一帧之间那一截
                        // 本来就是这一腿走过的时间。把它当成 0，纸会先停死一帧、下一帧再
                        // 一下跳出剩余行程的一成多 —— 读者读到的是「手一抬卡一下」。
                        let elapsed = cx
                            .background_executor()
                            .now()
                            .saturating_duration_since(since);
                        let eased = if cx.reduce_motion() {
                            1.0
                        } else {
                            let t = elapsed.as_secs_f32() / over.as_secs_f32().max(1e-6);
                            release_easing().sample(t.clamp(0.0, 1.0))
                        };
                        // 松手那一腿不是从 0 走的：从手指那一点接着走完或走回，否则读者会
                        // 看见自己握过的那一截被重放一遍。
                        let progress = chrome::tween(from, if revert { 0.0 } else { 1.0 }, eased);
                        (progress, cx.reduce_motion() || elapsed >= over, Some(eased))
                    }
                    _ => (sampled.progress, sampled.progress >= 1.0, None),
                }
            }
        };
        if walked {
            // 弹回那一趟不落地：它「走完」的意思是回到出发那一张，页号与锚章一个字都不动。
            // 定版式没有章可跨（`chapter` 是页号），也不走 `finish_chapter_turn`。
            if turn.run.lands()
                && !self.is_fixed()
                && let Some(to) = turn.arriving
                && to.chapter != self.chapter_index()
            {
                self.finish_chapter_turn(to);
            }
            // 走完的那一帧起就没有第二张要画了：进场那张的位移正好归零，出场那张已经走满
            // `dist` 出画。少摆一张省下一趟场景重建，而**减弱动态效果那一档走的就是这条路**
            // （`Presence` 在 `reduce_motion` 时直接给终值）⇒ 那一档是当场落定，不多画一张纸。
            self.turn = None;
            // 进场页那张纹理只服务这一趟：落定之后这一页就是「当前页」，正面那张纹理
            // 接管它的静态捕获，进场页这张用不着了（纹理留在渲染器里等下一趟覆盖）。
            self.captured_incoming = None;
            // 捕获是「动画不在飞」时才烘的（`capture_wanted` 那一格），而**续帧只归时钟**：
            // 这一刻场上既没有动效也没有别的通知，下一帧根本不会来 —— 于是这一页的纹理要等
            // 读者下一次碰屏幕才烘。仿真折页的门读的就是它，读者读到的是「翻完这一页之后，
            // 要再点一下才翻得动」。落定这一帧补一发重画，纹理下一帧就烘上。
            cx.notify();
            return None;
        }
        Some(TurnFrame {
            progress,
            style: turn.style,
            leaving: turn.leaving,
            arriving: turn.arriving,
            scroll: turn.scroll,
            grip: turn.grip,
            run: turn.run,
            release,
            forward: turn.forward,
            rtl: self.progression_rtl(),
        })
    }
}

/// 选区这一帧的屏幕几何输入。
struct SelectionGeom {
    mode: chrome::PageMode,
    columns: usize,
    usable: f32,
    gap: f32,
    page_shift: (f32, f32),
    content_top: f32,
    viewport_height: f32,
    chapter_gap: f32,
}

/// 选区这一帧的产物：高亮/把手，外加菜单条的锚位。
struct SelectionFrame {
    visual: SelectionVisual,
    /// 首行高亮的顶与末行高亮的底（菜单条放不下时往下掉）。
    anchor: (Option<f32>, Option<f32>),
    /// 首末高亮中心的横向中点。
    x: Option<f32>,
}

impl ReaderSession {
    /// 把「选区 ∩ 可见范围」换成元素局部矩形，并算出菜单条锚位。
    ///
    /// `book` 只在可重排那一族给：定版式的高亮已经由它自己那一臂算好（`fixed_visual`），
    /// 这里只接过来补把手与锚位。两端都滚出可视范围就没有把手，与 gpui-kit
    /// 「端点跟着它标记的字消失」同一套规矩。
    fn selection_frame(
        &self,
        book: Option<&Book>,
        window: &Window,
        cx: &App,
        geom: SelectionGeom,
        fixed_visual: Option<SelectionVisual>,
    ) -> SelectionFrame {
        let SelectionGeom {
            mode,
            columns,
            usable,
            gap,
            page_shift,
            content_top,
            viewport_height,
            chapter_gap,
        } = geom;
        let mut visual = fixed_visual.unwrap_or_else(|| SelectionVisual {
            color: cx.theme().selection,
            ..Default::default()
        });
        if let (Some(book), Some(selected)) = (book, self.selection.clone()) {
            if let Some(doc) = book.span_doc(selected.chapter)
                && doc.is_current()
            {
                if mode.is_continuous() {
                    // 章基线把视口顶换算成章内坐标：可见带 → 覆盖它的字符区间；
                    // 选区与它求交，矩形只扫可见的那几行。
                    let base = book.base_of(selected.chapter, chapter_gap).unwrap_or(0.0);
                    let layout = doc.layout();
                    let total = doc.text_map().chars();
                    // 章内排版坐标 = 全局 y − 章基线（与 `local_band` 同一口径：
                    // `content_top` 在绘制式子里已经加过一次，这里不许再减）。
                    let band = local_band(
                        self.scroll_painted..(self.scroll_painted + viewport_height),
                        base,
                    );
                    let band_top = band.start;
                    let band_bottom = band.end;
                    let first = line_at_y(layout, band_top);
                    let last = line_at_y(layout, band_bottom);
                    let sub = selected.range.start.max(doc.coordinate_of_line(first))
                        ..selected.range.end.min(if last + 1 < layout.lines.len() {
                            doc.coordinate_of_line(last + 1)
                        } else {
                            total
                        });
                    highlight_rects(
                        &MarkSpot {
                            doc,
                            window,
                            cx,
                            x: self.prefs.margin_x,
                            origin_y: band_top,
                            // 连续滚动永远走横排（竖排只在分页那一档落地），长度这一格不参与。
                            flow_len: band_bottom - band_top,
                            header: content_top,
                            band: band_top..band_bottom,
                        },
                        sub,
                        &mut visual.highlights,
                    );
                } else {
                    // 分页 / 双页：翻走即清，选区必在锚章 —— 逐栏求交照搬。
                    let layout = doc.layout();
                    let total = doc.text_map().chars();
                    let width =
                        folio_core::layout::fragment::column_width(usable, columns, gap).max(1.0);
                    for index in 0..columns {
                        let Some(slice) = doc.slice_of(self.page, index) else {
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
                            &MarkSpot {
                                doc,
                                window,
                                cx,
                                // 与 `Scene::build` 那两个参数同源：正文偏了多少，高亮就跟着偏。
                                x: self.prefs.margin_x
                                    + column_offset(width, gap, slice.column)
                                    + page_shift.0,
                                origin_y: slice.origin_y,
                                flow_len: slice.height,
                                header: content_top + page_shift.1,
                                // 带说的是推进轴：递分片自己那一段，比从首末行盒再折一次准，
                                // 而横排下这两个数是同一个区间（片的长就是末行底减首行顶）。
                                band: first_box.y..(last_box.y + last_box.height),
                            },
                            sub,
                            &mut visual.highlights,
                        );
                    }
                }
            }
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
        let anchor = (
            visual.highlights.first().map(|rect| f32::from(rect.top())),
            visual
                .highlights
                .last()
                .map(|rect| f32::from(rect.bottom())),
        );
        let x = visual
            .highlights
            .first()
            .zip(visual.highlights.last())
            .map(|(first, last)| (f32::from(first.center().x) + f32::from(last.center().x)) * 0.5);
        SelectionFrame { visual, anchor, x }
    }
}

/// 两臂共用的帧内只读输入；每条臂只读自己关心的字段。
#[derive(Clone)]
struct RenderFrame<'a> {
    mode: chrome::PageMode,
    viewport_height: f32,
    content_top: f32,
    chapter_gap: f32,
    page_box: PageBox,
    page_key: (usize, usize),
    capture_front: bool,
    turn: Option<TurnFrame>,
    page_turn: PageTurn,
    turn_over: std::time::Duration,
    gap: f32,
    fixed_layout: bool,
    layer_top: f32,
    family: &'a SharedString,
    page_percent: SharedString,
    foreground: Hsla,
    link: Hsla,
    palette: &'a Palette,
    image_pans: &'a HashMap<String, (f32, f32)>,
    viewer: Option<Entity<ImageViewer>>,
    reader_weak: WeakEntity<ReaderSession>,
    image_hits: Rc<RefCell<Vec<ImageHit>>>,
    image_hits_frame: u64,
    stack_hits: Rc<RefCell<Vec<StackHit>>>,
    speech_span: Option<(usize, Range<usize>)>,
    atlas: Entity<ImageAtlas>,
    view: EntityId,
    speaking: bool,
    long_press: bool,
}

impl RenderFrame<'_> {
    /// 一页几栏。双页在装不下两栏时会自己退回一栏（`PageMode::columns`）。
    fn columns(&self) -> usize {
        self.mode.columns()
    }

    /// 这一档是不是按「页」画读数带：连续滚动没有页，带子是浮在正文上的占位。
    fn paper_bands(&self) -> bool {
        !self.mode.is_continuous()
    }

    /// 窗口尺寸。纸就是窗口矩形（定版式的 `Paper::fixed` 也这么填），所以纸宽高
    /// 与窗口是同一个数，不必各存一份。
    fn viewport(&self) -> (f32, f32) {
        (self.page_box.paper.w, self.page_box.paper.h)
    }

    fn viewport_width(&self) -> f32 {
        self.page_box.paper.w
    }

    fn window_height(&self) -> f32 {
        self.page_box.paper.h
    }

    /// 翻页几何用的那一块：连续滚动是正文版心，其余是整张纸。
    fn page_area(&self) -> turn_animation::Area {
        let rect = if self.mode.is_continuous() {
            self.page_box.content
        } else {
            self.page_box.paper
        };
        turn_animation::Area::new(rect.x, rect.y, rect.right(), rect.bottom())
    }

    /// 整张纸：从窗口顶铺到窗口底。仿真折页揭的就是这一整张。
    fn sheet_area(&self) -> turn_animation::Area {
        sheet_area_in_page(self.viewport())
    }

    /// 正文那一块（页眉带下面那一条），翻页时按它归一。
    fn area(&self) -> turn_animation::Area {
        turn_animation::Area::new(
            0.0,
            self.content_top,
            self.viewport_width(),
            self.content_top + self.viewport_height,
        )
    }

    /// 这一帧是不是仿真折页（含松手那一腿）。
    fn curl_turn(&self) -> bool {
        self.turn.map_or(self.page_turn, |frame| frame.style) == PageTurn::Curl
    }
}

/// 这一帧画出来的非纸面产物：整页画、读数带旗、读屏文本、图集账与章界落点。
///
/// 页面树本身（字形元素、装饰、图片、纸层）仍由 `body` 的局部变量装着：它们要按
/// 「一张纸」分组，装进同一个结构体反而切不开借用。两臂写同一份 `FrameOut`。
struct FrameOut {
    page_art: Option<PageArt>,
    page_picture: bool,
    bare_bands: bool,
    neighbour_backdrop: bool,
    bands_drawn: bool,
    spoken: Vec<SharedString>,
    painted_keys: Vec<String>,
    upcoming: Vec<crate::ui::foundation::images::DecodeRequest>,
    prefetch_side: Option<PrefetchSide>,
    land_chapter: Option<PageRef>,
    fixed_visual: Option<SelectionVisual>,
}

impl FrameOut {
    fn new() -> Self {
        Self {
            page_art: None,
            page_picture: false,
            bare_bands: false,
            neighbour_backdrop: false,
            bands_drawn: false,
            spoken: Vec::new(),
            painted_keys: Vec::new(),
            upcoming: Vec::new(),
            prefetch_side: None,
            land_chapter: None,
            fixed_visual: None,
        }
    }
}

/// 连续滚动这一臂要交回公共装配的产物。
struct ContinuousArm<'a> {
    backdrops: &'a mut Vec<AnyElement>,
    out: &'a mut FrameOut,
}

/// 分页 / 双页这一臂要交回公共装配的产物。
struct PagedArm<'a> {
    backdrops: &'a mut Vec<AnyElement>,
    paint: &'a mut TurnPaint,
    paper_layers: &'a mut Vec<AnyElement>,
    /// 每页自己的读数带（页眉 / 页脚）：挂在正文裁切层外面，静止可见、翻页跟着纸走。
    page_bands: &'a mut Vec<AnyElement>,
    out: &'a mut FrameOut,
}

impl Render for ReaderSession {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::ui::features::reader::model::perf::reset();
        // 这一帧真摆上几张定版式纸：翻页那一趟旧页走纹理，只该剩一张。
        self.fixed_sheets_drawn = 0;
        // 上一帧的绘制回调确认页框完整落在窗口内、并且真的给捕获 pass 排了请求，才把
        // 「已烘」落定。必须放在 `sync_prefs` / 尺寸比较之前：那一帧之后发生的偏好或
        // 尺寸变化要继续把这张纹理标脏，不能被这里晚到的提交覆盖掉。
        if let Some((key, size)) = self.capture_committed.take() {
            self.captured_page = Some(key);
            self.captured_size = Some(size);
            self.capture_dirty = false;
        }
        // 背面那张同口径：绘制回调真把 leaf 那一块交给背面捕获 pass 了，才算烘好。
        if let Some(back) = self.capture_back_committed.take() {
            self.captured_back = Some(back);
        }
        // 进场页那张同口径：绘制回调真把整张纸那一块交给捕获 pass（第 3 层）了，才算烘好。
        // 下一帧起进场页整页跳过、由平摊网格采这张纹理。
        if let Some(incoming) = self.capture_incoming_committed.take() {
            self.captured_incoming = Some(incoming);
        }
        // 纸张几何变过的那一笔要**在收完提交之后**并进脏标记：阅读页报告带子高度/可见性
        // 发生在这一帧的子视图之前，上一帧那份「烘好了」的提交盖不掉它。
        if std::mem::take(&mut self.capture_box_dirty) {
            self.capture_dirty = true;
            self.captured_back = None;
            self.captured_incoming = None;
        }
        // 「会话出了元素树」与「正文那一层真画了」是两件事，分开数才分得清帧间隔归谁。
        crate::ui::features::reader::model::perf::note_render();
        let __render = crate::ui::features::reader::model::perf::Timer::start();
        // 配置同步：比对一次 `ConfigHolder`，不一致就 `set_prefs_on_all`（逐章作废行单元、
        // shaping 表与排版输入）。改字号那一帧的钱全在这一格里，而它从前没人看。
        let __t = crate::ui::features::reader::model::perf::Timer::start();
        self.sync_prefs(cx);
        let __sync = __t.ns();
        crate::ui::features::reader::model::perf::add(|frame| frame.sync_ns += __sync);
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
        // 有效视口与正文原点在下面由 [`PageBox`] 一次算出来：可见行筛选、滚动上限、
        // `on_scroll` 的夹范围都读它。浮层（顶栏/底栏/面板）压在正文**之上**，不参与
        // 这个数：点一下屏幕字不该跳走。
        //
        // 翻页动画这一帧的时钟。必须在进 `match` 之前采：`Presence::sample` 要
        // `&mut Window` + `&mut App`，而 `match &mut self.source` 进去之后 `self` 就只能
        // 逐字段借了（见下面那条同样的取舍）。采完给回的是一个 `Copy` 的快照，两臂各读各的。
        let turn = self.turn_frame(window, cx);
        // 章界那一趟起时要抄的档位与时长。在 `self` 被拆成字段借之前采好：`begin_turn` 那种
        // 要整块 `&mut self` 的方法在下面那臂里调不动。
        let page_turn = ConfigHolder::appearance(cx).page_turn;
        let turn_over = turn_duration(cx);
        let gap = chrome::spread_gap(self.prefs.margin_x);
        // 退档判据与排版必须量同一个版心宽，所以用 `Document` 的那个式子算，不在这里另定一套。
        let usable = Document::content_width(window, &self.prefs, self.reserve.width);
        // 摆法这一帧有两个来源，按顺序解：读者为这本书选过档就用他那一档，没选过
        // （第一次读、或读了但从没换过档）才按宽度给默认档；然后「翻页方式」里那一档
        // 有**否决权** —— 选了「上下滚动」就必须是连续，选了任何一张纸的走法就不该还
        // 站在连续里。这条耦合放在渲染帧而不是写配置那一头，是因为设置屏改的是全局
        // `appearance.page_turn`、而摆法按书存在会话里，两头隔着一条边界
        // （判据本身在 `chrome::reconcile`）。
        //
        // 从前这里有一条 `!is_compact()` 的闸：手机档不参与按宽度重算，因为装不下两栏时
        // 默认档就是连续滚动 —— 那正是「在手机上选了仿真翻页却什么都不变」的成因。
        // 默认档不再回连续之后，那条闸要的东西没有了。
        //
        // 换档统一走 `adopt_mode`：它把「第几个字」从旧摆法的坐标搬到新摆法里。
        // 这里直接赋 `self.mode` 会让连续 → 分页那一趟丢掉章内位置 —— 读者在
        // 第 40% 处换回纸面动画，落到的却是这一章的章首。
        // 定版式没有「两栏」这回事：它的页是既定的一张位图，按宽度分成两条窄柱子只会
        // 把同一页砍成两半。所以这一族的默认档恒为整页翻；读者手动塞进来的双页也按
        // 相邻两页各占半张来摆（`fixed::fixed_slots`），不是把一页排成两栏。
        // 「上下滚动」照旧由 `reconcile` 带进连续。
        let fixed_layout = self.is_fixed();
        let by_width = if fixed_layout {
            chrome::PageMode::Paged
        } else {
            chrome::default_mode(usable, gap)
        };
        let wanted = if self.mode_undecided {
            by_width
        } else {
            self.mode
        };
        self.adopt_mode(chrome::reconcile(page_turn, wanted, by_width), cx);
        let mode = self.mode.effective(usable, gap);
        let columns = mode.columns();

        // 一页的屏幕几何：纸 / 安全区 / 两条带 / 正文。三种摆法与定版式共用这一份矩形，
        // 页面不再各自拼 `content_top` / `viewport_height` 的第二套原点。
        //
        // `left/right` 眼下给 0：桌面侧栏是压在正文上的浮层（`reserve.width` 实际为 0），
        // 横屏挖孔的左右让位等横屏那一轮再从这里开口。竖直方向的 insets 必须与
        // `ReadingPage` 报上来的 `top_inset` / `bottom_inset` 是同一对数。
        let page_box = PageBox::new(
            PageRect::new(
                0.0,
                0.0,
                f32::from(viewport_size.width),
                f32::from(viewport_size.height),
            ),
            PageInsets {
                top: self.insets.top,
                bottom: self.insets.bottom,
                left: 0.0,
                right: 0.0,
            },
            self.bands.header,
            self.bands.footer,
            self.prefs.margin_x,
            self.prefs.max_measure,
            columns,
            gap,
            if fixed_layout {
                BandPolicy::Overlay
            } else {
                BandPolicy::Reserved
            },
        );

        // 存一份给会话：命中、选区、`content_top()` 的替代都读它，不必每处再拼一次。
        self.page_box = page_box;

        // 那一页纸占的矩形，**元素坐标**（与字形的 `run.x` / `run.baseline` 同一把尺）：
        // 折线、逐字裁剪、折面多边形、网格的页框、手指坐标的归一化，全算在这一套里。
        //
        // 只有这一处定义它。从前捕获纹理按 `content_bounds`（字形元素自己的那块）烘、折线按
        // 这一块摆、手指按 `content_bounds` 归一 —— 三个数差着页边距，卷起来那一片采到的是
        // 横移过的页面，读成「纸上的字与底下那半页对不齐」。
        // 两种内容都从窗口 `(0,0)` 起：状态栏/手势条是**纸内**的安全区，正文与读数带
        // 在页内让开它，纸（底色/整页图）铺到它上面。
        //
        // 可重排的正文原点与页高从 [`PageBox`] 来；定版式的页图就是整张纸
        // （`content == paper`），所以原点 0、页高 = 窗高，两条带只是浮在图上。
        let content_top = if fixed_layout {
            0.0
        } else {
            page_box.content.y
        };
        let viewport_height = if fixed_layout {
            f32::from(viewport_size.height)
        } else {
            page_box
                .metrics()
                .map_or(page_box.content.h, |metrics| metrics.content_height)
        };
        // 定版式不再由外壳把整层推下去：页图要铺到状态栏与手势条下面。
        let layer_top = 0.0;
        let area = turn_animation::Area::new(
            0.0,
            content_top,
            f32::from(viewport_size.width),
            content_top + viewport_height,
        );
        // 整张纸：**这一块阅读盒的全部**。仿真折页揭的是「一整张纸」而不是「纸上的正文」——
        // 页眉那行章名、页脚那行页码、上下两条页边距，以及**系统状态栏与手势条那两条**都在
        // 同一张纸上。顶边是**窗口坐标的 0**，底边是窗口底（手势条那一条由外壳的 padding
        // 让开，不进这块盒）；文字仍然排在安全区以内，这一格只管纸面。其余几档的纸仍是正文
        // 那一块（它们的位移/覆盖只跟正文走，带子不动）。
        //
        // **不许**把两条系统边 / 两条带子的高再加回来：它们已经在 `PageBox` 里各算过一次，加回来纸就比窗口高，捕获矩形会被 [`in_viewport`]
        // 每帧拒掉（`curl_capture_ready()` 因此永远为假，点击就什么都不发生）。
        let sheet_area = sheet_area_in_page((
            f32::from(viewport_size.width),
            f32::from(viewport_size.height),
        ));

        // 仿真翻页的 GPU 网格采样的是一张**旧页纹理**。页号/章号变了、或偏好重排过
        // 才重烘；同一页的静止帧不再每帧跑一整趟捕获 pass。动画期间不烘焙 ——
        // 那时场景里已经多了一张新页，烘出来的就不是正在被翻的那张。
        //
        // 这里只决定「这一帧要不要烘」。真正发请求要等到那块 canvas 的绘制回调（见下面
        // `capture_region` 那一格）：只有在那里才知道这一栏在窗口里的真实原点，而那个原点
        // 必须与网格取 uv 用的那一块是同一个数。
        //
        // 还有一条条件不在这里的「页号」里：**这一章的排版得已经落地**。进书第一帧的排版
        // 是这一帧才丢进后台队列的（`ensure_viewport`），当帧元素树里一个字都没有；那时
        // 烘下来的是一张纸色纹理，而它一旦被记成「已烘」，排版回来既不动 `captured_page`
        // 也不置 `capture_dirty`（`apply_typeset` 只管行盒与分片），于是这张白纸会一直
        // 留着 —— 真机上第一次仿真翻页整页白，翻完一页页号变了才自己好。判据在
        // [`capture_wanted`]。
        let page_key = (self.chapter_index(), self.page);
        // 窗口尺寸变了 ⇒ 那张纹理作废：它里面被写过的只有**当时**那一块，而网格按今天的窗口归一
        // 取 uv，差出来的那一截采到没被写过的地方（读成黑）。页号在这时候不动，所以它进不了
        // `page_key`，只能这样单独比一次。
        let viewport = (
            f32::from(viewport_size.width),
            f32::from(viewport_size.height),
        );
        if self.captured_size != Some(viewport) {
            self.capture_dirty = true;
        }
        // 版心 / 偏好 / 摆法变了 ⇒ 背面那张一起作废。它的键里只有「章·页·栏」，拦不住重排：
        // 字号一改，同一栏里已经是另一批字了。
        if self.capture_dirty {
            self.captured_back = None;
            self.captured_incoming = None;
        }
        // 定版式那一族没有排版可等（页是既定的位图），但它有另一条「这一帧烘出来是不是
        // 空的」：这一页每一格该画的像素都得先在图集里落地。少一格就烘，纹理里那一块
        // 是纸色，而它一旦被记成「已烘」就不会自己重来。双页那一趟烘的是**两张**页图，
        // 右页也得一起等。
        //
        // 这一问要逐格查图集，**只在真可能烘的那一帧**才问：动画里、连续滚动里、
        // 或手里这张纹理本来就是这一页的，`capture_wanted` 都是假，值不必算。
        let capture_stale = self.capture_dirty || self.captured_page != Some(page_key);
        let content_ready = if !fixed_layout {
            self.book().is_none_or(|book| book.document().is_current())
        } else if turn.is_some() || mode.is_continuous() || !capture_stale {
            true
        } else {
            let left = self.page();
            let count = self.page_count();
            let right_ready = mode != chrome::PageMode::Spread
                || left + 1 >= count
                || self
                    .fixed()
                    .is_some_and(|book| fixed::fixed_capture_ready(book, left + 1, &atlas, cx));
            right_ready
                && self
                    .fixed()
                    .is_some_and(|book| fixed::fixed_capture_ready(book, left, &atlas, cx))
        };
        // 进场那 280 ms 里整层在横移：副本按**最终位置**画、上屏那一份按 `+enter_shift` 画
        // （见下面 `capture_region` 那一段）。两条路径画的是同一页字，**只有副本落进主 pass**
        // 才会错开一截印两遍，所以「进场期间不烘」那是临时闸，不是根因的修法：层壳
        // （[`capture_layer`]）实测已经把副本的 primitive 全排进 `CAPTURE_ORDER_BASE` 之上
        // （quad `order = 10010000xx`、字形 `capture_layer() == 1`），主 pass 取不到它。
        // 闸拆掉，进场这一段照常烘，读者一进书就翻页不必再多等一帧。
        let capture_needed = capture_wanted(
            turn.is_some(),
            mode.is_continuous(),
            content_ready,
            self.capture_dirty,
            self.captured_page == Some(page_key),
        ) && (!fixed_layout || !self.fixed_zoom.is_zoomed());
        // 捕获层 1（正面）这一帧要不要画一份副本。两条臂都要：定版式的页图也是
        // 一份能烘进纹理的位图，折页与平摊三档采的就是它。连续滚动那一档不烘
        // （上面那条已经挡掉）。副本必须自己画一份 —— 主 pass 只画
        // `order < CAPTURE_ORDER_BASE`，层里的东西不上屏。
        let capture_front = capture_needed;

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
        // 批注划线只有那五种（`ANNOTATION_COLORS`），一次取好，逐条画时查表。
        let palette = Palette::of(cx.theme());
        let family = self.family.clone();
        // 这一帧的「可拖动插图」命中区：每帧重填（`window.on_mouse_event` 只活到本帧，
        // 命中区也只能本帧现算，与选区那几条同一规矩）。偏移表是**快照**：它在下面
        // `match &mut self.source` 那一段里借不出 `self`。
        let image_hits = self.image_hits.clone();
        let stack_hits = self.stack_hits.clone();
        self.image_hits_frame = self.image_hits_frame.wrapping_add(1);
        let image_hits_frame = self.image_hits_frame;
        // 不整表清空（理由见 `ImageHit::frame`）：只把过期的那几条丢掉。
        let fresh = image_hits_frame.saturating_sub(2);
        image_hits.borrow_mut().retain(|hit| hit.frame >= fresh);
        let image_pans = self.image_pans.clone();
        // 听书正在念的那一段（哪一章的哪个字符区间）。要在 `font_cache` 的可变借用
        // 与 `match &mut self.source` 之前解出来：进了那两道门 `self` 都借不出，
        // 而 `speech_range()` 是个 `&self` 方法。
        let speech_span = self.speech_range();
        // 这一帧读者还在不在快速滑动、往哪边滑。只喂给「排哪一档」与预取方向：
        // 滑动中只铺 720 预览，停下那一帧才排高清。判据在 `ReaderSession::mark_scroll`。
        //
        // 放在 `font_cache` 那个可变借用**之前**：进了 `match &mut self.source` 与
        // 那几个 `&mut self.xxx` 之后 `self` 整体借不出，`&mut self` 的方法就调不了。
        let now = std::time::Instant::now();
        let motion = self.mark_scroll(now, viewport_height);
        // 图集的内存档位要按设备分（手机 24 MB / 平板 48 / 桌面 128），而设备那一格
        // 只有窗口量得出来：每帧推一次逻辑宽度，档位随窗口变化也跟着走。
        crate::ui::features::reader::model::image_atlas::set_screen_width(f32::from(
            viewport_size.width,
        ));
        // 两章之间的缝：拼接坐标用它隔开相邻的章（口径在 `Book::chapter_gap`）。
        let chapter_gap = Book::chapter_gap(&self.prefs);
        // `FontKey → Font` 的解析每帧对每条可见 run 都要一次，交给会话持有的缓存
        // （避免逐 run 扫设备字体表）。body / mono / faces / 设备表变了由会话清或重建。
        // 读数带在 `font_cache` 那句之前算：那一句把 `self` 可变借走，之后再借整份
        // 会话就过不去（进度要读整本书）。每页的章名/页号在循环里按 page_ref 算，
        // 跨章翻页时左右两页才各念各的。
        let page_percent: SharedString = chrome::percent_label(self.progress()).into();
        // 页面底色放在 `column` 自己身上（父背景先于所有子节点画）：字形元素铺满
        // 整个版心，若把不透明背景挂在它身上、它又在图片之后才入树，白底会把先画的
        // 图片整个盖住 —— 封面章整页空白的根因（gpui 的 z-order 是「相交即 +1，
        // 后画者居上」，见 plans/facts.md）。层级从此固定为：页底 → 块级装饰 →
        // 字形 → 图片。
        //
        // 底色在**装配那一步**才挂（见函数末尾）：整页画那一档要把这层底色让开，
        // 让窗口背板透上来。
        let mut element = GlyphsElement::new();
        let mut column = div().size_full().relative();
        // 这一帧的非纸面产物（整页画、读数带旗、读屏文本、图集账、章界落点）。
        // 邻章那张整窗背板也记在里面：纸底色若照旧挂在 `column` 上会把它整个盖住
        // （2026-11-02 真机《哈利·波特全集》`v1.xhtml`：视口顶还在目录，封面已经在屏上）。
        let mut frame_out = FrameOut::new();
        // 图片永远压在字形之上：收集起来，等 `element` 入树之后再挂。
        let mut overlays: Vec<AnyElement> = Vec::new();
        // 整页画（`background-attachment: fixed` 底图章 / 单图页）的背板元素。
        // 背板挂在 `column` **之前**、`body` 之下 —— 压着页底色、让正文与读数带浮在上面；
        // `body` 自己不再裁切，所以背板可以向上铺到安全区里（见函数末尾的装配）。
        let mut backdrops: Vec<AnyElement> = Vec::new();
        // 这一档这一帧的翻页产物（折面投影、卷曲网格、三份捕获副本与目标页）。
        // 两臂写同一份，装配那一段只读它。
        let mut turn_paint = TurnPaint::new();
        // 「点这张图」要写出去的那个状态，由阅读页持有（见 `image_viewer`）。没挂上的
        // 会话（单独的渲染测试）图片就不可点。
        let viewer = self.image_viewer.clone();
        // 定版式那一页用什么手势放大：按得住长按的那一端用长按（单击留给翻页），
        // 按不住的那一端（桌面鼠标）用单击图内，翻页让给页外的留白。
        let long_press = crate::ui::host::supports_long_press(cx);
        // 读屏（TalkBack 那一类）连着时才收集「这一屏念得出来的字」：一屏几十行、每行
        // 一次分配，压在画字那条最热的路径上就是每帧的白花钱。gpui 在读屏接上的那一帧
        // 会强制重画一次，所以这一格不需要自己盯变化（`Window::is_a11y_active` 的文档写的
        // 就是这套）。收集到的那份在树根那一头交给 `a11y_synthetic_children`。
        let speaking = window.is_a11y_active();
        let mut paper_layers: Vec<AnyElement> = Vec::new();
        // 每页一份的读数带：与纸同一套位移，但不进正文裁切层。
        let mut page_bands: Vec<AnyElement> = Vec::new();
        let frame = RenderFrame {
            mode,
            viewport_height,
            content_top,
            chapter_gap,
            page_box,
            page_key,
            capture_front,
            turn,
            page_turn,
            turn_over,
            gap,
            fixed_layout,
            layer_top,
            family: &family,
            page_percent: page_percent.clone(),
            foreground,
            link,
            palette: &palette,
            image_pans: &image_pans,
            viewer: viewer.clone(),
            reader_weak: reader_weak.clone(),
            image_hits: image_hits.clone(),
            image_hits_frame,
            stack_hits: stack_hits.clone(),
            speech_span: speech_span.clone(),
            atlas: atlas.clone(),
            view,
            speaking,
            long_press,
        };
        // 先把 source 取到局部：臂里要调 `&mut self` 的方法（样本臂），而
        // `match &mut self.source` 会把 `self.source` 整段借走。放回去在 match 之后，
        // 公共装配读 `self.source` 之前。
        let mut source = std::mem::replace(&mut self.source, Source::Empty);
        match &mut source {
            Source::Empty => {}
            Source::Fixed(book) => {
                self.render_fixed(
                    book,
                    fixed::FixedArm {
                        frame: &frame,
                        usable,
                        motion,
                        now,
                        paper_layers: &mut paper_layers,
                        paint: &mut turn_paint,
                        out: &mut frame_out,
                    },
                    &mut overlays,
                    cx,
                );
            }
            Source::Book(book) => {
                if mode.is_continuous() {
                    self.render_continuous(
                        book,
                        frame,
                        &mut element,
                        &mut column,
                        &mut overlays,
                        ContinuousArm {
                            backdrops: &mut backdrops,
                            out: &mut frame_out,
                        },
                        window,
                        cx,
                    );
                } else {
                    self.render_paged(
                        book,
                        frame,
                        &mut element,
                        &mut overlays,
                        PagedArm {
                            backdrops: &mut backdrops,
                            paint: &mut turn_paint,
                            paper_layers: &mut paper_layers,
                            page_bands: &mut page_bands,
                            out: &mut frame_out,
                        },
                        window,
                        cx,
                    );
                }
            }
        }
        self.source = source;
        // 两臂都把产物写进 `turn_paint`；装配这一段仍按原来的名字读，省得在
        // 一百多行几何代码里逐处改字段路径。
        let TurnPaint {
            shades,
            mesh: gpu_curl_mesh,
            leaf: curl_leaf,
            front: paged_front_capture,
            back: mut paged_back_capture,
            incoming: paged_incoming_capture,
            back_dx,
            back_key,
            incoming_bake,
            mesh_page_drawn,
            arriving: turn_arriving,
        } = turn_paint;
        let FrameOut {
            page_art,
            page_picture,
            bare_bands,
            neighbour_backdrop,
            bands_drawn,
            spoken,
            painted_keys,
            mut upcoming,
            prefetch_side,
            land_chapter,
            fixed_visual,
        } = frame_out;
        column = column.children(paper_layers);
        // 单页仿真折页时，卷起来的是**整张纸**（含页眉/页脚），而纸底下露出来的是目标那一页。
        // 所以主场景里的两条读数带要念目标页；它们必须在卷曲网格**之前**入树，网格才盖得住
        // 卷着的那张纸，而读者又能从纸底下看见目标页的章名与页码。
        let whole_sheet_curl = gpu_curl_mesh.is_some() && mode != chrome::PageMode::Spread;
        let band_bare = bare_bands || page_box.bands == BandPolicy::Overlay;
        let band_readouts = if whole_sheet_curl {
            turn_arriving.map_or_else(
                || bands::readouts_of(self, cx),
                |at| bands::readouts_at(self, at.chapter, at.page, cx),
            )
        } else {
            bands::readouts_of(self, cx)
        };
        // 整张纸折页那一档的带子由这里画（网格盖得住）；定版式自己有每页一份的
        // live 带子（`bands_drawn`），不再叠这一份。
        if whole_sheet_curl && !bands_drawn {
            column = column.child(bands::chrome(
                &page_box,
                &band_readouts,
                band_bare,
                bands::Width::Paper,
                cx,
            ));
        }
        // 选区挂在它自己的章上：那一章被裁出拼接窗口（滚远了）就一并丢 ——
        // 高亮、把手、复制都再没有数据源了。
        //
        // 这一问**只属于可重排那一族**：定版式没有 `book()`（字在 `fixed()` 那一臂里），
        // 用 `is_none_or` 判它等于「定版式每一帧都把刚成立的选区清掉」—— 拖选的手感
        // 于是变成一个都抓不住的空动作。那一族的「翻走即清」按**页号**判，写在上面
        // 定版式那一臂里（一屏就是一页，不必问拼接窗口）。
        if let Some(selected) = &self.selection
            && let Some(book) = self.book()
            && book.span_doc(selected.chapter).is_none()
        {
            self.selection = None;
            self.selection_menu_open = false;
            // 选区没有数据源了，笔记便签也就没有可锚的东西：别把一张能打字、存不进去的
            // 便签留在屏上。
            self.note_editor = None;
        }
        // 选中件（M6）：把「选区 ∩ 可见范围」换成元素局部矩形喂给字形元素
        // （高亮压在字下、把手压在字上、手势监听随 paint 注册），两端坐标留给菜单条。
        // 排版还没跟上（换章/改字号在飞）的那一帧不画：行盒是旧的，矩形会指错字。
        // 定版式的高亮算在页那一臂里，菜单条却和可重排共用后面这段摆位；不把它接回来，
        // `visual.highlights` 永远是空的，`selection::slot` 判 Hidden —— 屏上选区在、
        // 菜单不出现（手机 PDF 长按的症状）。可重排那一臂仍是空的起手，下面自己填。
        let selection = self.selection_frame(
            self.book(),
            window,
            cx,
            SelectionGeom {
                mode,
                columns,
                usable,
                gap,
                page_shift: self.page_shift,
                content_top,
                viewport_height,
                chapter_gap,
            },
            fixed_visual,
        );
        let visual = selection.visual;
        let menu_anchor = selection.anchor;
        let menu_x = selection.x;
        // 外壳进场那 280 ms 的横移：捕获层里的副本按 `-enter_shift` 画在**最终位置**上，捕获
        // 矩形也按最终位置取（[`capture_region`]）。两处必须同一个数 —— 副本画在最终位置而矩形
        // 按当下位置取，烘出来的就是错位的一页。
        let enter_shift = self.enter_shift;
        // 背面那张纹理是目标页：它的读数带也念目标页。只有整张纸那一档把带子卷进去；
        // 双页的 leaf 只盖半张纸，带子留在主场景里不动。
        if whole_sheet_curl
            && back_key.is_some()
            && curl_leaf.is_some()
            && let Some(capture) = paged_back_capture.as_mut()
        {
            capture.bands = Some(bands::chrome(
                &page_box,
                &band_readouts,
                bare_bands,
                bands::Width::Paper,
                cx,
            ));
        }
        // 只有这一帧真产出了副本才请求捕获。副本那一趟可能因为某一格/某张图还没
        // 进图集而空着（`painted_tile` / `painted_picture` 那两条早退），请求了就会
        // 把「烘好」记在一张缺内容（甚至纸色）的纹理上 —— 而门再也不会自己重开。
        let front_copy_ready = paged_front_capture.is_some();
        let incoming_copy_ready = paged_incoming_capture.is_some();
        let back_copy_ready = paged_back_capture.is_some();
        let mut capture_layers = Vec::new();
        if let Some(capture) = paged_front_capture {
            capture_layers.push(turn_view::CaptureLayer {
                order: 1,
                dx: -enter_shift,
                capture,
            });
        }
        if back_key.is_some()
            && curl_leaf.is_some()
            && let Some(capture) = paged_back_capture
        {
            capture_layers.push(turn_view::CaptureLayer {
                order: 2,
                dx: back_dx - enter_shift,
                capture,
            });
        }
        // 捕获层 3（进场页）：副本里的 runs / 装饰 / 图都已带着进场页这一帧的位移，
        // 包裹 div 按 `-offset` 把它退回静止位置、再按 `-enter_shift` 落到最终位置 ——
        // 与正面副本同一把尺（正面烘焙时位移本来就是 0，所以只让横移）。
        if incoming_bake.is_some()
            && let Some(capture) = paged_incoming_capture
        {
            capture_layers.push(turn_view::CaptureLayer {
                order: 3,
                dx: -enter_shift,
                capture,
            });
        }
        column = turn_view::mount_capture_layers(column, capture_layers);
        element = element.selection(visual);
        if matches!(self.source, Source::Book(_)) {
            element = element.reader(Some(cx.entity().downgrade()));
        }
        // 正文硬裁到 `PageBox.content`：页眉/页脚是带子，不是压在字上的第二层。
        // 外层给裁切矩形，内层把元素原点还回窗口坐标 —— 字形 run 用的是窗口坐标，
        // 多包一层不能让它整体平移一个 `content`。定版式 `content == paper`，
        // 这一层对它等于没裁。
        //
        // 翻页期间这一面纸整体在动：正文跟着纸走，裁剪就该是纸（整窗），页眉/页脚那两条
        // 带子随纸移动、自己遮住边缘。裁剪钉在静止版心上时，纸在动而字的可见范围不动，
        // 上下平移会在旧版心的上下边界切掉一条，切掉的部分等纹理接管又补回来 —— 闪。
        // 连续滚动的字本来就该被视口带子夹住，定版式的 `content == paper`，两者保持原样。
        let clip =
            if turn.is_some() && !mode.is_continuous() && page_box.bands == BandPolicy::Reserved {
                page_box.paper
            } else {
                page_box.content
            };
        column = column.child(
            div()
                .id("reader-content-clip")
                .test_support()
                .absolute()
                .left(px(clip.x))
                .top(px(clip.y))
                .w(px(clip.w))
                .h(px(clip.h))
                .overflow_hidden()
                .child(
                    div()
                        .absolute()
                        .left(px(-clip.x))
                        .top(px(-clip.y))
                        .w(px(f32::from(viewport_size.width)))
                        .h(px(f32::from(viewport_size.height)))
                        .child(element),
                ),
        );
        column = turn_view::paint_shades(
            column,
            shades,
            gpu_curl_mesh.is_some(),
            (self.enter_shift, layer_top),
            cx.theme().background,
        );
        // 图片最后入树：绝对定位不参与布局，却要压住上面所有层级。
        for image in overlays {
            column = column.child(image);
        }
        // 这里**不画**双页的中缝暗部。原先那一格是两条各 `gap × 0.9` 宽、最深处 14% 黑的渐变带
        // 贴在中线两侧（页边距默认 24 时每条 21.6 逻辑像素），压在正文与图片之上。撤掉它的理由
        // 不是难看：翻页的那张纸要越过中线落到另一半页上，而这条带子永远摆在最上层（只有卷曲
        // 网格压得过它），于是读者读到「纸推到中间那道阴影就停了」。两页的分界本来就由中缝那段
        // 留白给（[`chrome::spread_gap`]，下限 `MIN_SPREAD_GAP`），纸色一致、不需要额外的暗部。
        // 捕获区清屏色必须取纸张底色；闭包里的 `window` 拿不到主题，所以先取好。卷曲网格取 uv
        // 落在捕获矩形外半像素时，透明黑就是页边那条黑带的来源（参照实现的
        // `glClearColor(mBackgroundColor)` 用同一个口径）。
        let capture_clear = cx.theme().background.to_rgb();
        // 三块捕获纹理与网格共用这一份坐标口径：`layer` 是这一层自己的平移，
        // `viewport` 用来拒掉出窗的矩形，`clear` 必须是纸底色。
        let pass = turn_view::CapturePass {
            layer: (self.enter_shift, layer_top),
            viewport,
            clear: capture_clear,
        };
        // 烘一张旧页纹理（捕获层 1）：整张纸（含页眉/页脚），只在定住那一帧请求。
        if capture_needed && front_copy_ready {
            column = turn_view::request_front_capture(
                column,
                sheet_area,
                page_key,
                Rc::clone(&self.capture_committed),
                Rc::clone(&self.curl_bake_origin),
                pass,
            );
        }
        // 背面那张纹理（捕获层 2）：内容与请求排在同一帧，网格这一帧就采得到。
        if let Some(key) = back_key
            && let Some(leaf) = curl_leaf
            && back_copy_ready
        {
            column = turn_view::request_back_capture(
                column,
                leaf,
                key,
                back_dx,
                Rc::clone(&self.capture_back_committed),
                pass,
            );
        }
        // 进场页那张纹理（捕获层 3）：烘焙那一帧与副本同一帧发出请求。
        if let Some((key, _)) = incoming_bake
            && incoming_copy_ready
        {
            column = turn_view::request_incoming_capture(
                column,
                sheet_area,
                key,
                Rc::clone(&self.capture_incoming_committed),
                Rc::clone(&self.incoming_bake_origin),
                pass,
            );
        }
        // 原始触摸流那一格：折页要的是手指此刻的二维位置，而识别器折出来的事件只剩一根轴。
        column = column.child(touch_follow(reader_weak.clone()));
        // 定版式才挂双指缩放与放大之后的平移：可重排那一族的字号是排版算的，缩放没有意义。
        if self.is_fixed() {
            column = column.child(fixed_gestures(reader_weak.clone()));
        }
        // C 方案的卷曲网格：盖在新页与它的图片之上，UI 浮层之下。旧页的字、图、
        // 背景都在捕获纹理里，网格一次画完；这里只把窗口坐标补齐。
        if let Some(mesh) = gpu_curl_mesh {
            column = turn_view::paint_curl_mesh(
                column,
                mesh,
                area,
                curl_leaf,
                Rc::clone(&self.curl_bake_origin),
                pass,
            );
        }
        // 每页一份的读数带压在纸与网格之上、菜单与便签之下。它不在正文裁切层里，
        // 所以静止帧也看得见；每一份已经按自己那张纸的 `Sheet::offset` 摆好。
        if !page_bands.is_empty() {
            column = column.child(
                div()
                    .id("reader-page-bands")
                    .test_support()
                    .absolute()
                    .inset_0()
                    .children(page_bands),
            );
        }
        // 预解下一次翻页要用的那几张图。**必须在 `end_frame` 之前**：那一趟按「离场满几帧」
        // 放格子，而预解进来的那一格这一帧还算不上「在场」—— 晚一步，它落地后的第一帧
        // 就被收走了。
        //
        // 两档分开排。**低清在动画期间也排**：它有自己的预算
        // （`image_atlas::preview_limit`，与高清那 24 MB 各记各的账），不会把读者眼前
        // 那几张挤出去，而它正是「翻过去那一页不是白的」那一半。
        //
        // **高清仍然只在静止帧排**：动画期间场景里已经多了一张纸，此时排的是正在被翻走/
        // 翻来的那一页，而它们下一帧本来就上屏；两趟解压在 24 MB 的封顶下还会把读者眼前
        // 那几张挤出去（这一条理由从 2026-10-28 起就写在这里，封顶分档之后它对高清仍然成立）。
        // 叠图里「下一张」先预热（见 `Scene::upcoming`）：与下面那条翻页预取同一个图集、
        // 同一条 `prepare`，已经在图集里时它什么都不做。这一条决定横滑切图那一瞬卡不卡。
        for request in std::mem::take(&mut upcoming) {
            self.atlas
                .update(cx, |atlas, cx| atlas.prepare(&request, view, cx));
        }
        if let Some(side) = prefetch_side
            && let Some(book) = self.book()
        {
            // 「还没进图集」只能这么知道：`painted` 那一问已经把「在场」记上了。
            let targets = if mode.is_continuous() {
                prefetch::scroll_targets(
                    &book.placements(chapter_gap),
                    self.scroll_painted..(self.scroll_painted + viewport_height),
                    viewport_height,
                    side,
                )
            } else {
                prefetch::paged_targets(book, self.page, self.laid_out.pages, side)
            };
            let picked = prefetch::first_unseen(book, &targets, |href| {
                painted_keys.iter().any(|k| k == href)
            });
            if let Some(request) = picked {
                if let Some(low) = request.preview() {
                    self.atlas
                        .update(cx, |atlas, cx| atlas.prepare(&low, view, cx));
                }
                if turn.is_none() {
                    self.atlas
                        .update(cx, |atlas, cx| atlas.prepare(&request, view, cx));
                }
            }
        }
        // 帧末回收图集。**必须**在这一帧的元素树全部搭完之后：中途放掉的格子会被同一帧
        // 里的 `img` 重新传回去，白花一趟上传（`image_atlas::end_frame` 那两条不动的规矩）。
        //
        // 焦点页是挤出顺序的第一个键（离它最远的先走）：快速滑动时一屏能压住两三页，
        // 按「最久没在场」会把刚滑过去的那一页先扔掉，往回一滑就得重画。
        let focus = Some(self.page);
        self.atlas.update(cx, |atlas, cx| {
            atlas.set_focus(focus);
            atlas.end_frame(window, cx);
        });
        // 选中菜单（M6）：摆位是 `selection::slot` 那条纯判断，元素本体在
        // `selection.rs` 里。浮层点不住的教训见 page.rs 模块注释，所以它是压在正文
        // 之上的真按钮；拖动期间收起，松手即回。
        // 卡片高固定两块（动作条 + 恒在的颜色胶囊），所以不必再看这一帧有没有挑中某条划线。
        let slot = if self.selection_menu_open() {
            selection::slot(
                menu_anchor.0,
                menu_anchor.1,
                f32::from(viewport_size.height),
                selection::card_height(),
            )
        } else {
            selection::Slot::Hidden
        };
        // 没有菜单上屏时必须把上一帧的锚位清掉：`tap_with_selection` 读的就是那份
        // bounds，留着的话「点一下菜单原来那块空地」还算点在菜单上。
        match self.selection_menu(cx, &slot, menu_x) {
            Some(menu) => column = column.child(menu),
            None => self.menu_bounds.set(None),
        }
        // 笔记便签最后挂：它盖着正文、把手与菜单，遮罩上的一击归它自己。
        if let Some(note) = self.note_editor_element(window, cx) {
            column = column.child(note);
        }

        // `render` 这一段结账。收在**整条元素树搭完**之后：从前它收在连续滚动那一臂的
        // 中途，于是选区矩形、菜单条、overlay 装配那一段两头都不算（`total = render + paint`
        // 是恒等式，那段时间就凭空消失了）。gpui 拿这棵树去做 taffy 布局与合成仍然在
        // 这一格**之外** —— 那部分落在 `gap_us` 里，这一层量不到。
        let __total = __render.ns();
        crate::ui::features::reader::model::perf::add(|frame| frame.render_ns = __total);
        // 「不走动画」那一档的章界：目标章排好后当场落章。放在元素树搭完之后、
        // 与 `drain_pending_turn` 同一处、同一个理由 —— 换锚会让同一帧上下半截按两章画。
        if let Some(to) = land_chapter {
            self.finish_chapter_turn(to);
            cx.notify();
        }
        // 进书横移那 280 ms 里被旧页纹理门拒掉的那一趟，到这里再补。放在元素树搭完
        // 之后：`turn()` 会改页号与 `laid_out`，中途调会让同一帧上下半截按两页画。
        self.drain_pending_turn(cx);
        let listener = cx.listener(|this: &mut Self, event: &ScrollEvent, _window, cx| {
            // 挪动了才重画：滚到章尾夹住、分页没攒够半屏时一整帧的重建可以整份省掉
            // （判据与 gpui 的 `div` 滚动容器、zed editor 的 `scroll` 一致）。
            if this.on_scroll(event, cx) {
                cx.notify();
            }
        });
        // 版心底色与裁切都只属于**正文**：整页画那一档把底色让开（窗口背板透上来），
        // 而裁切从 `body` 挪到 `column` 上 —— `body` 不裁，背板才铺得进顶上那条安全区。
        //
        // 让开与否问的是「这一帧画没画背板」，不是「锚章是不是整页画」：邻章那张整窗背板
        // 铺在同一扇窗上，锚章的纸底色压着它就等于没画（`neighbour_backdrop`）。
        if page_art.is_none() && !page_picture && !neighbour_backdrop {
            column = column.bg(cx.theme().background);
        }
        // 页面自己带读数带：实时绘制的那一页走 `page_bands`；出场页走捕获网格时
        // 带子烘在正面捕获纹理里（`PaperCapture.bands`）；进场页走平摊网格时由那张
        // 只有带子的 `Paper` 实时画回来。
        // 只有一页都没画出来（连续滚动、定版式、排版未就绪）才补窗口坐标的全局带子。
        let page_chrome_drawn = mesh_page_drawn || bands_drawn;
        let mut body = div()
            .id("folio-reader")
            // 宿主的测试要按 id 把滚轮事件投到这里。非 `test-support` 构建里这一层是透明的
            // （`Observed` 原样转交内部元素），所以真机上不多花一个节点。
            .test_support()
            .size_full()
            .relative()
            .on_scroll(listener)
            .children(backdrops)
            .child(column.overflow_hidden());
        // 不是整张纸折页的那几档，读数带照旧压在正文与浮层之上。整张纸那一档要它们在
        // 卷曲网格**之下**（见上面 `whole_sheet_curl` 那一段），所以这里不再挂。
        // 单页分页已经有每页一份的带子（`page_bands`），这一份不再重复。
        if !page_chrome_drawn && !whole_sheet_curl {
            // 全局那一份带子直接吃 `PageBox`：连续档是占位带，定版式是纯文字浮层。
            // 定版式没有任何底色 —— 页图整屏，读数只是浮在照片上的两行字。
            body = body.child(bands::chrome(
                &page_box,
                &band_readouts,
                band_bare,
                bands::Width::Paper,
                cx,
            ));
        }
        if !speaking {
            return body;
        }
        // 正文是逐字形自绘的：accesskit 走的是元素树那一层，字不在树里，读屏念到的就是
        // 一片空白。这一格把这一屏的行一行一个节点挂进 `folio-reader` 那个文档节点底下。
        //
        // 节点用 `Role::Label`，不用看着更贴切的 `Role::TextRun`：accesskit 那个通用过滤器
        // （`accesskit_consumer::common_filter`）把 `TextRun` 判成 `ExcludeNode`，建了等于
        // 没建；而 `Label` 正是 gpui 自己那个 `Text` 元素用的那一档（`elements/text.rs`），
        // 也是 `accesskit_android` 唯一映射成 `android.widget.TextView` 的角色。
        //
        // `synthetic_node_id(ix)` 按行号取，所以**滚动前后同一格是同一个节点**：不然每翻
        // 一屏 TalkBack 都会当成「一整片新内容」重新播报一遍。
        body.role(Role::Document).a11y_synthetic_children(
            move |builder: &mut A11ySubtreeBuilder| {
                for (ix, line) in spoken.into_iter().enumerate() {
                    let mut node = accesskit::Node::new(Role::Label);
                    node.set_value(line.to_string());
                    let id = builder.synthetic_node_id(ix);
                    builder.push_child(id, node);
                }
            },
        )
    }
}

/// 一屏的**全局带** → 这一章在排版坐标里的可见带。只减章基线，别的什么都不减。
///
/// 绘制那一句在 `Scene::build` 里：`屏 = base + top + line.y − scroll`，其中
/// `top = content_top`（正文让给页眉与安全区的那一截）、`scroll` 是视口顶的全局 y。
/// 所以屏上 `[top, top + 视口高]`（内容区）对应排版坐标
/// `[scroll − base, scroll − base + 视口高]` —— `top` 在式子两边同时出现，**这里再
/// 减一次就是重复**。
///
/// 2026-11-01 真机踩过：把 `content_top` 也减掉之后，可见带整体上移了一截。图片行高，
/// 只是被吃掉下半截、看不出来；文字行矮，屏底就空出两行多，读起来像「下边距变大了」。
fn local_band(band: std::ops::Range<f32>, base: f32) -> std::ops::Range<f32> {
    (band.start - base)..(band.end - base)
}

/// 这一栏的字按哪一条折线裁。
///
/// 唯一不裁的是双页里**静态**那一栏（当前跨页没被揭起来的另一半）：那条折线是 leaf 那半块
/// 的折线，拿它去裁静态那一栏，等于把读者正在读的另一半整栏抹掉。
///
/// 从前这里还跟着 `gpu_curl_mesh` 一起关掉裁剪 —— 「进场页整张画，遮挡交给网格」。那一刀
/// 押错了对象：网格是**渲染器那一趟**画的，它缺捕获纹理时会静默跳过整趟（fork 里
/// `page_curl.rs` 那两句 `缺…捕获纹理，这一帧不画卷曲网格`），而应用这侧的 `capture_valid`
/// 只量得出正面那块。跳过的那一帧，未裁的新页字就整屏铺在卷起来的纸该在的地方 ——
/// 读者读到的是「下一页的字闪一下又没了」。
///
/// 留着这一刀不需要网格让路：它与网格的覆盖边界是**同一条线** —— 两边都从
/// [`turn_animation::cylinder_for`] 出来（`crease_for` 就是那个圆柱的轴，见 `curl.rs`
/// 那句「圆柱的轴**就是**那条折线」）。所以网格在的时候裁掉的正是被压住的那些字，一格不少；
/// 网格不在的时候，它是唯一还挡住那一屏字的东西。
fn column_cut(
    leaf_turn: bool,
    is_leaf: bool,
    sheet: &turn_animation::Sheet,
) -> Option<turn_animation::Cut> {
    if leaf_turn && !is_leaf {
        None
    } else {
        sheet.cut
    }
}

#[cfg(test)]
mod tests;

/// `column_cut` 与 `capture_wanted` 的真值表。这两个判定是纯函数，用不着窗口，
/// 按仓库约定内联在被测文件底下 —— 不占宿主套件里一个带窗测试的名额。
#[cfg(test)]
mod truth_tables {
    use super::capture::{capture_wanted, in_viewport, sheet_area_in_page};
    use super::column_cut;
    use super::decor::{dash_spans, three_d_tone};
    use super::marks::text_ink_box;
    use crate::ui::features::reader::motion::turn_animation::{Crease, Cut, Sheet};
    use folio_core::content::format::css::computed::{ComputedStyle, ObjectFit, TextAlign};
    use folio_core::layout::{ImageRun, LineBox, LineFragment, LineRun, RunStyle};

    /// 一张整页底图只属于自己那一节的盒子。
    ///
    /// 量到的现场（Meizu 20，1080×2400，字号约 24）：页眉带 + 安全区 242 px、章间距
    /// 132 px、行距 39 px。第 4 节的 `fm1.jpg` 从前按「章基线 − 滚量」摆而**高过**自己
    /// 那一节的正文框，于是压住第 3 节末段三行字。当时的补法是给图顶也加一截
    /// `content_top` —— 那只是把图往下推，代价是两张满版图之间空出一条缝（读者报的
    /// 「图片之间有间隙」），而且《诡秘之主》制作说明那张卡（正文画在背板之上）的白顶
    /// 反过来盖到了上一节的封面上。
    ///
    /// 现在的口径是**盒子与铺贴同一个数**：图顶 = 章基线 − 滚量，图高 = 盒子高与纸高里
    /// 矮的那个。排版侧把整页画那一档撑到一整格纸，于是两张图首尾正好相接，谁也高不过
    /// 自己那一节。
    #[test]
    fn a_backdrop_is_exactly_its_own_chapter_box() {
        let paper = 853.0_f32; // 一张纸的高（窗口高 − 外壳高）
        let painted = 900.0_f32; // 这一帧的滚量
        let base = 1500.0_f32; // 这一节盒子的顶（全局坐标）
        for placed_height in [paper, 2_400.0] {
            let top = base - painted;
            let art_h = placed_height.min(paper);
            let next_top = base + placed_height - painted; // 下一节盒子顶在屏上的位置
            assert!(
                top + art_h <= next_top + 0.01,
                "高 {art_h} 的图从 {top} 铺到 {}，越过下一节的盒子顶 {next_top}",
                top + art_h
            );
        }
    }

    /// 选区的上下边取**字墨**：行距与行内高图撑出来的那一截都不算。
    ///
    /// 行盒高 = 半行距 + 字墨；默认行距 1.75em 时前者能占四分之一条行。行内一张
    /// 240 高的图还会把整条行盒撑到 240，按行盒画底色时选区跟着变成图那么高 ——
    /// 读者看到的是「选中区域比文字高出一大截」。这一条钉的是两个来源都不参与。
    #[test]
    fn a_selection_band_hugs_the_text_not_the_line_box() {
        let style = RunStyle::new(&ComputedStyle {
            font_size: 17.0,
            line_height: 1.75,
            ..ComputedStyle::default()
        });
        // 行盒被图撑到 240 高，字墨只有 ascent + descent = 18。
        let line = LineBox {
            owner: 0,
            fragments: vec![
                LineFragment::Image(ImageRun {
                    node: 0,
                    href: "x.jpg".into(),
                    width: 80.0,
                    height: 240.0,
                    advance: 80.0,
                    x_offset: 0.0,
                    baseline_shift: 0.0,
                    fit: ObjectFit::Fill,
                }),
                LineFragment::Text(LineRun {
                    node: 0,
                    norm: 0..6,
                    orig: 0..6,
                    style,
                    width: 60.0,
                    ascent: 14.0,
                    descent: 4.0,
                    x_offset: 0.0,
                }),
            ],
            x: 0.0,
            y: 100.0,
            width: 140.0,
            height: 240.0,
            baseline: 230.0,
            align: TextAlign::Start,
            justify: None,
            stack: None,
        };
        let (top, height) = text_ink_box(&line).expect("有文本片段");
        assert_eq!(top, 100.0 + 230.0 - 14.0, "字墨顶 = 行顶 + 基线 − ascent");
        assert_eq!(height, 18.0, "高度只数文本片段：图高与半行距都不进");
        assert!(height < line.height, "行盒被图撑高了，字墨不跟着长");
    }

    /// 字体的度量盒比行距高时（CJK 那一族普遍如此），底色带要**夹回行盒里**。
    ///
    /// 数的是真机那本《哈利·波特》：20px 的字，`ascent + descent` = 31.3 而行距只有 27.0，
    /// 相邻两行的底色带因此互相吃进去 4.3。底色是逐行 `paint_quad` 压上去的，重叠那一档
    /// 合成两遍 —— 屏上量到单层 `(204,226,254)`、两层 `(168,206,253)`，缝正落在两行中间
    /// （读者报的「选中两行时高光有一部分重叠」）。gpui-kit 的 `selection_quad_bounds`
    /// 按 `line_height` 平铺，两块只相接不重叠，这一条对齐它。
    #[test]
    fn a_band_taller_than_the_line_is_clamped_inside_it() {
        let style = RunStyle::new(&ComputedStyle {
            font_size: 20.0,
            line_height: 1.35,
            ..ComputedStyle::default()
        });
        let line = |y: f32| LineBox {
            owner: 0,
            fragments: vec![LineFragment::Text(LineRun {
                node: 0,
                norm: 0..6,
                orig: 0..6,
                style: style.clone(),
                width: 120.0,
                ascent: 24.4,
                descent: 6.9,
                x_offset: 0.0,
            })],
            x: 0.0,
            y,
            width: 300.0,
            height: 27.0,
            baseline: 22.0,
            align: TextAlign::Start,
            justify: None,
            stack: None,
        };
        // 度量盒 31.3 > 行盒 27.0：不收就必然吃进下一行。
        let ink_height = 24.4_f32 + 6.9;
        assert!(ink_height > 27.0, "这一条要钉的就是度量盒高出行盒那一格");
        let (first_top, first_height) = text_ink_box(&line(0.0)).expect("有文本片段");
        let (second_top, second_height) = text_ink_box(&line(27.0)).expect("有文本片段");
        for (top, height, y) in [
            (first_top, first_height, 0.0),
            (second_top, second_height, 27.0),
        ] {
            assert!(
                top >= y - 0.01 && top + height <= y + 27.0 + 0.01,
                "底色带 [{top}, {top}+{height}) 越出了行盒 [{y}, {})",
                y + 27.0
            );
        }
        assert!(
            first_top + first_height <= second_top + 0.01,
            "相邻两行的底色带不许重叠：{} vs {}",
            first_top + first_height,
            second_top
        );
    }

    /// 折页那一趟里**每一栏该按哪条折线裁**，包括 GPU 网格开着的那些帧。
    ///
    /// 这一条钉的是「下一页的字闪一下又没了」：网格是渲染器那一趟画的，它缺捕获纹理时只写一行
    /// 日志、静默不画整趟（fork 的 `page_curl.rs`），而应用这侧的 `capture_valid` 只量得出正面
    /// 那块。被跳过的那一帧，未裁的新页字就整屏铺在卷起来的纸该在的地方。从前 `column_cut`
    /// 跟着 `gpu_curl_mesh` 一起关掉，等于把遮挡全押在「网格一定画得出来」上。
    #[test]
    fn the_curl_clips_every_column_even_while_the_mesh_is_on() {
        let crease = Crease {
            nx: 1.0,
            ny: 0.0,
            d: 200.0,
        };
        let sheet = Sheet {
            offset: (0.0, 0.0),
            z: 0,
            cut: Some(Cut::new(crease, true)),
        };
        assert_eq!(
            column_cut(false, false, &sheet),
            sheet.cut,
            "单页折页里这一栏的字该按折线裁 —— 网格在不在都一样"
        );
        assert_eq!(
            column_cut(true, true, &sheet),
            sheet.cut,
            "双页 leaf 那一栏同样裁"
        );
        assert_eq!(
            column_cut(true, false, &sheet),
            None,
            "双页静态那一栏不裁：那条折线是 leaf 半块的，裁它会抹掉读者正在读的另一半"
        );
    }

    /// 捕获门的真值表。
    ///
    /// 这一格存在的理由是 `content_ready` 那一条：进书第一帧的排版还在后台（`ensure_viewport`
    /// 是这一帧才把作业丢出去的），此时烘下来的纹理只有纸色；而它一旦被记成「已烘」，
    /// 排版回来既不动 `captured_page` 也不置 `capture_dirty` —— 那张白纸就留到下一次换页。
    /// 真机上读到的就是「第一次进书划一页整页白，等它翻完自己好了」。
    #[test]
    fn a_page_whose_typesetting_has_not_landed_is_never_baked() {
        // 稳态：这一页还脏着，该烘。
        assert!(capture_wanted(false, false, true, true, false));
        // 同一页烘过、又不脏：不重复跑整趟捕获 pass。
        assert!(!capture_wanted(false, false, true, false, true));
        // 动画在飞：场景里已经有新页，烘出来不是正在翻的那张。
        assert!(!capture_wanted(true, false, true, true, false));
        // 连续滚动没有「页」。
        assert!(!capture_wanted(false, true, true, true, false));
        // **这一条是主角**：排版没落地时，脏与不脏都不烘 —— 烘出来的只有纸色。
        for dirty in [true, false] {
            for captured in [true, false] {
                assert!(
                    !capture_wanted(false, false, false, dirty, captured),
                    "排版没落地时不该烘（dirty={dirty} captured={captured}）"
                );
            }
        }
    }

    /// 折页那张整屏的纸**不许比窗口高**：捕获矩形按它量，高一截就每一帧都被
    /// [`in_viewport`] 拒掉，捕获永不落地 ⇒ `curl_capture_ready()` 恒假 ⇒ 点屏幕什么都不
    /// 发生。外壳那层已经用 `pt(top_inset)` 推过整页，所以纸就是从阅读页坐标的 0 铺到窗口底，
    /// 与 `viewport_size()` 同一个数，**不**再加 `top_inset` / `bottom_inset` / 两条带子。
    #[test]
    fn the_curl_sheet_never_grows_taller_than_the_window() {
        for (w, h) in [
            (384.0f32, 853.0f32),
            (800.0, 1280.0),
            (1280.0, 800.0),
            (460.0, 520.0),
        ] {
            let sheet = sheet_area_in_page((w, h));
            assert_eq!(sheet.left, 0.0);
            assert_eq!(
                sheet.top, 0.0,
                "纸面从阅读页坐标的 0 起（外壳已经推过安全区）"
            );
            assert_eq!(sheet.width(), w);
            assert_eq!(sheet.height(), h, "纸面到窗口底为止，实得 {sheet:?}");
            let rect = super::capture::capture_region(sheet, 0.0);
            assert!(
                in_viewport(rect, (w, h)),
                "整屏那张纸必须落得进窗口（{w}×{h}），实得 {rect:?}"
            );
        }
        // 竖向那一条是这条门守的东西：带子高、安全区高、手势条高**一个都不许**加回纸面。
        // 少了这一条，「手机内缩一摆就整页比窗高、捕获被拒、点击什么都不发生」会原样回来。
        for (top, bottom) in [(41.0f32, 24.0f32), (0.0, 0.0)] {
            let sheet = sheet_area_in_page((384.0, 853.0));
            let grown = super::turn_animation::Area::new(
                0.0,
                0.0,
                sheet.width(),
                top + 24.0 + (853.0 - top - bottom) + 24.0 + bottom,
            );
            if top + bottom > 0.0 {
                assert!(
                    !in_viewport(super::capture::capture_region(grown, 0.0), (384.0, 853.0)),
                    "把安全区与带子加回去之后矩形会超出窗口 —— 这正是不许加回来的理由"
                );
            }
        }
    }

    /// 虚线/点线边框的切段：从一端铺到另一端，尾巴**截短**而不是留空或平分。
    ///
    /// 平分（把余量摊到每一段）会让一条 1px 的虚线在两端各差半个像素；留空则最后
    /// 一段消失。CSS 与浏览器都是「铺到铺不下为止」，这里钉住同一件事。
    #[test]
    fn dash_spans_run_from_one_end_and_cut_the_tail() {
        // 30px 长、段 3、隙 3 → 起点 0/6/12/18/24，最后一段 24..27 后面只剩 3px 的隙。
        let spans = dash_spans(30.0, 3.0, 3.0);
        assert_eq!(
            spans,
            vec![
                (0.0, 3.0),
                (6.0, 3.0),
                (12.0, 3.0),
                (18.0, 3.0),
                (24.0, 3.0)
            ]
        );
        // 尾巴不够一段：8px、段 3、隙 3 → 第二段从 6 起只剩 2px，照样画。
        assert_eq!(
            dash_spans(8.0, 3.0, 3.0),
            vec![(0.0, 3.0), (6.0, 2.0)],
            "最后一段按剩余长度截短，不是整段丢掉"
        );
        // 尾巴只剩隙：10px 时第五段要 12 才起头，铺到这里就停 —— 尾部留 1px 的空。
        assert_eq!(dash_spans(10.0, 3.0, 3.0), vec![(0.0, 3.0), (6.0, 3.0)]);
        // 点线：段 = 隙 = 线宽（1px 的线就是 1px 实、1px 空）。
        assert_eq!(dash_spans(4.0, 1.0, 1.0), vec![(0.0, 1.0), (2.0, 1.0)]);
        // 退化输入给空表，不许死在循环里。
        assert!(dash_spans(0.0, 3.0, 3.0).is_empty());
        assert!(dash_spans(10.0, 0.0, 3.0).is_empty());
        assert!(dash_spans(10.0, 3.0, -1.0).is_empty());
    }

    /// 3D 边框的两档明暗：亮档比原色亮、暗档比原色暗，纯黑纯白也各能推出一档。
    ///
    /// 用「朝目标补」而不是加减常数，就是为了纯黑 / 纯白这两头：加减会把纯黑压成纯黑，
    /// 亮档就看不见了。
    #[test]
    fn three_d_tones_always_move_away_from_the_extremes() {
        let mid = gpui_kit::Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.5,
            a: 1.0,
        };
        assert!(three_d_tone(mid, true).l > mid.l, "亮档要亮");
        assert!(three_d_tone(mid, false).l < mid.l, "暗档要暗");
        let black = gpui_kit::Hsla { l: 0.0, ..mid };
        assert!(
            three_d_tone(black, true).l > 0.05,
            "纯黑的亮档也得看得见：{}",
            three_d_tone(black, true).l
        );
        assert_eq!(three_d_tone(black, false).l, 0.0, "纯黑没有更暗可去");
        let white = gpui_kit::Hsla { l: 1.0, ..mid };
        assert!(
            three_d_tone(white, false).l < 0.95,
            "纯白的暗档也得看得见：{}",
            three_d_tone(white, false).l
        );
        assert_eq!(three_d_tone(white, true).l, 1.0, "纯白没有更亮可去");
        assert_eq!(three_d_tone(mid, true).a, mid.a, "明暗只动明度，透明度原样");
    }
}
