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

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use folio_core::net::remote::Transport;
use folio_core::store::Collection;
use folio_core::{
    Cancel, ImportOutcome, ImportReport, Imported, PathsSource, Store, TaskEvent, UrlSource,
    submit_import,
};
use gpui_kit::assets::IconName;
use gpui_kit::base::NavStackState;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme, Selectable, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FontWeight, Image,
    InteractiveElement, IntoElement, ParentElement, PathPromptOptions, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, TestSupportExt as _,
    UniformListScrollHandle, WeakEntity, Window, div, px, uniform_list,
};

use super::book_card::{COVER_RATIO, Card, GroupCard, TILE_SLOTS};
use super::book_menu;
use super::overlay;
use super::scan::ScanPhase;
use crate::ui::shared::config_model::ConfigHolder;
use crate::ui::shared::error::{Context as _, report};
use crate::ui::shared::l10n::{tf, tr};
use crate::ui::shared::library::{delete_book, pick_dir, suggested_dir, target_dir};
use crate::ui::shared::services;
use crate::ui::shell::navigation::{RAIL_WIDTH, SIDEBAR_WIDTH, SizeClass};
use crate::ui::widgets::{
    BottomSheetEvent, BottomSheetState, SearchBox, SearchBoxEvent, empty_state, muted_label,
};

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
const COLUMNS: u16 = 3;

/// 网格间距，与 `grid` 的 `gap_2` 同一档（默认 rem 下 8 逻辑像素）。
const GAP: f32 = 8.0;

/// 桌面上一格封面的最小宽度（逻辑像素）。
///
/// 比手机的 112 宽：鼠标桌面上书名要一眼读完，而不是靠点进去看，而一格多宽，
/// 标题那一行就能塞下几个字。再往下加一列会开始把长书名截成看不懂的半句。
const MIN_CARD: f32 = 168.0;

/// 网格与搜索行两侧的留白（逻辑像素）。和顶部那条章导航的 `px_3` 不同一档，
/// 因为这里是整片卡片的边，视觉上要比一条文字栏松。
pub(super) const GUTTER: f32 = 12.0;

/// 一横排的量：几列，以及一格封面该多高。
///
/// 封面高必须由格宽**在这里算出来**，不能留给 `aspect_ratio` 在排版时按宽度推：
/// `uniform_list` 量行高时给的是 **min-content 宽度**（`measure_item(None, ..)`），
/// 而书名那一行是 `whitespace_nowrap` —— 它的 min-content 宽就是整条不折行的标题宽。
/// 于是量出来的那一格有 300 多逻辑像素宽，封面按 2:3 推出 500 多的高，行高比真实的
/// 两还多，第二排直接被推到屏幕外（真机 1080×2400 实测：量出 544，实际 236）。
/// 写死高度让行高与测量时给的宽度无关，这个坑就填上了。
#[derive(Clone, Copy)]
pub(super) struct GridSpec {
    pub(super) columns: u16,
    pub(super) cover_height: f32,
}

/// 网格真正能用到的宽度：扣掉导航栏与两侧留白。
///
/// `columns_for` 与 [`grid_spec`] 都从这一个式子取数：两处各算一份的话，改一侧忘另一侧
/// 会表现为「列数对了但每格宽得莫名其妙」。
fn available_width(width: f32, size: SizeClass) -> f32 {
    let nav = match size {
        SizeClass::Compact => 0.0,
        SizeClass::Medium => RAIL_WIDTH,
        SizeClass::Expanded => SIDEBAR_WIDTH,
    };
    (width - nav - GUTTER * 2.0).max(0.0)
}

/// 一横排该排成什么样：列数按 [`columns_for`]，封面高按那个列数均出来的格宽。
fn grid_spec(width: f32, size: SizeClass) -> GridSpec {
    let columns = columns_for(width, size);
    // n 格之间只有 n−1 条间距，所以只扣一条 `GAP * (n−1)`。
    let card = (available_width(width, size) - GAP * f32::from(columns - 1)) / f32::from(columns);
    GridSpec {
        columns,
        cover_height: card / COVER_RATIO,
    }
}

/// 一行放几本：手机固定 [`COLUMNS`]，桌面与平板按**内容区实际宽度**排到每格
/// 不低于 [`MIN_CARD`]。
///
/// 3 列写死在桌面上是不成立的：1600 宽的窗口会给出三格 440 像素的封面，
/// 一屏只看得见两本书 —— 而「我的书都在哪儿」正是架面要回答的问题。
///
/// 导航栏的宽度从 `shell::navigation` 取：那两个常数是阅读页预留版心用的同一份，
/// 在这里再写一遍数字的话，改一侧忘另一侧只会表现为「书架的格子宽得莫名其妙」。
///
/// `pub` 只为测试能钉住这三档的列数 —— 列数算错不会报错，只会让架面在某个窗口宽上
/// 突然变成一格一本。
pub fn columns_for(width: f32, size: SizeClass) -> u16 {
    if size.is_compact() {
        return COLUMNS;
    }
    // 第 n 格要占 n 个格宽加 n−1 条间距，所以分子补回一条 GAP。
    let fitted = ((available_width(width, size) + GAP) / (MIN_CARD + GAP)).floor() as u16;
    fitted.max(1)
}

/// 书架向外层报的两件事，都带书库主键而不是路径 —— 选择器给的那份路径是临时的，
/// 书的内容早就在库里了。
/// 架面顶部那颗分组筛选看的是哪一片。
///
/// `Ungrouped` 是**筛**出来的、不是存出来的：库里没有一个叫这名字的组，它是
/// 「不在任何 `collection_books` 行里」那一批（`Store::shelf_ungrouped`）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum GroupFilter {
    /// 整架：与「读者还没建过任何组」时看到的是同一片。
    All,
    /// 还没进任何组的那一批。
    Ungrouped,
    /// 某一个组里的书。
    Collection(i64),
}

pub enum ShelfEvent {
    /// 点了一张卡片。
    Open(i64),
    /// 删掉了一本。外壳需要这条：正文可能正开着这一本，书没了，那一层就该退回架面。
    Deleted(i64),
}

pub struct ShelfView {
    /// 书库句柄，来自 [`crate::ui::shared::app_store`]。放 `None` 时架面是空的，
    /// 但界面照常起得来。
    ///
    /// `pub(super)` 只为 [`super::book_info`] 那一页：它要在点开的时刻自己查一次库。
    pub(super) store: Option<Arc<Store>>,
    /// 书架这一目的地的导航栈（外壳建的，见 `AppShell::boot`）。
    ///
    /// 「信息」那一页由架面自己压上去 —— 与阅读页把设置屏压进同一条栈是同一条路。
    /// 弱句柄：栈是外壳的东西，架面不该反过来把它拽住不让它退。
    pub(super) stack: WeakEntity<NavStackState>,
    items: Vec<Card>,
    /// 架面最前面那几张组卡。只在「整架 + 没搜词」时非空，其余时候是空的。
    ///
    /// 与 [`Self::items`] 分开放而不是并成一个列表：`items` 的格数有好几处按「几本」来读
    /// （回归门数的是书，不是格子），混在一起会让那个数悄悄变成「书 + 组」。
    /// 排格子时两组才接成一条下标（见 [`Self::grid`]）。
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
    /// 架面此刻筛的是哪一片。默认整架。
    group: GroupFilter,
    /// 库里的全部组（带书数）。顶部那颗筛选的条目读这一份，与架面同一次重查更新。
    collections: Vec<Collection>,
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
    pub(super) confirming: bool,
    /// 封面解码结果，按书的主键存着。
    ///
    /// 键用 `id` 而不是每次现解：改一个搜索词会把同一本书再查出来一遍，
    /// 没缓存的话每敲一个字都要把整个架面的 JPEG 重解一遍。
    /// `None` 也要存 —— 不记住这次失败，每查一次都会再试一遍同样的失败。
    ///
    /// `pub(super)` 只为 [`super::book_card`] 那一格：它按需解码，缓存得跟着架面活。
    pub(super) covers: HashMap<i64, Option<Arc<Image>>>,
    _search_watch: Subscription,
    /// 导入那条异步链：等选择器 → 把选中的文件交给后端管线 → 收事件直到这批结束。
    /// Task 一 drop 链就断，于是「选了文件但界面已经忘了」；而断在管线跑到一半时，
    /// 后台那份拷贝照旧跑完、结果却没人登记 —— 架上少一本，书库目录里多一份副本。
    /// 所以这条链要活到整批导完。
    _import: Option<Task<()>>,
    /// 正在进行的那批导入的读数：`(已完成, 总数)`。`total == 0` 是不定进度 ——
    /// 刚提交、管线第一条读数还没上来时就是这个样子；`None` 表示没在导入。
    /// 读数来自后端报的 `Progress` 事件，画在哪一处见 `Render` 那一段。
    pub(super) importing: Option<(u64, u64)>,
    /// 正在进行的这批导入的取消旗标，来自 [`TaskHandle`](folio_core::infra::task::TaskHandle)。
    /// 读数行旁那颗「取消」按钮置位它；管线在两本书之间看见，已落库的那些是真的
    /// （`Cancelled` 事件来了重查一次架面就有）。
    import_cancel: Option<Cancel>,
    /// 整机扫描那一屏的意图（内容在 [`Self::scan`]）。见 [`Self::group_open`] 那一段为什么分两半。
    pub(super) scan_open: bool,
    /// 整机扫描那一屏的状态：正在扫 / 已出清单。`None` = 这一屏没东西可画了。
    /// 面板本体与全部动作在 [`super::scan`]。
    pub(super) scan: Option<ScanPhase>,
    /// 扫描那条异步链（事件桥）。与 `_import` 同理，要活到整轮结束。
    pub(super) _scan: Option<Task<()>>,
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
    /// 压上栈的那页「信息」的订阅。`Some` = 那一页还开着。
    ///
    /// 只要一条：那一页写回书名作者之后报一声，架面重查 —— 那一行字就是这张卡上的。
    /// 每压一页换掉它，上一页的订阅跟着 drop（`Subscription` 一没就退订）。
    pub(super) _info_watch: Option<Subscription>,
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
    /// `pub(super)` 只为 [`super::book_card::ShelfView::enter_clock`] 那一处：动效的
    /// 那几条规矩都收在卡片文件里，时钟也得由它自己拨。
    pub(super) enter_at: Option<Instant>,
}

impl ShelfView {
    pub fn boot(
        window: &mut Window,
        cx: &mut Context<Self>,
        stack: WeakEntity<NavStackState>,
    ) -> Self {
        let store = crate::ui::shared::app_store::handle(cx);
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
        // 自动导入（监听、传书）的事件流。处理与手工导入完全同一套。
        let auto_events = crate::ui::shared::import_queue::auto_import_events();
        let _auto_import = cx.spawn(async move |this: WeakEntity<Self>, cx| {
            while let Ok(event) = auto_events.recv().await {
                if this
                    .update(cx, |this, cx| this.import_event(event, cx))
                    .is_err()
                {
                    return; // 架面没了，订阅跟着收。
                }
            }
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
            group: GroupFilter::All,
            collections: Vec::new(),
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
            _auto_import: Some(_auto_import),
            url_entry: None,
            url_open: false,
            group_sheet,
            url_sheet,
            scan_sheet,
            _sheet_watches,
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
        // 组清单与架面同一次重查：那颗筛选的条目与卡片菜单读的都是这一份。
        self.collections = self
            .store
            .as_ref()
            .and_then(|store| store.collections().ok())
            .unwrap_or_default();
        // 筛着的那个组已经没了（在别处删掉的）：退回整架，而不是给一屏看着像坏掉的空白。
        if let GroupFilter::Collection(id) = self.group
            && !self.collections.iter().any(|c| c.id == id)
        {
            self.group = GroupFilter::All;
        }
        // 组卡只在「整架 + 没搜词」那一屏出现。筛到某一组时再摆一排组卡，等于把读者刚
        // 走进去的那一层又摊回他面前；而搜的是书不是组 —— 词非空时那几张卡上的封面跟
        // 搜索结果没有半点关系，摆出来只会读成「这几本匹配」。
        self.tiles = if matches!(self.group, GroupFilter::All) && query.trim().is_empty() {
            self.tile_cards()
        } else {
            Vec::new()
        };
        let queried = self.store.as_ref().map(|store| match self.group {
            GroupFilter::All => store.shelf(query),
            GroupFilter::Ungrouped => store.shelf_ungrouped(query),
            GroupFilter::Collection(id) => store.shelf_in_collection(id, query),
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
    }

    /// 架面顶部那几张组卡：每个组一张，卡上是这个组最后丢进去的那几本的封面。
    ///
    /// 封面走的是 [`Self::resolve_cover`] 那一条，与书卡同一份缓存：组卡与下面那片书
    /// 十有八九是同一批封面，解两遍是白解。
    ///
    /// 查不动就是一片空的：架面顶部少一排卡看着像「还没建组」，比摆几张假卡或整屏报错好。
    fn tile_cards(&mut self) -> Vec<GroupCard> {
        let Some(store) = &self.store else {
            return Vec::new();
        };
        let Ok(tiles) = store.collection_tiles(TILE_SLOTS) else {
            tracing::error!("ShelfView: 查组卡失败，架面顶部那一排是空的");
            return Vec::new();
        };
        tiles
            .into_iter()
            .map(|(collection, items)| {
                let covers = items
                    .iter()
                    .map(|item| self.resolve_cover(item))
                    .collect();
                GroupCard {
                    id: collection.id,
                    name: collection.name.into(),
                    count: collection.book_count,
                    covers,
                }
            })
            .collect()
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
    pub(super) fn dispatch_import(
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
        let Some(store) = crate::ui::shared::app_store::handle(cx) else {
            tracing::error!("ShelfView: 书库没装上，这一批没有导入");
            return;
        };
        // 拷贝、解析、落库都在后端管线里逐本完成（`folio_core::ingest::import`）。
        let handle = submit_import(&services::pool(), source, dir, store);
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

    /// 架面顶部此刻摆着几张组卡。只给回归门读，理由同 [`Self::card_count`]。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn tile_count(&self) -> usize {
        self.tiles.len()
    }

    /// 第 `ix` 张组卡认的是哪个组、下面那行念的是几本。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn tile_at(&self, ix: usize) -> Option<(i64, SharedString, usize)> {
        self.tiles
            .get(ix)
            .map(|t| (t.id, t.name.clone(), t.count))
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
                self.items.clear();
                let query = self.query.clone();
                self.reload(&query, cx);
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
        // 新入库的书要立刻上架，但搜索词得留着：读者可能正搜到一半。
        // `reload` 的去重按「词没变且架面非空」判，所以先把它清空逼它重查。
        self.items.clear();
        let query = self.query.clone();
        self.reload(&query, cx);
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

    /// 「从链接导入」那一屏的内容。与建组那一屏同一形状，也只有容器不同。
    fn url_form(&self, entry: Entity<InputState>, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity().clone();
        let close_entity = entity.clone();
        let import_entity = entity.clone();
        let cancel_label = tr(cx, "shelf.cancel");
        let import_label = tr(cx, "shelf.url_import");
        v_flex()
            .id("shelf-url-panel")
            .test_support()
            .w_full()
            .gap_3()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr(cx, "shelf.url_title")),
            )
            .child(muted_label(tr(cx, "shelf.url_hint"), cx))
            .child(div().w_full().child(Input::new(&entry).cleanable(true)))
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        Button::new("shelf-url-cancel")
                            .ghost()
                            .label(cancel_label.to_string())
                            .accessibility_label(cancel_label)
                            .on_click(move |_, _, cx| {
                                close_entity.update(cx, |this, cx| this.close_url_panel(cx))
                            }),
                    )
                    .child(
                        Button::new("shelf-url-import")
                            .primary()
                            .label(import_label.to_string())
                            .accessibility_label(import_label)
                            .on_click(move |_, _, cx| {
                                import_entity.update(cx, |this, cx| this.import_from_url(cx))
                            }),
                    ),
            )
            .into_any_element()
    }

    /// 「从链接导入」在桌面那一档：屏幕居中的一张浮层。
    ///
    /// 居中而不是钉在 ⋮ 那颗按钮底下：这一屏的入口在那条菜单里，而菜单点开这张卡的
    /// 同一帧就散了 —— 钉在一个刚消失的元素上，读者看到的是卡从半空里长出来。
    fn url_popover(&self, entry: Entity<InputState>, cx: &mut Context<Self>) -> AnyElement {
        let shelf = cx.entity().clone();
        let body = self.url_form(entry, cx);
        overlay::centered(
            "shelf-url-popover",
            overlay::URL_HEIGHT,
            &shelf,
            |this, cx| this.close_url_panel(cx),
            div()
                .w(px(overlay::URL_WIDTH))
                .min_h(px(overlay::URL_HEIGHT))
                .child(body)
                .into_any_element(),
        )
    }

    /// 顶部那颗分组按钮，外加它底下那张浮层（开着的时候）。
    ///
    /// 浮层挂在按钮**之后**而不是包着那颗按钮：`Popover` 要拿一个元素的 bounds 当锚点，
    /// 而那颗按钮自己已经挂着一条筛选菜单 —— 两层浮层抢同一个触发器会互相顶掉。
    /// 外面包一层 `div` 而不是把它塞成 `h_flex` 的下一个孩子：那样那两颗按钮之间会多算
    /// 一条 `gap_2`，而浮层是个零尺寸的东西。
    fn group_cell(&self, cx: &mut Context<Self>) -> AnyElement {
        let button = self.group_button(cx).into_any_element();
        match (&self.group_entry, self.group_open) {
            (Some((_, entry)), true) => div()
                .child(button)
                .child(self.group_popover(entry.clone(), cx))
                .into_any_element(),
            _ => button,
        }
    }

    /// 建组那一屏在手机上：同一份表单压进底部弹层。
    fn group_sheet(&self, cx: &mut Context<Self>) -> AnyElement {
        let body = self.form_body(&self.group_sheet, self.group_entry.as_ref().map(|(_, e)| e), cx, |this, entry, cx| {
            this.group_form(entry, cx)
        });
        overlay::sheet(
            "shelf-group-sheet",
            &self.group_sheet,
            overlay::SHEET_EXPANDED,
            body,
        )
    }

    /// 链接导入那一屏在手机上。
    fn url_sheet(&self, cx: &mut Context<Self>) -> AnyElement {
        let body = self.form_body(&self.url_sheet, self.url_entry.as_ref(), cx, |this, entry, cx| {
            this.url_form(entry, cx)
        });
        overlay::sheet(
            "shelf-url-sheet",
            &self.url_sheet,
            overlay::SHEET_EXPANDED,
            body,
        )
    }

    /// 扫描那一屏在手机上。清单要滚，所以展开那一档比另两屏高。
    fn scan_sheet(&self, cx: &mut Context<Self>) -> AnyElement {
        let body = (!self.scan_sheet.read(cx).is_idle() && self.scan.is_some()).then(|| {
            div()
                .flex_1()
                .min_h_0()
                .px(px(GUTTER))
                .pb_2()
                .child(self.scan_body(cx))
                .into_any_element()
        });
        overlay::sheet(
            "shelf-scan-sheet",
            &self.scan_sheet,
            overlay::SCAN_SHEET_EXPANDED,
            body,
        )
    }

    /// 扫描那一屏在桌面那一档：屏幕居中的一张卡，清单在里面自己滚。
    ///
    /// 高是**定死**的而不是「至少」：`scan_body` 里那条 `v_virtual_list` 要一个能滚的框，
    /// 「至少多高」给不出这个框。
    fn scan_popover(&self, cx: &mut Context<Self>) -> AnyElement {
        let shelf = cx.entity().clone();
        let body = div()
            .w(px(overlay::SCAN_WIDTH))
            .h(px(overlay::SCAN_HEIGHT))
            .flex()
            .flex_col()
            .child(self.scan_body(cx))
            .into_any_element();
        overlay::centered(
            "shelf-scan-popover",
            overlay::SCAN_HEIGHT,
            &shelf,
            |this, cx| this.close_scan(cx),
            body,
        )
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
    pub(super) fn group_items(&self, book_id: i64) -> Vec<book_menu::GroupRow> {
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
    pub(super) fn toggle_group(
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
    /// 入口在书的长按菜单里，浮层却钉在顶部那颗分组按钮上 —— 菜单点完就散了，把它当锚点
    /// 等于钉在一个下一帧就不存在的元素上（见 [`Self::group_popover`]）。
    pub(super) fn new_group(&mut self, book_id: i64, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn close_group_panel(&mut self, cx: &mut Context<Self>) {
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

    /// 换架面看的那一片。搜索词不动而架子动了，所以要先清空再查（同 [`Self::force_reload`]
    /// 那一手）—— 否则 `reload` 的去重会认为「什么都没变」。
    ///
    /// 组卡那一格点下去走的就是这一条：卡与顶部那颗筛选改的是同一个 [`Self::group`]，
    /// 所以两处永远说同一件事。
    pub(super) fn set_group(&mut self, group: GroupFilter, cx: &mut Context<Self>) {
        self.group = group;
        self.items.clear();
        let query = self.query.clone();
        self.reload(&query, cx);
    }

    /// 顶部那颗分组筛选：图标一颗，筛着某一组时吃选中态。
    ///
    /// 手机上那一行只剩「满幅框 + 两颗图标」的宽度，所以这里不带文字；点开那一条菜单里
    /// 写着筛的是哪一组，组名后面缀着书数（「想读 · 12」），读者不必点进去才知道有几本。
    fn group_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().clone();
        let label = tr(cx, "shelf.group");
        let all_label = tr(cx, "shelf.group_all");
        let ungrouped_label = tr(cx, "shelf.ungrouped");
        let collections = self.collections.clone();
        let current = self.group;
        Button::new("shelf-group-filter")
            .ghost()
            .small()
            .icon(IconName::FolderTree)
            .selected(current != GroupFilter::All)
            .tooltip(label.clone().to_string())
            .accessibility_label(label)
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                let menu = menu
                    .min_w(px(book_menu::MIN_WIDTH))
                    .item(
                        PopupMenuItem::new(all_label.clone())
                            .checked(current == GroupFilter::All)
                            .on_click({
                                let entity = entity.clone();
                                move |_, _, cx| {
                                    entity.update(cx, |this, cx| {
                                        this.set_group(GroupFilter::All, cx)
                                    });
                                }
                            }),
                    )
                    .item(
                        PopupMenuItem::new(ungrouped_label.clone())
                            .checked(current == GroupFilter::Ungrouped)
                            .on_click({
                                let entity = entity.clone();
                                move |_, _, cx| {
                                    entity.update(cx, |this, cx| {
                                        this.set_group(GroupFilter::Ungrouped, cx)
                                    });
                                }
                            }),
                    );
                // 一个组都还没建时这一节整块不出现：条目只有「全部分组 / 未分组」两条的
                // 菜单，读起来像坏了。
                if collections.is_empty() {
                    return menu;
                }
                let menu = menu.separator();
                // 菜单每开一遍重建一次（builder 是 `Fn`），所以这一份清单每次现摘一份。
                collections.clone().into_iter().fold(menu, |menu, c| {
                    let entity = entity.clone();
                    let name = format!("{} · {}", c.name, c.book_count);
                    menu.item(
                        PopupMenuItem::new(name)
                            .checked(current == GroupFilter::Collection(c.id))
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |this, cx| {
                                    this.set_group(GroupFilter::Collection(c.id), cx)
                                });
                            }),
                    )
                })
            })
    }

    /// 「新建分组」那一屏的内容：一行输入 + 两颗按钮。桌面与移动共用这一份，
    /// 只有容器不同（见 [`overlay`]）。
    ///
    /// 不带横向留白也不带 `flex_1`：宽度与高归容器给 —— 桌面那张浮层是定宽的，手机上
    /// 弹层天生满幅，写在这里两头都会多算一层。
    fn group_form(&self, entry: Entity<InputState>, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity().clone();
        let close_entity = entity.clone();
        let create_entity = entity.clone();
        let cancel_label = tr(cx, "shelf.cancel");
        let create_label = tr(cx, "shelf.group_create");
        v_flex()
            .id("shelf-group-panel")
            .test_support()
            .w_full()
            .gap_3()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr(cx, "shelf.group_new")),
            )
            .child(muted_label(tr(cx, "shelf.group_hint"), cx))
            .child(div().w_full().child(Input::new(&entry).cleanable(true)))
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        Button::new("shelf-group-cancel")
                            .ghost()
                            .label(cancel_label.to_string())
                            .accessibility_label(cancel_label)
                            .on_click(move |_, _, cx| {
                                close_entity.update(cx, |this, cx| this.close_group_panel(cx))
                            }),
                    )
                    .child(
                        Button::new("shelf-group-create")
                            .primary()
                            .label(create_label.to_string())
                            .accessibility_label(create_label)
                            .on_click(move |_, _, cx| {
                                create_entity.update(cx, |this, cx| this.create_group(cx))
                            }),
                    ),
            )
            .into_any_element()
    }

    /// 「新建分组」在桌面那一档：钉在顶部那颗分组按钮底下的一张浮层。
    ///
    /// 入口仍在书的长按菜单里（那一格说的是「为这本书建个组」），但浮层跟着**管组的那颗
    /// 按钮**开：菜单点完就没了，钉在它消失的位置上等于钉在一个已经不存在的锚点上。
    /// 挂在按钮之后而不是按钮里面 —— 它是那颗按钮那一条命令的延续。
    fn group_popover(&self, entry: Entity<InputState>, cx: &mut Context<Self>) -> AnyElement {
        let shelf = cx.entity().clone();
        let body = self.group_form(entry, cx);
        overlay::anchored(
            "shelf-group-popover",
            Anchor::TopLeft,
            6.0,
            &shelf,
            |this, cx| this.close_group_panel(cx),
            div().w(px(overlay::GROUP_WIDTH)).child(body).into_any_element(),
        )
    }

    /// 删掉一本：库里的行、属于我们自己的那份副本，然后重查一次架面。
    ///
    /// 「哪些文件算我们自己的」这条判据在 [`delete_book`] 里，不在这里：桌面端记的是
    /// 读者手里的原文件，和移动端的副本在库里长得一模一样，只有位置能分开它们，
    /// 而删错的那一头是不可逆的。菜单上那颗「删除」见 [`super::book_menu`]。
    pub(super) fn remove(&mut self, book_id: i64, cx: &mut Context<Self>) {
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

    /// 右上角那颗三点按钮：本体不做任何事，点开一条菜单，三种导入方式摆在菜单里。
    ///
    /// 三颗按钮并排把搜索框挤成一条缝，而导入是偶尔才用一次的命令 —— 收进一条
    /// 「⋮」后面，这一行就只剩「搜书」与「更多」两件事。组件是 gpui-kit 的
    /// [`DropdownMenu`]：按钮负责开合，浮层内容是一条 [`PopupMenu`]，与架面卡片
    /// 长按（`ContextMenuExt`）弹出的那一份同源，只是触发从右键/长按换成了点击。
    /// 浮层在 Android 上点得住（`plans/facts.md`，2026-10-11 读者更正过旧的记录），
    /// 所以两端共用这一颗。
    ///
    /// 菜单项各自把命令派回本视图的既有方法（`import` / `open_url_panel` /
    /// `start_scan`），门禁照旧：书库位置没确认过先摆确认面板，一批没导完就按不动。
    /// 菜单每次打开都会重建一遍（builder 是 `Fn`），所以闭包里先 clone 再 move：
    /// 从捕获的句柄里现摘一份，而不是把上次那一份搬走。
    fn more_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().clone();
        let url_entity = entity.clone();
        let scan_entity = entity.clone();
        let import_label = tr(cx, "shelf.import");
        let scan_label = tr(cx, "shelf.scan");
        let url_label = tr(cx, "shelf.url_import");
        let more_label = tr(cx, "shelf.more");
        Button::new("shelf-more")
            .ghost()
            .small()
            .icon(IconName::EllipsisVertical)
            .tooltip(more_label.clone().to_string())
            .accessibility_label(more_label)
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                let entity = entity.clone();
                let url_entity = url_entity.clone();
                let scan_entity = scan_entity.clone();
                menu.min_w(px(book_menu::MIN_WIDTH))
                    .item(
                        PopupMenuItem::new(import_label.clone())
                            .icon(IconName::Plus)
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |this, cx| this.import(cx))
                            }),
                    )
                    .item(
                        PopupMenuItem::new(url_label.clone())
                            .icon(IconName::Link)
                            .on_click(move |_, window, cx| {
                                url_entity.update(cx, |this, cx| this.open_url_panel(window, cx))
                            }),
                    )
                    .item(
                        PopupMenuItem::new(scan_label.clone())
                            .icon(IconName::ScanSearch)
                            .on_click(move |_, _, cx| {
                                scan_entity.update(cx, |this, cx| this.start_scan(cx))
                            }),
                    )
            })
    }

    /// 搜索框那一行：框占满剩下的宽度，三点按钮钉在右边。
    fn search_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .px(px(GUTTER))
            .py_2()
            .child(div().flex_1().min_w_0().child(self.search.clone()))
            .child(self.group_cell(cx))
            .child(self.more_button(cx))
    }

    /// 桌面/平板的工具栏：钉在架面最上方，搜索框与三点按钮靠右。
    ///
    /// 不复用手机上那一行，因为两者要回答的问题不同：手机上宽度只够一条满幅的框，
    /// 框被拉满就是「先搜」；桌面上把同一颗框拉到 1300 像素宽，它就从一条命令
    /// 变成一块空白。所以这里给它一个和「书名或作者」这句话相称的定宽，
    /// 与三点按钮一起收在右上角。
    ///
    /// 左右留白与网格共用 [`GUTTER`]：两行不共用一条对齐脊线的话，
    /// 搜索框的左边会莫名其妙地比第一格封面缩进一截。
    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .flex_shrink_0()
            .items_center()
            .justify_end()
            .gap_2()
            .px(px(GUTTER))
            .py_2()
            .child(
                // 工具栏按 compact 档排：框和按钮同一档，否则一行里会长出两种高度。
                // 框的 small 档在 `boot` 建组件时就定下了。
                div().w_64().child(self.search.clone()),
            )
            .child(self.group_cell(cx))
            .child(self.more_button(cx))
    }

    /// 「书要拷到哪儿」那块面板。
    ///
    /// 它是架面的一部分而不是浮层：浮层在 Android 上现在点不住（长按菜单那条路的缺陷），
    /// 而这一步是导入的前提，点不动就等于没有导入。
    fn confirm_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let pick_entity = cx.entity().clone();
        let confirm_entity = pick_entity.clone();
        let cancel_entity = pick_entity.clone();
        let pick_label = tr(cx, "library.pick_dir");
        let confirm_label = tr(cx, "library.confirm");
        let cancel_label = tr(cx, "library.cancel");
        // 已经选过就显示选中的那个位置，没选过显示建议位置 —— 选完当场能看出结果。
        let chosen = ConfigHolder::library(cx).dir;
        let shown = if chosen.trim().is_empty() {
            suggested_dir().display().to_string()
        } else {
            chosen
        };
        v_flex()
            .id("library-confirm-panel")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap_3()
            .px(px(GUTTER))
            .py_4()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr(cx, "library.title")),
            )
            .child(muted_label(tr(cx, "library.hint"), cx))
            .child(
                div()
                    .w_full()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(cx.theme().muted)
                    .text_color(cx.theme().muted_foreground)
                    .child(shown),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        Button::new("library-pick")
                            .ghost()
                            .label(pick_label.clone().to_string())
                            .accessibility_label(pick_label)
                            .on_click(move |_, _, cx| {
                                pick_entity.update(cx, |this, cx| this.pick_library(cx))
                            }),
                    )
                    .child(
                        Button::new("library-confirm")
                            .primary()
                            .label(confirm_label.clone().to_string())
                            .accessibility_label(confirm_label)
                            .on_click(move |_, _, cx| {
                                confirm_entity.update(cx, |this, cx| this.confirm_library(cx))
                            }),
                    )
                    .child(
                        Button::new("library-cancel")
                            .ghost()
                            .label(cancel_label.clone().to_string())
                            .accessibility_label(cancel_label)
                            .on_click(move |_, _, cx| {
                                cancel_entity.update(cx, |this, cx| this.cancel_confirm(cx))
                            }),
                    ),
            )
            .into_any_element()
    }

    /// 网格本体，或者两种「这里没有东西」—— 它们必须说得不一样。
    ///
    /// 建组 / 链接导入 / 扫描那三块**不在这里**：它们是浮在架面之上的浮层与弹层，
    /// 架面照旧画在底下（见 [`Self::overlays`]）。只有「书库位置」那一屏还是就地换掉整片
    /// —— 它是导入的前置而不是「答一句话」，答完之前那片格子没有意义。
    fn body(&self, cx: &mut Context<Self>, spec: GridSpec) -> AnyElement {
        if self.confirming {
            return self.confirm_panel(cx);
        }
        if !self.items.is_empty() || !self.tiles.is_empty() {
            return self.grid(cx, spec).into_any_element();
        }
        // 空架要说「导一本书进来」，有书但都不匹配要说「换个词」：
        // 写成一句会让读者以为自己把书弄丢了。
        let (title, note) = if self.query.is_empty() {
            ("shelf.empty", "shelf.empty_hint")
        } else {
            ("shelf.no_results", "shelf.no_results_hint")
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
    fn grid(&self, cx: &mut Context<Self>, spec: GridSpec) -> impl IntoElement {
        // 卡片管理菜单的两条文案：整个架面取一次，逐卡 clone 进菜单。条目本身
        // （哪几条、什么图标、点了派给谁）在 [`book_menu`]。
        let labels = book_menu::Labels::resolve(cx);
        let columns = spec.columns as usize;
        // 一横排算一项；不满一排的尾巴也算一排，不然最后那几本会画不出来。
        // 数的是**格子**：组卡占的那几格与书占的那几格排在同一条下标上。
        let rows = (self.tiles.len() + self.items.len()).div_ceil(columns);
        let shelf = cx.entity().clone();
        let scroll = self.grid_scroll.clone();
        let cover_height = spec.cover_height;
        let list = uniform_list("shelf-grid", rows, move |range, window, cx| {
            range
                .map(|row| {
                    let from = row * columns;
                    // 句柄只在这一排真要画的时候读那几行数据：整片 `items` 不 clone。
                    shelf.update(cx, |this, cx| {
                        let tiles = this.tiles.len();
                        let total = tiles + this.items.len();
                        let to = (from + columns).min(total);
                        // 落定的时钟先取，再借 `items`：进度是这一片格子共用的一个
                        // 数，逐格只是起步早晚不同。
                        let enter = this.enter_clock(total, window, cx);
                        div()
                            .w_full()
                            .grid()
                            .grid_cols(spec.columns)
                            .gap_2()
                            // 左右留白仍归每一排自己：滚轴贴的是这块区域真正的边。
                            .px(px(GUTTER))
                            .py_1()
                            .children((from..to).map(|ix| {
                                // 前面那几格是组卡，往后才是书 —— 与 [`Self::tiles`] /
                                // [`Self::items`] 那两个数的分界是同一条。
                                if ix < tiles {
                                    this.group_cell_element(
                                        &this.tiles[ix],
                                        cover_height,
                                        &enter,
                                        ix,
                                        cx,
                                    )
                                } else {
                                    this.card_element(
                                        &this.items[ix - tiles],
                                        &labels,
                                        cover_height,
                                        &enter,
                                        ix,
                                        cx,
                                    )
                                }
                            }))
                            .into_any_element()
                    })
                })
                .collect()
        })
        .track_scroll(&scroll)
        .size_full();

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
            self.items.clear();
            let query = self.query.clone();
            self.reload(&query, cx);
        }
        // 顶部那一行按窗口档位换：手机上满幅的搜索框是「先搜再翻」，桌面上同一颗框
        // 拉到整片内容宽就成了一块空白，所以那边是定宽搜索框 + 导入的工具栏。
        let size = SizeClass::of(window);
        let top = if size.is_compact() {
            self.search_bar(cx).into_any_element()
        } else {
            self.toolbar(cx).into_any_element()
        };
        let spec = grid_spec(f32::from(window.viewport_size().width), size);
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
        // 建组那一张挂在顶部那颗分组按钮上（锚点在那儿），所以不在这一列里，见 `group_cell`。
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
            if self.url_open && let Some(entry) = &self.url_entry {
                overlays.push(self.url_popover(entry.clone(), cx));
            }
            if self.scan_open && self.scan.is_some() {
                overlays.push(self.scan_popover(cx));
            }
            overlays
        };
        v_flex()
            .id("folio-shelf")
            // 那两块居中的浮层要有一个「内容区」可以居中，而这一格就是内容区：
            // 导航栏在它的左边、不在它里面。
            .relative()
            .size_full()
            .child(top)
            .children(status)
            .child(self.body(cx, spec))
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
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, ElementId, Entity, TestAppContext, px, size,
    };

    use super::{COVER_RATIO, GAP, GUTTER, GroupFilter, ShelfView, columns_for, grid_spec};
    use crate::ui::shared::config_model::ConfigHolder;
    use crate::ui::shell::navigation::SizeClass;

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

        // 桌面最小窗口（图标栏档）：两格，宁可小也不许没有。
        assert_eq!(columns_for(520.0, SizeClass::Medium), 2);
        // 每格都不窄于 168：再加一列就要挤破这个下限时，宁可少一列。
        assert_eq!(columns_for(776.0, SizeClass::Medium), 4);
        assert_eq!(columns_for(1600.0, SizeClass::Expanded), 7);

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
    /// 那块「书要拷到哪儿」的面板，不是格子）。返回（架面, 窗口, 书库）。
    fn boot_shelf(cx: &mut TestAppContext) -> (Entity<ShelfView>, AnyWindowHandle, Arc<Store>) {
        cx.update(gpui_kit::init);
        let mut built = None;
        let handle = cx
            .open_window(size(px(900.), px(600.)), |window, cx| {
                crate::ui::shared::app_store::install(Store::in_memory().expect("内存库"), cx);
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
                built = Some(view.clone());
                Root::new(view, window, cx)
            })
            .into();
        let store = cx
            .update(|cx| crate::ui::shared::app_store::handle(cx))
            .expect("书库应已装上");
        (built.expect("架面应已建起"), handle, store)
    }

    /// 分组那三处共用同一份库状态：卡片菜单里那颗是**开关**（切进 / 切出）、顶部那颗
    /// 筛选跟着换看到的那一片、而「新建分组…」建完要把这本书顺手挂进去。
    ///
    /// 三条各自都能单独坏掉而编译照样绿：开关写成单向的「加入」→ 点第二下什么也不发生；
    /// 筛选只改状态不重查 → 架面看着还是整架（`reload` 的去重按「词没变且架子非空」判，
    /// 正会跳过这一趟，所以 `set_group` 必须先清空）；新建只建组不挂书 → 组有了、书还在外面；
    /// 空名那一趟把面板清了 → 读者打了一半的字凭空消失。
    #[gpui_kit::test]
    fn the_group_switch_and_the_filter_agree(cx: &mut TestAppContext) {
        let (shelf, handle, store) = boot_shelf(cx);
        let a = add(&store, "甲");
        let b = add(&store, "乙");
        let group = store.create_collection("想读").expect("建组");
        store.add_to_collection(group, a).expect("挂进去");

        shelf.update(cx, |this, cx| this.force_reload(cx));
        assert_eq!(
            shelf.read_with(cx, |this, _| this.card_count()),
            2,
            "整架那一片两本都在"
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

        // 那颗开关两头都要动。
        shelf.update(cx, |this, cx| this.toggle_group(b, group, cx));
        assert_eq!(
            store.collections_of_book(b).expect("读库").len(),
            1,
            "不在里面时点一下是加入"
        );
        shelf.update(cx, |this, cx| this.toggle_group(b, group, cx));
        assert!(
            store.collections_of_book(b).expect("读库").is_empty(),
            "已经在里面时同一下是移出"
        );

        // 顶部那颗筛选：某一组 / 未分组，两片是互补的。
        shelf.update(cx, |this, cx| {
            this.set_group(GroupFilter::Collection(group), cx)
        });
        assert_eq!(
            shelf.read_with(cx, |this, _| this.card_count()),
            1,
            "组里只有甲那一本"
        );
        shelf.update(cx, |this, cx| this.set_group(GroupFilter::Ungrouped, cx));
        assert_eq!(
            shelf.read_with(cx, |this, _| this.card_count()),
            1,
            "没进组的只剩乙"
        );
        assert_eq!(
            shelf.read_with(cx, |this, _| this.card_title(0)),
            Some("乙".into()),
            "筛出来的那一本要认得"
        );

        // 搜索词与筛选项**叠加**：同一句「甲」，整架那一片命中、未分组那一片不该命中。
        shelf.update(cx, |this, cx| this.set_group(GroupFilter::All, cx));
        shelf.update(cx, |this, cx| this.reload("甲", cx));
        assert_eq!(shelf.read_with(cx, |this, _| this.card_count()), 1);
        shelf.update(cx, |this, cx| this.set_group(GroupFilter::Ungrouped, cx));
        assert_eq!(
            shelf.read_with(cx, |this, _| this.card_count()),
            0,
            "未分组那一头不许绕开搜索词"
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

    /// 组卡只在「整架 + 没搜词」那一屏领头，一筛一搜就退场；点它等于筛到那一组。
    ///
    /// 三条各自都能单独坏掉而编译照样绿：筛到某一组时还摆一排组卡，等于把读者刚走进去的
    /// 那层又摊回他面前；带着搜索词还摆，卡上那几本封面与搜索结果毫无关系，读起来就是
    /// 「这几本匹配」；而 `reload` 的去重按「词没变且架子非空」判，换筛不重排的话
    /// 那一排卡会留在原地不动。
    #[gpui_kit::test]
    fn the_group_tiles_lead_the_shelf_and_step_aside(cx: &mut TestAppContext) {
        let (shelf, _, store) = boot_shelf(cx);
        let a = add(&store, "甲");
        let b = add(&store, "乙");
        let c = add(&store, "丙");
        let want = store.create_collection("想读").expect("建组");
        let misc = store.create_collection("杂项").expect("建组");
        store.add_to_collection(want, a).expect("挂进去");
        store.add_to_collection(want, b).expect("挂进去");
        store.add_to_collection(misc, c).expect("挂进去");

        shelf.update(cx, |this, cx| this.force_reload(cx));
        assert_eq!(
            shelf.read_with(cx, |this, _| this.tile_count()),
            2,
            "整架那一屏顶部两张组卡"
        );
        // 组卡在前、书在后，所以「第 0 格」与「第 0 本」不是一回事：`items` 只数书。
        assert_eq!(
            shelf.read_with(cx, |this, _| this.card_count()),
            3,
            "组卡不占书的格子数"
        );
        let tiles = shelf.read_with(cx, |this, _| {
            (0..this.tile_count())
                .map(|ix| this.tile_at(ix).expect("第 {ix} 张在"))
                .collect::<Vec<_>>()
        });
        assert_eq!(
            tiles,
            vec![(want, "想读".into(), 2), (misc, "杂项".into(), 1)],
            "两张卡跟着库里那条名字序（`ORDER BY c.name` 是字节序），下面那行念的是这一组一共几本"
        );

        // 筛到某一组：那一排的卡退场，剩下的就是这一组里的书。
        shelf.update(cx, |this, cx| {
            this.set_group(GroupFilter::Collection(want), cx)
        });
        assert_eq!(shelf.read_with(cx, |this, _| this.tile_count()), 0);
        assert_eq!(shelf.read_with(cx, |this, _| this.card_count()), 2);

        // 回到整架再搜一个词：卡还是不摆。
        shelf.update(cx, |this, cx| this.set_group(GroupFilter::All, cx));
        assert_eq!(shelf.read_with(cx, |this, _| this.tile_count()), 2);
        shelf.update(cx, |this, cx| this.reload("甲", cx));
        assert_eq!(
            shelf.read_with(cx, |this, _| this.tile_count()),
            0,
            "带着搜索词还摆组卡，读起来就是「这几本匹配」"
        );

        // 「未分组」那一头同理：筛的是书，不是组。
        shelf.update(cx, |this, cx| this.reload("", cx));
        shelf.update(cx, |this, cx| this.set_group(GroupFilter::Ungrouped, cx));
        assert_eq!(shelf.read_with(cx, |this, _| this.tile_count()), 0);
    }

    /// 建组那一屏是**浮在**架面上的：它开着的时候，底下那张格子还在原地、还点得着。
    ///
    /// 迁浮层之前它是就地换掉整片格子的，所以这一条钉的是这次改动的正面：`body` 不再
    /// 为它让位。浮层自己晚一帧才挂进弹窗层（要先量一次触发器），所以断言前连画两帧。
    #[gpui_kit::test]
    fn the_group_form_floats_over_the_grid(cx: &mut TestAppContext) {
        let (shelf, handle, store) = boot_shelf(cx);
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
}
