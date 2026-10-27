//! 目录那一屏：整棵目录树 —— 书头、「已读 N / M」那一行、搜索、列表 —— 装在同一份状态里。
//! 手机档用底部弹层升起它；平板与 PC 档用一张压在正文左边、底下垫遮罩的抽屉。
//!
//! 顶上那两格在「目录」与「全文」之间来回摆（`Screen`）：目录那一格摆的是这本书的目录树，
//! 全文那一格摆的是正文里搜到的那几处。两格共用同一个容器、同一块书头、同一份行高与刻度，
//! 换的是列表本体与顶上那一句读数 —— 换的从来不该是形状。
//!
//! 从 `page.rs` 搬出来独立成一个实体，是因为这一屏要跨帧持有的状态一大把（整棵树与它的
//! 折叠集、搜索词与那颗输入框、弹层开合与停在哪一档、虚拟列表的尺寸表与滚动句柄、书头查回来的
//! 那一行书目），而它们与阅读页自己的那几样（浮层开合、三条滑条、图片查看器）改的从来不是
//! 同一件事。搬完之后 `page.rs` 只剩一个句柄：`toc`。
//!
//! 分工照 `reader` 这一层一贯的样子：**判断**在 [`crate::ui::features::reader::model::toc_tree`]（摊平、折叠、当前行、
//! 搜索，纯函数，宿主的 `cargo test` 直接跑），**会话那一侧**的跳转命令在
//! `modules/toc.rs`，这一文件只管「这一屏摆成什么、点了哪一行要做什么」。正文里找那几个字
//! 本身不在这里 —— 在 [`folio_core::search`]，这一屏只把它的读数摆出来、把落点交给会话。
//!
//! 它向外只报两件事（[`TocEvent`]）：跳到了某一章（顶栏底栏与排版面板都在阅读页手里，
//! 收干净得由它来），以及「目录样式」那一下（设置那一屏要压进导航栈，栈也在阅读页手里）。

use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use folio_core::Priority;
use folio_core::infra::task::{Cancel, TaskEvent, TaskHandle};
use folio_core::search::{Found, Hit, Options, Query, scan};
use folio_core::store::{Annotation, BookRow};
use gpui_kit::assets::IconName;
use gpui_kit::base::{VirtualListScrollHandle, v_virtual_list};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme, Icon, Selectable, Sizable, ThemeStyled as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, Context, ElementId, Entity, EventEmitter, FontWeight,
    Image, InteractiveElement, IntoElement, ObjectFit, ParentElement, Pixels, Render,
    ScrollStrategy, SharedString, Size, StatefulInteractiveElement, Styled, StyledImage,
    Subscription, Task, WeakEntity, Window, div, hsla, img, px, size,
};

use crate::ui::components::{BottomSheet, BottomSheetState, SearchBox, SearchBoxEvent};
use crate::ui::features::reader::model::annotations::Palette;
use crate::ui::features::reader::model::chrome::percent_label;
use crate::ui::features::reader::model::session::ReaderSession;
use crate::ui::features::reader::model::toc_tree::TocView;
use crate::ui::features::shelf::{AUTHOR_SEP, fmt_size};
use crate::ui::foundation::images::cover_image;
use crate::ui::services::config::ConfigHolder;
use crate::ui::services::l10n::{tf, tr};
use crate::ui::services::resident;
use crate::ui::shell::navigation::SizeClass;

/// 目录这一屏的横向刻度，全部是在 Meizu 20（scale 2.8）上逐行数参照 App 的墨迹
/// 边界量出来的（dp = px / 2.8）。
///
/// 面板的统一左右内缩：书头那张封面与列表文字都从这条起（实测封面左沿 x=41px）。
const TOC_GUTTER: f32 = 15.0;
/// 当前行左边那条标记条的宽（实测 x=6..13px）。
const TOC_MARKER: f32 = 2.9;
/// 那条标记条距屏幕左边多少。
const TOC_MARKER_INSET: f32 = 2.1;
/// 当前行那条底色带的左边界 = 标记条的右沿（实测灰底从 x=14px 起，一直铺到屏幕右边）。
const TOC_BAND_LEFT: f32 = TOC_MARKER_INSET + TOC_MARKER;
/// 顶层标题文字的左边界（实测 L1 墨迹左沿 x=87..91px，两条独立行互相对上）。
const TOC_TEXT_LEFT: f32 = 31.0;
/// chevron 那一列的宽度。叶子也占同样一格，标题才不会因为有没有下级而左右跳。
const TOC_CHEVRON: f32 = 15.3;
/// 每一级往右让多少（实测 L1 文字 87px、L2 文字 120px → 差 32px）。
///
/// 这一档比「看得出挂在下级下面」的直觉窄得多，但参照 App 三级书也只让这么多：
/// 小屏上缩进每多一级就吃掉一行能摆几个字，层级靠 chevron 那一列已经读得出来。
const TOC_INDENT: f32 = 11.4;

/// 目录弹层的两档：高的一档留 15% 屏幕让读者认「这是从哪本书抬上来的」（顶上那截还露着
/// 正文与章名），半屏档是扫「当前章上下各几章」的那一眼 —— 目录真正的用途是扫，所以打开
/// 就落在高的一档，缩到半屏是主动选择。
///
/// 0.85 是读者 2026-10-16 定的那一档（原来是 0.92，留 8%）：留得多一点，遮罩底下那截正文
/// 与顶栏的章名还看得见，「我在读哪一本」不用靠书头那一行去认。
const TOC_EXPANDED: f32 = 0.85;
const TOC_PEEK: f32 = 0.5;
/// 宽档（平板 / PC）上目录抽屉的宽。
///
/// 这一档不再是占住正文左边那一条栏：抽屉盖在正文上、底下压一层遮罩，正文一寸不让
/// （读者开着目录时也还看得见字，关掉时更不该让字跳一下）。320 是「书头那三行 + 两三级
/// 缩进 + 长标题」还读得顺的最小一档，再窄就只剩书名和章名了。
const TOC_DRAWER: f32 = 320.0;
/// 宽档抽屉离屏幕左边与底边的留白。上边距留给顶栏（44）与它下面的缝。
const TOC_DRAWER_INSET: f32 = 12.0;
/// 宽档抽屉的顶边：顶栏那 44 像素从它上面走，抽屉从顶栏底下长出来。
const TOC_DRAWER_TOP: f32 = 56.0;
/// 目录列表底部留出的空隙：滚到最后一条时，那一行不贴着弹层下沿（否则最后一行
/// 与手势条/圆角挤在一起，读起来像被切掉了）。
const TOC_BOTTOM_SPACE: f32 = 24.0;
/// 全文那一档每一行的高：两行字（上面那行章名，下面那行上下文）。
///
/// **这个数没在 Meizu 20 上量过** —— `TOC_*` 那几档刻度都是从参照 App 的墨迹边界数出来的，
/// 这一屏没有参照可比。它比目录那两档（38 / 46）高出一截是应当的，因为多一行字；真机上
/// 要核对的是这一行高与「一句上下文摆得下几个字」之间的关系（那由核心那两档裁切决定）。
const SEARCH_ROW: f32 = 52.0;
/// 「批注」那一格每一行的高：与全文那一档同一族的两行字（原文/笔记一行，出处一行）。
/// 同样没有参照可比 —— 它随批注闭环一起进来，真机上要一起核。
const MARK_ROW: f32 = 56.0;
/// 这一屏顶上那三格：目录树，还是正文全文，还是书签与批注。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Screen {
    /// 目录树（默认那一格）。
    #[default]
    Toc,
    /// 正文里搜出来的那几处。
    Search,
    /// 书签与批注：本内的清单，点行跳回去，行尾可删。
    Marks,
}

/// 正文全文那一档的状态：在跑的那一趟、摆出来的命中、进度读数。
///
/// 为什么收成一块而不是摊平进 [`TocPanel`] 的字段：这几样同生同灭 —— 每一次换查询都要
/// 整体清一遍（`reset`），漏掉任何一格的症状都是「上一趟的命中挂在新的查询下面」。
struct Search {
    /// 在跑的那一趟的取消旗标。`Some` = 有一趟还没落地。
    cancel: Option<Cancel>,
    /// 事件桥那条异步链。**必须活着**：它一 drop，接收端就没了，结果永远不会进来
    /// （架面那条导入链记过同一件事）。
    stream: Option<Task<()>>,
    hits: Vec<Hit>,
    /// 进度读数：扫到第几章 / 一共几章。总章数由那趟扫描的第一条进度事件带来。
    scanned: usize,
    chapters: usize,
    /// 到了命中上限就收的手 —— 后面**可能还有**，读数里要说清。
    capped: bool,
    /// 有没有一趟在跑（顶上那一句念它）。
    running: bool,
}

impl Search {
    fn new() -> Self {
        Self {
            cancel: None,
            stream: None,
            hits: Vec::new(),
            scanned: 0,
            chapters: 0,
            capped: false,
            running: false,
        }
    }

    /// 作废在跑的那一趟并把读数清零。换查询走它（新的词与旧的命中不能同桌摆着）。
    fn reset(&mut self) {
        self.cancel_inflight();
        self.hits.clear();
        self.scanned = 0;
        self.chapters = 0;
        self.capped = false;
    }

    /// 只在跑的那一趟作废，读数留着 —— 这一层收起来不等于读者放弃了那个词。
    fn cancel_inflight(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.trigger();
        }
        // 断开那条桥：接收端一没，后台那一头的 `send_blocking` 就报错自己收手。
        self.stream = None;
        self.running = false;
    }

    /// 顶上那一句读数：还在跑就说扫到第几章，跑完说命中几处。
    fn label(&self, cx: &App) -> SharedString {
        if self.running {
            return tf(
                cx,
                "reader.search.scanning",
                &[("done", &self.scanned), ("total", &self.chapters)],
            )
            .into();
        }
        if self.hits.is_empty() {
            return "".into();
        }
        let hits = self.hits.len();
        // 到上限收手的那一趟不报「命中 N 处」了事 —— 那一屏得说「只看了前 N 处」，
        // 否则读者以为全书就这几处，而真实数是没数过的。
        tf(
            cx,
            if self.capped {
                "reader.search.capped"
            } else {
                "reader.search.hits"
            },
            &[("hits", &hits), ("total", &self.chapters)],
        )
        .into()
    }
}

/// 「批注」那一格的一行。书签与批注各占一行，字段是两族的并集：书签没有颜色与
/// 笔记，批注没有「书签」这个身份。原文那句（`quote`）取自锚点里存的 `exact`，
/// 与导出同一条口径 —— 不开排版也有得显。
struct MarkRow {
    is_bookmark: bool,
    id: i64,
    /// 跳转落点：哪一章（定版式即哪一页）+ 章内第几个字。
    chapter: usize,
    offset: usize,
    /// 出处那句（会话给：章名，定版式是「第几页」）。
    place: String,
    quote: String,
    note: String,
    color: Option<String>,
    /// 库里的 stale 标记。本次解没解得回要到正文里才答得出来，这一格只转述存档的结论。
    stale: bool,
}

/// 这一屏那四块，装配一次、两种容器各摆各的（见 [`TocPanel::pieces`]）。
struct Pieces {
    /// 钉住的那一行：「已读 N / M」+ 搜索 + ⋯（抽屉那一档再多一颗关闭）。
    header: AnyElement,
    head: AnyElement,
    /// 搜索那一行：只在点开搜索之后出现。
    search_row: Option<AnyElement>,
    list: AnyElement,
}

/// 目录面板顶上那块书头要的那几样：一次打开查一回库。
///
/// 存整条 [`BookRow`] 而不是拆成书名 / 作者 / 格式三个字段：拆出来的那几份与库里那一行
/// 是同一件事的两处记录，读者在「信息」那一屏改过书名之后就会有一处是旧的。
/// 第三行那个「读到 X%」不在这里 —— 它是当场算出来的进度，存进来就成了第二真相源。
struct BookHeader {
    row: BookRow,
    /// 解码好的封面。`None` 画一块灰底，不留一格空白。
    cover: Option<Arc<Image>>,
}

/// 目录那一屏向外报的两件事。跳转与开设置的动作都长在**阅读页**那一层（浮层的开合状态、
/// 导航栈都在它手里），这一屏只说「读者要的是哪个」。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::ui::features::reader) enum TocEvent {
    /// 从目录里跳到了某一章：字已经在了，顶栏底栏与排版面板该整个收干净。
    Navigated,
    /// 「⋯」里那颗「目录样式」：设置那一屏要压进导航栈。
    OpenSettings,
}

/// 目录那一屏自己的状态。跨帧持有，一页一个（阅读页建它，随阅读页一起活）。
pub(in crate::ui::features::reader) struct TocPanel {
    /// 那一本书。树的形状、当前章、到过的章、进度都从它问，跳转命令也派给它。
    reader: Entity<ReaderSession>,
    /// 底部弹层：开合与停在哪一档（全屏 / 半屏）都在它手里。句柄必须跨帧持有 ——
    /// 弹层每帧都要构造（弹簧的采样在里面），与三条滑条同一条理由。
    sheet: Entity<BottomSheetState>,
    /// 目录树开着时的那份状态（整棵树 + 折叠集 + 搜索词）。`None` = 还没按当前位置
    /// 建过 —— 每次重新打开都重建一次，于是「树跟着阅读位置走」这条规矩不会被上一次
    /// 手动折的格带过来。
    tree: Option<TocView>,
    /// 顶上那两格现在摆的是哪一格（目录树 / 正文全文）。
    screen: Screen,
    /// 正文全文那一档的状态。
    search: Search,
    /// 「批注」那一格的行表。每帧在 [`Self::marks_pieces`] 里重建一次 —— 书签与批注
    /// 都是几十条的量级，重建比「记得哪一格脏了」便宜，也不会有忘了失效的缓存。
    marks: Vec<MarkRow>,
    /// 搜索框开没开。框自己的状态由 `window.use_keyed_state` 按这一屏的键持有，
    /// 所以这里只存一个开关。
    search_open: bool,
    /// 虚拟列表的逐项尺寸表。只在「行数或行高变了」时重建，不每帧分配一张千项的表。
    sizes: Rc<Vec<Size<Pixels>>>,
    list_len: usize,
    row_height: f32,
    /// 那张表是为哪一格造的。两档的行数与行高都可能恰好相同（都是一行空态），
    /// 少了这一格就会把目录那档的表留给全文这档用。
    sized_on: Screen,
    /// 列表的滚动句柄。**必须由这一屏跨帧持有**：`v_virtual_list` 每次调用都会新建一份
    /// 内部句柄，若不在建列表时用 `.track_scroll` 顶掉它，滚动偏移就每帧归零 —— 手指滑了、
    /// 下一帧又弹回原处，看起来像「滑不动」。
    scroll: VirtualListScrollHandle,
    /// 全文那一档列表的滚动句柄。与上面那条分开：两档是两份列表，共用一个句柄会在切换时
    /// 把上一档的偏移带到这一档上（症状是一进来列表就停在半截）。
    search_scroll: VirtualListScrollHandle,
    /// 下一次画全文那一格时，把焦点送进那颗框（读者刚点的是「全文」那一格，他下一步就是打字，
    /// 手机的软键盘只有焦点进框那一条路能把它叫出来）。画完即清，不许每帧都抢焦点 ——
    /// 抢了两次就会把读者自己点在别处的焦点吹掉。
    focus_wanted: bool,
    /// 右上角那颗「⋯」的菜单开没开。菜单本体是 gpui-kit 的 `PopupMenu`（按下即弹，
    /// 点别处/选中即收），这一格只为了让那颗按钮吃到选中态，并让阅读页知道「身上压着东西」。
    more_open: bool,
    /// 书头那几行。与目录树同一次打开一起建（见 [`TocPanel::toggle`]）。
    header: Option<BookHeader>,
    _watches: Vec<Subscription>,
}

impl TocPanel {
    /// 建这一屏。弹层状态订阅一次：它不是这一屏的子视图，`notify` 只有观察方才收得到，
    /// 而开合、换档、读者一把下拉关掉全靠随后那一次重画。
    pub(in crate::ui::features::reader) fn new(
        reader: Entity<ReaderSession>,
        cx: &mut Context<Self>,
    ) -> Self {
        let sheet = cx.new(BottomSheetState::closed);
        let watches = vec![cx.observe(&sheet, |_, _, cx| cx.notify())];
        Self {
            reader,
            sheet,
            tree: None,
            screen: Screen::default(),
            search: Search::new(),
            marks: Vec::new(),
            search_open: false,
            sizes: Rc::new(Vec::new()),
            list_len: 0,
            row_height: 0.0,
            sized_on: Screen::default(),
            scroll: VirtualListScrollHandle::new(),
            search_scroll: VirtualListScrollHandle::new(),
            focus_wanted: false,
            more_open: false,
            header: None,
            _watches: watches,
        }
    }

    /// 弹层开着吗。阅读页拿它决定「身上压着东西」要不要报给会话（那一击的语义是收浮层，
    /// 不是选字），也拿它决定底栏那一格的选中态。
    pub(in crate::ui::features::reader) fn is_open(&self, cx: &App) -> bool {
        self.sheet.read(cx).is_open()
    }

    /// 那颗「⋯」的菜单开着吗。它也是压在正文之上的一层。
    pub(in crate::ui::features::reader) fn menu_open(&self) -> bool {
        self.more_open
    }

    /// 目录弹层：开 or 关。
    ///
    /// 开的时候按当前位置重算一遍：读者上次手动折的那些格不该跨次生效，他这一次要回答的
    /// 问题还是「我现在在哪」。搜索框与「⋯」那一层一并收掉；书头跟着重查（读者可能在
    /// 「信息」那一屏改过书名或作者）。
    pub(in crate::ui::features::reader) fn toggle(&mut self, cx: &mut Context<Self>) {
        if self.sheet.read(cx).is_open() {
            self.hide(cx);
            return;
        }
        self.tree = None;
        self.search_open = false;
        // 每次打开都从目录那一格起，全文那一档的查询与命中整个清掉：读者这次打开这一屏
        // 要回答的问题是「我现在在哪」，不是「上一次我找的那个词在哪儿」。
        self.screen = Screen::default();
        self.search.reset();
        self.header = self.read_header(cx);
        self.sheet.update(cx, |sheet, cx| sheet.show(cx));
        cx.notify();
    }

    /// 收掉这一层（读者点了遮罩、下拉到底、或按了系统返回）。
    pub(in crate::ui::features::reader) fn hide(&mut self, cx: &mut Context<Self>) {
        // 一趟扫描最坏要读一整本书（实测 432 万字 274 ms）。这一层已经收了，
        // 那份力气没有要的人 —— 作废它，别让它跟着一本没人看的正文跑完。
        self.search.cancel_inflight();
        self.sheet.update(cx, |sheet, cx| sheet.hide(cx));
        cx.notify();
    }

    /// 查一次书头要的那几样（书名 / 作者 / 格式 / 大小 / 封面）。
    ///
    /// 查不到就 `None`：兜底那一行没有书，那时候这一屏只剩「已读」那一行与一句实话，
    /// 而不是摆半截空书头让读者以为这本书坏了。
    ///
    /// 在打开这一层时查而不是在渲染里查，与架面点开「信息」那一屏同一取舍：一次打开
    /// 一回，两趟查询（`book` 与 `book_cover`），不每帧把界面线程钉在 SQLite 上。
    fn read_header(&self, cx: &App) -> Option<BookHeader> {
        let reader = self.reader.read(cx);
        let store = reader.store.clone()?;
        let id = reader.book_id?;
        let row = store.book(id).ok().flatten()?;
        let cover = match store.book_cover(id).ok().flatten() {
            Some(bytes) => cover_image(&bytes),
            None => None,
        };
        Some(BookHeader { row, cover })
    }

    /// 按当前位置建一次目录树。已经建过就不动 —— 重建会把读者手动折的格冲掉；
    /// 「重新打开要跟着位置走」那条重置在 [`Self::toggle`] 里显式做。
    ///
    /// 建好做两件事：① 把当前章那条的**祖先链**展开（其余分支照旧全折），读者一进来
    /// 就看得见「我在第几章的哪一节下面」，判据与为什么只在这一次做见 `TocView::reveal`；
    /// ② 把列表滚到那一行。`scroll_to_item` 记的是「下一趟布局要滚到哪」，此刻列表还没搭
    /// —— 正合适，虚拟列表建起来那一帧自己会应用它。
    fn ensure_tree(&mut self, cx: &mut Context<Self>) {
        if self.tree.is_some() {
            return;
        }
        let rows = self.reader.read(cx).toc_rows();
        let current = self.reader.read(cx).chapter_index();
        // 定当前行要**章内偏移**与这一章的锚点表：只看章号的话，一个文件挂十条小节的书
        // （《未来简史》就是这样）永远定在那个父级行上，而父级行自己还折着 —— 展开祖先
        // 等于什么都没展开。判据见 `toc_tree::position_row`。
        let offset = self
            .reader
            .read(cx)
            .position_in_chapter()
            .map_or(0, |(_, at)| at);
        let offsets = self.reader.read(cx).id_offsets();
        let mut toc = TocView::open(rows);
        if let Some(ix) = toc.locate(current, offset, &offsets) {
            toc.reveal(ix);
        }
        let landing = toc
            .landing_ix(current)
            .and_then(|ix| toc.visible_position(ix));
        self.tree = Some(toc);
        if let Some(ix) = landing {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
    }

    /// 这一屏那四块：钉住的表头、书头、（开着的）搜索行、列表。
    ///
    /// 底部弹层与宽档抽屉两种容器读的是同一份装配 —— 哪些行看得见、折了哪几枝、搜的什么词、
    /// 行高哪一档，换容器不该改任何一条。`width` 递进来只为一件事：虚拟列表那张逐项尺寸表
    /// 要用**它自己那块容器**的宽，而不是视口的宽。
    ///
    /// 目录那一格与全文那一格也是同一份装配的形状（表头 / 书头 / 那一行框 / 列表），
    /// 换的只有后两块 —— 顶上那两格切换不该让书头跳一位。
    ///
    /// 这几块子元素先各自算完：`cx.theme()` 还回去的 `&Theme` 借的是 `*cx`，把它留在
    /// 一条链里会活过整条 `let`，链上那几个要 `&mut cx` 的调用就过不去。
    fn pieces(&mut self, window: &mut Window, drawer: bool, cx: &mut Context<Self>) -> Pieces {
        // 尺寸表用**这一屏自己那块容器**的宽：弹层铺满视口，抽屉只有 `TOC_DRAWER`。
        let width = if drawer {
            TOC_DRAWER
        } else {
            f32::from(window.viewport_size().width)
        };
        match self.screen {
            Screen::Toc => self.toc_pieces(window, width, drawer, cx),
            Screen::Search => self.fulltext_pieces(window, width, drawer, cx),
            Screen::Marks => self.marks_pieces(window, width, drawer, cx),
        }
    }

    /// 目录那一格：树 + 那行按标题筛的框。
    fn toc_pieces(
        &mut self,
        window: &mut Window,
        width: f32,
        drawer: bool,
        cx: &mut Context<Self>,
    ) -> Pieces {
        self.ensure_tree(cx);
        // 搜索框的状态按这一屏的键持有，订阅句柄跟着它一起活：输入一改变就重画这一屏。
        let search = window.use_keyed_state("reader-toc-search", cx, |window, cx| {
            let input = cx.new(|cx| InputState::new(window, cx));
            let watch = cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify());
            (input, watch)
        });
        // 只克隆那个句柄：对 `search.read(cx)` 整体调 `.clone()` 克隆的是**引用**
        // （`&T: Clone` 是平凡实现），拿回来的还是借用的元组，下面要 `&mut cx` 的
        // 每一次调用都会撞在它上面。
        let input = search.read(cx).0.clone();
        if let Some(toc) = &mut self.tree {
            toc.set_query(input.read(cx).value().to_string());
        }

        let row_height = ConfigHolder::reader(cx).toc_density.row_height();
        let count = self.tree.as_ref().map_or(0, |toc| toc.visible().len());
        self.set_sizes(count, row_height, width);

        Pieces {
            head: self.book_head(cx),
            header: self.header_row(drawer, cx),
            search_row: self.search_open.then(|| self.filter_row(&input, cx)),
            list: self.tree_list(cx),
        }
    }

    /// 正文全文那一格：那颗框 + 命中列表。
    fn fulltext_pieces(
        &mut self,
        window: &mut Window,
        width: f32,
        drawer: bool,
        cx: &mut Context<Self>,
    ) -> Pieces {
        // 框的状态由窗口按这个键持有（与目录那行筛标题的框同一个办法）：它要跨帧活过每一帧
        // 重画，不然每帧新建一颗，光标与输入法那边刚接上的焦点就没了。
        // 订阅把它报回这一屏 —— **词一变就重扫一趟**，不每帧去读它的值。
        let panel = cx.entity().clone();
        let holder = window.use_keyed_state("reader-fulltext-box", cx, move |window, cx| {
            let boxed =
                cx.new(|cx| SearchBox::new(tr(cx, "reader.search.placeholder"), window, cx));
            let watch = cx.subscribe(&boxed, move |_, _, event: &SearchBoxEvent, cx| {
                let SearchBoxEvent::Change(value) = event;
                let value = value.clone();
                panel.update(cx, |this, cx| this.submit(&value, cx));
            });
            (boxed, watch)
        });
        let boxed = holder.read(cx).0.clone();
        if self.focus_wanted {
            // 焦点进框 = 软键盘出来（`gpui-mobile` 只有 FocusLost 那一条收键盘的路，
            // 反过来说进来这一条就是唯一能让键盘自己出来的路）。
            self.focus_wanted = false;
            boxed.update(cx, |boxed, cx| boxed.focus(window, cx));
        }
        self.set_sizes(self.search.hits.len(), SEARCH_ROW, width);

        Pieces {
            head: self.book_head(cx),
            header: self.header_row(drawer, cx),
            search_row: Some(self.fulltext_row(&boxed, cx)),
            list: self.hit_list(cx),
        }
    }

    /// 「批注」那一格：书签与批注的清单。书签在前（钉下的顺序），批注在后（按章序、
    /// 同章按钉下的顺序）—— 前者回答「我夹过哪几页」，后者回答「我写过哪几句」。
    ///
    /// 行表每帧重建：会话手里的两份清单就是权威，这里不做第二份账。
    fn marks_pieces(
        &mut self,
        _window: &mut Window,
        width: f32,
        drawer: bool,
        cx: &mut Context<Self>,
    ) -> Pieces {
        self.marks = self.build_marks(cx);
        self.set_sizes(self.marks.len(), MARK_ROW, width);
        Pieces {
            head: self.book_head(cx),
            header: self.header_row(drawer, cx),
            search_row: None,
            list: self.marks_list(cx),
        }
    }

    /// 从会话手里折出行表。书签的落点是行首（锚住的就是那一行），批注的落点是锚点的
    /// 起始偏移 —— 目标章多半还没排版，落点交给 `goto_offset` 挂恢复请求。
    fn build_marks(&self, cx: &App) -> Vec<MarkRow> {
        let reader = self.reader.read(cx);
        let mut rows = Vec::with_capacity(reader.bookmarks.len() + reader.annotations.len());
        for b in &reader.bookmarks {
            rows.push(MarkRow {
                is_bookmark: true,
                id: b.id,
                chapter: b.chapter as usize,
                offset: b.anchor.start,
                place: reader.hit_place_at(b.chapter as usize, cx),
                quote: snippet(&b.anchor.exact),
                note: String::new(),
                color: None,
                stale: false,
            });
        }
        // 按章序排（同章保持创建序）：扫批注的人认的是「第几章里写了什么」，
        // 与目录、导出同一份读法。
        let mut annotations: Vec<&Annotation> = reader.annotations.iter().collect();
        annotations.sort_by_key(|a| (a.chapter, a.created_at, a.id));
        for a in annotations {
            rows.push(MarkRow {
                is_bookmark: false,
                id: a.id,
                chapter: a.chapter as usize,
                offset: a.anchor.start,
                place: reader.hit_place_at(a.chapter as usize, cx),
                quote: snippet(&a.anchor.exact),
                note: a.note.trim().to_string(),
                color: Some(a.color.clone()),
                stale: a.stale,
            });
        }
        rows
    }

    /// 「批注」那一格的列表。空态一句实话：这一屏两样都还没有。
    fn marks_list(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.marks.is_empty() {
            return div()
                .w_full()
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .px(px(TOC_GUTTER))
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(tr(cx, "reader.marks.none"))
                .into_any_element();
        }
        let view = cx.entity().clone();
        div()
            .flex_1()
            .min_h_0()
            .pb(px(TOC_BOTTOM_SPACE))
            .child(
                v_virtual_list(
                    view,
                    "reader-marks",
                    self.sizes.clone(),
                    |panel, range, _, cx| panel.mark_rows(range, cx),
                )
                .track_scroll(&self.scroll),
            )
            .into_any_element()
    }

    /// `[range.start, range.end)` 那几行：左侧颜色点或书签图标，原文/笔记一行，
    /// 出处与提醒一行，行尾一颗删除。
    fn mark_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let end = range.end.min(self.marks.len());
        let palette = Palette::of(cx.theme());
        let (foreground, muted) = (cx.theme().foreground, cx.theme().muted_foreground);
        let panel = cx.entity().clone();
        let mut out = Vec::new();
        for ix in range.start..end {
            let row = &self.marks[ix];
            let MarkRow {
                is_bookmark,
                id,
                chapter,
                offset,
                place,
                quote,
                note,
                color,
                stale,
            } = row;
            // 行首那枚识别物：书签是图标，批注是那一条颜色（认不出的名字由
            // `Palette` 兜回默认那枚，与正文回显同一口径）。
            let badge = if *is_bookmark {
                Icon::new(IconName::Bookmark)
                    .small()
                    .text_color(muted)
                    .into_any_element()
            } else {
                div()
                    .flex_none()
                    .w(px(8.0))
                    .h(px(8.0))
                    .rounded_full()
                    .bg(palette.color(color.as_deref().unwrap_or_default()))
                    .into_any_element()
            };
            let second: SharedString = if *stale {
                tf(cx, "reader.marks.stale", &[("place", place)]).into()
            } else if note.is_empty() {
                place.clone().into()
            } else {
                format!("{place} · {note}").into()
            };
            let (panel_delete, panel_jump) = (panel.clone(), panel.clone());
            let (is_bookmark, id) = (*is_bookmark, *id);
            let (chapter, offset) = (*chapter, *offset);
            let quote = quote.clone();
            let place = place.clone();
            out.push(
                div()
                    .id(ElementId::named_usize("reader-mark-row", ix))
                    .w_full()
                    .h(px(MARK_ROW))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px(px(TOC_GUTTER))
                    .overflow_hidden()
                    .child(badge)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .overflow_hidden()
                            .child(
                                div()
                                    .w_full()
                                    .text_sm()
                                    .text_color(foreground)
                                    .truncate()
                                    .child(if quote.is_empty() { place } else { quote }),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .text_xs()
                                    .text_color(muted)
                                    .truncate()
                                    .child(second),
                            ),
                    )
                    .child(
                        Button::new("mark-delete")
                            .ghost()
                            .small()
                            .icon(IconName::Trash)
                            .on_click(move |_, _, cx| {
                                panel_delete
                                    .update(cx, |this, cx| this.remove_mark(is_bookmark, id, cx))
                            }),
                    )
                    .on_click(move |_, _, cx| {
                        panel_jump.update(cx, |this, cx| this.jump_to_hit(chapter, offset, cx))
                    })
                    .into_any_element(),
            );
        }
        out
    }

    /// 删一行：书签与批注各派各的命令，撤销走同一条路（胶囊由阅读页渲染）。
    /// 列表下一帧按会话手里的清单重建，这里不用自己摘行。
    fn remove_mark(&mut self, is_bookmark: bool, id: i64, cx: &mut Context<Self>) {
        if is_bookmark {
            self.reader
                .update(cx, |reader, cx| reader.remove_bookmark(id, cx));
        } else {
            self.reader
                .update(cx, |reader, cx| reader.remove_annotation(id, cx));
        }
        cx.notify();
    }

    /// 顶上那句读数（「批注」那一格）：书签几枚、批注几条。
    fn marks_label(&self, cx: &App) -> SharedString {
        let reader = self.reader.read(cx);
        let bookmarks = reader.bookmarks.len();
        let notes = reader.annotations.len();
        tf(
            cx,
            "reader.marks.count",
            &[("bookmarks", &bookmarks), ("notes", &notes)],
        )
        .into()
    }

    /// 行数（折展、搜索、命中会改）、行高（设置里换了密度）、或**换了哪一格**，三者之一变了
    /// 才重造那张尺寸表。
    ///
    /// 换一格也要重造：两档的行高不同（目录按设置的密度，全文按两行字），而「几行」恰好
    /// 相等是常事（都只有一条空态）—— 只比行数与行高就会把目录那档的表留给全文这档用。
    fn set_sizes(&mut self, count: usize, row_height: f32, width: f32) {
        if count != self.list_len
            || (row_height - self.row_height).abs() > 0.01
            || self.sized_on != self.screen
        {
            self.sizes = Rc::new(vec![size(px(width), px(row_height)); count]);
            self.list_len = count;
            self.row_height = row_height;
            self.sized_on = self.screen;
        }
    }

    /// 目录那一屏（手机档）：一块底部弹层，停在「全屏 / 半屏」两档上，往下拖过头就关掉。
    ///
    /// 换成弹层换来的是**一套统一的退场**：拇指在屏幕上往下一划、点遮罩、按返回，三条路都
    /// 收这一层。以前只有「点正文那一下」收得掉它，而那一击在目录开着的时候语义是含糊的 ——
    /// 读者以为点的是空白，实际点的是遮罩，而遮罩底下还有整屏的正文 hitbox 在等同一下。
    ///
    /// 顶栏与底栏一起压在遮罩下面，所以这一屏不再为它们让出那 172 像素，只还回把手带那 36：
    /// 在 Meizu 20（逻辑高约 857）上净多约 67 像素（Compact 档两行不到），桌面窗口越矮省得
    /// 越多。行高密度还是从设置读，不因为换了容器就改读者量出来的那一档。
    ///
    /// 列表自带滚动，所以这里**不开** `surface_draggable`：内容区那一把长拖该归目录自己滚，
    /// 判「滚到顶了再接着往下拽」这一步 gpui 给不出偏移。
    fn sheet_element(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let sheet = BottomSheet::new(&self.sheet, "reader-toc")
            .expanded(TOC_EXPANDED)
            .peek(TOC_PEEK);
        // 收稳了就不白算这一屏：整棵树要逐层展开成可见行、尺寸表要比对、搜索框要回读。
        // 元素照旧构造（弹簧的采样在里面），只是这一帧不给内容。
        if self.sheet.read(cx).is_idle() {
            return sheet.into_any_element();
        }
        let pieces = self.pieces(window, false, cx);
        sheet
            // 「已读 N / M」+ 搜索 + ⋯ 那一行钉在把手下面、不跟着列表滚：读者翻到第 800 章
            // 那一带时还得够得着搜索和「⋯」。
            //
            // 书头**也钉住**（曾经设想它跟着列表滚，但列表是 `v_virtual_list`、自己就是滚动
            // 容器：它是书头的兄弟，两者不在同一条滚动里，滑的只会是列表）。实测下来钉住更好认
            // ——「这是哪一本、读到哪儿了」始终在眼前。搜索那一行只在点开搜索后出现，也跟着钉。
            .header(pieces.header)
            .child(pieces.head)
            .children(pieces.search_row)
            .child(pieces.list)
            .into_any_element()
    }

    /// 目录那一屏（平板 / PC 档）：压在正文左边的一张抽屉，底下垫一层遮罩。
    ///
    /// 以前这一档是**占位**的左侧栏：正文让出 240，顶栏也跟着从栏右边起，读者看到的是
    /// 「一开目录，返回栏被挤走了」。改成浮层之后两件事都消失：正文一寸不让，顶栏留在
    /// 原地（阅读页把它抬到遮罩之上），目录只盖住左边一条，右边那大半屏的字还看得见。
    ///
    /// 与手机弹层的**唯一**区别是容器：四块内容、行高、折叠、搜索全同一份
    /// （见 [`Self::pieces`]）。遮罩负责两件事：点它即收（与底部弹层同一条退场），
    /// 以及把正文的整屏 hitbox 拦住 —— 不拦的话，点抽屉里的一片空白会同时被正文吃掉，
    /// 在正文那一侧就是「顺手翻了页」。
    fn drawer_element(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if !self.sheet.read(cx).is_open() {
            // 没开着就一个节点都不多：没有占宽，也没有一条看不见的遮罩挡着正文。
            return div().into_any_element();
        }
        let pieces = self.pieces(window, true, cx);
        let background = cx.theme().background;
        let overlay = cx.theme().overlay;
        let close_panel = cx.entity().clone();
        let scrim = div()
            .id("reader-toc-scrim")
            .occlude()
            .absolute()
            .inset_0()
            .bg(overlay)
            .on_click(move |_, _, cx| close_panel.update(cx, |panel, cx| panel.hide(cx)));
        let drawer = div()
            .id("reader-toc-drawer")
            .occlude()
            .absolute()
            .left(px(TOC_DRAWER_INSET))
            .top(px(TOC_DRAWER_TOP))
            .bottom(px(TOC_DRAWER_INSET))
            .w(px(TOC_DRAWER))
            .flex()
            .flex_col()
            // 圆角要把行底色与列表一起裁进来，否则当前行那条灰带会从卡片角上露出去。
            .overflow_hidden()
            .bg(background)
            .popover_style(cx)
            .rounded(px(16.0))
            .child(pieces.header)
            .child(pieces.head)
            .children(pieces.search_row)
            .child(pieces.list);
        div()
            .id("reader-toc-overlay")
            .absolute()
            .inset_0()
            .child(scrim)
            .child(drawer)
            .into_any_element()
    }

    /// 目录右上角那颗「⋯」：四件事收进一条 gpui-kit 的 `PopupMenu`。
    ///
    /// 换成组件换来的是**一整套现成的开合**：按下即弹、点别处即收、上下键走位、回车确认，
    /// 全在 `PopupMenu` 里，这一屏不再自己垫底、不再判「点在不在卡片里」。菜单内容在
    /// **每次打开时重建**（`DropdownMenu` 的 builder 是 `Fn`），所以它读到的是当下状态
    /// （展开 / 折叠、逆向排序开没开），与旧的那块弹层同一套语义。四件事各自把命令派回
    /// 这一屏的既有方法。
    ///
    /// 锚点选 `BottomRight`：菜单挂在按钮的右下角，往下展开时不会盖住它自己的那张脸。
    fn more_button(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let more_label = tr(cx, "reader.toc.more");
        let build = {
            let panel = cx.entity().clone();
            move |menu: PopupMenu, _window: &mut Window, cx: &mut Context<PopupMenu>| {
                let (all_open, reversed) = {
                    let this = panel.read(cx);
                    let all_open = this.tree.as_ref().is_some_and(|toc| toc.all_open());
                    let reversed = this.tree.as_ref().is_some_and(|toc| toc.reversed());
                    (all_open, reversed)
                };
                let expand_label = tr(
                    cx,
                    if all_open {
                        "reader.toc.collapse_all"
                    } else {
                        "reader.toc.expand_all"
                    },
                );
                let reverse_label = tr(cx, "reader.toc.reverse");
                let style_label = tr(cx, "reader.toc.style");
                let clear_label = tr(cx, "reader.toc.clear_read");
                // 四个闭包各自要一份句柄（`Entity` 不 `Copy`），所以一件事一个名字。
                let panel_expand = panel.clone();
                let panel_reverse = panel.clone();
                let panel_style = panel.clone();
                let panel_clear = panel.clone();
                menu.min_w(px(180.))
                    .item(
                        PopupMenuItem::new(expand_label)
                            .icon(if all_open {
                                IconName::FoldVertical
                            } else {
                                IconName::UnfoldVertical
                            })
                            .on_click(move |_, _, cx| {
                                panel_expand.update(cx, |this, cx| this.toggle_expand_all(cx))
                            }),
                    )
                    .item(
                        PopupMenuItem::new(reverse_label)
                            .icon(IconName::ArrowDownUp)
                            .checked(reversed)
                            .on_click(move |_, _, cx| {
                                panel_reverse.update(cx, |this, cx| this.toggle_reversed(cx))
                            }),
                    )
                    .item(
                        PopupMenuItem::new(style_label)
                            .icon(IconName::SlidersHorizontal)
                            .on_click(move |_, _, cx| {
                                panel_style.update(cx, |this, cx| this.settings_wanted(cx))
                            }),
                    )
                    .item(
                        PopupMenuItem::new(clear_label)
                            .icon(IconName::Eraser)
                            .on_click(move |_, _, cx| {
                                panel_clear.update(cx, |this, cx| this.clear_read_marks(cx))
                            }),
                    )
            }
        };
        let open_panel = cx.entity().clone();
        Button::new("toc-more")
            .ghost()
            .small()
            .selected(self.more_open)
            .icon(IconName::CircleEllipsis)
            .tooltip(more_label.clone().to_string())
            .accessibility_label(more_label)
            .dropdown_menu_with_anchor(Anchor::BottomRight, build)
            .on_open_change(move |open, _window, cx| {
                let open = *open;
                open_panel.update(cx, |this, cx| {
                    if this.more_open != open {
                        this.more_open = open;
                        cx.notify();
                    }
                });
            })
            .into_any_element()
    }

    /// 头部那一行：「已读 N / M 章」+ 搜索 + ⋯。
    ///
    /// ⋯ 里放的是这一屏用得上的那四件事（展开目录 / 逆向排序 / 目录样式 / 清除已读痕迹），
    /// 见 [`Self::more_button`]。原先这里直接是一颗「目录样式」，那是四件事里的一件，
    /// 摆成一颗按钮就把其余三件挤没了。
    fn header_row(&mut self, drawer: bool, cx: &mut Context<Self>) -> AnyElement {
        let current = self.reader.read(cx).chapter_index();
        let (read, total) = self
            .tree
            .as_ref()
            .map(|toc| toc.read_count(toc.current_ix(current)))
            .unwrap_or((0, 0));
        let read_label = tf(cx, "reader.toc.read", &[("read", &read), ("total", &total)]);
        // 顶上那几句读数跟着那一格换：目录那一格答「来过几章」，全文那一格答「命中几处 /
        // 扫到第几章」，批注那一格答「书签几枚 · 批注几条」。同一个位置摆几句就是几屏挤成
        // 一屏，而读者要的从来只是当下那一格的答案。
        let label: SharedString = match self.screen {
            Screen::Toc => read_label.into(),
            Screen::Search => self.search.label(cx),
            Screen::Marks => self.marks_label(cx),
        };
        let search_panel = cx.entity().clone();
        let search_label = tr(cx, "reader.toc.search");
        let toc_panel = cx.entity().clone();
        let fulltext_panel = cx.entity().clone();
        let marks_panel = cx.entity().clone();
        let toc_screen_label = tr(cx, "reader.toc");
        let fulltext_screen_label = tr(cx, "reader.search.label");
        let marks_screen_label = tr(cx, "reader.toc.marks");
        let on_toc = self.screen == Screen::Toc;
        let on_search = self.screen == Screen::Search;
        let on_marks = self.screen == Screen::Marks;
        // 「⋯」里那四件事全是目录树自己的（展开 / 逆向 / 样式 / 清除已读痕迹）。全文那一格
        // 摆着它等于摆四个点了没反应的空位，所以整颗连同样那颗筛标题的放大镜一起收掉。
        let filter = on_toc.then(|| {
            Button::new("toc-search")
                .ghost()
                .small()
                .selected(self.search_open)
                .icon(IconName::Search)
                .tooltip(search_label.clone().to_string())
                .accessibility_label(search_label)
                .on_click(move |_, _, cx| {
                    search_panel.update(cx, |this, cx| {
                        this.search_open = !this.search_open;
                        cx.notify();
                    })
                })
                .into_any_element()
        });
        let more = on_toc.then(|| self.more_button(cx));
        h_flex()
            .w_full()
            .items_center()
            .gap_1()
            .px(px(TOC_GUTTER))
            .h(px(44.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            // 顶上那三格：目录 / 全文 / 批注。定版式（CBZ、逐页 SVG 的漫画）与兜底那一行
            // 没有正文可搜，「全文」那一格于是不摆；「批注」没有这样的前提，永远摆。
            .child(
                Button::new("toc-screen-toc")
                    .ghost()
                    .small()
                    .selected(on_toc)
                    .label(toc_screen_label.to_string())
                    .accessibility_label(toc_screen_label)
                    .on_click(move |_, _, cx| {
                        toc_panel.update(cx, |this, cx| this.set_screen(Screen::Toc, cx))
                    }),
            )
            .when(self.reader.read(cx).searchable().is_some(), |this| {
                this.child(
                    Button::new("toc-screen-search")
                        .ghost()
                        .small()
                        .selected(on_search)
                        .label(fulltext_screen_label.to_string())
                        .accessibility_label(fulltext_screen_label)
                        .on_click(move |_, _, cx| {
                            fulltext_panel
                                .update(cx, |this, cx| this.set_screen(Screen::Search, cx))
                        }),
                )
            })
            // 「批注」那一格三端都在：书签两族（可重排与定版式）都钉得上，批注至少
            // 列得出 —— 不像「全文」那一格要看这本书有没有正文可搜。
            .child(
                Button::new("toc-screen-marks")
                    .ghost()
                    .small()
                    .selected(on_marks)
                    .label(marks_screen_label.to_string())
                    .accessibility_label(marks_screen_label)
                    .on_click(move |_, _, cx| {
                        marks_panel.update(cx, |this, cx| this.set_screen(Screen::Marks, cx))
                    }),
            )
            .children(filter)
            .children(more)
            // 抽屉这一档没有「往下划」这回事，虽然遮罩可点，但抽屉自己的右上角也该有
            // 一颗明确的关闭键：读者在目录树里翻了几屏之后，最顺手的出口就在手边。
            .when(drawer, |this| {
                let close_panel = cx.entity().clone();
                let close_label = tr(cx, "reader.toc.close");
                this.child(
                    Button::new("toc-close")
                        .ghost()
                        .small()
                        .icon(IconName::X)
                        .tooltip(close_label.clone().to_string())
                        .accessibility_label(close_label)
                        .on_click(move |_, _, cx| close_panel.update(cx, |this, cx| this.hide(cx))),
                )
            })
            .into_any_element()
    }

    /// 书头那一块：封面 + 书名 + 作者 + 「读到 X% · 格式 · 大小」。
    ///
    /// 摆在目录树上面，因为读者从目录里认的是「这是哪一本、读到哪儿了」，而那一屏
    /// 原本只有「已读 N / M 章」一句 —— 那个数不带书名，跳完一章回来就不确定还在不在
    /// 同一本书里。
    ///
    /// 查不到书（兜底那一行）时整块不画，而不是画三个空框：那一行本来就没有书可认。
    fn book_head(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(header) = self.header.as_ref() else {
            return div().into_any_element();
        };
        let progress = self.reader.read(cx).progress();
        let row = &header.row;
        let authors: SharedString = row.authors.join(AUTHOR_SEP).into();
        // 那个百分比与底栏状态行、页脚那一条共用 `percent_label`：同一本书在两处念出来
        // 的进度必须是同一个数，两处各写一套小数位迟早对不上。
        let meta: SharedString = tf(
            cx,
            "reader.toc.head_meta",
            &[
                ("percent", &percent_label(progress)),
                ("format", &row.format),
                ("size", &fmt_size(row.size)),
            ],
        )
        .into();
        let muted = cx.theme().muted_foreground;
        let placeholder = cx.theme().muted;
        h_flex()
            .id("reader-toc-head")
            .w_full()
            .items_start()
            .gap_3()
            .px(px(TOC_GUTTER))
            .pt_3()
            .pb_2()
            .child(
                // 封面 48×66dp（参照 App 实测 135×185px @2.8），与架上那一格同一条
                // 2:3 比例；没有图就画一块灰底 —— 旁边那两行写着书名，不会读成坏了。
                div()
                    .flex_none()
                    .w(px(48.0))
                    .h(px(72.0))
                    .overflow_hidden()
                    .rounded(px(2.0))
                    .bg(placeholder)
                    .when_some(header.cover.clone(), |this, image| {
                        this.child(img(image).size_full().object_fit(ObjectFit::Cover))
                    }),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .truncate()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(row.title.clone()),
                    )
                    .when(!authors.is_empty(), |this| {
                        this.child(div().truncate().text_sm().text_color(muted).child(authors))
                    })
                    .child(div().truncate().text_sm().text_color(muted).child(meta)),
            )
            .into_any_element()
    }

    /// 「展开目录 / 折叠目录」：两端来回摆。
    ///
    /// 行数变了不在这儿重造那张尺寸表：渲染那一趟自己比对「行数或行高变了才重建」
    /// （见 [`Self::sheet_element`]），在这儿再写一遍就是同一件事两处做。
    /// 菜单的收场由 `PopupMenu` 自己管（选中一项即 dismiss），这里不再手动收。
    fn toggle_expand_all(&mut self, cx: &mut Context<Self>) {
        if let Some(toc) = self.tree.as_mut() {
            if toc.all_open() {
                toc.collapse_all();
            } else {
                toc.expand_all();
            }
        }
        cx.notify();
    }

    /// 正着摆 / 倒着摆。
    fn toggle_reversed(&mut self, cx: &mut Context<Self>) {
        if let Some(toc) = self.tree.as_mut() {
            toc.set_reversed(!toc.reversed());
        }
        cx.notify();
    }

    /// 抹掉「来过」的那份痕迹：灰字是读者自己翻出来的，他要一次清干净。
    fn clear_read_marks(&mut self, cx: &mut Context<Self>) {
        self.reader
            .update(cx, |reader, cx| reader.clear_read_marks(cx));
        cx.notify();
    }

    /// 目录那一格的那行框：按**标题**筛目录条。
    fn filter_row(&self, input: &Entity<InputState>, cx: &mut Context<Self>) -> AnyElement {
        div()
            .w_full()
            .px(px(TOC_GUTTER))
            .pb_2()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .child(
                        Icon::new(IconName::Search)
                            .small()
                            .text_color(cx.theme().muted_foreground),
                    )
                    // 没有占位文案可给：`InputState` 与 `Input` 都不收 placeholder
                    // （gpui-kit 0.7.1 只有数字输入框有那一项）。这一行只在点了搜索
                    // 之后出现，旁边那颗放大镜就是它自己的说明。
                    .child(Input::new(input).flex_1().min_w_0()),
            )
            .into_any_element()
    }

    /// 全文那一格的那行框：用共用的那颗 [`SearchBox`]（清空按钮、「点框外面就退编辑态」、
    /// 软键盘那唯一一条收路都在它自己身上 —— 见 `ui/components/search_box.rs`）。
    fn fulltext_row(&self, boxed: &Entity<SearchBox>, _cx: &mut Context<Self>) -> AnyElement {
        div()
            .w_full()
            .px(px(TOC_GUTTER))
            .pb_2()
            .child(div().w_full().child(boxed.clone()))
            .into_any_element()
    }

    /// 全文那一格的列表。空态三句实话，各说各的：还没写词、还在扫、扫完了没有。
    fn hit_list(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.list_len == 0 {
            let line: SharedString = if self.search.running {
                tf(
                    cx,
                    "reader.search.scanning",
                    &[
                        ("done", &self.search.scanned),
                        ("total", &self.search.chapters),
                    ],
                )
                .into()
            } else if self.search.chapters == 0 {
                // 还没写过任何词：这一屏要说的不是「没有」，而是「要做什么」。
                tr(cx, "reader.search.hint")
            } else {
                tr(cx, "reader.search.none")
            };
            return div()
                .w_full()
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .px(px(TOC_GUTTER))
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(line)
                .into_any_element();
        }
        let view = cx.entity().clone();
        div()
            .flex_1()
            .min_h_0()
            .pb(px(TOC_BOTTOM_SPACE))
            .child(
                v_virtual_list(
                    view,
                    "reader-fulltext",
                    self.sizes.clone(),
                    |panel, range, _, cx| panel.hit_rows(range, cx),
                )
                // 句柄跨帧持有，理由与目录那条一模一样：不顶掉它就每帧归零，列表滑不动。
                .track_scroll(&self.search_scroll),
            )
            .into_any_element()
    }

    /// `[range.start, range.end)` 那几处命中：上面一行章名，下面一行那句上下文，
    /// 命中的那几个字单独着色。
    fn hit_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let end = range.end.min(self.search.hits.len());
        if range.start >= end {
            return Vec::new();
        }
        let reader = self.reader.clone();
        let (foreground, muted, mark_color) = (
            cx.theme().foreground,
            cx.theme().muted_foreground,
            cx.theme().primary,
        );
        let panel = cx.entity().clone();
        self.search
            .hits
            .iter()
            .enumerate()
            .skip(range.start)
            .take(end - range.start)
            .map(|(ix, hit)| {
                // 那一行上面那句「这是哪儿的字」由会话给：可重排是章名（会话手里那份章节表
                // 才是权威，命中自己不带标题，免得同一章在目录那一屏与这一屏念出两个名字），
                // 定版式是「第几页 / 共几页」—— 那一族没有章表，念章名会念出一个空摆位。
                let title = reader.read(cx).hit_place_at(hit.chapter, cx);
                let pre: String = hit.snippet.chars().take(hit.mark.start).collect();
                let mid: String = hit
                    .snippet
                    .chars()
                    .skip(hit.mark.start)
                    .take(hit.mark.end - hit.mark.start)
                    .collect();
                let post: String = hit.snippet.chars().skip(hit.mark.end).collect();
                let chapter = hit.chapter;
                let at = hit.start;
                let panel_jump = panel.clone();
                div()
                    .id(ElementId::named_usize("reader-fulltext-row", ix))
                    .w_full()
                    .h(px(SEARCH_ROW))
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap_1()
                    .px(px(TOC_GUTTER))
                    .overflow_hidden()
                    .child(
                        div()
                            .w_full()
                            .text_xs()
                            .text_color(muted)
                            .truncate()
                            .child(title),
                    )
                    .child(
                        // 三段摆在同一条横排里：字号与颜色从这条继承，只有中间那段自己改。
                        // 读者一眼要认的是「哪几个字是搜到的」，上下文只是它的邻居。
                        h_flex()
                            .w_full()
                            .overflow_hidden()
                            .items_center()
                            .text_sm()
                            .text_color(foreground)
                            .child(div().flex_none().child(pre))
                            .child(
                                div()
                                    .flex_none()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(mark_color)
                                    .child(mid),
                            )
                            .child(div().flex_1().min_w_0().overflow_hidden().child(post)),
                    )
                    .on_click(move |_, _, cx| {
                        panel_jump.update(cx, |this, cx| this.jump_to_hit(chapter, at, cx))
                    })
                    .into_any_element()
            })
            .collect()
    }

    /// 点一处命中：跳到那一章的那几个字上，并把这一层整个收掉（读者要看的正是那几行字）。
    ///
    /// 落点交给会话那一句 `goto_offset` —— 与「定位原文」共用同一条路：目标章多半还没排过版，
    /// 「第几个字落在哪一像素」要等排版做完那一帧才落地。
    fn jump_to_hit(&mut self, chapter: usize, offset: usize, cx: &mut Context<Self>) {
        self.reader
            .update(cx, |reader, cx| reader.goto_offset(chapter, offset, cx));
        self.sheet.update(cx, |sheet, cx| sheet.hide(cx));
        self.tree = None;
        self.search_open = false;
        // 顶栏底栏与排版面板都在阅读页手里，收干净由它来（见 [`TocEvent::Navigated`]）。
        cx.emit(TocEvent::Navigated);
        cx.notify();
    }

    /// 提交一趟扫描。**同一时刻只允许一趟在跑**：新的查询先作废上一趟，而不是排队。
    ///
    /// 判据与 `plans/阅读管线.md` §10.3 合并那一档同一条：读者连打五个字会产出五趟请求，
    /// 排队的结果是列表停在第一个字的那两百处上，而那早已不是他要看的了。让在跑的那趟自己
    /// 看见旗标收手（协作式取消，不在半章中间打断），列表由回来的那一趟整个换掉。
    fn submit(&mut self, raw: &str, cx: &mut Context<Self>) {
        self.search.reset();
        let (Some(query), Some(doc)) = (Query::parse(raw), self.reader.read(cx).searchable())
        else {
            // 没词、或这一屏根本没有正文可搜（定版式与兜底那一行）：不跑那一趟注定为空的。
            cx.notify();
            return;
        };
        self.search.running = true;
        cx.notify();

        // 取正文、折可见文本、找命中全在作业线程上（最坏实测 274 ms，那张表在
        // `folio_core::search` 的模块头）；界面线程一帧都不该为它停。
        let handle: TaskHandle<Found> = resident::pool()
            .submit_with(Priority::Interactive, move |ctx| {
                Ok(scan(&*doc, &query, &Options::default(), ctx))
            });
        self.search.cancel = Some(handle.cancel.clone());

        // `std::sync::mpsc` 的接收端只会阻塞读它的那条线程，而界面线程一阻塞整个窗口就停：
        // 所以把读的那一手搬到后台，每条事件转成一次界面更新 —— 与架面那条导入链同一条形状。
        let (forward, events) = async_channel::unbounded::<TaskEvent<Found>>();
        cx.background_spawn(async move {
            // 只有「接收端已经没了」才会报错：这一屏收了、或读者又改了词。
            for event in handle.events.iter() {
                if forward.send_blocking(event).is_err() {
                    break;
                }
            }
        })
        .detach();
        self.search.stream = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            while let Ok(event) = events.recv().await {
                if this
                    .update(cx, |this, cx| this.search_event(event, cx))
                    .is_err()
                {
                    return;
                }
            }
            // 通道关了而结果永远不会来（后台那条链出事了）：不能让「搜集中」永远挂着。
            let _ = this.update(cx, |this, cx| {
                this.search.running = false;
                cx.notify();
            });
        }));
    }

    /// 一趟扫描流回来的读数落进这一屏。
    fn search_event(&mut self, event: TaskEvent<Found>, cx: &mut Context<Self>) {
        match event {
            TaskEvent::Started | TaskEvent::Bytes(_) => {}
            TaskEvent::Progress(progress) => {
                // 进度只认「扫到第几章」那一格（那趟扫描只发这一种读数）。
                self.search.scanned = progress.done as usize;
                if progress.total > 0 {
                    self.search.chapters = progress.total as usize;
                }
                cx.notify();
            }
            TaskEvent::Finished(Ok(found)) => {
                self.search.hits = found.hits;
                self.search.scanned = found.scanned;
                self.search.chapters = found.chapters;
                self.search.capped = found.truncated;
                self.search.running = false;
                self.search.cancel = None;
                cx.notify();
            }
            TaskEvent::Finished(Err(error)) => {
                // 作业线程 panic，或池已经关掉。这一屏回落到「没有」而不是「还在搜」：
                // 挂着一个永远不结束的进度比一句实话更糟。
                tracing::warn!(error = %error, "全文检索那一趟没跑完");
                self.search.running = false;
                self.search.cancel = None;
                cx.notify();
            }
            TaskEvent::Cancelled => {
                self.search.running = false;
                self.search.cancel = None;
                cx.notify();
            }
        }
    }

    /// 顶上那两格：目录树 / 正文全文。
    fn set_screen(&mut self, screen: Screen, cx: &mut Context<Self>) {
        if self.screen == screen {
            return;
        }
        if screen == Screen::Search {
            self.focus_wanted = true;
        } else {
            // 离开全文那一格：那一趟没有要的人了，别让它读着一本没人看的正文跑完。
            self.search.cancel_inflight();
        }
        self.screen = screen;
        cx.notify();
    }

    /// 列表本体。没有目录的书（TXT / MD / FB2 与定版式）在这里给一句实话，
    /// 而不是摆一屏空白让读者以为是排版没做完。
    ///
    /// 外面那层 `flex_1` 是必需的：`v_virtual_list` 建成 `size_full`，若让它直接做弹层
    /// 内容那一格的兄弟，它会按**整格高**撑开、比剩下可用高多出书头那一段 —— 多出来的
    /// 部分被裁在格底之下，滚到最后一条时那一行就看不见了（用户报的「滑到底被遮挡」）。
    /// 包一层 `flex_1` 把「剩下多少高」交给它，列表底边才与内容底边对齐；底部再留出
    /// [`TOC_BOTTOM_SPACE`]，最后一行不贴着弹层下沿。
    fn tree_list(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.list_len == 0 {
            return div()
                .w_full()
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(tr(cx, "reader.toc.empty"))
                .into_any_element();
        }
        let view = cx.entity().clone();
        div()
            .flex_1()
            .min_h_0()
            .pb(px(TOC_BOTTOM_SPACE))
            .child(
                v_virtual_list(
                    view,
                    "reader-toc",
                    self.sizes.clone(),
                    |panel, range, _, cx| panel.rows(range, cx),
                )
                // 句柄跨帧持有（见字段 `toc_scroll`）：不顶掉 `v_virtual_list` 每帧新建的
                // 那一份，滚动偏移每帧归零，目录就滑不动。
                .track_scroll(&self.scroll),
            )
            .into_any_element()
    }

    /// `[range.start, range.end)` 这几行目录。
    ///
    /// 三个信号各管各的，不互相顶：当前行 = 底色 + 左边那条标记；来过 = 文字变灰
    /// （设置里可以关掉）；对不上正文的条目 = 灰到底且**没有**点击回调。
    fn rows(&mut self, range: std::ops::Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(toc) = &self.tree else {
            return Vec::new();
        };
        let rows = toc.visible();
        let end = range.end.min(rows.len());
        if range.start >= end {
            return Vec::new();
        }
        let prefs = ConfigHolder::reader(cx);
        let row_height = prefs.toc_density.row_height();
        let lines = prefs.toc_max_title_lines as usize;
        let current = self.reader.read(cx).chapter_index();
        let read = self.reader.read(cx).read_chapters().clone();
        let (foreground, muted, surface) = (
            cx.theme().foreground,
            cx.theme().muted_foreground,
            cx.theme().muted,
        );
        let transparent = hsla(0.0, 0.0, 0.0, 0.0);
        let panel = cx.entity().clone();
        rows[range.start..end]
            .iter()
            .map(|row| {
                let ix = row.ix;
                let active = row.chapter == Some(current);
                let visited = prefs.toc_dim_read
                    && row.chapter.is_some_and(|chapter| read.contains(&chapter));
                // 当前行不跟着「来过」变灰：它已经吃了底色与标记条，再灰一层就变成
                // 「看不出现在在哪」，而那正是这一屏唯一必须答对的问题。
                let color = if active || !visited {
                    foreground
                } else {
                    muted
                };
                let chevron = if row.branch {
                    let panel_toggle = panel.clone();
                    Button::new(ElementId::named_usize("reader-toc-toggle", ix))
                        .ghost()
                        .small()
                        .w(px(TOC_CHEVRON))
                        .p_0()
                        .justify_center()
                        .icon(if toc.is_collapsed(ix) {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        })
                        .on_click(move |_, _, cx| {
                            // 不拦的话这一击会接着冒到整行上：读者点的是「展开」，
                            // 结果书也跳了、面板也收了，看起来像这颗箭头是坏的。
                            cx.stop_propagation();
                            panel_toggle.update(cx, |this, cx| {
                                if let Some(toc) = &mut this.tree {
                                    toc.toggle(ix);
                                }
                                cx.notify();
                            })
                        })
                        .into_any_element()
                } else {
                    // 叶子占同样一格，标题才不会因为有没有下级而左右跳。
                    div().w(px(TOC_CHEVRON)).flex_none().into_any_element()
                };
                // 三段：一点空隙 → 标记条 → 底色带。带子从标记条右边起、铺到屏幕右沿，
                // 标题落在 `TOC_TEXT_LEFT` 那一列上（chevron 那一格在它左边）。
                let mut row_el = div()
                    .id(ElementId::named_usize("reader-toc-row", ix))
                    .w_full()
                    .h(px(row_height))
                    .flex()
                    .items_stretch()
                    .child(
                        div()
                            .flex_none()
                            .w(px(TOC_MARKER))
                            .ml(px(TOC_MARKER_INSET))
                            .bg(if active { foreground } else { transparent }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .overflow_hidden()
                            .pl(px(TOC_TEXT_LEFT - TOC_BAND_LEFT - TOC_CHEVRON
                                + (row.depth as f32) * TOC_INDENT))
                            .pr(px(TOC_GUTTER))
                            .when(active, |el| el.bg(surface))
                            .child(chevron)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .line_clamp(lines)
                                    .when(active, |el| el.font_weight(FontWeight::SEMIBOLD))
                                    .text_color(color)
                                    .child(row.title.clone()),
                            ),
                    );
                // 对不上正文的那几条不挂回调：灰着且点不动，比「点了什么也没发生」诚实。
                if let Some(chapter) = row.chapter {
                    let fragment = row.fragment.clone();
                    let panel_jump = panel.clone();
                    row_el = row_el.on_click(move |_, _, cx| {
                        panel_jump.update(cx, |this, cx| {
                            this.jump_to_entry(chapter, fragment.clone(), cx)
                        })
                    });
                }
                row_el.into_any_element()
            })
            .collect()
    }

    /// 跳到目录里的某一条，并把这一层整个收掉：读者要看的正是跳到的那几行字。
    fn jump_to_entry(&mut self, chapter: usize, fragment: Option<String>, cx: &mut Context<Self>) {
        self.reader
            .update(cx, |reader, cx| reader.goto_entry(chapter, fragment, cx));
        // 跳到哪一条就把目录整个收掉：读者要看的正是跳到的那几行字。
        self.sheet.update(cx, |sheet, cx| sheet.hide(cx));
        self.tree = None;
        self.search_open = false;
        // 顶栏底栏与排版面板都在阅读页手里，收干净由它来（见 [`TocEvent::Navigated`]）。
        cx.emit(TocEvent::Navigated);
        cx.notify();
    }

    /// 「目录样式」那一下：设置那一屏要压进导航栈，而栈长在阅读页手里 —— 这里只把意愿报出去。
    fn settings_wanted(&mut self, cx: &mut Context<Self>) {
        cx.emit(TocEvent::OpenSettings);
    }
}

impl Render for TocPanel {
    /// 同一份状态、两种表面：手机那一档是压在正文之上的底部弹层，平板与桌面那一档是
    /// 左边一张带遮罩的抽屉（正文一寸不让）。哪些行看得见、折了哪几枝、搜的是什么词、
    /// 跳到哪一条 —— 全部只有一份，两种摆法读的是同一个 `TocView`。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if SizeClass::of(window).is_compact() {
            // 弹层**每帧都构造**，收着时也构造 —— 弹簧的采样在里面，只在开着时构造会把
            // 退场动画连状态一起丢（见 `widgets::bottom_sheet` 那段模块注释）。
            self.sheet_element(window, cx)
        } else {
            self.drawer_element(window, cx)
        }
    }
}

/// 这一屏与阅读页之间那两道通知。订阅方是 [`crate::ui::features::reader::view::body::reading_page::ReadingPage`]（见它的 `_watches`）。
impl EventEmitter<TocEvent> for TocPanel {}

/// 一句原文在列表里显多长。`exact` 是可见文本、可能带着排版换行，先折空白再截 ——
/// 与导出那条 `collapse` 同一件事的界面侧版本，太长的那半本来就被行 truncate 掉了。
fn snippet(exact: &str) -> String {
    let folded: String = exact.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = folded.chars().take(48).collect();
    if folded.chars().count() > 48 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    //! 目录那一屏搬进自己的实体之后，这几条钉的是**接线**：`toggle` 开得起这一层、画一帧
    //! 整屏不炸（树、书头、尺寸表、虚拟列表都在这条路上）、点一条能把跳转派给会话并把
    //! 「浮层该收了」报回阅读页。摆出来的样子 —— 刻度、缩进、颜色 —— 仍然只有真机量得出来。
    //!
    //! 全文那一档那几条钉的是**同一件事的另一格**：换格换的是列表与读数（尺寸表因此要跟着
    //! 换档）、一处命中的落点必须带上那个**偏移**而不是只跳章、读者改了词要把上一趟作废、
    //! 空词与收起这一层都不该留一趟没人要的扫描在跑。
    //!
    //! 这几条都不等作业线程：`submit` 往池里丢的那几趟在测试里没人泵（`cx.spawn` 那一条读
    //! 事件的链要测试执行器转起来才会动），所以断言只看**同步**能定下的事 —— 旗标置位没有、
    //! running 翻没翻、尺寸表给哪一档造。回来的那一趟真正落进列表的形状，由
    //! `search_event` 手动喂一条 `Finished` 来验，与真线程的快慢无关。
    //!
    //! 点名导入而不 `use super::*`：`gpui_kit` 根把 `gpui` 整个转了出来，里面有一个
    //! 叫 `test` 的属性宏，glob 会让下面的 `#[test]` 递归展开成它。

    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::time::Instant;

    use folio_core::content::document::Document;
    use folio_core::content::format::TocEntry;
    use folio_core::infra::task::TaskEvent;
    use folio_core::search::{Found, Hit};
    use folio_core::{Chapter, Config, Content, DocMeta, FlowDoc, Format, Locator, Reader};
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Entity, InputEvent as _, ScrollDelta, ScrollWheelEvent,
        Subscription, TestAppContext, point, px, size,
    };

    use super::{SEARCH_ROW, Screen, TOC_DRAWER, TOC_DRAWER_INSET, TocEvent, TocPanel};
    use crate::ui::features::reader::model::session::ReaderSession;
    use crate::ui::services::config::ConfigHolder;

    /// 一套开好了的「目录这一屏」：会话、这一屏、窗口句柄、收到的事件，以及那条订阅自己。
    ///
    /// 起别名而不写在这里展开：五层嵌套的元组签名，clippy 与读的人都要费劲。
    type Opened = (
        Entity<ReaderSession>,
        Entity<TocPanel>,
        AnyWindowHandle,
        Rc<RefCell<Vec<TocEvent>>>,
        Subscription,
    );

    /// 一本两章、每章各挂着一条目录的书，加一扇把**这一屏**摆在根上的窗。
    ///
    /// 根就是这一屏：`v_virtual_list` 与弹层都要真窗口，而这一屏的渲染路径正是这里要测的那条。
    /// 最后那格是订阅句柄 —— `Subscription` 一 drop 就退订，测试必须把它攥着。
    fn open_panel(cx: &mut TestAppContext, width: f32) -> Opened {
        let toc = vec![
            TocEntry {
                title: "第一章".into(),
                href: "c0.xhtml".into(),
                ..Default::default()
            },
            TocEntry {
                title: "第二章".into(),
                href: "c1.xhtml".into(),
                ..Default::default()
            },
        ];
        open_panel_with(cx, width, toc)
    }

    /// 同一套起法，目录换成递进来的那一棵：滚动仪表要的是 1400 行那一档，断言要的是两行。
    fn open_panel_with(cx: &mut TestAppContext, width: f32, toc: Vec<TocEntry>) -> Opened {
        let flow = FlowDoc {
            chapters: vec![
                Chapter::inline("第一章", "<p>第一章的第一段字。</p>")
                    .with_spine_href("c0.xhtml".to_string()),
                Chapter::inline("第二章", "<p>第二章的第一段字。</p>")
                    .with_spine_href("c1.xhtml".to_string()),
            ],
            toc,
            ..Default::default()
        };
        let meta = DocMeta {
            title: "测试书".into(),
            authors: Vec::new(),
            format: Format::Markdown,
            size: 0,
            ..DocMeta::default()
        };
        let doc: Arc<dyn Document> = Arc::from(folio_core::content::document::from_content(
            meta,
            Content::Reflow(flow),
        ));
        let events: Rc<RefCell<Vec<TocEvent>>> = Rc::new(RefCell::new(Vec::new()));
        let mut built = None;
        let mut watch = None;
        let handle = cx.open_window(size(px(width), px(520.)), |window, cx| {
            ConfigHolder::install(Config::default(), cx);
            let session = cx.new(|cx| {
                ReaderSession::with_document(
                    doc,
                    Reader::default(),
                    None,
                    None,
                    Locator::start(),
                    cx,
                )
            });
            let panel = cx.new(|cx| TocPanel::new(session.clone(), cx));
            let sink = events.clone();
            // 句柄必须留着：`Subscription` 一 drop 就退订，事件那条路就此不通。
            watch = Some(cx.subscribe(&panel, move |_, _, event: &TocEvent, _| {
                sink.borrow_mut().push(*event)
            }));
            let root_view = panel.clone();
            built = Some((session, panel));
            Root::new(root_view, window, cx)
        });
        let (session, panel) = built.expect("会话与目录那一屏都该建起来");
        (
            session,
            panel,
            handle.into(),
            events,
            watch.expect("订阅句柄该在开窗时挂上"),
        )
    }

    /// 打开这一屏：弹层开着，两条目录都摆得出来。
    #[gpui_kit::test]
    fn the_panel_opens_and_draws_its_rows(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (_session, panel, handle, _events, _watch) = open_panel(cx, 460.);
        panel.update(cx, |panel, cx| panel.toggle(cx));
        assert!(
            panel.read_with(cx, |panel, cx| panel.is_open(cx)),
            "toggle 该把这块底部弹层开起来"
        );
        // 画一帧：树、书头、尺寸表与虚拟列表都在这条路上。挪文件时漏掉任何一根线，
        // 红要红在这里，而不是等读者在真机上看到一屏空白。
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("这一屏该画得出来");
        let rows = panel.update(cx, |panel, cx| panel.rows(0..4, cx));
        assert_eq!(rows.len(), 2, "两条目录都该摆出来");
    }

    /// 宽档上同一份状态换的是**容器**，不是内容：目录是一张压在正文上的抽屉，
    /// 不再占正文的宽。列表里摆的还是那两行。
    ///
    /// 这一条能挡住的错法：把抽屉改回「占位栏」之后，正文会让宽、顶栏也会被挤到抽屉
    /// 右边 —— 读者报的正是「一开目录，返回栏被挤走」。抽屉宽必须窄于窗口，才说明它
    /// 是浮在正文上的一层而不是把窗口一分为二。
    #[gpui_kit::test]
    fn the_wide_screen_uses_a_drawer_overlay(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (_session, panel, handle, _events, _watch) = open_panel(cx, 900.);
        assert!(
            !panel.read_with(cx, |panel, cx| panel.is_open(cx)),
            "没 toggle 之前不该开着"
        );
        panel.update(cx, |panel, cx| panel.toggle(cx));
        assert!(
            panel.read_with(cx, |panel, cx| panel.is_open(cx)),
            "toggle 该把宽档抽屉开起来"
        );
        let window_width = cx
            .update_window(handle, |_, window, _| window.viewport_size().width)
            .expect("窗口应可读到");
        assert!(
            TOC_DRAWER + TOC_DRAWER_INSET * 2.0 < f32::from(window_width),
            "抽屉宽超过窗口就不再是浮层了"
        );
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("宽档抽屉那一屏该画得出来");
        let rows = panel.update(cx, |panel, cx| panel.rows(0..4, cx));
        assert_eq!(rows.len(), 2, "两种表面读的是同一棵树");
    }

    /// 宽档抽屉外面那一片是遮罩：点一下就该收。抽屉自己的点击不能顺手把这一层关掉。
    ///
    /// 这一条钉的是「遮罩在上面」那件事的退场：抽屉只盖左边一条，右边的正文仍然可见，
    /// 但那一击必须先归遮罩，而不是穿到底下的正文去。
    #[gpui_kit::test]
    fn tapping_the_scrim_closes_the_wide_drawer(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (_session, panel, handle, _events, _watch) = open_panel(cx, 900.);
        panel.update(cx, |panel, cx| panel.toggle(cx));
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("宽档抽屉那一屏该画得出来");

        // 抽屉右边、窗口以内：落点归遮罩。
        let mut visual = gpui_kit::VisualTestContext::from_window(handle, cx);
        visual.simulate_click(point(px(700.0), px(300.0)), Default::default());
        assert!(
            !panel.read_with(cx, |panel, cx| panel.is_open(cx)),
            "点遮罩该把抽屉收起来"
        );
    }

    /// 点一条目录：跳章派给会话，「浮层该收了」报回阅读页，这一层自己收掉。
    ///
    /// 那两条报事（`Navigated` / `OpenSettings`）是这一屏与阅读页之间**唯一**的口子，
    /// 事件没接上的症状是「跳完字了、顶栏底栏还压在上面」。
    #[gpui_kit::test]
    fn a_jump_goes_to_the_session_and_reports_back(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (session, panel, handle, events, _watch) = open_panel(cx, 460.);
        panel.update(cx, |panel, cx| panel.toggle(cx));
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("这一屏该画得出来");

        panel.update(cx, |panel, cx| panel.jump_to_entry(1, None, cx));

        assert_eq!(
            session.read_with(cx, |v, _| v.chapter_index()),
            1,
            "会话该落到第二章"
        );
        assert_eq!(
            events.borrow().as_slice(),
            &[TocEvent::Navigated],
            "跳转要报给阅读页，它才好把顶栏底栏与排版面板收干净"
        );
        assert!(
            !panel.read_with(cx, |panel, cx| panel.is_open(cx)),
            "跳完该把这一层收掉：读者要看的正是跳到的那几行字"
        );
    }

    /// 顶上那一格换到「全文」：换的是列表与那张尺寸表，不是容器，也不是书头。
    ///
    /// 这一条挡两种错法：一是尺寸表只比行数与行高 —— 两档都只有一条空态时行数恰好相等，
    /// 于是全文那一格拿到目录那档的行高（症状是列表高度不对）；二是换格时代码去新建了
    /// 一块弹层/抽屉（读者看到的是整屏跳一下）。
    #[gpui_kit::test]
    fn the_fulltext_cell_swaps_the_list_not_the_container(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (_session, panel, handle, _events, _watch) = open_panel(cx, 460.);
        panel.update(cx, |panel, cx| panel.toggle(cx));
        panel.update(cx, |panel, cx| panel.set_screen(Screen::Search, cx));
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("全文那一格该画得出来");
        panel.read_with(cx, |panel, _| {
            assert_eq!(panel.screen, Screen::Search);
            assert_eq!(panel.sized_on, Screen::Search, "尺寸表该跟着换一档");
            assert_eq!(panel.row_height, SEARCH_ROW, "两档的行高不是一回事");
        });
        // 还没写词：一行命中都没有，而列表那一格摆的是「要做什么」那句实话（空态在 `hit_list`）。
        assert!(
            panel
                .update(cx, |panel, cx| panel.hit_rows(0..8, cx))
                .is_empty()
        );
        // 容器没换：这一层还开着，弹层句柄还是那一个（换了容器就会看见整屏跳）。
        assert!(panel.read_with(cx, |panel, cx| panel.is_open(cx)));
    }

    /// 点一处命中：会话收到的落点是**那一章的第几个字**，而不是那一章的开头。
    ///
    /// 这一条是全文检索在产品上成不成立的判据：只跳章的话，搜「孙悟空」跳回那一章第一行，
    /// 读者还得自己找 —— 那与没有搜索只差一句「请上下滑动」。目录那一格跳章首是对的
    /// （见上面那条），全文这一格跳章首就是错的，两格各自把落点交出去。
    #[gpui_kit::test]
    fn a_hit_carries_its_offset_into_the_session(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (session, panel, handle, events, _watch) = open_panel(cx, 460.);
        panel.update(cx, |panel, cx| {
            panel.toggle(cx);
            panel.set_screen(Screen::Search, cx);
        });
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("这一格该画得出来");

        // 喂一条回来的结果（不等作业线程：见本模块那份说明）。夹具的第二章正文是
        // 「第二章的第一段字。」，命中记在第 3 个字上。
        let found = Found {
            hits: vec![Hit {
                chapter: 1,
                start: 3,
                end: 6,
                snippet: "第二章的第一段字。".into(),
                mark: 3..6,
            }],
            chapters: 2,
            scanned: 2,
            truncated: false,
            cancelled: false,
        };
        panel.update(cx, |panel, cx| {
            panel.search_event(TaskEvent::Finished(Ok(found)), cx)
        });
        assert_eq!(
            panel.update(cx, |panel, cx| panel.hit_rows(0..4, cx)).len(),
            1,
            "一处命中该摆出一行"
        );

        panel.update(cx, |panel, cx| panel.jump_to_hit(1, 3, cx));
        assert_eq!(
            session.read_with(cx, |v, _| v.chapter_index()),
            1,
            "会话该落到第二章"
        );
        assert_eq!(
            session.read_with(cx, |v, _| v.pending_restore),
            Some(Locator {
                chapter: 1,
                offset: 3
            }),
            "挂着的落位请求要带那个偏移；只写章号等于把读者送回章首"
        );
        assert_eq!(
            events.borrow().as_slice(),
            &[TocEvent::Navigated],
            "跳完要把「浮层该收了」报回阅读页"
        );
        assert!(
            !panel.read_with(cx, |panel, cx| panel.is_open(cx)),
            "跳完该把这一层收掉"
        );
    }

    /// 读者改一个词，上一趟就该作废 —— 而不是排在后面、把列表拖回他要的那个词之前。
    ///
    /// 判据取自 `plans/阅读管线.md` §10.3 合并那一档：只丢请求、让在跑的那趟自己收手。
    /// 这里断的是**同步**那一半（旗标置位、新的一趟接手），回来的结果由上一条验。
    #[gpui_kit::test]
    fn a_newer_query_voids_the_scan_in_flight(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (_session, panel, _handle, _events, _watch) = open_panel(cx, 460.);
        panel.update(cx, |panel, cx| {
            panel.toggle(cx);
            panel.set_screen(Screen::Search, cx);
        });
        panel.update(cx, |panel, cx| panel.submit("第一章的", cx));
        let first = panel
            .read_with(cx, |panel, _| panel.search.cancel.clone())
            .expect("前置：第一趟该在跑");
        assert!(
            panel.read_with(cx, |panel, _| panel.search.running),
            "有词就该有一趟在跑"
        );

        panel.update(cx, |panel, cx| panel.submit("第二章的", cx));
        assert!(
            first.triggered(),
            "读者改了词，上一趟该被作废而不是排在后面"
        );

        // 空词（清空那颗框、或全删）：不跑那一趟注定为空的，读数也一并清掉。
        panel.update(cx, |panel, cx| panel.submit("   ", cx));
        panel.read_with(cx, |panel, _| {
            assert!(!panel.search.running, "没词就没有在跑的那趟");
            assert!(panel.search.cancel.is_none());
            assert!(
                panel.search.hits.is_empty() && panel.search.chapters == 0,
                "读数清干净"
            );
        });
    }

    /// 这一层收了，那一趟就该作废：没人看的正文不该继续读完整本书。
    ///
    /// 最坏那一本现读现扫实测 274 ms（那张表在 `folio_core::search` 的模块头），而读者收掉
    /// 这一层之后一秒都不会再看它。
    #[gpui_kit::test]
    fn closing_the_layer_voids_the_scan_in_flight(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (_session, panel, _handle, _events, _watch) = open_panel(cx, 460.);
        panel.update(cx, |panel, cx| {
            panel.toggle(cx);
            panel.set_screen(Screen::Search, cx);
        });
        panel.update(cx, |panel, cx| panel.submit("第一章的", cx));
        let running = panel
            .read_with(cx, |panel, _| panel.search.cancel.clone())
            .expect("前置：那一趟该在跑");
        panel.update(cx, |panel, cx| panel.hide(cx));
        assert!(running.triggered(), "收了这一层就不该让那趟把书读完");
        assert!(
            !panel.read_with(cx, |panel, _| panel.search.running),
            "读数也要跟着落到「没在跑」"
        );
    }

    /// 那一趟没跑出结果的两条收尾：报错与被取消。
    ///
    /// 两条都该把「搜集中」落下 —— 挂着一个永远不结束的进度比一句「没有」更糟：读者会一直
    /// 等一个不会来的列表。
    #[gpui_kit::test]
    fn a_scan_that_never_returns_does_not_leave_the_progress_hanging(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (_session, panel, _handle, _events, _watch) = open_panel(cx, 460.);
        panel.update(cx, |panel, cx| {
            panel.toggle(cx);
            panel.set_screen(Screen::Search, cx);
            panel.submit("第一章的", cx);
        });
        assert!(panel.read_with(cx, |panel, _| panel.search.running));
        // `TaskError` 就是一个装箱的错误对象（`infra::task.rs`），这里不必是真的那一种。
        let error: folio_core::infra::task::TaskError = "作业池已经关掉".into();
        panel.update(cx, |panel, cx| {
            panel.search_event(TaskEvent::Finished(Err(error)), cx);
        });
        panel.read_with(cx, |panel, _| {
            assert!(!panel.search.running, "报错那一趟不该还在跑");
            assert!(panel.search.cancel.is_none(), "旗标跟着放掉");
            assert!(panel.search.hits.is_empty());
        });

        panel.update(cx, |panel, cx| panel.submit("第二章的", cx));
        panel.update(cx, |panel, cx| {
            panel.search_event(TaskEvent::Cancelled, cx);
        });
        assert!(
            !panel.read_with(cx, |panel, _| panel.search.running),
            "被取消的那一趟也不该留下一个永远转的进度"
        );
    }

    /// **目录滚动仪表**（`#[ignore]`：量的是墙钟，不当断言用）。
    ///
    /// 为什么要这一条：滑动卡顿的判据是「一帧的单价」，而这一屏每帧跑的那几趟全表只住在
    /// 渲染路径里，别处量不到 —— `pieces` 里 `visible()` 一趟只为拿一个行数、`rows` 里
    /// 再一次。这里把**整帧**与**那一趟**分开报，改哪一处值多少钱就有数了。
    /// 表头「已读 N / M」那两个数顺带钉住是 O(1)（`read_count` 就是 `ix + 1`），
    /// 不在每帧的账上。
    ///
    /// 三个读数只在**同一台机器、同一个 profile** 下互相比，别把绝对值当手机上的数：
    /// debug 的 CPU 段比 release 虚高数倍，所以要 `--release`。已读集合这一项在本台夹具里
    /// 只有 1 条，克隆费量不出真书（上千条已读）的那一笔。
    ///
    /// 量的是**宽档抽屉那一档**（`SizeClass::of` 在桌面上把宽度夹到至少 Medium，460 也走抽屉）：
    /// 容器与弹层不同，但那四块内容、虚拟列表与每帧的全表遍历是同一份（见 `Self::pieces`），
    /// 所以这几趟的单价可以直接搬到手机那一档；弹层多出来的拖拽与抽屉的遮罩不在这一格里。
    ///
    /// 跑法：`cargo test -p folio-app --lib --release -- --ignored --nocapture measure`
    #[ignore = "量墙钟，只在读数字时手动跑"]
    #[gpui_kit::test]
    fn measure_a_toc_scroll_frame(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        // 200 组 × 每组 6 条 = 1400 行，基督山伯爵那一档的分量；全展开是 worst case
        // （折叠着的时候 `visible()` 走得快，而读者滑目录正是展开着滑）。
        const GROUPS: usize = 200;
        const PER_GROUP: usize = 6;
        const ROWS: usize = GROUPS * (PER_GROUP + 1);
        const FRAMES: usize = 60;

        let toc = (0..GROUPS)
            .map(|group| TocEntry {
                title: format!("第{group}组"),
                href: "c0.xhtml".into(),
                children: (0..PER_GROUP)
                    .map(|ix| TocEntry {
                        // 标题按真书那样长：一行放不下，截断与「最多几行」的活儿才落得下来。
                        title: format!("第{group}组第{ix}节 · 一个足够长的标题用来把一行撑满"),
                        href: "c0.xhtml".into(),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            })
            .collect();

        let (_session, panel, handle, _events, _watch) = open_panel_with(cx, 460., toc);
        panel.update(cx, |panel, cx| panel.toggle(cx));
        for _ in 0..3 {
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .expect("前置：这一屏该画得出来");
        }
        panel.update(cx, |panel, cx| {
            panel
                .tree
                .as_mut()
                .expect("前置：树该建起来了")
                .expand_all();
            cx.notify();
        });
        for _ in 0..3 {
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .expect("前置：全展开也该画得出来");
        }
        panel.update(cx, |panel, _| {
            assert_eq!(
                panel.tree.as_ref().map_or(0, |toc| toc.visible().len()),
                ROWS,
                "前置：{ROWS} 行都该看得见，否则量的不是那一档"
            );
        });

        // 整帧：滚一下 + 走完一帧（gpui 的布局、绘制、文字成形全在里面）。
        // 滚轮事件按**窗口坐标**投进列表那一条命中区，走的是 gpui 真的滚动派发
        // （`elements/div.rs` 的 ScrollWheel 监听 → `cx.notify(current_view)`），
        // 不是替它直接挪 offset —— 替它挪就把要量的那一段绕过去了。
        let clock = Instant::now();
        for _ in 0..FRAMES {
            cx.update_window(handle, |_, window, cx| {
                window.dispatch_event(
                    ScrollWheelEvent {
                        position: point(px(120.0), px(300.0)),
                        delta: ScrollDelta::Pixels(point(px(0.0), px(-28.0))),
                        ..Default::default()
                    }
                    .to_platform_input(),
                    cx,
                );
                window.render_frame(cx);
            })
            .expect("滚动帧该画得出来");
        }
        let frame_us = clock.elapsed().as_micros() as f64 / FRAMES as f64;
        // 前置：这一趟确实在滚（偏移没动的话整帧量的是静止帧，那不是滚动单价）。
        panel.update(cx, |panel, _| {
            assert!(
                panel.scroll.offset().y < px(0.0),
                "前置：滚轮该把列表推动了，实测偏移 {:?}",
                panel.scroll.offset()
            );
        });

        // `visible()` 一趟（一帧里跑两趟）。
        let clock = Instant::now();
        for _ in 0..FRAMES {
            panel.update(cx, |panel, _| {
                let seen = panel.tree.as_ref().map_or(0, |toc| toc.visible().len());
                assert_eq!(seen, ROWS, "前置：这一趟量的是全展开");
            });
        }
        let visible_us = clock.elapsed().as_micros() as f64 / FRAMES as f64;

        // 表头那两个数**每帧是 O(1)**：`read_count` 就是 `ix + 1` 与 `rows.len()`，
        // `current_ix` 命中开屏那一次记下的 `(章号, 行号)`。所以这一格量的不是每帧的活儿，
        // 而是**章一变就走一趟**的那条兜底（`row_of_chapter` 找不着就得扫完全表）——
        // 一次翻页一笔，不在滚动帧里。放在这里是把它的单价也钉住，别当成滚动成本。
        let clock = Instant::now();
        for _ in 0..FRAMES {
            panel.update(cx, |panel, _| {
                let toc = panel.tree.as_ref().expect("前置：树该在");
                assert!(
                    toc.current_ix(usize::MAX).is_none(),
                    "前置：这一趟该走完全表"
                );
                let (read, total) = toc.read_count(Some(ROWS - 1));
                assert_eq!((read, total), (ROWS, ROWS), "前置：最后一行当下标时该读满");
            });
        }
        let chapter_change_us = clock.elapsed().as_micros() as f64 / FRAMES as f64;

        // 一屏行的元素构造（不含布局与绘制）：`rows` 里那次 `visible()` 与已读集合克隆
        // 都记在这一格上。
        let clock = Instant::now();
        for _ in 0..FRAMES {
            panel.update(cx, |panel, cx| {
                assert_eq!(panel.rows(0..20, cx).len(), 20, "前置：20 行都该建得出来");
            });
        }
        let rows_us = clock.elapsed().as_micros() as f64 / FRAMES as f64;

        println!(
            "目录 {ROWS} 行全展开 · 窗口 460×520 · {} 帧 · profile={}：\n  整帧 {frame_us:>7.0} µs/帧\
             \n  visible() 一趟 {visible_us:>7.0} µs（一帧两趟 = {:.0} µs，占整帧 {:.1}%）\
             \n  章一变才走的那一趟 {chapter_change_us:>7.0} µs（**不在滚动帧里**）\
             \n  rows(0..20) 构造 {rows_us:>7.0} µs（占整帧 {:.1}%）\n  表头那两个数每帧 O(1)，没有可占的份额",
            FRAMES,
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
            visible_us * 2.0,
            visible_us * 2.0 / frame_us * 100.0,
            rows_us / frame_us * 100.0,
        );
    }
}
