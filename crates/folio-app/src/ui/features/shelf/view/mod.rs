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
pub(in crate::ui::features::shelf) mod network_import;
pub(in crate::ui::features::shelf) mod scan;
pub(in crate::ui::features::shelf) mod sheets;
pub(in crate::ui::features::shelf) mod status;
pub(in crate::ui::features::shelf) mod toolbar;

use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use folio_core::FormatExt;
use folio_core::store::Collection;
use folio_core::{
    Cancel, Format, ImportOutcome, ImportReport, Imported, PathsSource, Store, Tag, TaskEvent,
    submit_import,
};
use gpui_kit::base::motion::{Transition, transition};
use gpui_kit::base::{NavMotion, NavStackState, VirtualListScrollHandle};
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::input::InputEvent;
use gpui_kit::component::input::InputState;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Sizable as _, WindowExt as _, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, Image, InteractiveElement,
    IntoElement, ParentElement, PathPromptOptions, Render, SharedString, Styled, Subscription,
    Task, UniformListScrollHandle, WeakEntity, Window, canvas, div, px, uniform_list,
};

use super::components::book_card::{Card, Enter, GroupCard, TILE_SLOTS};
use super::components::book_menu;
use crate::ui::components::{
    BottomSheetEvent, BottomSheetState, SearchBox, SearchBoxEvent, SearchDebounce, TopBar,
    empty_state,
};
use crate::ui::features::cloud::{CloudEvent, CloudView};
use crate::ui::features::opds::{OpdsEvent, OpdsView};
use crate::ui::foundation::error::{Context as _, report};
use crate::ui::foundation::file_stem::title_stem;
use crate::ui::host;
use crate::ui::services::cloud_upload;
use crate::ui::services::config::ConfigHolder;
use crate::ui::services::l10n::{book_count, tf, tr};
use crate::ui::services::library::{delete_book, pick_dir, suggested_dir, target_dir};
use crate::ui::services::resident;
use crate::ui::services::transfer;
use crate::ui::shell::navigation::{self, SizeClass};
use grid::{Cell, GUTTER, GridSpec, grid_spec, merge_cells};
use scan::ScanPhase;

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
///
/// 从前还有第三条 `Fold(bool)`：架面自己读位移判「导航栏该让位」再报出去。判据已经搬到
/// 导航栏那一层（`shell::navigation::fold`），四个目的地共用一句，所以这里不再上报；
/// 架面手上那份 `folded` 现在是外壳递进来的投影（[`ShelfView::set_fold`]）。
pub enum ShelfEvent {
    /// 点了一张卡片。
    Open(i64),
    /// 删掉了一本。外壳需要这条：正文可能正开着这一本，书没了，那一层就该退回架面。
    Deleted(i64),
    /// 多选态变了（进入 / 退出 / 勾选）。外壳据此把底部导航换成多选工具栏。
    SelectionChanged,
}

/// 外壳渲染多选工具栏要的几格。**一次读齐**：分几次 `read` 的话，同一个句柄上读到的
/// 可能是两次改动之间的两种状态（多选计数与全选标记对不上）。
pub struct SelectionSummary {
    pub active: bool,
    pub count: usize,
    pub all_selected: bool,
    /// 只选了一本时是它的主键 —— 「信息」那一格只对这一档有意义。
    pub single: Option<i64>,
    /// 可加入的分组（按库里的序）。没有分组时工具栏那一颗按不动。
    pub groups: Vec<(i64, SharedString)>,
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

/// 搜索行收起时让出的高度（逻辑像素）：`py_2` 上下各 8，加搜索框那一档。
const SEARCH_BAND: f32 = 52.0;

/// 一排封面在滚动里的缩放档：视口正中 1.0，进出视口的两端收到 0.96。
///
/// 只走封面图自己的尺寸（这个框架的 div 没有 transform），所以不影响排距；
/// 幅度刻意压得很小 —— 它是滚动时的环境感，不是每一排都要「弹一下」。
fn cover_scale(row: usize, row_pitch: f32, scroll_y: f32, viewport_height: f32) -> f32 {
    if row_pitch <= 0.0 {
        return 1.0;
    }
    let half = (viewport_height / 2.0).max(1.0);
    let center = row as f32 * row_pitch + row_pitch / 2.0 - scroll_y;
    let distance = ((center - half) / half).clamp(-1.0, 1.0);
    1.0 - 0.04 * distance * distance
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
    /// 顶栏此刻收没收。**这是外壳递进来的投影**（[`ShelfView::set_fold`]）：让位的判据
    /// 在导航栏那一层、四个目的地共用一句，架面不再自己读位移判方向。
    folded: bool,
    /// 此刻手指/指针按着的那张卡（书库主键）。按下即记、抬起或按空处就清：
    /// 卡片的封面按这一格收一档，给「按住了」一个即时回执。
    pub(in crate::ui::features::shelf) pressed_card: Option<i64>,
    /// 上一次查库用的串。
    query: SharedString,
    /// 还没到期的那一次搜索（输入框每变一次重新起表，安静满那一档才去查库）。
    search_debounce: SearchDebounce,
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
    /// 标记浮层为哪一本书开着，带**打开那一刻**库里的标签快照（打勾读的是它，
    /// 落库走 [`Self::toggle_tag`] 一格一写）。`None` = 浮层没开。
    tag_entry: Option<(i64, Vec<Tag>)>,
    /// 「网络导入」那张卡摆没摆。开着时 Wi-Fi 传书服务一定起着（打开那一手起的），
    /// 收卡**不停**服务 —— 读者要切到另一台设备上去传书。
    network_open: bool,
    /// 手机那一档的弹层状态。桌面那一档用 `overlay::centered`，不需要它；留着是为了
    /// 转屏 / 拉窗口时不必重建（与 `group_sheet` / `scan_sheet` 同一条）。
    network_sheet: Entity<BottomSheetState>,
    /// 卡片上那张二维码（第一个地址的 PNG）。打开卡片时算一次，之后每帧只画，
    /// 不重编码；收卡时丢掉。没探到局域网地址时是 `None`。
    network_qr: Option<Arc<Image>>,
    /// 整架的标签表：书号 → 挂着的标签（**手动覆盖**的那几行，老库可能一格多行）。
    /// 与架面同一次重查取（[`Self::reload`]），卡片角标与状态筛选都读它；
    /// 标记浮层落库后也只改它那一格，不必整架重查。
    pub(in crate::ui::features::shelf) tags: HashMap<i64, Vec<Tag>>,
    /// 架面的状态筛选。`Some(tag)` = 整架那一片只摆**有效状态**是它的书 —— 手动挂过
    /// 标签的以手动为准（`Tag::effective`），没挂的按阅读数据自动判
    /// （`ShelfItem::auto_status`）；组卡区跟着藏起（与搜索词非空同一处理）。
    /// 单颗可点选 / 再点取消；一次筛一颗。
    tag_filter: Option<Tag>,
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
    /// 上面那份封面是在哪一档下解的（水墨 / 正常）。翻档之后整份丢掉重解：缓存键是
    /// 书号，里面没有「哪一档」这一格，彩色那几份会原样留着继续画。
    pub(in crate::ui::features::shelf) covers_ink: bool,
    /// 解这批封面时后台补齐作业走到第几趟（`folio_core::cover_epoch`）。
    /// 补回一本就把 `None` 缓存作废 —— 否则封面文件已经出现在 `covers/`，架面还画占位。
    pub(in crate::ui::features::shelf) covers_epoch: u64,
    _search_watch: Subscription,
    /// 导入那条异步链：等选择器 → 把选中的文件交给后端管线 → 收事件直到这批结束。
    /// Task 一 drop 链就断，于是「选了文件但界面已经忘了」；而断在管线跑到一半时，
    /// 后台那份拷贝照旧跑完、结果却没人登记 —— 架上少一本，书库目录里多一份副本。
    /// 所以这条链要活到整批导完。
    _import: Option<Task<()>>,
    /// 正在进行的那批导入的读数：`(已完成, 总数)`。`total == 0` 是不定进度 ——
    /// 刚提交、管线第一条读数还没上来时就是这个样子；`None` 表示没在导入。
    /// 读数来自后端报的 `Progress` 事件，画在哪儿见 [`super::status`]。
    pub(in crate::ui::features::shelf) importing: Option<(u64, u64)>,
    /// 正在进行的这批导入的取消旗标，来自 [`TaskHandle`](folio_core::infra::task::TaskHandle)。
    /// 状态卡上那颗「取消」按钮置位它；管线在两本书之间看见，已落库的那些是真的
    /// （`Cancelled` 事件来了重查一次架面就有）。
    import_cancel: Option<Cancel>,
    /// 正在跑的这一批的记录行，按字节读数长出来（见 [`super::status`]）。
    import_live: Vec<status::Record>,
    /// 已经结清的那几批的记录行，新的在前。这一次运行里跑完的那些在内存里，重启之前读过
    /// 库里那一份接在后面（见 [`super::status::ShelfView::load_import_log`]）。
    import_history: Vec<status::Record>,
    /// 库里那份记录读回来了没有。一次就够；库还没装上时保持 `false`，下一帧再来。
    import_log_loaded: bool,
    /// 记录那张卡里那条清单的滚动位置，跨帧握着（与架面那片格子同一个理由）。
    record_scroll: VirtualListScrollHandle,
    /// 读者从三点菜单里**点名**要看导入记录（历史那一份）。
    ///
    /// 与 [`Self::importing`] / [`Self::import_live`] 分开：那两格是「这一批正在跑 / 刚跑完，
    /// 有话要说」，这一格是「读者自己把历史翻了出来」。库里那份历史是**重启之后才读回来**的，
    /// 只凭它非空就把卡抬起来 —— 每次开软件都先弹一张昨天的记录，那不是读者要的。
    pub(in crate::ui::features::shelf) import_records_open: bool,
    /// 这一次运行里**已经有活干完了**：那一批的逐本着落值得再交代一次。
    ///
    /// 与「历史非空」分开：库里那份是重启之后读回来的（只说「以前导过」），这一格只说
    /// 「刚刚这一批结了」。只有它能让卡自己冒出来；两者的差正是「打开软件先弹昨天的
    /// 记录」那个 bug。
    pub(in crate::ui::features::shelf) import_settled: bool,
    /// 读者自己把导入状态那张卡关掉过（点卡外面、按返回）。
    ///
    /// 只在**这一批**有效：提交下一批时清掉，一批跑完发现有几本没进来也清掉 ——
    /// 那一幕是新信息，值得再说一次。判据与置位都在 [`super::status`]。
    import_status_dismissed: bool,
    /// 整机扫描那一屏的意图（内容在 [`Self::scan`]）。见 [`Self::group_open`] 那一段为什么分两半。
    pub(in crate::ui::features::shelf) scan_open: bool,
    /// 整机扫描那一屏的状态：正在扫 / 已出清单。`None` = 这一屏没东西可画了。
    /// 面板本体与全部动作在 [`super::scan`]。
    pub(in crate::ui::features::shelf) scan: Option<ScanPhase>,
    /// 扫描清单顶上那颗搜索框：按**文件名**筛（清单那份在内存里，逐字筛得动，
    /// 不必像架面那样等安静）。
    pub(in crate::ui::features::shelf) scan_search: Entity<SearchBox>,
    /// 扫描清单的筛选条件（格式 / 大小上限 / 在架）。判据在 `folio_core::ScanFilter` 里 ——
    /// 「哪种文件算哪种格式」与扫描自己那套口径同一处。
    pub(in crate::ui::features::shelf) scan_filter: folio_core::ScanFilter,
    /// 大小上限那个输入框（MB）：只留**小于**它的候选。读者的手写的是「5」，
    /// 条件里存的是字节 —— 换算只在那句订阅里做一次。
    pub(in crate::ui::features::shelf) scan_size: Entity<InputState>,
    /// 筛选那一排展开着没有。只是界面上的开合，条件本身在 [`Self::scan_filter`]。
    pub(in crate::ui::features::shelf) scan_filter_open: bool,
    /// 扫描那条异步链（事件桥）。与 `_import` 同理，要活到整轮结束。
    pub(in crate::ui::features::shelf) _scan: Option<Task<()>>,
    /// 扫描搜索框与大小输入的订阅：一断，框里打字就不再筛清单。
    _scan_search_watch: Subscription,
    _scan_size_watch: Vec<Subscription>,
    /// 自动导入（监听 / 传书）事件的订阅链。这些批次不是这个视图提交的，
    /// 靠共享事件通道流回来 —— 处理与手工导入同一套（读数、重查架面）。
    _auto_import: Option<Task<()>>,
    /// 移动端那两块弹层各自的开合与停靠档。桌面那一档用的是 `Popover`，这两份只是
    /// 没挂载而已 —— 状态留着比按档位分叉省一件事：转屏 / 拉窗口时不必重建。
    group_sheet: Entity<BottomSheetState>,
    scan_sheet: Entity<BottomSheetState>,
    /// 两块弹层各自「读者自己关掉了」那一条：下拉过头、点遮罩、按返回都走它。
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
    /// OPDS 浏览器那一页的导入事件：有书进了库，架面要重查。
    _opds_watch: Option<Subscription>,
    /// 网盘浏览器那一页的导入事件：同一条，谁进了库都报同一声。
    _cloud_watch: Option<Subscription>,
    /// 上传实体那一条观察：后台那趟回来要重画进度浮层。与 `_cloud_watch` 同一条
    /// 「订阅活到这一屏结束」的规矩。
    _upload_watch: Option<Subscription>,
    /// 「书要拷到哪儿」那块面板里，系统文件夹选择器那条异步链。与 `_import` 同理，要活到选完。
    _pick: Option<Task<()>>,
    /// 对「没拿到所有文件访问」那一问的订阅：那一问摆不摆由服务那份实体说了算，它一
    /// `notify` 架面就得重画，否则答完面板还杵在那儿，看着像按了没反应。
    _access_watch: Subscription,
    /// 导入完成后的**下一帧兜底重查**。选择器把应用切到后台，恢复前台的那一帧上
    /// 后台期间发的 notify 可能丢（真机实测：导入后卡片要重新进书架才出现），
    /// render 里见到这面旗就强制重查一次 —— 代价只是多查一遍库。
    pending_reload: bool,
    /// 格子流（组卡与书卡并成的一条）。**重查那一趟拼一次**，不每帧拼：
    /// `uniform_list` 的可见排每帧重造，而这条流只随 `items` / `tiles` 变。
    /// 用 `Rc` 是为了把它交给列表闭包时不整份复制（闭包要 `'static`）。
    pub(in crate::ui::features::shelf) flow: Rc<[Cell]>,
    /// 上面那条流该重拼了。
    pub(in crate::ui::features::shelf) flow_dirty: bool,
    /// 多选态：`true` 时点卡片是勾选而不是开书，底部导航换成工具栏。
    pub(in crate::ui::features::shelf) selecting: bool,
    /// 勾中的书（库主键）。用 `BTreeSet` 而不是 `HashSet`：删除时按主键从小到大做，
    /// 同一批书的日志与报错顺序稳定，测试也不必容忍随机序。
    pub(in crate::ui::features::shelf) selected: BTreeSet<i64>,
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
            this.search_soon(query, cx);
        });
        // 两块弹层各订阅一次「读者自己把它关掉了」：下拉过头、点遮罩、按返回都只报这一条。
        // 不据此清架面那一侧的状态的话，下一帧 [`Self::sync_panels`] 看着「面板还开着」
        // 又把弹层抬起来 —— 在读者手里就成了「这层关不掉」。
        // 「没拿到所有文件访问」那一问：与那两块弹层同一立场——状态归服务那份实体，画不画
        // 归架面，所以这里必须观察它，否则答完不重画。
        let question = crate::ui::services::library::access(cx);
        let access_watch = cx.observe(&question, |_, _, cx| cx.notify());
        let group_sheet = cx.new(BottomSheetState::closed);
        let scan_sheet = cx.new(BottomSheetState::closed);
        let network_sheet = cx.new(BottomSheetState::closed);
        // 扫描清单的搜索框：与架面顶上那颗分开（一个查库、一个筛内存里的候选）。
        // 小一档：它坐在卡片里，与旁边那颗筛选按钮同高。
        let scan_search = cx
            .new(|cx| SearchBox::new(tr(cx, "shelf.scan_search_placeholder"), window, cx).small());
        let _scan_search_watch = cx.subscribe(&scan_search, |this, _, event, cx| {
            let SearchBoxEvent::Change(query) = event;
            this.set_scan_query(query.clone(), cx);
        });
        // 大小上限一个框：打字即筛（清单在内存里），换算 MB → 字节在这里做一次。
        let scan_size = cx.new(|cx| {
            InputState::new(window, cx).placeholder(tr(cx, "shelf.scan_filter_size_under"))
        });
        let _scan_size_watch = vec![cx.subscribe(&scan_size, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let text = this.scan_size.read(cx).value();
                this.set_scan_size(&text, cx);
            }
        })];
        let _sheet_watches = vec![
            cx.subscribe(&group_sheet, |this, _, event, _| {
                if matches!(event, BottomSheetEvent::Dismissed) {
                    this.group_open = false;
                }
            }),
            cx.subscribe(&scan_sheet, |this, _, event, _| {
                if matches!(event, BottomSheetEvent::Dismissed) {
                    this.scan_open = false;
                }
            }),
            cx.subscribe(&network_sheet, |this, _, event, cx| {
                if matches!(event, BottomSheetEvent::Dismissed) {
                    // 与 `close_network_import` 同一条：收卡即停服务、丢掉二维码。
                    this.network_open = false;
                    this.network_qr = None;
                    transfer::set_enabled(false, cx);
                }
            }),
        ];
        let mut this = Self {
            store,
            stack,
            items: Vec::new(),
            tiles: Vec::new(),
            grid_scroll: UniformListScrollHandle::new(),
            folded: false,
            pressed_card: None,
            query: SharedString::default(),
            search_debounce: SearchDebounce::default(),
            search,
            scope,
            collections: Vec::new(),
            group_page: None,
            group_entry: None,
            group_open: false,
            tag_entry: None,
            network_open: false,
            network_qr: None,
            tags: HashMap::new(),
            tag_filter: None,
            confirming: false,
            covers: HashMap::new(),
            covers_ink: false,
            covers_epoch: folio_core::cover_epoch(),
            _search_watch,
            _import: None,
            importing: None,
            import_cancel: None,
            import_live: Vec::new(),
            import_history: Vec::new(),
            import_log_loaded: false,
            record_scroll: VirtualListScrollHandle::new(),
            import_records_open: false,
            import_settled: false,
            import_status_dismissed: false,
            scan: None,
            scan_open: false,
            scan_search,
            scan_filter: folio_core::ScanFilter::default(),
            scan_size,
            scan_filter_open: false,
            _scan_search_watch,
            _scan_size_watch,
            _scan: None,
            _auto_import: None,
            group_sheet,
            scan_sheet,
            network_sheet,
            _sheet_watches,
            _group_watch: None,
            _info_watch: None,
            _opds_watch: None,
            _cloud_watch: None,
            _upload_watch: None,
            _pick: None,
            _access_watch: access_watch,
            pending_reload: false,
            flow: Rc::from(Vec::new()),
            flow_dirty: true,
            selecting: false,
            selected: BTreeSet::new(),
            enter_at: None,
        };
        this.reload(&SharedString::default(), cx);
        this
    }

    /// 查一遍库换成卡片。查不动时保留上一次的架面而不是清空：
    /// 一条语句报错不该让读者的书看起来消失了。
    /// 输入框每变一次都走这里。搜的是**整库**（每本一次查询），而一次输入是十几个字 ——
    /// 逐字提交就是每字扫一遍架面 + 逐本解封面（`reload` 里那一趟是 UI 线程上的库读）。
    /// 所以词当场记下来（回显与清空那条快路都读它），那一趟查询等安静。
    ///
    /// 空词**不 delay**：他要的是「立刻看到整架」，而那是我们已经有的那份 `items`。
    fn search_soon(&mut self, query: &str, cx: &mut Context<Self>) {
        if let Some(word) = self.search_debounce.typed(query) {
            self.reload(&word, cx);
            return;
        }
        // 没到点：只要让自己再画一帧 —— 去抖是**数帧**的，帧到了才会在 `render` 里结算。
        cx.notify();
    }

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
        // 组卡只在「整架 + 没搜词 + 没筛标签」那一屏出现。某一组那一页再摆一排组卡，等于把读者刚
        // 走进去的那一层又摊回他面前；而搜的是书不是组 —— 词非空时那几张卡上的封面跟
        // 搜索结果没有半点关系，摆出来只会读成「这几本匹配」。筛着标签时同理：那一排
        // 回答的是「按组收纳」，不是「按状态筛」。
        self.tiles = if self.scope == ShelfScope::Shelf
            && query.trim().is_empty()
            && self.tag_filter.is_none()
        {
            self.tile_cards()
        } else {
            Vec::new()
        };
        // 整架那一片摆的是还没进任何组的书 —— 组既是去处也是收纳，进了组的书只有走进
        // 那一组才看得见。搜索那一头不跟着筛：词非空时查全库，否则进过组的书在架上再也
        // 搜不出来（某一组那一页没有搜索行），而读者读到的结果是「书丢了」。
        let queried = self.store.as_ref().map(|store| match self.scope {
            ShelfScope::Shelf => {
                // 筛着状态时查整架再按有效状态滤（下面）：状态回答「读到哪儿了」，
                // 不回答「收进哪个组」，进了组的书也按状态看得见 —— 与旧的那条
                // `shelf_by_tag` 同一片。搜索词叠在状态之上。
                if self.tag_filter.is_some() {
                    store.shelf(query)
                } else if query.trim().is_empty() {
                    store.shelf_ungrouped(query)
                } else {
                    store.shelf(query)
                }
            }
            ShelfScope::Collection(id) => store.shelf_in_collection(id, query),
        });
        // 这一趟换没换内容都要重拼一次流：`tiles` 与 `items` 刚才都被重写过了。
        self.flow_dirty = true;
        match queried {
            Some(Ok(items)) => {
                self.query = query.into();
                // 卡片角标要的那份标签表与架面同一次重查取：逐卡再查一次等于几百本就是几百趟。
                // 它同时也是状态筛选的「手动那一半」，所以要先落进来再滤。
                if let Some(store) = &self.store
                    && let Ok(map) = store.tags_map()
                {
                    self.tags = map;
                }
                // 筛着状态时按**有效状态**留书：手动挂过标签的以手动为准
                // （`Tag::effective`），没挂的按阅读数据自动判（`ShelfItem::auto_status`
                // —— 没翻开是未读，进度到线是读完，其余在读）。在 Rust 这一侧滤而不是
                // 在 SQL 里滤：有效状态是两条数据（进度、手动标签）会合出来的，SQL 里
                // 拼得出但读不懂；而整架那一趟本来就全量取回来（进度条与封面都要）。
                let items = match self.tag_filter {
                    Some(tag) => items
                        .into_iter()
                        .filter(|item| {
                            Tag::effective(
                                self.tags.get(&item.id).map_or(&[], Vec::as_slice),
                                item.auto_status(),
                            ) == tag
                        })
                        .collect::<Vec<_>>(),
                    None => items,
                };
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

    /// 架面「⋮」菜单里的 OPDS 书源：把浏览器压成一整页。
    ///
    /// 与「信息」页同一条栈：系统返回键弹掉整页；浏览器内部的子目录由它自己的顶栏退。
    /// 它下载成功时往外报一条 [`OpdsEvent::Imported`]，架面收到就重查一次。
    pub(in crate::ui::features::shelf) fn open_opds(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let page = cx.new(|cx| OpdsView::new(self.stack.clone(), cx));
        self._opds_watch = Some(cx.subscribe(&page, |this, _, event, cx| {
            if matches!(event, OpdsEvent::Imported) {
                this.force_reload(cx);
                this.pending_reload = true;
            }
        }));
        let _ = self
            .stack
            .update(cx, |state, cx| state.push(page, NavMotion::Animated, cx));
    }

    /// 架面「⋮」菜单里的网盘取书：把浏览器压成一整页。接线与 [`Self::open_opds`]
    /// 同一条 —— 系统返回键弹掉整页，浏览器内部自己管目录层级；下载成功报
    /// [`CloudEvent::Imported`]，架面重查。
    pub(in crate::ui::features::shelf) fn open_cloud(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let page = cx.new(|cx| CloudView::new(self.stack.clone(), cx));
        self._cloud_watch = Some(cx.subscribe(&page, |this, _, event, cx| {
            if matches!(event, CloudEvent::Imported) {
                this.force_reload(cx);
                this.pending_reload = true;
            }
        }));
        let _ = self
            .stack
            .update(cx, |state, cx| state.push(page, NavMotion::Animated, cx));
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
                    group: format!("shelf-tile-{}", collection.id).into(),
                    face_id: format!("shelf-tile-face-{}", collection.id).into(),
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

    /// 这一帧的格子流。脏了才重拼 —— 那两段 `Option<i64>` 与 `merge_cells` 的分配
    /// 从前是**每一帧**付一次，架上有几百本时它就是滚动帧预算里一项白花的钱。
    fn ensure_flow(&mut self) {
        if self.flow_dirty {
            self.flow = self.cells().into();
            self.flow_dirty = false;
        }
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
    /// 几本架上看不见的副本。那张导入状态卡（见 [`super::status`]）就是「等一下再点」的提示。
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
        // 这一批的记录行重开一列，「读者关掉过这张卡」也只到上一批为止：新的批次要重新说话。
        self.begin_import_records(cx);

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
            // 开始那条不带读数：提交的那一手已经把状态卡标成不定进度了（`super::status`）。
            TaskEvent::Started => {}
            TaskEvent::Progress(progress) => {
                self.importing = Some((progress.done, progress.total));
                // 管线附送的文案只进日志：它是 core 里的事实描述，屏幕上那张卡说的每一句
                // 都要走 locale（卡头是 `shelf.status_importing`），否则英文界面上会冒出中文来。
                // 进度不进卡头 —— 它归每行名字下面那条 bar。
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
                // 字节级读数带着书名（`ByteProgress::name`），所以记录行是它喂大的：
                // 那一本的名字下面那条进度条读的就是这两个数。从前这一手只进日志。
                self.note_import_bytes(&bytes.name, bytes.done, bytes.total, cx);
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
                // 在跑那一列没有结论可写（结果表不会来了），所以不留一排「还在传」的行骗人。
                // 已经进去的那些本来就上了架 —— 那才是取消之后给读者的答案。
                self.import_live.clear();
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
        // 一句分类总结（`plans/基础设施.md` §3.2：批量作业要能聚合报表）。逐本的指名道姓
        // 在日志里（上面那几条 `Err`），界面上只报「几本、为什么」那一句 ——
        // 可折叠的作业卡片仍是待决（`plans/导入管线.md` §十一 待决第 7 条）。
        let report = ImportReport::tally(&outcomes);
        tracing::info!(
            "ShelfView: 一批 {} 本结清 —— {}",
            outcomes.len(),
            report.summary()
        );
        // 逐本长成记录行（新的接到历史头上），在跑那一列就此清空。
        self.finish_import_records(&outcomes, cx);
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

    /// 表单那一屏在弹层里的内容：弹层还没停稳、且内容还在的时候才给。
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

    /// 把两块弹层的开合**派生**自那两个意图位，顺手把「已经停稳」的那些内容 drop 掉。
    ///
    /// 派生而不是在每一处改意图的地方各叫一次 `show()` / `hide()`：那样有五六处，漏一处
    /// 就是一屏关不掉的弹层。桌面那一档 `want` 恒假，于是意图一收内容当场跟着没 ——
    /// 与那张本来就没有退场动画的浮层正好对得上。
    fn sync_panels(&mut self, compact: bool, cx: &mut Context<Self>) {
        for (want, state) in [
            (self.group_open, &self.group_sheet),
            (self.scan_open, &self.scan_sheet),
            (self.network_open, &self.network_sheet),
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
    /// 进入多选态。`with` 是长按菜单里选中那一本时顺手勾上的那一本；不传就是空的，
    /// 进来等读者逐本点。
    pub(in crate::ui::features::shelf) fn begin_selection(
        &mut self,
        with: Option<i64>,
        cx: &mut Context<Self>,
    ) {
        self.selecting = true;
        self.selected.clear();
        if let Some(id) = with {
            self.selected.insert(id);
        }
        self.notify_selection(cx);
    }

    /// 退出多选态。取消 / 返回 / 做完批量动作都走这一条。
    pub(in crate::ui::features::shelf) fn exit_selection(&mut self, cx: &mut Context<Self>) {
        if !self.selecting && self.selected.is_empty() {
            return;
        }
        self.selecting = false;
        self.selected.clear();
        self.notify_selection(cx);
    }

    /// 退出多选态：根实例与栈上那一页都走这一条，由 [`Self::selection_page`] 决定退谁。
    ///
    /// 与那条栏上那颗「退出」（[`Self::selection_action`] 的 `Exit` 一支）同一件事，
    /// 只是这一条**不要窗口** —— 系统返回那一路是应用级的动作，手上没有 `Window`
    /// （见 `shell::AppShell::back` 的注册处）。
    pub fn drop_selection(&mut self, cx: &mut Context<Self>) {
        if let Some(page) = self.selection_page(cx) {
            page.update(cx, |page, cx| page.exit_selection(cx));
            return;
        }
        self.exit_selection(cx);
    }

    /// 眼下是不是多选态，**栈上那一组那一页也算**。外壳据此决定底部那一条画导航还是画工具栏。
    pub fn selection_on(&self, cx: &App) -> bool {
        self.selecting || self.selection_page(cx).is_some()
    }

    /// 外壳每帧取一份：计数、全选、单本、可加入的分组。
    ///
    /// 栈上那一组那一页是**另一块架面**，勾中的那批书长在它身上，而外壳手上只有根实例
    /// 这一条句柄 —— 那一页自己进了多选态时这里就报它那份，与 [`Self::force_reload`] 转
    /// 一道是同一个道理。两处都不在多选态时报根架面那份（`active` 为 `false`）。
    pub fn selection_summary(&self, cx: &App) -> SelectionSummary {
        self.selection_page(cx)
            .map(|page| page.read(cx).own_selection_summary())
            .unwrap_or_else(|| self.own_selection_summary())
    }

    /// 眼下握着多选那批书的那一页：压在栈上的那一组还开着、并且自己进了多选态时才是它。
    ///
    /// 只往下一层：那一页摆的是某一组里的书，`scope` 是 `Collection`，架面上没有组卡，
    /// 读者在那一页上点不到第三层架面，所以不存在「孙页也在多选」那种形状。
    fn selection_page(&self, cx: &App) -> Option<Entity<ShelfView>> {
        self.group_page
            .as_ref()
            .and_then(|page| page.upgrade())
            .filter(|page| page.read(cx).selecting)
    }

    fn own_selection_summary(&self) -> SelectionSummary {
        let count = self.selected.len();
        SelectionSummary {
            active: self.selecting,
            count,
            all_selected: self.all_selected(),
            single: (count == 1)
                .then(|| self.selected.iter().next().copied())
                .flatten(),
            groups: self
                .collections
                .iter()
                .map(|group| (group.id, SharedString::from(group.name.clone())))
                .collect(),
        }
    }

    /// 点卡片：多选态切勾，平时开书。两个动作同一个入口，卡片那一层不必知道当前是哪一档。
    pub(in crate::ui::features::shelf) fn tap_card(
        &mut self,
        book_id: i64,
        cx: &mut Context<Self>,
    ) {
        if self.selecting {
            if !self.selected.remove(&book_id) {
                self.selected.insert(book_id);
            }
            self.notify_selection(cx);
        } else {
            cx.emit(ShelfEvent::Open(book_id));
        }
    }

    /// 全选 / 全不选（当前这一片书，不含组卡）。
    pub(in crate::ui::features::shelf) fn toggle_select_all(&mut self, cx: &mut Context<Self>) {
        if self.all_selected() {
            self.selected.clear();
        } else {
            self.selected = self.items.iter().map(|card| card.id).collect();
        }
        self.notify_selection(cx);
    }

    fn all_selected(&self) -> bool {
        !self.items.is_empty()
            && self
                .items
                .iter()
                .all(|card| self.selected.contains(&card.id))
    }

    /// 多选工具栏派下来的一件事。`pub`：那条栏画在外壳那一层（它才拿得到整窗的几何），
    /// 而动作本体在架面 —— 外壳只有根实例这一条句柄，所以转给眼下握着那批书的一页。
    pub fn selection_action(
        &mut self,
        action: navigation::SelectionAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(page) = self.selection_page(cx) {
            page.update(cx, |page, cx| page.own_selection_action(action, window, cx));
            return;
        }
        self.own_selection_action(action, window, cx);
    }

    fn own_selection_action(
        &mut self,
        action: navigation::SelectionAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            navigation::SelectionAction::Exit => self.exit_selection(cx),
            navigation::SelectionAction::ToggleAll => self.toggle_select_all(cx),
            navigation::SelectionAction::Group(collection_id) => {
                self.add_selection_to_collection(collection_id, cx)
            }
            navigation::SelectionAction::Info => self.show_selected_info(window, cx),
            navigation::SelectionAction::Upload => self.upload_selection(cx),
            navigation::SelectionAction::Delete => self.ask_delete_selection(window, cx),
        }
    }

    fn notify_selection(&mut self, cx: &mut Context<Self>) {
        cx.emit(ShelfEvent::SelectionChanged);
        cx.notify();
    }

    /// 把勾中的书全部加进某个组。已经在里面的由 `INSERT OR IGNORE` 吞掉，不重复。
    fn add_selection_to_collection(&mut self, collection_id: i64, cx: &mut Context<Self>) {
        let Some(store) = self.store.clone() else {
            tracing::error!("ShelfView: 没有书库，多选的书加不进第 {collection_id} 组");
            return;
        };
        let ids: Vec<i64> = self.selected.iter().copied().collect();
        for id in &ids {
            if let Err(err) = store
                .add_to_collection(collection_id, *id)
                .with_context(|| format!("把第 {id} 本加进第 {collection_id} 组"))
            {
                report("ShelfView", &err);
            }
        }
        self.exit_selection(cx);
        self.force_reload(cx);
    }

    fn show_selected_info(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.own_selection_summary().single else {
            return;
        };
        self.exit_selection(cx);
        self.show_info(id, window, cx);
    }

    /// 把勾中的书传进网盘同步仓库（`<remote_dir>/books/<content_hash>.<ext>`）。
    /// 后端与目录跟着设置里的同步配置走；没配好由那张浮层说，不在这里拦。
    fn upload_selection(&mut self, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            return;
        }
        let ids: Vec<i64> = self.selected.iter().copied().collect();
        cloud_upload::start(cx, ids);
        cx.notify();
    }

    /// 删除前先问一句：批量删是不可撤销的，而工具栏那颗「删除」离手指很近。
    ///
    /// 用的是框架的 AlertDialog（`window.open_alert_dialog`）而不是手搭的居中卡片：
    /// 尺寸、遮罩、居中、Escape / 返回键与焦点框都由框架给，且宽度自动夹在视口之内
    /// —— 从前的手写卡写死 420 宽、152 高，手机上比视口还宽，把整屏占得满满当当。
    /// 点头才落库（[`Self::confirm_delete_selection`]），取消什么都不动。
    fn ask_delete_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            return;
        }
        let count = self.selected.len();
        let entity = cx.entity().clone();
        let title = tf(cx, "shelf.selection_delete_title", &[("n", &count)]);
        let body = tf(cx, "shelf.selection_delete_body", &[("n", &count)]);
        let ok = tr(cx, "shelf.selection_delete_confirm");
        let cancel = tr(cx, "shelf.cancel");
        window.open_alert_dialog(cx, move |alert, _, _| {
            // build 闭包是 `Fn`（dialog 层重建时会再跑一遍），所以句柄只在每次重建时
            // 现摘一份交给 `on_ok`，不能把外层捕获的那一整个 move 进去。
            let on_ok_entity = entity.clone();
            alert
                .title(title.clone())
                .description(body.clone())
                .confirm()
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(ok.clone())
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text(cancel.clone())
                        .on_ok(move |_, _, cx| {
                            on_ok_entity.update(cx, |this, cx| this.confirm_delete_selection(cx));
                            true
                        }),
                )
        });
    }

    /// 确认框上那颗「删除」：真正落库。留在架面这一层、由 `on_ok` 派回来 —— 这一段的
    /// 动作（删书、退出多选、重查）与确认框是谁摆的无关，回归门也直接敲这一句。
    pub(in crate::ui::features::shelf) fn confirm_delete_selection(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        self.delete_selection(cx);
    }

    fn delete_selection(&mut self, cx: &mut Context<Self>) {
        let Some(store) = self.store.clone() else {
            tracing::error!("ShelfView: 没有书库，多选的书删不掉");
            return;
        };
        let ids: Vec<i64> = self.selected.iter().copied().collect();
        let root = target_dir(&ConfigHolder::library(cx));
        for id in ids {
            match delete_book(&store, id, root.as_deref()).with_context(|| format!("删第 {id} 本"))
            {
                Ok(()) => {
                    self.covers.remove(&id);
                    cx.emit(ShelfEvent::Deleted(id));
                }
                Err(err) => report("ShelfView", &err),
            }
        }
        self.selected.clear();
        self.selecting = false;
        // 清空是为了绕过 `reload` 的去重：搜索词没变，但架面少了几本，必须重查一次。
        self.force_reload(cx);
        self.notify_selection(cx);
    }

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

    /// 收「新建分组」那一屏。**只**改意图：内容（那个输入框）留到弹层停稳才 drop，
    /// 不然手机上读者看到的是一张空卡往下沉；桌面那张浮层没有退场动画，同一帧就没了
    /// —— 看意图位在哪一格收（[`Self::sync_panels`]）。
    pub(in crate::ui::features::shelf) fn close_group_panel(&mut self, cx: &mut Context<Self>) {
        self.group_open = false;
        cx.notify();
    }

    // ────────────────────────────────────────────────────────────────────
    // 标记与阅读记录
    // ────────────────────────────────────────────────────────────────────

    /// 长按菜单里那格「标记」：开那张居中的标记浮层，记下是为哪本书开的。
    ///
    /// 打勾读的是**打开那一刻**库里的标签（快照进 [`Self::tag_entry`]），之后每一格
    /// 点按当场落库（[`Self::toggle_tag`]），不攒一笔总账 —— 菜单里分组那节的开关
    /// 就是这么做的，两处同一个习惯。
    pub(in crate::ui::features::shelf) fn open_tag_popover(
        &mut self,
        book_id: i64,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self
            .store
            .as_ref()
            .and_then(|store| store.tags_of_book(book_id).ok())
            .unwrap_or_default();
        self.tag_entry = Some((book_id, snapshot));
        // 这张浮层的入口也是卡片菜单：菜单点完就散，没有留下能钉的元素 —— 与建组
        // 那一屏同一条理由，卡摆在屏幕正中（`sheets::tag_popover`）。
        cx.notify();
    }

    /// 收标记浮层。只改意图：落库已经在每格点按的那一刻做完了。
    pub(in crate::ui::features::shelf) fn close_tag_popover(&mut self, cx: &mut Context<Self>) {
        if self.tag_entry.take().is_some() {
            cx.notify();
        }
    }

    /// 标记浮层里点了一格：**单选** —— 点中的那档当场落库，再点同一档退回自动（清空），
    /// 点别的档直接换过去。手动指定的书以手动为准（[`Tag::effective`]），清空就是交给
    /// 阅读数据自动判。
    ///
    /// 写走 [`Store::set_book_tags`] 整组替换（幂等），架面不整架重查 —— 角标归
    /// [`Self::tags`] 管，改完那一格 `notify` 一帧就够；只有「这本的有效状态」变了，
    /// 架面上的书一排都没动。
    pub(in crate::ui::features::shelf) fn toggle_tag(
        &mut self,
        book_id: i64,
        tag: Tag,
        cx: &mut Context<Self>,
    ) {
        let Some((entry_id, snapshot)) = self.tag_entry.as_mut() else {
            return;
        };
        if *entry_id != book_id {
            return;
        }
        // 老数据可能一格多行：点任何一档都先归一成单选 —— 点中的那档，或清空。
        let want: Vec<Tag> = if snapshot.len() == 1 && snapshot[0] == tag {
            Vec::new()
        } else {
            vec![tag]
        };
        // 要写的整组先抄一份：快照那笔可变借用借在 `self.tag_entry` 上，而下面要借
        // `self.store` 落库 —— 两次借用都得从 `self` 出发，抄一份让前一笔当场收手。
        let on = !want.is_empty();
        *snapshot = want.clone();
        let Some(store) = &self.store else {
            return;
        };
        let written = store
            .set_book_tags(book_id, &want)
            .with_context(|| format!("写第 {book_id} 本的标签"));
        if let Err(err) = written {
            report("ShelfView", &err);
            // 写失败把快照拨回去：浮层上的勾说的是库，不是本地这次没写成的意图。
            if let Some((_, snapshot)) = self.tag_entry.as_mut() {
                if on {
                    snapshot.clear();
                } else {
                    *snapshot = vec![tag];
                }
            }
        } else {
            self.tags.insert(book_id, want);
        }
        cx.notify();
    }

    /// 长按菜单里那格「清除阅读记录」：先问一句再动手 —— 清掉的东西找不回来。
    ///
    /// 与多选删除同一张框架的 AlertDialog：尺寸、遮罩、居中、返回键都由框架给，
    /// 不再手搭 420 宽的居中卡（那张在手机上比视口还宽）。哪一本由 `book_id` 直接
    /// 交给 `on_ok`，所以不需要再在架面上留一个「确认浮层开给谁」的状态位。
    pub(in crate::ui::features::shelf) fn open_clear_history(
        &mut self,
        book_id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.entity().clone();
        let title = tr(cx, "shelf.clear_history");
        let body = tr(cx, "shelf.clear_history_body");
        let ok = tr(cx, "shelf.clear_history_confirm");
        let cancel = tr(cx, "shelf.cancel");
        window.open_alert_dialog(cx, move |alert, _, _| {
            // build 是 `Fn`（dialog 层重建会再跑），句柄每次现摘一份给 `on_ok`。
            let on_ok_entity = entity.clone();
            alert
                .title(title.clone())
                .description(body.clone())
                .confirm()
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(ok.clone())
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text(cancel.clone())
                        .on_ok(move |_, _, cx| {
                            on_ok_entity
                                .update(cx, |this, cx| this.confirm_clear_history(book_id, cx));
                            true
                        }),
                )
        });
    }

    /// 确认框上按了「清除」：清库（进度、已读章、时长、最近打开），然后重查架面
    /// ——「最近打开」变了，整架那条序要跟着重排。
    pub(in crate::ui::features::shelf) fn confirm_clear_history(
        &mut self,
        book_id: i64,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = &self.store else {
            return;
        };
        if let Err(err) = store
            .clear_reading_history(book_id)
            .with_context(|| format!("清第 {book_id} 本的阅读记录"))
        {
            report("ShelfView", &err);
        }
        self.force_reload(cx);
    }

    // ────────────────────────────────────────────────────────────────────
    // 网络导入（Wi-Fi 传书）
    // ────────────────────────────────────────────────────────────────────

    /// ⋮ 菜单里那格「网络导入」：确保传书服务起着，把它的第一个地址画成二维码，摆卡。
    ///
    /// 服务没起就当场起（设置页那个开关是同一条 `transfer::set_enabled`），起了就复用 ——
    /// 端口被占会顺延，所以地址与二维码都要**这一刻**重新问一次，不能缓存上一次的。
    pub(in crate::ui::features::shelf) fn open_network_import(&mut self, cx: &mut Context<Self>) {
        if transfer::running().is_none() {
            transfer::set_enabled(true, cx);
        }
        self.network_qr = transfer::running()
            .and_then(|status| status.urls.into_iter().next())
            .and_then(|url| network_import::qr_image(&url));
        self.network_open = true;
        cx.notify();
    }

    /// 收卡：**顺手停服务**（卡上没有关闭 / 停止按钮，点外面或按返回就是这一条）。
    ///
    /// 服务不设口令，开着等于把局域网里的上传入口一直敞着；读者关掉这张卡就是
    /// 「我用完了」。二维码也一起丢掉，下次打开按那时候的地址重算。
    pub(in crate::ui::features::shelf) fn close_network_import(&mut self, cx: &mut Context<Self>) {
        self.network_open = false;
        self.network_qr = None;
        transfer::set_enabled(false, cx);
        cx.notify();
    }

    /// 筛选那一排点了一颗标签 chip：点中的那颗在筛与不筛之间切换（一次只筛一颗）。
    ///
    /// 换了筛选就重查一次架面 —— 查的是另一片书，`reload` 的去重帮不上忙。
    pub(in crate::ui::features::shelf) fn toggle_tag_filter(
        &mut self,
        tag: Tag,
        cx: &mut Context<Self>,
    ) {
        self.tag_filter = if self.tag_filter == Some(tag) {
            None
        } else {
            Some(tag)
        };
        tracing::debug!(
            "ShelfView: 标签筛选 → {:?}",
            self.tag_filter.map(|tag| tag.as_str())
        );
        self.items.clear();
        let query = self.query.clone();
        self.reload(&query, cx);
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
            // 那一页进了多选态：外壳要据此把底部导航换成工具栏，所以这一声也转一道。
            ShelfEvent::SelectionChanged => cx.emit(ShelfEvent::SelectionChanged),
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
    fn body(
        &self,
        flow: Rc<[Cell]>,
        enter: Enter,
        cx: &mut Context<Self>,
        spec: GridSpec,
        tail: f32,
    ) -> AnyElement {
        if self.confirming {
            return self.confirm_panel(cx);
        }
        if !self.items.is_empty() || !self.tiles.is_empty() {
            return self.grid(flow, enter, cx, spec, tail).into_any_element();
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

    /// 这一帧「往下滚了多远」（逻辑像素，正数）。句柄在布局之后写，读到的是刚过去
    /// 那一帧的落点。
    ///
    /// 取负：GPUI 的 `offset` 是内容平移量，往上滚是负数；而收起判据与封面缩放读的
    /// 都是读者视角的「过滚动距离」，翻转只在这一处。
    fn scroll_offset(&self) -> f32 {
        -f32::from(self.grid_scroll.0.borrow().base_handle.offset().y)
    }

    /// 回归门用：架面这片格子已经往下滚了多远。浮层那张卡里的清单滚动时**不该**带动它，
    /// 而那条判据只有在这儿量得出来。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn grid_offset_for_test(&self) -> f32 {
        self.scroll_offset()
    }

    /// 外壳递来的让位投影：顶栏那一行跟着收放。只在状态真变了时敲重画 —— 滚动每帧都
    /// 在报增量，而让位是低频事件。
    pub(crate) fn set_fold(&mut self, folded: bool, cx: &mut Context<Self>) {
        if self.folded != folded {
            self.folded = folded;
            cx.notify();
        }
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
    fn grid(
        &self,
        flow: Rc<[Cell]>,
        enter: Enter,
        cx: &mut Context<Self>,
        spec: GridSpec,
        tail: f32,
    ) -> impl IntoElement {
        // 卡片管理菜单的两条文案：整个架面取一次，逐卡 clone 进菜单。条目本身
        // （哪几条、什么图标、点了派给谁）在 [`book_menu`]。
        let labels = book_menu::Labels::resolve(cx);
        let columns = spec.columns as usize;
        // 一横排算一项；不满一排的尾巴也算一排，不然最后那几本会画不出来。
        // 数的是**格子**：组卡与书并成的那一条流（[`Self::ensure_flow`]），一张卡占一格。
        let cells = flow;
        let rows = cells.len().div_ceil(columns);
        let shelf = cx.entity().clone();
        let scroll = self.grid_scroll.clone();
        let cover_height = spec.cover_height;
        // 封面随滚动收放的输入：上一帧量出来的排距与这一帧的滚动位移。排距还没量到
        // （第一帧）时按 1.0 画，一棵架子不该为第一帧的未知数闪一下。
        let row_pitch = self
            .grid_scroll
            .0
            .borrow()
            .last_item_size
            .map_or(0.0, |size| f32::from(size.item.height));
        let scroll_y = self.scroll_offset();
        let list = uniform_list("shelf-grid", rows, move |range, window, cx| {
            let viewport_height = f32::from(window.viewport_size().height);
            range
                .map(|row| {
                    // 这一排的中心落在视口的哪儿：-1 = 顶外，0 = 正中，1 = 底外。
                    let scroll_scale = cover_scale(row, row_pitch, scroll_y, viewport_height);
                    let row_started = Instant::now();
                    let from = row * columns;
                    let to = (from + columns).min(cells.len());
                    // 句柄只在这一排真要画的时候读那几行数据：整片 `items` 不 clone，
                    // 跟着这一屏走的只有那串格子下标。
                    let el = shelf.update(cx, |this, cx| {
                        // 落定的时钟**一帧取一次**（在 `render` 里），这里只按格取进度：
                        // 从前是在每个可见排的 item 闭包里各取一次，`Instant::now` 与
                        // 「还有没有在动」那一问就每排每帧各付一遍。
                        let _ = &enter;
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
                                    scroll_scale,
                                    &mut *window,
                                    cx,
                                ),
                            }))
                            .into_any_element()
                    });
                    crate::ui::features::shelf::perf::note_item(row_started.elapsed());
                    el
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
        // 欠着的那一次搜索在这里结算：`SearchDebounce` 按渲染帧数安静多久，理由写在
        // 它自己的注释里（一句话：按 `timer` 的话这条去抖就没法有门）。
        if let Some(word) = self.search_debounce.due() {
            self.reload(&word, cx);
        }
        // 导入后的兜底重查：notify 在后台被丢的那一帧，恢复前台的第一帧补上。
        if self.pending_reload {
            self.pending_reload = false;
            self.force_reload(cx);
        }
        // 水墨翻档：封面是在 `reload` 那一步解的（`Card` 里存的是解好的 `Image`），
        // 缓存键里又没有「哪一档」这一格 —— 只清缓存不够，得让这一屏把封面重解一遍。
        // 走 `force_reload` 这一条现成的重查路：书卡与组卡一起重拼，下一趟 `resolve_cover`
        // 就会按新的旗子把封面压成灰。
        let ink = crate::ui::foundation::images::ink_images();
        if self.covers_ink != ink {
            self.force_reload(cx);
            self.covers_ink = ink;
        }
        // 库里那份导入记录只读一次（重启之后那张卡还要接着说话）。判据是「库装上了没有」：
        // 没装上时这一手什么都不做，下一帧再来，慢机器与快机器看到的是同一个结果。
        if !self.import_log_loaded {
            self.load_import_log(cx);
        }
        // 上传实体是全进程那一份：换了这一屏实例、或后台那趟在别处发起，这一帧第一次
        // 看到它时挂上观察，完成时才有一次重画把「正在传」换成结果。
        if self._upload_watch.is_none()
            && let Some(upload) = cloud_upload::handle(cx)
        {
            self._upload_watch = Some(cx.observe(&upload, |_, _, cx| cx.notify()));
        }
        let render_started = Instant::now();
        // 这一帧的格子流与入场时钟各取一次：两者从前都住在每个可见排的 item 闭包里，
        // 一排一份 —— 滚动时每帧要付「重拼整条流 / 每排各取一次时钟」两笔白账。
        self.ensure_flow();
        let enter = self.enter_clock(self.flow.len(), window, cx);
        let flow = self.flow.clone();
        // 顶部那一行按窗口档位换：手机上满幅的搜索框是「先搜再翻」，桌面上同一颗框
        // 拉到整片内容宽就成了一块空白，所以那边是定宽搜索框 + 导入的工具栏。
        let size = SizeClass::of(window);
        // 顶栏让位不再由这一屏判：外壳听内容那一路的滚动增量（`shell::AppShell::note_scroll`），
        // 四屏共用一句判据，投影由 [`ShelfView::set_fold`] 递回来。
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
        // 往下读时顶栏收成零高，让出的那一段给格子；往回拨再滑回来。弹簧按元素 id
        // 存状态，反向拨动会从**当前值**接着走而不是重头再来（可打断）。
        // 手机（搜索行）与平板（工具栏）同一档高度；桌面不收（`folded` 恒 false）。
        let top_fold = if !matches!(size, SizeClass::Expanded) {
            transition(
                "shelf-top-fold",
                if self.folded { 1.0 } else { 0.0 },
                Transition::new(Duration::from_millis(220)),
                window,
                cx,
            )
        } else {
            0.0
        };
        let top = if top_fold > 0.001 {
            div()
                .w_full()
                .h(px((SEARCH_BAND * (1.0 - top_fold)).max(0.0)))
                .overflow_hidden()
                .opacity(1.0 - top_fold)
                .child(top)
                .into_any_element()
        } else {
            top
        };
        // 标签筛选那一排挂在顶栏**下面**、折叠容器之外：它是常驻的筛入口，不跟着
        // 搜索行收折 —— 收进那个定高裁切的容器里，它会被 SEARCH_BAND 那个高度裁掉
        // 而不是让位。整架那一屏才有（某一组那一页筛的是组里的人，不归标签管）。
        let tag_bar = if self.scope == ShelfScope::Shelf {
            Some(self.tag_filter_bar(cx))
        } else {
            None
        };
        let spec = grid_spec(f32::from(window.viewport_size().width), size);
        // 悬浮底栏压在内容上（Compact 档），滚动内容得自己空出那一段 —— 加在容器的
        // 留白上会把视口切短，格子就再也到不了栏底下。侧栏两档平时是嵌入的、由外壳让位，
        // 这里不加；但多选那一态在哪个档位都是同一条浮在底边的工具栏（见
        // `shell::navigation::selection_bar`），所以那一态下三档一起让位，否则最后一排
        // 永远停在栏底下滚不出去。
        let tail = if size.is_compact() || self.selecting {
            navigation::clearance(crate::ui::host::safe_area(window, cx).bottom)
        } else {
            0.0
        };
        // 导入这件事的两句话（正在跑的读数 + 取消、上一批「几本没进来、为什么」）不再占
        // 顶栏与格子之间那一格：它们是同一张状态卡的内容，摆在屏幕正中，见 [`super::status`]。
        // 从前那样摆，读数每报一次就要把整片架子往下顶一行，而这句话说的事离左上角最远。
        // 那三块「答一句话」的面板：桌面是一张浮层，手机是同一份内容压进底部弹层。
        self.sync_panels(size.is_compact(), cx);
        let mut overlays: Vec<AnyElement> = if size.is_compact() {
            // 两块弹层每帧都交元素：弹簧的采样在里面，收稳的那些只是不给内容。
            vec![
                self.group_sheet(cx),
                self.scan_sheet(cx),
                self.network_sheet(cx),
            ]
        } else {
            let mut overlays = Vec::new();
            if self.group_open
                && let Some((_, entry)) = &self.group_entry
            {
                overlays.push(self.group_popover(entry.clone(), cx));
            }
            if self.scan_open && self.scan.is_some() {
                overlays.push(self.scan_popover(cx));
            }
            overlays
        };
        // 导入记录那一张：两端都是屏幕正中那张浮层，所以它不进上面那个档位分支。
        // 宽要按视口现算 —— 手机上照搬 420 的上限会把右边那两颗钮顶到屏幕外（见
        // `status::record_width`），那张卡最主要的动作就是那两颗。
        let viewport = f32::from(window.viewport_size().width);
        if let Some(card) = self.import_status_popover(viewport, cx) {
            overlays.push(card);
        }
        // 标记与「清除阅读记录」那两张：两端都是居中浮层（与多选删除的确认同一档
        // —— 问的是一句话，不是一块要滚的清单），所以不进上面的档位分支。
        if self.tag_entry.is_some() {
            overlays.push(self.tag_popover(cx));
        }
        // 网络导入：桌面那一档的居中卡。手机那一档在上面那段弹层分支里
        // （`network_sheet`），与扫描同一套容器。
        if self.network_open && !size.is_compact() {
            overlays.push(self.network_popover(cx));
        }
        if let Some(progress) =
            cloud_upload::handle(cx).and_then(|entity| entity.read(cx).progress().cloned())
        {
            overlays.push(self.upload_popover(progress, cx));
        }
        let tree = v_flex()
            .id("folio-shelf")
            // 那几张居中的浮层要有一个「内容区」可以居中，而这一格就是内容区：
            // 导航栏在它的左边、不在它里面。
            .relative()
            .size_full()
            .child(top)
            .children(tag_bar)
            .child(self.body(flow, enter, cx, spec, tail))
            .children(overlays)
            // 帧探针：这一格排在最后，绘制回调跑在别的孩子之后，两条回调之间的间隔
            // 就是读者感觉到的帧间隔（见 `shelf::perf` 的模块头）。
            .child(
                canvas(
                    |_, _, _| {},
                    |_, _, _, _| crate::ui::features::shelf::perf::note_paint(),
                )
                .absolute()
                .inset_0(),
            );
        crate::ui::features::shelf::perf::note_render(render_started.elapsed());
        tree
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
    use super::{ShelfScope, cover_scale, share_file_name};
    // 这三条是留给其余用例的：它们散在测试模块里几处（架面起窗、切目的地、压页）。
    use crate::ui::services::config::ConfigHolder;
    use crate::ui::shell::navigation;
    use crate::ui::shell::navigation::SizeClass;
    use gpui_kit::base::NavMotion;

    /// 封面缩放只认「这一排离视口中心多远」：正中不动，边缘收到 0.96。
    #[test]
    fn the_cover_scale_settles_at_the_viewport_center() {
        // 视口 800 高、排距 400：第 0 排中心 200（视口上部），第 1 排中心 600（下部）。
        assert_eq!(cover_scale(0, 400.0, 0.0, 800.0), 0.99);
        assert_eq!(cover_scale(1, 400.0, 0.0, 800.0), 0.99);
        // 滚到第 1 排正对中心：原样。
        assert_eq!(cover_scale(1, 400.0, 200.0, 800.0), 1.0);
        // 滚过头、第 0 排到了顶外：收到下限 0.96。
        assert_eq!(cover_scale(0, 400.0, 800.0, 800.0), 0.96);
        // 排距还没量出来时不动。
        assert_eq!(cover_scale(0, 0.0, 0.0, 800.0), 1.0);
    }

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

    /// 标记浮层**浮在**架面上，点一格当场落库，关掉只收意图。
    ///
    /// 形状与建组那张浮层的门同一条：开合是架面自己的状态（`tag_entry`），浮层晚一帧
    /// 才挂进弹窗层，所以断言前连画两帧。落库写的是 `Store::set_book_tags` —— 门要钉的
    /// 正是「点一格就写」，不是攒一笔总账等关浮层才写。
    #[gpui_kit::test]
    fn the_tag_popover_floats_and_writes_through(cx: &mut TestAppContext) {
        use folio_core::Tag;

        let (shelf, handle, store, _) = boot_shelf(cx);
        let a = add(&store, "甲");
        shelf.update(cx, |this, cx| this.force_reload(cx));

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("shelf-tag-panel").is_none(),
                "没点「标记」时不该有面板"
            );
            shelf.update(cx, |this, cx| this.open_tag_popover(a, cx));
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(
                window.try_find("shelf-tag-panel").is_some(),
                "点「标记」要浮起那张卡"
            );
            assert!(
                window
                    .try_find(ElementId::Name(format!("shelf-cover-{a}").into()))
                    .is_some(),
                "卡浮在架面上，不是把整片格子换掉"
            );

            // 点「在读」那一格：当场写库，不必等关浮层。
            shelf.update(cx, |this, cx| this.toggle_tag(a, Tag::Reading, cx));
            assert_eq!(
                store.tags_of_book(a).unwrap(),
                vec![Tag::Reading],
                "点一格当场写库"
            );
            // 再点同一格：摘掉，也是当场。
            shelf.update(cx, |this, cx| this.toggle_tag(a, Tag::Reading, cx));
            assert!(
                store.tags_of_book(a).unwrap().is_empty(),
                "再点同一格是摘掉"
            );

            shelf.update(cx, |this, cx| this.close_tag_popover(cx));
            window.render_frame(cx);
            assert!(
                window.try_find("shelf-tag-panel").is_none(),
                "关掉之后就没了"
            );
        })
        .expect("窗口应可更新");
    }

    /// 状态筛选按**有效状态**放书：没挂手动标签的书按阅读数据自动判（没翻开 = 未读，
    /// 翻开过 = 在读），手动挂过的以手动为准 —— 进度差得远也归手动那一档。
    ///
    /// 这扇门钉的是自动判状态的两句承诺：筛「未读」筛的是「从没翻开过的书」，判据
    /// 跟着数据走（`touch_opened` 一顶它就挪档，不需要谁记得去改标签）；筛「读完」
    /// 时手动标的赢过自动算的 —— 进度停在九成七的书有处可去。
    #[gpui_kit::test]
    fn the_status_filter_follows_reading_data_with_manual_override(cx: &mut TestAppContext) {
        use folio_core::Tag;

        let (shelf, _, store, _) = boot_shelf(cx);
        add(&store, "未翻"); // 没翻开 → 自动未读
        let opened = add(&store, "翻开");
        let pinned = add(&store, "手标");
        store.touch_opened(opened).expect("翻开");
        store
            .set_book_tags(pinned, &[Tag::Finished])
            .expect("手动标成读完");
        shelf.update(cx, |this, cx| this.force_reload(cx));

        shelf.update(cx, |this, cx| this.toggle_tag_filter(Tag::Unread, cx));
        assert_eq!(shelf.read_with(cx, |this, _| this.card_count()), 1);
        assert_eq!(
            shelf.read_with(cx, |this, _| this.card_title(0)),
            Some("未翻".into()),
            "没翻开的那本归未读"
        );

        // 翻开过的自动挪到在读：判据跟着数据走。
        shelf.update(cx, |this, cx| this.toggle_tag_filter(Tag::Reading, cx));
        assert_eq!(shelf.read_with(cx, |this, _| this.card_count()), 1);
        assert_eq!(
            shelf.read_with(cx, |this, _| this.card_title(0)),
            Some("翻开".into()),
            "翻开过、进度没到线的就是在读"
        );

        // 手动覆盖赢过自动判断：进度没动的书，手动标了读完就在读完那一档。
        shelf.update(cx, |this, cx| this.toggle_tag_filter(Tag::Finished, cx));
        assert_eq!(shelf.read_with(cx, |this, _| this.card_count()), 1);
        assert_eq!(
            shelf.read_with(cx, |this, _| this.card_title(0)),
            Some("手标".into()),
            "手动挂的标签赢过自动判断"
        );

        // 再点同一颗是取消筛选：整架回来，三本都在。
        shelf.update(cx, |this, cx| this.toggle_tag_filter(Tag::Finished, cx));
        assert_eq!(
            shelf.read_with(cx, |this, _| this.card_count()),
            3,
            "取消筛选后整架都回来"
        );
    }

    /// 多选态里点一张卡是**勾选**，不是开书：两条路共用 `tap_card` 这一个入口，走错一条
    /// 读者就会在批量删到一半的时候被推进一本书里。
    #[gpui_kit::test]
    fn a_checked_card_is_not_an_opened_book(cx: &mut TestAppContext) {
        let (shelf, _, store, _) = boot_shelf(cx);
        let a = add(&store, "甲");
        let b = add(&store, "乙");
        shelf.update(cx, |this, cx| this.force_reload(cx));

        // 平时那一下开书：勾选那一串该一直空着。
        shelf.update(cx, |this, cx| this.tap_card(a, cx));
        assert!(
            shelf.read_with(cx, |this, _| this.selected.is_empty()),
            "没进多选态的点击不该往勾选里塞东西"
        );

        shelf.update(cx, |this, cx| this.begin_selection(None, cx));
        shelf.update(cx, |this, cx| this.tap_card(a, cx));
        shelf.update(cx, |this, cx| this.tap_card(b, cx));
        assert_eq!(
            shelf.read_with(cx, |this, _| this
                .selected
                .iter()
                .copied()
                .collect::<Vec<_>>()),
            vec![a, b]
        );

        // 再点回甲：把那一本退出来，而不是顺手把整态也退掉。
        shelf.update(cx, |this, cx| this.tap_card(a, cx));
        let summary = shelf.read_with(cx, |this, app| this.selection_summary(app));
        assert!(summary.active, "还剩一本，这一态不该跟着没");
        assert_eq!(summary.count, 1);
        assert_eq!(summary.single, Some(b), "只剩一本时「信息」那一格才有对象");
    }

    /// 全选只覆盖**这一片书**：组卡摆的是「一组」而不是一本，把它算进勾选中就是拿一个
    /// 并不存在的书行主键去删库。
    #[gpui_kit::test]
    fn select_all_counts_books_and_never_the_group_tiles(cx: &mut TestAppContext) {
        let (shelf, _, store, _) = boot_shelf(cx);
        let a = add(&store, "甲");
        add(&store, "乙");
        let group = store.create_collection("想读").expect("建组");
        store.add_to_collection(group, a).expect("挂进去");
        shelf.update(cx, |this, cx| this.force_reload(cx));
        // 前置：整架只摆未分组的那一本，外加一张组卡。
        assert_eq!(shelf.read_with(cx, |this, _| this.card_count()), 1);
        assert_eq!(shelf.read_with(cx, |this, _| this.tile_count()), 1);

        shelf.update(cx, |this, cx| this.begin_selection(None, cx));
        shelf.update(cx, |this, cx| this.toggle_select_all(cx));
        let summary = shelf.read_with(cx, |this, app| this.selection_summary(app));
        assert_eq!(summary.count, 1, "全选只勾这一片书，那张组卡不参与");
        assert!(summary.all_selected);
        assert_eq!(
            summary.groups.len(),
            1,
            "工具栏那颗「加入分组」该看得见库里现成的组"
        );

        // 再按一次是全不选：清勾而已，不是把这一态也退掉。
        shelf.update(cx, |this, cx| this.toggle_select_all(cx));
        let summary = shelf.read_with(cx, |this, app| this.selection_summary(app));
        assert!(!summary.all_selected);
        assert_eq!(summary.count, 0);
        assert!(summary.active, "全不选之后栏还杵在那儿，读者接着挑");
    }

    /// 某一组那一页是**另一块架面**，勾中的那几本长在它身上，而外壳手上只有根实例这一条
    /// 句柄：读要转一道，按下去的动作也要转回到同一页，否则那条栏在组里读到的永远是零。
    #[gpui_kit::test]
    fn the_group_page_holds_the_selection_the_bar_reads(cx: &mut TestAppContext) {
        let (shelf, handle, store, stack) = boot_shelf(cx);
        let a = add(&store, "甲");
        add(&store, "乙");
        let group = store.create_collection("想读").expect("建组");
        store.add_to_collection(group, a).expect("挂进去");
        shelf.update(cx, |this, cx| this.force_reload(cx));
        cx.update_window(handle, |_, window, cx| {
            shelf.update(cx, |this, cx| {
                this.open_page(ShelfScope::Collection(group), window, cx)
            });
        })
        .expect("窗口应可更新");
        // 那条栈要在手上：那一页是栈养着的，句柄一撒weak 就升级不回来了（这不是缺陷，
        // 是「弹掉的那一页不该留下东西」那一条的另一面）。
        assert_eq!(stack.read_with(cx, |state, _| state.depth()), 2);
        let page = shelf
            .read_with(cx, |this, _| {
                this.group_page.as_ref().and_then(|page| page.upgrade())
            })
            .expect("那一页应已挂上");

        assert!(
            !shelf.read_with(cx, |this, app| this.selection_on(app)),
            "前置：压上那一页本身不算多选"
        );
        page.update(cx, |this, cx| this.begin_selection(Some(a), cx));

        let summary = shelf.read_with(cx, |this, app| this.selection_summary(app));
        assert!(summary.active, "根实例要替那一页报这一态");
        assert_eq!(summary.count, 1);
        assert_eq!(summary.single, Some(a));

        // 动作同样转一道：在根实例上派「退出」，退掉的是那一页的多选。
        let root = shelf.clone();
        cx.update_window(handle, |_, window, cx| {
            root.update(cx, |this, cx| {
                this.selection_action(navigation::SelectionAction::Exit, window, cx)
            });
        })
        .expect("窗口应可更新");
        assert!(
            !page.read_with(cx, |this, _| this.selecting),
            "退出派下去的那一页该真的退了"
        );
        assert!(!shelf.read_with(cx, |this, app| this.selection_on(app)));
    }

    /// 批量删除：那颗「删除」只是把确认摆出来，落库在读者点头之后；删完退出这一态并重查。
    ///
    /// 确认框现在由框架的 `AlertDialog` 摆在窗口的 dialog 层，所以「问」这一手要借窗口；
    /// 落库那一句仍是架面自己的方法，回归门直接敲它（点 OK 走的是同一句）。
    #[gpui_kit::test]
    fn a_batch_delete_takes_every_checked_book_off_the_shelf(cx: &mut TestAppContext) {
        let (shelf, handle, store, _) = boot_shelf(cx);
        let a = add(&store, "甲");
        let b = add(&store, "乙");
        add(&store, "丙");
        shelf.update(cx, |this, cx| this.force_reload(cx));
        shelf.update(cx, |this, cx| this.begin_selection(Some(a), cx));
        shelf.update(cx, |this, cx| this.tap_card(b, cx));

        cx.update_window(handle, |_, window, cx| {
            shelf.update(cx, |this, cx| this.ask_delete_selection(window, cx));
        })
        .expect("窗口应可更新");
        assert_eq!(
            shelf.read_with(cx, |this, _| this.card_count()),
            3,
            "问的那一帧一本都还没没"
        );

        shelf.update(cx, |this, cx| this.confirm_delete_selection(cx));
        assert_eq!(shelf.read_with(cx, |this, _| this.card_count()), 1);
        assert_eq!(
            shelf.read_with(cx, |this, _| this.card_title(0)),
            Some("丙".into()),
            "没勾中的那一本该留在架上"
        );
        assert!(store.book(a).expect("查库").is_none(), "甲那一行该真的没了");
        assert!(
            !shelf.read_with(cx, |this, app| this.selection_on(app)),
            "这批做完就退出多选态"
        );
    }

    /// 把勾中的那一批一次加进某个组：两条都落库，然后退出这一态（读者已经不需要再挑了）。
    #[gpui_kit::test]
    fn the_batch_group_add_lands_every_checked_book(cx: &mut TestAppContext) {
        let (shelf, _, store, _) = boot_shelf(cx);
        let a = add(&store, "甲");
        let b = add(&store, "乙");
        shelf.update(cx, |this, cx| this.force_reload(cx));
        let group = store.create_collection("想读").expect("建组");
        shelf.update(cx, |this, cx| this.begin_selection(Some(a), cx));
        shelf.update(cx, |this, cx| this.tap_card(b, cx));

        shelf.update(cx, |this, cx| this.add_selection_to_collection(group, cx));
        for id in [a, b] {
            assert!(
                store
                    .collections_of_book(id)
                    .expect("查名分")
                    .iter()
                    .any(|mine| mine.id == group),
                "第 {id} 本该在这一批里落进组"
            );
        }
        assert!(
            !shelf.read_with(cx, |this, app| this.selection_on(app)),
            "加完组就退出这一态"
        );
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
