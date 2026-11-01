//! 阅读视图：把会话的状态摆成元素——逐行画字形、把图片绝对定位、接住滚轮。
//!
//! 这里是**纯渲染**：排版不在这里（在 [`crate::ui::features::reader::model::chapter_layout`]），
//! 状态与命令不在这里（在 [`crate::ui::features::reader::model::session`]）。`render` 只按当前
//! 模式挑出要摆的那几「片」，把排版行摊成 [`Scene`]，再挂上滚轮监听。
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

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use folio_core::PageTurn;
use folio_core::content::anchor::line_at_y;
use folio_core::content::format::css::computed::{BackgroundSize, BorderStyle};
use folio_core::content::format::{PageRegion, pdf::RENDER_EDGE};
use folio_core::layout::fragment::column_offset;
use folio_core::layout::{DecorSeed, FontKey, LineBox, LineFragment};
use folio_core::store::{Annotation, AnnotationStyle};
use gpui_kit::base::motion::{Presence, TransitionId};
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::StyledExt as _;
use gpui_kit::{
    A11ySubtreeBuilder, AnyElement, App, AppContext as _, Background, Bounds, ClickEvent, Context,
    Corners, Div, Element, ElementId, Entity, EntityId, GlobalElementId, Hsla, InspectorElementId,
    InteractiveElement as _, IntoElement, LayoutId, LongPressEvent, PageCurlMesh, PageCurlUniforms,
    PageCurlVertex, ParentElement, PathBuilder, PinchEvent, Pixels, Point, Render, RenderImage,
    Role, ScrollWheelEvent, SharedString, StatefulInteractiveElement as _, Styled,
    TestSupportExt as _, TouchEvent, TouchPhase, WeakEntity, Window, accesskit, canvas, div, hsla,
    img, linear_color_stop, linear_gradient, point, px, size,
};

use crate::ui::features::reader::components::image_viewer::ImageViewer;
use crate::ui::features::reader::components::selection::{self, SelectionVisual};
use crate::ui::features::reader::model::annotations::{self, Palette};
use crate::ui::features::reader::model::book::Book;
use crate::ui::features::reader::model::book_fonts::BookFonts;
use crate::ui::features::reader::model::chapter_layout::{Document, Viewport};
use crate::ui::features::reader::model::chrome;
use crate::ui::features::reader::model::fixed_book::{FixedBook, PageTile};
use crate::ui::features::reader::model::fixed_zoom::{
    FixedFrame, FixedZoom, max_scale, region_output, visible_region,
};
use crate::ui::features::reader::model::image_atlas::ImageAtlas;
use crate::ui::features::reader::model::page_geometry::{
    BandPolicy, PageBox, PageInsets, PageRect,
};
use crate::ui::features::reader::model::paging::PrefetchSide;
use crate::ui::features::reader::model::prefetch;
use crate::ui::features::reader::model::session::{
    AnnotationSpans, CurlMesh, ImageHit, PageRef, PinnedRegion, REGION_SETTLE, ReaderSession,
    Source, StackHit, Turn, TurnRun, land_at_anchor, start_turn,
};
use crate::ui::features::reader::model::swipe::{release_easing, release_transition};
use crate::ui::features::reader::motion::turn_animation::{self, Sheet, Tint};
use crate::ui::features::reader::view::bands;
use crate::ui::features::reader::view::paint::{self, GlyphRun, GlyphsElement, Mark, shape};
use crate::ui::features::reader::view::scene::{BlockDecor, BlockPicture, Scene, VisibleImage};
use crate::ui::foundation::images::DecodeRequest;
use crate::ui::services::config::ConfigHolder;
use crate::ui::shell::{turn_duration, turn_transition};

/// 定版式预取几页。
///
/// 两张是「读者手上这一页 + 下一张」的量：720 档预览一页约 2 MB，两页叠在 24 MB 的
/// 手机预算里还够；一张的话快速滑动时读者到那一页它往往还在画（手机上栅格化一页是
/// 几十到几百 ms）。再多就不再给了 —— 内存先炸，而滑得更快的读者本来也看不清。
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
    /// `true` = 往后走（下一页 / 往下滚）。
    forward: bool,
    /// `true` = 这一本从右往左读。只有折页那一档读它（它决定纸的**自由边**在哪一侧，
    /// 见 `turn_animation::Pose::free_edge_right`）；平移/覆盖那几档今天还没接这一格。
    rtl: bool,
}

/// 这一帧当前章要画成整页画时的那张画（见 [`current_page_art`]）。
struct PageArt {
    /// 属于哪一章：绘制时只对**这一章**跳过版心里的那张图。
    chapter: usize,
    /// 按窗口尺寸交给 `scene::backdrop_decor` 折格子的层。
    pictures: Vec<folio_core::layout::DecorPicture>,
    /// 单图页：那唯一一张图已经在背板上，正文那一章整章不画。
    picture_only: bool,
}

/// 整页图该不该用 `contain`：比例和屏幕差得远时，宁可留底色也不裁掉内容。
///
/// 封面有 `0~slim` 这种同比例兄弟，走 `cover` 仍然不裁；正文里的横图/方图没有兄弟，
/// `cover` 会只剩中间一条 —— 读者报的「图片显示不全」就是它。
fn page_art_contain(doc: &Document, href: &str, want_aspect: f32) -> bool {
    let (w, h) = doc.images().natural_size(href);
    if w <= 0.0 || h <= 0.0 || !want_aspect.is_finite() || want_aspect <= 0.0 {
        return false;
    }
    ((w / h) - want_aspect).abs() / want_aspect > 0.15
}

/// 这一帧的整页画：当前章（视口顶所在的那章）是一整页底图、或整章只有一张满宽图。
///
/// 阅读器把它贴到**整窗**上、读数带浮在上面（Reeden 那种画法），而不是排进版心：
/// 版心从页眉下面开始、左右还让着页边距，整页的画在那里永远只是一截。
/// 这一章的**整页底图**：根上那张铺满整章的背景图。
///
/// 两档，判据与 [`page_art_for_doc`] 第 1 条同一份：
///
/// - `full_bleed`（作者写了 `background-attachment: fixed`）无条件算；
/// - 其余根底图只在**这一章没有可读正文**时算 —— 扉页那种「底图 + 标题图、一个字没有」
///   的章，底图也该铺满整窗。
///
/// 返回装饰种子里那一条；绘制侧按它的 `pictures` 铺，排版侧按它决定「块里那份别画」。
fn page_backdrop(doc: &Document) -> Option<&DecorSeed> {
    page_backdrop_index(doc).and_then(|index| doc.decor_seeds().get(index))
}

/// [`page_backdrop`] 挑中的那一条种子在 `decor_seeds()` 里的**下标**。
///
/// 判据与那一问完全同一份，只是多回一个位置：绘制侧要跳过的正是**这一条**，而
/// `full_bleed` 只是它的一个特例。《哈利·波特全集》`v1.xhtml` 的底图没写
/// `background-attachment: fixed`，从前按 `full_bleed` 筛的那一道闸（`scene::place_decorations`）
/// 拦不住它，于是同一张图既进了整窗背板、又留在版心里画了一遍 —— 而版心那一份压在
/// 不透明的纸底色下面，读者只看得见被裁成章高一条的那一截（2026-11-02 真机）。
fn page_backdrop_index(doc: &Document) -> Option<usize> {
    let seeds = doc.decor_seeds();
    if let Some(index) = seeds
        .iter()
        .position(|seed| seed.full_bleed && !seed.pictures.is_empty())
    {
        return Some(index);
    }
    if doc.has_text() {
        return None;
    }
    seeds
        .iter()
        .position(|seed| seed.x.abs() < 0.5 && seed.y.abs() < 0.5 && !seed.pictures.is_empty())
}

fn current_page_art(book: &Book, want_aspect: f32) -> Option<PageArt> {
    let chapter = book.chapter_index();
    let doc = book.span_doc(chapter)?;
    // 排版还没落地的那一帧没有可信的种子与行盒：等下一帧。
    if !doc.is_current() {
        return None;
    }
    page_art_for_doc(chapter, doc, want_aspect)
}

/// 某一章的整页画：根上的底图章，或整章只有一张满宽图。
fn page_art_for_doc(chapter: usize, doc: &Document, want_aspect: f32) -> Option<PageArt> {
    // 1) 根上的底图。两档：
    //
    //    - `full_bleed`（作者写了 `background-attachment: fixed`）：卷首 / 画廊那类
    //      整页底图章，**不管有没有正文**都铺满；
    //    - 其余根底图只在**这一章没有可读正文**时铺满。《哈利·波特全集》`v1.xhtml`
    //      是「body 背景 + 两张标题图、一个字没有」的扉页，底图没写 `fixed`，从前掉进
    //      正文页，于是顶上顶着一条不透明页眉带、图从它下面起（读者报的
    //      「封面没有全屏，上面空出来一大片白」）。
    //
    //    判据用的是「有没有可读正文」（`Document::has_text`），不是「这一屏有没有字」：
    //    按屏判会让页眉带在滚动中忽隐忽现，比现在这版更吵。
    if let Some(seed) = page_backdrop(doc) {
        return Some(PageArt {
            chapter,
            pictures: seed.pictures.clone(),
            picture_only: false,
        });
    }
    // 2) 单图页：Calibre 封面、整页插图章。判据**不问界面**——排版层切完片之后那一片
    //    自己带着旗（`Fragment::picture_page`：这一片没有一行有宽度的正文，而且恰好一张
    //    铺到版心八成以上的图）。连续滚动那一档一片装完整章，所以这一问等价于从前
    //    「扫整章」那一条，但只有一份实现。
    //
    //    从前这里还要求「同目录里有一张与窗口宽高比差 ≤0.2 的兄弟图」（`0.jpg` 旁边
    //    摆 `0~slim.jpg` 那种封面变体）才肯拉满屏——两道闸只有封面页过得去，
    //    正文里那张独占一页的照片因此从来没被当整页画过。铺满那一档已定为 cover
    //    （盖住整窗、超出裁掉），比例不合不再是理由。
    let hrefs = doc.slice_of(0, 0)?.picture_page.clone();
    let [href] = hrefs.as_slice() else {
        return None;
    };
    // 4:3 的 `0.jpg` 与手机版的 `0~slim.jpg` 并排时，按屏幕比例挑更合身的那张：
    // 否则 cover 会把 4:3 左右各裁一截（读者报的「标题两端被切」）。
    let href = doc
        .images()
        .closest_portrait_variant(href, want_aspect)
        .unwrap_or_else(|| href.clone());
    let fit = if page_art_contain(doc, &href, want_aspect) {
        BackgroundSize::Contain
    } else {
        BackgroundSize::Cover
    };
    Some(PageArt {
        chapter,
        pictures: vec![crate::ui::features::reader::view::scene::full_page_picture(
            &href, fit,
        )],
        picture_only: true,
    })
}

/// 滚动档的整页画：宽度铺满这一格的图改成**贴顶**画（`background-position-y: 0`），
/// 其余原样。
///
/// 读者要的是「封面那种图在滚动档里贴着系统栏」——`contain` 居中会在顶上留一条底色，
/// 看起来像页眉下面又空了一块（Reeden 那张封面就是宽度铺满、贴顶、余下的地方留给底色）。
/// 分页 / 双页不走这里：那两档要的是「单独居中放一页」。
fn scroll_art_pictures(
    art: &PageArt,
    width: f32,
    height: f32,
    images: &crate::ui::foundation::images::ChapterImages,
) -> Vec<folio_core::layout::DecorPicture> {
    let mut pictures = art.pictures.clone();
    for picture in &mut pictures {
        if crate::ui::features::reader::view::scene::picture_fills_width(
            picture, width, height, images,
        ) {
            picture.position.y =
                folio_core::content::format::css::computed::BackgroundAxis::Fraction(0.0);
        }
    }
    pictures
}

/// 分页档里「这一页的整页底图」那一层的元素 id。
///
/// 章与页各占一段：翻页途中两张纸可能同章也可能跨章，id 不能撞；宿主测试按它量
/// 「这张画这一帧在不在、走没走」（见 `body/tests/swipe.rs` 里那条平移用例）。
fn page_art_element_id(chapter: usize, page: usize) -> gpui_kit::ElementId {
    (
        "reader-page-art",
        chapter.saturating_mul(1_000_000).saturating_add(page),
    )
        .into()
}

/// 这一章在滚动档里是不是按**窗口坐标**画的（单图页 / Calibre 封面）。
///
/// 命中换算要它：这一档的正文从窗口顶起画（`Scene::build` 拿到的 `top = 0`），
/// 反算时不把「页眉 + 安全区」那一段加回去，点到的是上面一截的字。
/// **固定底图章不算**：它的底图铺满整窗，正文仍从 `content_top` 起画。
///
/// 判据本体在 [`Document::is_picture_only_page`] —— 与 `page_art_for_doc` 的
/// `picture_only` 是同一件事，别在这里另写一份。
pub(in crate::ui::features::reader) fn chapter_is_picture_only_page(doc: &Document) -> bool {
    doc.is_picture_only_page()
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
        let (progress, walked) = match turn.run {
            TurnRun::Following { t } => (t, false),
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
                        (progress, cx.reduce_motion() || elapsed >= over)
                    }
                    _ => (sampled.progress, sampled.progress >= 1.0),
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
            forward: turn.forward,
            rtl: self.progression_rtl(),
        })
    }
}

impl Render for ReaderSession {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::ui::features::reader::model::perf::reset();
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
        // 把同一页砍成两半。所以这一族的默认档恒为整页翻，读者手动塞进来的双页也按
        // 分页走；「上下滚动」照旧由 `reconcile` 带进连续。
        let fixed_layout = self.is_fixed();
        let by_width = if fixed_layout {
            chrome::PageMode::Paged
        } else {
            chrome::default_mode(usable, gap)
        };
        let mut wanted = if self.mode_undecided {
            by_width
        } else {
            self.mode
        };
        if fixed_layout && wanted == chrome::PageMode::Spread {
            wanted = chrome::PageMode::Paged;
        }
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

        // 翻页几何用的那一块：连续滚动是正文版心，其余是整张纸。与 `ReaderSession::page_area()`
        // 同一个式子；这里先算成局部值，下面 `font_cache` 把 `self` 借走之后就不能再调方法了。
        let page_area = {
            let rect = if mode.is_continuous() {
                page_box.content
            } else {
                page_box.paper
            };
            turn_animation::Area::new(rect.x, rect.y, rect.right(), rect.bottom())
        };

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
        // 定版式那一族没有排版可等（页是既定的位图），所以那一族恒算「已落地」。
        let content_ready =
            self.is_fixed() || self.book().is_none_or(|book| book.document().is_current());
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
        );
        // 捕获层 1（正面）这一帧要不要画一份副本。只有可重排那一族要：定版式那一族不出卷曲
        // 网格（它的页面本来就是整张位图），连续滚动那一档根本不烘（上面那条已经挡掉）。
        // 副本必须自己画一份 —— 主 pass 只画 `order < CAPTURE_ORDER_BASE`，层里的东西不上屏。
        let capture_front = capture_needed && !self.is_fixed();
        // 日志里要摆的是「烘那一张时」的两组数，与今天这帧的比一眼就能看出差在哪。`Option` 进不了
        // tracing 的字段，所以摊成数：哨兵 = 还没烘过（那一格正是要盯的状态）。
        let (captured_ch, captured_pg) = self.captured_page.unwrap_or((usize::MAX, usize::MAX));
        let (captured_w, captured_h) = self.captured_size.unwrap_or((-1.0, -1.0));

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
        // 等宽偏好整帧取一次：兜底那一行与正文的 run 都要它。
        let mono = SharedString::from(self.prefs.mono_font_family.clone());
        // `FontKey → Font` 的解析每帧对每条可见 run 都要一次，交给会话持有的缓存
        // （避免逐 run 扫设备字体表）。body / mono / faces / 设备表变了由会话清或重建。
        // 读数带在 `font_cache` 那句之前算：那一句把 `self` 可变借走，之后再借整份
        // 会话就过不去（进度要读整本书）。每页的章名/页号在循环里按 page_ref 算，
        // 跨章翻页时左右两页才各念各的。
        let curl_turn = turn.as_ref().map_or(page_turn, |frame| frame.style) == PageTurn::Curl;
        let page_percent: SharedString = chrome::percent_label(self.progress()).into();
        let font_cache = &mut self.font_cache;
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
        // 这一帧有没有**邻章**的整窗背板。锚章那一格由 `page_art` 自己说话，而邻章的画
        // 也铺在同一扇窗上（`placements` 那条循环），纸底色若照旧挂在 `column` 上就会
        // 把它整个盖住 —— 读者于是只看得见版心里那份被裁成章高一条的复制品
        // （2026-11-02 真机《哈利·波特全集》`v1.xhtml`：视口顶还在目录，封面已经在屏上）。
        let mut neighbour_backdrop = false;
        // 图片永远压在字形之上：收集起来，等 `element` 入树之后再挂。
        let mut overlays: Vec<AnyElement> = Vec::new();
        // 整页画（`background-attachment: fixed` 底图章 / 单图页）：背板元素与这一帧的判定。
        // 背板挂在 `column` **之前**、`body` 之下 —— 压着页底色、让正文与读数带浮在上面；
        // `body` 自己不再裁切，所以背板可以向上铺到安全区里（见函数末尾的装配）。
        let mut backdrops: Vec<AnyElement> = Vec::new();
        let mut page_art: Option<PageArt> = None;
        let mut bare_bands = false;
        // 分页里出现了整页图/固定底图：这一帧的底色要让开，整窗背板才透得上来。
        let mut page_picture = false;
        // 固定底图章（带正文）也要让读数带透明浮在画上；它没有 `picture_page` 旗。
        // 分页档按**页**判（见下面 `slots` 里的 `art_drawn`）。
        let mut page_bands: Vec<AnyElement> = Vec::new();
        // 这一档这一帧要盖在两张纸之上的那几块：折页的纸面与折谷四道，覆盖/层叠的前缘
        // 投影与纸边光。其余档位给的是空清单，所以下面那一块一个形状都不会入树。
        let mut shades = turn_animation::Shades::new();
        // C 方案：这一帧要画的卷曲网格，以及它对应的进场页。网格取代旧页的
        // 字形与图片 —— 旧页已经烘进页面纹理，网格自己把它卷起来。
        let mut gpu_curl_mesh: Option<PageCurlMesh> = None;
        // 折页这一趟被揭起来的那一张 leaf（双页 = 半张跨页，单页 = 整张纸，含页眉/页脚）。
        // 捕获纹理、卷曲网格、手指归一化三处共用这一块；`None` = 这一帧没有折页在飞。
        let mut curl_leaf: Option<turn_animation::Area> = None;
        // 捕获层 1（正面）那一份副本里，不属于字形元素的两类内容：块级装饰与图片。
        // 字形与批注痕迹整份克隆 `element` 就有（见 `GlyphsElement::capture_copy`）。
        let mut front_decor: Vec<AnyElement> = Vec::new();
        let mut front_images: Vec<AnyElement> = Vec::new();
        // 捕获层 2（背面）那一份：揭起来那张纸的背面 = 目标跨页的另一半（双页）/ 目标页整张
        // （单页）。双页那一栏在主场景里本来就跳过，所以这里攒的是单独的一份。
        let mut back_element = GlyphsElement::new();
        let mut back_children: Vec<AnyElement> = Vec::new();
        // 背面那一栏要横向挪进 leaf 那一格：纹理按 leaf 那块矩形烘，网格也按 leaf 的尺寸采，
        // 而这一栏的字与图算出来的 x 是它**自己**那一栏的位置。
        let mut back_dx = 0.0f32;
        // 这一趟要烘进捕获层 2 的是哪一格（`(章, 页, 物理栏)`）。`None` = 这一帧不烘背面。
        let mut back_key: Option<(usize, usize, usize)> = None;
        // 捕获层 3（进场页）那一份：平摊翻页里底下露出来的那一张。它不能与背面共用一张
        // 纹理 —— 双页折页的背面装的是目标跨页的另一半，进场页是再下一页，两者不是同一页。
        let mut incoming_element = GlyphsElement::new();
        let mut incoming_children: Vec<AnyElement> = Vec::new();
        let mut incoming_bands: Vec<AnyElement> = Vec::new();
        // 这一帧要烘进场页的是哪一格（`(章, 页)`）+ 烘那一帧进场页自己的位移（副本按
        // `-offset` 退回静止位置再烘）。`None` = 这一帧不烘。
        let mut incoming_bake: Option<((usize, usize), (f32, f32))> = None;
        // 进场页已经烘好：这一帧起整页跳过，由平摊网格采那张纹理（`incoming = 1`）。
        // 分页臂里无条件重赋，初值在那条路上没人读 —— 其余臂（样本 / 定版式 / 连续滚动）
        // 靠它保持 `None`。
        #[allow(unused_assignments)]
        let mut flat_incoming: Option<PageCurlMesh> = None;
        // 这一趟翻到哪一页。放在 `match` 之外：翻页途中**底下的那一页**的页眉/页脚
        // 要按它念，而捕获层 2 烘过一次之后 `back_key` 就不再是 `Some` 了。
        let mut turn_arriving: Option<PageRef> = None;
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
        let mut spoken: Vec<SharedString> = Vec::new();
        // `&mut self.source` 只借用这一个字段，所以臂里写 `self.content_height`、
        // 读 `self.scroll_y` 都不冲突（前提是别把 `self` 整个传进函数）。
        let mut prefetch_side: Option<PrefetchSide> = None;
        // 章界那一笔落在哪：只有「这一档在这种摆法下不挂时钟」（不走动画）时才会有值。
        // 真换锚要等这一帧的元素树搭完（与 `drain_pending_turn` 同一处），中途换会让
        // 同一帧上下半截按两章画。
        let mut land_chapter: Option<PageRef> = None;
        // 这一帧主场景与捕获副本真正交给图集碰过的那些图（容器路径）。预解那一步要跳过它们 ——
        // 「还没进图集」只能这样知道：`painted` 已经把「在场」记上了。
        let mut painted_keys: Vec<String> = Vec::new();
        // 叠图里「下一张」的活儿：搭场景时顺手收下（`Scene::upcoming`），帧末交给图集
        // 预热。切图那一下就不用等解码 —— 大图首解一次几十毫秒，读者读成「卡一下」。
        let mut upcoming: Vec<crate::ui::foundation::images::DecodeRequest> = Vec::new();
        // 定版式这一臂算出的高亮几何要活到 match 外面：菜单条与可重排共用同一段摆位。
        let mut fixed_visual: Option<SelectionVisual> = None;
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
                            content_top + self.prefs.margin_x + self.prefs.font_size,
                        )
                        .color(foreground),
                );
                self.content_height = 0.0;
                self.laid_out.viewport = viewport_height;
                // 兜底那一行也是正文位置上的字：读屏该念到它，不然这一状态整屏是静音的。
                if speaking {
                    spoken.push(text.clone());
                }
            }
            Source::Fixed(book) => {
                // 定版式：页是既定的，没有排版链。两档共用同一张页表：
                //
                // - 连续竖滚：页按版心宽铺开、纵向排成一条栈，位置是栈上的像素；
                // - 整页翻：一屏一页，位置是页号。
                //
                // 连续那一档的做法与可重排那一族的「拼接链」同源：`scroll_y` 是权威坐标，
                // 页号只是「视口顶落在哪一页」的读数。
                let pages = book.page_count();
                let fallback_aspect = usable / viewport_height.max(1.0);
                if mode.is_continuous() {
                    // 宽度变了要重建页栈。重建之前先把「读者眼前是第几页、页内几成」
                    // 记下来，建完按新页高搬回同一处 —— 否则转屏 / 分屏会让屏上跳一页。
                    // 挂着一份恢复请求时不算：那一份才是目标，旧坐标属于上一本书 / 上一档。
                    let anchor = self
                        .pending_restore
                        .is_none()
                        .then(|| {
                            book.stack()
                                .and_then(|s| s.page_and_fraction(self.scroll_y))
                        })
                        .flatten();
                    if book.ensure_stack(usable, FIXED_PAGE_GAP, fallback_aspect)
                        && let Some((page, fraction)) = anchor
                        && let Some(fresh) = book.stack()
                    {
                        let top = fresh.top_of(page).unwrap_or(0.0);
                        let height = fresh.height_of(page).unwrap_or(0.0);
                        self.scroll_y = top + fraction * height;
                    }
                    let total = book.stack().map_or(0.0, |stack| stack.total());
                    self.laid_out = crate::ui::features::reader::model::session::LaidOut {
                        mode,
                        columns: 1,
                        pages,
                        viewport: viewport_height,
                    };
                    self.content_height = total;
                    // 恢复落点：定版式的 `locator.chapter` 就是页号，落在那一页的页首。
                    if let Some(locator) = self.pending_restore.take() {
                        book.set_page(locator.chapter as usize);
                        self.scroll_y = book.stack_top_of(book.page()).unwrap_or(0.0);
                    }
                    // 这一帧的缩放几何先写下来（捏合与拖动发生在事件里，几何只有这里量得出来），
                    // 再把倍率与平移收进这一页允许的范围：转屏 / 分屏之后上一帧的倍率可能
                    // 已经越过新的上限。
                    //
                    // 页栈是按**版心宽**铺的，所以上下滚动这一档的轴心是版心左上角，
                    // 每行在 1x 里就是 `margin_x .. margin_x + usable`。
                    let current = book.page();
                    let page_height = book
                        .stack()
                        .and_then(|stack| stack.height_of(current))
                        .unwrap_or(0.0);
                    let source_width = zoom_source_width(book.page_tiles(current).iter());
                    self.fixed_frame = FixedFrame {
                        origin: (0.0, content_top),
                        pivot: (0.0, content_top),
                        page: (usable, page_height),
                        area: (f32::from(viewport_size.width), viewport_height),
                        row: (self.prefs.margin_x, usable),
                        total,
                        max: max_scale(source_width, usable),
                        continuous: true,
                    };
                    // 按本帧的几何收一次倍率与平移（`settle_fixed_zoom` 要整块
                    // `&mut self`，而这里 `book` 还借着 `source`，所以就地做同一步）。
                    {
                        let frame = self.fixed_frame;
                        self.fixed_zoom
                            .set_scale(self.fixed_zoom.scale(), frame.max);
                        let pan = self.fixed_zoom.pan();
                        let scale = self.fixed_zoom.scale();
                        self.fixed_zoom.set_pan(frame.clamp_pan(pan, scale));
                    }
                    // 视口在 1x 内容坐标里只占 `viewport / scale`：页是按 1x 铺的，
                    // 放大之后一屏压着的字变少，能滚到的底也跟着变。
                    let band = viewport_height / self.fixed_zoom.scale().max(1.0);
                    let max_scroll = self.fixed_frame.max_scroll(self.fixed_zoom.scale());
                    self.scroll_y = self.scroll_y.clamp(0.0, max_scroll);
                    // 画出来的位置：点屏 / 方向键那一趟在补间，手指与滚轮那一趟直接跟
                    // `scroll_y`。命中与绘制都读这一个数（与可重排连续那一臂同一条规矩）。
                    let painted_y = match turn {
                        Some(frame) => frame.scroll.map_or(self.scroll_y, |(from, to)| {
                            chrome::tween(from, to, frame.progress)
                        }),
                        None => self.scroll_y,
                    };
                    self.scroll_painted = painted_y.clamp(0.0, max_scroll);
                    // 页号由**输入**那一头改写（滚轮 / 落页 / 恢复），渲染帧只把它抄回来：
                    // 在这里再按 `scroll_y` 推一次，会把「目录跳到最后一页」那个被夹到
                    // `max_scroll` 的落点读回成上一页 —— 最后一页常常比一屏矮，页首永远
                    // 到不了视口顶。
                    self.page = book.page();
                    // 这一帧要画的页：按页栈切出可见区间，先抄成一个小表再画 ——
                    // `stack()` 借着 `book`，抄完就放开，下面还要问字层与图集。
                    let rows: Vec<(usize, f32, f32)> = book
                        .stack()
                        .map(|stack| {
                            stack
                                .visible(self.scroll_painted, band)
                                .filter_map(|page| {
                                    Some((page, stack.top_of(page)?, stack.height_of(page)?))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let mut boxes: Vec<(usize, Bounds<Pixels>)> = Vec::new();
                    let mut visible_pages: Vec<usize> = Vec::with_capacity(rows.len());
                    // 字层还没取到的页：这一帧先不画，循环走完统一交给后台
                    // （见 `request_page_texts` 与 `FixedBook::page_text_cached`）。
                    let mut pending_text: Vec<usize> = Vec::new();
                    for (page, top, height) in rows {
                        visible_pages.push(page);
                        let page_tiles = book.page_tiles(page);
                        let has_text = page_tiles.iter().any(|tile| tile.renders);
                        // 页栈已经把这一页的高度算好了（按版心宽与宽高比），这里只把
                        // 每一格按同一高度横排；顶边是栈坐标减掉画出来的滚动量。
                        let (tiles, placed) = fixed_page_tiles_at(
                            page_tiles,
                            usable,
                            height,
                            content_top + top - self.scroll_painted,
                            self.prefs.margin_x,
                            fallback_aspect,
                            has_text,
                        );
                        // 只查缓存：PDF 一页字层桌面要 1.5 s，在绘制帧里同步取就是
                        // 打开书卡十秒（见 `FixedBook::page_text_cached` 那一段）。
                        // 没到的页记下来，循环走完丢给后台，取回来重画。
                        let (text, text_key) = if has_text {
                            match book.page_text_cached(page) {
                                Some((text, hash)) => {
                                    let key = text.as_ref().map(|_| hash);
                                    (text, key)
                                }
                                None => {
                                    pending_text.push(page);
                                    (None, None)
                                }
                            }
                        } else {
                            (None, None)
                        };
                        // 页格仍然按 1x 算，再映射到屏上：轴心是版心左上角，所以放大看
                        // 起来是「从版心顶长出来」，纵向那一段由 `scroll_painted` 先减掉。
                        let zoom = self.fixed_zoom;
                        let pivot = (0.0, content_top);
                        for at in tiles {
                            // 解不出像素的那一格空着，但仍然占位：多图页的排版不该因为
                            // 少一张图就把剩下的往左挤。
                            let Some(request) = at.tile.request.as_ref() else {
                                continue;
                            };
                            let key = request.key.clone();
                            let Some(image) =
                                painted_tile(&atlas, &at.tile, view, cx, motion.scrolling)
                            else {
                                continue;
                            };
                            let rect = zoom.map(
                                Bounds::new(
                                    point(px(at.x), px(at.y)),
                                    size(px(at.width), px(at.height)),
                                ),
                                pivot,
                            );
                            let selected = text.is_some() && at.tile.renders;
                            // 与整页翻那一臂同一条分岔：手机上「看大图」吃长按，
                            // 有字的那一页把长按留给选中（入口在浮层与底栏）。
                            let no_entry: Option<Entity<ImageViewer>> = None;
                            let entry = if selected && long_press {
                                &no_entry
                            } else {
                                &viewer
                            };
                            let index = overlays.len();
                            overlays.push(page_image(
                                f32::from(rect.origin.x),
                                f32::from(rect.origin.y),
                                f32::from(rect.size.width),
                                f32::from(rect.size.height),
                                image,
                                key,
                                entry,
                                long_press,
                                index,
                                &reader_weak,
                            ));
                            // 放大停稳那一档：看得见的那一块按 `REGION_EDGE` 重画，叠在这一页那一张上面。
                            let block = pinned_region(
                                RegionAsk {
                                    book,
                                    atlas: &atlas,
                                    page,
                                    tile: &at.tile,
                                    rect: (at.x, at.y, at.width, at.height),
                                    pivot: (0.0, content_top),
                                    viewport: (
                                        0.0,
                                        content_top,
                                        f32::from(viewport_size.width),
                                        viewport_height,
                                    ),
                                    zoom: self.fixed_zoom,
                                    now,
                                },
                                &self.region_moved_at,
                                &self.region_pinned,
                                view,
                                cx,
                            );
                            if let Some(pin) = block {
                                let key = pin.request.key.clone();
                                if let Some(image) = painted(&atlas, pin.request, view, cx) {
                                    let zoomed = self.fixed_zoom.map(
                                        Bounds::new(
                                            point(px(pin.rect.0), px(pin.rect.1)),
                                            size(px(pin.rect.2), px(pin.rect.3)),
                                        ),
                                        (0.0, content_top),
                                    );
                                    overlays.push(page_image(
                                        f32::from(zoomed.origin.x),
                                        f32::from(zoomed.origin.y),
                                        f32::from(zoomed.size.width),
                                        f32::from(zoomed.size.height),
                                        image,
                                        key,
                                        &no_entry,
                                        long_press,
                                        overlays.len(),
                                        &reader_weak,
                                    ));
                                }
                            }
                        }
                        if let (Some(text), Some(at)) = (&text, placed) {
                            let at = self.fixed_zoom.map(at, (0.0, content_top));
                            // 命中用的尺每页各记一条：连续竖滚里屏上可能同时有两页带字，
                            // 只记一页会让另一页的拖选按到隔壁的盒子上。
                            boxes.push((page, at));
                            let marks = fixed_marks(
                                &self.annotation_spans,
                                &self.annotations,
                                page,
                                text_key.unwrap_or(0),
                                text,
                                at,
                                &palette,
                            );
                            if !marks.is_empty() {
                                overlays.push(marks_above(marks));
                            }
                            if let Some(selected) = self
                                .selection
                                .as_ref()
                                .filter(|selected| selected.chapter == page)
                            {
                                fixed_visual = Some(selection::page_visual(
                                    text,
                                    selected.range.clone(),
                                    at,
                                    cx.theme().selection,
                                    cx.theme().selection.opacity(HIGHLIGHT_ALPHA),
                                ));
                            }
                        }
                    }
                    *self.fixed_text_boxes.borrow_mut() = boxes;
                    // 字层没到的那几页：这一帧先空着（页图照画），后台取回来再重画。
                    request_page_texts(book, pending_text, &reader_weak, cx);
                    // 下一页先解好（PDF 要栅格化、CBZ 要读字节），取**可见页的后一页**：
                    // 连续竖滚一屏可能压着两页，从视口顶那一页往后数会把它漏掉。
                    // 预取：往读者滑的那边取两页（停下那一帧仍是「后一页优先」，
                    // 由 `fixed_target` 决定）。滑动中只排预览 —— 见 `prepare_tile`。
                    if turn.is_none()
                        && let Some(last) = visible_pages.last().copied()
                    {
                        let targets = match motion.direction {
                            Some(direction) => {
                                prefetch::fixed_targets(last, pages, direction, PREFETCH_RADIUS)
                            }
                            None => prefetch::fixed_target(last, pages).into_iter().collect(),
                        };
                        for target in targets {
                            for tile in book.page_tiles(target) {
                                prepare_tile(&atlas, &tile, view, cx);
                            }
                        }
                    }
                    // 选中件只摆一份（它铺满整块版心，监听也只该挂一份）；但「能不能从
                    // 这一页起手选」由上面那张多页表回答。没有带字的页就整层不摆。
                    if !self.fixed_text_boxes.borrow().is_empty() {
                        overlays.push(selection::overlay(
                            fixed_visual.clone().unwrap_or_default(),
                            &reader_weak,
                        ));
                    }
                    // 选区挂的页滚出可见区间就是「翻走了」：与分页那一族同一条规矩。
                    // 这一问放在所有 `book` 借用之后，才能调 `&mut self` 的方法。
                    if self
                        .selection
                        .as_ref()
                        .is_some_and(|selected| !visible_pages.contains(&selected.chapter))
                    {
                        self.clear_selection();
                    }
                } else {
                    self.laid_out = crate::ui::features::reader::model::session::LaidOut {
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
                    // 定版式的下一页先解好：PDF 的一页要回引擎栅格化、CBZ 的一页要读字节 +
                    // 解码，从前都要等读者翻过去那一下才开始。静止帧里每帧 prepare 一次，
                    // 图集按「这一帧碰过」留住那一格；翻页动画期间不抢这笔活儿。
                    if turn.is_none() {
                        let targets = match motion.direction {
                            Some(direction) => {
                                prefetch::fixed_targets(current, pages, direction, PREFETCH_RADIUS)
                            }
                            None => prefetch::fixed_target(current, pages).into_iter().collect(),
                        };
                        for target in targets {
                            for tile in book.page_tiles(target) {
                                prepare_tile(&atlas, &tile, view, cx);
                            }
                        }
                    }
                    // 这一页有没有字：PDF / AZW4 / DjVu 那一族才有（漫画的一页就是一张图，
                    // EPUB 的定版式字在 XHTML 里而不在页上）。字层只查缓存，没到就先不画、
                    // 交给后台（理由与竖滚那一臂同一条：绘制帧里同步取一页就是 1.5 s）。
                    let has_text = book.page_tiles(current).iter().any(|tile| tile.renders);
                    let cached_text = if has_text {
                        book.page_text_cached(current)
                    } else {
                        None
                    };
                    let text_pending = has_text && cached_text.is_none();
                    let (text, text_key) = match cached_text {
                        Some((text, hash)) => {
                            let key = text.as_ref().map(|_| hash);
                            (text, key)
                        }
                        None => (None, None),
                    };
                    if text_pending {
                        request_page_texts(book, [current], &reader_weak, cx);
                    }
                    // 定版式的翻页动画与流式共用同一套纸面位移；折页在 `begin_turn` 里
                    // 已经换成平移，这里再收一次口，配置从别处带进来的怪值也不会去卷一张位图。
                    let mut incoming_shift = (0.0, 0.0);
                    let mut outgoing_page = None;
                    if let Some(frame) = turn {
                        let style = chrome::fixed_turn_style(frame.style);
                        if style != PageTurn::None {
                            let pose = turn_animation::Pose {
                                forward: frame.forward,
                                rtl: frame.rtl,
                                grip: None,
                                revert: None,
                            };
                            let [incoming, outgoing] =
                                turn_animation::of(style).sheets_at(frame.progress, area, pose);
                            incoming_shift = incoming.offset;
                            if let Some(leaving) = frame.leaving {
                                outgoing_page = Some((leaving.page, outgoing.offset));
                            }
                        }
                    }
                    // 出场页先入树、进场页后入树：覆盖档靠后画的那张压住停着的旧页，
                    // 平移 / 上下平移两张不重叠，顺序无所谓。出场那一页只画像素，不挂
                    // 选中 / 痕迹 / 放大入口 —— 它正在离开，那三样都属于目标页。
                    if let Some((page, shift)) = outgoing_page {
                        let (tiles, _) = fixed_page_tiles(
                            book,
                            page,
                            usable,
                            viewport_height,
                            content_top,
                            self.prefs.margin_x,
                            false,
                        );
                        let no_viewer: Option<Entity<ImageViewer>> = None;
                        for at in tiles {
                            let Some(request) = at.tile.request.as_ref() else {
                                continue;
                            };
                            let key = request.key.clone();
                            let Some(image) =
                                painted_tile(&atlas, &at.tile, view, cx, motion.scrolling)
                            else {
                                continue;
                            };
                            let index = overlays.len();
                            overlays.push(page_image(
                                at.x + shift.0,
                                at.y + shift.1,
                                at.width,
                                at.height,
                                image,
                                key,
                                &no_viewer,
                                long_press,
                                index,
                                &reader_weak,
                            ));
                        }
                    }
                    // 一页可能摆好几格（多图页 / 对开跨页，或渲染型的一整页）：按**共享高度**
                    // 横成一行，整体居中。一格时这条式子退化成「等比缩进视口」，与普通漫画的
                    // 旧表现逐像素相同。
                    // 命中那把尺只在**字层真的取到**时记：字层还没回来与这一页没字
                    // 在屏上是两件事 —— 前者等后台到货后重画这一帧再记，后者永远不记
                    // （没有字就选不出东西，记了只会让长按落到一块空盒上）。
                    let (tiles, placed) = fixed_page_tiles(
                        book,
                        current,
                        usable,
                        viewport_height,
                        content_top,
                        self.prefs.margin_x,
                        text.is_some(),
                    );
                    // 这一帧的缩放几何（轴心 = 铺满那一页的左上角），再按上限收一次。
                    // 翻页动画在飞的时候倍率已经在 `turn()` 里清回 1x，所以下面两张纸
                    // 都按铺满画；动画之外才真按手指给的倍率映射。
                    let fit = fixed_row_bounds(&tiles);
                    let source_width = zoom_source_width(tiles.iter().map(|at| &at.tile));
                    self.fixed_frame = FixedFrame {
                        origin: (0.0, content_top),
                        pivot: fit.map_or((0.0, content_top), |(origin, _)| origin),
                        page: fit.map_or((usable, viewport_height), |(_, size)| size),
                        area: (f32::from(viewport_size.width), viewport_height),
                        row: (self.prefs.margin_x, usable),
                        total: 0.0,
                        max: fit.map_or(1.0, |(_, size)| max_scale(source_width, size.0)),
                        continuous: false,
                    };
                    // 与连续那一臂同一步：本帧几何收一次，再拿收过的倍率去映射。
                    {
                        let frame = self.fixed_frame;
                        self.fixed_zoom
                            .set_scale(self.fixed_zoom.scale(), frame.max);
                        let pan = self.fixed_zoom.pan();
                        let scale = self.fixed_zoom.scale();
                        self.fixed_zoom.set_pan(frame.clamp_pan(pan, scale));
                    }
                    let zoom = self.fixed_zoom;
                    let pivot = self.fixed_frame.pivot;
                    // 带字的那一格才有选中：它的矩形就是「屏幕点 ↔ 页内字序」那一把尺
                    // 唯一该记的地方。`placed` 是**最终位置**（不含动画位移）：动画期间
                    // 手势让位，落定后位移正好归零，命中永远读这一份。
                    for at in tiles {
                        // 解不出像素的那一格空着，但仍然占位：多图页的排版不该因为
                        // 少一张图就把剩下的往左挤。
                        let Some(request) = at.tile.request.as_ref() else {
                            continue;
                        };
                        let key = request.key.clone();
                        let Some(image) =
                            painted_tile(&atlas, &at.tile, view, cx, motion.scrolling)
                        else {
                            continue;
                        };
                        let rect = zoom.map(
                            Bounds::new(
                                point(px(at.x + incoming_shift.0), px(at.y + incoming_shift.1)),
                                size(px(at.width), px(at.height)),
                            ),
                            pivot,
                        );
                        let selected = text.is_some() && at.tile.renders;
                        // 「看大图」那一格在这一族里按端分岔（`page_image` 顶上写着）：
                        // 手机上它吃的是**长按**，而那正是选中要的手势 —— 于是有字的
                        // 那一页不再挂它（入口挪进浮层那颗按钮），没字的页照旧。
                        // 桌面那一端挂的是**单击**，与拖选不打架（拖过阈值那一下的合成
                        // 点击由 `open_on_tap` 里那一条吃掉），所以原样留着。
                        let no_entry = None;
                        let entry = if selected && long_press {
                            &no_entry
                        } else {
                            &viewer
                        };
                        let index = overlays.len();
                        overlays.push(page_image(
                            f32::from(rect.origin.x),
                            f32::from(rect.origin.y),
                            f32::from(rect.size.width),
                            f32::from(rect.size.height),
                            image,
                            key,
                            entry,
                            long_press,
                            index,
                            &reader_weak,
                        ));
                        // 放大停稳那一档：看得见的那一块按 `REGION_EDGE` 重画，叠在这一页那一张上面。
                        let block = pinned_region(
                            RegionAsk {
                                book,
                                atlas: &atlas,
                                page: current,
                                tile: &at.tile,
                                rect: (at.x, at.y, at.width, at.height),
                                pivot,
                                viewport: (
                                    0.0,
                                    content_top,
                                    f32::from(viewport_size.width),
                                    viewport_height,
                                ),
                                zoom: self.fixed_zoom,
                                now,
                            },
                            &self.region_moved_at,
                            &self.region_pinned,
                            view,
                            cx,
                        );
                        if let Some(pin) = block {
                            let key = pin.request.key.clone();
                            if let Some(image) = painted(&atlas, pin.request, view, cx) {
                                let zoomed = self.fixed_zoom.map(
                                    Bounds::new(
                                        point(px(pin.rect.0), px(pin.rect.1)),
                                        size(px(pin.rect.2), px(pin.rect.3)),
                                    ),
                                    pivot,
                                );
                                overlays.push(page_image(
                                    f32::from(zoomed.origin.x),
                                    f32::from(zoomed.origin.y),
                                    f32::from(zoomed.size.width),
                                    f32::from(zoomed.size.height),
                                    image,
                                    key,
                                    &no_entry,
                                    long_press,
                                    overlays.len(),
                                    &reader_weak,
                                ));
                            }
                        }
                    }
                    // 命中换算读的那一把尺，与上面摆出去的那一格同源。每一帧都写：
                    // 没有字的那一页必须把它清干净，否则留着上一页的矩形，长按会按新页的
                    // 字序去解旧页的盒子 —— 症状是色块整片画在别处。
                    *self.fixed_text_boxes.borrow_mut() = placed
                        .map(|at| (current, zoom.map(at, pivot)))
                        .into_iter()
                        .collect();
                    // 一屏就是一页：选区挂在别的页上就是「翻走了」，按分页那一族同一条规矩
                    // 收掉（那里的选区也必须落在锚章）。不收会留着一份屏上看不见的选中 ——
                    // 菜单条还开着，而它锚的那一段字已经不在这一帧里了。
                    if self
                        .selection
                        .as_ref()
                        .is_some_and(|selected| selected.chapter != current)
                    {
                        self.clear_selection();
                    }
                    // 留在书上的那几条痕迹，与这一帧挑中的那一块。这两层在定版式上必须
                    // **盖在页图之上**：这一族的字就在那张栅格出来的图里，压在它下面等于没画
                    // （可重排那一族正相反 —— 高亮压在字形之下才不吃掉笔画）。
                    //
                    // 动画期间痕迹跟着**进场页**一起走：页图在滑，痕迹停在终点就会读成
                    // 「色块浮在纸外面」。`fixed_text_boxes` 仍记最终位置，手势在动画期间让位，
                    // 落定后位移归零，两边自然重合。
                    let drawn_at = placed.map(|at| {
                        zoom.map(
                            Bounds::new(
                                point(
                                    at.origin.x + px(incoming_shift.0),
                                    at.origin.y + px(incoming_shift.1),
                                ),
                                at.size,
                            ),
                            pivot,
                        )
                    });
                    if let (Some(text), Some(at)) = (&text, drawn_at) {
                        let marks = fixed_marks(
                            &self.annotation_spans,
                            &self.annotations,
                            current,
                            text_key.unwrap_or(0),
                            text,
                            at,
                            &palette,
                        );
                        if !marks.is_empty() {
                            overlays.push(marks_above(marks));
                        }
                        // 选中的那一块：选区挂在它自己那一页上，翻到别页就不再画。
                        let visual = self
                            .selection
                            .clone()
                            .filter(|selected| selected.chapter == current)
                            .map(|selected| {
                                selection::page_visual(
                                    text,
                                    selected.range,
                                    at,
                                    cx.theme().selection,
                                    cx.theme().selection.opacity(HIGHLIGHT_ALPHA),
                                )
                            })
                            .unwrap_or_default();
                        fixed_visual = Some(visual.clone());
                        overlays.push(selection::overlay(visual, &reader_weak));
                    }
                }
            }
            Source::Book(book) => {
                if mode.is_continuous() {
                    // —— 连续滚动：锚章 ± 窗口拼成一条全局坐标 ——
                    // 0. 章盒里正文之前那一截（页眉带 + 安全区）先报进链：`placements` 的
                    //    章高要按**画出来的**那一段量，否则每章末尾都会漏出这一截字，
                    //    下一章的整页底图正好盖上去（见 `Book::chapter_top`）。
                    book.set_chapter_top(content_top);
                    // 1. 邻章补齐入库（0 高占位）：排版作业这就能排进后台队列，
                    //    狂飙滚动时接缝提前两章就铺好了。窗口铺多远要看**视口顶**
                    //    （读者停在长章中后段时，前方那几章也得够一屏多才滚得动）。
                    let __t = crate::ui::features::reader::model::perf::Timer::start();
                    let __before = book.docs_mut().count();
                    book.ensure_window_spans(&self.prefs, viewport_height, self.scroll_y);
                    let __new = book.docs_mut().count().saturating_sub(__before) as u32;
                    let __spans = __t.ns();
                    crate::ui::features::reader::model::perf::add(|frame| {
                        frame.spans_ns += __spans;
                        frame.spans_new += __new;
                    });
                    // 2. 按当前（上一帧的）坐标链记下视口顶：排版收完结果好补偿。
                    let snapshot = book.spine_snapshot(self.scroll_y, chapter_gap);
                    // 3. 排版：锚章先提交（恢复落点等它），邻居随后。连续模式只排版
                    //    不切片（一片高 0 是「不分片」的哨兵值）。
                    //
                    // **递给排版的纵轴基准是整窗高**：连续滚动这一档
                    // `Viewport.viewport_height` 只当排版的 `page_height` 用（切片高是 0，
                    // 不参与），而它决定「整章只有一张图」那一档把章撑到多高。
                    //
                    // 从前这里是「纸高」= 窗高 − `reserve.height`（状态栏 + 手势条），
                    // 理由是背板也按同一个数铺。但背板是**出血图**：它要盖住状态栏与手势条
                    // （`bare_bands` 已经把读数带让开），盒子却是安全区以内那一条 —— 而挑图
                    // 与 `cover/contain` 又都按**整窗**比例算。`cover` 按宽放大之后图比盒子
                    // 高出 `reserve.height`，`background-position: center` 于是上下各裁一半：
                    // 封面顶部被切掉约半个状态栏那么高（读者报的「封面上面被裁切一部分」）。
                    //
                    // 盒子、挑图、`cover/contain` 判定三处从此同一个数（整窗），封面
                    // `0~slim` 那种与屏同比例的一张就是严丝合缝。章高也跟着整窗走，
                    // 「撑到哪一格就铺到哪一格」仍然成立，不会压到下一章头上。
                    let window_h = f32::from(window.viewport_size().height);
                    let __t = crate::ui::features::reader::model::perf::Timer::start();
                    for doc in book.docs_mut() {
                        doc.ensure_viewport(
                            window,
                            cx,
                            Viewport {
                                width: page_box.content.w,
                                height: 0.0,
                                // 连续滚动的 `%` 基准是**整窗**：封面/扉页靠
                                // `height:100%` 撑满一屏，让开页眉页脚会把满版图缩一截。
                                // 分页那一档才是正文盒的高。
                                page_height: Some(window_h),
                                columns: 1,
                            },
                        );
                    }
                    let __layout = __t.ns();
                    crate::ui::features::reader::model::perf::add(|frame| {
                        frame.layout_ns += __layout
                    });
                    // 4–7. 坐标链那一段：补偿、恢复落点、锚章迁移、总高与夹取。
                    let __spine = crate::ui::features::reader::model::perf::Timer::start();
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
                    self.laid_out = crate::ui::features::reader::model::session::LaidOut {
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
                    // 整页画（固定底图章 / 单图页）：判定放在锚章迁移之后，读的就是这一帧
                    // 视口顶所在的那一章。宽高比用整窗：同一本书里 `0.jpg` / `0~slim.jpg`
                    // 谁更像这块屏，就挑谁。
                    let want_aspect =
                        f32::from(viewport_size.width) / f32::from(viewport_size.height).max(1.0);
                    page_art = current_page_art(book, want_aspect);
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
                    // 整页画的背板：**每一章各一张**，跟着这一章滚，铺满整窗。只画当前
                    // 锚章的话，短章/章界会先空出一截白；逐章画、后一章盖前一章，图与图
                    // 之间就不会有缝。固定底图章也走这条 —— `background-attachment:
                    // fixed` 在滚动阅读里要的是「这张图跟着这一章走」，钉在窗口上反而不
                    // 动（读者报的「底图不会跟着滚」）。
                    let size = window.viewport_size();
                    let anchor = book.chapter_index();
                    // 背板铺的是**这一章自己的盒子**：盒子从章基线起，里面先是页眉带那
                    // 一截（`Book::chapter_top`），再是正文。图顶因此就是章盒顶，不让开
                    // `content_top` —— 上一章的正文现在也不会漏进这一格（章高把它算进去了），
                    // 让开只会让两张满版图之间空出一条缝（读者报的「图片之间有间隙」）。
                    let art_top = |base: f32| base - painted_y;
                    // 图高：章盒与「整窗 + 页眉带」里矮的那个。整页画那一档的盒子正好是
                    // 这个数（排版侧撑到整窗，盒子再加 `content_top`），两张满版图于是首尾
                    // 相接；比屏还高的插图章只铺一屏多一点，不溢进下一章。这两个数从前
                    // 都按整窗取，与「章盒里还有一截页眉」差着一个 `content_top` —— 章的
                    // 正文被算进链里之后，铺贴这一侧才跟着对齐。
                    let art_h = |height: f32| height.min(window_h + content_top);
                    // 邻章先画、**锚章最后画**：每张背板都盖住自己这一格，后画的会压住
                    // 前一张的下半截。锚章最后入树，它这一屏的底图才压得住下一章从章首
                    // 开始铺上来的那张（读者报的「封面下半截被下一章的黑底盖住」）。滚动
                    // 换锚时下一章自然接上。
                    let mut anchor_h = 0.0;
                    for placed in book.placements(chapter_gap) {
                        if placed.index == anchor {
                            // 锚章那一张排在最后画，高度先记下来。
                            anchor_h = art_h(placed.height);
                            continue;
                        }
                        let base = placed.base_y;
                        let Some(doc) = book.span_doc(placed.index) else {
                            continue;
                        };
                        let Some(art) = page_art_for_doc(placed.index, doc, want_aspect) else {
                            continue;
                        };
                        let height = art_h(placed.height);
                        if height <= 0.0 {
                            continue;
                        }
                        let top = art_top(base);
                        if top > window_h || top + height < 0.0 {
                            continue;
                        }
                        let pictures =
                            scroll_art_pictures(&art, f32::from(size.width), height, doc.images());
                        if let Some(decor) =
                            crate::ui::features::reader::view::scene::backdrop_decor(
                                &pictures,
                                0.0,
                                top,
                                f32::from(size.width),
                                height,
                                doc.images(),
                            )
                        {
                            backdrops.extend(decoration_child(decor, &atlas, view, cx));
                            neighbour_backdrop = true;
                        }
                    }
                    if let Some(art) = &page_art
                        && let Some(doc) = book.span_doc(art.chapter)
                    {
                        let base = book.base_of(art.chapter, chapter_gap).unwrap_or(0.0);
                        // 锚章与邻章同一口径：图顶 = 这一节盒子的顶（章基线 − 滚量），
                        // 图高 = 盒子与「整窗 + 页眉带」里矮的那个。锚章还没进链时
                        // （`anchor_h == 0`）退回整窗，与从前一样先铺一屏。
                        let height = if anchor_h > 0.0 {
                            anchor_h
                        } else {
                            art_h(window_h)
                        };
                        let pictures =
                            scroll_art_pictures(art, f32::from(size.width), height, doc.images());
                        if let Some(decor) =
                            crate::ui::features::reader::view::scene::backdrop_decor(
                                &pictures,
                                0.0,
                                art_top(base),
                                f32::from(size.width),
                                height,
                                doc.images(),
                            )
                        {
                            backdrops.extend(decoration_child(decor, &atlas, view, cx));
                        }
                    }
                    // 整页画（单图页或固定底图章）一律让读数带透明：底图要铺到状态栏与
                    // 手势条，不透明的带子会在上下各切掉一条。压在画上的字属于这张画，
                    // 不为它留夹在两条带子之间的那一档（判据见下面那一句）。
                    // 连续滚动里「下一张纸」是下面那一屏：只要这一章还没滚到底，就往下预解；
                    // 滚到底之后往回看上一屏。判据只看坐标链的两端，不看章 —— 章界那一头
                    // 由 `prefetch::scroll_targets` 按全局带换到邻章去。
                    prefetch_side = prefetch::side_for_scroll(
                        self.scroll_y,
                        (self.content_height - viewport_height).max(0.0),
                    );
                    let __spine = __spine.ns();
                    crate::ui::features::reader::model::perf::add(|frame| {
                        frame.spine_ns += __spine
                    });
                    // 8. 可见带 → 逐章切场景：每章用自己的排版与图片索引，
                    //    换算只差一个章基线。没排完的章不进链，自然一帧都不画。
                    let band = painted_y..(painted_y + viewport_height);
                    // 读数带只给**没有可读正文**的整页画让路（封面、整页插图页）：这种
                    // 页上的字与底图是同一张画，把画夹进两条带子之间会切掉上下两条。
                    //
                    // 有正文的章反过来 —— 底图只铺内容区、两条带子照常不透明
                    // （2026-11-01 深夜读者改的口径）。否则《诡秘之主》`body.fm1` 那一族
                    // （固定底图 + 二十来屏人物志）滚到哪一屏都全屏：页眉那行章名直接压在
                    // 正文上、正文压进页脚，全屏档的规矩被套到了正文页头上。
                    // 判据是 `Document::has_text`（有非空白字符），不是「有没有字宽」——
                    // 卷首常摆一行 `&nbsp;` 把根撑开，那种仍算没有正文。
                    // 整页画一律让读数带透明：底图铺到状态栏与手势条，字压在画上是
                    // 作者自己的摆法（2026-11-01 深夜读者重新拍板：诡秘分页也要全屏）。
                    bare_bands = page_art.is_some();
                    // 整页画那一章按**窗口**坐标画，可见的不是内容区那 719，而是整扇窗：
                    // 上沿多安全区、下沿多页脚那一段。它的裁剔带要跟着加长，否则页底
                    // 那一截（人物志那几行字正落在那里）会被悄悄裁掉。
                    let window_extra = (f32::from(viewport_size.height) - viewport_height).max(0.0);
                    for placed in book.placements(chapter_gap) {
                        // 这一章的整页画（固定底图 / 单图页）。固定底图还要画正文，
                        // 只有 picture page 才整章跳过；两者的根底图都交给背板，不许在
                        // 版心里再画一遍 —— 那会把整窗背板裁成章高那一块，章界留白。
                        let page_art_here =
                            page_art.as_ref().filter(|art| art.chapter == placed.index);
                        let picture_only = page_art_here.is_some_and(|art| art.picture_only);
                        if picture_only {
                            continue;
                        }
                        let scene_end = if page_art_here.is_some() {
                            band.end + window_extra
                        } else {
                            band.end
                        };
                        if placed.base_y + placed.height < band.start || placed.base_y > scene_end {
                            continue;
                        }
                        let Some(doc) = book.span_doc(placed.index) else {
                            continue;
                        };
                        // 这一章有没有根上的整页底图（判据见 [`page_backdrop`]）。块里那
                        // 一份一律跳过：整窗背板已经由上面的逐章循环画过，再在版心里画一遍
                        // 就是「图多出来一截」。
                        let taken_over = page_backdrop_index(doc);
                        // 整页画的正文按窗口坐标画（背板也是）：顶边从窗口顶起，而不是
                        // 页眉带下面那一格 —— 作者把字摆在画上，两个坐标系差着一个页眉
                        // 加一条安全区。普通页仍是页眉带。
                        let top = if picture_only { 0.0 } else { content_top };
                        // 这一章在排版坐标里的可见带：**只减章基线**，不要再减 `top`。
                        //
                        // 绘制那一句是 `屏 = base + top + line.y − scroll`，所以屏上
                        // `[top, top + 视口高]` 那一段正好对应排版坐标 `[scroll − base,
                        // scroll − base + 视口高]` —— `top` 在两边同时出现，一减就重复了。
                        // 减它的后果是可见带整体上移一截：图片行高、只是被吃掉下半截看不
                        // 出来，文字行矮，屏底就空出两行（2026-11-01 真机「下边距变大」）。
                        let local = local_band(band.start..scene_end, placed.base_y);
                        // 可见行区间向引擎要一次，画与念共用同一个数：两处各要一次，
                        // 念出来的与画出来的就会差一行。
                        let lines = doc.visible_lines(local.clone());
                        if speaking {
                            spoken.extend(spoken_lines(doc, lines.clone()));
                        }
                        let __t = crate::ui::features::reader::model::perf::Timer::start();
                        let mut scene = Scene::build(
                            doc,
                            font_cache,
                            &family,
                            lines,
                            local.clone(),
                            band.start - placed.base_y,
                            // 连续滚动不分片，也就永远不走竖排（`allow_vertical` 那一档），
                            // 长度这一格在横排那条路里根本不参与，递整章的长只为读起来不心虚。
                            doc.height(),
                            self.prefs.margin_x,
                            top,
                            foreground,
                            link,
                            taken_over,
                        );
                        let __scene = __t.ns();
                        let __sh = crate::ui::features::reader::model::perf::Timer::start();
                        shape_runs(&mut scene, doc, window);
                        let __sh = __sh.ns();
                        crate::ui::features::reader::model::perf::add(|frame| {
                            frame.shape_runs_ns += __sh
                        });
                        let (__runs, __images) =
                            (scene.runs.len() as u32, scene.images.len() as u32);
                        crate::ui::features::reader::model::perf::add(|frame| {
                            frame.scene_ns += __scene;
                            frame.scenes += 1;
                            frame.scene_runs += __runs;
                            frame.images += __images;
                        });
                        element.extend(scene.runs);
                        for decor in scene.decorations {
                            column.extend(decoration_child(decor, &atlas, view, cx));
                        }
                        upcoming.extend(scene.upcoming);
                        for image in scene.images {
                            painted_keys.push(image.image.key.clone());
                            let Some(rendered) = painted_picture(&atlas, &image.image, view, cx)
                            else {
                                continue;
                            };
                            // 序号按「这一帧已经摆了几张」取：连续滚动时一屏可能跨两章，
                            // 各自从 0 数会撞同名。
                            let index = overlays.len();
                            let pan = image_pans
                                .get(&image.image.key)
                                .copied()
                                .unwrap_or((0.0, 0.0));
                            // Reeden 那种「图下面一排小点」：先记下这一格的几何与叠标，
                            // 图交出去之后再把点补在它下面（点在图框外面，不能塞进去 ——
                            // 图框是 `overflow_hidden` 的）。
                            let dots = image
                                .stack
                                .filter(|mark| mark.count > 1)
                                .map(|mark| (image.x, image.top + image.height, image.width, mark));
                            // 底下先垫一条「下一张」的卡片边（Reeden 那种堆叠观感）：这一叠
                            // 后面还有一张时，在图的下沿露出 6px 的一条，读者一看就知道能往
                            // 左滑。画在图之前入树，被图压住上半截，露出来的就是那一条。
                            if let Some((x, bottom, w, mark)) = dots
                                && mark.index + 1 < mark.count
                            {
                                overlays.push(stack_peek(x, bottom, w, cx));
                            }
                            overlays.push(image_child(
                                image,
                                rendered,
                                &viewer,
                                long_press,
                                index,
                                &reader_weak,
                                pan,
                                &image_hits,
                                image_hits_frame,
                                placed.index,
                                &stack_hits,
                            ));
                            if let Some((x, bottom, w, mark)) = dots {
                                overlays.push(stack_dots(x, bottom, w, mark, cx));
                            }
                        }
                        let __fills = crate::ui::features::reader::model::perf::Timer::start();
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
                            // 连续滚动这一臂递整带的长：竖排不在这档落地，所以这一格
                            // 只在底色带那条换算里被读一次（见 `highlight_rects`）。
                            band.end - band.start,
                            top,
                            local.clone(),
                            // 连续滚动这一臂只摆一张纸（翻页就是补间那一个 y），没有要裁的
                            // 另一张 ⇒ 不裁。
                            None,
                        );
                        speech_fill(
                            &mut element,
                            doc,
                            window,
                            cx,
                            speech_span
                                .as_ref()
                                .filter(|(chapter, _)| *chapter == placed.index)
                                .map(|(_, range)| range.clone()),
                            self.prefs.margin_x,
                            band.start - placed.base_y,
                            band.end - band.start,
                            top,
                            local,
                            None,
                        );
                        let __fills = __fills.ns();
                        crate::ui::features::reader::model::perf::add(|frame| {
                            frame.fills_ns += __fills
                        });
                    }
                } else {
                    // —— 分页 / 双页：当前页 +（动画时）同章邻页或跨章目标页 ——
                    // 这一档不拼坐标链：章高按「一页」量，页表自己就带着页眉页脚。
                    // 清掉上一档留下的那个数，免得 `placements` 被谁拿去读时多算一截。
                    book.set_chapter_top(0.0);
                    let metrics = page_box.metrics().expect("可重排分页必须有 PageMetrics");
                    let paged = Viewport {
                        width: metrics.column_width,
                        height: viewport_height,
                        page_height: Some(viewport_height),
                        columns,
                    };
                    let column_width = book.document_mut().ensure_viewport(window, cx, paged);
                    let pages = book.document().page_count();
                    // 整页图按整窗比例挑源（单页与双页分别铺满一帧 / 半帧）。
                    let want_aspect =
                        f32::from(viewport_size.width) / f32::from(viewport_size.height).max(1.0);
                    // `background-attachment: fixed` 的底图章按**这一页**摊（见下面 `slots`
                    // 那一趟）：翻页途中两张纸各带各的底图走，平移 / 覆盖才像一整张纸在动，
                    // 而不是「字在滑、画留在原地」。从前这一块只在 `turn.is_none()` 时画，
                    // 于是除了仿真折页（底图烘进了捕获纹理），其余几档一开翻底图就整块消失
                    // ——读者那句「只有仿真翻页是真把一整张屏幕当成一张纸」量的就是它。
                    self.laid_out = crate::ui::features::reader::model::session::LaidOut {
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
                        self.pending_chapter_turn = None;
                    }
                    self.page = self.page.min(pages.saturating_sub(1));
                    // 图预解：这一屏上下紧挨着的那一张纸（章末那一头换到邻章去，见
                    // `prefetch::paged_targets`）。`pages` 双页时是张数，这里不与它挂钩：
                    // 预解只关心「还有没有下一次翻页」。
                    prefetch_side = prefetch::side_for_paged(
                        self.page,
                        pages,
                        book.chapter_index(),
                        book.chapter_count(),
                    );

                    // 邻章预排版：跨章滑动要两张纸同时上屏，目标章必须先有排版与切页。
                    // 当前章排完才铺邻章，避免刚换章时把三章的重排一起压进队列。
                    let current = book.chapter_index();
                    if book.document().is_current() {
                        for delta in [-1isize, 1] {
                            let target = current as isize + delta;
                            if target < 0 || target as usize >= book.chapter_count() {
                                continue;
                            }
                            let target = target as usize;
                            book.ensure_paged_span(target, &self.prefs);
                            if let Some(doc) = book.span_doc_mut(target) {
                                doc.ensure_viewport(window, cx, paged);
                            }
                        }
                    }

                    // 读者在章界按了一下：目标章一排好就起滑。排版没落地时保持当前页
                    // 不动 —— 先切章再等排版会留下白屏，正是跨章瞬切最难看的那个空档。
                    if let Some(delta) = self.pending_chapter_turn {
                        let target = current as isize + delta;
                        if target >= 0
                            && (target as usize) < book.chapter_count()
                            && let Some(doc) = book.span_doc(target as usize)
                            && doc.is_current()
                        {
                            let target = target as usize;
                            let target_pages = doc.page_count().max(1);
                            let target_page = if delta > 0 {
                                0
                            } else {
                                target_pages.saturating_sub(1)
                            };
                            // 起这一趟走 [`start_turn`]，不在这里手摆：从前这里写死
                            // `seq: 0` 再把计数器加一，于是**每一趟**跨章用的都是同一把时钟键
                            // —— 第二次跨章时那把键上还挂着上一趟「已经走完」的状态，
                            // `Presence` 第一帧就给回 1.0，章当场落定。
                            self.pending_chapter_turn = None;
                            let arriving = PageRef {
                                chapter: target,
                                page: target_page,
                            };
                            let started = start_turn(
                                &mut self.turn,
                                &mut self.turn_seq,
                                page_turn,
                                turn_over,
                                mode,
                                Turn {
                                    leaving: Some(PageRef {
                                        chapter: current,
                                        page: self.page,
                                    }),
                                    arriving: Some(arriving),
                                    forward: delta > 0,
                                    ..Default::default()
                                },
                            );
                            // 这一档不挂时钟（「不走动画」就是它）：换章只能当场落。没有这一句，
                            // pending 已经清掉、又没有动画走完那一帧去调 `finish_chapter_turn`
                            // —— 这一步就静默丢了，读者读到的是「章末点两侧没反应」。
                            if !started {
                                land_chapter = Some(arriving);
                            }
                            // 这一帧的 `turn` 已经在上面采过样了；再要一帧才轮到这次滑动。
                            window.request_animation_frame();
                        }
                    }

                    // 要摆的那几页：定住时只有当前这一页；动画在走时**多出另一张纸**
                    // （同章邻页，或跨章的目标页），两张各自怎么摆由这一趟带上来的那一档
                    // 决定 —— 位移与层级都来自 [`turn_animation`]。
                    let rest = Sheet {
                        offset: (0.0, 0.0),
                        z: 0,
                        cut: None,
                    };
                    let mut slots: [(PageRef, Sheet); 2] = [(
                        PageRef {
                            chapter: current,
                            page: self.page,
                        },
                        rest,
                    ); 2];
                    let mut count = 1usize;
                    let mut cross_chapter = false;
                    // 双页折页这一趟的逐栏取舍要在 `if let Some(frame)` 之外读，先把这几格提出来。
                    let mut leaf_turn = false;
                    let mut leaf_column = 0usize;
                    let mut other_column = 1usize;
                    let mut turn_leaving: Option<PageRef> = None;
                    if let Some(frame) = turn {
                        let style = turn_animation::of(frame.style);
                        // 自由边到底在哪一侧（往回翻与「从右往左读的那一本」各反一次）：折页那一档
                        // 的几何全按它判，别的档不看。
                        let free_right =
                            turn_animation::Pose::free_edge_right(frame.forward, frame.rtl);
                        // 「这一屏有多宽」按档答（判据与理由在 [`turn_animation::paper_area`]）：
                        // 双页里只有仿真折页揭靠手指那半张 leaf（它有背面），其余几档是单面的一张
                        // 纸、带不动两栏的新内容，就按整块版心走。纸的位移、手的归一化、逐栏取舍、
                        // 网格的裁剪框四件事都从这一个数出来。单页折页时 `page_area` 已经是
                        // 整张纸（含页眉/页脚），所以这里拿到的就是那一整张。
                        let curl_area =
                            turn_animation::paper_area(style, mode, page_area, free_right);
                        if frame.style == PageTurn::Curl {
                            curl_leaf = Some(curl_area);
                        }
                        // 手是在 `page_area` 里归一化的；双页那张 leaf 要映进它自己的 0..1，
                        // 否则从右缘划到中缝只走了半张 leaf，纸却已经该翻完了。
                        let frame_grip = frame.grip.map(|grip| {
                            if curl_area.spread_leaf {
                                grip.remap(area, curl_area)
                            } else {
                                grip
                            }
                        });
                        // 手与弹回的时钟：走完那一腿把这一笔沿**它自己的方向**拉长到落点，弹回
                        // 那一腿不借手 —— 手指停在松手那一刻那一格，改由 `Pose::revert` 那一个数
                        // 让折线自己退出去。两条腿的第一帧都逐字等于松手那一刻，所以都不跳；
                        // 这一腿自己走了几成由 [`turn_animation::release_clock`] 给（两边的尺
                        // 不一样：那一腿 `progress` 是往 0 退的）。
                        let (grip, revert) = match frame.run {
                            TurnRun::Released { from, revert, .. } => {
                                let u = turn_animation::release_clock(frame.progress, from, revert);
                                if revert {
                                    (frame_grip, Some(u))
                                } else {
                                    (
                                        frame_grip.map(|grip| {
                                            turn_animation::release_grip(
                                                grip, u, free_right, curl_area,
                                            )
                                        }),
                                        None,
                                    )
                                }
                            }
                            _ => (frame_grip, None),
                        };
                        // 摆位层（逐字裁剪）与 GPU 网格（判哪一半已经卷走）从这里**共走一个**入口：
                        // `cylinder_for` 一份几何两处用，「圆柱的轴就是那条折线」不可能在两处走样。
                        // 旧页由页面捕获纹理提供，网格只负责把它卷起来。
                        let pose = turn_animation::Pose {
                            forward: frame.forward,
                            rtl: frame.rtl,
                            grip,
                            revert,
                        };
                        // 只有这张「出场页」的捕获确实完整时才开 GPU 卷曲。捕获也可能因为
                        // 进场横移还没停、尺寸刚变或偏好刚重排而不可用；此时强行用半张/旧的
                        // 纹理，右侧就会采到捕获矩形外。没有可用纹理时退回 CPU 裁页，宁可不
                        // 卷，也不要那条黑带。
                        let leaving_ref = frame.leaving.unwrap_or(PageRef {
                            chapter: current,
                            page: self.page,
                        });
                        let capture_valid = !self.capture_dirty
                            && self.captured_size == Some(viewport)
                            && self.captured_page == Some((leaving_ref.chapter, leaving_ref.page));
                        if frame.style == PageTurn::Curl
                            && capture_valid
                            && let Some(cylinder) =
                                turn_animation::cylinder_for(frame.progress, curl_area, pose)
                        {
                            // 途中那一帧的读数。「页边有时候一条黑带」这种抱怨没法用眼睛验，也
                            // 没法在 headless 里画出来，只能把这几组同源约束一起落进日志：几何
                            // （轴 / 卷径）、版心那块矩形、今天的窗口尺寸与烘纹理那一刻的页号 ——
                            // 哪一处不同源，这里的数字就差哪一处。目标名与 `perf` 那几条同一个：
                            // 设备上的 `logging.targets` 里 `folio_app::perf=debug` 早开着。
                            tracing::debug!(
                                target: "folio_app::perf",
                                free_right,
                                progress = frame.progress,
                                released = matches!(frame.run, TurnRun::Released { .. }),
                                revert = matches!(
                                    frame.run,
                                    TurnRun::Released { revert: true, .. }
                                ),
                                area_left = area.left,
                                area_top = area.top,
                                area_right = area.right,
                                area_bottom = area.bottom,
                                window_w = viewport.0,
                                window_h = viewport.1,
                                page = page_key.1,
                                chapter = page_key.0,
                                captured_ch = captured_ch,
                                captured_pg = captured_pg,
                                captured_w = captured_w,
                                captured_h = captured_h,
                                capture_valid = capture_valid,
                                nx = cylinder.nx,
                                ny = cylinder.ny,
                                axis_d = cylinder.d,
                                radius = cylinder.radius,
                                "折页途中那一帧的读数"
                            );
                            let page_width = curl_area.width();
                            let page_height = curl_area.height();
                            if page_width > 0.0 && page_height > 0.0 {
                                let paper = cx.theme().background.to_rgb();
                                // 顶点与索引只由页宽页高决定，进度改的是 uniform。尺寸没变就
                                // 接着用缓存那一份 —— 每帧重造是 9409 个顶点加 55296 个索引
                                // （近 1 MB）的分配与拷贝，全在这一帧的绘制里跑。
                                let cached = self.curl_mesh.clone().filter(|mesh| {
                                    mesh.width == page_width && mesh.height == page_height
                                });
                                let mesh = match cached {
                                    Some(mesh) => mesh,
                                    None => {
                                        let (vertices, indices) =
                                            folio_curl::build_page_mesh(page_width, page_height);
                                        let built = CurlMesh {
                                            width: page_width,
                                            height: page_height,
                                            vertices: vertices
                                                .into_iter()
                                                .map(|vertex| PageCurlVertex {
                                                    position: vertex.position,
                                                    uv: vertex.uv,
                                                })
                                                .collect::<Vec<_>>()
                                                .into(),
                                            indices: indices.into(),
                                        };
                                        self.curl_mesh = Some(built.clone());
                                        built
                                    }
                                };
                                let page = Bounds::new(
                                    point(px(curl_area.left), px(curl_area.top)),
                                    size(px(page_width), px(page_height)),
                                );
                                // 裁剪框按**整张纸**给：单页折页时就是含页眉/页脚的那一整页，
                                // 双页时仍按整块版心（那张 leaf 要越过书脊落到另一半上，按
                                // leaf 自己剪就把越过中缝的那一片切掉了）。fork 里
                                // `wgpu_renderer/page_curl.rs` 拿 `mesh.bounds` 算出一个矩形再
                                // `set_scissor_rect`（钉的 rev 8959066，`encode_mesh_pass` 尾上那
                                // 一句）。取样不受影响：shader 的 uv 由页面局部坐标算，纸盖到
                                // 屏幕哪儿都不换采样点，背面那张纹理仍按 leaf 那一格烘
                                // （`curl_leaf`）。
                                let clip_area = if curl_area.spread_leaf {
                                    area
                                } else {
                                    curl_area
                                };
                                let clip = Bounds::new(
                                    point(px(clip_area.left), px(clip_area.top)),
                                    size(px(clip_area.width()), px(clip_area.height())),
                                );
                                gpu_curl_mesh = Some(PageCurlMesh {
                                    bounds: clip,
                                    page_bounds: page,
                                    vertices: mesh.vertices,
                                    indices: mesh.indices,
                                    uniforms: PageCurlUniforms {
                                        page_origin: [curl_area.left, curl_area.top],
                                        // 采样原点：先按本帧的页框填，画那一趟的回调里再换成
                                        // 「烘那一刻」的那一个（`curl_bake_origin`）。层在动时
                                        // 两者不同，混用就会采到纹理里未写的区域。
                                        capture_origin: [curl_area.left, curl_area.top],
                                        back_capture_origin: [curl_area.left, curl_area.top],
                                        // 卷曲网格永远采正面那张（出场页）；进场页仍实时画。
                                        incoming: 0.0,
                                        page_size: [page_width, page_height],
                                        // `window_size` 与 `scale_factor` 由
                                        // `paint_page_curl_mesh` 按真实窗口与 DPI 覆盖，
                                        // 这里填什么都不参与成像。
                                        window_size: [0.0, 0.0],
                                        axis_normal: [cylinder.nx, cylinder.ny],
                                        axis_d: cylinder.d,
                                        radius: cylinder.radius,
                                        camera_distance: (page_width.max(page_height) * 3.0)
                                            .max(600.0),
                                        scale_factor: 1.0,
                                        back_color: [paper.r, paper.g, paper.b, 1.0],
                                        // 光从**卷起那一侧**的斜上方来（`+n` 是已经卷走的那一半），
                                        // 影子才会落在还平摊着的那半边（`-n` 一侧）上；固定一个
                                        // 世界方向的 `[-0.35, -0.25, …]` 只对正向翻页成立，
                                        // 往回翻时影子会落到已经卷走的那一边、屏上一片干净。
                                        //
                                        // 水平分量只给 0.10：影子的偏移量是 `水平分量 × 纸抬起来的
                                        // 高度`，给到 0.25 时柱顶那一段的影还会从折痕外探出六十来
                                        // 像素；0.10 只探出二十来像素 —— 影基本压在纸底下，只在
                                        // 折痕那一线露出一点。竖直分量稍大一点，影往下方沉。
                                        light_dir: [
                                            cylinder.nx * 0.10,
                                            cylinder.ny * 0.10 - 0.08,
                                            1.0,
                                            0.0,
                                        ],
                                        cone_a: [
                                            cylinder.cone_apex_s,
                                            cylinder.cone_far_s,
                                            cylinder.cone_weight,
                                            cylinder.cone_min,
                                        ],
                                        cone_b: [cylinder.cone_max, 0.0, 0.0, 0.0],
                                    },
                                });
                            }
                        }
                        // 折页那一档不再给 CPU 影子：纸面与圆柱受光都在 GPU 那张网格里
                        // （见 `curl.rs`、`page_curl.wgsl`）。`shades_at` 的默认实现给回空清单，
                        // 所以这一句对别的几档就是「照旧」。`pose` 是上面拼的那一份：带着**补间
                        // 之后**的手与弹回的时钟，网格与摆位层读的都是它。
                        shades = style.shades_at(frame.progress, curl_area, pose);
                        let [incoming, outgoing] = style.sheets_at(frame.progress, curl_area, pose);
                        let leaving_ref = frame.leaving.unwrap_or(PageRef {
                            chapter: current,
                            page: self.page,
                        });
                        let arriving_ref = frame.arriving.unwrap_or(PageRef {
                            chapter: current,
                            page: self.page,
                        });
                        // 同一张纸既当出场又当进场时不拆栏：正常走不到（`turn()` 只按
                        // `TurnPlan` 里真会换页那几支起趟），但真走到了，拆栏会把两栏都判给
                        // 捕获层，主场景一片空白。
                        // 这一趟是不是「双页里只揭靠手指那一张 leaf」—— 只有仿真折页是，判据在
                        // [`turn_animation::paper_area`]，这里读它盖上的那一格。它管的是**哪一栏
                        // 在纸上**：其余几档在双页里两栏同属那张移动的纸，谁都不许停在原地。
                        leaf_turn = curl_area.spread_leaf && leaving_ref != arriving_ref;
                        leaf_column = if free_right { 1 } else { 0 };
                        other_column = 1 - leaf_column;
                        turn_leaving = Some(leaving_ref);
                        turn_arriving = Some(arriving_ref);
                        // 这一趟要烘的背面（捕获层 2）：揭起来那张纸的背面 = 目标跨页的另一半
                        // （双页）/ 目标页整张（单页）。网格真画出来了才有采样它的人；同一趟里
                        // 目标不变、纹理跨帧留着，所以只对**没烘过**的那一格烘一趟。
                        // 跨章那一趟的目标章可能还没排完版：那一栏排不出内容，背面就只是一张
                        // 清成纸色的空纹理（与同一帧底下那张目标页同样空着是一致的）。
                        if gpu_curl_mesh.is_some() {
                            let key = (
                                arriving_ref.chapter,
                                arriving_ref.page,
                                if leaf_turn { other_column } else { 0 },
                            );
                            if self.captured_back != Some((key, viewport)) {
                                back_key = Some(key);
                            }
                        }
                        if arriving_ref.chapter != current {
                            // 跨章：源页往外走、目标页往里走，各用各的章。
                            cross_chapter = true;
                            slots[0] = (leaving_ref, outgoing);
                            slots[1] = (arriving_ref, incoming);
                            count = 2;
                        } else {
                            // 同章：`slots[0]` 恒是进场那一张 —— 命中与 `page_shift` 归它。
                            // 入树顺序另按 `z` 排（见下），两者不必一致：覆盖档要把这张
                            // 抬到停着的旧页之上，而它始终是「这一屏的当前页」。
                            slots[0] = (arriving_ref, incoming);
                            if leaving_ref != arriving_ref {
                                slots[1] = (leaving_ref, outgoing);
                                count = 2;
                            }
                        }
                        // 单页 C 方案只摆进场那一张：旧页已经在捕获纹理里，由 GPU 网格卷。
                        // 双页不能只摆一张：静态页（当前跨页的另一半）与底层新页（下一跨页的
                        // 同侧半页）都得留着，网格只盖住被揭起来的那一张 leaf。两张纸都不裁，
                        // 由下面的逐栏跳过把 leaf 正面与背面取掉。
                        if gpu_curl_mesh.is_some() && mode != chrome::PageMode::Spread {
                            let arriving_slot = if slots[0].0 == arriving_ref { 0 } else { 1 };
                            slots[0] = slots[arriving_slot];
                            count = 1;
                        }
                    }
                    // 当前页这一帧偏了多少。命中换算读这一格（`to_layout` 里减掉它）：
                    // 跨章时两张纸的章不同，命中在 [`Self::turn_in_flight`] 已经让位，
                    // 这里给 0 是让旧坐标不再被减第二次。
                    self.page_shift = if cross_chapter {
                        (0.0, 0.0)
                    } else {
                        slots[0].1.offset
                    };

                    // 入树顺序就是绘制层级（后入树的压在上面）。两张同层时保持上面的
                    // 原顺序，所以平移仍然读成一条纸带整体在走。
                    let mut order = [0usize, 1];
                    if count == 2 && slots[0].1.z > slots[1].1.z {
                        order.swap(0, 1);
                    }
                    // 出场页这一帧改用静止时烘好的整页纹理。左右平移 / 上下平移 / 覆盖
                    // 三档从前逐帧重画字形，文字会随子像素档位在帧间换位图（读者看到的
                    // 是「图上的字在闪」）；纹理搬过去就没有这一层重栅格化。
                    //
                    // 只换出场页：捕获纹理只有正面这一张，进场页还是实时画。仿真折页
                    // 那张由下面的卷曲网格自己画，不走这里。整页画（`page_art`）不进
                    // 捕获层，遇到它退回实时画，免得平摊出来是一张纸色。
                    let capture_dirty = self.capture_dirty;
                    let captured_size = self.captured_size;
                    let captured_page = self.captured_page;
                    let page_no = self.page;
                    let flat_capture = turn.and_then(|frame| {
                        if frame.style == PageTurn::Curl || fixed_layout || page_art.is_some() {
                            return None;
                        }
                        let leaving = frame.leaving.unwrap_or(PageRef {
                            chapter: current,
                            page: page_no,
                        });
                        let capture_valid = !capture_dirty
                            && captured_size == Some(viewport)
                            && captured_page == Some((leaving.chapter, leaving.page));
                        if !capture_valid {
                            return None;
                        }
                        // 整页画（`background-attachment: fixed` / 单图页）不进捕获层：
                        // 它铺在元素树外面，烘进纹理的只有纸色。遇到它退回实时画，
                        // 否则翻页途中那张画会整块消失。
                        if book.span_doc(leaving.chapter).is_none_or(|doc| {
                            page_art_for_doc(leaving.chapter, doc, want_aspect).is_some()
                        }) {
                            return None;
                        }
                        let (_, sheet) = slots
                            .iter()
                            .take(count)
                            .find(|(page, _)| *page == leaving)?;
                        let paper = cx.theme().background.to_rgb();
                        Some(flat_capture_mesh(
                            sheet_area,
                            area,
                            sheet.offset,
                            sheet.cut,
                            paper,
                            0.0,
                        ))
                    });
                    // 进场页走同一套平摊纹理，方向相反：**第一帧**照旧实时画，同时把一份
                    // 副本烘进捕获层 3（`incoming_bake`）；烘好之后（键对上）整页跳过、
                    // 改画平摊网格（`flat_incoming`）。排除项与出场页同一批 —— 仿真折页
                    // 的进场页不动、没有子像素重栅格化的问题，定版式与整页画不进捕获层。
                    // 目标章还没排完版（跨章那一趟的常客）时退回实时画，下一帧再试。
                    let captured_incoming = self.captured_incoming;
                    incoming_bake = turn.and_then(|frame| {
                        if frame.style == PageTurn::Curl || fixed_layout || page_art.is_some() {
                            return None;
                        }
                        let arriving = frame.arriving?;
                        let key = (arriving.chapter, arriving.page);
                        if captured_incoming == Some((key, viewport)) {
                            return None;
                        }
                        let doc = book.span_doc(arriving.chapter)?;
                        if !doc.is_current()
                            || page_art_for_doc(arriving.chapter, doc, want_aspect).is_some()
                        {
                            return None;
                        }
                        let (_, sheet) = slots
                            .iter()
                            .take(count)
                            .find(|(page, _)| *page == arriving)?;
                        Some((key, sheet.offset))
                    });
                    flat_incoming = turn.and_then(|frame| {
                        if frame.style == PageTurn::Curl || fixed_layout || page_art.is_some() {
                            return None;
                        }
                        let arriving = frame.arriving?;
                        let capture_valid = !capture_dirty
                            && captured_size == Some(viewport)
                            && captured_incoming
                                == Some(((arriving.chapter, arriving.page), viewport));
                        if !capture_valid {
                            return None;
                        }
                        if book.span_doc(arriving.chapter).is_none_or(|doc| {
                            page_art_for_doc(arriving.chapter, doc, want_aspect).is_some()
                        }) {
                            return None;
                        }
                        let (_, sheet) = slots
                            .iter()
                            .take(count)
                            .find(|(page, _)| *page == arriving)?;
                        let paper = cx.theme().background.to_rgb();
                        Some(flat_capture_mesh(
                            sheet_area,
                            area,
                            sheet.offset,
                            sheet.cut,
                            paper,
                            1.0,
                        ))
                    });
                    for which in order.into_iter().take(count) {
                        let (page_ref, at) = slots[which];
                        if flat_capture.is_some() && turn_leaving == Some(page_ref) {
                            let mesh = flat_capture.clone().expect("上一行刚判过 Some");
                            let layer = (self.enter_shift, layer_top);
                            let bake_origin = self.curl_bake_origin.clone();
                            column = column.child(
                                div()
                                    .id(ElementId::from("reader-flat-capture"))
                                    .test_support()
                                    .absolute()
                                    .inset_0()
                                    .child(
                                        canvas(
                                            |_, _, _| {},
                                            move |bounds, _, window, _| {
                                                let mut mesh = mesh.clone();
                                                // 与卷曲网格同一套坐标：`bounds.origin` 是窗口坐标，
                                                // 先还掉阅读层自己的平移。
                                                let origin = point(
                                                    bounds.origin.x - px(layer.0),
                                                    bounds.origin.y - px(layer.1),
                                                );
                                                mesh.bounds.origin = point(
                                                    mesh.bounds.origin.x + origin.x,
                                                    mesh.bounds.origin.y + origin.y,
                                                );
                                                mesh.page_bounds.origin = point(
                                                    mesh.page_bounds.origin.x + origin.x,
                                                    mesh.page_bounds.origin.y + origin.y,
                                                );
                                                mesh.uniforms.page_origin[0] += f32::from(origin.x);
                                                mesh.uniforms.page_origin[1] += f32::from(origin.y);
                                                mesh.uniforms.capture_origin[0] +=
                                                    f32::from(origin.x);
                                                mesh.uniforms.capture_origin[1] +=
                                                    f32::from(origin.y);
                                                // 采样原点换成烘那一刻的那一个：纹理里被写过的
                                                // 只有当时那一块，层在动时两者不同。
                                                if let Some((x, y)) = bake_origin.get() {
                                                    mesh.uniforms.capture_origin = [x, y];
                                                }
                                                window.paint_page_curl_mesh(mesh);
                                            },
                                        )
                                        .absolute()
                                        .inset_0(),
                                    ),
                            );
                            continue;
                        }
                        if flat_incoming.is_some() && turn_arriving == Some(page_ref) {
                            let mesh = flat_incoming.clone().expect("上一行刚判过 Some");
                            let layer = (self.enter_shift, layer_top);
                            let bake_origin = self.incoming_bake_origin.clone();
                            column = column.child(
                                div()
                                    .id(ElementId::from("reader-flat-incoming"))
                                    .test_support()
                                    .absolute()
                                    .inset_0()
                                    .child(
                                        canvas(
                                            |_, _, _| {},
                                            move |bounds, _, window, _| {
                                                let mut mesh = mesh.clone();
                                                // 与出场页那张平摊网格同一套坐标：先还掉阅读
                                                // 层自己的平移，再把采样原点换成烘那一刻的。
                                                let origin = point(
                                                    bounds.origin.x - px(layer.0),
                                                    bounds.origin.y - px(layer.1),
                                                );
                                                mesh.bounds.origin = point(
                                                    mesh.bounds.origin.x + origin.x,
                                                    mesh.bounds.origin.y + origin.y,
                                                );
                                                mesh.page_bounds.origin = point(
                                                    mesh.page_bounds.origin.x + origin.x,
                                                    mesh.page_bounds.origin.y + origin.y,
                                                );
                                                mesh.uniforms.page_origin[0] += f32::from(origin.x);
                                                mesh.uniforms.page_origin[1] += f32::from(origin.y);
                                                mesh.uniforms.capture_origin[0] +=
                                                    f32::from(origin.x);
                                                mesh.uniforms.capture_origin[1] +=
                                                    f32::from(origin.y);
                                                if let Some((x, y)) = bake_origin.get() {
                                                    mesh.uniforms.capture_origin = [x, y];
                                                }
                                                window.paint_page_curl_mesh(mesh);
                                            },
                                        )
                                        .absolute()
                                        .inset_0(),
                                    ),
                            );
                            continue;
                        }
                        let Some(doc) = book.span_doc(page_ref.chapter) else {
                            continue;
                        };
                        // 这一页的整页底图（`background-attachment: fixed` 那一档）。**按页**
                        // 摊而不是按屏摊：平移 / 上下平移 / 覆盖这几档翻页时两张纸同时在屏上，
                        // 各带各的底图才是一整张纸在走。仿真折页例外 —— 旧页那张已经在捕获
                        // 纹理里（网格自己把它卷走），主场景只铺目标页那张，否则卷起来底下
                        // 还压着一张不动的旧底图。
                        //
                        // 只认排好版的那一帧（`is_current`）：跨章目标还没排完时它既没有
                        // 种子也没有行盒，铺出来是一张上一版留下的画。
                        let art = if doc.is_current() {
                            page_art_for_doc(page_ref.chapter, doc, want_aspect)
                        } else {
                            None
                        };
                        let art_drawn = art.as_ref().is_some_and(|art| !art.picture_only)
                            && (gpu_curl_mesh.is_none() || turn_arriving == Some(page_ref));
                        // 这一页的根底图由阅读器当整窗背板画（就是下面这一块），版心里那份
                        // 别再画一遍 —— 位置还差着一个页眉与安全区。**按页**判，不是按屏：
                        // 翻页途中两张纸是两章。
                        let taken_over = page_backdrop_index(doc);
                        if art_drawn && let Some(art) = &art {
                            let size = window.viewport_size();
                            let width = f32::from(size.width);
                            let height = f32::from(size.height);
                            if let Some(decor) =
                                crate::ui::features::reader::view::scene::backdrop_decor(
                                    &art.pictures,
                                    0.0,
                                    0.0,
                                    width,
                                    height,
                                    doc.images(),
                                )
                            {
                                // 位移挂在这一层上、里面那张画按 0 画：平移 / 覆盖的位移
                                // 与正文用的是同一个数（`Sheet::offset`），宿主测试也量得到
                                // 这张画到底走没走 —— 从前它只在静止帧画，翻页一开始就整个
                                // 消失，读者看到的是「字在滑，纸没动」。
                                backdrops.push(
                                    div()
                                        .absolute()
                                        .left(px(at.offset.0))
                                        .top(px(at.offset.1))
                                        .w(px(width))
                                        .h(px(height))
                                        .id(page_art_element_id(page_ref.chapter, page_ref.page))
                                        .test_support()
                                        .children(decoration_child(decor, &atlas, view, cx))
                                        .into_any_element(),
                                );
                            }
                            page_picture = true;
                            // 底图铺到了状态栏与手势条那两条：读数带得透明，不然会在画上
                            // 盖出两块底色（这一页自己的带子按 `art_drawn` 判）。
                            bare_bands = true;
                        } else if count == 2 && art.is_none() {
                            // 另一张纸的整页画已经铺上整窗（`page_picture` 让正文盒的底色
                            // 让开了），这一页自己却没有画：给它垫一块不透明的纸色，否则
                            // 另一章那张底图会从这一页的字底下透上来（跨章翻页的现场）。
                            let size = window.viewport_size();
                            backdrops.push(
                                div()
                                    .absolute()
                                    .left(px(at.offset.0))
                                    .top(px(at.offset.1))
                                    .w(px(f32::from(size.width)))
                                    .h(px(f32::from(size.height)))
                                    .bg(cx.theme().background)
                                    .into_any_element(),
                            );
                        }
                        let pages = doc.pages();
                        let Some(page) = pages.get(page_ref.page) else {
                            continue;
                        };
                        for index in 0..columns {
                            let Some(slice) = page.column(index) else {
                                continue;
                            };
                            if slice.empty {
                                continue;
                            }
                            // 分片的“页高”与 `PageBox.content` 必须是同一个数。真机上
                            // 出现过「正文落到页脚下面」，这一条在 release 里只记一笔，
                            // 不改变绘制；debug 宿主测试里它会把越界的那一页指出来。
                            if page_ref.chapter == book.chapter_index()
                                && slice.height > page_box.content.h + 1.0
                            {
                                tracing::warn!(
                                    target: "folio_app::perf",
                                    page = page_ref.page,
                                    slice_h = slice.height,
                                    content_h = page_box.content.h,
                                    start = slice.lines.start,
                                    end = slice.lines.end,
                                    "page slice taller than the content box"
                                );
                            }
                            // 这一栏在**物理**上靠左还是靠右：判据一律读 `slice.column`，不读
                            // `index` —— RTL 的书 `slices()` 按逻辑栏序给，两者是反的。
                            let is_leaf = leaf_turn && slice.column == leaf_column;
                            let is_other = leaf_turn && slice.column == other_column;
                            // 双页里新跨页的**另一半**这一趟不进主场景：那是被揭起来的叶子落下去
                            // 之后才出现的一面，由背面那张纹理演（下面 `to_back` 把它烘进层 2）。
                            // 单面那几档（平移 / 上下平移 / 覆盖）不再有这一格 —— 它们在双页里按
                            // 整块版心走，两栏同属那张移动的纸，缺任何一栏读者就看到「一半先变」。
                            // 折页那一趟这一栏归谁：
                            //  - 揭起来那张纸的正面（leaf 那一栏）在 GPU 路径由网格从捕获纹理里
                            //    取，主场景不再画它；CPU 兜底（纹理不可用那一帧）仍画，按折线裁字。
                            //  - 背面那一栏（目标跨页的另一半）任何情况下都不进主场景 —— 它是纸
                            //    的背面，只该出现在卷起来的那一片上。
                            let skip_main = leaf_turn
                                && ((turn_arriving == Some(page_ref) && is_other)
                                    || (gpu_curl_mesh.is_some()
                                        && turn_leaving == Some(page_ref)
                                        && is_leaf));
                            // 这一栏要不要画进捕获层 2。`back_key` 只在纹理对不上这一趟的目标
                            // 那一格时才是 `Some`：同一趟只烘一帧，后面那些帧用纹理里留着的那张。
                            let to_back =
                                back_key.is_some() && turn_arriving == Some(page_ref) && !is_leaf;
                            // 这一栏要不要画进捕获层 3（进场页烘焙那一帧的副本）：只认平摊
                            // 翻页（折页那几档 `incoming_bake` 恒为 `None`），整页收、不裁。
                            let to_incoming =
                                incoming_bake.is_some() && turn_arriving == Some(page_ref);
                            if skip_main && !to_back {
                                continue;
                            }
                            // 这一栏的字按哪一条折线裁（判据与它的理由在 [`column_cut`]）。
                            let cut = column_cut(leaf_turn, is_leaf, &at);
                            // 停在原地的那半页**不带位移**：双页里只有 leaf 那一栏在纸上走，
                            // 另一栏是读者的「还没翻到的那一页」，位置就是它的最终位置。
                            let (shift_x, shift_y) = if is_other { (0.0, 0.0) } else { at.offset };
                            // 栏位从 `PageBox` 来：`content.x` 已经让过页边距，
                            // `column(slice.column)` 与引擎分片用的是同一对除法/位移。
                            let x = page_box.column(slice.column).x + shift_x;
                            let top = content_top + shift_y;
                            // 这一栏是整页图吗：是的话图源按屏比例挑，图本身不排进
                            // 版心，改成铺满这一页（单页整窗、双页半窗）的纸面图。
                            // 整页图这一档与连续滚动统一：只有**这一章没有可读正文**时
                            // 才把那一页拉满屏。正文页里「这一页恰好只有一张图」也按
                            // 内容区摆（滚动档就是那样），两边观感一致。
                            let picture = slice.picture_page.first().map(|base| {
                                let href = doc
                                    .images()
                                    .closest_portrait_variant(base, want_aspect)
                                    .unwrap_or_else(|| base.clone());
                                let page_w = f32::from(viewport_size.width) / columns as f32;
                                let page_x = page_w * slice.column as f32 + shift_x;
                                let contain = page_art_contain(doc, &href, want_aspect);
                                (href, page_x, page_w, shift_y, contain)
                            });
                            // 背面那一栏要横向挪进 leaf 那一格：背面纹理按 leaf 那块矩形烘、网格
                            // 也按 leaf 的尺寸采，而这一栏算出来的 x 是它自己那一栏的位置。
                            if to_back && leaf_turn {
                                back_dx = column_offset(column_width, gap, leaf_column)
                                    - column_offset(column_width, gap, slice.column);
                            }
                            // 分页/双页这一臂的「在屏」就是这一片自己的行区间（片是引擎切的，
                            // 一片一栏）。两栏按 `index` 依次接上去：`slices()` 的顺序是
                            // 「页优先、页内按栏」的**逻辑**栏序，所以 RTL 的书念出来也是
                            // 逻辑顺序 —— 与那一栏画在左边还是右边无关。只为**上屏**的那一栏
                            // 收集：背面那一栏不在屏上，念出来就是把同一句话念两遍。
                            if speaking && !skip_main {
                                spoken.extend(spoken_lines(doc, slice.lines.clone()));
                            }
                            let mut scene = Scene::build(
                                doc,
                                font_cache,
                                &family,
                                slice.lines.clone(),
                                slice.origin_y..(slice.origin_y + slice.height + 1.0),
                                slice.origin_y,
                                slice.height,
                                x,
                                top,
                                foreground,
                                link,
                                taken_over,
                            );
                            let __sh = crate::ui::features::reader::model::perf::Timer::start();
                            shape_runs(&mut scene, doc, window);
                            let __sh = __sh.ns();
                            crate::ui::features::reader::model::perf::add(|f| {
                                f.shape_runs_ns += __sh
                            });
                            // 整页图：清掉排版层那张流内图，换成铺满这一页的纸面图；
                            // 外面再压一层同图的背板，专门把状态栏 / 手势条那两条也铺到。
                            // 背板只在这一页上屏时加（leaf 被捕获带走的那一帧不再画它，
                            // 否则纸已经卷起来了、顶上那条还留着原图）。
                            let is_picture = picture.is_some();
                            if let Some((href, page_x, page_w, art_top, contain)) = picture {
                                page_picture = true;
                                bare_bands = true;
                                scene.images.clear();
                                if let Some(image) = doc.images().decode_request(&href) {
                                    let page_h = f32::from(viewport_size.height);
                                    let fit = if contain {
                                        crate::ui::foundation::images::ObjectFit::Contain
                                    } else {
                                        crate::ui::foundation::images::ObjectFit::Cover
                                    };
                                    scene.images.push(VisibleImage {
                                        x: page_x,
                                        top: art_top,
                                        width: page_w,
                                        height: page_h,
                                        image,
                                        footnote: None,
                                        fit,
                                        // 这一张是「铺满这一页的整页图」，不是哪一叠里的一张。
                                        stack: None,
                                    });
                                    if !skip_main && turn.is_none() {
                                        let size = if contain {
                                            BackgroundSize::Contain
                                        } else {
                                            BackgroundSize::Cover
                                        };
                                        let picture =
                                            crate::ui::features::reader::view::scene::full_page_picture(
                                                &href, size,
                                            );
                                        if let Some(decor) =
                                            crate::ui::features::reader::view::scene::backdrop_decor(
                                                &[picture],
                                                page_x,
                                                art_top,
                                                page_w,
                                                page_h,
                                                doc.images(),
                                            )
                                        {
                                            backdrops
                                                .extend(decoration_child(decor, &atlas, view, cx));
                                        }
                                    }
                                }
                            }
                            // 读数带：一页一份，跟着这一页的位移走。单页时就是一整条；
                            // 双页时左右各半条 —— 左页念位置，右页念百分比，章名只留在
                            // 左页，避免同一组数在跨页上印两遍。Curl 仍由捕获层里的那一份
                            // 负责（见 `front_bands`），这里不重复画。
                            if !skip_main && !curl_turn {
                                let readouts = bands::readouts_for(
                                    doc,
                                    book.chapter_title_at(page_ref.chapter).into(),
                                    page_ref.page,
                                    page_percent.clone(),
                                    cx,
                                );
                                let page_w = f32::from(viewport_size.width) / columns as f32;
                                let page_x = page_w * slice.column as f32;
                                let mut half = readouts;
                                if columns > 1 {
                                    if slice.column == 0 {
                                        half.percent = SharedString::default();
                                    } else {
                                        half.chapter = SharedString::default();
                                        half.position = SharedString::default();
                                    }
                                }
                                page_bands.push(
                                    div()
                                        .absolute()
                                        .left(px(page_x + shift_x))
                                        .top(px(shift_y))
                                        .w(px(page_w))
                                        .h_full()
                                        .child(bands::overlay_page(
                                            &page_box,
                                            &half,
                                            // 这一页自己有整页画时读数带透明：画铺到了状态栏
                                            // 那一条，不透明的带子会在顶上盖出一块底色。
                                            is_picture || art_drawn,
                                            cx,
                                        ))
                                        .into_any_element(),
                                );
                                // 进场页副本里的那一份按**静止位置**摆（不带位移）：副本
                                // 外层的包裹 div 会把位移抵消掉，纹理里的带子落在最终位置。
                                if to_incoming {
                                    incoming_bands.push(
                                        div()
                                            .absolute()
                                            .left(px(page_x))
                                            .top(px(0.0))
                                            .w(px(page_w))
                                            .h_full()
                                            .child(bands::overlay_page(
                                                &page_box,
                                                &half,
                                                is_picture || art_drawn,
                                                cx,
                                            ))
                                            .into_any_element(),
                                    );
                                }
                            }
                            // 背面那一份不裁：整张纸的背面都得在纹理里，卷到哪儿就采到哪儿。
                            // 克隆只到 run 这一层（`shaped` 是 `Arc<LineLayout>`），不重跑 shaping。
                            if to_back {
                                back_element.extend(scene.runs.iter().cloned());
                            }
                            // 进场页那一份同口径：烘焙那一帧整页收进层 3，之后帧由网格采。
                            if to_incoming {
                                incoming_element.extend(scene.runs.iter().cloned());
                            }
                            if !skip_main {
                                // 折页：这一张纸按它**自己那一条**裁剪收字 —— 进场那一张只画
                                // 已经空出来的那一侧，出场那一张只画还平摊着的那一侧。裁剪与
                                // 折面用的是同一条折线，所以影子收掉的那一刻字也正好收干净，
                                // 不会出现「影子没了、字还压着走」的那一帧。
                                if let Some(cut) = cut {
                                    for run in scene.runs.iter_mut() {
                                        run.cut = Some(cut);
                                    }
                                }
                                element.extend(scene.runs);
                            }
                            // 块级装饰（`background-color`、`border`、`<hr>`）也跟着那张纸
                            // 走：中心落在裁掉那一侧就整块不画。字是逐点裁的，而一个装饰块
                            // 是一个矩形、没有「裁一半」的画法 —— 逐像素的多边形裁剪要的是
                            // gpui 没有的东西（`ContentMask` 只有矩形）。留白块比伸到隔壁那
                            // 张纸上的色块好读。捕获层里那两份副本照单全收（不裁）。
                            for decor in scene.decorations {
                                if capture_front {
                                    front_decor.extend(decoration_child(
                                        decor.clone(),
                                        &atlas,
                                        view,
                                        cx,
                                    ));
                                }
                                if to_back {
                                    back_children.extend(decoration_child(
                                        decor.clone(),
                                        &atlas,
                                        view,
                                        cx,
                                    ));
                                }
                                if to_incoming {
                                    incoming_children.extend(decoration_child(
                                        decor.clone(),
                                        &atlas,
                                        view,
                                        cx,
                                    ));
                                }
                                if skip_main {
                                    continue;
                                }
                                if cut.is_some_and(|cut| {
                                    !cut.keeps(
                                        decor.x + decor.width * 0.5,
                                        decor.top + decor.height * 0.5,
                                    )
                                }) {
                                    continue;
                                }
                                column.extend(decoration_child(decor, &atlas, view, cx));
                            }
                            // 两张栏的图都要摆进同一份 `overlays`；跨章滑动时目标章那几张
                            // 也按「这一帧在场」记账，帧末回收就不会把还在画的图放掉。
                            for image in scene.images {
                                // 中心落在折线哪一侧，就按那一侧决定画不画。整张一起收而不
                                // 是切一半：切一半要逐像素的多边形裁剪，而 gpui 的 `ContentMask`
                                // 只有矩形（gpui-pre-0.3.9 `src/window.rs:2226` 那条注释）。
                                // 但 `painted` 照问 —— 那一问就是「这一帧在场」的记账，跳过它
                                // 会让被折走的图在 17 帧里被判离场，露出来时再白上传一趟。
                                let shown = cut.is_none_or(|cut| {
                                    cut.keeps(
                                        image.x + image.width * 0.5,
                                        image.top + image.height * 0.5,
                                    )
                                });
                                painted_keys.push(image.image.key.clone());
                                let Some(rendered) =
                                    painted_picture(&atlas, &image.image, view, cx)
                                else {
                                    continue;
                                };
                                // 捕获层里那两份用不带手势的纯 `img`：同一屏摆两个同 id 的元素，
                                // 命中区就叠了一份，「看大图」那一击会落到纹理的副本上。
                                if capture_front {
                                    front_images.push(plain_image(
                                        image.x,
                                        image.top,
                                        image.width,
                                        image.height,
                                        rendered.clone(),
                                    ));
                                }
                                if to_back {
                                    back_children.push(plain_image(
                                        image.x,
                                        image.top,
                                        image.width,
                                        image.height,
                                        rendered.clone(),
                                    ));
                                }
                                if to_incoming {
                                    incoming_children.push(plain_image(
                                        image.x,
                                        image.top,
                                        image.width,
                                        image.height,
                                        rendered.clone(),
                                    ));
                                }
                                if skip_main || !shown {
                                    continue;
                                }
                                let index = overlays.len();
                                let pan = image_pans
                                    .get(&image.image.key)
                                    .copied()
                                    .unwrap_or((0.0, 0.0));
                                // 叠图那一叠的「下面还有一张」提示与一排小点：与连续滚动
                                // 那一档同一套（`stack_peek` / `stack_dots`）。分页这一臂
                                // 从前只画当前那张，叠起来的几张看起来就是一整张 ——
                                // 读者在分页档根本看不出还能横滑。
                                let dots = image.stack.filter(|mark| mark.count > 1).map(|mark| {
                                    (image.x, image.top + image.height, image.width, mark)
                                });
                                if let Some((x, bottom, w, mark)) = dots
                                    && mark.index + 1 < mark.count
                                {
                                    overlays.push(stack_peek(x, bottom, w, cx));
                                }
                                overlays.push(image_child(
                                    image,
                                    rendered,
                                    &viewer,
                                    long_press,
                                    index,
                                    &reader_weak,
                                    pan,
                                    &image_hits,
                                    image_hits_frame,
                                    page_ref.chapter,
                                    &stack_hits,
                                ));
                                if let Some((x, bottom, w, mark)) = dots {
                                    overlays.push(stack_dots(x, bottom, w, mark, cx));
                                }
                            }
                            let __fills = crate::ui::features::reader::model::perf::Timer::start();
                            // 正面那一份副本不用在这儿再算一遍批注：它整份克隆 `element`
                            // （见下面 `capture_copy`），痕迹与高亮跟着一起过去。背面那一份
                            // 是另一页的内容，得自己算一趟（同样不裁）。
                            if !skip_main {
                                annotation_fills(
                                    &mut element,
                                    doc,
                                    window,
                                    cx,
                                    &self.annotation_spans,
                                    &self.annotations,
                                    &palette,
                                    page_ref.chapter,
                                    x,
                                    slice.origin_y,
                                    slice.height,
                                    top,
                                    slice.origin_y..(slice.origin_y + slice.height + 1.0),
                                    cut,
                                );
                                speech_fill(
                                    &mut element,
                                    doc,
                                    window,
                                    cx,
                                    speech_span
                                        .as_ref()
                                        .filter(|(chapter, _)| *chapter == page_ref.chapter)
                                        .map(|(_, range)| range.clone()),
                                    x,
                                    slice.origin_y,
                                    slice.height,
                                    top,
                                    slice.origin_y..(slice.origin_y + slice.height + 1.0),
                                    cut,
                                );
                            }
                            if to_back {
                                annotation_fills(
                                    &mut back_element,
                                    doc,
                                    window,
                                    cx,
                                    &self.annotation_spans,
                                    &self.annotations,
                                    &palette,
                                    page_ref.chapter,
                                    x,
                                    slice.origin_y,
                                    slice.height,
                                    top,
                                    slice.origin_y..(slice.origin_y + slice.height + 1.0),
                                    None,
                                );
                                speech_fill(
                                    &mut back_element,
                                    doc,
                                    window,
                                    cx,
                                    speech_span
                                        .as_ref()
                                        .filter(|(chapter, _)| *chapter == page_ref.chapter)
                                        .map(|(_, range)| range.clone()),
                                    x,
                                    slice.origin_y,
                                    slice.height,
                                    top,
                                    slice.origin_y..(slice.origin_y + slice.height + 1.0),
                                    None,
                                );
                            }
                            // 进场页副本的批注痕迹与读屏段：与背面同一套「自己算一趟」。
                            if to_incoming {
                                annotation_fills(
                                    &mut incoming_element,
                                    doc,
                                    window,
                                    cx,
                                    &self.annotation_spans,
                                    &self.annotations,
                                    &palette,
                                    page_ref.chapter,
                                    x,
                                    slice.origin_y,
                                    slice.height,
                                    top,
                                    slice.origin_y..(slice.origin_y + slice.height + 1.0),
                                    None,
                                );
                                speech_fill(
                                    &mut incoming_element,
                                    doc,
                                    window,
                                    cx,
                                    speech_span
                                        .as_ref()
                                        .filter(|(chapter, _)| *chapter == page_ref.chapter)
                                        .map(|(_, range)| range.clone()),
                                    x,
                                    slice.origin_y,
                                    slice.height,
                                    top,
                                    slice.origin_y..(slice.origin_y + slice.height + 1.0),
                                    None,
                                );
                            }
                            let __fills = __fills.ns();
                            crate::ui::features::reader::model::perf::add(|f| {
                                f.fills_ns += __fills
                            });
                        }
                    }
                }
            }
        }
        // 单页仿真折页时，卷起来的是**整张纸**（含页眉/页脚），而纸底下露出来的是目标那一页。
        // 所以主场景里的两条读数带要念目标页；它们必须在卷曲网格**之前**入树，网格才盖得住
        // 卷着的那张纸，而读者又能从纸底下看见目标页的章名与页码。
        let whole_sheet_curl = gpu_curl_mesh.is_some() && mode != chrome::PageMode::Spread;
        let band_readouts = if whole_sheet_curl {
            turn_arriving.map_or_else(
                || bands::readouts_of(self, cx),
                |at| bands::readouts_at(self, at.chapter, at.page, cx),
            )
        } else {
            bands::readouts_of(self, cx)
        };
        if whole_sheet_curl {
            column = column.child(bands::overlay_in(&page_box, &band_readouts, bare_bands, cx));
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
        let mut visual = fixed_visual.unwrap_or_else(|| SelectionVisual {
            color: cx.theme().selection,
            ..Default::default()
        });
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
                    // 章内排版坐标 = 全局 y − 章基线（与上面那句 `local_band` 同一口径：
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
                        doc,
                        window,
                        cx,
                        sub,
                        self.prefs.margin_x,
                        band_top,
                        // 连续滚动永远走横排（竖排只在分页那一档落地），长度这一格不参与。
                        band_bottom - band_top,
                        content_top,
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
                            doc,
                            window,
                            cx,
                            sub,
                            // 与 `Scene::build` 那两个参数同源：正文偏了多少，高亮就跟着偏。
                            self.prefs.margin_x
                                + column_offset(width, gap, slice.column)
                                + self.page_shift.0,
                            slice.origin_y,
                            slice.height,
                            content_top + self.page_shift.1,
                            // 带说的是推进轴：递分片自己那一段，比从首末行盒再折一次准，
                            // 而横排下这两个数是同一个区间（片的长就是末行底减首行顶）。
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
        // 菜单条锚在首行高亮之上，放不下就掉到末行高亮之下；横向则对着选区中线。
        let menu_anchor = (
            visual.highlights.first().map(|rect| f32::from(rect.top())),
            visual
                .highlights
                .last()
                .map(|rect| f32::from(rect.bottom())),
        );
        let menu_x = visual
            .highlights
            .first()
            .zip(visual.highlights.last())
            .map(|(first, last)| (f32::from(first.center().x) + f32::from(last.center().x)) * 0.5);
        // 捕获层 1（正面）那一份副本：字形与批注痕迹整份克隆（`shaped` 是 `Arc<LineLayout>`，
        // 克隆只是引用计数，不重跑 shaping），块级装饰与图片在上面那个循环里各收了一份。
        // 选中件不带 —— 高亮与把手是这一次交互的临时物，烘进纹理就跟着纸一起卷走了。
        let front_copy = capture_front.then(|| element.capture_copy());
        // 捕获层 1 里的两条读数带：它们现在也是纸的一部分。按**当前页**念（这张纹理正是
        // 当前页的正面）；折页时它跟着纸一起卷走，露出来的目标页读数带在主场景里另画。
        let front_bands = capture_front.then(|| {
            let readouts = bands::readouts_of(self, cx);
            bands::overlay_in(&page_box, &readouts, bare_bands, cx)
        });
        // 外壳进场那 280 ms 的横移：捕获层里的副本按 `-enter_shift` 画在**最终位置**上，捕获
        // 矩形也按最终位置取（[`capture_region`]）。两处必须同一个数 —— 副本画在最终位置而矩形
        // 按当下位置取，烘出来的就是错位的一页。
        let enter_shift = self.enter_shift;
        // 捕获层里的那两份副本。主 pass 只画 `order < CAPTURE_ORDER_BASE`，层里的东西不上屏、
        // 只被捕获 pass 取走，所以它可以画在与上屏那一份不同的位置上（见上面 `enter_shift`）。
        //
        // 入树必须排在字形元素**之前**：`log_paint` 会把本帧那一份账 `take()` 走，副本晚了就
        // 并进下一帧去（见 `perf::log_capture_paint`）。两份也不会同帧出现 —— 正面只在定住
        // 那一帧烘，背面只在动画在飞且目标还没烘过时烘。
        //
        // 两份都走 [`capture_layer`]：切层与画子树在同一个元素的那一趟 `paint` 里，**不靠
        // 兄弟元素的绘制顺序**。从前这里是「闸 → 内容 → 闸」三个兄弟，闸那一步只要被跳过
        // （`canvas` 没上屏、重放把 `SetCaptureLayer` 丢了），副本就落进主 pass，与正文逐字
        // 重合地再画一遍 —— 读起来就是「整页字印了两遍」。
        if let Some(copy) = front_copy {
            column = column.child(capture_layer(
                1,
                div()
                    .absolute()
                    .left(px(-enter_shift))
                    .top(px(0.0))
                    .size_full()
                    .children(front_decor)
                    .child(copy)
                    .children(front_images)
                    .children(front_bands),
            ));
        }
        if back_key.is_some() && curl_leaf.is_some() {
            // 背面那张纹理是目标页：它的读数带也念目标页。只有整张纸那一档把带子卷进去；
            // 双页的 leaf 只盖半张纸，带子留在主场景里不动。
            let back_bands = whole_sheet_curl
                .then(|| bands::overlay_in(&page_box, &band_readouts, bare_bands, cx));
            column = column.child(capture_layer(
                2,
                div()
                    .absolute()
                    .left(px(back_dx - enter_shift))
                    .top(px(0.0))
                    .size_full()
                    .children(back_children)
                    .child(back_element.capture())
                    .children(back_bands),
            ));
        }
        // 捕获层 3（进场页）：烘焙那一帧的副本整页收。副本里的 runs / 装饰 / 图都已带着
        // 进场页这一帧的位移，包裹 div 按 `-offset` 把它退回静止位置、再按 `-enter_shift`
        // 落到最终位置 —— 与正面副本同一把尺（正面烘焙时位移本来就是 0，所以只让横移）。
        if let Some((_, offset)) = incoming_bake {
            column = column.child(capture_layer(
                3,
                div()
                    .absolute()
                    .left(px(-offset.0 - enter_shift))
                    .top(px(-offset.1))
                    .size_full()
                    .children(incoming_children)
                    .child(incoming_element.capture())
                    .children(incoming_bands),
            ));
        }
        element = element.selection(visual);
        if matches!(self.source, Source::Book(_)) {
            element = element.reader(Some(cx.entity().downgrade()));
        }
        // 正文硬裁到 `PageBox.content`：页眉/页脚是带子，不是压在字上的第二层。
        // 外层给裁切矩形，内层把元素原点还回窗口坐标 —— 字形 run 用的是窗口坐标，
        // 多包一层不能让它整体平移一个 `content`。定版式 `content == paper`，
        // 这一层对它等于没裁。
        let clip = page_box.content;
        column = column.child(
            div()
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
        // 这一档要盖在两张纸**之上**的那几块：折页的纸面与它的四道阴影，覆盖/层叠的前缘
        // 投影与纸边光。它们都不属于任何一张纸 —— 两张纸的字形 runs 烘在同一个元素里，
        // 纸与纸之间没有可插入的绘制层 —— 所以只能作为一组盖上去的多边形来画。形状、渐变
        // 方向、浓淡全由 `turn_animation` 算好，这一头一个几何都不做。
        //
        // 层级是字形之上、图片之下：这些影子本来就落在纸面上，而图是纸面上的另一层内容。
        if gpu_curl_mesh.is_none() && !shades.items().is_empty() {
            let paper = cx.theme().background;
            let layer = (self.enter_shift, layer_top);
            column = column.child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        // 折面与投影的坐标全是**阅读页局部**（`shades` 由 `tree_area` 那一块
                        // 长出来），而 `bounds.origin` 是窗口坐标：把这一层自己的平移还掉。
                        let origin =
                            point(bounds.origin.x - px(layer.0), bounds.origin.y - px(layer.1));
                        for shade in shades.items() {
                            // 零面积与零墨都是「这一帧这一块不存在」：折到尽头折面镜像到
                            // 页外去、投影走到页的左缘自己被裁没，两处都靠这一步收干净。
                            if shade.shape.is_empty() || shade.ink <= 0.0 {
                                continue;
                            }
                            let base = match shade.tint {
                                // 纸面是唯一不透明的一块。有它在那儿，那一片才读成「纸折
                                // 过去了」，而不是「字一排排消失了」。
                                Tint::Paper => paper,
                                Tint::Dark => hsla(0., 0., 0., shade.ink),
                                Tint::Light => hsla(0., 0., 1., shade.ink),
                            };
                            let color: Background = match shade.angle {
                                // 渐变方向也由那一头给：它指的是「离开折线（或离开前缘）
                                // 的那一侧」，所以浓的那端总正好贴在自己的边上。
                                Some(angle) => linear_gradient(
                                    angle,
                                    linear_color_stop(base, 0.),
                                    linear_color_stop(base.opacity(0.), 1.),
                                ),
                                None => base.into(),
                            };
                            paint_fold(window, &shade.shape, origin, color);
                        }
                    },
                )
                .absolute()
                .inset_0(),
            );
        }
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
        // 烘一张旧页纹理：整张纸（含页眉/页脚），原点按这一栏在窗口里的位置补齐。纹理是整窗
        // 大小、只有这一块被写过，而网格取 uv 用的是 `(页框原点 + 局部坐标) / 窗口` —— 两处
        // 必须同一个数，差一个页边距就读成「纸上的字与底下那半页叠在一起」。
        //
        // 无论读者当前选的是哪一档都烘整张纸：仿真折页可以随时被选上，而那一刻不该要求
        // 先等一次重烘；其余几档只是多烘两条带子的字，代价在捕获那一趟里可以忽略。
        if capture_needed {
            let region = capture_region(sheet_area, enter_shift);
            // 本次请求的那一页。绘制回调把它记进 `capture_committed`，下一帧 `render` 才把
            // `captured_page` 推成它 —— 而**这次请求是哪一页**只有这里知道：绘制回调里那份
            // 页号是上一次 `render` 读到的，中间若已经翻过一页，回调会把旧页写成「已烘」。
            let requested_key = page_key;
            let committed = std::rc::Rc::clone(&self.capture_committed);
            let layer = (self.enter_shift, layer_top);
            column = column.child(
                canvas(|_, _, _| {}, {
                    let bake_origin = std::rc::Rc::clone(&self.curl_bake_origin);
                    move |bounds, _, window, _| {
                        // `region` 是阅读页坐标，`bounds.origin` 是窗口坐标：先把这一层自己的
                        // 平移（进场横移）还掉，两者才同一套尺。
                        let shaped = shape_in_window(bounds.origin, layer, region);
                        let at = shaped.origin;
                        // 捕获纹理只有整窗大：页框有一截在窗外，那一截就永远烘不进去，而网格仍按
                        // 完整页宽采 UV，边上便采到矩形外（真机读数是一条黑色横带）。此时**不请求
                        // 捕获**，下一帧重试。进场那一段的横移已经在 `region` 里抵消掉了，走到这儿
                        // 还不合身的只剩「窗口本身比这一页窄」（分屏、桌面把窗口拖小）。
                        if !in_viewport(shaped, viewport) {
                            tracing::warn!(
                                target: "folio_app::debug",
                                x = f32::from(at.x),
                                y = f32::from(at.y),
                                w = f32::from(shaped.size.width),
                                h = f32::from(shaped.size.height),
                                vw = viewport.0,
                                vh = viewport.1,
                                layer_y = layer.1,
                                bounds_y = f32::from(bounds.origin.y),
                                "正面捕获被拒：矩形出窗"
                            );
                            return;
                        }
                        // 记下这一刻的原点：画网格那一趟取 uv 要用它，而不是本帧的。
                        bake_origin.set(Some((f32::from(at.x), f32::from(at.y))));
                        // 捕获矩形落在窗口哪儿 —— 这一格与网格取 uv 用的 `page_origin` 必须是
                        // 同一个数，差多少像素，那条边上就有多少像素是采在未写区域（读成黑）。
                        tracing::debug!(
                            target: "folio_app::perf",
                            x = f32::from(at.x),
                            y = f32::from(at.y),
                            w = f32::from(shaped.size.width),
                            h = f32::from(shaped.size.height),
                            "烘这一页：捕获矩形"
                        );
                        window.capture_page_region_with_color(shaped, capture_clear);
                        committed.set(Some((requested_key, viewport)));
                        // 提交落在**下一帧**的 `render` 开头，所以发出请求的这一帧之后还得再有
                        // 一帧。这一帧没人买：`capture_page_region_with_color` 只把矩形写进
                        // `next_frame.scene`，gpui 也不为触摸或滚动事件重画，而翻页落定那一帧
                        // 补的 `notify` 只买得到「发出请求」的这一帧。少了下面这一发，提交一直
                        // 挂着落不进 `captured_page` ⇒ 仿真折页那道门（`curl_capture_ready`）
                        // 永远不开 ⇒ 读者的下一划在 `claim_swipe` 里整趟被吞。与
                        // `chapter_layout` 那条「作业在飞就必须再要一帧」同一个口径。
                        window.request_animation_frame();
                    }
                })
                .absolute()
                .inset_0(),
            );
        }
        // 背面那张纹理：揭起来那张纸的背面 = 目标跨页的另一半（双页）/ 目标页整张（单页）。
        // 内容与请求都排在**这一帧**：捕获 pass 在主 pass 之前编码，所以同一帧画进层 2 的东西，
        // 这一帧的网格就采得到，不必为它多等一帧。
        //
        // 目标章还没排完版那一趟排不出内容（上面那个循环里 `span_doc` 给 `None` 就跳过了），
        // 这一帧仍然请求捕获 —— 纹理被清成纸色，背面是一张干净的纸，而不是上一趟留下的字。
        if let Some(key) = back_key
            && let Some(leaf) = curl_leaf
        {
            let region = capture_region(leaf, enter_shift);
            let committed = std::rc::Rc::clone(&self.capture_back_committed);
            let layer = (self.enter_shift, layer_top);
            column = column.child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        // 与正面同一把尺：`region` 是阅读页坐标，`bounds.origin` 是窗口坐标。
                        let shaped = shape_in_window(bounds.origin, layer, region);
                        let at = shaped.origin;
                        if !in_viewport(shaped, viewport) {
                            return;
                        }
                        tracing::debug!(
                            target: "folio_app::perf",
                            chapter = key.0,
                            page = key.1,
                            column = key.2,
                            dx = back_dx,
                            x = f32::from(at.x),
                            y = f32::from(at.y),
                            w = f32::from(shaped.size.width),
                            h = f32::from(shaped.size.height),
                            "烘背面那一页：捕获矩形"
                        );
                        window.capture_page_region_back_with_color(shaped, capture_clear);
                        committed.set(Some((key, viewport)));
                    },
                )
                .absolute()
                .inset_0(),
            );
        }
        // 进场页那张纹理（捕获层 3）：烘焙那一帧与副本同一帧发出请求，矩形按**静止位置**
        // 取（与正面同一把尺 [`capture_region`]）。动画在飞，帧自然会来 —— 不像正面那张
        // 要为「静止帧之后没有下一帧」补一发 `request_animation_frame`。
        if let Some((key, _)) = incoming_bake {
            let region = capture_region(sheet_area, enter_shift);
            let committed = std::rc::Rc::clone(&self.capture_incoming_committed);
            let bake = std::rc::Rc::clone(&self.incoming_bake_origin);
            let layer = (self.enter_shift, layer_top);
            column = column.child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        let shaped = shape_in_window(bounds.origin, layer, region);
                        if !in_viewport(shaped, viewport) {
                            tracing::warn!(
                                target: "folio_app::debug",
                                chapter = key.0,
                                page = key.1,
                                "进场页捕获被拒：矩形出窗"
                            );
                            return;
                        }
                        bake.set(Some((
                            f32::from(shaped.origin.x),
                            f32::from(shaped.origin.y),
                        )));
                        tracing::debug!(
                            target: "folio_app::perf",
                            chapter = key.0,
                            page = key.1,
                            x = f32::from(shaped.origin.x),
                            y = f32::from(shaped.origin.y),
                            w = f32::from(shaped.size.width),
                            h = f32::from(shaped.size.height),
                            "烘进场页：捕获矩形"
                        );
                        window.capture_page_region_incoming_with_color(shaped, capture_clear);
                        committed.set(Some((key, viewport)));
                    },
                )
                .absolute()
                .inset_0(),
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
            let layer = (self.enter_shift, layer_top);
            column = column.child(
                canvas(|_, _, _| {}, {
                    let bake_origin = std::rc::Rc::clone(&self.curl_bake_origin);
                    // 正面纹理按整张纸（top = 0）烘，网格只卷其中一张 leaf：采样原点要往右让出
                    // leaf 的起点（单页那一档 leaf 就是整张纸，横纵都是 0；双页是半张、且背面
                    // 那块仍按正文上沿烘）。两个偏移各管一轴。
                    let leaf_dx = curl_leaf.map_or(0.0, |leaf| leaf.left - area.left);
                    let leaf_top = curl_leaf.map_or(0.0, |leaf| leaf.top);
                    move |bounds, _, window, _| {
                        // 走到这里必须已经切回主场景：漏了收尾那道闸，网格的 primitive 会排进
                        // 捕获那一段、主 pass 一个都不画 —— 翻页就读成「字消失了但没有纸在卷」，
                        // 而且不报错。
                        debug_assert_eq!(window.capture_layer(), 0, "捕获层没切回主场景就画了网格");
                        let mut mesh = mesh.clone();
                        // `page_bounds` 是从 `page_area` 出来的**阅读页局部**坐标，而
                        // `bounds.origin` 是窗口坐标；采样原点与几何原点都按窗口坐标算，
                        // 所以这里也走同一套：先把这一层自己的平移还掉。
                        let origin =
                            point(bounds.origin.x - px(layer.0), bounds.origin.y - px(layer.1));
                        mesh.bounds.origin = point(
                            mesh.bounds.origin.x + origin.x,
                            mesh.bounds.origin.y + origin.y,
                        );
                        mesh.page_bounds.origin = point(
                            mesh.page_bounds.origin.x + origin.x,
                            mesh.page_bounds.origin.y + origin.y,
                        );
                        mesh.uniforms.page_origin[0] += f32::from(origin.x);
                        mesh.uniforms.page_origin[1] += f32::from(origin.y);
                        mesh.uniforms.capture_origin[0] += f32::from(origin.x);
                        mesh.uniforms.capture_origin[1] += f32::from(origin.y);
                        // 采样原点换成**烘那一刻**的那一个：那张纹理里被写过的只有当时那一块。
                        // 没烘过（`None`）就与几何同源 —— 那一帧渲染器本来也会因为缺纹理
                        // 跳过这一趟。
                        if let Some((x, y)) = bake_origin.get() {
                            // `capture_origin` 是**页左上角**在窗口里的位置，不是捕获矩形
                            // 的左上角。正面烘的是整张纸，而被卷的那一页在双页里只是半张：
                            // 页原点要从捕获矩形原点加上 `leaf` 的偏移。横 = 半页宽，纵 =
                            // 正文上沿（单页时两个都是 0）。
                            mesh.uniforms.capture_origin = [x + leaf_dx, y + leaf_top];
                            // 背面纹理只烘 `curl_leaf` 那一格，页左上角就是那一格的左上角；
                            // 与正面用的是同一个偏移，所以同一个数。
                            mesh.uniforms.back_capture_origin = [x + leaf_dx, y + leaf_top];
                        }
                        // 两条原点并排打：它们不等就是层在动，而这正是那条黑带的成因。
                        tracing::debug!(
                            target: "folio_app::perf",
                            x = mesh.uniforms.page_origin[0],
                            y = mesh.uniforms.page_origin[1],
                            sample_x = mesh.uniforms.capture_origin[0],
                            sample_y = mesh.uniforms.capture_origin[1],
                            w = mesh.uniforms.page_size[0],
                            h = mesh.uniforms.page_size[1],
                            "画网格：页框原点与采样原点"
                        );
                        window.paint_page_curl_mesh(mesh);
                    }
                })
                .absolute()
                .inset_0(),
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
        let listener = cx.listener(|this: &mut Self, event: &ScrollWheelEvent, _window, cx| {
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
        let had_page_bands = !page_bands.is_empty();
        let mut body = div()
            .id("folio-reader")
            // 宿主的测试要按 id 把滚轮事件投到这里。非 `test-support` 构建里这一层是透明的
            // （`Observed` 原样转交内部元素），所以真机上不多花一个节点。
            .test_support()
            .size_full()
            .relative()
            .on_scroll_wheel(listener)
            .children(backdrops)
            .child(column.overflow_hidden())
            .children(page_bands);
        // 不是整张纸折页的那几档，读数带照旧压在正文与浮层之上。整张纸那一档要它们在
        // 卷曲网格**之下**（见上面 `whole_sheet_curl` 那一段），所以这里不再挂。
        // 单页分页已经有每页一份的带子（`page_bands`），这一份不再重复。
        if !had_page_bands && !whole_sheet_curl {
            // 全局那一份带子直接吃 `PageBox`：连续档是占位带，定版式是纯文字浮层。
            // 定版式没有任何底色 —— 页图整屏，读数只是浮在照片上的两行字。
            let band_bare = bare_bands || page_box.bands == BandPolicy::Overlay;
            body = body.child(bands::overlay_in(&page_box, &band_readouts, band_bare, cx));
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

/// 读屏（TalkBack 那一类）要的那份「这一屏的字」：`lines` 里每一行的可见文本，空行不占一格。
///
/// 文本向引擎要（`Layout::line_text`，与状态栏和那一族测试断言同一个来源），界面不重算：
/// 行内图片位置上的哨兵字符它已经摘掉，念出来不会带一串乱码。列表标记（`::marker` 那个
/// 「1.」）也不在这一份里 —— 它与选区、命中、批注锚点那三条路同一个口径，都不进可见文本，
/// 把它念进去等于让同一个节点上两套坐标系并存。
///
/// 只在有读屏连着的时候调（判据是调用点那一句 `Window::is_a11y_active`）：一屏几十行、
/// 每行一次分配，压在画字那条最热的路径上就是每帧白花的钱。
pub(in crate::ui::features::reader) fn spoken_lines(
    doc: &Document,
    lines: Range<usize>,
) -> Vec<SharedString> {
    lines
        .filter_map(|ix| {
            // 一叠图只念「现在这一张」那一条注：一叠五张全念，就是把五句话连着念一遍。
            if doc
                .layout()
                .lines
                .get(ix)
                .and_then(|line| line.stack)
                .is_some_and(|mark| doc.stack_current(mark.id) != mark.index)
            {
                return None;
            }
            let text = doc.layout().line_text(ix);
            (!text.is_empty()).then(|| SharedString::from(text))
        })
        .collect()
}

/// 把要盖在纸上的那一个多边形填进场景。顶点已经带在元素坐标里（与字形的 `run.x` /
/// `run.baseline` 同一套），所以这里只加一个 `origin` 就完事，不必再补页边距 —— 补了就是
/// 旧版那一处「影子按 `margin + at` 摆、裁剪按 `at` 判」，两处差着一个页边距。
///
/// 为什么不用 `paint_quad`：quad 只能轴对齐，而这一档要的全部就是那条**斜的**折线（以及
/// 层叠那条跟着纸一起沉的上缘）；用 quad 画出来的就是旧版那一根竖棍。`build()` 失败只可能
/// 是退化到零面积的那一片，画不出东西不该让整帧失败 —— 与 `paint.rs` 里单个字形画不出去时
/// 同一个取舍。
fn paint_fold(
    window: &mut Window,
    poly: &turn_animation::Poly,
    origin: Point<Pixels>,
    color: impl Into<Background>,
) {
    let mut path = PathBuilder::fill();
    let mut first = true;
    poly.each(|x, y| {
        let at = point(origin.x + px(x), origin.y + px(y));
        if first {
            path.move_to(at);
            first = false;
        } else {
            path.line_to(at);
        }
    });
    path.close();
    if let Ok(built) = path.build() {
        window.paint_path(built, color);
    }
}

/// 把一整棵子树只画进捕获层 `layer`（1 = 旧页正面，2 = 旧页背面）的壳。
///
/// 从前这是三个兄弟元素：`canvas` 闸 → 内容 → `canvas` 闸，靠**绘制顺序**保证「内容在层里」。
/// 那条不变量只要有一处被跳过（闸那个 `canvas` 没上屏、重放时把 `SetCaptureLayer` 丢了），
/// 内容就落进主 pass：克隆出来的那一页字形与正文逐字重合地再画一遍 —— 读起来是「整页字印了
/// 两遍」，而它只在「这一页还没烘好」的那几帧出现（见 [`capture_wanted`]），所以是「有的页面
/// 有、有的页面没有」。
///
/// 做成一个元素之后，切层与画子树在**同一趟 `paint`** 里：不存在「层切了但内容没画」或
/// 「内容画了但层没切」的姿势，层号也不可能从这一帧漏进主场景。
struct CaptureLayer {
    layer: u8,
    child: AnyElement,
}

impl Element for CaptureLayer {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        // 壳没有自己的状态，也没有自己的布局盒：`request_layout` 直接把子树那一格交出去。
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        // 进层之前必须停在主场景：层里再套一层，外层那一段会整块丢字。
        let before = window.capture_layer();
        debug_assert_eq!(
            before, 0,
            "捕获层套层了：切到 {} 之前场景在 {before}",
            self.layer
        );
        window.set_capture_layer(self.layer);
        self.child.paint(window, cx);
        // 无论子树画了什么都切回来：漏了这一步，后面所有内容都会排进捕获段、主 pass 一个都不画。
        window.set_capture_layer(0);
    }
}

impl IntoElement for CaptureLayer {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// [`CaptureLayer`] 的构造口。`layer` 只能是 1 或 2 —— 0 是主场景，没有要夹的东西。
fn capture_layer(layer: u8, child: impl IntoElement) -> AnyElement {
    debug_assert!(layer != 0, "capture_layer(0) 没有意义：主场景不需要壳");
    CaptureLayer {
        layer,
        child: child.into_any_element(),
    }
    .into_any_element()
}

/// 捕获层里的一张图：与 [`image_child`] 同一块矩形，但**不带**任何手势与元素 id。
///
/// 同一屏摆两个同 id 的元素，命中区就叠了一份 —— 「看大图」那一击可能落到纹理的副本上。
/// 而层里这一份根本不上屏（主 pass 跳过它的 order），它只要被捕获 pass 取走，画出来就够。
fn plain_image(
    x: f32,
    top: f32,
    width: f32,
    height: f32,
    rendered: Arc<RenderImage>,
) -> AnyElement {
    div()
        .absolute()
        .left(px(x))
        .top(px(top))
        .w(px(width))
        .h(px(height))
        .child(img(rendered).size_full())
        .into_any_element()
}

/// 这一帧要不要（重新）烘那张旧页纹理。
///
/// 五个输入各自挡掉一种情况的：
///
/// - `animating`：动画期间场景里已经多了一张新页，烘出来的就不是正在被翻的那张。
/// - `continuous`：连续滚动根本不是「一页」，也没有卷起来的那张纸要采它。
/// - `content_ready`：**这一章的排版已经是当前键那一代**（`Document::is_current`）。
///   没排完时画出来的只有纸色，而那张纸色一旦被记成「已烘」就不会再烘第二次 ——
///   第一次进书划那一下仿真翻页整页白，就是这一格缺着（见 `view/body` 里 `page_key`
///   那一段注释）。它与 `dirty` 是**并列**的两条：`dirty` 说「手里那张过期了」，
///   这一条说「手里那张（或马上要烘的那张）根本不会有内容」。
/// - `dirty` / `captured`：偏好、主题、尺寸、摆法变了要重烘；同一页且不脏就不重复跑
///   整趟捕获 pass。
fn capture_wanted(
    animating: bool,
    continuous: bool,
    content_ready: bool,
    dirty: bool,
    captured: bool,
) -> bool {
    !animating && !continuous && content_ready && (dirty || !captured)
}

/// 平面捕获网格：把烘好的整页纹理当一张不会变形的纸搬。
///
/// 左右平移 / 上下平移 / 覆盖三档没有 GPU 卷曲，从前是逐帧重画字形的 —— 文字在移动中
/// 会随子像素档位换位图，看起来发虚/闪。这里把静止帧已经烘好的整页纹理用卷曲网格的
/// **平摊分支**画出来：`axis_d` 放到页宽的四倍，所有点的 `past` 都 < 0，顶点与片元
/// 都走「原样采样纸面」那一支。网格只用四个角，不建曲面。
///
/// `cut` 只在覆盖档有：两张纸会重叠，旧页只画它还没被盖住的那一侧。网格的裁剪框只能
/// 是矩形，而覆盖的折线正好是竖直/水平，所以这里把矩形收到折线那一侧；斜折线只有
/// 仿真折页有，不走这一路。
fn flat_capture_mesh(
    sheet: turn_animation::Area,
    cut_area: turn_animation::Area,
    offset: (f32, f32),
    cut: Option<turn_animation::Cut>,
    paper: gpui_kit::Rgba,
    incoming: f32,
) -> PageCurlMesh {
    let width = sheet.width().max(1.0);
    let height = sheet.height().max(1.0);
    let page_origin = (sheet.left + offset.0, sheet.top + offset.1);
    let mut left = page_origin.0;
    let mut top = page_origin.1;
    let mut right = left + width;
    let mut bottom = top + height;
    if let Some(cut) = cut {
        let crease = cut.crease;
        if crease.ny.abs() < 1e-3 {
            // 覆盖档：折线竖直。`d` 是页面局部坐标，先折回窗口。
            let edge = cut_area.left + crease.d / crease.nx;
            if cut.keeps(cut_area.left, cut_area.top) {
                right = right.min(edge);
            } else {
                left = left.max(edge);
            }
        } else if crease.nx.abs() < 1e-3 {
            let edge = cut_area.top + crease.d / crease.ny;
            if cut.keeps(cut_area.left, cut_area.top) {
                bottom = bottom.min(edge);
            } else {
                top = top.max(edge);
            }
        }
    }
    let bounds = Bounds::new(
        point(px(left), px(top)),
        size(px((right - left).max(0.0)), px((bottom - top).max(0.0))),
    );
    let vertices: Arc<[PageCurlVertex]> = Arc::from(vec![
        PageCurlVertex {
            position: [0.0, 0.0],
            uv: [0.0, 0.0],
        },
        PageCurlVertex {
            position: [width, 0.0],
            uv: [1.0, 0.0],
        },
        PageCurlVertex {
            position: [width, height],
            uv: [1.0, 1.0],
        },
        PageCurlVertex {
            position: [0.0, height],
            uv: [0.0, 1.0],
        },
    ]);
    let indices: Arc<[u32]> = Arc::from(vec![0, 1, 2, 0, 2, 3]);
    PageCurlMesh {
        bounds,
        page_bounds: bounds,
        vertices,
        indices,
        uniforms: PageCurlUniforms {
            page_origin: [page_origin.0, page_origin.1],
            page_size: [width, height],
            window_size: [0.0, 0.0],
            axis_normal: [1.0, 0.0],
            // 页内 x ∈ [0, width]，取 4 倍宽：所有点都落在平摊那一侧。
            axis_d: width * 4.0,
            radius: width.max(height),
            camera_distance: width.max(height) * 3.0,
            scale_factor: 1.0,
            back_color: [paper.r, paper.g, paper.b, paper.a],
            light_dir: [0.0, 0.0, 1.0, 0.0],
            cone_a: [0.0; 4],
            cone_b: [0.0; 4],
            capture_origin: [sheet.left, sheet.top],
            // `0` = 采出场页那张（正面捕获），`1` = 采进场页那张（捕获层 3 烘出来的）。
            incoming,
            back_capture_origin: [sheet.left, sheet.top],
        },
    }
}

/// 捕获矩形（**阅读页局部**坐标）：整张纸那一块，横向抵消掉外壳进场那一段的横移。
///
/// 副本画在最终位置、矩形也按最终位置取，于是进书那 280 ms 里就能烘：页框滑出窗口的那一截
/// 是永远烘不进纹理的（纹理只有整窗大），而网格随后按整页 UV 去采，边上一条黑带。
///
/// **这块矩形与 `page_area` 同一把尺**，也就是阅读页自己的坐标（y = 0 是这一屏的顶边）。
/// 绘制回调里 `bounds.origin` 已经是**窗口**坐标 —— 阅读层从窗口 0 起，
/// 那块 canvas 的原点因此是 `(enter_shift, 0)`。取捕获矩形要先把进场横移还掉
/// （[`shape_in_window`]），否则每一条边都多出一个 `enter_shift`：矩形下沿越过窗口底，
/// `in_viewport` 每一帧都拒，仿真折页的门永远不开，点屏幕什么都不发生。
fn capture_region(area: turn_animation::Area, enter_shift: f32) -> Bounds<Pixels> {
    Bounds::new(
        point(px(area.left - enter_shift), px(area.top)),
        size(px(area.width()), px(area.height())),
    )
}

/// 把一块**阅读页局部**矩形落到窗口坐标上。
///
/// `origin` 是那块 canvas 在窗口里的原点（`bounds.origin`，已经把进场横移吃掉了一部分），
/// `layer` 是外壳报给这一屏的那两格（[`ReaderSession::enter_shift`] / 层顶 0）—— 两者同源，所以
/// 「这一块自己在窗口里的原点」就是 `origin - layer`。用同源的两个数相减而不是把
/// `top_inset` 写死成 0：外壳哪天改了推法，两边仍然对得上。
fn shape_in_window(
    origin: Point<Pixels>,
    layer: (f32, f32),
    local: Bounds<Pixels>,
) -> Bounds<Pixels> {
    Bounds::new(
        point(
            origin.x - px(layer.0) + local.origin.x,
            origin.y - px(layer.1) + local.origin.y,
        ),
        local.size,
    )
}

/// 捕获矩形与窗口边界比较时容的浮点误差（逻辑像素）。
///
/// 整张纸的下沿就是屏幕底：几百个像素加减出来的 f32 可能比窗口高多出小数末位的一点点，
/// 而 `in_viewport` 是一条「差一点就整趟不烘」的硬闸。容半像素，只吞舍入误差。
const CAPTURE_EDGE_TOLERANCE: f32 = 0.5;

/// 整张纸这一帧该占的那块矩形（**阅读页局部**坐标）：从阅读页顶铺到窗口底，宽是整窗。
///
/// 只按**窗口自己的高度**量，不把两条系统边 / 两条带子的高加回去：
/// 阅读层从窗口 0 起（`shell::reading_overlay`），
/// 阅读页坐标里的 0 就是那块盒的顶边，`viewport_size().height` 就是它的高 —— 挖孔那一条
/// 与手势条那一条本来就在这之外。从前这里是「字那几格之和」，而且逐项各自可能被重复计
/// （`set_reserve` 已经是顶上 + 底下两条之和）：算出来的纸比窗口高，捕获矩形于是每一帧都
/// 被 [`in_viewport`] 拒掉。拒掉之后捕获永远不落地 ⇒ `curl_capture_ready()` 恒假 ⇒
/// 仿真折页那一趟被排在门外无限期等待，点屏幕没有任何反应。
fn sheet_area_in_page(viewport: (f32, f32)) -> turn_animation::Area {
    turn_animation::Area::new(0.0, 0.0, viewport.0.max(0.0), viewport.1.max(0.0))
}

/// 这一块是否完整落在窗口内 —— 出了一截就**不要**请求捕获（那一截永远烘不进来，
/// 而网格仍按整页采 UV；真机读数就是页边那条黑色横带）。
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

fn in_viewport(rect: Bounds<Pixels>, viewport: (f32, f32)) -> bool {
    f32::from(rect.origin.x) >= -CAPTURE_EDGE_TOLERANCE
        && f32::from(rect.origin.y) >= -CAPTURE_EDGE_TOLERANCE
        && f32::from(rect.origin.x) + f32::from(rect.size.width)
            <= viewport.0 + CAPTURE_EDGE_TOLERANCE
        && f32::from(rect.origin.y) + f32::from(rect.size.height)
            <= viewport.1 + CAPTURE_EDGE_TOLERANCE
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
    let __t = crate::ui::features::reader::model::perf::Timer::start();
    let image = atlas.update(cx, |atlas, cx| atlas.painted(&request, view, cx));
    let __image = __t.ns();
    crate::ui::features::reader::model::perf::add(|frame| frame.image_ns += __image);
    image
}

/// 一格的像素：先画低清预览，高清到了再换。
///
/// 两档**不同时起**：高清那一趟的回开容器 + 栅格化要几秒，先起它会把低清挤在同一批
/// 核上（实测手机上低清也跟着拖到三四秒才出来）。这里按顺序排：预览先到先画，
/// 预览在手之后才排高清 —— 首图从「等一整趟 2048」变成「等一趟 720」。
///
/// 替换时排版不动：两张图的声明尺寸相同，只有像素变了。低清那格在高清到货后不再被
/// 问，两帧后由图集回收。
fn painted_tile(
    atlas: &Entity<ImageAtlas>,
    tile: &PageTile,
    view: EntityId,
    cx: &mut App,
    scrolling: bool,
) -> Option<Arc<RenderImage>> {
    if let Some(preview) = tile.preview.as_ref() {
        if let Some(low) = painted(atlas, preview.clone(), view, cx) {
            // **快速滑动时只要高清已经在手就换，但不去起它。** 高清那一趟是几十到几百
            // ms 的栅格化，滑动中起它等于把「下一帧」的钱挪到「这一帧」上 —— 读者看见
            // 的是滑动卡顿，而他要的那张图本来也还在下面好几屏外。停下 150 ms 之后
            // 那一帧 `scrolling` 为假，这里才排高清（`atlas.prepare` 自己会去重）。
            let high = tile.request.as_ref();
            if scrolling
                && !high.is_some_and(|request| {
                    atlas.read_with(cx, |atlas, _| atlas.ready(&request.key))
                })
            {
                return Some(low);
            }
            if let Some(image) = tile
                .request
                .as_ref()
                .and_then(|request| painted(atlas, request.clone(), view, cx))
            {
                return Some(image);
            }
            return Some(low);
        }
        // 预览解不出来（坏页）就退回高清；还只是没到就继续等，**不碰高清**（理由见上）。
        if !atlas.read_with(cx, |atlas, _| atlas.failed(&preview.key)) {
            return None;
        }
    }
    // 没有预览的格子（容器里本来就有图）：直接走它自己那一趟。
    tile.request
        .as_ref()
        .and_then(|request| painted(atlas, request.clone(), view, cx))
}

/// 正文插图（与 CSS 背景图）那一格：先画低清预览，高清到了再换。
///
/// 这是 [`painted_tile`] 给定版式那一族早就在做的事，2026-10-31 搬到可重排正文上：
/// 那一头的书里插图没有预览档，于是「解完之前什么都不画」在快滑时读成一片白纸
/// （`view::body` 从前那个 `continue`）。顺序照旧是**预览先到、高清后起**，理由也照旧：
/// 两趟同时排进池子会挤在同一批核上，实测手机上低清跟着拖到三四秒。
///
/// 与 `painted_tile` 的一处分别是这里没有 `scrolling` 那一档闸门：高清只在**它的预览
/// 到货之后**才起，而预览到货时这一格还在屏上 —— 快滑里一页只停两三帧，那条闸门
/// 实际挡不住什么，却要把它一路塞进 `decoration_child`。滑动期间的预取深度另有一刀
/// （`doc/图片回收.md` 刀 3）。
///
/// 替换时排版不动：两张图的声明尺寸相同，只有像素变了。低清那格在高清到货后不再被问，
/// 两帧后由图集回收（它记在低清那一档的账上，不占高清的额度）。
fn painted_picture(
    atlas: &Entity<ImageAtlas>,
    request: &DecodeRequest,
    view: EntityId,
    cx: &mut App,
) -> Option<Arc<RenderImage>> {
    let Some(preview) = request.preview() else {
        // 这张图本来就不大（或尺寸没记在索引里，见 `DecodeRequest::preview` 那三条）：
        // 直接走它自己那一趟，与从前一模一样。
        return painted(atlas, request.clone(), view, cx);
    };
    if let Some(low) = painted(atlas, preview.clone(), view, cx) {
        if let Some(high) = painted(atlas, request.clone(), view, cx) {
            return Some(high);
        }
        return Some(low);
    }
    // 预览还只是没到：这一格继续空着，**不碰高清**（理由与 `painted_tile` 那一段相同）。
    // 预览解不出来（坏图）才退回高清，那一页不该因为低清失败而永远白着。
    if !atlas.read_with(cx, |atlas, _| atlas.failed(&preview.key)) {
        return None;
    }
    painted(atlas, request.clone(), view, cx)
}

/// 预取一格：只排**预览**那一趟，而且排进 [`Priority::Prewarm`] 档。
///
/// 高清是翻到那一页时才值得花的钱 —— 预取高清会把下一趟翻页的首帧又拖回几百毫秒
/// （手机上是几秒），而预取低清足够让那一页先出来。
///
/// 档位降一档是为了**池子里的取活顺序**：读者手上这一页的预览/高清是 `Interactive`，
/// 预取两页是 `Prewarm`，于是「翻过去那一下」不会被排在两页预取后面（见
/// `folio_core::Priority` 的档序）。
fn prepare_tile(atlas: &Entity<ImageAtlas>, tile: &PageTile, view: EntityId, cx: &mut App) {
    if let Some(request) = tile.preview.as_ref().or(tile.request.as_ref()) {
        let request = request.clone().with_priority(folio_core::Priority::Prewarm);
        atlas.update(cx, |atlas, cx| atlas.prepare(&request, view, cx));
    }
}

/// 放大上限要的「源宽」。
///
/// **矢量页不按手里这一张算**：那种页捏稳之后会把看得见的那一块按 `REGION_EDGE`
/// 重画一遍，细节上限是 `RENDER_EDGE → REGION_EDGE` 那一倍，所以宽也按那一倍报给
/// [`max_scale`]。扫描件与容器图那一族手里这一张就是全部 —— 按它们自己的宽度封顶，
/// 于是「放到 8 倍」对那一族仍然是糊的，而那正是源图的极限（计划 §六 写明的限制）。
fn zoom_source_width<'a>(tiles: impl IntoIterator<Item = &'a PageTile>) -> f32 {
    let tiles: Vec<&PageTile> = tiles.into_iter().collect();
    let base: f32 = tiles.iter().map(|tile| tile.width as f32).sum();
    let boost = if tiles.iter().any(|tile| tile.vector) {
        folio_core::content::format::pdf::REGION_EDGE as f32 / RENDER_EDGE as f32
    } else {
        1.0
    };
    base * boost
}

/// 页表像素那一块 → **1x 内容坐标**里的那一块（同一张纸的同一个地方，两把尺）。
fn region_rect(
    tile: (f32, f32, f32, f32),
    region: PageRegion,
    source: (u32, u32),
) -> (f32, f32, f32, f32) {
    let (tx, ty, tw, th) = tile;
    let frac = |from: u32, span: u32| from as f32 / span.max(1) as f32;
    let (x0, y0) = (frac(region.x0, source.0), frac(region.y0, source.1));
    let (x1, y1) = (frac(region.x1, source.0), frac(region.y1, source.1));
    (tx + x0 * tw, ty + y0 * th, (x1 - x0) * tw, (y1 - y0) * th)
}

/// 一次「放大停稳了没有、该重画哪一块」的输入。
///
/// 合成一个结构而不是十三个参数：连续竖滚与整页翻两臂递的是同一组量，摊成名字之后
/// 调用点自己说清「哪一格、哪一块屏、哪一个轴心、哪一刻」。
struct RegionAsk<'a> {
    book: &'a FixedBook,
    atlas: &'a Entity<ImageAtlas>,
    page: usize,
    tile: &'a PageTile,
    /// 那一格在 **1x 内容坐标**里的矩形（dp）。
    rect: (f32, f32, f32, f32),
    /// 缩放轴心（dp）：两臂不同 —— 连续滚是版心左上角，整页翻是铺满那一页的左上角。
    pivot: (f32, f32),
    viewport: (f32, f32, f32, f32),
    zoom: FixedZoom,
    now: std::time::Instant,
}

/// 放大那一档：这一帧该不该排「看得见的那一块按 `REGION_EDGE` 重画」，以及钉着的那一张
/// 该叠在哪。交回 `Some(PinnedRegion)` 只表示**这一帧该去问图集要那一块的像素**
/// （还没到货时 `painted` 给 `None`，那一页就只画它手里那张 2048 —— 叠层是加细节，
/// 永远不该让屏上出现空洞）。
///
/// 三条规矩：
/// - 只有**矢量的渲染页**问得出东西（`FixedBook::region_request` 对扫描件与容器图回
///   `None`，那一趟栅格化根本不会起）；
/// - 只有**停稳** [`REGION_SETTLE`] 之后才排：手在动就一帧帧把表推后（与「滑动中只铺
///   预览」同一条理由 —— 那一趟是几百 ms 的活，边滑边排等于把下一帧的钱花在这一帧）；
/// - 叠层画在**它自己那一刻**的矩形上：读者随后平移，那一张跟着自己的矩形走，
///   新的那一块要等下一次停稳。这就是 [`PinnedRegion`] 存在的唯一理由。
fn pinned_region(
    ask: RegionAsk<'_>,
    moved_at: &Cell<Option<std::time::Instant>>,
    pinned: &RefCell<Option<PinnedRegion>>,
    view: EntityId,
    cx: &mut App,
) -> Option<PinnedRegion> {
    let RegionAsk {
        book,
        atlas,
        page,
        tile,
        rect,
        pivot,
        viewport,
        zoom,
        now,
    } = ask;
    if !tile.vector || !zoom.is_zoomed() {
        // 撤大回铺满：钉着的那一张不再属于这一屏（它占的图集名额由帧末回收按「没在场」放掉）。
        moved_at.set(None);
        pinned.borrow_mut().take();
        return None;
    }
    let source = (tile.width, tile.height);
    let region = visible_region(zoom, pivot, rect, source, viewport)?;
    let scale = zoom.scale();
    // 这一块已经钉住了而且倍率没变：一帧一趟都不多排。
    if let Some(pin) = pinned.borrow().as_ref()
        && pin.region == region
        && (pin.scale - scale).abs() < 0.01
    {
        return Some(pin.clone());
    }
    let Some(last_moved) = moved_at.get() else {
        // 第一次看见放大后的这一屏：起表，这一帧不排。
        moved_at.set(Some(now));
        return None;
    };
    if now.duration_since(last_moved).as_millis() < REGION_SETTLE {
        // 还在动（或刚停下）：把表推后，旧的那一张继续按它自己的矩形画。
        moved_at.set(Some(now));
        return pinned.borrow().clone();
    }
    // 停够了：算输出尺寸 → 排进图集（`Interactive`：读者正看着这一块）→ 钉住。
    let (width, height) = region_output(region, source)?;
    let request = book.region_request(page, region, width, height)?;
    atlas.update(cx, |atlas, cx| atlas.prepare(&request, view, cx));
    let pin = PinnedRegion {
        region,
        scale,
        rect: region_rect(rect, region, source),
        request,
    };
    *pinned.borrow_mut() = Some(pin.clone());
    Some(pin)
}

/// 一片里的一张图：绝对摆位交给 gpui 的 `img`，外加「点一下摊开看」。
///
/// 一张图若是**脚注标记**（老书把标记做成一张小「注」图），那一击归脚注 —— 抬起注文，
/// 不进图片查看器。判据是排版期就解好的 `image.footnote`（见 [`VisibleImage::footnote`]），
/// 这一层只按结果分流。
///
/// 「看大图」的入口与定版式那一页同一套（见 [`page_image`]）：按得住长按的那一端用**长按**
/// —— 单击留给翻页，插图上一下点按不该把正文顶开一块浮层；按不住长按的那一端（桌面鼠标）
/// 退回单击。
///
/// `rendered` 是图集那一层交回来的像素：`image` 里只有摆位与那条活儿（`DecodeRequest`），
/// 像素不随场景走（见 [`crate::ui::features::reader::model::image_atlas`]）。
#[allow(clippy::too_many_arguments)]
fn image_child(
    image: VisibleImage,
    rendered: Arc<RenderImage>,
    viewer: &Option<Entity<ImageViewer>>,
    long_press: bool,
    index: usize,
    reader: &WeakEntity<ReaderSession>,
    pan: (f32, f32),
    hits: &Rc<RefCell<Vec<ImageHit>>>,
    frame_no: u64,
    chapter: usize,
    stack_hits: &Rc<RefCell<Vec<StackHit>>>,
) -> AnyElement {
    // `object-fit`：图按哪一块区域采样进这个盒。`Fill`（初值）与前一字不差的
    // `size_full()`；`Contain` 留底、`Cover` 裁、`None` 原尺寸 —— 三种都要先算出一个
    // 可能超出盒的矩形，再靠盒的 `overflow_hidden` 裁。
    let source = rendered.size(0);
    let (source_w, source_h) = (source.width.0 as f32, source.height.0 as f32);
    let (ox, oy, w, h) = image
        .fit
        .placement(source_w, source_h, image.width, image.height);
    // 这一张比框大出多少：`cover` 按比例盖满盒子，多出来的那一圈被 `overflow_hidden`
    // 裁掉 —— 那一圈正是读者想拖着看的内容。`slack` 为 0 的图不进命中表（拖它没意义）。
    let slack = ((w - image.width).max(0.0), (h - image.height).max(0.0));
    let draggable = slack.0 > 0.5 || slack.1 > 0.5;
    // 偏移是**当前**拖到哪儿（负值 = 内容往上/往左露出来的那一截）。
    let (px_off, py_off) = if draggable { pan } else { (0.0, 0.0) };
    let key = image.image.key.clone();
    // 叠里的图带一点圆角（Reeden 图册那个形状）；单张图仍按原样直角。
    let radius = if image.stack.is_some() { 8.0 } else { 0.0 };
    let frame = div()
        .absolute()
        .left(px(image.x))
        .top(px(image.top))
        .w(px(image.width))
        .h(px(image.height))
        .rounded(px(radius))
        .overflow_hidden()
        .child(
            img(rendered)
                .absolute()
                .left(px(ox + px_off))
                .top(px(oy + py_off))
                .w(px(w))
                .h(px(h)),
        );
    // 脚注标记优先：它是「注」图，不是插图，点它要的是注文而不是放大。
    if let Some(note) = image.footnote {
        return footnote_marker(frame, note, reader.clone(), index);
    }
    // 可拖动的那些：把框登记进本帧的命中表（`on_scroll` 按按下点查它，决定这一笔
    // 翻页还是拖图）。只登记、不拦事件 —— 拦下来就再没有翻页手势了。
    let frame = if draggable {
        let hits = hits.clone();
        let key = key.clone();
        frame.child(image_pan_zone(key, slack, hits, frame_no))
    } else {
        frame
    };
    // 一叠图：登记这一格给横滑切图认领，并在角上标 `k/n`。
    //
    // 角标不是装饰 —— 一叠只占一格，屏上看着就是一张图，不标一下读者不知道
    // 这里还有四张 —— 没有出口，折叠就等于把 8 张压成 1 张。
    let frame = match image.stack {
        Some(mark) if mark.count > 1 => frame
            .child(stack_zone(mark, chapter, stack_hits.clone(), frame_no))
            .child(
                div()
                    .id("reader-stack-badge")
                    .test_support()
                    .absolute()
                    .top(px(6.0))
                    .right(px(6.0))
                    .px(px(6.0))
                    .rounded(px(8.0))
                    .text_xs()
                    .child(format!("{} / {}", mark.index + 1, mark.count)),
            ),
        _ => frame,
    };
    match viewer {
        Some(viewer) if long_press => frame
            .child(long_press_to_open(viewer.clone(), reader.clone(), key))
            .into_any_element(),
        Some(viewer) => {
            open_on_tap(frame, viewer.clone(), reader.clone(), key, index).into_any_element()
        }
        None => frame.into_any_element(),
    }
}

/// 叠图的「下一张」卡片边：在图下面露出 6px 的一条，提示还能往左滑。
///
/// 宽度取图的 92%、水平居中，颜色是前景色的 12%（暗底亮底都看得出，又不抢图）。
/// 它画在图**之前**，上面那一截被图盖住，露出来的就是一条圆角的边 —— 不提前解下一张
/// 的图，也不改排版。
fn stack_peek(x: f32, bottom: f32, width: f32, cx: &App) -> AnyElement {
    let inset = width * 0.04;
    div()
        .id("reader-stack-peek")
        .test_support()
        .absolute()
        .left(px(x + inset))
        .top(px(bottom - 4.0))
        .w(px(width - inset * 2.0))
        .h(px(12.0))
        .rounded(px(8.0))
        .bg(cx.theme().foreground.opacity(0.12))
        .into_any_element()
}

/// 一叠图底下那排小圆点：`count` 个，当前那张实心。
///
/// 与图右上角那格 `k / n` 角标**同时存在**：角标说「第几张、一共几张」，这排点说的是
/// 「还能往两边走」。样式照着 Reeden 的图册来 —— 5px、间距 4、居中、当前那张用前景色，
/// 其余用弱化的前景色（不抢图的注意力）。
fn stack_dots(
    x: f32,
    bottom: f32,
    width: f32,
    mark: folio_core::layout::StackMark,
    cx: &App,
) -> AnyElement {
    let on = cx.theme().foreground;
    let off = cx.theme().muted_foreground.opacity(0.35);
    div()
        .id("reader-stack-dots")
        .test_support()
        .absolute()
        .left(px(x))
        .top(px(bottom + 5.0))
        .w(px(width))
        .flex()
        .justify_center()
        .gap(px(4.0))
        .children((0..mark.count).map(move |i| {
            div()
                .w(px(5.0))
                .h(px(5.0))
                .rounded(px(2.5))
                .bg(if i == mark.index { on } else { off })
        }))
        .into_any_element()
}

/// 一张可拖动插图的**命中区**：只把这一帧它在窗口里的矩形登记下来，不拦任何事件。
///
/// 为什么用 `canvas` 而不是 `on_click`/`on_mouse_down`：拖动这一笔必须交给
/// `on_scroll`（识别器把触摸折成滚动事件，`TouchDragEvent` 只在按下那一下递一次探针，
/// 接了就把点屏翻页与长按吸词一起吃掉 —— 那一条写在 `swipe.rs` 的模块注释里）。
/// 这里要的只是「这张图这一帧在窗口的哪一块」，而绘制回调给的 `bounds` 正是窗口坐标。
fn image_pan_zone(
    key: String,
    slack: (f32, f32),
    hits: Rc<RefCell<Vec<ImageHit>>>,
    frame_no: u64,
) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, _window, _cx| {
            let mut hits = hits.borrow_mut();
            // 同一帧里重放可能再来一次：同一个 key 只留一条（后写的框是最终位置）。
            if let Some(existing) = hits.iter_mut().find(|hit| hit.key == key) {
                existing.rect = bounds;
                existing.slack = slack;
                existing.frame = frame_no;
            } else {
                hits.push(ImageHit {
                    key: key.clone(),
                    rect: bounds,
                    slack,
                    frame: frame_no,
                });
            }
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

/// 一叠图那一格的**命中区**：只登记矩形，不拦事件（与 [`image_pan_zone`] 同一条理由 ——
/// 拦下来就把点屏翻页与长按吸词一起吃了）。切图那一笔由 `swipe_scroll` 按起手点认领。
fn stack_zone(
    mark: folio_core::layout::StackMark,
    chapter: usize,
    hits: Rc<RefCell<Vec<StackHit>>>,
    frame_no: u64,
) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, _window, _cx| {
            let mut hits = hits.borrow_mut();
            if let Some(existing) = hits
                .iter_mut()
                .find(|hit| hit.chapter == chapter && hit.id == mark.id)
            {
                existing.rect = bounds;
                existing.count = mark.count;
                existing.frame = frame_no;
            } else {
                hits.push(StackHit {
                    chapter,
                    id: mark.id,
                    count: mark.count,
                    rect: bounds,
                    frame: frame_no,
                });
            }
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
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
#[allow(clippy::too_many_arguments)]
fn page_image(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    image: Arc<RenderImage>,
    key: String,
    viewer: &Option<Entity<ImageViewer>>,
    long_press: bool,
    index: usize,
    reader: &WeakEntity<ReaderSession>,
) -> AnyElement {
    let frame = div()
        .absolute()
        .left(px(x))
        .top(px(y))
        .w(px(w))
        .h(px(h))
        .child(img(image).size_full());
    let Some(viewer) = viewer.clone() else {
        return frame.into_any_element();
    };
    let reader = reader.clone();
    if long_press {
        frame
            .child(long_press_to_open(viewer, reader, key))
            .into_any_element()
    } else {
        open_on_tap(frame, viewer, reader, key, index).into_any_element()
    }
}

/// 把「正文里的这一张」翻成「整本书那一列里的第几条」，然后开那一层。
///
/// 为什么按**图集的键**去问而不是把手上的像素递过去：查看器摆的现在是那一列里的第几张 ——
/// 左右滑要能接着往下走、计数要念得出来、定位原文要有出处，三样都在那一列里，
/// 光有一份像素哪一样都给不出。
///
/// 问不到（那一列里没有这一条）时这一击什么都不做：开一层「0 / 0」、既滑不动也定不了位，
/// 比这一下没反应更难向读者解释。
pub(in crate::ui::features::reader) fn open_picture(
    viewer: &Entity<ImageViewer>,
    reader: &WeakEntity<ReaderSession>,
    key: &str,
    window: &mut Window,
    cx: &mut App,
) {
    let viewport = viewport_size(window);
    // 下标与那一列一起取回来：`Arc` 的克隆只是引用计数，查看器因此不必每帧回会话要。
    let ask = reader.update(cx, |session, _| {
        session
            .image_index_of_key(key)
            .map(|index| (index, session.image_sequence().clone()))
    });
    // 会话已经随阅读页收掉了（`WeakEntity` 升级不上来）时这一击什么都不做。
    let Some((index, sequence)) = ask.ok().flatten() else {
        tracing::trace!("图片查看器: {key} 不在整本书那一列里，这一击不开");
        return;
    };
    viewer.update(cx, |viewer, cx| {
        viewer.open(index, viewport, sequence);
        cx.notify();
    });
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
    reader: WeakEntity<ReaderSession>,
    key: String,
    index: usize,
) -> impl IntoElement {
    frame
        .id(("reader-image", index))
        .block_mouse_except_scroll()
        .on_click(
            move |event: &ClickEvent, window: &mut Window, cx: &mut App| {
                let at = event.position();
                // 这一击先问「选中」那一套，再谈放大 —— 走的是正文那一臂同一个入口
                // （`ReaderSession::tap_on_body`，四条判据与顺序都写在那儿）。为什么要在
                // 这一层问：`block_mouse_except_scroll` 把这一击从正文那三层里抢了过来，
                // 正文那一臂就再也收不到它 —— 于是桌面上读 PDF 的人点在已划的那条线上，
                // 会被这张图整页放大抢走，「挑回去改一条」那条路在定版式上断掉。
                // 三条判据都不成立（没有选区、没压在划线上、不在拖选的合成 click 里）才开
                // 大图：普通单击的行为与从前逐字相同。
                let claimed = reader
                    .update(cx, |session, cx| session.tap_on_body(window, cx, at))
                    .unwrap_or(false);
                if claimed {
                    return;
                }
                open_picture(&viewer, &reader, &key, window, cx);
            },
        )
        // 挂在最外层：宿主的测试要按 `("reader-image", 序)` 点在这张图上。
        .test_support()
}

/// 把**原始**触摸流记进会话的那块监听。
///
/// 为什么非要这一路：跟手翻页吃的是识别器折出来的 `ScrollWheelEvent`，而它在把一笔定性成
/// 某一轴的那一刻就把另一轴的增量抹成 0（`lock_delta_to_axis`）。于是「起手点 + 累计位移」
/// 反推出来的手指此刻只剩一个自由度，折线转不起来。同一笔的原始触摸是唯一还带着第二个自由
/// 度的地方。
///
/// 只记账，不 `prevent_default`、不停传播：这一格要是把事件认下来，点屏翻页与长按吸词就一起
/// 没了。挂在 `canvas` 的绘制回调里也是同一规矩 —— `window.on_mouse_event` 只活到本帧事件
/// 分发结束，每个绘制帧都要重挂一遍。
fn touch_follow(reader: WeakEntity<ReaderSession>) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window: &mut Window, _cx: &mut App| {
            let reader = reader.clone();
            let origin = bounds.origin;
            window.on_mouse_event(move |event: &TouchEvent, phase, _window, cx| {
                // 同一条事件会走 Capture 与 Bubble 两遍，只在 Bubble 那一次记账。
                if !phase.bubble() {
                    return;
                }
                let _ = reader.update(cx, |session, _| {
                    let tracked = session.finger.is_some_and(|(id, _)| id == event.id);
                    match event.phase {
                        // 新的那一根手指只有落在这一栏里才算数；已经认下的那根不受这条约束
                        // —— 拖出页外正是「把整页折走」那一笔会做的事。
                        TouchPhase::Started => {
                            // 起手落在某一叠上：这一笔先按「切图」记账（`swipe_scroll`
                            // 那一头决定它到底算切图还是算滚）。
                            let grab = session.stack_hit_at(event.position);
                            session.stack_grab = grab;
                            session.stack_dx = 0.0;
                            session.stack_dy = 0.0;
                            if bounds.contains(&event.position) {
                                let at = session.finger_of(event.position, origin);
                                session.finger = Some((event.id, at));
                            }
                        }
                        TouchPhase::Moved if tracked => {
                            let at = session.finger_of(event.position, origin);
                            session.finger = Some((event.id, at));
                        }
                        TouchPhase::Ended | TouchPhase::Cancelled if tracked => {
                            session.finger = None;
                        }
                        _ => {}
                    }
                });
            });
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

/// 定版式的双指缩放，以及放大之后的单指平移。
///
/// 挂法与 [`touch_follow`] 同一条：`window.on_mouse_event` 只活到本帧事件分发结束，
/// 每个绘制帧都要在 `canvas` 的绘制回调里重挂一遍。
///
/// 两路事件分工不同的理由：
///
/// - **捏合**吃 `PinchEvent`。`gpui-mobile` 的 `PinchGuard` 在平台层就把第二根手指折成了
///   它，并且把这几根手指的单点流一起吞掉 —— 所以捏合不会顺手翻一页。
/// - **拖动**吃原始 `TouchEvent`。识别器在把一笔定性成某一轴的那一刻就把另一轴的增量
///   抹成 0，而平移一张放大过的纸两个轴都要；原始触摸是唯一还带着两个自由度的地方。
///
/// 两路都**不** `prevent_default`：这一趟也可能只是一次点击（点屏幕左右翻页），
/// 在起手那一刻认领，翻页就没了。识别器那边折出来的滚动在 `on_scroll` 里让掉即可。
fn fixed_gestures(reader: WeakEntity<ReaderSession>) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window: &mut Window, _cx: &mut App| {
            let origin = bounds.origin;
            let pinch_reader = reader.clone();
            let pan_reader = reader;
            window.on_mouse_event(move |event: &PinchEvent, phase, _window, cx| {
                // 同一条事件会走 Capture 与 Bubble 两遍，只记 Bubble 那一次。
                if !phase.bubble() {
                    return;
                }
                let focal = (
                    f32::from(event.position.x) - f32::from(origin.x),
                    f32::from(event.position.y) - f32::from(origin.y),
                );
                let _ = pinch_reader.update(cx, |session, cx| {
                    session.pinch_fixed(event.delta, focal, cx);
                });
            });

            let inside = bounds;
            window.on_mouse_event(move |event: &TouchEvent, phase, _window, cx| {
                if !phase.bubble() {
                    return;
                }
                let at = (
                    f32::from(event.position.x) - f32::from(origin.x),
                    f32::from(event.position.y) - f32::from(origin.y),
                );
                let _ = pan_reader.update(cx, |session, cx| match event.phase {
                    TouchPhase::Started => {
                        session.fixed_pan_start(event.id, at, inside.contains(&event.position));
                    }
                    TouchPhase::Moved => {
                        session.fixed_pan_move(event.id, at, cx);
                    }
                    TouchPhase::Ended | TouchPhase::Cancelled => session.fixed_pan_end(event.id),
                });
            });
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

/// 「按住这一页」的监听，挂在 `canvas` 的绘制回调里：`window.on_mouse_event` 只活到本帧
/// 事件分发结束，每个绘制帧都要重挂一遍（与 [`crate::ui::features::reader::view::paint`] 里选中那几条同一规矩）。
///
/// `prevent_default` 就是**认领**这一手势：gpui 拿 `default_prevented` 当「这一手势被
/// 接走了」的依据（`window.rs` 里 `resolve_long_press(claimed)`）。不认领的话手指抬起时
/// 会补出一个合成点击，而那一击的意思正是翻页 —— 按住看大图会变成「翻一页再看大图」。
fn long_press_to_open(
    viewer: Entity<ImageViewer>,
    reader: WeakEntity<ReaderSession>,
    key: String,
) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window: &mut Window, _cx: &mut App| {
            let viewer = viewer.clone();
            let reader = reader.clone();
            window.on_mouse_event(move |event: &LongPressEvent, phase, window, cx| {
                if !phase.bubble() || event.phase != TouchPhase::Started {
                    return;
                }
                // 这一格只负责自己那一页：抬起之前手指挪出图外就不算「按住这张图」。
                if !bounds.contains(&event.start_position) {
                    return;
                }
                open_picture(&viewer, &reader, &key, window, cx);
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

/// 一行里**字墨**的上下边（相对行顶）与高度。
///
/// 选区与批注底色的高取这一份，不取 `line.height`（行盒高）。行盒按读者的行距
/// （默认 1.75em）撑开，行盒高比字实际占的那一块高出小半行 —— 那一截是行距，不是
/// 「选中的字」。行盒还会被行内图片与注音撑大：图高 200 时整条选区跟着 200 高，
/// 屏幕上看就是「选中区域比文字高出一大截」。
///
/// 只数文本片段，`baseline_shift`（注音、上标）跟着它自己那一笔抬/落；一个文本片段
/// 都没有（纯图片行）时给 `None`，调用方退回行盒。
///
/// 最后一刀把这一条**夹回行盒里**：`run.ascent + run.descent` 是字体的度量盒（hhea 那
/// 一对），CJK 那一族普遍比行距高（真机那本 20px 的字：度量盒 31.3、行距 27.0），于是
/// 相邻两行的底色带互相吃进去一截。底色是逐行 `paint_quad` 压上去的，重叠那一档被
/// 合成两遍，读出来就是一条更深的横带（2026-11-02 真机量的：单层 `(204,226,254)`、
/// 两层 `(168,206,253)`，缝正好在 1190..1201）。gpui-kit 自家 `SelectableText` 的
/// `selection_quad_bounds` 是按 `line_height` 平铺的，两块只相接不重叠 —— 这一刀对齐它。
pub(in crate::ui::features::reader) fn text_ink_box(line: &LineBox) -> Option<(f32, f32)> {
    let mut up = 0.0f32;
    let mut down = 0.0f32;
    for fragment in &line.fragments {
        if let LineFragment::Text(run) = fragment {
            let shift = run.style.baseline_shift;
            up = up.max(run.ascent + shift);
            down = down.max(run.descent - shift);
        }
    }
    if up + down <= 0.0 {
        return None;
    }
    let up = up.min(line.baseline.max(0.0));
    let down = down.min((line.height - line.baseline).max(0.0));
    (up + down > 0.0).then(|| (line.y + line.baseline - up, up + down))
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
    // 本片沿推进轴的长度（`Fragment::height`）。连续滚动那一档递整章的长，
    // 而竖排只存在于分页，所以那一档里这一格根本不参与。
    flow_len: f32,
    header: f32,
    // 可见带，说的是**推进轴**上的一段（横排 = 章内 y，竖排 = 从长条右缘往里数）。
    band: std::ops::Range<f32>,
    out: &mut Vec<Bounds<Pixels>>,
) {
    if sub.start >= sub.end {
        return;
    }
    let layout = doc.layout();
    let vertical = layout.vertical();
    // 与 `Scene::build` 里同一个 `slide`：竖排那一档推进轴从长条右缘反着数。
    let slide = if vertical {
        origin_y + flow_len - layout.width
    } else {
        0.0
    };
    for rect in doc.selection_rects_in_band(window, cx, sub, band.clone()) {
        // 重复的那一道滤，问的还是推进轴上那一段（竖排时矩形的推进轴那一格是 x）。
        let (at, thick) = if vertical {
            (layout.width - (rect.x + rect.width), rect.width)
        } else {
            (rect.y, rect.height)
        };
        if at + thick < band.start || at > band.end {
            continue;
        }
        // 横排：底色贴字墨，不铺满整条行盒（见 [`text_ink_box`]）。`rect.y` 就是行顶，
        // 行盒按 y 单调，`+0.5` 落回它自己那一行。
        let (top, height) = if vertical {
            (rect.y, rect.height)
        } else {
            let line = &layout.lines[line_at_y(layout, rect.y + 0.5)];
            text_ink_box(line).unwrap_or((rect.y, rect.height))
        };
        // 落位分轴：横排沿 y 减片原点，竖排沿 x 加 `slide`（推进轴从长条右缘反着数）。
        let left = x + rect.x + if vertical { slide } else { 0.0 };
        let top_at = if vertical {
            top + header
        } else {
            top - origin_y + header
        };
        out.push(Bounds::new(
            point(px(left), px(top_at)),
            size(px(rect.width), px(height)),
        ));
    }
}

/// 一章的批注：库里的锚点 → 本章坐标 → 元素局部的一条条痕迹，追加进字形元素。
///
/// 换算口径与 [`highlight_rects`] 完全一致（同一套「这一栏摆在哪、可见带是哪一段」），
/// 只是来源从这一次挑中的选区换成了留在书上的批注。三档画法各要一块不同的矩形：
///
/// - **底色带**要的就是字墨那一块，所以直接复用 [`highlight_rects`] —— 与选中高亮同一条
///   换算（贴字、不铺满行盒），顺带把竖排一起对上（那条认推进轴，而字下那两条不认，见
///   [`annotation_underline_rects`] 末尾那条限制）。
/// - **字下实线**与**波浪线**要的是基线下同一条中线上的两块矩形，只差厚度。
///
/// 解不回来的那一条**不画** —— 宁可少一条痕迹，也不把读者划过的痕迹画到别的字上去
/// （见 `modules/annotations.rs`）。
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
    // 本片沿推进轴的长度（`Fragment::height`）。只有底色带那一档要它：那一档走
    // `highlight_rects`。连续滚动永远横排，那一档里这一格不参与。
    flow_len: f32,
    header: f32,
    band: Range<f32>,
    cut: Option<crate::ui::features::reader::motion::turn_animation::Cut>,
) {
    if stored.is_empty() || band.start >= band.end {
        return;
    }
    let spans =
        annotations::resolved_for(cache, stored, chapter, doc.chapter_hash(), doc.text_map());
    for span in spans.iter().filter(|span| !span.is_stale()) {
        let mut lines = Vec::new();
        match span.style {
            AnnotationStyle::Highlight => highlight_rects(
                doc,
                window,
                cx,
                span.start..span.end,
                x,
                origin_y,
                flow_len,
                header,
                band.clone(),
                &mut lines,
            ),
            AnnotationStyle::Line => annotation_underline_rects(
                doc,
                window,
                cx,
                span.start..span.end,
                x,
                origin_y,
                header,
                band.clone(),
                UNDERLINE_THICKNESS,
                &mut lines,
            ),
            AnnotationStyle::Wavy => annotation_underline_rects(
                doc,
                window,
                cx,
                span.start..span.end,
                x,
                origin_y,
                header,
                band.clone(),
                paint::WAVE_HEIGHT,
                &mut lines,
            ),
        }
        // 底色带压透明度：那一块是整片铺在字下的，实色会把字吃掉。字下那两条不压 ——
        // 它们只占基线下那几像素，压了就分不清是哪一档。
        let solid = palette.color(&span.color);
        let color = if span.style == AnnotationStyle::Highlight {
            solid.opacity(HIGHLIGHT_ALPHA)
        } else {
            solid
        };
        // 带文字笔记的那一条：在**真正末尾**补一枚便签图标。用最后一个字的矩形来定，
        // 而不是这一趟画出的最后一块 —— 长批注跨屏/跨页时，每一带都会画出「最后一块」，
        // 按它摆就会在每个接缝处都长出一枚图标。最后一个字不落在这一带就整枚不画。
        let note_anchor = if !span.note.trim().is_empty() && span.end > span.start {
            let mut tail = Vec::new();
            highlight_rects(
                doc,
                window,
                cx,
                (span.end - 1)..span.end,
                x,
                origin_y,
                flow_len,
                header,
                band.clone(),
                &mut tail,
            );
            tail.last().copied()
        } else {
            None
        };
        for line in lines {
            // 读者划的那道也跟着那张纸走：中心落在被裁掉那一侧就不画。不然一条亮黄的
            // 带会浮在已经被盖走/折走的字上，那一帧读起来像划线自己没跟着翻页。
            let center = line.center();
            if cut.is_some_and(|cut| !cut.keeps(f32::from(center.x), f32::from(center.y))) {
                continue;
            }
            element.mark(if span.style == AnnotationStyle::Wavy {
                Mark::Wave(line, color)
            } else {
                Mark::Quad(line, color)
            });
        }
        if let Some(last) = note_anchor {
            let center = last.center();
            if cut.is_none_or(|cut| cut.keeps(f32::from(center.x), f32::from(center.y))) {
                element.note(Mark::Note(note_icon_bounds(last, span.style), solid));
            }
        }
    }
}

/// 笔记图标的外盒：12×12，贴在划线末端的右侧。
///
/// 高亮带的末块是整行盒，取它的竖直中线；字下实线/波浪的末块只是贴着盒底的一条
/// 细线，拿它的中线会把图标画进字里，所以改成在线**上方**落。
fn note_icon_bounds(last: Bounds<Pixels>, style: AnnotationStyle) -> Bounds<Pixels> {
    const ICON: f32 = 12.0;
    let top = match style {
        AnnotationStyle::Highlight => f32::from(last.center().y) - ICON * 0.5,
        // `last` 是末字的整行盒；字下实线/波浪贴着盒底，图标落在线之上、盒底之内。
        _ => f32::from(last.origin.y) + f32::from(last.size.height) - ICON - 2.0,
    };
    Bounds::new(
        point(
            px(f32::from(last.origin.x) + f32::from(last.size.width) + 2.0),
            px(top),
        ),
        size(px(ICON), px(ICON)),
    )
}

/// 听书这一段底色的透明度。比选区（[`HIGHLIGHT_ALPHA`]）淡：它是背景里的一条
/// 「机器念到这儿了」，不该与读者亲手挑出来的痕迹抢眼。
const SPEECH_ALPHA: f32 = 0.2;

/// 听书正在念的那一段：**整段**一块底色，画在字形之下。
///
/// 数据源是会话的播放状态（`speech_range`），调用点在 `match &mut self.source` 之前
/// 解好、按章号滤进来 —— 念哪段亮哪段；段内逐句高亮没有数据源（Android 没有
/// `onRangeStart`，按时钟估的那条路线第一版不做，见 `model/speech.rs` 的模块文档）。
/// 换算与批注底色带共用 [`highlight_rects`]（同一套「这一栏摆在哪、可见带是哪一段」），
/// 颜色用主题的 `primary` 而不是 `selection`：前者念出来是「在念这里」，后者是
/// 「人挑了这里」，两种意思不该同色。`cut` 与批注同一条规矩：中心落在被裁掉那一侧
/// 的矩形不画，别让底色浮在已经折走的那半张纸上。
#[allow(clippy::too_many_arguments)]
fn speech_fill(
    element: &mut GlyphsElement,
    doc: &Document,
    window: &Window,
    cx: &App,
    speaking: Option<Range<usize>>,
    x: f32,
    origin_y: f32,
    flow_len: f32,
    header: f32,
    band: Range<f32>,
    cut: Option<crate::ui::features::reader::motion::turn_animation::Cut>,
) {
    let Some(sub) = speaking else {
        return;
    };
    if band.start >= band.end {
        return;
    }
    let mut rects = Vec::new();
    highlight_rects(
        doc, window, cx, sub, x, origin_y, flow_len, header, band, &mut rects,
    );
    let color = cx.theme().primary.opacity(SPEECH_ALPHA);
    for rect in rects {
        let center = rect.center();
        if cut.is_some_and(|cut| !cut.keeps(f32::from(center.x), f32::from(center.y))) {
            continue;
        }
        element.mark(Mark::Quad(rect, color));
    }
}

/// 定版式一页上**留在书上的那几条痕迹**（元素局部矩形，交给盖在页图之上那一层画）。
///
/// 与 [`annotation_fills`] 的分工只有一处：解锚那一步完全复用 core 那一份
/// （`annotations::resolved_for`：库里的锚点 → 第几个字到第几个字，四级降级同一条），
/// 只有「一段字 → 矩形」换了一把尺 —— 排版那一族问的是行盒，这一族问的是
/// `PageText::rects` 那份逐字盒。键也跟着换：那一族用章文本的内容哈希，这一族用
/// **这一页字层**的内容哈希（`FixedBook::page_text_key`），两者担的是同一个问题
/// 「坐标系所依据的那份内容变了没有」。
///
/// 三档画法落的还是那三块矩形：读者在设置里挑的那一档不该因为书的族别换个长相。
/// 字下那两条贴的是**字盒的底**而不是基线下 `descent × 0.618`：栅格出来的那一页里
/// 没有「基线」这一格可问，引擎给的盒就是那一段字的全部占位，盒底也就是墨迹之下
/// 最近的一条线。差的那几个像素在屏上是「线离字更近了一点」，不是画到别处去。
#[allow(clippy::too_many_arguments)]
/// 定版式一页里的一格：像素活儿 + 它在这一页里的最终摆位（不含动画位移）。
struct FixedTileAt {
    tile: PageTile,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

/// 连续竖滚里两页之间的缝（逻辑像素）。
///
/// **`0`**：PDF / 漫画的页要连成一条，页界由页面自己的白边给出；再垫一条 12 的空隙，
/// 一屏里就会出现两道黑带（上下各一页时尤其明显）。缝仍参与页栈的坐标 —— 调它只改
/// 页与页的距离，不改每页的高度（页高只由版心宽与页面自己的宽高比决定）。
const FIXED_PAGE_GAP: f32 = 0.0;

/// 把一页的每一格摆成一条共享高度的横排，整体居中。返回每格的最终位置，以及
/// 「带字那一格」的矩形（页内字序那把尺记在它上面）。
///
/// 一页可能摆好几格（多图页 / 对开跨页，或渲染型的一整页）：一格时这条式子退化成
/// 「等比缩进视口」，与普通漫画的旧表现逐像素相同。尺寸未知（0）时按视口那一格的
/// 宽高比补 —— 一格等于铺满，多格共享高度则必须有个比例，视口比例是最不失真的猜。
/// 字层没到的那几页丢给后台，取回来写进会话并重画一帧。
///
/// 为什么不在绘制帧里等：PDF 一页字层要重开一次文档再抽整页字（桌面实测 1.5 s/页，
/// 手机上 3–5 s），打开一本 210 页的 PDF 时那几页串起来就是界面卡住十秒
/// （见 `model::fixed_book` 的 `text` 那一栏）。这一帧先空着字层 —— 页图照画 ——
/// 后台取回来再补，观感是「页先出来，选中与批注痕迹晚几帧」。
fn request_page_texts(
    book: &FixedBook,
    pages: impl IntoIterator<Item = usize>,
    reader: &WeakEntity<ReaderSession>,
    cx: &mut Context<ReaderSession>,
) {
    for page in pages {
        // 缓存已有 / 已在飞 / 页号越界时 `request_page_text` 给 `None`，这一帧不重复排活儿。
        let Some(doc) = book.request_page_text(page) else {
            continue;
        };
        let fetch = {
            let doc = doc.clone();
            cx.background_spawn(async move { doc.page_text(page) })
        };
        let reader = reader.clone();
        cx.spawn(async move |_, cx| {
            let text = fetch.await;
            let _ = reader.update(cx, |session, cx| {
                // `doc` 已经不是当前这本（期间换过书）时 `store_page_text` 回 false，
                // 这一帧不必为旧书重画。
                if session.accept_page_text(&doc, page, text) {
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

fn fixed_page_tiles(
    book: &FixedBook,
    page: usize,
    usable: f32,
    viewport_height: f32,
    header_reserve: f32,
    left: f32,
    has_text: bool,
) -> (Vec<FixedTileAt>, Option<Bounds<Pixels>>) {
    let tiles = book.page_tiles(page);
    let box_aspect = usable / viewport_height.max(1.0);
    let row_aspect = fixed_row_aspect(&tiles, box_aspect);
    if row_aspect <= 0.0 {
        return (Vec::new(), None);
    }
    let height = viewport_height.min(usable / row_aspect);
    let top = header_reserve + (viewport_height - height).max(0.0) / 2.0;
    fixed_page_tiles_at(tiles, usable, height, top, left, box_aspect, has_text)
}

/// 一条页行在 1x 里的 `(左上角, 宽高)`：一页摆几格时它们是同一高度横排的。
///
/// 空表给 `None`（那一帧一格的像素都还没解出来），缩放几何于是退回「放不大」。
fn fixed_row_bounds(tiles: &[FixedTileAt]) -> Option<((f32, f32), (f32, f32))> {
    let first = tiles.first()?;
    let last = tiles.last()?;
    Some((
        (first.x, first.y),
        (last.x + last.width - first.x, first.height),
    ))
}

/// 一条页栈里的一页：高度已经由 [`crate::ui::features::reader::model::fixed_book::FixedStack`]
/// 按版心宽算好，这里只把每一格按同一高度横排、摆到 `top` 上。
fn fixed_page_tiles_at(
    tiles: Vec<PageTile>,
    usable: f32,
    height: f32,
    top: f32,
    left: f32,
    fallback_aspect: f32,
    has_text: bool,
) -> (Vec<FixedTileAt>, Option<Bounds<Pixels>>) {
    let row_aspect = fixed_row_aspect(&tiles, fallback_aspect);
    if row_aspect <= 0.0 {
        return (Vec::new(), None);
    }
    // 整行在**版心**里居中：版心左缘是 `left`（页边距），不是窗口的 0。页栈那一档
    // `row_aspect * height` 正好等于版心宽，这里退化成从 `left` 开始 —— 右边因此与
    // 左边留下同一个页边距，而不是把整条行贴在窗口左缘、把两倍页边距全丢在右边。
    let mut x = left + (usable - row_aspect * height) / 2.0;
    let mut placed = None;
    let mut out = Vec::with_capacity(tiles.len());
    for tile in tiles {
        let width = fixed_tile_aspect(&tile, fallback_aspect) * height;
        if has_text && tile.renders && tile.request.is_some() {
            placed = Some(Bounds::new(
                point(px(x), px(top)),
                size(px(width), px(height)),
            ));
        }
        out.push(FixedTileAt {
            tile,
            x,
            y: top,
            width,
            height,
        });
        x += width;
    }
    (out, placed)
}

fn fixed_tile_aspect(tile: &PageTile, fallback: f32) -> f32 {
    if tile.width == 0 || tile.height == 0 {
        fallback
    } else {
        tile.width as f32 / tile.height as f32
    }
}

fn fixed_row_aspect(tiles: &[PageTile], fallback: f32) -> f32 {
    tiles
        .iter()
        .map(|tile| fixed_tile_aspect(tile, fallback))
        .sum()
}

fn fixed_marks(
    cache: &RefCell<AnnotationSpans>,
    stored: &[Annotation],
    page: usize,
    key: u64,
    text: &folio_core::content::format::PageText,
    at: Bounds<Pixels>,
    palette: &Palette,
) -> Vec<Mark> {
    if stored.is_empty() {
        return Vec::new();
    }
    let map = text.text_map();
    let spans = annotations::resolved_for(cache, stored, page, key, &map);
    let mut out = Vec::new();
    for span in spans.iter().filter(|span| !span.is_stale()) {
        let boxes = selection::page_boxes(text, span.start..span.end, at);
        let note_anchor = (!span.note.trim().is_empty())
            .then(|| boxes.last().copied())
            .flatten();
        let solid = palette.color(&span.color);
        let (lines, color, wavy) = match span.style {
            // 底色带压透明度：那一块是整片铺在字上的，实色会把字吃掉。
            AnnotationStyle::Highlight => (
                boxes,
                palette.color(&span.color).opacity(HIGHLIGHT_ALPHA),
                false,
            ),
            AnnotationStyle::Line => (
                strip_bottom(boxes, UNDERLINE_THICKNESS),
                palette.color(&span.color),
                false,
            ),
            AnnotationStyle::Wavy => (
                strip_bottom(boxes, paint::WAVE_HEIGHT),
                palette.color(&span.color),
                true,
            ),
        };
        for line in lines {
            out.push(if wavy {
                Mark::Wave(line, color)
            } else {
                Mark::Quad(line, color)
            });
        }
        if let Some(last) = note_anchor {
            out.push(Mark::Note(page_note_icon(last, at), solid));
        }
    }
    out
}

/// 定版式那一页上的便签图标：贴在末字右侧，夹在**页框**里 —— PDF 页几乎铺满屏，
/// 不做这一道夹，选到行尾时图标会被切在屏幕外。
fn page_note_icon(last: Bounds<Pixels>, page: Bounds<Pixels>) -> Bounds<Pixels> {
    const ICON: f32 = 12.0;
    let page_left = f32::from(page.origin.x);
    let page_top = f32::from(page.origin.y);
    let page_right = page_left + f32::from(page.size.width);
    let page_bottom = page_top + f32::from(page.size.height);
    let left = (f32::from(last.right()) + 2.0)
        .min(page_right - ICON - 1.0)
        .max(page_left + 1.0);
    let top = (f32::from(last.center().y) - ICON * 0.5)
        .min(page_bottom - ICON - 1.0)
        .max(page_top + 1.0);
    Bounds::new(point(px(left), px(top)), size(px(ICON), px(ICON)))
}

/// 把每块矩形压成贴着**盒底**的那一条（字下那两档画法要的就是它）。
fn strip_bottom(mut boxes: Vec<Bounds<Pixels>>, thickness: f32) -> Vec<Bounds<Pixels>> {
    for box_ in &mut boxes {
        let bottom = f32::from(box_.origin.y) + f32::from(box_.size.height);
        box_.origin.y = px(bottom - thickness);
        box_.size.height = px(thickness);
    }
    boxes
}

/// 把一批痕迹盖在页图**之上**的那一层。
///
/// 为什么定版式要另起一层，而不是复用字形元素里那一份 `mark`：这一族的字就在那张
/// 栅格出来的图里，而页图排在 `element` 之后入树，gpui 的 z-order 是后入树者居上 ——
/// 压在字形之下等于没画。画法一行都不重写：走 [`paint::paint_mark`]，与字形元素里
/// 那一份逐像素同一条式子（波浪的起伏排布、实线的落法都在那一处）。
fn marks_above(marks: Vec<Mark>) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            for mark in &marks {
                paint::paint_mark(mark, bounds.origin, window);
            }
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

/// 划线那条实线多厚（逻辑像素）。够看清、又不至于把字下面的笔画糊成一团。
const UNDERLINE_THICKNESS: f32 = 2.0;

/// 高亮带那一片压的透明度。`0.35` 是这一档还是唯一画法时量下来的那个数：整片铺在字下、
/// 不能把字吃掉，又要在浅深两套主题里都读得出「这一句我划过」。
///
/// 只给底色带那一档用。字下那两条线是实色 —— 它们不占字的背景，压了反而看不清是哪一档。
const HIGHLIGHT_ALPHA: f32 = 0.35;

/// 一段批注在**可见带内**字下那两块矩形（元素局部）：每行盒一块，追加进 `out`。
///
/// 与 [`highlight_rects`] 同一套坐标换算，只把「行盒整块」换成「字下那条线」：
/// 线落在基线下 `descent × 0.618` 处（与 [`crate::ui::features::reader::view::paint`] 里 `decoration_y` 同源，
/// 所以和正文自己的 `text-decoration: underline` 画在同一条横线上）。行盒的
/// `baseline` 是「行顶到基线的距离」（`layout::collect` 的 `acc.ascent`，半 leading 已含），
/// 于是 `height − baseline` 就是基线下那段。
/// `thickness` 决定这块矩形多厚 —— 实线给 [`UNDERLINE_THICKNESS`]，波浪给
/// [`paint::WAVE_HEIGHT`]（那条线要起伏，盒子得比线本身高出一倍峰峰值）。两种厚度**压在
/// 同一条中线**上：只按顶边对齐的话，读者换一个画法会发现痕迹往下跳了一小截 ——
/// 那是这一屏里最容易被看见的错。
///
/// 这一条只算横排。竖排那一线该往哪偏，`selection_rects_in_band` 给的**物理**矩形里
/// 没有这个信息（要问的是推进轴上的 `line.baseline`），所以竖排的书只有底色带那一档
/// 是对的（它复用 [`highlight_rects`]，那条认轴）。字下那两档在竖排里照旧画进行盒的
/// `y` 那一维 —— 已知限制，与盒模型那几族边一起留给竖排的第二轮。
#[allow(clippy::too_many_arguments)]
fn annotation_underline_rects(
    doc: &Document,
    window: &Window,
    cx: &App,
    range: std::ops::Range<usize>,
    x: f32,
    origin_y: f32,
    header: f32,
    band: std::ops::Range<f32>,
    thickness: f32,
    out: &mut Vec<Bounds<Pixels>>,
) {
    if range.start >= range.end {
        return;
    }
    let layout = doc.layout();
    for rect in doc.selection_rects_in_band(window, cx, range, band.clone()) {
        if rect.y + rect.height <= band.start || rect.y >= band.end {
            continue;
        }
        // 这一行盒的基线。`rects_for_range_in` 给的 `y` 就是行盒的 `line.y`，
        // 行盒按 y 单调，所以 `+0.5` 落回它自己那一行（`line_at_y`）。
        let line = &layout.lines[line_at_y(layout, rect.y + 0.5)];
        let descent = (rect.height - line.baseline).max(0.0);
        let underline_y = rect.y + line.baseline + descent * 0.618;
        // 那条 2 像素实线的中心。波浪那块更厚的盒子围绕同一个中心摆。
        let center = underline_y + UNDERLINE_THICKNESS * 0.5;
        out.push(Bounds::new(
            point(
                px(x + rect.x),
                px(center - thickness * 0.5 - origin_y + header),
            ),
            size(px(rect.width), px(thickness)),
        ));
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

/// 一块背景图：外壳是那一块的边框盒、带矩形裁剪，里面按 `scene` 算好的格子摆图。
///
/// 为什么要外壳那一层 `overflow_hidden`：`cover` 折出来的格子**本来就比盒大**
/// （缩到盖满，多出来的一截要裁掉），而 `repeat` 那一格会从定位那一点向两头溢出。
/// gpui 的 `ContentMask` 只有矩形 —— 这里要的正好是矩形，够用。
///
/// 坐标：外壳落在 `decor.x/top`（章内量，平移已由 `place_decorations` 加过），
/// 里面的格子相对外壳左上角，与 [`BlockPicture::tiles`] 的定义一致。
fn background_child(
    decor: &BlockDecor,
    picture: &BlockPicture,
    rendered: Arc<RenderImage>,
) -> AnyElement {
    div()
        .absolute()
        .left(px(decor.x))
        .top(px(decor.top))
        .w(px(decor.width))
        .h(px(decor.height))
        .overflow_hidden()
        .children(picture.tiles.iter().map(|(left, top, width, height)| {
            div()
                .absolute()
                .left(px(*left))
                .top(px(*top))
                .w(px(*width))
                .h(px(*height))
                // 格子已经是「按比例算好的那一个矩形」，所以这里要的是铺满而不是再保比例：
                // 再套一层 `object_fit` 会在格子与比例对不上时**悄悄改小**，露出底色。
                .child(img(rendered.clone()).size_full())
        }))
        .into_any_element()
}

/// 一块实心矩形（背景或一条边框）。
fn fill_quad(x: f32, top: f32, width: f32, height: f32, color: Hsla) -> AnyElement {
    fill_rounded_quad(x, top, width, height, color, [0.0; 4])
}

/// 同 [`fill_quad`]，多给四个圆角半径（`[左上, 右上, 右下, 左下]`，px）。
///
/// 圆角走 gpui 自己那条通道（`corner_radii`），不是自己拼路径：四边形是唯一轴对齐的图元，
/// 而 `paint_quad` 的圆角与它同一份几何，自己拼一条圆角路径只会多一套对不齐的边。
fn fill_rounded_quad(
    x: f32,
    top: f32,
    width: f32,
    height: f32,
    color: Hsla,
    radius: [f32; 4],
) -> AnyElement {
    div()
        .absolute()
        .left(px(x))
        .top(px(top))
        .w(px(width))
        .h(px(height))
        .bg(color)
        .corner_radii(Corners {
            top_left: px(radius[0]),
            top_right: px(radius[1]),
            bottom_right: px(radius[2]),
            bottom_left: px(radius[3]),
        })
        .into_any_element()
}

/// 一块盒子的块级装饰：背景一整块 + 背景图 + 四边边框各一条实心条。
/// 几何与 [`image_child`] 同一坐标系；边条在角上互相压边，与 CSS 的 45° 拼角
/// 差一个斜切，肉眼分不出来。
///
/// **三段的先后就是 CSS 的层次**：底色在最下、图盖在底色上、边框再盖在图上
/// （`background-clip` 我们一律按边框盒，所以那道边正好压在底图边上，与浏览器一致）。
/// 图层倒着摆：`background-image` 列表里最后写的那一层垫底，而 gpui 的子元素是
/// 「后入树的压在上面」，两个次序正好反。
fn decoration_child(
    decor: BlockDecor,
    atlas: &Entity<ImageAtlas>,
    view: EntityId,
    cx: &mut App,
) -> Vec<AnyElement> {
    let mut parts = Vec::new();
    if let Some(background) = decor.background {
        parts.push(fill_rounded_quad(
            decor.x,
            decor.top,
            decor.width,
            decor.height,
            background,
            decor.radius,
        ));
    }
    for picture in decor.pictures.iter().rev() {
        // 问一次 `painted` 就是替这一格记一笔「这一帧在场」，帧末的回收按这个数放人 ——
        // 与正文插图同一本账，扉页那张底图不会被单独饿掉。
        let Some(rendered) = painted_picture(atlas, &picture.image, view, cx) else {
            continue;
        };
        parts.push(background_child(&decor, picture, rendered));
    }
    let [top, right, bottom, left] = decor.border;
    if top.0 > 0.0 {
        push_border(
            &mut parts,
            decor.x,
            decor.top,
            decor.width,
            top.0,
            top.1,
            top.2,
            0,
        );
    }
    if bottom.0 > 0.0 {
        push_border(
            &mut parts,
            decor.x,
            decor.top + decor.height - bottom.0,
            decor.width,
            bottom.0,
            bottom.1,
            bottom.2,
            2,
        );
    }
    if left.0 > 0.0 {
        push_border(
            &mut parts,
            decor.x,
            decor.top,
            left.0,
            decor.height,
            left.1,
            left.2,
            3,
        );
    }
    if right.0 > 0.0 {
        push_border(
            &mut parts,
            decor.x + decor.width - right.0,
            decor.top,
            right.0,
            decor.height,
            right.1,
            right.2,
            1,
        );
    }
    parts
}

/// 沿一条边框带子的长边切出「实、空、实…」，返回每段的 `(起点, 长度)`。
///
/// **不平分**：从起点按 `dash + gap` 一路铺，最后一段按剩余长度截短 —— CSS 的虚线本来
/// 就是从一端铺到另一端、尾巴可能是半段，浏览器同样这么做。`dash` 非正或 `gap` 为负时
/// 给空表，防止调用方给一个除零的线宽时把这一帧卡在这条循环里。
fn dash_spans(length: f32, dash: f32, gap: f32) -> Vec<(f32, f32)> {
    // 三个数都得是有限正数（`gap` 可以是 0，那是实线）：NaN 落进下面这条 `while`
    // 会一直为真，所以先一次判完再进循环。
    let usable = length > 0.0 && dash > 0.0 && gap >= 0.0 && length.is_finite();
    if !usable {
        return Vec::new();
    }
    let mut spans = Vec::new();
    let mut at = 0.0f32;
    while at < length {
        let len = dash.min(length - at);
        spans.push((at, len));
        at += dash + gap;
    }
    spans
}

/// 3D 边框的亮/暗两档：从作者的边框色派生。
///
/// 浏览器在 HSL 空间里调明度；这里同一条思路，但两档都写成**朝目标补**而不是加减常数 ——
/// 加减会把纯黑压成纯黑（亮档看不见）、把纯白顶成纯白。纯黑因此只能变亮、纯白只能变暗，
/// 与浏览器观察到的行为一致。
fn three_d_tone(color: Hsla, light: bool) -> Hsla {
    let l = if light {
        color.l + (1.0 - color.l) * 0.35
    } else {
        color.l * 0.7
    };
    Hsla { l, ..color }
}

/// 把一条边框带子沿**厚度**方向切成外、内两半 → `(外, 内)` 两个盒子。
///
/// 「外」= 离盒子外缘更近的那一半：上边与左边是前一半，下边与右边是后一半。
fn split_outer_inner(x: f32, y: f32, width: f32, height: f32, side: usize) -> ([f32; 4], [f32; 4]) {
    match side {
        0 => (
            [x, y, width, height / 2.0],
            [x, y + height / 2.0, width, height / 2.0],
        ),
        2 => (
            [x, y + height / 2.0, width, height / 2.0],
            [x, y, width, height / 2.0],
        ),
        1 => (
            [x + width / 2.0, y, width / 2.0, height],
            [x, y, width / 2.0, height],
        ),
        _ => (
            [x, y, width / 2.0, height],
            [x + width / 2.0, y, width / 2.0, height],
        ),
    }
}

/// 一条边框带子。`side` 是 `[上, 右, 下, 左]` 的下标 —— 3D 那四档要靠它分「哪条边暗」，
/// `dotted` / `dashed` 靠它分切段方向。
///
/// 各档画法：`solid` 一整条；`dotted` / `dashed` 按 [`dash_spans`] 切段；`double` 三段等分
/// （外线 1/3、缝 1/3、内线 1/3）；`inset` / `outset` 上/左与下/右各一档明暗；
/// `groove` / `ridge` 每条边再拆成外、内两半。
#[allow(clippy::too_many_arguments)]
fn push_border(
    parts: &mut Vec<AnyElement>,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    color: Hsla,
    style: BorderStyle,
    side: usize,
) {
    // 上/下边是横带子（沿纵向拆厚度），左/右边是竖带子。
    let split_y = matches!(side, 0 | 2);
    match style {
        BorderStyle::Inset | BorderStyle::Outset => {
            // `inset`：上/左暗、下/右亮；`outset` 相反。
            let dark = (style == BorderStyle::Inset) == matches!(side, 0 | 3);
            parts.push(fill_quad(x, y, width, height, three_d_tone(color, !dark)));
            return;
        }
        BorderStyle::Groove | BorderStyle::Ridge => {
            // `groove` 外暗内亮；`ridge` 相反。
            let outer_light = style == BorderStyle::Ridge;
            let (outer, inner) = split_outer_inner(x, y, width, height, side);
            for (rect, light) in [(outer, outer_light), (inner, !outer_light)] {
                parts.push(fill_quad(
                    rect[0],
                    rect[1],
                    rect[2],
                    rect[3],
                    three_d_tone(color, light),
                ));
            }
            return;
        }
        _ => {}
    }
    if matches!(style, BorderStyle::Dotted | BorderStyle::Dashed) {
        // 带子的**长边**按段切开：`dotted` 方点边长 = 线宽、隙 = 线宽；
        // `dashed` 段 = 3×线宽、隙 = 3×线宽（浏览器那套比例，CSS 本身没规定）。
        // 点是方的不是圆的：绘制原语只有轴对齐四边形，1px 的点在屏上分不出圆方。
        let (long, thickness) = if split_y {
            (width, height)
        } else {
            (height, width)
        };
        let (dash, gap) = if style == BorderStyle::Dotted {
            (thickness, thickness)
        } else {
            (thickness * 3.0, thickness * 3.0)
        };
        for (at, len) in dash_spans(long, dash, gap) {
            parts.push(if split_y {
                fill_quad(x + at, y, len, thickness, color)
            } else {
                fill_quad(x, y + at, thickness, len, color)
            });
        }
        return;
    }
    if style != BorderStyle::Double {
        parts.push(fill_quad(x, y, width, height, color));
        return;
    }
    let seg = if split_y { height } else { width } / 3.0;
    if split_y {
        // 上/下边：两条横线落在这条带子的两端（3px 带 = 1px 线 / 1px 缝 / 1px 线）。
        parts.push(fill_quad(x, y, width, seg, color));
        parts.push(fill_quad(x, y + 2.0 * seg, width, seg, color));
    } else {
        // 左/右边：两条竖线同理。
        parts.push(fill_quad(x, y, seg, height, color));
        parts.push(fill_quad(x + 2.0 * seg, y, seg, height, color));
    }
}

#[cfg(test)]
mod tests;

/// `column_cut` 与 `capture_wanted` 的真值表。这两个判定是纯函数，用不着窗口，
/// 按仓库约定内联在被测文件底下 —— 不占宿主套件里一个带窗测试的名额。
#[cfg(test)]
mod truth_tables {
    use super::{
        capture_wanted, column_cut, dash_spans, in_viewport, sheet_area_in_page, text_ink_box,
        three_d_tone,
    };
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
        assert!(24.4 + 6.9 > 27.0, "这一条要钉的就是度量盒高出行盒那一格");
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
            let rect = super::capture_region(sheet, 0.0);
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
                    !in_viewport(super::capture_region(grown, 0.0), (384.0, 853.0)),
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
