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

use std::cell::RefCell;
use std::ops::Range;
use std::sync::Arc;

use folio_core::PageTurn;
use folio_core::content::anchor::line_at_y;
use folio_core::content::format::css::computed::BorderStyle;
use folio_core::layout::FontKey;
use folio_core::layout::fragment::column_offset;
use folio_core::store::{Annotation, AnnotationStyle};
use gpui_kit::base::motion::{Presence, TransitionId};
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    A11ySubtreeBuilder, AnyElement, App, Background, Bounds, ClickEvent, Context, Div, ElementId,
    Entity, EntityId, Hsla, InteractiveElement as _, IntoElement, LongPressEvent, PageCurlMesh,
    PageCurlUniforms, PageCurlVertex, ParentElement, PathBuilder, Pixels, Point, Render,
    RenderImage, Role, ScrollWheelEvent, SharedString, StatefulInteractiveElement as _, Styled,
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
use crate::ui::features::reader::model::image_atlas::ImageAtlas;
use crate::ui::features::reader::model::session::{
    AnnotationSpans, CurlMesh, PageRef, ReaderSession, Source, Turn, TurnRun, land_at_anchor,
    start_turn,
};
use crate::ui::features::reader::model::swipe::release_transition;
use crate::ui::features::reader::motion::turn_animation::{self, Sheet, Tint};
use crate::ui::features::reader::view::paint::{self, GlyphRun, GlyphsElement, Mark, shape};
use crate::ui::features::reader::view::scene::{BlockDecor, Scene, VisibleImage};
use crate::ui::foundation::images::DecodeRequest;
use crate::ui::services::config::ConfigHolder;
use crate::ui::shell::navigation::SizeClass;
use crate::ui::shell::{turn_duration, turn_transition};

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
    fn turn_frame(&mut self, window: &mut Window, cx: &mut App) -> Option<TurnFrame> {
        let turn = self.turn?;
        // 进度由谁给，看这一趟是谁在驱动它。
        //
        // 手指还握着时**不采时钟**：`Presence` 的意义是「自己续帧追一个终点」，而此刻终点
        // 在手指上，两头一起动就是画面在拉扯。
        let (progress, walked) = match turn.run {
            TurnRun::Following { t } => (t, false),
            _ => {
                // `present` 给 `true`：这一趟只需要「进场」那半 —— 走了多少就是 `progress`，
                // 出场那一页由同一个数反着算（`turn_animation::slide::slide_shifts` 那两个返回值），
                // 所以不必再为出场页单独挂一把 `Presence`。
                // 松手那一腿有自己的时长与曲线：它要以松手那一刻的速度起步（
                // `swipe::release_duration` / `release_shape`），而共用那一条 `EaseInOut`
                // 两端速度都是 0，接手就是「手一抬纸先停死」。点屏 / 按键那一趟用的是读者
                // 那一格的快照（`Turn::over`）—— 它没有前一段速度要接，但也**不能**每帧现读：
                // 拖那条滑条每一帧都在写配置，现读等于给正在走的时钟每帧重新瞄准。
                let transition = match turn.run {
                    TurnRun::Released { over, .. } => release_transition(over),
                    _ => turn_transition(turn.over, cx),
                };
                let id = TransitionId::from(ElementId::from(("reader-turn", turn.seq)));
                let clock = Presence::new(id, true)
                    .transition(transition)
                    .sample(window, cx)
                    .progress;
                // 松手那一腿不是从 0 走的：从手指那一点接着走完或走回，否则读者会看见
                // 自己握过的那一截被重放一遍。
                let progress = match turn.run {
                    TurnRun::Released { from, revert, .. } => {
                        chrome::tween(from, if revert { 0.0 } else { 1.0 }, clock)
                    }
                    _ => clock,
                };
                (progress, clock >= 1.0)
            }
        };
        if walked {
            // 弹回那一趟不落地：它「走完」的意思是回到出发那一张，页号与锚章一个字都不动。
            if turn.run.lands()
                && let Some(to) = turn.arriving
                && to.chapter != self.chapter_index()
            {
                self.finish_chapter_turn(to);
            }
            // 走完的那一帧起就没有第二张要画了：进场那张的位移正好归零，出场那张已经走满
            // `dist` 出画。少摆一张省下一趟场景重建，而**减弱动态效果那一档走的就是这条路**
            // （`Presence` 在 `reduce_motion` 时直接给终值）⇒ 那一档是当场落定，不多画一张纸。
            self.turn = None;
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
        // 章界那一趟起时要抄的档位与时长。在 `self` 被拆成字段借之前采好：`begin_turn` 那种
        // 要整块 `&mut self` 的方法在下面那臂里调不动。
        let page_turn = ConfigHolder::appearance(cx).page_turn;
        let turn_over = turn_duration(cx);
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

        // 那一页纸占的矩形，**元素坐标**（与字形的 `run.x` / `run.baseline` 同一把尺）：
        // 折线、逐字裁剪、折面多边形、网格的页框、手指坐标的归一化，全算在这一套里。
        //
        // 只有这一处定义它。从前捕获纹理按 `content_bounds`（字形元素自己的那块）烘、折线按
        // 这一块摆、手指按 `content_bounds` 归一 —— 三个数差着页边距，卷起来那一片采到的是
        // 横移过的页面，读成「纸上的字与底下那半页对不齐」。
        let area = turn_animation::Area::new(
            0.0,
            self.header_reserve,
            f32::from(viewport_size.width),
            self.header_reserve + viewport_height,
        );
        self.page_area = area;

        // 仿真翻页的 GPU 网格采样的是一张**旧页纹理**。页号/章号变了、或偏好重排过
        // 才重烘；同一页的静止帧不再每帧跑一整趟捕获 pass。动画期间不烘焙 ——
        // 那时场景里已经多了一张新页，烘出来的就不是正在被翻的那张。
        //
        // 这里只决定「这一帧要不要烘」。真正发请求要等到那块 canvas 的绘制回调（见下面
        // `capture_region` 那一格）：只有在那里才知道这一栏在窗口里的真实原点，而那个原点
        // 必须与网格取 uv 用的那一块是同一个数。
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
        let capture_needed = turn.is_none()
            && !mode.is_continuous()
            && (self.capture_dirty || self.captured_page != Some(page_key));
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
        // 这一档这一帧要盖在两张纸之上的那几块：折页的纸面与折谷四道，覆盖/层叠的前缘
        // 投影与纸边光。其余档位给的是空清单，所以下面那一块一个形状都不会入树。
        let mut shades = turn_animation::Shades::new();
        // C 方案：这一帧要画的卷曲网格，以及它对应的进场页。网格取代旧页的
        // 字形与图片 —— 旧页已经烘进页面纹理，网格自己把它卷起来。
        let mut gpu_curl_mesh: Option<PageCurlMesh> = None;
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
                // 兜底那一行也是正文位置上的字：读屏该念到它，不然这一状态整屏是静音的。
                if speaking {
                    spoken.push(text.clone());
                }
            }
            Source::Fixed(book) => {
                // 定版式：页是既定的，没有排版链。摆当前页，等比缩进视口、居中。
                let pages = book.page_count();
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
                // 一页可能摆好几格（多图页 / 对开跨页，或渲染型的一整页）：按**共享高度**
                // 横成一行，整体居中。一格时这条式子退化成「等比缩进视口」，与普通漫画的
                // 旧表现逐像素相同。
                let tiles = book.page_tiles(current);
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
                let row_aspect: f32 = tiles
                    .iter()
                    .map(|tile| aspect(tile.width, tile.height))
                    .sum();
                // 这一页有没有字：PDF / AZW4 / DjVu 那一族才有（漫画的一页就是一张图，
                // EPUB 的定版式字在 XHTML 里而不在页上）。问一次用三处 —— 长按归谁、
                // 命中那把尺记不记、上面那两层画不画 —— 而每一次问的都是会话里那份
                // 按页备忘（`FixedBook::page_text`），不在这一帧里回引擎第二趟。
                let text = book.page_text(current);
                let text_key = book.page_text_key(current);
                let mut placed: Option<Bounds<Pixels>> = None;
                if row_aspect > 0.0 {
                    let height = viewport_height.min(usable / row_aspect);
                    let y = self.header_reserve + (viewport_height - height).max(0.0) / 2.0;
                    // 整行居中：先从左边把行宽的余量让出一半，再逐格往右推进。
                    let mut x = (usable - row_aspect * height) / 2.0;
                    for tile in tiles {
                        let width = aspect(tile.width, tile.height) * height;
                        // 解不出像素的那一格空着，但仍然占位：多图页的排版不该因为
                        // 少一张图就把剩下的往左挤。
                        if let Some(request) = tile.request {
                            let key = request.key.clone();
                            let Some(image) = painted(&atlas, request, view, cx) else {
                                continue;
                            };
                            // 带字的那一格才有选中：它的矩形就是「屏幕点 ↔ 页内字序」
                            // 那一把尺唯一该记的地方。
                            let selected = text.is_some() && tile.renders;
                            if selected {
                                placed = Some(Bounds::new(point(px(x), px(y)), size(px(width), px(height))));
                            }
                            // 「看大图」那一格在这一族里按端分岔（`page_image` 顶上写着）：
                            // 手机上它吃的是**长按**，而那正是选中要的手势 —— 于是有字的
                            // 那一页不再挂它（入口挪进浮层那颗按钮），没字的页照旧。
                            // 桌面那一端挂的是**单击**，与拖选不打架（拖过阈值那一下的合成
                            // 点击由 `open_on_tap` 里那一条吃掉），所以原样留着。
                            let no_entry = None;
                            let entry = if selected && long_press { &no_entry } else { &viewer };
                            let index = overlays.len();
                            overlays.push(page_image(
                                x,
                                y,
                                width,
                                height,
                                image,
                                key,
                                entry,
                                long_press,
                                index,
                                &reader_weak,
                            ));
                        }
                        x += width;
                    }
                }
                // 命中换算读的那一把尺，与上面摆出去的那一格同源。每一帧都写：
                // 没有字的那一页必须把它清干净，否则留着上一页的矩形，长按会按新页的
                // 字序去解旧页的盒子 —— 症状是色块整片画在别处。
                self.fixed_text_box.set(placed.map(|at| (current, at)));
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
                if let (Some(text), Some(at)) = (&text, placed) {
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
                    overlays.push(selection::overlay(visual, &reader_weak));
                }
            }
            Source::Book(book) => {
                if mode.is_continuous() {
                    // —— 连续滚动：锚章 ± 窗口拼成一条全局坐标 ——
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
                    let __t = crate::ui::features::reader::model::perf::Timer::start();
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
                    crate::ui::features::reader::model::perf::add(|frame| {
                        frame.spine_ns += __spine
                    });
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
                            local,
                            band.start - placed.base_y,
                            // 连续滚动不分片，也就永远不走竖排（`allow_vertical` 那一档），
                            // 长度这一格在横排那条路里根本不参与，递整章的长只为读起来不心虚。
                            doc.height(),
                            self.prefs.margin_x,
                            self.header_reserve,
                            foreground,
                            link,
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
                                long_press,
                                index,
                                &reader_weak,
                            ));
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
                            self.header_reserve,
                            (band.start - placed.base_y)..(band.end - placed.base_y),
                            // 连续滚动这一臂只摆一张纸（翻页就是补间那一个 y），没有要裁的
                            // 另一张 ⇒ 不裁。
                            None,
                        );
                        let __fills = __fills.ns();
                        crate::ui::features::reader::model::perf::add(|frame| {
                            frame.fills_ns += __fills
                        });
                    }
                } else {
                    // —— 分页 / 双页：当前页 +（动画时）同章邻页或跨章目标页 ——
                    let paged = Viewport {
                        reserve: self.reserve.width,
                        columns,
                        height: viewport_height,
                        gap,
                    };
                    let column_width = book.document_mut().ensure_viewport(window, cx, paged);
                    let pages = book.document().page_count();
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
                            start_turn(
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
                                    arriving: Some(PageRef {
                                        chapter: target,
                                        page: target_page,
                                    }),
                                    forward: delta > 0,
                                    ..Default::default()
                                },
                            );
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
                    if let Some(frame) = turn {
                        let style = turn_animation::of(frame.style);
                        // 自由边到底在哪一侧（往回翻与「从右往左读的那一本」各反一次）：折页那一档
                        // 的几何全按它判，别的档不看。
                        let free_right =
                            turn_animation::Pose::free_edge_right(frame.forward, frame.rtl);
                        // 手与弹回的时钟：走完那一腿把这一笔沿**它自己的方向**拉长到落点，弹回
                        // 那一腿不借手 —— 手指停在松手那一刻那一格，改由 `Pose::revert` 那一个数
                        // 让折线自己退出去。两条腿的第一帧都逐字等于松手那一刻，所以都不跳；
                        // 这一腿自己走了几成由 [`turn_animation::release_clock`] 给（两边的尺
                        // 不一样：那一腿 `progress` 是往 0 退的）。
                        let (grip, revert) = match frame.run {
                            TurnRun::Released { from, revert, .. } => {
                                let u =
                                    turn_animation::release_clock(frame.progress, from, revert);
                                if revert {
                                    (frame.grip, Some(u))
                                } else {
                                    (
                                        frame.grip.map(|grip| {
                                            turn_animation::release_grip(grip, u, free_right, area)
                                        }),
                                        None,
                                    )
                                }
                            }
                            _ => (frame.grip, None),
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
                                turn_animation::cylinder_for(frame.progress, area, pose)
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
                            let page_width = area.width();
                            let page_height = area.height();
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
                                    point(px(area.left), px(area.top)),
                                    size(px(page_width), px(page_height)),
                                );
                                gpu_curl_mesh = Some(PageCurlMesh {
                                    bounds: page,
                                    page_bounds: page,
                                    vertices: mesh.vertices,
                                    indices: mesh.indices,
                                    uniforms: PageCurlUniforms {
                                        page_origin: [area.left, area.top],
                                        // 采样原点：先按本帧的页框填，画那一趟的回调里再换成
                                        // 「烘那一刻」的那一个（`curl_bake_origin`）。层在动时
                                        // 两者不同，混用就会采到纹理里未写的区域。
                                        capture_origin: [area.left, area.top],
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
                                        light_dir: [-0.35, -0.25, 1.0, 0.0],
                                    },
                                });
                            }
                        }
                        // 折页那一档不再给 CPU 影子：纸面与圆柱受光都在 GPU 那张网格里
                        // （见 `curl.rs`、`page_curl.wgsl`）。`shades_at` 的默认实现给回空清单，
                        // 所以这一句对别的几档就是「照旧」。`pose` 是上面拼的那一份：带着**补间
                        // 之后**的手与弹回的时钟，网格与摆位层读的都是它。
                        shades = style.shades_at(frame.progress, area, pose);
                        let [incoming, outgoing] = style.sheets_at(frame.progress, area, pose);
                        let leaving_ref = frame.leaving.unwrap_or(PageRef {
                            chapter: current,
                            page: self.page,
                        });
                        let arriving_ref = frame.arriving.unwrap_or(PageRef {
                            chapter: current,
                            page: self.page,
                        });
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
                        // C 方案只摆进场那一张：旧页已经在捕获纹理里，由 GPU 网格卷。
                        // 进场页要整张画（不裁）—— 网格的平面段会盖住 `d < 0` 那一半。
                        if gpu_curl_mesh.is_some() {
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
                    for which in order.into_iter().take(count) {
                        let (page_ref, at) = slots[which];
                        // C 方案：进场页整张画，裁剪交给 GPU 网格的平面/卷曲面。
                        let cut = if gpu_curl_mesh.is_some() {
                            None
                        } else {
                            at.cut
                        };
                        let Some(doc) = book.span_doc(page_ref.chapter) else {
                            continue;
                        };
                        let slices = doc.slices();
                        let (shift_x, shift_y) = at.offset;
                        for index in 0..columns {
                            let Some(slice) = slices.get(page_ref.page * columns + index) else {
                                continue;
                            };
                            if slice.empty {
                                continue;
                            }
                            let x = self.prefs.margin_x
                                + column_offset(column_width, gap, slice.column)
                                + shift_x;
                            let top = self.header_reserve + shift_y;
                            // 分页/双页这一臂的「在屏」就是这一片自己的行区间（片是引擎切的，
                            // 一片一栏）。两栏按 `index` 依次接上去：`slices()` 的顺序是
                            // 「页优先、页内按栏」的**逻辑**栏序，所以 RTL 的书念出来也是
                            // 逻辑顺序 —— 与那一栏画在左边还是右边无关。
                            if speaking {
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
                            );
                            let __sh = crate::ui::features::reader::model::perf::Timer::start();
                            shape_runs(&mut scene, doc, window);
                            let __sh = __sh.ns();
                            crate::ui::features::reader::model::perf::add(|f| {
                                f.shape_runs_ns += __sh
                            });
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
                            // 块级装饰（`background-color`、`border`、`<hr>`）也跟着那张纸
                            // 走：中心落在裁掉那一侧就整块不画。字是逐点裁的，而一个装饰块
                            // 是一个矩形、没有「裁一半」的画法 —— 逐像素的多边形裁剪要的是
                            // gpui 没有的东西（`ContentMask` 只有矩形）。留白块比伸到隔壁那
                            // 张纸上的色块好读。
                            for decor in scene.decorations {
                                if cut.is_some_and(|cut| {
                                    !cut.keeps(
                                        decor.x + decor.width * 0.5,
                                        decor.top + decor.height * 0.5,
                                    )
                                }) {
                                    continue;
                                }
                                column.extend(decoration_child(decor));
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
                                let Some(rendered) = painted(&atlas, image.image.clone(), view, cx)
                                else {
                                    continue;
                                };
                                if !shown {
                                    continue;
                                }
                                let index = overlays.len();
                                overlays.push(image_child(
                                    image,
                                    rendered,
                                    &viewer,
                                    long_press,
                                    index,
                                    &reader_weak,
                                ));
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
                                page_ref.chapter,
                                x,
                                slice.origin_y,
                                slice.height,
                                top,
                                slice.origin_y..(slice.origin_y + slice.height + 1.0),
                                cut,
                            );
                            let __fills = __fills.ns();
                            crate::ui::features::reader::model::perf::add(|f| {
                                f.fills_ns += __fills
                            });
                        }
                    }
                }
            }
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
                        // 连续滚动永远走横排（竖排只在分页那一档落地），长度这一格不参与。
                        band_bottom - band_top,
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
                            slice.height,
                            self.header_reserve + self.page_shift.1,
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
        element = element.selection(visual);
        if matches!(self.source, Source::Book(_)) {
            element = element.reader(Some(cx.entity().downgrade()));
        }
        column = column.child(element);
        // 这一档要盖在两张纸**之上**的那几块：折页的纸面与它的四道阴影，覆盖/层叠的前缘
        // 投影与纸边光。它们都不属于任何一张纸 —— 两张纸的字形 runs 烘在同一个元素里，
        // 纸与纸之间没有可插入的绘制层 —— 所以只能作为一组盖上去的多边形来画。形状、渐变
        // 方向、浓淡全由 `turn_animation` 算好，这一头一个几何都不做。
        //
        // 层级是字形之上、图片之下：这些影子本来就落在纸面上，而图是纸面上的另一层内容。
        if gpu_curl_mesh.is_none() && !shades.items().is_empty() {
            let paper = cx.theme().background;
            column = column.child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        let origin = bounds.origin;
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
        // 烘一张旧页纹理：就是上面那块 `area`，原点按这一栏在窗口里的位置补齐。纹理是整窗
        // 大小、只有这一块被写过，而网格取 uv 用的是 `(页框原点 + 局部坐标) / 窗口` —— 两处
        // 必须同一个数，差一个页边距就读成「纸上的字与底下那半页叠在一起」。
        if capture_needed {
            let region = Bounds::new(
                point(px(area.left), px(area.top)),
                size(px(area.width()), px(area.height())),
            );
            // 捕获区清屏色必须取纸张底色；闭包里的 `window` 拿不到主题，所以先取好。
            let capture_clear = cx.theme().background.to_rgb();
            let committed = std::rc::Rc::clone(&self.capture_committed);
            column = column.child(
                canvas(|_, _, _| {}, {
                    let bake_origin = std::rc::Rc::clone(&self.curl_bake_origin);
                    move |bounds, _, window, _| {
                        let at = point(
                            bounds.origin.x + region.origin.x,
                            bounds.origin.y + region.origin.y,
                        );
                        let right = f32::from(at.x) + f32::from(region.size.width);
                        let bottom = f32::from(at.y) + f32::from(region.size.height);
                        // 捕获纹理只有整窗大。页框只要有一部分在窗外，那一部分就永远烘不进
                        // 纹理；而网格仍按完整页宽采 UV，右侧便会采到矩形外那条边（真机读数
                        // 是一条黑色横带）。此时**不请求捕获**，下一帧等整层进场滑完再重试。
                        if f32::from(at.x) < 0.0
                            || f32::from(at.y) < 0.0
                            || right > viewport.0
                            || bottom > viewport.1
                        {
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
                            w = f32::from(region.size.width),
                            h = f32::from(region.size.height),
                            "烘这一页：捕获矩形"
                        );
                        // 捕获矩形之外的清屏色取纸张底色：卷曲网格取 uv 落在矩形外
                        // 半像素时，透明黑就是页边那条黑带的来源（参照实现的
                        // `glClearColor(mBackgroundColor)` 用同一个口径）。
                        window.capture_page_region_with_color(
                            Bounds::new(at, region.size),
                            capture_clear,
                        );
                        committed.set(Some((page_key, viewport)));
                    }
                })
                .absolute()
                .inset_0(),
            );
        }
        // 原始触摸流那一格：折页要的是手指此刻的二维位置，而识别器折出来的事件只剩一根轴。
        column = column.child(touch_follow(reader_weak.clone()));
        // C 方案的卷曲网格：盖在新页与它的图片之上，UI 浮层之下。旧页的字、图、
        // 背景都在捕获纹理里，网格一次画完；这里只把窗口坐标补齐。
        if let Some(mesh) = gpu_curl_mesh {
            column = column.child(
                canvas(|_, _, _| {}, {
                    let bake_origin = std::rc::Rc::clone(&self.curl_bake_origin);
                    move |bounds, _, window, _| {
                        let mut mesh = mesh.clone();
                        let origin = bounds.origin;
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
                            mesh.uniforms.capture_origin = [x, y];
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
        // 帧末回收图集。**必须**在这一帧的元素树全部搭完之后：中途放掉的格子会被同一帧
        // 里的 `img` 重新传回去，白花一趟上传（`image_atlas::end_frame` 那两条不动的规矩）。
        self.atlas
            .update(cx, |atlas, cx| atlas.end_frame(window, cx));
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
        let listener = cx.listener(|this: &mut Self, event: &ScrollWheelEvent, _window, cx| {
            // 挪动了才重画：滚到章尾夹住、分页没攒够半屏时一整帧的重建可以整份省掉
            // （判据与 gpui 的 `div` 滚动容器、zed editor 的 `scroll` 一致）。
            if this.on_scroll(event, cx) {
                cx.notify();
            }
        });
        let body = div()
            .id("folio-reader")
            // 宿主的测试要按 id 把滚轮事件投到这里。非 `test-support` 构建里这一层是透明的
            // （`Observed` 原样转交内部元素），所以真机上不多花一个节点。
            .test_support()
            .size_full()
            .overflow_hidden()
            .on_scroll_wheel(listener)
            .child(column);
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
fn image_child(
    image: VisibleImage,
    rendered: Arc<RenderImage>,
    viewer: &Option<Entity<ImageViewer>>,
    long_press: bool,
    index: usize,
    reader: &WeakEntity<ReaderSession>,
) -> AnyElement {
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
    // 这一张在图集里的身份：整本书那一列就是按它认的（见 [`open_picture`]）。
    let key = image.image.key;
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
        .on_click(move |event: &ClickEvent, window: &mut Window, cx: &mut App| {
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
        })
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
        // 落位分轴：横排沿 y 减片原点，竖排沿 x 加 `slide`（推进轴从长条右缘反着数）。
        let left = x + rect.x + if vertical { slide } else { 0.0 };
        let top_at = if vertical {
            rect.y + header
        } else {
            rect.y - origin_y + header
        };
        out.push(Bounds::new(
            point(px(left), px(top_at)),
            size(px(rect.width), px(rect.height)),
        ));
    }
}

/// 一章的批注：库里的锚点 → 本章坐标 → 元素局部的一条条痕迹，追加进字形元素。
///
/// 换算口径与 [`highlight_rects`] 完全一致（同一套「这一栏摆在哪、可见带是哪一段」），
/// 只是来源从这一次挑中的选区换成了留在书上的批注。三档画法各要一块不同的矩形：
///
/// - **底色带**要的就是行盒那一整块，所以直接复用 [`highlight_rects`] —— 与选中高亮同一条
///   换算，顺带把竖排一起对上（那条认推进轴，而字下那两条不认，见
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
        let (lines, color, wavy) = match span.style {
            // 底色带压透明度：那一块是整片铺在字上的，实色会把字吃掉。
            AnnotationStyle::Highlight => (boxes, palette.color(&span.color).opacity(HIGHLIGHT_ALPHA), false),
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
    }
    out
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
        push_border(
            &mut parts,
            decor.x,
            decor.top,
            decor.width,
            top.0,
            top.1,
            top.2,
            true,
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
            true,
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
            false,
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
            false,
        );
    }
    parts
}

/// 把一条边框条推进 `parts`。
///
/// `solid` 就是整段一块；`double` 按 CSS 的口径三段等分 —— 外线 1/3、缝 1/3、内线 1/3，
/// 于是 3px 的双线画成两条 1px 细线。`split_y` 说明沿**纵**向拆（上/下边，条是横的）；
/// 为假则沿**横**向拆（左/右边，条是竖的）。
#[allow(clippy::too_many_arguments)]
fn push_border(
    parts: &mut Vec<AnyElement>,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    color: Hsla,
    style: BorderStyle,
    split_y: bool,
) {
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
