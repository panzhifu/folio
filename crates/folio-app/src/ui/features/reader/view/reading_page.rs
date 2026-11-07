//! 阅读页：正文 + 干净态的两行小字 + 点正文中间浮出的顶栏与底栏。
//!
//! 形态照 Reeden 的阅读界面量出来（1080×2400 @3.0）：干净态只剩左上角一行章名与底部
//! 一行进度；点正文中间才浮出顶栏（返回 + 书名）与底栏（进度条 + 一排入口）。
//! 那一排入口里带档位的两块 —— 目录与阅读设置 —— 各是盖住整屏的底部弹层，自己的状态住在
//! 自己的实体里（见 [`TocPanel`] 与 [`ReadingSheet`]）：底栏常驻只有那三行，不跟着
//! 展开的内容一起长高。
//!
//! 这一屏两端**共用一份**：桌面上原来的工具条 + 状态栏那套常驻 chrome 换成了这个形态，
//! 因为「以手机那一版为准」不只是挑一套皮肤 —— 两套 chrome 意味着每个新功能要落两次地，
//! 而其中一端一定会漏。桌面多出来的那一格是「双页」，它按宽度自己退档（见
//! [`chrome::PageMode::effective`]），所以这一格在窄窗口里就是分页，不需要 `cfg`。
//!
//! 顶栏与底栏**压在正文之上**，不推动正文重排：读者点一下屏幕，字不该跳走。
//! 正文只让出干净态那两条带的高度，那两个数住在 [`chrome`] 里，布局与视口算法共用它。
//!
//! 点击分区、浮层开合、模式退档都在 [`chrome`] 里判：那里是纯函数，宿主的 `cargo test`
//! 直接跑，这一屏只把结果落成动作。

use std::time::{Duration, Instant};

use folio_core::content::format::novel::rules::MatchKind;
use folio_core::infra::config::Mode;
use folio_core::net::ai::Ask;
use gpui_kit::assets::IconName;
use gpui_kit::base::motion::{Spring, spring};
use gpui_kit::base::{NavMotion, NavStackState};
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::popover::{Popover, PopoverState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::slider::{SliderEvent, SliderState};
use gpui_kit::component::{
    ActiveTheme, Disableable as _, Sizable as _, ThemeMode, ThemeStyled as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AbsoluteLength, Anchor, AnyElement, App, AppContext as _, AppLifecyclePhase, ClickEvent,
    Context, DefiniteLength, Entity, FocusHandle, Font, FontWeight, HighlightStyle, Hsla, Image,
    InteractiveElement, InteractiveText, IntoElement, ObjectFit, ParentElement, Pixels, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement, Styled, StyledImage as _, StyledText,
    Subscription, TestSupportExt as _, TextRun, TextStyle, WeakEntity, Window, WrappedLine, div,
    img, point, px,
};

use crate::ui::features::reader::components::image_viewer::{self, ImageViewer};
use crate::ui::features::reader::components::reading_sheet::ReadingSheet;
use crate::ui::features::reader::components::toc_panel::{TocEvent, TocPanel};
use crate::ui::features::reader::model::ai::AiStatus;
use crate::ui::features::reader::model::chrome::{self, Chrome, Effect, Reserve, zone};
use crate::ui::features::reader::model::dictionary::DictionaryStatus;
use crate::ui::features::reader::model::rules_draft::DraftStatus;
use crate::ui::features::reader::model::session::ReaderSession;
use crate::ui::features::reader::model::speech::split_sentences;
use crate::ui::features::reader::model::translation::{self, TranslationStatus};
use crate::ui::features::settings::CompactSettings;
use crate::ui::features::shelf::AUTHOR_SEP;
use crate::ui::foundation::images::cover_image;
use crate::ui::foundation::reading_theme;
use crate::ui::platform;
use crate::ui::services::config::ConfigHolder;
use crate::ui::services::l10n::{tf, tr};

/// 阅读页这一帧的正文调色板。根底色（纸）与兜底文字色都从它取 ——
/// 壳主题只管浮上来的那几层界面，不再管纸。
fn reading_palette(cx: &gpui_kit::App) -> reading_theme::ReadingPalette {
    reading_theme::resolve(&ConfigHolder::appearance(cx), cx)
}
use crate::ui::shell::AppShell;
use crate::ui::shell::navigation::SizeClass;

/// 脚注小纸片的最宽那一条：内容的 `max_w`，短的一条按自己实际宽居中在标记上方。
const FOOTNOTE_CARD_W: Pixels = px(300.);
/// 标记下缘与卡片之间的缝（三角长在缝里，尖正对着标记）。
const FOOTNOTE_GAP: Pixels = px(10.);
/// 翻译弹层那张卡的宽：引文与译文都按「一句到一段」估，320 装得下中英两栏
/// 各约 20 字 —— 与脚注那张同一族的尺寸。
const TRANSLATION_CARD_W: Pixels = px(320.);
/// 听书那一屏的两种面孔：封面 / 文字（Apple Music 歌词那种）。
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum ListenFace {
    #[default]
    Cover,
    Text,
}

/// 封面那一档的封面尺寸（3:4 左右，与架面同一条比例）。
const LISTEN_COVER_W: f32 = 208.0;
const LISTEN_COVER_H: f32 = 288.0;

/// 文稿那一列左右各让出多宽（`px_3` 那一档）。**摆上去与量折行的用的是这一个数**：
/// 页边与列宽的算式分在两处写，改一处就会让字顶出屏幕或者白留一条边。
const LISTEN_PAGE_X: f32 = 12.0;
/// 滚动条那一格（列表右缘让出来的宽）。
const LISTEN_SCROLLBAR: f32 = 8.0;
/// 文稿那一列的最大宽：平板 / 桌面上不让一行拉到一千多，视线要在一行里折返。
const LISTEN_TEXT_W: f32 = 640.0;
/// 书页文稿的字号。与歌词流那 22px 不同：这一面是**整段连续排版**，一行装得下
/// 三十来个字，22px 会挤成书本正文的密度反而难读；17px 与阅读设置默认正文字号一档。
const LISTEN_BOOK_PX: f32 = 17.0;
/// 文稿的行高。**量折行与画出来必须同一个数**：跟读的落点按「第几行 × 行高」算，
/// 画的那一侧若用了另一个行高，焦点就会一行一行地漂。段间空行也是它（`\n\n`）。
const LISTEN_LINE_H: f32 = 30.0;
/// 跟读那一趟弹簧的周期。句间平均 2–4 s（真机量到 227 ms/字），这半秒在下一句开口
/// 之前走完，读者的眼睛不用追声音。阻尼 0.85 = 过一点点就回来，不像定时器那样硬停。
const LISTEN_FOLLOW: Duration = Duration::from_millis(380);
/// 读者手动划过文稿之后让位多久，过了这一格自动跟读才接着抢 —— 与「手动翻页不暂停
/// 听书」同一条口径：读的人说了算。
const LISTEN_HAND_OVER: Duration = Duration::from_secs(4);

/// 听书文稿里的一句话：两套坐标。
#[derive(Clone)]
struct ListenSentence {
    /// 章内字符区间 —— TTS 的落点，直接可以交给 [`ReaderSession::speak_from`]。
    chapter: std::ops::Range<usize>,
    /// 文稿文本内的 **UTF-8 字节区间** —— 与 gpui 文本下标同一口径
    /// （`TextRun::len`、`position_for_index`、`InteractiveText` 的命中全是字节），
    /// 当前句高亮与「点哪句念哪句」都落在这上面。
    script: std::ops::Range<usize>,
}

/// 听书那一屏缓存的文稿：整章拼成一段连续文本，书页一样自然折行。
struct ListenScript {
    chapter: usize,
    /// 整章文稿文本。段间 `\n\n` —— 空行就是段距，书页感由它来。
    text: SharedString,
    /// 每一句的两套区间（章内 / 文稿内），顺序即朗读顺序。
    sentences: Vec<ListenSentence>,
    /// 每一句的文稿文本落在第几行（整章一次排版后数出来的）。跟读的落点按它算。
    line_of: Vec<usize>,
    /// 建表时这一列多宽（逻辑像素）—— 变了要重建，不然折点会落到屏幕外面。
    width: f32,
    /// 这张表是**整章**的吗。排版还没给段界时兜底摆出来的单句表是 `false`：它只够
    /// 把「正在念的那句」摆上屏，绝不能进缓存 —— 判据里放它一眼看中的
    /// `!sentences.is_empty()`，单句表就把整章的位置占了，焦点从此钉死在一句上，
    /// 声音念到哪文稿都不动（真机「文稿不跟读」的一种）。不完整就每帧重建，
    /// 段界一到整章的表自然落地。
    complete: bool,
}

/// 听书屏书头要的三样：一次查询就够，不每帧摸 SQLite。
struct ListenBook {
    title: SharedString,
    authors: SharedString,
    cover: Option<std::sync::Arc<Image>>,
}
/// 词典弹层那张卡的宽：释义比译文长，给宽一点；仍是与脚注、译文同一族的尺寸。
const DICTIONARY_CARD_W: Pixels = px(340.);

pub struct ReadingPage {
    pub(in crate::ui::features::reader) reader: Entity<ReaderSession>,
    /// 外壳报来的「不属于正文的那一段」。顶层盖满整窗，所以那一段只剩顶上那条系统状态栏 /
    /// 挖孔（底边见 [`ReadingPage::bottom_inset`]）—— `shell::reading_reserve`。
    pub(in crate::ui::features::reader) reserve: Reserve,
    /// 外壳进场横移的当前位移（逻辑像素）。捕获层要把这一格抵消掉，才能在整个
    /// 280 ms 进场期间按**最终位置**烘页，而不是等层停稳。
    pub(in crate::ui::features::reader) enter_shift: f32,
    /// 顶上那条系统状态栏 / 挖孔占掉多高（逻辑像素）。见 [`shell::reading_top_inset`]。
    ///
    /// 外壳**不**推整层（两种纸都从窗口 0 起，见 `shell::reading_overlay`），所以这一段
    /// 由屏幕上每一处自己让：顶栏那条的内容行从让完之后起（[`TopBar::overlay`]，它同时要
    /// 把表面铺到屏幕顶边，不然投影的上半截会被条子自己的边切成一条横线）、图片查看器那
    /// 一排控件、正文与会话的 `insets.top`。
    ///
    /// [`shell::reading_top_inset`]: crate::ui::shell::reading_top_inset
    /// [`TopBar::overlay`]: crate::ui::components::TopBar::overlay
    pub(in crate::ui::features::reader) top_inset: f32,
    /// 底下那条系统手势条占掉多高（逻辑像素）。
    ///
    /// 它**不拿去推整层**，只给底栏卡片与干净态页脚用：那两处把内容抬高 `bottom_inset`、
    /// 底色铺到屏幕底，所以卡片与屏幕底之间不是一条空缝（与读者那句「位于系统底部栏之上、
    /// 颜色保持一致」同一条口径）。正文那边已经由 `PageInsets` 让开它，不再重复扣。
    pub(in crate::ui::features::reader) bottom_inset: f32,
    /// 这一层自己的返回栈（弱句柄）。**正文是栈底**，从阅读页压进去的那一屏（设置）叠在
    /// 它上面 —— 借架面那条栈的话，压进来的那一屏会被这一层盖住。
    ///
    /// 弱是因为栈里存着正文自己的 `AnyView`：两边都攥紧就是互相拽着，这一层放手之后
    /// 两边都散不掉。
    pub(in crate::ui::features::reader) stack: WeakEntity<NavStackState>,
    /// 拿着这一层的外壳（同样是弱句柄：外壳攥着这一层，这一层不该反过来攥着外壳）。
    /// 顶栏那颗返回要请它收起这一层 —— 收层是外壳的事，正文这一屏不知道自己盖在谁上面。
    pub(in crate::ui::features::reader) shell: WeakEntity<AppShell>,
    /// 浮层（顶栏与底栏这一层）的开合。见 [`chrome`]。
    pub(in crate::ui::features::reader) chrome: Chrome,
    /// 进度滑条，底栏那一格。`Slider` 是有状态组件，句柄必须跨帧持有 —— 每帧新建会把拖动打断。
    pub(in crate::ui::features::reader) progress: Entity<SliderState>,
    /// 阅读设置那一屏。七条滑条、那颗翻页下拉、那只字体输入框与弹层档位都装在它自己
    /// 那个实体里（见 [`ReadingSheet`]），这一屏只留一个句柄：开合状态要报给会话
    /// （身上压着东西时正文的手势让位），元素要挂在最后（谁在上面由入树顺序定）。
    pub(in crate::ui::features::reader) settings: Entity<ReadingSheet>,
    /// 目录那一屏。整棵树、折叠集、搜索、弹层档位、书头都装在它自己那个实体里
    /// （见 [`TocPanel`]），这一屏只留一个句柄：开合状态要报给会话（身上压着东西时
    /// 正文的手势让位），元素要挂在最后（谁在上面由入树顺序定）。
    pub(in crate::ui::features::reader) toc: Entity<TocPanel>,
    /// 图片放大那一层的状态。它画在**最上面**（要盖住顶栏底栏与那块弹层），而写它的
    /// 是会话里那一次点击 / 那一次长按 —— 它不是视图，改动靠下面那次 `observe` 才
    /// 收得到重画，形状与两块弹层一致。
    pub(in crate::ui::features::reader) image_viewer: Entity<ImageViewer>,
    /// 查看器那一层的焦点句柄。它拿在阅读页手里而不是查看器自己里：那一层是 `overlay`
    /// 每帧造的元素树里的 `track_focus`，句柄得属于**这一棵**元素树的那个视图，
    /// 方向键那几条事件才能沿祖先链派发到它（见 `image_viewer::overlay`）。
    pub(in crate::ui::features::reader) image_viewer_focus: FocusHandle,
    /// 听书那一屏开着没有。它盖整屏（opaque），返回键第一下先收它。
    listen_open: bool,
    /// 听书屏现在是哪一面：封面 / 文字。
    listen_face: ListenFace,
    /// 文字那一档的滚动句柄（跨帧持有，理由同目录列表）。文稿是**一块**连续文本放在
    /// 普通滚动容器里，不再是等格虚拟列表 —— 这里要的就是普通 `ScrollHandle` 的那几样：
    /// 读实测位移、读 max_offset、每帧写位移。
    listen_scroll: ScrollHandle,
    /// 上一帧我们替文稿写进列表的滚动位移（逻辑像素，向下为正）。它与列表此刻实测到的
    /// 值对不上，就是读者手动划过 —— 那一段位移归读者，自动跟读让位
    /// [`LISTEN_HAND_OVER`]。
    listen_written: Option<f32>,
    /// 最近一次读者手动划过文稿的时刻。`None` = 没划过，或者已经让位够了。
    listen_manual_at: Option<Instant>,
    /// 当前章的文稿（换章或换宽重建）。
    listen_script: Option<std::rc::Rc<ListenScript>>,
    /// 书头三样。`None` = 还没查；`Some(None)` = 这本书没有可查的那一行。
    listen_book: Option<Option<ListenBook>>,
    pub(in crate::ui::features::reader) _watches: Vec<Subscription>,
}

impl ReadingPage {
    pub fn new(
        reader: Entity<ReaderSession>,
        stack: WeakEntity<NavStackState>,
        shell: WeakEntity<AppShell>,
        cx: &mut Context<Self>,
    ) -> Self {
        let progress = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(1.0)
                .step(0.001)
                .default_value(0.0)
        });
        // 进度这一条订阅一次：拖动当场把正文滚到那一处。设置卡片里那七条滑条各自的
        // 订阅归它自己那一屏（见 [`ReadingSheet::new`]），拖动即写配置、即重排 ——
        // 两端都没有「应用」按钮，也没有改了不生效的中间态。
        let mut watches = vec![cx.subscribe(&progress, |this: &mut Self, _, event, cx| {
            if let SliderEvent::Change(value) = event {
                this.reader
                    .update(cx, |reader, cx| reader.seek_ratio(value.end(), cx));
            }
        })];
        // 阅读设置那一屏自己的实体：弹层的开合与换档都要重画这一屏（`layers_up` 那个数
        // 与底栏那一格的选中态读的是它），所以观察一道，与目录同一形状。
        let settings = cx.new(|cx| ReadingSheet::new(reader.clone(), cx));
        watches.push(cx.observe(&settings, |_, _, cx| cx.notify()));
        // 目录那一屏自己的实体：它要观察它（弹层的开合与换档都得重画这一屏，
        // `layers_up` 那个数与顶栏底栏的挂载都读它），也要听它报回来的两件事。
        let toc = cx.new(|cx| TocPanel::new(reader.clone(), cx));
        watches.push(cx.observe(&toc, |_, _, cx| cx.notify()));
        watches.push(cx.subscribe(&toc, |this, _, event, cx| match event {
            TocEvent::Navigated => {
                // 跳到哪一章就把浮层整个收干净：读者要看的正是跳到的那几行字。
                this.chrome = Chrome::default();
                cx.notify();
            }
            TocEvent::OpenSettings => this.open_settings(cx),
        }));
        // 会话不是这一屏的子视图以外的东西，但脚注弹层长在**这一屏**的元素树上：
        // 会话里那一次「点中脚注」只把状态写进它自己并 `notify`，而重画这一屏要有人
        // 转一道。下面这次订阅就是那道转发 —— 会话无论为什么重画，这一屏跟着重画。
        watches.push(cx.observe(&reader, |_, _, cx| cx.notify()));
        // 图片放大那一层：状态挂在这一屏（它盖的是整块窗口），句柄交给会话，
        // 会话那一侧的点击与长按只管往里写「要看这张图」。
        let image_viewer = cx.new(|_| ImageViewer::default());
        watches.push(cx.observe(&image_viewer, |_, _, cx| cx.notify()));
        // 查看器那一层的焦点句柄。它拿在阅读页手里而不是查看器自己里：这一层是
        // `overlay` 每帧造的元素树里的 `track_focus`，句柄要属于**这一棵**元素树的
        // 那个视图（`reading-page`），方向键那几条事件才能沿祖先链派发到它。
        let image_viewer_focus = cx.focus_handle();
        reader.update(cx, |reader, _| {
            reader.set_image_viewer(image_viewer.clone())
        });
        Self {
            reader,
            reserve: Reserve::ZERO,
            enter_shift: 0.0,
            top_inset: 0.0,
            bottom_inset: 0.0,
            stack,
            shell,
            chrome: Chrome::default(),
            progress,
            settings,
            toc,
            image_viewer,
            image_viewer_focus,
            listen_open: false,
            listen_face: ListenFace::default(),
            listen_scroll: ScrollHandle::new(),
            listen_written: None,
            listen_manual_at: None,
            listen_script: None,
            listen_book: None,
            _watches: watches,
        }
    }

    pub fn open(&mut self, book_id: i64, cx: &mut Context<Self>) {
        self.reader
            .update(cx, |reader, cx| reader.open_book(book_id, cx));
    }

    /// 落库。外壳在 pop 之前调它。
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.reader.update(cx, |reader, cx| {
            // 先把挂着的那一趟翻页放下。退场那几百毫秒里这一层的时钟不再续帧，`turn` 会冻在
            // 松手前那一刻 —— 于是仿真翻页那张网格把一片纸背留在书架上（2026-10-19 真机撞见）。
            reader.turn = None;
            reader.persist_position();
            // 差量那一笔与位置同一次离席要结：它平时等安静 24 帧（去抖，见
            // `sync_book_settings`），而「拖完最后一格就按返回」不会等到那一档。
            reader.flush_book_settings(cx);
            // 位置与时长是同一次离席要结的两笔账：漏掉后一笔，这一段就只能靠
            // 那条时钟在五分钟之内结掉，而读者往往在五分钟之内就再没回来过。
            reader.end_reading();
        });
    }

    /// 只结位置那一笔：外壳在这一层还在、但窗口刚失去焦点时调（Android 切后台之后进程
    /// 随时可能被回收，那是最后一个可靠的机会）。时长那一笔不结 —— 它有自己的 60 秒
    /// 时钟，把一段还没走完的段落在这里结掉只会把它切碎。
    pub fn flush_position(&mut self, cx: &mut Context<Self>) {
        self.reader.update(cx, |reader, cx| {
            reader.persist_position();
            // 同上：失焦是 Android 切后台之前最后一个可靠的机会，两笔账一起结。
            reader.flush_book_settings(cx);
        });
    }

    /// 应用生命周期（只有移动平台会发）：后台按离开这一刻结账并停钟，前台续段。
    pub fn on_app_lifecycle(&mut self, phase: AppLifecyclePhase, cx: &mut Context<Self>) {
        self.reader.update(cx, |reader, cx| match phase {
            AppLifecyclePhase::Background => reader.suspend_reading(),
            AppLifecyclePhase::Active => reader.resume_reading(cx),
            _ => {}
        });
    }

    /// 系统内存告警：丢掉可重建的图像缓存，当前屏由图集保护。
    pub fn on_memory_warning(&mut self, cx: &mut Context<Self>) {
        self.reader.update(cx, |reader, cx| reader.trim_caches(cx));
    }

    pub fn book_id(&self, cx: &App) -> Option<i64> {
        self.reader.read(cx).book_id()
    }

    pub fn set_reserve(&mut self, reserve: Reserve, cx: &mut Context<Self>) {
        self.reserve = reserve;
        self.reader
            .update(cx, |reader, cx| reader.set_reserve(reserve, cx));
    }

    /// 顶上那条系统边（状态栏 / 挖孔）有多高。外壳每帧报（没装安全区时是 0），与它推整层
    /// 用的是同一个数。
    ///
    /// 变化时重画：顶栏表面向上长的那一段读它。
    pub fn set_top_inset(&mut self, top: f32, cx: &mut Context<Self>) {
        // 会话里那一格跟着一起换，而且**从这里**换：这一屏每帧都会把本页记着的数再推一遍
        // 给会话（`render` 里 `set_chrome_bands` 那一串），两边不同步时那一次推送就把外壳
        // 刚写进去的值覆盖回去。判据只放在「变了才通知」那一层，会话那边无条件写。
        self.reader.update(cx, |reader, _| reader.insets.top = top);
        if (self.top_inset - top).abs() > 0.5 {
            self.top_inset = top;
            cx.notify();
        }
    }

    /// 系统手势条那一条有多高。外壳每帧报（没装安全区时是 0）。
    ///
    /// 变化时重画：底栏卡片的内缩与干净态页脚的高度都读它。
    pub fn set_bottom_inset(&mut self, bottom: f32, cx: &mut Context<Self>) {
        self.reader
            .update(cx, |reader, _| reader.insets.bottom = bottom);
        if (self.bottom_inset - bottom).abs() > 0.5 {
            self.bottom_inset = bottom;
            cx.notify();
        }
    }

    /// 外壳进场横移这一帧走了多少（逻辑像素）。正文画捕获层那两份副本时按 `-enter_shift`
    /// 抵消它，于是进书那 280 ms 里也能按**最终位置**烘页，不必等层停稳。
    ///
    /// 这一格**不** `cx.notify()`：它是外壳在自己那一帧的渲染里报进来的，而这一帧这一页本来
    /// 就要画；补一发通知只会让同一帧再排一次重绘，转场期间逐帧空转。
    pub(in crate::ui) fn set_enter_shift(&mut self, shift: f32) {
        self.enter_shift = shift;
    }

    /// 浮层挡着的时候，系统返回先收它；收干净了才轮到 pop 这一页。
    ///
    /// 返回 `true` 表示这次返回已被浮层消费，调用方不该再往下走。
    ///
    /// 收的顺序与摆的顺序相反：图片放大那一层在最上面，先收它；再收笔记便签、脚注、
    /// 阅读设置那块卡片、目录那块弹层，最后才是顶栏底栏。
    /// 焦点的归还不在这里做 —— 这条路上没有 `&mut Window`（`App::on_action` 的派发里窗口是
    /// 借着的），弹层自己在下一帧的 `render` 里还。
    pub fn dismiss_chrome(&mut self, cx: &mut Context<Self>) -> bool {
        // 听书那一屏盖着整窗：返回键第一下先收它。
        if self.listen_open {
            self.close_listen(cx);
            return true;
        }
        if self.image_viewer.read(cx).is_open() {
            self.image_viewer.update(cx, |viewer, _| viewer.close());
            cx.notify();
            return true;
        }
        // 笔记便签排在最上面那一档：它自带遮罩，先收它；翻译弹层同为「就地升起的
        // 一张卡」，紧随其后；再轮到脚注与目录。
        if self.reader.read(cx).note_editor_open() {
            self.reader
                .update(cx, |reader, cx| reader.cancel_note_editor(cx));
            cx.notify();
            return true;
        }
        if self.reader.read(cx).rules_draft_open() {
            self.reader
                .update(cx, |reader, cx| reader.close_rules_draft(cx));
            cx.notify();
            return true;
        }
        if self.reader.read(cx).ai_open() {
            self.reader.update(cx, |reader, cx| reader.close_ai(cx));
            cx.notify();
            return true;
        }
        if self.reader.read(cx).dictionary_open() {
            self.reader
                .update(cx, |reader, cx| reader.close_dictionary(cx));
            cx.notify();
            return true;
        }
        if self.reader.read(cx).translation_open() {
            self.reader
                .update(cx, |reader, cx| reader.close_translation(cx));
            cx.notify();
            return true;
        }
        // 脚注弹层在最上面那一档里，先收它；再收目录那块弹层；最后才是顶栏底栏。
        // 「⋯」那条菜单是 gpui-kit 自己的 `PopupMenu`，返回键那一下由它自己的
        // `Cancel` 绑键吃掉，不走这里。
        if self.reader.read(cx).footnote().is_some() {
            self.reader
                .update(cx, |reader, cx| reader.dismiss_footnote(cx));
            cx.notify();
            return true;
        }
        if self.settings.read(cx).is_open(cx) {
            self.settings.update(cx, |sheet, cx| sheet.hide(cx));
            cx.notify();
            return true;
        }
        if self.toc.read(cx).is_open(cx) {
            self.toc.update(cx, |panel, cx| panel.hide(cx));
            cx.notify();
            return true;
        }
        if !self.chrome.dismiss() {
            return false;
        }
        cx.notify();
        true
    }

    /// 音量键要翻的那一页。`delta` 说的是**读书顺序里的那一侧**，不是屏幕的左右：
    /// 页号本来就按阅读序排（左右分区那条换算在 `chrome::zone` 里），所以 +1 在 RTL
    /// 的书里也还是「往后一页」。
    ///
    /// 走的正是点屏幕左右两边那条路（`on_body_tap` 里 `Effect::NextPage` 那一支），
    /// 所以连续滚动与分页两种模式的行为、以及章末自动跨章，全都跟着正文一致。
    /// 目录 / 设置 / 「⋯」那一层浮着的时候也照样翻正文 —— 那几屏自己有滚动条，
    /// 让音量键去翻它们要另写一套消费逻辑，这一版不做。
    pub fn turn_page(&mut self, delta: isize, cx: &mut Context<Self>) {
        // 查看器开着时翻页：那一层还停在旧的一页上，先把界面收干净再翻。
        if self.image_viewer.read(cx).is_open() {
            self.image_viewer.update(cx, |viewer, _| viewer.close());
            cx.notify();
        }
        self.reader
            .update(cx, |reader, cx| reader.scroll_page(delta, cx));
    }

    /// 方向键翻页（PC）。返回 `false` = 这一键不归阅读页，调用方该 `cx.propagate()`。
    ///
    /// 子栈上压着别的屏（紧凑设置）时让开：那一屏的滑条与列表用同一套方向键，抢过来
    /// 就是滑条不动、背后正文在翻页。其余判据（图片查看器开着、文字输入框拿着焦点）
    /// 都在会话与 gpui 那一侧 —— 见 [`ReaderSession::arrow_turn`]。
    pub fn arrow_turn(&mut self, arrow: chrome::Arrow, cx: &mut Context<Self>) -> bool {
        if self
            .stack
            .upgrade()
            .is_some_and(|stack| stack.read(cx).depth() > 1)
        {
            return false;
        }
        self.reader
            .update(cx, |reader, cx| reader.arrow_turn(arrow, cx))
    }

    /// 当前页号。**只由宿主测试问**：方向键那一条要从键绑定一路扎到页号上，而翻页
    /// 落没落位在屏幕上只是「字换了一屏」，没有别的可断言的数。
    #[cfg(test)]
    pub fn page_number(&self, cx: &App) -> usize {
        self.reader.read(cx).page()
    }

    /// 这一章切成了多少页（双页对开时是「多少张纸」）。同样只给宿主测试。
    #[cfg(test)]
    pub fn sheet_count(&self, cx: &App) -> usize {
        self.reader.read(cx).page_count()
    }

    /// 顶栏那颗返回。正文是这一层那条子栈的栈底，弹不动 —— 该请外壳把整层收走。
    ///
    /// 从正文压进来的那一屏（设置）自己带返回，它压着的时候这一屏根本看不见，所以这里
    /// 不必先问子栈。
    ///
    /// 收尾分两步走是因为**这一句跑在本页自己的更新里**（顶栏那一下点击）：
    /// `page.update(|this| this.pop())` 已经在借着自己了，再请外壳 `update` 一次本页
    /// （`close_reading` 干的正是那件事）就是二次可变借用，直接 panic —— release 下
    /// `panic = abort`，读者看到的「点返回直奔桌面」就是它。所以本页该做的事自己做，
    /// 外壳那边只翻退出那一格（[`AppShell::dismiss_reading`]）。
    pub(crate) fn pop(&mut self, cx: &mut Context<Self>) {
        self.close(cx);
        if let Some(shell) = self.shell.upgrade() {
            shell.update(cx, |shell, cx| shell.dismiss_reading(cx));
        }
    }

    /// 正文上的一击：先让选中说话（拖选松手的豁免、选中的清除/菜单回开），
    /// 再按分区照结果动。见 [`chrome::Chrome::handle_body_tap`] 与
    /// `selection.rs` 的 `tap_with_selection`。
    pub(in crate::ui::features::reader) fn on_body_tap(
        &mut self,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let position = event.position();
        // 三条判据的归因与顺序都在 `ReaderSession::tap_on_body` 里（那里才测得到）。
        let consumed = self
            .reader
            .update(cx, |reader, cx| reader.tap_on_body(window, cx, position));
        if consumed {
            cx.notify();
            return;
        }
        // 脚注优先于链接也优先于分区：脚注标记本身就是一条 `<a href="#...">`，但读者
        // 要的是「就地看注文」，不是「跳到注文那一行」。排在选中之后（拖选松手那一下的
        // 豁免仍在 `tap_on_body` 里判过）。
        if self.reader.update(cx, |reader, cx| {
            reader.open_footnote_at(window, cx, position)
        }) {
            return;
        }
        // 链接优先于分区：点中 `<a href>` 就跳过去，不翻页、不开浮层。排在选中之后、
        // 分区之前 —— 拖选松手那一下（`tap_on_body` 已豁免）不该被当成点链接。
        if self
            .reader
            .update(cx, |reader, cx| reader.open_link_at(window, cx, position))
        {
            return;
        }
        // 三区分的是**正文那一栏**，不是整块窗口。宽档的目录改成浮层之后正文不再让宽，
        // 所以这里不需要再减一条栏：整块窗口就是正文的可用宽。
        let width = f32::from(window.viewport_size().width).max(1.0);
        // 左右两档意味着什么，跟着整本的翻页方向走（`zone` 里那一条）。
        let rtl = self.reader.read(cx).progression_rtl();
        let tap = zone(f32::from(position.x), width, rtl);
        match self.chrome.handle_body_tap(tap) {
            Effect::OpenChrome | Effect::DismissChrome => cx.notify(),
            Effect::PrevPage => {
                self.reader
                    .update(cx, |reader, cx| reader.scroll_page(-1, cx));
            }
            Effect::NextPage => {
                self.reader
                    .update(cx, |reader, cx| reader.scroll_page(1, cx));
            }
        }
    }

    /// 把「别处也改得动」的数推回底栏那条进度滑块：正文滚了要挪它。
    ///
    /// 设置卡片里那七条滑条不归这里 —— 它们对齐的是配置而不是正文，各自住在
    /// [`ReadingSheet::sync`] 里。
    ///
    /// 带阈值比较：拖动的时候滑条自己就是权威，每帧再按正文读回来的数写回去，
    /// 就是拿手指算出来的值和滚动算出来的值互相追。
    pub(in crate::ui::features::reader) fn sync_sliders(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 算不出全书量时滑条停在 0：它是**控件**不是读数，那一格的读数由 `percent_label`
        // 空着表达。拖它没有后果 —— `seek_ratio` 在同一个 `None` 上直接返回。
        let progress = self.reader.read(cx).progress().unwrap_or(0.0);
        if (self.progress.read(cx).value().end() - progress).abs() > 0.005 {
            self.progress
                .update(cx, |state, cx| state.set_value(progress, window, cx));
        }
    }

    /// 阅读设置那张卡片：开 or 关。九格的内容、弹层停在哪一档与那七条滑条都装在它自己
    /// 那个实体里（见 [`ReadingSheet`]）；这一屏只多管一件事 —— 目录那块弹层同时收掉：
    /// 两块都是盖住整屏的模态层，叠着抬起来就是同一片遮罩底下两层东西。
    pub(in crate::ui::features::reader) fn toggle_settings(&mut self, cx: &mut Context<Self>) {
        if !self.settings.read(cx).is_open(cx) && self.toc.read(cx).is_open(cx) {
            self.toc.update(cx, |panel, cx| panel.hide(cx));
        }
        self.settings.update(cx, |sheet, cx| sheet.toggle(cx));
        cx.notify();
    }

    /// 目录弹层：开 or 关。树跟着阅读位置重建、搜索框与「⋯」一并收掉、书头跟着重查，
    /// 那些都在 [`TocPanel::toggle`] 里；这一屏只多管一件事 —— 设置那张卡片同时收掉，
    /// 理由与反过来那一条相同（两块模态层不许同时在场）。
    pub(in crate::ui::features::reader) fn toggle_toc(&mut self, cx: &mut Context<Self>) {
        if !self.toc.read(cx).is_open(cx) && self.settings.read(cx).is_open(cx) {
            self.settings.update(cx, |sheet, cx| sheet.hide(cx));
        }
        self.toc.update(cx, |panel, cx| panel.toggle(cx));
        cx.notify();
    }

    /// 听书那一下：没念 → 从视口当前句开口；念着 → 暂停；暂停 → 这一句从头再念。
    /// 状态机全在会话里（`model/speech.rs`），这一屏只把那一击递过去。
    ///
    /// 会员门禁在视图层拦一道：圆钮缺席之后，能摸到这一下的只剩快捷键。
    pub(in crate::ui::features::reader) fn toggle_speech(&mut self, cx: &mut Context<Self>) {
        if !crate::ui::services::activation::premium(cx) {
            return;
        }
        // 听书起念就把自动前进停掉：两个「推位置」的东西同时在跑，读者分不清是谁在翻。
        self.reader.update(cx, |reader, cx| {
            reader.stop_auto(cx);
            reader.toggle_speech(cx);
        });
    }

    /// 底栏那一格「自动」：自动前进 on/off。状态与拍子都在会话里（`model/paging` 的
    /// `set_auto` / `auto_step`），这一屏只把那一击递过去。
    pub(in crate::ui::features::reader) fn toggle_auto(&mut self, cx: &mut Context<Self>) {
        self.reader.update(cx, |reader, cx| {
            reader.set_auto(!reader.auto_on(), cx);
        });
        cx.notify();
    }

    /// 打开听书那一屏：**只把门推开，不开口**。待念的位置当场摆好（视口当前句，
    /// `arm_speech`）—— 文稿那一面有东西可定位，但声音要等读者按下圆钮才出来。
    ///
    /// 这一屏是**专用的**听书面孔（封面 / 文字两面），圆钮只负责把门推开 ——
    /// 从前那一颗直接 toggle，读者想换一面看时没有地方按。
    pub(in crate::ui::features::reader) fn open_listen(&mut self, cx: &mut Context<Self>) {
        if !self.reader.read(cx).speech_entry_visible()
            || !crate::ui::services::activation::premium(cx)
        {
            return;
        }
        self.reader.update(cx, |reader, cx| reader.arm_speech(cx));
        // 顶栏底栏收干净：这一层盖整屏，留着它们只会在底下闪。
        self.chrome = Chrome::default();
        self.listen_open = true;
        self.listen_book = None;
        // 文稿那一面的两格账本重开：列表句柄里的位移是上一次留下的，而弹簧的状态随
        // 元素树一起没了。留着旧账，第一帧会把「我们自己上一帧写的」当成参照，
        // 把没动过的列表误判成读者手动划过，让位整整四秒。
        self.listen_written = None;
        self.listen_manual_at = None;
        cx.notify();
    }

    fn close_listen(&mut self, cx: &mut Context<Self>) {
        if self.listen_open {
            self.listen_open = false;
            cx.notify();
        }
    }

    /// 上一句 / 下一句：按**这一章**的文稿走一步。到头就停在头尾。
    ///
    /// 只把「念哪一句」交出去，不动列表：文稿那一面的居中与滚动在 [`Self::listen_overlay`]
    /// 里按正在念的那一句算，这里再滚一次就是把同一个决定做两遍。让位那格要清掉 ——
    /// 读者按下一句就是要让屏跟着走，不是还在自己翻。
    fn listen_step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some((chapter, range)) = self.reader.read(cx).speech_range() else {
            return;
        };
        let Some(script) = self.listen_script.clone() else {
            return;
        };
        if script.chapter != chapter {
            return;
        }
        let Some(ix) = sentence_index(&script.sentences, range.start) else {
            return;
        };
        let last = script.sentences.len().saturating_sub(1) as isize;
        let target = (ix as isize + delta).clamp(0, last) as usize;
        if target == ix {
            return;
        }
        let want = script.sentences[target].chapter.clone();
        self.listen_manual_at = None;
        self.reader
            .update(cx, |reader, cx| reader.speak_from(chapter, want, cx));
    }

    /// 书头三样查一次：书名 / 作者 / 封面。查询走与目录书头同一条（`store.book` +
    /// `store.book_cover`），结果缓存在这一屏 —— 听书屏开着的时候不每帧摸 SQLite。
    fn ensure_listen_book(&mut self, cx: &App) {
        if self.listen_book.is_some() {
            return;
        }
        let reader = self.reader.read(cx);
        let fallback = reader
            .book()
            .map(|book| book.title().clone())
            .unwrap_or_default();
        let Some(id) = reader.book_id() else {
            self.listen_book = Some(None);
            return;
        };
        let Some(store) = reader.store.clone() else {
            self.listen_book = Some(Some(ListenBook {
                title: fallback,
                authors: SharedString::default(),
                cover: None,
            }));
            return;
        };
        let row = store.book(id).ok().flatten();
        let cover = store
            .book_cover(id)
            .ok()
            .flatten()
            .and_then(|bytes| cover_image(&bytes));
        let title = row
            .as_ref()
            .map(|row| SharedString::from(row.title.clone()))
            .unwrap_or(fallback);
        let authors = row
            .as_ref()
            .map(|row| SharedString::from(row.authors.join(AUTHOR_SEP)))
            .unwrap_or_default();
        self.listen_book = Some(Some(ListenBook {
            title,
            authors,
            cover,
        }));
    }

    /// 当前章的文稿行表。**换章或换宽才重建**：一章几千字，逐帧重排是白付。
    ///
    /// 与 `model::speech` 同一个尺：`split_sentences` 给出的字符区间就是引擎那一条
    /// utterance 的范围，点哪一格就从它那一句念。每一句按**真实排版**切成若干行
    /// （见 [`build_listen_script`]）：整章拼成一段连续文本，书页一样自然折行。
    /// 换宽（转屏 / 平板）要重建，不然折点会落到屏幕外面。
    ///
    /// 返回「这一帧是不是重建了」。重建那一帧弹簧要当场落定（换章可能差出几屏），
    /// 让它当场落定 —— 换章演成一段长途滚动是假的：读者的确跳到了别处，但那不是滑行。
    fn ensure_listen_script(
        &mut self,
        chapter: usize,
        fallback: std::ops::Range<usize>,
        width: f32,
        family: &SharedString,
        window: &Window,
        cx: &App,
    ) -> bool {
        if self.listen_script.as_ref().is_some_and(|script| {
            script.chapter == chapter
                && script.width == width
                && script.complete
                && !script.sentences.is_empty()
        }) {
            return false;
        }
        let built = self
            .reader
            .read(cx)
            .book()
            .and_then(|book| book.span_doc(chapter))
            .map(|doc| {
                let text = doc.text_map().text();
                let chars: Vec<char> = text.chars().collect();
                let (script_text, mut sentences) =
                    build_listen_script(&chars, |at| doc.paragraph_range(at));
                let (script_text, sentences, complete) = if sentences.is_empty() {
                    // 排版还没给段界（或这一章还没排完）时，至少把「正在念的这一句」
                    // 摆出来：文稿那一面不该因为整张表建不起来就写着「正在准备」。
                    // 这张单句表**不完整**，缓存判据会拒绝它 —— 每帧重建，直到段界
                    // 落地整章的表建出来为止。
                    let only: String = doc.text_map().slice(fallback.clone());
                    let only = only.trim();
                    if only.is_empty() {
                        (script_text, sentences, true)
                    } else {
                        // 每帧可能连发（表不完整就不进缓存），压在 debug 档。
                        tracing::debug!("文稿：段界未就绪，先摆当前一句（表不完整，不进缓存）");
                        let mut text = String::from(only);
                        // 兜底表按「句间原样拼接」的口径走；trim 过的单句不会带空白，
                        // 字节区间因此正好是整段 0..len。
                        let script = 0..text.len();
                        sentences.push(ListenSentence {
                            chapter: fallback,
                            script,
                        });
                        (std::mem::take(&mut text), sentences, false)
                    }
                } else {
                    (script_text, sentences, true)
                };
                // 整章排一次：每一句落在第几行（跟读的落点）。折行点只由字体、字号、
                // 行宽决定 —— 高亮只换颜色不换字重，换句不重排。
                let shaped = listen_shape_script(&script_text, width, family, window);
                let line_lens: Vec<usize> = shaped.iter().map(|line| line.text.len()).collect();
                let line_of = sentences
                    .iter()
                    .map(|sentence| line_index_at(&line_lens, sentence.script.start))
                    .collect();
                std::rc::Rc::new(ListenScript {
                    chapter,
                    text: script_text.into(),
                    sentences,
                    line_of,
                    width,
                    complete,
                })
            });
        self.listen_script = built;
        true
    }

    /// 听书那一屏。两面共用同一条顶栏与同一条控制排，中间那块换。
    ///
    /// 文字那一面是一张书页：整章连续排版、自然折行，字号统一；正在念的那一句换上实色，
    /// 其余沉在次要色里，读到哪儿由一根弹簧把那一行滑到屏的正中。读者手动划过文稿的那
    /// 几秒不抢。点哪一句就从哪一句念。从前这一面是 Apple Music 歌词流（逐行缩放调透明），
    /// 读者读到的是「满屏只有一行」—— 明暗的邻居看不清，长句还常被折行预算吃掉；
    /// 2026-11-07 换成书页（读者要的「文稿要看得到上下文」）。
    fn listen_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.listen_open {
            return None;
        }
        let Some((chapter, range)) = self.reader.read(cx).speech_range() else {
            // 念的位置没了（一章都没有 / 会话停了）：这一层自己收起来。
            self.listen_open = false;
            return None;
        };
        let playing = self.reader.read(cx).speech_playing();
        // 封面那一面根本没有那条列表，「上一帧我们写了多少位移」的账就此断了：切回文稿
        // 时不能拿旧账当参照，否则列表里没动过的位移会被读成「读者手动划过」，让位整整
        // 四秒 —— 那正是「切过去不跟读」的一种。
        if self.listen_face != ListenFace::Text {
            self.listen_written = None;
        }
        // 文稿那一列多宽：页边与滚动条都从这一算里扣，折行与摆出来共用同一个数（见
        // [`listen_column_width`]）。
        let width = listen_column_width(f32::from(window.viewport_size().width));
        // 量的尺与画的尺必须连字体都同一个：主题那一份 UI 字体。
        let family = cx.theme().font_family.clone();
        let rebuilt = self.ensure_listen_script(chapter, range.clone(), width, &family, window, cx);
        self.ensure_listen_book(cx);
        let script = self.listen_script.clone();
        // 正在念的那一句是文稿里的第几句。`None` = 这一章的文稿还没建出来。
        let active = script
            .as_ref()
            .and_then(|script| sentence_index(&script.sentences, range.start));

        let (
            background,
            foreground,
            muted,
            primary,
            primary_foreground,
            primary_hover,
            primary_active,
        ) = {
            let theme = cx.theme();
            (
                theme.background,
                theme.foreground,
                theme.muted_foreground,
                theme.primary,
                theme.primary_foreground,
                theme.primary_hover,
                theme.primary_active,
            )
        };
        let close_label = tr(cx, "listen.close");
        let page_close = cx.entity().clone();
        let page_face = cx.entity().clone();
        // 顶栏中间只有**一颗**分段胶囊：左「封面」、右「文稿」，选中那半用主题主色
        // 填实。左右各一段等宽的 `flex_1`，胶囊因此正落在屏中；标题那行「听书」
        // 字样去掉 —— 这一屏是干什么的，两格按钮已经说清楚了。
        let cover_label = tr(cx, "listen.cover");
        let manuscript_label = tr(cx, "listen.text");
        let segment = |id: &'static str,
                       label: SharedString,
                       target: ListenFace,
                       on: bool,
                       page: gpui_kit::Entity<ReadingPage>| {
            div()
                .id(id)
                .test_support()
                .px(px(14.))
                .py(px(6.))
                .rounded_full()
                .text_sm()
                .when(on, |this| this.bg(primary).text_color(primary_foreground))
                .when(!on, |this| this.text_color(muted))
                .child(label)
                .on_click(move |_, _, cx| {
                    page.update(cx, |this, cx| {
                        this.listen_face = target;
                        cx.notify();
                    })
                })
                .into_any_element()
        };
        let face_switch = h_flex()
            .id("listen-face-switch")
            .flex_none()
            .items_center()
            .gap_1()
            .px(px(2.))
            .py(px(2.))
            .rounded_full()
            .bg(cx.theme().muted)
            .child(segment(
                "listen-face-cover",
                cover_label,
                ListenFace::Cover,
                self.listen_face == ListenFace::Cover,
                page_face.clone(),
            ))
            .child(segment(
                "listen-face-text",
                manuscript_label,
                ListenFace::Text,
                self.listen_face == ListenFace::Text,
                page_face,
            ));
        let top = h_flex()
            .w_full()
            .items_center()
            .gap_1()
            .child(
                div().flex_1().child(
                    Button::new("listen-close")
                        .ghost()
                        .small()
                        .icon(IconName::ChevronDown)
                        .tooltip(close_label.clone().to_string())
                        .accessibility_label(close_label)
                        .on_click(move |_, _, cx| {
                            page_close.update(cx, |this, cx| this.close_listen(cx))
                        }),
                ),
            )
            .child(face_switch)
            .child(div().flex_1());

        let book = self.listen_book.as_ref().and_then(|book| book.as_ref());
        let title: SharedString = book.map(|book| book.title.clone()).unwrap_or_default();
        let authors: SharedString = book.map(|book| book.authors.clone()).unwrap_or_default();
        let chapter_title: SharedString = self
            .reader
            .read_with(cx, |reader, _| reader.chapter_title_at(chapter))
            .into();
        let position: SharedString = self
            .listen_script
            .as_ref()
            .zip(active)
            .map(|(script, ix)| {
                SharedString::from(tf(
                    cx,
                    "listen.position",
                    &[("index", &(ix + 1)), ("total", &script.sentences.len())],
                ))
            })
            .unwrap_or_else(|| SharedString::from(&*chapter_title));

        let body = match self.listen_face {
            ListenFace::Cover => {
                let placeholder = cx.theme().muted;
                let cover = book.and_then(|book| book.cover.clone());
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(
                        div()
                            .w(px(LISTEN_COVER_W))
                            .h(px(LISTEN_COVER_H))
                            .overflow_hidden()
                            .rounded(px(10.))
                            .bg(placeholder)
                            .when_some(cover, |this, image| {
                                this.child(img(image).size_full().object_fit(ObjectFit::Cover))
                            }),
                    )
                    .child(
                        div()
                            .max_w_full()
                            .truncate()
                            .text_size(px(20.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .when(!authors.is_empty(), |this| {
                        this.child(
                            div()
                                .max_w_full()
                                .truncate()
                                .text_sm()
                                .text_color(muted)
                                .child(authors),
                        )
                    })
                    .child(div().text_sm().text_color(muted).child(chapter_title))
                    .child(div().text_xs().text_color(muted).child(position))
                    .into_any_element()
            }
            ListenFace::Text => {
                if let Some(script) = script.clone() {
                    // ── 跟读的一根弹簧 ────────────────────────────────────────────────
                    // 目标位移 = 正在念的那一句落在第几行 × 行高，让那一行居中。视口多高
                    // 要容器自己量过才算数：第一帧那个数还是 0，按 0 算会把内容顶出屏外
                    // —— 所以这一帧先不驱动，再要一帧。
                    let viewport = f32::from(self.listen_scroll.bounds().size.height);
                    if viewport <= 0.0 {
                        window.request_animation_frame();
                    }
                    // 实测到的位移与上一帧我们自己写下去的对不上，就是读者手动划过（触屏
                    // 的惯性也算：每动一次续一次让位）。第一帧没有参照，不判。
                    //
                    // 门取 0.75 像素：上面记下的是容器自己钳过的那一个数，剩下的出入只有
                    // 浮点表示那一级，比一次真实划动小三个数量级 —— 既不会把「没人动」读成
                    // 划过，也不会漏掉读者慢慢拖着看的那一笔。
                    let measured = -f32::from(self.listen_scroll.offset().y);
                    if self
                        .listen_written
                        .is_some_and(|written| (measured - written).abs() > 0.75)
                    {
                        // 逐帧可能连发（这正是它要抓的症状），压在 debug 档。
                        tracing::debug!(
                            measured,
                            written = ?self.listen_written,
                            "文稿：位移对不上，按读者手动划过让位"
                        );
                        self.listen_manual_at = Some(Instant::now());
                    }
                    let handed_over = viewport <= 0.0
                        || active.is_none()
                        || self
                            .listen_manual_at
                            .is_some_and(|at| at.elapsed() < LISTEN_HAND_OVER);
                    let follow = Spring::new(LISTEN_FOLLOW).with_damping(0.85);
                    // 让位的那几秒目标就钉在实测到的那一个数上，弹簧停在原地；让位一过，
                    // 从读者停住的地方接着往念到的那一句滑。重建那一帧（换章 / 换宽）
                    // 当场落定 —— 跨章可能差出几屏，把它演成一段长途滚动是假的。
                    let top = spring(
                        "listen-scroll-top",
                        if handed_over {
                            measured
                        } else {
                            listen_focus_offset(&script, active.unwrap_or(0), viewport)
                        },
                        if handed_over || rebuilt {
                            follow.with_travel(false)
                        } else {
                            follow.with_epsilon(0.5)
                        },
                        window,
                        cx,
                    );
                    // 记下来的必须是**容器自己会留下的那一个数**：它在绘制那一趟把写进去
                    // 的位移按它自己量到的 `max_offset` 再钳一次 —— 与我们按行数 × 行高推
                    // 的有零点几像素的出入，章越长积得越多。拿没钳过的那个数当参照，
                    // 每一帧都会被读成「读者手动划过」，跟读自己停住 —— 症状与从前那条
                    // 「不跟着声音走」一模一样。
                    let written = top.min(f32::from(self.listen_scroll.max_offset().y).max(0.0));
                    self.listen_written = Some(written);
                    self.listen_scroll.set_offset(point(px(0.0), -px(written)));

                    // 当前句高亮：只换颜色（muted → foreground）。**不换字重** —— 折行点
                    // 只由字体、字号、行宽决定，换句不重排整章，弹簧的落点才不会漂。
                    // 真机上若觉得不够醒目，下一步是叠 `HighlightStyle::background_color`
                    // （与正文屏 `speech_fill` 同语义）：背景色同样不进折行。
                    let text_style = TextStyle {
                        color: muted,
                        font_family: family.clone(),
                        font_size: AbsoluteLength::Pixels(px(LISTEN_BOOK_PX)),
                        line_height: DefiniteLength::Absolute(AbsoluteLength::Pixels(px(
                            LISTEN_LINE_H,
                        ))),
                        ..Default::default()
                    };
                    let highlights: Vec<(std::ops::Range<usize>, HighlightStyle)> = active
                        .and_then(|ix| script.sentences.get(ix))
                        .map(|sentence| {
                            (
                                sentence.script.clone(),
                                HighlightStyle {
                                    color: Some(foreground),
                                    ..Default::default()
                                },
                            )
                        })
                        .into_iter()
                        .collect();
                    let sentences = script.sentences.clone();
                    let ranges = sentences
                        .iter()
                        .map(|sentence| sentence.script.clone())
                        .collect::<Vec<_>>();
                    let jump = cx.entity().clone();
                    let text_element = InteractiveText::new(
                        "listen-script",
                        StyledText::new(script.text.clone())
                            .with_default_highlights(&text_style, highlights),
                    )
                    .on_click(ranges, move |ix, _, cx| {
                        // 点哪一句就从那一句念。让位清掉：这一击要的就是「屏跟着走」。
                        let Some(entry) = sentences.get(ix) else {
                            return;
                        };
                        let want = entry.chapter.clone();
                        jump.update(cx, |this, cx| {
                            this.listen_manual_at = None;
                            this.reader
                                .update(cx, |reader, cx| reader.speak_from(chapter, want, cx));
                        });
                    });
                    // 文稿必须包在 `flex_1 + min_h_0` 的壳里：整屏是
                    // 顶栏 / 内容 / 控制排三兄弟，直接给滚动容器 `h_full` 的父格没高度，
                    // 屏上就是一片空白 —— 真机上「切到文稿，文字不显示」的那一半原因
                    // 在这里。列宽封顶（[`listen_column_width`]），宽屏上整块居中。
                    div()
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .relative()
                        .child(
                            div()
                                .id("listen-script-scroll")
                                .h_full()
                                .overflow_y_scroll()
                                .track_scroll(&self.listen_scroll)
                                .vertical_scrollbar(&self.listen_scroll)
                                .child(div().w(px(width)).mx_auto().child(text_element)),
                        )
                        .into_any_element()
                } else {
                    let loading = tr(cx, "listen.loading");
                    v_flex()
                        .flex_1()
                        .min_h_0()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .text_color(muted)
                        .child(loading)
                        .into_any_element()
                }
            }
        };

        let page_play = cx.entity().clone();
        let page_prev = cx.entity().clone();
        let page_next = cx.entity().clone();
        let play_style = ButtonCustomVariant::new(cx)
            .color(primary)
            .foreground(primary_foreground)
            .hover(primary_hover)
            .active(primary_active)
            .shadow(true);
        let can_prev = active.is_some_and(|ix| ix > 0);
        let can_next = self
            .listen_script
            .as_ref()
            .zip(active)
            .is_some_and(|(script, ix)| ix + 1 < script.sentences.len());
        // 引擎现状：点圆钮那一刻才起的探测，几秒内见分晓。两格没就绪都要在屏上说清
        // ——「按下去什么都不发生的按钮，读者读到的是坏了」。
        let engine_ready = self.reader.read(cx).speech_available();
        let engine_note = match engine_ready {
            Some(true) => None,
            Some(false) => Some(tr(cx, "listen.engine_unavailable")),
            None => Some(tr(cx, "listen.engine_preparing")),
        };
        let prev_label = tr(cx, "listen.prev");
        let next_label = tr(cx, "listen.next");
        let controls = h_flex()
            .w_full()
            .items_center()
            .justify_center()
            .gap_6()
            .child(
                Button::new("listen-prev")
                    .ghost()
                    .icon(IconName::SkipBack)
                    .tooltip(prev_label.clone().to_string())
                    .accessibility_label(prev_label)
                    .disabled(!can_prev)
                    .on_click(move |_, _, cx| {
                        page_prev.update(cx, |this, cx| this.listen_step(-1, cx))
                    }),
            )
            .child(
                Button::new("listen-play")
                    .custom(play_style)
                    .rounded_full()
                    .w(px(64.))
                    .h(px(64.))
                    .p_0()
                    .justify_center()
                    .icon(if playing {
                        IconName::Pause
                    } else {
                        IconName::Play
                    })
                    .disabled(engine_ready != Some(true))
                    .on_click(move |_, _, cx| {
                        page_play.update(cx, |this, cx| this.toggle_speech(cx))
                    }),
            )
            .child(
                Button::new("listen-next")
                    .ghost()
                    .icon(IconName::SkipForward)
                    .tooltip(next_label.clone().to_string())
                    .accessibility_label(next_label)
                    .disabled(!can_next)
                    .on_click(move |_, _, cx| {
                        page_next.update(cx, |this, cx| this.listen_step(1, cx))
                    }),
            );

        Some(
            v_flex()
                .id("reader-listen")
                .occlude()
                .absolute()
                .inset_0()
                .bg(background)
                .text_color(foreground)
                .px(px(LISTEN_PAGE_X))
                .pt(px(self.top_inset + 10.))
                .pb(px(self.bottom_inset + 16.))
                .child(top)
                .child(body)
                .children(engine_note.map(|note| {
                    div()
                        .w_full()
                        .pb_2()
                        .text_center()
                        .text_sm()
                        .text_color(muted)
                        .child(note)
                }))
                .child(controls)
                .into_any_element(),
        )
    }

    /// 书签那一下：钉 / 拔当前位置。状态机在会话里（`model/bookmarks.rs`），
    /// 这一屏只把那一击递过去 —— 顶栏那颗开关的选中态由会话答。
    pub(in crate::ui::features::reader) fn toggle_bookmark(&mut self, cx: &mut Context<Self>) {
        self.reader
            .update(cx, |reader, cx| reader.toggle_bookmark(cx));
    }

    /// 跳到某一章的第几个字。「笔记」目的地跳批注 / 书签的那条路：外壳开完书把落点
    /// 递过来，会话那句 `goto_offset` 自己处理「目标章还没排版」（挂恢复请求，
    /// 排版做完那一帧落地）。
    pub fn goto(&mut self, chapter: usize, offset: usize, cx: &mut Context<Self>) {
        self.reader
            .update(cx, |reader, cx| reader.goto_offset(chapter, offset, cx));
    }

    /// 翻译弹层：从选区菜单那格「翻译」升起的一张卡，锚在菜单落地的那一处
    /// （菜单先收，坐标由会话在升起那一刻拍下 —— 拍的是菜单**顶边中点**，正是尾巴
    /// 原来指的那一处）。形状与 [`Self::footnote_popover`] 同一套：零尺寸触发器 +
    /// `open(true)` + `TopCenter` + `arrow(true)`，点外面 / 按返回由 `on_open_change`
    /// 收回会话；区别只在内容 —— 译文可能长过一屏，所以滚动区只包住引文与译文，
    /// 头部钉住：换语言的那颗下拉在哪儿都够得着。
    fn translation_popover(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let reader = self.reader.clone();
        let anchor = self.reader.read(cx).translation.as_ref()?.anchor;
        // 卡宽给足引文与译文；夹边交给弹层自己（离屏边 8 像素），三角跟着锚点夹在卡的
        // 范围之内，所以贴屏边时尖不会指到卡外去。
        let width = px(f32::from(window.viewport_size().width).max(1.0));
        let (left, top) = match anchor {
            Some(at) => (at.x, at.y),
            None => (width / 2.0, px(72.0)),
        };
        Some(
            div()
                .absolute()
                .left(left)
                .top(top)
                .child(
                    Popover::new("reader-translation")
                        .anchor(Anchor::TopCenter)
                        .arrow(true)
                        .open(true)
                        .on_open_change({
                            let reader = reader.clone();
                            move |open, _, cx| {
                                if !*open {
                                    reader.update(cx, |reader, cx| reader.close_translation(cx));
                                }
                            }
                        })
                        .trigger(
                            Button::new("reader-translation-trigger")
                                .ghost()
                                .p_0()
                                .w(px(0.))
                                .h(px(0.))
                                .min_w(px(0.))
                                .min_h(px(0.)),
                        )
                        // 内容**每帧现建**：Pending → Done 的变化不需要有人敲这一层，
                        // 会话一 notify 重画自然带过来；元素也不是 Clone，闭包里重建
                        // 是唯一不撒谎的写法。
                        .content(move |_, _, cx| translation_card(&reader, cx)),
                )
                .into_any_element(),
        )
    }

    /// 查词典的弹层：与译文同一套挂载（零尺寸触发器 + `Popover` + 顶边中点锚），
    /// 内容换成 [`dictionary_card`]。
    fn dictionary_popover(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let reader = self.reader.clone();
        let anchor = self.reader.read(cx).dictionary.as_ref()?.anchor;
        let width = px(f32::from(window.viewport_size().width).max(1.0));
        let (left, top) = match anchor {
            Some(at) => (at.x, at.y),
            None => (width / 2.0, px(72.0)),
        };
        Some(
            div()
                .absolute()
                .left(left)
                .top(top)
                .child(
                    Popover::new("reader-dictionary")
                        .anchor(Anchor::TopCenter)
                        .arrow(true)
                        .open(true)
                        .on_open_change({
                            let reader = reader.clone();
                            move |open, _, cx| {
                                if !*open {
                                    reader.update(cx, |reader, cx| reader.close_dictionary(cx));
                                }
                            }
                        })
                        .trigger(
                            Button::new("reader-dictionary-trigger")
                                .ghost()
                                .p_0()
                                .w(px(0.))
                                .h(px(0.))
                                .min_w(px(0.))
                                .min_h(px(0.)),
                        )
                        .content(move |_, _, cx| dictionary_card(&reader, cx)),
                )
                .into_any_element(),
        )
    }

    /// 「这本书该怎么切」那张卡：与译文、问 AI 同一类「就地升起的一张卡」，所以复用同一套
    /// 挂载（一张 `Popover`，点外面即收）。它**没有锚点** —— 那一击来自目录右上角的「⋯」，
    /// 不是屏幕上的某一片字，因此摆在正文上方居中那一处。
    fn rules_draft_popover(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let reader = self.reader.clone();
        self.reader.read(cx).rules_draft.as_ref()?;
        let width = px(f32::from(window.viewport_size().width).max(1.0));
        Some(
            div()
                .absolute()
                .left(width / 2.0)
                .top(px(72.0))
                .child(
                    Popover::new("reader-rules-draft")
                        .anchor(Anchor::TopCenter)
                        .arrow(false)
                        .open(true)
                        .on_open_change({
                            let reader = reader.clone();
                            move |open, _, cx| {
                                if !*open {
                                    reader.update(cx, |reader, cx| reader.close_rules_draft(cx));
                                }
                            }
                        })
                        .trigger(
                            Button::new("reader-rules-draft-trigger")
                                .ghost()
                                .p_0()
                                .w(px(0.))
                                .h(px(0.))
                                .min_w(px(0.))
                                .min_h(px(0.)),
                        )
                        .content(move |_, _, cx| draft_card(&reader, cx)),
                )
                .into_any_element(),
        )
    }

    /// 问 AI 的弹层：与译文那张同一条链（零尺寸触发器钉在菜单落地的点上，
    /// `Popover` 拿它的 bounds 摆位，开关由会话的状态管）。
    ///
    /// 不另立第二套卡的形状：头部那颗下拉、引文、正文、滚动区、关闭钮五样与
    /// [`translation_card`] 一一对应，差别只在头部换的是**问法**而不是目标语言。
    fn ai_popover(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let reader = self.reader.clone();
        let anchor = self.reader.read(cx).ai.as_ref()?.anchor;
        let width = px(f32::from(window.viewport_size().width).max(1.0));
        let (left, top) = match anchor {
            Some(at) => (at.x, at.y),
            None => (width / 2.0, px(72.0)),
        };
        Some(
            div()
                .absolute()
                .left(left)
                .top(top)
                .child(
                    Popover::new("reader-ai")
                        .anchor(Anchor::TopCenter)
                        .arrow(true)
                        .open(true)
                        .on_open_change({
                            let reader = reader.clone();
                            move |open, _, cx| {
                                if !*open {
                                    reader.update(cx, |reader, cx| reader.close_ai(cx));
                                }
                            }
                        })
                        .trigger(
                            Button::new("reader-ai-trigger")
                                .ghost()
                                .p_0()
                                .w(px(0.))
                                .h(px(0.))
                                .min_w(px(0.))
                                .min_h(px(0.)),
                        )
                        .content(move |_, _, cx| ai_card(&reader, cx)),
                )
                .into_any_element(),
        )
    }

    /// 撤销胶囊：最近一次**拔掉的书签**的原样恢复。取消划线不挂这一槽（真删，
    /// 见 `model/undo.rs`），所以这颗只服务书签。状态与计时都在会话里（`model/undo.rs`），
    /// 这一屏只画它、把「撤销」那一击递回去。
    /// 摆在正文之上、底栏之下：chrome 开着时悬在底栏卡片上方（右侧让开听书那颗圆钮），
    /// 收着时贴着手势条上方 —— 它是临时的一层，不该推动正文，也不该盖住弹层。
    fn undo_pill(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (label, action) = {
            let reader = self.reader.read(cx);
            let slot = reader.undo.as_ref()?;
            (slot.label.clone(), tr(cx, "reader.undo.action"))
        };
        let bottom = if self.chrome.is_open() {
            150.0
        } else {
            24.0 + self.bottom_inset
        };
        let page = cx.entity().clone();
        Some(
            h_flex()
                .occlude()
                .absolute()
                .left(px(24.0))
                .right(px(80.0))
                .bottom(px(bottom))
                .items_center()
                .justify_between()
                .gap_3()
                .px_4()
                .py_2()
                .popover_style(cx)
                .rounded_full()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(cx.theme().popover_foreground)
                        .child(label),
                )
                .child(
                    Button::new("reader-undo")
                        .ghost()
                        .small()
                        .label(action.to_string())
                        .accessibility_label(action)
                        .on_click(move |_, _, cx| {
                            page.update(cx, |this, cx| {
                                this.reader.update(cx, |reader, cx| reader.undo_delete(cx))
                            })
                        }),
                )
                .into_any_element(),
        )
    }

    /// 「回原处」那一格：从正文点链接/脚注跳出去之后才出现，**底部居中**。
    ///
    /// 与撤销胶囊同一族（都是临时的一格），但两块同时在场时这一格**抬到撤销那条之上**
    /// 而不是并排 —— 那条是 `left: 24 / right: 80` 的横条，居中之后并排会撞在一起。
    /// 右侧那颗 × 只收这一格，不跳转：读者读完了、不想回去时得有个明确的出口，
    /// 否则这一格要等到下一次点链接才换掉。
    ///
    /// 干净态（底栏收起）与开着底栏两种落点沿用撤销胶囊那两个数，不另起一套。
    fn jump_back_pill(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (action, close, undo_up) = {
            let reader = self.reader.read(cx);
            // 没有来源就没有这一格：平时屏上不多一枚按钮。
            reader.jump_back.as_ref()?;
            (
                tr(cx, "reader.jump_back.action"),
                tr(cx, "reader.jump_back.close"),
                reader.undo.is_some(),
            )
        };
        let bottom = if self.chrome.is_open() {
            150.0
        } else {
            24.0 + self.bottom_inset
        };
        let page = cx.entity().clone();
        let dismiss = page.clone();
        Some(
            // 外层只管居中（铺满一条、不 occlude）：真正吃点击的只有里头那颗胶囊。
            h_flex()
                .absolute()
                .left_0()
                .right_0()
                .bottom(px(if undo_up { bottom + 52.0 } else { bottom }))
                .justify_center()
                .child(
                    h_flex()
                        .occlude()
                        .items_center()
                        .gap_1()
                        .px_3()
                        .py_2()
                        .popover_style(cx)
                        .rounded_full()
                        .child(
                            Button::new("reader-jump-back")
                                .ghost()
                                .small()
                                .icon(IconName::ArrowLeft)
                                .label(action.to_string())
                                .accessibility_label(action)
                                .on_click(move |_, _, cx| {
                                    page.update(cx, |this, cx| {
                                        this.reader
                                            .update(cx, |reader, cx| reader.back_to_jump_origin(cx))
                                    })
                                }),
                        )
                        .child(
                            Button::new("reader-jump-back-dismiss")
                                .ghost()
                                .small()
                                .icon(IconName::X)
                                .tooltip(close.clone().to_string())
                                .accessibility_label(close)
                                .on_click(move |_, _, cx| {
                                    dismiss.update(cx, |this, cx| {
                                        this.reader
                                            .update(cx, |reader, cx| reader.dismiss_jump_back(cx))
                                    })
                                }),
                        ),
                )
                .into_any_element(),
        )
    }

    /// 浅深一键，按的仍是**这一本书**。
    ///
    /// 判据是**解析出来的**那一档（`cx.theme().mode`），不是配置里写的 `system`：
    /// 跟着系统走的读者点这一下要落到系统此刻的反面，而不是在 `system` 上原地打转。
    ///
    /// 写的是**这一本**的 `reading_mode` 差量，不是软件外观的 `mode`：夜里读这一本不该
    /// 把书架与设置也一起拧黑，而关掉书之后那一档要还在设置屏选的位置上。
    pub(in crate::ui::features::reader) fn toggle_night(&mut self, cx: &mut Context<Self>) {
        let dark = cx.theme().mode == ThemeMode::Dark;
        ConfigHolder::edit_book_appearance(cx, |appearance| {
            appearance.reading_mode = if dark {
                Some(Mode::Light)
            } else {
                Some(Mode::Dark)
            }
        });
    }

    /// 往正文这条子栈里再压一屏钻入式设置 —— 现在只剩一个入口：目录那屏右上角「⋯」里
    /// 那颗「目录样式」（阅读设置自己那张卡片已经装了行距、段距、版心、字体家族，
    /// 底栏不再有「更多设置」那一格）。
    ///
    /// 手机上没有独立设置窗口（两栏的整页字段塞不进这块小屏），压进栈的这屏吃的是
    /// 与桌面同一份页面规格。压进栈而不是切目的地：系统返回 pop 它就回到刚才那
    /// 一行字，阅读位置一点没动。桌面同样走这条 —— 少一个只在一端存在的窗口，
    /// 就少一处「这功能那端没有」。
    ///
    /// 压的是**这一层自己的**那条栈而不是架面那条：这一层盖在整窗上，架面的栈在它下面，
    /// 压那边的话这一屏会被自己盖住。
    pub(in crate::ui::features::reader) fn open_settings(&mut self, cx: &mut Context<Self>) {
        let settings = cx.new(CompactSettings::new);
        let _ = self.stack.update(cx, |state, cx| {
            state.push(settings, NavMotion::Animated, cx)
        });
    }

    pub(in crate::ui::features::reader) fn advance(
        &mut self,
        delta: isize,
        cx: &mut Context<Self>,
    ) {
        self.reader
            .update(cx, |reader, cx| reader.advance_chapter(delta, cx));
    }

    /// 脚注弹层：从读者点的那一处长出来的一张小纸片，用 gpui-kit 的 [`Popover`]。
    ///
    /// 正文是**画出来的**字与图，不是一棵按链接分好的元素树 —— 没有现成的「标记元素」
    /// 可以当触发器。所以这里放一个**零尺寸的触发器**在锚点上，`Popover` 拿它的 bounds
    /// 定位；开关由会话的状态控制（`open(true)`），点别处 / 按返回把状态清掉，这一格
    /// 下一帧自然消失。
    ///
    /// 锚是 `TopCenter` 加 `arrow(true)`：卡片顶边居中挂在标记下缘，那枚小三角从顶边
    /// 长出去、尖正对标记 —— 与划词那条浮条同一族（浮条的尾巴在 `selection::menu_tail`）。
    /// 夹边交给弹层自己（离屏边 8 像素），所以短的一条注文真的居中在标记上方，而不是
    /// 按最宽那一档偏到左边去。
    ///
    /// 宽度给上限而不是铺满：一条注文常常只有一句，铺满整屏读起来像一屏正文。
    fn footnote_popover(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let note = self.reader.read(cx).footnote()?;
        let text = note.text.clone();
        let anchor = note.anchor;
        let page = cx.entity().clone();
        Some(
            div()
                .absolute()
                .left(anchor.x)
                .top(anchor.y)
                .child(
                    Popover::new("reader-footnote")
                        .anchor(Anchor::TopCenter)
                        .arrow(true)
                        .offset(FOOTNOTE_GAP)
                        .open(true)
                        .on_open_change(move |open, _window, cx| {
                            if !*open {
                                page.update(cx, |this, cx| {
                                    this.reader
                                        .update(cx, |reader, cx| reader.dismiss_footnote(cx))
                                });
                            }
                        })
                        .trigger(
                            Button::new("reader-footnote-trigger")
                                .ghost()
                                .p_0()
                                .w(px(0.))
                                .h(px(0.))
                                .min_w(px(0.))
                                .min_h(px(0.)),
                        )
                        .content(move |_, _, cx| {
                            div()
                                .max_w(FOOTNOTE_CARD_W)
                                .text_sm()
                                .text_color(cx.theme().popover_foreground)
                                .child(text.clone())
                        }),
                )
                .into_any_element(),
        )
    }
}

/// 文稿那一列多宽：整屏减去页边（[`LISTEN_PAGE_X`] 两侧）与滚动条
/// ([`LISTEN_SCROLLBAR`])，再按 [`LISTEN_TEXT_W`] 封顶。折行量的宽与摆出来的宽是同一个
/// 数，所以一格的宽就是字能用的宽；宽过上限之后不再增长 —— 折点不该跟着屏幕无限变。
fn listen_column_width(viewport_width: f32) -> f32 {
    (viewport_width - 2.0 * LISTEN_PAGE_X - LISTEN_SCROLLBAR).clamp(0.0, LISTEN_TEXT_W)
}

/// 把一章拼成书页文稿：段落依次相连，段间 `\n\n`（空行即段距）；段内句子**原样**相连，
/// 句间的原始空白（拉丁文的词间空格、源里的硬换行）照抄 —— 中文两句连排，英文不粘连。
///
/// `paragraph_at(at)` 给「`at` 这个字符所在的段界」；拿不到（章首是无字块、块还没排完）
/// 就**逐字探过去**，与 `model::speech` 的 `sentence_from` 同一条走法 —— 从前这一步是
/// `break`，章首一碰壁整章的文稿就只剩一句（真机「文稿只有一行」的那一半根因）。
///
/// 返回 `(文稿文本, 每句的两套区间)`。区间按朗读顺序排；文稿内区间是 **UTF-8 字节**
/// 偏移，与 gpui 文本下标（`TextRun::len`、`position_for_index`、`InteractiveText`）
/// 同一口径。
fn build_listen_script(
    chars: &[char],
    mut paragraph_at: impl FnMut(usize) -> Option<std::ops::Range<usize>>,
) -> (String, Vec<ListenSentence>) {
    let mut text = String::new();
    let mut sentences: Vec<ListenSentence> = Vec::new();
    let mut at = 0usize;
    while at < chars.len() {
        let Some(paragraph) = paragraph_at(at) else {
            // 段界拿不到的格子（图、空白块）跳过一个字符再探 —— 不是整章放弃。
            at += 1;
            continue;
        };
        if paragraph.end <= at {
            at = paragraph.end.max(at + 1);
            continue;
        }
        // 段落先拼进临时串：空段（全是空白）不落 `\n\n` 那条缝。
        let mut piece = String::new();
        let first_new = sentences.len();
        // 首句之前的空白（段首缩进之类）不进文稿；句间空白照抄，词间空格才留得住。
        let mut gap_from = None;
        for sentence in split_sentences(chars, paragraph.clone()) {
            let chapter = sentence.clone();
            if let Some(from) = gap_from {
                for ch in &chars[from..chapter.start] {
                    piece.push(*ch);
                }
            }
            let script_start = piece.len();
            for ch in &chars[chapter.clone()] {
                piece.push(*ch);
            }
            sentences.push(ListenSentence {
                chapter,
                script: script_start..piece.len(),
            });
            gap_from = Some(sentence.end);
        }
        at = paragraph.end;
        while at < chars.len() && chars[at].is_whitespace() {
            at += 1;
        }
        if piece.is_empty() {
            // 空段进不了文稿，它那几句也不算数。
            sentences.truncate(first_new);
            continue;
        }
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        // 文稿内区间整体平移到本章文稿文本的坐标系里（字节口径）。
        let base = text.len();
        text.push_str(&piece);
        for sentence in &mut sentences[first_new..] {
            sentence.script.start += base;
            sentence.script.end += base;
        }
    }
    (text, sentences)
}

/// 整章排一次：gpui 的 text system 在哪儿折，文稿就到哪儿折 —— 量的尺与画出来的尺
/// 必须完全一致（同一字体族、[`LISTEN_BOOK_PX`]、同一列宽；行高不影响折点，画的那侧
/// 由 `TextStyle::line_height` 走 [`LISTEN_LINE_H`]）。量出来的每一条行给「每句落在
/// 第几行」与内容高用。
///
/// 排不出来（字体没落地、text system 报错）返回空：那一句的行号全部按第 0 行算，
/// 文稿仍摆得出来，只是跟读的落点停在顶部 —— 不如整面空白。
fn listen_shape_script(
    text: &str,
    width: f32,
    family: &SharedString,
    window: &Window,
) -> Vec<WrappedLine> {
    let run = TextRun {
        len: text.len(),
        font: Font {
            family: family.clone(),
            weight: FontWeight::NORMAL,
            ..Default::default()
        },
        ..Default::default()
    };
    window
        .text_system()
        .shape_text(
            SharedString::from(text),
            px(LISTEN_BOOK_PX),
            &[run],
            Some(px(width.max(1.0))),
            None,
        )
        .map(|lines| lines.into_iter().collect::<Vec<_>>())
        .unwrap_or_default()
}

/// `at`（文稿文本内的字节下标）落在第几行：逐行累加字节长，换行符本身不进任何一行的
/// 文本但要占一格下标 —— 与 `TextLayout::position_for_index` 同一条走法，两边的行号
/// 才对得上。越界的下标（量过的行数与建表之间差了一格时）钉在最后一行，不漂出去。
fn line_index_at(line_lens: &[usize], at: usize) -> usize {
    let mut start = 0usize;
    for (line_ix, len) in line_lens.iter().enumerate() {
        let end = start + len;
        if at <= end {
            return line_ix;
        }
        start = end + 1;
    }
    line_lens.len().saturating_sub(1)
}

/// 让第 `ix` 句所在的那一行落在视口正中要滚多少。下端不在这里钳：容器绘制时自己按
/// `max_offset` 钳，跟读那格账记的也是钳过的数。
fn listen_focus_offset(script: &ListenScript, ix: usize, viewport: f32) -> f32 {
    let line = script.line_of.get(ix).copied().unwrap_or(0);
    let want = line as f32 * LISTEN_LINE_H + LISTEN_LINE_H * 0.5 - viewport * 0.5;
    want.max(0.0)
}

/// 这一句在文稿里的下标：`at` 落在哪一句（按章内区间对账）就返回哪一句。
fn sentence_index(sentences: &[ListenSentence], at: usize) -> Option<usize> {
    sentences
        .iter()
        .position(|sentence| sentence.chapter.start <= at && at < sentence.chapter.end)
        .or_else(|| {
            sentences
                .iter()
                .rposition(|sentence| sentence.chapter.start <= at)
        })
}

impl Render for ReadingPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 正文让出干净态那两条带。浮层不参与：它压字，不推字。
        // 「身上压着东西」也报给会话：那一刻正文的手势让位 —— 那一击的语义是收浮层，
        // 而按住一张图不该同时在底下选中字。报的是**有没有**，不是哪一层。
        //
        // 目录与阅读设置两块卡片，两种表面（手机的底部弹层、平板与桌面的左侧抽屉）都
        // **压在正文之上**：抽屉不再占宽，正文因此一寸不让，点字照常选得中 ——
        // 遮罩会先把那一击收掉。
        let size = SizeClass::of(window);
        let wide = !size.is_compact();
        let toc_open = self.toc.read(cx).is_open(cx);
        let settings_open = self.settings.read(cx).is_open(cx);
        let layers_up = self.chrome.is_open()
            || toc_open
            || settings_open
            || self.toc.read(cx).menu_open()
            || self.reader.read(cx).footnote().is_some()
            || self.reader.read(cx).note_editor_open()
            || self.reader.read(cx).translation_open()
            || self.reader.read(cx).dictionary_open()
            || self.reader.read(cx).ai_open()
            || self.reader.read(cx).rules_draft_open()
            || self.image_viewer.read(cx).is_open()
            || self.listen_open;
        // 上下页边距的默认值来自书的 `@page`；读者的「上下页边距」滑条离开 0 就整个
        // 覆盖它（见 `chrome::effective_page_margins`）。带子自己的高与正文让出的高必须
        // 是同一对数，否则滚动时那半行字会从缝里露出来。
        let (header_margin, footer_margin) = chrome::effective_page_margins(
            ConfigHolder::reader(cx).margin_y,
            self.reader.read(cx).page_margins(),
            f32::from(window.viewport_size().width),
        );
        let (header_band, footer_band) =
            chrome::bands_with_vertical_margins(header_margin, footer_margin);
        self.reader.update(cx, |reader, cx| {
            reader.set_reserve(self.reserve, cx);
            reader.set_chrome_bands(header_band, footer_band, self.bottom_inset, cx);
            reader.set_overlay_open(layers_up, cx);
            reader.enter_shift = self.enter_shift;
            // 折页那张纸的顶边要贴屏幕顶（读者 2026-10-30 要的「整屏一张纸」）：
            // 版面不让它，只有这一格用它，见 `view::body` 的 `sheet_area`。
            reader.insets.top = self.top_inset;
        });
        self.sync_sliders(window, cx);
        let tap = cx.listener(
            |this: &mut Self, event: &ClickEvent, window: &mut Window, cx: &mut Context<Self>| {
                this.on_body_tap(event, window, cx)
            },
        );
        // 目录那一层此刻开着。两种表面的遮罩都盖整块窗口，白底顶栏压在半透明黑下面
        // 会成一片灰 —— 而那条上的返回键正是读者开完目录要按的第一颗：又压暗又点不动是
        // 最坏的组合。所以这一档把顶栏抬到遮罩之上，让它保持原色也保持能按。
        //
        // 底栏**不**跟着抬：那几颗（字号、进度、翻页模式）在目录里没有对应物，压在遮罩下
        // 才是「这一层是临时抬起来的」的正确读法，而当初那句「让那 172 像素反而让人以为
        // 还能点」的担心对顶栏不成立 —— 抬上来之后它是真能点。
        let toc_up = toc_open && self.chrome.is_open();
        let column = div()
            .absolute()
            .top_0()
            .bottom_0()
            .left_0()
            .right_0()
            .child(
                // 正文的点击落在这层而不是 `ReaderSession` 自己身上。它的 hitbox 是整屏的，
                // 而 gpui 的一击会发给**每一个**「自己的 hitbox 含有按下点」的 handler
                // （`Window::dispatch_mouse_event` 倒序遍历帧内的监听列表，不是沿祖先链冒泡），
                // 所以顶栏、底栏与目录那一层各自声明挡住身后的 hitbox，
                // 否则浮层的按钮点下去会连着触发这里，把刚切过去的面板当场收掉。
                div()
                    .id("reader-body")
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .on_click(tap)
                    .child(self.reader.clone()),
            )
            // 干净态那两条读数带在会话里画（折页时它们是纸的一部分），这里只管浮层那两条。
            //
            // 顶栏在目录那一层开着时不在这里画 —— 抬到遮罩之上去了（见下面
            // `toc_up` 那一段）。底栏照旧压在遮罩下。
            .when(self.chrome.is_open() && !toc_up, |this| {
                this.child(self.top_bar(wide, cx))
                    .child(self.bottom_bar(cx))
            })
            .when(self.chrome.is_open() && toc_up, |this| {
                this.child(self.bottom_bar(cx))
            });
        // 阅读页的根底色是**正文纸**：跟随正文主题（选了哪套就是哪张纸），不再
        // 跟壳主题走 —— 设置页弹到纸上面时，白壳配羊皮纸正是分离想要的样子。
        let paper = reading_palette(cx);
        let mut root = div()
            .id("reader-page")
            .size_full()
            .relative()
            .bg(paper.bg)
            .text_color(paper.fg)
            .child(column);
        // 目录那一屏最后挂：`anchored` 层按登记的先后决定谁在上面，它盖的是整块窗口，
        // 顶栏底栏因此压在遮罩下面（让那 172 像素反而会让读者以为底栏还能点，而那一击的
        // 真实归属是遮罩）。它每帧都构造，收着时也构造 —— 弹簧的采样在里面（见
        // `TocPanel::render` 与 `widgets::bottom_sheet` 那段模块注释）。右上角「⋯」那条
        // 菜单是 gpui-kit 自己的 `PopupMenu`，由那颗按钮按下时挂进它自己的浮层。
        root = root.child(self.toc.clone());
        // 阅读设置那张卡片挂在目录之后：两块弹层由底栏那两颗入口互斥地打开（见
        // [`ReadingPage::toggle_settings`] 与 [`ReadingPage::toggle_toc`]），真叠到一处时
        // 卡片在上面，返回键那一下也先收它 —— 与 `dismiss_chrome` 里那个顺序同一份读法。
        root = root.child(self.settings.clone());
        // 抬上来的顶栏：还是原来那一颗条，只是挂在目录之后。包一层绝对定位容器，
        // 几何就不变（正文现在整窗铺开，没有栏要让）。
        if toc_up {
            root = root.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .child(self.top_bar(wide, cx)),
            );
        }
        // 脚注弹层压在目录之上、图片查看器之下：它是「从正文里抬起来的一张小纸」，
        // 盖住目录那种整屏弹层是对的，而放大看图时读者要的是图。
        root = root.children(self.footnote_popover(cx));
        // 翻译弹层压在脚注之上：两者都是「从正文里升起的一张卡」，同一时刻只该有
        // 一张在场（开翻译时菜单先收、脚注按各自的路径收）。
        root = root.children(self.translation_popover(window, cx));
        // 词典那张与译文同一层：开词典时菜单先收，两者不会同时在场。
        root = root.children(self.dictionary_popover(window, cx));
        // 问 AI 那张压在译文之上：两件事共用同一个锚点，后开的盖住先开的那张，
        // 与「读者最后要看的正是他刚点的那一件事」同一条读法。
        root = root.children(self.ai_popover(window, cx));
        // 「这本书该怎么切」那张压在问 AI 之上：同一时刻只该有一张从正文里升起来的卡在场，
        // 而它是从目录那一侧升起来的，比正文里的一次划词更晚。
        root = root.children(self.rules_draft_popover(window, cx));
        // 撤销胶囊压在脚注之上：它是「刚删了什么」的回执，比一条注文更要紧；
        // 图片查看器照旧最上面。
        root = root.children(self.undo_pill(cx));
        // 「回原处」那一格压在撤销之后：两块同时在场时它自己抬到撤销那条之上（见
        // [`ReadingPage::jump_back_pill`]）。
        root = root.children(self.jump_back_pill(cx));
        // 查看器最后挂：它盖住的正是顶栏、底栏与弹层，所以谁都不该浮在它上面。
        // 没开着时这一格是空的，一个节点都不多。
        //
        // 像素每帧由这一层向图集要（并预取左右那两张），落点与「定位原文」都归会话，
        // 所以这三样都递给它：图集那格解完了要喊**这一屏**重画（`view` 传的是本页的
        // 实体号，不是会话的 —— 喊会话还会绕一圈回到这里，多一趟元素树）。
        let atlas = self.reader.read(cx).atlas.clone();
        let viewer_layer_top = 0.0;
        root = root.children(image_viewer::overlay(
            &self.image_viewer,
            &atlas,
            &self.reader,
            cx.entity().entity_id(),
            self.top_inset,
            viewer_layer_top,
            &self.image_viewer_focus,
            window,
            cx,
        ));
        // 「界面亮度」在这一端的**另一手**：桌面上背光不归窗口管（那是显示器的地盘），
        // 于是把读者要的那一成压成一层黑，透明度从同一个数算出来（[`chrome::dim_alpha`]）。
        // Android 上这一格恒为 0 —— 那边的暗走的是真背光（`platform::set_screen_brightness`），
        // 判据只写在 [`platform::BRIGHTNESS_IS_BACKLIGHT`] 那一处：两处各压一次就是压两次。
        let dim = if platform::BRIGHTNESS_IS_BACKLIGHT {
            0.0
        } else {
            chrome::dim_alpha(ConfigHolder::appearance(cx).brightness)
        };
        // 最后挂，连图片查看器那一层也一起暗：这一格说的是「这块屏」，不是「正文那一栏」。
        // 色取主题的 `overlay`（那一格本来就是给屏幕压暗用的颜色），只换它的透明度 ——
        // 应用层不嵌裸色值。
        let scrim = (dim > 0.0).then(|| {
            let overlay = cx.theme().overlay;
            div()
                .absolute()
                .inset_0()
                .bg(Hsla { a: dim, ..overlay })
                .into_any_element()
        });
        // 听书那一屏压在所有弹层与图片查看器之上：它是一块整窗的专用屏，开着的时候
        // 底下那些浮层都不该再露出来。亮度那一层照旧最上面（它说的是「这块屏」）。
        root = root.children(self.listen_overlay(window, cx));
        root.children(scrim)
    }
}

/// 翻译弹层的卡片本体。弹层开着时**每帧现建**：状态（待译 / 到手 / 失败）从会话
/// 现读，目标语言从配置现读 —— 换语言那一下只写配置并重发请求，这里自然跟上。
/// 引擎名照实摆着：gtx 是非合同接口，读者该知道这份译文出自谁。
/// 问 AI 那张卡。头部那颗下拉换的是**问法**（三档，见 `folio_core::net::ai::Ask`），
/// 换一次重新问一遍 —— 这一族不落缓存，所以「换一档」就是一次真请求，界面上不装作免费。
fn ai_card(reader: &Entity<ReaderSession>, cx: &mut Context<PopoverState>) -> AnyElement {
    let ai_reader = reader.clone();
    // 当前那一档在**建卡这一刻**读一次就够：菜单每次打开重建的是勾选项，而这张卡本身
    // 每帧重建（见 `translation_card` 上那条同样的话），闭包里再借 `reader` 就会让
    // 借用检查把它的生命周期拖进闭包。
    let current_ask = reader.read(cx).ai.as_ref().map(|state| state.ask);
    let ask_menu = move |menu: PopupMenu, _: &mut Window, _cx: &mut Context<PopupMenu>| {
        let mut menu = menu.min_w(px(200.));
        for ask in Ask::ALL {
            let item_reader = ai_reader.clone();
            menu = menu.item(
                PopupMenuItem::new(tr(_cx, ask_label_key(ask)).to_string())
                    .checked(Some(ask) == current_ask)
                    .on_click(move |_, _, cx| {
                        item_reader.update(cx, |reader, cx| reader.set_ai_ask(ask, cx))
                    }),
            );
        }
        menu
    };
    let Some(state) = reader.read(cx).ai.as_ref() else {
        // 弹层开着而状态没了（返回键刚收掉那一帧）：画一张空卡比 panic 诚实。
        return div().into_any_element();
    };
    let excerpt: SharedString = {
        let mut head: String = state.source.chars().take(120).collect();
        if state.source.chars().count() > 120 {
            head.push('…');
        }
        head.into()
    };
    let body = match &state.status {
        AiStatus::Pending => div()
            .py_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(tr(cx, "reader.ai.pending")),
        AiStatus::Done { text, tokens } => v_flex()
            .gap_1()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .child(text.clone()),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(state.label.clone()),
                    )
                    .children(tokens.map(|count| {
                        let count = count.to_string();
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(tf(cx, "reader.ai.tokens", &[("n", &count)]))
                    })),
            ),
        AiStatus::Failed(reason) => div()
            .py_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(tf(cx, "reader.ai.failed", &[("reason", reason)])),
    };
    let close_reader = reader.clone();
    let close_label = tr(cx, "reader.ai.dismiss");
    v_flex()
        .w(TRANSLATION_CARD_W)
        .gap_2()
        .child(
            h_flex()
                .w_full()
                .items_center()
                .gap_1()
                .child(
                    Button::new("ai-ask")
                        .ghost()
                        .small()
                        .label(tr(cx, ask_label_key(state.ask)).to_string())
                        .accessibility_label(tr(cx, "reader.ai.ask"))
                        .dropdown_menu_with_anchor(Anchor::TopLeft, ask_menu),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(state.label.clone()),
                )
                .child(
                    Button::new("ai-close")
                        .ghost()
                        .small()
                        .icon(IconName::X)
                        .tooltip(close_label.clone().to_string())
                        .accessibility_label(close_label)
                        .on_click(move |_, _, cx| {
                            close_reader.update(cx, |reader, cx| reader.close_ai(cx))
                        }),
                ),
        )
        .child(
            // 引文与答案同住一条滚动区，头部钉在它外面：换问法那颗下拉在哪儿都够得着。
            div()
                .id("reader-ai-scroll")
                .max_h(px(280.0))
                .overflow_y_scrollbar()
                .child(
                    v_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .line_clamp(2)
                                .text_color(cx.theme().muted_foreground)
                                .child(excerpt),
                        )
                        .child(body),
                ),
        )
        .into_any_element()
}

/// 卡片本体。四档状态画四样东西，其中「解开了但切不动」那一档**没有**「用上」那颗按钮：
/// 判据是引擎按整本书算出来的章数与中位字数（`novel::draft::Preview::is_usable`），
/// 摆一颗会写坏配置的按钮比不摆更坏 —— 那条规则会全局生效，而这本书只是没切出来。
fn draft_card(reader: &Entity<ReaderSession>, cx: &mut Context<PopoverState>) -> AnyElement {
    let Some(state) = reader.read(cx).rules_draft.as_ref() else {
        // 弹层开着而状态没了（返回键刚收掉那一帧）：画一张空卡比 panic 诚实。
        return div().into_any_element();
    };
    let head = match &state.status {
        DraftStatus::Pending => v_flex().gap_1().child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(tr(cx, "reader.draft.pending")),
        ),
        DraftStatus::Ready {
            draft,
            preview,
            usable,
        } => {
            let shape = match draft.marker.kind {
                MatchKind::Regex => draft
                    .marker
                    .pattern
                    .as_deref()
                    .unwrap_or_default()
                    .to_string(),
                MatchKind::Literal => draft.marker.text.as_deref().unwrap_or_default().to_string(),
            };
            let measure = tf(
                cx,
                "reader.draft.preview",
                &[
                    ("chapters", &preview.chapters.to_string()),
                    ("median", &preview.median_body_chars.to_string()),
                ],
            );
            let titles = preview.titles.join(" · ");
            let body = v_flex()
                .gap_1()
                // 理由与形状：读者先认「它说的是不是我这本」，再看那条模式。
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().foreground)
                        .child(draft.reason.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(draft.marker.name.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .font_family(cx.theme().mono_font_family.clone())
                        .child(shape),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().foreground)
                        .child(measure),
                );
            let body = if titles.is_empty() {
                body
            } else {
                body.child(
                    div()
                        .text_xs()
                        .line_clamp(2)
                        .text_color(cx.theme().muted_foreground)
                        .child(tf(cx, "reader.draft.titles", &[("titles", &titles)])),
                )
            };
            if *usable {
                body.child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("draft-apply")
                                .label(tr(cx, "reader.draft.apply").to_string())
                                .primary()
                                .on_click({
                                    let apply = reader.clone();
                                    move |_, _, cx| {
                                        apply
                                            .update(cx, |reader, cx| reader.apply_chapter_rules(cx))
                                    }
                                }),
                        )
                        .child(
                            Button::new("draft-keep-away")
                                .ghost()
                                .label(tr(cx, "reader.draft.dismiss").to_string())
                                .on_click({
                                    let away = reader.clone();
                                    move |_, _, cx| {
                                        away.update(cx, |reader, cx| reader.close_rules_draft(cx))
                                    }
                                }),
                        ),
                )
            } else {
                body.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr(cx, "reader.draft.unusable")),
                )
            }
        }
        DraftStatus::Applied { name } => {
            v_flex()
                .gap_1()
                .child(div().text_sm().text_color(cx.theme().foreground).child(tf(
                    cx,
                    "reader.draft.applied",
                    &[("name", name)],
                )))
        }
        DraftStatus::Failed(reason) => v_flex().gap_1().child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(tf(cx, "reader.draft.failed", &[("reason", reason)])),
        ),
    };
    let close_label = tr(cx, "reader.draft.close");
    let close_reader = reader.clone();
    v_flex()
        .w(TRANSLATION_CARD_W)
        .gap_2()
        .child(
            h_flex()
                .w_full()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr(cx, "reader.draft.title")),
                )
                .child(
                    Button::new("draft-close")
                        .ghost()
                        .small()
                        .icon(IconName::X)
                        .tooltip(close_label.clone().to_string())
                        .accessibility_label(close_label)
                        .on_click(move |_, _, cx| {
                            close_reader.update(cx, |reader, cx| reader.close_rules_draft(cx))
                        }),
                ),
        )
        .child(head)
        .into_any_element()
}

/// 三档问法的文案键。住在界面这一侧：键名是文案的事，core 那边只管提示词。
fn ask_label_key(ask: Ask) -> &'static str {
    match ask {
        Ask::Explain => "reader.ai.ask.explain",
        Ask::Terms => "reader.ai.ask.terms",
        Ask::Plain => "reader.ai.ask.plain",
    }
}

fn dictionary_card(reader: &Entity<ReaderSession>, cx: &mut Context<PopoverState>) -> AnyElement {
    let Some(state) = reader.read(cx).dictionary.as_ref() else {
        // 弹层开着而状态没了（返回键刚收掉的那一帧）：画一张空卡比 panic 诚实。
        return div().into_any_element();
    };
    let close_label = tr(cx, "reader.dict.dismiss");
    let body = match &state.status {
        DictionaryStatus::Pending => div()
            .py_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(tr(cx, "reader.dict.pending")),
        DictionaryStatus::Done { hits } if hits.is_empty() => div()
            .py_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(tf(cx, "reader.dict.empty", &[("word", &state.query)])),
        DictionaryStatus::Done { hits } => v_flex().gap_3().children(hits.iter().map(|hit| {
            v_flex()
                .gap_1()
                .child(
                    h_flex()
                        .items_baseline()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(hit.word.clone()),
                        )
                        .children(hit.phonetic.clone().map(|phonetic| {
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(phonetic)
                        })),
                )
                .child(div().text_sm().child(hit.meaning.clone()))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(hit.source.clone()),
                )
        })),
        DictionaryStatus::Failed(reason) => div()
            .py_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(tf(cx, "reader.dict.failed", &[("reason", reason)])),
    };
    let close_reader = reader.clone();
    v_flex()
        .w(DICTIONARY_CARD_W)
        .gap_2()
        .child(
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(tr(cx, "reader.dictionary")),
                )
                .child(div().flex_1())
                .child(
                    Button::new("dictionary-close")
                        .ghost()
                        .small()
                        .icon(IconName::X)
                        .tooltip(close_label.clone().to_string())
                        .accessibility_label(close_label)
                        .on_click(move |_, _, cx| {
                            close_reader.update(cx, |reader, cx| reader.close_dictionary(cx))
                        }),
                ),
        )
        .child(
            div()
                .id("reader-dictionary-scroll")
                .max_h(px(320.0))
                .overflow_y_scrollbar()
                .child(body),
        )
        .into_any_element()
}

fn translation_card(reader: &Entity<ReaderSession>, cx: &mut Context<PopoverState>) -> AnyElement {
    let current_lang = translation::target_lang(cx);
    let language_label: SharedString = translation::LANGUAGES
        .iter()
        .find(|(code, _)| *code == current_lang)
        .map(|(_, label)| (*label).into())
        .unwrap_or_else(|| current_lang.clone().into());
    let close_label = tr(cx, "reader.translation.dismiss");
    // 语言下拉的选中态在菜单每次打开时重建（builder 是 `Fn`），当前码从配置现读；
    // 换一档 = 写配置 + 源文原样重译，命令在会话里（`set_translation_lang`）。
    let lang_reader = reader.clone();
    let lang_menu = move |menu: PopupMenu, _: &mut Window, cx: &mut Context<PopupMenu>| {
        let current = translation::target_lang(cx);
        let mut menu = menu.min_w(px(180.));
        for (code, label) in translation::LANGUAGES {
            let (item_reader, item_code) = (lang_reader.clone(), *code);
            menu = menu.item(
                PopupMenuItem::new(*label)
                    .checked(*code == current)
                    .on_click(move |_, _, cx| {
                        item_reader
                            .update(cx, |reader, cx| reader.set_translation_lang(item_code, cx))
                    }),
            );
        }
        menu
    };
    let Some(translation) = reader.read(cx).translation.as_ref() else {
        // 弹层开着而状态没了（返回键刚收掉的那一帧）：画一张空卡比 panic 诚实。
        return div().into_any_element();
    };
    let excerpt: SharedString = {
        let mut head: String = translation.source.chars().take(120).collect();
        if translation.source.chars().count() > 120 {
            head.push('…');
        }
        head.into()
    };
    let body = match &translation.status {
        TranslationStatus::Pending => div()
            .py_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(tr(cx, "reader.translation.pending")),
        TranslationStatus::Done {
            text,
            detected,
            cached,
        } => v_flex()
            .gap_1()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .child(text.clone()),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(match detected {
                                Some(code) => {
                                    tf(cx, "reader.translation.detected", &[("lang", &code)])
                                }
                                None => translation.engine.clone(),
                            }),
                    )
                    .when(*cached, |row| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(tr(cx, "reader.translation.cached")),
                        )
                    }),
            ),
        TranslationStatus::Failed(reason) => div()
            .py_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(tf(cx, "reader.translation.failed", &[("reason", reason)])),
    };
    let close_reader = reader.clone();
    // 卡的表面（底色、描边、投影、12 内边距）由 `Popover` 的 `appearance` 给，这里不再
    // 叠第二份 —— 从前这里也来一份 `popover_style` + `p_3`，于是「卡里还有一层卡」，
    // 里外两圈投影与描边各画一遍，读起来像弹窗里又浮了一层。
    v_flex()
        .w(TRANSLATION_CARD_W)
        .gap_2()
        .child(
            h_flex()
                .w_full()
                .items_center()
                .gap_1()
                .child(
                    Button::new("translation-lang")
                        .ghost()
                        .small()
                        .label(language_label.to_string())
                        .accessibility_label(language_label)
                        .dropdown_menu_with_anchor(Anchor::TopLeft, lang_menu),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(translation.engine.clone()),
                )
                .child(
                    Button::new("translation-close")
                        .ghost()
                        .small()
                        .icon(IconName::X)
                        .tooltip(close_label.clone().to_string())
                        .accessibility_label(close_label)
                        .on_click(move |_, _, cx| {
                            close_reader.update(cx, |reader, cx| reader.close_translation(cx))
                        }),
                ),
        )
        .child(
            // 引文与译文同住一条滚动区，头部钉在它外面：换语言的那颗下拉在哪儿都够得着。
            div()
                .id("reader-translation-scroll")
                .max_h(px(280.0))
                .overflow_y_scrollbar()
                .child(
                    v_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .line_clamp(2)
                                .text_color(cx.theme().muted_foreground)
                                .child(excerpt),
                        )
                        .child(body),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    //! 听书那一屏里几件不用窗口就能钉死的事：那一列多宽、整章怎么拼成书页文稿（段界
    //! 拿不到时逐字探，不整章放弃）、「每句落在第几行」怎么数、跟读的落点怎么算、以及
    //! 「现在念到第几句」。摆出来好不好看只有真机量得出来，但这几格错了会直接对上读者
    //! 的两句抱怨：文稿只有一行，或者文稿根本不跟着声音走。

    use super::{
        LISTEN_LINE_H, LISTEN_TEXT_W, ListenScript, ListenSentence, build_listen_script,
        line_index_at, listen_column_width, listen_focus_offset, sentence_index,
    };

    /// 列宽：手机窄、平板宽；宽过 [`LISTEN_TEXT_W`] 之后不再增长（折点不跟着屏幕无限变）。
    #[test]
    fn the_column_grows_with_the_window_then_stops_at_the_cap() {
        let phone = listen_column_width(384.0);
        let desktop = listen_column_width(1280.0);
        assert!(phone > 0.0, "再窄也得摆得下几个字：{phone}");
        assert!(desktop > phone, "宽屏一行摆得更多");
        assert_eq!(desktop, LISTEN_TEXT_W, "封顶就是那一列的宽");
        assert_eq!(
            desktop,
            listen_column_width(800.0),
            "宽过上限之后不再跟着窗口长"
        );
    }

    /// 整章拼成书页文稿：段与段之间空一行（`\n\n`），句间原始空白照抄（拉丁词距留得住），
    /// 段首空白不进文稿；每句的两套区间——章内（喂 TTS）与文稿内（高亮与点击，UTF-8
    /// 字节偏移）——都要指得回同一句话。
    #[test]
    fn the_chapter_is_stitched_into_one_page_with_paragraph_gaps() {
        // char 下标：0-1 两个空格，2-5「甲一句。」，6 空格，7-10「甲二句。」，
        // 11-14「乙一句。」，15-18「乙二句。」。
        let chars: Vec<char> = "  甲一句。 甲二句。乙一句。乙二句。".chars().collect();
        let (text, sentences) = build_listen_script(&chars, |at| match at {
            0..=10 => Some(0..11),
            11..=18 => Some(11..19),
            _ => None,
        });
        assert_eq!(
            text, "甲一句。 甲二句。\n\n乙一句。乙二句。",
            "段首空白丢掉，句间空格照抄，段间空一行"
        );
        let expect = [
            (2..6, "甲一句。"),
            (7..11, "甲二句。"),
            (11..15, "乙一句。"),
            (15..19, "乙二句。"),
        ];
        assert_eq!(sentences.len(), expect.len());
        for (sentence, (chapter, display)) in sentences.iter().zip(expect) {
            assert_eq!(sentence.chapter, chapter, "章内区间是 TTS 的落点");
            assert_eq!(
                &text[sentence.script.clone()],
                display,
                "文稿内字节区间切出来必须还是那一句"
            );
        }
    }

    /// 段界拿不到的字符**逐字探过去**，不整章放弃 —— 章首是无字块（图、空白）时，
    /// 从前的 `break` 让整章文稿只剩一句，这是「文稿只有一行」的那一半根因。
    #[test]
    fn characters_without_a_paragraph_are_probed_past_not_abandoned() {
        let chars: Vec<char> = "图图图甲一句。乙一句。".chars().collect();
        let (text, sentences) = build_listen_script(&chars, |at| {
            if at < 3 {
                None // 前三个字符落在无字块上，段界给不出
            } else {
                Some(3..chars.len())
            }
        });
        assert_eq!(text, "甲一句。乙一句。");
        assert_eq!(sentences.len(), 2, "整章的句子一句不能少");
        assert_eq!(sentences[0].chapter, 3..7);
        assert_eq!(sentences[1].chapter, 7..11);
    }

    /// 全是空白的段不落 `\n\n` 那条缝：文稿里不该出现凭空的空行。
    #[test]
    fn a_whitespace_paragraph_leaves_no_gap() {
        let chars: Vec<char> = "  甲一句。".chars().collect();
        let (text, sentences) = build_listen_script(&chars, |at| match at {
            0..=1 => Some(0..2), // 只有空白的段
            2..=5 => Some(2..6),
            _ => None,
        });
        assert_eq!(text, "甲一句。");
        assert_eq!(sentences.len(), 1);
        assert_eq!(&text[sentences[0].script.clone()], "甲一句。");
    }

    /// 「第几行」的数法与 `TextLayout::position_for_index` 同一条：行末的下标算那一行，
    /// 换行符本身占一格下标。数错了，跟读的落点就一行一行地漂。
    #[test]
    fn the_line_index_walks_bytes_the_way_the_text_layout_does() {
        let lens = [5usize, 3, 4];
        assert_eq!(line_index_at(&lens, 0), 0);
        assert_eq!(line_index_at(&lens, 4), 0);
        assert_eq!(line_index_at(&lens, 5), 0, "行末下标算这一行");
        assert_eq!(line_index_at(&lens, 6), 1, "换行符占一格下标");
        assert_eq!(line_index_at(&lens, 9), 1);
        assert_eq!(line_index_at(&lens, 10), 2);
        assert_eq!(line_index_at(&lens, 999), 2, "越过末尾钉在最后一行");
        assert_eq!(line_index_at(&[], 0), 0, "没量到排版时落在第一行");
    }

    /// 跟读的落点：让当句那一行落在视口正中；顶到头时钳在 0（不出现负位移）；
    /// 下一句的落点必须比上一句远，否则屏不会动。下端交给容器自己钳。
    #[test]
    fn the_focus_offset_centers_the_line_and_never_goes_negative() {
        let script = ListenScript {
            chapter: 0,
            text: "".into(),
            sentences: vec![],
            line_of: vec![0, 20, 30],
            width: 300.0,
            complete: true,
        };
        let viewport = 600.0;
        assert_eq!(
            listen_focus_offset(&script, 0, viewport),
            0.0,
            "第一行居中会顶出上界，钳在 0"
        );
        let middle = listen_focus_offset(&script, 1, viewport);
        let target = 20.0 * LISTEN_LINE_H + LISTEN_LINE_H * 0.5 - viewport * 0.5;
        assert!((middle - target).abs() < 0.01, "第 20 行要落在视口正中");
        assert!(
            listen_focus_offset(&script, 2, viewport) > middle,
            "念到下一句落点必须变，不然文稿根本不跟手"
        );
        assert_eq!(
            listen_focus_offset(&script, 999, viewport),
            0.0,
            "下标越界按第一行算，不 panic"
        );
    }

    /// 当前句的落点：落在句内直接命中；落在句间的空白里回到刚念完那一句；
    /// 越过最后一句钉在末尾（列表不会滚出界）。
    #[test]
    fn the_active_sentence_is_the_one_containing_the_offset() {
        let entry = |chapter: std::ops::Range<usize>| ListenSentence {
            chapter,
            script: 0..0,
        };
        let sentences = vec![entry(0..3), entry(5..10), entry(13..16)];
        assert_eq!(sentence_index(&sentences, 1), Some(0));
        assert_eq!(
            sentence_index(&sentences, 4),
            Some(0),
            "句间空白归刚念完那一句"
        );
        assert_eq!(sentence_index(&sentences, 7), Some(1));
        assert_eq!(
            sentence_index(&sentences, 999),
            Some(2),
            "越过末尾钉在最后一句"
        );
        assert_eq!(sentence_index(&[], 0), None);
    }
}
