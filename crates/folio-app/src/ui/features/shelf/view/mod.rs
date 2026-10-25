//! 书架：顶部一行命令（搜索 + 导入入口），下面一片按封面排的网格。
//!
//! 两端用的是同一份架面，只有顶部那一行和一行放几本按窗口档位换：手机上是一条
//! 满幅的搜索框，桌面与平板上是定宽搜索框加一颗三点按钮 —— 三种导入方式收在
//! 点开的那条菜单里（见 [`Self::more_button`]）。见 `toolbar` 与 [`columns_for`]。
//!
//! 搜索框查的是**书名与作者**（`Store::shelf`，一条 `LIKE`），不是正文。
//! 正文的全文检索是另一件事（`Store::search`，trigram FTS，短于 3 个字命中不了）：
//! 书架上这颗框在读者眼里是「找我那本书」，输入两个字就该立刻把书名对得上的那几本
//! 列出来 —— 把两件事混在一个入口会让短查询静默返回空。
//!
//! 一片格子怎么排（几列、格间距、整片滚动）在 `grid`；一格长什么样在
//! [`super::book_card`] —— 包括「没有封面的书也要照样占住一格」那一条。
//!
//! 组只有一个入口：架面那几张组卡。组卡与书排在**同一条流**上，尺只有一把 —— 谁最近被
//! 翻开谁的卡就在先，所以点开分组里的书时顶到第 1 格的是那张组卡（那本书本身不在整架
//! 那一片，见下）。并法在 [`grid::merge_cells`]。点下去是**在架面这条栈上压一整页**
//! （`open_page`）：那一页用的是同一份架面，只是查的是另一片书（`ShelfScope`）。压成
//! 「一页」买到的正是返回那一头 —— 系统返回键把它弹掉就回到书架，而身后那一片连滚动
//! 位置都没动过。
//!
//! 组是**去处**也是**收纳**：一本书一旦挂进某个组，它就不在整架那一片出现了（`Store::shelf_ungrouped`），
//! 只有走进那一组才看得见；那几张组卡就是它去的去处。「未分组」不是一条筛选、也不是
//! 一个入口，它就是整架那一片本身。
//!
//! 顶部那颗搜索框是这一条的例外：词非空时查的是**全库**（`Store::shelf`）。某一组那一页
//! 没有搜索行（顶栏是返回 + 组名），跟着一起筛掉的话，进过组的书在这台机器上就再也搜不
//! 出来了 —— 而那在读者眼里是书丢了，不是收纳过。

// 这一屏压出来的两页与它共用同一个实体：信息页（压导航栈的整屏）与整机扫描的候选清单。
pub(in crate::ui::features::shelf) mod book_info;
pub(in crate::ui::features::shelf) mod grid;
pub(in crate::ui::features::shelf) mod scan;
pub(in crate::ui::features::shelf) mod sheets;
pub(in crate::ui::features::shelf) mod toolbar;

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use folio_core::net::remote::Transport;
use folio_core::store::Collection;
use folio_core::{
    Cancel, Format, ImportOutcome, ImportReport, Imported, PathsSource, Store, TaskEvent,
    UrlSource, submit_import,
};
use gpui_kit::base::{NavMotion, NavStackState};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::InputState;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, Image, InteractiveElement,
    IntoElement, ParentElement, PathPromptOptions, Render, SharedString, Styled, Subscription,
    Task, TestSupportExt as _, UniformListScrollHandle, WeakEntity, Window, div, px, uniform_list,
};

use super::components::book_card::{Card, GroupCard, TILE_SLOTS};
use super::components::book_menu;
use crate::ui::components::{
    BottomSheetEvent, BottomSheetState, SearchBox, SearchBoxEvent, TopBar, empty_state, muted_label,
};
use crate::ui::foundation::error::{Context as _, report};
use crate::ui::foundation::file_stem::title_stem;
use crate::ui::host;
use crate::ui::services::config::ConfigHolder;
use crate::ui::services::l10n::{book_count, tf, tr};
use crate::ui::services::library::{delete_book, pick_dir, suggested_dir, target_dir};
use crate::ui::services::resident;
use crate::ui::shell::navigation::{self, SizeClass};
use grid::{Cell, GUTTER, GridSpec, grid_spec, merge_cells};
use scan::ScanPhase;

/// 出网共用的那一份传输（`ureq` + rustls）。一个进程一个连接池就够 ——
/// 所有 URL 下载都从这里拿。
fn url_transport() -> Arc<dyn Transport> {
    static TRANSPORT: OnceLock<Arc<dyn Transport>> = OnceLock::new();
    TRANSPORT
        .get_or_init(folio_core::net::remote::ureq::shared)
        .clone()
}

/// 手机上一行放几本。
///
/// 3 是在 1080×2400 / scale 2.8 那块屏上量的：可用宽约 386 逻辑像素，三列扣掉格间距
/// 后每格约 112，封面 2:3 就是 168 高。两列会让书名只看得下三个字，四列低于拇指
/// 能稳定命中的最小格宽（56）。
///
/// 桌面与平板不按这个数，见 [`columns_for`]。
/// 架面这一屏摆的是哪一片书。**一块架面一个片**，在建成那一刻就定死 —— 不是同一块
/// 架面上来回换的筛选。
///
/// `Shelf` 摆的是**还没进任何组的那些书**加上那几张组卡，两段按「谁最近被翻开」并成
/// 一条流（前者出自 [`Store::shelf_ungrouped`]，插在哪由 [`Self::cells`] 定）。这一片不是库里存的某个组，
/// 它就是「不在任何 `collection_books` 行里」那一批（`Store::shelf_ungrouped`）—— 所以
/// 「未分组」不再单独有一页。唯一的例外是搜索：词非空时整架改查全库（`Store::shelf`），
/// 进了组的书照样搜得出来（见模块头那一段）。
///
/// `Shelf` 那一块是外壳拿着的根实例；`Collection` 是**压上导航栈的一页**（见 `open_page`）：
/// 它与整架同一条栈，所以系统返回键把这页弹掉就回到书架，而栈上压着页时那条导航栏
/// 自己收起 —— 这两件事都是「一页」买来的，不必各自再维护一遍。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::ui::features::shelf) enum ShelfScope {
    /// 整架：还没进任何组的那些书 + 那几张组卡，按最近打开并成一条流（带搜索词时是命中的全部书）。
    Shelf,
    /// 某一个组里的书。
    Collection(i64),
}

/// 书架向外层报的两件事，都带书库主键而不是路径 —— 选择器给的那份路径是临时的，
/// 书的内容早就在库里了。
pub enum ShelfEvent {
    /// 点了一张卡片。
    Open(i64),
    /// 删掉了一本。外壳需要这条：正文可能正开着这一本，书没了，那一层就该退回架面。
    Deleted(i64),
}

/// 「分享这本书」递出去时用的文件名：书名的干（过滤与划词分享卡共用一份，
/// 见 [`title_stem`]）加上这个格式自己的扩展名。
///
/// 后缀问 [`Format::copy_extension`] 而不是在这里对照格式写一张：库里的副本正是按那张表
/// 命名的（`Store::book` 交回的 `path` 那一头），两处各写一份会漂到「发出去的文件后缀和
/// 容器格式对不上」—— 而收件的应用按后缀与 media type 认领，对不上就是打不开。
fn share_file_name(title: &str, format: Format) -> String {
    format!("{}.{}", title_stem(title), format.copy_extension())
}

pub struct ShelfView {
    /// 书库句柄，来自 [`crate::ui::services::app_store`]。放 `None` 时架面是空的，
    /// 但界面照常起得来。
    ///
    /// `pub(in crate::ui::features::shelf)` 只为 [`super::book_info`] 那一页：它要在点开的时刻自己查一次库。
    pub(in crate::ui::features::shelf) store: Option<Arc<Store>>,
    /// 书架这一目的地的导航栈（外壳建的，见 `AppShell::boot`）。
    ///
    /// 「信息」那一页由架面自己压上去 —— 与阅读页把设置屏压进同一条栈是同一条路。
    /// 弱句柄：栈是外壳的东西，架面不该反过来把它拽住不让它退。
    pub(in crate::ui::features::shelf) stack: WeakEntity<NavStackState>,
    items: Vec<Card>,
    /// 架面那几张组卡。只在「整架 + 没搜词」时非空，其余时候是空的。
    ///
    /// 与 [`Self::items`] 分开放而不是并成一个列表：`items` 的格数有好几处按「几本」来读
    /// （回归门数的是书，不是格子），混在一起会让那个数悄悄变成「书 + 组」。
    /// 排格子时才接成一条下标（[`Self::cells`]），尺是那两段的 `opened_at`。
    tiles: Vec<GroupCard>,
    /// 架面那片虚拟格子滚到哪儿了。
    ///
    /// 句柄归视图拿着，不是每帧现造：`uniform_list` 的滚动位置、可见区间都记在它里面，
    /// 帧与帧之间换了句柄就等于每次重画都跳回第一排。滚轴也从这一个读位置
    /// （[`super::view`] 的 `grid` 里 `.vertical_scrollbar(&...)`）。
    grid_scroll: UniformListScrollHandle,
    /// 上一次查库用的串。
    query: SharedString,
    /// 顶上那颗搜索框。外观与「点外面退编辑态」都在 [`SearchBox`] 里，这里只
    /// 接它的 `Change` 事件重查库。
    search: Entity<SearchBox>,
    /// 这一屏摆的是哪一片书。建成那一刻定，之后不改：换片 = 另开一块架面压上栈。
    scope: ShelfScope,
    /// 库里的全部组（带书数），与架面同一次重查取的那一份。两处读它：某一页顶栏那个
    /// 名字按 id 从这里取，而「这一组还在不在」也问它。
    /// 顺序归库：组内有过打开的在前（`Store::collections`）。
    collections: Vec<Collection>,
    /// 压在栈上的那一页（某一组），只有根实例这一格是 `Some`。
    ///
    /// 拿弱句柄：那一页归栈管，pop 掉就该跟着没；根实例拽着强句柄等于让一屏已经看不见
    /// 的格子和它的封面缓存常驻。
    group_page: Option<WeakEntity<ShelfView>>,
    /// 「新建分组」那一屏的输入框 + **为哪本书**建的那个书 id。
    ///
    /// 这一格存的是**内容**，不是「开没开」：开没开看 [`Self::group_open`]。分开两半是因为
    /// 手机上那半程 —— 读者关掉弹层之后卡片还要往下沉 ~280ms，那一段内容得继续画，
    /// 所以内容要活到弹层报「停稳了」（[`Self::rest`]）才 drop，而意图得当场就变。
    group_entry: Option<(i64, Entity<InputState>)>,
    /// 建组那一屏的意图。`false` 而内容还在 = 正在沉下去。
    group_open: bool,
    /// 架面上是不是站着那块「书库位置」确认面板。
    ///
    /// 导入会把文件**拷一份**进硬盘，所以位置要先说定。没定过的时候点导入不开选择器，
    /// 而是就地换成这块面板 —— 用普通按钮而不是浮层：浮层（长按菜单那条路）在
    /// Android 上现在点不住，见 `plans/facts.md`。
    pub(in crate::ui::features::shelf) confirming: bool,
    /// 封面解码结果，按书的主键存着。
    ///
    /// 键用 `id` 而不是每次现解：改一个搜索词会把同一本书再查出来一遍，
    /// 没缓存的话每敲一个字都要把整个架面的 JPEG 重解一遍。
    /// `None` 也要存 —— 不记住这次失败，每查一次都会再试一遍同样的失败。
    ///
    /// `pub(in crate::ui::features::shelf)` 只为 [`super::book_card`] 那一格：它按需解码，缓存得跟着架面活。
    pub(in crate::ui::features::shelf) covers: HashMap<i64, Option<Arc<Image>>>,
    _search_watch: Subscription,
    /// 导入那条异步链：等选择器 → 把选中的文件交给后端管线 → 收事件直到这批结束。
    /// Task 一 drop 链就断，于是「选了文件但界面已经忘了」；而断在管线跑到一半时，
    /// 后台那份拷贝照旧跑完、结果却没人登记 —— 架上少一本，书库目录里多一份副本。
    /// 所以这条链要活到整批导完。
    _import: Option<Task<()>>,
    /// 正在进行的那批导入的读数：`(已完成, 总数)`。`total == 0` 是不定进度 ——
    /// 刚提交、管线第一条读数还没上来时就是这个样子；`None` 表示没在导入。
    /// 读数来自后端报的 `Progress` 事件，画在哪一处见 `Render` 那一段。
    pub(in crate::ui::features::shelf) importing: Option<(u64, u64)>,
    /// 正在进行的这批导入的取消旗标，来自 [`TaskHandle`](folio_core::infra::task::TaskHandle)。
    /// 读数行旁那颗「取消」按钮置位它；管线在两本书之间看见，已落库的那些是真的
    /// （`Cancelled` 事件来了重查一次架面就有）。
    import_cancel: Option<Cancel>,
    /// 整机扫描那一屏的意图（内容在 [`Self::scan`]）。见 [`Self::group_open`] 那一段为什么分两半。
    pub(in crate::ui::features::shelf) scan_open: bool,
    /// 整机扫描那一屏的状态：正在扫 / 已出清单。`None` = 这一屏没东西可画了。
    /// 面板本体与全部动作在 [`super::scan`]。
    pub(in crate::ui::features::shelf) scan: Option<ScanPhase>,
    /// 扫描那条异步链（事件桥）。与 `_import` 同理，要活到整轮结束。
    pub(in crate::ui::features::shelf) _scan: Option<Task<()>>,
    /// 自动导入（监听 / 传书）事件的订阅链。这些批次不是这个视图提交的，
    /// 靠共享事件通道流回来 —— 处理与手工导入同一套（读数、重查架面）。
    _auto_import: Option<Task<()>>,
    /// 「从链接导入」那一屏的输入框（内容）。开没开看 [`Self::url_open`]。
    url_entry: Option<Entity<InputState>>,
    /// 链接导入那一屏的意图。见 [`Self::group_open`] 那一段为什么分两半。
    url_open: bool,
    /// 移动端那三块弹层各自的开合与停靠档。桌面那一档用的是 `Popover`，这三份只是
    /// 没挂载而已 —— 状态留着比按档位分叉省一件事：转屏 / 拉窗口时不必重建。
    group_sheet: Entity<BottomSheetState>,
    url_sheet: Entity<BottomSheetState>,
    scan_sheet: Entity<BottomSheetState>,
    /// 三块弹层各自「读者自己关掉了」那一条：下拉过头、点遮罩、按返回都走它。
    /// 订阅在 `boot` 里装一次，清的是架面这一侧的状态（面板留着而弹层没了会立刻被
    /// 重新抬起来，见 [`Self::sync_sheet`]）。
    _sheet_watches: Vec<Subscription>,
    /// 压上栈的那页「分组」的订阅。`Some` = 那一页还开着。
    ///
    /// 外壳只挂着**根实例**这一条订阅（`AppShell::boot` 里的 `_shelf_watch`），而点开一本
    /// 书与删掉一本都是从眼下那一页发出来的 —— 所以那一页报的两件事要由根实例转一道，
    /// 否则在组里点封面没反应、删了开着的那本也不退那一层。
    _group_watch: Option<Subscription>,
    /// 压上栈的那页「信息」的订阅。`Some` = 那一页还开着。
    ///
    /// 只要一条：那一页写回书名作者之后报一声，架面重查 —— 那一行字就是这张卡上的。
    /// 每压一页换掉它，上一页的订阅跟着 drop（`Subscription` 一没就退订）。
    pub(in crate::ui::features::shelf) _info_watch: Option<Subscription>,
    /// 「书要拷到哪儿」那块面板里，系统文件夹选择器那条异步链。与 `_import` 同理，要活到选完。
    _pick: Option<Task<()>>,
    /// 导入完成后的**下一帧兜底重查**。选择器把应用切到后台，恢复前台的那一帧上
    /// 后台期间发的 notify 可能丢（真机实测：导入后卡片要重新进书架才出现），
    /// render 里见到这面旗就强制重查一次 —— 代价只是多查一遍库。
    pending_reload: bool,
    /// 格子「落定」那段动画的起算时刻，第一次真画出格子时才记。
    ///
    /// 只为**第一次**上屏留的，之后不再重放（理由见 [`super::book_card::Enter`]）。
    /// 记在架面而不是元素状态里：`uniform_list` 量行高时也在 `request_layout` 阶段
    /// 跑一遍 item 闭包，那里碰不得 gpui 的元素状态。
    ///
    /// `pub(in crate::ui::features::shelf)` 只为 [`super::book_card::ShelfView::enter_clock`] 那一处：动效的
    /// 那几条规矩都收在卡片文件里，时钟也得由它自己拨。
    pub(in crate::ui::features::shelf) enter_at: Option<Instant>,
}

impl ShelfView {
    pub fn boot(
        window: &mut Window,
        cx: &mut Context<Self>,
        stack: WeakEntity<NavStackState>,
    ) -> Self {
        let mut this = Self::build(window, cx, stack, ShelfScope::Shelf);
        // 自动导入（监听、传书）的事件流。处理与手工导入完全同一套，而**只由根实例接**：
        // `auto_import_events()` 给的是同一条通道的又一个接收端（`async_channel` 的
        // `Receiver::clone` 是**分流**不是广播），组里那一页再挂一条就会把架面该收到的那些
        // 事件偷走 —— 表现是导入完成后哪一屏都不动，全看谁先抢到新事件。
        let auto_events = crate::ui::services::import_queue::auto_import_events();
        this._auto_import = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            while let Ok(event) = auto_events.recv().await {
                if this
                    .update(cx, |this, cx| this.import_event(event, cx))
                    .is_err()
                {
                    return; // 架面没了，订阅跟着收。
                }
            }
        }));
        this
    }

    /// 起一块架面：查库、建那三块弹层，并按 `scope` 决定这一屏摆哪一片书。
    ///
    /// 整架与某一组走的是同一份实现（同一套格子、同一张管理菜单、同一条空态），只有顶上
    /// 那一行不同 —— 见 `Render` 里对 [`Self::scope`] 的那一次分叉。
    fn build(
        window: &mut Window,
        cx: &mut Context<Self>,
        stack: WeakEntity<NavStackState>,
        scope: ShelfScope,
    ) -> Self {
        let store = crate::ui::services::app_store::handle(cx);
        if store.is_none() {
            tracing::error!("ShelfView: 书库没装，架面会是空的");
        }
        // 尺寸按窗口档位在建的时候就定下：手机上一条满幅的框用默认档（拇指按得中），
        // 桌面工具栏那条用 small 与旁边的按钮同高（见 `toolbar`）。桌面窗口再怎么拉，
        // 档位也夹在 Medium 往上（`SizeClass::of` 的约定），不会中途换档。
        let compact = SizeClass::of(window).is_compact();
        let search = cx.new(|cx| {
            let search = SearchBox::new(tr(cx, "shelf.search_placeholder"), window, cx);
            if compact { search } else { search.small() }
        });
        let _search_watch = cx.subscribe(&search, |this, _, event, cx| {
            // `SearchBoxEvent` 现在只有 Change 一条：直接解构。将来加变体时这里
            // 编译器会逼着补齐。
            let SearchBoxEvent::Change(query) = event;
            this.reload(query, cx);
        });
        // 三块弹层各订阅一次「读者自己把它关掉了」：下拉过头、点遮罩、按返回都只报这一条。
        // 不据此清架面那一侧的状态的话，下一帧 [`Self::sync_sheet`] 看着「面板还开着」
        // 又把弹层抬起来 —— 在读者手里就成了「这层关不掉」。
        let group_sheet = cx.new(|cx| BottomSheetState::closed(cx));
        let url_sheet = cx.new(|cx| BottomSheetState::closed(cx));
        let scan_sheet = cx.new(|cx| BottomSheetState::closed(cx));
        let _sheet_watches = vec![
            cx.subscribe(&group_sheet, |this, _, event, _| {
                if matches!(event, BottomSheetEvent::Dismissed) {
                    this.group_open = false;
                }
            }),
            cx.subscribe(&url_sheet, |this, _, event, _| {
                if matches!(event, BottomSheetEvent::Dismissed) {
                    this.url_open = false;
                }
            }),
            cx.subscribe(&scan_sheet, |this, _, event, _| {
                if matches!(event, BottomSheetEvent::Dismissed) {
                    this.scan_open = false;
                }
            }),
        ];
        let mut this = Self {
            store,
            stack,
            items: Vec::new(),
            tiles: Vec::new(),
            grid_scroll: UniformListScrollHandle::new(),
            query: SharedString::default(),
            search,
            scope,
            collections: Vec::new(),
            group_page: None,
            group_entry: None,
            group_open: false,
            confirming: false,
            covers: HashMap::new(),
            _search_watch,
            _import: None,
            importing: None,
            import_cancel: None,
            scan: None,
            scan_open: false,
            _scan: None,
            _auto_import: None,
            url_entry: None,
            url_open: false,
            group_sheet,
            url_sheet,
            scan_sheet,
            _sheet_watches,
            _group_watch: None,
            _info_watch: None,
            _pick: None,
            pending_reload: false,
            enter_at: None,
        };
        this.reload(&SharedString::default(), cx);
        this
    }

    /// 查一遍库换成卡片。查不动时保留上一次的架面而不是清空：
    /// 一条语句报错不该让读者的书看起来消失了。
    fn reload(&mut self, query: &str, cx: &mut Context<Self>) {
        if self.query == query && !self.items.is_empty() {
            return;
        }
        // 组清单与架面同一次重查：某一页顶栏那个名字与「这一组还在不在」读的都是这一份。
        self.collections = self
            .store
            .as_ref()
            .and_then(|store| store.collections().ok())
            .unwrap_or_default();
        // 这一页对应的那个组已经没了（在别处删掉的）：pop 自己回上一层，而不是给一屏
        // 看着像坏掉的空白。根实例不受这一条影响 —— 它的 `scope` 不是某一组。
        if let ShelfScope::Collection(id) = self.scope
            && !self.collections.iter().any(|c| c.id == id)
        {
            self.pop(cx);
            return;
        }
        // 组卡只在「整架 + 没搜词」那一屏出现。某一组那一页再摆一排组卡，等于把读者刚
        // 走进去的那一层又摊回他面前；而搜的是书不是组 —— 词非空时那几张卡上的封面跟
        // 搜索结果没有半点关系，摆出来只会读成「这几本匹配」。
        self.tiles = if self.scope == ShelfScope::Shelf && query.trim().is_empty() {
            self.tile_cards()
        } else {
            Vec::new()
        };
        // 整架那一片摆的是还没进任何组的书 —— 组既是去处也是收纳，进了组的书只有走进
        // 那一组才看得见。搜索那一头不跟着筛：词非空时查全库，否则进过组的书在架上再也
        // 搜不出来（某一组那一页没有搜索行），而读者读到的结果是「书丢了」。
        let queried = self.store.as_ref().map(|store| match self.scope {
            ShelfScope::Shelf => {
                if query.trim().is_empty() {
                    store.shelf_ungrouped(query)
                } else {
                    store.shelf(query)
                }
            }
            ShelfScope::Collection(id) => store.shelf_in_collection(id, query),
        });
        match queried {
            Some(Ok(items)) => {
                self.query = query.into();
                // 先把封面解析出来再收集：`map(|item| self.card(item))` 那种写法同时借
                // `self` 的不可变与可变两半，编译器不让过。
                self.items = items
                    .into_iter()
                    .map(|item| {
                        let cover = self.resolve_cover(&item);
                        self.card(item, cover)
                    })
                    .collect();
            }
            Some(Err(err)) => tracing::error!("ShelfView: 查书架失败 {err}"),
            None => {
                self.query = query.into();
                self.items.clear();
            }
        }
        cx.notify();
    }

    /// 按当前搜索词**强制**重查一遍架面。
    ///
    /// `reload` 的去重按「词没变且架面非空」判，而这几处要办的正是「词没变、架上的字却变了」：
    /// 删了一本、导进来一本、读者在「信息」那一页改了书名 —— 先清空逼它重查，这一手不必每个
    /// 调用处各自记着。`pub(crate)` 是为最后那一条：外壳收到 [`BookInfoEvent::Saved`](super::BookInfoEvent)
    /// 后叫它一次。
    pub(crate) fn force_reload(&mut self, cx: &mut Context<Self>) {
        self.items.clear();
        let query = self.query.clone();
        self.reload(&query, cx);
        // 压在栈上的那一页是**另一块架面**，查的是自己那一片，而外壳手上只有根实例这一条
        // 句柄。开一本书把它的 `last_opened_at` 顶到最前，正落在某一组那一页的序与进度上，
        // 监听目录与传书那几本也只有根实例接得到事件 —— 不跟着敲一下，读者读完退回来看见
        // 的是进组前的旧序。
        if let Some(page) = self.group_page.as_ref().and_then(|page| page.upgrade()) {
            page.update(cx, |page, cx| page.force_reload(cx));
        }
    }

    /// 架面那几张组卡：每个组一张，卡上是这个组最后丢进去的那几本的封面。
    ///
    /// 封面走的是 [`Self::resolve_cover`] 那一条，与书卡同一份缓存：组卡与下面那片书
    /// 十有八九是同一批封面，解两遍是白解。
    ///
    /// 查不动就是一片空的：架面少一排卡看着像「还没建组」，比摆几张假卡或整屏报错好。
    fn tile_cards(&mut self) -> Vec<GroupCard> {
        let Some(store) = &self.store else {
            return Vec::new();
        };
        let Ok(tiles) = store.collection_tiles(TILE_SLOTS) else {
            tracing::error!("ShelfView: 查组卡失败，架面那一排卡是空的");
            return Vec::new();
        };
        tiles
            .into_iter()
            .map(|(collection, items)| {
                let covers = items.iter().map(|item| self.resolve_cover(item)).collect();
                GroupCard {
                    id: collection.id,
                    name: collection.name.into(),
                    count: collection.book_count,
                    // 这一组的「最近被翻开」取它自己那条序的头一本：`collection_tiles` 里
                    // 那一串走的就是 `SHELF_ORDER`（最近在先），头一本的 `last_opened_at`
                    // 就是库给组卡排序用的那个 `MAX(last_opened_at)` —— 同一把尺，不是近似。
                    opened_at: items.first().and_then(|item| item.last_opened_at),
                    covers,
                }
            })
            .collect()
    }

    /// 架面那一条流的格子顺序：第 `ix` 格该从 `tiles` 还是 `items` 取。
    ///
    /// 两段的先后各自归库（整架那条 `SHELF_ORDER`，组卡那条 `MAX(last_opened_at)`），
    /// 这里只把它们**按同一把尺并起来**：谁最近被翻开，谁的卡就在先 —— 点开分组里的书时
    /// 那本书不在整架那一片，顶到第 1 格的就是那张组卡。并法与它的三条断言在
    /// [`grid::merge_cells`]。
    fn cells(&self) -> Vec<Cell> {
        let tiles: Vec<Option<i64>> = self.tiles.iter().map(|tile| tile.opened_at).collect();
        let books: Vec<Option<i64>> = self.items.iter().map(|card| card.opened_at).collect();
        merge_cells(&tiles, &books)
    }

    /// 把焦点放进搜索框。
    ///
    /// 外壳那条「返回先退编辑态」的回归门要走**真实**焦点路径：gpui 的测试窗口没有
    /// 「点这一块元素」的定位法，而自己造一个 `FocusHandle` 就绕过了输入框真正挂在树上的
    /// 那一条，测到的只是测试自己。只在跑那条门的那一端编译（门要 `try_find`，移动端没有
    /// 测试支持注册路径）。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |search, cx| search.focus(window, cx));
    }

    /// 点「导入」。书库位置没确认过就不开选择器，先把那块面板摆出来。
    ///
    /// 一批还没导完时这一颗按不动：再开一个选择器会把上一条链挤掉（`_import` 只有一格），
    /// 而链断在管线跑到一半时，后台那份拷贝照样跑完、结果却没人登记 —— 书库目录里就多了
    /// 几本架上看不见的副本。命令行下面那一行读数（见 `Render`）就是「等一下再点」的提示。
    fn import(&mut self, cx: &mut Context<Self>) {
        if self.importing.is_some() {
            return;
        }
        if target_dir(&ConfigHolder::library(cx)).is_some() {
            self.open_picker(cx);
            return;
        }
        self.confirming = true;
        cx.notify();
    }

    /// 用系统文件夹选择器挑一个书库目录。选中结果由 [`pick_dir`] 写进配置，
    /// 下一帧这块面板上显示的路径就跟着变；读者再点「确认并导入」。
    fn pick_library(&mut self, cx: &mut Context<Self>) {
        self._pick = Some(pick_dir(cx));
    }

    /// 确认「书就拷到这里」。已经有一个位置了（选过或以前确认过）就原样用，
    /// 没有才落回建议位置；然后接着把选择器打开 —— 读者点导入要的是导书，不是填表格。
    fn confirm_library(&mut self, cx: &mut Context<Self>) {
        if target_dir(&ConfigHolder::library(cx)).is_none() {
            let dir = suggested_dir();
            ConfigHolder::edit_library(cx, |library| {
                library.dir = dir.to_string_lossy().into_owned()
            });
        }
        self.confirming = false;
        self.open_picker(cx);
    }

    fn cancel_confirm(&mut self, cx: &mut Context<Self>) {
        self.confirming = false;
        cx.notify();
    }

    /// 打开系统文件选择器选书，把选中的文件交给**后端导入管线**（`folio_core::ingest::import`）。
    ///
    /// **先复制**不是多余的一步：选择器给的是它自己临时目录里的路径，系统随时会清，
    /// 而 `books.path` 那一列是书的身份（同路径重复导入算更新，不算第二本书）。
    /// 手机那两端的路径本来就是桥先拷出来的临时副本：导入读完由
    /// [`PathsSource::picked_temporary`] 的 `Drop` 把 `gpui-import-<uuid>/` 连同文件清掉；
    /// 桌面选择器给的是读者原文件，照旧不删。
    ///
    /// 拷贝、解析、落库都发生在 `folio_core::infra::task` 的工作线程上：解析一本 EPUB 是几十
    /// 到几百毫秒，一次选十本串在界面线程上跑就是几秒的卡顿。界面线程只留两件本来
    /// 就该它做的事 —— 读配置里的书库位置，以及按管线交回的结果刷新架面
    /// （见 [`Self::finish_import`]）。
    fn open_picker(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        // 选择器那条链 detached 即可：它只等选择器回来，真正的长活（事件桥）
        // 由 [`Self::dispatch_import`] 存进 `_import`。
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let paths = match receiver.await {
                Ok(Ok(Some(paths))) => paths,
                Ok(Ok(None)) => return,
                Ok(Err(err)) => {
                    tracing::error!("ShelfView: 文件选择器失败 {err}");
                    return;
                }
                Err(_) => return,
            };
            if paths.is_empty() {
                // 一个都没选上：不去敲后端，也别让架面闪一下「导入中」。
                return;
            }
            let _ = this.update(cx, |this, cx| {
                let source: Box<dyn folio_core::ingest::import::Source> =
                    if crate::ui::host::picked_files_are_temporary(cx) {
                        Box::new(PathsSource::picked_temporary(paths))
                    } else {
                        Box::new(PathsSource::picked(paths))
                    };
                this.dispatch_import(source, cx)
            });
        })
        .detach();
    }

    /// 把一个来源交给后端导入管线，并把事件桥挂起来 —— 选择器选完文件与
    /// 扫描清单勾选「导入所选」走的是同一条路。
    ///
    /// 架面已经不在了（`update` 报错的那些时刻）就直接返回：后面没有界面可画了。
    pub(in crate::ui::features::shelf) fn dispatch_import(
        &mut self,
        source: Box<dyn folio_core::ingest::import::Source>,
        cx: &mut Context<Self>,
    ) {
        // 书库位置到这一刻再问一次：选择器开着的那段时间里读者可能改过它。
        let Some(dir) = target_dir(&ConfigHolder::library(cx)) else {
            tracing::error!("ShelfView: 书库位置没确认过，这一批没有导入");
            return;
        };
        // 管线在后台落库，要的是能跨线程的 `Arc<Store>`。
        let Some(store) = crate::ui::services::app_store::handle(cx) else {
            tracing::error!("ShelfView: 书库没装上，这一批没有导入");
            return;
        };
        // 拷贝、解析、落库都在后端管线里逐本完成（`folio_core::ingest::import`）。
        let handle = submit_import(&resident::pool(), source, dir, store);
        let cancel = handle.cancel.clone();
        self.importing = Some((0, 0));
        self.import_cancel = Some(cancel);
        cx.notify();

        // 管线交回的是 `std::sync::mpsc` 的接收端：它只会阻塞读它的那条线程，
        // 而界面线程一阻塞整个窗口就停了。所以把读的那一手整个搬到后台，每条事件
        // 转成一次界面更新 —— `async_channel` 这一头能 await，进度就是一条条流上来的。
        let (forward, events) = async_channel::unbounded::<TaskEvent<Vec<ImportOutcome>>>();
        cx.background_spawn(async move {
            // 只有「接收端已经没了」才会报错，也就是这条链断掉（架面关了、或整批结束）。
            // 那时候剩下的事件没有要的人，收手即可。
            for event in handle.events.iter() {
                if forward.send_blocking(event).is_err() {
                    break;
                }
            }
        })
        .detach();

        self._import = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            while let Ok(event) = events.recv().await {
                if this
                    .update(cx, |this, cx| this.import_event(event, cx))
                    .is_err()
                {
                    return;
                }
            }
            // 通道关了：要么这批结束了（`Finished` 那条本来就已经把读数收掉），要么后台
            // 那条线程出事了、结果永远不会来。两种都不能让「导入中」永远挂在架面上。
            let _ = this.update(cx, |this, cx| {
                if this.importing.take().is_some() {
                    cx.notify();
                }
            });
        }));
    }

    /// 把后端管线的一条事件喂进架面。只在跑那几扇回归门的那一端编译。
    ///
    /// 存在的理由只有一条：这条链的起点是系统文件选择器，而测试窗口里它不会返回，
    /// 于是门只能从「事件已经上了界面线程」这一处进 —— 自己造一份 `ShelfView` 就绕过了
    /// 外壳里真正挂在树上的那一个，测到的只是测试自己（`shell::booted` 里同理）。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn feed_import_event(
        &mut self,
        event: TaskEvent<Vec<ImportOutcome>>,
        cx: &mut Context<Self>,
    ) {
        self.import_event(event, cx);
    }

    /// 架上此刻摆着几本。只给外壳那几扇回归门读：门要钉的是「后端交回的结果有没有变成
    /// 架面上的格子」，而数格子这件事不该成为一个公开接口。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn card_count(&self) -> usize {
        self.items.len()
    }
    /// 架上第 `ix` 格此刻摆的书名。同样只给回归门读：那一行字是「信息」那一页改完
    /// 该立刻看到的结果，而读格子这件事不该成为公开接口。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn card_title(&self, ix: usize) -> Option<SharedString> {
        self.items.get(ix).map(|card| card.title.clone())
    }

    /// 喂一条「点开某一组」的命令。真实入口是架面那几张组卡，
    /// 而测试窗口里没有「点到这一块元素」的定位法 —— 与 [`Self::feed_import_event`] 同一取舍。
    ///
    /// 收的是组的主键而不是 [`ShelfScope`]：那个枚举归架面模块自己用，外壳那侧的门不该为了
    /// 压一页去 import 它。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn open_group_for_test(
        &mut self,
        group: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_page(ShelfScope::Collection(group), window, cx);
    }

    /// 架面此刻摆着几张组卡。只给回归门读，理由同 [`Self::card_count`]。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn tile_count(&self) -> usize {
        self.tiles.len()
    }

    /// 第 `ix` 张组卡认的是哪个组、下面那行念的是几本。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn tile_at(&self, ix: usize) -> Option<(i64, SharedString, usize)> {
        self.tiles.get(ix).map(|t| (t.id, t.name.clone(), t.count))
    }

    /// 第 `ix` **格**摆的是哪一张：组卡回组名、书回书名。只给回归门读 —— 门要钉的是
    /// 「刚翻开的那一格有没有顶到前面」，而读格子这件事不该成为公开接口。
    ///
    /// 与 [`Self::card_title`] 的区别正是这一格的全部意义：那一行读的是库里那一段的序，
    /// 这一行读的是两段并完之后读者真正看见的序。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn cell_label(&self, ix: usize) -> Option<SharedString> {
        match *self.cells().get(ix)? {
            Cell::Tile(at) => Some(self.tiles[at].name.clone()),
            Cell::Book(at) => Some(self.items[at].title.clone()),
        }
    }

    /// 后端导入管线的一条事件 → 架面的一次变化。
    fn import_event(&mut self, event: TaskEvent<Vec<ImportOutcome>>, cx: &mut Context<Self>) {
        match event {
            // 开始那条不带读数：提交的那一行已经把架面标成「导入中」了。
            TaskEvent::Started => {}
            TaskEvent::Progress(progress) => {
                self.importing = Some((progress.done, progress.total));
                // 管线附送的文案只进日志：它是 core 里的事实描述，屏幕上的一句话
                // 要走 locale（见 `shelf.importing_of`），否则英文界面上会冒出中文来。
                if !progress.message.is_empty() {
                    tracing::info!(
                        "ShelfView: 导入进度 {}/{} —— {}",
                        progress.done,
                        progress.total,
                        progress.message
                    );
                }
                cx.notify();
            }
            TaskEvent::Bytes(bytes) => {
                // 字节级读数（下载 / 拷贝）。本地导入不报这一级；等链接下载
                // 接上后这里换画「正在传《名字》」的读数。
                tracing::debug!("ShelfView: 字节进度 {}（{:?}）", bytes.done, bytes.total);
            }
            TaskEvent::Finished(Ok(outcomes)) => self.finish_import(outcomes, cx),
            TaskEvent::Finished(Err(err)) => {
                // 管线不会整体失败：一个源解不开只影响它自己，其余的照常交回（见
                // `folio_core::ingest::import`）。走到这一条只有一种可能 —— 后台那件事根本没跑成。
                tracing::error!("ShelfView: 导入作业失败 {err}");
                self.importing = None;
                self.import_cancel = None;
                cx.notify();
            }
            TaskEvent::Cancelled => {
                // 取消的语义（`folio_core::ingest::import`）：**已经落库的那些是真的**。
                // 重查一次架面，读者就能看见取消那一刻已经进去的书。
                tracing::info!("ShelfView: 导入被取消，已登记的那些保留");
                self.importing = None;
                self.import_cancel = None;
                self.force_reload(cx);
                self.pending_reload = true;
                cx.notify();
            }
        }
    }

    /// 后端管线交回的一批结果 → 架面刷新：清掉换过封面的缓存条目，重查库。
    ///
    /// 落库已经在管线里做完了；这里只消费「哪几本的 id 变了」—— 封面可能跟着
    /// 这次导入换了，缓存里那条旧的要清掉。
    fn finish_import(&mut self, outcomes: Vec<ImportOutcome>, cx: &mut Context<Self>) {
        self.importing = None;
        self.import_cancel = None;
        for outcome in &outcomes {
            match &outcome.result {
                Ok(Imported::Added(registered)) => {
                    // 封面可能跟着这次导入换了；缓存里那条旧的要清掉。
                    self.covers.remove(&registered.book_id);
                }
                // 同内容的第二个来源：架上那一本早就在了，没有要刷的东西。
                // 「已存在」与「成功」在报表里是两栏，这里也分开说（见下面那句总结）。
                Ok(Imported::Duplicate { .. }) => tracing::debug!(
                    "ShelfView: 《{}》与架上某一本同内容，没收下第二份",
                    outcome.candidate.name
                ),
                // 坏的那本指名道姓进日志：读者只知道少了几本，得有人能查为什么。
                Err(err) => {
                    tracing::error!("ShelfView: 导入《{}》失败 {err:#}", outcome.candidate.name)
                }
            }
        }
        // 一句分类总结（`plans/基础设施.md` §3.2：批量作业要能聚合报表）。界面上那张
        // 可折叠的作业卡片还是待决（`plans/导入管线.md` §十一 待决第 7 条），但数已经
        // 先落进日志了 —— 读者问「我少了哪三本」时，这一行加上上面那几条指名道姓的
        // `Err`，就是全部证据。
        tracing::info!(
            "ShelfView: 一批 {} 本结清 —— {}",
            outcomes.len(),
            ImportReport::tally(&outcomes).summary()
        );
        // 新入库的书要立刻上架，但搜索词得留着：读者可能正搜到一半。走 `force_reload`
        // 那一手（清空逼它重查，顺带敲一下压着的那一页 —— 监听目录与传书那几本只有根实例
        // 接得到事件，压着的那一页要由这一手带着重查）。
        self.force_reload(cx);
        // 这一批是后台跑完才回到这里的一次更新，没有别的动作会顺带把架面弄脏 ——
        // 不 `notify` 的话读者得等到下一次别的输入才看见新上来的那几本。
        // 下一帧再兜底重查一次：选择器把应用切到后台期间发的 notify 可能丢。
        self.pending_reload = true;
        cx.notify();
    }

    /// 打开「从链接导入」那一屏。书库位置没确认过先摆确认面板 —— 与选文件导入
    /// 同一道门禁：往哪儿拷还没说清楚，就不该动手。
    fn open_url_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.url_open || self.importing.is_some() || self.scan_open {
            return;
        }
        if target_dir(&ConfigHolder::library(cx)).is_none() {
            self.confirming = true;
            cx.notify();
            return;
        }
        self.url_entry =
            Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(tr(cx, "shelf.url_placeholder"))
            }));
        self.url_open = true;
        cx.notify();
    }

    /// 收「从链接导入」那一屏。**只**改意图：内容（那个输入框）留到弹层停稳才 drop，
    /// 不然手机上读者看到的是一张空卡往下沉。桌面那张浮层没有退场动画，同一帧就没了。
    fn close_url_panel(&mut self, cx: &mut Context<Self>) {
        self.url_open = false;
        cx.notify();
    }

    /// 那一屏上那颗「导入」。链接不对（不是 http/https）就不动 —— 假链接交给
    /// 管线只会得到一条谁都看不懂的网络错误。
    fn import_from_url(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = &self.url_entry else {
            return;
        };
        let url = entry.read(cx).value().trim().to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            tracing::info!(
                "ShelfView: 不是 http(s) 链接，不导入：{}",
                &url[..url.len().min(80)]
            );
            return;
        }
        self.close_url_panel(cx);
        self.dispatch_import(Box::new(UrlSource::new(url, url_transport())), cx);
    }

    /// 两块表单那一屏在弹层里的内容：弹层还没停稳、且内容还在的时候才给。
    ///
    /// 弹层里给横向留白而浮层里不给：浮层自己那一圈 `p_3` 已经留了，弹层那块表面是满幅的。
    fn form_body(
        &self,
        state: &Entity<BottomSheetState>,
        entry: Option<&Entity<InputState>>,
        cx: &mut Context<Self>,
        form: impl Fn(&Self, Entity<InputState>, &mut Context<Self>) -> AnyElement,
    ) -> Option<AnyElement> {
        if state.read(cx).is_idle() {
            return None;
        }
        Some(
            div()
                .w_full()
                .px(px(GUTTER))
                .pb_4()
                .child(form(self, entry?.clone(), cx))
                .into_any_element(),
        )
    }

    /// 把三块弹层的开合**派生**自那三个意图位，顺手把「已经停稳」的那些内容 drop 掉。
    ///
    /// 派生而不是在每一处改意图的地方各叫一次 `show()` / `hide()`：那样有五六处，漏一处
    /// 就是一屏关不掉的弹层。桌面那一档 `want` 恒假，于是意图一收内容当场跟着没 ——
    /// 与那张本来就没有退场动画的浮层正好对得上。
    fn sync_panels(&mut self, compact: bool, cx: &mut Context<Self>) {
        for (want, state) in [
            (self.group_open, &self.group_sheet),
            (self.url_open, &self.url_sheet),
            (self.scan_open, &self.scan_sheet),
        ] {
            match (want && compact, state.read(cx).is_open()) {
                (true, false) => state.update(cx, |state, cx| state.show(cx)),
                (false, true) => state.update(cx, |state, cx| state.hide(cx)),
                _ => {}
            }
        }
        if !self.group_open && self.rest(&self.group_sheet, cx) {
            self.group_entry = None;
        }
        if !self.url_open && self.rest(&self.url_sheet, cx) {
            self.url_entry = None;
        }
        if !self.scan_open && self.rest(&self.scan_sheet, cx) {
            self.scan = None;
        }
    }

    /// 这一块弹层是不是已经**沉到底、不在动了**：那一刻起内容不必再画。
    fn rest(&self, state: &Entity<BottomSheetState>, cx: &App) -> bool {
        state.read(cx).is_idle()
    }

    /// 读数行旁那颗「取消」。置位旗标就返回：管线在两本书之间看见它，已落库的
    /// 那些是真的（`Cancelled` 事件来了重查架面）。
    fn cancel_import(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = &self.import_cancel {
            cancel.trigger();
            tracing::info!("ShelfView: 请求取消导入");
        }
        cx.notify();
    }

    /// 这本书在各分组里的名分：`(组 id, 组名, 在不在里面)`，供卡片菜单在**打开那一刻**现查。
    ///
    /// 现查而不是用架面那份 [`Self::collections`]：读者可能刚在另一本书的菜单里建了一个组，
    /// 这一本紧接着就该看得见它，而那份清单只在重查架面时更新一次。库里的组是几十行的量，
    /// 一次点击查两趟比缓存它便宜。
    pub(in crate::ui::features::shelf) fn group_items(
        &self,
        book_id: i64,
    ) -> Vec<book_menu::GroupRow> {
        let Some(store) = &self.store else {
            return Vec::new();
        };
        let (mine, all) = (
            store.collections_of_book(book_id).ok(),
            store.collections().ok(),
        );
        let (Some(mine), Some(all)) = (mine, all) else {
            tracing::error!("ShelfView: 查分组失败，那张卡的菜单里这一节是空的");
            return Vec::new();
        };
        all.into_iter()
            .map(|c| {
                let inside = mine.iter().any(|mine| mine.id == c.id);
                (c.id, c.name.into(), inside)
            })
            .collect()
    }

    /// 把这本书在某个组里切进 / 切出，然后重查架面。
    ///
    /// 「在不在」问的是库里那份名分而不是界面状态：这一条既是加入也是移出，判错一次
    /// 就是把读者刚加的组又删回去。存储层两头都幂等（重复加是 `false`、空移出也是 `false`），
    /// 所以这里不需要额外的防抖。
    pub(in crate::ui::features::shelf) fn toggle_group(
        &mut self,
        book_id: i64,
        collection_id: i64,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = &self.store else {
            tracing::error!("ShelfView: 没有书库，第 {book_id} 本的分组改不了");
            return;
        };
        let inside = store
            .collections_of_book(book_id)
            .map(|mine| mine.iter().any(|c| c.id == collection_id))
            .unwrap_or(false);
        let got = if inside {
            store
                .remove_from_collection(collection_id, book_id)
                .map(|_| ())
        } else {
            store.add_to_collection(collection_id, book_id).map(|_| ())
        };
        if let Err(err) = got.with_context(|| format!("改第 {book_id} 本的分组")) {
            report("ShelfView", &err);
            return;
        }
        // 词没变、筛的可能也不是同一批，所以走 `force_reload` 那一手。
        self.force_reload(cx);
    }

    /// 「新建分组…」：开那一屏，并记下是为哪一本书建的。
    ///
    /// 入口只有卡片菜单里那一格（长按或右键，说的是「为这本书建个组」）：桌面上是一张
    /// 居中的浮层，手机上压进底部弹层，两种容器都不需要触发器（见 [`Self::group_popover`]）。
    pub(in crate::ui::features::shelf) fn new_group(
        &mut self,
        book_id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.group_open {
            return;
        }
        self.group_entry = Some((
            book_id,
            cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "shelf.group_placeholder"))),
        ));
        self.group_open = true;
        cx.notify();
    }

    /// 收「新建分组」那一屏。**只**改意图，理由同 [`Self::close_url_panel`]。
    pub(in crate::ui::features::shelf) fn close_group_panel(&mut self, cx: &mut Context<Self>) {
        self.group_open = false;
        cx.notify();
    }

    /// 那一屏上那颗「建组」：建好就把这本书丢进去，然后重查架面。
    ///
    /// 同名即复用是库里定的规矩（`ON CONFLICT(name) DO NOTHING` 再回查 id），所以这里
    /// 不该拦「那个组已经有了」—— 读者要的是进那个组。空名由存储层回
    /// `EmptyCollectionName`，报出来而**那一屏留着**：把人家打了一半的字清掉是错的方向。
    fn create_group(&mut self, cx: &mut Context<Self>) {
        let Some((book_id, entry)) = self.group_entry.clone() else {
            return;
        };
        let name = entry.read(cx).value().trim().to_string();
        let Some(store) = &self.store else {
            tracing::error!("ShelfView: 没有书库，建不了组");
            return;
        };
        let created = store
            .create_collection(&name)
            .and_then(|id| store.add_to_collection(id, book_id).map(|_| id))
            .with_context(|| format!("建组「{name}」"));
        match created {
            Ok(id) => {
                tracing::info!("ShelfView: 第 {book_id} 本进了组 {id}「{name}」");
                self.group_open = false;
                self.force_reload(cx);
            }
            Err(err) => report("ShelfView", &err),
        }
    }

    /// 从组卡那一格点进某一组：在架面那条栈上**压一整页**。
    /// 与「信息」那一页同一条路（见 [`super::book_info`]）。
    ///
    /// 压页而不是把眼前这片换个筛选，换来三件不用各自维护的事：系统返回键把这页弹掉就回到
    /// 书架（`AppShell::back` 那条「栈深大于 1 就 pop」）、栈上压着页时那条导航栏自己收起、
    /// 而整架那一片的格子与**滚到的那一排**原样留在身后 —— 换筛选那一手要重查一遍，读者
    /// 退回来看见的不是自己离开时的那一排。
    pub(in crate::ui::features::shelf) fn open_page(
        &mut self,
        scope: ShelfScope,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 这里**不**加「已经压着一页就退回去」那道守卫。转场那一段 `NavStack` 自己在这一层
        // 上盖一块 `occlude`，两页都不吃点击（gpui-base `nav_stack.rs` 的「Neither page
        // takes pointer input while the change runs」）；而「有没有一页还开着」只能问手上
        // 那个弱句柄什么时候失效 —— 栈要把出场那一页挂在树上直到退场走完，拿它当判据会把
        // 「回过整架再点进别一组」一起拦掉（试过，就是那样）。
        let stack = self.stack.clone();
        let page = cx.new(|cx| Self::build(window, cx, stack, scope));
        self.group_page = Some(page.downgrade());
        // 那一页与架面同一种格子，报的也是同两件事，而外壳只订阅了根实例这一个 ——
        // 不转这一道，在组里点封面开不了书，删掉开着的那本也不退那一层。
        self._group_watch = Some(cx.subscribe(&page, |this, _, event, cx| match event {
            ShelfEvent::Open(book_id) => cx.emit(ShelfEvent::Open(*book_id)),
            ShelfEvent::Deleted(book_id) => {
                // 删掉一本改的是**两片**：那一页自己已经重查过了，身后的整架还留着那一格。
                this.items.clear();
                let query = this.query.clone();
                this.reload(&query, cx);
                cx.emit(ShelfEvent::Deleted(*book_id));
            }
        }));
        let _ = self
            .stack
            .update(cx, |state, cx| state.push(page, NavMotion::Animated, cx));
    }

    /// 收掉自己这一页，回到上一层。顶栏那颗「返回」与「这一组已经没了」那一条都走这里，
    /// 与「信息」那一页的退场同一手。
    fn pop(&mut self, cx: &mut Context<Self>) {
        let _ = self
            .stack
            .update(cx, |state, cx| state.pop(NavMotion::Animated, cx));
    }

    /// 某一页顶栏那一行叫什么：组名按 id 从库里那份清单现取（在别处改名了跟着变）。
    /// 整架那一屏不走这里 —— 它的顶行是搜索那一行。
    pub(in crate::ui::features::shelf) fn page_title(&self) -> SharedString {
        match self.scope {
            ShelfScope::Shelf => SharedString::default(),
            ShelfScope::Collection(id) => self
                .collections
                .iter()
                .find(|c| c.id == id)
                .map(|c| SharedString::from(c.name.clone()))
                .unwrap_or_default(),
        }
    }

    /// 某一页头顶那一条：返回 + 这一组叫什么 + 几本。
    /// 用的是「信息」那一页同一条 [`TopBar`](crate::ui::components::TopBar)，内缩也对齐
    /// 同一份 [`GUTTER`]。
    ///
    /// 那个本数读的是**这一页已经摆出来的格子**而不是库里那个 `book_count`：一屏上两行字
    /// 说的必须是同一件事，而格子才是读者数得见的那一份。
    pub(in crate::ui::features::shelf) fn page_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let back = cx.entity().clone();
        TopBar::new("shelf-group-bar")
            .title(self.page_title())
            .subtitle(book_count(cx, self.items.len()))
            .strong()
            .back_tooltip(tr(cx, "shelf.back"))
            .show_back_label()
            .pad_x(GUTTER)
            .on_back(move |_, cx| back.update(cx, |this, cx| this.pop(cx)))
            .into_any_element()
    }

    /// 「分享这本书」：把库里那一份副本的字节递给系统的分享面板。
    ///
    /// 递的是**那个文件本身**（`books.path` 指着的一份），不是重画一张带封面的图：读者
    /// 发出去的那一样，对方收到就能直接打开来读。菜单里那一条见 [`book_menu`]，而它只摆得
    /// 给有系统分享面板的那一端（[`crate::ui::platform::HAS_SHARE_SHEET`]）。
    ///
    /// 读文件不在 UI 线程上：一本书是几兆到几十兆，一次 `fs::read` 是看得见的卡，而按完
    /// 按钮的人期待的是「面板过一会儿自己出来」—— 与图片查看器导出那一张同一立场。递给
    /// 系统那一步回主执行器做：Android 那侧走 JNI，只能在界面线程上调。
    /// 读不到字节只写一行日志：一次没分享成不该把架面拖退。
    pub(in crate::ui::features::shelf) fn share_book(
        &mut self,
        book_id: i64,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = &self.store else {
            tracing::error!("ShelfView: 没有书库，分享不了第 {book_id} 本");
            return;
        };
        let row = match store
            .book(book_id)
            .with_context(|| format!("查第 {book_id} 本"))
        {
            Ok(Some(row)) => row,
            Ok(None) => {
                tracing::warn!("分享: 库里没有第 {book_id} 本");
                return;
            }
            Err(err) => {
                report("ShelfView", &err);
                return;
            }
        };
        // `books.format` 是导入时按 `Format::as_str` 写进去的。认不出来的那一条不递：
        // 报一个错的 media type 会让对方那一排应用全都拒收，不如不弹面板。
        let Some(format) = Format::parse(&row.format) else {
            tracing::warn!("分享: 第 {book_id} 本的格式认不出来：{}", row.format);
            return;
        };
        let mime = format.mime();
        let name = share_file_name(&row.title, format);
        let path = std::path::PathBuf::from(&row.path);
        let logged = path.clone();
        let read = cx.background_spawn(async move { std::fs::read(&path).ok() });
        cx.spawn(async move |this, cx| {
            let Some(bytes) = read.await else {
                tracing::warn!("分享: 读不出 {} 的字节", logged.display());
                return;
            };
            let _ = this.update(cx, |_, cx| host::share_file(&bytes, mime, &name, cx));
        })
        .detach();
    }

    /// 删掉一本：库里的行、属于我们自己的那份副本，然后重查一次架面。
    ///
    /// 「哪些文件算我们自己的」这条判据在 [`delete_book`] 里，不在这里：桌面端记的是
    /// 读者手里的原文件，和移动端的副本在库里长得一模一样，只有位置能分开它们，
    /// 而删错的那一头是不可逆的。菜单上那颗「删除」见 [`super::book_menu`]。
    pub(in crate::ui::features::shelf) fn remove(&mut self, book_id: i64, cx: &mut Context<Self>) {
        let Some(store) = &self.store else {
            tracing::error!("ShelfView: 没有书库，删不掉第 {book_id} 本");
            return;
        };
        if let Err(err) = delete_book(
            store,
            book_id,
            target_dir(&ConfigHolder::library(cx)).as_deref(),
        )
        .with_context(|| format!("删第 {book_id} 本"))
        {
            report("ShelfView", &err);
            return;
        }
        self.covers.remove(&book_id);
        cx.emit(ShelfEvent::Deleted(book_id));
        // 清空是为了绕过 `reload` 的去重：搜索词没变，但架面少了一本，必须重查一次。
        self.items.clear();
        let query = self.query.clone();
        self.reload(&query, cx);
    }

    /// 网格本体，或者两种「这里没有东西」—— 它们必须说得不一样。
    ///
    /// 建组 / 链接导入 / 扫描那三块**不在这里**：它们是浮在架面之上的浮层与弹层，
    /// 架面照旧画在底下（见 [`Self::overlays`]）。只有「书库位置」那一屏还是就地换掉整片
    /// —— 它是导入的前置而不是「答一句话」，答完之前那片格子没有意义。
    fn body(&self, cx: &mut Context<Self>, spec: GridSpec, tail: f32) -> AnyElement {
        if self.confirming {
            return self.confirm_panel(cx);
        }
        if !self.items.is_empty() || !self.tiles.is_empty() {
            return self.grid(cx, spec, tail).into_any_element();
        }
        // 空架要说「导一本书进来」，有书但都不匹配要说「换个词」，某一组那一页空了要说
        // 「这一片还没有书」：三件事写成一句，读者会以为自己把书弄丢了。
        let (title, note) = match self.scope {
            ShelfScope::Collection(_) => ("shelf.group_empty", "shelf.group_empty_hint"),
            ShelfScope::Shelf if self.query.is_empty() => ("shelf.empty", "shelf.empty_hint"),
            ShelfScope::Shelf => ("shelf.no_results", "shelf.no_results_hint"),
        };
        empty_state(tr(cx, title), tr(cx, note), cx).into_any_element()
    }

    /// 一片格子：只画看得见的那几排。一格长什么样在 [`super::book_card`]，不在这里。
    ///
    /// 用 GPUI 的 `uniform_list`（等高虚拟列表）而不是把整片格子搭进一个会滚的 div：
    /// 它以「一横排」为项，每帧只渲染视区里那 `Range<usize>` 几排，架上有多少本与每帧
    /// 建出多少张卡就此脱钩。它也不走 taffy 那套完整排版，只量一项的高度按等高排
    /// （所以只适用于等高项）—— 一横排是「封面格 + 一行书名 + 一行作者」，两行文字都
    /// 截断，天生等高。
    ///
    /// 滚动归这个 list 自己（它内部就是 `overflow-y: scroll` + `track_scroll`），所以这里
    /// **不再**套 `Scrollable`：两层滚动会各吃一次同一根手指。滚轴由
    /// `.vertical_scrollbar(&句柄)` 画在这一格的边沿上 —— list 自己故意不画轴。
    /// 句柄存在 [`ShelfView::grid_scroll`] 里，不是每帧现造。
    ///
    /// 排间距烘进每一项（`.py_1()`）而不是靠容器的 `gap`：虚拟列表不在项之间插空隙，
    /// 上下各 4 就是原来 `gap_2` 那 8 的节律，头一排与尾一排也各留一点边。
    ///
    /// `tail` 是悬浮底栏要的那一段（Compact 档，见 `navigation::clearance`）。它加在
    /// list **自己的 padding** 上而不是外壳的容器上：`uniform_list` 的裁剪按 `bounds`
    /// 算、滚动上限按扣掉 padding 的 `padded_bounds` 算，于是滚动途中格子会画进栏底下，
    /// 滚到底时最后一排正好停在栏的上沿 —— 两头都对。
    fn grid(&self, cx: &mut Context<Self>, spec: GridSpec, tail: f32) -> impl IntoElement {
        // 卡片管理菜单的两条文案：整个架面取一次，逐卡 clone 进菜单。条目本身
        // （哪几条、什么图标、点了派给谁）在 [`book_menu`]。
        let labels = book_menu::Labels::resolve(cx);
        let columns = spec.columns as usize;
        // 一横排算一项；不满一排的尾巴也算一排，不然最后那几本会画不出来。
        // 数的是**格子**：组卡与书并成的那一条流（[`Self::cells`]），一张卡占一格。
        let cells = self.cells();
        let rows = cells.len().div_ceil(columns);
        let shelf = cx.entity().clone();
        let scroll = self.grid_scroll.clone();
        let cover_height = spec.cover_height;
        let list = uniform_list("shelf-grid", rows, move |range, window, cx| {
            range
                .map(|row| {
                    let from = row * columns;
                    let to = (from + columns).min(cells.len());
                    // 句柄只在这一排真要画的时候读那几行数据：整片 `items` 不 clone，
                    // 跟着这一屏走的只有那串格子下标。
                    shelf.update(cx, |this, cx| {
                        // 落定的时钟先取，再借 `items`：进度是这一片格子共用的一个
                        // 数，逐格只是起步早晚不同。
                        let enter = this.enter_clock(cells.len(), window, cx);
                        div()
                            .w_full()
                            .grid()
                            .grid_cols(spec.columns)
                            .gap_2()
                            // 左右留白仍归每一排自己：滚轴贴的是这块区域真正的边。
                            .px(px(GUTTER))
                            .py_1()
                            .children((from..to).map(|ix| match cells[ix] {
                                // 这一格是组卡还是书只在那一条流里定一次（[`Self::cells`]），
                                // 这里只按它取那一份 vec。`ix` 传的是**格子**下标 —— 落定的
                                // 早晚按眼睛扫的顺序，不按两段 vec 各自的位置。
                                Cell::Tile(at) => this.group_cell_element(
                                    &this.tiles[at],
                                    cover_height,
                                    &enter,
                                    ix,
                                    cx,
                                ),
                                Cell::Book(at) => this.card_element(
                                    &this.items[at],
                                    &labels,
                                    cover_height,
                                    &enter,
                                    ix,
                                    cx,
                                ),
                            }))
                            .into_any_element()
                    })
                })
                .collect()
        })
        .track_scroll(&scroll)
        .size_full()
        .pb(px(tail));

        div()
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(list)
            .vertical_scrollbar(&scroll)
    }
}

impl EventEmitter<ShelfEvent> for ShelfView {}

impl Render for ShelfView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 导入后的兜底重查：notify 在后台被丢的那一帧，恢复前台的第一帧补上。
        if self.pending_reload {
            self.pending_reload = false;
            self.force_reload(cx);
        }
        // 顶部那一行按窗口档位换：手机上满幅的搜索框是「先搜再翻」，桌面上同一颗框
        // 拉到整片内容宽就成了一块空白，所以那边是定宽搜索框 + 导入的工具栏。
        let size = SizeClass::of(window);
        let top = match self.scope {
            ShelfScope::Shelf => {
                if size.is_compact() {
                    self.search_bar(cx).into_any_element()
                } else {
                    self.toolbar(cx).into_any_element()
                }
            }
            // 某一组那一页不摆搜索行：这一屏要回答的是「这一组都有谁」，而搜书是整架那一片
            // 的命令 —— 那颗框此刻还在身后那一屏上，读者退回就够得着。
            _ => self.page_bar(cx),
        };
        let spec = grid_spec(f32::from(window.viewport_size().width), size);
        // 悬浮底栏压在内容上（Compact 档），滚动内容得自己空出那一段 —— 加在容器的
        // 留白上会把视口切短，格子就再也到不了栏底下。侧栏两档是嵌入的、由外壳让位，
        // 这里不再加。
        let tail = if size.is_compact() {
            navigation::clearance(crate::ui::host::safe_area(window, cx).bottom)
        } else {
            0.0
        };
        // 导入进行中的那一句读数摆在命令行与架面之间：这几秒里架面是不动的，读者要确认
        // 的是「它还在干活」，而不是又看见一片和点导入之前一模一样的格子。
        // `total == 0` 是不定进度（刚提交、第一条读数还没上来），那句不带数字。
        // 旁边一颗「取消」：管线在两本书之间看见旗标，已落库的那些是真的。
        let status = self.importing.map(|(done, total)| {
            let text: SharedString = if total == 0 {
                tr(cx, "shelf.importing")
            } else {
                tf(
                    cx,
                    "shelf.importing_of",
                    &[("done", &done), ("total", &total)],
                )
                .into()
            };
            let entity = cx.entity().clone();
            let cancel_label = tr(cx, "shelf.cancel_import");
            h_flex()
                .id("shelf-importing")
                // 外壳那扇回归门要按 id 认出这一行画没画出来。非 `test-support` 构建里
                // 这一层是透明的（`Observed` 原样转交内部元素），真机不多花一个节点。
                .test_support()
                .px(px(GUTTER))
                .pb_1()
                .gap_2()
                .child(muted_label(text, cx))
                .child(
                    Button::new("shelf-import-cancel")
                        .ghost()
                        .small()
                        .label(cancel_label.clone().to_string())
                        .accessibility_label(cancel_label)
                        .on_click(move |_, _, cx| {
                            entity.update(cx, |this, cx| this.cancel_import(cx))
                        }),
                )
                .into_any_element()
        });
        // 那三块「答一句话」的面板：桌面是一张浮层，手机是同一份内容压进底部弹层。
        self.sync_panels(size.is_compact(), cx);
        let overlays: Vec<AnyElement> = if size.is_compact() {
            // 三块弹层每帧都交元素：弹簧的采样在里面，收稳的那些只是不给内容。
            vec![
                self.group_sheet(cx),
                self.url_sheet(cx),
                self.scan_sheet(cx),
            ]
        } else {
            let mut overlays = Vec::new();
            if self.group_open
                && let Some((_, entry)) = &self.group_entry
            {
                overlays.push(self.group_popover(entry.clone(), cx));
            }
            if self.url_open
                && let Some(entry) = &self.url_entry
            {
                overlays.push(self.url_popover(entry.clone(), cx));
            }
            if self.scan_open && self.scan.is_some() {
                overlays.push(self.scan_popover(cx));
            }
            overlays
        };
        v_flex()
            .id("folio-shelf")
            // 那几张居中的浮层要有一个「内容区」可以居中，而这一格就是内容区：
            // 导航栏在它的左边、不在它里面。
            .relative()
            .size_full()
            .child(top)
            .children(status)
            .child(self.body(cx, spec, tail))
            .children(overlays)
    }
}

#[cfg(test)]
mod tests {
    //! 列数这件事是架面最容易在改宽度时悄悄坏掉的一处，所以纯函数直接钉。

    use std::path::Path;
    use std::sync::Arc;

    use folio_core::{Config, DocMeta, Format, Library, Store};
    use gpui_kit::base::NavStackState;
    use gpui_kit::component::Root;
    use gpui_kit::component::input::InputState;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AnyWindowHandle, AppContext as _, ElementId, Entity, TestAppContext, px, size};

    use super::super::components::book_card::COVER_RATIO;
    use super::ShelfView;
    use super::grid::{GAP, GUTTER, columns_for, grid_spec};
    use super::{ShelfScope, share_file_name};
    use crate::ui::services::config::ConfigHolder;
    use crate::ui::shell::navigation::SizeClass;
    use gpui_kit::base::NavMotion;

    /// 架面一行放几本：手机钉在 3，桌面与平板随窗口宽往上加，任何宽度都不许塌成 0 列。
    ///
    /// 列数写死在 3 上是桌面书架最先坏掉的一处：1600 宽的窗口会给出三格 440 像素的封面，
    /// 一屏只看得见两本书，而「我的书都在哪儿」正是架面要回答的问题。反过来在最小窗口
    /// （520）上按同一公式硬塞，长出来的会是零宽的一格。两种都只在渲染出来之后看得见。
    #[test]
    fn the_shelf_grid_grows_with_the_window() {
        // 手机：386 逻辑像素可用宽上量出来的 3 列，不随窗口继续变。
        assert_eq!(columns_for(390.0, SizeClass::Compact), 3);
        assert_eq!(columns_for(430.0, SizeClass::Compact), 3);

        // 桌面最小窗口（悬浮侧栏档）：两格，宁可小也不许没有。
        assert_eq!(columns_for(520.0, SizeClass::Medium), 2);
        // 每格都不窄于 168：再加一列就要挤破这个下限时，宁可少一列。
        // 侧栏占位宽从 48（图标栏）换成 120（96 卡片 + 两侧 12）之后，776 上由 4 列落到 3。
        assert_eq!(columns_for(776.0, SizeClass::Medium), 3);
        // 嵌入侧栏从 260 收到 96：1600 上反而多出一列。
        assert_eq!(columns_for(1600.0, SizeClass::Expanded), 8);

        // 同一档内加宽不会反而少一列。
        for (width, size) in [
            (600.0, SizeClass::Medium),
            (700.0, SizeClass::Medium),
            (900.0, SizeClass::Expanded),
            (1200.0, SizeClass::Expanded),
            (2000.0, SizeClass::Expanded),
        ] {
            let at = columns_for(width, size);
            assert!(at >= 1, "{width} 宽不该一行放 0 本");
            assert!(
                columns_for(width + 1.0, size) >= at,
                "{width} 处加宽反而少了一列：{at} → {}",
                columns_for(width + 1.0, size)
            );
        }
    }

    /// 一横排的排距只能由几何算出来，不许被格子的内容（书名有多长）带着走。
    ///
    /// `uniform_list` 量行高时给的是 **min-content 宽度**，而书名那一行是不折行的 ——
    /// 它的 min-content 宽就是整条标题。封面高一旦留给排版按宽度自己推（`aspect_ratio`），
    /// 量出来的排距就跟着书名长度涨：真机上那版架面量出 544 逻辑像素（一格实际 236），
    /// 第二排被整个推到屏幕外。这里钉的是「排距只认窗口宽与列数」这一条。
    #[test]
    fn the_row_pitch_ignores_the_book_titles() {
        // 手机三列：格宽 = (390 − 两侧 12 − 两条 8) / 3，封面高 = 格宽 ÷ 2:3。
        let phone = grid_spec(390.0, SizeClass::Compact);
        let card = (390.0 - GUTTER * 2.0 - GAP * 2.0) / 3.0;
        assert_eq!(phone.columns, 3);
        assert!(
            (phone.cover_height - card / COVER_RATIO).abs() < 0.01,
            "封面高该是格宽的 3/2：{} ≠ {}",
            phone.cover_height,
            card / COVER_RATIO
        );
        // 排距要留在「一屏看得见好几排」那一档；544 就是那次真机坏掉的数。
        assert!(
            phone.cover_height < 260.0,
            "手机上一格封面高到 {}, 排距已经不是几何说的算了",
            phone.cover_height
        );

        // 桌面：列数与格宽都跟着窗口走，排距不会停在手机那一档。
        let desk = grid_spec(1600.0, SizeClass::Expanded);
        assert!(
            desk.columns > phone.columns,
            "1600 宽还只排 {} 列",
            desk.columns
        );
        assert!(desk.cover_height > phone.cover_height);
    }

    /// 一本测试书入库，返回它的主键。`Store::add_book` 不碰磁盘：路径只是那一行的身份。
    fn add(store: &Store, title: &str) -> i64 {
        store
            .add_book(
                &Path::new("/books").join(format!("{title}.epub")),
                &DocMeta {
                    title: title.into(),
                    authors: vec!["作者丙".into()],
                    format: Format::Epub,
                    size: 1,
                    ..DocMeta::default()
                },
            )
            .expect("入库")
    }

    /// 起一屏架面：装一个内存书库，加上**确认过位置**的配置（没确认过的时候架面画的是
    /// 那块「书要拷到哪儿」的面板，不是格子）。返回（架面, 窗口, 书库, 架面那条栈）。
    ///
    /// 栈也要交出来：某一组与「信息」都是压在同一条栈上的一页，而这一轮要钉的正是
    /// 「压上去 = 栈深 +1、返回 = 弹回整架」这一头一尾。架面自己也**压上去**当栈底 ——
    /// 外壳给每个目的地压根视图就是这么做的，少这一手测出来的栈深会差一层。
    fn boot_shelf(
        cx: &mut TestAppContext,
    ) -> (
        Entity<ShelfView>,
        AnyWindowHandle,
        Arc<Store>,
        Entity<NavStackState>,
    ) {
        cx.update(gpui_kit::init);
        let mut built = None;
        let handle = cx
            .open_window(size(px(900.), px(600.)), |window, cx| {
                crate::ui::services::app_store::install(Store::in_memory().expect("内存库"), cx);
                ConfigHolder::install(
                    Config {
                        library: Library {
                            dir: "/tmp/folio-books".into(),
                            watch: None,
                        },
                        ..Config::default()
                    },
                    cx,
                );
                let stack = cx.new(|_| NavStackState::new());
                let view = cx.new(|cx| ShelfView::boot(window, cx, stack.downgrade()));
                stack.update(cx, |state, cx| {
                    state.push(view.clone(), NavMotion::Immediate, cx)
                });
                built = Some((view.clone(), stack.clone()));
                Root::new(view, window, cx)
            })
            .into();
        let store = cx
            .update(|cx| crate::ui::services::app_store::handle(cx))
            .expect("书库应已装上");
        let (view, stack) = built.expect("架面应已建起");
        (view, handle, store, stack)
    }

    /// 分组那两处共用同一份库状态：卡片菜单里那颗是**开关**（切进 / 切出），而
    /// 「新建分组…」建完要把这本书顺手挂进去。
    ///
    /// 两条各自都能单独坏掉而编译照样绿：开关写成单向的「加入」→ 点第二下什么也不发生；
    /// 新建只建组不挂书 → 组有了、书还在外面；空名那一趟把面板清了 → 读者打了一半的字
    /// 凭空消失。
    #[gpui_kit::test]
    fn the_group_switch_and_the_new_group_agree(cx: &mut TestAppContext) {
        let (shelf, handle, store, _) = boot_shelf(cx);
        let a = add(&store, "甲");
        let b = add(&store, "乙");
        let group = store.create_collection("想读").expect("建组");
        store.add_to_collection(group, a).expect("挂进去");

        shelf.update(cx, |this, cx| this.force_reload(cx));
        assert_eq!(
            shelf.read_with(cx, |this, _| this.card_count()),
            1,
            "甲已经进了组，整架那一片只剩没名分的乙"
        );
        assert_eq!(
            shelf.read_with(cx, |this, _| this.card_title(0)),
            Some("乙".into()),
            "那一格该是乙"
        );

        // 菜单里那一节的名分：一条组一行，甲打了勾。
        let rows = shelf.read_with(cx, |this, _| this.group_items(a));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, group, "行的 id 就是库里那条组");
        assert!(rows[0].2, "甲已经在「想读」里");
        assert!(
            !shelf.read_with(cx, |this, _| this.group_items(b))[0].2,
            "乙不在"
        );

        // 那颗开关两头都要动，而且要动到架面上：一进一出，那一格跟着消失、跟着回来。
        shelf.update(cx, |this, cx| this.toggle_group(b, group, cx));
        assert_eq!(
            store.collections_of_book(b).expect("读库").len(),
            1,
            "不在里面时点一下是加入"
        );
        assert_eq!(
            shelf.read_with(cx, |this, _| this.card_count()),
            0,
            "乙一进组，整架那一片就没有书了：两本都收在组里"
        );
        shelf.update(cx, |this, cx| this.toggle_group(b, group, cx));
        assert!(
            store.collections_of_book(b).expect("读库").is_empty(),
            "已经在里面时同一下是移出"
        );
        assert_eq!(
            shelf.read_with(cx, |this, _| this.card_title(0)),
            Some("乙".into()),
            "移出来的那本要回到整架那一片"
        );

        // 「新建分组…」：面板上那颗「建组」建完就把这本书挂进去，面板随后收掉。
        cx.update_window(handle, |_, window, cx| {
            let entry = cx.new(|cx| InputState::new(window, cx).default_value("杂项"));
            shelf.update(cx, |this, cx| {
                this.group_entry = Some((a, entry.clone()));
                this.create_group(cx);
            });
        })
        .expect("窗口应可更新");
        let groups = store.collections().expect("读库");
        assert!(
            groups.iter().any(|c| c.name == "杂项" && c.book_count == 1),
            "建的组要当场把这本书装进去：{}",
            groups.len()
        );
        assert!(
            shelf.read_with(cx, |this, _| this.group_entry.is_none()),
            "建完了面板该收"
        );

        // 空名那一趟：存储层报 `EmptyCollectionName`，面板必须留着让读者接着改。
        cx.update_window(handle, |_, window, cx| {
            let blank = cx.new(|cx| InputState::new(window, cx));
            shelf.update(cx, |this, cx| {
                this.group_entry = Some((a, blank));
                this.create_group(cx);
            });
        })
        .expect("窗口应可更新");
        assert!(
            shelf.read_with(cx, |this, _| this.group_entry.is_some()),
            "没建成不该把面板和他打了一半的字一起清掉"
        );
    }

    /// 组是**收纳**：一本挂进组就从整架那一片消失，可顶部那颗框查的仍是全库，词一清空它
    /// 又藏回那一组里。
    ///
    /// 两句都能单独坏掉而另一句照样绿：只改取数不改搜索，进过组的书在这台机器上再也搜不
    /// 出来（某一组那一页没有搜索行，那里摆的是组名不是框），读者读到的是「书丢了」；
    /// 反过来若整架那一片仍旧全查，组就只是一个别名而不收纳。
    #[gpui_kit::test]
    fn a_grouped_book_leaves_the_shelf_but_a_query_still_finds_it(cx: &mut TestAppContext) {
        let (shelf, _, store, _) = boot_shelf(cx);
        let a = add(&store, "甲");
        add(&store, "乙");
        let group = store.create_collection("想读").expect("建组");
        store.add_to_collection(group, a).expect("挂进去");
        shelf.update(cx, |this, cx| this.force_reload(cx));
        assert_eq!(
            shelf.read_with(cx, |this, _| this.card_count()),
            1,
            "甲进了组，整架那一片该少一格"
        );
        assert_eq!(
            shelf.read_with(cx, |this, _| this.card_title(0)),
            Some("乙".into()),
            "整架那一片只摆还没进组的"
        );

        shelf.update(cx, |this, cx| this.reload("甲", cx));
        assert_eq!(
            shelf.read_with(cx, |this, _| this.card_count()),
            1,
            "进了组不许等于搜不到"
        );
        assert_eq!(
            shelf.read_with(cx, |this, _| this.card_title(0)),
            Some("甲".into()),
            "搜出来的那一本要认得"
        );

        shelf.update(cx, |this, cx| this.reload("", cx));
        assert_eq!(
            shelf.read_with(cx, |this, _| this.card_count()),
            1,
            "词一清空，甲那一格要收回去"
        );
        assert_eq!(
            shelf.read_with(cx, |this, _| this.card_title(0)),
            Some("乙".into()),
            "词一清空，甲就又藏回那一组"
        );
        assert_eq!(
            shelf.read_with(cx, |this, _| this.tile_count()),
            1,
            "而它的去处就是那一张组卡"
        );
    }

    /// 点开**分组里**的那本书：那本书不在整架那一片（`Store::shelf_ungrouped` 收掉了它），
    /// 所以顶到第 1 格的必须是它所在的那张组卡；再点开一本没分组的书，它自己回到第 1 格，
    /// 那张组卡退到第 2 格而不是掉回末尾。尺只有一把 —— 谁最近被翻开谁的卡就在先，
    /// 与它是书还是组无关（读者报的「点了组里的书，组没排到最前面」正是这一处）。
    #[gpui_kit::test]
    fn the_cell_that_leads_is_whatever_was_opened_last(cx: &mut TestAppContext) {
        let (shelf, _, store, _) = boot_shelf(cx);
        let grouped = add(&store, "组里");
        let loose = add(&store, "散着");
        let team = store.create_collection("想读").expect("建组");
        store.add_to_collection(team, grouped).expect("挂进去");
        let misc = store.create_collection("杂项").expect("建组");
        store
            .add_to_collection(misc, add(&store, "另一组"))
            .expect("挂进去");
        shelf.update(cx, |this, cx| this.force_reload(cx));
        assert_eq!(
            shelf.read_with(cx, |this, _| this.cell_label(0)),
            Some("散着".into()),
            "谁都没翻开过：整架那一片有书，组卡只能跟在后面"
        );

        store.touch_opened(grouped).expect("翻开组里那本");
        shelf.update(cx, |this, cx| this.force_reload(cx));
        assert_eq!(
            shelf.read_with(cx, |this, _| this.cell_label(0)),
            Some("想读".into()),
            "点开的是组里那本 → 顶到第 1 格的是那张组卡"
        );

        store.touch_opened(loose).expect("再翻开散着那本");
        shelf.update(cx, |this, cx| this.force_reload(cx));
        assert_eq!(
            shelf.read_with(cx, |this, _| this.cell_label(0)),
            Some("散着".into()),
            "再点开一本没分组的书 → 它自己回到第 1 格"
        );
        assert_eq!(
            shelf.read_with(cx, |this, _| this.cell_label(1)),
            Some("想读".into()),
            "刚才那张组卡退到第 2 格，而不是掉回末尾"
        );
    }

    /// 组卡只在「整架 + 没搜词」那一屏出现（插在流里的哪一格看谁最近被翻开），而那一屏
    /// 的书格子只有还没进组的；点一张组卡 = 在架面那条栈上**压一整页**，身后那一片原样不动。
    ///
    /// 三条各自都能单独坏掉而编译照样绿：某一组那一页还摆一排组卡，等于把读者刚走进去的那层
    /// 又摊回他面前；带着搜索词还摆，卡上那几本封面与搜索结果毫无关系，读起来就是「这几本
    /// 匹配」；而 `reload` 的去重按「词没变且架子非空」判，换片不重排的话那一排卡会留在原地。
    #[gpui_kit::test]
    fn the_group_tiles_share_the_shelf_line_and_step_aside(cx: &mut TestAppContext) {
        let (shelf, handle, store, stack) = boot_shelf(cx);
        let a = add(&store, "甲");
        let b = add(&store, "乙");
        let c = add(&store, "丙");
        add(&store, "丁");
        let want = store.create_collection("想读").expect("建组");
        let misc = store.create_collection("杂项").expect("建组");
        store.add_to_collection(want, a).expect("挂进去");
        store.add_to_collection(want, b).expect("挂进去");
        store.add_to_collection(misc, c).expect("挂进去");

        shelf.update(cx, |this, cx| this.force_reload(cx));
        assert_eq!(
            shelf.read_with(cx, |this, _| this.tile_count()),
            2,
            "整架那一屏两张组卡"
        );
        // 组卡与书各一份 vec，所以「第 0 格」与「第 0 本」不是一回事（并法在 [`merge_cells`]）：`items` 只数书，
        // 而这一片只数**还没进组的**那些本 —— 甲乙丙三本都收在组里了。
        assert_eq!(
            shelf.read_with(cx, |this, _| this.card_count()),
            1,
            "组卡不占书的格子数，进了组的书也不占这一片"
        );
        let tiles = shelf.read_with(cx, |this, _| {
            (0..this.tile_count())
                .map(|ix| this.tile_at(ix).expect("第 {ix} 张在"))
                .collect::<Vec<_>>()
        });
        assert_eq!(
            tiles,
            vec![(want, "想读".into(), 2), (misc, "杂项".into(), 1)],
            "两张卡跟着库里那条序：这一轮谁都没翻开过，于是落到组名（`ORDER BY c.name` 是字节序）"
        );

        // 点一张组卡 = 压一页：那一页里没有组卡，摆的是这一组的书，身后的整架一次库都没重查。
        cx.update_window(handle, |_, window, cx| {
            shelf.update(cx, |this, cx| {
                this.open_page(ShelfScope::Collection(want), window, cx)
            });
        })
        .expect("窗口应可更新");
        assert_eq!(
            stack.read_with(cx, |state, _| state.depth()),
            2,
            "某一组是栈上的一页，不是架面换个筛选"
        );
        let page = shelf
            .read_with(cx, |this, _| {
                this.group_page.as_ref().and_then(|p| p.upgrade())
            })
            .expect("那一页应已挂上");
        assert_eq!(
            page.read_with(cx, |this, _| this.tile_count()),
            0,
            "走进那一组时不许把那排组卡又摊回面前"
        );
        assert_eq!(page.read_with(cx, |this, _| this.card_count()), 2);
        assert_eq!(
            shelf.read_with(cx, |this, _| this.tile_count()),
            2,
            "身后的整架还在原样"
        );

        // 弹回整架再搜一个词：卡还是不摆。
        drop(page);
        stack.update(cx, |state, cx| state.pop(NavMotion::Immediate, cx));
        shelf.update(cx, |this, cx| this.reload("甲", cx));
        assert_eq!(
            shelf.read_with(cx, |this, _| this.tile_count()),
            0,
            "带着搜索词还摆组卡，读起来就是「这几本匹配」"
        );
    }

    /// 站在某一组那一页里按返回，落点是**书架**：栈深从 2 回到 1，那一页没了，还能再点进去。
    ///
    /// 这一条钉的是这轮改动买的东西本身。旧写法里某一组是架面上的一格筛选状态，那条栈的
    /// 栈深自始至终是 1 —— 手机那一端 `host::back_exits_app` 是真，于是在组里按返回 = 退出
    /// 应用，读者报的「在分组内返回直接返回系统主界面」就是这一条。
    #[gpui_kit::test]
    fn a_group_opens_as_a_page_and_back_returns_to_the_shelf(cx: &mut TestAppContext) {
        let (shelf, handle, store, stack) = boot_shelf(cx);
        let a = add(&store, "甲");
        add(&store, "乙");
        let group = store.create_collection("想读").expect("建组");
        store.add_to_collection(group, a).expect("挂进去");
        shelf.update(cx, |this, cx| this.force_reload(cx));
        assert_eq!(
            stack.read_with(cx, |state, _| state.depth()),
            1,
            "前置：栈底就是整架"
        );

        cx.update_window(handle, |_, window, cx| {
            shelf.update(cx, |this, cx| {
                this.open_page(ShelfScope::Collection(group), window, cx)
            });
        })
        .expect("窗口应可更新");
        let page = shelf
            .read_with(cx, |this, _| {
                this.group_page.as_ref().and_then(|p| p.upgrade())
            })
            .expect("那一页应已挂上");
        assert_eq!(page.read_with(cx, |this, _| this.card_count()), 1);
        assert_eq!(
            page.read_with(cx, |this, _| this.card_title(0)),
            Some("甲".into()),
            "那一页摆的该是组里的那一本"
        );
        assert_eq!(
            page.read_with(cx, |this, _| this.page_title().to_string()),
            "想读",
            "顶栏那一行叫的就是组名"
        );
        assert_eq!(
            shelf.read_with(cx, |this, _| this.card_count()),
            1,
            "身后的整架没被换掉，也没重查（甲进了组，那一片只剩乙）"
        );

        // 返回：弹掉那一页，栈深回 1。
        stack.update(cx, |state, cx| state.pop(NavMotion::Animated, cx));
        drop(page);
        assert_eq!(
            stack.read_with(cx, |state, _| state.depth()),
            1,
            "返回弹掉那一页，落回整架"
        );

        // 回过整架之后还要能再点进同一组 —— 根实例手上那个弱句柄不许成为守卫。
        cx.update_window(handle, |_, window, cx| {
            shelf.update(cx, |this, cx| {
                this.open_page(ShelfScope::Collection(group), window, cx)
            });
        })
        .expect("窗口应可更新");
        assert_eq!(
            stack.read_with(cx, |state, _| state.depth()),
            2,
            "回到整架之后再点进同一组，还得进得去"
        );
    }

    /// 某一组那一页对应的组被在别处删掉时，它自己弹回上一层，不摆一屏看着像坏掉的空白。
    #[gpui_kit::test]
    fn a_vanished_group_pops_its_page(cx: &mut TestAppContext) {
        let (shelf, handle, store, stack) = boot_shelf(cx);
        let a = add(&store, "甲");
        add(&store, "乙");
        let group = store.create_collection("想读").expect("建组");
        store.add_to_collection(group, a).expect("挂进去");
        shelf.update(cx, |this, cx| this.force_reload(cx));

        // 点进某一组：那一组被在别处删掉 → 重查这一次就是它退场的时候。
        cx.update_window(handle, |_, window, cx| {
            shelf.update(cx, |this, cx| {
                this.open_page(ShelfScope::Collection(group), window, cx)
            });
        })
        .expect("窗口应可更新");
        assert_eq!(stack.read_with(cx, |state, _| state.depth()), 2);
        store.delete_collection(group).expect("删组");
        // 读者在那一页上做点什么（挪书、删书）都会撞上一次重查，那正是发现组没了的时刻。
        shelf.update(cx, |this, cx| this.force_reload(cx));
        assert_eq!(
            stack.read_with(cx, |state, _| state.depth()),
            1,
            "组没了，那一页自己弹掉，回到它压上来的那一片"
        );
    }

    /// 建组那一屏是**浮在**架面上的：它开着的时候，底下那张格子还在原地、还点得着。
    ///
    /// 迁浮层之前它是就地换掉整片格子的，所以这一条钉的是这次改动的正面：`body` 不再
    /// 为它让位。浮层自己晚一帧才挂进弹窗层（要先量一次触发器），所以断言前连画两帧。
    #[gpui_kit::test]
    fn the_group_form_floats_over_the_grid(cx: &mut TestAppContext) {
        let (shelf, handle, store, _) = boot_shelf(cx);
        let a = add(&store, "甲");
        shelf.update(cx, |this, cx| this.force_reload(cx));

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("shelf-group-panel").is_none(),
                "没开那一屏时不该有表单"
            );
            shelf.update(cx, |this, cx| this.new_group(a, window, cx));
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(
                window.try_find("shelf-group-panel").is_some(),
                "点开「新建分组」要浮起那张卡"
            );
            assert!(
                window
                    .try_find(ElementId::Name(format!("shelf-cover-{a}").into()))
                    .is_some(),
                "卡浮在架面上，不是把整片格子换掉"
            );
            shelf.update(cx, |this, cx| this.close_group_panel(cx));
            window.render_frame(cx);
            assert!(
                window.try_find("shelf-group-panel").is_none(),
                "关掉之后就没了：桌面那张浮层没有退场动画"
            );
        })
        .expect("窗口应可更新");
    }

    /// 「分享这本书」递出去的文件名：书名过一遍那道过滤，后缀问 `Format::copy_extension`
    /// （与库里那份副本用的是同一张表）。钉的是两条会静默坏掉的形状 —— 书名里带 `/` 时
    /// 那串会被收件那侧当成路径成分；后缀与容器格式对不上时，对方那一排应用直接拒开。
    #[test]
    fn a_shared_book_is_named_by_its_title_and_its_own_extension() {
        assert_eq!(
            share_file_name("三体: 地球往事", Format::Epub),
            "三体地球往事.epub"
        );
        assert_eq!(share_file_name("a/b/../c", Format::Mobi), "abc.mobi");
        assert_eq!(share_file_name("   ", Format::Cbz), "folio.cbz");
    }
}
